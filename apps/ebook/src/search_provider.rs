//! Library-wide Books search backed by canonical resource view actions.
//!
//! Every hit names an action exported by [`crate::actions`]. The shared search
//! host resolves that stable address against the fresh resource tree before it
//! navigates, so stale rows never fall through to private activation code.

use std::collections::HashSet;

use qos_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::model::{AnnoKind, Annotation, Book};

pub const BOOK_KIND: &str = "Books";
pub const CHAPTER_KIND: &str = "Chapters";
pub const ANNOTATION_KIND: &str = "Annotations";
pub struct BookSearchProvider {
    /// qOS' shared, paged catalogue. It retains every resource row but never
    /// forces Ctrl+K to inspect/materialise the entire library in one turn.
    catalogue: SearchCatalog,
}

struct SearchEntry {
    haystack: String,
    result: SearchResult,
}

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
                    // A chapter is part of its book, not an isolated file.
                    // Include book metadata here too so an author/title search
                    // brings matching passages along with the book row.
                    haystack: format!("{title} {} {chapter_title} {chapter_text}", book.author)
                        .to_lowercase(),
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

        let catalogue = SearchCatalog::new("Books");
        for entry in entries {
            let Some(address) = entry.result.action_address() else {
                continue;
            };
            // Every row above is constructed with `for_action`, and its
            // resource id is therefore both stable and canonical. The shared
            // catalogue enforces that contract defensively at this boundary.
            let _ = catalogue.upsert(address.resource, entry.haystack, entry.result);
        }
        Self { catalogue }
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
            let book_author = books
                .iter()
                .find(|book| book.id == annotation.doc_id)
                .map_or("", |book| book.author.as_str());
            let kind = annotation.kind.label();
            // A note that belongs to a highlight is the reader's comment
            // model. Index both names so people can search with either term.
            let comment_alias = if annotation.kind == AnnoKind::Note
                && highlight_groups.contains(&(annotation.doc_id.clone(), annotation.group))
            {
                "comment"
            } else {
                ""
            };
            entries.push(SearchEntry {
                haystack: format!(
                    "{book_title} {book_author} {kind} {comment_alias} {} {}",
                    annotation.excerpt, annotation.note
                )
                .to_lowercase(),
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

    fn action_directory_generation(&self) -> Option<u64> {
        Some(self.catalogue.stats().generation)
    }

    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.catalogue.cursor_is_current(cursor)
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.catalogue.search_limited(query, limit)
    }
    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.catalogue
            .query_page(query, cursor, work_budget, result_limit)
    }
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
    fn annotation_search_includes_book_context_and_comment_kind() {
        let books = sample_books();
        let book = &books[0];
        let group = uuid::Uuid::new_v4();
        let highlight = Annotation {
            id: group,
            doc_id: book.id.clone(),
            kind: AnnoKind::Highlight,
            chapter: 0,
            block: 0,
            start: 0,
            end: 1,
            excerpt: "marked passage".into(),
            note: String::new(),
            color: crate::model::HiColor::Yellow,
            created: 1,
            group,
        };
        let comment = Annotation {
            id: uuid::Uuid::new_v4(),
            doc_id: book.id.clone(),
            kind: AnnoKind::Note,
            chapter: 0,
            block: 0,
            start: 0,
            end: 1,
            excerpt: "marked passage".into(),
            note: "A shared observation".into(),
            color: crate::model::HiColor::Yellow,
            created: 2,
            group,
        };
        let provider = BookSearchProvider::new(&books, &[highlight, comment]);

        for query in [&book.author, "comment", "highlight", "shared observation"] {
            assert!(
                provider
                    .search(query)
                    .iter()
                    .any(|hit| hit.kind == ANNOTATION_KIND),
                "{query:?} should find the annotation context"
            );
        }
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
    fn paged_search_is_bounded_and_reaches_every_broad_match() {
        let books = (0..257)
            .map(|i| Book {
                id: format!("book:{i}"),
                title: "A common title".into(),
                ..Book::default()
            })
            .collect::<Vec<_>>();
        let provider = BookSearchProvider::new(&books, &[]);
        let mut cursor = None;
        let mut seen = 0usize;
        loop {
            let page = provider.search_page("common", cursor.as_ref(), 17, 5);
            assert!(page.inspected <= 17, "work must stay bounded per page");
            assert!(page.results.len() <= 5, "materialisation is page sized");
            seen += page.results.len();
            let Some(next) = page.next else {
                break;
            };
            cursor = Some(next);
        }
        assert_eq!(seen, books.len(), "continuations cannot lose broad hits");
    }

    #[test]
    fn paged_index_preserves_unicode_substrings() {
        let books = (0..1_000)
            .map(|i| Book {
                id: format!("book:{i}"),
                title: format!("Über den Fluss {i}"),
                ..Book::default()
            })
            .collect::<Vec<_>>();
        let provider = BookSearchProvider::new(&books, &[]);
        let page = provider.search_page("DEN FL", None, 8, 8);
        assert!(!page.results.is_empty());
        assert!(page.inspected <= 8);
    }

    #[test]
    fn a_unique_late_hit_remains_findable_beyond_one_hundred_thousand_books() {
        let mut books = (0..100_001)
            .map(|i| Book {
                id: format!("book:{i:06}"),
                title: format!("Library volume {i}"),
                ..Book::default()
            })
            .collect::<Vec<_>>();
        books.last_mut().expect("nonempty library").title = "Late catalogue needle".into();

        let provider = BookSearchProvider::new(&books, &[]);
        let page = provider.search_page("catalogue needle", None, 4, 4);
        assert_eq!(
            page.inspected, 1,
            "the rare index posting avoids a full scan"
        );
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].title, "Late catalogue needle");
    }
}
