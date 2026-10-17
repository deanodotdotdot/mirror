//! Bounded model-metadata requests and OpenAI-compatible context-limit parsing.

use std::time::Duration;

use futures_util::FutureExt;
use reqwest::{Client, RequestBuilder};
use serde_json::Value;

use super::{LlmError, ModelApi, ModelProvider, ModelProviderConfig};

pub const DEFAULT_CONTEXT_WINDOW: usize = 256_000;

pub(crate) struct ModelContext {
    pub window: usize,
    pub notice: Option<String>,
}

/// Discover context with the selected provider's own auth and fallback policy.
pub async fn model_context_window(
    provider: &ModelProvider,
    config: &ModelProviderConfig,
) -> Result<usize, LlmError> {
    Ok(model_context(provider, config).await?.window)
}

pub(crate) async fn model_context(
    provider: &ModelProvider,
    config: &ModelProviderConfig,
) -> Result<ModelContext, LlmError> {
    config.validate_subscription_configuration()?;
    if config.api == ModelApi::AnthropicOAuth {
        use super::claude::{
            metadata::ClaudeMetadataService, ClaudeConfigurationError, ClaudeError,
        };
        let authority = provider.claude_authority.clone().ok_or_else(|| {
            ClaudeError::configuration(ClaudeConfigurationError::UnsupportedSurface)
        })?;
        let binding = provider
            .session()
            .binding()
            .ok_or_else(|| ClaudeError::configuration(ClaudeConfigurationError::MissingBinding))?;
        if config.claude_session.as_ref() != Some(&binding) {
            return Err(super::claude::session::binding_changed(
                config.claude_session.as_ref(),
                Some(&binding),
            )
            .into());
        }
        let explicit = config
            .context_window
            .map(u64::try_from)
            .transpose()
            .map_err(|_| {
                ClaudeError::configuration(ClaudeConfigurationError::InvalidRequestOptions)
            })?;
        let service = ClaudeMetadataService::new(authority)?;
        #[cfg(test)]
        let service = provider.test_metadata.as_deref().unwrap_or(&service);
        let result = service
            .model_context(
                &binding.identity,
                &config.model,
                explicit,
                provider.request_guard.as_deref(),
                std::future::pending().boxed(),
            )
            .await?;
        let window = usize::try_from(result.context_window).map_err(|_| {
            ClaudeError::configuration(ClaudeConfigurationError::InvalidRequestOptions)
        })?;
        return Ok(ModelContext {
            window,
            notice: Some(claude_context_notice(&result)),
        });
    }
    let fallback = match config.api {
        ModelApi::CodexResponses => 272_000,
        ModelApi::AnthropicMessages | ModelApi::AnthropicOAuth => 200_000,
        _ => DEFAULT_CONTEXT_WINDOW,
    };
    // Bound connection resolution and authorization, not only the HTTP request.
    let window = tokio::time::timeout(
        Duration::from_secs(10),
        discover_context_window(provider, config),
    )
    .await
    .unwrap_or(Err("model metadata discovery timed out"))
    .unwrap_or_else(|reason| {
        eprintln!("{reason}; using a {fallback}-token fallback");
        fallback
    });
    Ok(ModelContext {
        window,
        notice: None,
    })
}

fn claude_context_notice(result: &super::claude::metadata::ClaudeModelContextResult) -> String {
    use super::claude::metadata::{ContextProvenance, MetadataState};
    let provenance = match result.provenance {
        ContextProvenance::Explicit => "explicit configuration",
        ContextProvenance::ModelMetadata => "selected-model metadata",
        ContextProvenance::KnownModelFallback => "reviewed model fallback",
        ContextProvenance::ConservativeFallback => "conservative fallback",
    };
    let state = match result.info.state {
        MetadataState::Fresh => "fresh",
        MetadataState::Cached => "cached",
        MetadataState::Stale => "stale",
        MetadataState::Offline => "offline",
        MetadataState::Unavailable => "unavailable",
    };
    let fetched = result
        .info
        .fetched_at
        .map(|time| prost_types::Timestamp::from(time).to_string())
        .unwrap_or_else(|| "not fetched".into());
    let mut notice = format!(
        "Claude context for {}: {} tokens; {provenance}; {state}; source fetched: {fetched}.",
        result.model, result.context_window
    );
    if let Some(error) = &result.info.error {
        let error = error.to_protocol();
        notice.push_str(&format!(
            " Metadata error: {}; recovery: {}.",
            error.kind, error.recovery
        ));
    }
    notice
}

async fn discover_context_window(
    provider: &ModelProvider,
    config: &ModelProviderConfig,
) -> Result<usize, &'static str> {
    if let Some(window) = config.context_window {
        return Ok(window);
    }
    // The plan-billed Codex backend has no model catalog.
    if config.api == ModelApi::CodexResponses {
        return Ok(272_000);
    }
    let (base_url, api_key) = config
        .connection()
        .await
        .map_err(|_| "model metadata connection unavailable")?;
    provider
        .verify_request()
        .await
        .map_err(|_| "model metadata request not authorized")?;
    let api_key = (!api_key.is_empty()).then_some(api_key.as_str());
    if config.api != ModelApi::AnthropicMessages {
        return openai_context_window(&base_url, api_key, &config.model).await;
    }
    let mut url = reqwest::Url::parse(&format!("{}/v1/models", base_url.trim_end_matches('/')))
        .map_err(|_| "invalid Anthropic model endpoint")?;
    url.path_segments_mut()
        .map_err(|_| "invalid Anthropic model endpoint")?
        .push(&config.model);
    let mut request = metadata_client()?
        .get(url)
        .header("anthropic-version", "2023-06-01");
    if let Some(key) = api_key {
        request = request.header("x-api-key", key);
    }
    let payload = model_metadata(request).await?;
    // A per-model lookup may resolve an alias to a canonical ID.
    let model = payload["id"]
        .as_str()
        .ok_or("model metadata did not contain an ID")?;
    context_window_from_models(&payload, model)
}

/// Metadata requests must not forward credentials through redirects.
pub fn metadata_client() -> Result<Client, &'static str> {
    Client::builder()
        .user_agent(super::USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| "could not create model metadata client")
}

/// Fetch metadata without including URLs, credentials, or response bodies in diagnostics.
pub async fn model_metadata(request: RequestBuilder) -> Result<Value, &'static str> {
    let mut response = request
        .send()
        .await
        .map_err(|_| "model metadata request failed")?
        .error_for_status()
        .map_err(|_| "model metadata endpoint returned an error")?;
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "could not read model metadata")?
    {
        if body.len() + chunk.len() > 4 * 1024 * 1024 {
            return Err("model metadata exceeded the size limit");
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| "model metadata was not valid JSON")
}

/// Query a model catalog shared by OpenAI-compatible providers.
pub async fn openai_context_window(
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
) -> Result<usize, &'static str> {
    let mut request = metadata_client()?.get(format!("{}/models", base_url.trim_end_matches('/')));
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    context_window_from_models(&model_metadata(request).await?, model)
}

/// Read only the requested model; never borrow another deployment's limit.
pub fn context_window_from_models(payload: &Value, model: &str) -> Result<usize, &'static str> {
    let entry = payload
        .get("data")
        .and_then(Value::as_array)
        .and_then(|entries| entries.iter().find(|entry| entry["id"] == model))
        .or_else(|| (payload["id"] == model).then_some(payload))
        .ok_or("model metadata did not contain the selected model")?;
    [
        "max_model_len",
        "context_length",
        "max_input_tokens",
        "context_window",
    ]
    .into_iter()
    .find_map(|key| positive_limit(&entry[key]))
    .or_else(|| {
        ["n_ctx", "n_ctx_train"]
            .into_iter()
            .find_map(|key| positive_limit(&entry["meta"][key]))
    })
    .ok_or("model metadata did not report a valid context length")
}

fn positive_limit(value: &Value) -> Option<usize> {
    let tokens = usize::try_from(value.as_u64()?).ok()?;
    // Budget arithmetic multiplies a window by percentages before dividing.
    (tokens > 0 && tokens <= usize::MAX / 100).then_some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stale_context_notice_retains_source_time_provenance_and_closed_error() {
        use super::super::claude::{metadata::*, *};
        let result = ClaudeModelContextResult {
            info: MetadataInfo {
                schema_version: 1,
                provider: CLAUDE_PROVIDER,
                identity: ClaudeIdentityBinding {
                    account_id: uuid::Uuid::from_u128(1),
                    organization_id: uuid::Uuid::from_u128(2),
                },
                state: MetadataState::Stale,
                fetched_at: Some(std::time::UNIX_EPOCH + Duration::from_secs(42)),
                age_seconds: Some(999),
                retry_at: None,
                error: Some(ClaudeError::new(
                    ClaudeErrorKind::AuthTemporarilyUnavailable,
                )),
            },
            model: "claude-opus-4-8".into(),
            context_window: 1_000_000,
            provenance: ContextProvenance::ModelMetadata,
        };
        let notice = claude_context_notice(&result);
        assert!(
            notice.contains("selected-model metadata; stale; source fetched: 1970-01-01T00:00:42Z")
        );
        assert!(
            notice.contains("Metadata error: auth_temporarily_unavailable; recovery: retry_later")
        );
        assert!(!notice.contains("fallback") && !notice.contains("fresh"));
    }

    #[test]
    fn reads_reflection_models_contract_without_confusing_output_and_context_limits() {
        let model = json!({
            "id": "reflection-501B-A23B",
            "object": "model",
            "created": 1791158400,
            "owned_by": "reflection",
            "shutdown_date": null,
            "context_length": 262144,
            "max_output_tokens": 65536
        });
        assert_eq!(
            context_window_from_models(&model, "reflection-501B-A23B"),
            Ok(262144)
        );
        assert_eq!(
            context_window_from_models(
                &json!({"object": "list", "data": [model]}),
                "reflection-501B-A23B"
            ),
            Ok(262144)
        );
        assert!(context_window_from_models(
            &json!({"id": "reflection-501B-A23B", "max_output_tokens": 65536}),
            "reflection-501B-A23B",
        )
        .is_err());
    }

    #[tokio::test]
    async fn discovers_reflection_models_contract_over_http() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let base_url = format!(
            "http://{}/openai/v1",
            listener.local_addr().expect("address")
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).await.expect("request header");
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).expect("HTTP headers");
            assert!(request.starts_with("GET /openai/v1/models HTTP/1.1\r\n"));
            assert!(request.contains("Bearer synthetic-reflection-key"));
            assert!(request
                .to_ascii_lowercase()
                .contains("\r\nuser-agent: mirror-cli\r\n"));
            let body = r#"{"object":"list","data":[{"id":"reflection-501B-A23B","object":"model","created":1791158400,"owned_by":"reflection","shutdown_date":null,"context_length":262144}]}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .expect("response");
        });
        // Reflection uses the same catalog lookup, but forbids overriding its endpoint.
        let config = ModelProviderConfig {
            context_window: None,
            api: ModelApi::ChatCompletions,
            model: "reflection-501B-A23B".into(),
            api_key: Some("synthetic-reflection-key".into()),
            base_url: Some(base_url),
            auth_path: None,
            claude_session: None,
            prompt_cache_key: "session".into(),
            request_timeout: None,
        };
        let provider = ModelProvider::new(config.clone(), None)
            .await
            .expect("provider");
        assert_eq!(
            model_context_window(&provider, &config).await.unwrap(),
            262144
        );
        server.await.expect("server");
    }

    #[test]
    fn selects_the_matching_model_and_valid_positive_limit() {
        let payload = json!({"data": [
            {"id": "other", "max_model_len": 999999},
            {"id": "served", "max_model_len": false, "context_length": 131072}
        ]});
        assert_eq!(
            context_window_from_models(&payload, "served").expect("limit"),
            131072
        );
        assert!(context_window_from_models(&payload, "missing").is_err());
        assert_eq!(
            context_window_from_models(
                &json!({"id": "served", "meta": {"n_ctx": 32768}}),
                "served"
            )
            .expect("limit"),
            32768
        );
        assert!(context_window_from_models(
            &json!({"id": "served", "max_model_len": u64::MAX}),
            "served"
        )
        .is_err());
        assert!(context_window_from_models(
            &json!({"id": "served", "context_length": 0}),
            "served"
        )
        .is_err());
    }
}
