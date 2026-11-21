class CliOauthRefreshToken < ApplicationRecord
  include HashedTokenLookup
  token_hash_attribute :token_hash
  belongs_to :user

  scope :usable, -> { where(revoked_at: nil, used_at: nil).where("expires_at > ? AND connection_expires_at > ?", Time.current, Time.current) }

  def with_connection_lock
    self.class.transaction do
      self.class.connection.execute(self.class.sanitize_sql_array([
        "SELECT pg_advisory_xact_lock(hashtextextended(?, 0))", "cli-connection:#{connection_id}"
      ]))
      yield
    end
  end

  def revoke!
    with_connection_lock { self.class.where(connection_id: connection_id).update_all(revoked_at: Time.current) }
  end
end
