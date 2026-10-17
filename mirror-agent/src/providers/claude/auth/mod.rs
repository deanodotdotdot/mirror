//! Mirror-owned Claude grant lifecycle. Tokens never enter provider configuration or IPC.

mod login;
mod oauth;
mod store;

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{future::BoxFuture, FutureExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::*;
use oauth::{OAuthTransport, TokenRequest};
use store::{ReauthReason, ReauthRequired, Record, Store};

pub use login::{ClaudeCallbackInput, ClaudeLoginEvent};

/// Fixed native progress phases; never constructed from upstream/input strings.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Protocol,
    Preparing,
    AwaitingCallback,
    ExchangingCode,
    ResolvingIdentity,
    Committing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitOutcome {
    NotCommitted,
    Unknown,
}

#[derive(Debug)]
pub struct CommandError {
    pub error: ClaudeError,
    pub phase: Phase,
    pub commit_outcome: Option<CommitOutcome>,
}
impl CommandError {
    fn new(error: ClaudeError, phase: Phase) -> Self {
        Self {
            error,
            phase,
            commit_outcome: Some(CommitOutcome::NotCommitted),
        }
    }
}
impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}
impl std::error::Error for CommandError {}

pub(super) fn storage_error() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::CredentialStorageError)
}
pub(super) fn temporary_error() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::AuthTemporarilyUnavailable)
}
pub(super) fn configuration_error() -> ClaudeError {
    ClaudeError::configuration(ClaudeConfigurationError::EndpointNotAllowed)
}
fn invalid_token() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::CompatibilityError)
}
fn cancelled() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::RequestCancelled)
}

#[derive(Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct Secret(String);
impl Secret {
    fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([REDACTED])")
    }
}

#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    access_token: Secret,
    #[serde(default)]
    refresh_token: Secret,
    #[serde(with = "store::timestamp")]
    expires_at: SystemTime,
    #[serde(with = "store::timestamp")]
    authorized_at: SystemTime,
    account_id: Uuid,
    organization_id: Uuid,
    email: Option<String>,
    organization_name: Option<String>,
    scopes: Vec<String>,
}

fn secret_valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= 8192 && value.bytes().all(|byte| byte > b' ' && byte < 0x7f)
}
fn label_valid(value: &Option<String>) -> bool {
    value
        .as_ref()
        .is_none_or(|value| value.len() <= 1024 && !value.chars().any(char::is_control))
}
impl Credential {
    fn identity(&self) -> ClaudeIdentityBinding {
        ClaudeIdentityBinding {
            account_id: self.account_id,
            organization_id: self.organization_id,
        }
    }
    fn validate(&self) -> Result<(), ClaudeError> {
        if !secret_valid(self.access_token.expose())
            || !self.access_token.expose().starts_with("sk-ant-oat")
            || (!self.refresh_token.expose().is_empty()
                && !secret_valid(self.refresh_token.expose()))
            || self.account_id.is_nil()
            || self.organization_id.is_nil()
            || self.authorized_at <= UNIX_EPOCH
            || self.expires_at <= self.authorized_at
            || self.scopes.len() > 32
            || !self.scopes.iter().any(|scope| scope == "user:inference")
            || self
                .scopes
                .iter()
                .any(|scope| scope.len() > 128 || !secret_valid(scope))
            || !label_valid(&self.email)
            || !label_valid(&self.organization_name)
        {
            return Err(invalid_token());
        }
        Ok(())
    }
    fn from_token(
        value: &Value,
        previous: Option<&Self>,
        now: SystemTime,
    ) -> Result<Self, ClaudeError> {
        let value = value.as_object().ok_or_else(invalid_token)?;
        let access = value
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(invalid_token)?;
        let refresh = match value.get("refresh_token") {
            Some(value) => value.as_str().ok_or_else(invalid_token)?,
            None => previous
                .map(|old| old.refresh_token.expose())
                .ok_or_else(invalid_token)?,
        };
        if value
            .get("token_type")
            .and_then(Value::as_str)
            .is_none_or(|kind| !kind.eq_ignore_ascii_case("bearer"))
        {
            return Err(invalid_token());
        }
        let expiry = value
            .get("expires_in")
            .and_then(Value::as_u64)
            .filter(|seconds| *seconds > 0 && *seconds <= 366 * 86400)
            .ok_or_else(invalid_token)?;
        let scopes = match value.get("scope") {
            Some(Value::String(scopes)) => scopes.split_whitespace().map(str::to_owned).collect(),
            Some(Value::Array(scopes)) => scopes
                .iter()
                .map(|scope| scope.as_str().map(str::to_owned).ok_or_else(invalid_token))
                .collect::<Result<_, _>>()?,
            None if previous.is_some() => previous.unwrap().scopes.clone(),
            _ => return Err(invalid_token()),
        };
        let id = |field: &str, fallback: Option<Uuid>| -> Result<Uuid, ClaudeError> {
            match value.get(field) {
                Some(object) => match object.as_object().ok_or_else(invalid_token)?.get("uuid") {
                    Some(value) => value
                        .as_str()
                        .and_then(|text| text.parse().ok())
                        .filter(|id: &Uuid| !id.is_nil())
                        .ok_or_else(invalid_token),
                    None => fallback.ok_or_else(invalid_token),
                },
                None => fallback.ok_or_else(invalid_token),
            }
        };
        let account_id = id("account", previous.map(|old| old.account_id))?;
        let organization_id = id("organization", previous.map(|old| old.organization_id))?;
        if previous.is_some_and(|old| {
            old.account_id != account_id || old.organization_id != organization_id
        }) {
            return Err(ClaudeError::new(ClaudeErrorKind::AccountChanged));
        }
        let label = |object: &str,
                     field: &str,
                     fallback: Option<&String>|
         -> Result<Option<String>, ClaudeError> {
            match value.get(object).and_then(|value| value.get(field)) {
                None | Some(Value::Null) => Ok(fallback.cloned()),
                Some(Value::String(text)) => Ok(Some(text.clone())),
                _ => Err(invalid_token()),
            }
        };
        let credential = Self {
            access_token: Secret(access.into()),
            refresh_token: Secret(refresh.into()),
            expires_at: now
                .checked_add(Duration::from_secs(expiry))
                .ok_or_else(invalid_token)?,
            authorized_at: previous.map_or(now, |old| old.authorized_at),
            account_id,
            organization_id,
            scopes,
            email: label(
                "account",
                "email_address",
                previous.and_then(|old| old.email.as_ref()),
            )?,
            organization_name: label(
                "organization",
                "name",
                previous.and_then(|old| old.organization_name.as_ref()),
            )?,
        };
        credential.validate()?;
        if credential.refresh_token.expose().is_empty() {
            return Err(invalid_token());
        }
        Ok(credential)
    }
}

/// Uses only ~/.mirror/claude_oauth.json. Constructing it never reads a grant.
pub struct ClaudeAuthManager {
    store: Store,
    transport: Arc<dyn OAuthTransport>,
}

impl ClaudeAuthManager {
    pub fn new() -> Result<Self, ClaudeError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(storage_error)?;
        Ok(Self {
            store: Store::new(home.join(".mirror"))?,
            transport: oauth::transport()?,
        })
    }

    pub async fn status(
        &self,
        cancellation: BoxFuture<'_, ()>,
    ) -> Result<ClaudeAuthStatus, ClaudeError> {
        Ok(self.local_snapshot(cancellation).await?.status)
    }

    pub async fn logout(
        &self,
        cancellation: BoxFuture<'_, ()>,
    ) -> Result<ClaudeAuthStatus, CommandError> {
        let cancellation = cancellation.shared();
        tokio::select! { biased;
            _ = cancellation.clone() => Err(CommandError::new(cancelled(), Phase::Preparing)),
            result = async {
                let locked = self.store.lock(true).await.map_err(|error| CommandError::new(error, Phase::Preparing))?.ok_or_else(|| CommandError::new(storage_error(), Phase::Preparing))?;
                let previous = locked.load().map_err(|error| CommandError::new(error, Phase::Preparing))?;
                let next = Record::tombstone(previous.as_ref());
                if cancellation.clone().now_or_never().is_some() {
                    return Err(CommandError::new(cancelled(), Phase::Committing));
                }
                locked.write(&next)?;
                Ok(snapshot(Some(&next)).status)
            } => result,
        }
    }

    async fn authorize_inner(
        &self,
        operation: ClaudeOperation,
        expected_identity: &ClaudeIdentityBinding,
        request_guard: Option<&dyn crate::providers::ModelRequestGuard>,
        rejected: Option<ClaudeTokenVersion>,
    ) -> Result<ClaudeAccessLease, ClaudeError> {
        oauth::check_environment()?;
        if expected_identity.account_id.is_nil() || expected_identity.organization_id.is_nil() {
            return Err(ClaudeError::configuration(
                ClaudeConfigurationError::InvalidIdentity,
            ));
        }
        // The operation is a closed enum with a fixed reviewed HTTPS origin.
        debug_assert!(operation.endpoint().starts_with(profile::API_ORIGIN));
        if let Some(guard) = request_guard {
            guard.verify().await.map_err(|error| match error {
                crate::providers::LlmError::Claude(error) => error,
                _ => cancelled(),
            })?;
        }
        let locked = self
            .store
            .lock(false)
            .await?
            .ok_or_else(|| ClaudeError::new(ClaudeErrorKind::LoginRequired))?;
        let mut record = locked
            .load()?
            .ok_or_else(|| ClaudeError::new(ClaudeErrorKind::LoginRequired))?;
        let old = record
            .credential
            .as_ref()
            .ok_or_else(|| ClaudeError::new(ClaudeErrorKind::LoginRequired))?;
        check_identity(expected_identity, &old.identity())?;
        if record.reauth_required.is_some() || old.refresh_token.expose().is_empty() {
            return Err(ClaudeError::new(ClaudeErrorKind::LoginRequired));
        }
        let now = SystemTime::now();
        let near_expiry =
            old.expires_at.duration_since(now).unwrap_or_default() < Duration::from_secs(300);
        let same_rejected = rejected == Some(record.version());
        if same_rejected
            && record.last_refreshed_at.is_some_and(|minted| {
                now.duration_since(minted).unwrap_or_default() < Duration::from_secs(60)
            })
        {
            return Err(temporary_error());
        }
        // A different persisted revision wins even if its short lifetime remains
        // within the normal skew window: concurrent waiters reuse that refresh.
        let changed_rejected = rejected.is_some_and(|version| version != record.version());
        let expired = old.expires_at <= now;
        let recently_renewed = !expired
            && record.last_refreshed_at.is_some_and(|minted| {
                now.duration_since(minted).unwrap_or_default() < Duration::from_secs(60)
            });
        if same_rejected || (near_expiry && (!changed_rejected || expired) && !recently_renewed) {
            let prior = record.version();
            let next_revision = record.revision.checked_add(1).ok_or_else(storage_error)?;
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                self.transport
                    .token(TokenRequest::Refresh(&old.refresh_token)),
            )
            .await
            .map_err(|_| temporary_error())?;
            let data = match result {
                Ok(value) => value,
                Err(error) => {
                    // Reload once before treating an older revoked refresh as
                    // definitive. Never poison a newer generation or revision.
                    let current = locked
                        .load()?
                        .ok_or_else(|| ClaudeError::new(ClaudeErrorKind::LoginRequired))?;
                    if current != record {
                        return Err(temporary_error());
                    }
                    if matches!(
                        (error.details.http_status, error.details.provider_code),
                        (Some(400 | 401), Some(ClaudeProviderCode::InvalidGrant))
                            | (Some(401), Some(ClaudeProviderCode::AuthenticationError))
                    ) {
                        record.reauth_required = Some(ReauthRequired {
                            generation: prior.generation,
                            revision: prior.revision,
                            reason: if error.details.provider_code
                                == Some(ClaudeProviderCode::InvalidGrant)
                            {
                                ReauthReason::InvalidGrant
                            } else {
                                ReauthReason::Revoked
                            },
                        });
                        locked.write(&record).map_err(|error| error.error)?;
                    }
                    return Err(error);
                }
            };
            let refreshed_at = SystemTime::now();
            let credential = Credential::from_token(&data, Some(old), refreshed_at)?;
            let current = locked
                .load()?
                .ok_or_else(|| ClaudeError::new(ClaudeErrorKind::LoginRequired))?;
            if current != record {
                return Err(temporary_error());
            }
            record.credential = Some(credential);
            record.revision = next_revision;
            record.last_refreshed_at = Some(refreshed_at);
            locked.write(&record).map_err(|error| error.error)?;
        }
        if let Some(guard) = request_guard {
            guard.verify().await.map_err(|error| match error {
                crate::providers::LlmError::Claude(error) => error,
                _ => cancelled(),
            })?;
        }
        // Revalidate the stable lock and persisted bytes after the last await.
        // A replaced lock must not authorize a stale lease after another process
        // has already logged out under its replacement. Failed/uncertain writes
        // likewise never publish a token.
        let current = locked
            .load()?
            .ok_or_else(|| ClaudeError::new(ClaudeErrorKind::LoginRequired))?;
        if current != record {
            return Err(temporary_error());
        }
        let credential = current
            .credential
            .as_ref()
            .ok_or_else(|| ClaudeError::new(ClaudeErrorKind::LoginRequired))?;
        check_identity(expected_identity, &credential.identity())?;
        if credential.expires_at <= SystemTime::now() {
            return Err(temporary_error());
        }
        Ok(ClaudeAccessLease {
            access_token: ClaudeAccessToken::new(credential.access_token.expose().into())?,
            identity: credential.identity(),
            installation_id: current.installation_id,
            version: current.version(),
            expires_at: credential.expires_at,
            operation: operation,
        })
    }
}

fn check_identity(
    expected: &ClaudeIdentityBinding,
    current: &ClaudeIdentityBinding,
) -> Result<(), ClaudeError> {
    if expected == current {
        return Ok(());
    }
    let mut error = ClaudeError::new(ClaudeErrorKind::AccountChanged);
    error.details.expected_identity = Some(expected.clone());
    error.details.current_identity = Some(current.clone());
    Err(error)
}

fn snapshot(record: Option<&Record>) -> ClaudeLocalAuthSnapshot {
    let mut status = ClaudeAuthStatus::new(ClaudeAuthState::LoggedOut);
    if let Some(record) = record {
        if let Some(credential) = &record.credential {
            status = ClaudeAuthStatus::new(
                if record.reauth_required.is_some() || credential.refresh_token.expose().is_empty()
                {
                    ClaudeAuthState::LoginRequired
                } else if credential
                    .expires_at
                    .duration_since(SystemTime::now())
                    .unwrap_or_default()
                    < Duration::from_secs(300)
                {
                    ClaudeAuthState::Refreshable
                } else {
                    ClaudeAuthState::Usable
                },
            );
            status.identity = Some(credential.identity());
            status.display = Some(ClaudeAccountDisplay {
                email: credential.email.clone(),
                organization_name: credential.organization_name.clone(),
            });
            status.expires_at = Some(credential.expires_at);
            if status.state == ClaudeAuthState::LoginRequired {
                status.error = Some(ClaudeError::new(ClaudeErrorKind::LoginRequired));
            }
        }
    }
    ClaudeLocalAuthSnapshot {
        status,
        version: record.map(Record::version),
    }
}

impl ClaudeAuthorization for ClaudeAuthManager {
    fn authorize<'a>(
        &'a self,
        context: ClaudeAuthorizationContext<'a>,
        rejected: Option<ClaudeTokenVersion>,
    ) -> BoxFuture<'a, Result<ClaudeAccessLease, ClaudeError>> {
        Box::pin(async move {
            let ClaudeAuthorizationContext {
                operation,
                expected_identity,
                request_guard,
                mut cancellation,
            } = context;
            let lease = tokio::select! { biased;
                _ = &mut cancellation => Err(cancelled()),
                result = self.authorize_inner(operation, expected_identity, request_guard, rejected) => result,
            }?;
            // A ready refresh/guard can make cancellation ready in the same
            // poll after select checked it. Keep any completed durable rotation,
            // but never hand that cancelled operation a dispatch lease.
            if cancellation.now_or_never().is_some() {
                return Err(cancelled());
            }
            Ok(lease)
        })
    }
    fn local_snapshot<'a>(
        &'a self,
        cancellation: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<ClaudeLocalAuthSnapshot, ClaudeError>> {
        Box::pin(async move {
            tokio::select! { biased;
                _ = cancellation => Err(cancelled()),
                result = async {
                    let Some(locked) = self.store.lock(false).await? else { return Ok(snapshot(None)); };
                    Ok(snapshot(locked.load()?.as_ref()))
                } => result,
            }
        })
    }

    fn local_snapshot_guarded<'a>(
        &'a self,
        request_guard: &'a dyn crate::providers::ModelRequestGuard,
        mut cancellation: BoxFuture<'a, ()>,
    ) -> BoxFuture<'a, Result<ClaudeLocalAuthSnapshot, ClaudeError>> {
        Box::pin(async move {
            let result = tokio::select! { biased;
                _ = &mut cancellation => Err(cancelled()),
                result = async {
                    let Some(locked) = self.store.lock(false).await? else {
                        return Ok(snapshot(None));
                    };
                    request_guard.verify().await.map_err(|error| match error {
                        crate::providers::LlmError::Claude(error) => error,
                        _ => cancelled(),
                    })?;
                    // No await after the guard: load checks the stable lock and
                    // reads the current record before the snapshot is published.
                    Ok(snapshot(locked.load()?.as_ref()))
                } => result,
            }?;
            if cancellation.now_or_never().is_some() {
                return Err(cancelled());
            }
            Ok(result)
        })
    }
}

#[cfg(test)]
mod tests;
