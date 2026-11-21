use std::{
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tokio::sync::Mutex;

use centaur_session_core::{Session, ThreadKey};
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use reqwest::{
    Client as HttpClient, StatusCode,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use thiserror::Error;

use crate::types::{
    AppendMessagesRequest, AppendMessagesResponse, CreateSessionRequest, ExecuteSessionRequest,
    ExecuteSessionResponse,
};

/// Supplies renewable human tokens without exposing the CLI credential store to the API library.
pub trait AccessTokenSource: Send + Sync {
    fn token(
        &self,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, ClientError>> + Send + '_>>;
}

#[derive(Clone)]
pub struct CentaurClient {
    client: HttpClient,
    base_url: String,
    oauth: Option<Arc<OAuthCredentials>>,
    user_tokens: Option<Arc<dyn AccessTokenSource>>,
}

struct OAuthCredentials {
    client_id: String,
    secret: String,
    token: Mutex<Option<(String, Instant)>>,
}

impl CentaurClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_client(HttpClient::new(), base_url)
    }

    pub fn with_bearer_token(
        base_url: impl Into<String>,
        token: &str,
    ) -> Result<Self, ClientError> {
        let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| ClientError::InvalidBearerToken)?;
        value.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, value);
        let client = HttpClient::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self::with_client(client, base_url))
    }

    pub fn with_client(client: HttpClient, base_url: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            oauth: None,
            user_tokens: None,
        }
    }

    /// Secrets remain in client memory, never in the sandbox or Debug output.
    pub fn with_client_credentials(
        base_url: &str,
        client_id: String,
        secret: String,
    ) -> Result<Self, ClientError> {
        let client = HttpClient::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(45))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut result = Self::with_client(client, base_url);
        result.oauth = Some(Arc::new(OAuthCredentials {
            client_id,
            secret,
            token: Mutex::new(None),
        }));
        Ok(result)
    }

    pub fn with_user_tokens(
        resource: String,
        tokens: Arc<dyn AccessTokenSource>,
    ) -> Result<Self, ClientError> {
        let mut client = Self::with_client_credentials(&resource, String::new(), String::new())?;
        client.oauth = None;
        client.user_tokens = Some(tokens);
        Ok(client)
    }

    pub fn is_human(&self) -> bool {
        self.user_tokens.is_some()
    }

    fn api_url(&self, path: &str) -> String {
        if self.is_human() {
            format!(
                "{}/{}",
                self.base_url,
                path.strip_prefix("api/").unwrap_or(path)
            )
        } else {
            format!("{}/{}", self.base_url, path)
        }
    }

    pub async fn human_identity(&self) -> Result<Value, ClientError> {
        Ok(self
            .send(self.client.get(format!("{}/me", self.base_url)))
            .await?
            .json()
            .await?)
    }

    pub async fn human_action(
        &self,
        key: &ThreadKey,
        action: &str,
        body: Option<Value>,
    ) -> Result<Value, ClientError> {
        let path = match action {
            "interrupt" => format!("{}/interrupt", self.session_url(key)),
            "release" => format!("{}/sandbox", self.session_url(key)),
            "archive" => format!("{}/archive", self.session_url(key)),
            "delete" => self.session_url(key),
            "prune" => format!("{}/prune", self.session_url(key)),
            _ => return Err(ClientError::InvalidUserAction),
        };
        let request = if action == "delete" || action == "prune" {
            self.client.delete(path)
        } else {
            self.client.post(path)
        };
        let request = if let Some(body) = body {
            request.json(&body)
        } else {
            request
        };
        self.send(request).await?.json().await.map_err(Into::into)
    }

    pub async fn submit_human_turn(
        &self,
        key: &ThreadKey,
        text: &str,
        request_id: &str,
        duration: u64,
    ) -> Result<ExecuteSessionResponse, ClientError> {
        self.post_json(
            &format!("{}/turn", self.session_url(key)),
            &json!({"message":text,"request_id":request_id,"max_duration_ms":duration}),
        )
        .await
    }

    pub async fn authenticate(&self) -> Result<(), ClientError> {
        self.access_token().await.map(|_| ())
    }

    async fn access_token(&self) -> Result<Option<String>, ClientError> {
        if let Some(tokens) = &self.user_tokens {
            return tokens.token().await.map(Some);
        }
        let Some(auth) = &self.oauth else {
            return Ok(None);
        };
        let mut cached = auth.token.lock().await;
        if let Some((token, expires)) = cached.as_ref()
            && Instant::now() < *expires
        {
            return Ok(Some(token.clone()));
        }
        let response = self
            .client
            .post(format!("{}/oauth/token", self.base_url))
            .basic_auth(&auth.client_id, Some(&auth.secret))
            .form(&[("grant_type", "client_credentials")])
            .timeout(Duration::from_secs(30))
            .send()
            .await?;
        if !response.status().is_success() {
            // Never print an authentication response that could contain credentials.
            return Err(ClientError::Authentication(response.status()));
        }
        let data: Value = response.json().await?;
        let token = data["access_token"]
            .as_str()
            .ok_or(ClientError::InvalidTokenResponse)?
            .to_owned();
        let ttl = data["expires_in"]
            .as_u64()
            .unwrap_or(3600)
            .saturating_sub(30);
        *cached = Some((token.clone(), Instant::now() + Duration::from_secs(ttl)));
        Ok(Some(token))
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ClientError> {
        let retry = request.try_clone();
        let token = self.access_token().await?;
        let request = match token {
            Some(token) => request.bearer_auth(token),
            None => request,
        };
        let response = request.send().await?;
        if response.status() == StatusCode::UNAUTHORIZED
            && let (Some(auth), Some(request)) = (&self.oauth, retry)
        {
            *auth.token.lock().await = None;
            let token = self
                .access_token()
                .await?
                .ok_or(ClientError::InvalidTokenResponse)?;
            return ensure_response_success(request.bearer_auth(token).send().await?).await;
        }
        ensure_response_success(response).await
    }

    pub async fn session_state(&self, key: &ThreadKey) -> Result<Value, ClientError> {
        Ok(self
            .send(
                self.client
                    .get(format!("{}/state", self.session_url(key)))
                    .timeout(Duration::from_secs(30)),
            )
            .await?
            .json()
            .await?)
    }

    pub async fn list_sessions(&self, after: &str) -> Result<Value, ClientError> {
        self.list_sessions_with_archived(after, false).await
    }

    pub async fn list_sessions_with_archived(
        &self,
        after: &str,
        include_archived: bool,
    ) -> Result<Value, ClientError> {
        if self.is_human() {
            return Ok(self
                .send(self.client.get(format!(
                    "{}?after_thread_key={}&include_archived={}",
                    self.base_url,
                    urlencoding::encode(after),
                    include_archived
                )))
                .await?
                .json()
                .await?);
        }
        self.list_sessions_filtered(after, include_archived, false)
            .await
    }
    pub async fn list_human_prunable_sessions(&self, after: &str) -> Result<Value, ClientError> {
        Ok(self
            .send(self.client.get(format!(
                "{}?after_thread_key={after}&prunable_only=true",
                self.base_url
            )))
            .await?
            .json()
            .await?)
    }
    pub async fn interrupt_human_session(&self, key: &ThreadKey) -> Result<Value, ClientError> {
        let state = self.session_state(key).await?;
        if let Some(id) = state["execution"]["execution_id"].as_str() {
            self.human_action(key, "interrupt", Some(json!({"execution_id":id})))
                .await
        } else {
            Ok(json!({"interrupted":false,"execution_id":null}))
        }
    }
    pub async fn release_human_sandbox(&self, key: &ThreadKey) -> Result<Value, ClientError> {
        self.human_action(key, "release", None).await
    }
    pub async fn archive_human_session(
        &self,
        key: &ThreadKey,
        archived: bool,
    ) -> Result<Value, ClientError> {
        self.human_action(key, "archive", Some(json!({"archived":archived})))
            .await
    }
    pub async fn delete_human_session(&self, key: &ThreadKey) -> Result<Value, ClientError> {
        self.human_action(key, "delete", None).await
    }
    pub async fn prune_human_session(&self, key: &ThreadKey) -> Result<Value, ClientError> {
        self.human_action(key, "prune", None).await
    }

    pub async fn list_prunable_sessions(&self, after: &str) -> Result<Value, ClientError> {
        self.list_sessions_filtered(after, true, true).await
    }

    async fn list_sessions_filtered(
        &self,
        after: &str,
        include_archived: bool,
        prunable_only: bool,
    ) -> Result<Value, ClientError> {
        Ok(self
            .send(
                self.client
                    .get(format!(
                        "{}?after_thread_key={}&include_archived={}&prunable_only={}",
                        self.api_url("api/sessions"),
                        urlencoding::encode(after),
                        include_archived,
                        prunable_only
                    ))
                    .timeout(Duration::from_secs(30)),
            )
            .await?
            .json()
            .await?)
    }

    pub async fn interrupt_session(&self, key: &ThreadKey) -> Result<Value, ClientError> {
        if self.is_human() {
            let state = self.session_state(key).await?;
            if let Some(id) = state["execution"]["execution_id"].as_str() {
                return self
                    .post_json(
                        &format!("{}/interrupt", self.session_url(key)),
                        &json!({"execution_id":id}),
                    )
                    .await;
            }
            return Ok(json!({"interrupted":false,"execution_id":null}));
        }
        self.post_json(
            &format!("{}/interrupt", self.session_url(key)),
            &json!({"reason":"Interrupted from terminal"}),
        )
        .await
    }

    pub async fn list_mcp_servers(&self) -> Result<Value, ClientError> {
        Ok(self
            .send(
                self.client
                    .get(self.api_url("api/mcp/servers"))
                    .timeout(Duration::from_secs(30)),
            )
            .await?
            .json()
            .await?)
    }

    pub async fn connect_mcp_integration(
        &self,
        server: &centaur_session_core::mcp::McpServer,
    ) -> Result<Value, ClientError> {
        self.post_json(&self.api_url("api/mcp/servers"), server)
            .await
    }

    pub async fn login_mcp_server(&self, name: &str) -> Result<Value, ClientError> {
        self.post_json(
            &format!(
                "{}/{}/login",
                self.api_url("api/mcp/servers"),
                urlencoding::encode(name)
            ),
            &json!({}),
        )
        .await
    }

    pub async fn archive_session(
        &self,
        key: &ThreadKey,
        archived: bool,
    ) -> Result<Value, ClientError> {
        self.post_json(
            &format!("{}/archive", self.session_url(key)),
            &json!({"archived": archived}),
        )
        .await
    }

    pub async fn delete_session(&self, key: &ThreadKey) -> Result<Value, ClientError> {
        Ok(self
            .send(
                self.client
                    .delete(self.session_url(key))
                    .timeout(Duration::from_secs(120)),
            )
            .await?
            .json()
            .await?)
    }

    pub async fn prune_session(&self, key: &ThreadKey) -> Result<Value, ClientError> {
        Ok(self
            .send(
                self.client
                    .delete(format!("{}/prune", self.session_url(key)))
                    .timeout(Duration::from_secs(120)),
            )
            .await?
            .json()
            .await?)
    }

    pub async fn release_sandbox(&self, key: &ThreadKey) -> Result<Value, ClientError> {
        Ok(self
            .send(
                self.client
                    .delete(format!("{}/sandbox", self.session_url(key)))
                    .timeout(Duration::from_secs(30)),
            )
            .await?
            .json()
            .await?)
    }

    pub async fn create_session(
        &self,
        thread_key: &ThreadKey,
        request: CreateSessionRequest,
    ) -> Result<Session, ClientError> {
        self.post_json(&self.session_url(thread_key), &request)
            .await
    }

    pub async fn append_messages(
        &self,
        thread_key: &ThreadKey,
        request: AppendMessagesRequest,
    ) -> Result<AppendMessagesResponse, ClientError> {
        self.post_json(
            &format!("{}/messages", self.session_url(thread_key)),
            &request,
        )
        .await
    }

    pub async fn execute_session(
        &self,
        thread_key: &ThreadKey,
        request: ExecuteSessionRequest,
    ) -> Result<ExecuteSessionResponse, ClientError> {
        self.post_json(
            &format!("{}/execute", self.session_url(thread_key)),
            &request,
        )
        .await
    }

    pub async fn stream_events(
        &self,
        thread_key: &ThreadKey,
        after_event_id: i64,
    ) -> Result<SseEventStream, ClientError> {
        let events_url = format!(
            "{}/events?after_event_id={after_event_id}",
            self.session_url(thread_key)
        );
        let response = self.send(self.client.get(&events_url)).await?;
        let stream = response
            .bytes_stream()
            .eventsource()
            .map(|event| event.map_err(|error| ClientError::EventStream(error.to_string())));
        Ok(Box::pin(stream))
    }

    /// Dropping this reader never interrupts the server-owned execution.
    pub fn reconnecting_events(&self, key: &ThreadKey, after: i64) -> SseEventStream {
        let state = (
            self.clone(),
            key.clone(),
            None::<SseEventStream>,
            after,
            1u64,
        );
        Box::pin(futures_util::stream::unfold(
            state,
            |(client, key, mut stream, mut cursor, mut delay)| async move {
                loop {
                    if stream.is_none() {
                        match client.stream_events(&key, cursor).await {
                            Ok(opened) => stream = Some(opened),
                            Err(error) if error.is_retryable() => {
                                tokio::time::sleep(Duration::from_secs(delay)).await;
                                delay = (delay * 2).min(30);
                                let notice = SseEvent {
                                    event: "session.client_reconnecting".to_owned(),
                                    data: "{}".to_owned(),
                                    ..Default::default()
                                };
                                return Some((Ok(notice), (client, key, None, cursor, delay)));
                            }
                            Err(error) => {
                                return Some((Err(error), (client, key, None, cursor, delay)));
                            }
                        }
                    }
                    let next = stream.as_mut()?.next().await;
                    match next {
                        Some(Ok(event)) if event.event != "session.stream_error" => {
                            if let Ok(id) = event.id.parse::<i64>() {
                                if id <= cursor {
                                    continue;
                                }
                                cursor = id;
                            }
                            delay = 1;
                            return Some((Ok(event), (client, key, stream, cursor, delay)));
                        }
                        _ => {
                            tokio::time::sleep(Duration::from_secs(delay)).await;
                            delay = (delay * 2).min(30);
                            let notice = SseEvent {
                                event: "session.client_reconnecting".to_owned(),
                                data: "{}".to_owned(),
                                ..Default::default()
                            };
                            return Some((Ok(notice), (client, key, None, cursor, delay)));
                        }
                    }
                }
            },
        ))
    }

    async fn post_json<T, R>(&self, url: &str, payload: &T) -> Result<R, ClientError>
    where
        T: serde::Serialize + ?Sized,
        R: serde::de::DeserializeOwned,
    {
        let response = self
            .send(
                self.client
                    .post(url)
                    .json(payload)
                    .timeout(Duration::from_secs(30)),
            )
            .await?;
        Ok(response.json().await?)
    }

    fn session_url(&self, thread_key: &ThreadKey) -> String {
        format!(
            "{}/{}",
            self.api_url("api/session"),
            urlencoding::encode(thread_key.as_str())
        )
    }
}

impl ClientError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http(error) => {
                error.is_connect() || error.is_timeout() || error.is_body() || error.is_request()
            }
            Self::Api { status, .. } | Self::Authentication(status) => {
                status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS
            }
            Self::EventStream(_) => true,
            _ => false,
        }
    }
}

pub type SseEventStream = Pin<Box<dyn Stream<Item = Result<SseEvent, ClientError>> + Send>>;
pub type SseEvent = eventsource_stream::Event;

async fn ensure_response_success(
    response: reqwest::Response,
) -> Result<reqwest::Response, ClientError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await?;
    Err(ClientError::Api { status, body })
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("HTTP {status}: {body}")]
    Api { status: StatusCode, body: String },
    #[error("event stream parse failed: {0}")]
    EventStream(String),
    #[error("bearer token cannot be represented as an HTTP header")]
    InvalidBearerToken,
    #[error("unsupported human CLI action")]
    InvalidUserAction,
    #[error("Centaur authentication failed (HTTP {0}); check API client configuration")]
    Authentication(StatusCode),
    #[error(
        "Browser credential refresh failed; run centaur auth status or centaur login (no credential fallback)"
    )]
    UserAuthentication,
    #[error("Centaur returned an invalid token response")]
    InvalidTokenResponse,
}
