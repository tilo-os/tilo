//! Git's own answers on the same content, as the reference side of every
//! paired measurement.
//!
//! The reference is deliberately the operation a person already does, not a
//! microbenchmark of an equivalent library: "status", "read every tracked
//! file", "diff one checkpoint against another", "send one change to a second
//! person". Each runs in a scratch clone built from the manifest, so nothing
//! here touches the repository it was measured from, and each is written to
//! answer the same *question* as the candidate, so the pair means something.
//!
//! A reference that fails to produce its answer is not a slow reference: the
//! caller sees `correct: false` and the pair is refused by
//! [`crate::measurements::ComparisonLine::verdict`].

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::codebase::{Manifest, SourceTree};

/// The revision Git resolves to the tree `main` currently carries. Spelled
/// apart from the format strings around it so it reads as a revision rather
/// than as a placeholder.
const MAIN_TREE: &str = "main^{tree}";
use crate::measurements::Timed;

/// A scratch repository holding one commit whose tree is `manifest`'s.
///
/// Built with `git fast-import`, from the bytes the manifest already read, so
/// creating it costs no checkout of the tree it was read from.
pub struct Scratch {
    path: PathBuf,
    tree: Option<String>,
}

/// What a status run found: every path a working tree reports as changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusReport {
    pub modified: usize,
    pub added: usize,
    pub deleted: usize,
}

impl StatusReport {
    #[must_use]
    pub fn total(&self) -> usize {
        self.modified
            .saturating_add(self.added)
            .saturating_add(self.deleted)
    }
}

impl Scratch {
    /// An empty scratch repository at `path`.
    ///
    /// # Errors
    ///
    /// Returns the error when the directory or the repository cannot be made.
    pub fn create(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        std::fs::create_dir_all(&path)?;
        let scratch = Self { path, tree: None };
        scratch.git(&["init", "--quiet", "--initial-branch=main", "--bare"])?;
        Ok(scratch)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The tree id of the commit [`Self::import`] made.
    #[must_use]
    pub fn tree(&self) -> Option<&str> {
        self.tree.as_deref()
    }

    /// Write every entry of `source`'s manifest into one commit.
    ///
    /// Symlinks keep their target and submodules keep their commit, so the
    /// tree Git ends up with has the same shape the manifest describes. The
    /// scratch repository is never checked out: a checkout would be a
    /// filesystem operation measured in Git's favour and in nobody else's.
    ///
    /// # Errors
    ///
    /// Returns the error when the import stream or Git fails.
    pub fn import(&mut self, source: &SourceTree) -> std::io::Result<()> {
        let entries = &source.manifest().entries;
        let mut stream = Vec::with_capacity(256 * 1024);
        let mut marks = std::collections::BTreeMap::new();
        for entry in entries {
            let mark = marks.len().saturating_add(1);
            let mode = match entry.kind {
                crate::codebase::Kind::Directory => continue,
                crate::codebase::Kind::File { executable: true } => "100755",
                crate::codebase::Kind::Symlink => "120000",
                crate::codebase::Kind::Submodule => "160000",
                crate::codebase::Kind::File { executable: false } => "100644",
            };
            stream.extend_from_slice(format!("blob\nmark :{mark}\n").as_bytes());
            if entry.kind == crate::codebase::Kind::Submodule {
                // A submodule's "content" is a commit id on its own line.
                stream.extend_from_slice(entry.hash.as_bytes());
                stream.extend_from_slice(b"\n");
                marks.insert(entry.path.clone(), (mark, mode, true));
            } else {
                // The reference side has to hold the same bytes the candidate
                // does or the pair means nothing, so a tree that names content
                // it does not hold stops the import.
                let Some(bytes) = source.blob(&entry.hash) else {
                    return Err(std::io::Error::other(format!(
                        "{} names {}, which the tree does not hold",
                        entry.path, entry.hash
                    )));
                };
                stream.extend_from_slice(format!("data {}\n", bytes.len()).as_bytes());
                stream.extend_from_slice(bytes);
                stream.extend_from_slice(b"\n");
                marks.insert(entry.path.clone(), (mark, mode, false));
            }
        }
        let mut commit = String::from(
            "commit refs/heads/main\ncommitter Tilo Qualification <q@example.invalid> 1700000000 +0000\n",
        );
        commit.push_str("data 12\nImport tree\n");
        for (path, (mark, mode, _)) in &marks {
            let path = quote(path);
            // A submodule's "mark" is its commit id, which fast-import takes
            // as it stands; everything else names a blob by mark.
            let separator = if *mode == "160000" { "" } else { ":" };
            let _ = writeln!(commit, "M {mode} {separator}{mark} {path}");
        }
        stream.extend_from_slice(commit.as_bytes());
        stream.extend_from_slice(b"\n");

        self.fast_import(&stream)?;
        let tree = self.git(&["rev-parse", MAIN_TREE])?;
        self.tree = Some(tree.trim().to_owned());
        Ok(())
    }

    /// Feed `stream` to `git fast-import`, which is how every tree in this
    /// scratch repository is built: from a stream of the bytes under test, not
    /// from a working directory.
    fn fast_import(&self, stream: &[u8]) -> std::io::Result<()> {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(["fast-import", "--quiet", "--force"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        {
            let Some(mut stdin) = child.stdin.take() else {
                return Err(std::io::Error::other("fast-import has no stdin"));
            };
            stdin.write_all(stream)?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "git fast-import failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(())
    }

    /// The paths a checkpoint changed against `base`, and how long Git took to
    /// say so.
    ///
    /// # Errors
    ///
    /// Returns the error when Git cannot diff the two trees.
    pub fn changed_paths(&self, base: &str, tree: &str) -> std::io::Result<(Timed, Vec<String>)> {
        let started = Instant::now();
        let listing = self.git(&["diff-tree", "-r", "-z", "--name-only", base, tree])?;
        let paths: Vec<String> = listing
            .as_bytes()
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .map(|record| String::from_utf8_lossy(record).into_owned())
            .collect();
        Ok((
            Timed {
                millis: crate::measurements::millis(started.elapsed()),
                correct: true,
            },
            paths,
        ))
    }

    /// Resolve every tracked path of `worktree`, which is how Git answers
    /// "what is at this path" for a whole tree: its index, one entry per path.
    ///
    /// This is the counterpart of walking a namespace from the root for each
    /// path, because it is what a build's first stat of each source file
    /// finds on the other side — not a diff, and not a tree listing.
    ///
    /// # Errors
    ///
    /// Returns the error when Git cannot read the worktree's index.
    pub fn resolve_all(&self, worktree: &std::path::Path) -> std::io::Result<(Timed, usize)> {
        let started = Instant::now();
        let listing = Self::git_in(worktree, &["ls-files", "-s", "-z"])?;
        let paths = listing
            .as_bytes()
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
            .count();
        Ok((
            Timed {
                millis: crate::measurements::millis(started.elapsed()),
                correct: paths > 0,
            },
            paths,
        ))
    }

    /// A second tree: the one `import` made, with `edits` replacing the content
    /// at those paths.
    ///
    /// Git's side of "what changed since the checkpoint", built the way the
    /// first tree was — through `fast-import`, from the same bytes — so the
    /// diff it produces is a diff of this content and nothing else. Every
    /// unchanged path is re-emitted from the object the first tree already
    /// named, so no blob is written twice.
    ///
    /// # Errors
    ///
    /// Returns the error when Git refuses the stream.
    pub fn tree_with(&mut self, edits: &[(String, Vec<u8>)]) -> std::io::Result<String> {
        let mut stream =
            Vec::with_capacity(64_usize.saturating_mul(edits.len()).saturating_add(512));
        for (index, (_path, bytes)) in edits.iter().enumerate() {
            let mark = index.saturating_add(1);
            stream.extend_from_slice(
                format!("blob\nmark :{mark}\ndata {}\n", bytes.len()).as_bytes(),
            );
            stream.extend_from_slice(bytes);
            stream.extend_from_slice(b"\n");
        }
        let message = format!("Change {} paths\n", edits.len());
        let mut commit = String::from(
            "commit refs/heads/main\ncommitter Tilo Qualification <q@example.invalid> 1700000100 +0000\n",
        );
        let _ = writeln!(commit, "data {}\n{message}", message.len());
        // The second commit starts from the first, so every path the edits do
        // not name keeps the object and the mode it already had — which is
        // what makes the diff between them the edit set and nothing else.
        commit.push_str("from refs/heads/main^0\n");
        for (index, (path, _)) in edits.iter().enumerate() {
            // An edit replaces content, not mode: the path stays the plain
            // file the tree already said it was.
            let mark = index.saturating_add(1);
            let _ = writeln!(commit, "M 100644 :{mark} {}", quote(path));
        }
        stream.extend_from_slice(commit.as_bytes());
        stream.extend_from_slice(b"\n");
        self.fast_import(&stream)?;
        let tree = self.git(&["rev-parse", MAIN_TREE])?;
        Ok(tree.trim().to_owned())
    }

    /// Commit `tree` on `main`, so a second commit can be diffed against the
    /// first.
    ///
    /// # Errors
    ///
    /// Returns the error when the commit cannot be made.
    pub fn commit_tree(&self, tree: &str, message: &str) -> std::io::Result<String> {
        let commit = self.git(&[
            "-c",
            "user.name=Qualification",
            "-c",
            "user.email=q@example.invalid",
            "commit-tree",
            tree,
            "-p",
            "main",
            "-m",
            message,
        ])?;
        let commit = commit.trim().to_owned();
        self.git(&["update-ref", "refs/heads/main", &commit])?;
        Ok(commit)
    }

    /// How long a working-checkout `status` takes on `tree`.
    ///
    /// Git answers status from the index and the worktree, so a fair
    /// comparison has to give it both: the tree is checked out next to the
    /// scratch repository and the answer is read with `status
    /// --porcelain -z`. Returns the report as well, so the caller can check
    /// Git saw what it should have before trusting the number.
    ///
    /// # Errors
    ///
    /// Returns the error when the checkout or `status` fails.
    pub fn status_of_checkout(
        &self,
        worktree: &Path,
        tree: &str,
    ) -> std::io::Result<(Timed, StatusReport)> {
        if !worktree.exists() {
            // Only a commit can be checked out, so the commit that carries
            // `tree` is what the scratch repository serves — the tree alone
            // would be an object a fetch may not hand over.
            let _ = tree;
            // `main` is the commit `import` made, whose tree this is.
            let commit = self.git(&["rev-parse", "main"])?;
            std::fs::create_dir_all(worktree)?;
            Self::git_in(worktree, &["init", "--quiet", "--initial-branch=main"])?;
            Self::git_in(
                worktree,
                &["fetch", "--quiet", self.path_str()?, commit.trim()],
            )?;
            Self::git_in(worktree, &["checkout", "--quiet", "FETCH_HEAD"])?;
        }
        let started = Instant::now();
        let listing = Self::git_in(
            worktree,
            &["status", "--porcelain", "-z", "--untracked-files=no"],
        )?;
        let mut report = StatusReport::default();
        for record in listing.as_bytes().split(|byte| *byte == 0) {
            let Some(state) = record.first_chunk::<2>() else {
                continue;
            };
            let state = *state;
            match state {
                [b'M' | b'R', _] | [_, b'M'] => report.modified = report.modified.saturating_add(1),
                [b'A' | b'?', _] => report.added = report.added.saturating_add(1),
                [b'D', _] | [_, b'D'] => report.deleted = report.deleted.saturating_add(1),
                _ => report.modified = report.modified.saturating_add(1),
            }
        }
        Ok((
            Timed {
                millis: crate::measurements::millis(started.elapsed()),
                // A clean checkout reporting nothing is Git getting the right
                // answer; reporting a change nobody made is not.
                correct: report.total() == 0,
            },
            report,
        ))
    }

    fn path_str(&self) -> std::io::Result<&str> {
        self.path
            .to_str()
            .ok_or_else(|| std::io::Error::other("the scratch repository path is not UTF-8"))
    }

    fn git(&self, arguments: &[&str]) -> std::io::Result<String> {
        let bytes = self.git_bytes(arguments)?;
        String::from_utf8(bytes)
            .map_err(|error| std::io::Error::other(format!("git output is not UTF-8: {error}")))
    }

    fn git_in(root: &Path, arguments: &[&str]) -> std::io::Result<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "git {} failed in {}: {}",
                arguments.join(" "),
                root.display(),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn git_bytes(&self, arguments: &[&str]) -> std::io::Result<Vec<u8>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.path)
            .args(arguments)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format!(
                "git {} failed: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
        Ok(output.stdout)
    }
}

/// Quote a path for a `fast-import` `M` line, which takes C-style quoting for
/// anything that is not a bare token.
fn quote(path: &str) -> String {
    let bare = !path.is_empty()
        && !path.starts_with('-')
        && path
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\');
    if bare {
        return path.to_owned();
    }
    let mut quoted = String::with_capacity(path.len().saturating_add(2));
    quoted.push('"');
    for byte in path.bytes() {
        match byte {
            b'"' => quoted.push_str("\\\""),
            b'\\' => quoted.push_str("\\\\"),
            b'\n' => quoted.push_str("\\n"),
            0x20..=0x7e => quoted.push(char::from(byte)),
            other => {
                let _ = write!(quoted, "\\{other:o}");
            }
        }
    }
    quoted.push('"');
    quoted
}

/// How many entries the manifest names below each top-level directory, so a
/// receipt says where a 21 000-file tree's weight actually is.
#[must_use]
pub fn entries_per_top_level(manifest: &Manifest) -> Vec<(String, usize)> {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for entry in &manifest.entries {
        let top = entry.path.split_once('/').map_or("(root)", |(top, _)| top);
        let count = counts.entry(top).or_default();
        *count = count.saturating_add(1);
    }
    let mut counts: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(top, count)| (top.to_owned(), count))
        .collect();
    counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    counts
}
