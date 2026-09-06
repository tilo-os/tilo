//! Library-wide Books search backed by canonical resource view actions.
//!
//! Every hit names an action exported by [`crate::actions`]. The shared search
//! host resolves that stable address against the fresh resource tree before it
//! navigates, so stale rows never fall through to private activation code.

use std::collections::{HashMap, HashSet};

use qos_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult};

use crate::model::{AnnoKind, Annotation, Book};

pub const BOOK_KIND: &str = "Books";
pub const CHAPTER_KIND: &str = "Chapters";
pub const ANNOTATION_KIND: &str = "Annotations";
/// A query never probes more than this many full entry strings.  The index is
/// complete, but a pathological query such as "the" must not turn Ctrl+K into
/// an unbounded main-thread scan on a large shared library.
const MAX_QUERY_CANDIDATES: usize = 2_048;

pub struct BookSearchProvider {
    entries: Vec<SearchEntry>,
    /// One-, two-, and three-character postings.  Indexing all three sizes
    /// means even a one-letter query is narrowed by the index rather than
    /// falling back to a linear scan. `GramKey` packs Unicode scalar values
    /// into a `u64`, avoiding one heap allocation per character window. Each
    /// entry contributes at most three postings per input character, so the
    /// index remains linear in library text size.
    gram_index: HashMap<GramKey, Vec<usize>>,
}

struct SearchEntry {
    haystack: String,
    result: SearchResult,
}

/// A compact one-, two-, or three-scalar search gram. Three Unicode scalar
/// values need 63 bits (each scalar fits in 21), leaving no need for a hash or
/// string allocation. Missing trailing scalars use the first value above the
/// Unicode range, so one-, two-, and three-scalar keys cannot alias even when
/// real text contains NUL. That matters because candidate verification is
/// deliberately bounded: a key collision must not let unrelated early entries
/// hide a valid late match.
type GramKey = u64;
const GRAM_END: u64 = 0x11_0000;

impl BookSearchProvider {
    pub fn new(books: &[Book], annotations: &[Annotation]) -> Self {
        // Search is a library feature, not a viewport feature: every loaded
        // book, chapter, full chapter body, and live annotation participates.
        // In particular, never use the UI result/index limits while building
        // this durable snapshot; doing so made old books undiscoverable.
        let book_ids: HashSet<&str> = books.iter().map(|book| book.id.as_str()).collect();
        let live_annotations = annotations
            .iter()
            .filter(|annotation| book_ids.contains(annotation.doc_id.as_str()))
            .collect::<Vec<_>>();
        let highlight_groups: HashSet<_> = live_annotations
            .iter()
            .filter(|annotation| annotation.kind == AnnoKind::Highlight)
            .map(|annotation| (annotation.doc_id.clone(), annotation.group))
            .collect();
        let mut entries = Vec::new();
        for book in books {
            let title = Self::book_title(book);
            let mut result =
                SearchResult::for_action(BOOK_KIND, title, Self::book_address(&book.id))
                    .with_icon(Icon::FileText);
            if !book.author.trim().is_empty() {
                result = result.with_subtitle(book.author.clone());
            }
            entries.push(SearchEntry {
                haystack: format!("{} {}", title, book.author).to_lowercase(),
                result,
            });
            for (chapter, item) in book.chapters.iter().enumerate() {
                let chapter_title = if item.title.trim().is_empty() {
                    format!("Chapter {}", chapter + 1)
                } else {
                    item.title.clone()
                };
                let mut chapter_text = String::new();
                for block in &item.blocks {
                    chapter_text.push_str(&crate::model::Block::plain_text(block));
                    chapter_text.push(' ');
                }
                entries.push(SearchEntry {
                    haystack: format!("{chapter_title} {chapter_text}").to_lowercase(),
                    result: SearchResult::for_action(
                        CHAPTER_KIND,
                        chapter_title,
                        Self::chapter_address(&book.id, chapter),
                    )
                    .with_subtitle(format!("{} · Chapter {}", title, chapter + 1))
                    .with_icon(Icon::List),
                });
            }
        }
        Self::append_annotation_entries(&mut entries, &live_annotations, books, &highlight_groups);

        let gram_index = Self::build_gram_index(&entries);
        Self {
            entries,
            gram_index,
        }
    }

    /// Index each distinct 1/2/3-gram once per row so repetition does not
    /// bloat postings or make ranking depend on repeated words.
    fn build_gram_index(entries: &[SearchEntry]) -> HashMap<GramKey, Vec<usize>> {
        let mut gram_index: HashMap<GramKey, Vec<usize>> = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let mut unique = HashSet::new();
            for_each_gram_key(&entry.haystack, |gram| {
                if unique.insert(gram) {
                    gram_index.entry(gram).or_default().push(index);
                }
            });
        }
        gram_index
    }

    fn append_annotation_entries(
        entries: &mut Vec<SearchEntry>,
        annotations: &[&Annotation],
        books: &[Book],
        highlight_groups: &HashSet<(String, uuid::Uuid)>,
    ) {
        let mut emitted_highlights = HashSet::new();
        for annotation in annotations {
            if annotation.kind == AnnoKind::Highlight
                && !emitted_highlights.insert((annotation.doc_id.clone(), annotation.group))
            {
                continue;
            }
            let book_title = books
                .iter()
                .find(|book| book.id == annotation.doc_id)
                .map_or("Unknown book", Self::book_title);
            entries.push(SearchEntry {
                haystack: format!("{} {}", annotation.excerpt, annotation.note).to_lowercase(),
                result: SearchResult::for_action(
                    ANNOTATION_KIND,
                    Self::preview(annotation),
                    Self::annotation_address(annotation, highlight_groups),
                )
                .with_subtitle(format!(
                    "{} · {} · Chapter {}",
                    book_title,
                    annotation.kind.label(),
                    annotation.chapter + 1
                ))
                .with_icon(match annotation.kind {
                    AnnoKind::Bookmark => Icon::Star,
                    _ => Icon::List,
                }),
            });
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

    fn annotation_address(
        annotation: &Annotation,
        highlight_groups: &HashSet<(String, uuid::Uuid)>,
    ) -> ActionAddress {
        let resource = match annotation.kind {
            AnnoKind::Highlight => format!("ebook.highlight:{}", annotation.group),
            AnnoKind::Bookmark => format!("ebook.bookmark:{}", annotation.id),
            AnnoKind::Note
                if highlight_groups.contains(&(annotation.doc_id.clone(), annotation.group)) =>
            {
                format!("ebook.comment:{}", annotation.id)
            }
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
}

impl SearchProvider for BookSearchProvider {
    fn name(&self) -> &'static str {
        "Books"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let q = query.trim().to_lowercase();
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        if limit == 0 {
            return Vec::new();
        }
        if q.is_empty() {
            return self
                .entries
                .iter()
                .take(limit)
                .map(|entry| entry.result.clone())
                .collect();
        }

        self.candidate_indexes(&q)
            .into_iter()
            // The final substring check makes every returned hit exact while
            // intersecting *all* query grams prevents an exact title hit from
            // suppressing other partial body/annotation matches.
            .filter_map(|index| self.entries.get(index))
            .filter(|entry| entry.haystack.contains(&q))
            .take(limit)
            .map(|entry| entry.result.clone())
            .collect()
    }
}

impl BookSearchProvider {
    /// Return a bounded, ordered candidate set by intersecting every query
    /// gram. Posting lists are entry-order sorted as they are built, so output
    /// is stable and exact/title results are never treated as a terminal tier.
    fn candidate_indexes(&self, query: &str) -> Vec<usize> {
        let chars = query.chars().count();
        let gram_len = chars.min(3);
        let Some(mut postings) = gram_keys(query, gram_len)
            .into_iter()
            .map(|gram| self.gram_index.get(&gram))
            .collect::<Option<Vec<_>>>()
        else {
            return Vec::new();
        };
        postings.sort_by_key(|list| list.len());
        let Some((first, rest)) = postings.split_first() else {
            return Vec::new();
        };
        first
            .iter()
            .copied()
            .filter(|candidate| {
                rest.iter()
                    .all(|list| list.binary_search(candidate).is_ok())
            })
            .take(MAX_QUERY_CANDIDATES)
            .collect()
    }

    #[cfg(test)]
    fn index_stats(&self) -> (usize, usize) {
        (
            self.gram_index.len(),
            self.gram_index.values().map(Vec::len).sum(),
        )
    }
}

/// Run `visit` once for every 1/2/3-scalar gram in `text`, without building a
/// temporary character array or allocating a string for every overlapping
/// window. This preserves Unicode scalar boundaries — unlike byte trigrams,
/// it cannot split a multi-byte character.
fn for_each_gram_key(text: &str, mut visit: impl FnMut(GramKey)) {
    let mut prev2 = '\0';
    let mut prev1 = '\0';
    for (seen, ch) in text.chars().enumerate() {
        visit(pack_gram(ch, None, None));
        if seen >= 1 {
            visit(pack_gram(prev1, Some(ch), None));
        }
        if seen >= 2 {
            visit(pack_gram(prev2, Some(prev1), Some(ch)));
        }
        prev2 = prev1;
        prev1 = ch;
    }
}

/// Pack up to three Unicode scalar values in their original order. Unicode's
/// largest scalar (`0x10_FFFF`) fits in 21 bits.
fn pack_gram(a: char, b: Option<char>, c: Option<char>) -> GramKey {
    u64::from(u32::from(a))
        | (b.map_or(GRAM_END, |value| u64::from(u32::from(value))) << 21)
        | (c.map_or(GRAM_END, |value| u64::from(u32::from(value))) << 42)
}

/// Compact query grams; this allocates only one small vector for sorting the
/// relevant posting lists, never one allocation per window.
fn gram_keys(text: &str, width: usize) -> Vec<GramKey> {
    let mut keys = Vec::with_capacity(text.chars().count().saturating_sub(width - 1));
    let mut prev2 = '\0';
    let mut prev1 = '\0';
    for (seen, ch) in text.chars().enumerate() {
        match width {
            1 => keys.push(pack_gram(ch, None, None)),
            2 if seen >= 1 => keys.push(pack_gram(prev1, Some(ch), None)),
            3 if seen >= 2 => keys.push(pack_gram(prev2, Some(prev1), Some(ch))),
            _ => {}
        }
        prev2 = prev1;
        prev1 = ch;
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ROOT};
    use ui::widget::Widget;

    fn sample_books() -> Vec<Book> {
        crate::content::load_sample_library()
    }

    #[test]
    fn every_result_uses_a_canonical_resource_action() {
        let books = sample_books();
        let annotation = Annotation::new(&books[0].id, AnnoKind::Bookmark, 2, 4)
            .with_excerpt("the wine-dark sea");
        let provider = BookSearchProvider::new(&books, &[annotation]);
        let hits = provider.search("");
        assert!(!hits.is_empty());
        assert!(hits.iter().all(|hit| hit.action_address().is_some()));
        assert!(hits.iter().any(|hit| hit.kind == CHAPTER_KIND));
        assert!(hits.iter().any(|hit| hit.kind == ANNOTATION_KIND));
    }

    #[test]
    fn chapter_body_text_is_searchable() {
        let books = sample_books();
        let needle = books[0].chapters[0]
            .blocks
            .iter()
            .map(crate::model::Block::plain_text)
            .find_map(|text| {
                text.split_whitespace()
                    .find(|word| word.len() > 7)
                    .map(str::to_owned)
            })
            .expect("sample chapter has searchable prose");
        let hits = BookSearchProvider::new(&books, &[]).search(&needle);
        assert!(hits.iter().any(|hit| hit.kind == CHAPTER_KIND));
    }

    #[test]
    fn search_address_resolves_and_navigates_exactly() {
        let books = sample_books();
        let book_id = books[0].id.clone();
        let annotation =
            Annotation::new(&book_id, AnnoKind::Bookmark, 1, 3).with_excerpt("wine-dark sea");
        let mut doc = AutoCommit::new();
        crate::doc::BooksDoc::upsert_annotation(&mut doc, &annotation);
        let mut app = crate::app::BooksApp::with_library(books);
        app.sync(&mut doc, &ROOT);

        let hit = app.search_providers()[0]
            .search("wine-dark")
            .into_iter()
            .find(|hit| hit.kind == ANNOTATION_KIND)
            .expect("annotation is searchable");
        let address = hit.action_address().expect("canonical address");
        let resources = app.resources(&doc);
        let encoded = qos_actions::door::encode(&resources, &address, None)
            .expect("search address resolves against the exported tree");
        assert!(
            app.invoke_resource_operation(
                qos_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
                &doc,
                &ROOT,
            )
            .unwrap()
        );
        assert_eq!(app.open_book().map(|book| &book.id), Some(&book_id));
        assert_eq!(app.search_navigation_target(), (1, Some(3)));
    }

    #[test]
    fn labels_are_concise_and_disambiguate_the_book() {
        let books = sample_books();
        let annotation = Annotation::new(&books[0].id, AnnoKind::Note, 0, 0)
            .with_excerpt("context")
            .with_note("A very useful thought");
        let hits = BookSearchProvider::new(&books, &[annotation]).search("useful");
        let hit = hits.iter().find(|hit| hit.kind == ANNOTATION_KIND).unwrap();
        assert_eq!(hit.title, "A very useful thought");
        assert!(
            hit.subtitle
                .as_deref()
                .unwrap()
                .contains(BookSearchProvider::book_title(&books[0]))
        );
    }

    #[test]
    fn limited_search_uses_a_hard_materialisation_budget() {
        let books = sample_books();
        let provider = BookSearchProvider::new(&books, &[]);
        let hits = provider.search_limited("", 3);
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().all(|hit| hit.action_address().is_some()));
        assert!(provider.search_limited("", 0).is_empty());
    }

    #[test]
    fn trigram_index_preserves_substring_matches() {
        let books = sample_books();
        let needle = books[0].chapters[0]
            .blocks
            .iter()
            .map(crate::model::Block::plain_text)
            .find_map(|text| {
                let chars = text.chars().collect::<Vec<_>>();
                (chars.len() > 8).then(|| chars[2..8].iter().collect::<String>())
            })
            .expect("sample chapter has a substring");
        let hits = BookSearchProvider::new(&books, &[]).search_limited(&needle, 2);
        assert!(hits.iter().any(|hit| hit.kind == CHAPTER_KIND));
    }

    #[test]
    fn complete_index_includes_books_after_the_old_ui_cap() {
        let mut books = (0..4_097)
            .map(|i| Book {
                id: format!("book:{i}"),
                title: format!("Library volume {i}"),
                ..Book::default()
            })
            .collect::<Vec<_>>();
        books.last_mut().expect("nonempty test library").title = "Late catalogue needle".into();

        let hits = BookSearchProvider::new(&books, &[]).search("catalogue needle");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, BOOK_KIND);
        assert_eq!(hits[0].title, "Late catalogue needle");
    }

    #[test]
    fn complete_index_keeps_text_after_the_old_chapter_cap() {
        let book = Book {
            id: "long-book".into(),
            title: "Long book".into(),
            chapters: vec![crate::model::Chapter {
                title: "A long chapter".into(),
                blocks: vec![crate::model::Block::Math {
                    expr: ui::math::MathExpr::new(),
                    source: format!("{} tail-needle", "x".repeat(65_537)),
                    display: false,
                }],
                ..crate::model::Chapter::default()
            }],
            ..Book::default()
        };

        let hits = BookSearchProvider::new(&[book], &[]).search("tail-needle");
        assert!(hits.iter().any(|hit| hit.kind == CHAPTER_KIND));
    }

    #[test]
    fn title_exact_match_does_not_hide_partial_body_match() {
        let books = vec![
            Book {
                id: "title".into(),
                title: "Needle".into(),
                ..Book::default()
            },
            Book {
                id: "body".into(),
                title: "Other".into(),
                chapters: vec![crate::model::Chapter {
                    title: "Chapter".into(),
                    blocks: vec![crate::model::Block::Math {
                        expr: ui::math::MathExpr::new(),
                        source: "a partial needle match in the book body".into(),
                        display: false,
                    }],
                    ..crate::model::Chapter::default()
                }],
                ..Book::default()
            },
        ];
        let provider = BookSearchProvider::new(&books, &[]);

        let hits = provider.search_limited("needle", 10);
        assert!(hits.iter().any(|hit| hit.kind == BOOK_KIND));
        assert!(hits.iter().any(|hit| hit.kind == CHAPTER_KIND));
    }

    #[test]
    fn candidate_probe_is_bounded_even_for_a_common_short_query() {
        let books = (0..(MAX_QUERY_CANDIDATES + 100))
            .map(|i| Book {
                id: format!("book:{i}"),
                title: "A common title".into(),
                ..Book::default()
            })
            .collect::<Vec<_>>();
        let provider = BookSearchProvider::new(&books, &[]);

        assert_eq!(provider.candidate_indexes("a").len(), MAX_QUERY_CANDIDATES);
    }

    #[test]
    fn compact_unicode_index_preserves_unicode_substrings_and_linear_postings() {
        let books = (0..1_000)
            .map(|i| Book {
                id: format!("book:{i}"),
                title: format!("Über den Fluss {i}"),
                ..Book::default()
            })
            .collect::<Vec<_>>();
        let provider = BookSearchProvider::new(&books, &[]);
        let indexed_chars = provider
            .entries
            .iter()
            .map(|entry| entry.haystack.chars().count())
            .sum::<usize>();
        let (unique_keys, postings) = provider.index_stats();

        assert!(provider.search("den Fl").len() > 1);
        assert!(unique_keys <= postings);
        assert!(postings <= indexed_chars * 3);
    }

    #[test]
    fn gram_widths_do_not_alias_when_text_contains_nul() {
        assert_ne!(pack_gram('a', None, None), pack_gram('a', Some('\0'), None));
        assert_ne!(
            pack_gram('a', Some('b'), None),
            pack_gram('a', Some('b'), Some('\0'))
        );
    }
}
