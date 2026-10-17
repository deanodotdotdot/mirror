//! Strict OAuth validation around the shared Anthropic completion accumulator.
use super::{wire, ClaudeError, ClaudeErrorKind};
use crate::providers::{anthropic_messages::CompletionState, Completion, StreamEvent};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) fn partial() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::PartialStreamError)
}
const MAX_EVENT: usize = 2 * 1024 * 1024;
const MAX_STREAM: usize = 32 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Framer {
    line: Vec<u8>,
    data: Vec<u8>,
    frame_size: usize,
    total: usize,
}
impl Framer {
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, ClaudeError> {
        self.total = self.total.checked_add(bytes.len()).ok_or_else(partial)?;
        if self.total > MAX_STREAM {
            return Err(partial());
        }
        let mut events = Vec::new();
        for &byte in bytes {
            self.frame_size += 1;
            if self.frame_size > MAX_EVENT {
                return Err(partial());
            }
            if byte != b'\n' {
                self.line.push(byte);
                continue;
            }
            if self.line.last() == Some(&b'\r') {
                self.line.pop();
            }
            if self.line.is_empty() {
                if !self.data.is_empty() {
                    self.data.pop();
                    events.push(std::mem::take(&mut self.data));
                }
                self.frame_size = 0;
            } else if let Some(data) = self.line.strip_prefix(b"data:") {
                let data = data.strip_prefix(b" ").unwrap_or(data);
                self.data.extend_from_slice(data);
                self.data.push(b'\n');
            }
            self.line.clear();
        }
        Ok(events)
    }
}

struct Block {
    kind: String,
    closed: bool,
    signature: String,
    input: Value,
    fragments: String,
}
#[derive(Default)]
pub(super) struct Decoder {
    state: CompletionState,
    blocks: Vec<Block>,
    ids: BTreeSet<String>,
    started: bool,
    pub(super) ended: bool,
    stop_reason: Option<String>,
    pub(super) exposed: bool,
}
impl Decoder {
    pub(super) fn accept(
        &mut self,
        payload: &[u8],
        names: &wire::ToolNames,
    ) -> Result<Vec<StreamEvent>, ClaudeError> {
        let mut value: Value = serde_json::from_slice(payload).map_err(|_| partial())?;
        if self.ended {
            return Err(partial());
        }
        let kind = value["type"].as_str().ok_or_else(partial)?.to_owned();
        match kind.as_str() {
            "ping" => return Ok(Vec::new()),
            "error" => {
                return Err(super::oauth::classify_error(
                    None,
                    &Default::default(),
                    &value,
                ))
            }
            "message_start" if !self.started => {
                self.started = true;
            }
            "content_block_start" if self.started && self.stop_reason.is_none() => {
                let index = index(&value)?;
                if index != self.blocks.len() {
                    return Err(partial());
                }
                let block = &mut value["content_block"];
                let kind = block["type"].as_str().ok_or_else(partial)?.to_owned();
                let mut signature = String::new();
                let mut input = Value::Null;
                match kind.as_str() {
                    "text" => {
                        if !block["text"].is_string() {
                            return Err(partial());
                        }
                    }
                    "thinking" => {
                        if !block["thinking"].is_string() {
                            return Err(partial());
                        }
                        signature = block
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into();
                    }
                    "redacted_thinking" => {
                        if block["data"].as_str().is_none_or(str::is_empty) {
                            return Err(partial());
                        }
                    }
                    "tool_use" => {
                        let id = block["id"]
                            .as_str()
                            .filter(|s| !s.is_empty() && s.len() <= 1024)
                            .ok_or_else(partial)?;
                        if !self.ids.insert(id.into()) {
                            return Err(partial());
                        }
                        let canonical =
                            names.decode(block["name"].as_str().ok_or_else(partial)?)?;
                        block["name"] = Value::String(canonical.into());
                        input = block["input"].clone();
                        if !input.is_object() {
                            return Err(partial());
                        }
                    }
                    _ => return Err(partial()),
                }
                // Mirror's canonical AssistantMessage stores reasoning, visible
                // text and tools separately. Reject an otherwise valid upstream
                // ordering we cannot replay instead of silently reordering it.
                let unrepresentable = self.blocks.last().is_some_and(|prior| match kind.as_str() {
                    "thinking" | "redacted_thinking" => {
                        !matches!(prior.kind.as_str(), "thinking" | "redacted_thinking")
                    }
                    "text" => prior.kind == "tool_use",
                    _ => false,
                });
                if unrepresentable {
                    return Err(if self.exposed {
                        partial()
                    } else {
                        wire::compatibility()
                    });
                }
                self.blocks.push(Block {
                    kind,
                    closed: false,
                    signature,
                    input,
                    fragments: String::new(),
                });
            }
            "content_block_delta" if self.started && self.stop_reason.is_none() => {
                let index = index(&value)?;
                let block = self
                    .blocks
                    .get_mut(index)
                    .filter(|b| !b.closed)
                    .ok_or_else(partial)?;
                let delta = &value["delta"];
                let field = match (block.kind.as_str(), delta["type"].as_str()) {
                    ("text", Some("text_delta")) => "text",
                    ("thinking", Some("thinking_delta")) => "thinking",
                    ("thinking", Some("signature_delta")) => "signature",
                    ("tool_use", Some("input_json_delta")) => "partial_json",
                    _ => return Err(partial()),
                };
                let text = delta[field].as_str().ok_or_else(partial)?;
                if field == "signature" {
                    block.signature.push_str(text);
                }
                if field == "partial_json" {
                    // Empty input is the SSE placeholder. Nonempty initial input
                    // followed by a second input is ambiguous and unreviewed.
                    if block.input.as_object().is_none_or(|o| !o.is_empty()) {
                        return Err(partial());
                    }
                    block.fragments.push_str(text);
                }
            }
            "content_block_stop" if self.started && self.stop_reason.is_none() => {
                let index = index(&value)?;
                let block = self
                    .blocks
                    .get_mut(index)
                    .filter(|b| !b.closed)
                    .ok_or_else(partial)?;
                if block.kind == "thinking" && block.signature.is_empty() {
                    return Err(partial());
                }
                if block.kind == "tool_use" && !block.fragments.is_empty() {
                    let input: Value =
                        serde_json::from_str(&block.fragments).map_err(|_| partial())?;
                    if !input.is_object() {
                        return Err(partial());
                    }
                }
                block.closed = true;
            }
            "message_delta" if self.started => {
                if !self.blocks.iter().all(|b| b.closed) {
                    return Err(partial());
                }
                if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                    if self.stop_reason.is_some() {
                        return Err(partial());
                    }
                    if !matches!(
                        reason,
                        "end_turn" | "tool_use" | "stop_sequence" | "max_tokens" | "refusal"
                    ) {
                        return Err(partial());
                    }
                    self.stop_reason = Some(reason.into());
                }
            }
            "message_stop" if self.started && self.stop_reason.is_some() => {
                if self.blocks.is_empty() || !self.blocks.iter().all(|b| b.closed) {
                    return Err(partial());
                }
                let has_tools = self.blocks.iter().any(|b| b.kind == "tool_use");
                if (self.stop_reason.as_deref() == Some("tool_use")) != has_tools {
                    return Err(partial());
                }
                self.ended = true;
                return Ok(Vec::new());
            }
            _ => return Err(partial()),
        }
        let mut events = self
            .state
            .apply_value(value.clone())
            .map_err(|_| partial())?;
        if kind == "content_block_start" {
            let block = self.blocks.last().expect("validated block");
            if block.kind == "tool_use"
                && block
                    .input
                    .as_object()
                    .is_some_and(|input| !input.is_empty())
            {
                for event in &mut events {
                    if let StreamEvent::ToolCall(call) = event {
                        call.arguments = block.input.to_string();
                    }
                }
            }
        } else if kind == "content_block_stop" {
            let index = index(&value)?;
            let block = &self.blocks[index];
            if block.kind == "tool_use"
                && block.fragments.is_empty()
                && block
                    .input
                    .as_object()
                    .is_some_and(|input| input.is_empty())
            {
                events.push(StreamEvent::ToolCall(mirror_protocol::StreamToolCall {
                    index: index as u32,
                    name: String::new(),
                    arguments: "{}".into(),
                }));
            }
        }
        if events.iter().any(|event| {
            matches!(
                event,
                StreamEvent::Content(_)
                    | StreamEvent::Reasoning(_)
                    | StreamEvent::ToolCall(_)
                    | StreamEvent::Refusal(_)
                    | StreamEvent::OutputTokens(1..)
            )
        }) {
            self.exposed = true;
        }
        // Opaque/signature content is also not safely replayable after failure,
        // even if it has no visible callback. An empty text/thinking start alone
        // does not expose output and may still fail as a compatibility boundary.
        if value["content_block"]["type"] == "redacted_thinking"
            || value["content_block"]["signature"]
                .as_str()
                .is_some_and(|signature| !signature.is_empty())
            || value["delta"]["signature"]
                .as_str()
                .is_some_and(|signature| !signature.is_empty())
        {
            self.exposed = true;
        }
        Ok(events)
    }
    pub(super) fn finish(self) -> Result<Completion, ClaudeError> {
        if !self.ended {
            return Err(partial());
        }
        self.state.finish().map_err(|_| partial())
    }
}
fn index(value: &Value) -> Result<usize, ClaudeError> {
    value["index"]
        .as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(partial)
}
