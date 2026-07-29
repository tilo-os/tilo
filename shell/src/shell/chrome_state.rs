//! Shared chrome state for the nirios shell root.
//!
//! Chrome widgets (TopBarChrome, SidebarWheelChrome, ShellOverlayHost)
//! live at sibling positions in the widget tree and need to mutate the
//! same set of fields — overlay visibility flags and the workspace tree
//! itself. This module packages that state into one `Rc<RefCell<_>>`
//! handle so each widget can hold a clone and read/write without routing
//! every interaction through the tree's root.
//!
//! This is the shell's equivalent of the Elm-style `Model` that widgets
//! mutate via events; keeping it out of the Automerge doc is deliberate
//! — overlay visibility is local UI state that should not sync to peers,
//! and the WorkspaceTree already has its own JSON persistence layer.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use parking_lot::{RwLockReadGuard, RwLockWriteGuard};

use crate::notifications::{CloseReason, NotificationBridge};
use crate::shell::notification::{
    MAX_HISTORY, MAX_NOTIFICATIONS, Notification, NotificationIconKind, NotificationSeverity,
};
use crate::shell::quick_settings::{BtDevice, PowerProfile, QsLevel, WifiNetwork};
use crate::shell::state::{
    AppId, Capability, GroupId, StripId, WorkspaceTree, WorkspaceTreeHandle,
};
// Re-export WsTab so chrome users can refer to the tab routing enum
// without reaching into `workspace_settings`. The enum itself lives
// next to the modal widget; the chrome holds a copy of the active tab
// as a typed field — see `ChromeState::ws_settings_tab`.
pub use crate::shell::workspace_settings::WsTab;

/// Snapshot of which shell overlays are currently on screen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverlayFlags {
    pub overview: bool,
    pub quick_settings: bool,
    pub ws_settings: bool,
    /// Collaborator dropdown, opened by clicking the avatar stack in the
    /// top bar's right cluster. Lists every peer by name so invites and
    /// session handoff have a visible surface; pure UI state — the roster
    /// itself lives on `WorkspaceTree::collaborators`.
    pub collaborators: bool,
    /// Room-activity sheet — the workspace's own change history, by everyone.
    /// Opened from the command palette or from the foot of the collaborator
    /// dropdown. Pure UI state; the history it shows is derived from
    /// `WorkspaceTree::doc`, which is shared and replicated.
    pub room_activity: bool,
}

/// Global-search scope. Mirrors the concept's "context banner" scope
/// radios; defaults to `Workspace` so a bare open (e.g. clicking the
/// scope pill without a chord) stays scoped to the active workspace
/// unless the user explicitly widens or narrows.
///
/// The scope is seeded by which chord opened the palette:
/// - `Ctrl+K` → [`SearchScope::ActiveApp`] (the focused app's own
///   resources and actions — "search in what I am looking at")
/// - `Super+K` / `Super+Space` / `Alt+Space` → [`SearchScope::All`]
///   (every app plus full-text over the filesystem — "search the
///   machine", and the only scope that offers to launch something you
///   do not already have open)
///
/// Seeding a narrowing scope is safe because it degrades rather than
/// emptying: when no provider claims the focused tab — a foreign
/// Wayland surface, say — [`SearchScope::ActiveApp`] allows every
/// provider, and with no tab focused at all the palette widens to
/// [`SearchScope::All`]. The pill labels the narrow case `Only <App>`,
/// so a short result list is attributable to the scope rather than read
/// as a broken search.
///
/// The `Alt+Space` binding is a nested-friendly Super substitute: when
/// the compositor runs as a Wayland CLIENT the host DE (GNOME etc.)
/// swallows Super, so a chord that doesn't touch Super is needed to
/// preserve global-search reachability inside a dev window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchScope {
    /// Scoped to the currently focused tab's content. Seeded by
    /// `Ctrl+K` and by the focused group's search affordance.
    ActiveApp,
    #[default]
    Workspace,
    All,
}

impl SearchScope {
    /// Short label shown inside the scope pill. `ActiveApp`'s label
    /// depends on which tab is focused, so callers render that variant
    /// with the tab's title instead of this static string.
    pub fn label(self) -> &'static str {
        match self {
            SearchScope::ActiveApp => "Only this app",
            SearchScope::Workspace => "Workspace",
            SearchScope::All => "All",
        }
    }

    /// Whether this scope can hide results a wider scope would show.
    /// The pill renders a narrowing scope with an explicit `Only …`
    /// prefix so an empty result list is attributable to the scope
    /// rather than to the query.
    pub fn narrows_results(self) -> bool {
        matches!(self, SearchScope::ActiveApp)
    }
}

/// Quick-settings tray values. Local UI state; does not sync to peers —
/// device settings like brightness, volume, and radio toggles are always
/// per-device, never shared.
#[derive(Clone, Debug, PartialEq)]
pub struct QuickSettingsState {
    /// Brightness level, 0.0..=1.0.
    pub brightness: f32,
    /// Volume level, 0.0..=1.0.
    pub volume: f32,
    pub wifi: bool,
    pub bluetooth: bool,
    /// Navigation stack for the popover. The renderer dispatches on
    /// `nav_stack.last()` (always at least `Root`) — pushing `Wifi` or
    /// `Bluetooth` flips the popover body to the drill-in list.
    /// Concept ref: `QuickSettings.tsx:108-128, 101-162`.
    pub nav_stack: Vec<QsLevel>,
    /// Demo WiFi networks rendered in the WiFi drill-in list. Mock data
    /// until real scanning lands; concept ref: `QuickSettings.tsx:108-128`.
    pub wifi_networks: Vec<WifiNetwork>,
    /// Demo Bluetooth devices rendered in the BT drill-in list. Mock data
    /// until real pairing lands; concept ref: `QuickSettings.tsx:140-160`.
    pub bt_devices: Vec<BtDevice>,
    /// Active power profile, mirrored from `powerprofilesctl` when the
    /// panel opens and pushed back via `set_power_profile` when the
    /// user cycles the chip. Defaults to `Balanced` so a host without
    /// `power-profiles-daemon` still renders a sensible label.
    pub power_profile: PowerProfile,
}

impl Default for QuickSettingsState {
    fn default() -> Self {
        Self {
            brightness: 0.6,
            volume: 0.4,
            wifi: true,
            bluetooth: false,
            nav_stack: vec![QsLevel::Root],
            // Empty until `refresh_quick_settings_from_os` runs — nmcli /
            // bluetoothctl populate live lists on the popover's rising
            // edge. Starting empty keeps the "no fake content" contract:
            // a fresh boot never shows a network it isn't actually seeing.
            wifi_networks: Vec::new(),
            bt_devices: Vec::new(),
            power_profile: PowerProfile::Balanced,
        }
    }
}

impl QuickSettingsState {
    /// Currently visible level — the renderer's dispatch key. Defaults to
    /// `Root` if the stack is somehow empty (`new()` always seeds it).
    pub fn current_level(&self) -> &QsLevel {
        self.nav_stack.last().unwrap_or(&QsLevel::Root)
    }

    /// Push a new level onto the nav stack. Used by the toggle-card
    /// chevron-affordance click in `draw_quick_settings`.
    pub fn push_level(&mut self, level: QsLevel) {
        self.nav_stack.push(level);
    }

    /// Pop the top level. No-op when only `Root` remains so the popover
    /// can't be emptied.
    pub fn pop_level(&mut self) {
        if self.nav_stack.len() > 1 {
            self.nav_stack.pop();
        }
    }
}

/// Mutable chrome state shared across the shell widget tree.
#[derive(Debug)]
pub struct ChromeState {
    /// Shared workspace tree handle. The portal (grants) and peer-sync
    /// (replica) hold clones of the same `Arc<RwLock<_>>` so every
    /// mutation is observed across subsystems. See C-3 RFC at
    /// `docs/superpowers/specs/2026-05-22-workspace-tree-arc-mutex-design.md`.
    ///
    /// Field is named `tree_handle` (not `tree`) so unmigrated call-sites
    /// that still read `chrome.tree.workspaces.iter()` get a clear
    /// type-mismatch error rather than silently borrowing the wrong
    /// thing. Use [`Self::tree`] / [`Self::tree_mut`] for guarded access.
    pub tree_handle: WorkspaceTreeHandle,
    pub overlays: OverlayFlags,
    pub quick_settings: QuickSettingsState,
    /// When `Some`, the workspace-settings modal's Name input is in text-
    /// entry mode. The string is the edit buffer (may diverge from the
    /// active workspace's persisted name until the user commits with
    /// Enter). Cleared on commit or Esc.
    pub editing_workspace_name: Option<String>,
    /// Caret + selection inside `editing_workspace_name`. Only
    /// meaningful while the option is `Some(_)` — when no edit is
    /// in flight this is a collapsed `Selection::default()` placeholder.
    /// `begin_workspace_name_edit` seeds it to a collapsed selection
    /// at the buffer's end so typing immediately appends; the overlay
    /// host's keyboard handler mutates it on arrow / shift / backspace
    /// / Ctrl-chord events.
    pub editing_workspace_name_selection: ui::text_selection::Selection,
    /// Active global-search scope. The overlay host paints a scope-pill
    /// banner above the palette, and clicking a pill rotates through the
    /// variants of [`SearchScope`].
    pub search_scope: SearchScope,
    /// Currently-active tab inside the Workspace Settings modal.
    /// Independent of [`Self::editing_workspace_name`] (which is only
    /// the name-input edit buffer). `Default::default()` is
    /// [`WsTab::General`]; the renderer dispatches its modal-body
    /// content on this field.
    ///
    /// Concept ref: `WorkspaceSettingsModal.tsx:50-95` — the tabs are
    /// independent UI state, never entangled with the name-edit buffer.
    pub ws_settings_tab: WsTab,
    /// Monotonic counter bumped every time the tree is mutated. Widgets
    /// poll this in `update` to re-sync their caches; cheaper than running
    /// diff logic and easier than an event bus.
    pub revision: u64,
    /// Queue of pending capability grants the user must respond to.
    /// Phase 4 Task 4.3 (the portal D-Bus backend) pushes onto the back
    /// of this queue when a sandboxed app calls one of the portal's
    /// request methods; the overlay host renders a [`PermissionPrompt`]
    /// for `front()` and drains the entry once the user clicks an
    /// action button. v1 stores pairs of `(app_id, capability)` —
    /// the capability's `scope` is set by the portal layer before
    /// pushing (typically `GRANT_ONCE`, since the prompt
    /// itself escalates to `Session`/`Always` based on the user's
    /// click). Empty by default.
    pub pending_grants: VecDeque<(AppId, Capability)>,
    /// Test-only override for the `is_active_workspace_owner` helper
    /// in `overlay_host.rs`. `None` (the default) keeps production
    /// behaviour: the helper returns `true` unconditionally because
    /// the shell has no logged-in-user identity yet (Sandbox 4.2's
    /// stub). `Some(false)` flips the helper to non-owner so the
    /// Sandbox Phase 6 Permissions tab tests can exercise the
    /// disabled-Revoke code path. Real ownership tracking will
    /// replace this field once the peer-sync work lands a
    /// `local_user_id` + `Workspace::owner_id` pair.
    pub test_only_owner_override: Option<bool>,
    /// Who is sitting at this machine.
    ///
    /// Every permission decision the compositor makes on a person's
    /// behalf is made for *this* id: it is what
    /// [`crate::shell::overlay_host`] compares against a workspace's
    /// owner, and the subject of any capability the local user grants.
    /// Seeded at boot from the device identity
    /// ([`crate::shell::state::local_user_id`]).
    pub local_user_id: crate::shell::state::UserId,
    /// Active toast / banner notifications. Ordered oldest-first so the
    /// stack widget paints from the top of the screen downward in the
    /// same order entries were pushed. Capped at
    /// [`MAX_NOTIFICATIONS`]; pushing past the cap evicts the oldest
    /// entry (FIFO drop). Any subsystem can append via
    /// [`Self::push_notification`] / [`Self::push_notification_full`];
    /// the [`crate::shell::notification::NotificationStack`] widget
    /// reads from here and drives expiry via
    /// [`Self::update_notification_lifecycle`] each frame.
    pub notifications: Vec<Notification>,
    /// Bridge to the `org.freedesktop.Notifications` D-Bus service
    /// (Track D1). `None` until the compositor wires it up
    /// ([`Self::set_notification_bridge`]); tests and the offscreen
    /// `shots` path run without it. When present, toast lifecycle
    /// transitions (dismiss / expire / close / action) emit the
    /// corresponding D-Bus signal back to the originating app via the
    /// bridge's reverse-event queues.
    notification_bridge: Option<NotificationBridge>,
    /// Notification history — the message-log the notification centre
    /// reads. Ordered oldest-first. Every notification that leaves the
    /// active stack (dismissed / expired / closed / cap-evicted) is
    /// appended here, as is every notification suppressed while
    /// [`Self::dnd`] is on. Bounded to [`MAX_HISTORY`] entries (oldest
    /// dropped from the front). Distinct from [`Self::notifications`]:
    /// active toasts are *currently on screen*; history is what has
    /// already come and gone (plus anything Do-Not-Disturb held back).
    notification_history: Vec<Notification>,
    /// Do-Not-Disturb. While `true`, freshly posted notifications are
    /// **not** shown as banners — they go straight into
    /// [`Self::notification_history`] instead — so the display surface
    /// stays quiet but nothing is lost. The compositor owns this flag
    /// (OS-owned chrome); the user toggles it via the command palette /
    /// quick settings. Independent of [`Self::session_locked`], which
    /// suppresses *content* on the lock screen but still shows a count.
    dnd: bool,
    /// Whether the session is currently locked (mirrored from the
    /// compositor's `SessionManager` each frame). Drives notification
    /// lock-suppression: while `true`, the notification stack withholds
    /// app names / summaries / bodies and shows only a count.
    session_locked: bool,
    /// **Transient fullscreen surface** — the id of the window group that has
    /// requested `xdg_toplevel.set_fullscreen` and is currently promoted to
    /// occupy the whole output. `None` in the normal tiled state. NOT
    /// persisted (a live presentation must never survive a reboot) — the
    /// compositor sets it from its live fullscreen state each frame via
    /// [`Self::set_fullscreen_group`]. The top bar reads it to suppress
    /// itself; the overlay host reads it to promote the group and hide the
    /// sidebar + other strips. Clearing it restores the exact prior layout.
    fullscreen_group: Option<u64>,
    /// **User-expanded window group** — the id of the group the user put full
    /// screen with the tab's Expand chip. Deliberately a *separate* field from
    /// [`Self::fullscreen_group`]: the compositor rewrites that one every frame
    /// from its live client-fullscreen state (normally `None`), so a value
    /// parked there by the Expand harvest would be wiped on the next tick.
    ///
    /// Both fields feed [`Self::is_fullscreen`], which is the single lever the
    /// top bar and the sidebar wheel gate their suppression on — so an
    /// expanded window covers the whole output (wheel included) exactly like a
    /// client-driven fullscreen surface does.
    maximized_group: Option<u64>,
}

impl ChromeState {
    /// Back-compat constructor: takes an owned [`WorkspaceTree`] and
    /// wraps it in a fresh [`WorkspaceTreeHandle`]. Existing call-sites
    /// (every test fixture in `quick-shell/tests/*`) keep this shape;
    /// production callers that need to share the handle with portal /
    /// peer-sync go through [`Self::new_with_handle`] instead.
    pub fn new(tree: WorkspaceTree) -> Rc<RefCell<Self>> {
        Self::new_with_handle(tree.shared())
    }

    /// Construct a [`ChromeState`] sharing an existing
    /// [`WorkspaceTreeHandle`]. Used by the hosted entry point so the
    /// portal's `GrantStoreHandle` and peer-sync's
    /// `WorkspaceTreeReplica` mutate the same underlying tree as the
    /// shell chrome — closes audit finding C-3.
    pub fn new_with_handle(tree_handle: WorkspaceTreeHandle) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            tree_handle,
            overlays: OverlayFlags::default(),
            quick_settings: QuickSettingsState::default(),
            editing_workspace_name: None,
            editing_workspace_name_selection: ui::text_selection::Selection::default(),
            search_scope: SearchScope::default(),
            ws_settings_tab: WsTab::default(),
            revision: 0,
            pending_grants: VecDeque::new(),
            test_only_owner_override: None,
            local_user_id: crate::shell::state::local_user_id(),
            notifications: Vec::new(),
            notification_bridge: None,
            notification_history: Vec::new(),
            dnd: false,
            session_locked: false,
            fullscreen_group: None,
            maximized_group: None,
        }))
    }

    /// The window group currently promoted to full-output (a surface that
    /// requested `xdg_toplevel.set_fullscreen`), or `None` in the normal tiled
    /// state.
    pub fn fullscreen_group(&self) -> Option<u64> {
        self.fullscreen_group
    }

    /// Whether anything currently holds the output fullscreen — either a
    /// client that requested `xdg_toplevel.set_fullscreen`, or a group the
    /// user expanded with the tab's Expand chip. The top bar and the sidebar
    /// wheel both suppress themselves on this, so either route genuinely
    /// covers the whole output.
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen_group.is_some() || self.maximized_group.is_some()
    }

    /// The group the user expanded to full screen, if any. See
    /// [`Self::maximized_group`]'s field docs for why this is distinct from
    /// [`Self::fullscreen_group`].
    pub fn maximized_group(&self) -> Option<u64> {
        self.maximized_group
    }

    /// Set (or clear) the user-expanded group and bump the revision so the top
    /// bar / overlay host re-sync their presentation next frame. Idempotent.
    pub fn set_maximized_group(&mut self, group: Option<u64>) {
        if self.maximized_group != group {
            self.maximized_group = group;
            self.bump();
        }
    }

    /// Set (or clear) the transient fullscreen surface and bump the revision so
    /// the top bar / overlay host re-sync their fullscreen presentation on the
    /// next frame. Idempotent — a no-op when the value is unchanged, so it's
    /// cheap to call every frame from the compositor's fullscreen reconcile.
    pub fn set_fullscreen_group(&mut self, group: Option<u64>) {
        if self.fullscreen_group != group {
            self.fullscreen_group = group;
            self.bump();
        }
    }

    /// Read-guard accessor for the shared workspace tree. The returned
    /// guard derefs to `&WorkspaceTree`, so existing patterns like
    /// `state.tree().workspaces.iter()` work verbatim. Cheap (~10 ns
    /// uncontended on `parking_lot::RwLock`).
    pub fn tree(&self) -> RwLockReadGuard<'_, WorkspaceTree> {
        self.tree_handle.read()
    }

    /// Write-guard accessor for the shared workspace tree. Used by
    /// mutating helpers (`append_empty_workspace`, `set_active`, etc.)
    /// — the guard is dropped at the end of the borrow scope, so
    /// concurrent readers (paint, portal lookup, peer-sync export)
    /// unblock as soon as the mutation lands.
    pub fn tree_mut(&self) -> RwLockWriteGuard<'_, WorkspaceTree> {
        self.tree_handle.write()
    }

    /// Test-only / observability accessor for the underlying shared
    /// handle. Tests use this to assert that portal mutations land on
    /// the same instance the chrome reads from (see
    /// `grant_inserted_via_portal_is_visible_in_ws_settings`).
    pub fn tree_handle(&self) -> WorkspaceTreeHandle {
        self.tree_handle.clone()
    }

    /// Commit the current `editing_workspace_name` buffer (if any) to the
    /// active workspace's name and clear the buffer. Empty buffers are
    /// treated as a cancel — we never overwrite the name with "".
    pub fn commit_workspace_name(&mut self) {
        if let Some(buf) = self.editing_workspace_name.take() {
            let trimmed = buf.trim();
            if !trimmed.is_empty() {
                if let Some(ws) = self.tree_mut().active_workspace_mut() {
                    ws.name = trimmed.to_string();
                }
            }
            self.bump();
        }
    }

    pub fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Route a winit `WindowEvent::DroppedFile` payload into the active
    /// workspace's first strip's first window group via
    /// [`super::workspace_drop::apply_workspace_drop_from_path`]. Bumps
    /// the revision counter so widgets re-sync on the next frame.
    /// Returns the new tab id, or `None` if the path has no filename.
    pub fn handle_file_drop(&mut self, path: &std::path::Path) -> Option<u64> {
        let tab_id = {
            let mut tree = self.tree_mut();
            super::workspace_drop::apply_workspace_drop_from_path(&mut *tree, path)
        };
        if tab_id.is_some() {
            self.bump();
        }
        tab_id
    }

    /// Cursor-targeted file drop: splice the dropped file's app into the
    /// specific `(strip_id, group_id)` the pointer was over at drop time,
    /// then record a per-workspace filesystem grant so the target app can
    /// actually read the file. This is the production
    /// `WindowEvent::DroppedFile` path — the overlay host resolves the
    /// group under the cursor via the same hit-test used for click-focus
    /// (`overlay_host::pick_group_at`) and hands the winner here.
    ///
    /// Returns the new tab id, or `None` when the target is stale / the
    /// path has no filename — the caller treats `None` as a defined
    /// no-op (an empty-area drop opens nothing rather than dumping the
    /// file into an arbitrary window). No grant is recorded on `None`.
    pub fn handle_file_drop_at(
        &mut self,
        path: &std::path::Path,
        strip_id: StripId,
        group_id: GroupId,
    ) -> Option<u64> {
        let tab_id = {
            let mut tree = self.tree_mut();
            super::workspace_drop::apply_workspace_drop_at_from_path(
                &mut *tree, path, strip_id, group_id,
            )
        };
        if tab_id.is_some() {
            // A dropped file is an explicit capability grant to the app
            // the user aimed at — record it before the widget re-syncs.
            self.grant_dropped_file_access(path);
            self.bump();
        }
        tab_id
    }

    /// Record the dropped file as a per-workspace filesystem capability
    /// for the app that will open it, honouring the sandbox: the grant
    /// goes through the SAME [`GrantStoreHandle`] the portal / permission
    /// prompt uses, so `sandbox::profile::apply_grants` picks it up when
    /// the app spawns. Keyed by the built-in app's name (the same
    /// vocabulary the persisted tile uses) so `Files` / `Editor` drops
    /// each accrue their own allowlist entry.
    ///
    /// A drop is treated like the permission prompt's "Session" choice:
    /// read-only, session-scoped (cleared on shell restart) — the least
    /// privilege that satisfies "let this app use this file now". Skipped
    /// if the app already holds a grant covering the path, so repeated
    /// drops don't pile up duplicate capabilities.
    fn grant_dropped_file_access(&self, path: &std::path::Path) {
        use crate::shell::state::{FsMode, GRANT_ALWAYS, GRANT_ONCE, GRANT_SESSION};
        // Which built-in app opens this file governs which grant key we
        // write — same classification the tab splice used.
        let Some(item) = super::workspace_drop::DraggedItem::from_path(path) else {
            return;
        };
        let app_id = super::workspace_drop::decide_app_for_drop(&item)
            .as_str()
            .to_string();
        let ws_id = self.tree().active;
        let store = crate::portal::GrantStoreHandle::new(self.tree_handle());
        let caller = crate::portal::PortalCaller::new(ws_id, app_id);
        if store.filesystem_path_granted(&caller, path) {
            return;
        }
        let cap = Capability::Filesystem {
            path: path.to_path_buf(),
            mode: FsMode::Read,
            scope: GRANT_SESSION,
        };
        store.insert(&caller, cap);
    }

    /// Append a new empty workspace to `tree.workspaces`, activate it,
    /// and bump the revision counter so widgets re-sync on the next
    /// frame. Returns the new workspace's id.
    ///
    /// This is the plain "add workspace" entrypoint — the wheel-disc's
    /// "+" slot (`sidebar_wheel_disc::SidebarWheelDisc::plus_rect`) calls
    /// it directly on press. The concept's sidebar `+` button has the
    /// same semantics: it creates a fresh workspace and switches to it
    /// without opening any settings modal. Closes the concept's
    /// `## Known gaps` #4 ("Plus button") in
    /// `docs/nirios/wheel-disc-design.md`.
    pub fn add_workspace(&mut self, name: impl Into<String>) -> crate::shell::state::WorkspaceId {
        let id = {
            let mut tree = self.tree_mut();
            let id = tree.append_empty_workspace(&name.into());
            tree.set_active(id);
            id
        };
        self.bump();
        id
    }

    /// Title of the currently focused tab, or `None` when nothing is
    /// focused (no workspace / no strip / no group / no tabs). Read by
    /// the search-scope pill so the "This app" pill can display the
    /// tab's title (e.g. `Notes`, `Files`) instead of the static
    /// [`SearchScope::label`].
    pub fn active_app_title(&self) -> Option<String> {
        let tab = self.active_tab_ref()?;
        let title = tab.title.trim();
        if title.is_empty() {
            None
        } else {
            Some(title.to_string())
        }
    }

    /// Id of the currently focused tab, if any. Read by the search
    /// registry to pick the "this app" scope provider via
    /// [`crate::shell::search::SearchProvider::owns_tab`].
    pub fn active_tab_id(&self) -> Option<crate::shell::state::TabId> {
        self.active_tab_ref().map(|t| t.id)
    }

    /// Look up the currently focused tab. `None` when no workspace is
    /// active, or when `active_strip` / `active_group` haven't been
    /// tracked yet — matches the honest "no focused app" state instead
    /// of falling back to `strips.first()` / `window_groups.first()`,
    /// which was the "Ctrl+K opens the wrong tab" bug (2026-07-09
    /// user report). A live focus-writeback reconciler (see
    /// `overlay_host::harvest_active_tab_writeback`) keeps
    /// `group.active_tab` in sync with the tab the user actually
    /// clicked, so this method can rely on the tree pointer being
    /// current.
    fn active_tab_ref(&self) -> Option<crate::shell::state::TabRef> {
        let tree = self.tree();
        let ws_id = tree.active;
        let strip_id = tree.active_strip.get(&ws_id).copied()?;
        let group_id = tree.active_group.get(&strip_id).copied()?;
        // Guard: if any of these dangle (a workspace/strip/group that no
        // longer exists) we don't want to walk them silently.
        let ws = tree.workspaces.iter().find(|w| w.id == ws_id)?;
        let strip = ws.strips.iter().find(|s| s.id == strip_id)?;
        let group = strip.window_groups.iter().find(|g| g.id == group_id)?;
        let tab = group.tabs.iter().find(|t| t.id == group.active_tab)?;
        // The `Option<TabRef>` is opaque so callers can't stash a bare
        // borrow into `tree` past the guard's lifetime — we clone the
        // small fields the callers actually need.
        Some(crate::shell::state::TabRef {
            id: tab.id,
            title: tab.title.clone(),
        })
    }

    /// Cycle the active tab within the active group of the active strip
    /// of the active workspace by `dir` (sign-extracted; ±1). Wraps
    /// around the tab list ends — a forward step past the last tab
    /// lands on the first, and vice versa. Returns true if a tab
    /// switch actually occurred (false if there's no active workspace
    /// / strip / group, or the group has fewer than two tabs).
    ///
    /// Used by the DE-level horizontal-scroll fallback in
    /// `overlay_host`: when the focused app/widget doesn't consume a
    /// horizontal-dominant wheel event, the shell promotes the scroll
    /// to a tab cycle so "swipe sideways = next window" works
    /// everywhere — including over the empty desktop, where no app
    /// is positioned to claim the gesture in the first place.
    pub fn cycle_active_tab(&mut self, dir: i32) -> bool {
        let dir = dir.signum();
        if dir == 0 {
            return false;
        }
        let switched = {
            let mut tree = self.tree_mut();
            let ws_id = tree.active;
            let strip_id_opt = tree.active_strip.get(&ws_id).copied().or_else(|| {
                tree.workspaces
                    .iter()
                    .find(|w| w.id == ws_id)
                    .and_then(|w| w.strips.first().map(|s| s.id))
            });
            let Some(strip_id) = strip_id_opt else {
                return false;
            };
            let group_id_opt = tree.active_group.get(&strip_id).copied().or_else(|| {
                tree.workspaces
                    .iter()
                    .find(|w| w.id == ws_id)
                    .and_then(|w| w.strips.iter().find(|s| s.id == strip_id))
                    .and_then(|s| s.window_groups.first().map(|g| g.id))
            });
            let Some(group_id) = group_id_opt else {
                return false;
            };
            let Some(ws) = tree.workspaces.iter_mut().find(|w| w.id == ws_id) else {
                return false;
            };
            let Some(strip) = ws.strips.iter_mut().find(|s| s.id == strip_id) else {
                return false;
            };
            let Some(group) = strip.window_groups.iter_mut().find(|g| g.id == group_id) else {
                return false;
            };
            if group.tabs.len() < 2 {
                return false;
            }
            let Some(cur_idx) = group.tabs.iter().position(|t| t.id == group.active_tab) else {
                return false;
            };
            let n = group.tabs.len() as i32;
            let new_idx = ((cur_idx as i32 + dir).rem_euclid(n)) as usize;
            group.active_tab = group.tabs[new_idx].id;
            true
        };
        if switched {
            self.bump();
        }
        switched
    }

    /// Switch to the workspace at the given direction (±1). Returns true
    /// if the active workspace changed.
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
            // Smoke-test the notification system: every successful
            // workspace switch pushes a transient toast naming the
            // new active workspace. Any subsystem can call
            // `push_notification`; this is just the canonical first
            // consumer so the system is visibly exercised.
            let new_name = self
                .tree()
                .active_workspace()
                .map(|ws| ws.name.clone())
                .unwrap_or_else(|| "(unnamed)".to_string());
            self.push_notification(format!("Switched to workspace {new_name}"), String::new());
            self.bump();
        }
        changed
    }

    // ---------- Notifications ----------

    /// Read-only view of the currently active notifications, ordered
    /// oldest first.
    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    /// Push a new info-level notification with the given title and
    /// body. Returns the freshly assigned id so callers can dismiss
    /// it later via [`Self::dismiss_notification`]. Bumps the chrome
    /// revision so widgets re-sync on the next frame.
    pub fn push_notification(&mut self, title: impl Into<String>, body: impl Into<String>) -> u64 {
        self.push_notification_full(Notification::new(title, body))
    }

    /// Push a fully-formed [`Notification`] (typically constructed
    /// with `.with_severity(...)` / `.with_dismiss_after(...)`).
    /// Returns the notification's id. Enforces the
    /// [`MAX_NOTIFICATIONS`] cap by dropping oldest entries first.
    ///
    /// **Do-Not-Disturb**: when [`Self::dnd`] is set the notification is
    /// *not* shown as a banner — it is recorded straight into the
    /// history log and the id is returned as usual. The caller (and the
    /// originating D-Bus app) still gets a valid id; the toast is simply
    /// held back from the display surface, honouring "suppress display
    /// but keep history".
    pub fn push_notification_full(&mut self, n: Notification) -> u64 {
        let id = n.id;
        if self.dnd {
            self.record_history(n);
            self.bump();
            return id;
        }
        self.notifications.push(n);
        // Entries pushed off the active stack by the cap are still real
        // notifications the user may want to review — retire them to
        // history rather than dropping them on the floor.
        while self.notifications.len() > MAX_NOTIFICATIONS {
            let evicted = self.notifications.remove(0);
            self.record_history(evicted);
        }
        self.bump();
        id
    }

    /// Append a notification to the bounded history log, evicting the
    /// oldest entry once [`MAX_HISTORY`] is exceeded. Every path that
    /// removes a toast from the active stack — and the DND-suppression
    /// path — funnels through here so history is the single, complete
    /// record of what has been posted.
    fn record_history(&mut self, n: Notification) {
        self.notification_history.push(n);
        while self.notification_history.len() > MAX_HISTORY {
            self.notification_history.remove(0);
        }
    }

    /// Convenience: push a warning-severity notification.
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

    /// Convenience: push an error-severity notification.
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

    /// Dismiss a notification by its internal id. No-op if the id has
    /// already expired or was never pushed. If the removed toast came
    /// from a D-Bus `Notify`, emits `NotificationClosed(id, 2)` (reason:
    /// dismissed-by-user) via the bridge.
    pub fn dismiss_notification(&mut self, id: u64) {
        if let Some(pos) = self.notifications.iter().position(|n| n.id == id) {
            let removed = self.notifications.remove(pos);
            self.emit_closed(&removed, CloseReason::Dismissed);
            self.record_history(removed);
            self.bump();
        }
    }

    /// Activate a notification by its internal id (the user clicked the
    /// toast body, not the X). If the toast carries actions, emits
    /// `ActionInvoked(id, key)` for the default/first action before
    /// removing it; otherwise behaves exactly like
    /// [`Self::dismiss_notification`]. Returns `true` if a toast was
    /// found.
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

    /// Close a notification by its freedesktop (`dbus_id`) id with an
    /// explicit reason. Used by the `CloseNotification` pump path. No-op
    /// if no live toast carries that id.
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

    /// Drop every notification whose `created_at + dismiss_after`
    /// has passed `now`. Returns the number of entries removed.
    /// Called by [`crate::shell::notification::NotificationStack::update`]
    /// each frame so the stack shrinks automatically. Expired toasts that
    /// came from a D-Bus `Notify` emit `NotificationClosed(id, 1)`.
    pub fn update_notification_lifecycle(&mut self, now: std::time::Instant) -> usize {
        // Partition into still-live and expired, preserving order.
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
        // Expired toasts move into the history log — they came and went,
        // but the notification centre still shows them.
        for n in expired {
            self.record_history(n);
        }
        if dropped > 0 {
            self.bump();
        }
        dropped
    }

    /// Number of currently active notifications (used by the lock screen
    /// to show "N notifications" without exposing any content).
    pub fn notification_count(&self) -> usize {
        self.notifications.len()
    }

    // ---------- History ----------

    /// Read-only view of the notification history log, ordered
    /// oldest-first. Contains every notification that has left the
    /// active stack (dismissed / expired / closed / cap-evicted) plus
    /// anything Do-Not-Disturb suppressed. A notification-centre surface
    /// reads this to let the user review what they missed.
    pub fn notification_history(&self) -> &[Notification] {
        &self.notification_history
    }

    /// Number of entries in the history log.
    pub fn notification_history_count(&self) -> usize {
        self.notification_history.len()
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

    /// Set Do-Not-Disturb on/off. Idempotent — a no-op (no revision
    /// bump) when the value is unchanged.
    pub fn set_dnd(&mut self, on: bool) {
        if self.dnd != on {
            self.dnd = on;
            self.bump();
        }
    }

    /// Toggle Do-Not-Disturb, returning the new state. Wired to the
    /// command-palette "Toggle Do Not Disturb" action.
    pub fn toggle_dnd(&mut self) -> bool {
        self.dnd = !self.dnd;
        self.bump();
        self.dnd
    }

    // ---------- Track D1: D-Bus notification wiring ----------

    /// Install the [`NotificationBridge`] the compositor shares with the
    /// `org.freedesktop.Notifications` D-Bus service. Once set, the toast
    /// lifecycle emits `NotificationClosed` / `ActionInvoked` events back
    /// to apps via the bridge.
    pub fn set_notification_bridge(&mut self, bridge: NotificationBridge) {
        self.notification_bridge = Some(bridge);
    }

    /// Mirror the compositor's session-lock state into the chrome so the
    /// notification stack can suppress content while locked.
    pub fn set_session_locked(&mut self, locked: bool) {
        if self.session_locked != locked {
            self.session_locked = locked;
            self.bump();
        }
    }

    /// Whether the session is currently locked.
    pub fn session_locked(&self) -> bool {
        self.session_locked
    }

    /// Drain the D-Bus notification bridge into the live toast stack:
    /// every queued `Notify` becomes a toast (honouring `replaces_id` by
    /// evicting the prior toast with the same `dbus_id` first), and every
    /// `CloseNotification` removes the matching toast. Called once per
    /// frame by the compositor; a no-op when no bridge is installed.
    pub fn pump_dbus_notifications(&mut self) {
        let Some(bridge) = self.notification_bridge.clone() else {
            return;
        };
        for inc in bridge.drain_incoming() {
            let dbus_id = inc.dbus_id;
            // `replaces_id` semantics: a fresh Notify with an existing id
            // supersedes the prior toast.
            self.notifications.retain(|n| n.dbus_id != Some(dbus_id));
            self.push_notification_full(Notification::from_incoming(inc));
        }
        for id in bridge.drain_close_requests() {
            self.close_notification_by_dbus_id(id, CloseReason::Closed);
        }
    }

    /// Emit a `NotificationClosed` for a removed toast iff it originated
    /// from D-Bus and the bridge is installed.
    fn emit_closed(&self, removed: &Notification, reason: CloseReason) {
        if let (Some(bridge), Some(dbus_id)) = (self.notification_bridge.as_ref(), removed.dbus_id)
        {
            bridge.note_closed(dbus_id, reason);
        }
    }
}

pub type Chrome = Rc<RefCell<ChromeState>>;

#[cfg(test)]
mod active_tab_tests {
    //! Pin the "which tab is Ctrl+K scoped to" contract. Regression for
    //! the 2026-07-09 report where the palette scoped to the first tab
    //! of the active group rather than the tab the user actually clicked.
    //!
    //! `active_app_title` / `active_tab_id` are the source of truth the
    //! search registry reads to seed `SearchScope::ActiveApp`. Both now
    //! return `None` when the tree hasn't tracked a real focus target
    //! (rather than falling back to `strips.first()`), and the shell's
    //! `harvest_active_tab_writeback` reconciler keeps
    //! `group.active_tab` in sync with the widget-side `bar.active_id`.
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
        // Blank tree — no strips, no groups. Must not fall back to a
        // ghost first-of-nothing.
        let handle = Arc::new(RwLock::new(WorkspaceTree::default()));
        let chrome = ChromeState::new_with_handle(handle);
        let st = chrome.borrow();
        assert_eq!(st.active_tab_id(), None);
        assert!(st.active_app_title().is_none());
    }

    #[test]
    fn without_active_strip_returns_none_not_first_strip() {
        // Regression for the 2026-07-09 report: an active `WorkspaceId`
        // exists but the tree hasn't stamped `active_strip` yet. The
        // old fallback picked `strips.first()` and reported the wrong
        // tab; the new implementation reports None.
        let mut tree = tree_with(100);
        tree.active_strip.clear();
        let handle = Arc::new(RwLock::new(tree));
        let chrome = ChromeState::new_with_handle(handle);
        let st = chrome.borrow();
        assert_eq!(st.active_tab_id(), None);
    }
}
