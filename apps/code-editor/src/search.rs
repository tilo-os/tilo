//! Project-wide search: find a string across every text file under the project
//! root, without the UI thread ever waiting for the filesystem.
//!
//! Searching this repository means walking tens of thousands of files. Doing
//! that between two frames would freeze the window for seconds, so a [`Job`]
//! runs the walk on a worker thread and streams batches of [`Hit`]s back over a
//! channel that the app drains once per frame — the same shape the portal and
//! the language-server reader use. Results appear progressively as they are
//! found rather than all at once at the end, and [`Job::cancel`] stops a walk
//! the moment the query changes, so typing does not pile up eight live searches.
//!
//! The matcher itself is [`line_matches`], which is also what the in-buffer
//! find bar uses — one definition of "what counts as a match" for both, so the
//! count in the find bar and the hits in the panel can never disagree.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError};

use crate::disk;

/// How many hits a single search will report before it stops. A query like `e`
/// over a large repository matches millions of times; past a few thousand the
/// list has stopped being a search result and become a denial of service
/// against the person reading it. The UI says plainly when it truncated.
pub const MAX_HITS: usize = 5_000;

/// A no-match query must not be able to turn an arbitrarily huge checkout into
/// a forever-running background job. The result panel calls this out as a
/// partial search through its existing `truncated` state.
pub const MAX_FILES_SCANNED: usize = 100_000;

/// Directory listings are collected so each level can be sorted deterministically.
/// Put a ceiling on that temporary allocation too; a generated directory with
/// millions of entries is not a useful source-project view.
const MAX_DIRECTORY_ENTRIES: usize = 50_000;

/// At most this many batches may wait for the UI. The worker uses cancellable
/// backpressure rather than an unbounded channel, preserving complete results
/// up to `MAX_HITS` without turning a dense search into memory growth.
const SEARCH_CHANNEL_BATCH_CAPACITY: usize = 8;

/// Hits are shipped in batches rather than one message per hit — a channel send
/// per match dominates the actual searching on a dense query.
const BATCH: usize = 64;

/// Characters of context kept around a match in the preview line. Long minified
/// lines would otherwise blow out the results list.
const PREVIEW_LIMIT: usize = 200;

/// What to look for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub needle: String,
    pub case_sensitive: bool,
    /// Match only where the needle stands alone as a word, so searching `fs`
    /// does not bury you in `offset`, `refs` and `fsync`.
    pub whole_word: bool,
}

impl Query {
    pub fn new(needle: &str) -> Self {
        Query {
            needle: needle.to_string(),
            case_sensitive: false,
            whole_word: false,
        }
    }

    pub fn case_sensitive(mut self, on: bool) -> Self {
        self.case_sensitive = on;
        self
    }

    pub fn whole_word(mut self, on: bool) -> Self {
        self.whole_word = on;
        self
    }

    /// A query with nothing to look for matches nothing; the UI shows its empty
    /// state rather than every line in the project.
    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }
}

/// One match: a file, a position, and the line it sits on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Absolute path of the file the match is in.
    pub path: PathBuf,
    /// Path relative to the project root, as shown in the results list.
    pub rel: String,
    /// One-based line number, matching what every other tool prints.
    pub line: usize,
    /// Zero-based character column of the match start.
    pub col: usize,
    /// Character length of the match.
    pub len: usize,
    /// The matched line, trimmed of leading whitespace and clipped for display.
    pub preview: String,
    /// Character offset the preview was clipped by, so the highlight in the
    /// preview lands on the same characters the match covers.
    pub preview_offset: usize,
}

/// A message from the search worker.
#[derive(Debug)]
pub enum Msg {
    Hits(Vec<Hit>),
    /// The walk finished (or hit [`MAX_HITS`]).
    Done {
        /// How many files the walk covered, shown in the search summary and
        /// asserted by the worker tests.
        files: usize,
        truncated: bool,
    },
}

/// A running search. Dropping it cancels the worker.
pub struct Job {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    finished: bool,
}

impl Job {
    /// Whether the worker reported completion.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Stop the walk. The worker checks this between files, so a cancel takes
    /// effect within one file rather than at the end of the tree.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Drain everything the worker produced since the last call. Never blocks.
    pub fn drain(&mut self) -> Vec<Msg> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(msg) => {
                    if matches!(msg, Msg::Done { .. }) {
                        self.finished = true;
                    }
                    out.push(msg);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.finished = true;
                    break;
                }
            }
        }
        out
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Start searching `root` for `query` on a worker thread.
pub fn start(root: &Path, query: Query) -> Job {
    let (tx, rx) = std::sync::mpsc::sync_channel(SEARCH_CHANNEL_BATCH_CAPACITY);
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let root = root.to_path_buf();
    let worker_query = query;
    // The OS refusing to spawn a worker thread (resource exhaustion) is not
    // fatal here: `tx` drops with the closure, so `rx` reports done
    // immediately and the search just comes back empty instead of crashing
    // the editor.
    let _ = std::thread::Builder::new()
        .name("code-editor-search".into())
        .spawn(move || {
            walk(&root, &worker_query, &worker_cancel, &mut |msg| {
                send_cancellable(&tx, msg, &worker_cancel)
            });
        });
    Job {
        rx,
        cancel,
        finished: false,
    }
}

/// Deliver one batch without allowing a busy UI to make the worker retain an
/// unlimited result queue. Waiting is short and checks cancellation, so typing
/// a new query or closing the editor tears an old walk down promptly.
fn send_cancellable(tx: &SyncSender<Msg>, mut msg: Msg, cancel: &AtomicBool) -> bool {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        match tx.try_send(msg) {
            Ok(()) => return true,
            Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(returned)) => {
                msg = returned;
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
    }
}

/// The walk itself, factored out of the thread so tests can drive it
/// synchronously and assert on exactly the messages a real search emits.
///
/// `emit` returns `false` when the receiver is gone, which ends the walk — a
/// closed window must not keep a thread scanning a repository.
pub fn walk(root: &Path, query: &Query, cancel: &AtomicBool, emit: &mut dyn FnMut(Msg) -> bool) {
    let mut files = 0usize;
    let mut hits_total = 0usize;
    let mut batch: Vec<Hit> = Vec::with_capacity(BATCH);
    let mut truncated = false;

    if query.is_empty() {
        emit(Msg::Done {
            files: 0,
            truncated: false,
        });
        return;
    }

    // An explicit stack rather than recursion: a deep or symlink-looped tree
    // must not blow the worker's stack.
    let mut stack = vec![root.to_path_buf()];
    'walk: while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Sorted so results arrive in a stable, explicable order rather than
        // whatever order the filesystem happens to hand back.
        let mut dirs = Vec::new();
        let mut paths = Vec::new();
        for entry in entries.flatten() {
            if dirs.len().saturating_add(paths.len()) >= MAX_DIRECTORY_ENTRIES {
                truncated = true;
                break 'walk;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || disk::is_ignored_dir(&name) {
                continue;
            }
            // `file_type` does not follow symlinks, so a link pointing back up
            // the tree is neither descended into nor read.
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                dirs.push(entry.path());
            } else if ft.is_file() {
                paths.push(entry.path());
            }
        }
        paths.sort();
        dirs.sort();

        for path in paths {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let Ok(md) = std::fs::metadata(&path) else {
                continue;
            };
            if md.len() > disk::MAX_TEXT_BYTES {
                continue;
            }
            if files >= MAX_FILES_SCANNED {
                truncated = true;
                break 'walk;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            let sniff_len = text.len().min(8192);
            if text.as_bytes().get(..sniff_len).unwrap_or(&[]).contains(&0) {
                continue;
            }
            files += 1;
            let rel = relative_display(root, &path);
            for (idx, line) in text.lines().enumerate() {
                for (col, len) in line_matches(line, query) {
                    let (preview, preview_offset) = clip_preview(line, col);
                    batch.push(Hit {
                        path: path.clone(),
                        rel: rel.clone(),
                        line: idx + 1,
                        col,
                        len,
                        preview,
                        preview_offset,
                    });
                    hits_total += 1;
                    if batch.len() >= BATCH && !emit(Msg::Hits(std::mem::take(&mut batch))) {
                        return;
                    }
                    if hits_total >= MAX_HITS {
                        truncated = true;
                        break 'walk;
                    }
                }
            }
        }
        // Push in reverse so popping walks them in sorted order.
        for d in dirs.into_iter().rev() {
            stack.push(d);
        }
    }

    if !batch.is_empty() && !emit(Msg::Hits(batch)) {
        return;
    }
    emit(Msg::Done { files, truncated });
}

/// Every match of `query` in one line, as `(char_col, char_len)` pairs.
///
/// Columns are in *characters*, not bytes, because that is what the editor's
/// caret model and the CRDT's `Text` are addressed in — converting at the edges
/// here is what keeps a match on a line containing `é` from landing one column
/// off.
///
/// Matches do not overlap: after a hit, scanning resumes past its end, so
/// searching `aa` in `aaaa` finds two matches rather than three.
pub fn line_matches(line: &str, query: &Query) -> Vec<(usize, usize)> {
    if query.needle.is_empty() {
        return Vec::new();
    }
    let hay: Vec<char> = if query.case_sensitive {
        line.chars().collect()
    } else {
        line.chars().flat_map(char::to_lowercase).collect()
    };
    let needle: Vec<char> = if query.case_sensitive {
        query.needle.chars().collect()
    } else {
        query.needle.chars().flat_map(char::to_lowercase).collect()
    };
    // Case folding can change length (`ß` → `ss`), which would desynchronise
    // the folded index from the caret index. When it does, fall back to a
    // case-sensitive scan rather than reporting a column that is a lie.
    if !query.case_sensitive && hay.len() != line.chars().count() {
        return line_matches(
            line,
            &Query {
                case_sensitive: true,
                ..query.clone()
            },
        );
    }
    if needle.is_empty() || needle.len() > hay.len() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay.get(i..i + needle.len()) == Some(needle.as_slice())
            && (!query.whole_word || is_word_bounded(&hay, i, needle.len()))
        {
            out.push((i, needle.len()));
            i += needle.len();
            continue;
        }
        i += 1;
    }
    out
}

/// Whether the span at `start..start+len` is bounded by non-word characters on
/// both sides — the "whole word" rule.
fn is_word_bounded(hay: &[char], start: usize, len: usize) -> bool {
    let before_ok = start == 0 || hay.get(start - 1).is_none_or(|&c| !is_word_char(c));
    let after = start + len;
    let after_ok = after >= hay.len() || hay.get(after).is_none_or(|&c| !is_word_char(c));
    before_ok && after_ok
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Trim a line down to something that fits a results row, keeping the match
/// visible. Returns the preview and the character offset it was clipped by.
fn clip_preview(line: &str, col: usize) -> (String, usize) {
    let chars: Vec<char> = line.chars().collect();
    // Drop leading indentation — every hit in a deeply nested block would
    // otherwise start with twenty spaces and no content.
    let indent = chars
        .iter()
        .take_while(|c| c.is_whitespace())
        .count()
        .min(col);
    let mut start = indent;
    if col - start > PREVIEW_LIMIT / 2 {
        start = col - PREVIEW_LIMIT / 2;
    }
    let end = (start + PREVIEW_LIMIT).min(chars.len());
    let preview: String = chars.get(start..end).unwrap_or(&[]).iter().collect();
    (preview, start)
}

/// `path` written relative to `root`, for the results list. Falls back to the
/// full path when the file is somehow outside the root.
pub fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| path.to_string_lossy().to_string(),
        |p| p.to_string_lossy().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

    fn q(n: &str) -> Query {
        Query::new(n)
    }

    #[test]
    fn line_matches_use_character_columns_and_honour_query_options() {
        // Each phase matches its own line; no state is shared.
        finds_every_occurrence_in_a_line();
        overlapping_matches_are_not_double_counted();
        case_insensitive_by_default_and_exact_when_asked();
        columns_are_characters_not_bytes();
        case_folding_that_changes_length_does_not_shift_columns();
        whole_word_rejects_substrings();
        an_empty_needle_matches_nothing();
    }

    fn finds_every_occurrence_in_a_line() {
        assert_eq!(line_matches("foo bar foo", &q("foo")), vec![(0, 3), (8, 3)]);
    }

    fn overlapping_matches_are_not_double_counted() {
        // "aa" in "aaaa" is two matches, not three: after a hit we resume past
        // its end, which is what every editor's find does.
        assert_eq!(line_matches("aaaa", &q("aa")), vec![(0, 2), (2, 2)]);
    }

    fn case_insensitive_by_default_and_exact_when_asked() {
        assert_eq!(line_matches("Foo foo FOO", &q("foo")).len(), 3);
        assert_eq!(
            line_matches("Foo foo FOO", &q("foo").case_sensitive(true)),
            vec![(4, 3)]
        );
    }

    fn columns_are_characters_not_bytes() {
        // Each `é` is two bytes. A byte-indexed matcher would report column 8
        // for `x`; the caret model counts characters, so it must be 4.
        let line = "ééé x";
        assert_eq!(line_matches(line, &q("x")), vec![(4, 1)]);
        // And the reported column really does index the char the user sees.
        let chars: Vec<char> = line.chars().collect();
        assert_eq!(chars[4], 'x');
    }

    fn case_folding_that_changes_length_does_not_shift_columns() {
        // Lowercasing `ß` yields `ss`, one char becoming two. Reporting a folded
        // index against the original line would put the caret in the wrong
        // place, so the matcher falls back to an exact scan instead of lying.
        let line = "straße x";
        let hits = line_matches(line, &q("x"));
        assert_eq!(hits, vec![(7, 1)]);
        let chars: Vec<char> = line.chars().collect();
        assert_eq!(chars[7], 'x', "the reported column indexes the real char");
    }

    fn whole_word_rejects_substrings() {
        // Plain: `fs`, the `fs` inside `offset`, the one in `refs`, the one
        // starting `fs_read`, and the standalone one at the end.
        let line = "fs offset refs fs_read fs";
        assert_eq!(line_matches(line, &q("fs")).len(), 5);
        let whole = line_matches(line, &q("fs").whole_word(true));
        assert_eq!(whole.len(), 2, "only the standalone `fs` occurrences");
        assert_eq!(whole[0].0, 0);
        assert_eq!(whole[1].0, 23);
    }

    fn an_empty_needle_matches_nothing() {
        assert!(line_matches("anything", &q("")).is_empty());
        assert!(q("").is_empty());
    }

    // ---- the walk ------------------------------------------------------

    /// Run a walk synchronously and collect every message it emitted.
    fn run(root: &Path, query: &Query) -> (Vec<Hit>, Option<(usize, bool)>) {
        let cancel = AtomicBool::new(false);
        let mut hits = Vec::new();
        let mut done = None;
        walk(root, query, &cancel, &mut |msg| {
            match msg {
                Msg::Hits(h) => hits.extend(h),
                Msg::Done { files, truncated } => done = Some((files, truncated)),
            }
            true
        });
        (hits, done)
    }

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        std::fs::create_dir_all(r.join("src")).unwrap();
        std::fs::create_dir_all(r.join("target/debug")).unwrap();
        std::fs::create_dir_all(r.join(".git")).unwrap();
        std::fs::write(
            r.join("src/main.rs"),
            "fn main() {\n    let needle = 1;\n}\n",
        )
        .unwrap();
        std::fs::write(r.join("src/lib.rs"), "// no match here\n").unwrap();
        std::fs::write(r.join("README.md"), "the needle and the haystack\n").unwrap();
        // Both of these contain the needle but must never be searched.
        std::fs::write(r.join("target/debug/out.txt"), "needle\n").unwrap();
        std::fs::write(r.join(".git/COMMIT_EDITMSG"), "needle\n").unwrap();
        d
    }

    #[test]
    fn tree_search_reports_source_hits_skips_non_source_and_clips_previews() {
        // Each phase builds a fresh fixture tree (one phase adds a binary file).
        searches_the_tree_and_reports_file_line_and_column();
        build_output_and_vcs_directories_are_never_searched();
        binary_files_are_skipped();
        an_empty_query_finishes_immediately_without_walking();
        a_long_line_is_clipped_around_the_match();
    }

    fn searches_the_tree_and_reports_file_line_and_column() {
        let d = fixture();
        let (hits, done) = run(d.path(), &q("needle"));
        assert_eq!(hits.len(), 2, "one in src/main.rs, one in README.md");

        let main_hit = hits.iter().find(|h| h.rel.ends_with("main.rs")).unwrap();
        assert_eq!(main_hit.line, 2, "line numbers are 1-based");
        assert_eq!(
            main_hit.col, 8,
            "column is the character index of the match"
        );
        assert_eq!(main_hit.len, 6);
        assert_eq!(Path::new(&main_hit.rel), Path::new("src/main.rs"));
        assert_eq!(
            main_hit.preview, "let needle = 1;",
            "the preview drops indentation but keeps the code"
        );

        let (files, truncated) = done.expect("the walk reports completion");
        assert_eq!(files, 3, "only the three real source files were read");
        assert!(!truncated);
    }

    fn build_output_and_vcs_directories_are_never_searched() {
        let d = fixture();
        let (hits, _) = run(d.path(), &q("needle"));
        for h in &hits {
            assert!(
                !h.rel.starts_with("target") && !h.rel.starts_with(".git"),
                "{} was searched but must be skipped",
                h.rel
            );
        }
        // Proof the fixture really does contain the needle in those places, so
        // this test could fail if the skip regressed.
        assert!(
            std::fs::read_to_string(d.path().join("target/debug/out.txt"))
                .unwrap()
                .contains("needle")
        );
    }

    fn binary_files_are_skipped() {
        let d = fixture();
        std::fs::write(d.path().join("src/blob.bin"), b"needle\x00\x01\x02").unwrap();
        let (hits, _) = run(d.path(), &q("needle"));
        assert!(
            hits.iter().all(|h| !h.rel.ends_with("blob.bin")),
            "a NUL-bearing file is not text and must not produce hits"
        );
    }

    #[test]
    fn search_walk_stops_on_cancel_or_closed_receiver_and_streams_in_background() {
        // Each phase builds a fresh fixture tree and cancel flag.
        a_cancelled_walk_stops_early();
        a_dropped_receiver_ends_the_walk();
        the_background_job_streams_results_and_finishes();
    }

    fn a_cancelled_walk_stops_early() {
        let d = fixture();
        let cancel = AtomicBool::new(true); // cancelled before it starts
        let mut saw_any = false;
        walk(d.path(), &q("needle"), &cancel, &mut |_| {
            saw_any = true;
            true
        });
        assert!(!saw_any, "a cancelled walk emits nothing at all");
    }

    fn a_dropped_receiver_ends_the_walk() {
        let d = fixture();
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        // `emit` returning false stands in for a closed channel.
        walk(d.path(), &q("needle"), &cancel, &mut |_| {
            calls += 1;
            false
        });
        assert_eq!(calls, 1, "the walk stops at the first failed send");
    }

    fn the_background_job_streams_results_and_finishes() {
        let d = fixture();
        let mut job = start(d.path(), q("needle"));
        let mut hits = Vec::new();
        let deadline = ui::time::now() + HANG_BOUND;
        while ui::time::now() < deadline {
            for msg in job.drain() {
                match msg {
                    Msg::Hits(h) => hits.extend(h),
                    Msg::Done { .. } => {}
                }
            }
            if job.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(job.is_finished(), "the worker reported Done");
        assert_eq!(hits.len(), 2);
    }

    fn an_empty_query_finishes_immediately_without_walking() {
        let d = fixture();
        let (hits, done) = run(d.path(), &q(""));
        assert!(hits.is_empty());
        assert_eq!(done, Some((0, false)), "no file was even opened");
    }

    fn a_long_line_is_clipped_around_the_match() {
        let filler = "x".repeat(400);
        let line = format!("{filler}needle{filler}");
        let (preview, offset) = clip_preview(&line, 400);
        assert!(preview.chars().count() <= PREVIEW_LIMIT);
        assert!(preview.contains("needle"), "the match stays visible");
        assert_eq!(offset, 400 - PREVIEW_LIMIT / 2);
    }
}
