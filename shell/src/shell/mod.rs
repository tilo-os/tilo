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
    AppKind as UiKind, AppWidget, Column, Container, StripView, TabDescriptor,
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
///
/// In addition to the tree, the shell owns a single Automerge document
/// ([`quick_ui::state::State`]) shared across every hosted `AppWidget`. Each
/// app's own sub-tree under `state.root` is what the widget reads from in
/// `sync` and mutates in `handle_event` / `update`, matching the direct-
/// mutation event model `quick-ui` is built around (see `quick-ui` docs).
pub struct Shell {
    tree: RuntimeTree,
    /// CRDT document shared by every hosted `AppWidget`. In the production
    /// wiring (Plan 3 follow-up) this is the same `State` the compositor
    /// feeds into the quick-ui `App`; here we create one up-front so the
    /// test harness (and any eager `sync` call during construction) has a
    /// document to bind against.
    state: quick_ui::state::State,
    /// Test-only side channel: raw pointer to the first hosted
    /// [`TerminalWidget`], captured at construction before coercion into the
    /// `Box<dyn AppWidget>` stored in the runtime tree. Stays valid for the
    /// lifetime of the `Shell` because `Box<T>` -> `Box<dyn Trait>` is an
    /// unsizing coercion that does not move the heap allocation. Used only
    /// by the `cfg(any(test, debug_assertions))` introspection helpers
    /// below; `None` if the PTY spawn failed and the terminal tile fell
    /// back to a `Stub`.
    #[cfg(any(test, debug_assertions))]
    first_terminal_ptr: Option<*const TerminalWidget>,
    /// Test-only pointer to the first hosted [`FileExplorerWidget`]. Same
    /// allocation-stability argument as `first_terminal_ptr`. `None` when
    /// the default workspace has no file-explorer tile (e.g. future
    /// variants that omit it).
    #[cfg(any(test, debug_assertions))]
    first_file_explorer_ptr: Option<*const FileExplorerWidget>,
}

impl Shell {
    /// Construct a shell hosting the default demo workspace: three tabs in a
    /// single window-group on the `Main` strip — Terminal (real PTY via
    /// `TerminalWidget::spawn_default`, falling back to a `Stub` with the
    /// spawn error per spec §9a), Files (`FileExplorerWidget::new`), and
    /// the Browser stub (Servo not ported).
    pub fn new_with_default_demo_workspace() -> Self {
        let DemoTabs {
            tabs,
            #[cfg(any(test, debug_assertions))]
            first_terminal_ptr,
            #[cfg(any(test, debug_assertions))]
            first_file_explorer_ptr,
        } = default_demo_tabs();
        let group = RuntimeGroup::with_tabs(tabs, state::WindowWidth::Normal);
        let strip = RuntimeStrip::new("Main", vec![group]);
        let workspace = RuntimeWorkspace::new("Home", vec![strip]);
        let mut shell = Self {
            tree: RuntimeTree::new(vec![workspace]),
            state: quick_ui::state::State::new(),
            #[cfg(any(test, debug_assertions))]
            first_terminal_ptr,
            #[cfg(any(test, debug_assertions))]
            first_file_explorer_ptr,
        };
        // Prime every app against the shared doc so subsequent `update`
        // calls have their CRDT sub-trees initialised. This mirrors the
        // first-frame `sync` pass the quick-ui `App` runs before its first
        // `draw`.
        shell.sync_all_apps();
        shell
    }

    /// Borrow the runtime workspace tree. Callers iterate
    /// `tree.workspaces[..].strips[..].window_groups[..].tabs[..].tile` to
    /// introspect tile content.
    pub fn workspace_tree(&self) -> &RuntimeTree {
        &self.tree
    }

    /// Run `sync` on every hosted `AppWidget`. Extracted so both the
    /// constructor and [`Self::tick_for_test`] can share the traversal
    /// without duplicating the four nested loops.
    fn sync_all_apps(&mut self) {
        for ws in &mut self.tree.workspaces {
            for strip in &mut ws.strips {
                for group in &mut strip.window_groups {
                    for tab in &mut group.tabs {
                        if let RuntimeTile::App(app) = &mut tab.tile {
                            app.sync(&mut self.state.doc, &self.state.root);
                        }
                    }
                }
            }
        }
    }

    /// Drive every hosted `AppWidget` through one `sync` + `update` pass
    /// against the shared doc, without painting. Used by the Task 16
    /// integration test harness to advance the terminal's PTY reader thread
    /// and the CRDT without needing a GPU-backed render loop.
    ///
    /// Parallel to the five-pass pipeline in `quick-ui`'s `App`, but skips
    /// `measure` / `build_layout` / `apply_layout` / `draw` — introspection
    /// only cares about state, and the apps' `update` paths are what move
    /// bytes from the PTY reader into the CRDT grid.
    #[cfg(any(test, debug_assertions))]
    pub fn tick_for_test(&mut self) {
        for ws in &mut self.tree.workspaces {
            for strip in &mut ws.strips {
                for group in &mut strip.window_groups {
                    for tab in &mut group.tabs {
                        if let RuntimeTile::App(app) = &mut tab.tile {
                            app.sync(&mut self.state.doc, &self.state.root);
                            let _ = app.update(&mut self.state.doc, &self.state.root);
                        }
                    }
                }
            }
        }
    }

    /// Read the first row of the first hosted terminal's grid. Returns
    /// `None` if no live terminal tile exists (e.g. PTY spawn failed and
    /// the tile fell back to a `Stub`).
    ///
    /// Implementation note: the `TerminalWidget::grid` field is private to
    /// `quick-terminal`, but every byte the PTY produces is also mirrored
    /// into the shared Automerge doc under `terminal_grid` (a `List` of
    /// `Map`s each carrying a `text` field — see `TerminalPerformer`).
    /// Since the shell drives `sync` + `update` through its own doc, we can
    /// read the first line back from the CRDT without touching the app
    /// crate's internals.
    #[cfg(any(test, debug_assertions))]
    pub fn first_terminal_line(&self) -> Option<String> {
        use automerge::{ObjType, ReadDoc, Value};

        // Gate on presence of a live terminal tile; if spawn failed, the
        // tile is a `Stub` and the caller should treat that as a skip.
        self.first_terminal_ptr?;

        let doc = &self.state.doc;
        let (grid_val, grid_id) = doc.get(&self.state.root, "terminal_grid").ok().flatten()?;
        if !matches!(grid_val, Value::Object(ObjType::List)) {
            return None;
        }
        if doc.length(&grid_id) == 0 {
            return Some(String::new());
        }
        let (line_val, line_id) = doc.get(&grid_id, 0).ok().flatten()?;
        if !matches!(line_val, Value::Object(ObjType::Map)) {
            return None;
        }
        let (text_val, text_id) = doc.get(&line_id, "text").ok().flatten()?;
        if !matches!(text_val, Value::Object(ObjType::Text)) {
            return None;
        }
        doc.text(&text_id).ok()
    }

    /// Read the entry-name list of the first hosted file-explorer. Returns
    /// an empty vec if there is no file-explorer tile or if its initial
    /// directory read came back empty (e.g. a pristine sandbox).
    ///
    /// Unlike the terminal, the file-explorer keeps its listing in local
    /// widget state rather than the CRDT, so we reach through the raw
    /// pointer captured at construction. Safety: the pointer targets the
    /// heap allocation of the `Box<FileExplorerWidget>` that was coerced
    /// into `Box<dyn AppWidget>` and now lives inside the runtime tree. A
    /// `Box<T>` -> `Box<dyn Trait>` unsizing coercion keeps the same heap
    /// address, and the `Shell` owns the tree for its entire lifetime, so
    /// the pointer remains valid as long as `&self` does.
    #[cfg(any(test, debug_assertions))]
    pub fn first_file_explorer_entries(&self) -> Vec<String> {
        let Some(ptr) = self.first_file_explorer_ptr else {
            return Vec::new();
        };
        // SAFETY: see the doc-comment above; pointer is live for `&self`.
        let fe: &FileExplorerWidget = unsafe { &*ptr };
        fe.entries().iter().map(|e| e.name.clone()).collect()
    }
}

/// Builder output for the default demo workspace. Carries the tab list plus
/// test-only raw pointers to the concrete `TerminalWidget` /
/// `FileExplorerWidget` allocations that live inside two of those tabs, so
/// the `Shell` can expose introspection helpers without having to downcast
/// through `Box<dyn AppWidget>` (which is not `Any`).
struct DemoTabs {
    tabs: Vec<RuntimeTab>,
    #[cfg(any(test, debug_assertions))]
    first_terminal_ptr: Option<*const TerminalWidget>,
    #[cfg(any(test, debug_assertions))]
    first_file_explorer_ptr: Option<*const FileExplorerWidget>,
}

/// Build the default demo workspace's tab list. Extracted so future tasks
/// (e.g. adding an editor or settings tab) can reuse the fallback-to-Stub
/// pattern without duplicating `TerminalWidget::spawn_default`'s error path.
fn default_demo_tabs() -> DemoTabs {
    let mut tabs: Vec<RuntimeTab> = Vec::new();
    #[cfg(any(test, debug_assertions))]
    let mut first_terminal_ptr: Option<*const TerminalWidget> = None;
    // `first_file_explorer_ptr` is written unconditionally once we construct
    // the file-explorer tab below, but declared up here so the final struct
    // literal can reference it without a local scope leak.
    #[cfg(any(test, debug_assertions))]
    let first_file_explorer_ptr: Option<*const FileExplorerWidget>;

    // Terminal — spawn PTY; on failure fall back to a Stub per spec §9a.
    match TerminalWidget::spawn_default() {
        Ok(t) => {
            let boxed: Box<TerminalWidget> = Box::new(t);
            #[cfg(any(test, debug_assertions))]
            {
                // Capture the pointer to the heap allocation *before*
                // coercing the `Box<TerminalWidget>` into
                // `Box<dyn AppWidget>`. The unsizing coercion preserves the
                // same address, so this pointer stays valid for the life
                // of the owning `Box` inside the runtime tree.
                first_terminal_ptr = Some(&*boxed as *const TerminalWidget);
            }
            let app: Box<dyn AppWidget> = boxed;
            tabs.push(RuntimeTab::new("Terminal", RuntimeTile::App(app)));
        }
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
    let fe_boxed: Box<FileExplorerWidget> = Box::new(FileExplorerWidget::new());
    #[cfg(any(test, debug_assertions))]
    {
        first_file_explorer_ptr = Some(&*fe_boxed as *const FileExplorerWidget);
    }
    let fe_app: Box<dyn AppWidget> = fe_boxed;
    tabs.push(RuntimeTab::new("Files", RuntimeTile::App(fe_app)));

    // Browser — explicit stub (Servo not ported; spec §10.6).
    tabs.push(RuntimeTab::new(
        "Browser",
        RuntimeTile::App(Box::new(BrowserStubWidget::new())),
    ));

    DemoTabs {
        tabs,
        #[cfg(any(test, debug_assertions))]
        first_terminal_ptr,
        #[cfg(any(test, debug_assertions))]
        first_file_explorer_ptr,
    }
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
