//! The compositor-owned `org.freedesktop.Notifications` D-Bus service.
//!
//! In Tilo the desktop *chrome* is owned by the compositor, and
//! notifications are chrome too: this module makes the compositor own
//! `org.freedesktop.Notifications` so `libnotify`/freedesktop-API calls
//! render as OS-drawn, OS-themed toasts (see
//! [`tilo_desktop::notifications::NotificationStack`]) instead of app-drawn windows.
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

#[cfg(target_os = "linux")]
use crate::priority_contacts::ContactProjection;
#[cfg(target_os = "linux")]
use crate::priority_contacts::PriorityContactRegistry;
use std::collections::{HashMap, HashSet, VecDeque};
use tilo_desktop::notifications::ContinuityNotificationId;
#[cfg(target_os = "linux")]
use zbus::names::{InterfaceName, MemberName, OwnedUniqueName};
#[cfg(target_os = "linux")]
use zbus::object_server::{DispatchResult2, Interface, SignalEmitter};
#[cfg(target_os = "linux")]
use zbus::{Connection, ObjectServer, fdo, message::Header, message::Message};

#[cfg(any(test, target_os = "linux"))]
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

#[cfg(any(test, target_os = "linux"))]
use zbus::zvariant::OwnedValue;

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
use crate::portal::CallerMap;
use crate::portal::PortalCaller;
#[cfg(target_os = "linux")]
use crate::portal::interface_dispatch;

/// Bus name the freedesktop notification spec reserves for the
/// notification daemon; owning it makes the compositor the system notification server.
#[cfg(target_os = "linux")]
mod scheduled;

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

/// Most permissive projection an application allows for one notification.
/// The compositor's user policy is applied independently and can only make
/// this more restrictive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSharing {
    #[default]
    Full,
    Redacted,
    LocalOnly,
}

pub const MAX_CONTINUITY_NOTIFICATION_TTL_MS: u64 = 15 * 60 * 1_000;
const MAX_CONTINUITY_NOTIFICATIONS: usize = 128;
const MAX_CONTINUITY_ACTIONS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityNotificationProjection {
    pub id: ContinuityNotificationId,
    pub app_id: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub body_redacted: bool,
    pub actions: Vec<(String, String)>,
    pub urgency: u8,
    pub expires_unix_ms: u64,
}

impl ContinuityNotificationProjection {
    pub fn validate_at(&self, now_unix_ms: u64) -> bool {
        self.id.epoch != [0; 16]
            && self.id.token != [0; 16]
            && valid_notification_app_id(&self.app_id)
            && !self.app_name.is_empty()
            && self.app_name.len() <= 160
            && self.title.len() <= 512
            && self.body.len() <= 4_096
            && self.urgency <= 2
            && self.actions.len() <= MAX_CONTINUITY_ACTIONS
            && self.expires_unix_ms > now_unix_ms
            && self.expires_unix_ms.saturating_sub(now_unix_ms)
                <= MAX_CONTINUITY_NOTIFICATION_TTL_MS
            && [&self.app_name, &self.title]
                .into_iter()
                .all(|value| !value.chars().any(char::is_control))
            && self
                .body
                .chars()
                .all(|character| !character.is_control() || matches!(character, '\n' | '\t'))
            && (!self.body_redacted || self.body.is_empty())
            && self.actions.iter().all(|(key, label)| {
                !key.is_empty()
                    && key.len() <= 128
                    && label.len() <= 256
                    && !key.chars().any(char::is_control)
                    && !label.chars().any(char::is_control)
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuityNotificationDraft {
    pub dbus_id: u32,
    pub app_id: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub actions: Vec<(String, String)>,
    pub urgency: u8,
    pub sharing: NotificationSharing,
    pub previews_allowed: bool,
    pub expires_unix_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContinuityOutboundEvent {
    Project(ContinuityNotificationProjection),
    Retire(ContinuityNotificationId),
}

/// One notification received over an already-authenticated personal fabric
/// route. This is intentionally a distinct type from
/// [`ContinuityNotificationDraft`]: it has no local D-Bus id and cannot be
/// handed back to the local projection API.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteContinuityNotification {
    pub source_device: String,
    pub source_label: String,
    pub projection: ContinuityNotificationProjection,
}

impl RemoteContinuityNotification {
    /// Remote records are displayable/routable but never projectable. Keeping
    /// this property on the type makes the loop-prevention rule available to
    /// consumers without asking them to infer it from absent fields.
    pub const fn is_projectable(&self) -> bool {
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteContinuityEvent {
    Upsert(RemoteContinuityNotification),
    Retire {
        source_device: String,
        id: ContinuityNotificationId,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContinuityNotificationCommand {
    Dismiss,
    Invoke { action_key: String },
}

/// A locally-authorized action for the runtime to send to exactly the
/// authenticated device which authored the displayed projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuityCommandEgress {
    pub target_device: String,
    pub id: ContinuityNotificationId,
    pub command: ContinuityNotificationCommand,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ContinuityOriginRoute {
    dbus_id: u32,
    action_keys: Vec<String>,
    expires_unix_ms: u64,
}

#[derive(Default)]
struct ContinuityState {
    by_id: HashMap<ContinuityNotificationId, ContinuityOriginRoute>,
    by_dbus_id: HashMap<u32, ContinuityNotificationId>,
    outbound: VecDeque<ContinuityOutboundEvent>,
    remote_by_key: HashMap<(String, ContinuityNotificationId), RemoteContinuityNotification>,
    remote_order: VecDeque<(String, ContinuityNotificationId)>,
    remote_events: VecDeque<RemoteContinuityEvent>,
    command_egress: VecDeque<ContinuityCommandEgress>,
    local_closes: VecDeque<u32>,
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
/// it is mapped into a [`tilo_desktop::notifications::Notification`] toast by the chrome pump.
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
    /// Tilo extension: exact unread count. It is ignored unless `caller` was
    /// launcher-verified and policy permits badges for that package.
    pub badge_count: Option<u32>,
    /// Tilo extension: sender endpoint asserted by an explicitly authorized
    /// communications app. The shell hashes and checks it against its private
    /// Contacts-owned registry before it can affect focus policy.
    pub contact_endpoint: Option<String>,
    /// The system sound this notification stands for, from its `category`
    /// hint; `None` when the sender asked for silence (`suppress-sound`).
    pub sound: Option<settings_store::system_sounds::SystemSoundEvent>,
}

type ActionTap = Arc<dyn Fn(u32, &str) + Send + Sync + 'static>;

/// A host action on one durable calendar-alarm occurrence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ScheduledAction {
    Snooze,
    Dismiss,
}

#[derive(Clone, Debug)]
pub(crate) struct ScheduledActionRequest {
    pub id: u64,
    pub app_id: String,
    pub target: tilo_desktop::notifications::ScheduledNotificationTarget,
    pub action: ScheduledAction,
}

#[cfg(target_os = "linux")]
struct ScheduledActionController {
    store: std::sync::Weak<Mutex<scheduled::ScheduleStore>>,
    changed: crate::idle_wake::IdleWake,
}

/// A pending snooze projected from the durable scheduler, independent of history.
#[derive(Clone, Debug)]
pub(crate) struct SnoozedAlarm {
    pub app_id: String,
    pub title: String,
    pub target: tilo_desktop::notifications::ScheduledNotificationTarget,
}

#[derive(Default)]
struct BridgeInner {
    /// `Notify` → compositor toast pump.
    incoming: Vec<IncomingNotification>,
    /// `CloseNotification(id)` → compositor toast pump.
    close_requests: Vec<u32>,
    /// Ongoing notices end when their authenticated unique bus owner exits.
    ongoing_owners: HashMap<u32, String>,
    /// Toast removed in the compositor → `NotificationClosed` signal.
    closed_events: Vec<(u32, CloseReason)>,
    /// Toast action activated in the compositor → `ActionInvoked` signal.
    action_events: Vec<(u32, String)>,
    #[cfg(target_os = "linux")]
    schedule_controller: Option<ScheduledActionController>,
    scheduled_actions: Vec<ScheduledActionRequest>,
    scheduled_action_results: HashMap<u64, Result<(), String>>,
    next_scheduled_action: u64,
    scheduled_targets: HashMap<
        u32,
        (
            PortalCaller,
            tilo_desktop::notifications::ScheduledNotificationTarget,
        ),
    >,
    /// Additional protocol-specific consumers. The freedesktop notification
    /// service still drains `action_events`; taps receive a copy at write time
    /// so a portal backend never races it for the same toast click.
    action_taps: Vec<ActionTap>,
    /// Restrictive sharing hints keyed by the server-assigned D-Bus id. Kept
    /// separate from the long-standing incoming record so non-D-Bus producers
    /// cannot accidentally opt themselves into continuity.
    sharing: HashMap<u32, NotificationSharing>,
    continuity: ContinuityState,
}

impl std::fmt::Debug for BridgeInner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("BridgeInner");
        #[cfg(target_os = "linux")]
        debug.field("schedule_controller", &self.schedule_controller.is_some());
        debug
            .field("incoming", &self.incoming.len())
            .field("close_requests", &self.close_requests.len())
            .field("ongoing_owners", &self.ongoing_owners.len())
            .field("closed_events", &self.closed_events.len())
            .field("action_events", &self.action_events.len())
            .field("scheduled_targets", &self.scheduled_targets.len())
            .field("scheduled_actions", &self.scheduled_actions.len())
            .field(
                "scheduled_action_results",
                &self.scheduled_action_results.len(),
            )
            .field("next_scheduled_action", &self.next_scheduled_action)
            .field("action_taps", &self.action_taps.len())
            .field("sharing", &self.sharing.len())
            .field("continuity_routes", &self.continuity.by_id.len())
            .field("continuity_outbound", &self.continuity.outbound.len())
            .field("continuity_remote", &self.continuity.remote_by_key.len())
            .field(
                "continuity_remote_events",
                &self.continuity.remote_events.len(),
            )
            .field(
                "continuity_local_closes",
                &self.continuity.local_closes.len(),
            )
            .finish()
    }
}

/// Thread-safe mailbox between the async D-Bus service and the
/// synchronous chrome pump; cloneable (`Arc` bump) so each side holds its own handle.
#[derive(Clone)]
pub struct NotificationBridge {
    inner: Arc<Mutex<BridgeInner>>,
    /// Monotonic id source. The spec requires ids be non-zero and unique for
    /// the server's lifetime; `0` is reserved to mean "allocate a fresh id" in `Notify`.
    next_id: Arc<AtomicU32>,
    /// A restart-scoped namespace. Stale routes can never address a new
    /// notification daemon process, even if its D-Bus ids are reused.
    continuity_epoch: [u8; 16],
    /// Parked by the D-Bus signal emitter until a close or action is queued.
    signal_wake: crate::idle_wake::IdleWake,
    compositor_wake: Arc<Mutex<Option<crate::session_runtime::Wake>>>,
}

impl std::fmt::Debug for NotificationBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationBridge")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl Default for NotificationBridge {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationBridge {
    pub fn new() -> Self {
        let continuity_epoch = random_nonzero_token();
        Self {
            inner: Arc::new(Mutex::new(BridgeInner::default())),
            next_id: Arc::new(AtomicU32::new(1)),
            continuity_epoch,
            signal_wake: crate::idle_wake::IdleWake::default(),
            compositor_wake: Arc::new(Mutex::new(None)),
        }
    }

    /// Wake the native session when a background deadline delivers a notice.
    pub fn set_wake(&self, wake: crate::session_runtime::Wake) {
        if let Ok(mut slot) = self.compositor_wake.lock() {
            *slot = Some(wake);
        }
    }

    fn wake_compositor(&self) {
        let wake = self
            .compositor_wake
            .lock()
            .ok()
            .and_then(|slot| slot.clone());
        if let Some(wake) = wake {
            wake();
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
        self.wake_compositor();
    }

    #[cfg(target_os = "linux")]
    fn attach_schedule_controller(
        &self,
        store: &Arc<Mutex<scheduled::ScheduleStore>>,
        changed: crate::idle_wake::IdleWake,
    ) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.schedule_controller = Some(ScheduledActionController {
                store: Arc::downgrade(store),
                changed,
            });
        }
    }

    pub(crate) fn scheduled_actions_available(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.inner.lock().ok().is_some_and(|inner| {
                inner
                    .schedule_controller
                    .as_ref()
                    .is_some_and(|controller| controller.store.upgrade().is_some())
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    pub(crate) fn snoozed_alarms(&self, room: &str) -> Vec<SnoozedAlarm> {
        #[cfg(target_os = "linux")]
        {
            let store = self.inner.lock().ok().and_then(|inner| {
                inner
                    .schedule_controller
                    .as_ref()
                    .and_then(|controller| controller.store.upgrade())
            });
            store
                .and_then(|store| store.lock().ok().map(|store| store.snoozed_alarms(room)))
                .unwrap_or_default()
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = room;
            Vec::new()
        }
    }

    pub(crate) fn request_scheduled_action(
        &self,
        app_id: String,
        target: tilo_desktop::notifications::ScheduledNotificationTarget,
        action: ScheduledAction,
    ) -> Result<u64, String> {
        #[cfg(target_os = "linux")]
        {
            let (id, changed) = {
                let mut inner = self
                    .inner
                    .lock()
                    .map_err(|_| "notification mailbox failed")?;
                let controller = inner
                    .schedule_controller
                    .as_ref()
                    .filter(|controller| controller.store.upgrade().is_some())
                    .ok_or("durable alarm actions are unavailable")?;
                let changed = controller.changed.clone();
                if inner
                    .scheduled_actions
                    .len()
                    .saturating_add(inner.scheduled_action_results.len())
                    >= 64
                {
                    return Err("too many pending alarm actions".into());
                }
                let id = inner
                    .next_scheduled_action
                    .checked_add(1)
                    .ok_or("alarm action identifiers exhausted")?;
                inner.next_scheduled_action = id;
                inner.scheduled_actions.push(ScheduledActionRequest {
                    id,
                    app_id,
                    target,
                    action,
                });
                drop(inner);
                (id, changed)
            };
            changed.poke();
            Ok(id)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (app_id, target, action);
            Err("durable alarm actions are unavailable".into())
        }
    }

    #[cfg(target_os = "linux")]
    fn drain_scheduled_actions(&self) -> Vec<ScheduledActionRequest> {
        self.inner
            .lock()
            .map(|mut inner| std::mem::take(&mut inner.scheduled_actions))
            .unwrap_or_default()
    }

    #[cfg(target_os = "linux")]
    fn finish_scheduled_action(&self, id: u64, result: Result<(), String>) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.scheduled_action_results.insert(id, result);
        }
        self.wake_compositor();
    }

    pub(crate) fn take_scheduled_action_result(&self, id: u64) -> Option<Result<(), String>> {
        self.inner.lock().ok()?.scheduled_action_results.remove(&id)
    }

    #[cfg(target_os = "linux")]
    fn push_scheduled(
        &self,
        notice: IncomingNotification,
        target: tilo_desktop::notifications::ScheduledNotificationTarget,
    ) {
        if let Some(caller) = notice.caller.clone()
            && let Ok(mut inner) = self.inner.lock()
        {
            inner
                .scheduled_targets
                .insert(notice.dbus_id, (caller, target));
            inner.incoming.push(notice);
        }
        self.wake_compositor();
    }

    pub(crate) fn take_scheduled_target(
        &self,
        id: u32,
        caller: Option<&PortalCaller>,
    ) -> Option<tilo_desktop::notifications::ScheduledNotificationTarget> {
        let (owner, target) = self.inner.lock().ok()?.scheduled_targets.remove(&id)?;
        let caller = caller?;
        (owner.app_id == caller.app_id
            && owner.workspace_id == caller.workspace_id
            && target.valid())
        .then_some(target)
    }

    fn push_incoming_with_sharing(
        &self,
        n: IncomingNotification,
        sharing: NotificationSharing,
        ongoing_owner: Option<String>,
    ) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.ongoing_owners.remove(&n.dbus_id);
            if let Some(owner) = ongoing_owner {
                inner.ongoing_owners.insert(n.dbus_id, owner);
            }
            inner.sharing.insert(n.dbus_id, sharing);
            inner.incoming.push(n);
        }
    }

    #[cfg(target_os = "linux")]
    fn close_ongoing_owner(&self, owner: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            let ids: HashSet<_> = inner
                .ongoing_owners
                .iter()
                .filter_map(|(&id, current)| (current == owner).then_some(id))
                .collect();
            inner.ongoing_owners.retain(|id, _| !ids.contains(id));
            inner
                .incoming
                .retain(|notice| !ids.contains(&notice.dbus_id));
            inner.close_requests.extend(ids);
        }
    }

    #[cfg(target_os = "linux")]
    fn ongoing_update_allowed(&self, id: u32, sender: Option<&str>) -> bool {
        self.inner.lock().is_ok_and(|inner| {
            inner
                .ongoing_owners
                .get(&id)
                .is_none_or(|owner| Some(owner.as_str()) == sender)
        })
    }

    /// Consume the sender's privacy ceiling while handling its notification.
    /// Non-D-Bus/test producers have no hint and default to ordinary sharing;
    /// verified caller policy still decides whether projection is allowed.
    pub fn take_sharing(&self, id: u32) -> NotificationSharing {
        self.inner
            .lock()
            .ok()
            .and_then(|mut inner| inner.sharing.remove(&id))
            .unwrap_or_default()
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
            g.ongoing_owners.remove(&id);
            retire_continuity_route(&mut g, id);
            self.signal_wake.poke();
        }
    }

    /// compositor → app: record that a toast action was activated, to be emitted as `ActionInvoked`.
    pub fn note_action(&self, id: u32, action_key: impl Into<String>) {
        if let Ok(mut g) = self.inner.lock() {
            let action_key = action_key.into();
            g.action_events.push((id, action_key.clone()));
            for tap in &g.action_taps {
                tap(id, &action_key);
            }
            self.signal_wake.poke();
        }
    }

    /// Subscribe a protocol-specific action observer. Observers are invoked
    /// while the bridge lock is held, so they must only enqueue/copy data and
    /// never re-enter `NotificationBridge`.
    pub fn add_action_tap(&self, tap: ActionTap) {
        if let Ok(mut g) = self.inner.lock() {
            g.action_taps.push(tap);
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

    /// Register a policy-approved local notification for certified-device
    /// projection. Returns `None` for private, malformed, or expired drafts.
    pub fn project_continuity_notification(
        &self,
        draft: ContinuityNotificationDraft,
        now_unix_ms: u64,
    ) -> Option<ContinuityNotificationId> {
        if draft.sharing == NotificationSharing::LocalOnly
            || draft.dbus_id == 0
            || draft.expires_unix_ms <= now_unix_ms
        {
            return None;
        }
        let mut inner = self.inner.lock().ok()?;
        prune_continuity_routes(&mut inner, now_unix_ms);
        let id = inner
            .continuity
            .by_dbus_id
            .get(&draft.dbus_id)
            .copied()
            .unwrap_or_else(|| ContinuityNotificationId {
                epoch: self.continuity_epoch,
                token: random_nonzero_token(),
            });
        let redact = !draft.previews_allowed || draft.sharing == NotificationSharing::Redacted;
        let projection = ContinuityNotificationProjection {
            id,
            app_id: draft.app_id,
            app_name: draft.app_name,
            title: draft.title,
            body: if redact { String::new() } else { draft.body },
            body_redacted: redact,
            actions: draft.actions,
            urgency: draft.urgency,
            expires_unix_ms: draft
                .expires_unix_ms
                .min(now_unix_ms.saturating_add(MAX_CONTINUITY_NOTIFICATION_TTL_MS)),
        };
        if !projection.validate_at(now_unix_ms) {
            return None;
        }
        if !inner.continuity.by_id.contains_key(&id)
            && inner.continuity.by_id.len() >= MAX_CONTINUITY_NOTIFICATIONS
        {
            return None;
        }
        inner.continuity.by_dbus_id.insert(draft.dbus_id, id);
        inner.continuity.by_id.insert(
            id,
            ContinuityOriginRoute {
                dbus_id: draft.dbus_id,
                action_keys: projection
                    .actions
                    .iter()
                    .map(|(key, _)| key.clone())
                    .collect(),
                expires_unix_ms: projection.expires_unix_ms,
            },
        );
        push_bounded(
            &mut inner.continuity.outbound,
            ContinuityOutboundEvent::Project(projection),
        );
        drop(inner);
        Some(id)
    }

    pub fn drain_continuity_outbound(&self) -> Vec<ContinuityOutboundEvent> {
        self.inner.lock().map_or_else(
            |_| Vec::new(),
            |mut inner| inner.continuity.outbound.drain(..).collect(),
        )
    }

    /// Apply a command routed back over an authenticated device-fabric lane.
    /// Unknown, expired, restarted, or non-allowlisted routes fail closed.
    pub fn apply_continuity_command(
        &self,
        id: ContinuityNotificationId,
        command: &ContinuityNotificationCommand,
        now_unix_ms: u64,
    ) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        prune_continuity_routes(&mut inner, now_unix_ms);
        let Some(route) = inner.continuity.by_id.get(&id) else {
            return false;
        };
        let dbus_id = route.dbus_id;
        if let ContinuityNotificationCommand::Invoke { action_key } = command {
            if !route
                .action_keys
                .iter()
                .any(|allowed| allowed == action_key)
            {
                return false;
            }
            inner.action_events.push((dbus_id, action_key.clone()));
            for tap in &inner.action_taps {
                tap(dbus_id, action_key);
            }
        }
        inner.closed_events.push((dbus_id, CloseReason::Dismissed));
        push_bounded(&mut inner.continuity.local_closes, dbus_id);
        retire_continuity_route(&mut inner, dbus_id);
        self.signal_wake.poke();
        true
    }

    /// Admit a projection only under the authenticated transport source that
    /// delivered it. The source is not read from sender-controlled payload.
    /// Exact duplicates are ignored; replacements update the existing key.
    pub fn receive_continuity_projection(
        &self,
        authenticated_source: &str,
        source_label: &str,
        projection: ContinuityNotificationProjection,
        now_unix_ms: u64,
    ) -> bool {
        if !valid_continuity_source(authenticated_source, source_label)
            || !projection.validate_at(now_unix_ms)
        {
            return false;
        }
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        expire_remote_continuity(&mut inner, now_unix_ms);
        let key = (authenticated_source.to_string(), projection.id);
        let incoming = RemoteContinuityNotification {
            source_device: authenticated_source.to_string(),
            source_label: source_label.to_string(),
            projection,
        };
        if inner.continuity.remote_by_key.get(&key) == Some(&incoming) {
            return false;
        }
        if !inner.continuity.remote_by_key.contains_key(&key) {
            evict_oldest_remote_if_full(&mut inner);
            inner.continuity.remote_order.push_back(key.clone());
        }
        inner.continuity.remote_by_key.insert(key, incoming.clone());
        push_bounded(
            &mut inner.continuity.remote_events,
            RemoteContinuityEvent::Upsert(incoming),
        );
        true
    }

    /// Retire only the record authored by the authenticated source. A peer
    /// cannot remove a sibling device's notification by copying its id.
    pub fn retire_remote_continuity(
        &self,
        authenticated_source: &str,
        id: ContinuityNotificationId,
    ) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        remove_remote_continuity(&mut inner, authenticated_source, id, true)
    }

    /// Validate a local user command against the currently displayed remote
    /// record, then queue it for the exact authenticated origin. Action keys
    /// are accepted only when advertised by that live projection.
    pub fn request_remote_continuity_command(
        &self,
        authenticated_source: &str,
        id: ContinuityNotificationId,
        command: ContinuityNotificationCommand,
        now_unix_ms: u64,
    ) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        expire_remote_continuity(&mut inner, now_unix_ms);
        let key = (authenticated_source.to_string(), id);
        let Some(notification) = inner.continuity.remote_by_key.get(&key) else {
            return false;
        };
        if let ContinuityNotificationCommand::Invoke { action_key } = &command
            && !notification
                .projection
                .actions
                .iter()
                .any(|(allowed, _)| allowed == action_key)
        {
            return false;
        }
        inner.continuity.remote_by_key.remove(&key);
        inner
            .continuity
            .remote_order
            .retain(|candidate| candidate != &key);
        push_bounded(
            &mut inner.continuity.command_egress,
            ContinuityCommandEgress {
                target_device: authenticated_source.to_string(),
                id,
                command,
            },
        );
        push_bounded(
            &mut inner.continuity.remote_events,
            RemoteContinuityEvent::Retire {
                source_device: authenticated_source.to_string(),
                id,
            },
        );
        true
    }

    /// Expire remote projections even when no new fabric traffic arrives.
    pub fn expire_remote_continuity(&self, now_unix_ms: u64) {
        if let Ok(mut inner) = self.inner.lock() {
            expire_remote_continuity(&mut inner, now_unix_ms);
        }
    }

    /// Drop ephemeral projections whose authenticated origin is no longer an
    /// online certified peer. This state is deliberately not persisted: a
    /// reconnect requires the origin to send a fresh, live projection.
    pub fn retain_remote_continuity_sources(&self, online_sources: &HashSet<String>) -> usize {
        let Ok(mut inner) = self.inner.lock() else {
            return 0;
        };
        let stale = inner
            .continuity
            .remote_by_key
            .keys()
            .filter(|(source, _)| !online_sources.contains(source))
            .cloned()
            .collect::<Vec<_>>();
        let mut removed: usize = 0;
        for (source, id) in stale {
            removed = removed.saturating_add(usize::from(remove_remote_continuity(
                &mut inner, &source, id, true,
            )));
        }
        removed
    }

    pub fn drain_remote_continuity_events(&self) -> Vec<RemoteContinuityEvent> {
        self.inner.lock().map_or_else(
            |_| Vec::new(),
            |mut inner| inner.continuity.remote_events.drain(..).collect(),
        )
    }

    pub fn drain_continuity_command_egress(&self) -> Vec<ContinuityCommandEgress> {
        self.inner.lock().map_or_else(
            |_| Vec::new(),
            |mut inner| inner.continuity.command_egress.drain(..).collect(),
        )
    }

    /// Local D-Bus toasts dismissed from another certified device. Chrome
    /// removes these without emitting a second `NotificationClosed` signal.
    pub fn drain_continuity_local_closes(&self) -> Vec<u32> {
        self.inner.lock().map_or_else(
            |_| Vec::new(),
            |mut inner| inner.continuity.local_closes.drain(..).collect(),
        )
    }

    #[cfg(test)]
    fn remote_continuity_len(&self) -> usize {
        self.inner
            .lock()
            .map_or(0, |inner| inner.continuity.remote_by_key.len())
    }
}

fn random_nonzero_token() -> [u8; 16] {
    let mut token = [0; 16];
    while token == [0; 16] {
        UnwrapErr(SysRng).fill_bytes(&mut token);
    }
    token
}

fn push_bounded<T>(queue: &mut VecDeque<T>, item: T) {
    if queue.len() == MAX_CONTINUITY_NOTIFICATIONS {
        queue.pop_front();
    }
    queue.push_back(item);
}

fn prune_continuity_routes(inner: &mut BridgeInner, now_unix_ms: u64) {
    let expired = inner
        .continuity
        .by_id
        .iter()
        .filter_map(|(id, route)| (route.expires_unix_ms <= now_unix_ms).then_some(*id))
        .collect::<Vec<_>>();
    for id in expired {
        if let Some(route) = inner.continuity.by_id.remove(&id) {
            inner.continuity.by_dbus_id.remove(&route.dbus_id);
            push_bounded(
                &mut inner.continuity.outbound,
                ContinuityOutboundEvent::Retire(id),
            );
        }
    }
}

fn retire_continuity_route(inner: &mut BridgeInner, dbus_id: u32) {
    let Some(id) = inner.continuity.by_dbus_id.remove(&dbus_id) else {
        return;
    };
    inner.continuity.by_id.remove(&id);
    push_bounded(
        &mut inner.continuity.outbound,
        ContinuityOutboundEvent::Retire(id),
    );
}

fn valid_continuity_source(device: &str, label: &str) -> bool {
    !device.is_empty()
        && device.len() <= 128
        && !device.chars().any(char::is_control)
        && !label.is_empty()
        && label.len() <= 128
        && !label.chars().any(char::is_control)
}

fn remove_remote_continuity(
    inner: &mut BridgeInner,
    source: &str,
    id: ContinuityNotificationId,
    emit: bool,
) -> bool {
    let key = (source.to_string(), id);
    if inner.continuity.remote_by_key.remove(&key).is_none() {
        return false;
    }
    inner
        .continuity
        .remote_order
        .retain(|candidate| candidate != &key);
    if emit {
        push_bounded(
            &mut inner.continuity.remote_events,
            RemoteContinuityEvent::Retire {
                source_device: source.to_string(),
                id,
            },
        );
    }
    true
}

fn expire_remote_continuity(inner: &mut BridgeInner, now_unix_ms: u64) {
    let expired = inner
        .continuity
        .remote_by_key
        .iter()
        .filter_map(|((source, id), notification)| {
            (notification.projection.expires_unix_ms <= now_unix_ms)
                .then_some((source.clone(), *id))
        })
        .collect::<Vec<_>>();
    for (source, id) in expired {
        remove_remote_continuity(inner, &source, id, true);
    }
}

fn evict_oldest_remote_if_full(inner: &mut BridgeInner) {
    if inner.continuity.remote_by_key.len() < MAX_CONTINUITY_NOTIFICATIONS {
        return;
    }
    while let Some((source, id)) = inner.continuity.remote_order.pop_front() {
        if inner
            .continuity
            .remote_by_key
            .remove(&(source.clone(), id))
            .is_some()
        {
            push_bounded(
                &mut inner.continuity.remote_events,
                RemoteContinuityEvent::Retire {
                    source_device: source,
                    id,
                },
            );
            break;
        }
    }
}

/// Parse the `urgency` hint (a `y`/byte); defaults to `1` (normal) when absent or the wrong type — never refuse a notification over a bad hint.
#[cfg(any(test, target_os = "linux"))]
fn parse_urgency(hints: &HashMap<String, OwnedValue>) -> u8 {
    hints
        .get("urgency")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(1)
}

/// The system sound a notification asks for: `None` when the sender set
/// `suppress-sound`, otherwise the event its `category` hint names.
#[cfg(any(test, target_os = "linux"))]
fn parse_notification_sound(
    hints: &HashMap<String, OwnedValue>,
) -> Option<settings_store::system_sounds::SystemSoundEvent> {
    let suppressed = hints
        .get("suppress-sound")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false);
    let category = hints
        .get("category")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| String::try_from(value).ok());
    (!suppressed).then(|| {
        settings_store::system_sounds::SystemSoundEvent::for_notification_category(
            category.as_deref(),
        )
    })
}

#[cfg(any(test, target_os = "linux"))]
fn parse_badge_count(hints: &HashMap<String, OwnedValue>) -> Option<u32> {
    hints
        .get("x-tilo-badge-count")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| u32::try_from(value).ok())
        .map(|value| value.min(9_999))
}

#[cfg(any(test, target_os = "linux"))]
fn parse_contact_endpoint(hints: &HashMap<String, OwnedValue>) -> Option<String> {
    let value = hints
        .get("x-tilo-contact-handle")?
        .try_clone()
        .ok()
        .and_then(|value| String::try_from(value).ok())?;
    (value.len() <= 320 && !value.chars().any(char::is_control)).then_some(value)
}

#[cfg(any(test, target_os = "linux"))]
fn parse_notification_sharing(hints: &HashMap<String, OwnedValue>) -> NotificationSharing {
    let Some(value) = hints.get("x-tilo-device-sharing") else {
        return NotificationSharing::Full;
    };
    match value
        .try_clone()
        .ok()
        .and_then(|value| String::try_from(value).ok())
        .as_deref()
    {
        Some("full") => NotificationSharing::Full,
        Some("redacted") => NotificationSharing::Redacted,
        _ => NotificationSharing::LocalOnly,
    }
}

/// Un-flatten the spec's `[key1, label1, key2, label2, …]` actions array into `(key, label)` pairs, dropping a dangling trailing key.
#[cfg(any(test, target_os = "linux"))]
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
                    .map(|workspace| workspace.room.to_string())
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

/// Return a policy identity for a foreign desktop notification sender only
/// when the process is demonstrably inside an installed Flatpak sandbox.
///
/// Plain RPM/host processes are deliberately not guessed from their argv or
/// the `Notify.app_name` field: when Tilo launched one, the trusted launcher
/// ancestry above supplies its desktop id; otherwise it remains global-policy
/// only. Flatpak provides two independent pieces of immutable runtime
/// metadata — its per-process `.flatpak-info` and systemd cgroup — which are
/// checked against an installed desktop entry before a per-app rule is used.
#[cfg(target_os = "linux")]
fn verified_foreign_notification_identity(pid: u32) -> Option<String> {
    let entries = &desktop_entry::cache::cached_scan().entries;
    verified_foreign_notification_identity_from(Path::new("/proc"), pid, entries)
}

#[cfg(any(test, target_os = "linux"))]
fn verified_foreign_notification_identity_from(
    proc_root: &Path,
    pid: u32,
    entries: &[desktop_entry::DesktopEntry],
) -> Option<String> {
    let proc = proc_root.join(pid.to_string());
    let app_id = flatpak_application_id(&proc.join("root/.flatpak-info"))?;
    let cgroup = read_bounded(&proc.join("cgroup"), 16 * 1024)?;
    if !flatpak_cgroup_matches(&cgroup, &app_id) {
        return None;
    }
    entries
        .iter()
        .any(|entry| {
            matches!(
                &entry.source,
                desktop_entry::EntrySource::Flatpak { app_id: installed } if installed == &app_id
            )
        })
        .then_some(app_id)
}

#[cfg(any(test, target_os = "linux"))]
fn read_bounded(path: &Path, max_bytes: u64) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    (metadata.len() <= max_bytes).then(|| std::fs::read_to_string(path).ok())?
}

fn valid_notification_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

#[cfg(any(test, target_os = "linux"))]
fn flatpak_application_id(info_path: &Path) -> Option<String> {
    let info = read_bounded(info_path, 16 * 1024)?;
    let mut application_section = false;
    for line in info.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            application_section = line == "[Application]";
            continue;
        }
        if application_section && let Some(id) = line.strip_prefix("name=") {
            let id = id.trim();
            return valid_notification_app_id(id).then(|| id.to_string());
        }
    }
    None
}

#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Systemd scope unit names use the literal lowercase `.scope` suffix; accepting a differently cased unit would not identify a Flatpak application scope."
)]
#[cfg(any(test, target_os = "linux"))]
fn flatpak_cgroup_matches(cgroup: &str, app_id: &str) -> bool {
    let raw_prefix = format!("app-flatpak-{app_id}");
    let escaped_prefix = format!("app-flatpak-{}", systemd_escape(app_id));
    cgroup.lines().any(|line| {
        let Some((_, path)) = line.rsplit_once(':') else {
            return false;
        };
        path.split('/').any(|component| {
            [raw_prefix.as_str(), escaped_prefix.as_str()]
                .iter()
                .any(|prefix| {
                    component.strip_prefix(prefix).is_some_and(|suffix| {
                        suffix == ".scope"
                            || (suffix.starts_with('-') && suffix.ends_with(".scope"))
                    })
                })
        })
    })
}

#[cfg(any(test, target_os = "linux"))]
fn systemd_escape(value: &str) -> String {
    value.bytes().fold(String::new(), |mut out, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_') {
            out.push(char::from(byte));
        } else {
            out.push_str("\\x");
            out.push_str(&hex::encode([byte]));
        }
        out
    })
}

/// One `Notify` call's spec arguments; `app_icon` is accepted on the wire
/// and ignored.
#[cfg(target_os = "linux")]
struct NotifyRequest {
    app_name: String,
    replaces_id: u32,
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
type NotifyArgs = (
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
        (app_name, replaces_id, _app_icon, summary, body, actions, hints, expire_timeout): NotifyArgs,
    ) -> Self {
        Self {
            app_name,
            replaces_id,
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
const NOTIFICATIONS_INTROSPECTION: &str = r#"<interface name="org.freedesktop.Notifications">
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
</interface>
"#;

#[cfg(target_os = "linux")]
#[zbus::export::async_trait::async_trait]
impl Interface for NotificationService {
    fn name() -> InterfaceName<'static> {
        InterfaceName::from_static_str_unchecked("org.freedesktop.Notifications")
    }

    async fn get(
        &self,
        _property: &str,
        _server: &ObjectServer,
        _connection: &Connection,
        _header: Option<&Header<'_>>,
        _emitter: &SignalEmitter<'_>,
    ) -> Option<fdo::Result<OwnedValue>> {
        None
    }

    async fn get_all(
        &self,
        _server: &ObjectServer,
        _connection: &Connection,
        _header: Option<&Header<'_>>,
        _emitter: &SignalEmitter<'_>,
    ) -> fdo::Result<HashMap<String, OwnedValue>> {
        Ok(HashMap::new())
    }

    fn set<'call>(
        &'call self,
        _property: &'call str,
        _value: &'call zbus::zvariant::Value<'_>,
        _server: &'call ObjectServer,
        _connection: &'call Connection,
        _header: Option<&'call Header<'_>>,
        _emitter: &'call SignalEmitter<'_>,
    ) -> DispatchResult2<'call> {
        DispatchResult2::NotFound
    }

    async fn set_mut(
        &mut self,
        _property: &str,
        _value: &zbus::zvariant::Value<'_>,
        _server: &ObjectServer,
        _connection: &Connection,
        _header: Option<&Header<'_>>,
        _emitter: &SignalEmitter<'_>,
    ) -> Option<fdo::Result<()>> {
        None
    }

    fn call<'call>(
        &'call self,
        _server: &'call ObjectServer,
        connection: &'call Connection,
        message: &'call Message,
        name: MemberName<'call>,
    ) -> DispatchResult2<'call> {
        match name.as_str() {
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

    fn call_mut<'call>(
        &'call mut self,
        _server: &'call ObjectServer,
        _connection: &'call Connection,
        _message: &'call Message,
        _name: MemberName<'call>,
    ) -> DispatchResult2<'call> {
        DispatchResult2::NotFound
    }

    fn introspect_to_writer(&self, writer: &mut dyn std::fmt::Write, level: usize) {
        interface_dispatch::write_introspection(writer, level, NOTIFICATIONS_INTROSPECTION);
    }
}

#[cfg(target_os = "linux")]
impl NotificationService {
    /// `Notify` — the core method. Queues a toast and returns its id.
    async fn notify(&self, hdr: &Header<'_>, request: NotifyRequest) -> u32 {
        let NotifyRequest {
            app_name,
            replaces_id,
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
        self.bridge.push_incoming_with_sharing(
            IncomingNotification {
                dbus_id: id,
                app_name,
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
                sound: parse_notification_sound(&hints),
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
///
/// # Errors
///
/// Returns an error if registering [`NotificationService`] at
/// [`OBJECT_PATH`] fails. A failed [`BUS_NAME`] request is logged, not
/// propagated.
#[cfg(target_os = "linux")]
pub async fn serve_notifications(
    connection: zbus::Connection,
    service: NotificationService,
) -> Result<ServerHandle, zbus::Error> {
    use futures_util::StreamExt;
    // Subscribe before admitting ongoing notifications. A missing bus observer
    // must fail registration rather than leave persistent orphaned notices.
    let proxy = zbus::fdo::DBusProxy::new(&connection).await?;
    let mut owners = proxy.receive_name_owner_changed().await?;
    let bridge = service.bridge.clone();
    tokio::spawn(async move {
        while let Some(event) = owners.next().await {
            let Ok(args) = event.args() else {
                continue;
            };
            if args.name().as_str().starts_with(':')
                && args.new_owner().as_ref().is_none()
                && let Some(owner) = args.old_owner().as_ref()
            {
                bridge.close_ongoing_owner(owner.as_str());
            }
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
                let _ = NotificationService::action_invoked(&ctxt, id, key).await;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::shell::state::WorkspaceId;

    fn flatpak_entries(root: &Path, app_id: &str) -> Vec<desktop_entry::DesktopEntry> {
        let apps = root.join("applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::write(
            apps.join(format!("{app_id}.desktop")),
            format!(
                "[Desktop Entry]\nType=Application\nName=Trusted Chat\nExec=/bin/true\nX-Flatpak={app_id}\n"
            ),
        )
        .unwrap();
        desktop_entry::scan_dirs(&[apps], &[desktop_entry::scan::CURRENT_DESKTOP.to_string()])
            .entries
    }

    fn write_flatpak_proc(root: &Path, pid: u32, app_id: &str, cgroup_app_id: &str) {
        let proc = root.join(pid.to_string());
        std::fs::create_dir_all(proc.join("root")).unwrap();
        std::fs::write(
            proc.join("root/.flatpak-info"),
            format!("[Application]\nname={app_id}\n"),
        )
        .unwrap();
        std::fs::write(
            proc.join("cgroup"),
            format!(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-{cgroup_app_id}-123.scope\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn bridge_messages_round_trip_and_hints_are_bounded() {
        // Phase `alloc_id_is_monotonic_and_never_zero`.
        {
            let b = NotificationBridge::new();
            let a = b.alloc_id();
            let c = b.alloc_id();
            assert_ne!(a, 0);
            assert_ne!(c, 0);
            assert!(c > a, "ids must be monotonic: {a} then {c}");
        }
        // Phase `incoming_round_trips_through_bridge`.
        {
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
                sound: Some(settings_store::system_sounds::SystemSoundEvent::Notification),
            });
            assert_eq!(b.pending_incoming_len(), 1);
            let drained = b.drain_incoming();
            assert_eq!(drained.len(), 1);
            assert_eq!(drained[0].summary, "Hi");
            // Drain is destructive.
            assert_eq!(b.pending_incoming_len(), 0);
            assert!(b.drain_incoming().is_empty());
        }
        // Phase `close_request_round_trips`.
        {
            let b = NotificationBridge::new();
            b.request_close(42);
            assert_eq!(b.drain_close_requests(), vec![42]);
            assert!(b.drain_close_requests().is_empty());
        }
        // Phase `closed_and_action_events_round_trip`.
        {
            let b = NotificationBridge::new();
            b.note_closed(7, CloseReason::Expired);
            b.note_action(7, "default");
            assert_eq!(b.drain_closed_events(), vec![(7, CloseReason::Expired)]);
            assert_eq!(b.drain_action_events(), vec![(7, "default".to_string())]);
        }
        // Phase `close_reason_codes_match_spec`.
        {
            assert_eq!(CloseReason::Expired.code(), 1);
            assert_eq!(CloseReason::Dismissed.code(), 2);
            assert_eq!(CloseReason::Closed.code(), 3);
        }
        // Phase `pair_actions_unflattens_and_drops_dangling_key`.
        {
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
        // Phase `parse_urgency_defaults_to_normal`.
        {
            let hints: HashMap<String, OwnedValue> = HashMap::new();
            assert_eq!(parse_urgency(&hints), 1);
        }
        // Phase `parse_urgency_reads_critical_byte`.
        {
            let mut hints: HashMap<String, OwnedValue> = HashMap::new();
            hints.insert("urgency".to_string(), OwnedValue::from(2u8));
            assert_eq!(parse_urgency(&hints), 2);
        }
        // Phase `tilo_hints_are_bounded_before_they_reach_chrome`.
        {
            let mut hints = HashMap::new();
            hints.insert("x-tilo-badge-count".into(), OwnedValue::from(u32::MAX));
            hints.insert(
                "x-tilo-contact-handle".into(),
                OwnedValue::from(zbus::zvariant::Str::from("ada@example.test")),
            );
            assert_eq!(parse_badge_count(&hints), Some(9_999));
            assert_eq!(
                parse_contact_endpoint(&hints).as_deref(),
                Some("ada@example.test")
            );
            hints.insert(
                "x-tilo-contact-handle".into(),
                OwnedValue::from(zbus::zvariant::Str::from("bad\nhandle")),
            );
            assert_eq!(parse_contact_endpoint(&hints), None);
        }
    }

    fn continuity_draft(sharing: NotificationSharing) -> ContinuityNotificationDraft {
        ContinuityNotificationDraft {
            dbus_id: 7,
            app_id: "so.tilo.mail".into(),
            app_name: "Mail".into(),
            title: "Private subject".into(),
            body: "Private body".into(),
            actions: vec![("reply".into(), "Reply".into())],
            urgency: 1,
            sharing,
            previews_allowed: true,
            expires_unix_ms: 10_000,
        }
    }

    #[test]
    fn continuity_projection_commands_and_routes_are_allowlisted_and_expire() {
        // Phase `continuity_projection_redacts_and_local_only_never_registers`.
        {
            let bridge = NotificationBridge::new();
            assert!(
                bridge
                    .project_continuity_notification(
                        continuity_draft(NotificationSharing::LocalOnly),
                        1_000
                    )
                    .is_none()
            );
            let id = bridge
                .project_continuity_notification(
                    continuity_draft(NotificationSharing::Redacted),
                    1_000,
                )
                .unwrap();
            let events = bridge.drain_continuity_outbound();
            let ContinuityOutboundEvent::Project(projected) = &events[0] else {
                panic!("expected projection");
            };
            assert_eq!(projected.id, id);
            assert!(projected.body.is_empty());
            assert!(projected.body_redacted);
        }
        // Phase `continuity_commands_are_live_allowlisted_and_retire_once`.
        {
            let bridge = NotificationBridge::new();
            let id = bridge
                .project_continuity_notification(continuity_draft(NotificationSharing::Full), 1_000)
                .unwrap();
            bridge.drain_continuity_outbound();

            assert!(!bridge.apply_continuity_command(
                id,
                &ContinuityNotificationCommand::Invoke {
                    action_key: "delete-everything".into(),
                },
                2_000,
            ));
            assert!(bridge.apply_continuity_command(
                id,
                &ContinuityNotificationCommand::Invoke {
                    action_key: "reply".into(),
                },
                2_000,
            ));
            assert_eq!(bridge.drain_action_events(), vec![(7, "reply".into())]);
            assert_eq!(
                bridge.drain_closed_events(),
                vec![(7, CloseReason::Dismissed)]
            );
            assert_eq!(
                bridge.drain_continuity_outbound(),
                vec![ContinuityOutboundEvent::Retire(id)]
            );
            assert!(!bridge.apply_continuity_command(
                id,
                &ContinuityNotificationCommand::Dismiss,
                2_001,
            ));
        }
        // Phase `continuity_routes_expire_and_restart_epoch_invalidates_them`.
        {
            let first = NotificationBridge::new();
            let id = first
                .project_continuity_notification(continuity_draft(NotificationSharing::Full), 1_000)
                .unwrap();
            assert!(!first.apply_continuity_command(
                id,
                &ContinuityNotificationCommand::Dismiss,
                10_000,
            ));

            let restarted = NotificationBridge::new();
            let replacement = restarted
                .project_continuity_notification(continuity_draft(NotificationSharing::Full), 1_000)
                .unwrap();
            assert_ne!(id.epoch, replacement.epoch);
            assert!(!restarted.apply_continuity_command(
                id,
                &ContinuityNotificationCommand::Dismiss,
                2_000,
            ));
        }
    }

    fn remote_projection(token: u8, title: &str) -> ContinuityNotificationProjection {
        ContinuityNotificationProjection {
            id: ContinuityNotificationId {
                epoch: [1; 16],
                token: [token; 16],
            },
            app_id: "so.tilo.mail".into(),
            app_name: "Mail".into(),
            title: title.into(),
            body: "Message".into(),
            body_redacted: false,
            actions: vec![("reply".into(), "Reply".into())],
            urgency: 1,
            expires_unix_ms: 10_000,
        }
    }

    #[test]
    fn remote_inbox_keys_dedupes_prunes_and_bounds_by_source() {
        // Phase `remote_inbox_keys_identical_ids_by_authenticated_source`.
        {
            let bridge = NotificationBridge::new();
            let projection = remote_projection(9, "Same id");
            assert!(bridge.receive_continuity_projection("phone", "Phone", projection.clone(), 1));
            assert!(bridge.receive_continuity_projection("tablet", "Tablet", projection, 1));
            assert_eq!(bridge.remote_continuity_len(), 2);

            assert!(bridge.retire_remote_continuity("phone", remote_projection(9, "x").id));
            assert_eq!(bridge.remote_continuity_len(), 1);
            assert!(!bridge.retire_remote_continuity("attacker", remote_projection(9, "x").id));
        }
        // Phase `remote_inbox_dedupes_and_replaces_one_source_record`.
        {
            let bridge = NotificationBridge::new();
            let first = remote_projection(2, "First");
            assert!(bridge.receive_continuity_projection("phone", "Phone", first.clone(), 1));
            assert!(!bridge.receive_continuity_projection("phone", "Phone", first.clone(), 1));
            assert_eq!(bridge.drain_remote_continuity_events().len(), 1);

            let mut replacement = first;
            replacement.title = "Replacement".into();
            assert!(bridge.receive_continuity_projection("phone", "Phone", replacement, 1));
            assert_eq!(bridge.remote_continuity_len(), 1);
            let events = bridge.drain_remote_continuity_events();
            let RemoteContinuityEvent::Upsert(notification) = &events[0] else {
                panic!("expected replacement upsert");
            };
            assert_eq!(notification.projection.title, "Replacement");
        }
        // Phase `offline_source_is_pruned_without_retiring_online_sibling`.
        {
            let bridge = NotificationBridge::new();
            let id = remote_projection(12, "Shared id").id;
            assert!(bridge.receive_continuity_projection(
                "phone",
                "Phone",
                remote_projection(12, "Phone"),
                1,
            ));
            assert!(bridge.receive_continuity_projection(
                "tablet",
                "Tablet",
                remote_projection(12, "Tablet"),
                1,
            ));
            let _ = bridge.drain_remote_continuity_events();

            assert_eq!(
                bridge.retain_remote_continuity_sources(&HashSet::from(["tablet".to_string()])),
                1
            );
            assert_eq!(bridge.remote_continuity_len(), 1);
            assert_eq!(
                bridge.drain_remote_continuity_events(),
                vec![RemoteContinuityEvent::Retire {
                    source_device: "phone".into(),
                    id,
                }]
            );
            assert!(bridge.retire_remote_continuity("tablet", id));
        }
        // Phase `remote_inbox_is_bounded_and_evicts_oldest`.
        {
            let bridge = NotificationBridge::new();
            for index in 0..=MAX_CONTINUITY_NOTIFICATIONS {
                let token = u8::try_from(index.saturating_add(1)).unwrap();
                assert!(bridge.receive_continuity_projection(
                    "phone",
                    "Phone",
                    remote_projection(token, "Bounded"),
                    1,
                ));
            }
            assert_eq!(bridge.remote_continuity_len(), MAX_CONTINUITY_NOTIFICATIONS);
            let events = bridge.drain_remote_continuity_events();
            assert!(events.iter().any(|event| matches!(
                event,
                RemoteContinuityEvent::Retire { id, .. } if id.token == [1; 16]
            )));
        }
    }

    #[test]
    fn remote_records_route_live_actions_expire_and_fail_closed() {
        // Phase `remote_commands_validate_live_action_and_route_exact_origin`.
        {
            let bridge = NotificationBridge::new();
            let projection = remote_projection(3, "Action");
            let id = projection.id;
            assert!(bridge.receive_continuity_projection("phone", "Phone", projection, 1));
            assert!(!bridge.request_remote_continuity_command(
                "phone",
                id,
                ContinuityNotificationCommand::Invoke {
                    action_key: "archive".into(),
                },
                2,
            ));
            assert!(bridge.request_remote_continuity_command(
                "phone",
                id,
                ContinuityNotificationCommand::Invoke {
                    action_key: "reply".into(),
                },
                2,
            ));
            assert_eq!(
                bridge.drain_continuity_command_egress(),
                vec![ContinuityCommandEgress {
                    target_device: "phone".into(),
                    id,
                    command: ContinuityNotificationCommand::Invoke {
                        action_key: "reply".into(),
                    },
                }]
            );
            assert_eq!(bridge.remote_continuity_len(), 0);
        }
        // Phase `remote_expiry_and_retire_emit_local_removal`.
        {
            let bridge = NotificationBridge::new();
            let projection = remote_projection(4, "Expires");
            let id = projection.id;
            assert!(bridge.receive_continuity_projection("phone", "Phone", projection, 1));
            bridge.drain_remote_continuity_events();
            bridge.expire_remote_continuity(10_000);
            assert_eq!(bridge.remote_continuity_len(), 0);
            assert_eq!(
                bridge.drain_remote_continuity_events(),
                vec![RemoteContinuityEvent::Retire {
                    source_device: "phone".into(),
                    id,
                }]
            );
        }
        // Phase `remote_record_carries_provenance_and_is_never_projectable`.
        {
            let bridge = NotificationBridge::new();
            assert!(bridge.receive_continuity_projection(
                "certified-phone",
                "Quinten's phone",
                remote_projection(5, "Remote"),
                1,
            ));
            let events = bridge.drain_remote_continuity_events();
            let RemoteContinuityEvent::Upsert(notification) = &events[0] else {
                panic!("expected remote upsert");
            };
            assert_eq!(notification.source_device, "certified-phone");
            assert_eq!(notification.source_label, "Quinten's phone");
            assert!(!notification.is_projectable());
        }
        // Phase `malformed_sharing_hint_fails_closed`.
        {
            let hints = HashMap::from([(
                "x-tilo-device-sharing".to_string(),
                OwnedValue::from(zbus::zvariant::Str::from("surprise")),
            )]);
            assert_eq!(
                parse_notification_sharing(&hints),
                NotificationSharing::LocalOnly
            );
        }
    }

    #[test]
    fn flatpak_identity_requires_matching_scope_runtime_and_install() {
        // Phase `systemd_escape_and_scope_match_handle_flatpak_ids_without_indexing`.
        {
            assert_eq!(systemd_escape("org.example.App"), "org.example.App");
            assert_eq!(systemd_escape("org.example App"), "org.example\\x20App");
            assert!(flatpak_cgroup_matches(
                "0::/app.slice/app-flatpak-org.example\\x20App-42.scope",
                "org.example App"
            ));
            assert!(!flatpak_cgroup_matches(
                "0::/app.slice/app-flatpak-org.example\\x20App-42.SCOPE",
                "org.example App"
            ));
        }
        // Phase `foreign_flatpak_identity_requires_matching_runtime_cgroup_and_install`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let app_id = "org.example.Chat";
            let entries = flatpak_entries(tmp.path(), app_id);
            write_flatpak_proc(tmp.path(), 404, app_id, app_id);

            assert_eq!(
                verified_foreign_notification_identity_from(tmp.path(), 404, &entries).as_deref(),
                Some(app_id),
                "identity comes from runtime metadata/cgroup, not Notify.app_name"
            );

            write_flatpak_proc(tmp.path(), 405, app_id, "org.example.Spoof");
            assert_eq!(
                verified_foreign_notification_identity_from(tmp.path(), 405, &entries),
                None,
                "a mismatched cgroup leaves a foreign sender global-only"
            );

            write_flatpak_proc(
                tmp.path(),
                406,
                "org.example.Missing",
                "org.example.Missing",
            );
            assert_eq!(
                verified_foreign_notification_identity_from(tmp.path(), 406, &entries),
                None,
                "an uninstalled Flatpak id is never accepted as a policy key"
            );
        }
    }

    // D-Bus round-trip over an in-process p2p connection, mirroring
    // `tests/portal_test.rs`'s hermetic fixture: a `UnixStream::pair()`
    // carries a server and a raw-proxy client — no session bus or broker needed.

    #[cfg(target_os = "linux")]
    const TEST_GUID: &str = "0123456789abcdef0123456789abcdef";

    #[cfg(target_os = "linux")]
    #[test]
    fn notify_decodes_exactly_the_advertised_arguments() {
        use zbus::zvariant::Type;
        let advertised = |method| {
            format!(
                "({})",
                interface_dispatch::advertised_in_signature(NOTIFICATIONS_INTROSPECTION, method)
            )
        };
        assert_eq!(
            advertised("Notify"),
            <NotifyArgs as Type>::SIGNATURE.to_string()
        );
        assert_eq!(
            advertised("XTiloReplacePriorityContacts"),
            <(Vec<(String, String, Vec<String>)>,) as Type>::SIGNATURE.to_string()
        );
        assert_eq!(
            advertised("XTiloScheduleNotification"),
            <scheduled::ScheduleArgs as Type>::SIGNATURE.to_string()
        );
        assert_eq!(
            advertised("XTiloCancelScheduledNotification"),
            <(String,) as Type>::SIGNATURE.to_string()
        );
        for method in ["XTiloScheduledNotifications", "XTiloScheduledAlarms"] {
            assert_eq!(
                interface_dispatch::advertised_in_signature(NOTIFICATIONS_INTROSPECTION, method),
                <() as Type>::SIGNATURE.to_string()
            );
        }
        assert_eq!(
            advertised("CloseNotification"),
            <(u32,) as Type>::SIGNATURE.to_string()
        );
    }

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    async fn scheduled_alarm_history_round_trips_calendar_and_delivery_state() {
        let callers = CallerMap::new();
        callers.force_caller_for_tests(PortalCaller::new(WorkspaceId(1), "so.tilo.clocks"));
        let bridge = NotificationBridge::new();
        let (_server, client) = bring_up_pair(bridge.clone(), callers).await;
        let proxy = notifications_proxy(&client).await;
        let at = chrono::Utc::now().timestamp_millis().saturating_sub(1000);
        let calendar = (12, 0, 0, 42);
        let args: scheduled::ScheduleArgs = (
            "once".into(),
            "Alarm".into(),
            "Wake up".into(),
            "tilo-clocks:alarms".into(),
            at,
            vec![(
                calendar.0,
                calendar.1,
                calendar.2,
                calendar.3,
                String::new(),
            )],
        );
        proxy
            .call_method("XTiloScheduleNotification", &args)
            .await
            .expect("handoff");
        let states: Vec<scheduled::AlarmStateWire> = proxy
            .call_method("XTiloScheduledAlarms", &())
            .await
            .expect("history")
            .body()
            .deserialize()
            .expect("calendar wire rows");
        assert_eq!(states.len(), 1);
        let state = states.first().expect("alarm state");
        assert_eq!(state.0, "once");
        assert_eq!(state.1, calendar);
        assert!(state.2.is_empty());
        assert_eq!(state.3, vec![at]);
        assert!(!state.4.is_empty());
        assert_eq!(bridge.drain_incoming().len(), 1);
        proxy
            .call_method("XTiloScheduledAlarms", &())
            .await
            .expect("query again");
        assert!(bridge.drain_incoming().is_empty());
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn scheduled_notification_rpc_requires_identity_and_isolates_owners() {
        let (_server, client) = bring_up_pair(NotificationBridge::new(), CallerMap::new()).await;
        let proxy = notifications_proxy(&client).await;
        assert!(
            proxy
                .call_method("XTiloScheduledNotifications", &())
                .await
                .is_err(),
            "unverified sender denied"
        );
        let callers = CallerMap::new();
        callers.force_caller_for_tests(PortalCaller::new(WorkspaceId(1), "so.tilo.clocks"));
        let (_server, client) = bring_up_pair(NotificationBridge::new(), callers.clone()).await;
        let proxy = notifications_proxy(&client).await;
        let at = chrono::Utc::now()
            .timestamp_millis()
            .saturating_add(3_600_000);
        let args: scheduled::ScheduleArgs = (
            "alarm".into(),
            "Alarm".into(),
            "Wake up".into(),
            "tilo-clocks:alarms".into(),
            at,
            Vec::new(),
        );
        proxy
            .call_method("XTiloScheduleNotification", &args)
            .await
            .expect("schedule own deadline");
        let pending: Vec<(String, i64)> = proxy
            .call_method("XTiloScheduledNotifications", &())
            .await
            .expect("pending")
            .body()
            .deserialize()
            .expect("wire rows");
        assert_eq!(pending, vec![("alarm".into(), at)]);
        callers.force_caller_for_tests(PortalCaller::new(WorkspaceId(2), "so.tilo.clocks"));
        proxy
            .call_method("XTiloCancelScheduledNotification", &("alarm",))
            .await
            .expect("cancel scoped to caller");
        let pending: Vec<(String, i64)> = proxy
            .call_method("XTiloScheduledNotifications", &())
            .await
            .expect("other workspace pending")
            .body()
            .deserialize()
            .expect("wire rows");
        assert!(pending.is_empty());
        callers.force_caller_for_tests(PortalCaller::new(WorkspaceId(1), "so.tilo.clocks"));
        let pending: Vec<(String, i64)> = proxy
            .call_method("XTiloScheduledNotifications", &())
            .await
            .expect("original pending")
            .body()
            .deserialize()
            .expect("wire rows");
        assert_eq!(
            pending.len(),
            1,
            "another workspace cannot cancel this alarm"
        );
        proxy
            .call_method("XTiloCancelScheduledNotification", &("alarm",))
            .await
            .expect("cancel own deadline");
        let pending: Vec<(String, i64)> = proxy
            .call_method("XTiloScheduledNotifications", &())
            .await
            .expect("cancelled pending")
            .body()
            .deserialize()
            .expect("wire rows");
        assert!(pending.is_empty());
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    async fn dbus_server_round_trips_notifications_with_attribution() {
        // Phase `notify_round_trips_with_caller_attribution`.
        {
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
        // Phase `replaces_id_is_preserved`.
        {
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
        // Phase `close_notification_enqueues_close_request`.
        {
            let bridge = NotificationBridge::new();
            let (_server, client) = bring_up_pair(bridge.clone(), CallerMap::new()).await;
            let proxy = notifications_proxy(&client).await;

            proxy
                .call_method("CloseNotification", &(5u32))
                .await
                .expect("call CloseNotification");
            assert_eq!(bridge.drain_close_requests(), vec![5]);
        }
        // Phase `get_capabilities_and_server_information`.
        {
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
            assert_eq!(name, "Tilo Notifications");
            assert_eq!(vendor, "Tilo");
            assert_eq!(spec, "1.2");
        }
    }

    #[test]
    fn category_and_suppress_sound_hints_pick_the_system_sound() {
        use settings_store::system_sounds::SystemSoundEvent;
        let mut hints: HashMap<String, OwnedValue> = HashMap::new();
        assert_eq!(
            parse_notification_sound(&hints),
            Some(SystemSoundEvent::Notification)
        );
        hints.insert(
            "category".into(),
            OwnedValue::from(zbus::zvariant::Str::from("im.received")),
        );
        assert_eq!(
            parse_notification_sound(&hints),
            Some(SystemSoundEvent::Message)
        );
        hints.insert("suppress-sound".into(), OwnedValue::from(true));
        assert_eq!(parse_notification_sound(&hints), None);
    }
}
