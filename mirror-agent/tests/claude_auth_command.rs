//! Synthetic private-pipe tests. No callback is ever submitted for exchange.
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use uuid::Uuid;

struct Native {
    child: Child,
    lines: Lines<BufReader<ChildStdout>>,
    id: Uuid,
}
impl Native {
    fn start(home: &std::path::Path) -> Self {
        Self::with_stdin(home, std::process::Stdio::piped(), Uuid::new_v4())
    }
    fn with_stdin(home: &std::path::Path, stdin: std::process::Stdio, id: Uuid) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mirror-agent"))
            .args(["auth", "--stdio"])
            // No foreign-store overrides or credentials may escape the test HOME.
            .env_clear()
            .env("HOME", home)
            .env("ANTHROPIC_API_KEY", "SYNTHETIC_API_KEY_CANARY")
            .env("CLAUDE_CODE_OAUTH_TOKEN", "SYNTHETIC_BEARER_CANARY")
            .current_dir(home)
            .stdin(stdin)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        Self { child, lines, id }
    }
    async fn write(&mut self, value: Value) {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(&bytes)
            .await
            .unwrap();
    }
    async fn request(&mut self, action: &str) {
        self.write(json!({"schema_version":1,"request_id":self.id,"type":"request","provider":"anthropic-oauth","action":action,"params":{}})).await;
    }
    async fn event(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(3), self.lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("native event");
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            value["compatibility_profile"],
            mirror_agent::providers::claude::profile::PROFILE_ID
        );
        value
    }
    async fn url(&mut self) -> reqwest::Url {
        for _ in 0..10 {
            let event = self.event().await;
            if event["type"] == "authorization_url" {
                return reqwest::Url::parse(event["authorization_url"].as_str().unwrap()).unwrap();
            }
            assert_eq!(event["type"], "progress");
        }
        panic!("expected authorization event");
    }
    async fn finish(&mut self, code: i32) {
        // Keep stdin open; completing a loopback or status must not hang on it.
        let status = tokio::time::timeout(Duration::from_secs(3), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.code(), Some(code));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), self.lines.next_line())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .await
            .unwrap();
        assert!(stderr.is_empty());
    }
}
fn home() -> (tempfile::TempDir, std::path::PathBuf) {
    let temp = tempfile::TempDir::new().unwrap();
    let path = temp.path().canonicalize().unwrap();
    (temp, path)
}
fn public_clean(value: &Value) {
    let bytes = serde_json::to_string(value).unwrap();
    for canary in [
        "SYNTHETIC_API_KEY_CANARY",
        "SYNTHETIC_BEARER_CANARY",
        "SYNTHETIC_CALLBACK_CANARY",
        "SYNTHETIC_FOREIGN_",
        "code_verifier",
        "access_token",
        "refresh_token",
        "authorization_url",
    ] {
        assert!(!bytes.contains(canary));
    }
}

#[tokio::test]
async fn local_status_creates_nothing_and_logout_has_one_durable_result() {
    let (_temp, path) = home();
    let mut native = Native::start(&path);
    native.request("status").await;
    let result = native.event().await;
    assert_eq!(result["type"], "completed");
    assert_eq!(result["result"]["status"]["state"], "logged_out");
    public_clean(&result);
    native.finish(0).await;
    assert!(!path.join(".mirror").exists());
    let mut native = Native::start(&path);
    native.request("logout").await;
    let result = native.event().await;
    assert_eq!(result["type"], "completed");
    public_clean(&result);
    // Only receipt of the terminal result proves that the synchronous commit
    // has happened. A subsequent cancel may race process exit (BrokenPipe).
    let late = format!(
        "{}\n",
        json!({"schema_version":1,"request_id":native.id,"type":"cancel"})
    );
    let _ = native
        .child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(late.as_bytes())
        .await;
    native.finish(0).await;
    let record: Value =
        serde_json::from_slice(&std::fs::read(path.join(".mirror/claude_oauth.json")).unwrap())
            .unwrap();
    assert!(record["credential"].is_null());
    assert_eq!(record["revision"], 0);
    let mut native = Native::start(&path);
    native.request("status").await;
    assert_eq!(
        native.event().await["result"]["status"]["state"],
        "logged_out"
    );
    native.finish(0).await;
}

#[tokio::test]
async fn queued_cancel_and_eof_prevent_logout_before_commit() {
    use std::io::Write;
    use std::os::fd::FromRawFd;
    for mode in ["cancel", "eof"] {
        let (_temp, path) = home();
        let id = Uuid::new_v4();
        let mut input = format!(
            "{}\n",
            json!({"schema_version":1,"request_id":id,"type":"request","provider":"anthropic-oauth","action":"logout","params":{}})
        );
        if mode == "cancel" {
            input.push_str(&format!(
                "{}\n",
                json!({"schema_version":1,"request_id":id,"type":"cancel"})
            ));
        }
        let (read, write) = nix::unistd::pipe().unwrap();
        // SAFETY: pipe returned two newly owned descriptors.
        let read = unsafe { std::fs::File::from_raw_fd(read) };
        let mut write = Some(unsafe { std::fs::File::from_raw_fd(write) });
        use std::os::fd::AsRawFd;
        for file in [&read, write.as_ref().unwrap()] {
            nix::fcntl::fcntl(
                file.as_raw_fd(),
                nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
            )
            .unwrap();
        }
        write.as_mut().unwrap().write_all(input.as_bytes()).unwrap();
        // Preload input and close the EOF writer before spawning: cancellation
        // is already observable before the child can begin any mutation.
        if mode == "eof" {
            write.take();
        }
        let mut native = Native::with_stdin(&path, read.into(), id);
        let result = native.event().await;
        assert_eq!(result["type"], "error");
        assert_eq!(result["error"]["kind"], "request_cancelled");
        assert_eq!(result["commit_outcome"], "not_committed");
        public_clean(&result);
        native.finish(130).await;
        assert!(!path.join(".mirror").exists());
    }
}

#[tokio::test]
async fn login_wrong_callback_eof_and_signals_leave_no_grant_or_listener() {
    for mode in ["eof", "cancel", "interrupt", "terminate"] {
        let (_temp, path) = home();
        let mut native = Native::start(&path);
        native.request("login").await;
        let url = native.url().await;
        let redirect = url
            .query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .unwrap()
            .1
            .into_owned();
        let port = reqwest::Url::parse(&redirect).unwrap().port().unwrap();
        native.write(json!({"schema_version":1,"request_id":native.id,"type":"callback","value":"SYNTHETIC_CALLBACK_CANARY#wrong"})).await;
        loop {
            let event = native.event().await;
            public_clean(&event);
            if event["notice"] == "callback_rejected" {
                break;
            }
        }
        match mode {
            "eof" => {
                native.child.stdin.take();
            }
            "cancel" => {
                native
                    .write(json!({"schema_version":1,"request_id":native.id,"type":"cancel"}))
                    .await
            }
            signal => {
                nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(native.child.id().unwrap() as i32),
                    if signal == "interrupt" {
                        nix::sys::signal::Signal::SIGINT
                    } else {
                        nix::sys::signal::Signal::SIGTERM
                    },
                )
                .unwrap();
            }
        }
        let result = native.event().await;
        assert_eq!(result["type"], "error");
        assert_eq!(result["error"]["kind"], "login_cancelled");
        assert_eq!(result["commit_outcome"], "not_committed");
        public_clean(&result);
        native.finish(130).await;
        assert!(!path.join(".mirror/claude_oauth.json").exists());
        assert!(
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn protocol_errors_are_payload_free_and_conflicting_endpoint_precedes_storage() {
    let (_temp, path) = home();
    for payload in [
        json!({"schema_version":1,"request_id":"SYNTHETIC_CALLBACK_CANARY","type":"request"}),
        json!({"schema_version":1,"request_id":Uuid::new_v4(),"type":"request","provider":"anthropic-oauth","action":"login","params":{"token":"SYNTHETIC_CALLBACK_CANARY"}}),
    ] {
        let mut native = Native::start(&path);
        native.write(payload).await;
        let result = native.event().await;
        assert_eq!(result["type"], "error");
        public_clean(&result);
        native.finish(1).await;
    }
    let mut native = Native::start(&path);
    native
        .child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(format!("{}\n", "SYNTHETIC_CALLBACK_CANARY".repeat(900)).as_bytes())
        .await
        .unwrap();
    let result = native.event().await;
    assert_eq!(result["type"], "error");
    public_clean(&result);
    native.finish(1).await;
    let mut child = Command::new(env!("CARGO_BIN_EXE_mirror-agent"))
        .args(["auth", "--stdio"])
        .env("HOME", &path)
        .env(
            "ANTHROPIC_BASE_URL",
            "http://SYNTHETIC_CALLBACK_CANARY.invalid",
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(format!("{}\n",json!({"schema_version":1,"request_id":Uuid::new_v4(),"type":"request","provider":"anthropic-oauth","action":"login","params":{}})).as_bytes()).await.unwrap();
    let output = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    public_clean(&result);
    assert_eq!(
        result["error"]["details"]["configuration"],
        "endpoint_not_allowed"
    );
    assert!(!path.join(".mirror").exists());
}

#[tokio::test]
async fn terminal_input_is_rejected_without_changing_echo() {
    use std::os::fd::FromRawFd;
    let (_temp, path) = home();
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: valid output descriptor pointers; remaining openpty args are optional.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let _master = unsafe { std::fs::File::from_raw_fd(master) };
    let slave_file = unsafe { std::fs::File::from_raw_fd(slave) };
    let mut before = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(unsafe { libc::tcgetattr(slave, before.as_mut_ptr()) }, 0);
    let before = unsafe { before.assume_init() };
    let child = Command::new(env!("CARGO_BIN_EXE_mirror-agent"))
        .args(["auth", "--stdio"])
        .env("HOME", &path)
        .stdin(std::process::Stdio::from(slave_file.try_clone().unwrap()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    public_clean(&serde_json::from_slice::<Value>(&output.stdout).unwrap());
    let mut after = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(unsafe { libc::tcgetattr(slave, after.as_mut_ptr()) }, 0);
    let after = unsafe { after.assume_init() };
    assert_eq!(before.c_lflag, after.c_lflag);
    assert!(output.stderr.is_empty());
}

#[tokio::test]
async fn metadata_actions_validate_typed_params_and_never_hide_missing_login() {
    let (_temp, path) = home();
    let identity = json!({"account_id":Uuid::from_u128(1),"organization_id":Uuid::from_u128(2)});
    for (action, params) in [
        ("models", json!({})),
        ("usage", json!({"expected_identity":identity})),
        (
            "model_context",
            json!({"expected_identity":identity,"model":"claude-opus-4-8","explicit_context_window":200000}),
        ),
    ] {
        let mut native = Native::start(&path);
        native.write(json!({"schema_version":1,"request_id":native.id,"type":"request","provider":"anthropic-oauth","action":action,"params":params})).await;
        let result = native.event().await;
        assert_eq!(result["type"], "error");
        assert_eq!(result["error"]["kind"], "login_required");
        public_clean(&result);
        native.finish(1).await;
    }
    assert!(!path.join(".mirror").exists());
    for (action, params) in [
        ("models", json!({"api_key":"SYNTHETIC_API_KEY_CANARY"})),
        ("usage", json!({"base_url":"https://unreviewed.invalid"})),
        ("model_context", json!({"model":"a"})),
        ("status", json!({"expected_identity":identity})),
        ("usage", Value::Null),
    ] {
        let mut native = Native::start(&path);
        native.write(json!({"schema_version":1,"request_id":native.id,"type":"request","provider":"anthropic-oauth","action":action,"params":params})).await;
        let result = native.event().await;
        assert_eq!(result["type"], "error");
        assert_eq!(result["error"]["kind"], "invalid_configuration");
        assert_eq!(result["phase"], "protocol");
        public_clean(&result);
        native.finish(1).await;
    }
    assert!(!path.join(".mirror").exists());
}

fn seed_metadata_auth(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    let directory = path.join(".mirror");
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let now = std::time::SystemTime::now();
    let stamp = |t| prost_types::Timestamp::from(t).to_string();
    let record = json!({"schema_version":1,"generation":Uuid::from_u128(3),"revision":1,"installation_id":Uuid::from_u128(4),"credential":{"access_token":"sk-ant-oat-SYNTHETIC_ACCESS_CANARY","refresh_token":"SYNTHETIC_REFRESH_CANARY","expires_at":stamp(now+Duration::from_secs(86400)),"authorized_at":stamp(now-Duration::from_secs(3600)),"account_id":Uuid::from_u128(1),"organization_id":Uuid::from_u128(2),"email":null,"organization_name":null,"scopes":["user:inference"]}});
    for (name, bytes) in [
        ("claude_oauth.json", serde_json::to_vec(&record).unwrap()),
        ("claude_oauth.lock", Vec::new()),
    ] {
        let file = directory.join(name);
        std::fs::write(&file, bytes).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

/// Synthetic foreign stores under a disposable HOME, never real user files.
/// Claude Code's path is documented at
/// https://code.claude.com/docs/en/authentication#credential-management.
/// Its JSON is only a credential/account sentinel, not a validated Claude Code
/// schema or a macOS Keychain emulation. The partial OMP database uses the
/// auth_credentials columns and OAuth row representation from pinned
/// packages/ai/src/auth/sqlite-credential-store.ts at df4f996e66a200ab7e0496114d43bdf85f32194d;
/// it does not emulate OMP's complete schema, migrations, or process behavior.
fn foreign_account_fixtures(
    home: &std::path::Path,
    account: u128,
) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    use std::os::unix::fs::PermissionsExt;
    for directory in [".claude", ".omp", ".omp/agent"] {
        let path = home.join(directory);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let account_id = Uuid::from_u128(account);
    let organization_id = Uuid::from_u128(account + 1);
    let expires_at = (std::time::SystemTime::now() + Duration::from_secs(86400))
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let claude = home.join(".claude/.credentials.json");
    let sentinel = json!({
        "claudeAiOauth": {
            "accessToken":format!("sk-ant-oat-SYNTHETIC_FOREIGN_CLAUDE_ACCESS_{account}"),
            "refreshToken":format!("SYNTHETIC_FOREIGN_CLAUDE_REFRESH_{account}"),
            "expiresAt":expires_at,
            "scopes":["user:inference"]
        },
        "syntheticFixtureIdentity": {
            "account_id":account_id,
            "organization_id":organization_id
        }
    });
    std::fs::write(&claude, serde_json::to_vec(&sentinel).unwrap()).unwrap();
    let omp = home.join(".omp/agent/agent.db");
    let database = rusqlite::Connection::open(&omp).unwrap();
    database
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS auth_credentials (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            provider TEXT NOT NULL,
            credential_type TEXT NOT NULL,
            data TEXT NOT NULL,
            disabled_cause TEXT DEFAULT NULL,
            identity_key TEXT DEFAULT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        ); DELETE FROM auth_credentials;",
        )
        .unwrap();
    let oauth = json!({
        "access":format!("sk-ant-oat-SYNTHETIC_FOREIGN_OMP_ACCESS_{account}"),
        "refresh":format!("SYNTHETIC_FOREIGN_OMP_REFRESH_{account}"),
        "expires":expires_at,
        "accountId":account_id,
        "orgId":organization_id
    });
    database
        .execute(
            "INSERT INTO auth_credentials
            (provider, credential_type, data, identity_key, created_at, updated_at)
            VALUES ('anthropic', 'oauth', ?1, ?2, 1, 1)",
            rusqlite::params![
                oauth.to_string(),
                format!("account:{account_id}|org:{organization_id}")
            ],
        )
        .unwrap();
    database.close().unwrap();
    [claude, omp]
        .into_iter()
        .map(|path| {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}

async fn native_status(home: &std::path::Path) -> Value {
    let mut native = Native::start(home);
    native.request("status").await;
    let event = native.event().await;
    assert_eq!(event["type"], "completed");
    public_clean(&event);
    native.finish(0).await;
    event["result"]["status"].clone()
}

async fn native_local_context(home: &std::path::Path, logged_in: bool) {
    let mut native = Native::start(home);
    // Explicit context is entirely local: this fixture never calls a provider,
    // even if a regression mistakenly treats a foreign grant as a local login.
    native.write(json!({
        "schema_version":1,"request_id":native.id,"type":"request",
        "provider":"anthropic-oauth","action":"model_context",
        "params":{
            "expected_identity":{"account_id":Uuid::from_u128(1),"organization_id":Uuid::from_u128(2)},
            "model":"claude-opus-4-8","explicit_context_window":123456
        }
    })).await;
    let event = native.event().await;
    public_clean(&event);
    if logged_in {
        assert_eq!(event["type"], "completed");
        assert_eq!(event["result"]["model_context"]["context_window"], 123456);
        assert_eq!(event["result"]["model_context"]["provenance"], "explicit");
        native.finish(0).await;
    } else {
        assert_eq!(event["type"], "error");
        assert_eq!(event["error"]["kind"], "login_required");
        native.finish(1).await;
    }
}

#[tokio::test]
async fn foreign_account_switch_cannot_supply_rebind_or_logout_mirror_grant() {
    let (_temp, home) = home();
    let mut foreign = foreign_account_fixtures(&home, 101);
    let missing = native_status(&home).await;
    assert_eq!(missing["state"], "logged_out");
    assert!(missing["identity"].is_null());
    native_local_context(&home, false).await;
    assert!(!home.join(".mirror").exists());
    for (path, bytes) in &foreign {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }

    seed_metadata_auth(&home);
    let mirror_path = home.join(".mirror/claude_oauth.json");
    let mirror_before = std::fs::read(&mirror_path).unwrap();
    let mirror_version: Value = serde_json::from_slice(&mirror_before).unwrap();
    assert_eq!(mirror_version["generation"], Uuid::from_u128(3).to_string());
    assert_eq!(mirror_version["revision"], 1);
    let expected = native_status(&home).await;
    assert_eq!(expected["state"], "usable");
    assert_eq!(expected["source"], "Mirror login");
    assert_eq!(
        expected["identity"],
        json!({
            "account_id":Uuid::from_u128(1),"organization_id":Uuid::from_u128(2)
        })
    );

    for account in [201, 301] {
        let changed = foreign_account_fixtures(&home, account);
        assert!(changed
            .iter()
            .zip(&foreign)
            .all(|(new, old)| new.1 != old.1));
        foreign = changed;
        assert_eq!(native_status(&home).await, expected);
        native_local_context(&home, true).await;
        // Public status deliberately has no version. Equality of the entire
        // test-owned record proves generation/revision and grant are unchanged.
        assert_eq!(std::fs::read(&mirror_path).unwrap(), mirror_before);
        for (path, bytes) in &foreign {
            assert_eq!(&std::fs::read(path).unwrap(), bytes);
        }
    }

    let mut native = Native::start(&home);
    native.request("logout").await;
    let event = native.event().await;
    assert_eq!(event["type"], "completed");
    assert_eq!(event["result"]["status"]["state"], "logged_out");
    public_clean(&event);
    native.finish(0).await;
    assert_eq!(native_status(&home).await["state"], "logged_out");
    native_local_context(&home, false).await;
    let tombstone: Value = serde_json::from_slice(&std::fs::read(&mirror_path).unwrap()).unwrap();
    assert!(tombstone["credential"].is_null());
    assert_eq!(tombstone["revision"], 0);
    assert_ne!(tombstone["generation"], mirror_version["generation"]);
    for (path, bytes) in &foreign {
        assert_eq!(&std::fs::read(path).unwrap(), bytes);
    }
}

#[tokio::test]
async fn explicit_context_native_result_is_safe_local_and_identity_bound() {
    let (_temp, path) = home();
    seed_metadata_auth(&path);
    let before = std::fs::read(path.join(".mirror/claude_oauth.json")).unwrap();
    let identity = json!({"account_id":Uuid::from_u128(1),"organization_id":Uuid::from_u128(2)});
    let mut native = Native::start(&path);
    native.write(json!({"schema_version":1,"request_id":native.id,"type":"request","provider":"anthropic-oauth","action":"model_context","params":{"expected_identity":identity,"model":"selected","explicit_context_window":123456}})).await;
    let result = native.event().await;
    assert_eq!(result["type"], "completed");
    let context = &result["result"]["model_context"];
    assert_eq!(context["context_window"], 123456);
    assert_eq!(context["provenance"], "explicit");
    assert_eq!(context["identity"], identity);
    assert_eq!(context["fetched_at"], Value::Null);
    public_clean(&result);
    assert!(!result.to_string().contains("SYNTHETIC"));
    native.finish(0).await;
    let mut native = Native::start(&path);
    native.write(json!({"schema_version":1,"request_id":native.id,"type":"request","provider":"anthropic-oauth","action":"model_context","params":{"expected_identity":{"account_id":Uuid::from_u128(77),"organization_id":Uuid::from_u128(2)},"model":"selected","explicit_context_window":123456}})).await;
    let result = native.event().await;
    assert_eq!(result["error"]["kind"], "account_changed");
    native.finish(1).await;
    assert_eq!(
        before,
        std::fs::read(path.join(".mirror/claude_oauth.json")).unwrap()
    );
    assert!(!path.join(".mirror/claude_metadata.lock").exists());
}

#[tokio::test]
async fn metadata_cancel_eof_and_signals_interrupt_authority_lock_without_output_or_cache() {
    for mode in ["cancel", "eof", "interrupt", "terminate"] {
        let (_temp, path) = home();
        seed_metadata_auth(&path);
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join(".mirror/claude_oauth.lock"))
            .unwrap();
        lock.lock().unwrap();
        let mut native = Native::start(&path);
        native.request("usage").await;
        // Authority is locked; this read-only operation cannot finish first.
        tokio::time::sleep(Duration::from_millis(50)).await;
        match mode {
            "cancel" => {
                native
                    .write(json!({"schema_version":1,"request_id":native.id,"type":"cancel"}))
                    .await
            }
            "eof" => {
                native.child.stdin.take();
            }
            name => {
                nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(native.child.id().unwrap() as i32),
                    if name == "interrupt" {
                        nix::sys::signal::Signal::SIGINT
                    } else {
                        nix::sys::signal::Signal::SIGTERM
                    },
                )
                .unwrap();
            }
        }
        let result = native.event().await;
        assert_eq!(result["type"], "error");
        assert_eq!(result["error"]["kind"], "request_cancelled");
        public_clean(&result);
        native.finish(130).await;
        assert!(!path.join(".mirror/claude_metadata.json").exists());
    }
}
