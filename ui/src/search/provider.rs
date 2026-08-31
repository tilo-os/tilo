//! The [`SearchProvider`] trait, the typed [`SearchResult`] it returns, and the
//! shared [`fuzzy_score`] ranking primitive the registry uses to merge results
//! from many providers on an even footing.

use crate::icons::Icon;

/// One typed hit produced by a [`SearchProvider`].
///
/// App-agnostic on purpose: an app fills the display fields (`title`,
/// `subtitle`, `kind`, `icon`) and an opaque `action_id` it can dispatch on
/// when the row is activated. The framework never interprets `action_id` — it
/// hands it straight back through
/// [`SearchResultsView::take_activated`](crate::widget::SearchResultsView::take_activated).
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
    /// Opaque token the caller dispatches on when this row is activated —
    /// a record id, a command name, a URL, whatever the app can resolve.
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
}
