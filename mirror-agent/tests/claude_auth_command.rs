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
            .env("HOME", home)
            .env_remove("ANTHROPIC_BASE_URL")
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
