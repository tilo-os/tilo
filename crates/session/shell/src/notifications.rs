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

use std::collections::{HashMap, HashSet, VecDeque};
use tilo_desktop::notifications::ContinuityNotificationId;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::portal::PortalCaller;

#[cfg(target_os = "linux")]
mod dbus;
mod flatpak;
mod hints;
/// Bus name the freedesktop notification spec reserves for the
/// notification daemon; owning it makes the compositor the system notification server.
#[cfg(target_os = "linux")]
mod scheduled;

#[cfg(target_os = "linux")]
pub use dbus::{NotificationService, ServerHandle, serve_notifications, spawn_signal_emitter};
#[cfg(target_os = "linux")]
pub(crate) use flatpak::verified_flatpak_app_id;
use flatpak::*;
pub use hints::IncomingImageData;

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
    /// The sender's own icon name, absolute path, or `image-path` hint. A
    /// lower-trust hint than a launcher-verified app id, and only ever used
    /// for pixels.
    pub app_icon: Option<String>,
    /// The freedesktop `desktop-entry` hint, also only an artwork hint.
    pub desktop_entry: Option<String>,
    /// Decoded `image-data`: straight-alpha RGBA, already bounded. Only ever
    /// pixels, never identity or policy.
    pub image_data: Option<IncomingImageData>,
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
    /// Tilo extension: the display name of the person the message is from,
    /// shown with `contact_endpoint` on the banner. Trusted no further than
    /// the handle it travels with.
    pub sender_name: Option<String>,
    /// The system sound this notification stands for, from its `category`
    /// hint; `None` when the sender asked for silence (`suppress-sound`).
    pub sound: Option<settings_store::system_sounds::SystemSoundEvent>,
    /// The sender filed it as a call ringing now (freedesktop category
    /// `call.incoming`). Trusted only together with a verified sender
    /// person ([`Self::contact_endpoint`]): a bare category is the app's
    /// word alone.
    pub incoming_call: bool,
    /// The system says something about the whole machine (critical battery):
    /// shown on every display, not only the one in use. Never from the wire.
    pub machine_wide: bool,
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
    /// The store's committed snoozes, read without its lock.
    snoozed: scheduled::SnoozedView,
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
    /// The `xdg_activation_v1` token the compositor minted for an action the
    /// person activated, sent as `ActivationToken` just before that
    /// action's `ActionInvoked`, so the app can bring its window forward.
    action_tokens: HashMap<u32, String>,
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
            .field("action_tokens", &self.action_tokens.len())
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
        let continuity_epoch = crate::random::nonzero::<16>();
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
        let snoozed = store
            .lock()
            .map(|store| store.snoozed_view())
            .unwrap_or_default();
        if let Ok(mut inner) = self.inner.lock() {
            inner.schedule_controller = Some(ScheduledActionController {
                store: Arc::downgrade(store),
                snoozed,
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
            // Called from the compositor's loop. The store's own lock is held
            // across each commit's disk write, so read the committed view.
            let snoozed = self.inner.lock().ok().and_then(|inner| {
                inner
                    .schedule_controller
                    .as_ref()
                    .filter(|controller| controller.store.strong_count() > 0)
                    .map(|controller| Arc::clone(&controller.snoozed))
            });
            snoozed
                .and_then(|snoozed| {
                    snoozed.lock().ok().map(|alarms| {
                        alarms
                            .iter()
                            .filter(|alarm| alarm.target.room == room)
                            .cloned()
                            .collect()
                    })
                })
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
        self.note_action_with_token(id, action_key, None);
    }

    /// [`Self::note_action`] for an action the person activated, with the
    /// activation token the compositor minted for it: emitted as
    /// `ActivationToken` before the `ActionInvoked`, as the notification
    /// specification (1.2) has it, so the app's window can come forward.
    pub fn note_action_with_token(
        &self,
        id: u32,
        action_key: impl Into<String>,
        activation_token: Option<String>,
    ) {
        if let Ok(mut g) = self.inner.lock() {
            let action_key = action_key.into();
            if let Some(token) = activation_token {
                g.action_tokens.insert(id, token);
            }
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

    /// signal emitter: the activation token minted for notification `id`'s
    /// activated action, once.
    pub fn take_action_token(&self, id: u32) -> Option<String> {
        self.inner
            .lock()
            .ok()
            .and_then(|mut g| g.action_tokens.remove(&id))
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
                token: crate::random::nonzero::<16>(),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use crate::portal::CallerMap;
    #[cfg(target_os = "linux")]
    use crate::portal::interface_dispatch;
    #[cfg(target_os = "linux")]
    use crate::shell::state::WorkspaceId;
    use std::path::Path;
    #[cfg(target_os = "linux")]
    use zbus::zvariant::OwnedValue;
    #[cfg(target_os = "linux")]
    use {dbus::NOTIFICATIONS_INTROSPECTION, dbus::NotifyArgs, hints::*};

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

    #[cfg(target_os = "linux")]
    #[test]
    fn image_data_hints_are_decoded_and_bounded() {
        use zbus::zvariant::{OwnedValue, Value};
        let mut hints = HashMap::new();
        hints.insert(
            "image-data".to_string(),
            OwnedValue::try_from(Value::new((
                2_i32,
                1_i32,
                8_i32,
                true,
                8_i32,
                4_i32,
                vec![255_u8, 0, 0, 255, 0, 255, 0, 255],
            )))
            .unwrap(),
        );
        let image = parse_image_data(&hints).expect("RGBA is decoded");
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.rgba, [255, 0, 0, 255, 0, 255, 0, 255]);

        // Three channels gain an opaque alpha.
        hints.insert(
            "image-data".to_string(),
            OwnedValue::try_from(Value::new((
                1_i32,
                1_i32,
                4_i32,
                false,
                8_i32,
                3_i32,
                vec![1_u8, 2, 3, 0],
            )))
            .unwrap(),
        );
        assert_eq!(
            parse_image_data(&hints).map(|image| image.rgba),
            Some(vec![1, 2, 3, 255])
        );

        // More than 8 bits per sample is not something the card can paint.
        hints.insert(
            "image-data".to_string(),
            OwnedValue::try_from(Value::new((
                1_i32,
                1_i32,
                4_i32,
                false,
                16_i32,
                3_i32,
                vec![0_u8; 32],
            )))
            .unwrap(),
        );
        assert!(parse_image_data(&hints).is_none());
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
        {
            let b = NotificationBridge::new();
            assert_eq!(b.pending_incoming_len(), 0);
            b.push_incoming(IncomingNotification {
                dbus_id: 1,
                app_name: "App".into(),
                app_icon: None,
                desktop_entry: None,
                image_data: None,
                summary: "Hi".into(),
                body: "There".into(),
                actions: vec![],
                expire_timeout: -1,
                urgency: 1,
                caller: None,
                badge_count: None,
                contact_endpoint: None,
                sender_name: None,
                sound: Some(settings_store::system_sounds::SystemSoundEvent::Notification),
                incoming_call: false,
                machine_wide: false,
            });
            assert_eq!(b.pending_incoming_len(), 1);
            let drained = b.drain_incoming();
            assert_eq!(drained.len(), 1);
            assert_eq!(drained[0].summary, "Hi");
            assert_eq!(b.pending_incoming_len(), 0);
            assert!(b.drain_incoming().is_empty());
        }
        {
            let b = NotificationBridge::new();
            b.request_close(42);
            assert_eq!(b.drain_close_requests(), vec![42]);
            assert!(b.drain_close_requests().is_empty());
        }
        {
            let b = NotificationBridge::new();
            b.note_closed(7, CloseReason::Expired);
            b.note_action(7, "default");
            assert_eq!(b.drain_closed_events(), vec![(7, CloseReason::Expired)]);
            assert_eq!(b.drain_action_events(), vec![(7, "default".to_string())]);
        }
        {
            assert_eq!(CloseReason::Expired.code(), 1);
            assert_eq!(CloseReason::Dismissed.code(), 2);
            assert_eq!(CloseReason::Closed.code(), 3);
        }
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
        {
            let hints: HashMap<String, OwnedValue> = HashMap::new();
            assert_eq!(parse_urgency(&hints), 1);
        }
        {
            let mut hints: HashMap<String, OwnedValue> = HashMap::new();
            hints.insert("urgency".to_string(), OwnedValue::from(2u8));
            assert_eq!(parse_urgency(&hints), 2);
        }
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

    #[test]
    fn sender_name_hint_is_one_trimmed_line() {
        let mut hints = HashMap::new();
        assert_eq!(parse_sender_name(&hints), None);
        hints.insert(
            "x-tilo-sender-name".into(),
            OwnedValue::from(zbus::zvariant::Str::from("  Ada  ")),
        );
        assert_eq!(parse_sender_name(&hints).as_deref(), Some("Ada"));
        hints.insert(
            "x-tilo-sender-name".into(),
            OwnedValue::from(zbus::zvariant::Str::from("Ada\nLovelace")),
        );
        assert_eq!(parse_sender_name(&hints), None);
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
        {
            let bridge = NotificationBridge::new();
            assert!(bridge.receive_continuity_projection(
                "certified-phone",
                "ada's phone",
                remote_projection(5, "Remote"),
                1,
            ));
            let events = bridge.drain_remote_continuity_events();
            let RemoteContinuityEvent::Upsert(notification) = &events[0] else {
                panic!("expected remote upsert");
            };
            assert_eq!(notification.source_device, "certified-phone");
            assert_eq!(notification.source_label, "ada's phone");
            assert!(!notification.is_projectable());
        }
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
