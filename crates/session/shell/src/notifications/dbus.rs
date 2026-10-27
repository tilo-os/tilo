//! The D-Bus face of the notification service: the
//! `org.freedesktop.Notifications` interface — its methods, its
//! introspection, its signals — the connection that owns the bus name, and
//! the pump that turns the bridge's queues back into signals.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::names::OwnedUniqueName;
use zbus::object_server::{DispatchResult2, Interface, SignalEmitter};
use zbus::{Connection, fdo, message::Header, message::Message};

#[cfg(any(test, target_os = "linux"))]
use zbus::zvariant::OwnedValue;

use crate::portal::CallerMap;
use crate::portal::PortalCaller;
use crate::portal::interface_dispatch;
use crate::priority_contacts::ContactProjection;
use crate::priority_contacts::PriorityContactRegistry;

use super::flatpak::verified_foreign_notification_identity;
use super::hints::{
    pair_actions, parse_badge_count, parse_contact_endpoint, parse_icon_hint, parse_image_data,
    parse_incoming_call, parse_notification_sharing, parse_notification_sound, parse_sender_name,
    parse_urgency,
};
use super::scheduled;
use super::{BUS_NAME, IncomingNotification, NotificationBridge, OBJECT_PATH};

/// The `org.freedesktop.Notifications` D-Bus interface. One instance per
/// [`serve_notifications`] call, held by the zbus `ObjectServer` for the connection's life.
#[cfg(target_os = "linux")]
pub struct NotificationService {
    bridge: NotificationBridge,
    callers: CallerMap,
    connection: Option<zbus::Connection>,
    priority_contacts: PriorityContactRegistry,
    schedules: Option<Arc<Mutex<scheduled::ScheduleStore>>>,
    schedule_changed: crate::idle_wake::IdleWake,
    schedule_worker: Option<tokio::task::JoinHandle<()>>,
    schedule_workspaces: Option<crate::portal::GrantStoreHandle>,
}

#[cfg(target_os = "linux")]
impl NotificationService {
    pub fn new(bridge: NotificationBridge, callers: CallerMap) -> Self {
        Self {
            bridge,
            callers,
            connection: None,
            priority_contacts: PriorityContactRegistry::in_memory(),
            schedules: scheduled::ScheduleStore::open(None)
                .ok()
                .map(|s| Arc::new(Mutex::new(s))),
            schedule_changed: crate::idle_wake::IdleWake::default(),
            schedule_worker: None,
            schedule_workspaces: None,
        }
    }

    pub fn with_connection(
        bridge: NotificationBridge,
        callers: CallerMap,
        connection: zbus::Connection,
        priority_contacts: PriorityContactRegistry,
        schedule_workspaces: Option<crate::portal::GrantStoreHandle>,
    ) -> Self {
        let path = tilo_paths::xdg::state_home()
            .map(|root| root.join("tilo/notifications/schedules.json"));
        let schedules = path.and_then(|path| {
            scheduled::ScheduleStore::open(Some(path))
                .inspect_err(|error| tracing::warn!(%error, "notification schedules unavailable"))
                .ok()
                .map(|store| Arc::new(Mutex::new(store)))
        });
        let schedule_changed = crate::idle_wake::IdleWake::default();
        let schedule_worker = schedules.as_ref().map(|store| {
            bridge.attach_schedule_controller(store, schedule_changed.clone());
            tokio::spawn(scheduled::drive(
                Arc::downgrade(store),
                bridge.clone(),
                schedule_changed.clone(),
                schedule_workspaces.clone(),
            ))
        });
        Self {
            bridge,
            callers,
            connection: Some(connection),
            priority_contacts,
            schedules,
            schedule_changed,
            schedule_worker,
            schedule_workspaces,
        }
    }

    async fn schedule_caller(&self, hdr: &Header<'_>) -> fdo::Result<scheduled::Owner> {
        let denied = || {
            fdo::Error::AccessDenied(
                "scheduled notifications require a verified application workspace".into(),
            )
        };
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let key = sender.as_ref().map(ToString::to_string).unwrap_or_default();
        if let Some(caller) = self.callers.lookup(&key) {
            if let Some(workspaces) = &self.schedule_workspaces {
                let tree = workspaces.tree_handle();
                let room = tree
                    .read()
                    .desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id == caller.workspace_id)
                    .map(|workspace| workspace.room().to_string())
                    .ok_or_else(denied)?;
                return Ok(scheduled::Owner { caller, room });
            }
            return Ok(scheduled::Owner::mapped(caller));
        }
        let connection = self.connection.as_ref().ok_or_else(denied)?;
        let sender = sender.as_ref().ok_or_else(denied)?;
        let bus = zbus::fdo::DBusProxy::new(connection)
            .await
            .map_err(|_| denied())?;
        let name = zbus::names::BusName::try_from(sender.as_str()).map_err(|_| denied())?;
        let pid = bus
            .get_connection_unix_process_id(name)
            .await
            .map_err(|_| denied())?;
        let launch =
            crate::shell::launcher::verified_launch_for_descendant(pid).ok_or_else(denied)?;
        let room = launch
            .workspace_id
            .filter(|room| !room.is_empty())
            .ok_or_else(denied)?;
        let caller = self
            .schedule_workspaces
            .as_ref()
            .and_then(|workspaces| workspaces.caller_in_room(&room, &launch.package_id))
            .ok_or_else(denied)?;
        Ok(scheduled::Owner { caller, room })
    }

    fn with_schedules<T>(
        &self,
        f: impl FnOnce(&mut scheduled::ScheduleStore) -> Result<T, String>,
    ) -> fdo::Result<T> {
        let store = self.schedules.as_ref().ok_or_else(|| {
            fdo::Error::Failed("durable notification scheduler unavailable".into())
        })?;
        let mut store = store
            .lock()
            .map_err(|_| fdo::Error::Failed("notification scheduler lock failed".into()))?;
        f(&mut store).map_err(fdo::Error::Failed)
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

    /// Resolve the identity used solely for ordinary notification policy.
    /// The bool reports a foreign Flatpak fallback so wire-only Tilo
    /// extensions (badges/contact handles) remain unavailable to it.
    async fn resolve_notification_caller(
        &self,
        sender: Option<&OwnedUniqueName>,
    ) -> (Option<PortalCaller>, bool) {
        // A palette-launched Flatpak can also appear in the generic launch
        // registry under its desktop-file id. Its sandbox runtime ID is more
        // canonical (and is what Settings exposes), so prefer it before that
        // fallback. A Tilo package is not a Flatpak and proceeds unchanged.
        if let (Some(connection), Some(sender)) = (self.connection.as_ref(), sender)
            && let Ok(bus) = zbus::fdo::DBusProxy::new(connection).await
            && let Ok(name) = zbus::names::BusName::try_from(sender.as_str())
            && let Ok(pid) = bus.get_connection_unix_process_id(name).await
            && let Some(app_id) = verified_foreign_notification_identity(pid)
        {
            // Foreign callers have no Tilo workspace capability. This default
            // workspace is an attribution placeholder only; policy keys come
            // from the verified id, never this workspace or Notify's name.
            return (
                Some(PortalCaller::new(
                    crate::shell::state::WorkspaceId::default(),
                    app_id,
                )),
                true,
            );
        }
        (self.resolve_caller(sender).await, false)
    }
}

#[cfg(target_os = "linux")]
impl Drop for NotificationService {
    fn drop(&mut self) {
        if let Some(worker) = &self.schedule_worker {
            worker.abort();
        }
    }
}

/// One `Notify` call's spec arguments.
#[cfg(target_os = "linux")]
struct NotifyRequest {
    app_name: String,
    replaces_id: u32,
    app_icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: HashMap<String, OwnedValue>,
    expire_timeout: i32,
}

/// `Notify`'s wire arguments (freedesktop spec): `app_name s`,
/// `replaces_id u`, `app_icon s`, `summary s`, `body s`, `actions as`,
/// `hints a{sv}`, `expire_timeout i`.
#[cfg(target_os = "linux")]
pub(super) type NotifyArgs = (
    String,
    u32,
    String,
    String,
    String,
    Vec<String>,
    HashMap<String, OwnedValue>,
    i32,
);

#[cfg(target_os = "linux")]
impl From<NotifyArgs> for NotifyRequest {
    fn from(
        (app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout): NotifyArgs,
    ) -> Self {
        Self {
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            hints,
            expire_timeout,
        }
    }
}

/// The interface's introspection, byte-identical to what zbus's
/// `#[interface]` macro wrote for the former macro-dispatched service.
#[cfg(target_os = "linux")]
pub(super) const NOTIFICATIONS_INTROSPECTION: &str = r#"<interface name="org.freedesktop.Notifications">
  <!--
   `Notify` — the core method. Queues a toast and returns its id.

   Args (freedesktop spec): `app_name s`, `replaces_id u`, `app_icon s`,
   `summary s`, `body s`, `actions as`, `hints a{sv}`, `expire_timeout i`. Returns the assigned id `u`.
   -->
  <method name="Notify">
    <arg name="app_name" type="s" direction="in"/>
    <arg name="replaces_id" type="u" direction="in"/>
    <arg name="_app_icon" type="s" direction="in"/>
    <arg name="summary" type="s" direction="in"/>
    <arg name="body" type="s" direction="in"/>
    <arg name="actions" type="as" direction="in"/>
    <arg name="hints" type="a{sv}" direction="in"/>
    <arg name="expire_timeout" type="i" direction="in"/>
    <arg type="u" direction="out"/>
  </method>
  <!--
   Tilo extension used by Contacts. The complete favourite projection is
   replaced atomically; only a launcher-verified Contacts process can call
   it. Other apps cannot mutate the host registry even on the same UID.
   -->
  <method name="XTiloReplacePriorityContacts">
    <arg name="contacts" type="a(ssas)" direction="in"/>
    <arg type="u" direction="out"/>
  </method>
  <!--
   `CloseNotification` — ask the server to close a notification by id.
   The compositor pump removes the toast and the emitter fires `NotificationClosed(id, 3)`.
   -->
  <method name="XTiloScheduleNotification">
    <arg name="tag" type="s" direction="in"/>
    <arg name="title" type="s" direction="in"/>
    <arg name="body" type="s" direction="in"/>
    <arg name="activation" type="s" direction="in"/>
    <arg name="at_ms" type="x" direction="in"/>
    <arg name="calendar" type="a(yyyxs)" direction="in"/>
  </method>
  <method name="XTiloScheduledNotifications">
    <arg type="a(sx)" direction="out"/>
  </method>
  <method name="XTiloScheduledAlarms">
    <arg type="a(s(yyyx)axaxs)" direction="out"/>
  </method>
  <method name="XTiloCancelScheduledNotification">
    <arg name="tag" type="s" direction="in"/>
  </method>
  <method name="CloseNotification">
    <arg name="id" type="u" direction="in"/>
  </method>
  <!--
   `GetCapabilities` — advertise the optional features we honour:
   `body` (multi-line text), `actions` (default-action activate), `persistence` (critical toasts stay until closed).
   -->
  <method name="GetCapabilities">
    <arg type="as" direction="out"/>
  </method>
  <!--
   `GetServerInformation` — `(name, vendor, version, spec_version)`.
   -->
  <method name="GetServerInformation">
    <arg type="s" direction="out"/>
    <arg type="s" direction="out"/>
    <arg type="s" direction="out"/>
    <arg type="s" direction="out"/>
  </method>
  <!--
   `NotificationClosed(id, reason)` — emitted when a toast goes away.
   -->
  <signal name="NotificationClosed">
    <arg name="id" type="u"/>
    <arg name="reason" type="u"/>
  </signal>
  <!--
   `ActionInvoked(id, action_key)` — emitted when the user activates an action on a toast.
   -->
  <signal name="ActionInvoked">
    <arg name="id" type="u"/>
    <arg name="action_key" type="s"/>
  </signal>
  <!--
   `ActivationToken(id, activation_token)` — emitted just before `ActionInvoked`
   for an action the person activated: an `xdg_activation_v1` token the app
   uses to bring its window forward.
   -->
  <signal name="ActivationToken">
    <arg name="id" type="u"/>
    <arg name="activation_token" type="s"/>
  </signal>
</interface>
"#;

#[cfg(target_os = "linux")]
interface_dispatch::method_interface!(
    NotificationService,
    "org.freedesktop.Notifications",
    NOTIFICATIONS_INTROSPECTION
);

#[cfg(target_os = "linux")]
impl NotificationService {
    fn dispatch_call<'call>(
        &'call self,
        connection: &'call Connection,
        message: &'call Message,
        name: &str,
    ) -> DispatchResult2<'call> {
        match name {
            "Notify" => interface_dispatch::dispatch(
                connection,
                message,
                move |hdr, args: NotifyArgs| async move { Ok(self.notify(&hdr, args.into()).await) },
            ),
            "XTiloReplacePriorityContacts" => interface_dispatch::dispatch(
                connection,
                message,
                move |hdr, contacts: Vec<(String, String, Vec<String>)>| async move {
                    self.x_tilo_replace_priority_contacts(&hdr, contacts).await
                },
            ),
            "XTiloScheduleNotification" => interface_dispatch::dispatch(
                connection,
                message,
                move |hdr, args: scheduled::ScheduleArgs| async move {
                    let caller = self.schedule_caller(&hdr).await?;
                    self.with_schedules(|store| store.schedule(&caller, args))?;
                    self.schedule_changed.poke();
                    Ok(())
                },
            ),
            "XTiloScheduledNotifications" => {
                interface_dispatch::answer(connection, message, async move {
                    let caller = self.schedule_caller(&message.header()).await?;
                    self.with_schedules(|store| {
                        store.deliver_due(
                            &self.bridge,
                            &chrono::Local::now(),
                            self.schedule_workspaces.as_ref(),
                        )?;
                        Ok(store.pending(&caller))
                    })
                })
            }
            "XTiloScheduledAlarms" => interface_dispatch::answer(connection, message, async move {
                let caller = self.schedule_caller(&message.header()).await?;
                self.with_schedules(|store| {
                    store.deliver_due(
                        &self.bridge,
                        &chrono::Local::now(),
                        self.schedule_workspaces.as_ref(),
                    )?;
                    Ok(store.alarm_states(&caller))
                })
            }),
            "XTiloCancelScheduledNotification" => interface_dispatch::dispatch(
                connection,
                message,
                move |hdr, tag: String| async move {
                    let caller = self.schedule_caller(&hdr).await?;
                    self.with_schedules(|store| store.cancel(&caller, &tag))?;
                    self.schedule_changed.poke();
                    Ok(())
                },
            ),
            "CloseNotification" => {
                interface_dispatch::dispatch(connection, message, move |hdr, id: u32| async move {
                    self.close_notification(id, hdr.sender().map(zbus::names::UniqueName::as_str))
                })
            }
            "GetCapabilities" => interface_dispatch::answer(connection, message, async move {
                Ok(self.get_capabilities())
            }),
            "GetServerInformation" => interface_dispatch::answer(connection, message, async move {
                Ok(self.get_server_information())
            }),
            _ => DispatchResult2::NotFound,
        }
    }

    /// `Notify` — the core method. Queues a toast and returns its id.
    async fn notify(&self, hdr: &Header<'_>, request: NotifyRequest) -> u32 {
        let NotifyRequest {
            app_name,
            replaces_id,
            app_icon,
            summary,
            body,
            actions,
            hints,
            expire_timeout,
        } = request;
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let id = if replaces_id != 0
            && self
                .bridge
                .ongoing_update_allowed(replaces_id, sender.as_ref().map(|name| name.as_str()))
        {
            replaces_id
        } else {
            self.bridge.alloc_id()
        };
        let (caller, foreign_identity) = self.resolve_notification_caller(sender.as_ref()).await;
        tracing::debug!(
            "Notifications.Notify: app={app_name:?} id={id} caller_ws={:?}",
            caller.as_ref().map(|c| c.workspace_id)
        );
        let sharing = parse_notification_sharing(&hints);
        let ongoing = caller.is_some()
            && !foreign_identity
            && hints
                .get("category")
                .and_then(|value| <&str>::try_from(value).ok())
                == Some("x-tilo.android-ongoing");
        let ongoing_owner = ongoing
            .then(|| sender.as_ref().map(ToString::to_string))
            .flatten();
        let app_icon = (!app_icon.trim().is_empty())
            .then(|| app_icon.trim().to_string())
            .or_else(|| parse_icon_hint(&hints, "image-path"));
        let desktop_entry = parse_icon_hint(&hints, "desktop-entry");
        let image_data = parse_image_data(&hints);
        self.bridge.push_incoming_with_sharing(
            IncomingNotification {
                dbus_id: id,
                app_name,
                app_icon,
                desktop_entry,
                image_data,
                summary,
                body,
                actions: pair_actions(&actions),
                expire_timeout: if ongoing_owner.is_some() {
                    0
                } else {
                    expire_timeout
                },
                urgency: parse_urgency(&hints),
                caller,
                // Per-app banner/sound rules may use a verified Flatpak identity,
                // but badges and contact routing are Tilo-only extensions. Do not
                // let foreign identity prove authority for either extension.
                badge_count: (!foreign_identity)
                    .then(|| parse_badge_count(&hints))
                    .flatten(),
                contact_endpoint: (!foreign_identity)
                    .then(|| parse_contact_endpoint(&hints))
                    .flatten(),
                sender_name: (!foreign_identity)
                    .then(|| parse_sender_name(&hints))
                    .flatten(),
                sound: parse_notification_sound(&hints),
                incoming_call: parse_incoming_call(&hints),
                machine_wide: false,
            },
            sharing,
            ongoing_owner,
        );
        id
    }

    /// Tilo extension used by Contacts. The complete favourite projection is
    /// replaced atomically; only a launcher-verified Contacts process can call
    /// it. Other apps cannot mutate the host registry even on the same UID.
    async fn x_tilo_replace_priority_contacts(
        &self,
        hdr: &Header<'_>,
        contacts: Vec<(String, String, Vec<String>)>,
    ) -> zbus::fdo::Result<u32> {
        let sender = hdr.sender().cloned().map(OwnedUniqueName::from);
        let caller = self.resolve_caller(sender.as_ref()).await;
        if caller.as_ref().map(|caller| caller.app_id.as_str()) != Some("so.tilo.contacts") {
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
    fn close_notification(&self, id: u32, sender: Option<&str>) -> zbus::fdo::Result<()> {
        if !self.bridge.ongoing_update_allowed(id, sender) {
            return Err(zbus::fdo::Error::AccessDenied(
                "ongoing notification belongs to another sender".into(),
            ));
        }
        self.bridge.request_close(id);
        Ok(())
    }

    /// `GetCapabilities` — advertise the optional features we honour:
    /// `body` (multi-line text), `actions` (default-action activate), `persistence` (critical toasts stay until closed).
    #[allow(
        clippy::unused_self,
        reason = "D-Bus methods dispatch on the service instance, even though this fixed capability list doesn't depend on self"
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
        reason = "D-Bus methods dispatch on the service instance, even though this fixed server info doesn't depend on self"
    )]
    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "Tilo Notifications".to_string(),
            "Tilo".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
            "1.2".to_string(),
        )
    }

    /// `NotificationClosed(id, reason)` — emitted when a toast goes away.
    async fn notification_closed(
        ctxt: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()> {
        ctxt.emit(Self::name(), "NotificationClosed", &(id, reason))
            .await
    }

    /// `ActivationToken(id, activation_token)` — emitted just before
    /// `ActionInvoked` for an action the person activated.
    async fn activation_token(
        ctxt: &SignalEmitter<'_>,
        id: u32,
        activation_token: String,
    ) -> zbus::Result<()> {
        ctxt.emit(Self::name(), "ActivationToken", &(id, activation_token))
            .await
    }

    /// `ActionInvoked(id, action_key)` — emitted when the user activates an action on a toast.
    async fn action_invoked(
        ctxt: &SignalEmitter<'_>,
        id: u32,
        action_key: String,
    ) -> zbus::Result<()> {
        ctxt.emit(Self::name(), "ActionInvoked", &(id, action_key))
            .await
    }
}

/// RAII handle keeping the notification D-Bus connection alive; dropping it releases the bus name and tears down the object server.
#[cfg(target_os = "linux")]
pub struct ServerHandle {
    connection: zbus::Connection,
}

#[cfg(target_os = "linux")]
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
#[cfg(target_os = "linux")]
pub async fn serve_notifications(
    connection: zbus::Connection,
    service: NotificationService,
) -> Result<ServerHandle, zbus::Error> {
    use futures_util::StreamExt;
    // Subscribe before admitting ongoing notifications. A missing bus observer
    // must fail registration rather than leave persistent orphaned notices.
    let mut gone = crate::portal::lifecycle::disconnected_peers(&connection).await?;
    let bridge = service.bridge.clone();
    tokio::spawn(async move {
        while let Some(owner) = gone.next().await {
            bridge.close_ongoing_owner(&owner);
        }
    });
    connection.object_server().at(OBJECT_PATH, service).await?;
    if let Err(e) = connection.request_name(BUS_NAME).await {
        tracing::debug!("notifications: request_name skipped/failed: {e}");
    }
    Ok(ServerHandle { connection })
}

/// Spawn the reverse-direction signal pump: a tokio task draining the
/// bridge's closed/action queues into D-Bus signals; must run inside a tokio runtime, lives until the connection drops.
#[cfg(target_os = "linux")]
pub fn spawn_signal_emitter(
    connection: zbus::Connection,
    bridge: NotificationBridge,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let ctxt = match SignalEmitter::new(&connection, OBJECT_PATH) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("notifications: signal context build failed: {e}");
                return;
            }
        };
        loop {
            let closed = bridge.drain_closed_events();
            let actions = bridge.drain_action_events();
            if closed.is_empty() && actions.is_empty() {
                bridge.signal_wake.wait().await;
                continue;
            }
            for (id, reason) in closed {
                let _ = NotificationService::notification_closed(&ctxt, id, reason.code()).await;
            }
            for (id, key) in actions {
                if let Some(token) = bridge.take_action_token(id) {
                    let _ = NotificationService::activation_token(&ctxt, id, token).await;
                }
                let _ = NotificationService::action_invoked(&ctxt, id, key).await;
            }
        }
    })
}
