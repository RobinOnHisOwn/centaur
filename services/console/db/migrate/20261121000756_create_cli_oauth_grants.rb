class CreateCliOauthGrants < ActiveRecord::Migration[8.1]
  def change
    create_table :cli_oauth_authorization_codes do |t|
      t.references :user, null: false, foreign_key: true
      t.string :code_hash, null: false, index: { unique: true }
      t.string :redirect_uri, null: false
      t.string :code_challenge, null: false
      t.jsonb :scopes, null: false, default: []
      t.jsonb :resources, null: false, default: []
      t.datetime :expires_at, null: false
      t.datetime :consumed_at
      t.timestamps
    end
    create_table :cli_oauth_refresh_tokens do |t|
      t.references :user, null: false, foreign_key: true
      t.string :token_hash, null: false, index: { unique: true }
      t.uuid :connection_id, null: false, index: true
      t.jsonb :scopes, null: false, default: []
      t.jsonb :resources, null: false, default: []
      t.datetime :expires_at, null: false
      t.datetime :connection_expires_at, null: false
      t.datetime :used_at
      t.datetime :revoked_at
      t.timestamps
    end
  end
end
