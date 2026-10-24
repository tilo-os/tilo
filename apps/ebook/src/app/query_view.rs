//! Search in Books.
//!
//! The window bar's field turns the shelf, or the open book, into a query
//! view of what Books holds: the books, their chapters, the reader's
//! highlights and notes, and every passage of their prose. It reads the
//! same entries the platform search does ([`Item::from_entry`]), so a hit
//! here and a hit there are one thing. Below the library, Project
//! Gutenberg's catalogue answers too ([`crate::store`]).
//!
//! Searching from inside a book starts with the Book chip on that book: its
//! passages, highlights and chapters. Clearing the chip widens the search to
//! the library. A passage opens the book at it. Escape returns to the shelf
//! or the page exactly as they were.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use ui::TextRole;
use ui::background::Pumped;
use ui::framework::page_part::PagePart;
use ui::framework::views::{
    Chip, FacetMenu, FacetOffer, FieldDecl, FieldRole, FilterKind, Form, Item, KindDecl, OpenFacet,
    QueryEngine, QuerySpec, RemoteError, RemoteSection, RemoteSource, ResultGroup, facet_bar,
};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::widget::{
    ButtonFace, ButtonRole, ControlSize, Interaction, ListRowFace, MediaArt, MediaAspect,
    MediaSize, RowLeading, ScrollMetrics, card_column_width, card_columns,
};
use vello::kurbo::{Point, Rect};
use vello::peniko::{BlendMode, Fill};

use super::{Action, BooksApp, ShelfCard, ShelfGrid, View, paint_scroll_thumb};
use crate::library_index::{LibraryIndex, Read, Standing};
use crate::model::Book;
use crate::search_provider::{
    ANNOTATION_KIND, AUTHOR_FACET, BOOK_FACET, BOOK_KIND, BookSearch, CHAPTER_KIND, LOCATION_KIND,
};
use crate::store::{STORE_BOOK, StoreBook};

/// A book on the shelf, or one a collaborator shared.
pub(crate) const BOOK: &str = "books.book";
/// A chapter of a book.
pub(crate) const CHAPTER: &str = "books.chapter";
/// A highlight, a note or a bookmark.
pub(crate) const ANNOTATION: &str = "books.annotation";
/// A passage of a book's prose that a search matched.
pub(crate) const PASSAGE: &str = "books.passage";

fn author() -> FieldDecl {
    FieldDecl::new(AUTHOR_FACET, "Author")
        .searchable()
        .filter(FilterKind::Value)
}

fn in_book() -> FieldDecl {
    FieldDecl::new(BOOK_FACET, "Book").filter(FilterKind::Value)
}

fn place() -> FieldDecl {
    FieldDecl::new("place", "Where").role(FieldRole::Subtitle)
}

/// What Books holds, as its views and its search read it. Each type is
/// named by the group its search entries carry.
pub(crate) fn kinds() -> Vec<KindDecl> {
    vec![
        KindDecl::new(BOOK, "Book", BOOK_KIND, Form::Tile)
            .field(FieldDecl::title("title", "Title"))
            .field(author().role(FieldRole::Subtitle))
            .field(in_book()),
        KindDecl::new(CHAPTER, "Chapter", CHAPTER_KIND, Form::Row)
            .field(FieldDecl::title("title", "Chapter"))
            .field(place())
            .field(author())
            .field(in_book()),
        KindDecl::new(ANNOTATION, "Annotation", ANNOTATION_KIND, Form::Row)
            .field(FieldDecl::title("title", "Annotation"))
            .field(place())
            .field(author())
            .field(in_book()),
        KindDecl::new(PASSAGE, "Passage", LOCATION_KIND, Form::Row)
            .field(FieldDecl::title("title", "Passage"))
            .field(place())
            .field(FieldDecl::new("excerpt", "Passage").role(FieldRole::Excerpt))
            .field(author())
            .field(in_book()),
        KindDecl::new(STORE_BOOK, "Book", "Books", Form::Row)
            .field(FieldDecl::title("title", "Title"))
            .field(author().role(FieldRole::Subtitle)),
    ]
}

/// The query view's state: what is typed and chosen, the engine answering
/// it, and where the person is in the results.
pub(super) struct BookQuery {
    pub(super) engine: QueryEngine,
    /// What the window bar's field holds.
    pub(super) input: String,
    /// The chips chosen under the field, rather than typed.
    pub(super) picked: Vec<Chip>,
    /// The window's search is open.
    pub(super) open: bool,
    /// The view Escape returns to.
    pub(super) returns_to: View,
    pub(super) scroll: f64,
    max_scroll: f64,
    /// Where the results scroll, as laid out last frame.
    viewport: Rect,
    /// Which build of the library's search entries the engine holds.
    synced: Option<u64>,
    /// The shelf's books by the address their result carries.
    shelved: HashMap<String, usize>,
    pub(super) chips: PagePart<Action>,
    pub(super) facets: FacetMenu,
    /// The chips' offers, for the results they were counted among.
    offers: Option<((u64, usize), Vec<FacetOffer>)>,
    /// The result the keyboard is on.
    pub(super) cursor: Option<usize>,
    /// The results laid out last frame, top down.
    rows: Vec<QueryRow>,
    /// The store's placeholder steps as painted last: a deadline repaints
    /// when one moves on.
    painted_steps: Vec<Option<ui::arriving::Step>>,
    /// A search a capture staged, opened once the library's search entries
    /// are built: the view it is typed over, and what is typed.
    pub(super) staged: Option<(View, String)>,
    /// A capture's or a test's executor: what the engine runs off the UI
    /// thread waits for a frame (or [`BooksApp::settle_query`]) to pump it.
    pumped: Option<Pumped>,
    /// The store's covers, fetched off the UI thread as their rows show.
    covers: ui::ImageStore,
    /// The index of the library's text, built here for the books here and
    /// read back for the books elsewhere.
    index: LibraryIndex,
    /// What of the library the index was last brought up to date with.
    indexed: Option<u64>,
    /// A passage opened in a book whose text is on its way: the book, its
    /// chapter and its paragraph.
    pub(super) reveal_on_arrival: Option<(String, usize, usize)>,
    /// When it was opened.
    reveal_since: ui::time::Instant,
}

/// What a book's tile says while its text waits for a device that has it.
const WAITING_STATUS: &str = "Waiting for a device that has it";
/// What it adds for a screen reader.
const WAITING_DETAIL: &str = "Opens when a device that has it is online.";
/// What a book's tile says when none of the connected devices has its text.
const UNAVAILABLE_STATUS: &str = "None of your online devices has this book.";

/// One result as laid out: where it is, what opening it does, and what it
/// is called.
struct QueryRow {
    rect: Rect,
    action: Action,
    /// What the result is addressed by: its anchor, or the store book's id.
    id: String,
    label: String,
}

impl BookQuery {
    /// The search of a capture or a test: the sample store, and the
    /// library's prose indexed when a frame pumps it.
    pub(super) fn hermetic() -> Self {
        let pumped = Pumped::new();
        let engine = QueryEngine::new(kinds())
            .with_background(pumped.background())
            .with_remote(Arc::new(crate::store::SampleStore));
        // Nothing goes over the network: the store's rows keep their
        // placeholder art.
        let offline: ui::Fetcher = Arc::new(|_: &str| None);
        let mut query = Self::with_engine(engine, &offline);
        query.index = LibraryIndex::default().with_background(pumped.background());
        query.pumped = Some(pumped);
        query
    }

    /// The live search: Project Gutenberg from the device's copy of its
    /// catalogue, or over the network, waking `wake`'s window when it
    /// answers.
    pub(super) fn live(
        wake: ui::WakeHandle,
        catalogue: Arc<crate::catalogue::DeviceCatalogue>,
    ) -> Self {
        let store: Arc<dyn RemoteSource> =
            Arc::new(crate::store::Gutenberg::with_catalogue(catalogue));
        let mut engine = QueryEngine::new(kinds()).with_remote(store);
        engine.set_waker(wake.clone());
        let mut query = Self::with_engine(engine, &crate::store::cover_fetcher());
        query.index.set_waker(wake);
        query
    }

    fn with_engine(engine: QueryEngine, covers: &ui::Fetcher) -> Self {
        Self {
            engine,
            input: String::new(),
            picked: Vec::new(),
            open: false,
            returns_to: View::Library,
            scroll: 0.0,
            max_scroll: 0.0,
            viewport: Rect::ZERO,
            synced: None,
            shelved: HashMap::new(),
            chips: PagePart::new("books.search."),
            facets: FacetMenu::default(),
            offers: None,
            cursor: None,
            rows: Vec::new(),
            painted_steps: Vec::new(),
            staged: None,
            pumped: None,
            covers: ui::ImageStore::new(covers),
            index: LibraryIndex::default(),
            indexed: None,
            reveal_on_arrival: None,
            reveal_since: ui::time::now(),
        }
    }

    /// The library's index as this device holds it.
    #[cfg(test)]
    pub(super) fn index_mut(&mut self) -> &mut LibraryIndex {
        &mut self.index
    }

    /// Scroll the results by `dy`; whether they moved.
    pub(super) fn scroll_by(&mut self, dy: f64) -> bool {
        let before = self.scroll;
        self.scroll = (self.scroll + dy).clamp(0.0, self.max_scroll);
        (self.scroll - before).abs() > 0.0
    }

    /// What the result addressed by `id` is called, for its accessible
    /// name.
    pub(super) fn label_of(&self, id: &str) -> Option<&str> {
        self.rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.label.as_str())
    }
}

impl BooksApp {
    /// The query the field and the chips ask together.
    fn query_spec(&self) -> QuerySpec {
        QuerySpec::parse(
            &self.query.input,
            &self.query.picked,
            self.query.engine.kinds(),
        )
    }

    /// Hand the engine the library's search entries when they were built
    /// again since it last took them.
    pub(super) fn sync_query_local(&mut self) {
        if self.query.synced == Some(self.search_revision) {
            return;
        }
        self.query.synced = Some(self.search_revision);
        let kinds = kinds();
        let items: Vec<Item> = self
            .book_search
            .entries()
            .iter()
            .filter_map(|entry| Item::from_entry(&kinds, entry))
            .collect();
        self.query.shelved = self
            .library
            .iter()
            .enumerate()
            .map(|(index, book)| (crate::resource::book_place_resource(&book.id), index))
            .collect();
        self.query.engine.sync_local(items);
    }

    /// Bring the library's index up to date with `entries`: build the
    /// segments of the books here that have none, read back those of the
    /// books elsewhere, and search what is read back; take in a book a hit
    /// was opened in once it arrives.
    pub(super) fn sync_library_index(&mut self, entries: &[crate::model::LibraryEntry]) {
        use std::hash::{Hash as _, Hasher as _};
        // A book that arrived joins the shelf's books here before the shelf
        // is brought up to date, so it is never on it twice.
        if let Some((id, read)) = self.query.index.take_fetched() {
            self.take_fetched_book(&id, read);
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for entry in entries {
            entry.id.hash(&mut hasher);
            entry.title.hash(&mut hasher);
            entry.author.hash(&mut hasher);
            entry.subject.hash(&mut hasher);
            entry.index.hash().hash(&mut hasher);
        }
        for book in self.library.iter() {
            book.id.hash(&mut hasher);
        }
        let may_index = self.may_index();
        may_index.hash(&mut hasher);
        let key = hasher.finish();
        self.query.index.read_arrived();
        let arrived = self.query.index.take_loaded();
        let changed = self.query.indexed != Some(key);
        if changed {
            self.query.indexed = Some(key);
            self.elsewhere = Arc::new(books_elsewhere(entries, &self.library));
            // A segment built here is kept in the content store with the
            // book's row; without one it would not reach other devices. A
            // participant who may not keep it there builds none: the books
            // here are searched from their own text.
            if self.content.is_some() && may_index {
                self.query.index.build_missing(&self.library, entries);
            }
            let here: std::collections::HashSet<&str> =
                self.library.iter().map(|book| book.id.as_str()).collect();
            let local = |id: &str| here.contains(id);
            self.query
                .index
                .load_missing(entries, &local, self.content.as_ref());
        }
        if arrived || changed {
            let here: std::collections::HashSet<&str> =
                self.library.iter().map(|book| book.id.as_str()).collect();
            let local = |id: &str| here.contains(id);
            let source = self.query.index.source(entries, &local);
            self.query.engine.set_local_source(Some(source));
            self.dirty = true;
        }
        self.reveal_arrived();
    }

    /// What `book`'s tile says of its text while it is being opened and is
    /// neither here nor on its way, and what a screen reader hears besides.
    /// It follows the devices as they come and go.
    pub(super) fn shelf_status(&self, book: &str) -> Option<(&'static str, Option<&'static str>)> {
        let opening = self.query.reveal_on_arrival.as_ref()?;
        if opening.0 != book {
            return None;
        }
        let (fetching, standing) = self.query.index.fetching()?;
        if fetching != book {
            return None;
        }
        match standing {
            Standing::Fetching => None,
            Standing::Waiting => Some((WAITING_STATUS, Some(WAITING_DETAIL))),
            Standing::Unavailable => Some((UNAVAILABLE_STATUS, None)),
        }
    }

    /// Take in what fetching the book `id` came to: the book on the shelf,
    /// or why it is not coming, said plainly with what will bring it.
    fn take_fetched_book(&mut self, id: &str, read: Read<crate::content::LoadedBook>) {
        let message = match read {
            Read::Here(loaded) => {
                if !self.library.iter().any(|book| book.id == loaded.book.id) {
                    self.library_paths
                        .insert(loaded.book.id.clone(), loaded.path);
                    Arc::make_mut(&mut self.library).push(Arc::new(loaded.book));
                }
                None
            }
            // Still to come: where it stands is followed, not ended.
            Read::OnItsWay | Read::Waiting | Read::Unavailable => None,
            Read::Unreadable => Some("This book's text could not be read."),
        };
        if let Some(message) = message {
            tracing::debug!(book = %id, %message, "books: a book opened here did not arrive");
            if self
                .query
                .reveal_on_arrival
                .as_ref()
                .is_some_and(|(book, ..)| book == id)
            {
                self.query.reveal_on_arrival = None;
            }
            self.report_failure(message.to_owned());
        }
        self.dirty = true;
    }

    /// The shelf: the books here, then those whose text is elsewhere.
    pub(super) fn shelf_books(&self) -> Arc<Vec<Arc<Book>>> {
        if self.elsewhere.is_empty() {
            return Arc::clone(&self.library);
        }
        Arc::new(
            self.library
                .iter()
                .chain(self.elsewhere.iter())
                .cloned()
                .collect(),
        )
    }

    /// The book at shelf index `index`: one here, or past them one whose
    /// text is elsewhere.
    pub(super) fn shelf_book(&self, index: usize) -> Option<&Arc<Book>> {
        self.library
            .get(index)
            .or_else(|| self.elsewhere.get(index.checked_sub(self.library.len())?))
    }

    /// Whether the library's row says a copy of `book` is in the shared
    /// workspace.
    pub(super) fn shared_in_workspace(&self, book: &str) -> bool {
        self.record
            .entries
            .values()
            .any(|entry| entry.id == book && entry.shared)
    }

    /// Since when the text of `book` has been on its way here, while it is:
    /// fetched from a connected device, or downloaded from the workspace.
    pub(super) fn arriving(&self, book: &str) -> Option<ui::time::Instant> {
        if self
            .query
            .index
            .fetching()
            .is_some_and(|(fetching, _)| fetching == book)
        {
            return self.query.index.arriving(book);
        }
        self.query
            .reveal_on_arrival
            .as_ref()
            .filter(|(waiting, ..)| waiting == book)
            .map(|_| self.query.reveal_since)
    }

    /// Whether a book's text is on its way, its card's bar moving. Not
    /// while it waits for a device that has it: nothing moves then.
    pub(super) fn awaiting_arrival(&self) -> bool {
        self.query
            .reveal_on_arrival
            .as_ref()
            .is_some_and(|(book, ..)| self.arriving(book).is_some())
    }

    /// Open the passage a hit named once its book is here.
    pub(super) fn reveal_arrived(&mut self) {
        let Some((book, chapter, block)) = self.query.reveal_on_arrival.clone() else {
            return;
        };
        if !self.library.iter().any(|open| open.id == book) {
            return;
        }
        self.query.reveal_on_arrival = None;
        if let Err(error) = self.reveal(&book, chapter, 0, Some(block)) {
            tracing::debug!(%book, %error, "books: a passage opened elsewhere is not in the book");
        }
        self.record_progress();
        self.dirty = true;
    }

    /// Whether this participant may keep a book's index with its row.
    fn may_index(&self) -> bool {
        self.dispatch.may_invoke(&crate::resource::index_book(
            "",
            crate::resource::StoredContent {
                hash: String::new(),
                size: 0,
            },
        ))
    }

    /// Keep the segments built since the last frame in the content store,
    /// each joining its book's row.
    pub(super) fn keep_built_segments(&mut self, doc: &mut ui::widget::Edits<'_>) {
        let built = self.query.index.take_built();
        let Some(store) = self.content.clone() else {
            return;
        };
        if built.is_empty() || !self.may_index() {
            return;
        }
        for (book, segment) in built {
            match store.put(&segment) {
                Ok(hash) => {
                    let size = u64::try_from(segment.len()).unwrap_or(u64::MAX);
                    self.edit(
                        doc,
                        crate::resource::index_book(
                            &book,
                            crate::resource::StoredContent { hash, size },
                        ),
                    );
                }
                Err(error) => tracing::debug!(%book, %error, "books: an index could not be kept"),
            }
        }
    }

    /// Open the passage `anchor` names in a book whose text is elsewhere:
    /// fetch the book, from the workspace or the content store, and land on
    /// the passage once it is here. Whether the anchor named such a book.
    fn open_elsewhere(&mut self, anchor: &str) -> bool {
        let target = parse_location(anchor).or_else(|| {
            // A book itself opens where its reader left off.
            let entry = self
                .record
                .entries
                .values()
                .find(|entry| crate::resource::book_place_resource(&entry.id) == anchor)?;
            let progress = self.record.progress_of(&entry.id);
            Some((entry.id.clone(), progress.chapter, progress.block))
        });
        let Some((book, chapter, block)) = target else {
            return false;
        };
        if self.library.iter().any(|open| open.id == book) {
            return false;
        }
        let Some(entry) = self
            .record
            .entries
            .values()
            .find(|entry| entry.id == book)
            .cloned()
        else {
            return false;
        };
        self.query.reveal_on_arrival = Some((book.clone(), chapter, block));
        self.query.reveal_since = ui::time::now();
        if entry.shared && entry.content.is_empty() {
            self.dispatch
                .effect(&crate::resource::BooksEffect::Download { book });
        } else if !self.query.index.fetch(&entry, self.content.as_ref()) {
            self.query.reveal_on_arrival = None;
            return false;
        }
        self.dirty = true;
        true
    }

    /// Whether the search is still answering what a capture shows: a
    /// staged search not yet typed, or an answer on its way.
    pub(super) fn query_busy(&self) -> bool {
        self.query.staged.is_some()
            || (self.query.open
                && (self.query.engine.results().pending() || self.query.index.reading()))
    }

    /// Take in what the library and the store sent since the last frame.
    pub(super) fn poll_query(&mut self) {
        self.follow_catalogue_setting();
        // A staged search waits for the library's entries to be built.
        if self.search_worker.is_none()
            && self.search_fingerprint != 0
            && let Some((from, text)) = self.query.staged.take()
        {
            self.stage_query(from, &text);
        }
        // What runs off the UI thread runs whether or not the search is
        // open: the library's index and the books on their way are not the
        // search's alone.
        if let Some(pumped) = &self.query.pumped {
            pumped.run_pending();
        }
        if !self.query.open {
            return;
        }
        self.sync_query_local();
        if self.query.engine.poll() {
            self.query.offers = None;
            self.dirty = true;
        }
        let steps = self
            .query
            .engine
            .results()
            .placeholder_steps(ui::motion::frame_now());
        if steps != self.query.painted_steps {
            self.dirty = true;
        }
    }

    /// When the store's placeholder next moves on a step.
    pub(super) fn next_query_placeholder_step(
        &self,
        now: ui::time::Instant,
    ) -> Option<ui::time::Instant> {
        if !self.query.open {
            return None;
        }
        self.query.engine.results().next_placeholder_step(now)
    }

    /// Ask what the field and the chips say now.
    fn run_query(&mut self) {
        self.sync_query_local();
        if self.query.engine.set_query(self.query_spec()) {
            self.query.scroll = 0.0;
            self.query.cursor = None;
            self.query.offers = None;
        }
        self.dirty = true;
    }

    /// A chip was picked or cleared: the chosen chips are what it asks,
    /// and what is typed stays as typed.
    pub(super) fn on_set_query(&mut self, query: &QuerySpec) {
        self.query.picked = query
            .chips
            .iter()
            .filter(|chip| !chip.typed)
            .cloned()
            .collect();
        self.run_query();
    }

    /// Open the values of the filter `key` over its chip.
    pub(super) fn open_facet(&mut self, key: &str) {
        let Some(offer) = self
            .query
            .engine
            .facet_offers()
            .into_iter()
            .find(|offer| offer.key == key)
        else {
            return;
        };
        let anchor = self
            .query
            .chips
            .find(&format!("view-chip:{key}"))
            .unwrap_or_else(|| Rect::from_origin_size(self.cursor, (1.0, 1.0)));
        let query = self.query.engine.results().query.clone();
        let window = self.rect;
        self.query.facets.open(&offer, &query, anchor, window);
        self.dirty = true;
    }

    /// Close the search: back to the shelf or the page it was opened from,
    /// as they were.
    pub(super) fn close_query(&mut self) {
        self.query.open = false;
        self.query.input.clear();
        self.query.picked.clear();
        self.query.cursor = None;
        self.query.offers = None;
        self.query.engine.close();
        self.query.facets.menu_mut().close_at_once();
        if self.view == View::Search {
            self.view = self.query.returns_to;
        }
        self.dirty = true;
    }

    /// Open what a result names: the book, chapter, annotation or passage
    /// it is anchored at, as the platform search opens its hits.
    ///
    /// Opening is the person's own gesture, done at once like a click on
    /// the page; under an invocation, or for a collaborator's book that is
    /// fetched first and answers once its bytes land, it goes through the
    /// host like any caller's.
    pub(super) fn open_hit(&mut self, anchor: &str) {
        use crate::resource::BooksEffect;
        if self.open_elsewhere(anchor) {
            return;
        }
        let Some(effect) = self.book_search.opened(anchor) else {
            tracing::debug!(%anchor, "books: a search hit to open is gone");
            return;
        };
        if matches!(effect, BooksEffect::Download { .. }) || !self.dispatch.is_own_gesture(&effect)
        {
            self.dispatch.effect(&effect);
            return;
        }
        let entries = self.book_search.registry().to_vec();
        if let Err(error) = self.begin_effect(tilo_actions::InvocationId::new(), effect, &entries) {
            tracing::debug!(%anchor, %error, "books: a search hit did not open");
        }
        self.dirty = true;
    }

    /// Get the store's book `id`: fetch it and open it like a picked file.
    pub(super) fn get_from_store(&mut self, id: u64) {
        if let Err(error) = self.begin_get_from_store(id) {
            self.settle_import_failure(error, "ebook.store_unavailable");
        }
    }

    /// The keyboard in the results: Up and Down move between them, Enter
    /// opens the one it is on. Whether the key was the results'.
    pub(super) fn handle_query_key(
        &mut self,
        key: &winit::keyboard::Key,
        doc: &mut ui::widget::Edits<'_>,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        let count = self.query.rows.len();
        match key {
            Key::Named(NamedKey::ArrowDown) if count > 0 => {
                self.query.cursor = Some(
                    self.query
                        .cursor
                        .map_or(0, |at| at.saturating_add(1).min(count - 1)),
                );
            }
            Key::Named(NamedKey::ArrowUp) if count > 0 => {
                self.query.cursor = Some(self.query.cursor.map_or(0, |at| at.saturating_sub(1)));
            }
            Key::Named(NamedKey::Enter) => {
                let Some(action) = self
                    .query
                    .cursor
                    .and_then(|at| self.query.rows.get(at))
                    .map(|row| row.action.clone())
                else {
                    return false;
                };
                self.dispatch(action, doc);
            }
            _ => return false,
        }
        self.reveal_query_cursor();
        self.dirty = true;
        true
    }

    /// Scroll the result the keyboard is on into view.
    fn reveal_query_cursor(&mut self) {
        let query = &mut self.query;
        let Some(row) = query.cursor.and_then(|at| query.rows.get(at)) else {
            return;
        };
        let view = query.viewport;
        if row.rect.y0 < view.y0 {
            query.scroll -= view.y0 - row.rect.y0;
        } else if row.rect.y1 > view.y1 {
            query.scroll += row.rect.y1 - view.y1;
        }
        query.scroll = query.scroll.clamp(0.0, query.max_scroll);
    }

    /// The search page: the chips, the library's results grouped by type,
    /// and the store's below them.
    pub(super) fn draw_query_view(&mut self, ctx: &mut PaintCtx<'_>) {
        self.query.rows.clear();
        let theme = ctx.theme;
        let band = self.draw_page_header(ctx);
        let height = ButtonFace::height(ControlSize::Regular, theme);
        let inset = f64::from(theme.tokens.spacing.s3);
        let back = Rect::new(
            band.x0 + inset,
            band.center().y - height / 2.0,
            band.x0 + inset + height,
            band.center().y + height / 2.0,
        );
        self.button(
            ctx,
            ButtonFace::icon_only(back, Icon::ArrowLeft, "Back"),
            Action::CloseSearch,
        );
        let body = Rect::new(self.rect.x0, band.y1, self.rect.x1, self.rect.y1);
        self.query.viewport = body;
        let rhythm = &theme.tokens.spacing.rhythm;
        let (gutter, section) = (f64::from(rhythm.group), f64::from(rhythm.section));
        let results = self.query.engine.results().clone();
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &body,
        );
        let span = (body.x0 + gutter, body.x1 - gutter);
        let found = results.order().len();
        let count = (!results.query.is_empty()).then(|| match found {
            1 => "1 result".to_owned(),
            n => format!("{n} results"),
        });
        // The title names what is shown: the words searched for, or the
        // chips alone when nothing is typed (their chips stand below).
        let title = match results.query.text.trim() {
            "" if results.query.chips.is_empty() => "Search".to_owned(),
            "" => ui::framework::views::KeptViews::default_name(
                &results.query,
                self.query.engine.kinds(),
            ),
            text => text.to_owned(),
        };
        let mut y = Self::draw_page_title(
            ctx,
            Point::new(span.0, body.y0 + gutter - self.query.scroll),
            &title,
            count.as_deref(),
        ) + gutter;
        y = self.query_chips(ctx, span, y, body, &results) + section;
        if results.query.is_empty() {
            y = Self::query_note(
                ctx,
                span,
                y,
                "Type to search your books, their pages and your notes.",
            );
        } else {
            if results.local.is_empty() {
                y = Self::query_note(ctx, span, y, "Nothing in your library matches.") + section;
            }
            for group in &results.local {
                y = self.query_group(ctx, span, body, y, group) + section;
            }
            let now = ui::motion::frame_now();
            self.query.painted_steps = results.placeholder_steps(now);
            for remote in &results.remote {
                y = self.query_remote(ctx, span, body, y, remote, now);
            }
        }
        let content = y + self.query.scroll - body.y0 + gutter;
        self.query.max_scroll = (content - body.height()).max(0.0);
        self.query.scroll = self.query.scroll.clamp(0.0, self.query.max_scroll);
        ctx.scene.pop_layer();
        paint_scroll_thumb(
            ctx,
            body,
            ScrollMetrics::new(content, body.height(), self.query.scroll),
        );
    }

    /// The filter chips under the field, the framework's own.
    fn query_chips(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        span: (f64, f64),
        y: f64,
        body: Rect,
        results: &ui::framework::views::QueryResults,
    ) -> f64 {
        let counted = (results.generation, results.order().len());
        if self
            .query
            .offers
            .as_ref()
            .is_none_or(|(key, _)| *key != counted)
        {
            self.query.offers = Some((counted, self.query.engine.facet_offers()));
        }
        let offers = self
            .query
            .offers
            .as_ref()
            .map(|(_, offers)| offers.clone())
            .unwrap_or_default();
        let query = results.query.clone();
        let key = format!("chips:{offers:?}:{query:?}");
        self.query.chips.update(key, move |asked| {
            let sink = Rc::clone(asked);
            let on_change: Rc<dyn Fn(QuerySpec)> =
                Rc::new(move |next| sink.borrow_mut().push(Action::SetQuery(next)));
            let sink = Rc::clone(asked);
            let open: OpenFacet =
                Rc::new(move |key| sink.borrow_mut().push(Action::OpenFacet(key.to_owned())));
            facet_bar(&offers, &query, &on_change, &open)
        });
        self.query.chips.host.place(ctx, span, y, body)
    }

    /// A quiet line where results would be.
    fn query_note(ctx: &mut PaintCtx<'_>, span: (f64, f64), y: f64, text: &str) -> f64 {
        let theme = ctx.theme;
        ctx.text_role(
            text,
            TextRole::Body,
            theme.text_dim,
            Point::new(span.0, y),
            Some(ui::num::f32_of_f64(span.1 - span.0)),
        );
        y + ctx.line_box(TextRole::Body)
    }

    /// A heading over a group or a section of results.
    fn query_heading(ctx: &mut PaintCtx<'_>, span: (f64, f64), y: f64, text: &str) -> f64 {
        let theme = ctx.theme;
        ctx.text_role(
            text,
            TextRole::Heading,
            theme.text_primary,
            Point::new(span.0, y),
            Some(ui::num::f32_of_f64(span.1 - span.0)),
        );
        y + ctx.line_box(TextRole::Heading) + f64::from(theme.tokens.spacing.rhythm.related)
    }

    /// One group of results under its heading: books as the shelf shows
    /// them, the rest as rows.
    fn query_group(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        span: (f64, f64),
        body: Rect,
        y: f64,
        group: &ResultGroup,
    ) -> f64 {
        let mut y = Self::query_heading(ctx, span, y, &group.label);
        // Every book is a card: one here as the shelf shows it, one whose
        // text is elsewhere with its generated cover.
        let (shelved, rows): (Vec<&Item>, Vec<&Item>) =
            group.items.iter().partition(|item| item.kind == BOOK);
        if !shelved.is_empty() {
            y = self.query_shelf(ctx, span, body, y, &shelved);
        }
        for item in rows {
            y = self.query_row(ctx, span, body, y, item);
        }
        if group.more > 0 {
            y = Self::query_note(ctx, span, y, &format!("{} more", group.more));
        }
        y
    }

    /// Books on the shelf, as the shelf's own cards.
    fn query_shelf(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        span: (f64, f64),
        body: Rect,
        y: f64,
        items: &[&Item],
    ) -> f64 {
        let theme = ctx.theme;
        let library = Arc::clone(&self.library);
        let elsewhere = Arc::clone(&self.elsewhere);
        let cards: Vec<ShelfCard<'_>> = items
            .iter()
            .filter_map(|item| {
                let Some(&index) = self.query.shelved.get(&item.id) else {
                    let book = elsewhere
                        .iter()
                        .find(|book| crate::resource::book_place_resource(&book.id) == item.id)?;
                    return Some(ShelfCard {
                        book,
                        open: Action::OpenHit(item.id.clone()),
                        here: false,
                        shared: self.shared_in_workspace(&book.id),
                        arriving: self.arriving(&book.id),
                        status: self.shelf_status(&book.id),
                        progress: self.shelf_fraction.get(&book.id).copied().unwrap_or(0.0),
                        notes: None,
                        focused: false,
                    });
                };
                let book = library.get(index)?;
                let notes = self.shelf_annos.get(&book.id).copied().unwrap_or(0);
                Some(ShelfCard {
                    book,
                    open: Action::OpenBook(index),
                    here: true,
                    arriving: None,
                    status: None,
                    shared: self
                        .library_paths
                        .get(&book.id)
                        .is_some_and(|path| crate::content::is_shared(path)),
                    progress: self.shelf_fraction.get(&book.id).copied().unwrap_or(0.0),
                    notes: match notes {
                        0 => None,
                        1 => Some("1 note".to_owned()),
                        n => Some(format!("{n} notes")),
                    },
                    focused: false,
                })
            })
            .collect();
        let rhythm = &theme.tokens.spacing.rhythm;
        let (column_gap, row_gap) = (f64::from(rhythm.section), f64::from(rhythm.group));
        let avail = (span.1 - span.0).max(1.0);
        let cols = card_columns(avail, MediaSize::Small.extent(theme), column_gap);
        let card_w = card_column_width(avail, cols, column_gap);
        let row_text = cards
            .chunks(cols.max(1))
            .map(|row| {
                row.iter()
                    .map(|card| {
                        Self::book_face(card, MediaArt::Placeholder, &[])
                            .text_height(ctx.text, theme, card_w)
                    })
                    .fold(0.0, f64::max)
            })
            .collect();
        let grid = ShelfGrid {
            cols: cols.max(1),
            card_w,
            column_gap,
            row_gap,
            start_x: span.0,
            start_y: y,
            row_text,
        };
        for (slot, card) in cards.iter().enumerate() {
            let cell = grid.cell(slot);
            self.query.rows.push(QueryRow {
                rect: cell,
                action: card.open.clone(),
                id: card.book.id.clone(),
                label: format!("Open {}", card.book.title),
            });
            if cell.y0 < body.y1 && cell.y1 > body.y0 {
                self.draw_book_card(ctx, card, cell, body);
            }
        }
        y + grid.height() + row_gap
    }

    /// One result as a row: a passage by its words, a chapter or a note by
    /// its name, each with where it is; a store book with its author and
    /// the button that gets it.
    fn query_row(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        span: (f64, f64),
        body: Rect,
        y: f64,
        item: &Item,
    ) -> f64 {
        let theme = ctx.theme;
        let store = item.payload_as::<StoreBook>().cloned();
        let title = match item.kind {
            PASSAGE => item.value("excerpt").or_else(|| item.value("title")),
            _ => item.value("title"),
        }
        .unwrap_or_default()
        .to_owned();
        let secondary = item
            .value("place")
            .or_else(|| item.value(AUTHOR_FACET))
            .map(str::to_owned);
        let icon = item
            .entry()
            .and_then(|entry| entry.icon.as_deref())
            .and_then(Icon::from_name)
            .unwrap_or(Icon::FileText);
        let (action, label) = match &store {
            Some(book) => (
                Action::GetBook(book.id),
                format!("Get {} from {}", book.title, crate::store::STORE_LABEL),
            ),
            None => (Action::OpenHit(item.id.clone()), format!("Open {title}")),
        };
        let id = item.id.clone();
        let row_index = self.query.rows.len();
        let get_label = "Get";
        let get_width = ButtonFace::new(Rect::ZERO, get_label)
            .control_size(ControlSize::Small)
            .intrinsic_width(ctx.text, theme);
        // A store book's cover once it has arrived; its placeholder, the
        // same size, until then.
        let cover = store
            .as_ref()
            .and_then(|book| self.query.covers.get(&crate::store::cover_url(book.id)));
        let leading = if store.is_some() {
            RowLeading::Media(
                cover
                    .as_ref()
                    .map_or(MediaArt::Placeholder, MediaArt::Image),
                MediaAspect::Portrait,
            )
        } else {
            RowLeading::Icon(icon)
        };
        let bleed = ui::chrome::row_bleed(theme);
        let words: Vec<String> = self
            .query
            .engine
            .results()
            .query
            .text
            .split_whitespace()
            .filter(|word| !word.contains(':'))
            .map(str::to_owned)
            .collect();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        let mut face = ListRowFace::new(&title).leading(leading).found(&words);
        if item.kind == PASSAGE {
            face = face.two_line_title();
        }
        if let Some(secondary) = &secondary {
            face = face.secondary(secondary);
        }
        if store.is_some() {
            face = face.trailing_width(get_width);
        }
        let height = face.height(ctx.text, theme);
        let rect = Rect::new(span.0 - bleed, y, span.1 + bleed, y + height);
        let visible = rect.y0 < body.y1 && rect.y1 > body.y0;
        let face = face.interaction(Interaction {
            hovered: visible && rect.contains(self.cursor) && body.contains(self.cursor),
            focused: self.query.cursor == Some(row_index),
            ..Interaction::default()
        });
        self.query.rows.push(QueryRow {
            rect,
            action: action.clone(),
            id,
            label,
        });
        if visible {
            face.paint(ctx, rect);
            self.push_hit(rect.intersect(body), action.clone());
            if let Some(slot) = face.trailing_rect(rect, ctx.text, theme) {
                let button_h = ButtonFace::height(ControlSize::Small, theme);
                let button = Rect::new(
                    slot.x1 - get_width,
                    slot.center().y - button_h / 2.0,
                    slot.x1,
                    slot.center().y + button_h / 2.0,
                );
                self.button(
                    ctx,
                    ButtonFace::new(button, get_label)
                        .control_size(ControlSize::Small)
                        .role(ButtonRole::Secondary),
                    action,
                );
            }
        }
        y + height
    }

    /// The store's section: its heading, then its rows. While its answer
    /// is on the way a row's place stands reserved for it (a skeleton after
    /// the grace period, where the answer comes from after a while); once
    /// in, a quiet line says when it could not be reached or found nothing.
    fn query_remote(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        span: (f64, f64),
        body: Rect,
        y: f64,
        remote: &RemoteSection,
        now: ui::time::Instant,
    ) -> f64 {
        if remote.state.is_none() {
            return y;
        }
        let section = f64::from(ctx.theme.tokens.spacing.rhythm.section);
        let mut y = Self::query_heading(ctx, span, y, &remote.label);
        if let Some(clock) = remote.placeholder() {
            let face = ui::widget::ArrivalFace::new(ui::widget::SkeletonShape::Row, clock, now);
            let h = face.height(ctx.text, ctx.theme, span.1 - span.0);
            face.paint(ctx, Rect::new(span.0, y, span.1, y + h));
            y += h;
        } else if let Some(RemoteError(text)) = remote.failure() {
            let note = if text.is_empty() {
                format!("{} can\u{2019}t be reached right now.", remote.label)
            } else {
                text.clone()
            };
            y = Self::query_note(ctx, span, y, &note);
        } else if remote.is_answered() && remote.groups.is_empty() {
            y = Self::query_note(ctx, span, y, &format!("No matches in {}.", remote.label));
        }
        for item in remote.rows() {
            y = self.query_row(ctx, span, body, y, item);
        }
        y + section
    }

    /// Stage a search for a capture or a test: `text` typed into the window
    /// bar's field over `from`, with the store's answer in and the
    /// library's prose indexed.
    pub(super) fn stage_query(&mut self, from: View, text: &str) {
        use ui::widget::Queryable as _;
        self.view = from;
        self.query_opened();
        self.set_query_text(text);
        self.settle_query();
    }

    /// Take in what a capture's library and store answer the open query:
    /// its executor runs what was asked of it, and the answers land.
    pub(super) fn settle_query(&mut self) {
        self.poll_query();
        self.poll_query();
    }
}

impl ui::widget::Queryable for BooksApp {
    fn query_opened(&mut self) {
        if self.query.open {
            return;
        }
        self.query.open = true;
        if self.view != View::Search {
            self.query.returns_to = self.view;
        }
        // From inside a book, the search is the book's: clearing the chip
        // widens it to the library.
        self.query.picked = match (self.view, self.open_book()) {
            (View::Reader, Some(book)) => {
                vec![Chip::value(BOOK_FACET, BookSearch::book_title(book))]
            }
            _ => Vec::new(),
        };
        self.sync_query_local();
        self.query.engine.warm();
        self.dirty = true;
    }

    fn set_query_text(&mut self, text: &str) {
        text.clone_into(&mut self.query.input);
        self.query.cursor = None;
        if self.view != View::Search {
            // The field holding the keyboard is not yet a search: typing is.
            if text.trim().is_empty() {
                return;
            }
            self.query.scroll = 0.0;
            self.query.returns_to = self.view;
            self.reading_settings.close();
            self.view = View::Search;
        }
        self.run_query();
    }

    fn enter_results(&mut self) {
        if !self.query.rows.is_empty() {
            self.query.cursor = Some(0);
            self.dirty = true;
        }
    }

    fn query_closed(&mut self) {
        self.close_query();
    }
}

impl BooksApp {
    /// Give the search's open filter menu, then its chips, the event first.
    /// Whether one of them took it.
    pub(super) fn route_query_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) -> bool {
        use ui::widget::Widget as _;
        if self.query.facets.is_open() {
            let mut taken = false;
            let mut focus = None;
            self.query.facets.menu_mut().handle_event(
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut taken,
                    focus_target: &mut focus,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if let Some(query) = self.query.facets.take_choice() {
                self.on_set_query(&query);
                taken = true;
            }
            // A press anywhere while it is open is its: a choice, or the
            // press that dismisses it.
            let pressed = matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Pressed,
                    ..
                }
            );
            if taken || pressed {
                self.dirty = true;
                *ctx.handled = true;
                return true;
            }
        }
        if self.view != View::Search {
            return false;
        }
        self.query.chips.host.handle_event(event, ctx);
        let asked = self.query.chips.take_asked();
        let took = *ctx.handled || !asked.is_empty();
        for action in asked {
            self.dispatch(action, &mut ctx.state);
        }
        if took {
            self.dirty = true;
            *ctx.handled = true;
        }
        took
    }
}

/// The book, chapter and paragraph a location anchor names.
fn parse_location(anchor: &str) -> Option<(String, usize, usize)> {
    let rest = anchor.strip_prefix("ebook.location:")?;
    let (rest, block) = rest.rsplit_once(':')?;
    let (book, chapter) = rest.rsplit_once(':')?;
    Some((book.to_owned(), chapter.parse().ok()?, block.parse().ok()?))
}

/// The books of `entries` whose text is not among `here`, as their rows
/// name them, with the cover generated from their id.
fn books_elsewhere(entries: &[crate::model::LibraryEntry], here: &[Arc<Book>]) -> Vec<Arc<Book>> {
    let here: std::collections::HashSet<&str> = here.iter().map(|book| book.id.as_str()).collect();
    entries
        .iter()
        .filter(|entry| !here.contains(entry.id.as_str()))
        .map(|entry| {
            Arc::new(Book {
                id: entry.id.clone(),
                title: entry.title.clone(),
                author: entry.author.clone(),
                subject: entry.subject.clone(),
                cover_tint: crate::content::tint_for(&entry.id),
                ..Book::default()
            })
        })
        .collect()
}
