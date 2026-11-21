require "base64"
require "digest"

module Cli
  class OauthController < ApplicationController
    layout "oauth"
    include ServerRenderedPagePolicy
    skip_before_action :require_login, :require_active_account
    skip_forgery_protection only: %i[token revoke]
    before_action :require_cli_enabled
    before_action { response.headers["Cache-Control"] = "no-store" }
    rate_limit to: 60, within: 1.minute, only: %i[authorize token], with: -> { head :too_many_requests }
    rescue_from CliOauth::InvalidGrant, with: -> { oauth_error("invalid_grant") }
    rescue_from CliOauth::Unavailable, with: -> { render json: { error: "temporarily_unavailable" }, status: :service_unavailable }

    def metadata
      render json: { issuer: CliOauth.issuer, authorization_endpoint: "#{CliOauth.base}/cli/oauth/authorize",
        token_endpoint: "#{CliOauth.base}/cli/oauth/token", revocation_endpoint: "#{CliOauth.base}/cli/oauth/revoke",
        response_types_supported: [ "code" ], grant_types_supported: %w[authorization_code refresh_token],
        code_challenge_methods_supported: [ "S256" ], token_endpoint_auth_methods_supported: [ "none" ],
        authorization_response_iss_parameter_supported: true, client_id: CliOauth::CLIENT,
        session_resource: CliOauth.session_resource, console_resource: CliOauth.console_resource }
    end

    def authorize
      return redirect_to_login unless current_user
      return head :forbidden unless current_user.active?
      validate_authorization!
      @authorization_params = params.permit(:client_id, :response_type, :redirect_uri, :state, :code_challenge, :code_challenge_method).to_h
      @admin = current_user.admin?
      @signed_in_email = current_user.email
    end

    def approve
      return head :unauthorized unless current_user&.active?
      return head :forbidden unless request.origin == request.base_url && verified_request?
      validate_authorization!
      unless params[:decision] == "approve"
        return callback(error: "access_denied")
      end
      scopes = CliOauth::SCOPES.dup
      scopes << "cli:admin" if current_user.admin? && params[:admin] == "1"
      code = "clicode_#{SecureRandom.urlsafe_base64(48)}"
      CliOauthAuthorizationCode.create!(user: current_user, code_hash: CliOauthAuthorizationCode.hash_token(code),
        redirect_uri: params[:redirect_uri], code_challenge: params[:code_challenge], scopes: scopes,
        resources: CliOauth.resources, expires_at: 5.minutes.from_now)
      callback(code: code)
    end

    def token
      require_client!
      resource = params[:resource].to_s
      raise CliOauth::InvalidGrant unless CliOauth.resources.include?(resource)
      CliOauth.key # Reject unavailable signing configuration before consuming a credential.
      row, raw = case params[:grant_type]
      when "authorization_code" then exchange_code
      when "refresh_token" then exchange_refresh
      else return oauth_error("unsupported_grant_type")
      end
      render json: CliOauth.token_response(row, raw, resource)
    end

    def revoke
      require_client!
      raise CliOauth::InvalidGrant if params[:token].blank?
      row = CliOauthRefreshToken.find_by(token_hash: CliOauthRefreshToken.hash_token(params[:token]))
      row&.revoke!
      head :ok
    end

    private

    def require_cli_enabled
      head :not_found unless CliOauth.enabled?
    end

    def require_client!
      raise CliOauth::InvalidGrant unless params[:client_id] == CliOauth::CLIENT && request.content_length.to_i <= 16_384
    end

    def validate_authorization!
      require_client!
      uri = URI.parse(params[:redirect_uri].to_s)
      valid = uri.is_a?(URI::HTTP) && !uri.is_a?(URI::HTTPS) && %w[127.0.0.1 [::1]].include?(uri.host) && uri.userinfo.nil? && uri.query.nil? && uri.fragment.nil? && uri.path == "/callback"
      raise CliOauth::InvalidGrant unless valid && params[:response_type] == "code" && params[:code_challenge_method] == "S256" && params[:code_challenge].to_s.match?(/\A[A-Za-z0-9_-]{43}\z/) && params[:state].to_s.match?(/\A[A-Za-z0-9_-]{16,128}\z/)
    rescue URI::InvalidURIError
      raise CliOauth::InvalidGrant
    end

    def exchange_code
      result = nil
      CliOauthAuthorizationCode.transaction do
        code = CliOauthAuthorizationCode.lock.find_by(code_hash: CliOauthAuthorizationCode.hash_token(params[:code]))
        if code
          code.user.with_lock do
            actual = Base64.urlsafe_encode64(Digest::SHA256.digest(params[:code_verifier].to_s), padding: false)
            if code.consumed_at.nil? && code.expires_at > Time.current && code.user.active? && code.redirect_uri == params[:redirect_uri] && code.resources.include?(params[:resource]) && params[:code_verifier].to_s.match?(/\A[A-Za-z0-9._~-]{43,128}\z/) && ActiveSupport::SecurityUtils.secure_compare(actual, code.code_challenge)
              code.update!(consumed_at: Time.current)
              result = CliOauth.issue_refresh(user: code.user, scopes: code.scopes, resources: code.resources)
            end
          end
        end
      end
      result || raise(CliOauth::InvalidGrant)
    end

    def exchange_refresh
      row = CliOauthRefreshToken.find_by(token_hash: CliOauthRefreshToken.hash_token(params[:refresh_token]))
      raise CliOauth::InvalidGrant unless row
      result = nil
      row.user.with_lock do
        row.with_connection_lock do
          row.reload
          if row.used_at.present?
            row.revoke! # Replay of an ancestor invalidates its currently active descendant.
          elsif row.revoked_at.nil? && row.expires_at > Time.current && row.connection_expires_at > Time.current && row.user.active? && row.resources.include?(params[:resource])
            row.update!(used_at: Time.current)
            result = CliOauth.issue_refresh(user: row.user, scopes: row.scopes, resources: row.resources,
              connection_id: row.connection_id, deadline: row.connection_expires_at)
          end
        end
      end
      result || raise(CliOauth::InvalidGrant)
    end

    def callback(**values)
      uri = URI(params[:redirect_uri])
      uri.query = URI.encode_www_form(values.merge(state: params[:state], iss: CliOauth.issuer))
      redirect_to uri.to_s, allow_other_host: true
    end

    def redirect_to_login
      remember_console_return_path(request.fullpath)
      redirect_to login_path
    end

    def oauth_error(code)
      render json: { error: code }, status: :bad_request
    end
  end
end
