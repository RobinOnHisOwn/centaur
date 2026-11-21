module Api
  module Cli
    module V1
      class RolesController < ::Api::V1::RolesController
        include CliApiAuthentication
      end
    end
  end
end
