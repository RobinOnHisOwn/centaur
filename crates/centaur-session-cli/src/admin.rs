//! Thin client of Console's existing bearer operator API, not its session-only API.
use std::{
    env,
    fs::File,
    io::{self, Read},
    time::Duration,
};

use clap::{Args, Subcommand};
use eyre::{Result, bail, eyre};
use reqwest::{
    Method,
    header::{AUTHORIZATION, HeaderValue},
};
use serde_json::{Value, json};

use crate::terminal::{endpoint, load_config, read_secret, save_config, setting};

const MAX_BYTES: usize = 1024 * 1024;
const GRANTEES: &[(&str, &str)] = &[("principal_id", "prn_"), ("role_id", "role_")];
const SECRETS: &[(&str, &str)] = &[
    ("static_secret_id", "ssr_"),
    ("gcp_auth_secret_id", "gas_"),
    ("gcp_id_token_secret_id", "gid_"),
    ("aws_auth_secret_id", "aas_"),
    ("oauth_token_secret_id", "ots_"),
    ("pg_dsn_secret_id", "pgs_"),
    ("hmac_secret_id", "hms_"),
    ("ssh_key_secret_id", "sks_"),
];

#[derive(Debug, Subcommand)]
pub(crate) enum AdminCommand {
    /// Inspect or change the separate Console operator endpoint.
    Config {
        #[command(subcommand)]
        command: AdminConfigCommand,
    },
    /// Verify and save an operator API key, not chat-client credentials.
    Login {
        #[arg(long)]
        key_stdin: bool,
    },
    /// Remove only the locally saved operator key; does not revoke it.
    Logout,
    /// Check current operator authorization without modifying server state.
    Status,
    /// Inspect and provision generic execution principals (not Console users).
    Principals {
        #[command(subcommand)]
        command: PrincipalCommand,
    },
    /// Manage reusable permission bundles, not Console administrator status.
    Roles {
        #[command(subcommand)]
        command: RoleCommand,
    },
    /// Manage direct secret grants; inherited grants may still authorize access.
    Grants {
        #[command(subcommand)]
        command: GrantCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum AdminConfigCommand {
    Show,
    Set {
        #[arg(value_parser = ["endpoint"])]
        key: String,
        value: String,
    },
    Unset {
        #[arg(value_parser = ["endpoint"])]
        key: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum ResourceCommand {
    /// List all pages; --limit controls page size, not the total result count.
    List {
        #[command(flatten)]
        options: ListOptions,
    },
    /// Read an exact OID or foreign ID, never a fuzzy display name.
    Get { reference: String },
    /// Create once. Server principal defaults still apply; not least privilege.
    Create {
        #[arg(long)]
        file: String,
    },
    /// Update an existing record only; supplied labels replace existing labels.
    Update {
        reference: String,
        #[arg(long)]
        file: String,
    },
    /// Server-side upsert by foreign ID; omitted permissions are not revoked.
    Upsert {
        foreign_id: String,
        #[arg(long)]
        file: String,
    },
}

#[derive(Debug, Args)]
pub(crate) struct ListOptions {
    #[arg(long = "label", value_parser = label)]
    labels: Vec<(String, String)>,
    #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u16).range(1..=200))]
    limit: u16,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PrincipalCommand {
    #[command(flatten)]
    Resource(ResourceCommand),
    /// Inspect server-redacted proxy config, not a connectivity/permission test.
    EffectiveConfig { reference: String },
    Roles {
        #[command(subcommand)]
        command: AssignmentCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum RoleCommand {
    #[command(flatten)]
    Resource(ResourceCommand),
    /// Delete the role, all its grants and all assignments; keep principals/secrets.
    Delete {
        reference: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum AssignmentCommand {
    List {
        principal: String,
    },
    Add {
        principal: String,
        role: String,
    },
    /// Remove only the assignment, not the role or its grants.
    Remove {
        principal: String,
        role: String,
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum GrantCommand {
    /// List direct grants to exactly one principal or role.
    List {
        #[arg(long, required_unless_present = "role", conflicts_with = "role")]
        principal: Option<String>,
        #[arg(long)]
        role: Option<String>,
        #[arg(long, default_value_t = 200, value_parser = clap::value_parser!(u16).range(1..=200))]
        limit: u16,
    },
    Get {
        oid: String,
    },
    Create {
        #[arg(long)]
        file: String,
    },
    /// Remove one grant; other direct or inherited grants can still authorize access.
    Delete {
        oid: String,
        #[arg(long)]
        yes: bool,
    },
}

fn label(value: &str) -> std::result::Result<(String, String), String> {
    let (key, value) = value
        .split_once('=')
        .filter(|(key, _)| !key.is_empty())
        .ok_or_else(|| "label must be KEY=VALUE".to_owned())?;
    Ok((key.to_owned(), value.to_owned()))
}

fn reference(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 256
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-._~".contains(&c))
    {
        bail!("invalid reference: use an OID or URL-safe foreign ID, not a name/path")
    }
    Ok(())
}

fn oid(value: &str, prefix: &str) -> Result<()> {
    reference(value)?;
    if !value.starts_with(prefix) || value.len() == prefix.len() {
        bail!("expected a resource OID, not a display name or foreign ID")
    }
    Ok(())
}

fn prefix(resource: &str) -> &str {
    match resource {
        "principals" => "prn_",
        "roles" => "role_",
        _ => "grant_",
    }
}

fn admin_endpoint(config: &Value) -> Result<String> {
    endpoint(&setting(config, "admin-endpoint", "CENTAUR_ADMIN_URL")?)
}

fn forget_key(config: &mut Value) {
    for field in ["admin-api-key", "admin-credential-endpoint"] {
        config
            .as_object_mut()
            .expect("validated configuration")
            .remove(field);
    }
}

struct Console {
    http: reqwest::Client,
    base: String,
    authorization: HeaderValue,
}

impl Console {
    fn new(base: String, key: &str) -> Result<Self> {
        let key_bytes = key.as_bytes();
        if key_bytes.len() != 68
            || !key.starts_with("iak_")
            || !key_bytes[4..]
                .iter()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
        {
            bail!("operator key must have the iak_ prefix followed by 64 lowercase hex characters")
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| eyre!("invalid operator key"))?;
        authorization.set_sensitive(true);
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| eyre!("could not initialize Console HTTP client"))?;
        Ok(Self {
            http,
            base,
            authorization,
        })
    }

    fn configured(config: &Value) -> Result<Self> {
        let base = admin_endpoint(config)?;
        let key = match env::var("CENTAUR_ADMIN_API_KEY")
            .ok()
            .filter(|s| !s.is_empty())
        {
            Some(key) => key,
            None => {
                if config["admin-credential-endpoint"].as_str() != Some(&base) {
                    bail!("no saved operator key bound to this admin endpoint; run admin login")
                }
                setting(config, "admin-api-key", "CENTAUR_ADMIN_API_KEY")?
            }
        };
        Self::new(base, &key)
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value> {
        let mutation = method != Method::GET;
        let uncertain = || {
            eyre!(if mutation {
                "Console mutation outcome uncertain; not retried. Inspect the resource with get/list before resubmitting."
            } else {
                "Console request failed (transport error); no response body displayed"
            })
        };
        // Rails recognizes literal dots as format suffixes before decoding route captures.
        let path = path.replace('.', "%2E");
        let mut url = reqwest::Url::parse(&format!("{}/api/v1/{path}", self.base))
            .map_err(|_| eyre!("invalid Console request URL"))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let mut request = self
            .http
            .request(method, url)
            .header(AUTHORIZATION, self.authorization.clone());
        if let Some(body) = body {
            request = request.json(body);
        }
        let mut response = request.send().await.map_err(|_| uncertain())?;
        let status = response.status();
        if !status.is_success() {
            bail!(
                "Console returned HTTP {}; response body omitted. Check operator authorization, resource references and document fields.{}",
                status.as_u16(),
                if mutation {
                    " Mutation was not retried; inspect get/list before resubmitting."
                } else {
                    ""
                }
            )
        }
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(json!({"ok": true}));
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_BYTES as u64)
        {
            bail!(
                "Console response exceeds 1 MiB; no output emitted{}",
                if mutation {
                    "; mutation outcome uncertain, inspect get/list"
                } else {
                    ""
                }
            )
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| uncertain())? {
            if bytes.len() + chunk.len() > MAX_BYTES {
                bail!(
                    "Console response exceeds 1 MiB; no output emitted{}",
                    if mutation {
                        "; mutation outcome uncertain, inspect get/list"
                    } else {
                        ""
                    }
                )
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| uncertain())?;
        if !value["data"].is_object() && !value["data"].is_array() {
            return Err(uncertain());
        }
        Ok(value)
    }

    async fn authorize(&self) -> Result<()> {
        let response = self
            .request(
                Method::GET,
                "roles",
                &[("limit".into(), "1".into()), ("page".into(), "1".into())],
                None,
            )
            .await?;
        if !response["data"].is_array() {
            bail!("invalid Console operator authorization response; expected a roles listing")
        }
        Ok(())
    }

    async fn get(&self, path: &str) -> Result<Value> {
        let value = self.request(Method::GET, path, &[], None).await?;
        if !value["data"].is_object() {
            bail!("invalid Console resource response")
        }
        Ok(value)
    }

    async fn resolve(&self, resource: &str, value: &str) -> Result<String> {
        reference(value)?;
        if value.starts_with(prefix(resource)) {
            oid(value, prefix(resource))?;
            return Ok(value.to_owned());
        }
        let found = self.get(&format!("{resource}/lookup/{value}")).await?;
        let id = found["data"]["id"]
            .as_str()
            .ok_or_else(|| eyre!("Console response has no OID"))?;
        oid(id, prefix(resource))?;
        Ok(id.to_owned())
    }

    async fn list(
        &self,
        path: &str,
        mut query: Vec<(String, String)>,
        limit: u16,
        unpaginated: bool,
    ) -> Result<Value> {
        let mut items = Vec::new();
        let mut bytes = 0;
        query.push(("limit".into(), limit.to_string()));
        query.push(("page".into(), "1".into()));
        // ponytail: aggregate at most 8 MiB / 10,000 pages; add explicit streaming if larger inventories are needed.
        for page in 1_u64..=10_000 {
            query.last_mut().expect("page query").1 = page.to_string();
            let response = self.request(Method::GET, path, &query, None).await?;
            let data = response["data"]
                .as_array()
                .ok_or_else(|| eyre!("invalid Console list"))?;
            bytes += serde_json::to_vec(data)?.len();
            if bytes > 8 * MAX_BYTES {
                bail!("Console list exceeds 8 MiB; no partial output emitted")
            }
            items.extend(data.iter().cloned());
            if unpaginated && response.get("meta").is_none() {
                return Ok(json!({"items": items}));
            }
            let meta = &response["meta"];
            let pages = meta["total_pages"]
                .as_u64()
                .ok_or_else(|| eyre!("missing Console pagination metadata"))?;
            let size = meta["limit"]
                .as_u64()
                .filter(|n| (1..=200).contains(n))
                .ok_or_else(|| eyre!("invalid Console page size"))?;
            let total = meta["total"]
                .as_u64()
                .ok_or_else(|| eyre!("invalid Console total"))?;
            if meta["page"].as_u64() != Some(page)
                || pages != total.div_ceil(size)
                || data.len() as u64 > size
                || (page < pages && data.is_empty())
                || (pages > 0 && page > pages)
            {
                bail!("invalid Console pagination; no partial output emitted")
            }
            if page >= pages {
                return Ok(json!({"items": items}));
            }
        }
        bail!("Console list exceeds 10,000 pages; no partial output emitted")
    }
}

fn document(file: &str, resource: &str, mode: &str, foreign_id: Option<&str>) -> Result<Value> {
    let mut bytes = Vec::new();
    let input: Box<dyn Read> = if file == "-" {
        Box::new(io::stdin())
    } else {
        Box::new(File::open(file).map_err(|_| eyre!("could not open JSON document"))?)
    };
    input
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| eyre!("could not read JSON document"))?;
    if bytes.len() > MAX_BYTES {
        bail!("JSON document exceeds 1 MiB")
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| eyre!("invalid JSON document"))?;
    let root = value
        .as_object()
        .ok_or_else(|| eyre!("expected a JSON object with only a data object"))?;
    let data = value["data"]
        .as_object()
        .filter(|_| root.len() == 1)
        .ok_or_else(|| eyre!("expected a JSON object with only a data object"))?;
    for (key, val) in data {
        let valid = if resource == "grants" {
            GRANTEES
                .iter()
                .chain(SECRETS)
                .any(|(name, p)| name == key && val.as_str().is_some_and(|v| oid(v, p).is_ok()))
        } else {
            match key.as_str() {
                "foreign_id" => {
                    mode != "update"
                        && val.as_str().is_some_and(|v| {
                            reference(v).is_ok()
                                && !v.starts_with(prefix(resource))
                                && foreign_id.is_none_or(|id| id == v)
                        })
                }
                "name" => val.is_null() || val.is_string(),
                "labels" => val.is_object(),
                "sandbox_repo_cache" if resource == "principals" => val
                    .as_str()
                    .is_some_and(|v| matches!(v, "none" | "public" | "all")),
                "sandbox_observability_enabled"
                | "sandbox_sessions_read_enabled"
                | "sandbox_workflows_read_enabled"
                | "sandbox_workflows_write_enabled"
                    if resource == "principals" =>
                {
                    val.is_boolean()
                }
                _ => false,
            }
        };
        if !valid {
            bail!("unsupported or invalid document field; see admin documentation (values omitted)")
        }
    }
    if data.is_empty() {
        bail!("data must contain at least one supported field")
    }
    if resource == "grants" {
        for keys in [GRANTEES, SECRETS] {
            if keys
                .iter()
                .filter(|(key, _)| data.contains_key(*key))
                .count()
                != 1
            {
                bail!("grant document requires exactly one grantee and exactly one secret OID")
            }
        }
    }
    Ok(value)
}

fn confirmed(yes: bool) -> Result<()> {
    if !yes {
        bail!("no mutation sent: review the documented removal/revocation effects, then pass --yes")
    }
    Ok(())
}

async fn resource_command(
    client: &Console,
    resource: &str,
    command: &ResourceCommand,
) -> Result<Value> {
    match command {
        ResourceCommand::List { options } => {
            client
                .list(
                    resource,
                    options
                        .labels
                        .iter()
                        .map(|(k, v)| (format!("labels[{k}]"), v.clone()))
                        .collect(),
                    options.limit,
                    false,
                )
                .await
        }
        ResourceCommand::Get { reference: value } => {
            reference(value)?;
            let path = if value.starts_with(prefix(resource)) {
                value.to_owned()
            } else {
                format!("lookup/{value}")
            };
            client.get(&format!("{resource}/{path}")).await
        }
        ResourceCommand::Create { file } => {
            let body = document(file, resource, "create", None)?;
            client
                .request(Method::POST, resource, &[], Some(&body))
                .await
        }
        ResourceCommand::Update { reference, file } => {
            let body = document(file, resource, "update", None)?;
            let id = client.resolve(resource, reference).await?;
            client
                .request(Method::PATCH, &format!("{resource}/{id}"), &[], Some(&body))
                .await
        }
        ResourceCommand::Upsert { foreign_id, file } => {
            reference(foreign_id)?;
            if foreign_id.starts_with(prefix(resource)) {
                bail!("upsert requires a foreign ID, not an OID")
            }
            let body = document(file, resource, "upsert", Some(foreign_id))?;
            client
                .request(
                    Method::PUT,
                    &format!("{resource}/{foreign_id}"),
                    &[],
                    Some(&body),
                )
                .await
        }
    }
}

pub(crate) async fn run(command: &AdminCommand, json_output: bool) -> Result<()> {
    let mut config = load_config()?;
    let value = match command {
        AdminCommand::Config { command } => {
            match command {
                AdminConfigCommand::Show => {
                    let configured = env::var("CENTAUR_ADMIN_URL")
                        .ok()
                        .filter(|s| !s.is_empty())
                        .or_else(|| config["admin-endpoint"].as_str().map(str::to_owned));
                    let url = configured.as_deref().map(endpoint).transpose()?;
                    return output(
                        json!({"data": {"endpoint": url, "source": if env::var("CENTAUR_ADMIN_URL").is_ok_and(|s| !s.is_empty()) {"CENTAUR_ADMIN_URL"} else {"config"},
                        "saved_key_present": config["admin-api-key"].is_string(),
                        "environment_key_present": env::var("CENTAUR_ADMIN_API_KEY").is_ok_and(|s| !s.is_empty())}}),
                        json_output,
                    );
                }
                AdminConfigCommand::Set { value, .. } => {
                    let url = endpoint(value)?;
                    if config["admin-endpoint"].as_str() != Some(&url) {
                        forget_key(&mut config);
                    }
                    config["admin-endpoint"] = json!(url);
                }
                AdminConfigCommand::Unset { .. } => {
                    forget_key(&mut config);
                    config
                        .as_object_mut()
                        .expect("validated configuration")
                        .remove("admin-endpoint");
                }
            }
            save_config(&config)?;
            json!({"ok": true, "message": "Admin endpoint saved/removed; environment overrides remain active."})
        }
        AdminCommand::Logout => {
            forget_key(&mut config);
            save_config(&config)?;
            json!({"ok": true, "message": "Saved operator key removed locally; environment keys remain active. No server key was revoked."})
        }
        AdminCommand::Login { key_stdin } => {
            let base = admin_endpoint(&config)?;
            let key = if *key_stdin {
                read_secret(true, "Centaur operator API key", "--key-stdin")?
            } else if let Ok(key) = env::var("CENTAUR_ADMIN_API_KEY") {
                key
            } else {
                read_secret(false, "Centaur operator API key", "--key-stdin")?
            };
            let client = Console::new(base.clone(), &key)?;
            client.authorize().await?;
            config["admin-api-key"] = json!(key);
            config["admin-credential-endpoint"] = json!(base);
            save_config(&config)?;
            json!({"ok": true, "message": "Operator authorization verified; key saved (mode 0600), not a session-readiness check."})
        }
        _ => {
            let client = Console::configured(&config)?;
            match command {
                AdminCommand::Status => {
                    client.authorize().await?;
                    json!({"data": {"authorized": true, "endpoint": client.base, "auth_mode": "operator_api_key"}})
                }
                AdminCommand::Principals {
                    command: PrincipalCommand::Resource(command),
                } => resource_command(&client, "principals", command).await?,
                AdminCommand::Roles {
                    command: RoleCommand::Resource(command),
                } => resource_command(&client, "roles", command).await?,
                AdminCommand::Principals {
                    command: PrincipalCommand::EffectiveConfig { reference: value },
                } => {
                    reference(value)?;
                    let path = if value.starts_with("prn_") {
                        value.to_owned()
                    } else {
                        format!("lookup/{value}")
                    };
                    client
                        .get(&format!("principals/{path}/effective_config"))
                        .await?
                }
                AdminCommand::Principals {
                    command: PrincipalCommand::Roles { command },
                } => {
                    let (principal, role) = match command {
                        AssignmentCommand::List { principal } => (principal, None),
                        AssignmentCommand::Add { principal, role } => (principal, Some(role)),
                        AssignmentCommand::Remove {
                            principal,
                            role,
                            yes,
                        } => {
                            confirmed(*yes)?;
                            (principal, Some(role))
                        }
                    };
                    reference(principal)?;
                    if let Some(role) = role {
                        reference(role)?;
                    }
                    let id = client.resolve("principals", principal).await?;
                    let path = format!("principals/{id}/roles");
                    match command {
                        AssignmentCommand::List { .. } => {
                            client.list(&path, vec![], 200, true).await?
                        }
                        AssignmentCommand::Add { role, .. } => {
                            let role = client.resolve("roles", role).await?;
                            client
                                .request(
                                    Method::POST,
                                    &path,
                                    &[],
                                    Some(&json!({"data": {"role_id": role}})),
                                )
                                .await?
                        }
                        AssignmentCommand::Remove { role, .. } => {
                            let role = client.resolve("roles", role).await?;
                            client
                                .request(Method::DELETE, &format!("{path}/{role}"), &[], None)
                                .await?
                        }
                    }
                }
                AdminCommand::Roles {
                    command: RoleCommand::Delete { reference, yes },
                } => {
                    confirmed(*yes)?;
                    let id = client.resolve("roles", reference).await?;
                    client
                        .request(Method::DELETE, &format!("roles/{id}"), &[], None)
                        .await?
                }
                AdminCommand::Grants { command } => match command {
                    GrantCommand::List {
                        principal,
                        role,
                        limit,
                    } => {
                        let (resource, value) = match (principal, role) {
                            (Some(value), None) => ("principals", value),
                            (None, Some(value)) => ("roles", value),
                            _ => bail!("choose exactly one of --principal and --role"),
                        };
                        let id = client.resolve(resource, value).await?;
                        client
                            .list(&format!("{resource}/{id}/grants"), vec![], *limit, false)
                            .await?
                    }
                    GrantCommand::Get { oid: value } => {
                        oid(value, "grant_")?;
                        client.get(&format!("grants/{value}")).await?
                    }
                    GrantCommand::Create { file } => {
                        let body = document(file, "grants", "create", None)?;
                        client
                            .request(Method::POST, "grants", &[], Some(&body))
                            .await?
                    }
                    GrantCommand::Delete { oid: value, yes } => {
                        confirmed(*yes)?;
                        oid(value, "grant_")?;
                        client
                            .request(Method::DELETE, &format!("grants/{value}"), &[], None)
                            .await?
                    }
                },
                _ => unreachable!("local configuration commands handled above"),
            }
        }
    };
    output(value, json_output)
}

fn output(value: Value, json_output: bool) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string(&value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}
