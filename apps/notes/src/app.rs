//! Root widget for the notes binary. Owns the in-memory [`Workspace`],
//! the storage layer, and the two pane widgets ([`NoteTreeView`] +
//! [`MainPanel`]). Coordinates the data flow:
//!
//! - sidebar emits [`OutEvent`]s when the user clicks / renames /
//!   creates / deletes,
//! - the app applies them to the workspace,
//! - the workspace is then pushed back into the sidebar (so the new
//!   tree shape reflects), the main panel (title update), and the
//!   storage saver (debounced disk persistence).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::Widget;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

use crate::main_panel::MainPanel;
use crate::model::Workspace;
use crate::sidebar::{NoteTreeView, OutEvent};
use crate::storage::{self, PageSaver, Saver};

/// Fixed sidebar width for Phase 1 (we ship a collapsible drawer in
/// a later phase — see the roadmap). 240 px reads as "comfortable but
/// not greedy" against the dashboard widgets in the existing apps.
const SIDEBAR_WIDTH: f64 = 240.0;
/// Debounce window for the background save thread. Long enough that
/// a burst of keystrokes during inline rename coalesces, short enough
/// that the user's next "is this saved yet" sanity check doesn't have
/// to wait.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

pub struct NotesApp {
    workspace: Workspace,
    sidebar: NoteTreeView,
    main: MainPanel,
    /// Root container rect, populated by `apply_layout`.
    rect: Rect,
    /// Cached taffy node IDs for the two children. Filled in
    /// `build_layout`, used by `apply_layout` and `handle_event`.
    sidebar_node: Option<NodeId>,
    main_node: Option<NodeId>,
    root: PathBuf,
    saver: Option<Saver>,
    /// Debounced background saver for per-page bodies. Receives a
    /// new snapshot whenever the main panel's `BlockEditor` reports
    /// a dirty edit; coalesces multiple edits in `SAVE_DEBOUNCE`.
    page_saver: Option<PageSaver>,
}

impl NotesApp {
    /// Load (or create) the workspace under `root` and wire up the
    /// debounced background saver. Returns a fully-initialised widget
    /// ready to be handed to `ui::App`.
    pub fn spawn(root: PathBuf) -> Result<Self, String> {
        storage::ensure_layout(&root).map_err(|e| e.to_string())?;
        let mut workspace = storage::load_workspace(&root)?.unwrap_or_default();
        if workspace.pages().is_empty() {
            seed_welcome(&root, &mut workspace);
            storage::save_workspace(&root, &workspace).map_err(|e| e.to_string())?;
        }

        let mut sidebar = NoteTreeView::new();
        sidebar.sync_from_workspace(&workspace);
        let mut main = MainPanel::new();
        if let Some(sel) = workspace.selected() {
            if let Some(meta) = workspace.get(sel) {
                let body = storage::read_page_body(&root, sel).unwrap_or_default();
                main.load_page(sel, meta.title.clone(), body);
            }
        } else {
            // Auto-select the first root page on launch so the panel
            // isn't empty out of the gate. Clone the picked metadata
            // so we drop the immutable borrow before mutating.
            let first = workspace
                .children_of(None)
                .first()
                .map(|p| (p.id, p.title.clone()));
            if let Some((id, title)) = first {
                workspace.set_selected(Some(id));
                let body = storage::read_page_body(&root, id).unwrap_or_default();
                main.load_page(id, title, body);
                sidebar.sync_from_workspace(&workspace);
            }
        }

        let saver = Saver::spawn(root.clone(), SAVE_DEBOUNCE);
        let page_saver = PageSaver::spawn(root.clone(), SAVE_DEBOUNCE);

        Ok(Self {
            workspace,
            sidebar,
            main,
            rect: Rect::ZERO,
            sidebar_node: None,
            main_node: None,
            root,
            saver: Some(saver),
            page_saver: Some(page_saver),
        })
    }

    /// Default constructor — resolves the workspace root via `dirs`,
    /// falls back to `./notes` if the home dir is unavailable.
    pub fn spawn_default() -> Result<Self, String> {
        let root = storage::default_root()
            .unwrap_or_else(|| PathBuf::from("./notes"));
        Self::spawn(root)
    }

    /// Apply one [`OutEvent`] from the sidebar to the workspace + disk.
    fn apply_event(&mut self, ev: OutEvent) {
        match ev {
            OutEvent::Select(id) => {
                // Flush any pending edits to the previously-selected
                // page before swapping. Otherwise a quick click from
                // page A to page B could lose A's most recent edit
                // if the debounce window hadn't elapsed yet.
                self.flush_main_panel_edit();
                self.workspace.set_selected(Some(id));
                if let Some(meta) = self.workspace.get(id) {
                    let title = meta.title.clone();
                    let body = storage::read_page_body(&self.root, id).unwrap_or_default();
                    self.main.load_page(id, title, body);
                }
            }
            OutEvent::Rename { id, new_title } => {
                self.workspace.rename(id, &new_title);
                if self.workspace.selected() == Some(id) {
                    self.main.set_title(Some(new_title));
                }
            }
            OutEvent::Delete(id) => {
                let removed = self.workspace.delete_page(id);
                for rid in &removed {
                    if let Err(e) = storage::delete_page_file(&self.root, *rid) {
                        tracing::error!(error = %e, page = %rid, "notes: delete page file failed");
                    }
                }
                // If the selection was nuked, pick a reasonable fallback.
                if self.workspace.selected().is_none() {
                    let first = self
                        .workspace
                        .children_of(None)
                        .first()
                        .map(|p| (p.id, p.title.clone()));
                    if let Some((id, title)) = first {
                        self.workspace.set_selected(Some(id));
                        let body =
                            storage::read_page_body(&self.root, id).unwrap_or_default();
                        self.main.load_page(id, title, body);
                    } else {
                        self.main.clear();
                    }
                }
            }
            OutEvent::CreateChild { parent } => {
                self.flush_main_panel_edit();
                let id = self.workspace.create_page(parent);
                self.workspace.set_selected(Some(id));
                if let Err(e) = storage::write_page_body(&self.root, id, "") {
                    tracing::error!(error = %e, page = %id, "notes: write empty page failed");
                }
                self.main.load_page(id, "Untitled".to_string(), String::new());
            }
        }
        self.persist();
    }

    /// If the editor has pending edits for the current page, snapshot
    /// the body and request a save before swapping pages.
    fn flush_main_panel_edit(&mut self) {
        if let (Some(id), true) = (self.main.page_id(), self.main.editor_is_dirty()) {
            let body = self.main.take_body_snapshot();
            if let Some(saver) = &self.page_saver {
                saver.request(id, body);
                // Force the worker to flush immediately so the next
                // load_page (for a different page) doesn't race with
                // the still-queued save of this page.
                saver.flush();
            }
        }
    }

    /// Poll-style: called once per tick after a `handle_event` to
    /// detect editor edits and request a debounced save. Triggers a
    /// workspace mtime bump so the sidebar can highlight "modified"
    /// state if we ever add that affordance.
    fn drain_editor_edits(&mut self) {
        if !self.main.editor_is_dirty() {
            return;
        }
        let Some(id) = self.main.page_id() else { return };
        let body = self.main.take_body_snapshot();
        if let Some(saver) = &self.page_saver {
            saver.request(id, body);
        }
    }

    /// Schedule a debounced save of the current workspace.
    fn persist(&self) {
        if let Some(s) = &self.saver {
            s.request(self.workspace.clone());
        }
    }

    /// Drain the sidebar's outbound event queue and apply each.
    /// Called after every `handle_event` call.
    fn drain_sidebar_events(&mut self) {
        let events = self.sidebar.take_events();
        if events.is_empty() {
            return;
        }
        for ev in events {
            self.apply_event(ev);
        }
        // After mutation, push the new workspace snapshot back into
        // the sidebar so the tree shape reflects.
        self.sidebar.sync_from_workspace(&self.workspace);
    }

    /// Force-flush the background saver. Called from main on
    /// shutdown so the user's last edits never get lost in the
    /// debounce window.
    pub fn flush_saves(&mut self) {
        // Drain any pending body before we ask the savers to flush.
        self.flush_main_panel_edit();
        if let Some(s) = &self.saver {
            s.flush();
        }
        if let Some(s) = &self.page_saver {
            s.flush();
        }
    }

    /// Test accessor: snapshot of the current page list. Kept out of
    /// the public surface so external callers can't poke at internal
    /// model state — `cfg(any)` since both the integration tests in
    /// `tests/` and the unit tests need it.
    #[doc(hidden)]
    pub fn workspace_pages_for_test(&self) -> Vec<crate::model::PageMeta> {
        self.workspace.pages().to_vec()
    }

    /// Test hook: mutable access to the main panel. Lets integration
    /// tests drive the editor programmatically without dispatching
    /// fake winit events.
    #[doc(hidden)]
    pub fn main_panel_mut(&mut self) -> &mut MainPanel {
        &mut self.main
    }

    /// Test hook: run one tick's worth of the edit-drain logic so
    /// tests don't have to actually go through `handle_event`. After
    /// this returns, any dirty edit is queued on the page saver.
    #[doc(hidden)]
    pub fn tick_for_test(&mut self) {
        self.drain_editor_edits();
    }
}

/// First-launch seed: one page titled "Welcome" with a small
/// example of every supported block kind so the user sees what the
/// notes app is capable of immediately. The body must round-trip
/// through `notes::markdown::{parse,serialize}` byte-stably —
/// `welcome_page_renders_all_block_kinds` pins that.
pub const WELCOME_BODY: &str = "\
# Welcome to notes

This is a paragraph — start typing anywhere.

## Block types

- Bullet list

  - Nested with Tab / Shift-Tab

- [ ] Open todo

- [x] Done todo

```rust
fn hello() { println!(\"slash to open the menu\"); }
```

---

### Tips

- Type `/` to open the block menu

- Press `Enter` inside a list to continue it

- `Tab` indents, `Shift-Tab` outdents
";

fn seed_welcome(root: &std::path::Path, workspace: &mut Workspace) {
    let id = workspace.create_page(None);
    workspace.rename(id, "Welcome");
    workspace.set_selected(Some(id));
    if let Err(e) = storage::write_page_body(root, id, WELCOME_BODY) {
        tracing::error!(error = %e, "notes: write Welcome page failed");
    }
}

impl Widget for NotesApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.sidebar.sync(state, path);
        self.main.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let sidebar_node = self.sidebar.build_layout(taffy, theme, node_map);
        // Wrap the sidebar in a fixed-width container so the split is
        // deterministic (no flexbox squeezing).
        let _ = taffy.set_style(
            sidebar_node,
            Style {
                size: Size {
                    width: Dimension::length(SIDEBAR_WIDTH as f32),
                    height: Dimension::percent(1.0),
                },
                min_size: Size {
                    width: Dimension::length(160.0),
                    height: Dimension::auto(),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
        );
        self.sidebar_node = Some(sidebar_node);

        let main_node = self.main.build_layout(taffy, theme, node_map);
        // Augment the panel's own style (preserve its flex-column
        // layout) with the parent-driven sizing. `taffy.style(...)`
        // returns the existing Style, which we clone then overwrite
        // only the fields we care about — preserves the panel's
        // internal `flex_direction: Column`, padding, etc.
        if let Ok(existing) = taffy.style(main_node).cloned() {
            let _ = taffy.set_style(
                main_node,
                Style {
                    size: Size {
                        width: Dimension::auto(),
                        height: Dimension::percent(1.0),
                    },
                    flex_grow: 1.0,
                    flex_basis: Dimension::length(0.0),
                    ..existing
                },
            );
        }
        self.main_node = Some(main_node);

        let root = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    gap: taffy::Size {
                        width: length(0.0),
                        height: length(0.0),
                    },
                    ..Default::default()
                },
                &[sidebar_node, main_node],
            )
            .unwrap();
        node_map.insert(root, self as *mut dyn Widget);
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        if let Some(n) = self.sidebar_node {
            self.sidebar.apply_layout(taffy, n, new_origin);
        }
        if let Some(n) = self.main_node {
            self.main.apply_layout(taffy, n, new_origin);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Background of the whole window.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        self.sidebar.draw(scene, transform, text_renderer, theme, presence);
        self.main.draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(n) = self.sidebar_node {
            self.sidebar.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
        }
        // Drain any commands the sidebar produced.
        self.drain_sidebar_events();
        if let Some(n) = self.main_node {
            self.main.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
        }
        // After every event, observe the editor's dirty flag and
        // request a debounced save if the user just edited.
        self.drain_editor_edits();
    }

    fn name(&self) -> &str {
        "NotesApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.sidebar.needs_redraw() || self.main.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.sidebar.mark_drawn();
        self.main.mark_drawn();
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        // The root rect is the whole window — always paint, never cull.
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn spawn_seeds_welcome_on_empty_root() {
        let tmp = TempDir::new().unwrap();
        let app = NotesApp::spawn(tmp.path().into()).unwrap();
        assert_eq!(app.workspace.pages().len(), 1);
        assert_eq!(app.workspace.pages()[0].title, "Welcome");
        // Welcome page file exists on disk.
        let id = app.workspace.pages()[0].id;
        assert!(storage::page_file_path(tmp.path(), id).exists());
    }

    #[test]
    fn apply_event_create_child_appends_and_selects() {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
        let root = app.workspace.pages()[0].id;
        app.apply_event(OutEvent::CreateChild { parent: Some(root) });
        let children = app.workspace.children_of(Some(root));
        assert_eq!(children.len(), 1);
        assert_eq!(app.workspace.selected(), Some(children[0].id));
    }

    #[test]
    fn apply_event_delete_removes_file_and_resets_selection() {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
        let root = app.workspace.pages()[0].id;
        app.apply_event(OutEvent::CreateChild { parent: Some(root) });
        let child = app.workspace.selected().unwrap();
        let child_path = storage::page_file_path(tmp.path(), child);
        assert!(child_path.exists());
        app.apply_event(OutEvent::Delete(child));
        assert!(!child_path.exists());
        // Selection falls back to a remaining root page.
        assert_eq!(app.workspace.selected(), Some(root));
    }

    #[test]
    fn rename_updates_main_panel_title() {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn(tmp.path().into()).unwrap();
        let root = app.workspace.pages()[0].id;
        app.apply_event(OutEvent::Rename {
            id: root,
            new_title: "Index".into(),
        });
        assert_eq!(app.main.title(), Some("Index"));
    }
}
