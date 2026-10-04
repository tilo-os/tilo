//! Root widget for the snippet manager.
//!
//! The window is the shared [`NavShell`]: the sidebar lists the library's
//! places (smart folders, folders, tags) in a [`NavList`], the overview is
//! the snippet list of the chosen place, and the content is the open
//! snippet. The shell owns the header row, the sidebar toggle, the widths,
//! the drawer and the push stack; this module owns the state they show.
//!
//! The library is mirrored into the shared Automerge document by
//! [`crate::doc`] so two instances converge. Selection, the open snippet and
//! the variables dialog are view state and stay local to each peer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::AutoCommit;
use taffy::{NodeId, TaffyTree};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::code_editor::CodeEditor;
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    AppModel, Button, ContextMenu, Element, MenuItem, ModelPath, NavItem, NavList, NavPane,
    NavSection, NavShell, SidebarPlacement, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::actions::{self, SnippetsAction};
use crate::detail_pane::{self, DetailKey};
use crate::doc::SnippetsDoc;
use crate::expand::ExpandModal;
use crate::highlight;
use crate::list_pane::{ListPane, ListState};
use crate::model::{Language, Library, Selection, SmartFolder, Snippet, SnippetId};
use crate::resource::Snippets;
use crate::seed;
use crate::{tr, tr_with};
use ui::framework::Dispatcher;
use ui::tilo_resource::{PlainText, text::splice_between};

/// The core every part of the window reads and changes.
pub(crate) type Shared = Rc<RefCell<Core>>;

/// Requests from retained controls (the sidebar list, header buttons). They
/// are applied by the root, which holds the document.
pub(crate) type Requests = Rc<RefCell<Vec<Action>>>;

/// What a click, a key or a control asks the app to do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    Select(Selection),
    OpenSnippet(SnippetId),
    NewSnippet,
    Copy(SnippetId),
    ToggleFavorite(SnippetId),
    Duplicate(SnippetId),
    Delete(SnippetId),
    SetLanguage(SnippetId, Language),
    /// Show the language picker under the language button.
    PickLanguage(SnippetId),
    /// Show the menu of the snippet's less frequent actions.
    More(SnippetId),
}

impl Action {
    pub(crate) fn semantic_id(&self) -> String {
        match self {
            Self::Select(Selection::Smart(smart)) => {
                format!("snippets.sidebar.smart:{}", smart.key())
            }
            Self::Select(Selection::Folder(folder)) => {
                format!("snippets.sidebar.folder:{}", actions::stable_key(folder))
            }
            Self::Select(Selection::Tag(tag)) => {
                format!("snippets.sidebar.tag:{}", actions::stable_key(tag))
            }
            Self::OpenSnippet(id) => format!("snippets.snippet:{id}.open"),
            Self::NewSnippet => "snippets.library.new-snippet".into(),
            Self::Copy(id) => format!("snippets.snippet:{id}.copy"),
            Self::ToggleFavorite(id) => format!("snippets.snippet:{id}.toggle-favorite"),
            Self::Duplicate(id) => format!("snippets.snippet:{id}.duplicate"),
            Self::Delete(id) => format!("snippets.snippet:{id}.delete"),
            Self::SetLanguage(id, _) => format!("snippets.snippet:{id}.set-language"),
            Self::PickLanguage(id) => format!("snippets.snippet:{id}.language"),
            Self::More(id) => format!("snippets.snippet:{id}.more"),
        }
    }

    pub(crate) fn accessible_label(&self, core: &Core) -> String {
        match self {
            Self::Select(Selection::Smart(smart)) => smart.label(),
            Self::Select(Selection::Folder(folder)) => {
                tr_with("snippets-folder-named", &[("name", folder)])
            }
            Self::Select(Selection::Tag(tag)) => tr_with("snippets-tag-named", &[("name", tag)]),
            Self::OpenSnippet(id) => core
                .library
                .get(*id)
                .map_or_else(|| tr("snippets-untitled"), |snippet| snippet.title.clone()),
            Self::NewSnippet => tr("snippets-new-snippet"),
            Self::Copy(id) => core.copy_label(*id),
            Self::ToggleFavorite(id) => core.favorite_label(*id),
            Self::Duplicate(_) => tr("snippets-duplicate"),
            Self::Delete(_) => tr("snippets-delete"),
            Self::SetLanguage(_, language) => {
                tr_with("snippets-set-language", &[("language", &language.name())])
            }
            Self::PickLanguage(id) => core.library.get(*id).map_or_else(
                || tr("snippets-language"),
                |snippet| {
                    tr_with(
                        "snippets-language-of",
                        &[("language", &snippet.language.name())],
                    )
                },
            ),
            Self::More(_) => tr("snippets-more-actions"),
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

    /// The control binding a painted control with this action publishes.
    pub(crate) fn binding(&self) -> ui::widget::ControlBindingMetadata {
        let control_id = self.semantic_id();
        ui::widget::ControlBindingMetadata {
            kind: self.canonical_address().map_or_else(
                || ui::widget::ControlBindingKind::SemanticAction(control_id.clone()),
                ui::widget::ControlBindingKind::Action,
            ),
            control_id,
        }
    }
}

/// A short confirmation shown after an action; it goes after the toolkit's
/// toast lifetime, read from the clock (pinned in captures).
#[derive(Clone)]
struct Toast {
    message: String,
    shown_at: ui::clock::Instant,
}

/// An open menu of the snippet: its language picker or its overflow menu.
pub(crate) struct OpenMenu {
    pub(crate) menu: ContextMenu,
    /// What the menu is called, for assistive technology.
    pub(crate) name: String,
    /// What each item does, by index.
    pub(crate) actions: Vec<Action>,
}

/// The state of the window: the library, what is chosen and open, and the
/// painted panes' geometry.
pub(crate) struct Core {
    pub(crate) library: Library,
    pub(crate) selection: Selection,
    pub(crate) filter: String,
    /// Editing cache only; every change is dispatched into `SnippetsDoc`.
    pub(crate) body_editor: CodeEditor,
    body_identity: Option<(SnippetId, Language)>,
    body_edits: Option<Dispatcher<PlainText>>,
    pending_open: Option<SnippetId>,
    /// The snippet the content column shows.
    pub(crate) open: Option<SnippetId>,
    /// Ordered ids the list shows (the chosen place's snippets).
    pub(crate) visible: Vec<SnippetId>,
    /// The fill-in-variables dialog, when open.
    pub(crate) expand: Option<ExpandModal>,
    pub(crate) menu: Option<OpenMenu>,
    toast: Option<Toast>,
    dispatch: Option<Dispatcher<Snippets>>,
    /// The pointer, in window coordinates.
    pub(crate) pointer: Point,
    /// The window, for dialogs and menus.
    pub(crate) window: Rect,
    pub(crate) list: ListState,
    /// Where the header's overflow button is, for its menu.
    pub(crate) more_anchor: Rect,
    /// Where the language button is, for its picker.
    pub(crate) language_anchor: Rect,
    /// The screen a narrow window should bring to the front next.
    show_pane: Option<NavPane>,
    /// The body editor may take input (the content column is on screen and
    /// nothing floats over it).
    pub(crate) body_reachable: bool,
}

impl Core {
    fn with_library(library: Library) -> Self {
        let mut core = Self {
            library,
            selection: Selection::default(),
            filter: String::new(),
            body_editor: CodeEditor::local()
                .with_id("snippets.body")
                .with_placeholder("snippets-body-placeholder"),
            body_identity: None,
            body_edits: None,
            pending_open: None,
            open: None,
            visible: Vec::new(),
            expand: None,
            menu: None,
            toast: None,
            dispatch: None,
            pointer: Point::ZERO,
            window: Rect::ZERO,
            list: ListState::default(),
            more_anchor: Rect::ZERO,
            language_anchor: Rect::ZERO,
            show_pane: None,
            body_reachable: false,
        };
        core.recompute_visible();
        core
    }

    pub(crate) fn recompute_visible(&mut self) {
        self.visible = self.library.query(&self.selection, &self.filter);
        // Keep the open snippet valid; otherwise open the first one.
        if let Some(open) = self.open {
            if !self.visible.contains(&open) {
                self.open = self.visible.first().copied();
            }
        } else {
            self.open = self.visible.first().copied();
        }
    }

    fn select(&mut self, sel: Selection) {
        if self.selection == sel {
            return;
        }
        self.selection = sel;
        self.list.scroll = 0.0;
        self.recompute_visible();
    }

    pub(crate) fn open_snippet(&mut self, id: SnippetId) {
        self.open = Some(id);
        self.list.reveal = true;
        // A narrow window pushes the content screen in; a wide one already
        // shows it.
        self.show_pane = Some(NavPane::Content);
    }

    /// The column title of the list: what the chosen place is called.
    pub(crate) fn selection_title(&self) -> String {
        match &self.selection {
            Selection::Smart(smart) => smart.label(),
            Selection::Folder(folder) | Selection::Tag(folder) => folder.clone(),
        }
    }

    /// Open the snippet a capture shows off: a substantial, templated piece
    /// of code, so the content column shows the highlighter and the
    /// variables action.
    fn open_specimen(&mut self) {
        self.open = self
            .visible
            .iter()
            .copied()
            .max_by_key(|id| {
                let s = self.library.get(*id);
                let lines = s.map_or(0, |s| s.body.lines().count());
                let templated = usize::from(s.is_some_and(Snippet::has_placeholders));
                let is_program = usize::from(s.is_some_and(|s| {
                    !matches!(s.language, Language::Markdown | Language::PlainText)
                }));
                (is_program, templated, lines)
            })
            .or_else(|| self.visible.first().copied());
        self.list.reveal = true;
    }

    /// The column title of the content: the open snippet's title.
    pub(crate) fn open_title(&self) -> String {
        self.open
            .and_then(|id| self.library.get(id))
            .map(|s| s.title.clone())
            .unwrap_or_default()
    }

    pub(crate) fn copy_label(&self, id: SnippetId) -> String {
        tr(
            if self.library.get(id).is_some_and(Snippet::has_placeholders) {
                "snippets-fill-and-copy"
            } else {
                "snippets-copy"
            },
        )
    }

    pub(crate) fn favorite_label(&self, id: SnippetId) -> String {
        tr(if self.library.get(id).is_some_and(|s| s.favorite) {
            "snippets-remove-favorite"
        } else {
            "snippets-add-favorite"
        })
    }

    /// The sidebar: the smart folders, then folders and tags (a section only
    /// when it has places), each with its count.
    fn nav_sections(&self) -> Vec<NavSection> {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        let smart = [
            (SmartFolder::All, Icon::Layers),
            (SmartFolder::Favorites, Icon::Star),
            (SmartFolder::MostUsed, Icon::History),
        ]
        .into_iter()
        .map(|(smart, icon)| {
            let selection = Selection::Smart(smart);
            NavItem::new(nav_id(&selection), smart.label())
                .icon(icon)
                .count(count(self.library.query(&selection, "").len()))
        })
        .collect();
        let mut sections = vec![NavSection::new(
            "library",
            tr("snippets-section-library"),
            smart,
        )];
        let folders: Vec<NavItem> = self
            .library
            .folders()
            .into_iter()
            .map(|folder| {
                let selection = Selection::Folder(folder.clone());
                NavItem::new(nav_id(&selection), folder)
                    .icon(Icon::Folder)
                    .count(count(self.library.query(&selection, "").len()))
            })
            .collect();
        if !folders.is_empty() {
            sections.push(NavSection::new(
                "folders",
                tr("snippets-section-folders"),
                folders,
            ));
        }
        let tags: Vec<NavItem> = self
            .library
            .tags()
            .into_iter()
            .map(|(tag, n)| NavItem::new(nav_id(&Selection::Tag(tag.clone())), tag).count(count(n)))
            .collect();
        if !tags.is_empty() {
            sections.push(NavSection::new("tags", tr("snippets-section-tags"), tags));
        }
        sections
    }

    /// Dispatch one [`SnippetsAction`] — the single mutation path shared with
    /// the palette. The shared document is authoritative; the in-memory
    /// library is re-projected from it so the panes reflect the change this
    /// frame.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &SnippetsAction) {
        if let Some(dispatch) = &self.dispatch {
            if dispatch.apply_now(doc, action).is_ok() {
                self.show_library(&SnippetsDoc::list(doc));
            }
        }
    }

    /// Show the authoritative library the framework resource holds,
    /// including an empty library after the final snippet is deleted.
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

    pub(crate) fn sync_body_editor(&mut self) {
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

    pub(crate) fn flush_body_edit(&mut self, doc: &mut AutoCommit) {
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

    pub(crate) fn body_available(&self) -> bool {
        self.open.is_some()
            && self.expand.is_none()
            && self.menu.is_none()
            && self.body_reachable
            && self.body_editor.rect().area() > 0.0
    }

    /// Hand `event` to the body editor when it is the editor's: a press on
    /// it, the pointer while it drags, keys while it has focus.
    pub(crate) fn route_body_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.body_available() {
            self.body_editor.set_focused(false);
            return false;
        }
        let modifiers = *ctx.modifiers;
        let inside = self.body_editor.rect().contains(self.pointer);
        let captured = self.body_editor.wants_pointer_capture();
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            self.body_editor.set_focused(inside);
        }
        let focused = self.body_editor.is_focused();
        if let WindowEvent::KeyboardInput { event, .. } = event {
            // Escape and the app's own shortcuts leave the editor.
            let global = matches!(&event.logical_key, Key::Named(NamedKey::Escape))
                || ((modifiers.state().control_key() || modifiers.state().super_key())
                    && matches!(&event.logical_key, Key::Character(key) if (key.eq_ignore_ascii_case("n") || key.eq_ignore_ascii_case("f"))));
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
                handled: &mut handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        self.flush_body_edit(ctx.state);
        // Pointer motion stays everyone's; the editor only claims what it
        // consumed.
        handled && !matches!(event, WindowEvent::CursorMoved { .. })
    }

    /// The presentation context the palette needs to build the library-level
    /// "New snippet" action (which folder to file it under).

    fn new_snippet(&mut self, doc: &mut AutoCommit) {
        let folder = match &self.selection {
            Selection::Folder(f) => f.clone(),
            _ => String::new(),
        };
        // The uuid is minted here, at authoring time, and carried in the
        // action payload so replay reproduces the identical row.
        let s = Snippet::blank(&folder);
        let id = s.id;
        self.perform_action(doc, &actions::create_snippet_action(s));
        // A new snippet is neither a favorite nor used yet: show it in a
        // place that holds it.
        if matches!(
            self.selection,
            Selection::Smart(SmartFolder::Favorites | SmartFolder::MostUsed) | Selection::Tag(_)
        ) {
            self.selection = Selection::default();
        }
        self.recompute_visible();
        self.open_snippet(id);
        self.toast(&tr("snippets-created"));
    }

    fn copy(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        // A body with placeholders goes through the fill-in dialog so raw
        // `${...}` never lands on the clipboard.
        let needs_expand = self.library.get(id).is_some_and(Snippet::has_placeholders);
        if needs_expand {
            self.start_expand(id);
            return;
        }
        // Copying is presentation; the usage bump it records is a document
        // mutation, so it goes through an action.
        if let Some(body) = self.library.get(id).map(|s| s.body.clone()) {
            ui::clipboard_copy(&body);
            self.record_use(id, doc);
            self.toast(&tr("snippets-copied"));
        }
    }

    /// Record one use of a snippet (drives "Most used") through the action
    /// path, so the bump replicates and is undoable like any mutation.
    fn record_use(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.perform_action(doc, &actions::record_use_action(id));
    }

    pub(crate) fn start_expand(&mut self, id: SnippetId) {
        if let Some(s) = self.library.get(id) {
            self.body_editor.set_focused(false);
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
            copy.title = tr_with("snippets-copy-title", &[("title", &src.title)]);
            copy.favorite = false;
            copy.uses = 0;
            let new_id = copy.id;
            self.perform_action(doc, &actions::create_snippet_action(copy));
            self.recompute_visible();
            if self.visible.contains(&new_id) {
                self.open_snippet(new_id);
            }
            self.toast(&tr("snippets-duplicated"));
        }
    }

    fn delete(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.perform_action(doc, &actions::delete_snippet_action(id));
        if self.open == Some(id) {
            self.open = None;
        }
        self.recompute_visible();
        self.toast(&tr("snippets-deleted"));
    }

    fn set_language(&mut self, id: SnippetId, lang: Language, doc: &mut AutoCommit) {
        self.perform_action(doc, &actions::set_language_action(id, lang));
        self.recompute_visible();
    }

    fn toast(&mut self, message: &str) {
        self.toast = Some(Toast {
            message: message.to_string(),
            shown_at: ui::clock::now(),
        });
    }

    /// Apply a retained control's request through the shared action path.
    pub(crate) fn apply(&mut self, action: Action, doc: &mut AutoCommit) {
        match action {
            Action::Select(sel) => self.select(sel),
            Action::OpenSnippet(id) => self.open_snippet(id),
            Action::NewSnippet => self.new_snippet(doc),
            Action::Copy(id) => self.copy(id, doc),
            Action::ToggleFavorite(id) => self.toggle_favorite(id, doc),
            Action::Duplicate(id) => self.duplicate(id, doc),
            Action::Delete(id) => self.delete(id, doc),
            Action::SetLanguage(id, lang) => self.set_language(id, lang, doc),
            Action::PickLanguage(id) => self.open_languages(id),
            Action::More(id) => self.open_more(id),
        }
    }

    /// Open the snippet's language picker under the language button, the
    /// current language checked.
    pub(crate) fn open_languages(&mut self, id: SnippetId) {
        let Some(current) = self.library.get(id).map(|s| s.language) else {
            return;
        };
        let mut menu = ContextMenu::new();
        for language in Language::ALL {
            menu.push(MenuItem::action(language.name(), || {}).with_checked(language == current));
        }
        let actions = Language::ALL
            .into_iter()
            .map(|language| Action::SetLanguage(id, language))
            .collect();
        self.open_menu(menu, self.language_anchor, tr("snippets-language"), actions);
    }

    /// Open the snippet's overflow menu under the header's overflow button.
    pub(crate) fn open_more(&mut self, id: SnippetId) {
        if self.library.get(id).is_none() {
            return;
        }
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action(tr("snippets-duplicate"), || {}).with_icon(Icon::Copy));
        menu.push(
            MenuItem::destructive_action(tr("snippets-delete"), || {}).with_icon(Icon::Trash2),
        );
        self.open_menu(
            menu,
            self.more_anchor,
            tr("snippets-more-actions"),
            vec![Action::Duplicate(id), Action::Delete(id)],
        );
    }

    /// Show `menu` under `anchor`, the control that asked for it.
    fn open_menu(
        &mut self,
        mut menu: ContextMenu,
        anchor: Rect,
        name: String,
        actions: Vec<Action>,
    ) {
        if anchor.area() <= 0.0 {
            return;
        }
        menu.open_anchored(anchor, self.window);
        self.body_editor.set_focused(false);
        self.menu = Some(OpenMenu {
            menu,
            name,
            actions,
        });
    }

    /// Route an event to the open menu; apply what it chose.
    fn route_menu_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let Some(open) = self.menu.as_mut() else {
            return;
        };
        open.menu.handle_event(event, ctx);
        let chosen = open
            .menu
            .take_selection()
            .and_then(|index| open.actions.get(index).cloned());
        if !open.menu.is_open() {
            self.menu = None;
        }
        if let Some(action) = chosen {
            self.menu = None;
            self.apply(action, ctx.state);
        }
    }

    fn finish_expand(&mut self, modal: ExpandModal, outcome: ExpandOutcome, doc: &mut AutoCommit) {
        match outcome {
            ExpandOutcome::Stay => {
                self.expand = Some(modal);
            }
            ExpandOutcome::Cancel => {}
            ExpandOutcome::Copy(text, id) => {
                ui::clipboard_copy(&text);
                self.record_use(id, doc);
                self.toast(&tr("snippets-copied"));
            }
        }
    }

    /// Every painted row on screen.
    pub(crate) fn hits(&self) -> impl Iterator<Item = &(Rect, Action)> {
        self.list.hits.iter()
    }

    fn handle_keyboard_input(&mut self, ctx: &mut ui::widget::EventCtx<'_>, key: &Key) -> bool {
        let mods = ctx.modifiers.state();
        if !(mods.control_key() || mods.super_key()) {
            return false;
        }
        let Key::Character(ch) = key else {
            return false;
        };
        match ch.to_lowercase().as_str() {
            "n" => {
                self.new_snippet(ctx.state);
                true
            }
            "c" if !self.body_editor.is_focused() => {
                if let Some(id) = self.open {
                    self.copy(id, ctx.state);
                }
                true
            }
            _ => false,
        }
    }

    fn update_toast(&mut self) {
        if self.toast.as_ref().is_some_and(|toast| {
            ui::clock::now().saturating_duration_since(toast.shown_at)
                >= ui::widget::toast::DEFAULT_TTL
        }) {
            self.toast = None;
        }
    }
}

/// The sidebar item id of a place.
fn nav_id(selection: &Selection) -> String {
    match selection {
        Selection::Smart(SmartFolder::All) => "smart:all".into(),
        Selection::Smart(SmartFolder::Favorites) => "smart:favorites".into(),
        Selection::Smart(SmartFolder::MostUsed) => "smart:most-used".into(),
        Selection::Folder(folder) => format!("folder:{folder}"),
        Selection::Tag(tag) => format!("tag:{tag}"),
    }
}

/// The place a sidebar item id names.
fn selection_of(id: &str) -> Option<Selection> {
    match id {
        "smart:all" => Some(Selection::Smart(SmartFolder::All)),
        "smart:favorites" => Some(Selection::Smart(SmartFolder::Favorites)),
        "smart:most-used" => Some(Selection::Smart(SmartFolder::MostUsed)),
        _ => id
            .strip_prefix("folder:")
            .map(|folder| Selection::Folder(folder.to_string()))
            .or_else(|| {
                id.strip_prefix("tag:")
                    .map(|tag| Selection::Tag(tag.to_string()))
            }),
    }
}

/// The sidebar: the shared [`NavList`], kept in step with the library.
struct SidebarNav {
    list: NavList,
    core: Shared,
    sections: Vec<NavSection>,
    selected: Option<String>,
}

impl SidebarNav {
    fn new(core: &Shared, requests: &Requests) -> Self {
        let requests = Rc::clone(requests);
        let (sections, selected) = {
            let core = core.borrow();
            (core.nav_sections(), nav_id(&core.selection))
        };
        let list = NavList::new(sections.clone())
            .with_id("snippets.sidebar")
            .with_accessible_name(tr("snippets-library"))
            .selected(&selected)
            .on_select(move |id| {
                if let Some(selection) = selection_of(&id) {
                    requests.borrow_mut().push(Action::Select(selection));
                }
            });
        Self {
            list,
            core: Rc::clone(core),
            sections,
            selected: Some(selected),
        }
    }

    /// Follow the library and the chosen place.
    fn refresh(&mut self) {
        let (sections, selected) = {
            let core = self.core.borrow();
            (core.nav_sections(), nav_id(&core.selection))
        };
        if sections != self.sections {
            self.list.set_sections(sections.clone());
            self.sections = sections;
        }
        if self.selected.as_deref() != Some(selected.as_str())
            || self.list.selected_id().as_deref() != Some(selected.as_str())
        {
            self.list.set_selected(Some(&selected));
            self.selected = Some(selected);
        }
    }
}

impl ui::widget::Decorator for SidebarNav {
    type Inner = NavList;

    fn inner(&self) -> &NavList {
        &self.list
    }

    fn inner_mut(&mut self) -> &mut NavList {
        &mut self.list
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.refresh();
        Widget::sync(&mut self.list, state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.refresh();
        Widget::update(&mut self.list, state, path)
    }

    fn name(&self) -> &'static str {
        "SnippetsSidebar"
    }
}

/// Result of routing an event to the variables dialog.
pub enum ExpandOutcome {
    /// Keep the dialog open.
    Stay,
    /// The person dismissed it.
    Cancel,
    /// The person confirmed; the filled text and the snippet it came from.
    Copy(String, SnippetId),
}

/// The application root: the window shell around the library's parts.
pub struct SnippetsApp {
    core: Shared,
    requests: Requests,
    shell: NavShell,
    /// The column titles last handed to the shell.
    titles: (String, String),
    /// What the content column was last built for.
    detail: DetailKey,
    /// Whether the overview column is in the shell (not for an empty
    /// library, whose one action the content column offers).
    overview_shown: bool,
    /// A screenshot scenario to stage once the shell has laid out.
    pending_scenario: Option<String>,
    /// The capture's specimen snippet has been opened.
    specimen_open: bool,
    /// How many layouts in a row gave the window the same rect.
    settled_layouts: u32,
}

/// Layouts at one window size before a capture stages its scenario.
const SETTLED_LAYOUTS: u32 = 3;

impl SnippetsApp {
    /// Build the live app on an **empty** library. A fresh install has no
    /// snippets — the user saves their own, or a peer's shared library is
    /// adopted on the first `sync`. The starter set is demo/`--shots` only
    /// (see [`Self::new_demo`]).
    pub fn new() -> Self {
        Self::with_library(Library::new())
    }

    /// Like [`Self::new`] but with the starter library and a specimen open:
    /// a library doing real work, for tests. A capture creates the same
    /// library through the resource's actions instead
    /// ([`Self::stage_scenario`]).
    pub fn new_demo() -> Self {
        let mut library = Library::new();
        for s in seed::starter_library() {
            library.insert(s);
        }
        let mut app = Self::with_library(library);
        {
            let mut core = app.core.borrow_mut();
            core.open_specimen();
            core.show_pane = None;
        }
        app.refresh_columns();
        app
    }

    fn with_library(library: Library) -> Self {
        let core: Shared = Rc::new(RefCell::new(Core::with_library(library)));
        let requests: Requests = Rc::default();
        let new_snippet = {
            let requests = Rc::clone(&requests);
            Button::icon_only("snippets.new", Icon::Plus, &tr("snippets-new-snippet"))
                .on_effect(move || requests.borrow_mut().push(Action::NewSnippet))
        };
        let search = ui::search::search_button(ui::search::SearchRequest::this_app());
        let (titles, detail, library_empty) = {
            let core = core.borrow();
            (
                (core.selection_title(), core.open_title()),
                DetailKey::of(&core),
                core.library.snippets.is_empty(),
            )
        };
        let mut shell = NavShell::new()
            .with_id("snippets.shell")
            .sidebar(Box::new(SidebarNav::new(&core, &requests)))
            .content(detail_pane::build(&core, &requests))
            .overview_header(
                titles.0.clone(),
                vec![Box::new(search) as Element, Box::new(new_snippet)],
            )
            .content_header(
                titles.1.clone(),
                vec![
                    detail_pane::favorite_button(&core, &requests),
                    detail_pane::more_button(&core, &requests),
                ],
            );
        if !library_empty {
            shell.set_overview(Some(Box::new(ui::widget::Filtered::new(Box::new(
                ListPane::new(&core),
            )))));
        }
        Self {
            core,
            requests,
            shell,
            titles,
            detail,
            overview_shown: !library_empty,
            pending_scenario: None,
            specimen_open: false,
            settled_layouts: 0,
        }
    }

    /// Stage a capture: create the starter library through the resource's
    /// own action (none for the `empty` scenario), then, once it is
    /// presented and the window has its size, the scenario's view state:
    ///
    /// * `narrow` — the sidebar drawer open over the content.
    /// * `detail` — a narrow window showing the open snippet.
    /// * `expand` — the fill-in-variables dialog over the open snippet.
    /// * `language`, `more` — the language picker or the overflow menu.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.pending_scenario = Some(scenario.unwrap_or_default().to_owned());
    }

    fn stage_pending_scenario(&mut self) {
        let Some(scenario) = self.pending_scenario.take() else {
            return;
        };
        // The starter library is still on its way.
        if scenario != "empty" && self.core.borrow().library.snippets.is_empty() {
            self.pending_scenario = Some(scenario);
            return;
        }
        // First open the specimen, and let the window lay its column out.
        if !self.specimen_open {
            self.specimen_open = true;
            {
                let mut core = self.core.borrow_mut();
                core.open_specimen();
                core.show_pane = None;
            }
            self.refresh_columns();
            self.settled_layouts = 0;
            self.pending_scenario = Some(scenario);
            return;
        }
        match scenario.as_str() {
            "narrow" => self.shell.open_sidebar(),
            "detail" => self.shell.show(NavPane::Content),
            "expand" => {
                let mut core = self.core.borrow_mut();
                if let Some(id) = core.open {
                    core.start_expand(id);
                }
            }
            "language" | "more" => {
                let mut core = self.core.borrow_mut();
                if let Some(id) = core.open {
                    if scenario == "language" {
                        core.open_languages(id);
                    } else {
                        core.open_more(id);
                    }
                }
            }
            _ => {}
        }
        self.shell.snap();
    }

    /// Keep the columns in step with the library: the overview while there
    /// is a library to list, and the content column for what is open.
    fn refresh_columns(&mut self) {
        let (detail, library_empty) = {
            let core = self.core.borrow();
            (DetailKey::of(&core), core.library.snippets.is_empty())
        };
        if self.overview_shown == library_empty {
            self.overview_shown = !library_empty;
            self.shell.set_overview((!library_empty).then(|| {
                Box::new(ui::widget::Filtered::new(Box::new(ListPane::new(
                    &self.core,
                )))) as Element
            }));
        }
        if detail != self.detail {
            self.detail = detail;
            self.shell
                .set_content(detail_pane::build(&self.core, &self.requests));
        }
    }

    /// Apply what retained controls asked for, bring the screen a choice
    /// asked for to the front, and keep the column titles current.
    fn settle(&mut self, doc: &mut AutoCommit) {
        let requests = std::mem::take(&mut *self.requests.borrow_mut());
        for action in requests {
            self.core.borrow_mut().apply(action, doc);
        }
        self.settle_view();
    }

    fn settle_view(&mut self) {
        self.refresh_columns();
        let (pane, titles) = {
            let mut core = self.core.borrow_mut();
            (
                core.show_pane.take(),
                (core.selection_title(), core.open_title()),
            )
        };
        if let Some(pane) = pane {
            self.shell.show(pane);
        }
        if titles.0 != self.titles.0 {
            self.shell.set_overview_title(titles.0.clone());
        }
        if titles.1 != self.titles.1 {
            self.shell.set_content_title(titles.1.clone());
        }
        self.titles = titles;
        // The body editor takes input only while the content column is on
        // screen and no drawer floats over it.
        let geometry = self.shell.geometry();
        let drawer_open =
            geometry.sidebar == SidebarPlacement::Drawer && self.shell.is_sidebar_open();
        let content_hidden =
            !geometry.stack.is_empty() && self.shell.front_pane() != NavPane::Content;
        self.core.borrow_mut().body_reachable = !drawer_open && !content_hidden;
    }

    pub(crate) fn show_library(&mut self, state: &[Snippet]) {
        self.core.borrow_mut().show_library(state);
        self.refresh_columns();
    }

    pub(crate) fn attach_dispatcher(&mut self, dispatcher: Dispatcher<Snippets>) {
        let mut core = self.core.borrow_mut();
        core.dispatch = Some(dispatcher);
        core.body_identity = None;
        core.sync_body_editor();
    }

    pub(crate) fn show(&mut self, view: actions::SnippetsView) -> ui::framework::EffectAnswer {
        let mut core = self.core.borrow_mut();
        let mut reset_filter = false;
        match view {
            actions::SnippetsView::Open { snippet } => {
                if core.library.get(snippet).is_none() {
                    return Err("snippet no longer exists".into());
                }
                if !core.visible.contains(&snippet) {
                    core.filter.clear();
                    core.select(Selection::default());
                    core.recompute_visible();
                    reset_filter = true;
                }
                core.open_snippet(snippet);
            }
            actions::SnippetsView::Reveal { tag } => {
                if !core.library.tags().iter().any(|(known, _)| known == &tag) {
                    return Err("tag no longer exists".into());
                }
                core.filter.clear();
                core.select(Selection::Tag(tag));
                core.recompute_visible();
                reset_filter = true;
                core.show_pane = Some(NavPane::Overview);
            }
            actions::SnippetsView::Search => {
                core.body_editor.set_focused(false);
                if let Some(dispatch) = &core.dispatch {
                    dispatch.open_search(&ui::search::SearchRequest::this_app());
                }
            }
        }
        drop(core);
        if reset_filter {
            self.shell
                .set_overview(Some(Box::new(ui::widget::Filtered::new(Box::new(
                    ListPane::new(&self.core),
                )))));
        }
        self.settle_view();
        Ok(ui::framework::Effected::done())
    }

    // ---- test accessors -------------------------------------------------

    #[doc(hidden)]
    pub fn open_id_for_test(&self) -> Option<SnippetId> {
        self.core.borrow().open
    }

    #[doc(hidden)]
    pub fn visible_count_for_test(&self) -> usize {
        self.core.borrow().visible.len()
    }

    #[doc(hidden)]
    pub fn library_len_for_test(&self) -> usize {
        self.core.borrow().library.snippets.len()
    }

    #[doc(hidden)]
    pub fn expand_open_for_test(&self) -> bool {
        self.core.borrow().expand.is_some()
    }

    #[doc(hidden)]
    pub fn copy_for_test(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.core.borrow_mut().copy(id, doc);
    }

    #[doc(hidden)]
    pub fn new_snippet_for_test(&mut self, doc: &mut AutoCommit) {
        self.core.borrow_mut().new_snippet(doc);
    }

    #[doc(hidden)]
    pub fn snippet_uses_for_test(&self, id: SnippetId) -> u32 {
        self.core.borrow().library.get(id).map_or(0, |s| s.uses)
    }

    #[doc(hidden)]
    #[allow(dead_code, reason = "framework migration test helper")]

    /// Drive the Delete menu item exactly as a choice resolves it.
    #[doc(hidden)]
    pub fn delete_for_test(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.core.borrow_mut().apply(Action::Delete(id), doc);
    }

    /// Drive the favorite button the same way.
    #[doc(hidden)]
    pub fn toggle_favorite_for_test(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.core
            .borrow_mut()
            .apply(Action::ToggleFavorite(id), doc);
    }

    #[doc(hidden)]
    pub fn sidebar_is_overlay_for_test(&self) -> bool {
        self.shell.geometry().sidebar == SidebarPlacement::Drawer
    }

    #[doc(hidden)]
    pub fn sidebar_open_for_test(&self) -> bool {
        self.shell.is_sidebar_open()
    }

    #[doc(hidden)]
    pub fn is_push_for_test(&self) -> bool {
        !self.shell.geometry().stack.is_empty()
    }

    #[doc(hidden)]
    pub fn push_showing_detail_for_test(&self) -> bool {
        self.is_push_for_test() && self.shell.front_pane() == NavPane::Content
    }

    #[doc(hidden)]
    pub fn toggle_sidebar_for_test(&mut self) {
        self.shell.toggle_sidebar();
        self.shell.snap();
    }

    #[doc(hidden)]
    pub fn open_first_for_test(&mut self) {
        let first = self.core.borrow().visible.first().copied();
        if let Some(id) = first {
            self.core.borrow_mut().open_snippet(id);
            self.settle_view();
        }
    }
}

impl Default for SnippetsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ui::widget::Decorator for SnippetsApp {
    type Inner = NavShell;

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    ui::provides!(dyn ui::undo::UndoPoints; inner = shell);

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            {
                let mut core = self.core.borrow_mut();
                core.sync_body_editor();
            }
            self.settle(doc);
        }
        Widget::sync(&mut self.shell, state, path);
    }

    fn needs_idle_sync(&self) -> bool {
        Widget::needs_idle_sync(&self.shell)
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.settle(doc);
        }
        // A capture's staged screen needs the window's final geometry (the
        // host may still add its own bars in the first frames): stage it
        // once a few layouts have given the window the same rect.
        if self.settled_layouts >= SETTLED_LAYOUTS {
            self.stage_pending_scenario();
        }
        self.core.borrow_mut().update_toast();
        Widget::update(&mut self.shell, state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = Widget::build_layout(&mut self.shell, taffy, theme, node_map);
        // The undo hooks the host sweeps through the node map are this
        // root's, not the shell's.
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        Widget::apply_layout(&mut self.shell, taffy, node, origin);
        let window = Widget::get_rect(&self.shell);
        let mut core = self.core.borrow_mut();
        self.settled_layouts = if core.window == window && window.area() > 0.0 {
            self.settled_layouts.saturating_add(1)
        } else {
            0
        };
        core.window = window;
        if let Some(open) = core.menu.as_mut() {
            open.menu.constrain_to(window);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.core.borrow_mut().body_editor.set_rect(Rect::ZERO);
        Widget::draw(&mut self.shell, scene, transform, text, theme, presence);
        self.paint_toast(scene, transform, text, theme);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.core.borrow_mut().body_editor.set_rect(Rect::ZERO);
        Widget::draw_with_presence(
            &mut self.shell,
            scene,
            transform,
            text,
            theme,
            presence,
            permissions,
        );
        self.paint_toast(scene, transform, text, theme);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        Widget::draw_overlay(&mut self.shell, scene, transform, text, theme, presence);
        let mut core = self.core.borrow_mut();
        let window = core.window;
        if let Some(open) = core.menu.as_mut() {
            open.menu.layout_within(text, theme);
            Widget::draw(&mut open.menu, scene, transform, text, theme, presence);
        }
        if let Some(modal) = core.expand.as_mut() {
            modal.draw(scene, transform, text, theme, window);
            modal.draw_overlay(scene, transform, text, theme, presence);
        } else if core.body_available() {
            core.body_editor
                .draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.core.borrow_mut().pointer =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        let consumed = {
            let mut core = self.core.borrow_mut();
            // The variables dialog is modal: it owns all input while open.
            if let Some(mut modal) = core.expand.take() {
                let pointer = core.pointer;
                let outcome = modal.handle_event(event, ctx, pointer);
                core.finish_expand(modal, outcome, ctx.state);
                true
            } else if core.menu.is_some() {
                core.route_menu_event(event, ctx);
                !matches!(event, WindowEvent::CursorMoved { .. })
            } else {
                false
            }
        };
        if consumed {
            *ctx.handled = true;
        } else {
            Widget::handle_event(&mut self.shell, event, ctx);
            if !*ctx.handled
                && let WindowEvent::KeyboardInput {
                    event:
                        KeyEvent {
                            logical_key,
                            state: ElementState::Pressed,
                            ..
                        },
                    ..
                } = event
                && self
                    .core
                    .borrow_mut()
                    .handle_keyboard_input(ctx, logical_key)
            {
                *ctx.handled = true;
            }
        }
        self.settle(ctx.state);
    }

    fn modal_capture(&self) -> Option<Rect> {
        let modal = self
            .core
            .borrow()
            .expand
            .as_ref()
            .map(ExpandModal::card_rect);
        modal.or_else(|| Widget::modal_capture(&self.shell))
    }

    fn needs_animation_frame(&self) -> bool {
        // A capture keeps frames coming until its scenario is staged.
        self.pending_scenario.is_some()
            || self.core.borrow().toast.is_some()
            || Widget::needs_animation_frame(&self.shell)
    }

    fn has_pending_visual_work(&self) -> bool {
        // Capture staging waits for visual readiness, independently of the
        // animation clock. The requested scene is incomplete until its
        // geometry-dependent view state has been applied.
        self.pending_scenario.is_some() || Widget::has_pending_visual_work(&self.shell)
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        let core = self.core.borrow();
        if let Some(modal) = &core.expand {
            modal.ime_cursor_rect()
        } else if core.body_editor.is_focused() {
            core.body_editor.ime_cursor_rect()
        } else {
            drop(core);
            Widget::ime_cursor_rect(&self.shell)
        }
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        let core = self.core.borrow();
        if let Some(modal) = &core.expand {
            modal.ime_purpose()
        } else if core.body_editor.is_focused() {
            core.body_editor.ime_purpose()
        } else {
            drop(core);
            Widget::ime_purpose(&self.shell)
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let core = self.core.borrow();
        if let Some(modal) = &core.expand {
            modal.cursor_shape(point)
        } else if core.body_available() && core.body_editor.rect().contains(point) {
            core.body_editor.cursor_shape(point)
        } else {
            drop(core);
            Widget::cursor_shape(&self.shell, point)
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let core = self.core.borrow();
        if let Some(modal) = core.expand.as_ref().and_then(ExpandModal::accessibility) {
            return Some(modal);
        }
        let mut children: Vec<AccessNode> =
            Widget::accessibility(&self.shell).into_iter().collect();
        if let Some(open) = &core.menu {
            let items = open
                .actions
                .iter()
                .enumerate()
                .map(|(index, action)| {
                    AccessNode::new(Role::MenuItem)
                        .with_name(action.accessible_label(&core))
                        .with_action_target(format!("snippets.menu:{index}"))
                        .with_bounds(open.menu.item_rect(index).unwrap_or(Rect::ZERO))
                        .with_state(AccessState {
                            focusable: true,
                            ..Default::default()
                        })
                })
                .collect();
            children.push(
                AccessNode::new(Role::Menu)
                    .with_name(open.name.clone())
                    .with_children(items),
            );
        }
        Some(
            AccessNode::new(Role::Window)
                .with_name(<crate::resource::Snippets as ui::tilo_resource::Resource>::LABEL)
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
        let handled = {
            let (doc, obj) = ui::doc::document(state, path);
            let mut core = self.core.borrow_mut();
            if let Some(mut modal) = core.expand.take() {
                let Some(outcome) = modal.perform_semantic_action(target, action, doc, obj) else {
                    core.expand = Some(modal);
                    return false;
                };
                core.finish_expand(modal, outcome, doc);
                Some(true)
            } else if let Some(index) = target
                .strip_prefix("snippets.menu:")
                .and_then(|index| index.parse::<usize>().ok())
            {
                let chosen = core
                    .menu
                    .as_ref()
                    .and_then(|open| open.actions.get(index).cloned());
                match (chosen, action) {
                    (Some(chosen), ui::a11y::Action::Activate) => {
                        core.menu = None;
                        core.apply(chosen, doc);
                        Some(true)
                    }
                    (Some(_), _) => Some(true),
                    (None, _) => Some(false),
                }
            } else if target == "snippets.body" && core.open.is_some() {
                core.sync_body_editor();
                if matches!(action, ui::a11y::Action::Focus) {
                    core.body_editor.set_focused(true);
                    Some(true)
                } else {
                    let accepted = Widget::perform_action(&mut core.body_editor, action, doc, obj);
                    core.flush_body_edit(doc);
                    Some(accepted)
                }
            } else {
                let hit = core.hits().find_map(|(_, candidate)| {
                    (candidate.semantic_id() == target).then(|| candidate.clone())
                });
                hit.map(|hit| {
                    if matches!(action, ui::a11y::Action::Activate) {
                        core.apply(hit, doc);
                    }
                    matches!(action, ui::a11y::Action::Activate | ui::a11y::Action::Focus)
                })
            }
        };
        if let Some(handled) = handled {
            let (doc, _) = ui::doc::document(state, path);
            self.settle(doc);
            return handled;
        }
        Widget::perform_semantic_action(&mut self.shell, target, action, state, path)
    }

    fn wants_pointer_capture(&self) -> bool {
        let core = self.core.borrow();
        (core.body_available() && core.body_editor.wants_pointer_capture()) || {
            drop(core);
            Widget::wants_pointer_capture(&self.shell)
        }
    }

    fn wants_tab_input(&self) -> bool {
        let core = self.core.borrow();
        core.expand.is_some() || (core.body_available() && core.body_editor.is_focused())
    }

    fn text_review(&self) -> Option<(usize, String)> {
        let core = self.core.borrow();
        if core.body_available() && core.body_editor.is_focused() {
            core.body_editor.text_review()
        } else {
            drop(core);
            Widget::text_review(&self.shell)
        }
    }

    fn name(&self) -> &'static str {
        "SnippetsApp"
    }
}

impl SnippetsApp {
    fn paint_toast(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let core = self.core.borrow();
        if let Some(toast) = &core.toast {
            paint_transient(
                scene,
                transform,
                text,
                theme,
                core.window,
                ToastAnchor::BottomRight,
                &ToastCard::new(&toast.message, ToastKind::Info),
            );
        }
    }
}

impl ui::undo::UndoPoints for SnippetsApp {
    fn get_state(&self) -> Option<String> {
        let core = self.core.borrow();
        serde_json::to_string(&(core.open, core.body_editor.get_state())).ok()
    }

    fn set_state(&mut self, state: &str) {
        let mut core = self.core.borrow_mut();
        if let Ok((id, Some(caret))) =
            serde_json::from_str::<(Option<SnippetId>, Option<String>)>(state)
            && id == core.open
        {
            core.body_editor.set_state(&caret);
        }
    }
}

#[cfg(test)]
mod tests;
