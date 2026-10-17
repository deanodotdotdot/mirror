//! Frozen OAuth requests only. No environment credentials, redirects, proxies or retries.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{Client, RequestBuilder};
use serde_json::Value;

use super::{configuration_error, temporary_error, Secret};
use crate::providers::claude::{profile, ClaudeError, ClaudeErrorKind, ClaudeProviderCode};

pub(super) enum TokenRequest<'a> {
    Exchange {
        code: &'a Secret,
        state: &'a Secret,
        verifier: &'a Secret,
        redirect: &'a str,
    },
    Refresh(&'a Secret),
}

#[async_trait]
pub(super) trait OAuthTransport: Send + Sync {
    async fn token(&self, request: TokenRequest<'_>) -> Result<Value, ClaudeError>;
    async fn bootstrap(&self, access: &Secret) -> Result<Value, ClaudeError>;
}

struct FixedTransport {
    client: Client,
}

fn client_builder() -> reqwest::ClientBuilder {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(10))
}

pub(super) fn transport() -> Result<Arc<dyn OAuthTransport>, ClaudeError> {
    Ok(Arc::new(FixedTransport {
        client: client_builder().build().map_err(|_| temporary_error())?,
    }))
}

pub(super) fn check_environment() -> Result<(), ClaudeError> {
    if let Some(base) = std::env::var_os("ANTHROPIC_BASE_URL") {
        if base.to_str().is_none_or(|base| {
            !base.is_empty() && base.trim_end_matches('/') != profile::API_ORIGIN
        }) {
            return Err(configuration_error());
        }
    }
    Ok(())
}

impl FixedTransport {
    async fn json(&self, request: RequestBuilder) -> Result<Value, ClaudeError> {
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut response = request.send().await.map_err(|_| temporary_error())?;
            let status = response.status();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| temporary_error())? {
                if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                    return Err(ClaudeError::new(ClaudeErrorKind::CompatibilityError));
                }
                bytes.extend_from_slice(&chunk);
            }
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                if status.is_server_error() || status.as_u16() == 429 {
                    temporary_error()
                } else {
                    ClaudeError::new(ClaudeErrorKind::CompatibilityError)
                }
            })?;
            if !status.is_success() {
                let code = value.get("error").and_then(|error| {
                    error
                        .as_str()
                        .or_else(|| error.get("type").and_then(Value::as_str))
                });
                let provider_code = match code {
                    Some("invalid_grant") => ClaudeProviderCode::InvalidGrant,
                    Some("authentication_error") => ClaudeProviderCode::AuthenticationError,
                    Some("permission_error") => ClaudeProviderCode::PermissionError,
                    Some("rate_limit_error") => ClaudeProviderCode::RateLimitError,
                    Some("overloaded_error") => ClaudeProviderCode::OverloadedError,
                    _ => ClaudeProviderCode::Unknown,
                };
                let mut error = match provider_code {
                    _ if status.is_server_error() || status.as_u16() == 429 => temporary_error(),
                    ClaudeProviderCode::InvalidGrant if matches!(status.as_u16(), 400 | 401) => {
                        ClaudeError::new(ClaudeErrorKind::LoginRequired)
                    }
                    ClaudeProviderCode::AuthenticationError if status.as_u16() == 401 => {
                        ClaudeError::new(ClaudeErrorKind::LoginRequired)
                    }
                    _ if status.as_u16() == 401 => ClaudeError::new(ClaudeErrorKind::LoginExpired),
                    _ if status.as_u16() == 403 => ClaudeError::new(ClaudeErrorKind::AccessDenied),
                    ClaudeProviderCode::PermissionError => {
                        ClaudeError::new(ClaudeErrorKind::AccessDenied)
                    }
                    _ => ClaudeError::new(ClaudeErrorKind::CompatibilityError),
                };
                error.details.http_status = Some(status.as_u16());
                error.details.provider_code = Some(provider_code);
                return Err(error);
            }
            if !value.is_object() {
                return Err(ClaudeError::new(ClaudeErrorKind::CompatibilityError));
            }
            Ok(value)
        })
        .await
        .map_err(|_| temporary_error())?
    }
}

impl FixedTransport {
    fn token_request(&self, request: TokenRequest<'_>) -> RequestBuilder {
        let mut builder = self
            .client
            .post(profile::TOKEN_URL)
            .header("Accept", "application/json")
            .header("Accept-Encoding", "identity");
        let body = match request {
            TokenRequest::Exchange {
                code,
                state,
                verifier,
                redirect,
            } => serde_json::json!({
                "grant_type":"authorization_code", "client_id":profile::CLIENT_ID,
                "code":code.expose(), "state":state.expose(), "code_verifier":verifier.expose(), "redirect_uri":redirect,
            }),
            TokenRequest::Refresh(refresh) => {
                builder = builder
                    .header("anthropic-beta", profile::OAUTH_BETA)
                    .header(
                        "User-Agent",
                        format!(
                            "anthropic-sdk-typescript/{} userOAuthProvider",
                            profile::SDK_VERSION
                        ),
                    );
                serde_json::json!({"grant_type":"refresh_token", "client_id":profile::CLIENT_ID, "refresh_token":refresh.expose()})
            }
        };
        builder.json(&body)
    }

    fn bootstrap_request(&self, access: &Secret) -> RequestBuilder {
        let mut url = reqwest::Url::parse(profile::BOOTSTRAP_URL).expect("frozen bootstrap URL");
        url.query_pairs_mut()
            .extend_pairs([("entrypoint", "cli"), ("model", "claude-opus-4-8")]);
        self.client
            .get(url)
            .bearer_auth(access.expose())
            .header("Accept", "application/json, text/plain, */*")
            .header("Accept-Encoding", "identity")
            .header("anthropic-beta", profile::OAUTH_BETA)
            .header(
                "User-Agent",
                format!("claude-code/{}", profile::CLI_VERSION),
            )
    }
}

#[async_trait]
impl OAuthTransport for FixedTransport {
    async fn token(&self, request: TokenRequest<'_>) -> Result<Value, ClaudeError> {
        check_environment()?;
        self.json(self.token_request(request)).await
    }
    async fn bootstrap(&self, access: &Secret) -> Result<Value, ClaudeError> {
        check_environment()?;
        self.json(self.bootstrap_request(access)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[test]
    fn frozen_exchange_refresh_bootstrap_requests_never_create_api_keys() {
        let transport = FixedTransport {
            client: client_builder().build().unwrap(),
        };
        let secret = Secret("SYNTHETIC_CANARY".into());
        let exchange = transport
            .token_request(TokenRequest::Exchange {
                code: &secret,
                state: &secret,
                verifier: &secret,
                redirect: "http://localhost:54321/callback",
            })
            .build()
            .unwrap();
        assert_eq!(exchange.url().as_str(), profile::TOKEN_URL);
        assert!(!exchange.headers().contains_key("authorization"));
        assert!(!exchange.headers().contains_key("anthropic-beta"));
        let body: Value =
            serde_json::from_slice(exchange.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["redirect_uri"], "http://localhost:54321/callback");
        assert_eq!(body["client_id"], profile::CLIENT_ID);
        assert_eq!(body["grant_type"], "authorization_code");
        let refresh = transport
            .token_request(TokenRequest::Refresh(&secret))
            .build()
            .unwrap();
        assert_eq!(refresh.url().as_str(), profile::TOKEN_URL);
        assert_eq!(refresh.headers()["anthropic-beta"], profile::OAUTH_BETA);
        assert_eq!(
            refresh.headers()["user-agent"],
            format!(
                "anthropic-sdk-typescript/{} userOAuthProvider",
                profile::SDK_VERSION
            )
        );
        assert!(!refresh.headers().contains_key("x-api-key"));
        let bootstrap = transport.bootstrap_request(&secret).build().unwrap();
        assert_eq!(
            bootstrap.url().origin().ascii_serialization(),
            profile::API_ORIGIN
        );
        assert_eq!(bootstrap.url().path(), "/api/claude_cli/bootstrap");
        assert_eq!(
            bootstrap.headers()["user-agent"],
            format!("claude-code/{}", profile::CLI_VERSION)
        );
        assert!(!bootstrap.headers().contains_key("x-api-key"));
    }
    async fn server(
        status: &str,
        extra: &str,
        body: &str,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
            body.len()
        );
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 8192];
            let _ = stream.read(&mut bytes).await.unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        (url, task)
    }
    #[tokio::test]
    async fn bounded_http_reader_redacts_canaries_and_refuses_redirects() {
        // Only this cfg(test) constructor permits local HTTP. The production
        // constructor and all operation request builders remain fixed HTTPS.
        let transport = FixedTransport {
            client: client_builder().https_only(false).build().unwrap(),
        };
        for (status, body, kind) in [
            (
                "400 Bad Request",
                r#"{"error":"invalid_grant","message":"RAW_SECRET_CANARY"}"#,
                ClaudeErrorKind::LoginRequired,
            ),
            (
                "500 Internal Server Error",
                r#"{"error":"invalid_grant","message":"RAW_SECRET_CANARY"}"#,
                ClaudeErrorKind::AuthTemporarilyUnavailable,
            ),
            (
                "200 OK",
                "RAW_SECRET_CANARY",
                ClaudeErrorKind::CompatibilityError,
            ),
        ] {
            let (url, task) = server(status, "", body).await;
            let error = transport.json(transport.client.get(url)).await.unwrap_err();
            task.await.unwrap();
            assert_eq!(error.kind, kind);
            assert!(!format!("{error:?} {error}").contains("RAW_SECRET_CANARY"));
        }
        let target = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let (url, task) = server(
            "302 Found",
            &format!(
                "Location: http://{}/steal\r\n",
                target.local_addr().unwrap()
            ),
            "{}",
        )
        .await;
        assert_eq!(
            transport
                .json(transport.client.get(url).bearer_auth("SYNTHETIC_CANARY"))
                .await
                .unwrap_err()
                .kind,
            ClaudeErrorKind::CompatibilityError
        );
        task.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), target.accept())
                .await
                .is_err()
        );
    }
}
