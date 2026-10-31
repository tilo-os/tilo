//! What the person can do to one window, as the desktop offers it.
//!
//! The rows are the desktop's own and no app's: going to a window that asked
//! for the person, covering the display with it, filling the strip with it
//! and giving the strip back, carrying it into another strip, workspace or
//! display, giving it a share of its strip, opening another tab of its app,
//! sharing it with the people of its workspace, naming it, splitting a tab
//! out into a window of its own, closing the tab or the window, and stopping
//! an app from controlling the desktop beyond it. Every row is a navigation
//! command the desktop already answers, so a menu — the overflow menu in the
//! window's own bar, or one a sheet mounts beside it — offers exactly what
//! the desktop can do, and choosing a row sends the same command a chord or
//! a control sends.
//!
//! Which rows apply follows the window: a window covering its display offers
//! it back, a window filling the strip offers the strip back instead of
//! filling it, the widest window cannot be widened, a window asking for the
//! person offers to be gone to only until it has them, a window of one tab
//! offers no split, and a window someone else holds can be neither named,
//! moved nor closed. A place with nowhere to go — another strip, another
//! workspace, another display — keeps its row, disabled, with the reason, so
//! the menu reads the same for every window.

use tilo_desktop_model::commands::{DesktopCommand, apply_command, move_tab_to_workspace};
use tilo_desktop_model::width::{
    WIDTH_NARROW_PERMILLE, WIDTH_NORMAL_PERMILLE, WIDTH_WIDE_PERMILLE, equalize_widths,
};
use tilo_desktop_model::{
    DesktopTree, StripId, Tab, TabId, TileContent, WindowGroup, Workspace, WorkspaceId,
};

use crate::i18n::{t, t_args};
use crate::navigation::{Command, Route, RouteIds, WidthPreset};

use super::Shared;

pub const REVEAL: &str = "window-reveal";
pub const FULLSCREEN: &str = "window-fullscreen";
pub const EXIT_FULLSCREEN: &str = "window-exit-fullscreen";
pub const MAXIMIZE: &str = "window-maximize";
pub const RESTORE: &str = "window-restore";
pub const WIDER: &str = "window-wider";
pub const NARROWER: &str = "window-narrower";
pub const HALF: &str = "window-width-half";
pub const WIDTH_NARROW: &str = "window-width-narrow";
pub const WIDTH_NORMAL: &str = "window-width-normal";
pub const WIDTH_WIDE: &str = "window-width-wide";
pub const EQUALIZE: &str = "window-width-equal";
pub const MOVE_TO_STRIP: &str = "window-move-to-strip";
pub const MOVE_TO_WORKSPACE: &str = "window-move-to-workspace";
pub const MOVE_TO_DISPLAY: &str = "window-move-to-display";
pub const NEW_TAB: &str = "window-new-tab";
pub const SHARE: &str = "window-share";
pub const UN_SHARE: &str = "window-unshare";
pub const RENAME: &str = "window-rename";
pub const SPLIT_RIGHT: &str = "window-split-right";
pub const NEW_GROUP: &str = "window-new-group";
pub const CLOSE: &str = "window-close";
pub const CLOSE_WINDOW: &str = "window-close-window";
pub const STOP_DESKTOP_CONTROL: &str = "window-stop-desktop-control";

/// The widths a wider or a narrower window lands on, narrowest first.
const WIDTHS: [u32; 3] = [
    WIDTH_NARROW_PERMILLE,
    WIDTH_NORMAL_PERMILLE,
    WIDTH_WIDE_PERMILLE,
];

/// The share row a window offers, when this desktop can share it: how it
/// reads, and whether the window is shared now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sharing {
    pub label: String,
    pub shared: bool,
}

/// What the desktop knows about one window beside its place in the tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The person who holds the window on a shared desktop, when someone
    /// else does: their window is theirs to move, to name and to close.
    pub held: Option<String>,
    /// The window asked for the person and has not had them yet.
    pub asking: bool,
    /// The window runs on this device, so its app can be opened again here.
    pub local: bool,
    /// This person opens windows and tabs on this desktop (someone let only
    /// look is offered none).
    pub opens: bool,
    /// The share row the window offers; `None` for a window this desktop
    /// cannot share.
    pub sharing: Option<Sharing>,
}

/// One row of a window's menu: what it does, how it reads, and why it
/// cannot be chosen when it cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The stable id a menu host sends back with the window it is for.
    pub id: String,
    pub label: String,
    /// Whether the person can choose it now; a row they cannot is shown
    /// with `reason` beside it.
    pub enabled: bool,
    pub reason: Option<String>,
}

impl Row {
    fn offered(id: impl Into<String>, label: String) -> Self {
        Self {
            id: id.into(),
            label,
            enabled: true,
            reason: None,
        }
    }

    /// A row shown for want of anywhere to go: it says why.
    fn refused(id: impl Into<String>, label: String, reason: String) -> Self {
        Self {
            id: id.into(),
            label,
            enabled: false,
            reason: Some(reason),
        }
    }

    /// A row for the window's own person: anyone else sees why they cannot
    /// choose it.
    fn theirs(self, own: bool, reason: Option<String>) -> Self {
        if own {
            self
        } else {
            Self {
                enabled: false,
                reason,
                ..self
            }
        }
    }
}

/// The id of the row carrying the window into strip `strip`.
fn move_to_strip_id(strip: StripId) -> String {
    format!("{MOVE_TO_STRIP}:{}", strip.0)
}

/// The id of the row taking the window into workspace `workspace`.
fn move_to_workspace_id(workspace: WorkspaceId) -> String {
    format!("{MOVE_TO_WORKSPACE}:{}", workspace.0)
}

/// The id of the row showing the window on display `display`.
fn move_to_display_id(display: u32) -> String {
    format!("{MOVE_TO_DISPLAY}:{display}")
}

/// The menu rows that apply to the window at `route`, in the order to show
/// them, as `facts` describes the window beyond its place in the tree.
pub fn rows(tree: &DesktopTree, route: &RouteIds, facts: &Facts) -> Vec<Row> {
    let Some(group) = group_at(tree, route) else {
        return Vec::new();
    };
    let Some(tab) = group.tabs.iter().find(|tab| tab.id == route.tab) else {
        return Vec::new();
    };
    let own = facts.held.is_none();
    let held = || {
        facts
            .held
            .clone()
            .map(|name| t_args("window-menu-someone-elses", &[("name", name.into())]))
    };
    let permille = group.width.permille();
    let wider = WIDTHS.iter().any(|width| *width > permille);
    let narrower = WIDTHS.iter().any(|width| *width < permille);
    let fullscreen = tree.fullscreen_group() == Some(group.id);
    let renamable = may_rename(tab) && facts.local;
    let mut rows = Vec::new();
    if facts.asking {
        rows.push(Row::offered(REVEAL, t("window-menu-reveal")));
    }
    rows.push(Row::offered(
        if fullscreen {
            EXIT_FULLSCREEN
        } else {
            FULLSCREEN
        },
        t(if fullscreen {
            "window-menu-exit-full-screen"
        } else {
            "window-menu-full-screen"
        }),
    ));
    rows.push(Row::offered(
        if group.maximized { RESTORE } else { MAXIMIZE },
        t(if group.maximized {
            "window-menu-restore"
        } else {
            "window-menu-maximize"
        }),
    ));
    // Where the window can go is the tree's to say; carrying it there is the
    // owner's, not a viewer's.
    rows.extend(
        move_rows(tree, route)
            .into_iter()
            .map(|row| row.theirs(own, held())),
    );
    rows.extend([
        Row::offered(HALF, t("window-menu-width-half")),
        Row::offered(WIDTH_NARROW, t("window-menu-width-narrow")),
        Row::offered(WIDTH_NORMAL, t("window-menu-width-normal")),
        Row::offered(WIDTH_WIDE, t("window-menu-width-wide")),
        Row::offered(EQUALIZE, t("window-menu-width-equal")),
        Row {
            id: WIDER.into(),
            label: t("window-menu-wider"),
            enabled: wider,
            reason: (!wider).then(|| t("window-menu-widest")),
        },
        Row {
            id: NARROWER.into(),
            label: t("window-menu-narrower"),
            enabled: narrower,
            reason: (!narrower).then(|| t("window-menu-narrowest")),
        },
    ]);
    if let Some(sharing) = &facts.sharing {
        rows.push(Row::offered(
            if sharing.shared { UN_SHARE } else { SHARE },
            sharing.label.clone(),
        ));
    }
    rows.push(
        Row::offered(NEW_TAB, t("window-menu-new-tab"))
            .theirs(own, held())
            .theirs(facts.opens, Some(t("window-menu-not-yours-to-open")))
            .theirs(app_of(tab).is_some(), Some(t("window-menu-no-app"))),
    );
    if renamable {
        rows.push(Row::offered(RENAME, t("window-menu-rename")).theirs(own, held()));
    }
    if group.tabs.len() >= 2 {
        rows.push(Row::offered(SPLIT_RIGHT, t("window-menu-split-right")).theirs(own, held()));
        rows.push(Row::offered(NEW_GROUP, t("window-menu-new-group")).theirs(own, held()));
        rows.push(Row::offered(CLOSE, t("window-menu-close")).theirs(own, held()));
    }
    rows.push(Row::offered(CLOSE_WINDOW, t("window-menu-close-window")).theirs(own, held()));
    if controls_desktop(tree, route) {
        rows.push(Row::offered(
            STOP_DESKTOP_CONTROL,
            t("window-menu-stop-desktop-control"),
        ));
    }
    rows
}

/// The rows carrying the window to another strip, workspace or display, each
/// place by its name. A place with nowhere to go keeps its row, disabled.
fn move_rows(tree: &DesktopTree, route: &RouteIds) -> Vec<Row> {
    let mut rows = Vec::new();
    let strips: Vec<(StripId, String)> = workspace_at(tree, route)
        .map(|workspace| {
            workspace
                .strips
                .iter()
                .enumerate()
                .filter(|(_, strip)| strip.id != route.strip)
                .map(|(index, strip)| (strip.id, strip_shown_name(&strip.name, index)))
                .collect()
        })
        .unwrap_or_default();
    if strips.is_empty() {
        rows.push(Row::refused(
            MOVE_TO_STRIP,
            t("window-menu-move-to-strip"),
            t("window-menu-no-other-strip"),
        ));
    }
    for (strip, name) in strips {
        rows.push(Row::offered(
            move_to_strip_id(strip),
            t_args("window-menu-move-to-strip-named", &[("place", name.into())]),
        ));
    }
    let workspaces: Vec<(WorkspaceId, String)> = tree
        .workspaces
        .iter()
        .filter(|workspace| workspace.id != route.workspace)
        .map(|workspace| (workspace.id, workspace.name.clone()))
        .collect();
    if workspaces.is_empty() {
        rows.push(Row::refused(
            MOVE_TO_WORKSPACE,
            t("window-menu-move-to-workspace"),
            t("window-menu-no-other-workspace"),
        ));
    }
    for (workspace, name) in workspaces {
        rows.push(Row::offered(
            move_to_workspace_id(workspace),
            t_args(
                "window-menu-move-to-workspace-named",
                &[("place", name.into())],
            ),
        ));
    }
    let here = tree.outputs.display_of(route.group);
    let displays: Vec<(u32, String)> = tree
        .outputs
        .reading_order()
        .into_iter()
        .enumerate()
        .filter(|(_, display)| Some(*display) != here)
        .map(|(index, display)| {
            (
                display,
                t_args(
                    "desktop-display-name",
                    &[("number", place_number_of(index).into())],
                ),
            )
        })
        .collect();
    if displays.is_empty() {
        rows.push(Row::refused(
            MOVE_TO_DISPLAY,
            t("window-menu-move-to-display"),
            t("window-menu-no-other-display"),
        ));
    }
    for (display, name) in displays {
        rows.push(Row::offered(
            move_to_display_id(display),
            t_args(
                "window-menu-move-to-display-named",
                &[("place", name.into())],
            ),
        ));
    }
    rows
}

/// `index` as the number a person counts from, one on.
fn place_number_of(index: usize) -> i64 {
    i64::try_from(index.saturating_add(1)).unwrap_or(i64::MAX)
}

/// A strip's name as the strip switcher shows it: "Strip 2" for one nobody
/// named, by its place in the workspace.
fn strip_shown_name(name: &str, index: usize) -> String {
    if name.trim().is_empty() {
        t_args(
            "strip-switcher-unnamed",
            &[("number", place_number_of(index).into())],
        )
    } else {
        name.to_owned()
    }
}

/// The navigation command row `id` of the window at `route` means, for a
/// desktop whose generation is `generation`; `name` is what the person
/// typed for [`RENAME`]. `None` for a row that does not apply here.
pub fn command(
    tree: &DesktopTree,
    route: &RouteIds,
    id: &str,
    name: Option<&str>,
    generation: &str,
) -> Option<Command> {
    let group = group_at(tree, route)?;
    let tab = group.tabs.iter().find(|tab| tab.id == route.tab)?;
    let generation = generation.to_owned();
    let route = route_of(route);
    Some(match id {
        REVEAL => Command::Select { generation, route },
        RENAME => Command::Rename {
            generation,
            route,
            name: name?.to_owned(),
        },
        FULLSCREEN | EXIT_FULLSCREEN => Command::Fullscreen {
            generation,
            route,
            fullscreen: id == FULLSCREEN,
        },
        MAXIMIZE | RESTORE => Command::Maximize {
            generation,
            route,
            maximized: id == MAXIMIZE,
        },
        WIDER => Command::Width {
            generation,
            route,
            permille: WIDTHS
                .iter()
                .copied()
                .find(|width| *width > group.width.permille())?,
        },
        NARROWER => Command::Width {
            generation,
            route,
            permille: WIDTHS
                .iter()
                .rev()
                .copied()
                .find(|width| *width < group.width.permille())?,
        },
        HALF | WIDTH_NARROW | WIDTH_NORMAL | WIDTH_WIDE => Command::WidthPreset {
            generation,
            route,
            preset: match id {
                HALF => WidthPreset::Half,
                WIDTH_NARROW => WidthPreset::Narrow,
                WIDTH_NORMAL => WidthPreset::Normal,
                _ => WidthPreset::Wide,
            },
        },
        EQUALIZE => Command::EqualizeWidths { generation, route },
        SHARE | UN_SHARE => Command::ShareWindow {
            generation,
            route,
            shared: id == SHARE,
        },
        NEW_TAB => Command::TabOpenBeside {
            generation,
            app: app_of(tab)?,
            beside: route,
            background: false,
        },
        SPLIT_RIGHT => Command::SplitRight { generation, route },
        NEW_GROUP => Command::MoveToNewGroup { generation, route },
        CLOSE => Command::Close { generation, route },
        CLOSE_WINDOW => Command::CloseWindow { generation, route },
        STOP_DESKTOP_CONTROL => Command::StopDesktopControl { generation, route },
        place => {
            if let Some(strip) = place.strip_prefix(MOVE_TO_STRIP) {
                Command::MoveToStrip {
                    generation,
                    route,
                    strip: place_number(strip)?.to_string(),
                }
            } else if let Some(workspace) = place.strip_prefix(MOVE_TO_WORKSPACE) {
                Command::MoveToWorkspace {
                    generation,
                    route,
                    workspace: place_number(workspace)?.to_string(),
                }
            } else {
                Command::MoveToDisplay {
                    generation,
                    route,
                    display: u32::try_from(place_number(place.strip_prefix(MOVE_TO_DISPLAY)?)?)
                        .ok()?,
                }
            }
        }
    })
}

/// The id `place` names after its `kind:` prefix.
fn place_number(place: &str) -> Option<u64> {
    place.strip_prefix(':')?.parse().ok()
}

/// The app the window `tab` runs.
fn app_of(tab: &Tab) -> Option<String> {
    match &tab.tile {
        TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. } => {
            Some(app_id.clone())
        }
        _ => None,
    }
}

/// Whether the person may name the window `tab`: a host (non-Tilo) window's
/// tab, whose title is the window's own rather than an app's resource.
fn may_rename(tab: &Tab) -> bool {
    matches!(
        tab.tile,
        TileContent::Foreign { .. } | TileContent::HostedWindow { .. }
    )
}

/// Whether the window at `route` runs an app that controls this desktop
/// beyond its own window now.
fn controls_desktop(tree: &DesktopTree, route: &RouteIds) -> bool {
    let Some((workspace, app)) = holder_of(tree, route) else {
        return false;
    };
    workspace.holds_desktop_control_at(&app, tilo_clock::unix_now())
}

/// The workspace holding the window at `route`, and the app it runs.
fn holder_of<'a>(tree: &'a DesktopTree, route: &RouteIds) -> Option<(&'a Workspace, String)> {
    let group = group_at(tree, route)?;
    let tab = group.tabs.iter().find(|tab| tab.id == route.tab)?;
    Some((workspace_at(tree, route)?, app_of(tab)?))
}

/// The workspace at `route`, while the tree still holds it.
fn workspace_at<'a>(tree: &'a DesktopTree, route: &RouteIds) -> Option<&'a Workspace> {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.id == route.workspace)
}

/// The window group at `route`, while the tree still holds it.
fn group_at<'a>(tree: &'a DesktopTree, route: &RouteIds) -> Option<&'a WindowGroup> {
    workspace_at(tree, route)
        .and_then(|workspace| {
            workspace
                .strips
                .iter()
                .find(|strip| strip.id == route.strip)
        })?
        .window_groups
        .iter()
        .find(|group| group.id == route.group)
}

/// `ids` as a command names a route.
fn route_of(ids: &RouteIds) -> Route {
    Route {
        workspace: ids.workspace.0.to_string(),
        strip: ids.strip.0.to_string(),
        group: ids.group.to_string(),
        tab: ids.tab.to_string(),
    }
}

/// Apply a window-menu command to the tree, and name the widget follow-ups
/// the windows need. False for a command that is not a window menu's.
pub(super) fn apply(
    shared: &mut super::Shared,
    command: &crate::navigation::Admitted,
    ops: &mut Vec<super::RowOp>,
) -> bool {
    use crate::navigation::Admitted;
    let handled = match command {
        Admitted::MoveToStrip(ids, strip) => move_to_strip(shared, *ids, *strip),
        Admitted::MoveToWorkspace(ids, to) => move_to_workspace(shared, *ids, *to),
        Admitted::MoveToDisplay(ids, display) => move_to_display(shared, *ids, *display),
        Admitted::WidthPreset(ids, width) => width_preset(shared, *ids, *width, ops),
        Admitted::EqualizeWidths(ids) => equalize(shared, *ids, ops),
        // The share is the sharing app's to keep: the ask names the row the
        // window's bar carries, and the app does the same with it.
        Admitted::ShareWindow(ids, shared_now) => {
            ops.push(super::RowOp::WindowShare(ids.tab, *shared_now));
            true
        }
        Admitted::SplitRight(ids) => detach(shared, *ids, false),
        Admitted::MoveToNewGroup(ids) => detach(shared, *ids, true),
        Admitted::CloseWindow(ids) => close_window(shared, *ids, ops),
        Admitted::StopDesktopControl(ids) => stop_desktop_control(shared, *ids),
        _ => return false,
    };
    if handled {
        shared.changed();
    }
    true
}

/// Carry the window at `ids` into strip `strip` of its own workspace.
fn move_to_strip(shared: &mut Shared, ids: RouteIds, strip: StripId) -> bool {
    let Shared { tree, minter, .. } = shared;
    let moved = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == ids.workspace)
        .is_some_and(|workspace| {
            let command = DesktopCommand::MoveGroup {
                group: ids.group,
                strip: strip.0,
                before: None,
            };
            apply_command(
                workspace,
                &command,
                &mut || Ok(minter.mint()),
                &mut |_, _| Err("a move opens no window".into()),
            )
            .is_ok()
        });
    // The windows were rearranged, so the rows follow the tree they are in
    // now rather than the one they were built from.
    shared.replaced |= moved;
    moved
}

/// Carry the tab at `ids` into workspace `to`, with a window of its own.
fn move_to_workspace(shared: &mut Shared, ids: RouteIds, to: WorkspaceId) -> bool {
    let Shared { tree, minter, .. } = shared;
    let moved = move_tab_to_workspace(tree, ids.tab, to, &mut || Ok(minter.mint())).is_ok();
    shared.replaced |= moved;
    moved
}

/// Carry the window at `ids` onto display `display`.
fn move_to_display(shared: &mut Shared, ids: RouteIds, display: u32) -> bool {
    let tree = &mut shared.tree;
    let moved = tree
        .outputs
        .move_group(&tree.workspaces, ids.group, display);
    if moved {
        tree.external_revision = tree.external_revision.wrapping_add(1);
    }
    shared.replaced |= moved;
    moved
}

/// Give the window at `ids` the share `width` of its strip.
fn width_preset(
    shared: &mut Shared,
    ids: RouteIds,
    width: tilo_desktop_model::WindowWidth,
    ops: &mut Vec<super::RowOp>,
) -> bool {
    match super::find_group(&mut shared.tree, ids.strip, ids.group) {
        Some(group) => {
            group.width = width;
            ops.push(super::RowOp::Width(ids.strip, ids.group, width));
            true
        }
        None => false,
    }
}

/// Give every window of the strip at `ids` the same share of it.
fn equalize(shared: &mut Shared, ids: RouteIds, ops: &mut Vec<super::RowOp>) -> bool {
    let strip = shared
        .tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == ids.workspace)
        .and_then(|workspace| {
            workspace
                .strips
                .iter_mut()
                .find(|strip| strip.id == ids.strip)
        });
    match strip {
        Some(strip) => {
            if equalize_widths(strip) {
                for group in &strip.window_groups {
                    ops.push(super::RowOp::Width(ids.strip, group.id, group.width));
                }
            }
            true
        }
        None => false,
    }
}

/// Close every tab of the window at `ids`: the window goes, and a reopen
/// brings its tabs back.
fn close_window(shared: &mut Shared, ids: RouteIds, ops: &mut Vec<super::RowOp>) -> bool {
    // Backwards, so each close remembers the window's own place and a reopen
    // puts the tabs back in the order they were in.
    let tabs: Vec<TabId> = group_at(&shared.tree, &ids)
        .map(|group| group.tabs.iter().rev().map(|tab| tab.id).collect())
        .unwrap_or_default();
    let mut last = None;
    for tab in tabs {
        match shared.tree.close_tab(tab) {
            Some(removed) => last = Some(removed),
            None => break,
        }
    }
    match last {
        Some(removed) => {
            ops.push(super::RowOp::Close {
                strip: removed.strip,
                group: removed.group,
                tab: removed.tab.id,
                group_removed: removed.group_removed,
            });
            true
        }
        None => false,
    }
}

/// Take the desktop-control grant back from the app the window at `ids`
/// holds it for.
fn stop_desktop_control(shared: &mut Shared, ids: RouteIds) -> bool {
    let Some((workspace, app)) = holder_of(&shared.tree, &ids) else {
        return false;
    };
    let workspace = workspace.id;
    shared
        .tree
        .workspaces
        .iter_mut()
        .find(|held| held.id == workspace)
        .is_some_and(|held| held.stop_desktop_control(&app))
}

/// Detach the tab at `ids` into a window of its own, at the strip's end
/// (`at_end`) or right after the window it is in, and make it the one the
/// strip shows. Whether it moved.
fn detach(shared: &mut super::Shared, ids: RouteIds, at_end: bool) -> bool {
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
    shared.replaced = true;
    true
}

#[cfg(test)]
#[path = "window_menu_tests.rs"]
mod tests;

impl crate::surface::DesktopHandle {
    /// Who holds tab `tab` on a shared desktop, when someone else does: the
    /// menu offers that person's window only what a viewer may do to it.
    #[must_use]
    pub fn held_by(&self, tab: TabId) -> Option<String> {
        self.shared
            .borrow()
            .holders
            .get(&tab)
            .map(|holder| holder.name.clone())
    }

    /// Fill the strip with the window showing `tab`, or give it its width
    /// back, on the next update pass. False when no window shows it.
    pub fn maximize_tab(&self, tab: TabId, maximized: bool) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(ids) = crate::surface::route_of(&shared.tree, tab) else {
            return false;
        };
        shared
            .queue
            .push(crate::navigation::Admitted::Maximize(ids, maximized));
        true
    }

    /// Cover the display with the window showing `tab`, or give it back, on
    /// the next update pass. False when no window shows it.
    pub fn fullscreen_tab(&self, tab: TabId, fullscreen: bool) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(ids) = crate::surface::route_of(&shared.tree, tab) else {
            return false;
        };
        shared
            .queue
            .push(crate::navigation::Admitted::Fullscreen(ids, fullscreen));
        true
    }

    /// What the desktop knows of tab `tab` beyond its place in the tree:
    /// who holds it, whether it is waiting for the person, whether it runs
    /// here and whether the sharing app lists it.
    fn window_facts(&self, tab: TabId) -> Facts {
        let (held, asking, local, opens, offered) = {
            let shared = self.shared.borrow();
            let offer = shared
                .window_offers
                .iter()
                .find(|window| window.tab == tab)
                .cloned();
            (
                shared.holders.get(&tab).map(|holder| holder.name.clone()),
                shared.asking.contains(&tab),
                tilo_desktop_model::peer_salt_of(tab) == shared.minter.salt(),
                shared.opens,
                offer,
            )
        };
        let sharing = offered.map(|window| {
            let item = ui::window_share::host::menu_item(&window, &ui::window_share::shared());
            Sharing {
                label: item.name,
                shared: item.id == ui::window_share::host::STOP_ITEM,
            }
        });
        Facts {
            held,
            asking,
            local,
            opens,
            sharing,
        }
    }

    /// The menu rows of the window showing `tab`, as the desktop would show
    /// them now. Empty when no window shows it.
    pub fn window_menu(&self, tab: TabId) -> Vec<Row> {
        let facts = self.window_facts(tab);
        self.read(|tree| {
            let Some(route) = super::route_of(tree, tab) else {
                return Vec::new();
            };
            rows(tree, &route, &facts)
        })
    }

    /// Choose row `id` of the window showing `tab`: the command the row
    /// means, dispatched. `name` is what the person typed for [`RENAME`].
    ///
    /// # Errors
    /// A row the window does not offer now, or one it offers but cannot be
    /// chosen, is refused with the reason the menu shows beside it.
    pub fn choose_window_menu(
        &self,
        tab: TabId,
        id: &str,
        name: Option<&str>,
        generation: &str,
    ) -> Result<(), String> {
        let facts = self.window_facts(tab);
        let found = self.read(|tree| {
            let route = super::route_of(tree, tab)?;
            let offered = rows(tree, &route, &facts)
                .into_iter()
                .find(|row| row.id == id)?;
            Some((command(tree, &route, id, name, generation), offered))
        });
        let Some((command, offered)) = found else {
            return Err("That is not something this window offers".into());
        };
        if !offered.enabled {
            return Err(offered
                .reason
                .unwrap_or_else(|| "That is not something this window offers".into()));
        }
        match command {
            Some(command) => self.dispatch(&command, generation),
            None => Err("That is not something this window offers".into()),
        }
    }
}
