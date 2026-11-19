//! A mounted document kept current: what the workspace holds of it merged in,
//! only the changes after the heads it merged last.
use super::{
    DocumentRoute, MAX_MOUNTED, Mounted, Worker, WorkspaceId, WorkspaceState, Writer, is_part,
    reads,
};
use crate::apps::Seed;
use crate::worker::ConfinedReplica;
use collab_types::DocumentMutation;
use std::collections::{HashMap, HashSet};
use tilo_actions::MemberId;

/// Merge into `mounted` what `state` holds of its record and its parts that
/// changed since it merged them last.
///
/// The document and its parts are read only when a write landed in them
/// since the last refresh (`changed`), or when their last merge failed and
/// they are taken whole again; a replica mounted anew, or taken back from a
/// save, is passed `None` and takes all of them. A chain held whole is a part
/// per item it keeps, so reading every part each time made every request cost
/// what the document holds; a document nothing was written in is already what
/// the replica holds.
pub(super) fn catch_up(
    route: &DocumentRoute,
    mounted: &mut Mounted,
    state: &WorkspaceState,
    changed: Option<&HashSet<DocumentRoute>>,
) {
    let moved = mounted.merged.is_none() || changed.is_none_or(|changed| changed.contains(route));
    if moved {
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

/// The rounds of [`Worker::hand_wanted`]: how many documents a view's ask for further
/// documents may lead to within one action.
const WANTED_ROUNDS: usize = 4;

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
///
/// The parts the document wants that the session's replica does not hold yet
/// are answered, for the caller to ask the workspace for
/// ([`Worker::ask_parts`]): a workspace that sends a document only once it is
/// opened sent this service the head of each chain and nothing before it, so
/// a service that started again held only the newest part of a chain read
/// whole until someone asked for the others, which nobody did.
#[must_use = "the parts not here yet are asked for"]
pub(super) fn hold_parts(
    holder: &DocumentRoute,
    mounted: &mut Mounted,
    state: &WorkspaceState,
) -> Vec<DocumentRoute> {
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
        return Vec::new();
    }
    let asked: Vec<DocumentRoute> = newest
        .values()
        .map(|(_, route)| (*route).clone())
        .filter(|route| !mounted.parts.contains_key(route) && !mounted.foreign.contains(route))
        .collect();
    // A record holding none of its chains asks for nothing more.
    if asked.is_empty() && mounted.parts.is_empty() {
        return Vec::new();
    }
    // What it wants moves only with what it holds: nothing merged or handed
    // to it since it was asked last, it is not asked again.
    if asked.is_empty() && mounted.wanted_at == Some(holding(mounted)) {
        return Vec::new();
    }
    let missing = hold_wanted(holder, mounted, state, &newest, asked);
    // A part still on its way is handed once it arrives: the document is asked
    // again then, though it holds what it held now.
    mounted.wanted_at = missing.is_empty().then(|| holding(mounted));
    missing
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

/// The rounds of [`hold_parts`] from `asked`, the parts first handed. The
/// parts of its chains the document wants that the replica does not hold.
fn hold_wanted(
    holder: &DocumentRoute,
    mounted: &mut Mounted,
    state: &WorkspaceState,
    newest: &std::collections::HashMap<ui::resource::DocumentId, (u64, &DocumentRoute)>,
    mut asked: Vec<DocumentRoute>,
) -> Vec<DocumentRoute> {
    let mut missing = Vec::new();
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
                if !newest.contains_key(&chain) {
                    return None;
                }
                if !state.holds(&route) {
                    if !missing.contains(&route) {
                        missing.push(route);
                    }
                    return None;
                }
                Some(route)
            })
            .filter(|route| !mounted.parts.contains_key(route) && !mounted.foreign.contains(route))
            .collect();
        if asked.is_empty() {
            break;
        }
    }
    missing
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

/// The documents the view opened from its workspace that `holder` holds not yet, as routes:
/// everything its store asked its workspace for, other than the parts of its chains
/// ([`hold_parts`] hands those) and the document itself. A Files window opens the workspace's
/// folder and each chunk it lists this way; a window whose app keeps nothing beside its own asks
/// for nothing.
pub(super) fn wanted_untold(
    holder: &DocumentRoute,
    mounted: Option<&Mounted>,
) -> Vec<DocumentRoute> {
    let Some(mounted) = mounted else {
        return Vec::new();
    };
    let own = mounted.app.document_id();
    mounted
        .app
        .wanted_resources()
        .into_iter()
        .filter(|document| document != own)
        .filter(|document| document.chain_part().is_none() && !document.is_chain_head())
        .map(|document| DocumentRoute {
            app_id: holder.app_id.clone(),
            cluster_id: holder.cluster_id.clone(),
            document: document.as_str().to_owned(),
        })
        .filter(|route| !mounted.parts.contains_key(route) && !mounted.foreign.contains(route))
        .collect()
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

/// `mounted`, the record at `holder`, holds what it wrote at `wrote`: the
/// next merge into it starts from the heads it stands at, so its own change
/// is never read back from the workspace and merged into it again.
pub(super) fn caught_up_with_own(
    holder: &DocumentRoute,
    mounted: &mut Mounted,
    wrote: &[DocumentRoute],
) {
    for route in wrote {
        if route == holder {
            mounted.merged = mounted.app.heads_of(mounted.app.document_id());
        } else if let Ok(record) = reads::document_id(&route.document)
            && let Some(heads) = mounted.app.heads_of(&record)
        {
            mounted.parts.insert(route.clone(), Some(heads));
        }
    }
}

impl Worker {
    /// The record at `route`, mounted from what the workspace retains: a part of
    /// a chain held beside its app's own record, in that record's mount.
    pub(super) fn mount(
        &mut self,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<&mut Mounted, String> {
        let holder = self.mount_held(route, state)?;
        self.brought_up_to_date(&holder, state);
        self.mounted
            .get_mut(&holder)
            .ok_or_else(|| "the record did not mount".to_string())
    }

    /// [`Self::mount`] as it stands, what was written since it was last used
    /// not taken up: the route of the record holding `route`.
    pub(super) fn mount_held(
        &mut self,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<DocumentRoute, String> {
        if is_part(&route.document) {
            self.mount_part(route, state)
        } else {
            self.mount_own(route, state)?;
            Ok(route.clone())
        }
    }

    /// The record mounted at `holder` brought up to what the workspace holds:
    /// what was written since it was last used merged in, and the parts of
    /// its chains and what its app keeps for the workspace handed to it.
    pub(super) fn brought_up_to_date(&mut self, holder: &DocumentRoute, state: &WorkspaceState) {
        let kept = self.kept_held(kept_untold(holder, self.mounted.get(holder)));
        let Some(mounted) = self.mounted.get_mut(holder) else {
            return;
        };
        if let Some(behind) = mounted.behind.take() {
            catch_up(holder, mounted, state, Some(&behind));
        }
        let missing = hold_parts(holder, mounted, state);
        tell_kept(mounted, state, kept);
        // What the view opened from the workspace, after the chains are handed over: a part
        // of one is left to [`hold_parts`].
        self.hand_wanted(holder, state);
        self.ask_parts(holder, missing);
    }

    /// Hand `mounted` each document its view opens from the workspace and does not hold yet,
    /// until it asks for none: one it was handed opens a part of itself, or names the chunks of
    /// a tree, which it then asks for ([`wanted_untold`]). The rounds are bounded so a view
    /// that keeps asking for what the workspace holds none of does not hold the action.
    fn hand_wanted(&mut self, holder: &DocumentRoute, state: &WorkspaceState) {
        for _ in 0..WANTED_ROUNDS {
            let wanted = self.kept_held(wanted_untold(holder, self.mounted.get(holder)));
            if wanted.is_empty() {
                return;
            }
            let Some(mounted) = self.mounted.get_mut(holder) else {
                return;
            };
            tell_kept(mounted, state, wanted);
        }
    }

    /// Ask the workspace of `holder` for `parts`, parts of its chains the
    /// document held there wants and the session's replica does not hold: each
    /// arrives as a change, and the document is handed it the next time it is
    /// used ([`hold_parts`]).
    pub(super) fn ask_parts(&mut self, holder: &DocumentRoute, parts: Vec<DocumentRoute>) {
        if parts.is_empty() {
            return;
        }
        let Some(workspace_id) = WorkspaceId::parse(&holder.cluster_id) else {
            return;
        };
        if let Some(workspace) = self.workspaces.get_mut(&workspace_id) {
            workspace.ask_routes(workspace_id, parts);
        }
    }

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

impl Worker {
    /// The route of the mounted record `route`'s record is held in: its own,
    /// or for a part, the record it is held beside.
    pub(super) fn holder_of(&self, route: &DocumentRoute) -> DocumentRoute {
        self.beside.get(route).unwrap_or(route).clone()
    }

    /// Mount the part at `route` beside the record of its app that keeps it,
    /// mounting that record first. The holder's route.
    pub(super) fn mount_part(
        &mut self,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<DocumentRoute, String> {
        if let Some(holder) = self.beside.get(route).cloned()
            && self.mounted.contains_key(&holder)
        {
            self.mount_own(&holder, state)?;
            return Ok(holder);
        }
        let (bytes, heads) = state.document(route)?;
        let record = reads::document_id(&route.document)?;
        let holders: Vec<DocumentRoute> = state
            .documents
            .iter()
            .filter(|held| {
                held.app_id == route.app_id
                    && held.cluster_id == route.cluster_id
                    && !is_part(&held.document)
            })
            .cloned()
            .collect();
        let mut refusals = Vec::new();
        for holder in holders {
            if let Err(error) = self.mount_own(&holder, state) {
                refusals.push(error);
                continue;
            }
            let Some(mounted) = self.mounted.get_mut(&holder) else {
                continue;
            };
            match mounted
                .app
                .apply_document(&record, &DocumentMutation::Snapshot(bytes.clone()))
            {
                Ok(()) => {
                    mounted.parts.insert(route.clone(), Some(heads));
                    self.beside.insert(route.clone(), holder.clone());
                    return Ok(holder);
                }
                Err(error) => refusals.push(error.to_string()),
            }
        }
        Err(format!(
            "no record of {} keeps {} beside it: {}",
            route.app_id,
            route.document,
            refusals.join("; ")
        ))
    }

    /// The record at `route` itself, mounted from what the workspace retains.
    pub(super) fn mount_own(
        &mut self,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Result<(), String> {
        self.clock = self.clock.wrapping_add(1);
        let clock = self.clock;
        if !self.mounted.contains_key(route) {
            let (bytes, heads) = state.document(route)?;
            while self.mounted.len() >= MAX_MOUNTED {
                let Some(oldest) = self
                    .mounted
                    .iter()
                    .min_by_key(|(_, mounted)| mounted.used)
                    .map(|(route, _)| route.clone())
                else {
                    break;
                };
                self.mounted.remove(&oldest);
                self.beside.retain(|_, holder| *holder != oldest);
            }
            let app = ConfinedReplica::open(
                &self.catalogue,
                &route.app_id,
                &route.cluster_id,
                &MemberId::Person(self.member.clone()),
                &crate::ReplicaActor::Owner,
                Seed::Existing {
                    record: reads::document_id(&route.document)?,
                    changes: vec![bytes],
                },
                true,
            )
            .map_err(|error| error.to_string())?;
            let app = self.lend(app, &route.cluster_id);
            self.mounted.insert(
                route.clone(),
                Mounted {
                    app,
                    merged: Some(heads),
                    parts: HashMap::new(),
                    foreign: HashSet::new(),
                    wanted_at: None,
                    behind: None,
                    used: clock,
                },
            );
        }
        let mounted = self
            .mounted
            .get_mut(route)
            .ok_or_else(|| "the record did not mount".to_string())?;
        mounted.used = clock;
        Ok(())
    }

    /// Have every replica this service holds but `writer` take up `wrote` when it is next
    /// used: what one of them wrote is read by the others from the session's replica at
    /// once, and its landing later is not merged again, least of all into the replica that
    /// made it. Reading the document back after every write cost a sealed part's whole reading
    /// per action.
    pub(super) fn behind_all_but(&mut self, writer: &Writer, wrote: &[DocumentRoute]) {
        let mounted = self
            .mounted
            .iter_mut()
            .filter(|(holder, _)| *writer != Writer::Mounted((*holder).clone()))
            .map(|(_, mounted)| mounted);
        let bounded = self
            .bounded
            .iter_mut()
            .filter(|(key, _)| *writer != Writer::Bounded((*key).clone()))
            .map(|(_, replica)| &mut replica.held);
        for held in mounted.chain(bounded) {
            held.behind
                .get_or_insert_with(HashSet::new)
                .extend(wrote.iter().cloned());
        }
    }
}
