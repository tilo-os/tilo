//! The one search ranker.
//!
//! Every surface that orders search hits — the app registry, the in-app
//! palette, catalogue pages and the shell's providers — scores text through
//! this module, so "an exact title beats a title prefix beats a body mention"
//! means the same thing everywhere. A hit is ranked by the best of its
//! weighted fields: the [`MatchTier`] of the query in that field, scaled by
//! the field's [`FieldWeight`]. [`fuzzy_score`] breaks ties between hits of
//! the same tier and lets scattered subsequence matches surface at all.

use super::provider::SearchResult;
use tilo_search::fold as fold_search_text;

pub use tilo_search::{FieldWeight, MAX_FIELD_RANK, MatchTier, SPANNING_RANK};

/// One weighted field of a hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RankField<'a> {
    pub text: &'a str,
    pub weight: FieldWeight,
}

impl<'a> RankField<'a> {
    #[must_use]
    pub const fn new(text: &'a str, weight: FieldWeight) -> Self {
        Self { text, weight }
    }
}

/// The tier at which folded `query` matches folded `text`, if it occurs.
#[must_use]
pub fn match_tier(query: &str, text: &str) -> Option<MatchTier> {
    if query.is_empty() {
        return None;
    }
    if text == query {
        return Some(MatchTier::Exact);
    }
    if text.starts_with(query) {
        return Some(MatchTier::Prefix);
    }
    let mut found = false;
    for (at, _) in text.match_indices(query) {
        found = true;
        let starts_word = text
            .get(..at)
            .and_then(|before| before.chars().next_back())
            .is_none_or(|previous| !previous.is_alphanumeric());
        if starts_word {
            return Some(MatchTier::WordPrefix);
        }
    }
    found.then_some(MatchTier::Contains)
}

/// Rank of a query that is only a subsequence of a full-weight field. A
/// scattered subsequence only means something in a name the person
/// remembers, so lighter fields never match this way.
pub const SUBSEQUENCE_RANK: i32 = 10;

/// Relevance of already folded `fields` for an already folded `query`, on a
/// `0..=MAX_FIELD_RANK` scale, strongest evidence first:
///
/// 1. the query occurs in a field: that field's [`MatchTier`] scaled by its
///    weight (the best such field wins);
/// 2. every word of a multi-word query occurs in some field:
///    [`SPANNING_RANK`];
/// 3. the query is a subsequence of a full-weight field: [`SUBSEQUENCE_RANK`].
///
/// `None` when none applies. An empty query ranks `0`.
#[must_use]
pub fn folded_field_rank(query: &str, fields: &[RankField<'_>]) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let substring = fields
        .iter()
        .filter_map(|field| Some(match_tier(query, field.text)?.rank_in(field.weight)))
        .max();
    if substring.is_some() {
        return substring;
    }
    let mut words = query.split_whitespace().peekable();
    let multi_word = words.clone().nth(1).is_some();
    if multi_word
        && words.peek().is_some()
        && words.all(|word| fields.iter().any(|field| field.text.contains(word)))
    {
        return Some(SPANNING_RANK);
    }
    fields
        .iter()
        .filter(|field| field.weight == FieldWeight::TITLE)
        .any(|field| folded_fuzzy_score(query, field.text).is_some())
        .then_some(SUBSEQUENCE_RANK)
}

/// [`folded_field_rank`] for raw text, folding query and fields with the
/// default tokenizer.
#[must_use]
pub fn field_rank(query: &str, fields: &[RankField<'_>]) -> Option<i32> {
    let query = fold_search_text(query.trim());
    let folded: Vec<String> = fields
        .iter()
        .map(|field| fold_search_text(field.text))
        .collect();
    let folded_fields: Vec<RankField<'_>> = fields
        .iter()
        .zip(&folded)
        .map(|(field, text)| RankField::new(text, field.weight))
        .collect();
    folded_field_rank(&query, &folded_fields)
}

/// [`field_rank`] on a `0.0..=1.0` scale, for surfaces that blend relevance
/// with their own signals (recency, frequency, distance).
#[must_use]
pub fn relevance_fraction(query: &str, fields: &[RankField<'_>]) -> Option<f64> {
    field_rank(query, fields).map(|rank| f64::from(rank) / f64::from(MAX_FIELD_RANK))
}

/// Points one [`field_rank`] step is worth when merging hits: large enough
/// that a tier always outweighs any [`fuzzy_score`] tie-break.
const FIELD_RANK_POINTS: i32 = 20;

/// Query relevance of one displayed hit, without any provider bias: its
/// title and subtitle ranked as weighted fields, plus the [`fuzzy_score`]
/// tie-break of whichever of them matches as a subsequence.
#[must_use]
pub fn result_relevance(query: &str, result: &SearchResult) -> i32 {
    let query = fold_search_text(query.trim());
    if query.is_empty() {
        return 0;
    }
    let title = fold_search_text(&result.title);
    let subtitle = result.subtitle.as_deref().map(fold_search_text);
    let mut fields = vec![RankField::new(&title, FieldWeight::TITLE)];
    if let Some(subtitle) = subtitle.as_deref() {
        fields.push(RankField::new(subtitle, FieldWeight::SUBTITLE));
    }
    let lexical = folded_field_rank(&query, &fields).unwrap_or(0);
    let bonus = folded_fuzzy_score(&query, &title)
        .or_else(|| {
            subtitle
                .as_deref()
                .and_then(|subtitle| folded_fuzzy_score(&query, subtitle))
        })
        .unwrap_or(0);
    lexical
        .saturating_mul(FIELD_RANK_POINTS)
        .saturating_add(bonus)
}

/// Case- and diacritic-insensitive subsequence fuzzy score: `Some(score)` when
/// `query` matches `text` as a subsequence, `None` otherwise. Higher is a
/// better match. An empty query matches everything with score `0`.
///
/// The score rewards, in order of weight, matches that are **contiguous**, that
/// land on a **word boundary** (start of string, or right after a space /
/// separator), and that **start** the string; it penalises gaps between matched
/// characters.
#[must_use]
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    folded_fuzzy_score(&fold_search_text(query), &fold_search_text(text))
}

fn folded_fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.chars().collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.chars().collect();

    let is_boundary = |i: usize| -> bool {
        if i == 0 {
            return true;
        }
        t.get(i.saturating_sub(1))
            .is_none_or(|prev| !prev.is_alphanumeric())
    };

    let mut qi = 0usize;
    let mut score = 0i32;
    let mut prev_match: Option<usize> = None;
    let mut streak = 0i32;

    for (ti, ch) in t.iter().enumerate() {
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
    fn tiers_order_exact_prefix_word_and_infix_matches() {
        assert_eq!(match_tier("dune", "dune"), Some(MatchTier::Exact));
        assert_eq!(match_tier("dune", "dune notes"), Some(MatchTier::Prefix));
        assert_eq!(
            match_tier("dune", "the-dune-notes.md"),
            Some(MatchTier::WordPrefix)
        );
        assert_eq!(match_tier("dune", "redundant"), None);
        assert_eq!(match_tier("dun", "redundant"), Some(MatchTier::Contains));
        assert_eq!(match_tier("", "anything"), None);
        // A later word-start occurrence wins over an earlier infix one.
        assert_eq!(
            match_tier("dun", "redundant dune"),
            Some(MatchTier::WordPrefix)
        );
    }

    #[test]
    fn field_weights_order_title_over_subtitle_over_body() {
        let title = field_rank("atlas", &[RankField::new("Atlas", FieldWeight::TITLE)]);
        let subtitle = field_rank(
            "atlas",
            &[RankField::new("Atlas Press", FieldWeight::SUBTITLE)],
        );
        let body = field_rank(
            "atlas",
            &[RankField::new("an atlas of stars", FieldWeight::BODY)],
        );
        assert_eq!(title, Some(MAX_FIELD_RANK));
        assert!(
            title > subtitle && subtitle > body,
            "{title:?} {subtitle:?} {body:?}"
        );
        assert_eq!(
            field_rank(
                "atlas",
                &[
                    RankField::new("unrelated", FieldWeight::TITLE),
                    RankField::new("atlas", FieldWeight::BODY),
                ],
            ),
            Some(MatchTier::Exact.percent() * i32::from(FieldWeight::BODY.percent()) / 100),
            "the best matching field ranks the hit"
        );
        assert_eq!(
            field_rank("zzz", &[RankField::new("atlas", FieldWeight::TITLE)]),
            None
        );
    }

    #[test]
    fn words_spread_over_fields_and_subsequences_rank_below_any_substring() {
        let fields = [
            RankField::new("LibreOffice Calc", FieldWeight::TITLE),
            RankField::new("office spreadsheet program", FieldWeight::BODY),
        ];
        let spanning = field_rank("calc spreadsheet", &fields).unwrap();
        let subsequence = field_rank("lbrcalc", &fields).unwrap();
        let body = field_rank("spreadsheet", &fields).unwrap();
        assert!(
            body > spanning && spanning > subsequence,
            "{body} {spanning} {subsequence}"
        );
        // Only a full-weight field matches as a subsequence.
        assert_eq!(field_rank("sprdsht", &fields), None);
        assert_eq!(field_rank("calc zebra", &fields), None);
    }

    #[test]
    fn ranking_is_diacritic_and_case_insensitive() {
        let fields = [RankField::new("Crème Brûlée", FieldWeight::TITLE)];
        assert_eq!(field_rank("creme brulee", &fields), Some(MAX_FIELD_RANK));
        assert_eq!(field_rank("CRÈME", &fields), field_rank("creme", &fields));
    }

    #[test]
    fn relevance_prefers_a_title_hit_over_a_subtitle_hit() {
        let title = SearchResult::new("Apps", "Calculator", "a");
        let subtitle =
            SearchResult::new("Files", "Draft", "f").with_subtitle("Quarterly calculator notes");
        assert!(result_relevance("calculator", &title) > result_relevance("calculator", &subtitle));
        assert!(result_relevance("calculator", &subtitle) > 0);
        assert_eq!(result_relevance("", &title), 0);
    }

    #[test]
    fn relevance_fraction_orders_substring_over_subsequence_matches() {
        let fields = |text| [RankField::new(text, FieldWeight::TITLE)];
        let prefix = relevance_fraction("git", &fields("GitHub")).unwrap();
        let scattered = relevance_fraction("git", &fields("goldieturtle")).unwrap();
        assert!((prefix - 0.88).abs() < 1e-9);
        assert!(prefix > scattered && scattered > 0.0);
        assert_eq!(relevance_fraction("zzz", &fields("example.com")), None);
        assert_eq!(relevance_fraction("acb", &fields("abc")), None);
    }

    #[test]
    fn fuzzy_prefix_beats_midword_beats_scattered() {
        let prefix = fuzzy_score("doc", "Documents").unwrap();
        let midword = fuzzy_score("doc", "My docs").unwrap();
        let scattered = fuzzy_score("doc", "Dialog of Chess").unwrap();
        assert!(prefix > scattered, "{prefix} {scattered}");
        assert!(midword > scattered, "{midword} {scattered}");
        assert_eq!(
            fuzzy_score("éclair", "Eclairs"),
            fuzzy_score("eclair", "Éclairs")
        );
    }
}
