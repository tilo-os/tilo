//! How the merge places rows: each source's own order, and how much nearer
//! the person a row is in the workspace they are looking at.
//!
//! Both are applied after the providers have answered, so they change the
//! order a search shows without changing anything a provider ranked.

use std::collections::HashMap;

use ui::search::system;

use super::{QueryArgs, SearchProvider, SearchProximity, SearchResult, SearchScope};

/// An app orders its own rows ([`SearchProvider::source_order`]): among
/// one source's rows the merge hands the strongest strengths and earliest
/// places to the rows the source ranks first, whatever the merge read in
/// their titles. Tube's "Search `YouTube` for “lofi”" leads the videos that
/// begin with "lofi", so Enter searches, and the videos keep `YouTube`'s
/// order. The rows only trade their strengths and places among themselves,
/// so a source's rows weigh no more against other providers' than they
/// did: no ranking of its own floods a search with one app.
pub(super) fn sources_keep_their_order(
    merged: &mut Vec<(SearchResult, f32)>,
    providers: &HashMap<&str, &dyn SearchProvider>,
) {
    // Each source's rows: where each stands, its strength, its rank there.
    type Rows = Vec<(usize, f32, i64)>;
    let mut sources: HashMap<(String, String), Rows> = HashMap::new();
    for (at, (row, strength)) in merged.iter().enumerate() {
        let Some((source, order)) = providers
            .get(row.provider_id.as_str())
            .and_then(|provider| provider.source_order(row))
        else {
            continue;
        };
        sources
            .entry((row.provider_id.clone(), source))
            .or_default()
            .push((at, *strength, order));
    }
    // Where each place's row and strength come from.
    let mut from: Vec<(usize, f32)> = merged
        .iter()
        .enumerate()
        .map(|(at, (_, strength))| (at, *strength))
        .collect();
    let mut moved = false;
    for mut rows in sources.into_values().filter(|rows| rows.len() > 1) {
        // The places and strengths these rows hold, best first ...
        let mut places: Vec<(usize, f32)> = rows
            .iter()
            .map(|&(at, strength, _)| (at, strength))
            .collect();
        places.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        // ... go to the rows in their source's order; rows it ranks alike
        // keep the merge's order.
        rows.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| b.1.total_cmp(&a.1))
                .then(a.0.cmp(&b.0))
        });
        for ((place, strength), (row, _, _)) in places.into_iter().zip(rows) {
            if let Some(slot) = from.get_mut(place) {
                moved |= slot.0 != row;
                *slot = (row, strength);
            }
        }
    }
    if !moved {
        return;
    }
    let mut taken: Vec<Option<SearchResult>> = merged.drain(..).map(|(row, _)| Some(row)).collect();
    // `from` is a permutation of the places: every row moves once.
    merged.extend(from.into_iter().filter_map(|(row, strength)| {
        taken
            .get_mut(row)
            .and_then(Option::take)
            .map(|row| (row, strength))
    }));
}

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
