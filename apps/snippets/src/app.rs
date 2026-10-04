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
//! The library is the [`crate::resource::Snippets`] resource the framework
//! presents; every change is a [`SnippetsAction`] submitted through its
//! dispatcher. Selection / search / the open expand-modal are view state and
//! stay local to each peer.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::widget::code_editor::CodeEditor;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Button, ButtonFace, ButtonStyle, FlapState, PushPane, SplitLayout, TextInput, Widget,
};

use crate::actions::{self, SnippetsAction};
use crate::doc::SnippetsDoc;
use crate::draw;
use crate::expand::ExpandModal;
use crate::highlight;
use crate::model::{Language, Library, Selection, SmartFolder, Snippet, SnippetId};
use crate::resource::Snippets;
use ui::framework::Dispatcher;
use ui::tilo_resource::{PlainText, text::splice_between};

const SIDEBAR_W: f64 = 232.0;
const LIST_W: f64 = 340.0;
const ROW_H: f64 = 64.0;
const SIDEBAR_ROW_H: f64 = 32.0;
const SEARCH_H: f64 = 52.0;
pub(crate) const BODY_FONT: f32 = 13.5;
pub(crate) const BODY_LINE: f64 = 20.0;

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
/// Inset of the floating top-left toggle from the content band's corner.
const TOGGLE_PAD: f64 = 10.0;
/// Where the list's search row starts below the band's top edge, and how tall
/// it is; the toggle centres itself on this row so the two share one centre
/// line when the toggle floats beside the search field.
const SEARCH_TOP: f64 = 10.0;
const SEARCH_ROW_H: f64 = SEARCH_H - 16.0;

/// Type sizes the list row stacks: title over a monospace preview line.
const ROW_TITLE_FONT: f32 = 14.5;
const ROW_SUB_FONT: f32 = 12.0;
/// Type size of the sidebar rows, the search field and the toolbar buttons.
const CONTROL_FONT: f32 = 14.0;
const BUTTON_FONT: f32 = 13.0;

/// Vertical pitches of every text-bearing band, derived once per frame from
/// the measured line boxes of the type sizes each band stacks. The `*_H`
/// constants above are the 1× design; each band grows by exactly the extra
/// height its lines gain at the current accessibility scale, so paint and the
/// per-frame hit rects always agree. At 1× this is [`Metrics::ONE`].
#[derive(Clone, Copy, Debug, PartialEq)]
struct Metrics {
    /// List row: title line + preview line.
    row_h: f64,
    /// Sidebar folder/tag row (one `CONTROL_FONT` line).
    sidebar_row_h: f64,
    /// Search field band (one `CONTROL_FONT` line) and the floating toggle
    /// that sits level with it.
    search_row_h: f64,
    toggle: f64,
    /// Growth of one `BUTTON_FONT` line — the "+ New" chip, toolbar buttons
    /// and the list header row add this.
    button_extra: f64,
    /// Measured line boxes the row and detail painters stack.
    title_line: f64,
    sub_line: f64,
    /// Detail title (24 px) and description (14 px) line boxes.
    detail_title_line: f64,
    detail_desc_line: f64,
    /// Measured monospace line of the code card (`BODY_LINE` at 1×).
    body_line: f64,
    /// Growth of one 12 px pill label — the meta pills add this.
    pill_extra: f64,
}

impl Metrics {
    /// The 1× design metrics — also what the constructor seeds before the
    /// first frame has measured anything.
    const ONE: Self = Self {
        row_h: ROW_H,
        sidebar_row_h: SIDEBAR_ROW_H,
        search_row_h: SEARCH_ROW_H,
        toggle: TOGGLE_SIZE,
        button_extra: 0.0,
        title_line: ROW_TITLE_FONT as f64 * 1.25,
        sub_line: ROW_SUB_FONT as f64 * 1.25,
        detail_title_line: 24.0 * 1.25,
        detail_desc_line: 14.0 * 1.25,
        body_line: BODY_LINE,
        pill_extra: 0.0,
    };

    /// Measure with the renderer that will paint this frame.
    fn measure(tr: &mut TextRenderer) -> Self {
        let control = draw::line_growth(tr, CONTROL_FONT);
        Self {
            row_h: ROW_H
                + draw::line_growth(tr, ROW_TITLE_FONT)
                + draw::line_growth(tr, ROW_SUB_FONT),
            sidebar_row_h: SIDEBAR_ROW_H + control,
            search_row_h: SEARCH_ROW_H + control,
            toggle: TOGGLE_SIZE + control,
            button_extra: draw::line_growth(tr, BUTTON_FONT),
            title_line: draw::line_box(tr, ROW_TITLE_FONT),
            sub_line: draw::mono_line_box(tr, ROW_SUB_FONT),
            detail_title_line: draw::line_box(tr, 24.0),
            detail_desc_line: draw::line_box(tr, 14.0),
            body_line: BODY_LINE + draw::line_growth(tr, BODY_FONT),
            pill_extra: draw::line_growth(tr, 12.0),
        }
    }

    /// Height of the list's search band (`SEARCH_H` at 1×).
    fn search_h(self) -> f64 {
        self.search_row_h + (SEARCH_H - SEARCH_ROW_H)
    }
}

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
    /// Editing cache only; every change is a splice of the open snippet's
    /// body, submitted through `body_edits`.
    body_editor: CodeEditor,
    body_identity: Option<(SnippetId, Language)>,
    /// The open snippet's body text, edited as a child of the library.
    body_edits: Option<Dispatcher<PlainText>>,
    /// Currently open snippet in the detail pane.
    open: Option<SnippetId>,
    /// A snippet this view just created or duplicated, opened as soon as the
    /// library it was added to is presented.
    pending_open: Option<SnippetId>,
    /// Ordered ids currently shown in the list pane (result of `query`).
    visible: Vec<SnippetId>,
    /// The expand-with-variables modal, when open.
    expand: Option<ExpandModal>,
    toast: Option<Toast>,

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
    /// Text-derived pitches for this frame (rows, controls, header bands),
    /// measured in `draw`; the 1× design values until the first frame.
    m: Metrics,
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

    /// The resource's dispatcher, once the framework attaches the view.
    /// Each on-screen affordance submits a [`SnippetsAction`] through it.
    dispatch: Option<Dispatcher<Snippets>>,
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

impl Action {
    fn semantic_id(&self) -> String {
        match self {
            Self::Select(Selection::Smart(smart)) => format!(
                "snippets.sidebar.smart:{}",
                smart.label().to_lowercase().replace(' ', "-")
            ),
            Self::Select(Selection::Folder(folder)) => {
                format!("snippets.sidebar.folder:{}", actions::stable_key(folder))
            }
            Self::Select(Selection::Tag(tag)) => {
                format!("snippets.sidebar.tag:{}", actions::stable_key(tag))
            }
            Self::OpenSnippet(id) => format!("snippets.snippet:{id}.open"),
            Self::NewSnippet => "snippets.library.new-snippet".into(),
            Self::Copy(id) => format!("snippets.snippet:{id}.copy"),
            Self::Expand(id) => format!("snippets.snippet:{id}.expand"),
            Self::ToggleFavorite(id) => format!("snippets.snippet:{id}.toggle-favorite"),
            Self::Duplicate(id) => format!("snippets.snippet:{id}.duplicate"),
            Self::Delete(id) => format!("snippets.snippet:{id}.delete"),
            Self::FocusSearch => "snippets.library.search".into(),
            Self::ClearSearch => "snippets.library.clear-search".into(),
            Self::SetLanguage(id, _) => format!("snippets.snippet:{id}.set-language"),
            Self::ToggleSidebar => "snippets.sidebar.toggle".into(),
            Self::DismissSidebar => "snippets.sidebar.dismiss".into(),
            Self::PushBack => "snippets.navigation.back".into(),
        }
    }

    fn accessible_label(&self, app: &SnippetsApp) -> String {
        match self {
            Self::Select(Selection::Smart(smart)) => smart.label().into(),
            Self::Select(Selection::Folder(folder)) => format!("Folder {folder}"),
            Self::Select(Selection::Tag(tag)) => format!("Tag {tag}"),
            Self::OpenSnippet(id) => app.library.get(*id).map_or_else(
                || "Open snippet".into(),
                |snippet| format!("Open {}", snippet.title),
            ),
            Self::NewSnippet => "New snippet".into(),
            Self::Copy(_) => "Copy snippet".into(),
            Self::Expand(_) => "Fill variables and copy".into(),
            Self::ToggleFavorite(id) => {
                if app.library.get(*id).is_some_and(|snippet| snippet.favorite) {
                    "Unstar snippet".into()
                } else {
                    "Star snippet".into()
                }
            }
            Self::Duplicate(_) => "Duplicate snippet".into(),
            Self::Delete(_) => "Delete snippet".into(),
            Self::FocusSearch => "Search snippets".into(),
            Self::ClearSearch => "Clear search".into(),
            Self::SetLanguage(_, language) => format!("Set language to {}", language.label()),
            Self::ToggleSidebar => "Toggle folders and tags".into(),
            Self::DismissSidebar => "Close folders and tags".into(),
            Self::PushBack => "Back to snippet list".into(),
        }
    }

    fn canonical_address(&self) -> Option<tilo_actions::ActionAddress> {
        match self {
            Self::OpenSnippet(id) => Some(actions::open_snippet_address(*id)),
            Self::Select(Selection::Tag(tag)) => Some(actions::reveal_tag_address(tag)),
            Self::NewSnippet => Some(tilo_actions::ActionAddress::new(
                "snippets.library",
                "new-snippet",
            )),
            Self::FocusSearch => Some(tilo_actions::ActionAddress::new(
                "snippets.library",
                "search",
            )),
            Self::ToggleFavorite(id) => Some(tilo_actions::ActionAddress::new(
                actions::snippet_resource_id(*id),
                "toggle-favorite",
            )),
            Self::Delete(id) => Some(tilo_actions::ActionAddress::new(
                actions::snippet_resource_id(*id),
                "delete",
            )),
            Self::SetLanguage(id, _) => Some(tilo_actions::ActionAddress::new(
                actions::snippet_resource_id(*id),
                "set-language",
            )),
            _ => None,
        }
    }
}

impl SnippetsApp {
    /// The view before the framework presents the library: empty, until the
    /// resource's state arrives.
    pub fn new() -> Self {
        let mut app = Self {
            library: Library::new(),
            selection: Selection::default(),
            search_input: TextInput::local()
                .with_kind(ui::widget::TextInputKind::Search)
                .with_id("snippets.search")
                .with_placeholder("Search  ·  try language:rust  or  tag:k8s")
                .with_font_size(14.0)
                .with_plain_chrome(),
            search_rect: Rect::ZERO,
            body_editor: CodeEditor::local()
                .with_id("snippets.body")
                .with_placeholder("snippets-body-placeholder")
                .with_font_size(BODY_FONT),
            body_identity: None,
            body_edits: None,
            open: None,
            pending_open: None,
            visible: Vec::new(),
            expand: None,
            toast: None,
            rect: Rect::ZERO,
            last_mouse: Point::ZERO,
            hit: Vec::new(),
            scroll: 0.0,
            sidebar: FlapState::open_when_docked(),
            m: Metrics::ONE,
            push: FlapState::closed_by_default(),
            push_pane: PushPane::Master,
            settled_layout: false,
            dispatch: None,
        };
        app.recompute_visible();
        app
    }

    fn recompute_visible(&mut self) {
        self.visible = self
            .library
            .query(&self.selection, self.search_input.value());
        // Keep the open snippet valid; otherwise open the first result. A
        // snippet this view just created stays open while it is on its way.
        if let Some(open) = self.open {
            if !self.visible.contains(&open) && self.pending_open != Some(open) {
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

    /// Submit one [`SnippetsAction`] — the single mutation path shared with
    /// the palette — through the resource's dispatcher. The host checks the
    /// participant's permission, applies it as one undo step, and presents
    /// the changed library.
    fn submit(&self, action: &SnippetsAction) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.apply(action);
        }
    }

    /// Show the library the resource holds: a local edit, a collaborator's,
    /// an undo.
    pub(crate) fn show_library(&mut self, state: &[Snippet]) {
        self.library = Library {
            snippets: state.to_vec(),
        };
        if let Some(id) = self.pending_open
            && self.library.get(id).is_some()
        {
            self.pending_open = None;
            self.open = Some(id);
        }
        self.recompute_visible();
        self.sync_body_editor();
    }

    /// Edit through the resource's dispatcher from now on.
    pub(crate) fn attach_dispatcher(&mut self, dispatcher: Dispatcher<Snippets>) {
        self.dispatch = Some(dispatcher);
        self.body_identity = None;
        self.sync_body_editor();
    }

    fn sync_body_editor(&mut self) {
        let snippet = self.open.and_then(|id| self.library.get(id));
        let identity = snippet.map(|s| (s.id, s.language));
        if self.body_identity.map(|(id, _)| id) != identity.map(|(id, _)| id) {
            self.body_edits = identity.and_then(|(id, _)| {
                self.dispatch.as_ref().map(|dispatch| {
                    dispatch.child::<PlainText>(&actions::body_resource_id(id), move |edit| {
                        actions::edit_body_action(id, edit)
                    })
                })
            });
        }
        if self.body_identity != identity {
            self.body_editor
                .set_highlighter(identity.map(|(_, language)| {
                    Box::new(highlight::EditorHighlighter::new(language))
                        as Box<dyn ui::widget::code_editor::Highlighter>
                }));
        }
        let text = snippet.map_or("", |s| s.body.as_str());
        if self.body_editor.value() != text {
            self.body_editor.set_text(text);
        }
        if self.body_identity.map(|(id, _)| id) != identity.map(|(id, _)| id) {
            self.body_editor.set_cursor_pos(0);
            self.body_editor.set_focused(false);
        }
        self.body_identity = identity;
        // Adopting document state is not an authored edit.
        self.body_editor.take_dirty();
    }

    /// Submit what the user typed in the body as one splice of the stored
    /// text, applied now so the next keystroke splices the text it made.
    /// Typing a word is one undo step. A refused edit (a viewer's) writes
    /// nothing, and the editor shows the stored text again.
    fn flush_body_edit(&mut self, doc: &mut AutoCommit) {
        let Some(text) = self.body_editor.take_dirty() else {
            return;
        };
        let Some(id) = self.open else {
            return;
        };
        let stored = SnippetsDoc::get(doc, id).map(|snippet| snippet.body);
        let applied = match (&self.body_edits, &stored) {
            (Some(edits), Some(stored)) => {
                splice_between(stored, &text).is_none_or(|edit| edits.apply_now(doc, &edit).is_ok())
            }
            _ => false,
        };
        if !applied {
            self.body_editor
                .set_text(stored.as_deref().unwrap_or_default());
            self.body_editor.take_dirty();
        }
    }

    fn body_available(&self) -> bool {
        self.open.is_some()
            && self.expand.is_none()
            && !(self.sidebar_layout() == SplitLayout::Overlay && self.sidebar_visible())
            && (!self.is_push_layout() || self.push_pane == PushPane::Detail)
    }

    fn route_body_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let modifiers = *ctx.modifiers;
        let state = &mut *ctx.state;
        if !self.body_available() {
            self.body_editor.set_focused(false);
            return false;
        }
        let inside = self.body_editor.rect().contains(self.last_mouse);
        let captured = self.body_editor.wants_pointer_capture();
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            self.body_editor.set_focused(inside);
            if inside {
                self.search_input.set_focus(false);
            }
        }
        let focused = self.body_editor.is_focused();
        if let WindowEvent::KeyboardInput { event, .. } = event {
            // Preserve app navigation shortcuts; other editing keys belong to
            // TextInput, including selection copy and the shared undo path.
            let global = matches!(&event.logical_key, Key::Named(NamedKey::Escape))
                || ((modifiers.state().control_key() || modifiers.state().super_key())
                    && matches!(&event.logical_key, Key::Character(key) if key.eq_ignore_ascii_case("f") || key.eq_ignore_ascii_case("n")));
            if global {
                self.body_editor.set_focused(false);
                return false;
            }
        }
        let route = match event {
            WindowEvent::CursorMoved { .. } | WindowEvent::Focused(false) => true,
            WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. } => inside || captured,
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => focused,
            _ => false,
        };
        if !route {
            return false;
        }
        let mut handled = false;
        self.body_editor.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: &modifiers,
                state,
                path: ctx.path,
                handled: &mut handled,
                focus_target: ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            },
        );
        self.flush_body_edit(state);
        handled
    }

    fn new_snippet(&mut self) {
        let folder = match &self.selection {
            Selection::Folder(f) => f.clone(),
            _ => String::new(),
        };
        // The uuid is minted here, at authoring time, and carried in the action
        // payload so replay reproduces the identical row.
        let s = Snippet::blank(&folder);
        self.open_when_presented(s.id);
        self.submit(&actions::create_snippet_action(s));
        self.toast("New snippet created");
    }

    /// Open `id`, which this view just submitted, once it is presented.
    fn open_when_presented(&mut self, id: SnippetId) {
        self.pending_open = Some(id);
        self.open = Some(id);
    }

    fn copy(&mut self, id: SnippetId) {
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
            self.record_use(id);
            self.toast("Copied to clipboard");
        }
    }

    /// Record one use of a snippet (drives "Most Used" + the usage hint) through
    /// the action path, so the bump replicates and is undoable like any mutation.
    fn record_use(&self, id: SnippetId) {
        self.submit(&actions::record_use_action(id));
    }

    fn start_expand(&mut self, id: SnippetId) {
        if let Some(s) = self.library.get(id) {
            self.expand = Some(ExpandModal::new(s));
        }
    }

    fn toggle_favorite(&self, id: SnippetId) {
        let cur = self.library.get(id).is_some_and(|s| s.favorite);
        self.submit(&actions::set_favorite_action(id, !cur));
    }

    fn duplicate(&mut self, id: SnippetId) {
        if let Some(src) = self.library.get(id).cloned() {
            let mut copy = src.clone();
            copy.id = uuid::Uuid::new_v4();
            copy.title = format!("{} (copy)", src.title);
            copy.favorite = false;
            copy.uses = 0;
            self.open_when_presented(copy.id);
            self.submit(&actions::create_snippet_action(copy));
            self.toast("Duplicated");
        }
    }

    fn delete(&mut self, id: SnippetId) {
        self.submit(&actions::delete_snippet_action(id));
        self.toast("Deleted");
    }

    fn set_language(&self, id: SnippetId, lang: Language) {
        self.submit(&actions::set_language_action(id, lang));
    }

    fn toast(&mut self, message: &str) {
        self.toast = Some(Toast {
            message: message.to_string(),
            ttl: 90,
        });
    }

    fn activate(&mut self, action: Action) {
        match action {
            Action::Select(sel) => self.select(sel),
            Action::OpenSnippet(id) => self.open_snippet(id),
            Action::NewSnippet => self.new_snippet(),
            Action::Copy(id) => self.copy(id),
            Action::Expand(id) => self.start_expand(id),
            Action::ToggleFavorite(id) => self.toggle_favorite(id),
            Action::Duplicate(id) => self.duplicate(id),
            Action::Delete(id) => self.delete(id),
            Action::FocusSearch => {
                self.body_editor.set_focused(false);
                self.search_input.set_focus(true);
            }
            Action::ClearSearch => {
                self.search_input.set_text("");
                self.recompute_visible();
            }
            Action::SetLanguage(id, lang) => self.set_language(id, lang),
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

    /// Stage the view for a screenshot scenario, once its library has been
    /// presented and laid out (the live app eases via `update`):
    ///
    /// * every scenario opens a substantial, templated snippet so the detail
    ///   pane shows off the highlighter, line gutter and variable affordance;
    /// * `"narrow"` opens the overlay sidebar over the content + scrim;
    /// * `"detail"` shows the single-pane push on the detail with a Back chip.
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        if let Some(richest) = self.richest_visible() {
            self.open = Some(richest);
            self.sync_body_editor();
        }
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

    /// The visible snippet with the most body lines, preferring one that
    /// carries placeholders.
    fn richest_visible(&self) -> Option<SnippetId> {
        self.visible.iter().copied().max_by_key(|id| {
            let s = self.library.get(*id);
            let lines = s.map_or(0, |s| s.body.lines().count());
            let templated = usize::from(s.is_some_and(Snippet::has_placeholders));
            (templated, lines)
        })
    }

    // ---- test accessors -------------------------------------------------

    #[cfg(test)]
    pub(crate) fn open_id_for_test(&self) -> Option<SnippetId> {
        self.open
    }

    #[cfg(test)]
    pub(crate) fn visible_count_for_test(&self) -> usize {
        self.visible.len()
    }

    #[cfg(test)]
    pub(crate) fn set_search_for_test(&mut self, q: &str) {
        self.search_input.set_text(q);
        self.recompute_visible();
    }

    #[cfg(test)]
    pub(crate) fn expand_open_for_test(&self) -> bool {
        self.expand.is_some()
    }

    // ---- responsive test accessors -------------------------------------

    /// Lay the app out at `width` x 800 through the real `build_layout` /
    /// `apply_layout` passes (so `rect` + the layout reconcile run exactly as
    /// in a live frame), then settle any pending reveal animation.
    ///
    /// # Errors
    /// Propagates [`taffy::TaffyError`] if `compute_layout` fails — never in
    /// practice, since `node` was just created in the same tree.
    #[cfg(test)]
    pub(crate) fn layout_at_width_for_test(&mut self, width: f64) -> taffy::TaffyResult<()> {
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

    #[cfg(test)]
    pub(crate) fn sidebar_is_overlay_for_test(&self) -> bool {
        self.sidebar_layout() == SplitLayout::Overlay
    }

    #[cfg(test)]
    pub(crate) fn sidebar_open_for_test(&self) -> bool {
        self.sidebar_open()
    }

    #[cfg(test)]
    pub(crate) fn is_push_for_test(&self) -> bool {
        self.is_push_layout()
    }

    #[cfg(test)]
    pub(crate) fn push_showing_detail_for_test(&self) -> bool {
        self.push_pane == PushPane::Detail
    }

    #[cfg(test)]
    pub(crate) fn toggle_sidebar_for_test(&mut self) {
        self.activate(Action::ToggleSidebar);
    }

    #[cfg(test)]
    pub(crate) fn open_first_for_test(&mut self) {
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

impl SnippetsApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
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
            actions::SnippetsOperation::RevealTag { tag } => {
                if !self.library.tags().iter().any(|(known, _)| known == &tag) {
                    return Err("tag no longer exists".into());
                }
                self.search_input.set_text("");
                self.select(Selection::Tag(tag));
            }
            actions::SnippetsOperation::FocusSearch => {
                self.body_editor.set_focused(false);
                self.search_input.set_focus(true);
            }
        }
        Ok(true)
    }
}

impl ui::widget::Widget for SnippetsApp {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::undo::UndoPoints
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::SNIPPETS_OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        self.sync_body_editor();
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
        // Every text-bearing band takes its pitch from this frame's measured
        // line boxes; the hit rects recorded below read the same values.
        self.m = Metrics::measure(tr);
        self.sync_body_editor();
        self.body_editor.set_rect(Rect::ZERO);
        let c = &theme.tokens.colors;
        // Whole-window background.
        draw::fill_rect(scene, t, self.rect, c.surfaces.base);

        // The content band (list + detail) is inset by the docked sidebar,
        // or spans the full width when the sidebar is an overlay flap.
        let band = self.content_band();
        self.draw_content_band(scene, t, tr, theme, band);
        if !self.body_available() {
            self.body_editor.set_rect(Rect::ZERO);
            self.body_editor.set_focused(false);
        }

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
                ui::surface::edge_on(c.surfaces.base),
            );
        }

        // The hamburger toggle is shown whenever the sidebar isn't fully
        // docked-open, so a collapsed rail can be summoned and an overlay
        // rail dismissed. It records its own hit region.
        let presence = self.toggle_presence();
        if presence > 0.001 {
            // Fade with the docked rail's ease instead of popping at its end.
            let fading = presence < 0.999;
            if fading {
                let clip = self.toggle_rect(band).inflate(2.0, 2.0);
                scene.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    presence as f32,
                    t,
                    &clip,
                );
            }
            self.draw_toggle(scene, t, theme, band, presence);
            if fading {
                scene.pop_layer();
            }
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Track the cursor for hit-testing.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }

        // The expand modal is modal: it owns all input while open.
        if self.handle_expand_event(event, ctx) {
            *ctx.handled = true;
            return;
        }

        if self.route_body_event(event, ctx) {
            *ctx.handled = true;
            return;
        }

        if self.route_pointer_to_search(event, ctx) {
            *ctx.handled = true;
            return;
        }

        self.dispatch_event(event, ctx);
    }

    fn needs_animation_frame(&self) -> bool {
        self.toast.is_some()
            || self.sidebar.is_animating(self.sidebar_layout())
            || self.push.is_animating(SplitLayout::Docked)
    }

    fn update(&mut self, _state: &mut AppModel, _path: &ModelPath) -> bool {
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

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn test_id(&self) -> Option<&str> {
        Some("snippets.application")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        if let Some(modal) = &self.expand {
            modal.ime_cursor_rect()
        } else if self.body_editor.is_focused() {
            self.body_editor.ime_cursor_rect()
        } else {
            self.search_input.ime_cursor_rect()
        }
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        if let Some(modal) = &self.expand {
            modal.ime_purpose()
        } else if self.body_editor.is_focused() {
            self.body_editor.ime_purpose()
        } else {
            self.search_input.ime_purpose()
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if let Some(modal) = &self.expand {
            modal.cursor_shape(point)
        } else if self.body_available() && self.body_editor.rect().contains(point) {
            self.body_editor.cursor_shape(point)
        } else {
            self.search_input.cursor_shape(point)
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        if let Some(modal) = self.expand.as_ref().and_then(ExpandModal::accessibility) {
            return Some(modal);
        }

        let mut seen = std::collections::HashSet::new();
        let mut children: Vec<AccessNode> = self
            .hit
            .iter()
            .filter_map(|(bounds, action)| {
                let target = action.semantic_id();
                seen.insert(target.clone()).then(|| {
                    let role = if matches!(action, Action::FocusSearch) {
                        Role::TextInput
                    } else if matches!(action, Action::ToggleFavorite(_)) {
                        Role::ToggleButton
                    } else {
                        Role::Button
                    };
                    AccessNode::new(role)
                        .with_name(action.accessible_label(self))
                        .with_action_target(target)
                        .with_bounds(*bounds)
                        .with_state(AccessState {
                            focusable: true,
                            focused: matches!(action, Action::FocusSearch)
                                && self.search_input.is_focused(),
                            checked: match action {
                                Action::ToggleFavorite(id) => {
                                    self.library.get(*id).map(|snippet| snippet.favorite)
                                }
                                _ => None,
                            },
                            ..Default::default()
                        })
                })
            })
            .collect();
        if self.body_available()
            && self.body_editor.rect().area() > 0.0
            && let Some(node) = self.body_editor.accessibility()
        {
            children.push(node.with_action_target("snippets.body"));
        }
        Some(
            AccessNode::new(Role::Window)
                .with_name("Snippets")
                .with_semantic_key("snippets.application")
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = ui::doc::document(state, path);
        if let Some(mut modal) = self.expand.take() {
            let Some(outcome) = modal.perform_semantic_action(target, action, state, path) else {
                self.expand = Some(modal);
                return false;
            };
            self.finish_expand(modal, outcome);
            return true;
        }
        if target == "snippets.body" && self.open.is_some() && self.expand.is_none() {
            self.sync_body_editor();
            self.search_input.set_focus(false);
            if matches!(action, ui::a11y::Action::Focus) {
                self.body_editor.set_focused(true);
                return true;
            }
            let accepted = Widget::perform_action(&mut self.body_editor, action, state, path);
            self.flush_body_edit(state);
            return accepted;
        }
        if !matches!(action, ui::a11y::Action::Activate | ui::a11y::Action::Focus) {
            return false;
        }
        let Some(hit_action) = self.hit.iter().find_map(|(_, candidate)| {
            (candidate.semantic_id() == target).then(|| candidate.clone())
        }) else {
            return false;
        };
        if matches!(action, ui::a11y::Action::Focus) && !matches!(hit_action, Action::FocusSearch) {
            return true;
        }
        self.activate(hit_action);
        true
    }

    fn wants_pointer_capture(&self) -> bool {
        self.body_available() && self.body_editor.wants_pointer_capture()
    }

    fn wants_tab_input(&self) -> bool {
        self.body_available() && self.body_editor.is_focused()
    }

    fn text_review(&self) -> Option<(usize, String)> {
        if self.body_available() && self.body_editor.is_focused() {
            self.body_editor.text_review()
        } else {
            self.search_input.text_review()
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
            self.body_editor
                .draw_overlay(scene, transform, tr, theme, presence);
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

impl ui::undo::UndoPoints for SnippetsApp {
    fn get_state(&self) -> Option<String> {
        serde_json::to_string(&(self.open, self.body_editor.get_state())).ok()
    }
    fn set_state(&mut self, state: &str) {
        if let Ok((id, Some(caret))) =
            serde_json::from_str::<(Option<SnippetId>, Option<String>)>(state)
            && id == self.open
        {
            self.body_editor.set_state(&caret);
        }
    }
}

impl ui::widget::controls::ActionControls for SnippetsApp {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        let mut seen = std::collections::HashSet::new();
        self.hit
            .iter()
            .filter_map(|(_, action)| {
                let control_id = action.semantic_id();
                seen.insert(control_id.clone())
                    .then(|| ui::widget::ControlBindingMetadata {
                        control_id: control_id.clone(),
                        kind: action.canonical_address().map_or_else(
                            || ui::widget::ControlBindingKind::SemanticAction(control_id),
                            ui::widget::ControlBindingKind::Action,
                        ),
                    })
            })
            .collect()
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
            ui::surface::edge_on(c.surfaces.base),
        );
    }

    // ---- toggles --------------------------------------------------------

    /// Whether the top-left toggle (hamburger / Back) should be shown. It
    /// appears whenever the primary sidebar isn't fully docked-open (so it
    /// can be summoned / dismissed), or while the push stack shows the detail
    /// (so the user can go Back to the list).
    /// Eased visibility of the floating toggle in `0..=1`. As an overlay
    /// rail's summoner, or as the Back chevron of a pushed detail pane, it is
    /// simply present. Beside a docked rail it fades out as the rail eases
    /// open and back in as it collapses — the same eased reveal the rail
    /// itself follows — so it never pops on the last frame of the animation,
    /// and the search field's leading inset can ease with it.
    fn toggle_presence(&self) -> f64 {
        let push_in_detail = self.is_push_layout() && self.push.reveal() > 0.5;
        if push_in_detail || self.sidebar_layout() == SplitLayout::Overlay {
            return 1.0;
        }
        (1.0 - self.sidebar.reveal()).clamp(0.0, 1.0)
    }

    /// The floating toggle's rect inside `band`: inset from the left edge and
    /// vertically centred on the list's search row, so it sits level with the
    /// field it floats beside.
    fn toggle_rect(&self, band: Rect) -> Rect {
        let bx = band.x0 + TOGGLE_PAD;
        let by = band.y0 + SEARCH_TOP + (self.m.search_row_h - self.m.toggle) / 2.0;
        Rect::new(bx, by, bx + self.m.toggle, by + self.m.toggle)
    }

    /// Draw the top-left toggle button + glyph and record its hit region.
    /// In single-pane push showing the detail it's a Back chevron (returns to
    /// the list); otherwise it's a hamburger (open) / close-X (dismiss the
    /// overlay sidebar).
    fn draw_toggle(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        band: Rect,
        presence: f64,
    ) {
        let c = &theme.tokens.colors;
        let btn = self.toggle_rect(band);
        ui::surface::card(scene, t, btn, theme.radii_md(), c.surfaces.raised);

        let cx = btn.center().x;
        let cy = btn.center().y;
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
            if presence > 0.001 {
                self.record(btn, Action::PushBack);
            }
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
            if presence > 0.001 {
                self.record(btn, Action::ToggleSidebar);
            }
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
            if presence > 0.001 {
                self.record(btn, Action::ToggleSidebar);
            }
        }
    }

    /// The per-`WindowEvent`-kind dispatch, once the expand modal and the
    /// embedded search editor have both had first refusal.
    fn dispatch_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let modifiers = *ctx.modifiers;
        let state = &mut *ctx.state;
        let path = ctx.path;
        let focus_target = &mut *ctx.focus_target;
        let node_id = ctx.node_id;
        let taffy = ctx.taffy;
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor: ctx.scale_factor,
                        modifiers: &modifiers,
                        state,
                        path,
                        handled: &mut *ctx.handled,
                        focus_target,
                        node_id,
                        taffy,
                    },
                );
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * BODY_LINE,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y / ctx.scale_factor,
                };
                self.scroll = (self.scroll - dy).max(0.0);
                *ctx.handled = true;
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
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor: ctx.scale_factor,
                        modifiers: &modifiers,
                        state,
                        path,
                        handled: &mut *ctx.handled,
                        focus_target,
                        node_id,
                        taffy,
                    },
                    logical_key,
                );
            }
            WindowEvent::Ime(_)
                if self.search_input.is_focused()
                    && self.route_search_event(
                        event,
                        &mut ui::widget::EventCtx {
                            scale_factor: ctx.scale_factor,
                            modifiers: &modifiers,
                            state,
                            path,
                            handled: &mut *ctx.handled,
                            focus_target,
                            node_id,
                            taffy,
                        },
                    ) =>
            {
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    /// Route input to the expand-with-variables modal while it's open — it
    /// owns all input, so the panes beneath and the rest of `handle_event`
    /// are never consulted. Returns `true` once a modal was open and has
    /// consumed the event (closing, copying and toasting side effects are
    /// already applied); `false` when there is no modal to route to.
    fn handle_expand_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let Some(mut modal) = self.expand.take() else {
            return false;
        };
        let outcome = modal.handle_event(event, ctx, self.last_mouse);
        self.finish_expand(modal, outcome);
        true
    }

    fn finish_expand(&mut self, modal: ExpandModal, outcome: ExpandOutcome) {
        match outcome {
            ExpandOutcome::Stay => {
                self.expand = Some(modal);
            }
            ExpandOutcome::Cancel => {}
            ExpandOutcome::Copy(text, id) => {
                ui::clipboard_copy(&text);
                self.record_use(id);
                self.toast("Expanded and copied");
            }
        }
    }

    /// Feed pointer motion and clicks to the embedded search editor. The
    /// editor isn't mounted as a taffy child, so its hit testing needs this
    /// explicit routing rather than the normal event-tree dispatch. Returns
    /// `true` when the editor consumed a click and the caller should stop
    /// routing the event further.
    fn route_pointer_to_search(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            let _ = self.route_search_event(event, ctx);
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
            && self.route_search_event(event, ctx)
    }

    fn handle_left_click(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(action) = self.action_at(self.last_mouse) {
            // Clicking anywhere that isn't the search field blurs it.
            if !matches!(action, Action::FocusSearch) {
                self.search_input.set_focus(false);
            }
            let search_click = matches!(action, Action::FocusSearch);
            self.activate(action);
            if search_click {
                let _ = self.route_search_event(event, ctx);
            }
            *ctx.handled = true;
        } else {
            self.search_input.set_focus(false);
        }
    }

    fn handle_keyboard_input(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        logical_key: &Key,
    ) {
        let mods = ctx.modifiers.state();
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
                    *ctx.handled = true;
                    return;
                }
                "n" => {
                    self.new_snippet();
                    *ctx.handled = true;
                    return;
                }
                "c" if !self.search_input.is_focused() => {
                    if let Some(id) = self.open {
                        self.copy(id);
                    }
                    *ctx.handled = true;
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
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.search_input.set_focus(false);
                    *ctx.handled = true;
                }
                _ => {
                    if self.route_search_event(event, ctx) {
                        *ctx.handled = true;
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
                self.activate(Action::PushBack);
                *ctx.handled = true;
            } else if self.sidebar_layout() == SplitLayout::Overlay && self.sidebar_open() {
                self.activate(Action::DismissSidebar);
                *ctx.handled = true;
            } else if !self.search_input.value().is_empty() {
                self.search_input.set_text("");
                self.recompute_visible();
                *ctx.handled = true;
            }
        }
    }

    /// Hand `event` to the search field; `true` when the field used it. The
    /// field's own handled flag stays local: the caller decides what the
    /// event means for the rest of the app.
    fn route_search_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        let mut input_handled = false;
        self.search_input.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: ctx.scale_factor,
                modifiers: ctx.modifiers,
                state: &mut *ctx.state,
                path: ctx.path,
                handled: &mut input_handled,
                focus_target: &mut *ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            },
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
            area.y0 + self.m.toggle + 14.0
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
                &mut PaintCtx::new(scene, t, tr, theme),
                area,
                y,
                smart.label(),
                Some(count),
                active,
                0,
            );
            self.record(row, Action::Select(Selection::Smart(smart)));
            y += self.m.sidebar_row_h;
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
                &mut PaintCtx::new(scene, t, tr, theme),
                area,
                y,
                &folder,
                Some(count),
                active,
                1,
            );
            self.record(row, Action::Select(Selection::Folder(folder.clone())));
            y += self.m.sidebar_row_h;
        }

        y += 10.0;
        section_header(scene, t, tr, theme, "TAGS", area.x0 + pad, y);
        y += 22.0;
        for (tag, cnt) in self.library.tags() {
            let active = matches!(&self.selection, Selection::Tag(x) if *x == tag);
            let label = format!("#{tag}");
            let row = self.sidebar_row(
                &mut PaintCtx::new(scene, t, tr, theme),
                area,
                y,
                &label,
                Some(cnt),
                active,
                1,
            );
            self.record(row, Action::Select(Selection::Tag(tag.clone())));
            y += self.m.sidebar_row_h;
        }
    }

    fn sidebar_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        y: f64,
        label: &str,
        count: Option<usize>,
        active: bool,
        indent: u32,
    ) -> Rect {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let pad = 10.0;
        let row = Rect::new(
            area.x0 + 8.0,
            y,
            area.x1 - 8.0,
            y + self.m.sidebar_row_h - 4.0,
        );
        if active {
            draw::fill_round(ctx.scene, ctx.transform, row, theme.radii_md(), c.accent[5]);
        } else if row.contains(self.last_mouse) {
            draw::fill_round(
                ctx.scene,
                ctx.transform,
                row,
                theme.radii_md(),
                c.lift(0.35),
            );
        }
        let fg = if active { c.text_primary } else { c.neutral[7] };
        let x = row.x0 + pad + f64::from(indent) * 12.0;
        let ty = y + (row.height() - draw::line_box(ctx.text, CONTROL_FONT)) / 2.0;
        ctx.text(
            label,
            &TextStyle::new(14.0).with_weight(TextWeight::Medium),
            fg,
            Point::new(x, ty),
            None,
        );
        if let Some(shown) = count {
            let count_label = shown.to_string();
            let count_w = f64::from(ctx.text.measure(&count_label, 12.0, None).0);
            let cfg = if active { c.accent[8] } else { c.text_dim };
            ctx.text(
                &count_label,
                &TextStyle::new(12.0),
                cfg,
                Point::new(row.x1 - pad - count_w, ty + 1.0),
                None,
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

        // Search bar. When the floating toggle (hamburger / close) sits in
        // this corner, the field starts past it instead of underneath it —
        // the same rect feeds the shared input's layout and the hit region,
        // so paint and pointer routing agree.
        // The field's leading inset eases with the toggle's presence, so it
        // slides beside the fading toggle instead of stepping 44 px on the
        // last frame of the rail's animation.
        let pad = 12.0;
        let lead = pad + (self.toggle_rect(area).x1 - area.x0) * self.toggle_presence();
        let search_rect = Rect::new(
            area.x0 + lead,
            area.y0 + SEARCH_TOP,
            area.x1 - pad,
            area.y0 + SEARCH_TOP + self.m.search_row_h,
        );
        self.search_rect = search_rect;
        let focused = self.search_input.is_focused();
        ui::surface::card(scene, t, search_rect, theme.radii_md(), c.surfaces.base);
        if focused {
            draw::stroke_round(scene, t, search_rect, theme.radii_md(), c.border_focus, 1.5);
        }
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
            PaintCtx::new(scene, t, tr, theme).text(
                "×",
                &TextStyle::new(18.0),
                c.text_dim,
                Point::new(
                    clear.x0 + 6.0,
                    search_rect.y0 + (search_rect.height() - 15.0) / 2.0 - 2.0,
                ),
                None,
            );
            self.record(clear, Action::ClearSearch);
        }

        // "+ New" header row with the result count.
        let header_y = area.y0 + self.m.search_h() + 4.0;
        let count_label = format!("{} snippets", self.visible.len());
        PaintCtx::new(scene, t, tr, theme).text(
            &count_label,
            &TextStyle::new(12.0).with_weight(TextWeight::Medium),
            c.text_dim,
            Point::new(area.x0 + pad, header_y),
            None,
        );
        let new_label = "+ New";
        let (nw, nh) = tr.measure(new_label, BUTTON_FONT, None);
        let nw = f64::from(nw) + 20.0;
        // The chip is the 1× 22 px grown by its label line; the label centres
        // on its measured box.
        let new_rect = Rect::new(
            area.x1 - pad - nw,
            header_y - 4.0,
            area.x1 - pad,
            header_y + 18.0 + self.m.button_extra,
        );
        draw::fill_round(scene, t, new_rect, theme.radii_md(), c.accent[5]);
        PaintCtx::new(scene, t, tr, theme).text(
            new_label,
            &TextStyle::new(BUTTON_FONT).with_weight(TextWeight::Semibold),
            c.text_primary,
            Point::new(
                new_rect.x0 + 10.0,
                new_rect.y0 + (new_rect.height() - f64::from(nh)) / 2.0,
            ),
            None,
        );
        self.record(new_rect, Action::NewSnippet);

        // Snippet rows (scrollable region).
        let list_top = header_y + 26.0 + self.m.button_extra;
        let mut y = list_top - self.scroll;
        let ids = self.visible.clone();
        for id in ids {
            let Some(s) = self.library.get(id).cloned() else {
                continue;
            };
            let row = Rect::new(area.x0 + 6.0, y, area.x1 - 6.0, y + self.m.row_h - 6.0);
            // Cull rows scrolled out of view but still record the hit so
            // clicks resolve; skip drawing if fully off-pane.
            if row.y1 >= list_top - 4.0 && row.y0 <= area.y1 {
                self.draw_list_row(scene, t, tr, theme, row, &s);
            }
            self.record(row, Action::OpenSnippet(id));
            y += self.m.row_h;
        }

        if self.visible.is_empty() {
            self.draw_list_empty(scene, t, tr, theme, area, list_top);
        }
    }

    /// The list pane's zero-state. An empty library gets a primary "New snippet"
    /// action; a non-empty library filtered down to nothing gets a "no match"
    /// note with a Clear-search affordance.
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
            self.draw_empty_new_snippet_cta(scene, t, tr, theme, cx, y, area.width() - 24.0);
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
        ui::surface::card(
            scene,
            t,
            tile_rect,
            theme.radii_md() + 4.0,
            c.surfaces.raised,
        );
        let glyph = "{ }";
        let (gw, gh) = tr.measure(glyph, 22.0, None);
        let gw = f64::from(gw);
        PaintCtx::new(scene, t, tr, theme).text(
            glyph,
            &TextStyle::new(22.0).with_weight(TextWeight::Bold),
            c.accent[6],
            Point::new(cx - gw / 2.0, top + (tile - f64::from(gh)) / 2.0),
            None,
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
        let (hw, hh) = tr.measure(headline, 17.0, None);
        let hw = f64::from(hw);
        PaintCtx::new(scene, t, tr, theme).text(
            headline,
            &TextStyle::new(17.0).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(cx - hw / 2.0, y),
            None,
        );
        // Stacked lines step on their measured boxes (26 / 19 px at 1×).
        y += f64::from(hh) + 4.75;
        // Sub-copy, wrapped to the available width so it never clips.
        for line in wrap_center(sub, tr, 13.0, area.width() - 48.0) {
            let (lw, lh) = tr.measure(&line, 13.0, None);
            let lw = f64::from(lw);
            PaintCtx::new(scene, t, tr, theme).text(
                &line,
                &TextStyle::new(13.0),
                c.text_dim,
                Point::new(cx - lw / 2.0, y),
                None,
            );
            y += f64::from(lh) + 2.75;
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
        max_w: f64,
    ) {
        let c = &theme.tokens.colors;
        let label = "New snippet";
        let (lw, lh) = tr.measure(label, 13.5, None);
        let lw = f64::from(lw);
        let bw = lw + 34.0;
        // 36 px at 1×, grown by the label's measured line growth.
        let bh = 36.0 + draw::line_growth(tr, 13.5);
        let btn = Rect::new(cx - bw / 2.0, y, cx + bw / 2.0, y + bh);
        let hovered = btn.contains(self.last_mouse);
        draw::fill_round(
            scene,
            t,
            btn,
            theme.radii_md(),
            if hovered { c.accent[4] } else { c.accent[5] },
        );
        PaintCtx::new(scene, t, tr, theme).text(
            label,
            &TextStyle::new(13.5).with_weight(TextWeight::Semibold),
            c.text_primary,
            Point::new(btn.x0 + 17.0, btn.y0 + (bh - f64::from(lh)) / 2.0),
            None,
        );
        self.record(btn, Action::NewSnippet);
        // A quiet hint at the search grammar.
        let hint_y = btn.y1 + 22.0;
        // Elided to the list's width so it never runs off the pane.
        let hint = tr.elide_to_width(
            "Tip: filter with  language:rust  or  tag:k8s",
            11.5,
            max_w as f32,
        );
        let hint = hint.as_str();
        let hw = f64::from(tr.measure(hint, 11.5, None).0);
        PaintCtx::new(scene, t, tr, theme).text(
            hint,
            &TextStyle::new(11.5),
            c.text_dim,
            Point::new(cx - hw / 2.0, hint_y),
            None,
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
        let (lw, lh) = tr.measure(label, 13.0, None);
        let lw = f64::from(lw);
        let bw = lw + 30.0;
        let bh = 34.0 + draw::line_growth(tr, 13.0);
        let btn = Rect::new(cx - bw / 2.0, y, cx + bw / 2.0, y + bh);
        let hovered = btn.contains(self.last_mouse);
        ui::surface::card(
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
        PaintCtx::new(scene, t, tr, theme).text(
            label,
            &TextStyle::new(13.0).with_weight(TextWeight::Semibold),
            c.text_primary,
            Point::new(btn.x0 + 15.0, btn.y0 + (bh - f64::from(lh)) / 2.0),
            None,
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
            draw::stroke_round(scene, t, row, theme.radii_md(), c.accent[5], 1.0);
        } else if row.contains(self.last_mouse) {
            draw::fill_round(scene, t, row, theme.radii_md(), c.lift(0.35));
        }
        let pad = 12.0;
        // The row stacks the title line over the preview line (10 px top pad,
        // 1.875 px between at 1×); every band below reads the measured boxes
        // so the two lines and the badge never collide at any text scale.
        let title_top = row.y0 + 10.0;
        let sub_top = title_top + self.m.title_line + 1.875;
        // Language sigil badge: sized from its measured label (30 × 20 px at
        // 1×) and centred on the title line.
        let sig = s.language.sigil();
        let (sw, sh) = tr.measure(sig, 11.0, None);
        let badge_h = f64::from(sh) + 6.25;
        let badge_w = (f64::from(sw) + 12.0).max(30.0);
        let badge_top = title_top + (self.m.title_line - badge_h) / 2.0 + 0.9;
        let badge = Rect::new(
            row.x0 + pad,
            badge_top,
            row.x0 + pad + badge_w,
            badge_top + badge_h,
        );
        draw::fill_round(scene, t, badge, 6.0, lang_tint(theme, s.language));
        PaintCtx::new(scene, t, tr, theme).text(
            sig,
            &TextStyle::new(11.0).with_weight(TextWeight::Bold),
            c.neutral[0],
            Point::new(
                badge.x0 + (badge.width() - f64::from(sw)) / 2.0,
                badge.y0 + (badge_h - f64::from(sh)) / 2.0,
            ),
            None,
        );

        let tx = badge.x1 + 10.0;

        // Trailing usage / placeholder hint — measured first so the title is
        // elided to the width that remains beside it instead of running
        // underneath it when the row is narrow or the text is large.
        let hint = if s.has_placeholders() {
            format!("{} vars", s.placeholders().len())
        } else if s.uses > 0 {
            format!("{}×", s.uses)
        } else {
            String::new()
        };
        let (hw, hh) = tr.measure(&hint, 11.0, None);
        let hint_reserve = if hint.is_empty() {
            0.0
        } else {
            f64::from(hw) + 10.0
        };

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
        let title_max_w = row.x1 - pad - hint_reserve - tx;
        let title = tr.elide_to_width_weighted(
            &title,
            ROW_TITLE_FONT,
            title_max_w as f32,
            TextWeight::Semibold,
        );
        PaintCtx::new(scene, t, tr, theme).text(
            &title,
            &TextStyle::new(ROW_TITLE_FONT).with_weight(TextWeight::Semibold),
            tcolor,
            Point::new(tx, title_top),
            None,
        );

        // Subtitle: first body line, dimmed and elided to the row's width.
        let first_line = s.body.lines().next().unwrap_or("").trim();
        let preview = draw::mono_fit(tr, &clip(first_line, 120), ROW_SUB_FONT, row.x1 - pad - tx);
        PaintCtx::new(scene, t, tr, theme).text(
            &preview,
            &draw::mono_style(ROW_SUB_FONT, TextWeight::Regular),
            c.text_dim,
            Point::new(tx, sub_top),
            None,
        );

        if !hint.is_empty() {
            // Level with the title's measured line.
            let hint_top = title_top + (self.m.title_line - f64::from(hh)) / 2.0;
            PaintCtx::new(scene, t, tr, theme).text(
                &hint,
                &TextStyle::new(11.0).with_weight(TextWeight::Medium),
                c.text_dim,
                Point::new(row.x1 - pad - f64::from(hw), hint_top),
                None,
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
        let y = self.draw_detail_toolbar(
            &mut PaintCtx::new(scene, t, tr, theme),
            area,
            y,
            open,
            &snippet,
        );

        // Code card with syntax-highlighted body.
        let pad = 28.0;
        let card = Rect::new(area.x0 + pad, y, area.x1 - pad, area.y1 - 20.0);
        draw::fill_round(scene, t, card, theme.radii_md(), c.neutral[0]);
        draw::stroke_round(scene, t, card, theme.radii_md(), c.border_standard, 1.0);
        self.body_editor.set_rect(card);
        self.body_editor.draw_embedded(scene, t, tr, theme);
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
        PaintCtx::new(scene, t, tr, theme).text(
            msg,
            &TextStyle::new(15.0),
            c.text_dim,
            Point::new(
                area.x0 + (area.width() - mw) / 2.0,
                area.y0 + area.height() * 0.4,
            ),
            None,
        );
    }

    /// Title, then the language/folder/tag pill row, then the free-text
    /// description. Returns the y cursor the action toolbar starts from.
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
            area.x0 + pad + self.m.toggle
        } else {
            area.x0 + pad
        };
        PaintCtx::new(scene, t, tr, theme).text(
            &snippet.title,
            &TextStyle::new(24.0).with_weight(TextWeight::Bold),
            c.text_primary,
            Point::new(title_x, y),
            None,
        );
        // Stacked lines step by their measured boxes (36 / 26 px at 1×).
        y += self.m.detail_title_line + 6.0;

        y = self.draw_detail_meta_row(
            &mut PaintCtx::new(scene, t, tr, theme),
            area,
            y,
            open,
            snippet,
        );

        // Description.
        if !snippet.description.is_empty() {
            PaintCtx::new(scene, t, tr, theme).text(
                &snippet.description,
                &TextStyle::new(14.0),
                c.text_dim,
                Point::new(area.x0 + pad, y),
                None,
            );
            y += self.m.detail_desc_line + 8.5;
        }
        y
    }

    /// The language pill (cycles the language on click) + folder pill +
    /// one pill per tag. Returns the y cursor beneath the row.
    fn draw_detail_meta_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        y: f64,
        open: SnippetId,
        snippet: &Snippet,
    ) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let pad = 28.0;
        let mut run = draw::PillRun {
            x: area.x0 + pad,
            y,
            height: 22.0 + self.m.pill_extra,
            size: 12.0,
        };
        let lp = run.pill(
            ctx,
            snippet.language.label(),
            c.neutral[0],
            lang_tint(theme, snippet.language),
        );
        // Clicking the language pill cycles to the next language.
        let next = next_language(snippet.language);
        self.record(lp, Action::SetLanguage(open, next));
        run.x = lp.x1 + 8.0;
        if !snippet.folder.is_empty() {
            let fp = run.pill(ctx, &snippet.folder, c.text_dim, c.neutral[2]);
            run.x = fp.x1 + 8.0;
        }
        for tag in &snippet.tags {
            let label = format!("#{tag}");
            let tp = run.pill(ctx, &label, c.accent[7], c.accent[1]);
            self.record(tp, Action::Select(Selection::Tag(tag.clone())));
            run.x = tp.x1 + 8.0;
        }
        y + 34.0 + self.m.pill_extra
    }

    /// Copy / Expand / Favorite / Duplicate / Delete. Returns the y cursor
    /// the code card starts from.
    fn draw_detail_toolbar(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        area: Rect,
        y: f64,
        open: SnippetId,
        snippet: &Snippet,
    ) -> f64 {
        let pad = 28.0;
        let y = y + 4.0;
        let primary_label = if snippet.has_placeholders() {
            "Fill & Copy"
        } else {
            "Copy"
        };
        let star_label = if snippet.favorite {
            "★ Starred"
        } else {
            "☆ Star"
        };
        let mut buttons: Vec<(&str, bool, Action)> =
            vec![(primary_label, true, Action::Copy(open))];
        if snippet.has_placeholders() {
            buttons.push(("Variables…", false, Action::Expand(open)));
        }
        buttons.push((star_label, false, Action::ToggleFavorite(open)));
        buttons.push(("Duplicate", false, Action::Duplicate(open)));
        buttons.push(("Delete", false, Action::Delete(open)));

        // Buttons flow left to right and wrap onto a new row when the next one
        // would cross the pane's right inset — at 200 % a pane that seats five
        // buttons at 1× seats two or three per row. Every action stays
        // reachable; nothing is clipped off the edge. Rows step by the
        // measured button height plus the 1× 8 px gap.
        let btn_h = 32.0 + self.m.button_extra;
        let row_step = btn_h + 8.0;
        let x_start = area.x0 + pad;
        let x_end = area.x1 - pad;
        let mut bx = x_start;
        let mut by = y;
        for (label, primary, action) in buttons {
            let w = Self::toolbar_button_w(ctx.text, label);
            if bx > x_start && bx + w > x_end {
                bx = x_start;
                by += row_step;
            }
            bx = self.toolbar_button(ctx, Point::new(bx, by), label, primary, action);
        }
        by + row_step + 4.0
    }

    /// Width a toolbar button paints at for `label` — the same measurement
    /// [`Self::toolbar_button`] sizes its face from, so wrapping decisions
    /// and paint agree.
    fn toolbar_button_w(tr: &mut TextRenderer, label: &str) -> f64 {
        f64::from(tr.measure(label, BUTTON_FONT, None).0) + 24.0
    }

    /// Draw a toolbar button, record its hit region against `action`, and
    /// return the x-coordinate the next button should start at.
    fn toolbar_button(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        origin: Point,
        label: &str,
        primary: bool,
        action: Action,
    ) -> f64 {
        let theme = ctx.theme;
        let w = Self::toolbar_button_w(ctx.text, label);
        let rect = Rect::new(
            origin.x,
            origin.y,
            origin.x + w,
            origin.y + 32.0 + self.m.button_extra,
        );
        let hovered = rect.contains(self.last_mouse);
        let face = ButtonFace::new(rect, label)
            .radius(theme.radii_md())
            .text_size(BUTTON_FONT)
            .weight(TextWeight::Semibold)
            .hovered(hovered)
            .style(if primary {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Neutral
            });
        Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
        self.record(rect, action);
        rect.x1 + 8.0
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
    PaintCtx::new(scene, t, tr, theme).text(
        label,
        &TextStyle::new(11.0).with_weight(TextWeight::Bold),
        theme.tokens.colors.text_dim,
        Point::new(x, y),
        None,
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
    use crate::resource::SnippetsShell;
    use std::rc::Rc;
    use ui::framework::ResourceSurface;
    use ui::prelude::TestApp;
    use ui::tilo_resource::Invoke;
    use ui::widget::Widget;

    /// The bare view showing the starter library, as the framework presents it.
    fn demo() -> SnippetsApp {
        let mut app = SnippetsApp::new();
        app.present(&Rc::new(crate::seed::starter_library()));
        app
    }

    /// The app on a fresh record holding `snippets`, created through the door.
    fn launched(snippets: Vec<Snippet>) -> TestApp<SnippetsShell> {
        let mut app = TestApp::<SnippetsShell>::new().unwrap();
        for snippet in snippets {
            app.invoke(Invoke::apply(&actions::create_snippet_action(snippet)).unwrap())
                .unwrap();
        }
        app
    }

    fn snippet(title: &str, body: &str) -> Snippet {
        let mut s = Snippet::blank("Ops");
        s.title = title.into();
        s.body = body.into();
        s
    }

    /// Click the control labelled `label` where the window shows it.
    fn click(app: &mut TestApp<SnippetsShell>, label: &str) {
        let harness = app.harness();
        let nodes = harness.a11y_nodes();
        let bounds = nodes
            .iter()
            .find(|(_, node)| node.label() == Some(label))
            .and_then(|(_, node)| node.bounds())
            .unwrap_or_else(|| {
                let labels: Vec<&str> = nodes.iter().filter_map(|(_, n)| n.label()).collect();
                panic!("no control labelled {label:?} among {labels:?}")
            });
        harness.click_at(
            f64::midpoint(bounds.x0, bounds.x1),
            f64::midpoint(bounds.y0, bounds.y1),
        );
        app.settle();
    }

    /// The value of the snippet body's editor, as assistive technology reads it.
    fn body_shown(app: &mut TestApp<SnippetsShell>) -> String {
        app.harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| node.role() == ui::accesskit::Role::MultilineTextInput)
            .and_then(|(_, node)| node.value().map(str::to_owned))
            .unwrap_or_default()
    }

    /// The view shows what it is presented: empty until the library arrives,
    /// and searching narrows it without writing anything.
    #[test]
    fn presented_library_fills_the_list_and_search_stays_local() {
        let empty = SnippetsApp::new();
        assert_eq!(empty.visible_count_for_test(), 0);
        assert!(empty.open_id_for_test().is_none());

        let mut app = demo();
        let before = app.visible_count_for_test();
        assert!(before >= 10);
        assert!(app.open_id_for_test().is_some());
        app.set_search_for_test("language:rust");
        let after = app.visible_count_for_test();
        assert!((1..before).contains(&after));

        let mut app = demo();
        let mut doc = AutoCommit::new();
        let heads_before = doc.get_heads();
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mut focus_target = None;
        app.search_input.set_focus(true);
        let mut handled = false;
        assert!(app.route_search_event(
            &WindowEvent::Ime(winit::event::Ime::Commit("language:rust".into())),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        ));
        assert!(app.visible_count_for_test() < before);
        assert_eq!(app.search_input.value(), "language:rust");
        assert_eq!(doc.get_heads(), heads_before, "search stays per-peer state");

        let last = *Language::ALL.last().unwrap();
        assert_eq!(next_language(last), Language::ALL[0]);
    }

    /// The toolbar's Star, Copy, New and Delete submit the library's
    /// actions: each is one undo step, and the view follows the stored
    /// library.
    #[test]
    fn affordances_submit_library_actions_as_one_undo_step_each() {
        let mut app = launched(vec![snippet("Plain", "echo hi"), snippet("Templated", "")]);
        let id = |app: &TestApp<SnippetsShell>, title: &str| {
            app.state().into_iter().find(|s| s.title == title).unwrap()
        };
        click(&mut app, "Open Plain");
        click(&mut app, "Star snippet");
        assert!(id(&app, "Plain").favorite);
        assert!(app.undo(), "the star is one undo step");
        assert!(!id(&app, "Plain").favorite);

        click(&mut app, "Copy snippet");
        assert_eq!(id(&app, "Plain").uses, 1, "copying records a use");

        click(&mut app, "New snippet");
        assert_eq!(app.state().len(), 3);
        // The new snippet is the one open, so the toolbar deletes it.
        click(&mut app, "Delete snippet");
        let titles: Vec<String> = app.state().into_iter().map(|s| s.title).collect();
        assert_eq!(titles, ["Plain", "Templated"]);
        assert!(app.undo(), "the delete is one undo step");
        assert_eq!(app.state().len(), 3);
    }

    /// Typing in the body splices the stored text through the body's child
    /// resource; the editor follows a collaborator's edit and an undo.
    #[test]
    fn body_edits_are_text_actions_and_the_editor_follows_the_document() {
        let mut app = launched(vec![snippet("Only", "")]);
        let id = app.state()[0].id;
        // Into the code card, below the detail pane's toolbar.
        app.harness().click_at(830.0, 400.0);
        app.harness().type_text("Grün");
        app.settle();
        assert_eq!(app.state()[0].body, "Grün");
        assert!(app.undo(), "the typed word is one undo step");
        assert_eq!(app.state()[0].body, "");
        assert_eq!(body_shown(&mut app), "");

        app.merge_peer_edit(|doc| {
            tilo_actions::Action::apply(&actions::set_body_action(id, "remote 🌍\n"), doc).unwrap();
        });
        app.settle();
        assert_eq!(app.state()[0].body, "remote 🌍\n");
        assert_eq!(body_shown(&mut app), "remote 🌍\n");
    }

    /// The variable dialog is addressable and peer-local, and holds the body
    /// while it is open.
    #[test]
    fn variable_dialog_is_semantic_and_local() {
        let mut app = SnippetsApp::new();
        let templated = snippet("Greet", "Hello ${name}");
        app.present(&Rc::new(vec![templated.clone()]));
        let mut doc = AutoCommit::new();
        let heads = doc.get_heads();
        app.copy(templated.id);
        assert!(
            app.expand_open_for_test(),
            "a templated copy fills its variables first"
        );
        assert!(app.perform_semantic_action(
            "snippets.expand.field.name",
            &ui::a11y::Action::SetValue("Grün".into()),
            &mut doc,
            &automerge::ROOT
        ));
        assert_eq!(
            app.expand.as_ref().unwrap().preview_for_test(),
            "Hello Grün"
        );
        assert!(!app.perform_semantic_action(
            "snippets.body",
            &ui::a11y::Action::SetValue("hidden edit".into()),
            &mut doc,
            &automerge::ROOT
        ));
        assert!(app.perform_semantic_action(
            "snippets.expand.cancel",
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT
        ));
        assert!(!app.expand_open_for_test());
        assert_eq!(doc.get_heads(), heads, "the dialog writes nothing");
    }

    /// Focusing the New button does not author a snippet; activating it does.
    #[test]
    fn focusing_new_snippet_does_not_activate_it() {
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        app.record(Rect::new(0.0, 0.0, 100.0, 40.0), Action::NewSnippet);
        let target = Action::NewSnippet.semantic_id();
        assert!(Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::Focus,
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(
            app.pending_open.is_none(),
            "focusing a button authors nothing"
        );
        assert!(Widget::perform_semantic_action(
            &mut app,
            &target,
            &ui::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
        assert!(
            app.pending_open.is_some(),
            "activating it creates a snippet"
        );
    }

    // ---- responsive layout ---------------------------------------------

    /// Sidebar docking, overlay collapse, the hamburger and the collapsed toggle/search row.
    #[test]
    fn sidebar_docks_wide_and_overlays_narrow() {
        // Case `wide_keeps_sidebar_docked_and_open`.
        {
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

        // Case `narrow_collapses_sidebar_to_overlay_closed_by_default`.
        {
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

        // Case `hamburger_toggles_the_overlay_sidebar`.
        {
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

        // Case `collapsed_sidebar_toggle_and_search_field_share_a_row_without_overlap`.
        // With the sidebar collapsed the hamburger floats over the list's
        // top-left corner. The search field must start past it — both the
        // painted field/hit region and the shared input's own layout — and the
        // two must share one centre line, instead of the toggle covering the
        // start of the placeholder.
        {
            for width in [700.0, 480.0] {
                let mut app = demo();
                app.layout_at_width_for_test(width).unwrap();
                draw_once(&mut app);
                let toggle = hit_rect(&app, &Action::ToggleSidebar).expect("hamburger recorded");
                let search = hit_rect(&app, &Action::FocusSearch).expect("search recorded");
                assert!(
                    toggle.x1 <= search.x0,
                    "w={width}: toggle {toggle:?} must end before the search field {search:?}"
                );
                let input = Widget::get_rect(&app.search_input);
                assert!(
                    input.x0 >= toggle.x1,
                    "w={width}: the shared input's layout {input:?} starts past the toggle {toggle:?}"
                );
                assert!(
                    (toggle.center().y - search.center().y).abs() < 0.5,
                    "w={width}: toggle {toggle:?} and search {search:?} share a centre line"
                );
            }
            // Docked open: no toggle, and the field keeps its ordinary inset.
            let mut app = demo();
            app.layout_at_width_for_test(1280.0).unwrap();
            draw_once(&mut app);
            assert!(hit_rect(&app, &Action::ToggleSidebar).is_none());
            let search = hit_rect(&app, &Action::FocusSearch).expect("search recorded");
            assert!(
                (search.x0 - (SIDEBAR_W + 12.0)).abs() < 0.5,
                "search {search:?}"
            );
        }
    }

    /// Paint one frame the way the host does, so the recorded hit regions
    /// (and the shared search input's layout) reflect the production draw.
    fn draw_once(app: &mut SnippetsApp) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        Widget::draw(
            app,
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );
    }

    fn hit_rect(app: &SnippetsApp, want: &Action) -> Option<Rect> {
        app.hit
            .iter()
            .find(|(_, action)| action == want)
            .map(|(rect, _)| *rect)
    }

    /// Very narrow single-pane push navigation and restoring the docked default.
    #[test]
    fn very_narrow_single_pane_pushes_and_restores() {
        // Case `very_narrow_uses_single_pane_push`.
        {
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

        // Case `opening_a_snippet_pushes_the_detail_when_very_narrow`.
        {
            let mut app = demo();
            app.layout_at_width_for_test(480.0).unwrap();
            assert!(!app.push_showing_detail_for_test());
            app.open_first_for_test();
            assert!(
                app.push_showing_detail_for_test(),
                "selecting a snippet pushes the detail pane in"
            );
        }

        // Case `opening_a_snippet_does_not_push_when_wide`.
        {
            let mut app = demo();
            app.layout_at_width_for_test(1280.0).unwrap();
            app.open_first_for_test();
            assert!(
                !app.push_showing_detail_for_test(),
                "wide layout shows both panes; opening must not trigger a push"
            );
        }

        // Case `resizing_wide_again_restores_the_docked_default`.
        {
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
    }
}
