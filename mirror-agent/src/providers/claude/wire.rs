//! Frozen OMP-derived Messages compatibility. See diagnostics/claude_oauth/README.md
//! and LICENSE-OMP (MIT), revision df4f996e66a200ab7e0496114d43bdf85f32194d.
//! Canonical messages never contain the transport identity/billing prefixes.
use std::collections::{BTreeMap, BTreeSet};

use mirror_protocol::chat_record;
use ring::digest::{digest, SHA256};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::Value;
use uuid::Uuid;

use super::{profile, ClaudeConfigurationError, ClaudeError, ClaudeErrorKind};
use crate::providers::CompletionRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeModelCapabilities {
    pub model: &'static str,
    pub tools: bool,
    /// Phase 0 observed a signed block and replay, not just a reasoning candidate.
    pub signed_reasoning_verified: bool,
    pub reasoning_efforts: &'static [&'static str],
    pub summarized_display: bool,
    /// Observed metadata ceiling; not a promise of entitlement.
    pub max_output_tokens: u32,
}

/// Only reviewed presets are exposed. Unknown models/capabilities are not guessed
/// from their names. Task 5 can present other discovery results as candidates.
pub fn claude_model_capabilities(model: &str) -> Option<ClaudeModelCapabilities> {
    let (model, verified, display) = match model {
        "claude-opus-4-8" => ("claude-opus-4-8", true, true),
        "claude-sonnet-4-6" => ("claude-sonnet-4-6", true, false),
        "claude-sonnet-5" => ("claude-sonnet-5", false, true),
        _ => return None,
    };
    Some(ClaudeModelCapabilities {
        model,
        tools: true,
        signed_reasoning_verified: verified,
        reasoning_efforts: if verified {
            &["none", "low"]
        } else {
            &["none"]
        },
        summarized_display: display,
        max_output_tokens: 128_000,
    })
}

pub(super) fn invalid() -> ClaudeError {
    ClaudeError::configuration(ClaudeConfigurationError::InvalidRequestOptions)
}
pub(super) fn compatibility() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::CompatibilityError)
}

#[derive(Debug)]
pub(super) struct ToolNames {
    forward: BTreeMap<String, String>,
    active: BTreeMap<String, String>,
}
impl ToolNames {
    pub(super) fn new(request: &CompletionRequest) -> Result<Self, ClaudeError> {
        let mut names = Self {
            forward: BTreeMap::new(),
            active: BTreeMap::new(),
        };
        for tool in &request.tools {
            let wire = names.register(&tool.name)?;
            if names.active.insert(wire, tool.name.clone()).is_some() {
                return Err(invalid());
            }
        }
        for message in &request.messages {
            if let chat_record::Message::Assistant(message) = message {
                for call in &message.tool_calls {
                    let function = call.function.as_ref().ok_or_else(invalid)?;
                    names.register(&function.name)?;
                }
            }
        }
        Ok(names)
    }
    fn register(&mut self, name: &str) -> Result<String, ClaudeError> {
        if name.is_empty()
            || name.len() > 63
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(invalid());
        }
        let wire = format!("_{name}");
        self.forward.insert(name.into(), wire.clone());
        Ok(wire)
    }
    pub(super) fn encode(&self, name: &str) -> Result<&str, ClaudeError> {
        self.forward
            .get(name)
            .map(String::as_str)
            .ok_or_else(invalid)
    }
    pub(super) fn decode(&self, wire: &str) -> Result<&str, ClaudeError> {
        self.active
            .get(wire)
            .map(String::as_str)
            .ok_or_else(compatibility)
    }
}

// Local ordered objects avoid changing serde_json's map ordering (and therefore
// API-key wire behavior) across the workspace.
enum Node {
    Raw(Value),
    Object(Vec<(&'static str, Node)>),
    Array(Vec<Node>),
}
impl Serialize for Node {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Raw(value) => value.serialize(serializer),
            Self::Object(fields) => {
                let mut map = serializer.serialize_map(Some(fields.len()))?;
                for (key, value) in fields {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
            Self::Array(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    seq.serialize_element(value)?;
                }
                seq.end()
            }
        }
    }
}
fn raw(value: impl Into<Value>) -> Node {
    Node::Raw(value.into())
}
fn object(fields: Vec<(&'static str, Node)>) -> Node {
    Node::Object(fields)
}
fn cache() -> Node {
    object(vec![("type", raw("ephemeral"))])
}
fn text(text: &str, cached: bool) -> Node {
    let mut fields = vec![("type", raw("text")), ("text", raw(text))];
    if cached {
        fields.push(("cache_control", cache()));
    }
    object(fields)
}

pub(super) struct PreparedRequest {
    model: String,
    messages: Node,
    system: String,
    tools: Node,
    max_tokens: u32,
    thinking: Option<Node>,
    choice: Option<Node>,
    stops: Option<Node>,
    pub(super) names: ToolNames,
}

impl PreparedRequest {
    /// Validate everything caller-controlled before requesting an access lease.
    pub(super) fn new(model: &str, request: &CompletionRequest) -> Result<Self, ClaudeError> {
        let capabilities = claude_model_capabilities(model)
            .ok_or_else(|| ClaudeError::new(ClaudeErrorKind::ModelUnavailable))?;
        let max_tokens = request.max_tokens.unwrap_or(4096);
        if max_tokens == 0 || max_tokens > capabilities.max_output_tokens {
            return Err(invalid());
        }
        let inference = request.inference.as_ref();
        if let Some(inference) = inference {
            if inference.base_url.as_deref().is_some_and(|url| {
                !url.is_empty() && url.trim_end_matches('/') != profile::API_ORIGIN
            }) {
                return Err(ClaudeError::configuration(
                    ClaudeConfigurationError::EndpointNotAllowed,
                ));
            }
            if (!inference.model.is_empty() && inference.model != model)
                || inference
                    .api
                    .as_deref()
                    .is_some_and(|api| !api.is_empty() && api != "anthropic-oauth")
                || inference.temperature.is_some()
                || inference.top_p.is_some()
                || inference.seed.is_some()
            {
                return Err(invalid());
            }
        }
        let effort = inference
            .and_then(|i| i.reasoning_effort.as_deref())
            .filter(|s| !s.is_empty())
            .unwrap_or(if capabilities.signed_reasoning_verified {
                "low"
            } else {
                "none"
            });
        if !capabilities.reasoning_efforts.contains(&effort) {
            return Err(invalid());
        }
        let names = ToolNames::new(request)?;
        let mut choice = None;
        let mut stops = None;
        if let Some(extra) = inference
            .and_then(|i| i.extra_body.as_deref())
            .filter(|s| !s.trim().is_empty())
        {
            let Value::Object(extra) = serde_json::from_str(extra).map_err(|_| invalid())? else {
                return Err(invalid());
            };
            for (key, value) in extra {
                match key.as_str() {
                    "tool_choice" => {
                        let Value::Object(fields) = &value else {
                            return Err(invalid());
                        };
                        if fields.len() != 2 || value["type"] != "tool" {
                            return Err(invalid());
                        }
                        let canonical = value["name"].as_str().ok_or_else(invalid)?;
                        let wire = names.encode(canonical)?;
                        names.decode(wire).map_err(|_| invalid())?;
                        choice = Some(object(vec![("type", raw("tool")), ("name", raw(wire))]));
                    }
                    "stop_sequences" => {
                        let values = value.as_array().ok_or_else(invalid)?;
                        if values.len() > 16
                            || values
                                .iter()
                                .any(|v| v.as_str().is_none_or(|s| s.is_empty() || s.len() > 4096))
                        {
                            return Err(invalid());
                        }
                        stops = Some(Node::Raw(value));
                    }
                    // Positive allowlist: no unknown billing/service-tier/cache/
                    // identity/model or serialization override can slip through.
                    _ => return Err(invalid()),
                }
            }
        }
        let mut ids = BTreeSet::new();
        let mut results = BTreeSet::new();
        let mut messages = Vec::new();
        let mut systems = Vec::new();
        for message in &request.messages {
            if let chat_record::Message::System(message) = message {
                systems.push(message.content.as_str());
                continue;
            }
            let value = super::super::anthropic_messages::serialize_message(message)
                .map_err(|_| invalid())?;
            let content = match &value["content"] {
                Value::String(value) => raw(value.as_str()),
                Value::Array(blocks) => {
                    let mut output = Vec::new();
                    for block in blocks {
                        let block = match block["type"].as_str() {
                            Some("thinking") => {
                                let thinking = block["thinking"].as_str().ok_or_else(invalid)?;
                                let signature = block["signature"]
                                    .as_str()
                                    .filter(|s| !s.is_empty())
                                    .ok_or_else(invalid)?;
                                object(vec![
                                    ("type", raw("thinking")),
                                    ("thinking", raw(thinking)),
                                    ("signature", raw(signature)),
                                ])
                            }
                            Some("redacted_thinking") => {
                                let data = block["data"]
                                    .as_str()
                                    .filter(|s| !s.is_empty())
                                    .ok_or_else(invalid)?;
                                object(vec![
                                    ("type", raw("redacted_thinking")),
                                    ("data", raw(data)),
                                ])
                            }
                            Some("text") => {
                                text(block["text"].as_str().ok_or_else(invalid)?, false)
                            }
                            Some("tool_use") => {
                                let id = block["id"]
                                    .as_str()
                                    .filter(|s| !s.is_empty())
                                    .ok_or_else(invalid)?;
                                if !ids.insert(id.to_owned()) || !block["input"].is_object() {
                                    return Err(invalid());
                                }
                                let wire =
                                    names.encode(block["name"].as_str().ok_or_else(invalid)?)?;
                                object(vec![
                                    ("type", raw("tool_use")),
                                    ("name", raw(wire)),
                                    ("id", raw(id)),
                                    ("input", Node::Raw(block["input"].clone())),
                                ])
                            }
                            Some("tool_result") => {
                                let id = block["tool_use_id"].as_str().ok_or_else(invalid)?;
                                if !ids.contains(id) || !results.insert(id.to_owned()) {
                                    return Err(invalid());
                                }
                                object(vec![
                                    ("type", raw("tool_result")),
                                    ("tool_use_id", raw(id)),
                                    ("content", Node::Raw(block["content"].clone())),
                                ])
                            }
                            _ => return Err(invalid()),
                        };
                        output.push(block);
                    }
                    Node::Array(output)
                }
                _ => return Err(invalid()),
            };
            messages.push(object(vec![
                ("role", Node::Raw(value["role"].clone())),
                ("content", content),
            ]));
        }
        let tool_count = request.tools.len();
        let tools = request
            .tools
            .iter()
            .enumerate()
            .map(|(i, tool)| {
                let mut fields = vec![
                    ("name", raw(names.encode(&tool.name).expect("validated"))),
                    ("description", raw(tool.description.as_str())),
                    (
                        "input_schema",
                        Node::Raw(Value::Object(tool.parameters.clone())),
                    ),
                ];
                if i + 1 == tool_count {
                    fields.push(("cache_control", cache()));
                }
                object(fields)
            })
            .collect();
        let thinking = (effort != "none" && choice.is_none()).then(|| {
            let mut fields = vec![("type", raw("adaptive"))];
            if capabilities.summarized_display {
                fields.push(("display", raw("summarized")));
            }
            object(fields)
        });
        Ok(Self {
            model: model.into(),
            messages: Node::Array(messages),
            system: systems.join("\n\n"),
            tools: Node::Array(tools),
            max_tokens,
            thinking,
            choice,
            stops,
            names,
        })
    }

    pub(super) fn body(
        &self,
        installation: Uuid,
        account: Uuid,
        session: &str,
    ) -> Result<Vec<u8>, ClaudeError> {
        // First user text, with JS UTF-16 indexing rather than Rust chars/bytes.
        let Node::Array(messages) = &self.messages else {
            unreachable!()
        };
        let first = messages
            .iter()
            .find_map(|message| {
                let Node::Object(fields) = message else {
                    return None;
                };
                if !fields.iter().any(|(k, v)| {
                    *k == "role" && matches!(v, Node::Raw(Value::String(s)) if s == "user")
                }) {
                    return None;
                }
                Some(
                    fields
                        .iter()
                        .find(|(k, _)| *k == "content")
                        .map(|(_, value)| node_text(value))
                        .unwrap_or_default(),
                )
            })
            .unwrap_or_default();
        let units: Vec<u16> = first.encode_utf16().collect();
        let selected: Vec<u16> = [4, 7, 20]
            .into_iter()
            .map(|i| units.get(i).copied().unwrap_or(b'0' as u16))
            .collect();
        let seed_text = format!(
            "59cf53e54c78{}{}",
            String::from_utf16_lossy(&selected),
            profile::CLI_VERSION
        );
        let suffix = hex(digest(&SHA256, seed_text.as_bytes()).as_ref());
        let billing = format!(
            "x-anthropic-billing-header: cc_version={}.{}; cc_entrypoint=cli; cch=00000;",
            profile::CLI_VERSION,
            &suffix[..3]
        );
        let device = hex(digest(
            &SHA256,
            format!("mirror-claude-device-id-v1\0{installation}\0{account}").as_bytes(),
        )
        .as_ref());
        let metadata = object(vec![
            ("device_id", raw(device)),
            ("session_id", raw(session)),
            ("account_uuid", raw(account.to_string())),
        ]);
        let user_id = serde_json::to_string(&metadata).map_err(|_| compatibility())?;
        let mut system = vec![
            text(&billing, false),
            text(profile::IDENTITY_TEXT, self.system.is_empty()),
        ];
        if !self.system.is_empty() {
            system.push(text(&self.system, true));
        }
        let system = Node::Array(system);
        // References serialize identically, without cloning/reordering user data.
        let mut fields: Vec<(&str, &dyn erased::Json)> = vec![
            ("model", &self.model),
            ("messages", &self.messages),
            ("system", &system),
            ("tools", &self.tools),
        ];
        let metadata = object(vec![("user_id", raw(user_id))]);
        fields.extend([
            ("metadata", &metadata as &dyn erased::Json),
            ("max_tokens", &self.max_tokens),
        ]);
        let context = object(vec![(
            "edits",
            Node::Array(vec![object(vec![
                ("type", raw("clear_thinking_20251015")),
                ("keep", raw("all")),
            ])]),
        )]);
        if let Some(thinking) = &self.thinking {
            fields.push(("thinking", thinking));
            fields.push(("context_management", &context));
        }
        let output = object(vec![("effort", raw("low"))]);
        // This profile uses low effort when thinking is omitted/forced as well.
        fields.push(("output_config", &output));
        let stream = true;
        fields.push(("stream", &stream));
        if let Some(choice) = &self.choice {
            fields.push(("tool_choice", choice));
        }
        if let Some(stops) = &self.stops {
            fields.push(("stop_sequences", stops));
        }
        // Serialize once. The writer records the structurally known system field
        // offset, never searches user text for a marker or reserializes the body.
        let mut encoded = Vec::new();
        encoded.push(b'{');
        let mut cch_offset = None;
        for (i, (key, value)) in fields.into_iter().enumerate() {
            if i > 0 {
                encoded.push(b',');
            }
            serde_json::to_writer(&mut encoded, key).map_err(|_| compatibility())?;
            encoded.push(b':');
            if key == "system" {
                // Known generated system prefix; billing is ASCII and contains
                // no JSON escapes. This is not a content-dependent search.
                let prefix = format!(
                    "[{{\"type\":\"text\",\"text\":\"{}",
                    billing.split_once("cch=").expect("generated billing").0
                );
                cch_offset = Some(encoded.len() + prefix.len() + 4);
            }
            value.write(&mut encoded)?;
        }
        encoded.push(b'}');
        let offset = cch_offset.ok_or_else(compatibility)?;
        if encoded.get(offset..offset + 5) != Some(b"00000") {
            return Err(compatibility());
        }
        let cch = format!("{:05x}", xxhash64(&encoded, 0x4d659218e32a3268) & 0xfffff);
        encoded[offset..offset + 5].copy_from_slice(cch.as_bytes());
        Ok(encoded)
    }
}
fn node_text(node: &Node) -> String {
    match node {
        Node::Raw(Value::String(text)) => text.clone(),
        Node::Array(blocks) => blocks
            .iter()
            .filter_map(|block| {
                let Node::Object(fields) = block else {
                    return None;
                };
                fields.iter().find_map(|(key, node)| match (*key, node) {
                    ("text", Node::Raw(Value::String(text))) => Some(text.as_str()),
                    _ => None,
                })
            })
            .collect(),
        _ => String::new(),
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// A tiny object-safe serializer adapter; no new serialization dependency or
// global serde_json map feature affects the existing API provider.
mod erased {
    use super::*;
    pub(super) trait Json {
        fn write(&self, output: &mut Vec<u8>) -> Result<(), ClaudeError>;
    }
    impl<T: Serialize> Json for T {
        fn write(&self, output: &mut Vec<u8>) -> Result<(), ClaudeError> {
            serde_json::to_writer(output, self).map_err(|_| compatibility())
        }
    }
}

/// XXH64 reference algorithm, full 64-bit seed; frozen Bun oracle vectors test
/// both short/long inputs. Wrapping arithmetic is intentional modulo 2^64.
pub(super) fn xxhash64(bytes: &[u8], seed: u64) -> u64 {
    const P1: u64 = 11400714785074694791;
    const P2: u64 = 14029467366897019727;
    const P3: u64 = 1609587929392839161;
    const P4: u64 = 9650029242287828579;
    const P5: u64 = 2870177450012600261;
    fn round(acc: u64, word: u64) -> u64 {
        acc.wrapping_add(word.wrapping_mul(P2))
            .rotate_left(31)
            .wrapping_mul(P1)
    }
    let word = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().expect("bounded"));
    let mut i = 0;
    let mut h;
    if bytes.len() >= 32 {
        let mut lanes = [
            seed.wrapping_add(P1).wrapping_add(P2),
            seed.wrapping_add(P2),
            seed,
            seed.wrapping_sub(P1),
        ];
        while i <= bytes.len() - 32 {
            for (n, lane) in lanes.iter_mut().enumerate() {
                *lane = round(*lane, word(i + n * 8));
            }
            i += 32;
        }
        h = lanes[0]
            .rotate_left(1)
            .wrapping_add(lanes[1].rotate_left(7))
            .wrapping_add(lanes[2].rotate_left(12))
            .wrapping_add(lanes[3].rotate_left(18));
        for lane in lanes {
            h = (h ^ round(0, lane)).wrapping_mul(P1).wrapping_add(P4);
        }
    } else {
        h = seed.wrapping_add(P5);
    }
    h = h.wrapping_add(bytes.len() as u64);
    while i + 8 <= bytes.len() {
        h = (h ^ round(0, word(i)))
            .rotate_left(27)
            .wrapping_mul(P1)
            .wrapping_add(P4);
        i += 8;
    }
    if i + 4 <= bytes.len() {
        h = (h
            ^ (u32::from_le_bytes(bytes[i..i + 4].try_into().expect("bounded")) as u64)
                .wrapping_mul(P1))
        .rotate_left(23)
        .wrapping_mul(P2)
        .wrapping_add(P3);
        i += 4;
    }
    while i < bytes.len() {
        h = (h ^ (bytes[i] as u64).wrapping_mul(P5))
            .rotate_left(11)
            .wrapping_mul(P1);
        i += 1;
    }
    h ^= h >> 33;
    h = h.wrapping_mul(P2);
    h ^= h >> 29;
    h = h.wrapping_mul(P3);
    h ^ (h >> 32)
}
