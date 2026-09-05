//! Library-wide Books search backed by canonical resource view actions.
//!
//! Every hit names an action exported by [`crate::actions`]. The shared search
//! host resolves that stable address against the fresh resource tree before it
//! navigates, so stale rows never fall through to private activation code.

use std::collections::{HashMap, HashSet};

use qos_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::model::{AnnoKind, Annotation, Book};

pub const BOOK_KIND: &str = "Books";
pub const CHAPTER_KIND: &str = "Chapters";
pub const ANNOTATION_KIND: &str = "Annotations";
const MAX_SEARCH_TEXT_CHARS: usize = 65_536;

pub struct BookSearchProvider {
    entries: Vec<SearchEntry>,
    trigram_index: HashMap<String, Vec<usize>>,
}

struct SearchEntry {
    haystack: String,
    result: SearchResult,
}

impl BookSearchProvider {
    pub fn new(books: &[Book], annotations: &[Annotation]) -> Self {
        let indexed_books = books
            .iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .collect::<Vec<_>>();
        let book_ids: HashSet<&str> = indexed_books.iter().map(|book| book.id.as_str()).collect();
        let live_annotations = annotations
            .iter()
            .filter(|annotation| book_ids.contains(annotation.doc_id.as_str()))
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .collect::<Vec<_>>();
        let highlight_groups: HashSet<_> = live_annotations
            .iter()
            .filter(|annotation| annotation.kind == AnnoKind::Highlight)
            .map(|annotation| (annotation.doc_id.clone(), annotation.group))
            .collect();
        let mut entries = Vec::new();
        'books: for book in indexed_books {
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
            if entries.len() == DEFAULT_SEARCH_INDEX_LIMIT {
                break;
            }
            for (chapter, item) in book.chapters.iter().enumerate() {
                let chapter_title = if item.title.trim().is_empty() {
                    format!("Chapter {}", chapter + 1)
                } else {
                    item.title.clone()
                };
                let mut chapter_text = String::new();
                let mut indexed_chars = 0;
                for block in &item.blocks {
                    for ch in crate::model::Block::plain_text(block).chars() {
                        if indexed_chars == MAX_SEARCH_TEXT_CHARS {
                            break;
                        }
                        chapter_text.push(ch);
                        indexed_chars += 1;
                    }
                    if indexed_chars == MAX_SEARCH_TEXT_CHARS {
                        break;
                    }
                    chapter_text.push(' ');
                    indexed_chars += 1;
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
                if entries.len() == DEFAULT_SEARCH_INDEX_LIMIT {
                    break 'books;
                }
            }
        }
        let mut emitted_highlights = HashSet::new();
        for annotation in live_annotations {
            if entries.len() == DEFAULT_SEARCH_INDEX_LIMIT {
                break;
            }
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
                    Self::annotation_address(annotation, &highlight_groups),
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

        let mut trigram_index: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let mut unique = HashSet::new();
            for trigram in trigrams(&entry.haystack) {
                if unique.insert(trigram.clone()) {
                    trigram_index.entry(trigram).or_default().push(index);
                }
            }
        }
        Self {
            entries,
            trigram_index,
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
        let candidates: Box<dyn Iterator<Item = usize> + '_> = if q.chars().count() >= 3 {
            let first = trigrams(&q).into_iter().next();
            Box::new(
                first
                    .and_then(|key| self.trigram_index.get(&key))
                    .into_iter()
                    .flatten()
                    .copied(),
            )
        } else {
            Box::new(0..self.entries.len())
        };
        candidates
            .filter_map(|index| self.entries.get(index))
            .filter(|entry| q.is_empty() || entry.haystack.contains(&q))
            .take(limit)
            .map(|entry| entry.result.clone())
            .collect()
    }
}

fn trigrams(text: &str) -> Vec<String> {
    let chars = text.chars().collect::<Vec<_>>();
    chars
        .windows(3)
        .map(|window| window.iter().collect())
        .collect()
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
}
