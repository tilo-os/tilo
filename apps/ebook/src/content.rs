//! Library sourcing.
//!
//! At runtime the shelf is the reader's *own* books: every `.epub` found under
//! their library directories is parsed by the real container reader. When no
//! books are present yet the shelf is genuinely empty — the app opens onto the
//! "no books yet" onboarding rather than a book the reader never added.
//!
//! The bundled EPUB + the multi-book **sample** library (bundled EPUB + two
//! authored volumes) are reserved for the hermetic `--shots`/test path, so
//! screenshots exercise a populated shelf without fabricating content into a
//! real user's library.

use std::path::{Path, PathBuf};

use crate::epub::parse_epub;
use crate::model::{Book, Chapter};
use crate::xhtml::parse_xhtml;

/// The bundled EPUB, parsed by the real container reader. Used only by the
/// `--shots`/test sample library and as test-fixture epub bytes — never seeded
/// onto the live shelf.
pub(crate) const SAMPLE_EPUB: &[u8] = include_bytes!("../assets/sample.epub");

/// Environment override (`:`-separated absolute paths) for the directories
/// scanned for `.epub` files. Lets the OS point the reader at a synced library
/// (e.g. a `/var/workspace` share) and lets tests pin a fixture directory.
const LIBRARY_DIR_ENV: &str = "EBOOK_LIBRARY_DIR";

/// The runtime library: the reader's real `.epub` files, discovered on disk.
/// Returns an empty shelf when none are found — the app must never present a
/// bundled sample book as if the user owned it. The empty case is handled by
/// the library view's "no books yet" onboarding.
pub fn load_library() -> Vec<Book> {
    discover_epubs(&library_roots())
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

/// Open a single `.epub` file by path — the entry point used when the OS hands
/// the reader a book to open (file association / "Open with").
pub fn open_epub_path(path: &Path) -> Option<Book> {
    let bytes = std::fs::read(path).ok()?;
    match parse_epub(&bytes, 5, "EPUB") {
        Ok(b) => Some(b),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "ebook: failed to open epub");
            None
        }
    }
}

/// The directories scanned for the user's books. The `EBOOK_LIBRARY_DIR`
/// override wins outright; otherwise a small set of conventional locations
/// (including the qOS P2P workspace) is probed.
fn library_roots() -> Vec<PathBuf> {
    if let Ok(v) = std::env::var(LIBRARY_DIR_ENV) {
        return v
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
    }
    let mut roots = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join("Books"));
        roots.push(home.join("Documents"));
        roots.push(home.join("Downloads"));
    }
    roots.push(PathBuf::from("/var/workspace"));
    roots.push(PathBuf::from("/var/workspace/Books"));
    roots
}

/// Walk each root (shallow — two levels) collecting `.epub` files, parse them,
/// and return the books sorted by title. Unreadable files are skipped.
fn discover_epubs(roots: &[PathBuf]) -> Vec<Book> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for root in roots {
        collect_epubs(root, 2, &mut paths);
        if paths.len() >= MAX_LIBRARY {
            break;
        }
    }
    paths.sort();
    paths.dedup();
    paths.truncate(MAX_LIBRARY);

    let mut books: Vec<Book> = Vec::new();
    for (i, path) in paths.iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else { continue };
        match parse_epub(&bytes, i, "EPUB") {
            Ok(b) => books.push(b),
            Err(e) => tracing::warn!(error = %e, path = %path.display(), "ebook: skipping epub"),
        }
    }
    books.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    books
}

/// Upper bound on discovered books, so a pathological directory can't stall
/// startup parsing thousands of files.
const MAX_LIBRARY: usize = 80;

/// Recursively (bounded `depth`) collect `*.epub` file paths under `dir`.
fn collect_epubs(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if out.len() >= MAX_LIBRARY {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
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

// ===========================================================================
// Sample library — hermetic `--shots` / test content only.
// ===========================================================================

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
            href: format!("{}.xhtml", t),
            blocks: parse_xhtml(body),
        })
        .collect();
    Book {
        id: id.to_string(),
        title: title.to_string(),
        author: author.to_string(),
        publisher: publisher.to_string(),
        description: String::new(),
        chapters,
        cover_tint: tint,
        subject: subject.to_string(),
        cover: None,
    }
}

const FIELD_NOTES: &[(&str, &str)] = &[
    ("On Switches", r#"<body>
<h1>On Switches</h1>
<p>The most honest object in any house is the light switch. It makes a single promise and keeps it, and it asks for nothing in return but the smallest motion of a finger. There is a whole philosophy folded into that click, if you care to unfold it.</p>
<p>Consider what a switch does <em>not</em> do. It does not explain itself. It does not update. It does not, in the small hours, decide that what you really wanted was a different room entirely. It simply is <strong>on</strong>, or it is <strong>off</strong>, and the distance between those two states is the width of a thought.</p>
<blockquote><p>We have spent a century teaching machines to guess. Perhaps the next century's luxury will be a machine that simply does as it is told.</p></blockquote>
<p>I keep a drawer of old switches. Bakelite, brass, porcelain. Each one outlived the wiring it was sold with, and most outlived the houses. They are small monuments to the idea that a thing can be finished — that design can arrive somewhere and decide to stay.</p>
</body>"#),
    ("On Waiting", r#"<body>
<h1>On Waiting</h1>
<p>A kettle is a teacher of patience that never raises its voice until the very end. You set it going and you are, for ninety seconds, returned to a slower century — there is nothing to do but wait, and waiting, it turns out, is a skill we have been quietly letting rust.</p>
<ul>
<li>The kettle does not ask you to rate your experience.</li>
<li>It does not suggest three other beverages you might enjoy.</li>
<li>It boils. It clicks off. It waits for you to come back.</li>
</ul>
<p>I have come to believe that the measure of a good machine is the quality of the waiting it asks of you. The bad ones fill the wait with noise and lights and small anxious demands. The good ones hand the time back to you, intact, and let you decide what it is for.</p>
</body>"#),
    ("On Repair", r#"<body>
<h1>On Repair</h1>
<p>To repair a thing is to argue, gently, with its ending. You take the broken machine to the bench, and you open it, and inside you find a small museum of decisions made by people you will never meet — and you add one of your own.</p>
<p>There is a particular silence in a workshop at dusk, when the soldering iron has cooled and the thing on the bench is, against all odds, working again. It is the silence of a sentence that has found its full stop. <em>There.</em> Done, for now, and done well.</p>
<blockquote><p>Nothing is finished while someone is still willing to mend it.</p></blockquote>
</body>"#),
];

const TIDEGLASS: &[(&str, &str)] = &[
    ("First Letter", r#"<body>
<h1>First Letter</h1>
<p>Dear M—,</p>
<p>The glassworks took me on this morning, which is to say a man with three fingers and a voice like a closing door looked at my hands for a long moment and then handed me an apron. I am, apparently, to learn the tide-glass: the trick of pouring colour into a pane so that it shifts as the day turns, blue at the flood and amber at the ebb.</p>
<p>He says it takes ten years to learn and a lifetime to forgive. I told him I had the lifetime to spare. He did not laugh, but the door of his voice opened, very slightly, and I think that is the local equivalent.</p>
<p>Write soon. Tell me the river is still where I left it.</p>
<p>— O.</p>
</body>"#),
    ("Second Letter", r#"<body>
<h1>Second Letter</h1>
<p>Dear M—,</p>
<p>I have ruined eleven panes. The master counts them each evening with the patience of a man counting waves, and each evening he says the same thing: <em>the glass remembers how you held it.</em> I did not understand this until today, when the twelfth pane came out of the annealer whole and humming, and I realised I had stopped fighting it somewhere around the ninth.</p>
<blockquote><p>You cannot force colour to stay. You can only make a place calm enough that it chooses to.</p></blockquote>
<p>The tide was at the half when I held it to the window, and the blue and the amber met in the middle of the pane like two people deciding, at last, to be kind to one another.</p>
<p>— O.</p>
</body>"#),
];

#[cfg(test)]
mod tests {
    //! Discovery is exercised against a real temp directory via the
    //! `EBOOK_LIBRARY_DIR` override. The env-var tests are `#[serial]` because
    //! the variable is process-global.

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
    fn empty_library_dir_yields_an_empty_shelf() {
        // The live shelf must never fabricate a book: with no epubs on disk the
        // library is genuinely empty and the app shows its onboarding state.
        let tmp = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var(LIBRARY_DIR_ENV, tmp.path()) };
        let lib = load_library();
        unsafe { std::env::remove_var(LIBRARY_DIR_ENV) };
        assert!(lib.is_empty(), "no epubs on disk -> an empty shelf, no bundled book");
    }

    #[test]
    #[serial]
    fn discovers_epubs_recursively_and_ignores_non_epubs() {
        let tmp = tempfile::tempdir().unwrap();
        // Two real epubs, one nested one level down; plus a decoy non-epub.
        std::fs::write(tmp.path().join("alpha.epub"), SAMPLE_EPUB).unwrap();
        let sub = tmp.path().join("shelf");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("beta.EPUB"), SAMPLE_EPUB).unwrap();
        std::fs::write(tmp.path().join("readme.txt"), b"not a book").unwrap();

        unsafe { std::env::set_var(LIBRARY_DIR_ENV, tmp.path()) };
        let lib = load_library();
        unsafe { std::env::remove_var(LIBRARY_DIR_ENV) };

        // Both epubs found (case-insensitive extension, shallow recursion); the
        // .txt is skipped; and since epubs were found there is no starter added.
        assert_eq!(lib.len(), 2, "both epubs discovered, non-epub ignored");
    }

    #[test]
    #[serial]
    fn multiple_roots_are_all_scanned() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        std::fs::write(a.path().join("one.epub"), SAMPLE_EPUB).unwrap();
        std::fs::write(b.path().join("two.epub"), SAMPLE_EPUB).unwrap();
        let joined = format!("{}:{}", a.path().display(), b.path().display());
        unsafe { std::env::set_var(LIBRARY_DIR_ENV, &joined) };
        let lib = load_library();
        unsafe { std::env::remove_var(LIBRARY_DIR_ENV) };
        assert_eq!(lib.len(), 2, "both colon-separated roots scanned");
    }

    #[test]
    fn open_epub_path_reads_a_real_file_and_rejects_junk() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("book.epub");
        std::fs::write(&good, SAMPLE_EPUB).unwrap();
        assert!(open_epub_path(&good).is_some(), "a real epub opens by path");

        let bad = tmp.path().join("bad.epub");
        std::fs::write(&bad, b"definitely not a zip").unwrap();
        assert!(open_epub_path(&bad).is_none(), "a corrupt file is rejected, not panicked on");

        let missing = tmp.path().join("gone.epub");
        assert!(open_epub_path(&missing).is_none(), "a missing file is rejected");
    }
}
