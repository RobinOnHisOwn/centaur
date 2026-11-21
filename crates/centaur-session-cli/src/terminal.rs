//! Operator-facing commands; all session and execution state remains in api-rs.
use std::{
    env, fs,
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
};

use centaur_api_server::{
    client::{CentaurClient, ClientError},
    types::{
        AppendMessagesRequest, CreateSessionRequest, ExecuteSessionRequest, ExecuteSessionResponse,
    },
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
    /// Administer Console using the shared browser identity or explicit operator credentials.
    Admin {
        #[command(subcommand)]
        command: crate::admin::AdminCommand,
        /// Emit one JSON object, with diagnostics on stderr.
        #[arg(long, global = true)]
        json: bool,
    },
    /// Inspect or change local configuration (environment values take precedence).
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Sign in through the browser; --secret-stdin retains explicit machine-client setup.
    Login {
        /// Read the client secret from stdin rather than a hidden terminal prompt.
        #[arg(long)]
        secret_stdin: bool,
        /// Canonical Console URL, not the session API URL.
        #[arg(long, conflicts_with = "secret_stdin")]
        server: Option<String>,
        /// Print the login URL without launching the system browser (same machine).
        #[arg(long, conflicts_with = "secret_stdin")]
        no_open: bool,
    },
    /// Revoke the browser CLI connection, or remove explicit saved machine credentials.
    Logout,
    /// Inspect the browser identity and manage your device connections.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Create, inspect and control remote conversations.
    Chat {
        #[command(subcommand)]
        command: ChatCommand,
    },
    /// Connect external Console integrations used by remote agents (not MCP serving).
    #[command(visible_alias = "mcp")]
    Integrations {
        #[command(subcommand)]
        command: McpCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum AuthCommand {
    Status {
        #[arg(long)]
        json: bool,
    },
    Connections {
        #[command(subcommand)]
        command: ConnectionCommand,
    },
}
#[derive(Debug, Subcommand)]
pub(crate) enum ConnectionCommand {
    List,
    Revoke { id: String },
}

#[derive(Debug, Subcommand)]
pub(crate) enum McpCommand {
    /// Deprecated: validate an existing integration URL and print consent; creates nothing.
    Add {
        name: String,
        #[arg(long)]
        url: String,
    },
    /// List registered integrations and account/grant status, not connectivity.
    List,
    /// Print the personal browser-consent link; provider tokens stay in Console.
    #[command(name = "connect", visible_alias = "login")]
    Login { name: String },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ConfigCommand {
    /// Show resolved non-secret settings, sources and credential presence without networking.
    Show,
    /// Remove a saved setting; environment overrides remain active.
    Unset {
        #[arg(value_parser = ["endpoint", "client-id", "principal"])]
        key: String,
    },
    /// Save a setting; changing endpoint/client invalidates the saved credential.
    Set {
        #[arg(value_parser = ["endpoint", "client-id", "principal"])]
        key: String,
        value: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ChatCommand {
    /// Create or reuse a named conversation; attach unless --detach is supplied.
    New {
        #[arg(long)]
        name: Option<String>,
        #[arg(long, value_enum, default_value = "codex")]
        harness: HarnessTypeArg,
        #[arg(long)]
        persona: Option<String>,
        #[arg(long)]
        message: Option<String>,
        /// Return without attaching; accepted work continues remotely.
        #[arg(long)]
        detach: bool,
        /// Print one JSON execution receipt; requires --detach and --message.
        #[arg(long, requires_all = ["detach", "message"])]
        json: bool,
        /// Deadline for each newly submitted turn, in milliseconds (not a session setting).
        #[arg(long, default_value_t = 3_600_000, value_parser = clap::value_parser!(u64).range(1..))]
        max_duration_ms: u64,
    },
    /// Reconnect to a conversation; Ctrl-D detaches, Ctrl-C interrupts remote work.
    Attach {
        name: String,
        /// Deadline for subsequent turns from this attachment; does not alter running work.
        #[arg(long, default_value_t = 3_600_000, value_parser = clap::value_parser!(u64).range(1..))]
        max_duration_ms: u64,
    },
    /// Enqueue a prompt and print its receipt; does not wait for completion.
    Send {
        name: String,
        message: String,
        /// Reuse this ID after an ambiguous network failure; never reuse for different text.
        #[arg(long)]
        request_id: Option<String>,
        /// Print one JSON execution receipt; diagnostics stay on stderr.
        #[arg(long)]
        json: bool,
        /// Deadline for this newly submitted turn, in milliseconds.
        #[arg(long, default_value_t = 3_600_000, value_parser = clap::value_parser!(u64).range(1..))]
        max_duration_ms: u64,
    },
    /// List conversations belonging to this API client.
    List {
        /// Print one JSON object containing all listed items.
        #[arg(long)]
        json: bool,
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
    Archive { name: String },
    /// Make an archived session visible again.
    Unarchive { name: String },
    /// Show server session state and current/latest execution (not a completion wait).
    Status {
        name: String,
        /// Print compact JSON; default is the same state as pretty JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print persisted messages, not a complete conversation; agent events are in logs.
    History { name: String },
    /// Request interruption of the session's active execution.
    Interrupt { name: String },
    /// Print retained raw events as JSONL; --follow keeps reading new events.
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

pub(super) fn config_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("CENTAUR_CONFIG") {
        return Ok(path.into());
    }
    let root = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| eyre!("set HOME or CENTAUR_CONFIG"))?;
    Ok(root.join("centaur/config.json"))
}

pub(super) fn load_config() -> Result<Value> {
    match fs::read(config_path()?) {
        Ok(bytes) => {
            use std::os::unix::fs::PermissionsExt;
            let config: Value = serde_json::from_slice(&bytes)
                .map_err(|_| eyre!("invalid Centaur configuration JSON"))?;
            if !config.is_object() {
                bail!("Centaur configuration must be an object")
            }
            if (config["client-secret"].is_string()
                || config["admin-api-key"].is_string()
                || config["browser"].is_object())
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

pub(super) fn save_config(config: &Value) -> Result<()> {
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

pub(super) fn endpoint(value: &str) -> Result<String> {
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

pub(super) fn setting(config: &Value, name: &str, variable: &str) -> Result<String> {
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

async fn client(config: &Value, args: &Args) -> Result<(CentaurClient, String)> {
    if crate::browser_auth::selected(config, args) {
        return crate::browser_auth::client(config, args).await;
    }
    if args.auth_mode.as_deref() == Some("operator") {
        bail!("operator mode is only for admin commands")
    }
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
    if client_id.starts_with("personal:") && client_id.ends_with(':') {
        return Ok(ThreadKey::parse(format!("{client_id}{name}"))?);
    }
    Ok(ThreadKey::parse(format!("api:{client_id}:{name}"))?)
}

pub(super) fn read_secret(stdin: bool, prompt: &str, stdin_option: &str) -> Result<String> {
    if stdin {
        let mut secret = String::new();
        io::stdin().take(16_385).read_to_string(&mut secret)?;
        if secret.len() > 16_384 {
            bail!("credential input exceeds 16 KiB")
        }
        return Ok(secret.trim_end_matches(['\r', '\n']).to_owned());
    }
    if !io::stdin().is_terminal() {
        bail!("login needs a terminal or {stdin_option}")
    }
    eprint!("{prompt}: ");
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
                    KeyCode::Char(c) => {
                        if secret.len() + c.len_utf8() > 16_384 {
                            bail!("credential input exceeds 16 KiB")
                        }
                        secret.push(c);
                    }
                    _ => {}
                }
            }
        }
    })();
    disable_raw_mode()?;
    eprintln!();
    result
}

fn forget_credential(config: &mut Value) {
    for key in [
        "client-secret",
        "credential-endpoint",
        "credential-client-id",
    ] {
        config
            .as_object_mut()
            .expect("validated configuration")
            .remove(key);
    }
}

fn show_config(config: &Value, args: &Args) -> Result<()> {
    println!("config: {}", config_path()?.display());
    for (key, variable) in [
        ("endpoint", "CENTAUR_API_URL"),
        ("client-id", "CENTAUR_CLIENT_ID"),
        ("principal", ""),
    ] {
        let overridden = env::var(variable).ok().filter(|value| !value.is_empty());
        let (value, source) = if key == "endpoint" && args.api_url.is_some() {
            (
                args.api_url.as_ref().map(|url| url.as_str().to_owned()),
                args.api_url_source.unwrap_or("--api-url"),
            )
        } else if let Some(value) = overridden {
            (Some(value), variable)
        } else {
            (config[key].as_str().map(str::to_owned), "config")
        };
        if key == "endpoint"
            && let Some(value) = &value
        {
            endpoint(value)?; // Never display a URL containing embedded credentials.
        }
        match value {
            Some(value) => println!("{key}: {value:?} (source: {source})"),
            None => println!("{key}: not set"),
        }
    }
    for (label, present) in [
        ("saved client secret", config["client-secret"].is_string()),
        (
            "CENTAUR_CLIENT_SECRET",
            env::var("CENTAUR_CLIENT_SECRET").is_ok_and(|v| !v.is_empty()),
        ),
        (
            "bearer token (argument/environment)",
            !args.api_token.is_empty(),
        ),
    ] {
        println!("{label}: {}", if present { "present" } else { "absent" });
    }
    println!("Session readiness: unknown (configuration inspection makes no requests).");
    Ok(())
}

fn creation_error(error: ClientError) -> eyre::Report {
    if let ClientError::Api { status, body } = &error
        && status.as_u16() == 400
        && body.contains("principal is required: this API client has no default principal")
    {
        return eyre!(
            "This API client has no usable default principal. In Console, attach an approved principal to this API client and select it as default, or use config set principal with an already attached identity. Personal integrations require your linked Console-user principal; do not reuse infrastructure identities."
        );
    }
    error.into()
}

fn print_receipt(
    execution: &ExecuteSessionResponse,
    request_id: &str,
    json_output: bool,
) -> Result<()> {
    if json_output {
        let mut receipt = serde_json::to_value(execution)?;
        receipt["request_id"] = json!(request_id);
        println!("{}", receipt);
    } else {
        println!(
            "Accepted: execution={} status={} request_id={} (not a completion result)",
            execution.execution_id, execution.status, request_id
        );
    }
    Ok(())
}

pub(crate) async fn run(command: &Command, args: &Args) -> Result<()> {
    let _lock = if matches!(
        command,
        Command::Config {
            command: ConfigCommand::Set { .. } | ConfigCommand::Unset { .. }
        } | Command::Login {
            secret_stdin: true,
            ..
        }
    ) {
        Some(crate::browser_auth::lock()?)
    } else {
        None
    };
    let mut config = load_config()?;
    match command {
        Command::Admin { command, json } => return crate::admin::run(command, *json, args).await,
        Command::Auth { command } => {
            return crate::browser_auth::auth(command, &config, args).await;
        }
        Command::Config {
            command: ConfigCommand::Show,
        } => show_config(&config, args)?,
        Command::Config {
            command: ConfigCommand::Unset { key },
        } => {
            config
                .as_object_mut()
                .expect("validated configuration")
                .remove(key);
            if matches!(key.as_str(), "endpoint" | "client-id") {
                forget_credential(&mut config);
            }
            save_config(&config)?;
            println!("{key} removed from config; environment overrides remain active");
        }
        Command::Logout if crate::browser_auth::selected(&config, args) => {
            return crate::browser_auth::logout(args).await;
        }
        Command::Logout => {
            let _lock = crate::browser_auth::lock()?;
            config = load_config()?;
            forget_credential(&mut config);
            save_config(&config)?;
            println!(
                "Saved local credential removed. Argument/environment credentials remain active; no server credential was revoked and no work was stopped."
            );
        }
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
                forget_credential(&mut config);
            }
            config[key] = json!(value);
            save_config(&config)?;
            println!("{key} configured");
        }
        Command::Login {
            secret_stdin: false,
            server,
            no_open,
        } if args.auth_mode.as_deref() == Some("user")
            || (args.auth_mode.as_deref() != Some("machine")
                && (server.is_some()
                    || *no_open
                    || env::var_os("CENTAUR_CLIENT_SECRET").is_none())) =>
        {
            return crate::browser_auth::login(server.as_deref(), *no_open, args).await;
        }
        Command::Login { secret_stdin, .. } => {
            if args.auth_mode.as_deref() == Some("user") {
                bail!("browser login does not accept machine secrets")
            }
            if !args.api_token.is_empty() {
                bail!("unset CENTAUR_API_TOKEN for OAuth client login")
            }
            let secret = match env::var("CENTAUR_CLIENT_SECRET").ok() {
                Some(secret) => secret,
                None => read_secret(*secret_stdin, "Centaur API client secret", "--secret-stdin")?,
            };
            if secret.trim().is_empty() {
                bail!("client secret must not be empty")
            }
            config["client-secret"] = json!(secret);
            config["credential-endpoint"] = json!(resolved_endpoint(&config, args)?);
            config["credential-client-id"] =
                json!(setting(&config, "client-id", "CENTAUR_CLIENT_ID")?);
            if crate::browser_auth::selected(&config, args) {
                bail!("use --auth-mode machine for saved machine-client setup")
            }
            let (client, _) = client(&config, args).await?;
            client.authenticate().await?;
            save_config(&config)?;
            println!(
                "Authenticated; credential saved in {} (mode 0600). Session readiness: unknown; an approved attached principal and session scopes are still required.",
                config_path()?.display()
            );
        }
        Command::Integrations { command } => {
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
            let (client, _) = client(&config, args).await?;
            match command {
                McpCommand::Add { .. } => {
                    eprintln!(
                        "Deprecated: mcp add does not register a server; use integrations connect <name> for existing Console integrations."
                    );
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
            if matches!(command, ChatCommand::New { .. })
                && crate::browser_auth::selected(&config, args)
            {
                crate::browser_auth::prepare(&config, args).await?;
            }
            let (client, id) = client(&config, args).await?;
            match command {
                ChatCommand::New {
                    name,
                    harness,
                    persona,
                    message,
                    detach,
                    json,
                    max_duration_ms,
                } => {
                    let human = client.is_human();
                    if message.as_ref().is_some_and(|text| text.trim().is_empty()) {
                        bail!("prompt must not be empty")
                    }
                    let name = name
                        .clone()
                        .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
                    let key = thread_key(&id, &name)?;
                    if human {
                        let request = CreateSessionRequest {
                            harness_type: (*harness).into(),
                            persona_id: persona.clone(),
                            metadata: None,
                            on_harness_conflict: None,
                            principal: None,
                        };
                        client
                            .create_session(&key, request)
                            .await
                            .map_err(creation_error)?;
                    } else {
                        client
                            .create_session(
                                &key,
                                CreateSessionRequest {
                                    harness_type: (*harness).into(),
                                    persona_id: persona.clone(),
                                    metadata: Some(json!({"source":"terminal","name":name})),
                                    on_harness_conflict: None,
                                    principal: config["principal"].as_str().map(str::to_owned),
                                },
                            )
                            .await
                            .map_err(creation_error)?;
                    }
                    eprintln!("session={name} thread_key={key} (create or reuse)");
                    if let Some(text) = message {
                        let request = Uuid::new_v4().to_string();
                        eprintln!(
                            "request_id={request} (reuse with chat send to retry this exact prompt)"
                        );
                        let execution = if human {
                            client
                                .submit_human_turn(&key, text, &request, *max_duration_ms)
                                .await?
                        } else {
                            send_message(&client, &key, text, &request, *max_duration_ms).await?
                        };
                        print_receipt(&execution, &request, *json)?;
                    }
                    if !detach {
                        attach(client, key, *max_duration_ms).await?;
                    }
                }
                ChatCommand::List { all, json } => {
                    let mut items = Vec::new();
                    if !json {
                        println!("NAME\tHARNESS\tSTATUS\tLAST ACTIVITY");
                    }
                    let prefix = if client.is_human() {
                        format!("personal:{id}:")
                    } else {
                        format!("api:{id}:")
                    };
                    let mut after = String::new();
                    loop {
                        let page = client.list_sessions_with_archived(&after, *all).await?;
                        for item in page["items"]
                            .as_array()
                            .ok_or_else(|| eyre!("invalid session list response"))?
                        {
                            if *json {
                                items.push(item.clone());
                                continue;
                            }
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
                    if *json {
                        println!("{}", json!({"items": items}));
                    }
                }
                ChatCommand::Prune { yes } => {
                    let prefix = if client.is_human() {
                        format!("personal:{id}:")
                    } else {
                        format!("api:{id}:")
                    };
                    let mut after = String::new();
                    let (mut matched, mut deleted, mut skipped, mut failed) = (0, 0, 0, 0);
                    loop {
                        let page = match if client.is_human() {
                            client.list_human_prunable_sessions(&after).await
                        } else {
                            client.list_prunable_sessions(&after).await
                        } {
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
                                match if client.is_human() {
                                    client.prune_human_session(&key).await
                                } else {
                                    client.prune_session(&key).await
                                } {
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
                ChatCommand::Attach {
                    name,
                    max_duration_ms,
                } => {
                    if *max_duration_ms == 0 {
                        bail!("max duration must be positive")
                    }
                    attach(client, thread_key(&id, name)?, *max_duration_ms).await?
                }
                ChatCommand::Send {
                    name,
                    message,
                    request_id,
                    json,
                    max_duration_ms,
                } => {
                    let request = request_id
                        .clone()
                        .unwrap_or_else(|| Uuid::new_v4().to_string());
                    eprintln!("request_id={request} (reuse to retry this exact prompt)");
                    let key = thread_key(&id, name)?;
                    let execution = if client.is_human() {
                        client
                            .submit_human_turn(&key, message, &request, *max_duration_ms)
                            .await?
                    } else {
                        send_message(&client, &key, message, &request, *max_duration_ms).await?
                    };
                    print_receipt(&execution, &request, *json)?;
                }
                ChatCommand::Status { name, json } => {
                    let state = client.session_state(&thread_key(&id, name)?).await?;
                    println!(
                        "{}",
                        if *json {
                            serde_json::to_string(&state)?
                        } else {
                            serde_json::to_string_pretty(&state)?
                        }
                    );
                }
                ChatCommand::History { name } => {
                    let state = client.session_state(&thread_key(&id, name)?).await?;
                    for line in prompt_history(&state) {
                        println!("{line}");
                    }
                    println!("Agent output: use chat logs {name} (retained durable events).");
                }
                ChatCommand::Interrupt { name } => {
                    let key = thread_key(&id, name)?;
                    let result = if client.is_human() {
                        client.interrupt_human_session(&key).await?
                    } else {
                        client.interrupt_session(&key).await?
                    };
                    println!("{result}");
                }
                ChatCommand::Release { name, yes } => {
                    if !yes {
                        bail!(
                            "release destroys workspace and native Pi state, but keeps transcript/events; pass --yes"
                        )
                    }
                    let key = thread_key(&id, name)?;
                    let result = if client.is_human() {
                        client.release_human_sandbox(&key).await?
                    } else {
                        client.release_sandbox(&key).await?
                    };
                    println!("{result}");
                }
                ChatCommand::Archive { name } | ChatCommand::Unarchive { name } => {
                    let archived = matches!(command, ChatCommand::Archive { .. });
                    let key = thread_key(&id, name)?;
                    let result = if client.is_human() {
                        client.archive_human_session(&key, archived).await?
                    } else {
                        client.archive_session(&key, archived).await?
                    };
                    println!("{result}");
                }
                ChatCommand::Delete { name, yes } => {
                    if !yes {
                        bail!(
                            "delete permanently destroys transcript, events, workspace and native harness state; pass --yes"
                        )
                    }
                    let key = thread_key(&id, name)?;
                    let result = if client.is_human() {
                        client.delete_human_session(&key).await?
                    } else {
                        client.delete_session(&key).await?
                    };
                    println!("{result}");
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
) -> Result<ExecuteSessionResponse> {
    if text.trim().is_empty() || request_id.trim().is_empty() || max_duration_ms == 0 {
        bail!("prompt and request ID must not be empty, and max duration must be positive")
    }
    if client.is_human() {
        return Ok(client
            .submit_human_turn(key, text, request_id, max_duration_ms)
            .await?);
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
    client
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
        .wrap_err("enqueue execution (prompt is persisted; retry using the same request ID)")
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
