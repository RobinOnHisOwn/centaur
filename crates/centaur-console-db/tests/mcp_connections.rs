//! Run against Console's migrated test database, not a synthetic auth schema.
use centaur_console_db::{mcp_connection_identity, oid};
use serde_json::json;
use sqlx::PgPool;

#[tokio::test]
async fn connection_binding_checks_revocation_identity_scope_and_account_live() {
    let Ok(url) = std::env::var("CONSOLE_DB_TEST_DATABASE_URL") else {
        assert!(
            std::env::var_os("CI").is_none(),
            "Console test database required"
        );
        eprintln!("skipping: CONSOLE_DB_TEST_DATABASE_URL is not set");
        return;
    };
    let pool = PgPool::connect(&url).await.unwrap();
    let name = format!("session-mcp-{:032x}", rand::random::<u128>());
    let user: i64 = sqlx::query_scalar("INSERT INTO users (email,status,created_at,updated_at) VALUES ($1,'active',now(),now()) RETURNING id")
        .bind(format!("{name}@example.com")).fetch_one(&pool).await.unwrap();
    let principal: i64 = sqlx::query_scalar("INSERT INTO principals (foreign_id,name,kind,console_user_id,created_by_id,created_at,updated_at) VALUES ($1,'MCP test','console_user',$2,$2,now(),now()) RETURNING id")
        .bind(&name).bind(user).fetch_one(&pool).await.unwrap();
    let client: i64 = sqlx::query_scalar("INSERT INTO mcp_oauth_clients (name,scopes,created_at,updated_at) VALUES ($1,'[\"mcp:sessions:read\"]',now(),now()) RETURNING id")
        .bind(&name).fetch_one(&pool).await.unwrap();
    let connection: String = sqlx::query_scalar("INSERT INTO mcp_oauth_refresh_tokens (token_hash,mcp_oauth_client_id,user_id,principal_id,resource,scopes,expires_at,created_at,updated_at) VALUES ($1,$2,$3,$4,'https://api.example/mcp',$5,now()+interval '1 day',now(),now()) RETURNING connection_id::text")
        .bind(&name).bind(client).bind(user).bind(principal).bind(json!(["mcp:sessions:read"]))
        .fetch_one(&pool).await.unwrap();
    let user_oid = oid("usr", user);
    let principal_oid = oid("prn", principal);
    let client_oid = oid("moc", client);
    let scopes = vec!["mcp:sessions:read".to_owned()];
    let check = |connection: String,
                 user: String,
                 principal: String,
                 client: String,
                 scopes: Vec<String>| {
        let pool = pool.clone();
        async move {
            mcp_connection_identity(
                &pool,
                &connection,
                &user,
                &principal,
                &client,
                "https://api.example/mcp",
                &scopes,
            )
            .await
            .unwrap()
        }
    };
    let identity = check(
        connection.clone(),
        user_oid.clone(),
        principal_oid.clone(),
        client_oid.clone(),
        scopes.clone(),
    )
    .await
    .unwrap();
    assert_eq!(identity.user_id, user);
    assert_eq!(identity.principal_id, principal);
    for (c, u, p, client, s) in [
        (
            "not-a-uuid".into(),
            user_oid.clone(),
            principal_oid.clone(),
            client_oid.clone(),
            scopes.clone(),
        ),
        (
            connection.clone(),
            oid("usr", user + 1),
            principal_oid.clone(),
            client_oid.clone(),
            scopes.clone(),
        ),
        (
            connection.clone(),
            user_oid.clone(),
            oid("prn", principal + 1),
            client_oid.clone(),
            scopes.clone(),
        ),
        (
            connection.clone(),
            user_oid.clone(),
            principal_oid.clone(),
            oid("moc", client + 1),
            scopes.clone(),
        ),
        (
            connection.clone(),
            user_oid.clone(),
            principal_oid.clone(),
            client_oid.clone(),
            vec!["mcp:sessions:write".into()],
        ),
    ] {
        assert!(check(c, u, p, client, s).await.is_none());
    }
    for (change, restore) in [
        (
            "resource='https://other.example/mcp'",
            "resource='https://api.example/mcp'",
        ),
        (
            "expires_at=now()-interval '1 second'",
            "expires_at=now()+interval '1 day'",
        ),
        ("scopes='[]'", "scopes='[\"mcp:sessions:read\"]'"),
    ] {
        sqlx::query(&format!(
            "UPDATE mcp_oauth_refresh_tokens SET {change} WHERE token_hash=$1"
        ))
        .bind(&name)
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            check(
                connection.clone(),
                user_oid.clone(),
                principal_oid.clone(),
                client_oid.clone(),
                scopes.clone()
            )
            .await
            .is_none()
        );
        sqlx::query(&format!(
            "UPDATE mcp_oauth_refresh_tokens SET {restore} WHERE token_hash=$1"
        ))
        .bind(&name)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("UPDATE principals SET console_user_id=NULL WHERE id=$1")
        .bind(principal)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        check(
            connection.clone(),
            user_oid.clone(),
            principal_oid.clone(),
            client_oid.clone(),
            scopes.clone()
        )
        .await
        .is_none()
    );
    sqlx::query("UPDATE principals SET console_user_id=$2 WHERE id=$1")
        .bind(principal)
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        check(
            connection.clone(),
            user_oid.clone(),
            principal_oid.clone(),
            client_oid.clone(),
            scopes.clone()
        )
        .await
        .is_none()
    );
    sqlx::query("UPDATE users SET status='active' WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE mcp_oauth_refresh_tokens SET revoked_at=now() WHERE token_hash=$1")
        .bind(&name)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        check(connection, user_oid, principal_oid, client_oid, scopes)
            .await
            .is_none()
    );
    sqlx::query("DELETE FROM mcp_oauth_refresh_tokens WHERE token_hash=$1")
        .bind(&name)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM mcp_oauth_clients WHERE id=$1")
        .bind(client)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM principals WHERE id=$1")
        .bind(principal)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
}
