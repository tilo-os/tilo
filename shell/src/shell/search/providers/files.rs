//! Files provider — searches the home directory for files and directories.
//!
//! Two backends: an in-memory boot-time walk of `~` for name/fuzzy queries,
//! and a persistent tantivy index (see [`super::super::files_index`]) over
//! file contents at `~/.cache/qos/files-index`, rebuilt incrementally in the
//! background. Enter routes a hit through [`crate::shell::doc_open`], which
//! resolves the extension to the installed package that opens it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

/// Skip these paths anywhere in the tree — they explode the index without
/// adding user-visible files. A pattern with a `/` matches that many
/// consecutive path components; see [`should_skip`].
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".cache",
    ".cargo",
    ".rustup",
    ".mozilla",
    ".config/gnome",
    ".gradle",
    ".pnpm-store",
    "__pycache__",
    ".venv",
    "venv",
    ".idea",
    ".vscode",
    "build",
    "dist",
    "Downloads/tmp",
    ".steam",
    "go/pkg/mod",
    "Android/Sdk",
    "bin/cache",
    "site-packages",
    "dist-packages",
];

const MAX_DEPTH: usize = 8;
const MAX_ENTRIES: usize = 50_000;
const MAX_RESULTS: usize = 40;
/// At most one modest retained ranked-query plan survives across palette
/// edits. Positions are `usize`s, so this caps the cache below 1 MiB on
/// 64-bit builds instead of retaining one corpus-sized vector per keystroke.
const RANKED_QUERY_CACHE_MAX_POSITIONS: usize = 131_072;
/// A publication gets a never-reused revision before it reaches the tiny
/// snapshot-swap lock. A global sequence is intentionally conservative:
/// another provider's rescan can make a cursor stale, never incorrectly
/// current, while avoiding a mutable field inside an otherwise immutable
/// `NameIndex` publication.
static NEXT_NAME_INDEX_GENERATION: AtomicU64 = AtomicU64::new(1);

/// In-memory index, populated by a background walker and refreshed only via
/// manual `rescan` (no inotify yet).
#[derive(Default)]
struct NameIndex {
    entries: Arc<Vec<NameEntry>>,
    exact_leaf: HashMap<String, Arc<Vec<usize>>>,
    exact_stem: HashMap<String, Arc<Vec<usize>>>,
    leaf_prefix: HashMap<String, Arc<Vec<usize>>>,
    leaf_grams: HashMap<String, Arc<Vec<usize>>>,
    path_trigrams: HashMap<String, Arc<Vec<usize>>>,
    /// Bumped on every walker replace; the narrowing cache keys on it so a
    /// re-scan invalidates stale candidate indices.
    generation: u64,
}

impl NameIndex {
    /// Build an immutable publication off the compositor path.  Readers only
    /// clone its outer `Arc`; a rescan never holds their short publication
    /// lock while allocating postings or sorting short-query buckets.
    fn from_entries(entries: Vec<NameEntry>, generation: u64) -> Self {
        let entries = Arc::new(entries);
        let mut exact_leaf: HashMap<String, Vec<usize>> = HashMap::new();
        let mut exact_stem: HashMap<String, Vec<usize>> = HashMap::new();
        let mut leaf_prefix: HashMap<String, Vec<usize>> = HashMap::new();
        let mut leaf_grams: HashMap<String, Vec<usize>> = HashMap::new();
        let mut path_trigrams: HashMap<String, Vec<usize>> = HashMap::new();
        for (position, entry) in entries.iter().enumerate() {
            exact_leaf
                .entry(entry.facts.leaf.to_string())
                .or_default()
                .push(position);
            let stem_len = usize::from(entry.facts.stem_len);
            if let Some(stem) = entry.facts.leaf.get(..stem_len) {
                exact_stem
                    .entry(stem.to_string())
                    .or_default()
                    .push(position);
            }
            for end in entry.facts.leaf.char_indices().map(|(at, _)| at).skip(1) {
                if end <= 64
                    && let Some(prefix) = entry.facts.leaf.get(..end)
                {
                    leaf_prefix
                        .entry(prefix.to_string())
                        .or_default()
                        .push(position);
                }
            }
            if entry.facts.leaf.len() <= 64 {
                leaf_prefix
                    .entry(entry.facts.leaf.to_string())
                    .or_default()
                    .push(position);
            }
            for gram in search_grams(&entry.facts.leaf) {
                leaf_grams.entry(gram).or_default().push(position);
            }
            for gram in search_grams(&entry.facts.lower) {
                if gram.chars().count() == 3 {
                    path_trigrams.entry(gram).or_default().push(position);
                }
            }
        }
        // A one/two-character request intentionally means exact/prefix/
        // contiguous *leaf* matching, never a vague subsequence across a
        // whole path. Pre-rank those retained postings now, off the UI
        // thread, with the same score/tie rules as a query. A huge `a` or
        // `th` bucket therefore still gives the globally best first page.
        let sort_short_postings = |needle: &str, positions: &mut Vec<usize>| {
            positions.sort_by(|left, right| {
                let rank = |position: &usize| {
                    entries
                        .get(*position)
                        .map_or((i32::MIN, usize::MAX, ""), |entry| {
                            (
                                name_score(needle, &entry.facts).unwrap_or(i32::MIN),
                                path_depth(&entry.facts.lower),
                                entry.facts.lower.as_str(),
                            )
                        })
                };
                let left = rank(left);
                let right = rank(right);
                right
                    .0
                    .cmp(&left.0)
                    .then_with(|| left.1.cmp(&right.1))
                    .then_with(|| left.2.cmp(right.2))
            });
        };
        for (gram, positions) in &mut leaf_grams {
            if gram.chars().count() <= 2 {
                sort_short_postings(gram, positions);
            }
        }
        for (prefix, positions) in &mut leaf_prefix {
            if prefix.chars().count() <= 2 {
                sort_short_postings(prefix, positions);
            }
        }
        let shared = |postings: HashMap<String, Vec<usize>>| {
            postings
                .into_iter()
                .map(|(key, positions)| (key, Arc::new(positions)))
                .collect()
        };
        Self {
            entries,
            exact_leaf: shared(exact_leaf),
            exact_stem: shared(exact_stem),
            leaf_prefix: shared(leaf_prefix),
            leaf_grams: shared(leaf_grams),
            path_trigrams: shared(path_trigrams),
            generation,
        }
    }
}

/// Retained 1/2/3-character leaf postings.  Long needles select their
/// rarest trigram; short input has explicit character/bigram buckets instead
/// of falling back to a corpus scan.  The final score still validates the
/// whole needle, so a posting is an accelerator, never a false result.
fn search_grams(value: &str) -> Vec<String> {
    let boundaries = value
        .char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(value.len()))
        .collect::<Vec<_>>();
    let mut out = Vec::new();
    for width in 1_usize..=3 {
        for window in boundaries.windows(width.saturating_add(1)) {
            if let [start, end] = window
                && let Some(gram) = value.get(*start..*end)
            {
                out.push(gram.to_string());
            }
        }
    }
    out
}

#[derive(Clone)]
struct NameEntry {
    path: PathBuf,
    display: String,
    facts: NameFacts,
    is_dir: bool,
}

pub struct FilesProvider {
    home: PathBuf,
    /// A tiny publication lock protects only swapping/cloning the immutable
    /// index `Arc`; no query copies the corpus or holds it through ranking.
    name_index: Arc<Mutex<Arc<NameIndex>>>,
    tantivy: Option<crate::shell::search::files_index::FilesIndex>,
    /// Entries examined by the most recent query — the per-keystroke cost,
    /// and what the narrowing test asserts on (unlike wall-clock time).
    last_scanned: AtomicUsize,
    ranked_queries: Mutex<RankedQueryCache>,
    ranked_query_epoch: AtomicU64,
    rank_mailbox: Arc<Mutex<Option<RankRequest>>>,
    rank_notify: mpsc::SyncSender<()>,
    #[cfg_attr(not(test), allow(dead_code))]
    active_rank_workers: Arc<AtomicUsize>,
    #[cfg_attr(not(test), allow(dead_code))]
    peak_rank_workers: Arc<AtomicUsize>,
}

struct RankedQuery {
    generation: u64,
    needle: String,
    positions: Arc<Vec<usize>>,
}

struct PendingRankedQuery {
    generation: u64,
    needle: String,
    receiver: mpsc::Receiver<RankedQuery>,
    cancelled: Arc<AtomicBool>,
}

struct RankRequest {
    generation: u64,
    needle: String,
    entries: Arc<Vec<NameEntry>>,
    candidates: Arc<Vec<usize>>,
    cancelled: Arc<AtomicBool>,
    sender: mpsc::SyncSender<RankedQuery>,
}

#[derive(Default)]
struct RankedQueryCache {
    ready: Vec<RankedQuery>,
    pending: Option<PendingRankedQuery>,
}

type RankWorker = (
    Arc<Mutex<Option<RankRequest>>>,
    mpsc::SyncSender<()>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
);

impl FilesProvider {
    fn rank_worker() -> RankWorker {
        // A single coalescing worker is deliberately shared by every
        // keystroke.  The one-slot mailbox retains only the newest request;
        // a cancelled current rank checks at small chunks and then picks it
        // up.  This prevents rapid typing from creating a thread/corpus-plan
        // pile-up while keeping the compositor entirely non-blocking.
        let mailbox = Arc::new(Mutex::new(None::<RankRequest>));
        let (notify, wake) = mpsc::sync_channel(1);
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let worker_mailbox = Arc::clone(&mailbox);
        let worker_active = Arc::clone(&active);
        let worker_peak = Arc::clone(&peak);
        let spawned = thread::Builder::new()
            .name("qos-search-files-rank".into())
            .spawn(move || {
                while wake.recv().is_ok() {
                    let request = worker_mailbox.lock().ok().and_then(|mut slot| slot.take());
                    let Some(request) = request else {
                        continue;
                    };
                    #[allow(
                        clippy::arithmetic_side_effects,
                        reason = "the sole rank worker increments from zero once per dequeued request and decrements before it receives another"
                    )]
                    let current = worker_active.fetch_add(1, Ordering::Relaxed) + 1;
                    worker_peak.fetch_max(current, Ordering::Relaxed);
                    let mut positions = Vec::new();
                    for (ordinal, position) in request.candidates.iter().copied().enumerate() {
                        if ordinal.is_multiple_of(512) && request.cancelled.load(Ordering::Relaxed)
                        {
                            break;
                        }
                        let Some(entry) = request.entries.get(position) else {
                            continue;
                        };
                        if let Some(score) = name_score(&request.needle, &entry.facts) {
                            positions.push((score, path_depth(&entry.facts.lower), position));
                        }
                    }
                    if !request.cancelled.load(Ordering::Relaxed) {
                        positions.sort_by(|left, right| {
                            right
                                .0
                                .cmp(&left.0)
                                .then_with(|| left.1.cmp(&right.1))
                                .then_with(|| {
                                    request.entries.get(left.2).map(|entry| &entry.facts.lower).cmp(
                                        &request.entries.get(right.2).map(|entry| &entry.facts.lower),
                                    )
                                })
                        });
                        let positions = Arc::new(
                            positions
                                .into_iter()
                                .map(|(_, _, position)| position)
                                .collect::<Vec<_>>(),
                        );
                        let _ = request.sender.send(RankedQuery {
                            generation: request.generation,
                            needle: request.needle,
                            positions,
                        });
                    }
                    worker_active.fetch_sub(1, Ordering::Relaxed);
                }
            });
        if spawned.is_err() {
            // A disconnected wake channel makes the synchronous caller
            // expose a bounded pending state instead of falling back to a
            // corpus scan. This is exceptionally rare but remains safe.
        }
        (mailbox, notify, active, peak)
    }

    /// Kicks off the background name-index walk and returns immediately.
    /// Users who query before it finishes get only the tantivy content index.
    pub fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        let name_index: Arc<Mutex<Arc<NameIndex>>> =
            Arc::new(Mutex::new(Arc::new(NameIndex::default())));
        {
            let name_index = Arc::clone(&name_index);
            let home = home.clone();
            thread::Builder::new()
                .name("qos-search-files-walk".into())
                .spawn(move || walk_home(&home, &name_index))
                .ok();
        }
        let tantivy = crate::shell::search::files_index::FilesIndex::open_or_create(&home).ok();
        let (rank_mailbox, rank_notify, active_rank_workers, peak_rank_workers) =
            Self::rank_worker();
        Self {
            home,
            name_index,
            tantivy,
            last_scanned: AtomicUsize::new(0),
            ranked_queries: Mutex::new(RankedQueryCache::default()),
            ranked_query_epoch: AtomicU64::new(0),
            rank_mailbox,
            rank_notify,
            active_rank_workers,
            peak_rank_workers,
        }
    }

    /// Force a re-walk of the home directory. Idempotent; safe to call from any thread.
    pub fn rescan(&self) {
        let name_index = Arc::clone(&self.name_index);
        let home = self.home.clone();
        thread::Builder::new()
            .name("qos-search-files-rescan".into())
            .spawn(move || walk_home(&home, &name_index))
            .ok();
    }

    /// Content hits from tantivy, appended to `out` and deduped against
    /// `seen` name hits so a file matching both only shows once (the
    /// higher score wins). Split out of [`SearchProvider::query`] to stay
    /// under clippy's line budget.
    fn push_content_hits(
        &self,
        needle: &str,
        seen: &HashSet<PathBuf>,
        out: &mut Vec<SearchResult>,
    ) {
        let Some(idx) = self.tantivy.as_ref() else {
            return;
        };
        let content_hits = idx.query(needle, MAX_RESULTS);
        for hit in content_hits {
            if seen.contains(&hit.path) {
                continue;
            }
            if out.len() >= MAX_RESULTS {
                break;
            }
            let display = hit.path.strip_prefix(&self.home).map_or_else(
                |_| hit.path.display().to_string(),
                |p| format!("~/{}", p.display()),
            );
            let entry = NameEntry {
                path: hit.path.clone(),
                facts: NameFacts::new(&display, None),
                display,
                is_dir: false,
            };
            // Clamp BM25 into the band the name scale reserves for it.
            let content = hit.score.clamp(CONTENT_SCORE_MIN, CONTENT_SCORE_MAX);
            out.push(make_result(&entry, content, hit.snippet));
        }
    }

    /// Return a globally score-sorted retained query plan, or start its
    /// off-thread construction.  The compositor never walks a broad posting
    /// list synchronously: a pending plan is explicitly empty rather than a
    /// misleading traversal-order partial answer.  Completion bumps
    /// `ranked_query_epoch`, which makes an open palette refresh itself.
    fn ranked_query_positions(
        &self,
        generation: u64,
        needle: &str,
        entries: Arc<Vec<NameEntry>>,
        candidates: Arc<Vec<usize>>,
    ) -> Option<Arc<Vec<usize>>> {
        let Ok(mut cache) = self.ranked_queries.lock() else {
            return None;
        };
        let received =
            cache
                .pending
                .as_mut()
                .and_then(|pending| match pending.receiver.try_recv() {
                    Ok(ready) => Some(Ok(ready)),
                    Err(mpsc::TryRecvError::Disconnected) => Some(Err(())),
                    Err(mpsc::TryRecvError::Empty) => None,
                });
        if let Some(received) = received {
            match received {
                Ok(ready) => {
                    cache.ready.retain(|query| {
                        query.generation != ready.generation || query.needle != ready.needle
                    });
                    cache.ready.push(ready);
                    while cache
                        .ready
                        .iter()
                        .map(|query| query.positions.len())
                        .sum::<usize>()
                        > RANKED_QUERY_CACHE_MAX_POSITIONS
                    {
                        cache.ready.remove(0);
                    }
                    cache.pending = None;
                    self.ranked_query_epoch.fetch_add(1, Ordering::Relaxed);
                }
                Err(()) => cache.pending = None,
            }
        }
        if let Some(query) = cache
            .ready
            .iter()
            .find(|query| query.generation == generation && query.needle == needle)
        {
            return Some(Arc::clone(&query.positions));
        }
        let already_building = cache
            .pending
            .as_ref()
            .is_some_and(|pending| pending.generation == generation && pending.needle == needle);
        if already_building {
            return None;
        }
        if let Some(previous) = cache.pending.take() {
            previous.cancelled.store(true, Ordering::Relaxed);
        }
        let needle = needle.to_string();
        let (sender, result_rx) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let request = RankRequest {
            generation,
            needle: needle.clone(),
            entries,
            candidates,
            cancelled: Arc::clone(&cancelled),
            sender,
        };
        if let Ok(mut mailbox) = self.rank_mailbox.lock() {
            *mailbox = Some(request);
            // A full wake slot means the single worker is either active or
            // already scheduled; in both cases it will take this newest
            // mailbox value next. Never create another worker for a key.
            let _ = self.rank_notify.try_send(());
            cache.pending = Some(PendingRankedQuery {
                generation,
                needle,
                receiver: result_rx,
                cancelled,
            });
        }
        None
    }

    fn sorted_positions(needle: &str, entries: &[NameEntry]) -> Arc<Vec<usize>> {
        let mut positions = entries
            .iter()
            .enumerate()
            .filter_map(|(position, entry)| {
                name_score(needle, &entry.facts)
                    .map(|score| (score, path_depth(&entry.facts.lower), position))
            })
            .collect::<Vec<_>>();
        positions.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| {
                    entries
                        .get(left.2)
                        .map(|entry| &entry.facts.lower)
                        .cmp(&entries.get(right.2).map(|entry| &entry.facts.lower))
                })
        });
        Arc::new(
            positions
                .into_iter()
                .map(|(_, _, position)| position)
                .collect(),
        )
    }

    /// A page cursor is deliberately only an index generation and a scan
    /// offset.  It never embeds paths (which would leak names across a later
    /// permission/mount change) nor a giant match list.  A page inspects at
    /// most `budget.max_work` retained entries; `last_scanned` is kept as
    /// instrumentation and regression protection for the palette path.
    #[allow(
        clippy::too_many_lines,
        reason = "one bounded paging operation keeps cursor validation, ranked-plan selection, and result accounting consistent"
    )]
    fn page_name_hits(
        &self,
        ctx: &SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        if ctx.needle.trim().is_empty() || budget.max_results == 0 || budget.max_work == 0 {
            return SearchPage::exhausted("empty", Vec::new());
        }
        let want_file = ctx.wanted.contains(&ResourceKind::File);
        let want_folder = ctx.wanted.contains(&ResourceKind::Folder);
        if !want_file && !want_folder {
            return SearchPage::exhausted("not-wanted", Vec::new());
        }
        let Ok(index) = self.name_index.lock() else {
            return SearchPage::exhausted("unavailable", Vec::new());
        };
        let entries = Arc::clone(&index.entries);
        let generation = index.generation;
        let mut revision = format!(
            "{generation}:{}",
            self.ranked_query_epoch.load(Ordering::Relaxed)
        );
        let needle = ctx.needle.to_lowercase();
        let offset = match cursor {
            Some(cursor) if cursor.revision != revision => return SearchPage::stale(revision),
            Some(cursor) => match cursor.token.split_once('\u{1f}') {
                Some((cursor_needle, offset)) if cursor_needle == needle => match offset.parse() {
                    Ok(offset) => offset,
                    Err(_) => return SearchPage::stale(revision),
                },
                _ => return SearchPage::stale(revision),
            },
            None => 0,
        };
        let needle_chars = needle.chars().count();
        let gram_width = needle_chars.min(3);
        let all_grams = search_grams(&needle);
        let rarest_gram = (1..=gram_width).rev().find_map(|width| {
            all_grams
                .iter()
                .filter(|gram| gram.chars().count() == width)
                .filter_map(|gram| index.leaf_grams.get(gram))
                .min_by_key(|positions| positions.len())
                .cloned()
        });
        let rarest_path_trigram = all_grams
            .iter()
            .filter(|gram| gram.chars().count() == 3)
            .filter_map(|gram| index.path_trigrams.get(gram))
            .min_by_key(|positions| positions.len())
            .cloned();
        let needs_ranked_plan = needle_chars > 2 && entries.len() > budget.max_work;
        let ranked_candidates = match (rarest_gram.as_ref(), rarest_path_trigram.as_ref()) {
            (Some(leaf), Some(path)) if leaf.len() <= path.len() => Some(leaf.clone()),
            (Some(_) | None, Some(path)) => Some(path.clone()),
            (Some(leaf), None) => Some(leaf.clone()),
            (None, None) => None,
        };
        // Exact postings remain a retained accelerator/consistency check,
        // but cannot by themselves be the page source: a globally ranked
        // result set must still include weaker prefix/path matches after an
        // exact leaf hit.
        let exact_hit_count = index
            .exact_leaf
            .get(&needle)
            .or_else(|| index.exact_stem.get(&needle))
            .map_or(0, |positions| positions.len());
        debug_assert!(exact_hit_count <= entries.len());
        if needs_ranked_plan && ranked_candidates.is_none() {
            // Large-corpus fuzzy subsequence search would require a full
            // candidate walk for every keypress.  Keep it intentionally out
            // of this mode rather than claiming an incomplete page is a
            // search result; contiguous leaf/path discovery remains indexed.
            return SearchPage::exhausted(revision, Vec::new());
        }
        let short_positions = (needle_chars <= 2)
            .then(|| index.leaf_prefix.get(&needle).cloned().or(rarest_gram))
            .flatten();
        drop(index);
        let source_positions = if needle_chars <= 2 {
            short_positions
        } else if needs_ranked_plan {
            ranked_candidates.and_then(|candidates| {
                self.ranked_query_positions(generation, &needle, Arc::clone(&entries), candidates)
            })
        } else {
            Some(Self::sorted_positions(&needle, &entries))
        };
        // Polling a completed retained plan can advance the ranking epoch in
        // this call. Mint its first cursor against that new revision, not the
        // pre-poll revision which would be stale before the next page.
        revision = format!(
            "{generation}:{}",
            self.ranked_query_epoch.load(Ordering::Relaxed)
        );
        if needle_chars <= 2 && source_positions.is_none() {
            // Short input has intentionally narrow, truthfully-ranked leaf
            // semantics. Falling through to a path/fuzzy corpus sweep would
            // make the displayed first page look exhaustive when it cannot
            // be under the work budget.
            return SearchPage::exhausted(revision, Vec::new());
        }
        // Broad queries do not display a traversal-order approximation while
        // their ranked retained plan is building. The epoch above changes on
        // completion, causing the open palette to retry automatically.
        if needs_ranked_plan && source_positions.is_none() {
            return SearchPage {
                revision,
                results: Vec::new(),
                next_cursor: None,
                exhausted: false,
                stale_cursor: false,
                budget_exhausted: true,
            };
        }
        let source_len = source_positions
            .as_ref()
            .map_or(entries.len(), |positions| positions.len());
        if offset > source_len {
            return SearchPage::stale(revision);
        }
        let end = offset.saturating_add(budget.max_work).min(source_len);
        self.last_scanned
            .store(end.saturating_sub(offset), Ordering::Relaxed);
        let mut bytes = 0usize;
        let mut results = Vec::new();
        let mut next = end;
        let mut stopped_for_budget = false;
        for source_position in offset..end {
            let position = source_positions
                .as_ref()
                .and_then(|positions| positions.get(source_position).copied())
                .unwrap_or(source_position);
            let Some(entry) = entries.get(position) else {
                continue;
            };
            if (entry.is_dir && !want_folder) || (!entry.is_dir && !want_file) {
                continue;
            }
            let Some(score) = name_score(&needle, &entry.facts) else {
                continue;
            };
            if results.len() >= budget.max_results {
                next = source_position;
                stopped_for_budget = true;
                break;
            }
            // Budget what the palette receives, including both labels and
            // the opaque path id, before cloning any row into the UI.
            let row_bytes = entry
                .path
                .display()
                .to_string()
                .len()
                .saturating_add(entry.display.len())
                .saturating_add(file_title(&entry.path).len())
                .saturating_add(96);
            if bytes.saturating_add(row_bytes) > budget.max_bytes {
                next = source_position;
                stopped_for_budget = true;
                break;
            }
            bytes = bytes.saturating_add(row_bytes);
            results.push(make_result(entry, score, None));
        }
        let exhausted = !stopped_for_budget && end == source_len;
        SearchPage {
            revision: revision.clone(),
            results,
            next_cursor: (!exhausted).then(|| SearchCursor {
                revision,
                token: format!("{needle}\u{1f}{next}"),
            }),
            exhausted,
            stale_cursor: false,
            budget_exhausted: stopped_for_budget || !exhausted,
        }
    }
}

impl Default for FilesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for FilesProvider {
    fn id(&self) -> &'static str {
        "files"
    }
    fn name(&self) -> &'static str {
        "Files"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::File, ResourceKind::Folder]
    }
    /// See [`MAX_SCORE`].
    fn max_score(&self) -> i32 {
        MAX_SCORE
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        // Compatibility callers get exactly one bounded first page.  They
        // never trigger the former synchronous corpus sweep; broad results
        // may be temporarily empty while the retained plan is being ranked.
        self.query_page(
            ctx,
            None,
            SearchBudget {
                max_results: MAX_RESULTS,
                max_bytes: 64 * 1024,
                max_work: SearchBudget::PALETTE.max_work,
            },
        )
        .results
    }

    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        self.name_index.lock().map_or_else(
            |_| "unavailable".into(),
            |index| {
                format!(
                    "{}:{}",
                    index.generation,
                    self.ranked_query_epoch.load(Ordering::Relaxed)
                )
            },
        )
    }

    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        // Name lookup owns all continuation pages.  Tantivy already has its
        // own retained inverted index and returns a bounded top-K; append it
        // only to the first page so scrolling never repeats content rows.
        let mut page = self.page_name_hits(&ctx, cursor, budget);
        if cursor.is_some() || page.stale_cursor || !ctx.wanted.contains(&ResourceKind::File) {
            return page;
        }
        let mut seen = page
            .results
            .iter()
            .map(|row| PathBuf::from(&row.id))
            .collect::<HashSet<_>>();
        let remaining = budget.max_results.saturating_sub(page.results.len());
        if remaining > 0 {
            let mut content = Vec::new();
            self.push_content_hits(ctx.needle, &seen, &mut content);
            for row in content.into_iter().take(remaining) {
                seen.insert(PathBuf::from(&row.id));
                page.results.push(row);
            }
        }
        page.results.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| path_depth(&a.id).cmp(&path_depth(&b.id)))
                .then_with(|| a.id.cmp(&b.id))
        });
        page
    }

    fn owns_tab(&self, _tab_id: u64, _chrome: &ChromeState) -> bool {
        // Files isn't a tab-owning provider yet; the file explorer app owns tabs.
        false
    }
}

fn make_result(entry: &NameEntry, score: i32, subtitle: Option<String>) -> SearchResult {
    let kind = if entry.is_dir {
        ResourceKind::Folder
    } else {
        ResourceKind::File
    };
    let path = entry.path.clone();
    let is_dir = entry.is_dir;
    SearchResult::nullary(
        kind,
        "files",
        entry.path.display().to_string(),
        file_title(&entry.path),
        subtitle.or_else(|| Some(entry.display.clone())),
        score,
        move |_doc, _root, chrome: &Chrome| {
            open_search_hit(&path, is_dir, chrome);
        },
    )
}

fn file_title(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |s| s.to_string_lossy().to_string(),
    )
}

/// Bounds for a content hit, at the level of a path-only name hit
/// ([`STRUCT_PATH_SUBSTRING`]). Clamped because the index reports raw BM25,
/// which is unbounded and would otherwise dwarf every other provider's scale.
const CONTENT_SCORE_MIN: i32 = 20;
const CONTENT_SCORE_MAX: i32 = 40;

// Four signals feed the score: structure, length ratio, recency, and
// provenance/depth penalties. Penalties are ranking signals, never
// filters — the floor is 1, so anything that matches is still findable.

/// Structural tiers, strongest first. The needle either matched the leaf
/// (the filename) or, failing that, somewhere up the path.
const STRUCT_EXACT_LEAF: i32 = 88;
/// The leaf minus its extension is exactly the needle: `open` → `open.md`.
const STRUCT_EXACT_STEM: i32 = 84;
/// The needle is exactly the first word of the name: `open` → `open-notes.md`.
const STRUCT_FIRST_TOKEN: i32 = 76;
/// The needle is exactly a later word: `open` → `xdg-open`.
const STRUCT_TOKEN: i32 = 70;
/// The needle starts the first word but does not finish it:
/// `open` → `openttd-logo.png`.
const STRUCT_FIRST_TOKEN_PREFIX: i32 = 64;
/// The needle starts a later word: `open` → `test-openai-upload.py`.
const STRUCT_TOKEN_PREFIX: i32 = 56;
/// The needle sits inside a word, touching no boundary: `pen` → `open.md`.
const STRUCT_IN_TOKEN: i32 = 44;
/// The needle is exactly one of the parent directory names.
const STRUCT_PATH_COMPONENT: i32 = 36;
/// The needle starts a word in one of the parent directory names.
const STRUCT_PATH_TOKEN_PREFIX: i32 = 32;
/// The needle appears somewhere in the path but at no word boundary.
const STRUCT_PATH_SUBSTRING: i32 = 28;
/// The needle's characters appear in order in the leaf, with gaps.
const STRUCT_FUZZY: i32 = 12;

/// Weight of the match-length ratio (needle length over stem length).
const RATIO_WEIGHT: i32 = 16;
/// Weight of the freshest recency band.
const RECENCY_MAX: i32 = 16;

/// Deductions for content shipped *with* software rather than written by
/// the person searching.
const PENALTY_ASSET_EXT: i32 = 18;
const PENALTY_VENDOR_DIR: i32 = 14;
const PENALTY_GENERATED_LEAF: i32 = 20;
/// Cap on the total penalty — uncapped, a vendored font would stack both
/// deductions and sink below a fuzzy match, a poor answer read as none.
const PENALTY_MAX: i32 = 28;

/// Path components a file may sit under before depth counts against it —
/// free so ordinary project layouts pay nothing.
const DEPTH_FREE: usize = 4;
const PENALTY_PER_LEVEL: i32 = 3;
const PENALTY_DEPTH_MAX: i32 = 15;

/// Top of the provider's scale, normalised against by the registry.
/// Derived from the parts: an exact filename match, modified today,
/// nothing held against it.
const MAX_SCORE: i32 = STRUCT_EXACT_LEAF + RATIO_WEIGHT + RECENCY_MAX;

/// Extensions almost never what someone means when they type a word —
/// shipped alongside software rather than authored.
const ASSET_EXTS: &[&str] = &[
    "ttf", "otf", "ttc", "woff", "woff2", "eot", "pfb", "bdf", "pcf", "so", "a", "o", "dll",
    "dylib", "obj", "pyc", "pyo", "class", "jar", "wasm", "rlib", "rmeta", "deb", "rpm",
    "appimage", "flatpak", "snap", "msi", "exe", "cab", "iso", "img", "dmg", "apk",
    // Game/engine data blobs.
    "grf", "pak", "package", "dat", "bin", "vpk", "wad", "bsp",
    // Icon/cursor blobs — photographic formats (png/jpg/webp) are excluded,
    // those are usually the user's own.
    "ico", "icns", "xpm", "cur", "bmp",
];

/// Directory names marking a tree as fetched/generated rather than
/// authored; the heaviest offenders are already gone via [`SKIP_DIRS`].
const VENDOR_DIRS: &[&str] = &[
    "vendor",
    "third_party",
    "thirdparty",
    "site-packages",
    "dist-packages",
    "bower_components",
    "jspm_packages",
    // Shipped data sets of unpacked games (OpenTTD's `baseset`, OpenRCT's `data`).
    "baseset",
    "basesets",
    // Wine/Proton prefixes: a whole Windows install under `~`.
    "drive_c",
    "generated",
    "autogen",
    "__generated__",
    "cmakefiles",
    "coverage",
    "htmlcov",
];

/// Lockfiles and minified bundles: machine-written, machine-read.
fn is_generated_leaf(leaf_lower: &str) -> bool {
    let ext = Path::new(leaf_lower).extension().and_then(|e| e.to_str());
    ext == Some("lock")
        || leaf_lower.ends_with(".min.js")
        || leaf_lower.ends_with(".min.mjs")
        || leaf_lower.ends_with(".min.css")
        || ext == Some("map")
        || matches!(
            leaf_lower,
            "package-lock.json" | "npm-shrinkwrap.json" | "go.sum" | "pnpm-lock.yaml"
        )
}

/// Everything a path's score depends on besides the needle. Precomputed by
/// the walker — a keystroke re-scores tens of thousands of entries and must
/// never tokenise or `stat` a file.
#[derive(Clone)]
pub(crate) struct NameFacts {
    /// The `~/…` display path, lowercased. Path-level tiers match here.
    lower: String,
    /// The final component of [`Self::lower`].
    leaf: Box<str>,
    /// Word ranges within [`Self::leaf`].
    tokens: Box<[(u16, u16)]>,
    /// Leaf length minus a recognised extension; equals leaf length when there is none.
    stem_len: u16,
    /// Provenance + depth deductions, summed once since both depend only on the path.
    penalty: i32,
    recency: i32,
}

impl NameFacts {
    /// `display` keeps its original case — camelCase is a word boundary and
    /// lowercasing destroys it.
    pub(crate) fn new(display: &str, modified: Option<SystemTime>) -> Self {
        let lower = display.to_lowercase();
        let leaf_lower = leaf_of(&lower).to_string();
        let leaf_orig = leaf_of(display);
        let stem_len = stem_len(&leaf_lower);
        let tokens = tokenize(leaf_orig, &leaf_lower);
        let penalty =
            vendor_penalty(&lower, &leaf_lower, stem_len).saturating_add(depth_penalty(&lower));
        Self {
            leaf: leaf_lower.into_boxed_str(),
            tokens: tokens.into_boxed_slice(),
            stem_len: u16::try_from(stem_len).unwrap_or(u16::MAX),
            penalty,
            recency: recency_score(modified),
            lower,
        }
    }
}

/// Deduction for how far a path is buried. See [`DEPTH_FREE`].
fn depth_penalty(lower_path: &str) -> i32 {
    let past = path_depth(lower_path).saturating_sub(DEPTH_FREE);
    let past = i32::try_from(past).unwrap_or(i32::MAX);
    past.saturating_mul(PENALTY_PER_LEVEL)
        .min(PENALTY_DEPTH_MAX)
}

fn leaf_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Length of `leaf` up to a recognised extension — short, alphanumeric,
/// something in front, so a version-numbered directory doesn't count as one.
fn stem_len(leaf_lower: &str) -> usize {
    let Some(dot) = leaf_lower.rfind('.') else {
        return leaf_lower.len();
    };
    if dot == 0 {
        return leaf_lower.len();
    }
    let Some(ext) = leaf_lower.get(dot.saturating_add(1)..) else {
        return leaf_lower.len();
    };
    if !ext.is_empty()
        && ext.len() <= 8
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        && !ext.chars().all(|c| c.is_ascii_digit())
    {
        dot
    } else {
        leaf_lower.len()
    }
}

fn extension_of(leaf_lower: &str, stem_len: usize) -> Option<&str> {
    (stem_len < leaf_lower.len())
        .then(|| leaf_lower.get(stem_len.saturating_add(1)..))
        .flatten()
}

/// Split a filename into words (byte ranges into the lowercased form).
/// Breaks on `-`, `_`, `.`, space and camelCase.
fn tokenize(leaf_orig: &str, leaf_lower: &str) -> Vec<(u16, u16)> {
    if leaf_lower.is_empty() || leaf_lower.len() > u16::MAX as usize {
        return Vec::new();
    }
    let camel_ok = leaf_orig.len() == leaf_lower.len();
    let orig = leaf_orig.as_bytes();
    let bytes = leaf_lower.as_bytes();
    let mut out: Vec<(u16, u16)> = Vec::new();
    let mut start: Option<usize> = None;
    for i in 0..=bytes.len() {
        let sep = i == bytes.len()
            || bytes
                .get(i)
                .is_some_and(|&b| matches!(b, b'-' | b'_' | b'.' | b' '));
        if sep {
            if let Some(s) = start.take() {
                out.push((
                    u16::try_from(s).unwrap_or(u16::MAX),
                    u16::try_from(i).unwrap_or(u16::MAX),
                ));
            }
            continue;
        }
        if start.is_none() {
            start = Some(i);
            continue;
        }
        // Mid-run camelCase/letter-digit boundary: close and reopen at `i`.
        if camel_ok && is_word_boundary(orig, i) {
            // `start` is always `Some` here — the `if start.is_none()` arm
            // above always `continue`s — but a stray `None` just means one
            // fewer token boundary rather than a panic.
            if let Some(s) = start.replace(i) {
                out.push((
                    u16::try_from(s).unwrap_or(u16::MAX),
                    u16::try_from(i).unwrap_or(u16::MAX),
                ));
            }
        }
    }
    out
}

/// Is `i` a camelCase (or letter↔digit) word start inside a run?
fn is_word_boundary(b: &[u8], i: usize) -> bool {
    if i == 0 || i >= b.len() {
        return false;
    }
    let Some(prev_idx) = i.checked_sub(1) else {
        return false;
    };
    let (Some(&prev), Some(&cur)) = (b.get(prev_idx), b.get(i)) else {
        return false;
    };
    // `openTtd` — lower/digit then upper.
    if cur.is_ascii_uppercase() && (prev.is_ascii_lowercase() || prev.is_ascii_digit()) {
        return true;
    }
    // `HTTPServer` — the last capital of a run that starts a new word.
    if cur.is_ascii_uppercase()
        && prev.is_ascii_uppercase()
        && b.get(i.saturating_add(1))
            .is_some_and(u8::is_ascii_lowercase)
    {
        return true;
    }
    // `report2026` / `2026report` — letter/digit transition.
    (cur.is_ascii_digit() && prev.is_ascii_alphabetic())
        || (cur.is_ascii_alphabetic() && prev.is_ascii_digit())
}

/// Deduction for content that ships with software. Bounded by
/// [`PENALTY_MAX`]; a match never disappears, it only sinks.
fn vendor_penalty(lower_path: &str, leaf_lower: &str, stem_len: usize) -> i32 {
    let mut p: i32 = 0;
    if let Some(ext) = extension_of(leaf_lower, stem_len)
        && ASSET_EXTS.contains(&ext)
    {
        p = p.saturating_add(PENALTY_ASSET_EXT);
    }
    if is_generated_leaf(leaf_lower) {
        p = p.saturating_add(PENALTY_GENERATED_LEAF);
    }
    // Parent components only — a directory *named* `vendor` is a fine answer.
    let parent = lower_path.rsplit_once('/').map_or("", |(head, _)| head);
    if parent.split('/').any(|c| VENDOR_DIRS.contains(&c)) {
        p = p.saturating_add(PENALTY_VENDOR_DIR);
    }
    p.min(PENALTY_MAX)
}

/// mtime → bounded score contribution, in bands so it's reproducible
/// without pinning a clock. Capped so recency never outvotes the name.
fn recency_score(modified: Option<SystemTime>) -> i32 {
    const DAY: u64 = 86_400;
    let Some(m) = modified else {
        return 0;
    };
    let Ok(age) = SystemTime::now().duration_since(m) else {
        // Dated in the future: "just touched" is the right reading.
        return RECENCY_MAX;
    };
    match age.as_secs() / DAY {
        0 => RECENCY_MAX,
        1..=6 => 13,
        7..=30 => 10,
        31..=90 => 7,
        91..=365 => 4,
        366..=1095 => 1,
        _ => 0,
    }
}

/// Score `facts` against an already-lowercased needle, or `None` if it does
/// not match. Monotone in the needle, which is what makes the narrowing
/// cache sound.
fn name_score(needle_lower: &str, facts: &NameFacts) -> Option<i32> {
    if needle_lower.is_empty() {
        return None;
    }
    let (structural, denom) = structural_match(needle_lower, facts)?;
    // Clamped at 1.0: "more than all of it" is still just all of it.
    let needle_len = u16::try_from(needle_lower.len()).unwrap_or(u16::MAX);
    let denom_len = u16::try_from(denom.max(needle_lower.len())).unwrap_or(u16::MAX);
    let ratio = (f32::from(needle_len) / f32::from(denom_len.max(1))).min(1.0);
    let ratio_weight = f32::from(u16::try_from(RATIO_WEIGHT).unwrap_or(u16::MAX));
    #[allow(
        clippy::cast_possible_truncation,
        reason = "ratio_bonus = round(ratio_weight * ratio) is mathematically bounded to \
                  0..=RATIO_WEIGHT (16), well inside i32"
    )]
    let ratio_bonus = (ratio_weight * ratio).round() as i32;
    let score = structural
        .saturating_add(ratio_bonus)
        .saturating_add(facts.recency)
        .saturating_sub(facts.penalty);
    // Floored at 1, not 0: a penalised match is a bad answer, not a hidden one.
    Some(score.clamp(1, MAX_SCORE))
}

/// Which structural tier the needle earned, and the length the match
/// ratio should be measured against.
fn structural_match(needle: &str, facts: &NameFacts) -> Option<(i32, usize)> {
    let leaf: &str = &facts.leaf;
    let stem_len = facts.stem_len as usize;
    // One substring scan decides whether any filename tier can apply —
    // the check that lets most of the index out cheaply.
    if leaf.contains(needle) {
        if leaf == needle {
            return Some((STRUCT_EXACT_LEAF, stem_len));
        }
        if stem_len == needle.len() && leaf.get(..stem_len) == Some(needle) {
            return Some((STRUCT_EXACT_STEM, stem_len));
        }
        return Some((leaf_tier(needle, leaf, stem_len, &facts.tokens), stem_len));
    }
    if facts.lower.contains(needle) {
        return Some(path_tier(needle, &facts.lower));
    }
    // Last resort: the characters in order, anywhere in the filename.
    // `ntes` → `notes.md`.
    fuzzy_subsequence(needle, leaf).then_some((STRUCT_FUZZY, stem_len))
}

/// Best word-boundary tier over every occurrence of `needle` in `leaf`:
/// `note` in `my-note-notes.md` lands mid-name once and on a whole word
/// once, and the better reading is the one that counts.
fn leaf_tier(needle: &str, leaf: &str, stem_len: usize, tokens: &[(u16, u16)]) -> i32 {
    let mut best = STRUCT_IN_TOKEN;
    for (at, _) in leaf.match_indices(needle) {
        let end = at.saturating_add(needle.len());
        let starts_word = at == 0 || tokens.iter().any(|&(s, _)| s as usize == at);
        if !starts_word {
            continue;
        }
        let ends_word =
            end == leaf.len() || end == stem_len || tokens.iter().any(|&(_, e)| e as usize == end);
        let tier = match (at == 0, ends_word) {
            (true, true) => STRUCT_FIRST_TOKEN,
            (false, true) => STRUCT_TOKEN,
            (true, false) => STRUCT_FIRST_TOKEN_PREFIX,
            (false, false) => STRUCT_TOKEN_PREFIX,
        };
        best = best.max(tier);
        if best == STRUCT_FIRST_TOKEN {
            break;
        }
    }
    best
}

/// Tier for a match that missed the filename but landed up the path.
/// Reached only once the path is known to contain the needle.
fn path_tier(needle: &str, lower_path: &str) -> (i32, usize) {
    let parent = lower_path.rsplit_once('/').map_or("", |(head, _)| head);
    let mut best = (STRUCT_PATH_SUBSTRING, lower_path.len());
    for comp in parent.split('/') {
        if comp == needle {
            return (STRUCT_PATH_COMPONENT, comp.len());
        }
        if !comp.contains(needle) {
            continue;
        }
        let tokens = tokenize(comp, comp);
        let starts_word = comp.starts_with(needle)
            || tokens.iter().any(|&(s, _)| {
                comp.get(s as usize..)
                    .is_some_and(|t| t.starts_with(needle))
            });
        if starts_word && best.0 < STRUCT_PATH_TOKEN_PREFIX {
            best = (STRUCT_PATH_TOKEN_PREFIX, comp.len());
        }
    }
    best
}

/// Number of path components — the tiebreak between two equal-score matches.
fn path_depth(display_lower: &str) -> usize {
    display_lower.bytes().filter(|b| *b == b'/').count()
}

fn fuzzy_subsequence(needle: &str, hay: &str) -> bool {
    let mut hi = hay.chars();
    'ch: for nc in needle.chars() {
        for hc in hi.by_ref() {
            if hc == nc {
                continue 'ch;
            }
        }
        return false;
    }
    true
}

/// Open a palette hit via [`crate::shell::doc_open`] when an installed app
/// claims the type; unclaimed types and directories fall back to
/// `xdg-open`, and a claimed type with no editor is reported, not silent.
fn open_search_hit(path: &Path, is_dir: bool, chrome: &Chrome) {
    use crate::shell::doc_open::{self, DocumentTarget};
    if is_dir {
        spawn_open(path, is_dir);
        return;
    }
    match doc_open::resolve(path) {
        DocumentTarget::App { .. } => {
            if let Err(e) = doc_open::open_in_workspace(path, chrome) {
                notify_open_failure(path, &e.user_message(), chrome);
            }
        }
        DocumentTarget::NoAppForDocument { ext } => {
            notify_open_failure(
                path,
                &format!("no app on this desktop opens .{ext} documents"),
                chrome,
            );
        }
        DocumentTarget::Unclaimed | DocumentTarget::Directory => spawn_open(path, is_dir),
    }
}

/// Report a failed palette open; shared with every other open surface.
fn notify_open_failure(path: &Path, detail: &str, chrome: &Chrome) {
    crate::shell::doc_open::report_open_failure(chrome, path, detail, Some("Files"));
}

/// Best-effort `xdg-open`, fire-and-forget so a slow child never blocks the palette.
fn spawn_open(path: &Path, _is_dir: bool) {
    let path = path.to_owned();
    thread::Builder::new()
        .name("qos-search-open".into())
        .spawn(move || {
            let _ = std::process::Command::new("xdg-open").arg(path).spawn();
        })
        .ok();
}

/// Build the name index. Breadth-first, because a single unpacked game is
/// tens of thousands of paths (`~/Spiele` alone walks to 31 168):
/// depth-first would spend [`MAX_ENTRIES`] on whichever branch it entered
/// first and exclude whole trees.
fn walk_home(home: &Path, index: &Arc<Mutex<Arc<NameIndex>>>) {
    let entries = walk_breadth_first(home, MAX_ENTRIES);
    // Construct all retained maps before touching the publication lock. A
    // palette request meanwhile just clones the prior immutable snapshot.
    let generation = NEXT_NAME_INDEX_GENERATION.fetch_add(1, Ordering::Relaxed);
    let next = Arc::new(NameIndex::from_entries(entries, generation));
    if let Ok(mut guard) = index.lock() {
        *guard = next;
    }
}

/// [`walk_home`]'s walk, with the budget as a parameter so a test can
/// exercise the exhaustion behaviour on a tree it can afford to build.
fn walk_breadth_first(home: &Path, budget: usize) -> Vec<NameEntry> {
    let mut entries: Vec<NameEntry> = Vec::new();
    let mut frontier: std::collections::VecDeque<(PathBuf, usize)> =
        std::collections::VecDeque::new();
    frontier.push_back((home.to_owned(), 0));

    'walk: while let Some((dir, depth)) = frontier.pop_front() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Sorted so a re-scan of an unchanged tree produces the same index.
        let mut children: Vec<std::fs::DirEntry> = rd.filter_map(Result::ok).collect();
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children {
            let path = child.path();
            if should_skip(&path, home) {
                continue;
            }
            // `file_type` is free on Linux (from readdir's `d_type`);
            // `metadata` is the one stat, taken here off the UI thread.
            let is_dir = child.file_type().is_ok_and(|t| t.is_dir());
            let modified = child.metadata().ok().and_then(|m| m.modified().ok());
            let next_depth = depth.saturating_add(1);
            if is_dir && next_depth < MAX_DEPTH {
                frontier.push_back((path.clone(), next_depth));
            }
            let display = display_path(home, &path);
            entries.push(NameEntry {
                facts: NameFacts::new(&display, modified),
                path,
                display,
                is_dir,
            });
            if entries.len() >= budget {
                break 'walk;
            }
            // Yield periodically so the walker doesn't pin a core.
            if entries.len().is_multiple_of(2_000) {
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
    entries
}

/// Does any pattern in [`SKIP_DIRS`] match this path, or is any component
/// hidden? A pattern containing `/` must match that many consecutive
/// components (`.config/gnome`, `Downloads/tmp`).
fn should_skip(path: &Path, home: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(home) else {
        return false;
    };
    let comps: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    for comp in &comps {
        if comp.starts_with('.') && comp != "." && comp != ".." {
            // Hidden by default; reachable via the file-explorer app instead.
            return true;
        }
    }
    SKIP_DIRS.iter().any(|pattern| {
        let want: Vec<&str> = pattern.split('/').collect();
        comps.windows(want.len()).any(|w| {
            w.iter()
                .zip(&want)
                .all(|(have, want)| have.as_str() == *want)
        })
    })
}

fn display_path(home: &Path, path: &Path) -> String {
    path.strip_prefix(home).map_or_else(
        |_| path.display().to_string(),
        |rel| format!("~/{}", rel.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Case-insensitive-by-construction extension check for test assertions,
    /// via `Path::extension` rather than a raw `ends_with(".ext")`.
    fn has_ext(name: &str, ext: &str) -> bool {
        Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
    }

    /// Facts for a path with no mtime; `aged_facts` covers the recency signal.
    fn facts(display: &str) -> NameFacts {
        NameFacts::new(display, None)
    }

    /// Facts for a path last modified `days` ago.
    fn aged_facts(display: &str, days: u64) -> NameFacts {
        let secs = days.saturating_mul(86_400).saturating_add(3_600);
        NameFacts::new(
            display,
            SystemTime::now().checked_sub(Duration::from_secs(secs)),
        )
    }

    #[test]
    fn name_score_prefers_the_leaf_over_the_path() {
        assert_eq!(
            name_score("todo", &facts("~/notes/todo.md")),
            Some(STRUCT_EXACT_STEM + RATIO_WEIGHT)
        );
        assert_eq!(
            name_score("todo", &facts("~/notes/day-todo-list.md")),
            Some(STRUCT_TOKEN + 5)
        );
        assert_eq!(
            name_score("todo", &facts("~/todo/notes.md")),
            Some(STRUCT_PATH_COMPONENT + RATIO_WEIGHT)
        );
        assert_eq!(name_score("xzq", &facts("~/notes/todo.md")), None);
    }

    #[test]
    fn a_document_outranks_the_font_assets_of_a_game() {
        const GAME: &str = "~/Spiele/openttd-jgrpp-0.72.3-linux-generic-amd64";
        let p = provider_with_index_aged(vec![
            (&format!("{GAME}/baseset/OpenTTD-Mono.ttf"), false, 50),
            (&format!("{GAME}/baseset/OpenTTD-Sans.ttf"), false, 50),
            (&format!("{GAME}/baseset/OpenTTD-Serif.ttf"), false, 50),
            (&format!("{GAME}/baseset/OpenTTD-Small.ttf"), false, 50),
            (&format!("{GAME}/baseset/openttd.grf"), false, 50),
            (&format!("{GAME}/openttd"), false, 50),
            ("~/Spiele/openttd-logo.png", false, 260),
            ("~/Documents/opening-hours.md", false, 3),
        ]);
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let out = p.query(ctx_for(
            "open",
            &[ResourceKind::File, ResourceKind::Folder],
            &st,
            &doc,
        ));

        let listing = || {
            out.iter()
                .map(|r| format!("{} ({})", r.title, r.score))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            out.first().map(|r| r.title.as_str()),
            Some("opening-hours.md"),
            "the user's own document must lead the game's assets: {:?}",
            listing()
        );
        let doc_score = out[0].score;
        for r in out.iter().filter(|r| has_ext(&r.title, "ttf")) {
            assert!(
                r.score < doc_score,
                "{} scored {} against the document's {doc_score}: {:?}",
                r.title,
                r.score,
                listing()
            );
        }
        // Below every non-asset hit, not merely the document.
        let first_font = out
            .iter()
            .position(|r| has_ext(&r.title, "ttf"))
            .expect("the fonts are still in the results");
        let last_non_asset = out
            .iter()
            .rposition(|r| !has_ext(&r.title, "ttf") && !has_ext(&r.title, "grf"))
            .expect("some non-asset hit");
        assert!(
            first_font > last_non_asset,
            "a font appears at {first_font}, above the last non-asset hit at \
             {last_non_asset}: {:?}",
            listing()
        );
        assert_eq!(
            out.iter().filter(|r| has_ext(&r.title, "ttf")).count(),
            4,
            "all four fonts must still be reachable: {:?}",
            listing()
        );
    }

    #[test]
    fn a_whole_word_beats_the_start_of_a_longer_word() {
        let whole = name_score("open", &facts("~/a/open.txt")).unwrap();
        let prefix = name_score("open", &facts("~/a/opening-notes.md")).unwrap();
        let inside = name_score("open", &facts("~/a/reopen.md")).unwrap();
        let other_word = name_score("open", &facts("~/a/xdg-open")).unwrap();
        assert!(
            whole > prefix && prefix > inside,
            "expected open.txt > opening-notes.md > reopen.md, got \
             {whole} / {prefix} / {inside}"
        );
        assert!(
            other_word > prefix,
            "`open` is the whole second word of `xdg-open` but only the start \
             of `opening`, so xdg-open ({other_word}) must beat \
             opening-notes.md ({prefix})"
        );
    }

    #[test]
    fn an_exact_filename_match_clearly_wins() {
        let exact = name_score("open", &facts("~/a/open.md")).unwrap();
        let long = name_score("open", &facts("~/a/opentelemetry-collector-config.md")).unwrap();
        assert!(
            exact >= long + 30,
            "an exact name match ({exact}) must clearly beat four characters \
             of a thirty-character name ({long}), not tie with it"
        );
    }

    #[test]
    fn tokenize_splits_on_separators_and_camel_case() {
        let words = |s: &str| {
            let lower = s.to_lowercase();
            tokenize(s, &lower)
                .into_iter()
                .map(|(a, b)| {
                    lower
                        .get(a as usize..b as usize)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(words("OpenTTD-Sans.ttf"), ["open", "ttd", "sans", "ttf"]);
        assert_eq!(words("my_notes file.md"), ["my", "notes", "file", "md"]);
        assert_eq!(words("HTTPServerLog"), ["http", "server", "log"]);
        assert_eq!(words("report2026final"), ["report", "2026", "final"]);
        assert_eq!(words("openapi.yaml"), ["openapi", "yaml"]);
    }

    /// A version-numbered directory must not lose its name to a bogus "extension".
    #[test]
    fn stem_length_ignores_a_version_number_masquerading_as_an_extension() {
        assert_eq!(stem_len("openttd-jgrpp-0.72.3-linux-generic-amd64"), 40);
        assert_eq!(stem_len("open.md"), 4);
        assert_eq!(stem_len("archive.tar.gz"), 11);
        assert_eq!(stem_len("noextension"), 11);
        assert_eq!(stem_len(".hidden"), 7);
    }

    #[test]
    fn vendored_content_sinks_but_stays_findable() {
        let mine = name_score("parser", &facts("~/proj/src/parser.rs")).unwrap();
        let theirs = name_score("parser", &facts("~/proj/vendor/pkg/parser.rs")).unwrap();
        assert!(
            mine > theirs,
            "the user's own parser.rs ({mine}) must outrank the vendored copy \
             ({theirs})"
        );

        let p = provider_with_index(vec![("~/proj/vendor/pkg/parser.rs", false)]);
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let out = p.query(ctx_for("parser.rs", &[ResourceKind::File], &st, &doc));
        assert_eq!(
            out.len(),
            1,
            "a penalised match must still be returned: {out:?}"
        );
        assert!(
            out[0].score >= 1,
            "the score floor keeps the row above the no-match threshold"
        );
    }

    #[test]
    fn recency_separates_two_files_with_the_same_name() {
        let fresh = name_score("report", &aged_facts("~/a/report.md", 0)).unwrap();
        let stale = name_score("report", &aged_facts("~/b/report.md", 900)).unwrap();
        assert!(
            fresh > stale,
            "the file touched today ({fresh}) must outrank the one from three \
             years ago ({stale})"
        );
    }

    /// A freshly unpacked archive cannot displace an exact old match.
    #[test]
    fn recency_cannot_outvote_what_the_name_says() {
        let old_exact = name_score("notes", &aged_facts("~/a/notes.md", 900)).unwrap();
        let new_weak = name_score(
            "notes",
            &aged_facts("~/b/my-meeting-notes-and-agenda-draft.md", 0),
        )
        .unwrap();
        assert!(
            old_exact > new_weak,
            "a three-year-old notes.md ({old_exact}) must still beat a \
             freshly-touched my-meeting-notes-and-agenda-draft.md ({new_weak})"
        );
    }

    /// Proven by scoring a path that does not exist on disk.
    #[test]
    fn the_recency_band_comes_from_the_index_not_the_disk() {
        let ghost = "~/definitely/not/on/this/disk/report.md";
        assert!(!Path::new("/home/tester/definitely").exists());
        let fresh = name_score("report", &aged_facts(ghost, 0)).unwrap();
        let none = name_score("report", &facts(ghost)).unwrap();
        assert_eq!(
            fresh - none,
            RECENCY_MAX,
            "a today mtime carried in the index must contribute the full \
             recency band"
        );
    }

    /// The registry divides by this; outgrowing it silently takes over the palette.
    #[test]
    fn max_score_is_the_honest_ceiling() {
        assert_eq!(
            name_score("open", &aged_facts("~/open", 0)),
            Some(MAX_SCORE),
            "an exact filename match touched today must land exactly on the \
             declared ceiling"
        );
        let p = provider_with_index(vec![("~/open", false)]);
        assert_eq!(p.max_score(), MAX_SCORE);

        for display in [
            "~/open",
            "~/open.md",
            "~/OpenTTD-Sans.ttf",
            "~/a/vendor/b/baseset/open.min.js",
            "~/open/open/open/open-open_open.open",
            "~/o/p/e/n/deeply/nested/thing.txt",
        ] {
            for needle in ["o", "open", "open.md", "opn", "openopenopen"] {
                for days in [0u64, 5, 400] {
                    if let Some(s) = name_score(needle, &aged_facts(display, days)) {
                        assert!(
                            (1..=MAX_SCORE).contains(&s),
                            "{needle:?} on {display:?} at {days}d scored {s}, \
                             outside 1..={MAX_SCORE}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn skip_dirs_hits_common_bloat() {
        let home = Path::new("/home/x");
        assert!(should_skip(Path::new("/home/x/proj/node_modules"), home));
        assert!(should_skip(Path::new("/home/x/proj/target/debug"), home));
        assert!(should_skip(Path::new("/home/x/.cache/foo"), home));
        assert!(!should_skip(Path::new("/home/x/proj/src/lib.rs"), home));
    }

    /// A pattern with a `/` must match consecutive components, not just one.
    #[test]
    fn multi_component_skip_patterns_match() {
        let home = Path::new("/home/x");
        assert!(
            SKIP_DIRS.contains(&"Downloads/tmp"),
            "fixture precondition: the list still carries a multi-component \
             pattern"
        );
        assert!(should_skip(Path::new("/home/x/Downloads/tmp/a.iso"), home));
        assert!(should_skip(
            Path::new("/home/x/go/pkg/mod/rsc.io/quote"),
            home
        ));
        assert!(should_skip(
            Path::new("/home/x/Android/Sdk/platforms"),
            home
        ));
        assert!(!should_skip(Path::new("/home/x/Downloads/keep.iso"), home));
        assert!(!should_skip(Path::new("/home/x/go/src/mod/thing.go"), home));
        assert!(!should_skip(Path::new("/home/x/tmp/Downloads/a.iso"), home));
        assert!(!should_skip(Path::new("/home/x/Android/notes.md"), home));
    }

    #[test]
    fn depth_counts_against_a_buried_path() {
        assert_eq!(depth_penalty("~/report.md"), 0);
        assert_eq!(depth_penalty("~/dev/proj/src/lib.rs"), 0);
        let shallow = name_score("open", &facts("~/Dokumente/opening.md")).unwrap();
        let buried = name_score(
            "open",
            &facts("~/Spiele/factorio/data/base/sound/open-close/silo-open.ogg"),
        )
        .unwrap();
        assert!(
            shallow > buried,
            "a document two levels down ({shallow}) must outrank a game sound \
             effect seven levels down ({buried})"
        );
        assert_eq!(depth_penalty("~/a/b/c/d/e/f/g/h/i/j/k"), PENALTY_DEPTH_MAX);
    }

    /// Breadth-first spends the budget on the shallowest paths everywhere.
    #[test]
    fn the_walk_indexes_every_tree_before_it_runs_out() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = home.path();
        std::fs::create_dir_all(root.join("Aaa-huge")).unwrap();
        let mut deep = root.join("Aaa-huge");
        for level in 0..6 {
            deep = deep.join(format!("level{level}"));
            std::fs::create_dir_all(&deep).unwrap();
            for i in 0..40 {
                std::fs::write(deep.join(format!("filler-{i}.bin")), b"x").unwrap();
            }
        }
        for tree in ["Documents", "Notes", "Projects", "Pictures"] {
            std::fs::create_dir_all(root.join(tree)).unwrap();
            std::fs::write(root.join(tree).join("target.md"), b"x").unwrap();
        }

        let entries = walk_breadth_first(root, 60);
        assert_eq!(entries.len(), 60, "the budget must be spent in full");
        for tree in ["Documents", "Notes", "Projects", "Pictures"] {
            assert!(
                entries
                    .iter()
                    .any(|e| e.display == format!("~/{tree}/target.md")),
                "~/{tree}/target.md missing — the budget went to one branch \
                 again. Indexed: {:?}",
                entries.iter().map(|e| &e.display).collect::<Vec<_>>()
            );
        }
    }

    fn provider_with_index(entries: Vec<(&str, bool)>) -> FilesProvider {
        provider_from_entries(
            entries
                .into_iter()
                .map(|(display, is_dir)| NameEntry {
                    path: PathBuf::from(display.replace('~', "/home/tester")),
                    facts: NameFacts::new(display, None),
                    display: display.to_string(),
                    is_dir,
                })
                .collect(),
        )
    }

    fn provider_with_index_aged(entries: Vec<(&str, bool, u64)>) -> FilesProvider {
        provider_from_entries(
            entries
                .into_iter()
                .map(|(display, is_dir, days)| NameEntry {
                    path: PathBuf::from(display.replace('~', "/home/tester")),
                    facts: NameFacts::new(
                        display,
                        SystemTime::now().checked_sub(Duration::from_secs(
                            days.saturating_mul(86_400).saturating_add(3_600),
                        )),
                    ),
                    display: display.to_string(),
                    is_dir,
                })
                .collect(),
        )
    }

    fn provider_from_entries(entries: Vec<NameEntry>) -> FilesProvider {
        let home = PathBuf::from("/home/tester");
        let (rank_mailbox, rank_notify, active_rank_workers, peak_rank_workers) =
            FilesProvider::rank_worker();
        FilesProvider {
            home,
            name_index: Arc::new(Mutex::new(Arc::new(NameIndex::from_entries(entries, 1)))),
            tantivy: None,
            last_scanned: AtomicUsize::new(0),
            ranked_queries: Mutex::new(RankedQueryCache::default()),
            ranked_query_epoch: AtomicU64::new(0),
            rank_mailbox,
            rank_notify,
            active_rank_workers,
            peak_rank_workers,
        }
    }

    fn ctx_for<'a>(
        needle: &'a str,
        wanted: &'a [ResourceKind],
        chrome: &'a ChromeState,
        doc: &'a automerge::AutoCommit,
    ) -> SearchContext<'a> {
        SearchContext {
            needle,
            wanted,
            chrome,
            doc,
            doc_root: &automerge::ROOT,
        }
    }

    fn empty_chrome() -> crate::shell::chrome_state::Chrome {
        use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
        let handle: WorkspaceTreeHandle =
            std::sync::Arc::new(parking_lot::RwLock::new(WorkspaceTree::default()));
        ChromeState::new_with_handle(handle)
    }

    fn eventually_page(mut query: impl FnMut() -> SearchPage) -> SearchPage {
        for _ in 0..2_000 {
            let page = query();
            if !page.results.is_empty() || page.exhausted || page.stale_cursor {
                return page;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("retained ranked query did not finish within two seconds");
    }

    /// The best matches win regardless of where they sit in the index.
    #[test]
    fn the_best_matches_win_regardless_of_walk_position() {
        let mut entries: Vec<(String, bool)> = (0..50)
            .map(|i| (format!("~/report/notes/entry-{i:03}.md"), false))
            .collect();
        entries.push(("~/archive/report-final.md".to_string(), false));
        let owned: Vec<(&str, bool)> = entries.iter().map(|(s, d)| (s.as_str(), *d)).collect();
        let p = provider_with_index(owned);

        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let out = p.query(ctx_for(
            "report",
            &[ResourceKind::File, ResourceKind::Folder],
            &st,
            &doc,
        ));
        assert_eq!(
            out.first().map(|r| r.title.as_str()),
            Some("report-final.md"),
            "the leaf-name match must lead the fifty path-only matches it \
             sits behind in the index: {:?}",
            out.iter().take(3).map(|r| &r.title).collect::<Vec<_>>()
        );
    }

    #[test]
    fn equal_scores_prefer_the_shallower_path() {
        // Same filename/age/penalty, so the scores really tie and depth is
        // the only tiebreak left. Both sit inside `DEPTH_FREE`, where depth
        // costs nothing — a deeper path would separate by penalty instead.
        let p = provider_with_index(vec![
            ("~/src/project/module/todo.md", false),
            ("~/src/todo.md", false),
        ]);
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let out = p.query(ctx_for("todo", &[ResourceKind::File], &st, &doc));
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].score, out[1].score,
            "the fixture must actually tie on score, or the depth tiebreak \
             is not what is being tested: {out:?}"
        );
        assert_eq!(out[0].id, "/home/tester/src/todo.md");
    }

    /// Compatibility queries use the same retained, bounded page path as
    /// the palette; no character may revive the former corpus sweep.
    #[test]
    fn typing_a_word_narrows_instead_of_re_sweeping() {
        let paths: Vec<String> = (0..MAX_ENTRIES)
            .map(|i| format!("~/Development/project-{i}/{}.rs", WORDS[i % WORDS.len()]))
            .collect();
        let entries: Vec<(&str, bool)> = paths.iter().map(|s| (s.as_str(), false)).collect();
        let p = provider_with_index(entries);
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();

        let word = "component";
        let mut scanned = Vec::new();
        for end in 1..=word.len() {
            let prefix = word.get(..end).unwrap_or_default();
            p.query(ctx_for(prefix, &[ResourceKind::File], &st, &doc));
            scanned.push(p.last_scanned.load(Ordering::Relaxed));
        }

        for (i, w) in scanned.iter().enumerate() {
            assert!(
                *w <= SearchBudget::PALETTE.max_work,
                "character {i} examined {w} rows; compatibility must remain \
                 within the page work ceiling: {scanned:?}"
            );
        }
    }

    const WORDS: &[&str] = &[
        "component",
        "compose",
        "common",
        "config",
        "codec",
        "buffer",
        "render",
        "layout",
        "widget",
        "theme",
        "search",
        "palette",
        "index",
        "walker",
    ];

    /// Extending a needle must return what a cold query for it would.
    #[test]
    fn narrowing_returns_what_a_cold_query_would() {
        let p = provider_with_index(vec![
            ("~/notes/todo.md", false),
            ("~/todo/notes.md", false),
            ("~/Development/day-todo-list.md", false),
            ("~/tmp/tdo.md", false),
            ("~/unrelated/readme.md", false),
        ]);
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let kinds = [ResourceKind::File, ResourceKind::Folder];

        for prefix in ["t", "to", "tod", "todo"] {
            let mut warm = Vec::new();
            for end in 1..=prefix.len() {
                let sub = prefix.get(..end).unwrap_or_default();
                warm = p.query(ctx_for(sub, &kinds, &st, &doc));
            }
            let cold = p.query(ctx_for(prefix, &kinds, &st, &doc));

            let ids = |v: &[SearchResult]| v.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
            assert_eq!(
                ids(&warm),
                ids(&cold),
                "typing into {prefix:?} produced a different result set than \
                 querying it outright"
            );
        }
    }

    #[test]
    fn a_reindex_invalidates_the_narrowing_cache() {
        let p = provider_with_index(vec![("~/alpha.md", false), ("~/alpha-two.md", false)]);
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        assert_eq!(
            p.query(ctx_for("alpha", &[ResourceKind::File], &st, &doc))
                .len(),
            2
        );

        {
            let mut guard = p.name_index.lock().unwrap();
            let generation = guard.generation.wrapping_add(1);
            *guard = Arc::new(NameIndex::from_entries(
                vec![NameEntry {
                    path: PathBuf::from("/home/tester/alphabet.md"),
                    display: "~/alphabet.md".into(),
                    facts: NameFacts::new("~/alphabet.md", None),
                    is_dir: false,
                }],
                generation,
            ));
        }
        let out = p.query(ctx_for("alphab", &[ResourceKind::File], &st, &doc));
        assert_eq!(
            out.len(),
            1,
            "the post-rescan entry must be found, not looked up through \
             stale indices: {out:?}"
        );
        assert_eq!(out[0].title, "alphabet.md");
    }

    /// Paging an enormous retained corpus must not quietly turn the first
    /// (or a late) palette page back into a whole-index scan.  This measures
    /// inspected entries, not elapsed time, so it remains meaningful under
    /// sanitizers and on slow CI workers.
    #[test]
    fn pages_a_hundred_thousand_retained_entries_with_fixed_work() {
        let mut paths = (0..100_000)
            .map(|i| format!("~/archive/{i:06}-target-needle.md"))
            .collect::<Vec<_>>();
        // The exact filename is deliberately last: page quality must come
        // from the retained exact-name index, not traversal position.
        paths.push("~/archive/target-needle.md".into());
        let entries = paths.iter().map(|path| NameEntry {
            path: PathBuf::from(path.replace('~', "/home/tester")),
            display: path.clone(),
            facts: NameFacts::new(path, None),
            is_dir: false,
        });
        let p = provider_from_entries(entries.collect());
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let budget = SearchBudget {
            max_results: 17,
            max_bytes: 8_192,
            max_work: 113,
        };
        let first = eventually_page(|| {
            p.query_page(
                ctx_for("target-needle", &[ResourceKind::File], &st, &doc),
                None,
                budget,
            )
        });
        assert!(p.last_scanned.load(Ordering::Relaxed) <= 113);
        assert!(first.results.len() <= 17);
        assert_eq!(
            first.results.first().map(|row| row.title.as_str()),
            Some("target-needle.md"),
            "an exact filename beyond 100k earlier candidates must lead"
        );
    }

    #[test]
    fn late_non_exact_pages_keep_the_same_work_ceiling() {
        let paths = (0..100_001)
            .map(|i| format!("~/archive/{i:06}-needle.md"))
            .collect::<Vec<_>>();
        let entries = paths.iter().map(|path| NameEntry {
            path: PathBuf::from(path.replace('~', "/home/tester")),
            display: path.clone(),
            facts: NameFacts::new(path, None),
            is_dir: false,
        });
        let p = provider_from_entries(entries.collect());
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let budget = SearchBudget {
            max_results: 17,
            max_bytes: 8_192,
            max_work: 113,
        };
        let first = eventually_page(|| {
            p.query_page(
                ctx_for("needle", &[ResourceKind::File], &st, &doc),
                None,
                budget,
            )
        });
        let mut cursor = first.next_cursor;
        for _ in 0..700 {
            let page = p.query_page(
                ctx_for("needle", &[ResourceKind::File], &st, &doc),
                cursor.as_ref(),
                budget,
            );
            assert!(!page.stale_cursor);
            assert!(p.last_scanned.load(Ordering::Relaxed) <= 113);
            cursor = page.next_cursor;
        }
        assert!(cursor.is_some(), "fixture must reach a genuine late page");
    }

    #[test]
    fn late_leaf_prefix_outranks_a_hundred_thousand_path_substrings() {
        let mut paths = (0..100_000)
            .map(|i| format!("~/report/archive/item-{i:06}.md"))
            .collect::<Vec<_>>();
        paths.push("~/later/report-final.md".into());
        let entries = paths.iter().map(|path| NameEntry {
            path: PathBuf::from(path.replace('~', "/home/tester")),
            display: path.clone(),
            facts: NameFacts::new(path, None),
            is_dir: false,
        });
        let p = provider_from_entries(entries.collect());
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let page = eventually_page(|| {
            p.query_page(
                ctx_for("report", &[ResourceKind::File], &st, &doc),
                None,
                SearchBudget {
                    max_results: 8,
                    max_bytes: 8_192,
                    max_work: 32,
                },
            )
        });
        assert_eq!(p.last_scanned.load(Ordering::Relaxed), 1);
        assert_eq!(page.results[0].title, "report-final.md");
    }

    #[test]
    fn late_leaf_substring_uses_retained_trigram_postings() {
        let mut paths = (0..100_000)
            .map(|i| format!("~/lunar/archive/item-{i:06}.md"))
            .collect::<Vec<_>>();
        paths.push("~/later/monthlylunarlog.md".into());
        let entries = paths.iter().map(|path| NameEntry {
            path: PathBuf::from(path.replace('~', "/home/tester")),
            display: path.clone(),
            facts: NameFacts::new(path, None),
            is_dir: false,
        });
        let p = provider_from_entries(entries.collect());
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let page = eventually_page(|| {
            p.query_page(
                ctx_for("lunar", &[ResourceKind::File], &st, &doc),
                None,
                SearchBudget {
                    max_results: 8,
                    max_bytes: 8_192,
                    max_work: 32,
                },
            )
        });
        assert_eq!(p.last_scanned.load(Ordering::Relaxed), 1);
        assert_eq!(page.results[0].title, "monthlylunarlog.md");
    }

    #[test]
    fn late_fuzzy_hit_uses_a_retained_character_bucket() {
        let mut paths = (0..100_000)
            .map(|i| format!("~/archive/item-{i:06}.md"))
            .collect::<Vec<_>>();
        paths.push("~/later/monthlylunarlog.md".into());
        let entries = paths.iter().map(|path| NameEntry {
            path: PathBuf::from(path.replace('~', "/home/tester")),
            display: path.clone(),
            facts: NameFacts::new(path, None),
            is_dir: false,
        });
        let p = provider_from_entries(entries.collect());
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let page = eventually_page(|| {
            p.query_page(
                ctx_for("mnlr", &[ResourceKind::File], &st, &doc),
                None,
                SearchBudget {
                    max_results: 8,
                    max_bytes: 8_192,
                    max_work: 32,
                },
            )
        });
        assert_eq!(p.last_scanned.load(Ordering::Relaxed), 1);
        assert_eq!(page.results[0].title, "monthlylunarlog.md");
    }

    #[test]
    fn one_character_bucket_is_preranked_before_the_palette_opens() {
        let mut paths = (0..100_000)
            .map(|i| format!("~/archive/another-item-{i:06}.md"))
            .collect::<Vec<_>>();
        paths.push("~/later/zreport.md".into());
        let entries = paths.iter().map(|path| NameEntry {
            path: PathBuf::from(path.replace('~', "/home/tester")),
            display: path.clone(),
            facts: NameFacts::new(path, None),
            is_dir: false,
        });
        let p = provider_from_entries(entries.collect());
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let page = p.query_page(
            ctx_for("r", &[ResourceKind::File], &st, &doc),
            None,
            SearchBudget {
                max_results: 8,
                max_bytes: 8_192,
                max_work: 32,
            },
        );
        assert_eq!(p.last_scanned.load(Ordering::Relaxed), 32);
        assert_eq!(page.results[0].title, "zreport.md");
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the concurrent-search regression retains its shared state guards until stale-plan assertions complete"
    )]
    #[test]
    fn rapid_broad_edits_keep_one_worker_and_never_publish_a_stale_plan() {
        let mut paths = (0..100_000)
            .map(|i| format!("~/archive/{i:06}-needlestick.md"))
            .collect::<Vec<_>>();
        paths.push("~/later/needlestick-current.md".into());
        let entries = paths.iter().map(|path| NameEntry {
            path: PathBuf::from(path.replace('~', "/home/tester")),
            display: path.clone(),
            facts: NameFacts::new(path, None),
            is_dir: false,
        });
        let p = provider_from_entries(entries.collect());
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let budget = SearchBudget {
            max_results: 8,
            max_bytes: 8_192,
            max_work: 32,
        };

        // Each call only takes retained posting handles. The rank worker may
        // do candidate work off-thread, but no keypress copies/scans the
        // 100k-entry corpus on the compositor path.
        for needle in ["need", "needs", "needles", "needlestic", "needlestick"] {
            let page = p.query_page(
                ctx_for(needle, &[ResourceKind::File], &st, &doc),
                None,
                budget,
            );
            assert!(page.results.is_empty());
            assert_eq!(p.last_scanned.load(Ordering::Relaxed), 0);
            assert!(p.active_rank_workers.load(Ordering::Relaxed) <= 1);
        }
        let page = eventually_page(|| {
            p.query_page(
                ctx_for("needlestick", &[ResourceKind::File], &st, &doc),
                None,
                budget,
            )
        });
        assert_eq!(page.results[0].title, "needlestick-current.md");
        assert_eq!(p.peak_rank_workers.load(Ordering::Relaxed), 1);
        let cache = p.ranked_queries.lock().unwrap();
        assert!(cache.ready.iter().all(|plan| plan.needle == "needlestick"));
        assert!(
            cache
                .ready
                .iter()
                .map(|plan| plan.positions.len())
                .sum::<usize>()
                <= RANKED_QUERY_CACHE_MAX_POSITIONS
        );
    }

    #[test]
    fn path_substring_uses_a_retained_path_trigram_without_a_corpus_copy() {
        let mut paths = (0..100_000)
            .map(|i| format!("~/very-long-project-{i:06}/unrelated.md"))
            .collect::<Vec<_>>();
        paths.push("~/later/ordinary.md".into());
        let entries = paths.iter().map(|path| NameEntry {
            path: PathBuf::from(path.replace('~', "/home/tester")),
            display: path.clone(),
            facts: NameFacts::new(path, None),
            is_dir: false,
        });
        let p = provider_from_entries(entries.collect());
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let page = eventually_page(|| {
            p.query_page(
                ctx_for("project-099", &[ResourceKind::File], &st, &doc),
                None,
                SearchBudget {
                    max_results: 8,
                    max_bytes: 8_192,
                    max_work: 32,
                },
            )
        });
        assert!(
            page.results
                .iter()
                .all(|row| row.id.contains("project-099"))
        );
        assert!(p.last_scanned.load(Ordering::Relaxed) <= 32);
    }

    #[test]
    fn rejects_a_cursor_after_the_retained_index_changes() {
        let p = provider_with_index(vec![("~/one/needle.md", false), ("~/two/needle.md", false)]);
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let page = eventually_page(|| {
            p.query_page(
                ctx_for("needle.md", &[ResourceKind::File], &st, &doc),
                None,
                SearchBudget {
                    max_results: 1,
                    max_bytes: 8_192,
                    max_work: 1,
                },
            )
        });
        let cursor = page.next_cursor.expect("a continuation page");
        {
            let mut publication = p.name_index.lock().unwrap();
            *publication = Arc::new(NameIndex::from_entries(
                publication.entries.as_ref().clone(),
                2,
            ));
        }
        let stale = p.query_page(
            ctx_for("needle.md", &[ResourceKind::File], &st, &doc),
            Some(&cursor),
            SearchBudget::PALETTE,
        );
        assert!(stale.stale_cursor);
        assert!(stale.results.is_empty());
    }

    #[test]
    fn byte_limited_page_does_not_skip_the_first_unemitted_match() {
        let huge = format!("~/{}-needle.md", "x".repeat(4_096));
        let p = provider_with_index(vec![("~/a-needle.md", false), (&huge, false)]);
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let first = p.query_page(
            ctx_for("needle", &[ResourceKind::File], &st, &doc),
            None,
            SearchBudget {
                max_results: 10,
                max_bytes: 512,
                max_work: 10,
            },
        );
        assert_eq!(first.results.len(), 1);
        assert!(first.budget_exhausted);
        let retry = p.query_page(
            ctx_for("needle", &[ResourceKind::File], &st, &doc),
            first.next_cursor.as_ref(),
            SearchBudget {
                max_results: 10,
                max_bytes: 16_384,
                max_work: 10,
            },
        );
        assert_eq!(retry.results.len(), 1, "the oversized match was skipped");
        assert_eq!(
            retry.results[0].title.len(),
            huge.rsplit('/').next().unwrap().len()
        );
    }

    #[test]
    fn fuzzy_subsequence_true_and_false() {
        assert!(fuzzy_subsequence("ntes", "notes"));
        assert!(fuzzy_subsequence("tdo", "todo.md"));
        assert!(!fuzzy_subsequence("xz", "notes"));
    }
}
