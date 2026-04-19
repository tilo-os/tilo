pub mod browser_stub;
pub mod nirios_tokens;
pub mod overview;
pub mod persist;
pub mod quick_settings;
pub mod remote_cursors;
pub mod search;
pub mod sidebar_wheel;
pub mod state;
pub mod system_monitor;
pub mod tile;
pub mod topbar;
pub mod workspace_settings;

use quick_ui::widget::{
    AppKind as UiKind, Column, Container, StripView, TabDescriptor,
    TileContent as UiTile, Widget, WindowGroup as UiGroup, WindowWidth as UiWidth,
    WorkspaceView,
};
use state::{AppKind as ShellKind, TileContent as ShellTile, WorkspaceTree};

use crate::shell::browser_stub::BrowserStubWidget;
use crate::shell::tile::{
    Strip as RuntimeStrip, Tab as RuntimeTab, TileContent as RuntimeTile,
    WindowGroup as RuntimeGroup, Workspace as RuntimeWorkspace,
    WorkspaceTree as RuntimeTree,
};
use file_explorer::FileExplorerWidget;
use quick_terminal::TerminalWidget;

/// Root nirios shell handle. Owns the runtime workspace tree — the live
/// counterpart to the persisted [`state::WorkspaceTree`], carrying concrete
/// `Box<dyn AppWidget>` instances in [`tile::TileContent::App`] rather than
/// the persisted tree's symbolic `AppPlaceholder` placeholders.
///
/// Introduced in Plan 3 Task 15 so the `apps_integration` test harness (and
/// future chrome-level logic that mutates tabs, handles drops, etc.) has a
/// single, owned root to operate on. The existing `build_nirios_root` entry
/// point continues to render from the serializable `WorkspaceTree` until a
/// follow-up task rewires it onto the runtime tree.
pub struct Shell {
    tree: RuntimeTree,
}

impl Shell {
    /// Construct a shell hosting the default demo workspace: three tabs in a
    /// single window-group on the `Main` strip — Terminal (real PTY via
    /// `TerminalWidget::spawn_default`, falling back to a `Stub` with the
    /// spawn error per spec §9a), Files (`FileExplorerWidget::new`), and
    /// the Browser stub (Servo not ported).
    pub fn new_with_default_demo_workspace() -> Self {
        let tabs = default_demo_tabs();
        let group = RuntimeGroup::with_tabs(tabs, state::WindowWidth::Normal);
        let strip = RuntimeStrip::new("Main", vec![group]);
        let workspace = RuntimeWorkspace::new("Home", vec![strip]);
        Self {
            tree: RuntimeTree::new(vec![workspace]),
        }
    }

    /// Borrow the runtime workspace tree. Callers iterate
    /// `tree.workspaces[..].strips[..].window_groups[..].tabs[..].tile` to
    /// introspect tile content.
    pub fn workspace_tree(&self) -> &RuntimeTree {
        &self.tree
    }
}

/// Build the default demo workspace's tab list. Extracted so future tasks
/// (e.g. adding an editor or settings tab) can reuse the fallback-to-Stub
/// pattern without duplicating `TerminalWidget::spawn_default`'s error path.
fn default_demo_tabs() -> Vec<RuntimeTab> {
    let mut tabs: Vec<RuntimeTab> = Vec::new();

    // Terminal — spawn PTY; on failure fall back to a Stub per spec §9a.
    match TerminalWidget::spawn_default() {
        Ok(t) => tabs.push(RuntimeTab::new("Terminal", RuntimeTile::App(Box::new(t)))),
        Err(e) => {
            tracing::warn!("terminal spawn failed: {e}; falling back to stub");
            tabs.push(RuntimeTab::new(
                "Terminal",
                RuntimeTile::Stub {
                    kind: UiKind::Terminal,
                    label: format!("Terminal: {e}"),
                },
            ));
        }
    }

    // File-explorer — `new` is infallible; surface OS errors via the
    // widget's own in-pane status bar rather than the Stub fallback.
    tabs.push(RuntimeTab::new(
        "Files",
        RuntimeTile::App(Box::new(FileExplorerWidget::new())),
    ));

    // Browser — explicit stub (Servo not ported; spec §10.6).
    tabs.push(RuntimeTab::new(
        "Browser",
        RuntimeTile::App(Box::new(BrowserStubWidget::new())),
    ));

    tabs
}

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
