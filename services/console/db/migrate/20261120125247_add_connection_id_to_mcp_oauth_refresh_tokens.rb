class AddConnectionIdToMcpOauthRefreshTokens < ActiveRecord::Migration[8.1]
  def change
    add_column :mcp_oauth_refresh_tokens, :connection_id, :uuid, null: false, default: -> { "gen_random_uuid()" }
    add_index :mcp_oauth_refresh_tokens, :connection_id
  end
end
