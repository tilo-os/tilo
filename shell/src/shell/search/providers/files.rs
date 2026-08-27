//! Files provider — searches the user's home directory for files and
//! directories.
//!
//! Two backends behind a shared façade: an in-memory boot-time walk of `~`
//! for name/fuzzy-subsequence queries, and a persistent tantivy index (see
//! [`super::super::files_index`]) over file *contents* at
//! `~/.cache/qos/files-index`, rebuilt incrementally in the background.
//! The palette's Enter callback routes a hit through
//! [`crate::shell::doc_open`], which resolves the extension to the
//! installed package that opens it; directories and unclaimed types fall
//! back to the desktop's default handler.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};


use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::search::{ResourceKind, SearchContext, SearchProvider, SearchResult};

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
    // Package/SDK caches — same category as node_modules/.cargo above:
    // fetched code that crowds out the trees the user actually works in.
    "go/pkg/mod",
    "Android/Sdk",
    "bin/cache",
    "site-packages",
    "dist-packages",
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
    /// Bumped every time the walker replaces `entries`. The narrowing
    /// cache keys on it so a re-scan invalidates candidate lists that
    /// refer to the old entry indices.
    generation: u64,
}

/// The entries that matched the previous needle. Match sets only shrink as
/// the needle grows (`starts_with`, `contains` and subsequence matching are
/// all monotone under extension), so extending a query re-scores only what
/// already matched — one full scan of the 50k-path index per word rather
/// than per character.
struct NarrowCache {
    needle: String,
    candidates: Vec<usize>,
    generation: u64,
}

struct NameEntry {
    path: PathBuf,
    display: String,
    facts: NameFacts,
    is_dir: bool,
}

pub struct FilesProvider {
    home: PathBuf,
    name_index: Arc<Mutex<NameIndex>>,
    tantivy: Option<crate::shell::search::files_index::FilesIndex>,
    /// See [`NarrowCache`]. Behind a `Mutex` because `query` takes
    /// `&self` — the provider trait is shared-reference by design so
    /// the registry can fan out without owning anything mutably.
    narrow: Mutex<Option<NarrowCache>>,
    /// Entries examined by the most recent query. This is the quantity
    /// the per-keystroke cost is proportional to, and unlike a wall
    /// clock it means the same thing in every build profile — so it is
    /// what the narrowing test asserts on.
    last_scanned: AtomicUsize,
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
            narrow: Mutex::new(None),
            last_scanned: AtomicUsize::new(0),
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
    /// See [`MAX_SCORE`]: an exact filename match on a file touched
    /// today, with nothing held against it.
    fn max_score(&self) -> i32 {
        MAX_SCORE
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

        // Score the whole index, then keep the best `MAX_RESULTS` — taking
        // the first N instead would make ranking depend on directory
        // traversal order. Ranking by (score, path) rather than collecting
        // `SearchResult`s avoids a callback allocation and a `PathBuf` clone
        // for every entry that loses.
        if let Ok(guard) = self.name_index.lock() {
            // Narrow from the previous match set when this needle
            // extends the previous one; otherwise sweep the index.
            let mut cache = self.narrow.lock().ok();
            let reuse: Option<&NarrowCache> = cache
                .as_ref()
                .and_then(|c| c.as_ref())
                .filter(|c| {
                    !c.needle.is_empty()
                        && c.generation == guard.generation
                        && needle_lower.starts_with(&c.needle)
                });
            let mut candidates: Vec<usize> = Vec::new();
            let mut ranked: Vec<(i32, usize, usize)> = Vec::new();
            {
                let mut consider = |i: usize| {
                    let entry = &guard.entries[i];
                    let Some(score) = name_score(&needle_lower, &entry.facts) else {
                        return;
                    };
                    // Cached before the kind filter, so toggling the
                    // Files/Folders chips cannot poison the candidate
                    // set for the next keystroke.
                    candidates.push(i);
                    if (entry.is_dir && !want_folder) || (!entry.is_dir && !want_file) {
                        return;
                    }
                    ranked.push((score, path_depth(&entry.facts.lower), i));
                };
                match reuse {
                    Some(c) => {
                        self.last_scanned.store(c.candidates.len(), Ordering::Relaxed);
                        c.candidates.iter().for_each(|&i| consider(i));
                    }
                    None => {
                        self.last_scanned
                            .store(guard.entries.len(), Ordering::Relaxed);
                        (0..guard.entries.len()).for_each(&mut consider);
                    }
                }
            }
            if let Some(slot) = cache.as_mut() {
                **slot = Some(NarrowCache {
                    needle: needle_lower.clone(),
                    candidates,
                    generation: guard.generation,
                });
            }
            // Best score first, shallower path among equals (depth tracks
            // "the user's file" vs "something fetched"). The final compare
            // on path makes this a total order, so the partition below is
            // deterministic.
            let cmp = |a: &(i32, usize, usize), b: &(i32, usize, usize)| {
                b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)).then_with(|| {
                    guard.entries[a.2]
                        .facts
                        .lower
                        .cmp(&guard.entries[b.2].facts.lower)
                })
            };
            // Partition out the best `MAX_RESULTS`, then order only those:
            // sorting the whole candidate set costs 84 ms on a full 50 000-
            // entry index (five frames per character) to then discard all
            // but forty. Selection is linear; the sort that follows is over
            // forty items.
            let k = MAX_RESULTS.min(ranked.len());
            if k < ranked.len() {
                ranked.select_nth_unstable_by(k, cmp);
                ranked.truncate(k);
            }
            ranked.sort_by(cmp);
            for (score, _, i) in ranked.into_iter() {
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
                    let entry = NameEntry {
                        path: hit.path.clone(),
                        facts: NameFacts::new(&display, None),
                        display,
                        is_dir: false,
                    };
                    // Clamp BM25 into the band the name scale reserves
                    // for it — see [`CONTENT_SCORE_MIN`].
                    let content = hit.score.clamp(CONTENT_SCORE_MIN, CONTENT_SCORE_MAX);
                    out.push(make_result(&entry, content, hit.snippet));
                }
            }
        }

        // Final ordering across both backends: score, then shallower path.
        // Content hits arrive clamped into a narrow band, so a content-heavy
        // query ties almost everything, and a tie left unordered here
        // becomes an arbitrary order downstream.
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
    path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string())
}

/// Bounds for a content hit. The band sits at the level of a path-only name
/// hit ([`STRUCT_PATH_SUBSTRING`]) — both rank below any filename match, and
/// both can outrank one the vendored-content penalty pushed down. Clamped
/// because the index reports raw BM25, which is unbounded and would
/// otherwise dwarf every other provider's scale.
const CONTENT_SCORE_MIN: i32 = 20;
const CONTENT_SCORE_MAX: i32 = 40;

// Four independent signals feed the score: structure (where the needle
// landed in the name), length ratio, recency, and provenance/depth
// penalties. Both penalties are *ranking* signals, never filters — the
// floor is 1, so anything that matches at all is still findable.

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

/// Deductions for content that ships *with* software rather than being
/// written by the person searching.
const PENALTY_ASSET_EXT: i32 = 18;
const PENALTY_VENDOR_DIR: i32 = 14;
const PENALTY_GENERATED_LEAF: i32 = 20;
/// Cap on the total. Left uncapped, a font inside a vendored data set
/// would take deductions twice and fall below a fuzzy match, which
/// overstates the case: the file is a poor answer, not a non-answer.
const PENALTY_MAX: i32 = 28;

/// Path components a file may sit under before depth counts against it, and
/// the deduction per component past that. Depth is the one signal that
/// separates somebody's own work (`~/Documents/x.md`) from an application's
/// payload (seven levels into a game's data set) without a name list to
/// maintain. Free for the first [`DEPTH_FREE`] components so ordinary
/// project layouts pay nothing.
const DEPTH_FREE: usize = 4;
const PENALTY_PER_LEVEL: i32 = 3;
const PENALTY_DEPTH_MAX: i32 = 15;

/// Top of the provider's scale — the value [`SearchProvider::max_score`]
/// reports and the registry normalises against. Derived from the parts
/// rather than written twice: an exact filename match, modified today,
/// with nothing held against it.
const MAX_SCORE: i32 = STRUCT_EXACT_LEAF + RATIO_WEIGHT + RECENCY_MAX;

/// Extensions whose files are almost never what someone means when they
/// type a word. Fonts, compiled output, packaged payloads and icon blobs
/// are shipped alongside software; documents and source are not.
const ASSET_EXTS: &[&str] = &[
    // Fonts.
    "ttf", "otf", "ttc", "woff", "woff2", "eot", "pfb", "bdf", "pcf",
    // Compiled / linkable output.
    "so", "a", "o", "dll", "dylib", "obj", "pyc", "pyo", "class", "jar", "wasm", "rlib", "rmeta",
    // Packaged payloads and installers.
    "deb", "rpm", "appimage", "flatpak", "snap", "msi", "exe", "cab", "iso", "img", "dmg", "apk",
    // Game / engine data blobs.
    "grf", "pak", "package", "dat", "bin", "vpk", "wad", "bsp",
    // Icon and cursor blobs. Photographic formats (png/jpg/webp) are
    // deliberately absent — those are usually the user's own.
    "ico", "icns", "xpm", "cur", "bmp",
];

/// Directory names that mark a tree as fetched or generated rather than
/// authored. The heaviest offenders — `node_modules`, `target`, `build`,
/// `dist`, `__pycache__`, `.venv` — never reach scoring at all because
/// [`SKIP_DIRS`] drops them during the walk; these are the ones that
/// survive it.
const VENDOR_DIRS: &[&str] = &[
    "vendor",
    "third_party",
    "thirdparty",
    "site-packages",
    "dist-packages",
    "bower_components",
    "jspm_packages",
    // Shipped data sets of unpacked games and engines (OpenTTD's
    // `baseset`, OpenRCT's `data`-alike layouts).
    "baseset",
    "basesets",
    // Wine / Proton prefixes: a whole Windows install under `~`.
    "drive_c",
    // Generated sources and build scaffolding.
    "generated",
    "autogen",
    "__generated__",
    "cmakefiles",
    "coverage",
    "htmlcov",
];

/// Lockfiles and minified bundles: machine-written, machine-read.
fn is_generated_leaf(leaf_lower: &str) -> bool {
    leaf_lower.ends_with(".lock")
        || leaf_lower.ends_with(".min.js")
        || leaf_lower.ends_with(".min.mjs")
        || leaf_lower.ends_with(".min.css")
        || leaf_lower.ends_with(".map")
        || matches!(
            leaf_lower,
            "package-lock.json" | "npm-shrinkwrap.json" | "go.sum" | "pnpm-lock.yaml"
        )
}

/// Everything a path's score depends on that the needle does not.
/// Precomputed by the walker: a keystroke re-scores tens of thousands of
/// entries, so it must never tokenise a filename or `stat` a file — both
/// happen once per index generation, on the background thread.
pub(crate) struct NameFacts {
    /// The `~/…` display path, lowercased. Path-level tiers match here.
    lower: String,
    /// The final component of [`Self::lower`].
    leaf: Box<str>,
    /// Word ranges within [`Self::leaf`].
    tokens: Box<[(u16, u16)]>,
    /// Length of the leaf with a recognised extension removed; equal to
    /// the leaf's length when there is none.
    stem_len: u16,
    /// Provenance and depth deductions, summed. Both depend only on the
    /// path, so both are folded in here once.
    penalty: i32,
    recency: i32,
}

impl NameFacts {
    /// `display` is the original-case `~/…` path — the case matters,
    /// because camelCase is a word boundary and lowercasing destroys it.
    /// `modified` is the file's mtime; `None` (an unreadable stat) simply
    /// contributes no recency rather than being guessed at.
    pub(crate) fn new(display: &str, modified: Option<SystemTime>) -> Self {
        let lower = display.to_lowercase();
        let leaf_lower = leaf_of(&lower).to_string();
        let leaf_orig = leaf_of(display);
        let stem_len = stem_len(&leaf_lower);
        let tokens = tokenize(leaf_orig, &leaf_lower);
        let penalty = vendor_penalty(&lower, &leaf_lower, stem_len) + depth_penalty(&lower);
        Self {
            leaf: leaf_lower.into_boxed_str(),
            tokens: tokens.into_boxed_slice(),
            stem_len: stem_len.min(u16::MAX as usize) as u16,
            penalty,
            recency: recency_score(modified),
            lower,
        }
    }
}

/// Deduction for how far a path is buried. See [`DEPTH_FREE`].
fn depth_penalty(lower_path: &str) -> i32 {
    let past = path_depth(lower_path).saturating_sub(DEPTH_FREE) as i32;
    (past * PENALTY_PER_LEVEL).min(PENALTY_DEPTH_MAX)
}

fn leaf_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Length of `leaf` up to a recognised extension. "Recognised" is
/// deliberately narrow (short, alphanumeric, something in front): a
/// version-numbered directory like `openttd-jgrpp-0.72.3-linux-generic-amd64`
/// has a dot but no extension, and treating the tail as one would make the
/// stem — the length ratio's denominator — nonsense.
fn stem_len(leaf_lower: &str) -> usize {
    let Some(dot) = leaf_lower.rfind('.') else {
        return leaf_lower.len();
    };
    if dot == 0 {
        return leaf_lower.len();
    }
    let ext = &leaf_lower[dot + 1..];
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
    (stem_len < leaf_lower.len()).then(|| &leaf_lower[stem_len + 1..])
}

/// Split a filename into words, returning byte ranges into its lowercased
/// form. Words break on `-`, `_`, `.`, space and camelCase; the camelCase
/// pass needs the original case, so it is skipped when lowercasing changed
/// any byte length (`İ` → `i̇`) and the offsets no longer line up. The
/// separator split needs no case information and always applies.
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
        let sep = i == bytes.len() || matches!(bytes[i], b'-' | b'_' | b'.' | b' ');
        if sep {
            if let Some(s) = start.take() {
                out.push((s as u16, i as u16));
            }
            continue;
        }
        if start.is_none() {
            start = Some(i);
            continue;
        }
        // Mid-run camelCase / letter-digit boundary: close the current
        // word and open the next one at `i`.
        if camel_ok && is_word_boundary(orig, i) {
            let s = start.replace(i).expect("start is Some in this branch");
            out.push((s as u16, i as u16));
        }
    }
    out
}

/// Is `i` a camelCase (or letter↔digit) word start inside a run?
///
/// Three shapes, all ASCII — a non-ASCII byte is never uppercase, so
/// multibyte characters simply never open a word here.
fn is_word_boundary(b: &[u8], i: usize) -> bool {
    if i == 0 || i >= b.len() {
        return false;
    }
    let (prev, cur) = (b[i - 1], b[i]);
    // `openTtd` — lower/digit then upper.
    if cur.is_ascii_uppercase() && (prev.is_ascii_lowercase() || prev.is_ascii_digit()) {
        return true;
    }
    // `HTTPServer` — the last capital of a run that starts a new word.
    if cur.is_ascii_uppercase()
        && prev.is_ascii_uppercase()
        && b.get(i + 1).is_some_and(|n| n.is_ascii_lowercase())
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
    let mut p = 0;
    if let Some(ext) = extension_of(leaf_lower, stem_len) {
        if ASSET_EXTS.contains(&ext) {
            p += PENALTY_ASSET_EXT;
        }
    }
    if is_generated_leaf(leaf_lower) {
        p += PENALTY_GENERATED_LEAF;
    }
    // Parent components only. A directory *named* `vendor` is a fine
    // answer to someone typing "vendor"; it is the files underneath it
    // that are somebody else's.
    let parent = lower_path
        .rsplit_once('/')
        .map(|(head, _)| head)
        .unwrap_or("");
    if parent.split('/').any(|c| VENDOR_DIRS.contains(&c)) {
        p += PENALTY_VENDOR_DIR;
    }
    p.min(PENALTY_MAX)
}

/// mtime → bounded score contribution. Bands rather than a continuous curve
/// keep the contribution reproducible without pinning a clock. Capped at
/// [`RECENCY_MAX`] of [`MAX_SCORE`] so recency reorders near-equals but
/// never outvotes the name — a freshly unpacked archive gives thousands of
/// files today's mtime.
fn recency_score(modified: Option<SystemTime>) -> i32 {
    let Some(m) = modified else {
        return 0;
    };
    let Ok(age) = SystemTime::now().duration_since(m) else {
        // Dated in the future — a bad clock somewhere, or a file just
        // written across one. "Just touched" is the right reading.
        return RECENCY_MAX;
    };
    const DAY: u64 = 86_400;
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
/// not match. Monotone in the needle — every tier is either "occurs in" or
/// "is a subsequence of", both of which survive dropping trailing
/// characters, which is what makes [`FilesProvider::query`]'s narrowing
/// cache sound.
fn name_score(needle_lower: &str, facts: &NameFacts) -> Option<i32> {
    if needle_lower.is_empty() {
        return None;
    }
    let (structural, denom) = structural_match(needle_lower, facts)?;
    // Clamped at 1.0: an exact leaf match whose extension is part of what
    // was typed (`open.txt`) is longer than the stem it is measured
    // against, and "more than all of it" is still just all of it.
    let ratio = (needle_lower.len() as f32 / denom.max(needle_lower.len()) as f32).min(1.0);
    let ratio_bonus = (RATIO_WEIGHT as f32 * ratio).round() as i32;
    let score = structural + ratio_bonus + facts.recency - facts.penalty;
    // Floored at 1, not at 0: a penalised match is a bad answer, not a
    // hidden one. Whatever the user typed stays reachable.
    Some(score.clamp(1, MAX_SCORE))
}

/// Which structural tier the needle earned, and the length the match
/// ratio should be measured against.
fn structural_match(needle: &str, facts: &NameFacts) -> Option<(i32, usize)> {
    let leaf: &str = &facts.leaf;
    let stem_len = facts.stem_len as usize;
    // One substring scan of the leaf decides whether any of the
    // filename tiers can apply. Most of the index matches nothing, and
    // this is the check that lets those entries out cheaply.
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

/// Best word-boundary tier over every occurrence of `needle` in `leaf`.
///
/// Every occurrence is considered, not just the first: `note` in
/// `my-note-notes.md` lands mid-name once and on a whole word once, and
/// the better reading is the one that counts.
fn leaf_tier(needle: &str, leaf: &str, stem_len: usize, tokens: &[(u16, u16)]) -> i32 {
    let mut best = STRUCT_IN_TOKEN;
    for (at, _) in leaf.match_indices(needle) {
        let end = at + needle.len();
        let starts_word = at == 0 || tokens.iter().any(|&(s, _)| s as usize == at);
        if !starts_word {
            continue;
        }
        let ends_word = end == leaf.len()
            || end == stem_len
            || tokens.iter().any(|&(_, e)| e as usize == end);
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

/// Tier for a match that missed the filename but landed up the path, plus
/// the length to measure the ratio against. Reached only when the path is
/// already known to contain the needle, so the per-component tokenising
/// runs on a handful of entries per keystroke, not the whole index.
fn path_tier(needle: &str, lower_path: &str) -> (i32, usize) {
    let parent = lower_path
        .rsplit_once('/')
        .map(|(head, _)| head)
        .unwrap_or("");
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
            || tokens
                .iter()
                .any(|&(s, _)| comp.get(s as usize..).is_some_and(|t| t.starts_with(needle)));
        if starts_word && best.0 < STRUCT_PATH_TOKEN_PREFIX {
            best = (STRUCT_PATH_TOKEN_PREFIX, comp.len());
        }
    }
    best
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

/// Open a palette hit. A file whose type an installed app declares is opened
/// by that app via [`crate::shell::doc_open`], landing in the active
/// workspace with its capabilities and room. Directories and unclaimed types
/// fall back to the desktop's default handler; a document type with no
/// installed editor is reported rather than silently handed to `xdg-open`.
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

/// Report a failed palette open, attributed to the Files surface the row
/// came from. The reporting itself is shared with every other open
/// surface — see [`crate::shell::doc_open::report_open_failure`].
fn notify_open_failure(path: &Path, detail: &str, chrome: &Chrome) {
    crate::shell::doc_open::report_open_failure(chrome, path, detail, Some("Files"));
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

/// Build the name index, giving every part of `~` a share of it. The walk is
/// breadth-first because a single unpacked game is tens of thousands of
/// paths (`~/Spiele` alone walks to 31 168): depth-first would spend
/// [`MAX_ENTRIES`] on whichever branch it entered first and exclude whole
/// trees, while breadth-first spends the budget on the shallowest paths
/// across all of `~` — the same priority [`DEPTH_FREE`] encodes in the score.
fn walk_home(home: &Path, index: Arc<Mutex<NameIndex>>) {
    let entries = walk_breadth_first(home, MAX_ENTRIES);
    if let Ok(mut guard) = index.lock() {
        guard.entries = entries;
        guard.ready = true;
        guard.generation = guard.generation.wrapping_add(1);
    }
}

/// [`walk_home`]'s walk, with the budget as a parameter so a test can
/// exercise the exhaustion behaviour on a tree it can afford to build.
fn walk_breadth_first(home: &Path, budget: usize) -> Vec<NameEntry> {
    let mut entries: Vec<NameEntry> = Vec::new();
    // Directories still to read, shallowest first.
    let mut frontier: std::collections::VecDeque<(PathBuf, usize)> =
        std::collections::VecDeque::new();
    frontier.push_back((home.to_owned(), 0));

    'walk: while let Some((dir, depth)) = frontier.pop_front() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        // Sorted, so a re-scan of an unchanged tree produces the same
        // index and the narrowing cache's generation bump is the only
        // thing that changed.
        let mut children: Vec<std::fs::DirEntry> = rd.filter_map(Result::ok).collect();
        children.sort_by_key(|c| c.file_name());
        for child in children {
            let path = child.path();
            if should_skip(&path, home) {
                continue;
            }
            // `DirEntry::file_type` is free on Linux (it comes off the
            // `d_type` readdir already returned); `metadata` is the one stat,
            // taken here on the background thread so a keystroke never
            // touches the disk.
            let is_dir = child.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let modified = child.metadata().ok().and_then(|m| m.modified().ok());
            if is_dir && depth + 1 < MAX_DEPTH {
                frontier.push_back((path.clone(), depth + 1));
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
            if entries.len() % 2_000 == 0 {
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
    entries
}

/// Does any pattern in [`SKIP_DIRS`] match this path, or is any component
/// hidden? Patterns match a component at a time, and one containing `/`
/// must match that many consecutive components — the list carries
/// multi-component patterns (`.config/gnome`, `Downloads/tmp`).
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
            // Hidden files/dirs: skip by default. Users looking for a
            // dotfile can still reach it via the file-explorer app;
            // the palette focuses on the mostly-visible surface.
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
    path.strip_prefix(home)
        .map(|rel| format!("~/{}", rel.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Facts for a path with no mtime, so a score built from them is the
    /// structural + ratio + penalty part alone. Recency is exercised
    /// separately by the tests that pass an age.
    fn facts(display: &str) -> NameFacts {
        NameFacts::new(display, None)
    }

    /// Facts for a path last modified `days` ago.
    fn aged_facts(display: &str, days: u64) -> NameFacts {
        NameFacts::new(
            display,
            Some(SystemTime::now() - Duration::from_secs(days * 86_400 + 3_600)),
        )
    }

    #[test]
    fn name_score_prefers_the_leaf_over_the_path() {
        // The whole filename minus its extension.
        assert_eq!(
            name_score("todo", &facts("~/notes/todo.md")),
            Some(STRUCT_EXACT_STEM + RATIO_WEIGHT)
        );
        // A whole word inside a longer filename.
        assert_eq!(
            name_score("todo", &facts("~/notes/day-todo-list.md")),
            Some(STRUCT_TOKEN + 5)
        );
        // Only a directory name matched.
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
        for r in out.iter().filter(|r| r.title.ends_with(".ttf")) {
            assert!(
                r.score < doc_score,
                "{} scored {} against the document's {doc_score}: {:?}",
                r.title,
                r.score,
                listing()
            );
        }
        // Not merely below the document — below every non-asset hit, so
        // no run of them can occupy the top of the card again.
        let first_font = out
            .iter()
            .position(|r| r.title.ends_with(".ttf"))
            .expect("the fonts are still in the results");
        let last_non_asset = out
            .iter()
            .rposition(|r| !r.title.ends_with(".ttf") && !r.title.ends_with(".grf"))
            .expect("some non-asset hit");
        assert!(
            first_font > last_non_asset,
            "a font appears at {first_font}, above the last non-asset hit at \
             {last_non_asset}: {:?}",
            listing()
        );
        // Still findable: demotion is a ranking signal, not a filter.
        assert_eq!(
            out.iter().filter(|r| r.title.ends_with(".ttf")).count(),
            4,
            "all four fonts must still be reachable: {:?}",
            listing()
        );
    }

    /// Word boundaries, the signal the step function had no room for.
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

    /// Four characters of a sixteen-character name is a weaker claim
    /// than four characters of five.
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

    /// camelCase is a word boundary: `Open` in `OpenTTD` is a word even
    /// though nothing separates it from `TTD`.
    #[test]
    fn tokenize_splits_on_separators_and_camel_case() {
        let words = |s: &str| {
            let lower = s.to_lowercase();
            tokenize(s, &lower)
                .into_iter()
                .map(|(a, b)| lower[a as usize..b as usize].to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(words("OpenTTD-Sans.ttf"), ["open", "ttd", "sans", "ttf"]);
        assert_eq!(words("my_notes file.md"), ["my", "notes", "file", "md"]);
        assert_eq!(words("HTTPServerLog"), ["http", "server", "log"]);
        assert_eq!(words("report2026final"), ["report", "2026", "final"]);
        assert_eq!(words("openapi.yaml"), ["openapi", "yaml"]);
    }

    /// The extension test has to stay narrow, or a version-numbered
    /// directory loses most of its name to a bogus "extension" and the
    /// length ratio it feeds becomes meaningless.
    #[test]
    fn stem_length_ignores_a_version_number_masquerading_as_an_extension() {
        assert_eq!(stem_len("openttd-jgrpp-0.72.3-linux-generic-amd64"), 40);
        assert_eq!(stem_len("open.md"), 4);
        assert_eq!(stem_len("archive.tar.gz"), 11);
        assert_eq!(stem_len("noextension"), 11);
        assert_eq!(stem_len(".hidden"), 7);
    }

    /// Vendored content sinks but stays reachable — a penalty, not a
    /// filter. Someone who types the vendored file's own name means it.
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

    /// …and bounded low enough that it cannot overrule the name. An
    /// archive unpacked this morning gives thousands of files today's
    /// mtime; none of them should displace a file actually called what
    /// was typed.
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

    /// The mtime band is read from the precomputed facts, never stat'ed
    /// per keystroke — proven by scoring a path that does not exist: a
    /// scorer that reached for the disk would find nothing there.
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

    /// `max_score()` has to be the real ceiling: [`SearchRegistry::query`]
    /// divides by it, and a provider that outgrows its declared top
    /// normalises above 1.0 and silently takes over the palette.
    #[test]
    fn max_score_is_the_honest_ceiling() {
        // The best case the scale can produce, reached by the production
        // scorer rather than asserted arithmetically.
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

    /// A pattern with a `/` in it has to match consecutive components.
    /// The list carried two such patterns from the start and the
    /// single-component equality test could never match either, so they
    /// did nothing at all.
    #[test]
    fn multi_component_skip_patterns_match() {
        let home = Path::new("/home/x");
        assert!(
            SKIP_DIRS.contains(&"Downloads/tmp"),
            "fixture precondition: the list still carries a multi-component \
             pattern"
        );
        assert!(should_skip(Path::new("/home/x/Downloads/tmp/a.iso"), home));
        assert!(should_skip(Path::new("/home/x/go/pkg/mod/rsc.io/quote"), home));
        assert!(should_skip(Path::new("/home/x/Android/Sdk/platforms"), home));
        // …and only when the components are adjacent and in order.
        assert!(!should_skip(Path::new("/home/x/Downloads/keep.iso"), home));
        assert!(!should_skip(Path::new("/home/x/go/src/mod/thing.go"), home));
        assert!(!should_skip(Path::new("/home/x/tmp/Downloads/a.iso"), home));
        // A directory the user might plausibly own, unaffected.
        assert!(!should_skip(Path::new("/home/x/Android/notes.md"), home));
    }

    /// How far down a path sits counts against it, so an application's
    /// payload does not outrank the user's own work on the strength of a
    /// filename alone. Free for the first few levels.
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
        // Bounded: depth alone can never sink a match out of reach.
        assert_eq!(
            depth_penalty("~/a/b/c/d/e/f/g/h/i/j/k"),
            PENALTY_DEPTH_MAX
        );
    }

    /// Breadth-first spends the budget on the shallowest paths
    /// everywhere, so every top-level tree is represented.
    #[test]
    fn the_walk_indexes_every_tree_before_it_runs_out() {
        let home = tempfile::tempdir().expect("tempdir");
        let root = home.path();
        // One enormous branch and four small ones, laid out so a
        // depth-first walk meets the big one first.
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

        // A budget far smaller than the huge branch.
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

    /// Build a provider whose name index is already populated, without
    /// waiting on the background walk or touching the disk.
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

    /// [`provider_with_index`] with an mtime age in days per entry, for
    /// the tests that exercise the recency signal.
    fn provider_with_index_aged(entries: Vec<(&str, bool, u64)>) -> FilesProvider {
        provider_from_entries(
            entries
                .into_iter()
                .map(|(display, is_dir, days)| NameEntry {
                    path: PathBuf::from(display.replace('~', "/home/tester")),
                    facts: NameFacts::new(
                        display,
                        Some(SystemTime::now() - Duration::from_secs(days * 86_400 + 3_600)),
                    ),
                    display: display.to_string(),
                    is_dir,
                })
                .collect(),
        )
    }

    fn provider_from_entries(entries: Vec<NameEntry>) -> FilesProvider {
        let home = PathBuf::from("/home/tester");
        FilesProvider {
            home,
            name_index: Arc::new(Mutex::new(NameIndex {
                entries,
                ready: true,
                generation: 1,
            })),
            tantivy: None,
            narrow: Mutex::new(None),
            last_scanned: AtomicUsize::new(0),
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

    /// The best matches win, not the ones the walker happened to reach
    /// first. Stopping at the first `MAX_RESULTS` made relevance depend
    /// on directory traversal order — a query with more matches than the
    /// cap never scored the entries past it at all.
    #[test]
    fn the_best_matches_win_regardless_of_walk_position() {
        // Fifty weak matches first (the needle is only in the path), then
        // the strong one (the needle starts the filename) last.
        let mut entries: Vec<(String, bool)> = (0..50)
            .map(|i| (format!("~/report/notes/entry-{i:03}.md"), false))
            .collect();
        entries.push(("~/archive/report-final.md".to_string(), false));
        let owned: Vec<(&str, bool)> =
            entries.iter().map(|(s, d)| (s.as_str(), *d)).collect();
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

    /// Equally-good matches order by how shallow the path is, so a hit
    /// in the user's own tree beats one buried inside something they
    /// cloned.
    #[test]
    fn equal_scores_prefer_the_shallower_path() {
        // Same filename, same age, same (absent) penalty — so the two
        // scores really are equal and depth is the only thing left to
        // decide with. Both sit inside [`DEPTH_FREE`], where depth costs
        // nothing; a vendored or deeper path would have been separated by
        // its penalty instead and this test would have proved nothing.
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

    /// Build the worst-case index the walker is allowed to produce.
    fn full_index_paths() -> Vec<String> {
        (0..MAX_ENTRIES)
            .map(|i| {
                format!(
                    "~/Development/project-{}/src/module_{}/component_{i}.rs",
                    i % 97,
                    i % 31
                )
            })
            .collect()
    }

    /// Typing a word must cost one sweep of the index, not one per character.
    /// Asserted on entries *examined* rather than elapsed time, so the count
    /// means the same in debug, release and on a loaded CI box. (One sweep of
    /// this 50 000-entry index measures 6.1 ms in release.)
    #[test]
    fn typing_a_word_narrows_instead_of_re_sweeping() {
        // A corpus where each further character genuinely rules entries
        // out — which is what a home directory looks like, and what the
        // narrowing exists to exploit.
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
            p.query(ctx_for(&word[..end], &[ResourceKind::File], &st, &doc));
            scanned.push(p.last_scanned.load(Ordering::Relaxed));
        }

        assert_eq!(
            scanned[0], MAX_ENTRIES,
            "the first character has nothing to narrow from and must sweep"
        );
        // The work per keystroke only ever falls. A one- or two-letter
        // needle can still legitimately match the whole index — the
        // saving arrives once the query becomes selective, not at the
        // second character.
        for (i, w) in scanned.iter().enumerate().skip(1) {
            assert!(
                *w <= scanned[i - 1],
                "character {i} examined more entries ({w}) than character \
                 {} did ({}) — match sets only shrink, so narrowing must \
                 never grow the work: {scanned:?}",
                i - 1,
                scanned[i - 1]
            );
        }
        let last = *scanned.last().unwrap();
        assert!(
            last * 10 < MAX_ENTRIES,
            "the last character of a 9-letter word still examined {last} of \
             {MAX_ENTRIES} entries: {scanned:?}"
        );
        let total: usize = scanned.iter().sum();
        let without_narrowing = MAX_ENTRIES * word.len();
        assert!(
            total * 2 < without_narrowing,
            "typing {} characters examined {total} entries; re-sweeping every \
             keystroke would be {without_narrowing}, so the narrowing is not \
             earning its keep: {scanned:?}",
            word.len()
        );
    }

    /// Filenames used to build a corpus that actually narrows.
    const WORDS: &[&str] = &[
        "component", "compose", "common", "config", "codec", "buffer", "render",
        "layout", "widget", "theme", "search", "palette", "index", "walker",
    ];

    /// Extending a needle must return exactly what a cold query for that
    /// needle returns. Narrowing is only sound because match sets shrink
    /// as the needle grows; this is the assertion that keeps a future
    /// scoring rule from quietly breaking that.
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
            // Warm: reached by typing one character at a time.
            p.narrow.lock().unwrap().take();
            let mut warm = Vec::new();
            for end in 1..=prefix.len() {
                warm = p.query(ctx_for(&prefix[..end], &kinds, &st, &doc));
            }
            // Cold: the same needle with no cache at all.
            p.narrow.lock().unwrap().take();
            let cold = p.query(ctx_for(prefix, &kinds, &st, &doc));

            let ids = |v: &[SearchResult]| {
                v.iter().map(|r| r.id.clone()).collect::<Vec<_>>()
            };
            assert_eq!(
                ids(&warm),
                ids(&cold),
                "typing into {prefix:?} produced a different result set than \
                 querying it outright"
            );
        }
    }

    /// A re-scan replaces the entry list, so candidate indices from
    /// before it are meaningless and must not be reused.
    #[test]
    fn a_reindex_invalidates_the_narrowing_cache() {
        let p = provider_with_index(vec![("~/alpha.md", false), ("~/alpha-two.md", false)]);
        let chrome = empty_chrome();
        let st = chrome.borrow();
        let doc = automerge::AutoCommit::new();
        assert_eq!(p.query(ctx_for("alpha", &[ResourceKind::File], &st, &doc)).len(), 2);

        // The walker commits a completely different set.
        {
            let mut guard = p.name_index.lock().unwrap();
            guard.entries = vec![NameEntry {
                path: PathBuf::from("/home/tester/alphabet.md"),
                display: "~/alphabet.md".into(),
                facts: NameFacts::new("~/alphabet.md", None),
                is_dir: false,
            }];
            guard.generation = guard.generation.wrapping_add(1);
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

    #[test]
    fn fuzzy_subsequence_true_and_false() {
        assert!(fuzzy_subsequence("ntes", "notes"));
        assert!(fuzzy_subsequence("tdo", "todo.md"));
        assert!(!fuzzy_subsequence("xz", "notes"));
    }
}
