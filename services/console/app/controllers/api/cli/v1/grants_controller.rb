module Api
  module Cli
    module V1
      class GrantsController < ::Api::V1::GrantsController
        include CliApiAuthentication
      end
    end
  end
end
