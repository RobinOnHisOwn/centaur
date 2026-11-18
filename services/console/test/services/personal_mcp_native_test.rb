require "test_helper"

# MCP is a projection of existing OAuth apps, owned credentials and grants.
class PersonalMcpNativeTest < ActionDispatch::IntegrationTest
  setup do
    @user = users(:member_user)
    @admin = users(:acme_admin)
    @oauth_app = oauth_apps(:acme_google).dup
    @oauth_app.assign_attributes(slug: "native-docs", provider: "custom", client_secret: "synthetic", labels: { "mcp" => "true" },
      provider_config: { "authorization_endpoint" => "https://auth.example.com/authorize",
        "token_endpoint" => "https://auth.example.com/token", "resource" => "https://mcp.example.com/mcp" })
    @oauth_app.save!
    @principal = Principal.create!(kind: "console_user", console_user: @user, created_by: @admin,
      foreign_id: "native-mcp-test", name: "Native MCP test")
    @integrations = ConsoleIntegrations.new(user: @user, acting_admin: false)
  end

  def connect(user = @user, subject = "native-test")
    credential = BrokerCredential.create!(oauth_app: @oauth_app, created_by: user, provider_subject: subject,
      token_endpoint: @oauth_app.provider_strategy.token_endpoint)
    secret = StaticSecret.create!(name: "Native MCP test", kind: "custom", broker_credential: credential,
      inject_config: { "header" => "Authorization", "formatter" => "Bearer %s" },
      source: SecretSource.new(source_type: "token_broker", config: { "credential_id" => credential.oid }),
      rules: [ RequestRule.new(host: "mcp.example.com", http_methods: [], paths: [ "/mcp" ], position: 0) ])
    Grant.find_or_create_by!(principal: @principal, static_secret: secret) { |grant| grant.created_by = @admin }
    [ credential, secret ]
  end

  test "native catalog keeps admin bearer and personal browser boundaries separate" do
    path = "/api/v1/personal_mcp_connections/#{@user.id}?principal=#{@principal.oid}"
    assert_admin_only_api :get, path
    get path, headers: bearer_headers
    assert_response :ok
    assert_equal @oauth_app.slug, json_body.dig("data", "items", 0, "name")
    foreign = principals(:acme_channel)
    get "/api/v1/personal_mcp_connections/#{@user.id}?principal=#{foreign.oid}", headers: bearer_headers
    assert_response :not_found
  end

  test "the existing integrations UI and native disconnect use the same connection" do
    credential, = connect
    path = "/api/v1/console/integrations/#{@oauth_app.slug}/connection"
    assert_active_user_api :delete, path
    sign_in_console(@user)
    get "/api/v1/console/integrations"
    item = json_body.dig("data", "apps").find { |app| app["slug"] == @oauth_app.slug }
    assert_equal @oauth_app.mcp_resource, item["mcp_resource"]
    assert item["can_disconnect_mcp"]
    delete path, headers: csrf_headers
    assert_response :no_content
    assert_not BrokerCredential.exists?(credential.id)
  end

  def entry
    @integrations.mcp_connections(principal: @principal).find { |item| item[:name] == @oauth_app.slug }
  end

  test "native connection and grant determine MCP availability without a settings record" do
    assert_equal @oauth_app.provider_config["resource"], @oauth_app.mcp_resource
    assert_equal "not_connected", entry[:status]
    credential, secret = connect
    @integrations = ConsoleIntegrations.new(user: @user, acting_admin: false)
    assert entry[:available]
    assert_equal @oauth_app.oid, entry[:id]
    assert_empty entry.keys & %i[access_token refresh_token client_secret]
    @principal.grants.where(static_secret: secret).destroy_all
    assert_not entry[:available]
    credential.update!(dead: true)
    @integrations = ConsoleIntegrations.new(user: @user, acting_admin: false)
    assert_equal "needs_reconnect", entry[:status]
  end

  test "foreign ownership, missing opt-in, shared apps and disabled owners fail closed" do
    connect(@admin)
    assert_not entry[:available]
    assert_raises(ActiveRecord::RecordNotFound) { @integrations.disconnect_mcp!(@oauth_app.slug) }
    shared = @oauth_app.dup
    shared.shared = true
    assert_nil shared.mcp_resource
    @oauth_app.update!(labels: {})
    assert_nil @oauth_app.mcp_resource
    @user.update!(status: "disabled")
    assert_raises(ActiveRecord::RecordNotFound) { @integrations.mcp_connections(principal: @principal) }
  end

  test "a newer email match does not shadow the owner's own credential" do
    connect
    foreign, = connect(@admin, "foreign")
    foreign.update!(provider_email: @user.email)
    assert entry[:available]
  end

  test "credential-bearing resource URLs are never projected onto the integration card" do
    @oauth_app.provider_config["resource"] += "?key=synthetic"
    assert_nil @oauth_app.mcp_resource
  end

  test "native reference protection rolls back a partially attempted disconnect" do
    credential, secret = connect
    reference = secret.dup
    reference.broker_credential = nil
    reference.source = SecretSource.new(source_type: "token_broker", config: { "credential_id" => credential.oid })
    reference.rules = secret.rules.map(&:dup)
    reference.save!
    version = @principal.reload.sync_config_cache_version
    assert_raises(ActiveRecord::RecordNotDestroyed) { @integrations.disconnect_mcp!(@oauth_app.slug) }
    assert BrokerCredential.exists?(credential.id)
    assert StaticSecret.exists?(secret.id)
    assert @principal.grants.exists?(static_secret: secret)
    assert_equal version, @principal.reload.sync_config_cache_version
  end

  test "disconnect removes all own native connections and invalidates proxy grants without deleting the app" do
    first, secret = connect
    second, = connect(@user, "second-account")
    version = @principal.reload.sync_config_cache_version
    @integrations.disconnect_mcp!(@oauth_app.slug)
    assert OauthApp.exists?(@oauth_app.id)
    assert_not BrokerCredential.exists?(first.id)
    assert_not BrokerCredential.exists?(second.id)
    assert_not StaticSecret.exists?(secret.id)
    assert_empty @principal.grants
    assert_operator @principal.reload.sync_config_cache_version, :>, version
  end
end
