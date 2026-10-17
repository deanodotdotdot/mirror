use super::*;
use crate::providers::claude::*;
use crate::providers::{
    Completion, CompletionRequest, LlmError, ModelApi, ModelProvider, ModelProviderConfig,
    StreamEvent,
};
use crate::react::{InMemoryStore, ReactAgentConfig};
use crate::system_prompt::SystemPromptConfig;
use crate::tools::DirectToolExecutor;
use mirror_protocol::{AssistantMessage, StreamContent, StreamToolCall};
use std::sync::Mutex;

#[derive(Default)]
struct Authority {
    identity: Mutex<Option<ClaudeIdentityBinding>>,
    snapshots: AtomicUsize,
    dispatches: AtomicUsize,
    allow_dispatch: bool,
    gate: Mutex<Option<(oneshot::Sender<()>, oneshot::Receiver<()>)>>,
}
impl ClaudeAuthorization for Authority {
    fn authorize<'a>(
        &'a self,
        context: ClaudeAuthorizationContext<'a>,
        _rejected: Option<ClaudeTokenVersion>,
    ) -> BoxFuture<'a, Result<ClaudeAccessLease, ClaudeError>> {
        Box::pin(async move {
            assert!(
                self.allow_dispatch,
                "independent binding tests must never dispatch"
            );
            if let Some(guard) = context.request_guard {
                guard
                    .verify()
                    .await
                    .map_err(|_| ClaudeError::new(ClaudeErrorKind::RequestCancelled))?;
            }
            if self.identity.lock().unwrap().as_ref() != Some(context.expected_identity) {
                return Err(ClaudeError::new(ClaudeErrorKind::AccountChanged));
            }
            self.dispatches.fetch_add(1, Ordering::SeqCst);
            Ok(ClaudeAccessLease {
                access_token: ClaudeAccessToken::new("SYNTHETIC-TEST-ONLY".into()).unwrap(),
                identity: context.expected_identity.clone(),
                installation_id: Uuid::from_u128(4),
                version: ClaudeTokenVersion {
                    generation: Uuid::from_u128(5),
                    revision: 1,
                },
                expires_at: std::time::SystemTime::now() + std::time::Duration::from_secs(3600),
                operation: context.operation,
            })
        })
    }
    fn local_snapshot<'a>(
        &'a self,
        _cancellation: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<ClaudeLocalAuthSnapshot, ClaudeError>> {
        self.snapshots.fetch_add(1, Ordering::SeqCst);
        let identity = self.identity.lock().unwrap().clone();
        let gate = self.gate.lock().unwrap().take();
        Box::pin(async move {
            if let Some((started, release)) = gate {
                let _ = started.send(());
                let _ = release.await;
            }
            let mut status = ClaudeAuthStatus::new(if identity.is_some() {
                ClaudeAuthState::Usable
            } else {
                ClaudeAuthState::LoggedOut
            });
            status.identity = identity;
            Ok(ClaudeLocalAuthSnapshot {
                status,
                version: None,
            })
        })
    }
}
fn identity(value: u128) -> ClaudeIdentityBinding {
    ClaudeIdentityBinding {
        account_id: Uuid::from_u128(value),
        organization_id: Uuid::from_u128(9),
    }
}
fn reconnect(expected: Option<&ClaudeSessionBinding>, target: u128) -> ReconnectClaude {
    ReconnectClaude {
        expected_binding: expected.map(Into::into),
        target_identity: Some((&identity(target)).into()),
    }
}
struct Model {
    partial: bool,
}
impl Llm for Model {
    fn model(&self) -> &str {
        "test"
    }
    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> impl futures_core::Stream<Item = Result<StreamEvent, LlmError>> + Send {
        let partial = self.partial;
        async_stream::stream! {
            if partial {
                yield Ok(StreamEvent::Content(StreamContent { content: "Visible unfinished answer".into() }));
                yield Ok(StreamEvent::ToolCall(StreamToolCall { index: 0, name: "dangerous".into(), arguments: "{\"unfinished\":".into() }));
                yield Err(ClaudeError::new(ClaudeErrorKind::PartialStreamError).into());
            } else {
                yield Ok(StreamEvent::Final(Completion { message: AssistantMessage { content: Some("Done".into()), ..Default::default() }, usage: None, truncated: false }));
            }
        }
    }
}
async fn start(
    store: Box<dyn Store>,
    authority: Arc<Authority>,
    api: ModelApi,
    partial: bool,
) -> (ReactAgentServiceHandle, tokio::task::JoinHandle<()>) {
    start_model(store, authority, api, Model { partial }, vec![]).await
}

async fn start_model<M: Llm + 'static>(
    store: Box<dyn Store>,
    authority: Arc<Authority>,
    api: ModelApi,
    model: M,
    tools: Vec<Arc<dyn crate::tools::AnyTool>>,
) -> (ReactAgentServiceHandle, tokio::task::JoinHandle<()>) {
    let provider = ModelProvider::new_with_authority(
        ModelProviderConfig {
            api,
            model: "test".into(),
            context_window: Some(200_000),
            api_key: None,
            base_url: None,
            auth_path: None,
            claude_session: None,
            prompt_cache_key: "session".into(),
            request_timeout: None,
        },
        None,
        Some(authority),
    )
    .await
    .unwrap();
    let tools = DirectToolExecutor::new(tools);
    let agent = ReactAgent::new(
        model,
        provider,
        None,
        &tools,
        SystemPromptConfig {
            custom_prompt: Some("system".into()),
            ..Default::default()
        },
        ReactAgentConfig::default(),
    )
    .unwrap();
    let (service, runtime) = ReactAgentService::start(
        agent,
        Box::new(tools),
        store,
        "session",
        AgentApprovalMode::Auto,
        vec![],
        vec![],
        None,
        broadcast::channel(1024).0,
        None,
    )
    .await
    .unwrap();
    (service, tokio::spawn(runtime))
}
async fn finish(service: &ReactAgentServiceHandle, runtime: tokio::task::JoinHandle<()>) {
    service.close().await.unwrap();
    runtime.await.unwrap();
}
fn authority(value: u128) -> Arc<Authority> {
    Arc::new(Authority {
        identity: Mutex::new(Some(identity(value))),
        snapshots: AtomicUsize::new(0),
        gate: Mutex::new(None),
        ..Default::default()
    })
}

#[tokio::test]
async fn reconnect_is_explicit_cas_and_duplicates_return_current_state_without_authority() {
    let authority = authority(1);
    let (service, runtime) = start(
        Box::new(InMemoryStore::default()),
        authority.clone(),
        ModelApi::Responses,
        false,
    )
    .await;
    assert!(service.claude_session_state(None).binding.is_none());
    let first = reconnect(None, 1);
    service
        .reconnect_claude("first".into(), first.clone())
        .await
        .unwrap();
    let one = ClaudeSessionBinding::new(identity(1));
    assert_eq!(service.claude_session.binding(), Some(one.clone()));
    *authority.identity.lock().unwrap() = Some(identity(2));
    service
        .reconnect_claude("second".into(), reconnect(Some(&one), 2))
        .await
        .unwrap();
    let snapshots = authority.snapshots.load(Ordering::SeqCst);
    *authority.identity.lock().unwrap() = None;
    service
        .reconnect_claude("first".into(), first)
        .await
        .unwrap();
    assert_eq!(authority.snapshots.load(Ordering::SeqCst), snapshots);
    assert_eq!(
        service.claude_session.binding(),
        Some(ClaudeSessionBinding::new(identity(2)))
    );
    assert!(matches!(
        service
            .reconnect_claude("first".into(), reconnect(None, 2))
            .await,
        Err(ReactServiceError::MessageIdConflict(_))
    ));
    let failure = reconnect(None, 1);
    assert!(matches!(
        service
            .reconnect_claude("failed".into(), failure.clone())
            .await,
        Err(ReactServiceError::Claude(_))
    ));
    *authority.identity.lock().unwrap() = Some(identity(1));
    assert!(matches!(
        service.reconnect_claude("failed".into(), failure).await,
        Err(ReactServiceError::Claude(_))
    ));
    finish(&service, runtime).await;
}

#[tokio::test]
async fn actor_tracks_external_future_before_poll_and_releases_on_drop_or_delivery_failure() {
    let (service, runtime) = start(
        Box::new(InMemoryStore::default()),
        authority(1),
        ModelApi::Responses,
        false,
    )
    .await;
    let future = service
        .request(|response| Command::CallTool {
            name: "missing".into(),
            arguments: "{}".into(),
            response,
        })
        .await
        .unwrap();
    // Receiving but never polling the exposed future still owns an activity permit.
    assert!(matches!(
        service
            .reconnect_claude("busy".into(), reconnect(None, 1))
            .await,
        Err(ReactServiceError::ConversationBusy)
    ));
    drop(future);
    let (response, receiver) = oneshot::channel();
    drop(receiver);
    service
        .commands
        .send(Command::CallTool {
            name: "missing".into(),
            arguments: "{}".into(),
            response,
        })
        .unwrap();
    // A command barrier confirms the failed delivery was processed and dropped.
    service.pending_approvals().await.unwrap();
    service
        .reconnect_claude("idle".into(), reconnect(None, 1))
        .await
        .unwrap();
    assert!(matches!(
        service
            .reconnect_claude("busy".into(), reconnect(None, 1))
            .await,
        Err(ReactServiceError::ConversationBusy)
    ));
    finish(&service, runtime).await;
}

#[tokio::test]
async fn settings_cannot_establish_or_replace_binding_and_excluded_runtime_surface_is_typed() {
    let (service, runtime) = start(
        Box::new(InMemoryStore::default()),
        authority(1),
        ModelApi::Responses,
        false,
    )
    .await;
    assert!(matches!(
        service
            .set_inference_config(InferenceConfig {
                claude_session: Some((&ClaudeSessionBinding::new(identity(1))).into()),
                ..Default::default()
            })
            .await,
        Err(ReactServiceError::Claude(_))
    ));
    assert!(service.claude_session.binding().is_none());
    service
        .reconnect_claude("bind".into(), reconnect(None, 1))
        .await
        .unwrap();
    service
        .set_inference_config(InferenceConfig {
            api: Some("anthropic-messages".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        service.claude_session.binding(),
        Some(ClaudeSessionBinding::new(identity(1)))
    );
    finish(&service, runtime).await;
    let provider = crate::react_agent_service::tests::test_provider(200_000).await;
    let error = provider
        .config_for(Some(&InferenceConfig {
            api: Some("anthropic-oauth".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert!(
        matches!(error, LlmError::Claude(error) if error.details.configuration == Some(ClaudeConfigurationError::UnsupportedSurface))
    );
}

#[tokio::test]
async fn native_partial_failure_is_durable_incomplete_and_never_executes_fragmented_tools() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (service, runtime) = start_model(
        Box::new(InMemoryStore::default()),
        authority(1),
        ModelApi::Responses,
        Model { partial: true },
        vec![Arc::new(Counter(calls.clone()))],
    )
    .await;
    let mut events = service.events();
    service
        .execute(AgentMessage {
            id: "user".into(),
            payload: Some(agent_message::Payload::Chat(ChatRecord {
                message: Some(chat_record::Message::User(UserMessage {
                    content: "hello".into(),
                    ..Default::default()
                })),
                success: true,
                ..Default::default()
            })),
            ..Default::default()
        })
        .unwrap();
    let mut typed_error = false;
    loop {
        match events.recv().await.unwrap() {
            ReactServiceEvent::TurnError(error) => {
                assert_eq!(error.claude_error.unwrap().kind, "partial_stream_error");
                typed_error = true;
            }
            ReactServiceEvent::TurnCompleted => break,
            _ => {}
        }
    }
    assert!(typed_error);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let entries = service.messages().await.unwrap();
    let partial = entries
        .iter()
        .find_map(|entry| match entry.payload.as_ref() {
            Some(agent_message::Payload::Chat(chat))
                if matches!(chat.message, Some(chat_record::Message::Assistant(_))) =>
            {
                Some(chat)
            }
            _ => None,
        })
        .unwrap();
    assert!(!partial.success);
    assert!(partial.truncated);
    let Some(chat_record::Message::Assistant(message)) = &partial.message else {
        unreachable!()
    };
    assert_eq!(
        message.content.as_deref(),
        Some("Visible unfinished answer")
    );
    assert!(message.tool_calls.is_empty());
    assert!(partial
        .display
        .as_ref()
        .unwrap()
        .contains("Incomplete tool call dangerous"));
    finish(&service, runtime).await;
}

#[tokio::test]
async fn only_a_native_insert_auto_binds_and_resuming_never_rebinds_even_if_empty() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sessions.db");
    let authority = authority(1);
    let store =
        crate::local::SQLiteStore::open(&database, "session", directory.path(), true, false)
            .unwrap();
    let (service, runtime) = start(
        Box::new(store),
        authority.clone(),
        ModelApi::AnthropicOAuth,
        false,
    )
    .await;
    assert_eq!(
        service.claude_session.binding(),
        Some(ClaudeSessionBinding::new(identity(1)))
    );
    finish(&service, runtime).await;
    *authority.identity.lock().unwrap() = Some(identity(2));
    let store =
        crate::local::SQLiteStore::open(&database, "session", directory.path(), true, false)
            .unwrap();
    let (service, runtime) = start(
        Box::new(store),
        authority.clone(),
        ModelApi::AnthropicOAuth,
        false,
    )
    .await;
    assert_eq!(
        service.claude_session.binding(),
        Some(ClaudeSessionBinding::new(identity(1)))
    );
    assert!(!service.messages().await.unwrap().is_empty());
    finish(&service, runtime).await;

    let database = directory.path().join("empty.db");
    drop(
        crate::local::SQLiteStore::open(&database, "session", directory.path(), true, false)
            .unwrap(),
    );
    let store =
        crate::local::SQLiteStore::open(&database, "session", directory.path(), true, false)
            .unwrap();
    let (service, runtime) =
        start(Box::new(store), authority, ModelApi::AnthropicOAuth, false).await;
    assert!(service.claude_session.binding().is_none());
    // History and explicit reconnect remain available in blocked startup state.
    assert!(!service.messages().await.unwrap().is_empty());
    service
        .reconnect_claude("recover".into(), reconnect(None, 2))
        .await
        .unwrap();
    finish(&service, runtime).await;
}

#[tokio::test]
async fn storage_failure_does_not_publish_and_successful_receipts_survive_worker_restart() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sessions.db");
    let authority = authority(1);
    let store =
        crate::local::SQLiteStore::open(&database, "session", directory.path(), true, false)
            .unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_binding BEFORE UPDATE OF claude_session ON react_session BEGIN SELECT RAISE(FAIL, 'injected'); END;").unwrap();
    let (service, runtime) = start(
        Box::new(store),
        authority.clone(),
        ModelApi::Responses,
        false,
    )
    .await;
    let request = reconnect(None, 1);
    assert!(matches!(
        service
            .reconnect_claude("failed".into(), request.clone())
            .await,
        Err(ReactServiceError::Store(_))
    ));
    assert!(service.claude_session.binding().is_none());
    let stored: Option<String> = connection
        .query_row(
            "SELECT claude_session FROM react_session WHERE id = 'session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stored.is_none());
    connection
        .execute_batch("DROP TRIGGER fail_binding")
        .unwrap();
    assert!(matches!(
        service
            .reconnect_claude("failed".into(), request.clone())
            .await,
        Err(ReactServiceError::Store(_))
    ));
    service
        .reconnect_claude("success".into(), request.clone())
        .await
        .unwrap();
    finish(&service, runtime).await;
    *authority.identity.lock().unwrap() = None;
    let snapshots = authority.snapshots.load(Ordering::SeqCst);
    let store =
        crate::local::SQLiteStore::open(&database, "session", directory.path(), false, false)
            .unwrap();
    let (service, runtime) = start(
        Box::new(store),
        authority.clone(),
        ModelApi::Responses,
        false,
    )
    .await;
    service
        .reconnect_claude("success".into(), request)
        .await
        .unwrap();
    assert_eq!(authority.snapshots.load(Ordering::SeqCst), snapshots);
    assert_eq!(
        service.claude_session.binding(),
        Some(ClaudeSessionBinding::new(identity(1)))
    );
    finish(&service, runtime).await;
}

#[tokio::test]
async fn activity_permit_releases_on_completion_and_cancelled_call() {
    let activity = Arc::new(AtomicUsize::new(0));
    let (release, wait) = oneshot::channel();
    let future = tracked_tool(
        &activity,
        Box::pin(async move {
            wait.await.unwrap();
            Ok(ToolOutput::success("done"))
        }),
    );
    assert_eq!(activity.load(Ordering::Acquire), 1);
    release.send(()).unwrap();
    let _ = future.await.unwrap();
    assert_eq!(activity.load(Ordering::Acquire), 0);
    let future = tracked_tool(&activity, Box::pin(std::future::pending()));
    assert_eq!(activity.load(Ordering::Acquire), 1);
    drop(future);
    assert_eq!(activity.load(Ordering::Acquire), 0);
}

struct Counter(Arc<AtomicUsize>);
impl crate::tools::AnyTool for Counter {
    fn needs_approval(&self) -> bool {
        false
    }
    fn name(&self) -> &str {
        "dangerous"
    }
    fn definition(&self) -> crate::tools::ToolDefinition {
        crate::tools::ToolDefinition {
            name: self.name().into(),
            description: "fixture".into(),
            parameters: serde_json::Map::new(),
        }
    }
    fn execute<'a>(
        &'a self,
        _arguments: &'a str,
        _context: &'a ExecutionContext,
    ) -> BoxFuture<'a, ToolOutput> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { ToolOutput::success("executed") })
    }
}

fn user(id: &str) -> AgentMessage {
    AgentMessage {
        id: id.into(),
        payload: Some(agent_message::Payload::Chat(ChatRecord {
            message: Some(chat_record::Message::User(UserMessage {
                content: "hello".into(),
                ..Default::default()
            })),
            success: true,
            ..Default::default()
        })),
        ..Default::default()
    }
}

#[tokio::test]
async fn input_queued_during_authority_check_prevents_reconnect_commit() {
    let authority = authority(1);
    let (started, waiting) = oneshot::channel();
    let (release, gate) = oneshot::channel();
    *authority.gate.lock().unwrap() = Some((started, gate));
    let (service, runtime) = start(
        Box::new(InMemoryStore::default()),
        authority,
        ModelApi::Responses,
        false,
    )
    .await;
    let reconnect = tokio::spawn({
        let service = service.clone();
        async move {
            service
                .reconnect_claude("race".into(), reconnect(None, 1))
                .await
        }
    });
    waiting.await.unwrap();
    let mut events = service.events();
    service.execute(user("queued")).unwrap();
    release.send(()).unwrap();
    assert!(matches!(
        reconnect.await.unwrap(),
        Err(ReactServiceError::ConversationBusy)
    ));
    assert!(service.claude_session.binding().is_none());
    while !matches!(
        events.recv().await.unwrap(),
        ReactServiceEvent::TurnCompleted
    ) {}
    finish(&service, runtime).await;
}

struct HoldingModel(mpsc::UnboundedSender<()>);
impl Llm for HoldingModel {
    fn model(&self) -> &str {
        "holding"
    }
    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> impl futures_core::Stream<Item = Result<StreamEvent, LlmError>> + Send {
        let started = self.0.clone();
        async_stream::stream! {
            started.send(()).unwrap();
            std::future::pending::<()>().await;
            yield Ok(StreamEvent::Content(StreamContent { content: String::new() }));
        }
    }
}

#[tokio::test]
async fn foreground_model_work_blocks_reconnect_and_midturn_settings_cannot_bind() {
    let (started, mut running) = mpsc::unbounded_channel();
    let (service, runtime) = start_model(
        Box::new(InMemoryStore::default()),
        authority(1),
        ModelApi::Responses,
        HoldingModel(started),
        vec![],
    )
    .await;
    service.execute(user("active")).unwrap();
    running.recv().await.unwrap();
    assert!(matches!(
        service
            .reconnect_claude("busy".into(), reconnect(None, 1))
            .await,
        Err(ReactServiceError::ConversationBusy)
    ));
    assert!(matches!(
        service
            .set_inference_config(InferenceConfig {
                claude_session: Some((&ClaudeSessionBinding::new(identity(1))).into()),
                ..Default::default()
            })
            .await,
        Err(ReactServiceError::Claude(_))
    ));
    assert!(service.claude_session.binding().is_none());
    service.interrupt().unwrap().await.unwrap();
    service.pending_approvals().await.unwrap();
    service
        .reconnect_claude("after".into(), reconnect(None, 1))
        .await
        .unwrap();
    finish(&service, runtime).await;
}

#[tokio::test]
async fn rpc_publishes_native_state_before_ack_and_typed_nacks_without_mutation() {
    use mirror_protocol::{client_message, session_message, ClientMessage};
    let authority = authority(1);
    let (service, runtime) = start(
        Box::new(InMemoryStore::default()),
        authority.clone(),
        ModelApi::Responses,
        false,
    )
    .await;
    let mut rpc = crate::rpc::AgentInProcessRpc::new(service.clone())
        .connect()
        .await
        .unwrap();
    let initial = rpc.recv().await.unwrap().unwrap();
    assert!(
        matches!(initial.payload, Some(session_message::Payload::ClaudeSessionState(state)) if state.conversation_id == "session" && state.binding.is_none() && state.message_id.is_none())
    );
    let first = reconnect(None, 1);
    for (id, request) in [
        ("bind", first.clone()),
        (
            "next",
            reconnect(Some(&ClaudeSessionBinding::new(identity(1))), 2),
        ),
        ("bind", first.clone()),
    ] {
        if id == "next" {
            *authority.identity.lock().unwrap() = Some(identity(2));
        }
        rpc.send(ClientMessage {
            session_id: "session".into(),
            message_id: id.into(),
            payload: Some(client_message::Payload::ReconnectClaude(request)),
        })
        .await
        .unwrap();
        let state = rpc.recv().await.unwrap().unwrap();
        assert!(
            matches!(state.payload, Some(session_message::Payload::ClaudeSessionState(state)) if state.message_id.as_deref() == Some(id) && state.binding == service.claude_session_state(None).binding)
        );
        assert!(
            matches!(rpc.recv().await.unwrap().unwrap().payload, Some(session_message::Payload::InputAck(ack)) if ack.message_id == id)
        );
    }
    rpc.send(ClientMessage {
        session_id: "session".into(),
        message_id: "stale".into(),
        payload: Some(client_message::Payload::ReconnectClaude(first)),
    })
    .await
    .unwrap();
    assert!(
        matches!(rpc.recv().await.unwrap().unwrap().payload, Some(session_message::Payload::InputNack(nack)) if nack.claude_error.as_ref().is_some_and(|error| error.kind == "account_changed"))
    );
    assert_eq!(
        service.claude_session.binding(),
        Some(ClaudeSessionBinding::new(identity(2)))
    );
    drop(rpc);
    finish(&service, runtime).await;
}

struct ContextTool(mpsc::UnboundedSender<ExecutionContext>);
impl crate::tools::AnyTool for ContextTool {
    fn name(&self) -> &str {
        "capture_context"
    }
    fn needs_approval(&self) -> bool {
        false
    }
    fn definition(&self) -> crate::tools::ToolDefinition {
        crate::tools::ToolDefinition {
            name: self.name().into(),
            description: "fixture".into(),
            parameters: serde_json::Map::new(),
        }
    }
    fn execute<'a>(
        &'a self,
        _arguments: &'a str,
        context: &'a ExecutionContext,
    ) -> BoxFuture<'a, ToolOutput> {
        let _ = self.0.send(context.clone());
        Box::pin(async { ToolOutput::success("captured") })
    }
}

#[tokio::test]
async fn detached_children_and_their_queued_completions_block_reconnect() {
    let (contexts, mut captured) = mpsc::unbounded_channel();
    let (started, mut model_started) = mpsc::unbounded_channel();
    let (service, runtime) = start_model(
        Box::new(InMemoryStore::default()),
        authority(1),
        ModelApi::Responses,
        HoldingModel(started),
        vec![Arc::new(ContextTool(contexts))],
    )
    .await;
    let _ = service
        .call_tool("capture_context".into(), "{}".into())
        .await
        .unwrap();
    let context = captured.recv().await.unwrap();
    let (release, completion) = oneshot::channel();
    context
        .tasks()
        .spawn("child", None, crate::TaskBehavior::Detach, |_| async move {
            completion.await.unwrap();
            ToolOutput::success("child result")
        })
        .await
        .unwrap();
    assert!(matches!(
        service
            .reconnect_claude("child-active".into(), reconnect(None, 1))
            .await,
        Err(ReactServiceError::ConversationBusy)
    ));
    release.send(()).unwrap();
    // Race the completion delivery with reconnect. The completed task's queued
    // result must be drained and seen before reporting an idle conversation.
    assert!(matches!(
        service
            .reconnect_claude("completion-race".into(), reconnect(None, 1))
            .await,
        Err(ReactServiceError::ConversationBusy)
    ));
    tokio::time::timeout(std::time::Duration::from_secs(5), model_started.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(service.claude_session.binding().is_none());
    drop(context);
    finish(&service, runtime).await;
}

struct ApprovalCounter(Arc<AtomicUsize>);
impl crate::tools::AnyTool for ApprovalCounter {
    fn name(&self) -> &str {
        "approval"
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn definition(&self) -> crate::tools::ToolDefinition {
        crate::tools::ToolDefinition {
            name: self.name().into(),
            description: "fixture".into(),
            parameters: serde_json::Map::new(),
        }
    }
    fn execute<'a>(
        &'a self,
        _arguments: &'a str,
        _context: &'a ExecutionContext,
    ) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async {
            self.0.fetch_add(1, Ordering::SeqCst);
            ToolOutput::success("executed")
        })
    }
}

#[tokio::test]
async fn external_approval_blocks_until_cancelled_call_is_dropped_without_tool_execution() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (service, runtime) = start_model(
        Box::new(InMemoryStore::default()),
        authority(1),
        ModelApi::Responses,
        Model { partial: false },
        vec![Arc::new(ApprovalCounter(calls.clone()))],
    )
    .await;
    service
        .set_approval_mode(AgentApprovalMode::Hitl)
        .await
        .unwrap();
    let mut events = service.events();
    let call = tokio::spawn({
        let service = service.clone();
        async move { service.call_tool("approval".into(), "{}".into()).await }
    });
    while !matches!(
        events.recv().await.unwrap(),
        ReactServiceEvent::HitlRequest(_)
    ) {}
    assert!(matches!(
        service
            .reconnect_claude("approval-active".into(), reconnect(None, 1))
            .await,
        Err(ReactServiceError::ConversationBusy)
    ));
    call.abort();
    let _ = call.await;
    service.pending_approvals().await.unwrap();
    service
        .reconnect_claude("after-drop".into(), reconnect(None, 1))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    finish(&service, runtime).await;
}

#[tokio::test]
async fn real_truncated_oauth_tool_stream_persists_partial_and_never_executes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Box::leak(
        format!("http://{}/v1/messages", listener.local_addr().unwrap()).into_boxed_str(),
    );
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            header.push(byte[0]);
        }
        let header = String::from_utf8(header).unwrap();
        let length = header
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(str::trim)
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "_dangerous"));
        let events = [
            serde_json::json!({"type":"message_start","message":{"id":"fixture","type":"message","role":"assistant","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
            serde_json::json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            serde_json::json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Visible unfinished answer"}}),
            serde_json::json!({"type":"content_block_stop","index":0}),
            serde_json::json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"never-execute","name":"_dangerous","input":{}}}),
            serde_json::json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"unfinished\":"}}),
        ];
        let sse: String = events
            .iter()
            .map(|event| {
                format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap()
                )
            })
            .collect();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}", sse.len()).as_bytes()).await.unwrap();
    });
    let authority = Arc::new(Authority {
        identity: Mutex::new(Some(identity(1))),
        allow_dispatch: true,
        ..Default::default()
    });
    let model = ClaudeMessages::new(
        ClaudeMessagesConfig {
            model: "claude-opus-4-8".into(),
            session_id: Uuid::new_v4().to_string(),
            binding: ClaudeSessionBinding::new(identity(1)),
            request_timeout: Some(std::time::Duration::from_secs(5)),
        },
        authority.clone(),
    )
    .unwrap()
    .with_test_endpoint(endpoint);
    let calls = Arc::new(AtomicUsize::new(0));
    // Metadata is outside this stream/service fixture. The separate settings
    // provider has an explicit limit and supplies no runtime inference override.
    let settings = crate::react_agent_service::tests::test_provider(200_000).await;
    let tools = DirectToolExecutor::new(vec![Arc::new(Counter(calls.clone()))]);
    let agent = ReactAgent::new(
        model,
        settings,
        None,
        &tools,
        SystemPromptConfig {
            custom_prompt: Some("system".into()),
            ..Default::default()
        },
        ReactAgentConfig::default(),
    )
    .unwrap();
    let (service, runtime) = ReactAgentService::start(
        agent,
        Box::new(tools),
        Box::new(InMemoryStore::default()),
        "session",
        AgentApprovalMode::Auto,
        vec![],
        vec![],
        None,
        broadcast::channel(1024).0,
        None,
    )
    .await
    .unwrap();
    let runtime = tokio::spawn(runtime);
    let mut events = service.events();
    service.execute(user("real-partial")).unwrap();
    let mut failed_finals = 0;
    let mut typed_errors = 0;
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(10), events.recv())
            .await
            .unwrap()
            .unwrap()
        {
            ReactServiceEvent::Agent(AgentEvent::StreamFinal { entry, .. }) => {
                let Some(agent_message::Payload::Chat(chat)) = &entry.payload else {
                    unreachable!()
                };
                assert!(!chat.success);
                assert!(chat.truncated);
                failed_finals += 1;
            }
            ReactServiceEvent::TurnError(error) => {
                assert_eq!(
                    failed_finals, 1,
                    "terminal event arrived before partial: {error:?}"
                );
                assert_eq!(error.claude_error.unwrap().kind, "partial_stream_error");
                typed_errors += 1;
            }
            ReactServiceEvent::TurnCompleted => break,
            _ => {}
        }
    }
    assert_eq!(failed_finals, 1);
    assert_eq!(typed_errors, 1);
    assert!(events.try_recv().is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(authority.dispatches.load(Ordering::SeqCst), 1);
    let history = service.messages().await.unwrap();
    assert!(history.iter().any(|entry| matches!(&entry.payload, Some(agent_message::Payload::Chat(chat)) if !chat.success && chat.truncated && chat.display.as_deref().unwrap_or("").contains("Incomplete tool call dangerous"))));
    server.await.unwrap();
    finish(&service, runtime).await;
}

#[tokio::test]
async fn failed_initial_binding_commit_keeps_worker_attached_and_unbound() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sessions.db");
    let store =
        crate::local::SQLiteStore::open(&database, "session", directory.path(), true, false)
            .unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_initial_binding BEFORE UPDATE OF claude_session ON react_session BEGIN SELECT RAISE(FAIL, 'fixture'); END").unwrap();
    let (service, runtime) = start(
        Box::new(store),
        authority(1),
        ModelApi::AnthropicOAuth,
        false,
    )
    .await;
    assert!(service.claude_session.binding().is_none());
    assert!(!service.messages().await.unwrap().is_empty());
    let saved: Option<String> = connection
        .query_row(
            "SELECT claude_session FROM react_session WHERE id = 'session'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(saved.is_none());
    connection
        .execute_batch("DROP TRIGGER fail_initial_binding")
        .unwrap();
    service
        .reconnect_claude("recover-store".into(), reconnect(None, 1))
        .await
        .unwrap();
    assert_eq!(
        service.claude_session.binding(),
        Some(ClaudeSessionBinding::new(identity(1)))
    );
    finish(&service, runtime).await;
}
