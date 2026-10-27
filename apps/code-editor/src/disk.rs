//! Real filesystem I/O for the editor: reading a file into a buffer, writing a
//! buffer back atomically, and noticing when a file changed underneath us.
//!
//! Two rules shape this module.
//!
//! **Writes are atomic.** A save goes through [`tilo_files::native::atomic`],
//! which writes a sibling temp file, fsyncs it, and renames it over the
//! target, keeping an existing file's permission bits and symbolic link. A
//! crash or a full disk therefore leaves either the old file or the new one,
//! never a half-written source file. An editor that can truncate your code on a
//! bad day is worse than one that cannot save at all.
//!
//! **A save never silently clobbers.** Every buffer carries the [`Stamp`] its
//! bytes were read at. [`save`] re-stats the file first and refuses with
//! [`SaveError::Conflict`] when the stamp moved — the file was rewritten by
//! `git checkout`, a formatter, or another editor since we read it. The app
//! turns that into a choice (overwrite / reload / cancel) rather than deciding
//! for the user.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use ui::time::SystemTime;

/// Files larger than this are not opened as text buffers. The editor holds the
/// whole body in the CRDT as a character-addressed `Text`, so a hundred-megabyte
/// blob is not a slow editor, it is an unusable one.
pub const MAX_TEXT_BYTES: u64 = 24 * 1024 * 1024;

/// What we know about a file on disk at the moment we last agreed with it.
///
/// Modification time plus length is what every editor uses for this, and it is
/// the right trade: it costs one `stat`, whereas hashing costs a full read of
/// every open file on every save. `mtime` is `None` on the filesystems that do
/// not report one, in which case length alone still catches the common case.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Stamp {
    pub mtime: Option<SystemTime>,
    pub len: u64,
}

impl Stamp {
    /// Read the current stamp of `path`. A missing file has no stamp.
    pub fn of(path: &Path) -> io::Result<Stamp> {
        let md = std::fs::metadata(path)?;
        Ok(Stamp {
            mtime: md.modified().ok(),
            len: md.len(),
        })
    }
}

/// Why a load failed, in terms the user can act on.
#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    /// The file is larger than [`MAX_TEXT_BYTES`].
    TooLarge(u64),
    /// The bytes are not UTF-8 text (or contain NULs) — opening it as a text
    /// buffer would corrupt it on the next save.
    NotText,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "{e}"),
            LoadError::TooLarge(n) => write!(
                f,
                "file is {} — too large to open as text (limit {})",
                ui::format::size(*n),
                ui::format::size(MAX_TEXT_BYTES)
            ),
            LoadError::NotText => write!(f, "not a text file"),
        }
    }
}

/// Why a save failed.
#[derive(Debug)]
pub enum SaveError {
    Io(io::Error),
    /// The file changed on disk since the buffer was read. The app resolves
    /// this through overwrite/reload/cancel and re-stamps the chosen contents.
    Conflict,
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::Io(e) => write!(f, "{e}"),
            SaveError::Conflict => write!(f, "the file changed on disk since it was opened"),
        }
    }
}

/// A file's bytes as text, plus the stamp they were read at.
#[derive(Debug)]
pub struct Loaded {
    pub text: String,
    pub stamp: Stamp,
}

/// Read `path` as a text buffer, refusing what must not become one.
///
/// The size check happens against the metadata *before* the read, so opening a
/// huge file costs a `stat` rather than a full slurp into memory.
pub fn load(path: &Path) -> Result<Loaded, LoadError> {
    let md = std::fs::metadata(path).map_err(LoadError::Io)?;
    if md.len() > MAX_TEXT_BYTES {
        return Err(LoadError::TooLarge(md.len()));
    }
    let file = std::fs::File::open(path).map_err(LoadError::Io)?;
    let text = read_body(file)?;
    Ok(Loaded {
        text,
        stamp: Stamp {
            mtime: md.modified().ok(),
            len: md.len(),
        },
    })
}

/// Bound the actual read as well as stat: the file can grow between them.
fn read_body(reader: impl Read) -> Result<String, LoadError> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(LoadError::Io)?;
    if bytes.len() as u64 > MAX_TEXT_BYTES {
        return Err(LoadError::TooLarge(bytes.len() as u64));
    }
    if !looks_like_text(&bytes) {
        return Err(LoadError::NotText);
    }
    String::from_utf8(bytes).map_err(|_| LoadError::NotText)
}

/// Write `text` to `path` atomically, refusing if the file moved on underneath.
///
/// `expected` is the stamp the buffer was last in agreement with. Pass `None`
/// to write unconditionally — that is the "overwrite anyway" the user chose
/// after seeing the conflict, and the only way to reach this function without a
/// freshness check.
///
/// A file that does not exist yet cannot conflict: creating it is the save.
pub fn save(path: &Path, text: &str, expected: Option<&Stamp>) -> Result<Stamp, SaveError> {
    if let Some(expected) = expected {
        match Stamp::of(path) {
            Ok(current) if &current != expected => return Err(SaveError::Conflict),
            Ok(_) => {}
            // Not existing yet is not a conflict — the save creates it.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(SaveError::Io(e)),
        }
    }
    write(path, text.as_bytes()).map_err(SaveError::Io)?;
    Stamp::of(path).map_err(SaveError::Io)
}

/// Replace `path`'s bytes atomically. A file that exists stays the person's
/// file: a script keeps its executable bit, and a symbolic link (a dotfile
/// kept in another repository) keeps pointing where it did, with the new
/// bytes written to its target. A path that does not exist yet is created
/// with its folders.
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => tilo_files::native::atomic::write_preserving_target(path, bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            tilo_files::native::atomic::write_creating_parent(path, bytes)
        }
        Err(e) => Err(e),
    }
}

/// Whether a file changed on disk relative to `stamp`. A file that vanished
/// counts as changed — saving over it would recreate something the user may
/// have deliberately deleted.
pub fn changed_on_disk(path: &Path, stamp: &Stamp) -> bool {
    match Stamp::of(path) {
        Ok(current) => &current != stamp,
        Err(_) => true,
    }
}

/// A cheap, conventional binary sniff: a NUL byte in the first 8 KiB. Real
/// source files do not contain NUL; ELF objects, images and archives do, within
/// the first few bytes.
fn looks_like_text(bytes: &[u8]) -> bool {
    let head = bytes.get(..bytes.len().min(8192)).unwrap_or(bytes);
    !head.contains(&0)
}

/// Directory names never worth walking or listing in a source project: build
/// output and VCS internals. On this repository `target/` alone is 150+ GB, so
/// this is the difference between a project tree that opens and one that does
/// not.
pub const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".jj",
    "target",
    "node_modules",
    ".venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".cargo",
    ".direnv",
    "dist",
    ".next",
];

/// Whether a directory entry should be skipped when listing or searching a
/// project tree.
///
/// Matched on the **name**, whatever the entry's type. In a git worktree `.git`
/// is a *file* holding a gitdir pointer rather than a directory, so a
/// type-gated test lets it through and it shows up as an openable source file.
pub fn is_ignored_dir(name: &str) -> bool {
    IGNORED_DIRS.contains(&name)
}

/// The absolute, symlink-resolved form of `path`, falling back to the input
/// when it cannot be canonicalised (a path that does not exist yet).
pub fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("create tempdir")
    }

    #[test]
    fn save_and_load_round_trip_bytes_parents_and_utf8() {
        save_writes_the_bytes_and_load_reads_them_back();
        save_creates_missing_parent_directories();
        utf8_survives_a_save_load_round_trip();
    }

    fn save_writes_the_bytes_and_load_reads_them_back() {
        let d = tmpdir();
        let p = d.path().join("a.rs");
        let stamp = save(&p, "fn main() {}\n", None).expect("first save creates the file");
        // The bytes are really on disk, read back through the filesystem and
        // not through any cache of ours.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "fn main() {}\n");
        assert_eq!(stamp.len, 13);

        let loaded = load(&p).expect("load the file we just wrote");
        assert_eq!(loaded.text, "fn main() {}\n");
        assert_eq!(loaded.stamp, stamp);
    }

    fn save_creates_missing_parent_directories() {
        let d = tmpdir();
        let p = d.path().join("nested/deeper/new.rs");
        save(&p, "x", None).expect("save creates the parent chain");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "x");
    }

    #[test]
    fn stamped_saves_refuse_to_clobber_external_changes() {
        a_second_save_against_a_current_stamp_succeeds();
        a_file_changed_underneath_refuses_to_be_clobbered();
        an_explicit_overwrite_after_a_conflict_wins();
        a_deleted_file_counts_as_changed();
    }

    fn a_second_save_against_a_current_stamp_succeeds() {
        let d = tmpdir();
        let p = d.path().join("a.rs");
        let s1 = save(&p, "one", None).unwrap();
        // Change the length as well as the contents so the new stamp is
        // observably different even on filesystems whose mtimes have coarse
        // resolution (or when both atomic renames land in the same clock tick).
        let s2 = save(&p, "two!", Some(&s1)).expect("we are still in agreement with the file");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two!");
        assert_ne!(s1, s2, "the stamp advances with the content");
    }

    fn a_file_changed_underneath_refuses_to_be_clobbered() {
        let d = tmpdir();
        let p = d.path().join("a.rs");
        let stale = save(&p, "original", None).unwrap();

        // Something else rewrites the file — `git checkout`, rustfmt, a peer.
        std::fs::write(&p, "rewritten by someone else").unwrap();

        assert!(matches!(
            save(&p, "my edit", Some(&stale)),
            Err(SaveError::Conflict)
        ));
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "rewritten by someone else",
            "a refused save must not have written anything"
        );

        assert!(changed_on_disk(&p, &stale));
    }

    fn an_explicit_overwrite_after_a_conflict_wins() {
        let d = tmpdir();
        let p = d.path().join("a.rs");
        let stale = save(&p, "original", None).unwrap();
        std::fs::write(&p, "theirs").unwrap();
        assert!(matches!(
            save(&p, "mine", Some(&stale)),
            Err(SaveError::Conflict)
        ));
        // Passing no expectation is the user having said "overwrite anyway".
        save(&p, "mine", None).expect("an unconditional save always writes");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "mine");
    }

    /// Saving a script keeps it executable, and saving through a symbolic
    /// link writes the file it points at and leaves the link in place.
    #[cfg(unix)]
    #[test]
    fn a_save_keeps_the_files_permissions_and_its_symbolic_link() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = tmpdir();
        let script = d.path().join("build.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let stamp = Stamp::of(&script).unwrap();
        save(&script, "#!/bin/sh\necho built\n", Some(&stamp)).expect("the script saves");
        assert_eq!(
            std::fs::metadata(&script).unwrap().permissions().mode() & 0o7777,
            0o755,
            "a saved script stays executable"
        );

        let target = d.path().join("dotfiles/bashrc");
        std::fs::create_dir(d.path().join("dotfiles")).unwrap();
        std::fs::write(&target, "export A=1\n").unwrap();
        let link = d.path().join(".bashrc");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let stamp = load(&link).unwrap().stamp;
        let saved = save(&link, "export A=2\n", Some(&stamp)).expect("the link saves");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link is still a link"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "export A=2\n");
        assert!(
            !changed_on_disk(&link, &saved),
            "the stamp is the one the next save checks against"
        );
    }

    fn a_deleted_file_counts_as_changed() {
        let d = tmpdir();
        let p = d.path().join("a.rs");
        let stamp = save(&p, "x", None).unwrap();
        std::fs::remove_file(&p).unwrap();
        assert!(changed_on_disk(&p, &stamp));
        // Saving it back is then a fresh create, not a conflict.
        save(&p, "x", Some(&stamp)).expect("recreating a deleted file is not a conflict");
    }

    #[test]
    fn load_refuses_binary_and_oversized_files() {
        binary_files_are_refused_rather_than_opened_as_text();
        oversized_files_are_refused_before_being_read();
        too_large_message_uses_si_units();
    }

    fn binary_files_are_refused_rather_than_opened_as_text() {
        let d = tmpdir();
        let p = d.path().join("thing.bin");
        std::fs::write(&p, [0x7f, b'E', b'L', b'F', 0x00, 0x01, 0x02]).unwrap();
        assert!(matches!(load(&p), Err(LoadError::NotText)));
    }

    fn oversized_files_are_refused_before_being_read() {
        let d = tmpdir();
        let p = d.path().join("huge.log");
        let big = vec![b'a'; (MAX_TEXT_BYTES + 1) as usize];
        std::fs::write(&p, &big).unwrap();
        match load(&p) {
            Err(LoadError::TooLarge(n)) => assert_eq!(n, MAX_TEXT_BYTES + 1),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn actual_read_refuses_a_file_that_grows_after_its_metadata_was_checked() {
        let directory = tmpdir();
        let path = directory.path().join("growing.rs");
        std::fs::write(&path, "small source").unwrap();
        let reader = std::fs::File::open(&path).unwrap();
        assert!(reader.metadata().unwrap().len() < MAX_TEXT_BYTES);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_TEXT_BYTES + 1)
            .unwrap();
        assert!(
            matches!(read_body(reader), Err(LoadError::TooLarge(size)) if size == MAX_TEXT_BYTES + 1)
        );
    }

    fn utf8_survives_a_save_load_round_trip() {
        let d = tmpdir();
        let p = d.path().join("é.rs");
        let body = "// héllo wörld — 日本語\nfn main() {}\n";
        save(&p, body, None).unwrap();
        assert_eq!(load(&p).unwrap().text, body);
    }

    #[test]
    fn build_output_and_vcs_dirs_are_ignored() {
        assert!(is_ignored_dir("target"));
        assert!(is_ignored_dir(".git"));
        assert!(is_ignored_dir("node_modules"));
        assert!(!is_ignored_dir("src"));
        assert!(!is_ignored_dir("targets"));
    }

    fn too_large_message_uses_si_units() {
        let message = LoadError::TooLarge(MAX_TEXT_BYTES + 1).to_string();
        assert!(
            message.ends_with("too large to open as text (limit 25.2 MB)"),
            "{message}"
        );
    }
}
