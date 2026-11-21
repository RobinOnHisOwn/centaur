module CliApiAuthentication
  extend ActiveSupport::Concern

  included do
    skip_before_action :authenticate_api_key!
    before_action :authenticate_cli_user!
    before_action :require_cli_admin!
    after_action :audit_cli_action
    rescue_from CliOauth::Unavailable, with: -> { render_error(status: :service_unavailable, message: "CLI authorization unavailable") }
    rescue_from CliOauth::InvalidGrant, with: -> { render_error(status: :unauthorized, message: "invalid CLI credential") }
  end

  class_methods do
    def accepted_credential = :cli
  end

  def current_user = @cli_connection&.user

  private

  def authenticate_cli_user!
    raise CliOauth::InvalidGrant unless controller_path.start_with?("api/cli/v1/") && request.path.start_with?("/api/cli/v1/")
    value = request.headers["Authorization"].to_s
    raise CliOauth::InvalidGrant unless value.start_with?("Bearer ")
    @cli_connection, @cli_scopes = CliOauth.authenticate(value.delete_prefix("Bearer "), resource: CliOauth.console_resource)
    response.headers["Cache-Control"] = "no-store"
  end

  def require_cli_admin!
    return if current_user&.admin? && @cli_scopes.include?("cli:admin")
    render_error(status: :forbidden, message: "CLI administrator permission required")
  end

  def audit_cli_action
    return unless current_user
    Rails.logger.info({ event: "cli_api", user_id: current_user.oid, connection_id: @cli_connection.connection_id,
      controller: controller_path, action: action_name, status: response.status }.to_json)
  end
end
