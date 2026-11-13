//! What the person can do to one window, as the desktop offers it.
//!
//! The rows are the desktop's own and no app's: going to a window that asked
//! for the person, covering the display with it, filling the strip with it
//! and giving the strip back, carrying it into another strip, workspace or
//! display, giving it a share of its strip, opening another tab of its app,
//! sharing its live picture with its workspace or its record in a channel,
//! naming it, splitting a tab out into a window of its own, closing the tab
//! or the window, and stopping an app from controlling the desktop beyond
//! it. Every row is a navigation command the desktop already answers, so a
//! menu — the overflow menu in the window's own bar, or one a sheet mounts
//! beside it — offers exactly what the desktop can do, and choosing a row
//! sends the same command a chord or a control sends. The share with the
//! workspace alone is the host session's: its row reads what the session's
//! window shares say and leaves the ask to them ([`super::workspace_share`]).
//!
//! Which rows apply follows the window: a window covering its display offers
//! it back, a window filling the strip offers the strip back instead of
//! filling it, the widest window cannot be widened, a window asking for the
//! person offers to be gone to only until it has them, a window of one tab
//! offers no split, and a window someone else holds can be neither named,
//! moved nor closed. A place with nowhere to go — another strip, another
//! workspace, another display — is not offered at all: its row would be a
//! choice that cannot be made.
//!
//! The rows stand in groups of what they do to the window, a rule between
//! one group and the next, and a group with nothing in it leaves no rule
//! behind: covering and filling it, carrying it somewhere (each place a
//! submenu of its own), giving it a share of its strip, sharing it, opening
//! or naming what it shows, and closing it. Every row carries the picture of
//! the command behind it, as the window's own controls draw it.

use tilo_desktop_model::commands::{
    DesktopCommand, TabLocation, TabPlacement, apply_command, move_tab_to_workspace,
};
use tilo_desktop_model::width::{
    WIDTH_NARROW_PERMILLE, WIDTH_NORMAL_PERMILLE, WIDTH_WIDE_PERMILLE, equalize_widths,
};
use tilo_desktop_model::{
    DesktopTree, GroupId, StripId, Tab, TabId, TileContent, WindowGroup, Workspace, WorkspaceSlot,
};
use ui::icons::Icon;

use crate::i18n::{t, t_args};
use crate::navigation::{ChromeEffect, DeskEffect, Destination, RouteIds, Verb, WidthPreset};

use super::{DesktopHandle, Shared, WorkspaceShare};

impl DesktopHandle {
    /// The windows whose bar was asked for its menu since the last call — a secondary click — in
    /// the order they asked, each with the place on the desk the person pressed, where the menu
    /// stands.
    pub fn take_tab_menus(&self) -> Vec<(TabId, ui::kurbo::Point)> {
        std::mem::take(&mut self.shared.borrow_mut().menus)
    }
}

/// The tabs whose bar asked for its menu and where it was pressed, in the row's window order.
pub(super) fn take_asked_menus(row: &mut ui::widget::GroupRow) -> Vec<(TabId, ui::kurbo::Point)> {
    row.groups
        .iter_mut()
        .filter_map(ui::widget::WindowGroup::take_tab_context_menu)
        .collect()
}

/// The window a menu is on, named as the person knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowSubject {
    pub title: String,
    pub app: Option<String>,
}

impl DesktopHandle {
    /// The window `tab` shows, named as the person knows it: its title and the app that paints it.
    #[must_use]
    pub fn window_subject(&self, tab: TabId) -> Option<WindowSubject> {
        let shared = self.shared.borrow();
        let tab = shared.tree.tab(tab)?;
        let app = match &tab.tile {
            TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. } => {
                Some(app_id.clone())
            }
            _ => None,
        };
        Some(WindowSubject {
            title: tab.title.clone(),
            app,
        })
    }
}

pub const REVEAL: &str = "window-reveal";
pub const FULLSCREEN: &str = "window-fullscreen";
pub const FLOAT: &str = "window-float";
pub const SNAP_BACK: &str = "window-snap-back";
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
pub const WIDTH: &str = "window-width";
pub const NEW_TAB: &str = "window-new-tab";
pub const SHARE: &str = "window-share";
pub const UN_SHARE: &str = "window-unshare";
/// Share the window with its workspace: its live picture, in its tab on the workspace's other
/// devices.
pub const SHARE_WITH_WORKSPACE: &str = "window-share-with-workspace";
pub const STOP_SHARING_WITH_WORKSPACE: &str = "window-stop-sharing-with-workspace";
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

/// The share row a window offers, when this desktop can share it: how it reads, and whether the
/// window is shared now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sharing {
    pub label: String,
    pub shared: bool,
}

/// What the desktop knows about one window beside its place in the tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// The person who holds the window on a shared desktop, when someone else does: their window is
    /// theirs to move, to name and to close.
    pub held: Option<String>,
    /// The window asked for the person and has not had them yet.
    pub asking: bool,
    /// The window runs on this device, so its app can be opened again here.
    pub local: bool,
    /// This person opens windows and tabs on this desktop (someone let only look is offered none).
    pub opens: bool,
    /// The row asking the sharing app to post the window in a channel; `None` for a window the
    /// sharing app does not list.
    pub sharing: Option<Sharing>,
    /// What the session reads of the window's share with its workspace; `None` for a window that
    /// cannot be shared from this device.
    pub workspace_share: Option<WorkspaceShare>,
}

/// One row of a window's menu: what it does, how it reads, and why it cannot be chosen when it
/// cannot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The stable id a menu host sends back with the window it is for; empty for a rule, which
    /// chooses nothing.
    pub id: String,
    pub label: String,
    /// Whether the person can choose it now; a row they cannot is shown with `reason` beside it.
    pub enabled: bool,
    pub reason: Option<String>,
    /// The picture of the command the row runs, as the window's own controls draw it.
    pub icon: Option<Icon>,
    /// The rows a row that opens a submenu offers; empty for a plain row.
    pub children: Vec<Row>,
    /// A rule between two groups of rows: drawn as a line, never a choice.
    pub separator: bool,
}

impl Row {
    fn offered(id: impl Into<String>, label: String) -> Self {
        Self {
            id: id.into(),
            label,
            enabled: true,
            reason: None,
            icon: None,
            children: Vec::new(),
            separator: false,
        }
    }

    /// The row drawn with `icon` at its start.
    fn with_icon(self, icon: Icon) -> Self {
        Self {
            icon: Some(icon),
            ..self
        }
    }

    /// A row shown for want of a step the window cannot take: it says why.
    fn refused(id: impl Into<String>, label: String, reason: String) -> Self {
        Self {
            enabled: false,
            reason: Some(reason),
            ..Self::offered(id, label)
        }
    }

    /// A row opening `children` beside it: what is left of a submenu with nowhere to go is no row
    /// at all.
    fn submenu(id: impl Into<String>, label: String, children: Vec<Row>) -> Self {
        Self {
            children,
            ..Self::offered(id, label)
        }
    }

    /// The rule between the group before and the one after.
    fn rule() -> Self {
        Self {
            enabled: false,
            separator: true,
            ..Self::offered(String::new(), String::new())
        }
    }

    /// A row for the window's own person: anyone else sees why they cannot choose it, their whole
    /// submenu with it.
    fn theirs(self, own: bool, reason: Option<String>) -> Self {
        if own {
            return self;
        }
        let children = self
            .children
            .into_iter()
            .map(|child| child.theirs(false, reason.clone()))
            .collect();
        Self {
            enabled: false,
            reason,
            children,
            ..self
        }
    }

    /// The row `id` this row or a row of its submenu offers.
    fn find(&self, id: &str) -> Option<&Self> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(id))
    }
}

/// The id of the row carrying the window into strip `strip` .
fn move_to_strip_id(strip: StripId) -> String {
    format!("{MOVE_TO_STRIP}:{}", strip.0)
}

/// The id of the row taking the window into workspace `workspace` .
fn move_to_workspace_id(workspace: WorkspaceSlot) -> String {
    format!("{MOVE_TO_WORKSPACE}:{}", workspace.0)
}

/// The id of the row showing the window on display `display` .
fn move_to_display_id(display: u32) -> String {
    format!("{MOVE_TO_DISPLAY}:{display}")
}

/// The menu rows that apply to the window at `route` , in the order to show them, as `facts`
/// describes the window beyond its place in the tree.
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
    let fullscreen = tree.fullscreen_group() == Some(group.id);
    let floating = floating(tree, group.id);
    let renamable = may_rename(tab) && facts.local;
    let mut rows = Vec::new();
    if facts.asking {
        add_group(
            &mut rows,
            vec![Row::offered(REVEAL, t("window-menu-reveal")).with_icon(Icon::CornerUpRight)],
        );
    }
    add_group(&mut rows, covering_rows(group, fullscreen));
    add_group(&mut rows, float_rows(floating));
    // Where the window can go is the tree's to say; carrying it there is the owner's, not a
    // viewer's.
    add_group(
        &mut rows,
        move_rows(tree, route)
            .into_iter()
            .map(|row| row.theirs(own, held()))
            .collect(),
    );
    add_group(
        &mut rows,
        vec![
            Row::submenu(
                WIDTH,
                t("window-menu-width"),
                width_rows(group.width.permille()),
            )
            .with_icon(Icon::Columns2),
        ],
    );
    add_group(&mut rows, sharing_rows(facts, own, held()));
    add_group(
        &mut rows,
        show_rows(tree, route, tab, own, held(), renamable, facts.opens),
    );
    add_group(&mut rows, closing_rows(tree, route, own, held()));
    if controls_desktop(tree, route) {
        add_group(
            &mut rows,
            vec![
                Row::offered(STOP_DESKTOP_CONTROL, t("window-menu-stop-desktop-control"))
                    .with_icon(Icon::Ban),
            ],
        );
    }
    rows
}

/// What sharing the window offers: its live picture to its workspace, which is its owner's to
/// give, and its record posted in a channel. Each says which of the two it is, and each offers
/// the way back instead while it is shared.
fn sharing_rows(facts: &Facts, own: bool, held: Option<String>) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(share) = facts.workspace_share {
        rows.push(workspace_share_row(share).theirs(own, held));
    }
    if let Some(sharing) = &facts.sharing {
        rows.push(
            Row::offered(
                if sharing.shared { UN_SHARE } else { SHARE },
                sharing.label.clone(),
            )
            .with_icon(if sharing.shared {
                Icon::EyeOff
            } else {
                Icon::ScreenShare
            }),
        );
    }
    rows
}

/// The row sharing the window with its workspace, or, once the session reads it as shared, the
/// row that stops: it says how many devices watch the window while any does.
fn workspace_share_row(share: WorkspaceShare) -> Row {
    if !share.shared {
        return Row::offered(SHARE_WITH_WORKSPACE, t("window-menu-share-with-workspace"))
            .with_icon(Icon::MonitorSmartphone);
    }
    let label = if share.viewers == 0 {
        t("window-menu-stop-sharing-with-workspace")
    } else {
        let viewers = i64::try_from(share.viewers).unwrap_or(i64::MAX);
        t_args(
            "window-menu-stop-sharing-watched",
            &[("viewers", viewers.into())],
        )
    };
    Row::offered(STOP_SHARING_WITH_WORKSPACE, label).with_icon(Icon::EyeOff)
}

/// What covering and filling the window offer: it covers its display, or gives it back, and it
/// fills its strip, or fills it again.
fn covering_rows(group: &WindowGroup, fullscreen: bool) -> Vec<Row> {
    vec![
        Row::offered(
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
        )
        .with_icon(if fullscreen {
            Icon::Minimize2
        } else {
            Icon::Fullscreen
        }),
        Row::offered(
            if group.maximized { RESTORE } else { MAXIMIZE },
            t(if group.maximized {
                "window-menu-restore"
            } else {
                "window-menu-maximize"
            }),
        )
        .with_icon(if group.maximized {
            Icon::Minimize2
        } else {
            Icon::Maximize2
        }),
    ]
}

/// Whether the workspace holds the window `group` floating above the desk.
fn floating(tree: &DesktopTree, group: GroupId) -> bool {
    tree.workspaces
        .iter()
        .any(|workspace| workspace.arrangement.float_of(group).is_some())
}

/// What lifting the window offers: over the desk, or back in its strip.
fn float_rows(floating: bool) -> Vec<Row> {
    vec![
        Row::offered(
            if floating { SNAP_BACK } else { FLOAT },
            t(if floating {
                "window-menu-snap-back"
            } else {
                "window-menu-float"
            }),
        )
        .with_icon(if floating {
            Icon::LayoutGrid
        } else {
            Icon::Layers
        }),
    ]
}

/// What the window shows: another tab of its app, its name, a split of a tab out, and a window of
/// its own — each offered to the owner alone.
fn show_rows(
    tree: &DesktopTree,
    route: &RouteIds,
    tab: &Tab,
    own: bool,
    held: Option<String>,
    renamable: bool,
    opens: bool,
) -> Vec<Row> {
    let mut rows = vec![
        Row::offered(NEW_TAB, t("window-menu-new-tab"))
            .with_icon(Icon::Plus)
            .theirs(own, held.clone())
            .theirs(opens, Some(t("window-menu-not-yours-to-open")))
            .theirs(app_of(tab).is_some(), Some(t("window-menu-no-app"))),
    ];
    if renamable {
        rows.push(
            Row::offered(RENAME, t("window-menu-rename"))
                .with_icon(Icon::Pencil)
                .theirs(own, held.clone()),
        );
    }
    if crate::navigation::has_a_second_tab(tree, route) {
        rows.push(
            Row::offered(SPLIT_RIGHT, t("window-menu-split-right"))
                .with_icon(Icon::Columns2)
                .theirs(own, held.clone()),
        );
        rows.push(
            Row::offered(NEW_GROUP, t("window-menu-new-group"))
                .with_icon(Icon::AppWindow)
                .theirs(own, held),
        );
    }
    rows
}

/// What closing offers: the tab alone, while the window has another, and the window itself.
fn closing_rows(tree: &DesktopTree, route: &RouteIds, own: bool, held: Option<String>) -> Vec<Row> {
    let mut rows = Vec::new();
    if crate::navigation::has_a_second_tab(tree, route) {
        rows.push(
            Row::offered(CLOSE, t("window-menu-close"))
                .with_icon(Icon::X)
                .theirs(own, held.clone()),
        );
    }
    rows.push(
        Row::offered(CLOSE_WINDOW, t("window-menu-close-window"))
            .with_icon(Icon::X)
            .theirs(own, held),
    );
    rows
}

/// Add `members` as a group of their own: a rule between it and what the menu offered before, and
/// nothing at all for a group with no rows.
fn add_group(rows: &mut Vec<Row>, members: Vec<Row>) {
    if members.is_empty() {
        return;
    }
    if !rows.is_empty() {
        rows.push(Row::rule());
    }
    rows.extend(members);
}

/// The shares of its strip a window offers, and the steps between them: a window at the widest or
/// the narrowest says so rather than stepping past.
fn width_rows(permille: u32) -> Vec<Row> {
    let wider = WIDTHS.iter().any(|width| *width > permille);
    let narrower = WIDTHS.iter().any(|width| *width < permille);
    vec![
        Row::offered(HALF, t("window-menu-width-half")),
        Row::offered(WIDTH_NARROW, t("window-menu-width-narrow")),
        Row::offered(WIDTH_NORMAL, t("window-menu-width-normal")),
        Row::offered(WIDTH_WIDE, t("window-menu-width-wide")),
        Row::offered(EQUALIZE, t("window-menu-width-equal")),
        step(WIDER, "window-menu-wider", wider, "window-menu-widest"),
        step(
            NARROWER,
            "window-menu-narrower",
            narrower,
            "window-menu-narrowest",
        ),
    ]
}

/// The row stepping the width to `id` , or—at the end of the steps—the row saying there is no
/// further one, why being `reason` .
fn step(id: &str, label: &str, steps: bool, reason: &str) -> Row {
    if steps {
        Row::offered(id, t(label))
    } else {
        Row::refused(id, t(label), t(reason))
    }
}

/// The rows carrying the window to another strip, workspace or display, each place by its name; a
/// place with nowhere to go offers no submenu at all.
fn move_rows(tree: &DesktopTree, route: &RouteIds) -> Vec<Row> {
    let mut rows = Vec::new();
    let strips: Vec<Row> = workspace_at(tree, route)
        .map(|workspace| {
            workspace
                .strips
                .iter()
                .enumerate()
                .filter(|(_, strip)| strip.id != route.strip)
                .map(|(index, strip)| {
                    Row::offered(
                        move_to_strip_id(strip.id),
                        crate::names::strip_named(&strip.name, index),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if !strips.is_empty() {
        rows.push(
            Row::submenu(MOVE_TO_STRIP, t("window-menu-move-to-strip"), strips)
                .with_icon(Icon::ArrowUpDown),
        );
    }
    let workspaces: Vec<Row> = tree
        .workspaces
        .iter()
        .filter(|workspace| workspace.slot != route.workspace)
        .map(|workspace| {
            Row::offered(
                move_to_workspace_id(workspace.slot),
                crate::names::workspace(&workspace.name),
            )
        })
        .collect();
    if !workspaces.is_empty() {
        rows.push(
            Row::submenu(
                MOVE_TO_WORKSPACE,
                t("window-menu-move-to-workspace"),
                workspaces,
            )
            .with_icon(Icon::Layers),
        );
    }
    let here = tree.outputs.display_of(route.group);
    let displays: Vec<Row> = tree
        .outputs
        .reading_order()
        .into_iter()
        .enumerate()
        .filter(|(_, display)| Some(*display) != here)
        .map(|(index, display)| {
            Row::offered(
                move_to_display_id(display),
                t_args(
                    "desktop-display-name",
                    &[("number", place_number_of(index).into())],
                ),
            )
        })
        .collect();
    if !displays.is_empty() {
        rows.push(
            Row::submenu(MOVE_TO_DISPLAY, t("window-menu-move-to-display"), displays)
                .with_icon(Icon::Monitor),
        );
    }
    rows
}

/// `index` as the number a person counts from, one on.
fn place_number_of(index: usize) -> i64 {
    i64::try_from(index.saturating_add(1)).unwrap_or(i64::MAX)
}

/// The verb row `id` of the window at `route` means; `name` is what the person typed for
/// [`RENAME`]. `None` for a row that does not apply here.
pub fn command(tree: &DesktopTree, route: &RouteIds, id: &str, name: Option<&str>) -> Option<Verb> {
    let group = group_at(tree, route)?;
    let tab = group.tabs.iter().find(|tab| tab.id == route.tab)?;
    let group_id = route.group.to_string();
    let tab_id = route.tab.to_string();
    let verb = match id {
        REVEAL => Verb::Desk(DeskEffect::Focus { tab: tab_id }),
        RENAME => Verb::Chrome(ChromeEffect::Rename {
            tab: Some(tab_id),
            group: None,
            strip: None,
            workspace: None,
            name: name?.to_owned(),
        }),
        FULLSCREEN | EXIT_FULLSCREEN => Verb::Chrome(ChromeEffect::Fullscreen {
            group: group_id,
            on: id == FULLSCREEN,
        }),
        MAXIMIZE | RESTORE => Verb::Chrome(ChromeEffect::Maximize {
            group: group_id,
            on: id == MAXIMIZE,
        }),
        FLOAT | SNAP_BACK => Verb::Chrome(ChromeEffect::Float {
            group: group_id,
            on: id == FLOAT,
        }),
        WIDER => Verb::Chrome(ChromeEffect::SetWidth {
            group: group_id,
            permille: Some(
                WIDTHS
                    .iter()
                    .copied()
                    .find(|width| *width > group.width.permille())?,
            ),
            preset: None,
        }),
        NARROWER => Verb::Chrome(ChromeEffect::SetWidth {
            group: group_id,
            permille: Some(
                WIDTHS
                    .iter()
                    .rev()
                    .copied()
                    .find(|width| *width < group.width.permille())?,
            ),
            preset: None,
        }),
        HALF | WIDTH_NARROW | WIDTH_NORMAL | WIDTH_WIDE => Verb::Chrome(ChromeEffect::SetWidth {
            group: group_id,
            permille: None,
            preset: Some(match id {
                HALF => WidthPreset::Half,
                WIDTH_NARROW => WidthPreset::Narrow,
                WIDTH_NORMAL => WidthPreset::Normal,
                _ => WidthPreset::Wide,
            }),
        }),
        EQUALIZE => Verb::Chrome(ChromeEffect::Equalize {
            strip: route.strip.0.to_string(),
        }),
        SHARE | UN_SHARE => Verb::Chrome(ChromeEffect::ShareWindow {
            group: group_id,
            on: id == SHARE,
        }),
        NEW_TAB => Verb::Chrome(ChromeEffect::OpenApp {
            app: app_of(tab)?,
            beside: Some(tab_id),
            background: None,
            float: None,
        }),
        SPLIT_RIGHT => Verb::Chrome(ChromeEffect::SplitRight { tab: tab_id }),
        NEW_GROUP => Verb::Chrome(ChromeEffect::NewGroup { tab: tab_id }),
        CLOSE => Verb::Desk(DeskEffect::CloseTab { tab: tab_id }),
        CLOSE_WINDOW => Verb::Chrome(ChromeEffect::CloseWindow { group: group_id }),
        STOP_DESKTOP_CONTROL => Verb::Chrome(ChromeEffect::StopDesktopControl { group: group_id }),
        place => {
            let to = if let Some(strip) = place.strip_prefix(MOVE_TO_STRIP) {
                Destination {
                    strip: Some(place_number(strip)?.to_string()),
                    ..Destination::default()
                }
            } else if let Some(workspace) = place.strip_prefix(MOVE_TO_WORKSPACE) {
                Destination {
                    workspace: Some(place_number(workspace)?.to_string()),
                    ..Destination::default()
                }
            } else {
                Destination {
                    display: Some(place_number(place.strip_prefix(MOVE_TO_DISPLAY)?)?.to_string()),
                    ..Destination::default()
                }
            };
            Verb::Chrome(ChromeEffect::MoveTab { tab: tab_id, to })
        }
    };
    Some(verb)
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

/// Whether the person may name the window `tab` : a host (non-Tilo) window's tab, whose title is
/// the window's own rather than an app's resource.
fn may_rename(tab: &Tab) -> bool {
    matches!(
        tab.tile,
        TileContent::Foreign { .. } | TileContent::HostedWindow { .. }
    )
}

/// Whether the window at `route` runs an app that controls this desktop beyond its own window now.
fn controls_desktop(tree: &DesktopTree, route: &RouteIds) -> bool {
    let Some((workspace, app)) = holder_of(tree, route) else {
        return false;
    };
    workspace.holds_desktop_control_at(&app, tilo_clock::unix_now())
}

/// The workspace holding the window at `route` , and the app it runs.
fn holder_of<'a>(tree: &'a DesktopTree, route: &RouteIds) -> Option<(&'a Workspace, String)> {
    let group = group_at(tree, route)?;
    let tab = group.tabs.iter().find(|tab| tab.id == route.tab)?;
    Some((workspace_at(tree, route)?, app_of(tab)?))
}

/// The workspace at `route` , while the tree still holds it.
fn workspace_at<'a>(tree: &'a DesktopTree, route: &RouteIds) -> Option<&'a Workspace> {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.slot == route.workspace)
}

/// The window group at `route` , while the tree still holds it.
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

/// Apply a window-menu command to the tree, and name the widget follow-ups the windows need. False
/// for a command that is not a window menu's.
pub(super) fn apply(
    shared: &mut super::Shared,
    command: &crate::navigation::Admitted,
    ops: &mut Vec<super::row_op::RowOp>,
) -> bool {
    use crate::navigation::Admitted;
    let handled = match command {
        Admitted::MoveToStrip(ids, strip, before) => move_to_strip(shared, *ids, *strip, *before),
        Admitted::JoinGroup(ids, into) => join_group(shared, *ids, *into),
        Admitted::MoveToWorkspace(ids, to) => move_to_workspace(shared, *ids, *to),
        Admitted::MoveToDisplay(ids, display) => move_to_display(shared, *ids, *display),
        Admitted::WidthPreset(ids, width) => width_preset(shared, *ids, *width, ops),
        Admitted::EqualizeWidths(ids) => equalize(shared, *ids, ops),
        // The share is the sharing app's to keep: the ask names the row the window's bar carries,
        // and the app does the same with it.
        Admitted::ShareWindow(ids, shared_now) => {
            ops.push(super::row_op::RowOp::WindowShare(ids.tab, *shared_now));
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

/// Carry the window at `ids` into strip `strip` of its own workspace, before the window `before`
/// when one is named: its own strip reorders it in place.
fn move_to_strip(
    shared: &mut Shared,
    ids: RouteIds,
    strip: StripId,
    before: Option<GroupId>,
) -> bool {
    let Shared { tree, minter, .. } = shared;
    let moved = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.slot == ids.workspace)
        .is_some_and(|workspace| {
            let command = if strip == ids.strip {
                DesktopCommand::ReorderGroup {
                    strip: strip.0,
                    group: ids.group,
                    before,
                }
            } else {
                DesktopCommand::MoveGroup {
                    group: ids.group,
                    strip: strip.0,
                    before,
                }
            };
            apply_command(
                workspace,
                &command,
                &mut || Ok(minter.mint()),
                &mut |_, _| Err("a move opens no window".into()),
            )
            .is_ok()
        });
    if moved {
        // The person stays with the window they moved: the desk shows the strip it landed in, with
        // it in front, as dragging it there does.
        tree.active_strip.insert(ids.workspace, strip);
        tree.set_active_group(strip, ids.group);
    }
    // The windows were rearranged, so the rows follow the tree they are in now rather than the one
    // they were built from.
    shared.replaced |= moved;
    moved
}

/// Put the tab at `ids` into the window `into` of its own strip, its tab standing last there.
fn join_group(shared: &mut Shared, ids: RouteIds, into: GroupId) -> bool {
    let Shared { tree, minter, .. } = shared;
    let moved = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.slot == ids.workspace)
        .is_some_and(|workspace| {
            let command = DesktopCommand::MoveTab {
                location: TabLocation {
                    strip: ids.strip.0,
                    group: ids.group,
                    tab: ids.tab,
                },
                placement: TabPlacement::InGroup {
                    strip: ids.strip.0,
                    group: into,
                    after: None,
                },
            };
            apply_command(
                workspace,
                &command,
                &mut || Ok(minter.mint()),
                &mut |_, _| Err("a join opens no window".into()),
            )
            .is_ok()
        });
    if moved {
        // The window the tab joined is the one in front, showing the tab that just arrived: the
        // window it left may have gone with its last tab, so the desk would be left with nothing in
        // front.
        tree.active_strip.insert(ids.workspace, ids.strip);
        tree.set_active_group(ids.strip, into);
        if let Some(group) = tree
            .workspaces
            .iter_mut()
            .flat_map(|workspace| &mut workspace.strips)
            .filter(|strip| strip.id == ids.strip)
            .flat_map(|strip| &mut strip.window_groups)
            .find(|group| group.id == into)
        {
            group.activate_tab(ids.tab);
        }
    }
    shared.replaced |= moved;
    moved
}

/// Carry the tab at `ids` into workspace `to` , with a window of its own.
fn move_to_workspace(shared: &mut Shared, ids: RouteIds, to: WorkspaceSlot) -> bool {
    let Shared { tree, minter, .. } = shared;
    let moved = move_tab_to_workspace(tree, ids.tab, to, &mut || Ok(minter.mint())).is_ok();
    shared.replaced |= moved;
    moved
}

/// Carry the window at `ids` onto display `display` .
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
    ops: &mut Vec<super::row_op::RowOp>,
) -> bool {
    match super::find_group(&mut shared.tree, ids.strip, ids.group) {
        Some(group) => {
            group.width = width;
            ops.push(super::row_op::RowOp::Width(ids.strip, ids.group, width));
            true
        }
        None => false,
    }
}

/// Give every window of the strip at `ids` the same share of it.
fn equalize(shared: &mut Shared, ids: RouteIds, ops: &mut Vec<super::row_op::RowOp>) -> bool {
    let strip = shared
        .tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.slot == ids.workspace)
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
                    ops.push(super::row_op::RowOp::Width(
                        ids.strip,
                        group.id,
                        group.width,
                    ));
                }
            }
            true
        }
        None => false,
    }
}

/// Close every tab of the window at `ids` : the window goes, and a reopen brings its tabs back.
fn close_window(shared: &mut Shared, ids: RouteIds, ops: &mut Vec<super::row_op::RowOp>) -> bool {
    // Backwards, so each close remembers the window's own place and a reopen puts the tabs back in
    // the order they were in.
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
            ops.push(super::row_op::RowOp::Close {
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

/// Take the desktop-control grant back from the app the window at `ids` holds it for.
fn stop_desktop_control(shared: &mut Shared, ids: RouteIds) -> bool {
    let Some((workspace, app)) = holder_of(&shared.tree, &ids) else {
        return false;
    };
    let workspace = workspace.slot;
    shared
        .tree
        .workspaces
        .iter_mut()
        .find(|held| held.slot == workspace)
        .is_some_and(|held| held.stop_desktop_control(&app))
}

/// Detach the tab at `ids` into a window of its own, at the strip's end (`at_end`) or right after
/// the window it is in, and make it the one the strip shows. Whether it moved.
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

#[path = "window_menu_popup.rs"]
pub mod popup;

impl crate::surface::DesktopHandle {
    /// Who holds tab `tab` on a shared desktop, when someone else does: the menu offers that
    /// person's window only what a viewer may do to it.
    #[must_use]
    pub fn held_by(&self, tab: TabId) -> Option<String> {
        self.shared
            .borrow()
            .holders
            .get(&tab)
            .map(|holder| holder.name.clone())
    }

    /// Fill the strip with the window showing `tab` , or give it its width back, on the next update
    /// pass. False when no window shows it.
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

    /// Cover the display with the window showing `tab` , or give it back, on the next update pass.
    /// False when no window shows it.
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

    /// What the desktop knows of tab `tab` beyond its place in the tree: who holds it, whether it
    /// is waiting for the person, whether it runs here and whether the sharing app lists it.
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
            workspace_share: self.workspace_share(tab),
        }
    }

    /// The menu rows of the window showing `tab` , as the desktop would show them now. Empty when
    /// no window shows it.
    pub fn window_menu(&self, tab: TabId) -> Vec<Row> {
        let facts = self.window_facts(tab);
        self.read(|tree| {
            let Some(route) = super::route_of(tree, tab) else {
                return Vec::new();
            };
            rows(tree, &route, &facts)
        })
    }

    /// Choose row `id` of the window showing `tab`: the verb the row means,
    /// dispatched. `name` is what the person typed for [`RENAME`].
    ///
    /// # Errors
    /// A row the window does not offer now, or one it offers but cannot be
    /// chosen, is refused with the reason the menu shows beside it.
    pub fn choose_window_menu(
        &self,
        tab: TabId,
        id: &str,
        name: Option<&str>,
    ) -> Result<(), String> {
        let facts = self.window_facts(tab);
        let found = self.read(|tree| {
            let route = super::route_of(tree, tab)?;
            let listed = rows(tree, &route, &facts);
            // Rows a row opens stand beside the menu, not in it: a choice of one reaches through
            // its row.
            let offered = listed.iter().find_map(|row| row.find(id)).cloned()?;
            Some((command(tree, &route, id, name), offered))
        });
        let Some((command, offered)) = found else {
            return Err("That is not something this window offers".into());
        };
        if !offered.enabled {
            return Err(offered
                .reason
                .unwrap_or_else(|| "That is not something this window offers".into()));
        }
        // A person types the name: the row opens the tab's name field in its window's bar, and
        // what they commit there is renamed by the same action an agent names it with.
        if id == RENAME && name.is_none() {
            self.rename_in_bar(tab);
            return Ok(());
        }
        // The share with the workspace is the session's: the row leaves the ask for it, and the
        // session invokes its window share with the tab.
        if id == SHARE_WITH_WORKSPACE || id == STOP_SHARING_WITH_WORKSPACE {
            self.ask_workspace_share(tab, id == SHARE_WITH_WORKSPACE);
            return Ok(());
        }
        match command {
            Some(verb) => self.dispatch(verb),
            None => Err("That is not something this window offers".into()),
        }
    }
}
