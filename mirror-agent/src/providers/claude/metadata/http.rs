//! Fixed-origin, immediate-dispatch metadata GETs. No redirects, proxies,
//! application retries, environment credentials or production endpoint override.
use super::{compatibility, temporary, ClaudeError};
use crate::providers::claude::{
    profile, ClaudeAccessLease, ClaudeErrorKind, ClaudeOperation, ClaudeProviderCode,
};
use async_trait::async_trait;
use reqwest::{Client, Url};
use serde_json::Value;
use std::time::{Duration, SystemTime};

pub(super) const MAX_BYTES: usize = 2 * 1024 * 1024;
#[derive(Clone, Copy)]
pub(super) enum Query<'a> {
    Models(Option<&'a str>),
    Context(&'a str),
    Usage,
}
impl Query<'_> {
    pub fn operation(self) -> ClaudeOperation {
        match self {
            Self::Usage => ClaudeOperation::Usage,
            _ => ClaudeOperation::Models,
        }
    }
}
#[async_trait]
pub(super) trait Transport: Send + Sync {
    async fn get(
        &self,
        lease: ClaudeAccessLease,
        query: Query<'_>,
        remaining: &mut usize,
    ) -> Result<Value, ClaudeError>;
}
pub(super) struct FixedTransport {
    client: Client,
    #[cfg(test)]
    origin: Option<String>,
}
fn builder() -> reqwest::ClientBuilder {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(10))
}
impl FixedTransport {
    pub fn new() -> Result<Self, ClaudeError> {
        Ok(Self {
            client: builder().build().map_err(|_| temporary())?,
            #[cfg(test)]
            origin: None,
        })
    }
    #[cfg(test)]
    pub fn loopback(origin: String) -> Self {
        assert!(origin.starts_with("http://127.0.0.1:"));
        Self {
            client: builder().https_only(false).build().unwrap(),
            origin: Some(origin),
        }
    }
    fn url(&self, query: Query<'_>) -> Url {
        let origin = profile::API_ORIGIN;
        #[cfg(test)]
        let origin = self.origin.as_deref().unwrap_or(origin);
        let mut url = Url::parse(origin).expect("reviewed origin");
        match query {
            Query::Usage => url.set_path("/api/oauth/usage"),
            Query::Models(cursor) => {
                url.set_path("/v1/models");
                url.query_pairs_mut().append_pair("limit", "100");
                if let Some(cursor) = cursor {
                    url.query_pairs_mut().append_pair("after_id", cursor);
                }
            }
            Query::Context(model) => {
                url.set_path("/v1/models");
                url.path_segments_mut().expect("fixed origin").push(model);
            }
        }
        url
    }
}
fn retry_at(value: Option<&reqwest::header::HeaderValue>) -> Option<SystemTime> {
    let value = value?.to_str().ok()?;
    if value.len() > 128 {
        return None;
    }
    let time = if let Ok(seconds) = value.parse::<u64>() {
        SystemTime::now().checked_add(Duration::from_secs(seconds))?
    } else {
        // HTTP-date is parsed without adopting arbitrary provider text in output.
        httpdate::parse_http_date(value).ok()?
    };
    // SystemTime can represent dates beyond protobuf/RFC3339's UTC year9999.
    // Invalid enormous hints use the normal bounded failure cooldown, never a
    // serialization error or an immediate retry. Keep the Messages boundary.
    let seconds = time.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    (seconds <= 253_402_300_799).then_some(time)
}

fn status_error(status: u16, query: Query<'_>, retry: Option<SystemTime>) -> ClaudeError {
    let mut error = match status {
        401 => ClaudeError::new(ClaudeErrorKind::LoginExpired),
        403 => ClaudeError::new(ClaudeErrorKind::AccessDenied),
        404 if matches!(query, Query::Context(_)) => {
            ClaudeError::new(ClaudeErrorKind::ModelUnavailable)
        }
        429 | 500..=599 => temporary(),
        _ => compatibility(),
    };
    error.details.http_status = Some(status);
    error.details.retry_at = retry;
    error
}
#[async_trait]
impl Transport for FixedTransport {
    async fn get(
        &self,
        lease: ClaudeAccessLease,
        query: Query<'_>,
        remaining: &mut usize,
    ) -> Result<Value, ClaudeError> {
        if lease.operation != query.operation() {
            return Err(compatibility());
        }
        let mut request = self
            .client
            .get(self.url(query))
            .bearer_auth(lease.access_token.expose_secret())
            .header("Accept", "application/json")
            .header("Accept-Encoding", "identity")
            .header("anthropic-beta", profile::OAUTH_BETA)
            .header(
                "User-Agent",
                format!("claude-cli/{} (external, cli)", profile::CLI_VERSION),
            );
        if query.operation() == ClaudeOperation::Models {
            request = request.header("anthropic-version", profile::ANTHROPIC_VERSION);
        }
        let mut response = request.send().await.map_err(|_| temporary())?;
        let status = response.status().as_u16();
        let retry = retry_at(response.headers().get("retry-after"));
        // Auth/model rejection is authoritative even if its body is invalid,
        // oversized or never completes. Do not turn it into an offline result.
        if matches!(status, 401 | 403) || status == 404 && matches!(query, Query::Context(_)) {
            return Err(status_error(status, query, retry));
        }
        // A 429 need not have a JSON body, and must not wait for or retry it.
        if status == 429 {
            let mut e = status_error(status, query, retry);
            e.details.provider_code = Some(ClaudeProviderCode::RateLimitError);
            return Err(e);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| temporary())? {
            if chunk.len() > *remaining {
                return Err(compatibility());
            }
            *remaining -= chunk.len();
            bytes.extend_from_slice(&chunk);
        }
        if !(200..300).contains(&status) {
            let mut error = status_error(status, query, retry);
            error.details.provider_code = Some(
                match serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .as_ref()
                    .and_then(|v| v["error"]["type"].as_str())
                {
                    Some("api_error") => ClaudeProviderCode::ApiError,
                    Some("overloaded_error") => ClaudeProviderCode::OverloadedError,
                    Some("invalid_request_error") => ClaudeProviderCode::InvalidRequestError,
                    Some("not_found_error") => ClaudeProviderCode::NotFoundError,
                    _ => ClaudeProviderCode::Unknown,
                },
            );
            return Err(error);
        }
        serde_json::from_slice(&bytes).map_err(|_| compatibility())
    }
}
