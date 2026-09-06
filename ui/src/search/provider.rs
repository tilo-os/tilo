//! The [`SearchProvider`] trait, the typed [`SearchResult`] it returns, and the
//! shared [`fuzzy_score`] ranking primitive the registry uses to merge results
//! from many providers on an even footing.

use crate::icons::Icon;
use std::collections::HashSet;

/// Maximum number of rows a provider should return from the legacy, unbudgeted
/// [`SearchProvider::search`] entry point. Interactive callers use
/// `search_limited`; this ceiling keeps tests and secondary consumers from
/// accidentally materialising an entire library.
pub const DEFAULT_SEARCH_RESULT_LIMIT: usize = 256;

/// Default maximum number of records copied into an in-memory provider
/// snapshot. Providers with a real indexed backend may use a different,
/// explicitly reviewed bound.
pub const DEFAULT_SEARCH_INDEX_LIMIT: usize = 4_096;
/// Default maximum number of full records an indexed provider should verify
/// for one interactive query. The index may contain the complete corpus; this
/// is a query-time latency budget, not a data-loss limit.
pub const DEFAULT_SEARCH_CANDIDATE_LIMIT: usize = 256;
use qos_actions::ActionAddress;
use std::borrow::Borrow;

/// A compact, reusable exact substring index for app search providers.
///
/// The complete supplied corpus is retained. Normalisation and 1/2/3-scalar
/// suffixes are built once, so an interactive query finds the exact matching
/// suffix range without scanning or speculatively verifying full records.
/// Providers keep
/// ownership of their domain records and use the returned stable snapshot
/// offsets to materialise typed [`SearchResult`]s.
///
/// Each suffix is represented by a record id and UTF-8 boundary, so storage is
/// linear in the amount of normalised searchable text. Embedded NULs and all
/// Unicode scalar values retain their ordinary string semantics.
#[derive(Clone, Debug, Default)]
pub struct SearchTextIndex {
    texts: Vec<String>,
    suffixes: Vec<(usize, usize)>,
}

impl SearchTextIndex {
    /// Build an index over every supplied searchable record.
    pub fn new<S>(texts: impl IntoIterator<Item = S>) -> Self
    where
        S: AsRef<str>,
    {
        let texts: Vec<String> = texts
            .into_iter()
            .map(|text| normalize_search_text(text.as_ref()))
            .collect();
        let mut suffixes: Vec<_> = texts
            .iter()
            .enumerate()
            .flat_map(|(record_id, text)| {
                text.char_indices()
                    .map(move |(byte_offset, _)| (record_id, byte_offset))
            })
            .collect();
        suffixes.sort_unstable_by(|&(left_id, left_offset), &(right_id, right_offset)| {
            indexed_suffix(&texts, left_id, left_offset).cmp(indexed_suffix(
                &texts,
                right_id,
                right_offset,
            ))
        });

        Self { texts, suffixes }
    }

    /// Number of complete records represented by this snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.texts.len()
    }

    /// Whether the snapshot contains no records.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }

    /// Return matching snapshot offsets while inspecting at most `budget`
    /// complete texts. For broad queries the earliest matching records in the
    /// provider's deliberate snapshot order form the bounded result window.
    #[must_use]
    pub fn matching_ids(&self, query: &str, budget: usize) -> Vec<usize> {
        if budget == 0 {
            return Vec::new();
        }
        let query = normalize_search_text(query);
        if query.is_empty() {
            return (0..self.texts.len()).take(budget).collect();
        }

        let first = self.suffixes.partition_point(|&(record_id, byte_offset)| {
            indexed_suffix(&self.texts, record_id, byte_offset) < query.as_str()
        });
        let mut seen = HashSet::new();
        self.suffixes
            .get(first..)
            .unwrap_or_default()
            .iter()
            .take_while(|&&(record_id, byte_offset)| {
                indexed_suffix(&self.texts, record_id, byte_offset).starts_with(&query)
            })
            .filter_map(|&(record_id, _)| seen.insert(record_id).then_some(record_id))
            .take(budget)
            .collect()
    }

    /// The normalised text stored for a snapshot offset, for provider-specific
    /// ranking without repeating normalisation on every keystroke.
    #[must_use]
    pub fn normalized_text(&self, record_id: usize) -> Option<&str> {
        self.texts.get(record_id).map(String::as_str)
    }
}

fn indexed_suffix(texts: &[String], record_id: usize, byte_offset: usize) -> &str {
    texts
        .get(record_id)
        .and_then(|text| text.get(byte_offset..))
        .unwrap_or_default()
}

/// Apply the shared case-folding convention used by [`SearchTextIndex`].
#[must_use]
pub fn normalize_search_text(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// Marks an `action_id` as a framework-owned canonical activation. The tag
/// keeps arbitrary legacy ids from becoming actions merely because they happen
/// to contain qos-actions' wire separator.
const ACTION_ADDRESS_PREFIX: &str = "\u{1}action\u{1f}";

/// One typed hit produced by a [`SearchProvider`].
///
/// App-agnostic on purpose: an app fills the display fields (`title`,
/// `subtitle`, `kind`, `icon`) and an activation target. New providers should
/// use [`SearchResult::for_action`] or [`SearchResult::with_action_address`] to
/// name an exported resource action. The palette resolves that address against
/// the current resource tree and sends it through the same operation broker as
/// every other affordance.
///
/// `action_id` remains public and opaque for compatibility with providers that
/// predate the resource/action model. Those legacy values are still handed to
/// the owning widget's navigation callback when they are activated.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchResult {
    /// Primary line — what the row reads as (e.g. a contact's display name).
    pub title: String,
    /// Optional secondary line rendered dimmer under the title (e.g. an email
    /// address, a file path, a command description).
    pub subtitle: Option<String>,
    /// Grouping label, e.g. `"Contacts"`, `"Files"`, `"Commands"`. Rows sharing
    /// a `kind` are bucketed together under this label in the results overlay,
    /// so a mixed query reads as sectioned groups rather than one flat list.
    pub kind: String,
    /// Activation token. Prefer the canonical encoded [`ActionAddress`] made
    /// by [`for_action`](Self::for_action) or
    /// [`with_action_address`](Self::with_action_address). Any value that is
    /// not tagged as canonical is treated as a legacy opaque navigation id.
    /// A tagged address that is not currently exported is omitted by the
    /// palette rather than gaining authority through the legacy callback.
    pub action_id: String,
    /// Optional leading glyph drawn at the start of the row.
    pub icon: Option<Icon>,
    /// Provider-supplied relevance bias — a nudge, not an override, since the
    /// registry adds a query-match bonus on top. `0` ranks by query match
    /// alone; raise it to float pinned/recent/favourite hits.
    pub score: i32,
}

impl SearchResult {
    /// A result with the three required fields; chain the builders for the rest.
    pub fn new(
        kind: impl Into<String>,
        title: impl Into<String>,
        action_id: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            kind: kind.into(),
            action_id: action_id.into(),
            icon: None,
            score: 0,
        }
    }

    /// A result that activates a canonical exported resource action.
    ///
    /// This is the preferred constructor for new providers. The address is
    /// encoded into the existing `action_id` storage so adding canonical
    /// activation does not break providers built against the legacy shape.
    pub fn for_action(
        kind: impl Into<String>,
        title: impl Into<String>,
        address: impl Borrow<ActionAddress>,
    ) -> Self {
        Self::new(
            kind,
            title,
            format!("{ACTION_ADDRESS_PREFIX}{}", address.borrow().encode()),
        )
    }

    /// Replace this hit's legacy activation id with a canonical action
    /// address while retaining its display metadata.
    #[must_use]
    pub fn with_action_address(mut self, address: impl Borrow<ActionAddress>) -> Self {
        self.action_id = format!("{ACTION_ADDRESS_PREFIX}{}", address.borrow().encode());
        self
    }

    /// Decode this hit's canonical action address, if it carries one.
    pub fn action_address(&self) -> Option<ActionAddress> {
        self.action_id
            .strip_prefix(ACTION_ADDRESS_PREFIX)
            .and_then(ActionAddress::parse)
    }

    /// Whether this result opted into canonical action activation.
    pub(crate) fn has_action_address(&self) -> bool {
        self.action_id.starts_with(ACTION_ADDRESS_PREFIX)
    }

    #[must_use]
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    #[must_use]
    pub fn with_icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    #[must_use]
    pub fn with_score(mut self, score: i32) -> Self {
        self.score = score;
        self
    }
}

/// A source of search results — implemented by an app for each searchable
/// *type* it exposes (its contacts, its open files, its commands, …).
///
/// A provider **filters** to plausible matches; the
/// [`SearchRegistry`](crate::search::SearchRegistry) **ranks** across
/// providers via [`fuzzy_score`]. Return everything you consider a match (a
/// contact whose phone number matches even though the title is a name, say) —
/// the registry floats the strongest title matches without discarding
/// your field-aware hits.
pub trait SearchProvider {
    /// The kind label this provider produces, used for diagnostics and as the
    /// default group header. Individual results may still carry a different
    /// [`SearchResult::kind`].
    fn name(&self) -> &str;

    /// Return the matching results for `query`. An empty query conventionally
    /// returns a provider's "browse" set (recents / favourites / all), or an
    /// empty vec if browsing makes no sense for the source.
    fn search(&self, query: &str) -> Vec<SearchResult>;

    /// Query with a materialisation budget. App providers must override this
    /// and stop constructing hits once `limit` is reached. The fallback exists
    /// only for tiny/test providers outside `apps/`; the repository contract
    /// checker rejects app implementations that inherit it.
    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let mut hits = self.search(query);
        hits.truncate(limit);
        hits
    }

    /// Lazily resolve a canonical search address that is intentionally absent
    /// from the small open-resource snapshot.
    fn resolve_action_resource(&self, _address: &ActionAddress) -> Option<qos_actions::Resource> {
        None
    }
}

/// Case-insensitive subsequence fuzzy score: `Some(score)` when `query` matches
/// `text` as a subsequence, `None` otherwise. Higher is a better match. An
/// empty query matches everything with score `0`.
///
/// The score rewards, in order of weight, matches that are **contiguous**, that
/// land on a **word boundary** (start of string, or right after a space /
/// separator), and that **start** the string; it penalises gaps between matched
/// characters. This is the single ranking primitive the registry uses so a hit
/// from one provider is comparable to a hit from another.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();

    let is_boundary = |i: usize| -> bool {
        if i == 0 {
            return true;
        }
        // `i > 0` here, so `i - 1` is always a valid index into `t`; go
        // through `get` anyway so the lookup can never panic.
        t.get(i.saturating_sub(1))
            .is_none_or(|prev| !prev.is_alphanumeric())
    };

    let mut qi = 0usize;
    let mut score = 0i32;
    let mut prev_match: Option<usize> = None;
    let mut streak = 0i32;

    for (ti, ch) in t.iter().enumerate() {
        // `q.get` doubles as the "query exhausted" check the old `qi >=
        // q.len()` guard made explicit.
        let Some(&qc) = q.get(qi) else {
            break;
        };
        if *ch == qc {
            let mut bonus = 1i32;
            match prev_match {
                Some(pm) if ti.checked_sub(1) == Some(pm) => {
                    // Contiguous with the previous match — reward the streak so
                    // "docs" beats a scattered "d…o…c…s".
                    streak = streak.saturating_add(1);
                    bonus = bonus.saturating_add(2).saturating_add(streak);
                }
                Some(pm) => {
                    // A gap: pay for the distance skipped, reset the streak.
                    let gap = ti.saturating_sub(pm).saturating_sub(1).min(3);
                    bonus = bonus.saturating_sub(i32::try_from(gap).unwrap_or(3));
                    streak = 0;
                }
                None => {
                    // First matched char: reward starting at, or near, the front.
                    if ti == 0 {
                        bonus = bonus.saturating_add(4);
                    } else if is_boundary(ti) {
                        bonus = bonus.saturating_add(3);
                    } else {
                        let penalty = i32::try_from(ti.min(3)).unwrap_or(3);
                        bonus = bonus.saturating_sub(penalty);
                    }
                    streak = 0;
                }
            }
            if prev_match.is_some() && is_boundary(ti) {
                bonus = bonus.saturating_add(2);
            }
            score = score.saturating_add(bonus);
            prev_match = Some(ti);
            qi = qi.saturating_add(1);
        }
    }

    if qi < q.len() { None } else { Some(score) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn non_subsequence_does_not_match() {
        assert_eq!(fuzzy_score("xyz", "alphabet"), None);
        assert_eq!(fuzzy_score("abz", "abc"), None);
    }

    #[test]
    fn canonical_action_address_round_trips_without_changing_legacy_shape() {
        let address = ActionAddress::new("contacts.contact:ada", "open");
        let hit = SearchResult::for_action("Contacts", "Ada", address.clone());
        assert_eq!(hit.action_address(), Some(address));

        let legacy = SearchResult::new("Contacts", "Ada", "contact:ada");
        assert_eq!(legacy.action_address(), None);
    }

    #[test]
    fn prefix_beats_midword_beats_scattered() {
        // "cal" as a prefix of "Calculator" must outrank the same letters
        // scattered through "Critical Analysis Log".
        let prefix = fuzzy_score("cal", "Calculator").unwrap();
        let scattered = fuzzy_score("cal", "Critical Analysis Log").unwrap();
        assert!(
            prefix > scattered,
            "prefix {prefix} should beat scattered {scattered}"
        );
    }

    #[test]
    fn word_boundary_match_scores_well() {
        // Matching the start of the second word ("Ng") should score positively
        // — a common case for "last name" queries over "First Last".
        let boundary = fuzzy_score("ng", "Alice Ng").unwrap();
        assert!(
            boundary > 0,
            "word-boundary match should be positive, got {boundary}"
        );
    }

    #[test]
    fn contiguous_beats_gappy() {
        let contiguous = fuzzy_score("abc", "abcdef").unwrap();
        let gappy = fuzzy_score("abc", "a_b_c_def").unwrap();
        assert!(
            contiguous > gappy,
            "contiguous {contiguous} should beat gappy {gappy}"
        );
    }

    #[test]
    fn is_case_insensitive() {
        assert!(fuzzy_score("ALICE", "alice").is_some());
        assert_eq!(fuzzy_score("Alice", "alice"), fuzzy_score("alice", "ALICE"));
    }

    #[test]
    fn result_builders_populate_fields() {
        let r = SearchResult::new("Contacts", "Alice Ng", "contact:42")
            .with_subtitle("alice@example.com")
            .with_icon(Icon::Mail)
            .with_score(5);
        assert_eq!(r.kind, "Contacts");
        assert_eq!(r.title, "Alice Ng");
        assert_eq!(r.action_id, "contact:42");
        assert_eq!(r.subtitle.as_deref(), Some("alice@example.com"));
        assert_eq!(r.icon, Some(Icon::Mail));
        assert_eq!(r.score, 5);
    }

    #[test]
    fn shared_text_index_keeps_the_complete_corpus() {
        let texts =
            (0..(DEFAULT_SEARCH_INDEX_LIMIT + 500)).map(|index| format!("Document {index}"));
        let index = SearchTextIndex::new(texts);

        assert_eq!(index.len(), DEFAULT_SEARCH_INDEX_LIMIT + 500);
        assert_eq!(
            index.matching_ids("Document 4595", DEFAULT_SEARCH_CANDIDATE_LIMIT),
            vec![4595]
        );
    }

    #[test]
    fn shared_text_index_is_unicode_aware_and_case_insensitive() {
        let index = SearchTextIndex::new(["Résumé — Αθήνα", "東京 notes"]);

        assert_eq!(index.matching_ids("ΑΘΉ", 8), vec![0]);
        assert_eq!(index.matching_ids("東京", 8), vec![1]);
        assert!(index.matching_ids("resume", 8).is_empty());
    }

    #[test]
    fn shared_text_index_honours_the_query_budget() {
        let index = SearchTextIndex::new((0..1_000).map(|i| format!("common item {i}")));
        assert_eq!(index.matching_ids("common", 7).len(), 7);
        assert!(index.matching_ids("common", 0).is_empty());
    }

    #[test]
    fn embedded_nul_substrings_remain_distinct() {
        let index = SearchTextIndex::new(["a", "a\0", "a\0\0"]);
        assert_eq!(index.matching_ids("a\0\0", 8), vec![2]);
    }

    #[test]
    fn repeated_occurrences_do_not_consume_the_result_budget_twice() {
        let index = SearchTextIndex::new(["aaaaaa", "aaab"]);
        assert_eq!(index.matching_ids("aaa", 2), vec![0, 1]);
    }

    #[test]
    fn exact_suffix_range_does_not_lose_late_matches_to_false_candidates() {
        let mut texts = vec!["abcXbcd"; DEFAULT_SEARCH_CANDIDATE_LIMIT + 40];
        texts.push("the exact abcd resource");
        let index = SearchTextIndex::new(texts);

        assert_eq!(
            index.matching_ids("abcd", DEFAULT_SEARCH_CANDIDATE_LIMIT),
            vec![DEFAULT_SEARCH_CANDIDATE_LIMIT + 40]
        );
    }
}
