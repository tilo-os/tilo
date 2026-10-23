//! Project Gutenberg's catalogue kept on this device: one copy for every
//! app and workspace, which the filesystem daemon fetches, refreshes and
//! evicts (`public_content` in the fs crate). Books asks the copy instead of
//! the network when it is here, so the store's rows come at the speed of
//! the library's; until then, and wherever no copy is kept, the store is
//! asked over the network ([`crate::store::Gutenberg`]).
//!
//! Keeping the copy is the person's choice per device
//! ([`CatalogueCopies`]): on by default on their own computer, off in a
//! browser, where the browser lends the storage.

use std::io::Read as _;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use ui::content::PublicState;
use ui::storage::{DeviceClass, Scope, SettingsSchema};
use ui::tilo_resource;

use crate::store::StoreBook;

/// The name the daemon keeps the catalogue under.
pub(crate) const CATALOGUE: &str = "gutenberg-catalogue";

/// What the setting is called wherever it shows.
pub(crate) const KEEP_CATALOGUES: &str = "Keep book catalogues on this device";

/// The most the decompressed catalogue may weigh: it is about 25 MB.
const MAX_CATALOGUE_BYTES: u64 = 256 * 1024 * 1024;

/// Whether this device keeps a copy of the book catalogues Books searches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Model)]
#[serde(default)]
pub struct CatalogueCopies {
    /// Keep book catalogues on this device
    pub keep_catalogues: bool,
}

impl Default for CatalogueCopies {
    fn default() -> Self {
        Self::defaults_on(DeviceClass::Native)
    }
}

impl SettingsSchema for CatalogueCopies {
    const SCOPE: Scope = Scope::DeviceLocal;

    fn defaults_on(device: DeviceClass) -> Self {
        Self {
            keep_catalogues: device == DeviceClass::Native,
        }
    }

    /// Only the person's own computer keeps a copy: a browser has no device
    /// store to keep it in.
    fn available_on(device: DeviceClass) -> bool {
        device == DeviceClass::Native
    }
}

/// The section as a record the person edits by path.
#[tilo_resource::resource(kind = "settings.books-catalogues", label = "Book catalogues")]
impl CatalogueCopies {}

/// The catalogue's books, searchable by any words of their title and
/// author.
#[derive(Debug, Default)]
pub(crate) struct CatalogueIndex {
    books: Vec<Entry>,
}

#[derive(Debug)]
struct Entry {
    book: StoreBook,
    /// The title, folded as searches are.
    title: String,
    /// The author, folded.
    author: String,
}

impl CatalogueIndex {
    /// The books of a decompressed catalogue (`pg_catalog.csv`): its texts,
    /// leaving out its audio books, which Books cannot read.
    pub(crate) fn parse(csv: &str) -> Self {
        let mut rows = Rows::new(csv);
        let Some(header) = rows.next() else {
            return Self::default();
        };
        let column = |name: &str| header.iter().position(|field| field == name);
        let (Some(id), Some(kind), Some(title), Some(authors)) = (
            column("Text#"),
            column("Type"),
            column("Title"),
            column("Authors"),
        ) else {
            return Self::default();
        };
        let books = rows
            .filter(|row| row.get(kind).is_some_and(|kind| kind == "Text"))
            .filter_map(|row| {
                let book = StoreBook {
                    id: row.get(id)?.parse().ok()?,
                    title: one_line(row.get(title)?),
                    author: first_author(row.get(authors).map_or("", String::as_str)),
                };
                (!book.title.is_empty()).then(|| Entry {
                    title: ui::search::normalize_search_text(&book.title),
                    author: ui::search::normalize_search_text(&book.author),
                    book,
                })
            })
            .collect();
        Self { books }
    }

    /// The books of the gzip-compressed catalogue the daemon keeps.
    pub(crate) fn from_gzip(bytes: &[u8]) -> Option<Self> {
        let mut csv = String::new();
        flate2::read::GzDecoder::new(bytes)
            .take(MAX_CATALOGUE_BYTES)
            .read_to_string(&mut csv)
            .ok()?;
        Some(Self::parse(&csv))
    }

    pub(crate) fn len(&self) -> usize {
        self.books.len()
    }

    /// The books every one of `words` (already folded) finds in their title
    /// or author, at most `limit`: those found by their title alone first,
    /// shorter titles first, then by catalogue number.
    pub(crate) fn search(&self, words: &[String], limit: usize) -> Vec<StoreBook> {
        if words.is_empty() {
            return Vec::new();
        }
        let mut found: Vec<(bool, usize, &Entry)> = self
            .books
            .iter()
            .filter_map(|entry| {
                let mut by_title = true;
                for word in words {
                    if entry.title.contains(word.as_str()) {
                        continue;
                    }
                    if !entry.author.contains(word.as_str()) {
                        return None;
                    }
                    by_title = false;
                }
                Some((!by_title, entry.title.len(), entry))
            })
            .collect();
        found.sort_by_key(|&(by_author, len, entry)| (by_author, len, entry.book.id));
        found
            .into_iter()
            .take(limit)
            .map(|(_, _, entry)| entry.book.clone())
            .collect()
    }
}

/// A title on one line: a subtitle the catalogue puts on the next line
/// follows a colon.
fn one_line(title: &str) -> String {
    title
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(": ")
}

/// The first author of the catalogue's list ("Shelley, Mary Wollstonecraft,
/// 1797-1851; Ward, Lynd [Illustrator]") as people write names: "Mary
/// Wollstonecraft Shelley". Contributors in brackets (illustrators,
/// translators) are not the author.
fn first_author(authors: &str) -> String {
    let Some(author) = authors
        .split(';')
        .map(str::trim)
        .find(|author| !author.is_empty() && !author.contains('['))
    else {
        return String::new();
    };
    let parts: Vec<&str> = author
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty() && !part.starts_with(|c: char| c.is_ascii_digit()))
        .filter(|part| !part.starts_with("BCE") && !part.starts_with("active"))
        .collect();
    match parts.as_slice() {
        [last, first, ..] => format!("{first} {last}"),
        [name] => (*name).to_owned(),
        [] => String::new(),
    }
}

/// The rows of a CSV document (RFC 4180): fields split at commas, a quoted
/// field holding commas, line breaks and doubled quotes.
struct Rows<'a> {
    rest: std::str::Chars<'a>,
    done: bool,
}

impl<'a> Rows<'a> {
    fn new(csv: &'a str) -> Self {
        Self {
            rest: csv.chars(),
            done: csv.is_empty(),
        }
    }
}

impl Iterator for Rows<'_> {
    type Item = Vec<String>;

    fn next(&mut self) -> Option<Vec<String>> {
        if self.done {
            return None;
        }
        let mut row = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        loop {
            let Some(c) = self.rest.next() else {
                self.done = true;
                row.push(field);
                return Some(row);
            };
            match (quoted, c) {
                (true, '"') => {
                    if self.rest.as_str().starts_with('"') {
                        self.rest.next();
                        field.push('"');
                    } else {
                        quoted = false;
                    }
                }
                (false, '"') => quoted = true,
                (false, ',') => row.push(std::mem::take(&mut field)),
                (false, '\r') => {}
                (false, '\n') => {
                    row.push(field);
                    if self.rest.as_str().is_empty() {
                        self.done = true;
                    }
                    return Some(row);
                }
                (_, c) => field.push(c),
            }
        }
    }
}

/// A public file the daemon keeps: the device's capability, or a test's
/// stand-in.
pub(crate) trait PublicFile: Send + Sync {
    fn keep(&self) -> std::io::Result<()>;
    fn release(&self) -> std::io::Result<()>;
    /// The current copy's hash, when it is wholly here.
    fn here(&self) -> std::io::Result<Option<String>>;
    fn read(&self) -> std::io::Result<PublicState>;
}

impl PublicFile for ui::content::PublicCopy {
    fn keep(&self) -> std::io::Result<()> {
        Self::keep(self)
    }

    fn release(&self) -> std::io::Result<()> {
        Self::release(self)
    }

    fn here(&self) -> std::io::Result<Option<String>> {
        Self::here(self)
    }

    fn read(&self) -> std::io::Result<PublicState> {
        Self::read(self)
    }
}

/// The catalogue copy as Books sees it: kept or released as the setting
/// says, and its index built once per copy.
pub(crate) struct DeviceCatalogue {
    copy: Option<Arc<dyn PublicFile>>,
    /// The index of the copy it was built from, by the copy's hash.
    index: Mutex<Option<(String, Arc<CatalogueIndex>)>>,
    kept: Mutex<Option<bool>>,
}

impl DeviceCatalogue {
    /// The catalogue on this device, through the capability the launcher
    /// handed Books; a host without one keeps no copy.
    pub(crate) fn on_this_device() -> Arc<Self> {
        Self::over(
            ui::content::public_copy(CATALOGUE).map(|copy| Arc::new(copy) as Arc<dyn PublicFile>),
        )
    }

    /// The catalogue kept as `copy`; with none, every search goes to the
    /// network.
    pub(crate) fn over(copy: Option<Arc<dyn PublicFile>>) -> Arc<Self> {
        Arc::new(Self {
            copy,
            index: Mutex::new(None),
            kept: Mutex::new(None),
        })
    }

    /// Keep the copy on this device, or let it go: released, it is no longer
    /// refreshed and the device evicts it when it needs the room, and
    /// searches go back to the network.
    pub(crate) fn keep(&self, keep: bool) {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        if *kept == Some(keep) {
            return;
        }
        let Some(copy) = &self.copy else {
            return;
        };
        let done = if keep { copy.keep() } else { copy.release() };
        match done {
            Ok(()) => {
                *kept = Some(keep);
                drop(kept);
                if !keep {
                    *self.index.lock().unwrap_or_else(PoisonError::into_inner) = None;
                }
            }
            Err(error) => {
                drop(kept);
                tracing::debug!(%error, keep, "books: the catalogue copy did not change");
            }
        }
    }

    /// The index of the copy, when one is here; built the first time it is
    /// asked for after the copy arrived or changed. Runs off the UI thread.
    pub(crate) fn index(&self) -> Option<Arc<CatalogueIndex>> {
        if *self.kept.lock().unwrap_or_else(PoisonError::into_inner) != Some(true) {
            return None;
        }
        let copy = self.copy.as_ref()?;
        let held = self
            .index
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let current = copy.here().ok().flatten();
        match (current, held) {
            (Some(hash), Some((built, index))) if hash == built => Some(index),
            (Some(_), _) => {
                let PublicState::Here { bytes, hash } = copy.read().ok()? else {
                    return None;
                };
                let index = Arc::new(CatalogueIndex::from_gzip(&bytes)?);
                *self.index.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some((hash, Arc::clone(&index)));
                Some(index)
            }
            // The device made room: the index built already still answers
            // while the daemon fetches the copy again.
            (None, held) => held.map(|(_, index)| index),
        }
    }
}

#[cfg(test)]
mod tests;
