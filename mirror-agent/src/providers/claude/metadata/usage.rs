//! Usage normalization from pinned OMP usage/claude.ts (see module README).
//! Preserve reported values and scope; never gate inference or infer costs.
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{compatibility, models::label, timestamp, ClaudeError, MetadataInfo};
use crate::providers::claude::{ClaudeExtraUsage, ClaudeLimitScope};

pub(super) const MAX_WINDOWS: usize = 128;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeUsageWindow {
    pub id: String,
    pub scope: ClaudeLimitScope,
    /// Provider-reported model family label, not a fabricated model ID.
    pub model: Option<String>,
    pub window_seconds: Option<u64>,
    pub used_percent: Option<f64>,
    #[serde(with = "timestamp")]
    pub resets_at: Option<SystemTime>,
    pub is_active: Option<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmountUnit {
    Credits,
    CurrencyMinor,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedAmount {
    pub value: f64,
    pub unit: AmountUnit,
    pub currency: Option<String>,
    /// Scale only when reported. A missing scale/currency is not USD cents.
    pub decimal_places: Option<u32>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtraUsageSource {
    Legacy,
    Spend,
    Unreported,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtraUsage {
    pub state: ClaudeExtraUsage,
    pub source: ExtraUsageSource,
    pub used: Option<ReportedAmount>,
    pub limit: Option<ReportedAmount>,
}
impl Default for ExtraUsage {
    fn default() -> Self {
        Self {
            state: ClaudeExtraUsage::Unknown,
            source: ExtraUsageSource::Unreported,
            used: None,
            limit: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(super) struct UsageData {
    pub windows: Vec<ClaudeUsageWindow>,
    pub extra_usage: ExtraUsage,
}
#[derive(Debug, Clone, Serialize)]
pub struct ClaudeUsageResult {
    #[serde(flatten)]
    pub info: MetadataInfo,
    pub windows: Vec<ClaudeUsageWindow>,
    pub extra_usage: ExtraUsage,
}
fn number(value: &Value) -> Option<f64> {
    let number = value
        .as_f64()
        .or_else(|| value.as_str().filter(|s| s.len() <= 32)?.parse().ok())?;
    (number.is_finite() && number >= 0.0 && number <= 9_007_199_254_740_991.0).then_some(number)
}
fn reset(value: &Value) -> Option<SystemTime> {
    value
        .as_str()?
        .parse::<prost_types::Timestamp>()
        .ok()?
        .try_into()
        .ok()
}
fn currency(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    (text.len() == 3 && text.bytes().all(|c| c.is_ascii_alphabetic()))
        .then(|| text.to_ascii_uppercase())
}
fn amount(value: &Value, unit: AmountUnit, money: &Value, scale: &str) -> Option<ReportedAmount> {
    if let Some(reported) = money.get("unit") {
        let expected = match unit {
            AmountUnit::Credits => "credits",
            AmountUnit::CurrencyMinor => "currency_minor",
        };
        if reported.as_str() != Some(expected) {
            return None;
        }
    }
    Some(ReportedAmount {
        value: number(value)?,
        unit,
        currency: currency(&money["currency"]),
        decimal_places: money[scale].as_u64().filter(|n| *n <= 18).map(|n| n as u32),
    })
}
fn enabled(value: &Value) -> ClaudeExtraUsage {
    match value.as_bool() {
        Some(true) => ClaudeExtraUsage::Enabled,
        Some(false) => ClaudeExtraUsage::Disabled,
        None => ClaudeExtraUsage::Unknown,
    }
}
fn extra(payload: &Value) -> ExtraUsage {
    if let Some(spend) = payload.get("spend").filter(|v| !v.is_null()) {
        return ExtraUsage {
            state: enabled(&spend["enabled"]),
            source: ExtraUsageSource::Spend,
            used: amount(
                &spend["used"]["amount_minor"],
                AmountUnit::CurrencyMinor,
                &spend["used"],
                "exponent",
            ),
            limit: amount(
                &spend["limit"]["amount_minor"],
                AmountUnit::CurrencyMinor,
                &spend["limit"],
                "exponent",
            ),
        };
    }
    if let Some(legacy) = payload.get("extra_usage").filter(|v| !v.is_null()) {
        return ExtraUsage {
            state: enabled(&legacy["is_enabled"]),
            source: ExtraUsageSource::Legacy,
            used: amount(
                &legacy["used_credits"],
                AmountUnit::Credits,
                legacy,
                "decimal_places",
            ),
            limit: amount(
                &legacy["monthly_limit"],
                AmountUnit::Credits,
                legacy,
                "decimal_places",
            ),
        };
    }
    ExtraUsage::default()
}

pub(super) fn parse(payload: Value) -> Result<UsageData, ClaudeError> {
    let object = payload.as_object().ok_or_else(compatibility)?;
    if ![
        "five_hour",
        "seven_day",
        "seven_day_opus",
        "seven_day_sonnet",
        "limits",
        "extra_usage",
        "spend",
    ]
    .iter()
    .any(|k| object.contains_key(*k))
    {
        return Err(compatibility());
    }
    let mut windows = Vec::new();
    for (key, duration, model) in [
        ("five_hour", 18000, None),
        ("seven_day", 604800, None),
        ("seven_day_opus", 604800, Some("Opus")),
        ("seven_day_sonnet", 604800, Some("Sonnet")),
    ] {
        if let Some(bucket) = object.get(key) {
            if !bucket.is_null() && !bucket.is_object() {
                return Err(compatibility());
            }
            windows.push(ClaudeUsageWindow {
                id: key.into(),
                scope: if model.is_some() {
                    ClaudeLimitScope::Model
                } else {
                    ClaudeLimitScope::Shared
                },
                model: model.map(str::to_owned),
                window_seconds: Some(duration),
                used_percent: number(&bucket["utilization"]),
                resets_at: reset(&bucket["resets_at"]),
                is_active: bucket["is_active"].as_bool(),
            });
        }
    }
    if let Some(raw) = object.get("limits").filter(|v| !v.is_null()) {
        let entries = raw
            .as_array()
            .filter(|a| a.len() <= MAX_WINDOWS - 4)
            .ok_or_else(compatibility)?;
        for (index, entry) in entries.iter().enumerate() {
            let kind = label(&entry["kind"]).ok_or_else(compatibility)?;
            let model = label(&entry["scope"]["model"]["display_name"]);
            let (duration, scope) = match kind.as_str() {
                "session" => (Some(18000), ClaudeLimitScope::Shared),
                "weekly_all" => (Some(604800), ClaudeLimitScope::Shared),
                "weekly_scoped" => (Some(604800), ClaudeLimitScope::Model),
                _ => (
                    None,
                    if model.is_some() {
                        ClaudeLimitScope::Model
                    } else {
                        ClaudeLimitScope::Unknown
                    },
                ),
            };
            // Preserve all reported generic rows, including is_active:false and
            // nullable percentages. Source-qualified IDs avoid collisions with
            // legacy or repeated model-scoped rows; no values are added together.
            windows.push(ClaudeUsageWindow {
                id: format!("limits:{index}:{kind}"),
                scope,
                model,
                window_seconds: duration,
                used_percent: number(&entry["percent"]),
                resets_at: reset(&entry["resets_at"]),
                is_active: entry["is_active"].as_bool(),
            });
        }
    }
    Ok(UsageData {
        windows,
        extra_usage: extra(&payload),
    })
}

impl UsageData {
    pub fn valid(&self) -> bool {
        fn amount_valid(amount: &Option<ReportedAmount>) -> bool {
            amount.as_ref().is_none_or(|a| {
                a.value.is_finite()
                    && a.value >= 0.0
                    && a.value <= 9_007_199_254_740_991.0
                    && a.currency
                        .as_ref()
                        .is_none_or(|c| currency(&serde_json::json!(c)).is_some())
                    && a.decimal_places.is_none_or(|n| n <= 18)
            })
        }
        self.windows.len() <= MAX_WINDOWS
            && self.windows.iter().all(|w| {
                w.id.len() <= 280
                    && !w.id.chars().any(char::is_control)
                    && w.model
                        .as_ref()
                        .is_none_or(|s| label(&serde_json::json!(s)).is_some())
                    && w.used_percent
                        .is_none_or(|n| n.is_finite() && n >= 0.0 && n <= 9_007_199_254_740_991.0)
            })
            && amount_valid(&self.extra_usage.used)
            && amount_valid(&self.extra_usage.limit)
    }
}
