//! The book store Books searches beside the library: Project Gutenberg's
//! catalogue of free books. Where this device keeps a copy of the catalogue
//! ([`crate::catalogue`]) the copy answers; otherwise the store is asked as
//! the person types. A book found there is fully usable before it is local:
//! getting it fetches the EPUB and opens it like a book picked from disk.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ui::framework::views::{Cancel, Item, QuerySpec, RemoteError, RemoteSource};

use crate::search_provider::AUTHOR_FACET;

/// The type of a book in the store's answers.
pub(crate) const STORE_BOOK: &str = "books.store-book";

/// What the store's section is called.
pub(crate) const STORE_LABEL: &str = "Project Gutenberg";

/// Where the catalogue is searched.
const SEARCH_URL: &str = "https://www.gutenberg.org/ebooks/search.opds/";

/// The largest catalogue page read, in bytes.
const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;

/// The largest book fetched, in bytes: the reader's own EPUB budget.
pub(crate) const MAX_BOOK_BYTES: usize = 128 * 1024 * 1024;

/// One book in the store: what its row shows and what getting it fetches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoreBook {
    /// The catalogue number.
    pub id: u64,
    pub title: String,
    pub author: String,
}

impl StoreBook {
    /// The book as a result row, carrying itself for getting it.
    pub(crate) fn item(self) -> Item {
        Item::new(STORE_BOOK, format!("gutenberg:{}", self.id))
            .with("title", self.title.clone())
            .with(AUTHOR_FACET, self.author.clone())
            .payload(Arc::new(self))
    }
}

/// Where the EPUB of catalogue book `id` is fetched from.
pub(crate) fn book_url(id: u64) -> String {
    format!("https://www.gutenberg.org/ebooks/{id}.epub3.images")
}

/// Where the cover of catalogue book `id` is fetched from.
pub(crate) fn cover_url(id: u64) -> String {
    format!("https://www.gutenberg.org/cache/epub/{id}/pg{id}.cover.medium.jpg")
}

/// The largest cover fetched, in bytes.
const MAX_COVER_BYTES: usize = 2 * 1024 * 1024;

/// How the store's covers are fetched: from the store alone, off the UI
/// thread, on the connection its searches keep open.
pub(crate) fn cover_fetcher() -> ui::Fetcher {
    Arc::new(|url: &str| {
        if !url.starts_with("https://www.gutenberg.org/") {
            return None;
        }
        fetch(url, &[], MAX_COVER_BYTES)
            .map_err(|error| tracing::debug!(%error, "books: a store cover did not arrive"))
            .ok()
    })
}

/// Where a fetched book is kept until the import has placed it on the
/// shelf.
pub(crate) fn fetched_path(id: u64) -> PathBuf {
    tilo_paths::cache_dir()
        .or_else(tilo_paths::temp_dir)
        .unwrap_or_default()
        .join("so.tilo.ebook")
        .join("store")
        .join(format!("gutenberg-{id}.epub"))
}

/// The client every request to the store goes through, so its connection
/// stays open between keystrokes.
fn client() -> Result<&'static tilo_net::Client, RemoteError> {
    static CLIENT: OnceLock<Option<tilo_net::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            tilo_net::Client::builder()
                .user_agent("Tilo-Books/0.1 (+https://tilo.so)")
                .timeout(Duration::from_secs(20))
                .fetch_proxy(tilo_net::proxy::ProxyUse::CrossOrigin)
                .build()
                .ok()
        })
        .as_ref()
        .ok_or_else(|| RemoteError("Books can\u{2019}t reach the internet here.".into()))
}

/// GET `url` and read at most `limit` bytes of it, from this (non-UI)
/// thread.
pub(crate) fn fetch(url: &str, query: &[(&str, &str)], limit: usize) -> Result<Vec<u8>, String> {
    let client = client().map_err(|error| error.0)?;
    let request = client.get(url).query(query);
    let runtime = ui::rt::try_app().map_err(|error| error.to_string())?;
    runtime
        .block_on(async move {
            let response = request
                .send()
                .await
                .map_err(|error| error.to_string())?
                .error_for_status()
                .map_err(|error| error.to_string())?;
            response
                .bytes_limited(limit)
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|error| error.to_string())
        })
        .map_err(|error| error.to_string())?
}

/// Why a book did not arrive from the store: it could not be reached, so
/// trying again once online is the way on.
pub(crate) fn unreachable_message() -> String {
    format!("Couldn\u{2019}t get the book from {STORE_LABEL}. Try again when you\u{2019}re online.")
}

/// Fetch catalogue book `url` into `path`, unless an earlier fetch left it
/// there. Runs on the import worker.
///
/// # Errors
///
/// The store could not be reached, or the book could not be kept.
pub(crate) fn download(url: &str, path: &Path) -> Result<(), String> {
    if path.is_file() {
        return Ok(());
    }
    let bytes = fetch(url, &[], MAX_BOOK_BYTES).map_err(|error| {
        tracing::debug!(%error, "books: the store's book did not arrive");
        unreachable_message()
    })?;
    let keep = |error: tilo_files::FsError| {
        tracing::debug!(%error, "books: the store's book could not be kept");
        "Couldn\u{2019}t keep the book on this device.".to_string()
    };
    // Written whole or not at all, its folders made as needed.
    let (dir, name) = tilo_files::file_at(path).map_err(keep)?;
    tilo_files::blocking::write_atomic(&dir, &name, &bytes).map_err(keep)
}

/// The books in one page of the catalogue's OPDS answer, in its order.
/// Entries that are not books (the "Authors" and "Subjects" links it puts
/// first) are left out.
pub(crate) fn parse_page(xml: &str) -> Vec<StoreBook> {
    let Ok(document) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    document
        .descendants()
        .filter(|node| node.has_tag_name("entry"))
        .filter_map(|entry| {
            let text = |name: &str| {
                entry
                    .children()
                    .find(|child| child.has_tag_name(name))
                    .and_then(|child| child.text())
                    .map(str::trim)
                    .unwrap_or_default()
                    .to_owned()
            };
            let id = text("id");
            let number = id
                .strip_suffix(".opds")?
                .rsplit('/')
                .next()?
                .parse::<u64>()
                .ok()?;
            let title = text("title");
            (!title.is_empty()).then(|| StoreBook {
                id: number,
                title,
                author: text("content"),
            })
        })
        .collect()
}

/// What the store is asked for `query`: its words and any authors chosen.
fn store_words(query: &QuerySpec) -> String {
    let mut words = query.text.trim().to_owned();
    for chip in query.chips.iter().filter(|chip| chip.key == AUTHOR_FACET) {
        if let Some(author) = &chip.value {
            words.push(' ');
            words.push_str(author);
        }
    }
    words
}

/// Whether the store has anything to say to `query`: words, and no book of
/// the library chosen (a search inside a book is the book's alone).
fn store_accepts(query: &QuerySpec) -> bool {
    !query.text.trim().is_empty()
        && !query
            .chips
            .iter()
            .any(|chip| chip.key == crate::search_provider::BOOK_FACET)
}

/// Project Gutenberg: the device's copy of its catalogue when one is here,
/// asked over the network otherwise.
pub(crate) struct Gutenberg {
    catalogue: Option<Arc<crate::catalogue::DeviceCatalogue>>,
}

impl Gutenberg {
    /// Asked from `catalogue` while it holds a copy.
    pub(crate) fn with_catalogue(catalogue: Arc<crate::catalogue::DeviceCatalogue>) -> Self {
        Self {
            catalogue: Some(catalogue),
        }
    }

    /// Always asked over the network.
    pub(crate) fn live() -> Self {
        Self { catalogue: None }
    }

    fn copy(&self) -> Option<Arc<crate::catalogue::CatalogueIndex>> {
        self.catalogue.as_ref()?.index()
    }
}

impl RemoteSource for Gutenberg {
    fn id(&self) -> &'static str {
        "gutenberg"
    }

    fn label(&self) -> String {
        STORE_LABEL.into()
    }

    fn accepts(&self, query: &QuerySpec) -> bool {
        store_accepts(query)
    }

    fn warm(&self) {
        // Read the copy before the first character, or else open the
        // connection: the answer to nothing is small.
        if self.copy().is_none() {
            let _ = fetch(SEARCH_URL, &[("query", "")], MAX_PAGE_BYTES);
        }
    }

    fn search(
        &self,
        query: &QuerySpec,
        cancel: &Cancel,
        page: &mut dyn FnMut(Vec<Item>),
    ) -> Result<(), RemoteError> {
        let words = store_words(query);
        if let Some(copy) = self.copy() {
            let folded: Vec<String> = words
                .split_whitespace()
                .map(ui::search::normalize_search_text)
                .collect();
            page(
                copy.search(&folded, ui::framework::views::GROUP_LIMIT)
                    .into_iter()
                    .map(StoreBook::item)
                    .collect(),
            );
            return Ok(());
        }
        let body =
            fetch(SEARCH_URL, &[("query", words.as_str())], MAX_PAGE_BYTES).map_err(|error| {
                tracing::debug!(%error, "books: the store did not answer");
                RemoteError(format!("{STORE_LABEL} can\u{2019}t be reached right now."))
            })?;
        if cancel.is_cancelled() {
            return Ok(());
        }
        let xml = String::from_utf8_lossy(&body);
        page(parse_page(&xml).into_iter().map(StoreBook::item).collect());
        Ok(())
    }
}

/// The store's answers in captures and tests: a few well-known books, found
/// by any word of their title or author. Nothing goes over the network.
pub(crate) struct SampleStore;

/// The books [`SampleStore`] holds.
const SAMPLE_BOOKS: &[(u64, &str, &str)] = &[
    (2701, "Moby-Dick; or, The Whale", "Herman Melville"),
    (
        11,
        "Bartleby, the Scrivener: A Story of Wall-Street",
        "Herman Melville",
    ),
    (1342, "Pride and Prejudice", "Jane Austen"),
    (
        84,
        "Frankenstein; Or, The Modern Prometheus",
        "Mary Wollstonecraft Shelley",
    ),
    (
        1661,
        "The Adventures of Sherlock Holmes",
        "Arthur Conan Doyle",
    ),
    (98, "A Tale of Two Cities", "Charles Dickens"),
    (12, "Through the Looking-Glass", "Lewis Carroll"),
];

impl RemoteSource for SampleStore {
    fn id(&self) -> &'static str {
        "gutenberg"
    }

    fn label(&self) -> String {
        STORE_LABEL.into()
    }

    fn accepts(&self, query: &QuerySpec) -> bool {
        store_accepts(query)
    }

    fn search(
        &self,
        query: &QuerySpec,
        _cancel: &Cancel,
        page: &mut dyn FnMut(Vec<Item>),
    ) -> Result<(), RemoteError> {
        let words: Vec<String> = store_words(query)
            .split_whitespace()
            .map(ui::search::normalize_search_text)
            .collect();
        page(
            SAMPLE_BOOKS
                .iter()
                .filter(|(_, title, author)| {
                    let haystack = ui::search::normalize_search_text(&format!("{title} {author}"));
                    words.iter().all(|word| haystack.contains(word.as_str()))
                })
                .map(|&(id, title, author)| {
                    StoreBook {
                        id,
                        title: title.into(),
                        author: author.into(),
                    }
                    .item()
                })
                .collect(),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_catalogue_page_reads_as_its_books_in_order() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
<entry><id>https://www.gutenberg.org/ebooks/authors/search.opds/?query=moby</id><title>Authors</title><content type="text">One author name matches your search.</content></entry>
<entry><id>https://www.gutenberg.org/ebooks/2701.opds</id><title>Moby Dick; Or, The Whale</title><content type="text">Herman Melville</content></entry>
<entry><id>https://www.gutenberg.org/ebooks/15.opds</id><title>Moby-Dick; or, The Whale</title><content type="text">Herman Melville</content></entry>
</feed>"#;
        let books = parse_page(xml);
        assert_eq!(
            books,
            [
                StoreBook {
                    id: 2701,
                    title: "Moby Dick; Or, The Whale".into(),
                    author: "Herman Melville".into(),
                },
                StoreBook {
                    id: 15,
                    title: "Moby-Dick; or, The Whale".into(),
                    author: "Herman Melville".into(),
                },
            ]
        );
        assert!(parse_page("not xml").is_empty());
    }

    #[test]
    fn the_store_is_not_asked_inside_a_book() {
        let text = QuerySpec::text("whale");
        assert!(store_accepts(&text));
        assert!(!store_accepts(&QuerySpec::text("  ")));
        let inside = text.toggled(&ui::framework::views::Chip::value(
            crate::search_provider::BOOK_FACET,
            "Moby-Dick",
        ));
        assert!(!store_accepts(&inside));
        let by = QuerySpec::text("whale").toggled(&ui::framework::views::Chip::value(
            AUTHOR_FACET,
            "Herman Melville",
        ));
        assert_eq!(store_words(&by), "whale Herman Melville");
    }
}
