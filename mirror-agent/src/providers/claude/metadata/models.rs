//! Normalized catalog and context data. Fallbacks never claim entitlement.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{compatibility, ClaudeError, MetadataInfo};

pub(super) const MAX_MODELS: usize = 1024;
pub(super) const MAX_PAGES: usize = 16;
pub(super) const MAX_CONTEXT: u64 = 100_000_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeModel {
    pub id: String,
    pub display_name: Option<String>,
    pub context_window: Option<u64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSource {
    Remote,
    Bundled,
}
#[derive(Debug, Clone, Serialize)]
pub struct ClaudeModelsResult {
    #[serde(flatten)]
    pub info: MetadataInfo,
    pub source: CatalogSource,
    pub models: Vec<ClaudeModel>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextProvenance {
    Explicit,
    ModelMetadata,
    KnownModelFallback,
    ConservativeFallback,
}
#[derive(Debug, Clone, Serialize)]
pub struct ClaudeModelContextResult {
    #[serde(flatten)]
    pub info: MetadataInfo,
    pub model: String,
    pub context_window: u64,
    pub provenance: ContextProvenance,
}

pub(super) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
}
pub(super) fn label(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    (!text.is_empty() && text.len() <= 256 && !text.chars().any(char::is_control))
        .then(|| text.to_owned())
}
pub(super) fn context(value: &Value) -> Option<u64> {
    value.as_u64().filter(|v| *v > 0 && *v <= MAX_CONTEXT)
}
pub(super) fn parse_model(value: &Value) -> Result<ClaudeModel, ClaudeError> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| valid_id(id))
        .ok_or_else(compatibility)?;
    // Reviewed API field, then the existing Mirror metadata aliases. Output
    // token limits are deliberately not candidates for input context length.
    let context_window = [
        "max_input_tokens",
        "context_window",
        "context_length",
        "max_model_len",
    ]
    .into_iter()
    .find_map(|key| context(&value[key]));
    Ok(ClaudeModel {
        id: id.to_owned(),
        display_name: label(&value["display_name"]),
        context_window,
    })
}
pub(super) struct Page {
    pub models: Vec<ClaudeModel>,
    pub next: Option<String>,
}
pub(super) fn parse_page(value: Value, seen: &mut HashSet<String>) -> Result<Page, ClaudeError> {
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .filter(|data| data.len() <= MAX_MODELS)
        .ok_or_else(compatibility)?;
    let has_more = value
        .get("has_more")
        .and_then(Value::as_bool)
        .ok_or_else(compatibility)?;
    let mut models = Vec::with_capacity(data.len());
    for value in data {
        let model = parse_model(value)?;
        if !seen.insert(model.id.clone()) || seen.len() > MAX_MODELS {
            return Err(compatibility());
        }
        models.push(model);
    }
    let next = if has_more {
        let last = value
            .get("last_id")
            .and_then(Value::as_str)
            .filter(|id| valid_id(id))
            .ok_or_else(compatibility)?;
        if models.last().is_none_or(|model| model.id != last) {
            return Err(compatibility());
        }
        Some(last.to_owned())
    } else {
        None
    };
    Ok(Page { models, next })
}

/// Reviewed exact IDs only. Phase 0's reported 1M metadata was not proof of
/// long-context entitlement. Offline fallback stays at the conservative 200k.
pub(super) fn known_context(model: &str) -> Option<u64> {
    match model {
        "claude-opus-4-8" | "claude-sonnet-4-6" => Some(200_000),
        _ => None,
    }
}
pub(super) fn bundled() -> Vec<ClaudeModel> {
    [
        ("claude-opus-4-8", "Claude Opus 4.8"),
        ("claude-sonnet-4-6", "Claude Sonnet 4.6"),
    ]
    .into_iter()
    .map(|(id, name)| ClaudeModel {
        id: id.into(),
        display_name: Some(name.into()),
        context_window: None,
    })
    .collect()
}
pub(super) fn fallback(model: &str) -> (u64, ContextProvenance) {
    match known_context(model) {
        Some(window) => (window, ContextProvenance::KnownModelFallback),
        None => (200_000, ContextProvenance::ConservativeFallback),
    }
}
