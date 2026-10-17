use super::super::{
    stream::{Decoder, Framer},
    wire::{xxhash64, PreparedRequest, ToolNames},
};
use super::*;
use crate::{
    providers::{responses::json_to_struct, AnthropicMessages, AnthropicMessagesConfig},
    tools::ToolDefinition,
};
use futures_util::TryStreamExt;
use mirror_protocol::{
    chat_record, AssistantMessage, FunctionCall, InferenceConfig, SystemMessage, ToolCall,
    ToolMessage, UserMessage,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

const ACCOUNT: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const INSTALLATION: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
const SESSION: &str = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
const CANARY: &str = "SYNTHETIC-SECRET-CANARY";
fn identity() -> ClaudeIdentityBinding {
    ClaudeIdentityBinding {
        account_id: ACCOUNT.parse().unwrap(),
        organization_id: Uuid::from_u128(2),
    }
}
fn request() -> CompletionRequest {
    CompletionRequest {
        messages: vec![chat_record::Message::User(UserMessage {
            content: "hello".into(),
            ..Default::default()
        })],
        tools: vec![ToolDefinition {
            name: "read".into(),
            description: "Read fixture".into(),
            parameters: serde_json::from_value(json!({"type":"object"})).unwrap(),
        }],
        max_tokens: Some(2048),
        inference: None,
    }
}
fn body(model: &str, request: &CompletionRequest) -> Result<Vec<u8>, ClaudeError> {
    PreparedRequest::new(model, request)?.body(
        INSTALLATION.parse().unwrap(),
        ACCOUNT.parse().unwrap(),
        SESSION,
    )
}
fn fixture() -> Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../diagnostics/claude_oauth/tests/fixtures/wire.json"
    )))
    .unwrap()
}
fn fixture_request(f: &Value) -> CompletionRequest {
    let mut r = request();
    r.messages = vec![chat_record::Message::System(SystemMessage {
        content: "Run only the requested small Mirror protocol diagnostic.".into(),
    })];
    for m in f["canonical_messages"].as_array().unwrap() {
        r.messages.push(match m["role"].as_str().unwrap() {
            "user" if m["content"].is_string() => chat_record::Message::User(UserMessage {
                content: m["content"].as_str().unwrap().into(),
                ..Default::default()
            }),
            "user" => {
                let b = &m["content"][0];
                chat_record::Message::Tool(ToolMessage {
                    content: b["content"].as_str().unwrap().into(),
                    tool_call_id: b["tool_use_id"].as_str().unwrap().into(),
                    name: None,
                })
            }
            "assistant" => {
                let mut a = AssistantMessage::default();
                for b in m["content"].as_array().unwrap() {
                    if b["type"] == "tool_use" {
                        a.tool_calls.push(ToolCall {
                            id: b["id"].as_str().unwrap().into(),
                            r#type: "function".into(),
                            function: Some(FunctionCall {
                                name: b["name"].as_str().unwrap().into(),
                                arguments: b["input"].to_string(),
                            }),
                            provider_fields: None,
                        });
                    } else {
                        a.reasoning_details.push(json_to_struct(b.clone()).unwrap());
                    }
                }
                chat_record::Message::Assistant(a)
            }
            _ => unreachable!(),
        });
    }
    r.tools = f["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| ToolDefinition {
            name: t["name"].as_str().unwrap().into(),
            description: t["description"].as_str().unwrap().into(),
            parameters: serde_json::from_value(t["input_schema"].clone()).unwrap(),
        })
        .collect();
    r.inference = Some(InferenceConfig {
        extra_body: Some(json!({"tool_choice": f["tool_choice"]}).to_string()),
        ..Default::default()
    });
    r
}
#[test]
fn exact_bun_golden_bytes_full_seed_and_untouched_canonical_data() {
    let f = fixture();
    let r = fixture_request(&f);
    let before = r.clone();
    let actual = body("claude-opus-4-8", &r).unwrap();
    assert_eq!(String::from_utf8(actual).unwrap(), f["request_utf8"]);
    assert_eq!(r, before);
    for v in f["xxhash64_bun_vectors"].as_array().unwrap() {
        let s = v["input_hex"].as_str().unwrap();
        let bytes: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(
            format!("{:016x}", xxhash64(&bytes, v["seed"].as_u64().unwrap())),
            v["expected"]
        );
    }
}
#[test]
fn unicode_seed_uses_utf16_code_units_and_exact_user_cch_is_untouched() {
    // Independent Bun fixture's astral prompt differentiates UTF-16 from chars.
    let mut r = fixture_request(&fixture());
    if let chat_record::Message::User(u) = &mut r.messages[1] {
        u.content = "🪞 π Unicode prompt: cch=00000 remains user content.".into();
    }
    let v: Value = serde_json::from_slice(&body("claude-opus-4-8", &r).unwrap()).unwrap();
    assert!(v["system"][0]["text"]
        .as_str()
        .unwrap()
        .contains("2.1.280.947"));
    assert!(v["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("cch=00000"));
}
#[test]
fn capability_presets_are_explicit_and_forced_tools_disable_thinking() {
    for model in ["claude-opus-4-8", "claude-sonnet-4-6", "claude-sonnet-5"] {
        let cap = claude_model_capabilities(model).unwrap();
        for effort in cap.reasoning_efforts {
            let mut r = request();
            r.inference = Some(InferenceConfig {
                reasoning_effort: Some((*effort).into()),
                ..Default::default()
            });
            let v: Value = serde_json::from_slice(&body(model, &r).unwrap()).unwrap();
            assert_eq!(v.get("thinking").is_some(), *effort != "none");
            if *effort != "none" {
                assert_eq!(
                    v["thinking"].get("display").is_some(),
                    cap.summarized_display
                );
            }
        }
        let mut r = request();
        r.inference = Some(InferenceConfig {
            extra_body: Some(r#"{"tool_choice":{"type":"tool","name":"read"}}"#.into()),
            ..Default::default()
        });
        let v: Value = serde_json::from_slice(&body(model, &r).unwrap()).unwrap();
        assert!(v.get("thinking").is_none());
        assert!(v.get("context_management").is_none());
        assert_eq!(v["tool_choice"]["name"], "_read");
    }
    assert!(
        !claude_model_capabilities("claude-sonnet-5")
            .unwrap()
            .signed_reasoning_verified
    );
    assert!(body("unknown", &request()).is_err());
    for effort in ["medium", "high", "xhigh", "max", "512"] {
        let mut r = request();
        r.inference = Some(InferenceConfig {
            reasoning_effort: Some(effort.into()),
            ..Default::default()
        });
        assert!(body("claude-opus-4-8", &r).is_err());
    }
}
#[test]
fn controlled_fields_rejected_and_tools_remain_bijective() {
    for extra in [
        json!({"system":[]}),
        json!({"metadata":{}}),
        json!({"messages":[]}),
        json!({"tools":[]}),
        json!({"stream":false}),
        json!({"speed":"fast"}),
        json!({"service_tier":"auto"}),
        json!({"model":"other"}),
        json!({"thinking":{}}),
        json!({"cache_control":{}}),
    ] {
        let mut r = request();
        r.inference = Some(InferenceConfig {
            extra_body: Some(extra.to_string()),
            ..Default::default()
        });
        assert!(body("claude-opus-4-8", &r).is_err());
    }
    let mut r = request();
    for name in ["_read", "mcp__extension__read"] {
        let mut tool = r.tools[0].clone();
        tool.name = name.into();
        r.tools.push(tool);
    }
    let map = ToolNames::new(&r).unwrap();
    for tool in &r.tools {
        assert_eq!(
            map.decode(map.encode(&tool.name).unwrap()).unwrap(),
            tool.name
        );
    }
    assert_eq!(map.encode("_read").unwrap(), "__read");
    assert!(map.decode("_unregistered").is_err());
    r.tools.push(r.tools[0].clone());
    assert!(ToolNames::new(&r).is_err());
    r.tools.pop();
    for bad in ["x".repeat(64), "extension.bad".into(), "".into()] {
        r.tools[0].name = bad;
        assert!(ToolNames::new(&r).is_err());
    }
}
fn events(blocks: Vec<Value>, reason: &str) -> Vec<Value> {
    let mut out = vec![json!({"type":"message_start","message":{"usage":{"input_tokens":12}}})];
    for (i, b) in blocks.into_iter().enumerate() {
        out.push(json!({"type":"content_block_start","index":i,"content_block":b}));
        out.push(json!({"type":"content_block_stop","index":i}));
    }
    out.push(
        json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":3}}),
    );
    out.push(json!({"type":"message_stop"}));
    out
}
fn sse(events: &[Value]) -> String {
    events
        .iter()
        .map(|e| {
            format!(
                "event: {}\r\ndata: {e}\r\n\r\n",
                e["type"].as_str().unwrap()
            )
        })
        .collect()
}
fn decode(
    events: &[Value],
) -> Result<(Vec<StreamEvent>, crate::providers::Completion), ClaudeError> {
    let names = ToolNames::new(&request()).unwrap();
    let mut decoder = Decoder::default();
    let mut callbacks = Vec::new();
    for e in events {
        callbacks.extend(decoder.accept(e.to_string().as_bytes(), &names)?);
    }
    Ok((callbacks, decoder.finish()?))
}
#[test]
fn strict_stream_preserves_signatures_initial_input_ids_and_callback_names() {
    let mut e = events(
        vec![
            json!({"type":"thinking","thinking":"plan","signature":"s".repeat(16384)}),
            json!({"type":"redacted_thinking","data":"r".repeat(16384)}),
            json!({"type":"text","text":"hello"}),
            json!({"type":"tool_use","name":"_read","id":"original-id","input":{"path":"file"}}),
        ],
        "tool_use",
    );
    let (callbacks, c) = decode(&e).unwrap();
    assert_eq!(c.message.tool_calls[0].id, "original-id");
    assert_eq!(
        c.message.tool_calls[0].function.as_ref().unwrap().arguments,
        r#"{"path":"file"}"#
    );
    assert_eq!(c.message.reasoning_details.len(), 2);
    assert_eq!(c.message.content.as_deref(), Some("hello"));
    assert!(callbacks
        .iter()
        .any(|e| matches!(e,StreamEvent::ToolCall(call) if call.name=="read" && call.index==3)));
    e = events(
        vec![json!({"type":"tool_use","name":"_read","id":"zero","input":{}})],
        "tool_use",
    );
    assert_eq!(
        decode(&e).unwrap().1.message.tool_calls[0]
            .function
            .as_ref()
            .unwrap()
            .arguments,
        "{}"
    );
}
#[test]
fn fragmented_json_multiple_tools_and_results_replay_without_id_changes() {
    let mut e = events(
        vec![
            json!({"type":"tool_use","name":"_read","id":"id1","input":{}}),
            json!({"type":"tool_use","name":"_read","id":"id2","input":{}}),
        ],
        "tool_use",
    );
    e.insert(2,json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"p\":"}}));
    e.insert(3,json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"π\"}"}}));
    let (callbacks, c) = decode(&e).unwrap();
    assert_eq!(c.message.tool_calls.len(), 2);
    assert_eq!(
        callbacks
            .iter()
            .filter(|e| matches!(e, StreamEvent::ToolCall(_)))
            .count(),
        5
    );
    let mut r = request();
    r.messages.push(chat_record::Message::Assistant(c.message));
    for id in ["id1", "id2"] {
        r.messages.push(chat_record::Message::Tool(ToolMessage {
            tool_call_id: id.into(),
            content: "result".into(),
            name: None,
        }));
    }
    let v: Value = serde_json::from_slice(&body("claude-opus-4-8", &r).unwrap()).unwrap();
    assert_eq!(v["messages"][1]["content"][0]["id"], "id1");
    assert_eq!(v["messages"][2]["content"][0]["tool_use_id"], "id1");
}
#[test]
fn invalid_order_unsigned_unknown_duplicate_and_truncated_streams_fail() {
    let valid = events(vec![json!({"type":"text","text":"hello"})], "end_turn");
    for cut in 0..valid.len() {
        assert!(decode(&valid[..cut]).is_err());
    }
    let mut cases = vec![
        events(
            vec![json!({"type":"thinking","thinking":"plan","signature":""})],
            "end_turn",
        ),
        events(
            vec![json!({"type":"tool_use","name":"_unknown","id":"id","input":{}})],
            "tool_use",
        ),
        events(
            vec![
                json!({"type":"tool_use","name":"_read","id":"id","input":{}}),
                json!({"type":"tool_use","name":"_read","id":"id","input":{}}),
            ],
            "tool_use",
        ),
    ];
    let mut duplicate = valid.clone();
    duplicate.insert(2, duplicate[1].clone());
    cases.push(duplicate);
    let mut after_stop = valid.clone();
    after_stop.insert(
        3,
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"late"}}),
    );
    cases.push(after_stop);
    let mut missing_stop = valid.clone();
    missing_stop.remove(2);
    cases.push(missing_stop);
    let mut bad_json = events(
        vec![json!({"type":"tool_use","name":"_read","id":"id","input":{}})],
        "tool_use",
    );
    bad_json.insert(2,json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"incomplete\":"}}));
    cases.push(bad_json);
    for case in cases {
        assert!(decode(&case).is_err(), "{case:?}");
    }
}
#[test]
fn framing_handles_split_utf8_crlf_comments_and_bounds() {
    let text = sse(&events(
        vec![json!({"type":"text","text":"π 🪞"})],
        "end_turn",
    ));
    let mut f = Framer::default();
    let mut payloads = Vec::new();
    for byte in text.as_bytes() {
        payloads.extend(f.feed(&[*byte]).unwrap());
    }
    let names = ToolNames::new(&request()).unwrap();
    let mut d = Decoder::default();
    for payload in payloads {
        d.accept(&payload, &names).unwrap();
    }
    assert!(d.finish().is_ok());
    let mut f = Framer::default();
    assert!(f.feed(b":comment\n\n").unwrap().is_empty());
    assert!(f.feed(&vec![b'x'; 2 * 1024 * 1024 + 1]).is_err());
}

struct Authority {
    calls: Mutex<Vec<Option<ClaudeTokenVersion>>>,
    fail_at: Option<usize>,
    pending: bool,
    dropped: Arc<AtomicUsize>,
}
impl Authority {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_at: None,
            pending: false,
            dropped: Arc::new(AtomicUsize::new(0)),
        }
    }
}
struct DropMark(Arc<AtomicUsize>);
impl Drop for DropMark {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl ClaudeAuthorization for Authority {
    fn authorize<'a>(
        &'a self,
        context: ClaudeAuthorizationContext<'a>,
        rejected: Option<ClaudeTokenVersion>,
    ) -> BoxFuture<'a, Result<ClaudeAccessLease, ClaudeError>> {
        async move {
            let _mark = DropMark(self.dropped.clone());
            let attempt = {
                let mut c = self.calls.lock().unwrap();
                c.push(rejected);
                c.len()
            };
            if self.pending {
                std::future::pending::<()>().await;
            }
            if let Some(guard) = context.request_guard {
                guard
                    .verify()
                    .await
                    .map_err(|_| ClaudeError::new(ClaudeErrorKind::RequestCancelled))?;
            }
            if self.fail_at == Some(attempt) {
                return Err(ClaudeError::new(ClaudeErrorKind::LoginRequired));
            }
            Ok(ClaudeAccessLease {
                access_token: ClaudeAccessToken::new(format!("{CANARY}-{attempt}")).unwrap(),
                identity: context.expected_identity.clone(),
                installation_id: INSTALLATION.parse().unwrap(),
                version: ClaudeTokenVersion {
                    generation: Uuid::from_u128(1),
                    revision: attempt as u64,
                },
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
        async { panic!("Messages must not create a second credential path") }.boxed()
    }
}
type Captures = Arc<Mutex<Vec<(String, Vec<u8>)>>>;
async fn server(
    replies: Vec<(u16, Vec<(&'static str, String)>, String)>,
) -> (&'static str, Captures, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint: &'static str = Box::leak(
        format!("http://{}/v1/messages", listener.local_addr().unwrap()).into_boxed_str(),
    );
    let captures = Arc::new(Mutex::new(Vec::new()));
    let saved = captures.clone();
    let task = tokio::spawn(async move {
        for (status, headers, body) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                socket.read_exact(&mut b).await.unwrap();
                header.push(b[0]);
            }
            let header = String::from_utf8(header).unwrap();
            let len = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                        .and_then(|s| s.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut request_body = vec![0; len];
            socket.read_exact(&mut request_body).await.unwrap();
            saved.lock().unwrap().push((header, request_body));
            let extra: String = headers
                .into_iter()
                .map(|(k, v)| format!("{k}: {v}\r\n"))
                .collect();
            let response=format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",body.len());
            socket.write_all(response.as_bytes()).await.unwrap();
        }
    });
    (endpoint, captures, task)
}
fn provider(endpoint: &'static str, authority: Arc<Authority>) -> ClaudeMessages {
    let config = ClaudeMessagesConfig {
        model: "claude-opus-4-8".into(),
        session_id: SESSION.into(),
        binding: ClaudeSessionBinding::new(identity()),
        request_timeout: Some(Duration::from_secs(5)),
    };
    ClaudeMessages {
        config,
        authority,
        client: oauth_client(Some(Duration::from_secs(5)), false).unwrap(),
        endpoint,
    }
}
fn ok_reply() -> (u16, Vec<(&'static str, String)>, String) {
    (
        200,
        vec![("Content-Type", "text/event-stream".into())],
        sse(&events(
            vec![json!({"type":"text","text":"done"})],
            "end_turn",
        )),
    )
}
fn error_reply(status: u16, kind: &str) -> (u16, Vec<(&'static str, String)>, String) {
    (
        status,
        vec![("Content-Type", "application/json".into())],
        json!({"type":"error","error":{"type":kind,"message":CANARY}}).to_string(),
    )
}
#[tokio::test]
async fn sends_exact_golden_body_and_pinned_headers_with_a_synthetic_authority() {
    let (url, captures, server) = server(vec![ok_reply()]).await;
    let auth = Arc::new(Authority::new());
    let p = provider(url, auth.clone());
    let f = fixture();
    let mut r = fixture_request(&f);
    if let chat_record::Message::Assistant(a) = &mut r.messages[2] {
        anthropic_reasoning_provenance("claude-opus-4-8", AnthropicBillingMode::Subscription)
            .insert_into(&mut a.provider_fields);
    }
    let events = p.stream(r).try_collect::<Vec<_>>().await.unwrap();
    server.await.unwrap();
    assert!(matches!(events.last(), Some(StreamEvent::Final(_))));
    let c = captures.lock().unwrap();
    assert_eq!(c[0].1, f["request_utf8"].as_str().unwrap().as_bytes());
    let h = c[0].0.to_ascii_lowercase();
    assert!(h.contains("authorization: bearer synthetic-secret-canary-1"));
    assert!(!h.contains("x-api-key"));
    assert!(h.contains("x-claude-code-session-id:"));
    assert!(h.contains("x-stainless-package-version: 0.112.1"));
    assert!(h.contains(&format!(
        "anthropic-beta: {}",
        profile::MESSAGE_BETAS.join(",")
    )));
    assert_eq!(auth.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn one_401_recovery_uses_rejected_version_and_a_fresh_lease() {
    let (url, captures, server) =
        server(vec![error_reply(401, "authentication_error"), ok_reply()]).await;
    let auth = Arc::new(Authority::new());
    let p = provider(url, auth.clone());
    assert!(p.stream(request()).try_collect::<Vec<_>>().await.is_ok());
    server.await.unwrap();
    let calls = auth.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls[0].is_none());
    assert_eq!(calls[1].unwrap().revision, 1);
    let c = captures.lock().unwrap();
    assert!(c[0].0.contains(&format!("{CANARY}-1")));
    assert!(c[1].0.contains(&format!("{CANARY}-2")));
    assert_eq!(c[0].1, c[1].1);
}
#[tokio::test]
async fn repeated_401_and_partial_output_are_terminal_and_redacted() {
    let (url, captures, server) = server(vec![
        error_reply(401, "authentication_error"),
        error_reply(401, "authentication_error"),
    ])
    .await;
    let auth = Arc::new(Authority::new());
    let p = provider(url, auth.clone());
    let error = p
        .stream(request())
        .try_collect::<Vec<_>>()
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(matches!(&error,LlmError::Claude(e) if e.kind==ClaudeErrorKind::LoginExpired));
    assert!(!format!("{error:?} {error}").contains(CANARY));
    assert_eq!(captures.lock().unwrap().len(), 2);
    let e = events(
        vec![json!({"type":"tool_use","name":"_read","id":"never-execute","input":{}})],
        "tool_use",
    );
    let (url, captures, server) = self::server(vec![(
        200,
        vec![("Content-Type", "text/event-stream".into())],
        sse(&e[..2]),
    )])
    .await;
    let p = provider(url, Arc::new(Authority::new()));
    let output = p.stream(request()).collect::<Vec<_>>().await;
    server.await.unwrap();
    assert!(output
        .iter()
        .any(|e| matches!(e, Ok(StreamEvent::ToolCall(_)))));
    assert!(!output
        .iter()
        .any(|e| matches!(e, Ok(StreamEvent::Final(_)))));
    assert!(
        matches!(output.last(),Some(Err(LlmError::Claude(e))) if e.kind==ClaudeErrorKind::PartialStreamError)
    );
    assert_eq!(captures.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn shared_transient_budget_is_bounded_and_reauthorizes() {
    let (url, captures, server) = server(vec![
        error_reply(529, "overloaded_error"),
        error_reply(401, "authentication_error"),
        error_reply(529, "overloaded_error"),
        error_reply(529, "overloaded_error"),
    ])
    .await;
    let auth = Arc::new(Authority::new());
    let p = provider(url, auth.clone());
    let error = p
        .stream(request())
        .try_collect::<Vec<_>>()
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(
        matches!(error,LlmError::Claude(e) if e.kind==ClaudeErrorKind::AuthTemporarilyUnavailable && e.retry==ClaudeRetryDisposition::Terminal)
    );
    assert_eq!(auth.calls.lock().unwrap().len(), 4);
    assert_eq!(captures.lock().unwrap().len(), 4);
}
#[tokio::test]
async fn invalid_options_do_not_resolve_tokens_and_authority_failure_prevents_retry() {
    let auth = Arc::new(Authority::new());
    let p = provider("http://127.0.0.1:1", auth.clone());
    let mut r = request();
    r.inference = Some(InferenceConfig {
        extra_body: Some(r#"{"metadata":{"secret":"bad"}}"#.into()),
        ..Default::default()
    });
    assert!(p.stream(r).try_collect::<Vec<_>>().await.is_err());
    assert!(auth.calls.lock().unwrap().is_empty());
    let (url, captures, server) = server(vec![error_reply(401, "authentication_error")]).await;
    let mut authority = Authority::new();
    authority.fail_at = Some(2);
    let auth = Arc::new(authority);
    let p = provider(url, auth.clone());
    assert!(
        matches!(p.stream(request()).try_collect::<Vec<_>>().await.unwrap_err(),LlmError::Claude(e) if e.kind==ClaudeErrorKind::LoginRequired)
    );
    server.await.unwrap();
    assert_eq!(captures.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn cancellation_and_drop_stop_pending_authorization_without_detached_work() {
    let mut a = Authority::new();
    a.pending = true;
    let auth = Arc::new(a);
    let p = provider("http://127.0.0.1:1", auth.clone());
    let error = p
        .stream_guarded(request(), None, async {}.boxed())
        .try_collect::<Vec<_>>()
        .await
        .unwrap_err();
    assert!(matches!(error,LlmError::Claude(e) if e.kind==ClaudeErrorKind::RequestCancelled));
    assert!(auth.calls.lock().unwrap().is_empty());
    {
        let mut stream = Box::pin(p.stream(request()));
        assert!(
            tokio::time::timeout(Duration::from_millis(5), stream.next())
                .await
                .is_err()
        );
    }
    assert_eq!(auth.dropped.load(Ordering::SeqCst), 1);
}
#[test]
fn classified_errors_and_backoff_metadata_never_include_provider_messages() {
    for (status, kind, expected) in [
        (403, "permission_error", ClaudeErrorKind::AccessDenied),
        (404, "not_found_error", ClaudeErrorKind::ModelUnavailable),
        (429, "rate_limit_error", ClaudeErrorKind::RateLimited),
        (
            400,
            "invalid_request_error",
            ClaudeErrorKind::CompatibilityError,
        ),
    ] {
        let error = classify_error(
            Some(status),
            &HeaderMap::new(),
            &json!({"error":{"type":kind,"message":CANARY}}),
        );
        assert_eq!(error.kind, expected);
        assert!(!serde_json::to_string(&error).unwrap().contains(CANARY));
    }
    let mut headers = HeaderMap::new();
    headers.insert("retry-after", HeaderValue::from_static("120"));
    let error = classify_error(
        Some(529),
        &headers,
        &json!({"error":{"type":"overloaded_error"}}),
    );
    assert!(error.details.retry_at.is_some());
    assert!(RetryBudget::default().delay(&error).is_none());
}
#[tokio::test]
async fn redirects_never_forward_the_grant() {
    let (destination, captures, mut target) = server(vec![ok_reply()]).await;
    let (url, captures_first, server) = server(vec![(
        307,
        vec![("Location", destination.into())],
        String::new(),
    )])
    .await;
    let p = provider(url, Arc::new(Authority::new()));
    assert!(p.stream(request()).try_collect::<Vec<_>>().await.is_err());
    server.await.unwrap();
    assert_eq!(captures_first.lock().unwrap().len(), 1);
    assert!(captures.lock().unwrap().is_empty());
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut target)
        .await
        .is_err());
    target.abort();
    let _ = target.await;
}

#[test]
fn normalization_is_symmetric_preserves_canonical_history_and_reports_reasons() {
    let detail = json_to_struct(
        json!({"type":"thinking","thinking":"readable plan","signature":"unchanged"}),
    )
    .unwrap();
    for target_mode in [
        AnthropicBillingMode::ApiKey,
        AnthropicBillingMode::Subscription,
    ] {
        let target = anthropic_reasoning_provenance("claude-opus-4-8", target_mode);
        for (source, expected) in [
            (None, Some(HistoryNormalizationReason::UnknownProvenance)),
            (Some(target.clone()), None),
            (
                Some(anthropic_reasoning_provenance(
                    "claude-sonnet-4-6",
                    target_mode,
                )),
                Some(HistoryNormalizationReason::ModelChanged),
            ),
            (
                Some(anthropic_reasoning_provenance(
                    "claude-opus-4-8",
                    if target_mode == AnthropicBillingMode::ApiKey {
                        AnthropicBillingMode::Subscription
                    } else {
                        AnthropicBillingMode::ApiKey
                    },
                )),
                Some(HistoryNormalizationReason::BillingModeChanged),
            ),
        ] {
            let mut a = AssistantMessage {
                content: Some("visible answer".into()),
                reasoning_content: Some("readable plan".into()),
                reasoning_details: vec![detail.clone()],
                ..Default::default()
            };
            if let Some(source) = source {
                source.insert_into(&mut a.provider_fields);
            }
            let canonical = vec![chat_record::Message::Assistant(a.clone())];
            let before = canonical.clone();
            let (normalized, notices) = normalize_anthropic_history(&canonical, &target);
            assert_eq!(canonical, before);
            let chat_record::Message::Assistant(normalized) = &normalized[0] else {
                unreachable!()
            };
            assert_eq!(normalized.content, a.content);
            assert_eq!(normalized.reasoning_content, a.reasoning_content);
            if let Some(reason) = expected {
                assert!(normalized.reasoning_details.is_empty());
                assert_eq!(notices[0].reason, reason);
            } else {
                assert_eq!(normalized.reasoning_details, a.reasoning_details);
                assert!(notices.is_empty());
            }
        }
    }
}
#[tokio::test]
async fn api_key_wire_keeps_tools_headers_extra_options_and_normalizes_subscription_history() {
    let (url, captures, server) = server(vec![ok_reply()]).await;
    let base = url.strip_suffix("/v1/messages").unwrap();
    let api = AnthropicMessages::new(
        AnthropicMessagesConfig {
            model: "claude-opus-4-8".into(),
            api_key: CANARY.into(),
            base_url: base.into(),
        },
        crate::providers::http_client(Some(Duration::from_secs(5))).unwrap(),
    );
    let mut r = request();
    r.messages.insert(
        0,
        chat_record::Message::System(SystemMessage {
            content: "user system instructions".into(),
        }),
    );
    let mut a = AssistantMessage {
        content: Some("prior visible text".into()),
        reasoning_details: vec![json_to_struct(
            json!({"type":"thinking","thinking":"plan","signature":"signed"}),
        )
        .unwrap()],
        ..Default::default()
    };
    anthropic_reasoning_provenance("claude-opus-4-8", AnthropicBillingMode::Subscription)
        .insert_into(&mut a.provider_fields);
    r.messages.push(chat_record::Message::Assistant(a));
    r.inference = Some(InferenceConfig {
        extra_body: Some(r#"{"max_tokens":123,"custom_api_option":true}"#.into()),
        ..Default::default()
    });
    let output = api.stream(r).try_collect::<Vec<_>>().await.unwrap();
    server.await.unwrap();
    assert!(
        matches!(output.first(),Some(StreamEvent::Notice(n)) if n.reason==HistoryNormalizationReason::BillingModeChanged)
    );
    let c = captures.lock().unwrap();
    let v: Value = serde_json::from_slice(&c[0].1).unwrap();
    assert_eq!(v["max_tokens"], 123);
    assert_eq!(v["custom_api_option"], true);
    assert_eq!(v["tools"][0]["name"], "read");
    assert_eq!(v["system"][0]["text"], "user system instructions");
    assert_eq!(
        v["messages"][1]["content"],
        json!([{"type":"text","text":"prior visible text"}])
    );
    assert!(!String::from_utf8_lossy(&c[0].1).contains("x-anthropic-billing-header"));
    assert!(c[0].0.contains(&format!("x-api-key: {CANARY}")));
    assert!(!c[0].0.to_ascii_lowercase().contains("authorization:"));
    let Some(StreamEvent::Final(c)) = output.last() else {
        panic!("final")
    };
    assert_eq!(
        ReasoningProvenance::from_fields(c.message.provider_fields.as_ref())
            .unwrap()
            .billing_mode,
        AnthropicBillingMode::ApiKey
    );
}
#[test]
fn quota_rules_do_not_guess_scope_reset_or_extra_usage_from_counters() {
    let payload = json!({"error":{"type":"rate_limit_error","message":CANARY}});
    let mut h = HeaderMap::new();
    h.insert(
        "anthropic-ratelimit-unified-5h-utilization",
        HeaderValue::from_static("1.1"),
    );
    h.insert(
        "anthropic-ratelimit-unified-5h-reset",
        HeaderValue::from_static("1999999999"),
    );
    h.insert(
        "anthropic-ratelimit-unified-overage-status",
        HeaderValue::from_static("allowed"),
    );
    h.insert("retry-after", HeaderValue::from_static("120"));
    let e = classify_error(Some(429), &h, &payload);
    assert_eq!(e.kind, ClaudeErrorKind::RateLimited);
    assert_eq!(e.retry, ClaudeRetryDisposition::Terminal);
    h.insert(
        "anthropic-ratelimit-unified-status",
        HeaderValue::from_static("rejected"),
    );
    let e = classify_error(Some(429), &h, &payload);
    assert_eq!(e.kind, ClaudeErrorKind::SubscriptionLimited);
    assert_eq!(e.details.limit_scope, None);
    assert_eq!(e.details.resets_at, None);
    assert_eq!(e.details.extra_usage, None);
    let e = classify_error(
        Some(429),
        &HeaderMap::new(),
        &json!({"error":{"type":"rate_limit_error","details":{"error_code":"enforced_spend_limit_reached"}}}),
    );
    assert_eq!(e.kind, ClaudeErrorKind::SubscriptionLimited);
    assert_eq!(e.retry, ClaudeRetryDisposition::Terminal);
}
#[tokio::test]
async fn all_429_are_terminal_even_with_short_retry_after() {
    for rejected in [false, true] {
        let mut reply = error_reply(429, "rate_limit_error");
        reply.1.push(("Retry-After", "1".into()));
        if rejected {
            reply
                .1
                .push(("anthropic-ratelimit-unified-status", "rejected".into()));
        }
        let (url, captures, server) = server(vec![reply]).await;
        let auth = Arc::new(Authority::new());
        let p = provider(url, auth.clone());
        assert!(p.stream(request()).try_collect::<Vec<_>>().await.is_err());
        server.await.unwrap();
        assert_eq!(auth.calls.lock().unwrap().len(), 1);
        assert_eq!(captures.lock().unwrap().len(), 1);
    }
}
struct Guard {
    calls: AtomicUsize,
    reject_at: usize,
}
impl ModelRequestGuard for Guard {
    fn verify(&self) -> BoxFuture<'_, Result<(), LlmError>> {
        async move {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n >= self.reject_at {
                Err(LlmError::Request("stopped".into()))
            } else {
                Ok(())
            }
        }
        .boxed()
    }
}
#[tokio::test]
async fn guard_is_rechecked_after_backoff_before_a_second_network_dispatch() {
    let (url, captures, server) = server(vec![error_reply(529, "overloaded_error")]).await;
    let auth = Arc::new(Authority::new());
    let p = provider(url, auth.clone());
    let guard = Guard {
        calls: AtomicUsize::new(0),
        reject_at: 2,
    };
    let error = p
        .stream_guarded(request(), Some(&guard), std::future::pending().boxed())
        .try_collect::<Vec<_>>()
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(matches!(error,LlmError::Claude(e) if e.kind==ClaudeErrorKind::RequestCancelled));
    assert_eq!(guard.calls.load(Ordering::SeqCst), 2);
    assert_eq!(captures.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn connection_outcome_unknown_is_not_replayed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url: &'static str = Box::leak(
        format!("http://{}/v1/messages", listener.local_addr().unwrap()).into_boxed_str(),
    );
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        drop(socket);
    });
    let auth = Arc::new(Authority::new());
    let p = provider(url, auth.clone());
    assert!(
        matches!(p.stream(request()).try_collect::<Vec<_>>().await.unwrap_err(),LlmError::Claude(e) if e.kind==ClaudeErrorKind::AuthTemporarilyUnavailable)
    );
    server.await.unwrap();
    assert_eq!(auth.calls.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn cancellation_interrupts_an_http_wait() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url: &'static str = Box::leak(
        format!("http://{}/v1/messages", listener.local_addr().unwrap()).into_boxed_str(),
    );
    let (accepted, ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let _ = accepted.send(());
        let mut bytes = Vec::new();
        let _ = socket.read_to_end(&mut bytes).await;
    });
    let auth = Arc::new(Authority::new());
    let p = provider(url, auth.clone());
    let error = p
        .stream_guarded(
            request(),
            None,
            async {
                let _ = ready.await;
            }
            .boxed(),
        )
        .try_collect::<Vec<_>>()
        .await
        .unwrap_err();
    assert!(matches!(error,LlmError::Claude(e) if e.kind==ClaudeErrorKind::RequestCancelled));
    server.await.unwrap();
    assert_eq!(auth.calls.lock().unwrap().len(), 1);
}

#[test]
fn normalization_omits_only_turns_made_empty_by_reasoning_removal() {
    let target =
        anthropic_reasoning_provenance("claude-opus-4-8", AnthropicBillingMode::Subscription);
    let blank = chat_record::Message::Assistant(AssistantMessage::default());
    let opaque = chat_record::Message::Assistant(AssistantMessage {
        reasoning_details: vec![json_to_struct(
            json!({"type":"redacted_thinking","data":"opaque"}),
        )
        .unwrap()],
        ..Default::default()
    });
    let canonical = vec![blank.clone(), opaque.clone()];
    let (view, notices) = normalize_anthropic_history(&canonical, &target);
    assert_eq!(view, vec![blank]);
    assert_eq!(canonical[1], opaque);
    assert_eq!(notices[0].affected_messages, 1);
}
#[test]
fn cache_anchors_have_no_empty_system_block_and_survive_token_rotation() {
    let r = request();
    let v: Value = serde_json::from_slice(&body("claude-opus-4-8", &r).unwrap()).unwrap();
    assert_eq!(v["system"].as_array().unwrap().len(), 2);
    assert_eq!(v["system"][1]["cache_control"], json!({"type":"ephemeral"}));
    assert_eq!(v["tools"][0]["cache_control"], json!({"type":"ephemeral"}));
    assert!(v["system"]
        .as_array()
        .unwrap()
        .iter()
        .all(|b| !b["text"].as_str().unwrap().is_empty()));
}
#[test]
fn historical_inactive_tool_can_replay_but_cannot_be_called() {
    let mut r = request();
    r.messages
        .push(chat_record::Message::Assistant(AssistantMessage {
            tool_calls: vec![ToolCall {
                id: "old".into(),
                r#type: "function".into(),
                function: Some(FunctionCall {
                    name: "disabled__tool".into(),
                    arguments: "{}".into(),
                }),
                provider_fields: None,
            }],
            ..Default::default()
        }));
    let names = ToolNames::new(&r).unwrap();
    assert_eq!(names.encode("disabled__tool").unwrap(), "_disabled__tool");
    assert!(names.decode("_disabled__tool").is_err());
    assert!(body("claude-opus-4-8", &r).is_ok());
}
#[tokio::test]
async fn cancellation_interrupts_backoff_without_another_lease() {
    let (url, captures, server) = server(vec![error_reply(529, "overloaded_error")]).await;
    let auth = Arc::new(Authority::new());
    let p = provider(url, auth.clone());
    let error = p
        .stream_guarded(
            request(),
            None,
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            .boxed(),
        )
        .try_collect::<Vec<_>>()
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(matches!(error,LlmError::Claude(e) if e.kind==ClaudeErrorKind::RequestCancelled));
    assert_eq!(auth.calls.lock().unwrap().len(), 1);
    assert_eq!(captures.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn sse_overload_can_retry_only_before_exposure() {
    let overloaded = json!({"type":"error","error":{"type":"overloaded_error","message":CANARY}});
    let mut before_output = events(vec![json!({"type":"text","text":""})], "end_turn");
    before_output.truncate(2);
    before_output.push(overloaded.clone());
    let (url, captures, server) = server(vec![
        (
            200,
            vec![("Content-Type", "text/event-stream".into())],
            sse(&before_output),
        ),
        ok_reply(),
    ])
    .await;
    let p = provider(url, Arc::new(Authority::new()));
    assert!(p.stream(request()).try_collect::<Vec<_>>().await.is_ok());
    server.await.unwrap();
    assert_eq!(captures.lock().unwrap().len(), 2);
    for (block, delta) in [
        (json!({"type":"text","text":"partial"}), None),
        (
            json!({"type":"thinking","thinking":"","signature":"opaque signature"}),
            None,
        ),
        (
            json!({"type":"redacted_thinking","data":"opaque data"}),
            None,
        ),
        (
            json!({"type":"thinking","thinking":""}),
            Some(
                json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"opaque signature"}}),
            ),
        ),
    ] {
        let mut partial = events(vec![block], "end_turn");
        partial.truncate(2);
        partial.extend(delta);
        partial.push(overloaded.clone());
        let (url, captures, server) = self::server(vec![(
            200,
            vec![("Content-Type", "text/event-stream".into())],
            sse(&partial),
        )])
        .await;
        let auth = Arc::new(Authority::new());
        let p = provider(url, auth.clone());
        let output = p.stream(request()).collect::<Vec<_>>().await;
        server.await.unwrap();
        assert!(
            matches!(output.last(),Some(Err(LlmError::Claude(e))) if e.kind==ClaudeErrorKind::PartialStreamError)
        );
        assert!(!output
            .iter()
            .any(|e| matches!(e, Ok(StreamEvent::Final(_)))));
        assert_eq!(auth.calls.lock().unwrap().len(), 1);
        assert_eq!(captures.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn conflicting_request_binding_stops_before_authorization() {
    let auth = Arc::new(Authority::new());
    let p = provider("http://127.0.0.1:1", auth.clone());
    let mut binding = ClaudeSessionBinding::new(identity());
    binding.identity.organization_id = Uuid::from_u128(999);
    let mut r = request();
    r.inference = Some(InferenceConfig {
        claude_session: Some((&binding).into()),
        ..Default::default()
    });
    assert!(p.stream(r).try_collect::<Vec<_>>().await.is_err());
    assert!(auth.calls.lock().unwrap().is_empty());
}
#[test]
fn tool_callbacks_include_initial_and_empty_object_arguments() {
    for (input, expected) in [(json!({"p":"file"}), r#"{"p":"file"}"#), (json!({}), "{}")] {
        let (callbacks, c) = decode(&events(
            vec![json!({"type":"tool_use","id":"original","name":"_read","input":input})],
            "tool_use",
        ))
        .unwrap();
        let arguments: String = callbacks
            .iter()
            .filter_map(|event| {
                if let StreamEvent::ToolCall(call) = event {
                    Some(call.arguments.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(arguments, expected);
        assert_eq!(c.message.tool_calls[0].id, "original");
    }
}

#[tokio::test]
async fn proxy_environment_cannot_capture_even_a_synthetic_grant() {
    if std::env::var_os("MIRROR_CLAUDE_PROXY_ISOLATION_TEST").is_none() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args(["--exact","providers::claude::oauth::tests::proxy_environment_cannot_capture_even_a_synthetic_grant"])
            .env("MIRROR_CLAUDE_PROXY_ISOLATION_TEST","1");
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            child.env(name, "http://127.0.0.1:1");
        }
        child.env("NO_PROXY", "").env("no_proxy", "");
        let result = child.output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let (url, captures, server) = server(vec![ok_reply()]).await;
    let p = provider(url, Arc::new(Authority::new()));
    assert!(p.stream(request()).try_collect::<Vec<_>>().await.is_ok());
    server.await.unwrap();
    assert_eq!(captures.lock().unwrap().len(), 1);
}
#[test]
fn max_token_stop_never_finalizes_tools_from_a_truncated_response() {
    assert!(decode(&events(
        vec![json!({"type":"tool_use","id":"never-execute","name":"_read","input":{}})],
        "max_tokens"
    ))
    .is_err());
    let (_, completion) = decode(&events(
        vec![json!({"type":"text","text":"partial text"})],
        "max_tokens",
    ))
    .unwrap();
    assert!(completion.truncated);
    assert!(completion.message.tool_calls.is_empty());
}

#[test]
fn invalid_or_unrepresentable_retry_hints_never_become_short_default_retries() {
    for hint in ["18446744073709551615", "9223372036854775807", "not-a-time"] {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_str(hint).unwrap());
        let error = classify_error(
            Some(529),
            &headers,
            &json!({"error":{"type":"overloaded_error","message":CANARY}}),
        );
        assert_eq!(error.retry, ClaudeRetryDisposition::Terminal);
        assert!(error.details.retry_at.is_none());
        assert!(!serde_json::to_string(&error).unwrap().contains(CANARY));
    }
}
#[tokio::test]
async fn an_auth_retry_also_honors_the_shared_backoff_budget() {
    let mut reply = error_reply(401, "authentication_error");
    reply.1.push(("Retry-After", "120".into()));
    let (url, captures, server) = server(vec![reply]).await;
    let auth = Arc::new(Authority::new());
    let p = provider(url, auth.clone());
    let error = p
        .stream(request())
        .try_collect::<Vec<_>>()
        .await
        .unwrap_err();
    server.await.unwrap();
    assert!(
        matches!(error,LlmError::Claude(e) if e.kind==ClaudeErrorKind::LoginExpired && e.details.retry_at.is_some())
    );
    assert_eq!(captures.lock().unwrap().len(), 1);
    assert_eq!(auth.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn custom_inference_endpoint_is_actionable_and_rejected_before_tokens() {
    let auth = Arc::new(Authority::new());
    let p = provider("http://127.0.0.1:1", auth.clone());
    let mut r = request();
    r.inference = Some(InferenceConfig {
        base_url: Some("https://untrusted.invalid".into()),
        ..Default::default()
    });
    assert!(
        matches!(p.stream(r).try_collect::<Vec<_>>().await.unwrap_err(),LlmError::Claude(e) if e.details.configuration==Some(ClaudeConfigurationError::EndpointNotAllowed))
    );
    assert!(auth.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unrepresentable_upstream_interleavings_fail_without_final_or_retry() {
    let thinking = json!({"type":"thinking","thinking":"late plan","signature":"unchanged"});
    let redacted = json!({"type":"redacted_thinking","data":"unchanged opaque block"});
    let text = json!({"type":"text","text":"already visible"});
    let tool = json!({"type":"tool_use","id":"do-not-execute","name":"_read","input":{}});
    for (blocks, expected) in [
        (
            vec![text.clone(), thinking.clone()],
            ClaudeErrorKind::PartialStreamError,
        ),
        (
            vec![text.clone(), redacted.clone()],
            ClaudeErrorKind::PartialStreamError,
        ),
        (
            vec![tool.clone(), thinking.clone()],
            ClaudeErrorKind::PartialStreamError,
        ),
        (
            vec![tool.clone(), redacted],
            ClaudeErrorKind::PartialStreamError,
        ),
        (vec![tool, text], ClaudeErrorKind::PartialStreamError),
        (
            vec![json!({"type":"text","text":""}), thinking],
            ClaudeErrorKind::CompatibilityError,
        ),
    ] {
        let reason = if blocks.iter().any(|block| block["type"] == "tool_use") {
            "tool_use"
        } else {
            "end_turn"
        };
        let (url, captures, server) = server(vec![(
            200,
            vec![("Content-Type", "text/event-stream".into())],
            sse(&events(blocks, reason)),
        )])
        .await;
        let auth = Arc::new(Authority::new());
        let p = provider(url, auth.clone());
        let output = p.stream(request()).collect::<Vec<_>>().await;
        server.await.unwrap();
        assert!(
            matches!(output.last(),Some(Err(LlmError::Claude(error))) if error.kind==expected && error.retry==ClaudeRetryDisposition::Terminal)
        );
        assert_eq!(output.iter().filter(|event| event.is_err()).count(), 1);
        assert!(!output
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::Final(_)))));
        assert_eq!(auth.calls.lock().unwrap().len(), 1);
        assert_eq!(captures.lock().unwrap().len(), 1);
        assert_eq!(
            output.iter().any(|event| matches!(
                event,
                Ok(StreamEvent::Content(_) | StreamEvent::ToolCall(_))
            )),
            expected == ClaudeErrorKind::PartialStreamError
        );
    }
}

#[tokio::test]
async fn overlapping_blocks_never_reorder_output_or_finalize_tools() {
    for first in [
        json!({"type":"text","text":""}),
        json!({"type":"thinking","thinking":"","signature":"signed"}),
    ] {
        let delta = if first["type"] == "thinking" {
            json!({"type":"thinking_delta","thinking":"late plan"})
        } else {
            json!({"type":"text_delta","text":"second"})
        };
        let input = vec![
            json!({"type":"message_start","message":{"usage":{}}}),
            json!({"type":"content_block_start","index":0,"content_block":first}),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":"first"}}),
            json!({"type":"content_block_delta","index":0,"delta":delta}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"never-execute","name":"_read","input":{}}}),
            json!({"type":"content_block_stop","index":2}),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
            json!({"type":"message_stop"}),
        ];
        let (url, captures, server) = server(vec![(
            200,
            vec![("Content-Type", "text/event-stream".into())],
            sse(&input),
        )])
        .await;
        let auth = Arc::new(Authority::new());
        let p = provider(url, auth.clone());
        let output = p.stream(request()).collect::<Vec<_>>().await;
        server.await.unwrap();
        assert!(matches!(output.last(), Some(Err(LlmError::Claude(e)))
            if e.kind == ClaudeErrorKind::PartialStreamError && e.retry == ClaudeRetryDisposition::Terminal));
        assert!(!output
            .iter()
            .any(|event| matches!(event, Ok(StreamEvent::Final(_)))));
        assert_eq!(auth.calls.lock().unwrap().len(), 1);
        assert_eq!(captures.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn api_key_model_override_drives_normalization_and_response_provenance() {
    for source_model in ["claude-opus-4-8", "claude-sonnet-4-6"] {
        let response = events(
            vec![
                json!({"type":"thinking","thinking":"new plan","signature":"new signature"}),
                json!({"type":"text","text":"done"}),
            ],
            "end_turn",
        );
        let (url, captures, server) = server(vec![(
            200,
            vec![("Content-Type", "text/event-stream".into())],
            sse(&response),
        )])
        .await;
        let api = AnthropicMessages::new(
            AnthropicMessagesConfig {
                model: "claude-opus-4-8".into(),
                api_key: CANARY.into(),
                base_url: url.strip_suffix("/v1/messages").unwrap().into(),
            },
            crate::providers::http_client(Some(Duration::from_secs(5))).unwrap(),
        );
        let mut r = request();
        let mut history = AssistantMessage {
            content: Some("prior visible answer".into()),
            reasoning_details: vec![json_to_struct(json!({
                "type":"thinking","thinking":"prior plan","signature":"prior signature"
            }))
            .unwrap()],
            ..Default::default()
        };
        anthropic_reasoning_provenance(source_model, AnthropicBillingMode::ApiKey)
            .insert_into(&mut history.provider_fields);
        r.messages.push(chat_record::Message::Assistant(history));
        r.inference = Some(InferenceConfig {
            extra_body: Some(r#"{"model":"claude-sonnet-4-6"}"#.into()),
            ..Default::default()
        });
        let output = api.stream(r).try_collect::<Vec<_>>().await.unwrap();
        server.await.unwrap();
        let captures = captures.lock().unwrap();
        let body: Value = serde_json::from_slice(&captures[0].1).unwrap();
        assert_eq!(body["model"], "claude-sonnet-4-6");
        let compatible = source_model == "claude-sonnet-4-6";
        assert_eq!(
            body["messages"][1]["content"].as_array().unwrap().len(),
            if compatible { 2 } else { 1 }
        );
        assert_eq!(
            output
                .iter()
                .any(|event| matches!(event, StreamEvent::Notice(n)
            if n.reason == HistoryNormalizationReason::ModelChanged)),
            !compatible
        );
        let Some(StreamEvent::Final(completion)) = output.last() else {
            panic!("final")
        };
        assert_eq!(
            ReasoningProvenance::from_fields(completion.message.provider_fields.as_ref())
                .unwrap()
                .model_id,
            "claude-sonnet-4-6"
        );
    }
}

#[tokio::test]
async fn cancellation_after_a_buffered_callback_prevents_tools_and_final() {
    for cancel_after in ["text", "tool", "usage"] {
        let reply = events(
            vec![
                json!({"type":"text","text":"visible before cancellation"}),
                json!({"type":"tool_use","id":"never-execute","name":"_read","input":{}}),
            ],
            "tool_use",
        );
        let (url, captures, server) = server(vec![(
            200,
            vec![("Content-Type", "text/event-stream".into())],
            sse(&reply),
        )])
        .await;
        let auth = Arc::new(Authority::new());
        let p = provider(url, auth.clone());
        let (cancel, cancelled) = tokio::sync::oneshot::channel();
        let mut output = Box::pin(
            p.stream_guarded(
                request(),
                None,
                async {
                    let _ = cancelled.await;
                }
                .boxed(),
            ),
        );
        loop {
            let event = output.next().await.unwrap().unwrap();
            if matches!(
                (cancel_after, &event),
                ("text", StreamEvent::Content(_))
                    | ("tool", StreamEvent::ToolCall(_))
                    | ("usage", StreamEvent::OutputTokens(_))
            ) {
                break;
            }
            assert!(!matches!(event, StreamEvent::Final(_)));
        }
        cancel.send(()).unwrap();
        let remaining = output.collect::<Vec<_>>().await;
        server.await.unwrap();
        assert_eq!(remaining.len(), 1, "after {cancel_after}");
        assert!(
            matches!(remaining.last(), Some(Err(LlmError::Claude(error)))
            if error.kind == ClaudeErrorKind::RequestCancelled)
        );
        assert_eq!(auth.calls.lock().unwrap().len(), 1);
        assert_eq!(captures.lock().unwrap().len(), 1);
    }
}
