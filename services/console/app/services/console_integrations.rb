# What the Integrations page shows one console user: every enabled OAuth app
# they may connect (shared apps only for admins), their newest
# connection to each, the provider presets an admin can still set up,
# and whether they saved a personal SSH key.
#
# A user's connections are the credentials they minted while signed in
# (created_by, recorded by the consent callback) plus any whose IdP-reported
# email matches their console login -- the fallback for consents made without
# a console session. Every credential of a shared app counts, because a shared
# app has one admin-connected account. Newest wins if several match one app.
class ConsoleIntegrations
  def initialize(user:, acting_admin:)
    @user = user
    @acting_admin = acting_admin
  end

  def apps
    @apps ||= begin
      scope = OauthApp.where(enabled: true).order(:slug)
      scope = scope.where(shared: false) unless @acting_admin
      scope.to_a
    end
  end

  # The user's newest credential for +app+, or nil.
  def credential_for(app)
    credentials_by_app_id[app.id]
  end

  # Non-secret projection, not a second registry. Harness access additionally
  # requires the selected personal principal's existing effective grant.
  def mcp_connections(principal:)
    unless @user.active? && principal.kind == "console_user" && principal.console_user_id == @user.id
      raise ActiveRecord::RecordNotFound
    end

    granted_ids = principal.effective_grants.where.not(static_secret_id: nil).pluck(:static_secret_id)
    owned = BrokerCredential.where(created_by: @user, oauth_app_id: apps.map(&:id)).includes(:static_secret)
      .order(:updated_at, :id).index_by(&:oauth_app_id)
    apps.filter_map do |app|
      next unless app.mcp_resource

      credential = owned[app.id]
      status = if credential.nil?
        "not_connected"
      elsif app.credential_needs_reconnect?(credential)
        "needs_reconnect"
      else
        "connected"
      end
      { id: app.oid, name: app.slug, url: app.mcp_resource, status: status,
        available: status == "connected" && granted_ids.include?(credential.static_secret&.id) }
    end
  end

  # Disconnect is the native account operation, not deletion of a preference:
  # remove only this user's credentials, their wrapping secrets and grants.
  # Other owners, the operator's app and upstream provider tokens are untouched.
  def disconnect_mcp!(slug)
    raise ActiveRecord::RecordNotFound unless @user.active?

    BrokerCredential.transaction do
      app = OauthApp.lock.find_by!(slug: slug, shared: false)
      raise ActiveRecord::RecordNotFound unless app.mcp_resource

      credentials = app.broker_credentials.where(created_by: @user).lock.to_a
      raise ActiveRecord::RecordNotFound if credentials.empty?

      credentials.each do |credential|
        credential.static_secret&.destroy!
        credential.reload.destroy!
      end
    end
  end

  def presets
    @acting_admin ? OauthPresetSetup.available_providers : []
  end

  def ssh_key_configured?
    @user.personal_ssh_key.present?
  end

  private

  def credentials_by_app_id
    @credentials_by_app_id ||= begin
      app_ids = apps.map(&:id)
      shared_ids = apps.select(&:shared?).map(&:id)
      mine = BrokerCredential.where(created_by: @user).or(BrokerCredential.where(provider_email: @user.email))
      mine = mine.or(BrokerCredential.where(oauth_app_id: shared_ids)) if shared_ids.any?
      mine.where(oauth_app_id: app_ids).order(:updated_at, :id).index_by(&:oauth_app_id)
    end
  end
end
