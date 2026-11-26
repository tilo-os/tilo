//! Project text search: find a string across every text file under the project
//! root, without the UI thread ever waiting for the filesystem. The platform
//! search lists what it finds (see `search_provider.rs`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};

use crate::disk;

pub const MAX_HITS: usize = 5_000;

/// A no-match query must not be able to turn an arbitrarily huge checkout into
/// a forever-running background job. The result panel calls this out as a
/// partial search through its existing `truncated` state.
pub const MAX_FILES_SCANNED: usize = 100_000;

const MAX_DIRECTORY_ENTRIES: usize = 50_000;

const SEARCH_CHANNEL_BATCH_CAPACITY: usize = 8;

const BATCH: usize = 64;

/// Characters of context kept around a match in the preview line. Long minified
/// lines would otherwise blow out the results list.
const PREVIEW_LIMIT: usize = 200;

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

    pub fn is_empty(&self) -> bool {
        self.needle.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    pub rel: String,
    pub line: usize,
    pub col: usize,
    pub len: usize,
    pub preview: String,
    pub preview_offset: usize,
}

#[derive(Debug)]
pub enum Msg {
    Hits(Vec<Hit>),
    Done { files: usize, truncated: bool },
}

pub struct Job {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    /// What stops the walk.
    #[must_use]
    pub fn canceller(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// The walk's next message, once it is sent; `None` once the walk has let go.
    pub fn wait_next(&mut self) -> Option<Msg> {
        self.rx.recv().ok()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}

pub fn start(root: &Path, query: Query) -> Job {
    let (tx, rx) = std::sync::mpsc::sync_channel(SEARCH_CHANNEL_BATCH_CAPACITY);
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let root = root.to_path_buf();
    let worker_query = query;
    let _ = std::thread::Builder::new()
        .name("code-editor-search".into())
        .spawn(move || {
            walk(&root, &worker_query, &worker_cancel, &mut |msg| {
                send_cancellable(&tx, msg, &worker_cancel)
            });
        });
    Job { rx, cancel }
}

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
            if files >= MAX_FILES_SCANNED {
                truncated = true;
                break 'walk;
            }
            let Ok(loaded) = disk::load(&path) else {
                continue;
            };
            let text = loaded.text;
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

fn is_word_bounded(hay: &[char], start: usize, len: usize) -> bool {
    let before_ok = start == 0 || hay.get(start - 1).is_none_or(|&c| !is_word_char(c));
    let after = start + len;
    let after_ok = after >= hay.len() || hay.get(after).is_none_or(|&c| !is_word_char(c));
    before_ok && after_ok
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

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

pub fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).map_or_else(
        |_| path.to_string_lossy().to_string(),
        |p| p.to_string_lossy().to_string(),
    )
}
