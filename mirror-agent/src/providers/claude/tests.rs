use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use futures_util::{future::BoxFuture, StreamExt};
use prost::Message;
use uuid::Uuid;

use super::*;
use crate::providers::{
    CompletionRequest, Llm, LlmError, ModelApi, ModelProvider, ModelProviderConfig,
};

fn identity() -> ClaudeIdentityBinding {
    ClaudeIdentityBinding {
        account_id: Uuid::from_u128(1),
        organization_id: Uuid::from_u128(2),
    }
}

#[test]
fn profile_identity_matches_the_reviewed_wire_fixture() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../diagnostics/claude_oauth/tests/fixtures/wire.json"
    ))
    .unwrap();
    assert_eq!(fixture["profile"], profile::PROFILE_ID);
    assert_eq!(fixture["source_revision"], profile::OMP_REVISION);
    for operation in [
        ClaudeOperation::Messages,
        ClaudeOperation::Models,
        ClaudeOperation::Usage,
    ] {
        let url = reqwest::Url::parse(operation.endpoint()).unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.origin().ascii_serialization(), profile::API_ORIGIN);
    }
}
fn config(api: ModelApi) -> ModelProviderConfig {
    ModelProviderConfig {
        api,
        model: "claude-opus-4-8".into(),
        context_window: None,
        api_key: None,
        base_url: None,
        auth_path: None,
        claude_session: Some(ClaudeSessionBinding::new(identity())),
        prompt_cache_key: "conversation".into(),
        request_timeout: None,
    }
}

#[test]
fn subscription_identity_is_distinct_and_api_aliases_stay_api_billed() {
    assert_eq!("anthropic-oauth".parse(), Ok(ModelApi::AnthropicOAuth));
    for name in ["anthropic", "anthropic-messages"] {
        assert_eq!(name.parse(), Ok(ModelApi::AnthropicMessages));
    }
}

#[test]
fn public_binding_roundtrips_and_rejects_unknown_versions_and_malformed_ids() {
    let binding = ClaudeSessionBinding::new(identity());
    let wire = mirror_protocol::ClaudeSessionBinding::from(&binding);
    let decoded =
        mirror_protocol::ClaudeSessionBinding::decode(wire.encode_to_vec().as_slice()).unwrap();
    assert_eq!(ClaudeSessionBinding::try_from(&decoded).unwrap(), binding);
    let mut invalid = wire.clone();
    invalid.schema_version = 2;
    assert_eq!(
        ClaudeSessionBinding::try_from(&invalid)
            .unwrap_err()
            .details
            .configuration,
        Some(ClaudeConfigurationError::UnknownSchema)
    );
    invalid = wire.clone();
    invalid.identity.as_mut().unwrap().account_id = "synthetic-secret-canary".into();
    let error = ClaudeSessionBinding::try_from(&invalid).unwrap_err();
    assert!(!format!("{error:?} {error}").contains("synthetic-secret-canary"));
    invalid = wire;
    invalid.compatibility_profile = "unknown".into();
    assert!(ClaudeSessionBinding::try_from(&invalid).is_err());
}

#[test]
fn token_and_lease_debug_are_redacted_and_invalid_tokens_are_not_echoed() {
    let token = ClaudeAccessToken::new("synthetic-secret-canary".into()).unwrap();
    assert_eq!(token.expose_secret(), "synthetic-secret-canary");
    let lease = ClaudeAccessLease {
        access_token: token,
        identity: identity(),
        installation_id: Uuid::from_u128(3),
        version: ClaudeTokenVersion {
            generation: Uuid::from_u128(4),
            revision: 1,
        },
        expires_at: UNIX_EPOCH + Duration::from_secs(3600),
        operation: ClaudeOperation::Messages,
    };
    assert!(!format!("{lease:?}").contains("synthetic-secret-canary"));
    let error = ClaudeAccessToken::new("synthetic-secret-canary\r\n".into()).unwrap_err();
    assert!(!format!("{error:?} {error}").contains("synthetic-secret-canary"));
}

#[test]
fn error_json_and_proto_preserve_safe_machine_fields_and_utc_times() {
    let mut error = ClaudeError::new(ClaudeErrorKind::SubscriptionLimited);
    error.details.http_status = Some(429);
    error.details.provider_code = Some(ClaudeProviderCode::RateLimitError);
    error.details.retry_at = Some(UNIX_EPOCH + Duration::from_secs(60));
    error.details.extra_usage = Some(ClaudeExtraUsage::Unknown);
    let json = serde_json::to_value(&error).unwrap();
    assert_eq!(json["kind"], "subscription_limited");
    assert_eq!(json["details"]["retry_at"], "1970-01-01T00:01:00Z");
    assert_eq!(serde_json::from_value::<ClaudeError>(json).unwrap(), error);
    let wire = error.to_protocol();
    assert_eq!(wire.kind, "subscription_limited");
    assert_eq!(wire.http_status, Some(429));
    assert_eq!(wire.retry_disposition, "terminal");
}

#[test]
fn public_status_has_fixed_source_and_no_labels_in_debug_or_internal_version() {
    let mut status = ClaudeAuthStatus::new(ClaudeAuthState::Refreshable);
    status.identity = Some(identity());
    status.expires_at = Some(UNIX_EPOCH);
    status.display = Some(ClaudeAccountDisplay {
        email: Some("user@example.test".into()),
        organization_name: Some("private-label".into()),
    });
    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["provider"], "anthropic-oauth");
    assert_eq!(json["source"], "Mirror login");
    assert_eq!(json["state"], "refreshable");
    assert_eq!(json["expires_at"], "1970-01-01T00:00:00Z");
    assert!(json.get("version").is_none());
    assert!(!format!("{status:?}").contains("private-label"));
    assert!(!format!("{status:?}").contains("user@example.test"));
    let mut error = ClaudeError::new(ClaudeErrorKind::AuthTemporarilyUnavailable);
    error.retry = ClaudeRetryDisposition::SafeBeforeOutput;
    assert_eq!(serde_json::to_value(error).unwrap()["retry"], "terminal");
}

#[tokio::test]
async fn subscription_configuration_never_resolves_an_api_key_or_succeeds_with_fallback() {
    for (mut selected, reason) in [
        (
            config(ModelApi::AnthropicOAuth),
            ClaudeConfigurationError::NotImplemented,
        ),
        (
            {
                let mut c = config(ModelApi::AnthropicOAuth);
                c.api_key = Some("synthetic-secret-canary".into());
                c
            },
            ClaudeConfigurationError::ApiKeyNotAllowed,
        ),
        (
            {
                let mut c = config(ModelApi::AnthropicOAuth);
                c.base_url = Some("http://127.0.0.1:1".into());
                c
            },
            ClaudeConfigurationError::EndpointNotAllowed,
        ),
        (
            {
                let mut c = config(ModelApi::AnthropicOAuth);
                c.auth_path = Some("/must-not-open".into());
                c
            },
            ClaudeConfigurationError::CodexAuthPathNotAllowed,
        ),
    ] {
        selected.context_window = Some(1_000_000); // Explicit metadata cannot hide failure.
        let error = selected.connection().await.unwrap_err();
        assert!(
            matches!(&error, LlmError::Claude(error) if error.details.configuration == Some(reason))
        );
        assert!(!format!("{error:?} {error}").contains("synthetic-secret-canary"));
        assert!(ModelProvider::new(selected.clone(), None).await.is_err());
        let provider = ModelProvider::new(config(ModelApi::Responses), None)
            .await
            .unwrap();
        assert!(crate::providers::model_context_window(&provider, &selected)
            .await
            .is_err());
    }
    let provider = ModelProvider::new(config(ModelApi::Responses), None)
        .await
        .unwrap();
    let mut events = Box::pin(provider.stream(CompletionRequest {
        messages: vec![],
        tools: vec![],
        max_tokens: None,
        inference: Some(mirror_protocol::InferenceConfig {
            api: Some("anthropic-oauth".into()),
            ..Default::default()
        }),
    }));
    assert!(matches!(
        events.next().await,
        Some(Err(LlmError::Claude(_)))
    ));
    assert!(events.next().await.is_none());
}

#[test]
fn switches_clear_credentials_but_preserve_conversation_identity() {
    let mut original = config(ModelApi::CodexResponses);
    original.api_key = Some("synthetic-key".into());
    original.auth_path = Some("/codex-only".into());
    original.base_url = Some("http://localhost:1".into());
    let api = original
        .for_inference(&mirror_protocol::InferenceConfig {
            api: Some("anthropic-messages".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(api.api_key.is_none() && api.auth_path.is_none() && api.base_url.is_none());
    assert_eq!(api.claude_session, original.claude_session);
    let back = api
        .for_inference(&mirror_protocol::InferenceConfig {
            api: Some("anthropic-oauth".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(back.claude_session, original.claude_session);
    let replacement = ClaudeSessionBinding::new(ClaudeIdentityBinding {
        account_id: Uuid::from_u128(5),
        ..identity()
    });
    let error = api
        .for_inference(&mirror_protocol::InferenceConfig {
            claude_session: Some((&replacement).into()),
            ..Default::default()
        })
        .unwrap_err();
    assert!(
        matches!(error, LlmError::Claude(error) if error.kind == ClaudeErrorKind::AccountChanged)
    );
}

#[test]
fn provenance_uses_existing_fields_and_legacy_is_unknown() {
    assert!(ReasoningProvenance::from_fields(None).is_none());
    let provenance = ReasoningProvenance {
        schema_version: 1,
        provider_family: "anthropic".into(),
        billing_mode: AnthropicBillingMode::Subscription,
        model_id: "claude-opus-4-8".into(),
        compatibility_profile: Some(profile::PROFILE_ID.into()),
    };
    let mut fields = Some(prost_types::Struct::default());
    fields
        .as_mut()
        .unwrap()
        .fields
        .insert("unrelated".into(), prost_types::Value::default());
    provenance.insert_into(&mut fields);
    assert_eq!(
        ReasoningProvenance::from_fields(fields.as_ref()),
        Some(provenance.clone())
    );
    assert!(fields.as_ref().unwrap().fields.contains_key("unrelated"));
    let mut unknown = provenance;
    unknown.schema_version = 2;
    unknown.insert_into(&mut fields);
    assert!(ReasoningProvenance::from_fields(fields.as_ref()).is_none());
}

struct MockAuthority;
impl ClaudeAuthorization for MockAuthority {
    fn local_snapshot<'a>(
        &'a self,
        _cancellation: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<ClaudeLocalAuthSnapshot, ClaudeError>> {
        Box::pin(async {
            Ok(ClaudeLocalAuthSnapshot {
                status: ClaudeAuthStatus::new(ClaudeAuthState::LoggedOut),
                version: None,
            })
        })
    }

    fn authorize<'a>(
        &'a self,
        context: ClaudeAuthorizationContext<'a>,
        rejected: Option<ClaudeTokenVersion>,
    ) -> BoxFuture<'a, Result<ClaudeAccessLease, ClaudeError>> {
        Box::pin(async move {
            assert_eq!(context.operation.endpoint(), profile::MESSAGES_URL);
            assert_eq!(rejected.unwrap().revision, 1);
            tokio::select! {
                _ = context.cancellation => Err(ClaudeError::new(ClaudeErrorKind::RequestCancelled)),
                _ = std::future::pending::<()>() => unreachable!(),
            }
        })
    }
}

#[tokio::test]
async fn guarded_snapshot_requires_an_explicit_authority_implementation() {
    struct NeverGuard;
    impl crate::providers::ModelRequestGuard for NeverGuard {
        fn verify(&self) -> BoxFuture<'_, Result<(), crate::providers::LlmError>> {
            Box::pin(async { panic!("unsupported guarded snapshots must fail before work") })
        }
    }
    let authority: Arc<dyn ClaudeAuthorization> = Arc::new(MockAuthority);
    let error = authority
        .local_snapshot_guarded(&NeverGuard, Box::pin(std::future::pending()))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ClaudeErrorKind::InvalidConfiguration);
    assert_eq!(
        error.details.configuration,
        Some(ClaudeConfigurationError::NotImplemented)
    );
}

#[tokio::test]
async fn authority_is_object_safe_and_cancellation_can_interrupt_a_pending_wait() {
    let authority: Arc<dyn ClaudeAuthorization> = Arc::new(MockAuthority);
    let identity = identity();
    let snapshot = authority
        .local_snapshot(Box::pin(std::future::pending()))
        .await
        .unwrap();
    assert_eq!(snapshot.status.state, ClaudeAuthState::LoggedOut);
    assert!(snapshot.version.is_none());
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    let future = authority.authorize(
        ClaudeAuthorizationContext {
            operation: ClaudeOperation::Messages,
            expected_identity: &identity,
            request_guard: None,
            cancellation: Box::pin(async {
                let _ = cancelled.await;
            }),
        },
        Some(ClaudeTokenVersion {
            generation: Uuid::from_u128(4),
            revision: 1,
        }),
    );
    cancel.send(()).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(1), future)
        .await
        .unwrap();
    assert!(matches!(result, Err(error) if error.kind == ClaudeErrorKind::RequestCancelled));
}
