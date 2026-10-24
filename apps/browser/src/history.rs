//! Persistent browsing history + fuzzy address-bar ranking.
//!
//! The store is a flat list of [`HistoryEntry`] rows keyed by URL. Every
//! navigation calls [`History::record_at`], which de-duplicates by URL
//! (bumping the visit count and refreshing the title + timestamp) so the
//! list stays one-row-per-page. It is the person's own record
//! ([`BrowsingHistory`]): it follows them to their other devices and never
//! reaches a collaborator, whatever room the browser is open in.
//!
//! [`History::suggest`] powers the address-bar overlay. It fuzzy-matches
//! the query as a case-insensitive *subsequence* of each entry's title
//! and URL, then ranks survivors by a blend of three signals:
//!
//! 1. **match quality** — a contiguous run (or a prefix / word-boundary
//!    start) scores higher than the same characters scattered with gaps,
//!    so typing `git` surfaces `github.com` above `go-i-t.example`; a
//!    match inside a word needs four letters (`code` finds `barcode`);
//! 2. **recency** — a half-life decay on `last_visit`, so a page seen
//!    minutes ago outranks a stale one of equal match quality;
//! 3. **frequency** — a saturating function of `visit_count`, so a
//!    frequently-visited page edges out a one-off.
//!
//! Match quality dominates (it carries the largest weight) so a clearly
//! better textual match always wins; recency and frequency only break
//! ties between comparable matches. An empty query skips the fuzzy filter
//! and ranks the whole history by recency + frequency alone — the
//! "most-recent / most-visited" default the overlay shows the moment the
//! address bar is focused.

use std::collections::BTreeMap;
use std::sync::Arc;
use tilo_clock::unix_now;
use ui::search::{FieldWeight, ListIndex, MAX_FIELD_RANK};
use ui::tilo_resource::{self, Atom, Counter, Model, Refusal, resource};

/// Keep history useful without letting a long-lived profile turn every
/// address-bar query, resource snapshot, or persisted JSON write into an
/// unbounded operation.
pub const MAX_HISTORY_ENTRIES: usize = 10_000;
pub const MAX_HISTORY_URL_BYTES: usize = 8 * 1024;
pub const MAX_HISTORY_TITLE_BYTES: usize = 4 * 1024;

/// One visited page. De-duplicated by [`HistoryEntry::url`] in the store.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    /// Canonical URL as navigated (the store's dedup key).
    pub url: String,
    /// Last-seen page title. Refreshed on re-visit when non-empty so a
    /// later `TitleChanged` doesn't wipe a good title with a blank one.
    pub title: String,
    /// Unix-epoch seconds of the most recent visit — the recency signal.
    pub last_visit: u64,
    /// Number of times this URL has been recorded — the frequency signal.
    pub visit_count: u32,
}

/// The full browsing history: a flat, URL-keyed list of [`HistoryEntry`].
#[derive(Clone, Debug, Default)]
pub struct History {
    entries: Vec<HistoryEntry>,
    /// Changes only when the durable corpus changes.  Consumers such as the
    /// global-search index can therefore make an unchanged sync O(1).
    revision: u64,
    /// Titles and URLs, indexed while they stay the same.
    search: Arc<ListIndex>,
}

// The revision is an in-memory cache token rather than part of history's
// durable model value.
impl PartialEq for History {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

// --- Ranking weights ----------------------------------------------------
// Match quality carries the largest weight so a clearly better textual
// match always outranks a worse one regardless of recency/frequency;
// recency and frequency only order comparable matches.
struct Weights {
    matching: f64,
    recency: f64,
    frequency: f64,
}
const WEIGHTS: Weights = Weights {
    matching: 1.0,
    recency: 0.3,
    frequency: 0.2,
};

/// Recency half-life: a page loses half its recency weight every this
/// many seconds (3 days). Long enough that yesterday's pages still rank,
/// short enough that today's clearly win.
const RECENCY_HALF_LIFE_SECS: f64 = 3.0 * 24.0 * 60.0 * 60.0;

impl History {
    /// An empty history (no persistence side-effects).
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            revision: 0,
            search: Arc::default(),
        }
    }

    /// Drop all history (the settings "clear history" action).
    pub fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.entries.clear();
            self.revision = self.revision.wrapping_add(1);
        }
    }

    /// All entries, unordered (insertion order). Mostly for inspection
    /// and tests — the address bar goes through [`History::suggest`].
    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    /// Number of distinct URLs recorded.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no page has been recorded yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Monotonic-in-practice change token for cached projections.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Record a visit to `url` with `title`, stamped at `now` (Unix
    /// seconds), so ranking/recency behaviour is unit-testable without
    /// touching the wall clock.
    ///
    /// De-dup rule: if `url` already exists, bump its `visit_count`,
    /// set `last_visit = now`, and replace the title *only when the new
    /// one is non-empty*. Otherwise append a fresh entry with count 1.
    /// Blank/whitespace URLs are ignored.
    pub fn record_at(&mut self, url: &str, title: &str, now: u64) {
        let url = url.trim();
        if url.is_empty() || url.len() > MAX_HISTORY_URL_BYTES {
            return;
        }
        let title = bounded_text(title, MAX_HISTORY_TITLE_BYTES);
        if let Some(existing) = self.entries.iter_mut().find(|e| e.url == url) {
            existing.visit_count = existing.visit_count.saturating_add(1);
            existing.last_visit = now;
            if !title.trim().is_empty() {
                existing.title = title;
            }
        } else {
            self.entries.push(HistoryEntry {
                url: url.to_string(),
                title,
                last_visit: now,
                visit_count: 1,
            });
        }
        self.trim_to_limit();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Fuzzy-ranked address-bar suggestions for `query`, best first,
    /// capped at `limit`. Stamped at the current wall clock — see
    /// [`History::suggest_at`] for the deterministic core.
    pub fn suggest(&self, query: &str, limit: usize) -> Vec<HistoryEntry> {
        self.suggest_at(query, limit, unix_now())
    }

    /// Deterministic core of [`History::suggest`]: rank against `now`
    /// (Unix seconds, drives the recency decay) so ordering is testable.
    ///
    /// A non-empty `query` keeps only entries whose title *or* URL
    /// contains the query as a case-insensitive subsequence; each
    /// survivor's match score is the better of its title/URL match. An
    /// empty query keeps everything with a neutral match score, so the
    /// result is pure recency + frequency (the focus-time default).
    ///
    /// Ties (equal blended score) fall back to most-recent, then
    /// most-visited, then URL — a total order so results never jitter
    /// between frames for the same inputs.
    pub fn suggest_at(&self, query: &str, limit: usize, now: u64) -> Vec<HistoryEntry> {
        let needle = query.trim();
        let mut scored: Vec<(f64, &HistoryEntry)> = Vec::new();
        // Title and URL are both things a person types from memory, so they
        // weigh the same. An empty needle ranks every entry 0: purely on
        // recency and frequency.
        let ranks = self.search.ranks(
            needle,
            self.entries.iter().map(|entry| {
                [
                    (entry.title.as_str(), FieldWeight::TITLE),
                    (entry.url.as_str(), FieldWeight::TITLE),
                ]
            }),
        );
        for (entry, rank) in self.entries.iter().zip(ranks) {
            // An entry matching neither field is dropped.
            let Some(rank) = rank else {
                continue;
            };
            let match_score = f64::from(rank) / f64::from(MAX_FIELD_RANK);

            let score = WEIGHTS.matching * match_score
                + WEIGHTS.recency * recency_score(entry.last_visit, now)
                + WEIGHTS.frequency * frequency_score(entry.visit_count);
            scored.push((score, entry));
        }

        scored.sort_by(|a, b| {
            // Primary: blended score, descending.
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Deterministic tie-breaks: recent, then frequent, then URL.
                .then(b.1.last_visit.cmp(&a.1.last_visit))
                .then(b.1.visit_count.cmp(&a.1.visit_count))
                .then(a.1.url.cmp(&b.1.url))
        });

        scored
            .into_iter()
            .take(limit)
            .map(|(_, e)| e.clone())
            .collect()
    }

    /// The history of `entries`, as the person's record holds them.
    pub(crate) fn from_entries(entries: Vec<HistoryEntry>) -> History {
        let mut history = History {
            entries,
            ..History::new()
        };
        history.trim_to_limit();
        history
    }

    pub(crate) fn from_imported_entries(entries: Vec<HistoryEntry>) -> History {
        let mut history = History {
            entries,
            ..History::new()
        };
        history.trim_to(crate::continuity::MAX_BUNDLE_ENTRIES.saturating_add(MAX_HISTORY_ENTRIES));
        history
    }
    fn trim_to(&mut self, limit: usize) {
        if self.entries.len() <= limit {
            return;
        }
        self.entries.sort_by(|a, b| {
            b.last_visit
                .cmp(&a.last_visit)
                .then_with(|| b.visit_count.cmp(&a.visit_count))
                .then_with(|| a.url.cmp(&b.url))
        });
        self.entries.truncate(limit);
    }
    fn trim_to_limit(&mut self) {
        if self.entries.len() <= MAX_HISTORY_ENTRIES {
            return;
        }
        // Drop the least useful entry first.  The URL tie-break keeps the
        // retained corpus deterministic even when timestamps are identical.
        self.entries.sort_by(|a, b| {
            b.last_visit
                .cmp(&a.last_visit)
                .then_with(|| b.visit_count.cmp(&a.visit_count))
                .then_with(|| a.url.cmp(&b.url))
        });
        self.entries.truncate(MAX_HISTORY_ENTRIES);
    }
}

/// Copy at most `limit` bytes without splitting a UTF-8 code point.  Labels
/// cross the accessibility and global-search boundaries, so this protects
/// more than the on-disk representation.
pub(crate) fn bounded_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.get(..end).unwrap_or_default().to_string()
}

/// Recency weight in `[0, 1]`: `1.0` for a page visited *now*, halving
/// every [`RECENCY_HALF_LIFE_SECS`]. `now` before `last_visit` (clock
/// skew) clamps to `1.0`.
fn recency_score(last_visit: u64, now: u64) -> f64 {
    let age = now.saturating_sub(last_visit) as f64;
    0.5_f64.powf(age / RECENCY_HALF_LIFE_SECS)
}

/// Frequency weight in `[0, 1)`: saturating so early visits matter most
/// and a very popular page can't dwarf match quality. `1 → 0.5`,
/// `4 → 0.8`, `9 → 0.9`.
fn frequency_score(visit_count: u32) -> f64 {
    let n = f64::from(visit_count.max(1));
    1.0 - 1.0 / (1.0 + n)
}

/// The visits the address suggestions show in a screenshot.
pub(crate) fn sample_entries() -> Vec<HistoryEntry> {
    vec![
        HistoryEntry {
            url: "https://github.com/tilo/tilo".into(),
            title: "Tilo on code.tilo.so".into(),
            last_visit: 3,
            visit_count: 12,
        },
        HistoryEntry {
            url: "https://en.wikipedia.org/wiki/Web_browser".into(),
            title: "Web browser - Wikipedia".into(),
            last_visit: 2,
            visit_count: 5,
        },
        HistoryEntry {
            url: "https://duckduckgo.com/?q=rust".into(),
            title: "rust at DuckDuckGo".into(),
            last_visit: 1,
            visit_count: 3,
        },
    ]
}

/// One visited page in the person's record.
#[derive(Model, Clone, Debug, PartialEq)]
pub struct Visit {
    /// Title: the last page title seen. Written whole.
    #[model(label)]
    pub title: Atom,
    /// When it was last visited, in Unix seconds.
    pub last: u64,
    /// How often it was visited: visits on two devices add up.
    pub count: Counter,
}

/// The person's browsing history as a resource of their own room: each
/// visited page, keyed by URL, with its title, last visit and visit count.
/// It follows them to their other devices and never reaches a
/// collaborator.
#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = BrowsingHistory::valid_imports)]
pub struct BrowsingHistory {
    /// Visited pages, by URL. Only visiting and forgetting change them.
    #[model(readonly)]
    pub visits: BTreeMap<String, Visit>,
    /// Immutable imported contributions, keyed by source and URL. Import receipts
    /// and tombstones stay in the person's record when visits are forgotten.
    #[model(readonly)]
    pub imported: BTreeMap<String, ImportedVisit>,
    #[model(readonly)]
    pub import_receipts: BTreeMap<String, Atom>,
    #[model(readonly)]
    pub forgotten_imports: tilo_resource::Set<String>,
}

#[derive(Model, Clone, Debug, Default, PartialEq)]
#[model(check = ImportedVisit::valid)]
pub struct ImportedVisit {
    pub url: Atom,
    pub title: Atom,
    pub last: u64,
    pub count: u32,
}

impl ImportedVisit {
    fn valid(&self) -> Result<(), Refusal> {
        if crate::actions::normalized_bookmark_url(&self.url.0).as_deref()
            != Some(self.url.0.as_str())
            || self.title.0.len() > MAX_HISTORY_TITLE_BYTES
            || self.count == 0
        {
            return Err(Refusal::new("Invalid imported private visit"));
        }
        Ok(())
    }
}

impl BrowsingHistory {
    fn valid_imports(&self) -> Result<(), Refusal> {
        let hash = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if self.imported.len() > crate::continuity::MAX_BUNDLE_ENTRIES
            || self.import_receipts.len() > 1024
            || self
                .forgotten_imports
                .iter()
                .any(|key| !self.imported.contains_key(key))
        {
            return Err(Refusal::new("Invalid private history import ledger"));
        }
        for (key, row) in &self.imported {
            let Some((source, url)) = key.split_once(':') else {
                return Err(Refusal::new("Invalid imported visit identity"));
            };
            if !hash(source)
                || url != blake3::hash(row.url.0.as_bytes()).to_hex().as_str()
                || !self.import_receipts.contains_key(source)
            {
                return Err(Refusal::new("Invalid imported visit receipt"));
            }
        }
        if self.import_receipts.iter().any(|(key, digest)| {
            !hash(key.strip_prefix("bundle:").unwrap_or(key)) || !hash(&digest.0)
        }) {
            return Err(Refusal::new("Invalid private history receipt"));
        }
        Ok(())
    }
}

#[resource(kind = "browser.browsing-history", label = "Browsing history")]
impl BrowsingHistory {
    pub fn entries(&self) -> Vec<HistoryEntry> {
        History::of(self).entries().to_vec()
    }
    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }

    /// Record a visit to a page.
    ///
    /// Recording a visit is the browser keeping its history, not an edit of
    /// the person's to take back.
    #[action(housekeeping)]
    pub fn visit(
        &mut self,
        #[param("The page's address")] url: String,
        #[param("The page's title; blank keeps the one known")] title: String,
        #[param("When it finished loading, in Unix seconds")] at: u64,
    ) -> Result<(), Refusal> {
        let url = url.trim();
        if url.is_empty() || url.len() > MAX_HISTORY_URL_BYTES {
            return Err(Refusal::new("a visit names one page's URL"));
        }
        let title = bounded_text(&title, MAX_HISTORY_TITLE_BYTES);
        if let Some(visit) = self.visits.get_mut(url) {
            if !title.trim().is_empty() {
                visit.title = Atom(title);
            }
            visit.last = visit.last.max(at);
            visit.count.add(1);
        } else {
            let mut count = Counter::default();
            count.add(1);
            self.visits.insert(
                url.to_owned(),
                Visit {
                    title: Atom(title),
                    last: at,
                    count,
                },
            );
        }
        // Keep the most useful pages; the rest fall off.
        if self.visits.len() > MAX_HISTORY_ENTRIES {
            let kept: std::collections::BTreeSet<String> = History::from_entries(
                self.visits
                    .iter()
                    .map(|(url, visit)| HistoryEntry {
                        url: url.clone(),
                        title: visit.title.0.clone(),
                        last_visit: visit.last,
                        visit_count: u32::try_from(visit.count.get()).unwrap_or(u32::MAX),
                    })
                    .collect(),
            )
            .entries
            .into_iter()
            .map(|entry| entry.url)
            .collect();
            self.visits.retain(|url, _| kept.contains(url));
        }
        Ok(())
    }

    /// Import a complete reviewed private history snapshot. All conflicts and
    /// capacity checks precede edits; an identical retry never recounts visits.
    #[action]
    pub fn import_history_bundle(
        &mut self,
        #[param(secret, "Reviewed private history snapshot")] bundle: String,
    ) -> Result<(), Refusal> {
        let bundle = crate::continuity::ImportBundle::decode(&bundle)
            .map_err(|_| Refusal::new("Invalid private history import"))?;
        if bundle.imported_tree.is_some()
            || bundle
                .chunks
                .iter()
                .any(|chunk| !chunk.bookmarks.is_empty())
        {
            return Err(Refusal::new("Bookmarks belong to their private library"));
        }
        let bundle_key = format!("bundle:{}", blake3::Hash::from(bundle.source).to_hex());
        let bundle_digest = blake3::hash(
            &serde_json::to_vec(&bundle)
                .map_err(|_| Refusal::new("Invalid private history import"))?,
        )
        .to_hex()
        .to_string();
        if let Some(receipt) = self.import_receipts.get(&bundle_key) {
            return if receipt.0 == bundle_digest {
                Ok(())
            } else {
                Err(Refusal::new(
                    "Browser source receipt conflicts with this import",
                ))
            };
        }
        let mut pending = Vec::new();
        let mut added = 0usize;
        for chunk in &bundle.chunks {
            let source = blake3::Hash::from(chunk.source).to_hex().to_string();
            let digest = blake3::hash(
                &serde_json::to_vec(chunk)
                    .map_err(|_| Refusal::new("Invalid private history import"))?,
            )
            .to_hex()
            .to_string();
            if let Some(receipt) = self.import_receipts.get(&source) {
                if receipt.0 != digest {
                    return Err(Refusal::new(
                        "Browser source receipt conflicts with this import",
                    ));
                }
                continue;
            }
            added = added.saturating_add(chunk.history.len());
            pending.push((source, digest, chunk));
        }
        if self
            .import_receipts
            .len()
            .saturating_add(pending.len())
            .saturating_add(1)
            > 1024
            || self
                .imported
                .len()
                .saturating_add(added)
                .saturating_add(self.visits.len())
                > crate::continuity::MAX_BUNDLE_ENTRIES
        {
            return Err(Refusal::new(
                "Private history import exceeds its bounded capacity",
            ));
        }
        for (source, digest, chunk) in pending {
            for row in &chunk.history {
                let key = format!("{source}:{}", blake3::hash(row.url.as_bytes()).to_hex());
                self.imported.insert(
                    key,
                    ImportedVisit {
                        url: Atom(row.url.clone()),
                        title: Atom(row.title.clone()),
                        last: row.last_visit,
                        count: row.visit_count,
                    },
                );
            }
            self.import_receipts.insert(source, Atom(digest));
        }
        self.import_receipts.insert(bundle_key, Atom(bundle_digest));
        Ok(())
    }

    /// Name a visited page recorded before its title was known: a page a
    /// web app moved to without loading one, or one that names itself once
    /// it has loaded. A page that has a title keeps it.
    ///
    /// Like recording the visit, this is the browser keeping its history,
    /// not an edit of the person's to take back.
    #[action(housekeeping)]
    pub fn name_visit(
        &mut self,
        #[param("The page's address")] url: String,
        #[param("The page's title")] title: String,
    ) -> Result<(), Refusal> {
        let title = bounded_text(&title, MAX_HISTORY_TITLE_BYTES);
        if title.trim().is_empty() {
            return Err(Refusal::new("a page is named by a title"));
        }
        if let Some(visit) = self.visits.get_mut(url.trim())
            && visit.title.0.trim().is_empty()
        {
            visit.title = Atom(title);
        }
        Ok(())
    }

    /// Forget one page.
    #[action]
    pub fn forget(&mut self, #[param("The page's address")] url: String) {
        self.visits.remove(&url);
        for (key, row) in &self.imported {
            if row.url.0 == url {
                self.forgotten_imports.insert(key.clone());
            }
        }
    }

    /// Forget every page.
    #[action]
    pub fn clear_history(&mut self) {
        self.visits.clear();
        for key in self.imported.keys() {
            self.forgotten_imports.insert(key.clone());
        }
    }
}

impl History {
    /// The history the person's record holds.
    pub fn of(record: &BrowsingHistory) -> History {
        let mut rows: BTreeMap<String, HistoryEntry> = record
            .visits
            .iter()
            .map(|(url, visit)| {
                (
                    url.clone(),
                    HistoryEntry {
                        url: url.clone(),
                        title: visit.title.0.clone(),
                        last_visit: visit.last,
                        visit_count: u32::try_from(visit.count.get()).unwrap_or(u32::MAX),
                    },
                )
            })
            .collect();
        for (key, row) in &record.imported {
            if record.forgotten_imports.contains(key) {
                continue;
            }
            rows.entry(row.url.0.clone())
                .and_modify(|entry| {
                    entry.visit_count = entry.visit_count.saturating_add(row.count);
                    if row.last > entry.last_visit {
                        entry.last_visit = row.last;
                        if !row.title.0.trim().is_empty() {
                            entry.title.clone_from(&row.title.0);
                        }
                    }
                })
                .or_insert_with(|| HistoryEntry {
                    url: row.url.0.clone(),
                    title: row.title.0.clone(),
                    last_visit: row.last,
                    visit_count: row.count,
                });
        }
        History::from_imported_entries(rows.into_values().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_import_preserves_complete_corpus_retry_receipts_and_forgotten_rows() {
        use ui::tilo_resource::Replica;
        let batch = crate::continuity::ImportBatch {
            source: [3; 32],
            bookmarks: vec![],
            history: (0..=MAX_HISTORY_ENTRIES)
                .map(|i| crate::continuity::HistoryImport {
                    url: format!("https://import{i}.test/"),
                    title: format!("Page {i}"),
                    last_visit: i as u64 + 1,
                    visit_count: 7,
                })
                .collect(),
        };
        let bundle = crate::continuity::ImportBundle::from_batch(batch).unwrap();
        let bundle = bundle.history_bundle();
        let encoded = serde_json::to_string(&bundle).unwrap();
        let mut replica = Replica::<BrowsingHistory>::new();
        replica
            .apply(BrowsingHistory::IMPORT_HISTORY_BUNDLE.with(encoded.clone()))
            .unwrap();
        assert_eq!(
            History::of(&replica.state().unwrap()).len(),
            MAX_HISTORY_ENTRIES + 1
        );
        replica
            .apply(BrowsingHistory::IMPORT_HISTORY_BUNDLE.with(encoded.clone()))
            .unwrap();
        assert!(
            replica
                .state()
                .unwrap()
                .entries()
                .iter()
                .all(|row| row.visit_count == 7)
        );
        replica
            .apply(BrowsingHistory::FORGET.with("https://import0.test/".to_owned()))
            .unwrap();
        replica
            .apply(BrowsingHistory::IMPORT_HISTORY_BUNDLE.with(encoded))
            .unwrap();
        assert!(
            !replica
                .state()
                .unwrap()
                .entries()
                .iter()
                .any(|row| row.url == "https://import0.test/")
        );
        let mut changed = bundle.clone();
        changed.chunks[0].history[0].title = "Changed source".into();
        let before = replica.state().unwrap();
        assert!(
            replica
                .apply(
                    BrowsingHistory::IMPORT_HISTORY_BUNDLE
                        .with(serde_json::to_string(&changed).unwrap())
                )
                .is_err()
        );
        assert_eq!(before, replica.state().unwrap());
        replica
            .apply(BrowsingHistory::CLEAR_HISTORY.with(()))
            .unwrap();
        replica
            .apply(
                BrowsingHistory::IMPORT_HISTORY_BUNDLE
                    .with(serde_json::to_string(&bundle).unwrap()),
            )
            .unwrap();
        assert!(replica.state().unwrap().is_empty());
    }

    fn entry(url: &str, title: &str, last_visit: u64, visit_count: u32) -> HistoryEntry {
        HistoryEntry {
            url: url.to_string(),
            title: title.to_string(),
            last_visit,
            visit_count,
        }
    }

    /// Recording visits.
    #[test]
    fn record_dedups_keeps_titles_and_evicts_at_the_bound() {
        {
            let mut h = History::new();
            h.record_at("https://a.test", "First", 100);
            h.record_at("https://a.test", "Second", 200);
            assert_eq!(h.len(), 1, "same URL collapses to one entry");
            let e = &h.entries()[0];
            assert_eq!(e.visit_count, 2, "revisit bumps the count");
            assert_eq!(e.last_visit, 200, "revisit refreshes the timestamp");
            assert_eq!(e.title, "Second", "revisit refreshes the title");
        }

        {
            let mut h = History::new();
            h.record_at("https://a.test", "Real Title", 100);
            h.record_at("https://a.test", "   ", 200);
            assert_eq!(
                h.entries()[0].title,
                "Real Title",
                "a blank title must not overwrite a good one"
            );
        }

        {
            let mut h = History::new();
            h.record_at("   ", "nope", 100);
            assert!(h.is_empty(), "a blank URL is not recorded");
        }

        {
            let mut h = History::new();
            h.record_at("https://a.test", "A", 100);
            h.record_at("https://b.test", "B", 100);
            assert_eq!(h.len(), 2);
        }

        {
            let mut h = History::new();
            for index in 0..=MAX_HISTORY_ENTRIES {
                h.record_at(
                    &format!("https://{index}.example.test"),
                    "Page",
                    u64::try_from(index).unwrap_or(u64::MAX),
                );
            }
            assert_eq!(h.len(), MAX_HISTORY_ENTRIES);
            assert!(
                !h.entries()
                    .iter()
                    .any(|entry| entry.url == "https://0.example.test"),
                "the least recent row is evicted first"
            );
        }
    }

    /// Matching: substrings and typos match, others do not, prefixes score higher.
    #[test]
    fn matching_is_case_insensitive_and_tolerates_typos() {
        let matches = |needle: &str, row: HistoryEntry| {
            let mut history = History::new();
            history.entries.push(row);
            !history.suggest_at(needle, 1, 1_000).is_empty()
        };
        let github = || entry("https://github.com", "Code hosting", 1_000, 1);
        assert!(matches("githbu", github()), "a typo in the URL matches");
        assert!(matches("HOSTING", github()), "a title substring matches");
        assert!(!matches("zzz", entry("https://example.com", "", 1, 1)));
        assert!(!matches("acb", entry("abc", "", 1, 1)));

        let mut history = History::new();
        history.entries.push(entry("barcode", "", 1, 1));
        history.entries.push(entry("codeberg", "", 1, 1));
        let urls: Vec<_> = history
            .suggest_at("code", 2, 1)
            .into_iter()
            .map(|row| row.url)
            .collect();
        assert_eq!(urls, ["codeberg", "barcode"], "a prefix beats an infix");
        assert!(
            !matches("git", entry("legit", "", 1, 1)),
            "a match inside a word needs four letters"
        );
    }

    /// Suggestion filtering, ranking signals and tie-breaks.
    #[test]
    fn suggestions_rank_by_match_recency_frequency_and_limit() {
        struct Case {
            name: &'static str,
            rows: &'static [(&'static str, &'static str, u64, u32)],
            query: &'static str,
            now: u64,
            len: Option<usize>,
            first: Option<&'static str>,
        }
        let cases = [
            // `suggest` folds case for the whole pipeline, so an uppercase
            // query still matches a mixed-case title/URL.
            Case {
                name: "'GIT' matches 'GitHub' case-insensitively",
                rows: &[("https://GitHub.com", "GitHub", 1_000, 1)],
                query: "GIT",
                now: 2_000,
                len: Some(1),
                first: None,
            },
            // Equal recency + frequency isolates the match-quality signal;
            // both hold 'code'.
            Case {
                name: "the prefix match ranks first",
                rows: &[
                    ("https://barcode.example", "Barcode", 1_000, 1),
                    ("https://codeberg.org", "Codeberg", 1_000, 1),
                ],
                query: "code",
                now: 2_000,
                len: Some(2),
                first: Some("https://codeberg.org"),
            },
            Case {
                name: "only the entry matching 'git' survives",
                rows: &[
                    ("https://github.com", "GitHub", 1_000, 1),
                    ("https://example.org", "Example", 1_000, 1),
                ],
                query: "git",
                now: 2_000,
                len: Some(1),
                first: Some("https://github.com"),
            },
            Case {
                name: "with no query and equal visits, the most-recent page leads",
                rows: &[
                    ("https://old.test", "Old", 100, 1),
                    ("https://recent.test", "Recent", 5_000, 1),
                ],
                query: "",
                now: 5_000,
                len: None,
                first: Some("https://recent.test"),
            },
            Case {
                name: "equal recency → the more-visited page leads",
                rows: &[
                    ("https://rare.test", "Rare", 1_000, 1),
                    ("https://popular.test", "Popular", 1_000, 25),
                ],
                query: "",
                now: 1_000,
                len: None,
                first: Some("https://popular.test"),
            },
            // Same URL shape so 'test' matches identically; recency decides.
            Case {
                name: "equal match quality → the more-recent page leads",
                rows: &[
                    ("https://a-test.com", "test", 100, 1),
                    ("https://b-test.com", "test", 9_000, 1),
                ],
                query: "test",
                now: 9_000,
                len: None,
                first: Some("https://b-test.com"),
            },
        ];
        for case in cases {
            let mut h = History::new();
            for &(url, title, last_visit, visit_count) in case.rows {
                h.entries.push(entry(url, title, last_visit, visit_count));
            }
            let out = h.suggest_at(case.query, 10, case.now);
            if let Some(len) = case.len {
                assert_eq!(out.len(), len, "{}", case.name);
            }
            if let Some(first) = case.first {
                assert_eq!(out[0].url, first, "{}", case.name);
            }
        }

        let mut h = History::new();
        for i in 0..10 {
            h.record_at(&format!("https://site{i}.test"), "Site", 1_000 + i as u64);
        }
        let out = h.suggest_at("site", 3, 5_000);
        assert_eq!(out.len(), 3, "the result is capped at the limit");
    }

    /// The person's record keeps one row per page, merges devices by page
    /// and refuses what a visit cannot name.
    #[test]
    fn browsing_history_record_counts_visits_and_merges_devices() {
        use ui::tilo_resource::Replica;
        let visit = |url: &str, title: &str, at: u64| {
            BrowsingHistory::VISIT.with(browsing_history::Visit {
                url: url.into(),
                title: title.into(),
                at,
            })
        };
        let entries = |record: &Replica<BrowsingHistory>| {
            History::of(&record.state().unwrap()).entries().to_vec()
        };
        let mut laptop = Replica::<BrowsingHistory>::new();
        laptop.apply(visit("https://a.test", "Alpha", 111)).unwrap();
        laptop.apply(visit("https://b.test", "Beta", 222)).unwrap();
        laptop.apply(visit("https://a.test", "", 333)).unwrap();
        let history = entries(&laptop);
        assert_eq!(history.len(), 2);
        let alpha = history
            .iter()
            .find(|entry| entry.url == "https://a.test")
            .unwrap();
        assert_eq!((alpha.visit_count, alpha.last_visit), (2, 333));
        assert_eq!(alpha.title, "Alpha", "a blank title keeps the known one");

        let long_title = "t".repeat(MAX_HISTORY_TITLE_BYTES + 64);
        laptop
            .apply(visit("https://kept.test", &long_title, 1))
            .unwrap();
        let too_long_url = format!("https://example.test/{}", "x".repeat(MAX_HISTORY_URL_BYTES));
        assert!(laptop.apply(visit(&too_long_url, "ignored", 1)).is_err());
        assert_eq!(
            entries(&laptop)
                .iter()
                .find(|entry| entry.url == "https://kept.test")
                .unwrap()
                .title
                .len(),
            MAX_HISTORY_TITLE_BYTES
        );

        let mut phone = laptop.fork();
        phone.apply(visit("https://c.test", "Gamma", 444)).unwrap();
        phone.apply(visit("https://a.test", "", 400)).unwrap();
        laptop.apply(visit("https://a.test", "", 350)).unwrap();
        laptop
            .apply(BrowsingHistory::FORGET.with("https://b.test".to_owned()))
            .unwrap();
        laptop.sync_with(&mut phone).unwrap();
        assert_eq!(laptop.state().unwrap(), phone.state().unwrap());
        let merged = entries(&laptop);
        let urls: Vec<&str> = merged.iter().map(|entry| entry.url.as_str()).collect();
        assert!(urls.contains(&"https://c.test"));
        assert!(!urls.contains(&"https://b.test"));
        let alpha = merged
            .iter()
            .find(|entry| entry.url == "https://a.test")
            .unwrap();
        assert_eq!(alpha.visit_count, 4, "visits on two devices add up");

        laptop
            .apply(BrowsingHistory::CLEAR_HISTORY.with(()))
            .unwrap();
        assert!(entries(&laptop).is_empty());
    }
}
