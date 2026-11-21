module Api
  module Cli
    module V1
      class ConnectionsController < ::Api::BaseController
        include CliApiAuthentication
        skip_before_action :require_cli_admin!
        before_action { raise CliOauth::InvalidGrant unless @cli_scopes.include?("cli:identity") }

        def me
          render json: { user: current_user.oid, admin: current_user.admin? && @cli_scopes.include?("cli:admin") }
        end

        def prepare
          principal = ConsoleUserPrincipalProvisioner.call(current_user)
          render json: { principal: principal.oid }
        rescue ConsoleUserPrincipalProvisioner::IdentityConflict
          render_error(status: :conflict, message: "personal identity needs operator reconciliation")
        end

        def index
          rows = CliOauthRefreshToken.usable.where(user: current_user).order(created_at: :desc).limit(100)
          render json: { items: rows.map { |row| { id: row.connection_id, created_at: row.created_at,
            expires_at: row.connection_expires_at, current: row.connection_id == @cli_connection.connection_id } } }
        end

        def destroy
          raise ActiveRecord::RecordNotFound unless params[:id].to_s.match?(/\A[0-9a-f-]{36}\z/)
          row = CliOauthRefreshToken.where(user: current_user, connection_id: params[:id]).first!
          row.revoke!
          head :no_content
        end
      end
    end
  end
end
