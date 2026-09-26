//! Authentication for calls made to the portal *implementation* backend.
//!
//! The implementation bus name is not an application API.  Only the process
//! currently owning `org.freedesktop.portal.Desktop` may call it; that trusted
//! frontend has already derived `app_id` from the original application's
//! credentials.  Workspace attribution is a separate, injected lookup because
//! every backend call itself has the frontend's D-Bus sender, not the app's.

use std::sync::Arc;

use zbus::names::{BusName, OwnedUniqueName};

use super::{PortalCaller, PortalContext};

pub const FRONTEND_BUS_NAME: &str = "org.freedesktop.portal.Desktop";
const MAX_APP_ID_BYTES: usize = 255;

/// Resolves a frontend-authenticated application id (and, where present, its
/// exported parent window) to a concrete Tilo application instance.
pub trait PortalCallerResolver: Send + Sync {
    fn resolve(&self, app_id: &str, parent_window: Option<&str>) -> Option<PortalCaller>;
}

impl<F> PortalCallerResolver for F
where
    F: Fn(&str, Option<&str>) -> Option<PortalCaller> + Send + Sync,
{
    fn resolve(&self, app_id: &str, parent_window: Option<&str>) -> Option<PortalCaller> {
        self(app_id, parent_window)
    }
}

#[derive(Clone)]
enum FrontendAuthority {
    SessionBus(zbus::Connection),
    /// Explicit injection for p2p protocol tests, which have no bus daemon.
    Fixed(OwnedUniqueName),
}

/// A verified backend call. `owner` is retained so request/session registries
/// can bind their objects to the frontend generation that created them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedCall {
    pub owner: OwnedUniqueName,
    pub caller: PortalCaller,
}

#[derive(Clone)]
pub struct PortalAuthenticator {
    authority: FrontendAuthority,
    resolver: Arc<dyn PortalCallerResolver>,
}

impl PortalAuthenticator {
    /// Authenticate a call which intentionally carries no application id
    /// (`PermissionStore` and `Lockdown`).  This establishes only that the
    /// portal frontend made the request; policy authorization remains with
    /// the compositor integration.
    pub async fn authenticate_frontend(
        &self,
        sender: Option<&OwnedUniqueName>,
    ) -> Result<OwnedUniqueName, AuthenticationError> {
        let expected = self.frontend_owner().await?;
        let sender = match (sender, &self.authority) {
            (Some(sender), _) => sender,
            (None, FrontendAuthority::Fixed(owner)) => owner,
            (None, FrontendAuthority::SessionBus(_)) => {
                return Err(AuthenticationError::MissingSender);
            }
        };
        (sender == &expected)
            .then_some(expected)
            .ok_or(AuthenticationError::UntrustedSender)
    }

    /// Production authenticator. The well-known name owner is queried for
    /// every call, so a restarted frontend cannot inherit the old one's
    /// request/session authority.
    pub fn session_bus(
        connection: zbus::Connection,
        resolver: Arc<dyn PortalCallerResolver>,
    ) -> Self {
        Self {
            authority: FrontendAuthority::SessionBus(connection),
            resolver,
        }
    }

    /// P2P test authenticator with an explicit trusted peer. Never use this
    /// constructor in compositor bring-up.
    pub fn fixed_for_tests(
        owner: OwnedUniqueName,
        resolver: Arc<dyn PortalCallerResolver>,
    ) -> Self {
        Self {
            authority: FrontendAuthority::Fixed(owner),
            resolver,
        }
    }

    /// Authenticate the backend sender, validate the asserted app id, then
    /// resolve it to a real Tilo application instance. Unknown/ambiguous apps
    /// fail closed; there is deliberately no synthetic workspace zero.
    pub async fn authenticate(
        &self,
        sender: Option<&OwnedUniqueName>,
        app_id: &str,
        parent_window: Option<&str>,
    ) -> Result<AuthenticatedCall, AuthenticationError> {
        validate_app_id(app_id)?;
        let expected = self.authenticate_frontend(sender).await?;
        let caller = self
            .resolver
            .resolve(app_id, parent_window)
            .ok_or(AuthenticationError::UnknownApplication)?;
        if caller.app_id != app_id {
            return Err(AuthenticationError::ApplicationMismatch);
        }
        Ok(AuthenticatedCall {
            owner: expected,
            caller,
        })
    }

    async fn frontend_owner(&self) -> Result<OwnedUniqueName, AuthenticationError> {
        match &self.authority {
            FrontendAuthority::Fixed(owner) => Ok(owner.clone()),
            FrontendAuthority::SessionBus(connection) => {
                let proxy = zbus::fdo::DBusProxy::new(connection)
                    .await
                    .map_err(|_| AuthenticationError::FrontendUnavailable)?;
                let name = BusName::try_from(FRONTEND_BUS_NAME)
                    .map_err(|_| AuthenticationError::FrontendUnavailable)?;
                proxy
                    .get_name_owner(name)
                    .await
                    .map_err(|_| AuthenticationError::FrontendUnavailable)
            }
        }
    }
}

/// Shared adapter entry point. A missing authenticator is not a compatibility
/// mode: it means the backend was brought up without an identity boundary and
/// the request must be refused.
pub async fn authenticate_call(
    ctx: &PortalContext,
    sender: Option<&OwnedUniqueName>,
    app_id: &str,
    parent_window: Option<&str>,
) -> Result<AuthenticatedCall, AuthenticationError> {
    let authenticator = ctx
        .authenticator
        .as_ref()
        .ok_or(AuthenticationError::FrontendUnavailable)?;
    authenticator
        .authenticate(sender, app_id, parent_window)
        .await
}

/// Authenticate the backend sender for implementation interfaces that don't
/// have an app id in their wire signature.
pub async fn authenticate_frontend_call(
    ctx: &PortalContext,
    sender: Option<&OwnedUniqueName>,
) -> Result<OwnedUniqueName, AuthenticationError> {
    ctx.authenticator
        .as_ref()
        .ok_or(AuthenticationError::FrontendUnavailable)?
        .authenticate_frontend(sender)
        .await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AuthenticationError {
    #[error("portal backend call has no D-Bus sender")]
    MissingSender,
    #[error("portal frontend is not available")]
    FrontendUnavailable,
    #[error("sender is not the portal frontend")]
    UntrustedSender,
    #[error("invalid application id")]
    InvalidApplicationId,
    #[error("application has no unambiguous Tilo instance")]
    UnknownApplication,
    #[error("resolved application does not match the frontend assertion")]
    ApplicationMismatch,
}

fn validate_app_id(app_id: &str) -> Result<(), AuthenticationError> {
    if app_id.is_empty()
        || app_id.len() > MAX_APP_ID_BYTES
        || app_id.trim() != app_id
        || app_id.chars().any(char::is_control)
    {
        return Err(AuthenticationError::InvalidApplicationId);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceId;

    fn auth(owner: &str) -> PortalAuthenticator {
        PortalAuthenticator::fixed_for_tests(
            owner.try_into().expect("unique name"),
            Arc::new(|app: &str, _: Option<&str>| Some(PortalCaller::new(WorkspaceId(9), app))),
        )
    }

    #[tokio::test]
    async fn only_the_injected_frontend_and_resolved_app_ids_are_accepted() {
        // Phase `only_the_injected_frontend_is_accepted`.
        {
            let trusted: OwnedUniqueName = ":1.7".try_into().unwrap();
            let attacker: OwnedUniqueName = ":1.8".try_into().unwrap();
            assert!(
                auth(":1.7")
                    .authenticate(Some(&trusted), "org.example.App", None)
                    .await
                    .is_ok()
            );
            assert_eq!(
                auth(":1.7")
                    .authenticate(Some(&attacker), "org.example.App", None)
                    .await
                    .unwrap_err(),
                AuthenticationError::UntrustedSender
            );
        }
        // Phase `unresolved_and_hostile_app_ids_fail_closed`.
        {
            let owner: OwnedUniqueName = ":1.7".try_into().unwrap();
            let unresolved = PortalAuthenticator::fixed_for_tests(
                owner.clone(),
                Arc::new(|_: &str, _: Option<&str>| None),
            );
            assert_eq!(
                unresolved
                    .authenticate(Some(&owner), "org.example.App", None)
                    .await
                    .unwrap_err(),
                AuthenticationError::UnknownApplication
            );
            assert_eq!(
                auth(":1.7")
                    .authenticate(Some(&owner), " bad\napp ", None)
                    .await
                    .unwrap_err(),
                AuthenticationError::InvalidApplicationId
            );
        }
    }
}
