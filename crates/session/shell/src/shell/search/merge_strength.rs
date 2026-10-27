//! What the merge credits a row with beside its lexical fit: nearness in
//! Workspace scope, what the person chose before, the open window a query
//! names, and the order loose and file matches give way in.

use std::collections::HashSet;

use ui::search::system;

use super::{
    QueryArgs, ResourceKind, SearchProvider, SearchProximity, SearchResult, SearchScope,
    provider_score, usage,
};

/// How much nearer the person a row is in Workspace scope: from the focused
/// app's provider, and in the workspace being looked at.
pub(super) fn proximity_boost(
    provider: &dyn SearchProvider,
    row: &SearchResult,
    args: &QueryArgs<'_>,
) -> f32 {
    if args.scope != SearchScope::Workspace {
        return 0.0;
    }
    let active_app = if args.active_provider_id == Some(provider.id()) {
        system::ACTIVE_APP_PROXIMITY_BOOST
    } else {
        0.0
    };
    let workspace = if provider.proximity(row, args.chrome) == SearchProximity::CurrentWorkspace {
        system::ACTIVE_APP_PROXIMITY_BOOST
    } else {
        0.0
    };
    active_app + workspace
}

/// Queries this short are what launchers live on: the thing chosen most
/// leads every other prefix match.
const SHORT_QUERY_SCALARS: usize = 3;
/// How much a row's whole frecency adds at the merge, for a short query and
/// for a longer one. For a short query it outweighs the gap between an exact
/// name and a prefix (1.5) and an app's launch intent; for a longer one it
/// only settles near ties.
const SHORT_QUERY_USAGE: f32 = ui::tokens::search::SHORT_QUERY_USAGE;
const LONG_QUERY_USAGE: f32 = 0.4;
/// What a row chosen before for the same typed text adds: it leads.
const SAME_TEXT_CHOICE: f32 = ui::tokens::search::SAME_TEXT_CHOICE;
/// A title matched from a word start: the hits frecency may reorder.
const WORD_START_LEXICAL: f32 = 1.5;

/// What the person chose before adds to a row at the merge: how often and
/// how recently they chose it, which leads a short query's prefix matches
/// ("t" is the Terminal they open daily, not whichever action sorts first),
/// and whether they chose it before for this very text.
pub(super) fn usage_strength(needle: &str, row: &SearchResult) -> f32 {
    let typed = needle.trim();
    if typed.is_empty() {
        return 0.0;
    }
    let choice = usage::choice_share(typed, &row.provider_id, &row.id) * SAME_TEXT_CHOICE;
    let lexical = system::lexical_strength(typed, &row.kind, &row.title, None);
    if lexical < WORD_START_LEXICAL {
        return choice;
    }
    let weight = if typed.chars().count() <= SHORT_QUERY_SCALARS {
        SHORT_QUERY_USAGE
    } else {
        LONG_QUERY_USAGE
    };
    choice + usage::share(&row.provider_id, &row.id) * weight
}

/// A window named by a query is as much what the person reaches for as the
/// app it belongs to: it gets the launch intent an app row gets.
pub(super) fn window_intent(needle: &str, row: &SearchResult, top: i32) -> f32 {
    if needle.trim().is_empty()
        || row.kind != ResourceKind::Tab
        || row.operation_kind != tilo_actions::OperationKind::View
    {
        return 0.0;
    }
    system::application_intent_boost(row.score, top)
}

/// What a row that matched every word of the query, spread over its fields
/// (the least a real match ranks), scores on its provider's scale, as a
/// share of the top. A row below it matched only by a typo.
#[allow(
    clippy::cast_precision_loss,
    reason = "provider scores are small integers"
)]
fn loose_share() -> f32 {
    provider_score(tilo_search::SPANNING_RANK) as f32 / system::MAX_PROVIDER_SCORE as f32
}

/// Whether `row` matched `needle` only loosely: by a typo, with nothing of
/// the query in its title. A row that holds every word the person typed,
/// spread over its fields ("gnome calculator": GNOME's Calculator, whose
/// title is "Calculator"), is a real match, not a loose one.
#[allow(
    clippy::cast_precision_loss,
    reason = "provider scores are small integers"
)]
pub(super) fn only_loosely_matched(needle: &str, row: &SearchResult, top: i32) -> bool {
    (row.score as f32 / top.max(1) as f32) < loose_share()
        && system::lexical_strength(needle, &row.kind, &row.title, None) < 1.0
}

/// A typo is how a misspelt name is still found, not a reason to list
/// everything one letter away from a word that matched: once anything
/// matches the query as typed, the rows that only match it loosely go
/// ("move" lists the move actions, not "Mobile broadband").
pub(super) fn loose_matches_give_way(
    merged: &mut Vec<(SearchResult, f32)>,
    loose: &HashSet<(String, String)>,
) {
    let key = |row: &SearchResult| (row.provider_id.clone(), row.id.clone());
    if merged.iter().any(|(row, _)| !loose.contains(&key(row))) {
        merged.retain(|(row, _)| !loose.contains(&key(row)));
    }
}

/// Whether typed text asks for a file by its shape: a path, a home-relative
/// path, or a name with an extension ("report.pdf").
pub(super) fn looks_like_a_path(needle: &str) -> bool {
    let typed = needle.trim();
    if typed.contains('/') || typed.starts_with('~') {
        return true;
    }
    typed.rsplit_once('.').is_some_and(|(stem, extension)| {
        !stem.is_empty()
            && (1..=5).contains(&extension.len())
            && extension.chars().all(|c| c.is_ascii_alphanumeric())
            && extension.chars().any(|c| c.is_ascii_alphabetic())
    })
}

/// Files and folders follow every app, window, action and setting the query
/// names by a word start of its title, unless the query is shaped like a
/// file ([`looks_like_a_path`]). A launcher that leads with
/// `Settings-backup.txt` for "settings" is a filename grep; the files are
/// still there, below what the person most likely meant.
pub(super) fn files_follow_what_was_named(merged: &mut [(SearchResult, f32)], needle: &str) {
    if looks_like_a_path(needle) {
        return;
    }
    let is_file = |kind: &ResourceKind| matches!(kind, ResourceKind::File | ResourceKind::Folder);
    let is_named = |kind: &ResourceKind| {
        matches!(
            kind,
            ResourceKind::Action
                | ResourceKind::Application
                | ResourceKind::Tab
                | ResourceKind::WindowGroup
                | ResourceKind::Custom(_)
        )
    };
    // The weakest best of the kinds whose title the query named: files sit
    // just below it, so no named group is ordered after them.
    let mut best: HashMap<(u8, String), f32> = HashMap::new();
    for (row, strength) in merged.iter() {
        if is_named(&row.kind)
            && system::lexical_strength(needle, &row.kind, &row.title, None) >= WORD_START_LEXICAL
        {
            let slot = best.entry(row.kind.sort_key()).or_insert(f32::MIN);
            *slot = slot.max(*strength);
        }
    }
    let Some(floor) = best.values().copied().reduce(f32::min) else {
        return;
    };
    let ceiling = floor - 0.01;
    for (row, strength) in merged.iter_mut() {
        if is_file(&row.kind) && *strength >= floor {
            *strength = ceiling;
        }
    }
}
