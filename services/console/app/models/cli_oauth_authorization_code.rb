class CliOauthAuthorizationCode < ApplicationRecord
  include HashedTokenLookup
  token_hash_attribute :code_hash
  belongs_to :user
end
