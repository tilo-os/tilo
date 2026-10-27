//! The rows that answer any query (asking an agent, searching the web):
//! whether anything matched directly, which decides whether they lead, and
//! how they share the card with the matches.

use ui::search::system;

use super::{KindGroup, ResourceKind, SearchProvider, SearchRegistry, SearchResult};

/// Put the rows that answer any query (`fallbacks`, each with its rank)
/// beside the matches in `merged`: after them when something matched
/// directly, which Enter then runs; before them otherwise, so Enter hands
/// the query on. Among themselves they go by rank, in their providers'
/// order on a tie.
pub(super) fn place(
    merged: &mut Vec<(SearchResult, f32)>,
    mut fallbacks: Vec<(SearchResult, f32)>,
    direct: bool,
) {
    fallbacks.sort_by(|a, b| a.1.total_cmp(&b.1));
    if direct {
        merged.append(&mut fallbacks);
    } else {
        fallbacks.append(merged);
        *merged = fallbacks;
    }
}

impl SearchRegistry {
    /// Whether `result` answers any query; see
    /// [`SearchProvider::is_fallback`].
    pub fn is_fallback(&self, result: &SearchResult) -> bool {
        self.enabled_providers()
            .find(|provider| provider.id() == result.provider_id)
            .is_some_and(|provider| provider.is_fallback(result))
    }

    /// `groups` cut to `row_budget` rows, each group a fair share
    /// ([`system::trim_groups`]), except the groups of rows that answer any
    /// query: they keep their rows and take them off the budget, never a
    /// share, so asking an agent and searching the web cost a match no more
    /// than their own rows.
    pub fn trim_groups(
        &self,
        groups: Vec<KindGroup>,
        row_budget: usize,
        expanded: &[ResourceKind],
    ) -> Vec<KindGroup> {
        let answers_any = |group: &KindGroup| {
            !group.results.is_empty() && group.results.iter().all(|row| self.is_fallback(row))
        };
        let lead = groups.iter().take_while(|group| answers_any(group)).count();
        let mut groups = groups;
        let mut trailing = Vec::new();
        while groups.len() > lead && groups.last().is_some_and(answers_any) {
            trailing.extend(groups.pop());
        }
        trailing.reverse();
        let leading: Vec<KindGroup> = groups.drain(..lead).collect();
        let kept: usize = leading
            .iter()
            .chain(&trailing)
            .map(|group| group.results.len())
            .sum();
        let budget = if row_budget == 0 {
            0
        } else {
            row_budget.saturating_sub(kept).max(1)
        };
        let middle = system::trim_groups(groups, budget, expanded);
        leading.into_iter().chain(middle).chain(trailing).collect()
    }
}

/// Whether `row` matches `needle` directly, as a thing the person named:
/// an app, an action or a window, which are found only by their names
/// (however loosely: a typo, initials, a word they are known by); a row its
/// provider ranks in the upper half of its scale, which is how a provider
/// says the query named it rather than something inside it; a row whose
/// title or another of its names ([`SearchProvider::match_reason`]) the
/// whole query fits, or whose title and subtitle show every word of it; or
/// a row the query names some other way the merge already credits
/// (`boost`: a service it searches, an open window it names, the row
/// chosen for this text before). Content found only by what is inside it,
/// or by a stray word of a longer sentence, is not.
#[allow(
    clippy::cast_precision_loss,
    reason = "provider scores are small integers"
)]
pub(super) fn directly_matched(
    provider: &dyn SearchProvider,
    row: &SearchResult,
    needle: &str,
    top: i32,
    boost: f32,
) -> bool {
    /// The share of its provider's scale from which a row was named.
    const NAMED_SHARE: f32 = 0.5;
    use ui::search::FieldWeight;
    let named = |name: &str| system::lexical_strength(needle, &row.kind, name, None) > 0.0;
    let shown = |word: &str| {
        tilo_search::locate(word, &row.title, FieldWeight::TITLE).is_some()
            || row.subtitle.as_deref().is_some_and(|subtitle| {
                tilo_search::locate(word, subtitle, FieldWeight::SUBTITLE).is_some()
            })
    };
    boost > 0.0
        || matches!(
            row.kind,
            ResourceKind::Application
                | ResourceKind::Action
                | ResourceKind::Tab
                | ResourceKind::WindowGroup
        )
        || row.score as f32 / top.max(1) as f32 >= NAMED_SHARE
        || named(&row.title)
        || needle.split_whitespace().all(shown)
        || provider
            .match_reason(row, needle)
            .is_some_and(|name| named(&name))
}
