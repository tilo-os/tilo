//! Tantivy-backed content index for the files provider.
//!
//! Owns a persistent on-disk index at `~/.cache/qos/files-index` with
//! BM25 scoring over file bodies. The index is populated by a background
//! walker that runs at provider construction time and rebuilds only what
//! changed since the last commit — the schema stores each doc's mtime,
//! so a re-walk skips files whose mtime hasn't moved.
//!
//! Smart caching:
//!
//! - **Persistent** — the index survives shell restarts. Boot-time cost
//!   drops from "walk-and-index everything" to "check mtimes on stat".
//! - **Incremental** — only files whose `mtime` changed since the last
//!   indexed value are re-read + re-tokenised. The walker maintains a
//!   `path → mtime` snapshot in the index itself (via a `mtime` field
//!   the writer overwrites each time a path is re-added).
//! - **Query cache** — a small LRU keyed by `(needle, limit)` so
//!   repeated keystrokes (e.g. Ctrl+K → type "hello" → backspace →
//!   retype) hit an in-memory cache rather than re-executing the query.
//! - **Debounced writes** — the writer commits every `COMMIT_BATCH`
//!   docs or `COMMIT_INTERVAL_SECS` seconds, whichever comes first, so
//!   we amortise the tantivy commit cost across many files.
//!
//! Bounds:
//!
//! - Files larger than [`MAX_FILE_BYTES`] are indexed by header only
//!   (first 1 MB); the rest is left out to keep the index sane. A
//!   50-line binary that starts with a magic number still contributes
//!   a filename-only doc, so it surfaces via the name index.
//! - Files whose extension is on [`BINARY_EXTS`] are skipped entirely.
//! - Same skip-dir list as the name index (`.git`, `node_modules`,
//!   `target`, hidden paths, …).

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, Value, FAST, INDEXED, STORED, STRING, TEXT};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};
use walkdir::WalkDir;

/// Skip these directory names anywhere in the tree — same list the name
/// index uses; hoisted here so the two agree without a runtime coupling.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".cache",
    ".cargo",
    ".rustup",
    ".mozilla",
    ".gradle",
    ".pnpm-store",
    "__pycache__",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
    "build",
    "dist",
];

/// File extensions we skip outright — binaries with no useful body text.
const BINARY_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "svg", "avif",
    "mp3", "flac", "ogg", "wav", "opus", "aac", "m4a",
    "mp4", "mkv", "webm", "avi", "mov", "m4v",
    "zip", "tar", "gz", "xz", "zst", "bz2", "7z", "rar", "iso", "dmg",
    "exe", "dll", "so", "dylib", "o", "a", "lib", "class", "jar",
    "pyc", "pyo", "wasm",
    "pdf", "epub", "mobi",
    "ttf", "otf", "woff", "woff2",
    "sqlite", "sqlite3", "db",
];

const MAX_DEPTH: usize = 8;
/// Bytes of each file's body we index. Only the head of a file is needed
/// for the name/content search this powers, and capping it low is the
/// single biggest lever on total index size — which in turn bounds the
/// memory tantivy's background segment merge needs. A 1 MB cap over a
/// 50k-file home produced a multi-GB merge spike that OOM'd the nested
/// compositor ~90 s after boot; 16 KB keeps relevance for real text
/// files while holding the on-disk index (and its merges) small.
/// Peak merge memory scales with `MAX_FILE_BYTES * MAX_DOCS_BEFORE_MERGE`,
/// so both are kept small deliberately.
const MAX_FILE_BYTES: u64 = 16_384;
const MAX_ENTRIES: usize = 50_000;
const COMMIT_BATCH: usize = 512;
const COMMIT_INTERVAL_SECS: u64 = 3;
const QUERY_CACHE_CAPACITY: usize = 32;
/// Cap on how many docs a segment may hold before the merge policy stops
/// growing it further. Without a cap tantivy's default `LogMergePolicy`
/// will merge every segment into one ever-larger segment, and that final
/// merge reads the whole index into memory at once. Capping segment size
/// keeps every merge bounded (at the cost of a few more segments, which
/// the reader handles fine at this scale). With `MAX_FILE_BYTES` at 16 KB
/// this caps a merged segment's indexed text at ~32 MB, so the transient
/// memory a background merge needs stays well under the OOM threshold.
const MAX_DOCS_BEFORE_MERGE: usize = 2_000;
/// Minimum gap between reader-reload calls. Reader-reload does disk I/O
/// (unmap old segment, map new), so per-keystroke reloads produced a
/// visible palette jitter as the user typed fast. Anything under a
/// second is imperceptible for content-freshness — the walker commits
/// in batches anyway — and this keeps the query hot-path allocation-
/// and-syscall-free on cache misses.
const READER_RELOAD_INTERVAL_MS: u64 = 500;
/// Tantivy writer heap for the single indexing thread. Bounds the
/// in-memory buffer before each flush; smaller means smaller segments
/// (and smaller merges). 15 MB comfortably fits the `COMMIT_BATCH` doc
/// batches we push.
const WRITER_HEAP_BYTES: usize = 15_000_000;

/// One row of tantivy output.
#[derive(Clone, Debug)]
pub struct ContentHit {
    pub path: PathBuf,
    pub score: i32,
    pub snippet: Option<String>,
}

struct Fields {
    path: Field,
    stem: Field,
    body: Field,
    mtime: Field,
}

/// LRU query cache. Small keys, small values (Vec<ContentHit>), so a
/// bare VecDeque + linear scan is cheaper than a real LRU crate.
struct QueryCache {
    entries: VecDeque<(String, usize, Vec<ContentHit>)>,
    capacity: usize,
}

impl QueryCache {
    fn new(capacity: usize) -> Self {
        Self { entries: VecDeque::new(), capacity }
    }
    fn get(&self, needle: &str, limit: usize) -> Option<Vec<ContentHit>> {
        self.entries.iter().find_map(|(n, l, v)| {
            if n == needle && *l == limit {
                Some(v.clone())
            } else {
                None
            }
        })
    }
    fn put(&mut self, needle: String, limit: usize, hits: Vec<ContentHit>) {
        self.entries.retain(|(n, l, _)| !(n == &needle && *l == limit));
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back((needle, limit, hits));
    }
    fn invalidate(&mut self) {
        self.entries.clear();
    }
}

/// Public façade over the tantivy index. Cheap to construct — heavy
/// work runs on background threads.
pub struct FilesIndex {
    fields: Fields,
    reader: IndexReader,
    query_parser: QueryParser,
    cache: Arc<Mutex<QueryCache>>,
    /// Timestamp of the last `reader.reload()` call. Reader-reload does
    /// disk I/O per invocation, and a naive per-keystroke reload gave
    /// the palette a jitter spike whenever the user typed fast.
    /// Rate-limited to at most one reload per
    /// [`READER_RELOAD_INTERVAL_MS`].
    last_reload: Arc<Mutex<Option<Instant>>>,
    /// Kept alive so the writer thread doesn't drop while we're using
    /// the reader.
    _writer_handle: Arc<RwLock<Option<thread::JoinHandle<()>>>>,
}

impl FilesIndex {
    /// Open the on-disk index at `~/.cache/qos/files-index` (creating it
    /// if missing) and kick off a background walker that populates /
    /// updates it. Returns immediately; the reader can answer queries
    /// as soon as the first batch commits.
    pub fn open_or_create(home: &Path) -> Result<Self, tantivy::error::TantivyError> {
        let dir = files_index_dir()?;
        fs::create_dir_all(&dir).ok();
        let schema = build_schema();
        let fields = Fields {
            path: schema.get_field("path")?,
            stem: schema.get_field("stem")?,
            body: schema.get_field("body")?,
            mtime: schema.get_field("mtime")?,
        };
        let index = Index::open_in_dir(&dir).or_else(|_| Index::create_in_dir(&dir, schema.clone()))?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        let query_parser =
            QueryParser::for_index(&index, vec![fields.stem, fields.body]);
        let cache = Arc::new(Mutex::new(QueryCache::new(QUERY_CACHE_CAPACITY)));

        // Background walker thread. Owns the writer for its lifetime so
        // the reader on the main thread only ever needs a &IndexReader.
        let writer_handle = Arc::new(RwLock::new(None));
        {
            let writer_handle = Arc::clone(&writer_handle);
            let cache = Arc::clone(&cache);
            let index = index.clone();
            let home = home.to_owned();
            let stem = fields.stem;
            let path = fields.path;
            let body = fields.body;
            let mtime = fields.mtime;
            let handle = thread::Builder::new()
                .name("qos-files-index-walker".into())
                .spawn(move || {
                    // Single indexing thread with a bounded heap, plus a
                    // merge policy that caps segment size — together these
                    // keep the walker's peak memory small. The default
                    // multi-threaded writer + unbounded `LogMergePolicy`
                    // merged the whole home index in one shot and spiked to
                    // >1 GB ~90 s after boot, OOM-ing the compositor.
                    let Ok(mut writer) = index.writer_with_num_threads(1, WRITER_HEAP_BYTES) else {
                        return;
                    };
                    let mut merge_policy = tantivy::merge_policy::LogMergePolicy::default();
                    merge_policy.set_max_docs_before_merge(MAX_DOCS_BEFORE_MERGE);
                    writer.set_merge_policy(Box::new(merge_policy));
                    // Load a `path → indexed mtime` snapshot from the
                    // reader so we can decide which files need re-indexing.
                    let existing = load_mtime_snapshot(&index, path, mtime).unwrap_or_default();
                    walk_and_index(
                        &home,
                        &existing,
                        &mut writer,
                        Fields { path, stem, body, mtime },
                    );
                    let _ = writer.commit();
                    if let Ok(mut c) = cache.lock() {
                        c.invalidate();
                    }
                })
                .ok();
            if let Ok(mut slot) = writer_handle.write() {
                *slot = handle;
            }
        }
        Ok(Self {
            fields,
            reader,
            query_parser,
            cache,
            last_reload: Arc::new(Mutex::new(None)),
            _writer_handle: writer_handle,
        })
    }

    /// Run a query against the current index snapshot. Blocks; returns
    /// up to `limit` hits sorted by BM25 score descending. Rate-limits
    /// reader-reload to at most one per
    /// [`READER_RELOAD_INTERVAL_MS`] so a fast-typing user doesn't
    /// stall on repeat disk I/O.
    pub fn query(&self, needle: &str, limit: usize) -> Vec<ContentHit> {
        if needle.trim().is_empty() {
            return Vec::new();
        }
        if let Ok(cache) = self.cache.lock() {
            if let Some(hit) = cache.get(needle, limit) {
                return hit;
            }
        }
        // Throttled reader-reload. The walker commits in batches of up
        // to `COMMIT_BATCH` docs (or every `COMMIT_INTERVAL_SECS`), so
        // one reload per `READER_RELOAD_INTERVAL_MS` is more than
        // enough to see fresh content while keeping the keystroke
        // hot-path allocation-free.
        {
            let now = Instant::now();
            let mut should_reload = true;
            if let Ok(mut slot) = self.last_reload.lock() {
                if let Some(prev) = *slot {
                    if now.duration_since(prev)
                        < Duration::from_millis(READER_RELOAD_INTERVAL_MS)
                    {
                        should_reload = false;
                    }
                }
                if should_reload {
                    *slot = Some(now);
                }
            }
            if should_reload {
                let _ = self.reader.reload();
            }
        }
        let Ok(query) = self.query_parser.parse_query(needle) else {
            return Vec::new();
        };
        let searcher = self.reader.searcher();
        let Ok(top) = searcher.search(&query, &TopDocs::with_limit(limit)) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (score, addr) in top {
            let Ok(doc) = searcher.doc::<TantivyDocument>(addr) else {
                continue;
            };
            let Some(path_val) = doc.get_first(self.fields.path).and_then(|v| v.as_str()) else {
                continue;
            };
            let path = PathBuf::from(path_val);
            let normalised = (score * 20.0) as i32;
            out.push(ContentHit {
                path,
                score: normalised,
                snippet: None,
            });
        }
        // Only cache non-empty results. An empty response often means
        // "the walker hasn't reached these files yet" — caching it
        // pins a stale miss until the next explicit invalidate. This
        // was the "the palette shows nothing right after boot even
        // though the file is obviously there" bug the perf test
        // surfaced.
        if !out.is_empty() {
            if let Ok(mut cache) = self.cache.lock() {
                cache.put(needle.to_string(), limit, out.clone());
            }
        }
        out
    }
}

fn files_index_dir() -> Result<PathBuf, tantivy::error::TantivyError> {
    // `QOS_FILES_INDEX_DIR` is honoured first so tests + local dev can
    // point the on-disk index at a scratch dir without racing the
    // developer's real `~/.cache`. Production reads `dirs::cache_dir`
    // (`$XDG_CACHE_HOME` or `~/.cache`) and lands the index under
    // `qos/files-index`.
    if let Some(explicit) = std::env::var_os("QOS_FILES_INDEX_DIR") {
        return Ok(PathBuf::from(explicit));
    }
    let cache = dirs::cache_dir().ok_or_else(|| {
        tantivy::error::TantivyError::InvalidArgument("no XDG_CACHE_HOME".to_string())
    })?;
    Ok(cache.join("qos").join("files-index"))
}

fn build_schema() -> Schema {
    let mut b = Schema::builder();
    b.add_text_field("path", STRING | STORED);
    b.add_text_field("stem", TEXT | STORED);
    b.add_text_field("body", TEXT);
    b.add_u64_field("mtime", INDEXED | STORED | FAST);
    b.build()
}

fn load_mtime_snapshot(
    index: &Index,
    path_field: Field,
    mtime_field: Field,
) -> Result<HashMap<PathBuf, u64>, tantivy::error::TantivyError> {
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommitWithDelay)
        .try_into()?;
    let searcher = reader.searcher();
    let mut out = HashMap::new();
    for segment_reader in searcher.segment_readers() {
        let store_reader = segment_reader.get_store_reader(0)?;
        let max_doc = segment_reader.max_doc();
        for doc_id in 0..max_doc {
            let Ok(doc) = store_reader.get::<TantivyDocument>(doc_id) else {
                continue;
            };
            let Some(path_str) = doc.get_first(path_field).and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(mtime) = doc.get_first(mtime_field).and_then(|v| v.as_u64()) else {
                continue;
            };
            out.insert(PathBuf::from(path_str), mtime);
        }
    }
    Ok(out)
}

/// Read at most `cap` bytes from the head of `path` into a fresh buffer.
///
/// Crucially this never allocates more than `cap` bytes regardless of the
/// file's real size: `File::take(cap).read_to_end` stops reading at the
/// cap. The obvious `fs::read(path)` alternative allocates the *entire*
/// file first and truncates after, so a single large non-binary file (a
/// multi-GB log / json / csv / sql dump the extension filter doesn't
/// catch) ballooned the background walker to gigabytes and OOM'd the
/// compositor. `known_len` just sizes the initial `Vec` to avoid growth
/// churn; it is not trusted as an upper bound (the file may have grown).
fn read_head(path: &Path, known_len: u64, cap: u64) -> Option<Vec<u8>> {
    let reserve = known_len.min(cap) as usize;
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(reserve);
    file.take(cap).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn walk_and_index(
    home: &Path,
    existing: &HashMap<PathBuf, u64>,
    writer: &mut IndexWriter,
    fields: Fields,
) {
    let mut count = 0usize;
    let mut since_commit = 0usize;
    let mut last_commit = SystemTime::now();
    for entry in WalkDir::new(home)
        .max_depth(MAX_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !should_skip(e.path(), home))
    {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if is_binary(path) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if existing.get(path).copied() == Some(mtime) {
            continue;
        }
        let Some(bytes) = read_head(path, meta.len(), MAX_FILE_BYTES) else {
            continue;
        };
        let Ok(body) = std::str::from_utf8(&bytes) else {
            continue;
        };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        writer.delete_term(Term::from_field_text(
            fields.path,
            &path.to_string_lossy(),
        ));
        let mut doc = TantivyDocument::default();
        doc.add_text(fields.path, path.to_string_lossy());
        doc.add_text(fields.stem, stem);
        doc.add_text(fields.body, body);
        doc.add_u64(fields.mtime, mtime);
        writer.add_document(doc).ok();

        count += 1;
        since_commit += 1;
        if since_commit >= COMMIT_BATCH
            || last_commit
                .elapsed()
                .map(|d| d.as_secs())
                .unwrap_or(0)
                >= COMMIT_INTERVAL_SECS
        {
            writer.commit().ok();
            since_commit = 0;
            last_commit = SystemTime::now();
        }
        if count >= MAX_ENTRIES {
            break;
        }
    }
}

fn should_skip(path: &Path, home: &Path) -> bool {
    if let Ok(rel) = path.strip_prefix(home) {
        for comp in rel.components() {
            let comp = comp.as_os_str().to_string_lossy();
            if SKIP_DIRS.iter().any(|s| *s == comp) {
                return true;
            }
            if comp.starts_with('.') && comp != "." && comp != ".." {
                return true;
            }
        }
    }
    false
}

fn is_binary(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    BINARY_EXTS.contains(&ext.as_str())
}

fn _unused_duration() -> Duration {
    Duration::from_secs(COMMIT_INTERVAL_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_binary_hits_common_extensions() {
        assert!(is_binary(Path::new("/home/x/a.png")));
        assert!(is_binary(Path::new("/home/x/a.MP3")));
        assert!(!is_binary(Path::new("/home/x/a.md")));
        assert!(!is_binary(Path::new("/home/x/a")));
    }

    #[test]
    fn skip_dirs_hits_common_bloat() {
        let home = Path::new("/home/x");
        assert!(should_skip(Path::new("/home/x/proj/node_modules"), home));
        assert!(should_skip(Path::new("/home/x/.cache/foo"), home));
        assert!(!should_skip(Path::new("/home/x/proj/src/lib.rs"), home));
    }

    #[test]
    fn read_head_caps_allocation_regardless_of_file_size() {
        // Regression: `walk_and_index` used `fs::read` (whole file into
        // memory) then truncated, so one large non-binary file OOM'd the
        // walker. `read_head` must never return more than `cap` bytes even
        // when the file is far larger — and even when `known_len` lies
        // (understates the real size, e.g. a file that grew after `stat`).
        let dir = std::env::temp_dir().join(format!(
            "qos-read-head-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.log");
        // 5 MiB file; cap at 16 KiB.
        let big = vec![b'a'; 5 * 1024 * 1024];
        fs::write(&path, &big).unwrap();

        let cap = 16_384u64;
        let out = read_head(&path, big.len() as u64, cap).unwrap();
        assert_eq!(
            out.len(),
            cap as usize,
            "read_head must stop at the cap for a large file",
        );
        // Even if the caller under-reports the length (0), the read is
        // still capped — proves the bound comes from `take`, not `known_len`.
        let out2 = read_head(&path, 0, cap).unwrap();
        assert_eq!(out2.len(), cap as usize, "cap holds even when known_len lies");

        // A small file returns its whole content, not the cap.
        let small = dir.join("small.txt");
        fs::write(&small, b"hello").unwrap();
        let out3 = read_head(&small, 5, cap).unwrap();
        assert_eq!(out3, b"hello");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn query_cache_lru_evicts_oldest() {
        let mut c = QueryCache::new(2);
        c.put("a".to_string(), 10, Vec::new());
        c.put("b".to_string(), 10, Vec::new());
        c.put("c".to_string(), 10, Vec::new());
        assert!(c.get("a", 10).is_none());
        assert!(c.get("b", 10).is_some());
        assert!(c.get("c", 10).is_some());
    }

    #[test]
    fn query_cache_moves_repeat_to_head() {
        let mut c = QueryCache::new(2);
        c.put("a".to_string(), 10, Vec::new());
        c.put("b".to_string(), 10, Vec::new());
        // Re-put "a" — should not double-insert, and eviction of the
        // oldest on the next put should drop "b" (not "a").
        c.put("a".to_string(), 10, Vec::new());
        c.put("c".to_string(), 10, Vec::new());
        assert!(c.get("a", 10).is_some());
        assert!(c.get("b", 10).is_none());
        assert!(c.get("c", 10).is_some());
    }
}

#[cfg(test)]
mod content_index_integration {
    //! End-to-end coverage for the tantivy content index. Writes a
    //! handful of files into a `tempdir`, opens a
    //! [`FilesIndex`] on that dir as `home`, waits for the walker to
    //! finish, and asserts a query lands the right file.
    //!
    //! Runs against an OVERRIDDEN cache dir (`XDG_CACHE_HOME=<tmp>`)
    //! so the shared `~/.cache/qos/files-index` isn't touched by tests.
    //! [`serial_test::serial`] pins the env-var mutation so parallel
    //! tests can't race the write.
    use super::*;
    use serial_test::serial;
    use std::fs;
    use std::time::{Duration, Instant};

    fn wait_for<F: FnMut() -> bool>(mut cond: F, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    #[serial]
    fn walk_and_index_runs_synchronously_and_writes_docs() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let index_dir = tmp.path().join("idx");
        fs::create_dir_all(&index_dir).unwrap();

        fs::write(home.join("hello.md"), "world").unwrap();

        let schema = build_schema();
        let idx = Index::create_in_dir(&index_dir, schema.clone()).unwrap();
        let path_f = schema.get_field("path").unwrap();
        let stem_f = schema.get_field("stem").unwrap();
        let body_f = schema.get_field("body").unwrap();
        let mtime_f = schema.get_field("mtime").unwrap();
        let mut writer = idx.writer(WRITER_HEAP_BYTES).unwrap();
        let empty: HashMap<PathBuf, u64> = HashMap::new();
        walk_and_index(
            home,
            &empty,
            &mut writer,
            Fields {
                path: path_f,
                stem: stem_f,
                body: body_f,
                mtime: mtime_f,
            },
        );
        writer.commit().unwrap();
        drop(writer);

        // Reader-side probe.
        let reader = idx
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        reader.reload().unwrap();
        let searcher = reader.searcher();
        assert!(
            searcher.num_docs() > 0,
            "walk_and_index must add at least one doc; got {}",
            searcher.num_docs()
        );
    }

    #[test]
    #[serial]
    fn walks_home_and_finds_content_match() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // XDG_CACHE_HOME redirect so the on-disk index lives under
        // `tmp/.cache/qos/files-index`, not the developer's real
        // `~/.cache`. Restored at test-scope drop by
        // `serial_test::serial`.
        let cache_dir = tmp.path().join(".cache/qos/files-index");
        fs::create_dir_all(&cache_dir).ok();
        unsafe {
            std::env::set_var("QOS_FILES_INDEX_DIR", &cache_dir);
        }

        fs::write(
            home.join("shopping.md"),
            "# Shopping list\n\napples oranges pears\n",
        )
        .unwrap();
        fs::write(
            home.join("notes.md"),
            "# Notes\n\nsearch registry uses BM25.\n",
        )
        .unwrap();
        fs::write(home.join("binary.png"), &[0u8, 1, 2, 3]).unwrap();

        let index = FilesIndex::open_or_create(home).expect("open index");
        // Wait for the walker + first commit.
        let ready = wait_for(
            || {
                let hits = index.query("registry BM25", 10);
                !hits.is_empty()
            },
            Duration::from_secs(15),
        );
        assert!(ready, "index walker didn't produce hits within 15s");

        let hits = index.query("apples", 5);
        assert!(
            hits.iter().any(|h| h.path.ends_with("shopping.md")),
            "expected shopping.md in results, got {:?}",
            hits
        );

        // Binary file must NOT surface — we filter by extension.
        let hits = index.query("binary", 5);
        assert!(
            !hits.iter().any(|h| h.path.ends_with("binary.png")),
            "binary.png should be excluded, got {:?}",
            hits
        );
    }

    /// Rough latency budget for a keystroke through the tantivy path.
    /// Two goals:
    /// - a cache-miss query must land well below the perceived-instant
    ///   threshold (100 ms is the informal one-frame-of-latency budget
    ///   for search UIs; we set the assertion at 200 ms to leave room
    ///   for CI jitter and cold-JIT effects).
    /// - a cache-hit query must be sub-millisecond so a user pressing
    ///   backspace to restore a prior needle doesn't pay again for
    ///   the reader-reload / segment-search chain.
    #[test]
    #[serial]
    fn query_latency_stays_inside_keystroke_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cache_dir = tmp.path().join(".cache/qos/files-index");
        fs::create_dir_all(&cache_dir).ok();
        unsafe {
            std::env::set_var("QOS_FILES_INDEX_DIR", &cache_dir);
        }

        // Sprinkle enough files that BM25 has something to score.
        for i in 0..40 {
            fs::write(
                home.join(format!("f{i}.md")),
                format!("alpha bravo charlie {i} keyword"),
            )
            .unwrap();
        }

        let idx = FilesIndex::open_or_create(home).unwrap();
        // The walker's initial commit + reader-refresh dance can take a
        // second or two on cold disk, especially in CI. Widen the wait
        // — the perf assertion below only kicks in AFTER the index is
        // hot, so a slow walker doesn't skew the measurement.
        let ready = wait_for(
            || !idx.query("keyword", 10).is_empty(),
            Duration::from_secs(30),
        );
        assert!(ready, "walker didn't populate within 30s");

        // Force a cache miss.
        let miss_start = Instant::now();
        let hits = idx.query("bravo", 10);
        let miss_elapsed = miss_start.elapsed();
        assert!(!hits.is_empty(), "expected results for 'bravo'");
        assert!(
            miss_elapsed < Duration::from_millis(200),
            "cache-miss query took {miss_elapsed:?}, over the 200 ms budget"
        );

        // Repeat — this one hits the LRU cache.
        let hit_start = Instant::now();
        let _ = idx.query("bravo", 10);
        let hit_elapsed = hit_start.elapsed();
        assert!(
            hit_elapsed < Duration::from_millis(5),
            "cache-hit query took {hit_elapsed:?}, over the 5 ms budget"
        );
    }

    #[test]
    #[serial]
    fn incremental_reindex_skips_untouched_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cache_dir = tmp.path().join(".cache/qos/files-index");
        fs::create_dir_all(&cache_dir).ok();
        unsafe {
            std::env::set_var("QOS_FILES_INDEX_DIR", &cache_dir);
        }

        fs::write(home.join("a.md"), "alpha content").unwrap();
        fs::write(home.join("b.md"), "bravo content").unwrap();

        // First pass — populate the index.
        {
            let idx = FilesIndex::open_or_create(home).unwrap();
            let ready = wait_for(
                || !idx.query("alpha", 10).is_empty() && !idx.query("bravo", 10).is_empty(),
                Duration::from_secs(15),
            );
            assert!(ready, "first pass didn't index within 15s");
        }

        // Second pass — mtime unchanged, walker should skip both files.
        // We can't observe "skipped" directly, but the incremental
        // path still leaves the existing docs queryable. This proves
        // the reopen path doesn't wipe the index.
        let idx = FilesIndex::open_or_create(home).unwrap();
        // The reopened index reader might need a tick before it sees
        // the previous commit — poll briefly.
        let ready = wait_for(
            || !idx.query("alpha", 10).is_empty(),
            Duration::from_secs(15),
        );
        assert!(ready, "second-pass reader must see previously-committed docs");
    }
}
