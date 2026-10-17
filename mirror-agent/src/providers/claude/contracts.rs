use std::fmt;
use std::time::SystemTime;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{profile, ClaudeOperation};
use crate::providers::ModelRequestGuard;

pub const CLAUDE_SCHEMA_VERSION: u32 = 1;
pub const CLAUDE_PROVIDER: &str = "anthropic-oauth";

/// Durable identity, independent of the current provider and token generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeIdentityBinding {
    pub account_id: Uuid,
    pub organization_id: Uuid,
}

/// Per-conversation state; retain it while using another provider so switching
/// back cannot silently bind an old conversation to a replacement account.
/// Installation identity comes from the authority; session ID remains the
/// existing conversation/prompt_cache_key, never a second stored identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeSessionBinding {
    pub schema_version: u32,
    pub identity: ClaudeIdentityBinding,
    pub compatibility_profile: String,
}

impl ClaudeSessionBinding {
    pub fn new(identity: ClaudeIdentityBinding) -> Self {
        Self {
            schema_version: CLAUDE_SCHEMA_VERSION,
            identity,
            compatibility_profile: profile::PROFILE_ID.into(),
        }
    }

    pub fn validate(&self) -> Result<(), ClaudeError> {
        if self.schema_version != CLAUDE_SCHEMA_VERSION {
            return Err(ClaudeError::configuration(
                ClaudeConfigurationError::UnknownSchema,
            ));
        }
        if self.identity.account_id.is_nil() || self.identity.organization_id.is_nil() {
            return Err(ClaudeError::configuration(
                ClaudeConfigurationError::InvalidIdentity,
            ));
        }
        if self.compatibility_profile != profile::PROFILE_ID {
            return Err(ClaudeError::configuration(
                ClaudeConfigurationError::UnknownProfile,
            ));
        }
        Ok(())
    }
}

impl From<&ClaudeSessionBinding> for mirror_protocol::ClaudeSessionBinding {
    fn from(value: &ClaudeSessionBinding) -> Self {
        Self {
            schema_version: value.schema_version,
            identity: Some((&value.identity).into()),
            compatibility_profile: value.compatibility_profile.clone(),
        }
    }
}

impl From<&ClaudeIdentityBinding> for mirror_protocol::ClaudeIdentityBinding {
    fn from(value: &ClaudeIdentityBinding) -> Self {
        Self {
            account_id: value.account_id.to_string(),
            organization_id: value.organization_id.to_string(),
        }
    }
}

impl TryFrom<&mirror_protocol::ClaudeSessionBinding> for ClaudeSessionBinding {
    type Error = ClaudeError;
    fn try_from(value: &mirror_protocol::ClaudeSessionBinding) -> Result<Self, Self::Error> {
        let invalid = || ClaudeError::configuration(ClaudeConfigurationError::InvalidIdentity);
        let identity = value.identity.as_ref().ok_or_else(invalid)?;
        let binding = Self {
            schema_version: value.schema_version,
            identity: ClaudeIdentityBinding {
                account_id: identity.account_id.parse().map_err(|_| invalid())?,
                organization_id: identity.organization_id.parse().map_err(|_| invalid())?,
            },
            compatibility_profile: value.compatibility_profile.clone(),
        };
        binding.validate()?;
        Ok(binding)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaudeTokenVersion {
    pub generation: Uuid,
    pub revision: u64,
}

/// Opaque access material. Deliberately not Clone, Display or serializable.
pub struct ClaudeAccessToken(String);

impl ClaudeAccessToken {
    pub fn new(value: String) -> Result<Self, ClaudeError> {
        // A bearer token must also be a valid header value. Do not echo invalid input.
        if value.is_empty() || value.bytes().any(|byte| byte <= b' ' || byte >= 0x7f) {
            return Err(ClaudeError::configuration(
                ClaudeConfigurationError::InvalidAccessToken,
            ));
        }
        Ok(Self(value))
    }
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ClaudeAccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClaudeAccessToken([REDACTED])")
    }
}

/// One authorization for immediate dispatch, never reusable after a wait/retry.
/// Not Clone or serializable. No refresh token is exposed to consumers.
///
/// ```compile_fail
/// use mirror_agent::providers::claude::ClaudeAccessLease;
/// fn serialized<T: serde::Serialize>() {}
/// serialized::<ClaudeAccessLease>();
/// ```
/// ```compile_fail
/// use mirror_agent::providers::claude::ClaudeAccessLease;
/// fn cloned<T: Clone>() {}
/// cloned::<ClaudeAccessLease>();
/// ```
pub struct ClaudeAccessLease {
    pub access_token: ClaudeAccessToken,
    pub identity: ClaudeIdentityBinding,
    pub installation_id: Uuid,
    pub version: ClaudeTokenVersion,
    pub expires_at: SystemTime,
    pub operation: ClaudeOperation,
}

impl fmt::Debug for ClaudeAccessLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaudeAccessLease")
            .field("access_token", &self.access_token)
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}

pub struct ClaudeAuthorizationContext<'a> {
    pub operation: ClaudeOperation,
    pub expected_identity: &'a ClaudeIdentityBinding,
    pub request_guard: Option<&'a dyn ModelRequestGuard>,
    /// Must cancel lock acquisition, refresh HTTP and guard awaits. Dropping the
    /// returned authorize future must do the same; never detach renewal work.
    /// A caller whose lifetime is cancellation may pass std::future::pending().
    pub cancellation: BoxFuture<'a, ()>,
}

/// The single authority entry point for each network attempt. Resolve freshness,
/// coordinate renewal, then recheck generation/identity/guard under the stable
/// credential lock. Release that lock before returning the immediate-use lease.
/// A rejected version permits one coordinated 401 recovery; reload before
/// deciding whether to force renewal. No second resolve/refresh credential path.
pub trait ClaudeAuthorization: Send + Sync {
    fn authorize<'a>(
        &'a self,
        context: ClaudeAuthorizationContext<'a>,
        rejected: Option<ClaudeTokenVersion>,
    ) -> BoxFuture<'a, Result<ClaudeAccessLease, ClaudeError>>;

    /// Inspect under the stable credential lock without network, renewal or
    /// mutation. Cancellation/drop covers lock waiting. Report persisted known
    /// revocation as LoginRequired so metadata caches cannot conceal it.
    fn local_snapshot<'a>(
        &'a self,
        cancellation: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<ClaudeLocalAuthSnapshot, ClaudeError>>;
}

/// Internal cache coordination only; deliberately not serializable or Clone.
/// Missing file has no version; logout tombstones retain generation/revision 0.
#[derive(Debug)]
pub struct ClaudeLocalAuthSnapshot {
    pub status: ClaudeAuthStatus,
    pub version: Option<ClaudeTokenVersion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeAuthState {
    LoggedOut,
    Usable,
    Refreshable,
    LoginRequired,
}

/// User-facing labels are excluded from Debug and generic telemetry.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct ClaudeAccountDisplay {
    pub email: Option<String>,
    pub organization_name: Option<String>,
}
impl fmt::Debug for ClaudeAccountDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClaudeAccountDisplay { .. }")
    }
}

/// Local-only inspection result. Constructing/fetching it must not renew or probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaudeAuthStatus {
    pub schema_version: u32,
    pub provider: &'static str,
    pub source: &'static str,
    pub state: ClaudeAuthState,
    pub identity: Option<ClaudeIdentityBinding>,
    pub display: Option<ClaudeAccountDisplay>,
    #[serde(with = "optional_utc")]
    pub expires_at: Option<SystemTime>,
    pub recovery: ClaudeRecovery,
    pub error: Option<ClaudeError>,
}

impl ClaudeAuthStatus {
    pub fn new(state: ClaudeAuthState) -> Self {
        Self {
            schema_version: CLAUDE_SCHEMA_VERSION,
            provider: CLAUDE_PROVIDER,
            source: "Mirror login",
            state,
            identity: None,
            display: None,
            expires_at: None,
            recovery: match state {
                ClaudeAuthState::LoggedOut | ClaudeAuthState::LoginRequired => {
                    ClaudeRecovery::Login
                }
                _ => ClaudeRecovery::None,
            },
            error: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeRecovery {
    None,
    Login,
    Reconnect,
    RetryLater,
    RepairStorage,
    CheckConfiguration,
    Upgrade,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeErrorKind {
    LoginRequired,
    LoginExpired,
    LoginCancelled,
    LoginTimeout,
    CredentialStorageError,
    AccountChanged,
    SubscriptionLimited,
    ModelUnavailable,
    AccessDenied,
    AuthTemporarilyUnavailable,
    CompatibilityError,
    InvalidConfiguration,
    PartialStreamError,
    RequestCancelled,
}

/// Only the transport's shared bounded budget may consume SafeBeforeOutput.
/// Once returned to the agent/worker, every Claude error is final for that turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeRetryDisposition {
    Terminal,
    SafeBeforeOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeProviderCode {
    AuthenticationError,
    InvalidGrant,
    PermissionError,
    NotFoundError,
    RateLimitError,
    OverloadedError,
    InvalidRequestError,
    ApiError,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeExtraUsage {
    Enabled,
    Disabled,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeLimitScope {
    Shared,
    Model,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeConfigurationError {
    ApiKeyNotAllowed,
    EndpointNotAllowed,
    CodexAuthPathNotAllowed,
    MissingBinding,
    UnknownSchema,
    UnknownProfile,
    InvalidIdentity,
    InvalidAccessToken,
    UnsupportedSurface,
    NotImplemented,
}

/// Safe allowlisted details only; never response text, URLs or arbitrary codes.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeErrorMetadata {
    pub http_status: Option<u16>,
    pub provider_code: Option<ClaudeProviderCode>,
    #[serde(with = "optional_utc", default)]
    pub retry_at: Option<SystemTime>,
    #[serde(with = "optional_utc", default)]
    pub resets_at: Option<SystemTime>,
    pub extra_usage: Option<ClaudeExtraUsage>,
    pub limit_scope: Option<ClaudeLimitScope>,
    pub expected_identity: Option<ClaudeIdentityBinding>,
    pub current_identity: Option<ClaudeIdentityBinding>,
    pub configuration: Option<ClaudeConfigurationError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeError {
    pub schema_version: u32,
    pub kind: ClaudeErrorKind,
    #[serde(serialize_with = "serialize_final_retry")]
    pub retry: ClaudeRetryDisposition,
    pub recovery: ClaudeRecovery,
    pub details: ClaudeErrorMetadata,
}

impl ClaudeError {
    pub fn new(kind: ClaudeErrorKind) -> Self {
        let recovery = match kind {
            ClaudeErrorKind::LoginRequired | ClaudeErrorKind::LoginExpired => ClaudeRecovery::Login,
            ClaudeErrorKind::AccountChanged => ClaudeRecovery::Reconnect,
            ClaudeErrorKind::CredentialStorageError => ClaudeRecovery::RepairStorage,
            ClaudeErrorKind::AuthTemporarilyUnavailable | ClaudeErrorKind::SubscriptionLimited => {
                ClaudeRecovery::RetryLater
            }
            ClaudeErrorKind::InvalidConfiguration
            | ClaudeErrorKind::ModelUnavailable
            | ClaudeErrorKind::AccessDenied => ClaudeRecovery::CheckConfiguration,
            ClaudeErrorKind::CompatibilityError => ClaudeRecovery::Upgrade,
            _ => ClaudeRecovery::None,
        };
        Self {
            schema_version: CLAUDE_SCHEMA_VERSION,
            kind,
            retry: ClaudeRetryDisposition::Terminal,
            recovery,
            details: ClaudeErrorMetadata::default(),
        }
    }
    pub fn configuration(reason: ClaudeConfigurationError) -> Self {
        let mut error = Self::new(ClaudeErrorKind::InvalidConfiguration);
        error.details.configuration = Some(reason);
        error.recovery = match reason {
            ClaudeConfigurationError::UnknownSchema | ClaudeConfigurationError::UnknownProfile => {
                ClaudeRecovery::Upgrade
            }
            ClaudeConfigurationError::MissingBinding => ClaudeRecovery::Reconnect,
            _ => error.recovery,
        };
        error
    }
    pub fn to_protocol(&self) -> mirror_protocol::ClaudeErrorDetails {
        mirror_protocol::ClaudeErrorDetails {
            schema_version: self.schema_version,
            kind: enum_name(self.kind),
            // Crossing into the worker/CLI is a final result. An internal
            // transport retry hint never restarts the agent's outer loop.
            retry_disposition: enum_name(ClaudeRetryDisposition::Terminal),
            recovery: enum_name(self.recovery),
            http_status: self.details.http_status.map(u32::from),
            provider_code: self.details.provider_code.map(enum_name),
            retry_at: self.details.retry_at.map(Into::into),
            resets_at: self.details.resets_at.map(Into::into),
            extra_usage: self.details.extra_usage.map(enum_name),
            limit_scope: self.details.limit_scope.map(enum_name),
            expected_identity: self.details.expected_identity.as_ref().map(Into::into),
            current_identity: self.details.current_identity.as_ref().map(Into::into),
            configuration: self.details.configuration.map(enum_name),
            compatibility_profile: profile::PROFILE_ID.into(),
        }
    }
}

fn serialize_final_retry<S: serde::Serializer>(
    _value: &ClaudeRetryDisposition,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    ClaudeRetryDisposition::Terminal.serialize(serializer)
}

fn enum_name(value: impl Serialize) -> String {
    // These closed unit enums serialize only to their fixed snake_case names.
    serde_json::to_value(value)
        .expect("unit enum serialization")
        .as_str()
        .expect("unit enum name")
        .into()
}

impl fmt::Display for ClaudeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Claude subscription {}", enum_name(self.kind))?;
        if let Some(reason) = self.details.configuration {
            f.write_str(match reason {
                ClaudeConfigurationError::ApiKeyNotAllowed => ": --api-key is not allowed with anthropic-oauth",
                ClaudeConfigurationError::EndpointNotAllowed => ": use the default Anthropic endpoint; unset conflicting ANTHROPIC_BASE_URL or select anthropic-messages",
                ClaudeConfigurationError::CodexAuthPathNotAllowed => ": Codex auth_path is not a Claude credential source",
                ClaudeConfigurationError::MissingBinding => ": establish this conversation's Claude identity before dispatch",
                ClaudeConfigurationError::UnknownSchema => ": unsupported schema version; upgrade Mirror",
                ClaudeConfigurationError::UnknownProfile => ": unsupported compatibility profile; explicit reconnect/update required",
                ClaudeConfigurationError::InvalidIdentity => ": invalid account/workspace identity",
                ClaudeConfigurationError::InvalidAccessToken => ": invalid access token",
                ClaudeConfigurationError::UnsupportedSurface => ": anthropic-oauth is not supported on this launch surface",
                ClaudeConfigurationError::NotImplemented => ": subscription auth/transport integration is not available in this build",
            })?;
        }
        match self.recovery {
            ClaudeRecovery::Login => f.write_str("; run `mirror auth login claude`"),
            ClaudeRecovery::Reconnect => {
                f.write_str("; use `/reconnect claude` to bind this conversation explicitly")
            }
            ClaudeRecovery::RepairStorage => {
                f.write_str("; repair Mirror's credential storage and retry")
            }
            ClaudeRecovery::RetryLater => f.write_str("; retry later without changing providers"),
            _ => Ok(()),
        }
    }
}
impl std::error::Error for ClaudeError {}

mod optional_utc {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &Option<SystemTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .map(|time| prost_types::Timestamp::from(time).to_string())
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| {
                text.parse::<prost_types::Timestamp>()
                    .map_err(|_| serde::de::Error::custom("invalid UTC timestamp"))?
                    .try_into()
                    .map_err(|_| serde::de::Error::custom("UTC timestamp out of range"))
            })
            .transpose()
    }
}
