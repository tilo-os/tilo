//! What the Books search finds: books, chapters, annotations and remote
//! library entries, as the search entries the reader surface contributes to
//! the app's one search source, each invoking the canonical action that
//! opens it.
//!
//! Each chapter's prose is an entry's [`SearchBody`]: the reader stays the
//! sole owner of EPUB text, the index reads it in place, and a match reports
//! the exact block it is in. Rebuilds after an annotation or library edit
//! hand back the same bodies for every book they keep, so the prose of a
//! kept book is not read again.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use tilo_actions::Resource;
use ui::icons::Icon;
use ui::search::SearchResult;
use ui::tilo_resource::{Derived, SearchBody, SearchPiece, SearchText, SearchWeight};

use crate::model::{AnnoKind, Annotation, Book, LibraryEntry};
use crate::resource::{Ebook, book_place_resource, mark_place_resource};

pub const BOOK_KIND: &str = "Books";
pub const CHAPTER_KIND: &str = "Chapters";
pub const ANNOTATION_KIND: &str = "Annotations";
/// A precise, lazy full-text match. Unlike a chapter row this resource names
/// the exact immutable block that satisfied a query, so activating a search
/// hit never guesses at a nearby reading location.
pub const LOCATION_KIND: &str = "Reading locations";

fn chapter_label(chapter: &crate::model::Chapter, index: usize) -> String {
    if chapter.title.trim().is_empty() {
        format!("Chapter {}", index + 1)
    } else {
        chapter.title.clone()
    }
}

/// One chapter's prose as a search body: one segment per block, one piece
/// per span. The source borrows the shared book snapshot; nothing is copied.
struct ChapterBody {
    book: Arc<Book>,
    chapter: usize,
}

impl ChapterBody {
    fn blocks(&self) -> &[crate::model::Block] {
        self.book
            .chapters
            .get(self.chapter)
            .map_or(&[], |chapter| chapter.blocks.as_slice())
    }
}

impl SearchText for ChapterBody {
    fn segment_count(&self) -> usize {
        self.blocks().len()
    }

    fn piece_count(&self, segment: usize) -> usize {
        match self.blocks().get(segment) {
            Some(crate::model::Block::Math { .. }) => 1,
            Some(crate::model::Block::Separator) | None => 0,
            Some(block) => block.spans().len(),
        }
    }

    fn piece(&self, segment: usize, piece: usize) -> Option<SearchPiece<'_>> {
        match self.blocks().get(segment)? {
            crate::model::Block::Math { source, .. } => (piece == 0).then_some(SearchPiece {
                text: source,
                space_before: segment > 0,
            }),
            crate::model::Block::Separator => None,
            block => {
                let span = block.spans().get(piece)?;
                Some(SearchPiece {
                    text: &span.text,
                    space_before: if piece == 0 {
                        segment > 0
                    } else {
                        span.space_before
                    },
                })
            }
        }
    }

    fn locates(&self, segment: usize) -> bool {
        segment < self.blocks().len()
    }

    /// One canonical, block-level full-text hit. It retains no excerpt; the
    /// stable resource is resolved lazily on activation against the same
    /// immutable book snapshot.
    fn location(&self, segment: usize) -> Option<ui::tilo_resource::SearchEntry> {
        let chapter = self.book.chapters.get(self.chapter)?;
        chapter.blocks.get(segment)?;
        let chapter_title = chapter_label(chapter, self.chapter);
        Some(
            ui::tilo_resource::SearchEntry::new(format!("Match in {chapter_title}"), "")
                .anchored(BookSearch::location_anchor(
                    &self.book.id,
                    self.chapter,
                    segment,
                ))
                .grouped(LOCATION_KIND)
                .subtitled(format!(
                    "{}: {chapter_title}",
                    BookSearch::book_title(&self.book)
                ))
                .with_icon(Icon::FileText.name()),
        )
    }
}

/// Stable identity of a shared book allocation. The provider holding the
/// map also holds every book in it, so an address is never reused while the
/// map can be consulted.
fn book_key(book: &Arc<Book>) -> usize {
    Arc::as_ptr(book).addr()
}

#[derive(Clone)]
enum SearchTarget {
    Book(usize),
    Chapter { book: usize, chapter: usize },
    Annotation { index: usize, hydrated: bool },
    RemoteBook(usize),
}

#[derive(Clone)]
struct SearchEntry {
    result: SearchResult,
    target: SearchTarget,
}

#[derive(Clone)]
pub struct BookSearch {
    /// What the search finds, built with the rest of this projection.
    search: Arc<Vec<ui::tilo_resource::SearchEntry>>,
    // The live app and each palette session share these Arcs. No EPUB body is
    // cloned solely for searching.
    books: Arc<Vec<Arc<Book>>>,
    annotations: Arc<Vec<Annotation>>,
    registry: Arc<Vec<LibraryEntry>>,
    entries: Arc<Vec<SearchEntry>>,
    resource_rows: Arc<BTreeMap<String, usize>>,
    /// Indexed chapter bodies per book allocation, reused by every rebuild
    /// that keeps the book.
    bodies: Arc<HashMap<usize, Arc<[SearchBody]>>>,
    available: bool,
}

impl BookSearch {
    /// Compatibility constructor for small callers and tests. The live app
    /// uses `Self::new_shared` so its content cache is not copied.
    pub fn new(books: &[Book], annotations: &[Annotation]) -> Self {
        Self::new_shared(
            Arc::new(books.iter().cloned().map(Arc::new).collect()),
            Arc::new(annotations.to_vec()),
            Arc::new(Vec::new()),
            1,
        )
    }

    pub fn new_with_registry(
        books: &[Book],
        annotations: &[Annotation],
        registry: &[LibraryEntry],
    ) -> Self {
        Self::new_shared(
            Arc::new(books.iter().cloned().map(Arc::new).collect()),
            Arc::new(annotations.to_vec()),
            Arc::new(registry.to_vec()),
            1,
        )
    }

    /// Stage a complete immutable projection before the app swaps it in. A
    /// malformed or ambiguous projection withdraws atomically rather than
    /// publishing a partial action directory.
    pub(crate) fn new_shared(
        books: Arc<Vec<Arc<Book>>>,
        annotations: Arc<Vec<Annotation>>,
        registry: Arc<Vec<LibraryEntry>>,
        generation: u64,
    ) -> Self {
        Self::build(books, annotations, registry, generation, None)
    }

    /// Like [`Self::new_shared`], but reuse `prior`'s chapter-prose index
    /// for every book it already indexed. Annotation and library edits then
    /// rebuild only the metadata rows, never re-read a kept book.
    pub(crate) fn new_shared_after(
        prior: &Self,
        books: Arc<Vec<Arc<Book>>>,
        annotations: Arc<Vec<Annotation>>,
        registry: Arc<Vec<LibraryEntry>>,
        generation: u64,
    ) -> Self {
        Self::build(books, annotations, registry, generation, Some(prior))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the staged canonical row builder is deliberately together"
    )]
    fn build(
        books: Arc<Vec<Arc<Book>>>,
        annotations: Arc<Vec<Annotation>>,
        registry: Arc<Vec<LibraryEntry>>,
        generation: u64,
        prior: Option<&Self>,
    ) -> Self {
        let mut entries = Vec::new();
        let mut resource_rows = BTreeMap::new();
        let mut search = Vec::new();
        let mut bodies = HashMap::<usize, Arc<[SearchBody]>>::new();
        let hydrated: HashSet<&str> = books.iter().map(|book| book.id.as_str()).collect();
        let mut book_metadata = HashMap::<String, (String, String)>::new();
        for book in books.iter() {
            book_metadata.insert(
                book.id.clone(),
                (Self::book_title(book).to_string(), book.author.clone()),
            );
        }
        for entry in registry.iter() {
            book_metadata.entry(entry.id.clone()).or_insert_with(|| {
                (
                    if entry.title.trim().is_empty() {
                        "Untitled".into()
                    } else {
                        entry.title.clone()
                    },
                    entry.author.clone(),
                )
            });
        }
        let known: HashSet<&str> = hydrated
            .iter()
            .copied()
            .chain(registry.iter().map(|entry| entry.id.as_str()))
            .collect();
        let groups: HashSet<(String, uuid::Uuid)> = annotations
            .iter()
            .filter(|annotation| annotation.kind == AnnoKind::Highlight)
            .filter(|annotation| known.contains(annotation.doc_id.as_str()))
            .map(|annotation| (annotation.doc_id.clone(), annotation.group))
            .collect();

        // `fields` beyond the title and subtitle (an author, a subject, a
        // note) are what else the entry is found by, each at its weight.
        let mut add = |result: SearchResult,
                       target: SearchTarget,
                       fields: &[(&str, SearchWeight)],
                       body: Option<SearchBody>|
         -> bool {
            let anchor = result.action_id.clone();
            if anchor.is_empty()
                || resource_rows
                    .insert(anchor.clone(), entries.len())
                    .is_some()
            {
                return false;
            }
            let mut entry = ui::tilo_resource::SearchEntry::new(result.title.clone(), "")
                .anchored(anchor)
                .grouped(result.kind.clone());
            for (text, weight) in fields {
                entry = entry.field(*text, *weight);
            }
            if let Some(subtitle) = &result.subtitle {
                entry = entry.subtitled(subtitle.clone());
            }
            if let Some(icon) = result.icon {
                entry = entry.with_icon(icon.name());
            }
            if let Some(body) = body {
                entry = entry.with_body(body);
            }
            search.push(entry);
            entries.push(SearchEntry { result, target });
            true
        };

        for (book_index, book) in books.iter().enumerate() {
            let title = Self::book_title(book).to_string();
            let mut result =
                SearchResult::new(BOOK_KIND, title.clone(), book_place_resource(&book.id))
                    .with_icon(Icon::FileText);
            if !book.author.trim().is_empty() {
                result = result.with_subtitle(book.author.clone());
            }
            if !add(result, SearchTarget::Book(book_index), &[], None) {
                return Self::unavailable(books, annotations, registry);
            }
            let chapter_bodies = prior
                .and_then(|prior| prior.bodies.get(&book_key(book)))
                .filter(|reused| reused.len() == book.chapters.len())
                .map_or_else(
                    || -> Arc<[SearchBody]> {
                        (0..book.chapters.len())
                            .map(|chapter| {
                                SearchBody::new(Arc::new(ChapterBody {
                                    book: Arc::clone(book),
                                    chapter,
                                }))
                            })
                            .collect()
                    },
                    Arc::clone,
                );
            for (chapter_index, chapter) in book.chapters.iter().enumerate() {
                let chapter_title = chapter_label(chapter, chapter_index);
                let result = SearchResult::new(
                    CHAPTER_KIND,
                    chapter_title.clone(),
                    Self::chapter_anchor(&book.id, chapter_index),
                )
                .with_subtitle(title.clone())
                .with_icon(Icon::List);
                if !add(
                    result,
                    SearchTarget::Chapter {
                        book: book_index,
                        chapter: chapter_index,
                    },
                    &[
                        (&title, SearchWeight::Subtitle),
                        (&book.author, SearchWeight::Metadata),
                    ],
                    chapter_bodies.get(chapter_index).cloned(),
                ) {
                    return Self::unavailable(books, annotations, registry);
                }
            }
            bodies.insert(book_key(book), chapter_bodies);
        }
        for (registry_index, entry) in registry.iter().enumerate() {
            if hydrated.contains(entry.id.as_str()) {
                continue;
            }
            let title = if entry.title.trim().is_empty() {
                "Untitled".to_string()
            } else {
                entry.title.clone()
            };
            // A collaborator's book is fetched before it opens.
            let mut result =
                SearchResult::new(BOOK_KIND, title.clone(), book_place_resource(&entry.id))
                    .with_icon(Icon::FileText);
            if !entry.author.trim().is_empty() {
                result = result.with_subtitle(entry.author.clone());
            }
            if !add(
                result,
                SearchTarget::RemoteBook(registry_index),
                &[(&entry.subject, SearchWeight::Metadata)],
                None,
            ) {
                return Self::unavailable(books, annotations, registry);
            }
        }
        let mut emitted_highlights = HashSet::new();
        for (annotation_index, annotation) in annotations.iter().enumerate() {
            if !known.contains(annotation.doc_id.as_str()) {
                continue;
            }
            if annotation.kind == AnnoKind::Highlight
                && !emitted_highlights.insert((annotation.doc_id.as_str(), annotation.group))
            {
                continue;
            }
            let (title, author) = book_metadata
                .get(&annotation.doc_id)
                .cloned()
                .unwrap_or_else(|| ("Unknown book".into(), String::new()));
            let comment = annotation.kind == AnnoKind::Note
                && groups.contains(&(annotation.doc_id.clone(), annotation.group));
            let result = SearchResult::new(
                ANNOTATION_KIND,
                Self::preview(annotation),
                mark_place_resource(annotation, comment),
            )
            .with_subtitle(format!(
                "{} in chapter {} of {title}",
                annotation.kind.label(),
                annotation.chapter + 1
            ))
            .with_icon(match annotation.kind {
                AnnoKind::Bookmark => Icon::Bookmark,
                AnnoKind::Highlight => Icon::Highlighter,
                AnnoKind::Note => Icon::MessageSquare,
            });
            let kind = if comment {
                format!("{} comment", annotation.kind.label())
            } else {
                annotation.kind.label().to_string()
            };
            if !add(
                result,
                SearchTarget::Annotation {
                    index: annotation_index,
                    hydrated: hydrated.contains(annotation.doc_id.as_str()),
                },
                &[
                    (&annotation.note, SearchWeight::Title),
                    (&annotation.excerpt, SearchWeight::Title),
                    (&title, SearchWeight::Subtitle),
                    (&author, SearchWeight::Metadata),
                    (&kind, SearchWeight::Metadata),
                ],
                None,
            ) {
                return Self::unavailable(books, annotations, registry);
            }
        }
        if generation == 0 {
            return Self::unavailable(books, annotations, registry);
        }
        Self {
            search: Arc::new(search),
            books,
            annotations,
            registry,
            entries: Arc::new(entries),
            resource_rows: Arc::new(resource_rows),
            bodies: Arc::new(bodies),
            available: true,
        }
    }

    fn unavailable(
        books: Arc<Vec<Arc<Book>>>,
        annotations: Arc<Vec<Annotation>>,
        registry: Arc<Vec<LibraryEntry>>,
    ) -> Self {
        Self {
            search: Arc::new(Vec::new()),
            books,
            annotations,
            registry,
            entries: Arc::new(Vec::new()),
            resource_rows: Arc::new(BTreeMap::new()),
            bodies: Arc::new(HashMap::new()),
            available: false,
        }
    }

    /// What the search finds in this projection; empty while it is
    /// withdrawn.
    #[must_use]
    pub fn entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.search.as_ref().clone()
    }

    #[cfg(test)]
    pub(crate) fn shares_prose_index_with(&self, other: &Self) -> bool {
        self.books.iter().all(|book| {
            match (
                self.bodies.get(&book_key(book)),
                other.bodies.get(&book_key(book)),
            ) {
                (Some(mine), Some(theirs)) => Arc::ptr_eq(mine, theirs),
                _ => false,
            }
        })
    }

    fn book_title(book: &Book) -> &str {
        if book.title.trim().is_empty() {
            "Untitled"
        } else {
            &book.title
        }
    }

    fn chapter_anchor(book_id: &str, chapter: usize) -> String {
        format!("ebook.chapter:{book_id}:{chapter}")
    }

    fn preview(annotation: &Annotation) -> String {
        let note = annotation.note.clone();
        let text = if note.trim().is_empty() {
            annotation.excerpt.trim()
        } else {
            note.trim()
        };
        let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            annotation.kind.label().to_string()
        } else if collapsed.chars().count() > 72 {
            collapsed.chars().take(69).collect::<String>() + "…"
        } else {
            collapsed
        }
    }

    fn location_anchor(book_id: &str, chapter: usize, block: usize) -> String {
        format!("ebook.location:{book_id}:{chapter}:{block}")
    }

    /// Resolve a full-text location resource exactly. This performs a bounded
    /// metadata lookup plus one block bounds check; it does not construct a
    /// prose index or hydrate unrelated chapters.
    fn location_resource_for(&self, id: &str) -> Option<Resource> {
        let suffix = id.strip_prefix("ebook.location:")?;
        let (book, location) = self.books.iter().find_map(|book| {
            let prefix = format!("{}:", book.id);
            suffix
                .strip_prefix(&prefix)
                .map(|location| (book, location))
        })?;
        let mut parts = location.split(':');
        let (Some(chapter), Some(block), None) = (parts.next(), parts.next(), parts.next()) else {
            return None;
        };
        let (Ok(chapter), Ok(block)) = (chapter.parse::<usize>(), block.parse::<usize>()) else {
            return None;
        };
        let chapter_data = book.chapters.get(chapter)?;
        chapter_data.blocks.get(block)?;
        let chapter_title = chapter_label(chapter_data, chapter);
        Some(
            Resource::new(
                id,
                "ebook.location",
                format!("{chapter_title} in {}", Self::book_title(book)),
            )
            .with_action(crate::actions::open_at(
                &book.id,
                chapter,
                Some(block),
                "Open match",
            )),
        )
    }

    /// The resource a hit names: a chapter opened, or the place the library
    /// publishes for a book or an annotation, with what acts on it.
    fn resource_for(&self, entry: &SearchEntry, id: &str) -> Option<Resource> {
        let book = match entry.target {
            SearchTarget::Chapter { book, chapter } => {
                let book = self.books.get(book)?;
                book.chapters.get(chapter)?;
                return Some(
                    Resource::new(id, "ebook.chapter", &entry.result.title)
                        .with_action(crate::actions::open_at(&book.id, chapter, None, "Open")),
                );
            }
            SearchTarget::Book(index) => self.books.get(index)?.id.clone(),
            SearchTarget::RemoteBook(index) => self.registry.get(index)?.id.clone(),
            SearchTarget::Annotation {
                index, hydrated, ..
            } => {
                if !hydrated {
                    return None;
                }
                self.annotations.get(index)?.doc_id.clone()
            }
        };
        let row = self
            .registry
            .iter()
            .find(|row| row.id == book)
            .cloned()
            .or_else(|| {
                self.books
                    .iter()
                    .find(|candidate| candidate.id == book)
                    .map(|candidate| LibraryEntry {
                        id: candidate.id.clone(),
                        title: candidate.title.clone(),
                        author: candidate.author.clone(),
                        subject: candidate.subject.clone(),
                        ..LibraryEntry::default()
                    })
            })?;
        let marks: Vec<&Annotation> = self
            .annotations
            .iter()
            .filter(|mark| mark.doc_id == book)
            .collect();
        let place = Ebook::book_place(&row, &marks);
        let mut resource = Derived::find(std::slice::from_ref(&place), id)?;
        resource.children.clear();
        Some(resource)
    }
}

impl BookSearch {
    /// The exact resource a hit is anchored at: a book, a chapter, an
    /// annotation, a remote book or a reading location.
    pub(crate) fn resolve(&self, id: &str) -> Option<Resource> {
        if !self.available {
            return None;
        }
        if let Some(resource) = self.location_resource_for(id) {
            return Some(resource);
        }
        let row = *self.resource_rows.get(id)?;
        let entry = self.entries.get(row)?;
        self.resource_for(entry, id)
    }

    /// What opening the hit anchored at `id` runs: the reader opens the
    /// book, chapter, annotation or match there; a collaborator's book not
    /// on this device is fetched first. `None` once the hit is gone.
    pub(crate) fn opened(&self, id: &str) -> Option<crate::resource::BooksEffect> {
        use ui::tilo_resource::Effect as _;
        let resource = self.resolve(id)?;
        let action = resource
            .actions
            .iter()
            .find(|action| action.id == "open")
            .or_else(|| {
                resource
                    .actions
                    .iter()
                    .find(|action| action.id == "download")
            })?;
        let encoded = action.build_json(&serde_json::Map::new()).ok()?;
        crate::resource::BooksEffect::from_payload(&encoded.payload).ok()
    }
}

/// The search source the framework builds over a projection, resolving
/// its hits' addresses the way the reader surface does: what the tests of
/// what Books finds search and open.
#[cfg(test)]
pub(crate) struct BookSearchProvider {
    books: BookSearch,
    source: Box<dyn ui::search::SearchProvider>,
}

#[cfg(test)]
impl BookSearchProvider {
    fn over(books: BookSearch) -> Self {
        let source = Box::new(ui::framework::testing::search_source::<
            crate::resource::Ebook,
        >(books.entries()));
        Self { books, source }
    }

    pub fn new(books: &[Book], annotations: &[Annotation]) -> Self {
        Self::over(BookSearch::new(books, annotations))
    }

    pub fn new_with_registry(
        books: &[Book],
        annotations: &[Annotation],
        registry: &[LibraryEntry],
    ) -> Self {
        Self::over(BookSearch::new_with_registry(books, annotations, registry))
    }

    pub(crate) fn new_shared(
        books: Arc<Vec<Arc<Book>>>,
        annotations: Arc<Vec<Annotation>>,
        registry: Arc<Vec<LibraryEntry>>,
        generation: u64,
    ) -> Self {
        Self::over(BookSearch::new_shared(
            books,
            annotations,
            registry,
            generation,
        ))
    }

    pub(crate) fn new_shared_after(
        prior: &Self,
        books: Arc<Vec<Arc<Book>>>,
        annotations: Arc<Vec<Annotation>>,
        registry: Arc<Vec<LibraryEntry>>,
        generation: u64,
    ) -> Self {
        Self::over(BookSearch::new_shared_after(
            &prior.books,
            books,
            annotations,
            registry,
            generation,
        ))
    }

    /// How many entries are found by prose read in place.
    pub(crate) fn body_entries(&self) -> usize {
        self.books
            .entries()
            .iter()
            .filter(|entry| entry.body.is_some())
            .count()
    }

    pub(crate) fn shares_prose_index_with(&self, other: &Self) -> bool {
        self.books.shares_prose_index_with(&other.books)
    }
}

#[cfg(test)]
impl ui::search::SearchProvider for BookSearchProvider {
    fn name(&self) -> &str {
        self.source.name()
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.source.search(query)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.source.search_limited(query, limit)
    }

    fn search_page(
        &self,
        query: &ui::search::SearchQuery,
        cursor: Option<&ui::search::SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> ui::search::SearchPage {
        self.source
            .search_page(query, cursor, work_budget, result_limit)
    }

    fn action_directory_generation(&self) -> Option<u64> {
        self.source.action_directory_generation()
    }

    fn action_directory_cursor_is_current(&self, cursor: &ui::search::SearchCursor) -> bool {
        self.source.action_directory_cursor_is_current(cursor)
    }

    fn resolve_action_resource(&self, address: &tilo_actions::ActionAddress) -> Option<Resource> {
        self.books.resolve(&address.resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::BooksEffect;
    use tilo_actions::{ConfirmationPolicy, RiskLevel};
    use ui::search::SearchProvider;
    use ui::search::testing::{CorpusRow, LATE_HIT_NEEDLE, assert_late_hit_reachable};
    use ui::tilo_resource::Effect as _;

    fn sample_books() -> Vec<Book> {
        crate::content::load_sample_library()
    }

    /// Metadata and full-text hits (including cross-boundary phrases and remote books) resolve to exact canonical resources and locations.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn search_hits_resolve_to_canonical_book_locations() {
        // --- phase: canonical_results_and_exact_lazy_resources ---
        {
            let books = sample_books();
            let provider = BookSearchProvider::new(&books, &[]);
            let hit = provider
                .search("")
                .into_iter()
                .find(|hit| hit.kind == CHAPTER_KIND)
                .expect("chapter");
            let address = hit.action_id;
            let resource = provider
                .books
                .resolve(&address)
                .expect("exact local resource");
            assert_eq!(resource.id.as_str(), address);
            assert!(resource.children.is_empty());
            assert_eq!(resource.actions.len(), 1);
        }

        // --- phase: body_substrings_share_the_sole_book_snapshot ---
        {
            let books: Arc<Vec<Arc<Book>>> =
                Arc::new(sample_books().into_iter().map(Arc::new).collect());
            let needle = books[0].chapters[0]
                .blocks
                .iter()
                .map(crate::model::Block::plain_text)
                .find_map(|text| {
                    text.split_whitespace()
                        .find(|word| word.len() > 7)
                        .map(str::to_owned)
                })
                .expect("prose");
            let provider = BookSearchProvider::new_shared(
                Arc::clone(&books),
                Arc::new(Vec::new()),
                Arc::new(Vec::new()),
                7,
            );
            assert!(
                provider
                    .search(&needle)
                    .iter()
                    .any(|hit| hit.kind == LOCATION_KIND)
            );
            assert!(provider.body_entries() > 0);
            assert!(Arc::ptr_eq(&books, &provider.books.books));
        }

        // --- phase: full_text_hits_resolve_to_the_exact_canonical_block_location ---
        {
            let books = vec![Book {
                id: "stable-book".into(),
                title: "Exact places".into(),
                chapters: vec![crate::model::Chapter {
                    title: "Opening".into(),
                    blocks: vec![
                        crate::model::Block::Paragraph {
                            spans: vec![crate::model::Span::plain("an earlier paragraph")],
                        },
                        crate::model::Block::Paragraph {
                            spans: vec![crate::model::Span::plain("the unique needle is here")],
                        },
                    ],
                }],
                ..Book::default()
            }];
            let provider = BookSearchProvider::new(&books, &[]);
            let hit = provider
                .search("unique needle")
                .into_iter()
                .find(|hit| hit.kind == LOCATION_KIND)
                .expect("precise full-text location");
            let address = hit.action_id;
            assert_eq!(address, "ebook.location:stable-book:0:1");
            let resource = provider
                .books
                .resolve(&address)
                .expect("exact lazy location resource");
            let open = resource
                .actions
                .iter()
                .find(|action| action.id == "open")
                .expect("open action");
            let encoded = open
                .build_json(&serde_json::Map::new())
                .expect("a bound open takes no arguments");
            assert_eq!(
                BooksEffect::from_payload(&encoded.payload).unwrap(),
                BooksEffect::Open {
                    book: "stable-book".into(),
                    chapter: 0,
                    page: 0,
                    block: Some(1),
                }
            );
        }

        // --- phase: body_phrase_crosses_inline_but_not_block_boundaries ---
        {
            let mut beta = crate::model::Span::plain("beta");
            beta.space_before = true;
            let books = vec![Book {
                id: "boundaries".into(),
                title: "Boundaries".into(),
                chapters: vec![crate::model::Chapter {
                    title: "Boundaries".into(),
                    blocks: vec![
                        crate::model::Block::Paragraph {
                            spans: vec![crate::model::Span::plain("alpha"), beta],
                        },
                        crate::model::Block::Paragraph {
                            spans: vec![crate::model::Span::plain("gamma")],
                        },
                    ],
                }],
                ..Book::default()
            }];
            let provider = BookSearchProvider::new(&books, &[]);
            let locations = |query: &str| {
                provider
                    .search(query)
                    .into_iter()
                    .filter(|result| result.kind == LOCATION_KIND)
                    .count()
            };
            // Spans of one block read as one text; each block is its own
            // location, so a phrase never runs from one into the next.
            assert_eq!(locations("alpha beta"), 1);
            assert_eq!(locations("beta gamma"), 0);
        }

        // --- phase: remote_metadata_hydrates_and_opens_the_canonical_workspace_book ---
        {
            let entry = LibraryEntry {
                id: "book:remote".into(),
                title: "Remote Atlas".into(),
                author: "Mira Peer".into(),
                subject: "Maps".into(),
                shared: true,
                added: 1,
                content: ui::tilo_resource::Bytes::Empty,
            };
            let provider = BookSearchProvider::new_with_registry(&[], &[], &[entry]);
            let hit = provider.search("mira").pop().expect("metadata");
            let resource = provider
                .books
                .resolve(&hit.action_id)
                .expect("actionable remote resource");
            let open = resource
                .actions
                .iter()
                .find(|action| action.id == "download")
                .expect("download");
            assert_eq!(open.label, "Download and open");
            assert!(matches!(
                open.contract.execution,
                tilo_actions::ExecutionMode::Background { .. }
            ));
            let operation = open
                .build_json(&serde_json::Map::new())
                .expect("a bound download takes no arguments");
            assert_eq!(
                BooksEffect::from_payload(&operation.payload).unwrap(),
                BooksEffect::Download {
                    book: "book:remote".into(),
                }
            );
            let remove = resource
                .actions
                .iter()
                .find(|action| action.id == "remove-from-library")
                .expect("remote remove");
            assert_eq!(remove.contract.risk, RiskLevel::Destructive);
            assert_eq!(remove.contract.confirmation, ConfirmationPolicy::Confirm);
            assert!(remove.contract.reversible);
        }
    }

    /// The index retains no prose, finds late hits within bounds without library scans, and reuses allocations across rebuilds.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn index_bounds_work_and_memory_on_large_libraries() {
        // --- phase: million_scalar_book_keeps_its_prose_by_reference ---
        {
            let books = Arc::new(vec![Arc::new(Book {
                id: "large".into(),
                title: "Large book".into(),
                chapters: vec![crate::model::Chapter {
                    title: "Long chapter".into(),
                    blocks: vec![crate::model::Block::Math {
                        expr: ui::math::MathExpr::new(),
                        source: format!("{} tail-needle", "x".repeat(1_000_000)),
                        display: false,
                    }],
                }],
                ..Book::default()
            })]);
            let provider = BookSearchProvider::new_shared(
                Arc::clone(&books),
                Arc::new(Vec::new()),
                Arc::new(Vec::new()),
                3,
            );
            let mut cursor = None;
            let mut found = false;
            loop {
                let page = provider.search_page(&"tail-needle".into(), cursor.as_ref(), 32, 8);
                found |= page.results.iter().any(|hit| hit.kind == LOCATION_KIND);
                let Some(next) = page.next else { break };
                cursor = Some(next);
            }
            assert!(found);
            // The catalogue keeps the books' prose by reference, not a copy.
            assert_eq!(Arc::strong_count(&books), 2);
        }

        // --- phase: late_metadata_hit_is_found_within_the_search_bound ---
        {
            let (provider, hit) =
                assert_late_hit_reachable(|corpus| late_hit_library(corpus, false), 3_000);
            assert_eq!(hit.kind, BOOK_KIND);
            assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
            let address = hit.action_id;
            assert_eq!(address, crate::resource::book_place_resource("book:002999"));
            assert!(provider.books.resolve(&address).is_some());
        }

        // --- phase: late_body_only_hit_is_found_within_the_search_bound ---
        {
            let (provider, hit) =
                assert_late_hit_reachable(|corpus| late_hit_library(corpus, true), 3_000);
            assert_eq!(hit.kind, LOCATION_KIND);
            let address = hit.action_id;
            assert_eq!(address, "ebook.location:book:002999:0:0");
            assert!(provider.books.resolve(&address).is_some());
        }

        // --- phase: late_metadata_posting_avoids_a_library_scan ---
        {
            let books = (0..3_000)
                .map(|i| Book {
                    id: format!("book:{i:06}"),
                    title: format!("Library volume {i}"),
                    ..Book::default()
                })
                .collect::<Vec<_>>();
            let provider = BookSearchProvider::new(&books, &[]);
            let page = provider.search_page(&"volume 2999".into(), None, 4, 4);
            assert_eq!(page.inspected, 1);
            assert_eq!(page.results.len(), 1);
            assert_eq!(page.results[0].title, "Library volume 2999");
        }

        // --- phase: prose_no_chapter_contains_ends_without_inspecting_the_library ---
        {
            let books = (0..1_000)
                .map(|index| Book {
                    id: format!("book-{index}"),
                    title: "Shelf".into(),
                    chapters: vec![crate::model::Chapter {
                        title: "Opening".into(),
                        blocks: vec![crate::model::Block::Paragraph {
                            spans: vec![crate::model::Span::plain("present words only")],
                        }],
                    }],
                    ..Book::default()
                })
                .collect::<Vec<_>>();
            let provider = BookSearchProvider::new(&books, &[]);
            let first = provider.search_page(&"absent".into(), None, 3, 3);
            assert!(first.results.is_empty());
            assert_eq!(first.inspected, 0);
            assert!(first.next.is_none(), "no chapter holds the query's grams");
        }

        // --- phase: chapters_holding_the_words_out_of_order_do_not_match ---
        {
            // Every chapter holds each word of "dog cat" but not the phrase.
            let books = (0..40)
                .map(|index| Book {
                    id: format!("book-{index}"),
                    chapters: vec![crate::model::Chapter {
                        blocks: vec![crate::model::Block::Paragraph {
                            spans: vec![crate::model::Span::plain("cat dog g ca")],
                        }],
                        ..crate::model::Chapter::default()
                    }],
                    ..Book::default()
                })
                .collect::<Vec<_>>();
            let provider = BookSearchProvider::new(&books, &[]);
            let mut cursor = None;
            let mut inspected = 0;
            loop {
                let page = provider.search_page(&"dog cat".into(), cursor.as_ref(), 7, 7);
                assert!(page.results.is_empty());
                assert!(page.inspected <= 7);
                inspected += page.inspected;
                let Some(next) = page.next else { break };
                cursor = Some(next);
            }
            assert_eq!(inspected, 0);
        }

        // --- phase: rebuilds_over_the_same_books_reuse_the_prose_index ---
        {
            let books: Arc<Vec<Arc<Book>>> =
                Arc::new(sample_books().into_iter().map(Arc::new).collect());
            let first = BookSearchProvider::new_shared(
                Arc::clone(&books),
                Arc::new(Vec::new()),
                Arc::new(Vec::new()),
                1,
            );
            assert!(first.body_entries() > 0);
            assert!(first.shares_prose_index_with(&first));
            let mut annotation = Annotation::new(&books[0].id, AnnoKind::Note, 0, 0);
            annotation.note = "a fresh note".into();
            let annotated = BookSearchProvider::new_shared_after(
                &first,
                Arc::clone(&books),
                Arc::new(vec![annotation]),
                Arc::new(Vec::new()),
                2,
            );
            assert!(annotated.shares_prose_index_with(&first));
            assert!(
                annotated
                    .search("fresh note")
                    .iter()
                    .any(|hit| hit.kind == ANNOTATION_KIND)
            );

            let mut fewer = (*books).clone();
            let removed = fewer.remove(0);
            let rebuilt = BookSearchProvider::new_shared_after(
                &annotated,
                Arc::new(fewer),
                Arc::new(Vec::new()),
                Arc::new(Vec::new()),
                3,
            );
            assert!(
                rebuilt.shares_prose_index_with(&annotated),
                "the books a rebuild keeps are not re-read"
            );
            let needle = removed.chapters[0]
                .blocks
                .iter()
                .map(crate::model::Block::plain_text)
                .find(|text| text.len() > 40)
                .expect("prose");
            assert!(
                rebuilt.search(&needle).iter().all(|hit| !hit
                    .subtitle
                    .as_deref()
                    .unwrap_or("")
                    .contains(&removed.title)),
                "a removed book's prose is no longer indexed"
            );
        }

        // --- phase: membership_snapshots_keep_existing_book_allocations ---
        {
            let books: Arc<Vec<Arc<Book>>> = Arc::new(vec![Arc::new(Book {
                id: "kept".into(),
                chapters: vec![crate::model::Chapter {
                    blocks: vec![crate::model::Block::Math {
                        expr: ui::math::MathExpr::new(),
                        source: "reader-owned body".into(),
                        display: false,
                    }],
                    ..crate::model::Chapter::default()
                }],
                ..Book::default()
            })]);
            let mut next = Arc::clone(&books);
            Arc::make_mut(&mut next).push(Arc::new(Book::default()));
            assert!(Arc::ptr_eq(&books[0], &next[0]));
            Arc::make_mut(&mut next).remove(1);
            assert!(Arc::ptr_eq(&books[0], &next[0]));
        }
    }

    fn late_hit_library(
        corpus: impl Iterator<Item = CorpusRow>,
        in_body: bool,
    ) -> BookSearchProvider {
        let books = corpus
            .map(|row| {
                let (title, chapters) = if in_body {
                    (
                        format!("Library volume {}", row.index),
                        vec![crate::model::Chapter {
                            title: "Opening".into(),
                            blocks: vec![crate::model::Block::Paragraph {
                                spans: vec![crate::model::Span::plain(row.text)],
                            }],
                        }],
                    )
                } else {
                    (row.text, Vec::new())
                };
                Book {
                    id: format!("book:{:06}", row.index),
                    title,
                    chapters,
                    ..Book::default()
                }
            })
            .collect::<Vec<_>>();
        BookSearchProvider::new(&books, &[])
    }

    /// Body pages are clamped and complete and resume inside oversized blocks and spans without prose copies.
    #[test]
    fn body_pages_clamp_and_resume_without_prose_copies() {
        // --- phase: body_pages_are_clamped_and_complete ---
        {
            let books = (0..257)
                .map(|i| Book {
                    id: format!("book:{i}"),
                    title: "Other".into(),
                    chapters: vec![crate::model::Chapter {
                        title: "Chapter".into(),
                        blocks: vec![crate::model::Block::Math {
                            expr: ui::math::MathExpr::new(),
                            source: "common body term".into(),
                            display: false,
                        }],
                    }],
                    ..Book::default()
                })
                .collect::<Vec<_>>();
            let provider = BookSearchProvider::new(&books, &[]);
            let mut cursor = None;
            let mut seen = 0;
            loop {
                let page = provider.search_page(&"common".into(), cursor.as_ref(), 17, 5);
                assert!(page.inspected <= 17);
                assert!(page.results.len() <= 5);
                seen += page.results.len();
                let Some(next) = page.next else { break };
                cursor = Some(next);
            }
            assert_eq!(seen, 257);
        }

        // --- phase: oversized_single_block_is_found_directly ---
        {
            let books = vec![Book {
                id: "huge".into(),
                title: "Huge".into(),
                chapters: vec![crate::model::Chapter {
                    title: "One block".into(),
                    blocks: vec![crate::model::Block::Paragraph {
                        spans: vec![crate::model::Span::plain(format!(
                            "{}needle-at-the-end",
                            "x".repeat(4_096)
                        ))],
                    }],
                }],
                ..Book::default()
            }];
            let provider = BookSearchProvider::new(&books, &[]);
            let page = provider.search_page(&"needle-at-the-end".into(), None, 1, 1);
            assert_eq!(page.inspected, 1);
            assert_eq!(page.results.len(), 1, "the index finds the tail directly");
        }

        // --- phase: million_scalar_multi_span_paragraph_is_found_directly ---
        {
            let mut tail = crate::model::Span::plain("tail-needle");
            tail.space_before = true;
            let books = vec![Book {
                id: "multi-span".into(),
                title: "Multi span".into(),
                chapters: vec![crate::model::Chapter {
                    title: "One paragraph".into(),
                    blocks: vec![crate::model::Block::Paragraph {
                        spans: vec![crate::model::Span::plain("x".repeat(1_000_000)), tail],
                    }],
                }],
                ..Book::default()
            }];
            let provider = BookSearchProvider::new(&books, &[]);
            let page = provider.search_page(&"tail-needle".into(), None, 1, 1);
            assert_eq!(page.inspected, 1);
            assert!(
                page.results
                    .iter()
                    .any(|result| result.kind == LOCATION_KIND)
            );
        }
    }

    /// Continuations from an earlier projection and duplicate rows fail closed.
    #[test]
    fn hostile_or_stale_cursors_and_rows_fail_closed() {
        // --- phase: duplicate_rows_fail_closed ---
        {
            let book = Book {
                id: "duplicate".into(),
                ..Book::default()
            };
            let provider = BookSearchProvider::new(&[book.clone(), book], &[]);
            assert!(provider.search("duplicate").is_empty());
            assert!(
                provider
                    .books
                    .resolve(&crate::resource::book_place_resource("duplicate"))
                    .is_none(),
                "an ambiguous projection publishes no row"
            );
        }
    }

    /// Exact book and annotation resources export the reader's actions, index labels compactly and invalidate stale sessions.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn exact_resources_share_the_reader_action_contracts() {
        // --- phase: exact_annotation_resource_exports_the_same_delete_action_as_the_reader ---
        {
            let mut note = Annotation::new("book", AnnoKind::Note, 0, 0).with_note("keep me");
            note.id = uuid::Uuid::from_u128(42);
            let books = vec![Book {
                id: "book".into(),
                ..Book::default()
            }];
            let provider = BookSearchProvider::new(&books, &[note.clone()]);
            let hit = provider.search("keep").pop().expect("annotation row");
            let resource = provider
                .books
                .resolve(&hit.action_id)
                .expect("exact annotation resource");
            let delete = resource
                .actions
                .iter()
                .find(|action| action.id == "delete-annotation")
                .expect("the library's delete, bound to the note");
            let encoded = delete
                .build_json(&serde_json::Map::new())
                .expect("a bound delete takes no arguments");
            let action: <crate::resource::Ebook as ui::tilo_resource::Resource>::Action =
                serde_json::from_slice(&encoded.payload).expect("the library's action");
            let mut library = ui::tilo_resource::Replica::<crate::resource::Ebook>::from_doc(
                crate::resource::doc_with(&[], std::slice::from_ref(&note)),
            );
            library.apply_action(&action).unwrap();
            assert!(
                library.state().unwrap().marks().is_empty(),
                "the search hit's delete deletes the note it names"
            );
        }

        // --- phase: exact_resources_preserve_book_and_annotation_action_contracts ---
        {
            let book = Book {
                id: "book".into(),
                title: "Contract book".into(),
                chapters: vec![crate::model::Chapter {
                    blocks: vec![crate::model::Block::Paragraph {
                        spans: vec![crate::model::Span::plain("body")],
                    }],
                    ..crate::model::Chapter::default()
                }],
                ..Book::default()
            };
            let group = uuid::Uuid::from_u128(7);
            let highlight = Annotation::new("book", AnnoKind::Highlight, 0, 0)
                .with_group(group)
                .with_excerpt("amber passage");
            let comment = Annotation::new("book", AnnoKind::Note, 0, 0)
                .with_group(group)
                .with_note("peer comment");
            let note = Annotation::new("book", AnnoKind::Note, 0, 0).with_note("reader note");
            let provider = BookSearchProvider::new(&[book], &[highlight, comment, note]);

            let book_resource = provider
                .books
                .resolve(
                    &provider
                        .search("contract book")
                        .into_iter()
                        .find(|result| result.kind == BOOK_KIND)
                        .map(|result| result.action_id)
                        .expect("book address"),
                )
                .expect("book resource");
            let share = book_resource
                .actions
                .iter()
                .find(|action| action.id == "share")
                .expect("share");
            assert!(matches!(
                share.contract.execution,
                tilo_actions::ExecutionMode::Background { .. }
            ));
            assert_eq!(share.contract.retry, tilo_actions::RetryPolicy::Idempotent);
            assert_eq!(share.contract.result, tilo_actions::ResultSpec::Job);
            let remove = book_resource
                .actions
                .iter()
                .find(|action| action.id == "remove-from-library")
                .expect("remove");
            assert_eq!(remove.contract.risk, RiskLevel::Destructive);
            assert_eq!(remove.contract.confirmation, ConfirmationPolicy::Confirm);
            assert!(remove.contract.reversible);

            let highlight_resource = provider
                .books
                .resolve(
                    &provider
                        .search("amber")
                        .pop()
                        .map(|result| result.action_id)
                        .expect("highlight address"),
                )
                .expect("highlight resource");
            let mut highlight_actions = highlight_resource
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>();
            highlight_actions.sort_unstable();
            assert_eq!(
                highlight_actions,
                ["add-comment", "change-color", "delete-highlight", "open"]
            );

            for (query, expected) in [
                ("peer comment", ["delete-annotation", "open"].as_slice()),
                ("reader note", ["delete-annotation", "open"].as_slice()),
            ] {
                let resource = provider
                    .books
                    .resolve(
                        &provider
                            .search(query)
                            .pop()
                            .map(|result| result.action_id)
                            .expect("annotation address"),
                    )
                    .expect("annotation resource");
                let mut actions = resource
                    .actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>();
                actions.sort_unstable();
                assert_eq!(actions, expected, "{query}");
            }
        }
    }
}
