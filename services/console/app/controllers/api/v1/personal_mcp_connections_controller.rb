module Api
  module V1
    # Existing admin-key control plane, used by api-rs only after resolving the
    # caller's active owner and explicitly attached personal execution principal.
    # This exposes a projection of native integrations, never a settings store.
    class PersonalMcpConnectionsController < Api::BaseController
      include OauthAppUrls

      def index
        user = User.find(params[:user_id])
        principal = Principal.find_by_oid!(params.require(:principal))
        integrations = ConsoleIntegrations.new(user: user, acting_admin: false)
        response.set_header("Cache-Control", "no-store")
        items = integrations.mcp_connections(principal: principal).map do |item|
          item.merge(authorization_url: oauth_start_url(item[:name]))
        end
        render json: { data: { items: items } }
      end

    end
  end
end
