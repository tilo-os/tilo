//! Content index for the files provider, a [`DiskIndex`].
//!
//! Owns a persistent on-disk index at `~/.cache/tilo/files-index` with BM25
//! scoring over file bodies. A background walker rebuilds only what
//! changed since the last commit, using each doc's stored `mtime`. A small
//! LRU query cache, keyed by the searcher generation each result was read
//! from, absorbs repeated keystrokes; the writer commits every
//! `COMMIT_BATCH` docs or `COMMIT_INTERVAL_SECS` seconds, whichever is
//! first. Files over `MAX_FILE_BYTES` are indexed by header only.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use tilo_search::{DiskIndex, DiskWriter};
use walkdir::WalkDir;

/// Same list the name index uses, hoisted here so the two agree.
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
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "svg", "avif", "mp3", "flac", "ogg", "wav",
    "opus", "aac", "m4a", "mp4", "mkv", "webm", "avi", "mov", "m4v", "zip", "tar", "gz", "xz",
    "zst", "bz2", "7z", "rar", "iso", "dmg", "exe", "dll", "so", "dylib", "o", "a", "lib", "class",
    "jar", "pyc", "pyo", "wasm", "pdf", "epub", "mobi", "ttf", "otf", "woff", "woff2", "sqlite",
    "sqlite3", "db",
];

const MAX_DEPTH: usize = 8;
/// Bytes of each file's body indexed — the biggest lever on tantivy's
/// background merge memory: a 1 MB cap over a 50k-file home produced a
/// multi-GB merge spike that OOM'd the compositor ~90s after boot.
const MAX_FILE_BYTES: u64 = 16_384;
const MAX_ENTRIES: usize = 50_000;
const COMMIT_BATCH: usize = 512;
const COMMIT_INTERVAL_SECS: u64 = 3;
const QUERY_CACHE_CAPACITY: usize = 32;
/// Cap on docs per segment — without one, tantivy's default merge policy
/// eventually reads the whole index into memory. Caps a segment's text at ~32 MB.
const MAX_DOCS_BEFORE_MERGE: usize = 2_000;
/// Tantivy writer heap for the single indexing thread; smaller means
/// smaller segments and merges. Comfortably fits a [`COMMIT_BATCH`] batch.
const WRITER_HEAP_BYTES: usize = 15_000_000;

/// One content match.
#[derive(Clone, Debug)]
pub struct ContentHit {
    pub path: PathBuf,
    pub score: i32,
    pub snippet: Option<String>,
}

/// Small keys/values, so a bare `VecDeque` + linear scan beats a real LRU crate.
///
/// Every entry carries the searcher generation it was computed from, so a
/// result read before a commit can never answer a query against the index
/// after it — even when the search and the `put` straddle the reload.
struct QueryCache {
    entries: VecDeque<(String, usize, u64, Vec<ContentHit>)>,
    capacity: usize,
}

impl QueryCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity,
        }
    }
    fn get(&self, needle: &str, limit: usize, generation: u64) -> Option<Vec<ContentHit>> {
        self.entries.iter().find_map(|(n, l, g, v)| {
            if n == needle && *l == limit && *g == generation {
                Some(v.clone())
            } else {
                None
            }
        })
    }
    fn put(&mut self, needle: String, limit: usize, generation: u64, hits: Vec<ContentHit>) {
        self.entries
            .retain(|(n, l, _, _)| !(n == &needle && *l == limit));
        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back((needle, limit, generation, hits));
    }
}

/// Public façade over the content index; cheap to construct.
pub struct FilesIndex {
    index: Arc<DiskIndex>,
    cache: Arc<Mutex<QueryCache>>,
    /// Kept alive so the writer thread doesn't drop while in use.
    _writer_handle: Arc<RwLock<Option<thread::JoinHandle<()>>>>,
}

/// Scales a BM25 score into the small integer rank
/// [`ContentHit::score`] sorts by. Rust's `f32 as i32` cast saturates
/// (never wraps or is UB) for any input, including the astronomically
/// unlikely score that would overflow `i32` at this scale.
#[allow(
    clippy::cast_possible_truncation,
    reason = "f32 as i32 saturates rather than wrapping; a display rank losing sub-integer precision is intended, not a bug"
)]
fn bm25_score_to_rank(score: f32) -> i32 {
    (score * 20.0) as i32
}

impl FilesIndex {
    /// Open (or create) the index and kick off a background walker.
    /// Returns immediately; queries answer once the first batch commits.
    ///
    /// # Errors
    ///
    /// Returns the underlying `tantivy` error if the on-disk index at
    /// `~/.cache/tilo/files-index` can't be opened or created (a missing
    /// schema field, a corrupt directory, or similar).
    pub fn open_or_create(home: &Path) -> Result<Self, tilo_search::TantivyError> {
        #[cfg(test)]
        crate::shell::test_support::ambient_start("Files body index");
        let index = Arc::new(DiskIndex::open_or_create(&files_index_dir()?)?);
        let cache = Arc::new(Mutex::new(QueryCache::new(QUERY_CACHE_CAPACITY)));

        // Owns the writer for its lifetime; queries only read the index.
        let writer_handle = Arc::new(RwLock::new(None));
        {
            let writer_handle = Arc::clone(&writer_handle);
            let index = Arc::clone(&index);
            let home = home.to_owned();
            let handle = thread::Builder::new()
                .name("tilo-files-index-walker".into())
                .spawn(move || {
                    // Bounded heap + a merge policy capping segment size —
                    // see MAX_FILE_BYTES / MAX_DOCS_BEFORE_MERGE.
                    let Ok(mut writer) = index.writer(WRITER_HEAP_BYTES, MAX_DOCS_BEFORE_MERGE)
                    else {
                        return;
                    };
                    let existing = index.versions().unwrap_or_default();
                    walk_and_index(&home, &existing, &mut writer);
                    // Each commit reloads the reader on this worker, so the
                    // reconciled generation is published here and never on
                    // the compositor's per-keystroke query path.
                    let _ = writer.commit();
                })
                .ok();
            if let Ok(mut slot) = writer_handle.write() {
                *slot = handle;
            }
        }
        Ok(Self {
            index,
            cache,
            _writer_handle: writer_handle,
        })
    }

    /// Blocks; returns up to `limit` hits sorted by BM25 score descending.
    pub fn query(&self, needle: &str, limit: usize) -> Vec<ContentHit> {
        if needle.trim().is_empty() {
            return Vec::new();
        }
        // The walker's commits reload the reader on its own thread. Never
        // perform a disk-backed reader reload on the compositor's keystroke
        // path.
        let generation = self.index.generation();
        if let Ok(cache) = self.cache.lock()
            && let Some(hit) = cache.get(needle, limit, generation)
        {
            return hit;
        }
        let out: Vec<ContentHit> = self
            .index
            .search(needle, limit)
            .into_iter()
            .map(|hit| ContentHit {
                path: PathBuf::from(hit.key),
                score: bm25_score_to_rank(hit.score),
                snippet: None,
            })
            .collect();
        // Only cache non-empty results — an empty one often just means the
        // walker hasn't reached these files yet.
        if !out.is_empty()
            && let Ok(mut cache) = self.cache.lock()
        {
            cache.put(needle.to_string(), limit, generation, out.clone());
        }
        out
    }
}

fn files_index_dir() -> Result<PathBuf, tilo_search::TantivyError> {
    // Honoured first so tests can point at a scratch dir, not the real ~/.cache.
    if let Some(explicit) = std::env::var_os("TILO_FILES_INDEX_DIR") {
        return Ok(PathBuf::from(explicit));
    }
    let cache = tilo_paths::cache_dir().ok_or_else(|| {
        tilo_search::TantivyError::InvalidArgument("no XDG_CACHE_HOME".to_string())
    })?;
    Ok(cache.join("tilo").join("files-index"))
}

/// Read at most `cap` bytes from the head of `path`. Never allocates more
/// than `cap` regardless of file size — `fs::read` would allocate the
/// *entire* file first, which ballooned the walker to gigabytes on a
/// single large non-binary file. `known_len` only sizes the initial `Vec`;
/// it is not trusted as an upper bound.
fn read_head(path: &Path, known_len: u64, cap: u64) -> Option<Vec<u8>> {
    // Only a capacity hint for the allocation below, so a 32-bit usize
    // that can't hold the full u64 just under-reserves (0) rather than
    // truncating into a wrong-but-still-"valid" number.
    let reserve = usize::try_from(known_len.min(cap)).unwrap_or(0);
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(reserve);
    file.take(cap).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

fn walk_and_index(home: &Path, existing: &HashMap<String, u64>, writer: &mut DiskWriter) {
    let mut seen = HashSet::new();
    let mut reached_limit = false;
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
        if seen.len() >= MAX_ENTRIES {
            reached_limit = true;
            break;
        }
        let key = path.to_string_lossy().into_owned();
        seen.insert(key.clone());
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        if existing.get(&key).copied() == Some(mtime) {
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
            .unwrap_or_default();
        if writer.upsert(&key, mtime, stem, body).is_err() {
            return;
        }

        since_commit = since_commit.saturating_add(1);
        if since_commit >= COMMIT_BATCH
            || last_commit.elapsed().map_or(0, |d| d.as_secs()) >= COMMIT_INTERVAL_SECS
        {
            writer.commit().ok();
            since_commit = 0;
            last_commit = SystemTime::now();
        }
    }

    // A complete scan is also the deletion journal. Never reconcile after a
    // capped walk: paths beyond the bound were not observed and deleting them
    // would turn a performance ceiling into silent index data loss.
    if !reached_limit {
        for stale in existing.keys().filter(|key| !seen.contains(*key)) {
            writer.remove(stale);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_filters_skip_binaries_and_bloat_and_cap_head_reads() {
        // Phase `is_binary_hits_common_extensions`.
        {
            assert!(is_binary(Path::new("/home/x/a.png")));
            assert!(is_binary(Path::new("/home/x/a.MP3")));
            assert!(!is_binary(Path::new("/home/x/a.md")));
            assert!(!is_binary(Path::new("/home/x/a")));
        }

        // Phase `skip_dirs_hits_common_bloat`.
        {
            let home = Path::new("/home/x");
            assert!(should_skip(Path::new("/home/x/proj/node_modules"), home));
            assert!(should_skip(Path::new("/home/x/.cache/foo"), home));
            assert!(!should_skip(Path::new("/home/x/proj/src/lib.rs"), home));
        }

        // Phase `read_head_caps_allocation_regardless_of_file_size`.
        {
            let dir =
                std::env::temp_dir().join(format!("tilo-read-head-test-{}", std::process::id()));
            fs::create_dir_all(&dir).unwrap();
            let path = dir.join("big.log");
            let big = vec![b'a'; 5 * 1024 * 1024];
            fs::write(&path, &big).unwrap();

            let cap = 16_384u64;
            let out = read_head(&path, big.len() as u64, cap).unwrap();
            assert_eq!(
                out.len(),
                usize::try_from(cap).unwrap(),
                "read_head must stop at the cap for a large file",
            );
            // Under-reporting known_len (0) still caps — bound is `take`, not `known_len`.
            let out2 = read_head(&path, 0, cap).unwrap();
            assert_eq!(
                out2.len(),
                usize::try_from(cap).unwrap(),
                "cap holds even when known_len lies"
            );

            // A small file returns its whole content, not the cap.
            let small = dir.join("small.txt");
            fs::write(&small, b"hello").unwrap();
            let out3 = read_head(&small, 5, cap).unwrap();
            assert_eq!(out3, b"hello");

            fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn query_cache_is_an_lru_that_refreshes_repeats() {
        // Phase `query_cache_lru_evicts_oldest`.
        {
            let mut c = QueryCache::new(2);
            c.put("a".to_string(), 10, 0, Vec::new());
            c.put("b".to_string(), 10, 0, Vec::new());
            c.put("c".to_string(), 10, 0, Vec::new());
            assert!(c.get("a", 10, 0).is_none());
            assert!(c.get("b", 10, 0).is_some());
            assert!(c.get("c", 10, 0).is_some());
        }

        // Phase `query_cache_moves_repeat_to_head`.
        {
            let mut c = QueryCache::new(2);
            c.put("a".to_string(), 10, 0, Vec::new());
            c.put("b".to_string(), 10, 0, Vec::new());
            // Re-put "a": the next eviction should drop "b", not "a".
            c.put("a".to_string(), 10, 0, Vec::new());
            c.put("c".to_string(), 10, 0, Vec::new());
            assert!(c.get("a", 10, 0).is_some());
            assert!(c.get("b", 10, 0).is_none());
            assert!(c.get("c", 10, 0).is_some());
        }

        // An entry cached from an older searcher generation never answers
        // a query against a newer one.
        {
            let mut c = QueryCache::new(2);
            c.put("a".to_string(), 10, 1, Vec::new());
            assert!(c.get("a", 10, 1).is_some());
            assert!(c.get("a", 10, 2).is_none());
        }
    }
}

#[cfg(test)]
mod content_index_integration {
    //! End-to-end coverage against a `tempdir`, with `TILO_FILES_INDEX_DIR`
    //! overridden so the shared cache isn't touched; `serial` keeps the
    //! env-var mutation from racing.
    use super::*;
    use serial_test::serial;
    use std::fs;
    use std::time::{Duration, Instant};

    #[test]
    #[serial]
    fn walking_home_indexes_documents_and_finds_content() {
        // Phase `walk_and_index_runs_synchronously_and_writes_docs`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path();
            let index_dir = tmp.path().join("idx");
            fs::create_dir_all(&index_dir).unwrap();

            fs::write(home.join("hello.md"), "world").unwrap();

            let index = DiskIndex::open_or_create(&index_dir).unwrap();
            let mut writer = index
                .writer(WRITER_HEAP_BYTES, MAX_DOCS_BEFORE_MERGE)
                .unwrap();
            walk_and_index(home, &HashMap::new(), &mut writer);
            writer.commit().unwrap();
            drop(writer);
            index.reload().unwrap();
            assert!(
                !index.search("world", 5).is_empty(),
                "walk_and_index must add the document"
            );
        }

        // Phase `walks_home_and_finds_content_match`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path();
            let cache_dir = tmp.path().join(".cache/tilo/files-index");
            fs::create_dir_all(&cache_dir).ok();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_FILES_INDEX_DIR", &cache_dir);

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
            fs::write(home.join("binary.png"), [0u8, 1, 2, 3]).unwrap();

            let index = FilesIndex::open_or_create(home).expect("open index");
            tilo_test_env::wait_until("the index walker to produce hits", || {
                !index.query("registry BM25", 10).is_empty()
            });

            let hits = index.query("apples", 5);
            assert!(
                hits.iter().any(|h| h.path.ends_with("shopping.md")),
                "expected shopping.md in results, got {hits:?}"
            );

            // Binary must NOT surface — filtered by extension.
            let hits = index.query("binary", 5);
            assert!(
                !hits.iter().any(|h| h.path.ends_with("binary.png")),
                "binary.png should be excluded, got {hits:?}"
            );
        }
    }

    /// A cache-miss query must land under 200 ms (CI jitter budget); a
    /// cache-hit must be sub-millisecond.
    #[test]
    #[serial]
    fn query_latency_stays_inside_keystroke_budget() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let cache_dir = tmp.path().join(".cache/tilo/files-index");
        fs::create_dir_all(&cache_dir).ok();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_FILES_INDEX_DIR", &cache_dir);

        // Sprinkle enough files that BM25 has something to score.
        for i in 0..40 {
            fs::write(
                home.join(format!("f{i}.md")),
                format!("alpha bravo charlie {i} keyword"),
            )
            .unwrap();
        }

        let idx = FilesIndex::open_or_create(home).unwrap();
        // The perf assertion below kicks in only once the index is hot, so
        // a slow initial walker doesn't skew it.
        tilo_test_env::wait_until("the walker to populate the index", || {
            !idx.query("keyword", 10).is_empty()
        });

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
    #[expect(
        clippy::used_underscore_binding,
        reason = "the test observes the writer thread the index otherwise only keeps alive"
    )]
    fn incremental_reindex_skips_untouched_and_removes_deleted_files() {
        // Phase `incremental_reindex_skips_untouched_files`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path();
            let cache_dir = tmp.path().join(".cache/tilo/files-index");
            fs::create_dir_all(&cache_dir).ok();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_FILES_INDEX_DIR", &cache_dir);

            fs::write(home.join("a.md"), "alpha content").unwrap();
            fs::write(home.join("b.md"), "bravo content").unwrap();

            // First pass — populate the index.
            {
                let idx = FilesIndex::open_or_create(home).unwrap();
                tilo_test_env::wait_until("the first pass to index", || {
                    !idx.query("alpha", 10).is_empty() && !idx.query("bravo", 10).is_empty()
                });
            }

            // mtime unchanged; prove docs stay queryable, since "skipped" isn't observable.
            let idx = FilesIndex::open_or_create(home).unwrap();
            tilo_test_env::wait_until(
                "the second-pass reader to see previously-committed docs",
                || !idx.query("alpha", 10).is_empty(),
            );
        }

        // Phase `complete_incremental_scan_removes_deleted_files`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path();
            let cache_dir = tmp.path().join(".cache/tilo/files-index");
            fs::create_dir_all(&cache_dir).ok();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_FILES_INDEX_DIR", &cache_dir);

            let deleted_path = home.join("retired.md");
            fs::write(&deleted_path, "uniquely retired search content").unwrap();
            {
                let index = FilesIndex::open_or_create(home).unwrap();
                tilo_test_env::wait_until("the first scan to index the fixture", || {
                    !index.query("uniquely retired", 10).is_empty()
                });
                tilo_test_env::wait_until("the first scan's writer to finish", || {
                    index
                        ._writer_handle
                        .read()
                        .ok()
                        .and_then(|handle| handle.as_ref().map(thread::JoinHandle::is_finished))
                        .unwrap_or(false)
                });
            }

            fs::remove_file(&deleted_path).unwrap();
            let index = FilesIndex::open_or_create(home).unwrap();
            tilo_test_env::wait_until(
                "a complete reconciliation to drop the deleted file from search",
                || index.query("uniquely retired", 10).is_empty(),
            );
        }
    }
}
