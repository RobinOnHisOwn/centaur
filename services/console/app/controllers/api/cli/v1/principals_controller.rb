module Api
  module Cli
    module V1
      class PrincipalsController < ::Api::V1::PrincipalsController
        include CliApiAuthentication
      end
    end
  end
end
