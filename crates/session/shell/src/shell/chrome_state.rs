//! Shared chrome state for the tilo shell root.
//!
//! Chrome widgets sit at sibling positions in the widget tree and mutate
//! the same fields, so this packages that state into one
//! `Rc<RefCell<_>>` handle each can hold without routing through the
//! root. Kept out of the automerge doc deliberately: overlay visibility
//! is local UI state that must not sync to peers.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use parking_lot::{RwLockReadGuard, RwLockWriteGuard};
use ui::record::RecordStore;

use crate::notifications::{CloseReason, NotificationBridge};
use crate::shell::notification::{
    MAX_HISTORY, MAX_NOTIFICATIONS, Notification, NotificationIconKind, NotificationSeverity,
};
use crate::shell::quick_settings::{BtDevice, PowerProfile, QsLevel, WifiNetwork};
use crate::shell::state::{
    AppId, Capability, GroupId, StripId, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
};
pub use crate::shell::workspace_settings::{GrantDraft, WsTab};

fn visual_notification_alerts_enabled() -> bool {
    settings_store::load_section("accessibility")
        .get("visual_alerts")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn notification_unix_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

/// One capability request waiting for the user to answer it. Dropping
/// without answering drops the `reply` sender, which the portal side
/// reads as deny — refusal, not a hang.
pub struct PendingPrompt {
    pub app_id: AppId,
    pub capability: Capability,
    /// Reply channel to a blocked portal request; `None` for
    /// shell-originated prompts nobody is waiting on.
    pub reply: Option<tokio::sync::oneshot::Sender<crate::portal::PromptOutcome>>,
}

impl PendingPrompt {
    /// The shell asking on its own behalf; nobody is blocked on this.
    pub fn new(app_id: AppId, capability: Capability) -> Self {
        Self {
            app_id,
            capability,
            reply: None,
        }
    }

    pub fn with_reply(
        app_id: AppId,
        capability: Capability,
        reply: tokio::sync::oneshot::Sender<crate::portal::PromptOutcome>,
    ) -> Self {
        Self {
            app_id,
            capability,
            reply: Some(reply),
        }
    }

    pub fn answer(&mut self, outcome: crate::portal::PromptOutcome) {
        if let Some(tx) = self.reply.take() {
            let _ = tx.send(outcome);
        }
    }
}

impl std::fmt::Debug for PendingPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingPrompt")
            .field("app_id", &self.app_id)
            .field("capability", &self.capability)
            .field("awaiting_portal_reply", &self.reply.is_some())
            .finish()
    }
}

/// Snapshot of which shell overlays are currently on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent, non-exclusive overlay visibility toggle (any subset can be on screen at once), and the struct is read/written across ~170 call sites in 14 files under crates/session/shell — a flags-enum refactor would ripple invasively for no behavioural gain"
)]
pub struct OverlayFlags {
    /// Discoverable desktop interaction guide (Super+/ or F1).
    pub shortcut_help: bool,
    /// The full first-use desktop tour, reopened from global search.
    pub desktop_tour: bool,
    pub overview: bool,
    pub quick_settings: bool,
    pub ws_settings: bool,
    /// Pure UI state — the peer roster itself lives on
    /// `WorkspaceTree::collaborators`.
    pub collaborators: bool,
    /// Pure UI state — the history shown is derived from
    /// `WorkspaceTree::doc`, which is shared and replicated.
    pub room_activity: bool,
    /// Device-local wireless display picker (Win+P).
    pub projection: bool,
    /// Screenshot / recording / arbitrary-region `PiP` selector
    /// (`Super+Shift+S`, or direct `PiP` via `Super+Shift+P`).
    pub capture: bool,
    /// Compact, non-activating timer/stopwatch controller requested by Clocks.
    pub clock: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "this is a backwards-compatible JSON snapshot produced by the Clocks app; active, stopwatch, paused, and overlay are independent wire fields, and replacing them with enums would change the external schema"
)]
pub struct ClockActivity {
    #[serde(default)]
    pub version: u8,
    #[serde(default = "clock_activity_active_default")]
    pub active: bool,
    /// UUID of the exact timer represented by this snapshot. Older Clocks
    /// builds omit it; the singleton stopwatch intentionally has no ID.
    #[serde(default)]
    pub activity_id: Option<String>,
    pub label: String,
    pub remaining_secs: u64,
    pub stopwatch: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub overlay: bool,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub producer_pid: u32,
    #[serde(default)]
    pub notification_id: Option<String>,
    #[serde(default)]
    pub notification_title: String,
    #[serde(default)]
    pub notification_body: String,
}

fn clock_activity_active_default() -> bool {
    true
}

/// Device-local reduction of the active workspace's collaborative history.
/// The history document remains authoritative; this is only the small piece
/// of presentation state needed by passive chrome.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityGlance {
    pub headline: Option<String>,
    pub unread: usize,
    /// Changes whenever a new latest entry is observed, even when the text is
    /// coincidentally identical. The top bar uses it to restart its brief
    /// transient reveal without stealing the user's selected center widget.
    pub pulse: u64,
    pub focus: Option<ActivityFocus>,
}

#[derive(Debug)]
struct PendingNotificationHistoryClear {
    notifications: Vec<Notification>,
    deadline: std::time::Instant,
}

const NOTIFICATION_CLEAR_UNDO_WINDOW: std::time::Duration = std::time::Duration::from_secs(8);

fn retain_newest_history(history: &mut Vec<Notification>) {
    let excess = history.len().saturating_sub(MAX_HISTORY);
    if excess != 0 {
        history.drain(..excess);
    }
}

fn notification_clear_deadline(now: std::time::Instant) -> std::time::Instant {
    now.checked_add(NOTIFICATION_CLEAR_UNDO_WINDOW)
        .unwrap_or(now)
}

/// Intent-addressable destination inside History. Kept device-local; links
/// from chrome/search/agents all resolve through the same shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityFocus {
    pub author: Option<String>,
    pub group_id: Option<String>,
    pub entry_id: Option<[u8; 16]>,
    /// Stable record/resource identity. Paths are deliberately excluded:
    /// rename and move must not break a Files → Activity hand-off.
    pub resource_id: Option<String>,
    /// App-native immutable version identity, retained for detail renderers.
    pub resource_version: Option<[u8; 16]>,
}

/// Seeded by which chord opened the palette: `Ctrl+K` →
/// [`SearchScope::ActiveApp`]; `Super+K` / `Super+Space` / `Alt+Space` →
/// [`SearchScope::All`] (`Alt+Space` because a nested Wayland client can't
/// claim Super from the host DE). Degrades rather than empties: no
/// provider claiming the focused tab widens `ActiveApp` to every
/// provider; nothing focused widens the palette to `All`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchScope {
    ActiveApp,
    #[default]
    Workspace,
    All,
}

impl SearchScope {
    /// `ActiveApp` renders the tab's title instead of this static
    /// string. Must not share a word with the kind-filter chips —
    /// those name result sorts, this names places.
    pub fn label(self) -> &'static str {
        match self {
            SearchScope::ActiveApp => "This app",
            SearchScope::Workspace => "Workspace",
            SearchScope::All => "Everywhere",
        }
    }

    /// The pill renders a narrowing scope with an explicit `Only …`
    /// prefix so an empty result list is attributable to the scope,
    /// not the query.
    pub fn narrows_results(self) -> bool {
        matches!(self, SearchScope::ActiveApp)
    }
}

/// Normally decided by
/// [`crate::shell::placement::placement_for_new_window`].
/// [`OpenTarget::Here`] is the explicit override the tab strip's "+"
/// sets — one-shot, cleared on palette close, so a later launch is
/// never silently aimed at a stale group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenTarget {
    #[default]
    Beside,
    /// Force the new window into `GroupId` as a tab.
    Here(u64),
}

impl OpenTarget {
    /// Appended to launchable palette rows so a result says where it
    /// will land before the user commits.
    pub fn result_hint(self) -> &'static str {
        match self {
            OpenTarget::Beside => "Opens beside",
            OpenTarget::Here(_) => "Opens here, as a tab",
        }
    }
}

/// Local UI state; does not sync to peers — device settings like
/// brightness, volume, and radio toggles are always per-device.
#[derive(Clone, Debug, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each device-local quick-settings flag maps to a distinct hardware truth or user control and must remain independently renderable"
)]
pub struct QuickSettingsState {
    /// 0.0..=1.0.
    pub brightness: f32,
    /// False after the device probe confirms there is no controllable
    /// backlight. The slider remains visible but honestly disabled.
    pub brightness_available: bool,
    /// 0.0..=1.0.
    pub volume: f32,
    /// False after the audio service cannot provide a sink reading.
    pub volume_available: bool,
    pub wifi: bool,
    pub bluetooth: bool,
    /// `true` only when `BlueZ` reported a physical adapter. Keeping this
    /// separate from `bluetooth` prevents an unavailable controller from
    /// looking like a permanently-off switch.
    pub bluetooth_available: bool,
    /// Primary `NetworkManager` Ethernet interface. `None` means no adapter.
    pub wired: Option<os_actions::WiredInfo>,
    /// Read-only kernel battery snapshot for the quick status row.
    pub battery: os_actions::BatteryInfo,
    /// `None` when fwupd is unavailable, otherwise the offered update count.
    pub firmware_updates: Option<usize>,
    pub radios: os_actions::RadioState,
    pub vpns: Vec<os_actions::VpnConnectionInfo>,
    pub privacy: os_actions::PrivacyStatus,
    pub night_light: bool,
    /// The renderer dispatches on `nav_stack.last()` (always at least
    /// `Root`) — pushing `Wifi`/`Bluetooth` flips the popover to the
    /// drill-in list.
    pub nav_stack: Vec<QsLevel>,
    pub wifi_networks: Vec<WifiNetwork>,
    /// True only while NetworkManager discovery is outstanding.
    pub wifi_scanning: bool,
    /// Last discovery failure, kept separate from an honest empty result.
    pub wifi_scan_error: Option<String>,
    /// SSID currently being joined off-thread. Kept separate from the
    /// password prompt so open networks can show progress without presenting
    /// an irrelevant secret field.
    pub wifi_joining: Option<String>,
    pub bt_devices: Vec<BtDevice>,
    /// The passphrase prompt for a protected network; `None` except
    /// while the user is typing into it.
    pub wifi_password: Option<crate::shell::quick_settings::WifiPasswordPrompt>,
    /// Mirrored from `powerprofilesctl` when the panel opens and
    /// pushed back via `set_power_profile` on cycle. Defaults to
    /// `Balanced` so a host without `power-profiles-daemon` still
    /// renders a sensible label.
    pub power_profile: PowerProfile,
}

/// One compositor-authoritative screen exposure projected into trusted
/// chrome. This is session-local: it must never be persisted or replicated,
/// because a stale viewer is worse than no disclosure at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ActiveExposure {
    pub peer: u64,
    pub surface: u64,
    pub can_control: bool,
}

/// Session-local drill-down into one app-published resource directory.
/// Route identity is retained with the resource so a similarly named object
/// in another room or app can never receive the child query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResourceScope {
    pub peer_id: u64,
    pub cluster_id: String,
    pub app_id: String,
    pub resource: String,
    pub label: String,
}

/// Session-local presence changes requested through the semantic action
/// surface. The overlay host owns the live publisher and follow viewport, so
/// chrome queues these rather than pretending the action provider can mutate
/// either subsystem directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresenceRequest {
    Follow(String),
    StopFollowing,
    SetManualStatus(Option<String>),
    SetStatusSharing(ui::StatusSharing),
}

impl Default for QuickSettingsState {
    fn default() -> Self {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("Quick Settings production defaults");
        Self {
            brightness: 0.6,
            brightness_available: true,
            volume: 0.4,
            volume_available: true,
            wifi: true,
            bluetooth: false,
            bluetooth_available: false,
            wired: None,
            battery: os_actions::BatteryInfo::from_root(std::path::Path::new(
                "/nonexistent/tilo-power-supply",
            )),
            firmware_updates: None,
            radios: os_actions::RadioState::default(),
            vpns: Vec::new(),
            privacy: os_actions::get_privacy_status(),
            night_light: settings_store::bool_at(&settings_store::load(), "display", "night_light")
                .unwrap_or(false),
            nav_stack: vec![QsLevel::Root],
            // Populated by nmcli/bluetoothctl on the popover's rising
            // edge — no fake content.
            wifi_networks: Vec::new(),
            wifi_scanning: false,
            wifi_scan_error: None,
            wifi_joining: None,
            bt_devices: Vec::new(),
            wifi_password: None,
            power_profile: PowerProfile::Balanced,
        }
    }
}

impl QuickSettingsState {
    pub(crate) fn fixture() -> Self {
        Self {
            brightness: 0.6,
            brightness_available: true,
            volume: 0.4,
            volume_available: true,
            wifi: true,
            bluetooth: false,
            bluetooth_available: false,
            wired: None,
            battery: os_actions::BatteryInfo {
                present: false,
                capacity: 0,
                status: String::new(),
                cycle_count: None,
                on_ac: false,
                full_capacity: None,
                design_capacity: None,
                charge_limit: None,
            },
            firmware_updates: None,
            radios: os_actions::RadioState::default(),
            vpns: Vec::new(),
            privacy: os_actions::PrivacyStatus::default(),
            night_light: false,
            nav_stack: vec![QsLevel::Root],
            // Populated by nmcli/bluetoothctl on the popover's rising
            // edge — no fake content.
            wifi_networks: Vec::new(),
            wifi_scanning: false,
            wifi_scan_error: None,
            wifi_joining: None,
            bt_devices: Vec::new(),
            wifi_password: None,
            power_profile: PowerProfile::Balanced,
        }
    }

    /// The renderer's dispatch key. Defaults to `Root` if the stack is
    /// somehow empty (`new()` always seeds it).
    pub fn current_level(&self) -> &QsLevel {
        self.nav_stack.last().unwrap_or(&QsLevel::Root)
    }

    pub fn push_level(&mut self, level: QsLevel) {
        self.nav_stack.push(level);
    }

    /// Replace stale drill-in history with one authoritative destination.
    pub fn show_level(&mut self, level: QsLevel) {
        self.nav_stack.clear();
        self.nav_stack.push(QsLevel::Root);
        if level != QsLevel::Root {
            self.nav_stack.push(level);
        }
    }

    /// No-op when only `Root` remains so the popover can't be emptied.
    pub fn pop_level(&mut self) {
        if self.nav_stack.len() > 1 {
            self.nav_stack.pop();
        }
    }
}

/// Mutable chrome state shared across the shell widget tree.
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "chrome fields represent independently focusable overlays and transient controls; combining them would obscure widget lifetime boundaries"
)]
pub struct ChromeState {
    identity_source: IdentitySource,
    pub(crate) hosted_inputs: super::hosted_inputs::HostedTileInputs,
    /// Shared with the portal (grants) and peer-sync (replica), all
    /// holding clones of the same `Arc<RwLock<_>>`. Named `tree_handle`
    /// (not `tree`) so unmigrated call-sites get a type-mismatch error
    /// instead of silently borrowing the wrong thing; use
    /// [`Self::tree`] / [`Self::tree_mut`].
    pub tree_handle: WorkspaceTreeHandle,
    pub overlays: OverlayFlags,
    pub quick_settings: QuickSettingsState,
    /// `Some` while the workspace lens's Name input is in
    /// text-entry mode — the edit buffer, which may diverge from the
    /// persisted name until Enter commits it.
    pub editing_workspace_name: Option<String>,
    /// Caret + selection inside `editing_workspace_name`.
    pub editing_workspace_name_selection: ui::text_selection::Selection,
    pub search_scope: SearchScope,
    pub search_resource_scope: Option<SearchResourceScope>,
    /// Set by the tab strip's "+", cleared when the palette closes.
    pub open_target: OpenTarget,
    /// Copied here once per frame from
    /// [`crate::shell::status::StatusBridge`] with no lock held across
    /// a draw. `None` until a poller publishes.
    pub status: crate::shell::status::SystemStatus,
    /// Session-local projection from Clocks; schedules never enter chrome.
    pub clock_activity: Option<ClockActivity>,
    /// At most one — an OSD counts a value up, so eight volume presses
    /// show one slab, not eight banners. See [`crate::shell::osd`].
    pub osd: Option<crate::shell::osd::Osd>,
    pub ws_settings_tab: WsTab,
    /// Window-logical bounds of the top-bar workspace pill. Published by the
    /// top bar without bumping revision so the workspace lens can animate
    /// from the exact context that opened it.
    pub workspace_settings_anchor: vello::kurbo::Rect,
    /// The Permissions tab's in-progress grant. Lives here (not the
    /// overlay host) so it survives a re-render, and holds picker
    /// indices rather than a resolved capability so a subject leaving
    /// the workspace can't leave a dangling reference.
    pub ws_grant_draft: crate::shell::workspace_settings::GrantDraft,
    /// Bumped on every tree mutation. Widgets poll this in `update` to
    /// re-sync caches — cheaper than diffing, simpler than an event bus.
    pub revision: u64,
    /// Last non-UI tree mutation observed by the compositor tick.
    observed_external_tree_revision: u64,
    /// Passive, private history awareness for the top bar. Never serialized
    /// into or replicated with [`WorkspaceTree`].
    pub activity_glance: ActivityGlance,
    activity_focus_request: Option<ActivityFocus>,
    /// The portal D-Bus backend's prompt queue drains onto the back
    /// every frame; the overlay host renders `front()` and drains it
    /// on click. Entries carry the portal's reply channel when a D-Bus
    /// caller is blocked — see [`PendingPrompt`].
    pub pending_grants: VecDeque<PendingPrompt>,
    /// Test-only override for `is_active_workspace_owner`. `None` is
    /// production behaviour (always owner, no logged-in-user identity
    /// yet); `Some(false)` exercises the disabled-Revoke path.
    pub test_only_owner_override: Option<bool>,
    /// Seeded at boot from the device identity.
    pub local_user_id: crate::shell::state::UserId,
    /// Oldest-first, capped at [`MAX_NOTIFICATIONS`] (FIFO drop).
    pub notifications: Vec<Notification>,
    /// `None` until the compositor wires it up; tests and offscreen
    /// `shots` run without it.
    notification_bridge: Option<NotificationBridge>,
    /// Oldest-first, bounded to [`MAX_HISTORY`]. Every notification
    /// that leaves [`Self::notifications`] lands here, as does
    /// anything [`Self::dnd`] suppressed.
    notification_history: Vec<Notification>,
    /// History removed by the most recent Clear all action. Kept only long
    /// enough to make that destructive-looking action safely reversible.
    pending_notification_history_clear: Option<PendingNotificationHistoryClear>,
    /// Exact unread counts accepted only from launcher-verified senders.
    /// The top-bar tray consumes the aggregate; a future dock can consume the
    /// same per-package map without inventing another badge protocol.
    notification_badges: BTreeMap<String, u32>,
    notification_sound: Option<media::notification_sound::NotificationSoundPlayer>,
    priority_contacts: crate::priority_contacts::PriorityContactRegistry,
    notification_policy: settings_store::notification_policy::NotificationPolicy,
    notification_policy_checked: Option<std::time::Instant>,
    /// While `true`, fresh notifications go straight to
    /// [`Self::notification_history`] instead of showing as banners.
    /// Independent of [`Self::session_locked`], which suppresses
    /// lock-screen *content* but still shows a count.
    dnd: bool,
    /// Short, bounded edge flash requested by the visual-alert accessibility
    /// setting. Local session state only; it is never synced or written back.
    visual_alert_until: Option<std::time::Instant>,
    /// Mirrored from the compositor's `SessionManager` each frame.
    /// While `true`, the notification stack withholds app
    /// names/summaries/bodies and shows only a count.
    session_locked: bool,
    /// Live viewers/controllers copied from the compositor's share registry
    /// each frame. Trusted chrome and accessibility read this same snapshot.
    active_exposures: Vec<ActiveExposure>,
    /// Edge-triggered request from the shell's power menu.  The native
    /// compositor drains this and is the only layer allowed to transition
    /// the session lock state, so a UI action cannot diverge from the lock
    /// screen, client-focus and input-capture lifecycle.
    session_lock_requested: bool,
    /// Id of the group full-screened via `xdg_toplevel.set_fullscreen`.
    /// NOT persisted — the compositor sets it from live state each
    /// frame via [`Self::set_fullscreen_group`].
    fullscreen_group: Option<u64>,
    /// Id of the group put full screen with the tab's Expand chip. Kept
    /// separate from [`Self::fullscreen_group`], which the compositor
    /// rewrites every frame — a value parked there by Expand would be
    /// wiped on the next tick. Once the retained row exists, both can feed
    /// [`Self::is_fullscreen`] through `fullscreen_presentation`.
    maximized_group: Option<u64>,
    /// Whether the retained widget tree currently owns the requested
    /// fullscreen/maximized group. The raw request may arrive before its row;
    /// chrome must remain visible until the row is present.
    fullscreen_presentation: bool,
    /// Whether the palette should offer invite/connect commands.
    /// Mirrored from the overlay host's own flag.
    pub peer_sync_enabled: bool,
    /// `None` is working; `Some(reason)` means every app runs alone —
    /// the one collaboration failure an app can't report for itself.
    pub collab_unavailable: Option<String>,
    /// The documents open tabs are showing. Held here because chrome
    /// is what everything outside the widget tree already reaches
    /// through. Starts in-memory so tests write nothing to disk.
    records: RecordStore,
    /// Drained once per frame by `ShellOverlayHost::pump_peer_sync`.
    peer_sync_requests: Vec<crate::shell::overlay_host::PeerSyncAction>,
    presence_requests: Vec<PresenceRequest>,
    /// An exported action asked into the current room, as `(record
    /// id, record kind)`. Queued because the room's index lives on
    /// the overlay host, unreachable from an action's `apply`.
    record_requests: Vec<(String, String)>,
    /// Foreign surfaces whose tab close affordance was activated. The native
    /// compositor drains this edge and sends `xdg_toplevel.close`; keeping it
    /// here lets the chrome remove its durable tab immediately without owning
    /// Wayland protocol handles.
    close_surface_requests: Vec<u64>,
    /// Docked at the strip's left edge by default; `Some(point)`
    /// detaches the wheel to the pointer. Cleared on Escape or
    /// picking a workspace.
    wheel_summon: Option<vello::kurbo::Point>,
    /// Last layout pass, so placement can ask "which border can the
    /// user actually see" without reaching into the widget tree.
    strip_viewport: Option<crate::shell::placement::StripViewport>,
    /// Logical desktop bounds from the overlay host's latest layout pass.
    viewport_size: Option<(f64, f64)>,
}

#[derive(Debug)]
enum IdentitySource {
    SessionRegistry,
    Explicit(String),
}

impl IdentitySource {
    fn local_user_for_room(&self, room: &crate::shell::state::RoomId) -> String {
        match self {
            Self::SessionRegistry => crate::shell::state::local_user_id_for_room(room),
            Self::Explicit(principal) => principal.clone(),
        }
    }
}

impl ChromeState {
    /// Wraps an owned [`WorkspaceTree`] in a fresh handle. Production
    /// callers sharing the handle with portal/peer-sync use
    /// [`Self::new_with_handle`] instead.
    pub fn new(tree: WorkspaceTree) -> Rc<RefCell<Self>> {
        Self::new_with_handle(tree.shared())
    }

    /// Shares an existing [`WorkspaceTreeHandle`], so the portal's
    /// `GrantStoreHandle` and peer-sync's `WorkspaceTreeReplica` mutate
    /// the same tree as the shell chrome.
    pub fn new_with_handle(tree_handle: WorkspaceTreeHandle) -> Rc<RefCell<Self>> {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("Chrome production inputs");
        Self::from_inputs(
            tree_handle,
            super::hosted_inputs::HostedTileInputs::default(),
            QuickSettingsState::default(),
            crate::shell::state::local_user_id(),
            IdentitySource::SessionRegistry,
        )
    }

    pub(crate) fn new_with_fixture_handle(
        tree_handle: WorkspaceTreeHandle,
        inputs: super::hosted_inputs::HostedTileInputs,
    ) -> Chrome {
        Self::from_inputs(
            tree_handle,
            inputs,
            QuickSettingsState::fixture(),
            "shell-fixture".into(),
            IdentitySource::Explicit("shell-fixture".into()),
        )
    }

    fn from_inputs(
        tree_handle: WorkspaceTreeHandle,
        hosted_inputs: super::hosted_inputs::HostedTileInputs,
        quick_settings: QuickSettingsState,
        local_user_id: String,
        identity_source: IdentitySource,
    ) -> Chrome {
        Rc::new(RefCell::new(Self {
            tree_handle,
            hosted_inputs,
            identity_source,
            overlays: OverlayFlags::default(),
            quick_settings,
            editing_workspace_name: None,
            editing_workspace_name_selection: ui::text_selection::Selection::default(),
            search_scope: SearchScope::default(),
            search_resource_scope: None,
            open_target: OpenTarget::default(),
            status: crate::shell::status::SystemStatus::default(),
            clock_activity: None,
            osd: None,
            ws_settings_tab: WsTab::default(),
            workspace_settings_anchor: vello::kurbo::Rect::ZERO,
            ws_grant_draft: crate::shell::workspace_settings::GrantDraft::default(),
            revision: 0,
            observed_external_tree_revision: 0,
            activity_glance: ActivityGlance::default(),
            activity_focus_request: None,
            pending_grants: VecDeque::new(),
            test_only_owner_override: None,
            local_user_id,
            notifications: Vec::new(),
            notification_bridge: None,
            notification_history: Vec::new(),
            pending_notification_history_clear: None,
            notification_badges: BTreeMap::new(),
            notification_sound: None,
            priority_contacts: crate::priority_contacts::PriorityContactRegistry::in_memory(),
            notification_policy: settings_store::notification_policy::NotificationPolicy::default(),
            notification_policy_checked: None,
            dnd: false,
            visual_alert_until: None,
            session_locked: false,
            active_exposures: Vec::new(),
            session_lock_requested: false,
            fullscreen_group: None,
            maximized_group: None,
            fullscreen_presentation: false,
            peer_sync_enabled: false,
            collab_unavailable: None,
            peer_sync_requests: Vec::new(),
            presence_requests: Vec::new(),
            wheel_summon: None,
            record_requests: Vec::new(),
            close_surface_requests: Vec::new(),
            strip_viewport: None,
            viewport_size: None,
            records: RecordStore::in_memory(),
        }))
    }

    /// Summon the wheel to `at`, or send it back to its dock with `None`.
    pub fn set_wheel_summon(&mut self, at: Option<vello::kurbo::Point>) {
        if self.wheel_summon != at {
            self.wheel_summon = at;
            self.bump();
        }
    }

    pub fn wheel_summon(&self) -> Option<vello::kurbo::Point> {
        self.wheel_summon
    }

    /// Called once per layout pass by the overlay host, so the
    /// placement rule can read the active strip's layout.
    pub fn set_strip_viewport(&mut self, viewport: Option<crate::shell::placement::StripViewport>) {
        self.strip_viewport = viewport;
    }

    pub fn strip_viewport(&self) -> Option<&crate::shell::placement::StripViewport> {
        self.strip_viewport.as_ref()
    }

    pub fn set_viewport_size(&mut self, size: (f64, f64)) {
        self.viewport_size = Some(size);
    }

    pub fn viewport_size(&self) -> Option<(f64, f64)> {
        self.viewport_size
    }

    pub fn push_peer_sync_request(&mut self, request: crate::shell::overlay_host::PeerSyncAction) {
        self.peer_sync_requests.push(request);
    }

    pub fn take_peer_sync_requests(&mut self) -> Vec<crate::shell::overlay_host::PeerSyncAction> {
        std::mem::take(&mut self.peer_sync_requests)
    }

    pub fn push_presence_request(&mut self, request: PresenceRequest) {
        self.presence_requests.push(request);
    }

    pub fn take_presence_requests(&mut self) -> Vec<PresenceRequest> {
        std::mem::take(&mut self.presence_requests)
    }

    /// Same effect as activating that record's row in search — a
    /// reference is added to this room's index, no bytes move.
    pub fn push_record_request(&mut self, id: impl Into<String>, kind: impl Into<String>) {
        self.record_requests.push((id.into(), kind.into()));
    }

    pub fn take_record_requests(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.record_requests)
    }

    pub fn request_surface_close(&mut self, surface_id: u64) {
        if !self.close_surface_requests.contains(&surface_id) {
            self.close_surface_requests.push(surface_id);
        }
    }

    pub fn take_surface_close_requests(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.close_surface_requests)
    }

    pub fn fullscreen_group(&self) -> Option<u64> {
        self.fullscreen_group
    }

    /// Either a client-requested fullscreen or a group the user
    /// expanded with the tab's Expand chip — both suppress the top bar
    /// and sidebar wheel.
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen_presentation
    }

    pub(crate) fn set_fullscreen_presentation(&mut self, presented: bool) {
        self.fullscreen_presentation = presented;
    }

    pub fn maximized_group(&self) -> Option<u64> {
        self.maximized_group
    }

    /// Idempotent, so cheap to call every frame from the compositor's
    /// fullscreen reconcile.
    pub fn set_maximized_group(&mut self, group: Option<u64>) {
        if self.maximized_group != group {
            self.maximized_group = group;
            self.bump();
        }
    }

    pub fn set_fullscreen_group(&mut self, group: Option<u64>) {
        if self.fullscreen_group != group {
            self.fullscreen_group = group;
            self.bump();
        }
    }

    /// Cheap (~10 ns uncontended on `parking_lot::RwLock`).
    pub fn tree(&self) -> RwLockReadGuard<'_, WorkspaceTree> {
        self.tree_handle.read()
    }

    /// Caller-supplied principal, absent when identity comes from the session registry.
    pub(crate) fn explicit_local_user(&self) -> Option<&str> {
        match &self.identity_source {
            IdentitySource::Explicit(principal) => Some(principal),
            IdentitySource::SessionRegistry => None,
        }
    }

    /// Person principal this session presents inside `room`.
    pub fn local_user_for_room(
        &self,
        room: &crate::shell::state::RoomId,
    ) -> crate::shell::state::UserId {
        self.identity_source.local_user_for_room(room)
    }

    /// Person principal selected for the active workspace, or the session
    /// fallback when no workspace is active.
    pub fn active_local_user(&self) -> crate::shell::state::UserId {
        self.tree().active_workspace().map_or_else(
            || self.local_user_id.clone(),
            |workspace| self.local_user_for_room(&workspace.room),
        )
    }

    pub fn records(&self) -> &RecordStore {
        &self.records
    }

    /// Install the session's persistent record store; called before
    /// the window tree is built.
    pub fn set_records(&mut self, records: RecordStore) {
        self.records = records;
    }

    /// The guard drops at the end of the borrow scope, so concurrent
    /// readers (paint, portal lookup, peer-sync export) unblock as soon
    /// as the mutation lands.
    pub fn tree_mut(&self) -> RwLockWriteGuard<'_, WorkspaceTree> {
        self.tree_handle.write()
    }

    /// Test-only: asserts that portal mutations land on the same
    /// instance the chrome reads from.
    pub fn tree_handle(&self) -> WorkspaceTreeHandle {
        self.tree_handle.clone()
    }

    /// Whether the local person can administer the active workspace. This is
    /// the same room-scoped authority used for workspace policy controls;
    /// presentation must not offer mutations that this check would reject.
    pub fn can_rename_active_workspace(&self) -> bool {
        let user = self.active_local_user();
        let tree = self.tree();
        tree.active_workspace().is_some_and(|workspace| {
            tree.governs_room(workspace.room) && workspace.may_administer(&user)
        })
    }

    /// Empty buffers are a cancel — never overwrite the name with "". A
    /// guest can never turn an inline field into an apparent local rename.
    pub fn commit_workspace_name(&mut self) {
        if let Some(buf) = self.editing_workspace_name.take() {
            let trimmed = buf.trim();
            // A locally-created or first-boot workspace can still be
            // ownerless when the inline editor commits before the boot
            // adoption pass. Claim only locally-governed rooms here; a
            // foreign room remains closed to local mutation.
            if !trimmed.is_empty() {
                let claimant = self.local_user_id.clone();
                let mut tree = self.tree_mut();
                if let Some(active_room) = tree
                    .active_workspace()
                    .filter(|workspace| workspace.owner.is_none())
                    .map(|workspace| workspace.room)
                {
                    tree.adopt_ownerless_workspaces_with(|room| {
                        if *room == active_room {
                            claimant.clone()
                        } else {
                            self.identity_source.local_user_for_room(room)
                        }
                    });
                }
            }
            if self.can_rename_active_workspace() && !trimmed.is_empty() {
                let mut tree = self.tree_mut();
                let room = if let Some(ws) = tree.active_workspace_mut() {
                    if ws.name == trimmed {
                        None
                    } else {
                        ws.name = trimmed.to_string();
                        Some(ws.room)
                    }
                } else {
                    None
                };
                if let Some(room) = room {
                    // A workspace name is both local restart state and room
                    // metadata. Commit both authorities at the same mutation
                    // boundary so a crash/restart cannot restore the old
                    // label and a peer cannot immediately write it back.
                    tree.sync_room_doc(room);
                    tree.seed_doc_from_cache();
                    tree.flush_to_disk();
                }
            }
            self.bump();
        }
    }

    pub fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Fold non-chrome mutations of the shared tree into retained chrome's
    /// ordinary revision stream. Returns true exactly once per observed epoch.
    pub fn sync_external_tree_revision(&mut self) -> bool {
        let external = self.tree().external_revision;
        if external == self.observed_external_tree_revision {
            return false;
        }
        self.observed_external_tree_revision = external;
        self.bump();
        true
    }

    /// Publish a compact history reduction. A semantic no-op deliberately
    /// avoids bumping chrome revision, keeping the resting top bar cached.
    pub fn publish_activity_glance(
        &mut self,
        headline: Option<String>,
        unread: usize,
        pulse: u64,
        focus: Option<ActivityFocus>,
    ) {
        let next = ActivityGlance {
            headline,
            unread,
            pulse,
            focus,
        };
        if self.activity_glance != next {
            self.activity_glance = next;
            self.bump();
        }
    }

    pub fn open_room_activity(&mut self, focus: Option<ActivityFocus>) {
        self.activity_focus_request = focus;
        {
            let mut tree = self.tree_mut();
            crate::shell::tab_binding::open_workspace_activity(&mut tree);
        }
        // Kept as a request edge for compatibility with older control/state
        // clients; Activity itself is no longer a modal overlay.
        self.overlays.room_activity = false;
        self.overlays.collaborators = false;
        self.bump();
    }

    pub fn take_activity_focus_request(&mut self) -> Option<ActivityFocus> {
        self.activity_focus_request.take()
    }

    /// Reveal one authoritative workspace-settings destination. All shell
    /// entry points use this instead of independently coordinating a tab and
    /// overlay. Device Quick Settings and the workspace lens share the same
    /// top-right presentation lane, so opening either one closes the other;
    /// retaining both makes the older panel remain visibly and interactively
    /// mounted underneath the newer one. The caller owns the ordinary
    /// action-level repaint boundary.
    pub fn open_workspace_settings(&mut self, tab: WsTab) {
        self.ws_settings_tab = tab;
        self.overlays.ws_settings = true;
        self.overlays.quick_settings = false;
    }

    /// The top-bar workspace capsule is both the context indicator and the
    /// close affordance, so it alone needs toggle semantics. Contextual links
    /// always call [`Self::open_workspace_settings`] and can never close an
    /// already-open destination by accident.
    pub fn toggle_workspace_settings(&mut self) {
        let opening = !self.overlays.ws_settings;
        self.overlays.ws_settings = opening;
        if opening {
            self.overlays.quick_settings = false;
        }
        self.bump();
    }

    /// Open or close the device-controls popover from one policy point.
    /// Workspace Settings is a contextual lens in the same presentation lane
    /// and therefore cannot remain mounted beneath it.
    pub fn toggle_quick_settings(&mut self) {
        let opening = !self.overlays.quick_settings;
        self.overlays.quick_settings = opening;
        if opening {
            self.overlays.ws_settings = false;
        }
        self.bump();
    }

    /// Reveal Quick Settings without toggle semantics, for deep links such as
    /// media use and command search.
    pub fn open_quick_settings(&mut self) {
        self.open_quick_settings_level(crate::shell::quick_settings::QsLevel::Root);
    }

    /// Reveal one Quick Settings destination without retaining an unrelated
    /// prior drill-in stack.
    pub fn open_quick_settings_level(&mut self, level: crate::shell::quick_settings::QsLevel) {
        self.overlays.quick_settings = true;
        self.overlays.ws_settings = false;
        self.quick_settings.show_level(level);
        self.bump();
    }

    /// Focus is four coordinates, not one; setting all four in one place
    /// stops callers getting a subset wrong differently.
    pub fn focus_tab(&mut self, workspace: WorkspaceId, strip: StripId, group: GroupId, tab: u64) {
        {
            let mut tree = self.tree_mut();
            tree.active = workspace;
            tree.active_strip.insert(workspace, strip);
            tree.set_active_group(strip, group);
            if let Some(g) = tree
                .workspaces
                .iter_mut()
                .find(|w| w.id == workspace)
                .and_then(|w| w.strips.iter_mut().find(|s| s.id == strip))
                .and_then(|s| s.window_groups.iter_mut().find(|g| g.id == group))
            {
                g.active_tab = tab;
            }
            // Focus is part of the serialized local session view. Commit
            // the complete address together so a compositor restart cannot
            // reopen the right layout with an older window selected.
            tree.flush_to_disk();
        }
        self.bump();
    }

    /// Routes into the active workspace's first strip/group via
    /// [`super::workspace_drop::apply_workspace_drop_from_path`].
    /// Returns the new tab id, or `None` if the path has no filename.
    pub fn handle_file_drop(&mut self, path: &std::path::Path) -> Option<u64> {
        let tab_id = {
            let mut tree = self.tree_mut();
            super::workspace_drop::apply_workspace_drop_from_path(&mut tree, path)
        };
        if tab_id.is_some() {
            self.bump();
        }
        tab_id
    }

    /// Splices the file's app into the `(strip_id, group_id)` the
    /// pointer was over, then records a per-workspace filesystem grant.
    /// A type an installed app declares opens in that app instead (see
    /// [`crate::shell::doc_open`]).
    pub fn handle_file_drop_at(
        &mut self,
        path: &std::path::Path,
        strip_id: StripId,
        group_id: GroupId,
    ) -> Option<u64> {
        let tab_id = {
            let mut tree = self.tree_mut();
            super::workspace_drop::apply_workspace_drop_at_from_path(
                &mut tree, path, strip_id, group_id,
            )
        };
        if let Some(tab_id) = tab_id {
            // A dropped file is an explicit capability grant to the app
            // the user aimed at, keyed by the tab the drop created.
            self.grant_dropped_file_access(path, strip_id, group_id, tab_id);
            self.bump();
        }
        tab_id
    }

    /// Through the SAME [`GrantStoreHandle`] the portal uses, so
    /// `sandbox::profile::apply_grants` picks it up on spawn. Keyed by
    /// real app identity, not a display label. Read-only,
    /// session-scoped — same as the permission prompt's "Session" choice.
    fn grant_dropped_file_access(
        &self,
        path: &std::path::Path,
        strip_id: StripId,
        group_id: GroupId,
        tab_id: u64,
    ) {
        use crate::shell::state::{Expiry, FsMode};
        // If the tab can't be found (raced away between splice and
        // grant) skip silently rather than key a grant to a guess.
        let Some(app_id) = self.tab_app_id(strip_id, group_id, tab_id) else {
            return;
        };
        let ws_id = self.tree().active;
        let store = crate::portal::GrantStoreHandle::new(self.tree_handle());
        let caller = crate::portal::PortalCaller::new(ws_id, app_id);
        if store.filesystem_path_granted(&caller, path) {
            return;
        }
        let cap = Capability::Filesystem {
            path: path.to_path_buf(),
            mode: FsMode::Read,
            scope: Expiry::EndOfSession,
        };
        store.insert(&caller, &cap);
    }

    /// Real Wayland app-id for `Foreign`, or the built-in
    /// `AppKind::app_id` for a placeholder/stub.
    fn tab_app_id(
        &self,
        strip_id: StripId,
        group_id: GroupId,
        tab_id: u64,
    ) -> Option<crate::shell::state::AppId> {
        use crate::shell::state::TileContent;
        let tree = self.tree();
        let ws = tree.active_workspace()?;
        let strip = ws.strips.iter().find(|s| s.id == strip_id)?;
        let group = strip.window_groups.iter().find(|g| g.id == group_id)?;
        let tab = group.tabs.iter().find(|t| t.id == tab_id)?;
        let app_id = match &tab.tile {
            TileContent::WorkspaceActivity => "org.tilo.WorkspaceActivity".to_string(),
            // A peer window carries an app id in the same namespace
            // `AppKind::app_id` maps the others into.
            TileContent::Foreign { app_id, .. }
            | TileContent::PeerWindow { app_id, .. }
            | TileContent::LocalApp { app_id, .. } => app_id.clone(),
            TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
                kind.app_id()
            }
        };
        drop(tree);
        Some(app_id)
    }

    /// Called directly by the wheel-disc's "+" slot to create and
    /// switch without opening a settings modal.
    pub fn add_workspace(&mut self, name: impl Into<String>) -> crate::shell::state::WorkspaceId {
        let id = {
            let mut tree = self.tree_mut();
            let id = tree.append_empty_workspace(&name.into());
            tree.set_active(id);
            // A room with no owner has no administrators, so nobody can
            // grant anything in it, including the person who just made it.
            tree.adopt_ownerless_workspaces_with(|room| {
                self.identity_source.local_user_for_room(room)
            });
            id
        };
        self.bump();
        id
    }

    /// Read by the search-scope pill so "This app" can show the tab's
    /// title instead of the static [`SearchScope::label`].
    pub fn active_app_title(&self) -> Option<String> {
        let tab = self.active_tab_ref()?;
        let title = tab.title.trim();
        if title.is_empty() {
            None
        } else {
            Some(title.to_string())
        }
    }

    /// Read by the search registry to pick the "this app" scope
    /// provider via [`crate::shell::search::SearchProvider::owns_tab`].
    pub fn active_tab_id(&self) -> Option<crate::shell::state::TabId> {
        self.active_tab_ref().map(|t| t.id)
    }

    /// Canonical package/app id of the focused tab, used to scope dynamic
    /// providers that can represent more than one application.
    pub fn active_app_id(&self) -> Option<crate::shell::state::AppId> {
        let tree = self.tree();
        let ws_id = tree.active;
        let strip_id = tree.active_strip.get(&ws_id).copied()?;
        let group_id = tree.active_group.get(&strip_id).copied()?;
        let tab_id = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.id == ws_id)?
            .strips
            .iter()
            .find(|strip| strip.id == strip_id)?
            .window_groups
            .iter()
            .find(|group| group.id == group_id)?
            .active_tab;
        drop(tree);
        self.tab_app_id(strip_id, group_id, tab_id)
    }

    /// `None` when no workspace/strip/group is tracked yet — the honest
    /// "no focused app" state rather than falling back to `.first()`.
    fn active_tab_ref(&self) -> Option<crate::shell::state::TabRef> {
        let tree = self.tree();
        let ws_id = tree.active;
        let strip_id = tree.active_strip.get(&ws_id).copied()?;
        let group_id = tree.active_group.get(&strip_id).copied()?;
        let ws = tree.workspaces.iter().find(|w| w.id == ws_id)?;
        let strip = ws.strips.iter().find(|s| s.id == strip_id)?;
        let group = strip.window_groups.iter().find(|g| g.id == group_id)?;
        let tab = group.tabs.iter().find(|t| t.id == group.active_tab)?;
        // Opaque return type so callers can't stash a borrow into
        // `tree` past this scope — clone the fields they need instead.
        let tab_ref = crate::shell::state::TabRef {
            id: tab.id,
            title: tab.title.clone(),
        };
        drop(tree);
        Some(tab_ref)
    }

    /// Strips of the active workspace, and which one has focus.
    ///
    /// Returns `(count, focused_index)`. The focused index falls back to
    /// 0 when the tree has not stamped `active_strip` for this workspace
    /// — the ribbon has to draw something, and the first strip is what a
    /// workspace with no stamped focus behaves as.
    pub fn strip_focus(&self) -> (usize, usize) {
        let tree = self.tree();
        let Some(ws) = tree.active_workspace() else {
            return (0, 0);
        };
        let count = ws.strips.len();
        let focused = tree
            .active_strip
            .get(&ws.id)
            .and_then(|sid| ws.strips.iter().position(|s| s.id == *sid))
            .unwrap_or(0);
        drop(tree);
        (count, focused)
    }

    pub fn focus_strip_index(&mut self, index: usize) -> bool {
        let (changed, name) = {
            let mut tree = self.tree_mut();
            let Some(ws) = tree.active_workspace() else {
                return false;
            };
            let ws_id = ws.id;
            let Some(strip) = ws.strips.get(index) else {
                return false;
            };
            let sid = strip.id;
            let name = strip.name.clone();
            let prev = tree.active_strip.get(&ws_id).copied();
            if prev == Some(sid) {
                return false;
            }
            tree.active_strip.insert(ws_id, sid);
            tree.flush_to_disk();
            drop(tree);
            (true, name)
        };
        let _ = name;
        if changed {
            self.bump();
        }
        changed
    }

    /// Clamps at the ends rather than wrapping, matching workspace
    /// switching.
    pub fn switch_strip(&mut self, dir: i32) -> bool {
        let (count, focused) = self.strip_focus();
        if count == 0 {
            return false;
        }
        // Step by one strip in `dir`'s direction, saturating at both
        // ends rather than round-tripping through a signed cast.
        let next = match dir.signum() {
            -1 => focused.saturating_sub(1),
            1 => focused.saturating_add(1).min(count.saturating_sub(1)),
            _ => focused,
        };
        self.focus_strip_index(next)
    }

    pub fn switch_workspace(&mut self, dir: i32) -> bool {
        let changed = {
            let mut tree = self.tree_mut();
            match dir.signum() {
                -1 => tree.switch_up(),
                1 => tree.switch_down(),
                _ => false,
            }
        };
        if changed {
            self.bump();
        }
        changed
    }

    // ---------- Notifications ----------

    pub fn show_osd(&mut self, osd: crate::shell::osd::Osd) {
        crate::shell::osd::raise(&mut self.osd, osd);
        self.bump();
    }

    /// Returns `true` when something was cleared, so the caller can
    /// bump once rather than every frame.
    pub fn expire_osd(&mut self, now: std::time::Instant) -> bool {
        if crate::shell::osd::expire(&mut self.osd, now) {
            self.bump();
            return true;
        }
        false
    }

    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    pub fn push_notification(&mut self, title: impl Into<String>, body: impl Into<String>) -> u64 {
        self.push_notification_full(Notification::new(title, body))
    }

    /// Enforces the [`MAX_NOTIFICATIONS`] cap. When [`Self::dnd`] is
    /// set, goes straight to history instead of a banner, unless it
    /// carries [`Notification::bypass_dnd`] (shell-only; D-Bus can't set it).
    pub fn push_notification_full(&mut self, n: Notification) -> u64 {
        let id = n.id;
        if self.dnd && !n.bypass_dnd {
            self.record_history(n);
            self.bump();
            return id;
        }
        if visual_notification_alerts_enabled() {
            self.visual_alert_until =
                std::time::Instant::now().checked_add(std::time::Duration::from_millis(450));
        }
        self.notifications.push(n);
        // Cap-evicted entries retire to history rather than dropping.
        while self.notifications.len() > MAX_NOTIFICATIONS {
            let evicted = self.notifications.remove(0);
            self.record_history(evicted);
        }
        self.bump();
        id
    }

    /// Whether shell chrome should paint the accessibility edge flash this
    /// frame. No timer thread or persistent alert state is needed.
    pub fn visual_alert_active(&self, now: std::time::Instant) -> bool {
        self.visual_alert_until.is_some_and(|until| now < until)
    }

    /// Every path that removes a toast (and the DND-suppression path)
    /// funnels through here, so history is the single complete record.
    fn record_history(&mut self, n: Notification) {
        self.notification_history.push(n);
        while self.notification_history.len() > MAX_HISTORY {
            self.notification_history.remove(0);
        }
    }

    pub fn push_warning_notification(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> u64 {
        self.push_notification_full(
            Notification::new(title, body)
                .with_severity(NotificationSeverity::Warning)
                .with_icon_kind(NotificationIconKind::Generic),
        )
    }

    pub fn push_error_notification(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> u64 {
        self.push_notification_full(
            Notification::new(title, body)
                .with_severity(NotificationSeverity::Error)
                .with_icon_kind(NotificationIconKind::Generic),
        )
    }

    /// The flag stops collaborator surfaces reading an empty roster as
    /// "nobody else is here"; the notification tells the user directly.
    pub fn set_collab_unavailable(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        if self.collab_unavailable.as_deref() == Some(reason.as_str()) {
            return;
        }
        self.push_error_notification(
            "Collaboration unavailable",
            format!("Apps in this session cannot share edits. {reason}"),
        );
        self.collab_unavailable = Some(reason);
    }

    pub fn clear_collab_unavailable(&mut self) {
        self.collab_unavailable = None;
    }

    pub fn collab_available(&self) -> bool {
        self.collab_unavailable.is_none()
    }

    /// No-op if the id already expired or was never pushed. A toast
    /// from D-Bus `Notify` emits `NotificationClosed(id, 2)` (dismissed).
    pub fn dismiss_notification(&mut self, id: u64) {
        if let Some(pos) = self.notifications.iter().position(|n| n.id == id) {
            let removed = self.notifications.remove(pos);
            if let Some(command) = removed.remote_dismiss_command() {
                self.queue_remote_notification_command(command);
            } else {
                self.emit_closed(&removed, CloseReason::Dismissed);
            }
            self.record_history(removed);
            self.bump();
        }
    }

    /// The user clicked the toast body, not the X. Emits
    /// `ActionInvoked` for the default action before removing, if the
    /// toast carries one; otherwise like [`Self::dismiss_notification`].
    pub fn activate_notification(&mut self, id: u64) -> bool {
        let Some(pos) = self.notifications.iter().position(|n| n.id == id) else {
            return false;
        };
        let removed = self.notifications.remove(pos);
        if let Some(command) = removed
            .default_action_key()
            .and_then(|key| removed.remote_action_command(&key))
            .or_else(|| removed.remote_dismiss_command())
        {
            self.queue_remote_notification_command(command);
        } else if let (Some(bridge), Some(dbus_id)) =
            (self.notification_bridge.as_ref(), removed.dbus_id)
        {
            if let Some(key) = removed.default_action_key() {
                bridge.note_action(dbus_id, key);
            }
            bridge.note_closed(dbus_id, CloseReason::Dismissed);
        }
        self.record_history(removed);
        self.bump();
        true
    }

    fn queue_remote_notification_command(
        &self,
        command: crate::notifications::ContinuityCommandEgress,
    ) {
        let Some(bridge) = self.notification_bridge.as_ref() else {
            return;
        };
        let _ = bridge.request_remote_continuity_command(
            &command.target_device,
            command.id,
            command.command,
            notification_unix_ms(),
        );
    }

    /// Used by the `CloseNotification` pump path. No-op if no live
    /// toast carries that id.
    pub fn close_notification_by_dbus_id(&mut self, dbus_id: u32, reason: CloseReason) {
        if let Some(pos) = self
            .notifications
            .iter()
            .position(|n| n.dbus_id == Some(dbus_id))
        {
            let removed = self.notifications.remove(pos);
            self.emit_closed(&removed, reason);
            self.record_history(removed);
            self.bump();
        }
    }

    fn close_notification_by_dbus_id_silently(&mut self, dbus_id: u32) {
        if let Some(pos) = self
            .notifications
            .iter()
            .position(|notification| notification.dbus_id == Some(dbus_id))
        {
            let removed = self.notifications.remove(pos);
            self.record_history(removed);
            self.bump();
        }
    }

    /// Called each frame by
    /// [`crate::shell::notification::NotificationStack::update`].
    /// Expired toasts from a D-Bus `Notify` emit `NotificationClosed(id, 1)`.
    pub fn update_notification_lifecycle(&mut self, now: std::time::Instant) -> usize {
        let mut expired: Vec<Notification> = Vec::new();
        let mut kept: Vec<Notification> = Vec::with_capacity(self.notifications.len());
        for n in std::mem::take(&mut self.notifications) {
            if n.is_expired_at(now) {
                expired.push(n);
            } else {
                kept.push(n);
            }
        }
        self.notifications = kept;
        let dropped = expired.len();
        if let Some(bridge) = self.notification_bridge.as_ref() {
            for n in &expired {
                if let Some(d) = n.dbus_id {
                    bridge.note_closed(d, CloseReason::Expired);
                }
            }
        }
        for n in expired {
            self.record_history(n);
        }
        if dropped > 0 {
            self.bump();
        }
        dropped
    }

    /// Used by the lock screen to show "N notifications" without
    /// exposing content.
    pub fn notification_count(&self) -> usize {
        self.notifications.len()
    }

    // ---------- History ----------

    pub fn notification_history(&self) -> &[Notification] {
        &self.notification_history
    }

    /// Number of entries in the history log.
    pub fn notification_history_count(&self) -> usize {
        self.notification_history.len()
    }

    pub fn notification_badge_total(&self) -> u32 {
        self.notification_badges
            .iter()
            .filter(|(app_id, _)| {
                self.notification_policy
                    .per_app
                    .get(*app_id)
                    .and_then(|rule| rule.badges)
                    .unwrap_or(self.notification_policy.show_badges)
            })
            .map(|(_, count)| *count)
            .fold(0_u32, u32::saturating_add)
            .min(9_999)
    }

    pub fn notification_badge_for(&self, verified_app_id: &str) -> u32 {
        self.notification_badges
            .get(verified_app_id)
            .copied()
            .unwrap_or(0)
    }

    /// Clear the history log (the "Clear all" affordance in the
    /// notification centre). Bumps the revision so the surface re-syncs.
    pub fn clear_notification_history(&mut self) {
        self.clear_notification_history_at(std::time::Instant::now());
    }

    fn clear_notification_history_at(&mut self, now: std::time::Instant) {
        if !self.notification_history.is_empty() {
            let mut removed = std::mem::take(&mut self.notification_history);
            if let Some(pending) = self.pending_notification_history_clear.as_mut() {
                pending.notifications.append(&mut removed);
                retain_newest_history(&mut pending.notifications);
                pending.deadline = notification_clear_deadline(now);
            } else {
                self.pending_notification_history_clear = Some(PendingNotificationHistoryClear {
                    notifications: removed,
                    deadline: notification_clear_deadline(now),
                });
            }
            self.bump();
        }
    }

    /// Whether Clear all can still be undone.
    pub fn can_undo_notification_history_clear(&self) -> bool {
        self.pending_notification_history_clear.is_some()
    }

    /// Restore the cleared oldest-first snapshot ahead of notifications that
    /// arrived during the undo window, preserving exact chronological order.
    pub fn undo_notification_history_clear(&mut self) -> bool {
        let Some(mut pending) = self.pending_notification_history_clear.take() else {
            return false;
        };
        pending.notifications.append(&mut self.notification_history);
        retain_newest_history(&mut pending.notifications);
        self.notification_history = pending.notifications;
        self.bump();
        true
    }

    /// Expire the undo snapshot. Returns true only on the clearing edge.
    pub fn expire_notification_history_clear(&mut self, now: std::time::Instant) -> bool {
        let expired = self
            .pending_notification_history_clear
            .as_ref()
            .is_some_and(|pending| now >= pending.deadline);
        if expired {
            self.pending_notification_history_clear = None;
            self.bump();
        }
        expired
    }

    // ---------- Do-Not-Disturb ----------

    /// Whether Do-Not-Disturb is active. While `true`, freshly posted
    /// notifications are suppressed from the banner surface and go
    /// straight to history.
    pub fn dnd(&self) -> bool {
        self.dnd
    }

    /// No-op (no revision bump) when the value is unchanged.
    pub fn set_dnd(&mut self, on: bool) {
        if self.dnd != on {
            self.dnd = on;
            self.bump();
        }
    }

    pub fn toggle_dnd(&mut self) -> bool {
        self.dnd = !self.dnd;
        self.bump();
        self.dnd
    }

    // ---------- D-Bus notification wiring ----------

    /// Once set, the toast lifecycle emits `NotificationClosed` /
    /// `ActionInvoked` events back to apps via the bridge.
    pub fn set_notification_bridge(&mut self, bridge: NotificationBridge) {
        self.notification_bridge = Some(bridge);
    }

    pub fn set_notification_runtime(
        &mut self,
        sound: media::notification_sound::NotificationSoundPlayer,
        priority_contacts: crate::priority_contacts::PriorityContactRegistry,
    ) {
        self.notification_sound = Some(sound);
        self.priority_contacts = priority_contacts;
    }

    /// Keep notification unit tests independent of the developer account's
    /// live policy document. Production always refreshes from the shared
    /// settings authority in `pump_dbus_notifications`.
    #[cfg(test)]
    pub(crate) fn set_notification_policy_for_tests(
        &mut self,
        policy: settings_store::notification_policy::NotificationPolicy,
    ) {
        self.notification_policy = policy;
        self.notification_policy_checked = Some(std::time::Instant::now());
    }

    pub fn set_session_locked(&mut self, locked: bool) {
        if self.session_locked != locked {
            self.session_locked = locked;
            self.bump();
        }
    }

    pub fn session_locked(&self) -> bool {
        self.session_locked
    }

    /// Replace the trusted disclosure snapshot. Sorting keeps cache
    /// invalidation independent of the registry's hash iteration order.
    pub fn set_active_exposures(&mut self, mut exposures: Vec<ActiveExposure>) {
        exposures.sort_unstable();
        exposures.dedup();
        if self.active_exposures != exposures {
            self.active_exposures = exposures;
            self.bump();
        }
    }

    pub fn active_exposures(&self) -> &[ActiveExposure] {
        &self.active_exposures
    }

    /// Queue a lock requested by the in-compositor power menu.  This is an
    /// edge, rather than a second source of truth for whether the session is
    /// locked: the native loop drains it into [`SessionManager`].
    pub fn request_session_lock(&mut self) {
        self.session_lock_requested = true;
    }

    /// Consume one pending shell-originated lock request.  Kept private to
    /// the shell crate because only the compositor runtime may perform the
    /// session transition.
    pub fn take_session_lock_request(&mut self) -> bool {
        std::mem::take(&mut self.session_lock_requested)
    }

    /// Called once per frame; no-op when no bridge is installed.
    pub fn pump_dbus_notifications(&mut self) {
        use chrono::{Datelike, Timelike};
        let Some(bridge) = self.notification_bridge.clone() else {
            return;
        };
        let refresh_policy = self
            .notification_policy_checked
            .is_none_or(|checked| checked.elapsed() >= std::time::Duration::from_millis(750));
        if refresh_policy {
            let policy = settings_store::notification_policy::NotificationPolicy::load();
            if policy != self.notification_policy {
                self.notification_policy = policy;
                self.bump();
            }
            self.notification_policy_checked = Some(std::time::Instant::now());
        }
        for inc in bridge.drain_incoming() {
            let dbus_id = inc.dbus_id;
            // A fresh Notify with an existing id supersedes the prior toast.
            self.notifications.retain(|n| n.dbus_id != Some(dbus_id));
            let verified_app_id = inc.caller.as_ref().map(|caller| caller.app_id.clone());
            let sharing = bridge.take_sharing(dbus_id);
            let urgency = inc.urgency;
            let badge_count = inc.badge_count;
            let verified_priority_contact = verified_app_id.is_some()
                && inc
                    .contact_endpoint
                    .as_deref()
                    .is_some_and(|endpoint| self.priority_contacts.matches(endpoint));
            let now = chrono::Local::now();
            let effective = self.notification_policy.effective_for_context(
                verified_app_id.as_deref(),
                verified_priority_contact,
                now.timestamp(),
                u16::try_from(now.hour().saturating_mul(60).saturating_add(now.minute()))
                    .unwrap_or(u16::MAX),
                u8::try_from(now.weekday().num_days_from_monday()).unwrap_or(u8::MAX),
            );
            let mut notification = Notification::from_incoming(inc);
            notification.show_preview = effective.previews;
            notification.show_on_lock_screen = effective.lock_screen;
            if effective.cross_device
                && let (Some(app_id), Some(app_name)) =
                    (verified_app_id.clone(), notification.app_name.clone())
            {
                let now_unix_ms = notification_unix_ms();
                let lifetime_ms = u64::try_from(notification.dismiss_after.as_millis())
                    .unwrap_or(crate::notifications::MAX_CONTINUITY_NOTIFICATION_TTL_MS)
                    .clamp(1, crate::notifications::MAX_CONTINUITY_NOTIFICATION_TTL_MS);
                let _ = bridge.project_continuity_notification(
                    crate::notifications::ContinuityNotificationDraft {
                        dbus_id,
                        app_id,
                        app_name,
                        title: notification.title.clone(),
                        body: notification.body.clone(),
                        actions: notification.actions.clone(),
                        urgency,
                        sharing,
                        previews_allowed: effective.previews,
                        expires_unix_ms: now_unix_ms.saturating_add(lifetime_ms),
                    },
                    now_unix_ms,
                );
            }
            // Keep the verified producer's latest state even while badge
            // display is disabled. Visibility is a policy filter below; if we
            // discarded updates here, re-enabling badges would resurrect a
            // stale count from before the mute.
            if let (Some(app_id), Some(count)) = (verified_app_id, badge_count) {
                if count == 0 {
                    self.notification_badges.remove(&app_id);
                } else {
                    self.notification_badges.insert(app_id, count.min(9_999));
                }
            }
            if effective.sounds {
                let _ = self
                    .notification_sound
                    .as_ref()
                    .map(media::notification_sound::NotificationSoundPlayer::play);
            }
            if effective.banners {
                self.push_notification_full(notification);
            } else {
                self.record_history(notification);
                self.bump();
            }
        }
        for id in bridge.drain_close_requests() {
            self.close_notification_by_dbus_id(id, CloseReason::Closed);
        }
        for id in bridge.drain_continuity_local_closes() {
            self.close_notification_by_dbus_id_silently(id);
        }
        self.pump_remote_continuity_notifications(&bridge);
    }

    fn pump_remote_continuity_notifications(&mut self, bridge: &NotificationBridge) {
        use chrono::{Datelike, Timelike};

        let now_unix_ms = notification_unix_ms();
        bridge.expire_remote_continuity(now_unix_ms);
        for event in bridge.drain_remote_continuity_events() {
            match event {
                crate::notifications::RemoteContinuityEvent::Upsert(remote) => {
                    let app_id = remote.projection.app_id.clone();
                    let now = chrono::Local::now();
                    let effective = self.notification_policy.effective_for(
                        Some(&app_id),
                        now.timestamp(),
                        u16::try_from(now.hour().saturating_mul(60).saturating_add(now.minute()))
                            .unwrap_or(u16::MAX),
                        u8::try_from(now.weekday().num_days_from_monday()).unwrap_or(u8::MAX),
                    );
                    if !effective.cross_device {
                        continue;
                    }
                    let Some(mut notification) =
                        Notification::from_remote_continuity(remote, now_unix_ms)
                    else {
                        continue;
                    };
                    if !effective.previews {
                        notification.body.clear();
                        notification.show_preview = false;
                    }
                    if self.replace_remote_notification(&notification) {
                        continue;
                    }
                    if effective.banners {
                        self.push_notification_full(notification);
                    } else {
                        self.record_history(notification);
                        self.bump();
                    }
                }
                crate::notifications::RemoteContinuityEvent::Retire { source_device, id } => {
                    self.retire_remote_notification(&source_device, id);
                }
            }
        }
    }

    fn replace_remote_notification(&mut self, replacement: &Notification) -> bool {
        let Some(route) = replacement.remote_route.as_ref() else {
            return false;
        };
        if let Some(existing) = self
            .notifications
            .iter_mut()
            .find(|notification| notification.remote_route.as_ref() == Some(route))
        {
            let id = existing.id;
            *existing = replacement.clone();
            existing.id = id;
            self.bump();
            return true;
        }
        if let Some(existing) = self
            .notification_history
            .iter_mut()
            .find(|notification| notification.remote_route.as_ref() == Some(route))
        {
            let id = existing.id;
            *existing = replacement.clone();
            existing.id = id;
            self.bump();
            return true;
        }
        false
    }

    fn retire_remote_notification(
        &mut self,
        source_device: &str,
        id: crate::notifications::ContinuityNotificationId,
    ) {
        let matches = |notification: &Notification| {
            notification.remote_route.as_ref().is_some_and(|route| {
                route.source_device == source_device && route.notification_id == id
            })
        };
        let before = (
            self.notifications.len(),
            self.notification_history.len(),
            self.pending_notification_history_clear
                .as_ref()
                .map_or(0, |pending| pending.notifications.len()),
        );
        self.notifications
            .retain(|notification| !matches(notification));
        self.notification_history
            .retain(|notification| !matches(notification));
        if let Some(pending) = self.pending_notification_history_clear.as_mut() {
            pending
                .notifications
                .retain(|notification| !matches(notification));
        }
        let after = (
            self.notifications.len(),
            self.notification_history.len(),
            self.pending_notification_history_clear
                .as_ref()
                .map_or(0, |pending| pending.notifications.len()),
        );
        if after != before {
            self.bump();
        }
    }

    fn emit_closed(&self, removed: &Notification, reason: CloseReason) {
        if let (Some(bridge), Some(dbus_id)) = (self.notification_bridge.as_ref(), removed.dbus_id)
        {
            bridge.note_closed(dbus_id, reason);
        }
    }
}

pub type Chrome = Rc<RefCell<ChromeState>>;

#[cfg(test)]
mod panel_exclusion_tests {
    use super::*;

    #[test]
    fn workspace_and_device_settings_never_stack() {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        let mut state = chrome.borrow_mut();

        state.open_quick_settings();
        assert!(state.overlays.quick_settings);
        assert!(!state.overlays.ws_settings);

        state.open_workspace_settings(WsTab::Members);
        assert!(state.overlays.ws_settings);
        assert!(!state.overlays.quick_settings);
        assert_eq!(state.ws_settings_tab, WsTab::Members);

        state.toggle_quick_settings();
        assert!(state.overlays.quick_settings);
        assert!(!state.overlays.ws_settings);

        state.toggle_workspace_settings();
        assert!(state.overlays.ws_settings);
        assert!(!state.overlays.quick_settings);
    }

    #[test]
    fn quick_settings_deep_link_replaces_stale_navigation_history() {
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        let mut state = chrome.borrow_mut();
        state.quick_settings.push_level(QsLevel::Wifi);
        state
            .quick_settings
            .push_level(QsLevel::WifiDetails("Cafe".into()));

        state.open_quick_settings_level(QsLevel::Devices);

        assert_eq!(
            state.quick_settings.nav_stack,
            vec![QsLevel::Root, QsLevel::Devices]
        );
        assert!(state.overlays.quick_settings);
        assert!(!state.overlays.ws_settings);
    }
}

#[cfg(test)]
mod visual_alert_tests {
    use super::*;

    #[test]
    fn edge_flash_is_strictly_bounded() {
        let chrome = ChromeState::new(WorkspaceTree::default());
        let now = std::time::Instant::now();
        chrome.borrow_mut().visual_alert_until = Some(now + std::time::Duration::from_millis(450));
        assert!(chrome.borrow().visual_alert_active(now));
        assert!(
            !chrome
                .borrow()
                .visual_alert_active(now + std::time::Duration::from_millis(451))
        );
    }
}

#[cfg(test)]
mod notification_history_clear_tests {
    use super::*;
    use crate::shell::notification::Notification;

    #[test]
    fn clear_undo_restores_exact_order_before_new_arrivals() {
        let chrome = ChromeState::new(WorkspaceTree::default());
        let now = std::time::Instant::now();
        let mut state = chrome.borrow_mut();
        state.record_history(Notification::new("first", "one"));
        state.record_history(Notification::new("second", "two"));
        state.clear_notification_history_at(now);
        state.record_history(Notification::new("new", "three"));

        assert!(state.undo_notification_history_clear());
        let titles: Vec<_> = state
            .notification_history()
            .iter()
            .map(|notification| notification.title.as_str())
            .collect();
        assert_eq!(titles, ["first", "second", "new"]);
        assert!(!state.undo_notification_history_clear());
    }

    #[test]
    fn clear_snapshot_expires_at_eight_seconds() {
        let chrome = ChromeState::new(WorkspaceTree::default());
        let now = std::time::Instant::now();
        let mut state = chrome.borrow_mut();
        state.record_history(Notification::new("one", "body"));
        state.clear_notification_history_at(now);

        assert!(!state.expire_notification_history_clear(now + std::time::Duration::from_secs(7)));
        assert!(state.can_undo_notification_history_clear());
        assert!(state.expire_notification_history_clear(now + std::time::Duration::from_secs(8)));
        assert!(!state.can_undo_notification_history_clear());
        assert!(!state.undo_notification_history_clear());
    }

    #[test]
    fn second_clear_extends_one_recoverable_snapshot() {
        let chrome = ChromeState::new(WorkspaceTree::default());
        let now = std::time::Instant::now();
        let mut state = chrome.borrow_mut();
        state.record_history(Notification::new("before", "body"));
        state.clear_notification_history_at(now);
        state.record_history(Notification::new("during", "body"));
        state.clear_notification_history_at(now + std::time::Duration::from_secs(4));
        assert!(state.undo_notification_history_clear());
        let titles: Vec<_> = state
            .notification_history()
            .iter()
            .map(|notification| notification.title.as_str())
            .collect();
        assert_eq!(titles, ["before", "during"]);
    }
}

#[cfg(test)]
mod active_tab_tests {
    //! `active_app_title`/`active_tab_id` must return `None`, not
    //! `strips.first()`, when no real focus target is tracked.
    use super::*;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree,
    };
    use parking_lot::RwLock;
    use std::collections::HashMap as StdMap;
    use std::sync::Arc;

    fn tree_with(active_tab_id: u64) -> WorkspaceTree {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id: GroupId = 8;
        let strip = Strip {
            id: strip_id,
            name: "Solo".into(),
            window_groups: vec![StateGroup {
                id: group_id,
                tabs: vec![
                    Tab {
                        id: 100,
                        title: "First".into(),
                        tile: TileContent::Stub {
                            kind: AppKind::Other("Notes".into()),
                            label: "first".into(),
                        },
                    },
                    Tab {
                        id: 200,
                        title: "Second".into(),
                        tile: TileContent::Stub {
                            kind: AppKind::Other("Notes".into()),
                            label: "second".into(),
                        },
                    },
                ],
                active_tab: active_tab_id,
                width: StateWidth::Normal,
                maximized: false,
            }],
        };
        let mut ws = Workspace::new(ws_id, "Home");
        ws.strips = vec![strip];
        let mut active_strip = StdMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut active_group = StdMap::new();
        active_group.insert(strip_id, group_id);
        WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            ..WorkspaceTree::default()
        }
    }

    #[test]
    fn active_app_title_follows_group_active_tab() {
        let handle = Arc::new(RwLock::new(tree_with(200)));
        let chrome = ChromeState::new_with_handle(handle);
        let st = chrome.borrow();
        assert_eq!(st.active_app_title().as_deref(), Some("Second"));
    }

    #[test]
    fn active_tab_id_matches_the_active_tab_pointer() {
        let handle = Arc::new(RwLock::new(tree_with(100)));
        let chrome = ChromeState::new_with_handle(handle);
        let st = chrome.borrow();
        assert_eq!(st.active_tab_id(), Some(100));
    }

    #[test]
    fn focus_tab_and_strip_focus_survive_restart() {
        let dir = tempfile::tempdir().expect("state dir");
        let path = dir.path().join("workspace.json");
        let mut tree = tree_with(100);
        let workspace = tree.active;
        let first_strip = tree.workspaces[0].strips[0].id;
        let first_group = tree.workspaces[0].strips[0].window_groups[0].id;
        let second_strip = StripId::new();
        let second_group = 81;
        tree.workspaces[0].strips.push(Strip {
            id: second_strip,
            name: "Other".into(),
            window_groups: vec![StateGroup {
                id: second_group,
                tabs: vec![Tab {
                    id: 300,
                    title: "Third".into(),
                    tile: TileContent::Stub {
                        kind: AppKind::Other("Notes".into()),
                        label: "third".into(),
                    },
                }],
                active_tab: 300,
                width: StateWidth::Normal,
                maximized: false,
            }],
        });
        tree.persist_path = Some(path.clone());
        tree.flush_to_disk();
        let chrome = ChromeState::new(tree);

        chrome
            .borrow_mut()
            .focus_tab(workspace, first_strip, first_group, 200);
        let restarted = crate::shell::persist::load_tree_from(&path).expect("focus state");
        assert_eq!(restarted.active_group_for(first_strip), Some(first_group));
        assert_eq!(
            restarted.workspaces[0].strips[0].window_groups[0].active_tab,
            200
        );

        assert!(chrome.borrow_mut().focus_strip_index(1));
        let restarted = crate::shell::persist::load_tree_from(&path).expect("strip focus");
        assert_eq!(restarted.active_strip.get(&workspace), Some(&second_strip));
    }

    #[test]
    fn committed_workspace_name_survives_restart_and_replicates_to_a_room_peer() {
        let dir = tempfile::tempdir().expect("state dir");
        let path = dir.path().join("workspace.json");
        let mut tree = tree_with(100);
        let room = tree.workspaces[0].room;
        tree.persist_path = Some(path.clone());
        tree.adopt_ownerless_workspaces(&"shell-fixture".to_string());
        tree.flush_to_disk();

        // Establish a real shared-room lineage before the rename. Merely
        // assigning the same RoomId to two independently-seeded documents
        // creates concurrent `name` values; that is not how a peer joins and
        // cannot prove which later edit should win.
        let mut replica = tree_with(100);
        let replica_workspace = replica.active;
        assert!(replica.set_room(replica_workspace, room));
        let baseline = tree
            .export_room_changes_since(room, &[])
            .expect("host room baseline");
        replica
            .apply_room_changes(room, &baseline, &crate::shell::state::PeerOrigin::hub(1))
            .expect("guest joins host room");
        assert_eq!(replica.active_workspace().unwrap().name, "Home");
        let replica_heads = replica.room_heads(room);

        let chrome = ChromeState::new_with_fixture_handle(
            tree.shared(),
            crate::shell::hosted_inputs::HostedTileInputs::fixture()
                .expect("hosted fixture inputs"),
        );
        {
            let mut state = chrome.borrow_mut();
            assert!(state.can_rename_active_workspace());
            state.editing_workspace_name = Some("  Project Aurora  ".into());
            state.commit_workspace_name();
        }

        let restarted = crate::shell::persist::load_tree_from(&path).expect("renamed state");
        assert_eq!(restarted.active_workspace().unwrap().name, "Project Aurora");
        let changes = chrome
            .borrow()
            .tree_mut()
            .export_room_changes_since(room, &replica_heads)
            .expect("room rename delta");
        assert!(
            !changes.is_empty(),
            "the rename must enter the room document"
        );
        replica
            .apply_room_changes(room, &changes, &crate::shell::state::PeerOrigin::hub(1))
            .expect("room merge");
        assert_eq!(replica.workspaces[0].name, "Project Aurora");
    }

    #[test]
    fn no_active_returns_none_instead_of_first_group() {
        let handle = Arc::new(RwLock::new(WorkspaceTree::default()));
        let chrome = ChromeState::new_with_handle(handle);
        let st = chrome.borrow();
        assert_eq!(st.active_tab_id(), None);
        assert!(st.active_app_title().is_none());
    }

    #[test]
    fn without_active_strip_returns_none_not_first_strip() {
        let mut tree = tree_with(100);
        tree.active_strip.clear();
        let handle = Arc::new(RwLock::new(tree));
        let chrome = ChromeState::new_with_handle(handle);
        let st = chrome.borrow();
        assert_eq!(st.active_tab_id(), None);
    }

    /// A tree with one strip/group holding `tile`, plus the ids to
    /// drive `grant_dropped_file_access` against it.
    fn tree_with_single_tab(tile: TileContent) -> (WorkspaceTree, StripId, GroupId, u64) {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let group_id: GroupId = 8;
        let tab_id = 4242u64;
        let strip = Strip {
            id: strip_id,
            name: "Solo".into(),
            window_groups: vec![StateGroup {
                id: group_id,
                tabs: vec![Tab {
                    id: tab_id,
                    title: "Dropped".into(),
                    tile,
                }],
                active_tab: tab_id,
                width: StateWidth::Normal,
                maximized: false,
            }],
        };
        let mut ws = Workspace::new(ws_id, "Home");
        ws.strips = vec![strip];
        let mut active_strip = StdMap::new();
        active_strip.insert(ws_id, strip_id);
        let mut active_group = StdMap::new();
        active_group.insert(strip_id, group_id);
        let tree = WorkspaceTree {
            workspaces: vec![ws],
            active: ws_id,
            active_strip,
            active_group,
            ..WorkspaceTree::default()
        };
        (tree, strip_id, group_id, tab_id)
    }

    /// Gap-2 proof: when the drop target is a real adopted Wayland client,
    /// the recorded grant is keyed by that client's actual
    /// `xdg_toplevel::set_app_id` value — not a re-derived display label.
    #[test]
    fn dropped_file_grant_keyed_by_foreign_tabs_real_wayland_app_id() {
        let (tree, strip_id, group_id, tab_id) = tree_with_single_tab(TileContent::Foreign {
            app_id: "org.gimp.GIMP".into(),
            surface_id: 77,
        });
        let ws_id = tree.active;
        let handle = Arc::new(RwLock::new(tree));
        let chrome = ChromeState::new_with_handle(handle);
        let st = chrome.borrow();

        let path = std::path::Path::new("/drop/photo.xcf");
        st.grant_dropped_file_access(path, strip_id, group_id, tab_id);

        let guard = st.tree();
        let ws = guard.workspaces.iter().find(|w| w.id == ws_id).unwrap();
        let grants = ws.app_grants_snapshot();
        drop(guard);
        let caps = grants.get("org.gimp.GIMP").unwrap_or_else(|| {
            panic!(
                "grant must be keyed by the foreign client's real app-id; \
                 grants = {grants:?}"
            )
        });
        assert!(
            caps.iter().any(|c| matches!(
                c,
                Capability::Filesystem { path: p, .. } if p == path
            )),
            "the dropped path must be recorded as a Filesystem grant: {caps:?}"
        );
        assert!(
            !grants.contains_key("Foreign"),
            "grant must not be keyed by a display label: {grants:?}"
        );
    }

    /// A built-in app (no `set_app_id`) is keyed by its canonical
    /// reverse-DNS app-id, not the display label.
    #[test]
    fn dropped_file_grant_keyed_by_builtin_canonical_app_id() {
        let (tree, strip_id, group_id, tab_id) =
            tree_with_single_tab(TileContent::AppPlaceholder {
                kind: AppKind::Other("Editor".into()),
                label: "notes.ts".into(),
            });
        let ws_id = tree.active;
        let handle = Arc::new(RwLock::new(tree));
        let chrome = ChromeState::new_with_handle(handle);
        let st = chrome.borrow();

        let path = std::path::Path::new("/drop/notes.ts");
        st.grant_dropped_file_access(path, strip_id, group_id, tab_id);

        let guard = st.tree();
        let ws = guard.workspaces.iter().find(|w| w.id == ws_id).unwrap();
        let grants = ws.app_grants_snapshot();
        drop(guard);
        assert!(
            grants.contains_key("org.tilo.Editor"),
            "built-in Editor must be keyed by its canonical app-id; \
             grants = {grants:?}"
        );
        assert!(
            !grants.contains_key("Editor"),
            "the display label 'Editor' must no longer be a grant key: {grants:?}"
        );
    }

    #[test]
    fn switching_strips_moves_focus_and_clamps_at_the_ends() {
        let cell = ChromeState::new(WorkspaceTree::bootstrap_demo());
        let mut st = cell.borrow_mut();
        {
            let mut tree = st.tree_mut();
            let ws = tree.active_workspace_mut().expect("a workspace");
            let base = ws.strips.len();
            for n in 0..3u64 {
                let id = StripId(9000 + n);
                ws.strips.push(Strip::new(id, format!("Extra {n}")));
            }
            drop(tree);
            assert!(base + 3 >= 4, "fixture needs at least four strips");
        }
        st.bump();

        let (count, focused) = st.strip_focus();
        assert!(
            count >= 4,
            "fixture should hold at least four strips, saw {count}"
        );
        assert_eq!(focused, 0, "a fresh workspace focuses its first strip");

        assert!(
            st.switch_strip(1),
            "moving down a strip should change focus"
        );
        assert_eq!(st.strip_focus().1, 1);

        assert!(st.switch_strip(1));
        assert_eq!(st.strip_focus().1, 2);

        assert!(st.switch_strip(-1));
        assert_eq!(st.strip_focus().1, 1);

        // Clamps rather than wrapping, matching workspace switching.
        while st.switch_strip(-1) {}
        assert_eq!(st.strip_focus().1, 0);
        assert!(!st.switch_strip(-1), "focus wrapped past the first strip");

        while st.switch_strip(1) {}
        assert_eq!(st.strip_focus().1, count - 1);
        assert!(!st.switch_strip(1), "focus wrapped past the last strip");
    }

    #[test]
    fn focusing_a_strip_by_index_selects_that_strip() {
        let cell = ChromeState::new(WorkspaceTree::bootstrap_demo());
        let mut st = cell.borrow_mut();
        {
            let mut tree = st.tree_mut();
            let ws = tree.active_workspace_mut().expect("a workspace");
            for n in 0..3u64 {
                ws.strips
                    .push(Strip::new(StripId(9100 + n), format!("S{n}")));
            }
            drop(tree);
        }
        let (count, _) = st.strip_focus();
        for i in 0..count {
            st.focus_strip_index(i);
            assert_eq!(st.strip_focus().1, i, "focusing index {i} landed elsewhere");
        }
        // Out of range is a no-op, not a panic and not a silent clamp.
        let before = st.strip_focus().1;
        assert!(!st.focus_strip_index(count + 5));
        assert_eq!(st.strip_focus().1, before);
    }

    #[test]
    fn broker_tree_epoch_invalidates_chrome_exactly_once() {
        let cell = ChromeState::new(WorkspaceTree::default());
        let mut state = cell.borrow_mut();
        let initial = state.revision;
        assert!(!state.sync_external_tree_revision());
        state.tree_mut().external_revision = 1;
        assert!(state.sync_external_tree_revision());
        assert_eq!(state.revision, initial.wrapping_add(1));
        assert!(!state.sync_external_tree_revision());
        assert_eq!(state.revision, initial.wrapping_add(1));
    }

    #[test]
    fn active_exposure_snapshot_is_sorted_deduplicated_and_edge_triggered() {
        let cell = ChromeState::new(WorkspaceTree::default());
        let mut state = cell.borrow_mut();
        let initial = state.revision;
        let a = ActiveExposure {
            peer: 9,
            surface: 42,
            can_control: true,
        };
        let b = ActiveExposure {
            peer: 3,
            surface: 77,
            can_control: false,
        };
        state.set_active_exposures(vec![a, b, a]);
        assert_eq!(state.active_exposures(), &[b, a]);
        assert_eq!(state.revision, initial.wrapping_add(1));

        state.set_active_exposures(vec![a, b]);
        assert_eq!(
            state.revision,
            initial.wrapping_add(1),
            "an unchanged authority snapshot must not force a frame"
        );
    }
}
