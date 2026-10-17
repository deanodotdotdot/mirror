use super::*;
use async_trait::async_trait;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{atomic::AtomicBool, Mutex};
use tempfile::TempDir;
use tokio::sync::{mpsc, Notify};

struct Mock {
    calls: AtomicUsize,
}
#[async_trait]
impl OAuthTransport for Mock {
    async fn token(&self, _request: TokenRequest<'_>) -> Result<Value, ClaudeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(30)).await;
        Ok(token())
    }
    async fn bootstrap(&self, _access: &Secret) -> Result<Value, ClaudeError> {
        panic!("unexpected bootstrap")
    }
}
fn identity() -> ClaudeIdentityBinding {
    ClaudeIdentityBinding {
        account_id: Uuid::from_u128(1),
        organization_id: Uuid::from_u128(2),
    }
}
fn token() -> Value {
    serde_json::json!({"access_token":"sk-ant-oat-SYNTHETIC_ACCESS","refresh_token":"SYNTHETIC_REFRESH","token_type":"Bearer","expires_in":3600,"scope":"user:inference","account":{"uuid":identity().account_id},"organization":{"uuid":identity().organization_id}})
}
fn manager(path: &std::path::Path, transport: Arc<dyn OAuthTransport>) -> ClaudeAuthManager {
    ClaudeAuthManager {
        store: Store::new(
            path.parent()
                .unwrap()
                .canonicalize()
                .unwrap()
                .join(path.file_name().unwrap()),
        )
        .unwrap(),
        transport,
    }
}
async fn seed(manager: &ClaudeAuthManager, near: bool) -> ClaudeTokenVersion {
    let locked = manager.store.lock(true).await.unwrap().unwrap();
    let mut record = Record::tombstone(None);
    record.revision = 1;
    let mut data = token();
    if near {
        data["expires_in"] = serde_json::json!(30);
    }
    record.credential = Some(Credential::from_token(&data, None, SystemTime::now()).unwrap());
    locked.write(&record).unwrap();
    record.version()
}
fn context(expected: &ClaudeIdentityBinding) -> ClaudeAuthorizationContext<'_> {
    ClaudeAuthorizationContext {
        operation: ClaudeOperation::Messages,
        expected_identity: expected,
        request_guard: None,
        cancellation: Box::pin(std::future::pending()),
    }
}
#[tokio::test]
async fn refresh_race_converges_and_logout_fences_old_lease() {
    let temp = TempDir::new().unwrap();
    let mock = Arc::new(Mock {
        calls: AtomicUsize::new(0),
    });
    let first = manager(&temp.path().join("auth"), mock.clone());
    let second = manager(&temp.path().join("auth"), mock.clone());
    let original = seed(&first, true).await;
    let expected = identity();
    let (a, b) = tokio::join!(
        first.authorize(context(&expected), None),
        second.authorize(context(&expected), None)
    );
    assert_eq!(a.unwrap().version.revision, 2);
    assert_eq!(b.unwrap().version.revision, 2);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    let current = first
        .local_snapshot(Box::pin(std::future::pending()))
        .await
        .unwrap()
        .version
        .unwrap();
    assert!(first
        .authorize(context(&expected), Some(original))
        .await
        .is_ok());
    assert_eq!(
        first
            .authorize(context(&expected), Some(current))
            .await
            .unwrap_err()
            .kind,
        ClaudeErrorKind::AuthTemporarilyUnavailable
    );
    first
        .logout(Box::pin(std::future::pending()))
        .await
        .unwrap();
    assert_eq!(
        first
            .authorize(context(&expected), Some(original))
            .await
            .unwrap_err()
            .kind,
        ClaudeErrorKind::LoginRequired
    );
    let snapshot = first
        .local_snapshot(Box::pin(std::future::pending()))
        .await
        .unwrap();
    assert_eq!(snapshot.status.state, ClaudeAuthState::LoggedOut);
    assert_ne!(snapshot.version.unwrap().generation, original.generation);
}
#[tokio::test]
async fn snapshot_is_read_only_and_atomic_failures_are_precise() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("auth");
    let first = manager(
        &path,
        Arc::new(Mock {
            calls: AtomicUsize::new(0),
        }),
    );
    assert!(first
        .local_snapshot(Box::pin(std::future::pending()))
        .await
        .unwrap()
        .version
        .is_none());
    assert!(!path.exists());
    let original = seed(&first, false).await;
    let locked = first.store.lock(false).await.unwrap().unwrap();
    let mut record = locked.load().unwrap().unwrap();
    record.revision += 1;
    assert_eq!(
        locked
            .fault_write(&record, false)
            .unwrap_err()
            .commit_outcome,
        Some(CommitOutcome::NotCommitted)
    );
    assert_eq!(locked.load().unwrap().unwrap().version(), original);
    assert_eq!(
        locked
            .fault_write(&record, true)
            .unwrap_err()
            .commit_outcome,
        Some(CommitOutcome::Unknown)
    );
    assert_eq!(locked.load().unwrap().unwrap().revision, 2);
}

struct Script {
    reply: Mutex<Option<Result<Value, ClaudeError>>>,
    calls: AtomicUsize,
    bootstrap: Option<Value>,
}
#[async_trait]
impl OAuthTransport for Script {
    async fn token(&self, request: TokenRequest<'_>) -> Result<Value, ClaudeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match request {
            TokenRequest::Exchange {
                code,
                state,
                verifier,
                redirect,
            } => {
                assert_eq!(code.expose(), "SYNTHETIC_CODE");
                assert!(state.expose().len() >= 32 && verifier.expose().len() >= 43);
                assert!(redirect.starts_with("http://localhost:"));
            }
            TokenRequest::Refresh(refresh) => assert_eq!(refresh.expose(), "SYNTHETIC_REFRESH"),
        }
        self.reply.lock().unwrap().take().expect("one exchange")
    }
    async fn bootstrap(&self, access: &Secret) -> Result<Value, ClaudeError> {
        assert!(access.expose().starts_with("sk-ant-oat-"));
        Ok(self.bootstrap.clone().expect("expected bootstrap"))
    }
}
fn script(reply: Result<Value, ClaudeError>) -> Arc<Script> {
    Arc::new(Script {
        reply: Mutex::new(Some(reply)),
        calls: AtomicUsize::new(0),
        bootstrap: None,
    })
}

#[tokio::test]
async fn malformed_refreshes_and_uncertain_network_keep_exact_record() {
    let temp = TempDir::new().unwrap();
    let mut malformed = token();
    malformed["refresh_token"] = serde_json::json!("");
    let mut wrong_kind = token();
    wrong_kind["access_token"] = serde_json::json!("sk-ant-api01-SYNTHETIC");
    let mut changed = token();
    changed["organization"]["uuid"] = serde_json::json!(Uuid::new_v4());
    let mut upstream = temporary_error();
    upstream.details.provider_code = Some(ClaudeProviderCode::InvalidGrant);
    upstream.details.http_status = Some(500);
    for (index, reply) in [
        Ok(malformed),
        Ok(wrong_kind),
        Ok(changed),
        Err(temporary_error()),
        Err(upstream),
    ]
    .into_iter()
    .enumerate()
    {
        let auth = manager(&temp.path().join(format!("auth{index}")), script(reply));
        seed(&auth, true).await;
        let before = {
            let locked = auth.store.lock(false).await.unwrap().unwrap();
            serde_json::to_vec(&locked.load().unwrap().unwrap()).unwrap()
        };
        assert!(auth.authorize(context(&identity()), None).await.is_err());
        let locked = auth.store.lock(false).await.unwrap().unwrap();
        assert_eq!(
            serde_json::to_vec(&locked.load().unwrap().unwrap()).unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn revoked_refresh_is_persisted_but_missing_replacement_preserves_refresh_and_login_time() {
    let temp = TempDir::new().unwrap();
    let mut revoked = ClaudeError::new(ClaudeErrorKind::LoginRequired);
    revoked.details.http_status = Some(400);
    revoked.details.provider_code = Some(ClaudeProviderCode::InvalidGrant);
    let auth = manager(&temp.path().join("revoked"), script(Err(revoked)));
    let version = seed(&auth, true).await;
    assert_eq!(
        auth.authorize(context(&identity()), None)
            .await
            .unwrap_err()
            .kind,
        ClaudeErrorKind::LoginRequired
    );
    let other = manager(&temp.path().join("revoked"), script(Ok(token())));
    let snapshot = other
        .local_snapshot(Box::pin(std::future::pending()))
        .await
        .unwrap();
    assert_eq!(snapshot.status.state, ClaudeAuthState::LoginRequired);
    assert_eq!(snapshot.version, Some(version));
    assert_eq!(
        other
            .authorize(context(&identity()), None)
            .await
            .unwrap_err()
            .kind,
        ClaudeErrorKind::LoginRequired
    );
    let locked = other.store.lock(false).await.unwrap().unwrap();
    let retained = locked.load().unwrap().unwrap();
    assert!(retained.reauth_required.is_some());
    assert_eq!(
        retained.credential.unwrap().refresh_token.expose(),
        "SYNTHETIC_REFRESH"
    );
    drop(locked);
    let mut replacement = token();
    replacement.as_object_mut().unwrap().remove("refresh_token");
    let auth = manager(&temp.path().join("renewed"), script(Ok(replacement)));
    seed(&auth, true).await;
    let original_time = {
        let locked = auth.store.lock(false).await.unwrap().unwrap();
        locked
            .load()
            .unwrap()
            .unwrap()
            .credential
            .unwrap()
            .authorized_at
    };
    auth.authorize(context(&identity()), None).await.unwrap();
    let locked = auth.store.lock(false).await.unwrap().unwrap();
    let renewed = locked.load().unwrap().unwrap();
    assert!(renewed.last_refreshed_at.is_some());
    let credential = renewed.credential.unwrap();
    assert_eq!(credential.authorized_at, original_time);
    assert_eq!(credential.refresh_token.expose(), "SYNTHETIC_REFRESH");
}

struct Blocking {
    started: Notify,
    dropped: AtomicBool,
}
struct DropFlag<'a>(&'a AtomicBool);
impl Drop for DropFlag<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl OAuthTransport for Blocking {
    async fn token(&self, _request: TokenRequest<'_>) -> Result<Value, ClaudeError> {
        let _flag = DropFlag(&self.dropped);
        self.started.notify_one();
        std::future::pending().await
    }
    async fn bootstrap(&self, _: &Secret) -> Result<Value, ClaudeError> {
        panic!("unexpected bootstrap")
    }
}
#[tokio::test]
async fn cancellation_and_dropped_future_abort_refresh_and_release_lock() {
    for explicit in [true, false] {
        let temp = TempDir::new().unwrap();
        let blocking = Arc::new(Blocking {
            started: Notify::new(),
            dropped: AtomicBool::new(false),
        });
        let auth = manager(&temp.path().join("auth"), blocking.clone());
        let version = seed(&auth, true).await;
        let expected = identity();
        if explicit {
            let mut ctx = context(&expected);
            ctx.cancellation = Box::pin(blocking.started.notified());
            assert_eq!(
                auth.authorize(ctx, None).await.unwrap_err().kind,
                ClaudeErrorKind::RequestCancelled
            );
        } else {
            let mut future = auth.authorize(context(&expected), None);
            tokio::select! { _ = &mut future => panic!("refresh must remain pending"), _ = blocking.started.notified() => () }
            drop(future);
        }
        assert!(blocking.dropped.load(Ordering::SeqCst));
        let after = tokio::time::timeout(
            Duration::from_secs(1),
            auth.local_snapshot(Box::pin(std::future::pending())),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(after.version, Some(version));
    }
}

#[tokio::test(start_paused = true)]
async fn lock_wait_has_cancellation_and_thirty_second_deadline() {
    let temp = TempDir::new().unwrap();
    let auth = manager(&temp.path().join("auth"), script(Ok(token())));
    seed(&auth, false).await;
    let _held = auth.store.lock(false).await.unwrap().unwrap();
    let error = auth
        .local_snapshot(Box::pin(tokio::time::sleep(Duration::from_secs(1))))
        .await
        .unwrap_err();
    assert_eq!(error.kind, ClaudeErrorKind::RequestCancelled);
    let start = tokio::time::Instant::now();
    assert_eq!(
        auth.local_snapshot(Box::pin(std::future::pending()))
            .await
            .unwrap_err()
            .kind,
        ClaudeErrorKind::AuthTemporarilyUnavailable
    );
    assert_eq!(tokio::time::Instant::now() - start, Duration::from_secs(30));
}

#[tokio::test]
async fn token_exchange_has_whole_ten_second_timeout() {
    let temp = TempDir::new().unwrap();
    let blocking = Arc::new(Blocking {
        started: Notify::new(),
        dropped: AtomicBool::new(false),
    });
    let auth = manager(&temp.path().join("auth"), blocking.clone());
    let version = seed(&auth, true).await;
    let start = tokio::time::Instant::now();
    assert_eq!(
        auth.authorize(context(&identity()), None)
            .await
            .unwrap_err()
            .kind,
        ClaudeErrorKind::AuthTemporarilyUnavailable
    );
    assert!(tokio::time::Instant::now() - start >= Duration::from_secs(10));
    assert!(tokio::time::Instant::now() - start < Duration::from_secs(15));
    assert!(blocking.dropped.load(Ordering::SeqCst));
    assert_eq!(
        auth.local_snapshot(Box::pin(std::future::pending()))
            .await
            .unwrap()
            .version,
        Some(version)
    );
}

#[tokio::test]
async fn wrong_identity_and_guard_reject_before_http_and_final_guard_rechecks() {
    struct Guard(AtomicUsize);
    impl crate::providers::ModelRequestGuard for Guard {
        fn verify(&self) -> BoxFuture<'_, Result<(), crate::providers::LlmError>> {
            Box::pin(async {
                if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(())
                } else {
                    Err(ClaudeError::new(ClaudeErrorKind::RequestCancelled).into())
                }
            })
        }
    }
    let temp = TempDir::new().unwrap();
    let mock = Arc::new(Mock {
        calls: AtomicUsize::new(0),
    });
    let auth = manager(&temp.path().join("auth"), mock.clone());
    seed(&auth, false).await;
    let mut wrong = identity();
    wrong.organization_id = Uuid::new_v4();
    let error = auth.authorize(context(&wrong), None).await.unwrap_err();
    assert_eq!(error.kind, ClaudeErrorKind::AccountChanged);
    assert_eq!(error.details.current_identity, Some(identity()));
    let expected = identity();
    let guard = Guard(AtomicUsize::new(0));
    let mut ctx = context(&expected);
    ctx.request_guard = Some(&guard);
    assert_eq!(
        auth.authorize(ctx, None).await.unwrap_err().kind,
        ClaudeErrorKind::RequestCancelled
    );
    assert_eq!(guard.0.load(Ordering::SeqCst), 2);
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_during_final_guard_never_publishes_a_lease() {
    struct Guard {
        calls: AtomicUsize,
        cancel: Notify,
    }
    impl crate::providers::ModelRequestGuard for Guard {
        fn verify(&self) -> BoxFuture<'_, Result<(), crate::providers::LlmError>> {
            Box::pin(async {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                    // The renewal has committed before this final guard runs.
                    // This future is ready immediately, so the authorization
                    // driver's initial cancellation poll cannot observe it.
                    self.cancel.notify_one();
                }
                Ok(())
            })
        }
    }
    for near_expiry in [false, true] {
        let temp = TempDir::new().unwrap();
        let mock = Arc::new(Mock {
            calls: AtomicUsize::new(0),
        });
        let auth = manager(&temp.path().join("auth"), mock.clone());
        let original = seed(&auth, near_expiry).await;
        let expected = identity();
        let guard = Guard {
            calls: AtomicUsize::new(0),
            cancel: Notify::new(),
        };
        let mut ctx = context(&expected);
        ctx.request_guard = Some(&guard);
        ctx.cancellation = Box::pin(guard.cancel.notified());
        assert_eq!(
            auth.authorize(ctx, None).await.unwrap_err().kind,
            ClaudeErrorKind::RequestCancelled
        );
        let persisted = auth
            .local_snapshot(Box::pin(std::future::pending()))
            .await
            .unwrap()
            .version
            .unwrap();
        assert_eq!(persisted.generation, original.generation);
        assert_eq!(persisted.revision, if near_expiry { 2 } else { 1 });
        // Cancellation never rolls back a rotation already committed, and a
        // later request reloads it without performing another refresh.
        assert_eq!(
            auth.authorize(context(&expected), None)
                .await
                .unwrap()
                .version,
            persisted
        );
        assert_eq!(mock.calls.load(Ordering::SeqCst), usize::from(near_expiry));
    }
}

#[tokio::test]
async fn final_guard_wait_rechecks_a_replaced_stable_lock() {
    struct Guard {
        calls: AtomicUsize,
        entered: Notify,
        resume: Notify,
    }
    impl crate::providers::ModelRequestGuard for Guard {
        fn verify(&self) -> BoxFuture<'_, Result<(), crate::providers::LlmError>> {
            Box::pin(async {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                    self.entered.notify_one();
                    self.resume.notified().await;
                }
                Ok(())
            })
        }
    }
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("auth");
    let auth = manager(&path, script(Ok(token())));
    seed(&auth, false).await;
    let expected = identity();
    let guard = Guard {
        calls: AtomicUsize::new(0),
        entered: Notify::new(),
        resume: Notify::new(),
    };
    let mut ctx = context(&expected);
    ctx.request_guard = Some(&guard);
    let replace_and_logout = async {
        guard.entered.notified().await;
        // Simulate external replacement while authorize is suspended in its
        // final guard. Logout now orders against a different lock inode.
        let replacement = path.join("replacement.lock");
        std::fs::write(&replacement, b"").unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::rename(replacement, path.join("claude_oauth.lock")).unwrap();
        auth.logout(Box::pin(std::future::pending())).await.unwrap();
        guard.resume.notify_one();
    };
    let (result, ()) = tokio::join!(auth.authorize(ctx, None), replace_and_logout);
    assert_eq!(
        result.unwrap_err().kind,
        ClaudeErrorKind::CredentialStorageError
    );
    assert_eq!(
        auth.authorize(context(&expected), None)
            .await
            .unwrap_err()
            .kind,
        ClaudeErrorKind::LoginRequired
    );
}

#[tokio::test]
async fn guarded_snapshot_checks_guard_after_lock_wait_without_renewal() {
    struct Guard {
        revoked: AtomicBool,
        calls: AtomicUsize,
    }
    impl crate::providers::ModelRequestGuard for Guard {
        fn verify(&self) -> BoxFuture<'_, Result<(), crate::providers::LlmError>> {
            Box::pin(async {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if self.revoked.load(Ordering::SeqCst) {
                    Err(cancelled().into())
                } else {
                    Ok(())
                }
            })
        }
    }
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("auth");
    let mock = Arc::new(Mock {
        calls: AtomicUsize::new(0),
    });
    let auth = manager(&path, mock.clone());
    let version = seed(&auth, true).await;
    let before = std::fs::read(path.join("claude_oauth.json")).unwrap();
    let guard = Guard {
        revoked: AtomicBool::new(false),
        calls: AtomicUsize::new(0),
    };
    let locked = auth.store.lock(false).await.unwrap().unwrap();
    let operation = auth.local_snapshot_guarded(&guard, Box::pin(std::future::pending()));
    tokio::pin!(operation);
    assert!(
        tokio::time::timeout(Duration::from_millis(40), &mut operation)
            .await
            .is_err()
    );
    assert_eq!(
        guard.calls.load(Ordering::SeqCst),
        0,
        "guard must follow lock wait"
    );
    guard.revoked.store(true, Ordering::SeqCst);
    drop(locked);
    assert_eq!(
        operation.await.unwrap_err().kind,
        ClaudeErrorKind::RequestCancelled
    );
    guard.revoked.store(false, Ordering::SeqCst);
    let snapshot = auth
        .local_snapshot_guarded(&guard, Box::pin(std::future::pending()))
        .await
        .unwrap();
    assert_eq!(snapshot.status.state, ClaudeAuthState::Refreshable);
    assert_eq!(snapshot.version, Some(version));
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        before,
        std::fs::read(path.join("claude_oauth.json")).unwrap()
    );
}

#[tokio::test]
async fn guarded_snapshot_rechecks_lock_replacement_after_guard_wait() {
    struct Guard {
        entered: Notify,
        resume: Notify,
    }
    impl crate::providers::ModelRequestGuard for Guard {
        fn verify(&self) -> BoxFuture<'_, Result<(), crate::providers::LlmError>> {
            Box::pin(async {
                self.entered.notify_one();
                self.resume.notified().await;
                Ok(())
            })
        }
    }
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("auth");
    let auth = manager(&path, script(Ok(token())));
    seed(&auth, false).await;
    let guard = Guard {
        entered: Notify::new(),
        resume: Notify::new(),
    };
    let replace_and_logout = async {
        guard.entered.notified().await;
        let replacement = path.join("replacement.lock");
        std::fs::write(&replacement, b"").unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::rename(replacement, path.join("claude_oauth.lock")).unwrap();
        auth.logout(Box::pin(std::future::pending())).await.unwrap();
        guard.resume.notify_one();
    };
    let (result, ()) = tokio::join!(
        auth.local_snapshot_guarded(&guard, Box::pin(std::future::pending())),
        replace_and_logout
    );
    assert_eq!(
        result.unwrap_err().kind,
        ClaudeErrorKind::CredentialStorageError
    );
    assert_eq!(
        auth.local_snapshot(Box::pin(std::future::pending()))
            .await
            .unwrap()
            .status
            .state,
        ClaudeAuthState::LoggedOut
    );
}

#[tokio::test]
async fn guarded_snapshot_same_poll_cancellation_prevents_publication() {
    struct Guard(Notify);
    impl crate::providers::ModelRequestGuard for Guard {
        fn verify(&self) -> BoxFuture<'_, Result<(), crate::providers::LlmError>> {
            Box::pin(async {
                self.0.notify_one();
                Ok(())
            })
        }
    }
    let temp = TempDir::new().unwrap();
    let mock = Arc::new(Mock {
        calls: AtomicUsize::new(0),
    });
    let auth = manager(&temp.path().join("auth"), mock.clone());
    let version = seed(&auth, true).await;
    let guard = Guard(Notify::new());
    let result = auth
        .local_snapshot_guarded(&guard, Box::pin(guard.0.notified()))
        .await;
    assert_eq!(result.unwrap_err().kind, ClaudeErrorKind::RequestCancelled);
    assert_eq!(
        auth.local_snapshot(Box::pin(std::future::pending()))
            .await
            .unwrap()
            .version,
        Some(version)
    );
    assert_eq!(mock.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn permissions_symlinks_hardlinks_unknown_schema_and_marker_validation() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("auth");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let auth = manager(&path, script(Ok(token())));
    seed(&auth, false).await;
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let file = path.join(store::FILE);
    for name in [store::FILE, "claude_oauth.lock"] {
        assert_eq!(
            std::fs::metadata(path.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let original = std::fs::read(&file).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(auth.status(Box::pin(std::future::pending())).await.is_err());
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o644
    );
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(&file, path.join("hardlink")).unwrap();
    assert!(auth.status(Box::pin(std::future::pending())).await.is_err());
    std::fs::remove_file(path.join("hardlink")).unwrap();
    std::fs::remove_file(&file).unwrap();
    symlink(path.join("missing"), &file).unwrap();
    assert!(auth.logout(Box::pin(std::future::pending())).await.is_err());
    std::fs::remove_file(&file).unwrap();
    std::fs::write(&file, &original).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut future: Value = serde_json::from_slice(&original).unwrap();
    future["schema_version"] = serde_json::json!(999);
    std::fs::write(&file, serde_json::to_vec(&future).unwrap()).unwrap();
    let error = auth
        .logout(Box::pin(std::future::pending()))
        .await
        .unwrap_err();
    assert_eq!(
        error.error.details.configuration,
        Some(ClaudeConfigurationError::UnknownSchema)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&file).unwrap()).unwrap()["schema_version"],
        999
    );
    future["schema_version"] = serde_json::json!(1);
    future["reauth_required"] =
        serde_json::json!({"generation":Uuid::new_v4(),"revision":1,"reason":"invalid_grant"});
    std::fs::write(&file, serde_json::to_vec(&future).unwrap()).unwrap();
    assert_eq!(
        auth.status(Box::pin(std::future::pending()))
            .await
            .unwrap_err()
            .kind,
        ClaudeErrorKind::CredentialStorageError
    );
    let linked = root.join("linked");
    symlink(&path, &linked).unwrap();
    let linked_auth = manager(&linked, script(Ok(token())));
    assert!(linked_auth
        .status(Box::pin(std::future::pending()))
        .await
        .is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(auth.status(Box::pin(std::future::pending())).await.is_err());
}

#[tokio::test]
async fn login_bootstrap_callbacks_and_generation_fence() {
    for replace_with_logout in [false, true] {
        let temp = TempDir::new().unwrap();
        let mut data = token();
        data.as_object_mut().unwrap().remove("organization");
        let transport = Arc::new(Script {
            reply: Mutex::new(Some(Ok(data))),
            calls: AtomicUsize::new(0),
            bootstrap: Some(
                serde_json::json!({"oauth_account":{"account_uuid":identity().account_id,"organization_uuid":identity().organization_id}}),
            ),
        });
        let auth = manager(&temp.path().join("auth"), transport.clone());
        let old_version = seed(&auth, false).await;
        let (input, receiver) = mpsc::channel(4);
        let (events, mut event_rx) = mpsc::channel(32);
        let ui = async {
            while let Some(event) = event_rx.recv().await {
                match event {
                    ClaudeLoginEvent::AuthorizationUrl {
                        authorization_url, ..
                    } => {
                        let url =
                            reqwest::Url::parse(authorization_url.expose_for_login_ui()).unwrap();
                        let pairs: std::collections::HashMap<_, _> =
                            url.query_pairs().into_owned().collect();
                        let expected = URL_SAFE_NO_PAD_FOR_TEST(&pairs["code_challenge"]);
                        assert_eq!(expected.len(), 32);
                        if replace_with_logout {
                            auth.logout(Box::pin(std::future::pending())).await.unwrap();
                        }
                        input
                            .send(ClaudeCallbackInput::new("SYNTHETIC_CODE#wrong".into()).unwrap())
                            .await
                            .unwrap();
                        input
                            .send(
                                ClaudeCallbackInput::new(format!(
                                    "SYNTHETIC_CODE#{}",
                                    pairs["state"]
                                ))
                                .unwrap(),
                            )
                            .await
                            .unwrap();
                        // Replay after the first accepted code never exchanges twice.
                        let _ = input
                            .try_send(ClaudeCallbackInput::new("SYNTHETIC_CODE".into()).unwrap());
                    }
                    _ => (),
                }
            }
        };
        let (result, _) = tokio::join!(
            auth.login(receiver, events, Box::pin(std::future::pending())),
            ui
        );
        if replace_with_logout {
            assert_eq!(
                result.unwrap_err().error.kind,
                ClaudeErrorKind::LoginCancelled
            );
            assert_eq!(
                auth.status(Box::pin(std::future::pending()))
                    .await
                    .unwrap()
                    .state,
                ClaudeAuthState::LoggedOut
            );
        } else {
            assert_eq!(result.unwrap().identity, Some(identity()));
            let current = auth
                .local_snapshot(Box::pin(std::future::pending()))
                .await
                .unwrap()
                .version
                .unwrap();
            assert_ne!(current.generation, old_version.generation);
            assert_eq!(current.revision, 1);
        }
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }
}
#[allow(non_snake_case)]
fn URL_SAFE_NO_PAD_FOR_TEST(text: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .unwrap()
}

#[tokio::test(start_paused = true)]
async fn pending_login_timeout_closes_listener_without_exchange_or_commit() {
    let temp = TempDir::new().unwrap();
    let transport = script(Ok(token()));
    let auth = manager(&temp.path().join("auth"), transport.clone());
    let (_input, receiver) = mpsc::channel(4);
    let (events, mut event_rx) = mpsc::channel(32);
    let ui = async {
        let mut port = None;
        while let Some(event) = event_rx.recv().await {
            if let ClaudeLoginEvent::AuthorizationUrl {
                authorization_url, ..
            } = event
            {
                let url = reqwest::Url::parse(authorization_url.expose_for_login_ui()).unwrap();
                let redirect = url
                    .query_pairs()
                    .find(|(key, _)| key == "redirect_uri")
                    .unwrap()
                    .1
                    .into_owned();
                port = reqwest::Url::parse(&redirect).unwrap().port();
            }
        }
        port.unwrap()
    };
    let started = tokio::time::Instant::now();
    let (result, port) = tokio::join!(
        auth.login(receiver, events, Box::pin(std::future::pending())),
        ui
    );
    assert_eq!(
        result.unwrap_err().error.kind,
        ClaudeErrorKind::LoginTimeout
    );
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(300)
    );
    assert_eq!(transport.calls.load(Ordering::SeqCst), 0);
    assert!(
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .is_err()
    );
    assert!(auth
        .local_snapshot(Box::pin(std::future::pending()))
        .await
        .unwrap()
        .version
        .is_none());
}

#[tokio::test]
async fn failed_login_rejects_api_key_and_inconsistent_bootstrap_without_replacement() {
    for mismatch in [false, true] {
        let temp = TempDir::new().unwrap();
        let mut data = token();
        let bootstrap = if mismatch {
            data.as_object_mut().unwrap().remove("organization");
            Some(
                serde_json::json!({"oauth_account":{"account_uuid":Uuid::new_v4(),"organization_uuid":identity().organization_id}}),
            )
        } else {
            data["access_token"] = serde_json::json!("sk-ant-api01-SYNTHETIC_CANARY");
            None
        };
        let transport = Arc::new(Script {
            reply: Mutex::new(Some(Ok(data))),
            calls: AtomicUsize::new(0),
            bootstrap,
        });
        let auth = manager(&temp.path().join("auth"), transport.clone());
        let original = seed(&auth, false).await;
        let (input, receiver) = mpsc::channel(4);
        let (events, mut event_rx) = mpsc::channel(32);
        let ui = async {
            while let Some(event) = event_rx.recv().await {
                if matches!(event, ClaudeLoginEvent::AuthorizationUrl { .. }) {
                    input
                        .send(ClaudeCallbackInput::new("SYNTHETIC_CODE".into()).unwrap())
                        .await
                        .unwrap();
                }
            }
        };
        let (result, _) = tokio::join!(
            auth.login(receiver, events, Box::pin(std::future::pending())),
            ui
        );
        let error = result.unwrap_err();
        assert!(!format!("{error:?}").contains("SYNTHETIC_CANARY"));
        assert_eq!(
            auth.local_snapshot(Box::pin(std::future::pending()))
                .await
                .unwrap()
                .version,
            Some(original)
        );
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn cancellation_ready_with_login_response_prevents_replacement() {
    struct CancelOnResponse {
        cancel: Notify,
        bootstrap: bool,
    }
    #[async_trait]
    impl OAuthTransport for CancelOnResponse {
        async fn token(&self, _: TokenRequest<'_>) -> Result<Value, ClaudeError> {
            let mut data = token();
            if self.bootstrap {
                data.as_object_mut().unwrap().remove("organization");
            } else {
                self.cancel.notify_one();
            }
            Ok(data)
        }
        async fn bootstrap(&self, _: &Secret) -> Result<Value, ClaudeError> {
            self.cancel.notify_one();
            Ok(
                serde_json::json!({"oauth_account":{"account_uuid":identity().account_id,"organization_uuid":identity().organization_id}}),
            )
        }
    }
    for bootstrap in [false, true] {
        let temp = TempDir::new().unwrap();
        let transport = Arc::new(CancelOnResponse {
            cancel: Notify::new(),
            bootstrap,
        });
        let auth = manager(&temp.path().join("auth"), transport.clone());
        let original = seed(&auth, false).await;
        let (input, receiver) = mpsc::channel(4);
        let (events, mut event_rx) = mpsc::channel(32);
        let ui = async {
            while let Some(event) = event_rx.recv().await {
                if matches!(event, ClaudeLoginEvent::AuthorizationUrl { .. }) {
                    input
                        .send(ClaudeCallbackInput::new("SYNTHETIC_CODE".into()).unwrap())
                        .await
                        .unwrap();
                }
            }
        };
        let (result, _) = tokio::join!(
            auth.login(receiver, events, Box::pin(transport.cancel.notified())),
            ui
        );
        let error = result.unwrap_err();
        assert_eq!(error.error.kind, ClaudeErrorKind::LoginCancelled);
        assert_eq!(error.commit_outcome, Some(CommitOutcome::NotCommitted));
        assert_eq!(
            auth.local_snapshot(Box::pin(std::future::pending()))
                .await
                .unwrap()
                .version,
            Some(original)
        );
    }
}

struct ProcessTransport {
    root: PathBuf,
    block_after_rotation: bool,
}
#[async_trait]
impl OAuthTransport for ProcessTransport {
    async fn token(&self, _: TokenRequest<'_>) -> Result<Value, ClaudeError> {
        let mut calls = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(self.root.join("calls"))
            .unwrap();
        calls.write_all(b"refresh\n").unwrap();
        if self.block_after_rotation {
            std::fs::write(self.root.join("rotated"), b"synthetic-provider-rotated").unwrap();
            std::future::pending().await
        } else if self.root.join("rotated").exists() {
            let mut error = ClaudeError::new(ClaudeErrorKind::LoginRequired);
            error.details.http_status = Some(400);
            error.details.provider_code = Some(ClaudeProviderCode::InvalidGrant);
            Err(error)
        } else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(token())
        }
    }
    async fn bootstrap(&self, _: &Secret) -> Result<Value, ClaudeError> {
        panic!("not a login")
    }
}
#[test]
#[ignore = "subprocess-only synthetic authority helper"]
fn process_authority_helper() {
    let root = PathBuf::from(std::env::var_os("MIRROR_TEST_AUTH_ROOT").unwrap());
    let block_after_rotation = std::env::var_os("MIRROR_TEST_ROTATE_THEN_BLOCK").is_some();
    let auth = manager(
        &root.join("auth"),
        Arc::new(ProcessTransport {
            root,
            block_after_rotation,
        }),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(auth.authorize(context(&identity()), None));
    match result {
        Ok(lease) => println!("revision={}", lease.version.revision),
        Err(error) => println!("safe-error={error}"),
    }
}
fn process(root: &std::path::Path, block: bool) -> std::process::Child {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "providers::claude::auth::tests::process_authority_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("MIRROR_TEST_AUTH_ROOT", root)
        .env_remove("ANTHROPIC_BASE_URL")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if block {
        command.env("MIRROR_TEST_ROTATE_THEN_BLOCK", "1");
    }
    command.spawn().unwrap()
}
#[tokio::test]
async fn independent_process_refresh_race_and_death_after_synthetic_rotation() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let auth = manager(&root.join("auth"), script(Ok(token())));
    seed(&auth, true).await;
    let first = process(&root, false);
    let second = process(&root, false);
    for child in [first, second] {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(output.contains("revision=2"));
        assert!(!output.contains("SYNTHETIC_ACCESS"));
    }
    assert_eq!(
        std::fs::read_to_string(root.join("calls"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    seed(&auth, true).await;
    let original = std::fs::read(root.join("auth").join(store::FILE)).unwrap();
    let mut child = process(&root, true);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !root.join("rotated").exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(
        std::fs::read(root.join("auth").join(store::FILE)).unwrap(),
        original
    );
    let output = process(&root, false).wait_with_output().unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)
        .unwrap()
        .contains("login_required"));
    assert_eq!(
        auth.status(Box::pin(std::future::pending()))
            .await
            .unwrap()
            .state,
        ClaudeAuthState::LoginRequired
    );
}
