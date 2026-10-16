//! Shared lifecycle objects for the implementation-side portal protocol.
//!
//! The portal frontend supplies a request or session object path with every
//! operation. That path is not just an opaque correlation id: the backend is
//! required to export `org.freedesktop.impl.portal.Request` while a dialog is
//! live and `org.freedesktop.impl.portal.Session` while a long-lived operation
//! is live. This module owns both the object-server plumbing and the
//! fail-closed ownership checks, so individual portals cannot accidentally
//! make a handle closable by a different frontend connection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalEmitter;

use super::reply::{Ended, ensure};

/// Resource that must be torn down when a request or session ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LifecycleResource {
    Prompt,
    Inhibitor,
    Clipboard,
    GlobalShortcuts,
    InputCapture,
    RemoteDesktop,
    ScreenCast,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleEntry {
    /// Unique name of the authenticated portal frontend that created this
    /// resource. Never accept an app-provided id in its place.
    pub owner: String,
    pub app_id: String,
    pub resource: LifecycleResource,
}

type Cleanup = Arc<dyn Fn(&LifecycleEntry) + Send + Sync + 'static>;

#[derive(Clone)]
struct ManagedEntry {
    entry: LifecycleEntry,
    cleanup: Cleanup,
}

#[derive(Clone, Default)]
pub struct PortalLifecycle {
    requests: Arc<Mutex<HashMap<String, ManagedEntry>>>,
    sessions: Arc<Mutex<HashMap<String, ManagedEntry>>>,
    /// Poked when a session ends, so clipboard revocation does not poll.
    pub(crate) wake: crate::idle_wake::IdleWake,
}

impl PortalLifecycle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert an entry with no external resource cleanup. This is useful for
    /// pure bookkeeping and keeps focused unit tests simple.
    pub fn insert_request(&self, path: impl Into<String>, entry: LifecycleEntry) -> bool {
        self.insert_request_with_cleanup(path, entry, |_| {})
    }

    pub fn insert_session(&self, path: impl Into<String>, entry: LifecycleEntry) -> bool {
        self.insert_session_with_cleanup(path, entry, |_| {})
    }

    pub fn insert_request_with_cleanup(
        &self,
        path: impl Into<String>,
        entry: LifecycleEntry,
        cleanup: impl Fn(&LifecycleEntry) + Send + Sync + 'static,
    ) -> bool {
        insert(
            &self.requests,
            path.into(),
            ManagedEntry {
                entry,
                cleanup: Arc::new(cleanup),
            },
        )
    }

    pub fn insert_session_with_cleanup(
        &self,
        path: impl Into<String>,
        entry: LifecycleEntry,
        cleanup: impl Fn(&LifecycleEntry) + Send + Sync + 'static,
    ) -> bool {
        insert(
            &self.sessions,
            path.into(),
            ManagedEntry {
                entry,
                cleanup: Arc::new(cleanup),
            },
        )
    }

    /// Remove a request only when the authenticated frontend still owns it.
    /// Cleanup runs after releasing the registry lock: cleanup is allowed to
    /// wake a D-Bus task or acquire its portal's own mutex.
    pub fn close_request(&self, path: &str, owner: &str) -> Option<LifecycleEntry> {
        close(&self.requests, path, Some(owner))
    }

    /// Finish a request from its owning backend. Unlike `Close`, this is not
    /// a D-Bus entry point and thus doesn't need the owner argument.
    pub fn finish_request(&self, path: &str) -> Option<LifecycleEntry> {
        close(&self.requests, path, None)
    }

    /// Is this live request owned by this authenticated frontend generation?
    /// `UpdateChoices` has no app-id argument, so this completes its authority
    /// check after sender authentication.
    pub fn has_request(&self, path: &str, owner: &str) -> bool {
        has(&self.requests, path, owner, None)
    }

    /// Remove a session only when the authenticated frontend still owns it.
    pub fn close_session(&self, path: &str, owner: &str) -> Option<LifecycleEntry> {
        let closed = close(&self.sessions, path, Some(owner));
        if closed.is_some() {
            self.wake.poke();
        }
        closed
    }

    /// Finish a session from the compositor/backend (for example, output
    /// loss). The caller should use `close_session_object` to also emit the
    /// mandated `Session.Closed` signal.
    pub fn finish_session(&self, path: &str) -> Option<LifecycleEntry> {
        let closed = close(&self.sessions, path, None);
        if closed.is_some() {
            self.wake.poke();
        }
        closed
    }

    pub fn has_session(&self, path: &str, owner: &str, app_id: &str) -> bool {
        has(&self.sessions, path, owner, Some(app_id))
    }

    /// Ownership check for methods whose standard signature carries only a
    /// session path (for example Inhibit.QueryEndResponse).
    pub fn has_session_owner(&self, path: &str, owner: &str) -> bool {
        has(&self.sessions, path, owner, None)
    }

    /// Disconnect cleanup does not trust app-provided paths: it removes every
    /// resource belonging to the departed authenticated frontend.
    pub fn close_owner(&self, owner: &str) -> Vec<LifecycleEntry> {
        let mut released = close_matching(&self.requests, owner);
        let sessions = close_matching(&self.sessions, owner);
        if !sessions.is_empty() {
            self.wake.poke();
        }
        released.extend(sessions);
        released
    }

    /// Disconnect cleanup including D-Bus object removal. Session objects get
    /// their required `Closed` signal before being unregistered.
    pub async fn close_owner_objects(&self, connection: &zbus::Connection, owner: &str) {
        let request_paths = paths_for_owner(&self.requests, owner);
        let session_paths = paths_for_owner(&self.sessions, owner);
        for path in request_paths {
            let _ = self.close_request(&path, owner);
            let _ = connection
                .object_server()
                .remove::<RequestObject, _>(path.as_str())
                .await;
        }
        for path in session_paths {
            self.close_session_object(connection, &path).await;
        }
    }

    /// Watch the session bus for frontend disconnection. A unique bus name
    /// disappearing is the only ownership change that proves the frontend
    /// itself exited; changes to a well-known name intentionally do nothing.
    /// On p2p test connections there is no bus daemon, so this task simply
    /// returns without weakening the explicit Close ownership checks.
    pub fn spawn_disconnect_cleanup(&self, connection: zbus::Connection) {
        let lifecycle = self.clone();
        let task_connection = connection.clone();
        connection
            .executor()
            .spawn(
                async move {
                    let Ok(mut gone) = disconnected_peers(&task_connection).await else {
                        return;
                    };
                    while let Some(owner) = gone.next().await {
                        lifecycle
                            .close_owner_objects(&task_connection, &owner)
                            .await;
                    }
                },
                "portal frontend disconnect cleanup",
            )
            .detach();
    }

    /// Export a request object at `path`. The path is constrained to portal
    /// object-space so an untrusted frontend cannot attach an interface over
    /// a compositor-owned D-Bus object.
    pub async fn register_request(
        &self,
        connection: &zbus::Connection,
        path: &str,
        entry: LifecycleEntry,
        cleanup: impl Fn(&LifecycleEntry) + Send + Sync + 'static,
    ) -> zbus::Result<bool> {
        if !valid_portal_path(path, "/request/")
            || !self.insert_request_with_cleanup(path, entry, cleanup)
        {
            return Ok(false);
        }
        let object = RequestObject {
            lifecycle: self.clone(),
            path: path.into(),
        };
        if let Err(error) = connection.object_server().at(path, object).await {
            // A failed registration must not strand its privileged resource.
            let _ = self.finish_request(path);
            return Err(error);
        }
        Ok(true)
    }

    /// Run `body` while a request object is exported at `path`. A failed
    /// registration ends the call as failed without running `body`; the
    /// object is removed however `body` ends, so no early return can strand
    /// it on the bus.
    pub async fn with_request<T>(
        &self,
        connection: &zbus::Connection,
        path: &str,
        entry: LifecycleEntry,
        cleanup: impl Fn(&LifecycleEntry) + Send + Sync + 'static,
        body: impl Future<Output = Result<T, Ended>>,
    ) -> Result<T, Ended> {
        let registered = self
            .register_request(connection, path, entry, cleanup)
            .await;
        ensure(matches!(registered, Ok(true)))?;
        let outcome = body.await;
        self.complete_request_object(connection, path).await;
        outcome
    }

    /// Export a session object at `path` and arrange for cleanup on Close.
    pub async fn register_session(
        &self,
        connection: &zbus::Connection,
        path: &str,
        entry: LifecycleEntry,
        cleanup: impl Fn(&LifecycleEntry) + Send + Sync + 'static,
    ) -> zbus::Result<bool> {
        if !valid_portal_path(path, "/session/")
            || !self.insert_session_with_cleanup(path, entry, cleanup)
        {
            return Ok(false);
        }
        let object = SessionObject {
            lifecycle: self.clone(),
            path: path.into(),
        };
        if let Err(error) = connection.object_server().at(path, object).await {
            let _ = self.finish_session(path);
            return Err(error);
        }
        Ok(true)
    }

    /// Remove a completed request's D-Bus object. It is harmless when a
    /// concurrent `Request.Close` already removed it. Normal completion must
    /// not invoke the cancellation cleanup: that callback is exclusively for
    /// an explicit Close, frontend disconnect, or registration failure.
    pub async fn complete_request_object(&self, connection: &zbus::Connection, path: &str) {
        let _ = take_without_cleanup(&self.requests, path);
        let _ = connection
            .object_server()
            .remove::<RequestObject, _>(path)
            .await;
    }

    /// Close a session from backend/compositor code and notify the portal
    /// frontend. This is the one route that emits `Session.Closed`; a client
    /// initiated `Close` deliberately has the same observable signal so its
    /// state machine cannot retain a stale session.
    pub async fn close_session_object(&self, connection: &zbus::Connection, path: &str) {
        if self.finish_session(path).is_some() {
            if let Ok(context) = SignalEmitter::new(connection, path) {
                let _ = SessionObject::closed(&context).await;
            }
        }
        let _ = connection
            .object_server()
            .remove::<SessionObject, _>(path)
            .await;
    }
}

/// The unique bus names (`:1.42`) of connections as they close. A unique
/// name losing its owner is the only ownership change that proves a peer
/// exited; well-known names changing hands are left out.
///
/// # Errors
///
/// Fails when the bus does not answer the `NameOwnerChanged` subscription,
/// as on a p2p connection with no bus daemon.
pub(crate) async fn disconnected_peers(
    connection: &zbus::Connection,
) -> zbus::Result<impl futures_util::Stream<Item = String> + use<>> {
    let proxy = zbus::fdo::DBusProxy::new(connection).await?;
    let changes = proxy.receive_name_owner_changed().await?;
    Ok(changes.filter_map(|change| {
        let gone = change.args().ok().and_then(|args| {
            let name = args.name().as_str();
            (name.starts_with(':') && args.new_owner().is_none()).then(|| name.to_owned())
        });
        std::future::ready(gone)
    }))
}

fn insert(map: &Mutex<HashMap<String, ManagedEntry>>, path: String, entry: ManagedEntry) -> bool {
    map.lock()
        .is_ok_and(|mut entries| entries.insert(path, entry).is_none())
}

fn close(
    map: &Mutex<HashMap<String, ManagedEntry>>,
    path: &str,
    expected_owner: Option<&str>,
) -> Option<LifecycleEntry> {
    let managed = {
        let mut entries = map.lock().ok()?;
        let existing = entries.get(path)?;
        if expected_owner.is_some_and(|owner| existing.entry.owner != owner) {
            return None;
        }
        entries.remove(path)?
    };
    (managed.cleanup)(&managed.entry);
    Some(managed.entry)
}

fn take_without_cleanup(
    map: &Mutex<HashMap<String, ManagedEntry>>,
    path: &str,
) -> Option<LifecycleEntry> {
    map.lock().ok()?.remove(path).map(|managed| managed.entry)
}

fn has(
    map: &Mutex<HashMap<String, ManagedEntry>>,
    path: &str,
    owner: &str,
    app_id: Option<&str>,
) -> bool {
    map.lock()
        .ok()
        .and_then(|entries| entries.get(path).cloned())
        .is_some_and(|entry| {
            entry.entry.owner == owner && app_id.is_none_or(|app_id| entry.entry.app_id == app_id)
        })
}

fn close_matching(map: &Mutex<HashMap<String, ManagedEntry>>, owner: &str) -> Vec<LifecycleEntry> {
    let entries = {
        let mut entries = match map.lock() {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };
        let paths: Vec<String> = entries
            .iter()
            .filter(|(_, entry)| entry.entry.owner == owner)
            .map(|(path, _)| path.clone())
            .collect();
        paths
            .into_iter()
            .filter_map(|path| entries.remove(&path))
            .collect::<Vec<_>>()
    };
    entries
        .into_iter()
        .map(|managed| {
            (managed.cleanup)(&managed.entry);
            managed.entry
        })
        .collect()
}

fn paths_for_owner(map: &Mutex<HashMap<String, ManagedEntry>>, owner: &str) -> Vec<String> {
    map.lock()
        .map(|entries| {
            entries
                .iter()
                .filter(|(_, entry)| entry.entry.owner == owner)
                .map(|(path, _)| path.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn valid_portal_path(path: &str, component: &str) -> bool {
    path.len() <= 512
        && path.starts_with("/org/freedesktop/portal/desktop/")
        && path.contains(component)
        && zbus::zvariant::ObjectPath::try_from(path).is_ok()
}

struct RequestObject {
    lifecycle: PortalLifecycle,
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
        if self
            .lifecycle
            .close_request(&self.path, owner.as_str())
            .is_none()
        {
            return Err(zbus::fdo::Error::AccessDenied(
                "request is not owned by this portal frontend".into(),
            ));
        }
        let _ = connection
            .object_server()
            .remove::<RequestObject, _>(self.path.as_str())
            .await;
        Ok(())
    }
}

struct SessionObject {
    lifecycle: PortalLifecycle,
    path: String,
}

#[interface(name = "org.freedesktop.impl.portal.Session")]
impl SessionObject {
    #[zbus(property, name = "version")]
    fn version(&self) -> u32 {
        1
    }

    async fn close(
        &self,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(signal_emitter)] context: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        let owner = header
            .sender()
            .cloned()
            .map(OwnedUniqueName::from)
            .ok_or_else(|| zbus::fdo::Error::AccessDenied("session has no D-Bus sender".into()))?;
        if self
            .lifecycle
            .close_session(&self.path, owner.as_str())
            .is_none()
        {
            return Err(zbus::fdo::Error::AccessDenied(
                "session is not owned by this portal frontend".into(),
            ));
        }
        Self::closed(&context)
            .await
            .map_err(|error| zbus::fdo::Error::Failed(error.to_string()))?;
        let _ = connection
            .object_server()
            .remove::<SessionObject, _>(self.path.as_str())
            .await;
        Ok(())
    }

    #[zbus(signal)]
    async fn closed(context: &SignalEmitter<'_>) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(owner: &str) -> LifecycleEntry {
        LifecycleEntry {
            owner: owner.into(),
            app_id: "org.example.App".into(),
            resource: LifecycleResource::Inhibitor,
        }
    }

    #[test]
    fn portal_resources_are_owner_scoped_and_released_on_disconnect() {
        // Phase `owners_cannot_close_each_others_resources`.
        {
            let lifecycle = PortalLifecycle::new();
            assert!(lifecycle.insert_request("/request/one", entry(":1.1")));
            assert!(lifecycle.close_request("/request/one", ":1.2").is_none());
            assert!(lifecycle.close_request("/request/one", ":1.1").is_some());
        }
        // Phase `disconnect_releases_both_kinds`.
        {
            let lifecycle = PortalLifecycle::new();
            assert!(lifecycle.insert_request("/request/one", entry(":1.1")));
            assert!(lifecycle.insert_session("/session/one", entry(":1.1")));
            assert_eq!(lifecycle.close_owner(":1.1").len(), 2);
        }
        // Phase `only_portal_object_space_is_accepted`.
        assert!(valid_portal_path(
            "/org/freedesktop/portal/desktop/request/tilo/one",
            "/request/"
        ));
        assert!(!valid_portal_path(
            "/org/tilo/private/request/one",
            "/request/"
        ));
    }

    #[test]
    fn cleanup_runs_exactly_once_and_only_for_cancellation() {
        // Phase `cleanup_runs_exactly_once_after_close`.
        {
            let lifecycle = PortalLifecycle::new();
            let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let cleanup_count = count.clone();
            assert!(lifecycle.insert_request_with_cleanup(
                "/request/one",
                entry(":1.1"),
                move |_| {
                    cleanup_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            ));
            assert!(lifecycle.close_request("/request/one", ":1.1").is_some());
            assert!(lifecycle.close_request("/request/one", ":1.1").is_none());
            assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
        // Phase `successful_completion_does_not_run_cancellation_cleanup`.
        {
            let lifecycle = PortalLifecycle::new();
            let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let cleanup_count = count.clone();
            assert!(lifecycle.insert_request_with_cleanup(
                "/request/complete",
                entry(":1.1"),
                move |_| {
                    cleanup_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            ));
            assert!(take_without_cleanup(&lifecycle.requests, "/request/complete").is_some());
            assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    }
}
