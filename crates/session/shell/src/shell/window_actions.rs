//! Creating windows and tabs from the chrome.
//!
//! `WorkspaceTree::bootstrap_default` supplies no windows on first boot —
//! a window nobody opened is a window nobody opened — so these primitives
//! are not a convenience on top of a pre-populated desktop: on a fresh
//! machine they are the only way a first window exists at all, and they
//! have to work from empty.
//!
//! These are the primitives behind the "+" chip on a group's tab strip,
//! the `Super`-chords, and the palette's action rows. They all funnel
//! here so "new terminal" means the same thing however it was asked for,
//! and so placement stays a single decision made by
//! [`crate::shell::placement`].

use crate::shell::chrome_state::ChromeState;
use crate::shell::state::{AppKind, GroupId, TabId, TileContent};
use crate::shell::tab_binding::{self, Placement};
use tilo_desktop_model::commands::OpenIntent;

/// Where a newly-created window should go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Destination {
    /// Follow [`crate::shell::placement::placement_for`] — to
    /// the right of the active window, or into its group when that group
    /// is already tabbed.
    Beside,
    /// An app chosen from search: its own window right of the active one,
    /// never a tab of it, even when that group is tabbed.
    Search,
    /// Join the focused group as a tab, whatever the rule would say.
    /// Falls back to `Beside` when nothing is focused.
    Here,
    /// Join this very group as a tab: the group whose "+" asked.
    /// Falls back to `Here` when the group has since closed.
    InGroup(GroupId),
}

/// The group the user is looking at, or `None` when the active workspace
/// has no groups (a fresh empty workspace).
pub fn focused_group(st: &ChromeState) -> Option<GroupId> {
    let tree = st.tree();
    let ws = tree.desktop.active_workspace()?;
    let strip = tree
        .desktop
        .active_strip
        .get(&ws.id)
        .and_then(|id| ws.strips.iter().find(|s| s.id == *id))
        .or_else(|| ws.strips.first())?;
    tree.desktop
        .active_group
        .get(&strip.id)
        .copied()
        .filter(|g| strip.window_groups.iter().any(|w| w.id == *g))
        .or_else(|| strip.window_groups.first().map(|g| g.id))
}

/// Where something chosen from search opens: as a tab in the group whose
/// "+" opened the palette, otherwise where search puts a new window.
pub(crate) fn search_placement(st: &ChromeState) -> Placement {
    crate::shell::placement::placement_for_target(&st.tree(), st.strip_viewport(), st.open_target)
}

/// Resolve a [`Destination`] into the placement the splice will use.
fn placement_for(st: &ChromeState, dest: Destination) -> Placement {
    match dest {
        Destination::Here => match focused_group(st) {
            Some(g) => Placement::TabIn(g),
            None => rule_placement(st),
        },
        Destination::InGroup(group) => {
            let exists = st.tree().desktop.active_workspace().is_some_and(|ws| {
                ws.strips
                    .iter()
                    .any(|strip| strip.window_groups.iter().any(|g| g.id == group))
            });
            if exists {
                Placement::TabIn(group)
            } else {
                placement_for(st, Destination::Here)
            }
        }
        Destination::Beside => rule_placement(st),
        Destination::Search => {
            let tree = st.tree();
            crate::shell::placement::placement_for(&tree, OpenIntent::Search, st.strip_viewport())
        }
    }
}

/// The ordinary placement rule, evaluated against the layout the user is
/// currently looking at.
fn rule_placement(st: &ChromeState) -> Placement {
    let tree = st.tree();
    crate::shell::placement::placement_for(&tree, OpenIntent::Launch, st.strip_viewport())
}

/// The package id of the native Terminal app.
pub const TERMINAL_PACKAGE: &str = "so.tilo.terminal";

/// A new Terminal session at `dest`, always: another Terminal process in a
/// tab of the focused group (or by the placement rule), however many the
/// room already shows. Ordinary "Open Terminal" refocuses the running one;
/// this is the explicit ask for another shell — the group's "+", the
/// palette's "New terminal", Duplicate on a terminal tab. When no Terminal
/// package is installed the tab is hosted in-process instead, so the first
/// window of an image without one still opens.
///
/// # Errors
///
/// The launch was refused; nothing opened.
pub fn new_terminal(
    st: &mut ChromeState,
    dest: Destination,
) -> Result<(), crate::shell::app_tabs::AppTabError> {
    let anchor = match (dest, focused_group(st)) {
        (Destination::InGroup(group), _) | (Destination::Here, Some(group)) => {
            let tree = st.tree();
            crate::shell::app_tabs::anchor_in_group(&tree, group)
        }
        _ => None,
    };
    match crate::shell::app_tabs::open_beside(st, TERMINAL_PACKAGE, None, anchor, false) {
        Err(crate::shell::app_tabs::AppTabError::UnknownApp(_)) => {
            open_terminal(st, dest);
            Ok(())
        }
        other => other,
    }
}

/// Hosted in-process, so this is a tree splice rather than a launch — a
/// sane recovery path when the last terminal is gone.
pub fn open_terminal(st: &mut ChromeState, dest: Destination) -> TabId {
    open_hosted(st, AppKind::Terminal, "Terminal", dest)
}

/// Open any hosted app kind at `dest` and focus it.
pub fn open_hosted(
    st: &mut ChromeState,
    kind: AppKind,
    title: impl Into<String>,
    dest: Destination,
) -> TabId {
    open_hosted_after_setup(st, kind, title, dest, |_, _| {})
}

/// File openers finalize their reference and startup payload before the
/// creation owner captures its exact scratch-record ticket.
pub(crate) fn open_hosted_after_setup(
    st: &mut ChromeState,
    kind: AppKind,
    title: impl Into<String>,
    dest: Destination,
    setup: impl FnOnce(&mut ChromeState, TabId),
) -> TabId {
    let placement = placement_for(st, dest);
    let id = {
        let mut tree = st.tree_mut();
        tab_binding::open_hosted_app(&mut tree, kind, title, placement)
    };
    setup(st, id);
    st.create_new_tab_record(id);
    st.bump();
    id
}

/// Why a duplicate could not be made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DuplicateError {
    /// No tab is focused — nothing to copy.
    NoActiveTab,
    /// A foreign Wayland client's window belongs to another process; the
    /// compositor can't clone it, and guessing a package from `app_id`
    /// would as often start the wrong thing.
    ForeignWindow,
}

impl DuplicateError {
    pub fn user_message(self) -> String {
        use crate::shell::i18n::t;
        match self {
            DuplicateError::NoActiveTab => t("window-duplicate-no-tab"),
            DuplicateError::ForeignWindow => t("window-duplicate-foreign"),
        }
    }
}

/// A fresh instance, not a copy of state — the new tab starts however that
/// app starts (a second shell, for a terminal, the case this exists for).
///
/// # Errors
///
/// [`DuplicateError::NoActiveTab`] when nothing is focused (an empty
/// workspace, or a strip/group/tab list gone empty underneath the
/// active ids); [`DuplicateError::ForeignWindow`] when the focused tab
/// is a client-owned or peer surface the compositor can't clone.
pub fn duplicate_active_tab(st: &mut ChromeState) -> Result<Option<TabId>, DuplicateError> {
    let (kind, title, group) = {
        let tree = st.tree();
        let ws = tree
            .desktop
            .active_workspace()
            .ok_or(DuplicateError::NoActiveTab)?;
        let strip = tree
            .desktop
            .active_strip
            .get(&ws.id)
            .and_then(|id| ws.strips.iter().find(|s| s.id == *id))
            .or_else(|| ws.strips.first())
            .ok_or(DuplicateError::NoActiveTab)?;
        let group = tree
            .desktop
            .active_group
            .get(&strip.id)
            .and_then(|g| strip.window_groups.iter().find(|w| w.id == *g))
            .or_else(|| strip.window_groups.first())
            .ok_or(DuplicateError::NoActiveTab)?;
        let tab = group
            .tabs
            .iter()
            .find(|t| t.id == group.active_tab)
            .or_else(|| group.tabs.first())
            .ok_or(DuplicateError::NoActiveTab)?;
        match &tab.tile {
            TileContent::AppPlaceholder { kind, label } | TileContent::Stub { kind, label } => {
                (kind.clone(), label.clone(), group.id)
            }
            // A terminal of this machine is the case this exists for: a
            // second shell is another Terminal process, a tab right after it.
            TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. }
                if app_id == TERMINAL_PACKAGE
                    && crate::shell::state::peer_salt_of(tab.id) == tree.local_salt() =>
            {
                let anchor = crate::shell::app_tabs::anchor_for_tab(&tree, tab.id);
                drop(tree);
                return crate::shell::app_tabs::open_beside(
                    st,
                    TERMINAL_PACKAGE,
                    None,
                    anchor,
                    false,
                )
                .map(|()| None)
                .map_err(|_| DuplicateError::ForeignWindow);
            }
            // None can be duplicated: the workspace recap, a live client's
            // surface, or a window belonging to somebody else's machine.
            TileContent::WorkspaceActivity
            | TileContent::Foreign { .. }
            | TileContent::PeerWindow { .. }
            | TileContent::HostedWindow { .. }
            | TileContent::LocalApp { .. } => {
                return Err(DuplicateError::ForeignWindow);
            }
        }
    };
    let placement = Placement::TabIn(group);
    let id = {
        let mut tree = st.tree_mut();
        tab_binding::open_hosted_app(&mut tree, kind, title, placement)
    };
    st.create_new_tab_record(id);
    st.bump();
    Ok(Some(id))
}

/// Why a tab could not be closed or split. One type for the chord, the
/// tab's right-click menu and the `tab.*` actions, so every door refuses
/// with the same words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabActionError {
    /// No tab is focused — nothing to act on.
    NoActiveTab,
    /// The tab is not in the tree (already closed, or a stale id).
    UnknownTab,
    /// A group's only tab cannot be split off: the group would dissolve
    /// and be re-created one slot over.
    OnlyTab,
    /// The replicated document refused the mutation; nothing changed.
    DocumentRefused,
}

impl TabActionError {
    pub fn user_message(self) -> String {
        use crate::shell::i18n::t;
        match self {
            TabActionError::NoActiveTab => t("window-tab-action-no-tab"),
            TabActionError::UnknownTab => t("window-tab-action-unknown-tab"),
            TabActionError::OnlyTab => t("window-tab-action-only-tab"),
            TabActionError::DocumentRefused => t("window-tab-action-document-refused"),
        }
    }
}

/// Where a split-off tab's new group lands in its strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitPlacement {
    /// Immediately to the right of the tab's current group.
    Right,
    /// At the end of the strip.
    StripEnd,
}

/// The focused group's active tab, the way [`duplicate_active_tab`]
/// resolves it (active strip → active group → active tab, each falling
/// back to the first).
pub fn active_tab_id(st: &ChromeState) -> Option<TabId> {
    let tree = st.tree();
    let ws = tree.desktop.active_workspace()?;
    let strip = tree
        .desktop
        .active_strip
        .get(&ws.id)
        .and_then(|id| ws.strips.iter().find(|s| s.id == *id))
        .or_else(|| ws.strips.first())?;
    let group = tree
        .desktop
        .active_group
        .get(&strip.id)
        .and_then(|g| strip.window_groups.iter().find(|w| w.id == *g))
        .or_else(|| strip.window_groups.first())?;
    let tab = group
        .tabs
        .iter()
        .find(|t| t.id == group.active_tab)
        .or_else(|| group.tabs.first())
        .map(|t| t.id);
    drop(tree);
    tab
}

/// Close `tab_id` through the tab's owner. Local content is removed durably;
/// a confined native client first receives a close request and retains its
/// visible tab until actual surface destruction admits the close. The `✕`,
/// keyboard chord, tab menu and declared action converge here.
///
/// # Errors
///
/// [`TabActionError::UnknownTab`] when the tab is not in the tree or the
/// document refuses the deletion (nothing changed either way).
pub fn close_tab(st: &mut ChromeState, tab_id: TabId) -> Result<(), TabActionError> {
    let closed = crate::shell::closed_tabs::capture_tab(&st.tree(), tab_id);
    close_tab_unrecorded(st, tab_id)?;
    if let Some(entry) = closed {
        st.remember_closed_tab_after_removal(entry);
    }
    Ok(())
}

/// [`close_tab`] without remembering it for reopening, for a caller that
/// records a larger close itself (a whole window group).
///
/// # Errors
///
/// As [`close_tab`].
pub(crate) fn close_tab_unrecorded(
    st: &mut ChromeState,
    tab_id: TabId,
) -> Result<(), TabActionError> {
    let foreign = {
        let tree = st.tree();
        tree.desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .find(|tab| tab.id == tab_id)
            .and_then(|tab| match &tab.tile {
                TileContent::Foreign { app_id, surface_id } => Some((app_id.clone(), *surface_id)),
                _ => None,
            })
    };
    let launch_key = foreign
        .as_ref()
        .map(|(app_id, surface_id)| host_launch_key(app_id, *surface_id));
    // A confined/native client may defer close to save or show a decision.
    // Its surface, tab and focus stay live until the real destroy callback.
    // A restored/layout-only tab has no live client to answer and is removed
    // immediately, as are supervised unconfined host launches.
    #[cfg(any(test, target_os = "linux"))]
    let supervised_unconfined = launch_key.as_deref().is_some_and(|key| {
        let known = crate::shell::launcher::launch_key_for_surface(
            foreign.as_ref().map_or(0, |(_, id)| *id),
        )
        .is_some()
            || crate::shell::launcher::launch_key_for_app_id(
                foreign.as_ref().map_or("", |(id, _)| id),
            )
            .is_some();
        crate::shell::launcher::unconfined_launch_is_live(key)
            || (!known
                && !crate::shell::launcher::unconfined_launches_for_unresolved_close().is_empty())
    });
    #[cfg(not(any(test, target_os = "linux")))]
    let supervised_unconfined = false;
    if let Some((_, surface_id)) = &foreign
        && !supervised_unconfined
        && crate::shell::foreign::store().is_adopted(*surface_id)
    {
        st.request_surface_close(*surface_id);
        st.bump();
        return Ok(());
    }
    let removed = {
        let mut tree = st.tree_mut();
        // The person closed it: focus goes back to where they came from.
        tab_binding::close_tab_back_to_previous(&mut tree, tab_id)
    };
    if !removed {
        return Err(TabActionError::UnknownTab);
    }
    st.hosted_inputs.prune_documents(&mut st.tree_mut());
    // Close is durable before the client is asked to exit.
    st.tree().queue_save();
    if let Some((_, surface_id)) = foreign {
        st.request_surface_close(surface_id);
    }
    #[cfg(any(test, target_os = "linux"))]
    if let Some(key) = launch_key.as_deref()
        && !foreign_tabs_remain_for(&st.tree().desktop, key)
    {
        // Wine File's tab announces wineboot.exe. That is not the desktop
        // id the launch table uses, so terminate(key) is a no-op until the
        // surface was attributed to the launch. A second X11 window of that
        // launch (Wine Mono announces control.exe) still counts. Session
        // helpers that never asked for a window are not part of this close.
        let known = crate::shell::launcher::unconfined_launch_is_live(key)
            || crate::shell::launcher::launch_key_for_surface(
                foreign.as_ref().map_or(0, |(_, id)| *id),
            )
            .is_some()
            || crate::shell::launcher::launch_key_for_app_id(
                foreign.as_ref().map_or("", |(id, _)| id),
            )
            .is_some();
        crate::shell::launcher::terminate_unconfined_launches(key);
        if !known {
            for package_id in crate::shell::launcher::unconfined_launches_for_unresolved_close() {
                if !foreign_tabs_remain_for(&st.tree().desktop, &package_id) {
                    crate::shell::launcher::terminate_unconfined_launches(&package_id);
                }
            }
        }
    }
    st.bump();
    Ok(())
}

fn host_launch_key(app_id: &str, surface_id: u64) -> String {
    crate::shell::persist::durable_app_id(surface_id, app_id)
}

fn foreign_tabs_remain_for(desktop: &crate::shell::state::DesktopTree, launch_key: &str) -> bool {
    let mut unresolved = false;
    for workspace in &desktop.workspaces {
        for strip in &workspace.strips {
            for group in &strip.window_groups {
                for tab in &group.tabs {
                    let TileContent::Foreign { app_id, surface_id } = &tab.tile else {
                        continue;
                    };
                    let resolved = host_launch_key(app_id, *surface_id);
                    if resolved.eq_ignore_ascii_case(launch_key)
                        || app_id.eq_ignore_ascii_case(launch_key)
                    {
                        return true;
                    }
                    // `durable_app_id` falls back to the announced toolkit id
                    // (wineboot.exe, control.exe) when the desktop id never
                    // matched. That window still belongs to the user launch
                    // that asked for it, even when autostart helpers are also
                    // live.
                    if resolved.eq_ignore_ascii_case(app_id) {
                        unresolved = true;
                    }
                }
            }
        }
    }
    unresolved && unresolved_window_belongs_to(launch_key)
}

/// An unmatched toolkit window belongs to `launch_key` when that launch is
/// the only unconfined process, or the one user launch waiting on a window.
fn unresolved_window_belongs_to(launch_key: &str) -> bool {
    sole_live_unconfined_launch(launch_key) || unresolved_launch_is_the_user_launch(launch_key)
}

#[cfg(any(test, target_os = "linux"))]
fn unresolved_launch_is_the_user_launch(launch_key: &str) -> bool {
    crate::shell::launcher::unconfined_launches_for_unresolved_close()
        .iter()
        .any(|id| id.eq_ignore_ascii_case(launch_key))
}

#[cfg(not(any(test, target_os = "linux")))]
fn unresolved_launch_is_the_user_launch(launch_key: &str) -> bool {
    let _ = launch_key;
    false
}

/// True when `launch_key` is the only unconfined host launch still running.
///
/// On hosts that do not track launches this is false: there is no process
/// to keep, and a toolkit window id must not invent one.
fn sole_live_unconfined_launch(launch_key: &str) -> bool {
    #[cfg(any(test, target_os = "linux"))]
    {
        let live = crate::shell::launcher::live_unconfined_package_ids();
        live.len() == 1
            && live
                .first()
                .is_some_and(|package| package.eq_ignore_ascii_case(launch_key))
    }
    #[cfg(not(any(test, target_os = "linux")))]
    {
        let _ = launch_key;
        false
    }
}

/// [`close_tab`] on the focused group's active tab — the `tab.close`
/// action's form.
///
/// # Errors
///
/// [`TabActionError::NoActiveTab`] when nothing is focused, otherwise as
/// [`close_tab`].
pub fn close_active_tab(st: &mut ChromeState) -> Result<(), TabActionError> {
    let tab = active_tab_id(st).ok_or(TabActionError::NoActiveTab)?;
    close_tab(st, tab)
}

/// Why a window could not move to another workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveTabError {
    /// The tab is not in the tree (already closed, or a stale id).
    UnknownTab,
    /// The destination workspace is gone, or has no strip to land in.
    UnknownWorkspace,
    /// The window is already in that workspace.
    SameWorkspace,
    /// The window belongs to its workspace: its private history, or a
    /// collaborator's window shown here.
    Pinned,
    /// The replicated document refused the mutation; nothing changed.
    DocumentRefused,
    /// A room owner keeps that workspace's records, not this device.
    HostedWorkspace,
    /// The destination workspace does not let this app be opened there.
    NotPermitted,
    /// The window is already on its way somewhere.
    AlreadyMoving,
    /// Nobody is at this desktop to agree to what crosses.
    NobodyToConfirm,
}

impl MoveTabError {
    pub fn user_message(self) -> String {
        crate::shell::i18n::t(match self {
            MoveTabError::UnknownTab => "window-move-unknown-tab",
            MoveTabError::UnknownWorkspace => "window-move-unknown-workspace",
            MoveTabError::SameWorkspace => "window-move-same-workspace",
            MoveTabError::Pinned => "window-move-pinned",
            MoveTabError::DocumentRefused => "window-move-document-refused",
            MoveTabError::HostedWorkspace => "window-move-hosted-workspace",
            MoveTabError::NotPermitted => "window-move-not-permitted",
            MoveTabError::AlreadyMoving => "window-move-already-moving",
            MoveTabError::NobodyToConfirm => "window-move-nobody-to-confirm",
        })
    }
}

/// What asking to move a window did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveOutcome {
    /// A window the shell draws itself moved as it is.
    Moved,
    /// A client window crosses a sandbox boundary: the sheet asking what
    /// crosses is up, and nothing has changed yet
    /// ([`crate::shell::window_transfer`]).
    AwaitingConsent,
}

/// Move `tab_id` into workspace `target`, preferring to move its document
/// rather than copy it. See [`transfer_tab_to_workspace`].
///
/// # Errors
///
/// As [`transfer_tab_to_workspace`].
pub fn move_tab_to_workspace(
    st: &mut ChromeState,
    tab_id: TabId,
    target: crate::shell::state::WorkspaceId,
) -> Result<MoveOutcome, MoveTabError> {
    transfer_tab_to_workspace(
        st,
        tab_id,
        target,
        crate::shell::window_transfer::TransferMode::Move,
    )
}

/// Take `tab_id` into workspace `target`.
///
/// A window the shell draws itself moves as it is, as a new window at the
/// end of the destination's current strip, tree first
/// ([`crate::shell::state::WorkspaceTree::move_tab_across_strips`]).
///
/// The workspace is the sandbox boundary: a running client stays inside the
/// sandbox, grants and collaborators of the workspace it was launched in, so
/// a client window never crosses by itself. Its record and state are carried
/// by [`crate::shell::window_transfer`] once the person agrees on the sheet
/// this puts up, `preferred` being the answer it offers first.
///
/// # Errors
///
/// [`MoveTabError::UnknownTab`] when the tab is in no strip,
/// [`MoveTabError::UnknownWorkspace`] when `target` is gone or has no strip,
/// [`MoveTabError::SameWorkspace`] when the tab is already there,
/// [`MoveTabError::Pinned`] for a workspace's own history or a peer's
/// window, [`MoveTabError::DocumentRefused`] when the document refuses
/// (nothing changed), and the transfer's own refusals for a client window.
pub fn transfer_tab_to_workspace(
    st: &mut ChromeState,
    tab_id: TabId,
    target: crate::shell::state::WorkspaceId,
    preferred: crate::shell::window_transfer::TransferMode,
) -> Result<MoveOutcome, MoveTabError> {
    let tile = st
        .tree()
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .find(|tab| tab.id == tab_id)
        .map(|tab| tab.tile.clone())
        .ok_or(MoveTabError::UnknownTab)?;
    if matches!(
        tile,
        TileContent::Foreign { .. } | TileContent::LocalApp { .. }
    ) {
        crate::shell::window_transfer::request(st, tab_id, target, preferred)?;
        return Ok(MoveOutcome::AwaitingConsent);
    }
    {
        let mut tree = st.tree_mut();
        let (source, src_strip, tile) = tree
            .desktop
            .workspaces
            .iter()
            .find_map(|workspace| {
                workspace.strips.iter().find_map(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .flat_map(|group| group.tabs.iter())
                        .find(|tab| tab.id == tab_id)
                        .map(|tab| (workspace.id, strip.id, tab.tile.clone()))
                })
            })
            .ok_or(MoveTabError::UnknownTab)?;
        if source == target {
            return Err(MoveTabError::SameWorkspace);
        }
        if matches!(
            tile,
            TileContent::WorkspaceActivity
                | TileContent::PeerWindow { .. }
                | TileContent::HostedWindow { .. }
        ) {
            return Err(MoveTabError::Pinned);
        }
        let current = tree.desktop.active_strip.get(&target).copied();
        let destination = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.id == target)
            .and_then(|workspace| {
                workspace
                    .strips
                    .iter()
                    .find(|strip| Some(strip.id) == current)
                    .or_else(|| workspace.strips.first())
                    .map(|strip| (strip.id, strip.window_groups.len()))
            });
        let Some((dst_strip, gap_index)) = destination else {
            return Err(MoveTabError::UnknownWorkspace);
        };
        if !tree.move_tab_across_strips(
            src_strip,
            dst_strip,
            tab_id,
            crate::shell::state::next_id(),
            crate::shell::state::DropTarget::GroupGap { gap_index },
        ) {
            return Err(MoveTabError::DocumentRefused);
        }
    }
    st.bump();
    Ok(MoveOutcome::Moved)
}

/// Split `tab_id` into a new single-tab group in its own strip, tree first
/// ([`crate::shell::state::WorkspaceTree::split_tab_into_new_group`]): the
/// cache and the replicated document move together and the chrome
/// revision bumps, so the next resync builds the widget side — or, when the
/// caller already holds the live tab host, adopts the group it moved under
/// the same id. The tab menu's "Split Right" / "Move to New Group" and the
/// `tab.split-right` / `tab.move-to-new-group` actions converge here.
///
/// # Errors
///
/// [`TabActionError::UnknownTab`] when the tab is in no strip;
/// [`TabActionError::OnlyTab`] when its group has no other tab;
/// [`TabActionError::DocumentRefused`] when the document refuses (nothing
/// changed).
pub fn split_tab(
    st: &mut ChromeState,
    tab_id: TabId,
    placement: SplitPlacement,
) -> Result<GroupId, TabActionError> {
    let (strip_id, insert_at, only_tab) = {
        let tree = st.tree();
        let located = tree.desktop.workspaces.iter().find_map(|ws| {
            ws.strips.iter().find_map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .enumerate()
                    .find(|(_, group)| group.tabs.iter().any(|tab| tab.id == tab_id))
                    .map(|(group_idx, group)| {
                        let insert_at = match placement {
                            SplitPlacement::Right => group_idx.saturating_add(1),
                            SplitPlacement::StripEnd => strip.window_groups.len(),
                        };
                        (strip.id, insert_at, group.tabs.len() < 2)
                    })
            })
        });
        drop(tree);
        located.ok_or(TabActionError::UnknownTab)?
    };
    if only_tab {
        return Err(TabActionError::OnlyTab);
    }
    let new_group = st
        .tree_mut()
        .split_tab_into_new_group(strip_id, tab_id, insert_at)
        .ok_or(TabActionError::DocumentRefused)?;
    st.bump();
    Ok(new_group)
}

/// [`split_tab`] on the focused group's active tab — the actions' form.
///
/// # Errors
///
/// [`TabActionError::NoActiveTab`] when nothing is focused, otherwise as
/// [`split_tab`].
pub fn split_active_tab(
    st: &mut ChromeState,
    placement: SplitPlacement,
) -> Result<GroupId, TabActionError> {
    let tab = active_tab_id(st).ok_or(TabActionError::NoActiveTab)?;
    split_tab(st, tab, placement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::{Tab, WorkspaceTree, next_id};
    use std::cell::RefMut;

    fn chrome_with_default_tree() -> crate::shell::chrome_state::Chrome {
        ChromeState::new(WorkspaceTree::bootstrap_demo())
    }

    /// The active strip's group ids, in order, and the group holding `tab`.
    fn strip_order_and_group_of(st: &ChromeState, tab: TabId) -> (Vec<GroupId>, Option<GroupId>) {
        let tree = st.tree();
        let ws = tree.desktop.active_workspace().expect("active workspace");
        let strip = tree
            .desktop
            .active_strip
            .get(&ws.id)
            .and_then(|id| ws.strips.iter().find(|s| s.id == *id))
            .or_else(|| ws.strips.first())
            .expect("a strip");
        let order = strip.window_groups.iter().map(|g| g.id).collect();
        let group = strip
            .window_groups
            .iter()
            .find(|g| g.tabs.iter().any(|t| t.id == tab))
            .map(|g| g.id);
        drop(tree);
        (order, group)
    }

    #[test]
    fn a_searched_window_opens_beside_you_and_closing_it_brings_you_back() {
        let chrome = chrome_with_default_tree();
        let mut st = chrome.borrow_mut();
        let a = focused_group(&st).expect("the demo desk has a focused group");
        // A neighbour C right of A, then back to A.
        let c_tab = open_hosted(
            &mut st,
            AppKind::Other("C".into()),
            "C",
            Destination::Search,
        );
        let (_, c) = strip_order_and_group_of(&st, c_tab);
        let c = c.expect("C opened as its own window");
        let strip = {
            let tree = st.tree();
            let ws = tree.desktop.active_workspace().expect("active workspace");
            tree.desktop
                .active_strip
                .get(&ws.id)
                .copied()
                .unwrap_or(ws.strips[0].id)
        };
        st.tree_mut().desktop.set_active_group(strip, a);

        // From A, search opens B as its own window right of A (not a tab of A,
        // which holds several, and not after C at the far end).
        let b_tab = open_hosted(
            &mut st,
            AppKind::Other("B".into()),
            "B",
            Destination::Search,
        );
        let (order, b) = strip_order_and_group_of(&st, b_tab);
        let b = b.expect("B opened as its own window");
        assert_ne!(
            b, a,
            "a searched app never becomes a tab of the focused group"
        );
        let at = order.iter().position(|g| *g == a).unwrap();
        assert_eq!(order.get(at + 1..at + 3), Some(&[b, c][..]), "{order:?}");
        assert_eq!(focused_group(&st), Some(b));

        // Closing B goes back to A, the window it was opened from, not to C,
        // which now sits at B's index.
        close_tab(&mut st, b_tab).expect("B closes");
        assert_eq!(focused_group(&st), Some(a));
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn window_actions_are_durable_for_history_restart_and_peers() {
        // Phase `history_new_hosted_tabs_have_records_before_tree_rebuild`.
        {
            let chrome = chrome_with_default_tree();
            let mut st = chrome.borrow_mut();
            let first = open_hosted(
                &mut st,
                AppKind::Other("Notes".into()),
                "Notes",
                Destination::Beside,
            );
            let second = duplicate_active_tab(&mut st)
                .unwrap()
                .expect("hosted duplicate");
            for id in [first, second] {
                assert!(
                    st.records()
                        .read(&ui::RecordId::for_tab(id), |_| ())
                        .is_some(),
                    "the creating action must establish its new record before presentation"
                );
            }
            assert_ne!(first, second);
        }

        // Phase `opening_a_hosted_app_is_durable_for_restart_and_peer_export`.
        {
            let state_dir = tempfile::tempdir().expect("state directory");
            let state_path = state_dir.path().join("state.json");
            let chrome = chrome_with_default_tree();
            let mut st = chrome.borrow_mut();
            let baseline = {
                let mut tree = st.tree_mut();
                tree.persist_path = Some(state_path.clone());
                tree.current_heads()
            };
            let mut replica = st.tree().clone();

            let tab_id = open_terminal(&mut st, Destination::Beside);
            let (changes, restarted) = {
                let mut tree = st.tree_mut();
                let changes = tree.export_changes_since(&baseline);
                tree.queue_save();
                drop(tree);
                let restarted = crate::shell::persist::load_tree_from(&state_path)
                    .expect("opening a hosted app must leave a loadable state file");
                (changes, restarted)
            };

            replica
                .apply_changes(&changes)
                .expect("hosted app creation must be present in peer export");

            for (authority, tree) in [("restart", &restarted), ("peer export", &replica)] {
                assert!(
                    tree.desktop
                        .workspaces
                        .iter()
                        .flat_map(|workspace| workspace.strips.iter())
                        .flat_map(|strip| strip.window_groups.iter())
                        .flat_map(|group| group.tabs.iter())
                        .any(|tab| tab.id == tab_id),
                    "a hosted app opened in the chrome must survive the {authority} authority read"
                );
            }
        }

        // Phase `switching_workspace_is_durable_across_restart`.
        {
            let state_dir = tempfile::tempdir().expect("state directory");
            let state_path = state_dir.path().join("state.json");
            let mut tree = WorkspaceTree::bootstrap_demo();
            tree.desktop
                .workspaces
                .first_mut()
                .expect("demo has a first workspace")
                .name = "Home (workspace persistence test)".into();
            tree.persist_path = Some(state_path.clone());
            let first = tree
                .desktop
                .workspaces
                .first()
                .expect("demo has a first workspace")
                .id;
            let target = tree
                .desktop
                .workspaces
                .get(1)
                .expect("demo has a second workspace")
                .id;

            tree.set_active(target);
            // A restart ends the process that held the state file's single writer.
            drop(tree);

            let restarted = crate::shell::persist::load_tree_from(&state_path)
                .expect("workspace selection must leave a loadable state file");
            assert_eq!(
                restarted.desktop.active, target,
                "the workspace selected by the user must be the one reopened"
            );

            let mut moved_up = restarted;
            moved_up.persist_path = Some(state_path.clone());
            assert!(moved_up.switch_up());
            drop(moved_up);
            let restarted = crate::shell::persist::load_tree_from(&state_path)
                .expect("moving to the previous workspace must persist immediately");
            assert_eq!(restarted.desktop.active, first);

            let mut moved_down = restarted;
            moved_down.persist_path = Some(state_path.clone());
            assert!(moved_down.switch_down());
            drop(moved_down);
            let restarted = crate::shell::persist::load_tree_from(&state_path)
                .expect("moving to the next workspace must persist immediately");
            assert_eq!(restarted.desktop.active, target);
        }
    }

    /// The group the user is looking at, and the number of tabs in it.
    fn focused_group_tab_count(st: &ChromeState) -> usize {
        let g = focused_group(st).expect("a focused group");
        let tree = st.tree();
        tree.desktop
            .active_workspace()
            .unwrap()
            .strips
            .iter()
            .flat_map(|s| s.window_groups.iter())
            .find(|w| w.id == g)
            .map(|w| w.tabs.len())
            .unwrap()
    }

    #[test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each phase's borrow feeds a chain of derived references (strip -> \
    window_groups -> group -> tab) used through the assertions right after, or only builds \
    the fixture and is dropped at its block's closing brace; ending it earlier would mean \
    cloning test fixture data for no real gain"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn window_actions_open_and_duplicate_tabs_in_the_right_group() {
        // Phase `open_terminal_here_adds_a_tab_to_the_focused_group`.
        {
            let chrome = chrome_with_default_tree();
            let mut st: RefMut<'_, ChromeState> = chrome.borrow_mut();
            let before = focused_group_tab_count(&st);
            let group = focused_group(&st).unwrap();
            let id = open_terminal(&mut st, Destination::Here);
            assert_eq!(
                focused_group_tab_count(&st),
                before + 1,
                "the terminal joined the group the user was looking at"
            );
            let tree = st.tree();
            let g = tree
                .desktop
                .active_workspace()
                .unwrap()
                .strips
                .iter()
                .flat_map(|s| s.window_groups.iter())
                .find(|w| w.id == group)
                .unwrap();
            assert_eq!(g.active_tab, id, "the new tab is focused");
            let tab = g.tabs.iter().find(|t| t.id == id).unwrap();
            assert!(
                matches!(
                    &tab.tile,
                    TileContent::AppPlaceholder {
                        kind: AppKind::Terminal,
                        ..
                    }
                ),
                "a real terminal placeholder, not a stub: {:?}",
                tab.tile
            );
        }

        // Phase `open_terminal_beside_leaves_the_focused_group_alone_when_it_holds_one_tab`.
        {
            let chrome = chrome_with_default_tree();
            let mut st: RefMut<'_, ChromeState> = chrome.borrow_mut();
            // Collapse to a single tab so the "open beside" branch is under test.
            {
                let mut tree = st.tree_mut();
                let ws = tree.desktop.active_workspace_mut().unwrap();
                let g = &mut ws.strips[0].window_groups[0];
                g.tabs.truncate(1);
                g.active_tab = g.tabs[0].id;
            }
            let group = focused_group(&st).unwrap();
            let groups_before = {
                let tree = st.tree();
                tree.desktop.active_workspace().unwrap().strips[0]
                    .window_groups
                    .len()
            };
            open_terminal(&mut st, Destination::Beside);
            let tree = st.tree();
            let strip = &tree.desktop.active_workspace().unwrap().strips[0];
            assert_eq!(
                strip.window_groups.len(),
                groups_before + 1,
                "a single-window group gains a neighbour, not a tab"
            );
            let original = strip.window_groups.iter().find(|g| g.id == group).unwrap();
            assert_eq!(original.tabs.len(), 1, "the original group is untouched");
        }

        // Phase `duplicate_active_tab_copies_the_focused_app_into_the_same_group`.
        {
            let chrome = chrome_with_default_tree();
            let mut st: RefMut<'_, ChromeState> = chrome.borrow_mut();
            let group = focused_group(&st).unwrap();
            // Focus the Terminal tab so the duplicate has a known kind to copy.
            let terminal_tab = {
                let mut tree = st.tree_mut();
                let ws = tree.desktop.active_workspace_mut().unwrap();
                let g = ws.strips[0]
                    .window_groups
                    .iter_mut()
                    .find(|g| g.id == group)
                    .unwrap();
                let id = g
                    .tabs
                    .iter()
                    .find(|t| {
                        matches!(
                            &t.tile,
                            TileContent::AppPlaceholder {
                                kind: AppKind::Terminal,
                                ..
                            }
                        )
                    })
                    .map(|t| t.id)
                    .expect("the default tree seeds a terminal");
                g.activate_tab(id);
                id
            };
            let before = focused_group_tab_count(&st);
            let new_id = duplicate_active_tab(&mut st)
                .expect("a hosted tab duplicates")
                .expect("a hosted duplicate is a tab of this tree");
            assert_ne!(new_id, terminal_tab, "a duplicate is a new tab");
            assert_eq!(focused_group_tab_count(&st), before + 1);
            let tree = st.tree();
            let g = tree.desktop.active_workspace().unwrap().strips[0]
                .window_groups
                .iter()
                .find(|g| g.id == group)
                .unwrap();
            let dup = g.tabs.iter().find(|t| t.id == new_id).unwrap();
            assert!(
                matches!(
                    &dup.tile,
                    TileContent::AppPlaceholder {
                        kind: AppKind::Terminal,
                        ..
                    }
                ),
                "the duplicate is the same app kind: {:?}",
                dup.tile
            );
            assert_eq!(g.active_tab, new_id, "the duplicate takes focus");
        }

        // Phase `duplicating_a_foreign_window_reports_why_it_cannot`.
        {
            let chrome = chrome_with_default_tree();
            let mut st: RefMut<'_, ChromeState> = chrome.borrow_mut();
            {
                let mut tree = st.tree_mut();
                let ws = tree.desktop.active_workspace_mut().unwrap();
                let g = &mut ws.strips[0].window_groups[0];
                let id = next_id();
                g.tabs.push(Tab {
                    id,
                    title: "Foreign".into(),
                    tile: TileContent::Foreign {
                        app_id: "org.example.thing".into(),
                        surface_id: 7,
                    },
                });
                g.activate_tab(id);
            }
            assert_eq!(
                duplicate_active_tab(&mut st),
                Err(DuplicateError::ForeignWindow),
                "a client-owned window cannot be cloned by the compositor"
            );
        }
    }
}
