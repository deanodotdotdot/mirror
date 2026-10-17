//! Rust-owned subscription catalog, selected-model context and on-demand usage.
//! See README.md for endpoint evidence, cache ordering and normalized schemas.
mod cache;
mod http;
mod models;
mod usage;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use futures_util::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use serde::{Deserialize, Serialize};

use super::{
    profile, ClaudeAuthState, ClaudeAuthorization, ClaudeAuthorizationContext,
    ClaudeConfigurationError, ClaudeError, ClaudeErrorKind, ClaudeIdentityBinding,
    ClaudeTokenVersion, CLAUDE_PROVIDER,
};
use crate::providers::ModelRequestGuard;
use cache::{Cache, Failure, Key, Locked, Record, Timed};
use http::{Query, Transport};
pub use models::{
    CatalogSource, ClaudeModel, ClaudeModelContextResult, ClaudeModelsResult, ContextProvenance,
};
pub use usage::{
    AmountUnit, ClaudeUsageResult, ClaudeUsageWindow, ExtraUsage, ExtraUsageSource, ReportedAmount,
};

const BUDGET: Duration = Duration::from_secs(10);
const FINAL_VALIDATION_RESERVE: Duration = Duration::from_secs(1);
const CATALOG_TTL: u64 = 300;
const USAGE_TTL: u64 = 30;
type Cancellation<'a> = Shared<BoxFuture<'a, ()>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataState {
    Fresh,
    Cached,
    Stale,
    Offline,
    Unavailable,
}
#[derive(Debug, Clone, Serialize)]
pub struct MetadataInfo {
    pub schema_version: u32,
    pub provider: &'static str,
    pub identity: ClaudeIdentityBinding,
    pub state: MetadataState,
    #[serde(with = "timestamp")]
    pub fetched_at: Option<SystemTime>,
    pub age_seconds: Option<u64>,
    #[serde(with = "timestamp")]
    pub retry_at: Option<SystemTime>,
    pub error: Option<ClaudeError>,
}
impl MetadataInfo {
    fn new(
        key: &Key,
        state: MetadataState,
        fetched_at: Option<SystemTime>,
        error: Option<ClaudeError>,
    ) -> Self {
        Self {
            schema_version: 1,
            provider: CLAUDE_PROVIDER,
            identity: key.identity.clone(),
            state,
            fetched_at,
            age_seconds: fetched_at.map(|t| {
                SystemTime::now()
                    .duration_since(t)
                    .unwrap_or_default()
                    .as_secs()
            }),
            retry_at: error.as_ref().and_then(|e| e.details.retry_at),
            error,
        }
    }
}
fn compatibility() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::CompatibilityError)
}
fn temporary() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::AuthTemporarilyUnavailable)
}
fn cancelled() -> ClaudeError {
    ClaudeError::new(ClaudeErrorKind::RequestCancelled)
}
fn check_cancel(cancellation: &Cancellation<'_>) -> Result<(), ClaudeError> {
    if cancellation.clone().now_or_never().is_some() {
        Err(cancelled())
    } else {
        Ok(())
    }
}
fn check_environment() -> Result<(), ClaudeError> {
    if let Some(base) = std::env::var_os("ANTHROPIC_BASE_URL") {
        if base
            .to_str()
            .is_none_or(|v| !v.is_empty() && v.trim_end_matches('/') != profile::API_ORIGIN)
        {
            return Err(ClaudeError::configuration(
                ClaudeConfigurationError::EndpointNotAllowed,
            ));
        }
    }
    Ok(())
}
fn identity_error(
    expected: &ClaudeIdentityBinding,
    current: &ClaudeIdentityBinding,
) -> ClaudeError {
    let mut error = ClaudeError::new(ClaudeErrorKind::AccountChanged);
    error.details.expected_identity = Some(expected.clone());
    error.details.current_identity = Some(current.clone());
    error
}
struct FetchFailure {
    error: ClaudeError,
    blocks_fallback: bool,
}
impl FetchFailure {
    fn endpoint(error: ClaudeError) -> Self {
        let blocks_fallback = matches!(
            error.kind,
            ClaudeErrorKind::LoginRequired
                | ClaudeErrorKind::LoginExpired
                | ClaudeErrorKind::AccessDenied
                | ClaudeErrorKind::ModelUnavailable
                | ClaudeErrorKind::AccountChanged
                | ClaudeErrorKind::RequestCancelled
                | ClaudeErrorKind::CredentialStorageError
                | ClaudeErrorKind::InvalidConfiguration
        );
        Self {
            error,
            blocks_fallback,
        }
    }
    fn cache(self, previous: Option<&Failure>, usage: bool) -> Failure {
        let mut error = self.error;
        let delay = if usage { 60 } else { 5 };
        let retry_at = if self.blocks_fallback {
            None
        } else {
            Some(
                error
                    .details
                    .retry_at
                    .unwrap_or_else(|| SystemTime::now() + Duration::from_secs(delay)),
            )
        };
        error.details.retry_at = retry_at;
        Failure {
            error,
            retry_at,
            blocks_fallback: self.blocks_fallback || previous.is_some_and(|f| f.blocks_fallback),
        }
    }
}

/// Only this service reads metadata caches or performs metadata HTTP. The
/// authority remains the sole owner of tokens, refresh and local auth state.
pub struct ClaudeMetadataService {
    authority: Arc<dyn ClaudeAuthorization>,
    cache: Cache,
    transport: Arc<dyn Transport>,
}
impl ClaudeMetadataService {
    #[cfg(test)]
    pub(crate) fn for_test(
        authority: Arc<dyn ClaudeAuthorization>,
        directory: PathBuf,
        origin: String,
    ) -> Result<Self, ClaudeError> {
        Ok(Self {
            authority,
            cache: Cache::new(directory)?,
            transport: Arc::new(http::FixedTransport::loopback(origin)),
        })
    }

    pub fn new(authority: Arc<dyn ClaudeAuthorization>) -> Result<Self, ClaudeError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .ok_or_else(|| ClaudeError::new(ClaudeErrorKind::CredentialStorageError))?;
        Ok(Self {
            authority,
            cache: Cache::new(home.join(".mirror"))?,
            transport: Arc::new(http::FixedTransport::new()?),
        })
    }
    async fn bounded<T>(
        &self,
        deadline: tokio::time::Instant,
        cancellation: Cancellation<'_>,
        future: impl std::future::Future<Output = Result<T, ClaudeError>>,
    ) -> Result<T, ClaudeError> {
        let result = tokio::select! { biased;
            _=cancellation.clone()=>Err(cancelled()),
            result=tokio::time::timeout_at(deadline,future)=>result.unwrap_or_else(|_|Err(temporary())),
        };
        check_cancel(&cancellation)?;
        if tokio::time::Instant::now() >= deadline {
            return Err(temporary());
        }
        result
    }
    async fn key(
        &self,
        expected: Option<&ClaudeIdentityBinding>,
        cancellation: &Cancellation<'_>,
    ) -> Result<Key, ClaudeError> {
        self.key_guarded(expected, None, cancellation).await
    }
    async fn key_guarded(
        &self,
        expected: Option<&ClaudeIdentityBinding>,
        guard: Option<&dyn ModelRequestGuard>,
        cancellation: &Cancellation<'_>,
    ) -> Result<Key, ClaudeError> {
        check_cancel(cancellation)?;
        if expected.is_some_and(|id| id.account_id.is_nil() || id.organization_id.is_nil()) {
            return Err(ClaudeError::configuration(
                ClaudeConfigurationError::InvalidIdentity,
            ));
        }
        let snapshot = match guard {
            Some(guard) => {
                self.authority
                    .local_snapshot_guarded(guard, cancellation.clone().boxed())
                    .await?
            }
            None => {
                self.authority
                    .local_snapshot(cancellation.clone().boxed())
                    .await?
            }
        };
        check_cancel(cancellation)?;
        if !matches!(
            snapshot.status.state,
            ClaudeAuthState::Usable | ClaudeAuthState::Refreshable
        ) {
            return Err(snapshot
                .status
                .error
                .unwrap_or_else(|| ClaudeError::new(ClaudeErrorKind::LoginRequired)));
        }
        let identity = snapshot
            .status
            .identity
            .ok_or_else(|| ClaudeError::new(ClaudeErrorKind::LoginRequired))?;
        if identity.account_id.is_nil() || identity.organization_id.is_nil() {
            return Err(ClaudeError::configuration(
                ClaudeConfigurationError::InvalidIdentity,
            ));
        }
        if let Some(expected) = expected {
            if expected != &identity {
                return Err(identity_error(expected, &identity));
            }
        }
        let version = snapshot
            .version
            .ok_or_else(|| ClaudeError::new(ClaudeErrorKind::LoginRequired))?;
        Ok(Key {
            provider: CLAUDE_PROVIDER.into(),
            identity,
            generation: version.generation,
            compatibility_profile: profile::PROFILE_ID.into(),
        })
    }
    async fn validate(
        &self,
        key: &Key,
        guard: Option<&dyn ModelRequestGuard>,
        cancellation: &Cancellation<'_>,
    ) -> Result<(), ClaudeError> {
        let current = self
            .key_guarded(Some(&key.identity), guard, cancellation)
            .await?;
        if &current != key {
            return Err(temporary());
        }
        check_cancel(cancellation)
    }
    async fn start(
        &self,
        expected: Option<&ClaudeIdentityBinding>,
        guard: Option<&dyn ModelRequestGuard>,
        cancellation: &Cancellation<'_>,
    ) -> Result<(Locked, Record), ClaudeError> {
        check_environment()?;
        let key = self.key(expected, cancellation).await?;
        let locked = self.cache.lock().await?;
        // Revalidate after the possibly long metadata wait. Login/logout never
        // wait for this lock, so their changes cannot be hidden by a cache hit.
        self.validate(&key, guard, cancellation).await?;
        let record = locked.load(key)?;
        Ok((locked, record))
    }
    async fn finish(
        &self,
        locked: &Locked,
        record: &Record,
        guard: Option<&dyn ModelRequestGuard>,
        cancellation: &Cancellation<'_>,
        write: bool,
    ) -> Result<(), ClaudeError> {
        self.validate(&record.key, guard, cancellation).await?;
        locked.check()?;
        if write {
            locked.write(record)?;
        }
        check_cancel(cancellation)
    }
    async fn fetch(
        &self,
        key: &Key,
        query: Query<'_>,
        guard: Option<&dyn ModelRequestGuard>,
        cancellation: &Cancellation<'_>,
        recovered: &mut bool,
        remaining: &mut usize,
        deadline: tokio::time::Instant,
    ) -> Result<serde_json::Value, FetchFailure> {
        let mut rejected: Option<ClaudeTokenVersion> = None;
        loop {
            check_cancel(cancellation).map_err(FetchFailure::endpoint)?;
            let lease = self
                .authority
                .authorize(
                    ClaudeAuthorizationContext {
                        operation: query.operation(),
                        expected_identity: &key.identity,
                        request_guard: guard,
                        cancellation: cancellation.clone().boxed(),
                    },
                    rejected,
                )
                .await
                .map_err(|error| FetchFailure {
                    error,
                    blocks_fallback: true,
                })?;
            check_cancel(cancellation).map_err(FetchFailure::endpoint)?;
            if lease.version.generation != key.generation {
                return Err(FetchFailure {
                    error: temporary(),
                    blocks_fallback: true,
                });
            }
            let version = lease.version;
            // Leave time inside the original deadline to validate local auth
            // and publish a labeled stale/offline result after an HTTP stall.
            // Never turn an authorization timeout into endpoint fallback.
            let network_deadline = deadline - FINAL_VALIDATION_RESERVE;
            if tokio::time::Instant::now() >= network_deadline {
                return Err(FetchFailure::endpoint(temporary()));
            }
            let response = tokio::time::timeout_at(
                network_deadline,
                self.transport.get(lease, query, remaining),
            )
            .await
            .unwrap_or_else(|_| Err(temporary()));
            match response {
                Err(error) if error.details.http_status == Some(401) && !*recovered => {
                    *recovered = true;
                    rejected = Some(version);
                }
                result => return result.map_err(FetchFailure::endpoint),
            }
        }
    }
    pub async fn models(
        &self,
        expected_identity: Option<&ClaudeIdentityBinding>,
        request_guard: Option<&dyn ModelRequestGuard>,
        cancellation: BoxFuture<'_, ()>,
    ) -> Result<ClaudeModelsResult, ClaudeError> {
        let deadline = tokio::time::Instant::now() + BUDGET;
        let cancellation = cancellation.shared();
        self.bounded(deadline, cancellation.clone(), async {
            let (locked, mut record) = self
                .start(expected_identity, request_guard, &cancellation)
                .await?;
            let fresh = record
                .catalog
                .as_ref()
                .is_some_and(|v| v.fresh(CATALOG_TTL))
                && record.catalog_failure.is_none();
            if fresh {
                self.finish(&locked, &record, request_guard, &cancellation, false)
                    .await?;
                let catalog = record.catalog.as_ref().unwrap();
                return Ok(ClaudeModelsResult {
                    info: MetadataInfo::new(
                        &record.key,
                        MetadataState::Cached,
                        catalog.fetched_at,
                        None,
                    ),
                    source: CatalogSource::Remote,
                    models: catalog.data.clone(),
                });
            }
            let mut write = false;
            if !record
                .catalog_failure
                .as_ref()
                .is_some_and(Failure::cooling)
            {
                let result = async {
                    let mut seen = HashSet::new();
                    let mut cursors = HashSet::new();
                    let mut cursor = None;
                    let mut models = Vec::new();
                    let mut recovered = false;
                    let mut remaining = http::MAX_BYTES;
                    for _ in 0..models::MAX_PAGES {
                        let value = self
                            .fetch(
                                &record.key,
                                Query::Models(cursor.as_deref()),
                                request_guard,
                                &cancellation,
                                &mut recovered,
                                &mut remaining,
                                deadline,
                            )
                            .await?;
                        let page =
                            models::parse_page(value, &mut seen).map_err(FetchFailure::endpoint)?;
                        models.extend(page.models);
                        match page.next {
                            None => return Ok(models),
                            Some(next) => {
                                if !cursors.insert(next.clone()) {
                                    return Err(FetchFailure::endpoint(compatibility()));
                                }
                                cursor = Some(next);
                            }
                        }
                    }
                    Err(FetchFailure::endpoint(compatibility()))
                }
                .await;
                match result {
                    Ok(models) => {
                        record.catalog = Some(Timed::new(models));
                        record.catalog_failure = None;
                    }
                    Err(failure) => {
                        record.catalog_failure =
                            Some(failure.cache(record.catalog_failure.as_ref(), false))
                    }
                }
                write = true;
            }
            self.finish(&locked, &record, request_guard, &cancellation, write)
                .await?;
            if let Some(failure) = &record.catalog_failure {
                if failure.blocks_fallback {
                    return Err(failure.error.clone());
                }
            }
            let error = record.catalog_failure.as_ref().map(|f| f.error.clone());
            Ok(match &record.catalog {
                Some(catalog) => ClaudeModelsResult {
                    info: MetadataInfo::new(
                        &record.key,
                        if error.is_some() {
                            MetadataState::Stale
                        } else {
                            MetadataState::Fresh
                        },
                        catalog.fetched_at,
                        error,
                    ),
                    source: CatalogSource::Remote,
                    models: catalog.data.clone(),
                },
                None => ClaudeModelsResult {
                    info: MetadataInfo::new(&record.key, MetadataState::Offline, None, error),
                    source: CatalogSource::Bundled,
                    models: models::bundled(),
                },
            })
        })
        .await
    }
    pub async fn model_context(
        &self,
        expected_identity: &ClaudeIdentityBinding,
        model: &str,
        explicit_context_window: Option<u64>,
        request_guard: Option<&dyn ModelRequestGuard>,
        cancellation: BoxFuture<'_, ()>,
    ) -> Result<ClaudeModelContextResult, ClaudeError> {
        let deadline = tokio::time::Instant::now() + BUDGET;
        let cancellation = cancellation.shared();
        self.bounded(deadline, cancellation.clone(), async {
            check_environment()?;
            if !models::valid_id(model)
                || explicit_context_window.is_some_and(|v| v == 0 || v > models::MAX_CONTEXT)
            {
                return Err(ClaudeError::new(ClaudeErrorKind::InvalidConfiguration));
            }
            if let Some(context_window) = explicit_context_window {
                let key = self.key(Some(expected_identity), &cancellation).await?;
                self.validate(&key, request_guard, &cancellation).await?;
                return Ok(ClaudeModelContextResult {
                    info: MetadataInfo::new(&key, MetadataState::Fresh, None, None),
                    model: model.into(),
                    context_window,
                    provenance: ContextProvenance::Explicit,
                });
            }
            let (locked, mut record) = self
                .start(Some(expected_identity), request_guard, &cancellation)
                .await?;
            let known_failure = record.context_failures.get(model).or(record
                .catalog_failure
                .as_ref()
                .filter(|f| f.blocks_fallback));
            if known_failure.is_none() {
                let detail = record.contexts.get(model).filter(|c| c.fresh(CATALOG_TTL));
                let cached = detail
                    .filter(|c| c.data.context_window.is_some())
                    .map(|c| (&c.data, c.fetched_at))
                    .or_else(|| {
                        record
                            .catalog
                            .as_ref()
                            .filter(|c| c.fresh(CATALOG_TTL) || detail.is_some())
                            .and_then(|c| {
                                c.data
                                    .iter()
                                    .find(|m| m.id == model && m.context_window.is_some())
                                    .map(|m| (m, c.fetched_at))
                            })
                    })
                    .or_else(|| {
                        // A detail with no input limit can cache a fallback,
                        // but cannot hide validated context from the catalog.
                        detail.map(|c| (&c.data, c.fetched_at))
                    });
                if let Some((data, fetched_at)) = cached {
                    let (context_window, provenance) = data
                        .context_window
                        .map(|n| (n, ContextProvenance::ModelMetadata))
                        .unwrap_or_else(|| models::fallback(model));
                    self.finish(&locked, &record, request_guard, &cancellation, false)
                        .await?;
                    return Ok(ClaudeModelContextResult {
                        info: MetadataInfo::new(
                            &record.key,
                            if cache::fresh(fetched_at, CATALOG_TTL) {
                                MetadataState::Cached
                            } else {
                                MetadataState::Stale
                            },
                            fetched_at,
                            None,
                        ),
                        model: model.into(),
                        context_window,
                        provenance,
                    });
                }
            }
            let mut write = false;
            if !record
                .context_failures
                .get(model)
                .is_some_and(Failure::cooling)
            {
                let mut recovered = false;
                let mut remaining = http::MAX_BYTES;
                let result = self
                    .fetch(
                        &record.key,
                        Query::Context(model),
                        request_guard,
                        &cancellation,
                        &mut recovered,
                        &mut remaining,
                        deadline,
                    )
                    .await
                    .and_then(|value| {
                        let parsed = models::parse_model(&value).map_err(FetchFailure::endpoint)?;
                        if parsed.id != model {
                            return Err(FetchFailure::endpoint(compatibility()));
                        }
                        Ok(parsed)
                    });
                if record.contexts.len() >= cache::MAX_CONTEXTS {
                    record.contexts.clear();
                }
                if record.context_failures.len() >= cache::MAX_CONTEXTS {
                    record.context_failures.clear();
                }
                match result {
                    Ok(data) => {
                        record.contexts.insert(model.into(), Timed::new(data));
                        record.context_failures.remove(model);
                    }
                    Err(error) => {
                        let failure = error.cache(record.context_failures.get(model), false);
                        // A model-endpoint authentication failure also bars
                        // offline catalog success. A selected-model 404 does
                        // not imply anything about other models or usage.
                        if failure.blocks_fallback
                            && failure.error.kind != ClaudeErrorKind::ModelUnavailable
                        {
                            record.catalog_failure = Some(failure.clone());
                        }
                        record.context_failures.insert(model.into(), failure);
                    }
                }
                write = true;
            }
            self.finish(&locked, &record, request_guard, &cancellation, write)
                .await?;
            let failure = record.context_failures.get(model);
            if let Some(f) = failure {
                if f.blocks_fallback {
                    return Err(f.error.clone());
                }
            }
            // A known catalog auth failure cannot be hidden by an offline
            // context fallback. A successful selected detail request can stand.
            if failure.is_some() {
                if let Some(f) = record
                    .catalog_failure
                    .as_ref()
                    .filter(|f| f.blocks_fallback)
                {
                    return Err(f.error.clone());
                }
            }
            let data = record
                .contexts
                .get(model)
                .filter(|c| c.data.context_window.is_some())
                .map(|c| (&c.data, c.fetched_at))
                .or_else(|| {
                    record.catalog.as_ref().and_then(|c| {
                        c.data
                            .iter()
                            .find(|m| m.id == model && m.context_window.is_some())
                            .map(|m| (m, c.fetched_at))
                    })
                })
                .or_else(|| record.contexts.get(model).map(|c| (&c.data, c.fetched_at)));
            let (context_window, provenance) = data
                .and_then(|(model, _)| model.context_window)
                .map(|n| (n, ContextProvenance::ModelMetadata))
                .unwrap_or_else(|| models::fallback(model));
            let state = if failure.is_some() {
                if data.is_some() {
                    MetadataState::Stale
                } else {
                    MetadataState::Offline
                }
            } else if data.is_some_and(|(_, fetched)| !cache::fresh(fetched, CATALOG_TTL)) {
                // A successful detail may omit the input limit. If an older
                // catalog supplies it, preserve that source's actual age.
                MetadataState::Stale
            } else {
                MetadataState::Fresh
            };
            Ok(ClaudeModelContextResult {
                info: MetadataInfo::new(
                    &record.key,
                    state,
                    data.and_then(|(_, fetched)| fetched),
                    failure.map(|f| f.error.clone()),
                ),
                model: model.into(),
                context_window,
                provenance,
            })
        })
        .await
    }
    pub async fn usage(
        &self,
        expected_identity: Option<&ClaudeIdentityBinding>,
        request_guard: Option<&dyn ModelRequestGuard>,
        cancellation: BoxFuture<'_, ()>,
    ) -> Result<ClaudeUsageResult, ClaudeError> {
        let deadline = tokio::time::Instant::now() + BUDGET;
        let cancellation = cancellation.shared();
        self.bounded(deadline, cancellation.clone(), async {
            let (locked, mut record) = self
                .start(expected_identity, request_guard, &cancellation)
                .await?;
            let fresh = record.usage.as_ref().is_some_and(|u| u.fresh(USAGE_TTL))
                && record.usage_failure.is_none();
            let mut write = false;
            if !fresh && !record.usage_failure.as_ref().is_some_and(Failure::cooling) {
                let mut recovered = false;
                let mut remaining = http::MAX_BYTES;
                let result = self
                    .fetch(
                        &record.key,
                        Query::Usage,
                        request_guard,
                        &cancellation,
                        &mut recovered,
                        &mut remaining,
                        deadline,
                    )
                    .await
                    .and_then(|value| usage::parse(value).map_err(FetchFailure::endpoint));
                match result {
                    Ok(data) => {
                        record.usage = Some(Timed::new(data));
                        record.usage_failure = None;
                    }
                    Err(failure) => {
                        record.usage_failure =
                            Some(failure.cache(record.usage_failure.as_ref(), true))
                    }
                }
                write = true;
            }
            self.finish(&locked, &record, request_guard, &cancellation, write)
                .await?;
            if let Some(f) = &record.usage_failure {
                if f.blocks_fallback {
                    return Err(f.error.clone());
                }
            }
            let error = record.usage_failure.as_ref().map(|f| f.error.clone());
            let state = if fresh {
                MetadataState::Cached
            } else if error.is_none() {
                MetadataState::Fresh
            } else if record.usage.is_some() {
                MetadataState::Stale
            } else {
                MetadataState::Unavailable
            };
            let info = MetadataInfo::new(
                &record.key,
                state,
                record.usage.as_ref().and_then(|u| u.fetched_at),
                error,
            );
            let data = record.usage.map(|u| u.data).unwrap_or_default();
            Ok(ClaudeUsageResult {
                info,
                windows: data.windows,
                extra_usage: data.extra_usage,
            })
        })
        .await
    }
}
pub(super) mod timestamp {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &Option<SystemTime>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value
            .map(|v| prost_types::Timestamp::from(v).to_string())
            .serialize(s)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|v| {
                v.parse::<prost_types::Timestamp>()
                    .map_err(|_| serde::de::Error::custom("invalid UTC timestamp"))?
                    .try_into()
                    .map_err(|_| serde::de::Error::custom("invalid UTC timestamp"))
            })
            .transpose()
    }
}
#[cfg(test)]
mod tests;
