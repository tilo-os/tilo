//! Persistent browsing history + fuzzy address-bar ranking.
//!
//! The store is a flat list of [`HistoryEntry`] rows keyed by URL. Every
//! navigation calls [`History::record`], which de-duplicates by URL
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

use std::sync::Arc;
use tilo_clock::unix_now;
use ui::search::{FieldWeight, ListIndex, MAX_FIELD_RANK};

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

/// The person's browsing history as a resource of their own room: each
/// visited page, keyed by URL, with its title, last visit and visit count.
pub struct BrowsingHistory;

/// The edits of [`BrowsingHistory`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HistoryAction {
    /// A page finished loading at `at` (Unix seconds).
    Visit { url: String, title: String, at: u64 },
    /// Forget one page.
    Forget { url: String },
    /// Forget every page.
    Clear,
}

/// The map of visits at the record's root, keyed by URL.
const VISITS: &str = "visits";
const VISIT_TITLE: &str = "title";
const VISIT_LAST: &str = "last";
const VISIT_COUNT: &str = "count";

fn visits(doc: &tilo_actions::Doc) -> Option<automerge::ObjId> {
    use automerge::ReadDoc;
    match doc.get(&automerge::ROOT, VISITS).ok().flatten() {
        Some((automerge::Value::Object(automerge::ObjType::Map), id)) => Some(id),
        _ => None,
    }
}

fn read_visit(
    doc: &tilo_actions::Doc,
    visits: &automerge::ObjId,
    url: &str,
) -> Option<HistoryEntry> {
    use automerge::ReadDoc;
    let Some((automerge::Value::Object(automerge::ObjType::Map), visit)) =
        doc.get(visits, url).ok().flatten()
    else {
        return None;
    };
    let scalar = |key: &str| {
        doc.get(&visit, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| value.into_scalar().ok())
    };
    Some(HistoryEntry {
        url: url.to_owned(),
        title: scalar(VISIT_TITLE)
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default(),
        last_visit: scalar(VISIT_LAST)
            .and_then(|value| value.as_u64())
            .unwrap_or(0),
        visit_count: scalar(VISIT_COUNT)
            .and_then(|value| value.as_u64())
            .map_or(0, |count| u32::try_from(count).unwrap_or(u32::MAX)),
    })
}

fn read_visits(doc: &tilo_actions::Doc) -> Vec<HistoryEntry> {
    use automerge::ReadDoc;
    let Some(visits) = visits(doc) else {
        return Vec::new();
    };
    doc.keys(&visits)
        .filter_map(|url| read_visit(doc, &visits, &url))
        .collect()
}

impl tilo_actions::Action for HistoryAction {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        use automerge::transaction::Transactable;
        match self {
            Self::Visit { url, title, at } => {
                let url = url.trim();
                if url.is_empty() || url.len() > MAX_HISTORY_URL_BYTES {
                    return Err(tilo_actions::ActionError::Refused(
                        "a visit names one page's URL".into(),
                    ));
                }
                let visits = match visits(doc) {
                    Some(visits) => visits,
                    None => doc.put_object(automerge::ROOT, VISITS, automerge::ObjType::Map)?,
                };
                let previous = read_visit(doc, &visits, url);
                let title = bounded_text(title, MAX_HISTORY_TITLE_BYTES);
                let visit = match &previous {
                    Some(_) => match automerge::ReadDoc::get(doc, &visits, url)? {
                        Some((_, visit)) => visit,
                        None => doc.put_object(&visits, url, automerge::ObjType::Map)?,
                    },
                    None => doc.put_object(&visits, url, automerge::ObjType::Map)?,
                };
                if previous.is_none() || !title.trim().is_empty() {
                    doc.put(&visit, VISIT_TITLE, title)?;
                }
                let last = previous.as_ref().map_or(0, |entry| entry.last_visit);
                doc.put(&visit, VISIT_LAST, (*at).max(last))?;
                let count = previous.as_ref().map_or(0, |entry| entry.visit_count);
                doc.put(&visit, VISIT_COUNT, u64::from(count.saturating_add(1)))?;
                // Keep the most useful pages; the rest fall off.
                let kept = History::from_entries(read_visits(doc));
                if kept.len() < automerge::ReadDoc::length(doc, &visits) {
                    let keep: std::collections::BTreeSet<&str> = kept
                        .entries()
                        .iter()
                        .map(|entry| entry.url.as_str())
                        .collect();
                    let drop: Vec<String> = automerge::ReadDoc::keys(doc, &visits)
                        .filter(|url| !keep.contains(url.as_str()))
                        .collect();
                    for url in drop {
                        doc.delete(&visits, url)?;
                    }
                }
            }
            Self::Forget { url } => {
                if let Some(visits) = visits(doc)
                    && automerge::ReadDoc::get(doc, &visits, url.as_str())?.is_some()
                {
                    doc.delete(&visits, url.as_str())?;
                }
            }
            Self::Clear => {
                if visits(doc).is_some() {
                    doc.delete(automerge::ROOT, VISITS)?;
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> tilo_actions::ExecutionLocus {
        tilo_actions::ExecutionLocus::ClientLocal
    }

    /// Recording a visit is the browser keeping its history, not an edit of
    /// the person's to take back; forgetting is theirs.
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        match self {
            Self::Visit { .. } => tilo_actions::ChangeKind::Housekeeping,
            Self::Forget { .. } | Self::Clear => tilo_actions::ChangeKind::Content,
        }
    }
}

impl ui::tilo_resource::Resource for BrowsingHistory {
    const KIND: &'static str = "browser.browsing-history";
    const LABEL: &'static str = "Browsing history";
    const ACTION_TYPE: &'static str = "browser.history.action";

    type State = History;
    type Action = HistoryAction;

    fn read(doc: &ui::tilo_resource::Doc) -> Result<History, ui::tilo_resource::ModelError> {
        Ok(History::from_entries(read_visits(doc)))
    }
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

    /// The person's record keeps one row per page, merges devices by page
    /// and refuses what a visit cannot name.
    #[test]
    fn browsing_history_record_counts_visits_and_merges_devices() {
        use ui::tilo_resource::Replica;
        let visit = |url: &str, title: &str, at: u64| HistoryAction::Visit {
            url: url.into(),
            title: title.into(),
            at,
        };
        let mut laptop = Replica::<BrowsingHistory>::new();
        laptop
            .apply_action(&visit("https://a.test", "Alpha", 111))
            .unwrap();
        laptop
            .apply_action(&visit("https://b.test", "Beta", 222))
            .unwrap();
        laptop
            .apply_action(&visit("https://a.test", "", 333))
            .unwrap();
        let history = laptop.state().unwrap();
        assert_eq!(history.len(), 2);
        let alpha = history
            .entries()
            .iter()
            .find(|entry| entry.url == "https://a.test")
            .unwrap();
        assert_eq!((alpha.visit_count, alpha.last_visit), (2, 333));
        assert_eq!(alpha.title, "Alpha", "a blank title keeps the known one");

        let long_title = "t".repeat(MAX_HISTORY_TITLE_BYTES + 64);
        laptop
            .apply_action(&visit("https://kept.test", &long_title, 1))
            .unwrap();
        let too_long_url = format!("https://example.test/{}", "x".repeat(MAX_HISTORY_URL_BYTES));
        assert!(
            laptop
                .apply_action(&visit(&too_long_url, "ignored", 1))
                .is_err()
        );
        let history = laptop.state().unwrap();
        assert_eq!(
            history
                .entries()
                .iter()
                .find(|entry| entry.url == "https://kept.test")
                .unwrap()
                .title
                .len(),
            MAX_HISTORY_TITLE_BYTES
        );

        let mut phone = Replica::<BrowsingHistory>::from_doc(laptop.doc().clone().fork());
        phone
            .apply_action(&visit("https://c.test", "Gamma", 444))
            .unwrap();
        laptop
            .apply_action(&HistoryAction::Forget {
                url: "https://b.test".into(),
            })
            .unwrap();
        tilo_automerge::ApplyIncoming::merge_incoming(laptop.doc_mut(), phone.doc_mut()).unwrap();
        let urls: Vec<String> = laptop
            .state()
            .unwrap()
            .entries()
            .iter()
            .map(|entry| entry.url.clone())
            .collect();
        assert!(urls.contains(&"https://c.test".to_string()));
        assert!(!urls.contains(&"https://b.test".to_string()));

        laptop.apply_action(&HistoryAction::Clear).unwrap();
        assert!(laptop.state().unwrap().is_empty());
    }
}
