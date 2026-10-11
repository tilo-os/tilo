//! Root widget for the snippet manager.
//!
//! The window is the shared [`NavShell`]: the sidebar lists the library's
//! places (smart folders, folders, tags) in a [`NavList`], the overview is
//! the snippet list of the chosen place, and the content is the open
//! snippet. The shell owns the header row, the sidebar toggle, the widths,
//! the drawer and the push stack; this module owns the state they show.
//!
//! The library is the [`Snippets`] record the framework presents. Every
//! change is submitted through its dispatcher as a typed value: a field edit
//! at a path (star, language, the body's text) or one of the record's verbs
//! (new, duplicate, delete, record a use). Selection, the open snippet and
//! the variables dialog are view state and stay local to each peer.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use ui::framework::TextControl;
use ui::framework::{Dispatcher, Submit};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_resource::Resource;
use ui::widget::code_editor::CodeEditor;
use ui::widget::toast::ToastKind;
use ui::widget::{
    AppModel, Button, ContextMenu, Element, MenuItem, ModelPath, NavItem, NavList, NavPane,
    NavSection, NavShell, SidebarPlacement, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::detail_pane::{self, DetailKey};
use crate::expand::ExpandModal;
use crate::highlight;
use crate::list_pane::{ListPane, ListState};
use crate::model::{Language, Selection, SmartFolder, Snippet, SnippetId, SnippetPaths, Snippets};
use crate::resource::{SnippetsView, snippets};
use crate::{tr, tr_with};

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
                format!("snippets.sidebar.folder:{}", stable_key(folder))
            }
            Self::Select(Selection::Tag(tag)) => {
                format!("snippets.sidebar.tag:{}", stable_key(tag))
            }
            Self::OpenSnippet(id) => format!("snippets.snippet:{id}.open"),
            Self::NewSnippet => "snippets.library.new-snippet".into(),
            Self::Copy(id) => format!("snippets.snippet:{id}.copy"),
            Self::ToggleFavorite(id) => format!("snippets.snippet:{id}.toggle-favorite"),
            Self::Duplicate(id) => format!("snippets.snippet:{id}.duplicate"),
            Self::Delete(id) => format!("snippets.snippet:{id}.delete"),
            Self::SetLanguage(id, _) => format!("snippets.snippet:{id}.set-language"),
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
                .get(id)
                .map_or_else(|| tr("snippets-untitled"), |snippet| snippet.title.clone()),
            Self::NewSnippet => tr("snippets-new"),
            Self::Copy(id) => core.copy_label(id),
            Self::ToggleFavorite(id) => core.favorite_label(id),
            Self::Duplicate(_) => tr("snippets-menu-duplicate"),
            Self::Delete(_) => tr("snippets-menu-delete"),
            Self::SetLanguage(_, language) => {
                tr_with("snippets-set-language", &[("language", &language.name())])
            }
            Self::More(_) => tr("snippets-more-actions"),
        }
    }

    /// The control binding a painted control with this action publishes:
    /// its semantic target, which the list answers as view state.
    pub(crate) fn binding(&self) -> ui::widget::ControlBindingMetadata {
        let control_id = self.semantic_id();
        ui::widget::ControlBindingMetadata {
            kind: ui::widget::ControlBindingKind::SemanticAction(control_id.clone()),
            control_id,
        }
    }
}

/// A punctuation-safe key of a folder or tag name for semantic control ids.
pub(crate) fn stable_key(value: &str) -> String {
    hex::encode(value)
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
    pub(crate) library: Rc<Snippets>,
    pub(crate) selection: Selection,
    /// The open snippet's body, bound to its path: it shows the stored text
    /// and every keystroke is a splice there, a word one undo step.
    pub(crate) body: TextControl<CodeEditor, Snippets>,
    body_identity: Option<(SnippetId, Language)>,
    /// The snippet the content column shows.
    pub(crate) open: Option<SnippetId>,
    /// This view created or duplicated a snippet: the one its submission
    /// minted opens once it lands.
    pub(crate) open_minted: bool,
    /// Ordered ids the list shows (the chosen place's snippets).
    pub(crate) visible: Vec<SnippetId>,
    /// The fill-in-variables dialog, when open.
    pub(crate) expand: Option<ExpandModal>,
    pub(crate) menu: Option<OpenMenu>,
    /// A short confirmation of the last action, for the shell to show
    /// next.
    notice: Option<String>,
    /// The resource's dispatcher, once the framework attaches the view.
    /// Each on-screen control submits a field edit or a verb through it.
    dispatch: Option<Dispatcher<Snippets>>,
    /// The pointer, in window coordinates.
    pub(crate) pointer: Point,
    /// The window, for dialogs and menus.
    pub(crate) window: Rect,
    pub(crate) list: ListState,
    /// Where the header's overflow button is, for its menu.
    pub(crate) more_anchor: Rect,
    /// The language picker should open (taken by the picker).
    pub(crate) pick_language: bool,
    /// The screen a narrow window should bring to the front next.
    show_pane: Option<NavPane>,
    /// The body editor may take input (the content column is on screen and
    /// nothing floats over it).
    pub(crate) body_reachable: bool,
}

impl Core {
    fn new() -> Self {
        let mut core = Self {
            library: Rc::default(),
            selection: Selection::default(),
            body: Self::body_control(None, None),
            body_identity: None,
            open: None,
            open_minted: false,
            visible: Vec::new(),
            expand: None,
            menu: None,
            notice: None,
            dispatch: None,
            pointer: Point::ZERO,
            window: Rect::ZERO,
            list: ListState::default(),
            more_anchor: Rect::ZERO,
            pick_language: false,
            show_pane: None,
            body_reachable: false,
        };
        core.recompute_visible();
        core
    }

    pub(crate) fn recompute_visible(&mut self) {
        self.visible = self.library.query(&self.selection);
        // Keep the open snippet valid; otherwise open the first one.
        if self
            .open
            .as_ref()
            .is_none_or(|open| !self.visible.contains(open))
        {
            self.open = self.visible.first().cloned();
        }
    }

    fn select(&mut self, sel: Selection) {
        if self.selection == sel {
            return;
        }
        self.selection = sel;
        self.list.scroller.set_offset(Vec2::ZERO);
        self.recompute_visible();
    }

    pub(crate) fn open_snippet(&mut self, id: SnippetId) {
        if self.library.get(&id).is_none() {
            return;
        }
        // A snippet the chosen place does not list is shown among all.
        if !self.visible.contains(&id) {
            self.select(Selection::default());
        }
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
            .max_by_key(|id| {
                let s = self.library.get(id);
                let lines = s.map_or(0, |s| s.body.lines().count());
                let templated = usize::from(s.is_some_and(Snippet::has_placeholders));
                let is_program = usize::from(s.is_some_and(|s| {
                    !matches!(s.language, Language::Markdown | Language::PlainText)
                }));
                (is_program, templated, lines)
            })
            .or_else(|| self.visible.first())
            .cloned();
        self.list.reveal = true;
    }

    /// The column title of the content: the open snippet's title.
    pub(crate) fn open_title(&self) -> String {
        self.open
            .as_ref()
            .and_then(|id| self.library.get(id))
            .map(|s| s.title.clone())
            .unwrap_or_default()
    }

    pub(crate) fn copy_label(&self, id: &SnippetId) -> String {
        tr(
            if self.library.get(id).is_some_and(Snippet::has_placeholders) {
                "snippets-fill-and-copy"
            } else {
                "snippets-copy"
            },
        )
    }

    pub(crate) fn favorite_label(&self, id: &SnippetId) -> String {
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
                .count(count(self.library.query(&selection).len()))
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
                    .count(count(self.library.query(&selection).len()))
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

    /// Submit a field edit or a verb through the resource's dispatcher:
    /// the host checks the participant's permission, applies it as one undo
    /// step, records it in history, and presents the changed library.
    fn submit(&self, submit: impl Submit<Snippets>) {
        if let Some(dispatch) = &self.dispatch {
            dispatch.submit(submit);
        }
    }

    /// The paths of snippet `id`'s fields.
    fn paths(id: &SnippetId) -> SnippetPaths<Snippets> {
        Snippets::paths().snippets.item(id)
    }

    /// Show the library the resource holds: a local edit, a collaborator's,
    /// an undo.
    pub(crate) fn show_library(&mut self, state: &Rc<Snippets>) {
        self.library = Rc::clone(state);
        self.recompute_visible();
        self.open_landed();
        self.sync_body_editor(None);
    }

    /// Open the snippet this view's own "new" or "duplicate" minted, once
    /// the submission landed and the library presents it.
    pub(crate) fn open_landed(&mut self) {
        if !self.open_minted {
            return;
        }
        let Some(landed) = self.dispatch.as_ref().and_then(|d| d.landed().get()) else {
            return;
        };
        let Some(made) = landed.minted().into_iter().next() else {
            self.open_minted = false;
            return;
        };
        if self.library.get(&made).is_none() {
            return;
        }
        self.open_minted = false;
        self.open = Some(made);
        self.recompute_visible();
    }

    /// Edit through the resource's dispatcher from now on.
    fn attach_dispatcher(&mut self, dispatcher: Dispatcher<Snippets>) {
        self.dispatch = Some(dispatcher);
        self.body_identity = None;
        self.sync_body_editor(None);
    }

    /// The body editor over snippet `open`'s body, through `dispatch` once
    /// the view has one.
    fn body_control(
        dispatch: Option<Dispatcher<Snippets>>,
        open: Option<&SnippetId>,
    ) -> TextControl<CodeEditor, Snippets> {
        let editor = CodeEditor::local()
            .with_id("snippets.body")
            .with_placeholder("snippets-body-placeholder");
        let open = open.cloned().unwrap_or_else(|| SnippetId::from_raw(""));
        let mut body = TextControl::view_only_at(editor, Self::paths(&open).body);
        if let Some(dispatch) = dispatch {
            body.attach(dispatch);
        }
        body
    }

    /// Follow the open snippet: its body, its language's highlighter, and
    /// the stored text when the document is at hand.
    pub(crate) fn sync_body_editor(&mut self, doc: Option<&ui::tilo_resource::Doc>) {
        let identity = self.open.as_ref().and_then(|id| {
            self.library
                .get(id)
                .map(|snippet| (id.clone(), snippet.language))
        });
        let same_snippet =
            self.body_identity.as_ref().map(|(id, _)| id) == identity.as_ref().map(|(id, _)| id);
        if !same_snippet {
            self.body =
                Self::body_control(self.dispatch.clone(), identity.as_ref().map(|(id, _)| id));
        }
        if !same_snippet || self.body_identity != identity {
            self.body
                .control_mut()
                .set_highlighter(identity.as_ref().map(|(_, language)| {
                    Box::new(highlight::EditorHighlighter::new(*language))
                        as Box<dyn ui::widget::code_editor::Highlighter>
                }));
        }
        self.body_identity = identity;
        if let Some(doc) = doc {
            self.body.pull(doc);
        }
    }

    fn body_available(&self) -> bool {
        self.open.is_some()
            && self.expand.is_none()
            && self.menu.is_none()
            && self.body_reachable
            && self.body.control().rect().area() > 0.0
    }

    /// Hand `event` to the body editor when it is the editor's: a press on
    /// it, the pointer while it drags, keys while it has focus.
    fn route_body_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        if !self.body_available() {
            self.body.control_mut().set_focused(false);
            return false;
        }
        let modifiers = *ctx.modifiers;
        let inside = self.body.control().rect().contains(self.pointer);
        let captured = self.body.control().wants_pointer_capture();
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            self.body.control_mut().set_focused(inside);
        }
        let focused = self.body.control().is_focused();
        if let WindowEvent::KeyboardInput { event, .. } = event {
            // Escape and the app's own shortcuts leave the editor.
            let global = matches!(&event.logical_key, Key::Named(NamedKey::Escape))
                || ((modifiers.state().control_key() || modifiers.state().super_key())
                    && matches!(&event.logical_key, Key::Character(key) if key.eq_ignore_ascii_case("n")));
            if global {
                self.body.control_mut().set_focused(false);
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
        Widget::handle_event(
            &mut self.body,
            event,
            &mut ui::widget::EventCtx {
                handled: &mut handled,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        // Pointer motion stays everyone's; the editor only claims what it
        // consumed.
        handled && !matches!(event, WindowEvent::CursorMoved { .. })
    }

    fn new_snippet(&mut self) {
        let folder = match &self.selection {
            Selection::Folder(f) => Some(f.clone()),
            _ => None,
        };
        // A new snippet is neither a favorite nor used yet: show it in a
        // place that holds it.
        if matches!(
            self.selection,
            Selection::Smart(SmartFolder::Favorites | SmartFolder::MostUsed) | Selection::Tag(_)
        ) {
            self.select(Selection::default());
        }
        self.open_when_presented();
        self.submit(Snippets::NEW_SNIPPET.with(snippets::NewSnippet {
            title: tr("snippets-untitled"),
            folder,
        }));
        self.toast(&tr("snippets-created"));
    }

    /// Open the snippet this view is about to create once it is presented,
    /// in front on a narrow window.
    fn open_when_presented(&mut self) {
        self.open_minted = true;
        if let Some(dispatch) = &self.dispatch {
            dispatch.landed().set(None);
        }
        self.list.reveal = true;
        self.show_pane = Some(NavPane::Content);
    }

    fn copy(&mut self, id: &SnippetId) {
        // A body with placeholders goes through the fill-in dialog so raw
        // `${...}` never lands on the clipboard.
        let needs_expand = self.library.get(id).is_some_and(Snippet::has_placeholders);
        if needs_expand {
            self.start_expand(id);
            return;
        }
        // Copying is presentation; the use it records is a verb, so it
        // replicates and undoes like any change.
        if let Some(body) = self.library.get(id).map(|s| s.body.clone()) {
            ui::clipboard_copy(&body);
            self.record_use(id);
            self.toast(&tr("snippets-copied"));
        }
    }

    /// Record one use of a snippet (drives "Most used").
    fn record_use(&self, id: &SnippetId) {
        self.submit(Snippets::RECORD_USE.with(id.clone()));
    }

    pub(crate) fn start_expand(&mut self, id: &SnippetId) {
        if let Some(s) = self.library.get(id) {
            self.body.control_mut().set_focused(false);
            self.expand = Some(ExpandModal::new(id, s));
        }
    }

    fn toggle_favorite(&self, id: &SnippetId) {
        let favorite = self.library.get(id).is_some_and(|s| s.favorite);
        self.submit(Self::paths(id).favorite.set(!favorite));
    }

    fn duplicate(&mut self, id: &SnippetId) {
        let Some(title) = self.library.get(id).map(|s| s.title.clone()) else {
            return;
        };
        // The copy is no favorite, so Favorites does not list it: there
        // the open snippet stays.
        if self.selection != Selection::Smart(SmartFolder::Favorites) {
            self.open_when_presented();
        }
        self.submit(Snippets::DUPLICATE.with(snippets::Duplicate {
            snippet: id.clone(),
            title: Some(tr_with("snippets-copy-title", &[("title", &title)])),
        }));
        self.toast(&tr("snippets-duplicated"));
    }

    fn delete(&mut self, id: &SnippetId) {
        self.submit(Snippets::DELETE.with(id.clone()));
        self.toast(&tr("snippets-deleted"));
    }

    fn set_language(&self, id: &SnippetId, language: Language) {
        self.submit(Self::paths(id).language.set(language));
    }

    fn toast(&mut self, message: &str) {
        self.notice = Some(message.to_string());
    }

    pub(crate) fn apply(&mut self, action: Action) {
        match action {
            Action::Select(sel) => self.select(sel),
            Action::OpenSnippet(id) => self.open_snippet(id),
            Action::NewSnippet => self.new_snippet(),
            Action::Copy(id) => self.copy(&id),
            Action::ToggleFavorite(id) => self.toggle_favorite(&id),
            Action::Duplicate(id) => self.duplicate(&id),
            Action::Delete(id) => self.delete(&id),
            Action::SetLanguage(id, lang) => self.set_language(&id, lang),
            Action::More(id) => self.open_more(id),
        }
    }

    /// Open the open snippet's language picker.
    pub(crate) fn open_languages(&mut self, id: &SnippetId) {
        if self.open.as_ref() == Some(id) {
            self.pick_language = true;
        }
    }

    /// Open the snippet's overflow menu under the header's overflow button.
    pub(crate) fn open_more(&mut self, id: SnippetId) {
        if self.library.get(&id).is_none() {
            return;
        }
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action(tr("snippets-menu-duplicate"), || {}).with_icon(Icon::Copy));
        menu.push(
            MenuItem::destructive_action(tr("snippets-menu-delete"), || {}).with_icon(Icon::Trash2),
        );
        self.open_menu(
            menu,
            self.more_anchor,
            tr("snippets-more-actions"),
            vec![Action::Duplicate(id.clone()), Action::Delete(id)],
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
        self.body.control_mut().set_focused(false);
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
            self.apply(action);
        }
    }

    fn finish_expand(&mut self, modal: ExpandModal, outcome: ExpandOutcome) {
        match outcome {
            ExpandOutcome::Stay => {
                self.expand = Some(modal);
            }
            ExpandOutcome::Cancel => {}
            ExpandOutcome::Copy(text, id) => {
                ui::clipboard_copy(&text);
                self.record_use(&id);
                self.toast(&tr("snippets-copied"));
            }
        }
    }

    /// Every painted row on screen.
    pub(crate) fn hits(&self) -> impl Iterator<Item = &(Rect, Action)> {
        self.list.hits.iter()
    }

    fn handle_keyboard_input(&mut self, ctx: &ui::widget::EventCtx<'_>, key: &Key) -> bool {
        let mods = ctx.modifiers.state();
        if !(mods.control_key() || mods.super_key()) {
            return false;
        }
        let Key::Character(ch) = key else {
            return false;
        };
        match ch.to_lowercase().as_str() {
            "n" => {
                self.new_snippet();
                true
            }
            "c" if !self.body.control().is_focused() => {
                if let Some(id) = self.open.clone() {
                    self.copy(&id);
                }
                true
            }
            _ => false,
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

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.refresh();
        Widget::sync(&mut self.list, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
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
    search: ui::search::SearchOpener,
    /// The column titles last handed to the shell.
    titles: (String, String),
    /// What the content column was last built for.
    detail: DetailKey,
    /// Whether the overview column is in the shell (not for an empty
    /// library, whose one action the content column offers).
    overview_shown: bool,
    /// A screenshot scenario to stage once its library is presented.
    pending_scenario: Option<String>,
    /// The capture's specimen snippet has been opened.
    specimen_open: bool,
    /// How many layouts the window has had since the specimen opened.
    settled_layouts: u32,
}

impl SnippetsApp {
    /// The window before the framework presents the library: empty, until
    /// the resource's state arrives.
    pub fn new() -> Self {
        let core: Shared = Rc::new(RefCell::new(Core::new()));
        let requests: Requests = Rc::default();
        let new_snippet = {
            let requests = Rc::clone(&requests);
            Button::icon_only("snippets.new", Icon::Plus, &tr("snippets-new"))
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
                detail_pane::header_actions(&core, &requests, detail.has_snippet()),
            );
        if !library_empty {
            shell.set_overview(Some(Box::new(ListPane::new(&core))));
        }
        Self {
            core,
            requests,
            shell,
            search: ui::search::SearchOpener::new(),
            titles,
            detail,
            overview_shown: !library_empty,
            pending_scenario: None,
            specimen_open: false,
            settled_layouts: 0,
        }
    }

    /// Stage a capture once its library (the starter library the resource's
    /// scenario creates, none for `empty`) is presented and the window has
    /// its size:
    ///
    /// * `narrow` — the sidebar drawer open over the content.
    /// * `detail` — a narrow window showing the open snippet.
    /// * `expand` — the fill-in-variables dialog over the open snippet.
    /// * `language`, `more` — the language picker or the overflow menu.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        self.pending_scenario = Some(scenario.unwrap_or_default().to_string());
        self.stage_pending_scenario();
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
        // First open the specimen.
        if !self.specimen_open {
            self.specimen_open = true;
            {
                let mut core = self.core.borrow_mut();
                core.open_specimen();
                core.show_pane = None;
            }
            self.refresh_columns();
            self.settled_layouts = 0;
        }
        // A menu opens under its button: let the window lay the specimen's
        // column out first.
        if matches!(scenario.as_str(), "language" | "more") && self.settled_layouts == 0 {
            self.pending_scenario = Some(scenario);
            return;
        }
        match scenario.as_str() {
            "narrow" => self.shell.open_sidebar(),
            "detail" => self.shell.show(NavPane::Content),
            "expand" => {
                let mut core = self.core.borrow_mut();
                if let Some(id) = core.open.clone() {
                    core.start_expand(&id);
                }
            }
            "language" | "more" => {
                let mut core = self.core.borrow_mut();
                if let Some(id) = core.open.clone() {
                    if scenario == "language" {
                        core.open_languages(&id);
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
            self.shell.set_overview(
                (!library_empty).then(|| Box::new(ListPane::new(&self.core)) as Element),
            );
        }
        if detail != self.detail {
            if detail.has_snippet() != self.detail.has_snippet() {
                let title = self.core.borrow().open_title();
                self.shell.set_content_header(
                    title,
                    detail_pane::header_actions(&self.core, &self.requests, detail.has_snippet()),
                );
            }
            self.detail = detail;
            self.shell
                .set_content(detail_pane::build(&self.core, &self.requests));
        }
    }

    /// Apply what retained controls asked for, bring the screen a choice
    /// asked for to the front, and keep the column titles current. The
    /// columns themselves are rebuilt only between frames
    /// ([`Self::refresh_columns`] from `sync` and `update`), never while
    /// the host is still routing an event or an action through them.
    fn settle(&mut self) {
        let requests = std::mem::take(&mut *self.requests.borrow_mut());
        for action in requests {
            self.core.borrow_mut().apply(action);
        }
        self.core.borrow_mut().open_landed();
        let (pane, notice, titles) = {
            let mut core = self.core.borrow_mut();
            (
                core.show_pane.take(),
                core.notice.take(),
                (core.selection_title(), core.open_title()),
            )
        };
        if let Some(pane) = pane {
            self.shell.show(pane);
        }
        if let Some(notice) = notice {
            self.shell.toast(notice, ToastKind::Info);
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

    /// Show the library the resource holds.
    pub(crate) fn show_library(&mut self, state: &Rc<Snippets>) {
        self.core.borrow_mut().show_library(state);
        self.settle();
    }

    /// Edit through the resource's dispatcher from now on.
    pub(crate) fn attach_dispatcher(&self, dispatcher: Dispatcher<Snippets>) {
        self.core.borrow_mut().attach_dispatcher(dispatcher);
    }

    /// What shows the snippet a search hit or an agent's `open` names, for
    /// [`ViewCx::on_open_item`](ui::framework::ViewCx::on_open_item); a
    /// snippet that is gone is refused.
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&SnippetId) -> Result<(), ui::tilo_resource::Refusal> + 'static {
        let requests = Rc::clone(&self.requests);
        let core = Rc::clone(&self.core);
        move |id| {
            if core.borrow().library.get(id).is_none() {
                return Err(ui::tilo_resource::Refusal::new("there is no such snippet"));
            }
            requests.borrow_mut().push(Action::OpenSnippet(id.clone()));
            Ok(())
        }
    }

    // ---- test accessors -------------------------------------------------

    #[cfg(test)]
    pub(crate) fn open_id_for_test(&self) -> Option<SnippetId> {
        self.core.borrow().open.clone()
    }

    #[cfg(test)]
    pub(crate) fn visible_count_for_test(&self) -> usize {
        self.core.borrow().visible.len()
    }

    #[cfg(test)]
    pub(crate) fn expand_open_for_test(&self) -> bool {
        self.core.borrow().expand.is_some()
    }

    #[cfg(test)]
    pub(crate) fn sidebar_is_overlay_for_test(&self) -> bool {
        self.shell.geometry().sidebar == SidebarPlacement::Drawer
    }

    #[cfg(test)]
    pub(crate) fn sidebar_open_for_test(&self) -> bool {
        self.shell.is_sidebar_open()
    }

    #[cfg(test)]
    pub(crate) fn is_push_for_test(&self) -> bool {
        !self.shell.geometry().stack.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn push_showing_detail_for_test(&self) -> bool {
        self.is_push_for_test() && self.shell.front_pane() == NavPane::Content
    }

    #[cfg(test)]
    pub(crate) fn toggle_sidebar_for_test(&mut self) {
        self.shell.toggle_sidebar();
        self.shell.snap();
    }

    #[cfg(test)]
    pub(crate) fn open_first_for_test(&mut self) {
        let first = self.core.borrow().visible.first().cloned();
        if let Some(id) = first {
            self.core.borrow_mut().open_snippet(id);
            self.settle();
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
        if request.wants::<dyn ui::undo::UndoPoints>() {
            request.provide::<dyn ui::undo::UndoPoints>(self);
        } else if request.wants::<dyn ui::widget::controls::ActionControls>() {
            request.provide::<dyn ui::widget::controls::ActionControls>(self);
        } else {
            Widget::provide(&self.shell, request);
        }
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::undo::UndoPoints>() {
            request.provide::<dyn ui::undo::UndoPoints>(self);
        } else if request.wants::<dyn ui::widget::controls::ActionControls>() {
            request.provide::<dyn ui::widget::controls::ActionControls>(self);
        } else {
            Widget::provide_mut(&mut self.shell, request);
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let (doc, _) = ui::doc::document_ref(state, path);
        self.core.borrow_mut().sync_body_editor(Some(doc));
        self.settle();
        self.refresh_columns();
        Widget::sync(&mut self.shell, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.settle();
        self.refresh_columns();
        self.stage_pending_scenario();
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
        {
            let mut core = self.core.borrow_mut();
            if window.area() > 0.0 {
                self.settled_layouts = self.settled_layouts.saturating_add(1);
            }
            core.window = window;
            if let Some(open) = core.menu.as_mut() {
                open.menu.constrain_to(window);
            }
        }
        // A staged menu opens once its button has been laid out.
        if self.pending_scenario.is_some() {
            self.stage_pending_scenario();
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
        self.core
            .borrow_mut()
            .body
            .control_mut()
            .set_rect(Rect::ZERO);
        Widget::draw(&mut self.shell, scene, transform, text, theme, presence);
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
            Widget::draw_overlay(&mut open.menu, scene, transform, text, theme, presence);
        }
        if let Some(modal) = core.expand.as_mut() {
            modal.draw(scene, transform, text, theme, window);
            modal.draw_overlay(scene, transform, text, theme, presence);
        } else if core.body_available() {
            core.body
                .control_mut()
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
                core.finish_expand(modal, outcome);
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
        self.settle();
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
        self.pending_scenario.is_some() || Widget::needs_animation_frame(&self.shell)
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        let core = self.core.borrow();
        if let Some(modal) = &core.expand {
            modal.ime_cursor_rect()
        } else if core.body.control().is_focused() {
            core.body.control().ime_cursor_rect()
        } else {
            drop(core);
            Widget::ime_cursor_rect(&self.shell)
        }
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        let core = self.core.borrow();
        if let Some(modal) = &core.expand {
            modal.ime_purpose()
        } else if core.body.control().is_focused() {
            core.body.control().ime_purpose()
        } else {
            drop(core);
            Widget::ime_purpose(&self.shell)
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let core = self.core.borrow();
        if let Some(modal) = &core.expand {
            modal.cursor_shape(point)
        } else if core.body_available() && core.body.control().rect().contains(point) {
            core.body.control().cursor_shape(point)
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
        if core.body_available()
            && let Some(node) = core.body.control().accessibility()
        {
            children.push(node.with_action_target("snippets.body"));
        }
        Some(
            AccessNode::new(Role::Window)
                .with_name(<Snippets as Resource>::LABEL)
                .with_semantic_key("snippets.application")
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut ui::widget::Edits<'_>,
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
                core.finish_expand(modal, outcome);
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
                        core.apply(chosen);
                        Some(true)
                    }
                    (Some(_), _) => Some(true),
                    (None, _) => Some(false),
                }
            } else if target == "snippets.body" && core.open.is_some() {
                core.sync_body_editor(Some(doc.doc()));
                if matches!(action, ui::a11y::Action::Focus) {
                    core.body.control_mut().set_focused(true);
                    Some(true)
                } else {
                    Some(Widget::perform_action(
                        &mut core.body,
                        action,
                        &[],
                        doc,
                        obj,
                    ))
                }
            } else {
                let hit = core.hits().find_map(|(_, candidate)| {
                    (candidate.semantic_id() == target).then(|| candidate.clone())
                });
                hit.map(|hit| {
                    if matches!(action, ui::a11y::Action::Activate) {
                        core.apply(hit);
                    }
                    matches!(action, ui::a11y::Action::Activate | ui::a11y::Action::Focus)
                })
            }
        };
        if let Some(handled) = handled {
            self.settle();
            return handled;
        }
        Widget::perform_semantic_action(&mut self.shell, target, action, state, path)
    }

    fn wants_pointer_capture(&self) -> bool {
        let core = self.core.borrow();
        (core.body_available() && core.body.control().wants_pointer_capture()) || {
            drop(core);
            Widget::wants_pointer_capture(&self.shell)
        }
    }

    fn wants_tab_input(&self) -> bool {
        let core = self.core.borrow();
        core.expand.is_some() || (core.body_available() && core.body.control().is_focused())
    }

    fn text_review(&self) -> Option<(usize, String)> {
        let core = self.core.borrow();
        if core.body_available() && core.body.control().is_focused() {
            core.body.control().text_review()
        } else {
            drop(core);
            Widget::text_review(&self.shell)
        }
    }

    fn name(&self) -> &'static str {
        "SnippetsApp"
    }
}

impl ui::undo::UndoPoints for SnippetsApp {
    fn get_state(&self) -> Option<String> {
        let core = self.core.borrow();
        serde_json::to_string(&(&core.open, core.body.control().get_state())).ok()
    }

    fn set_state(&mut self, state: &str) {
        let mut core = self.core.borrow_mut();
        if let Ok((id, Some(caret))) =
            serde_json::from_str::<(Option<SnippetId>, Option<String>)>(state)
            && id == core.open
        {
            core.body.control_mut().set_state(&caret);
        }
    }
}

#[cfg(test)]
mod tests;

impl SnippetsApp {
    pub(crate) fn show(&mut self, view: SnippetsView) -> ui::framework::EffectAnswer {
        if matches!(view, SnippetsView::Search) {
            self.search.open(ui::search::SearchRequest::this_app());
            return Ok(ui::framework::Effected::done());
        }
        {
            let mut core = self.core.borrow_mut();
            match view {
                SnippetsView::Reveal { tag } => {
                    if !core.library.tags().iter().any(|(known, _)| known == &tag) {
                        return Err("tag no longer exists".into());
                    }
                    core.select(Selection::Tag(tag));
                    core.show_pane = Some(NavPane::Overview);
                }
                SnippetsView::RevealFolder { folder } => {
                    if !core.library.folders().contains(&folder) {
                        return Err("folder no longer exists".into());
                    }
                    core.select(Selection::Folder(folder));
                    core.show_pane = Some(NavPane::Overview);
                }
                SnippetsView::Search => {}
            }
        }
        self.settle();
        Ok(ui::framework::Effected::done())
    }
}

impl ui::widget::controls::ActionControls for SnippetsApp {
    fn take_control_invocation(&mut self) -> Option<ui::tilo_actions::ControlInvocation> {
        self.search
            .take()
            .or_else(|| ui::widget::controls::take_control_invocation(&mut self.shell))
    }
    fn has_pending_control_invocation(&self) -> bool {
        self.search.is_pending()
            || ui::widget::controls::has_pending_control_invocation(&self.shell)
    }
}
