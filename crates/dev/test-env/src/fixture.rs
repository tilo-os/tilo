//! Trees, documents and workspaces a test starts from.
//!
//! A test that opens an app on some files, or a desktop on a workspace, wrote
//! that starting point itself, and every test wrote it a little differently:
//! its own `tempfile::tempdir`, its own directory names, its own idea of where
//! an app keeps a document. Two costs follow. A temporary directory has no
//! owner here, so nothing is left to look in when a fixture leaks, and the
//! same tree is described a few hundred times over.
//!
//! A [`Fixture`] is where all of that goes: one temporary directory for the
//! test's files, [`file`](Fixture::file), [`files`](Fixture::files) and
//! [`dir`](Fixture::dir) to put them in it, [`workspace`](Fixture::workspace)
//! to seed the workspace document a desktop opens on, and
//! [`release`](Fixture::release) for a test that needs the tree to be gone
//! before the fixture drops.
//!
//! The fixture holds no environment. The directory it makes is a directory, at
//! a path the test passes around; only [`TestHome`](crate::TestHome) owns the
//! XDG variables, so a test that wants a home points the variables at a
//! fixture's directory with a [`ScopedEnv`](crate::ScopedEnv), or uses
//! [`TestHome`](crate::TestHome) and puts its files under
//! [`TestHome::root`](crate::TestHome::root).

use std::io;
use std::path::{Component, Path, PathBuf};

/// The name of the document a workspace opens with, as the workspace
/// document type stores it.
pub const WORKSPACE_DOCUMENT: &str = "workspace";

/// A temporary directory a test builds its starting point in.
///
/// Every path is relative to [`root`](Self::root), and the whole tree is
/// removed when the fixture is dropped, or at [`release`](Self::release) for
/// a test that has to see the tree gone.
pub struct Fixture {
    /// `None` once [`release`](Self::release) took the directory back.
    root: Option<tempfile::TempDir>,
}

impl Fixture {
    /// An empty fixture.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the temporary directory cannot be created.
    pub fn new() -> io::Result<Self> {
        Ok(Self::at(
            tempfile::Builder::new().prefix("tilo-fixture-").tempdir()?,
        ))
    }

    /// A fixture of the tree under the temporary directory `dir`.
    #[must_use]
    pub fn at(dir: tempfile::TempDir) -> Self {
        Self { root: Some(dir) }
    }

    /// The fixture's directory, which exists until the fixture is released.
    /// A released fixture is the empty path, which no test can hold: releasing
    /// one takes it by value.
    #[must_use]
    pub fn root(&self) -> &Path {
        // A released fixture is not reachable: `release` takes the fixture by
        // value, so no test holds one to ask.
        self.root
            .as_ref()
            .map_or_else(|| Path::new(""), tempfile::TempDir::path)
    }

    /// Create a directory below the root, with its parents.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the directory cannot be created, and
    /// [`InvalidInput`](io::ErrorKind::InvalidInput) for a path that is
    /// absolute or climbs above the root.
    pub fn dir(&self, path: impl AsRef<Path>) -> io::Result<PathBuf> {
        let path = self.resolve(path.as_ref())?;
        std::fs::create_dir_all(&path)?;
        Ok(path)
    }

    /// Write `bytes` to a file below the root, creating its parents.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the file cannot be written, and
    /// [`InvalidInput`](io::ErrorKind::InvalidInput) for a path that is
    /// absolute or climbs above the root, so a fixture never writes outside
    /// the directory it owns.
    pub fn file(&self, path: impl AsRef<Path>, bytes: impl AsRef<[u8]>) -> io::Result<PathBuf> {
        let path = self.resolve(path.as_ref())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, bytes)?;
        Ok(path)
    }

    /// Write every `(path, bytes)` pair, as a test that starts from a small
    /// tree of files lists it, in the order given.
    ///
    /// # Errors
    ///
    /// As [`file`](Self::file), for the first pair that fails.
    pub fn files<'a, I, P>(&self, files: I) -> io::Result<Vec<PathBuf>>
    where
        I: IntoIterator<Item = (P, &'a [u8])>,
        P: AsRef<Path>,
    {
        files
            .into_iter()
            .map(|(path, bytes)| self.file(path, bytes))
            .collect()
    }

    /// Write `entries` into the workspace document a desktop opens with, under
    /// [`data`](Self::data)/[`WORKSPACE_DOCUMENT`].
    ///
    /// The entries are the workspace stored as JSON, so a test seeds the
    /// document the desktop reads rather than reaching into the desktop after
    /// it started. A caller that builds them writes its own JSON object.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the document cannot be written.
    pub fn workspace(&self, entries: &str) -> io::Result<PathBuf> {
        let entries = entries.trim();
        let document =
            format!(r#"{{"type":"{WORKSPACE_DOCUMENT}","version":1,"entries":{entries}}}"#);
        self.file(
            Path::new("data").join(WORKSPACE_DOCUMENT),
            document.as_bytes(),
        )
    }

    /// The fixture's `$XDG_DATA_HOME`-shaped directory: where an app keeps
    /// its documents below a home, and where [`workspace`](Self::workspace)
    /// writes. It is created on first use.
    #[must_use]
    pub fn data(&self) -> PathBuf {
        self.root().join("data")
    }

    /// Take the directory back: removes the tree and every path into it.
    pub fn release(mut self) {
        self.root.take();
    }

    fn resolve(&self, path: &Path) -> io::Result<PathBuf> {
        let escapes = path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::RootDir));
        if escapes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{} is not below the fixture's root: a fixture writes only where it owns",
                    path.display()
                ),
            ));
        }
        Ok(self.root().join(path))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // `TempDir` removes the tree; dropping the field here says so at the
        // one place the fixture decides a tree is over.
        self.root.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario;

    #[test]
    fn a_fixture_writes_below_its_root_and_refuses_paths_outside_it() {
        // Phase: files_and_directories_are_created_below_the_root.
        {
            let fixture = Fixture::new().unwrap();
            let root = fixture.root().to_path_buf();
            let written = fixture.file("notes/today.md", "# today").unwrap();
            assert_eq!(written, root.join("notes/today.md"));
            assert_eq!(std::fs::read(&written).unwrap(), b"# today");
            let listed = fixture
                .files([("a/one", &b"1"[..]), ("b/two", &b"22"[..])])
                .unwrap();
            assert_eq!(listed, [root.join("a/one"), root.join("b/two")]);
            let dir = fixture.dir("empty").unwrap();
            assert!(dir.is_dir());
            assert_eq!(fixture.data(), root.join("data"));
        }

        // Phase: a_path_that_leaves_the_tree_is_refused.
        {
            let fixture = Fixture::new().unwrap();
            for path in ["/etc/passwd", "../outside", "a/../../outside"] {
                let error = fixture.file(path, "x").unwrap_err();
                assert_eq!(
                    error.kind(),
                    io::ErrorKind::InvalidInput,
                    "{path} is refused, not written"
                );
            }
            assert!(
                !fixture.root().parent().unwrap().join("outside").exists(),
                "nothing was written above the root"
            );
        }

        // Phase: releasing_removes_the_tree.
        {
            let fixture = Fixture::new().unwrap();
            let root = fixture.root().to_path_buf();
            fixture.file("kept.txt", "kept").unwrap();
            fixture.release();
            assert!(!root.exists(), "the tree is removed on release");
        }
    }

    #[test]
    fn a_workspace_document_is_a_document_the_desktop_opens() {
        let fixture = Fixture::new().unwrap();
        let path = fixture.workspace(r#"{"name": "Studio"}"#).unwrap();
        assert_eq!(path, fixture.data().join(WORKSPACE_DOCUMENT));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"type":"workspace","version":1,"entries":{"name": "Studio"}}"#
        );
        assert_eq!(
            std::fs::read_to_string(fixture.workspace("{}").unwrap()).unwrap(),
            r#"{"type":"workspace","version":1,"entries":{}}"#
        );
    }

    // The fixture hands out a directory and nothing else: two of them in one
    // test see two trees.
    scenario! {
        fn two_fixtures_in_one_test_own_two_trees;

        phase each_fixture_sees_only_its_own_files;
        {
            let first = Fixture::new().unwrap();
            let second = Fixture::new().unwrap();
            first.file("only-in-first", "1").unwrap();
            assert!(!second.root().join("only-in-first").exists());
            assert_ne!(first.root(), second.root());
        }
    }
}
