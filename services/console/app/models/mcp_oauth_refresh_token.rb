class McpOauthRefreshToken < ApplicationRecord
  include HashedTokenLookup

  oid_prefix "mor"
  token_hash_attribute :token_hash

  DEFAULT_TTL = 90.days
  TOKEN_PREFIX = "mcprt_".freeze

  attr_accessor :plaintext_token

  belongs_to :mcp_oauth_client
  belongs_to :user
  belongs_to :principal

  before_validation :issue_token, on: :create

  validates :token_hash, presence: true, uniqueness: true
  validates :resource, :expires_at, presence: true
  validates :scopes, presence: true

  scope :usable, -> { where(revoked_at: nil).where("expires_at > ?", Time.current) }

  # Serialize rotation and disconnect across processes. Revoked ancestors stay
  # usable as revocation handles, but never as refresh credentials.
  def with_connection_lock
    self.class.transaction do
      self.class.connection.execute(self.class.sanitize_sql_array([
        "SELECT pg_advisory_xact_lock(hashtextextended(?, 0))", "mcp-connection:#{connection_id}"
      ]))
      yield
    end
  end

  def revoke!
    with_connection_lock do
      self.class.where(connection_id: connection_id).update_all(revoked_at: Time.current, updated_at: Time.current)
    end
  end

  private

  def issue_token
    self.expires_at ||= DEFAULT_TTL.from_now
    return if token_hash.present?
    self.plaintext_token = "#{TOKEN_PREFIX}#{SecureRandom.urlsafe_base64(48)}"
    self.token_hash = self.class.hash_token(plaintext_token)
  end
end
