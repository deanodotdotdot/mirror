use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use mirror_agent::agent_message;
use mirror_agent::agent_rpc_client::AgentRpcClient;
use mirror_agent::GetMessagesRequest;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Command;

#[tokio::test]
async fn worker_describes_its_workspace_when_the_client_asks() {
    let workspace = tempfile::tempdir().expect("workspace");

    let prompt = initial_context(
        workspace.path(),
        serde_json::json!({"include_environment": true}),
        None,
    )
    .await
    .system_prompt;

    let header = format!(
        "# Environment\n- Working directory: {}\n- Git repository: no\n",
        workspace.path().display()
    );
    assert!(prompt.starts_with(&header), "{prompt}");
    assert!(prompt.contains("\n- Shell: bash "), "{prompt}");
}

#[tokio::test]
async fn worker_adds_no_environment_section_unless_asked() {
    let workspace = tempfile::tempdir().expect("workspace");

    let prompt = initial_context(workspace.path(), serde_json::json!({}), None)
        .await
        .system_prompt;

    assert!(!prompt.contains("# Environment"), "{prompt}");
}

#[tokio::test]
async fn codex_compaction_resolves_its_own_credential_path_override() {
    let workspace = tempfile::tempdir().expect("workspace");
    // The primary provider must not retain Codex's credential reference. The
    // explicitly selected compactor still needs the documented environment
    // override; HOME intentionally contains no default .codex/auth.json.
    let context = initial_context(
        workspace.path(),
        serde_json::json!({
            "api": "anthropic-messages",
            "context_window": 200_000,
            "compaction_model": "test-codex",
            "compaction_model_api": "openai-codex-responses",
        }),
        None,
    )
    .await;
    assert!(context.system_prompt_report.is_some());
}

#[tokio::test]
async fn worker_omits_unconfigured_web_tools_by_default() {
    let workspace = tempfile::tempdir().expect("workspace");
    let context = initial_context(workspace.path(), serde_json::json!({}), None).await;
    let tools = context.system_prompt_report.expect("prompt report").tools;
    assert!(tools.iter().any(|tool| tool.source == "bash"));
    assert!(!tools
        .iter()
        .any(|tool| tool.source == "web_search" || tool.source == "web_browse"));
}

#[tokio::test]
async fn worker_skips_unconfigured_web_tools_in_an_allowlist() {
    let workspace = tempfile::tempdir().expect("workspace");
    let context = initial_context(
        workspace.path(),
        serde_json::json!({"tools": ["bash", "web_search", "web_browse"]}),
        None,
    )
    .await;
    let tools = context.system_prompt_report.expect("prompt report").tools;
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.source.as_str())
            .collect::<Vec<_>>(),
        ["bash"]
    );
}

#[tokio::test]
async fn worker_omits_web_tools_with_a_blank_key() {
    let workspace = tempfile::tempdir().expect("workspace");
    let context = initial_context(
        workspace.path(),
        serde_json::json!({"tools": ["bash", "web_search", "web_browse"]}),
        Some(" \t "),
    )
    .await;
    let tools = context.system_prompt_report.expect("prompt report").tools;
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.source.as_str())
            .collect::<Vec<_>>(),
        ["bash"]
    );
}

#[tokio::test]
async fn worker_offers_web_tools_with_a_configured_key() {
    let workspace = tempfile::tempdir().expect("workspace");
    let context = initial_context(
        workspace.path(),
        serde_json::json!({"tools": ["bash", "web_search", "web_browse"]}),
        Some("synthetic-firecrawl-key"),
    )
    .await;
    let tools = context.system_prompt_report.expect("prompt report").tools;
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.source.as_str())
            .collect::<Vec<_>>(),
        ["bash", "web_search", "web_browse"]
    );
}

async fn initial_context(
    workspace: &Path,
    overrides: serde_json::Value,
    firecrawl_key: Option<&str>,
) -> mirror_agent::ContextWindowRecord {
    let auth = workspace.join("auth.json");
    std::fs::write(
        &auth,
        r#"{"tokens":{"access_token":"test","account_id":"test"}}"#,
    )
    .expect("write credentials");
    let mut config = serde_json::json!({
        "rpc_address": "127.0.0.1:0",
        "conversation_id": "session",
        "model": "test",
        "database": workspace.join("state.db"),
        "create_session": true,
    });
    config
        .as_object_mut()
        .expect("config object")
        .extend(overrides.as_object().expect("override object").clone());
    let mut command = Command::new(worker_binary());
    command.env_remove("FIRECRAWL_API_KEY");
    if let Some(key) = firecrawl_key {
        command.env("FIRECRAWL_API_KEY", key);
    }
    let mut child = command
        .arg(config.to_string())
        .env("MIRROR_WORKSPACE", workspace)
        .env("MIRROR_CODEX_AUTH_PATH", auth)
        .env("HOME", workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("start worker");
    let mut lines = BufReader::new(child.stdout.take().expect("worker stdout")).lines();
    let address = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let line = lines
                .next_line()
                .await
                .expect("read worker output")
                .expect("worker exited before publishing its address");
            if let Some(address) = line.strip_prefix("MIRROR_RPC_ADDRESS=") {
                break address.to_string();
            }
        }
    })
    .await
    .expect("worker startup deadline");
    let mut client = AgentRpcClient::connect(format!("http://{address}"))
        .await
        .expect("connect to worker");
    let messages = client
        .get_messages(GetMessagesRequest {
            session_id: "session".into(),
            offset: 0,
            limit: 1,
        })
        .await
        .expect("read history")
        .into_inner()
        .messages;
    child.kill().await.expect("stop worker");
    match messages
        .into_iter()
        .next()
        .and_then(|message| message.payload)
    {
        Some(agent_message::Payload::ContextWindow(window)) => window,
        payload => panic!("first record is not a context window: {payload:?}"),
    }
}

fn worker_binary() -> PathBuf {
    std::env::var_os("MIRROR_AGENT_BIN")
        .map(PathBuf::from)
        .or_else(|| option_env!("CARGO_BIN_EXE_mirror-agent").map(PathBuf::from))
        .expect("worker binary path")
}

#[tokio::test]
async fn subscription_without_login_still_attaches_with_native_unbound_state_and_history() {
    use base64::Engine;
    use prost::Message;
    use tokio_stream::wrappers::ReceiverStream;
    let workspace = tempfile::tempdir().unwrap();
    let config = serde_json::json!({
        "rpc_address":"127.0.0.1:0", "conversation_id":"session", "model":"claude-opus-4-8",
        "api":"anthropic-oauth", "database":workspace.path().join("state.db"), "create_session":true,
        "tools":[], "context_window":200_000,
    });
    let mut child = Command::new(worker_binary())
        .arg(config.to_string())
        .env("HOME", workspace.path())
        .env("MIRROR_WORKSPACE", workspace.path())
        .env("ANTHROPIC_API_KEY", "SYNTHETIC-PAID-KEY-MUST-NOT-BE-USED")
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("FIRECRAWL_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let (address, typed_error) = tokio::time::timeout(Duration::from_secs(10), async {
        let mut typed_error = false;
        loop {
            let line = lines
                .next_line()
                .await
                .unwrap()
                .expect("worker remains available");
            if let Some(encoded) = line.strip_prefix("MIRROR_SESSION_MESSAGE=") {
                let bytes = base64::prelude::BASE64_STANDARD.decode(encoded).unwrap();
                let event = mirror_protocol::SessionMessage::decode(bytes.as_slice()).unwrap();
                if let Some(mirror_protocol::session_message::Payload::RuntimeDelta(delta)) =
                    event.payload
                {
                    if let Some(mirror_protocol::runtime_delta::Payload::TurnError(error)) =
                        delta.payload
                    {
                        typed_error |= error.claude_error.is_some();
                    }
                }
            }
            if let Some(address) = line.strip_prefix("MIRROR_RPC_ADDRESS=") {
                break (address.to_string(), typed_error);
            }
        }
    })
    .await
    .unwrap();
    assert!(typed_error);
    let mut client = AgentRpcClient::connect(format!("http://{address}"))
        .await
        .unwrap();
    assert!(!client
        .get_messages(GetMessagesRequest {
            session_id: "session".into(),
            offset: 0,
            limit: 10
        })
        .await
        .unwrap()
        .into_inner()
        .messages
        .is_empty());
    let (_requests, receiver) = tokio::sync::mpsc::channel::<mirror_protocol::ClientMessage>(4);
    let mut session = client
        .session(ReceiverStream::new(receiver))
        .await
        .unwrap()
        .into_inner();
    let state = session.message().await.unwrap().unwrap();
    assert!(
        matches!(state.payload, Some(mirror_protocol::session_message::Payload::ClaudeSessionState(state)) if state.binding.is_none() && state.message_id.is_none())
    );
    child.kill().await.unwrap();
}

#[tokio::test]
async fn subscription_invalid_configuration_is_rejected_before_workspace_or_secret_resolution() {
    use base64::Engine;
    use prost::Message;
    for invalid in [
        serde_json::json!({"api_key":"SYNTHETIC-KEY"}),
        serde_json::json!({"base_url":"http://127.0.0.1:1"}),
    ] {
        let workspace = tempfile::tempdir().unwrap();
        let mut config = serde_json::json!({"rpc_address":"127.0.0.1:0", "conversation_id":"session", "model":"claude-opus-4-8", "api":"anthropic-oauth", "create_session":true});
        config
            .as_object_mut()
            .unwrap()
            .extend(invalid.as_object().unwrap().clone());
        let output = Command::new(worker_binary())
            .arg(config.to_string())
            .env("HOME", workspace.path().join("does-not-exist"))
            .env(
                "MIRROR_WORKSPACE",
                workspace.path().join("also-does-not-exist"),
            )
            .env_remove("ANTHROPIC_BASE_URL")
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains("SYNTHETIC-KEY"));
        let event = stdout
            .lines()
            .find_map(|line| line.strip_prefix("MIRROR_SESSION_MESSAGE="))
            .expect("typed startup error");
        let bytes = base64::prelude::BASE64_STANDARD.decode(event).unwrap();
        let event = mirror_protocol::SessionMessage::decode(bytes.as_slice()).unwrap();
        assert!(
            matches!(event.payload, Some(mirror_protocol::session_message::Payload::RuntimeDelta(delta)) if matches!(&delta.payload, Some(mirror_protocol::runtime_delta::Payload::TurnError(error)) if error.claude_error.as_ref().is_some_and(|detail| detail.kind == "invalid_configuration")))
        );
        assert!(!workspace.path().join("does-not-exist").exists());
    }
}
