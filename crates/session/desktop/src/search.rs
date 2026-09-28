//! Searchable window rows over the desktop topology.
//!
//! Every tab and window group in the active workspace becomes an exactly
//! addressed row: close a group, close a tab, or move a tab to another
//! workspace. The rows are render-free data; a host ranks them with its own
//! scorer and applies the [`WindowAction`] through its tab lifecycle, so an
//! invocation never synthesises pointer input.
use std::fmt::Write as _;

use tilo_desktop_model::{DesktopTree, GroupId, Tab, TabId, TileContent, Workspace, WorkspaceId};

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
}

/// One ranked window row with its localized copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowRow {
    pub kind: WindowRowKind,
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub badge: String,
    pub score: i32,
    pub action: WindowAction,
}

/// The provider's name in the desktop's language.
pub fn windows_provider_name() -> String {
    t("search-provider-desktop-windows")
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

/// The exact address of copying `tab`'s document in `group` into `workspace`.
pub fn tab_copy_id(group: GroupId, tab: TabId, workspace: WorkspaceId) -> String {
    format!("tab.copy:{group}:{tab}:{}", workspace.raw())
}

fn app_id(tile: &TileContent) -> Option<&str> {
    match tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. } => Some(app_id),
        TileContent::AppPlaceholder { .. }
        | TileContent::Stub { .. }
        | TileContent::WorkspaceActivity => None,
    }
}

/// One "move to workspace" row per other workspace `tab` may go to, and a
/// "copy to workspace" row for a client window. A workspace's own history
/// and a collaborator's window stay where they are.
fn move_rows(
    rows: &mut Vec<WindowRow>,
    rank: &impl Fn(&[&str]) -> Option<i32>,
    workspaces: &[Workspace],
    workspace: &Workspace,
    group: GroupId,
    position: usize,
    tab: &Tab,
) {
    if matches!(
        tab.tile,
        TileContent::WorkspaceActivity | TileContent::PeerWindow { .. }
    ) {
        return;
    }
    let title = &tab.title;
    let app = app_id(&tab.tile).unwrap_or("native");
    for other in workspaces.iter().filter(|other| other.id != workspace.id) {
        let id = tab_move_id(group, tab.id, other.id);
        let Some(score) = rank(&[title, app, &workspace.name, &other.name, &id]) else {
            continue;
        };
        rows.push(WindowRow {
            kind: WindowRowKind::Tab,
            id,
            title: t_args(
                "search-window-move-tab",
                &[
                    ("tab", title.as_str().into()),
                    ("workspace", other.name.as_str().into()),
                ],
            ),
            subtitle: t_args(
                "search-window-move-tab-detail",
                &[
                    ("app", app.into()),
                    ("workspace", workspace.name.as_str().into()),
                    ("position", position.into()),
                ],
            ),
            badge: t("search-badge-tab"),
            score,
            action: WindowAction::MoveTab {
                group,
                tab: tab.id,
                workspace: other.id,
            },
        });
        if !matches!(
            tab.tile,
            TileContent::Foreign { .. } | TileContent::LocalApp { .. }
        ) {
            continue;
        }
        let id = tab_copy_id(group, tab.id, other.id);
        let Some(score) = rank(&[title, app, &workspace.name, &other.name, &id]) else {
            continue;
        };
        rows.push(WindowRow {
            kind: WindowRowKind::Tab,
            id,
            title: t_args(
                "search-window-copy-tab",
                &[
                    ("tab", title.as_str().into()),
                    ("workspace", other.name.as_str().into()),
                ],
            ),
            subtitle: t_args(
                "search-window-move-tab-detail",
                &[
                    ("app", app.into()),
                    ("workspace", workspace.name.as_str().into()),
                    ("position", position.into()),
                ],
            ),
            badge: t("search-badge-tab"),
            score,
            action: WindowAction::CopyTab {
                group,
                tab: tab.id,
                workspace: other.id,
            },
        });
    }
}

/// Every window row of the active workspace that `rank` admits.
///
/// `rank` scores a row's names (each of which names it) and returns `None`
/// for a miss; only admitted rows pay for their localized copy.
pub fn window_rows(tree: &DesktopTree, rank: impl Fn(&[&str]) -> Option<i32>) -> Vec<WindowRow> {
    let Some(workspace) = tree.active_workspace() else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for strip in &workspace.strips {
        for (index, group) in strip.window_groups.iter().enumerate() {
            let group_id = group.id;
            // Ids are 64-bit handles; people count groups from the left.
            let position = index + 1;
            let group_title = group
                .tabs
                .iter()
                .find(|tab| tab.id == group.active_tab)
                .or_else(|| group.tabs.first())
                .map_or_else(
                    || {
                        t_args(
                            "search-window-group-untitled",
                            &[("position", position.into())],
                        )
                    },
                    |tab| tab.title.clone(),
                );
            let id = group_close_id(group_id);
            if let Some(score) = rank(&[&group_title, &workspace.name, &strip.name, &id]) {
                rows.push(WindowRow {
                    kind: WindowRowKind::WindowGroup,
                    id,
                    title: t_args(
                        "search-window-close-group",
                        &[("group", group_title.as_str().into())],
                    ),
                    subtitle: t_args(
                        "search-window-close-group-detail",
                        &[
                            ("workspace", workspace.name.as_str().into()),
                            ("strip", strip.name.as_str().into()),
                            ("position", position.into()),
                            ("count", group.tabs.len().into()),
                        ],
                    ),
                    badge: t("search-badge-window-group"),
                    score,
                    action: WindowAction::CloseGroup { group: group_id },
                });
            }
            for tab in &group.tabs {
                let app = app_id(&tab.tile).unwrap_or("native");
                let id = tab_close_id(group_id, tab.id);
                if let Some(score) = rank(&[&tab.title, app, &workspace.name, &strip.name, &id]) {
                    rows.push(WindowRow {
                        kind: WindowRowKind::Tab,
                        id,
                        title: t_args(
                            "search-window-close-tab",
                            &[("tab", tab.title.as_str().into())],
                        ),
                        subtitle: t_args(
                            "search-window-close-tab-detail",
                            &[
                                ("app", app.into()),
                                ("workspace", workspace.name.as_str().into()),
                                ("strip", strip.name.as_str().into()),
                                ("position", position.into()),
                            ],
                        ),
                        badge: t("search-badge-tab"),
                        score,
                        action: WindowAction::CloseTab {
                            group: group_id,
                            tab: tab.id,
                        },
                    });
                }
                move_rows(
                    &mut rows,
                    &rank,
                    &tree.workspaces,
                    workspace,
                    group_id,
                    position,
                    tab,
                );
            }
        }
    }
    rows
}

/// Changes exactly when the set of window rows can change: the active
/// workspace, its groups and tabs, and every workspace a tab can move to.
pub fn window_rows_revision(tree: &DesktopTree) -> String {
    let Some(workspace) = tree.active_workspace() else {
        return "desktop-windows:empty".into();
    };
    let mut revision = format!("desktop-windows:{}", workspace.id.raw());
    for other in &tree.workspaces {
        let _ = write!(revision, ":w{}:{}", other.id.raw(), other.name);
    }
    for strip in &workspace.strips {
        for group in &strip.window_groups {
            let _ = write!(revision, ":g{}", group.id);
            for tab in &group.tabs {
                let _ = write!(revision, ":t{}", tab.id);
            }
        }
    }
    revision
}
