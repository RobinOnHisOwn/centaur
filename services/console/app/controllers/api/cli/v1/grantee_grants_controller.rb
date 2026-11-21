module Api
  module Cli
    module V1
      class GranteeGrantsController < ::Api::V1::GranteeGrantsController
        include CliApiAuthentication
      end
    end
  end
end
