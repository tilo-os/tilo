//! A mounted record kept current: what the workspace holds of it merged in,
//! only the changes after the heads it merged last.
use super::{DocumentRoute, Mounted, WorkspaceState, reads};

/// Merge into `mounted` what `state` holds of its record and its parts that
/// changed since it merged them last.
pub(super) fn catch_up(route: &DocumentRoute, mounted: &mut Mounted, state: &WorkspaceState) {
    match state.update(route, mounted.merged.as_deref()) {
        Ok(Some((mutation, heads))) => {
            // A change that did not merge leaves the replica where it was:
            // the next merge takes the record whole.
            mounted.merged = match mounted.app.apply(&mutation) {
                Ok(()) => Some(heads),
                Err(error) => {
                    tracing::warn!(%error, "workspace actions: a change did not merge");
                    None
                }
            };
        }
        Ok(None) => {}
        Err(error) => tracing::debug!(%error, "workspace actions: a record does not read"),
    }
    for (part, merged) in &mut mounted.parts {
        let Ok(record) = reads::record_id(&part.record) else {
            continue;
        };
        let Ok(Some((mutation, heads))) = state.update(part, merged.as_deref()) else {
            continue;
        };
        *merged = match mounted.app.apply_record(&record, &mutation) {
            Ok(()) => Some(heads),
            Err(error) => {
                tracing::warn!(%error, "workspace actions: a change to a part did not merge");
                None
            }
        };
    }
}

/// The rounds of [`hold_parts`]: each takes the parts a chain wants after
/// the last round handed it one, so a chain held whole takes all of its
/// parts in the second round; the bound only keeps a mistake from asking
/// forever.
const HOLD_ROUNDS: usize = 4;

/// Hand the record mounted at `holder` the parts of its chains that the
/// workspace holds and it does not: the newest part of each chain kept
/// beside a record of its app, as a page opening the record is sent the
/// chain's head, and every part the record then wants (a chain whose views
/// read all of it wants the parts before its head). A part it holds stays
/// current through [`catch_up`]. A part the workspace does not hold is not
/// waited for: the record starts its chain's next part itself.
///
/// Without this, a record mounted for a member, or for the host's own
/// person, held its chains empty: what was written into a part on another
/// device or in another replica was neither listed nor read, and a write
/// started a part beside the chain's newest.
pub(super) fn hold_parts(holder: &DocumentRoute, mounted: &mut Mounted, state: &WorkspaceState) {
    let mut newest: std::collections::HashMap<ui::record::DocumentId, (u64, &DocumentRoute)> =
        std::collections::HashMap::new();
    for route in &state.records {
        if route.app_id != holder.app_id || route.cluster_id != holder.cluster_id {
            continue;
        }
        let Some((chain, index)) =
            ui::record::DocumentId::parse(&route.record).and_then(|id| id.chain_part())
        else {
            continue;
        };
        let entry = newest.entry(chain).or_insert((index, route));
        if index > entry.0 {
            *entry = (index, route);
        }
    }
    if newest.is_empty() {
        return;
    }
    let mut asked: Vec<DocumentRoute> = newest
        .values()
        .map(|(_, route)| (*route).clone())
        .filter(|route| !mounted.parts.contains_key(route) && !mounted.foreign.contains(route))
        .collect();
    // A record holding none of its chains asks for nothing more.
    if asked.is_empty() && mounted.parts.is_empty() {
        return;
    }
    for _ in 0..HOLD_ROUNDS {
        for route in std::mem::take(&mut asked) {
            hold(&route, mounted, state);
        }
        asked = mounted
            .app
            .wanted_records()
            .into_iter()
            .filter_map(|record| {
                // The chain's head names whichever part is its newest.
                if record.is_chain_head() {
                    return newest
                        .iter()
                        .find(|(chain, _)| chain.chain_head() == record)
                        .map(|(_, (_, route))| (*route).clone());
                }
                let (chain, _) = record.chain_part()?;
                let route = DocumentRoute {
                    app_id: holder.app_id.clone(),
                    cluster_id: holder.cluster_id.clone(),
                    record: record.as_str().to_owned(),
                };
                (newest.contains_key(&chain) && state.records.contains(&route)).then_some(route)
            })
            .filter(|route| !mounted.parts.contains_key(route) && !mounted.foreign.contains(route))
            .collect();
        if asked.is_empty() {
            return;
        }
    }
}

/// Hand the part at `route` to `mounted` whole, as the workspace holds it;
/// a part of another record's chain is remembered as not this one's.
fn hold(route: &DocumentRoute, mounted: &mut Mounted, state: &WorkspaceState) {
    let Ok(record) = reads::record_id(&route.record) else {
        return;
    };
    let Ok((bytes, heads)) = state.document(route) else {
        return;
    };
    match mounted
        .app
        .apply_record(&record, &collab_types::DocumentMutation::Snapshot(bytes))
    {
        Ok(()) => {
            mounted.parts.insert(route.clone(), Some(heads));
        }
        Err(error) => {
            tracing::debug!(%error, part = %route.record, "workspace actions: a part is not held here");
            mounted.foreign.insert(route.clone());
        }
    }
}
