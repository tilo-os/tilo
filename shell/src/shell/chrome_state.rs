//! Shared chrome state for the nirios shell root.
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
    reason = "each field is an independent, non-exclusive overlay visibility toggle (any subset can be on screen at once), and the struct is read/written across ~170 call sites in 14 files under shell/src and shell/tests — a flags-enum refactor would ripple invasively for no behavioural gain"
)]
pub struct OverlayFlags {
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
pub struct QuickSettingsState {
    /// 0.0..=1.0.
    pub brightness: f32,
    /// 0.0..=1.0.
    pub volume: f32,
    pub wifi: bool,
    pub bluetooth: bool,
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

impl Default for QuickSettingsState {
    fn default() -> Self {
        Self {
            brightness: 0.6,
            volume: 0.4,
            wifi: true,
            bluetooth: false,
            wired: None,
            battery: os_actions::BatteryInfo::from_root(std::path::Path::new(
                "/nonexistent/qos-power-supply",
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
            bt_devices: Vec::new(),
            wifi_password: None,
            power_profile: PowerProfile::Balanced,
        }
    }
}

impl QuickSettingsState {
    /// The renderer's dispatch key. Defaults to `Root` if the stack is
    /// somehow empty (`new()` always seeds it).
    pub fn current_level(&self) -> &QsLevel {
        self.nav_stack.last().unwrap_or(&QsLevel::Root)
    }

    pub fn push_level(&mut self, level: QsLevel) {
        self.nav_stack.push(level);
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
pub struct ChromeState {
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
    /// Set by the tab strip's "+", cleared when the palette closes.
    pub open_target: OpenTarget,
    /// Copied here once per frame from
    /// [`crate::shell::status::StatusBridge`] with no lock held across
    /// a draw. `None` until a poller publishes.
    pub status: crate::shell::status::SystemStatus,
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
    /// Id of the group full-screened via `xdg_toplevel.set_fullscreen`.
    /// NOT persisted — the compositor sets it from live state each
    /// frame via [`Self::set_fullscreen_group`].
    fullscreen_group: Option<u64>,
    /// Id of the group put full screen with the tab's Expand chip. Kept
    /// separate from [`Self::fullscreen_group`], which the compositor
    /// rewrites every frame — a value parked there by Expand would be
    /// wiped on the next tick. Both feed [`Self::is_fullscreen`].
    maximized_group: Option<u64>,
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
    /// An exported action asked into the current room, as `(record
    /// id, record kind)`. Queued because the room's index lives on
    /// the overlay host, unreachable from an action's `apply`.
    record_requests: Vec<(String, String)>,
    /// Docked at the strip's left edge by default; `Some(point)`
    /// detaches the wheel to the pointer. Cleared on Escape or
    /// picking a workspace.
    wheel_summon: Option<vello::kurbo::Point>,
    /// Last layout pass, so placement can ask "which border can the
    /// user actually see" without reaching into the widget tree.
    strip_viewport: Option<crate::shell::placement::StripViewport>,
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
        Rc::new(RefCell::new(Self {
            tree_handle,
            overlays: OverlayFlags::default(),
            quick_settings: QuickSettingsState::default(),
            editing_workspace_name: None,
            editing_workspace_name_selection: ui::text_selection::Selection::default(),
            search_scope: SearchScope::default(),
            open_target: OpenTarget::default(),
            status: crate::shell::status::SystemStatus::default(),
            osd: None,
            ws_settings_tab: WsTab::default(),
            workspace_settings_anchor: vello::kurbo::Rect::ZERO,
            ws_grant_draft: crate::shell::workspace_settings::GrantDraft::default(),
            revision: 0,
            pending_grants: VecDeque::new(),
            test_only_owner_override: None,
            local_user_id: crate::shell::state::local_user_id(),
            notifications: Vec::new(),
            notification_bridge: None,
            notification_history: Vec::new(),
            notification_badges: BTreeMap::new(),
            notification_sound: None,
            priority_contacts: crate::priority_contacts::PriorityContactRegistry::in_memory(),
            notification_policy: settings_store::notification_policy::NotificationPolicy::default(),
            notification_policy_checked: None,
            dnd: false,
            visual_alert_until: None,
            session_locked: false,
            fullscreen_group: None,
            maximized_group: None,
            peer_sync_enabled: false,
            collab_unavailable: None,
            peer_sync_requests: Vec::new(),
            wheel_summon: None,
            record_requests: Vec::new(),
            strip_viewport: None,
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

    pub fn push_peer_sync_request(&mut self, request: crate::shell::overlay_host::PeerSyncAction) {
        self.peer_sync_requests.push(request);
    }

    pub fn take_peer_sync_requests(&mut self) -> Vec<crate::shell::overlay_host::PeerSyncAction> {
        std::mem::take(&mut self.peer_sync_requests)
    }

    /// Same effect as activating that record's row in search — a
    /// reference is added to this room's index, no bytes move.
    pub fn push_record_request(&mut self, id: impl Into<String>, kind: impl Into<String>) {
        self.record_requests.push((id.into(), kind.into()));
    }

    pub fn take_record_requests(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.record_requests)
    }

    pub fn fullscreen_group(&self) -> Option<u64> {
        self.fullscreen_group
    }

    /// Either a client-requested fullscreen or a group the user
    /// expanded with the tab's Expand chip — both suppress the top bar
    /// and sidebar wheel.
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen_group.is_some() || self.maximized_group.is_some()
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

    /// Person principal this session presents inside `room`.
    pub fn local_user_for_room(
        &self,
        room: &crate::shell::state::RoomId,
    ) -> crate::shell::state::UserId {
        crate::shell::state::local_user_id_for_room(room)
    }

    /// Person principal selected for the active workspace, or the session
    /// fallback when no workspace is active.
    pub fn active_local_user(&self) -> crate::shell::state::UserId {
        self.tree()
            .active_workspace()
            .map(|workspace| self.local_user_for_room(&workspace.room))
            .unwrap_or_else(|| self.local_user_id.clone())
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

    /// Empty buffers are a cancel — never overwrite the name with "".
    pub fn commit_workspace_name(&mut self) {
        if let Some(buf) = self.editing_workspace_name.take() {
            let trimmed = buf.trim();
            if !trimmed.is_empty()
                && let Some(ws) = self.tree_mut().active_workspace_mut()
            {
                ws.name = trimmed.to_string();
            }
            self.bump();
        }
    }

    pub fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Reveal one authoritative workspace-settings destination. All shell
    /// entry points use this instead of independently coordinating a tab and
    /// overlay. The caller owns the ordinary action-level repaint boundary.
    pub fn open_workspace_settings(&mut self, tab: WsTab) {
        self.ws_settings_tab = tab;
        self.overlays.ws_settings = true;
    }

    /// The top-bar workspace capsule is both the context indicator and the
    /// close affordance, so it alone needs toggle semantics. Contextual links
    /// always call [`Self::open_workspace_settings`] and can never close an
    /// already-open destination by accident.
    pub fn toggle_workspace_settings(&mut self) {
        self.overlays.ws_settings = !self.overlays.ws_settings;
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
            // A peer window carries an app id in the same namespace
            // `AppKind::app_id` maps the others into.
            TileContent::Foreign { app_id, .. } | TileContent::PeerWindow { app_id, .. } => {
                app_id.clone()
            }
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
            tree.adopt_ownerless_workspaces_with(crate::shell::state::local_user_id_for_room);
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
            self.emit_closed(&removed, CloseReason::Dismissed);
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
        if let (Some(bridge), Some(dbus_id)) = (self.notification_bridge.as_ref(), removed.dbus_id)
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
        if !self.notification_history.is_empty() {
            self.notification_history.clear();
            self.bump();
        }
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
            grants.contains_key("org.nirios.Editor"),
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
}
