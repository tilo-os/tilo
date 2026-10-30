//! The `org.freedesktop.impl.portal.Request` objects a request lives as.
//!
//! The portal frontend hands the backend a request object path with every
//! call; the backend exports `org.freedesktop.impl.portal.Request` there
//! while the request is live, so the frontend can close it when the app
//! withdraws (or goes away). Closing ends what the request was waiting for:
//! the question it asked is withdrawn, an inhibition is lifted. Only the
//! frontend connection that made a request may close it, and a frontend
//! that leaves the bus takes all of its requests with it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt as _;
use tilo_lock::MutexRecover as _;
use tokio::sync::oneshot;
use zbus::interface;
use zbus::names::OwnedUniqueName;

use crate::reply::{Ended, ensure};

/// What a live request runs when it ends by being closed.
type OnClose = Box<dyn FnOnce() + Send>;

struct Live {
    /// The frontend connection that made the request.
    owner: String,
    on_close: OnClose,
}

/// The live requests, by object path.
#[derive(Clone, Default)]
pub struct Requests {
    live: Arc<Mutex<HashMap<String, Live>>>,
}

impl Requests {
    /// Export request `path` for `owner` until it is closed or finished;
    /// `on_close` runs when the frontend closes it (or leaves). `false`
    /// when the path is no request path or is already live.
    ///
    /// # Errors
    /// The object server refused the object.
    pub async fn export(
        &self,
        connection: &zbus::Connection,
        path: &str,
        owner: &str,
        on_close: impl FnOnce() + Send + 'static,
    ) -> zbus::Result<bool> {
        if !valid_request_path(path) {
            return Ok(false);
        }
        {
            let mut live = self.live.lock_recover();
            if live.contains_key(path) {
                return Ok(false);
            }
            live.insert(
                path.to_owned(),
                Live {
                    owner: owner.to_owned(),
                    on_close: Box::new(on_close),
                },
            );
        }
        let object = RequestObject {
            requests: self.clone(),
            path: path.to_owned(),
        };
        if let Err(error) = connection.object_server().at(path, object).await {
            self.live.lock_recover().remove(path);
            return Err(error);
        }
        Ok(true)
    }

    /// Run `body` as request `path` of `owner`: it ends cancelled when the
    /// frontend closes the request first, and dropping `body` then
    /// withdraws whatever it waited for.
    ///
    /// # Errors
    /// [`Ended::Failed`] when the request could not be exported;
    /// [`Ended::Cancelled`] when it was closed.
    pub async fn run<T>(
        &self,
        connection: &zbus::Connection,
        path: &str,
        owner: &str,
        body: impl Future<Output = Result<T, Ended>>,
    ) -> Result<T, Ended> {
        let (closed, on_closed) = oneshot::channel::<()>();
        let exported = self
            .export(connection, path, owner, move || {
                let _ = closed.send(());
            })
            .await;
        ensure(matches!(exported, Ok(true)))?;
        let outcome = tokio::select! {
            outcome = body => outcome,
            _ = on_closed => Err(Ended::Cancelled),
        };
        self.finish(connection, path).await;
        outcome
    }

    /// Take request `path` down without running its close action: it
    /// ended on its own.
    pub async fn finish(&self, connection: &zbus::Connection, path: &str) {
        self.live.lock_recover().remove(path);
        let _ = connection
            .object_server()
            .remove::<RequestObject, _>(path)
            .await;
    }

    /// Close request `path` for `owner`; `false` when `owner` has no such
    /// request.
    pub fn close(&self, path: &str, owner: &str) -> bool {
        let closed = {
            let mut live = self.live.lock_recover();
            match live.get(path) {
                Some(entry) if entry.owner == owner => live.remove(path),
                _ => None,
            }
        };
        closed.map(|entry| (entry.on_close)()).is_some()
    }

    /// Close every request of `owner`, which left the bus.
    pub async fn close_owner(&self, connection: &zbus::Connection, owner: &str) {
        let paths: Vec<String> = self
            .live
            .lock_recover()
            .iter()
            .filter(|(_, entry)| entry.owner == owner)
            .map(|(path, _)| path.clone())
            .collect();
        for path in paths {
            if self.close(&path, owner) {
                let _ = connection
                    .object_server()
                    .remove::<RequestObject, _>(path.as_str())
                    .await;
            }
        }
    }

    /// Whether request `path` is live and was made by `owner`.
    #[must_use]
    pub fn is_owned(&self, path: &str, owner: &str) -> bool {
        self.live
            .lock_recover()
            .get(path)
            .is_some_and(|entry| entry.owner == owner)
    }

    /// Whether request `path` is live.
    #[must_use]
    pub fn is_live(&self, path: &str) -> bool {
        self.live.lock_recover().contains_key(path)
    }

    /// Close the requests of every frontend connection that leaves the bus.
    pub fn close_with_their_owners(&self, connection: &zbus::Connection) {
        let requests = self.clone();
        let watched = connection.clone();
        connection
            .executor()
            .spawn(
                async move {
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
                            requests.close_owner(&watched, name).await;
                        }
                    }
                },
                "portal requests of departed frontends",
            )
            .detach();
    }
}

/// A request path the frontend makes: under its own tree, bounded, and an
/// object path at all.
fn valid_request_path(path: &str) -> bool {
    path.len() <= 512
        && path.starts_with("/org/freedesktop/portal/desktop/request/")
        && zbus::zvariant::ObjectPath::try_from(path).is_ok()
}

struct RequestObject {
    requests: Requests,
    path: String,
}

#[interface(name = "org.freedesktop.impl.portal.Request")]
impl RequestObject {
    async fn close(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> zbus::fdo::Result<()> {
        let owner = header
            .sender()
            .cloned()
            .map(OwnedUniqueName::from)
            .ok_or_else(|| zbus::fdo::Error::AccessDenied("request has no D-Bus sender".into()))?;
        if !self.requests.close(&self.path, owner.as_str()) {
            return Err(zbus::fdo::Error::AccessDenied(
                "request is not owned by this portal frontend".into(),
            ));
        }
        let _ = connection
            .object_server()
            .remove::<Self, _>(self.path.as_str())
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn counted(requests: &Requests, path: &str, owner: &str, closes: &Arc<AtomicUsize>) {
        let closes = Arc::clone(closes);
        requests.live.lock_recover().insert(
            path.to_owned(),
            Live {
                owner: owner.to_owned(),
                on_close: Box::new(move || {
                    closes.fetch_add(1, Ordering::SeqCst);
                }),
            },
        );
    }

    #[test]
    fn only_the_owning_frontend_closes_a_request_and_only_once() {
        let requests = Requests::default();
        let closes = Arc::new(AtomicUsize::new(0));
        counted(
            &requests,
            "/org/freedesktop/portal/desktop/request/1_2/t",
            ":1.7",
            &closes,
        );
        assert!(!requests.close("/org/freedesktop/portal/desktop/request/1_2/t", ":1.8"));
        assert!(requests.close("/org/freedesktop/portal/desktop/request/1_2/t", ":1.7"));
        assert!(!requests.close("/org/freedesktop/portal/desktop/request/1_2/t", ":1.7"));
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn request_paths_stay_under_the_frontends_request_tree() {
        assert!(valid_request_path(
            "/org/freedesktop/portal/desktop/request/1_42/t1"
        ));
        assert!(!valid_request_path(
            "/org/freedesktop/portal/desktop/session/1_42/t1"
        ));
        assert!(!valid_request_path("/org/example/request/1_42/t1"));
        assert!(!valid_request_path(&format!(
            "/org/freedesktop/portal/desktop/request/{}",
            "a".repeat(600)
        )));
    }
}
