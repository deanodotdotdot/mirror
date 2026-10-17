use super::*;
use crate::providers::claude::{
    ClaudeAccessLease, ClaudeAccessToken, ClaudeAuthStatus, ClaudeExtraUsage, ClaudeLimitScope,
    ClaudeLocalAuthSnapshot,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

fn identity() -> ClaudeIdentityBinding {
    ClaudeIdentityBinding {
        account_id: Uuid::from_u128(1),
        organization_id: Uuid::from_u128(2),
    }
}
fn pending<'a>() -> BoxFuture<'a, ()> {
    std::future::pending().boxed()
}
struct AuthState {
    identity: ClaudeIdentityBinding,
    version: ClaudeTokenVersion,
    state: ClaudeAuthState,
    calls: Vec<Option<ClaudeTokenVersion>>,
    error: Option<ClaudeError>,
}
struct Authority {
    state: Mutex<AuthState>,
    hold_snapshot: AtomicBool,
    hold_authorize: AtomicBool,
}
impl Authority {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(AuthState {
                identity: identity(),
                version: ClaudeTokenVersion {
                    generation: Uuid::from_u128(3),
                    revision: 1,
                },
                state: ClaudeAuthState::Usable,
                calls: Vec::new(),
                error: None,
            }),
            hold_snapshot: AtomicBool::new(false),
            hold_authorize: AtomicBool::new(false),
        })
    }
    fn calls(&self) -> usize {
        self.state.lock().unwrap().calls.len()
    }
    fn snapshot_now(&self) -> ClaudeLocalAuthSnapshot {
        let s = self.state.lock().unwrap();
        let mut status = ClaudeAuthStatus::new(s.state);
        status.identity = Some(s.identity.clone());
        ClaudeLocalAuthSnapshot {
            status,
            version: Some(s.version),
        }
    }
}
impl ClaudeAuthorization for Authority {
    fn authorize<'a>(
        &'a self,
        context: ClaudeAuthorizationContext<'a>,
        rejected: Option<ClaudeTokenVersion>,
    ) -> BoxFuture<'a, Result<ClaudeAccessLease, ClaudeError>> {
        async move {
            if self.hold_authorize.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if let Some(guard) = context.request_guard {
                guard.verify().await.map_err(|_| cancelled())?;
            }
            let mut s = self.state.lock().unwrap();
            if context.expected_identity != &s.identity {
                return Err(identity_error(context.expected_identity, &s.identity));
            }
            if let Some(error) = &s.error {
                return Err(error.clone());
            }
            s.calls.push(rejected);
            if rejected.is_some() {
                s.version.revision += 1;
            }
            Ok(ClaudeAccessLease {
                access_token: ClaudeAccessToken::new(format!(
                    "SYNTHETIC_ACCESS_CANARY_{}",
                    s.calls.len()
                ))
                .unwrap(),
                identity: s.identity.clone(),
                installation_id: Uuid::from_u128(4),
                version: s.version,
                expires_at: SystemTime::now() + Duration::from_secs(3600),
                operation: context.operation,
            })
        }
        .boxed()
    }
    fn local_snapshot<'a>(
        &'a self,
        _: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<ClaudeLocalAuthSnapshot, ClaudeError>> {
        async move {
            if self.hold_snapshot.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(self.snapshot_now())
        }
        .boxed()
    }
    fn local_snapshot_guarded<'a>(
        &'a self,
        guard: &'a dyn ModelRequestGuard,
        _: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<ClaudeLocalAuthSnapshot, ClaudeError>> {
        async move {
            if self.hold_snapshot.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            guard.verify().await.map_err(|_| cancelled())?;
            Ok(self.snapshot_now())
        }
        .boxed()
    }
}
struct Script {
    replies: Mutex<VecDeque<Result<Value, ClaudeError>>>,
    calls: AtomicUsize,
    hook: Mutex<Option<Box<dyn Fn() + Send>>>,
}
impl Script {
    fn new(replies: Vec<Result<Value, ClaudeError>>) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(replies.into()),
            calls: AtomicUsize::new(0),
            hook: Mutex::new(None),
        })
    }
}
#[async_trait]
impl Transport for Script {
    async fn get(
        &self,
        _: ClaudeAccessLease,
        _: Query<'_>,
        _: &mut usize,
    ) -> Result<Value, ClaudeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(hook) = self.hook.lock().unwrap().take() {
            hook();
        }
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected HTTP attempt")
    }
}
fn service(
    authority: Arc<dyn ClaudeAuthorization>,
    transport: Arc<dyn Transport>,
) -> (tempfile::TempDir, ClaudeMetadataService) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().canonicalize().unwrap().join(".mirror");
    let service = ClaudeMetadataService {
        authority,
        transport,
        cache: Cache::new(path).unwrap(),
    };
    (temp, service)
}
fn page(models: Value) -> Value {
    json!({"data":models,"has_more":false})
}
fn usage_value() -> Value {
    serde_json::from_str(include_str!("fixtures/usage-generic.json")).unwrap()
}
fn endpoint(status: u16) -> ClaudeError {
    let mut error = ClaudeError::new(match status {
        401 => ClaudeErrorKind::LoginExpired,
        403 => ClaudeErrorKind::AccessDenied,
        404 => ClaudeErrorKind::ModelUnavailable,
        _ => ClaudeErrorKind::AuthTemporarilyUnavailable,
    });
    error.details.http_status = Some(status);
    error
}
async fn age_cache(s: &ClaudeMetadataService, seconds: u64) {
    let key = s.key(None, &pending().shared()).await.unwrap();
    let locked = s.cache.lock().await.unwrap();
    let mut record = locked.load(key).unwrap();
    for time in record
        .catalog
        .iter_mut()
        .map(|v| &mut v.fetched_at)
        .chain(record.usage.iter_mut().map(|v| &mut v.fetched_at))
        .chain(record.contexts.values_mut().map(|v| &mut v.fetched_at))
    {
        *time = Some(SystemTime::now() - Duration::from_secs(seconds));
    }
    locked.write(&record).unwrap();
}
async fn clear_cooldown(s: &ClaudeMetadataService) {
    let key = s.key(None, &pending().shared()).await.unwrap();
    let locked = s.cache.lock().await.unwrap();
    let mut record = locked.load(key).unwrap();
    for f in record
        .catalog_failure
        .iter_mut()
        .chain(record.usage_failure.iter_mut())
        .chain(record.context_failures.values_mut())
    {
        f.retry_at = None;
    }
    locked.write(&record).unwrap();
}

#[test]
fn usage_fixtures_preserve_nulls_scopes_units_and_disabled_settings() {
    let legacy =
        usage::parse(serde_json::from_str(include_str!("fixtures/usage-legacy.json")).unwrap())
            .unwrap();
    assert_eq!(legacy.windows.len(), 4);
    assert_eq!(legacy.windows[0].used_percent, Some(27.5));
    assert_eq!(legacy.windows[1].used_percent, None);
    assert_eq!(legacy.windows[2].scope, ClaudeLimitScope::Model);
    assert_eq!(legacy.windows[3].used_percent, Some(105.0));
    assert_eq!(
        prost_types::Timestamp::from(legacy.windows[3].resets_at.unwrap()).to_string(),
        "2026-10-20T07:00:00Z"
    );
    assert_eq!(legacy.extra_usage.state, ClaudeExtraUsage::Disabled);
    assert_eq!(
        legacy.extra_usage.used.as_ref().unwrap(),
        &ReportedAmount {
            value: 12345.0,
            unit: AmountUnit::Credits,
            currency: Some("EUR".into()),
            decimal_places: Some(3)
        }
    );
    assert!(legacy.extra_usage.limit.is_none());
    let generic = usage::parse(usage_value()).unwrap();
    assert_eq!(generic.windows.len(), 4);
    assert_eq!(generic.windows[1].used_percent, Some(77.0));
    assert_eq!(generic.windows[2].model.as_deref(), Some("Fable"));
    assert_eq!(generic.windows[2].scope, ClaudeLimitScope::Model);
    assert_eq!(generic.windows[2].is_active, Some(false));
    assert_eq!(generic.windows[3].scope, ClaudeLimitScope::Unknown);
    assert_eq!(generic.extra_usage.state, ClaudeExtraUsage::Enabled);
    assert_eq!(generic.extra_usage.source, ExtraUsageSource::Spend);
    assert_eq!(
        generic.extra_usage.used.as_ref().unwrap().decimal_places,
        Some(3)
    );
    assert_eq!(
        generic
            .extra_usage
            .limit
            .as_ref()
            .unwrap()
            .currency
            .as_deref(),
        Some("JPY")
    );
    assert_eq!(
        generic.extra_usage.limit.as_ref().unwrap().decimal_places,
        Some(0)
    );
    let unknown =
        usage::parse(json!({"extra_usage":{"used_credits":0,"monthly_limit":null}})).unwrap();
    assert_eq!(unknown.extra_usage.state, ClaudeExtraUsage::Unknown);
    assert_eq!(unknown.extra_usage.used.as_ref().unwrap().currency, None);
    assert_eq!(
        unknown.extra_usage.used.as_ref().unwrap().decimal_places,
        None
    );
    assert!(usage::parse(json!({"not_usage":"SYNTHETIC_PAYLOAD_CANARY"})).is_err());
    for value in [
        json!({"limits":[{"kind":"weekly_all","percent":-1,"resets_at":"bad"}]}),
        json!({"five_hour":{"utilization":"NaN"}}),
    ] {
        let parsed = usage::parse(value).unwrap();
        assert!(parsed.windows[0].used_percent.is_none());
        assert!(parsed.windows[0].resets_at.is_none());
    }
    assert!(!serde_json::to_string(&generic)
        .unwrap()
        .contains("SYNTHETIC"));
}

#[tokio::test]
async fn catalog_paginates_fresh_authorization_and_coordinates_only_one_401() {
    let authority = Authority::new();
    let transport = Script::new(vec![
        Ok(json!({"data":[{"id":"a","max_input_tokens":12345}],"has_more":true,"last_id":"a"})),
        Err(endpoint(401)),
        Ok(page(json!([{"id":"b","max_tokens":99999}]))),
    ]);
    let (_temp, s) = service(authority.clone(), transport);
    let catalog = s.models(None, None, pending()).await.unwrap();
    assert_eq!(catalog.info.state, MetadataState::Fresh);
    assert_eq!(catalog.models.len(), 2);
    assert_eq!(catalog.models[0].context_window, Some(12345));
    assert_eq!(catalog.models[1].context_window, None);
    assert_eq!(authority.calls(), 3);
    let calls = authority.state.lock().unwrap().calls.clone();
    assert_eq!(calls[0], None);
    assert_eq!(calls[1], None);
    assert_eq!(calls[2].unwrap().revision, 1);
    let cached = s.models(None, None, pending()).await.unwrap();
    assert_eq!(cached.info.state, MetadataState::Cached);
    assert_eq!(authority.calls(), 3);
    assert!(!serde_json::to_string(&cached).unwrap().contains("CANARY"));
}
#[tokio::test]
async fn context_precedence_is_exact_validated_and_never_borrows_or_infers_one_million() {
    let auth = Authority::new();
    let transport = Script::new(vec![
        Ok(page(
            json!([{"id":"other","max_input_tokens":1_000_000},{"id":"selected","max_input_tokens":123_456}]),
        )),
        Ok(json!({"id":"claude-opus-4-8","max_tokens":128000})),
        Ok(json!({"id":"unknown-1m"})),
        Err(endpoint(404)),
    ]);
    let (_temp, s) = service(auth.clone(), transport);
    for value in [0, models::MAX_CONTEXT + 1] {
        assert_eq!(
            s.model_context(&identity(), "selected", Some(value), None, pending())
                .await
                .unwrap_err()
                .kind,
            ClaudeErrorKind::InvalidConfiguration
        );
    }
    let explicit = s
        .model_context(&identity(), "selected", Some(23456), None, pending())
        .await
        .unwrap();
    assert_eq!(explicit.context_window, 23456);
    assert_eq!(explicit.provenance, ContextProvenance::Explicit);
    assert_eq!(auth.calls(), 0);
    s.models(None, None, pending()).await.unwrap();
    let selected = s
        .model_context(&identity(), "selected", None, None, pending())
        .await
        .unwrap();
    assert_eq!(selected.context_window, 123456);
    assert_eq!(selected.provenance, ContextProvenance::ModelMetadata);
    assert_eq!(auth.calls(), 1);
    let known = s
        .model_context(&identity(), "claude-opus-4-8", None, None, pending())
        .await
        .unwrap();
    assert_eq!(known.context_window, 200000);
    assert_eq!(known.provenance, ContextProvenance::KnownModelFallback);
    let unknown = s
        .model_context(&identity(), "unknown-1m", None, None, pending())
        .await
        .unwrap();
    assert_eq!(unknown.context_window, 200000);
    assert_eq!(unknown.provenance, ContextProvenance::ConservativeFallback);
    assert_eq!(
        s.model_context(&identity(), "unavailable", None, None, pending())
            .await
            .unwrap_err()
            .kind,
        ClaudeErrorKind::ModelUnavailable
    );
}
#[tokio::test]
async fn detail_without_context_cannot_hide_exact_catalog_context() {
    let transport = Script::new(vec![
        Ok(json!({"id":"selected","max_tokens":128000})),
        Ok(page(json!([{"id":"selected","max_input_tokens":123456}]))),
        Err(temporary()),
        Ok(json!({"id":"selected","max_tokens":128000})),
    ]);
    let auth = Authority::new();
    let (_temp, service) = service(auth.clone(), transport);
    let detail = service
        .model_context(&identity(), "selected", None, None, pending())
        .await
        .unwrap();
    assert_eq!(detail.provenance, ContextProvenance::ConservativeFallback);
    let catalog = service.models(None, None, pending()).await.unwrap();
    let selected = service
        .model_context(&identity(), "selected", None, None, pending())
        .await
        .unwrap();
    assert_eq!(selected.context_window, 123456);
    assert_eq!(selected.provenance, ContextProvenance::ModelMetadata);
    assert_eq!(selected.info.fetched_at, catalog.info.fetched_at);
    assert_eq!(auth.calls(), 2);

    age_cache(&service, 600).await;
    let stale = service
        .model_context(&identity(), "selected", None, None, pending())
        .await
        .unwrap();
    assert_eq!(stale.context_window, 123456);
    assert_eq!(stale.provenance, ContextProvenance::ModelMetadata);
    assert_eq!(stale.info.state, MetadataState::Stale);
    assert!(stale.info.age_seconds.unwrap() >= 600);
    assert!(stale.info.error.is_some());
    clear_cooldown(&service).await;
    for _ in 0..2 {
        let omitted = service
            .model_context(&identity(), "selected", None, None, pending())
            .await
            .unwrap();
        assert_eq!(omitted.context_window, 123456);
        assert_eq!(omitted.provenance, ContextProvenance::ModelMetadata);
        assert_eq!(omitted.info.state, MetadataState::Stale);
        assert_eq!(omitted.info.fetched_at, stale.info.fetched_at);
        assert!(omitted.info.age_seconds.unwrap() >= 600);
        assert!(omitted.info.error.is_none());
    }
    assert_eq!(auth.calls(), 4);
}

#[tokio::test]
async fn stale_catalog_and_usage_preserve_original_age_and_429_cooldown() {
    let auth = Authority::new();
    let mut limited = endpoint(429);
    let retry = SystemTime::now() + Duration::from_secs(120);
    limited.details.retry_at = Some(retry);
    let transport = Script::new(vec![
        Ok(page(json!([{"id":"a"}]))),
        Ok(usage_value()),
        Err(temporary()),
        Err(limited),
    ]);
    let (_temp, s) = service(auth.clone(), transport);
    s.models(None, None, pending()).await.unwrap();
    s.usage(None, None, pending()).await.unwrap();
    age_cache(&s, 600).await;
    let stale = s.models(None, None, pending()).await.unwrap();
    assert_eq!(stale.info.state, MetadataState::Stale);
    assert!(stale.info.age_seconds.unwrap() >= 600);
    let usage = s.usage(None, None, pending()).await.unwrap();
    assert_eq!(usage.info.state, MetadataState::Stale);
    assert_eq!(usage.info.retry_at, Some(retry));
    assert!(usage.info.age_seconds.unwrap() >= 600);
    let again = s.usage(None, None, pending()).await.unwrap();
    assert_eq!(again.info.fetched_at, usage.info.fetched_at);
    assert_eq!(again.info.state, MetadataState::Stale);
    assert_eq!(auth.calls(), 4);
}
#[tokio::test]
async fn no_login_revoked_mismatch_and_generation_or_profile_cannot_use_stale_cache() {
    let auth = Authority::new();
    let transport = Script::new(vec![
        Ok(page(json!([{"id":"a"}]))),
        Ok(usage_value()),
        Ok(page(json!([{"id":"new-generation"}]))),
        Ok(page(json!([{"id":"new-profile"}]))),
    ]);
    let (_temp, s) = service(auth.clone(), transport);
    s.models(None, None, pending()).await.unwrap();
    s.usage(None, None, pending()).await.unwrap();
    age_cache(&s, 600).await;
    for state in [ClaudeAuthState::LoggedOut, ClaudeAuthState::LoginRequired] {
        auth.state.lock().unwrap().state = state;
        assert_eq!(
            s.models(None, None, pending()).await.unwrap_err().kind,
            ClaudeErrorKind::LoginRequired
        );
        assert_eq!(
            s.usage(None, None, pending()).await.unwrap_err().kind,
            ClaudeErrorKind::LoginRequired
        );
        assert_eq!(
            s.model_context(&identity(), "a", Some(200000), None, pending())
                .await
                .unwrap_err()
                .kind,
            ClaudeErrorKind::LoginRequired
        );
    }
    auth.state.lock().unwrap().state = ClaudeAuthState::Usable;
    let different = ClaudeIdentityBinding {
        account_id: Uuid::from_u128(77),
        organization_id: identity().organization_id,
    };
    assert_eq!(
        s.models(Some(&different), None, pending())
            .await
            .unwrap_err()
            .kind,
        ClaudeErrorKind::AccountChanged
    );
    assert_eq!(auth.calls(), 2);
    auth.state.lock().unwrap().version.generation = Uuid::new_v4();
    assert_eq!(
        s.models(None, None, pending()).await.unwrap().models[0].id,
        "new-generation"
    );
    {
        let key = s.key(None, &pending().shared()).await.unwrap();
        let locked = s.cache.lock().await.unwrap();
        let mut record = locked.load(key).unwrap();
        record.key.compatibility_profile = "old-profile".into();
        locked.write(&record).unwrap();
    }
    assert_eq!(
        s.models(None, None, pending()).await.unwrap().models[0].id,
        "new-profile"
    );
}
#[tokio::test]
async fn final_auth_rejection_and_failed_recovery_never_become_offline_success() {
    for code in [401, 403] {
        let auth = Authority::new();
        let transport = Script::new(vec![
            Ok(page(json!([{"id":"a"}]))),
            Err(endpoint(code)),
            Err(endpoint(code)),
            Err(temporary()),
        ]);
        let (_temp, s) = service(auth.clone(), transport);
        s.models(None, None, pending()).await.unwrap();
        age_cache(&s, 600).await;
        let error = s.models(None, None, pending()).await.unwrap_err();
        assert_eq!(
            error.kind,
            if code == 401 {
                ClaudeErrorKind::LoginExpired
            } else {
                ClaudeErrorKind::AccessDenied
            }
        );
        // Later transport outage must not resurrect cache/catalog fallback.
        assert!(s.models(None, None, pending()).await.is_err());
    }
    let auth = Authority::new();
    let transport = Script::new(vec![Err(endpoint(401))]);
    let auth2 = auth.clone();
    *transport.hook.lock().unwrap() = Some(Box::new(move || {
        auth2.state.lock().unwrap().error = Some(temporary())
    }));
    let (_temp, s) = service(auth, transport);
    assert_eq!(
        s.models(None, None, pending()).await.unwrap_err().kind,
        ClaudeErrorKind::AuthTemporarilyUnavailable
    );
}
#[tokio::test]
async fn usage_failure_does_not_poison_authority_or_other_endpoint_cache() {
    let auth = Authority::new();
    let transport = Script::new(vec![Err(endpoint(403)), Ok(page(json!([{"id":"a"}])))]);
    let (_temp, s) = service(auth.clone(), transport);
    assert_eq!(
        s.usage(None, None, pending()).await.unwrap_err().kind,
        ClaudeErrorKind::AccessDenied
    );
    assert_eq!(
        s.models(None, None, pending()).await.unwrap().info.state,
        MetadataState::Fresh
    );
    assert_eq!(auth.state.lock().unwrap().state, ClaudeAuthState::Usable);
}
#[tokio::test]
async fn revocation_or_generation_change_during_http_prevents_cache_publication() {
    for revoked in [false, true] {
        let auth = Authority::new();
        let transport = Script::new(vec![Ok(usage_value())]);
        let auth2 = auth.clone();
        *transport.hook.lock().unwrap() = Some(Box::new(move || {
            let mut s = auth2.state.lock().unwrap();
            if revoked {
                s.state = ClaudeAuthState::LoginRequired;
            } else {
                s.version.generation = Uuid::new_v4();
            }
        }));
        let (temp, s) = service(auth, transport);
        assert!(s.usage(None, None, pending()).await.is_err());
        assert!(!temp.path().join(".mirror/claude_metadata.json").exists());
    }
}
#[tokio::test(start_paused = true)]
async fn ten_second_budget_includes_authority_and_metadata_lock_waits() {
    for stage in ["snapshot", "authorize", "metadata"] {
        let auth = Authority::new();
        let (_temp, s) = service(auth.clone(), Script::new(vec![]));
        let lock = if stage == "metadata" {
            Some(s.cache.lock().await.unwrap())
        } else {
            None
        };
        auth.hold_snapshot
            .store(stage == "snapshot", Ordering::SeqCst);
        auth.hold_authorize
            .store(stage == "authorize", Ordering::SeqCst);
        let start = tokio::time::Instant::now();
        assert_eq!(
            s.models(None, None, pending()).await.unwrap_err().kind,
            ClaudeErrorKind::AuthTemporarilyUnavailable
        );
        assert_eq!(tokio::time::Instant::now() - start, BUDGET);
        drop(lock);
    }
}
#[tokio::test]
async fn cancellation_and_drop_release_metadata_lock_without_detached_work() {
    let auth = Authority::new();
    auth.hold_authorize.store(true, Ordering::SeqCst);
    let (_temp, s) = service(auth.clone(), Script::new(vec![]));
    let (send, recv) = tokio::sync::oneshot::channel();
    let cancellation = async {
        let _ = recv.await;
    }
    .boxed();
    let future = s.models(None, None, cancellation);
    tokio::pin!(future);
    assert!(tokio::time::timeout(Duration::from_millis(30), &mut future)
        .await
        .is_err());
    send.send(()).unwrap();
    assert_eq!(
        future.await.unwrap_err().kind,
        ClaudeErrorKind::RequestCancelled
    );
    let _lock = tokio::time::timeout(Duration::from_millis(100), s.cache.lock())
        .await
        .unwrap()
        .unwrap();
}
#[tokio::test]
async fn dropping_owned_operation_releases_lock_and_does_not_request_later() {
    let auth = Authority::new();
    auth.hold_authorize.store(true, Ordering::SeqCst);
    let script = Script::new(vec![]);
    let (_temp, s) = service(auth.clone(), script.clone());
    {
        let mut future = Box::pin(s.usage(None, None, pending()));
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut future)
            .await
            .is_err());
    }
    let _lock = tokio::time::timeout(Duration::from_millis(100), s.cache.lock())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(script.calls.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn same_poll_cancel_before_authorization_after_lease_and_before_publication() {
    struct Guard {
        calls: AtomicUsize,
        trigger: usize,
        send: tokio::sync::watch::Sender<bool>,
    }
    impl ModelRequestGuard for Guard {
        fn verify(&self) -> BoxFuture<'_, Result<(), crate::providers::LlmError>> {
            async {
                if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.trigger {
                    self.send.send_replace(true);
                }
                Ok(())
            }
            .boxed()
        }
    }
    for trigger in [1, 2, 3] {
        let auth = Authority::new();
        let script = Script::new(vec![Ok(page(json!([{"id":"a"}])))]);
        let (temp, s) = service(auth.clone(), script.clone());
        let (send, mut recv) = tokio::sync::watch::channel(false);
        let guard = Guard {
            calls: AtomicUsize::new(0),
            trigger,
            send,
        };
        let cancel = async move {
            while !*recv.borrow() {
                recv.changed().await.unwrap();
            }
        }
        .boxed();
        assert_eq!(
            s.models(None, Some(&guard), cancel).await.unwrap_err().kind,
            ClaudeErrorKind::RequestCancelled
        );
        assert_eq!(auth.calls(), usize::from(trigger >= 2));
        assert_eq!(
            script.calls.load(Ordering::SeqCst),
            usize::from(trigger == 3)
        );
        assert!(!temp.path().join(".mirror/claude_metadata.json").exists());
    }
}

#[test]
fn pagination_rejects_duplicate_models_nonprogress_oversize_and_invalid_limits() {
    for value in [
        json!({"data":[{"id":"a"}],"has_more":true,"last_id":"b"}),
        json!({"data":[],"has_more":true,"last_id":"a"}),
        json!({"data":[{"id":"a"},{"id":"a"}],"has_more":false}),
        json!({"data":[{"id":"bad/path"}],"has_more":false}),
        json!({"data":vec![json!({"id":"a"});1025],"has_more":false}),
    ] {
        assert!(models::parse_page(value, &mut HashSet::new()).is_err());
    }
    for v in [json!(true), json!(0), json!(-1), json!(u64::MAX)] {
        assert_eq!(
            models::parse_model(&json!({"id":"a","max_input_tokens":v,"max_tokens":1000000}))
                .unwrap()
                .context_window,
            None
        );
    }
}

struct Reply {
    status: u16,
    headers: String,
    body: String,
}
impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            headers: String::new(),
            body: value.to_string(),
        }
    }
}
async fn server(replies: Vec<Reply>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for reply in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let byte = socket.read_u8().await.unwrap();
                request.push(byte);
                assert!(request.len() < 16384);
            }
            requests.push(String::from_utf8(request).unwrap());
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 {} Test\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                        reply.status,
                        reply.body.len(),
                        reply.headers,
                        reply.body
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        requests
    });
    (origin, task)
}
#[tokio::test]
async fn actual_http_catalog_and_usage_use_only_fresh_oauth_headers_and_encoded_cursor() {
    let (origin, server) = server(vec![
        Reply::json(json!({"data":[{"id":"a:1"}],"has_more":true,"last_id":"a:1"})),
        Reply {
            status: 401,
            headers: String::new(),
            body: "SYNTHETIC_ERROR_CANARY".into(),
        },
        Reply::json(page(json!([{"id":"b"}]))),
        Reply::json(usage_value()),
    ])
    .await;
    let auth = Authority::new();
    let (temp, s) = service(auth, Arc::new(http::FixedTransport::loopback(origin)));
    s.models(None, None, pending()).await.unwrap();
    let result = s.usage(None, None, pending()).await.unwrap();
    let requests = server.await.unwrap();
    assert!(requests[0].starts_with("GET /v1/models?limit=100 "));
    assert!(requests[1].contains("after_id=a%3A1"));
    assert!(requests[3].starts_with("GET /api/oauth/usage "));
    for (i, request) in requests.iter().enumerate() {
        assert!(request.contains(&format!("Bearer SYNTHETIC_ACCESS_CANARY_{}", i + 1)));
        assert!(!request.contains("x-api-key"));
        assert!(request.contains("anthropic-beta: oauth-2025-04-20"));
    }
    assert!(!requests[3].contains("anthropic-version"));
    assert!(requests[0].contains("anthropic-version: 2023-06-01"));
    for output in [
        serde_json::to_string(&result).unwrap(),
        std::fs::read_to_string(temp.path().join(".mirror/claude_metadata.json")).unwrap(),
    ] {
        assert!(!output.contains("CANARY"));
        assert!(!output.contains("access_token"));
    }
}
#[tokio::test]
async fn redirects_never_forward_tokens_and_failed_payloads_are_bounded() {
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (origin, server) = server(vec![Reply {
        status: 302,
        headers: format!(
            "Location: http://{}/stolen\r\n",
            target.local_addr().unwrap()
        ),
        body: "SYNTHETIC_ERROR_CANARY".into(),
    }])
    .await;
    let (_temp, s) = service(
        Authority::new(),
        Arc::new(http::FixedTransport::loopback(origin)),
    );
    let result = s.models(None, None, pending()).await.unwrap();
    assert_eq!(result.info.state, MetadataState::Offline);
    assert!(
        tokio::time::timeout(Duration::from_millis(40), target.accept())
            .await
            .is_err()
    );
    server.await.unwrap();
    let (origin, server) = self::server(vec![Reply {
        status: 200,
        headers: String::new(),
        body: "x".repeat(http::MAX_BYTES + 1),
    }])
    .await;
    let (_temp, s) = service(
        Authority::new(),
        Arc::new(http::FixedTransport::loopback(origin)),
    );
    assert_eq!(
        s.usage(None, None, pending()).await.unwrap().info.state,
        MetadataState::Unavailable
    );
    let _ = server.await;
}

#[tokio::test]
async fn private_cache_rejects_symlinks_modes_hardlinks_and_replaced_lock() {
    use std::os::unix::fs::symlink;
    for kind in ["symlink", "mode", "hardlink", "lock"] {
        let (temp, s) = service(Authority::new(), Script::new(vec![Ok(usage_value())]));
        s.usage(None, None, pending()).await.unwrap();
        let file = temp.path().join(".mirror/claude_metadata.json");
        match kind {
            "symlink" => {
                std::fs::remove_file(&file).unwrap();
                symlink(temp.path().join("absent"), &file).unwrap();
            }
            "mode" => {
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap()
            }
            "hardlink" => std::fs::hard_link(&file, temp.path().join("linked")).unwrap(),
            _ => {
                let locked = s.cache.lock().await.unwrap();
                let lock = temp.path().join(".mirror/claude_metadata.lock");
                std::fs::remove_file(&lock).unwrap();
                std::fs::write(&lock, []).unwrap();
                std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600)).unwrap();
                assert!(locked.check().is_err());
                continue;
            }
        }
        assert_eq!(
            s.usage(None, None, pending()).await.unwrap_err().kind,
            ClaudeErrorKind::CredentialStorageError
        );
    }
}

fn seed_private_grant(home: &std::path::Path) {
    let directory = home.join(".mirror");
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let now = SystemTime::now();
    let stamp = |t| prost_types::Timestamp::from(t).to_string();
    let value = json!({"schema_version":1,"generation":Uuid::from_u128(3),"revision":1,"installation_id":Uuid::from_u128(4),"credential":{"access_token":"sk-ant-oat-SYNTHETIC_OWNED_ACCESS_CANARY","refresh_token":"SYNTHETIC_OWNED_REFRESH_CANARY","expires_at":stamp(now+Duration::from_secs(86400)),"authorized_at":stamp(now-Duration::from_secs(3600)),"account_id":identity().account_id,"organization_id":identity().organization_id,"email":null,"organization_name":null,"scopes":["user:inference"]}});
    for (name, bytes) in [
        ("claude_oauth.json", serde_json::to_vec(&value).unwrap()),
        ("claude_oauth.lock", Vec::new()),
    ] {
        let path = directory.join(name);
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
#[test]
#[ignore = "subprocess-only synthetic metadata helper"]
fn process_metadata_helper() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let authority = Arc::new(super::super::auth::ClaudeAuthManager::new().unwrap());
        let mut service = ClaudeMetadataService::new(authority).unwrap();
        service.transport = Arc::new(http::FixedTransport::loopback(
            std::env::var("MIRROR_TEST_METADATA_ORIGIN").unwrap(),
        ));
        let result = service.usage(None, None, pending()).await;
        match result {
            Ok(result) => println!("safe-result={}", serde_json::to_string(&result).unwrap()),
            Err(error) => println!("safe-error={}", serde_json::to_string(&error).unwrap()),
        }
    });
}
fn process(home: &std::path::Path, origin: &str, custom: bool) -> tokio::process::Child {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "providers::claude::metadata::tests::process_metadata_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("HOME", home)
        .env("MIRROR_TEST_METADATA_ORIGIN", origin)
        .env_remove("ANTHROPIC_BASE_URL")
        .env("ANTHROPIC_API_KEY", "SYNTHETIC_API_KEY_CANARY")
        .env("ANTHROPIC_OAUTH_TOKEN", "SYNTHETIC_ENV_TOKEN_CANARY")
        .env("CLAUDE_CODE_OAUTH_TOKEN", "SYNTHETIC_OTHER_TOKEN_CANARY")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if custom {
        command.env("ANTHROPIC_BASE_URL", "https://unreviewed.invalid");
    }
    command.spawn().unwrap()
}
#[tokio::test]
async fn independent_process_usage_coalesces_with_real_authority_and_rejects_custom_endpoint() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    seed_private_grant(&home);
    let (origin, server) = server(vec![Reply::json(usage_value())]).await;
    let first = process(&home, &origin, false);
    let second = process(&home, &origin, false);
    let (a, b) = tokio::join!(first.wait_with_output(), second.wait_with_output());
    let mut states = Vec::new();
    for output in [a.unwrap(), b.unwrap()] {
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(!output.contains("CANARY"));
        let line = output
            .lines()
            .find_map(|line| line.split_once("safe-result=").map(|(_, result)| result))
            .unwrap_or_else(|| {
                panic!("normalized result missing from safe helper output: {output}")
            });
        let result: Value = serde_json::from_str(line).unwrap();
        states.push(result["state"].as_str().unwrap().to_owned());
    }
    states.sort();
    assert_eq!(states, ["cached", "fresh"]);
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("Bearer sk-ant-oat-SYNTHETIC_OWNED_ACCESS_CANARY"));
    assert!(!requests[0].contains("ENV_TOKEN"));
    assert!(!requests[0].contains("API_KEY"));
    let before = std::fs::read(home.join(".mirror/claude_oauth.json")).unwrap();
    let output = process(&home, &origin, true)
        .wait_with_output()
        .await
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("endpoint_not_allowed"));
    assert!(!text.contains("CANARY"));
    assert_eq!(
        before,
        std::fs::read(home.join(".mirror/claude_oauth.json")).unwrap()
    );
}

#[tokio::test]
async fn usage_cache_expires_at_thirty_seconds_and_recovers_after_cooldown() {
    let auth = Authority::new();
    let script = Script::new(vec![
        Ok(usage_value()),
        Err(endpoint(429)),
        Ok(json!({"limits":[],"extra_usage":null})),
    ]);
    let (_temp, s) = service(auth.clone(), script);
    let first = s.usage(None, None, pending()).await.unwrap();
    let cached = s.usage(None, None, pending()).await.unwrap();
    assert_eq!(first.info.fetched_at, cached.info.fetched_at);
    assert_eq!(cached.info.state, MetadataState::Cached);
    assert_eq!(auth.calls(), 1);
    age_cache(&s, 31).await;
    let stale = s.usage(None, None, pending()).await.unwrap();
    assert_eq!(stale.info.state, MetadataState::Stale);
    assert!(stale.info.retry_at.unwrap() > SystemTime::now());
    assert_eq!(auth.calls(), 2);
    clear_cooldown(&s).await;
    let fresh = s.usage(None, None, pending()).await.unwrap();
    assert_eq!(fresh.info.state, MetadataState::Fresh);
    assert!(fresh.info.error.is_none());
    assert!(fresh.windows.is_empty());
    assert_eq!(fresh.extra_usage.state, ClaudeExtraUsage::Unknown);
    assert_eq!(auth.calls(), 3);
}
#[tokio::test]
async fn context_detail_mismatch_and_outage_remain_visible_without_substitution() {
    let auth = Authority::new();
    let script = Script::new(vec![
        Ok(json!({"id":"other","max_input_tokens":1000000})),
        Err(temporary()),
    ]);
    let (_temp, s) = service(auth, script);
    let mismatch = s
        .model_context(&identity(), "unknown", None, None, pending())
        .await
        .unwrap();
    assert_eq!(mismatch.model, "unknown");
    assert_eq!(mismatch.context_window, 200000);
    assert_eq!(mismatch.provenance, ContextProvenance::ConservativeFallback);
    assert_eq!(mismatch.info.state, MetadataState::Offline);
    assert_eq!(
        mismatch.info.error.unwrap().kind,
        ClaudeErrorKind::CompatibilityError
    );
    let known = s
        .model_context(&identity(), "claude-sonnet-4-6", None, None, pending())
        .await
        .unwrap();
    assert_eq!(known.provenance, ContextProvenance::KnownModelFallback);
    assert_eq!(known.info.state, MetadataState::Offline);
}

#[tokio::test(start_paused = true)]
async fn overall_budget_is_shared_across_pages_and_refresh_recovery() {
    struct Slow {
        calls: AtomicUsize,
        reject: bool,
    }
    #[async_trait]
    impl Transport for Slow {
        async fn get(
            &self,
            _: ClaudeAccessLease,
            _: Query<'_>,
            _: &mut usize,
        ) -> Result<Value, ClaudeError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(6)).await;
            if self.reject {
                Err(endpoint(401))
            } else if call == 0 {
                Ok(json!({"data":[{"id":"a"}],"has_more":true,"last_id":"a"}))
            } else {
                Ok(page(json!([{"id":"b"}])))
            }
        }
    }
    for reject in [false, true] {
        let auth = Authority::new();
        let transport = Arc::new(Slow {
            calls: AtomicUsize::new(0),
            reject,
        });
        let (temp, s) = service(auth.clone(), transport);
        let start = tokio::time::Instant::now();
        let result = s.models(None, None, pending()).await.unwrap();
        assert_eq!(result.info.state, MetadataState::Offline);
        assert_eq!(
            result.info.error.unwrap().kind,
            ClaudeErrorKind::AuthTemporarilyUnavailable
        );
        assert_eq!(
            tokio::time::Instant::now() - start,
            BUDGET - FINAL_VALIDATION_RESERVE
        );
        assert_eq!(auth.calls(), 2);
        assert!(temp.path().join(".mirror/claude_metadata.json").exists());
    }
}
#[tokio::test]
async fn stale_selected_catalog_context_is_used_but_detail_auth_failure_blocks_catalog_fallback() {
    let script = Script::new(vec![
        Ok(page(json!([{"id":"selected","max_input_tokens":123456}]))),
        Err(temporary()),
        Err(endpoint(403)),
        Err(temporary()),
    ]);
    let (_temp, s) = service(Authority::new(), script);
    s.models(None, None, pending()).await.unwrap();
    age_cache(&s, 600).await;
    let context = s
        .model_context(&identity(), "selected", None, None, pending())
        .await
        .unwrap();
    assert_eq!(context.context_window, 123456);
    assert_eq!(context.provenance, ContextProvenance::ModelMetadata);
    assert_eq!(context.info.state, MetadataState::Stale);
    assert!(context.info.age_seconds.unwrap() >= 600);
    assert_eq!(
        s.model_context(&identity(), "other", None, None, pending())
            .await
            .unwrap_err()
            .kind,
        ClaudeErrorKind::AccessDenied
    );
    assert!(s.models(None, None, pending()).await.is_err());
}
#[test]
fn unknown_monetary_units_and_invalid_scales_remain_unknown() {
    let lowercase =
        usage::parse(json!({"spend":{"used":{"amount_minor":25,"currency":"usd","exponent":2}}}))
            .unwrap();
    assert_eq!(
        lowercase.extra_usage.used.unwrap().currency.as_deref(),
        Some("USD")
    );

    let value=usage::parse(json!({"extra_usage":{"is_enabled":true,"unit":"unknown_currency","used_credits":250,"monthly_limit":1000}})).unwrap();
    assert!(value.extra_usage.used.is_none());
    assert!(value.extra_usage.limit.is_none());
    let value=usage::parse(json!({"spend":{"enabled":false,"used":{"amount_minor":250,"currency":"invalid","exponent":99},"limit":null}})).unwrap();
    assert_eq!(value.extra_usage.state, ClaudeExtraUsage::Disabled);
    let used = value.extra_usage.used.unwrap();
    assert_eq!(used.value, 250.0);
    assert_eq!(used.currency, None);
    assert_eq!(used.decimal_places, None);
}

#[tokio::test]
async fn final_snapshot_wait_cannot_hide_a_revoked_request_guard() {
    struct Guard(Arc<AtomicBool>);
    impl ModelRequestGuard for Guard {
        fn verify(&self) -> BoxFuture<'_, Result<(), crate::providers::LlmError>> {
            async {
                if self.0.load(Ordering::SeqCst) {
                    Err(crate::providers::LlmError::Claude(cancelled()))
                } else {
                    Ok(())
                }
            }
            .boxed()
        }
    }
    struct SnapshotWait {
        inner: Arc<Authority>,
        calls: AtomicUsize,
        revoke_on: usize,
        revoked: Arc<AtomicBool>,
    }
    impl SnapshotWait {
        async fn wait(&self) {
            if self.calls.fetch_add(1, Ordering::SeqCst) + 1 == self.revoke_on {
                // A credential-lock wait can outlive the request guard.
                tokio::task::yield_now().await;
                self.revoked.store(true, Ordering::SeqCst);
            }
        }
    }
    impl ClaudeAuthorization for SnapshotWait {
        fn authorize<'a>(
            &'a self,
            context: ClaudeAuthorizationContext<'a>,
            rejected: Option<ClaudeTokenVersion>,
        ) -> BoxFuture<'a, Result<ClaudeAccessLease, ClaudeError>> {
            self.inner.authorize(context, rejected)
        }
        fn local_snapshot<'a>(
            &'a self,
            cancellation: BoxFuture<'a, ()>,
        ) -> BoxFuture<'a, Result<ClaudeLocalAuthSnapshot, ClaudeError>> {
            async move {
                self.wait().await;
                self.inner.local_snapshot(cancellation).await
            }
            .boxed()
        }
        fn local_snapshot_guarded<'a>(
            &'a self,
            guard: &'a dyn ModelRequestGuard,
            cancellation: BoxFuture<'a, ()>,
        ) -> BoxFuture<'a, Result<ClaudeLocalAuthSnapshot, ClaudeError>> {
            async move {
                self.wait().await;
                self.inner.local_snapshot_guarded(guard, cancellation).await
            }
            .boxed()
        }
    }
    for action in ["models", "context", "usage", "explicit"] {
        let auth = Authority::new();
        let (_temp, mut service) = service(
            auth.clone(),
            Script::new(vec![
                Ok(page(json!([{"id":"selected","max_input_tokens":123456}]))),
                Ok(usage_value()),
            ]),
        );
        service.models(None, None, pending()).await.unwrap();
        service.usage(None, None, pending()).await.unwrap();
        let revoked = Arc::new(AtomicBool::new(false));
        let guard = Guard(revoked.clone());
        service.authority = Arc::new(SnapshotWait {
            inner: auth.clone(),
            calls: AtomicUsize::new(0),
            revoke_on: if action == "explicit" { 2 } else { 3 },
            revoked,
        });
        let result = match action {
            "models" => service
                .models(None, Some(&guard), pending())
                .await
                .map(|_| ()),
            "usage" => service
                .usage(None, Some(&guard), pending())
                .await
                .map(|_| ()),
            _ => service
                .model_context(
                    &identity(),
                    "selected",
                    (action == "explicit").then_some(123456),
                    Some(&guard),
                    pending(),
                )
                .await
                .map(|_| ()),
        };
        assert_eq!(
            result.unwrap_err().kind,
            ClaudeErrorKind::RequestCancelled,
            "{action}"
        );
        assert_eq!(auth.calls(), 2, "cached/explicit lookup must remain local");
    }
}

struct Stalled;
#[async_trait]
impl Transport for Stalled {
    async fn get(
        &self,
        _: ClaudeAccessLease,
        _: Query<'_>,
        _: &mut usize,
    ) -> Result<Value, ClaudeError> {
        std::future::pending().await
    }
}
#[tokio::test(start_paused = true)]
async fn stalled_http_reserves_final_validation_for_stale_or_offline_results() {
    for cached in [false, true] {
        for action in ["models", "context", "usage"] {
            let auth = Authority::new();
            let script = Script::new(vec![
                Ok(page(json!([{"id":"selected","max_input_tokens":123456}]))),
                Ok(usage_value()),
            ]);
            let (_temp, mut s) = service(auth.clone(), script);
            if cached {
                s.models(None, None, pending()).await.unwrap();
                s.usage(None, None, pending()).await.unwrap();
                age_cache(&s, 600).await;
            }
            let before = {
                let key = s.key(None, &pending().shared()).await.unwrap();
                let locked = s.cache.lock().await.unwrap();
                locked.load(key).unwrap()
            };
            s.transport = Arc::new(Stalled);
            let start = tokio::time::Instant::now();
            let info = match action {
                "models" => {
                    let result = s.models(None, None, pending()).await.unwrap();
                    assert_eq!(
                        result.source,
                        if cached {
                            CatalogSource::Remote
                        } else {
                            CatalogSource::Bundled
                        }
                    );
                    result.info
                }
                "context" => {
                    let result = s
                        .model_context(&identity(), "selected", None, None, pending())
                        .await
                        .unwrap();
                    assert_eq!(result.context_window, if cached { 123456 } else { 200000 });
                    assert_eq!(
                        result.provenance,
                        if cached {
                            ContextProvenance::ModelMetadata
                        } else {
                            ContextProvenance::ConservativeFallback
                        }
                    );
                    result.info
                }
                _ => s.usage(None, None, pending()).await.unwrap().info,
            };
            assert_eq!(
                tokio::time::Instant::now() - start,
                BUDGET - FINAL_VALIDATION_RESERVE
            );
            assert_eq!(
                info.state,
                if cached {
                    MetadataState::Stale
                } else if action == "usage" {
                    MetadataState::Unavailable
                } else {
                    MetadataState::Offline
                }
            );
            if cached {
                let original = if action == "usage" {
                    before.usage.unwrap().fetched_at
                } else {
                    before.catalog.unwrap().fetched_at
                };
                assert_eq!(info.fetched_at, original);
                assert!(info.age_seconds.unwrap() >= 600);
            } else {
                assert!(info.fetched_at.is_none());
                assert!(info.age_seconds.is_none());
            }
            assert_eq!(
                info.error.unwrap().kind,
                ClaudeErrorKind::AuthTemporarilyUnavailable
            );
        }
    }
}
#[tokio::test(start_paused = true)]
async fn reserved_interval_auth_guard_and_cancellation_fail_closed() {
    struct Guard {
        calls: AtomicUsize,
        mode: &'static str,
        auth: Arc<Authority>,
        send: tokio::sync::watch::Sender<bool>,
    }
    impl ModelRequestGuard for Guard {
        fn verify(&self) -> BoxFuture<'_, Result<(), crate::providers::LlmError>> {
            async {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 2 {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    match self.mode {
                        "guard" => return Err(crate::providers::LlmError::Claude(cancelled())),
                        "cancel" => {
                            self.send.send_replace(true);
                        }
                        "revoked" => {
                            self.auth.state.lock().unwrap().state = ClaudeAuthState::LoginRequired
                        }
                        "guard_timeout" => std::future::pending::<()>().await,
                        _ => unreachable!(),
                    }
                }
                Ok(())
            }
            .boxed()
        }
    }
    for mode in ["guard", "cancel", "revoked", "guard_timeout"] {
        let auth = Authority::new();
        let (temp, s) = service(auth.clone(), Arc::new(Stalled));
        let (send, mut recv) = tokio::sync::watch::channel(false);
        let guard = Guard {
            calls: AtomicUsize::new(0),
            mode,
            auth,
            send,
        };
        let cancellation = async move {
            while !*recv.borrow() {
                recv.changed().await.unwrap();
            }
        }
        .boxed();
        let start = tokio::time::Instant::now();
        let error = s
            .models(None, Some(&guard), cancellation)
            .await
            .unwrap_err();
        assert_eq!(
            error.kind,
            match mode {
                "revoked" => ClaudeErrorKind::LoginRequired,
                "guard_timeout" => ClaudeErrorKind::AuthTemporarilyUnavailable,
                _ => ClaudeErrorKind::RequestCancelled,
            }
        );
        assert!(tokio::time::Instant::now() - start <= BUDGET);
        assert!(!temp.path().join(".mirror/claude_metadata.json").exists());
    }
}

#[tokio::test]
async fn retry_after_is_utc_representable_and_invalid_hints_keep_the_cooldown() {
    for hint in ["18446744073709551615", "999999999999", "garbled"] {
        let (origin, server) = server(vec![Reply {
            status: 429,
            headers: format!("Retry-After: {hint}\r\n"),
            body: String::new(),
        }])
        .await;
        let auth = Authority::new();
        let (_temp, s) = service(
            auth.clone(),
            Arc::new(http::FixedTransport::loopback(origin)),
        );
        let start = SystemTime::now();
        let result = s.usage(None, None, pending()).await.unwrap();
        let retry = result.info.retry_at.unwrap();
        assert!(retry >= start + Duration::from_secs(60));
        assert!(retry <= SystemTime::now() + Duration::from_secs(60));
        assert_eq!(result.info.state, MetadataState::Unavailable);
        let serialized = serde_json::to_value(&result).unwrap();
        assert!(serialized["retry_at"].as_str().unwrap().ends_with('Z'));
        let second = s.usage(None, None, pending()).await.unwrap();
        assert_eq!(second.info.retry_at, Some(retry));
        assert_eq!(auth.calls(), 1);
        server.await.unwrap();
    }
    for hint in ["120", "Fri, 31 Dec 9999 23:59:59 GMT"] {
        let (origin, server) = server(vec![Reply {
            status: 429,
            headers: format!("Retry-After: {hint}\r\n"),
            body: String::new(),
        }])
        .await;
        let (_temp, s) = service(
            Authority::new(),
            Arc::new(http::FixedTransport::loopback(origin)),
        );
        let before = SystemTime::now();
        let result = s.usage(None, None, pending()).await.unwrap();
        let retry = result.info.retry_at.unwrap();
        if hint == "120" {
            assert!(retry >= before + Duration::from_secs(120));
        } else {
            assert_eq!(
                prost_types::Timestamp::from(retry).to_string(),
                "9999-12-31T23:59:59Z"
            );
        }
        server.await.unwrap();
    }
}
