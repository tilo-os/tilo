pub mod browser_stub;
pub mod nirios_tokens;
pub mod overview;
pub mod persist;
pub mod quick_settings;
pub mod search;
pub mod sidebar_wheel;
pub mod state;
pub mod system_monitor;
pub mod topbar;
pub mod workspace_settings;

use quick_ui::widget::{
    AppKind as UiKind, Column, Container, StripView, TabDescriptor,
    TileContent as UiTile, Widget, WindowGroup as UiGroup, WindowWidth as UiWidth,
    WorkspaceView,
};
use state::{AppKind as ShellKind, TileContent as ShellTile, WorkspaceTree};

fn to_ui_width(w: state::WindowWidth) -> UiWidth {
    match w {
        state::WindowWidth::Narrow => UiWidth::Narrow,
        state::WindowWidth::Normal => UiWidth::Normal,
        state::WindowWidth::Wide => UiWidth::Wide,
        state::WindowWidth::Custom(p) => UiWidth::Custom(p),
    }
}

fn to_ui_kind(k: &ShellKind) -> UiKind {
    // Note: `quick_ui::AppKind` was aligned to spec §7 variants in Plan 3
    // Task 1 (Terminal, Files, Editor, Browser, Settings). The shell's own
    // `AppKind` still carries `FileExplorer` and `Other(String)` for v1
    // compatibility; map them to the nearest UI variant.
    match k {
        ShellKind::Terminal => UiKind::Terminal,
        ShellKind::FileExplorer => UiKind::Files,
        ShellKind::Browser => UiKind::Browser,
        ShellKind::Settings => UiKind::Settings,
        ShellKind::Other(_) => UiKind::Settings,
    }
}

fn to_ui_tile(t: &ShellTile) -> UiTile {
    match t {
        ShellTile::Stub { kind, label } => UiTile::Stub {
            kind: to_ui_kind(kind),
            label: label.clone(),
        },
        ShellTile::AppPlaceholder { kind, label } => UiTile::Stub {
            kind: to_ui_kind(kind),
            label: label.clone(),
        },
    }
}

/// Build the nirios shell's root widget tree from a WorkspaceTree. v1 renders the
/// active workspace's active strip as a `StripView` overlay above the active
/// `WorkspaceView`, with placeholder window-groups laid out horizontally.
pub fn build_nirios_root(tree: &WorkspaceTree) -> Box<dyn Widget> {
    let strips_for_view: Vec<String> = tree
        .active_workspace()
        .map(|w| w.strips.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default();
    let strip_view: Box<dyn Widget> = Box::new(StripView::new(strips_for_view, 0));
    let workspace_view: Box<dyn Widget> = Box::new(WorkspaceView::new(
        tree.workspaces.len(),
        tree.workspaces
            .iter()
            .position(|w| w.id == tree.active)
            .unwrap_or(0),
    ));

    let mut col = Column::new().push(workspace_view).push(strip_view);

    if let Some(ws) = tree.active_workspace() {
        for strip in &ws.strips {
            for g in &strip.window_groups {
                let tabs_with_content: Vec<(TabDescriptor, UiTile)> = g
                    .tabs
                    .iter()
                    .map(|t| {
                        (
                            TabDescriptor {
                                id: t.id,
                                title: t.title.clone(),
                            },
                            to_ui_tile(&t.tile),
                        )
                    })
                    .collect();
                let ui_group =
                    UiGroup::new(g.id, tabs_with_content, g.active_tab, to_ui_width(g.width));
                col = col.push(Box::new(ui_group));
            }
        }
    }
    Box::new(Container::new().with_child(Box::new(col)).with_background())
}
