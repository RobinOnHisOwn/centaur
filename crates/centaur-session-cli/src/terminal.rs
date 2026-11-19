//! Operator-facing commands; all session and execution state remains in api-rs.
use std::{
    env, fs,
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
};

use centaur_api_server::{
    client::{CentaurClient, ClientError},
    types::{AppendMessagesRequest, CreateSessionRequest, ExecuteSessionRequest},
};
use centaur_session_core::{MessageRole, SessionMessageInput, ThreadKey};
use clap::Subcommand;
use crossterm::{
    event::{self, Event, KeyCode},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use eyre::{Result, WrapErr, bail, eyre};
use futures_util::StreamExt;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{Args, HarnessTypeArg, tui, user_input_line};

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Login {
        #[arg(long)]
        secret_stdin: bool,
    },
    Chat {
        #[command(subcommand)]
        command: ChatCommand,
    },
    /// Connect existing Console MCP integrations for new Codex sessions.
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    Add {
        name: String,
        #[arg(long)]
        url: String,
    },
    List,
    /// Print the personal browser-consent link; provider tokens stay in Console.
    Login {
        name: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    Set {
        #[arg(value_parser = ["endpoint", "client-id", "principal"])]
        key: String,
        value: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ChatCommand {
    New {
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_enum, default_value = "codex")]
        harness: HarnessTypeArg,
        #[arg(long)]
        persona: Option<String>,
        #[arg(long)]
        message: Option<String>,
        #[arg(long)]
        detach: bool,
        #[arg(long, default_value_t = 3_600_000)]
        max_duration_ms: u64,
    },
    Attach {
        name: String,
    },
    Send {
        name: String,
        message: String,
        /// Reuse this ID after an ambiguous network failure; never reuse for different text.
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long, default_value_t = 3_600_000)]
        max_duration_ms: u64,
    },
    List {
        /// Include archived sessions.
        #[arg(long)]
        all: bool,
    },
    /// Preview inactive sessions older than 30 days; --yes permanently deletes them.
    Prune {
        #[arg(long)]
        yes: bool,
    },
    /// Hide an idle session from the default list; preserve history and files.
    Archive {
        name: String,
    },
    /// Make an archived session visible again.
    Unarchive {
        name: String,
    },
    Status {
        name: String,
    },
    History {
        name: String,
    },
    Interrupt {
        name: String,
    },
    Logs {
        name: String,
        #[arg(long, default_value_t = 0)]
        after_event_id: i64,
        #[arg(long)]
        follow: bool,
    },
    /// Deletes filesystem/native harness state, NOT the persisted transcript.
    Release {
        name: String,
        #[arg(long)]
        yes: bool,
    },
    /// Permanently delete an idle session, its history and sandbox files.
    Delete {
        name: String,
        #[arg(long)]
        yes: bool,
    },
}

fn config_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("CENTAUR_CONFIG") {
        return Ok(path.into());
    }
    let root = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| eyre!("set HOME or CENTAUR_CONFIG"))?;
    Ok(root.join("centaur/config.json"))
}

fn load_config() -> Result<Value> {
    match fs::read(config_path()?) {
        Ok(bytes) => {
            use std::os::unix::fs::PermissionsExt;
            let config: Value =
                serde_json::from_slice(&bytes).wrap_err("parse Centaur configuration")?;
            if !config.is_object() {
                bail!("Centaur configuration must be an object")
            }
            if config["client-secret"].is_string()
                && fs::metadata(config_path()?)?.permissions().mode() & 0o077 != 0
            {
                bail!("credential configuration must have permissions 0600")
            }
            Ok(config)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(error.into()),
    }
}

fn save_config(config: &Value) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = config_path()?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    fs::create_dir_all(parent)?;
    // Atomic replacement requires the temporary file to be on the same filesystem.
    let temporary = parent.join(format!(".centaur-config-{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec_pretty(config)?)?;
        file.sync_all()?;
        fs::rename(&temporary, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn endpoint(value: &str) -> Result<String> {
    let url = reqwest::Url::parse(value).wrap_err("endpoint must be an absolute HTTPS URL")?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || (url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        bail!(
            "endpoint requires HTTPS (HTTP allowed only on loopback), no credentials/query/fragment"
        )
    }
    Ok(value.trim_end_matches('/').to_owned())
}

fn setting(config: &Value, name: &str, variable: &str) -> Result<String> {
    env::var(variable)
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            config[name]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .ok_or_else(|| eyre!("configure {name} or set {variable}"))
}

fn client(config: &Value, args: &Args) -> Result<(CentaurClient, String)> {
    let url = resolved_endpoint(config, args)?;
    let id = setting(config, "client-id", "CENTAUR_CLIENT_ID")?;
    if !id
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        bail!("invalid API client ID")
    }
    let client = if !args.api_token.is_empty() {
        CentaurClient::with_bearer_token(url, &args.api_token)?
    } else {
        let secret = match env::var("CENTAUR_CLIENT_SECRET")
            .ok()
            .filter(|secret| !secret.is_empty())
        {
            Some(secret) => secret,
            None => {
                if config["credential-endpoint"].as_str() != Some(&url)
                    || config["credential-client-id"].as_str() != Some(&id)
                {
                    bail!(
                        "saved credential belongs to a different endpoint/client; run login again"
                    )
                }
                setting(config, "client-secret", "CENTAUR_CLIENT_SECRET")?
            }
        };
        CentaurClient::with_client_credentials(&url, id.clone(), secret)?
    };
    Ok((client, id))
}

fn resolved_endpoint(config: &Value, args: &Args) -> Result<String> {
    let url = match &args.api_url {
        Some(url) => url.as_str().to_owned(),
        None => setting(config, "endpoint", "CENTAUR_API_URL")?,
    };
    endpoint(&url)
}

fn thread_key(client_id: &str, name: &str) -> Result<ThreadKey> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        bail!("session name must contain 1–64 ASCII letters, digits, '-' or '_'")
    }
    Ok(ThreadKey::parse(format!("api:{client_id}:{name}"))?)
}

fn read_secret(stdin: bool) -> Result<String> {
    if stdin {
        let mut secret = String::new();
        io::stdin().take(16_384).read_to_string(&mut secret)?;
        return Ok(secret.trim_end_matches(['\r', '\n']).to_owned());
    }
    if !io::stdin().is_terminal() {
        bail!("login needs a terminal or --secret-stdin")
    }
    eprint!("Centaur API client secret: ");
    io::stderr().flush()?;
    enable_raw_mode()?;
    let result = (|| -> Result<String> {
        let mut secret = String::new();
        loop {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Enter => return Ok(secret),
                    KeyCode::Esc => bail!("login cancelled"),
                    KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                        bail!("login cancelled")
                    }
                    KeyCode::Backspace => {
                        secret.pop();
                    }
                    KeyCode::Char(c) => secret.push(c),
                    _ => {}
                }
            }
        }
    })();
    disable_raw_mode()?;
    eprintln!();
    result
}

pub(crate) async fn run(command: &Command, args: &Args) -> Result<()> {
    let mut config = load_config()?;
    match command {
        Command::Config {
            command: ConfigCommand::Set { key, value },
        } => {
            if key == "endpoint" {
                endpoint(value)?;
            }
            if config[key].as_str() != Some(value)
                && matches!(key.as_str(), "endpoint" | "client-id")
            {
                // Never forward a saved secret to a newly selected endpoint/client.
                config
                    .as_object_mut()
                    .ok_or_else(|| eyre!("configuration must be an object"))?
                    .remove("client-secret");
            }
            config[key] = json!(value);
            save_config(&config)?;
            println!("{key} configured");
        }
        Command::Login { secret_stdin } => {
            if !args.api_token.is_empty() {
                bail!("unset CENTAUR_API_TOKEN for OAuth client login")
            }
            let secret = match env::var("CENTAUR_CLIENT_SECRET").ok() {
                Some(secret) => secret,
                None => read_secret(*secret_stdin)?,
            };
            if secret.trim().is_empty() {
                bail!("client secret must not be empty")
            }
            config["client-secret"] = json!(secret);
            config["credential-endpoint"] = json!(resolved_endpoint(&config, args)?);
            config["credential-client-id"] =
                json!(setting(&config, "client-id", "CENTAUR_CLIENT_ID")?);
            let (client, _) = client(&config, args)?;
            client.authenticate().await?;
            save_config(&config)?;
            println!(
                "Authenticated; credential saved in {} (mode 0600)",
                config_path()?.display()
            );
        }
        Command::Mcp { command } => {
            use centaur_session_core::mcp::{McpServer, valid_mcp_name};
            // Validate before authenticating; the server also requires this URL
            // to match an existing operator-approved Console integration.
            let server = if let McpCommand::Add { name, url } = command {
                let server = McpServer {
                    name: name.clone(),
                    url: url.clone(),
                };
                server.validate().map_err(|error| eyre!(error))?;
                Some(server)
            } else {
                None
            };
            if let McpCommand::Login { name } = command
                && !valid_mcp_name(name)
            {
                bail!("invalid MCP name")
            }
            let (client, _) = client(&config, args)?;
            match command {
                McpCommand::Add { .. } => {
                    let result = client
                        .connect_mcp_integration(
                            server
                                .as_ref()
                                .ok_or_else(|| eyre!("missing MCP settings"))?,
                        )
                        .await?;
                    let url = result["authorization_url"]
                        .as_str()
                        .ok_or_else(|| eyre!("backend returned no consent URL"))?;
                    endpoint(url)?;
                    println!(
                        "Existing Console integration found. Complete personal consent:\n{url}\nNo separate MCP configuration record was created."
                    );
                }
                McpCommand::List => {
                    let value = client.list_mcp_servers().await?;
                    let items = value["items"]
                        .as_array()
                        .ok_or_else(|| eyre!("invalid integration listing"))?;
                    println!("NAME\tURL\tACCOUNT STATUS\tAVAILABLE TO PRINCIPAL");
                    for item in items {
                        let server = McpServer {
                            name: item["name"]
                                .as_str()
                                .ok_or_else(|| eyre!("missing integration name"))?
                                .into(),
                            url: item["url"]
                                .as_str()
                                .ok_or_else(|| eyre!("missing integration URL"))?
                                .into(),
                        };
                        server.validate().map_err(|error| eyre!(error))?;
                        let status = item["status"]
                            .as_str()
                            .filter(|status| {
                                matches!(*status, "connected" | "not_connected" | "needs_reconnect")
                            })
                            .ok_or_else(|| eyre!("invalid account status"))?;
                        let available = item["available"]
                            .as_bool()
                            .ok_or_else(|| eyre!("missing grant status"))?;
                        println!("{}\t{}\t{}\t{}", server.name, server.url, status, available);
                    }
                }
                McpCommand::Login { name } => {
                    let result = client.login_mcp_server(name).await?;
                    let url = result["authorization_url"]
                        .as_str()
                        .ok_or_else(|| eyre!("backend returned no consent URL"))?;
                    endpoint(url)?;
                    println!(
                        "Open this URL and sign in with your personal Console account:\n{url}\nConsent and token refresh are server-side. This command does not confirm that consent completed."
                    );
                }
            }
        }
        Command::Chat { command } => {
            let (client, id) = client(&config, args)?;
            match command {
                ChatCommand::New {
                    name,
                    harness,
                    persona,
                    message,
                    detach,
                    max_duration_ms,
                } => {
                    let name = name
                        .clone()
                        .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
                    let key = thread_key(&id, &name)?;
                    client
                        .create_session(
                            &key,
                            CreateSessionRequest {
                                harness_type: (*harness).into(),
                                persona_id: persona.clone(),
                                metadata: Some(json!({"source":"terminal", "name": name})),
                                on_harness_conflict: None,
                                principal: config["principal"].as_str().map(str::to_owned),
                            },
                        )
                        .await?;
                    eprintln!("session={name} thread_key={key} (create or reuse)");
                    if let Some(text) = message {
                        let request = Uuid::new_v4().to_string();
                        eprintln!(
                            "request_id={request} (reuse with chat send to retry this exact prompt)"
                        );
                        send_message(&client, &key, text, &request, *max_duration_ms).await?;
                    }
                    if !detach {
                        attach(client, key, *max_duration_ms).await?;
                    }
                }
                ChatCommand::List { all } => {
                    println!("NAME\tHARNESS\tSTATUS\tLAST ACTIVITY");
                    let prefix = format!("api:{id}:");
                    let mut after = String::new();
                    loop {
                        let page = client.list_sessions_with_archived(&after, *all).await?;
                        for item in page["items"]
                            .as_array()
                            .ok_or_else(|| eyre!("invalid session list response"))?
                        {
                            let key = item["thread_key"].as_str().unwrap_or("");
                            println!(
                                "{}\t{}\t{}\t{}",
                                key.strip_prefix(&prefix).unwrap_or(key),
                                item["harness_type"].as_str().unwrap_or("?"),
                                if item["archived"].as_bool() == Some(true) {
                                    "archived"
                                } else {
                                    item["status"].as_str().unwrap_or("?")
                                },
                                item["updated_at"].as_str().unwrap_or("?")
                            );
                        }
                        let Some(next) = page["next_after_thread_key"].as_str() else {
                            break;
                        };
                        if next <= after.as_str() {
                            bail!("session list cursor did not advance")
                        }
                        after = next.to_owned();
                    }
                }
                ChatCommand::Prune { yes } => {
                    let prefix = format!("api:{id}:");
                    let mut after = String::new();
                    let (mut matched, mut deleted, mut skipped, mut failed) = (0, 0, 0, 0);
                    loop {
                        let page = match client.list_prunable_sessions(&after).await {
                            Ok(page) => page,
                            Err(error) => {
                                failed += 1;
                                eprintln!("prune scan stopped after partial progress: {error}");
                                break;
                            }
                        };
                        if page["prune_older_than_days"].as_u64() != Some(30) {
                            bail!(
                                "backend does not support safe 30-day pruning; no deletion attempted for this page"
                            )
                        }
                        for item in page["items"]
                            .as_array()
                            .ok_or_else(|| eyre!("invalid prune response"))?
                        {
                            let raw = item["thread_key"]
                                .as_str()
                                .ok_or_else(|| eyre!("prune candidate has no thread key"))?;
                            let name = raw
                                .strip_prefix(&prefix)
                                .filter(|s| !s.is_empty())
                                .ok_or_else(|| {
                                    eyre!("prune candidate is outside your namespace")
                                })?;
                            let key = ThreadKey::parse(raw)?;
                            matched += 1;
                            println!("{name:?}\t{}", item["updated_at"]);
                            if *yes {
                                match client.prune_session(&key).await {
                                    Ok(result) => match result["deleted"].as_bool() {
                                        Some(true) => deleted += 1,
                                        Some(false) => skipped += 1,
                                        None => {
                                            failed += 1;
                                            eprintln!("{key:?}: invalid prune result");
                                        }
                                    },
                                    Err(ClientError::Api { status, .. })
                                        if matches!(status.as_u16(), 404 | 409) =>
                                    {
                                        skipped += 1
                                    }
                                    Err(error) => {
                                        failed += 1;
                                        eprintln!("{key:?}: {error}");
                                    }
                                }
                            }
                        }
                        let Some(next) = page["next_after_thread_key"].as_str() else {
                            break;
                        };
                        if next <= after.as_str() {
                            bail!("prune cursor did not advance")
                        }
                        after = next.to_owned();
                    }
                    println!(
                        "matched={matched} deleted={deleted} skipped={skipped} failed={failed}"
                    );
                    if !yes {
                        println!(
                            "Preview only. chat prune --yes permanently deletes matching history and sandbox files."
                        );
                    }
                    if failed > 0 {
                        bail!("prune finished with {failed} failure(s); inspect before retrying")
                    }
                }
                ChatCommand::Attach { name } => {
                    attach(client, thread_key(&id, name)?, 3_600_000).await?
                }
                ChatCommand::Send {
                    name,
                    message,
                    request_id,
                    max_duration_ms,
                } => {
                    let request = request_id
                        .clone()
                        .unwrap_or_else(|| Uuid::new_v4().to_string());
                    eprintln!("request_id={request} (reuse to retry this exact prompt)");
                    send_message(
                        &client,
                        &thread_key(&id, name)?,
                        message,
                        &request,
                        *max_duration_ms,
                    )
                    .await?;
                }
                ChatCommand::Status { name } => println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &client.session_state(&thread_key(&id, name)?).await?
                    )?
                ),
                ChatCommand::History { name } => {
                    let state = client.session_state(&thread_key(&id, name)?).await?;
                    for line in prompt_history(&state) {
                        println!("{line}");
                    }
                    println!("Agent output: use chat logs {name} (retained durable events).");
                }
                ChatCommand::Interrupt { name } => println!(
                    "{}",
                    client.interrupt_session(&thread_key(&id, name)?).await?
                ),
                ChatCommand::Release { name, yes } => {
                    if !yes {
                        bail!(
                            "release destroys workspace and native Pi state, but keeps transcript/events; pass --yes"
                        )
                    }
                    println!("{}", client.release_sandbox(&thread_key(&id, name)?).await?);
                }
                ChatCommand::Archive { name } | ChatCommand::Unarchive { name } => {
                    let archived = matches!(command, ChatCommand::Archive { .. });
                    println!(
                        "{}",
                        client
                            .archive_session(&thread_key(&id, name)?, archived)
                            .await?
                    );
                }
                ChatCommand::Delete { name, yes } => {
                    if !yes {
                        bail!(
                            "delete permanently destroys transcript, events, workspace and native harness state; pass --yes"
                        )
                    }
                    println!("{}", client.delete_session(&thread_key(&id, name)?).await?);
                }
                ChatCommand::Logs {
                    name,
                    after_event_id,
                    follow,
                } => {
                    let key = thread_key(&id, name)?;
                    let state = client.session_state(&key).await?;
                    let end = state["last_event_id"]
                        .as_i64()
                        .ok_or_else(|| eyre!("missing event high-water mark"))?;
                    if !follow && end <= *after_event_id {
                        return Ok(());
                    }
                    let mut events = client.reconnecting_events(&key, *after_event_id);
                    while let Some(event) = events.next().await {
                        let event = event?;
                        let number = event.id.parse::<i64>().unwrap_or(0);
                        if !follow && number > end {
                            break;
                        }
                        println!(
                            "{}",
                            json!({"id":event.id,"event":event.event,"data":crate::parse_json_or_string(&event.data)})
                        );
                        if !follow && number == end {
                            break;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn prompt_history(state: &Value) -> Vec<String> {
    state["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|message| {
            let text = message["parts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}: {text}", message["role"].as_str().unwrap_or("message"))
        })
        .collect()
}

async fn attach(client: CentaurClient, key: ThreadKey, max_duration_ms: u64) -> Result<()> {
    let state = client
        .session_state(&key)
        .await
        .wrap_err("load existing session (backend must include terminal workflow API)")?;
    let events = client.reconnecting_events(&key, 0);
    tui::run(
        client,
        key,
        events,
        tui::TuiOptions {
            debug_visible: false,
            idle_timeout_ms: 1_000,
            max_duration_ms,
            exit_on_terminal: false,
            exit_on_output_type: None,
            initial_state: Some(state),
        },
    )
    .await
}

/// A stable request ID ties message deduplication to execution idempotency.
/// Do not retry with a new ID after an ambiguous response.
pub(crate) async fn send_message(
    client: &CentaurClient,
    key: &ThreadKey,
    text: &str,
    request_id: &str,
    max_duration_ms: u64,
) -> Result<()> {
    if text.trim().is_empty() || request_id.trim().is_empty() || max_duration_ms == 0 {
        bail!("prompt and request ID must not be empty, and max duration must be positive")
    }
    client
        .append_messages(
            key,
            AppendMessagesRequest {
                messages: vec![SessionMessageInput {
                    client_message_id: Some(request_id.to_owned()),
                    role: MessageRole::User,
                    parts: vec![json!({"type":"text", "text":text})],
                    metadata: json!({"source":"terminal"}),
                }],
            },
        )
        .await
        .wrap_err("append prompt (retry using the same request ID)")?;
    let _execution = client
        .execute_session(
            key,
            ExecuteSessionRequest {
                idempotency_key: Some(request_id.to_owned()),
                metadata: Some(json!({"source":"terminal"})),
                input_lines: vec![user_input_line(text)?],
                idle_timeout_ms: None,
                max_duration_ms: Some(max_duration_ms),
            },
        )
        .await
        .wrap_err("enqueue execution (prompt is persisted; retry using the same request ID)")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_isolated_and_cannot_escape_the_namespace() {
        assert_eq!(
            thread_key("acl_a", "one").unwrap().as_str(),
            "api:acl_a:one"
        );
        assert_ne!(
            thread_key("acl_a", "one").unwrap(),
            thread_key("acl_a", "two").unwrap()
        );
        for name in ["", "../other", "api:other:session", "two words", "a/b"] {
            assert!(thread_key("acl_a", name).is_err());
        }
    }
    #[test]
    fn credentials_require_tls_and_cannot_hide_in_endpoint_urls() {
        for url in [
            "http://remote.example",
            "https://user:secret@host",
            "https://host/?token=x",
            "file:///tmp/x",
        ] {
            assert!(endpoint(url).is_err(), "{url}");
        }
        for url in ["https://api.example", "http://127.0.0.1:8080"] {
            assert!(endpoint(url).is_ok());
        }
    }
}
