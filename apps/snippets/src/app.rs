//! Root widget for the snippet manager.
//!
//! Owns the in-memory [`Library`], the per-peer view state (selection,
//! search, focus), and renders the three-pane layout directly into the
//! Vello scene:
//!
//! ```text
//! ┌──────────┬──────────────────┬─────────────────────────────┐
//! │ sidebar  │  snippet list    │  detail / editor pane       │
//! │ folders  │  (search above)  │  title · language · tags    │
//! │ tags     │                  │  syntax-highlighted body    │
//! └──────────┴──────────────────┴─────────────────────────────┘
//! ```
//!
//! The library is mirrored into the shared Automerge document by
//! [`crate::doc`] so two instances converge. Selection / search / the
//! open expand-modal are view state and stay local to each peer.

use std::collections::HashMap;

use automerge::{AutoCommit, ChangeHash, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Button, ButtonFace, ButtonStyle, FlapState, PushPane, SplitLayout, TextInput, Widget,
};

use crate::actions::{self, SnippetsAction, SnippetsResourceCtx};
use crate::doc::SnippetsDoc;
use crate::draw;
use crate::expand::ExpandModal;
use crate::highlight;
use crate::model::{Language, Library, Selection, SmartFolder, Snippet, SnippetId};
use crate::seed;

const SIDEBAR_W: f64 = 232.0;
const LIST_W: f64 = 340.0;
const ROW_H: f64 = 64.0;
const SIDEBAR_ROW_H: f64 = 32.0;
const SEARCH_H: f64 = 52.0;
const BODY_FONT: f32 = 13.5;
const BODY_LINE: f64 = 20.0;

/// Below this allocated width (logical px) the primary sidebar (smart
/// folders + folders + tags) stops sitting beside the list and detail and
/// collapses into an overlay flap that slides over the content behind a
/// dimming scrim — closed by default, summoned by the hamburger. Chosen so
/// the 232px rail + 340px list + a readable detail pane stop fitting
/// comfortably side by side.
const SIDEBAR_BREAKPOINT: f64 = 820.0;

/// Below this width even the list and the detail can't share the row, so the
/// master-detail flow switches to single-pane push: the list OR the detail
/// fills the pane, and opening a snippet slides the detail in with a Back
/// affordance. Set so the 340px list + a usable code card stop coexisting.
const PUSH_BREAKPOINT: f64 = 560.0;

/// Top-left toggle (hamburger / Back / close) hit size, logical px.
const TOGGLE_SIZE: f64 = 34.0;

/// What kind of toast is currently showing (auto-dismisses on next event
/// tick after a short hold — for the headless shot it simply persists).
#[derive(Clone)]
struct Toast {
    message: String,
    /// Frames remaining before the toast fades. Decremented on idle.
    ttl: u32,
}

pub struct SnippetsApp {
    library: Library,
    selection: Selection,
    /// Local, per-peer search editor. It intentionally is not CRDT-bound:
    /// filtering is presentation state, while snippet mutations still flow
    /// through [`SnippetsDoc`].
    search_input: TextInput,
    search_rect: Rect,
    /// Currently open snippet in the detail pane.
    open: Option<SnippetId>,
    /// Ordered ids currently shown in the list pane (result of `query`).
    visible: Vec<SnippetId>,
    /// The expand-with-variables modal, when open.
    expand: Option<ExpandModal>,
    toast: Option<Toast>,
    /// Whether we've mirrored the local library into the shared doc.
    seeded: bool,

    // Layout / hit-testing caches, recomputed each frame in `draw` so
    // `handle_event` can map clicks to actions without a second layout
    // pass. Each entry is (rect, action).
    rect: Rect,
    last_mouse: Point,
    hit: Vec<(Rect, Action)>,
    scroll: f64,

    // ---- responsive layout (shared `ui` flap kernel) -------------------
    /// Primary sidebar flap: docked beside the content on wide windows,
    /// an overlay flap over the content behind a scrim on narrow ones.
    /// Defaults open when docked, closed (summon-on-demand) when overlay.
    sidebar: FlapState,
    /// Single-pane master-detail push for the very-narrow case: animates
    /// the list↔detail slide. `reveal == 1` means the detail is shown.
    push: FlapState,
    /// Which pane the push flow currently shows (list = master, detail).
    push_pane: PushPane,
    /// `false` until the first laid-out frame snaps the sidebar reveal to the
    /// resolved layout default. Without this, a window that first appears at
    /// narrow width would render one frame with the docked-default reveal
    /// (1.0) before the animation ticks it closed — the overlay would flash
    /// open. A staged `--shots` scenario sets this so its snap isn't undone.
    settled_layout: bool,

    /// Framework-owned action dispatch. Each on-screen affordance dispatches its
    /// [`SnippetsAction`] through this: the host brackets the undo boundary,
    /// applies the action through the executor (so it replays byte-identically on
    /// peers, exactly as the Ctrl+K palette does), and records the last one so a
    /// test can prove a button and its [`qos_actions::ActionSpec`] build the
    /// identical action.
    actions: ui::ActionHost,
}

/// A clickable region recorded during draw and resolved on mouse-down.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Select(Selection),
    OpenSnippet(SnippetId),
    NewSnippet,
    Copy(SnippetId),
    Expand(SnippetId),
    ToggleFavorite(SnippetId),
    Duplicate(SnippetId),
    Delete(SnippetId),
    FocusSearch,
    ClearSearch,
    SetLanguage(SnippetId, Language),
    /// Toggle the primary sidebar flap (hamburger / overlay close).
    ToggleSidebar,
    /// Dismiss the open overlay sidebar (scrim tap).
    DismissSidebar,
    /// Return from the detail pane to the list in single-pane push mode.
    PushBack,
}

impl SnippetsApp {
    /// Build the live app on an **empty** library. A fresh install has no
    /// snippets — the user saves their own, or a peer's shared library is
    /// adopted on the first `sync`. The starter set is demo/`--shots` only
    /// (see [`Self::new_demo`]): a real install must never fabricate
    /// snippets it would then persist to the user's store.
    pub fn new() -> Self {
        Self::with_library(Library::new())
    }

    /// Like [`Self::new`] but pre-seeded with the starter library and an
    /// opened specimen — used by `--shots` and any "try it" walkthrough so a
    /// capture shows the manager doing real work. Never the live first-run
    /// path.
    pub fn new_demo() -> Self {
        let mut library = Library::new();
        for s in seed::starter_library() {
            library.insert(s);
        }
        let mut app = Self::with_library(library);
        // Open a substantial, templated snippet so the detail pane shows
        // off the highlighter, line gutter and variable affordance. Prefer
        // the visible snippet with the most body lines that also carries
        // placeholders.
        app.open = app
            .visible
            .iter()
            .copied()
            .max_by_key(|id| {
                let s = app.library.get(*id);
                let lines = s.map_or(0, |s| s.body.lines().count());
                let templated = usize::from(s.is_some_and(super::model::Snippet::has_placeholders));
                (templated, lines)
            })
            .or_else(|| app.visible.first().copied());
        app
    }

    fn with_library(library: Library) -> Self {
        let mut app = Self {
            library,
            selection: Selection::default(),
            search_input: TextInput::local()
                .with_kind(ui::widget::TextInputKind::Search)
                .with_id("snippets.search")
                .with_placeholder("Search  ·  try language:rust  or  tag:k8s")
                .with_font_size(14.0)
                .with_plain_chrome(),
            search_rect: Rect::ZERO,
            open: None,
            visible: Vec::new(),
            expand: None,
            toast: None,
            seeded: false,
            rect: Rect::ZERO,
            last_mouse: Point::ZERO,
            hit: Vec::new(),
            scroll: 0.0,
            sidebar: FlapState::open_when_docked(),
            push: FlapState::closed_by_default(),
            push_pane: PushPane::Master,
            settled_layout: false,
            actions: ui::ActionHost::new(),
        };
        app.recompute_visible();
        app
    }

    /// Seeded constructor for the screenshot harness.
    pub fn for_shot() -> Self {
        Self::new_demo()
    }

    fn recompute_visible(&mut self) {
        self.visible = self
            .library
            .query(&self.selection, self.search_input.value());
        // Keep the open snippet valid; otherwise open the first result.
        if let Some(open) = self.open {
            if !self.visible.contains(&open) {
                self.open = self.visible.first().copied();
            }
        } else {
            self.open = self.visible.first().copied();
        }
    }

    fn select(&mut self, sel: Selection) {
        self.selection = sel;
        self.scroll = 0.0;
        self.recompute_visible();
    }

    fn open_snippet(&mut self, id: SnippetId) {
        self.open = Some(id);
        // In single-pane push mode, opening a snippet slides the detail in.
        // (No-op in docked / overlay layouts, where both panes are visible.)
        if self.is_push_layout() {
            self.push_pane = PushPane::Detail;
            self.push.open();
        }
    }

    /// Dispatch one [`SnippetsAction`] — the single mutation path shared with the
    /// Ctrl+K palette. The action runs through the qOS executor (a pinned actor +
    /// committed change, so it replays byte-identically on peers), the shared
    /// document being authoritative; the in-memory library is then re-projected
    /// from it so the panes reflect the change this frame. The heads captured
    /// just before the first action of the event are stashed so the host records
    /// **one** undo boundary bracketing it.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &SnippetsAction) {
        self.actions.dispatch_with_context(
            doc,
            actions::SNIPPETS_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.Snippets", "Snippets", action),
        );
        self.refresh_from_doc(doc);
    }

    /// Re-project the authoritative document into the in-memory library. Called
    /// after every dispatched action so the list/detail reflect it immediately,
    /// before the next frame's `sync`.
    fn refresh_from_doc(&mut self, doc: &AutoCommit) {
        self.library.snippets = SnippetsDoc::list(doc);
    }

    /// The presentation context Ctrl+K needs to build the library-level "New
    /// snippet" action (which folder to file it under).
    pub(crate) fn snippets_resource_ctx(&self) -> SnippetsResourceCtx {
        SnippetsResourceCtx {
            current_folder: match &self.selection {
                Selection::Folder(f) => f.clone(),
                _ => String::new(),
            },
        }
    }

    fn new_snippet(&mut self, doc: &mut AutoCommit) {
        let folder = match &self.selection {
            Selection::Folder(f) => f.clone(),
            _ => String::new(),
        };
        // The uuid is minted here, at authoring time, and carried in the action
        // payload so replay reproduces the identical row.
        let s = Snippet::blank(&folder);
        let id = s.id;
        self.perform_action(doc, &actions::create_snippet_action(s));
        self.recompute_visible();
        self.open = Some(id);
        self.toast("New snippet created");
    }

    fn copy(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        // If the body has placeholders, route through the expand flow so
        // we never paste raw `${...}` onto the clipboard.
        let needs_expand = self
            .library
            .get(id)
            .is_some_and(super::model::Snippet::has_placeholders);
        if needs_expand {
            self.start_expand(id);
            return;
        }
        // Copying to the clipboard is presentation; the usage bump it records is
        // a document mutation, so it goes through an action.
        if let Some(body) = self.library.get(id).map(|s| s.body.clone()) {
            ui::clipboard_copy(&body);
            self.record_use(id, doc);
            self.toast("Copied to clipboard");
        }
    }

    /// Record one use of a snippet (drives "Most Used" + the usage hint) through
    /// the action path, so the bump replicates and is undoable like any mutation.
    fn record_use(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.perform_action(doc, &actions::record_use_action(id));
    }

    fn start_expand(&mut self, id: SnippetId) {
        if let Some(s) = self.library.get(id) {
            self.expand = Some(ExpandModal::new(s));
        }
    }

    fn toggle_favorite(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        let cur = self.library.get(id).is_some_and(|s| s.favorite);
        self.perform_action(doc, &actions::set_favorite_action(id, !cur));
        self.recompute_visible();
    }

    fn duplicate(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        if let Some(src) = self.library.get(id).cloned() {
            let mut copy = src.clone();
            copy.id = uuid::Uuid::new_v4();
            copy.title = format!("{} (copy)", src.title);
            copy.favorite = false;
            copy.uses = 0;
            let new_id = copy.id;
            self.perform_action(doc, &actions::create_snippet_action(copy));
            self.recompute_visible();
            self.open = Some(new_id);
            self.toast("Duplicated");
        }
    }

    fn delete(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.perform_action(doc, &actions::delete_snippet_action(id));
        if self.open == Some(id) {
            self.open = None;
        }
        self.recompute_visible();
        self.toast("Deleted");
    }

    fn set_language(&mut self, id: SnippetId, lang: Language, doc: &mut AutoCommit) {
        self.perform_action(doc, &actions::set_language_action(id, lang));
        self.recompute_visible();
    }

    fn toast(&mut self, message: &str) {
        self.toast = Some(Toast {
            message: message.to_string(),
            ttl: 90,
        });
    }

    /// Reconcile the in-memory library with the shared document.
    ///
    /// The document is the source of truth: every mutation flows through a
    /// dispatched [`SnippetsAction`] (see [`crate::actions`]). On first sync we
    /// either seed our starter set into an empty document or adopt a peer's; from
    /// then on we re-project the document so a peer's change — and our own
    /// just-dispatched action — both surface. Guarded so an unchanged frame
    /// doesn't churn the visible list.
    fn sync_library(&mut self, doc: &mut AutoCommit) {
        let remote = SnippetsDoc::list(doc);
        if !self.seeded {
            if remote.is_empty() {
                // We're the origin — seed our starter set into the document,
                // which becomes authoritative from here on.
                for s in self.library.snippets.clone() {
                    SnippetsDoc::upsert(doc, &s);
                }
            } else {
                // A peer is already sharing — adopt their library.
                self.library.snippets = remote;
                self.recompute_visible();
            }
            self.seeded = true;
            return;
        }
        // Steady state: the document is authoritative — re-project it.
        if self.library.snippets != remote {
            self.library.snippets = remote;
            self.recompute_visible();
        }
    }

    fn apply(&mut self, action: Action, doc: &mut AutoCommit) {
        match action {
            Action::Select(sel) => self.select(sel),
            Action::OpenSnippet(id) => self.open_snippet(id),
            Action::NewSnippet => self.new_snippet(doc),
            Action::Copy(id) => self.copy(id, doc),
            Action::Expand(id) => self.start_expand(id),
            Action::ToggleFavorite(id) => self.toggle_favorite(id, doc),
            Action::Duplicate(id) => self.duplicate(id, doc),
            Action::Delete(id) => self.delete(id, doc),
            Action::FocusSearch => self.search_input.set_focus(true),
            Action::ClearSearch => {
                self.search_input.set_text("");
                self.recompute_visible();
            }
            Action::SetLanguage(id, lang) => self.set_language(id, lang, doc),
            Action::ToggleSidebar => self.sidebar.toggle(self.sidebar_layout()),
            Action::DismissSidebar => self.sidebar.close(),
            Action::PushBack => {
                self.push_pane = PushPane::Master;
                self.push.close();
            }
        }
    }

    /// Resolve a click at `p` against the recorded hit regions. Topmost
    /// (last-recorded) match wins.
    fn action_at(&self, p: Point) -> Option<Action> {
        self.hit
            .iter()
            .rev()
            .find(|(r, _)| r.contains(p))
            .map(|(_, a)| a.clone())
    }

    fn record(&mut self, rect: Rect, action: Action) {
        self.hit.push((rect, action));
    }

    // ---- responsive geometry -------------------------------------------

    /// How the primary sidebar resolves at the current allocated width:
    /// docked beside the content (wide) or an overlay flap (narrow).
    fn sidebar_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), SIDEBAR_BREAKPOINT)
    }

    /// Whether the window is narrow enough that the list and detail switch
    /// to single-pane push navigation (show one or the other).
    fn is_push_layout(&self) -> bool {
        self.rect.width() < PUSH_BREAKPOINT
    }

    /// `true` while the sidebar flap is (resolved) open.
    fn sidebar_open(&self) -> bool {
        self.sidebar.is_open(self.sidebar_layout())
    }

    /// The sidebar's on-screen rect for this frame. Docked: a fixed rail at
    /// the left whose width eases with the reveal. Overlay: a fixed-width
    /// rail that slides in from the left edge over the content.
    fn sidebar_rect(&self) -> Rect {
        let r = self.rect;
        match self.sidebar_layout() {
            SplitLayout::Docked => {
                let w = SIDEBAR_W * self.sidebar.reveal();
                Rect::new(r.x0, r.y0, r.x0 + w, r.y1)
            }
            SplitLayout::Overlay => {
                let slide = self.sidebar.slide();
                let x0 = r.x0 - SIDEBAR_W * (1.0 - slide);
                Rect::new(x0, r.y0, x0 + SIDEBAR_W, r.y1)
            }
        }
    }

    /// The horizontal band the list + detail occupy. Docked: inset by the
    /// eased sidebar width so the rail doesn't overlap. Overlay/push: the
    /// full width (the sidebar floats above it).
    fn content_band(&self) -> Rect {
        let r = self.rect;
        match self.sidebar_layout() {
            SplitLayout::Docked => {
                let inset = SIDEBAR_W * self.sidebar.reveal();
                Rect::new(r.x0 + inset, r.y0, r.x1, r.y1)
            }
            SplitLayout::Overlay => r,
        }
    }

    /// Whether the primary sidebar should paint anything this frame.
    fn sidebar_visible(&self) -> bool {
        self.sidebar.reveal() > 0.001
    }

    // ---- shot scenarios -------------------------------------------------

    /// Snap the responsive state to a deterministic configuration for the
    /// single-frame `--shots` renderer (the live app eases via `update`).
    ///
    /// * `"narrow"` — open the overlay sidebar over the content + scrim.
    /// * `"detail"` — single-pane push showing the detail with a Back chip.
    /// * anything else — the default frame.
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        match scenario {
            "narrow" => {
                self.sidebar.snap(true);
                // Keep the staged open flap from being re-settled to the
                // overlay default on the first laid-out frame.
                self.settled_layout = true;
            }
            "detail" => {
                self.push_pane = PushPane::Detail;
                self.push.snap(true);
                self.sidebar.snap(false);
                self.settled_layout = true;
            }
            _ => {}
        }
    }

    // ---- test accessors -------------------------------------------------

    #[doc(hidden)]
    pub fn open_id_for_test(&self) -> Option<SnippetId> {
        self.open
    }

    #[doc(hidden)]
    pub fn visible_count_for_test(&self) -> usize {
        self.visible.len()
    }

    #[doc(hidden)]
    pub fn set_search_for_test(&mut self, q: &str) {
        self.search_input.set_text(q);
        self.recompute_visible();
    }

    #[doc(hidden)]
    pub fn library_len_for_test(&self) -> usize {
        self.library.snippets.len()
    }

    #[doc(hidden)]
    pub fn expand_open_for_test(&self) -> bool {
        self.expand.is_some()
    }

    #[doc(hidden)]
    pub fn copy_for_test(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.copy(id, doc);
    }

    #[doc(hidden)]
    pub fn new_snippet_for_test(&mut self, doc: &mut AutoCommit) {
        self.new_snippet(doc);
    }

    #[doc(hidden)]
    pub fn snippet_uses_for_test(&self, id: SnippetId) -> u32 {
        self.library.get(id).map_or(0, |s| s.uses)
    }

    #[doc(hidden)]
    pub fn last_dispatched_for_test(&self) -> Option<qos_actions::EncodedAction> {
        self.actions.last_dispatched().cloned()
    }

    /// Drive the toolbar Delete affordance exactly as a click resolves it (via
    /// the shared `apply` dispatch), against the authoritative document.
    #[doc(hidden)]
    pub fn delete_for_test(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.apply(Action::Delete(id), doc);
    }

    /// Drive the toolbar Star affordance the same way.
    #[doc(hidden)]
    pub fn toggle_favorite_for_test(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.apply(Action::ToggleFavorite(id), doc);
    }

    // ---- responsive test accessors -------------------------------------

    /// Lay the app out at `width` x 800 through the real `build_layout` /
    /// `apply_layout` passes (so `rect` + the layout reconcile run exactly as
    /// in a live frame), then settle any pending reveal animation.
    ///
    /// # Errors
    /// Propagates [`taffy::TaffyError`] if `compute_layout` fails — never in
    /// practice, since `node` was just created in the same tree.
    #[doc(hidden)]
    pub fn layout_at_width_for_test(&mut self, width: f64) -> taffy::TaffyResult<()> {
        use ui::widget::Widget;
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = self.build_layout(&mut taffy, &theme, &mut node_map);
        taffy.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width as f32),
                height: taffy::AvailableSpace::Definite(800.0),
            },
        )?;
        self.apply_layout(&taffy, node, Point::ZERO);
        // Settle the reveal so `sidebar_open`/reveal reflect the target.
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        for _ in 0..240 {
            if !Widget::update(self, &mut doc, &root) {
                break;
            }
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn sidebar_is_overlay_for_test(&self) -> bool {
        self.sidebar_layout() == SplitLayout::Overlay
    }

    #[doc(hidden)]
    pub fn sidebar_open_for_test(&self) -> bool {
        self.sidebar_open()
    }

    #[doc(hidden)]
    pub fn is_push_for_test(&self) -> bool {
        self.is_push_layout()
    }

    #[doc(hidden)]
    pub fn push_showing_detail_for_test(&self) -> bool {
        self.push_pane == PushPane::Detail
    }

    #[doc(hidden)]
    pub fn toggle_sidebar_for_test(&mut self) {
        // Toggling the sidebar is pure presentation; a scratch document
        // satisfies the shared dispatch signature without any mutation.
        self.apply(Action::ToggleSidebar, &mut AutoCommit::new());
    }

    #[doc(hidden)]
    pub fn open_first_for_test(&mut self) {
        if let Some(id) = self.visible.first().copied() {
            self.open_snippet(id);
        }
    }
}

impl Default for SnippetsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ui::widget::Widget for SnippetsApp {
    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == qos_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        match operation {
            actions::SnippetsOperation::OpenSnippet { id } => {
                if self.library.get(id).is_none() {
                    return Err("snippet no longer exists".into());
                }
                self.open_snippet(id);
            }
            actions::SnippetsOperation::FocusSearch => self.search_input.set_focus(true),
        }
        Ok(true)
    }

    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        self.sync_library(state);
    }

    fn needs_idle_sync(&self) -> bool {
        !self.seeded
    }

    /// Publish the library + its snippets so the host's Ctrl+K action palette
    /// scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }

    /// Fold the library into the unified Ctrl+K palette: a snapshot
    /// [`SnippetSearchProvider`](crate::search_provider::SnippetSearchProvider)
    /// makes every snippet findable there by title, language, folder, tag,
    /// description or body, alongside the actions the app exports.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(
            crate::search_provider::SnippetSearchProvider::new(&self.library),
        )]
    }

    /// Open the snippet a chosen palette row identifies. Selection is app-local
    /// presentation (per-peer, never in the shared doc), so the doc args are
    /// unused.
    fn activate_search_result(&mut self, action_id: &str, _doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(id) = crate::search_provider::SnippetSearchProvider::parse_action_id(action_id)
            && self.library.get(id).is_some()
        {
            self.open_snippet(id);
        }
    }

    // `Widget::build_layout`/`apply_layout` return `NodeId`/`()`, not a
    // `Result` — the trait is shared by every widget in `ui`, so its
    // signature isn't ours to change here. `TaffyTree::new_leaf` and
    // `::layout` both wrap an infallible operation in this taffy version
    // (`new_leaf` always inserts; `layout` always finds the node we just
    // registered) — the `Result` is vestigial, not a real failure mode.
    #[allow(
        clippy::unwrap_used,
        reason = "TaffyTree::new_leaf never fails in this taffy version, and the Widget trait's build_layout signature isn't ours to change to Result"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn ui::widget::Widget>,
    ) -> NodeId {
        // The app paints itself; it just needs one full-bleed leaf node.
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn ui::widget::Widget>(self));
        node
    }

    #[allow(
        clippy::unwrap_used,
        reason = "node is the id this same build_layout registered moments earlier, so TaffyTree::layout always finds it, and the Widget trait's apply_layout signature isn't ours to change to Result"
    )]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // Crossing a breakpoint (a window resize) drops any stale user
        // preference so the sidebar adopts the new layout's natural default.
        let layout = self.sidebar_layout();
        self.sidebar.reconcile(layout);
        // On the first laid-out frame, snap the reveal to the resolved
        // default so a window that opens narrow doesn't flash the overlay
        // open before the animation settles. Subsequent frames ease via
        // `update`. A staged shot scenario marks this already-settled.
        if !self.settled_layout {
            self.sidebar.snap(self.sidebar.is_open(layout));
            self.settled_layout = true;
        }
        // Leaving the very-narrow push range collapses the single-pane stack
        // back so both panes show again on the next wider frame.
        if !self.is_push_layout() && self.push_pane == PushPane::Detail {
            self.push_pane = PushPane::Master;
            self.push.snap(false);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.hit.clear();
        let c = &theme.tokens.colors;
        // Whole-window background.
        draw::fill_rect(scene, t, self.rect, c.surfaces.base);

        // The content band (list + detail) is inset by the docked sidebar,
        // or spans the full width when the sidebar is an overlay flap.
        let band = self.content_band();
        self.draw_content_band(scene, t, tr, theme, band);

        // The primary sidebar: a docked rail beside the content, or — when
        // narrow — an overlay flap that slides in over the content behind a
        // dimming scrim. Painted last so it sits above the content.
        if self.sidebar_layout() == SplitLayout::Overlay && self.sidebar_visible() {
            // Scrim: fades in with the reveal, behind the sliding rail.
            let scrim = c
                .surfaces
                .overlay
                .with_alpha_factor((self.sidebar.reveal() * 0.55) as f32);
            draw::fill_rect(scene, t, self.rect, scrim);
            self.record(self.rect, Action::DismissSidebar);
        }
        if self.sidebar_visible() {
            let rail = self.sidebar_rect();
            self.draw_sidebar(scene, t, tr, theme, rail);
            // Right-edge divider for the rail.
            draw::fill_rect(
                scene,
                t,
                Rect::new(rail.x1 - 0.5, rail.y0, rail.x1 + 0.5, rail.y1),
                c.border_standard,
            );
        }

        // The hamburger toggle is shown whenever the sidebar isn't fully
        // docked-open, so a collapsed rail can be summoned and an overlay
        // rail dismissed. It records its own hit region.
        if self.show_toggle() {
            self.draw_toggle(scene, t, theme, band);
        }

        if let Some(toast) = &self.toast {
            paint_transient(
                scene,
                t,
                tr,
                theme,
                self.rect,
                ToastAnchor::BottomRight,
                &ToastCard::new(&toast.message, ToastKind::Info),
            );
        }

        // The expand modal renders above everything. While it's open the
        // panes beneath are inert, so the modal records hit regions
        // internally and the app's hit list isn't consulted.
        if let Some(mut expand) = self.expand.take() {
            expand.draw(scene, t, tr, theme, self.rect);
            self.expand = Some(expand);
        }
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
        // Track the cursor for hit-testing.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // The expand modal is modal: it owns all input while open.
        if self.handle_expand_event(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            focus_target,
            node_id,
            taffy,
        ) {
            *handled = true;
            return;
        }

        if self.route_pointer_to_search(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            focus_target,
            node_id,
            taffy,
        ) {
            *handled = true;
            return;
        }

        self.dispatch_event(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );
    }

    fn needs_animation_frame(&self) -> bool {
        self.toast.is_some()
            || self.sidebar.is_animating(self.sidebar_layout())
            || self.push.is_animating(SplitLayout::Docked)
    }

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        // Advance the sidebar reveal + the push (list↔detail) slide toward
        // their targets. Frame-rate-independent; the host re-pumps frames
        // while `needs_animation_frame` stays true.
        let dt = 1.0 / 60.0;
        self.sidebar.tick(self.sidebar_layout(), dt);
        self.push.tick(SplitLayout::Docked, dt);

        // Age out the toast.
        if let Some(toast) = &mut self.toast {
            if toast.ttl == 0 {
                self.toast = None;
            } else {
                toast.ttl -= 1;
            }
        }
        false
    }

    fn name(&self) -> &'static str {
        "SnippetsApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if let Some(modal) = &self.expand {
            modal.ime_cursor_rect()
        } else {
            self.search_input.ime_cursor_rect()
        }
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        if let Some(modal) = &self.expand {
            modal.ime_purpose()
        } else {
            self.search_input.ime_purpose()
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if let Some(modal) = &self.expand {
            modal.cursor_shape(point)
        } else {
            self.search_input.cursor_shape(point)
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.expand
            .as_ref()
            .and_then(ExpandModal::accessibility)
            .or_else(|| {
                self.search_input
                    .is_focused()
                    .then(|| self.search_input.accessibility())
                    .flatten()
            })
    }

    fn needs_commit(&self) -> bool {
        self.search_input.needs_commit()
            || self.expand.as_ref().is_some_and(ExpandModal::needs_commit)
    }

    fn on_commit(&mut self) {
        self.search_input.on_commit();
        if let Some(modal) = &mut self.expand {
            modal.on_commit();
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(modal) = &mut self.expand {
            modal.draw_overlay(scene, transform, tr, theme, presence);
        } else {
            self.search_input
                .draw_overlay(scene, transform, tr, theme, presence);
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, tr, theme, presence);
    }
}

/// Result of routing an event to the expand modal.
pub enum ExpandOutcome {
    /// Keep the modal open.
    Stay,
    /// User dismissed it.
    Cancel,
    /// User confirmed; the expanded text + originating snippet id.
    Copy(String, SnippetId),
}

impl SnippetsApp {
    // ---- content band (list + detail) ----------------------------------

    /// Paint the list and the detail within `band`. Wide / medium: the two
    /// panes sit side by side (list fixed-width, detail fills the rest).
    /// Very narrow: a single-pane push stack shows the list OR the detail,
    /// sliding between them, with a Back affordance on the detail.
    fn draw_content_band(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        band: Rect,
    ) {
        let c = &theme.tokens.colors;
        if self.is_push_layout() {
            // Single pane: list and detail each span the full band, offset
            // horizontally by the eased push amount (0 = list, 1 = detail).
            let w = band.width();
            let p = self.push.slide();
            let list = Rect::new(band.x0 - w * p, band.y0, band.x1 - w * p, band.y1);
            let detail = Rect::new(
                band.x0 + w * (1.0 - p),
                band.y0,
                band.x1 + w * (1.0 - p),
                band.y1,
            );
            // Draw the off-screen pane first so the incoming one sits on top.
            if p < 0.999 {
                self.draw_list(scene, t, tr, theme, list);
            }
            if p > 0.001 {
                self.draw_detail(scene, t, tr, theme, detail);
            }
            return;
        }

        // Side-by-side: list rail + detail fills the remainder. The list
        // narrows gracefully if the band can't fit the full fixed width.
        let list_w = LIST_W.min((band.width() - 240.0).max(200.0));
        let list = Rect::new(band.x0, band.y0, band.x0 + list_w, band.y1);
        let detail = Rect::new(list.x1, band.y0, band.x1, band.y1);
        self.draw_list(scene, t, tr, theme, list);
        self.draw_detail(scene, t, tr, theme, detail);
        // Divider between list and detail.
        draw::fill_rect(
            scene,
            t,
            Rect::new(list.x1 - 0.5, band.y0, list.x1 + 0.5, band.y1),
            c.border_standard,
        );
    }

    // ---- toggles --------------------------------------------------------

    /// Whether the top-left toggle (hamburger / Back) should be shown. It
    /// appears whenever the primary sidebar isn't fully docked-open (so it
    /// can be summoned / dismissed), or while the push stack shows the detail
    /// (so the user can go Back to the list).
    fn show_toggle(&self) -> bool {
        let sidebar_collapsed =
            self.sidebar.reveal() < 0.999 || self.sidebar_layout() == SplitLayout::Overlay;
        let push_in_detail = self.is_push_layout() && self.push.reveal() > 0.5;
        sidebar_collapsed || push_in_detail
    }

    /// Draw the top-left toggle button + glyph and record its hit region.
    /// In single-pane push showing the detail it's a Back chevron (returns to
    /// the list); otherwise it's a hamburger (open) / close-X (dismiss the
    /// overlay sidebar).
    fn draw_toggle(&mut self, scene: &mut Scene, t: Affine, theme: &Theme, band: Rect) {
        let c = &theme.tokens.colors;
        let pad = 10.0;
        let bx = band.x0 + pad;
        let by = band.y0 + pad;
        let btn = Rect::new(bx, by, bx + TOGGLE_SIZE, by + TOGGLE_SIZE);
        draw::fill_round(scene, t, btn, theme.radii_md(), c.surfaces.raised);
        draw::stroke_round(scene, t, btn, theme.radii_md(), 1.0, c.border_standard);

        let cx = btn.x0 + TOGGLE_SIZE / 2.0;
        let cy = btn.y0 + TOGGLE_SIZE / 2.0;
        let push_back = self.is_push_layout() && self.push.reveal() > 0.5;
        let overlay_open =
            self.sidebar_layout() == SplitLayout::Overlay && self.sidebar.reveal() > 0.5;

        if push_back {
            // Back chevron "‹".
            let r = 5.0;
            draw::stroke_line(
                scene,
                t,
                Point::new(cx + r * 0.6, cy - r),
                Point::new(cx - r * 0.6, cy),
                1.8,
                c.text_primary,
            );
            draw::stroke_line(
                scene,
                t,
                Point::new(cx - r * 0.6, cy),
                Point::new(cx + r * 0.6, cy + r),
                1.8,
                c.text_primary,
            );
            self.record(btn, Action::PushBack);
        } else if overlay_open {
            // Close "X" when the overlay sidebar is showing.
            let r = 6.0;
            draw::stroke_line(
                scene,
                t,
                Point::new(cx - r, cy - r),
                Point::new(cx + r, cy + r),
                1.8,
                c.text_primary,
            );
            draw::stroke_line(
                scene,
                t,
                Point::new(cx + r, cy - r),
                Point::new(cx - r, cy + r),
                1.8,
                c.text_primary,
            );
            self.record(btn, Action::ToggleSidebar);
        } else {
            // Three-line hamburger to summon the sidebar.
            let half = 7.0;
            for i in 0..3 {
                let ly = btn.y0 + 11.0 + f64::from(i) * 6.0;
                draw::stroke_line(
                    scene,
                    t,
                    Point::new(cx - half, ly),
                    Point::new(cx + half, ly),
                    1.8,
                    c.text_primary,
                );
            }
            self.record(btn, Action::ToggleSidebar);
        }
    }

    /// The per-`WindowEvent`-kind dispatch, once the expand modal and the
    /// embedded search editor have both had first refusal.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the WindowEvent-routing signature threaded through ui::widget::Widget::handle_event and route_search_event in this file — bundling it into a context struct would ripple into the shared ui widget trait, out of scope here"
    )]
    fn dispatch_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
                );
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * BODY_LINE,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                self.scroll = (self.scroll - dy).max(0.0);
                *handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.handle_keyboard_input(
                    logical_key,
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
                );
            }
            WindowEvent::Ime(_)
                if self.search_input.is_focused()
                    && self.route_search_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        focus_target,
                        node_id,
                        taffy,
                    ) =>
            {
                *handled = true;
            }
            _ => {}
        }
    }

    /// Route input to the expand-with-variables modal while it's open — it
    /// owns all input, so the panes beneath and the rest of `handle_event`
    /// are never consulted. Returns `true` once a modal was open and has
    /// consumed the event (closing, copying and toasting side effects are
    /// already applied); `false` when there is no modal to route to.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the WindowEvent-routing signature threaded through ui::widget::Widget::handle_event and route_search_event in this file — bundling it into a context struct would ripple into the shared ui widget trait, out of scope here"
    )]
    fn handle_expand_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let Some(mut modal) = self.expand.take() else {
            return false;
        };
        let outcome = modal.handle_event(
            event,
            scale_factor,
            self.last_mouse,
            &modifiers,
            state,
            path,
            focus_target,
            node_id,
            taffy,
        );
        match outcome {
            ExpandOutcome::Stay => {
                self.expand = Some(modal);
            }
            ExpandOutcome::Cancel => {}
            ExpandOutcome::Copy(text, id) => {
                ui::clipboard_copy(&text);
                self.record_use(id, state);
                self.toast("Expanded and copied");
            }
        }
        true
    }

    /// Feed pointer motion and clicks to the embedded search editor. The
    /// editor isn't mounted as a taffy child, so its hit testing needs this
    /// explicit routing rather than the normal event-tree dispatch. Returns
    /// `true` when the editor consumed a click and the caller should stop
    /// routing the event further.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the WindowEvent-routing signature threaded through ui::widget::Widget::handle_event and route_search_event in this file — bundling it into a context struct would ripple into the shared ui widget trait, out of scope here"
    )]
    fn route_pointer_to_search(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            let _ = self.route_search_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                focus_target,
                node_id,
                taffy,
            );
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) && self.search_rect.contains(self.last_mouse)
        {
            self.search_input.set_focus(true);
        }
        matches!(event, WindowEvent::MouseInput { .. })
            && (self.search_input.is_focused() || self.search_rect.contains(self.last_mouse))
            && self.route_search_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                focus_target,
                node_id,
                taffy,
            )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the WindowEvent-routing signature threaded through ui::widget::Widget::handle_event and route_search_event in this file — bundling it into a context struct would ripple into the shared ui widget trait, out of scope here"
    )]
    fn handle_left_click(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(action) = self.action_at(self.last_mouse) {
            // Clicking anywhere that isn't the search field blurs it.
            if !matches!(action, Action::FocusSearch) {
                self.search_input.set_focus(false);
            }
            let search_click = matches!(action, Action::FocusSearch);
            self.apply(action, state);
            if search_click {
                let _ = self.route_search_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    focus_target,
                    node_id,
                    taffy,
                );
            }
            *handled = true;
        } else {
            self.search_input.set_focus(false);
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the WindowEvent-routing signature threaded through ui::widget::Widget::handle_event and route_search_event in this file — bundling it into a context struct would ripple into the shared ui widget trait, out of scope here"
    )]
    fn handle_keyboard_input(
        &mut self,
        logical_key: &Key,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let mods = modifiers.state();
        // Cmd/Ctrl-F focuses search; Cmd/Ctrl-N new snippet;
        // Cmd/Ctrl-C copies the open snippet.
        if (mods.control_key() || mods.super_key())
            && let Key::Character(ch) = logical_key
        {
            match ch.to_lowercase().as_str() {
                "f" => {
                    self.search_input.set_focus(true);
                    self.search_input
                        .set_cursor_pos(self.search_input.value().len());
                    *handled = true;
                    return;
                }
                "n" => {
                    self.new_snippet(state);
                    *handled = true;
                    return;
                }
                "c" if !self.search_input.is_focused() => {
                    if let Some(id) = self.open {
                        self.copy(id, state);
                    }
                    *handled = true;
                    return;
                }
                _ => {}
            }
        }

        if self.search_input.is_focused() {
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.search_input.set_text("");
                    self.search_input.set_focus(false);
                    self.recompute_visible();
                    *handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.search_input.set_focus(false);
                    *handled = true;
                }
                _ => {
                    if self.route_search_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        focus_target,
                        node_id,
                        taffy,
                    ) {
                        *handled = true;
                    }
                }
            }
            return;
        }

        // Escape walks back through the responsive layers, then
        // finally clears the search. In single-pane push showing the
        // detail it returns to the list; with the overlay sidebar
        // open it dismisses the flap; otherwise it clears the search.
        if let Key::Named(NamedKey::Escape) = logical_key {
            if self.is_push_layout() && self.push.reveal() > 0.5 {
                self.apply(Action::PushBack, state);
                *handled = true;
            } else if self.sidebar_layout() == SplitLayout::Overlay && self.sidebar_open() {
                self.apply(Action::DismissSidebar, state);
                *handled = true;
            } else if !self.search_input.value().is_empty() {
                self.search_input.set_text("");
                self.recompute_visible();
                *handled = true;
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the WindowEvent-routing signature threaded through ui::widget::Widget::handle_event in this file — bundling it into a context struct would ripple into the shared ui widget trait, out of scope here"
    )]
    fn route_search_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let mut input_handled = false;
        self.search_input.handle_event(
            event,
            scale_factor,
            &modifiers,
            state,
            path,
            &mut input_handled,
            focus_target,
            node_id,
            taffy,
        );
        if self.search_input.take_dirty().is_some() {
            self.scroll = 0.0;
            self.recompute_visible();
            input_handled = true;
        }
        input_handled
    }

    // ---- sidebar --------------------------------------------------------

    fn draw_sidebar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, area, c.surfaces.base);
        let pad = 16.0;
        // No app-name wordmark: the OS titlebar shows "Snippets". Docked, the
        // folder list starts at the top; as an overlay flap, keep a small band
        // so the top-left close toggle floating over this corner has room.
        let mut y = if self.sidebar_layout() == SplitLayout::Overlay {
            area.y0 + TOGGLE_SIZE + 14.0
        } else {
            area.y0 + 14.0
        };

        // Smart folders.
        for smart in [
            SmartFolder::All,
            SmartFolder::Favorites,
            SmartFolder::MostUsed,
        ] {
            let active = matches!(&self.selection, Selection::Smart(s) if *s == smart);
            let count = self.library.query(&Selection::Smart(smart), "").len();
            let row = self.sidebar_row(
                scene,
                t,
                tr,
                theme,
                area,
                y,
                smart.label(),
                Some(count),
                active,
                0,
            );
            self.record(row, Action::Select(Selection::Smart(smart)));
            y += SIDEBAR_ROW_H;
        }

        y += 10.0;
        section_header(scene, t, tr, theme, "FOLDERS", area.x0 + pad, y);
        y += 22.0;
        for folder in self.library.folders() {
            let active = matches!(&self.selection, Selection::Folder(f) if *f == folder);
            let count = self
                .library
                .query(&Selection::Folder(folder.clone()), "")
                .len();
            let row = self.sidebar_row(
                scene,
                t,
                tr,
                theme,
                area,
                y,
                &folder,
                Some(count),
                active,
                1,
            );
            self.record(row, Action::Select(Selection::Folder(folder.clone())));
            y += SIDEBAR_ROW_H;
        }

        y += 10.0;
        section_header(scene, t, tr, theme, "TAGS", area.x0 + pad, y);
        y += 22.0;
        for (tag, cnt) in self.library.tags() {
            let active = matches!(&self.selection, Selection::Tag(x) if *x == tag);
            let label = format!("#{tag}");
            let row = self.sidebar_row(scene, t, tr, theme, area, y, &label, Some(cnt), active, 1);
            self.record(row, Action::Select(Selection::Tag(tag.clone())));
            y += SIDEBAR_ROW_H;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sidebar_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        y: f64,
        label: &str,
        count: Option<usize>,
        active: bool,
        indent: u32,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let pad = 10.0;
        let row = Rect::new(area.x0 + 8.0, y, area.x1 - 8.0, y + SIDEBAR_ROW_H - 4.0);
        if active {
            draw::fill_round(scene, t, row, theme.radii_md(), c.accent[5]);
        } else if row.contains(self.last_mouse) {
            draw::fill_round(scene, t, row, theme.radii_md(), c.surface_white_5);
        }
        let fg = if active { c.text_primary } else { c.neutral[7] };
        let x = row.x0 + pad + f64::from(indent) * 12.0;
        let ty = y + (SIDEBAR_ROW_H - 4.0 - 15.0) / 2.0;
        draw::text(scene, tr, t, label, x, ty, 14.0, fg, TextWeight::Medium);
        if let Some(shown) = count {
            let count_label = shown.to_string();
            let count_w = f64::from(tr.measure(&count_label, 12.0, None).0);
            let cfg = if active { c.accent[8] } else { c.text_dim };
            draw::text(
                scene,
                tr,
                t,
                &count_label,
                row.x1 - pad - count_w,
                ty + 1.0,
                12.0,
                cfg,
                TextWeight::Regular,
            );
        }
        row
    }

    // ---- list pane ------------------------------------------------------

    fn draw_list(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, area, c.neutral[1]);

        // Search bar.
        let pad = 12.0;
        let search_rect = Rect::new(
            area.x0 + pad,
            area.y0 + 10.0,
            area.x1 - pad,
            area.y0 + 10.0 + SEARCH_H - 16.0,
        );
        self.search_rect = search_rect;
        let focused = self.search_input.is_focused();
        let border = if focused {
            c.border_focus
        } else {
            c.border_standard
        };
        draw::fill_round(scene, t, search_rect, theme.radii_md(), c.surfaces.base);
        draw::stroke_round(scene, t, search_rect, theme.radii_md(), 1.5, border);
        let empty = self.search_input.value().is_empty();
        // Keep Snippets' compact rectangular chrome and grammar hint while
        // delegating text editing, selection, clipboard and IME to the shared
        // input. Reserve the old trailing slot for the clear affordance.
        self.search_input.apply_layout_external(Rect::new(
            search_rect.x0 + 12.0,
            search_rect.y0,
            search_rect.x1 - if empty { 12.0 } else { 30.0 },
            search_rect.y1,
        ));
        self.search_input
            .draw(scene, t, tr, theme, &PresenceCtx::empty());
        self.record(search_rect, Action::FocusSearch);
        if !empty {
            // A small clear affordance.
            let clear = Rect::new(
                search_rect.x1 - 26.0,
                search_rect.y0,
                search_rect.x1,
                search_rect.y1,
            );
            draw::text(
                scene,
                tr,
                t,
                "×",
                clear.x0 + 6.0,
                search_rect.y0 + (search_rect.height() - 15.0) / 2.0 - 2.0,
                18.0,
                c.text_dim,
                TextWeight::Regular,
            );
            self.record(clear, Action::ClearSearch);
        }

        // "+ New" header row with the result count.
        let header_y = area.y0 + SEARCH_H + 4.0;
        let count_label = format!("{} snippets", self.visible.len());
        draw::text(
            scene,
            tr,
            t,
            &count_label,
            area.x0 + pad,
            header_y,
            12.0,
            c.text_dim,
            TextWeight::Medium,
        );
        let new_label = "+ New";
        let nw = f64::from(tr.measure(new_label, 13.0, None).0) + 20.0;
        let new_rect = Rect::new(
            area.x1 - pad - nw,
            header_y - 4.0,
            area.x1 - pad,
            header_y + 18.0,
        );
        draw::fill_round(scene, t, new_rect, theme.radii_md(), c.accent[5]);
        draw::text(
            scene,
            tr,
            t,
            new_label,
            new_rect.x0 + 10.0,
            new_rect.y0 + 4.0,
            13.0,
            c.text_primary,
            TextWeight::Semibold,
        );
        self.record(new_rect, Action::NewSnippet);

        // Snippet rows (scrollable region).
        let list_top = header_y + 26.0;
        let mut y = list_top - self.scroll;
        let ids = self.visible.clone();
        for id in ids {
            let Some(s) = self.library.get(id).cloned() else {
                continue;
            };
            let row = Rect::new(area.x0 + 6.0, y, area.x1 - 6.0, y + ROW_H - 6.0);
            // Cull rows scrolled out of view but still record the hit so
            // clicks resolve; skip drawing if fully off-pane.
            if row.y1 >= list_top - 4.0 && row.y0 <= area.y1 {
                self.draw_list_row(scene, t, tr, theme, row, &s);
            }
            self.record(row, Action::OpenSnippet(id));
            y += ROW_H;
        }

        if self.visible.is_empty() {
            self.draw_list_empty(scene, t, tr, theme, area, list_top);
        }
    }

    /// The list pane's zero-state. A genuinely empty library gets a composed,
    /// inviting onboarding with a primary "New snippet" call to action; a
    /// non-empty library filtered down to nothing gets a "no match" note with
    /// a Clear-search affordance. Never fabricated rows — an honest empty.
    fn draw_list_empty(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        list_top: f64,
    ) {
        let cx = area.x0 + area.width() / 2.0;
        let library_empty = self.library.snippets.is_empty();
        let y = Self::draw_empty_glyph_and_copy(scene, t, tr, theme, area, list_top, library_empty);
        if library_empty {
            self.draw_empty_new_snippet_cta(scene, t, tr, theme, cx, y);
        } else {
            self.draw_empty_clear_search_cta(scene, t, tr, theme, cx, y);
        }
    }

    /// The glyph tile, headline and wrapped sub-copy shown above the call
    /// to action. Returns the y cursor the CTA should start from.
    fn draw_empty_glyph_and_copy(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        list_top: f64,
        library_empty: bool,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let cx = area.x0 + area.width() / 2.0;
        let top = list_top + (area.y1 - list_top).min(360.0) * 0.16;

        // A soft rounded tile carrying a glyph — a real, composed anchor.
        let tile = 56.0;
        let tile_rect = Rect::new(cx - tile / 2.0, top, cx + tile / 2.0, top + tile);
        draw::fill_round(
            scene,
            t,
            tile_rect,
            theme.radii_md() + 4.0,
            c.surfaces.raised,
        );
        draw::stroke_round(
            scene,
            t,
            tile_rect,
            theme.radii_md() + 4.0,
            1.0,
            c.border_standard,
        );
        let glyph = "{ }";
        let gw = f64::from(tr.measure(glyph, 22.0, None).0);
        draw::text(
            scene,
            tr,
            t,
            glyph,
            cx - gw / 2.0,
            top + 15.0,
            22.0,
            c.accent[6],
            TextWeight::Bold,
        );
        let mut y = tile_rect.y1 + 20.0;

        let (headline, sub) = if library_empty {
            (
                "No snippets yet",
                "Save a command, query, or block of code once — reuse it anywhere.",
            )
        } else {
            (
                "No snippets match",
                "Try a different search, or clear the filter.",
            )
        };
        let hw = f64::from(tr.measure(headline, 17.0, None).0);
        draw::text(
            scene,
            tr,
            t,
            headline,
            cx - hw / 2.0,
            y,
            17.0,
            c.text_primary,
            TextWeight::Bold,
        );
        y += 26.0;
        // Sub-copy, wrapped to the available width so it never clips.
        for line in wrap_center(sub, tr, 13.0, area.width() - 48.0) {
            let lw = f64::from(tr.measure(&line, 13.0, None).0);
            draw::text(
                scene,
                tr,
                t,
                &line,
                cx - lw / 2.0,
                y,
                13.0,
                c.text_dim,
                TextWeight::Regular,
            );
            y += 19.0;
        }
        y + 12.0
    }

    /// The "New snippet" primary call to action shown when the whole
    /// library is empty, plus a quiet hint at the search grammar beneath it.
    fn draw_empty_new_snippet_cta(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        cx: f64,
        y: f64,
    ) {
        let c = &theme.tokens.colors;
        let label = "New snippet";
        let lw = f64::from(tr.measure(label, 13.5, None).0);
        let bw = lw + 34.0;
        let btn = Rect::new(cx - bw / 2.0, y, cx + bw / 2.0, y + 36.0);
        let hovered = btn.contains(self.last_mouse);
        draw::fill_round(
            scene,
            t,
            btn,
            theme.radii_md(),
            if hovered { c.accent[4] } else { c.accent[5] },
        );
        draw::text(
            scene,
            tr,
            t,
            label,
            btn.x0 + 17.0,
            btn.y0 + (36.0 - 14.0) / 2.0,
            13.5,
            c.text_primary,
            TextWeight::Semibold,
        );
        self.record(btn, Action::NewSnippet);
        // A quiet hint at the search grammar.
        let hint_y = btn.y1 + 22.0;
        let hint = "Tip: filter with  language:rust  or  tag:k8s";
        let hw = f64::from(tr.measure(hint, 11.5, None).0);
        draw::text(
            scene,
            tr,
            t,
            hint,
            cx - hw / 2.0,
            hint_y,
            11.5,
            c.text_dim,
            TextWeight::Regular,
        );
    }

    /// The "Clear search" call to action shown when a search/filter has
    /// hidden every snippet in an otherwise non-empty library.
    fn draw_empty_clear_search_cta(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        cx: f64,
        y: f64,
    ) {
        let c = &theme.tokens.colors;
        let label = "Clear search";
        let lw = f64::from(tr.measure(label, 13.0, None).0);
        let bw = lw + 30.0;
        let btn = Rect::new(cx - bw / 2.0, y, cx + bw / 2.0, y + 34.0);
        let hovered = btn.contains(self.last_mouse);
        draw::fill_round(
            scene,
            t,
            btn,
            theme.radii_md(),
            if hovered {
                c.surfaces.raised
            } else {
                c.neutral[2]
            },
        );
        draw::stroke_round(scene, t, btn, theme.radii_md(), 1.0, c.border_standard);
        draw::text(
            scene,
            tr,
            t,
            label,
            btn.x0 + 15.0,
            btn.y0 + (34.0 - 14.0) / 2.0,
            13.0,
            c.text_primary,
            TextWeight::Semibold,
        );
        self.record(btn, Action::ClearSearch);
    }

    fn draw_list_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        s: &Snippet,
    ) {
        let c = &theme.tokens.colors;
        let active = self.open == Some(s.id);
        if active {
            draw::fill_round(scene, t, row, theme.radii_md(), c.surfaces.raised);
            draw::stroke_round(scene, t, row, theme.radii_md(), 1.0, c.accent[5]);
        } else if row.contains(self.last_mouse) {
            draw::fill_round(scene, t, row, theme.radii_md(), c.surface_white_5);
        }
        let pad = 12.0;
        // Language sigil badge.
        let badge = Rect::new(
            row.x0 + pad,
            row.y0 + 12.0,
            row.x0 + pad + 30.0,
            row.y0 + 32.0,
        );
        draw::fill_round(scene, t, badge, 6.0, lang_tint(theme, s.language));
        let sig = s.language.sigil();
        let sw = f64::from(tr.measure(sig, 11.0, None).0);
        draw::text(
            scene,
            tr,
            t,
            sig,
            badge.x0 + (badge.width() - sw) / 2.0,
            badge.y0 + 3.0,
            11.0,
            c.neutral[0],
            TextWeight::Bold,
        );

        let tx = badge.x1 + 10.0;
        // Title (+ star if favorite).
        let title = if s.favorite {
            format!("★ {}", s.title)
        } else {
            s.title.clone()
        };
        let tcolor = if s.favorite {
            c.semantic.warning
        } else {
            c.text_primary
        };
        draw::text(
            scene,
            tr,
            t,
            &title,
            tx,
            row.y0 + 10.0,
            14.5,
            tcolor,
            TextWeight::Semibold,
        );

        // Subtitle: first body line, dimmed and clipped.
        let first_line = s.body.lines().next().unwrap_or("").trim();
        let preview = clip(first_line, 42);
        draw::mono(
            scene,
            tr,
            t,
            &preview,
            tx,
            row.y0 + 30.0,
            12.0,
            c.text_dim,
            TextWeight::Regular,
        );

        // Trailing usage / placeholder hint.
        let hint = if s.has_placeholders() {
            format!("{} vars", s.placeholders().len())
        } else if s.uses > 0 {
            format!("{}×", s.uses)
        } else {
            String::new()
        };
        if !hint.is_empty() {
            let hw = f64::from(tr.measure(&hint, 11.0, None).0);
            draw::text(
                scene,
                tr,
                t,
                &hint,
                row.x1 - pad - hw,
                row.y0 + 11.0,
                11.0,
                c.text_dim,
                TextWeight::Medium,
            );
        }
    }

    // ---- detail pane ----------------------------------------------------

    fn draw_detail(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, area, c.surfaces.base);
        let Some(open) = self.open else {
            self.draw_detail_placeholder(scene, t, tr, theme, area);
            return;
        };
        let Some(snippet) = self.library.get(open).cloned() else {
            return;
        };
        let y = self.draw_detail_header(scene, t, tr, theme, area, open, &snippet);
        let y = self.draw_detail_toolbar(scene, t, tr, theme, area, y, open, &snippet);

        // Code card with syntax-highlighted body.
        let pad = 28.0;
        let card = Rect::new(area.x0 + pad, y, area.x1 - pad, area.y1 - 20.0);
        draw::fill_round(scene, t, card, theme.radii_md(), c.neutral[0]);
        draw::stroke_round(scene, t, card, theme.radii_md(), 1.0, c.border_standard);
        Self::draw_code(scene, t, tr, theme, card, &snippet);
    }

    /// The empty-detail placeholder shown when nothing is selected.
    fn draw_detail_placeholder(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        let msg = if self.library.snippets.is_empty() {
            "Your snippets will appear here."
        } else {
            "Select a snippet to preview it."
        };
        let mw = f64::from(tr.measure(msg, 15.0, None).0);
        draw::text(
            scene,
            tr,
            t,
            msg,
            area.x0 + (area.width() - mw) / 2.0,
            area.y0 + area.height() * 0.4,
            15.0,
            c.text_dim,
            TextWeight::Regular,
        );
    }

    /// Title, then the language/folder/tag pill row, then the free-text
    /// description. Returns the y cursor the action toolbar starts from.
    #[allow(
        clippy::too_many_arguments,
        reason = "scene/t/tr/theme is the draw-context quad every paint method in this file takes, plus area/open/snippet identify what's being painted — a context struct would ripple across every draw_* method here, out of scope for this split"
    )]
    fn draw_detail_header(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        open: SnippetId,
        snippet: &Snippet,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let pad = 28.0;
        let mut y = area.y0 + 24.0;

        // Title. In single-pane push showing the detail, the top-left Back
        // chevron floats over this corner, so indent the title past it.
        let back_shown = self.is_push_layout() && self.push.reveal() > 0.5;
        let title_x = if back_shown {
            area.x0 + pad + TOGGLE_SIZE
        } else {
            area.x0 + pad
        };
        draw::text(
            scene,
            tr,
            t,
            &snippet.title,
            title_x,
            y,
            24.0,
            c.text_primary,
            TextWeight::Bold,
        );
        y += 36.0;

        y = self.draw_detail_meta_row(scene, t, tr, theme, area, y, open, snippet);

        // Description.
        if !snippet.description.is_empty() {
            draw::text(
                scene,
                tr,
                t,
                &snippet.description,
                area.x0 + pad,
                y,
                14.0,
                c.text_dim,
                TextWeight::Regular,
            );
            y += 26.0;
        }
        y
    }

    /// The language pill (cycles the language on click) + folder pill +
    /// one pill per tag. Returns the y cursor beneath the row.
    #[allow(
        clippy::too_many_arguments,
        reason = "scene/t/tr/theme is the draw-context quad every paint method in this file takes, plus area/y/open/snippet identify what's being painted — a context struct would ripple across every draw_* method here, out of scope for this split"
    )]
    fn draw_detail_meta_row(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        y: f64,
        open: SnippetId,
        snippet: &Snippet,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let pad = 28.0;
        let mut x = area.x0 + pad;
        let lp = draw::pill(
            scene,
            tr,
            t,
            snippet.language.label(),
            x,
            y,
            22.0,
            12.0,
            c.neutral[0],
            lang_tint(theme, snippet.language),
        );
        // Clicking the language pill cycles to the next language.
        let next = next_language(snippet.language);
        self.record(lp, Action::SetLanguage(open, next));
        x = lp.x1 + 8.0;
        if !snippet.folder.is_empty() {
            let fp = draw::pill(
                scene,
                tr,
                t,
                &snippet.folder,
                x,
                y,
                22.0,
                12.0,
                c.text_dim,
                c.neutral[2],
            );
            x = fp.x1 + 8.0;
        }
        for tag in &snippet.tags {
            let label = format!("#{tag}");
            let tp = draw::pill(
                scene,
                tr,
                t,
                &label,
                x,
                y,
                22.0,
                12.0,
                c.accent[7],
                c.accent[1],
            );
            self.record(tp, Action::Select(Selection::Tag(tag.clone())));
            x = tp.x1 + 8.0;
        }
        y + 34.0
    }

    /// Copy / Expand / Favorite / Duplicate / Delete. Returns the y cursor
    /// the code card starts from.
    #[allow(
        clippy::too_many_arguments,
        reason = "scene/t/tr/theme is the draw-context quad every paint method in this file takes, plus area/y/open/snippet identify what's being painted — a context struct would ripple across every draw_* method here, out of scope for this split"
    )]
    fn draw_detail_toolbar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        y: f64,
        open: SnippetId,
        snippet: &Snippet,
    ) -> f64 {
        let pad = 28.0;
        let y = y + 4.0;
        let mut bx = area.x0 + pad;
        let primary_label = if snippet.has_placeholders() {
            "Fill & Copy"
        } else {
            "Copy"
        };
        bx = self.toolbar_button(
            scene,
            t,
            tr,
            theme,
            bx,
            y,
            primary_label,
            true,
            Action::Copy(open),
        );
        if snippet.has_placeholders() {
            bx = self.toolbar_button(
                scene,
                t,
                tr,
                theme,
                bx,
                y,
                "Variables…",
                false,
                Action::Expand(open),
            );
        }
        let star_label = if snippet.favorite {
            "★ Starred"
        } else {
            "☆ Star"
        };
        bx = self.toolbar_button(
            scene,
            t,
            tr,
            theme,
            bx,
            y,
            star_label,
            false,
            Action::ToggleFavorite(open),
        );
        bx = self.toolbar_button(
            scene,
            t,
            tr,
            theme,
            bx,
            y,
            "Duplicate",
            false,
            Action::Duplicate(open),
        );
        let _ = self.toolbar_button(
            scene,
            t,
            tr,
            theme,
            bx,
            y,
            "Delete",
            false,
            Action::Delete(open),
        );
        y + 44.0
    }

    /// Draw a toolbar button, record its hit region against `action`, and
    /// return the x-coordinate the next button should start at.
    #[allow(clippy::too_many_arguments)]
    fn toolbar_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        label: &str,
        primary: bool,
        action: Action,
    ) -> f64 {
        let tw = f64::from(tr.measure(label, 13.0, None).0);
        let w = tw + 24.0;
        let rect = Rect::new(x, y, x + w, y + 32.0);
        let hovered = rect.contains(self.last_mouse);
        let face = ButtonFace::new(rect, label)
            .radius(theme.radii_md())
            .text_size(13.0)
            .weight(TextWeight::Semibold)
            .hovered(hovered)
            .style(if primary {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Neutral
            });
        Button::paint(scene, t, tr, theme, &face);
        self.record(rect, action);
        rect.x1 + 8.0
    }

    fn draw_code(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        s: &Snippet,
    ) {
        let c = &theme.tokens.colors;
        let pad = 16.0;
        let gutter_w = 36.0;
        let x0 = card.x0 + pad;
        // Gutter background.
        let gutter = Rect::new(card.x0, card.y0, card.x0 + gutter_w + pad, card.y1);
        draw::fill_round(scene, t, gutter, theme.radii_md(), c.neutral[1]);
        // Re-cover the right side so only the left edge keeps the radius.
        draw::fill_rect(
            scene,
            t,
            Rect::new(card.x0 + 10.0, card.y0, card.x0 + gutter_w + pad, card.y1),
            c.neutral[1],
        );

        let mut y = card.y0 + pad;
        let line_h = BODY_LINE;
        let code_x = x0 + gutter_w;
        for (i, line) in s.body.lines().enumerate() {
            if y > card.y1 - line_h {
                break;
            }
            // Line number.
            let num = (i + 1).to_string();
            let nw = draw::mono_width(tr, &num, 11.0);
            draw::mono(
                scene,
                tr,
                t,
                &num,
                x0 + gutter_w - 10.0 - nw,
                y,
                11.0,
                c.text_dim,
                TextWeight::Regular,
            );
            // Highlighted spans.
            let mut sx = code_x;
            for span in highlight::highlight_line(line, s.language) {
                let color = span.kind.color(theme);
                let weight = if matches!(span.kind, highlight::TokenKind::Keyword) {
                    TextWeight::Semibold
                } else {
                    TextWeight::Regular
                };
                draw::mono(scene, tr, t, &span.text, sx, y, BODY_FONT, color, weight);
                sx += draw::mono_width(tr, &span.text, BODY_FONT);
            }
            y += line_h;
        }
    }
}

/// Cycle to the next language in the canonical order (wraps).
fn next_language(lang: Language) -> Language {
    let i = Language::ALL.iter().position(|l| *l == lang).unwrap_or(0);
    Language::ALL
        .get((i + 1) % Language::ALL.len())
        .copied()
        .unwrap_or(Language::PlainText)
}

/// A per-language accent tint for the badge. Derived from theme ramps so
/// it tracks the active palette (never a hard-coded hex).
fn lang_tint(theme: &Theme, lang: Language) -> vello::peniko::Color {
    let c = &theme.tokens.colors;
    match lang {
        Language::Rust | Language::Toml => c.semantic.warning,
        Language::Shell | Language::Dockerfile => c.semantic.info,
        Language::Sql | Language::Yaml | Language::Json => c.accent[5],
        Language::Git => c.semantic.danger,
        Language::Python => c.semantic.success,
        Language::JavaScript => c.accent[6],
        // Strong neutral slots: the pill ink is `neutral[0]` (the background
        // tone), so the fill must sit deep in the ramp to carry it in *both*
        // themes. A mid slot (neutral[4]/[5]) reads white-on-light-grey in the
        // light theme and fails contrast.
        Language::Markdown | Language::Html | Language::Css => c.neutral[7],
        Language::PlainText => c.neutral[6],
    }
}

fn section_header(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    label: &str,
    x: f64,
    y: f64,
) {
    draw::text(
        scene,
        tr,
        t,
        label,
        x,
        y,
        11.0,
        theme.tokens.colors.text_dim,
        TextWeight::Bold,
    );
}

/// Greedy word-wrap `s` into lines no wider than `max_w` at font `size`,
/// for centred multi-line zero-state copy. Falls back to the whole string
/// when a single word already exceeds the width (it will clip rather than
/// loop).
fn wrap_center(s: &str, tr: &mut TextRenderer, size: f32, max_w: f64) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let trial = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if f64::from(tr.measure(&trial, size, None).0) > max_w && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur.push_str(word);
        } else {
            cur = trial;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Convenience radius accessor used throughout the panes.
trait Radii {
    fn radii_md(&self) -> f64;
}
impl Radii for Theme {
    fn radii_md(&self) -> f64 {
        f64::from(self.tokens.radii.md)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::SNIPPETS_ACTION_TYPE;
    use ui::widget::Widget;

    #[test]
    fn live_app_starts_empty() {
        // Honest empty: a fresh install fabricates no snippets and persists
        // none. The starter set is demo/`--shots` only.
        let app = SnippetsApp::new();
        assert_eq!(app.library_len_for_test(), 0, "live library starts empty");
        assert!(
            app.open_id_for_test().is_none(),
            "nothing is opened on an empty library"
        );
        assert_eq!(app.visible_count_for_test(), 0);
    }

    #[test]
    fn demo_seeds_and_opens_a_snippet() {
        let app = SnippetsApp::new_demo();
        assert!(app.library_len_for_test() >= 10);
        assert!(app.open_id_for_test().is_some());
        assert!(app.visible_count_for_test() >= 10);
    }

    #[test]
    fn empty_live_app_does_not_seed_the_shared_doc() {
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert!(
            SnippetsDoc::list(&doc).is_empty(),
            "an empty live library must write no snippets into the shared store"
        );
    }

    #[test]
    fn search_narrows_results() {
        let mut app = SnippetsApp::new_demo();
        let before = app.visible_count_for_test();
        app.set_search_for_test("language:rust");
        let after = app.visible_count_for_test();
        assert!(after < before);
        assert!(after >= 1);
    }

    #[test]
    fn shared_search_editor_updates_results_without_writing_view_state_to_crdt() {
        let mut app = SnippetsApp::new_demo();
        let visible_before = app.visible_count_for_test();
        let mut doc = AutoCommit::new();
        let heads_before = doc.get_heads();
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mut focus_target = None;

        app.search_input.set_focus(true);
        assert!(app.route_search_event(
            &WindowEvent::Ime(winit::event::Ime::Commit("language:rust".into())),
            1.0,
            winit::event::Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut focus_target,
            node,
            &taffy,
        ));

        assert!(app.visible_count_for_test() < visible_before);
        assert_eq!(app.search_input.value(), "language:rust");
        assert_eq!(doc.get_heads(), heads_before, "search stays per-peer state");
    }

    #[test]
    fn copying_a_plain_snippet_bumps_uses() {
        let mut app = SnippetsApp::new_demo();
        // Seed the shared document (as the live app's per-frame sync does before
        // any event) so the usage-bump action has an authoritative row to edit.
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        // Find a snippet without placeholders.
        let id = app
            .library
            .snippets
            .iter()
            .find(|s| !s.has_placeholders())
            .map(|s| s.id)
            .expect("a placeholder-free snippet exists");
        let before = app.snippet_uses_for_test(id);
        app.copy_for_test(id, &mut doc);
        assert_eq!(app.snippet_uses_for_test(id), before + 1);
        assert!(!app.expand_open_for_test());
    }

    #[test]
    fn copying_a_templated_snippet_opens_expand() {
        let mut app = SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        let id = app
            .library
            .snippets
            .iter()
            .find(|s| s.has_placeholders())
            .map(|s| s.id)
            .expect("a templated snippet exists");
        app.copy_for_test(id, &mut doc);
        assert!(
            app.expand_open_for_test(),
            "templated copy routes through expand"
        );
    }

    #[test]
    fn new_snippet_grows_library_and_opens_it() {
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        let before = app.library_len_for_test();
        app.new_snippet_for_test(&mut doc);
        assert_eq!(app.library_len_for_test(), before + 1);
        assert!(app.open_id_for_test().is_some());
    }

    #[test]
    fn next_language_wraps() {
        let last = *Language::ALL.last().unwrap();
        assert_eq!(next_language(last), Language::ALL[0]);
    }

    #[test]
    fn sync_seeds_into_empty_doc() {
        let mut app = SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert_eq!(SnippetsDoc::list(&doc).len(), app.library_len_for_test());
    }

    // ---- responsive layout ---------------------------------------------

    #[test]
    fn wide_keeps_sidebar_docked_and_open() {
        let mut app = SnippetsApp::new();
        app.layout_at_width_for_test(1280.0).unwrap();
        assert!(
            !app.sidebar_is_overlay_for_test(),
            "wide window docks the sidebar"
        );
        assert!(
            app.sidebar_open_for_test(),
            "docked sidebar is open beside the content"
        );
        assert!(
            !app.is_push_for_test(),
            "wide window keeps list + detail side by side"
        );
    }

    #[test]
    fn narrow_collapses_sidebar_to_overlay_closed_by_default() {
        let mut app = SnippetsApp::new();
        app.layout_at_width_for_test(720.0).unwrap();
        assert!(
            app.sidebar_is_overlay_for_test(),
            "narrow window overlays the sidebar"
        );
        assert!(
            !app.sidebar_open_for_test(),
            "the overlay sidebar is closed by default, summoned by the hamburger"
        );
    }

    #[test]
    fn hamburger_toggles_the_overlay_sidebar() {
        let mut app = SnippetsApp::new();
        app.layout_at_width_for_test(720.0).unwrap();
        assert!(!app.sidebar_open_for_test());
        app.toggle_sidebar_for_test();
        assert!(
            app.sidebar_open_for_test(),
            "the toggle summons the overlay flap"
        );
        app.toggle_sidebar_for_test();
        assert!(!app.sidebar_open_for_test(), "toggling again dismisses it");
    }

    #[test]
    fn very_narrow_uses_single_pane_push() {
        let mut app = SnippetsApp::new();
        app.layout_at_width_for_test(480.0).unwrap();
        assert!(
            app.is_push_for_test(),
            "very-narrow window switches to single-pane push"
        );
        assert!(
            !app.push_showing_detail_for_test(),
            "push starts on the master list"
        );
    }

    #[test]
    fn opening_a_snippet_pushes_the_detail_when_very_narrow() {
        let mut app = SnippetsApp::new_demo();
        app.layout_at_width_for_test(480.0).unwrap();
        assert!(!app.push_showing_detail_for_test());
        app.open_first_for_test();
        assert!(
            app.push_showing_detail_for_test(),
            "selecting a snippet pushes the detail pane in"
        );
    }

    #[test]
    fn opening_a_snippet_does_not_push_when_wide() {
        let mut app = SnippetsApp::new_demo();
        app.layout_at_width_for_test(1280.0).unwrap();
        app.open_first_for_test();
        assert!(
            !app.push_showing_detail_for_test(),
            "wide layout shows both panes; opening must not trigger a push"
        );
    }

    #[test]
    fn resizing_wide_again_restores_the_docked_default() {
        let mut app = SnippetsApp::new();
        // Narrow first (overlay, closed), then widen back.
        app.layout_at_width_for_test(720.0).unwrap();
        assert!(!app.sidebar_open_for_test());
        app.layout_at_width_for_test(1280.0).unwrap();
        assert!(
            app.sidebar_open_for_test() && !app.sidebar_is_overlay_for_test(),
            "crossing back over the breakpoint restores the docked-open default"
        );
    }

    // ---- action model: affordances dispatch through the shared path ----

    /// Seed a demo app + its shared document, as the live frame order does.
    fn seeded_app() -> (SnippetsApp, AutoCommit) {
        let mut app = SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
    }

    /// The toolbar Delete button dispatches the byte-identical `EncodedAction`
    /// the Ctrl+K palette's "Delete snippet" `ActionSpec` exports — proof the two
    /// share one source of truth — and it actually removes the row.
    #[test]
    fn toolbar_delete_dispatches_the_same_action_as_the_palette() {
        let (mut app, mut doc) = seeded_app();
        let id = SnippetsDoc::list(&doc)[0].id;

        // What the palette would dispatch for this snippet's "delete".
        let resources = Widget::resources(&app, &doc);
        let snip = resources[0]
            .children
            .iter()
            .find(|r| r.id.as_str() == format!("snippets.snippet:{id}"))
            .expect("a resource for the target snippet");
        let spec = snip.actions.iter().find(|a| a.id == "delete").unwrap();
        let spec_encoded = match &spec.input {
            qos_actions::ActionInput::Immediate(e) => e.clone(),
            qos_actions::ActionInput::Prompted { .. } => panic!("delete is immediate"),
        };

        app.delete_for_test(id, &mut doc);

        let dispatched = app
            .last_dispatched_for_test()
            .expect("the button dispatched an action");
        assert_eq!(dispatched.type_id, SNIPPETS_ACTION_TYPE);
        assert_eq!(dispatched.type_id, spec_encoded.type_id);
        assert_eq!(
            dispatched.payload, spec_encoded.payload,
            "the button and the palette build the identical EncodedAction",
        );
        assert!(
            SnippetsDoc::get(&doc, id).is_none(),
            "the snippet was deleted via the dispatched action",
        );
        assert!(
            !app.library.snippets.iter().any(|s| s.id == id),
            "the library re-projected the deletion",
        );
    }

    /// The toolbar Star button dispatches the same `EncodedAction` the palette's
    /// "toggle-favorite" spec exports (both carry the flipped target value).
    #[test]
    fn toolbar_star_dispatches_the_same_action_as_the_palette() {
        let (mut app, mut doc) = seeded_app();
        // Pick an unstarred snippet so both sides compute `favorite: true`.
        let id = SnippetsDoc::list(&doc)
            .into_iter()
            .find(|s| !s.favorite)
            .map(|s| s.id)
            .expect("an unstarred seeded snippet");

        let resources = Widget::resources(&app, &doc);
        let snip = resources[0]
            .children
            .iter()
            .find(|r| r.id.as_str() == format!("snippets.snippet:{id}"))
            .unwrap();
        let spec = snip
            .actions
            .iter()
            .find(|a| a.id == "toggle-favorite")
            .unwrap();
        let spec_encoded = match &spec.input {
            qos_actions::ActionInput::Immediate(e) => e.clone(),
            qos_actions::ActionInput::Prompted { .. } => panic!("toggle-favorite is immediate"),
        };

        app.toggle_favorite_for_test(id, &mut doc);

        let dispatched = app
            .last_dispatched_for_test()
            .expect("the star dispatched an action");
        assert_eq!(
            dispatched.payload, spec_encoded.payload,
            "the star and the palette build the identical EncodedAction",
        );
        assert!(
            SnippetsDoc::get(&doc, id).unwrap().favorite,
            "the snippet is now starred"
        );
    }

    /// The full undo covenant through the real host path: a toolbar affordance
    /// dispatches its action, the host drains `take_committed_action` to record
    /// exactly one undo boundary, and Ctrl+Z reverts precisely that action.
    #[test]
    fn affordance_records_one_undo_boundary_through_the_host() {
        let mut app = SnippetsApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let id = SnippetsDoc::list(&state.doc)
            .into_iter()
            .find(|s| !s.favorite)
            .map(|s| s.id)
            .expect("an unstarred snippet");

        // Fire the toolbar Star through the shared dispatch path.
        app.toggle_favorite_for_test(id, &mut state.doc);
        assert!(SnippetsDoc::get(&state.doc, id).unwrap().favorite);

        // The host's own recorder drains the captured heads and records ONE undo
        // boundary. Going through it rather than pushing an entry by hand is the
        // point: attribution happens in there, and undo is author-scoped. The
        // node map is empty because the app widget is the root here.
        let local_author = ui::collaboration::local_user_id();
        let root: *mut dyn Widget = &mut app;
        assert!(
            ui::undo::record_dispatched_action(
                &mut state,
                root,
                &std::collections::HashMap::new(),
                &local_author,
            ),
            "the host recorded an undo boundary for the dispatched star"
        );
        assert!(
            Widget::take_committed_action(&mut app).is_none(),
            "rising-edge: the captured heads are cleared on read",
        );
        let entry = state
            .peek_last_undo()
            .expect("exactly one undo boundary recorded");
        assert_eq!(
            entry.author.as_ref(),
            Some(&local_author),
            "attributed to the local user, so their Ctrl+Z can reach it"
        );

        // Ctrl+Z reverts the star. Re-project so the library reflects the undo.
        state
            .undo_as(Some(&local_author), None, None)
            .expect("an undo entry to pop");
        app.sync(&mut state.doc, &state.root);
        assert!(
            !SnippetsDoc::get(&state.doc, id).unwrap().favorite,
            "undo reverted the star and nothing else",
        );
    }

    /// Actions make the document authoritative: a mutation applied straight to
    /// the document (as a peer's replayed action would arrive) is re-projected
    /// into the library on the next `sync`, and a local delete is not resurrected.
    #[test]
    fn steady_state_reprojects_the_document() {
        let (mut app, mut doc) = seeded_app();
        let before = app.library_len_for_test();
        let id = SnippetsDoc::list(&doc)[0].id;

        // A peer deletes a snippet — it arrives in our replica as a raw document
        // mutation (the shape a replayed `DeleteSnippet` action leaves behind).
        SnippetsDoc::remove(&mut doc, id);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            app.library_len_for_test(),
            before - 1,
            "the deletion re-projected"
        );
        assert!(
            !app.library.snippets.iter().any(|s| s.id == id),
            "and is not resurrected"
        );
    }
}
