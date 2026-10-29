//! The session's `org.freedesktop.Notifications` daemon.
//!
//! Apps send notifications over the freedesktop specification's D-Bus
//! interface ([`serve`], [`serve_on`]). The daemon draws nothing: what
//! arrives waits in [`Notifications`], a `Send + Sync` handle the session
//! drains each frame ([`Notifications::drain`]) into its notification log
//! and banners, and the session answers through the same handle
//! ([`Notifications::closed`], [`Notifications::action_invoked`],
//! [`Notifications::replied`]), which sends the specification's signals
//! back to the app.
//!
//! Each live notification is also published as a resource with the actions
//! a banner offers ([`tilo_service::Service`]), so a person, an
//! agent and a headless test close, invoke and answer a notification the
//! same way, and the banners read each one's data from its resource.
//!
//! A sender is attributed to an app by the session ([`Callers`]), or, for a
//! sandboxed sender, by its Flatpak runtime metadata
//! ([`verified_flatpak_app_id`]); nothing the sender says about itself is
//! believed.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;
use tokio::sync::mpsc;

mod dbus;
mod flatpak;
mod hints;
mod service;

pub use dbus::{Server, serve, serve_on};
pub use flatpak::verified_flatpak_app_id;
pub use hints::ImageData;
pub use service::{
    NOTIFICATION_KIND, NOTIFICATIONS_KIND, NOTIFICATIONS_RESOURCE, NotificationEffect,
    notification_resource,
};

/// Bus name the freedesktop notification specification reserves for the
/// notification daemon.
pub const BUS_NAME: &str = "org.freedesktop.Notifications";

/// Object path the specification fixes for the notification interface.
pub const OBJECT_PATH: &str = "/org/freedesktop/Notifications";

/// The action key of an inline reply: a sender offering it gets what the
/// person typed back as `NotificationReplied`.
pub const INLINE_REPLY: &str = "inline-reply";

/// Why a notification closed, as the specification's `NotificationClosed`
/// signal names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// 1 — the notification expired (its timeout elapsed).
    Expired,
    /// 2 — dismissed by the person.
    Dismissed,
    /// 3 — closed by a `CloseNotification` call from the app.
    Closed,
}

impl CloseReason {
    /// The on-the-wire `reason` code for the `NotificationClosed` signal.
    #[must_use]
    pub fn code(self) -> u32 {
        match self {
            Self::Expired => 1,
            Self::Dismissed => 2,
            Self::Closed => 3,
        }
    }
}

/// The app a notification was sent by, as the session or the sender's
/// sandbox proves it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Caller {
    /// The app's id.
    pub app_id: String,
    /// The workspace the app runs in, when the session knows it.
    pub workspace: Option<String>,
}

/// Who sent one call, as the bus tells it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sender<'a> {
    /// The sender's unique bus name; `None` on a peer-to-peer link.
    pub unique_name: Option<&'a str>,
    /// The sender's process, as the bus daemon reports it.
    pub pid: Option<u32>,
}

/// The session's knowledge of which app a sender is: the apps it launched.
pub trait Callers: Send + Sync {
    /// The app behind `sender`; `None` when the session cannot tell, and the
    /// notification shows unattributed.
    fn caller(&self, sender: Sender<'_>) -> Option<Caller>;
}

/// Attributes no sender: only a Flatpak's own metadata names its app.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCallers;

impl Callers for NoCallers {
    fn caller(&self, _sender: Sender<'_>) -> Option<Caller> {
        None
    }
}

/// One notification an app sent.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Notification {
    /// Its id: freshly allocated, or the one the sender replaces. A
    /// notification arriving under the id of one the session shows replaces
    /// it in place.
    pub id: u32,
    /// The sending application's own name for itself (`app_name`).
    pub app_name: String,
    /// The sender's icon name, absolute path, or `image-path` hint. A
    /// lower-trust hint than [`Self::caller`], and only ever used for
    /// pixels.
    pub app_icon: Option<String>,
    /// The `desktop-entry` hint, also only an artwork hint.
    pub desktop_entry: Option<String>,
    /// Decoded `image-data`: straight-alpha RGBA, already bounded. Only ever
    /// pixels, never identity or policy.
    #[serde(skip)]
    pub image_data: Option<ImageData>,
    /// One-line summary: the banner's title.
    pub summary: String,
    /// The banner's body; may be several lines.
    pub body: String,
    /// `(key, label)` pairs, from the specification's flat
    /// `[key1, label1, key2, label2, …]` array.
    pub actions: Vec<(String, String)>,
    /// The specification's `expire_timeout`: `-1` the server's default, `0`
    /// never, otherwise milliseconds.
    pub expire_timeout: i32,
    /// The `urgency` hint: `0` low, `1` normal, `2` critical.
    pub urgency: u8,
    /// The app it was attributed to; `None` when no proof named one.
    pub caller: Option<Caller>,
    /// Tilo extension: an exact unread count. Only from a sender the
    /// session attributed; policy decides whether the app may badge.
    pub badge_count: Option<u32>,
    /// Tilo extension: the endpoint of the person the message is from,
    /// asserted by a sender the session attributed. The session checks it
    /// against its contacts before it may affect focus policy.
    pub contact_endpoint: Option<String>,
    /// Tilo extension: the display name of the person the message is from,
    /// trusted no further than [`Self::contact_endpoint`].
    pub sender_name: Option<String>,
    /// The `category` hint (`im.received`, `device.added`): what system
    /// sound the notification stands for.
    pub category: Option<String>,
    /// The sender asked for no sound (`suppress-sound`).
    pub suppress_sound: bool,
    /// The sender filed it as a call ringing now (`call.incoming`). Trusted
    /// only together with a verified sender person
    /// ([`Self::contact_endpoint`]): a bare category is the app's word alone.
    pub incoming_call: bool,
    /// Says something about the whole machine: shown on every display.
    /// Never from the wire; only a producer inside the session sets it.
    pub machine_wide: bool,
}

impl Notification {
    /// A notification with `id` and nothing else set: what a producer
    /// inside the session fills in.
    #[must_use]
    pub fn new(id: u32) -> Self {
        Self {
            id,
            app_name: String::new(),
            app_icon: None,
            desktop_entry: None,
            image_data: None,
            summary: String::new(),
            body: String::new(),
            actions: Vec::new(),
            expire_timeout: -1,
            urgency: 1,
            caller: None,
            badge_count: None,
            contact_endpoint: None,
            sender_name: None,
            category: None,
            suppress_sound: false,
            incoming_call: false,
            machine_wide: false,
        }
    }

    /// Whether the sender takes an inline reply ([`INLINE_REPLY`]).
    #[must_use]
    pub fn takes_reply(&self) -> bool {
        self.actions.iter().any(|(key, _)| key == INLINE_REPLY)
    }
}

/// What the session takes from [`Notifications::drain`], in the order it
/// happened.
#[allow(
    clippy::large_enum_variant,
    reason = "arrivals live one frame and are mostly notifications; boxing each saves nothing"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arrival {
    /// A notification to show, or to replace the one shown under its id.
    Notify(Notification),
    /// The notification with this id is gone: the app closed it, or it was
    /// closed through its resource. The app has been told; the session only
    /// takes its banner away.
    Close(u32),
}

/// A signal on its way to the apps.
#[derive(Debug)]
enum Signal {
    Closed(u32, CloseReason),
    Action {
        id: u32,
        key: String,
        token: Option<String>,
    },
    Replied(u32, String),
}

type Wake = Arc<dyn Fn() + Send + Sync>;
type ActionTap = Arc<dyn Fn(u32, &str) + Send + Sync>;
type Mint = Arc<dyn Fn() -> Option<String> + Send + Sync>;

#[derive(Default)]
struct State {
    /// Counts every change to the live notifications.
    generation: u64,
    arrivals: Vec<Arrival>,
    /// Every notification shown and not yet closed, by id. Kept without
    /// its pixels.
    live: BTreeMap<u32, Notification>,
    /// An ongoing notification ends when its sender, by unique bus name,
    /// leaves; only that sender may replace or close it.
    ongoing_owners: HashMap<u32, String>,
    /// Where signals go while the daemon is served.
    signals: Option<mpsc::UnboundedSender<Signal>>,
    wake: Option<Wake>,
    action_taps: Vec<ActionTap>,
    mint: Option<Mint>,
}

/// The notifications apps sent, shared by the D-Bus daemon and the session.
/// Cloning shares it.
#[derive(Clone)]
pub struct Notifications {
    state: Arc<Mutex<State>>,
    /// The specification wants ids non-zero and unique for the server's
    /// life; `0` asks `Notify` for a fresh one.
    next_id: Arc<AtomicU32>,
    /// The generation this handle's [`tilo_service::Service::poll`] last
    /// saw; each clone polls for itself.
    seen: u64,
}

impl Default for Notifications {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Notifications {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock_recover();
        f.debug_struct("Notifications")
            .field("arrivals", &state.arrivals.len())
            .field("live", &state.live.keys().collect::<Vec<_>>())
            .field("served", &state.signals.is_some())
            .finish_non_exhaustive()
    }
}

impl Notifications {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::default(),
            next_id: Arc::new(AtomicU32::new(1)),
            seen: 0,
        }
    }

    /// Call `wake` whenever something arrives to drain, from whichever
    /// thread it arrived on.
    pub fn on_change(&self, wake: impl Fn() + Send + Sync + 'static) {
        self.state.lock_recover().wake = Some(Arc::new(wake));
    }

    /// Also tell `tap` of every action invoked on a notification, as it is
    /// sent: for a producer of its own (the notification portal) that
    /// routes its notifications' actions itself.
    pub fn on_action(&self, tap: impl Fn(u32, &str) + Send + Sync + 'static) {
        self.state.lock_recover().action_taps.push(Arc::new(tap));
    }

    /// Mint an activation token for each action invoked through a
    /// notification's resource, so the app may bring its window forward.
    pub fn mint_activation_tokens(
        &self,
        mint: impl Fn() -> Option<String> + Send + Sync + 'static,
    ) {
        self.state.lock_recover().mint = Some(Arc::new(mint));
    }

    /// A fresh, non-zero notification id.
    pub fn alloc_id(&self) -> u32 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if id == 0 {
            // Wrapped around: skip 0, which the specification reserves.
            self.next_id.fetch_add(1, Ordering::Relaxed)
        } else {
            id
        }
    }

    /// Show `notification`, as a producer inside the session (the
    /// notification portal) hands one over.
    pub fn push(&self, notification: Notification) {
        self.arrive(notification, None);
    }

    fn arrive(&self, notification: Notification, ongoing_owner: Option<String>) {
        let wake = {
            let mut state = self.state.lock_recover();
            state.ongoing_owners.remove(&notification.id);
            if let Some(owner) = ongoing_owner {
                state.ongoing_owners.insert(notification.id, owner);
            }
            let kept = Notification {
                image_data: None,
                ..notification.clone()
            };
            state.live.insert(notification.id, kept);
            state.generation = state.generation.wrapping_add(1);
            state.arrivals.push(Arrival::Notify(notification));
            state.wake.clone()
        };
        if let Some(wake) = wake {
            wake();
        }
    }

    /// Everything that arrived since the last drain, oldest first.
    #[must_use]
    pub fn drain(&self) -> Vec<Arrival> {
        std::mem::take(&mut self.state.lock_recover().arrivals)
    }

    /// The notifications shown and not yet closed, by id.
    #[must_use]
    pub fn live(&self) -> Vec<Notification> {
        self.state.lock_recover().live.values().cloned().collect()
    }

    /// Live notification `id`, without its pixels.
    #[must_use]
    pub fn notification(&self, id: u32) -> Option<Notification> {
        self.state.lock_recover().live.get(&id).cloned()
    }

    /// Notification `id` closed for `reason`: the session took its banner
    /// away (it expired, the person dismissed it). Sends
    /// `NotificationClosed` once; `false` when it was no longer live.
    pub fn closed(&self, id: u32, reason: CloseReason) -> bool {
        self.close(id, reason, false)
    }

    /// Close live notification `id` for `reason`, telling the app; with
    /// `arrival` the session is told to take its banner away too.
    fn close(&self, id: u32, reason: CloseReason, arrival: bool) -> bool {
        let wake = {
            let mut state = self.state.lock_recover();
            if state.live.remove(&id).is_none() {
                return false;
            }
            state.generation = state.generation.wrapping_add(1);
            state.ongoing_owners.remove(&id);
            send(&state, Signal::Closed(id, reason));
            if arrival {
                state.arrivals.push(Arrival::Close(id));
                state.wake.clone()
            } else {
                None
            }
        };
        if let Some(wake) = wake {
            wake();
        }
        true
    }

    /// The person invoked action `key` on notification `id`: sends
    /// `ActivationToken` with `activation_token` when there is one, then
    /// `ActionInvoked`. The notification may already be closed (a row in
    /// the session's history); closing a live one is the caller's
    /// [`Self::closed`] after this.
    pub fn action_invoked(&self, id: u32, key: &str, activation_token: Option<String>) {
        let (taps, signals) = {
            let state = self.state.lock_recover();
            (state.action_taps.clone(), state.signals.clone())
        };
        for tap in &taps {
            tap(id, key);
        }
        if let Some(signals) = signals {
            let _ = signals.send(Signal::Action {
                id,
                key: key.to_owned(),
                token: activation_token,
            });
        }
    }

    /// The person replied `text` to notification `id` inline: sends
    /// `NotificationReplied`. Closing it is the caller's [`Self::closed`]
    /// after this.
    pub fn replied(&self, id: u32, text: &str) {
        send(
            &self.state.lock_recover(),
            Signal::Replied(id, text.to_owned()),
        );
    }

    /// Close notification `id` for its app or producer: the app is told
    /// (`NotificationClosed` with [`CloseReason::Closed`]) and the session
    /// takes its banner away ([`Arrival::Close`]). `false` when it was not
    /// live.
    pub fn request_close(&self, id: u32) -> bool {
        self.close(id, CloseReason::Closed, true)
    }

    /// Whether `sender` may replace or close notification `id`: anyone, but
    /// for an ongoing notification only its owner.
    fn may_change(&self, id: u32, sender: Option<&str>) -> bool {
        self.state
            .lock_recover()
            .ongoing_owners
            .get(&id)
            .is_none_or(|owner| Some(owner.as_str()) == sender)
    }

    /// Unique bus name `owner` left: its ongoing notifications end.
    fn owner_left(&self, owner: &str) {
        let ids: Vec<u32> = self
            .state
            .lock_recover()
            .ongoing_owners
            .iter()
            .filter_map(|(&id, current)| (current == owner).then_some(id))
            .collect();
        for id in ids {
            self.close(id, CloseReason::Closed, true);
        }
    }

    /// The person dismissed live notification `id` outside the session's
    /// own banner (through its resource).
    fn dismiss(&self, id: u32) -> bool {
        self.close(id, CloseReason::Dismissed, true)
    }

    fn mint(&self) -> Option<String> {
        let mint = self.state.lock_recover().mint.clone();
        mint.and_then(|mint| mint())
    }

    /// Whether the live notifications changed since this handle last asked.
    fn take_changed(&mut self) -> bool {
        let generation = self.state.lock_recover().generation;
        let changed = generation != self.seen;
        self.seen = generation;
        changed
    }

    fn attach(&self, signals: mpsc::UnboundedSender<Signal>) {
        self.state.lock_recover().signals = Some(signals);
    }

    fn detach(&self) {
        self.state.lock_recover().signals = None;
    }
}

/// Send `signal` to the apps when the daemon is served; nobody could be
/// listening otherwise.
fn send(state: &State, signal: Signal) {
    if let Some(signals) = &state.signals {
        let _ = signals.send(signal);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod copy_lint_tests;
