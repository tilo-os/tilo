//! Files provider — searches the home directory for files and directories.
//!
//! Two backends: an in-memory index of the names under `~`, and a
//! persistent content index (see [`super::super::files_index`]) over file
//! contents at `~/.cache/tilo/files-index`, rebuilt incrementally in the
//! background. Neither starts at session construction — the first palette
//! query (or an explicit [`FilesProvider::rescan`]) opens the index and
//! kicks the walk, so `OverlayHost` boot does not pay for file search.
//! After that first walk an inotify watch keeps the name index current, one
//! record per changed path, so a file written into `~/Documents` is findable
//! without a manual rescan and without re-indexing every other name.
//! Enter routes a hit through [`crate::shell::doc_open`], which resolves the
//! extension to the installed package that opens it.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard};
use std::thread;
use std::time::{Duration, SystemTime};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::shell::chrome_state::Chrome;
use crate::shell::search::action_api::ActionError;
use crate::shell::search::{
    ResourceKind, SearchBudget, SearchContext, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};
use tilo_search::{Document, FieldWeight, MatchTier, Matching, SearchIndex, Signals};

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
/// Folders a person actually writes into. Watched recursively after the
/// first walk so a new file is searchable without waiting for `rescan`.
const WATCH_TREES: &[&str] = &[
    "Documents",
    "Dokumente",
    "Documentos",
    "Desktop",
    "Schreibtisch",
    "Downloads",
    "Pictures",
    "Bilder",
    "Music",
    "Musik",
    "Videos",
];
const WATCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// A publication gets a never-reused revision, so a cursor from before a
/// rescan is stale rather than read against a different corpus.
static NEXT_NAME_INDEX_GENERATION: AtomicU64 = AtomicU64::new(1);

/// A watched batch of at least this many paths that is also one in
/// [`BULK_CHANGE_FRACTION`] of the index is rebuilt beside the published
/// index and swapped in, as a walk is. Applied in place, a batch that large
/// would make the index rebuild anyway, holding off every query meanwhile.
const BULK_CHANGE_MIN: usize = 256;
const BULK_CHANGE_FRACTION: usize = 8;
/// When a name lands where its neighbours' keys leave no room, a widening
/// window of neighbours is renumbered until they sit at least this far apart,
/// so the next names added there fit without renumbering again.
const RELABEL_GAP: u64 = 1 << 20;

/// The walked names, searchable. Built by the background walker and kept
/// current by an inotify watch on well-known user folders, which upserts and
/// removes single records rather than rebuilding the index.
#[derive(Default)]
struct NameIndex {
    /// Sorted by [`name_order`]: shallow paths first, then by path. Each
    /// entry's key in [`SearchIndex`] ascends with its position, because the
    /// index orders equally scored hits by key. Keys are spread over the
    /// whole `u64` range, so a name added later takes a key between its
    /// neighbours' and nothing else is re-indexed.
    entries: Vec<(u64, NameEntry)>,
    index: SearchIndex,
    generation: u64,
}

impl NameIndex {
    fn from_entries(mut entries: Vec<NameEntry>, generation: u64) -> Self {
        #[cfg(test)]
        tests::NAME_INDEX_BUILDS.with(|count| count.set(count.get().saturating_add(1)));
        entries.sort_by(name_order);
        let step = key_step(u64::MAX, entries.len());
        let entries: Vec<(u64, NameEntry)> = (1u64..)
            .map(|at| at.saturating_mul(step))
            .zip(entries)
            .collect();
        let mut index = SearchIndex::new(Matching {
            spanning: false,
            typos: true,
        })
        .with_ranks(name_rank);
        index.replace_all(
            entries
                .iter()
                .map(|(key, entry)| name_document(*key, entry)),
        );
        if let Err(error) = index.commit() {
            tracing::error!(%error, "could not index home directory names");
        }
        Self {
            entries,
            index,
            generation,
        }
    }

    fn entries(&self) -> impl Iterator<Item = &NameEntry> {
        self.entries.iter().map(|(_, entry)| entry)
    }

    /// The entry indexed under `key`.
    fn entry(&self, key: u64) -> Option<&NameEntry> {
        let at = self
            .entries
            .binary_search_by_key(&key, |(key, _)| *key)
            .ok()?;
        self.entries.get(at).map(|(_, entry)| entry)
    }

    /// Fold watched paths in place: each added, changed or removed name is
    /// one upsert or removal, then one incremental commit. Returns whether
    /// anything changed, and so whether a new generation was published.
    fn apply(&mut self, observed: &[Observed]) -> bool {
        let mut changes = Changes::default();
        for change in observed {
            fold_change(&mut self.entries, change, &mut changes);
        }
        if changes.stale.is_empty() && changes.touched.is_empty() {
            return false;
        }
        for key in changes.stale {
            // A key that renumbering handed to another entry is overwritten
            // by that entry's upsert below instead.
            if self.entry(key).is_none() {
                self.index.remove(key);
            }
        }
        changes.touched.sort_unstable();
        changes.touched.dedup();
        for (lower, path) in &changes.touched {
            let Ok(at) = position(&self.entries, lower, path) else {
                continue;
            };
            if let Some((key, entry)) = self.entries.get(at) {
                self.index.upsert(name_document(*key, entry));
            }
        }
        if let Err(error) = self.index.commit() {
            tracing::error!(%error, "could not index changed home directory names");
        }
        self.generation = NEXT_NAME_INDEX_GENERATION.fetch_add(1, Ordering::Relaxed);
        #[cfg(test)]
        tests::NAME_INDEX_UPDATES.with(|count| count.set(count.get().saturating_add(1)));
        true
    }
}

/// The record a name is found by: the leaf, its stem, the whole path and
/// each parent directory, with recency less penalties as its boost.
fn name_document(key: u64, entry: &NameEntry) -> Document {
    let facts = &entry.facts;
    let parents = facts.lower.rsplit_once('/').map_or("", |(head, _)| head);
    parents
        .split('/')
        .fold(
            Document::new(key)
                .field(facts.leaf.as_ref(), LEAF)
                .field(
                    facts
                        .leaf
                        .get(..usize::from(facts.stem_len))
                        .unwrap_or_default(),
                    STEM,
                )
                .field(facts.lower.as_str(), PATH),
            |document, component| document.field(component, PARENT),
        )
        .signals(
            facts.recency.saturating_sub(facts.penalty),
            u32::from(facts.stem_len),
        )
}

/// Where a name sorts: by depth, then by lowercased display path, then by
/// the path itself, so two names differing only in case keep one order.
fn order_key(entry: &NameEntry) -> (usize, &str, &Path) {
    (
        path_depth(&entry.facts.lower),
        entry.facts.lower.as_str(),
        entry.path.as_path(),
    )
}

fn name_order(a: &NameEntry, b: &NameEntry) -> std::cmp::Ordering {
    order_key(a).cmp(&order_key(b))
}

/// The position of the name with this lowercased display path and path, or
/// the position it would be inserted at.
fn position(entries: &[(u64, NameEntry)], lower: &str, path: &Path) -> Result<usize, usize> {
    let probe = (path_depth(lower), lower, path);
    entries.binary_search_by(|(_, entry)| order_key(entry).cmp(&probe))
}

/// What a watched path is now.
enum Observed {
    Present(NameEntry),
    /// Gone, with the lowercased display path that locates its subtree.
    Gone {
        path: PathBuf,
        lower: String,
    },
}

/// What one watched batch did to the records. Applied after the whole
/// batch, so a key renumbering handed to another entry is never removed.
#[derive(Default)]
struct Changes {
    /// Keys a record may still be indexed under whose entry left or was
    /// renumbered.
    stale: Vec<u64>,
    /// Entries to (re)index, by lowercased display path and path.
    touched: Vec<(String, PathBuf)>,
}

impl Changes {
    fn touch(&mut self, entry: &NameEntry) {
        self.touched
            .push((entry.facts.lower.clone(), entry.path.clone()));
    }
}

fn same_record(a: &NameEntry, b: &NameEntry) -> bool {
    a.display == b.display && a.facts == b.facts && a.is_dir == b.is_dir
}

fn fold_change(entries: &mut Vec<(u64, NameEntry)>, change: &Observed, changes: &mut Changes) {
    match change {
        Observed::Gone { path, lower } => {
            let gone = subtree(entries, lower, path);
            if gone.len() <= 32 {
                for at in gone.into_iter().rev() {
                    let (key, _) = entries.remove(at);
                    changes.stale.push(key);
                }
            } else {
                let mut at = 0usize;
                let mut next = gone.into_iter().peekable();
                entries.retain(|(key, _)| {
                    let leaves = next.next_if_eq(&at).is_some();
                    if leaves {
                        changes.stale.push(*key);
                    }
                    at = at.saturating_add(1);
                    !leaves
                });
            }
        }
        Observed::Present(fresh) => match position(entries, &fresh.facts.lower, &fresh.path) {
            Ok(at) => {
                // Content and mtime changes within a recency band leave the
                // searchable names, rows and ranking unchanged.
                if let Some((_, existing)) = entries.get_mut(at)
                    && !same_record(existing, fresh)
                {
                    existing.clone_from(fresh);
                    changes.touch(fresh);
                }
            }
            Err(at) => insert_keyed(entries, at, fresh.clone(), changes),
        },
    }
}

/// Positions of `path` and of every entry under it, ascending. At each depth
/// below its own, its descendants share its lowercased prefix and so sit
/// together in name order: a few binary searches, not a scan.
fn subtree(entries: &[(u64, NameEntry)], lower: &str, path: &Path) -> Vec<usize> {
    let mut found = Vec::new();
    if let Ok(at) = position(entries, lower, path) {
        found.push(at);
    }
    let deepest = entries
        .last()
        .map_or(0, |(_, entry)| path_depth(&entry.facts.lower));
    let prefix = format!("{lower}/");
    for depth in path_depth(lower).saturating_add(1)..=deepest {
        let start = entries.partition_point(|(_, entry)| {
            (path_depth(&entry.facts.lower), entry.facts.lower.as_str()) < (depth, prefix.as_str())
        });
        let run = entries.get(start..).unwrap_or_default();
        for (offset, (_, entry)) in run.iter().enumerate() {
            if path_depth(&entry.facts.lower) != depth || !entry.facts.lower.starts_with(&prefix) {
                break;
            }
            if entry.path.starts_with(path) {
                found.push(start.saturating_add(offset));
            }
        }
    }
    found.sort_unstable();
    found
}

/// The spacing that puts `count` keys evenly strictly inside `span`.
fn key_step(span: u64, count: usize) -> u64 {
    let slots = u64::try_from(count).map_or(u64::MAX, |count| count.saturating_add(1));
    span.checked_div(slots).unwrap_or_default()
}

/// Insert `entry` at `at` with a key strictly between its neighbours'. When
/// they leave no room, renumber a window around it, doubling until its keys
/// can sit [`RELABEL_GAP`] apart; each renumbered neighbour is re-indexed.
fn insert_keyed(
    entries: &mut Vec<(u64, NameEntry)>,
    at: usize,
    entry: NameEntry,
    changes: &mut Changes,
) {
    changes.touch(&entry);
    entries.insert(at, (0, entry));
    let mut start = at;
    let mut end = at.saturating_add(1);
    let mut wanted = 1;
    loop {
        let low = start
            .checked_sub(1)
            .and_then(|before| entries.get(before))
            .map_or(0, |(key, _)| *key);
        let high = entries.get(end).map_or(u64::MAX, |(key, _)| *key);
        let count = end.saturating_sub(start);
        let step = key_step(high.saturating_sub(low), count);
        // The whole index always fits: 2^64 keys for at most MAX_ENTRIES
        // walked names and what the watch adds.
        if step >= wanted || (start == 0 && end >= entries.len()) {
            let mut key = low;
            let window = entries.get_mut(start..end).unwrap_or_default();
            for (offset, (slot, renumbered)) in window.iter_mut().enumerate() {
                key = key.saturating_add(step.max(1));
                if start.saturating_add(offset) != at && *slot != key {
                    changes.stale.push(*slot);
                    changes.touch(renumbered);
                }
                *slot = key;
            }
            #[cfg(test)]
            if count > 1 {
                tests::NAME_INDEX_RENUMBERS.with(|n| n.set(n.get().saturating_add(1)));
            }
            return;
        }
        wanted = RELABEL_GAP;
        start = start.saturating_sub(count);
        end = end.saturating_add(count).min(entries.len());
    }
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
    /// Queries read under the shared lock. A watched change holds the write
    /// lock only for its upserts and incremental commit (milliseconds); a
    /// walk or a bulk change builds its index beside this one and swaps it in.
    name_index: Arc<RwLock<NameIndex>>,
    /// Opened on first [`Self::ensure_ready`]. `None` means open failed.
    tantivy: OnceLock<Option<crate::shell::search::files_index::FilesIndex>>,
    /// Set once the home walk has been kicked and the content index opened.
    /// Test helpers that inject an index pre-set this so a walk never runs.
    ready: OnceLock<()>,
    /// Lives as long as the provider so dropping it closes the watch.
    watch: OnceLock<RecommendedWatcher>,
}

/// Home the files search walks.
///
/// A scratch or headless session points the XDG dirs at its profile and used
/// to leave `$HOME` as the login home, so the walker indexed the operator's
/// real files. When `XDG_CONFIG_HOME` is set and does not live under `$HOME`,
/// walk the profile's `home` (or the profile root if that directory does not
/// exist). A control-socket or identity-root process that still has the login
/// layout, and is not a Tilo desktop, gets a directory under the session cache
/// instead. A desktop session (`XDG_SESSION_DESKTOP=tilo`, including a dev VM
/// slot driven through `TILO_TEST_CONTROL`) indexes `$HOME`: that is where
/// `~/Documents` is written.
fn indexable_home() -> PathBuf {
    let home = tilo_paths::home().unwrap_or_else(|| PathBuf::from("/tmp"));
    if let Some(config) = tilo_paths::config_dir()
        && !config.starts_with(&home)
    {
        let profile = config.parent().unwrap_or(config.as_path());
        let profile_home = profile.join("home");
        return if profile_home.is_dir() {
            profile_home
        } else {
            profile.to_path_buf()
        };
    }
    if !is_tilo_desktop_session()
        && (std::env::var_os("TILO_TEST_CONTROL").is_some()
            || std::env::var_os("TILO_IDENTITY_ROOT").is_some())
    {
        let scratch = tilo_paths::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("tilo-headless-home");
        let _ = std::fs::create_dir_all(&scratch);
        return scratch;
    }
    home
}

/// Greetd's `tilo-session` and a dev VM slot both export this. The slot also
/// sets `TILO_TEST_CONTROL` so a harness can drive it; that socket must not
/// send file search to a scratch directory.
fn is_tilo_desktop_session() -> bool {
    std::env::var_os("XDG_SESSION_DESKTOP").as_deref() == Some(std::ffi::OsStr::new("tilo"))
}

impl FilesProvider {
    pub fn new() -> Self {
        Self::with_home(indexable_home())
    }

    fn with_home(home: PathBuf) -> Self {
        Self {
            home,
            name_index: Arc::new(RwLock::new(NameIndex::default())),
            tantivy: OnceLock::new(),
            ready: OnceLock::new(),
            watch: OnceLock::new(),
        }
    }

    /// Open the content index and kick the background name walk once.
    /// Users who query before the walk finishes get only the content index.
    fn ensure_ready(&self) {
        self.ready.get_or_init(|| {
            #[cfg(test)]
            crate::shell::test_support::ambient_start("Files search constructor");
            self.start_name_index();
            let _ = self.tantivy.set(
                crate::shell::search::files_index::FilesIndex::open_or_create(&self.home).ok(),
            );
        });
    }

    /// Kick the home walk and the inotify watch. Safe to call once; a
    /// second call walks again but does not arm another watcher.
    fn start_name_index(&self) {
        let wake = crate::shell::search::invocation_wake::waker();
        let name_index = Arc::clone(&self.name_index);
        let home = self.home.clone();
        thread::Builder::new()
            .name("tilo-search-files-walk".into())
            .spawn(move || {
                walk_home(&home, &name_index);
                wake.wake();
            })
            .ok();
        start_name_watch(&self.home, &self.name_index, &self.watch);
    }

    /// Force a re-walk of the home directory. Idempotent; safe to call from any thread.
    pub fn rescan(&self) {
        self.ensure_ready();
        #[cfg(test)]
        crate::shell::test_support::ambient_start("Files search rescan");
        let name_index = Arc::clone(&self.name_index);
        let home = self.home.clone();
        thread::Builder::new()
            .name("tilo-search-files-rescan".into())
            .spawn(move || walk_home(&home, &name_index))
            .ok();
    }

    fn published(&self) -> Option<RwLockReadGuard<'_, NameIndex>> {
        self.name_index.read().ok()
    }

    /// Content hits, appended to `out` and deduped against
    /// `seen` name hits so a file matching both only shows once.
    fn push_content_hits(
        &self,
        needle: &str,
        seen: &HashSet<PathBuf>,
        out: &mut Vec<SearchResult>,
    ) {
        let Some(idx) = self.tantivy.get().and_then(Option::as_ref) else {
            return;
        };
        for hit in idx.query(needle, MAX_RESULTS) {
            if seen.contains(&hit.path) {
                continue;
            }
            if out.len() >= MAX_RESULTS {
                break;
            }
            let display = display_path(&self.home, &hit.path);
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

    /// One page of name hits, best first, continuing after the hits a
    /// cursor has already consumed.
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
        let Some(index) = self.published() else {
            return SearchPage::exhausted("unavailable", Vec::new());
        };
        let revision = index.generation.to_string();
        let needle = ctx.needle.to_lowercase();
        let needle_len = needle.len();
        crate::shell::search::index_page(
            revision,
            cursor,
            &needle,
            budget,
            |page| {
                index.index.search_scored(
                    &needle,
                    tilo_search::Page {
                        limit: budget.max_work,
                        ..page
                    },
                    move |signals| name_score(needle_len, signals),
                )
            },
            |hit| {
                let entry = index.entry(hit.key)?;
                let wanted = if entry.is_dir { want_folder } else { want_file };
                wanted.then(|| make_result(entry, hit.score, None))
            },
        )
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
    fn display_name(&self) -> String {
        crate::shell::i18n::t("search-provider-files")
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::File, ResourceKind::Folder]
    }
    /// See `MAX_SCORE`.
    fn max_score(&self) -> i32 {
        MAX_SCORE
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
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

    fn query_incomplete_reason(
        &self,
        _ctx: &SearchContext<'_>,
    ) -> Option<crate::shell::search::QueryIncompleteReason> {
        // Generation zero is the placeholder before the first walk publishes.
        // It establishes neither an empty home nor a finished query source.
        self.published()
            .as_ref()
            .is_none_or(|index| index.generation == 0)
            .then_some(crate::shell::search::QueryIncompleteReason::Unavailable)
    }

    fn search_revision(&self, _ctx: &SearchContext<'_>) -> String {
        self.published().map_or_else(
            || "unavailable".into(),
            |index| index.generation.to_string(),
        )
    }

    fn query_page(
        &self,
        ctx: SearchContext<'_>,
        cursor: Option<&SearchCursor>,
        budget: SearchBudget,
    ) -> SearchPage {
        self.ensure_ready();
        // Name lookup owns all continuation pages.  The content index
        // returns a bounded top-K; append it only to the first page so
        // scrolling never repeats content rows.
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

    /// A file's address is its absolute path, so any file or folder under
    /// the indexed home opens by its path — whether or not a search found it
    /// first, and whether or not the background walk has reached it yet.
    fn resolve(&self, id: &str, ctx: SearchContext<'_>) -> Option<SearchResult> {
        let entry = self.entry_for_address(id)?;
        let kind = if entry.is_dir {
            ResourceKind::Folder
        } else {
            ResourceKind::File
        };
        ctx.wanted
            .contains(&kind)
            .then(|| make_result(&entry, MAX_SCORE, None))
    }
}

impl FilesProvider {
    /// The entry an absolute path names, if it exists under the indexed home
    /// and is not one of the trees the walk deliberately skips. The path must
    /// already be canonical: an address with `..` or a symlink that leaves
    /// the home is not a file this provider offers.
    fn entry_for_address(&self, id: &str) -> Option<NameEntry> {
        let path = Path::new(id);
        if !path.is_absolute() {
            return None;
        }
        let canonical = std::fs::canonicalize(path).ok()?;
        let home = std::fs::canonicalize(&self.home).ok()?;
        if canonical.as_path() != path
            || !canonical.starts_with(&home)
            || should_skip(&canonical, &home)
        {
            return None;
        }
        let metadata = std::fs::metadata(&canonical).ok()?;
        let display = display_path(&home, &canonical);
        Some(NameEntry {
            facts: NameFacts::new(&display, metadata.modified().ok()),
            display,
            is_dir: metadata.is_dir(),
            path: canonical,
        })
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
    SearchResult::view_with_output(
        kind,
        "files",
        entry.path.display().to_string(),
        file_title(&entry.path),
        subtitle.or_else(|| Some(entry.display.clone())),
        score,
        move |chrome: &Chrome| open_search_hit(&path, is_dir, chrome),
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

/// The indexed fields of a name, as weights of the shared index: the
/// filename, its stem, each parent directory, and the whole path.
const LEAF: FieldWeight = FieldWeight::TITLE;
const STEM: FieldWeight = FieldWeight::new(90);
const PARENT: FieldWeight = FieldWeight::IDENTITY;
const PATH: FieldWeight = FieldWeight::METADATA;

/// Structural tiers, strongest first. The needle either matched the leaf
/// (the filename) or, failing that, somewhere up the path.
const STRUCT_EXACT_LEAF: i32 = 88;
/// The leaf minus its extension is exactly the needle: `open` → `open.md`.
const STRUCT_EXACT_STEM: i32 = 84;
/// The needle is exactly a word of the name, wherever it sits:
/// `report` → `report-template.xlsx` and `quarterly-report-2024.pdf` alike.
const STRUCT_TOKEN: i32 = 76;
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
/// The needle is a filename word with a typo.
const STRUCT_FUZZY: i32 = 12;

/// Each field's structural tiers, for the shared index.
fn name_rank(tier: MatchTier, weight: FieldWeight) -> Option<i32> {
    use MatchTier::{Contains, Exact, Prefix, PrefixWord, Typo, Word, WordPrefix};
    match (weight, tier) {
        (_, Typo) => Some(STRUCT_FUZZY),
        (LEAF, Exact) => Some(STRUCT_EXACT_LEAF),
        (LEAF, PrefixWord | Word) => Some(STRUCT_TOKEN),
        (LEAF, Prefix) => Some(STRUCT_FIRST_TOKEN_PREFIX),
        (LEAF, WordPrefix) => Some(STRUCT_TOKEN_PREFIX),
        (LEAF, Contains) => Some(STRUCT_IN_TOKEN),
        (STEM, Exact) => Some(STRUCT_EXACT_STEM),
        (PARENT, Exact) => Some(STRUCT_PATH_COMPONENT),
        (PARENT, PrefixWord | Prefix | Word | WordPrefix) => Some(STRUCT_PATH_TOKEN_PREFIX),
        (PATH, Contains) => Some(STRUCT_PATH_SUBSTRING),
        _ => None,
    }
}

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
const PENALTY_PER_LEVEL: i32 = 4;
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

/// Everything a path's score depends on besides the needle, computed by the
/// walker so a keystroke never `stat`s a file.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NameFacts {
    /// The `~/…` display path, lowercased. Path-level tiers match here.
    lower: String,
    /// The final component of [`Self::lower`].
    leaf: Box<str>,
    /// Leaf length minus a recognised extension; equals leaf length when there is none.
    stem_len: u16,
    /// Provenance + depth deductions, summed once since both depend only on the path.
    penalty: i32,
    recency: i32,
}

impl NameFacts {
    pub(crate) fn new(display: &str, modified: Option<SystemTime>) -> Self {
        let lower = display.to_lowercase();
        let leaf_lower = leaf_of(&lower).to_string();
        let stem_len = stem_len(&leaf_lower);
        let penalty =
            vendor_penalty(&lower, &leaf_lower, stem_len).saturating_add(depth_penalty(&lower));
        Self {
            leaf: leaf_lower.into_boxed_str(),
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

/// The score of a name hit: its structural tier, how much of the stem
/// the needle covers, recency and penalties, floored at 1 (a penalised
/// match is a bad answer, not a hidden one) and capped at [`MAX_SCORE`].
fn name_score(needle_len: usize, signals: Signals) -> i32 {
    let structural = signals.rank.unwrap_or(0);
    let stem_len = usize::try_from(signals.length).unwrap_or(usize::MAX);
    // Clamped at 1.0: "more than all of it" is still just all of it.
    let needle_len = u16::try_from(needle_len).unwrap_or(u16::MAX);
    let denom_len = u16::try_from(stem_len.max(usize::from(needle_len))).unwrap_or(u16::MAX);
    let ratio = (f32::from(needle_len) / f32::from(denom_len.max(1))).min(1.0);
    let ratio_weight = f32::from(u16::try_from(RATIO_WEIGHT).unwrap_or(u16::MAX));
    #[allow(
        clippy::cast_possible_truncation,
        reason = "ratio_bonus = round(ratio_weight * ratio) is bounded to 0..=RATIO_WEIGHT"
    )]
    let ratio_bonus = (ratio_weight * ratio).round() as i32;
    structural
        .saturating_add(ratio_bonus)
        .saturating_add(signals.boost)
        .clamp(1, MAX_SCORE)
}

/// Number of path components — the tiebreak between two equal-score matches.
fn path_depth(display_lower: &str) -> usize {
    display_lower.bytes().filter(|b| *b == b'/').count()
}

/// Open a palette hit via [`crate::shell::doc_open`] when an installed app
/// claims the type; unclaimed types and directories fall back to
/// `xdg-open`, and a claimed type with no editor is reported, not silent.
///
/// A failure is both shown to the person and returned, so an action caller
/// that cannot see the notification is told the file did not open; success
/// answers what opened it.
fn open_search_hit(
    path: &Path,
    is_dir: bool,
    chrome: &Chrome,
) -> Result<serde_json::Value, ActionError> {
    use crate::shell::doc_open::{self, DocumentTarget};
    let handed_off =
        || serde_json::json!({ "opened": "handed-off", "path": path, "with": "xdg-open" });
    if is_dir {
        spawn_open(path, is_dir);
        return Ok(handed_off());
    }
    match doc_open::resolve(path) {
        DocumentTarget::App { .. } | DocumentTarget::HostApp { .. } => {
            doc_open::open_in_workspace(path, chrome)
                .map(|opened| opened.to_value(path))
                .map_err(|e| {
                    let message = e.user_message();
                    notify_open_failure(path, &message, chrome);
                    ActionError::Refused(message)
                })
        }
        DocumentTarget::NoAppForDocument { ext } => {
            let message =
                crate::shell::i18n::t_args("search-files-no-app", &[("ext", ext.as_str().into())]);
            notify_open_failure(path, &message, chrome);
            Err(ActionError::Refused(message))
        }
        DocumentTarget::Unclaimed | DocumentTarget::Directory => {
            spawn_open(path, is_dir);
            Ok(handed_off())
        }
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
        .name("tilo-search-open".into())
        .spawn(move || {
            let _ = std::process::Command::new("xdg-open").arg(path).spawn();
        })
        .ok();
}

/// Arm inotify on `home` and the well-known user folders, then fold
/// create/remove events into the published name index after the first walk.
fn start_name_watch(
    home: &Path,
    index: &Arc<RwLock<NameIndex>>,
    slot: &OnceLock<RecommendedWatcher>,
) {
    if slot.get().is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    let Ok(mut watcher) = notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
    }) else {
        return;
    };
    arm_name_watches(&mut watcher, home);
    if slot.set(watcher).is_err() {
        return;
    }
    let home = home.to_owned();
    let index = Arc::clone(index);
    thread::Builder::new()
        .name("tilo-search-files-watch".into())
        .spawn(move || watch_name_index(&home, &index, &rx))
        .ok();
}

fn arm_name_watches(watcher: &mut RecommendedWatcher, home: &Path) {
    let _ = watcher.watch(home, RecursiveMode::NonRecursive);
    for name in WATCH_TREES {
        let dir = home.join(name);
        if dir.is_dir() {
            let _ = watcher.watch(&dir, RecursiveMode::Recursive);
        }
    }
}

fn watch_name_index(
    home: &Path,
    index: &RwLock<NameIndex>,
    rx: &mpsc::Receiver<notify::Result<notify::Event>>,
) {
    let mut pending = Vec::new();
    let mut walked = false;
    loop {
        if !walked {
            if published_generation(index) > 0 {
                walked = true;
                apply_watched_paths(home, index, &pending);
                pending.clear();
                continue;
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(event) => pending.extend(event_paths(&event)),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            }
            continue;
        }
        let Ok(first) = rx.recv() else {
            return;
        };
        let mut paths = event_paths(&first);
        loop {
            match rx.recv_timeout(WATCH_DEBOUNCE) {
                Ok(event) => paths.extend(event_paths(&event)),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    apply_watched_paths(home, index, &paths);
                    return;
                }
            }
        }
        paths.sort();
        paths.dedup();
        apply_watched_paths(home, index, &paths);
    }
}

fn published_generation(index: &RwLock<NameIndex>) -> u64 {
    index.read().ok().map_or(0, |guard| guard.generation)
}

fn event_paths(event: &notify::Result<notify::Event>) -> Vec<PathBuf> {
    match event {
        Ok(event) if matches!(event.kind, EventKind::Access(_)) => Vec::new(),
        Ok(event) => event.paths.clone(),
        Err(_) => Vec::new(),
    }
}

/// What `path` is now, or `None` when the name index never holds it.
fn observe(home: &Path, path: &Path) -> Option<Observed> {
    if should_skip(path, home) {
        return None;
    }
    let rel = path.strip_prefix(home).ok()?;
    if rel.as_os_str().is_empty() {
        return None;
    }
    let display = display_path(home, path);
    Some(match path.symlink_metadata() {
        Ok(meta) => Observed::Present(NameEntry {
            path: path.to_owned(),
            facts: NameFacts::new(&display, meta.modified().ok()),
            display,
            is_dir: meta.file_type().is_dir(),
        }),
        Err(_) => Observed::Gone {
            path: path.to_owned(),
            lower: display.to_lowercase(),
        },
    })
}

/// Fold one debounced batch of watched paths into the name index: an upsert
/// per added or changed name, a removal per name gone (with everything under
/// it), one incremental commit. Nothing is published when no name, kind or
/// recency band changed. A batch that would rebuild the index anyway
/// ([`BULK_CHANGE_MIN`]) is rebuilt beside it so queries keep running.
fn apply_watched_paths(home: &Path, index: &RwLock<NameIndex>, paths: &[PathBuf]) {
    // Every `stat` happens before any lock is taken.
    let observed: Vec<Observed> = paths
        .iter()
        .filter_map(|path| observe(home, path))
        .collect();
    if observed.is_empty() {
        return;
    }
    let Some((seen, total)) = index
        .read()
        .ok()
        .map(|guard| (guard.generation, guard.entries.len()))
    else {
        return;
    };
    if observed.len() >= BULK_CHANGE_MIN
        && observed.len().saturating_mul(BULK_CHANGE_FRACTION) >= total
    {
        if let Some(next) = rebuilt_with(index, &observed) {
            let previous = index.write().ok().map(|mut guard| {
                if guard.generation == seen {
                    std::mem::replace(&mut *guard, next)
                } else {
                    // A walk published meanwhile: fold the batch into it.
                    guard.apply(&observed);
                    next
                }
            });
            drop(previous);
        }
        return;
    }
    if let Ok(mut guard) = index.write() {
        guard.apply(&observed);
    }
}

/// The published names with `observed` folded in, built as a fresh index;
/// `None` when the batch changes nothing.
fn rebuilt_with(index: &RwLock<NameIndex>, observed: &[Observed]) -> Option<NameIndex> {
    let mut names: BTreeMap<PathBuf, NameEntry> = index
        .read()
        .ok()?
        .entries()
        .map(|entry| (entry.path.clone(), entry.clone()))
        .collect();
    let mut changed = false;
    for change in observed {
        match change {
            Observed::Present(fresh) => {
                let previous = names.insert(fresh.path.clone(), fresh.clone());
                changed |= previous.is_none_or(|previous| !same_record(&previous, fresh));
            }
            Observed::Gone { path, .. } => {
                // Component order keeps a directory's subtree right after it.
                let under: Vec<PathBuf> = names
                    .range(path.clone()..)
                    .map(|(name, _)| name)
                    .take_while(|name| name.starts_with(path))
                    .cloned()
                    .collect();
                changed |= !under.is_empty();
                for name in under {
                    names.remove(&name);
                }
            }
        }
    }
    changed.then(|| {
        let generation = NEXT_NAME_INDEX_GENERATION.fetch_add(1, Ordering::Relaxed);
        NameIndex::from_entries(names.into_values().collect(), generation)
    })
}

/// Build the name index. Breadth-first, because a single unpacked game is
/// tens of thousands of paths (`~/Spiele` alone walks to 31 168):
/// depth-first would spend [`MAX_ENTRIES`] on whichever branch it entered
/// first and exclude whole trees.
fn walk_home(home: &Path, index: &RwLock<NameIndex>) {
    let entries = walk_breadth_first(home, MAX_ENTRIES);
    // Build the whole index before touching the lock; queries meanwhile read
    // the previous one, which is dropped after the lock is released.
    let generation = NEXT_NAME_INDEX_GENERATION.fetch_add(1, Ordering::Relaxed);
    let next = NameIndex::from_entries(entries, generation);
    let previous = index
        .write()
        .ok()
        .map(|mut guard| std::mem::replace(&mut *guard, next));
    drop(previous);
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
    use crate::shell::chrome_state::ChromeState;
    use tilo_search::Page;

    thread_local! {
        /// Whole-index builds ([`NameIndex::from_entries`]).
        pub(super) static NAME_INDEX_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        /// Watched batches applied in place ([`NameIndex::apply`] publishing).
        pub(super) static NAME_INDEX_UPDATES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        /// Insertions that renumbered neighbours ([`insert_keyed`]).
        pub(super) static NAME_INDEX_RENUMBERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    fn generation(provider: &FilesProvider) -> u64 {
        provider.published().unwrap().generation
    }

    fn set_modified(path: &Path, modified: SystemTime) {
        std::fs::File::open(path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
    }

    fn watched_provider(home: &Path) -> FilesProvider {
        let provider = FilesProvider::with_home(home.to_owned());
        provider.ready.set(()).unwrap();
        assert!(provider.tantivy.set(None).is_ok());
        walk_home(home, &provider.name_index);
        provider
    }

    #[test]
    fn unchanged_watched_paths_keep_the_index_and_cursor() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let file = home.join("a-needle.md");
        let folder = home.join("b-needle");
        std::fs::write(&file, b"first content").unwrap();
        std::fs::create_dir(&folder).unwrap();
        let now = SystemTime::now();
        let first_time = now.checked_sub(Duration::from_secs(2 * 86_400)).unwrap();
        let second_time = now.checked_sub(Duration::from_secs(3 * 86_400)).unwrap();
        set_modified(&file, first_time);
        set_modified(&folder, first_time);
        let provider = watched_provider(home);
        let before = generation(&provider);
        let builds = NAME_INDEX_BUILDS.get();
        let updates = NAME_INDEX_UPDATES.get();
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let wanted = [ResourceKind::File, ResourceKind::Folder];
        let budget = SearchBudget {
            max_results: 1,
            max_bytes: 8_192,
            max_work: 1,
        };
        let first = provider.query_page(ctx_for("needle", &wanted, &st, &doc), None, budget);
        let cursor = first.next_cursor.unwrap();
        let expected =
            provider.query_page(ctx_for("needle", &wanted, &st, &doc), Some(&cursor), budget);
        // A content write changes mtime but leaves both timestamps inside the
        // 1..=6 day ranking band. Names search must retain its publication.
        std::fs::write(&file, b"different content and length").unwrap();
        set_modified(&file, second_time);
        set_modified(&folder, second_time);
        assert_ne!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            first_time
        );
        for _ in 0..2 {
            apply_watched_paths(home, &provider.name_index, &[file.clone(), folder.clone()]);
            assert_eq!(generation(&provider), before, "unchanged name publication");
            assert_eq!(NAME_INDEX_BUILDS.get(), builds, "no Tantivy rebuild");
            assert_eq!(NAME_INDEX_UPDATES.get(), updates, "no record touched");
            let next =
                provider.query_page(ctx_for("needle", &wanted, &st, &doc), Some(&cursor), budget);
            assert!(!next.stale_cursor);
            assert_eq!(next.results.len(), 1);
            assert_eq!(next.results[0].id, expected.results[0].id);
            assert_eq!(next.results[0].score, expected.results[0].score);
        }
    }

    #[test]
    fn watched_changes_publish_ranking_kind_rename_and_removal() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let file = home.join("a-needle.md");
        let peer = home.join("b-needle.md");
        std::fs::write(&file, b"content").unwrap();
        std::fs::write(&peer, b"peer").unwrap();
        let now = SystemTime::now();
        set_modified(
            &file,
            now.checked_sub(Duration::from_secs(400 * 86_400)).unwrap(),
        );
        set_modified(
            &peer,
            now.checked_sub(Duration::from_secs(2 * 86_400)).unwrap(),
        );
        let provider = watched_provider(home);
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let wanted = [ResourceKind::File, ResourceKind::Folder];
        let budget = SearchBudget {
            max_results: 1,
            max_bytes: 8_192,
            max_work: 1,
        };
        let first = provider.query_page(ctx_for("needle", &wanted, &st, &doc), None, budget);
        assert_eq!(first.results[0].id, peer.display().to_string());
        let cursor = first.next_cursor.unwrap();
        let before = generation(&provider);
        let builds = NAME_INDEX_BUILDS.get();
        let updates = NAME_INDEX_UPDATES.get();
        set_modified(&file, now);
        // An unchanged path in the same batch must not suppress a real change.
        apply_watched_paths(home, &provider.name_index, &[peer.clone(), file.clone()]);
        assert_ne!(generation(&provider), before);
        // Every change below is applied in place; none rebuilds the index.
        assert_eq!(NAME_INDEX_BUILDS.get(), builds);
        assert_eq!(NAME_INDEX_UPDATES.get(), updates.saturating_add(1));
        let ranked = provider.query_page(ctx_for("needle", &wanted, &st, &doc), None, budget);
        assert_eq!(ranked.results[0].id, file.display().to_string());
        assert!(
            provider
                .query_page(ctx_for("needle", &wanted, &st, &doc), Some(&cursor), budget,)
                .stale_cursor
        );

        // Change only the kind: preserve the fresh ranking band and path.
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        set_modified(&file, now);
        apply_watched_paths(home, &provider.name_index, std::slice::from_ref(&file));
        let folders = provider.query_page(
            ctx_for("needle", &[ResourceKind::Folder], &st, &doc),
            None,
            SearchBudget::PALETTE,
        );
        assert_eq!(folders.results.len(), 1);
        assert_eq!(folders.results[0].id, file.display().to_string());
        assert_eq!(NAME_INDEX_BUILDS.get(), builds);
        assert_eq!(NAME_INDEX_UPDATES.get(), updates.saturating_add(2));

        let renamed = home.join("renamed.md");
        std::fs::rename(&peer, &renamed).unwrap();
        apply_watched_paths(home, &provider.name_index, &[peer, renamed.clone()]);
        let found = provider.query_page(
            ctx_for("renamed", &[ResourceKind::File], &st, &doc),
            None,
            SearchBudget::PALETTE,
        );
        assert_eq!(found.results.len(), 1);
        assert_eq!(found.results[0].id, renamed.display().to_string());
        assert_eq!(NAME_INDEX_BUILDS.get(), builds);
        assert_eq!(NAME_INDEX_UPDATES.get(), updates.saturating_add(3));

        std::fs::write(file.join("child.md"), b"child").unwrap();
        apply_watched_paths(home, &provider.name_index, &[file.join("child.md")]);
        std::fs::remove_dir_all(&file).unwrap();
        apply_watched_paths(home, &provider.name_index, std::slice::from_ref(&file));
        assert!(
            provider
                .published()
                .unwrap()
                .entries()
                .all(|entry| !entry.path.starts_with(&file))
        );
        assert_eq!(NAME_INDEX_BUILDS.get(), builds);
        assert_eq!(NAME_INDEX_UPDATES.get(), updates.saturating_add(5));
        let retained = generation(&provider);
        apply_watched_paths(home, &provider.name_index, &[file]);
        assert_eq!(generation(&provider), retained);
        assert_eq!(NAME_INDEX_BUILDS.get(), builds);
        assert_eq!(NAME_INDEX_UPDATES.get(), updates.saturating_add(5));
    }

    /// Needles whose hits [`watched_changes_answer_exactly_as_a_fresh_walk`]
    /// compares: many equal scores (so key order decides which page a hit
    /// lands on), a parent component, a typo.
    const EQUIVALENCE_NEEDLES: &[&str] = &["needle", "photo", "report", "documents", "nedle"];

    /// Everything a query can observe of a published index: its names in
    /// key order with every ranking fact.
    fn indexed_names(provider: &FilesProvider) -> Vec<String> {
        let index = provider.published().unwrap();
        assert!(
            index.entries.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "keys must ascend with name order"
        );
        assert_eq!(
            index.index.len(),
            index.entries.len(),
            "one record per name"
        );
        index
            .entries()
            .map(|entry| {
                let facts = &entry.facts;
                format!(
                    "{} {} dir={} lower={} leaf={} stem={} penalty={} recency={}",
                    entry.path.display(),
                    entry.display,
                    entry.is_dir,
                    facts.lower,
                    facts.leaf,
                    facts.stem_len,
                    facts.penalty,
                    facts.recency
                )
            })
            .collect()
    }

    /// Every hit for `needle`, paged three at a time so ties cross pages.
    fn every_hit(
        provider: &FilesProvider,
        needle: &str,
        chrome: &ChromeState,
        doc: &automerge::AutoCommit,
    ) -> Vec<(String, i32)> {
        let wanted = [ResourceKind::File, ResourceKind::Folder];
        let budget = SearchBudget {
            max_results: 3,
            max_bytes: 64 * 1024,
            max_work: 3,
        };
        let mut hits = Vec::new();
        let mut cursor = None;
        loop {
            let page = provider.query_page(
                ctx_for(needle, &wanted, chrome, doc),
                cursor.as_ref(),
                budget,
            );
            assert!(!page.stale_cursor, "{needle}: the index changed mid-query");
            hits.extend(page.results.into_iter().map(|row| (row.id, row.score)));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => return hits,
            }
            assert!(hits.len() < 10_000, "{needle}: paging never ended");
        }
    }

    /// Apply `paths` as one watched batch to `provider`, then check it answers
    /// exactly as an index freshly walked from the same tree: the same names
    /// with the same facts in the same order, and the same hits, scores and
    /// page boundaries for every needle. `bulk` says whether the batch is
    /// large enough to be rebuilt beside the index rather than applied.
    fn apply_and_compare(
        provider: &FilesProvider,
        paths: &[PathBuf],
        step: &str,
        bulk: bool,
        cx: (&ChromeState, &automerge::AutoCommit),
    ) {
        let home = provider.home.clone();
        let builds = NAME_INDEX_BUILDS.get();
        let before = generation(provider);
        apply_watched_paths(&home, &provider.name_index, paths);
        assert_ne!(generation(provider), before, "{step}: published");
        let expected_builds = if bulk {
            builds.saturating_add(1)
        } else {
            builds
        };
        assert_eq!(
            NAME_INDEX_BUILDS.get(),
            expected_builds,
            "{step}: whole-index builds"
        );
        let fresh = watched_provider(&home);
        let names = indexed_names(provider);
        let fresh_names = indexed_names(&fresh);
        assert!(
            names == fresh_names,
            "{step}: names differ from a fresh walk\nincremental: {names:#?}\nfresh: {fresh_names:#?}"
        );
        for needle in EQUIVALENCE_NEEDLES {
            assert_eq!(
                every_hit(provider, needle, cx.0, cx.1),
                every_hit(&fresh, needle, cx.0, cx.1),
                "{step}: hits for {needle:?} differ from a fresh walk"
            );
        }
    }

    /// Watched changes are applied record by record, and after each one the
    /// index answers exactly as a full rebuild over the same tree would:
    /// additions, a new folder, removals, a whole subtree going, renames,
    /// names differing only in case, recency and kind changes, a temporary
    /// file renamed into place, enough additions in one spot to renumber
    /// neighbours' keys, and a bulk unpack that is rebuilt beside the index.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one sequence of filesystem changes, each compared with a fresh walk"
    )]
    fn watched_changes_answer_exactly_as_a_fresh_walk() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let three_days = SystemTime::now()
            .checked_sub(Duration::from_secs(3 * 86_400))
            .unwrap();
        for file in [
            "needle.txt",
            "Documents/a-needle.md",
            "Documents/b-needle.md",
            "Documents/notes/needle.md",
            "Documents/zz-last.md",
            "Downloads/needle.zip",
            "Downloads/report-2026.pdf",
            "Pictures/trip/needle.jpg",
            "projects/alpha/needle.rs",
            "projects/beta/needle.rs",
            "projects/gamma/src/needle.rs",
        ] {
            let path = home.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, file).unwrap();
            set_modified(&path, three_days);
        }
        let provider = watched_provider(home);
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let cx = (&*st, &doc);
        let write = |rel: &str| {
            let path = home.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, rel).unwrap();
            path
        };

        let added = write("Documents/c-needle.md");
        apply_and_compare(&provider, &[added], "a new file", false, cx);

        let folder = home.join("Documents/new-needles");
        let children = [
            write("Documents/new-needles/x-needle.md"),
            write("Documents/new-needles/y-needle.md"),
        ];
        let mut batch = vec![folder];
        batch.extend(children);
        apply_and_compare(&provider, &batch, "a new folder of files", false, cx);

        let zip = home.join("Downloads/needle.zip");
        std::fs::remove_file(&zip).unwrap();
        apply_and_compare(&provider, &[zip], "a removed file", false, cx);

        // Only the folder is reported: everything under it goes with it.
        let alpha = home.join("projects/alpha");
        std::fs::remove_dir_all(&alpha).unwrap();
        apply_and_compare(&provider, &[alpha], "a removed folder", false, cx);

        let old = home.join("Documents/b-needle.md");
        let renamed = home.join("Documents/renamed-needle.md");
        std::fs::rename(&old, &renamed).unwrap();
        apply_and_compare(&provider, &[old, renamed], "a rename", false, cx);

        let cased = [write("Pictures/Needle.md"), write("Pictures/needle.md")];
        apply_and_compare(&provider, &cased, "names differing in case", false, cx);

        let aged = home.join("needle.txt");
        set_modified(
            &aged,
            SystemTime::now()
                .checked_sub(Duration::from_secs(400 * 86_400))
                .unwrap(),
        );
        apply_and_compare(&provider, &[aged], "a recency band change", false, cx);

        let kind = home.join("Downloads/report-2026.pdf");
        std::fs::remove_file(&kind).unwrap();
        std::fs::create_dir(&kind).unwrap();
        apply_and_compare(&provider, &[kind], "a kind change", false, cx);

        let temporary = write("Documents/d-needle.md.tmp");
        let landed = home.join("Documents/d-needle.md");
        std::fs::rename(&temporary, &landed).unwrap();
        apply_and_compare(
            &provider,
            &[temporary, landed],
            "a temporary file renamed into place",
            false,
            cx,
        );

        // Each photo sorts right after the previous one, so they keep
        // halving one gap between two keys until neighbours are renumbered.
        let renumbers = NAME_INDEX_RENUMBERS.get();
        for photo in 0..72 {
            let added = write(&format!("Documents/photo-{photo:04}.jpg"));
            apply_and_compare(&provider, &[added], &format!("photo {photo}"), false, cx);
        }
        assert!(
            NAME_INDEX_RENUMBERS.get() > renumbers,
            "the fixture must exhaust a gap and renumber neighbours"
        );

        let archive = home.join("Documents/archive");
        let mut unpacked = vec![archive.clone()];
        for file in 0..300 {
            unpacked.push(write(&format!("Documents/archive/needle-{file:03}.txt")));
        }
        apply_and_compare(&provider, &unpacked, "a bulk unpack", true, cx);

        std::fs::remove_dir_all(&archive).unwrap();
        apply_and_compare(
            &provider,
            &[archive],
            "the unpacked folder removed",
            false,
            cx,
        );
    }

    /// Names in [`a_watched_change_costs_one_record_not_a_rebuild`]'s index.
    /// The nightly performance job measures the walk's ceiling, `MAX_ENTRIES`.
    fn update_cost_names() -> usize {
        std::env::var("TILO_FILES_NAME_INDEX_NAMES")
            .ok()
            .and_then(|names| names.parse().ok())
            .unwrap_or(20_000)
    }

    fn process_cpu() -> Duration {
        crate::compositor::metrics::cpu_trace::process_cpu_time().expect("a process CPU clock")
    }

    fn cpu_ms(spent: Duration) -> String {
        format!("{:.3}", spent.as_secs_f64() * 1_000.0)
    }

    /// One watched change costs its own record, not a rebuild of every name:
    /// the before/after of this provider's idle cost. Prints the
    /// `FILES-NAME-INDEX` line the nightly performance job records
    /// (`tools/perf/adapters.py files-name-index`); process CPU, so the
    /// index's own writer and merge threads count too.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one fixture measured through each kind of watched batch, then reported"
    )]
    fn a_watched_change_costs_one_record_not_a_rebuild() {
        /// A typical change, the median of seven. A whole rebuild of 20k
        /// names costs 1.5-2 s of CPU on a CI runner; a change applied in
        /// place costs about 10 ms there.
        const CHANGE_CPU_BUDGET: Duration = Duration::from_millis(50);
        /// No single change may cost this share of a whole rebuild measured
        /// in the same process, so one sample inflated by a loaded runner
        /// passes, but a change that rebuilds the index never does.
        const REBUILD_SHARE: u32 = 10;
        const WORDS: [&str; 8] = [
            "report", "invoice", "holiday", "draft", "notes", "budget", "scan", "receipt",
        ];
        let root = tempfile::tempdir().unwrap();
        let home = root.path();
        let downloads = home.join("Downloads");
        let pictures = home.join("Pictures");
        std::fs::create_dir_all(&downloads).unwrap();
        std::fs::create_dir_all(&pictures).unwrap();
        let names = update_cost_names();
        // Only the changed paths exist on disk; the indexed ones need not.
        let entries: Vec<NameEntry> = (0..names)
            .map(|at| {
                let display = format!(
                    "~/Documents/{}-{:03}/{}/{at:06}-{}.md",
                    WORDS[at % 8],
                    at % 397,
                    WORDS[(at / 8) % 8],
                    WORDS[(at / 64) % 8]
                );
                NameEntry {
                    path: home.join(display.trim_start_matches("~/")),
                    facts: NameFacts::new(&display, None),
                    display,
                    is_dir: false,
                }
            })
            .collect();
        let started = process_cpu();
        let built = NameIndex::from_entries(
            entries,
            NEXT_NAME_INDEX_GENERATION.fetch_add(1, Ordering::Relaxed),
        );
        let rebuild = process_cpu().saturating_sub(started);
        let provider = FilesProvider::with_home(home.to_owned());
        provider.ready.set(()).unwrap();
        assert!(provider.tantivy.set(None).is_ok());
        *provider.name_index.write().unwrap() = built;
        let builds = NAME_INDEX_BUILDS.get();
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let wanted = [ResourceKind::File];
        let found = |needle: &str, path: &Path| {
            provider
                .page_name_hits(
                    &ctx_for(needle, &wanted, &st, &doc),
                    None,
                    SearchBudget::PALETTE,
                )
                .results
                .iter()
                .any(|row| row.id == path.display().to_string())
        };
        let timed = |paths: &[PathBuf]| {
            let started = process_cpu();
            apply_watched_paths(home, &provider.name_index, paths);
            process_cpu().saturating_sub(started)
        };

        // One new file per batch, as a save or a finished download lands.
        let mut changes = Vec::new();
        for sample in 0..7 {
            let path = downloads.join(format!("quarterly-statement-{sample}.pdf"));
            std::fs::write(&path, b"%PDF").unwrap();
            let before = generation(&provider);
            changes.push(timed(std::slice::from_ref(&path)));
            assert_ne!(generation(&provider), before, "the new file is published");
            assert!(found(&format!("quarterly-statement-{sample}"), &path));
        }

        // A download growing: every burst's batch is the same `.part` file
        // with a new mtime in the same recency band, which changes nothing.
        let part = downloads.join("dataset.zip.part");
        std::fs::write(&part, b"chunk").unwrap();
        timed(std::slice::from_ref(&part));
        let before = generation(&provider);
        let mut unchanged = Vec::new();
        for _ in 0..7 {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&part)
                .unwrap();
            std::io::Write::write_all(&mut file, b"more chunks").unwrap();
            unchanged.push(timed(std::slice::from_ref(&part)));
        }
        assert_eq!(
            generation(&provider),
            before,
            "a growing file is not republished"
        );
        // ... then it lands under its final name.
        let landed = downloads.join("dataset.zip");
        std::fs::rename(&part, &landed).unwrap();
        let rename = timed(&[part, landed.clone()]);
        assert!(found("dataset", &landed));

        // A camera import: a folder of 64 photos in one batch.
        let import = pictures.join("import");
        std::fs::create_dir(&import).unwrap();
        let mut burst = vec![import.clone()];
        for photo in 0..64 {
            let path = import.join(format!("IMG_{photo:04}.jpg"));
            std::fs::write(&path, b"jpeg").unwrap();
            burst.push(path);
        }
        let burst_cpu = timed(&burst);
        assert!(found("img_0063", &import.join("IMG_0063.jpg")));

        assert_eq!(
            NAME_INDEX_BUILDS.get(),
            builds,
            "no change rebuilt the index"
        );
        let join = |samples: &[Duration]| {
            samples
                .iter()
                .map(|spent| cpu_ms(*spent))
                .collect::<Vec<_>>()
                .join(",")
        };
        println!(
            "FILES-NAME-INDEX names={names} rebuild_ms={} change_ms={} unchanged_ms={} \
             rename_ms={} burst64_ms={}",
            cpu_ms(rebuild),
            join(&changes),
            join(&unchanged),
            cpu_ms(rename),
            cpu_ms(burst_cpu)
        );
        let mut sorted = changes.clone();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2];
        assert!(
            median < CHANGE_CPU_BUDGET,
            "a change costs {} ms of CPU at {names} names (median; budget {} ms; a whole \
             rebuild cost {} ms)",
            cpu_ms(median),
            CHANGE_CPU_BUDGET.as_millis(),
            cpu_ms(rebuild)
        );
        let share = rebuild.checked_div(REBUILD_SHARE).unwrap();
        for (what, spent) in changes
            .iter()
            .enumerate()
            .map(|(sample, spent)| (format!("change {sample}"), *spent))
            .chain([
                ("the rename".into(), rename),
                ("the burst".into(), burst_cpu),
            ])
        {
            assert!(
                spent < share,
                "{what} cost {} ms of CPU at {names} names, over a {REBUILD_SHARE}th of the \
                 {} ms whole rebuild",
                cpu_ms(spent),
                cpu_ms(rebuild)
            );
        }
    }

    /// Case-insensitive-by-construction extension check for test assertions,
    /// via `Path::extension` rather than a raw `ends_with(".ext")`.
    fn has_ext(name: &str, ext: &str) -> bool {
        Path::new(name)
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case(ext))
    }

    /// The score of `needle` against one indexed path with no mtime, or
    /// `None` when it does not match.
    fn score_of(needle: &str, display: &str) -> Option<i32> {
        aged_score_of(needle, display, None)
    }

    /// [`score_of`] for a path last modified `days` ago.
    fn aged_score_of(needle: &str, display: &str, days: Option<u64>) -> Option<i32> {
        let modified = days.and_then(|days| {
            let secs = days.saturating_mul(86_400).saturating_add(3_600);
            SystemTime::now().checked_sub(Duration::from_secs(secs))
        });
        let entry = NameEntry {
            path: PathBuf::from(display.replace('~', "/home/tester")),
            facts: NameFacts::new(display, modified),
            display: display.to_string(),
            is_dir: false,
        };
        let index = NameIndex::from_entries(vec![entry], 1);
        let needle = needle.to_lowercase();
        let needle_len = needle.len();
        index
            .index
            .search_scored(&needle, Page::first(1), move |signals| {
                name_score(needle_len, signals)
            })
            .hits
            .first()
            .map(|hit| hit.score)
    }

    #[test]
    fn file_name_scoring_prefers_the_exact_whole_word_leaf() {
        // Phase `name_score_prefers_the_leaf_over_the_path`.
        {
            assert_eq!(
                score_of("todo", "~/notes/todo.md"),
                Some(STRUCT_EXACT_STEM + RATIO_WEIGHT)
            );
            assert_eq!(
                score_of("todo", "~/notes/day-todo-list.md"),
                Some(STRUCT_TOKEN + 5)
            );
            let path_only = score_of("todo", "~/todo/notes.md").unwrap();
            assert!(path_only > STRUCT_PATH_COMPONENT && path_only < STRUCT_TOKEN);
            assert_eq!(score_of("xzq", "~/notes/todo.md"), None);
        }

        // Phase `a_whole_word_beats_the_start_of_a_longer_word`.
        {
            let whole = score_of("open", "~/a/open.txt").unwrap();
            let prefix = score_of("open", "~/a/opening-notes.md").unwrap();
            let inside = score_of("open", "~/a/reopen.md").unwrap();
            let other_word = score_of("open", "~/a/xdg-open").unwrap();
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

        // Phase `an_exact_filename_match_clearly_wins`.
        {
            let exact = score_of("open", "~/a/open.md").unwrap();
            let long = score_of("open", "~/a/opentelemetry-collector-config.md").unwrap();
            assert!(
                exact >= long + 30,
                "an exact name match ({exact}) must clearly beat four characters \
                 of a thirty-character name ({long}), not tie with it"
            );
        }

        // Phase `stem_length_ignores_a_version_number_masquerading_as_an_extension`.
        // A version-numbered directory must not lose its name to a bogus "extension".
        {
            assert_eq!(stem_len("openttd-jgrpp-0.72.3-linux-generic-amd64"), 40);
            assert_eq!(stem_len("open.md"), 4);
            assert_eq!(stem_len("archive.tar.gz"), 11);
            assert_eq!(stem_len("noextension"), 11);
            assert_eq!(stem_len(".hidden"), 7);
        }

        // Phase `max_score_is_the_honest_ceiling`.
        // The registry divides by this; outgrowing it silently takes over the palette.
        {
            assert_eq!(
                aged_score_of("open", "~/open", Some(0)),
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
                        if let Some(s) = aged_score_of(needle, display, Some(days)) {
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
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn file_placement_sinks_assets_vendored_and_buried_paths() {
        // Phase `a_document_outranks_the_font_assets_of_a_game`.
        {
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

        // Phase `vendored_content_sinks_but_stays_findable`.
        {
            let mine = score_of("parser", "~/proj/src/parser.rs").unwrap();
            let theirs = score_of("parser", "~/proj/vendor/pkg/parser.rs").unwrap();
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

        // Phase `depth_counts_against_a_buried_path`.
        {
            assert_eq!(depth_penalty("~/report.md"), 0);
            assert_eq!(depth_penalty("~/dev/proj/src/lib.rs"), 0);
            let shallow = score_of("open", "~/Dokumente/opening.md").unwrap();
            let buried = score_of(
                "open",
                "~/Spiele/factorio/data/base/sound/open-close/silo-open.ogg",
            )
            .unwrap();
            assert!(
                shallow > buried,
                "a document two levels down ({shallow}) must outrank a game sound \
                 effect seven levels down ({buried})"
            );
            assert_eq!(depth_penalty("~/a/b/c/d/e/f/g/h/i/j/k"), PENALTY_DEPTH_MAX);
        }

        // Phase `equal_scores_prefer_the_shallower_path`.
        {
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
    }

    #[test]
    fn filename_words_match_whole_and_with_typos() {
        // Phase `letters_and_digits_are_separate_words`.
        {
            let whole = score_of("2026", "~/a/report2026final.md").unwrap();
            let inside = score_of("026", "~/a/report2026final.md").unwrap();
            assert!(whole > inside, "{whole} / {inside}");
        }

        // Phase `a_typo_in_a_filename_still_matches`.
        {
            assert!(score_of("noets", "~/a/notes").is_some());
            assert!(
                score_of("todp", "~/a/todo.md").is_none(),
                "short words need no typo match"
            );
            assert!(score_of("xz", "~/a/notes").is_none());
        }
    }

    #[test]
    fn recency_breaks_name_ties_from_the_index_but_never_outvotes_the_name() {
        // Phase `recency_separates_two_files_with_the_same_name`.
        {
            let fresh = aged_score_of("report", "~/a/report.md", Some(0)).unwrap();
            let stale = aged_score_of("report", "~/b/report.md", Some(900)).unwrap();
            assert!(
                fresh > stale,
                "the file touched today ({fresh}) must outrank the one from three \
                 years ago ({stale})"
            );
        }

        // Phase `recency_cannot_outvote_what_the_name_says`.
        // A freshly unpacked archive cannot displace an exact old match.
        {
            let old_exact = aged_score_of("notes", "~/a/notes.md", Some(900)).unwrap();
            let new_weak =
                aged_score_of("notes", "~/b/my-meeting-notes-and-agenda-draft.md", Some(0))
                    .unwrap();
            assert!(
                old_exact > new_weak,
                "a three-year-old notes.md ({old_exact}) must still beat a \
                 freshly-touched my-meeting-notes-and-agenda-draft.md ({new_weak})"
            );
        }

        // Phase `the_recency_band_comes_from_the_index_not_the_disk`.
        // Proven by scoring a path that does not exist on disk.
        {
            let ghost = "~/definitely/not/on/this/disk/report.md";
            assert!(!Path::new("/home/tester/definitely").exists());
            let fresh = aged_score_of("report", ghost, Some(0)).unwrap();
            let none = score_of("report", ghost).unwrap();
            assert_eq!(
                fresh - none,
                RECENCY_MAX,
                "a today mtime carried in the index must contribute the full \
                 recency band"
            );
        }
    }

    #[test]
    fn the_walk_skips_bloat_and_indexes_every_tree_before_ranking() {
        // Phase `skip_dirs_hits_common_bloat`.
        {
            let home = Path::new("/home/x");
            assert!(should_skip(Path::new("/home/x/proj/node_modules"), home));
            assert!(should_skip(Path::new("/home/x/proj/target/debug"), home));
            assert!(should_skip(Path::new("/home/x/.cache/foo"), home));
            assert!(!should_skip(Path::new("/home/x/proj/src/lib.rs"), home));
        }

        // Phase `multi_component_skip_patterns_match`.
        // A pattern with a `/` must match consecutive components, not just one.
        {
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

        // Phase `the_walk_indexes_every_tree_before_it_runs_out`.
        // Breadth-first spends the budget on the shallowest paths everywhere.
        {
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

        // Phase `the_best_matches_win_regardless_of_walk_position`.
        // The best matches win regardless of where they sit in the index.
        {
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

    #[test]
    fn file_catalogue_completion_rejects_unpublished_placeholder() {
        use crate::shell::chrome_state::SearchScope;
        use crate::shell::search::action_api::{ActionCatalog, AllVisible};
        use crate::shell::search::{QueryArgs, QueryIncompleteReason, SearchRegistry};
        let provider = std::rc::Rc::new(provider_from_entries(Vec::new()));
        *provider.name_index.write().unwrap() = NameIndex::default();
        let mut registry = SearchRegistry::new();
        registry.register(provider.clone());
        let chrome = empty_chrome();
        let doc = automerge::AutoCommit::new();
        let args = QueryArgs {
            needle: "",
            scope: SearchScope::All,
            active_provider_id: None,
            wanted_kinds: &[],
            chrome: &chrome.borrow(),
            doc: &doc,
            doc_root: &automerge::ROOT,
        };
        let unknown = ActionCatalog::build_query(&registry, &args, &AllVisible, true);
        assert!(unknown.enumerate().is_empty());
        assert!(!unknown.completion().complete);
        assert_eq!(
            unknown
                .completion()
                .incomplete
                .first()
                .expect("unpublished source")
                .reason,
            QueryIncompleteReason::Unavailable
        );
        *provider.name_index.write().unwrap() = NameIndex::from_entries(Vec::new(), 1);
        let ready = ActionCatalog::build_query(&registry, &args, &AllVisible, true);
        assert!(ready.enumerate().is_empty());
        assert!(ready.completion().complete);
    }

    #[test]
    fn initial_file_walk_wakes_catalogue_waiter() {
        let home = tempfile::tempdir().expect("home");
        let provider = FilesProvider::with_home(home.path().to_owned());
        let (woken, wake) = std::sync::mpsc::channel();
        let _installed = crate::shell::search::invocation_wake::install(Arc::new(move || {
            woken.send(()).expect("waiting catalogue caller");
        }));
        assert_eq!(provider.published().unwrap().generation, 0);
        provider.start_name_index();
        wake.recv_timeout(tilo_test_env::hang_bound())
            .expect("the initial name walk must wake a held listing");
        assert_ne!(provider.published().unwrap().generation, 0);
        let chrome = empty_chrome();
        let doc = automerge::AutoCommit::new();
        assert_eq!(
            provider.query_incomplete_reason(&SearchContext {
                query_scope: Some(crate::shell::chrome_state::SearchScope::All),
                include_programmatic: true,
                needle: "",
                wanted: &[ResourceKind::File],
                chrome: &chrome.borrow(),
                doc: &doc,
                doc_root: &automerge::ROOT,
            }),
            None,
            "the production walk established a complete empty name source"
        );
    }

    fn provider_from_entries(entries: Vec<NameEntry>) -> FilesProvider {
        let home = PathBuf::from("/home/tester");
        let ready = OnceLock::new();
        let _ = ready.set(());
        let tantivy = OnceLock::new();
        let _ = tantivy.set(None);
        FilesProvider {
            home,
            name_index: Arc::new(RwLock::new(NameIndex::from_entries(entries, 1))),
            tantivy,
            ready,
            watch: OnceLock::new(),
        }
    }

    #[test]
    #[serial_test::serial]
    fn scratch_xdg_tree_indexes_the_profile_home_not_the_login_home() {
        let login = tempfile::tempdir().expect("login home");
        let profile = tempfile::tempdir().expect("scratch profile");
        let profile_home = profile.path().join("home");
        std::fs::create_dir_all(&profile_home).expect("profile home");
        std::fs::write(login.path().join("secret.md"), b"operator").unwrap();
        std::fs::write(profile_home.join("note.md"), b"scratch").unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", login.path())
            .set("XDG_CONFIG_HOME", profile.path().join("config"))
            .set("XDG_CACHE_HOME", profile.path().join("cache"));
        env.remove("TILO_TEST_CONTROL");
        env.remove("TILO_IDENTITY_ROOT");
        assert_eq!(
            indexable_home(),
            profile_home,
            "a scratch XDG tree must not walk the login home"
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_normal_xdg_tree_under_home_indexes_home() {
        let home = tempfile::tempdir().expect("home");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path())
            .set("XDG_CONFIG_HOME", home.path().join(".config"));
        env.remove("TILO_TEST_CONTROL");
        env.remove("TILO_IDENTITY_ROOT");
        assert_eq!(indexable_home(), home.path());
    }

    #[test]
    #[serial_test::serial]
    fn a_control_socket_session_with_the_login_layout_gets_a_scratch_home() {
        let login = tempfile::tempdir().expect("login home");
        let cache = tempfile::tempdir().expect("cache");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", login.path())
            .set("XDG_CONFIG_HOME", login.path().join(".config"))
            .set("XDG_CACHE_HOME", cache.path())
            .set("TILO_TEST_CONTROL", "/tmp/tilo-test-control.sock");
        env.remove("TILO_IDENTITY_ROOT");
        let got = indexable_home();
        assert_eq!(got, cache.path().join("tilo-headless-home"));
        assert!(
            !got.starts_with(login.path()),
            "the control-socket session must not walk the login home"
        );
    }

    /// The dev VM session is a real desktop (`XDG_SESSION_DESKTOP=tilo`) that
    /// also sets `TILO_TEST_CONTROL` so the harness can drive it. The slot
    /// user writes `~/Documents`; search has to query that home.
    #[test]
    #[serial_test::serial]
    fn a_driven_desktop_session_finds_a_file_just_written_in_documents() {
        let home = tempfile::tempdir().expect("session home");
        let cache = tempfile::tempdir().expect("cache");
        let documents = home.path().join("Documents");
        std::fs::create_dir_all(&documents).expect("Documents");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path())
            .set("XDG_CONFIG_HOME", home.path().join(".config"))
            .set("XDG_CACHE_HOME", cache.path())
            .set("XDG_SESSION_DESKTOP", "tilo")
            .set("TILO_TEST_CONTROL", "/run/tilo-vm/slot/control.sock");
        env.remove("TILO_IDENTITY_ROOT");
        assert_eq!(
            indexable_home(),
            home.path(),
            "a driven desktop session must search $HOME, not the headless cache"
        );

        let provider = FilesProvider::new();
        let chrome = empty_chrome();
        let state = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let hits = |needle: &str| {
            provider.query(ctx_for(
                needle,
                &[ResourceKind::File, ResourceKind::Folder],
                &state,
                &doc,
            ))
        };
        // The first palette query arms the walk and the Documents watch.
        drop(hits(" "));
        tilo_test_env::wait_until("the name walk to publish", || {
            provider
                .published()
                .is_some_and(|index| index.generation > 0)
        });

        let needle = "accneedleff3e9f";
        let dest = documents.join(format!("{needle}.txt"));
        let tmp = documents.join(format!("{needle}.txt.acc-tmp"));
        std::fs::write(&tmp, b"acceptance search probe\n").expect("probe temp");
        std::fs::rename(&tmp, &dest).expect("probe rename");

        tilo_test_env::wait_until("the Documents probe to be searchable", || {
            hits(needle)
                .iter()
                .any(|row| action_text_contains(row, needle))
        });
        let hit = hits(needle)
            .into_iter()
            .find(|row| action_text_contains(row, needle))
            .expect("probe");
        assert!(
            Path::new(&hit.id).starts_with(home.path()),
            "search indexed {} instead of the session home {}",
            hit.id,
            home.path().display()
        );
        assert!(
            !Path::new(&hit.id).starts_with(cache.path()),
            "search indexed the headless cache {} for a desktop session",
            hit.id
        );
    }

    fn action_text_contains(row: &SearchResult, needle: &str) -> bool {
        row.title.contains(needle)
            || row.id.contains(needle)
            || row
                .subtitle
                .as_deref()
                .is_some_and(|text| text.contains(needle))
    }

    #[test]
    fn constructing_files_provider_starts_no_ambient_file_search() {
        let guard = crate::shell::test_support::DenyAmbientStarts::new();
        let provider = FilesProvider::new();
        assert!(
            provider.ready.get().is_none(),
            "home walk and tantivy must wait for the first query"
        );
        assert_eq!(guard.attempts(), 0);
        drop(guard);
        drop(provider);
    }

    #[test]
    fn a_new_file_in_documents_is_searchable_without_rescan() {
        crate::shell::test_support::ambient_start("Files search name index");
        let root = tempfile::tempdir().expect("isolated home");
        let home = root.path();
        std::fs::create_dir(home.join("Documents")).expect("Documents");
        let provider = FilesProvider::with_home(home.to_path_buf());
        provider.start_name_index();
        tilo_test_env::wait_until("the name walk to publish", || {
            provider
                .published()
                .is_some_and(|index| index.generation > 0)
        });

        let needle = "accwatchprobe";
        std::fs::write(
            home.join("Documents").join(format!("{needle}.txt")),
            b"acceptance search probe\n",
        )
        .expect("write probe");

        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        tilo_test_env::wait_until("the watched Documents file to be indexed", || {
            provider
                .page_name_hits(
                    &ctx_for(
                        needle,
                        &[ResourceKind::File, ResourceKind::Folder],
                        &st,
                        &doc,
                    ),
                    None,
                    SearchBudget {
                        max_results: MAX_RESULTS,
                        max_bytes: 64 * 1024,
                        max_work: SearchBudget::PALETTE.max_work,
                    },
                )
                .results
                .iter()
                .any(|row| row.title.contains(needle))
        });
    }

    fn ctx_for<'a>(
        needle: &'a str,
        wanted: &'a [ResourceKind],
        chrome: &'a ChromeState,
        doc: &'a automerge::AutoCommit,
    ) -> SearchContext<'a> {
        SearchContext {
            query_scope: None,
            include_programmatic: false,
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
            crate::shell::state::WorkspaceTreeHandle::new(WorkspaceTree::default());
        ChromeState::new_with_handle(handle)
    }

    /// Candidates ahead of the late hit in the corpus tests. Every work
    /// ceiling asserted there is two orders of magnitude smaller, so a
    /// linear scan would still exceed it by far; the 100k case itself is
    /// covered by the shared search provider tests in `ui`.
    const LARGE_CORPUS: usize = 4_000;

    #[test]
    fn filtered_name_pages_consume_hits_and_respect_the_result_budget() {
        let provider = provider_with_index(vec![
            ("~/a-needle", true),
            ("~/b-needle", true),
            ("~/c-needle", true),
            ("~/d-needle", true),
            ("~/e-needle", true),
        ]);
        let chrome = empty_chrome();
        let state = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let budget = SearchBudget {
            max_results: 1,
            max_bytes: 8_192,
            max_work: 5,
        };
        let filtered = provider.query_page(
            ctx_for("needle", &[ResourceKind::File], &state, &doc),
            None,
            budget,
        );
        assert!(filtered.results.is_empty());
        assert!(
            filtered.exhausted,
            "all five declined hits must be consumed"
        );

        let mut cursor = None;
        let mut ids = std::collections::HashSet::new();
        for _ in 0..5 {
            let page = provider.query_page(
                ctx_for("needle", &[ResourceKind::Folder], &state, &doc),
                cursor.as_ref(),
                budget,
            );
            assert_eq!(page.results.len(), 1);
            assert!(ids.insert(page.results[0].id.clone()), "a hit was repeated");
            cursor = page.next_cursor;
        }
        assert!(cursor.is_none(), "the last emitted hit exhausts the page");
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn retained_corpus_pages_with_fixed_work_and_stable_cursors() {
        // Phase `pages_a_large_retained_corpus_with_fixed_work`.
        // Paging a large retained corpus must not quietly turn the first
        // (or a late) palette page back into a whole-index scan.  This measures
        // inspected entries, not elapsed time, so it remains meaningful under
        // sanitizers and on slow CI workers.
        {
            let mut paths = (0..LARGE_CORPUS)
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
            let first = p.query_page(
                ctx_for("target-needle", &[ResourceKind::File], &st, &doc),
                None,
                budget,
            );
            assert!(first.results.len() <= 17);
            assert_eq!(
                first.results.first().map(|row| row.title.as_str()),
                Some("target-needle.md"),
                "an exact filename behind thousands of earlier candidates must lead"
            );
        }

        // Phase `late_non_exact_pages_keep_the_same_work_ceiling`.
        {
            let paths = (0..=LARGE_CORPUS)
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
            let first = p.query_page(
                ctx_for("needle", &[ResourceKind::File], &st, &doc),
                None,
                budget,
            );
            let mut cursor = first.next_cursor;
            // 200 pages of 17 stay short of the corpus, so the last one is late
            // but not the end.
            for _ in 0..200 {
                let page = p.query_page(
                    ctx_for("needle", &[ResourceKind::File], &st, &doc),
                    cursor.as_ref(),
                    budget,
                );
                assert!(!page.stale_cursor);
                cursor = page.next_cursor;
            }
            assert!(cursor.is_some(), "fixture must reach a genuine late page");
        }

        // Phase `rejects_a_cursor_after_the_retained_index_changes`.
        {
            let p =
                provider_with_index(vec![("~/one/needle.md", false), ("~/two/needle.md", false)]);
            let chrome = empty_chrome();
            let st = chrome.borrow();
            let doc = automerge::AutoCommit::new();
            let page = p.query_page(
                ctx_for("needle.md", &[ResourceKind::File], &st, &doc),
                None,
                SearchBudget {
                    max_results: 1,
                    max_bytes: 8_192,
                    max_work: 1,
                },
            );
            let cursor = page.next_cursor.expect("a continuation page");
            {
                let mut publication = p.name_index.write().unwrap();
                let entries = publication.entries().cloned().collect();
                *publication = NameIndex::from_entries(entries, 2);
            }
            let stale = p.query_page(
                ctx_for("needle.md", &[ResourceKind::File], &st, &doc),
                Some(&cursor),
                SearchBudget::PALETTE,
            );
            assert!(stale.stale_cursor);
            assert!(stale.results.is_empty());
        }

        // Phase `byte_limited_page_does_not_skip_the_first_unemitted_match`.
        {
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
    }

    /// A file opens by its path on the first request: no search, no walk.
    /// Only canonical paths under the indexed home, outside hidden and
    /// skipped trees, are addresses.
    #[test]
    fn a_file_opens_by_its_path_without_any_search_first() {
        let home = tempfile::tempdir().expect("home");
        let home_path = std::fs::canonicalize(home.path()).expect("canonical home");
        std::fs::create_dir_all(home_path.join("Documents")).expect("documents");
        std::fs::create_dir_all(home_path.join(".secrets")).expect("hidden");
        let report = home_path.join("Documents/report.md");
        std::fs::write(&report, "# report").expect("report");
        std::fs::write(home_path.join(".secrets/key"), "k").expect("hidden file");
        let outside = tempfile::tempdir().expect("outside");
        let stray = std::fs::canonicalize(outside.path())
            .expect("outside")
            .join("stray.md");
        std::fs::write(&stray, "x").expect("stray");

        let provider = FilesProvider::with_home(home_path.clone());
        let chrome = empty_chrome();
        let state = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        let wanted = [ResourceKind::File, ResourceKind::Folder];
        let resolve = |id: &str| provider.resolve(id, ctx_for("", &wanted, &state, &doc));

        let row = resolve(&report.display().to_string()).expect("the file resolves");
        assert_eq!(row.title, "report.md");
        assert_eq!(row.kind, ResourceKind::File);
        let folder =
            resolve(&home_path.join("Documents").display().to_string()).expect("a folder resolves");
        assert_eq!(folder.kind, ResourceKind::Folder);
        for refused in [
            stray.display().to_string(),
            home_path.join(".secrets/key").display().to_string(),
            home_path
                .join("Documents/../Documents/report.md")
                .display()
                .to_string(),
            home_path.join("Documents/missing.md").display().to_string(),
            "Documents/report.md".to_string(),
        ] {
            assert!(resolve(&refused).is_none(), "{refused} is not an address");
        }
        assert!(
            provider
                .resolve(
                    &report.display().to_string(),
                    ctx_for("", &[ResourceKind::Folder], &state, &doc)
                )
                .is_none(),
            "a file is not offered where only folders are wanted"
        );
    }
}
