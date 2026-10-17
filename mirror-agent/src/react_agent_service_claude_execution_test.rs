use super::*;
use crate::providers::claude::metadata::ClaudeMetadataService;
use crate::providers::ModelRequestGuard;
use axum::body::Bytes;
use axum::extract::{OriginalUri, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

struct Guard {
    allowed: AtomicBool,
    calls: AtomicUsize,
}
impl Guard {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            allowed: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
        })
    }
}
impl ModelRequestGuard for Guard {
    fn verify(&self) -> BoxFuture<'_, Result<(), LlmError>> {
        Box::pin(async {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.allowed.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(ClaudeError::new(ClaudeErrorKind::RequestCancelled).into())
            }
        })
    }
}
struct Capture {
    path: String,
    headers: HeaderMap,
    body: Value,
}
struct Reply(u16, &'static str, String);
struct Server {
    origin: String,
    endpoint: &'static str,
    requests: mpsc::UnboundedReceiver<Capture>,
    runner: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.runner.abort();
    }
}
impl Server {
    async fn new(replies: Vec<Reply>) -> Self {
        type Fixture = (Arc<Mutex<VecDeque<Reply>>>, mpsc::UnboundedSender<Capture>);
        async fn respond(
            State((replies, requests)): State<Fixture>,
            OriginalUri(uri): OriginalUri,
            headers: HeaderMap,
            body: Bytes,
        ) -> impl IntoResponse {
            requests
                .send(Capture {
                    path: uri.path().into(),
                    headers,
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                })
                .unwrap();
            let Reply(status, content_type, body) = replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected HTTP request");
            (
                axum::http::StatusCode::from_u16(status).unwrap(),
                [(axum::http::header::CONTENT_TYPE, content_type)],
                body,
            )
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let endpoint = Box::leak(format!("{origin}/v1/messages").into_boxed_str());
        let (requests, receiver) = mpsc::unbounded_channel();
        let app = axum::Router::new()
            .fallback(respond)
            .with_state((Arc::new(Mutex::new(VecDeque::from(replies))), requests));
        let runner = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            origin,
            endpoint,
            requests: receiver,
            runner,
        }
    }
    async fn request(&mut self) -> Capture {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
}
fn sse(text: &str, partial_tool: bool) -> Reply {
    let mut events = vec![
        json!({"type":"message_start","message":{"id":"fixture","type":"message","role":"assistant","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        json!({"type":"content_block_stop","index":0}),
    ];
    if partial_tool {
        events.extend([
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"never-execute","name":"_dangerous","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"unfinished\":"}}),
        ]);
    } else {
        events.extend([json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}), json!({"type":"message_stop"})]);
    }
    Reply(
        200,
        "text/event-stream",
        events
            .iter()
            .map(|event| {
                format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap()
                )
            })
            .collect(),
    )
}
fn live_authority() -> Arc<Authority> {
    Arc::new(Authority {
        identity: Mutex::new(Some(identity(1))),
        allow_dispatch: true,
        ..Default::default()
    })
}
async fn provider(
    server: &Server,
    directory: &std::path::Path,
    authority: Arc<Authority>,
    guard: Arc<Guard>,
    window: Option<usize>,
    model: &str,
) -> ModelProvider {
    let metadata = ClaudeMetadataService::for_test(
        authority.clone(),
        directory.canonicalize().unwrap().join("metadata"),
        server.origin.clone(),
    )
    .unwrap();
    ModelProvider::new_with_authority(
        ModelProviderConfig {
            api: ModelApi::AnthropicOAuth,
            model: model.into(),
            context_window: window,
            api_key: None,
            base_url: None,
            auth_path: None,
            claude_session: None,
            prompt_cache_key: "session".into(),
            request_timeout: Some(Duration::from_secs(5)),
        },
        Some(guard),
        Some(authority),
    )
    .await
    .unwrap()
    .with_test_claude(Some(Arc::new(metadata)), Some(server.endpoint))
}
async fn native_service(
    provider: ModelProvider,
    compaction: Option<ModelProvider>,
    store: Box<dyn Store>,
    tools: Vec<Arc<dyn crate::tools::AnyTool>>,
) -> (
    ReactAgentServiceHandle,
    tokio::task::JoinHandle<()>,
    broadcast::Receiver<ReactServiceEvent>,
) {
    let tools = DirectToolExecutor::new(tools);
    let agent = ReactAgent::new(
        provider.clone(),
        provider,
        compaction,
        &tools,
        SystemPromptConfig {
            custom_prompt: Some("system".into()),
            ..Default::default()
        },
        ReactAgentConfig::default(),
    )
    .unwrap();
    let (events, receiver) = broadcast::channel(1024);
    let (service, runner) = ReactAgentService::start(
        agent,
        Box::new(tools),
        store,
        "session",
        AgentApprovalMode::Auto,
        vec![],
        vec![],
        None,
        events,
        None,
    )
    .await
    .unwrap();
    (service, tokio::spawn(runner), receiver)
}
fn bound_store() -> Box<dyn Store> {
    Box::new(
        InMemoryStore::default()
            .with_claude_session("session", Some(ClaudeSessionBinding::new(identity(1)))),
    )
}
async fn turn_events(
    events: &mut broadcast::Receiver<ReactServiceEvent>,
) -> Vec<ReactServiceEvent> {
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.recv())
            .await
            .unwrap()
            .unwrap();
        let done = event == ReactServiceEvent::TurnCompleted;
        seen.push(event);
        if done {
            return seen;
        }
    }
}
fn logs(entries: &[AgentMessage]) -> Vec<&str> {
    entries
        .iter()
        .filter_map(|entry| match &entry.payload {
            Some(agent_message::Payload::Log(log)) => Some(log.text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn native_provider_metadata_retry_partial_and_changed_account_share_bound_guarded_authority()
{
    let mut server = Server::new(vec![
        Reply(
            503,
            "application/json",
            json!({"error":{"type":"api_error","message":"UPSTREAM-CANARY"}}).to_string(),
        ),
        Reply(
            401,
            "application/json",
            json!({"error":{"type":"authentication_error"}}).to_string(),
        ),
        sse("Visible partial", true),
    ])
    .await;
    let directory = tempfile::tempdir().unwrap();
    let authority = live_authority();
    let guard = Guard::new();
    let provider = provider(
        &server,
        directory.path(),
        authority.clone(),
        guard.clone(),
        None,
        "claude-opus-4-8",
    )
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let (service, runner, mut events) = native_service(
        provider,
        None,
        bound_store(),
        vec![Arc::new(Counter(calls.clone()))],
    )
    .await;
    assert_eq!(server.request().await.path, "/v1/models/claude-opus-4-8");
    let startup: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    let notice_index = startup.iter().position(|event| matches!(event, ReactServiceEvent::Agent(AgentEvent::Entries(entries)) if !logs(entries).is_empty())).unwrap();
    let usage_index = startup
        .iter()
        .position(|event| {
            matches!(
                event,
                ReactServiceEvent::Agent(AgentEvent::ContextUsage {
                    window: 200_000,
                    ..
                })
            )
        })
        .unwrap();
    assert!(notice_index < usage_index);
    let history = service.messages().await.unwrap();
    let notice = logs(&history)
        .into_iter()
        .find(|text| text.starts_with("Claude context"))
        .unwrap();
    assert!(
        notice.contains("reviewed model fallback")
            && notice.contains("offline")
            && notice.contains("not fetched")
    );
    assert!(!notice.contains("UPSTREAM-CANARY") && !notice.contains("selected-model metadata"));
    service.execute(user("partial-provider")).unwrap();
    let seen = turn_events(&mut events).await;
    let finals: Vec<_> = seen
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            matches!(
                event,
                ReactServiceEvent::Agent(AgentEvent::StreamFinal { .. })
            )
        })
        .collect();
    let errors: Vec<_> = seen
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, ReactServiceEvent::TurnError(_)))
        .collect();
    assert_eq!(finals.len(), 1);
    assert_eq!(errors.len(), 1);
    assert!(finals[0].0 < errors[0].0);
    assert!(
        matches!(errors[0].1, ReactServiceEvent::TurnError(error) if error.claude_error.as_ref().unwrap().kind == "partial_stream_error")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let first = server.request().await;
    let retry = server.request().await;
    assert_eq!(first.path, "/v1/messages");
    assert_eq!(first.body, retry.body);
    assert_eq!(first.headers.get("x-api-key"), None);
    assert_eq!(
        first.headers.get("x-claude-code-session-id").unwrap(),
        "session"
    );
    let attempts = authority.attempts.lock().unwrap().clone();
    assert_eq!(attempts.len(), 3);
    assert!(attempts
        .iter()
        .all(|(_, expected, _, guarded)| expected == &identity(1) && *guarded));
    assert_eq!(attempts[0].0, ClaudeOperation::Models);
    assert_eq!(attempts[1].0, ClaudeOperation::Messages);
    assert!(attempts[1].2.is_none() && attempts[2].2.is_some());
    *authority.identity.lock().unwrap() = Some(identity(2));
    service.execute(user("changed-account")).unwrap();
    let seen = turn_events(&mut events).await;
    assert!(seen.iter().any(|event| matches!(event, ReactServiceEvent::TurnError(error) if error.claude_error.as_ref().unwrap().kind == "account_changed")));
    assert_eq!(authority.dispatches.load(Ordering::SeqCst), 3);
    assert!(server.requests.try_recv().is_err());
    let history = service.messages().await.unwrap();
    assert_eq!(
        logs(&history)
            .into_iter()
            .filter(|text| text.starts_with("Claude context"))
            .count(),
        1
    );
    assert_eq!(
        service.claude_session.binding().unwrap().identity,
        identity(1)
    );
    assert!(guard.calls.load(Ordering::SeqCst) >= 5);
    finish(&service, runner).await;
}

#[tokio::test]
async fn explicit_context_is_guarded_and_auth_failures_never_dispatch_or_fallback() {
    let mut server = Server::new(vec![sse("Done", false)]).await;
    let directory = tempfile::tempdir().unwrap();
    let authority = live_authority();
    let guard = Guard::new();
    let provider = provider(
        &server,
        directory.path(),
        authority.clone(),
        guard.clone(),
        Some(123_456),
        "claude-opus-4-8",
    )
    .await;
    let (service, runner, mut events) = native_service(provider, None, bound_store(), vec![]).await;
    let history = service.messages().await.unwrap();
    assert!(logs(&history).iter().any(|text| text
        .contains("123456 tokens; explicit configuration; fresh; source fetched: not fetched")));
    assert_eq!(authority.dispatches.load(Ordering::SeqCst), 0);
    while events.try_recv().is_ok() {}
    service.execute(user("explicit-context")).unwrap();
    let seen = turn_events(&mut events).await;
    assert!(!seen
        .iter()
        .any(|event| matches!(event, ReactServiceEvent::TurnError(_))));
    assert_eq!(server.request().await.path, "/v1/messages");
    guard.allowed.store(false, Ordering::SeqCst);
    service.execute(user("revoked-guard")).unwrap();
    let seen = turn_events(&mut events).await;
    assert!(seen.iter().any(|event| matches!(event, ReactServiceEvent::TurnError(error) if error.claude_error.as_ref().unwrap().kind == "request_cancelled")));
    assert_eq!(authority.dispatches.load(Ordering::SeqCst), 1);
    guard.allowed.store(true, Ordering::SeqCst);
    *authority.identity.lock().unwrap() = None;
    service.execute(user("logout")).unwrap();
    let seen = turn_events(&mut events).await;
    assert!(seen.iter().any(|event| matches!(event, ReactServiceEvent::TurnError(error) if error.claude_error.as_ref().unwrap().kind == "login_required")));
    assert_eq!(authority.dispatches.load(Ordering::SeqCst), 1);
    assert!(!service.messages().await.unwrap().is_empty());
    finish(&service, runner).await;
}

#[tokio::test]
async fn metadata_guard_failure_keeps_history_attached_without_fallback_or_http() {
    let server = Server::new(vec![]).await;
    let directory = tempfile::tempdir().unwrap();
    let authority = live_authority();
    let guard = Guard::new();
    guard.allowed.store(false, Ordering::SeqCst);
    let provider = provider(
        &server,
        directory.path(),
        authority.clone(),
        guard.clone(),
        Some(200_000),
        "claude-opus-4-8",
    )
    .await;
    let (service, runner, mut events) = native_service(provider, None, bound_store(), vec![]).await;
    let startup: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    assert!(startup.iter().any(|event| matches!(event, ReactServiceEvent::TurnError(error) if error.claude_error.as_ref().unwrap().kind == "request_cancelled")));
    let history = service.messages().await.unwrap();
    assert!(!history.is_empty());
    assert!(logs(&history).is_empty());
    service.execute(user("blocked-metadata")).unwrap();
    let seen = turn_events(&mut events).await;
    assert!(seen.iter().any(|event| matches!(event, ReactServiceEvent::TurnError(error) if error.claude_error.as_ref().unwrap().kind == "request_cancelled")));
    assert_eq!(authority.dispatches.load(Ordering::SeqCst), 0);
    assert_eq!(
        service.claude_session.binding().unwrap().identity,
        identity(1)
    );
    finish(&service, runner).await;
}

#[tokio::test]
async fn validated_remote_context_is_labeled_and_cached_without_duplicate_notices() {
    let mut server = Server::new(vec![
        Reply(
            200,
            "application/json",
            json!({"id":"claude-opus-4-8","max_input_tokens":1_000_000,"max_tokens":128_000})
                .to_string(),
        ),
        sse("one", false),
        sse("two", false),
    ])
    .await;
    let directory = tempfile::tempdir().unwrap();
    let authority = live_authority();
    let guard = Guard::new();
    let provider = provider(
        &server,
        directory.path(),
        authority.clone(),
        guard,
        None,
        "claude-opus-4-8",
    )
    .await;
    let (service, runner, mut events) = native_service(provider, None, bound_store(), vec![]).await;
    assert_eq!(server.request().await.path, "/v1/models/claude-opus-4-8");
    for id in ["first", "second"] {
        service.execute(user(id)).unwrap();
        let seen = turn_events(&mut events).await;
        assert!(!seen
            .iter()
            .any(|event| matches!(event, ReactServiceEvent::TurnError(_))));
        assert_eq!(server.request().await.path, "/v1/messages");
    }
    let history = service.messages().await.unwrap();
    let notices: Vec<_> = logs(&history)
        .into_iter()
        .filter(|text| text.starts_with("Claude context"))
        .collect();
    assert_eq!(notices.len(), 2);
    assert!(notices[0].contains("1000000 tokens; selected-model metadata; fresh; source fetched:"));
    assert!(notices[1].contains("selected-model metadata; cached; source fetched:"));
    assert!(!notices[0].contains("not fetched") && !notices[0].contains("fallback"));
    assert_eq!(
        authority
            .attempts
            .lock()
            .unwrap()
            .iter()
            .filter(|(operation, ..)| *operation == ClaudeOperation::Models)
            .count(),
        1
    );
    finish(&service, runner).await;
}

async fn compaction_store() -> Box<dyn Store> {
    let mut store = InMemoryStore::default()
        .with_claude_session("session", Some(ClaudeSessionBinding::new(identity(1))));
    let mut old = user("old");
    let Some(agent_message::Payload::Chat(chat)) = &mut old.payload else {
        unreachable!()
    };
    let Some(chat_record::Message::User(message)) = &mut chat.message else {
        unreachable!()
    };
    message.content = "old history ".repeat(8_000);
    let mut entries = vec![
        AgentMessage {
            payload: Some(agent_message::Payload::ContextWindow(
                mirror_protocol::ContextWindowRecord {
                    system_prompt: "system".into(),
                    ..Default::default()
                },
            )),
            ..Default::default()
        },
        old,
        user("recent"),
    ];
    store.append("session", 0, &mut entries).await.unwrap();
    Box::new(store)
}

#[tokio::test]
async fn inherited_oauth_and_explicit_api_compaction_keep_binding_and_report_actual_billing() {
    for explicit_api in [false, true] {
        let mut server = Server::new(vec![sse("summary", false)]).await;
        let directory = tempfile::tempdir().unwrap();
        let authority = live_authority();
        let guard = Guard::new();
        let main = provider(
            &server,
            directory.path(),
            authority.clone(),
            guard.clone(),
            Some(200_000),
            "claude-opus-4-8",
        )
        .await;
        let compaction = ModelProvider::new_with_authority(
            ModelProviderConfig {
                api: if explicit_api {
                    ModelApi::AnthropicMessages
                } else {
                    ModelApi::Responses
                },
                model: "claude-sonnet-4-6".into(),
                context_window: Some(200_000),
                api_key: Some(
                    if explicit_api {
                        "EXPLICIT-API-KEY"
                    } else {
                        "MUST-CLEAR-ON-SWITCH"
                    }
                    .into(),
                ),
                base_url: Some(if explicit_api {
                    server.origin.clone()
                } else {
                    "https://unused.invalid".into()
                }),
                auth_path: None,
                claude_session: None,
                prompt_cache_key: "session".into(),
                request_timeout: Some(Duration::from_secs(5)),
            },
            Some(guard.clone()),
            Some(authority.clone()),
        )
        .await
        .unwrap()
        .with_test_claude(None, Some(server.endpoint));
        let compaction = if explicit_api {
            compaction
        } else {
            compaction.with_inherited_compaction_provider()
        };
        let (service, runner, mut events) =
            native_service(main, Some(compaction), compaction_store().await, vec![]).await;
        while events.try_recv().is_ok() {}
        service.compact().await.unwrap();
        let seen = turn_events(&mut events).await;
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, ReactServiceEvent::TurnError(_))),
            "{seen:?}"
        );
        let request = server.request().await;
        assert_eq!(request.path, "/v1/messages");
        assert_eq!(request.body["model"], "claude-sonnet-4-6");
        let history = service.messages().await.unwrap();
        let selected = history
            .iter()
            .rev()
            .find_map(|entry| match &entry.payload {
                Some(agent_message::Payload::ContextWindow(window)) => window.compaction.as_ref(),
                _ => None,
            })
            .unwrap();
        let api = if explicit_api {
            "anthropic-messages"
        } else {
            "anthropic-oauth"
        };
        assert_eq!(selected.model, "claude-sonnet-4-6");
        assert_eq!(
            selected.inference.as_ref().unwrap().api.as_deref(),
            Some(api)
        );
        assert_eq!(
            selected.inference.as_ref().unwrap().claude_session.as_ref(),
            Some(&(&ClaudeSessionBinding::new(identity(1))).into())
        );
        assert!(logs(&history)
            .iter()
            .any(|text| text.contains(&format!("Compaction uses claude-sonnet-4-6 via {api}"))));
        if explicit_api {
            assert_eq!(
                request.headers.get("x-api-key").unwrap(),
                "EXPLICIT-API-KEY"
            );
            assert!(request.headers.get("authorization").is_none());
            assert_eq!(authority.dispatches.load(Ordering::SeqCst), 0);
            assert!(logs(&history)
                .iter()
                .any(|text| text.contains("API-key billing")));
        } else {
            assert!(request.headers.get("x-api-key").is_none());
            assert!(request.headers.get("authorization").is_some());
            let attempts = authority.attempts.lock().unwrap();
            assert_eq!(attempts.len(), 1);
            assert_eq!(attempts[0].1, identity(1));
            assert!(attempts[0].3);
        }
        assert!(guard.calls.load(Ordering::SeqCst) >= 2);
        finish(&service, runner).await;
    }
}

#[tokio::test]
async fn native_context_cache_rechecks_generation_before_using_prior_remote_limit() {
    let mut server = Server::new(vec![
        Reply(
            200,
            "application/json",
            json!({"id":"claude-opus-4-8","max_input_tokens":1_000_000}).to_string(),
        ),
        Reply(
            403,
            "application/json",
            json!({"error":{"type":"permission_error","message":"UPSTREAM-CANARY"}}).to_string(),
        ),
    ])
    .await;
    let directory = tempfile::tempdir().unwrap();
    let authority = live_authority();
    let guard = Guard::new();
    let provider = provider(
        &server,
        directory.path(),
        authority.clone(),
        guard,
        None,
        "claude-opus-4-8",
    )
    .await;
    let (service, runner, mut events) = native_service(provider, None, bound_store(), vec![]).await;
    assert_eq!(server.request().await.path, "/v1/models/claude-opus-4-8");
    while events.try_recv().is_ok() {}
    *authority.generation.lock().unwrap() = Some(Uuid::from_u128(6));
    service.execute(user("changed-generation")).unwrap();
    let seen = turn_events(&mut events).await;
    assert!(seen.iter().any(|event| matches!(event, ReactServiceEvent::TurnError(error) if error.claude_error.as_ref().unwrap().kind == "access_denied")));
    assert!(!seen.iter().any(|event| matches!(
        event,
        ReactServiceEvent::Agent(AgentEvent::ContextUsage { .. })
            | ReactServiceEvent::Agent(AgentEvent::StreamFinal { .. })
    )));
    assert_eq!(server.request().await.path, "/v1/models/claude-opus-4-8");
    let attempts = authority.attempts.lock().unwrap().clone();
    assert_eq!(attempts.len(), 2);
    assert!(attempts
        .iter()
        .all(
            |(operation, expected, _, guarded)| *operation == ClaudeOperation::Models
                && expected == &identity(1)
                && *guarded
        ));
    assert!(!logs(&service.messages().await.unwrap())
        .iter()
        .any(|text| text.contains("fallback") || text.contains("UPSTREAM-CANARY")));
    finish(&service, runner).await;
}

struct NestedChild(crate::tools::SubagentTool);
impl crate::tools::AnyTool for NestedChild {
    fn name(&self) -> &str {
        "nested"
    }
    fn needs_approval(&self) -> bool {
        false
    }
    fn definition(&self) -> crate::tools::ToolDefinition {
        crate::tools::ToolDefinition {
            name: "nested".into(),
            description: "fixture".into(),
            parameters: serde_json::from_value(json!({"type":"object"})).unwrap(),
        }
    }
    fn execute<'a>(
        &'a self,
        _: &'a str,
        context: &'a ExecutionContext,
    ) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            assert_eq!(
                context.claude_binding(),
                Some(ClaudeSessionBinding::new(identity(1)))
            );
            let task = self.0.spawn("leaf".into(), vec![], context).await.unwrap();
            assert_eq!(task, "1000");
            context.tasks().wait(&task).await.unwrap()
        })
    }
}
fn nested_call() -> Reply {
    let events = [
        json!({"type":"message_start","message":{"id":"fixture","type":"message","role":"assistant","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"nested-call","name":"_nested","input":{}}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}),
        json!({"type":"message_stop"}),
    ];
    Reply(
        200,
        "text/event-stream",
        events
            .iter()
            .map(|event| {
                format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap()
                )
            })
            .collect(),
    )
}

#[tokio::test]
async fn oauth_child_and_grandchild_inherit_authority_guard_and_distinct_native_sessions() {
    use crate::task_manager::{TaskEvent, TaskManager};
    let mut server = Server::new(vec![
        nested_call(),
        sse("leaf done", false),
        sse("child done", false),
        sse("child done", false),
    ])
    .await;
    let directory = tempfile::tempdir().unwrap();
    let authority = live_authority();
    let guard = Guard::new();
    let metadata = Arc::new(
        ClaudeMetadataService::for_test(
            authority.clone(),
            directory.path().canonicalize().unwrap().join("metadata"),
            server.origin.clone(),
        )
        .unwrap(),
    );
    let config = ModelProviderConfig {
        api: ModelApi::AnthropicOAuth,
        model: "claude-opus-4-8".into(),
        context_window: Some(200_000),
        api_key: None,
        base_url: None,
        auth_path: None,
        // The static tool configuration is stale. Each generation must inherit
        // the native parent's current binding rather than this expectation.
        claude_session: Some(ClaudeSessionBinding::new(identity(99))),
        prompt_cache_key: "root-session".into(),
        request_timeout: Some(Duration::from_secs(5)),
    };
    let grandchild = crate::tools::SubagentTool::new(
        directory.path(),
        "root-session",
        config.clone(),
        ReactAgentConfig::default(),
        None,
        vec![],
    )
    .unwrap()
    .with_request_guard(guard.clone())
    .with_test_claude(metadata.clone(), server.endpoint);
    let nested: Arc<dyn crate::tools::AnyTool> = Arc::new(NestedChild(grandchild));
    let child = crate::tools::SubagentTool::new(
        directory.path(),
        "root-session",
        config,
        ReactAgentConfig::default(),
        None,
        vec![nested.clone()],
    )
    .unwrap()
    .with_request_guard(guard.clone())
    .with_test_claude(metadata, server.endpoint);
    let (manager, tasks, mut events, runner) = TaskManager::new();
    let manager = tokio::spawn(manager.run());
    let runner = tokio::spawn(runner);
    let binding = ClaudeSessionBinding::new(identity(1));
    let context = ExecutionContext::new(AgentApprovalMode::Auto, None, tasks.clone(), None, None)
        .with_claude(
            ClaudeSession::new(Some(binding.clone())),
            Some(authority.clone()),
        );
    context.input().set_inference_config(Some(InferenceConfig {
        model: "claude-opus-4-8".into(),
        api: Some("anthropic-oauth".into()),
        claude_session: Some((&binding).into()),
        ..Default::default()
    }));
    let id = child
        .spawn("branch".into(), vec![nested], &context)
        .await
        .unwrap();
    assert_eq!(id, "1000");
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let TaskEvent::Finished(_, result) = events.recv().await.unwrap() {
                break result;
            }
        }
    })
    .await
    .unwrap();
    assert!(result.success, "{result:?}");
    let mut requests = Vec::new();
    while let Ok(request) = server.requests.try_recv() {
        requests.push(request);
    }
    assert!(requests.len() >= 3);
    assert!(requests.iter().all(
        |request| request.path == "/v1/messages" && request.headers.get("x-api-key").is_none()
    ));
    let sessions: Vec<_> = requests
        .iter()
        .map(|request| {
            request
                .headers
                .get("x-claude-code-session-id")
                .unwrap()
                .to_str()
                .unwrap()
        })
        .collect();
    assert_ne!(sessions[0], sessions[1]);
    assert!(sessions[2..].iter().all(|session| *session == sessions[0]));
    for session in &sessions {
        Uuid::parse_str(session).unwrap();
        assert_ne!(*session, "root-session");
    }
    let attempts = authority.attempts.lock().unwrap().clone();
    assert_eq!(attempts.len(), requests.len());
    assert!(attempts
        .iter()
        .all(
            |(operation, expected, _, guarded)| *operation == ClaudeOperation::Messages
                && expected == &identity(1)
                && *guarded
        ));
    assert!(guard.calls.load(Ordering::SeqCst) > attempts.len());
    tasks.close().await.unwrap();
    manager.await.unwrap();
    drop(context);
    drop(tasks);
    runner.await.unwrap();
}
