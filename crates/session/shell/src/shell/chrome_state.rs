//! Shared chrome state for the tilo shell root.
//!
//! Chrome widgets sit at sibling positions in the widget tree and mutate
//! the same fields, so this packages that state into one
//! `Rc<RefCell<_>>` handle each can hold without routing through the
//! root. Kept out of the automerge doc deliberately: overlay visibility
//! is local UI state that must not sync to peers.

use crate::shell::state::WorkspaceAuthority as _;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;

use parking_lot::RwLockReadGuard;
use ui::record::RecordStore;

use crate::notifications::{CloseReason, NotificationBridge};
use crate::shell::notification::{remote_action_command, remote_dismiss_command};
use crate::shell::quick_settings::{BtDevice, PowerProfile, QsLevel, WifiNetwork};
use crate::shell::state::{
    AppId, Capability, GroupId, StripId, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
};
use tilo_desktop::notifications::{
    HistoryActivation, Notification, NotificationIconKind, NotificationLog, NotificationSeverity,
};
pub use tilo_desktop::workspace_settings::{GrantDraft, WorkspaceSettingsSubject, WsTab};

/// An action-owned request to initialize only this freshly minted scratch tab.
/// It is intentionally session-local; reopening a saved tab never recreates it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TabRecordCreationIdentity {
    tab: u64,
    workspace: WorkspaceId,
    room: crate::shell::state::RoomId,
    tile: Vec<u8>,
    document: Option<crate::shell::state::NativeDocumentReference>,
}
impl TabRecordCreationIdentity {
    fn capture(tree: &WorkspaceTree, tab: u64) -> Option<Self> {
        for workspace in &tree.desktop.workspaces {
            let current = workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .find(|candidate| candidate.id == tab);
            if let Some(current) = current {
                return Some(Self {
                    tab,
                    workspace: workspace.id,
                    room: workspace.room,
                    tile: serde_json::to_vec(&current.tile).ok()?,
                    document: tree.native_document_tabs.get(&tab).cloned(),
                });
            }
        }
        None
    }
}
#[derive(Debug)]
struct PendingTabRecordCreation {
    identity: TabRecordCreationIdentity,
    store: RecordStore,
}

fn visual_notification_alerts_enabled() -> bool {
    settings_store::load_section("accessibility")
        .get("visual_alerts")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
mod dropped_content_startup_tests {
    use super::ChromeState;
    use crate::shell::hosted_inputs::{HostedDocumentState, HostedTileInputs};
    use crate::shell::state::WorkspaceTree;
    use crate::shell::tab_binding::Placement;

    #[test]
    fn dropped_plain_text_is_bound_to_the_exact_hosted_editor_tab() {
        let temp = tempfile::tempdir().unwrap();
        let document = temp.path().join("carried.txt");
        std::fs::write(&document, "carried words\n").unwrap();
        let tree = WorkspaceTree::bootstrap_demo();
        let group = tree.desktop.workspaces[0].strips[0].window_groups[0].id;
        let inputs = HostedTileInputs::fixture_in(temp.path()).unwrap();
        let chrome = ChromeState::new_with_fixture_handle(tree.shared(), inputs);

        let tab = chrome
            .borrow_mut()
            .handle_file_drop_at_placement(&document, Placement::TabAt(group, 0))
            .unwrap();
        assert!(matches!(
            chrome.borrow().hosted_inputs.document(tab),
            Some(HostedDocumentState::Ready(opened))
                if opened.path == document.canonicalize().unwrap()
                    && opened.text == "carried words\n"
        ));
        let state = chrome.borrow();
        let tree = state.tree();
        let group = tree.desktop.workspaces[0].strips[0]
            .window_groups
            .iter()
            .find(|candidate| candidate.id == group)
            .unwrap();
        assert_eq!(group.tabs[0].id, tab);
        drop(tree);
        drop(state);
    }
}

fn notification_unix_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0)
}

/// One capability request waiting for the user to answer it. Dropping
/// without answering drops the `reply` sender, which the portal side
/// reads as deny — refusal, not a hang.
pub struct PendingPrompt {
    /// Session-local identity; a stale answer must never consume another request.
    pub id: uuid::Uuid,
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
            id: uuid::Uuid::new_v4(),
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
            id: uuid::Uuid::new_v4(),
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
            .field("id", &self.id)
            .field("app_id", &self.app_id)
            .field("capability", &self.capability)
            .field("awaiting_portal_reply", &self.reply.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecordRequest {
    pub(crate) request_id: u64,
    pub(crate) record_id: String,
    pub(crate) kind: String,
    pub(crate) workspace_id: WorkspaceId,
    pub(crate) room: crate::shell::state::RoomId,
    pub(crate) actor: crate::shell::state::Actor,
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
    /// Device-local display sheet (Win+P): desktop layout across attached
    /// displays, plus wireless receivers.
    pub projection: bool,
    /// Screenshot / recording / arbitrary-region `PiP` selector
    /// (`Super+Shift+S`, or direct `PiP` via `Super+Shift+P`).
    pub capture: bool,
    /// Compact, non-activating timer/stopwatch controller requested by Clocks.
    pub clock: bool,
}

/// The live activity Clocks shows outside its windows: the timer or
/// stopwatch running, and a due timer to announce once.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "active, stopwatch, paused, and overlay are independent fields of the activity Clocks shows"
)]
pub struct ClockActivity {
    #[serde(default = "clock_activity_active_default")]
    pub active: bool,
    /// The timer shown; `None` is the stopwatch.
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
    pub notification_id: Option<String>,
    #[serde(default)]
    pub notification_title: String,
    #[serde(default)]
    pub notification_body: String,
}

fn clock_activity_active_default() -> bool {
    true
}

/// The history glance, its focus and screen exposures are the shared
/// desktop's chrome vocabulary; the shell holds them as the desktop reads
/// them. Exposures are session-local and never persisted or replicated.
pub use tilo_desktop::chrome::{ActiveExposure, ActivityFocus, ActivityGlance};
pub use ui::search::system::SearchScope;

/// The placeholder line for a new tab whose document could not be created.
fn new_document_unavailable(error: &impl std::fmt::Display) -> String {
    crate::shell::i18n::t_args(
        "new-document-unavailable",
        &[("error", error.to_string().into())],
    )
}

/// The shell's localized and addressable view of a [`SearchScope`].
pub trait SearchScopeText {
    /// `ActiveApp` renders the tab's title instead of this static
    /// string. Must not share a word with the kind-filter chips —
    /// those name result sorts, this names places.
    fn localized_label(self) -> String;

    /// Stable, language-independent address of the scope segment
    /// (`palette:scope:<slug>`).
    fn slug(self) -> &'static str;
}

impl SearchScopeText for SearchScope {
    fn localized_label(self) -> String {
        match self {
            SearchScope::ActiveApp => crate::shell::i18n::t("search-scope-this-app"),
            SearchScope::Workspace => crate::shell::i18n::t("search-scope-workspace"),
            SearchScope::All => crate::shell::i18n::t("search-scope-everywhere"),
        }
    }

    fn slug(self) -> &'static str {
        match self {
            SearchScope::ActiveApp => "this-app",
            SearchScope::Workspace => "workspace",
            SearchScope::All => "everywhere",
        }
    }
}

/// Normally decided by
/// [`crate::shell::placement::placement_for`].
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
    pub fn result_hint(self) -> String {
        match self {
            OpenTarget::Beside => crate::shell::i18n::t("search-opens-beside"),
            OpenTarget::Here(_) => crate::shell::i18n::t("search-opens-here"),
        }
    }
}

/// One monitor's brightness control in Quick Settings.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayBrightness {
    /// DRM connector, e.g. `DP-2` — what the brightness service keys on.
    pub connector: String,
    /// The monitor's EDID name, e.g. `Dell U2720Q`, else its connector.
    pub name: String,
    /// 0.0..=1.0.
    pub level: f32,
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
    /// Every monitor whose brightness can be set (the laptop panel's
    /// backlight, external monitors over DDC/CI), built-in first. Each gets
    /// its own slider once there is more than one; empty falls back to the
    /// single `brightness` control.
    pub displays: Vec<DisplayBrightness>,
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
    /// The top bar's battery reading — same type, same sysfs parser — so
    /// the panel and the tray cannot disagree about the charge.
    pub battery: tilo_power_mgmt::BatterySnapshot,
    /// `None` when fwupd is unavailable, otherwise the offered update count.
    pub firmware_updates: Option<usize>,
    pub radios: os_actions::RadioState,
    pub vpns: Vec<os_actions::VpnConnectionInfo>,
    pub privacy: os_actions::PrivacyStatus,
    pub night_light: bool,
    /// The machine has an accelerometer, so its screen can follow the device
    /// and the rotation-lock tile is offered.
    pub rotation_available: bool,
    /// `display.rotation_lock`: the screen stays put when the device turns.
    pub rotation_lock: bool,
    /// The renderer dispatches on `nav_stack.last()` (always at least
    /// `Root`) — pushing `Wifi`/`Bluetooth` flips the popover to the
    /// drill-in list.
    pub nav_stack: Vec<QsLevel>,
    pub wifi_networks: Vec<WifiNetwork>,
    /// True only while `NetworkManager` discovery is outstanding.
    pub wifi_scanning: bool,
    /// A typed radio mutation is admitted but has not completed.
    pub radio_change_pending: bool,
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
    /// The profiles the power daemon offers on this machine; empty until
    /// read. The power row cycles only through these.
    pub power_profiles: Vec<PowerProfile>,
    /// Apps currently keeping the screen awake (an idle inhibitor held
    /// through Wayland, the portal or `org.freedesktop.ScreenSaver`), named
    /// as they asked. Shown on the power row so a screen that never locks
    /// has a visible reason.
    pub awake_holders: Vec<String>,
    /// Certified personal devices known to the device fabric, read when the
    /// popover opens. `None` when this session has no device fabric.
    pub personal_devices: Option<usize>,
    /// Mirror of the persisted Do Not Disturb switch for the tile.
    pub do_not_disturb: bool,
    /// Private clipboard history entries, read when the popover opens.
    pub clipboard_items: usize,
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
    /// The resource's kind, which decides the shortcuts offered on it.
    pub kind: String,
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
            brightness_available: false,
            displays: Vec::new(),
            volume: 0.4,
            volume_available: true,
            wifi: true,
            bluetooth: false,
            bluetooth_available: false,
            wired: None,
            battery: tilo_power_mgmt::BatterySnapshot::absent(),
            firmware_updates: None,
            radios: os_actions::RadioState::default(),
            vpns: Vec::new(),
            privacy: os_actions::get_privacy_status(),
            night_light: settings_store::hardware::NIGHT_LIGHT.get(),
            rotation_available: os_actions::has_accelerometer(),
            rotation_lock: settings_store::hardware::ROTATION_LOCK.get(),
            nav_stack: vec![QsLevel::Root],
            // Populated by nmcli/bluetoothctl on the popover's rising
            // edge — no fake content.
            wifi_networks: Vec::new(),
            wifi_scanning: false,
            radio_change_pending: false,
            wifi_scan_error: None,
            wifi_joining: None,
            bt_devices: Vec::new(),
            wifi_password: None,
            power_profile: PowerProfile::Balanced,
            power_profiles: Vec::new(),
            awake_holders: Vec::new(),
            personal_devices: None,
            do_not_disturb: false,
            clipboard_items: 0,
        }
    }
}

impl QuickSettingsState {
    pub(crate) fn fixture() -> Self {
        Self {
            brightness: 0.6,
            brightness_available: true,
            displays: Vec::new(),
            volume: 0.4,
            volume_available: true,
            wifi: true,
            bluetooth: false,
            bluetooth_available: false,
            wired: None,
            battery: tilo_power_mgmt::BatterySnapshot::absent(),
            firmware_updates: None,
            radios: os_actions::RadioState::default(),
            vpns: Vec::new(),
            privacy: os_actions::PrivacyStatus::default(),
            night_light: false,
            rotation_available: false,
            rotation_lock: false,
            nav_stack: vec![QsLevel::Root],
            // Populated by nmcli/bluetoothctl on the popover's rising
            // edge — no fake content.
            wifi_networks: Vec::new(),
            wifi_scanning: false,
            radio_change_pending: false,
            wifi_scan_error: None,
            wifi_joining: None,
            bt_devices: Vec::new(),
            wifi_password: None,
            power_profile: PowerProfile::Balanced,
            power_profiles: Vec::new(),
            awake_holders: Vec::new(),
            personal_devices: None,
            do_not_disturb: false,
            clipboard_items: 0,
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
    pub(crate) native_app_opening: Option<super::launcher::NativeAppOpening>,
    /// Shared with the portal (grants) and peer-sync (replica), all
    /// holding clones of the same [`WorkspaceTreeHandle`]. Named `tree_handle`
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
    /// What a tablet pad's controls do, while its mode button (or the
    /// button its settings pick) is held. See [`crate::shell::pad_map`].
    pub pad_map: Option<crate::shell::pad_map::PadMap>,
    pub ws_settings_tab: WsTab,
    /// Exact durable participant revealed in Workspace Settings.
    pub ws_settings_subject: Option<WorkspaceSettingsSubject>,
    /// Ephemeral reveal edge: repeating the same public profile open must reveal it again.
    pub ws_settings_profile_reveal_generation: u64,
    /// Window-logical bounds of the top-bar workspace pill. Published by the
    /// top bar without bumping revision so the workspace lens can animate
    /// from the exact context that opened it.
    pub workspace_settings_anchor: vello::kurbo::Rect,
    /// The Permissions tab's in-progress grant. Lives here (not the
    /// overlay host) so it survives a re-render, and holds picker
    /// indices rather than a resolved capability so a subject leaving
    /// the workspace can't leave a dangling reference.
    pub ws_grant_draft: tilo_desktop::workspace_settings::GrantDraft,
    /// Bumped on every tree mutation. Widgets poll this in `update` to
    /// re-sync caches — cheaper than diffing, simpler than an event bus.
    pub revision: u64,
    /// Which chrome this is, unique in the process. [`Self::revision`]
    /// counts from zero in every chrome, so a cache keyed on it names the
    /// chrome too.
    instance: u64,
    /// Bumped when an appearance action writes the store so the live frame
    /// applies theme and locale on the next paint without waiting for the
    /// settings-file poll.
    pub appearance_generation: u64,
    /// Last non-UI tree mutation observed by the compositor tick.
    observed_external_tree_revision: u64,
    /// Passive, private history awareness for the top bar. Never serialized
    /// into or replicated with [`WorkspaceTree`].
    pub activity_glance: ActivityGlance,
    activity_focus_request: Option<ActivityFocus>,
    workspace_view_request: Option<(WorkspaceId, tilo_desktop::activity::WorkspaceViewRequest)>,
    history_action_request: Option<String>,
    /// The portal D-Bus backend's prompt queue drains onto the back
    /// every frame; the overlay host renders `front()` and drains it
    /// on click. Entries carry the portal's reply channel when a D-Bus
    /// caller is blocked — see [`PendingPrompt`].
    pub pending_grants: VecDeque<PendingPrompt>,
    /// Test-only override for `is_active_workspace_owner`. `None` is
    /// production behaviour: ownership is computed from the active
    /// workspace's capabilities. `Some(false)` exercises the disabled-Revoke
    /// path. It only decides which controls are offered; member role changes
    /// and removals are authorized by `crate::shell::membership` whatever it
    /// says.
    pub test_only_owner_override: Option<bool>,
    /// Seeded at boot from the device identity.
    pub local_user_id: crate::shell::state::UserId,
    /// The live banners and the retained history. Every notification that
    /// leaves the banners lands in the history, as does anything
    /// [`Self::notifications_quiet`] held back.
    notification_log: NotificationLog,
    /// `None` until the compositor wires it up; tests and offscreen
    /// `shots` run without it.
    notification_bridge: Option<NotificationBridge>,
    /// Private on-disk copy of the retained history, written at most once
    /// per frame by [`Self::flush_notification_history`]. `None` in tests
    /// and offscreen renders, which must never touch a user's state.
    notification_history_store:
        Option<crate::shell::notification_history::NotificationHistoryStore>,
    /// Exact unread counts accepted only from launcher-verified senders.
    /// The top-bar tray consumes the aggregate; a future dock can consume the
    /// same per-package map without inventing another badge protocol.
    notification_badges: BTreeMap<String, u32>,
    system_sounds: Option<crate::shell::system_sounds::SystemSounds>,
    priority_contacts: crate::priority_contacts::PriorityContactRegistry,
    notification_policy: settings_store::notification_policy::NotificationPolicy,
    notification_policy_checked: Option<std::time::Instant>,
    /// True while this session projects to a wireless display. Notification
    /// contents must not reach a room full of people, so banners are held
    /// in history exactly as under Do Not Disturb.
    presenting: bool,
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
    /// Session lifecycle actions invoked through the typed `session.*`
    /// actions, waiting for the overlay host's session executor — the same
    /// one the power flyout's rows reach.
    session_action_requests: Vec<os_actions::SessionAction>,
    notification_launch_requests: Vec<crate::shell::notification::NotificationLaunch>,
    notification_action_requests:
        HashMap<u64, (Option<u64>, bool, crate::notifications::ScheduledAction)>,
    notification_action_results: HashMap<u64, Result<(), String>>,
    /// Id of the group full-screened via `xdg_toplevel.set_fullscreen`.
    /// NOT persisted — the compositor sets it from live state each
    /// frame via [`Self::set_fullscreen_group`]. Client fullscreen is a
    /// per-output overlay, so the native loop writes `None` here every
    /// tick; do not park Super+F in this slot (W07).
    fullscreen_group: Option<u64>,
    /// Super+F (and the action door) park the focused group here. Kept
    /// separate from [`Self::fullscreen_group`], which the compositor
    /// rewrites every frame — a value parked there is wiped before
    /// state-dump can observe it. Unlike [`Self::maximized_group`], this
    /// covers the output, including the wheel.
    user_fullscreen_group: Option<u64>,
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
    /// Whether the active strip alone fills the work area, the others
    /// collapsed: while agents drive this compositor, so the strip an agent
    /// works in is the whole screen it sees and clicks. NOT persisted.
    single_strip: bool,
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
    pending_tab_records: VecDeque<PendingTabRecordCreation>,
    failed_tab_records: BTreeMap<u64, (TabRecordCreationIdentity, String)>,
    /// Drained once per frame by `ShellOverlayHost::pump_peer_sync`.
    peer_sync_requests: Vec<crate::shell::overlay_host::PeerSyncAction>,
    presence_requests: Vec<PresenceRequest>,
    /// "Send…" requests for the last screenshot from the action door,
    /// drained into the capture studio by the overlay host.
    screenshot_sends: Vec<ui::widget::ScreenshotSendTarget>,
    /// Compatibility queue for callers of the original active-room API.
    legacy_record_requests: Vec<(String, String)>,
    /// Tracked record actions retain the exact authorized destination until
    /// the overlay-owned room index consumes them on a later frame.
    tracked_record_requests: Vec<RecordRequest>,
    record_request_results: BTreeMap<u64, Result<(), String>>,
    next_record_request_id: u64,
    /// Terminal results of tracked peer-sync requests: `Null` for one that
    /// only presented something, the action output for a network request.
    peer_sync_request_results: BTreeMap<u64, Result<serde_json::Value, String>>,
    next_peer_sync_request_id: u64,
    /// Foreign surfaces whose tab close affordance was activated. The native
    /// compositor drains this edge and sends `xdg_toplevel.close`; keeping it
    /// here lets the chrome remove its durable tab immediately without owning
    /// Wayland protocol handles.
    close_surface_requests: Vec<u64>,
    /// Client windows being carried into another workspace, from the sheet
    /// asking the person through to the old window closing.
    pub window_transfers: super::window_transfer::WindowTransfers,
    /// The full-screen chord was pressed. The native compositor drains it
    /// and toggles the focused client's `xdg_toplevel` fullscreen state,
    /// which only it holds the protocol handles for.
    fullscreen_toggle_requested: bool,
    /// Compositor-only remaining paints after overview / workspace switch
    /// (W16). The host's crossfade demand still clears when the curve
    /// settles — this count is not `needs_animation_frame`.
    transition_paints_remaining: u32,
    /// Window-management operations an action asked for. The overlay host
    /// drains them on its next update and runs each exactly as its chord
    /// would, since the focused group's widgets live there.
    shortcut_action_requests: Vec<settings_store::hardware::ShortcutAction>,
    cancel_tab_drag_requested: bool,
    axis_navigation_enabled: bool,
    axis_gather_requested: bool,
    /// The input-source chord chose this source. The native compositor
    /// drains it and switches the seat and the physical keyboard at once;
    /// the persisted choice reaches them again on the settings poll.
    input_source_request: Option<usize>,
    /// Docked at the strip's left edge by default; `Some(point)`
    /// detaches the wheel to the pointer. Cleared on Escape or
    /// picking a workspace.
    wheel_summon: Option<vello::kurbo::Point>,
    /// Last layout pass, so placement can ask "which border can the
    /// user actually see" without reaching into the widget tree.
    strip_viewport: Option<tilo_desktop_model::commands::StripViewport>,
    /// Logical desktop bounds from the overlay host's latest layout pass.
    viewport_size: Option<(f64, f64)>,
    /// The offer to continue a tab from another of this person's devices.
    continuity_suggestions: crate::shell::continuity_suggestion::ContinuitySuggestions,
    /// Recently closed tabs, for `tab.reopen` and the close toast's Undo.
    closed_tabs: crate::shell::closed_tabs::ClosedTabStack,
    pending_closed_tabs: Vec<crate::shell::closed_tabs::ClosedTab>,
    /// Close toasts still offering Undo: notification id -> closed entry.
    /// They never enter notification history.
    undo_toasts: BTreeMap<u64, u64>,
    /// Toasts that undo a workspace deletion, by toast id: where the
    /// workspace was and what it held.
    workspace_undo_toasts: BTreeMap<u64, (usize, crate::shell::state::Workspace)>,
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
            native_app_opening: None,
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
            pad_map: None,
            ws_settings_tab: WsTab::default(),
            ws_settings_subject: None,
            ws_settings_profile_reveal_generation: 0,
            workspace_settings_anchor: vello::kurbo::Rect::ZERO,
            ws_grant_draft: tilo_desktop::workspace_settings::GrantDraft::default(),
            revision: 0,
            instance: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            appearance_generation: 0,
            observed_external_tree_revision: 0,
            activity_glance: ActivityGlance::default(),
            activity_focus_request: None,
            workspace_view_request: None,
            history_action_request: None,
            pending_grants: VecDeque::new(),
            test_only_owner_override: None,
            local_user_id,
            notification_log: NotificationLog::default(),
            notification_bridge: None,
            notification_history_store: None,
            notification_badges: BTreeMap::new(),
            system_sounds: None,
            priority_contacts: crate::priority_contacts::PriorityContactRegistry::in_memory(),
            notification_policy: settings_store::notification_policy::NotificationPolicy::default(),
            notification_policy_checked: None,
            presenting: false,
            visual_alert_until: None,
            session_locked: false,
            active_exposures: Vec::new(),
            session_lock_requested: false,
            session_action_requests: Vec::new(),
            notification_launch_requests: Vec::new(),
            notification_action_requests: HashMap::new(),
            notification_action_results: HashMap::new(),
            fullscreen_group: None,
            user_fullscreen_group: None,
            maximized_group: None,
            fullscreen_presentation: false,
            single_strip: false,
            peer_sync_enabled: false,
            collab_unavailable: None,
            peer_sync_requests: Vec::new(),
            presence_requests: Vec::new(),
            screenshot_sends: Vec::new(),
            wheel_summon: None,
            legacy_record_requests: Vec::new(),
            tracked_record_requests: Vec::new(),
            record_request_results: BTreeMap::new(),
            next_record_request_id: 1,
            peer_sync_request_results: BTreeMap::new(),
            next_peer_sync_request_id: 1,
            close_surface_requests: Vec::new(),
            window_transfers: super::window_transfer::WindowTransfers::default(),
            fullscreen_toggle_requested: false,
            transition_paints_remaining: 0,
            shortcut_action_requests: Vec::new(),
            cancel_tab_drag_requested: false,
            axis_navigation_enabled: false,
            axis_gather_requested: false,
            input_source_request: None,
            strip_viewport: None,
            viewport_size: None,
            continuity_suggestions:
                crate::shell::continuity_suggestion::ContinuitySuggestions::default(),
            closed_tabs: crate::shell::closed_tabs::ClosedTabStack::default(),
            pending_closed_tabs: Vec::new(),
            undo_toasts: BTreeMap::new(),
            workspace_undo_toasts: BTreeMap::new(),
            records: RecordStore::in_memory(),
            pending_tab_records: VecDeque::new(),
            failed_tab_records: BTreeMap::new(),
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
    pub fn set_strip_viewport(
        &mut self,
        viewport: Option<tilo_desktop_model::commands::StripViewport>,
    ) {
        self.strip_viewport = viewport;
    }

    pub fn strip_viewport(&self) -> Option<&tilo_desktop_model::commands::StripViewport> {
        self.strip_viewport.as_ref()
    }

    /// Replace app startup inputs for deterministic offscreen captures.
    #[doc(hidden)]
    pub fn set_hosted_inputs_for_capture(
        &mut self,
        inputs: super::hosted_inputs::HostedTileInputs,
    ) {
        self.hosted_inputs = inputs;
        self.bump();
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

    pub(crate) fn begin_peer_sync_request(
        &mut self,
        request: crate::shell::overlay_host::PeerSyncAction,
    ) -> u64 {
        let request_id = self.next_peer_sync_request_id;
        self.next_peer_sync_request_id = self.next_peer_sync_request_id.saturating_add(1);
        self.peer_sync_requests
            .push(crate::shell::overlay_host::PeerSyncAction::Tracked {
                request_id,
                action: Box::new(request),
            });
        request_id
    }

    pub fn take_peer_sync_requests(&mut self) -> Vec<crate::shell::overlay_host::PeerSyncAction> {
        std::mem::take(&mut self.peer_sync_requests)
    }

    pub(crate) fn complete_peer_sync_request(
        &mut self,
        request_id: u64,
        result: Result<serde_json::Value, String>,
    ) {
        self.peer_sync_request_results.insert(request_id, result);
    }

    pub fn push_presence_request(&mut self, request: PresenceRequest) {
        self.presence_requests.push(request);
    }

    pub fn take_presence_requests(&mut self) -> Vec<PresenceRequest> {
        std::mem::take(&mut self.presence_requests)
    }

    pub fn push_screenshot_send(&mut self, target: ui::widget::ScreenshotSendTarget) {
        self.screenshot_sends.push(target);
    }

    pub fn take_screenshot_sends(&mut self) -> Vec<ui::widget::ScreenshotSendTarget> {
        std::mem::take(&mut self.screenshot_sends)
    }

    pub(crate) fn take_peer_sync_request_result(
        &mut self,
        request_id: u64,
    ) -> Option<Result<serde_json::Value, String>> {
        self.peer_sync_request_results.remove(&request_id)
    }

    /// Queue an authorized record import against its exact workspace target.
    pub fn push_tracked_record_request(
        &mut self,
        id: impl Into<String>,
        kind: impl Into<String>,
        workspace_id: WorkspaceId,
        room: crate::shell::state::RoomId,
        actor: crate::shell::state::Actor,
    ) -> u64 {
        let request_id = self.next_record_request_id;
        self.next_record_request_id = self.next_record_request_id.saturating_add(1);
        self.tracked_record_requests.push(RecordRequest {
            request_id,
            record_id: id.into(),
            kind: kind.into(),
            workspace_id,
            room,
            actor,
        });
        request_id
    }

    #[deprecated(note = "use take_tracked_record_requests for authorization-aware requests")]
    pub fn take_record_requests(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.legacy_record_requests)
    }

    pub(crate) fn take_tracked_record_requests(&mut self) -> Vec<RecordRequest> {
        std::mem::take(&mut self.tracked_record_requests)
    }

    pub fn complete_record_request(&mut self, request_id: u64, result: Result<(), String>) {
        self.record_request_results.insert(request_id, result);
    }

    pub fn take_record_request_result(&mut self, request_id: u64) -> Option<Result<(), String>> {
        self.record_request_results.remove(&request_id)
    }

    pub fn request_surface_close(&mut self, surface_id: u64) {
        if !self.close_surface_requests.contains(&surface_id) {
            self.close_surface_requests.push(surface_id);
        }
    }

    pub fn take_surface_close_requests(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.close_surface_requests)
    }

    pub fn request_fullscreen_toggle(&mut self) {
        self.fullscreen_toggle_requested = true;
        self.bump();
    }

    pub fn take_fullscreen_toggle_request(&mut self) -> bool {
        std::mem::take(&mut self.fullscreen_toggle_requested)
    }

    /// Ask the compositor to paint at least `n` frames. Does not bump:
    /// host animation demand stays the crossfade, not this count.
    pub fn request_transition_paints(&mut self, n: u32) {
        self.transition_paints_remaining = self.transition_paints_remaining.max(n);
    }

    #[must_use]
    pub fn transition_paints_remaining(&self) -> u32 {
        self.transition_paints_remaining
    }

    pub fn consume_transition_paint(&mut self) {
        self.transition_paints_remaining = self.transition_paints_remaining.saturating_sub(1);
    }

    pub fn axis_navigation_enabled(&self) -> bool {
        self.axis_navigation_enabled
    }

    /// Experimental local view policy. Never persisted or shared with peers.
    pub fn set_axis_navigation_enabled(&mut self, enabled: bool) {
        self.axis_navigation_enabled = enabled;
        self.axis_gather_requested = enabled;
        self.bump();
    }

    pub fn request_axis_gather(&mut self) {
        self.axis_gather_requested = self.axis_navigation_enabled;
        self.bump();
    }

    pub fn take_axis_gather_request(&mut self) -> bool {
        std::mem::take(&mut self.axis_gather_requested)
    }

    /// Run `action` as if its chord had been pressed once, on the overlay
    /// host's next update.
    pub fn request_shortcut_action(&mut self, action: settings_store::hardware::ShortcutAction) {
        self.shortcut_action_requests.push(action);
        self.bump();
    }

    pub fn take_shortcut_action_requests(
        &mut self,
    ) -> Vec<settings_store::hardware::ShortcutAction> {
        std::mem::take(&mut self.shortcut_action_requests)
    }

    /// Cancel the shell's pending tab gesture through the same path as Escape.
    pub fn request_tab_drag_cancel(&mut self) {
        self.cancel_tab_drag_requested = true;
        self.bump();
    }

    pub fn take_tab_drag_cancel_request(&mut self) -> bool {
        std::mem::take(&mut self.cancel_tab_drag_requested)
    }

    pub fn request_input_source(&mut self, index: usize) {
        self.input_source_request = Some(index);
        self.bump();
    }

    pub fn take_input_source_request(&mut self) -> Option<usize> {
        self.input_source_request.take()
    }

    pub fn fullscreen_group(&self) -> Option<u64> {
        self.user_fullscreen_group.or(self.fullscreen_group)
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

    #[must_use]
    pub fn single_strip(&self) -> bool {
        self.single_strip
    }

    pub fn set_single_strip(&mut self, single: bool) {
        if self.single_strip != single {
            self.single_strip = single;
            self.bump();
        }
    }

    /// The active workspace's focused strip, or its first.
    #[must_use]
    pub fn active_strip_id(&self) -> Option<crate::shell::state::StripId> {
        let tree = self.tree();
        let ws = tree.desktop.active_workspace()?;
        tree.desktop
            .active_strip
            .get(&ws.id)
            .copied()
            .filter(|id| ws.strips.iter().any(|s| s.id == *id))
            .or_else(|| ws.strips.first().map(|s| s.id))
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

    pub fn set_user_fullscreen_group(&mut self, group: Option<u64>) {
        if self.user_fullscreen_group != group {
            self.user_fullscreen_group = group;
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

    /// Act as `principal` in every room, whatever the machine's identity
    /// registry selects. A renderer whose output must not depend on the
    /// machine it runs on pins its identity this way; a live session keeps
    /// the registry's per-room choice.
    pub fn act_as(&mut self, principal: crate::shell::state::UserId) {
        self.local_user_id.clone_from(&principal);
        self.identity_source = IdentitySource::Explicit(principal);
    }

    /// Person principal this session presents inside `room`.
    pub fn local_user_for_room(
        &self,
        room: &crate::shell::state::RoomId,
    ) -> crate::shell::state::UserId {
        self.identity_source.local_user_for_room(room)
    }

    /// Hand the workspaces made before this session had an identity to the
    /// identity first-login setup just provisioned. Until then the session
    /// acted as the OS `account` name; the new key-derived principal is no
    /// member of a workspace that name owns, so every app opened there would
    /// be refused its collaboration route. Boot does the same for a session
    /// that starts with an identity already in place.
    pub fn adopt_provisioned_identity(&mut self, account: &str) -> usize {
        if matches!(self.identity_source, IdentitySource::SessionRegistry) {
            self.local_user_id = crate::shell::state::local_user_id();
        }
        let rebound = self
            .tree_mut()
            .rebind_account_owned_workspaces(account, |room| {
                self.identity_source.local_user_for_room(room)
            })
            .len();
        if rebound > 0 {
            tracing::info!(
                "capabilities: {rebound} workspace(s) created before first-login setup now belong to its identity"
            );
            self.bump();
        }
        rebound
    }

    /// Person principal selected for the active workspace, or the session
    /// fallback when no workspace is active.
    pub fn active_local_user(&self) -> crate::shell::state::UserId {
        self.tree().desktop.active_workspace().map_or_else(
            || self.local_user_id.clone(),
            |workspace| self.local_user_for_room(&workspace.room),
        )
    }

    /// What this host hands the apps it mounts.
    pub fn hosted_inputs(&self) -> &super::hosted_inputs::HostedTileInputs {
        &self.hosted_inputs
    }

    pub fn records(&self) -> &RecordStore {
        &self.records
    }

    /// Install the session's persistent record store; called before
    /// the window tree is built.
    pub fn set_records(&mut self, records: RecordStore) {
        self.pending_tab_records.clear();
        self.failed_tab_records.clear();
        self.records = records;
        self.bump();
    }

    /// Finalize a new-tab action after its complete document reference/setup.
    /// Only transient record-loan contention retains a bounded retry ticket.
    pub(crate) fn create_new_tab_record(&mut self, tab: u64) {
        let Some(identity) = TabRecordCreationIdentity::capture(&self.tree(), tab) else {
            return;
        };
        match super::tab_documents::create_scratch_tab_record(&self.records, tab) {
            Ok(()) => {}
            Err(ui::record::RecordOpenError::Busy) => {
                self.pending_tab_records
                    .push_back(PendingTabRecordCreation {
                        identity,
                        store: self.records.clone(),
                    });
            }
            Err(error) => {
                self.failed_tab_records
                    .insert(tab, (identity, new_document_unavailable(&error)));
            }
        }
    }

    /// Retain every valid ticket, attempting at most 64 per drain. Busy tickets
    /// rotate behind unattempted work; rebuilding missing tabs adds no authority.
    pub(crate) fn retry_new_tab_records(&mut self) {
        let tree = self.tree_handle.read();
        let before = self.failed_tab_records.len();
        self.failed_tab_records.retain(|tab, (identity, _)| {
            TabRecordCreationIdentity::capture(&tree, *tab).as_ref() == Some(identity)
        });
        let mut changed = self.failed_tab_records.len() != before;
        self.pending_tab_records.retain(|pending| {
            TabRecordCreationIdentity::capture(&tree, pending.identity.tab).as_ref()
                == Some(&pending.identity)
        });
        let attempts = self.pending_tab_records.len().min(64);
        for _ in 0..attempts {
            let Some(pending) = self.pending_tab_records.pop_front() else {
                break;
            };
            match super::tab_documents::create_scratch_tab_record(
                &pending.store,
                pending.identity.tab,
            ) {
                Ok(()) => changed = true,
                Err(ui::record::RecordOpenError::Busy) => {
                    self.pending_tab_records.push_back(pending);
                }
                Err(error) => {
                    self.failed_tab_records.insert(
                        pending.identity.tab,
                        (pending.identity, new_document_unavailable(&error)),
                    );
                    changed = true;
                }
            }
        }
        drop(tree);
        if changed {
            self.bump();
        }
    }

    pub(crate) fn record_creation_errors(&self, tree: &WorkspaceTree) -> BTreeMap<u64, String> {
        self.failed_tab_records
            .iter()
            .filter(|(tab, (identity, _))| {
                TabRecordCreationIdentity::capture(tree, **tab).as_ref() == Some(identity)
            })
            .map(|(tab, (_, error))| (*tab, error.clone()))
            .collect()
    }

    /// The guard drops at the end of the borrow scope, so concurrent
    /// readers (paint, portal lookup, peer-sync export) unblock as soon
    /// as the mutation lands.
    pub fn tree_mut(&self) -> crate::shell::state::WorkspaceTreeWriteGuard<'_> {
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
        tree.desktop.active_workspace().is_some_and(|workspace| {
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
                    .desktop
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
                let room = if let Some(ws) = tree.desktop.active_workspace_mut() {
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
                    tree.queue_save();
                }
            }
            self.bump();
        }
    }

    /// Mirror the apps holding the screen awake into quick settings,
    /// repainting only when the set changed.
    pub fn set_awake_holders(&mut self, holders: Vec<String>) {
        if self.quick_settings.awake_holders != holders {
            self.quick_settings.awake_holders = holders;
            self.bump();
        }
    }

    pub fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// This chrome's identity, unique in the process; see `instance`.
    pub(crate) fn instance(&self) -> u64 {
        self.instance
    }

    /// Persist-side appearance change: chrome rebuilds, and the live frame
    /// applies the configured theme and locale on the next paint instead of
    /// waiting for the settings-file poll.
    pub fn note_appearance_change(&mut self) {
        self.appearance_generation = self.appearance_generation.wrapping_add(1);
        self.bump();
    }

    /// Fold non-chrome mutations of the shared tree into retained chrome's
    /// ordinary revision stream. Returns true exactly once per observed epoch.
    pub fn sync_external_tree_revision(&mut self) -> bool {
        let external = self.tree().desktop.external_revision;
        if external == self.observed_external_tree_revision {
            return false;
        }
        self.observed_external_tree_revision = external;
        self.publish_completed_closes();
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

    /// Private presentation requests share the portable descriptor contract.
    /// The descriptor names workspaces by their raw id; the shell resolves it
    /// against its own desktop model.
    ///
    /// # Errors
    ///
    /// Returns an error if the named workspace or requested view is unavailable.
    pub fn request_workspace_view(
        &mut self,
        request: tilo_desktop::activity::WorkspaceViewRequest,
    ) -> Result<(), String> {
        use tilo_desktop::activity::WorkspaceViewRequest;
        let workspace = WorkspaceId(request.workspace().0);
        if !self
            .tree()
            .desktop
            .workspaces
            .iter()
            .any(|candidate| candidate.id == workspace)
        {
            return Err("Workspace unavailable".into());
        }
        let previous = self.tree().desktop.active;
        match &request {
            WorkspaceViewRequest::Select { .. } => {
                self.tree_mut().desktop.active = workspace;
                self.overlays.room_activity = false;
            }
            WorkspaceViewRequest::OpenActivity { .. } => {
                if previous != workspace {
                    return Err("Workspace is no longer active".into());
                }
                self.open_room_activity(None);
            }
            WorkspaceViewRequest::CloseActivity { .. } => {
                if previous != workspace {
                    return Err("Workspace is no longer active".into());
                }
                self.overlays.room_activity = false;
            }
        }
        self.workspace_view_request = Some((previous, request));
        self.bump();
        Ok(())
    }
    pub fn take_workspace_view_request(
        &mut self,
    ) -> Option<(WorkspaceId, tilo_desktop::activity::WorkspaceViewRequest)> {
        self.workspace_view_request.take()
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

    /// Open Activity and run a selected-row history action (`history.compare`,
    /// `history.undo`, `history.restore`) on the next paint.
    ///
    /// # Errors
    ///
    /// Returns an error when `id` is not a recognised history action.
    pub fn request_history_selection_action(&mut self, id: &str) -> Result<(), String> {
        match id {
            tilo_desktop::activity::HISTORY_COMPARE
            | tilo_desktop::activity::HISTORY_UNDO
            | tilo_desktop::activity::HISTORY_RESTORE
            | tilo_desktop::activity::ACTIVITY_ACKNOWLEDGE
            | tilo_desktop::activity::ACTIVITY_TASKS => {}
            _ => return Err("Unknown history action".into()),
        }
        self.open_room_activity(None);
        self.history_action_request = Some(id.to_string());
        self.bump();
        Ok(())
    }

    pub fn take_history_action_request(&mut self) -> Option<String> {
        self.history_action_request.take()
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
        self.ws_settings_subject = None;
        self.overlays.ws_settings = true;
        self.overlays.quick_settings = false;
    }

    /// Reveal one exact durable participant without changing their authority.
    pub fn open_workspace_profile(&mut self, subject: WorkspaceSettingsSubject) {
        self.ws_settings_profile_reveal_generation =
            self.ws_settings_profile_reveal_generation.wrapping_add(1);
        self.ws_settings_tab = subject.tab();
        self.ws_settings_subject = Some(subject);
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
            tree.desktop.active = workspace;
            tree.desktop.active_strip.insert(workspace, strip);
            tree.desktop.set_active_group(strip, group);
            if let Some(g) = tree
                .desktop
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
            tree.queue_save();
        }
        self.bump();
    }

    /// Replace what personal continuity knows about other devices' focus.
    pub fn set_continuity_feed(
        &mut self,
        feed: crate::shell::continuity_suggestion::ContinuityFeed,
    ) {
        if self.continuity_suggestions.set_feed(feed) {
            self.bump();
        }
    }

    /// The tab another of this person's devices just moved to, when this
    /// desktop has it and is not already showing it.
    pub fn continuity_suggestion(
        &self,
        now: std::time::Instant,
    ) -> Option<crate::shell::continuity_suggestion::ContinuitySuggestion> {
        self.continuity_suggestions.current(&self.tree(), now)
    }

    /// Focus the offered tab here. Only an explicit accept ever moves focus;
    /// the offer is retired so it does not return when the person moves on.
    pub fn accept_continuity_suggestion(
        &mut self,
        now: std::time::Instant,
    ) -> Option<crate::shell::continuity_suggestion::ContinuitySuggestion> {
        let suggestion = self.continuity_suggestion(now)?;
        self.continuity_suggestions.dismiss(&suggestion);
        self.focus_tab(
            suggestion.workspace,
            suggestion.strip,
            suggestion.group,
            suggestion.tab,
        );
        Some(suggestion)
    }

    /// Hide the current offer until that device's focus changes again.
    pub fn dismiss_continuity_suggestion(
        &mut self,
        now: std::time::Instant,
    ) -> Option<crate::shell::continuity_suggestion::ContinuitySuggestion> {
        let suggestion = self.continuity_suggestion(now)?;
        self.continuity_suggestions.dismiss(&suggestion);
        self.bump();
        Some(suggestion)
    }

    /// Publish Undo and its toast only after removal. A live client may
    /// refuse its close request and keep the tab present.
    pub fn remember_closed_tab_after_removal(
        &mut self,
        entry: crate::shell::closed_tabs::ClosedTab,
    ) {
        if self.pending_closed_tabs.iter().any(|pending| {
            pending
                .tabs
                .iter()
                .map(|content| content.tab.id)
                .eq(entry.tabs.iter().map(|content| content.tab.id))
        }) {
            return;
        }
        self.pending_closed_tabs.push(entry);
        self.publish_completed_closes();
    }

    fn publish_completed_closes(&mut self) {
        let present: std::collections::HashSet<_> = self
            .tree()
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .map(|tab| tab.id)
            .collect();
        let entries = std::mem::take(&mut self.pending_closed_tabs);
        for entry in entries {
            if entry
                .tabs
                .iter()
                .any(|content| present.contains(&content.tab.id))
            {
                self.pending_closed_tabs.push(entry);
            } else {
                self.remember_closed_tab(entry);
            }
        }
    }

    /// Remember a completed close and offer a toast whose activation restores it.
    pub fn remember_closed_tab(&mut self, entry: crate::shell::closed_tabs::ClosedTab) -> u64 {
        use crate::shell::i18n::{t, t_args};

        let heading = if entry.tabs.len() > 1 {
            t_args(
                "tab-closed-group-toast",
                &[("count", entry.tabs.len().into())],
            )
        } else {
            t_args("tab-closed-toast", &[("title", entry.title().into())])
        };
        let hint = settings_store::hardware::ShortcutConfig::load()
            .bindings
            .get(&settings_store::hardware::ShortcutAction::ReopenClosedTab)
            .map_or_else(
                || t("tab-closed-undo-hint-unbound"),
                |chord| {
                    t_args(
                        "tab-closed-undo-hint",
                        &[(
                            "chord",
                            crate::shell::keyboard_shortcuts::chord_text(chord).into(),
                        )],
                    )
                },
            );
        let serial = self.closed_tabs.push(entry);
        let mut toast = Notification::new(heading, hint)
            .with_severity(NotificationSeverity::Info)
            .with_icon_kind(NotificationIconKind::Generic);
        toast.dismiss_after = crate::shell::closed_tabs::UNDO_WINDOW;
        toast.actions = vec![("default".into(), t("tab-closed-undo"))];
        // A direct answer to the person's own action, not an interruption.
        toast.bypass_dnd = true;
        toast.show_on_lock_screen = false;
        let id = toast.id;
        self.undo_toasts.insert(id, serial);
        self.push_notification_full(toast);
        serial
    }

    /// Delete the workspace on screen, here and on the person's other
    /// devices, and offer to undo it. The last workspace stays. Returns
    /// whether it was deleted.
    pub fn delete_active_workspace(&mut self) -> bool {
        use crate::shell::i18n::{t, t_args};

        let removed = {
            let mut tree = self.tree_mut();
            let active = tree.desktop.active;
            tree.delete_workspace(active)
        };
        let Some((index, workspace)) = removed else {
            return false;
        };
        let mut toast = Notification::new(
            t_args(
                "workspace-deleted-toast",
                &[("name", workspace.name.clone().into())],
            ),
            t("workspace-deleted-toast-hint"),
        )
        .with_severity(NotificationSeverity::Info)
        .with_icon_kind(NotificationIconKind::Generic);
        toast.dismiss_after = crate::shell::closed_tabs::UNDO_WINDOW;
        toast.actions = vec![("default".into(), t("workspace-deleted-undo"))];
        toast.bypass_dnd = true;
        toast.show_on_lock_screen = false;
        self.workspace_undo_toasts
            .insert(toast.id, (index, workspace));
        self.push_notification_full(toast);
        self.bump();
        true
    }

    /// The deletion toast's Undo: put the workspace back where it was, on
    /// screen. The person's other devices get it back with the next list.
    pub fn undo_deleted_workspace(&mut self, toast: u64) -> bool {
        let Some((index, workspace)) = self.workspace_undo_toasts.remove(&toast) else {
            return false;
        };
        let restored = self.tree_mut().restore_workspace(index, workspace);
        self.bump();
        restored
    }

    /// Move the workspace on screen one place earlier or later in the
    /// person's list.
    pub fn move_active_workspace(&mut self, step: i32) -> bool {
        let moved = {
            let mut tree = self.tree_mut();
            let active = tree.desktop.active;
            tree.move_workspace(active, step)
        };
        if moved {
            self.bump();
        }
        moved
    }

    /// Show the workspace on screen on the `index`th of the person's devices
    /// in its menu, or keep it off there.
    pub fn toggle_active_workspace_device(&mut self, index: usize) -> bool {
        let queued = {
            let mut tree = self.tree_mut();
            let Some(room) = tree
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room)
            else {
                return false;
            };
            let Some(choice) = tree
                .personal_device_choices
                .get(&room)
                .and_then(|choices| choices.get(index))
                .cloned()
            else {
                return false;
            };
            tree.personal_device_requests
                .push((room, choice.device, !choice.shown));
            if let Some(choice) = tree
                .personal_device_choices
                .get_mut(&room)
                .and_then(|choices| choices.get_mut(index))
            {
                choice.shown = !choice.shown;
            }
            drop(tree);
            true
        };
        self.bump();
        queued
    }

    /// Reopen the most recently closed tab in the active workspace, at its
    /// old group and index, and focus it.
    pub fn reopen_closed_tab(&mut self) -> Option<crate::shell::state::TabId> {
        let handle = self.tree_handle.clone();
        let tree = handle.read();
        let workspace = tree.desktop.active;
        let entry = self.closed_tabs.pop_for(&tree, workspace);
        drop(tree);
        self.restore_closed_tab(entry?)
    }

    /// The close toast's Undo: reopen exactly that close.
    pub fn undo_closed_tab(&mut self, serial: u64) -> Option<crate::shell::state::TabId> {
        let entry = self.closed_tabs.take(serial)?;
        self.restore_closed_tab(entry)
    }

    fn restore_closed_tab(
        &mut self,
        entry: crate::shell::closed_tabs::ClosedTab,
    ) -> Option<crate::shell::state::TabId> {
        let serial = entry.serial;
        let reopened = {
            let mut tree = self.tree_mut();
            let reopened = crate::shell::closed_tabs::restore(&mut tree, entry);
            if reopened.is_some() {
                tree.queue_save();
            }
            reopened
        };
        // The toast would now undo nothing.
        let stale: Vec<u64> = self
            .undo_toasts
            .iter()
            .filter(|(_, candidate)| **candidate == serial)
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.undo_toasts.remove(&id);
            self.notification_log
                .discard_active_where(|toast| toast.id == id);
        }
        self.bump();
        reopened
    }

    /// Routes into the active workspace's first strip/group via
    /// [`super::workspace_drop::apply_workspace_drop_from_path`].
    /// Returns the new tab id, or `None` if the path has no filename.
    pub fn handle_file_drop(&mut self, path: &std::path::Path) -> Option<u64> {
        let tab_id = {
            let mut tree = self.tree_mut();
            super::workspace_drop::apply_workspace_drop_from_path(&mut tree, path)
        };
        if let Some(tab_id) = tab_id {
            self.create_new_tab_record(tab_id);
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
            self.create_new_tab_record(tab_id);
            // A dropped file is an explicit capability grant to the app
            // the user aimed at, keyed by the tab the drop created.
            self.grant_dropped_file_access(path, strip_id, group_id, tab_id);
            self.bump();
        }
        tab_id
    }

    /// Exact-placement sibling of [`Self::handle_file_drop_at`]. External
    /// content and existing app tabs share the row's spatial target, so a
    /// fallback Files/Editor tab must preserve the named tab gap or window
    /// seam rather than collapsing it to a group id.
    pub fn handle_file_drop_at_placement(
        &mut self,
        path: &std::path::Path,
        placement: crate::shell::tab_binding::Placement,
    ) -> Option<u64> {
        const MAX_HOSTED_TEXT_BYTES: u64 = 32 * 1024 * 1024;
        let mut hosted_document = None;
        let mut files_directory = None;
        if path.exists() {
            let canonical = path.canonicalize().ok()?;
            if canonical.is_dir() {
                files_directory = Some(canonical);
            } else {
                let is_editor = canonical
                    .extension()
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|extension| {
                        super::workspace_drop::EDITOR_EXTENSIONS
                            .iter()
                            .any(|candidate| candidate.eq_ignore_ascii_case(extension))
                    });
                if is_editor {
                    if canonical.metadata().ok()?.len() > MAX_HOSTED_TEXT_BYTES {
                        return None;
                    }
                    hosted_document =
                        Some((canonical.clone(), std::fs::read_to_string(&canonical).ok()?));
                } else {
                    files_directory = canonical.parent().map(std::path::Path::to_path_buf);
                }
            }
        }
        let tab_id = {
            let mut tree = self.tree_mut();
            super::workspace_drop::apply_workspace_drop_at_placement_from_path(
                &mut tree, path, placement,
            )
        }?;
        let address = {
            let tree = self.tree();
            tree.desktop.active_workspace().and_then(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip.window_groups.iter().find_map(|group| {
                        group
                            .tabs
                            .iter()
                            .any(|tab| tab.id == tab_id)
                            .then_some((strip.id, group.id))
                    })
                })
            })
        };
        if let Some((strip, group)) = address {
            self.grant_dropped_file_access(path, strip, group, tab_id);
        }
        // Exact placement is only half of opening a drop: hosted apps also
        // need device-local startup data naming the thing they should show.
        // Synthetic/nonexistent paths used by shell layout fixtures retain the
        // historical placeholder behaviour; real paths are bound here.
        if let Some(directory) = files_directory {
            self.hosted_inputs.bind_files_directory(tab_id, directory);
        }
        if let Some((path, text)) = hosted_document {
            self.hosted_inputs.bind_document(
                tab_id,
                super::hosted_inputs::HostedDocument {
                    provider_id: "org.tilo.Editor",
                    path: path.clone(),
                    text,
                },
            );
            self.tree_mut().native_document_tabs.insert(
                tab_id,
                crate::shell::state::NativeDocumentReference {
                    provider_id: "org.tilo.Editor".to_string(),
                    path,
                },
            );
        }
        self.bump();
        Some(tab_id)
    }

    /// Through the SAME [`GrantStoreHandle`] the portal uses, so the
    /// portal's later filesystem checks see it exactly as they see a grant
    /// the user approved in a prompt. Keyed by
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
        let ws_id = self.tree().desktop.active;
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

    /// The tab's canonical app id — see
    /// [`tilo_desktop_model::apps::canonical_app_id`].
    pub(crate) fn tab_app_id(
        &self,
        strip_id: StripId,
        group_id: GroupId,
        tab_id: u64,
    ) -> Option<crate::shell::state::AppId> {
        let tree = self.tree();
        let ws = tree.desktop.active_workspace()?;
        let strip = ws.strips.iter().find(|s| s.id == strip_id)?;
        let group = strip.window_groups.iter().find(|g| g.id == group_id)?;
        let tab = group.tabs.iter().find(|t| t.id == tab_id)?;
        // A shell-owned surface such as workspace activity belongs to no
        // app, so there is nothing to key a grant to.
        let app_id = tilo_desktop_model::apps::canonical_app_id(&tab.tile)?;
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
    /// title instead of the static [`SearchScopeText::localized_label`].
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
        let ws_id = tree.desktop.active;
        let strip_id = tree.desktop.active_strip.get(&ws_id).copied()?;
        let group_id = tree.desktop.active_group.get(&strip_id).copied()?;
        let tab_id = tree
            .desktop
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
        let ws_id = tree.desktop.active;
        let strip_id = tree.desktop.active_strip.get(&ws_id).copied()?;
        let group_id = tree.desktop.active_group.get(&strip_id).copied()?;
        let ws = tree.desktop.workspaces.iter().find(|w| w.id == ws_id)?;
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
        let Some(ws) = tree.desktop.active_workspace() else {
            return (0, 0);
        };
        let count = ws.strips.len();
        let focused = tree
            .desktop
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
            let Some(ws) = tree.desktop.active_workspace() else {
                return false;
            };
            let ws_id = ws.id;
            let Some(strip) = ws.strips.get(index) else {
                return false;
            };
            let sid = strip.id;
            let name = strip.name.clone();
            let prev = tree.desktop.active_strip.get(&ws_id).copied();
            if prev == Some(sid) {
                return false;
            }
            tree.desktop.active_strip.insert(ws_id, sid);
            tree.queue_save();
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

    /// Focus `strip` of the active workspace. `false` when it is already
    /// focused or not in the active workspace.
    pub fn focus_strip(&mut self, strip: crate::shell::state::StripId) -> bool {
        let index = {
            let tree = self.tree();
            tree.desktop
                .active_workspace()
                .and_then(|ws| ws.strips.iter().position(|s| s.id == strip))
        };
        index.is_some_and(|index| self.focus_strip_index(index))
    }

    /// The strip an agent works in: `current` while it is still in the active
    /// workspace, else an empty strip nobody `claimed` yet, else a new strip
    /// named `label` at the bottom. `None` without an active workspace.
    pub fn agent_strip(
        &mut self,
        current: Option<crate::shell::state::StripId>,
        claimed: impl Fn(crate::shell::state::StripId) -> bool,
        label: &str,
    ) -> Option<crate::shell::state::StripId> {
        let strip = {
            let mut tree = self.tree_mut();
            let ws = tree.desktop.active_workspace_mut()?;
            if let Some(current) = current.filter(|id| ws.strips.iter().any(|s| s.id == *id)) {
                return Some(current);
            }
            let free = ws
                .strips
                .iter()
                .find(|s| s.window_groups.is_empty() && !claimed(s.id))
                .map(|s| s.id);
            if let Some(free) = free {
                return Some(free);
            }
            let strip = crate::shell::state::Strip::new(
                <crate::shell::state::StripId as crate::shell::state::FreshId>::new(),
                label.to_owned(),
            );
            let id = strip.id;
            ws.strips.push(strip);
            tree.queue_save();
            id
        };
        self.bump();
        Some(strip)
    }

    /// Remove `strip` from the active workspace if it holds no window and is
    /// not the workspace's last strip. Focus moves to the first strip when it
    /// had it. Returns whether the strip went.
    pub fn remove_strip_if_empty(&mut self, strip: crate::shell::state::StripId) -> bool {
        let removed = {
            let mut tree = self.tree_mut();
            let Some(ws) = tree.desktop.active_workspace_mut() else {
                return false;
            };
            let Some(index) = ws
                .strips
                .iter()
                .position(|s| s.id == strip && s.window_groups.is_empty())
            else {
                return false;
            };
            if ws.strips.len() < 2 {
                return false;
            }
            let ws_id = ws.id;
            ws.strips.remove(index);
            let first = ws.strips.first().map(|s| s.id);
            if tree.desktop.active_strip.get(&ws_id) == Some(&strip)
                && let Some(first) = first
            {
                tree.desktop.active_strip.insert(ws_id, first);
            }
            tree.desktop.active_group.remove(&strip);
            tree.queue_save();
            true
        };
        if removed {
            self.bump();
        }
        removed
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

    /// Show, update or (with `None`) close the pad map, repainting only
    /// when it changed.
    pub fn set_pad_map(&mut self, map: Option<crate::shell::pad_map::PadMap>) {
        if self.pad_map != map {
            self.pad_map = map;
            self.bump();
        }
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
        self.notification_log.active()
    }

    /// The live banners and retained history, as the shared notification
    /// stack and centre read them.
    pub fn notification_log(&self) -> &NotificationLog {
        &self.notification_log
    }

    pub fn push_notification(&mut self, title: impl Into<String>, body: impl Into<String>) -> u64 {
        self.push_notification_full(Notification::new(title, body))
    }

    /// Enforces the [`MAX_NOTIFICATIONS`] cap. While
    /// [`Self::notifications_quiet`], goes straight to history instead of a
    /// banner, unless it carries [`Notification::bypass_dnd`] (shell-only;
    /// D-Bus can't set it).
    pub fn push_notification_full(&mut self, n: Notification) -> u64 {
        let id = n.id;
        if self.notifications_quiet() && !n.bypass_dnd {
            self.record_history(n);
            self.bump();
            return id;
        }
        if visual_notification_alerts_enabled() {
            self.flash_visual_alert(std::time::Instant::now());
        }
        // Cap-evicted entries retire to history rather than dropping.
        for evicted in self.notification_log.show(n) {
            self.record_history(evicted);
        }
        self.bump();
        id
    }

    /// Start the accessibility edge flash now. Notifications raise it when
    /// visual alerts are on; bounce keys raises it for a swallowed press.
    pub fn flash_visual_alert(&mut self, now: std::time::Instant) {
        self.visual_alert_until = now.checked_add(std::time::Duration::from_millis(450));
        self.bump();
    }

    /// Whether shell chrome should paint the accessibility edge flash this
    /// frame. No timer thread or persistent alert state is needed.
    pub fn visual_alert_active(&self, now: std::time::Instant) -> bool {
        self.visual_alert_until.is_some_and(|until| now < until)
    }

    /// Every path that removes a toast (and the DND-suppression path)
    /// funnels through here, so history is the single complete record.
    fn record_history(&mut self, n: Notification) {
        // An Undo that has lapsed is not something to find later.
        if self.undo_toasts.remove(&n.id).is_some()
            || self.workspace_undo_toasts.remove(&n.id).is_some()
        {
            return;
        }
        self.notification_log.retain(n);
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
        let _ = self.play_system_sound(settings_store::system_sounds::SystemSoundEvent::Error);
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
            crate::shell::i18n::t("collab-unavailable-title"),
            crate::shell::i18n::t_args(
                "collab-unavailable-body",
                &[("reason", reason.as_str().into())],
            ),
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
        if let Some(removed) = self.notification_log.take_active(id) {
            if let Some(command) = remote_dismiss_command(&removed) {
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
        let Some(removed) = self.notification_log.take_active(id) else {
            return false;
        };
        if let Some(serial) = self.undo_toasts.remove(&id) {
            self.undo_closed_tab(serial);
            return true;
        }
        if self.workspace_undo_toasts.contains_key(&id) {
            self.undo_deleted_workspace(id);
            return true;
        }
        if let Some(request) = crate::shell::notification::scheduled_launch(&removed) {
            self.notification_launch_requests.push(request);
        }
        if let Some(command) = removed
            .default_action_key()
            .and_then(|key| remote_action_command(&removed, &key))
            .or_else(|| remote_dismiss_command(&removed))
        {
            self.queue_remote_notification_command(command);
        } else if let (Some(bridge), Some(dbus_id)) =
            (self.notification_bridge.as_ref(), removed.sender_id)
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
        if let Some(removed) = self
            .notification_log
            .take_active_where(|n| n.sender_id == Some(dbus_id))
        {
            self.emit_closed(&removed, reason);
            self.record_history(removed);
            self.bump();
        }
    }

    fn close_notification_by_dbus_id_silently(&mut self, dbus_id: u32) {
        if let Some(removed) = self
            .notification_log
            .take_active_where(|notification| notification.sender_id == Some(dbus_id))
        {
            self.record_history(removed);
            self.bump();
        }
    }

    /// Called each frame by the overlay host. Expired toasts from a D-Bus
    /// `Notify` emit `NotificationClosed(id, 1)`.
    pub fn update_notification_lifecycle(&mut self, now: std::time::Instant) -> usize {
        let expired = self.notification_log.take_expired(now);
        let dropped = expired.len();
        if let Some(bridge) = self.notification_bridge.as_ref() {
            for n in &expired {
                if let Some(d) = n.sender_id {
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
        self.notification_log.active().len()
    }

    // ---------- History ----------

    pub fn notification_history(&self) -> &[Notification] {
        self.notification_log.history()
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
        if self
            .notification_log
            .clear_history(std::time::Instant::now())
        {
            self.bump();
        }
    }

    /// Whether Clear all can still be undone.
    pub fn can_undo_notification_history_clear(&self) -> bool {
        self.notification_log.can_undo_clear()
    }

    /// Restore the cleared oldest-first snapshot ahead of notifications that
    /// arrived during the undo window, preserving exact chronological order.
    pub fn undo_notification_history_clear(&mut self) -> bool {
        let restored = self.notification_log.undo_clear();
        if restored {
            self.bump();
        }
        restored
    }

    /// Expire the undo snapshot. Returns true only on the clearing edge.
    pub fn expire_notification_history_clear(&mut self, now: std::time::Instant) -> bool {
        let expired = self.notification_log.expire_clear(now);
        if expired {
            self.bump();
        }
        expired
    }

    /// Restore the retained history from `store` and keep it written there
    /// from now on. Restored entries are older than anything this session
    /// already retained, so they go first and the newest bound still holds.
    pub fn attach_notification_history_store(
        &mut self,
        store: crate::shell::notification_history::NotificationHistoryStore,
    ) {
        self.notification_log.restore_history(store.load());
        self.notification_history_store = Some(store);
        self.bump();
    }

    /// Write the retained history if it changed since the last write. Called
    /// once per compositor tick; a failed write is logged and retried on the
    /// next change rather than every frame.
    pub fn flush_notification_history(&mut self) {
        if !self.notification_log.take_history_dirty() {
            return;
        }
        let Some(store) = self.notification_history_store.as_ref() else {
            return;
        };
        if let Err(error) = store.save(self.notification_log.history()) {
            tracing::warn!(
                path = %store.path().display(),
                "could not save notification history: {error}"
            );
        }
    }

    /// The user activated a retained notification. A live D-Bus sender gets
    /// `ActionInvoked` for the default action; a notification mirrored from
    /// another device routes that action back to exactly that device. An
    /// entry restored after a restart has no sender left, so the caller opens
    /// the verified app instead. Acted-on entries leave the history.
    pub fn activate_history_notification(&mut self, id: u64) -> HistoryActivation {
        let Some(notification) = self.notification_log.history_entry(id) else {
            return HistoryActivation::Missing;
        };
        let key = notification.default_action_key();
        let outcome =
            if let Some(request) = crate::shell::notification::scheduled_launch(notification) {
                self.notification_launch_requests.push(request);
                HistoryActivation::Invoked
            } else if notification.remote_route.is_some() {
                match key.and_then(|key| remote_action_command(notification, &key)) {
                    Some(command) => {
                        self.queue_remote_notification_command(command);
                        HistoryActivation::Invoked
                    }
                    None => HistoryActivation::Unavailable,
                }
            } else if let (Some(bridge), Some(dbus_id), Some(key)) = (
                self.notification_bridge.as_ref(),
                notification.sender_id,
                key,
            ) {
                bridge.note_action(dbus_id, key);
                HistoryActivation::Invoked
            } else if let Some(app_id) = notification.app_id.clone() {
                HistoryActivation::OpenApp {
                    label: tilo_desktop::notifications::group_label(notification),
                    app_id,
                }
            } else {
                HistoryActivation::Unavailable
            };
        if matches!(
            outcome,
            HistoryActivation::Invoked | HistoryActivation::OpenApp { .. }
        ) {
            self.notification_log.take_history(id);
            self.bump();
        }
        outcome
    }

    /// Remove one retained notification. Dismissing a mirrored notification
    /// here dismisses it on the device it came from too.
    pub fn dismiss_history_notification(&mut self, id: u64) -> bool {
        let Some(removed) = self.notification_log.take_history(id) else {
            return false;
        };
        if let Some(command) = remote_dismiss_command(&removed) {
            self.queue_remote_notification_command(command);
        }
        self.bump();
        true
    }

    /// Remove every retained notification of one app group.
    pub fn dismiss_history_group(&mut self, key: &str) -> usize {
        let ids = self.notification_log.group_ids(key);
        for id in &ids {
            self.dismiss_history_notification(*id);
        }
        ids.len()
    }

    /// The agent collaborator a retained notification could be handed to:
    /// the first agent admitted to the workspace the notification came from
    /// (or the active one, for shell and mirrored notifications). Whether
    /// that agent is live is the caller's question — only the collaboration
    /// broker knows.
    pub fn notification_agent_candidate(
        &self,
        notification: &Notification,
    ) -> Option<(WorkspaceId, crate::shell::state::RoomId, String)> {
        let tree = self.tree();
        let workspace = notification
            .source_workspace
            .and_then(|id| {
                tree.desktop
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id.0 == id)
            })
            .or_else(|| tree.desktop.active_workspace())?;
        let (agent, _) = workspace.agent_members().into_iter().next()?;
        Some((workspace.id, workspace.room, agent))
    }

    /// Hand a retained notification to an agent collaborator as a queued
    /// workspace task carrying the notification as a typed resource. Returns
    /// the task id, which the caller dispatches to the live agent.
    pub fn hand_history_notification_to_agent(
        &mut self,
        id: u64,
        workspace: WorkspaceId,
        agent: &str,
    ) -> Option<String> {
        let notification = self.notification_log.history_entry(id)?;
        let summary = if notification.body.is_empty() {
            notification.title.clone()
        } else {
            format!("{}: {}", notification.title, notification.body)
        };
        let title: String = summary.chars().take(240).collect();
        let resource = crate::shell::state::WorkspaceTaskResource {
            app_id: notification.app_id.clone(),
            app_label: notification.app_name.clone(),
            id: format!("notification:{}", uuid::Uuid::new_v4()),
            kind: "notification".to_string(),
            label: notification.title.clone(),
            agent_request: None,
        };
        let room = self
            .tree()
            .desktop
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .map(|candidate| candidate.room)?;
        let actor = self.local_user_for_room(&room);
        let task = self.tree_mut().create_workspace_task(
            workspace,
            title,
            actor,
            Some(agent.to_string()),
            crate::shell::state::WorkspaceTaskRoute::Model,
            Some(resource),
        )?;
        self.bump();
        Some(task)
    }

    // ---------- Do-Not-Disturb ----------

    /// The person's persisted Do Not Disturb switch
    /// ([`NotificationPolicy::do_not_disturb`]). The palette action, the
    /// Quick Settings tile and Personal Settings all edit this one value.
    ///
    /// [`NotificationPolicy::do_not_disturb`]: settings_store::notification_policy::NotificationPolicy::do_not_disturb
    pub fn dnd(&self) -> bool {
        self.notification_policy.do_not_disturb
    }

    /// No-op (no revision bump) when the value is unchanged. A live session
    /// (one with a notification bridge, which also reloads the policy) persists
    /// the change to the shared settings document; unit tests and offscreen
    /// captures never write the developer's real settings.
    pub fn set_dnd(&mut self, on: bool) {
        if self.notification_policy.do_not_disturb == on {
            return;
        }
        self.notification_policy.do_not_disturb = on;
        self.quick_settings.do_not_disturb = on;
        if self.notification_bridge.is_some()
            && !cfg!(test)
            && let Err(error) =
                settings_store::notification_policy::NotificationPolicy::save_do_not_disturb(on)
        {
            tracing::warn!(%error, "could not persist Do Not Disturb");
        }
        self.bump();
    }

    pub fn toggle_dnd(&mut self) -> bool {
        let on = !self.dnd();
        self.set_dnd(on);
        on
    }

    /// Mirror whether a wireless projection is live. No-op when unchanged.
    pub fn set_presenting(&mut self, presenting: bool) {
        if self.presenting != presenting {
            self.presenting = presenting;
            self.bump();
        }
    }

    /// Notifications are withheld automatically while the screen is seen by
    /// others: a live projection, or a surface shared with a collaborator.
    pub fn notifications_auto_quiet(&self) -> bool {
        self.presenting || !self.active_exposures.is_empty()
    }

    /// Whether fresh banners are held in history right now, for any reason
    /// that does not depend on the sender (a focus schedule can be pierced
    /// by priority apps, so it is applied per notification instead).
    pub fn notifications_quiet(&self) -> bool {
        self.dnd() || self.notifications_auto_quiet()
    }

    /// Whether the person's focus schedule is holding banners at this local
    /// time.
    pub fn focus_schedule_on(&self, local_minute: u16, weekday_from_monday: u8) -> bool {
        self.notification_policy
            .focus
            .active_until(local_minute, weekday_from_monday)
            .is_some()
    }

    /// Why banners are being held, for the status pill: Do Not Disturb, an
    /// automatic hold while presenting or sharing, or the focus schedule and
    /// when it ends. `None` when notifications show normally.
    pub fn quiet_status(&self, local_minute: u16, weekday_from_monday: u8) -> Option<String> {
        use crate::shell::i18n::{t, t_args};
        if self.dnd() {
            return Some(t("quiet-status-dnd"));
        }
        if self.notifications_auto_quiet() {
            return Some(if self.presenting {
                t("quiet-status-dnd-presenting")
            } else {
                t("quiet-status-dnd-sharing")
            });
        }
        self.notification_policy
            .focus
            .active_until(local_minute, weekday_from_monday)
            .map(|end| {
                let time = format!("{:02}:{:02}", end / 60, end % 60);
                t_args("quiet-status-focus-until", &[("time", time.into())])
            })
    }

    // ---------- D-Bus notification wiring ----------

    /// Once set, the toast lifecycle emits `NotificationClosed` /
    /// `ActionInvoked` events back to apps via the bridge.
    pub fn set_notification_bridge(&mut self, bridge: NotificationBridge) {
        self.notification_bridge = Some(bridge);
    }

    pub fn set_notification_runtime(
        &mut self,
        sounds: crate::shell::system_sounds::SystemSounds,
        priority_contacts: crate::priority_contacts::PriorityContactRegistry,
    ) {
        self.system_sounds = Some(sounds);
        self.priority_contacts = priority_contacts;
    }

    /// Play a system event sound, subject to the person's "System sounds"
    /// choices, Do Not Disturb, focus time and presenting. A session without
    /// a sound runtime (tests, renders) stays silent.
    pub fn play_system_sound(
        &self,
        event: settings_store::system_sounds::SystemSoundEvent,
    ) -> bool {
        self.play_system_sound_allowed(event, true)
    }

    fn play_system_sound_allowed(
        &self,
        event: settings_store::system_sounds::SystemSoundEvent,
        delivery_allows: bool,
    ) -> bool {
        self.system_sounds.as_ref().is_some_and(|sounds| {
            sounds.play(event, self.notifications_auto_quiet(), delivery_allows)
        })
    }

    /// Keep notification unit tests independent of the developer account's
    /// live policy document. Production always refreshes from the shared
    /// settings authority in `pump_dbus_notifications`.
    #[cfg(test)]
    pub(crate) fn set_notification_policy_for_tests(
        &mut self,
        policy: settings_store::notification_policy::NotificationPolicy,
    ) {
        self.quick_settings.do_not_disturb = policy.do_not_disturb;
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
    /// locked: the native loop drains it into [`SessionManager`](crate::compositor::idle::SessionManager).
    pub fn request_session_lock(&mut self) {
        self.session_lock_requested = true;
    }

    /// Consume one pending shell-originated lock request.  Kept private to
    /// the shell crate because only the compositor runtime may perform the
    /// session transition.
    pub fn take_session_lock_request(&mut self) -> bool {
        std::mem::take(&mut self.session_lock_requested)
    }

    /// Queue a typed session action for the overlay host's executor, which
    /// drains it on the next update.
    pub fn request_session_action(&mut self, action: os_actions::SessionAction) {
        self.session_action_requests.push(action);
    }

    pub fn take_session_action_requests(&mut self) -> Vec<os_actions::SessionAction> {
        std::mem::take(&mut self.session_action_requests)
    }

    pub(crate) fn take_notification_launch_requests(
        &mut self,
    ) -> Vec<crate::shell::notification::NotificationLaunch> {
        std::mem::take(&mut self.notification_launch_requests)
    }

    pub(crate) fn alarm_actions_available(&self) -> bool {
        self.notification_bridge
            .as_ref()
            .is_some_and(crate::notifications::NotificationBridge::scheduled_actions_available)
    }

    pub(crate) fn snoozed_alarms(&self, room: &str) -> Vec<crate::notifications::SnoozedAlarm> {
        self.notification_bridge
            .as_ref()
            .map_or_else(Vec::new, |bridge| bridge.snoozed_alarms(room))
    }

    fn alarm_action_capacity(&self) -> Result<(), String> {
        if self
            .notification_action_requests
            .len()
            .saturating_add(self.notification_action_results.len())
            >= 64
        {
            Err("too many pending alarm actions".into())
        } else {
            Ok(())
        }
    }

    fn alarm_target_authorized(
        &self,
        target: &tilo_desktop::notifications::ScheduledNotificationTarget,
        address: &str,
    ) -> bool {
        self.tree().desktop.workspaces.iter().any(|workspace| {
            if workspace.room.to_string() != target.room {
                return false;
            }
            let actor = workspace.actor_for_person(&self.local_user_for_room(&workspace.room));
            crate::shell::search::providers::actions::shell_action_authorized_for(
                workspace, &actor, address,
            )
        })
    }

    pub(crate) fn request_snoozed_alarm_dismiss(
        &mut self,
        source: &crate::notifications::SnoozedAlarm,
    ) -> Result<u64, String> {
        self.alarm_action_capacity()?;
        let address = crate::shell::notification::snoozed_alarm_dismiss_id(&source.target);
        if !source.target.valid()
            || !source.target.calendar_alarm
            || !self.alarm_target_authorized(&source.target, &address)
        {
            return Err("alarm's original workspace authority is unavailable".into());
        }
        let bridge = self
            .notification_bridge
            .as_ref()
            .ok_or("alarm service unavailable")?;
        let action = crate::notifications::ScheduledAction::Dismiss;
        let request = bridge.request_scheduled_action(
            source.app_id.clone(),
            source.target.clone(),
            action,
        )?;
        self.notification_action_requests
            .insert(request, (None, true, action));
        self.bump();
        Ok(request)
    }

    pub(crate) fn alarm_action_authorized(&self, notification: &Notification) -> bool {
        let Some(request) = crate::shell::notification::scheduled_launch(notification)
            .filter(|request| request.target.calendar_alarm)
        else {
            return false;
        };
        self.alarm_target_authorized(
            &request.target,
            &format!("notification.{}.snooze", notification.id),
        )
    }

    pub(crate) fn request_alarm_action(
        &mut self,
        id: u64,
        action: crate::notifications::ScheduledAction,
        retain_result: bool,
    ) -> Result<u64, String> {
        self.alarm_action_capacity()?;
        let notification = self
            .notification_log
            .active()
            .iter()
            .find(|notice| notice.id == id)
            .or_else(|| self.notification_log.history_entry(id))
            .ok_or("alarm notification is no longer available")?;
        let request = crate::shell::notification::scheduled_launch(notification)
            .filter(|request| request.target.calendar_alarm)
            .ok_or("notification is not a host calendar alarm")?;
        if !self.alarm_action_authorized(notification) {
            return Err("alarm's original workspace authority is unavailable".into());
        }
        let bridge = self
            .notification_bridge
            .as_ref()
            .ok_or("alarm service unavailable")?;
        let request_id = bridge.request_scheduled_action(request.app_id, request.target, action)?;
        self.notification_action_requests
            .insert(request_id, (Some(id), retain_result, action));
        self.bump();
        Ok(request_id)
    }

    pub(crate) fn take_alarm_action_result(&mut self, id: u64) -> Option<Result<(), String>> {
        self.notification_action_results.remove(&id)
    }

    fn finish_alarm_card_action(&mut self, id: u64, action: crate::notifications::ScheduledAction) {
        let removed = if let Some(removed) = self.notification_log.take_active(id) {
            self.emit_closed(&removed, CloseReason::Dismissed);
            Some(removed)
        } else {
            self.notification_log.take_history(id)
        };
        if action == crate::notifications::ScheduledAction::Snooze
            && let Some(mut notice) = removed
        {
            if let Some(target) = notice.scheduled_target.as_mut() {
                if !target.snoozed {
                    let status = tilo_desktop::notifications::alarm_snoozed_label();
                    notice.body = if notice.body.is_empty() {
                        status
                    } else {
                        format!("{}\n\n{status}", notice.body)
                    };
                }
                target.snoozed = true;
            }
            self.notification_log.retain(notice);
        }
    }

    fn pump_alarm_action_results(&mut self, bridge: &crate::notifications::NotificationBridge) {
        let requests: Vec<_> = self.notification_action_requests.keys().copied().collect();
        for request in requests {
            let Some(result) = bridge.take_scheduled_action_result(request) else {
                continue;
            };
            let Some((id, retain_result, action)) =
                self.notification_action_requests.remove(&request)
            else {
                continue;
            };
            if result.is_ok() {
                if let Some(id) = id {
                    self.finish_alarm_card_action(id, action);
                }
            } else if let Err(error) = &result
                && !retain_result
            {
                tracing::warn!(%error, "alarm notification action failed");
                self.push_warning_notification(
                    tilo_desktop::notifications::alarm_action_failed_label(),
                    tilo_desktop::notifications::alarm_action_retry_label(),
                );
            }
            if retain_result {
                self.notification_action_results.insert(request, result);
            }
            self.bump();
        }
    }

    /// Called once per frame; no-op when no bridge is installed.
    pub fn pump_dbus_notifications(&mut self) {
        use chrono::{Datelike, Timelike};
        let Some(bridge) = self.notification_bridge.clone() else {
            return;
        };
        self.pump_alarm_action_results(&bridge);
        let refresh_policy = self
            .notification_policy_checked
            .is_none_or(|checked| checked.elapsed() >= std::time::Duration::from_millis(750));
        if refresh_policy {
            let policy = settings_store::notification_policy::NotificationPolicy::load();
            if policy != self.notification_policy {
                self.quick_settings.do_not_disturb = policy.do_not_disturb;
                self.notification_policy = policy;
                self.bump();
            }
            self.notification_policy_checked = Some(std::time::Instant::now());
        }
        for inc in bridge.drain_incoming() {
            let dbus_id = inc.dbus_id;
            let scheduled_target = bridge.take_scheduled_target(dbus_id, inc.caller.as_ref());
            // A fresh Notify with an existing id supersedes the prior toast.
            self.notification_log
                .discard_active_where(|n| n.sender_id == Some(dbus_id));
            let verified_app_id = inc.caller.as_ref().map(|caller| caller.app_id.clone());
            let sharing = bridge.take_sharing(dbus_id);
            let urgency = inc.urgency;
            let badge_count = inc.badge_count;
            let sound = inc.sound;
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
            let mut notification = crate::shell::notification::from_incoming(inc);
            notification.scheduled_target = scheduled_target;
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
            if let Some(event) = sound {
                use settings_store::system_sounds::SystemSoundEvent;
                // Notifications and messages carry the notification policy's
                // per-app verdict; other events follow System sounds alone.
                let allows = !matches!(
                    event,
                    SystemSoundEvent::Notification | SystemSoundEvent::Message
                ) || effective.sounds;
                let _ = self.play_system_sound_allowed(event, allows);
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
                        crate::shell::notification::from_remote_continuity(remote, now_unix_ms)
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
        let replaced = self.notification_log.replace_remote(replacement);
        if replaced {
            self.bump();
        }
        replaced
    }

    fn retire_remote_notification(
        &mut self,
        source_device: &str,
        id: tilo_desktop::notifications::ContinuityNotificationId,
    ) {
        if self.notification_log.retire_remote(source_device, id) {
            self.bump();
        }
    }

    fn emit_closed(&self, removed: &Notification, reason: CloseReason) {
        if let (Some(bridge), Some(dbus_id)) =
            (self.notification_bridge.as_ref(), removed.sender_id)
        {
            bridge.note_closed(dbus_id, reason);
        }
    }
}

pub type Chrome = Rc<RefCell<ChromeState>>;

/// The workspace wheel's view of the shell: its summon flag lives in the
/// chrome state, and a switch or a new workspace lands in the tree with a
/// bump so the rest of the shell re-syncs on the next frame.
pub struct ChromeWheel(pub Chrome);

impl tilo_desktop::sidebar_wheel_disc::WheelHost for ChromeWheel {
    fn summon(&self) -> Option<vello::kurbo::Point> {
        self.0.borrow().wheel_summon()
    }

    fn dismiss_summon(&self) {
        self.0.borrow_mut().set_wheel_summon(None);
    }

    fn active_workspace(&self) -> Option<crate::shell::state::WorkspaceId> {
        Some(self.0.borrow().tree().desktop.active)
    }

    fn select_workspace(&self, workspace: crate::shell::state::WorkspaceId) {
        let mut state = self.0.borrow_mut();
        state.tree_mut().set_active(workspace);
        state.bump();
    }

    fn add_workspace(&self) {
        self.0.borrow_mut().add_workspace("Untitled");
    }
}

#[cfg(test)]
mod panel_exclusion_tests {
    use super::*;

    #[test]
    fn workspace_and_device_panels_are_exclusive_and_deep_links_reset_history() {
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

        workspace_and_device_settings_never_stack();
        quick_settings_deep_link_replaces_stale_navigation_history();
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
mod active_tab_tests {
    //! `active_app_title`/`active_tab_id` must return `None`, not
    //! `strips.first()`, when no real focus target is tracked.
    use super::*;
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{
        AppKind, GroupId, Strip, StripId, Tab, TileContent, WindowGroup as StateGroup,
        WindowWidth as StateWidth, Workspace, WorkspaceId, WorkspaceTree,
    };
    use std::collections::HashMap as StdMap;

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
            desktop: crate::shell::state::DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                active_strip,
                active_group,
                ..Default::default()
            },
            ..WorkspaceTree::default()
        }
    }

    #[test]
    fn active_tab_and_strip_lookups_follow_the_live_pointers_or_return_none() {
        fn active_app_title_follows_group_active_tab() {
            let handle = crate::shell::state::WorkspaceTreeHandle::new(tree_with(200));
            let chrome = ChromeState::new_with_handle(handle);
            let st = chrome.borrow();
            assert_eq!(st.active_app_title().as_deref(), Some("Second"));
        }

        fn active_tab_id_matches_the_active_tab_pointer() {
            let handle = crate::shell::state::WorkspaceTreeHandle::new(tree_with(100));
            let chrome = ChromeState::new_with_handle(handle);
            let st = chrome.borrow();
            assert_eq!(st.active_tab_id(), Some(100));
        }

        fn no_active_returns_none_instead_of_first_group() {
            let handle = crate::shell::state::WorkspaceTreeHandle::new(WorkspaceTree::default());
            let chrome = ChromeState::new_with_handle(handle);
            let st = chrome.borrow();
            assert_eq!(st.active_tab_id(), None);
            assert!(st.active_app_title().is_none());
        }

        fn without_active_strip_returns_none_not_first_strip() {
            let mut tree = tree_with(100);
            tree.desktop.active_strip.clear();
            let handle = crate::shell::state::WorkspaceTreeHandle::new(tree);
            let chrome = ChromeState::new_with_handle(handle);
            let st = chrome.borrow();
            assert_eq!(st.active_tab_id(), None);
        }

        fn switching_strips_moves_focus_and_clamps_at_the_ends() {
            let cell = ChromeState::new(WorkspaceTree::bootstrap_demo());
            let mut st = cell.borrow_mut();
            {
                let mut tree = st.tree_mut();
                let ws = tree.desktop.active_workspace_mut().expect("a workspace");
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

        fn focusing_a_strip_by_index_selects_that_strip() {
            let cell = ChromeState::new(WorkspaceTree::bootstrap_demo());
            let mut st = cell.borrow_mut();
            {
                let mut tree = st.tree_mut();
                let ws = tree.desktop.active_workspace_mut().expect("a workspace");
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

        active_app_title_follows_group_active_tab();
        active_tab_id_matches_the_active_tab_pointer();
        no_active_returns_none_instead_of_first_group();
        without_active_strip_returns_none_not_first_strip();
        switching_strips_moves_focus_and_clamps_at_the_ends();
        focusing_a_strip_by_index_selects_that_strip();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn focus_and_workspace_names_survive_restart_and_replicate() {
        fn focus_tab_and_strip_focus_survive_restart() {
            let dir = tempfile::tempdir().expect("state dir");
            let path = dir.path().join("workspace.json");
            let mut tree = tree_with(100);
            let workspace = tree.desktop.active;
            let first_strip = tree.desktop.workspaces[0].strips[0].id;
            let first_group = tree.desktop.workspaces[0].strips[0].window_groups[0].id;
            let second_strip = StripId::new();
            let second_group = 81;
            tree.desktop.workspaces[0].strips.push(Strip {
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
            tree.queue_save();
            let chrome = ChromeState::new(tree);

            chrome
                .borrow_mut()
                .focus_tab(workspace, first_strip, first_group, 200);
            let restarted = crate::shell::persist::load_tree_from(&path).expect("focus state");
            assert_eq!(
                restarted.desktop.active_group_for(first_strip),
                Some(first_group)
            );
            assert_eq!(
                restarted.desktop.workspaces[0].strips[0].window_groups[0].active_tab,
                200
            );

            assert!(chrome.borrow_mut().focus_strip_index(1));
            let restarted = crate::shell::persist::load_tree_from(&path).expect("strip focus");
            assert_eq!(
                restarted.desktop.active_strip.get(&workspace),
                Some(&second_strip)
            );
        }

        fn committed_workspace_name_survives_restart_and_replicates_to_a_room_peer() {
            let dir = tempfile::tempdir().expect("state dir");
            let path = dir.path().join("workspace.json");
            let mut tree = tree_with(100);
            let room = tree.desktop.workspaces[0].room;
            tree.persist_path = Some(path.clone());
            tree.adopt_ownerless_workspaces(&"shell-fixture".to_string());
            tree.queue_save();

            // Establish a real shared-room lineage before the rename. Merely
            // assigning the same RoomId to two independently-seeded documents
            // creates concurrent `name` values; that is not how a peer joins and
            // cannot prove which later edit should win.
            let mut replica = tree_with(100);
            let replica_workspace = replica.desktop.active;
            assert!(replica.set_room(replica_workspace, room));
            // The host's invite pins it (hub 1) as the room's governor.
            assert!(replica.pin_room_authority(room, crate::shell::state::PeerOrigin::hub(1)));
            let baseline = tree
                .export_room_changes_since(room, &[])
                .expect("host room baseline");
            replica
                .apply_room_changes(room, &baseline, &crate::shell::state::PeerOrigin::hub(1))
                .expect("guest joins host room");
            assert_eq!(replica.desktop.active_workspace().unwrap().name, "Home");
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
            assert_eq!(
                restarted.desktop.active_workspace().unwrap().name,
                "Project Aurora"
            );
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
            assert_eq!(replica.desktop.workspaces[0].name, "Project Aurora");
        }

        focus_tab_and_strip_focus_survive_restart();
        committed_workspace_name_survives_restart_and_replicates_to_a_room_peer();
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
            desktop: crate::shell::state::DesktopTree {
                workspaces: vec![ws],
                active: ws_id,
                active_strip,
                active_group,
                ..Default::default()
            },
            ..WorkspaceTree::default()
        };
        (tree, strip_id, group_id, tab_id)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn dropped_files_create_only_new_records_and_grant_the_real_app_id() {
        fn history_file_drop_creates_only_the_new_tabs_record() {
            let (tree, strip, group, existing) =
                tree_with_single_tab(TileContent::AppPlaceholder {
                    kind: AppKind::Other("Notes".into()),
                    label: "Existing".into(),
                });
            let chrome = ChromeState::new(tree);
            let mut state = chrome.borrow_mut();
            let first = state
                .handle_file_drop(std::path::Path::new("/missing/example.txt"))
                .unwrap();
            let second = state
                .handle_file_drop_at(std::path::Path::new("/missing/another.txt"), strip, group)
                .unwrap();
            for id in [first, second] {
                assert!(
                    state
                        .records()
                        .read(&ui::RecordId::for_tab(id), |_| ())
                        .is_some(),
                    "a successful drop owns its newly minted scratch record"
                );
            }
            assert!(
                state
                    .records()
                    .read(&ui::RecordId::for_tab(existing), |_| ())
                    .is_none(),
                "a drop never authorizes repairing a missing persisted sibling"
            );
        }

        /// Gap-2 proof: when the drop target is a real adopted Wayland client,
        /// the recorded grant is keyed by that client's actual
        /// `xdg_toplevel::set_app_id` value — not a re-derived display label.
        fn dropped_file_grant_keyed_by_foreign_tabs_real_wayland_app_id() {
            let (tree, strip_id, group_id, tab_id) = tree_with_single_tab(TileContent::Foreign {
                app_id: "org.gimp.GIMP".into(),
                surface_id: 77,
            });
            let ws_id = tree.desktop.active;
            let handle = crate::shell::state::WorkspaceTreeHandle::new(tree);
            let chrome = ChromeState::new_with_handle(handle);
            let st = chrome.borrow();

            let path = std::path::Path::new("/drop/photo.xcf");
            st.grant_dropped_file_access(path, strip_id, group_id, tab_id);

            let guard = st.tree();
            let ws = guard
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .unwrap();
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
        fn dropped_file_grant_keyed_by_builtin_canonical_app_id() {
            let (tree, strip_id, group_id, tab_id) =
                tree_with_single_tab(TileContent::AppPlaceholder {
                    kind: AppKind::Other("Editor".into()),
                    label: "notes.ts".into(),
                });
            let ws_id = tree.desktop.active;
            let handle = crate::shell::state::WorkspaceTreeHandle::new(tree);
            let chrome = ChromeState::new_with_handle(handle);
            let st = chrome.borrow();

            let path = std::path::Path::new("/drop/notes.ts");
            st.grant_dropped_file_access(path, strip_id, group_id, tab_id);

            let guard = st.tree();
            let ws = guard
                .desktop
                .workspaces
                .iter()
                .find(|w| w.id == ws_id)
                .unwrap();
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

        history_file_drop_creates_only_the_new_tabs_record();
        dropped_file_grant_keyed_by_foreign_tabs_real_wayland_app_id();
        dropped_file_grant_keyed_by_builtin_canonical_app_id();
    }

    #[test]
    fn chrome_invalidation_and_exposure_snapshots_are_edge_triggered() {
        fn broker_tree_epoch_invalidates_chrome_exactly_once() {
            let cell = ChromeState::new(WorkspaceTree::default());
            let mut state = cell.borrow_mut();
            let initial = state.revision;
            assert!(!state.sync_external_tree_revision());
            state.tree_mut().desktop.external_revision = 1;
            assert!(state.sync_external_tree_revision());
            assert_eq!(state.revision, initial.wrapping_add(1));
            assert!(!state.sync_external_tree_revision());
            assert_eq!(state.revision, initial.wrapping_add(1));
        }

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

        broker_tree_epoch_invalidates_chrome_exactly_once();
        active_exposure_snapshot_is_sorted_deduplicated_and_edge_triggered();
    }

    #[test]
    fn acting_as_a_principal_pins_every_room_to_it() {
        let tree = tree_with(100);
        let room = tree.desktop.workspaces[0].room;
        let chrome = ChromeState::new_with_fixture_handle(
            tree.shared(),
            crate::shell::hosted_inputs::HostedTileInputs::fixture()
                .expect("hosted fixture inputs"),
        );
        chrome.borrow_mut().act_as("renderer".into());
        let state = chrome.borrow();
        assert_eq!(state.local_user_id, "renderer");
        assert_eq!(state.local_user_for_room(&room), "renderer");
        assert_eq!(state.active_local_user(), "renderer");
    }
}

#[cfg(test)]
mod provisioned_identity_tests {
    use super::*;
    use crate::shell::state::{CapRole, Workspace, WorkspaceId};

    #[test]
    fn first_login_setup_hands_account_owned_workspaces_to_the_new_identity() {
        let mut tree = WorkspaceTree::default();
        let mut workspace = Workspace::new(WorkspaceId(5), "Home");
        workspace.set_owner("quickuser".to_string());
        let room = workspace.room;
        tree.desktop.workspaces.push(workspace);
        let chrome = ChromeState::new(tree);
        let person = "ab".repeat(32);
        chrome.borrow_mut().act_as(person.clone());

        assert_eq!(
            chrome.borrow_mut().adopt_provisioned_identity("quickuser"),
            1
        );

        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree
            .desktop
            .workspaces
            .iter()
            .find(|w| w.room == room)
            .expect("workspace");
        assert_eq!(workspace.owner.as_deref(), Some(person.as_str()));
        assert_eq!(workspace.role_of_at(&person, 0), Some(CapRole::Owner));
        assert_eq!(workspace.role_of_at(&"quickuser".to_string(), 0), None);
        drop(tree);
        drop(state);
        assert_eq!(
            chrome.borrow_mut().adopt_provisioned_identity("quickuser"),
            0
        );
    }
}
