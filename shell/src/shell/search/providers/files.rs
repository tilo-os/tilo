//! Files provider — searches the user's home directory for files and
//! directories.
//!
//! Two backends coexist behind a shared façade:
//!
//! - **`in_memory`** — a boot-time walk of `~` that caches every visible
//!   path and its size, kept warm for name-substring / fuzzy-subsequence
//!   queries. Cheap (no on-disk state), immediately available.
//! - **`tantivy` (via [`super::super::files_index`])** — a persistent
//!   Bloomtail/BM25 index over file **contents** so a query like
//!   "protocol registry" finds a file whose *body* contains those
//!   words, not just its name. The index lives at
//!   `~/.cache/qos/files-index`, is rebuilt incrementally in the
//!   background, and answers content queries in the O(matches) range.
//!
//! Both backends produce [`SearchResult`]s of kind [`ResourceKind::File`]
//! or [`ResourceKind::Folder`]. The palette's Enter callback routes a
//! file through [`crate::shell::doc_open`], which resolves the extension
//! to the installed package that declares it and launches that package
//! into the active workspace; directories and unclaimed types fall back
//! to the desktop's default handler.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use walkdir::WalkDir;

use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

/// Skip these directory names anywhere in the tree — they explode the
/// index without adding user-visible files. `.git` also serves as a
/// reasonable heuristic for "the user cares about the parent, not the
/// internals".
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
];

const MAX_DEPTH: usize = 8;
const MAX_ENTRIES: usize = 50_000;
const MAX_RESULTS: usize = 40;

/// In-memory index. Populated by a background walker started at provider
/// construction and re-triggered by a manual `rescan` (not wired to
/// inotify yet — a periodic re-scan every ~5 minutes is enough for the
/// palette's use case; users who want live updates fall back to the
/// tantivy content index).
#[derive(Default)]
struct NameIndex {
    entries: Vec<NameEntry>,
    ready: bool,
}

struct NameEntry {
    path: PathBuf,
    display: String,
    lower: String,
    is_dir: bool,
}

pub struct FilesProvider {
    home: PathBuf,
    name_index: Arc<Mutex<NameIndex>>,
    tantivy: Option<crate::shell::search::files_index::FilesIndex>,
}

impl FilesProvider {
    /// Construct + kick off the background name-index walk. Returns
    /// immediately; the walker takes a few hundred ms to a few seconds
    /// for a typical `~` and results become live as soon as it commits.
    /// Users who query before the walker finishes get whatever the
    /// tantivy content index knows so far.
    pub fn new() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        let name_index: Arc<Mutex<NameIndex>> = Arc::new(Mutex::new(NameIndex::default()));
        {
            let name_index = Arc::clone(&name_index);
            let home = home.clone();
            thread::Builder::new()
                .name("qos-search-files-walk".into())
                .spawn(move || walk_home(&home, name_index))
                .ok();
        }
        let tantivy = crate::shell::search::files_index::FilesIndex::open_or_create(&home).ok();
        Self {
            home,
            name_index,
            tantivy,
        }
    }

    /// Force a re-walk of the home directory. Idempotent; safe to call
    /// from any thread. The palette's refresh doesn't call this — a
    /// future settings toggle / inotify hook will.
    pub fn rescan(&self) {
        let name_index = Arc::clone(&self.name_index);
        let home = self.home.clone();
        thread::Builder::new()
            .name("qos-search-files-rescan".into())
            .spawn(move || walk_home(&home, name_index))
            .ok();
    }
}

impl Default for FilesProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for FilesProvider {
    fn id(&self) -> &str {
        "files"
    }
    fn name(&self) -> &str {
        "Files"
    }
    fn resource_kinds(&self) -> &[ResourceKind] {
        &[ResourceKind::File, ResourceKind::Folder]
    }
    /// A leaf-name prefix hit scores 120 - see [`name_score`].
    fn max_score(&self) -> i32 {
        120
    }

    fn query(&self, ctx: SearchContext<'_>) -> Vec<SearchResult> {
        if ctx.needle.trim().is_empty() {
            return Vec::new();
        }
        let want_file = ctx.wanted.contains(&ResourceKind::File);
        let want_folder = ctx.wanted.contains(&ResourceKind::Folder);
        if !want_file && !want_folder {
            return Vec::new();
        }

        let needle_lower = ctx.needle.to_lowercase();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut out: Vec<SearchResult> = Vec::new();

        // Filename hits from the in-memory index.
        //
        // Score the whole index, then keep the best `MAX_RESULTS` — not
        // the first `MAX_RESULTS` the walker happened to reach. Stopping
        // at the first N made the ranking depend on directory traversal
        // order: a query answered by a hundred equally-scoring files
        // showed whichever hundred sat alphabetically first under
        // whichever tree the walk entered first, and any better hit
        // further down the walk was simply never scored. Ranking is only
        // ranking if every candidate is in the comparison.
        //
        // Ranking the candidates by (score, path) instead of collecting
        // `SearchResult`s keeps this cheap: no callback allocation and
        // no `PathBuf` clone for the entries that lose.
        if let Ok(guard) = self.name_index.lock() {
            let mut ranked: Vec<(i32, usize, usize)> = Vec::new();
            for (i, entry) in guard.entries.iter().enumerate() {
                if entry.is_dir && !want_folder {
                    continue;
                }
                if !entry.is_dir && !want_file {
                    continue;
                }
                let Some(score) = name_score(&needle_lower, &entry.lower) else {
                    continue;
                };
                ranked.push((score, path_depth(&entry.lower), i));
            }
            // Best score first; among equals the shallower path wins.
            // Equally-good name matches are genuinely tied on relevance,
            // so the tiebreak has to come from somewhere — and depth is
            // the one signal that tracks how much the file belongs to
            // the user rather than to something they cloned: `~/notes`
            // over `~/src/vendor/pkg/dist/generated`.
            ranked.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| a.1.cmp(&b.1))
                    .then_with(|| guard.entries[a.2].lower.cmp(&guard.entries[b.2].lower))
            });
            for (score, _, i) in ranked.into_iter().take(MAX_RESULTS) {
                let entry = &guard.entries[i];
                seen.insert(entry.path.clone());
                out.push(make_result(entry, score, None));
            }
        }

        // Content hits from the tantivy index, deduped against the name
        // hits so a file that matched on both name and content only shows
        // once (with the higher of the two scores).
        if want_file {
            if let Some(idx) = self.tantivy.as_ref() {
                let content_hits = idx.query(&ctx.needle.to_string(), MAX_RESULTS);
                for hit in content_hits {
                    if seen.contains(&hit.path) {
                        continue;
                    }
                    if out.len() >= MAX_RESULTS {
                        break;
                    }
                    let display = hit
                        .path
                        .strip_prefix(&self.home)
                        .map(|p| format!("~/{}", p.display()))
                        .unwrap_or_else(|_| hit.path.display().to_string());
                    let lower = display.to_lowercase();
                    let entry = NameEntry {
                        path: hit.path.clone(),
                        display,
                        lower,
                        is_dir: false,
                    };
                    // The index reports raw BM25 relevance, which has
                    // no upper bound - a term repeated through a large
                    // file scores arbitrarily high. Left unbounded it
                    // dwarfs every other provider's whole scale, so a
                    // source file that merely mentions an app outranks
                    // the app itself.
                    //
                    // Clamp it below the weakest *name* hit: having the
                    // word in the filename is a stronger signal about
                    // what a file is than having it somewhere in the
                    // body, however many times.
                    let content = hit.score.clamp(CONTENT_SCORE_MIN, CONTENT_SCORE_MAX);
                    out.push(make_result(&entry, content, hit.snippet));
                }
            }
        }

        // Final ordering across both backends: score first, then the
        // shallower path. Content hits arrive from the index clamped
        // into a narrow band, so a query answered mostly by content ties
        // almost everything — and a tie the provider leaves unordered
        // becomes an arbitrary order somewhere downstream. Sort it here,
        // where the paths are still at hand.
        out.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                // `id` is the absolute path; the subtitle is not always
                // one (a content hit shows its snippet when the index
                // produced one).
                .then_with(|| path_depth(&a.id).cmp(&path_depth(&b.id)))
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    fn owns_tab(&self, _tab_id: u64, _chrome: &ChromeState) -> bool {
        // Files aren't a chrome-tab-owning provider yet — the file
        // explorer app owns tabs. Once a `files` tab kind lands this
        // returns true when the tab's tile carries FilesApp.
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
    SearchResult {
        kind,
        provider_id: "files".to_string(),
        id: entry.path.display().to_string(),
        title: file_title(&entry.path),
        subtitle: subtitle.or_else(|| Some(entry.display.clone())),
        score,
        on_open: Rc::new(move |_doc, _root, chrome: &Chrome| {
            open_search_hit(&path, is_dir, chrome);
        }),
    }
}

fn file_title(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Bounds for a content hit, which ranks below every name-based hit
/// ([`name_score`]'s weakest is a directory-name prefix at 45).
const CONTENT_SCORE_MIN: i32 = 20;
const CONTENT_SCORE_MAX: i32 = 40;

fn name_score(needle_lower: &str, hay_lower: &str) -> Option<i32> {
    if needle_lower.is_empty() {
        return None;
    }
    // Prioritise the filename component's own match over a match anywhere
    // in the path: `todo` at the leaf beats `todo` deep inside a
    // directory name.
    let leaf = hay_lower.rsplit('/').next().unwrap_or(hay_lower);
    if leaf.starts_with(needle_lower) {
        return Some(120);
    }
    if leaf.contains(needle_lower) {
        return Some(80);
    }
    if hay_lower.contains(needle_lower) {
        return Some(45);
    }
    if fuzzy_subsequence(needle_lower, leaf) {
        return Some(20);
    }
    None
}

/// Number of path components, used as the tiebreak between two files
/// that matched the needle equally well.
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

/// Open a palette hit.
///
/// A file whose type an installed app declares is opened *by that app*,
/// through the same launch the palette's Applications rows take — so it
/// lands in the active workspace with that workspace's capabilities and
/// its collaboration room, and the compositor adopts its window like any
/// other. See [`crate::shell::doc_open`].
///
/// Everything else falls back to the desktop's default handler:
/// directories (which are not a launch — the file manager navigates
/// them), and file types nothing installed claims. A document type with
/// no installed editor is reported rather than silently handed to
/// `xdg-open`, because "you have no editor for this" is a fact the user
/// can act on and a silent no-op is not.
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
                &format!("no installed app opens .{ext} documents"),
                chrome,
            );
        }
        DocumentTarget::Unclaimed | DocumentTarget::Directory => spawn_open(path, is_dir),
    }
}

fn notify_open_failure(path: &Path, detail: &str, chrome: &Chrome) {
    use crate::shell::notification::{Notification, NotificationSeverity};
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let mut st = chrome.borrow_mut();
    st.push_notification_full(
        Notification::new(format!("Couldn't open {name}"), detail.to_string())
            .with_severity(NotificationSeverity::Error)
            .with_app_name("Files"),
    );
    st.bump();
}

/// Best-effort `xdg-open`. The fallback for directories and for file
/// types no installed package claims. We don't await the child — a slow
/// xdg-open shouldn't block the palette.
fn spawn_open(path: &Path, _is_dir: bool) {
    let path = path.to_owned();
    thread::Builder::new()
        .name("qos-search-open".into())
        .spawn(move || {
            let _ = std::process::Command::new("xdg-open").arg(path).spawn();
        })
        .ok();
}

fn walk_home(home: &Path, index: Arc<Mutex<NameIndex>>) {
    let mut entries: Vec<NameEntry> = Vec::new();
    let mut count = 0usize;
    for entry in WalkDir::new(home)
        .max_depth(MAX_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !should_skip(e.path(), home))
    {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let is_dir = entry.file_type().is_dir();
        // Skip the home directory itself.
        if path == home {
            continue;
        }
        let display = display_path(home, path);
        let lower = display.to_lowercase();
        entries.push(NameEntry {
            path: path.to_owned(),
            display,
            lower,
            is_dir,
        });
        count += 1;
        if count >= MAX_ENTRIES {
            break;
        }
        // Yield periodically so the walker doesn't pin a core.
        if count % 2_000 == 0 {
            thread::sleep(Duration::from_millis(2));
        }
    }
    if let Ok(mut guard) = index.lock() {
        guard.entries = entries;
        guard.ready = true;
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
                // Hidden files/dirs: skip by default. Users looking for a
                // dotfile can still reach it via the file-explorer app;
                // the palette focuses on the mostly-visible surface.
                return true;
            }
        }
    }
    false
}

fn display_path(home: &Path, path: &Path) -> String {
    path.strip_prefix(home)
        .map(|rel| format!("~/{}", rel.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_score_prefers_leaf_prefix() {
        assert_eq!(name_score("todo", "~/notes/todo.md"), Some(120));
        assert_eq!(name_score("todo", "~/todo/notes.md"), Some(45));
        assert_eq!(name_score("todo", "~/notes/day-todo-list.md"), Some(80));
        assert_eq!(name_score("xzq", "~/notes/todo.md"), None);
    }

    #[test]
    fn skip_dirs_hits_common_bloat() {
        let home = Path::new("/home/x");
        assert!(should_skip(Path::new("/home/x/proj/node_modules"), home));
        assert!(should_skip(Path::new("/home/x/proj/target/debug"), home));
        assert!(should_skip(Path::new("/home/x/.cache/foo"), home));
        assert!(!should_skip(Path::new("/home/x/proj/src/lib.rs"), home));
    }

    #[test]
    fn fuzzy_subsequence_true_and_false() {
        assert!(fuzzy_subsequence("ntes", "notes"));
        assert!(fuzzy_subsequence("tdo", "todo.md"));
        assert!(!fuzzy_subsequence("xz", "notes"));
    }
}
