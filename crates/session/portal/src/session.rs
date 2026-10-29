//! The `org.freedesktop.impl.portal.Session` objects a portal session
//! lives as.
//!
//! `ScreenCast`, `RemoteDesktop`, `InputCapture` and `GlobalShortcuts`
//! work over sessions: the frontend names a session object path at
//! `CreateSession`, the backend exports `org.freedesktop.impl.portal.Session`
//! there, and the session lasts until the frontend closes it (or leaves the
//! bus) or the backend ends it, telling the app with `Closed`: the person
//! stopped the share, the shared window went away. One registry holds them
//! all, since a `RemoteDesktop` session also takes `ScreenCast` calls; each
//! interface keeps what it needs per session in a map of its own and lets
//! go of it when the registry says a session ended ([`Sessions::on_end`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use futures_util::StreamExt as _;
use tilo_lock::MutexRecover as _;
use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalEmitter;

use crate::PortalCaller;
use crate::reply::Ended;

const MAX_SESSIONS: usize = 256;

/// What a session is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionKind {
    ScreenCast,
    RemoteDesktop,
    InputCapture,
    GlobalShortcuts,
}

/// One live session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveSession {
    /// The frontend connection that made it.
    pub owner: String,
    pub caller: PortalCaller,
    pub kind: SessionKind,
}

type OnEnd = Arc<dyn Fn(&str) + Send + Sync>;
type Wake = Arc<dyn Fn() + Send + Sync>;

/// Where the sessions are served, once they are.
struct Bus {
    connection: zbus::Connection,
    runtime: tokio::runtime::Handle,
}

#[derive(Default)]
struct Inner {
    live: Mutex<HashMap<String, LiveSession>>,
    on_end: Mutex<Vec<OnEnd>>,
    wake: Mutex<Option<Wake>>,
    changed: std::sync::atomic::AtomicBool,
    bus: OnceLock<Bus>,
}

/// The live sessions, by object path.
#[derive(Clone, Default)]
pub struct Sessions {
    inner: Arc<Inner>,
}

impl Sessions {
    /// Run `on_end` with the path of every session that ends, however it
    /// ends. It must not create or end sessions itself.
    pub fn on_end(&self, on_end: impl Fn(&str) + Send + Sync + 'static) {
        self.inner.on_end.lock_recover().push(Arc::new(on_end));
    }

    /// Call `wake` whenever what the portal publishes about its sessions
    /// changed, from whichever thread changed it.
    pub fn on_change(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.inner.wake.lock_recover() = Some(Arc::new(wake));
    }

    /// Say that what is published about the sessions changed.
    pub fn changed(&self) {
        self.inner
            .changed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let wake = self.inner.wake.lock_recover().clone();
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Whether anything changed since the last call.
    pub fn take_changed(&self) -> bool {
        self.inner
            .changed
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    /// Serve the sessions on `connection`: end those of frontends that
    /// leave the bus, and send `Closed` from there. Called from the
    /// runtime the connection runs on.
    pub fn serve_on(&self, connection: &zbus::Connection) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("portal sessions served outside a runtime: Closed is never sent");
            return;
        };
        if self
            .inner
            .bus
            .set(Bus {
                connection: connection.clone(),
                runtime: runtime.clone(),
            })
            .is_err()
        {
            return;
        }
        let sessions = self.clone();
        let watched = connection.clone();
        runtime.spawn(async move {
            let Ok(proxy) = zbus::fdo::DBusProxy::new(&watched).await else {
                return;
            };
            let Ok(mut changes) = proxy.receive_name_owner_changed().await else {
                return;
            };
            while let Some(change) = changes.next().await {
                let Ok(args) = change.args() else { continue };
                let name = args.name().as_str();
                if name.starts_with(':') && args.new_owner().is_none() {
                    sessions.close_owner(&watched, name).await;
                }
            }
        });
    }

    /// Run `future` on the runtime the sessions are served from; `false`
    /// before they are served.
    pub(crate) fn spawn(&self, future: impl Future<Output = ()> + Send + 'static) -> bool {
        self.inner
            .bus
            .get()
            .map(|bus| bus.runtime.spawn(future))
            .is_some()
    }

    /// The connection the sessions are served on.
    pub(crate) fn connection(&self) -> Option<zbus::Connection> {
        self.inner.bus.get().map(|bus| bus.connection.clone())
    }

    /// Create session `path` of `kind` for `caller`, made by frontend
    /// `owner`, and export it.
    ///
    /// # Errors
    /// [`Ended::Failed`] for a path that is no session path or is taken,
    /// too many sessions, or an object the bus refused.
    pub async fn create(
        &self,
        connection: &zbus::Connection,
        path: &str,
        owner: &str,
        caller: PortalCaller,
        kind: SessionKind,
    ) -> Result<(), Ended> {
        if !valid_session_path(path) {
            return Err(Ended::Failed);
        }
        {
            let mut live = self.inner.live.lock_recover();
            if live.contains_key(path) || live.len() >= MAX_SESSIONS {
                return Err(Ended::Failed);
            }
            live.insert(
                path.to_owned(),
                LiveSession {
                    owner: owner.to_owned(),
                    caller,
                    kind,
                },
            );
        }
        let object = SessionObject {
            sessions: self.clone(),
            path: path.to_owned(),
        };
        if connection.object_server().at(path, object).await.is_err() {
            self.inner.live.lock_recover().remove(path);
            return Err(Ended::Failed);
        }
        self.changed();
        Ok(())
    }

    /// Session `path`, when frontend `owner` made it for `caller`.
    #[must_use]
    pub fn owned(&self, path: &str, owner: &str, caller: &PortalCaller) -> Option<SessionKind> {
        self.inner
            .live
            .lock_recover()
            .get(path)
            .filter(|session| session.owner == owner && &session.caller == caller)
            .map(|session| session.kind)
    }

    /// Session `path`.
    #[must_use]
    pub fn get(&self, path: &str) -> Option<LiveSession> {
        self.inner.live.lock_recover().get(path).cloned()
    }

    /// The live sessions of `kind`, by path.
    #[must_use]
    pub fn of_kind(&self, kind: SessionKind) -> Vec<(String, LiveSession)> {
        let mut sessions: Vec<_> = self
            .inner
            .live
            .lock_recover()
            .iter()
            .filter(|(_, session)| session.kind == kind)
            .map(|(path, session)| (path.clone(), session.clone()))
            .collect();
        sessions.sort_by(|a, b| a.0.cmp(&b.0));
        sessions
    }

    /// Take session `path` out of the registry and let every interface go
    /// of it; `false` when it was not live.
    fn remove(&self, path: &str) -> bool {
        if self.inner.live.lock_recover().remove(path).is_none() {
            return false;
        }
        let on_end: Vec<OnEnd> = self.inner.on_end.lock_recover().clone();
        for on_end in on_end {
            on_end(path);
        }
        self.changed();
        true
    }

    /// Close session `path` for frontend `owner`; `false` when `owner` has
    /// no such session.
    pub fn close(&self, path: &str, owner: &str) -> bool {
        let owns = self
            .inner
            .live
            .lock_recover()
            .get(path)
            .is_some_and(|session| session.owner == owner);
        owns && self.remove(path)
    }

    /// End session `path` from the backend's side (the person stopped it,
    /// what it shared is gone): the app is told with `Closed`. `false`
    /// when it was not live.
    pub fn end(&self, path: &str) -> bool {
        if !self.remove(path) {
            return false;
        }
        if let Some(connection) = self.connection() {
            let path = path.to_owned();
            self.spawn(async move {
                if let Ok(emitter) = SignalEmitter::new(&connection, path.as_str()) {
                    let _ = SessionObject::closed(&emitter).await;
                }
                let _ = connection
                    .object_server()
                    .remove::<SessionObject, _>(path.as_str())
                    .await;
            });
        }
        true
    }

    /// Close every session of frontend `owner`, which left the bus.
    pub async fn close_owner(&self, connection: &zbus::Connection, owner: &str) {
        let paths: Vec<String> = self
            .inner
            .live
            .lock_recover()
            .iter()
            .filter(|(_, session)| session.owner == owner)
            .map(|(path, _)| path.clone())
            .collect();
        for path in paths {
            if self.close(&path, owner) {
                let _ = connection
                    .object_server()
                    .remove::<SessionObject, _>(path.as_str())
                    .await;
            }
        }
    }
}

impl crate::Portal {
    /// Authenticate a call on session `path` that carries no request
    /// handle: it comes from the frontend that made the session, for the
    /// session's app (`app_id` may be empty when the call names none).
    ///
    /// # Errors
    /// [`Ended::Failed`] for any other sender, app or path.
    pub(crate) async fn authenticate_session(
        &self,
        header: &zbus::message::Header<'_>,
        path: &str,
        app_id: &str,
    ) -> Result<(OwnedUniqueName, LiveSession), Ended> {
        let sender = header.sender().cloned().map(OwnedUniqueName::from);
        let owner = self
            .authenticator
            .authenticate_frontend(sender.as_ref())
            .await
            .map_err(|_| Ended::Failed)?;
        let session = self
            .sessions
            .get(path)
            .filter(|session| session.owner == owner.as_str())
            .filter(|session| app_id.is_empty() || session.caller.app_id == app_id)
            .ok_or(Ended::Failed)?;
        Ok((owner, session))
    }
}

/// A session path the frontend makes: under its own tree, bounded, and an
/// object path at all.
fn valid_session_path(path: &str) -> bool {
    path.len() <= 512
        && path.starts_with("/org/freedesktop/portal/desktop/session/")
        && zbus::zvariant::ObjectPath::try_from(path).is_ok()
}

struct SessionObject {
    sessions: Sessions,
    path: String,
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl SessionObject {
    #[zbus(property, name = "version")]
    const fn version(&self) -> u32 {
        1
    }

    async fn close(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let owner = header
            .sender()
            .cloned()
            .map(OwnedUniqueName::from)
            .ok_or_else(|| zbus::fdo::Error::AccessDenied("session has no D-Bus sender".into()))?;
        if !self.sessions.close(&self.path, owner.as_str()) {
            return Err(zbus::fdo::Error::AccessDenied(
                "session is not owned by this portal frontend".into(),
            ));
        }
        let _ = connection
            .object_server()
            .remove::<Self, _>(self.path.as_str())
            .await;
        Ok(())
    }

    #[zbus(signal)]
    async fn closed(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_ends_once_for_its_owner_and_every_interface_hears_it() {
        let sessions = Sessions::default();
        let ended = Arc::new(Mutex::new(Vec::new()));
        let heard = Arc::clone(&ended);
        sessions.on_end(move |path| heard.lock_recover().push(path.to_owned()));
        let path = "/org/freedesktop/portal/desktop/session/1_2/s";
        sessions.inner.live.lock_recover().insert(
            path.into(),
            LiveSession {
                owner: ":1.7".into(),
                caller: PortalCaller::new("org.example.Share"),
                kind: SessionKind::ScreenCast,
            },
        );
        assert_eq!(
            sessions.owned(path, ":1.7", &PortalCaller::new("org.example.Share")),
            Some(SessionKind::ScreenCast)
        );
        assert_eq!(
            sessions.owned(path, ":1.7", &PortalCaller::new("org.example.Other")),
            None
        );
        assert!(!sessions.close(path, ":1.8"));
        assert!(sessions.close(path, ":1.7"));
        assert!(!sessions.end(path));
        assert_eq!(*ended.lock_recover(), [path.to_owned()]);
        assert!(sessions.take_changed());
    }

    #[test]
    fn session_paths_stay_under_the_frontends_session_tree() {
        assert!(valid_session_path(
            "/org/freedesktop/portal/desktop/session/1_42/t1"
        ));
        assert!(!valid_session_path(
            "/org/freedesktop/portal/desktop/request/1_42/t1"
        ));
    }
}
