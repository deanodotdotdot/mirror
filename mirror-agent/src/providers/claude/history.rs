//! Nonsecret provenance only. Replay/normalization policy belongs to transport.
use std::collections::BTreeMap;

use prost_types::{value::Kind, Struct, Value};
use serde::{Deserialize, Serialize};

pub const REASONING_PROVENANCE_KEY: &str = "mirror_reasoning_provenance";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnthropicBillingMode {
    ApiKey,
    Subscription,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningProvenance {
    pub schema_version: u32,
    pub provider_family: String,
    pub billing_mode: AnthropicBillingMode,
    pub model_id: String,
    pub compatibility_profile: Option<String>,
}

impl ReasoningProvenance {
    /// Adds the reserved namespace while retaining unrelated provider fields.
    pub fn insert_into(&self, fields: &mut Option<Struct>) {
        let mut values = BTreeMap::from([
            (
                "schema_version".into(),
                Value {
                    kind: Some(Kind::NumberValue(self.schema_version.into())),
                },
            ),
            ("provider_family".into(), string(&self.provider_family)),
            (
                "billing_mode".into(),
                string(match self.billing_mode {
                    AnthropicBillingMode::ApiKey => "api_key",
                    AnthropicBillingMode::Subscription => "subscription",
                }),
            ),
            ("model_id".into(), string(&self.model_id)),
        ]);
        if let Some(profile) = &self.compatibility_profile {
            values.insert("compatibility_profile".into(), string(profile));
        }
        fields.get_or_insert_with(Struct::default).fields.insert(
            REASONING_PROVENANCE_KEY.into(),
            Value {
                kind: Some(Kind::StructValue(Struct { fields: values })),
            },
        );
    }

    /// Unknown/legacy/malformed provenance never establishes opaque portability.
    pub fn from_fields(fields: Option<&Struct>) -> Option<Self> {
        let Kind::StructValue(value) = fields?
            .fields
            .get(REASONING_PROVENANCE_KEY)?
            .kind
            .as_ref()?
        else {
            return None;
        };
        if value.fields.get("schema_version")?.kind != Some(Kind::NumberValue(1.0)) {
            return None;
        }
        let provider_family = text(value, "provider_family")?;
        let model_id = text(value, "model_id")?;
        if provider_family != "anthropic" || model_id.is_empty() {
            return None;
        }
        let billing_mode = match text(value, "billing_mode")? {
            "api_key" => AnthropicBillingMode::ApiKey,
            "subscription" => AnthropicBillingMode::Subscription,
            _ => return None,
        };
        let compatibility_profile = if value.fields.contains_key("compatibility_profile") {
            Some(text(value, "compatibility_profile")?.to_owned())
        } else {
            None
        };
        if billing_mode == AnthropicBillingMode::Subscription
            && compatibility_profile.as_deref().is_none_or(str::is_empty)
        {
            return None;
        }
        Some(Self {
            schema_version: 1,
            provider_family: provider_family.into(),
            billing_mode,
            model_id: model_id.into(),
            compatibility_profile,
        })
    }
}

fn string(value: &str) -> Value {
    Value {
        kind: Some(Kind::StringValue(value.into())),
    }
}
fn text<'a>(value: &'a Struct, key: &str) -> Option<&'a str> {
    match value.fields.get(key)?.kind.as_ref()? {
        Kind::StringValue(text) => Some(text),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryNormalizationReason {
    UnknownProvenance,
    ProviderChanged,
    BillingModeChanged,
    ModelChanged,
    CompatibilityProfileChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryNormalizationNotice {
    pub schema_version: u32,
    pub reason: HistoryNormalizationReason,
    pub affected_messages: u32,
}

impl HistoryNormalizationNotice {
    pub fn to_protocol(&self) -> mirror_protocol::HistoryNormalizationNotice {
        mirror_protocol::HistoryNormalizationNotice {
            schema_version: self.schema_version,
            reason: match self.reason {
                HistoryNormalizationReason::UnknownProvenance => "unknown_provenance",
                HistoryNormalizationReason::ProviderChanged => "provider_changed",
                HistoryNormalizationReason::BillingModeChanged => "billing_mode_changed",
                HistoryNormalizationReason::ModelChanged => "model_changed",
                HistoryNormalizationReason::CompatibilityProfileChanged => {
                    "compatibility_profile_changed"
                }
            }
            .into(),
            affected_messages: self.affected_messages,
        }
    }
}
