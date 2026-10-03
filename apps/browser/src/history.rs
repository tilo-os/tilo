//! Persistent browsing history + fuzzy address-bar ranking.
//!
//! The store is a flat list of [`HistoryEntry`] rows keyed by URL. Every
//! navigation calls [`History::record`], which de-duplicates by URL
//! (bumping the visit count and refreshing the title + timestamp) so the
//! list stays one-row-per-page. It persists as a JSON array under
//! `tilo_paths::data_dir()/tilo-browser/history.json`.
//!
//! [`History::suggest`] powers the address-bar overlay. It fuzzy-matches
//! the query as a case-insensitive *subsequence* of each entry's title
//! and URL, then ranks survivors by a blend of three signals:
//!
//! 1. **match quality** — a contiguous run (or a prefix / word-boundary
//!    start) scores higher than the same characters scattered with gaps,
//!    so typing `git` surfaces `github.com` above `go-i-t.example`;
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
//!
//! Persistence uses `serde_json::Value` directly rather than a derived
//! `Serialize`/`Deserialize`: `serde`'s derive macro is not a dependency
//! of this crate, and the hand-rolled shape is trivial and forgiving of
//! partial rows.

use std::path::PathBuf;
use std::sync::Arc;
use tilo_clock::unix_now;
use ui::search::{FieldWeight, ListIndex, MAX_FIELD_RANK};

/// Keep history useful without letting a long-lived profile turn every
/// address-bar query, resource snapshot, or persisted JSON write into an
/// unbounded operation.
pub const MAX_HISTORY_ENTRIES: usize = 10_000;
/// A profile is user-controlled input.  Refuse a corrupt or hostile history
/// file before handing it to JSON parsing, which otherwise has to allocate a
/// complete value tree first.
pub const MAX_HISTORY_FILE_BYTES: usize = 8 * 1024 * 1024;
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
const W_MATCH: f64 = 1.0;
const W_RECENCY: f64 = 0.3;
const W_FREQ: f64 = 0.2;

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

    /// Record a visit to `url` with `title`, stamped at the current wall
    /// clock. De-duplicates by URL — see [`History::record_at`].
    pub fn record(&mut self, url: &str, title: &str) {
        self.record_at(url, title, unix_now());
    }

    /// Deterministic core of [`History::record`]: record a visit stamped
    /// at `now` (Unix seconds). Split out so ranking/recency behaviour is
    /// unit-testable without touching the wall clock.
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

            let score = W_MATCH * match_score
                + W_RECENCY * recency_score(entry.last_visit, now)
                + W_FREQ * frequency_score(entry.visit_count);
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

    // --- Persistence ----------------------------------------------------

    /// Load history from the on-disk JSON file, or an empty history when
    /// the file is missing or unreadable/corrupt (never fails — a broken
    /// history file must not stop the browser from starting).
    pub fn load() -> History {
        let Some(path) = history_path() else {
            return History::new();
        };
        match std::fs::metadata(&path) {
            Ok(metadata) if metadata.len() > MAX_HISTORY_FILE_BYTES as u64 => History::new(),
            Err(_) => History::new(),
            Ok(_) => match std::fs::read_to_string(&path) {
                Ok(text) => History::from_json_str(&text),
                Err(_) => History::new(),
            },
        }
    }

    /// Persist history to the JSON file, creating the parent directory if
    /// needed. Returns an error only on genuine IO failure.
    ///
    /// # Errors
    ///
    /// Returns an error if the platform exposes no data directory, if the
    /// parent directory can't be created, or if the write itself fails.
    pub fn save(&self) -> std::io::Result<()> {
        let path = history_path().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no data directory available for browser history",
            )
        })?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, self.to_json_string())
    }

    /// Serialise the history to a pretty JSON array string. Pure — no IO,
    /// so the round-trip is unit-testable.
    pub fn to_json_string(&self) -> String {
        let arr: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "url": e.url,
                    "title": e.title,
                    "last_visit": e.last_visit,
                    "visit_count": e.visit_count,
                })
            })
            .collect();
        serde_json::to_string_pretty(&serde_json::Value::Array(arr))
            .unwrap_or_else(|_| "[]".to_string())
    }

    /// Parse a history from a JSON array string. Unknown/partial rows are
    /// tolerated (missing fields default; rows without a URL are skipped)
    /// and any parse failure yields an empty history.
    pub fn from_json_str(text: &str) -> History {
        let mut entries = Vec::new();
        if text.len() > MAX_HISTORY_FILE_BYTES {
            return History::new();
        }
        if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(text)
        {
            for item in items {
                let url = item
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if url.is_empty() || url.len() > MAX_HISTORY_URL_BYTES {
                    continue;
                }
                let title = item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map_or_else(String::new, |title| {
                        bounded_text(title, MAX_HISTORY_TITLE_BYTES)
                    });
                let last_visit = item
                    .get("last_visit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let visit_count = item
                    .get("visit_count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32;
                entries.push(HistoryEntry {
                    url: url.to_string(),
                    title,
                    last_visit,
                    visit_count,
                });
            }
        }
        let mut history = History {
            entries,
            ..History::new()
        };
        history.trim_to_limit();
        history
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
fn bounded_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    value.get(..end).unwrap_or_default().to_string()
}

/// Absolute path to the history file:
/// `tilo_paths::data_dir()/tilo-browser/history.json`. `None` when the platform
/// exposes no data directory. The legacy directory remains the canonical
/// default until storage migration is explicit and crash-safe.
pub fn history_path() -> Option<PathBuf> {
    crate::storage::data_dir().map(|d| d.join("history.json"))
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // Phase `record_dedups_by_url_and_bumps_count`.
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

        // Phase `record_keeps_old_title_when_new_is_blank`.
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

        // Phase `record_ignores_blank_url`.
        {
            let mut h = History::new();
            h.record_at("   ", "nope", 100);
            assert!(h.is_empty(), "a blank URL is not recorded");
        }

        // Phase `record_keeps_distinct_urls_separate`.
        {
            let mut h = History::new();
            h.record_at("https://a.test", "A", 100);
            h.record_at("https://b.test", "B", 100);
            assert_eq!(h.len(), 2);
        }

        // Phase `history_evicts_oldest_rows_at_the_resource_bound`.
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
        history.entries.push(entry("legit", "", 1, 1));
        history.entries.push(entry("github", "", 1, 1));
        let urls: Vec<_> = history
            .suggest_at("git", 2, 1)
            .into_iter()
            .map(|row| row.url)
            .collect();
        assert_eq!(urls, ["github", "legit"], "a prefix beats an infix");
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
            // both hold 'git'.
            Case {
                name: "the prefix match ranks first",
                rows: &[
                    ("https://legit.example", "Legit", 1_000, 1),
                    ("https://github.com", "GitHub", 1_000, 1),
                ],
                query: "git",
                now: 2_000,
                len: Some(2),
                first: Some("https://github.com"),
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
            // Equal visit counts isolate the recency signal.
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

    /// JSON persistence, garbage tolerance, bounds and data root.
    #[test]
    fn history_persistence_round_trips_and_bounds_hostile_rows() {
        // Phase `json_round_trips`.
        {
            let mut h = History::new();
            h.record_at("https://a.test", "Alpha", 111);
            h.record_at("https://b.test", "Beta", 222);
            h.record_at("https://a.test", "Alpha", 333); // count=2
            let text = h.to_json_string();
            let back = History::from_json_str(&text);
            assert_eq!(back, h, "history survives a JSON round-trip intact");
        }

        // Phase `from_json_skips_rows_without_url_and_tolerates_garbage`.
        {
            let json = r#"[
                {"url":"https://ok.test","title":"OK","last_visit":5,"visit_count":2},
                {"title":"no url"},
                {"url":"https://partial.test"}
            ]"#;
            let h = History::from_json_str(json);
            assert_eq!(h.len(), 2, "the URL-less row is skipped, partials default");
            let partial = h
                .entries()
                .iter()
                .find(|e| e.url == "https://partial.test")
                .unwrap();
            assert_eq!(
                partial.visit_count, 0,
                "missing numeric fields default to 0"
            );
            assert_eq!(partial.title, "", "missing title defaults to empty");
        }

        // Phase `from_json_garbage_is_empty`.
        {
            assert!(History::from_json_str("not json at all").is_empty());
            assert!(History::from_json_str("{\"not\":\"an array\"}").is_empty());
        }

        // Phase `hostile_history_rows_are_bounded_before_search_can_materialize_them`.
        {
            let too_long_url =
                format!("https://example.test/{}", "x".repeat(MAX_HISTORY_URL_BYTES));
            let json = serde_json::json!([
                { "url": too_long_url, "title": "ignored" },
                {
                    "url": "https://kept.example.test",
                    "title": "t".repeat(MAX_HISTORY_TITLE_BYTES + 64),
                }
            ]);
            let history = History::from_json_str(&json.to_string());
            assert_eq!(history.len(), 1);
            assert_eq!(history.entries()[0].title.len(), MAX_HISTORY_TITLE_BYTES);
            assert!(History::from_json_str(&" ".repeat(MAX_HISTORY_FILE_BYTES + 1)).is_empty());
        }

        // Phase `history_path_preserves_the_existing_browser_data_root`.
        {
            // Only assert the tail so the test is host-agnostic.
            if let Some(p) = history_path() {
                assert!(p.ends_with("tilo-browser/history.json"), "path was {p:?}");
            }
        }
    }
}
