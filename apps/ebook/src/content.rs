//! The library: the reader's own collection of books.
//!
//! The shelf is app-managed and persisted, not re-derived from a magic
//! folder every launch. A book enters the library when the reader opens it
//! or when it turns up in a watched folder; from then on its identity and
//! public metadata live in the CRDT ([`crate::resource`]). Source paths and folder
//! grants remain local. Shared copies resolve from their content-addressed
//! workspace filename; local copies are rediscovered only through explicitly
//! supplied watch roots. [`hydrate_library`] reads those available books, and
//! [`find_watched_books`] finds new ones for the reader to shelve.
//!
//! The multi-book sample library is reserved for screenshots and tests; the
//! bundled EPUB is the browser's one book.

use std::collections::{BTreeSet, HashMap};
use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

use tilo_clock::unix_now_ms;

use crate::epub::{MAX_EPUB_BYTES, content_id, parse_epub};
use crate::model::{Book, Chapter, LibraryEntry};
use crate::xhtml::parse_xhtml;

/// The bundled EPUB, parsed by the real container reader. The `--shots`/test
/// sample library and test fixtures use it; the native live shelf never gains
/// it by itself, and only a host without files places it there
/// ([`bundled_book_entry`]).
pub(crate) const SAMPLE_EPUB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/sample.epub"));

/// The shelf location of a bundled book: inside the app, never shared.
const BUNDLED_PATH: &str = "sample.epub";

/// The shelf location of a book held in the host's content store: not a
/// file, and never shared as one.
pub(crate) fn content_path(hash: &str) -> PathBuf {
    PathBuf::from("content").join(format!("{hash}.epub"))
}

/// Environment override (platform-separated absolute paths: `;` on Windows,
/// `:` on Unix) for extra folders to
/// watch. Lets the OS point the reader at a synced library (e.g. a
/// `/var/workspace` share) and lets tests pin a fixture directory.
const LIBRARY_DIR_ENV: &str = "EBOOK_LIBRARY_DIR";

/// Compatibility override for the collaborative-workspace mount root (tests
/// point it at a temp directory); production asks
/// [`tilo_paths::workspace_root`].
const WORKSPACE_DIR_ENV: &str = "TILO_WORKSPACE_DIR";

/// Upper bound on library size, so a pathological watched directory can't stall
/// startup parsing thousands of files.
const MAX_LIBRARY: usize = 200;

/// A book hydrated from disk together with where its file lives — enough for the
/// shelf to open it again and to tell a workspace-shared book from a local one.
pub struct LoadedBook {
    pub book: Book,
    pub path: PathBuf,
}

/// The collaborative-workspace mount root. A book under here is shared with the
/// workspace's peers; a book anywhere else is local to this device. Without a
/// mounted workspace this is the canonical root peers' documents name.
pub fn workspace_root() -> PathBuf {
    std::env::var_os(WORKSPACE_DIR_ENV)
        .map(PathBuf::from)
        .or_else(tilo_paths::workspace_root)
        .unwrap_or_else(|| PathBuf::from(tilo_paths::WORKSPACE_MOUNT))
}

/// Where a book copied into the workspace lands.
pub fn workspace_books_dir() -> PathBuf {
    workspace_root().join("Books")
}

/// Whether a path lives on the collaborative workspace (shared with peers).
pub fn is_shared(path: &Path) -> bool {
    let root = normalise_lexically(&workspace_root());
    let path = normalise_lexically(path);
    // An empty/degenerate root must never make every path look "shared".
    !root.as_os_str().is_empty() && path.starts_with(&root)
}

/// Resolve `.` and `..` without consulting the filesystem. This keeps the
/// capability check useful for not-yet-created workspace paths while ensuring
/// `/workspace/../private.epub` is never classified as shared.
fn normalise_lexically(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A stable, pleasant cover tint derived from the content id, so a book without
/// cover art always gets the same generated spine colour.
pub(crate) fn tint_for(id: &str) -> usize {
    id.bytes().fold(0usize, |a, b| a.wrapping_add(b as usize)) % 5
}

/// Parse an EPUB without touching the library. The potentially expensive
/// file read/hash/parse runs in the one bounded worker; the shelf row is the
/// UI thread's action.
///
/// # Errors
///
/// Returns a concise reason when the path cannot be read within the EPUB byte
/// budget or the EPUB container/content cannot be parsed safely.
pub fn parse_path_detailed(path: &Path) -> Result<Book, String> {
    let bytes = read_epub_bytes(path)
        .ok_or_else(|| "The file couldn’t be read, or it’s larger than 128 MiB.".to_string())?;
    parse_epub(&bytes, 0, "").map_err(|error| error.to_string())
}

/// Parse an EPUB and place its bytes in the collaborative workspace before it
/// is admitted to the shelf.  Opening a book is consequently collaborative by
/// default: the CRDT library row and the content-addressed file become
/// available to peers together, rather than leaving a remote peer with only a
/// title and annotations it cannot resolve against any text. If a host has no
/// writable workspace mount, it remains usable as a clearly local import.
///
/// The source is read once, under the normal EPUB byte limit.  This is used by
/// the background importer, so the potentially expensive copy never runs on
/// the UI thread.  Already-shared files are parsed in place.
///
/// # Errors
///
/// Returns a reader-safe message when the source cannot be read or parsed.
pub fn parse_shared_path_detailed(path: &Path) -> Result<(Book, PathBuf), String> {
    let bytes = read_epub_bytes(path)
        .ok_or_else(|| "The file couldn’t be read, or it’s larger than 128 MiB.".to_string())?;
    // Never place a corrupt EPUB in the shared collection. Parsing before the
    // copy also means a malformed file has no externally visible side effect.
    let book = parse_epub(&bytes, 0, "").map_err(|error| error.to_string())?;
    let destination = if is_shared(path) {
        path.to_path_buf()
    } else {
        match write_shared_copy(&bytes, &content_id(&bytes)) {
            Ok(destination) => destination,
            Err(error) => {
                // A workspace mount is host capability, not a precondition for
                // reading. Preserve the local fallback while making every
                // normal import collaborative whenever that capability exists.
                tracing::warn!(%error, path = %path.display(), "ebook: shared import unavailable; keeping local copy");
                path.to_path_buf()
            }
        }
    };
    Ok((book, destination))
}

/// A book a host handed over as bytes ([`ui::application::OpenFile`]),
/// parsed and kept where the library finds it again.
pub struct HandedBook {
    pub book: Book,
    /// The bytes' full content hash when a content store now holds them.
    pub content: Option<String>,
    /// How many bytes the book is.
    pub size: u64,
}

/// Parse a handed-over EPUB under the usual byte and archive bounds, then
/// keep its bytes in `store` so every mount of the library (and a reload in
/// the same session) can read the book by the hash its library row names.
pub fn open_handed_book(
    bytes: &[u8],
    store: Option<&tilo_content::ContentStore>,
) -> Result<HandedBook, String> {
    let mut book = parse_epub(bytes, 0, "").map_err(|error| error.to_string())?;
    book.cover_tint = tint_for(&book.id);
    let content = store
        .map(|store| store.put(bytes))
        .transpose()
        .map_err(|error| format!("the book could not be kept: {error}"))?;
    Ok(HandedBook {
        book,
        content,
        size: byte_count(bytes),
    })
}

/// A book ready to join the shelf: parsed, its bytes where the shelf finds
/// them again, and the row it joins with.
pub struct Placed {
    pub book: Book,
    pub entry: LibraryEntry,
    pub path: PathBuf,
}

/// Place a book a person chose by its contents where the library reads it
/// again: on the shared workspace when this device has one, so peers read
/// it too (collaborative by default, as an import is), else in `store`.
pub fn place_chosen_book(
    bytes: &[u8],
    store: Option<&tilo_content::ContentStore>,
) -> Result<Placed, String> {
    let book = parse_epub(bytes, 0, "").map_err(|error| error.to_string())?;
    if let Ok(path) = write_shared_copy(bytes, &book.id) {
        let (book, entry) = prepare_imported_book(&path, book);
        return Ok(Placed { book, entry, path });
    }
    let handed = HandedBook {
        book,
        content: store
            .map(|store| store.put(bytes))
            .transpose()
            .map_err(|error| format!("the book could not be kept: {error}"))?,
        size: byte_count(bytes),
    };
    Ok(placed(handed))
}

/// A handed-over book as it joins the shelf.
pub fn placed(mut handed: HandedBook) -> Placed {
    handed.book.cover_tint = tint_for(&handed.book.id);
    let entry = handed_book_entry(&handed);
    let path = handed
        .content
        .as_deref()
        .map_or_else(|| PathBuf::from(BUNDLED_PATH), content_path);
    Placed {
        book: handed.book,
        entry,
        path,
    }
}

fn byte_count(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

pub fn handed_book_entry(handed: &HandedBook) -> LibraryEntry {
    LibraryEntry {
        id: handed.book.id.clone(),
        shared: false,
        title: handed.book.title.clone(),
        author: handed.book.author.clone(),
        subject: handed.book.subject.clone(),
        added: unix_now_ms(),
        content: handed
            .content
            .as_deref()
            .and_then(|hash| LibraryEntry::stored(hash, handed.size))
            .unwrap_or_default(),
        index: ui::tilo_resource::Bytes::Empty,
    }
}

/// A recorded book whose bytes a content store holds, verified against both
/// the hash the row names and the row's content id.
fn content_book(entry: &LibraryEntry, store: &tilo_content::ContentStore) -> Option<LoadedBook> {
    let hash = entry.content_hash()?;
    let bytes = store.read(&hash)?;
    let mut book = parse_epub(&bytes, 0, "EPUB").ok()?;
    if book.id != entry.id {
        tracing::warn!(expected = %entry.id, actual = %book.id, "ebook: refused content whose id does not match its library row");
        return None;
    }
    book.cover_tint = tint_for(&book.id);
    Some(LoadedBook {
        book,
        path: content_path(&hash),
    })
}

/// A parsed book and the shelf row it joins with: the row is the library's
/// action, taken by whoever shelves it.
pub fn prepare_imported_book(path: &Path, mut book: Book) -> (Book, LibraryEntry) {
    book.cover_tint = tint_for(&book.id);
    let entry = LibraryEntry {
        id: book.id.clone(),
        shared: is_shared(path),
        title: book.title.clone(),
        author: book.author.clone(),
        subject: book.subject.clone(),
        added: unix_now_ms(),
        content: ui::tilo_resource::Bytes::Empty,
        index: ui::tilo_resource::Bytes::Empty,
    };
    (book, entry)
}

/// Materialise a content-addressed EPUB object in the shared workspace.
/// Never overwrites an extant object: a conflicting file is reported instead
/// of risking another reader's bytes.
fn write_shared_copy(bytes: &[u8], id: &str) -> Result<PathBuf, String> {
    let dir = workspace_books_dir();
    let file_id = id.strip_prefix("book:").unwrap_or(id);
    let file_name = format!("book-{file_id}.epub");
    let dest = dir.join(&file_name);
    let shelf = tilo_files::Dir::open(tilo_files::Location::Native(dir))
        .map_err(|_| "the shared Books folder could not be opened".to_string())?;
    let name = tilo_files::VPath::new(&file_name)
        .map_err(|_| "the book has no valid shared name".to_string())?;
    // Publish only a complete, synced object, and never over an existing
    // name: a peer must not observe partial bytes under a valid name.
    match tilo_files::blocking::create_new(&shelf, &name, bytes) {
        Ok(true) => {}
        Ok(false) => {
            // A peer may have uploaded the same content first. Reuse that
            // exact object, but never replace a different/corrupt file.
            let existing = read_epub_bytes(&dest)
                .ok_or_else(|| "the existing shared book could not be read".to_string())?;
            if content_id(&existing) != id {
                tracing::warn!(path = %dest.display(), "ebook: workspace name collision");
                return Err("a different book already occupies this shared address".into());
            }
        }
        Err(_) => return Err("the book could not be published to the shared library".into()),
    }
    Ok(dest)
}

/// A book found in a folder the reader watches, where its file is, and the
/// shelf row it joins with.
pub struct FoundBook {
    pub path: PathBuf,
    pub entry: LibraryEntry,
}

/// The books in the folders the reader watches (and any `EBOOK_LIBRARY_DIR`
/// override), each placed on the shared workspace where there is one, as
/// picker imports are: collaborative by default, with an honest local
/// fallback. Unreadable files are skipped.
pub fn find_watched_books(watched_folders: &[PathBuf]) -> Vec<FoundBook> {
    scan_watched(watched_folders)
        .into_iter()
        .filter_map(|path| {
            let (book, path) = parse_shared_path_detailed(&path).ok()?;
            let (_, entry) = prepare_imported_book(&path, book);
            Some(FoundBook { path, entry })
        })
        .collect()
}

/// The shelf's books whose bytes this device can read: a shared workspace
/// copy, a local file the reader opened (`local_books`), the content store,
/// or the book compiled into the app. A row whose bytes are nowhere here
/// (an as-yet-unhydrated workspace book) is skipped, and shows once they
/// are.
pub fn hydrate_library<S: BuildHasher>(
    entries: &[LibraryEntry],
    local_books: &HashMap<String, PathBuf, S>,
    content: Option<&tilo_content::ContentStore>,
) -> Vec<LoadedBook> {
    // Prefer a shared copy over a local one when the same content is
    // recorded twice, so the shelf shows it as shared.
    let mut out: Vec<LoadedBook> = Vec::new();
    for entry in entries {
        let canonical_shared = workspace_book_path(&entry.id);
        let path = if canonical_shared.is_file() {
            Some(canonical_shared)
        } else if !entry.shared {
            local_books
                .get(&entry.id)
                .cloned()
                .filter(|path| path.is_file())
        } else {
            None
        };
        let Some(path) = path else {
            if let Some(loaded) = content.and_then(|store| content_book(entry, store)) {
                out.push(loaded);
                if out.len() >= MAX_LIBRARY {
                    break;
                }
                continue;
            }
            // A row naming a book compiled into the app stays readable on a
            // device that holds no copy, such as the browser.
            if let Some(mut book) = bundled_book(&entry.id) {
                book.cover_tint = tint_for(&book.id);
                out.push(LoadedBook {
                    book,
                    path: PathBuf::from(BUNDLED_PATH),
                });
                if out.len() >= MAX_LIBRARY {
                    break;
                }
            }
            continue;
        };
        let Some(mut book) = open_epub_path(&path) else {
            continue;
        };
        if book.id != entry.id {
            tracing::warn!(
                expected = %entry.id,
                actual = %book.id,
                path = %path.display(),
                "ebook: refused library object whose bytes do not match its content address"
            );
            continue;
        }
        book.cover_tint = tint_for(&book.id);
        out.push(LoadedBook { book, path });
        if out.len() >= MAX_LIBRARY {
            break;
        }
    }
    out
}

/// Resolve the only replicated location hint: a content-addressed workspace
/// object. The root is local configuration; the document stores neither it nor
/// any other raw path.
/// Deterministically resolve a replicated book id to its local workspace
/// object. The id is the only cross-device locator; no peer path is ever
/// accepted as authority.
pub fn workspace_book_path(id: &str) -> PathBuf {
    let file_id = id.strip_prefix("book:").unwrap_or(id);
    workspace_books_dir().join(format!("book-{file_id}.epub"))
}

/// Open a single `.epub` file by path — the entry point used when the OS hands
/// the reader a book to open (file association / "Open with"), and when
/// hydrating a recorded library entry.
pub fn open_epub_path(path: &Path) -> Option<Book> {
    let bytes = read_epub_bytes(path)?;
    match parse_epub(&bytes, 0, "EPUB") {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "ebook: failed to open epub");
            None
        }
    }
}

/// Read one EPUB, refusing one over [`MAX_EPUB_BYTES`] without loading it.
fn read_epub_bytes(path: &Path) -> Option<Vec<u8>> {
    let (dir, name) = tilo_files::file_at(path).ok()?;
    let bytes = tilo_files::blocking::read_bounded(&dir, &name, MAX_EPUB_BYTES).ok()?;
    if bytes.is_none() {
        tracing::warn!(path = %path.display(), "ebook: file exceeds size limit");
    }
    bytes
}

/// The `.epub` files under the reader's watched folders (and any
/// `EBOOK_LIBRARY_DIR` override). Used both by the launch load and by the
/// on-focus rescan, which imports only the ones not already on the shelf.
pub fn watched_epub_paths(watched_folders: &[PathBuf]) -> Vec<PathBuf> {
    scan_watched(watched_folders)
}

/// The watched folders to scan this launch: the reader's explicit set plus any
/// `EBOOK_LIBRARY_DIR` override, walked shallowly for `.epub` files.
fn scan_watched(watched_folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots = watched_folders.to_vec();
    if let Some(v) = std::env::var_os(LIBRARY_DIR_ENV) {
        roots.extend(std::env::split_paths(&v).filter(|path| path.is_absolute()));
    }
    let mut paths = BTreeSet::new();
    for root in roots {
        collect_epubs(&root, 2, &mut paths);
    }
    paths.into_iter().collect()
}

/// Recursively (bounded `depth`) collect `*.epub` file paths under `dir`.
fn collect_epubs(dir: &Path, depth: usize, out: &mut BTreeSet<PathBuf>) {
    let Ok(entries) = tilo_files::Dir::open(tilo_files::Location::Native(dir.to_path_buf()))
        .and_then(|folder| tilo_files::blocking::list(&folder, &tilo_files::VPath::root()))
    else {
        return;
    };
    for entry in entries {
        let path = dir.join(&entry.name);
        if entry.kind == tilo_files::Kind::Dir {
            if depth > 0 {
                collect_epubs(&path, depth - 1, out);
            }
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("epub"))
        {
            out.insert(path);
            if out.len() > MAX_LIBRARY {
                out.pop_last();
            }
        }
    }
}

/// Parse the bundled sample EPUB. `None` only if the embedded asset ever fails
/// to parse (a build-time invariant, not a runtime condition). Used only by the
/// `--shots`/test sample library, never on the live shelf.
pub fn bundled_sample() -> Option<Book> {
    match parse_epub(SAMPLE_EPUB, 5, "Fable") {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::error!(error = %e, "ebook: failed to parse bundled epub");
            None
        }
    }
}

/// The content id of the book compiled into the app.
pub fn bundled_book_id() -> String {
    content_id(SAMPLE_EPUB)
}

/// The book compiled into the app whose content id is `id`, if any.
pub fn bundled_book(id: &str) -> Option<Book> {
    if bundled_book_id() != id {
        return None;
    }
    parse_epub(SAMPLE_EPUB, 0, "EPUB").ok()
}

/// The shelf row of the bundled book. Only for a host with no files and no
/// picker (the browser), where it is the one book there is to read; the
/// native shelf starts empty.
pub fn bundled_book_entry() -> Option<LibraryEntry> {
    let book = bundled_book(&bundled_book_id())?;
    Some(prepare_imported_book(Path::new(BUNDLED_PATH), book).1)
}

/// The multi-book sample library used by the headless renderer and tests. Never
/// shown to a real user; it exists so screenshots have a populated shelf.
pub fn load_sample_library() -> Vec<Book> {
    let mut books = Vec::new();
    if let Some(b) = bundled_sample() {
        books.push(b);
    }
    books.push(authored(
        "book:field-notes",
        "Field Notes on Quiet Machines",
        "Pell Marchetti",
        7,
        "Essays",
        FIELD_NOTES,
    ));
    books.push(authored(
        "book:tideglass",
        "The Tideglass Letters",
        "Oda Fenwick",
        9,
        "Correspondence",
        TIDEGLASS,
    ));
    books
}

/// Lower a list of (title, xhtml-body) chapters into a Book.
fn authored(
    id: &str,
    title: &str,
    author: &str,
    tint: usize,
    subject: &str,
    chapters: &[(&str, &str)],
) -> Book {
    let chapters = chapters
        .iter()
        .map(|(t, body)| Chapter {
            title: t.to_string(),
            blocks: parse_xhtml(body),
        })
        .collect();
    Book {
        id: id.to_string(),
        title: title.to_string(),
        author: author.to_string(),
        language: "en-US".to_string(),
        chapters,
        cover_tint: tint,
        subject: subject.to_string(),
        cover: None,
    }
}

const FIELD_NOTES: &[(&str, &str)] = &[
    (
        "On Switches",
        r"<body>
<h1>On Switches</h1>
<p>The most honest object in any house is the light switch. It makes a single promise and keeps it, and it asks for nothing in return but the smallest motion of a finger. There is a whole philosophy folded into that click, if you care to unfold it.</p>
<p>Consider what a switch does <em>not</em> do. It does not explain itself. It does not update. It does not, in the small hours, decide that what you really wanted was a different room entirely. It simply is <strong>on</strong>, or it is <strong>off</strong>, and the distance between those two states is the width of a thought.</p>
<blockquote><p>We have spent a century teaching machines to guess. Perhaps the next century's luxury will be a machine that simply does as it is told.</p></blockquote>
<p>I keep a drawer of old switches. Bakelite, brass, porcelain. Each one outlived the wiring it was sold with, and most outlived the houses. They are small monuments to the idea that a thing can be finished — that design can arrive somewhere and decide to stay.</p>
</body>",
    ),
    (
        "On Waiting",
        r"<body>
<h1>On Waiting</h1>
<p>A kettle is a teacher of patience that never raises its voice until the very end. You set it going and you are, for ninety seconds, returned to a slower century — there is nothing to do but wait, and waiting, it turns out, is a skill we have been quietly letting rust.</p>
<ul>
<li>The kettle does not ask you to rate your experience.</li>
<li>It does not suggest three other beverages you might enjoy.</li>
<li>It boils. It clicks off. It waits for you to come back.</li>
</ul>
<p>I have come to believe that the measure of a good machine is the quality of the waiting it asks of you. The bad ones fill the wait with noise and lights and small anxious demands. The good ones hand the time back to you, intact, and let you decide what it is for.</p>
</body>",
    ),
    (
        "On Repair",
        r"<body>
<h1>On Repair</h1>
<p>To repair a thing is to argue, gently, with its ending. You take the broken machine to the bench, and you open it, and inside you find a small museum of decisions made by people you will never meet — and you add one of your own.</p>
<p>There is a particular silence in a workshop at dusk, when the soldering iron has cooled and the thing on the bench is, against all odds, working again. It is the silence of a sentence that has found its full stop. <em>There.</em> Done, for now, and done well.</p>
<blockquote><p>Nothing is finished while someone is still willing to mend it.</p></blockquote>
</body>",
    ),
];

const TIDEGLASS: &[(&str, &str)] = &[
    (
        "First Letter",
        r"<body>
<h1>First Letter</h1>
<p>Dear M—,</p>
<p>The glassworks took me on this morning, which is to say a man with three fingers and a voice like a closing door looked at my hands for a long moment and then handed me an apron. I am, apparently, to learn the tide-glass: the trick of pouring colour into a pane so that it shifts as the day turns, blue at the flood and amber at the ebb.</p>
<p>He says it takes ten years to learn and a lifetime to forgive. I told him I had the lifetime to spare. He did not laugh, but the door of his voice opened, very slightly, and I think that is the local equivalent.</p>
<p>Write soon. Tell me the river is still where I left it.</p>
<p>— O.</p>
</body>",
    ),
    (
        "Second Letter",
        r"<body>
<h1>Second Letter</h1>
<p>Dear M—,</p>
<p>I have ruined eleven panes. The master counts them each evening with the patience of a man counting waves, and each evening he says the same thing: <em>the glass remembers how you held it.</em> I did not understand this until today, when the twelfth pane came out of the annealer whole and humming, and I realised I had stopped fighting it somewhere around the ninth.</p>
<blockquote><p>You cannot force colour to stay. You can only make a place calm enough that it chooses to.</p></blockquote>
<p>The tide was at the half when I held it to the window, and the blue and the amber met in the middle of the pane like two people deciding, at last, to be kind to one another.</p>
<p>— O.</p>
</body>",
    ),
];

#[cfg(test)]
mod tests {
    //! Discovery + import run against real temp directories. The tests that set
    //! the process-global `EBOOK_LIBRARY_DIR` / `TILO_WORKSPACE_DIR` env vars do
    //! so through `tilo_test_env::ScopedEnv`, which restores the prior values
    //! when dropped.

    use super::*;
    use serial_test::serial;

    type Library = ui::tilo_resource::Replica<crate::resource::Ebook>;

    /// Read, place and shelve the EPUB at `path` as the reader's importer
    /// does, returning the parsed book.
    fn shelve_path(doc: &mut Library, path: &Path) -> Book {
        let (book, path) = parse_shared_path_detailed(path).expect("a real epub imports");
        let (book, entry) = prepare_imported_book(&path, book);
        doc.apply(crate::resource::shelve(&entry)).unwrap();
        book
    }

    fn shelf(doc: &Library) -> Vec<LibraryEntry> {
        doc.state().unwrap().entries.values().cloned().collect()
    }

    fn workspace_env(path: &Path) -> tilo_test_env::ScopedEnv {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(WORKSPACE_DIR_ENV, path);
        env
    }

    /// The bundled sample and real files parse; junk is rejected and detailed imports keep the parser reason.
    #[test]
    fn epub_sources_parse_or_report_a_plain_reason() {
        // --- phase: bundled_sample_parses_into_a_real_book ---
        {
            let b = bundled_sample().expect("the embedded starter epub parses");
            assert!(!b.title.is_empty(), "has a title");
            assert!(!b.chapters.is_empty(), "has chapters");
            assert!(b.block_count() > 0, "has readable content");
        }

        // --- phase: open_epub_path_reads_a_real_file_and_rejects_junk ---
        {
            let tmp = tempfile::tempdir().unwrap();
            let good = tmp.path().join("book.epub");
            std::fs::write(&good, SAMPLE_EPUB).unwrap();
            assert!(open_epub_path(&good).is_some(), "a real epub opens by path");

            let bad = tmp.path().join("bad.epub");
            std::fs::write(&bad, b"definitely not a zip").unwrap();
            assert!(
                open_epub_path(&bad).is_none(),
                "a corrupt file is rejected, not panicked on"
            );

            let missing = tmp.path().join("gone.epub");
            assert!(
                open_epub_path(&missing).is_none(),
                "a missing file is rejected"
            );
        }

        // --- phase: detailed_import_keeps_a_plain_reason_for_the_visible_error_state ---
        {
            let tmp = tempfile::tempdir().unwrap();
            let bad = tmp.path().join("damaged.epub");
            std::fs::write(&bad, b"definitely not a zip").unwrap();

            let error = parse_path_detailed(&bad).expect_err("a corrupt EPUB does not import");
            assert!(
                error == crate::epub::OpenProblem::Damaged.to_string(),
                "the UI receives the specific plain-language damaged-container reason: {error}"
            );
        }
    }

    /// A fresh library is empty; paths are recorded natively, imported once, rehydrated after restart and auto-imported from watched folders.
    #[test]
    #[serial]
    fn local_library_imports_and_watches_paths() {
        // --- phase: a_fresh_library_is_empty ---
        {
            // No recorded books and nothing watched → a genuinely empty shelf. The
            // app never fabricates a starter book.
            let tmp = tempfile::tempdir().unwrap();
            let mut library_env = tilo_test_env::ScopedEnv::new();
            library_env.set(LIBRARY_DIR_ENV, tmp.path());
            let found = find_watched_books(&[]);
            drop(library_env);
            assert!(found.is_empty(), "no epubs anywhere -> nothing to shelve");
            assert!(hydrate_library(&[], &HashMap::new(), None).is_empty());
        }

        // --- phase: watched_path_list_preserves_native_paths ---
        {
            let first = tempfile::tempdir().unwrap();
            let second = tempfile::tempdir().unwrap();
            let a = first.path().join("Bücher eins.epub");
            let b = second.path().join("second.epub");
            std::fs::write(&a, SAMPLE_EPUB).unwrap();
            std::fs::write(&b, SAMPLE_EPUB).unwrap();
            let mut library_env = tilo_test_env::ScopedEnv::new();
            library_env.set(
                LIBRARY_DIR_ENV,
                std::env::join_paths([first.path(), second.path()]).unwrap(),
            );
            let paths = watched_epub_paths(&[]);
            drop(library_env);
            assert_eq!(paths.len(), 2);
            assert!(paths.contains(&a));
            assert!(paths.contains(&b));
        }

        // --- phase: importing_a_path_adds_one_recorded_book ---
        {
            let workspace = tempfile::tempdir().unwrap();
            let _workspace = workspace_env(workspace.path());
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("book.epub");
            std::fs::write(&path, SAMPLE_EPUB).unwrap();
            let mut library_env = tilo_test_env::ScopedEnv::new();
            library_env.set(LIBRARY_DIR_ENV, tmp.path());
            let mut doc = Library::new();

            let book = shelve_path(&mut doc, &path);
            assert!(
                book.id.starts_with("book:"),
                "the import produced a content id"
            );
            assert_eq!(shelf(&doc).len(), 1);
            let lib = hydrate_library(&shelf(&doc), &HashMap::new(), None);
            drop(library_env);
            assert_eq!(lib.len(), 1, "the imported book is on the shelf");
            assert_eq!(lib[0].book.id, book.id);
            assert_eq!(lib[0].path, workspace_book_path(&book.id));
            let bytes = doc.save();
            let stored = String::from_utf8_lossy(&bytes);
            assert!(
                !stored.contains(path.to_string_lossy().as_ref()),
                "the local source path stays outside shared state"
            );

            // Importing the very same file again does not duplicate the shelf entry.
            shelve_path(&mut doc, &path);
            assert_eq!(shelf(&doc).len(), 1, "same file → one entry");
        }

        // --- phase: device_local_source_rehydrates_a_one_off_import_after_restart ---
        {
            // A regular file cannot contain a `Books` directory, deterministically
            // modelling a host without the collaborative-workspace capability.
            let workspace = tempfile::NamedTempFile::new().unwrap();
            let _workspace = workspace_env(workspace.path());
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("one-off.epub");
            std::fs::write(&path, SAMPLE_EPUB).unwrap();
            let mut doc = Library::new();
            let book = shelve_path(&mut doc, &path);
            let mut local_books = HashMap::new();
            local_books.insert(book.id.clone(), path.clone());

            let restored = hydrate_library(&shelf(&doc), &local_books, None);
            assert_eq!(restored.len(), 1);
            assert_eq!(restored[0].book.id, book.id);
            assert_eq!(restored[0].path, path);
        }

        // --- phase: a_watched_folder_auto_imports_new_books ---
        {
            let watched = tempfile::tempdir().unwrap();
            std::fs::write(watched.path().join("alpha.epub"), SAMPLE_EPUB).unwrap();
            let sub = watched.path().join("nested");
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join("beta.EPUB"), SAMPLE_EPUB).unwrap();
            std::fs::write(watched.path().join("readme.txt"), b"not a book").unwrap();

            let found = find_watched_books(&[watched.path().to_path_buf()]);
            assert_eq!(found.len(), 2, "both epubs found, the .txt ignored");
            // Both epubs are the *same bytes*, so they share a content id and collapse
            // to a single library entry.
            let mut doc = Library::new();
            let mut local_books = HashMap::new();
            for book in &found {
                doc.apply(crate::resource::shelve_found(&book.entry))
                    .unwrap();
                local_books.insert(book.entry.id.clone(), book.path.clone());
            }
            let lib = hydrate_library(&shelf(&doc), &local_books, None);
            assert_eq!(
                lib.len(),
                1,
                "watched epubs shelved once (deduped by content)"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    #[serial]
    fn unconfigured_windows_workspace_keeps_import_local() {
        let _workspace = workspace_env(Path::new(""));
        let local = tempfile::tempdir().unwrap();
        let path = local.path().join("local.epub");
        std::fs::write(&path, SAMPLE_EPUB).unwrap();
        let (_, destination) = parse_shared_path_detailed(&path).unwrap();
        assert_eq!(destination, path);
        assert!(!is_shared(&destination));
    }

    /// Workspace books are shared, shared parses materialise first, and sharing keeps identity without overwriting unrelated files.
    #[test]
    #[serial]
    fn workspace_sharing_keeps_identity_and_never_overwrites() {
        // --- phase: a_book_on_the_workspace_is_shared_others_are_local ---
        {
            let ws = tempfile::tempdir().unwrap();
            let _workspace = workspace_env(ws.path());
            let books_dir = workspace_books_dir();
            std::fs::create_dir_all(&books_dir).unwrap();
            let shared = books_dir.join("shared.epub");
            std::fs::write(&shared, SAMPLE_EPUB).unwrap();

            let local = tempfile::tempdir().unwrap();
            let local_path = local.path().join("local.epub");

            assert!(is_shared(&shared), "a /workspace/Books path is shared");
            assert!(!is_shared(&local_path), "a book elsewhere is device-local");
            assert!(
                !is_shared(&workspace_root().join("Books/../../private/book.epub")),
                "lexical traversal cannot spoof workspace membership"
            );
        }

        // --- phase: shared_parse_materialises_content_before_the_library_row_is_written ---
        {
            let ws = tempfile::tempdir().unwrap();
            let _workspace = workspace_env(ws.path());
            let local = tempfile::tempdir().unwrap();
            let source = local.path().join("peer-readable.epub");
            std::fs::write(&source, SAMPLE_EPUB).unwrap();

            let (book, destination) = parse_shared_path_detailed(&source).unwrap();
            assert!(is_shared(&destination));
            assert!(destination.is_file());
            assert_eq!(book.id, content_id(SAMPLE_EPUB));

            // The parser/materialiser owns no CRDT authority; the UI worker is
            // free to discard a superseded result without leaving a ghost shelf
            // row behind.
            let mut doc = Library::new();
            let (recorded, entry) = prepare_imported_book(&destination, book);
            doc.apply(crate::resource::shelve(&entry)).unwrap();
            let entries = shelf(&doc);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, recorded.id);
            assert!(entries[0].shared);
            assert!(
                std::fs::read_dir(workspace_books_dir())
                    .unwrap()
                    .flatten()
                    .all(|entry| entry.path().extension().is_none_or(|ext| ext != "part")),
                "atomic publication removes its private staging file"
            );
        }

        // --- phase: sharing_copies_the_book_onto_the_workspace_keeping_its_identity ---
        {
            let ws = tempfile::tempdir().unwrap();
            let _workspace = workspace_env(ws.path());
            let home = tempfile::tempdir().unwrap();
            let src = home.path().join("mine.epub");
            std::fs::write(&src, SAMPLE_EPUB).unwrap();

            let before = parse_path_detailed(&src).unwrap();
            assert!(!is_shared(&src));

            let (after, dest) = parse_shared_path_detailed(&src).expect("share succeeds");
            assert!(is_shared(&dest), "the copy lands on the workspace");
            assert!(dest.exists(), "the file was actually copied");
            assert_eq!(
                after.id, before.id,
                "identity (and thus annotations/progress) is preserved"
            );
            // The library records canonical shared availability, not a workspace
            // root that might mean something different on another device.
            let mut doc = Library::new();
            doc.apply(crate::resource::shelve(
                &prepare_imported_book(&dest, after).1,
            ))
            .unwrap();
            let entries = shelf(&doc);
            assert_eq!(entries.len(), 1);
            assert!(entries[0].shared, "the shared copy is discoverable by id");
        }

        // --- phase: sharing_never_overwrites_an_unrelated_same_named_workspace_file ---
        {
            let ws = tempfile::tempdir().unwrap();
            let _workspace = workspace_env(ws.path());
            let home = tempfile::tempdir().unwrap();
            let src = home.path().join("book.epub");
            std::fs::write(&src, SAMPLE_EPUB).unwrap();

            let books = workspace_books_dir();
            std::fs::create_dir_all(&books).unwrap();
            // This is the name older versions of sharing would have used. It must
            // survive, even though its contents are not an EPUB at all.
            let old_destination = books.join("book.epub");
            std::fs::write(&old_destination, b"another reader's file").unwrap();

            let (_book, destination) = parse_shared_path_detailed(&src).expect("share succeeds");
            assert_ne!(
                destination, old_destination,
                "share uses a content-addressed name"
            );
            assert_eq!(
                std::fs::read(&old_destination).unwrap(),
                b"another reader's file",
                "an unrelated same-named file was not overwritten"
            );
            assert!(
                destination.exists(),
                "the shared content was written separately"
            );
        }
    }

    #[test]
    #[serial]
    fn a_bundled_book_on_the_shelf_hydrates_without_any_file() {
        // A regular file cannot contain a `Books` directory: no workspace copy.
        let workspace = tempfile::NamedTempFile::new().unwrap();
        let _workspace = workspace_env(workspace.path());
        let mut doc = Library::new();
        let entry = bundled_book_entry().expect("the bundled book has a shelf row");
        doc.apply(crate::resource::shelve(&entry)).unwrap();
        let id = entry.id;
        assert_eq!(id, content_id(SAMPLE_EPUB));
        let entries = shelf(&doc);
        assert_eq!(entries.len(), 1);
        assert!(
            !entries[0].shared,
            "the bundled copy is nobody's shared file"
        );

        let library = hydrate_library(&entries, &HashMap::new(), None);
        assert_eq!(library.len(), 1);
        assert_eq!(library[0].book.id, id);
        assert_eq!(library[0].book.title, "The Lantern-Keeper's Almanac");
        assert!(bundled_book("book:0000").is_none());
    }

    #[test]
    #[serial]
    fn a_handed_book_is_read_back_from_the_content_store_its_row_names() {
        let workspace = tempfile::NamedTempFile::new().unwrap();
        let _workspace = workspace_env(workspace.path());
        let bytes = crate::test_epub::long_classic(3, 2);
        let store = tilo_content::ContentStore::memory();
        let handed = open_handed_book(&bytes, Some(&store)).unwrap();
        let hash = tilo_content::hash_bytes(&bytes);
        assert_eq!(handed.content.as_deref(), Some(hash.as_str()));
        assert_eq!(store.read(&hash).as_deref(), Some(bytes.as_slice()));

        let mut doc = Library::new();
        doc.apply(crate::resource::shelve(&handed_book_entry(&handed)))
            .unwrap();
        let entries = shelf(&doc);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content_hash().as_deref(), Some(hash.as_str()));
        assert!(!entries[0].shared, "a handed book is nobody's shared file");

        let library = hydrate_library(&entries, &HashMap::new(), Some(&store));
        assert_eq!(library.len(), 1);
        assert_eq!(library[0].book.id, handed.book.id);
        assert_eq!(library[0].book.chapters.len(), 3);
        // A device without the bytes keeps the row but has nothing to show.
        let elsewhere = tilo_content::ContentStore::memory();
        assert!(hydrate_library(&entries, &HashMap::new(), Some(&elsewhere)).is_empty());
        assert_eq!(shelf(&doc).len(), 1);

        // Content that is not the row's book is never shown as it.
        let other = crate::test_epub::long_classic(2, 1);
        let other_hash = store.put(&other).unwrap();
        let mut entry = handed_book_entry(&handed);
        entry.content = LibraryEntry::stored(&other_hash, 1).unwrap();
        assert!(hydrate_library(&[entry.clone()], &HashMap::new(), Some(&store)).is_empty());
        assert!(
            Library::new()
                .apply(crate::resource::shelve(&entry))
                .is_err(),
            "the library refuses a hash that does not name its book"
        );
        // A failed open says why in a plain sentence, not the parser's words.
        assert_eq!(
            open_handed_book(b"not a zip", Some(&store))
                .err()
                .as_deref(),
            Some("The file is damaged or isn’t an EPUB book.")
        );
    }
}
