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
use ui::widget::link_popover::PageEntry;
use ui::widget::page_palette::PageJumpPalette;

use crate::doc::NotesDoc;
use crate::main_panel::MainPanel;
use crate::model::{PageId, Workspace};
use crate::shortcuts_overlay::ShortcutsOverlay;
use crate::sidebar::{NoteTreeView, OutEvent};
use crate::status_bar::{count_words, StatusBar, StatusBarState};
use crate::storage::{self, PageSaver, SaveStatus, Saver};

/// Fixed sidebar width for Phase 1 (we ship a collapsible drawer in
/// a later phase — see the roadmap). 240 px reads as "comfortable but
/// not greedy" against the dashboard widgets in the existing apps.
const SIDEBAR_WIDTH: f64 = 240.0;
/// Debounce window for the background save thread. Long enough that
/// a burst of keystrokes during inline rename coalesces, short enough
/// that the user's next "is this saved yet" sanity check doesn't have
/// to wait.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// How long a freshly-started instance waits before seeding its own
/// welcome page into the shared document. This gives an already-running
/// peer's full-state reply (the late-join handshake) time to arrive, so
/// a second instance adopts the first's workspace instead of creating a
/// duplicate. A lone instance simply seeds after the window elapses.
const SEED_GRACE: Duration = Duration::from_millis(800);

pub struct NotesApp {
    workspace: Workspace,
    sidebar: NoteTreeView,
    main: MainPanel,
    status: StatusBar,
    shortcuts: ShortcutsOverlay,
    /// Root container rect, populated by `apply_layout`.
    rect: Rect,
    /// Cached taffy node IDs. Filled in `build_layout`, used by
    /// `apply_layout` and `handle_event`.
    sidebar_node: Option<NodeId>,
    main_node: Option<NodeId>,
    status_node: Option<NodeId>,
    /// Rect covering [sidebar + main] — the upper "content row" of
    /// the column layout, sibling of the status bar. We don't expose
    /// this through taffy node ids; it's a flex sub-container whose
    /// rect we never need outside of layout. Tracked locally for
    /// completeness.
    content_node: Option<NodeId>,
    root: PathBuf,
    saver: Option<Saver>,
    /// Debounced background saver for per-page bodies. Receives a
    /// new snapshot whenever the main panel's `BlockEditor` reports
    /// a dirty edit; coalesces multiple edits in `SAVE_DEBOUNCE`.
    page_saver: Option<PageSaver>,
    /// Cmd-K / Ctrl-K page jump palette. Always present; visibility
    /// is toggled by the keystroke handler below.
    palette: PageJumpPalette,
    /// Last mouse position in logical coords. Used to hit-test
    /// link marks on mouse click without dragging the click event
    /// through every nested widget.
    last_mouse: Point,
    /// Wall-clock start, used for the [`SEED_GRACE`] seed window.
    started: std::time::Instant,
    /// True once we've taken on shared document state — either by
    /// adopting a peer's or by seeding our own. Gates the one-time
    /// discard-local-welcome-on-adopt.
    adopted: bool,
    /// True once our page tree has been mirrored into the shared doc.
    seeded: bool,
    /// The page the MainPanel editor is currently bound to in the doc.
    /// `sync` rebinds whenever this drifts from the selected page.
    bound_page: Option<PageId>,
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
        // Build a title→id map for any [[Title]] link marks parsed
        // out of the initial page body.
        let by_title: std::collections::HashMap<String, PageId> = workspace
            .pages()
            .iter()
            .map(|p| (p.title.clone(), p.id))
            .collect();
        if let Some(sel) = workspace.selected() {
            if let Some(meta) = workspace.get(sel) {
                let body = storage::read_page_body(&root, sel).unwrap_or_default();
                main.load_page_with_link_resolver(sel, meta.title.clone(), body, |t| {
                    by_title.get(t).copied()
                });
            }
        } else {
            let first = workspace
                .children_of(None)
                .first()
                .map(|p| (p.id, p.title.clone()));
            if let Some((id, title)) = first {
                workspace.set_selected(Some(id));
                let body = storage::read_page_body(&root, id).unwrap_or_default();
                main.load_page_with_link_resolver(id, title, body, |t| {
                    by_title.get(t).copied()
                });
                sidebar.sync_from_workspace(&workspace);
            }
        }

        let saver = Saver::spawn(root.clone(), SAVE_DEBOUNCE);
        let page_saver = PageSaver::spawn(root.clone(), SAVE_DEBOUNCE);

        let mut app = Self {
            workspace,
            sidebar,
            main,
            status: StatusBar::new(),
            shortcuts: ShortcutsOverlay::new(),
            rect: Rect::ZERO,
            sidebar_node: None,
            main_node: None,
            status_node: None,
            content_node: None,
            root,
            saver: Some(saver),
            page_saver: Some(page_saver),
            palette: PageJumpPalette::new(),
            last_mouse: Point::ZERO,
            started: std::time::Instant::now(),
            adopted: false,
            seeded: false,
            bound_page: None,
        };
        app.refresh_link_pages();
        app.refresh_status();
        Ok(app)
    }

    /// Recompute the status bar snapshot from the workspace + savers.
    /// Cheap — called from `handle_event`'s tail and after every
    /// `apply_event`. The status bar diffs internally so a no-op
    /// refresh doesn't dirty its render.
    fn refresh_status(&mut self) {
        let path = match self.workspace.selected() {
            Some(id) => self.breadcrumb_for(id),
            None => String::new(),
        };
        // Count words from the live editor (not the on-disk body) so
        // the indicator stays in sync with what the user is typing.
        // We count over each block's plain text body — not the
        // serialised markdown — so chrome like `- `, `**`, `` ` ``
        // doesn't inflate the total.
        let word_count = if self.workspace.selected().is_some() {
            let mut total = 0usize;
            for b in self.main.editor().blocks() {
                total += count_words(b.text());
            }
            Some(total)
        } else {
            None
        };
        // Combined save status: worst-of(workspace, page bodies). If
        // either is in-flight the user sees "Saving…"; either failure
        // surfaces "Save failed".
        let workspace_status = self
            .saver
            .as_ref()
            .map(|s| s.status())
            .unwrap_or(SaveStatus::Saved);
        let page_status = self
            .page_saver
            .as_ref()
            .map(|s| s.status())
            .unwrap_or(SaveStatus::Saved);
        let save_status = match (workspace_status, page_status) {
            (SaveStatus::Failed, _) | (_, SaveStatus::Failed) => SaveStatus::Failed,
            (SaveStatus::Saving, _) | (_, SaveStatus::Saving) => SaveStatus::Saving,
            _ => SaveStatus::Saved,
        };
        self.status.set_state(StatusBarState {
            save_status,
            page_path: path,
            page_count: self.workspace.pages().len(),
            word_count,
        });
    }

    /// Resolve a page's breadcrumb path by walking parent pointers.
    /// Format: `Root / Mid / Leaf`. Caps recursion at 16 for safety.
    fn breadcrumb_for(&self, id: PageId) -> String {
        let mut chain: Vec<String> = Vec::new();
        let mut cur = Some(id);
        let mut hops = 0;
        while let Some(pid) = cur {
            if hops > 16 {
                break;
            }
            match self.workspace.get(pid) {
                Some(meta) => {
                    chain.push(meta.title.clone());
                    cur = meta.parent;
                }
                None => break,
            }
            hops += 1;
        }
        chain.reverse();
        chain.join(" / ")
    }

    /// Sync the page list into both the editor's [[link popover]] and
    /// the Cmd-K palette so they always see the latest set of titles.
    fn refresh_link_pages(&mut self) {
        let entries: Vec<PageEntry> = self
            .workspace
            .pages()
            .iter()
            .map(|p| PageEntry { id: p.id, title: p.title.clone() })
            .collect();
        self.main.editor_mut().set_link_pages(entries.clone());
        self.palette.set_items(entries);
    }

    /// Test accessor: the currently selected page id.
    #[doc(hidden)]
    pub fn selected_for_test(&self) -> Option<PageId> {
        self.workspace.selected()
    }

    /// Test accessor: handle to the palette so tests can drive
    /// open / close / typing programmatically.
    #[doc(hidden)]
    pub fn palette_mut(&mut self) -> &mut PageJumpPalette {
        &mut self.palette
    }

    /// Test accessor: handle to the shortcuts overlay.
    #[doc(hidden)]
    pub fn shortcuts_mut(&mut self) -> &mut ShortcutsOverlay {
        &mut self.shortcuts
    }

    /// Hit-test the cached last-mouse-position against every link
    /// span in every block. Returns the page id of the first link
    /// covering that pixel, or `None` if the click missed.
    fn link_under_last_mouse(&self) -> Option<PageId> {
        let p = self.last_mouse;
        for b in self.main.editor().blocks() {
            let rect = b.line.get_rect();
            if !rect.contains(p) {
                continue;
            }
            // The pixel-precise byte lookup uses the inner
            // TextInput's per-char advance cache. We need a theme to
            // resolve padding; conservatively use the dark theme
            // (the editor cells render in plain-chrome mode where
            // padding_x is zero, so this is a no-op anyway).
            let theme = ui::theme::Theme::dark();
            let byte = b.line.byte_for_pixel_x(&theme, p.x);
            if let Some(id) = ui::widget::rich_text_line::link_at_byte(b.line.marks(), byte) {
                return Some(id);
            }
        }
        None
    }

    /// Default constructor — resolves the workspace root via `dirs`,
    /// falls back to `./notes` if the home dir is unavailable.
    pub fn spawn_default() -> Result<Self, String> {
        let root = storage::default_root()
            .unwrap_or_else(|| PathBuf::from("./notes"));
        Self::spawn(root)
    }

    /// Reconcile our in-memory page tree with the shared Automerge
    /// document. Seeds the doc from our welcome page when we're the
    /// origin, adopts a peer's tree when we join late, and keeps both in
    /// step thereafter. Runs every frame from [`sync`].
    fn sync_workspace_tree(&mut self, state: &mut AutoCommit) {
        let doc_pages = NotesDoc::list_pages(state);
        if !doc_pages.is_empty() {
            if !self.adopted {
                // First shared state we've seen — adopt it wholesale,
                // discarding our locally-seeded welcome so we don't push
                // a duplicate page tree.
                self.workspace.data.pages = doc_pages;
                if let Some(s) = self.workspace.selected() {
                    if self.workspace.get(s).is_none() {
                        self.workspace.set_selected(None);
                    }
                }
                self.bound_page = None;
                self.main.clear();
                self.adopted = true;
                self.seeded = true;
                self.sidebar.sync_from_workspace(&self.workspace);
            } else {
                // Already sharing: pull any new remote pages and push our
                // local ones (idempotent upsert propagates local
                // creates / renames).
                let local: std::collections::HashSet<PageId> =
                    self.workspace.pages().iter().map(|p| p.id).collect();
                let mut changed = false;
                for dp in &doc_pages {
                    if !local.contains(&dp.id) {
                        self.workspace.data.pages.push(dp.clone());
                        changed = true;
                    }
                }
                for p in self.workspace.pages().to_vec() {
                    NotesDoc::upsert_page_meta(state, &p);
                }
                if changed {
                    self.sidebar.sync_from_workspace(&self.workspace);
                }
            }
        } else if !self.seeded && self.started.elapsed() > SEED_GRACE {
            // Doc still empty after the grace window — we're the origin.
            for p in self.workspace.pages().to_vec() {
                NotesDoc::upsert_page_meta(state, &p);
            }
            self.seeded = true;
            self.adopted = true;
        }
    }

    /// Ensure a page is selected and the MainPanel editor is bound to
    /// that page's `blocks` List in the shared doc, so its text syncs.
    fn ensure_editor_bound(&mut self, state: &mut AutoCommit) {
        if self.workspace.selected().is_none() {
            if let Some(first) =
                self.workspace.children_of(None).first().map(|p| p.id)
            {
                self.workspace.set_selected(Some(first));
            }
        }
        let Some(sel) = self.workspace.selected() else {
            if self.bound_page.is_some() {
                self.main.clear();
                self.bound_page = None;
            }
            return;
        };
        if self.bound_page != Some(sel) {
            self.bind_selected_page(state, sel);
        }
    }

    /// Bind the editor to `sel`'s blocks in the doc (creating the page
    /// in the doc if needed, seeding its blocks from the on-disk body).
    fn bind_selected_page(&mut self, state: &mut AutoCommit, sel: PageId) {
        if !NotesDoc::list_pages(state).iter().any(|p| p.id == sel) {
            if let Some(meta) = self.workspace.get(sel).cloned() {
                NotesDoc::upsert_page_meta(state, &meta);
            }
        }
        let Some(list) = NotesDoc::page_blocks_list(state, sel) else {
            return;
        };
        let title = self
            .workspace
            .get(sel)
            .map(|m| m.title.clone())
            .unwrap_or_default();
        let body = storage::read_page_body(&self.root, sel).unwrap_or_default();
        self.main.bind_page(state, sel, title, list, &body);
        self.bound_page = Some(sel);
    }

    /// Test hook: collapse the seed grace window so the next `sync`
    /// seeds immediately (tests can't wait 800 ms of wall-clock).
    #[doc(hidden)]
    pub fn bypass_seed_grace_for_test(&mut self) {
        self.started = std::time::Instant::now()
            .checked_sub(SEED_GRACE + Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
    }

    /// Test hook: the editor's block text joined by newlines.
    #[doc(hidden)]
    pub fn editor_text_for_test(&self) -> String {
        self.main
            .editor()
            .blocks()
            .iter()
            .map(|b| b.text())
            .collect::<Vec<_>>()
            .join("\n")
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
                    self.load_page_with_resolver(id, title, body);
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
        // After every event refresh the link pages so the popovers
        // see fresh titles.
        self.refresh_link_pages();
        self.persist();
        self.refresh_status();
    }

    /// Load a page through `MainPanel::load_page_with_link_resolver`,
    /// resolving `[[Title]]` link marks against the current
    /// workspace's page list.
    fn load_page_with_resolver(&mut self, id: PageId, title: String, body: String) {
        // Snapshot the title→id map ahead of borrowing main mutably.
        let by_title: std::collections::HashMap<String, PageId> = self
            .workspace
            .pages()
            .iter()
            .map(|p| (p.title.clone(), p.id))
            .collect();
        self.main.load_page_with_link_resolver(id, title, body, |t| {
            by_title.get(t).copied()
        });
    }

    /// Navigate to `id`: select + load the page. Used both by the
    /// Cmd-K palette and by click-on-link events.
    pub fn navigate_to(&mut self, id: PageId) {
        self.flush_main_panel_edit();
        if let Some(meta) = self.workspace.get(id) {
            let title = meta.title.clone();
            let body = storage::read_page_body(&self.root, id).unwrap_or_default();
            self.workspace.set_selected(Some(id));
            self.load_page_with_resolver(id, title, body);
            self.sidebar.sync_from_workspace(&self.workspace);
            self.persist();
        }
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
        self.refresh_status();
    }

    /// Test hook: read-only access to the status bar so integration
    /// tests can assert on save state / page count / word count
    /// without going through pixel-level inspection.
    #[doc(hidden)]
    pub fn status_bar_for_test(&self) -> &crate::status_bar::StatusBar {
        &self.status
    }

    /// Test hook: apply one [`OutEvent`] through the same code path
    /// the sidebar would drive it. Lets integration tests simulate
    /// "user clicked Create Child in the sidebar" without forging
    /// fake winit events.
    #[doc(hidden)]
    pub fn apply_event_for_test(&mut self, ev: OutEvent) {
        self.apply_event(ev);
    }
}

/// First-launch seed: one page titled "Welcome" that doubles as a
/// guided tour. Each section opens with a paragraph caption so the
/// downstream block reads as a labelled demo rather than a test
/// fixture. The body must round-trip through
/// `notes::markdown::{parse,serialize}` byte-stably —
/// `welcome_page_renders_all_block_kinds` pins that.
pub const WELCOME_BODY: &str = "\
# Welcome to notes

A collaborative notes app for qOS. Edit anywhere; the page tree on the left organises your pages, the panel on the right is for typing.

You can have **bold**, *italic*, `inline code`, and a [[Welcome]] link to any other page right in the middle of a paragraph.

## Block types

Headings look like this — type `#`, `##`, or `###` followed by a space at the start of a line.

Bullet lists look like this:

- Coffee

- Tea

- Hot chocolate

  - With marshmallows

Checklists track open work:

- [ ] Open todo

- [x] Completed item

Code blocks are great for snippets:

```rust
fn hello() { println!(\"slash to open the menu\"); }
```

A divider separates sections:

---

## Keyboard

The essentials, in roughly the order you'll need them:

- `Cmd-N` / `Ctrl-N` creates a new page under the selected one

- `Cmd-K` / `Ctrl-K` jumps to any page by name

- `/` opens the block menu wherever the cursor is

- `[[` opens a link picker — start typing a page title

- `**bold**`, `*italic*`, and backtick-code work inline as you type

- `Tab` / `Shift-Tab` indents and outdents list items

- `Delete` (with the sidebar focused) removes the selected page

- `?` opens the full shortcuts overlay

Create your first page from the sidebar — double-click any title to rename it.
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
        // Bridge the in-memory workspace to the shared CRDT document:
        // seed/adopt the page tree, then bind the editor to the selected
        // page so its text syncs between instances.
        self.sync_workspace_tree(state);
        self.ensure_editor_bound(state);
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

        // Content row: sidebar + main, side-by-side. Lives inside the
        // outer column so the status bar sits beneath it.
        let content_node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Row,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    flex_grow: 1.0,
                    flex_basis: Dimension::length(0.0),
                    gap: taffy::Size {
                        width: length(0.0),
                        height: length(0.0),
                    },
                    ..Default::default()
                },
                &[sidebar_node, main_node],
            )
            .unwrap();
        self.content_node = Some(content_node);

        // Status bar.
        let status_node = self.status.build_layout(taffy, theme, node_map);
        self.status_node = Some(status_node);

        // Outer column: content row + status bar.
        let root = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
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
                &[content_node, status_node],
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
        // The content row sits inside the root column; sidebar + main
        // are children of it. Each child widget's `apply_layout` adds
        // its own `layout.location` to the origin we pass, so we need
        // to hand it the *parent's* absolute origin.
        if let Some(content_node) = self.content_node {
            let content_layout = taffy.layout(content_node).unwrap();
            let content_origin = Point::new(
                new_origin.x + content_layout.location.x as f64,
                new_origin.y + content_layout.location.y as f64,
            );
            if let Some(n) = self.sidebar_node {
                self.sidebar.apply_layout(taffy, n, content_origin);
            }
            if let Some(n) = self.main_node {
                self.main.apply_layout(taffy, n, content_origin);
            }
        }
        if let Some(n) = self.status_node {
            self.status.apply_layout(taffy, n, new_origin);
        }
        // Keep the shortcuts overlay's viewport synced — host-driven
        // since the overlay never participates in the taffy pass.
        self.shortcuts.set_viewport(self.rect);
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
        // v0.1: status bar across the bottom edge of the window.
        self.status.draw(scene, transform, text_renderer, theme, presence);
        // Phase 4: page-jump palette renders on top of everything.
        if self.palette.is_visible() {
            // Dim backdrop — `surfaces.overlay` is `bg-black/80` (the
            // concept's modal scrim alpha). Reuse it rather than
            // hand-rolling a colour literal.
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.overlay,
                None,
                &self.rect,
            );
            self.palette.draw_overlay(scene, transform, text_renderer, theme);
        }
        // v0.1: shortcuts overlay sits above every other layer when
        // visible. Drawn last so it covers the palette too.
        self.shortcuts.draw_overlay(scene, transform, text_renderer, theme);
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
        use winit::event::{ElementState, KeyEvent, MouseButton};
        use winit::keyboard::Key;

        // v0.1: shortcuts overlay is fully blocking when visible.
        if self.shortcuts.is_visible() {
            if self.shortcuts.handle_key(event, scale_factor) {
                *handled = true;
                self.refresh_status();
                return;
            }
            // Even unconsumed events (e.g. mouse move when the overlay
            // is up) shouldn't reach the editor underneath.
            *handled = true;
            return;
        }

        // v0.1: `?` (Shift+/) opens the shortcuts overlay. Two flavours:
        // a literal `?` character commit (most layouts) and the named
        // key Slash with shift held (some IME / mobile-keyboard paths).
        if let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key: Key::Character(c), state: ElementState::Pressed, .. },
            ..
        } = event
        {
            if c.as_str() == "?" {
                self.shortcuts.set_visible(true);
                *handled = true;
                self.refresh_status();
                return;
            }
        }

        // Phase 4: Cmd-K / Ctrl-K opens the page jump palette.
        // Intercepted BEFORE child widgets so a focused input
        // doesn't swallow the keystroke.
        if !self.palette.is_visible() {
            if let WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key: Key::Character(c), state: ElementState::Pressed, .. },
                ..
            } = event
            {
                let mods = modifiers.state();
                if (mods.control_key() || mods.super_key()) && c.to_lowercase().as_str() == "k" {
                    let anchor_x =
                        (self.rect.x0 + self.rect.x1) / 2.0 - self.palette.width() / 2.0;
                    let anchor_y = self.rect.y0 + 80.0;
                    self.palette.set_anchor(vello::kurbo::Point::new(anchor_x, anchor_y));
                    self.palette.set_visible(true);
                    *handled = true;
                    return;
                }
            }
        }

        // While the palette is visible it owns ALL keystrokes.
        if self.palette.is_visible() {
            if let WindowEvent::KeyboardInput { .. } = event {
                let consumed = self.palette.handle_key(event);
                if let Some(entry) = self.palette.take_selection() {
                    self.navigate_to(entry.id);
                }
                if consumed {
                    *handled = true;
                    return;
                }
            }
        }

        // Phase 4: click on a link mark navigates.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            if let Some(id) = self.link_under_last_mouse() {
                self.navigate_to(id);
                *handled = true;
                return;
            }
        }

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

        // Update the last-mouse cache so the next MouseInput knows
        // where to hit-test for link clicks.
        if let WindowEvent::CursorMoved { position, .. } = event {
            let logical = vello::kurbo::Point::new(
                position.x / scale_factor,
                position.y / scale_factor,
            );
            self.last_mouse = logical;
        }
        // Status bar refresh runs at the tail of every event so word
        // count + save indicator stay current. Cheap — the StatusBar
        // diffs internally.
        self.refresh_status();
    }

    fn name(&self) -> &str {
        "NotesApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.sidebar.needs_redraw()
            || self.main.needs_redraw()
            || self.status.needs_redraw()
            || self.shortcuts.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.sidebar.mark_drawn();
        self.main.mark_drawn();
        self.status.mark_drawn();
        self.shortcuts.mark_drawn();
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
