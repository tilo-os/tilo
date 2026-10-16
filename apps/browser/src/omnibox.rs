//! The address bar's brain: what a typed string means, which visited pages
//! it recalls, which searches the engine offers, and the one ranked list of
//! rows that results.
//!
//! Everything here is pure and synchronous so it can run inside a frame:
//!
//! * [`HistoryIndex`] normalises the visited pages once per history change
//!   and answers each keystroke incrementally: a query that extends the last
//!   one only rescans that query's matches, never the whole history.
//! * Matches are ordered by [`Tier`] first (a URL or host prefix beats a
//!   word prefix beats a substring) and by *frecency* (visit count weighted
//!   by recency) within a tier, so `you` recalls `youtube.com` before a page
//!   that merely mentions "you" in its title however often it was opened.
//! * [`build_rows`] merges the typed intent, history and the engine's search
//!   suggestions into one deduplicated list of [`Row`]s.
//! * [`Debounce`] decides when a search-suggestion fetch may start and which
//!   result is still wanted, so typing never waits on the network.

use std::time::{Duration, Instant};

use crate::history::HistoryEntry;

/// Rows the dropdown shows at most.
pub const MAX_ROWS: usize = 8;
/// History rows a query may take before search suggestions fill the rest.
const MAX_HISTORY_ROWS: usize = 4;
/// Quiet time after the last keystroke before the engine is asked.
pub const DEBOUNCE: Duration = Duration::from_millis(140);
/// A frecency half-life: a visit loses half its pull every week.
const RECENCY_HALF_LIFE_SECS: f64 = 7.0 * 24.0 * 3600.0;
/// Recent visits count for more, but a page opened often long ago still
/// outranks a page seen once and never again.
const RECENCY_FLOOR: f64 = 0.15;

/// How a page matched the typed text; earlier is better.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// The address (without scheme or `www.`) starts with the text.
    Prefix,
    /// The host contains the text (`you` or `tube` in `m.youtube.com`).
    HostWord,
    /// A word of the title or address starts with the text.
    Word,
    /// The text occurs inside the title or address.
    Substring,
}

/// What a row is, which decides how it looks and what choosing it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// Open exactly what was typed as an address.
    Go,
    /// Search the configured engine for exactly what was typed.
    Search,
    /// A page visited before.
    History,
    /// A search the engine suggests.
    Suggestion,
}

/// One line of the dropdown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    /// The primary line: a page title, a query, or the address to open.
    pub title: String,
    /// The secondary line: a page's address, or nothing.
    pub detail: String,
    /// What choosing the row opens: an address for [`RowKind::Go`] and
    /// [`RowKind::History`], the query for the search kinds.
    pub url: String,
    /// How often the page was visited ([`RowKind::History`] only).
    pub visits: u32,
}

impl Row {
    /// A visited page as a row.
    pub fn history(entry: &HistoryEntry) -> Self {
        Self {
            kind: RowKind::History,
            title: if entry.title.trim().is_empty() {
                display_url(&entry.url)
            } else {
                entry.title.clone()
            },
            detail: display_url(&entry.url),
            url: entry.url.clone(),
            visits: entry.visit_count,
        }
    }

    fn search(kind: RowKind, query: &str) -> Self {
        Self {
            kind,
            title: query.to_owned(),
            detail: String::new(),
            url: query.to_owned(),
            visits: 0,
        }
    }
}

/// `https://www.example.com/a` as `example.com/a`: what a person reads.
pub fn display_url(url: &str) -> String {
    let url = url.trim();
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    rest.strip_prefix("www.")
        .unwrap_or(rest)
        .trim_end_matches('/')
        .to_owned()
}

/// The address without scheme and `www.`, lowercased: the text a typed
/// prefix is compared against.
fn normalize_url(url: &str) -> String {
    let lower = url.trim().to_lowercase();
    let rest = lower.split_once("://").map_or(lower.as_str(), |(_, r)| r);
    rest.strip_prefix("www.").unwrap_or(rest).to_owned()
}

fn host_of(key: &str) -> &str {
    key.split(['/', '?', '#']).next().unwrap_or(key)
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
}

/// How strongly a page pulls: visits weighted by how recently the last one
/// was. Deterministic in `now` so it is testable.
pub fn frecency(visits: u32, last_visit: u64, now: u64) -> f64 {
    let age = now.saturating_sub(last_visit) as f64;
    let recency = 0.5_f64
        .powf(age / RECENCY_HALF_LIFE_SECS)
        .max(RECENCY_FLOOR);
    (1.0 + f64::from(visits.max(1)).ln()) * recency
}

struct Indexed {
    /// Normalised address.
    key: String,
    title: String,
}

/// A ranked history match.
#[derive(Clone, Debug)]
pub struct Match {
    pub entry: HistoryEntry,
    pub tier: Tier,
    pub frecency: f64,
}

/// The visited pages, prepared for per-keystroke matching.
#[derive(Default)]
pub struct HistoryIndex {
    entries: Vec<HistoryEntry>,
    indexed: Vec<Indexed>,
    /// Query the candidate list was computed for.
    last_query: String,
    /// Positions that matched `last_query`; a longer query can only match
    /// among them.
    candidates: Vec<usize>,
    /// Entries the last [`HistoryIndex::search`] examined, for tests and
    /// frame-budget diagnostics.
    pub scanned: usize,
}

impl HistoryIndex {
    /// Index `entries` (replacing everything held). Call when the history
    /// changed, not per keystroke.
    pub fn rebuild(&mut self, entries: &[HistoryEntry]) {
        self.entries = entries.to_vec();
        self.indexed = entries
            .iter()
            .map(|e| Indexed {
                key: normalize_url(&e.url),
                title: e.title.to_lowercase(),
            })
            .collect();
        self.last_query.clear();
        self.candidates.clear();
    }

    /// The best `limit` matches for `query` at time `now`, best first.
    pub fn search(&mut self, query: &str, limit: usize, now: u64) -> Vec<Match> {
        let q = query.trim().to_lowercase();
        let q = q.strip_prefix("www.").unwrap_or(&q);
        let q = q.split_once("://").map_or(q, |(_, rest)| rest);
        if q.is_empty() {
            self.last_query.clear();
            self.candidates.clear();
            self.scanned = 0;
            return Vec::new();
        }
        let narrowed = !self.last_query.is_empty() && q.starts_with(self.last_query.as_str());
        let pool: Vec<usize> = if narrowed {
            std::mem::take(&mut self.candidates)
        } else {
            (0..self.indexed.len()).collect()
        };
        self.scanned = pool.len();
        let terms: Vec<&str> = q.split_whitespace().collect();
        let hits: Vec<(usize, Tier)> = pool
            .into_iter()
            .filter_map(|at| {
                self.indexed
                    .get(at)
                    .and_then(|i| tier_of(i, &terms))
                    .map(|t| (at, t))
            })
            .collect();
        q.clone_into(&mut self.last_query);
        self.candidates = hits.iter().map(|(at, _)| *at).collect();

        // Rank by index and copy out only the rows shown: on a short query
        // every page matches, and cloning and fully sorting all of them is
        // most of a keystroke's cost.
        let mut ranked: Vec<(usize, Tier, f64)> = hits
            .into_iter()
            .filter_map(|(at, tier)| {
                let entry = self.entries.get(at)?;
                Some((at, tier, frecency(entry.visit_count, entry.last_visit, now)))
            })
            .collect();
        let entries = &self.entries;
        let order = |a: &(usize, Tier, f64), b: &(usize, Tier, f64)| {
            let (url_a, url_b) = (
                entries.get(a.0).map_or("", |e| e.url.as_str()),
                entries.get(b.0).map_or("", |e| e.url.as_str()),
            );
            a.1.cmp(&b.1)
                .then(b.2.total_cmp(&a.2))
                .then(url_a.len().cmp(&url_b.len()))
                .then(url_a.cmp(url_b))
        };
        if let Some(last) = limit.checked_sub(1)
            && ranked.len() > limit
        {
            ranked.select_nth_unstable_by(last, order);
            ranked.truncate(limit);
        }
        ranked.sort_unstable_by(order);
        ranked.truncate(limit);
        ranked
            .into_iter()
            .filter_map(|(at, tier, frecency)| {
                Some(Match {
                    frecency,
                    entry: self.entries.get(at)?.clone(),
                    tier,
                })
            })
            .collect()
    }
}

fn tier_of(item: &Indexed, terms: &[&str]) -> Option<Tier> {
    match terms {
        [] => None,
        [one] => single_tier(item, one),
        many => {
            // Every term must match somewhere; the weakest term sets the tier
            // and a multi-word query is never a plain address prefix.
            let mut worst = Tier::Word;
            for term in many {
                let tier = single_tier(item, term)?;
                worst = worst.max(if tier == Tier::Substring {
                    Tier::Substring
                } else {
                    Tier::Word
                });
            }
            Some(worst)
        }
    }
}

fn single_tier(item: &Indexed, q: &str) -> Option<Tier> {
    if item.key.starts_with(q) {
        return Some(Tier::Prefix);
    }
    if host_of(&item.key).contains(q) {
        return Some(Tier::HostWord);
    }
    if words(&item.title).any(|w| w.starts_with(q)) || words(&item.key).any(|w| w.starts_with(q)) {
        return Some(Tier::Word);
    }
    if item.key.contains(q) || item.title.contains(q) {
        return Some(Tier::Substring);
    }
    None
}

/// Whether `text` is meant as an address rather than a search.
pub fn looks_like_address(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return false;
    }
    text.contains("://")
        || text.starts_with("localhost")
        || (text.starts_with('[') && text.contains(']'))
        || text.split(['/', '?', '#']).next().is_some_and(|host| {
            host.contains('.') && !host.starts_with('.') && !host.ends_with('.')
        })
}

/// The address a typed address row opens.
fn address_target(text: &str) -> String {
    let text = text.trim();
    if text.contains("://") {
        text.to_owned()
    } else {
        format!("https://{text}")
    }
}

/// The text to show with the untyped remainder selected, when the best
/// history match starts with what was typed: `you` becomes `youtube.com`
/// (the host, as a browser completes), `youtube.com/w` the full path.
pub fn inline_completion(typed: &str, best: Option<&Match>) -> Option<String> {
    let best = best?;
    if best.tier != Tier::Prefix
        || typed.is_empty()
        || typed.chars().any(char::is_whitespace)
        || typed.contains("://")
    {
        return None;
    }
    let key = display_url(&best.entry.url);
    let typed_lower = typed.to_lowercase();
    let typed_norm = typed_lower.strip_prefix("www.").unwrap_or(&typed_lower);
    let completed = if typed_norm.contains('/') {
        key.trim_end_matches('/').to_owned()
    } else {
        host_of(&key).to_owned()
    };
    // Matching folds case, but navigating must preserve a case-sensitive
    // path and query. Do not complete a differently cased typed path.
    let original_typed = if typed_lower.starts_with("www.") {
        typed.get(4..)?
    } else {
        typed
    };
    if let Some((_, path)) = original_typed.split_once('/') {
        let (_, target_path) = completed.split_once('/')?;
        if !target_path.starts_with(path) {
            return None;
        }
    }
    let prefix = completed.get(..original_typed.len())?;
    if prefix.to_lowercase() != typed_norm {
        return None;
    }
    let rest = completed.get(original_typed.len()..)?;
    if rest.is_empty() {
        return None;
    }
    Some(format!("{typed}{rest}"))
}

/// Merge what the bar knows into one ordered, deduplicated list.
///
/// * A typed address leads with a "go to" row; typed words lead with a
///   "search for" row, unless the best history match completes the text, in
///   which case the page leads.
/// * History comes next, then the engine's suggestions.
/// * Rows naming the same address or query appear once, and the page the
///   window already shows is not offered back.
pub fn build_rows(
    typed: &str,
    history: &[Match],
    remote: &[String],
    current_uri: &str,
    limit: usize,
) -> Vec<Row> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Vec::new();
    }
    let current = display_url(current_uri);
    let mut seen_urls: Vec<String> = vec![current];
    let mut seen_queries: Vec<String> = vec![typed.to_lowercase()];
    let mut rows: Vec<Row> = Vec::new();

    let mut go = None;
    if looks_like_address(typed) {
        let url = address_target(typed);
        seen_urls.push(display_url(&url));
        go = Some(Row {
            kind: RowKind::Go,
            title: display_url(&url),
            detail: url.clone(),
            url,
            visits: 0,
        });
    }
    let mut pages: Vec<Row> = Vec::new();
    for found in history {
        let key = display_url(&found.entry.url);
        if seen_urls.contains(&key) {
            continue;
        }
        seen_urls.push(key);
        pages.push(Row::history(&found.entry));
        if pages.len() == MAX_HISTORY_ROWS {
            break;
        }
    }
    let search = Row::search(RowKind::Search, typed);
    let completes = history.first().is_some_and(|m| m.tier == Tier::Prefix);
    match go {
        Some(go) => {
            rows.push(go);
            rows.extend(pages);
            rows.push(search);
        }
        None if completes => {
            rows.extend(pages);
            rows.push(search);
        }
        None => {
            rows.push(search);
            rows.extend(pages);
        }
    }
    for suggestion in remote {
        let suggestion = suggestion.trim();
        if suggestion.is_empty() || seen_queries.contains(&suggestion.to_lowercase()) {
            continue;
        }
        seen_queries.push(suggestion.to_lowercase());
        rows.push(Row::search(RowKind::Suggestion, suggestion));
    }
    rows.truncate(limit);
    rows
}

/// Put a "go to" row for the completed address first, replacing any row for
/// the same address: with an inline completion shown in the bar, Enter must
/// open what the bar says.
pub fn lead_with_address(rows: &mut Vec<Row>, address: &str) {
    let url = address_target(address);
    let shown = display_url(&url);
    rows.retain(|row| {
        !matches!(row.kind, RowKind::Go | RowKind::History) || display_url(&row.url) != shown
    });
    rows.insert(
        0,
        Row {
            kind: RowKind::Go,
            title: shown,
            detail: url.clone(),
            url,
            visits: 0,
        },
    );
    rows.truncate(MAX_ROWS);
}

/// The engine's answer to one query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    pub generation: u64,
    pub query: String,
    pub suggestions: Vec<String>,
}

/// Fetches the engine's suggestions off the UI thread. The UI never waits:
/// it asks with [`SuggestFetcher::fetch`] and later collects the newest
/// answer with [`SuggestFetcher::take`], discarding it if the generation is
/// no longer wanted ([`Debounce::is_current`]).
pub struct SuggestFetcher {
    answer: ui::LocalState<Option<Answer>>,
    running: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Default for SuggestFetcher {
    fn default() -> Self {
        Self {
            answer: ui::LocalState::new(None),
            running: std::sync::Arc::default(),
        }
    }
}

/// Requests in flight at once; a burst of typing never piles up sockets.
const MAX_IN_FLIGHT: usize = 2;
const FETCH_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_BODY: usize = 64 * 1024;

impl SuggestFetcher {
    /// Start fetching `endpoint` for `query`; the answer is tagged
    /// `generation`. Dropped when too many fetches are already running.
    pub fn fetch(&self, generation: u64, query: String, endpoint: String) {
        use std::sync::atomic::Ordering;
        if self.running.fetch_add(1, Ordering::AcqRel) >= MAX_IN_FLIGHT {
            self.running.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        let answer = self.answer.clone();
        let running = self.running.clone();
        let spawned = std::thread::Builder::new()
            .name("omnibox-suggest".into())
            .spawn(move || {
                let suggestions = fetch_suggestions(&endpoint).unwrap_or_default();
                answer.set(Some(Answer {
                    generation,
                    query,
                    suggestions,
                }));
                running.fetch_sub(1, Ordering::AcqRel);
            });
        if spawned.is_err() {
            self.running.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// The newest answer that arrived, if any.
    pub fn take(&self) -> Option<Answer> {
        let mut taken = None;
        self.answer.update(|slot| taken = slot.take());
        taken
    }

    /// Whether a fetch is still running.
    pub fn in_flight(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::Acquire) > 0
    }
}

fn fetch_suggestions(endpoint: &str) -> Option<Vec<String>> {
    let client = tilo_net::http::blocking_client()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    let response = client.get(endpoint).send().ok()?.error_for_status().ok()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY as u64)
    {
        return None;
    }
    let body = response.text().ok()?;
    (body.len() <= MAX_BODY).then(|| parse_suggestions(&body))
}

/// The suggest endpoint for a search engine template, when it is one we
/// know to speak `OpenSearch` suggestions. The typed text is only ever sent
/// to the engine the person chose to search with.
#[allow(
    clippy::literal_string_with_formatting_args,
    reason = "search-engine templates carry a literal `{query}` placeholder"
)]
pub fn suggest_endpoint(search_template: &str, query: &str) -> Option<String> {
    let url = url::Url::parse(&search_template.replace("{query}", "")).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.trim_start_matches("www.").to_owned();
    let q: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    let endpoint = match host.as_str() {
        "duckduckgo.com" => format!("https://duckduckgo.com/ac/?type=list&q={q}"),
        "google.com" => {
            format!("https://suggestqueries.google.com/complete/search?client=firefox&q={q}")
        }
        "bing.com" => format!("https://api.bing.com/osjson.aspx?query={q}"),
        "search.brave.com" => format!("https://search.brave.com/api/suggest?q={q}"),
        "ecosia.org" => format!("https://ac.ecosia.org/autocomplete?type=list&q={q}"),
        _ => return None,
    };
    Some(endpoint)
}

/// The suggestions of an `OpenSearch` suggest response: `["query", ["a", "b"]]`.
pub fn parse_suggestions(body: &str) -> Vec<String> {
    let Ok(serde_json::Value::Array(parts)) = serde_json::from_str::<serde_json::Value>(body)
    else {
        return Vec::new();
    };
    let Some(serde_json::Value::Array(items)) = parts.get(1) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 200)
        .take(MAX_ROWS)
        .map(str::to_owned)
        .collect()
}

/// Debounces and supersedes suggestion fetches: a query may start only
/// after typing paused, and only the newest query's answer is wanted.
#[derive(Debug, Default)]
pub struct Debounce {
    pending: Option<(String, Instant)>,
    generation: u64,
}

impl Debounce {
    /// Typing produced `query` at `now`. Supersedes everything earlier.
    pub fn typed(&mut self, query: &str, now: Instant) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some((query.to_owned(), now));
        self.generation
    }

    /// The query to fetch, once typing has been quiet for `delay`.
    pub fn due(&mut self, now: Instant, delay: Duration) -> Option<(u64, String)> {
        let (_, at) = self.pending.as_ref()?;
        if now.saturating_duration_since(*at) < delay {
            return None;
        }
        let (query, _) = self.pending.take()?;
        Some((self.generation, query))
    }

    /// Typing stopped mattering (the bar emptied or lost focus): nothing
    /// pending, every outstanding answer stale.
    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
    }

    /// Whether an answer for `generation` is still wanted.
    pub fn is_current(&self, generation: u64) -> bool {
        generation == self.generation && self.pending.is_none()
    }

    /// Whether a fetch is waiting on the quiet period.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 100_000_000;

    fn entry(url: &str, title: &str, visits: u32, last: u64) -> HistoryEntry {
        HistoryEntry {
            url: url.into(),
            title: title.into(),
            visit_count: visits,
            last_visit: last,
        }
    }

    fn index(entries: &[HistoryEntry]) -> HistoryIndex {
        let mut index = HistoryIndex::default();
        index.rebuild(entries);
        index
    }

    fn urls(matches: &[Match]) -> Vec<&str> {
        matches.iter().map(|m| m.entry.url.as_str()).collect()
    }

    #[test]
    fn host_prefix_beats_substring_whatever_the_frecency() {
        let mut idx = index(&[
            entry(
                "https://blog.example.org/why-you-should",
                "Why you should",
                90,
                NOW,
            ),
            entry("https://www.youtube.com/", "YouTube", 1, NOW - 40 * 86_400),
        ]);
        let hits = idx.search("you", 8, NOW);
        assert_eq!(urls(&hits)[0], "https://www.youtube.com/");
        assert_eq!(hits[0].tier, Tier::Prefix);
        assert_eq!(hits[1].tier, Tier::Word);
    }

    #[test]
    fn host_word_beats_title_substring() {
        let mut idx = index(&[
            entry("https://a.test/x", "Learn about tubes", 5, NOW),
            entry("https://m.youtube.com/watch", "A video", 1, NOW),
        ]);
        let hits = idx.search("tube", 8, NOW);
        assert_eq!(urls(&hits)[0], "https://m.youtube.com/watch");
    }

    #[test]
    fn frecency_orders_within_a_tier() {
        let mut idx = index(&[
            entry("https://youtube.com/a", "A", 1, NOW),
            entry("https://youtube.com/b", "B", 30, NOW),
            entry("https://youtube.com/c", "C", 30, NOW - 60 * 86_400),
        ]);
        let hits = idx.search("youtube", 8, NOW);
        assert_eq!(
            urls(&hits),
            vec![
                "https://youtube.com/b",
                "https://youtube.com/a",
                "https://youtube.com/c"
            ]
        );
        assert!(frecency(30, NOW, NOW) > frecency(1, NOW, NOW));
        assert!(frecency(3, NOW, NOW) > frecency(3, NOW - 30 * 86_400, NOW));
    }

    #[test]
    fn extending_a_query_rescans_only_its_matches() {
        let mut entries: Vec<HistoryEntry> = (0..500)
            .map(|i| entry(&format!("https://site{i}.test/"), "Other", 1, NOW))
            .collect();
        entries.push(entry("https://youtube.com/", "YouTube", 3, NOW));
        entries.push(entry("https://youtu.be/", "YouTube short", 1, NOW));
        let mut idx = index(&entries);
        assert_eq!(urls(&idx.search("y", 8, NOW)).len(), 2);
        assert_eq!(idx.scanned, 502);
        let hits = idx.search("yout", 8, NOW);
        assert_eq!(
            idx.scanned, 2,
            "a longer query only rescans the last matches"
        );
        assert_eq!(hits.len(), 2);
        idx.search("x", 8, NOW);
        assert_eq!(idx.scanned, 502, "a different query starts over");
    }

    #[test]
    fn typed_address_leads_and_pages_are_not_repeated() {
        let mut idx = index(&[entry("https://youtube.com/", "YouTube", 4, NOW)]);
        let hits = idx.search("youtube.com", 8, NOW);
        let rows = build_rows("youtube.com", &hits, &[], "https://other.test/", MAX_ROWS);
        assert_eq!(rows[0].kind, RowKind::Go);
        assert_eq!(rows[0].url, "https://youtube.com");
        // The visited page is the address typed: shown once, as "go to".
        assert_eq!(
            rows.iter().filter(|r| r.kind == RowKind::History).count(),
            0
        );
        assert_eq!(rows.last().map(|r| r.kind), Some(RowKind::Search));
    }

    #[test]
    fn words_lead_with_search_unless_a_page_completes_them() {
        let mut idx = index(&[entry("https://youtube.com/", "YouTube", 4, NOW)]);
        let hits = idx.search("you", 8, NOW);
        let rows = build_rows("you", &hits, &["you tube".into()], "", MAX_ROWS);
        assert_eq!(rows[0].kind, RowKind::History);
        assert_eq!(rows[0].title, "YouTube");
        assert_eq!(rows[1].kind, RowKind::Search);

        let mut idx = index(&[entry(
            "https://a.test/",
            "Notes about rust you wrote",
            4,
            NOW,
        )]);
        let hits = idx.search("rust", 8, NOW);
        let rows = build_rows("rust", &hits, &[], "", MAX_ROWS);
        assert_eq!(rows[0].kind, RowKind::Search);
        assert_eq!(rows[1].kind, RowKind::History);
    }

    #[test]
    fn suggestions_merge_without_duplicates_or_the_typed_text() {
        let remote: Vec<String> = ["Rust", "rust lang", "RUST LANG", "rust book", " "]
            .map(String::from)
            .to_vec();
        let rows = build_rows("rust", &[], &remote, "", MAX_ROWS);
        let kinds: Vec<_> = rows.iter().map(|r| (r.kind, r.title.as_str())).collect();
        assert_eq!(
            kinds,
            [
                (RowKind::Search, "rust"),
                (RowKind::Suggestion, "rust lang"),
                (RowKind::Suggestion, "rust book"),
            ]
        );
        let many: Vec<String> = (0..20).map(|i| format!("rust {i}")).collect();
        assert_eq!(build_rows("rust", &[], &many, "", MAX_ROWS).len(), MAX_ROWS);
    }

    #[test]
    fn current_page_is_not_offered_back() {
        let mut idx = index(&[entry("https://youtube.com/", "YouTube", 4, NOW)]);
        let hits = idx.search("you", 8, NOW);
        let rows = build_rows("you", &hits, &[], "https://www.youtube.com", MAX_ROWS);
        assert!(rows.iter().all(|r| r.kind != RowKind::History));
    }

    #[test]
    fn inline_completion_preserves_case_sensitive_destinations() {
        let mut index = HistoryIndex::default();
        let url = "https://example.test/Reports/Annual?token=AbC";
        index.rebuild(&[HistoryEntry {
            url: url.into(),
            title: "Report".into(),
            last_visit: NOW,
            visit_count: 1,
        }]);
        let hits = index.search("example.test/Re", 8, NOW);
        let completion = inline_completion("example.test/Re", hits.first()).unwrap();
        assert_eq!(completion, "example.test/Reports/Annual?token=AbC");
        let mut rows = Vec::new();
        lead_with_address(&mut rows, &completion);
        assert_eq!(rows[0].url, url);
        assert_eq!(display_url(url), "example.test/Reports/Annual?token=AbC");
        let hits = index.search("example.test/re", 8, NOW);
        assert_eq!(inline_completion("example.test/re", hits.first()), None);
    }

    #[test]
    fn inline_completion_completes_the_host_of_the_best_prefix_match() {
        let mut idx = index(&[entry("https://www.youtube.com/watch?v=1", "Cats", 4, NOW)]);
        let hits = idx.search("You", 8, NOW);
        assert_eq!(
            inline_completion("You", hits.first()).as_deref(),
            Some("Youtube.com")
        );
        let hits = idx.search("youtube.com/wa", 8, NOW);
        assert_eq!(
            inline_completion("youtube.com/wa", hits.first()).as_deref(),
            Some("youtube.com/watch?v=1")
        );
        let hits = idx.search("cats", 8, NOW);
        assert_eq!(inline_completion("cats", hits.first()), None);
        assert_eq!(
            inline_completion("youtube.com", idx.search("youtube.com", 8, NOW).first()),
            None
        );
    }

    #[test]
    fn what_looks_like_an_address() {
        assert!(looks_like_address("youtube.com"));
        assert!(looks_like_address("https://a.b/c"));
        assert!(looks_like_address("localhost:3000"));
        assert!(!looks_like_address("rust lang"));
        assert!(!looks_like_address("you"));
        assert!(!looks_like_address("what is e.g. this"));
    }

    #[test]
    fn suggest_endpoint_follows_the_configured_engine() {
        assert_eq!(
            suggest_endpoint("https://duckduckgo.com/?q=", "a b").as_deref(),
            Some("https://duckduckgo.com/ac/?type=list&q=a+b")
        );
        assert!(
            suggest_endpoint("https://www.google.com/search?q={query}", "x")
                .is_some_and(|u| u.contains("suggestqueries.google.com"))
        );
        assert_eq!(suggest_endpoint("https://example.org/?q=", "x"), None);
        assert_eq!(suggest_endpoint("http://duckduckgo.com/?q=", "x"), None);
    }

    #[test]
    fn opensearch_responses_parse() {
        assert_eq!(
            parse_suggestions(r#"["rus",["rust","rustup",""," "],[],[]]"#),
            ["rust", "rustup"]
        );
        assert!(parse_suggestions("not json").is_empty());
        assert!(parse_suggestions(r#"{"a":1}"#).is_empty());
    }

    #[test]
    fn debounce_waits_for_quiet_and_supersedes() {
        let t0 = Instant::now();
        let mut d = Debounce::default();
        let g1 = d.typed("r", t0);
        assert!(d.due(t0 + Duration::from_millis(50), DEBOUNCE).is_none());
        let g2 = d.typed("ru", t0 + Duration::from_millis(100));
        assert!(
            d.due(t0 + Duration::from_millis(200), DEBOUNCE).is_none(),
            "a keystroke restarts the quiet period"
        );
        let (generation, query) = d
            .due(t0 + Duration::from_millis(260), DEBOUNCE)
            .expect("quiet long enough");
        assert_eq!((generation, query.as_str()), (g2, "ru"));
        assert!(
            d.due(t0 + Duration::from_secs(5), DEBOUNCE).is_none(),
            "fires once"
        );
        assert!(!d.is_current(g1));
        assert!(d.is_current(g2));
    }

    #[test]
    fn cancel_makes_every_outstanding_answer_stale() {
        let t0 = Instant::now();
        let mut d = Debounce::default();
        d.typed("ru", t0);
        let (g, _) = d.due(t0 + DEBOUNCE, DEBOUNCE).expect("due");
        assert!(d.is_current(g));
        d.cancel();
        assert!(
            !d.is_current(g),
            "an answer arriving after cancel is dropped"
        );
        d.typed("rus", t0);
        d.cancel();
        assert!(d.due(t0 + Duration::from_secs(1), DEBOUNCE).is_none());
    }

    #[test]
    fn matching_ten_thousand_pages_fits_a_frame() {
        let entries: Vec<HistoryEntry> = (0..10_000)
            .map(|i| {
                entry(
                    &format!("https://site{i}.example.com/p/{i}"),
                    &format!("Page {i}"),
                    1 + i % 7,
                    NOW - u64::from(i),
                )
            })
            .collect();
        let mut idx = index(&entries);
        let start = Instant::now();
        for q in ["s", "si", "sit", "site", "site1"] {
            idx.search(q, MAX_ROWS, NOW);
        }
        assert!(
            start.elapsed() < Duration::from_millis(250),
            "five keystrokes over 10k pages took {:?}",
            start.elapsed()
        );
    }
}
