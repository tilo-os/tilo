//! Searchable window rows over the desktop topology.
//!
//! Every tab and window group in the active workspace becomes an exactly
//! addressed row: go to a tab, close a group, close a tab, or move a tab to
//! another workspace. The rows are render-free data; a host ranks them by
//! their names with its own scorer and applies the [`WindowAction`] through
//! its tab lifecycle, so an invocation never synthesises pointer input.
//!
//! Going to a window is what a search for it means. Closing, moving and
//! copying are listed in full for an empty query and for their exact
//! address, but a search only offers them once it names their verb, so Enter
//! on a window's name never closes it.
use std::fmt::Write as _;

use tilo_desktop_model::{
    DesktopTree, GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, Workspace,
    WorkspaceId,
};

use crate::i18n::{t, t_args};

/// The provider id window rows are addressed under.
pub const WINDOWS_PROVIDER_ID: &str = "desktop-windows";

/// What a window row names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRowKind {
    Tab,
    WindowGroup,
}

/// What invoking a window row does to the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowAction {
    /// Bring one tab to the front: its workspace, strip, group and tab.
    FocusTab {
        workspace: WorkspaceId,
        strip: StripId,
        group: GroupId,
        tab: TabId,
    },
    /// Close every tab of the group, remembered as one close.
    CloseGroup { group: GroupId },
    /// Close one tab through the tab lifecycle.
    CloseTab { group: GroupId, tab: TabId },
    /// Move one tab to another workspace. A client window's document
    /// leaves its workspace once the person agrees to what crosses.
    MoveTab {
        group: GroupId,
        tab: TabId,
        workspace: WorkspaceId,
    },
    /// Give another workspace its own copy of a client window's document,
    /// history included; the original window stays.
    CopyTab {
        group: GroupId,
        tab: TabId,
        workspace: WorkspaceId,
    },
    /// [`Self::MoveTab`] to the workspace the invocation names in its
    /// [`DESTINATION_PARAM`]: the one row a search for "move" offers.
    MoveTabTo { group: GroupId, tab: TabId },
    /// [`Self::CopyTab`] to the workspace named in [`DESTINATION_PARAM`].
    CopyTabTo { group: GroupId, tab: TabId },
}

/// The parameter naming the workspace a chosen move or copy goes to.
pub const DESTINATION_PARAM: &str = "workspace";

/// One window row with its localized copy and the names it answers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRow {
    pub kind: WindowRowKind,
    pub id: String,
    pub title: String,
    /// What tells this row apart beyond its title: the strip a window is on
    /// once there are several, the workspace a move starts from. Never the
    /// kind of row or a count.
    pub subtitle: Option<String>,
    /// The workspaces a chosen move or copy may go to, by name; empty for
    /// every other row.
    pub destinations: Vec<String>,
    /// What a person calls this row: its visible names and verb, never an
    /// internal id. A host ranks these as titles.
    pub names: Vec<String>,
    /// A weaker name, such as the app a window belongs to; ranked as a
    /// subtitle.
    pub hint: Option<String>,
    pub action: WindowAction,
}

/// The provider's name in the desktop's language.
pub fn windows_provider_name() -> String {
    t("search-provider-desktop-windows")
}

/// The tile whose face a window row wears — the same artwork the tab
/// strip and overview card show.
///
/// A tab row (`tab.focus`, `tab.close`, `tab.move`, `tab.copy`) wears
/// that tab. A group-close row wears the group's active tab, or its
/// first tab if the active one is gone.
pub fn window_row_tile<'a>(tree: &'a DesktopTree, id: &str) -> Option<&'a TileContent> {
    let (group_id, tab_id) = window_row_face(id)?;
    let group = tree
        .workspaces
        .iter()
        .flat_map(|workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| strip.window_groups.iter())
        })
        .find(|group| group.id == group_id)?;
    if let Some(tab_id) = tab_id {
        return group
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id)
            .map(|tab| &tab.tile);
    }
    group
        .tabs
        .iter()
        .find(|tab| tab.id == group.active_tab)
        .or_else(|| group.tabs.first())
        .map(|tab| &tab.tile)
}

fn window_row_face(id: &str) -> Option<(GroupId, Option<TabId>)> {
    let tab_rest = ["tab.focus:", "tab.close:", "tab.move:", "tab.copy:"]
        .into_iter()
        .find_map(|prefix| id.strip_prefix(prefix));
    if let Some(rest) = tab_rest {
        let mut parts = rest.split(':');
        let group = parts.next()?.parse().ok()?;
        let tab = parts.next()?.parse().ok()?;
        return Some((group, Some(tab)));
    }
    let rest = id.strip_prefix("group.close:")?;
    let group = rest.split(':').next()?.parse().ok()?;
    Some((group, None))
}

/// The exact address of going to `tab` in `group`.
pub fn tab_focus_id(group: GroupId, tab: TabId) -> String {
    format!("tab.focus:{group}:{tab}")
}

/// The exact address of closing `tab` in `group`.
pub fn tab_close_id(group: GroupId, tab: TabId) -> String {
    format!("tab.close:{group}:{tab}")
}

/// The exact address of closing `group`.
pub fn group_close_id(group: GroupId) -> String {
    format!("group.close:{group}")
}

/// The exact address of moving `tab` out of `group` to `workspace`.
pub fn tab_move_id(group: GroupId, tab: TabId, workspace: WorkspaceId) -> String {
    format!("tab.move:{group}:{tab}:{}", workspace.raw())
}

/// The exact address of moving `tab` out of `group` to a workspace the
/// invocation names.
pub fn tab_move_to_id(group: GroupId, tab: TabId) -> String {
    format!("tab.move:{group}:{tab}")
}

/// The exact address of copying `tab`'s document in `group` into a
/// workspace the invocation names.
pub fn tab_copy_to_id(group: GroupId, tab: TabId) -> String {
    format!("tab.copy:{group}:{tab}")
}

/// The exact address of copying `tab`'s document in `group` into `workspace`.
pub fn tab_copy_id(group: GroupId, tab: TabId, workspace: WorkspaceId) -> String {
    format!("tab.copy:{group}:{tab}:{}", workspace.raw())
}

fn app_id(tile: &TileContent) -> Option<&str> {
    match tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. }
        | TileContent::HostedWindow { app_id, .. } => Some(app_id),
        TileContent::AppPlaceholder { .. } | TileContent::Stub { .. } => None,
    }
}

/// The app a window row's tab belongs to, as an app id: what the window
/// announces, or for a built-in the package it is (`so.tilo.file-explorer`).
/// Search shows one row per app, so it needs to know which app row a window
/// row is the same thing as.
pub fn window_row_app_id(tree: &DesktopTree, id: &str) -> Option<String> {
    match window_row_tile(tree, id)? {
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
            Some(kind.app_id())
        }
        tile => app_id(tile).map(str::to_owned),
    }
}

/// Every app with a window open anywhere on the desktop, by app id: what
/// each window announces, or for a built-in the package it is.
pub fn open_app_ids(tree: &DesktopTree) -> std::collections::BTreeSet<String> {
    tree.workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .filter_map(|tab| match &tab.tile {
            TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => {
                Some(kind.app_id())
            }
            tile => app_id(tile).map(str::to_owned),
        })
        .collect()
}

/// The readable end of an app id — `so.tilo.file-explorer` answers to
/// "file explorer" — so a window titled after its document is still found
/// by its app.
fn app_hint(tile: &TileContent) -> Option<String> {
    let last = app_id(tile)?.rsplit('.').next()?;
    let hint = last.replace(['-', '_'], " ");
    (!hint.trim().is_empty()).then_some(hint)
}

/// Which window verbs a query names. An empty query names every verb: a
/// listing is complete, only a search is selective.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Verbs {
    close: bool,
    relocate: bool,
}

impl Verbs {
    fn of(needle: &str) -> Self {
        let words: Vec<String> = needle
            .split_whitespace()
            .map(str::to_lowercase)
            .filter(|word| word.chars().count() >= 3)
            .collect();
        if needle.trim().is_empty() {
            return Self {
                close: true,
                relocate: true,
            };
        }
        let names = |key: &str| {
            let keywords = t(key).to_lowercase();
            words.iter().any(|word| {
                keywords
                    .split_whitespace()
                    .any(|keyword| keyword.starts_with(word.as_str()))
            })
        };
        Self {
            close: names("search-window-verb-close"),
            relocate: names("search-window-verb-relocate"),
        }
    }
}

fn focused_group<'a>(tree: &'a DesktopTree, workspace: &'a Workspace) -> Option<&'a WindowGroup> {
    let strip = tree
        .active_strip
        .get(&workspace.id)
        .and_then(|id| workspace.strips.iter().find(|strip| strip.id == *id))
        .or_else(|| workspace.strips.first())?;
    tree.active_group_for(strip.id)
        .and_then(|id| strip.window_groups.iter().find(|group| group.id == id))
        .or_else(|| strip.window_groups.first())
}

fn focused_tab(tree: &DesktopTree, workspace: &Workspace) -> Option<TabId> {
    let group = focused_group(tree, workspace)?;
    group
        .tabs
        .iter()
        .any(|tab| tab.id == group.active_tab)
        .then_some(group.active_tab)
}

/// Where one tab may be moved or copied: the tab and the other workspaces.
struct Relocation<'a> {
    workspaces: &'a [Workspace],
    workspace: &'a Workspace,
    group: GroupId,
    tab: &'a Tab,
    /// Whether to offer the rows that ask where: in a listing, for their
    /// exact address, or when a search names the move verbs and this is the
    /// focused tab, the one tab a typed "move" is about.
    offered: bool,
}

/// The move and copy rows of one tab. A search for "move" offers one row per
/// verb, on the focused tab, that asks where ([`DESTINATION_PARAM`]); a list
/// of every tab times every workspace buries everything else. The listing
/// (an empty query, for agents) still names each destination, and each
/// destination's exact address always resolves. A workspace's own history
/// and a collaborator's window stay where they are.
fn move_rows(
    rows: &mut Vec<WindowRow>,
    admit: &impl Fn(&str, bool) -> bool,
    place: &Relocation<'_>,
) {
    let Relocation {
        workspaces,
        workspace,
        group,
        tab,
        offered,
    } = *place;
    if matches!(
        tab.tile,
        TileContent::PeerWindow { .. } | TileContent::HostedWindow { .. }
    ) {
        return;
    }
    let title = &tab.title;
    let copyable = matches!(
        tab.tile,
        TileContent::Foreign { .. } | TileContent::LocalApp { .. }
    );
    let others: Vec<&Workspace> = workspaces
        .iter()
        .filter(|other| other.id != workspace.id)
        .collect();
    if others.is_empty() {
        return;
    }
    if offered {
        let destinations = others.iter().map(|other| other.name.clone()).collect();
        chosen_destination_rows(rows, group, tab, copyable, destinations);
    }
    let detail = || {
        Some(t_args(
            "search-window-move-tab-detail",
            &[("workspace", workspace.name.as_str().into())],
        ))
    };
    for other in others {
        let id = tab_move_id(group, tab.id, other.id);
        if admit(&id, false) {
            let label = t_args(
                "search-window-move-tab",
                &[
                    ("tab", title.as_str().into()),
                    ("workspace", other.name.as_str().into()),
                ],
            );
            rows.push(WindowRow {
                kind: WindowRowKind::Tab,
                id,
                names: vec![label.clone()],
                title: label,
                subtitle: detail(),
                destinations: Vec::new(),
                hint: app_hint(&tab.tile),
                action: WindowAction::MoveTab {
                    group,
                    tab: tab.id,
                    workspace: other.id,
                },
            });
        }
        let id = tab_copy_id(group, tab.id, other.id);
        if copyable && admit(&id, false) {
            let label = t_args(
                "search-window-copy-tab",
                &[
                    ("tab", title.as_str().into()),
                    ("workspace", other.name.as_str().into()),
                ],
            );
            rows.push(WindowRow {
                kind: WindowRowKind::Tab,
                id,
                names: vec![label.clone()],
                title: label,
                subtitle: detail(),
                destinations: Vec::new(),
                hint: app_hint(&tab.tile),
                action: WindowAction::CopyTab {
                    group,
                    tab: tab.id,
                    workspace: other.id,
                },
            });
        }
    }
}

/// The "Move … to another workspace" row, and the copy row for a window
/// whose document can be copied, each asking which of `destinations`.
fn chosen_destination_rows(
    rows: &mut Vec<WindowRow>,
    group: GroupId,
    tab: &Tab,
    copyable: bool,
    destinations: Vec<String>,
) {
    let title = &tab.title;
    let label = t_args(
        "search-window-move-tab-to",
        &[("tab", title.as_str().into())],
    );
    rows.push(WindowRow {
        kind: WindowRowKind::Tab,
        id: tab_move_to_id(group, tab.id),
        names: vec![label.clone()],
        title: label,
        subtitle: None,
        destinations: destinations.clone(),
        hint: app_hint(&tab.tile),
        action: WindowAction::MoveTabTo { group, tab: tab.id },
    });
    if copyable {
        let label = t_args(
            "search-window-copy-tab-to",
            &[("tab", title.as_str().into())],
        );
        rows.push(WindowRow {
            kind: WindowRowKind::Tab,
            id: tab_copy_to_id(group, tab.id),
            names: vec![label.clone()],
            title: label,
            subtitle: None,
            destinations,
            hint: app_hint(&tab.tile),
            action: WindowAction::CopyTabTo { group, tab: tab.id },
        });
    }
}

/// The close rows for `group` and each of its tabs, admitted only once the
/// query names the verb.
fn close_rows(
    rows: &mut Vec<WindowRow>,
    admit: &impl Fn(&str, bool) -> bool,
    verbs: Verbs,
    workspace: &Workspace,
    strip: &Strip,
    group: &WindowGroup,
) {
    let group_id = group.id;
    let group_title = group
        .tabs
        .iter()
        .find(|tab| tab.id == group.active_tab)
        .or_else(|| group.tabs.first())
        .map_or_else(
            || t("search-window-group-untitled"),
            |tab| tab.title.clone(),
        );
    let id = group_close_id(group_id);
    if admit(&id, verbs.close) {
        let label = t_args(
            "search-window-close-group",
            &[("group", group_title.as_str().into())],
        );
        rows.push(WindowRow {
            kind: WindowRowKind::WindowGroup,
            id,
            names: vec![label.clone()],
            title: label,
            subtitle: Some(t_args(
                "search-window-close-group-detail",
                &[
                    ("workspace", workspace.name.as_str().into()),
                    ("strip", strip.name.as_str().into()),
                    ("count", group.tabs.len().into()),
                ],
            )),
            destinations: Vec::new(),
            hint: None,
            action: WindowAction::CloseGroup { group: group_id },
        });
    }
    for tab in &group.tabs {
        let id = tab_close_id(group_id, tab.id);
        if admit(&id, verbs.close) {
            let label = t_args(
                "search-window-close-tab",
                &[("tab", tab.title.as_str().into())],
            );
            rows.push(WindowRow {
                kind: WindowRowKind::Tab,
                id,
                names: vec![label.clone()],
                title: label,
                subtitle: Some(t_args(
                    "search-window-close-tab-detail",
                    &[
                        ("workspace", workspace.name.as_str().into()),
                        ("strip", strip.name.as_str().into()),
                    ],
                )),
                destinations: Vec::new(),
                hint: app_hint(&tab.tile),
                action: WindowAction::CloseTab {
                    group: group_id,
                    tab: tab.id,
                },
            });
        }
    }
}

/// Every window row of the active workspace that `needle` may be asking for.
///
/// Rows are admitted, not ranked: the host ranks [`WindowRow::names`]
/// against the same needle. A needle equal to a row's exact id always admits
/// that row, which is how an address resolves.
pub fn window_rows(tree: &DesktopTree, needle: &str) -> Vec<WindowRow> {
    let Some(workspace) = tree.active_workspace() else {
        return Vec::new();
    };
    let exact = needle.trim();
    let verbs = Verbs::of(needle);
    let admit = |id: &str, named: bool| named || exact == id;
    let focused = focused_tab(tree, workspace);
    // An empty listing is "windows to go back to"; a search names every
    // matching window, including the one already focused. Otherwise
    // searching for the open Terminal finds only close actions (S05).
    let searching = !exact.is_empty();
    let multi_strip = workspace.strips.len() > 1;
    let mut rows = Vec::new();
    for strip in &workspace.strips {
        for group in &strip.window_groups {
            let group_id = group.id;
            for tab in &group.tabs {
                let id = tab_focus_id(group_id, tab.id);
                if admit(&id, searching || focused != Some(tab.id)) {
                    rows.push(WindowRow {
                        kind: WindowRowKind::Tab,
                        id,
                        title: tab.title.clone(),
                        subtitle: multi_strip.then(|| {
                            t_args(
                                "search-window-focus-place-strip",
                                &[("strip", strip.name.as_str().into())],
                            )
                        }),
                        destinations: Vec::new(),
                        names: vec![tab.title.clone()],
                        hint: app_hint(&tab.tile),
                        action: WindowAction::FocusTab {
                            workspace: workspace.id,
                            strip: strip.id,
                            group: group_id,
                            tab: tab.id,
                        },
                    });
                }
            }
            close_rows(&mut rows, &admit, verbs, workspace, strip, group);
            for tab in &group.tabs {
                move_rows(
                    &mut rows,
                    &|id: &str, _| !searching || exact == id,
                    &Relocation {
                        workspaces: &tree.workspaces,
                        workspace,
                        group: group_id,
                        tab,
                        offered: !searching
                            || (verbs.relocate && focused == Some(tab.id))
                            || exact == tab_move_to_id(group_id, tab.id)
                            || exact == tab_copy_to_id(group_id, tab.id),
                    },
                );
            }
        }
    }
    // An empty listing is PALETTE-truncated. Keep the focused group's close
    // after the "go back to" focus rows so invoke and agents still see it
    // when a long session has more window actions than one page.
    if !searching && let Some(group) = focused_group(tree, workspace) {
        let close_id = group_close_id(group.id);
        if let Some(pos) = rows.iter().position(|row| row.id == close_id) {
            let row = rows.remove(pos);
            let insert_at = rows
                .iter()
                .position(|row| !row.id.starts_with("tab.focus:"))
                .unwrap_or(rows.len());
            rows.insert(insert_at, row);
        }
    }
    rows
}

/// Changes exactly when the set of window rows can change: the active
/// workspace, its groups and tabs, which tab is focused, and every
/// workspace a tab can move to.
pub fn window_rows_revision(tree: &DesktopTree) -> String {
    let Some(workspace) = tree.active_workspace() else {
        return "desktop-windows:empty".into();
    };
    let mut revision = format!("desktop-windows:{}", workspace.id.raw());
    if let Some(tab) = focused_tab(tree, workspace) {
        let _ = write!(revision, ":f{tab}");
    }
    for other in &tree.workspaces {
        let _ = write!(revision, ":w{}:{}", other.id.raw(), other.name);
    }
    for strip in &workspace.strips {
        let _ = write!(revision, ":s{}:{}", strip.id.raw(), strip.name);
        for group in &strip.window_groups {
            let _ = write!(revision, ":g{}", group.id);
            for tab in &group.tabs {
                let _ = write!(revision, ":t{}:{}", tab.id, tab.title);
            }
        }
    }
    revision
}
