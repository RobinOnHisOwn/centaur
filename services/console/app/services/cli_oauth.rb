require "jwt"
require "openssl"
require "uri"

# Native human credentials are never MCP, operator, service or sandbox tokens.
module CliOauth
  class InvalidGrant < StandardError; end
  class Unavailable < StandardError; end
  CLIENT = "centaur-cli".freeze
  SCOPES = %w[cli:identity cli:sessions:read cli:sessions:write cli:integrations].freeze
  TTL = 5.minutes

  module_function

  def enabled? = ENV["CENTAUR_CLI_AUTH_ENABLED"] == "true"

  def base
    configured_url(ENV.fetch("CENTAUR_CONSOLE_PUBLIC_URL", ""))
  end

  def configured_url(value)
    uri = URI.parse(value)
    raise Unavailable unless uri.is_a?(URI::HTTPS) && uri.host.present? && uri.userinfo.nil? && uri.query.nil? && uri.fragment.nil?
    value.delete_suffix("/")
  rescue URI::InvalidURIError
    raise Unavailable
  end

  def issuer = "#{base}/cli"
  def console_resource = "#{base}/api/cli/v1"
  def session_resource = configured_url(ENV.fetch("CENTAUR_CLI_SESSION_RESOURCE", ""))
  def resources = [ session_resource, console_resource ]

  def key
    OpenSSL::PKey::RSA.new(File.read(ENV.fetch("CENTAUR_CLI_SIGNING_KEY_FILE")))
  rescue KeyError, SystemCallError, OpenSSL::PKey::RSAError
    raise Unavailable
  end

  def issue_refresh(user:, scopes:, resources:, connection_id: SecureRandom.uuid, deadline: 30.days.from_now)
    raw = "clirt_#{SecureRandom.urlsafe_base64(48)}"
    row = CliOauthRefreshToken.create!(user: user, scopes: scopes, resources: resources,
      token_hash: CliOauthRefreshToken.hash_token(raw), connection_id: connection_id,
      connection_expires_at: deadline, expires_at: [ 7.days.from_now, deadline ].min)
    [ row, raw ]
  end

  def token_response(row, raw, resource)
    raise InvalidGrant unless row.resources.include?(resource) && row.user.active?
    scopes = resource == console_resource ? row.scopes & %w[cli:identity cli:admin] : row.scopes & SCOPES
    scopes -= [ "cli:admin" ] unless row.user.admin?
    now = Time.current.to_i
    claims = { iss: issuer, sub: row.user.oid, aud: resource, client_id: CLIENT, token_use: "cli_user",
      connection_id: row.connection_id, scope: scopes.join(" "), iat: now, nbf: now, exp: now + TTL.to_i, jti: SecureRandom.uuid }
    { access_token: JWT.encode(claims, key, "RS256", { typ: "centaur-cli+jwt", kid: "cli-v1" }),
      refresh_token: raw, token_type: "Bearer", expires_in: TTL.to_i, scope: scopes.join(" ") }
  end

  def authenticate(raw, resource:)
    raise InvalidGrant unless enabled?
    payload, header = JWT.decode(raw, key.public_key, true, { algorithms: [ "RS256" ], verify_iss: true,
      iss: issuer, verify_aud: true, aud: resource, verify_iat: true, required_claims: %w[sub exp nbf iat jti connection_id scope client_id token_use] })
    raise InvalidGrant unless header["typ"] == "centaur-cli+jwt" && header["kid"] == "cli-v1" && payload["token_use"] == "cli_user" && payload["client_id"] == CLIENT
    row = CliOauthRefreshToken.usable.includes(:user).find_by(connection_id: payload["connection_id"])
    scopes = payload["scope"].split
    raise InvalidGrant unless row&.user&.active? && row.user.oid == payload["sub"] && row.resources.include?(resource) && (scopes - row.scopes).empty?
    [ row, scopes ]
  rescue JWT::DecodeError, ArgumentError
    raise InvalidGrant
  end
end
