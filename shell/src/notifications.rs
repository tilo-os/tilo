//! The compositor-owned `org.freedesktop.Notifications` D-Bus service.
//!
//! In qOS the desktop *chrome* is owned by the compositor, and
//! notifications are chrome too: this module makes the compositor own
//! `org.freedesktop.Notifications` so `libnotify`/freedesktop-API calls
//! render as OS-drawn, OS-themed toasts (see
//! [`crate::shell::notification::NotificationStack`]) instead of app-drawn windows.
//!
//! The zbus service runs async on a tokio runtime; the toast queue lives
//! on [`crate::shell::chrome_state::ChromeState`], an `Rc<RefCell<…>>`
//! pinned to the compositor's main/winit thread (**not** `Send`). The
//! two communicate through [`NotificationBridge`], a `Send + Sync`
//! `Arc<Mutex<…>>` mailbox — the same shape
//! [`crate::portal::PromptQueueHandle`] uses to hand prompts from the async portal to the synchronous chrome pump.
//!
//! Each `Notify` call is attributed to the sending app's workspace via
//! the same [`crate::portal::CallerMap`] the portal backend uses, riding
//! along on [`IncomingNotification::caller`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use zbus::interface;
use zbus::names::OwnedUniqueName;
use zbus::object_server::SignalContext;
use zbus::zvariant::OwnedValue;

use crate::portal::{CallerMap, PortalCaller};
use crate::priority_contacts::{ContactProjection, PriorityContactRegistry};

/// Bus name the freedesktop notification spec reserves for the
/// notification daemon; owning it makes the compositor the system notification server.
pub const BUS_NAME: &str = "org.freedesktop.Notifications";

/// Object path the spec hard-codes for the notification interface.
pub const OBJECT_PATH: &str = "/org/freedesktop/Notifications";

/// Reason a notification was closed, per the freedesktop spec's `NotificationClosed` signal `reason` field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// 1 — the notification expired (its timeout elapsed).
    Expired,
    /// 2 — dismissed by the user (clicked / X-closed the toast).
    Dismissed,
    /// 3 — closed by a `CloseNotification` call from the app.
    Closed,
}

impl CloseReason {
    /// The on-the-wire `reason` code for the `NotificationClosed` signal.
    pub fn code(self) -> u32 {
        match self {
            CloseReason::Expired => 1,
            CloseReason::Dismissed => 2,
            CloseReason::Closed => 3,
        }
    }
}

/// One notification that crossed the bridge from a `Notify` call, before
/// it is mapped into a [`crate::shell::notification::Notification`] toast by the chrome pump.
#[derive(Clone, Debug)]
pub struct IncomingNotification {
    /// The notification id — either freshly allocated (`replaces_id == 0`) or the app-supplied id it is replacing.
    pub dbus_id: u32,
    /// Sending application's human-readable name (the `app_name` arg).
    pub app_name: String,
    /// Single-line summary (becomes the toast title).
    pub summary: String,
    /// Optional multi-line body (becomes the toast body).
    pub body: String,
    /// `(action_key, action_label)` pairs, un-flattened from the spec's `[key1, label1, key2, label2, …]` array.
    pub actions: Vec<(String, String)>,
    /// Raw spec `expire_timeout`: `-1` = server default, `0` = never expire, `> 0` = milliseconds.
    pub expire_timeout: i32,
    /// `urgency` hint: `0` low, `1` normal, `2` critical.
    pub urgency: u8,
    /// Workspace/app the call was attributed to (`None` if the sender couldn't be mapped — e.g. an unsandboxed caller).
    pub caller: Option<PortalCaller>,
    /// qOS extension: exact unread count. It is ignored unless `caller` was
    /// launcher-verified and policy permits badges for that package.
    pub badge_count: Option<u32>,
    /// qOS extension: sender endpoint asserted by an explicitly authorized
    /// communications app. The shell hashes and checks it against its private
    /// Contacts-owned registry before it can affect focus policy.
    pub contact_endpoint: Option<String>,
}

#[derive(Default, Debug)]
struct BridgeInner {
    /// `Notify` → compositor toast pump.
    incoming: Vec<IncomingNotification>,
    /// `CloseNotification(id)` → compositor toast pump.
    close_requests: Vec<u32>,
    /// Toast removed in the compositor → `NotificationClosed` signal.
    closed_events: Vec<(u32, CloseReason)>,
    /// Toast action activated in the compositor → `ActionInvoked` signal.
    action_events: Vec<(u32, String)>,
}

/// Thread-safe mailbox between the async D-Bus service and the
/// synchronous chrome pump; cloneable (`Arc` bump) so each side holds its own handle.
#[derive(Clone, Debug)]
pub struct NotificationBridge {
    inner: Arc<Mutex<BridgeInner>>,
    /// Monotonic id source. The spec requires ids be non-zero and unique for
    /// the server's lifetime; `0` is reserved to mean "allocate a fresh id" in `Notify`.
    next_id: Arc<AtomicU32>,
}

impl Default for NotificationBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationBridge {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(BridgeInner::default())),
            next_id: Arc::new(AtomicU32::new(1)),
        }
    }

    /// Allocate a fresh, non-zero notification id.
    pub fn alloc_id(&self) -> u32 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            // Wrapped around (after 4 billion notifications) — skip 0, which the spec reserves.
            self.next_id.fetch_add(1, Ordering::Relaxed)
        } else {
            id
        }
    }

    /// app → compositor: enqueue a new notification.
    pub fn push_incoming(&self, n: IncomingNotification) {
        if let Ok(mut g) = self.inner.lock() {
            g.incoming.push(n);
        }
    }

    /// app → compositor: request a notification be closed.
    pub fn request_close(&self, id: u32) {
        if let Ok(mut g) = self.inner.lock() {
            g.close_requests.push(id);
        }
    }

    /// compositor pump: take every queued incoming notification.
    pub fn drain_incoming(&self) -> Vec<IncomingNotification> {
        self.inner
            .lock()
            .map(|mut g| std::mem::take(&mut g.incoming))
            .unwrap_or_default()
    }

    /// compositor pump: take every queued close request.
    pub fn drain_close_requests(&self) -> Vec<u32> {
        self.inner
            .lock()
            .map(|mut g| std::mem::take(&mut g.close_requests))
            .unwrap_or_default()
    }

    /// compositor → app: record that a toast was closed, to be emitted as `NotificationClosed`.
    pub fn note_closed(&self, id: u32, reason: CloseReason) {
        if let Ok(mut g) = self.inner.lock() {
            g.closed_events.push((id, reason));
        }
    }

    /// compositor → app: record that a toast action was activated, to be emitted as `ActionInvoked`.
    pub fn note_action(&self, id: u32, action_key: impl Into<String>) {
        if let Ok(mut g) = self.inner.lock() {
            g.action_events.push((id, action_key.into()));
        }
    }

    /// signal emitter: take every queued closed event.
    pub fn drain_closed_events(&self) -> Vec<(u32, CloseReason)> {
        self.inner
            .lock()
            .map(|mut g| std::mem::take(&mut g.closed_events))
            .unwrap_or_default()
    }

    /// signal emitter: take every queued action event.
    pub fn drain_action_events(&self) -> Vec<(u32, String)> {
        self.inner
            .lock()
            .map(|mut g| std::mem::take(&mut g.action_events))
            .unwrap_or_default()
    }

    /// Test/observability: number of incoming notifications buffered but not yet drained by the chrome pump.
    pub fn pending_incoming_len(&self) -> usize {
        self.inner.lock().map_or(0, |g| g.incoming.len())
    }
}

/// Parse the `urgency` hint (a `y`/byte); defaults to `1` (normal) when absent or the wrong type — never refuse a notification over a bad hint.
fn parse_urgency(hints: &HashMap<String, OwnedValue>) -> u8 {
    hints
        .get("urgency")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(1)
}

fn parse_badge_count(hints: &HashMap<String, OwnedValue>) -> Option<u32> {
    hints
        .get("x-qos-badge-count")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| u32::try_from(value).ok())
        .map(|value| value.min(9_999))
}

fn parse_contact_endpoint(hints: &HashMap<String, OwnedValue>) -> Option<String> {
    let value = hints
        .get("x-qos-contact-handle")?
        .try_clone()
        .ok()
        .and_then(|value| String::try_from(value).ok())?;
    (value.len() <= 320 && !value.chars().any(char::is_control)).then_some(value)
}

/// Un-flatten the spec's `[key1, label1, key2, label2, …]` actions array into `(key, label)` pairs, dropping a dangling trailing key.
fn pair_actions(actions: &[String]) -> Vec<(String, String)> {
    actions
        .chunks_exact(2)
        .filter_map(|c| {
            let [key, label] = c else {
                return None; // chunks_exact(2) guarantees len == 2
            };
            Some((key.clone(), label.clone()))
        })
        .collect()
}

/// The `org.freedesktop.Notifications` D-Bus interface. One instance per
/// [`serve_notifications`] call, held by the zbus `ObjectServer` for the connection's life.
pub struct NotificationService {
    bridge: NotificationBridge,
    callers: CallerMap,
    connection: Option<zbus::Connection>,
    priority_contacts: PriorityContactRegistry,
}

impl NotificationService {
    pub fn new(bridge: NotificationBridge, callers: CallerMap) -> Self {
        Self {
            bridge,
            callers,
            connection: None,
            priority_contacts: PriorityContactRegistry::in_memory(),
        }
    }

    pub fn with_connection(
        bridge: NotificationBridge,
        callers: CallerMap,
        connection: zbus::Connection,
        priority_contacts: PriorityContactRegistry,
    ) -> Self {
        Self {
            bridge,
            callers,
            connection: Some(connection),
            priority_contacts,
        }
    }

    /// Resolve the D-Bus sender to a workspace/app caller via the shared
    /// [`CallerMap`]. `None` when no mapping exists — the notification still shows, just without workspace attribution.
    async fn resolve_caller(&self, sender: Option<&OwnedUniqueName>) -> Option<PortalCaller> {
        let key = sender.map(|n| n.as_str().to_string()).unwrap_or_default();
        if let Some(caller) = self.callers.lookup(&key) {
            return Some(caller);
        }
        let connection = self.connection.as_ref()?;
        let sender = sender?;
        let bus = zbus::fdo::DBusProxy::new(connection).await.ok()?;
        let name = zbus::names::BusName::try_from(sender.as_str()).ok()?;
        let pid = bus.get_connection_unix_process_id(name).await.ok()?;
        let package = crate::shell::launcher::verified_package_for_descendant(pid)?;
        Some(PortalCaller::new(
            crate::shell::state::WorkspaceId::default(),
            package,
        ))
    }
}

#[interface(name = "org.freedesktop.Notifications")]
impl NotificationService {
    /// `Notify` — the core method. Queues a toast and returns its id.
    ///
    /// Args (freedesktop spec): `app_name s`, `replaces_id u`, `app_icon s`,
    /// `summary s`, `body s`, `actions as`, `hints a{sv}`, `expire_timeout i`. Returns the assigned id `u`.
    #[allow(clippy::too_many_arguments)]
    #[allow(
        clippy::used_underscore_binding,
        reason = "the D-Bus `Notify` signature requires app_icon; the zbus `#[interface]` \
                  macro deserialises it into this named parameter and forwards it into the \
                  generated dispatcher even though this handler has no use for the value"
    )]
    async fn notify(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        app_name: String,
        replaces_id: u32,
        _app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let id = if replaces_id != 0 {
            replaces_id
        } else {
            self.bridge.alloc_id()
        };
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = self.resolve_caller(sender.as_ref()).await;
        tracing::debug!(
            "Notifications.Notify: app={app_name:?} id={id} caller_ws={:?}",
            caller.as_ref().map(|c| c.workspace_id)
        );
        self.bridge.push_incoming(IncomingNotification {
            dbus_id: id,
            app_name,
            summary,
            body,
            actions: pair_actions(&actions),
            expire_timeout,
            urgency: parse_urgency(&hints),
            caller,
            badge_count: parse_badge_count(&hints),
            contact_endpoint: parse_contact_endpoint(&hints),
        });
        id
    }

    /// qOS extension used by Contacts. The complete favourite projection is
    /// replaced atomically; only a launcher-verified Contacts process can call
    /// it. Other apps cannot mutate the host registry even on the same UID.
    async fn x_qos_replace_priority_contacts(
        &self,
        #[zbus(header)] hdr: zbus::message::Header<'_>,
        contacts: Vec<(String, String, Vec<String>)>,
    ) -> zbus::fdo::Result<u32> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = self.resolve_caller(sender.as_ref()).await;
        if caller.as_ref().map(|caller| caller.app_id.as_str()) != Some("dev.qos.contacts") {
            return Err(zbus::fdo::Error::AccessDenied(
                "only the verified Contacts provider may replace priority contacts".into(),
            ));
        }
        let contacts = contacts
            .into_iter()
            .map(|(id, name, endpoints)| ContactProjection {
                id,
                name,
                endpoints,
            })
            .collect();
        self.priority_contacts
            .replace_verified(contacts)
            .map(|count| u32::try_from(count).unwrap_or(u32::MAX))
            .map_err(zbus::fdo::Error::Failed)
    }

    /// `CloseNotification` — ask the server to close a notification by id.
    /// The compositor pump removes the toast and the emitter fires `NotificationClosed(id, 3)`.
    fn close_notification(&self, id: u32) {
        self.bridge.request_close(id);
    }

    /// `GetCapabilities` — advertise the optional features we honour:
    /// `body` (multi-line text), `actions` (default-action activate), `persistence` (critical toasts stay until closed).
    #[allow(
        clippy::unused_self,
        reason = "zbus's #[interface] macro dispatches D-Bus methods as instance methods, even though this fixed capability list doesn't depend on self"
    )]
    fn get_capabilities(&self) -> Vec<String> {
        vec![
            "body".to_string(),
            "actions".to_string(),
            "persistence".to_string(),
        ]
    }

    /// `GetServerInformation` — `(name, vendor, version, spec_version)`.
    #[allow(
        clippy::unused_self,
        reason = "zbus's #[interface] macro dispatches D-Bus methods as instance methods, even though this fixed server info doesn't depend on self"
    )]
    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "qOS Notifications".to_string(),
            "qOS".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
            "1.2".to_string(),
        )
    }

    /// `NotificationClosed(id, reason)` — emitted when a toast goes away.
    #[zbus(signal)]
    async fn notification_closed(
        ctxt: &SignalContext<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    /// `ActionInvoked(id, action_key)` — emitted when the user activates an action on a toast.
    #[zbus(signal)]
    async fn action_invoked(
        ctxt: &SignalContext<'_>,
        id: u32,
        action_key: String,
    ) -> zbus::Result<()>;
}

/// RAII handle keeping the notification D-Bus connection alive; dropping it releases the bus name and tears down the object server.
pub struct ServerHandle {
    connection: zbus::Connection,
}

impl ServerHandle {
    pub fn connection(&self) -> &zbus::Connection {
        &self.connection
    }
}

/// Register [`NotificationService`] at [`OBJECT_PATH`] on `connection`
/// and request [`BUS_NAME`].
///
/// **Production** passes `zbus::Connection::session()`. **Tests** pass an
/// in-process p2p connection; `request_name` is best-effort there since p2p connections have no broker.
///
/// # Errors
///
/// Returns an error if registering [`NotificationService`] at
/// [`OBJECT_PATH`] fails. A failed [`BUS_NAME`] request is logged, not
/// propagated.
pub async fn serve_notifications(
    connection: zbus::Connection,
    service: NotificationService,
) -> Result<ServerHandle, zbus::Error> {
    connection.object_server().at(OBJECT_PATH, service).await?;
    if let Err(e) = connection.request_name(BUS_NAME).await {
        tracing::debug!("notifications: request_name skipped/failed: {e}");
    }
    Ok(ServerHandle { connection })
}

/// Spawn the reverse-direction signal pump: a tokio task draining the
/// bridge's closed/action queues into D-Bus signals; must run inside a tokio runtime, lives until the connection drops.
pub fn spawn_signal_emitter(
    connection: zbus::Connection,
    bridge: NotificationBridge,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let ctxt = match SignalContext::new(&connection, OBJECT_PATH) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("notifications: signal context build failed: {e}");
                return;
            }
        };
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(120));
        loop {
            interval.tick().await;
            for (id, reason) in bridge.drain_closed_events() {
                let _ = NotificationService::notification_closed(&ctxt, id, reason.code()).await;
            }
            for (id, key) in bridge.drain_action_events() {
                let _ = NotificationService::action_invoked(&ctxt, id, key).await;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceId;

    #[test]
    fn alloc_id_is_monotonic_and_never_zero() {
        let b = NotificationBridge::new();
        let a = b.alloc_id();
        let c = b.alloc_id();
        assert_ne!(a, 0);
        assert_ne!(c, 0);
        assert!(c > a, "ids must be monotonic: {a} then {c}");
    }

    #[test]
    fn incoming_round_trips_through_bridge() {
        let b = NotificationBridge::new();
        assert_eq!(b.pending_incoming_len(), 0);
        b.push_incoming(IncomingNotification {
            dbus_id: 1,
            app_name: "App".into(),
            summary: "Hi".into(),
            body: "There".into(),
            actions: vec![],
            expire_timeout: -1,
            urgency: 1,
            caller: None,
            badge_count: None,
            contact_endpoint: None,
        });
        assert_eq!(b.pending_incoming_len(), 1);
        let drained = b.drain_incoming();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].summary, "Hi");
        // Drain is destructive.
        assert_eq!(b.pending_incoming_len(), 0);
        assert!(b.drain_incoming().is_empty());
    }

    #[test]
    fn close_request_round_trips() {
        let b = NotificationBridge::new();
        b.request_close(42);
        assert_eq!(b.drain_close_requests(), vec![42]);
        assert!(b.drain_close_requests().is_empty());
    }

    #[test]
    fn closed_and_action_events_round_trip() {
        let b = NotificationBridge::new();
        b.note_closed(7, CloseReason::Expired);
        b.note_action(7, "default");
        assert_eq!(b.drain_closed_events(), vec![(7, CloseReason::Expired)]);
        assert_eq!(b.drain_action_events(), vec![(7, "default".to_string())]);
    }

    #[test]
    fn close_reason_codes_match_spec() {
        assert_eq!(CloseReason::Expired.code(), 1);
        assert_eq!(CloseReason::Dismissed.code(), 2);
        assert_eq!(CloseReason::Closed.code(), 3);
    }

    #[test]
    fn pair_actions_unflattens_and_drops_dangling_key() {
        let actions = vec![
            "default".to_string(),
            "Open".to_string(),
            "snooze".to_string(),
            "Snooze".to_string(),
            "dangling".to_string(),
        ];
        let pairs = pair_actions(&actions);
        assert_eq!(
            pairs,
            vec![
                ("default".to_string(), "Open".to_string()),
                ("snooze".to_string(), "Snooze".to_string()),
            ]
        );
    }

    #[test]
    fn parse_urgency_defaults_to_normal() {
        let hints: HashMap<String, OwnedValue> = HashMap::new();
        assert_eq!(parse_urgency(&hints), 1);
    }

    #[test]
    fn parse_urgency_reads_critical_byte() {
        let mut hints: HashMap<String, OwnedValue> = HashMap::new();
        hints.insert("urgency".to_string(), OwnedValue::from(2u8));
        assert_eq!(parse_urgency(&hints), 2);
    }

    #[test]
    fn qos_hints_are_bounded_before_they_reach_chrome() {
        let mut hints = HashMap::new();
        hints.insert("x-qos-badge-count".into(), OwnedValue::from(u32::MAX));
        hints.insert(
            "x-qos-contact-handle".into(),
            OwnedValue::from(zbus::zvariant::Str::from("ada@example.test")),
        );
        assert_eq!(parse_badge_count(&hints), Some(9_999));
        assert_eq!(
            parse_contact_endpoint(&hints).as_deref(),
            Some("ada@example.test")
        );
        hints.insert(
            "x-qos-contact-handle".into(),
            OwnedValue::from(zbus::zvariant::Str::from("bad\nhandle")),
        );
        assert_eq!(parse_contact_endpoint(&hints), None);
    }

    // D-Bus round-trip over an in-process p2p connection, mirroring
    // `tests/portal_test.rs`'s hermetic fixture: a `UnixStream::pair()`
    // carries a server and a raw-proxy client — no real session bus or broker needed.

    const TEST_GUID: &str = "0123456789abcdef0123456789abcdef";

    async fn bring_up_pair(
        bridge: NotificationBridge,
        callers: CallerMap,
    ) -> (zbus::Connection, zbus::Connection) {
        use tokio::net::UnixStream;
        use zbus::connection::Builder;

        let (server_sock, client_sock) = UnixStream::pair().expect("socket pair");
        let service = NotificationService::new(bridge, callers);
        let server_builder = Builder::unix_stream(server_sock)
            .server(TEST_GUID)
            .expect("guid")
            .p2p()
            .serve_at(OBJECT_PATH, service)
            .expect("serve_at");
        let client_builder = Builder::unix_stream(client_sock).p2p();
        let (server_conn, client_conn) =
            futures_util::try_join!(server_builder.build(), client_builder.build())
                .expect("p2p connection pair");
        (server_conn, client_conn)
    }

    async fn notifications_proxy(client: &zbus::Connection) -> zbus::Proxy<'_> {
        zbus::proxy::Builder::<zbus::Proxy<'_>>::new(client)
            .destination(BUS_NAME)
            .expect("destination")
            .path(OBJECT_PATH)
            .expect("path")
            .interface("org.freedesktop.Notifications")
            .expect("interface")
            .build()
            .await
            .expect("proxy build")
    }

    #[tokio::test]
    async fn notify_round_trips_with_caller_attribution() {
        let bridge = NotificationBridge::new();
        let callers = CallerMap::new();
        // The p2p transport doesn't surface a unique-name, so force the caller the same way the portal tests do — the production attribution shape.
        callers.force_caller_for_tests(PortalCaller::new(WorkspaceId(7), "org.example.app"));

        let (_server, client) = bring_up_pair(bridge.clone(), callers).await;
        let proxy = notifications_proxy(&client).await;

        let app_name = "Example";
        let replaces_id: u32 = 0;
        let app_icon = "";
        let summary = "Build finished";
        let body = "All 524 tests passed";
        let actions: Vec<String> = vec!["default".into(), "Open".into()];
        let mut hints: HashMap<String, OwnedValue> = HashMap::new();
        hints.insert("urgency".into(), OwnedValue::from(2u8));
        let expire_timeout: i32 = -1;

        let id: u32 = proxy
            .call_method(
                "Notify",
                &(
                    app_name,
                    replaces_id,
                    app_icon,
                    summary,
                    body,
                    &actions,
                    &hints,
                    expire_timeout,
                ),
            )
            .await
            .expect("call Notify")
            .body()
            .deserialize()
            .expect("deserialize id");

        assert_eq!(id, 1, "first allocated id must be 1");

        let drained = bridge.drain_incoming();
        assert_eq!(drained.len(), 1, "Notify must enqueue exactly one toast");
        let inc = &drained[0];
        assert_eq!(inc.dbus_id, 1);
        assert_eq!(inc.summary, "Build finished");
        assert_eq!(inc.urgency, 2, "critical urgency hint must round-trip");
        assert_eq!(
            inc.actions,
            vec![("default".to_string(), "Open".to_string())]
        );
        let caller = inc.caller.as_ref().expect("caller attributed");
        assert_eq!(caller.workspace_id, WorkspaceId(7));
        assert_eq!(caller.app_id.as_str(), "org.example.app");
    }

    #[tokio::test]
    async fn replaces_id_is_preserved() {
        let bridge = NotificationBridge::new();
        let (_server, client) = bring_up_pair(bridge.clone(), CallerMap::new()).await;
        let proxy = notifications_proxy(&client).await;

        let hints: HashMap<String, OwnedValue> = HashMap::new();
        let actions: Vec<String> = vec![];
        let id: u32 = proxy
            .call_method(
                "Notify",
                &("App", 99u32, "", "Sum", "Body", &actions, &hints, 0i32),
            )
            .await
            .expect("call Notify")
            .body()
            .deserialize()
            .expect("deserialize id");
        assert_eq!(id, 99, "a non-zero replaces_id must be returned verbatim");
        let inc = bridge.drain_incoming();
        assert_eq!(inc[0].dbus_id, 99);
        assert_eq!(inc[0].expire_timeout, 0, "never-expire timeout preserved");
    }

    #[tokio::test]
    async fn close_notification_enqueues_close_request() {
        let bridge = NotificationBridge::new();
        let (_server, client) = bring_up_pair(bridge.clone(), CallerMap::new()).await;
        let proxy = notifications_proxy(&client).await;

        proxy
            .call_method("CloseNotification", &(5u32))
            .await
            .expect("call CloseNotification");
        assert_eq!(bridge.drain_close_requests(), vec![5]);
    }

    #[tokio::test]
    async fn get_capabilities_and_server_information() {
        let bridge = NotificationBridge::new();
        let (_server, client) = bring_up_pair(bridge, CallerMap::new()).await;
        let proxy = notifications_proxy(&client).await;

        let caps: Vec<String> = proxy
            .call_method("GetCapabilities", &())
            .await
            .expect("call GetCapabilities")
            .body()
            .deserialize()
            .expect("deserialize caps");
        assert!(caps.contains(&"body".to_string()));
        assert!(caps.contains(&"actions".to_string()));

        let (name, vendor, _ver, spec): (String, String, String, String) = proxy
            .call_method("GetServerInformation", &())
            .await
            .expect("call GetServerInformation")
            .body()
            .deserialize()
            .expect("deserialize server info");
        assert_eq!(name, "qOS Notifications");
        assert_eq!(vendor, "qOS");
        assert_eq!(spec, "1.2");
    }
}
