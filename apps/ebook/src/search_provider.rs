//! Bounded, canonical global search for Books.
//!
//! Every hit is served from postings. Metadata rows (books, chapters,
//! annotations, remote library entries) keep their short searchable text,
//! whole-term postings and 1–3-scalar gram postings. Chapter prose is indexed
//! by gram postings only: the reader stays the sole owner of EPUB text, and a
//! candidate chapter is verified against the shared book snapshot a bounded
//! chunk at a time. A search therefore inspects the chapters that contain
//! every gram of the query rather than walking the library, and the body
//! index is reused by every rebuild that leaves the book set unchanged.

#![allow(
    clippy::indexing_slicing,
    clippy::map_unwrap_or,
    clippy::obfuscated_if_else,
    clippy::redundant_else,
    clippy::string_slice,
    reason = "all cursor and KMP indices are advanced from checked lengths and UTF-8 character boundaries; using Option plumbing here would obscure those invariants"
)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tilo_actions::{
    ActionAddress, ActionContract, ActionSpec, ConfirmationPolicy, ExecutionMode, OperationKind,
    Resource, ResultSpec, RetryPolicy, RiskLevel,
};
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_CANDIDATE_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, MAX_SEARCH_QUERY_SCALARS,
    SearchCursor, SearchPage, SearchProvider, SearchResult,
};

use crate::actions::{
    CommentDraft, EBOOK_OPERATION_TYPE, EbookOperation, NoteDraft, change_color_action,
    delete_annotation_action, delete_highlight_action, edit_note_action, encode,
    remove_library_book_action,
};
use crate::model::{AnnoKind, Annotation, AnnotationAnchor, Book, HiColor, LibraryEntry};

pub const BOOK_KIND: &str = "Books";
pub const CHAPTER_KIND: &str = "Chapters";
pub const ANNOTATION_KIND: &str = "Annotations";
/// A precise, lazy full-text match. Unlike a chapter row this resource names
/// the exact immutable block that satisfied a query, so activating a search
/// hit never guesses at a nearby reading location.
pub const LOCATION_KIND: &str = "Reading locations";

const BODY_PHASE: u64 = 1_u64 << 63;
/// One verification unit.  This is deliberately smaller than the maximum
/// interactive query so an EPUB with one enormous paragraph still yields.
const BODY_CHUNK_SCALARS: usize = 1_024;

fn chapter_label(chapter: &crate::model::Chapter, index: usize) -> String {
    if chapter.title.trim().is_empty() {
        format!("Chapter {}", index + 1)
    } else {
        chapter.title.clone()
    }
}

/// Diagnostics distinguish the compact directory from reader-owned prose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BookSearchStats {
    pub rows: usize,
    pub metadata_tokens: usize,
    pub metadata_postings: usize,
    pub metadata_gram_postings: usize,
    /// Chapter-level gram postings. Prose itself is never copied.
    pub body_postings: usize,
    pub retained_chapter_scalars: usize,
    pub generation: u64,
}

/// Up to [`GRAM_WIDTH`] case-folded scalars packed into one key. Each scalar
/// is stored as `scalar + 1` in its own 21-bit lane, so a gram never collides
/// with a shorter one that shares its prefix.
type Gram = u64;
const GRAM_WIDTH: usize = 3;
const GRAM_LANE_SHIFTS: [u32; GRAM_WIDTH] = [0, 21, 42];

fn pack_gram(chars: &[char]) -> Gram {
    chars
        .iter()
        .zip(GRAM_LANE_SHIFTS)
        .fold(0, |key, (character, shift)| {
            key | u64::from(u32::from(*character))
                .wrapping_add(1)
                .wrapping_shl(shift)
        })
}

/// The distinct grams a folded query needs a record to contain: the whole
/// query when it is short, otherwise every [`GRAM_WIDTH`]-scalar window.
fn query_grams(needle: &[char]) -> Vec<Gram> {
    let mut grams: Vec<Gram> = if needle.len() <= GRAM_WIDTH {
        vec![pack_gram(needle)]
    } else {
        needle.windows(GRAM_WIDTH).map(pack_gram).collect()
    };
    grams.sort_unstable();
    grams.dedup();
    grams
}

/// Gram postings over rows added in ascending order. A row containing a
/// query contains every one of its [`query_grams`], so the rarest of their
/// postings is a complete candidate list and the others filter it before any
/// text is verified.
#[derive(Clone, Debug, Default)]
struct GramPostings {
    postings: HashMap<Gram, Vec<u32>>,
    count: usize,
}

impl GramPostings {
    /// Index one row's already case-folded text. Rows must arrive in
    /// ascending order, which keeps every posting sorted and deduplicated.
    /// Consecutive calls for the same row continue one text stream.
    fn add(&mut self, row: u32, folded: impl IntoIterator<Item = char>, window: &mut GramWindow) {
        for character in folded {
            window.push(character);
            for width in 1..=window.filled {
                self.insert(pack_gram(&window.chars[GRAM_WIDTH - width..]), row);
            }
        }
    }

    fn insert(&mut self, gram: Gram, row: u32) {
        let posting = self.postings.entry(gram).or_default();
        if posting.last() != Some(&row) {
            posting.push(row);
            self.count = self.count.saturating_add(1);
        }
    }

    /// The rarest posting among `grams`, or an empty list when a gram occurs
    /// nowhere and so nothing can match.
    fn rarest(&self, grams: &[Gram]) -> &[u32] {
        let mut rarest: Option<&[u32]> = None;
        for gram in grams {
            let Some(posting) = self.postings.get(gram) else {
                return &[];
            };
            if rarest.is_none_or(|current| posting.len() < current.len()) {
                rarest = Some(posting);
            }
        }
        rarest.unwrap_or(&[])
    }

    fn contains_all(&self, grams: &[Gram], row: u32) -> bool {
        grams.iter().all(|gram| {
            self.postings
                .get(gram)
                .is_some_and(|posting| posting.binary_search(&row).is_ok())
        })
    }
}

/// The last [`GRAM_WIDTH`] scalars of a text stream being indexed.
#[derive(Default)]
struct GramWindow {
    chars: [char; GRAM_WIDTH],
    filled: usize,
}

impl GramWindow {
    fn push(&mut self, character: char) {
        self.chars.rotate_left(1);
        self.chars[GRAM_WIDTH - 1] = character;
        self.filled = self.filled.saturating_add(1).min(GRAM_WIDTH);
    }
}

/// Case-fold text exactly as [`FoldMatcher`] does, so indexed grams and
/// verified text agree.
fn fold_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().flat_map(char::to_lowercase)
}

/// Feed a chapter's text to `sink` in the order and with the separators the
/// streaming verifier sees: a space before every later block that has text,
/// and before each later span that carries `space_before`.
fn chapter_text(chapter: &crate::model::Chapter, mut sink: impl FnMut(&str)) {
    for (block_index, block) in chapter.blocks.iter().enumerate() {
        match block {
            crate::model::Block::Math { source, .. } => {
                if block_index > 0 {
                    sink(" ");
                }
                sink(source);
            }
            crate::model::Block::Separator => {}
            _ => {
                for (span_index, span) in block.spans().iter().enumerate() {
                    if (span_index == 0 && block_index > 0) || (span_index > 0 && span.space_before)
                    {
                        sink(" ");
                    }
                    sink(&span.text);
                }
            }
        }
    }
}

/// Index every chapter's prose under its row in the entry list, where each
/// book row is followed by one row per chapter. `None` when the rows do not
/// fit the posting width.
fn build_body_index(books: &[Arc<Book>]) -> Option<GramPostings> {
    let mut body = GramPostings::default();
    let mut row = 0u32;
    for book in books {
        row = row.checked_add(1)?;
        for chapter in &book.chapters {
            let mut window = GramWindow::default();
            chapter_text(chapter, |text| body.add(row, fold_chars(text), &mut window));
            row = row.checked_add(1)?;
        }
    }
    Some(body)
}

#[derive(Clone)]
enum SearchTarget {
    Book(usize),
    Chapter {
        book: usize,
        chapter: usize,
    },
    Annotation {
        index: usize,
        hydrated: bool,
        comment: bool,
    },
    RemoteBook(usize),
}

#[derive(Clone)]
struct SearchEntry {
    result: SearchResult,
    target: SearchTarget,
    /// Case-folded metadata text this row is indexed and verified by.
    folded: String,
}

/// Outcome of one bounded body-verification unit.
enum BodyChunk {
    Scanned {
        matched: bool,
        next_span: usize,
        next_byte: usize,
        matcher_state: usize,
        done: bool,
    },
    Done,
    Invalid,
}

#[derive(Clone)]
pub struct BookSearchProvider {
    // The live app and each palette session share these Arcs. No EPUB body is
    // cloned solely for searching.
    books: Arc<Vec<Arc<Book>>>,
    annotations: Arc<Vec<Annotation>>,
    registry: Arc<Vec<LibraryEntry>>,
    entries: Arc<Vec<SearchEntry>>,
    metadata_tokens: Arc<HashMap<String, Vec<u32>>>,
    metadata_grams: Arc<GramPostings>,
    /// Chapter prose, keyed by the chapter's row in `entries`. Chapter rows
    /// depend only on the book list, so rebuilds over the same books share it.
    body: Arc<GramPostings>,
    resource_rows: Arc<BTreeMap<String, usize>>,
    action_rows: Arc<Vec<(String, SearchResult)>>,
    action_tokens: Arc<HashMap<String, Vec<u32>>>,
    action_grams: Arc<GramPostings>,
    generation: u64,
    available: bool,
}

impl BookSearchProvider {
    /// Compatibility constructor for small callers and tests. The live app
    /// uses [`Self::new_shared`] so its content cache is not copied.
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
    /// when the book set is unchanged. Annotation and library edits then
    /// rebuild only the metadata rows, never re-read every book.
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
        let mut tokens = HashMap::<String, Vec<u32>>::new();
        let mut metadata_grams = GramPostings::default();
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
                    (!entry.title.trim().is_empty())
                        .then(|| entry.title.clone())
                        .unwrap_or_else(|| "Untitled".into()),
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

        let mut add = |result: SearchResult, target: SearchTarget, metadata: &str| -> bool {
            let Some(address) = result.action_address() else {
                return false;
            };
            let row = entries.len();
            let Ok(posting_row) = u32::try_from(row) else {
                return false;
            };
            if resource_rows.insert(address.resource, row).is_some() {
                return false;
            }
            let folded: String = fold_chars(metadata).collect();
            index_row(&mut tokens, &mut metadata_grams, posting_row, &folded);
            entries.push(SearchEntry {
                result,
                target,
                folded,
            });
            true
        };

        for (book_index, book) in books.iter().enumerate() {
            let title = Self::book_title(book).to_string();
            let mut result =
                SearchResult::for_action(BOOK_KIND, title.clone(), Self::book_address(&book.id))
                    .with_icon(Icon::FileText);
            if !book.author.trim().is_empty() {
                result = result.with_subtitle(book.author.clone());
            }
            if !add(
                result,
                SearchTarget::Book(book_index),
                &format!("{title} {}", book.author),
            ) {
                return Self::unavailable(books, annotations, registry, generation);
            }
            for (chapter_index, chapter) in book.chapters.iter().enumerate() {
                let chapter_title = chapter_label(chapter, chapter_index);
                let result = SearchResult::for_action(
                    CHAPTER_KIND,
                    chapter_title.clone(),
                    Self::chapter_address(&book.id, chapter_index),
                )
                .with_subtitle(format!("{} · Chapter {}", title, chapter_index + 1))
                .with_icon(Icon::List);
                if !add(
                    result,
                    SearchTarget::Chapter {
                        book: book_index,
                        chapter: chapter_index,
                    },
                    &format!("{title} {} {chapter_title}", book.author),
                ) {
                    return Self::unavailable(books, annotations, registry, generation);
                }
            }
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
            let mut result =
                SearchResult::for_action(BOOK_KIND, title.clone(), Self::book_address(&entry.id))
                    .with_icon(Icon::FileText);
            if !entry.author.trim().is_empty() {
                result = result.with_subtitle(entry.author.clone());
            }
            if !add(
                result,
                SearchTarget::RemoteBook(registry_index),
                &format!("{title} {} {}", entry.author, entry.subject),
            ) {
                return Self::unavailable(books, annotations, registry, generation);
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
            let result = SearchResult::for_action(
                ANNOTATION_KIND,
                Self::preview(annotation),
                Self::annotation_address(annotation, comment),
            )
            .with_subtitle(format!(
                "{title} · {} · Chapter {}",
                annotation.kind.label(),
                annotation.chapter + 1
            ))
            .with_icon(match annotation.kind {
                AnnoKind::Bookmark => Icon::Star,
                _ => Icon::List,
            });
            if !add(
                result,
                SearchTarget::Annotation {
                    index: annotation_index,
                    hydrated: hydrated.contains(annotation.doc_id.as_str()),
                    comment,
                },
                &format!(
                    "{title} {author} {}{} {} {}",
                    annotation.kind.label(),
                    comment.then_some(" comment").unwrap_or(""),
                    annotation.excerpt,
                    annotation.note
                ),
            ) {
                return Self::unavailable(books, annotations, registry, generation);
            }
        }
        let body = match prior {
            Some(prior) if prior.indexes_the_same_books(&books) => Arc::clone(&prior.body),
            _ => match build_body_index(&books) {
                Some(body) => Arc::new(body),
                None => return Self::unavailable(books, annotations, registry, generation),
            },
        };
        let mut action_rows = Vec::new();
        let mut action_tokens = HashMap::<String, Vec<u32>>::new();
        let mut action_grams = GramPostings::default();
        for entry in &entries {
            let Some(address) = entry.result.action_address() else {
                continue;
            };
            let actions: &[(&str, &str)] = match entry.target {
                SearchTarget::Book(_) => &[
                    ("Open", "open"),
                    ("Share with workspace", "share"),
                    ("Remove from library", "remove-from-library"),
                ],
                SearchTarget::RemoteBook(_) => &[
                    ("Download and open", "open"),
                    ("Remove from library", "remove-from-library"),
                ],
                SearchTarget::Chapter { .. } => &[("Open", "open")],
                SearchTarget::Annotation {
                    hydrated: true,
                    comment,
                    index,
                } => {
                    let Some(annotation) = annotations.get(index) else {
                        continue;
                    };
                    match annotation.kind {
                        AnnoKind::Highlight => &[
                            ("Open", "open"),
                            ("Change colour", "change-color"),
                            ("Delete highlight", "delete-highlight"),
                            ("Add comment", "add-comment"),
                        ],
                        AnnoKind::Bookmark => &[("Open", "open"), ("Delete bookmark", "delete")],
                        AnnoKind::Note if comment => &[
                            ("Open", "open"),
                            ("Edit comment", "edit-comment"),
                            ("Delete comment", "delete-comment"),
                        ],
                        AnnoKind::Note => &[
                            ("Open", "open"),
                            ("Edit note", "edit"),
                            ("Delete note", "delete"),
                        ],
                    }
                }
                SearchTarget::Annotation {
                    hydrated: false, ..
                } => continue,
            };
            for (label, action) in actions {
                let Ok(row) = u32::try_from(action_rows.len()) else {
                    return Self::unavailable(books, annotations, registry, generation);
                };
                let result = SearchResult::for_action(
                    "Book actions",
                    *label,
                    ActionAddress::new(address.resource.clone(), *action),
                )
                .with_subtitle(entry.result.title.clone());
                let text = format!("{label} {}", entry.result.title);
                index_action_row(&mut action_tokens, &mut action_grams, row, &text);
                action_rows.push((text, result));
            }
        }
        Self {
            books,
            annotations,
            registry,
            entries: Arc::new(entries),
            metadata_tokens: Arc::new(tokens),
            metadata_grams: Arc::new(metadata_grams),
            body,
            resource_rows: Arc::new(resource_rows),
            action_rows: Arc::new(action_rows),
            action_tokens: Arc::new(action_tokens),
            action_grams: Arc::new(action_grams),
            generation,
            available: generation != 0,
        }
    }

    /// Whether this provider's body index was built over exactly `books`, in
    /// order. Book snapshots are immutable and shared, so pointer identity
    /// means identical prose and identical chapter rows.
    fn indexes_the_same_books(&self, books: &[Arc<Book>]) -> bool {
        self.available
            && self.books.len() == books.len()
            && self
                .books
                .iter()
                .zip(books)
                .all(|(indexed, book)| Arc::ptr_eq(indexed, book))
    }

    fn unavailable(
        books: Arc<Vec<Arc<Book>>>,
        annotations: Arc<Vec<Annotation>>,
        registry: Arc<Vec<LibraryEntry>>,
        generation: u64,
    ) -> Self {
        Self {
            books,
            annotations,
            registry,
            entries: Arc::new(Vec::new()),
            metadata_tokens: Arc::new(HashMap::new()),
            metadata_grams: Arc::new(GramPostings::default()),
            body: Arc::new(GramPostings::default()),
            resource_rows: Arc::new(BTreeMap::new()),
            action_rows: Arc::new(Vec::new()),
            action_tokens: Arc::new(HashMap::new()),
            action_grams: Arc::new(GramPostings::default()),
            generation,
            available: false,
        }
    }

    #[must_use]
    pub fn stats(&self) -> BookSearchStats {
        BookSearchStats {
            rows: self.entries.len(),
            metadata_tokens: self.metadata_tokens.len(),
            metadata_postings: self.metadata_tokens.values().map(Vec::len).sum(),
            metadata_gram_postings: self.metadata_grams.count,
            body_postings: self.body.count,
            retained_chapter_scalars: 0,
            generation: self.generation,
        }
    }

    fn book_title(book: &Book) -> &str {
        if book.title.trim().is_empty() {
            "Untitled"
        } else {
            &book.title
        }
    }

    fn book_address(id: &str) -> ActionAddress {
        ActionAddress::new(format!("ebook.book:{id}"), "open")
    }

    fn chapter_address(book_id: &str, chapter: usize) -> ActionAddress {
        ActionAddress::new(format!("ebook.chapter:{book_id}:{chapter}"), "open")
    }

    fn annotation_address(annotation: &Annotation, comment: bool) -> ActionAddress {
        let resource = match annotation.kind {
            AnnoKind::Highlight => format!("ebook.highlight:{}", annotation.group),
            AnnoKind::Bookmark => format!("ebook.bookmark:{}", annotation.id),
            AnnoKind::Note if comment => format!("ebook.comment:{}", annotation.id),
            AnnoKind::Note => format!("ebook.note:{}", annotation.id),
        };
        ActionAddress::new(resource, "open")
    }

    fn preview(annotation: &Annotation) -> String {
        let text = if annotation.note.trim().is_empty() {
            annotation.excerpt.trim()
        } else {
            annotation.note.trim()
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

    /// `query` is already folded by [`normalize_query`], the same fold the
    /// row's text was stored and indexed under.
    fn metadata_matches(entry: &SearchEntry, query: &str) -> bool {
        entry.folded.contains(query)
    }

    fn body_chunk_matches(
        &self,
        entry: &SearchEntry,
        query: &str,
        block_index: usize,
        span_index: usize,
        byte_offset: usize,
        matcher_state: usize,
    ) -> BodyChunk {
        let SearchTarget::Chapter { book, chapter } = entry.target else {
            return BodyChunk::Done;
        };
        let Some(chapter) = self
            .books
            .get(book)
            .and_then(|book| book.chapters.get(chapter))
        else {
            return BodyChunk::Invalid;
        };
        let Some(block) = chapter.blocks.get(block_index) else {
            // The scanner emits this exact one-past-the-last-block sentinel
            // after a fully consumed final block. Any other offset is an
            // untrusted cursor, not permission to skip prose.
            return (block_index == chapter.blocks.len() && span_index == 0 && byte_offset == 0)
                .then_some(BodyChunk::Done)
                .unwrap_or(BodyChunk::Invalid);
        };
        let Some(mut matcher) = FoldMatcher::with_state(query, matcher_state) else {
            return BodyChunk::Invalid;
        };
        match block {
            crate::model::Block::Math { source, .. } => {
                if span_index != 0 {
                    return BodyChunk::Invalid;
                }
                // A block boundary is textual too. Feed it once at the start
                // of the next valid block, so cross-block phrases survive a
                // chunk yield without making a concatenated chapter string.
                if byte_offset == 0 && block_index > 0 && matcher.push(' ') {
                    return BodyChunk::Scanned {
                        matched: true,
                        next_span: span_index,
                        next_byte: byte_offset,
                        matcher_state: matcher.state(),
                        done: false,
                    };
                }
                Self::source_body_chunk(source, span_index, byte_offset, &mut matcher)
            }
            crate::model::Block::Separator => {
                if span_index != 0 || byte_offset != 0 {
                    return BodyChunk::Invalid;
                }
                BodyChunk::Scanned {
                    matched: false,
                    next_span: span_index,
                    next_byte: byte_offset,
                    matcher_state: matcher.state(),
                    done: true,
                }
            }
            _ => {
                let Some(span) = block.spans().get(span_index) else {
                    if block.spans().is_empty() && span_index == 0 && byte_offset == 0 {
                        return BodyChunk::Scanned {
                            matched: false,
                            next_span: span_index,
                            next_byte: byte_offset,
                            matcher_state: matcher.state(),
                            done: true,
                        };
                    }
                    return BodyChunk::Invalid;
                };
                if byte_offset == 0 && block_index > 0 && span_index == 0 && matcher.push(' ') {
                    return BodyChunk::Scanned {
                        matched: true,
                        next_span: span_index,
                        next_byte: byte_offset,
                        matcher_state: matcher.state(),
                        done: false,
                    };
                }
                if byte_offset == 0 && span_index > 0 && span.space_before && matcher.push(' ') {
                    return BodyChunk::Scanned {
                        matched: true,
                        next_span: span_index,
                        next_byte: byte_offset,
                        matcher_state: matcher.state(),
                        done: false,
                    };
                }
                Self::source_body_chunk(&span.text, span_index, byte_offset, &mut matcher)
            }
        }
    }

    fn source_body_chunk(
        source: &str,
        span_index: usize,
        byte_offset: usize,
        matcher: &mut FoldMatcher,
    ) -> BodyChunk {
        if byte_offset > source.len() || !source.is_char_boundary(byte_offset) {
            return BodyChunk::Invalid;
        }
        let (start, end, done) = source_chunk(source, byte_offset);
        let found = source[start..end]
            .chars()
            .any(|character| matcher.push(character));
        BodyChunk::Scanned {
            matched: found,
            next_span: span_index,
            next_byte: end,
            matcher_state: matcher.state(),
            done,
        }
    }

    fn chapter_block_count(&self, entry: &SearchEntry) -> Option<usize> {
        let SearchTarget::Chapter { book, chapter } = entry.target else {
            return None;
        };
        Some(self.books.get(book)?.chapters.get(chapter)?.blocks.len())
    }

    fn next_body_position(
        &self,
        entry: &SearchEntry,
        block_index: usize,
        span_index: usize,
    ) -> Option<(usize, usize)> {
        let SearchTarget::Chapter { book, chapter } = entry.target else {
            return None;
        };
        let block = self
            .books
            .get(book)?
            .chapters
            .get(chapter)?
            .blocks
            .get(block_index)?;
        if matches!(block, crate::model::Block::Math { .. })
            || span_index.saturating_add(1) >= block.spans().len()
        {
            Some((block_index.saturating_add(1), 0))
        } else {
            Some((block_index, span_index.saturating_add(1)))
        }
    }

    /// Build one canonical, block-level full-text hit without retaining an
    /// excerpt or another copy of EPUB prose. The stable resource id is
    /// resolved lazily on activation against the same immutable book snapshot.
    fn body_result(&self, entry: &SearchEntry, block: usize) -> Option<SearchResult> {
        let SearchTarget::Chapter {
            book: book_index,
            chapter: chapter_index,
        } = entry.target
        else {
            return None;
        };
        let book = self.books.get(book_index)?;
        let chapter = book.chapters.get(chapter_index)?;
        chapter.blocks.get(block)?;
        let chapter_title = chapter_label(chapter, chapter_index);
        Some(
            SearchResult::for_action(
                LOCATION_KIND,
                format!("Match in {chapter_title}"),
                Self::location_address(&book.id, chapter_index, block),
            )
            .with_subtitle(format!("{} · {chapter_title}", Self::book_title(book)))
            .with_icon(Icon::FileText),
        )
    }

    fn location_address(book_id: &str, chapter: usize, block: usize) -> ActionAddress {
        ActionAddress::new(
            format!("ebook.location:{book_id}:{chapter}:{block}"),
            "open",
        )
    }

    /// Resolve a full-text location resource exactly. This performs a bounded
    /// metadata lookup plus one block bounds check; it does not construct a
    /// prose index or hydrate unrelated chapters.
    fn location_resource_for(&self, address: &ActionAddress) -> Option<Resource> {
        let suffix = address.resource.strip_prefix("ebook.location:")?;
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
        let resource = Resource::new(
            &address.resource,
            "ebook.location",
            format!("{} · {chapter_title}", Self::book_title(book)),
        )
        .with_action(
            ActionSpec::operation(
                "open",
                "Open match",
                OperationKind::View,
                EBOOK_OPERATION_TYPE,
                &EbookOperation::Reveal {
                    book_id: book.id.clone(),
                    chapter,
                    page: 0,
                    block: Some(block),
                },
            )
            .ok()?,
        );
        resource
            .actions
            .iter()
            .any(|action| action.id == address.action)
            .then_some(resource)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "all exact action variants are kept together so their canonical payloads can be audited"
    )]
    fn resource_for(&self, entry: &SearchEntry, address: &ActionAddress) -> Option<Resource> {
        if let SearchTarget::RemoteBook(index) = entry.target {
            let remote = self.registry.get(index)?;
            // A remote metadata hit is never a dead reveal. The operation is
            // typed and derives its only path from the content id, so opening
            // a peer's result remains a capability-safe workspace hydrate.
            let hydrate = ActionSpec::operation(
                "open",
                "Download and open",
                OperationKind::Effect,
                EBOOK_OPERATION_TYPE,
                &EbookOperation::HydrateSharedBook {
                    book_id: remote.id.clone(),
                },
            )
            .ok()?
            .with_contract(ActionContract {
                risk: RiskLevel::Low,
                retry: RetryPolicy::Idempotent,
                execution: ExecutionMode::Background { cancellable: true },
                result: ResultSpec::Job,
                tags: vec!["workspace".into(), "materialize".into(), "remote".into()],
                ..ActionContract::default()
            });
            let remove_from_library = ActionSpec::immediate(
                "remove-from-library",
                "Remove from library",
                crate::actions::EBOOK_ACTION_TYPE,
                &remove_library_book_action(&remote.id),
            )
            .ok()?
            .with_contract(ActionContract {
                risk: RiskLevel::Destructive,
                confirmation: ConfirmationPolicy::Confirm,
                retry: RetryPolicy::Idempotent,
                reversible: true,
                tags: vec!["library".into(), "tombstone".into()],
                ..ActionContract::default()
            });
            return Some(
                Resource::new(&address.resource, "ebook.book", &entry.result.title)
                    .with_action(hydrate)
                    .with_action(remove_from_library),
            );
        }
        let (kind, book_id, chapter, block) = match entry.target {
            SearchTarget::Book(index) => {
                let book = self.books.get(index)?;
                ("ebook.book", book.id.as_str(), 0, None)
            }
            SearchTarget::Chapter { book, chapter } => {
                let book = self.books.get(book)?;
                book.chapters.get(chapter)?;
                ("ebook.chapter", book.id.as_str(), chapter, None)
            }
            SearchTarget::Annotation {
                index,
                hydrated,
                comment,
            } => {
                let annotation = self.annotations.get(index)?;
                if !hydrated {
                    return None;
                }
                let kind = match annotation.kind {
                    AnnoKind::Highlight => "ebook.highlight",
                    AnnoKind::Bookmark => "ebook.bookmark",
                    AnnoKind::Note if comment => "ebook.comment",
                    AnnoKind::Note => "ebook.note",
                };
                (
                    kind,
                    annotation.doc_id.as_str(),
                    annotation.chapter,
                    Some(annotation.block),
                )
            }
            SearchTarget::RemoteBook(_) => return None,
        };
        let action = ActionSpec::operation(
            "open",
            "Open",
            OperationKind::View,
            EBOOK_OPERATION_TYPE,
            &EbookOperation::Reveal {
                book_id: book_id.to_string(),
                chapter,
                page: 0,
                block,
            },
        )
        .ok()?;
        let mut resource =
            Resource::new(&address.resource, kind, &entry.result.title).with_action(action);
        if matches!(entry.target, SearchTarget::Book(_)) {
            let share = ActionSpec::operation(
                "share",
                "Share with workspace",
                OperationKind::Effect,
                EBOOK_OPERATION_TYPE,
                &EbookOperation::ShareBook {
                    book_id: book_id.to_string(),
                },
            )
            .ok()?
            .with_contract(ActionContract {
                risk: RiskLevel::Low,
                retry: RetryPolicy::Idempotent,
                execution: ExecutionMode::Background { cancellable: true },
                result: ResultSpec::Job,
                tags: vec!["workspace".into(), "materialize".into()],
                ..ActionContract::default()
            });
            let remove = ActionSpec::immediate(
                "remove-from-library",
                "Remove from library",
                crate::actions::EBOOK_ACTION_TYPE,
                &remove_library_book_action(book_id),
            )
            .ok()?
            .with_contract(ActionContract {
                risk: RiskLevel::Destructive,
                confirmation: ConfirmationPolicy::Confirm,
                retry: RetryPolicy::Idempotent,
                reversible: true,
                tags: vec!["library".into(), "tombstone".into()],
                ..ActionContract::default()
            });
            resource = resource.with_action(share).with_action(remove);
        }
        if let SearchTarget::Annotation { index, comment, .. } = entry.target {
            let annotation = self.annotations.get(index)?;
            match annotation.kind {
                AnnoKind::Highlight => {
                    let group = annotation.group;
                    resource = resource.with_action(ActionSpec::prompted(
                        "change-color",
                        "Change colour",
                        "Colour",
                        "yellow, green, blue, pink, purple",
                        tilo_actions::ExecutionLocus::ClientLocal,
                        move |name| {
                            encode(&change_color_action(group, HiColor::from_str(name.trim())))
                        },
                    ));
                    let delete = ActionSpec::immediate(
                        "delete-highlight",
                        "Delete highlight",
                        crate::actions::EBOOK_ACTION_TYPE,
                        &delete_highlight_action(annotation.group),
                    )
                    .ok()?;
                    let book_id = annotation.doc_id.clone();
                    let anchor = AnnotationAnchor {
                        chapter: annotation.chapter,
                        block: annotation.block,
                        start: annotation.start,
                        end: annotation.end,
                        excerpt: annotation.excerpt.clone(),
                    };
                    let color = annotation.color;
                    resource = resource
                        .with_action(delete)
                        .with_action(ActionSpec::prompted(
                            "add-comment",
                            "Add comment",
                            "Comment",
                            "your comment…",
                            tilo_actions::ExecutionLocus::ClientLocal,
                            move |text| {
                                encode(
                                    &CommentDraft {
                                        group,
                                        note: NoteDraft {
                                            book_id: book_id.clone(),
                                            id: uuid::Uuid::new_v4(),
                                            anchor: anchor.clone(),
                                            color,
                                            text: text.to_string(),
                                        },
                                    }
                                    .into_action(),
                                )
                            },
                        ));
                }
                AnnoKind::Bookmark => {
                    let delete = ActionSpec::immediate(
                        "delete",
                        "Delete bookmark",
                        crate::actions::EBOOK_ACTION_TYPE,
                        &delete_annotation_action(annotation.id),
                    )
                    .ok()?;
                    resource = resource.with_action(delete);
                }
                AnnoKind::Note => {
                    let id = annotation.id;
                    let edit_id = if comment { "edit-comment" } else { "edit" };
                    let edit_label = if comment { "Edit comment" } else { "Edit note" };
                    let delete_id = if comment { "delete-comment" } else { "delete" };
                    let delete_label = if comment {
                        "Delete comment"
                    } else {
                        "Delete note"
                    };
                    resource = resource.with_action(ActionSpec::prompted(
                        edit_id,
                        edit_label,
                        if comment { "Comment" } else { "Note" },
                        if comment {
                            "edit the comment…"
                        } else {
                            "edit the note…"
                        },
                        tilo_actions::ExecutionLocus::ClientLocal,
                        move |text| encode(&edit_note_action(id, text)),
                    ));
                    let delete = ActionSpec::immediate(
                        delete_id,
                        delete_label,
                        crate::actions::EBOOK_ACTION_TYPE,
                        &delete_annotation_action(id),
                    )
                    .ok()?;
                    resource = resource.with_action(delete);
                }
            }
        }
        Some(resource)
    }

    fn page_start(&self, query: &str, cursor: Option<&SearchCursor>) -> (bool, usize) {
        cursor
            .and_then(|cursor| {
                let (generation, cursor_query, _, ordinal) = cursor.opaque_parts();
                (generation == self.generation && cursor_query == query)
                    .then(|| ordinal.unwrap_or_default())
            })
            .map(|ordinal| {
                (
                    (ordinal & BODY_PHASE) != 0,
                    (ordinal & !BODY_PHASE) as usize,
                )
            })
            .unwrap_or((false, 0))
    }

    fn next_cursor(&self, query: &str, body: bool, offset: usize) -> SearchCursor {
        let phase = body.then_some(BODY_PHASE).unwrap_or_default();
        SearchCursor::opaque(self.generation, query, None, Some(phase | offset as u64))
    }

    fn body_start(cursor: &SearchCursor) -> Option<(usize, usize, usize, usize, usize)> {
        let state = cursor.opaque_parts().2?;
        let mut parts = state.strip_prefix("body:")?.split(':');
        let (Some(entry), Some(block), Some(span), Some(byte), Some(matcher_state)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return None;
        };
        if parts.next().is_some() {
            return None;
        }
        match (
            entry.parse(),
            block.parse(),
            span.parse(),
            byte.parse(),
            matcher_state.parse(),
        ) {
            (Ok(entry), Ok(block), Ok(span), Ok(byte), Ok(matcher_state)) => {
                Some((entry, block, span, byte, matcher_state))
            }
            _ => None,
        }
    }

    fn next_body_cursor(
        &self,
        query: &str,
        entry: usize,
        block: usize,
        span: usize,
        byte: usize,
        matcher_state: usize,
    ) -> SearchCursor {
        SearchCursor::opaque(
            self.generation,
            query,
            Some(format!(
                "body:{entry}:{block}:{span}:{byte}:{matcher_state}"
            )),
            Some(BODY_PHASE),
        )
    }

    /// Chapter rows whose prose contains every gram of a folded, non-empty
    /// query, with the grams to re-check each one against.
    fn body_candidates(&self, query: &str) -> (&[u32], Vec<Gram>) {
        let needle: Vec<char> = query.chars().collect();
        let grams = query_grams(&needle);
        (self.body.rarest(&grams), grams)
    }

    pub(crate) fn action_search_provider(&self) -> BookActionSearchProvider {
        BookActionSearchProvider {
            books: self.clone(),
            rows: Arc::clone(&self.action_rows),
            tokens: Arc::clone(&self.action_tokens),
            grams: Arc::clone(&self.action_grams),
            live_resources: Arc::new(BTreeMap::new()),
            live_epoch: Arc::new(AtomicU64::new(self.generation)),
            snapshot_epoch: self.generation,
        }
    }
}

/// Compact action-label index. It retains labels and canonical addresses only;
/// exact resources are still rebuilt from the live book snapshot on activation.
#[derive(Clone)]
pub(crate) struct BookActionSearchProvider {
    books: BookSearchProvider,
    rows: Arc<Vec<(String, SearchResult)>>,
    tokens: Arc<HashMap<String, Vec<u32>>>,
    grams: Arc<GramPostings>,
    live_resources: Arc<BTreeMap<String, Resource>>,
    live_epoch: Arc<AtomicU64>,
    snapshot_epoch: u64,
}

impl BookActionSearchProvider {
    pub(crate) fn with_live_resources(
        mut self,
        resources: Vec<Resource>,
        live_epoch: Arc<AtomicU64>,
        snapshot_epoch: u64,
    ) -> Self {
        let mut rows = (*self.rows).clone();
        let mut tokens = (*self.tokens).clone();
        let mut grams = (*self.grams).clone();
        let mut exact = BTreeMap::new();
        let mut seen = rows
            .iter()
            .filter_map(|(_, result)| result.action_address())
            .map(|address| (address.resource, address.action))
            .collect::<HashSet<_>>();
        for resource in resources {
            for action in &resource.actions {
                if !seen.insert((resource.id.as_str().to_string(), action.id.clone())) {
                    continue;
                }
                let Ok(row) = u32::try_from(rows.len()) else {
                    continue;
                };
                let result = SearchResult::for_action(
                    "Book actions",
                    action.label.clone(),
                    ActionAddress::new(resource.id.as_str(), action.id.as_str()),
                )
                .with_subtitle(resource.label.clone());
                let text = format!("{} {}", action.label, resource.label);
                index_action_row(&mut tokens, &mut grams, row, &text);
                rows.push((text, result));
            }
            exact.insert(resource.id.as_str().to_string(), resource);
        }
        self.rows = Arc::new(rows);
        self.tokens = Arc::new(tokens);
        self.grams = Arc::new(grams);
        self.live_resources = Arc::new(exact);
        self.live_epoch = live_epoch;
        self.snapshot_epoch = snapshot_epoch;
        self
    }
}

impl SearchProvider for BookActionSearchProvider {
    fn name(&self) -> &'static str {
        "Book actions"
    }

    fn action_directory_generation(&self) -> Option<u64> {
        (self.live_epoch.load(Ordering::Acquire) == self.snapshot_epoch)
            .then_some(self.snapshot_epoch)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.live_epoch.load(Ordering::Acquire) == self.snapshot_epoch
            && cursor.opaque_parts().0 == self.snapshot_epoch
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.search_page(query, None, DEFAULT_SEARCH_CANDIDATE_LIMIT, limit)
            .results
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let Some(query) = normalize_query(query) else {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        };
        // A palette session is a snapshot.  A caller may start a page without
        // a cursor, so check the live revision here as well as when decoding a
        // continuation below; otherwise an old provider can quietly serve a
        // freshly-started search after its live actions changed.
        if !self.books.available
            || self.live_epoch.load(Ordering::Acquire) != self.snapshot_epoch
            || work_budget == 0
            || result_limit == 0
        {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        let offset = match cursor {
            None => 0,
            Some(cursor) => {
                let (generation, cursor_query, state, ordinal) = cursor.opaque_parts();
                if generation != self.snapshot_epoch
                    || cursor_query != query
                    || state.is_some()
                    || self.live_epoch.load(Ordering::Acquire) != self.snapshot_epoch
                {
                    return SearchPage {
                        results: Vec::new(),
                        next: None,
                        inspected: 0,
                    };
                }
                ordinal.unwrap_or_default() as usize
            }
        };
        let (length, at) = if query.is_empty() {
            (self.rows.len(), None)
        } else {
            let rows = posting_candidates(&self.tokens, &self.grams, &query);
            (rows.len(), Some(rows))
        };
        if offset > length {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        let mut inspected = 0;
        let mut results = Vec::new();
        let mut index = offset;
        while index < length && inspected < work_budget && results.len() < result_limit {
            let row = at.map_or(index, |rows| rows[index] as usize);
            index += 1;
            inspected += 1;
            if let Some((text, result)) = self.rows.get(row)
                && normalized_contains(text, &query)
            {
                results.push(result.clone());
            }
        }
        let next = (index < length)
            .then(|| SearchCursor::opaque(self.snapshot_epoch, &query, None, Some(index as u64)));
        SearchPage {
            results,
            next,
            inspected,
        }
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        if self.live_epoch.load(Ordering::Acquire) != self.snapshot_epoch {
            return None;
        }
        self.live_resources
            .get(&address.resource)
            .filter(|resource| {
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == address.action)
            })
            .cloned()
            .or_else(|| self.books.resolve_action_resource(address))
    }
}

impl SearchProvider for BookSearchProvider {
    fn name(&self) -> &'static str {
        "Books"
    }

    fn action_directory_generation(&self) -> Option<u64> {
        self.available.then_some(self.generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.available && cursor.opaque_parts().0 == self.generation
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let first = self.search_page(query, None, DEFAULT_SEARCH_CANDIDATE_LIMIT, limit);
        // Metadata and body matching are separate bounded phases. A normal
        // one-shot search should still include an exact prose location when
        // the metadata phase completed within its budget, so consume one body
        // page without turning the call into an unbounded corpus walk.
        let mut results = first.results;
        let remaining = limit.saturating_sub(results.len());
        if remaining == 0 {
            return results;
        }
        let Some(cursor) = first
            .next
            .as_ref()
            .filter(|cursor| Self::body_start(cursor).is_some())
        else {
            return results;
        };
        results.extend(
            self.search_page(
                query,
                Some(cursor),
                DEFAULT_SEARCH_CANDIDATE_LIMIT,
                remaining,
            )
            .results,
        );
        results
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the two continuation phases share one bounded accounting loop"
    )]
    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let work_budget = work_budget.min(DEFAULT_SEARCH_CANDIDATE_LIMIT);
        let result_limit = result_limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        let Some(query) = normalize_query(query) else {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        };
        if !self.available || work_budget == 0 || result_limit == 0 {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        if cursor.is_some_and(|cursor| {
            !self.action_directory_cursor_is_current(cursor) || cursor.opaque_parts().1 != query
        }) {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        let (body_phase, mut offset) = self.page_start(&query, cursor);
        let mut results = Vec::new();
        let mut inspected = 0usize;
        if !body_phase {
            if cursor.is_some_and(|cursor| cursor.opaque_parts().2.is_some()) {
                return SearchPage {
                    results: Vec::new(),
                    next: None,
                    inspected: 0,
                };
            }
            if query.is_empty() {
                // Every row matches the empty query, so browsing them in
                // order spends each inspection on a result.
                if offset > self.entries.len() {
                    return SearchPage {
                        results: Vec::new(),
                        next: None,
                        inspected: 0,
                    };
                }
                while offset < self.entries.len()
                    && inspected < work_budget
                    && results.len() < result_limit
                {
                    results.push(self.entries[offset].result.clone());
                    offset += 1;
                    inspected += 1;
                }
                let next =
                    (offset < self.entries.len()).then(|| self.next_cursor(&query, false, offset));
                return SearchPage {
                    results,
                    next,
                    inspected,
                };
            }
            let candidates =
                posting_candidates(&self.metadata_tokens, &self.metadata_grams, &query);
            if offset > candidates.len() {
                return SearchPage {
                    results: Vec::new(),
                    next: None,
                    inspected: 0,
                };
            }
            while offset < candidates.len()
                && inspected < work_budget
                && results.len() < result_limit
            {
                let index = candidates[offset] as usize;
                offset += 1;
                inspected += 1;
                if let Some(entry) = self.entries.get(index)
                    && Self::metadata_matches(entry, &query)
                {
                    results.push(entry.result.clone());
                }
            }
            if offset < candidates.len() {
                return SearchPage {
                    results,
                    next: Some(self.next_cursor(&query, false, offset)),
                    inspected,
                };
            }
            // Keep metadata and prose phases disjoint, so every metadata hit
            // precedes every prose location. The body phase only follows
            // when some chapter contains every gram of the query.
            let next = (!self.body_candidates(&query).0.is_empty())
                .then(|| self.next_body_cursor(&query, 0, 0, 0, 0, 0));
            return SearchPage {
                results,
                next,
                inspected,
            };
        }
        let Some(cursor) = cursor else {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        };
        let Some((
            cursor_entry,
            mut block_index,
            mut span_index,
            mut byte_offset,
            mut matcher_state,
        )) = Self::body_start(cursor)
        else {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        };
        if query.is_empty() || cursor_entry > self.entries.len() {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        let (candidates, grams) = self.body_candidates(&query);
        // Resume at the first candidate chapter at or after the cursor's row.
        // A position inside a chapter is only meaningful for that candidate;
        // anything else is an untrusted cursor, not permission to skip prose.
        let mut position = candidates.partition_point(|&row| (row as usize) < cursor_entry);
        let resuming = (block_index, span_index, byte_offset, matcher_state) != (0, 0, 0, 0);
        if resuming && candidates.get(position).map(|&row| row as usize) != Some(cursor_entry) {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        // A resumed chapter already passed the gram filter on an earlier page.
        let mut filtered = resuming;
        while position < candidates.len() && inspected < work_budget && results.len() < result_limit
        {
            let row = candidates[position];
            inspected += 1;
            if !filtered {
                if !self.body.contains_all(&grams, row) {
                    position += 1;
                    continue;
                }
                filtered = true;
            }
            let Some(entry) = self.entries.get(row as usize) else {
                return SearchPage {
                    results: Vec::new(),
                    next: None,
                    inspected: 0,
                };
            };
            match self.body_chunk_matches(
                entry,
                &query,
                block_index,
                span_index,
                byte_offset,
                matcher_state,
            ) {
                BodyChunk::Invalid => {
                    return SearchPage {
                        results: Vec::new(),
                        next: None,
                        inspected: 0,
                    };
                }
                chunk @ (BodyChunk::Done | BodyChunk::Scanned { matched: true, .. }) => {
                    if matches!(chunk, BodyChunk::Scanned { .. })
                        && let Some(result) = self.body_result(entry, block_index)
                    {
                        results.push(result);
                    }
                    position += 1;
                    filtered = false;
                    block_index = 0;
                    span_index = 0;
                    byte_offset = 0;
                    matcher_state = 0;
                }
                BodyChunk::Scanned {
                    next_byte,
                    next_span,
                    matcher_state: next_matcher_state,
                    done: false,
                    ..
                } => {
                    span_index = next_span;
                    byte_offset = next_byte;
                    matcher_state = next_matcher_state;
                }
                BodyChunk::Scanned {
                    matcher_state: next_matcher_state,
                    done: true,
                    ..
                } => {
                    let Some((next_block, next_span)) =
                        self.next_body_position(entry, block_index, span_index)
                    else {
                        return SearchPage {
                            results: Vec::new(),
                            next: None,
                            inspected: 0,
                        };
                    };
                    if self.chapter_block_count(entry) == Some(next_block) {
                        // The last block is verified: the chapter is done
                        // without spending an inspection on its end.
                        position += 1;
                        filtered = false;
                        block_index = 0;
                        span_index = 0;
                        byte_offset = 0;
                        matcher_state = 0;
                    } else {
                        block_index = next_block;
                        span_index = next_span;
                        byte_offset = 0;
                        matcher_state = next_matcher_state;
                    }
                }
            }
        }
        let next = candidates.get(position).map(|&row| {
            self.next_body_cursor(
                &query,
                row as usize,
                block_index,
                span_index,
                byte_offset,
                matcher_state,
            )
        });
        SearchPage {
            results,
            next,
            inspected,
        }
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        if let Some(resource) = self.location_resource_for(address) {
            return Some(resource);
        }
        let row = *self.resource_rows.get(&address.resource)?;
        let entry = self.entries.get(row)?;
        let resource = self.resource_for(entry, address)?;
        resource
            .actions
            .iter()
            .any(|action| action.id == address.action)
            .then_some(resource)
    }
}

/// Distinct alphanumeric terms of already case-folded text. Every term is
/// kept: a capped list would leave a row out of the posting a later term's
/// query picks.
fn index_metadata_tokens(folded: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    folded
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .filter(|token| seen.insert(*token))
        .map(str::to_owned)
        .collect()
}

fn index_action_row(
    tokens: &mut HashMap<String, Vec<u32>>,
    grams: &mut GramPostings,
    row: u32,
    text: &str,
) {
    let folded: String = fold_chars(text).collect();
    index_row(tokens, grams, row, &folded);
}

/// Add one row's term and gram postings. Rows arrive in ascending order.
fn index_row(
    tokens: &mut HashMap<String, Vec<u32>>,
    grams: &mut GramPostings,
    row: u32,
    folded: &str,
) {
    for token in index_metadata_tokens(folded) {
        tokens.entry(token).or_default().push(row);
    }
    grams.add(row, folded.chars(), &mut GramWindow::default());
}

/// Candidate rows for a folded, non-empty query: the shorter of any whole
/// query term's posting and the rarest gram posting. A gram that occurs
/// nowhere means nothing can match, so no row is a candidate.
fn posting_candidates<'a>(
    tokens: &'a HashMap<String, Vec<u32>>,
    grams: &'a GramPostings,
    query: &str,
) -> &'a [u32] {
    let needle: Vec<char> = query.chars().collect();
    let by_gram = grams.rarest(&query_grams(&needle));
    if by_gram.is_empty() {
        return by_gram;
    }
    query
        .split_whitespace()
        .filter_map(|token| tokens.get(token))
        .map(Vec::as_slice)
        .chain(std::iter::once(by_gram))
        .min_by_key(|rows| rows.len())
        .unwrap_or(by_gram)
}

fn normalize_query(query: &str) -> Option<String> {
    let mut normalized = String::new();
    for (index, character) in query.chars().enumerate() {
        if index >= MAX_SEARCH_QUERY_SCALARS {
            return None;
        }
        normalized.extend(character.to_lowercase());
    }
    Some(normalized)
}

fn normalized_contains(text: &str, query: &str) -> bool {
    folded_contains(text.chars(), query)
}

/// Case-folded substring verification without materialising a lowercased copy
/// of the searched EPUB text.  The query is small and already bounded by the
/// host; KMP keeps verification linear in every consumed chunk.
fn folded_contains(chars: impl IntoIterator<Item = char>, query: &str) -> bool {
    let mut matcher = FoldMatcher::new(query);
    if matcher.needle.is_empty() {
        return true;
    }
    for character in chars {
        if matcher.push(character) {
            return true;
        }
    }
    false
}

struct FoldMatcher {
    needle: Vec<char>,
    failure: Vec<usize>,
    matched: usize,
}

impl FoldMatcher {
    fn new(query: &str) -> Self {
        let needle: Vec<char> = query.chars().collect();
        let mut failure = vec![0usize; needle.len()];
        for i in 1..needle.len() {
            let mut j = failure[i - 1];
            while j > 0 && needle[i] != needle[j] {
                j = failure[j - 1];
            }
            if needle[i] == needle[j] {
                j += 1;
            }
            failure[i] = j;
        }
        Self {
            needle,
            failure,
            matched: 0,
        }
    }

    fn with_state(query: &str, state: usize) -> Option<Self> {
        let mut stream = Self::new(query);
        (state < stream.needle.len()).then(|| {
            stream.matched = state;
            stream
        })
    }

    const fn state(&self) -> usize {
        self.matched
    }

    fn push(&mut self, character: char) -> bool {
        for folded in character.to_lowercase() {
            while self.matched > 0 && folded != self.needle[self.matched] {
                self.matched = self.failure[self.matched - 1];
            }
            if folded == self.needle[self.matched] {
                self.matched += 1;
                if self.matched == self.needle.len() {
                    return true;
                }
            }
        }
        false
    }
}

fn source_chunk(source: &str, byte_offset: usize) -> (usize, usize, bool) {
    let start = byte_offset;
    let mut end = start;
    for character in source[start..].chars().take(BODY_CHUNK_SCALARS) {
        end += character.len_utf8();
    }
    (start, end, end == source.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::testing::{CorpusRow, LATE_HIT_NEEDLE, assert_late_hit_reachable};

    fn sample_books() -> Vec<Book> {
        crate::content::load_sample_library()
    }

    #[test]
    fn canonical_results_and_exact_lazy_resources() {
        let books = sample_books();
        let provider = BookSearchProvider::new(&books, &[]);
        let hit = provider
            .search("")
            .into_iter()
            .find(|hit| hit.kind == CHAPTER_KIND)
            .expect("chapter");
        let address = hit.action_address().expect("canonical");
        let resource = provider
            .resolve_action_resource(&address)
            .expect("exact local resource");
        assert_eq!(resource.id.as_str(), address.resource);
        assert!(resource.children.is_empty());
        assert_eq!(resource.actions.len(), 1);
    }

    #[test]
    fn body_substrings_share_the_sole_book_snapshot() {
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
        assert_eq!(provider.stats().retained_chapter_scalars, 0);
        assert!(Arc::ptr_eq(&books, &provider.books));
    }

    #[test]
    fn full_text_hits_resolve_to_the_exact_canonical_block_location() {
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
                ..crate::model::Chapter::default()
            }],
            ..Book::default()
        }];
        let provider = BookSearchProvider::new(&books, &[]);
        let hit = provider
            .search("unique needle")
            .into_iter()
            .find(|hit| hit.kind == LOCATION_KIND)
            .expect("precise full-text location");
        let address = hit.action_address().expect("canonical location address");
        assert_eq!(address.resource, "ebook.location:stable-book:0:1");
        let resource = provider
            .resolve_action_resource(&address)
            .expect("exact lazy location resource");
        let open = resource
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("open action");
        let tilo_actions::ActionInput::Immediate(encoded) = &open.input else {
            panic!("location open is immediate");
        };
        assert_eq!(
            crate::actions::decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(EbookOperation::Reveal {
                book_id: "stable-book".into(),
                chapter: 0,
                page: 0,
                block: Some(1),
            })
        );
    }

    #[test]
    fn million_scalar_book_retains_no_prose_and_few_postings() {
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
                ..crate::model::Chapter::default()
            }],
            ..Book::default()
        })]);
        let provider = BookSearchProvider::new_shared(
            Arc::clone(&books),
            Arc::new(Vec::new()),
            Arc::new(Vec::new()),
            3,
        );
        let mut cursor = Some(SearchCursor::opaque(
            3,
            "tail-needle",
            Some("body:0:0:0:0:0".into()),
            Some(BODY_PHASE),
        ));
        let mut found = false;
        while let Some(next_cursor) = cursor {
            let page = provider.search_page("tail-needle", Some(&next_cursor), 32, 8);
            found |= page.results.iter().any(|hit| hit.kind == LOCATION_KIND);
            cursor = page.next;
        }
        assert!(found);
        let stats = provider.stats();
        assert_eq!(stats.retained_chapter_scalars, 0);
        assert!(stats.metadata_postings < 20);
        // Postings grow with the distinct grams of the prose, not its length.
        assert!(stats.body_postings < 64, "{stats:?}");
        assert_eq!(Arc::strong_count(&books), 2);
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
                            ..crate::model::Chapter::default()
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

    #[test]
    fn late_metadata_hit_is_found_within_the_search_bound() {
        let (provider, hit) =
            assert_late_hit_reachable(|corpus| late_hit_library(corpus, false), 3_000);
        assert_eq!(hit.kind, BOOK_KIND);
        assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
        let address = hit.action_address().expect("canonical address");
        assert_eq!(address.resource, "ebook.book:book:002999");
        assert!(provider.resolve_action_resource(&address).is_some());
    }

    #[test]
    fn late_body_only_hit_is_found_within_the_search_bound() {
        let (provider, hit) =
            assert_late_hit_reachable(|corpus| late_hit_library(corpus, true), 3_000);
        assert_eq!(hit.kind, LOCATION_KIND);
        let address = hit.action_address().expect("canonical address");
        assert_eq!(address.resource, "ebook.location:book:002999:0:0");
        assert!(provider.resolve_action_resource(&address).is_some());
    }

    #[test]
    fn late_metadata_posting_avoids_a_library_scan() {
        let books = (0..3_000)
            .map(|i| Book {
                id: format!("book:{i:06}"),
                title: format!("Library volume {i}"),
                ..Book::default()
            })
            .collect::<Vec<_>>();
        let provider = BookSearchProvider::new(&books, &[]);
        let page = provider.search_page("volume 2999", None, 4, 4);
        assert_eq!(page.inspected, 1);
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].title, "Library volume 2999");
    }

    #[test]
    fn body_pages_are_clamped_and_complete() {
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
                    ..crate::model::Chapter::default()
                }],
                ..Book::default()
            })
            .collect::<Vec<_>>();
        let provider = BookSearchProvider::new(&books, &[]);
        let mut cursor = None;
        let mut seen = 0;
        loop {
            let page = provider.search_page("common", cursor.as_ref(), 17, 5);
            assert!(page.inspected <= 17);
            assert!(page.results.len() <= 5);
            seen += page.results.len();
            let Some(next) = page.next else { break };
            cursor = Some(next);
        }
        assert_eq!(seen, 257);
    }

    #[test]
    fn oversized_single_block_resumes_without_a_prose_copy() {
        let books = vec![Book {
            id: "huge".into(),
            title: "Huge".into(),
            chapters: vec![crate::model::Chapter {
                title: "One block".into(),
                blocks: vec![crate::model::Block::Paragraph {
                    spans: vec![crate::model::Span::plain(format!(
                        "{}needle-at-the-end",
                        "x".repeat(BODY_CHUNK_SCALARS * 4)
                    ))],
                }],
                ..crate::model::Chapter::default()
            }],
            ..Book::default()
        }];
        let provider = BookSearchProvider::new(&books, &[]);
        let mut cursor = Some(SearchCursor::opaque(
            1,
            "needle-at-the-end",
            Some("body:0:0:0:0:0".into()),
            Some(BODY_PHASE),
        ));
        let mut pages = 0;
        loop {
            let page = provider.search_page("needle-at-the-end", cursor.as_ref(), 1, 1);
            assert_eq!(page.inspected, 1);
            pages += 1;
            if !page.results.is_empty() {
                break;
            }
            cursor = page.next;
            assert!(cursor.is_some(), "continuation before tail match");
        }
        assert!(pages >= 5, "the block was verified in bounded chunks");
        assert_eq!(provider.stats().retained_chapter_scalars, 0);
    }

    #[test]
    fn million_scalar_multi_span_paragraph_resumes_inside_the_first_span() {
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
                ..crate::model::Chapter::default()
            }],
            ..Book::default()
        }];
        let provider = BookSearchProvider::new(&books, &[]);
        let mut cursor = Some(SearchCursor::opaque(
            1,
            "tail-needle",
            Some("body:1:0:0:0:0".into()),
            Some(BODY_PHASE),
        ));
        let mut pages = 0;
        let mut found = false;
        while let Some(next) = cursor {
            let page = provider.search_page("tail-needle", Some(&next), 1, 1);
            assert_eq!(page.inspected, 1);
            pages += 1;
            found |= page
                .results
                .iter()
                .any(|result| result.kind == LOCATION_KIND);
            cursor = page.next;
        }
        assert!(found);
        assert!(pages >= 978, "the first span was not resumed in chunks");
        assert_eq!(provider.stats().retained_chapter_scalars, 0);
    }

    #[test]
    fn body_phrase_can_cross_inline_and_block_boundaries() {
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
                ..crate::model::Chapter::default()
            }],
            ..Book::default()
        }];
        let provider = BookSearchProvider::new(&books, &[]);
        let mut cursor = Some(SearchCursor::opaque(
            1,
            "beta gamma",
            Some("body:1:0:0:0:0".into()),
            Some(BODY_PHASE),
        ));
        let mut found = false;
        while let Some(next) = cursor {
            let page = provider.search_page("beta gamma", Some(&next), 1, 1);
            found |= page
                .results
                .iter()
                .any(|result| result.kind == LOCATION_KIND);
            cursor = page.next;
        }
        assert!(
            found,
            "streaming state must survive span and block boundaries"
        );
    }

    #[test]
    fn prose_no_chapter_contains_ends_without_inspecting_the_library() {
        let books = (0..1_000)
            .map(|index| Book {
                id: format!("book-{index}"),
                title: "Shelf".into(),
                chapters: vec![crate::model::Chapter {
                    title: "Opening".into(),
                    blocks: vec![crate::model::Block::Paragraph {
                        spans: vec![crate::model::Span::plain("present words only")],
                    }],
                    ..crate::model::Chapter::default()
                }],
                ..Book::default()
            })
            .collect::<Vec<_>>();
        let provider = BookSearchProvider::new(&books, &[]);
        let first = provider.search_page("absent", None, 3, 3);
        assert!(first.results.is_empty());
        assert_eq!(first.inspected, 0);
        assert!(first.next.is_none(), "no chapter holds the query's grams");
        let cursor =
            SearchCursor::opaque(1, "absent", Some("body:0:0:0:0:0".into()), Some(BODY_PHASE));
        let page = provider.search_page("absent", Some(&cursor), 3, 3);
        assert!(page.results.is_empty());
        assert_eq!(page.inspected, 0);
        assert!(page.next.is_none());
    }

    #[test]
    fn gram_candidates_that_do_not_contain_the_phrase_cost_one_inspection() {
        // Every chapter holds each gram of "dog cat" but not the phrase, so
        // all are candidates and the gram filter cannot reject them; each
        // still costs exactly one inspection before the page yields.
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
            let page = provider.search_page("dog cat", cursor.as_ref(), 7, 7);
            assert!(page.results.is_empty());
            assert!(page.inspected <= 7);
            inspected += page.inspected;
            let Some(next) = page.next else { break };
            cursor = Some(next);
        }
        assert_eq!(inspected, 40);
    }

    #[test]
    fn rebuilds_over_the_same_books_reuse_the_prose_index() {
        let books: Arc<Vec<Arc<Book>>> =
            Arc::new(sample_books().into_iter().map(Arc::new).collect());
        let first = BookSearchProvider::new_shared(
            Arc::clone(&books),
            Arc::new(Vec::new()),
            Arc::new(Vec::new()),
            1,
        );
        assert!(first.stats().body_postings > 0);
        let mut annotation = Annotation::new(&books[0].id, AnnoKind::Note, 0, 0);
        annotation.note = "a fresh note".into();
        let annotated = BookSearchProvider::new_shared_after(
            &first,
            Arc::clone(&books),
            Arc::new(vec![annotation]),
            Arc::new(Vec::new()),
            2,
        );
        assert!(Arc::ptr_eq(&first.body, &annotated.body));
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
        assert!(!Arc::ptr_eq(&annotated.body, &rebuilt.body));
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

    #[test]
    fn membership_snapshots_keep_existing_book_allocations() {
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

    #[test]
    fn malformed_or_stale_continuations_fail_closed() {
        let provider = BookSearchProvider::new(&sample_books(), &[]);
        for cursor in [
            SearchCursor::opaque(0, "reader", Some("body:0:0:0:0:0".into()), Some(BODY_PHASE)),
            SearchCursor::opaque(
                1,
                "different",
                Some("body:0:0:0:0:0".into()),
                Some(BODY_PHASE),
            ),
            SearchCursor::opaque(
                1,
                "reader",
                Some("body:not-a-number".into()),
                Some(BODY_PHASE),
            ),
        ] {
            let page = provider.search_page("reader", Some(&cursor), 8, 8);
            assert!(page.results.is_empty());
            assert!(page.next.is_none());
            assert_eq!(page.inspected, 0);
        }
    }

    #[test]
    fn hostile_non_utf8_body_cursor_fails_closed() {
        let books = vec![Book {
            id: "unicode".into(),
            chapters: vec![crate::model::Chapter {
                blocks: vec![crate::model::Block::Math {
                    expr: ui::math::MathExpr::new(),
                    source: "é needle".into(),
                    display: false,
                }],
                ..crate::model::Chapter::default()
            }],
            ..Book::default()
        }];
        let provider = BookSearchProvider::new(&books, &[]);
        let cursor =
            SearchCursor::opaque(1, "needle", Some("body:1:0:0:1:0".into()), Some(BODY_PHASE));
        let page = provider.search_page("needle", Some(&cursor), 1, 1);
        assert!(page.results.is_empty());
        assert!(page.next.is_none());
        assert_eq!(page.inspected, 0);
    }

    #[test]
    fn hostile_out_of_range_body_positions_fail_closed() {
        let books = vec![Book {
            id: "positions".into(),
            chapters: vec![crate::model::Chapter {
                blocks: vec![crate::model::Block::Paragraph {
                    spans: vec![crate::model::Span::plain("one")],
                }],
                ..crate::model::Chapter::default()
            }],
            ..Book::default()
        }];
        let provider = BookSearchProvider::new(&books, &[]);
        for state in ["body:1:99:0:0:0", "body:1:0:99:0:0", "body:1:0:0:0:99"] {
            let cursor = SearchCursor::opaque(1, "one", Some(state.into()), Some(BODY_PHASE));
            let page = provider.search_page("one", Some(&cursor), 1, 1);
            assert!(page.results.is_empty(), "{state}");
            assert!(page.next.is_none(), "{state}");
            assert_eq!(page.inspected, 0, "{state}");
        }
    }

    #[test]
    fn exact_annotation_resource_exports_the_same_delete_action_as_the_reader() {
        let mut note = Annotation::new("book", AnnoKind::Note, 0, 0).with_note("keep me");
        note.id = uuid::Uuid::from_u128(42);
        let books = vec![Book {
            id: "book".into(),
            ..Book::default()
        }];
        let provider = BookSearchProvider::new(&books, &[note.clone()]);
        let hit = provider.search("keep").pop().expect("annotation row");
        let resource = provider
            .resolve_action_resource(&hit.action_address().expect("address"))
            .expect("exact annotation resource");
        let delete = resource
            .actions
            .iter()
            .find(|action| action.id == "delete")
            .expect("canonical delete action");
        let tilo_actions::ActionInput::Immediate(encoded) = &delete.input else {
            panic!("delete is immediate");
        };
        assert_eq!(
            encoded.payload,
            encode(&delete_annotation_action(note.id)).payload,
            "the exact search resource and reader action have one canonical payload"
        );
    }

    #[test]
    fn exact_resources_preserve_book_and_annotation_action_contracts() {
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
            .resolve_action_resource(
                &provider
                    .search("contract book")
                    .into_iter()
                    .find(|result| result.kind == BOOK_KIND)
                    .and_then(|result| result.action_address())
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
            ExecutionMode::Background { .. }
        ));
        assert_eq!(share.contract.retry, RetryPolicy::Idempotent);
        assert_eq!(share.contract.result, ResultSpec::Job);
        assert_eq!(share.contract.tags, ["workspace", "materialize"]);
        let remove = book_resource
            .actions
            .iter()
            .find(|action| action.id == "remove-from-library")
            .expect("remove");
        assert_eq!(remove.contract.risk, RiskLevel::Destructive);
        assert_eq!(remove.contract.confirmation, ConfirmationPolicy::Confirm);
        assert!(remove.contract.reversible);
        assert_eq!(remove.contract.tags, ["library", "tombstone"]);

        let highlight_resource = provider
            .resolve_action_resource(
                &provider
                    .search("amber")
                    .pop()
                    .and_then(|result| result.action_address())
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
            (
                "peer comment",
                ["delete-comment", "edit-comment", "open"].as_slice(),
            ),
            ("reader note", ["delete", "edit", "open"].as_slice()),
        ] {
            let resource = provider
                .resolve_action_resource(
                    &provider
                        .search(query)
                        .pop()
                        .and_then(|result| result.action_address())
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

    #[test]
    fn action_labels_are_compactly_indexed_and_resolve_exact_resources() {
        let book = Book {
            id: "book".into(),
            title: "Action book".into(),
            ..Book::default()
        };
        let group = uuid::Uuid::from_u128(8);
        let highlight = Annotation::new("book", AnnoKind::Highlight, 0, 0)
            .with_group(group)
            .with_excerpt("marked passage");
        let comment = Annotation::new("book", AnnoKind::Note, 0, 0)
            .with_group(group)
            .with_note("comment text");
        let provider = BookSearchProvider::new(&[book], &[highlight, comment]);
        let actions = provider.action_search_provider();
        for query in [
            "Change colour",
            "Add comment",
            "Delete highlight",
            "Delete comment",
        ] {
            let hit = actions.search(query).into_iter().next().expect(query);
            let address = hit.action_address().expect("canonical action address");
            let resource = actions
                .resolve_action_resource(&address)
                .expect("exact resource");
            assert!(
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == address.action)
            );
        }
    }

    #[test]
    fn annotation_publication_invalidates_existing_action_sessions() {
        let book = Book {
            id: "book".into(),
            title: "Live book".into(),
            ..Book::default()
        };
        let highlight =
            Annotation::new("book", AnnoKind::Highlight, 0, 0).with_group(uuid::Uuid::from_u128(9));
        let provider = BookSearchProvider::new(&[book], &[highlight]);
        let address = ActionAddress::new(
            "ebook.highlight:00000000-0000-0000-0000-000000000009",
            "delete-highlight",
        );
        let resource = provider
            .resolve_action_resource(&address)
            .expect("book resource");
        let epoch = Arc::new(AtomicU64::new(9));
        let actions = provider.action_search_provider().with_live_resources(
            vec![resource],
            Arc::clone(&epoch),
            9,
        );
        let opens = actions
            .search("delete highlight")
            .into_iter()
            .filter(|result| result.action_address().as_ref() == Some(&address))
            .count();
        assert_eq!(
            opens, 1,
            "a live exact resource must not duplicate indexed actions"
        );
        epoch.store(10, Ordering::Release);
        assert!(
            !actions.action_directory_cursor_is_current(&SearchCursor::opaque(
                9,
                "open",
                None,
                Some(0)
            ))
        );
        assert!(
            actions.search("delete highlight").is_empty(),
            "a stale provider must reject a fresh search as well as continuations"
        );
        assert!(actions.resolve_action_resource(&address).is_none());
    }

    #[test]
    fn remote_metadata_hydrates_and_opens_the_canonical_workspace_book() {
        let entry = LibraryEntry {
            id: "book:remote".into(),
            title: "Remote Atlas".into(),
            author: "Mira Peer".into(),
            subject: "Maps".into(),
            shared: true,
            added: 1,
        };
        let provider = BookSearchProvider::new_with_registry(&[], &[], &[entry]);
        let hit = provider.search("mira").pop().expect("metadata");
        let resource = provider
            .resolve_action_resource(&hit.action_address().expect("address"))
            .expect("actionable remote resource");
        let open = resource
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("open");
        assert_eq!(open.label, "Download and open");
        assert!(matches!(
            open.contract.execution,
            ExecutionMode::Background { .. }
        ));
        let tilo_actions::ActionInput::Immediate(operation) = &open.input else {
            panic!("remote open is a lifecycle operation");
        };
        assert_eq!(
            crate::actions::decode_operation(&operation.type_id, &operation.payload).unwrap(),
            Some(EbookOperation::HydrateSharedBook {
                book_id: "book:remote".into(),
            })
        );
        let remove = resource
            .actions
            .iter()
            .find(|action| action.id == "remove-from-library")
            .expect("remote remove");
        assert_eq!(remove.contract.risk, RiskLevel::Destructive);
        assert_eq!(remove.contract.confirmation, ConfirmationPolicy::Confirm);
        assert_eq!(remove.contract.retry, RetryPolicy::Idempotent);
        assert!(remove.contract.reversible);
    }

    #[test]
    fn duplicate_rows_fail_closed() {
        let book = Book {
            id: "duplicate".into(),
            ..Book::default()
        };
        let provider = BookSearchProvider::new(&[book.clone(), book], &[]);
        assert_eq!(provider.action_directory_generation(), None);
        assert!(provider.search("duplicate").is_empty());
    }
}
