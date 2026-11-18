//! A mounted document kept current: what the workspace holds of it merged in,
//! only the changes after the heads it merged last.
use super::{DocumentRoute, Mounted, Worker, WorkspaceId, WorkspaceState, reads};
use std::collections::HashSet;

/// Merge into `mounted` what `state` holds of its record and its parts that
/// changed since it merged them last.
///
/// The document itself is read every time: the request acts on it, and one
/// read costs the same however much its app keeps. Its parts are read only
/// when a write landed in them since the last refresh (`changed`), or when
/// their last merge failed and they are taken whole again; a replica mounted
/// anew, or taken back from a save, is passed `None` and takes all of them. A
/// chain held whole is a part per item it keeps, so reading every part each
/// time made every request cost what the document holds; a part nothing was
/// written in is already what the replica holds.
pub(super) fn catch_up(
    route: &DocumentRoute,
    mounted: &mut Mounted,
    state: &WorkspaceState,
    changed: Option<&HashSet<DocumentRoute>>,
) {
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
        if merged.is_some() && changed.is_some_and(|changed| !changed.contains(part)) {
            continue;
        }
        let Ok(record) = reads::document_id(&part.document) else {
            continue;
        };
        let Ok(Some((mutation, heads))) = state.update(part, merged.as_deref()) else {
            continue;
        };
        *merged = match mounted.app.apply_document(&record, &mutation) {
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
    let mut newest: std::collections::HashMap<ui::resource::DocumentId, (u64, &DocumentRoute)> =
        std::collections::HashMap::new();
    for route in state.documents.iter() {
        if route.app_id != holder.app_id || route.cluster_id != holder.cluster_id {
            continue;
        }
        let Some((chain, index)) =
            ui::resource::DocumentId::parse(&route.document).and_then(|id| id.chain_part())
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
    let asked: Vec<DocumentRoute> = newest
        .values()
        .map(|(_, route)| (*route).clone())
        .filter(|route| !mounted.parts.contains_key(route) && !mounted.foreign.contains(route))
        .collect();
    // A record holding none of its chains asks for nothing more.
    if asked.is_empty() && mounted.parts.is_empty() {
        return;
    }
    // What it wants moves only with what it holds: nothing merged or handed
    // to it since it was asked last, it is not asked again.
    if asked.is_empty() && mounted.wanted_at == Some(holding(mounted)) {
        return;
    }
    hold_wanted(holder, mounted, state, &newest, asked);
    mounted.wanted_at = Some(holding(mounted));
}

/// What `mounted` and its parts merged last, as one number: where
/// [`hold_parts`] asked it which parts it wants.
fn holding(mounted: &Mounted) -> u64 {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    mounted.merged.hash(&mut hasher);
    let mut parts: Vec<_> = mounted.parts.iter().collect();
    parts.sort_by(|a, b| a.0.document.cmp(&b.0.document));
    for (route, merged) in parts {
        route.document.hash(&mut hasher);
        merged.hash(&mut hasher);
    }
    hasher.finish()
}

/// The rounds of [`hold_parts`] from `asked`, the parts first handed.
fn hold_wanted(
    holder: &DocumentRoute,
    mounted: &mut Mounted,
    state: &WorkspaceState,
    newest: &std::collections::HashMap<ui::resource::DocumentId, (u64, &DocumentRoute)>,
    mut asked: Vec<DocumentRoute>,
) {
    for _ in 0..HOLD_ROUNDS {
        for route in std::mem::take(&mut asked) {
            hold(&route, mounted, state);
        }
        asked = mounted
            .app
            .wanted_resources()
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
                    document: record.as_str().to_owned(),
                };
                (newest.contains_key(&chain) && state.documents.iter().any(|held| held == &route))
                    .then_some(route)
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
    let Ok(record) = reads::document_id(&route.document) else {
        return;
    };
    let Ok((bytes, heads)) = state.document(route) else {
        return;
    };
    match mounted
        .app
        .apply_document(&record, &collab_types::DocumentMutation::Snapshot(bytes))
    {
        Ok(()) => {
            mounted.parts.insert(route.clone(), Some(heads));
        }
        Err(error) => {
            tracing::debug!(%error, part = %route.document, "workspace actions: a part is not held here");
            mounted.foreign.insert(route.clone());
        }
    }
}

/// The routes of what `holder`'s app keeps for its whole workspace beside its own document
/// ([`collab_types::WORKSPACE_DOCUMENTS`]: Chat's of the workspace's agents and emoji) that
/// `mounted` was told nothing of yet. Empty for every app that keeps none, which is nearly
/// all of them, and for a holder already told.
pub(super) fn kept_untold(holder: &DocumentRoute, mounted: Option<&Mounted>) -> Vec<DocumentRoute> {
    collab_types::WORKSPACE_DOCUMENTS
        .iter()
        .filter(|kept| kept.app == holder.app_id)
        .map(|kept| DocumentRoute {
            app_id: holder.app_id.clone(),
            cluster_id: holder.cluster_id.clone(),
            document: collab_types::workspace_document_id(kept.kind),
        })
        .filter(|route| {
            mounted.is_none_or(|mounted| {
                !mounted.parts.contains_key(route) && !mounted.foreign.contains(route)
            })
        })
        .collect()
}

/// Tell `mounted` of each of `kept`, with whether the workspace holds it: one it holds is
/// handed over whole, as a chain's part is, and kept current from then on; of one it holds
/// none of, the app is told so, and founds it when something is first written into it.
/// Until one of the two the app opens none of them, so nothing is founded beside a
/// document the workspace already has. What the workspace comes to hold later of one it
/// held none of is merged whole by [`catch_up`].
///
/// Without this a document mounted with no window open held nothing an app keeps for its
/// whole workspace: a caller reaching the workspace through this service could neither
/// read nor change the workspace's agents, though the app declares both.
pub(super) fn tell_kept(
    mounted: &mut Mounted,
    state: &WorkspaceState,
    kept: Vec<(DocumentRoute, bool)>,
) {
    for (route, held) in kept {
        if held {
            hold(&route, mounted, state);
            continue;
        }
        let Ok(id) = reads::document_id(&route.document) else {
            continue;
        };
        match mounted
            .app
            .workspace_answered(&id, ui::resource::WorkspaceAnswer::HoldsNone)
        {
            Ok(()) => {
                mounted.parts.insert(route, None);
            }
            Err(error) => {
                tracing::debug!(%error, document = %route.document, "workspace actions: an app was not told the workspace holds none of what it keeps for it");
            }
        }
    }
}

impl Worker {
    /// Whether the workspace holds each of `routes`, as far as it answered. One still on
    /// its way, or in a workspace this service has not joined, is left out: its holder is
    /// told of it the next time it is used.
    pub(super) fn kept_held(&mut self, routes: Vec<DocumentRoute>) -> Vec<(DocumentRoute, bool)> {
        routes
            .into_iter()
            .filter_map(|route| {
                let workspace = WorkspaceId::parse(&route.cluster_id)?;
                self.held_or_owed(workspace, &route)
                    .ok()
                    .map(|held| (route, held))
            })
            .collect()
    }
}

/// The most parts of its app's chains [`hand_all_parts`] hands at once.
const HOLD_PARTS: usize = 32;

/// Hand `mounted` every part of its app's chains the workspace holds, up to [`HOLD_PARTS`] —
/// what an action that named an item in a part its document was not handed (an issue read by its
/// id, in an older part than the head) is asked again with.
pub(super) fn hand_all_parts(
    holder: &DocumentRoute,
    mounted: &mut Mounted,
    state: &WorkspaceState,
) {
    let mut handed = 0;
    for route in state.documents.iter() {
        if handed >= HOLD_PARTS
            || route.app_id != holder.app_id
            || route.cluster_id != holder.cluster_id
        {
            continue;
        }
        let is_part = ui::resource::DocumentId::parse(&route.document)
            .is_some_and(|document| document.chain_part().is_some());
        if !is_part || mounted.parts.contains_key(route) || mounted.foreign.contains(route) {
            continue;
        }
        hold(route, mounted, state);
        handed = handed.saturating_add(1);
    }
}
