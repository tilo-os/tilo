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

use automerge::{AutoCommit, ChangeHash, ObjId};
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

use crate::actions::{self, SnippetsAction, SnippetsResourceCtx};
use crate::detail_pane::{DetailPane, DetailState};
use crate::doc::SnippetsDoc;
use crate::expand::ExpandModal;
use crate::highlight;
use crate::list_pane::{ListPane, ListState};
use crate::model::{Language, Library, Selection, SmartFolder, Snippet, SnippetId};
use crate::seed;

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
    /// Show the language picker under the language chip.
    PickLanguage(SnippetId),
    /// Show the menu of the snippet's less frequent actions.
    More(SnippetId),
}

impl Action {
    pub(crate) fn semantic_id(&self) -> String {
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
            Self::ToggleFavorite(id) => format!("snippets.snippet:{id}.toggle-favorite"),
            Self::Duplicate(id) => format!("snippets.snippet:{id}.duplicate"),
            Self::Delete(id) => format!("snippets.snippet:{id}.delete"),
            Self::SetLanguage(id, _) => format!("snippets.snippet:{id}.set-language"),
            Self::PickLanguage(id) => format!("snippets.snippet:{id}.pick-language"),
            Self::More(id) => format!("snippets.snippet:{id}.more"),
        }
    }

    pub(crate) fn accessible_label(&self, core: &Core) -> String {
        match self {
            Self::Select(Selection::Smart(smart)) => smart.label().into(),
            Self::Select(Selection::Folder(folder)) => format!("Folder {folder}"),
            Self::Select(Selection::Tag(tag)) => format!("Tag {tag}"),
            Self::OpenSnippet(id) => core
                .library
                .get(*id)
                .map_or_else(|| "Open snippet".into(), |snippet| snippet.title.clone()),
            Self::NewSnippet => "New snippet".into(),
            Self::Copy(id) => core.copy_label(*id).into(),
            Self::ToggleFavorite(id) => core.favorite_label(*id).into(),
            Self::Duplicate(_) => "Duplicate".into(),
            Self::Delete(_) => "Delete".into(),
            Self::SetLanguage(_, language) => format!("Set language to {}", language.label()),
            Self::PickLanguage(id) => core.library.get(*id).map_or_else(
                || "Language".into(),
                |snippet| format!("Language: {}", snippet.language.label()),
            ),
            Self::More(_) => "More actions".into(),
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

    /// The accessibility role a painted control with this action has.
    pub(crate) fn role(&self) -> ui::a11y::Role {
        match self {
            Self::ToggleFavorite(_) => ui::a11y::Role::ToggleButton,
            _ => ui::a11y::Role::Button,
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

/// A short confirmation shown after an action (auto-dismisses).
#[derive(Clone)]
struct Toast {
    message: String,
    /// Frames remaining before the toast fades.
    ttl: u32,
}

/// What an open menu chooses between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuKind {
    Language(SnippetId),
    More(SnippetId),
}

/// The open menu: a language picker or the overflow menu.
pub(crate) struct OpenMenu {
    pub(crate) menu: ContextMenu,
    pub(crate) kind: MenuKind,
    /// What each item does, by index.
    pub(crate) actions: Vec<Action>,
}

/// The state of the window: the library, what is chosen and open, and the
/// painted panes' geometry.
pub(crate) struct Core {
    pub(crate) library: Library,
    /// Complete incremental search catalogue. Provider handles clone this
    /// shared index in O(1), so opening search never snapshots the library.
    search_provider: crate::search_provider::SnippetSearchProvider,
    pub(crate) selection: Selection,
    /// Editing cache only; every change is dispatched into `SnippetsDoc`.
    pub(crate) body_editor: CodeEditor,
    body_identity: Option<(SnippetId, Language)>,
    /// The snippet the content column shows.
    pub(crate) open: Option<SnippetId>,
    /// Ordered ids the list shows (the chosen place's snippets).
    pub(crate) visible: Vec<SnippetId>,
    /// The fill-in-variables dialog, when open.
    pub(crate) expand: Option<ExpandModal>,
    pub(crate) menu: Option<OpenMenu>,
    toast: Option<Toast>,
    /// Whether the local library has been mirrored into the shared doc.
    seeded: bool,
    /// Framework dispatcher when hosted; the local action host otherwise.
    actions: crate::resource::ActionSink,
    /// The pointer, in window coordinates.
    pub(crate) pointer: Point,
    /// The window, for dialogs and menus.
    pub(crate) window: Rect,
    pub(crate) list: ListState,
    pub(crate) detail: DetailState,
    /// The screen a narrow window should bring to the front next.
    show_pane: Option<NavPane>,
    /// The body editor may take input (the content column is on screen and
    /// nothing floats over it).
    pub(crate) body_reachable: bool,
}

impl Core {
    fn with_library(library: Library) -> Self {
        let search_provider = crate::search_provider::SnippetSearchProvider::new(&library);
        let mut core = Self {
            library,
            search_provider,
            selection: Selection::default(),
            body_editor: CodeEditor::local()
                .with_id("snippets.body")
                .with_placeholder("snippets-body-placeholder"),
            body_identity: None,
            open: None,
            visible: Vec::new(),
            expand: None,
            menu: None,
            toast: None,
            seeded: false,
            actions: crate::resource::ActionSink::default(),
            pointer: Point::ZERO,
            window: Rect::ZERO,
            list: ListState::default(),
            detail: DetailState::default(),
            show_pane: None,
            body_reachable: false,
        };
        core.recompute_visible();
        core
    }

    pub(crate) fn recompute_visible(&mut self) {
        self.visible = self.library.query(&self.selection);
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
            Selection::Smart(smart) => smart.label().to_string(),
            Selection::Folder(folder) | Selection::Tag(folder) => folder.clone(),
        }
    }

    /// The column title of the content: the open snippet's title.
    pub(crate) fn open_title(&self) -> String {
        self.open
            .and_then(|id| self.library.get(id))
            .map(|s| s.title.clone())
            .unwrap_or_default()
    }

    pub(crate) fn copy_label(&self, id: SnippetId) -> &'static str {
        if self.library.get(id).is_some_and(Snippet::has_placeholders) {
            "Fill in and copy"
        } else {
            "Copy"
        }
    }

    pub(crate) fn favorite_label(&self, id: SnippetId) -> &'static str {
        if self.library.get(id).is_some_and(|s| s.favorite) {
            "Remove from favorites"
        } else {
            "Add to favorites"
        }
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
                .count(count(self.library.query(&selection).len()))
        })
        .collect();
        let mut sections = vec![NavSection::new("library", "Library", smart)];
        let folders: Vec<NavItem> = self
            .library
            .folders()
            .into_iter()
            .map(|folder| {
                let selection = Selection::Folder(folder.clone());
                NavItem::new(nav_id(&selection), folder)
                    .icon(Icon::Folder)
                    .count(count(self.library.query(&selection).len()))
            })
            .collect();
        if !folders.is_empty() {
            sections.push(NavSection::new("folders", "Folders", folders));
        }
        let tags: Vec<NavItem> = self
            .library
            .tags()
            .into_iter()
            .map(|(tag, n)| NavItem::new(nav_id(&Selection::Tag(tag.clone())), tag).count(count(n)))
            .collect();
        if !tags.is_empty() {
            sections.push(NavSection::new("tags", "Tags", tags));
        }
        sections
    }

    /// Dispatch one [`SnippetsAction`] — the single mutation path shared with
    /// the palette. The shared document is authoritative; the in-memory
    /// library is re-projected from it so the panes reflect the change this
    /// frame.
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &SnippetsAction) {
        self.actions.dispatch_with_context(
            doc,
            actions::SNIPPETS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.snippets", "Snippets", action),
        );
        self.refresh_from_doc(doc);
    }

    /// Show the library the framework resource already holds. An empty
    /// projection does not wipe snippets that have not been published yet.
    fn adopt_resource_state(&mut self, state: &[Snippet]) {
        if state.is_empty() {
            return;
        }
        let next = Library {
            snippets: state.to_vec(),
        };
        if self.library.snippets != next.snippets {
            self.search_provider.update(&self.library, &next);
            self.library = next;
            self.recompute_visible();
            self.sync_body_editor();
        }
        self.seeded = true;
    }

    fn refresh_from_doc(&mut self, doc: &AutoCommit) {
        let next = Library {
            snippets: SnippetsDoc::list(doc),
        };
        self.search_provider.update(&self.library, &next);
        self.library = next;
        self.sync_body_editor();
    }

    pub(crate) fn sync_body_editor(&mut self) {
        let snippet = self.open.and_then(|id| self.library.get(id));
        let identity = snippet.map(|s| (s.id, s.language));
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

    fn flush_body_edit(&mut self, doc: &mut AutoCommit) {
        if let Some(text) = self.body_editor.take_dirty()
            && let Some(id) = self.open
            && self
                .library
                .get(id)
                .is_some_and(|snippet| snippet.body != text)
        {
            self.perform_action(doc, &actions::set_body_action(id, &text));
        }
    }

    fn body_available(&self) -> bool {
        self.open.is_some()
            && self.expand.is_none()
            && self.menu.is_none()
            && self.body_reachable
            && self.body_editor.rect().area() > 0.0
    }

    /// Hand `event` to the body editor when it is the editor's: a press on
    /// it, the pointer while it drags, keys while it has focus.
    fn route_body_event(
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
                    && matches!(&event.logical_key, Key::Character(key) if key.eq_ignore_ascii_case("n")));
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
        self.toast("New snippet created");
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
            self.toast("Copied to clipboard");
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
            copy.title = format!("{} (copy)", src.title);
            copy.favorite = false;
            copy.uses = 0;
            let new_id = copy.id;
            self.perform_action(doc, &actions::create_snippet_action(copy));
            self.recompute_visible();
            if self.visible.contains(&new_id) {
                self.open_snippet(new_id);
            }
            self.toast("Snippet duplicated");
        }
    }

    fn delete(&mut self, id: SnippetId, doc: &mut AutoCommit) {
        self.perform_action(doc, &actions::delete_snippet_action(id));
        if self.open == Some(id) {
            self.open = None;
        }
        self.recompute_visible();
        self.toast("Snippet deleted");
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
    /// dispatched [`SnippetsAction`]. On first sync we either seed our
    /// starter set into an empty document or adopt a peer's; from then on we
    /// re-project the document so a peer's change — and our own
    /// just-dispatched action — both surface.
    fn sync_library(&mut self, doc: &mut AutoCommit) {
        let remote = SnippetsDoc::list(doc);
        if !self.seeded {
            if remote.is_empty() && !self.actions.is_hosted() {
                // A screenshot fixture may arrive with a local starter set.
                // The hosted app never writes that around the resource action.
                for s in self.library.snippets.clone() {
                    self.perform_action(doc, &SnippetsAction::CreateSnippet { snippet: s });
                }
                // Seeding is not a user undo bracket.
                let _ = self.actions.take_committed();
            } else {
                // A peer is already sharing — adopt their library.
                let next = Library { snippets: remote };
                self.search_provider.update(&self.library, &next);
                self.library = next;
                self.recompute_visible();
            }
            self.seeded = true;
            return;
        }
        // Steady state: the document is authoritative — re-project it.
        if self.library.snippets != remote {
            let next = Library { snippets: remote };
            self.search_provider.update(&self.library, &next);
            self.library = next;
            self.recompute_visible();
        }
    }

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
            Action::PickLanguage(id) => self.open_menu(MenuKind::Language(id)),
            Action::More(id) => self.open_menu(MenuKind::More(id)),
        }
    }

    /// Open the language picker or the overflow menu under the control that
    /// asked for it.
    fn open_menu(&mut self, kind: MenuKind) {
        let (anchor_action, id) = match kind {
            MenuKind::Language(id) => (Action::PickLanguage(id), id),
            MenuKind::More(id) => (Action::More(id), id),
        };
        let Some(anchor) = self
            .detail
            .hits
            .iter()
            .find(|(_, action)| *action == anchor_action)
            .map(|(rect, _)| *rect)
        else {
            return;
        };
        let mut menu = ContextMenu::new();
        let mut actions = Vec::new();
        match kind {
            MenuKind::Language(_) => {
                let current = self.library.get(id).map(|s| s.language);
                for language in Language::ALL {
                    menu.push(
                        MenuItem::action(language.label(), || {})
                            .with_checked(Some(language) == current),
                    );
                    actions.push(Action::SetLanguage(id, language));
                }
            }
            MenuKind::More(_) => {
                menu.push(MenuItem::action("Duplicate", || {}).with_icon(Icon::Copy));
                actions.push(Action::Duplicate(id));
                menu.push(MenuItem::destructive_action("Delete", || {}).with_icon(Icon::Trash2));
                actions.push(Action::Delete(id));
            }
        }
        menu.open_anchored(anchor, self.window);
        self.body_editor.set_focused(false);
        self.menu = Some(OpenMenu {
            menu,
            kind,
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
                self.toast("Copied to clipboard");
            }
        }
    }

    /// Every painted control on screen, list first.
    pub(crate) fn hits(&self) -> impl Iterator<Item = &(Rect, Action)> {
        self.list.hits.iter().chain(self.detail.hits.iter())
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

    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
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
                // Reveal it in a place that holds it.
                if !self.visible.contains(&id) {
                    self.select(Selection::default());
                }
                self.open_snippet(id);
            }
            actions::SnippetsOperation::RevealTag { tag } => {
                if !self.library.tags().iter().any(|(known, _)| known == &tag) {
                    return Err("tag no longer exists".into());
                }
                self.select(Selection::Tag(tag));
                self.show_pane = Some(NavPane::Overview);
            }
        }
        Ok(true)
    }

    fn update_toast(&mut self) {
        if let Some(toast) = &mut self.toast {
            if toast.ttl == 0 {
                self.toast = None;
            } else {
                toast.ttl -= 1;
            }
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
            .with_accessible_name("Snippet library")
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
    /// A screenshot scenario to stage once the shell has laid out.
    pending_scenario: Option<String>,
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

    /// Like [`Self::new`] but pre-seeded with the starter library and an
    /// opened specimen — used by `--shots` so a capture shows the manager
    /// doing real work. Never the live first-run path.
    pub fn new_demo() -> Self {
        let mut library = Library::new();
        for s in seed::starter_library() {
            library.insert(s);
        }
        let app = Self::with_library(library);
        {
            let mut core = app.core.borrow_mut();
            // Open a substantial, templated piece of code so the content
            // column shows the highlighter and the variables action.
            core.open = core
                .visible
                .iter()
                .copied()
                .max_by_key(|id| {
                    let s = core.library.get(*id);
                    let lines = s.map_or(0, |s| s.body.lines().count());
                    let templated = usize::from(s.is_some_and(Snippet::has_placeholders));
                    let is_program = usize::from(s.is_some_and(|s| {
                        !matches!(s.language, Language::Markdown | Language::PlainText)
                    }));
                    (is_program, templated, lines)
                })
                .or_else(|| core.visible.first().copied());
            core.show_pane = None;
        }
        app
    }

    fn with_library(library: Library) -> Self {
        let core: Shared = Rc::new(RefCell::new(Core::with_library(library)));
        let requests: Requests = Rc::default();
        let new_snippet = {
            let requests = Rc::clone(&requests);
            Button::icon_only("snippets.new", Icon::Plus, "New snippet")
                .on_effect(move || requests.borrow_mut().push(Action::NewSnippet))
        };
        let search = ui::search::search_button(ui::search::SearchRequest::this_app());
        let titles = {
            let core = core.borrow();
            (core.selection_title(), core.open_title())
        };
        let shell = NavShell::new()
            .with_id("snippets.shell")
            .sidebar(Box::new(SidebarNav::new(&core, &requests)))
            .overview(Box::new(ListPane::new(&core)))
            .content(Box::new(DetailPane::new(&core)))
            .overview_header(
                titles.0.clone(),
                vec![Box::new(search) as Element, Box::new(new_snippet)],
            )
            .content_header(titles.1.clone(), Vec::new());
        Self {
            core,
            requests,
            shell,
            titles,
            pending_scenario: None,
            settled_layouts: 0,
        }
    }

    /// Seeded constructor for the screenshot harness.
    pub fn for_shot() -> Self {
        Self::new_demo()
    }

    /// Stage a screenshot scenario once the window has its size:
    ///
    /// * `"narrow"` — the sidebar drawer open over the content.
    /// * `"detail"` — a narrow window showing the open snippet.
    /// * `"expand"` — the fill-in-variables dialog over the open snippet.
    /// * `"language"`, `"more"` — the language picker or the overflow menu.
    /// * anything else — the default frame.
    pub fn seed_shot_scenario(&mut self, scenario: &str) {
        match scenario {
            "narrow" | "detail" | "language" | "more" => {
                self.pending_scenario = Some(scenario.to_string());
            }
            "expand" => {
                let mut core = self.core.borrow_mut();
                if let Some(id) = core.open {
                    core.start_expand(id);
                }
            }
            _ => {}
        }
    }

    fn stage_pending_scenario(&mut self) {
        let Some(scenario) = self.pending_scenario.take() else {
            return;
        };
        match scenario.as_str() {
            "narrow" => self.shell.open_sidebar(),
            "detail" => self.shell.show(NavPane::Content),
            "language" | "more" => {
                let mut core = self.core.borrow_mut();
                // A menu opens under its control: wait for the first paint.
                let Some(id) = core.open.filter(|_| !core.detail.hits.is_empty()) else {
                    drop(core);
                    self.pending_scenario = Some(scenario);
                    return;
                };
                core.open_menu(if scenario == "language" {
                    MenuKind::Language(id)
                } else {
                    MenuKind::More(id)
                });
            }
            _ => {}
        }
        self.core.borrow_mut().show_pane = None;
        self.shell.snap();
    }

    /// Apply what retained controls asked for, bring the screen a choice
    /// asked for to the front, and keep the column titles current.
    fn settle(&mut self, doc: &mut AutoCommit) {
        let requests = std::mem::take(&mut *self.requests.borrow_mut());
        for action in requests {
            self.core.borrow_mut().apply(action, doc);
        }
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

    /// Adopt the shared library projection only — never wipe a local
    /// library that has not been published yet.
    pub(crate) fn adopt_resource_state(&self, state: &[Snippet]) {
        self.core.borrow_mut().adopt_resource_state(state);
    }

    /// Attach the framework dispatcher when hosted as a resource surface.
    pub(crate) fn attach_framework(
        &self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Snippets>,
    ) {
        let mut core = self.core.borrow_mut();
        core.actions.attach(dispatcher);
        core.seeded = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.core.borrow().actions.is_hosted()
    }

    pub(crate) fn snippets_resource_ctx(&self) -> SnippetsResourceCtx {
        self.core.borrow().snippets_resource_ctx()
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&self) {
        self.core.borrow_mut().actions.mark_hosted_for_test();
    }

    /// Test-only: select a sidebar folder so hosted extras bind folder-scoped create.
    #[cfg(test)]
    pub(crate) fn select_folder_for_test(&self, folder: &str) {
        self.core.borrow_mut().selection = Selection::Folder(folder.to_string());
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn snippet_count(&self) -> usize {
        self.core.borrow().library.snippets.len()
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
    pub fn last_dispatched_for_test(&self) -> Option<tilo_actions::EncodedAction> {
        self.core.borrow().actions.last_dispatched().cloned()
    }

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
            self.settle(&mut AutoCommit::new());
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

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::history_mode::capability::DocumentHistory>() {
            request.provide::<dyn ui::history_mode::capability::DocumentHistory>(self);
        } else if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
        } else if request.wants::<dyn ui::search::capability::SearchSource>() {
            request.provide::<dyn ui::search::capability::SearchSource>(self);
        } else if request.wants::<dyn ui::undo::UndoPoints>() {
            request.provide::<dyn ui::undo::UndoPoints>(self);
        } else {
            Widget::provide(&self.shell, request);
        }
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::undo::CommittedActions>() {
            request.provide::<dyn ui::undo::CommittedActions>(self);
        } else if request.wants::<dyn ui::history_mode::capability::DocumentHistory>() {
            request.provide::<dyn ui::history_mode::capability::DocumentHistory>(self);
        } else if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
        } else if request.wants::<dyn ui::search::capability::SearchSource>() {
            request.provide::<dyn ui::search::capability::SearchSource>(self);
        } else if request.wants::<dyn ui::undo::UndoPoints>() {
            request.provide::<dyn ui::undo::UndoPoints>(self);
        } else {
            Widget::provide_mut(&mut self.shell, request);
        }
    }

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
        let answered = self.core.borrow_mut().answer_resource_operation(
            request.kind,
            request.type_id,
            request.payload,
        )?;
        self.settle(doc);
        Ok(answered.then(|| {
            ui::widget::ResourceOperationStart::Completed(ui::tilo_actions::ActionOutput::default())
        }))
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            {
                let mut core = self.core.borrow_mut();
                core.sync_library(doc);
                core.sync_body_editor();
            }
            self.settle(doc);
        }
        Widget::sync(&mut self.shell, state, path);
    }

    fn needs_idle_sync(&self) -> bool {
        !self.core.borrow().seeded || Widget::needs_idle_sync(&self.shell)
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
            } else if core.route_body_event(event, ctx) {
                true
            } else if let WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } = event
            {
                core.handle_keyboard_input(ctx, logical_key)
            } else {
                false
            }
        };
        if consumed {
            *ctx.handled = true;
        } else {
            Widget::handle_event(&mut self.shell, event, ctx);
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
                    .with_name(match open.kind {
                        MenuKind::Language(_) => "Language",
                        MenuKind::More(_) => "More actions",
                    })
                    .with_children(items),
            );
        }
        if core.body_available()
            && let Some(node) = core.body_editor.accessibility()
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

    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.core.borrow_mut().actions.take_committed()
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
    fn needs_commit(&self) -> bool {
        let core = self.core.borrow();
        ui::undo::needs_commit(&core.body_editor)
            || core.expand.as_ref().is_some_and(ExpandModal::needs_commit)
    }

    fn on_commit(&mut self) {
        let mut core = self.core.borrow_mut();
        ui::undo::on_commit(&mut core.body_editor);
        if let Some(modal) = &mut core.expand {
            modal.on_commit();
        }
    }

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

impl ui::search::capability::SearchSource for SnippetsApp {
    /// Fold the library into the platform search. The provider is an O(1)
    /// handle to the retained incremental catalogue, so every snippet is
    /// findable by title, language, folder, tag, description or body, and
    /// `language:` and `tag:` narrow the results.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.core.borrow().search_provider.clone())]
    }
}

impl SnippetsApp {
    fn find_published(
        resource: &tilo_actions::Resource,
        id: &str,
    ) -> Option<tilo_actions::Resource> {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        for child in &resource.children {
            if let Some(found) = Self::find_published(child, id) {
                return Some(found);
            }
        }
        None
    }

    pub(crate) fn published_library(&self, doc: &AutoCommit) -> tilo_actions::Resource {
        let state = <crate::resource::Snippets as ui::tilo_resource::Resource>::read(doc)
            .unwrap_or_default();
        let mut root = ui::tilo_resource::publish::<crate::resource::Snippets>(
            crate::resource::LIBRARY_RESOURCE_ID,
            "Snippets",
            &state,
        );
        root.children.extend(crate::actions::live_folder_resources(
            &self.snippets_resource_ctx(),
        ));
        root
    }
}

impl ui::resources::PublishesResources for SnippetsApp {
    /// Standalone / `--shots` / unit tests publish the library root. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// that door — keep only the sidebar-folder create extras.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::actions::live_folder_resources(&self.snippets_resource_ctx());
        }
        vec![self.published_library(doc)]
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.is_hosted() && id == "snippets.library" {
            return None;
        }
        if let Some(found) = Self::find_published(&self.published_library(doc), id) {
            return Some(found);
        }
        if self.is_hosted()
            && let Some(found) =
                crate::actions::live_folder_resources(&self.snippets_resource_ctx())
                    .into_iter()
                    .find(|resource| resource.id.as_str() == id)
        {
            return Some(found);
        }
        let core = self.core.borrow();
        if let Some(raw_id) = id.strip_prefix("snippets.snippet:") {
            let snippet_id = uuid::Uuid::parse_str(raw_id).ok()?;
            return core
                .library
                .get(snippet_id)
                .map(crate::actions::provide_snippet_resource);
        }
        core.library
            .tags()
            .into_iter()
            .find(|(tag, _)| crate::actions::tag_resource_id(tag) == id)
            .map(|(tag, _)| crate::actions::provide_tag_resource(tag))
    }
}

impl ui::history_mode::capability::DocumentHistory for SnippetsApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl ui::undo::CommittedActions for SnippetsApp {
    /// Hand the host the heads captured before an on-screen control
    /// dispatched its action, so it records one undo boundary for it.
    /// Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<ChangeHash>> {
        self.core.borrow_mut().actions.take_committed()
    }
}

#[cfg(test)]
mod tests;
