//! The library: the reader's own collection of books.
//!
//! The shelf is app-managed and persisted, not re-derived from a magic
//! folder every launch. A book enters the library when the reader opens it
//! or when it turns up in a watched folder; from then on it lives in the
//! CRDT ([`crate::doc`]) keyed by its content hash, so the shelf survives a
//! restart and follows the reader across devices. [`load_library`] hydrates
//! recorded books from disk each launch and auto-imports anything new.
//!
//! The bundled EPUB + the multi-book sample library are reserved for the
//! hermetic `--shots`/test path.

use std::path::{Path, PathBuf};

use automerge::AutoCommit;

use crate::doc::BooksDoc;
use crate::epub::parse_epub;
use crate::model::{Book, Chapter, LibraryEntry, now_ms};
use crate::xhtml::parse_xhtml;

/// The bundled EPUB, parsed by the real container reader. Used only by the
/// `--shots`/test sample library and as test-fixture epub bytes — never seeded
/// onto the live shelf.
pub(crate) const SAMPLE_EPUB: &[u8] = include_bytes!("../assets/sample.epub");

/// Environment override (`:`-separated absolute paths) for extra folders to
/// watch. Lets the OS point the reader at a synced library (e.g. a
/// `/var/workspace` share) and lets tests pin a fixture directory.
const LIBRARY_DIR_ENV: &str = "EBOOK_LIBRARY_DIR";

/// Override for the collaborative-workspace mount root (tests point it at a temp
/// directory); production is the real qOS FUSE mount.
const WORKSPACE_DIR_ENV: &str = "QOS_WORKSPACE_DIR";
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

/// Whether a path lives on the collaborative workspace (shared with peers).
pub fn is_shared(path: &Path) -> bool {
    let root = workspace_root();
    // An empty/degenerate root must never make every path look "shared".
    !root.as_os_str().is_empty() && path.starts_with(&root)
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
    let bytes = std::fs::read(path).ok()?;
    let mut book = match parse_epub(&bytes, 0, "") {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "ebook: import skipped");
            return None;
        }
    };
    book.cover_tint = tint_for(&book.id);
    BooksDoc::upsert_library_entry(
        doc,
        &LibraryEntry {
            id: book.id.clone(),
            path: path.to_path_buf(),
            title: book.title.clone(),
            author: book.author.clone(),
            subject: book.subject.clone(),
            added: now_ms(),
        },
    );
    Some(book)
}

/// Copy a local book into the workspace so the reader's peers get it, record the
/// new (shared) location, and return the re-hydrated book + its workspace path.
/// A book already on the workspace is returned unchanged.
pub fn share_to_workspace(doc: &mut AutoCommit, src: &Path) -> Option<(Book, PathBuf)> {
    if is_shared(src) {
        return None;
    }
    let dir = workspace_books_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let name = src.file_name()?;
    let dest = dir.join(name);
    // The content is identical, so the book keeps its id (and all its
    // annotations + progress) — only its home changes from local to shared.
    std::fs::copy(src, &dest).ok()?;
    let book = import_path(doc, &dest)?;
    Some((book, dest))
}

/// Load the reader's library: auto-import any new books found in a watched
/// folder, then hydrate every recorded book from disk. A recorded book whose
/// file can't be read right now (e.g. an as-yet-unhydrated workspace book) is
/// skipped from the shelf but kept in the registry, so it reappears once its
/// bytes are available.
pub fn load_library(doc: &mut AutoCommit) -> Vec<LoadedBook> {
    // 1. Pull in anything new from the folders the reader watches (and the env
    //    override the OS/tests use to point at a synced library).
    let watched = scan_watched(doc);
    for path in watched {
        import_path(doc, &path);
    }

    // 2. Hydrate the recorded library. Prefer a shared copy over a local one when
    //    the same content is recorded twice, so the shelf shows it as shared.
    let mut out: Vec<LoadedBook> = Vec::new();
    for entry in BooksDoc::read_library_entries(doc) {
        let Some(mut book) = open_epub_path(&entry.path) else {
            continue;
        };
        book.cover_tint = tint_for(&book.id);
        out.push(LoadedBook {
            book,
            path: entry.path,
        });
        if out.len() >= MAX_LIBRARY {
            break;
        }
    }
    out
}

/// Open a single `.epub` file by path — the entry point used when the OS hands
/// the reader a book to open (file association / "Open with"), and when
/// hydrating a recorded library entry.
pub fn open_epub_path(path: &Path) -> Option<Book> {
    let bytes = std::fs::read(path).ok()?;
    match parse_epub(&bytes, 0, "EPUB") {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "ebook: failed to open epub");
            None
        }
    }
}

/// The `.epub` files under the reader's watched folders (and any
/// `EBOOK_LIBRARY_DIR` override). Used both by the launch load and by the
/// on-focus rescan, which imports only the ones not already on the shelf.
pub fn watched_epub_paths(doc: &AutoCommit) -> Vec<PathBuf> {
    scan_watched(doc)
}

/// The watched folders to scan this launch: the reader's explicit set plus any
/// `EBOOK_LIBRARY_DIR` override, walked shallowly for `.epub` files.
fn scan_watched(doc: &AutoCommit) -> Vec<PathBuf> {
    let mut roots = BooksDoc::read_watched_folders(doc);
    if let Ok(v) = std::env::var(LIBRARY_DIR_ENV) {
        roots.extend(v.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    for root in roots {
        collect_epubs(&root, 2, &mut paths);
        if paths.len() >= MAX_LIBRARY {
            break;
        }
    }
    paths.sort();
    paths.dedup();
    paths.truncate(MAX_LIBRARY);
    paths
}

/// Recursively (bounded `depth`) collect `*.epub` file paths under `dir`.
fn collect_epubs(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if out.len() >= MAX_LIBRARY {
        return;
    }
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
            out.push(path);
        }
        if out.len() >= MAX_LIBRARY {
            return;
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
    //! the process-global `EBOOK_LIBRARY_DIR` / `QOS_WORKSPACE_DIR` env vars are
    //! `#[serial]` so cargo's default parallel runner can't race them.

    use super::*;
    use serial_test::serial;

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
        unsafe { std::env::set_var(LIBRARY_DIR_ENV, tmp.path()) };
        let mut doc = AutoCommit::new();
        let lib = load_library(&mut doc);
        unsafe { std::env::remove_var(LIBRARY_DIR_ENV) };
        assert!(lib.is_empty(), "no epubs anywhere -> an empty shelf");
    }

    #[test]
    #[serial]
    fn importing_a_path_adds_one_recorded_book() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("book.epub");
        std::fs::write(&path, SAMPLE_EPUB).unwrap();
        let mut doc = AutoCommit::new();

        let book = import_path(&mut doc, &path).expect("a real epub imports");
        assert!(
            book.id.starts_with("book:"),
            "the import produced a content id"
        );
        // Recorded once, and hydrating the library yields exactly that book.
        assert_eq!(BooksDoc::read_library_entries(&doc).len(), 1);
        let lib = load_library(&mut doc);
        assert_eq!(lib.len(), 1, "the imported book is on the shelf");
        assert_eq!(lib[0].book.id, book.id);
        assert_eq!(lib[0].path, path);

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
    fn a_watched_folder_auto_imports_new_books() {
        let watched = tempfile::tempdir().unwrap();
        std::fs::write(watched.path().join("alpha.epub"), SAMPLE_EPUB).unwrap();
        let sub = watched.path().join("nested");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("beta.EPUB"), SAMPLE_EPUB).unwrap();
        std::fs::write(watched.path().join("readme.txt"), b"not a book").unwrap();

        let mut doc = AutoCommit::new();
        BooksDoc::write_watched_folders(&mut doc, &[watched.path().to_path_buf()]);
        let lib = load_library(&mut doc);
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
        unsafe { std::env::set_var(WORKSPACE_DIR_ENV, ws.path()) };
        let books_dir = workspace_books_dir();
        std::fs::create_dir_all(&books_dir).unwrap();
        let shared = books_dir.join("shared.epub");
        std::fs::write(&shared, SAMPLE_EPUB).unwrap();

        let local = tempfile::tempdir().unwrap();
        let local_path = local.path().join("local.epub");

        assert!(is_shared(&shared), "a /workspace/Books path is shared");
        assert!(!is_shared(&local_path), "a book elsewhere is device-local");
        unsafe { std::env::remove_var(WORKSPACE_DIR_ENV) };
    }

    #[test]
    #[serial]
    fn sharing_copies_the_book_onto_the_workspace_keeping_its_identity() {
        let ws = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var(WORKSPACE_DIR_ENV, ws.path()) };
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
        // The library now points at the shared copy — one entry, shared home.
        let entries = BooksDoc::read_library_entries(&doc);
        assert_eq!(entries.len(), 1);
        assert!(
            is_shared(&entries[0].path),
            "the recorded home is now the workspace"
        );
        unsafe { std::env::remove_var(WORKSPACE_DIR_ENV) };
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
}
