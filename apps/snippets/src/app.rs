//! Root widget for the snippet manager.
//!
//! A snippet manager has one job: keep reusable fragments and get one into
//! another app, filled in, fast. The layout answers that directly with two
//! panes:
//!
//! ```text
//! ┌──────────────────────┬──────────────────────────────────────┐
//! │  LIBRARY             │  SNIPPET                              │
//! │  🔍 search       +   │  Title (editable)          Copy       │
//! │  All Favorites Recent│  language · #tags · ★                 │
//! │  ─ selectable rows ─ │  code editor (syntax + ${vars})       │
//! │  badge · title · use │  ── Variables ──  ${name}[ ] [Copy]   │
//! └──────────────────────┴──────────────────────────────────────┘
//! ```
//!
//! Every text surface is a shared `ui` widget (a [`TextInput`] for search /
//! title / tags / description / each variable, a [`CodeEditor`] for the body,
//! an [`Autocomplete`] for the language) hosted in this monolithic immediate
//! -mode root — no hand-rolled fields. Every library mutation is a
//! [`SnippetsAction`] dispatched through [`ui::ActionHost`], so a toolbar
//! affordance and the Ctrl+K palette share one undoable, replicated path. The
//! library is mirrored into the shared Automerge document by [`crate::doc`] so
//! two instances converge; search, selection and the per-variable fill values
//! are per-peer view state and stay local.

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

use ui::icons::Icon;
use ui::widget::autocomplete::{Autocomplete, AutocompleteOutcome, Suggestion};
use ui::widget::code_editor::CodeEditor;
use ui::widget::{
    Button, ButtonFace, ButtonStyle, EmptyState, FlapState, PushPane, SplitLayout, TextInput,
    Widget,
};

use crate::actions::{self, SnippetsAction, SnippetsResourceCtx};
use crate::doc::SnippetsDoc;
use crate::draw;
use crate::highlight::SnippetHighlighter;
use crate::model::{self, Language, Library, Selection, SmartFolder, Snippet, SnippetId};
use crate::seed;

const LIST_W: f64 = 344.0;
const ROW_H: f64 = 60.0;
const PAD: f64 = 20.0;

/// Below this allocated width the list and the detail can't share the row, so
/// the flow switches to single-pane push: the list OR the detail fills the
/// pane, and opening a snippet slides the detail in with a Back affordance.
const PUSH_BREAKPOINT: f64 = 720.0;

/// Top-left Back toggle hit size (logical px), single-pane push only.
const TOGGLE_SIZE: f64 = 34.0;

/// A transient bottom-right confirmation. Auto-dismisses on idle; for a
/// headless shot it simply persists.
#[derive(Clone)]
struct Toast {
    message: String,
    ttl: u32,
}

/// Which hosted text surface currently owns keyboard input. Only one at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    None,
    Search,
    Title,
    Language,
    Tags,
    Description,
    Body,
    /// The i-th variable-fill field in the open snippet's fill panel.
    Fill(usize),
}

impl Focus {
    /// True for the detail-editor fields (as opposed to search / nothing). Used
    /// to keep the open snippet pinned while its own fields are being edited,
    /// even if a live edit filters it out of the current search.
    fn is_detail(self) -> bool {
        !matches!(self, Focus::None | Focus::Search)
    }
}

/// A clickable region recorded during draw and resolved on mouse-down.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Scope(SmartFolder),
    OpenSnippet(SnippetId),
    NewSnippet,
    Copy,
    ToggleFavorite,
    Duplicate,
    Delete,
    ClearSearch,
    PushBack,
}

pub struct SnippetsApp {
    library: Library,
    selection: Selection,
    open: Option<SnippetId>,
    visible: Vec<SnippetId>,
    seeded: bool,

    // ---- hosted shared widgets -----------------------------------------
    search: TextInput,
    title: TextInput,
    language: Autocomplete,
    tags: TextInput,
    description: TextInput,
    body: CodeEditor,
    /// One (variable name, value field) per `${placeholder}` in the open
    /// snippet's body. Values are ephemeral per-peer fill state, never stored.
    fills: Vec<(String, TextInput)>,
    focus: Focus,
    /// The snippet the detail fields were last seeded from, so a selection
    /// change re-seeds them and an in-place edit does not clobber the caret.
    seeded_detail: Option<SnippetId>,

    toast: Option<Toast>,

    // ---- layout / hit caches (recomputed each draw) --------------------
    rect: Rect,
    last_mouse: Point,
    hit: Vec<(Rect, Action)>,
    list_scroll: f64,
    // Detail field rects (last frame) for click routing.
    title_rect: Rect,
    language_rect: Rect,
    tags_rect: Rect,
    description_rect: Rect,
    body_rect: Rect,
    fill_rects: Vec<Rect>,
    search_rect: Rect,

    // ---- responsive single-pane push -----------------------------------
    push: FlapState,
    push_pane: PushPane,
    settled_layout: bool,

    actions: ui::ActionHost,
}

impl SnippetsApp {
    /// Build the live app on an **empty** library. A fresh install has no
    /// snippets — the user saves their own, or a peer's shared library is
    /// adopted on the first `sync`. The starter set is demo/`--shots` only.
    pub fn new() -> Self {
        Self::with_library(Library::new())
    }

    /// Like [`Self::new`] but pre-seeded with the starter library and an opened
    /// specimen — used by `--shots` and any "try it" walkthrough. Never the
    /// live first-run path.
    pub fn new_demo() -> Self {
        let mut library = Library::new();
        for s in seed::starter_library() {
            library.insert(s);
        }
        let mut app = Self::with_library(library);
        // Open a substantial, templated snippet so the detail shows the editor,
        // highlighter and the variable-fill affordance doing real work.
        app.open = app
            .visible
            .iter()
            .copied()
            .max_by_key(|id| {
                let s = app.library.get(*id);
                let lines = s.map(|s| s.body.lines().count()).unwrap_or(0);
                let templated = s.map(|s| s.has_placeholders()).unwrap_or(false) as usize;
                (templated, lines)
            })
            .or_else(|| app.visible.first().copied());
        app
    }

    fn with_library(library: Library) -> Self {
        let language = Autocomplete::new()
            .with_id("snippet.language")
            .with_placeholder("Language")
            .with_suggester(|q| {
                let q = q.to_lowercase();
                Language::ALL
                    .into_iter()
                    .filter(|l| q.is_empty() || l.label().to_lowercase().contains(&q))
                    .map(|l| Suggestion::new(l.label()).with_icon(Icon::Code))
                    .collect()
            });
        let body = CodeEditor::local()
            .with_id("snippet.body")
            .with_placeholder("Write the snippet — use ${name} for a variable to fill on copy.")
            .with_font_size(13.5);
        let mut app = Self {
            library,
            selection: Selection::default(),
            open: None,
            visible: Vec::new(),
            seeded: false,
            search: TextInput::local().with_id("snippet.search").with_placeholder("Search snippets"),
            title: TextInput::local()
                .with_id("snippet.title")
                .with_plain_chrome()
                .with_font_size(22.0)
                .with_placeholder("Untitled snippet"),
            language,
            tags: TextInput::local().with_id("snippet.tags").with_placeholder("tags, comma separated"),
            description: TextInput::local()
                .with_id("snippet.description")
                .with_placeholder("Description (optional)"),
            body,
            fills: Vec::new(),
            focus: Focus::None,
            seeded_detail: None,
            toast: None,
            rect: Rect::ZERO,
            last_mouse: Point::ZERO,
            hit: Vec::new(),
            list_scroll: 0.0,
            title_rect: Rect::ZERO,
            language_rect: Rect::ZERO,
            tags_rect: Rect::ZERO,
            description_rect: Rect::ZERO,
            body_rect: Rect::ZERO,
            fill_rects: Vec::new(),
            search_rect: Rect::ZERO,
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

    fn search_text(&self) -> String {
        self.search.value().to_string()
    }

    fn recompute_visible(&mut self) {
        let search = self.search_text();
        self.visible = self.library.query(&self.selection, &search);
        // Keep the open snippet valid. While a detail field is being edited we
        // pin the open snippet even if the live edit filtered it out, so typing
        // never yanks the editor away mid-word.
        match self.open {
            Some(open) if self.visible.contains(&open) => {}
            Some(open) if self.focus.is_detail() => {
                // Pinned: leave `open` as-is even though it's filtered out.
                let _ = open;
            }
            _ => self.open = self.visible.first().copied(),
        }
    }

    fn scope(&mut self, smart: SmartFolder) {
        self.selection = Selection::Smart(smart);
        self.list_scroll = 0.0;
        self.focus = Focus::None;
        self.recompute_visible();
    }

    fn open_snippet(&mut self, id: SnippetId) {
        self.open = Some(id);
        self.focus = Focus::None;
        if self.is_push_layout() {
            self.push_pane = PushPane::Detail;
            self.push.open();
        }
    }

    /// Dispatch one [`SnippetsAction`] — the single mutation path shared with
    /// the Ctrl+K palette. The action runs through the qOS executor (so it
    /// replays byte-identically on peers), the shared document being
    /// authoritative; the in-memory library is re-projected so the panes
    /// reflect it this frame.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: SnippetsAction) {
        self.actions.dispatch(doc, actions::SNIPPETS_ACTION_TYPE, &action);
        self.refresh_from_doc(doc);
    }

    fn refresh_from_doc(&mut self, doc: &AutoCommit) {
        self.library.snippets = SnippetsDoc::list(doc);
    }

    pub(crate) fn snippets_resource_ctx(&self) -> SnippetsResourceCtx {
        SnippetsResourceCtx { current_folder: String::new() }
    }

    fn new_snippet(&mut self, doc: &mut AutoCommit) {
        let s = Snippet::blank("");
        let id = s.id;
        self.perform_action(doc, actions::create_snippet_action(s));
        self.recompute_visible();
        self.open = Some(id);
        self.reconcile_detail();
        // Drop the user straight into the title so a new snippet is authorable
        // immediately.
        self.set_focus(Focus::Title);
        self.toast("New snippet — start typing");
    }

    /// Copy the open snippet, substituting the inline variable fields first so
    /// raw `${...}` never lands on the clipboard. The one dominant action.
    fn copy_open(&mut self, doc: &mut AutoCommit) {
        self.reconcile_detail();
        let Some(id) = self.open else { return };
        let text = self.filled_text();
        ui::clipboard_copy(&text);
        self.record_use(id, doc);
        let templated = !self.current_placeholders().is_empty();
        self.toast(if templated { "Filled and copied" } else { "Copied to clipboard" });
    }

    /// The text a copy would place on the clipboard: the live body with the
    /// current variable-field values substituted in (unfilled slots stay
    /// visible so a partial copy is honest).
    fn filled_text(&self) -> String {
        let body = self.body.value().to_string();
        let values: HashMap<String, String> = self
            .fills
            .iter()
            .map(|(name, f)| (name.clone(), f.value().to_string()))
            .collect();
        model::expand(&body, &values)
    }

    fn current_placeholders(&self) -> Vec<String> {
        model::placeholders_in(self.body.value())
    }

    fn record_use(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.perform_action(doc, actions::record_use_action(id));
    }

    fn toggle_favorite(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.open else { return };
        let cur = self.library.get(id).map(|s| s.favorite).unwrap_or(false);
        self.perform_action(doc, actions::set_favorite_action(id, !cur));
        self.recompute_visible();
    }

    fn duplicate(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.open else { return };
        if let Some(src) = self.library.get(id).cloned() {
            let mut copy = src.clone();
            copy.id = uuid::Uuid::new_v4();
            copy.title = format!("{} (copy)", src.title);
            copy.favorite = false;
            copy.uses = 0;
            let new_id = copy.id;
            self.perform_action(doc, actions::create_snippet_action(copy));
            self.recompute_visible();
            self.open = Some(new_id);
            self.reconcile_detail();
            self.toast("Duplicated");
        }
    }

    fn delete(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.open else { return };
        self.perform_action(doc, actions::delete_snippet_action(id));
        self.open = None;
        self.focus = Focus::None;
        self.recompute_visible();
        if self.is_push_layout() {
            self.push_pane = PushPane::Master;
            self.push.close();
        }
        self.toast("Deleted");
    }

    fn toast(&mut self, message: &str) {
        self.toast = Some(Toast { message: message.to_string(), ttl: 90 });
    }

    // ---- detail-field lifecycle ----------------------------------------

    fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
        self.search.set_focus(focus == Focus::Search);
        self.title.set_focus(focus == Focus::Title);
        self.tags.set_focus(focus == Focus::Tags);
        self.description.set_focus(focus == Focus::Description);
        self.body.set_focused(focus == Focus::Body);
        for (i, (_, f)) in self.fills.iter_mut().enumerate() {
            f.set_focus(focus == Focus::Fill(i));
        }
        if focus != Focus::Language && self.language.is_open() {
            self.language.close();
        }
    }

    /// Re-seed the detail editor from the open snippet when the selection
    /// changes. An in-place edit (open unchanged) never re-seeds, so the caret
    /// is preserved while typing.
    fn reconcile_detail(&mut self) {
        if self.open == self.seeded_detail {
            self.reconcile_fills();
            return;
        }
        match self.open.and_then(|id| self.library.get(id).cloned()) {
            Some(s) => self.seed_detail(&s),
            None => {
                self.title.set_text("");
                self.tags.set_text("");
                self.description.set_text("");
                self.body.set_text("");
                self.language.set_text("");
                self.fills.clear();
                self.fill_rects.clear();
            }
        }
        self.seeded_detail = self.open;
    }

    fn seed_detail(&mut self, s: &Snippet) {
        self.title.set_text(&s.title);
        self.language.set_text(s.language.label());
        self.tags.set_text(&s.tags.join(", "));
        self.description.set_text(&s.description);
        self.body.set_text(&s.body);
        self.body.set_highlighter(Some(SnippetHighlighter::boxed(s.language)));
        self.rebuild_fills(&model::placeholders_in(&s.body));
    }

    /// Rebuild the variable-fill fields when the open body's `${...}` set
    /// changes, preserving any values the user already typed for names that
    /// persist. Only runs while the body is not being actively edited, so
    /// adding a variable mid-word doesn't churn focus.
    fn reconcile_fills(&mut self) {
        if self.focus == Focus::Body {
            return;
        }
        let wanted = self.current_placeholders();
        let have: Vec<String> = self.fills.iter().map(|(n, _)| n.clone()).collect();
        if wanted == have {
            return;
        }
        self.rebuild_fills(&wanted);
    }

    fn rebuild_fills(&mut self, names: &[String]) {
        let prior: HashMap<String, String> = self
            .fills
            .drain(..)
            .map(|(n, f)| (n, f.value().to_string()))
            .collect();
        self.fills = names
            .iter()
            .map(|name| {
                let mut field = TextInput::local().with_placeholder("value…");
                if let Some(v) = prior.get(name) {
                    field.set_text(v);
                }
                (name.clone(), field)
            })
            .collect();
        self.fill_rects = vec![Rect::ZERO; self.fills.len()];
        // Any prior focus into a fill field is now stale.
        if matches!(self.focus, Focus::Fill(_)) {
            self.focus = Focus::None;
        }
    }

    /// After routing an event to the focused field, fold any resulting edit
    /// back into the library through the shared action path (or, for search,
    /// re-filter). This is the single place a hosted field's user edit becomes
    /// a [`SnippetsAction`].
    fn after_field_edit(&mut self, doc: &mut AutoCommit) {
        match self.focus {
            Focus::Search => {
                if self.search.take_dirty().is_some() {
                    self.list_scroll = 0.0;
                    self.recompute_visible();
                }
            }
            Focus::Title => {
                if self.title.take_dirty().is_some() {
                    if let Some(id) = self.open {
                        let title = self.title.value().to_string();
                        self.perform_action(doc, actions::rename_action(id, &title));
                        self.recompute_visible();
                    }
                }
            }
            Focus::Tags => {
                if self.tags.take_dirty().is_some() {
                    if let Some(id) = self.open {
                        let tags = actions::parse_tags(self.tags.value());
                        self.perform_action(doc, actions::set_tags_action(id, &tags));
                        self.recompute_visible();
                    }
                }
            }
            Focus::Description => {
                if self.description.take_dirty().is_some() {
                    if let Some(id) = self.open {
                        let d = self.description.value().to_string();
                        self.perform_action(doc, actions::set_description_action(id, &d));
                    }
                }
            }
            Focus::Body => {
                if self.body.take_dirty().is_some() {
                    if let Some(id) = self.open {
                        let body = self.body.value().to_string();
                        self.perform_action(doc, actions::set_body_action(id, &body));
                    }
                }
            }
            Focus::Language => match self.language.take_outcome() {
                AutocompleteOutcome::Accepted(label) => {
                    if let Some(id) = self.open {
                        let lang = language_from_label(&label);
                        self.perform_action(doc, actions::set_language_action(id, lang));
                        self.body.set_highlighter(Some(SnippetHighlighter::boxed(lang)));
                        self.recompute_visible();
                    }
                    self.set_focus(Focus::None);
                }
                AutocompleteOutcome::Cancelled | AutocompleteOutcome::None => {}
            },
            Focus::Fill(_) => {
                // Ephemeral fill values: read on copy, never stored.
            }
            Focus::None => {}
        }
    }

    fn apply(&mut self, action: Action, doc: &mut AutoCommit) {
        match action {
            Action::Scope(sf) => self.scope(sf),
            Action::OpenSnippet(id) => self.open_snippet(id),
            Action::NewSnippet => self.new_snippet(doc),
            Action::Copy => self.copy_open(doc),
            Action::ToggleFavorite => self.toggle_favorite(doc),
            Action::Duplicate => self.duplicate(doc),
            Action::Delete => self.delete(doc),
            Action::ClearSearch => {
                self.search.set_text("");
                self.list_scroll = 0.0;
                self.recompute_visible();
            }
            Action::PushBack => {
                self.push_pane = PushPane::Master;
                self.push.close();
                self.focus = Focus::None;
            }
        }
    }

    fn action_at(&self, p: Point) -> Option<Action> {
        self.hit.iter().rev().find(|(r, _)| r.contains(p)).map(|(_, a)| a.clone())
    }

    fn record(&mut self, rect: Rect, action: Action) {
        self.hit.push((rect, action));
    }

    // ---- responsive geometry -------------------------------------------

    fn is_push_layout(&self) -> bool {
        self.rect.width() < PUSH_BREAKPOINT
    }

    fn sync_library(&mut self, doc: &mut AutoCommit) {
        let remote = SnippetsDoc::list(doc);
        if !self.seeded {
            if remote.is_empty() {
                for s in self.library.snippets.clone() {
                    SnippetsDoc::upsert(doc, &s);
                }
            } else {
                self.library.snippets = remote;
                self.recompute_visible();
            }
            self.seeded = true;
            return;
        }
        if self.library.snippets != remote {
            self.library.snippets = remote;
            self.recompute_visible();
        }
    }

    // ---- shot scenarios -------------------------------------------------

    /// Snap the responsive/interaction state to a deterministic configuration
    /// for the single-frame `--shots` renderer.
    ///
    /// * `"detail"` — single-pane push showing the detail with a Back chip.
    /// * `"fill"` — open a templated snippet with its variables filled, so the
    ///   capture shows the fill flow mid-use.
    /// * anything else — the default frame.
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        match scenario {
            "detail" => {
                self.push_pane = PushPane::Detail;
                self.push.snap(true);
                self.settled_layout = true;
            }
            "fill" => {
                // Open the templated snippet with the most variables and pre-
                // fill them so the capture shows a real, in-progress fill.
                if let Some(id) = self
                    .visible
                    .iter()
                    .copied()
                    .max_by_key(|id| self.library.get(*id).map(|s| s.placeholders().len()).unwrap_or(0))
                {
                    self.open = Some(id);
                    self.reconcile_detail();
                    let samples = ["prod", "web-1", "api", "main", "42", "app"];
                    for (i, (_, f)) in self.fills.iter_mut().enumerate() {
                        f.set_text(samples[i % samples.len()]);
                    }
                }
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
        self.search.set_text(q);
        self.recompute_visible();
    }

    #[doc(hidden)]
    pub fn library_len_for_test(&self) -> usize {
        self.library.snippets.len()
    }

    #[doc(hidden)]
    pub fn snippet_uses_for_test(&self, id: SnippetId) -> u32 {
        self.library.get(id).map(|s| s.uses).unwrap_or(0)
    }

    #[doc(hidden)]
    pub fn last_dispatched_for_test(&self) -> Option<qos_actions::EncodedAction> {
        self.actions.last_dispatched().cloned()
    }

    /// Open `id` then copy it exactly as the Copy affordance resolves.
    #[doc(hidden)]
    pub fn copy_for_test(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.open = Some(id);
        self.reconcile_detail();
        self.copy_open(doc);
    }

    /// The number of variable-fill fields the open snippet exposes.
    #[doc(hidden)]
    pub fn fill_field_count_for_test(&mut self) -> usize {
        self.reconcile_detail();
        self.fills.len()
    }

    /// Type `value` into the fill field for `${name}` (test-only).
    #[doc(hidden)]
    pub fn set_fill_for_test(&mut self, name: &str, value: &str) {
        self.reconcile_detail();
        if let Some((_, f)) = self.fills.iter_mut().find(|(n, _)| n == name) {
            f.set_text(value);
        }
    }

    /// The text a copy of the open snippet would place on the clipboard.
    #[doc(hidden)]
    pub fn filled_text_for_test(&mut self) -> String {
        self.reconcile_detail();
        self.filled_text()
    }

    /// Type `text` into the open snippet's body editor and fold the edit back
    /// through the action path, exactly as a keystroke would (test-only).
    #[doc(hidden)]
    pub fn edit_body_for_test(&mut self, text: &str, doc: &mut AutoCommit) {
        self.reconcile_detail();
        self.body.set_text(text);
        if let Some(id) = self.open {
            self.perform_action(doc, actions::set_body_action(id, text));
        }
    }

    #[doc(hidden)]
    pub fn new_snippet_for_test(&mut self, doc: &mut AutoCommit) {
        self.new_snippet(doc);
    }

    #[doc(hidden)]
    pub fn delete_for_test(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.open = Some(id);
        self.apply(Action::Delete, doc);
    }

    #[doc(hidden)]
    pub fn toggle_favorite_for_test(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.open = Some(id);
        self.apply(Action::ToggleFavorite, doc);
    }

    #[doc(hidden)]
    pub fn set_language_for_test(&mut self, id: SnippetId, lang: Language, doc: &mut AutoCommit) {
        self.open = Some(id);
        self.perform_action(doc, actions::set_language_action(id, lang));
        self.recompute_visible();
    }

    #[doc(hidden)]
    pub fn layout_at_width_for_test(&mut self, width: f64) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = self.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(width as f32),
                    height: taffy::AvailableSpace::Definite(800.0),
                },
            )
            .unwrap();
        self.apply_layout(&taffy, node, Point::ZERO);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        for _ in 0..240 {
            if !Widget::update(self, &mut doc, &root) {
                break;
            }
        }
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

impl Widget for SnippetsApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        self.sync_library(state);
    }

    fn needs_idle_sync(&self) -> bool {
        !self.seeded
    }

    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.actions.take_committed()
    }

    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_provider::SnippetSearchProvider::new(&self.library))]
    }

    fn activate_search_result(&mut self, action_id: &str, _doc: &mut AutoCommit, _path: &ObjId) {
        if let Some(id) = crate::search_provider::SnippetSearchProvider::parse_action_id(action_id) {
            if self.library.get(id).is_some() {
                self.open_snippet(id);
            }
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        if !self.settled_layout {
            self.settled_layout = true;
        }
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
        self.reconcile_detail();
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, self.rect, c.surfaces.base);

        if self.is_push_layout() {
            let band = self.rect;
            let w = band.width();
            let p = self.push.slide();
            let list = Rect::new(band.x0 - w * p, band.y0, band.x1 - w * p, band.y1);
            let detail = Rect::new(band.x0 + w * (1.0 - p), band.y0, band.x1 + w * (1.0 - p), band.y1);
            if p < 0.999 {
                self.draw_list(scene, t, tr, theme, list);
            }
            if p > 0.001 {
                self.draw_detail(scene, t, tr, theme, detail);
            }
        } else {
            let band = self.rect;
            let list_w = LIST_W.min((band.width() - 320.0).max(260.0));
            let list = Rect::new(band.x0, band.y0, band.x0 + list_w, band.y1);
            let detail = Rect::new(list.x1, band.y0, band.x1, band.y1);
            self.draw_list(scene, t, tr, theme, list);
            self.draw_detail(scene, t, tr, theme, detail);
            draw::fill_rect(
                scene,
                t,
                Rect::new(list.x1 - 0.5, band.y0, list.x1 + 0.5, band.y1),
                c.border_standard,
            );
        }

        // The language picker's suggestion list floats above everything else.
        if self.language.is_open() {
            self.language.draw_overlay(scene, t, tr, theme, &PresenceCtx::empty());
        }

        if let Some(toast) = &self.toast {
            draw_toast(scene, t, tr, theme, self.rect, &toast.message);
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
        _focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            // Feed hover to every hosted field.
            self.forward_to_all_fields(event, scale_factor, modifiers, state, path, node_id, taffy);
            return;
        }

        // The open language dropdown captures pointer input first.
        if self.language.is_open() {
            if let WindowEvent::MouseInput { state: ElementState::Pressed, .. } = event {
                let mut throwaway = None;
                self.language.handle_event(
                    event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy,
                );
                self.after_field_edit(state);
                *handled = true;
                return;
            }
        }

        match event {
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let p = self.last_mouse;
                // A click inside a hosted field focuses it and positions the caret.
                if let Some(f) = self.field_at(p) {
                    self.set_focus(f);
                    self.route_press_to_field(f, event, scale_factor, modifiers, state, path, handled, node_id, taffy);
                    self.after_field_edit(state);
                    *handled = true;
                    return;
                }
                if let Some(action) = self.action_at(p) {
                    self.set_focus(Focus::None);
                    self.apply(action, state);
                    *handled = true;
                } else {
                    self.set_focus(Focus::None);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y as f64 * ROW_H,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                if self.body_rect.contains(self.last_mouse) {
                    self.body.scroll_by(-dy);
                } else {
                    self.list_scroll = (self.list_scroll - dy).max(0.0);
                }
                *handled = true;
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } => {
                let mods = modifiers.state();
                if mods.control_key() || mods.super_key() {
                    if let Key::Character(ch) = logical_key {
                        match ch.to_lowercase().as_str() {
                            "f" => {
                                self.set_focus(Focus::Search);
                                *handled = true;
                                return;
                            }
                            "n" => {
                                self.new_snippet(state);
                                *handled = true;
                                return;
                            }
                            "c" if !self.focus.is_detail() => {
                                self.copy_open(state);
                                *handled = true;
                                return;
                            }
                            _ => {}
                        }
                    }
                }

                // Escape unwinds: a push detail returns to the list; an open
                // language menu closes; a non-empty search clears; else blur.
                if let Key::Named(NamedKey::Escape) = logical_key {
                    if self.is_push_layout() && self.push.reveal() > 0.5 {
                        self.apply(Action::PushBack, state);
                        *handled = true;
                        return;
                    }
                    if self.language.is_open() {
                        self.language.close();
                        *handled = true;
                        return;
                    }
                    if !self.search_text().is_empty() && self.focus == Focus::Search {
                        self.search.set_text("");
                        self.recompute_visible();
                        *handled = true;
                        return;
                    }
                    self.set_focus(Focus::None);
                    return;
                }

                // Tab within the detail moves through the editor fields.
                if let Key::Named(NamedKey::Tab) = logical_key {
                    if self.focus.is_detail() {
                        self.focus_next_detail(mods.shift_key());
                        *handled = true;
                        return;
                    }
                }

                if self.focus != Focus::None {
                    self.route_key_to_field(event, scale_factor, modifiers, state, path, handled, node_id, taffy);
                    self.after_field_edit(state);
                }
            }
            _ => {}
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.toast.is_some() || self.push.is_animating(SplitLayout::Docked)
    }

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        let dt = 1.0 / 60.0;
        self.push.tick(SplitLayout::Docked, dt);
        if let Some(toast) = &mut self.toast {
            if toast.ttl == 0 {
                self.toast = None;
            } else {
                toast.ttl -= 1;
            }
        }
        false
    }

    fn name(&self) -> &str {
        "SnippetsApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
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

impl SnippetsApp {
    // ---- event routing helpers -----------------------------------------

    /// Which hosted field (if any) the point falls in, respecting the current
    /// layout (detail fields are only hittable when the detail is on screen).
    fn field_at(&self, p: Point) -> Option<Focus> {
        if self.search_rect.contains(p) {
            return Some(Focus::Search);
        }
        if !self.detail_on_screen() {
            return None;
        }
        if self.title_rect.contains(p) {
            return Some(Focus::Title);
        }
        if self.language_rect.contains(p) {
            return Some(Focus::Language);
        }
        if self.tags_rect.contains(p) {
            return Some(Focus::Tags);
        }
        if self.description_rect.contains(p) {
            return Some(Focus::Description);
        }
        if self.body_rect.contains(p) {
            return Some(Focus::Body);
        }
        for (i, r) in self.fill_rects.iter().enumerate() {
            if r.contains(p) {
                return Some(Focus::Fill(i));
            }
        }
        None
    }

    fn detail_on_screen(&self) -> bool {
        !self.is_push_layout() || self.push_pane == PushPane::Detail
    }

    #[allow(clippy::too_many_arguments)]
    fn route_press_to_field(
        &mut self,
        f: Focus,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let mut throwaway = None;
        match f {
            Focus::Search => self.search.handle_event(event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy),
            Focus::Title => self.title.handle_event(event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy),
            Focus::Language => self.language.handle_event(event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy),
            Focus::Tags => self.tags.handle_event(event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy),
            Focus::Description => self.description.handle_event(event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy),
            Focus::Body => self.body.handle_event(event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy),
            Focus::Fill(i) => {
                if let Some((_, field)) = self.fills.get_mut(i) {
                    field.handle_event(event, scale_factor, modifiers, state, path, handled, &mut throwaway, node_id, taffy);
                }
            }
            Focus::None => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn route_key_to_field(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        self.route_press_to_field(self.focus, event, scale_factor, modifiers, state, path, handled, node_id, taffy);
    }

    #[allow(clippy::too_many_arguments)]
    fn forward_to_all_fields(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let mut h = false;
        let mut throwaway = None;
        self.search.handle_event(event, scale_factor, modifiers, state, path, &mut h, &mut throwaway, node_id, taffy);
        if self.detail_on_screen() {
            self.title.handle_event(event, scale_factor, modifiers, state, path, &mut h, &mut throwaway, node_id, taffy);
            self.language.handle_event(event, scale_factor, modifiers, state, path, &mut h, &mut throwaway, node_id, taffy);
            self.tags.handle_event(event, scale_factor, modifiers, state, path, &mut h, &mut throwaway, node_id, taffy);
            self.description.handle_event(event, scale_factor, modifiers, state, path, &mut h, &mut throwaway, node_id, taffy);
            self.body.handle_event(event, scale_factor, modifiers, state, path, &mut h, &mut throwaway, node_id, taffy);
            for (_, f) in self.fills.iter_mut() {
                f.handle_event(event, scale_factor, modifiers, state, path, &mut h, &mut throwaway, node_id, taffy);
            }
        }
    }

    fn focus_next_detail(&mut self, backward: bool) {
        let mut order = vec![Focus::Title, Focus::Language, Focus::Tags, Focus::Description, Focus::Body];
        for i in 0..self.fills.len() {
            order.push(Focus::Fill(i));
        }
        let cur = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let n = order.len();
        let next = if backward { (cur + n - 1) % n } else { (cur + 1) % n };
        self.set_focus(order[next]);
    }

    // ---- list pane ------------------------------------------------------

    fn draw_list(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, area: Rect) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, area, c.neutral[1]);
        let pad = 14.0;

        // Search field: a rounded container with a magnifier, a plain-chrome
        // TextInput inset past it, and a clear affordance when non-empty.
        let sr = Rect::new(area.x0 + pad, area.y0 + 12.0, area.x1 - pad, area.y0 + 12.0 + 38.0);
        let focused = self.focus == Focus::Search;
        draw::fill_round(scene, t, sr, theme.radii_md(), c.surfaces.base);
        draw::stroke_round(scene, t, sr, theme.radii_md(), if focused { 1.5 } else { 1.0 }, if focused { c.border_focus } else { c.border_standard });
        let icon = Rect::new(sr.x0 + 10.0, sr.y0 + (sr.height() - 16.0) / 2.0, sr.x0 + 26.0, sr.y0 + (sr.height() - 16.0) / 2.0 + 16.0);
        Icon::Search.draw(scene, t, icon, c.text_dim);
        let has_query = !self.search_text().is_empty();
        let field_right = if has_query { sr.x1 - 30.0 } else { sr.x1 - 10.0 };
        self.search_rect = Rect::new(sr.x0 + 32.0, sr.y0, field_right, sr.y1);
        self.search.apply_layout_external(self.search_rect);
        self.search.draw(scene, t, tr, theme, &PresenceCtx::empty());
        if has_query {
            let clear = Rect::new(sr.x1 - 28.0, sr.y0, sr.x1, sr.y1);
            let g = Rect::new(clear.x0 + 4.0, clear.y0 + (clear.height() - 14.0) / 2.0, clear.x0 + 18.0, clear.y0 + (clear.height() - 14.0) / 2.0 + 14.0);
            Icon::X.draw(scene, t, g, c.text_dim);
            self.record(clear, Action::ClearSearch);
        }

        // Scope segmented control + New button.
        let seg_y = sr.y1 + 12.0;
        let new_w = 34.0;
        let seg_area = Rect::new(area.x0 + pad, seg_y, area.x1 - pad - new_w - 8.0, seg_y + 30.0);
        self.draw_scopes(scene, t, tr, theme, seg_area);
        let new_rect = Rect::new(area.x1 - pad - new_w, seg_y, area.x1 - pad, seg_y + 30.0);
        let new_face = ButtonFace::new(new_rect, "")
            .icon(Icon::Plus)
            .radius(theme.radii_md())
            .hovered(new_rect.contains(self.last_mouse))
            .accessible_name("New snippet")
            .style(ButtonStyle::Primary);
        Button::paint(scene, t, tr, theme, &new_face);
        self.record(new_rect, Action::NewSnippet);

        // Result rows (scrollable).
        let list_top = seg_y + 42.0;
        let clip = Rect::new(area.x0, list_top, area.x1, area.y1);
        if self.visible.is_empty() {
            self.draw_list_empty(scene, t, tr, theme, clip);
            return;
        }
        let mut y = list_top - self.list_scroll;
        let ids = self.visible.clone();
        for id in ids {
            let Some(s) = self.library.get(id).cloned() else { continue };
            let row = Rect::new(area.x0 + 6.0, y, area.x1 - 6.0, y + ROW_H - 6.0);
            if row.y1 >= list_top - 4.0 && row.y0 <= area.y1 {
                self.draw_list_row(scene, t, tr, theme, row, &s, self.open == Some(id));
            }
            if row.y1 >= list_top && row.y0 <= area.y1 {
                self.record(row, Action::OpenSnippet(id));
            }
            y += ROW_H;
        }
    }

    fn draw_scopes(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, area: Rect) {
        let scopes = [
            (SmartFolder::All, "All"),
            (SmartFolder::Favorites, "Starred"),
            (SmartFolder::MostUsed, "Recent"),
        ];
        let gap = 6.0;
        let w = (area.width() - gap * (scopes.len() as f64 - 1.0)) / scopes.len() as f64;
        let mut x = area.x0;
        for (sf, label) in scopes {
            let r = Rect::new(x, area.y0, x + w, area.y1);
            let active = matches!(&self.selection, Selection::Smart(s) if *s == sf);
            let face = ButtonFace::new(r, label)
                .radius(theme.radii_md())
                .text_size(12.5)
                .weight(TextWeight::Medium)
                .hovered(r.contains(self.last_mouse))
                .style(if active { ButtonStyle::Primary } else { ButtonStyle::Ghost });
            Button::paint(scene, t, tr, theme, &face);
            self.record(r, Action::Scope(sf));
            x += w + gap;
        }
    }

    fn draw_list_empty(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, area: Rect) {
        let library_empty = self.library.snippets.is_empty();
        let (title, msg, cta) = if library_empty {
            ("No snippets yet", "Save code you reuse once, then find it fast.", Some("New snippet"))
        } else {
            ("No matches", "Try a different search.", Some("Clear search"))
        };
        let cta_rect = EmptyState::paint(scene, t, tr, theme, area, Some(Icon::Code), title, Some(msg), cta, self.last_mouse);
        if let Some(rect) = cta_rect {
            self.record(rect, if library_empty { Action::NewSnippet } else { Action::ClearSearch });
        }
    }

    fn draw_list_row(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, row: Rect, s: &Snippet, active: bool) {
        let c = &theme.tokens.colors;
        if active {
            draw::fill_round(scene, t, row, theme.radii_md(), c.surfaces.raised);
            draw::stroke_round(scene, t, row, theme.radii_md(), 1.0, c.accent[5]);
        } else if row.contains(self.last_mouse) {
            draw::fill_round(scene, t, row, theme.radii_md(), c.surface_white_5);
        }
        let pad = 12.0;
        let badge = Rect::new(row.x0 + pad, row.y0 + 11.0, row.x0 + pad + 30.0, row.y0 + 31.0);
        draw::fill_round(scene, t, badge, 6.0, lang_tint(theme, s.language));
        let sig = s.language.sigil();
        let sw = tr.measure(sig, 11.0, None).0 as f64;
        draw::text(scene, tr, t, sig, badge.x0 + (badge.width() - sw) / 2.0, badge.y0 + 3.0, 11.0, c.neutral[0], TextWeight::Bold);

        let tx = badge.x1 + 10.0;
        let title = if s.title.trim().is_empty() { "Untitled snippet" } else { s.title.as_str() };
        draw::text(scene, tr, t, title, tx, row.y0 + 9.0, 14.5, c.text_primary, TextWeight::Semibold);
        if s.favorite {
            let star = Rect::new(row.x1 - pad - 16.0, row.y0 + 9.0, row.x1 - pad, row.y0 + 25.0);
            Icon::Star.draw_filled(scene, t, star, c.semantic.warning);
        }

        let hint = if s.has_placeholders() {
            let n = s.placeholders().len();
            if n == 1 { "1 var".to_string() } else { format!("{n} vars") }
        } else if s.uses > 0 {
            format!("{}×", s.uses)
        } else {
            String::new()
        };
        let hint_w = if hint.is_empty() { 0.0 } else { tr.measure(&hint, 11.0, None).0 as f64 + 14.0 };

        // Preview: the first non-blank body line, clipped to the space left of
        // the trailing hint so the two never overlap.
        let first_line = s.body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        let avail = (row.x1 - pad - hint_w) - tx;
        let max_chars = (avail / draw::mono_width(tr, "m", 12.0)).max(4.0) as usize;
        let preview = clip(first_line, max_chars);
        draw::mono(scene, tr, t, &preview, tx, row.y0 + 30.0, 12.0, c.text_dim, TextWeight::Regular);

        if !hint.is_empty() {
            let hw = tr.measure(&hint, 11.0, None).0 as f64;
            draw::text(scene, tr, t, &hint, row.x1 - pad - hw, row.y0 + 32.0, 11.0, c.text_dim, TextWeight::Medium);
        }
    }

    // ---- detail pane ----------------------------------------------------

    fn draw_detail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, area: Rect) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, area, c.surfaces.base);

        if self.open.is_none() {
            let msg = if self.library.snippets.is_empty() {
                ("Nothing here yet", "Create a snippet to start your library.")
            } else {
                ("No snippet selected", "Pick one from the list to view and copy it.")
            };
            EmptyState::paint(scene, t, tr, theme, area, Some(Icon::FileText), msg.0, Some(msg.1), None, self.last_mouse);
            self.clear_detail_rects();
            return;
        };
        let Some(s) = self.open.and_then(|id| self.library.get(id).cloned()) else {
            self.clear_detail_rects();
            return;
        };

        let back_shown = self.is_push_layout() && self.push.reveal() > 0.5;
        if back_shown {
            self.draw_back(scene, t, theme, area);
        }
        let x0 = area.x0 + PAD + if back_shown { TOGGLE_SIZE + 4.0 } else { 0.0 };
        let right = area.x1 - PAD;
        let mut y = area.y0 + 20.0;

        // Header: title (editable) + Copy (primary) on the right.
        let copy_label = if s.has_placeholders() { "Fill & Copy" } else { "Copy" };
        let copy_w = tr.measure(copy_label, 14.0, None).0 as f64 + 40.0;
        let copy_rect = Rect::new(right - copy_w, y, right, y + 38.0);
        // Title fills the space left of Copy.
        self.title_rect = Rect::new(x0, y, copy_rect.x0 - 14.0, y + 38.0);
        self.title.apply_layout_external(self.title_rect);
        self.title.draw(scene, t, tr, theme, &PresenceCtx::empty());
        let copy_face = ButtonFace::new(copy_rect, copy_label)
            .icon(Icon::Copy)
            .radius(theme.radii_md())
            .text_size(14.0)
            .weight(TextWeight::Semibold)
            .hovered(copy_rect.contains(self.last_mouse))
            .style(ButtonStyle::Primary);
        Button::paint(scene, t, tr, theme, &copy_face);
        self.record(copy_rect, Action::Copy);
        y += 50.0;

        // Meta row: language picker · star · duplicate · delete.
        let lang_w = 150.0;
        self.language_rect = Rect::new(x0, y, x0 + lang_w, y + 32.0);
        self.language.apply_layout_external(self.language_rect);
        self.language.draw(scene, t, tr, theme, &PresenceCtx::empty());
        let mut bx = self.language_rect.x1 + 8.0;
        // Star toggle.
        let star_rect = Rect::new(bx, y, bx + 34.0, y + 32.0);
        let star_face = ButtonFace::new(star_rect, "")
            .icon(Icon::Star)
            .radius(theme.radii_md())
            .hovered(star_rect.contains(self.last_mouse))
            .accessible_name(if s.favorite { "Unstar" } else { "Star" })
            .style(if s.favorite { ButtonStyle::Primary } else { ButtonStyle::Ghost });
        Button::paint(scene, t, tr, theme, &star_face);
        self.record(star_rect, Action::ToggleFavorite);
        bx = star_rect.x1 + 8.0;
        // Duplicate + Delete, quiet.
        let dup_rect = Rect::new(bx, y, bx + 34.0, y + 32.0);
        Button::paint(scene, t, tr, theme, &ButtonFace::new(dup_rect, "").icon(Icon::Copy).radius(theme.radii_md()).hovered(dup_rect.contains(self.last_mouse)).accessible_name("Duplicate").style(ButtonStyle::Ghost));
        self.record(dup_rect, Action::Duplicate);
        bx = dup_rect.x1 + 4.0;
        let del_rect = Rect::new(bx, y, bx + 34.0, y + 32.0);
        Button::paint(scene, t, tr, theme, &ButtonFace::new(del_rect, "").icon(Icon::Trash2).radius(theme.radii_md()).hovered(del_rect.contains(self.last_mouse)).accessible_name("Delete").style(ButtonStyle::Ghost));
        self.record(del_rect, Action::Delete);
        y += 42.0;

        // Tags + description fields.
        self.tags_rect = Rect::new(x0, y, right, y + 30.0);
        self.tags.apply_layout_external(self.tags_rect);
        self.tags.draw(scene, t, tr, theme, &PresenceCtx::empty());
        y += 38.0;
        self.description_rect = Rect::new(x0, y, right, y + 30.0);
        self.description.apply_layout_external(self.description_rect);
        self.description.draw(scene, t, tr, theme, &PresenceCtx::empty());
        y += 40.0;

        // Fill panel (pinned at the bottom for a templated snippet), then the
        // body editor fills the space between.
        let placeholders = model::placeholders_in(&s.body);
        let fill_h = if placeholders.is_empty() { 0.0 } else { self.fill_panel_height(&placeholders) };
        let body_bottom = area.y1 - PAD - fill_h - if fill_h > 0.0 { 14.0 } else { 0.0 };
        self.body_rect = Rect::new(x0, y, right, body_bottom.max(y + 80.0));
        draw::fill_round(scene, t, self.body_rect, theme.radii_md(), c.neutral[0]);
        draw::stroke_round(scene, t, self.body_rect, theme.radii_md(), 1.0, if self.focus == Focus::Body { c.border_focus } else { c.border_standard });
        let inner = Rect::new(
            self.body_rect.x0 + 8.0,
            self.body_rect.y0 + 8.0,
            self.body_rect.x1 - 8.0,
            self.body_rect.y1 - 8.0,
        );
        self.body.set_rect(inner);
        self.body.draw_embedded(scene, t, tr, theme);

        if !placeholders.is_empty() {
            let panel = Rect::new(x0, self.body_rect.y1 + 14.0, right, area.y1 - PAD);
            self.draw_fill_panel(scene, t, tr, theme, panel);
        } else {
            self.fill_rects.clear();
        }
    }

    fn fill_panel_height(&self, placeholders: &[String]) -> f64 {
        let rows = placeholders.len().div_ceil(2) as f64;
        // header + rows(32 each) + preview line.
        22.0 + rows * 38.0 + 26.0
    }

    fn draw_fill_panel(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, area: Rect) {
        let c = &theme.tokens.colors;
        section_header(scene, t, tr, theme, "FILL IN VARIABLES", area.x0, area.y0 + 4.0);
        let mut y = area.y0 + 22.0;
        let col_gap = 12.0;
        let col_w = (area.width() - col_gap) / 2.0;
        let n = self.fills.len();
        self.fill_rects = vec![Rect::ZERO; n];
        for (i, (name, field)) in self.fills.iter_mut().enumerate() {
            let col = i % 2;
            let cx = area.x0 + col as f64 * (col_w + col_gap);
            let label = format!("${{{name}}}");
            let lw = draw::mono_width(tr, &label, 12.0);
            draw::mono(scene, tr, t, &label, cx, y + 8.0, 12.0, c.semantic.warning, TextWeight::Semibold);
            let fr = Rect::new(cx + lw + 8.0, y, cx + col_w, y + 30.0);
            field.apply_layout_external(fr);
            field.draw(scene, t, tr, theme, &PresenceCtx::empty());
            self.fill_rects[i] = fr;
            if col == 1 || i == n - 1 {
                y += 38.0;
            }
        }
        // Live preview of the filled result.
        let preview = self.filled_text();
        let first = preview.lines().next().unwrap_or("");
        let shown = clip(first, ((area.width() - 70.0) / 7.0) as usize);
        draw::text(scene, tr, t, "Preview", area.x0, y + 2.0, 11.0, c.text_dim, TextWeight::Bold);
        draw::mono(scene, tr, t, &shown, area.x0 + 56.0, y + 1.0, 12.0, c.text_primary, TextWeight::Regular);
    }

    fn draw_back(&mut self, scene: &mut Scene, t: Affine, theme: &Theme, area: Rect) {
        let c = &theme.tokens.colors;
        let btn = Rect::new(area.x0 + 10.0, area.y0 + 10.0, area.x0 + 10.0 + TOGGLE_SIZE, area.y0 + 10.0 + TOGGLE_SIZE);
        draw::fill_round(scene, t, btn, theme.radii_md(), c.surfaces.raised);
        draw::stroke_round(scene, t, btn, theme.radii_md(), 1.0, c.border_standard);
        let g = Rect::new(btn.x0 + 9.0, btn.y0 + 9.0, btn.x1 - 9.0, btn.y1 - 9.0);
        Icon::ChevronLeft.draw(scene, t, g, c.text_primary);
        self.record(btn, Action::PushBack);
    }

    fn clear_detail_rects(&mut self) {
        self.title_rect = Rect::ZERO;
        self.language_rect = Rect::ZERO;
        self.tags_rect = Rect::ZERO;
        self.description_rect = Rect::ZERO;
        self.body_rect = Rect::ZERO;
        self.fill_rects.clear();
    }
}

/// Resolve a language label back to a [`Language`] (the Autocomplete accepts a
/// label; parse it, falling back to plain text).
fn language_from_label(label: &str) -> Language {
    Language::ALL.into_iter().find(|l| l.label() == label).unwrap_or(Language::PlainText)
}

/// A per-language accent tint for the row badge, from theme ramps so it tracks
/// the active palette.
fn lang_tint(theme: &Theme, lang: Language) -> vello::peniko::Color {
    let c = &theme.tokens.colors;
    match lang {
        Language::Rust | Language::Toml => c.semantic.warning,
        Language::Shell | Language::Dockerfile => c.semantic.info,
        Language::Sql | Language::Yaml | Language::Json => c.accent[5],
        Language::Git => c.semantic.danger,
        Language::Python => c.semantic.success,
        Language::JavaScript => c.accent[6],
        Language::Markdown | Language::Html | Language::Css => c.neutral[7],
        Language::PlainText => c.neutral[6],
    }
}

fn section_header(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, label: &str, x: f64, y: f64) {
    draw::text(scene, tr, t, label, x, y, 11.0, theme.tokens.colors.text_dim, TextWeight::Bold);
}

fn draw_toast(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, area: Rect, message: &str) {
    let c = &theme.tokens.colors;
    let w = tr.measure(message, 14.0, None).0 as f64 + 40.0;
    let h = 40.0;
    let x = area.x1 - w - 24.0;
    let y = area.y1 - h - 24.0;
    let rect = Rect::new(x, y, x + w, y + h);
    draw::fill_round(scene, t, rect, theme.radii_md(), c.surfaces.raised);
    draw::stroke_round(scene, t, rect, theme.radii_md(), 1.0, c.accent[5]);
    draw::text(scene, tr, t, message, x + 20.0, y + (h - 15.0) / 2.0, 14.0, c.text_primary, TextWeight::Medium);
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
        self.tokens.radii.md as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::SNIPPETS_ACTION_TYPE;
    use ui::widget::Widget;

    fn seeded_app() -> (SnippetsApp, AutoCommit) {
        let mut app = SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
    }

    #[test]
    fn live_app_starts_empty() {
        let app = SnippetsApp::new();
        assert_eq!(app.library_len_for_test(), 0, "live library starts empty");
        assert!(app.open_id_for_test().is_none(), "nothing is opened on an empty library");
        assert_eq!(app.visible_count_for_test(), 0);
    }

    #[test]
    fn empty_live_app_does_not_seed_the_shared_doc() {
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert!(SnippetsDoc::list(&doc).is_empty(), "an empty live library writes no snippets");
    }

    #[test]
    fn demo_seeds_and_opens_a_snippet() {
        let app = SnippetsApp::new_demo();
        assert!(app.library_len_for_test() >= 10);
        assert!(app.open_id_for_test().is_some());
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
    fn search_matches_body_text() {
        let mut app = SnippetsApp::new_demo();
        app.set_search_for_test("kubectl");
        assert!(app.visible_count_for_test() >= 1, "a body-text term filters the list");
        for id in app.visible.clone() {
            let s = app.library.get(id).unwrap();
            assert!(
                s.title.to_lowercase().contains("kubectl") || s.body.to_lowercase().contains("kubectl"),
                "every survivor actually matches the query"
            );
        }
    }

    #[test]
    fn copying_a_plain_snippet_copies_the_body_and_bumps_uses() {
        let (mut app, mut doc) = seeded_app();
        let id = app.library.snippets.iter().find(|s| !s.has_placeholders()).map(|s| s.id).expect("a plain snippet");
        let body = app.library.get(id).unwrap().body.clone();
        let before = app.snippet_uses_for_test(id);
        app.open = Some(id);
        app.reconcile_detail();
        assert_eq!(app.filled_text_for_test(), body, "a plain snippet copies verbatim");
        app.copy_for_test(id, &mut doc);
        assert_eq!(app.snippet_uses_for_test(id), before + 1);
    }

    #[test]
    fn templated_snippet_exposes_fill_fields_and_fills_on_copy() {
        let (mut app, _doc) = seeded_app();
        // Pick a templated snippet and open it.
        let id = app.library.snippets.iter().find(|s| s.has_placeholders()).map(|s| s.id).expect("a templated snippet");
        app.open = Some(id);
        assert!(app.fill_field_count_for_test() >= 1, "a templated snippet exposes variable fields");

        // Unfilled: placeholders survive in the copy (honest partial).
        let raw = app.filled_text_for_test();
        assert!(raw.contains("${"), "unfilled variables stay visible");

        // Fill every variable, then the copy substitutes them.
        let names = model::placeholders_in(&app.library.get(id).unwrap().body);
        for (i, n) in names.iter().enumerate() {
            app.set_fill_for_test(n, &format!("v{i}"));
        }
        let filled = app.filled_text_for_test();
        assert!(!filled.contains("${"), "every variable was substituted: {filled}");
        for (i, _) in names.iter().enumerate() {
            assert!(filled.contains(&format!("v{i}")), "the typed value landed in the copy");
        }
    }

    #[test]
    fn editing_the_body_dispatches_setbody_and_persists() {
        let (mut app, mut doc) = seeded_app();
        let id = app.open_id_for_test().expect("the demo opens a snippet");
        app.edit_body_for_test("echo ${greeting}", &mut doc);
        assert_eq!(SnippetsDoc::get(&doc, id).unwrap().body, "echo ${greeting}", "the body edit reached the document");
        let dispatched = app.last_dispatched_for_test().expect("an action was dispatched");
        assert_eq!(dispatched.type_id, SNIPPETS_ACTION_TYPE);
        // The new variable surfaces as a fill field.
        assert!(app.fill_field_count_for_test() >= 1, "a newly typed ${{}} becomes fillable");
    }

    #[test]
    fn new_snippet_grows_library_opens_and_focuses_title() {
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        let before = app.library_len_for_test();
        app.new_snippet_for_test(&mut doc);
        assert_eq!(app.library_len_for_test(), before + 1);
        assert!(app.open_id_for_test().is_some());
        assert_eq!(app.focus, Focus::Title, "a new snippet lands in the title, ready to author");
    }

    #[test]
    fn sync_seeds_into_empty_doc() {
        let mut app = SnippetsApp::new_demo();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert_eq!(SnippetsDoc::list(&doc).len(), app.library_len_for_test());
    }

    // ---- responsive ----------------------------------------------------

    #[test]
    fn wide_shows_both_panes() {
        let mut app = SnippetsApp::new();
        app.layout_at_width_for_test(1280.0);
        assert!(!app.is_push_for_test(), "a wide window keeps list + detail side by side");
    }

    #[test]
    fn very_narrow_uses_single_pane_push() {
        let mut app = SnippetsApp::new();
        app.layout_at_width_for_test(520.0);
        assert!(app.is_push_for_test(), "a very-narrow window switches to single-pane push");
        assert!(!app.push_showing_detail_for_test(), "push starts on the master list");
    }

    #[test]
    fn opening_a_snippet_pushes_detail_when_narrow() {
        let mut app = SnippetsApp::new_demo();
        app.layout_at_width_for_test(520.0);
        assert!(!app.push_showing_detail_for_test());
        app.open_first_for_test();
        assert!(app.push_showing_detail_for_test(), "selecting a snippet pushes the detail in");
    }

    #[test]
    fn opening_a_snippet_does_not_push_when_wide() {
        let mut app = SnippetsApp::new_demo();
        app.layout_at_width_for_test(1280.0);
        app.open_first_for_test();
        assert!(!app.push_showing_detail_for_test(), "a wide layout shows both panes");
    }

    // ---- action model: affordances dispatch through the shared path ----

    #[test]
    fn toolbar_delete_dispatches_the_same_action_as_the_palette() {
        let (mut app, mut doc) = seeded_app();
        let id = SnippetsDoc::list(&doc)[0].id;
        let resources = Widget::resources(&app, &doc);
        let snip = resources[0]
            .children
            .iter()
            .find(|r| r.id.as_str() == format!("snippets.snippet:{id}"))
            .expect("a resource for the target snippet");
        let spec = snip.actions.iter().find(|a| a.id == "delete").unwrap();
        let spec_encoded = match &spec.input {
            qos_actions::ActionInput::Immediate(e) => e.clone(),
            _ => panic!("delete is immediate"),
        };
        app.delete_for_test(id, &mut doc);
        let dispatched = app.last_dispatched_for_test().expect("the button dispatched an action");
        assert_eq!(dispatched.type_id, spec_encoded.type_id);
        assert_eq!(dispatched.payload, spec_encoded.payload, "button and palette build the identical EncodedAction");
        assert!(SnippetsDoc::get(&doc, id).is_none(), "the snippet was deleted via the dispatched action");
    }

    #[test]
    fn toolbar_star_dispatches_the_same_action_as_the_palette() {
        let (mut app, mut doc) = seeded_app();
        let id = SnippetsDoc::list(&doc).into_iter().find(|s| !s.favorite).map(|s| s.id).expect("an unstarred snippet");
        let resources = Widget::resources(&app, &doc);
        let snip = resources[0].children.iter().find(|r| r.id.as_str() == format!("snippets.snippet:{id}")).unwrap();
        let spec = snip.actions.iter().find(|a| a.id == "toggle-favorite").unwrap();
        let spec_encoded = match &spec.input {
            qos_actions::ActionInput::Immediate(e) => e.clone(),
            _ => panic!("toggle-favorite is immediate"),
        };
        app.toggle_favorite_for_test(id, &mut doc);
        let dispatched = app.last_dispatched_for_test().expect("the star dispatched an action");
        assert_eq!(dispatched.payload, spec_encoded.payload, "the star and the palette build the identical EncodedAction");
        assert!(SnippetsDoc::get(&doc, id).unwrap().favorite, "the snippet is now starred");
    }

    #[test]
    fn affordance_records_one_undo_boundary_through_the_host() {
        let mut app = SnippetsApp::new_demo();
        let mut state = ui::State::new();
        app.sync(&mut state.doc, &state.root);
        let id = SnippetsDoc::list(&state.doc).into_iter().find(|s| !s.favorite).map(|s| s.id).expect("an unstarred snippet");
        app.toggle_favorite_for_test(id, &mut state.doc);
        assert!(SnippetsDoc::get(&state.doc, id).unwrap().favorite);
        let before = Widget::take_committed_action(&mut app).expect("heads were captured");
        assert!(Widget::take_committed_action(&mut app).is_none(), "rising-edge: cleared on read");
        state.record_committed_action(before, None, None);
        assert!(state.peek_last_undo().is_some(), "exactly one undo boundary recorded");
        state.undo(None, None).expect("an undo entry to pop");
        app.sync(&mut state.doc, &state.root);
        assert!(!SnippetsDoc::get(&state.doc, id).unwrap().favorite, "undo reverted the star and nothing else");
    }

    #[test]
    fn steady_state_reprojects_the_document() {
        let (mut app, mut doc) = seeded_app();
        let before = app.library_len_for_test();
        let id = SnippetsDoc::list(&doc)[0].id;
        SnippetsDoc::remove(&mut doc, id);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.library_len_for_test(), before - 1, "the deletion re-projected");
        assert!(!app.library.snippets.iter().any(|s| s.id == id), "and is not resurrected");
    }
}
