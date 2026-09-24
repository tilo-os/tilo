//! The library: the reader's own collection of books.
//!
//! The shelf is app-managed and persisted, not re-derived from a magic
//! folder every launch. A book enters the library when the reader opens it
//! or when it turns up in a watched folder; from then on its identity and
//! public metadata live in the CRDT ([`crate::doc`]). Source paths and folder
//! grants remain local. Shared copies resolve from their content-addressed
//! workspace filename; local copies are rediscovered only through explicitly
//! supplied watch roots. [`load_library`] hydrates those available books and
//! auto-imports anything new.
//!
//! The bundled EPUB + the multi-book sample library are reserved for the
//! hermetic `--shots`/test path.

use std::collections::{BTreeSet, HashMap};
use std::hash::BuildHasher;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use automerge::AutoCommit;
use tilo_actions::Action;

use crate::doc::BooksDoc;
use crate::epub::{MAX_EPUB_BYTES, content_id, parse_epub};
use crate::model::{Book, Chapter, LibraryEntry, now_ms};
use crate::xhtml::parse_xhtml;

/// The bundled EPUB, parsed by the real container reader. Used only by the
/// `--shots`/test sample library and as test-fixture epub bytes — never seeded
/// onto the live shelf.
pub(crate) const SAMPLE_EPUB: &[u8] = include_bytes!("../assets/sample.epub");

/// Environment override (platform-separated absolute paths: `;` on Windows,
/// `:` on Unix) for extra folders to
/// watch. Lets the OS point the reader at a synced library (e.g. a
/// `/var/workspace` share) and lets tests pin a fixture directory.
const LIBRARY_DIR_ENV: &str = "EBOOK_LIBRARY_DIR";

/// Override for the collaborative-workspace mount root (tests point it at a temp
/// directory); production is the real Tilo FUSE mount.
const WORKSPACE_DIR_ENV: &str = "TILO_WORKSPACE_DIR";
const DEFAULT_WORKSPACE: &str = "/var/workspace";

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
/// workspace's peers; a book anywhere else is local to this device.
pub fn workspace_root() -> PathBuf {
    std::env::var(WORKSPACE_DIR_ENV)
        .map_or_else(|_| PathBuf::from(DEFAULT_WORKSPACE), PathBuf::from)
}

/// Where a book copied into the workspace lands.
pub fn workspace_books_dir() -> PathBuf {
    workspace_root().join("Books")
}

fn workspace_configured() -> bool {
    #[cfg(windows)]
    {
        std::env::var_os(WORKSPACE_DIR_ENV).is_some_and(|root| PathBuf::from(root).is_absolute())
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Whether a path lives on the collaborative workspace (shared with peers).
pub fn is_shared(path: &Path) -> bool {
    if !workspace_configured() {
        return false;
    }
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
fn tint_for(id: &str) -> usize {
    id.bytes().fold(0usize, |a, b| a.wrapping_add(b as usize)) % 5
}

/// Parse the `.epub` at `path` and record it in the library, returning the
/// parsed book. The entry is keyed by the book's content hash, so importing the
/// same file again (from any path) updates one entry rather than duplicating it.
pub fn import_path(doc: &mut AutoCommit, path: &Path) -> Option<Book> {
    match import_path_detailed(doc, path) {
        Ok(book) => Some(book),
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "ebook: import skipped");
            None
        }
    }
}

/// Like [`import_path`], but retain a reader-safe reason for a visible error
/// state. The UI uses this for a picker or file-association request so a
/// rejected oversized/corrupt book is never reported as a vague empty shelf.
///
/// # Errors
///
/// Returns a concise reason when the path cannot be read within the EPUB byte
/// budget or the EPUB container/content cannot be parsed safely.
pub fn import_path_detailed(doc: &mut AutoCommit, path: &Path) -> Result<Book, String> {
    let book = parse_path_detailed(path)?;
    Ok(record_imported_book(doc, path, book))
}

/// Parse an EPUB without mutating the shared document. This is deliberately
/// separate from [`import_path_detailed`] so the potentially expensive file
/// read/hash/parse can run in the one bounded worker while CRDT writes remain
/// on the UI thread.
///
/// # Errors
///
/// Returns a concise reason when the path cannot be read within the EPUB byte
/// budget or the EPUB container/content cannot be parsed safely.
pub fn parse_path_detailed(path: &Path) -> Result<Book, String> {
    let bytes = read_epub_bytes(path)
        .ok_or_else(|| "the file could not be read or exceeds the 128 MiB limit".to_string())?;
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
        .ok_or_else(|| "the file could not be read or exceeds the 128 MiB limit".to_string())?;
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

/// Commit a previously parsed book to the library CRDT. Call this only on the
/// UI/document owner after [`parse_path_detailed`] has completed.
pub fn record_imported_book(doc: &mut AutoCommit, path: &Path, book: Book) -> Book {
    let (book, entry) = prepare_imported_book(path, book);
    let _ = crate::actions::restore_library_book_action(&entry).apply(doc);
    book
}

/// Build the canonical shelf action input while keeping the actual durable
/// commit with the app's [`ui::ActionHost`]. Startup migration uses
/// [`record_imported_book`]; interactive import/share paths use this helper so
/// their history boundary is observable by the shell and agents.
pub fn prepare_imported_book(path: &Path, mut book: Book) -> (Book, LibraryEntry) {
    book.cover_tint = tint_for(&book.id);
    let entry = LibraryEntry {
        id: book.id.clone(),
        shared: is_shared(path),
        title: book.title.clone(),
        author: book.author.clone(),
        subject: book.subject.clone(),
        added: now_ms(),
    };
    (book, entry)
}

/// Copy a local book into the workspace so the reader's peers get it, record the
/// new (shared) location, and return the re-hydrated book + its workspace path.
/// A book already on the workspace is returned unchanged.
pub fn share_to_workspace(doc: &mut AutoCommit, src: &Path) -> Option<(Book, PathBuf)> {
    if is_shared(src) {
        return None;
    }
    let (book, dest) = parse_shared_path_detailed(src).ok()?;
    // `parse_shared_path_detailed` intentionally falls back to local-only
    // when a host exposes no workspace mount. The explicit Share control must
    // not report that fallback as a successful share.
    if !is_shared(&dest) {
        return None;
    }
    let book = record_imported_book(doc, &dest, book);
    Some((book, dest))
}

/// Materialise a content-addressed EPUB object in the shared workspace.
/// Never overwrites an extant object: a conflicting file is reported instead
/// of risking another reader's bytes.
fn write_shared_copy(bytes: &[u8], id: &str) -> Result<PathBuf, String> {
    if !workspace_configured() {
        return Err("No collaborative workspace is configured on this device".into());
    }
    let dir = workspace_books_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|_| "the shared Books folder could not be created".to_string())?;
    let file_id = id.strip_prefix("book:").unwrap_or(id);
    let dest = dir.join(format!("book-{file_id}.epub"));
    // Publish only a complete, flushed object. A direct write to `dest` lets a
    // peer observe a valid name containing partial bytes while an import is in
    // flight. A same-directory hard link is an atomic no-overwrite publish.
    let mut temporary = None;
    for attempt in 0..32 {
        let candidate = dir.join(format!(
            ".book-{file_id}-{}-{attempt}.part",
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| {
                        "the book could not be copied to the shared library".to_string()
                    })?;
                temporary = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("the book could not be copied to the shared library".into()),
        }
    }
    let temporary = temporary
        .ok_or_else(|| "the book could not reserve a shared-library upload".to_string())?;
    let publish = std::fs::hard_link(&temporary, &dest);
    let _ = std::fs::remove_file(&temporary);
    match publish {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
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

/// Load the reader's library: auto-import any new books found in a watched
/// folder, then hydrate every recorded book from disk. A recorded book whose
/// file can't be read right now (e.g. an as-yet-unhydrated workspace book) is
/// skipped from the shelf but kept in the registry, so it reappears once its
/// bytes are available.
pub fn load_library<S: BuildHasher>(
    doc: &mut AutoCommit,
    watched_folders: &[PathBuf],
    local_books: &HashMap<String, PathBuf, S>,
) -> Vec<LoadedBook> {
    // A path used to be replicated alongside this metadata. Remove it before
    // doing any local discovery: source locations are capabilities, not
    // collaborative data.
    BooksDoc::strip_legacy_library_paths(doc);
    // 1. Pull in anything new from the folders the reader watches (and the env
    //    override the OS/tests use to point at a synced library).
    let watched = scan_watched(watched_folders);
    let mut discovered = HashMap::new();
    for path in watched {
        // Startup discovery follows the same collaborative-by-default policy
        // as picker imports.  The synchronous loader predates the worker, but
        // it still records the materialised workspace path (or the honest
        // local fallback) rather than publishing metadata for a private file.
        if let Ok((book, resolved_path)) = parse_shared_path_detailed(&path) {
            let book = record_imported_book(doc, &resolved_path, book);
            discovered.insert(book.id, resolved_path);
        }
    }

    // 2. Hydrate the recorded library. Prefer a shared copy over a local one when
    //    the same content is recorded twice, so the shelf shows it as shared.
    let mut out: Vec<LoadedBook> = Vec::new();
    for entry in BooksDoc::read_library_entries(doc) {
        let canonical_shared = workspace_book_path(&entry.id);
        let path = if workspace_configured() && canonical_shared.is_file() {
            canonical_shared
        } else if !entry.shared {
            let Some(path) = local_books
                .get(&entry.id)
                .cloned()
                .or_else(|| discovered.remove(&entry.id))
                .filter(|path| path.is_file())
            else {
                continue;
            };
            path
        } else {
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

/// Read one EPUB without trusting its advertised filesystem size. The metadata
/// check gives a cheap refusal for ordinary huge files; `take` is the actual
/// guard if the file grows between that check and the read.
fn read_epub_bytes(path: &Path) -> Option<Vec<u8>> {
    if std::fs::metadata(path).ok()?.len() > MAX_EPUB_BYTES as u64 {
        tracing::warn!(path = %path.display(), "ebook: file exceeds size limit");
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_EPUB_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_EPUB_BYTES {
        tracing::warn!(path = %path.display(), "ebook: file grew beyond size limit");
        return None;
    }
    Some(bytes)
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
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
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

// --- Sample library: hermetic `--shots` / test content only -----------------

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
        "Iris House",
        7,
        "Essays",
        FIELD_NOTES,
    ));
    books.push(authored(
        "book:tideglass",
        "The Tideglass Letters",
        "Oda Fenwick",
        "Marrow & Vane",
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
    publisher: &str,
    tint: usize,
    subject: &str,
    chapters: &[(&str, &str)],
) -> Book {
    let chapters = chapters
        .iter()
        .map(|(t, body)| Chapter {
            title: t.to_string(),
            href: format!("{t}.xhtml"),
            blocks: parse_xhtml(body),
        })
        .collect();
    Book {
        id: id.to_string(),
        title: title.to_string(),
        author: author.to_string(),
        publisher: publisher.to_string(),
        language: "en-US".to_string(),
        description: String::new(),
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

    fn workspace_env(path: &Path) -> tilo_test_env::ScopedEnv {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(WORKSPACE_DIR_ENV, path);
        env
    }

    #[test]
    fn bundled_sample_parses_into_a_real_book() {
        let b = bundled_sample().expect("the embedded starter epub parses");
        assert!(!b.title.is_empty(), "has a title");
        assert!(!b.chapters.is_empty(), "has chapters");
        assert!(b.block_count() > 0, "has readable content");
    }

    #[test]
    #[serial]
    fn a_fresh_library_is_empty() {
        // No recorded books and nothing watched → a genuinely empty shelf. The
        // app never fabricates a starter book.
        let tmp = tempfile::tempdir().unwrap();
        let mut library_env = tilo_test_env::ScopedEnv::new();
        library_env.set(LIBRARY_DIR_ENV, tmp.path());
        let mut doc = AutoCommit::new();
        let lib = load_library(&mut doc, &[], &HashMap::new());
        drop(library_env);
        assert!(lib.is_empty(), "no epubs anywhere -> an empty shelf");
    }

    #[test]
    #[serial]
    fn watched_path_list_preserves_native_paths() {
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
        assert!(share_to_workspace(&mut AutoCommit::new(), &path).is_none());
    }

    #[test]
    #[serial]
    fn importing_a_path_adds_one_recorded_book() {
        let workspace = tempfile::tempdir().unwrap();
        let _workspace = workspace_env(workspace.path());
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("book.epub");
        std::fs::write(&path, SAMPLE_EPUB).unwrap();
        let mut library_env = tilo_test_env::ScopedEnv::new();
        library_env.set(LIBRARY_DIR_ENV, tmp.path());
        let mut doc = AutoCommit::new();

        let book = import_path(&mut doc, &path).expect("a real epub imports");
        assert!(
            book.id.starts_with("book:"),
            "the import produced a content id"
        );
        // Recorded once, and hydrating the library yields exactly that book.
        assert_eq!(BooksDoc::read_library_entries(&doc).len(), 1);
        let lib = load_library(&mut doc, &[], &HashMap::new());
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
        import_path(&mut doc, &path).unwrap();
        assert_eq!(
            BooksDoc::read_library_entries(&doc).len(),
            1,
            "same file → one entry"
        );
    }

    #[test]
    #[serial]
    fn device_local_source_rehydrates_a_one_off_import_after_restart() {
        // A regular file cannot contain a `Books` directory, deterministically
        // modelling a host without the collaborative-workspace capability.
        let workspace = tempfile::NamedTempFile::new().unwrap();
        let _workspace = workspace_env(workspace.path());
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("one-off.epub");
        std::fs::write(&path, SAMPLE_EPUB).unwrap();
        let mut doc = AutoCommit::new();
        let book = import_path(&mut doc, &path).unwrap();
        let mut local_books = HashMap::new();
        local_books.insert(book.id.clone(), path.clone());

        let restored = load_library(&mut doc, &[], &local_books);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].book.id, book.id);
        assert_eq!(restored[0].path, path);
    }

    #[test]
    #[serial]
    fn a_watched_folder_auto_imports_new_books() {
        let watched = tempfile::tempdir().unwrap();
        std::fs::write(watched.path().join("alpha.epub"), SAMPLE_EPUB).unwrap();
        let sub = watched.path().join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("beta.EPUB"), SAMPLE_EPUB).unwrap();
        std::fs::write(watched.path().join("readme.txt"), b"not a book").unwrap();

        let mut doc = AutoCommit::new();
        let lib = load_library(&mut doc, &[watched.path().to_path_buf()], &HashMap::new());
        // Both epubs are the *same bytes*, so they share a content id and collapse
        // to a single library entry — the non-epub is ignored.
        assert_eq!(
            lib.len(),
            1,
            "watched epubs imported (deduped by content), .txt ignored"
        );
    }

    #[test]
    #[serial]
    fn a_book_on_the_workspace_is_shared_others_are_local() {
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

    #[test]
    #[serial]
    fn shared_parse_materialises_content_before_the_library_row_is_written() {
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
        let mut doc = AutoCommit::new();
        let recorded = record_imported_book(&mut doc, &destination, book);
        let entries = BooksDoc::read_library_entries(&doc);
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

    #[test]
    #[serial]
    fn sharing_copies_the_book_onto_the_workspace_keeping_its_identity() {
        let ws = tempfile::tempdir().unwrap();
        let _workspace = workspace_env(ws.path());
        let home = tempfile::tempdir().unwrap();
        let src = home.path().join("mine.epub");
        std::fs::write(&src, SAMPLE_EPUB).unwrap();

        let mut doc = AutoCommit::new();
        let before = import_path(&mut doc, &src).unwrap();
        assert!(!is_shared(&src));

        let (after, dest) = share_to_workspace(&mut doc, &src).expect("share succeeds");
        assert!(is_shared(&dest), "the copy lands on the workspace");
        assert!(dest.exists(), "the file was actually copied");
        assert_eq!(
            after.id, before.id,
            "identity (and thus annotations/progress) is preserved"
        );
        // The library records canonical shared availability, not a workspace
        // root that might mean something different on another device.
        let entries = BooksDoc::read_library_entries(&doc);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].shared, "the shared copy is discoverable by id");
    }

    #[test]
    #[serial]
    fn sharing_never_overwrites_an_unrelated_same_named_workspace_file() {
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

        let mut doc = AutoCommit::new();
        let (_book, destination) = share_to_workspace(&mut doc, &src).expect("share succeeds");
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

    #[test]
    fn open_epub_path_reads_a_real_file_and_rejects_junk() {
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

    #[test]
    fn detailed_import_keeps_the_parser_reason_for_the_visible_error_state() {
        let tmp = tempfile::tempdir().unwrap();
        let bad = tmp.path().join("damaged.epub");
        std::fs::write(&bad, b"definitely not a zip").unwrap();

        let error = import_path_detailed(&mut AutoCommit::new(), &bad)
            .expect_err("a corrupt EPUB does not import");
        assert!(
            error.contains("open epub zip"),
            "the UI receives the actual parse reason, not a generic failure: {error}"
        );
    }
}
