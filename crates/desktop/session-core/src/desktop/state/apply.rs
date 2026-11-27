//! Queued navigation, applied to the tree.
//!
//! A command is admitted against the tree when it is dispatched (see
//! [`crate::desktop::navigation`]), and lands here on the next pass of whatever presents the
//! desktop: the tree takes it, so peers and the next session read the shell it leaves, and the
//! pass answers what changed for each window, in order ([`RowOp`]), so the windows on screen
//! follow without being rebuilt.

use tilo_desktop_model::width::equalize_widths;
use tilo_desktop_model::{
    GroupId, Layout, RemovedTab, Strip, StripId, Tab, TabId, WindowGroup, WindowWidth, Workspace,
    WorkspaceId,
};
use ui::search::Scope;
use ui::widget::TabDescriptor;

use crate::desktop::navigation::{self, Admitted, HistoryDirection, RouteIds, ScrollAction};

use super::window_menu::{
    group_at, join_group, move_to_display, move_to_strip, move_to_workspace, stop_desktop_control,
    workspace_at,
};
use super::{
    ACTIVITY_APP, DesktopHandle, Shared, TileSource, descriptor, find_group, find_tab, floats,
    route_of, search,
};

/// What a window on screen does to follow a command the tree already took.
#[derive(Debug)]
pub enum RowOp {
    /// The window comes to the front of its strip, showing `tab`.
    Show(StripId, GroupId, TabId),
    /// A window renamed in the tree, as the row names its pill.
    Rename {
        strip: StripId,
        group: GroupId,
        tab: TabDescriptor,
    },
    Width(StripId, GroupId, WindowWidth),
    ToggleMaximized(StripId, GroupId),
    /// A window the strip scrolls to once it has laid it out.
    Reveal(StripId, GroupId),
    /// A launched window, already in the tree at `index` of `strip`, over the shell beside it
    /// when `float`.
    Open {
        strip: StripId,
        group: GroupId,
        index: usize,
        tab: Tab,
        float: bool,
    },
    /// A tab opened into a group that is already there, at `index` of its tabs, with the tab that
    /// ends up in front.
    TabInGroup {
        strip: StripId,
        group: GroupId,
        index: usize,
        tab: Tab,
        front: TabId,
        background: bool,
    },
    Close {
        strip: StripId,
        group: GroupId,
        tab: TabId,
        group_removed: bool,
    },
    /// The window is asked to float over the place it stands: only what presents it knows where
    /// that is ([`DesktopHandle::float_over`] places it in a work area).
    Float(StripId, GroupId),
    Pan(StripId, f64),
    Scroll(StripId, GroupId, ScrollAction),
    History(StripId, GroupId, TabId, HistoryDirection),
    /// The share row of a window: share it with its workspace's people, or stop sharing it.
    WindowShare(TabId, bool),
}

impl DesktopHandle {
    /// Apply the queued navigation to the tree, launching what it asks through `tiles`: what the
    /// windows on screen do to follow, in order, or nothing when no command was queued.
    pub fn apply_queued(&self, tiles: &mut dyn TileSource) -> Option<Vec<RowOp>> {
        let queue = std::mem::take(&mut self.shared.borrow_mut().queue);
        if queue.is_empty() {
            return None;
        }
        let mut ops = Vec::new();
        let mut shared = self.shared.borrow_mut();
        for command in queue {
            let resolved = resolve(&mut shared, tiles, command);
            if apply(&mut shared, tiles, resolved, &mut ops) {
                shared.changed();
            }
        }
        Some(ops)
    }

    /// Float the window `group` of `strip` the size a float takes, over the middle of `area`,
    /// the work area of the display it stands on. Whether the tree changed.
    pub fn float_over(&self, strip: StripId, group: GroupId, area: ui::kurbo::Rect) -> bool {
        let mut shared = self.shared.borrow_mut();
        let (width, height) = ui::floating_layer::FLOAT_SIZE;
        let at = ui::kurbo::Rect::from_center_size(
            area.center(),
            (width.min(area.width()), height.min(area.height())),
        );
        let place =
            tilo_desktop_model::FloatPlace::of_rect(floats::area_of(area), floats::place_of(at));
        let lifted = floats::set(&mut shared, strip, group, Some(place));
        if lifted {
            shared.tree.set_active_group(strip, group);
            shared.changed();
        }
        lifted
    }
}

/// What `command` comes to against the desktop as it is now: an app opens, or its window already
/// showing comes forward.
fn resolve(shared: &mut Shared, tiles: &dyn TileSource, command: Admitted) -> Admitted {
    match command {
        Admitted::Open(app) => navigation::resolve_open(&shared.tree, &app),
        // A record already open in a window comes forward.
        Admitted::Launch(app, float) => match tiles
            .showing(&app)
            .and_then(|tab| route_of(&shared.tree, tab))
        {
            Some(ids) => Admitted::Select(ids),
            None => Admitted::Launch(app, float),
        },
        // The workspace's history is the Activity app's to show: it opens, or comes forward, and
        // is offered the view.
        Admitted::Activity(_, view, author) => {
            let link = crate::desktop::activity::ActivityDeepLink {
                author,
                ..crate::desktop::activity::ActivityDeepLink::default()
            };
            shared.activity_focus = Some((view, link));
            navigation::resolve_open(&shared.tree, ACTIVITY_APP)
        }
        other => other,
    }
}

/// A reopened window, `reopened`, back on the desktop: the row takes it in as it takes a window a
/// peer added, and the strip comes to it as it does to a launched one. Whether anything came back.
fn put_back(
    shared: &mut Shared,
    tiles: &mut dyn TileSource,
    ops: &mut Vec<RowOp>,
    reopened: Option<tilo_desktop_model::Reopened>,
) -> bool {
    let Some(reopened) = reopened else {
        return false;
    };
    ops.push(RowOp::Reveal(reopened.strip, reopened.group));
    shared.replaced = true;
    // A window an app's end took comes back over the card that said so: the card goes with it.
    shared.ended.remove(&reopened.tab);
    // The window that closed is gone for good, so the tab comes back as the app that opens it
    // again: the app is asked for, on what the window showed when the close knew it, and the
    // window that comes takes this tab.
    if let Some((workspace, tab)) = find_tab(&shared.tree, reopened.strip, reopened.tab) {
        tiles.reopened(workspace, tab, reopened.launch.as_deref());
    }
    true
}

fn apply(
    shared: &mut Shared,
    tiles: &mut dyn TileSource,
    command: Admitted,
    ops: &mut Vec<RowOp>,
) -> bool {
    // Admission ran at dispatch; an earlier command in this batch may since have removed the
    // target.
    let live = |tree: &Layout, ids: RouteIds| navigation::locate(tree, ids).is_some();
    match command {
        Admitted::Select(ids) if live(&shared.tree, ids) => select(shared, ids, ops),
        Admitted::Width(ids, permille) if live(&shared.tree, ids) => {
            resized(shared, ids, permille, ops)
        }
        Admitted::Maximize(ids, maximized) if live(&shared.tree, ids) => {
            set_maximized(shared, ids, maximized, ops)
        }
        Admitted::Fullscreen(ids, fullscreen) if live(&shared.tree, ids) => {
            // The window taken across the display takes the whole strip with it.
            shared.tree.set_fullscreen(ids.strip, ids.group, fullscreen);
            true
        }
        // A chord, a menu row or an agent floats the window over the place it stands, which what
        // presents it knows, or puts it back in its strip, as an arranging action the host makes.
        Admitted::Float(ids, true) if live(&shared.tree, ids) => {
            ops.push(RowOp::Float(ids.strip, ids.group));
            false
        }
        Admitted::Float(ids, false) if live(&shared.tree, ids) => unfloated(shared, ids),
        Admitted::Rename(ids, name) if live(&shared.tree, ids) => renamed(shared, ids, &name, ops),
        Admitted::Close(ids) if live(&shared.tree, ids) => {
            let removed = shared.close(ids.tab);
            closed(ops, ids.tab, removed)
        }
        Admitted::Dismiss(ids) if live(&shared.tree, ids) => {
            let removed = shared.tree.close_tab(ids.tab);
            closed(ops, ids.tab, removed)
        }
        // The window comes back where it was, its app launched again if that is what it takes.
        Admitted::Reopen(workspace, app) => {
            let reopened = shared.tree.reopen_closed(workspace, app.as_deref());
            put_back(shared, tiles, ops, reopened)
        }
        Admitted::ReopenWindow(tab) => {
            let reopened = shared.tree.reopen_closed_window(tab);
            put_back(shared, tiles, ops, reopened)
        }
        Admitted::Strip(workspace, strip) => switched(shared, workspace, Some(strip)),
        Admitted::Workspace(workspace) => switched(shared, workspace, None),
        Admitted::Launch(app, float) => pushed(ops, open(tiles, shared, &app, float, None)),
        Admitted::LaunchAt(app, index) => {
            pushed(ops, open(tiles, shared, &app, false, Some(index)))
        }
        // The tab lands right after the one it was asked to open beside, inside its window group.
        Admitted::TabOpenBeside(ids, app, background) if live(&shared.tree, ids) => {
            pushed(ops, open_tab_beside(tiles, shared, ids, &app, background))
        }
        // The tab it was asked to open beside is gone: its window ended while the request was
        // being carried out. The window lands as any launch's does.
        Admitted::TabOpenBeside(_, app, _) => pushed(ops, open(tiles, shared, &app, false, None)),
        // Opening search changes no model state.
        Admitted::Search(query) => searched(shared, query),
        // Presentation only: the tree does not change.
        Admitted::Pan(strip, offset) => {
            ops.push(RowOp::Pan(strip, offset));
            false
        }
        Admitted::Scroll(ids, action) if live(&shared.tree, ids) => {
            ops.push(RowOp::Scroll(ids.strip, ids.group, action));
            false
        }
        Admitted::History(ids, direction) if live(&shared.tree, ids) => {
            ops.push(RowOp::History(ids.strip, ids.group, ids.tab, direction));
            false
        }
        Admitted::CloseActivity(_) => close_activity(shared, ops),
        Admitted::NotificationCenter(open) => notification_centre(shared, open),
        // A window of an app this device started runs in the workspace that started it, with its
        // grants and its records: the tab does not cross by itself. The session carries it
        // instead, asking first when the move crosses to or from a workspace other people are in,
        // and this tab takes the window the app opens where the person put it.
        Admitted::MoveToWorkspace(ids, to)
            if live(&shared.tree, ids)
                && shared.window_of_tab(ids.tab).is_some()
                && tiles.carry(ids.tab, to) =>
        {
            false
        }
        // Another device opens the window's app on the same record: the session hands it over
        // through the record of the person's devices, and closes this window once it did.
        Admitted::MoveToDevice(ids, device, output) => {
            if live(&shared.tree, ids) {
                shared.device_moves.push((ids.tab, device, output));
                shared.wake.wake();
            }
            false
        }
        // Everything else is a window menu's row: the desktop's own moves, widths and closes of
        // one window.
        other => window_menu(shared, &other, ops),
    }
}

/// The window the person chose: it comes to the front of its strip with the keyboard, and a window
/// maximized over it stands beside it again so the chosen one shows.
fn select(shared: &mut Shared, ids: RouteIds, ops: &mut Vec<RowOp>) -> bool {
    let tree = &mut shared.tree;
    tree.active = ids.workspace;
    tree.active_strip.insert(ids.workspace, ids.strip);
    tree.set_active_group(ids.strip, ids.group);
    if let Some(group) = find_group(tree, ids.strip, ids.group) {
        group.activate_tab(ids.tab);
    }
    // Another window of the strip maximized would hide the one focused: it stands beside it again.
    let covering = restore_covering(tree, ids.strip, ids.group);
    let restored = !covering.is_empty();
    ops.extend(
        covering
            .into_iter()
            .map(|group| RowOp::ToggleMaximized(ids.strip, group)),
    );
    ops.push(RowOp::Show(ids.strip, ids.group, ids.tab));
    if restored {
        ops.push(RowOp::Reveal(ids.strip, ids.group));
    }
    true
}

/// The window's share of the strip, as the person gave it, and the window the desktop follows.
fn resized(shared: &mut Shared, ids: RouteIds, permille: u32, ops: &mut Vec<RowOp>) -> bool {
    let Some(group) = find_group(&mut shared.tree, ids.strip, ids.group) else {
        return true;
    };
    group.width = WindowWidth::Fraction(permille);
    ops.push(RowOp::Width(ids.strip, ids.group, group.width));
    shared.followed = Some(ids.group);
    true
}

/// The window taken across its display, or put beside its neighbours again.
fn set_maximized(
    shared: &mut Shared,
    ids: RouteIds,
    maximized: bool,
    ops: &mut Vec<RowOp>,
) -> bool {
    if let Some(group) = find_group(&mut shared.tree, ids.strip, ids.group)
        && group.maximized != maximized
    {
        group.maximized = maximized;
        ops.push(RowOp::ToggleMaximized(ids.strip, ids.group));
    }
    true
}

/// The window named: the tab that was named changes its pill where it stands, never the row.
fn renamed(shared: &mut Shared, ids: RouteIds, name: &str, ops: &mut Vec<RowOp>) -> bool {
    if let Some(group) = find_group(&mut shared.tree, ids.strip, ids.group)
        && let Some(tab) = group.tabs.iter_mut().find(|tab| tab.id == ids.tab)
        && tab.rename(name)
    {
        ops.push(RowOp::Rename {
            strip: ids.strip,
            group: ids.group,
            tab: descriptor(tab),
        });
    }
    true
}

/// The tab that closed, its window's row told where it stood.
fn closed(ops: &mut Vec<RowOp>, tab: TabId, removed: Option<RemovedTab>) -> bool {
    if let Some(removed) = removed {
        ops.push(RowOp::Close {
            strip: removed.strip,
            group: removed.group,
            tab,
            group_removed: removed.group_removed,
        });
    }
    true
}

/// The window floated back into its strip, where the strip shows it again.
fn unfloated(shared: &mut Shared, ids: RouteIds) -> bool {
    if floats::set(shared, ids.strip, ids.group, None) {
        shared.tree.set_active_group(ids.strip, ids.group);
    }
    true
}

/// The workspace on the shell, `strip` in front of it when one names the window to show.
fn switched(shared: &mut Shared, workspace: WorkspaceId, strip: Option<StripId>) -> bool {
    shared.tree.active = workspace;
    if let Some(strip) = strip {
        shared.tree.active_strip.insert(workspace, strip);
    }
    true
}

/// Search opened on the workspace in front, with what the person typed if they typed.
fn searched(shared: &mut Shared, query: Option<String>) -> bool {
    shared.search_request = Some(search::SearchRequest::Open {
        scope: Scope::Workspace,
        query,
    });
    false
}

/// The notification centre asked to show or put away, where this device serves one.
fn notification_centre(shared: &Shared, open: bool) -> bool {
    if let Some(notifications) = &shared.notifications {
        notifications.ask_centre(open);
    }
    false
}

/// The Activity app's windows close as any window does.
fn close_activity(shared: &mut Shared, ops: &mut Vec<RowOp>) -> bool {
    while let Some(ids) = navigation::window_of_app(&shared.tree, ACTIVITY_APP) {
        let Some(removed) = shared.tree.close_tab(ids.tab) else {
            break;
        };
        ops.push(RowOp::Close {
            strip: removed.strip,
            group: removed.group,
            tab: ids.tab,
            group_removed: removed.group_removed,
        });
    }
    true
}
/// Push `op` when there is one. Whether there was.
fn pushed(ops: &mut Vec<RowOp>, op: Option<RowOp>) -> bool {
    op.map(|op| ops.push(op)).is_some()
}

/// Restore every window of the strip `strip` maximized over the group `shown`, so `shown` stands
/// in view; the groups restored.
fn restore_covering(tree: &mut Layout, strip: StripId, shown: GroupId) -> Vec<GroupId> {
    let Some(strip) = tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| workspace.strips.iter_mut())
        .find(|held| held.id == strip)
    else {
        return Vec::new();
    };
    strip
        .window_groups
        .iter_mut()
        .filter(|group| group.maximized && group.id != shown)
        .map(|group| {
            group.maximized = false;
            group.id
        })
        .collect()
}

/// Put a window for `app` after the focused one of the shown strip, or at the place `at` in it (a
/// drop's [`super::StripPart::index`]), over the shell beside it when the app asked for a window
/// that floats.
fn open(
    tiles: &mut dyn TileSource,
    shared: &mut Shared,
    app: &str,
    float: bool,
    at: Option<usize>,
) -> Option<RowOp> {
    let (tab_id, group_id) = (shared.minter.mint(), shared.minter.mint());
    let workspace_id = shared.tree.active_workspace()?.id();
    let workspace = shared
        .tree
        .workspaces
        .iter()
        .find(|w| w.id() == workspace_id)?;
    let tab = tiles.launch(workspace, tab_id, app, None)?;
    let shown = shared.tree.active_strip_for(workspace);
    // A workspace a site lets this computer into comes with the strips its site holds, and one
    // joined before its site recorded any has none. The first window opened there makes the
    // strip it goes in.
    let strip = if let Some(strip) = shown {
        strip
    } else {
        let strip = Strip::new(StripId(shared.minter.mint()), "");
        let id = strip.id;
        let workspace = shared
            .tree
            .workspaces
            .iter_mut()
            .find(|w| w.id() == workspace_id)?;
        workspace.strips.push(strip);
        shared.tree.active_strip.insert(workspace_id, id);
        id
    };
    let after = shared.tree.active_group_for(strip);
    let model = shared
        .tree
        .workspaces
        .iter_mut()
        .flat_map(|w| w.strips.iter_mut())
        .find(|s| s.id == strip)?;
    let index = match at {
        Some(at) => at.min(model.window_groups.len()),
        None => after
            .and_then(|id| model.window_groups.iter().position(|g| g.id == id))
            .map_or(model.window_groups.len(), |position| {
                position.saturating_add(1)
            }),
    };
    model.window_groups.insert(
        index,
        WindowGroup {
            id: group_id,
            tabs: vec![tab.clone()],
            active_tab: tab.id,
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        },
    );
    // A window group the canvas does not place is a window no layout draws: the shell a person
    // just opened an app on would look empty. The group takes a place as any other group does,
    // by the same rule.
    if let Some(workspace) = shared
        .tree
        .workspaces
        .iter_mut()
        .find(|w| w.id() == workspace_id)
    {
        let Workspace { strips, layout, .. } = workspace;
        layout.place_missing(strips);
    }
    shared.tree.set_active_group(strip, group_id);
    shared.tree.external_revision = shared.tree.external_revision.wrapping_add(1);
    Some(RowOp::Open {
        strip,
        group: group_id,
        index,
        tab,
        float,
    })
}

/// Put a window for `app` into the group of the window at `ids`, as its tab right after it: what
/// an app's request to open beside one of its own tabs does. A tab opened behind stays where it
/// opened; one opened in front is the strip's active window.
fn open_tab_beside(
    tiles: &mut dyn TileSource,
    shared: &mut Shared,
    ids: RouteIds,
    app: &str,
    background: bool,
) -> Option<RowOp> {
    let tab_id = shared.minter.mint();
    let workspace = shared
        .tree
        .workspaces
        .iter()
        .find(|workspace| workspace.id() == ids.workspace)?;
    let tab = tiles.launch(workspace, tab_id, app, Some(ids.tab))?;
    let group = shared
        .tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id() == ids.workspace)?
        .strips
        .iter_mut()
        .find(|strip| strip.id == ids.strip)?
        .window_groups
        .iter_mut()
        .find(|group| group.id == ids.group)?;
    let index = group
        .tabs
        .iter()
        .position(|tab| tab.id == ids.tab)
        .map_or(group.tabs.len(), |position| position.saturating_add(1));
    group.tabs.insert(index, tab.clone());
    let front = if background {
        group.active_tab
    } else {
        group.activate_tab(tab.id);
        tab.id
    };
    if !background {
        // The person asked for this window: bring its group into view on the strip and focus it,
        // even when the tab it opens beside stands past the strip's edge.
        shared.tree.set_active_group(ids.strip, ids.group);
        shared.queue.push(Admitted::Select(RouteIds {
            workspace: ids.workspace,
            strip: ids.strip,
            group: ids.group,
            tab: tab.id,
        }));
    }
    shared.tree.external_revision = shared.tree.external_revision.wrapping_add(1);
    Some(RowOp::TabInGroup {
        strip: ids.strip,
        group: ids.group,
        index,
        tab,
        front,
        background,
    })
}

/// Apply a window-menu command to the tree, naming the follow-ups the windows need. Whether the
/// tree changed.
fn window_menu(shared: &mut Shared, command: &Admitted, ops: &mut Vec<RowOp>) -> bool {
    match command {
        Admitted::MoveToStrip(ids, strip, before) => move_to_strip(shared, *ids, *strip, *before),
        Admitted::JoinGroup(ids, into) => join_group(shared, *ids, ids.strip, *into),
        Admitted::MoveToWorkspace(ids, to) => move_to_workspace(shared, *ids, *to),
        Admitted::MoveToDisplay(ids, display) => move_to_display(shared, ids.group, *display),
        Admitted::WidthPreset(ids, width) => width_preset(shared, *ids, *width, ops),
        Admitted::EqualizeWidths(ids) => equalize(shared, *ids, ops),
        // The share is the sharing app's to keep: the ask names the row the window's bar carries,
        // and the app does the same with it.
        Admitted::ShareWindow(ids, shared_now) => {
            ops.push(RowOp::WindowShare(ids.tab, *shared_now));
            true
        }
        Admitted::SplitRight(ids) => detach(shared, *ids, false, ops),
        Admitted::MoveToNewGroup(ids) => detach(shared, *ids, true, ops),
        Admitted::CloseWindow(ids) => close_window(shared, *ids, ops),
        Admitted::StopDesktopControl(ids) => stop_desktop_control(shared, *ids),
        _ => false,
    }
}

/// Give the window at `ids` the share `width` of its strip.
fn width_preset(
    shared: &mut Shared,
    ids: RouteIds,
    width: WindowWidth,
    ops: &mut Vec<RowOp>,
) -> bool {
    let Some(group) = find_group(&mut shared.tree, ids.strip, ids.group) else {
        return false;
    };
    group.width = width;
    ops.push(RowOp::Width(ids.strip, ids.group, width));
    shared.followed = Some(ids.group);
    true
}

/// Give every window of the strip at `ids` the same share of it.
fn equalize(shared: &mut Shared, ids: RouteIds, ops: &mut Vec<RowOp>) -> bool {
    let Some(strip) = shared
        .tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id() == ids.workspace)
        .and_then(|workspace| {
            workspace
                .strips
                .iter_mut()
                .find(|strip| strip.id == ids.strip)
        })
    else {
        return false;
    };
    if equalize_widths(strip) {
        ops.extend(
            strip
                .window_groups
                .iter()
                .map(|group| RowOp::Width(ids.strip, group.id, group.width)),
        );
    }
    shared.followed = Some(ids.group);
    true
}

/// Close every tab of the window at `ids`: the window goes, and a reopen brings its tabs back.
fn close_window(shared: &mut Shared, ids: RouteIds, ops: &mut Vec<RowOp>) -> bool {
    // Backwards, so each close remembers the window's own place and a reopen puts the tabs back in
    // the order they were in.
    let tabs: Vec<TabId> = group_at(&shared.tree, &ids)
        .map(|group| group.tabs.iter().rev().map(|tab| tab.id).collect())
        .unwrap_or_default();
    let mut last = None;
    for tab in tabs {
        match shared.close(tab) {
            Some(removed) => last = Some(removed),
            None => break,
        }
    }
    let Some(removed) = last else {
        return false;
    };
    ops.push(RowOp::Close {
        strip: removed.strip,
        group: removed.group,
        tab: removed.tab.id,
        group_removed: removed.group_removed,
    });
    true
}

/// Detach the tab at `ids` into a window of its own, at the strip's end (`at_end`) or right after
/// the window it is in, and make it the one the strip shows: the strip scrolls to it. Whether it
/// moved.
fn detach(shared: &mut Shared, ids: RouteIds, at_end: bool, ops: &mut Vec<RowOp>) -> bool {
    let Some(strip) = workspace_at(&shared.tree, &ids)
        .and_then(|workspace| workspace.strips.iter().find(|strip| strip.id == ids.strip))
    else {
        return false;
    };
    let insert_at = match strip
        .window_groups
        .iter()
        .position(|group| group.id == ids.group)
    {
        Some(index) if !at_end => index.saturating_add(1),
        Some(_) => strip.window_groups.len(),
        None => return false,
    };
    let Shared { tree, minter, .. } = shared;
    let Some(moved) = tree.split_tab_into_new_group(ids.strip, ids.tab, insert_at, minter) else {
        return false;
    };
    tree.set_active_group(ids.strip, moved.new_group.id);
    ops.push(RowOp::Reveal(ids.strip, moved.new_group.id));
    shared.replaced = true;
    true
}
