//! Authentication for calls made to the portal *implementation* backend.
//!
//! The implementation bus name is not an application API.  Only the process
//! currently owning `org.freedesktop.portal.Desktop` may call it; that trusted
//! frontend has already derived `app_id` from the original application's
//! credentials.  Workspace attribution is a separate, injected lookup because
//! every backend call itself has the frontend's D-Bus sender, not the app's.
//!
//! The frontend can name a host process's app only from its own systemd
//! unit (`app-<launcher>-<id>-<n>.scope` with an `<id>.desktop` beside it),
//! so for an app the session launched itself it asserts the empty id. A
//! call that carries a request handle names its requester, though: the
//! frontend makes the handle
//! `/org/freedesktop/portal/desktop/request/<sender>/<token>` from the
//! caller's unique bus name.
//! [`PortalAuthenticator::authenticate_request`] asks the bus for that
//! connection's process and resolves it to the launch the session
//! supervises (normally the app's `xdg-dbus-proxy`, a child of its
//! launcher), so the app is named by its launch, not by a string.

use std::collections::HashMap;
use std::sync::Arc;

use zbus::names::{BusName, OwnedUniqueName};

use crate::PortalCaller;

pub const FRONTEND_BUS_NAME: &str = "org.freedesktop.portal.Desktop";
const MAX_APP_ID_BYTES: usize = 255;
/// Where the frontend exports the `Request` object of a call it forwards.
const REQUEST_PATH_PREFIX: &str = "/org/freedesktop/portal/desktop/request/";

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

/// Resolves the process behind a requester's bus connection to the live
/// launch that owns it.
pub type RequesterResolver = Arc<dyn Fn(u32) -> Option<PortalCaller> + Send + Sync>;

#[derive(Clone)]
enum FrontendAuthority {
    SessionBus(zbus::Connection),
    /// Explicit injection for p2p protocol tests, which have no bus daemon:
    /// the frontend's name and the process behind each requester's name.
    Fixed(OwnedUniqueName, HashMap<String, u32>),
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
    requesters: Option<RequesterResolver>,
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
            (None, FrontendAuthority::Fixed(owner, _)) => owner,
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
            requesters: None,
        }
    }

    /// P2P test authenticator with an explicit trusted peer. Never use this
    /// constructor in compositor bring-up.
    pub fn fixed_for_tests(
        owner: OwnedUniqueName,
        resolver: Arc<dyn PortalCallerResolver>,
    ) -> Self {
        Self {
            authority: FrontendAuthority::Fixed(owner, HashMap::new()),
            resolver,
            requesters: None,
        }
    }

    /// Name a request's requester by the launch its process belongs to.
    #[must_use]
    pub fn with_requesters(mut self, requesters: RequesterResolver) -> Self {
        self.requesters = Some(requesters);
        self
    }

    /// The process behind each requester's unique name, for a test
    /// authenticator that has no bus to ask.
    #[must_use]
    pub fn with_requester_pids_for_tests(mut self, pids: HashMap<String, u32>) -> Self {
        if let FrontendAuthority::Fixed(_, known) = &mut self.authority {
            *known = pids;
        }
        self
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
        let expected = self.authenticate_frontend(sender).await?;
        self.resolve_asserted(expected, app_id, parent_window)
    }

    /// [`Self::authenticate`] for a call carrying the request handle the
    /// frontend made for it. The requester's own launch names the app; the
    /// frontend's id, when it has one, must agree with it. A requester that
    /// is no supervised launch is judged by the frontend's id alone.
    pub async fn authenticate_request(
        &self,
        sender: Option<&OwnedUniqueName>,
        handle: &str,
        app_id: &str,
        parent_window: Option<&str>,
    ) -> Result<AuthenticatedCall, AuthenticationError> {
        let expected = self.authenticate_frontend(sender).await?;
        let Some(caller) = self.requester(handle).await else {
            if app_id.is_empty() {
                return Err(AuthenticationError::UnattributedRequester);
            }
            return self.resolve_asserted(expected, app_id, parent_window);
        };
        if !app_id.is_empty() && caller.app_id != app_id {
            return Err(AuthenticationError::ApplicationMismatch);
        }
        Ok(AuthenticatedCall {
            owner: expected,
            caller,
        })
    }

    /// The live launch behind the connection that made the request `handle`.
    async fn requester(&self, handle: &str) -> Option<PortalCaller> {
        let resolve = self.requesters.as_ref()?;
        let name = requester_of_handle(handle)?;
        let pid = match &self.authority {
            FrontendAuthority::Fixed(_, pids) => *pids.get(name.as_str())?,
            FrontendAuthority::SessionBus(connection) => zbus::fdo::DBusProxy::new(connection)
                .await
                .ok()?
                .get_connection_unix_process_id(BusName::from(name))
                .await
                .ok()?,
        };
        resolve(pid)
    }

    fn resolve_asserted(
        &self,
        expected: OwnedUniqueName,
        app_id: &str,
        parent_window: Option<&str>,
    ) -> Result<AuthenticatedCall, AuthenticationError> {
        validate_app_id(app_id)?;
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
            FrontendAuthority::Fixed(owner, _) => Ok(owner.clone()),
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

/// The unique bus name a request handle was made for: the frontend drops
/// its leading `:` and spells each `.` as `_`. Bus daemons hand out names of
/// digits and dots only, so anything else names no requester.
fn requester_of_handle(handle: &str) -> Option<OwnedUniqueName> {
    let escaped = handle
        .strip_prefix(REQUEST_PATH_PREFIX)?
        .split('/')
        .next()?;
    if escaped.is_empty() || !escaped.bytes().all(|b| b.is_ascii_digit() || b == b'_') {
        return None;
    }
    OwnedUniqueName::try_from(format!(":{}", escaped.replace('_', "."))).ok()
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
    #[error("the requester is no launch the compositor supervises and the frontend named no app")]
    UnattributedRequester,
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

    fn auth(owner: &str) -> PortalAuthenticator {
        PortalAuthenticator::fixed_for_tests(
            owner.try_into().expect("unique name"),
            Arc::new(|app: &str, _: Option<&str>| Some(PortalCaller::new(app))),
        )
    }

    #[tokio::test]
    async fn only_the_injected_frontend_and_resolved_app_ids_are_accepted() {
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

    #[test]
    fn a_request_handle_names_its_requester() {
        let name = |handle: &str| requester_of_handle(handle).map(|n| n.as_str().to_string());
        assert_eq!(
            name("/org/freedesktop/portal/desktop/request/1_42/t1").as_deref(),
            Some(":1.42")
        );
        // The frontend appends a counter to a token already in use.
        assert_eq!(
            name("/org/freedesktop/portal/desktop/request/1_42/t1/7").as_deref(),
            Some(":1.42")
        );
        assert_eq!(
            name("/org/freedesktop/portal/desktop/session/1_42/t1"),
            None
        );
        assert_eq!(name("/org/freedesktop/portal/desktop/request//t1"), None);
        assert_eq!(name("/org/freedesktop/portal/desktop/request/x_y/t1"), None);
    }

    #[tokio::test]
    async fn a_request_is_attributed_to_its_requesters_launch() {
        const HANDLE: &str = "/org/freedesktop/portal/desktop/request/1_42/t";
        let owner: OwnedUniqueName = ":1.7".try_into().unwrap();
        let notes = || PortalCaller::new("so.tilo.notes");
        let by_launch = |known: bool| {
            PortalAuthenticator::fixed_for_tests(
                owner.clone(),
                Arc::new(|_: &str, _: Option<&str>| None),
            )
            .with_requesters(Arc::new(move |pid| {
                (known && pid == 4242).then(|| PortalCaller::new("so.tilo.notes"))
            }))
            .with_requester_pids_for_tests(HashMap::from([(":1.42".to_string(), 4242)]))
        };

        // The frontend could not name the app: its launch does.
        let call = by_launch(true)
            .authenticate_request(Some(&owner), HANDLE, "", None)
            .await
            .unwrap();
        assert_eq!(call.caller, notes());
        // A frontend that names it must name the same app.
        assert!(
            by_launch(true)
                .authenticate_request(Some(&owner), HANDLE, "so.tilo.notes", None)
                .await
                .is_ok()
        );
        assert_eq!(
            by_launch(true)
                .authenticate_request(Some(&owner), HANDLE, "so.tilo.mail", None)
                .await
                .unwrap_err(),
            AuthenticationError::ApplicationMismatch
        );
        // A process no launch owns, named by nobody, is refused.
        assert_eq!(
            by_launch(false)
                .authenticate_request(Some(&owner), HANDLE, "", None)
                .await
                .unwrap_err(),
            AuthenticationError::UnattributedRequester
        );
        // Only the frontend's calls count.
        let attacker: OwnedUniqueName = ":1.8".try_into().unwrap();
        assert_eq!(
            by_launch(true)
                .authenticate_request(Some(&attacker), HANDLE, "", None)
                .await
                .unwrap_err(),
            AuthenticationError::UntrustedSender
        );
    }
}
