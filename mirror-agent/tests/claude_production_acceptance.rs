//! Explicitly opt-in production refresh acceptance; ordinary cargo test ignores it.
//!
//! This uses the shipping authority, never a second OAuth client or edited grant.
//! See docs/claude-integration/ACCEPTANCE.md before enabling the ignored test.
use std::future::pending;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use mirror_agent::providers::claude::{
    auth::ClaudeAuthManager, profile, ClaudeAuthState, ClaudeAuthorization,
    ClaudeAuthorizationContext, ClaudeError, ClaudeErrorKind, ClaudeOperation,
};
use serde_json::json;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const OPT_IN: &str = "refresh-owned-grant";
const MARKER: &str = "MIRROR_PRODUCTION_REFRESH_OK";
const WORKSPACE_MARKER: &str = "Mirror production acceptance workspace\n";

// Every diagnostic is a closed stage/reason. Do not put subprocess output,
// provider messages, paths, identities, credentials or raw IO errors in this type.
#[derive(Debug)]
enum Failure {
    Setup(&'static str),
    Check(&'static str),
    Authority(&'static str, ClaudeErrorKind),
}

fn authority(stage: &'static str, error: ClaudeError) -> Failure {
    Failure::Authority(stage, error.kind)
}

struct Settings {
    cli: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    revision: String,
    model: String,
}

fn absolute_path(value: Option<std::ffi::OsString>) -> Result<PathBuf, Failure> {
    let path = value
        .map(PathBuf::from)
        .ok_or(Failure::Setup("missing_path"))?;
    if !path.is_absolute() {
        return Err(Failure::Setup("path_must_be_absolute"));
    }
    path.canonicalize()
        .map_err(|_| Failure::Setup("path_unavailable"))
}

fn settings() -> Result<Settings, Failure> {
    // This check must precede even local authority construction or inspection.
    if std::env::var("MIRROR_CLAUDE_LIVE_ACCEPTANCE").as_deref() != Ok(OPT_IN) {
        return Err(Failure::Setup("explicit_live_opt_in_required"));
    }
    let revision = std::env::var("MIRROR_CLAUDE_ACCEPTANCE_REVISION")
        .map_err(|_| Failure::Setup("source_revision_required"))?;
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Failure::Setup("source_revision_must_be_commit_id"));
    }
    let model = std::env::var("MIRROR_CLAUDE_ACCEPTANCE_MODEL")
        .unwrap_or_else(|_| "claude-opus-4-8".into());
    if !matches!(model.as_str(), "claude-opus-4-8" | "claude-sonnet-4-6") {
        return Err(Failure::Setup("unreviewed_acceptance_model"));
    }
    let cli = absolute_path(std::env::var_os("MIRROR_CLAUDE_ACCEPTANCE_CLI"))?;
    let executable = cli
        .metadata()
        .map_err(|_| Failure::Setup("cli_unavailable"))?;
    if !executable.is_file() || executable.permissions().mode() & 0o111 == 0 {
        return Err(Failure::Setup("cli_must_be_executable"));
    }
    let home = absolute_path(std::env::var_os("HOME"))?;
    let workspace = absolute_path(std::env::var_os("MIRROR_CLAUDE_ACCEPTANCE_WORKSPACE"))?;
    if !home.is_dir() || !workspace.is_dir() || workspace == home {
        return Err(Failure::Setup("dedicated_workspace_required"));
    }
    // A deliberately prepared, already trusted synthetic workspace is required.
    // The harness never grants trust or patches the user's application config.
    if std::fs::read(workspace.join("mirror-claude-acceptance.txt"))
        .map_err(|_| Failure::Setup("workspace_marker_required"))?
        != WORKSPACE_MARKER.as_bytes()
    {
        return Err(Failure::Setup("workspace_marker_mismatch"));
    }
    Ok(Settings {
        cli,
        home,
        workspace,
        revision,
        model,
    })
}

// The installed CLI creates a worker in a separate session. On timeout/error,
// stop observed descendants before their parent, then reap the CLI. The private
// CLI process group is also a last-resort drop guard. Suppress stderr even if it
// contains an unexpected diagnostic. Captured stdout/stderr are never printed.
struct ChildGroup(tokio::process::Child);
impl ChildGroup {
    async fn terminate(&mut self) {
        let Some(root) = self.0.id() else {
            return;
        };
        // Read only PID/PPID, never process command lines or environments. Keep
        // the parent alive until discovery so setsid() workers remain attributable.
        let table = tokio::time::timeout(Duration::from_secs(2), async {
            let mut process = Command::new("/bin/ps")
                .args(["-axo", "pid=,ppid="])
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .ok()?;
            let mut bytes = Vec::new();
            process
                .stdout
                .take()?
                .take(1_048_577)
                .read_to_end(&mut bytes)
                .await
                .ok()?;
            if bytes.len() > 1_048_576 {
                return None;
            }
            process.wait().await.ok()?.success().then_some(bytes)
        })
        .await;
        if let Ok(Some(table)) = table {
            let pairs: Vec<(u32, u32)> = String::from_utf8_lossy(&table)
                .lines()
                .filter_map(|line| {
                    let mut fields = line.split_whitespace();
                    Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
                })
                .collect();
            let mut owned = vec![root];
            let mut index = 0;
            while index < owned.len() {
                let parent = owned[index];
                for &(pid, ppid) in &pairs {
                    if ppid == parent && !owned.contains(&pid) {
                        owned.push(pid);
                    }
                }
                index += 1;
            }
            for pid in owned.into_iter().skip(1).rev() {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
            // The CLI normally observes worker termination and reaps it.
            if tokio::time::timeout(Duration::from_secs(2), self.0.wait())
                .await
                .is_ok()
            {
                return;
            }
        }
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(root as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
        let _ = tokio::time::timeout(Duration::from_secs(2), self.0.wait()).await;
    }
}
impl Drop for ChildGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.0.id() {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

async fn normal_installed_request(settings: &Settings) -> Result<(), Failure> {
    installed_request_with_timeout(settings, Duration::from_secs(180)).await
}

async fn installed_request_with_timeout(
    settings: &Settings,
    timeout: Duration,
) -> Result<(), Failure> {
    let state = tempfile::Builder::new()
        .prefix("mirror-refresh-request-")
        .tempdir()
        .map_err(|_| Failure::Setup("request_state_directory"))?;
    let mut command = Command::new(&settings.cli);
    command
        .args([
            "exec",
            "--api",
            "anthropic-oauth",
            "--model",
            &settings.model,
            "--reasoning-effort",
            "none",
            "--approval-mode",
            "approve",
            "Reply with exactly MIRROR_PRODUCTION_REFRESH_OK. Do not call any tools.",
        ])
        .current_dir(&settings.workspace)
        .env_clear()
        .env("HOME", &settings.home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("LANG", "C.UTF-8")
        .env("TERM", "xterm-256color")
        .env("MIRROR_EXTENSIONS_ENABLED", "false")
        .env("MIRROR_SKIP_UPDATE", "1")
        .env("MIRROR_MODELS_CONFIG", state.path().join("models.json"))
        .env("MIRROR_STATE_DB", state.path().join("state.db"))
        .env("MIRROR_CLI_DB", state.path().join("cli.db"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(library_path) = std::env::var_os("LD_LIBRARY_PATH") {
        command.env("LD_LIBRARY_PATH", library_path);
    }
    command.as_std_mut().process_group(0);
    let mut child = ChildGroup(
        command
            .spawn()
            .map_err(|_| Failure::Check("request_spawn"))?,
    );
    let output = child
        .0
        .stdout
        .take()
        .ok_or(Failure::Check("request_stdout"))?;
    let result = async {
        let mut bytes = Vec::new();
        output
            .take(65_537)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| Failure::Check("request_output_read"))?;
        if bytes.len() > 65_536 {
            return Err(Failure::Check("request_output_limit"));
        }
        let status = child
            .0
            .wait()
            .await
            .map_err(|_| Failure::Check("request_wait"))?;
        if !status.success() {
            return Err(Failure::Check("new_process_request_failed"));
        }
        // Do not let a failed equality assertion dump model output into logs.
        if std::str::from_utf8(&bytes).ok().map(str::trim) != Some(MARKER) {
            return Err(Failure::Check("new_process_response_marker_missing"));
        }
        Ok(())
    };
    let result = tokio::time::timeout(timeout, result)
        .await
        .unwrap_or(Err(Failure::Check("new_process_request_timeout")));
    if result.is_err() {
        child.terminate().await;
    }
    result
}

async fn refresh_and_request(settings: &Settings) -> Result<(), Failure> {
    let manager = ClaudeAuthManager::new().map_err(|error| authority("construct", error))?;
    let initial = manager
        .local_snapshot(Box::pin(pending()))
        .await
        .map_err(|error| authority("initial_snapshot", error))?;
    if !matches!(
        initial.status.state,
        ClaudeAuthState::Usable | ClaudeAuthState::Refreshable
    ) {
        return Err(Failure::Check("usable_mirror_login_required"));
    }
    let identity = initial
        .status
        .identity
        .ok_or(Failure::Check("identity_missing"))?;
    let before = initial.version.ok_or(Failure::Check("version_missing"))?;
    println!(
        "{}",
        json!({"stage":"waiting_for_real_dedup_interval","seconds":61})
    );
    // No tokio paused clock, expiry edit, force flag, token comparison or hash.
    tokio::time::sleep(Duration::from_secs(61)).await;
    let current = manager
        .local_snapshot(Box::pin(pending()))
        .await
        .map_err(|error| authority("current_snapshot", error))?;
    if current.version != Some(before) || current.status.identity.as_ref() != Some(&identity) {
        return Err(Failure::Check("grant_changed_during_wait_retry_acceptance"));
    }
    let lease = manager
        .authorize(
            ClaudeAuthorizationContext {
                operation: ClaudeOperation::Messages,
                expected_identity: &identity,
                request_guard: None,
                cancellation: Box::pin(pending()),
            },
            Some(before),
        )
        .await
        .map_err(|error| authority("production_refresh", error))?;
    let after = lease.version;
    if after.generation != before.generation
        || before.revision.checked_add(1) != Some(after.revision)
        || lease.identity != identity
    {
        return Err(Failure::Check("refresh_version_or_identity_mismatch"));
    }
    // The access token stays opaque and is dropped without printing or dispatch.
    drop(lease);
    let restarted = ClaudeAuthManager::new().map_err(|error| authority("reopen", error))?;
    let saved = restarted
        .local_snapshot(Box::pin(pending()))
        .await
        .map_err(|error| authority("persisted_snapshot", error))?;
    if saved.version != Some(after) || saved.status.identity.as_ref() != Some(&identity) {
        return Err(Failure::Check("persisted_revision_mismatch"));
    }
    println!(
        "{}",
        json!({"stage":"production_refresh_persisted", "revision_before":before.revision, "revision_after":after.revision})
    );
    normal_installed_request(settings).await?;
    let final_snapshot = restarted
        .local_snapshot(Box::pin(pending()))
        .await
        .map_err(|error| authority("after_request_snapshot", error))?;
    if final_snapshot.version != Some(after)
        || final_snapshot.status.identity.as_ref() != Some(&identity)
    {
        return Err(Failure::Check("grant_changed_during_new_process_request"));
    }
    println!(
        "{}",
        json!({
            "outcome":"production_refresh_and_new_process_request_passed",
            "mirror_revision":settings.revision, "compatibility_profile":profile::PROFILE_ID,
            "platform":std::env::consts::OS, "architecture":std::env::consts::ARCH,
            "model":settings.model, "revision_before":before.revision, "revision_after":after.revision,
            "completed_at":prost_types::Timestamp::from(SystemTime::now()).to_string(),
            "tool_or_reasoning_acceptance":false,
        })
    );
    Ok(())
}

#[tokio::test]
#[ignore = "real production grant rotation and inference; explicit opt-in and private login required"]
async fn live_refresh_persists_and_new_process_request() {
    let result = match settings() {
        Ok(settings) => refresh_and_request(&settings).await,
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        match error {
            Failure::Setup(reason) | Failure::Check(reason) => {
                eprintln!("{}", json!({"outcome":"failed", "reason":reason}));
            }
            Failure::Authority(stage, kind) => {
                eprintln!(
                    "{}",
                    json!({"outcome":"failed", "stage":stage, "kind":kind})
                );
            }
        }
        panic!("production acceptance failed; only closed diagnostics are emitted");
    }
}

#[test]
fn ordinary_runs_cannot_enable_live_acceptance() {
    // Real subprocess boundary, so even an operator's opted-in parent environment
    // cannot turn this hermetic check into a credential read or provider call.
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "live_refresh_persists_and_new_process_request",
            "--nocapture",
        ])
        .env_clear()
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("explicit_live_opt_in_required"));
    assert!(std::fs::read_dir(home.path()).unwrap().next().is_none());
}

#[test]
fn relative_executable_path_is_rejected() {
    assert!(matches!(
        absolute_path(Some(Path::new("mirror").into())),
        Err(Failure::Setup("path_must_be_absolute"))
    ));
}

#[tokio::test]
async fn request_plumbing_handles_spaces_and_discards_untrusted_output() {
    // Process-plumbing fixture only: no authority construction or live opt-in.
    let root = tempfile::Builder::new()
        .prefix("synthetic acceptance path with spaces ")
        .tempdir()
        .unwrap();
    let cli = root.path().join("synthetic mirror");
    let settings = Settings {
        cli: cli.clone(),
        home: root.path().into(),
        workspace: root.path().into(),
        revision: "0".repeat(40),
        model: "claude-opus-4-8".into(),
    };
    for (script, expected) in [
        (
            "#!/bin/sh\nprintf 'SYNTHETIC_UNTRUSTED_PROVIDER_DIAGNOSTIC' >&2\nprintf 'MIRROR_PRODUCTION_REFRESH_OK\\n'\n",
            None,
        ),
        (
            "#!/bin/sh\nprintf 'SYNTHETIC_UNTRUSTED_PROVIDER_PAYLOAD'\nprintf 'SYNTHETIC_UNTRUSTED_PROVIDER_DIAGNOSTIC' >&2\nexit 1\n",
            Some("new_process_request_failed"),
        ),
        (
            "#!/bin/sh\nprintf 'SYNTHETIC_UNTRUSTED_PROVIDER_PAYLOAD'\n",
            Some("new_process_response_marker_missing"),
        ),
    ] {
        std::fs::write(&cli, script).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        match (normal_installed_request(&settings).await, expected) {
            (Ok(()), None) => {}
            (Err(Failure::Check(actual)), Some(expected)) => assert_eq!(actual, expected),
            _ => panic!("unexpected closed acceptance process outcome"),
        }
    }
}

#[test]
#[ignore = "synthetic subprocess helper exercised by timeout cleanup test"]
fn synthetic_detached_request_helper() {
    let root = PathBuf::from(std::env::var_os("HOME").unwrap());
    if std::env::var("MIRROR_SYNTHETIC_DETACHED_ROLE").as_deref() == Ok("worker") {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std::fs::write(
            root.join("synthetic-worker.json"),
            json!({"pid":std::process::id(), "port":listener.local_addr().unwrap().port()})
                .to_string(),
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(120));
        drop(listener);
    } else {
        assert_eq!(
            std::env::var("MIRROR_SYNTHETIC_DETACHED_ROLE").as_deref(),
            Ok("cli"),
            "synthetic helper must be started by its parent test"
        );
        let mut worker = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "synthetic_detached_request_helper"])
            .env_clear()
            .env("HOME", &root)
            .env("MIRROR_SYNTHETIC_DETACHED_ROLE", "worker")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        worker.wait().unwrap();
    }
}

#[tokio::test]
async fn timed_out_request_reaps_worker_outside_cli_process_group() {
    let root = tempfile::tempdir().unwrap();
    let cli = root.path().join("synthetic mirror");
    let executable = std::env::current_exe().unwrap();
    std::fs::write(
        &cli,
        format!(
            "#!/bin/sh\nMIRROR_SYNTHETIC_DETACHED_ROLE=cli exec '{}' --ignored --exact synthetic_detached_request_helper\n",
            executable.to_str().unwrap().replace('\'', "'\\''")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
    let settings = Settings {
        cli,
        home: root.path().into(),
        workspace: root.path().into(),
        revision: "0".repeat(40),
        model: "claude-opus-4-8".into(),
    };
    let result = installed_request_with_timeout(&settings, Duration::from_secs(1)).await;
    let worker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.path().join("synthetic-worker.json")).unwrap())
            .unwrap();
    let pid = nix::unistd::Pid::from_raw(worker["pid"].as_u64().unwrap() as i32);
    let gone = nix::sys::signal::kill(pid, None) == Err(nix::errno::Errno::ESRCH);
    // Do not leak this synthetic child even if cleanup regresses again.
    if !gone {
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
    }
    assert!(matches!(
        result,
        Err(Failure::Check("new_process_request_timeout"))
    ));
    assert!(gone, "synthetic detached worker survived request cleanup");
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", worker["port"].as_u64().unwrap() as u16))
            .is_err()
    );
}
