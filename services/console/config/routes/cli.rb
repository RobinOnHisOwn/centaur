get ".well-known/oauth-authorization-server/cli", to: "cli/oauth#metadata"
get "cli/oauth/authorize", to: "cli/oauth#authorize"
post "cli/oauth/authorize", to: "cli/oauth#approve"
post "cli/oauth/token", to: "cli/oauth#token"
post "cli/oauth/revoke", to: "cli/oauth#revoke"

namespace :api do
  namespace :cli do
    namespace :v1 do
      get "me", to: "connections#me"
      post "me/principal", to: "connections#prepare"
      get "me/connections", to: "connections#index"
      delete "me/connections/:id", to: "connections#destroy"
      resources :principals, only: %i[index show create update] do
        collection do
          get "lookup/:foreign_id", action: :lookup, constraints: { foreign_id: /[^\/]+/ }
          get "lookup/:foreign_id/effective_config", action: :effective_config, constraints: { foreign_id: /[^\/]+/ }
        end
        member { get :effective_config }
        resources :roles, only: %i[index create destroy], controller: :principal_roles
        resources :grants, only: :index, controller: :grantee_grants
      end
      resources :roles, only: %i[index show create update destroy] do
        collection { get "lookup/:foreign_id", action: :lookup }
        resources :grants, only: :index, controller: :grantee_grants
      end
      resources :grants, only: %i[show create destroy]
    end
  end
end
