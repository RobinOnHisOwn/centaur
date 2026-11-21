//! Native browser authorization and one refresh-family owner across processes.
use crate::{Args, terminal};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use eyre::{Result, bail, eyre};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) fn selected(config: &Value, args: &Args) -> bool {
    args.auth_mode
        .as_deref()
        .map_or(config["browser"].is_object(), |m| m == "user")
}
pub(crate) fn check_environment(args: &Args) -> Result<()> {
    if !args.api_token.is_empty()
        || args.api_url.is_some()
        || [
            "CENTAUR_CLIENT_SECRET",
            "CENTAUR_CLIENT_ID",
            "CENTAUR_ADMIN_API_KEY",
            "CENTAUR_ADMIN_URL",
        ]
        .iter()
        .any(|k| std::env::var_os(k).is_some())
    {
        bail!(
            "browser identity conflicts with machine/operator overrides; unset them or select --auth-mode machine/operator"
        )
    }
    Ok(())
}
fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()?)
}
async fn response_json(mut response: reqwest::Response) -> Result<Value> {
    if !response.status().is_success() {
        bail!(
            "authentication request denied (HTTP {}); run centaur login if access expired",
            response.status().as_u16()
        );
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > 65536 {
            bail!("authentication response exceeds limit")
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| eyre!("invalid authentication response"))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| eyre!("incomplete authentication response"))
}
fn validate_private(meta: &fs::Metadata) -> Result<()> {
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o077 != 0
    {
        bail!("credentials require an owned regular mode-0600 file")
    }
    Ok(())
}
pub(crate) fn lock() -> Result<File> {
    let path = terminal::config_path()?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    if !parent.exists() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let meta = fs::symlink_metadata(parent)?;
    if !meta.is_dir()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.permissions().mode() & 0o022 != 0
    {
        bail!("credential directory must be owned and not group/world writable")
    }
    if let Ok(meta) = fs::symlink_metadata(&path)
        && meta.file_type().is_symlink()
    {
        bail!("credential path cannot be a symlink")
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path.with_extension("lock"))?;
    validate_private(&lock.metadata()?)?;
    let deadline = Instant::now() + Duration::from_secs(35);
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25))
            }
            Err(_) => bail!("credential store is busy; retry after the other command finishes"),
        }
    }
}
fn browser(config: &Value) -> Result<&Value> {
    let v = &config["browser"];
    if !v.is_object() {
        bail!("run centaur login first")
    }
    Ok(v)
}
fn validate_saved(v: &Value) -> Result<()> {
    let server = terminal::endpoint(text(v, "server")?)?;
    if text(v, "issuer")? != format!("{server}/cli") {
        bail!("saved issuer mismatch; run centaur login")
    }
    for (field, suffix) in [
        ("token_endpoint", "/cli/oauth/token"),
        ("revocation_endpoint", "/cli/oauth/revoke"),
    ] {
        if text(v, field)? != format!("{server}{suffix}") {
            bail!("saved authentication destination mismatch")
        }
    }
    terminal::endpoint(text(v, "session_resource")?)?;
    if text(v, "console_resource")? != format!("{server}/api/cli/v1") {
        bail!("saved Console resource mismatch; run centaur login")
    }
    Ok(())
}
async fn refresh(resource: &str) -> Result<(String, u64)> {
    let _lock = tokio::task::spawn_blocking(lock).await??;
    let mut config = terminal::load_config()?;
    let v = browser(&config)?;
    validate_saved(v)?;
    if ![text(v, "session_resource")?, text(v, "console_resource")?].contains(&resource) {
        bail!("authentication resource mismatch")
    }
    let data = response_json(
        http()?
            .post(text(v, "token_endpoint")?)
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", "centaur-cli"),
                ("refresh_token", text(v, "refresh_token")?),
                ("resource", resource),
            ])
            .send()
            .await?,
    )
    .await?;
    let token = text(&data, "access_token")?.to_owned();
    config["browser"]["refresh_token"] = json!(text(&data, "refresh_token")?);
    terminal::save_config(&config)?;
    Ok((
        token,
        data["expires_in"]
            .as_u64()
            .filter(|v| *v > 30 && *v <= 300)
            .ok_or_else(|| eyre!("invalid access-token lifetime"))?,
    ))
}
pub(crate) struct Tokens {
    resource: String,
    cache: tokio::sync::Mutex<Option<(String, Instant)>>,
}
impl Tokens {
    pub(crate) fn new(resource: String) -> Arc<Self> {
        Arc::new(Self {
            resource,
            cache: tokio::sync::Mutex::new(None),
        })
    }
    pub(crate) async fn token(&self) -> Result<String> {
        let mut cache = self.cache.lock().await;
        if let Some((token, deadline)) = cache.as_ref()
            && Instant::now() < *deadline
        {
            return Ok(token.clone());
        }
        let (token, ttl) = refresh(&self.resource).await?;
        *cache = Some((
            token.clone(),
            Instant::now() + Duration::from_secs(ttl - 30),
        ));
        Ok(token)
    }
}
impl centaur_api_server::client::AccessTokenSource for Tokens {
    fn token(
        &self,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<String, centaur_api_server::client::ClientError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async {
            self.token()
                .await
                .map_err(|_| centaur_api_server::client::ClientError::UserAuthentication)
        })
    }
}

pub(crate) async fn login(server: Option<&str>, no_open: bool, args: &Args) -> Result<()> {
    check_environment(args)?;
    let old = terminal::load_config()?;
    let server = terminal::endpoint(
        server
            .or_else(|| old["browser"]["server"].as_str())
            .ok_or_else(|| eyre!("supply --server <Console HTTPS URL>"))?,
    )?;
    let metadata = response_json(
        http()?
            .get(format!(
                "{server}/.well-known/oauth-authorization-server/cli"
            ))
            .send()
            .await?,
    )
    .await?;
    if text(&metadata, "issuer")? != format!("{server}/cli")
        || text(&metadata, "client_id")? != "centaur-cli"
    {
        bail!("unsupported or mismatched CLI issuer")
    }
    for (field, suffix) in [
        ("authorization_endpoint", "/cli/oauth/authorize"),
        ("token_endpoint", "/cli/oauth/token"),
        ("revocation_endpoint", "/cli/oauth/revoke"),
    ] {
        if text(&metadata, field)? != format!("{server}{suffix}") {
            bail!("untrusted OAuth destination")
        }
    }
    let session_resource = terminal::endpoint(text(&metadata, "session_resource")?)?;
    if !session_resource.ends_with("/api/cli/v1")
        || text(&metadata, "console_resource")? != format!("{server}/api/cli/v1")
    {
        bail!("unsupported CLI resource")
    }
    let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(_) => tokio::net::TcpListener::bind("[::1]:0").await?,
    };
    let redirect = format!("http://{}/callback", listener.local_addr()?);
    let verifier = format!(
        "{}{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let state = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = reqwest::Url::parse(text(&metadata, "authorization_endpoint")?)?;
    url.query_pairs_mut().extend_pairs([
        ("client_id", "centaur-cli"),
        ("response_type", "code"),
        ("redirect_uri", &redirect),
        ("state", &state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
    ]);
    println!("{url}");
    std::io::stdout().flush()?;
    if !no_open {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        if std::process::Command::new(opener)
            .arg(url.as_str())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_err()
        {
            eprintln!("Open the URL above in a browser on this machine.");
        }
    }
    let code = tokio::select! {result=tokio::time::timeout(Duration::from_secs(300),callback(&listener,&state,text(&metadata,"issuer")?))=>result.map_err(|_|eyre!("browser login timed out"))??,_=tokio::signal::ctrl_c()=>bail!("browser login cancelled")};
    let data = response_json(
        http()?
            .post(text(&metadata, "token_endpoint")?)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", "centaur-cli"),
                ("code", &code),
                ("code_verifier", &verifier),
                ("redirect_uri", &redirect),
                ("resource", text(&metadata, "session_resource")?),
            ])
            .send()
            .await?,
    )
    .await?;
    let info = response_json(
        http()?
            .get(format!("{}/me", text(&metadata, "session_resource")?))
            .bearer_auth(text(&data, "access_token")?)
            .send()
            .await?,
    )
    .await?;
    if info["protocol_version"] != 1
        || text(&info, "thread_key_prefix")? != format!("personal:{}:", text(&info, "user")?)
    {
        bail!("server does not support the expected CLI user identity contract")
    }
    let _lock = tokio::task::spawn_blocking(lock).await??;
    let mut config = terminal::load_config()?;
    config["browser"] = json!({"server":server,"issuer":metadata["issuer"],"token_endpoint":metadata["token_endpoint"],"revocation_endpoint":metadata["revocation_endpoint"],"session_resource":metadata["session_resource"],"console_resource":metadata["console_resource"],"refresh_token":text(&data,"refresh_token")?,"user":text(&info,"user")?,"prefix":text(&info,"thread_key_prefix")?});
    terminal::save_config(&config)?;
    println!(
        "Signed in as {}. Administration uses this login when consented and permitted.",
        text(&info, "user")?
    );
    Ok(())
}
async fn callback(listener: &tokio::net::TcpListener, state: &str, issuer: &str) -> Result<String> {
    loop {
        let (mut socket, _) = listener.accept().await?;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 1024];
        let read = tokio::time::timeout(Duration::from_secs(3), async {
            while bytes.len() < 8192 {
                let n = socket.read(&mut chunk).await?;
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..n]);
                if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            Ok::<_, std::io::Error>(())
        })
        .await;
        if !matches!(read, Ok(Ok(()))) || bytes.len() > 8192 || !bytes.ends_with(b"\r\n\r\n") {
            continue;
        }
        let request = String::from_utf8_lossy(&bytes);
        let mut first = request
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace();
        let method = first.next();
        let path = first.next().unwrap_or_default();
        let url = reqwest::Url::parse(&format!("http://127.0.0.1{path}"));
        let accepted = url
            .ok()
            .filter(|u| method == Some("GET") && u.path() == "/callback")
            .and_then(|u| {
                let mut params = std::collections::HashMap::new();
                for (k, v) in u.query_pairs() {
                    if params.insert(k.into_owned(), v.into_owned()).is_some() {
                        return None;
                    }
                }
                (params.get("state").map(String::as_str) == Some(state)
                    && params.get("iss").map(String::as_str) == Some(issuer))
                .then_some(params)
            });
        let Some(params) = accepted else {
            let _ = socket
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        };
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 33\r\nConnection: close\r\nCache-Control: no-store\r\n\r\nReturn to your Centaur terminal.\n").await?;
        if params.contains_key("error") {
            bail!("browser authorization was denied")
        }
        let code = params
            .get("code")
            .filter(|s| !s.is_empty() && s.len() < 2048)
            .ok_or_else(|| eyre!("invalid authorization callback"))?;
        return Ok(code.to_owned());
    }
}
pub(crate) async fn client(
    config: &Value,
    args: &Args,
) -> Result<(centaur_api_server::client::CentaurClient, String)> {
    check_environment(args)?;
    let v = browser(config)?;
    validate_saved(v)?;
    let resource = text(v, "session_resource")?.to_owned();
    let c = centaur_api_server::client::CentaurClient::with_user_tokens(
        resource,
        Tokens::new(text(v, "session_resource")?.to_owned()),
    )?;
    Ok((c, text(v, "prefix")?.to_owned()))
}
pub(crate) fn console_resource(config: &Value, args: &Args) -> Result<String> {
    check_environment(args)?;
    let v = browser(config)?;
    validate_saved(v)?;
    Ok(text(v, "console_resource")?.to_owned())
}
pub(crate) async fn console(config: &Value, args: &Args) -> Result<(String, String)> {
    let resource = console_resource(config, args)?;
    let token = Tokens::new(resource.clone()).token().await?;
    Ok((resource, token))
}
pub(crate) async fn logout(args: &Args) -> Result<()> {
    check_environment(args)?;
    let _lock = tokio::task::spawn_blocking(lock).await??;
    let mut config = terminal::load_config()?;
    let v = browser(&config)?;
    validate_saved(v)?;
    let result = http()?
        .post(text(v, "revocation_endpoint")?)
        .form(&[
            ("client_id", "centaur-cli"),
            ("token", text(v, "refresh_token")?),
        ])
        .send()
        .await;
    config
        .as_object_mut()
        .ok_or_else(|| eyre!("invalid configuration"))?
        .remove("browser");
    terminal::save_config(&config)?;
    if !result.is_ok_and(|r| r.status().is_success()) {
        bail!(
            "local browser credential removed; server revocation is UNCONFIRMED. Sign in again and revoke the lost connection."
        )
    }
    println!(
        "CLI connection revoked and local browser credential removed. Accepted work and other devices are unchanged."
    );
    Ok(())
}

pub(crate) async fn prepare(config: &Value, args: &Args) -> Result<()> {
    let (resource, token) = console(config, args).await?;
    response_json(
        http()?
            .post(format!("{resource}/me/principal"))
            .bearer_auth(token)
            .send()
            .await?,
    )
    .await?;
    Ok(())
}
pub(crate) async fn auth(
    command: &terminal::AuthCommand,
    config: &Value,
    args: &Args,
) -> Result<()> {
    if !selected(config, args) {
        bail!("auth manages browser credentials; use config show for machine configuration")
    }
    match command {
        terminal::AuthCommand::Status { json: compact } => {
            let (c, _) = client(config, args).await?;
            let identity = c.human_identity().await?;
            let (resource, token) = console(config, args).await?;
            let permissions = response_json(
                http()?
                    .get(format!("{resource}/me"))
                    .bearer_auth(token)
                    .send()
                    .await?,
            )
            .await?;
            let value =
                json!({"auth_mode":"browser","identity":identity,"permissions":permissions});
            println!(
                "{}",
                if *compact {
                    value.to_string()
                } else {
                    serde_json::to_string_pretty(&value)?
                }
            );
        }
        terminal::AuthCommand::Connections { command } => {
            let (resource, token) = console(config, args).await?;
            match command {
                terminal::ConnectionCommand::List => println!(
                    "{}",
                    response_json(
                        http()?
                            .get(format!("{resource}/me/connections"))
                            .bearer_auth(token)
                            .send()
                            .await?
                    )
                    .await?
                ),
                terminal::ConnectionCommand::Revoke { id } => {
                    let id = uuid::Uuid::parse_str(id)
                        .map_err(|_| eyre!("expected a connection UUID"))?;
                    let response = http()?
                        .delete(format!("{resource}/me/connections/{id}"))
                        .bearer_auth(token)
                        .send()
                        .await?;
                    if !response.status().is_success() {
                        bail!("connection revocation was denied")
                    }
                    println!("Connection revoked. Accepted work is unchanged.");
                }
            }
        }
    }
    Ok(())
}
