//! Direct subscription Messages transport. Credential lifecycle belongs solely to
//! ClaudeAuthorization. No runtime provider selection is enabled by this module.
use super::{
    profile,
    stream::{partial, Decoder, Framer},
    wire::{compatibility, invalid, PreparedRequest},
    *,
};
use crate::providers::{CompletionRequest, Llm, LlmError, ModelRequestGuard, StreamEvent};
use async_stream::try_stream;
use futures_core::Stream;
use futures_util::{
    future::{BoxFuture, Shared},
    FutureExt, StreamExt,
};
use reqwest::{
    header::{HeaderMap, HeaderValue},
    Client,
};
use serde_json::Value;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub struct ClaudeMessagesConfig {
    pub model: String,
    /// Existing conversation/prompt_cache_key; children must receive their own ID.
    pub session_id: String,
    pub binding: ClaudeSessionBinding,
    /// None is appropriate only when a caller supplies its own turn deadline.
    pub request_timeout: Option<Duration>,
}

pub struct ClaudeMessages {
    config: ClaudeMessagesConfig,
    authority: Arc<dyn ClaudeAuthorization>,
    client: Client,
    endpoint: &'static str,
}
impl std::fmt::Debug for ClaudeMessages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaudeMessages")
            .field("profile", &profile::PROFILE_ID)
            .finish_non_exhaustive()
    }
}
impl ClaudeMessages {
    pub fn new(
        config: ClaudeMessagesConfig,
        authority: Arc<dyn ClaudeAuthorization>,
    ) -> Result<Self, ClaudeError> {
        config.binding.validate()?;
        if config.session_id.is_empty()
            || config.session_id.len() > 1024
            || config
                .session_id
                .bytes()
                .any(|byte| byte <= b' ' || byte >= 0x7f)
        {
            return Err(invalid());
        }
        validate_environment()?;
        Ok(Self {
            client: oauth_client(config.request_timeout, true)?,
            config,
            authority,
            endpoint: profile::MESSAGES_URL,
        })
    }

    /// Dropping this stream drops authorization/HTTP/backoff work. Cancellation
    /// is also forwarded to the authority, including its lock and refresh waits.
    pub fn stream_guarded<'a>(
        &'a self,
        mut request: CompletionRequest,
        guard: Option<&'a dyn ModelRequestGuard>,
        cancellation: BoxFuture<'a, ()>,
    ) -> impl Stream<Item = Result<StreamEvent, LlmError>> + Send + 'a {
        try_stream! {
            validate_environment()?;
            self.config.binding.validate()?;
            if let Some(binding) = request.inference.as_ref().and_then(|inference| inference.claude_session.as_ref()) {
                let binding = ClaudeSessionBinding::try_from(binding)?;
                if binding != self.config.binding { Err(invalid())?; }
            }
            let provenance = anthropic_reasoning_provenance(&self.config.model, AnthropicBillingMode::Subscription);
            let (messages, notices) = normalize_anthropic_history(&request.messages, &provenance);
            request.messages = messages;
            let prepared = PreparedRequest::new(&self.config.model, &request)?;
            let cancellation = cancellation.shared();
            for notice in notices { yield StreamEvent::Notice(notice); }
            let mut budget = RetryBudget::default();
            let mut rejected = None;
            'attempt: loop {
                validate_environment()?;
                // Authority performs the final identity/generation/guard check
                // after any renewal/lock wait. There is no await between receiving
                // this lease and starting the one authorized network attempt.
                let lease = cancellable(&cancellation, self.authority.authorize(ClaudeAuthorizationContext {
                    operation: ClaudeOperation::Messages, expected_identity: &self.config.binding.identity,
                    request_guard: guard, cancellation: cancellation.clone().boxed(),
                }, rejected.take())).await?;
                if lease.identity != self.config.binding.identity {
                    let mut error = ClaudeError::new(ClaudeErrorKind::AccountChanged);
                    error.details.expected_identity = Some(self.config.binding.identity.clone());
                    error.details.current_identity = Some(lease.identity);
                    Err(error)?;
                    unreachable!();
                }
                if lease.operation != ClaudeOperation::Messages || lease.installation_id.is_nil() { Err(compatibility())?; }
                let body = prepared.body(lease.installation_id, lease.identity.account_id, &self.config.session_id)?;
                let headers = message_headers(&lease, &self.config.session_id)?;
                let version = lease.version;
                let sending = self.client.post(self.endpoint).headers(headers).body(body).send();
                drop(lease);
                // A transport failure can have uncertain acceptance. Never replay
                // it automatically, even if no output has reached the caller.
                let mut response = cancellable(&cancellation, async { sending.await.map_err(|_| ClaudeError::new(ClaudeErrorKind::AuthTemporarilyUnavailable)) }).await?;
                let status = response.status().as_u16();
                if !response.status().is_success() {
                    let headers = response.headers().clone();
                    let payload = cancellable(&cancellation, error_payload(&mut response)).await?;
                    let mut error = classify_error(Some(status), &headers, &payload);
                    if status == 401 && !budget.auth_recovered && valid_retry_hint(&headers, &error) {
                        if let Some(delay) = budget.wait_delay(&error, Duration::ZERO) {
                            budget.auth_recovered = true;
                            rejected = Some(version);
                            drop(response);
                            if !delay.is_zero() {
                                cancellable(&cancellation, async { tokio::time::sleep(delay).await; Ok(()) }).await?;
                            }
                            continue 'attempt;
                        }
                    }
                    if let Some(delay) = budget.delay(&error) {
                        cancellable(&cancellation, async { tokio::time::sleep(delay).await; Ok(()) }).await?;
                        continue 'attempt;
                    }
                    error.retry = ClaudeRetryDisposition::Terminal;
                    Err(error)?;
                }
                let stream_headers = response.headers().clone();
                if response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_none_or(|v| v.split(';').next().is_none_or(|v| v.trim() != "text/event-stream")) { Err(compatibility())?; }
                let mut chunks = response.bytes_stream();
                let mut decoder = Decoder::default();
                let mut framer = Framer::default();
                loop {
                    let chunk = cancellable(&cancellation, async { chunks.next().await.transpose().map_err(|_| partial()) }).await?;
                    let Some(chunk) = chunk else { break; };
                    for payload in framer.feed(&chunk)? {
                        match decoder.accept(&payload, &prepared.names) {
                            Ok(events) => for event in events { yield event; },
                            Err(mut error) => {
                                if decoder.exposed { Err(partial())?; }
                                error.details.retry_at = retry_at(&stream_headers, SystemTime::now());
                                if !valid_retry_hint(&stream_headers, &error) { error.retry = ClaudeRetryDisposition::Terminal; }
                                if let Some(delay) = budget.delay(&error) {
                                    // Drop the old stream before waiting. A fresh
                                    // lease is mandatory after this delay.
                                    drop(chunks);
                                    cancellable(&cancellation, async { tokio::time::sleep(delay).await; Ok(()) }).await?;
                                    continue 'attempt;
                                }
                                error.retry = ClaudeRetryDisposition::Terminal;
                                Err(error)?;
                            }
                        }
                    }
                    if decoder.ended { break; }
                }
                let mut completion = decoder.finish()?;
                provenance.insert_into(&mut completion.message.provider_fields);
                yield StreamEvent::Final(completion);
                break;
            }
        }
    }
}
impl Llm for ClaudeMessages {
    fn model(&self) -> &str {
        &self.config.model
    }
    fn stream(
        &self,
        request: CompletionRequest,
    ) -> impl Stream<Item = Result<StreamEvent, LlmError>> + Send {
        self.stream_guarded(request, None, std::future::pending().boxed())
    }
}
fn validate_environment() -> Result<(), ClaudeError> {
    if std::env::var_os("ANTHROPIC_BASE_URL").is_some_and(|value| {
        value
            .to_str()
            .is_none_or(|s| !s.is_empty() && s.trim_end_matches('/') != profile::API_ORIGIN)
    }) {
        return Err(ClaudeError::configuration(
            ClaudeConfigurationError::EndpointNotAllowed,
        ));
    }
    Ok(())
}
fn oauth_client(timeout: Option<Duration>, https: bool) -> Result<Client, ClaudeError> {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let mut builder = Client::builder()
        .https_only(https)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10));
    if let Some(timeout) = timeout {
        builder = builder.timeout(timeout);
    }
    builder.build().map_err(|_| compatibility())
}
fn message_headers(lease: &ClaudeAccessLease, session: &str) -> Result<HeaderMap, ClaudeError> {
    let mut headers = HeaderMap::new();
    let mut bearer =
        HeaderValue::from_str(&format!("Bearer {}", lease.access_token.expose_secret()))
            .map_err(|_| compatibility())?;
    bearer.set_sensitive(true);
    headers.insert(reqwest::header::AUTHORIZATION, bearer);
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => "unknown",
    };
    let os = match std::env::consts::OS {
        "macos" => "MacOS",
        "linux" => "Linux",
        _ => "Unknown",
    };
    for (name, value) in [
        ("content-type", "application/json".to_owned()),
        ("accept", "text/event-stream".into()),
        ("accept-encoding", "identity".into()),
        (
            "user-agent",
            format!("claude-cli/{} (external, cli)", profile::CLI_VERSION),
        ),
        ("anthropic-version", profile::ANTHROPIC_VERSION.into()),
        ("anthropic-beta", profile::MESSAGE_BETAS.join(",")),
        ("anthropic-dangerous-direct-browser-access", "true".into()),
        ("x-app", "cli".into()),
        ("x-claude-code-session-id", session.into()),
        ("x-stainless-arch", arch.into()),
        ("x-stainless-os", os.into()),
        ("x-stainless-lang", "js".into()),
        ("x-stainless-package-version", profile::SDK_VERSION.into()),
        ("x-stainless-retry-count", "0".into()),
        ("x-stainless-runtime", "node".into()),
        (
            "x-stainless-runtime-version",
            profile::RUNTIME_VERSION.into(),
        ),
        ("x-stainless-timeout", "600".into()),
    ] {
        headers.insert(
            reqwest::header::HeaderName::from_static(name),
            HeaderValue::from_str(&value).map_err(|_| invalid())?,
        );
    }
    Ok(headers)
}
async fn cancellable<T>(
    cancellation: &Shared<BoxFuture<'_, ()>>,
    future: impl std::future::Future<Output = Result<T, ClaudeError>>,
) -> Result<T, ClaudeError> {
    tokio::select! { biased; _ = cancellation.clone() => Err(ClaudeError::new(ClaudeErrorKind::RequestCancelled)), value = future => value }
}
async fn error_payload(response: &mut reqwest::Response) -> Result<Value, ClaudeError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ClaudeError::new(ClaudeErrorKind::AuthTemporarilyUnavailable))?
    {
        if body.len() + chunk.len() > 64 * 1024 {
            return Ok(Value::Null);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&body).unwrap_or(Value::Null))
}

pub(super) fn classify_error(
    status: Option<u16>,
    headers: &HeaderMap,
    payload: &Value,
) -> ClaudeError {
    let code = match payload["error"]["type"].as_str() {
        Some("authentication_error") => ClaudeProviderCode::AuthenticationError,
        Some("permission_error") => ClaudeProviderCode::PermissionError,
        Some("not_found_error") => ClaudeProviderCode::NotFoundError,
        Some("rate_limit_error") => ClaudeProviderCode::RateLimitError,
        Some("overloaded_error") => ClaudeProviderCode::OverloadedError,
        Some("invalid_request_error") => ClaudeProviderCode::InvalidRequestError,
        Some("api_error") => ClaudeProviderCode::ApiError,
        _ => ClaudeProviderCode::Unknown,
    };
    let kind = match (status, code) {
        (Some(401), _) | (None, ClaudeProviderCode::AuthenticationError) => {
            ClaudeErrorKind::LoginExpired
        }
        (Some(403), _) | (None, ClaudeProviderCode::PermissionError) => {
            ClaudeErrorKind::AccessDenied
        }
        (Some(404), _) | (None, ClaudeProviderCode::NotFoundError) => {
            ClaudeErrorKind::ModelUnavailable
        }
        (Some(429), _) | (None, ClaudeProviderCode::RateLimitError) => ClaudeErrorKind::RateLimited,
        (Some(500 | 502 | 503 | 504 | 529), _) | (None, ClaudeProviderCode::OverloadedError) => {
            ClaudeErrorKind::AuthTemporarilyUnavailable
        }
        _ => ClaudeErrorKind::CompatibilityError,
    };
    let mut error = ClaudeError::new(kind);
    error.details.http_status = status;
    error.details.provider_code = Some(code);
    error.details.retry_at = retry_at(headers, SystemTime::now());
    // Official API spend-cap code; it does not identify a subscription window.
    // https://platform.claude.com/docs/en/api/rate-limits#reaching-your-spend-cap
    let spend_limit = status == Some(429)
        && code == ClaudeProviderCode::RateLimitError
        && payload["error"]["details"]["error_code"] == "enforced_spend_limit_reached";
    // Global rejection is an observational report in anthropics/claude-code
    // issue 29604, not an official/Phase0-tested schema. No window scope or
    // reset is inferred from utilization, per-window status or extra usage.
    let subscription_rejected = status == Some(429)
        && headers
            .get("anthropic-ratelimit-unified-status")
            .and_then(|v| v.to_str().ok())
            == Some("rejected");
    if spend_limit || subscription_rejected {
        error.kind = ClaudeErrorKind::SubscriptionLimited;
        error.recovery = ClaudeRecovery::RetryLater;
    }
    if !spend_limit
        && !subscription_rejected
        && valid_retry_hint(headers, &error)
        && matches!(
            (status, code),
            (Some(500), ClaudeProviderCode::ApiError)
                | (Some(502 | 503 | 504), _)
                | (Some(529), ClaudeProviderCode::OverloadedError)
                | (None, ClaudeProviderCode::OverloadedError)
        )
    {
        error.retry = ClaudeRetryDisposition::SafeBeforeOutput;
    }
    error
}
fn valid_retry_hint(headers: &HeaderMap, error: &ClaudeError) -> bool {
    !headers.contains_key("retry-after") || error.details.retry_at.is_some()
}
fn retry_at(headers: &HeaderMap, now: SystemTime) -> Option<SystemTime> {
    let value = headers.get("retry-after")?.to_str().ok()?;
    let time = if let Ok(seconds) = value.parse::<u64>() {
        now.checked_add(Duration::from_secs(seconds))?
    } else {
        httpdate::parse_http_date(value).ok()?
    };
    // Keep public UTC timestamps in protobuf's representable year 0001..9999
    // range. An invalid/overflowing hint stops recovery; it is never converted
    // into a short default wait that could hammer the provider.
    (time.duration_since(UNIX_EPOCH).ok()?.as_secs() <= 253_402_300_799).then_some(time)
}
#[derive(Default)]
struct RetryBudget {
    auth_recovered: bool,
    transient_retries: u8,
    waited: Duration,
}
impl RetryBudget {
    fn delay(&mut self, error: &ClaudeError) -> Option<Duration> {
        if error.retry != ClaudeRetryDisposition::SafeBeforeOutput || self.transient_retries >= 2 {
            return None;
        }
        let baseline = Duration::from_secs(1 << self.transient_retries);
        let delay = self.wait_delay(error, baseline)?;
        self.transient_retries += 1;
        Some(delay)
    }
    fn wait_delay(&mut self, error: &ClaudeError, baseline: Duration) -> Option<Duration> {
        let delay = error
            .details
            .retry_at
            .and_then(|time| time.duration_since(SystemTime::now()).ok())
            .unwrap_or_default()
            .max(baseline);
        if self.waited.saturating_add(delay) > Duration::from_secs(60) {
            return None;
        }
        self.waited += delay;
        Some(delay)
    }
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
