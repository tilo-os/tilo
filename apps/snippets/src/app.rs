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
    AppModel, Element, ModelPath, NavItem, NavList, NavPane, NavSection, NavShell,
    SidebarPlacement, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::detail_pane::{self, DetailKey};
use crate::expand::ExpandModal;
use crate::highlight;
use crate::list_pane::ListPane;
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
    Rename(SnippetId, String),
    /// File the snippet under `folder`; empty files it nowhere.
    SetFolder(SnippetId, String),
    /// What the snippet is for.
    SetDescription(SnippetId, String),
    /// Tag the snippet.
    AddTag(SnippetId, String),
    /// Take a tag off the snippet.
    RemoveTag(SnippetId, String),
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
            Self::Rename(id, _) => format!("snippets.snippet:{id}.title"),
            Self::SetFolder(id, _) => format!("snippets.snippet:{id}.folder"),
            Self::SetDescription(id, _) => format!("snippets.snippet:{id}.description"),
            Self::AddTag(id, _) => format!("snippets.snippet:{id}.add-tag"),
            Self::RemoveTag(id, tag) => format!("snippets.snippet:{id}.tag:{tag}.remove"),
        }
    }
}

/// A punctuation-safe key of a folder or tag name for semantic control ids.
pub(crate) fn stable_key(value: &str) -> String {
    hex::encode(value)
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
    /// A short confirmation of the last action, for the shell to show
    /// next.
    notice: Option<String>,
    /// A copy whose outcome the clipboard reports after it returned: one that
    /// fails later is said, not confirmed.
    copy: ui::CopyWatch,
    /// The resource's dispatcher, once the framework attaches the view.
    /// Each on-screen control submits a field edit or a verb through it.
    dispatch: Option<Dispatcher<Snippets>>,
    pub(crate) pointer: Point,
    /// The window, for dialogs.
    pub(crate) window: Rect,
    /// The snippet whose row the list should scroll into view, though it
    /// may already be the open one.
    pub(crate) reveal: Option<SnippetId>,
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
            notice: None,
            copy: ui::CopyWatch::default(),
            dispatch: None,
            pointer: Point::ZERO,
            window: Rect::ZERO,
            reveal: None,
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
        self.reveal = Some(id.clone());
        self.open = Some(id);
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
        self.reveal.clone_from(&self.open);
    }

    /// The column title of the content: the open snippet's title.
    pub(crate) fn open_title(&self) -> String {
        self.open
            .as_ref()
            .and_then(|id| self.library.get(id))
            .map(|s| s.title.clone())
            .unwrap_or_default()
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
    }

    /// Open the snippet this view is about to create once it is presented,
    /// in front on a narrow window.
    fn open_when_presented(&mut self) {
        self.open_minted = true;
        if let Some(dispatch) = &self.dispatch {
            dispatch.landed().set(None);
        }
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
            self.copy_text(body, id);
        }
    }

    /// Put a snippet's text on the clipboard, through the checked path — and
    /// through the expiring one when the snippet carries a credential, so an
    /// API key does not sit in the clipboard history for the rest of the day.
    /// The copy's outcome is watched: a clipboard that refuses is said, not
    /// confirmed.
    fn copy_text(&mut self, text: String, id: &SnippetId) {
        let secret = looks_like_a_credential(&text);
        let copy = if secret {
            ui::try_clipboard_copy_secret(&text, std::time::Duration::from_secs(45))
        } else {
            ui::try_clipboard_copy(&text)
        };
        let copied = self.copy.watch(copy);
        // The use is the person taking the snippet, which happened whether
        // the clipboard took it or not; the toast is what reports the
        // clipboard.
        self.record_use(id);
        if copied {
            self.toast(&tr(if secret {
                "snippets-copied-clears"
            } else {
                "snippets-copied"
            }));
        } else {
            self.toast(&tr("snippets-copy-failed"));
        }
    }

    /// A copy whose outcome the clipboard reported late as failed: say so
    /// once. `true` when it did.
    fn report_copy_failure(&mut self) -> bool {
        self.copy.failed()
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
        // The copy has no uses yet: leave Most used so it can open instead
        // of silently keeping the original in the editor.
        if self.selection == Selection::Smart(SmartFolder::MostUsed) {
            self.select(Selection::default());
        }
        // The copy is no favorite, so Favorites does not list it: there
        // the open snippet stays.
        if self.selection != Selection::Smart(SmartFolder::Favorites) {
            self.open_when_presented();
        }
        self.submit(Snippets::DUPLICATE.with(snippets::Duplicate {
            snippet: id.clone(),
            title: Some(tr_with("snippets-copy-title", &[("title", &title)])),
        }));
    }

    fn delete(&self, id: &SnippetId) {
        self.submit(Snippets::DELETE.with(id.clone()));
    }

    fn set_language(&self, id: &SnippetId, language: Language) {
        self.submit(Self::paths(id).language.set(language));
    }

    /// Name the snippet `title`; a cleared field keeps the old name until
    /// something is typed.
    fn rename(&self, id: &SnippetId, title: &str) {
        let title = title.trim();
        if title.is_empty() || self.library.get(id).is_some_and(|s| s.title == title) {
            return;
        }
        self.submit(Self::paths(id).title.set(title.to_owned()));
    }

    /// File the snippet under `folder`. The name is what the sidebar lists,
    /// so it is trimmed and a blank name means "no folder" — the same rule
    /// the record refuses a padded name with.
    fn set_folder(&self, id: &SnippetId, folder: &str) {
        let folder = folder.trim();
        if self.library.get(id).is_some_and(|s| s.folder.0 == folder) {
            return;
        }
        self.submit(
            Self::paths(id)
                .folder
                .set(ui::tilo_resource::Atom(folder.to_owned())),
        );
    }

    /// What the snippet is for: the line the list shows under its name.
    fn set_description(&self, id: &SnippetId, description: &str) {
        if self
            .library
            .get(id)
            .is_some_and(|s| s.description == description)
        {
            return;
        }
        self.submit(Self::paths(id).description.set(description.to_owned()));
    }

    /// Tag the snippet with `tag` (blank, padded and already-present tags
    /// change nothing).
    fn add_tag(&self, id: &SnippetId, tag: &str) {
        let tag = tag.trim();
        if tag.is_empty()
            || self
                .library
                .get(id)
                .is_some_and(|s| s.tags.contains(&tag.to_owned()))
        {
            return;
        }
        self.submit(Self::paths(id).tags.insert(&tag.to_owned()));
    }

    /// Take `tag` off the snippet; its other snippets keep it.
    fn remove_tag(&self, id: &SnippetId, tag: &str) {
        if !self
            .library
            .get(id)
            .is_some_and(|s| s.tags.contains(&tag.to_owned()))
        {
            return;
        }
        self.submit(Self::paths(id).tags.remove(&tag.to_owned()));
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
            Action::Rename(id, title) => self.rename(&id, &title),
            Action::SetFolder(id, folder) => self.set_folder(&id, &folder),
            Action::SetDescription(id, description) => {
                self.set_description(&id, &description);
            }
            Action::AddTag(id, tag) => self.add_tag(&id, &tag),
            Action::RemoveTag(id, tag) => self.remove_tag(&id, &tag),
        }
    }

    /// Open the open snippet's language picker.
    pub(crate) fn open_languages(&mut self, id: &SnippetId) {
        if self.open.as_ref() == Some(id) {
            self.pick_language = true;
        }
    }

    fn finish_expand(&mut self, modal: ExpandModal, outcome: ExpandOutcome) {
        match outcome {
            ExpandOutcome::Stay => {
                self.expand = Some(modal);
            }
            ExpandOutcome::Cancel => {}
            ExpandOutcome::Copy(text, id) => {
                self.copy_text(text, &id);
            }
        }
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

/// The sidebar: the shared [`NavList`], following the library and the
/// chosen place.
fn sidebar(core: &Shared, requests: &Requests) -> NavList {
    let requests = Rc::clone(requests);
    let core = Rc::clone(core);
    NavList::new(Vec::new())
        .with_id("snippets.sidebar")
        .with_accessible_name(tr("snippets-library"))
        .on_select(move |id| {
            if let Some(selection) = selection_of(&id) {
                requests.borrow_mut().push(Action::Select(selection));
            }
        })
        .follow(move || {
            let core = core.try_borrow().ok()?;
            Some((core.nav_sections(), Some(nav_id(&core.selection))))
        })
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
        let (detail, library_empty) = {
            let core = core.borrow();
            (DetailKey::of(&core), core.library.snippets.is_empty())
        };
        let mut shell = NavShell::new()
            .content_kind(ui::widget::ContentKind::Document)
            .with_id("snippets.shell")
            .with_window_toolbar()
            .sidebar(Box::new(sidebar(&core, &requests)))
            .content(detail_pane::build(&core, &requests));
        if !library_empty {
            shell.set_overview(Some(Box::new(ListPane::new(&core, &requests))));
        }
        Self {
            core,
            requests,
            shell,
            search: ui::search::SearchOpener::new(),
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
    /// * `language` — the language picker.
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
        // The language picker opens under its control: let the window lay
        // the specimen's column out first.
        if scenario == "language" && self.settled_layouts == 0 {
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
            "language" => {
                let mut core = self.core.borrow_mut();
                if let Some(id) = core.open.clone() {
                    core.open_languages(&id);
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
                (!library_empty)
                    .then(|| Box::new(ListPane::new(&self.core, &self.requests)) as Element),
            );
        }
        if detail != self.detail {
            self.detail = detail;
            self.shell
                .set_content(detail_pane::build(&self.core, &self.requests));
        }
    }

    /// Apply what retained controls asked for and bring the screen a choice
    /// asked for to the front. The
    /// columns themselves are rebuilt only between frames
    /// ([`Self::refresh_columns`] from `sync` and `update`), never while
    /// the host is still routing an event or an action through them.
    fn settle(&mut self) {
        let requests = std::mem::take(&mut *self.requests.borrow_mut());
        for action in requests {
            self.core.borrow_mut().apply(action);
        }
        self.core.borrow_mut().open_landed();
        let (pane, notice) = {
            let mut core = self.core.borrow_mut();
            (core.show_pane.take(), core.notice.take())
        };
        if let Some(pane) = pane {
            self.shell.show(pane);
        }
        if let Some(notice) = notice {
            self.shell.toast(notice, ToastKind::Info);
        }
        // The body editor takes input only while the content column is on
        // screen and no drawer floats over it.
        let geometry = self.shell.geometry();
        let drawer_open =
            geometry.sidebar == SidebarPlacement::Drawer && self.shell.is_sidebar_open();
        let content_hidden =
            !geometry.stack.is_empty() && self.shell.front_pane() != NavPane::Content;
        self.core.borrow_mut().body_reachable = !drawer_open && !content_hidden;
    }

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

    /// Ask for `action` the way a control does: queued, applied by the root.
    #[cfg(test)]
    pub(crate) fn request_for_test(&self, action: Action) {
        self.requests.borrow_mut().push(action);
    }

    /// The sidebar's sections as a test reads them.
    #[cfg(test)]
    pub(crate) fn nav_sections_for_test(&self) -> Vec<NavSection> {
        self.core.borrow().nav_sections()
    }

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

    /// How wide the sidebar stands on screen, as last laid out.
    #[cfg(test)]
    pub(crate) fn sidebar_width_for_test(&self) -> f64 {
        self.shell.geometry().sidebar_column.width()
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
        if self.core.borrow_mut().report_copy_failure() {
            let message = tr("snippets-copy-failed");
            self.core.borrow_mut().toast(&message);
        }
        let (doc, _) = ui::doc::document_ref(state, path);
        self.core.borrow_mut().sync_body_editor(Some(doc));
        self.take_window_toolbar_events();
        self.settle();
        self.refresh_columns();
        self.declare_window_toolbar();
        Widget::sync(&mut self.shell, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let asked = self.take_window_toolbar_events();
        self.settle();
        self.refresh_columns();
        self.stage_pending_scenario();
        self.declare_window_toolbar();
        Widget::update(&mut self.shell, state, path) || asked
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
        }
        // A staged picker opens once its control has been laid out.
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
            self.core.borrow_mut().pointer = ctx.logical(position);
        }
        // A popup of the shell (the language list) stands over the body
        // while open: its clicks and wheels, and the click outside that
        // closes it, are its own, never the editor's under it.
        let shell_holds_pointer = Widget::modal_capture(&self.shell).is_some()
            && matches!(
                event,
                WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. }
            );
        let consumed = {
            let mut core = self.core.borrow_mut();
            // The variables dialog is modal: it owns all input while open.
            if let Some(mut modal) = core.expand.take() {
                let pointer = core.pointer;
                let outcome = modal.handle_event(event, ctx, pointer);
                core.finish_expand(modal, outcome);
                true
            } else if !shell_holds_pointer && core.route_body_event(event, ctx) {
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

    fn ime_content(&self) -> Option<ui::ime::InputContent> {
        let core = self.core.borrow();
        if let Some(modal) = &core.expand {
            modal.ime_content()
        } else if core.body.control().is_focused() {
            core.body.control().ime_content()
        } else {
            drop(core);
            Widget::ime_content(&self.shell)
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
        use ui::a11y::{AccessNode, Role};
        let core = self.core.borrow();
        if let Some(modal) = core.expand.as_ref().and_then(ExpandModal::accessibility) {
            return Some(modal);
        }
        let mut children: Vec<AccessNode> =
            Widget::accessibility(&self.shell).into_iter().collect();
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
                None
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

mod window_bar;

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

/// Whether `text` looks like it carries a credential — an assignment to a
/// key, token, secret or password, a bearer header, or a vendor key prefix.
///
/// This is a guess, and it is deliberately conservative: a snippet wrongly
/// treated as a secret is cleared from the clipboard after a minute, which
/// costs the person a second copy; a snippet wrongly treated as ordinary
/// stays in the clipboard history. The checked, expiring path is the safe
/// direction to be wrong in.
fn looks_like_a_credential(text: &str) -> bool {
    const MARKERS: [&str; 6] = [
        "api_key",
        "apikey",
        "token",
        "secret",
        "password",
        "authorization:",
    ];
    let lower = text.to_lowercase();
    if lower.contains("bearer ") || lower.contains("-----begin ") {
        return true;
    }
    MARKERS.iter().any(|marker| {
        lower.contains(&format!("{marker}=")) || lower.contains(&format!("{marker}: "))
    }) || ["sk-", "ghp_", "xoxb-", "akia"]
        .iter()
        .any(|prefix| lower.contains(prefix))
}
