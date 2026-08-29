//! Ebook library + reading positions as a qOS global-search source.
//!
//! Implements [`SearchProvider`] over snapshots of the library (every book
//! by title/author) and the open book's annotations (bookmarks/highlights/
//! notes by excerpt), findable from the unified Ctrl+K palette. Activating
//! a book opens it; activating an annotation jumps to its place.
//!
//! Annotations are scoped to the currently open book because that's the set
//! the app holds in memory (hydrated from the CRDT when a book opens).

use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult};
use uuid::Uuid;

use crate::model::{Annotation, Book};

/// The group header for book results.
pub const BOOK_KIND: &str = "Books";
/// The group header for annotation (bookmark / highlight / note) results.
pub const ANNOTATION_KIND: &str = "Bookmarks";

const BOOK_PREFIX: &str = "book:";
const ANNO_PREFIX: &str = "anno:";

/// What an activated row points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activation {
    /// Open a book by its (content-hash) id.
    Book(String),
    /// Jump to an annotation by id, within the open book.
    Annotation(Uuid),
}

impl Activation {
    /// Resolve an `action_id` this provider emitted back into a target.
    pub fn parse(action_id: &str) -> Option<Self> {
        if let Some(rest) = action_id.strip_prefix(BOOK_PREFIX) {
            (!rest.is_empty()).then(|| Activation::Book(rest.to_string()))
        } else if let Some(rest) = action_id.strip_prefix(ANNO_PREFIX) {
            rest.parse::<Uuid>().ok().map(Activation::Annotation)
        } else {
            None
        }
    }
}

/// A [`SearchProvider`] over a snapshot of the library and the open book's
/// annotations.
pub struct BookSearchProvider {
    books: Vec<Book>,
    annotations: Vec<Annotation>,
}

impl BookSearchProvider {
    /// Snapshot the current library and open-book annotations into a provider.
    pub fn new(books: &[Book], annotations: &[Annotation]) -> Self {
        Self {
            books: books.to_vec(),
            annotations: annotations.to_vec(),
        }
    }

    /// The `action_id` for a book row.
    pub fn book_action_id(id: &str) -> String {
        format!("{BOOK_PREFIX}{id}")
    }

    /// The `action_id` for an annotation row.
    pub fn anno_action_id(id: Uuid) -> String {
        format!("{ANNO_PREFIX}{id}")
    }

    fn book_title(b: &Book) -> &str {
        if b.title.trim().is_empty() {
            "Untitled"
        } else {
            b.title.as_str()
        }
    }
}

impl SearchProvider for BookSearchProvider {
    fn name(&self) -> &'static str {
        "Books"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let q = query.to_lowercase();
        let mut out = Vec::new();

        for b in &self.books {
            let matches = q.is_empty()
                || b.title.to_lowercase().contains(&q)
                || b.author.to_lowercase().contains(&q);
            if !matches {
                continue;
            }
            let mut r =
                SearchResult::new(BOOK_KIND, Self::book_title(b), Self::book_action_id(&b.id))
                    .with_icon(Icon::FileText);
            if !b.author.trim().is_empty() {
                r = r.with_subtitle(b.author.clone());
            }
            out.push(r);
        }

        for a in &self.annotations {
            // A bookmark's human text is its excerpt (a snapshot of the anchored
            // passage); a note may add its own text.
            let hay = format!("{} {}", a.excerpt, a.note).to_lowercase();
            if !q.is_empty() && !hay.contains(&q) {
                continue;
            }
            let title = if a.excerpt.trim().is_empty() {
                a.kind.label().to_string()
            } else {
                a.excerpt.clone()
            };
            let icon = match a.kind {
                crate::model::AnnoKind::Bookmark => Icon::Star,
                _ => Icon::List,
            };
            out.push(
                SearchResult::new(ANNOTATION_KIND, title, Self::anno_action_id(a.id))
                    .with_subtitle(format!("{} · chapter {}", a.kind.label(), a.chapter + 1))
                    .with_icon(icon),
            );
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AnnoKind;
    use ui::search::SearchRegistry;

    fn sample_books() -> Vec<Book> {
        crate::content::load_sample_library()
    }

    #[test]
    fn provider_finds_a_book_by_title_or_author() {
        let books = sample_books();
        let provider = BookSearchProvider::new(&books, &[]);
        let some = books.first().expect("sample library is populated").clone();
        let word = some.title.split_whitespace().next().unwrap();
        let hits = provider.search(word);
        assert!(
            hits.iter().any(|r| r.kind == BOOK_KIND),
            "a real title word surfaces a book row"
        );
    }

    #[test]
    fn provider_finds_an_annotation_by_excerpt() {
        let books = sample_books();
        let book_id = books.first().unwrap().id.clone();
        let anno =
            Annotation::new(&book_id, AnnoKind::Bookmark, 2, 0).with_excerpt("the wine-dark sea");
        let provider = BookSearchProvider::new(&books, std::slice::from_ref(&anno));
        let hits = provider.search("wine-dark");
        let hit = hits
            .iter()
            .find(|r| r.kind == ANNOTATION_KIND)
            .expect("the bookmark surfaces by its excerpt");
        assert_eq!(
            Activation::parse(&hit.action_id),
            Some(Activation::Annotation(anno.id))
        );
    }

    #[test]
    fn book_action_id_round_trips() {
        let books = sample_books();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(BookSearchProvider::new(&books, &[])));
        let b = books.first().unwrap().clone();
        let hit = registry
            .query("")
            .into_iter()
            .find(|r| r.action_id == BookSearchProvider::book_action_id(&b.id))
            .expect("the book is browsable through the registry");
        assert_eq!(
            Activation::parse(&hit.action_id),
            Some(Activation::Book(b.id))
        );
    }

    #[test]
    fn parse_rejects_foreign_ids() {
        assert!(Activation::parse("file:/etc/passwd").is_none());
        assert!(Activation::parse("anno:not-a-uuid").is_none());
        assert!(Activation::parse("book:").is_none());
    }

    #[test]
    fn widget_hooks_find_then_open_the_book() {
        use automerge::{AutoCommit, ROOT};
        use ui::widget::Widget;

        let mut app = crate::app::BooksApp::with_library(sample_books());
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &ROOT);

        let (action_id, id) = {
            let providers = app.search_providers();
            assert!(!providers.is_empty(), "ebook publishes a search provider");
            let hit = providers[0]
                .search("")
                .into_iter()
                .find(|r| r.kind == BOOK_KIND)
                .expect("a book is browsable through the real hook");
            let Some(Activation::Book(id)) = Activation::parse(&hit.action_id) else {
                panic!("book row carries a book action_id");
            };
            (hit.action_id, id)
        };

        app.activate_search_result(&action_id, &mut doc, &ROOT);
        assert_eq!(
            app.open_book().map(|b| b.id.clone()),
            Some(id),
            "activation opened the chosen book in the reader"
        );
    }
}
