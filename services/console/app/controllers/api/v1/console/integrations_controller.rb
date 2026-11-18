module Api
  module V1
    module Console
      # GET /api/v1/console/integrations
      #
      # The Integrations page for the caller: enabled OAuth apps with their
      # public consent start links and the caller's connection state, plus (for
      # admins) shared apps and the provider presets still to set up.
      # Open to any active account. Only non-sensitive app fields are returned --
      # never the client id/secret or other users' credentials.
      class IntegrationsController < BaseController
        include OauthAppUrls

        allow_active_users only: %i[index disconnect]

        USAGE = { shared: "shared", always_available: "always_available", direct_messages: "direct_messages" }.freeze

        def index
          response.set_header("Cache-Control", "no-store")
          integrations = ConsoleIntegrations.new(user: current_user, acting_admin: acting_admin?)
          render json: { data: {
            apps: integrations.apps.map { |app| app_payload(app, integrations.credential_for(app)) },
            presets: integrations.presets.map { |provider| preset_payload(provider) },
            ssh_key: { configured: integrations.ssh_key_configured? }
          } }
        end

        def disconnect
          ConsoleIntegrations.new(user: current_user, acting_admin: false).disconnect_mcp!(params[:slug])
          head :no_content
        rescue ActiveRecord::RecordNotDestroyed
          render status: :conflict, json: { error: { message: "The connection is still referenced; ask an operator to disconnect it." } }
        end

        private

        def app_payload(app, credential)
          {
            id: app.oid,
            slug: app.slug,
            provider: app.provider,
            description: app.description,
            shared: app.shared,
            always_available: app.always_available,
            usage: usage(app),
            start_url: oauth_start_url(app.slug),
            mcp_resource: app.mcp_resource,
            can_disconnect_mcp: app.mcp_resource.present? && app.broker_credentials.where(created_by: current_user).exists?,
            connection: credential && {
              status: app.credential_needs_reconnect?(credential) ? "needs_reconnect" : "connected"
            }
          }
        end

        def usage(app)
          return USAGE[:shared] if app.shared?

          app.always_available? ? USAGE[:always_available] : USAGE[:direct_messages]
        end

        def preset_payload(provider)
          { provider: provider.key, display_name: provider.display_name, description: provider.preset[:description] }
        end
      end
    end
  end
end
