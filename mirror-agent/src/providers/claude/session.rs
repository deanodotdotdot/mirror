//! Native conversation identity. Consumers can read; only the service publishes
//! a replacement, after its store has committed it.
use std::sync::{Arc, RwLock};

use futures_util::FutureExt;

use super::{
    ClaudeAuthState, ClaudeAuthorization, ClaudeConfigurationError, ClaudeError, ClaudeErrorKind,
    ClaudeIdentityBinding, ClaudeSessionBinding,
};

#[derive(Clone, Default)]
pub struct ClaudeSession(Arc<RwLock<Option<ClaudeSessionBinding>>>);

impl ClaudeSession {
    pub(crate) fn new(binding: Option<ClaudeSessionBinding>) -> Self {
        Self(Arc::new(RwLock::new(binding)))
    }

    pub fn binding(&self) -> Option<ClaudeSessionBinding> {
        self.0.read().expect("Claude binding lock").clone()
    }

    pub(crate) fn publish(&self, binding: ClaudeSessionBinding) {
        *self.0.write().expect("Claude binding lock") = Some(binding);
    }
}

pub(crate) async fn local_identity(
    authority: &dyn ClaudeAuthorization,
) -> Result<ClaudeIdentityBinding, ClaudeError> {
    let status = authority
        .local_snapshot(std::future::pending().boxed())
        .await?
        .status;
    if !matches!(
        status.state,
        ClaudeAuthState::Usable | ClaudeAuthState::Refreshable
    ) {
        return Err(status
            .error
            .unwrap_or_else(|| ClaudeError::new(ClaudeErrorKind::LoginRequired)));
    }
    status
        .identity
        .ok_or_else(|| ClaudeError::configuration(ClaudeConfigurationError::InvalidIdentity))
}

pub(crate) fn binding_changed(
    expected: Option<&ClaudeSessionBinding>,
    current: Option<&ClaudeSessionBinding>,
) -> ClaudeError {
    let mut error = ClaudeError::new(ClaudeErrorKind::AccountChanged);
    error.details.expected_identity = expected.map(|binding| binding.identity.clone());
    error.details.current_identity = current.map(|binding| binding.identity.clone());
    error
}

/// Native opt-in does not inspect credentials or require a HOME for an API-key
/// conversation. Construct the sole authority only when Claude is actually used.
#[derive(Default)]
pub(crate) struct NativeAuthority(
    std::sync::OnceLock<Result<super::auth::ClaudeAuthManager, ClaudeError>>,
);
impl NativeAuthority {
    #[cfg(test)]
    pub(crate) fn for_test(manager: super::auth::ClaudeAuthManager) -> Self {
        Self(std::sync::OnceLock::from(Ok(manager)))
    }
    fn manager(&self) -> Result<&super::auth::ClaudeAuthManager, ClaudeError> {
        self.0
            .get_or_init(super::auth::ClaudeAuthManager::new)
            .as_ref()
            .map_err(Clone::clone)
    }
}
impl ClaudeAuthorization for NativeAuthority {
    fn authorize<'a>(
        &'a self,
        context: super::ClaudeAuthorizationContext<'a>,
        rejected: Option<super::ClaudeTokenVersion>,
    ) -> futures_util::future::BoxFuture<'a, Result<super::ClaudeAccessLease, ClaudeError>> {
        Box::pin(async move { self.manager()?.authorize(context, rejected).await })
    }
    fn local_snapshot<'a>(
        &'a self,
        cancellation: futures_util::future::BoxFuture<'a, ()>,
    ) -> futures_util::future::BoxFuture<'a, Result<super::ClaudeLocalAuthSnapshot, ClaudeError>>
    {
        Box::pin(async move { self.manager()?.local_snapshot(cancellation).await })
    }
    fn local_snapshot_guarded<'a>(
        &'a self,
        guard: &'a dyn crate::providers::ModelRequestGuard,
        cancellation: futures_util::future::BoxFuture<'a, ()>,
    ) -> futures_util::future::BoxFuture<'a, Result<super::ClaudeLocalAuthSnapshot, ClaudeError>>
    {
        Box::pin(async move {
            self.manager()?
                .local_snapshot_guarded(guard, cancellation)
                .await
        })
    }
}
