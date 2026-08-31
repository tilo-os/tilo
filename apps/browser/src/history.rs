//! Persistent browsing history + fuzzy address-bar ranking.
//!
//! The store is a flat list of [`HistoryEntry`] rows keyed by URL. Every
//! navigation calls [`History::record`], which de-duplicates by URL
//! (bumping the visit count and refreshing the title + timestamp) so the
//! list stays one-row-per-page. It persists as a JSON array under
//! `dirs::data_dir()/qos-browser/history.json`.
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
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    entries: Vec<HistoryEntry>,
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
        }
    }

    /// Drop all history (the settings "clear history" action).
    pub fn clear(&mut self) {
        self.entries.clear();
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

    /// Record a visit to `url` with `title`, stamped at the current wall
    /// clock. De-duplicates by URL — see [`History::record_at`].
    pub fn record(&mut self, url: &str, title: &str) {
        self.record_at(url, title, now_secs());
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
        if url.is_empty() {
            return;
        }
        if let Some(existing) = self.entries.iter_mut().find(|e| e.url == url) {
            existing.visit_count = existing.visit_count.saturating_add(1);
            existing.last_visit = now;
            if !title.trim().is_empty() {
                existing.title = title.to_string();
            }
        } else {
            self.entries.push(HistoryEntry {
                url: url.to_string(),
                title: title.to_string(),
                last_visit: now,
                visit_count: 1,
            });
        }
    }

    /// Fuzzy-ranked address-bar suggestions for `query`, best first,
    /// capped at `limit`. Stamped at the current wall clock — see
    /// [`History::suggest_at`] for the deterministic core.
    pub fn suggest(&self, query: &str, limit: usize) -> Vec<HistoryEntry> {
        self.suggest_at(query, limit, now_secs())
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
        let needle = query.trim().to_lowercase();
        let mut scored: Vec<(f64, &HistoryEntry)> = Vec::new();

        for entry in &self.entries {
            let match_score = if needle.is_empty() {
                // No text signal — rank purely on recency + frequency.
                0.0
            } else {
                let title_score = fuzzy_score(&entry.title.to_lowercase(), &needle);
                let url_score = fuzzy_score(&entry.url.to_lowercase(), &needle);
                match best(title_score, url_score) {
                    Some(s) => s,
                    None => continue, // matches neither field — drop it
                }
            };

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
        match std::fs::read_to_string(&path) {
            Ok(text) => History::from_json_str(&text),
            Err(_) => History::new(),
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
        if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(text)
        {
            for item in items {
                let url = item
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if url.is_empty() {
                    continue;
                }
                let title = item
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let last_visit = item
                    .get("last_visit")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let visit_count = item
                    .get("visit_count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32;
                entries.push(HistoryEntry {
                    url,
                    title,
                    last_visit,
                    visit_count,
                });
            }
        }
        History { entries }
    }
}

/// Absolute path to the history file:
/// `dirs::data_dir()/qos-browser/history.json`. `None` when the platform
/// exposes no data directory.
pub fn history_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("qos-browser").join("history.json"))
}

/// Current Unix-epoch time in seconds (0 if the clock is before the
/// epoch, which cannot happen on a sane system).
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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

/// The larger of two optional scores (`None` = no match).
fn best(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// Case-insensitive fuzzy subsequence score of `needle` within
/// `haystack` (both expected already lowercased), or `None` when
/// `needle` is not a subsequence of `haystack`.
///
/// The score lands in `[0, 1]` and rewards *contiguity* and a *strong
/// start*:
///  * a base credit for matching at all,
///  * a contiguity bonus proportional to how many matched characters are
///    adjacent (a full substring match maxes this out),
///  * a start bonus when the first matched character begins the string
///    (a prefix) or follows a word boundary (`.`, `/`, `-`, `_`, space,
///    `:`) — so `git` scores higher against `github.com` than against a
///    string where the `g` sits mid-word.
///
/// An empty needle scores `1.0` (everything "matches" the empty query);
/// callers that want the recency/frequency-only default filter the empty
/// query out before calling this.
fn fuzzy_score(haystack: &str, needle: &str) -> Option<f64> {
    // Blend weight: matching at all is worth this base, contiguity and start
    // each add up to the remainder, so a contiguous prefix ≈ 1.0 and a
    // scattered mid-word match ≈ the base.
    const BASE: f64 = 0.3;

    if needle.is_empty() {
        return Some(1.0);
    }
    let hay: Vec<char> = haystack.chars().collect();
    let need: Vec<char> = needle.chars().collect();

    // Greedy left-to-right subsequence match, recording matched indices.
    let mut matched: Vec<usize> = Vec::with_capacity(need.len());
    let mut hi = 0usize;
    for &nc in &need {
        let mut found = None;
        while hi < hay.len() {
            if hay.get(hi) == Some(&nc) {
                found = Some(hi);
                hi += 1;
                break;
            }
            hi += 1;
        }
        {
            let idx = found?;
            matched.push(idx);
        }
    }

    // Contiguity: fraction of the matched characters that are adjacent to
    // their predecessor. 1.0 for a pure substring, 0.0 for all-gaps.
    let contiguity = if need.len() > 1 {
        let adjacent = matched
            .iter()
            .zip(matched.iter().skip(1))
            .filter(|pair| *pair.1 == *pair.0 + 1)
            .count() as f64;
        adjacent / (need.len() - 1) as f64
    } else {
        // A single matched char is trivially "contiguous".
        1.0
    };

    // Start strength: prefix (index 0) beats a word-boundary start, which
    // beats a mid-word start.
    let Some(&first) = matched.first() else {
        return None;
    };
    let start_bonus = if first == 0 {
        1.0
    } else if hay.get(first - 1).is_some_and(|&c| is_boundary(c)) {
        0.6
    } else {
        0.0
    };

    let score = BASE + 0.45 * contiguity + 0.25 * start_bonus;
    Some(score.min(1.0))
}

/// Whether `c` is a word-boundary separator commonly found in URLs and
/// titles, so a match starting right after it reads as a "word start".
fn is_boundary(c: char) -> bool {
    matches!(c, '.' | '/' | '-' | '_' | ' ' | ':' | '?' | '&' | '=' | '#')
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

    #[test]
    fn record_dedups_by_url_and_bumps_count() {
        let mut h = History::new();
        h.record_at("https://a.test", "First", 100);
        h.record_at("https://a.test", "Second", 200);
        assert_eq!(h.len(), 1, "same URL collapses to one entry");
        let e = &h.entries()[0];
        assert_eq!(e.visit_count, 2, "revisit bumps the count");
        assert_eq!(e.last_visit, 200, "revisit refreshes the timestamp");
        assert_eq!(e.title, "Second", "revisit refreshes the title");
    }

    #[test]
    fn record_keeps_old_title_when_new_is_blank() {
        let mut h = History::new();
        h.record_at("https://a.test", "Real Title", 100);
        h.record_at("https://a.test", "   ", 200);
        assert_eq!(
            h.entries()[0].title,
            "Real Title",
            "a blank title must not overwrite a good one"
        );
    }

    #[test]
    fn record_ignores_blank_url() {
        let mut h = History::new();
        h.record_at("   ", "nope", 100);
        assert!(h.is_empty(), "a blank URL is not recorded");
    }

    #[test]
    fn record_keeps_distinct_urls_separate() {
        let mut h = History::new();
        h.record_at("https://a.test", "A", 100);
        h.record_at("https://b.test", "B", 100);
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn fuzzy_matches_lowercased_subsequence() {
        // `fuzzy_score` assumes its inputs are already lowercased (the
        // caller folds case); a subsequence then matches.
        let s = fuzzy_score("github.com", "git");
        assert!(s.is_some(), "'git' is a subsequence of 'github.com'");
    }

    #[test]
    fn suggest_is_case_insensitive() {
        // `suggest` folds case for the whole pipeline, so an uppercase
        // query still matches a mixed-case title/URL.
        let mut h = History::new();
        h.entries
            .push(entry("https://GitHub.com", "GitHub", 1_000, 1));
        let out = h.suggest_at("GIT", 10, 2_000);
        assert_eq!(out.len(), 1, "'GIT' matches 'GitHub' case-insensitively");
    }

    #[test]
    fn fuzzy_rejects_non_subsequence() {
        assert!(
            fuzzy_score("example.com", "zzz").is_none(),
            "characters absent from the haystack do not match"
        );
        assert!(
            fuzzy_score("abc", "acb").is_none(),
            "out-of-order characters are not a subsequence"
        );
    }

    #[test]
    fn fuzzy_prefix_scores_above_scattered() {
        // 'git' is a contiguous prefix of the first and scattered in the
        // second, so the prefix must score strictly higher.
        let prefix = fuzzy_score("github", "git").unwrap();
        let scattered = fuzzy_score("goldieturtle", "git").unwrap(); // g..i..t
        assert!(
            prefix > scattered,
            "contiguous prefix ({prefix}) must beat a scattered match ({scattered})"
        );
    }

    #[test]
    fn suggest_ranks_better_text_match_first() {
        // Equal recency + frequency isolates the match-quality signal.
        let mut h = History::new();
        h.entries
            .push(entry("https://github.com", "GitHub", 1_000, 1));
        h.entries
            .push(entry("https://goldieturtle.example", "Goldie", 1_000, 1));
        let out = h.suggest_at("git", 10, 2_000);
        assert_eq!(out.len(), 2, "both are subsequence matches for 'git'");
        assert_eq!(
            out[0].url, "https://github.com",
            "the contiguous prefix match ranks first"
        );
    }

    #[test]
    fn suggest_filters_out_non_matches() {
        let mut h = History::new();
        h.entries
            .push(entry("https://github.com", "GitHub", 1_000, 1));
        h.entries
            .push(entry("https://example.org", "Example", 1_000, 1));
        let out = h.suggest_at("git", 10, 2_000);
        assert_eq!(out.len(), 1, "only the entry matching 'git' survives");
        assert_eq!(out[0].url, "https://github.com");
    }

    #[test]
    fn empty_query_ranks_recent_first() {
        // Equal visit counts isolate the recency signal.
        let mut h = History::new();
        h.entries.push(entry("https://old.test", "Old", 100, 1));
        h.entries
            .push(entry("https://recent.test", "Recent", 5_000, 1));
        let out = h.suggest_at("", 10, 5_000);
        assert_eq!(
            out[0].url, "https://recent.test",
            "with no query and equal visits, the most-recent page leads"
        );
    }

    #[test]
    fn empty_query_breaks_recency_tie_by_frequency() {
        let mut h = History::new();
        h.entries.push(entry("https://rare.test", "Rare", 1_000, 1));
        h.entries
            .push(entry("https://popular.test", "Popular", 1_000, 25));
        let out = h.suggest_at("", 10, 1_000);
        assert_eq!(
            out[0].url, "https://popular.test",
            "equal recency → the more-visited page leads"
        );
    }

    #[test]
    fn recency_breaks_match_quality_tie() {
        // Same URL shape so 'test' matches identically; recency decides.
        let mut h = History::new();
        h.entries.push(entry("https://a-test.com", "test", 100, 1));
        h.entries
            .push(entry("https://b-test.com", "test", 9_000, 1));
        let out = h.suggest_at("test", 10, 9_000);
        assert_eq!(
            out[0].url, "https://b-test.com",
            "equal match quality → the more-recent page leads"
        );
    }

    #[test]
    fn suggest_respects_limit() {
        let mut h = History::new();
        for i in 0..10 {
            h.record_at(&format!("https://site{i}.test"), "Site", 1_000 + i as u64);
        }
        let out = h.suggest_at("site", 3, 5_000);
        assert_eq!(out.len(), 3, "the result is capped at the limit");
    }

    #[test]
    fn json_round_trips() {
        let mut h = History::new();
        h.record_at("https://a.test", "Alpha", 111);
        h.record_at("https://b.test", "Beta", 222);
        h.record_at("https://a.test", "Alpha", 333); // count=2
        let text = h.to_json_string();
        let back = History::from_json_str(&text);
        assert_eq!(back, h, "history survives a JSON round-trip intact");
    }

    #[test]
    fn from_json_skips_rows_without_url_and_tolerates_garbage() {
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

    #[test]
    fn from_json_garbage_is_empty() {
        assert!(History::from_json_str("not json at all").is_empty());
        assert!(History::from_json_str("{\"not\":\"an array\"}").is_empty());
    }

    #[test]
    fn history_path_is_under_qos_browser() {
        // Only assert the tail so the test is host-agnostic.
        if let Some(p) = history_path() {
            assert!(p.ends_with("qos-browser/history.json"), "path was {p:?}");
        }
    }
}
