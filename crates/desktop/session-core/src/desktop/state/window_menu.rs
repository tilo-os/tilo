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
    WIDTH_NARROW_PERMILLE, WIDTH_NORMAL_PERMILLE, WIDTH_WIDE_PERMILLE,
};
use tilo_desktop_model::{
    GroupId, Layout, StripId, Tab, TabId, TileContent, WindowGroup, Workspace, WorkspaceId,
};
use ui::icons::Icon;

use crate::desktop::i18n::{t, t_args};
use crate::desktop::navigation::{
    ChromeEffect, Destination, LayoutEffect, RouteIds, Verb, WidthPreset,
};

use super::{DesktopHandle, Shared, WorkspaceShare};

impl DesktopHandle {
    /// The windows whose bar was asked for its menu since the last call — a secondary click — in
    /// the order they asked, each with the place on the shell the person pressed, where the menu
    /// stands.
    pub fn take_tab_menus(&self) -> Vec<(TabId, ui::kurbo::Point)> {
        std::mem::take(&mut self.shared.borrow_mut().menus)
    }

    /// Answer what the person may change, and keep who made each strip, by `rules`.
    pub fn set_rules(&self, rules: std::rc::Rc<dyn crate::desktop::host::contract::DesktopRules>) {
        self.shared.borrow_mut().rules = Some(rules);
    }

    /// Make a strip named `name` below the last one, as the person's own, when the rules let them;
    /// a refusal is said at once. The strip made, if any.
    pub fn add_strip(&self, name: String) -> Option<StripId> {
        let rules = self.shared.borrow().rules.clone();
        let command = DesktopCommand::CreateStrip { name };
        let made = rules
            .as_ref()
            .map_or(Ok(()), |rules| rules.may(&command))
            .and_then(|()| self.apply_command(&command));
        match made {
            Ok(strip) => {
                if let (Some(strip), Some(rules)) = (strip, rules) {
                    rules.made_strip(strip);
                }
                strip
            }
            Err(refusal) => {
                self.acknowledge(refusal);
                None
            }
        }
    }

    /// Carry the window `tab` into a new strip below the last one: the move its menu's New strip
    /// row and the chord past the last strip ask for.
    pub(crate) fn move_to_new_strip(&self, tab: TabId) {
        let moved = self.add_strip(String::new()).is_some_and(|strip| {
            self.choose_window_menu(tab, &move_to_strip_id(strip), None)
                .is_ok()
        });
        if !moved {
            self.acknowledge(t("shell-window-edge"));
        }
    }

    /// Take the strip `strip` away, carrying its windows into the strip after it (the one before,
    /// for the last strip): what a strip menu's delete row asks for.
    ///
    /// # Errors
    /// Why it could not: the strip is not in the workspace shown, or is its last, or a window did
    /// not move.
    pub(crate) fn delete_strip(&self, strip: StripId) -> Result<(), String> {
        let (target, tabs) = self
            .read(|tree| {
                let workspace = tree
                    .workspaces
                    .iter()
                    .find(|workspace| workspace.id() == tree.active)?;
                if workspace.strips.len() <= 1 {
                    return None;
                }
                let at = workspace.strips.iter().position(|held| held.id == strip)?;
                let target = workspace
                    .strips
                    .get(at.saturating_add(1))
                    .or_else(|| {
                        at.checked_sub(1)
                            .and_then(|before| workspace.strips.get(before))
                    })?
                    .id;
                let tabs: Vec<TabId> = workspace
                    .strips
                    .get(at)?
                    .window_groups
                    .iter()
                    .filter_map(|group| group.tabs.first().map(|tab| tab.id))
                    .collect();
                Some((target, tabs))
            })
            .ok_or_else(|| {
                format!(
                    "strip {} is not in the workspace shown, or is its last",
                    strip.0
                )
            })?;
        for tab in tabs {
            self.choose_window_menu(tab, &move_to_strip_id(target), None)?;
        }
        self.apply_command(&DesktopCommand::RemoveStrip { strip: strip.0 })?;
        Ok(())
    }

    /// Ask for the menu of the window covering the display the person works on, at a
    /// place on the shell, as a secondary click on its bar would. False when no window covers it.
    pub fn ask_covering_window_menu(&self, at: ui::kurbo::Point) -> bool {
        let mut shared = self.shared.borrow_mut();
        let tab = shared.tree.fullscreen_group().and_then(|group| {
            shared
                .tree
                .active_workspace()?
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .find(|held| held.id == group)
                .map(|held| held.active_tab)
        });
        let Some(tab) = tab else {
            return false;
        };
        shared.menus.push((tab, at));
        true
    }
}

/// The window a menu is on, named as the person knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowSubject {
    /// The window's name by the one rule every surface names a window by: its title and the app it
    /// shows, or the app alone where the title only names the app.
    pub name: String,
}

impl DesktopHandle {
    /// The window `tab` shows, named as the person knows it: its title and the app that paints it,
    /// the app's name alone where the title is the app under another spelling, and never an id.
    #[must_use]
    pub fn window_subject(&self, tab: TabId) -> Option<WindowSubject> {
        let shared = self.shared.borrow();
        let tab = shared.tree.tab(tab)?;
        Some(WindowSubject {
            name: super::window_name(tab),
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
pub const MOVE_LEFT: &str = "window-move-left";
pub const MOVE_RIGHT: &str = "window-move-right";
pub const MOVE_TO_STRIP: &str = "window-move-to-strip";
/// Carry the window into a strip made for it below the last one.
pub const MOVE_TO_NEW_STRIP: &str = "window-move-to-new-strip";
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
pub const WIDTHS: [u32; 3] = [
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
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&Self> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(id))
    }
}

/// The id of the row carrying the window into strip `strip` .
pub fn move_to_strip_id(strip: StripId) -> String {
    format!("{MOVE_TO_STRIP}:{}", strip.0)
}

/// The id of the row taking the window into workspace `workspace`.
pub fn move_to_workspace_id(workspace: WorkspaceId) -> String {
    format!("{MOVE_TO_WORKSPACE}:{workspace}")
}

/// The id of the row showing the window on display `display` .
pub fn move_to_display_id(display: u32) -> String {
    format!("{MOVE_TO_DISPLAY}:{display}")
}

/// The menu rows that apply to the window at `route` , in the order to show them, as `facts`
/// describes the window beyond its place in the tree.
pub fn rows(tree: &Layout, route: &RouteIds, facts: &Facts) -> Vec<Row> {
    let Some(group) = group_at(tree, route) else {
        return Vec::new();
    };
    let Some(tab) = group.tabs.iter().find(|tab| tab.id == route.tab) else {
        return Vec::new();
    };
    let own = facts.held.is_none();
    let held = || {
        facts.held.clone().map(|name| {
            if ui::presence::is_unnamed_person(&name) {
                t("window-menu-someone-elses-unnamed")
            } else {
                t_args("window-menu-someone-elses", &[("name", name.into())])
            }
        })
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
pub fn sharing_rows(facts: &Facts, own: bool, held: Option<String>) -> Vec<Row> {
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
/// row that stops: it says how many devices watch the window while any does, and how many wait
/// for it while they asked to see it and nothing of it goes out to them.
pub fn workspace_share_row(share: WorkspaceShare) -> Row {
    if !share.shared {
        return Row::offered(SHARE_WITH_WORKSPACE, t("window-menu-share-with-workspace"))
            .with_icon(Icon::MonitorSmartphone);
    }
    let label = if share.viewers == 0 {
        t("window-menu-stop-sharing-with-workspace")
    } else {
        let viewers = i64::try_from(share.viewers).unwrap_or(i64::MAX);
        let key = if share.streaming {
            "window-menu-stop-sharing-watched"
        } else {
            "window-menu-stop-sharing-waiting"
        };
        t_args(key, &[("viewers", viewers.into())])
    };
    Row::offered(STOP_SHARING_WITH_WORKSPACE, label).with_icon(Icon::EyeOff)
}

/// Whether the workspace holds the window `group` floating above the shell.
pub fn floating(tree: &Layout, group: GroupId) -> bool {
    tree.workspaces
        .iter()
        .any(|workspace| workspace.layout.float_of(group).is_some())
}

/// What lifting the window offers: over the shell, or back in its strip.
pub fn float_rows(floating: bool) -> Vec<Row> {
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
pub fn show_rows(
    tree: &Layout,
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
    if crate::desktop::navigation::has_a_second_tab(tree, route) {
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
pub fn closing_rows(tree: &Layout, route: &RouteIds, own: bool, held: Option<String>) -> Vec<Row> {
    let mut rows = Vec::new();
    if crate::desktop::navigation::has_a_second_tab(tree, route) {
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
pub fn add_group(rows: &mut Vec<Row>, members: Vec<Row>) {
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
pub fn width_rows(permille: u32) -> Vec<Row> {
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
pub fn step(id: &str, label: &str, steps: bool, reason: &str) -> Row {
    if steps {
        Row::offered(id, t(label))
    } else {
        Row::refused(id, t(label), t(reason))
    }
}

/// The rows moving the window one place along its strip, then those carrying it to another strip,
/// workspace or display, each place by its name; a side or a place with nowhere to go offers no row
/// at all.
pub fn move_rows(tree: &Layout, route: &RouteIds) -> Vec<Row> {
    let mut rows = Vec::new();
    for (id, label, icon) in [
        (MOVE_LEFT, "window-menu-move-left", Icon::ArrowLeft),
        (MOVE_RIGHT, "window-menu-move-right", Icon::ArrowRight),
    ] {
        if moved_along(tree, route, id == MOVE_LEFT).is_some() {
            rows.push(Row::offered(id, t(label)).with_icon(icon));
        }
    }
    let workspace = workspace_at(tree, route);
    let mut strips: Vec<Row> = workspace
        .into_iter()
        .flat_map(|workspace| workspace.strips.iter().enumerate())
        .filter(|(_, strip)| strip.id != route.strip)
        .map(|(index, strip)| {
            Row::offered(
                move_to_strip_id(strip.id),
                crate::desktop::names::strip_named(&strip.name, index),
            )
        })
        .collect();
    // Below the last strip, a strip of its own: for a window that shares its strip, as one alone
    // would leave its strip empty behind it.
    let shares_its_strip = workspace
        .and_then(|workspace| {
            workspace
                .strips
                .iter()
                .find(|strip| strip.id == route.strip)
        })
        .is_some_and(|strip| strip.window_groups.len() > 1);
    if shares_its_strip {
        strips.push(Row::offered(
            MOVE_TO_NEW_STRIP,
            t("strip-switcher-new-strip"),
        ));
    }
    if !strips.is_empty() {
        rows.push(
            Row::submenu(MOVE_TO_STRIP, t("window-menu-move-to-strip"), strips)
                .with_icon(Icon::ArrowUpDown),
        );
    }
    let workspaces: Vec<Row> = tree
        .workspaces
        .iter()
        .filter(|workspace| workspace.id() != route.workspace)
        .map(|workspace| {
            Row::offered(
                move_to_workspace_id(workspace.id()),
                crate::desktop::names::workspace(&workspace.name),
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

/// Where the window at `route` goes moving one place along its strip, to the left or the right: before the window
/// after the one it passes, or the strip's end. `None` at that end of the strip.
pub fn moved_along(tree: &Layout, route: &RouteIds, left: bool) -> Option<Destination> {
    let groups = &workspace_at(tree, route)?
        .strips
        .iter()
        .find(|strip| strip.id == route.strip)?
        .window_groups;
    let at = groups.iter().position(|group| group.id == route.group)?;
    let (passed, land) = if left {
        (at.checked_sub(1)?, at.checked_sub(1)?)
    } else {
        (at.checked_add(1)?, at.checked_add(2)?)
    };
    groups.get(passed)?;
    Some(Destination {
        strip: Some(route.strip.0.to_string()),
        before: groups.get(land).map(|group| group.id.to_string()),
        ..Destination::default()
    })
}

/// `index` as the number a person counts from, one on.
pub fn place_number_of(index: usize) -> i64 {
    i64::try_from(index.saturating_add(1)).unwrap_or(i64::MAX)
}

/// The verb row `id` of the window at `route` means; `name` is what the person typed for
/// [`RENAME`]. `None` for a row that does not apply here.
pub fn command(tree: &Layout, route: &RouteIds, id: &str, name: Option<&str>) -> Option<Verb> {
    let group = group_at(tree, route)?;
    let tab = group.tabs.iter().find(|tab| tab.id == route.tab)?;
    let group_id = route.group.to_string();
    let tab_id = route.tab.to_string();
    let verb = match id {
        REVEAL => Verb::Layout(LayoutEffect::Focus { tab: tab_id }),
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
        CLOSE => Verb::Layout(LayoutEffect::CloseTab { tab: tab_id }),
        CLOSE_WINDOW => Verb::Chrome(ChromeEffect::CloseWindow { group: group_id }),
        STOP_DESKTOP_CONTROL => Verb::Chrome(ChromeEffect::StopDesktopControl { group: group_id }),
        place => Verb::Chrome(ChromeEffect::MoveTab {
            tab: tab_id,
            to: destination(tree, route, place)?,
        }),
    };
    Some(verb)
}

/// Where the row `id` carries the window at `route`: one place along its strip, or into the strip,
/// workspace or display it names.
pub fn destination(tree: &Layout, route: &RouteIds, id: &str) -> Option<Destination> {
    Some(match id {
        MOVE_LEFT | MOVE_RIGHT => moved_along(tree, route, id == MOVE_LEFT)?,
        _ => {
            if let Some(strip) = id.strip_prefix(MOVE_TO_STRIP) {
                Destination {
                    strip: Some(place_number(strip)?.to_string()),
                    ..Destination::default()
                }
            } else if let Some(workspace) = id.strip_prefix(MOVE_TO_WORKSPACE) {
                Destination {
                    workspace: Some(
                        tilo_desktop_model::WorkspaceId::parse(workspace.strip_prefix(':')?)?
                            .to_string(),
                    ),
                    ..Destination::default()
                }
            } else {
                Destination {
                    display: Some(place_number(id.strip_prefix(MOVE_TO_DISPLAY)?)?.to_string()),
                    ..Destination::default()
                }
            }
        }
    })
}

/// The id `place` names after its `kind:` prefix.
pub fn place_number(place: &str) -> Option<u64> {
    place.strip_prefix(':')?.parse().ok()
}

/// The app the window `tab` runs.
pub fn app_of(tab: &Tab) -> Option<String> {
    match &tab.tile {
        TileContent::Foreign { app_id, .. } | TileContent::LocalApp { app_id, .. } => {
            Some(app_id.clone())
        }
        _ => None,
    }
}

/// Whether the person may name the window `tab` : a host (non-Tilo) window's tab, whose title is
/// the window's own rather than an app's resource.
pub fn may_rename(tab: &Tab) -> bool {
    matches!(
        tab.tile,
        TileContent::Foreign { .. } | TileContent::HostedWindow { .. }
    )
}

/// Whether the window at `route` runs an app that controls this desktop beyond its own window now.
pub fn controls_desktop(tree: &Layout, route: &RouteIds) -> bool {
    let Some((workspace, app)) = holder_of(tree, route) else {
        return false;
    };
    tilo_desktop_model::commands::has_control_at(&workspace.policy, &app, tilo_clock::unix_now())
}

/// The workspace holding the window at `route` , and the app it runs.
pub fn holder_of<'a>(tree: &'a Layout, route: &RouteIds) -> Option<(&'a Workspace, String)> {
    let group = group_at(tree, route)?;
    let tab = group.tabs.iter().find(|tab| tab.id == route.tab)?;
    Some((workspace_at(tree, route)?, app_of(tab)?))
}

/// The workspace at `route` , while the tree still holds it.
pub fn workspace_at<'a>(tree: &'a Layout, route: &RouteIds) -> Option<&'a Workspace> {
    tree.workspaces
        .iter()
        .find(|workspace| workspace.id() == route.workspace)
}

/// Carry the window at `ids` into strip `strip` of its own workspace, before the window `before`
/// when one is named: its own strip reorders it in place.
pub fn move_to_strip(
    shared: &mut Shared,
    ids: RouteIds,
    strip: StripId,
    before: Option<GroupId>,
) -> bool {
    let Shared { tree, minter, .. } = shared;
    let moved = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id() == ids.workspace)
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
        // The shell moved: counted as [`Layout::external_revision`] counts it, so the look that
        // follows writes the layout as it is.
        tree.external_revision = tree.external_revision.wrapping_add(1);
        // The person stays with the window they moved: the shell shows the strip it landed in, with
        // it in front, as dragging it there does.
        tree.active_strip.insert(ids.workspace, strip);
        tree.set_active_group(strip, ids.group);
        shared.followed = Some(ids.group);
    }
    // The windows were rearranged, so the rows follow the tree they are in now rather than the one
    // they were built from.
    shared.replaced |= moved;
    moved
}

/// Put the tab at `ids` into the window `into` of `into_strip` , its tab last there. A tab of
/// another workspace comes to the one `into_strip` stands in, as a window sent to that workspace
/// does.
pub fn join_group(shared: &mut Shared, ids: RouteIds, into_strip: StripId, into: GroupId) -> bool {
    let Shared { tree, minter, .. } = shared;
    let same_workspace = tree
        .active_workspace()
        .is_some_and(|workspace| workspace.id() == ids.workspace);
    let moved = if same_workspace {
        tree.workspaces
            .iter_mut()
            .find(|workspace| workspace.id() == ids.workspace)
            .is_some_and(|workspace| {
                let command = DesktopCommand::MoveTab {
                    location: TabLocation {
                        strip: ids.strip.0,
                        group: ids.group,
                        tab: ids.tab,
                    },
                    placement: TabPlacement::InGroup {
                        strip: into_strip.0,
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
            })
    } else {
        tree.move_tab_across_strips(
            ids.strip,
            into_strip,
            ids.tab,
            minter.mint(),
            tilo_desktop_model::topology::DropTarget::ExistingGroup {
                group_id: into,
                tab_index: usize::MAX,
            },
        )
        .is_some()
    };
    if moved {
        // The shell moved: counted as [`Layout::external_revision`] counts it, so the look that
        // follows writes the layout as it is.
        tree.external_revision = tree.external_revision.wrapping_add(1);
        // The window the tab joined is the one in front, showing the tab that just arrived: the
        // window it left may have gone with its last tab, so the shell would be left with nothing in
        // front.
        if let Some(active) = tree
            .active_workspace()
            .map(tilo_desktop_model::Workspace::id)
        {
            tree.active_strip.insert(active, into_strip);
        }
        tree.set_active_group(into_strip, into);
        if let Some(group) = tree
            .workspaces
            .iter_mut()
            .flat_map(|workspace| &mut workspace.strips)
            .filter(|strip| strip.id == into_strip)
            .flat_map(|strip| &mut strip.window_groups)
            .find(|group| group.id == into)
        {
            group.activate_tab(ids.tab);
        }
    }
    shared.replaced |= moved;
    moved
}

/// Put the place showing `resource` into the window `into` of the shown strip, in front there:
/// what a window's bar let go over a window does, on this device and when another of the person's
/// devices carried the window here. A place already the window's comes to the front and stays
/// where it is. The place is in any workspace of the layout, which is where this device keeps it.
pub fn join_place(
    shared: &mut Shared,
    resources: &std::collections::BTreeMap<TabId, String>,
    strip: StripId,
    into: GroupId,
    resource: &str,
) -> bool {
    let Some(ids) = shared.tree.workspaces.iter().find_map(|workspace| {
        let held = workspace.id();
        workspace.strips.iter().find_map(|strip| {
            strip.window_groups.iter().find_map(|group| {
                let tab = group.tabs.iter().find(|tab| {
                    resources
                        .get(&tab.id)
                        .is_some_and(|shown| shown.as_str() == resource)
                })?;
                Some(RouteIds {
                    workspace: held,
                    strip: strip.id,
                    group: group.id,
                    tab: tab.id,
                })
            })
        })
    }) else {
        return false;
    };
    if ids.group == into && ids.strip == strip {
        shared.tree.set_active_group(strip, into);
        if let Some(group) = super::find_group(&mut shared.tree, strip, into) {
            group.activate_tab(ids.tab);
        }
        shared.replaced = true;
        return true;
    }
    join_group(shared, ids, strip, into)
}

/// Carry the tab at `ids` into workspace `to` , with a window of its own.
pub fn move_to_workspace(shared: &mut Shared, ids: RouteIds, to: WorkspaceId) -> bool {
    let Shared { tree, minter, .. } = shared;
    let moved = move_tab_to_workspace(tree, ids.tab, to, &mut || Ok(minter.mint())).is_ok();
    // The shell moved: counted as [`Layout::external_revision`] counts it, so the look that follows
    // writes the layout as it is and a session that ends keeps it for the one after.
    if moved {
        tree.external_revision = tree.external_revision.wrapping_add(1);
    }
    shared.replaced |= moved;
    moved
}

/// Carry the window `group` onto display `display` .
pub fn move_to_display(shared: &mut Shared, group: GroupId, display: u32) -> bool {
    let tree = &mut shared.tree;
    let moved = tree.outputs.move_group(&tree.workspaces, group, display);
    if moved {
        tree.external_revision = tree.external_revision.wrapping_add(1);
        shared.followed = Some(group);
    }
    shared.replaced |= moved;
    moved
}

/// Take the desktop-control grant back from the app the window at `ids` holds it for.
pub fn stop_desktop_control(shared: &mut Shared, ids: RouteIds) -> bool {
    let Some((workspace, app)) = holder_of(&shared.tree, &ids) else {
        return false;
    };
    let workspace = workspace.id();
    shared
        .tree
        .workspaces
        .iter_mut()
        .find(|held| held.id() == workspace)
        .is_some_and(|held| tilo_desktop_model::commands::stop_control(&mut held.policy, &app))
}

impl crate::desktop::state::DesktopHandle {
    /// Who keeps tab `tab` on a shared desktop, when someone else does: the menu offers that
    /// person's window only what a viewer may do to it.
    #[must_use]
    pub fn held_by(&self, tab: TabId) -> Option<String> {
        self.shared
            .borrow()
            .holders
            .get(&tab)
            .filter(|holder| holder.keeps)
            .map(|holder| holder.name.clone())
    }

    /// Fill the strip with the window showing `tab` , or give it its width back, on the next update
    /// pass. False when no window shows it.
    pub fn maximize_tab(&self, tab: TabId, maximized: bool) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(ids) = crate::desktop::state::route_of(&shared.tree, tab) else {
            return false;
        };
        shared
            .queue
            .push(crate::desktop::navigation::Admitted::Maximize(
                ids, maximized,
            ));
        true
    }

    /// Cover the display with the window showing `tab` , or give it back, on the next update pass.
    /// False when no window shows it.
    pub fn fullscreen_tab(&self, tab: TabId, fullscreen: bool) -> bool {
        let mut shared = self.shared.borrow_mut();
        let Some(ids) = crate::desktop::state::route_of(&shared.tree, tab) else {
            return false;
        };
        shared
            .queue
            .push(crate::desktop::navigation::Admitted::Fullscreen(
                ids, fullscreen,
            ));
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
                shared
                    .holders
                    .get(&tab)
                    .filter(|holder| holder.keeps)
                    .map(|holder| holder.name.clone()),
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
        // The new strip is the person's, kept as theirs by the host's rules.
        if id == MOVE_TO_NEW_STRIP {
            self.move_to_new_strip(tab);
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

/// What covering and filling the window offer: it covers its display, or gives it back, and it
/// fills its strip, or fills it again.
pub fn covering_rows(group: &WindowGroup, fullscreen: bool) -> Vec<Row> {
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

/// The window group at `route` , while the tree still holds it.
pub fn group_at<'a>(tree: &'a Layout, route: &RouteIds) -> Option<&'a WindowGroup> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::seed_tree;
    use crate::desktop::state::{DesktopHandle, DesktopPass, TileSource};
    use std::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, HashMap};
    use std::rc::Rc;
    use tilo_desktop_model::{Display, IdMinter, WindowWidth};
    use ui::widget::TabHolder;
    use ui::window_share::OpenWindow;

    /// The content every tab of these tests shows: a notice of its title.
    struct Notices;

    impl TileSource for Notices {
        fn tile(&mut self, _: &Workspace, tab: &Tab) -> ui::widget::TileHost {
            ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    /// The apps these tests run, each opening a tab of its own app.
    struct Apps;

    impl TileSource for Apps {
        fn tile(&mut self, _: &Workspace, tab: &Tab) -> ui::widget::TileHost {
            ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }

        fn launch(&mut self, _: &Workspace, id: TabId, app: &str, _: Option<TabId>) -> Option<Tab> {
            Some(Tab {
                id,
                title: app.to_owned(),
                tile: TileContent::LocalApp {
                    app_id: app.into(),
                    label: app.into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            })
        }
    }

    /// `windows` windows, each running an app of its own.
    fn windows_tabs(minter: &mut IdMinter, windows: usize) -> Vec<Tab> {
        (0..windows)
            .map(|i| Tab {
                id: minter.mint(),
                title: format!("App{i}"),
                tile: TileContent::LocalApp {
                    app_id: format!("so.tilo.app{i}"),
                    label: format!("App{i}"),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            })
            .collect()
    }

    fn shell(windows: usize) -> DesktopHandle {
        let mut minter = IdMinter::new(7);
        let tabs = windows_tabs(&mut minter, windows);
        let tree = seed_tree(&mut minter, "Office", tabs);
        DesktopHandle::new(tree, 7)
    }

    /// The same shell, whose apps open a tab of their own when launched.
    fn apps_shell(windows: usize) -> DesktopHandle {
        shell(windows)
    }

    /// A shell whose first window holds `tabbed` tabs on its bar, with `alone` windows of one tab
    /// after it.
    fn tabbed_shell(tabbed: usize, alone: usize) -> DesktopHandle {
        let mut minter = IdMinter::new(7);
        let tabs = windows_tabs(&mut minter, tabbed.saturating_add(alone));
        let mut tree = seed_tree(&mut minter, "Office", tabs);
        let strip = &mut tree.workspaces[0].strips[0];
        let id = strip.window_groups[0].id;
        let tabs: Vec<Tab> = strip
            .window_groups
            .drain(..tabbed)
            .flat_map(|group| group.tabs)
            .collect();
        let active_tab = tabs[0].id;
        strip.window_groups.insert(
            0,
            WindowGroup {
                id,
                tabs,
                active_tab,
                width: WindowWidth::Normal,
                maximized: false,
                focus_history: Vec::new(),
            },
        );
        DesktopHandle::new(tree, 7)
    }

    fn one_window_shell(windows: usize) -> DesktopHandle {
        tabbed_shell(windows, 0)
    }

    /// The passes a shell's queued commands take to reach its tree.
    fn settle(handle: &DesktopHandle) {
        DesktopPass::new(handle.clone()).step(&mut Notices);
    }

    /// The passes with a source of its own for the windows' content.
    fn settle_with(handle: &DesktopHandle, tiles: &mut dyn TileSource) {
        DesktopPass::new(handle.clone()).step(tiles);
    }

    fn first_route(handle: &DesktopHandle) -> RouteIds {
        handle.read(|tree| {
            let tab = tree.workspaces[0].strips[0].window_groups[0].tabs[0].id;
            super::super::route_of(tree, tab).unwrap()
        })
    }

    /// The tab of the `group` th window of the first strip.
    fn tab_at(handle: &DesktopHandle, group: usize) -> TabId {
        handle.read(|tree| tree.workspaces[0].strips[0].window_groups[group].tabs[0].id)
    }

    /// The group at `route` , as the tree holds it.
    fn group_at(tree: &Layout, route: RouteIds) -> WindowGroup {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .find(|strip| strip.id == route.strip)
            .and_then(|strip| {
                strip
                    .window_groups
                    .iter()
                    .find(|group| group.id == route.group)
            })
            .cloned()
            .expect("the window is in the tree")
    }

    /// The row `id` , in the menu or in one of its submenus.
    fn row<'a>(rows: &'a [Row], id: &str) -> Option<&'a Row> {
        rows.iter().find_map(|row| row.find(id))
    }

    /// The ids of the rows the menu lists, its rules and the rows of its submenus included, in the
    /// order a person reads them.
    fn ids(rows: &[Row]) -> Vec<&str> {
        let mut found = Vec::new();
        for row in rows {
            if !row.separator {
                found.push(row.id.as_str());
            }
            found.extend(ids(&row.children));
        }
        found
    }

    /// The window a menu is on is named as a person knows it, by the one rule every surface names a
    /// window by: its title and the app it shows, never the id the app installs under.
    #[test]
    fn the_subject_names_the_window_the_way_a_person_does() {
        let handle = one_window_shell(1);
        let tab = handle.read(|tree| tree.workspaces[0].strips[0].window_groups[0].tabs[0].id);
        let subject = handle.window_subject(tab).expect("the window's subject");
        assert_eq!(
            subject.name, "App0",
            "the window's title is its app's name, so it is said once: {}",
            subject.name
        );
    }

    /// A window offers every row the desktop has for it, in the groups the desktop shows them in,
    /// and each means the command it names.
    #[test]
    fn every_row_the_desktop_has_for_a_window_is_offered_in_its_order() {
        let handle = apps_shell(1);
        let (route, tree) = (first_route(&handle), handle.read(Clone::clone));
        let rows = super::rows(&tree, &route, &Facts::default());
        assert_eq!(
            ids(&rows),
            vec![
                FULLSCREEN,
                MAXIMIZE,
                FLOAT,
                WIDTH,
                HALF,
                WIDTH_NARROW,
                WIDTH_NORMAL,
                WIDTH_WIDE,
                EQUALIZE,
                WIDER,
                NARROWER,
                NEW_TAB,
                CLOSE_WINDOW,
            ]
        );
        let groups: Vec<Vec<&str>> = rows
            .split(|row| row.separator)
            .map(|group| group.iter().map(|row| row.id.as_str()).collect())
            .collect();
        assert_eq!(
            groups,
            vec![
                vec![FULLSCREEN, MAXIMIZE],
                vec![FLOAT],
                vec![WIDTH],
                vec![NEW_TAB],
                vec![CLOSE_WINDOW],
            ],
            "covering and filling the window, lifting it over the shell, its share of
                the strip, what it shows, and closing it"
        );
        // Nowhere to move it to: the place offers no row at all, rather than one that cannot be chosen.
        for place in [MOVE_TO_STRIP, MOVE_TO_WORKSPACE, MOVE_TO_DISPLAY] {
            assert!(row(&rows, place).is_none(), "{place}");
        }
        assert_eq!(
            row(&rows, WIDTH).map(|row| row.label.as_str()),
            Some("Width"),
            "the shares are nested under one row"
        );
        for listed in rows.iter().filter(|row| !row.separator) {
            assert!(listed.icon.is_some(), "every row carries its picture");
            if !listed.children.is_empty() {
                assert!(
                    command(&tree, &route, &listed.id, Some("A name")).is_none(),
                    "a row that opens a submenu is not itself a command"
                );
                continue;
            }
            assert!(
                command(&tree, &route, &listed.id, Some("A name")).is_some(),
                "{listed:?} means a command"
            );
            assert!(
                listed.children.is_empty(),
                "{listed:?} is a command, not a submenu"
            );
        }
        // A window of one tab is its own tab: it offers no split and no close of the tab alone.
        assert!(row(&rows, CLOSE).is_none());
        assert!(row(&rows, SPLIT_RIGHT).is_none());
        assert_eq!(row(&rows, CLOSE_WINDOW).unwrap().label, "Close this window");
        // A Tilo app's window titles itself, so it offers no name.
        assert!(row(&rows, RENAME).is_none());
    }

    /// The row of a window asking for the person takes them to it, and is gone once they are there.
    #[test]
    fn a_window_asking_for_the_person_offers_to_be_gone_to_first() {
        let handle = shell(2);
        let tab = tab_at(&handle, 1);
        assert!(handle.ask_attention(tab));
        let rows = handle.window_menu(tab);
        assert_eq!(rows.first().map(|row| row.id.as_str()), Some(REVEAL));
        assert_eq!(rows[0].label, "Go to this window");
        let route = first_route(&handle);
        let tree = handle.read(Clone::clone);
        assert!(matches!(
            command(&tree, &route, REVEAL, None),
            Some(Verb::Layout(LayoutEffect::Focus { .. }))
        ));

        handle
            .choose_window_menu(tab, REVEAL, None)
            .expect("going to the window is offered");
        settle(&handle);
        assert_eq!(
            handle
                .read(|tree| crate::desktop::navigation::active_route(tree).map(|route| route.tab)),
            Some(tab),
            "the person is looking at it"
        );
        assert!(!handle.asks_attention(tab), "and it has them");
        assert!(row(&handle.window_menu(tab), REVEAL).is_none());
    }

    /// The full screen row covers the display with the window and gives it back, and the tree keeps
    /// which window covers it.
    #[test]
    fn the_full_screen_row_covers_the_display_and_gives_it_back() {
        let handle = shell(2);
        let route = first_route(&handle);
        let covering =
            |handle: &DesktopHandle| handle.read(tilo_desktop_model::Layout::fullscreen_group);
        assert_eq!(covering(&handle), None);
        assert!(row(&handle.window_menu(route.tab), FULLSCREEN).is_some());

        handle
            .choose_window_menu(route.tab, FULLSCREEN, None)
            .unwrap();
        settle(&handle);
        assert_eq!(covering(&handle), Some(route.group), "the tree holds it");
        let rows = handle.window_menu(route.tab);
        assert!(row(&rows, FULLSCREEN).is_none(), "it reads as its exit");
        assert!(row(&rows, EXIT_FULLSCREEN).is_some());

        handle
            .choose_window_menu(route.tab, EXIT_FULLSCREEN, None)
            .unwrap();
        settle(&handle);
        assert_eq!(covering(&handle), None, "the display is given back");
    }

    /// The float row lifts the window over the shell at the floating size, and the row then offers to
    /// put it back into its strip.
    #[test]
    fn the_float_row_lifts_the_window_over_the_shell_and_puts_it_back() {
        let handle = apps_shell(2);
        let route = first_route(&handle);
        let floating = |handle: &DesktopHandle| {
            handle.read(|tree| {
                tree.workspaces
                    .iter()
                    .any(|workspace| workspace.layout.float_of(route.group).is_some())
            })
        };

        assert!(!floating(&handle));
        assert!(row(&handle.window_menu(route.tab), FLOAT).is_some());
        assert!(row(&handle.window_menu(route.tab), SNAP_BACK).is_none());

        handle.choose_window_menu(route.tab, FLOAT, None).unwrap();
        settle(&handle);
        handle.arrange_asked_floats();
        settle(&handle);
        assert!(floating(&handle), "the record has it floating");
        let rows = handle.window_menu(route.tab);
        assert!(row(&rows, FLOAT).is_none(), "it reads as snapping back");
        assert!(row(&rows, SNAP_BACK).is_some());

        handle
            .choose_window_menu(route.tab, SNAP_BACK, None)
            .unwrap();
        settle(&handle);
        handle.arrange_asked_floats();
        settle(&handle);
        assert!(!floating(&handle), "the record has it back in its strip");
        assert_eq!(row(&handle.window_menu(route.tab), SNAP_BACK), None);
    }

    /// Filling the strip is offered, and once filled the row gives the strip back.
    #[test]
    fn filling_the_strip_is_offered_and_then_giving_it_back() {
        let handle = apps_shell(1);
        let route = first_route(&handle);
        assert!(row(&handle.window_menu(route.tab), MAXIMIZE).is_some());
        handle
            .choose_window_menu(route.tab, MAXIMIZE, None)
            .unwrap();
        settle(&handle);
        assert!(handle.read(|tree| group_at(tree, route).maximized));
        let rows = handle.window_menu(route.tab);
        assert!(row(&rows, MAXIMIZE).is_none());
        assert!(row(&rows, RESTORE).is_some());
        handle.choose_window_menu(route.tab, RESTORE, None).unwrap();
        settle(&handle);
        assert!(!handle.read(|tree| group_at(tree, route).maximized));
    }

    /// Each width row gives the window the share of its strip it names, and the wider and narrower rows
    /// step between the same shares, stopping at both ends with a row that says why.
    #[test]
    fn every_width_row_gives_the_window_the_share_it_names() {
        let handle = apps_shell(1);
        let route = first_route(&handle);
        for (id, width) in [
            (HALF, WindowWidth::Fraction(500)),
            (WIDTH_NARROW, WindowWidth::Narrow),
            (WIDTH_NORMAL, WindowWidth::Normal),
            (WIDTH_WIDE, WindowWidth::Wide),
        ] {
            let tree = handle.read(Clone::clone);
            assert!(
                matches!(
                    command(&tree, &route, id, None),
                    Some(Verb::Chrome(ChromeEffect::SetWidth {
                        preset: Some(_),
                        ..
                    }))
                ),
                "{id} is the share it names"
            );
            handle.choose_window_menu(route.tab, id, None).expect(id);
            settle(&handle);
            assert_eq!(
                handle.read(|tree| group_at(tree, route).width),
                width,
                "{id}"
            );
        }
        let rows = handle.window_menu(route.tab);
        let widest = row(&rows, WIDER).unwrap();
        assert!(!widest.enabled, "no wider than the strip");
        assert_eq!(widest.reason.as_deref(), Some("Widest"), "the row says why");
        let tree = handle.read(Clone::clone);
        assert!(matches!(
            command(&tree, &route, NARROWER, None),
            Some(Verb::Chrome(ChromeEffect::SetWidth { permille: Some(permille), .. })) if permille == WIDTH_NORMAL_PERMILLE
        ));
        handle
            .choose_window_menu(route.tab, NARROWER, None)
            .unwrap();
        settle(&handle);
        assert_eq!(
            handle.read(|tree| group_at(tree, route).width.permille()),
            WIDTH_NORMAL_PERMILLE
        );
        handle
            .choose_window_menu(route.tab, NARROWER, None)
            .unwrap();
        settle(&handle);
        assert_eq!(
            handle.read(|tree| group_at(tree, route).width.permille()),
            WIDTH_NARROW_PERMILLE
        );
        let rows = handle.window_menu(route.tab);
        let narrowest = row(&rows, NARROWER).unwrap();
        assert!(!narrowest.enabled, "no narrower than a window goes");
        assert_eq!(narrowest.reason.as_deref(), Some("Narrowest"));
        assert!(
            handle
                .choose_window_menu(route.tab, NARROWER, None)
                .is_err(),
            "and it means no command"
        );
        let _ = WIDTH_WIDE_PERMILLE;
    }

    /// Equalising gives every window of the strip the same share of it.
    #[test]
    fn equalizing_gives_every_window_of_the_strip_the_same_share() {
        let handle = apps_shell(2);
        let route = first_route(&handle);
        let mut tree = handle.read(Clone::clone);
        tree.workspaces[0].strips[0].window_groups[0].width = WindowWidth::Narrow;
        tree.workspaces[0].strips[0].window_groups[1].width = WindowWidth::Wide;
        handle.replace_tree(tree);
        settle(&handle);

        handle
            .choose_window_menu(route.tab, EQUALIZE, None)
            .unwrap();
        settle(&handle);
        let widths: Vec<WindowWidth> = handle.read(|tree| {
            tree.workspaces[0].strips[0]
                .window_groups
                .iter()
                .map(|group| group.width)
                .collect()
        });
        assert_eq!(
            widths,
            vec![WindowWidth::Fraction(500), WindowWidth::Fraction(500)],
            "two windows share the strip evenly"
        );
    }

    /// A window moves into another strip of its workspace, by that strip's name: the strip is made
    /// and the window moved in one pass, as the New strip row does.
    #[test]
    fn a_window_moves_to_another_strip_by_its_name() {
        let handle = shell(2);
        let route = first_route(&handle);
        let strip = handle
            .apply_command(&DesktopCommand::CreateStrip {
                name: "Research".into(),
            })
            .unwrap()
            .expect("the strip is made");

        let rows = handle.window_menu(route.tab);
        let across = row(&rows, &move_to_strip_id(strip)).expect("the strip is offered");
        assert!(across.enabled, "{across:?}");
        assert_eq!(
            across.label, "Research",
            "the submenu names the places it goes to"
        );
        let tree = handle.read(Clone::clone);
        assert!(matches!(
            command(&tree, &route, &across.id, None),
            Some(Verb::Chrome(ChromeEffect::MoveTab { to: Destination { strip: Some(ref to), .. }, .. })) if *to == strip.0.to_string()
        ));
        handle
            .choose_window_menu(route.tab, &across.id, None)
            .unwrap();
        settle(&handle);
        assert_eq!(
            handle.read(|tree| super::super::route_of(tree, route.tab).map(|ids| ids.strip)),
            Some(strip),
            "the window is there now"
        );
    }

    /// With one strip, a window that shares it is offered a new strip below, and choosing it makes the
    /// strip and carries the window there; a window alone there is offered no strip at all.
    #[test]
    fn a_window_sharing_the_only_strip_is_offered_a_new_one() {
        let handle = shell(2);
        let route = first_route(&handle);
        let rows = handle.window_menu(route.tab);
        let strips = row(&rows, MOVE_TO_STRIP).expect("the strip submenu shows");
        assert_eq!(ids(&strips.children), [MOVE_TO_NEW_STRIP]);
        assert_eq!(strips.children[0].label, "New strip");

        handle
            .choose_window_menu(route.tab, MOVE_TO_NEW_STRIP, None)
            .expect("the row is offered, so it is chosen");
        settle(&handle);
        let (strips, holder) = handle.read(|tree| {
            let workspace = &tree.workspaces[0];
            let holder = workspace.strips.iter().position(|strip| {
                strip
                    .window_groups
                    .iter()
                    .any(|g| g.tabs.iter().any(|tab| tab.id == route.tab))
            });
            (workspace.strips.len(), holder)
        });
        assert_eq!(strips, 2, "a strip was made below the only one");
        assert_eq!(holder, Some(1), "and the window is in it");

        let alone = shell(1);
        let route = first_route(&alone);
        assert!(
            row(&alone.window_menu(route.tab), MOVE_TO_STRIP).is_none(),
            "a window alone in the only strip has nowhere to go"
        );
    }

    /// A window moves into another workspace, by that workspace's name.
    #[test]
    fn a_window_moves_to_another_workspace_by_its_name() {
        let handle = apps_shell(1);
        let route = first_route(&handle);
        let mut tree = handle.read(Clone::clone);
        tree.workspaces
            .push(Workspace::new(WorkspaceId::from_u128(91), "Studio"));
        handle.replace_tree(tree);
        settle(&handle);

        let rows = handle.window_menu(route.tab);
        let across = row(
            &rows,
            &format!("{MOVE_TO_WORKSPACE}:{}", WorkspaceId::from_u128(91)),
        )
        .expect("the workspace is offered");
        assert!(across.enabled, "{across:?}");
        assert_eq!(
            across.label, "Studio",
            "the submenu names the workspaces it goes to"
        );
        let tree = handle.read(Clone::clone);
        assert!(matches!(
            command(&tree, &route, &across.id, None),
            Some(Verb::Chrome(ChromeEffect::MoveTab { to: Destination { workspace: Some(ref workspace), .. }, .. })) if *workspace == WorkspaceId::from_u128(91).to_string()
        ));
        handle
            .choose_window_menu(route.tab, &across.id, None)
            .unwrap();
        settle(&handle);
        let home: Vec<WorkspaceId> = handle.read(|tree| {
            tree.workspaces
                .iter()
                .filter(|workspace| {
                    workspace.strips.iter().any(|strip| {
                        strip
                            .window_groups
                            .iter()
                            .any(|group| group.tabs.iter().any(|tab| tab.id == route.tab))
                    })
                })
                .map(tilo_desktop_model::Workspace::id)
                .collect()
        });
        assert_eq!(
            home,
            vec![WorkspaceId::from_u128(91)],
            "the window is there now"
        );
    }

    /// Renders every tab as a notice of its own, and records the windows the session is asked to carry,
    /// as the session's own source does.
    struct Carrying {
        carried: Rc<RefCell<Vec<(TabId, WorkspaceId)>>>,
    }

    impl TileSource for Carrying {
        fn tile(&mut self, _: &Workspace, tab: &Tab) -> ui::widget::TileHost {
            ui::widget::TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }

        fn carry(&self, tab: TabId, to: WorkspaceId) -> bool {
            self.carried.borrow_mut().push((tab, to));
            true
        }
    }

    /// The window of an app this device started runs in the workspace that started it, with its grants
    /// and its records: its tab does not cross by itself, and the shell hands the move to the session
    /// instead, leaving the tab where it is until the session carries it.
    #[test]
    fn a_running_apps_window_is_handed_to_the_session_instead_of_crossing_by_its_tab() {
        let carried = Rc::new(RefCell::new(Vec::new()));
        let handle = shell(1);
        let mut tiles = Carrying {
            carried: Rc::clone(&carried),
        };
        let route = first_route(&handle);
        let mut tree = handle.read(Clone::clone);
        let home = tree.workspaces[0].id();
        tree.workspaces
            .push(Workspace::new(WorkspaceId::from_u128(91), "Studio"));
        // What the compositor mapped: a window of an app this device started.
        tree.workspaces[0].strips[0].window_groups[0].tabs[0].tile = TileContent::Foreign {
            app_id: "so.tilo.notes".into(),
            surface_id: 11,
        };
        handle.replace_tree(tree);
        settle_with(&handle, &mut tiles);

        handle
            .choose_window_menu(
                route.tab,
                &format!("{MOVE_TO_WORKSPACE}:{}", WorkspaceId::from_u128(91)),
                None,
            )
            .unwrap();
        settle_with(&handle, &mut tiles);

        assert_eq!(
            *carried.borrow(),
            vec![(route.tab, WorkspaceId::from_u128(91))],
            "the session is asked to carry the window where the person put it"
        );
        let below: Vec<WorkspaceId> = handle.read(|tree| {
            tree.workspaces
                .iter()
                .filter(|workspace| {
                    workspace.strips.iter().any(|strip| {
                        strip
                            .window_groups
                            .iter()
                            .any(|group| group.tabs.iter().any(|tab| tab.id == route.tab))
                    })
                })
                .map(tilo_desktop_model::Workspace::id)
                .collect()
        });
        assert_eq!(
            below,
            vec![home],
            "the tab stays where it was until the app runs in the other workspace"
        );
    }

    /// A program of this machine runs with no workspace around it: no grants, no records. The session
    /// carries its window at once, as it does a Tilo app's after the app has started again — the
    /// surface only ever hands the move over, and the session decides what crosses.
    #[test]
    fn a_programs_window_is_handed_over_too_and_the_session_moves_it_at_once() {
        let carried = Rc::new(RefCell::new(Vec::new()));
        let handle = shell(1);
        let mut tiles = Carrying {
            carried: Rc::clone(&carried),
        };
        let route = first_route(&handle);
        let mut tree = handle.read(Clone::clone);
        tree.workspaces
            .push(Workspace::new(WorkspaceId::from_u128(91), "Studio"));
        tree.workspaces[0].strips[0].window_groups[0].tabs[0].tile = TileContent::Foreign {
            app_id: "org.gnome.Nautilus".into(),
            surface_id: 12,
        };
        handle.replace_tree(tree);
        settle_with(&handle, &mut tiles);

        handle
            .choose_window_menu(
                route.tab,
                &format!("{MOVE_TO_WORKSPACE}:{}", WorkspaceId::from_u128(91)),
                None,
            )
            .unwrap();
        settle_with(&handle, &mut tiles);

        assert_eq!(
            *carried.borrow(),
            vec![(route.tab, WorkspaceId::from_u128(91))],
            "the window crosses only through the session"
        );
    }

    /// A window shows on another display, by that display's name, as the keyboard chord does.
    #[test]
    fn a_window_moves_to_another_display_by_its_name() {
        let handle = apps_shell(1);
        let route = first_route(&handle);
        let mut tree = handle.read(Clone::clone);
        let displays = vec![
            Display {
                id: 1,
                monitor: "left".into(),
                x: 0.0,
                y: 0.0,
                width: 1280.0,
                height: 800.0,
                primary: true,
            },
            Display {
                id: 2,
                monitor: "right".into(),
                x: 1280.0,
                y: 0.0,
                width: 1280.0,
                height: 800.0,
                primary: false,
            },
        ];
        tree.outputs.reconcile(&tree.workspaces, &displays);
        tree.outputs.place_unhomed(&tree.workspaces);
        handle.replace_tree(tree);
        settle(&handle);
        assert_eq!(
            handle.read(|tree| tree.outputs.display_of(route.group)),
            Some(1),
            "it starts on the first display"
        );

        let rows = handle.window_menu(route.tab);
        let across = row(&rows, &format!("{MOVE_TO_DISPLAY}:2")).expect("the display is offered");
        assert!(across.enabled, "{across:?}");
        assert_eq!(
            across.label, "Display 2",
            "the submenu names the displays it goes to"
        );
        let tree = handle.read(Clone::clone);
        assert!(matches!(
            command(&tree, &route, &across.id, None),
            Some(Verb::Chrome(ChromeEffect::MoveTab { to: Destination { display: Some(ref display), .. }, .. })) if display == "2"
        ));
        handle
            .choose_window_menu(route.tab, &across.id, None)
            .unwrap();
        settle(&handle);
        assert_eq!(
            handle.read(|tree| tree.outputs.display_of(route.group)),
            Some(2),
            "the tree shows it there now"
        );
        // On the display it is on, the row offers the other one instead.
        let rows = handle.window_menu(route.tab);
        assert!(row(&rows, &format!("{MOVE_TO_DISPLAY}:2")).is_none());
        assert!(row(&rows, &format!("{MOVE_TO_DISPLAY}:1")).is_some());
    }

    /// The new tab row opens another tab of the window's app, right beside it.
    #[test]
    fn a_new_tab_row_opens_another_tab_of_its_app_beside_it() {
        let handle = apps_shell(1);
        let mut tiles = Apps;
        let route = first_route(&handle);
        handle.choose_window_menu(route.tab, NEW_TAB, None).unwrap();
        settle_with(&handle, &mut tiles);
        let group = handle.read(|tree| group_at(tree, route));
        assert_eq!(group.tabs.len(), 2, "the tab opened beside it");
        assert_eq!(group.tabs[1].id, group.active_tab, "and in front");
        assert_eq!(
            tilo_desktop_model::apps::canonical_app_id(&group.tabs[0].tile),
            tilo_desktop_model::apps::canonical_app_id(&group.tabs[1].tile),
            "it is the same app"
        );
        // Two tabs make the window a tabbed one: it offers the tab's own rows.
        let rows = handle.window_menu(route.tab);
        assert!(row(&rows, CLOSE).is_some());
        assert!(row(&rows, SPLIT_RIGHT).is_some());
    }

    /// The share row is offered exactly for the windows the sharing app lists,
    /// and choosing it makes the request the window bar's item makes.
    #[test]
    fn only_a_window_the_sharing_app_lists_offers_the_share_row() {
        let handle = apps_shell(1);
        let route = first_route(&handle);
        assert!(
            row(&handle.window_menu(route.tab), SHARE).is_none(),
            "a window the sharing app does not list offers no share"
        );

        let window = OpenWindow {
            tab: route.tab,
            app: "so.tilo.app0".into(),
            app_title: "App0".into(),
            title: "App0".into(),
            record: String::new(),
            kind: String::new(),
            item: None,
        };
        handle.shared.borrow_mut().window_offers = vec![window.clone()];
        let rows = handle.window_menu(route.tab);
        let share = row(&rows, SHARE).expect("the window is listed: it offers the share");
        assert_eq!(
            share.label,
            ui::window_share::host::menu_item(&window, &ui::window_share::shared()).name
        );
        let tree = handle.read(Clone::clone);
        assert!(matches!(
            command(&tree, &route, SHARE, None),
            Some(Verb::Chrome(ChromeEffect::ShareWindow { on: true, .. }))
        ));
        assert!(
            handle.choose_window_menu(route.tab, SHARE, None).is_ok(),
            "it is sent to the sharing app as the bar's item is"
        );

        // A window the sharing app holds is offered the way back instead.
        let shared = Facts {
            sharing: Some(Sharing {
                label: "Stop sharing".into(),
                shared: true,
            }),
            ..Facts::default()
        };
        let rows = super::rows(&tree, &route, &shared);
        assert!(row(&rows, SHARE).is_none());
        assert_eq!(
            row(&rows, UN_SHARE).map(|row| row.label.as_str()),
            Some("Stop sharing")
        );
        assert!(matches!(
            command(&tree, &route, UN_SHARE, None),
            Some(Verb::Chrome(ChromeEffect::ShareWindow { on: false, .. }))
        ));
    }

    /// What a session reads of the windows it shares with its workspaces, as a shell asks it: the
    /// reading a test changes, and every question the shell put to it.
    #[derive(Default)]
    struct SessionShares {
        /// The workspaces the session takes part in, by id.
        takes_part: RefCell<Vec<String>>,
        /// The windows it shares, and how many devices view each.
        shared: RefCell<BTreeMap<TabId, usize>>,
        /// Whether nothing of the windows it shares goes out to the devices that asked to see them.
        nothing_sent: Cell<bool>,
        /// The window and workspace of every question, in order.
        asked: RefCell<Vec<(TabId, String)>>,
    }

    impl SessionShares {
        /// The reading as `handle` 's shell is given it.
        fn read_by(self: &Rc<Self>, handle: &DesktopHandle) {
            let shares = Rc::clone(self);
            let reading: crate::desktop::state::WorkspaceShares = Rc::new(move |tab, window| {
                let workspace = window.workspace;
                shares.asked.borrow_mut().push((tab, workspace.to_owned()));
                if !shares.takes_part.borrow().iter().any(|id| id == workspace) {
                    return None;
                }
                let viewers = shares.shared.borrow().get(&tab).copied();
                Some(WorkspaceShare {
                    shared: viewers.is_some(),
                    viewers: viewers.unwrap_or(0),
                    streaming: !shares.nothing_sent.get(),
                })
            });
            handle.reads_workspace_shares(reading);
        }
    }

    /// A window of an outside program this device maps.
    fn mapped_notes() -> TileContent {
        TileContent::Foreign {
            app_id: "so.tilo.notes".into(),
            surface_id: 41,
        }
    }

    /// A shell of the device minting under 7 whose one window shows `tile`, in a tab minted under
    /// `salt`: the handle and the tab.
    fn shell_showing(salt: u16, tile: TileContent) -> (DesktopHandle, TabId) {
        let mut minter = IdMinter::new(salt);
        let id = minter.mint();
        let tab = Tab {
            id,
            title: "Notes".into(),
            tile,
            app_title: None,
            view: String::new(),
            pinned: false,
        };
        let tree = seed_tree(&mut minter, "Shell", vec![tab]);
        (DesktopHandle::new(tree, 7), id)
    }

    /// The id of the workspace the shell's one window stands in.
    fn workspace_of(handle: &DesktopHandle) -> String {
        handle.read(|tree| tree.workspaces[0].policy.workspace.clone())
    }

    /// A session that takes part in the workspace of `handle` 's shell and shares nothing yet, read by
    /// that shell.
    fn session_sharing_with(handle: &DesktopHandle) -> Rc<SessionShares> {
        let shares = Rc::new(SessionShares::default());
        shares.takes_part.borrow_mut().push(workspace_of(handle));
        shares.read_by(handle);
        shares
    }

    /// The share with the workspace is offered where it can work and nowhere else: for a window this
    /// device maps, in a tab it minted, in a workspace its session takes part in. Another device's
    /// window, the picture of one, an app still to open and a shell whose session shares nothing with
    /// the workspace offer no such row.
    #[test]
    fn only_a_window_this_device_can_share_offers_the_share_with_its_workspace() {
        let (handle, tab) = shell_showing(7, mapped_notes());
        assert!(
            row(&handle.window_menu(tab), SHARE_WITH_WORKSPACE).is_none(),
            "a shell no session reads the shares for offers no share"
        );

        let shares = session_sharing_with(&handle);
        let rows = handle.window_menu(tab);
        let share = row(&rows, SHARE_WITH_WORKSPACE)
            .expect("a window this device maps in a workspace it shares with offers the share");
        assert_eq!(share.label, "Share with workspace");
        assert!(share.enabled && share.icon.is_some());
        assert!(row(&rows, STOP_SHARING_WITH_WORKSPACE).is_none());
        assert_eq!(
            shares.asked.borrow().last(),
            Some(&(tab, workspace_of(&handle))),
            "the session is asked about the tab, in the workspace the tab stands in"
        );

        // A session that takes no part in the workspace has nobody to share with.
        shares.takes_part.borrow_mut().clear();
        assert!(row(&handle.window_menu(tab), SHARE_WITH_WORKSPACE).is_none());

        // A shell in no workspace is not asked about at all.
        shares.takes_part.borrow_mut().push(String::new());
        let mut nowhere = handle.read(Clone::clone);
        nowhere.workspaces[0].policy.workspace.clear();
        handle.replace_tree(nowhere);
        let asked = shares.asked.borrow().len();
        assert!(row(&handle.window_menu(tab), SHARE_WITH_WORKSPACE).is_none());
        assert_eq!(shares.asked.borrow().len(), asked);

        let app = || "so.tilo.notes".to_owned();
        let not_this_devices = [
            // The picture of a window another device shares.
            (
                9,
                TileContent::HostedWindow {
                    app_id: app(),
                    label: "Notes".into(),
                    host: "ab".repeat(32),
                    host_label: "Framework".into(),
                },
            ),
            // The same in a tab this device minted: what the tab shows decides, too.
            (
                7,
                TileContent::HostedWindow {
                    app_id: app(),
                    label: "Notes".into(),
                    host: "ab".repeat(32),
                    host_label: "Framework".into(),
                },
            ),
            // An app whose window has yet to open here.
            (
                7,
                TileContent::LocalApp {
                    app_id: app(),
                    label: "Notes".into(),
                    note: String::new(),
                },
            ),
            // A window mapped here in a tab another device minted is that device's to share.
            (9, mapped_notes()),
        ];
        for (salt, tile) in not_this_devices {
            let (handle, tab) = shell_showing(salt, tile.clone());
            let shares = session_sharing_with(&handle);
            shares.shared.borrow_mut().insert(tab, 1);
            let rows = handle.window_menu(tab);
            assert!(
                row(&rows, SHARE_WITH_WORKSPACE).is_none()
                    && row(&rows, STOP_SHARING_WITH_WORKSPACE).is_none(),
                "{tile:?} in a tab minted under {salt} is not this device's to share"
            );
            assert_eq!(
                handle
                    .choose_window_menu(tab, SHARE_WITH_WORKSPACE, None)
                    .err()
                    .as_deref(),
                Some("That is not something this window offers")
            );
            assert!(handle.take_workspace_share_asks().is_empty());
        }
    }

    /// The row says what the session reads of the share when the menu is asked for, every time: the
    /// shell keeps no flag of its own, so a share started, viewed, and ended elsewhere (a script, the
    /// window closing) is what the menu shows next.
    #[test]
    fn the_share_with_the_workspace_reads_as_the_session_says_it_stands() {
        let (handle, tab) = shell_showing(7, mapped_notes());
        let shares = session_sharing_with(&handle);
        let labels = |handle: &DesktopHandle| -> (Option<String>, Option<String>) {
            let rows = handle.window_menu(tab);
            let label = |id| row(&rows, id).map(|row| row.label.clone());
            (
                label(SHARE_WITH_WORKSPACE),
                label(STOP_SHARING_WITH_WORKSPACE),
            )
        };
        assert_eq!(labels(&handle), (Some("Share with workspace".into()), None));

        shares.shared.borrow_mut().insert(tab, 0);
        assert_eq!(
            labels(&handle),
            (None, Some("Stop sharing with workspace".into())),
            "a shared window offers the way back instead"
        );
        shares.shared.borrow_mut().insert(tab, 1);
        assert_eq!(
            labels(&handle),
            (
                None,
                Some("Stop sharing with workspace (1 watching)".into())
            )
        );
        shares.shared.borrow_mut().insert(tab, 3);
        assert_eq!(
            labels(&handle),
            (
                None,
                Some("Stop sharing with workspace (3 watching)".into())
            )
        );

        // Devices that asked to see a window nothing goes out of wait for it, and the row says so: it
        // counts nobody as watching who sees nothing.
        shares.nothing_sent.set(true);
        assert_eq!(
            labels(&handle),
            (None, Some("Stop sharing with workspace (3 waiting)".into()))
        );
        ui::i18n::I18nManager::set_locale("de-DE");
        let german_waiting = labels(&handle);
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(
            german_waiting,
            (
                None,
                Some("Teilen mit dem Arbeitsbereich beenden (3 warten)".into())
            )
        );
        shares.nothing_sent.set(false);

        ui::i18n::I18nManager::set_locale("de-DE");
        let german_shared = labels(&handle);
        shares.shared.borrow_mut().clear();
        let german = labels(&handle);
        ui::i18n::I18nManager::set_locale("en-US");
        assert_eq!(
            german_shared,
            (
                None,
                Some("Teilen mit dem Arbeitsbereich beenden (3 sehen zu)".into())
            )
        );
        assert_eq!(german, (Some("Mit dem Arbeitsbereich teilen".into()), None));
        assert_eq!(
            labels(&handle),
            (Some("Share with workspace".into()), None),
            "the share ended in the session, and the menu says so without being told"
        );
    }

    /// Choosing the row leaves the ask for the session, by the tab and nothing else, and changes
    /// nothing the menu says: the window reads as shared once the session's reading does.
    #[test]
    fn choosing_the_share_with_the_workspace_asks_the_session_by_the_tab() {
        let (handle, tab) = shell_showing(7, mapped_notes());
        let shares = session_sharing_with(&handle);
        assert!(handle.take_workspace_share_asks().is_empty());

        handle
            .choose_window_menu(tab, SHARE_WITH_WORKSPACE, None)
            .expect("the row is offered, so it is chosen");
        assert_eq!(handle.take_workspace_share_asks(), vec![(tab, true, None)]);
        assert!(
            handle.take_workspace_share_asks().is_empty(),
            "an ask is taken once"
        );
        assert!(
            row(&handle.window_menu(tab), SHARE_WITH_WORKSPACE).is_some(),
            "asking shares nothing by itself: the session's reading still says it is not shared"
        );
        assert_eq!(
            handle
                .choose_window_menu(tab, STOP_SHARING_WITH_WORKSPACE, None)
                .err()
                .as_deref(),
            Some("That is not something this window offers"),
            "a window that is not shared offers no stop"
        );

        shares.shared.borrow_mut().insert(tab, 2);
        assert_eq!(
            handle
                .choose_window_menu(tab, SHARE_WITH_WORKSPACE, None)
                .err()
                .as_deref(),
            Some("That is not something this window offers"),
            "a shared window is not shared twice"
        );
        handle
            .choose_window_menu(tab, STOP_SHARING_WITH_WORKSPACE, None)
            .expect("a shared window offers the stop");
        assert_eq!(handle.take_workspace_share_asks(), vec![(tab, false, None)]);
    }

    /// On a desktop several people share, a window is its holder's to share: anyone else sees the
    /// row and why they cannot choose it, and choosing it asks nothing.
    #[test]
    fn someone_elses_window_is_theirs_to_share_with_the_workspace() {
        let (handle, tab) = shell_showing(7, mapped_notes());
        let _shares = session_sharing_with(&handle);
        assert!(handle.set_holders(HashMap::from([(
            tab,
            TabHolder {
                name: "Ben".into(),
                rgb: (200, 120, 40),
                keeps: true,
            },
        )])));
        let rows = handle.window_menu(tab);
        let share = row(&rows, SHARE_WITH_WORKSPACE).expect("the row is shown");
        assert!(!share.enabled);
        assert_eq!(share.reason.as_deref(), Some("Ben's window"));
        assert_eq!(
            handle
                .choose_window_menu(tab, SHARE_WITH_WORKSPACE, None)
                .err()
                .as_deref(),
            Some("Ben's window")
        );
        assert!(handle.take_workspace_share_asks().is_empty());
    }

    /// A window held by someone nobody named says whose it is without hanging a possessive on the
    /// words that stand for their name.
    #[test]
    fn someone_unnamed_elses_window_says_someone_else() {
        ui::i18n::I18nManager::init("en-US");
        let (handle, tab) = shell_showing(7, mapped_notes());
        let _shares = session_sharing_with(&handle);
        assert!(handle.set_holders(HashMap::from([(
            tab,
            TabHolder {
                name: ui::presence::unnamed_person_label(),
                rgb: (200, 120, 40),
                keeps: true,
            },
        )])));
        let rows = handle.window_menu(tab);
        let share = row(&rows, SHARE_WITH_WORKSPACE).expect("the row is shown");
        assert!(!share.enabled);
        assert_eq!(share.reason.as_deref(), Some("Someone else's window"));
    }

    /// A window the sharing app lists offers both shares, side by side in the sharing group, and each
    /// says which it is: the window's live picture to the workspace, or its record in a channel.
    #[test]
    fn the_two_shares_of_a_window_stand_together_and_read_apart() {
        let (handle, tab) = shell_showing(7, mapped_notes());
        let shares = session_sharing_with(&handle);
        handle.shared.borrow_mut().window_offers = vec![OpenWindow {
            tab,
            app: "so.tilo.notes".into(),
            app_title: "Notes".into(),
            title: "Notes".into(),
            record: String::new(),
            kind: String::new(),
            item: None,
        }];
        let rows = handle.window_menu(tab);
        let sharing: Vec<(&str, &str)> = rows
            .split(|row| row.separator)
            .find(|group| group.iter().any(|row| row.id == SHARE))
            .expect("the sharing group")
            .iter()
            .map(|row| (row.id.as_str(), row.label.as_str()))
            .collect();
        assert_eq!(
            sharing,
            vec![
                (SHARE_WITH_WORKSPACE, "Share with workspace"),
                (SHARE, "Share to channel…"),
            ]
        );
        let icon = |id| row(&rows, id).and_then(|row| row.icon);
        assert_ne!(icon(SHARE_WITH_WORKSPACE), icon(SHARE));

        // Shared with the workspace, the stop names what it stops; the channel's row is as it was.
        shares.shared.borrow_mut().insert(tab, 0);
        let rows = handle.window_menu(tab);
        assert_eq!(
            row(&rows, STOP_SHARING_WITH_WORKSPACE).map(|row| row.label.as_str()),
            Some("Stop sharing with workspace")
        );
        assert_eq!(
            row(&rows, SHARE).map(|row| row.label.as_str()),
            Some("Share to channel…")
        );
    }

    /// Only a host (non-Tilo) window's tab offers a name; it names the window in place, and a name too
    /// long to show is refused.
    #[test]
    fn only_a_host_windows_tab_offers_a_name() {
        let handle = apps_shell(1);
        let route = first_route(&handle);
        let mut tree = handle.read(Clone::clone);
        tree.workspaces[0].strips[0].window_groups[0].tabs[0].tile = TileContent::Foreign {
            app_id: "Alacritty".into(),
            surface_id: 1,
        };
        handle.replace_tree(tree);
        settle(&handle);

        let rows = handle.window_menu(route.tab);
        let name = row(&rows, RENAME).expect("a host window's tab is named");
        assert_eq!(name.label, "Name this window…");
        let tree = handle.read(Clone::clone);
        assert!(matches!(
            command(&tree, &route, RENAME, Some("Plans")),
            Some(Verb::Chrome(ChromeEffect::Rename { .. }))
        ));
        assert!(
            command(&tree, &route, RENAME, None).is_none(),
            "a row without a name is no command"
        );
        let long = "x".repeat(crate::desktop::navigation::MAX_NAME_CHARS.saturating_add(1));
        assert!(
            handle
                .choose_window_menu(route.tab, RENAME, Some(&long))
                .is_err(),
            "a name too long to show is refused"
        );
        handle
            .choose_window_menu(route.tab, RENAME, Some("Plans"))
            .unwrap();
        settle(&handle);
        let named = handle
            .snapshot("g", vec![])
            .entries
            .into_iter()
            .find(|entry| entry.route.tab == route.tab.to_string())
            .map(|entry| entry.label)
            .unwrap();
        assert_eq!(named, "Plans");
    }

    /// Split right takes the tab out into a window of its own right after the one it was in, and Move
    /// to a new window takes it to the strip's end.
    #[test]
    fn a_tab_taken_out_gets_a_window_of_its_own() {
        let handle = tabbed_shell(3, 5);
        let route = first_route(&handle);
        let groups = |handle: &DesktopHandle| -> Vec<(u64, Vec<TabId>)> {
            handle.read(|tree| {
                tree.workspaces[0].strips[0]
                    .window_groups
                    .iter()
                    .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
                    .collect()
            })
        };
        let tabs = groups(&handle)[0].1.clone();

        handle
            .choose_window_menu(tabs[0], SPLIT_RIGHT, None)
            .unwrap();
        settle(&handle);
        let now = groups(&handle);
        assert_eq!(
            now[0],
            (route.group, vec![tabs[1], tabs[2]]),
            "the source stays where it was"
        );
        assert_eq!(
            now[1].1,
            vec![tabs[0]],
            "the tab has a window to itself beside it"
        );
        assert_eq!(
            handle.read(|tree| tree.active_group.get(&route.strip).copied()),
            Some(now[1].0),
            "and the strip shows it"
        );

        handle.choose_window_menu(tabs[1], NEW_GROUP, None).unwrap();
        settle(&handle);
        let now = groups(&handle);
        assert_eq!(now[0].1, vec![tabs[2]], "the other tab stays in its window");
        assert_eq!(
            now.last().map(|(_, tabs)| tabs.clone()),
            Some(vec![tabs[1]]),
            "the tab is at the end"
        );
    }

    /// A tabbed window offers closing the tab and closing the window; closing the window takes every
    /// tab with it, and a reopen brings it back.
    #[test]
    fn closing_the_tab_leaves_the_window_that_closing_it_ends() {
        let handle = one_window_shell(2);
        let route = first_route(&handle);
        let other = handle.read(|tree| {
            tree.workspaces[0].strips[0].window_groups[0]
                .tabs
                .iter()
                .map(|tab| tab.id)
                .find(|tab| *tab != route.tab)
                .unwrap()
        });
        let rows = handle.window_menu(route.tab);
        assert!(row(&rows, CLOSE).is_some());
        assert!(row(&rows, CLOSE_WINDOW).is_some());

        handle.choose_window_menu(route.tab, CLOSE, None).unwrap();
        settle(&handle);
        let group = handle.read(|tree| group_at(tree, route));
        assert_eq!(group.tabs.len(), 1, "the tab closed, the window stayed");
        assert_eq!(group.tabs[0].id, other, "the other tab is what is left");

        handle
            .choose_window_menu(other, CLOSE_WINDOW, None)
            .unwrap();
        settle(&handle);
        assert!(
            handle.read(|tree| tree.workspaces[0].strips[0].window_groups.is_empty()),
            "the window and its tab are gone"
        );
        assert!(handle.snapshot("g", vec![]).entries.is_empty());
        handle
            .dispatch(ChromeEffect::ReopenClosed {
                app: None,
                window: None,
            })
            .unwrap();
        settle(&handle);
        assert_eq!(
            handle.read(|tree| tree.workspaces[0].strips[0].window_groups.len()),
            1,
            "a reopen undoes the close"
        );
    }

    /// Closing the last window of a strip says what closed and offers Undo, which brings it back as the
    /// reopen chord does.
    #[test]
    fn a_close_says_what_closed_and_its_undo_reopens_it() {
        let handle = one_window_shell(1);
        let route = first_route(&handle);
        let said = handle.notice();
        assert!(said.borrow().get().is_none());

        handle
            .choose_window_menu(route.tab, CLOSE_WINDOW, None)
            .unwrap();
        settle(&handle);
        assert!(
            handle.read(|tree| tree.workspaces[0].strips[0].window_groups.is_empty()),
            "the strip's last window closed"
        );
        let shown = said.borrow().get().cloned().expect("the close is said");
        assert!(shown.text.ends_with("closed"), "{:?}", shown.text);
        let undo = &shown.actions[0];
        assert_eq!((undo.name.as_str(), undo.icon.as_str()), ("Undo", "undo-2"));
        let (since, gone) = said.borrow().span().expect("it is up");
        assert_eq!(
            gone - since,
            ui::widget::toast::ACTION_NOTICE_LIFETIME,
            "it stays long enough to reach Undo"
        );

        assert!(handle.press_notice(&undo.id), "Undo is there to press");
        settle(&handle);
        assert_eq!(
            handle.read(|tree| tree.workspaces[0].strips[0].window_groups.len()),
            1,
            "Undo reopens the window"
        );
        assert!(said.borrow().get().is_none(), "and the notice goes");
        assert!(!handle.press_notice(&undo.id), "an Undo is pressed once");
    }

    /// A window the system opened for something now over (a sign-in's page) closes without saying
    /// so: the person did not close it, so nothing offers to undo it.
    #[test]
    fn a_window_the_system_dismisses_closes_without_an_undo() {
        let handle = one_window_shell(1);
        let route = first_route(&handle);
        let said = handle.notice();

        assert!(handle.dismiss_window(route.tab));
        settle(&handle);
        assert!(
            handle.read(|tree| tree.workspaces[0].strips[0].window_groups.is_empty()),
            "the window closed"
        );
        assert!(said.borrow().get().is_none(), "and nothing was said of it");
        assert!(
            !handle.dismiss_window(route.tab),
            "a window that is gone is not dismissed"
        );
    }

    /// A window running an app that controls the desktop offers to end that,
    /// and the row is gone once the grant is taken back.
    #[test]
    fn stopping_desktop_control_takes_the_grant_back() {
        let handle = apps_shell(1);
        let route = first_route(&handle);
        assert!(row(&handle.window_menu(route.tab), STOP_DESKTOP_CONTROL).is_none());
        let mut tree = handle.read(Clone::clone);
        let policy = &mut tree.workspaces[0].policy;
        let control = tilo_desktop_model::commands::control_grant(
            policy,
            "so.tilo.app0",
            tilo_action_policy::Grantee::person("ada"),
        );
        policy.grant(control);
        handle.replace_tree(tree);
        let rows = handle.window_menu(route.tab);
        let stop = row(&rows, STOP_DESKTOP_CONTROL).expect("the app controls the desktop");
        assert!(stop.enabled);
        assert_eq!(stop.label, "Stop controlling the desktop");
        let tree = handle.read(Clone::clone);
        assert!(matches!(
            command(&tree, &route, STOP_DESKTOP_CONTROL, None),
            Some(Verb::Chrome(ChromeEffect::StopDesktopControl { .. }))
        ));
        handle
            .choose_window_menu(route.tab, STOP_DESKTOP_CONTROL, None)
            .unwrap();
        settle(&handle);
        assert!(
            !handle.read(|tree| tilo_desktop_model::commands::has_control_at(
                &tree.workspaces[0].policy,
                "so.tilo.app0",
                tilo_clock::unix_now()
            )),
            "the grant is taken back"
        );
        assert!(row(&handle.window_menu(route.tab), STOP_DESKTOP_CONTROL).is_none());
    }

    /// A window someone else holds is theirs to move and to close: those rows say whose it is, and
    /// choosing one is refused.
    #[test]
    fn someone_elses_window_offers_no_move_nor_close() {
        let handle = one_window_shell(2);
        let route = first_route(&handle);
        assert!(handle.set_holders(HashMap::from([(
            route.tab,
            TabHolder {
                name: "Ben".into(),
                rgb: (200, 120, 40),
                keeps: true,
            }
        )])));
        let rows = handle.window_menu(route.tab);
        for id in [CLOSE, CLOSE_WINDOW, SPLIT_RIGHT, NEW_GROUP, NEW_TAB] {
            let held = row(&rows, id).unwrap_or_else(|| panic!("{id} is offered"));
            assert!(!held.enabled, "{id} is theirs: {held:?}");
            assert_eq!(held.reason.as_deref(), Some("Ben's window"));
        }
        assert!(
            row(&rows, MAXIMIZE).unwrap().enabled,
            "still theirs to fill"
        );
        assert!(
            handle.choose_window_menu(route.tab, CLOSE, None).is_err(),
            "choosing it is refused"
        );
    }

    /// A window the tree no longer holds offers no rows and no command.
    #[test]
    fn a_window_that_is_gone_offers_nothing() {
        let handle = shell(1);
        let route = first_route(&handle);
        let mut gone = handle.read(Clone::clone);
        gone.workspaces[0].strips[0]
            .window_groups
            .retain(|group| group.id != route.group);
        handle.replace_tree(gone);
        let tree = handle.read(Clone::clone);
        assert!(super::rows(&tree, &route, &Facts::default()).is_empty());
        assert!(command(&tree, &route, CLOSE, None).is_none());
    }

    /// A window moves one place along its strip from its menu, and a side it cannot go to offers no row.
    #[test]
    fn a_window_moves_one_place_along_its_strip() {
        let handle = apps_shell(3);
        let order = || -> Vec<TabId> { (0..3).map(|group| tab_at(&handle, group)).collect() };
        let [first, second, third] = order()[..] else {
            panic!("three windows");
        };
        let offers = |tab: TabId| {
            let rows = handle.window_menu(tab);
            (
                row(&rows, MOVE_LEFT).is_some(),
                row(&rows, MOVE_RIGHT).is_some(),
            )
        };
        assert_eq!(
            [offers(first), offers(second), offers(third)],
            [(false, true), (true, true), (true, false)]
        );
        for (id, after) in [
            (MOVE_RIGHT, [second, first, third]),
            (MOVE_RIGHT, [second, third, first]),
            (MOVE_LEFT, [second, first, third]),
        ] {
            handle.choose_window_menu(first, id, None).unwrap();
            settle(&handle);
            assert_eq!(order(), after, "{id}");
        }
    }
}
