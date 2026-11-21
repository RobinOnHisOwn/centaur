require "test_helper"
require "openssl"
require "tempfile"

class CliOauthTest < ActionDispatch::IntegrationTest
  setup do
    @key = Tempfile.new("cli-key", "/tmp")
    @key.chmod(0o600)
    @key.write(OpenSSL::PKey::RSA.generate(2048).to_pem)
    @key.flush
    @env = {
      "CENTAUR_CLI_AUTH_ENABLED" => "true",
      "CENTAUR_CLI_SIGNING_KEY_FILE" => @key.path,
      "CENTAUR_CLI_SESSION_RESOURCE" => "https://api.example.test/api/cli/v1",
      "CENTAUR_CONSOLE_PUBLIC_URL" => "https://console.example.test"
    }
    @old = @env.keys.index_with { |k| ENV[k] }
    @env.each { |k, v| ENV[k] = v }
    @verifier = "v" * 64
    @params = {
      client_id: "centaur-cli", response_type: "code", redirect_uri: "http://127.0.0.1:53123/callback",
      state: "synthetic-state-long-enough", code_challenge: Base64.urlsafe_encode64(Digest::SHA256.digest(@verifier), padding: false),
      code_challenge_method: "S256"
    }
  end

  teardown do
    @old.each { |k, v| v.nil? ? ENV.delete(k) : ENV[k] = v }
    @key.close!
  end

  test "native metadata is distinct and disabled by default" do
    get "/.well-known/oauth-authorization-server/cli"
    assert_response :ok
    assert_equal "https://console.example.test/cli", json_body["issuer"]
    assert_equal [ "S256" ], json_body["code_challenge_methods_supported"]
    ENV.delete("CENTAUR_CLI_AUTH_ENABLED")
    get "/.well-known/oauth-authorization-server/cli"
    assert_response :not_found
  end

  test "browser grant supports scoped admin calls and independent resource tokens" do
    login_user(users(:acme_admin))
    before = [ ApiClient.count, Principal.count, ApiKey.count ]
    token = authorize_and_exchange(admin: true)
    assert_equal before, [ ApiClient.count, Principal.count, ApiKey.count ]
    get "/api/cli/v1/roles", headers: { "Authorization" => "Bearer #{token.fetch('access_token')}" }
    assert_response :ok
    get "/api/v1/roles", headers: { "Authorization" => "Bearer #{token.fetch('access_token')}" }
    assert_response :unauthorized
    session_token = refresh(token.fetch("refresh_token"), ENV.fetch("CENTAUR_CLI_SESSION_RESOURCE"))
    get "/api/cli/v1/roles", headers: { "Authorization" => "Bearer #{session_token.fetch('access_token')}" }
    assert_response :unauthorized
  end

  test "revocation invalidates an unexpired token without revoking another device" do
    login_user(users(:acme_admin))
    a = authorize_and_exchange(admin: true)
    b = authorize_and_exchange(admin: true)
    post "/cli/oauth/revoke", params: { client_id: "centaur-cli", token: a.fetch("refresh_token") }
    assert_response :ok
    get "/api/cli/v1/roles", headers: { "Authorization" => "Bearer #{a.fetch('access_token')}" }
    assert_response :unauthorized
    get "/api/cli/v1/roles", headers: { "Authorization" => "Bearer #{b.fetch('access_token')}" }
    assert_response :ok
  end

  test "rotated refresh replay revokes the active descendant" do
    login_user(users(:acme_admin))
    a = authorize_and_exchange(admin: true)
    b = refresh(a.fetch("refresh_token"), console_resource)
    post "/cli/oauth/token", params: { grant_type: "refresh_token", client_id: "centaur-cli", refresh_token: a.fetch("refresh_token"), resource: console_resource }
    assert_response :bad_request
    get "/api/cli/v1/roles", headers: { "Authorization" => "Bearer #{b.fetch('access_token')}" }
    assert_response :unauthorized
  end

  test "demotion blocks admin while ordinary users can manage their own connections" do
    user = users(:acme_admin)
    login_user(user)
    a = authorize_and_exchange(admin: true)
    user.update!(admin: false)
    get "/api/cli/v1/roles", headers: { "Authorization" => "Bearer #{a.fetch('access_token')}" }
    assert_response :forbidden
    get "/api/cli/v1/me/connections", headers: { "Authorization" => "Bearer #{a.fetch('access_token')}" }
    assert_response :ok
  end

  test "disabling a user revokes every CLI connection and blocks renewal" do
    user=users(:acme_admin)
    login_user(user)
    token=authorize_and_exchange(admin: true)
    user.update!(status: :disabled)
    get "/api/cli/v1/roles", headers: { "Authorization" => "Bearer #{token.fetch('access_token')}" }
    assert_response :unauthorized
    post "/cli/oauth/token", params: { grant_type: "refresh_token", client_id: "centaur-cli", refresh_token: token.fetch("refresh_token"), resource: console_resource }
    assert_response :bad_request
  end

  test "ordinary users explicitly prepare only their own principal after login" do
    login_user(users(:member_user))
    a = authorize_and_exchange(admin: false)
    headers = { "Authorization" => "Bearer #{a.fetch('access_token')}" }
    assert_difference -> { Principal.count }, 1 do
      post "/api/cli/v1/me/principal", headers: headers
      assert_response :ok
    end
    assert_no_difference -> { Principal.count } do
      post "/api/cli/v1/me/principal", headers: headers
      assert_response :ok
    end
    get "/api/cli/v1/me", headers: headers
    assert_response :ok
    assert_equal false, json_body["admin"]
  end

  test "bad verifier and non-loopback redirects cannot produce credentials" do
    login_user(users(:acme_admin))
    get "/cli/oauth/authorize", params: @params.merge(redirect_uri: "https://evil.example/callback")
    assert_response :bad_request
    code = approve_code(admin: false)
    post "/cli/oauth/token", params: exchange_params(code).merge(code_verifier: "wrong")
    assert_response :bad_request
  end

  private

  def console_resource = "https://console.example.test/api/cli/v1"

  def login_user(user)
    console_password_login(user.email)
  end

  def approve_code(admin:)
    get "/cli/oauth/authorize", params: @params
    assert_response :ok
    csrf = console_csrf_token
    post "/cli/oauth/authorize", params: @params.merge(authenticity_token: csrf, decision: "approve", admin: admin ? "1" : "0"), headers: { "Origin" => "http://www.example.com" }
    assert_response :redirect
    query = URI.decode_www_form(URI(response.location).query).to_h
    assert_equal "https://console.example.test/cli", query.fetch("iss")
    query.fetch("code")
  end

  def exchange_params(code)
    { grant_type: "authorization_code", client_id: "centaur-cli", code: code, code_verifier: @verifier, redirect_uri: @params[:redirect_uri], resource: console_resource }
  end

  def authorize_and_exchange(admin:)
    code = approve_code(admin: admin)
    post "/cli/oauth/token", params: exchange_params(code)
    assert_response :ok
    json_body
  end

  def refresh(token, resource)
    post "/cli/oauth/token", params: { grant_type: "refresh_token", client_id: "centaur-cli", refresh_token: token, resource: resource }
    assert_response :ok
    json_body
  end
end
