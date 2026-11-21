module Api
  module Cli
    module V1
      class PrincipalRolesController < ::Api::V1::PrincipalRolesController
        include CliApiAuthentication
      end
    end
  end
end
