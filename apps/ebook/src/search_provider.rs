//! Bounded, canonical global search for Books.
//!
//! The reader is the sole owner of EPUB prose. This provider retains stable
//! locators, labels, and a small metadata-token directory only; it verifies
//! full text directly against the shared book snapshot a bounded page at a
//! time. Large EPUBs therefore never become a second corpus of n-grams.

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

const MAX_METADATA_TOKENS_PER_ROW: usize = 32;
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
    pub retained_chapter_scalars: usize,
    pub generation: u64,
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
}

/// Outcome of one bounded body-verification unit.
enum BodyChunk {
    Scanned {
        matched: bool,
        next_byte: usize,
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
    book_metadata: Arc<HashMap<String, (String, String)>>,
    entries: Arc<Vec<SearchEntry>>,
    metadata_tokens: Arc<HashMap<String, Vec<usize>>>,
    resource_rows: Arc<BTreeMap<String, usize>>,
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
    #[allow(
        clippy::too_many_lines,
        reason = "the staged canonical row builder is deliberately together"
    )]
    pub(crate) fn new_shared(
        books: Arc<Vec<Arc<Book>>>,
        annotations: Arc<Vec<Annotation>>,
        registry: Arc<Vec<LibraryEntry>>,
        generation: u64,
    ) -> Self {
        let mut entries = Vec::new();
        let mut resource_rows = BTreeMap::new();
        let mut tokens = HashMap::<String, Vec<usize>>::new();
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

        let mut add = |entry: SearchEntry, metadata: String| -> bool {
            let Some(address) = entry.result.action_address() else {
                return false;
            };
            let row = entries.len();
            if resource_rows.insert(address.resource, row).is_some() {
                return false;
            }
            for token in index_metadata_tokens(&metadata) {
                tokens.entry(token).or_default().push(row);
            }
            entries.push(entry);
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
                SearchEntry {
                    result,
                    target: SearchTarget::Book(book_index),
                },
                format!("{title} {}", book.author),
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
                    SearchEntry {
                        result,
                        target: SearchTarget::Chapter {
                            book: book_index,
                            chapter: chapter_index,
                        },
                    },
                    format!("{title} {} {chapter_title}", book.author),
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
                SearchEntry {
                    result,
                    target: SearchTarget::RemoteBook(registry_index),
                },
                format!("{title} {} {}", entry.author, entry.subject),
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
                SearchEntry {
                    result,
                    target: SearchTarget::Annotation {
                        index: annotation_index,
                        hydrated: hydrated.contains(annotation.doc_id.as_str()),
                        comment,
                    },
                },
                format!(
                    "{title} {author} {} {} {} {}",
                    annotation.kind.label(),
                    comment.then_some("comment").unwrap_or(""),
                    annotation.excerpt,
                    annotation.note
                ),
            ) {
                return Self::unavailable(books, annotations, registry, generation);
            }
        }
        Self {
            books,
            annotations,
            registry,
            book_metadata: Arc::new(book_metadata),
            entries: Arc::new(entries),
            metadata_tokens: Arc::new(tokens),
            resource_rows: Arc::new(resource_rows),
            generation,
            available: generation != 0,
        }
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
            book_metadata: Arc::new(HashMap::new()),
            entries: Arc::new(Vec::new()),
            metadata_tokens: Arc::new(HashMap::new()),
            resource_rows: Arc::new(BTreeMap::new()),
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

    fn metadata_matches(&self, entry: &SearchEntry, query: &str) -> bool {
        let text = match entry.target {
            SearchTarget::Book(index) => self
                .books
                .get(index)
                .map(|book| format!("{} {}", Self::book_title(book), book.author)),
            SearchTarget::Chapter { book, chapter } => self.books.get(book).and_then(|book| {
                book.chapters.get(chapter).map(|item| {
                    format!(
                        "{} {} {}",
                        Self::book_title(book),
                        book.author,
                        chapter_label(item, chapter)
                    )
                })
            }),
            SearchTarget::Annotation { index, .. } => {
                self.annotations.get(index).map(|annotation| {
                    let (title, author) = self
                        .book_metadata
                        .get(&annotation.doc_id)
                        .cloned()
                        .unwrap_or_else(|| ("Unknown book".into(), String::new()));
                    format!(
                        "{title} {author} {} {} {}",
                        annotation.kind.label(),
                        annotation.excerpt,
                        annotation.note
                    )
                })
            }
            SearchTarget::RemoteBook(index) => self
                .registry
                .get(index)
                .map(|entry| format!("{} {} {}", entry.title, entry.author, entry.subject)),
        };
        text.is_some_and(|text| normalized_contains(&text, query))
    }

    fn body_chunk_matches(
        &self,
        entry: &SearchEntry,
        query: &str,
        block_index: usize,
        byte_offset: usize,
    ) -> BodyChunk {
        let SearchTarget::Chapter { book, chapter } = entry.target else {
            return BodyChunk::Done;
        };
        let Some(block) = self
            .books
            .get(book)
            .and_then(|book| book.chapters.get(chapter))
            .and_then(|chapter| chapter.blocks.get(block_index))
        else {
            return BodyChunk::Done;
        };
        match block {
            crate::model::Block::Math { source, .. } => {
                Self::source_body_chunk(source, query, byte_offset)
            }
            _ if block.spans().len() == 1 => {
                Self::source_body_chunk(&block.spans()[0].text, query, byte_offset)
            }
            _ => BodyChunk::Scanned {
                matched: block_contains(block, query),
                next_byte: 0,
                done: true,
            },
        }
    }

    fn source_body_chunk(source: &str, query: &str, byte_offset: usize) -> BodyChunk {
        if byte_offset > source.len() || !source.is_char_boundary(byte_offset) {
            return BodyChunk::Invalid;
        }
        let (start, end, done) = source_chunk(source, byte_offset);
        // Recheck a bounded overlap so a match crossing a yield point is
        // never lost. Query input itself is capped at 1,024 scalars.
        let overlap = rewind_scalars(source, start, query.chars().count());
        BodyChunk::Scanned {
            matched: normalized_contains(&source[overlap..end], query),
            next_byte: end,
            done,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "all exact action variants are kept together so their canonical payloads can be audited"
    )]
    fn resource_for(&self, entry: &SearchEntry, address: &ActionAddress) -> Option<Resource> {
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

    fn body_start(cursor: &SearchCursor) -> Option<(usize, usize, usize)> {
        let state = cursor.opaque_parts().2?;
        let mut parts = state.strip_prefix("body:")?.split(':');
        let (Some(entry), Some(block), Some(byte)) = (parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        if parts.next().is_some() {
            return None;
        }
        match (entry.parse(), block.parse(), byte.parse()) {
            (Ok(entry), Ok(block), Ok(byte)) => Some((entry, block, byte)),
            _ => None,
        }
    }

    fn next_body_cursor(
        &self,
        query: &str,
        entry: usize,
        block: usize,
        byte: usize,
    ) -> SearchCursor {
        SearchCursor::opaque(
            self.generation,
            query,
            Some(format!("body:{entry}:{block}:{byte}")),
            Some(BODY_PHASE),
        )
    }

    fn metadata_candidates(&self, query: &str) -> Option<&Vec<usize>> {
        query
            .split_whitespace()
            .filter_map(|token| self.metadata_tokens.get(token))
            .min_by_key(|ids| ids.len())
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
        self.search_page(query, None, DEFAULT_SEARCH_CANDIDATE_LIMIT, limit)
            .results
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
        let metadata = self.metadata_candidates(&query);
        if !body_phase {
            if cursor.is_some_and(|cursor| cursor.opaque_parts().2.is_some()) {
                return SearchPage {
                    results: Vec::new(),
                    next: None,
                    inspected: 0,
                };
            }
            if let Some(candidates) = metadata {
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
                    let index = candidates[offset];
                    offset += 1;
                    inspected += 1;
                    let Some(entry) = self.entries.get(index) else {
                        continue;
                    };
                    if self.metadata_matches(entry, &query) {
                        results.push(entry.result.clone());
                    }
                }
                if offset < candidates.len()
                    && (inspected == work_budget || results.len() == result_limit)
                {
                    return SearchPage {
                        results,
                        next: Some(self.next_cursor(&query, false, offset)),
                        inspected,
                    };
                }
                // Keep metadata and prose phases disjoint. The continuation
                // lets an exact late metadata hit return in O(1) inspection
                // instead of spending the caller's remaining budget walking
                // unrelated body rows.
                return SearchPage {
                    results,
                    next: Some(self.next_body_cursor(&query, 0, 0, 0)),
                    inspected,
                };
            } else {
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
                    let index = offset;
                    offset += 1;
                    inspected += 1;
                    let entry = &self.entries[index];
                    if self.metadata_matches(entry, &query) {
                        results.push(entry.result.clone());
                    }
                }
                let next = (offset < self.entries.len())
                    .then(|| self.next_cursor(&query, false, offset))
                    .or_else(|| Some(self.next_body_cursor(&query, 0, 0, 0)));
                return SearchPage {
                    results,
                    next,
                    inspected,
                };
            }
        }
        let Some(cursor) = cursor else {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        };
        let Some((mut entry_index, mut block_index, mut byte_offset)) = Self::body_start(cursor)
        else {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        };
        if entry_index > self.entries.len() {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        while entry_index < self.entries.len()
            && inspected < work_budget
            && results.len() < result_limit
        {
            let entry = &self.entries[entry_index];
            if !matches!(entry.target, SearchTarget::Chapter { .. })
                || self.metadata_matches(entry, &query)
            {
                // Skipped rows are still work: otherwise a catalogue of book
                // rows could hide an unbounded walk behind `inspected == 0`.
                inspected += 1;
                entry_index += 1;
                block_index = 0;
                byte_offset = 0;
                continue;
            }
            inspected += 1;
            match self.body_chunk_matches(entry, &query, block_index, byte_offset) {
                BodyChunk::Invalid => {
                    return SearchPage {
                        results: Vec::new(),
                        next: None,
                        inspected: 0,
                    };
                }
                BodyChunk::Done => {
                    entry_index += 1;
                    block_index = 0;
                    byte_offset = 0;
                }
                BodyChunk::Scanned { matched: true, .. } => {
                    results.push(entry.result.clone());
                    entry_index += 1;
                    block_index = 0;
                    byte_offset = 0;
                }
                BodyChunk::Scanned {
                    next_byte,
                    done: false,
                    ..
                } => byte_offset = next_byte,
                BodyChunk::Scanned { done: true, .. } => {
                    block_index += 1;
                    byte_offset = 0;
                }
            }
        }
        let next = (entry_index < self.entries.len())
            .then(|| self.next_body_cursor(&query, entry_index, block_index, byte_offset));
        SearchPage {
            results,
            next,
            inspected,
        }
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        let row = *self.resource_rows.get(&address.resource)?;
        let entry = self.entries.get(row)?;
        (entry.result.action_address().as_ref() == Some(address))
            .then(|| self.resource_for(entry, address))
            .flatten()
    }
}

fn index_metadata_tokens(text: &str) -> Vec<String> {
    let lowered = text.to_lowercase();
    let mut seen = HashSet::new();
    lowered
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .filter(|token| seen.insert((*token).to_string()))
        .take(MAX_METADATA_TOKENS_PER_ROW)
        .map(str::to_owned)
        .collect()
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

fn block_contains(block: &crate::model::Block, query: &str) -> bool {
    match block {
        crate::model::Block::Math { source, .. } => normalized_contains(source, query),
        crate::model::Block::Separator => false,
        _ => {
            let mut matcher = FoldMatcher::new(query);
            if matcher.needle.is_empty() {
                return true;
            }
            for (index, span) in block.spans().iter().enumerate() {
                if index > 0 && span.space_before && matcher.push(' ') {
                    return true;
                }
                for character in span.text.chars() {
                    if matcher.push(character) {
                        return true;
                    }
                }
            }
            false
        }
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

fn rewind_scalars(source: &str, mut byte: usize, scalars: usize) -> usize {
    for _ in 0..scalars {
        if byte == 0 {
            break;
        }
        byte = source[..byte]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index);
    }
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

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
                .any(|hit| hit.kind == CHAPTER_KIND)
        );
        assert_eq!(provider.stats().retained_chapter_scalars, 0);
        assert!(Arc::ptr_eq(&books, &provider.books));
    }

    #[test]
    fn million_scalar_book_has_no_retained_prose_or_grams() {
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
            Some("body:0:0:0".into()),
            Some(BODY_PHASE),
        ));
        let mut found = false;
        while let Some(next_cursor) = cursor {
            let page = provider.search_page("tail-needle", Some(&next_cursor), 32, 8);
            found |= page.results.iter().any(|hit| hit.kind == CHAPTER_KIND);
            cursor = page.next;
        }
        assert!(found);
        let stats = provider.stats();
        assert_eq!(stats.retained_chapter_scalars, 0);
        assert!(stats.metadata_postings < 20);
        assert_eq!(Arc::strong_count(&books), 2);
    }

    #[test]
    fn late_metadata_posting_avoids_a_hundred_thousand_row_scan() {
        let books = (0..100_001)
            .map(|i| Book {
                id: format!("book:{i:06}"),
                title: format!("Library volume {i}"),
                ..Book::default()
            })
            .collect::<Vec<_>>();
        let provider = BookSearchProvider::new(&books, &[]);
        let page = provider.search_page("volume 100000", None, 4, 4);
        assert_eq!(page.inspected, 1);
        assert_eq!(page.results[0].title, "Library volume 100000");
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
            Some("body:0:0:0".into()),
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
    fn skipped_body_rows_consume_the_page_budget() {
        let books = (0..10)
            .map(|index| Book {
                id: format!("book-{index}"),
                title: "No chapters".into(),
                ..Book::default()
            })
            .collect::<Vec<_>>();
        let provider = BookSearchProvider::new(&books, &[]);
        let cursor = SearchCursor::opaque(1, "absent", Some("body:0:0:0".into()), Some(BODY_PHASE));
        let page = provider.search_page("absent", Some(&cursor), 3, 3);
        assert!(page.results.is_empty());
        assert_eq!(page.inspected, 3);
        assert!(page.next.is_some());
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
            SearchCursor::opaque(0, "reader", Some("body:0:0:0".into()), Some(BODY_PHASE)),
            SearchCursor::opaque(1, "different", Some("body:0:0:0".into()), Some(BODY_PHASE)),
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
        let cursor = SearchCursor::opaque(1, "needle", Some("body:1:0:1".into()), Some(BODY_PHASE));
        let page = provider.search_page("needle", Some(&cursor), 1, 1);
        assert!(page.results.is_empty());
        assert!(page.next.is_none());
        assert_eq!(page.inspected, 0);
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
    fn remote_metadata_is_not_a_false_local_reveal() {
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
        assert!(
            provider
                .resolve_action_resource(&hit.action_address().expect("address"))
                .is_none()
        );
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
