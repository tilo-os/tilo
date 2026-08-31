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
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::Widget;
use ui::collaboration::PresencePayload;
use ui::color_compat::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::{PresenceCtx, PresenceKey};
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::SidebarDrawer;
use ui::widget::link_popover::PageEntry;
use ui::widget::page_palette::PageJumpPalette;

use crate::actions::{self, NotesAction};
use crate::doc::NotesDoc;
use crate::main_panel::MainPanel;
use crate::model::{PageId, Workspace};
use crate::num;
use crate::shortcuts_overlay::ShortcutsOverlay;
use crate::sidebar::{NoteTreeView, OutEvent};
use crate::status_bar::{StatusBarState, StatusModel, count_words};
use crate::storage::{self, PageSaver, SaveStatus, Saver};

/// Sidebar width when the window is wide enough to show it inline.
/// 240 px reads as "comfortable but not greedy" next to the editor.
const SIDEBAR_WIDTH: f64 = 240.0;
/// Width of the page tree when it slides in as an overlay drawer on a
/// narrow window. Slightly wider than the inline sidebar so titles and
/// the disclosure triangles have room on a small screen.
const DRAWER_WIDTH: f64 = 264.0;
/// Below this logical window width the sidebar stops taking a fixed
/// column and becomes a closed overlay drawer, reached from the
/// top-bar menu button. Chosen so 800 px keeps the sidebar inline but
/// 560 px collapses it.
const NARROW_BREAKPOINT: f64 = 720.0;
/// Height of the top bar shown only in narrow mode. Hosts the drawer
/// toggle and the current page title.
const TOPBAR_HEIGHT: f64 = 44.0;
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
const SEED_GRACE: Duration = Duration::from_millis(1500);

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independently-toggled flag from a distinct concern (shots override, collab sync-state tracking, responsive layout mode, dirty tracking) with no shared sub-concept; grouping them would only relocate the count, not reduce it"
)]
pub struct NotesApp {
    workspace: Workspace,
    sidebar: NoteTreeView,
    main: MainPanel,
    /// The note's save + page/word status, surfaced as a transient
    /// floating bubble (see `status_bubble`) instead of a permanent strip.
    status: StatusModel,
    /// Transient floating status bubble: poked on a status change (a save
    /// flip, a page switch, a word-count tick), and shown while the pointer
    /// hovers the bottom-right corner. Replaces the old bottom status bar so
    /// the editor spans to the window bottom.
    status_bubble: ui::StatusBubble,
    /// `--shots` forces the bubble fully visible so a still frame captures it.
    shots_status: bool,
    shortcuts: ShortcutsOverlay,
    /// Root container rect, populated by `apply_layout`.
    rect: Rect,
    /// Cached taffy node IDs. Filled in `build_layout`, used by
    /// `apply_layout` and `handle_event`.
    sidebar_node: Option<NodeId>,
    main_node: Option<NodeId>,
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
    /// Start of the seed-grace window. Set to the time of the FIRST
    /// `sync` (when collab is actually live), not spawn — renderer init
    /// takes seconds, and measuring from spawn would let a late joiner's
    /// grace lapse before its first sync, making it seed its own welcome
    /// instead of adopting an existing peer's.
    started: std::time::Instant,
    /// Whether the first `sync` has run (which (re)bases `started`).
    synced_once: bool,
    /// True once we've taken on shared document state — either by
    /// adopting a peer's or by seeding our own. Gates the one-time
    /// discard-local-welcome-on-adopt.
    adopted: bool,
    /// True once our page tree has been mirrored into the shared doc.
    seeded: bool,
    /// Pages deleted locally this session that still need to be removed from
    /// the shared doc on the next `sync` (`apply_event` has no CRDT access,
    /// and the sync pull-loop would otherwise resurrect them).
    pending_doc_removals: Vec<PageId>,
    /// The page the `MainPanel` editor is currently bound to in the doc.
    /// `sync` rebinds whenever this drifts from the selected page.
    bound_page: Option<PageId>,
    /// Desired layout mode derived from the last observed window width.
    /// Updated in `apply_layout`; consumed at the next `build_layout`.
    want_narrow: bool,
    /// The mode the current taffy tree was actually built with. `draw`
    /// and `handle_event` read this so they always match the live tree,
    /// even for the one frame before a breakpoint crossing rebuilds.
    built_narrow: bool,
    /// The responsive sidebar/drawer controller: it owns the docked-vs-overlay
    /// decision (at [`NARROW_BREAKPOINT`]), the open/closed state, and the
    /// dimming scrim, replacing the hand-rolled breakpoint + `drawer_open` flag +
    /// scrim fill this app used to carry.
    drawer: SidebarDrawer,
    /// Set when the window crossed the narrow breakpoint so the tree
    /// needs a structural rebuild. Drained by `take_structure_dirty`.
    pending_relayout: bool,
    /// Forces a redraw when our own chrome (top bar / drawer) changes
    /// without any child widget flagging itself dirty.
    self_dirty: bool,
    /// Top-bar taffy node — present only while `built_narrow`.
    topbar_node: Option<NodeId>,
    /// Rect of the narrow-mode top bar, from `apply_layout`.
    topbar_rect: Rect,
    /// Hit rect of the drawer toggle (hamburger) button.
    menu_btn_rect: Rect,
    /// Rect of the content area (between top bar and status bar). Used
    /// to place the drawer scrim in narrow mode.
    content_rect: Rect,
    /// Framework-owned action dispatch. Each on-screen affordance dispatches its
    /// [`NotesAction`] through this: the host brackets the undo boundary, applies
    /// the action through the executor (so it replays byte-identically on peers,
    /// exactly as the Ctrl+K palette does), and records the last one so a test can
    /// prove a sidebar affordance and its [`ActionSpec`](qos_actions::ActionSpec)
    /// build the identical action.
    actions: ui::ActionHost,
    /// Page ids that have appeared in the shared document. Steady-state `sync`
    /// uses this to tell a never-yet-mirrored local page (push it up) from a page
    /// the document dropped — a Ctrl+Z undo of a create, or a remote delete —
    /// which must be removed locally too. Without it the unconditional local→doc
    /// mirror would resurrect an undone page every frame.
    known_in_doc: std::collections::HashSet<PageId>,
}

impl NotesApp {
    /// Load the workspace under `root` and wire up the debounced background
    /// saver. Returns a fully-initialised widget ready to be handed to
    /// `ui::App`.
    ///
    /// A brand-new workspace starts **empty** — no fabricated pages. The
    /// sidebar's "+" (and its empty-area "New note" menu) is the affordance to
    /// create the first page. Tests and headless captures that need a starting
    /// page use [`Self::spawn_with_starter_page`].
    ///
    /// # Errors
    ///
    /// Returns `Err` if the workspace directory can't be created, its saved
    /// state fails to load or decode, or the background saver threads fail
    /// to spawn.
    pub fn spawn(root: PathBuf) -> Result<Self, String> {
        Self::spawn_inner(root, false)
    }

    /// Like [`Self::spawn`] but seeds a "Welcome" starter page into an empty
    /// workspace. Used by tests and dev fixtures that need content to exercise;
    /// the live binary always uses [`Self::spawn`] / [`Self::spawn_default`],
    /// which start empty.
    ///
    /// # Errors
    ///
    /// Same failure modes as [`Self::spawn`].
    pub fn spawn_with_starter_page(root: PathBuf) -> Result<Self, String> {
        Self::spawn_inner(root, true)
    }

    fn spawn_inner(root: PathBuf, seed_starter: bool) -> Result<Self, String> {
        storage::ensure_layout(&root).map_err(|e| e.to_string())?;
        let mut workspace = storage::load_workspace(&root)?.unwrap_or_default();
        if seed_starter && workspace.pages().is_empty() {
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
                main.load_page_with_link_resolver(sel, &meta.title, body, |t| {
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
                main.load_page_with_link_resolver(id, &title, body, |t| by_title.get(t).copied());
                sidebar.sync_from_workspace(&workspace);
            }
        }

        let saver = Saver::spawn(root.clone(), SAVE_DEBOUNCE).map_err(|e| e.to_string())?;
        let page_saver =
            PageSaver::spawn(root.clone(), SAVE_DEBOUNCE).map_err(|e| e.to_string())?;

        let mut app = Self {
            workspace,
            sidebar,
            main,
            status: StatusModel::new(),
            status_bubble: ui::StatusBubble::new(),
            shots_status: false,
            shortcuts: ShortcutsOverlay::new(),
            rect: Rect::ZERO,
            sidebar_node: None,
            main_node: None,
            content_node: None,
            root,
            saver: Some(saver),
            page_saver: Some(page_saver),
            palette: PageJumpPalette::new(),
            last_mouse: Point::ZERO,
            started: std::time::Instant::now(),
            synced_once: false,
            adopted: false,
            seeded: false,
            pending_doc_removals: Vec::new(),
            bound_page: None,
            want_narrow: false,
            built_narrow: false,
            drawer: SidebarDrawer::new()
                .left()
                .docked_width(SIDEBAR_WIDTH)
                .overlay_width(DRAWER_WIDTH)
                .breakpoint(NARROW_BREAKPOINT),
            pending_relayout: false,
            self_dirty: false,
            topbar_node: None,
            topbar_rect: Rect::ZERO,
            menu_btn_rect: Rect::ZERO,
            content_rect: Rect::ZERO,
            actions: ui::ActionHost::new(),
            known_in_doc: std::collections::HashSet::new(),
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
            .map_or(SaveStatus::Saved, super::storage::Saver::status);
        let page_status = self
            .page_saver
            .as_ref()
            .map_or(SaveStatus::Saved, super::storage::PageSaver::status);
        let save_status = combine_save_status(workspace_status, page_status);
        let changed = self.status.set_state(StatusBarState {
            save_status,
            page_path: path,
            page_count: self.workspace.pages().len(),
            word_count,
        });
        // Surface the floating status bubble only on a real change (a save
        // flip, a page switch, a word-count tick) — refresh_status runs every
        // frame, so poking unconditionally would pin the bubble open forever.
        if changed {
            self.status_bubble.poke(std::time::Instant::now());
        }
    }

    /// `--shots`: force the status bubble fully visible so a single headless
    /// frame captures the floating chip.
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.status_bubble.poke(std::time::Instant::now());
    }

    /// Draw the transient floating status bubble at the bottom-right of the
    /// window — the reclaimed home of the old bottom status strip. It shows
    /// on a status change or while the pointer hovers the bottom-right band,
    /// then fades. All colours come from the theme (via `draw_pill`).
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = std::time::Instant::now();
        let label = self.status.bubble_text();
        let text_size = 12.0f32;
        let (w, h) = ui::status_bubble::pill_size(&label, tr, text_size, 0.0);
        let margin = 14.0;
        let rect = Rect::new(
            self.rect.x1 - margin - w,
            self.rect.y1 - margin - h,
            self.rect.x1 - margin,
            self.rect.y1 - margin,
        );
        // Reveal zone: the bottom-right corner band (plus the pill), so nudging
        // the pointer there re-surfaces the status on demand.
        let zone = Rect::new(rect.x0 - 40.0, rect.y0 - 24.0, self.rect.x1, self.rect.y1);
        let over = self.shots_status || zone.contains(self.last_mouse);
        self.status_bubble.set_hover(over, now);
        let alpha = self.status_bubble.alpha(now);
        if alpha <= 0.0 {
            return;
        }
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            alpha,
            transform,
            &rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, transform, tr, theme, rect, &label, text_size);
        scene.pop_layer();
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
            .map(|p| PageEntry {
                id: p.id,
                title: p.title.clone(),
            })
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

    /// Paint the narrow-mode top bar: a drawer-toggle (hamburger)
    /// button on the left and the current page title beside it.
    fn draw_topbar(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let tokens = &theme.tokens;
        // Bar background + bottom hairline so it reads as chrome.
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.raised,
            None,
            &self.topbar_rect,
        );
        let hairline = Rect::new(
            self.topbar_rect.x0,
            self.topbar_rect.y1 - 1.0,
            self.topbar_rect.x1,
            self.topbar_rect.y1,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.border_standard,
            None,
            &hairline,
        );

        // Hamburger glyph: three evenly-spaced rounded bars centred in
        // the button hit rect. Highlight the button while the drawer is
        // open so the toggle state reads.
        if self.drawer.is_open() {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.accent_primary.with_alpha_factor(0.18),
                None,
                &self.menu_btn_rect,
            );
        }
        let cx = self.menu_btn_rect.center().x;
        let cy = self.menu_btn_rect.center().y;
        let bar_w = 18.0;
        let bar_h = 2.0;
        for dy in [-6.0f64, 0.0, 6.0] {
            let bar = vello::kurbo::RoundedRect::new(
                cx - bar_w / 2.0,
                cy + dy - bar_h / 2.0,
                cx + bar_w / 2.0,
                cy + dy + bar_h / 2.0,
                1.0,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                tokens.colors.text_primary,
                None,
                &bar,
            );
        }

        // Current page title, vertically centred, to the right of the
        // button.
        let title = self
            .workspace
            .selected()
            .and_then(|id| self.workspace.get(id))
            .map_or_else(|| "notes".to_string(), |m| m.title.clone());
        let size = tokens.typography.scale.base;
        let text_x = self.menu_btn_rect.x1 + 4.0;
        let text_y = self.topbar_rect.y0 + (self.topbar_rect.height() - f64::from(size)) / 2.0;
        let avail = num::f32_of_f64((self.topbar_rect.x1 - text_x - 12.0).max(0.0));
        text_renderer.draw_text(
            scene,
            &title,
            size,
            tokens.colors.text_primary,
            transform * Affine::translate((text_x, text_y)),
            Some(avail),
        );
    }

    /// Open or close the narrow-mode overlay drawer. Snaps (no slide) so the
    /// behaviour matches the pre-adoption instant toggle; the drawer's own
    /// scrim and breakpoint memory come from the shared [`SidebarDrawer`].
    fn set_drawer_open(&mut self, open: bool) {
        if self.drawer.is_open() != open {
            if open {
                self.drawer.open();
            } else {
                self.drawer.close();
            }
            self.drawer.snap(open);
            self.self_dirty = true;
        }
    }

    /// Default constructor — resolves the workspace root via `dirs`,
    /// falls back to `./notes` if the home dir is unavailable.
    ///
    /// # Errors
    ///
    /// Same failure modes as [`Self::spawn`].
    pub fn spawn_default() -> Result<Self, String> {
        let root = storage::default_root().unwrap_or_else(|| PathBuf::from("./notes"));
        Self::spawn(root)
    }

    /// Reconcile our in-memory page tree with the shared Automerge
    /// document. Seeds the doc from our welcome page when we're the
    /// origin, adopts a peer's tree when we join late, and keeps both in
    /// step thereafter. Runs every frame from [`sync`].
    fn sync_workspace_tree(&mut self, state: &mut AutoCommit) {
        // Base the seed-grace window on the first sync (collab is live by
        // now), not spawn — see `started`'s doc comment.
        if !self.synced_once {
            self.started = std::time::Instant::now();
            self.synced_once = true;
        }
        // Apply local deletions to the shared doc BEFORE reading it, so the
        // pull-loop below doesn't resurrect a just-deleted page.
        for id in std::mem::take(&mut self.pending_doc_removals) {
            NotesDoc::remove_page(state, id);
        }
        let doc_pages = NotesDoc::list_pages(state);
        if !doc_pages.is_empty() {
            if self.adopted {
                // Already sharing. The document is authoritative for the page
                // tree: every local tree mutation is dispatched as an action
                // that writes it (see `dispatch_tree_diff`), and remote peers'
                // mutations arrive by merge. Reconcile the workspace to match —
                // pull new pages, fold changed metadata back in, and drop pages
                // the document no longer holds (a Ctrl+Z undo of a create, or a
                // remote delete). A page the document has never held yet (a
                // freshly seeded welcome, or a test-created page) is pushed up.
                let doc_ids: std::collections::HashSet<PageId> =
                    doc_pages.iter().map(|p| p.id).collect();
                for dp in &doc_pages {
                    self.known_in_doc.insert(dp.id);
                }
                let mut changed = false;
                for dp in &doc_pages {
                    if let Some(existing) = self.workspace.get_mut(dp.id) {
                        if existing != dp {
                            *existing = dp.clone();
                            changed = true;
                        }
                    } else {
                        self.workspace.data.pages.push(dp.clone());
                        changed = true;
                    }
                }
                // Drop pages the document dropped. Only pages it once held — a
                // never-mirrored local page stays and is pushed up below.
                let to_drop: Vec<PageId> = self
                    .workspace
                    .pages()
                    .iter()
                    .filter(|p| !doc_ids.contains(&p.id) && self.known_in_doc.contains(&p.id))
                    .map(|p| p.id)
                    .collect();
                if !to_drop.is_empty() {
                    self.workspace
                        .data
                        .pages
                        .retain(|p| !to_drop.contains(&p.id));
                    changed = true;
                    if let Some(s) = self.workspace.selected()
                        && self.workspace.get(s).is_none()
                    {
                        self.workspace.set_selected(None);
                    }
                }
                // Push any local page the document has never held yet (seed
                // carry-over or a test-created page), recording it as known.
                for p in self.workspace.pages().to_vec() {
                    if !doc_ids.contains(&p.id) {
                        NotesDoc::upsert_page_meta(state, &p);
                        self.known_in_doc.insert(p.id);
                    }
                }
                if changed {
                    self.sidebar.sync_from_workspace(&self.workspace);
                }
            } else {
                // First shared state we've seen — adopt it wholesale,
                // discarding our locally-seeded welcome so we don't push
                // a duplicate page tree.
                if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                    eprintln!("[notes] ADOPT {} remote page(s)", doc_pages.len());
                }
                let ids: Vec<PageId> = doc_pages.iter().map(|p| p.id).collect();
                self.workspace.data.pages = doc_pages;
                self.known_in_doc.extend(ids);
                if let Some(s) = self.workspace.selected()
                    && self.workspace.get(s).is_none()
                {
                    self.workspace.set_selected(None);
                }
                self.bound_page = None;
                self.main.clear();
                self.adopted = true;
                self.seeded = true;
                self.sidebar.sync_from_workspace(&self.workspace);
            }
        } else if !self.seeded && self.started.elapsed() > SEED_GRACE {
            // Doc still empty after the grace window — we're the origin.
            if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                eprintln!(
                    "[notes] SEED own welcome ({} page(s)) after {:?} empty",
                    self.workspace.pages().len(),
                    self.started.elapsed()
                );
            }
            for p in self.workspace.pages().to_vec() {
                NotesDoc::upsert_page_meta(state, &p);
                self.known_in_doc.insert(p.id);
            }
            self.seeded = true;
            self.adopted = true;
        }
    }

    /// Ensure a page is selected and the `MainPanel` editor is bound to
    /// that page's `blocks` List in the shared doc, so its text syncs.
    fn ensure_editor_bound(&mut self, state: &mut AutoCommit) {
        // Don't touch the doc until we've seeded or adopted the shared
        // workspace. Binding earlier would `upsert` a page into the doc,
        // creating an independent `pages` Map before the late-join
        // handshake establishes a shared base — which makes two peers'
        // page trees conflict on merge and never converge.
        if !self.adopted {
            return;
        }
        if self.workspace.selected().is_none()
            && let Some(first) = self.workspace.children_of(None).first().map(|p| p.id)
        {
            self.workspace.set_selected(Some(first));
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

    /// Bind the editor to `sel`'s blocks in the shared doc. The page must
    /// already exist in the doc (placed there by seeding or adoption) —
    /// we deliberately do NOT create it here, so binding can never fork
    /// the `pages` Map ahead of the handshake.
    fn bind_selected_page(&mut self, state: &mut AutoCommit, sel: PageId) {
        let Some(list) = NotesDoc::page_blocks_list(state, sel) else {
            return; // page not in the shared doc yet — wait for sync
        };
        let title = self
            .workspace
            .get(sel)
            .map(|m| m.title.clone())
            .unwrap_or_default();
        let body = storage::read_page_body(&self.root, sel).unwrap_or_default();
        self.main.bind_page(state, sel, &title, list, &body);
        self.bound_page = Some(sel);
    }

    /// Set the local user's id for authorship attribution. The same id
    /// the binary derives from `QUICK_APP_ID` and uses for presence, so
    /// authored text and the peer's cursor share a color.
    pub fn set_local_author(&mut self, author: Option<String>) {
        self.main.set_local_author(author);
    }

    /// Test hook: collapse the seed grace window so the next `sync`
    /// seeds immediately (tests can't wait 800 ms of wall-clock).
    #[doc(hidden)]
    pub fn bypass_seed_grace_for_test(&mut self) {
        self.started = std::time::Instant::now()
            .checked_sub(SEED_GRACE + Duration::from_secs(1))
            .unwrap_or_else(std::time::Instant::now);
        // Mark first-sync as done so `sync` doesn't re-base `started`
        // back to "now" and undo the bypass.
        self.synced_once = true;
    }

    /// Test hook: the editor's block text joined by newlines.
    #[doc(hidden)]
    pub fn editor_text_for_test(&self) -> String {
        self.main
            .editor()
            .blocks()
            .iter()
            .map(ui::widget::Block::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    // --- action dispatch -------------------------------------------------

    /// Dispatch one [`NotesAction`] — the single CRDT-mutation path shared with
    /// the Ctrl+K palette. Applied through the qOS action executor (a pinned
    /// actor + committed change, so it replays byte-identically on peers). The
    /// document heads captured just before the first action of the current event
    /// are stashed so the host records **one** undo boundary bracketing it
    /// ([`Widget::take_committed_action`]). The in-memory workspace is updated by
    /// the caller for immediate UI feedback and reconciled from the document on
    /// the next `sync`. This app supplies only its domain: the action and its type.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &NotesAction) {
        self.actions
            .dispatch(doc, actions::NOTES_ACTION_TYPE, action);
    }

    /// The context Ctrl+K needs to build the notebook's resource tree: the page
    /// tree the workspace holds (which mirrors the shared `pages` map).
    pub(crate) fn notes_resource_ctx(&self) -> crate::actions::NotesResourceCtx {
        crate::actions::NotesResourceCtx {
            pages: self.workspace.pages().to_vec(),
        }
    }

    /// Snapshot every page's metadata by id, for the pre/post diff that turns an
    /// applied [`OutEvent`] into the matching [`NotesAction`](crate::actions).
    fn meta_map(&self) -> HashMap<PageId, crate::model::PageMeta> {
        self.workspace
            .pages()
            .iter()
            .map(|p| (p.id, p.clone()))
            .collect()
    }

    /// Apply one sidebar [`OutEvent`] to the workspace, then dispatch the
    /// matching [`NotesAction`](crate::actions) so the change lands in the shared
    /// document on the same replayable path the Ctrl+K palette uses.
    fn apply_and_dispatch(&mut self, doc: &mut AutoCommit, ev: OutEvent) {
        let before = self.meta_map();
        self.apply_event(ev);
        let after = self.meta_map();
        self.dispatch_tree_diff(doc, &before, &after);
        // The action path removed any deleted page from the document itself, so
        // drop the removals `apply_event` queued for the next `sync` — replaying
        // them would re-delete a page a Ctrl+Z undo has just restored.
        self.pending_doc_removals.clear();
    }

    /// Turn the page-tree change [`apply_event`] just made into the matching
    /// action(s). Deletes are dispatched before upserts so a delete's own sibling
    /// renumber (which `apply` recomputes in the document) doesn't double-write.
    fn dispatch_tree_diff(
        &mut self,
        doc: &mut AutoCommit,
        before: &HashMap<PageId, crate::model::PageMeta>,
        after: &HashMap<PageId, crate::model::PageMeta>,
    ) {
        // Deletions: a page present before and gone after. Dispatch one
        // DeletePage per *deletion root* (a removed page whose parent wasn't
        // also removed) — `apply` deletes the whole subtree, so descendants need
        // no separate action.
        for (id, meta) in before {
            if after.contains_key(id) {
                continue;
            }
            let parent_also_removed = meta
                .parent
                .is_some_and(|p| before.contains_key(&p) && !after.contains_key(&p));
            if !parent_also_removed {
                self.perform_action(doc, &actions::delete_page_action(*id));
            }
        }
        // Creates + edits: a page new in `after`, or one whose metadata changed
        // (title / parent / order). Each is a whole-page upsert.
        for (id, meta) in after {
            match before.get(id) {
                Some(old) if old == meta => {}
                _ => self.perform_action(doc, &actions::upsert_page_action(meta)),
            }
        }
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
                    self.load_page_with_resolver(id, &title, body);
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
                // Propagate the deletion to the shared doc (done in `sync`,
                // which has CRDT access) so it isn't resurrected next frame.
                self.pending_doc_removals.extend(removed.iter().copied());
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
                        let body = storage::read_page_body(&self.root, id).unwrap_or_default();
                        self.main.load_page(id, &title, body);
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
                self.main.load_page(id, "Untitled", String::new());
            }
            OutEvent::Move { id, delta } => {
                // Reorder among siblings. The new `order` propagates to
                // the shared doc on the next `sync` (which upserts every
                // local page's meta) and to disk via `persist()` below.
                self.workspace.move_sibling(id, delta);
            }
            OutEvent::Reparent {
                id,
                new_parent,
                before,
            } => {
                // Drag-drop reorder / re-parent. The new `parent` + `order`
                // propagate to the shared doc on the next `sync` and to
                // disk via `persist()` below. `move_to` rejects cycles.
                self.workspace.move_to(id, new_parent, before);
            }
            OutEvent::Duplicate(id) => {
                self.flush_main_panel_edit();
                if let Some((new_root, mapping)) = self.workspace.duplicate_page(id) {
                    // Copy each source page's body onto its fresh copy id so
                    // the duplicated subtree has independent content on disk.
                    for (old, new) in &mapping {
                        let body = storage::read_page_body(&self.root, *old).unwrap_or_default();
                        if let Err(e) = storage::write_page_body(&self.root, *new, &body) {
                            tracing::error!(error = %e, page = %new, "notes: duplicate body write failed");
                        }
                    }
                    self.workspace.set_selected(Some(new_root));
                    if let Some(meta) = self.workspace.get(new_root) {
                        let title = meta.title.clone();
                        let body =
                            storage::read_page_body(&self.root, new_root).unwrap_or_default();
                        self.load_page_with_resolver(new_root, &title, body);
                    }
                }
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
    fn load_page_with_resolver(&mut self, id: PageId, title: &str, body: String) {
        // Snapshot the title→id map ahead of borrowing main mutably.
        let by_title: std::collections::HashMap<String, PageId> = self
            .workspace
            .pages()
            .iter()
            .map(|p| (p.title.clone(), p.id))
            .collect();
        self.main
            .load_page_with_link_resolver(id, title, body, |t| by_title.get(t).copied());
    }

    /// Navigate to `id`: select + load the page. Used both by the
    /// Cmd-K palette and by click-on-link events.
    pub fn navigate_to(&mut self, id: PageId) {
        self.flush_main_panel_edit();
        if let Some(meta) = self.workspace.get(id) {
            let title = meta.title.clone();
            let body = storage::read_page_body(&self.root, id).unwrap_or_default();
            self.workspace.set_selected(Some(id));
            self.load_page_with_resolver(id, &title, body);
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
        let Some(id) = self.main.page_id() else {
            return;
        };
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

    /// Drain the sidebar's outbound event queue and apply each, dispatching the
    /// matching page-tree action against the shared document. Called after every
    /// `handle_event` call.
    fn drain_sidebar_events(&mut self, doc: &mut AutoCommit) {
        let events = self.sidebar.take_events();
        // An expand/collapse toggle mutates the sidebar's `expanded` set
        // without emitting an event; it still needs a re-flatten so the
        // affected children appear / disappear.
        let rows_dirty = self.sidebar.take_rows_dirty();
        if events.is_empty() {
            if rows_dirty {
                self.sidebar.sync_from_workspace(&self.workspace);
            }
            return;
        }
        for ev in events {
            self.apply_and_dispatch(doc, ev);
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

    /// Test hook: centre of the in-page title band, for click targeting.
    #[doc(hidden)]
    pub fn main_title_center_for_test(&self) -> Point {
        self.main.title_rect_for_test().center()
    }

    /// Test hook: whether the in-page title is currently being edited.
    #[doc(hidden)]
    pub fn main_title_editing_for_test(&self) -> bool {
        self.main.title_is_editing_for_test()
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
    pub fn status_bar_for_test(&self) -> &crate::status_bar::StatusModel {
        &self.status
    }

    /// Test hook: re-run the real `refresh_status` merge WITHOUT
    /// draining editor edits (so polling for an async saver result
    /// doesn't re-queue the save and starve its debounce).
    #[doc(hidden)]
    pub fn refresh_status_for_test(&mut self) {
        self.refresh_status();
    }

    /// Test hook: apply one [`OutEvent`] through the same code path
    /// the sidebar would drive it. Lets integration tests simulate
    /// "user clicked Create Child in the sidebar" without forging
    /// fake winit events.
    #[doc(hidden)]
    pub fn apply_event_for_test(&mut self, ev: OutEvent) {
        self.apply_event(ev);
    }

    /// Test hook: apply one [`OutEvent`] AND dispatch the matching page-tree
    /// action against `doc`, exactly as the live sidebar-event path does. Lets a
    /// test prove the on-screen affordance and the Ctrl+K palette resolve to the
    /// same [`EncodedAction`](qos_actions::EncodedAction), and drive the real
    /// undo path.
    #[doc(hidden)]
    pub fn dispatch_event_for_test(&mut self, doc: &mut AutoCommit, ev: OutEvent) {
        self.apply_and_dispatch(doc, ev);
    }

    /// Test hook: the last action an affordance dispatched through
    /// [`Self::perform_action`], so a test can compare it to a palette spec.
    #[doc(hidden)]
    pub fn last_dispatched_for_test(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Test hook: whether the current tree is the narrow (drawer) layout.
    #[doc(hidden)]
    pub fn built_narrow_for_test(&self) -> bool {
        self.built_narrow
    }

    /// Test hook: whether the narrow-mode overlay drawer is open.
    #[doc(hidden)]
    pub fn drawer_open_for_test(&self) -> bool {
        self.drawer.is_open()
    }

    /// Test hook: centre of the drawer-toggle button (narrow mode).
    #[doc(hidden)]
    pub fn menu_btn_center_for_test(&self) -> Point {
        self.menu_btn_rect.center()
    }

    /// Test hook: centre of sidebar row `idx`, for click targeting.
    #[doc(hidden)]
    pub fn sidebar_row_center_for_test(&self, idx: usize) -> Option<Point> {
        self.sidebar.row_center_for_test(idx)
    }

    /// Test hook: x of the disclosure triangle for sidebar row `idx`.
    #[doc(hidden)]
    pub fn sidebar_disclosure_x_for_test(&self, idx: usize) -> Option<f64> {
        self.sidebar.row_disclosure_x_for_test(idx)
    }

    /// Test hook: the tree depth of visible sidebar row `idx`.
    #[doc(hidden)]
    pub fn sidebar_row_depth_for_test(&self, idx: usize) -> Option<usize> {
        self.sidebar.row_depth_for_test(idx)
    }

    /// Test hook: the title shown on sidebar row `idx`.
    #[doc(hidden)]
    pub fn sidebar_row_title_for_test(&self, idx: usize) -> Option<&str> {
        self.sidebar.row_title_for_test(idx)
    }

    /// Test hook: whether visible sidebar row `idx` has children.
    #[doc(hidden)]
    pub fn sidebar_row_has_children_for_test(&self, idx: usize) -> Option<bool> {
        self.sidebar.row_has_children_for_test(idx)
    }

    /// Test hook: whether the active inline rename field is plain-chrome
    /// (styled like the row, not a boxed form field).
    #[doc(hidden)]
    pub fn sidebar_rename_is_plain_for_test(&self) -> Option<bool> {
        self.sidebar.rename_is_plain_for_test()
    }

    /// Test hook: centre of the sidebar's "+" new-page button, for
    /// click targeting through the real event path.
    #[doc(hidden)]
    pub fn sidebar_new_page_btn_center_for_test(&self) -> Point {
        self.sidebar.new_page_btn_center_for_test()
    }

    /// Test hook: whether a live page-tree drag is in flight.
    #[doc(hidden)]
    pub fn sidebar_drag_active_for_test(&self) -> bool {
        self.sidebar.drag_active_for_test()
    }

    /// Test hook: the live reflow shift for sidebar row `idx` (non-zero
    /// while the list reflows to open the drop gap).
    #[doc(hidden)]
    pub fn sidebar_row_shift_for_test(&self, idx: usize) -> f64 {
        self.sidebar.row_shift_for_test(idx)
    }

    /// Test hook: whether the sidebar's right-click context menu is open.
    #[doc(hidden)]
    pub fn sidebar_menu_open_for_test(&self) -> bool {
        self.sidebar.context_menu_open_for_test()
    }

    /// Test hook: whether the editor's `/` block menu is visible.
    #[doc(hidden)]
    pub fn editor_slash_menu_visible_for_test(&self) -> bool {
        self.main.editor().slash_menu_visible()
    }

    /// Test hook: drop the editor's keyboard focus so global single-key
    /// shortcuts (like `?`) fire instead of typing into a note. Mirrors
    /// the state when the page tree — not the editor — is focused.
    #[doc(hidden)]
    pub fn blur_editor_for_test(&mut self) {
        self.main.editor_mut().set_focus(false);
    }

    /// Test hook: whether the shortcuts overlay is currently visible.
    #[doc(hidden)]
    pub fn shortcuts_visible_for_test(&self) -> bool {
        self.shortcuts.is_visible()
    }

    /// Test hook: whether the sidebar currently has an inline rename
    /// editor open (e.g. after F2 / double-click).
    #[doc(hidden)]
    pub fn sidebar_is_renaming_for_test(&self) -> bool {
        self.sidebar.is_renaming()
    }

    /// Test hook: mirror the workspace into the sidebar's row list, the
    /// same refresh `drain_sidebar_events` does after applying an event.
    #[doc(hidden)]
    pub fn sync_sidebar_for_test(&mut self) {
        self.sidebar.sync_from_workspace(&self.workspace);
    }

    /// Snapshot hook: force the narrow-mode drawer open so a headless
    /// shot can show the overlay. No effect on a wide layout.
    #[doc(hidden)]
    pub fn open_drawer_for_shots(&mut self) {
        self.set_drawer_open(true);
    }
}

/// First-launch seed: a single starter note the user can edit or
/// delete. It doubles as a quick reference for the editing syntax, so
/// it deliberately uses each block kind at least once. The body must
/// round-trip through `notes::markdown::{parse,serialize}` byte-stably
/// — `welcome_page_renders_all_block_kinds` pins that.
pub const WELCOME_BODY: &str = "\
# Welcome

This is your first note. Edit it, or add a page from the left.

Type here. **Bold**, *italic*, `code`, and [[Welcome]] links work inline.

## Formatting

Start a line with `#` for a heading, or `- ` for a bullet:

- Coffee

- Tea

- Hot chocolate

  - With marshmallows

Add `[ ]` after the dash to track to-dos:

- [ ] Something to finish

- [x] Something already done

Triple backticks make a code block:

```rust
fn hello() {}
```

Or number them when order matters:

1. First, add a page.

2. Then write something.

3. Finally, link it with `[[`.

Three dashes make a divider:

---

## Shortcuts

- `Cmd-K` jumps to any page by name

- `Cmd-N` adds a page under the selection

- `/` opens the block menu; `[[` links a page

- `?` opens the full shortcuts list

Double-click a title in the sidebar to rename it.
";

/// Combine the workspace saver's status with the page-body saver's into
/// the single indicator the status bar renders. Worst-of wins so the
/// most actionable signal surfaces: any `Failed` shows "Save failed";
/// otherwise any `Saving` shows "Saving…"; only when both are clean do
/// we report `Saved`. Free function so the merge is unit-testable
/// without spinning up the background saver threads.
fn combine_save_status(workspace: SaveStatus, page: SaveStatus) -> SaveStatus {
    match (workspace, page) {
        (SaveStatus::Failed, _) | (_, SaveStatus::Failed) => SaveStatus::Failed,
        (SaveStatus::Saving, _) | (_, SaveStatus::Saving) => SaveStatus::Saving,
        _ => SaveStatus::Saved,
    }
}

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

    /// Publish the open notebook + its pages (nested by the tree) so the host's
    /// Ctrl+K action palette scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before a sidebar affordance dispatched
    /// its page-tree action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    /// Fold the notebook into the unified Ctrl+K palette: a snapshot
    /// [`NoteSearchProvider`](crate::search_provider::NoteSearchProvider) makes
    /// every page findable there by title, alongside the actions the app
    /// exports.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_provider::NoteSearchProvider::new(
            &self.workspace,
        ))]
    }

    /// Navigate to the page a chosen palette row identifies. Selection is
    /// app-local presentation (per-peer, never in the shared doc), so the doc
    /// args are unused — this mirrors the click-on-link / Cmd-K page jump.
    fn activate_search_result(&mut self, action_id: &str, _doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(id) = crate::search_provider::NoteSearchProvider::parse_action_id(action_id)
            && self.workspace.get(id).is_some()
        {
            self.navigate_to(id);
        }
    }

    fn presence_payload(&self) -> Option<(PresenceKey, PresencePayload)> {
        // Broadcast which page we're viewing as a Tab location so peers
        // can render "indicative mist" on the sidebar row that leads here.
        // This is per-peer (never in the shared doc), which is what lets
        // two instances view different pages.
        let sel = self.workspace.selected()?;
        Some((
            PresenceKey::Tab {
                id: sel.to_string(),
            },
            PresencePayload::Arrived,
        ))
    }

    fn take_structure_dirty(&mut self) -> bool {
        // Forward the main panel's editor structure-dirty flag so the App
        // rebuilds the taffy tree when the block count changes (local
        // split/merge OR a remote peer adding a block via sync). Without
        // this the new block gets no taffy node and isn't laid out. Also
        // rebuild when the window crossed the narrow breakpoint, since the
        // sidebar-vs-drawer split changes the tree shape.
        let relayout = std::mem::take(&mut self.pending_relayout);
        self.main.take_structure_dirty() || relayout
    }

    fn needs_idle_sync(&self) -> bool {
        // Until we've seeded or adopted the shared document, keep syncing
        // on idle frames: seeding happens in `sync` after a grace window,
        // and a first peer with no input/events would otherwise never run
        // `sync` again to perform it (leaving its doc empty, so a joiner
        // gets nothing and seeds a duplicate).
        !self.adopted
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Commit the mode the tree is built with so draw/event routing
        // stays in lock-step with the live taffy structure.
        self.built_narrow = self.want_narrow;
        let narrow = self.built_narrow;

        let sidebar_node = self.sidebar.build_layout(taffy, theme, node_map);
        let _ = taffy.set_style(sidebar_node, Self::sidebar_style(narrow));
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

        // Content row. Wide: [sidebar | main] side-by-side. Narrow: the
        // editor takes the full width and the drawer floats over it as
        // an absolutely-positioned child (out of flow).
        let content_children: Vec<NodeId> = if narrow {
            vec![main_node, sidebar_node]
        } else {
            vec![sidebar_node, main_node]
        };
        // `new_with_children`/`new_leaf` only error for a corrupted tree;
        // `Widget::build_layout` returns a bare `NodeId` across every
        // implementor, so surfacing a theoretical error here would mean
        // threading a `Result` through the whole toolkit for a branch that
        // can't be taken (see `ui::widget::TaffyTreeExt`).
        #[allow(
            clippy::unwrap_used,
            reason = "new_with_children on freshly-built children cannot fail"
        )]
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
                        width: length(0.0_f32),
                        height: length(0.0_f32),
                    },
                    ..Default::default()
                },
                &content_children,
            )
            .unwrap();
        self.content_node = Some(content_node);

        self.assemble_root(taffy, node_map, narrow, content_node)
    }

    // `taffy.layout(...)` only errors for a `NodeId` from a different tree,
    // which never happens here — every id it's called with is one this
    // widget (or a child it just laid out) received from its own
    // `build_layout`. `Widget::apply_layout` returns `()` across every
    // implementor, so surfacing a theoretical error here would mean
    // threading a `Result` through the whole toolkit for a branch that
    // can't be taken (see `ui::widget::TaffyTreeExt`).
    #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);

        // Decide the responsive mode from the real width. `> 0.0` guards the
        // very first frame, where the window size isn't known yet. The shared
        // drawer owns the breakpoint; at/above it the sidebar docks, below it it
        // overlays. Entering narrow starting closed is handled by the drawer's
        // own breakpoint reconciliation (in `resolve`, below).
        let width = self.rect.width();
        let want = width > 0.0
            && matches!(
                self.drawer.layout_for(width),
                ui::widget::SplitLayout::Overlay
            );
        if want != self.want_narrow {
            self.want_narrow = want;
            self.self_dirty = true;
        }
        // If the desired mode drifted from what's actually built, ask the
        // host to rebuild the tree on the next frame.
        if self.want_narrow != self.built_narrow {
            self.pending_relayout = true;
            self.self_dirty = true;
        }

        // The content row sits inside the root column; sidebar + main
        // are children of it. Each child widget's `apply_layout` adds
        // its own `layout.location` to the origin we pass, so we need
        // to hand it the *parent's* absolute origin.
        if let Some(content_node) = self.content_node {
            let content_layout = taffy.layout(content_node).unwrap();
            let content_origin = Point::new(
                new_origin.x + f64::from(content_layout.location.x),
                new_origin.y + f64::from(content_layout.location.y),
            );
            self.content_rect = Rect::new(
                content_origin.x,
                content_origin.y,
                content_origin.x + f64::from(content_layout.size.width),
                content_origin.y + f64::from(content_layout.size.height),
            );
            // Feed the fresh content rect to the shared drawer so its scrim
            // geometry and breakpoint memory track this frame (this is where a
            // stale open/closed preference is reconciled on a breakpoint cross).
            self.drawer.resolve(self.content_rect);
            if let Some(n) = self.sidebar_node {
                self.sidebar.apply_layout(taffy, n, content_origin);
            }
            if let Some(n) = self.main_node {
                self.main.apply_layout(taffy, n, content_origin);
            }
        }

        // Top bar (narrow only) and its drawer-toggle button rect.
        if let Some(n) = self.topbar_node {
            let l = taffy.layout(n).unwrap();
            self.topbar_rect = Rect::new(
                new_origin.x + f64::from(l.location.x),
                new_origin.y + f64::from(l.location.y),
                new_origin.x + f64::from(l.location.x) + f64::from(l.size.width),
                new_origin.y + f64::from(l.location.y) + f64::from(l.size.height),
            );
            self.menu_btn_rect = Rect::new(
                self.topbar_rect.x0,
                self.topbar_rect.y0,
                self.topbar_rect.x0 + TOPBAR_HEIGHT,
                self.topbar_rect.y1,
            );
        } else {
            self.topbar_rect = Rect::ZERO;
            self.menu_btn_rect = Rect::ZERO;
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
        // Editor always paints first; on a narrow window the drawer (if
        // open) composites over it below.
        self.main
            .draw(scene, transform, text_renderer, theme, presence);
        if self.built_narrow {
            self.draw_topbar(scene, transform, text_renderer, theme);
            if self.drawer.is_open() {
                // The shared drawer paints the (subtle) dimming scrim plus a
                // soft edge shadow behind the page tree, which draws on top.
                self.drawer.draw_scrim(scene, transform);
                self.sidebar
                    .draw(scene, transform, text_renderer, theme, presence);
            }
        } else {
            self.sidebar
                .draw(scene, transform, text_renderer, theme, presence);
        }
        // Transient floating status bubble (bottom-right), in place of the
        // old permanent bottom status strip.
        self.draw_status_bubble(scene, transform, text_renderer, theme);
        // Page-jump palette renders on top of everything.
        if self.palette.is_visible() {
            // Dim backdrop — `surfaces.overlay` is `bg-black/80` (the
            // modal scrim alpha). Reuse it rather than
            // hand-rolling a colour literal.
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.overlay,
                None,
                &self.rect,
            );
            self.palette
                .draw_overlay(scene, transform, text_renderer, theme);
        }
        // Shortcuts overlay sits above every other layer when
        // visible. Drawn last so it covers the palette too.
        self.shortcuts
            .draw_overlay(scene, transform, text_renderer, theme);
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming only while the floating status bubble is lingering
        // / fading, so it hides itself without needing a stray pointer event.
        // Idle otherwise (no idle redraw spin).
        self.status_bubble.needs_frame(std::time::Instant::now())
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
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if self.handle_shortcuts_overlay(event, scale_factor, handled) {
            return;
        }
        if self.handle_help_key(event, handled) {
            return;
        }
        if self.handle_palette_shortcut(event, *modifiers, focus_target, node_id, handled) {
            return;
        }
        if self.handle_new_child_shortcut(event, *modifiers, state, handled) {
            return;
        }
        if self.handle_palette_capture(event, focus_target, handled) {
            return;
        }
        if self.handle_narrow_drawer(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
            taffy,
        ) {
            return;
        }
        if self.handle_link_click(event, handled) {
            return;
        }
        self.route_to_children(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
            taffy,
        );
    }

    fn name(&self) -> &'static str {
        "NotesApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        self.palette.is_visible()
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.palette
            .is_visible()
            .then(|| Widget::ime_cursor_rect(&self.palette))
            .flatten()
    }

    fn needs_redraw(&self) -> bool {
        self.self_dirty
            || self.sidebar.needs_redraw()
            || self.main.needs_redraw()
            || self.shortcuts.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.self_dirty = false;
        self.sidebar.mark_drawn();
        self.main.mark_drawn();
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

impl NotesApp {
    /// Style for the sidebar node depending on the narrow/wide layout mode.
    /// Narrow: an absolutely-positioned overlay drawer pinned to the left
    /// edge of the content row, full height, taken out of flow so the
    /// editor keeps the whole width underneath it. Wide: a fixed-width
    /// column so the split is deterministic (no flexbox squeezing).
    fn sidebar_style(narrow: bool) -> Style {
        if narrow {
            Style {
                position: taffy::Position::Absolute,
                inset: taffy::Rect {
                    left: length(0.0_f32),
                    right: taffy::LengthPercentageAuto::auto(),
                    top: length(0.0_f32),
                    bottom: length(0.0_f32),
                },
                size: Size {
                    width: Dimension::length(num::f32_of_f64(DRAWER_WIDTH)),
                    height: Dimension::percent(1.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            }
        } else {
            Style {
                size: Size {
                    width: Dimension::length(num::f32_of_f64(SIDEBAR_WIDTH)),
                    height: Dimension::percent(1.0),
                },
                min_size: Size {
                    width: Dimension::length(160.0),
                    height: Dimension::auto(),
                },
                flex_shrink: 0.0,
                ..Default::default()
            }
        }
    }

    /// Assemble the outer flex column: an optional top bar (narrow only)
    /// above the content row. No status-bar row — the note's status floats
    /// as a transient bubble (see `draw_status_bubble`), so the content
    /// column spans to the window bottom and reclaims the strip's height.
    fn assemble_root(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        narrow: bool,
        content_node: NodeId,
    ) -> NodeId {
        let mut column_children: Vec<NodeId> = Vec::with_capacity(3);
        if narrow {
            #[allow(
                clippy::unwrap_used,
                reason = "new_leaf on a fresh style cannot fail — see ui::widget::TaffyTreeExt"
            )]
            let topbar_node = taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::length(num::f32_of_f64(TOPBAR_HEIGHT)),
                    },
                    flex_shrink: 0.0,
                    ..Default::default()
                })
                .unwrap();
            self.topbar_node = Some(topbar_node);
            column_children.push(topbar_node);
        } else {
            self.topbar_node = None;
        }
        column_children.push(content_node);

        #[allow(
            clippy::unwrap_used,
            reason = "new_with_children on freshly-built children cannot fail — see ui::widget::TaffyTreeExt"
        )]
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
                        width: length(0.0_f32),
                        height: length(0.0_f32),
                    },
                    ..Default::default()
                },
                &column_children,
            )
            .unwrap();
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    /// The shortcuts overlay is fully blocking when visible: it either
    /// consumes the key itself, or every other event is swallowed so
    /// nothing reaches the editor underneath.
    fn handle_shortcuts_overlay(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        handled: &mut bool,
    ) -> bool {
        if !self.shortcuts.is_visible() {
            return false;
        }
        if self.shortcuts.handle_key(event, scale_factor) {
            *handled = true;
            self.refresh_status();
            return true;
        }
        *handled = true;
        true
    }

    /// `?` opens the shortcuts overlay — but ONLY when a note isn't being
    /// typed into. A notes app is a text editor; if `?` were a global
    /// shortcut the character could never be typed into a note. So it's
    /// gated on the editor NOT holding focus (and the palette being
    /// closed, so `?` still filters there). When the editor is focused the
    /// key falls through to the block editor and types a literal `?`; when
    /// the page tree / nothing is focused, `?` opens help. This is the
    /// standard "help key only outside inputs" behaviour.
    fn handle_help_key(&mut self, event: &WindowEvent, handled: &mut bool) -> bool {
        if self.palette.is_visible() || self.main.editor().is_focused() {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        if c.as_str() != "?" {
            return false;
        }
        self.shortcuts.set_visible(true);
        *handled = true;
        self.refresh_status();
        true
    }

    /// Cmd-K / Ctrl-K opens the page jump palette. Intercepted BEFORE
    /// child widgets so a focused input doesn't swallow the keystroke.
    fn handle_palette_shortcut(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        handled: &mut bool,
    ) -> bool {
        if self.palette.is_visible() {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let mods = modifiers.state();
        if !(mods.control_key() || mods.super_key()) || c.to_lowercase().as_str() != "k" {
            return false;
        }
        let anchor_x = f64::midpoint(self.rect.x0, self.rect.x1) - self.palette.width() / 2.0;
        let anchor_y = self.rect.y0 + 80.0;
        self.palette
            .set_anchor(vello::kurbo::Point::new(anchor_x, anchor_y));
        self.palette.set_visible(true);
        *focus_target = Some(node_id);
        *handled = true;
        true
    }

    /// Cmd-N / Ctrl-N creates a child of the selected page. Handled at the
    /// app level — not just in the sidebar — so it works no matter which
    /// pane holds focus. The editor takes focus on load, so a
    /// sidebar-only handler (gated on the tree being focused) would
    /// silently no-op, making the welcome copy "Cmd-N adds a page under
    /// the selection" a lie. `N` is modifier-qualified, so it never
    /// conflicts with typing into a note.
    fn handle_new_child_shortcut(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) -> bool {
        if self.palette.is_visible() {
            return false;
        }
        let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Character(c),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        let mods = modifiers.state();
        if !(mods.control_key() || mods.super_key()) || c.to_lowercase().as_str() != "n" {
            return false;
        }
        let parent = self.workspace.selected();
        self.apply_and_dispatch(state, OutEvent::CreateChild { parent });
        self.sidebar.sync_from_workspace(&self.workspace);
        *handled = true;
        true
    }

    /// While the palette is visible it owns ALL keystrokes.
    fn handle_palette_capture(
        &mut self,
        event: &WindowEvent,
        focus_target: &mut Option<NodeId>,
        handled: &mut bool,
    ) -> bool {
        if !self.palette.is_visible()
            || !matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            )
        {
            return false;
        }
        let consumed = self.palette.handle_key(event);
        if let Some(entry) = self.palette.take_selection() {
            self.navigate_to(entry.id);
        }
        if !consumed {
            return false;
        }
        if !self.palette.is_visible() {
            *focus_target = self.main_node;
        }
        *handled = true;
        true
    }

    /// Narrow-mode overlay drawer. The top-bar button toggles it; while
    /// open it captures pointer/keys so the page tree behaves like a
    /// modal flyout over the editor.
    #[allow(
        clippy::too_many_arguments,
        reason = "forwards the exact Widget::handle_event context bundle"
    )]
    fn handle_narrow_drawer(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if !self.built_narrow {
            return false;
        }
        // Toggle button — hit-test the hamburger in the top bar.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && self.menu_btn_rect.contains(self.last_mouse)
        {
            self.set_drawer_open(!self.drawer.is_open());
            *handled = true;
            return true;
        }
        if !self.drawer.is_open() {
            return false;
        }
        // Escape closes the drawer.
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            self.set_drawer_open(false);
            *handled = true;
            return true;
        }
        // A press on the scrim (outside the drawer) closes it.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
            && !self.sidebar.get_rect().contains(self.last_mouse)
        {
            self.set_drawer_open(false);
            *handled = true;
            return true;
        }
        // Route to the page tree; auto-close once a page is picked.
        let sel_before = self.workspace.selected();
        if let Some(n) = self.sidebar_node {
            self.sidebar.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
        }
        self.drain_sidebar_events(state);
        if self.workspace.selected() != sel_before {
            self.set_drawer_open(false);
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        self.refresh_status();
        true
    }

    /// Click on a link mark navigates.
    fn handle_link_click(&mut self, event: &WindowEvent, handled: &mut bool) -> bool {
        let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return false;
        };
        let Some(id) = self.link_under_last_mouse() else {
            return false;
        };
        self.navigate_to(id);
        *handled = true;
        true
    }

    /// The default routing once none of the modal overlays above claimed
    /// the event: sidebar + editor, then the title-rename / dirty-save /
    /// last-mouse / status bookkeeping every event needs.
    #[allow(
        clippy::too_many_arguments,
        reason = "forwards the exact Widget::handle_event context bundle"
    )]
    fn route_to_children(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        taffy: &TaffyTree<()>,
    ) {
        // Modal focus capture: while the editor's `/` block menu (or the
        // `[[` link popover) is open, keyboard nav belongs to it — not the
        // page list behind it; symmetrically, while the sidebar's
        // right-click menu is open, keyboard nav belongs to the menu, not
        // the editor. Mouse events still reach both panes (a click only
        // ever hits one pane's rect), so we only gate keyboard routing.
        let is_keyboard = matches!(event, WindowEvent::KeyboardInput { .. });
        let editor_menu_open = self.main.editor().slash_menu_visible()
            || self.main.editor().link_popover().is_visible();
        let sidebar_menu_open = self.sidebar.menu_is_open();

        // Route to the inline sidebar only when it's actually on screen
        // (wide mode). In narrow mode the drawer block above owns it.
        if !self.built_narrow && (!editor_menu_open || !is_keyboard) {
            if let Some(n) = self.sidebar_node {
                self.sidebar.handle_event(
                    event,
                    scale_factor,
                    &modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    n,
                    taffy,
                );
            }
            // Drain any commands the sidebar produced.
            self.drain_sidebar_events(state);
        }
        // The editor never sees keystrokes the sidebar is capturing — its
        // right-click menu OR an in-progress inline rename — so a rename
        // keystroke doesn't also type into the note behind the field.
        let sidebar_capturing_kbd = sidebar_menu_open || self.sidebar.is_renaming();
        if !(sidebar_capturing_kbd && is_keyboard)
            && let Some(n) = self.main_node
        {
            self.main.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                n,
                taffy,
            );
        }
        // A committed edit of the note's in-page title renames the current
        // page on the same replayable action path the sidebar's inline rename
        // uses, then re-syncs the sidebar so the new name shows in the tree.
        if let Some(new_title) = self.main.take_title_rename()
            && let Some(id) = self.workspace.selected()
        {
            let trimmed = new_title.trim();
            let changed = self.workspace.get(id).is_some_and(|m| m.title != trimmed);
            if !trimmed.is_empty() && changed {
                self.apply_and_dispatch(
                    state,
                    OutEvent::Rename {
                        id,
                        new_title: trimmed.to_string(),
                    },
                );
                self.sidebar.sync_from_workspace(&self.workspace);
            }
        }
        // After every event, observe the editor's dirty flag and
        // request a debounced save if the user just edited.
        self.drain_editor_edits();

        // Update the last-mouse cache so the next MouseInput knows
        // where to hit-test for link clicks.
        if let WindowEvent::CursorMoved { position, .. } = event {
            let logical =
                vello::kurbo::Point::new(position.x / scale_factor, position.y / scale_factor);
            self.last_mouse = logical;
        }
        // Status bar refresh runs at the tail of every event so word
        // count + save indicator stay current. Cheap — the StatusBar
        // diffs internally.
        self.refresh_status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn spawn_starts_empty_by_default() {
        // The live default (`spawn` / `spawn_default`) must NOT fabricate a
        // page: a fresh workspace opens empty onto the "New note" affordance.
        let tmp = TempDir::new().unwrap();
        let app = NotesApp::spawn(tmp.path().into()).unwrap();
        assert!(
            app.workspace.pages().is_empty(),
            "a fresh launch seeds no pages"
        );
        assert!(
            app.workspace.selected().is_none(),
            "nothing selected on an empty workspace"
        );
        assert!(app.main.title().is_none(), "no page bound in the editor");
    }

    #[test]
    fn spawn_with_starter_page_seeds_welcome() {
        // The explicit test/dev fixture constructor still seeds a starter page.
        let tmp = TempDir::new().unwrap();
        let app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        assert_eq!(app.workspace.pages().len(), 1);
        assert_eq!(app.workspace.pages()[0].title, "Welcome");
        // Welcome page file exists on disk.
        let id = app.workspace.pages()[0].id;
        assert!(storage::page_file_path(tmp.path(), id).exists());
    }

    #[test]
    fn apply_event_create_child_appends_and_selects() {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let root = app.workspace.pages()[0].id;
        app.apply_event(OutEvent::CreateChild { parent: Some(root) });
        let children = app.workspace.children_of(Some(root));
        assert_eq!(children.len(), 1);
        assert_eq!(app.workspace.selected(), Some(children[0].id));
    }

    #[test]
    fn apply_event_delete_removes_file_and_resets_selection() {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
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
    fn combine_save_status_is_worst_of() {
        use SaveStatus::*;
        // Failed dominates every combination.
        assert_eq!(combine_save_status(Failed, Saved), Failed);
        assert_eq!(combine_save_status(Saved, Failed), Failed);
        assert_eq!(combine_save_status(Failed, Saving), Failed);
        assert_eq!(combine_save_status(Saving, Failed), Failed);
        assert_eq!(combine_save_status(Failed, Failed), Failed);
        // Saving wins over Saved when nothing failed.
        assert_eq!(combine_save_status(Saving, Saved), Saving);
        assert_eq!(combine_save_status(Saved, Saving), Saving);
        assert_eq!(combine_save_status(Saving, Saving), Saving);
        // Only both-clean reports Saved.
        assert_eq!(combine_save_status(Saved, Saved), Saved);
    }

    #[test]
    fn rename_updates_main_panel_title() {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        let root = app.workspace.pages()[0].id;
        app.apply_event(OutEvent::Rename {
            id: root,
            new_title: "Index".into(),
        });
        assert_eq!(app.main.title(), Some("Index"));
    }

    /// A fresh app whose seeded "Welcome" page is mirrored into `state.doc`, with
    /// the seed-grace window collapsed so the first `sync` seeds immediately.
    fn app_seeded_into_doc() -> (NotesApp, ui::State, TempDir, PageId) {
        let tmp = TempDir::new().unwrap();
        let mut app = NotesApp::spawn_with_starter_page(tmp.path().into()).unwrap();
        app.bypass_seed_grace_for_test();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let welcome = app
            .selected_for_test()
            .expect("welcome page seeded + selected");
        (app, state, tmp, welcome)
    }

    /// One source of truth: the sidebar's Delete event, dispatched through the
    /// real app path, encodes the byte-identical [`NotesAction`] the Ctrl+K
    /// palette's "Delete page" spec does.
    #[test]
    fn sidebar_delete_and_palette_encode_identically() {
        let (mut app, mut state, _tmp, welcome) = app_seeded_into_doc();

        // Palette side: the welcome page resource's delete spec (built before we
        // delete the page, while it still exists in the tree).
        let resources = Widget::resources(&app, &state.doc);
        let page = resources[0]
            .children
            .iter()
            .find(|r| r.label == "Welcome")
            .expect("a Welcome page resource");
        let delete_spec = page.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_palette = match &delete_spec.input {
            qos_actions::ActionInput::Immediate(enc) => enc.clone(),
            qos_actions::ActionInput::Prompted { .. } => panic!("delete is an immediate action"),
        };

        // Affordance side: the sidebar's Delete event through the app path.
        app.dispatch_event_for_test(&mut state.doc, OutEvent::Delete(welcome));
        let from_button = app
            .last_dispatched_for_test()
            .expect("the sidebar delete dispatched an action")
            .clone();

        assert_eq!(from_palette.type_id, from_button.type_id);
        assert_eq!(
            from_palette.payload, from_button.payload,
            "the sidebar affordance and the palette build the same EncodedAction"
        );
    }

    /// Creating a page through the sidebar path dispatches an `UpsertPage` that
    /// lands in the shared document — the create crosses the action executor,
    /// not a private mutation.
    #[test]
    fn creating_a_page_dispatches_an_upsert_into_the_doc() {
        let (mut app, mut state, _tmp, _welcome) = app_seeded_into_doc();
        let before = NotesDoc::list_pages(&state.doc).len();

        app.dispatch_event_for_test(&mut state.doc, OutEvent::CreateChild { parent: None });

        assert_eq!(
            NotesDoc::list_pages(&state.doc).len(),
            before + 1,
            "the create wrote a new page into the shared document"
        );
        let enc = app
            .last_dispatched_for_test()
            .expect("an action was dispatched");
        assert_eq!(enc.type_id, crate::actions::NOTES_ACTION_TYPE);
        let action: crate::actions::NotesAction =
            serde_json::from_slice(&enc.payload).expect("payload decodes");
        assert!(
            matches!(action, crate::actions::NotesAction::UpsertPage(_)),
            "a create dispatches an UpsertPage"
        );
    }

    /// The full undo covenant at the app level: deleting a page through the
    /// sidebar path records exactly one undo boundary, and Ctrl+Z (the host's
    /// `undo`) reverts it in both the document and — after the next `sync`
    /// reconciles — the app's own page list.
    #[test]
    fn page_delete_records_one_undo_boundary_and_reverts_in_the_app() {
        let (mut app, mut state, _tmp, welcome) = app_seeded_into_doc();
        assert!(
            NotesDoc::list_pages(&state.doc)
                .iter()
                .any(|p| p.id == welcome)
        );

        // Delete through the real sidebar-event path.
        app.dispatch_event_for_test(&mut state.doc, OutEvent::Delete(welcome));

        // The host's own recorder drains the captured heads and records the
        // boundary. Going through it rather than pushing an entry by hand is
        // the point: attribution happens in there, and undo is author-scoped.
        // The node map is empty because the app widget is the root here.
        let local_author = ui::collaboration::local_user_id();
        let root: *mut dyn Widget = &mut app;
        assert!(
            ui::undo::record_dispatched_action(
                &mut state,
                root,
                &std::collections::HashMap::new(),
                &local_author,
            ),
            "the host recorded an undo boundary for the dispatched delete"
        );

        assert!(
            !NotesDoc::list_pages(&state.doc)
                .iter()
                .any(|p| p.id == welcome),
            "the delete removed the page from the document"
        );
        let entry = state
            .peek_last_undo()
            .expect("exactly one undo boundary recorded");
        assert_eq!(
            entry.author.as_ref(),
            Some(&local_author),
            "attributed to the local user, so their Ctrl+Z can reach it"
        );
        assert!(
            Widget::take_committed_action(&mut app).is_none(),
            "the boundary is rising-edge — cleared on read"
        );

        // Ctrl+Z reverts the document; the next `sync` reconciles the workspace.
        state
            .undo_as(Some(&local_author), None, None)
            .expect("an undo entry to pop");
        assert!(
            NotesDoc::list_pages(&state.doc)
                .iter()
                .any(|p| p.id == welcome),
            "undo restored the page in the document"
        );
        app.sync(&mut state.doc, &state.root);
        assert!(
            app.workspace_pages_for_test()
                .iter()
                .any(|p| p.id == welcome),
            "the app shows the page again after undo"
        );
    }
}
