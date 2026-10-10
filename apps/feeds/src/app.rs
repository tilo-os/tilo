//! Root widget for the Feeds reader — RSS, Atom and Gemini.
//!
//! The window is the shared [`NavShell`]: the sidebar lists the library's
//! places (smart lists, folders with their feeds, tags) in a [`NavList`],
//! the overview is the article list of the chosen place, and the content is
//! the open article. The shell owns the header row, the sidebar toggle, the
//! drawer at narrow widths and the push to the article at very narrow
//! widths; this module owns the state they show.
//!
//! State that *converges* (subscriptions, read and star flags, cached
//! articles, reading settings) lives in the Automerge document via
//! [`crate::doc`]. State that is *local* to this peer (the chosen place, the
//! open article, scroll offsets, open dialogs and menus) lives in [`Core`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Politeness, Role};
use ui::icons::Icon;
use ui::portal::{
    FileFilter, LaunchChannel, LaunchOutcome, PickRequest, PortalChannel, PortalOutcome,
};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    AppModel, Button, Column, Element, ModalDialog, ModelPath, NavPane, NavShell, Popover,
    PopoverSide, SidebarPlacement, TextInputKind, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::chrome::{Anchor, HeaderButton, reader_settings_panel};
use crate::fetch::{FetchRequest, FetchResult, FetchWorker};
use crate::list_pane::{ListPane, ListState};
use crate::modal::{ModalBody, ModalIntent};
use crate::model::{
    Article, Filter, Id, Library, ReadFont, ReadSettings, ReadTheme, Row, normalize_tags,
};
use crate::reader_pane::{ReaderPane, ReaderState};
use crate::resource::library;
use crate::sidebar::SidebarNav;
use crate::{opml, sample};

fn opml_import_request() -> PickRequest {
    PickRequest::open("Import OPML", "Import")
        .thread_name("feeds-portal-import")
        .filter(
            FileFilter::new("OPML")
                .mimetype("text/x-opml")
                .mimetype("application/xml")
                .mimetype("text/xml")
                .glob("*.opml")
                .glob("*.xml"),
        )
        .all_files()
}

fn opml_export_request() -> PickRequest {
    PickRequest::save("Export OPML", "Export", "feeds-subscriptions.opml")
        .thread_name("feeds-portal-export")
        .filter(FileFilter::new("OPML").glob("*.opml"))
        .all_files()
}

/// Max CRDT writes persisted per frame. A big refresh queues its writes and
/// they flush across several frames, so no single frame stalls persisting the
/// whole batch. Kept small so even an unoptimised build with a large document
/// stays well inside a frame; the backlog still drains within a second or two
/// while the UI stays interactive.
pub(crate) const FLUSH_BUDGET: usize = 3;

/// How long a toast stays up.
const TOAST_FOR: std::time::Duration = std::time::Duration::from_secs(4);

/// The core every part of the window reads and changes.
pub(crate) type Shared = Rc<RefCell<Core>>;

/// What retained controls (the sidebar list, header buttons, the reading
/// settings) ask for. The root applies them with the document in hand.
pub(crate) type Requests = Rc<RefCell<Vec<Request>>>;

/// A request from a retained control.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Request {
    /// Show a place in the article list.
    Show(Filter),
    AddSubscription,
    NewFolder,
    Refresh,
    MarkAllRead,
    /// Star or unstar the open article.
    StarOpen,
    OpenOriginal,
    /// Show or hide the reading settings.
    ReaderSettings,
    /// The overview's menu (import, export).
    ListMenu,
    /// The open article's menu (read state, tags).
    ArticleMenu,
    /// A feed's menu: the row it acts on (its lift), and the point it opens
    /// at (the pointer, or under the row or its More button).
    FeedMenu(Id, Rect, Option<Point>),
    /// The person folded (`true`) or unfolded a folder in the sidebar.
    SetFolderCollapsed(Id, bool),
    SetFont(ReadFont),
    SetPaper(ReadTheme),
    /// The reading scale as a percentage.
    SetScale(f64),
}

/// What a menu row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuAction {
    MarkFeedRead(Id),
    RenameFeed(Id),
    Unsubscribe(Id),
    Import,
    Export,
    ToggleRead,
    EditTags,
}

impl MenuAction {
    fn target(&self) -> String {
        match self {
            Self::MarkFeedRead(id) => format!("feeds.menu.mark-read:{id}"),
            Self::RenameFeed(id) => format!("feeds.menu.rename:{id}"),
            Self::Unsubscribe(id) => format!("feeds.menu.unsubscribe:{id}"),
            Self::Import => "feeds.menu.import".into(),
            Self::Export => "feeds.menu.export".into(),
            Self::ToggleRead => "feeds.menu.toggle-read".into(),
            Self::EditTags => "feeds.menu.edit-tags".into(),
        }
    }
}

/// Which menu is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuKind {
    Feed(Id),
    List,
    Article,
}

/// The open menu and what each of its rows does.
pub(crate) struct OpenMenu {
    pub(crate) menu: ContextMenu,
    pub(crate) kind: MenuKind,
    pub(crate) actions: Vec<MenuAction>,
}

/// A live dialog: the shared [`ModalDialog`] plus the intent that says which
/// mutation its primary action runs. The `submit` / `cancel` cells are set by
/// the dialog's action callbacks and drained by the core.
pub(crate) struct FeedsModal {
    pub(crate) intent: ModalIntent,
    pub(crate) dialog: ModalDialog<ModalBody>,
    submit: Rc<Cell<bool>>,
    cancel: Rc<Cell<bool>>,
}

impl FeedsModal {
    fn new(intent: ModalIntent, title: &str, body: ModalBody, primary: &str) -> Self {
        let submit = Rc::new(Cell::new(false));
        let cancel = Rc::new(Cell::new(false));
        let dialog = ModalDialog::new(title, body)
            .open(true)
            .primary_action(primary, {
                let s = Rc::clone(&submit);
                move || s.set(true)
            })
            .secondary_action("Cancel", {
                let c = Rc::clone(&cancel);
                move || c.set(true)
            })
            .on_dismiss({
                let c = Rc::clone(&cancel);
                move || c.set(true)
            });
        Self {
            intent,
            dialog,
            submit,
            cancel,
        }
    }

    fn add_subscription() -> Self {
        let body = ModalBody::input(
            "Paste the address of an RSS, Atom or Gemini feed, or of a website that has one.",
            "Feed URL",
            "",
            "https://example.com/feed.xml",
            TextInputKind::Url,
        );
        Self::new(
            ModalIntent::AddSubscription,
            "Add subscription",
            body,
            "Subscribe",
        )
    }

    fn add_folder() -> Self {
        let body = ModalBody::input(
            "Folders group your subscriptions in the sidebar.",
            "Folder name",
            "",
            "Newsletters",
            TextInputKind::Name,
        );
        Self::new(ModalIntent::AddFolder, "New folder", body, "Create folder")
    }

    fn rename(feed_id: Id, title: &str) -> Self {
        let body = ModalBody::input(
            "Choose the name this feed shows in the sidebar.",
            "Feed name",
            title,
            "Feed name",
            TextInputKind::Name,
        );
        Self::new(
            ModalIntent::RenameFeed { feed_id },
            "Rename feed",
            body,
            "Rename",
        )
    }

    fn edit_tags(article_id: Id, tags: &[String]) -> Self {
        let body = ModalBody::input(
            "Separate tags with commas.",
            "Tags",
            &tags.join(", "),
            "design, research",
            TextInputKind::Prose,
        );
        Self::new(
            ModalIntent::EditArticleTags { article_id },
            "Edit tags",
            body,
            "Save tags",
        )
    }
}

/// Live progress for an in-flight (or just-finished) refresh, shown above
/// the article list.
#[derive(Clone, Debug, Default)]
pub(crate) struct RefreshState {
    /// Feeds queued in this refresh batch.
    pub(crate) total: usize,
    /// Feeds that have reported back (success or failure).
    pub(crate) done: usize,
    /// Feeds that returned at least one new article.
    pub(crate) updated_feeds: usize,
    /// Feeds that failed to fetch or parse.
    pub(crate) failed_feeds: usize,
    /// New articles across the batch.
    pub(crate) new_items: usize,
    /// Set once every feed reported back; drives the summary and its expiry.
    pub(crate) finished_at: Option<Instant>,
}

impl RefreshState {
    pub(crate) fn fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64).clamp(0.0, 1.0)
        }
    }

    pub(crate) fn in_progress(&self) -> bool {
        self.total > 0 && self.finished_at.is_none()
    }

    /// Whether the progress is still shown (in progress, or a summary that
    /// has not aged out).
    pub(crate) fn visible(&self) -> bool {
        if self.in_progress() {
            return true;
        }
        self.finished_at
            .is_some_and(|t| t.elapsed().as_secs_f32() < 6.0)
    }

    /// One sentence for the progress line.
    pub(crate) fn status(&self) -> String {
        if self.in_progress() {
            return format!("Refreshing {} of {} feeds", self.done, self.total);
        }
        let mut parts: Vec<String> = Vec::new();
        if self.new_items > 0 {
            parts.push(plural(self.new_items, "new article", "new articles"));
        }
        if self.failed_feeds > 0 {
            parts.push(format!(
                "{} could not be refreshed",
                plural(self.failed_feeds, "feed", "feeds")
            ));
        }
        // Each part opens with its count, so the sentence starts with one.
        if parts.is_empty() {
            "All feeds are up to date".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// `1 feed`, `3 feeds`.
pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// What a refresh fetched for one subscription, still to land in the
/// library. Draining happens while painting (no document in hand), so it
/// queues here and lands on the next pass that holds the document.
#[derive(Clone, Debug)]
pub(crate) struct Landing {
    pub(crate) fetched: library::Refreshed,
}

/// A new item this window asked for, shown once the library holds it: the
/// verb that makes it mints its id, so the window recognizes it by what it
/// asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Awaited {
    /// The subscription to this address: refresh it, and show it when
    /// asked to.
    Feed(String, bool),
    /// The folder of this name that was not there before: show it.
    Folder(String, Vec<Id>),
}

/// The state of the window.
pub(crate) struct Core {
    pub(crate) store: Library,
    pub(crate) filter: Filter,
    pub(crate) selected: Option<Id>,
    pub(crate) settings: ReadSettings,
    /// Epoch seconds the relative times are measured from.
    pub(crate) now: i64,
    pub(crate) list: ListState,
    pub(crate) reader: ReaderState,
    /// A dialog to present; the root takes it and owns it while it is open.
    pending_modal: Option<FeedsModal>,
    /// Whether the root presents a dialog.
    pub(crate) modal_open: bool,
    pub(crate) menu: Option<OpenMenu>,
    pub(crate) toast: Option<(String, Instant)>,
    pub(crate) refresh: RefreshState,
    pub(crate) pending_landings: Vec<Landing>,
    /// Articles opened from search or by an agent, to mark read on the next
    /// pass that holds the document, as a click does.
    pending_reads: Vec<Id>,
    /// New subscriptions and folders this window asked for, until the
    /// library holds them.
    pub(crate) awaited: Vec<Awaited>,
    pub(crate) import_rx: Option<PortalChannel>,
    pub(crate) export_rx: Option<(PortalChannel, String)>,
    open_link_rx: Option<LaunchChannel>,
    background_poll_deadline: Cell<Option<Instant>>,
    pub(crate) worker: Option<FetchWorker>,
    synced: bool,
    pub(crate) dispatcher: Option<ui::framework::Dispatcher<Library>>,
    read_settings: Option<ui::framework::Setting<ReadSettings>>,
    refresh_reply: Option<ui::framework::EffectReply>,
    /// The document heads the mirrored store was last reconciled against.

    /// The hermetic `--shots`/demo build seeds the bundled sample library;
    /// the live app starts empty on the person's document.
    /// The pointer, in window coordinates.
    pub(crate) pointer: Point,
    /// The window, for dialogs and menus.
    pub(crate) window: Rect,
    /// The screen a narrow window should bring to the front next.
    show_pane: Option<NavPane>,
    /// Where the header controls that open a menu or panel sit.
    pub(crate) anchors: HashMap<Anchor, Rect>,
    /// The first feed's sidebar row (its lift), as last painted.
    pub(crate) first_feed_row: Option<(Id, Rect)>,
}

/// Mark the article at `id` read or unread.
fn read_edit(id: &Id, read: bool) -> ui::tilo_resource::Edit<Library> {
    Library::paths().articles.item(id).read.set(read)
}

impl Core {
    fn new(demo: bool) -> Self {
        let store = if demo {
            sample::store()
        } else {
            Library::new()
        };
        let mut core = Self {
            store,
            filter: Filter::All,
            selected: None,
            settings: ReadSettings::default(),
            now: ui::clock::local_now().timestamp(),
            list: ListState::default(),
            reader: ReaderState::default(),
            pending_modal: None,
            modal_open: false,
            menu: None,
            toast: None,
            refresh: RefreshState::default(),
            pending_landings: Vec::new(),
            pending_reads: Vec::new(),
            awaited: Vec::new(),
            import_rx: None,
            export_rx: None,
            open_link_rx: None,
            background_poll_deadline: Cell::new(None),
            worker: None,
            synced: false,
            dispatcher: None,
            read_settings: None,
            refresh_reply: None,

            pointer: Point::ZERO,
            window: Rect::ZERO,
            show_pane: None,
            anchors: HashMap::new(),
            first_feed_row: None,
        };
        if demo {
            core.select_featured();
        }
        core
    }

    /// Open the richest unread article so a capture lands on a full body.
    pub(crate) fn select_featured(&mut self) {
        self.selected = self
            .store
            .articles_for(&Filter::All)
            .into_iter()
            .max_by_key(|a| a.blocks.0.len() + if a.read { 0 } else { 100 })
            .map(|a| a.id.clone());
    }

    fn ensure_selection(&mut self) {
        let articles = self.store.articles_for(&self.filter);
        let in_list = self
            .selected
            .as_ref()
            .is_some_and(|id| articles.iter().any(|a| a.id == id));
        if !in_list {
            self.selected = articles.first().map(|a| a.id.clone());
        }
    }

    pub(crate) fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), ui::time::now()));
    }

    pub(crate) fn toast_visible(&self) -> Option<&str> {
        self.toast
            .as_ref()
            .filter(|(_, since)| since.elapsed() < TOAST_FOR)
            .map(|(message, _)| message.as_str())
    }

    fn worker(&mut self) -> &mut FetchWorker {
        self.worker.get_or_insert_with(FetchWorker::spawn)
    }

    /// The open article, if it still exists.
    pub(crate) fn open_article(&self) -> Option<Row<'_, Article>> {
        self.selected.as_ref().and_then(|id| self.store.article(id))
    }

    // --- titles ----------------------------------------------------------

    /// The overview's title: what the chosen place is called.
    pub(crate) fn filter_title(&self) -> String {
        match &self.filter {
            Filter::All => "All articles".to_string(),
            Filter::Unread => "Unread".to_string(),
            Filter::Starred => "Starred".to_string(),
            Filter::Folder(id) => self
                .store
                .folder(id)
                .map_or_else(|| "Folder".into(), |f| f.name.clone()),
            Filter::Feed(id) => self
                .store
                .feed(id)
                .map_or_else(|| "Feed".into(), |f| f.title.clone()),
            Filter::Tag(key) => self
                .store
                .tags()
                .into_iter()
                .find(|tag| crate::model::tag_key(tag) == *key)
                .unwrap_or_else(|| "Tag".into()),
        }
    }

    /// The content's title: the feed the open article came from.
    pub(crate) fn article_title(&self) -> String {
        self.open_article()
            .and_then(|a| self.store.feed(&a.feed_id))
            .map(|f| f.title.clone())
            .unwrap_or_default()
    }

    // --- places and selection --------------------------------------------

    pub(crate) fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.list.scroll_to_top();
        self.list.reveal = true;
        self.ensure_selection();
        self.reader.scroll_to_top();
        // A narrow window shows the chosen list.
        self.show_pane = Some(NavPane::Overview);
    }

    pub(crate) fn select_article(&mut self, id: &Id, doc: &mut ui::widget::Edits<'_>) {
        self.selected = Some(id.clone());
        self.reader.scroll_to_top();
        self.list.reveal = true;
        if self.store.mark_read(id, true) {
            self.perform(doc, read_edit(id, true));
        }
        // A narrow window pushes the article in; a wide one already shows it.
        self.show_pane = Some(NavPane::Content);
    }

    fn move_selection(&mut self, delta: i64, doc: &mut ui::widget::Edits<'_>) {
        let ids: Vec<Id> = self
            .store
            .articles_for(&self.filter)
            .iter()
            .map(|a| a.id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|s| ids.iter().position(|x| x == s));
        let next = match current {
            Some(i) => (i as i64 + delta).clamp(0, ids.len() as i64 - 1) as usize,
            None => 0,
        };
        if let Some(id) = ids.get(next) {
            self.select_article(id, doc);
            // Arrow keys step through the list without leaving it.
            self.show_pane = None;
        }
    }

    pub(crate) fn toggle_star(&mut self, id: &Id, doc: &mut ui::widget::Edits<'_>) {
        if self.store.toggle_star(id) {
            let starred = self.store.article(id).is_some_and(|a| a.starred);
            self.perform(doc, Library::paths().articles.item(id).starred.set(starred));
            self.set_toast(if starred {
                "Article starred"
            } else {
                "Star removed"
            });
        }
    }

    fn toggle_read(&mut self, doc: &mut ui::widget::Edits<'_>) {
        if let Some(id) = self.selected.clone() {
            let now_read = self.store.article(&id).is_none_or(|a| !a.read);
            if self.store.mark_read(&id, now_read) {
                self.perform(doc, read_edit(&id, now_read));
                self.set_toast(if now_read {
                    "Marked as read"
                } else {
                    "Marked as unread"
                });
            }
        }
    }

    pub(crate) fn open_tag_editor(&mut self) {
        let Some(article) = self.open_article() else {
            return;
        };
        let modal = FeedsModal::edit_tags(article.id.clone(), &article.tag_list());
        self.open_modal(modal);
    }

    /// Mark everything the chosen place lists as read: the library's verbs
    /// for everything and for one subscription, one step of field edits for
    /// any other place.
    pub(crate) fn mark_all_read(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let filter = self.filter.clone();
        let ids = self.store.mark_all_read(&filter);
        if !ids.is_empty() {
            match &filter {
                Filter::All => self.perform(doc, Library::MARK_EVERYTHING_READ.invoke()),
                Filter::Feed(feed) => {
                    self.perform(doc, Library::MARK_FEED_READ.with(feed.clone()));
                }
                _ => {
                    for id in &ids {
                        self.perform(doc, read_edit(id, true));
                    }
                }
            }
        }
        self.set_toast(format!(
            "Marked {} as read",
            plural(ids.len(), "article", "articles")
        ));
    }

    // --- action model -----------------------------------------------------

    /// Submit one field edit or verb through the library's door, now: the
    /// single mutation path, shared with the palette and agents. One event's
    /// submissions are one undo step.
    pub(crate) fn perform(
        &mut self,
        edits: &mut ui::widget::Edits<'_>,
        submit: impl ui::framework::Submit<Library>,
    ) {
        let Some(dispatcher) = self.dispatcher.as_ref() else {
            self.set_toast("The library is not attached to its resource.");
            return;
        };
        if let Err(error) = dispatcher.submit_now(edits, submit) {
            self.set_toast(format!("The library was not changed: {error}"));
        }
    }

    // --- reading settings -------------------------------------------------

    /// Change the reading settings and persist them, so the change survives
    /// restarts and converges with collaborators.
    pub(crate) fn bump_setting(
        &mut self,
        _edits: &mut ui::widget::Edits<'_>,
        change: impl FnOnce(&mut ReadSettings),
    ) {
        if let Err(error) = self.change_read_settings(change) {
            self.set_toast(error);
        }
    }

    // --- folders and subscriptions -----------------------------------------

    /// Add a folder through the same verb the palette uses; it is shown
    /// once the library holds it.
    fn add_folder(&mut self, name: &str, doc: &mut ui::widget::Edits<'_>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let known = self.store.folders.ids().cloned().collect();
        self.awaited.push(Awaited::Folder(name.to_owned(), known));
        self.perform(doc, Library::NEW_FOLDER.with(name));
        self.set_toast(format!("Created folder {name}"));
    }

    /// Start a refresh of every subscription. Only enqueues on the worker:
    /// no network I/O on the UI thread. Results drain every frame.
    pub(crate) fn refresh_all(&mut self) {
        let ids: Vec<Id> = self.store.feeds.ids().cloned().collect();
        self.refresh_feeds(&ids);
        self.toast = None;
    }

    /// Enqueue a background refresh of `ids`.
    pub(crate) fn refresh_feeds(&mut self, ids: &[Id]) {
        let reqs: Vec<FetchRequest> = ids
            .iter()
            .filter_map(|id| {
                self.store.feed(id).map(|f| FetchRequest::Refresh {
                    feed_id: id.clone(),
                    url: f.url.clone(),
                    kind: f.kind,
                    certificate_pin: f.certificate_pin.clone(),
                })
            })
            .collect();
        if reqs.is_empty() {
            return;
        }
        let n = reqs.len();
        let w = self.worker();
        for r in reqs {
            w.send(r);
        }
        self.refresh = RefreshState {
            total: n,
            ..Default::default()
        };
    }

    /// Add a subscription through the library's verb, in the folder the
    /// chosen place is in. Once the library holds it, it is refreshed (an
    /// outside fetch) and shown.
    pub(crate) fn subscribe(&mut self, url: &str, doc: &mut ui::widget::Edits<'_>) {
        let url = url.trim().to_string();
        if url.is_empty() {
            return;
        }
        if let Some(existing) = self.store.feed_at(&url).map(|feed| feed.id.clone()) {
            self.set_filter(Filter::Feed(existing));
            self.set_toast(format!("Already subscribed to {url}"));
            return;
        }
        let folder = match &self.filter {
            Filter::Folder(id) => Some(id.clone()),
            Filter::Feed(fid) => self.store.feed(fid).and_then(|f| f.folder.clone()),
            _ => None,
        };
        self.awaited.push(Awaited::Feed(url.clone(), true));
        self.perform(
            doc,
            Library::ADD_SUBSCRIPTION.with(library::AddSubscription {
                url: url.clone(),
                folder,
            }),
        );
        self.set_toast(format!("Subscribing to {url}"));
    }

    /// Show and refresh what this window asked for, now that the library
    /// holds it.
    fn arrive(&mut self) {
        let mut refresh = Vec::new();
        let mut show = None;
        self.awaited.retain(|awaited| match awaited {
            Awaited::Feed(url, shown) => match self.store.feed_at(url) {
                Some(feed) => {
                    refresh.push(feed.id.clone());
                    if *shown {
                        show = Some(Filter::Feed(feed.id.clone()));
                    }
                    false
                }
                None => true,
            },
            Awaited::Folder(name, known) => match self
                .store
                .folders
                .iter()
                .find(|(id, folder)| &folder.name == name && !known.contains(id))
            {
                Some((id, _)) => {
                    show = Some(Filter::Folder(id.clone()));
                    false
                }
                None => true,
            },
        });
        if !refresh.is_empty() {
            self.refresh_feeds(&refresh);
        }
        if let Some(filter) = show {
            self.set_filter(filter);
        }
    }

    pub(crate) fn export_opml(&mut self) {
        if self.import_rx.is_some() || self.export_rx.is_some() || self.store.feeds.is_empty() {
            return;
        }
        let xml = opml::export(&self.store);
        self.export_rx = Some((opml_export_request().send(), xml));
    }

    fn drain_export(&mut self) {
        let Some((rx, _)) = &self.export_rx else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                PortalOutcome::Failed("the file chooser closed without a result".into())
            }
        };
        let Some((_, xml)) = self.export_rx.take() else {
            return;
        };
        match outcome {
            PortalOutcome::Opened(path) => {
                match tilo_files::file_at(&path).and_then(|(dir, name)| {
                    tilo_files::blocking::save_document(&dir, &name, xml.as_bytes())
                }) {
                    Ok(()) => self.set_toast(format!(
                        "Exported subscriptions to {}",
                        path.file_name().map_or_else(
                            || path.display().to_string(),
                            |n| n.to_string_lossy().into_owned()
                        )
                    )),
                    Err(error) => self.set_toast(format!("Export failed: {error}")),
                }
            }
            PortalOutcome::Cancelled => self.set_toast("Export cancelled"),
            PortalOutcome::Failed(reason) => self.set_toast(format!("Export failed: {reason}")),
        }
    }

    /// Ask the host portal for an `.opml` file. The chooser runs off the UI
    /// thread; the chosen path arrives over a channel drained each frame.
    pub(crate) fn request_import(&mut self) {
        if self.import_rx.is_some() || self.export_rx.is_some() {
            return;
        }
        self.import_rx = Some(opml_import_request().send());
    }

    /// Drain an in-flight import. A chosen file is read and parsed, every
    /// feed in it subscribed, and a background fetch started.
    fn drain_import(&mut self, doc: &mut ui::widget::Edits<'_>) -> bool {
        let Some(rx) = &self.import_rx else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.import_rx = None;
                return false;
            }
        };
        self.import_rx = None;
        match outcome {
            PortalOutcome::Opened(path) => match tilo_files::file_at(&path)
                .and_then(|(dir, name)| tilo_files::blocking::read(&dir, &name))
                .and_then(|bytes| {
                    String::from_utf8(bytes)
                        .map_err(|_| tilo_files::FsError::Io("the file is not UTF-8 text".into()))
                }) {
                Ok(text) => {
                    let added = self.apply_opml_import(&text, doc);
                    if added.is_empty() {
                        self.set_toast("That file has no new feeds");
                    } else {
                        self.set_toast(format!(
                            "Imported {}",
                            plural(added.len(), "feed", "feeds")
                        ));
                    }
                    true
                }
                Err(e) => {
                    self.set_toast(format!("Import failed: {e}"));
                    true
                }
            },
            PortalOutcome::Cancelled => false,
            PortalOutcome::Failed(reason) => {
                self.set_toast(format!("Import failed: {reason}"));
                true
            }
        }
    }

    /// Subscribe to what OPML `text` lists that the library does not have
    /// yet (folders as needed, de-duplicated by address) as one verb, and
    /// refresh those once the library holds them. Returns their addresses.
    pub(crate) fn apply_opml_import(
        &mut self,
        text: &str,
        doc: &mut ui::widget::Edits<'_>,
    ) -> Vec<String> {
        let outlines = opml::outlines(&opml::parse(text));
        let mut added: Vec<String> = Vec::new();
        for outline in &outlines {
            let url = outline.url.trim();
            if !url.is_empty()
                && self.store.feed_at(url).is_none()
                && !added.iter().any(|a| a == url)
            {
                added.push(url.to_owned());
            }
        }
        if added.is_empty() {
            return added;
        }
        self.awaited
            .extend(added.iter().map(|url| Awaited::Feed(url.clone(), false)));
        self.perform(doc, Library::IMPORT_SUBSCRIPTIONS.with(outlines));
        added
    }

    // --- menus --------------------------------------------------------------

    /// Open a menu of `actions` under `anchor`; `row` is the row it acts on.
    fn open_menu(
        &mut self,
        kind: MenuKind,
        anchor: Rect,
        row: Option<Rect>,
        items: Vec<(MenuItem, MenuAction)>,
    ) {
        let mut menu = ContextMenu::new();
        let mut actions = Vec::new();
        for (item, action) in items {
            menu.push(item);
            actions.push(action);
        }
        if let Some(row) = row {
            menu.anchor_row(row);
        }
        menu.open_anchored(anchor, self.menu_bounds());
        self.menu = Some(OpenMenu {
            menu,
            kind,
            actions,
        });
    }

    /// Where menus may open: the window; a menu keeps its own air from its
    /// edges.
    pub(crate) fn menu_bounds(&self) -> Rect {
        self.window
    }

    /// The menu of one feed, under its sidebar row (`row` is the row's lift),
    /// sharing the row's leading edge. The menu keeps its own air from the
    /// row it opens under.
    pub(crate) fn open_feed_menu(&mut self, feed_id: Id, row: Rect) {
        self.open_feed_menu_on(feed_id, row, row);
    }

    /// The menu of one feed, opened by a right click at `at` on its row.
    pub(crate) fn open_feed_menu_at(&mut self, feed_id: Id, row: Rect, at: Point) {
        self.open_feed_menu_on(feed_id, row, Rect::from_points(at, at));
    }

    fn open_feed_menu_on(&mut self, feed_id: Id, row: Rect, anchor: Rect) {
        if self.store.feed(&feed_id).is_none() {
            return;
        }
        let unread = self.store.unread_for_feed(&feed_id) > 0;
        self.open_menu(
            MenuKind::Feed(feed_id.clone()),
            anchor,
            Some(row),
            vec![
                (
                    MenuItem::action("Mark all as read", || {})
                        .with_icon(Icon::Check)
                        .with_enabled(unread),
                    MenuAction::MarkFeedRead(feed_id.clone()),
                ),
                (
                    MenuItem::action("Rename…", || {}).with_icon(Icon::Pencil),
                    MenuAction::RenameFeed(feed_id.clone()),
                ),
                (
                    MenuItem::destructive_action("Unsubscribe…", || {}).with_icon(Icon::Trash2),
                    MenuAction::Unsubscribe(feed_id),
                ),
            ],
        );
    }

    fn open_list_menu(&mut self) {
        let anchor = self
            .anchors
            .get(&Anchor::ListMenu)
            .copied()
            .unwrap_or_default();
        let has_feeds = !self.store.feeds.is_empty();
        self.open_menu(
            MenuKind::List,
            anchor,
            None,
            vec![
                (
                    MenuItem::action("Import OPML…", || {}).with_icon(Icon::FolderOpen),
                    MenuAction::Import,
                ),
                (
                    MenuItem::action("Export OPML…", || {})
                        .with_icon(Icon::Download)
                        .with_enabled(has_feeds),
                    MenuAction::Export,
                ),
            ],
        );
    }

    fn open_article_menu(&mut self) {
        let Some(read) = self.open_article().map(|a| a.read) else {
            return;
        };
        let anchor = self
            .anchors
            .get(&Anchor::ArticleMenu)
            .copied()
            .unwrap_or_default();
        self.open_menu(
            MenuKind::Article,
            anchor,
            None,
            vec![
                (
                    MenuItem::action(
                        if read {
                            "Mark as unread"
                        } else {
                            "Mark as read"
                        },
                        || {},
                    )
                    .with_icon(Icon::Mail),
                    MenuAction::ToggleRead,
                ),
                (
                    MenuItem::action("Edit tags…", || {}).with_icon(Icon::Pencil),
                    MenuAction::EditTags,
                ),
            ],
        );
    }

    pub(crate) fn apply_menu_action(
        &mut self,
        action: MenuAction,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        match action {
            MenuAction::MarkFeedRead(feed_id) => {
                let ids = self.store.mark_all_read(&Filter::Feed(feed_id.clone()));
                if !ids.is_empty() {
                    self.perform(doc, Library::MARK_FEED_READ.with(feed_id));
                }
                self.set_toast(format!(
                    "Marked {} as read",
                    plural(ids.len(), "article", "articles")
                ));
            }
            MenuAction::RenameFeed(feed_id) => {
                let title = self
                    .store
                    .feed(&feed_id)
                    .map(|f| f.title.clone())
                    .unwrap_or_default();
                self.open_modal(FeedsModal::rename(feed_id, &title));
            }
            // The host asks the person before it unsubscribes; the feed
            // leaves the sidebar once the library no longer holds it.
            MenuAction::Unsubscribe(feed_id) => {
                self.perform(doc, Library::UNSUBSCRIBE.with(feed_id));
            }
            MenuAction::Import => self.request_import(),
            MenuAction::Export => self.export_opml(),
            MenuAction::ToggleRead => self.toggle_read(doc),
            MenuAction::EditTags => self.open_tag_editor(),
        }
    }

    /// Route an event to the open menu; apply what it chose.
    fn route_menu_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if matches!(event, WindowEvent::Focused(false)) {
            self.menu = None;
            return;
        }
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
            self.apply_menu_action(action, &mut ctx.state);
        }
    }

    // --- dialogs ------------------------------------------------------------

    fn open_modal(&mut self, modal: FeedsModal) {
        self.menu = None;
        self.pending_modal = Some(modal);
    }

    pub(crate) fn open_add_subscription(&mut self) {
        self.open_modal(FeedsModal::add_subscription());
    }

    pub(crate) fn open_add_folder(&mut self) {
        self.open_modal(FeedsModal::add_folder());
    }

    /// Run a dialog's primary action with `value` against the store and the
    /// document. Returns `false` (the dialog stays open) when a required
    /// field is empty.
    pub(crate) fn submit_dialog(
        &mut self,
        intent: ModalIntent,
        value: &str,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        let value = value.trim().to_string();
        if intent.requires_input() && value.is_empty() {
            return false;
        }
        match intent {
            ModalIntent::AddSubscription => self.subscribe(&value, doc),
            ModalIntent::AddFolder => self.add_folder(&value, doc),
            ModalIntent::RenameFeed { feed_id } => self.rename_feed(&feed_id, &value, doc),
            ModalIntent::EditArticleTags { article_id } => self.set_tags(&article_id, &value, doc),
        }
        true
    }

    /// Give the article `id` the comma-separated tags `value`, normalized:
    /// one step of edits of its tag set.
    fn set_tags(&mut self, id: &Id, value: &str, doc: &mut ui::widget::Edits<'_>) {
        let Some(before) = self.store.article(id).map(|article| article.tags.clone()) else {
            return;
        };
        let tags = normalize_tags(value.split(','));
        let after: ui::tilo_resource::Set<String> = tags.iter().cloned().collect();
        if before == after {
            return;
        }
        let path = Library::paths().articles.item(id).tags;
        for edit in path.edits(&before, &after) {
            self.perform(doc, edit);
        }
        if let Some(article) = self.store.article_mut(id) {
            article.tags = after;
        }
        self.set_toast(if tags.is_empty() {
            "Tags removed".to_string()
        } else {
            format!("Saved {}", plural(tags.len(), "tag", "tags"))
        });
    }

    pub(crate) fn rename_feed(
        &mut self,
        feed_id: &Id,
        name: &str,
        doc: &mut ui::widget::Edits<'_>,
    ) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        if let Some(f) = self.store.feeds.get_mut(feed_id) {
            name.clone_into(&mut f.title);
            self.perform(
                doc,
                Library::paths().feeds.item(feed_id).title.replace(name),
            );
            self.set_toast("Feed renamed");
        }
    }

    // --- fetch results ------------------------------------------------------

    /// Fold what the fetch worker produced into the store and the refresh
    /// progress, without touching the document (safe while painting).
    /// Each change queues its CRDT write for the next pass that holds the
    /// document. Returns whether anything changed.
    pub(crate) fn drain_fetches_live(&mut self) -> bool {
        let results = match &mut self.worker {
            Some(w) => w.poll(),
            None => return false,
        };
        if results.is_empty() {
            return false;
        }
        for r in results {
            match r {
                FetchResult::Articles {
                    feed_id,
                    title,
                    articles,
                    certificate_pin,
                } => {
                    let Some(feed) = self.store.feed(&feed_id) else {
                        self.refresh.done += 1;
                        self.maybe_finish_refresh();
                        continue;
                    };
                    let title = title.filter(|t| !t.is_empty() && *t != feed.title);
                    // Trust on first use: a feed added without a fetch
                    // (OPML import, an action) pins what it first sees.
                    let certificate_pin =
                        certificate_pin.filter(|_| feed.certificate_pin.is_none());
                    let mut fresh: Vec<_> = Vec::new();
                    for article in articles {
                        let landing = self
                            .pending_landings
                            .iter()
                            .flat_map(|landing| &landing.fetched.articles)
                            .chain(&fresh)
                            .any(|queued| {
                                queued.url == article.url && queued.title == article.title
                            });
                        if !landing
                            && !self
                                .store
                                .has_article(&feed_id, &article.url, &article.title)
                        {
                            fresh.push(article);
                        }
                    }
                    let added = fresh.len();
                    if title.is_some() || certificate_pin.is_some() || !fresh.is_empty() {
                        self.pending_landings.push(Landing {
                            fetched: library::Refreshed {
                                feed: feed_id,
                                title,
                                certificate_pin,
                                articles: fresh,
                            },
                        });
                    }
                    self.refresh.done += 1;
                    if added > 0 {
                        self.refresh.updated_feeds += 1;
                        self.refresh.new_items += added;
                    }
                    self.maybe_finish_refresh();
                }
                FetchResult::Failed { feed_id, message } => {
                    if feed_id.is_some() {
                        self.refresh.done += 1;
                        self.refresh.failed_feeds += 1;
                        self.maybe_finish_refresh();
                    } else {
                        self.set_toast(format!("Could not fetch the feed: {message}"));
                    }
                }
            }
        }
        true
    }

    fn maybe_finish_refresh(&mut self) {
        if self.refresh.total > 0
            && self.refresh.done >= self.refresh.total
            && self.refresh.finished_at.is_none()
        {
            self.refresh.finished_at = Some(ui::time::now());
            if let Some(reply) = self.refresh_reply.take() {
                reply.finish(if self.refresh.failed_feeds > 0 {
                    Err(format!(
                        "{} of {} subscriptions failed to refresh",
                        self.refresh.failed_feeds, self.refresh.total
                    ))
                } else {
                    Ok(ui::tilo_actions::ActionOutput::default())
                });
            }
        }
    }

    /// Land what the live drain queued, at most [`FLUSH_BUDGET`] articles per
    /// call, so a big refresh never stalls a frame.
    pub(crate) fn flush_doc_writes(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(landing) = self.pending_landings.first_mut() else {
            return;
        };
        let rest = if landing.fetched.articles.len() > FLUSH_BUDGET {
            landing.fetched.articles.split_off(FLUSH_BUDGET)
        } else {
            Vec::new()
        };
        let fetched = if rest.is_empty() {
            self.pending_landings.remove(0).fetched
        } else {
            let fetched = landing.fetched.clone();
            landing.fetched = library::Refreshed {
                feed: fetched.feed.clone(),
                title: None,
                certificate_pin: None,
                articles: rest,
            };
            fetched
        };
        self.perform(doc, Library::REFRESHED.with(fetched));
    }

    /// Reconcile with the document: seed or adopt it on the first pass, then
    /// persist queued writes, drain portals and follow out-of-band changes.
    fn sync_document(&mut self, _doc: &AutoCommit) {
        self.background_poll_deadline.set(None);
        self.now = ui::clock::local_now().timestamp();
        self.synced = true;
        self.adopt_read_settings();
        self.drain_fetches_live();
        self.drain_export();
        self.drain_open_link();
    }

    // --- links --------------------------------------------------------------

    pub(crate) fn open_selected_link(&mut self) {
        if let Some(url) = self.open_article().map(|a| a.url.clone())
            && !url.is_empty()
        {
            self.open_url(&url);
        }
    }

    pub(crate) fn open_url(&mut self, url: &str) {
        let Some(url) = tilo_net::Url::parse(url.trim())
            .ok()
            .filter(|url| match url.scheme() {
                "http" | "https" | "gemini" => url.host_str().is_some(),
                "mailto" => !url.path().is_empty(),
                _ => false,
            })
        else {
            self.set_toast(ui::i18n!("feeds-open-link-invalid"));
            return;
        };
        self.open_link_rx = Some(ui::portal::send_open_uri(url.as_str()));
        self.set_toast(ui::i18n!("feeds-open-link-started"));
    }

    fn drain_open_link(&mut self) {
        let Some(rx) = &self.open_link_rx else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                LaunchOutcome::Failed("the opener stopped without a result".into())
            }
        };
        self.open_link_rx = None;
        if let LaunchOutcome::Failed(error) = outcome {
            self.set_toast(format!("{}: {error}", ui::i18n!("feeds-open-link-failed")));
        }
    }

    // --- requests -----------------------------------------------------------

    /// Apply what a retained control asked for.
    fn apply(&mut self, request: Request, doc: &mut ui::widget::Edits<'_>) {
        match request {
            Request::Show(filter) => self.set_filter(filter),
            Request::AddSubscription => self.open_add_subscription(),
            Request::NewFolder => self.open_add_folder(),
            Request::Refresh => self.refresh_all(),
            Request::MarkAllRead => self.mark_all_read(doc),
            Request::StarOpen => {
                if let Some(id) = self.selected.clone() {
                    self.toggle_star(&id, doc);
                }
            }
            Request::OpenOriginal => self.open_selected_link(),
            Request::ListMenu => self.open_list_menu(),
            Request::ArticleMenu => self.open_article_menu(),
            Request::FeedMenu(id, row, None) => self.open_feed_menu(id, row),
            Request::FeedMenu(id, row, Some(at)) => self.open_feed_menu_at(id, row, at),
            Request::SetFolderCollapsed(id, collapsed) => {
                let unchanged = self
                    .store
                    .folders
                    .get(&id)
                    .is_none_or(|folder| folder.collapsed == collapsed);
                if !unchanged {
                    self.perform(
                        doc,
                        Library::paths().folders.item(&id).collapsed.set(collapsed),
                    );
                }
            }
            Request::SetFont(font) => self.bump_setting(doc, |s| s.font = font),
            Request::SetPaper(paper) => self.bump_setting(doc, |s| s.theme = paper),
            Request::SetScale(percent) => {
                self.bump_setting(doc, |s| s.font_scale = (percent / 100.0) as f32);
            }
            // The root owns the reading-settings panel.
            Request::ReaderSettings => {}
        }
    }

    /// Show the item a search hit (or an agent's `open`) names: an article
    /// in its feed, a subscription's articles or a folder's.
    pub(crate) fn open_item(&mut self, id: &Id) -> Result<(), String> {
        if self.reveal_article(id) {
            return Ok(());
        }
        if self.store.feed(id).is_some() {
            self.set_filter(Filter::Feed(id.clone()));
            return Ok(());
        }
        if self.store.folder(id).is_some() {
            self.set_filter(Filter::Folder(id.clone()));
            return Ok(());
        }
        Err("no such article, subscription or folder".into())
    }

    /// Show `id` in its feed and open it; the read mark waits for the next
    /// pass that holds the document.
    fn reveal_article(&mut self, id: &Id) -> bool {
        let Some(feed_id) = self
            .store
            .article(id)
            .map(|article| article.feed_id.clone())
        else {
            return false;
        };
        self.set_filter(Filter::Feed(feed_id));
        self.selected = Some(id.clone());
        self.reader.scroll_to_top();
        if self.store.mark_read(id, true) {
            self.pending_reads.push(id.clone());
        }
        self.show_pane = Some(NavPane::Content);
        true
    }

    /// The reader's single-key shortcuts. Returns whether the key was used.
    fn handle_key(&mut self, key: &Key, modified: bool, doc: &mut ui::widget::Edits<'_>) -> bool {
        match key {
            Key::Named(NamedKey::ArrowDown) => self.move_selection(1, doc),
            Key::Named(NamedKey::ArrowUp) => self.move_selection(-1, doc),
            Key::Character(s) if !modified => match s.as_str() {
                "j" => self.move_selection(1, doc),
                "k" => self.move_selection(-1, doc),
                "m" => self.toggle_read(doc),
                "s" => {
                    if let Some(id) = self.selected.clone() {
                        self.toggle_star(&id, doc);
                    }
                }
                "t" => self.open_tag_editor(),
                "r" => self.refresh_all(),
                "a" => self.mark_all_read(doc),
                "n" => self.open_add_subscription(),
                "e" => self.export_opml(),
                "i" => self.request_import(),
                "1" => self.set_filter(Filter::All),
                "2" => self.set_filter(Filter::Unread),
                "3" => self.set_filter(Filter::Starred),
                _ => return false,
            },
            _ => return false,
        }
        true
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        if self.import_rx.is_none()
            && self.export_rx.is_none()
            && self
                .worker
                .as_ref()
                .is_none_or(|worker| worker.pending() == 0)
            && self.pending_landings.is_empty()
        {
            return None;
        }
        let deadline = self.background_poll_deadline.get().unwrap_or_else(|| {
            ui::time::now()
                .checked_add(std::time::Duration::from_millis(16))
                .unwrap_or_else(ui::time::now)
        });
        self.background_poll_deadline.set(Some(deadline));
        Some(deadline)
    }

    /// Whether background work still needs frames: outstanding fetches (the
    /// draw-time drain lands them), visible progress, queued writes, an open
    /// file chooser, a toast.
    pub(crate) fn needs_frames(&self) -> bool {
        self.worker.as_ref().is_some_and(|w| w.pending() > 0)
            || self.refresh.visible()
            || !self.pending_landings.is_empty()
            || self.import_rx.is_some()
            || self.export_rx.is_some()
            || self.toast_visible().is_some()
    }
}

/// Layouts at one window size before a capture stages its scenario.
const SETTLED_LAYOUTS: u32 = 3;

/// The header row's actions: icon buttons that ask the root for a request.
struct HeaderActions<'a> {
    core: &'a Shared,
    requests: &'a Requests,
}

impl HeaderActions<'_> {
    fn button(&self, id: &str, icon: Icon, label: &str, request: Request) -> HeaderButton {
        let requests = Rc::clone(self.requests);
        HeaderButton::new(
            Button::icon_only(id, icon, label)
                .on_effect(move || requests.borrow_mut().push(request.clone())),
            self.core,
        )
    }

    /// The library's commands, beside the sidebar toggle.
    fn sidebar(&self) -> Vec<Element> {
        vec![
            Box::new(
                self.button(
                    "feeds.refresh",
                    Icon::Refresh,
                    "Refresh all feeds",
                    Request::Refresh,
                )
                .enabled_when(|core| !core.store.feeds.is_empty() && !core.refresh.in_progress()),
            ),
            Box::new(self.button(
                "feeds.new-folder",
                Icon::FolderPlus,
                "New folder",
                Request::NewFolder,
            )),
            Box::new(self.button(
                "feeds.add",
                Icon::Plus,
                "Add subscription",
                Request::AddSubscription,
            )),
        ]
    }

    /// The article list's commands: search, mark all read, import and export.
    fn overview(&self) -> Vec<Element> {
        vec![
            Box::new(ui::search::search_button(
                ui::search::SearchRequest::this_app(),
            )),
            Box::new(
                self.button(
                    "feeds.mark-all-read",
                    Icon::Check,
                    "Mark all as read",
                    Request::MarkAllRead,
                )
                .enabled_when(|core| core.store.unread_count_for(&core.filter) > 0),
            ),
            Box::new(
                self.button(
                    "feeds.list-menu",
                    Icon::MoreHorizontal,
                    "More",
                    Request::ListMenu,
                )
                .anchor(Anchor::ListMenu),
            ),
        ]
    }

    /// The open article's commands.
    fn content(&self) -> Vec<Element> {
        let has_article = |core: &Core| core.open_article().is_some();
        vec![
            Box::new(
                self.button("feeds.star", Icon::Star, "Star article", Request::StarOpen)
                    .enabled_when(has_article)
                    .toggled_by(|core| core.open_article().is_some_and(|a| a.starred))
                    .named_by(|core| {
                        if core.open_article().is_some_and(|a| a.starred) {
                            "Remove star"
                        } else {
                            "Star article"
                        }
                    }),
            ),
            Box::new(
                self.button(
                    "feeds.open-original",
                    Icon::ExternalLink,
                    "Open original article",
                    Request::OpenOriginal,
                )
                .enabled_when(|core| core.open_article().is_some_and(|a| !a.url.is_empty())),
            ),
            Box::new(
                self.button(
                    "feeds.reader-settings",
                    Icon::Settings2,
                    "Reading settings",
                    Request::ReaderSettings,
                )
                .enabled_when(has_article)
                .anchor(Anchor::ReaderSettings),
            ),
            Box::new(
                self.button(
                    "feeds.article-menu",
                    Icon::MoreHorizontal,
                    "More",
                    Request::ArticleMenu,
                )
                .enabled_when(has_article)
                .anchor(Anchor::ArticleMenu),
            ),
        ]
    }
}

/// The root's nodes while a dialog or the reading settings are mounted
/// beside the shell. They join the tree so their controls take part in
/// focus order and native accessibility; both present themselves in the
/// window's top layer.
#[derive(Clone, Copy)]
struct OverlayMount {
    root: NodeId,
    shell: NodeId,
    dialog: Option<NodeId>,
    panel: Option<NodeId>,
}

/// The application root: the window shell around the reader's parts.
pub struct FeedsApp {
    core: Shared,
    requests: Requests,
    shell: NavShell,
    /// The column titles last handed to the shell.
    titles: (String, String),
    /// The reading settings, open under their header button.
    settings_panel: Option<Popover<Column>>,
    /// The open dialog.
    pub(crate) modal: Option<FeedsModal>,
    /// The dialog opened or closed: the retained tree must be rebuilt.
    overlays_dirty: bool,
    /// A screenshot scenario to stage once the shell has laid out.
    pending_scenario: Option<String>,
    /// How many layouts in a row gave the window the same rect.
    settled_layouts: u32,
    mount: Option<OverlayMount>,
    /// Items search hits and agents opened in the framework host.
    opened: Rc<RefCell<Vec<Id>>>,
}

impl Default for FeedsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedsApp {
    /// The live app: starts empty on the person's subscriptions and never
    /// fabricates feeds.
    pub fn new() -> Self {
        Self::build(false)
    }

    /// The populated build used only by the hermetic `--shots`/test path.
    pub fn new_demo() -> Self {
        Self::build(true)
    }

    fn build(demo: bool) -> Self {
        let core: Shared = Rc::new(RefCell::new(Core::new(demo)));
        let requests: Requests = Rc::default();
        let titles = {
            let core = core.borrow();
            (core.filter_title(), core.article_title())
        };
        let actions = HeaderActions {
            core: &core,
            requests: &requests,
        };
        let shell = NavShell::new()
            .with_id("feeds.shell")
            .sidebar(Box::new(SidebarNav::new(&core, &requests)))
            .overview(Box::new(ListPane::new(&core)))
            .content(Box::new(ReaderPane::new(&core)))
            .sidebar_header("", actions.sidebar())
            .overview_header(titles.0.clone(), actions.overview())
            .content_header(titles.1.clone(), actions.content());
        Self {
            core,
            requests,
            shell,
            titles,
            settings_panel: None,
            modal: None,
            overlays_dirty: false,
            pending_scenario: None,
            settled_layouts: 0,
            mount: None,
            opened: Rc::default(),
        }
    }

    /// What the framework calls with the item a search hit or an agent's
    /// `open` names: refused when the library holds no such item, shown on
    /// the next pass.
    pub(crate) fn opener(
        &self,
    ) -> impl FnMut(&Id) -> Result<(), ui::tilo_resource::Refusal> + 'static {
        let opened = Rc::clone(&self.opened);
        let core = Rc::clone(&self.core);
        move |id: &Id| {
            let core = core.borrow();
            let known = core.store.articles.contains(id)
                || core.store.feeds.contains(id)
                || core.store.folders.contains(id);
            if !known {
                return Err(ui::tilo_resource::Refusal::new(
                    "no such article, subscription or folder",
                ));
            }
            opened.borrow_mut().push(id.clone());
            Ok(())
        }
    }

    // --- capture staging ----------------------------------------------------

    /// Stage a screenshot scenario once the window has its final size:
    ///
    /// * `"sidebar-open"` — the sidebar drawer open over a narrow window.
    /// * `"detail"` — a narrow window showing the open article.
    /// * `"reader-settings"` — the reading settings under their button.
    /// * `"context-menu"` — a feed's menu on its sidebar row.
    pub fn stage(&mut self, scenario: &str) {
        self.pending_scenario = Some(scenario.to_string());
    }

    fn stage_pending_scenario(&mut self) {
        let Some(scenario) = self.pending_scenario.take() else {
            return;
        };
        match scenario.as_str() {
            "sidebar-open" => self.shell.open_sidebar(),
            "detail" => self.shell.show(NavPane::Content),
            "reader-settings" => {
                // This is capture-only staging; use the same personal preference
                // store the live reader follows, without changing the library.
                let mut core = self.core.borrow_mut();
                if let Err(error) = core.change_read_settings(|settings| {
                    settings.font = ReadFont::Serif;
                    settings.theme = ReadTheme::Sepia;
                }) {
                    core.set_toast(error);
                }
                drop(core);
                let ready = self
                    .core
                    .borrow()
                    .anchors
                    .get(&Anchor::ReaderSettings)
                    .is_some_and(|r| r.area() > 0.0);
                if !ready {
                    self.pending_scenario = Some(scenario);
                    return;
                }
                self.toggle_settings_panel();
            }
            "tags" => {
                let article = {
                    let core = self.core.borrow();
                    core.selected
                        .clone()
                        .or_else(|| core.store.articles.ids().next().cloned())
                };
                if let Some(article) = article {
                    self.core.borrow_mut().reveal_article(&article);
                    self.stage_tag_editor();
                    self.take_pending_modal();
                }
            }
            "context-menu" => {
                let row = self.core.borrow().first_feed_row.clone();
                let Some((id, rect)) = row else {
                    self.pending_scenario = Some(scenario);
                    return;
                };
                self.core.borrow_mut().open_feed_menu(id, rect);
            }
            _ => {}
        }
        self.core.borrow_mut().show_pane = None;
        self.shell.snap();
    }

    /// Open the open article's tag editor.
    pub fn stage_tag_editor(&mut self) {
        self.core.borrow_mut().open_tag_editor();
    }

    /// Merge the bundled sample OPML into the shown library, as right after
    /// an import.
    pub fn stage_post_import(&mut self) {
        let outlines = opml::outlines(&opml::parse(opml::sample_opml()));
        let mut minted = 0;
        self.core.borrow_mut().store.import(&outlines, || {
            minted += 1;
            Id::from_raw(format!("staged-import-{minted}"))
        });
    }

    /// A refresh of every feed partway through: some done, some updated,
    /// one failed.
    pub fn force_refresh_in_progress(&mut self) {
        let mut core = self.core.borrow_mut();
        let total = core.store.feeds.len();
        let done = total.div_ceil(2).saturating_sub(1);
        core.refresh = RefreshState {
            total,
            done,
            updated_feeds: done.saturating_sub(1),
            failed_feeds: done.min(1),
            new_items: done * 2,
            finished_at: None,
        };
    }

    pub fn set_read_theme(&mut self, theme: ReadTheme) {
        self.core.borrow_mut().settings.theme = theme;
    }

    pub fn set_read_font(&mut self, font: ReadFont) {
        self.core.borrow_mut().settings.font = font;
    }

    /// Give two feeds long titles, to check the sidebar elides them.
    pub fn stage_long_sidebar_titles(&mut self) {
        let long = [
            "International Space Station Daily Operations and Mission Updates",
            "The Exhaustively Comprehensive Front-End Engineering Weekly Digest",
        ];
        for ((_, f), title) in self.core.borrow_mut().store.feeds.iter_mut().zip(long) {
            f.title = title.to_string();
        }
    }

    // --- framework host ---------------------------------------------------------

    /// Attach the framework dispatcher when hosted as a resource surface.
    pub(crate) fn attach_framework(&self, dispatcher: ui::framework::Dispatcher<Library>) {
        self.core.borrow_mut().dispatcher = Some(dispatcher);
    }

    // --- observation --------------------------------------------------------------

    /// The open article.
    pub fn selected_article(&self) -> Option<Id> {
        self.core.borrow().selected.clone()
    }

    /// The place the article list shows.
    pub fn active_filter(&self) -> Filter {
        self.core.borrow().filter.clone()
    }

    /// The first-run prompt while there are no subscriptions.
    pub fn empty_onboarding_prompt(&self) -> Option<&'static str> {
        self.core
            .borrow()
            .store
            .feeds
            .is_empty()
            .then_some(crate::list_pane::EMPTY_LIBRARY_TITLE)
    }

    #[cfg(test)]
    pub(crate) fn core(&self) -> std::cell::Ref<'_, Core> {
        self.core.borrow()
    }

    #[cfg(test)]
    pub(crate) fn core_mut(&self) -> std::cell::RefMut<'_, Core> {
        self.core.borrow_mut()
    }

    #[cfg(test)]
    pub(crate) fn shell(&self) -> &NavShell {
        &self.shell
    }

    #[cfg(test)]
    pub(crate) fn shell_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    #[cfg(test)]
    pub(crate) fn settings_panel_open(&self) -> bool {
        self.settings_panel.as_ref().is_some_and(Popover::is_open)
    }

    // --- the loop -------------------------------------------------------------------

    /// Present the dialog the core asked for.
    fn take_pending_modal(&mut self) {
        let pending = self.core.borrow_mut().pending_modal.take();
        if let Some(modal) = pending {
            self.close_settings_panel();
            self.modal = Some(modal);
            self.overlays_dirty = true;
        }
        self.core.borrow_mut().modal_open = self.modal.is_some();
    }

    fn close_modal(&mut self) {
        self.modal = None;
        self.overlays_dirty = true;
        self.core.borrow_mut().modal_open = false;
    }

    /// Route an event to the open dialog and act on its callbacks. The dialog
    /// handles Escape, the scrim, its buttons, typing and the clipboard;
    /// Enter submits.
    fn route_modal_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::KeyboardInput { event: ke, .. } = event
            && ke.state == ElementState::Pressed
            && matches!(ke.logical_key, Key::Named(NamedKey::Enter))
        {
            self.submit_modal(&mut ctx.state);
            return;
        }
        if let Some(m) = self.modal.as_mut() {
            m.dialog.handle_detached(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                &mut ctx.state,
                ctx.path,
            );
        }
        self.drain_modal_actions(&mut ctx.edits());
    }

    fn drain_modal_actions(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some(modal) = self.modal.as_ref() else {
            return;
        };
        let (submit, cancel) = (modal.submit.replace(false), modal.cancel.replace(false));
        if submit {
            self.submit_modal(doc);
        } else if cancel {
            self.close_modal();
        }
    }

    /// Run the open dialog's primary action, then close it. An input dialog
    /// with an empty required field stays open.
    pub(crate) fn submit_modal(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let Some((intent, value)) = self
            .modal
            .as_ref()
            .map(|m| (m.intent.clone(), m.dialog.content().value().to_string()))
        else {
            return;
        };
        if self.core.borrow_mut().submit_dialog(intent, &value, doc) {
            self.close_modal();
        }
    }

    fn close_settings_panel(&mut self) {
        if self.settings_panel.take().is_some() {
            self.overlays_dirty = true;
        }
    }

    fn toggle_settings_panel(&mut self) {
        if self.settings_panel.is_some() {
            self.close_settings_panel();
            return;
        }
        let (anchor, settings) = {
            let core = self.core.borrow();
            if core.open_article().is_none() {
                return;
            }
            (
                core.anchors
                    .get(&Anchor::ReaderSettings)
                    .copied()
                    .unwrap_or_default(),
                core.settings,
            )
        };
        self.core.borrow_mut().menu = None;
        let window = Widget::get_rect(&self.shell);
        self.settings_panel = Some(
            Popover::with_content(reader_settings_panel(settings, &self.requests))
                .with_label("Reading settings")
                .anchored(anchor)
                .side(PopoverSide::Below)
                .with_viewport(window)
                .open(true),
        );
        self.overlays_dirty = true;
    }

    /// Apply what retained controls asked for, bring the screen a choice
    /// asked for to the front, and keep the column titles current.
    fn settle_controls(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let opened = std::mem::take(&mut *self.opened.borrow_mut());
        for id in opened {
            let _ = self.core.borrow_mut().open_item(&id);
        }
        let requests = std::mem::take(&mut *self.requests.borrow_mut());
        for request in requests {
            if matches!(request, Request::ReaderSettings) {
                self.toggle_settings_panel();
            } else {
                self.core.borrow_mut().apply(request, doc);
            }
        }
        self.take_pending_modal();
        if self
            .settings_panel
            .as_ref()
            .is_some_and(|panel| !panel.is_open())
            || self.core.borrow().open_article().is_none()
        {
            self.close_settings_panel();
        }
        let (pane, titles) = {
            let mut core = self.core.borrow_mut();
            (
                core.show_pane.take(),
                (core.filter_title(), core.article_title()),
            )
        };
        if let Some(pane) = pane {
            // Only a narrow window has screens to bring forward.
            if !self.shell.geometry().stack.is_empty() {
                self.shell.show(pane);
            }
            if pane != NavPane::Sidebar
                && self.shell.geometry().sidebar == SidebarPlacement::Drawer
                && self.shell.is_sidebar_open()
            {
                self.shell.close_sidebar();
            }
        }
        if titles.0 != self.titles.0 {
            self.shell.set_overview_title(titles.0.clone());
        }
        if titles.1 != self.titles.1 {
            self.shell.set_content_title(titles.1.clone());
        }
        self.titles = titles;
    }

    fn paint_toast(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let core = self.core.borrow();
        if let Some(message) = core.toast_visible() {
            paint_transient(
                scene,
                transform,
                text,
                theme,
                core.window,
                ToastAnchor::BottomRight,
                &ToastCard::new(message, ToastKind::Info),
            );
        }
    }

    fn handle_semantic(
        &self,
        target: &str,
        action: &A11yAction,
        doc: &mut ui::widget::Edits<'_>,
    ) -> Option<bool> {
        let mut core = self.core.borrow_mut();
        if let Some(index) = target
            .strip_prefix("feeds.menu:")
            .and_then(|i| i.parse::<usize>().ok())
        {
            let chosen = core
                .menu
                .as_ref()
                .and_then(|open| open.actions.get(index).cloned())?;
            if matches!(action, A11yAction::Activate) {
                core.menu = None;
                core.apply_menu_action(chosen, doc);
            }
            return Some(true);
        }
        if matches!(action, A11yAction::Focus | A11yAction::ScrollIntoView) {
            return (target.starts_with("feeds.article:")
                || target.starts_with("feeds.star:")
                || target.starts_with("feeds.reader-"))
            .then_some(true);
        }
        if !matches!(action, A11yAction::Activate | A11yAction::ToggleCheck) {
            return None;
        }
        let parse = |prefix: &str| {
            target
                .strip_prefix(prefix)
                .and_then(|raw| raw.parse::<Id>().ok())
        };
        if let Some(id) = parse("feeds.article:") {
            if core.store.article(&id).is_none() {
                return Some(false);
            }
            core.select_article(&id, doc);
        } else if let Some(id) = parse("feeds.star:") {
            if core.store.article(&id).is_none() {
                return Some(false);
            }
            core.toggle_star(&id, doc);
        } else if let Some(key) = target
            .strip_prefix("feeds.reader-tag:")
            .and_then(|raw| raw.parse::<crate::model::TagKey>().ok())
        {
            if !core.store.articles.values().any(|article| {
                article
                    .tags
                    .iter()
                    .any(|tag| crate::model::tag_key(tag) == key)
            }) {
                return Some(false);
            }
            core.set_filter(Filter::Tag(key));
        } else if let Some(index) = target
            .strip_prefix("feeds.reader-link:")
            .and_then(|raw| raw.parse::<usize>().ok())
        {
            let href = core
                .open_article()
                .and_then(|article| article.item().blocks.0.get(index))
                .and_then(|block| block.href.clone())?;
            core.open_url(&href);
        } else if target == "feeds.empty.add" {
            core.open_add_subscription();
        } else {
            return None;
        }
        Some(true)
    }
}

impl ui::widget::Decorator for FeedsApp {
    type Inner = NavShell;

    fn inner(&self) -> &NavShell {
        &self.shell
    }

    fn inner_mut(&mut self) -> &mut NavShell {
        &mut self.shell
    }

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::undo::LocalUndo>() {
            request.provide::<dyn ui::undo::LocalUndo>(self);
        } else {
            Widget::provide(&self.shell, request);
        }
    }
    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::undo::LocalUndo>() {
            request.provide::<dyn ui::undo::LocalUndo>(self);
        } else {
            Widget::provide_mut(&mut self.shell, request);
        }
    }
    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        for child in Widget::children_mut(self) {
            child.settle(edits, path);
        }
        self.core.borrow_mut().flush_doc_writes(edits);
        self.core.borrow_mut().drain_import(edits);
        let reads = std::mem::take(&mut self.core.borrow_mut().pending_reads);
        for id in reads {
            self.core.borrow_mut().perform(edits, read_edit(&id, true));
        }
        self.drain_modal_actions(edits);
        self.settle_controls(edits);
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document_ref(state, path);
            self.core.borrow_mut().sync_document(doc);
        }
        Widget::sync(&mut self.shell, state, path);
        if let Some(modal) = self.modal.as_mut() {
            modal.dialog.sync(state, path);
        }
    }

    fn needs_idle_sync(&self) -> bool {
        let core = self.core.borrow();
        !core.synced || core.needs_frames() || Widget::needs_idle_sync(&self.shell)
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        if self.settled_layouts >= SETTLED_LAYOUTS {
            self.stage_pending_scenario();
        }
        Widget::update(&mut self.shell, state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let shell = Widget::build_layout(&mut self.shell, taffy, theme, node_map);
        let floating = |taffy: &mut TaffyTree<()>, node: NodeId| {
            if let Ok(mut style) = taffy.style(node).cloned() {
                style.position = taffy::Position::Absolute;
                let _ = taffy.set_style(node, style);
            }
            node
        };
        let dialog = self.modal.as_mut().map(|modal| {
            let node = modal.dialog.build_layout(taffy, theme, node_map);
            floating(taffy, node)
        });
        let panel = self.settings_panel.as_mut().map(|panel| {
            let node = panel.build_layout(taffy, theme, node_map);
            floating(taffy, node)
        });
        let node = if dialog.is_some() || panel.is_some() {
            let children: Vec<NodeId> = std::iter::once(shell).chain(dialog).chain(panel).collect();
            let root = ui::widget::TaffyTreeExt::parent_node(
                taffy,
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &children,
            );
            self.mount = Some(OverlayMount {
                root,
                shell,
                dialog,
                panel,
            });
            root
        } else {
            self.mount = None;
            shell
        };
        // The hooks the host sweeps through the node map are this root's.
        ui::widget::decorator::register_node(node_map, node, self);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        match self.mount.filter(|mount| mount.root == node) {
            Some(mount) => {
                let l = taffy.layout(node).copied().unwrap_or_default();
                let inner = Point::new(
                    origin.x + f64::from(l.location.x),
                    origin.y + f64::from(l.location.y),
                );
                Widget::apply_layout(&mut self.shell, taffy, mount.shell, inner);
                if let (Some(modal), Some(dialog)) = (self.modal.as_mut(), mount.dialog) {
                    modal.dialog.apply_layout(taffy, dialog, inner);
                }
                if let (Some(panel), Some(node)) = (self.settings_panel.as_mut(), mount.panel) {
                    // The panel keeps its own air from the window's edges.
                    let window = Widget::get_rect(&self.shell);
                    panel.set_viewport(window);
                    panel.apply_layout(taffy, node, inner);
                }
            }
            None => Widget::apply_layout(&mut self.shell, taffy, node, origin),
        }
        let window = Widget::get_rect(&self.shell);
        let mut core = self.core.borrow_mut();
        self.settled_layouts = if core.window == window && window.area() > 0.0 {
            self.settled_layouts.saturating_add(1)
        } else {
            0
        };
        core.window = window;
        let bounds = core.menu_bounds();
        if let Some(open) = core.menu.as_mut() {
            open.menu.constrain_to(bounds);
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let modal = std::mem::take(&mut self.overlays_dirty);
        Widget::take_structure_dirty(&mut self.shell) || modal
    }

    /// With an overlay mounted the root is a container of the shell and the
    /// overlays, in the order of its layout children; otherwise it is the
    /// shell.
    fn children(&self) -> Vec<&dyn Widget> {
        let Some(mount) = self.mount else {
            return Widget::children(&self.shell);
        };
        let mut out: Vec<&dyn Widget> = vec![&self.shell];
        if let (Some(modal), Some(_)) = (&self.modal, mount.dialog) {
            out.push(&modal.dialog);
        }
        if let (Some(panel), Some(_)) = (&self.settings_panel, mount.panel) {
            out.push(panel);
        }
        out
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in ui::widget::Decorator::children(self) {
            visitor(child);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let Some(mount) = self.mount else {
            return Widget::children_mut(&mut self.shell);
        };
        let mut out: Vec<&mut (dyn Widget + '_)> = vec![&mut self.shell];
        if let (Some(modal), Some(_)) = (self.modal.as_mut(), mount.dialog) {
            out.push(&mut modal.dialog);
        }
        if let (Some(panel), Some(_)) = (self.settings_panel.as_mut(), mount.panel) {
            out.push(panel);
        }
        out
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Results land live, every frame, without a document in hand.
        self.core.borrow_mut().drain_fetches_live();
        Widget::draw(&mut self.shell, scene, transform, text, theme, presence);
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
        self.core.borrow_mut().drain_fetches_live();
        Widget::draw_with_presence(
            &mut self.shell,
            scene,
            transform,
            text,
            theme,
            presence,
            permissions,
        );
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
        if let Some(panel) = self.settings_panel.as_mut() {
            panel.draw_overlay(scene, transform, text, theme, presence);
        }
        {
            let mut core = self.core.borrow_mut();
            if let Some(open) = core.menu.as_mut() {
                open.menu.layout_within(text, theme);
                Widget::draw_overlay(&mut open.menu, scene, transform, text, theme, presence);
            }
        }
        self.paint_toast(scene, transform, text, theme);
        if let Some(modal) = self.modal.as_mut() {
            modal
                .dialog
                .draw_overlay(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.core.borrow_mut().pointer =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        self.core.borrow_mut().flush_doc_writes(&mut ctx.state);
        if self.modal.is_some() {
            self.route_modal_event(event, ctx);
            *ctx.handled = true;
            self.settle_controls(&mut ctx.edits());
            return;
        }
        if let Some(panel) = self.settings_panel.as_mut() {
            panel.handle_event(event, ctx);
            if !panel.is_open() {
                self.close_settings_panel();
            }
            self.settle_controls(&mut ctx.edits());
            return;
        }
        let menu_open = self.core.borrow().menu.is_some();
        // A right click on another feed replaces an open menu.
        let right_press = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            }
        );
        if menu_open && !right_press {
            self.core.borrow_mut().route_menu_event(event, ctx);
            if !matches!(event, WindowEvent::CursorMoved { .. }) {
                *ctx.handled = true;
            }
            self.settle_controls(&mut ctx.edits());
            return;
        }
        if menu_open {
            self.core.borrow_mut().menu = None;
        }
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
        {
            let modified = {
                let m = ctx.modifiers.state();
                m.control_key() || m.super_key() || m.alt_key()
            };
            if self
                .core
                .borrow_mut()
                .handle_key(logical_key, modified, &mut ctx.edits())
            {
                *ctx.handled = true;
            }
        }
        self.settle_controls(&mut ctx.edits());
    }

    fn modal_capture(&self) -> Option<Rect> {
        if let Some(modal) = self.modal.as_ref() {
            return Some(modal.dialog.dialog_rect());
        }
        self.settings_panel
            .as_ref()
            .filter(|panel| panel.is_open())
            .map(Popover::rect)
            .or_else(|| Widget::modal_capture(&self.shell))
    }

    fn needs_animation_frame(&self) -> bool {
        self.pending_scenario.is_some()
            || self.core.borrow().needs_frames()
            || Widget::needs_animation_frame(&self.shell)
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.core
            .borrow()
            .next_wake_deadline()
            .or_else(|| Widget::next_wake_deadline(&self.shell))
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        match &self.modal {
            Some(modal) => Widget::ime_cursor_rect(&modal.dialog),
            None => Widget::ime_cursor_rect(&self.shell),
        }
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        match &self.modal {
            Some(modal) => Widget::ime_purpose(&modal.dialog),
            None => Widget::ime_purpose(&self.shell),
        }
    }

    fn accessibility(&self) -> Option<AccessNode> {
        if self.modal.is_some() {
            return None;
        }
        let mut children: Vec<AccessNode> = Vec::new();
        children.extend(Widget::accessibility(&self.shell));
        let core = self.core.borrow();
        if let Some(open) = &core.menu {
            let items = open
                .actions
                .iter()
                .enumerate()
                .filter_map(|(index, action)| {
                    let item = open.menu.items().get(index)?;
                    Some(
                        AccessNode::new(Role::MenuItem)
                            .with_name(item.label().unwrap_or_default().to_string())
                            .with_action_target(format!("feeds.menu:{index}"))
                            .with_hashed_key(action.target())
                            .with_bounds(open.menu.item_rect(index).unwrap_or(Rect::ZERO))
                            .with_state(
                                AccessState::new()
                                    .focusable(true)
                                    .focused(open.menu.hovered() == Some(index))
                                    .disabled(!item.is_enabled()),
                            ),
                    )
                })
                .collect();
            children.push(
                AccessNode::new(Role::Menu)
                    .with_name(match open.kind {
                        MenuKind::Feed(_) => "Feed actions",
                        MenuKind::List => "More",
                        MenuKind::Article => "Article actions",
                    })
                    .with_children(items),
            );
        }
        if core.refresh.visible() {
            children.push(
                AccessNode::new(Role::ProgressBar)
                    .with_name(core.refresh.status())
                    .with_live(Politeness::Polite),
            );
        }
        if let Some(message) = core.toast_visible() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(message.to_string())
                    .with_live(Politeness::Polite),
            );
        }
        Some(
            AccessNode::new(Role::Window)
                .with_name("Feeds")
                .with_semantic_key("feeds.application")
                .with_bounds(core.window)
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
        edits: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        if let Some(handled) = self.handle_semantic(target, action, edits) {
            self.settle_controls(edits);
            return handled;
        }
        let handled = Widget::perform_semantic_action(&mut self.shell, target, action, edits, path);
        self.settle_controls(edits);
        handled
    }

    fn handle_local_undo_request(&mut self, _request: ui::undo::UndoRequest) -> bool {
        self.core.borrow().menu.is_some() || self.settings_panel.is_some()
    }

    fn name(&self) -> &'static str {
        "FeedsApp"
    }
}

impl ui::undo::LocalUndo for FeedsApp {
    fn handle_local_undo_request(&mut self, _request: ui::undo::UndoRequest) -> bool {
        self.core.borrow().menu.is_some() || self.settings_panel.is_some()
    }
}

#[cfg(test)]
mod tests;

impl Core {
    pub(crate) fn follow_read_settings(&mut self, setting: ui::framework::Setting<ReadSettings>) {
        self.settings = setting.get().clamped();
        self.read_settings = Some(setting);
    }
    fn adopt_read_settings(&mut self) {
        if let Some(setting) = &self.read_settings {
            let stored = setting.state().with(|s| s.clamped());
            if stored != self.settings {
                self.settings = stored;
            }
        }
    }
    pub(crate) fn change_read_settings(
        &mut self,
        change: impl FnOnce(&mut ReadSettings),
    ) -> Result<(), String> {
        let mut s = self.settings;
        change(&mut s);
        let s = s.clamped();
        if let Some(setting) = &self.read_settings {
            setting
                .set(s)
                .map_err(|error| format!("Couldn't save the reading settings: {error}"))?;
        }
        self.settings = s;
        Ok(())
    }
}

impl FeedsApp {
    pub(crate) fn present_library(&self, store: Library) {
        let mut core = self.core.borrow_mut();
        let first = !core.synced;
        core.store = store;
        core.arrive();
        // A feed shown on its own that is gone (unsubscribed here or by a
        // collaborator) leaves the list on everything.
        if matches!(&core.filter, Filter::Feed(id) if core.store.feed(id).is_none()) {
            core.set_filter(Filter::All);
        }
        core.ensure_selection();
        if first && core.selected.is_none() {
            core.select_featured();
        }
        core.synced = true;
    }
    pub(crate) fn attach_dispatcher(&self, dispatcher: ui::framework::Dispatcher<Library>) {
        self.attach_framework(dispatcher);
    }
    pub(crate) fn follow_read_settings(&self, setting: ui::framework::Setting<ReadSettings>) {
        self.core.borrow_mut().follow_read_settings(setting);
    }
    pub(crate) fn change_read_settings(
        &self,
        change: impl FnOnce(&mut ReadSettings),
    ) -> Result<(), String> {
        self.core.borrow_mut().change_read_settings(change)
    }
    #[cfg(test)]
    pub(crate) fn read_settings_for_test(&self) -> ReadSettings {
        self.core.borrow().settings
    }
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        if let Some(name) = scenario {
            self.stage(name);
        }
    }
    pub(crate) fn refresh_for(&self, reply: ui::framework::EffectReply) {
        let mut core = self.core.borrow_mut();
        core.refresh_all();
        core.refresh_reply = Some(reply);
    }
    pub(crate) fn refresh_unavailable(&self) -> Option<(&'static str, &'static str)> {
        let core = self.core.borrow();
        if core.store.feeds.is_empty() {
            Some(("no_subscriptions", "There are no subscriptions to refresh"))
        } else if core.refresh.in_progress() {
            Some(("refreshing", "A refresh is already running"))
        } else {
            None
        }
    }
    pub(crate) fn request_import(&self) {
        self.core.borrow_mut().request_import();
    }
    pub(crate) fn export_opml(&self) {
        self.core.borrow_mut().export_opml();
    }
    /// Show the articles tagged `tag`.
    pub(crate) fn reveal_tag(&self, tag: &str) -> Result<(), String> {
        let mut core = self.core.borrow_mut();
        if !core.store.has_tag(tag) {
            return Err("no article carries that tag".into());
        }
        core.set_filter(crate::resource::tag_filter(tag));
        Ok(())
    }
}
