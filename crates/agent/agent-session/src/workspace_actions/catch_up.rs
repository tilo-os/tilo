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
use std::sync::Arc;
use std::time::{Duration, Instant};
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
pub(super) const HOLD_ROUNDS: usize = 4;

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
/// ([`Worker::hold_chains`]): a workspace that sends a document only once it is
/// opened sent this service the head of each chain and nothing before it, so
/// a service that started again held only the newest part of a chain read
/// whole until someone asked for the others, which nobody did.
#[must_use = "the parts not here yet are asked for"]
pub(super) fn hold_parts(
    holder: &DocumentRoute,
    mounted: &mut Mounted,
    state: &WorkspaceState,
) -> Vec<DocumentRoute> {
    hold_parts_told(holder, mounted, state, &|| {})
}

/// [`hold_parts`], calling `told` after each part it hands.
#[must_use = "the parts not here yet are asked for"]
pub(super) fn hold_parts_told(
    holder: &DocumentRoute,
    mounted: &mut Mounted,
    state: &WorkspaceState,
    told: &dyn Fn(),
) -> Vec<DocumentRoute> {
    let newest = newest_parts(holder, state);
    answer_unheld(holder, mounted, state, &newest);
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
    let missing = hold_wanted(holder, mounted, state, (&newest, asked), told);
    // A part still on its way is handed once it arrives: the document is asked
    // again then, though it holds what it held now.
    mounted.wanted_at = missing.is_empty().then(|| holding(mounted));
    missing
}

/// The newest part the workspace holds of each chain kept beside `holder`, by chain.
fn newest_parts<'s>(
    holder: &DocumentRoute,
    state: &'s WorkspaceState,
) -> std::collections::HashMap<ui::resource::DocumentId, (u64, &'s DocumentRoute)> {
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
    newest
}

/// [`answer_unheld`] for what `mounted` asks for now: a write refused because a part it
/// changes has not arrived names that part as it is refused, and is asked again once the
/// app was told the workspace holds none of it.
pub(super) fn answer_unheld_parts(
    holder: &DocumentRoute,
    mounted: &Mounted,
    state: &WorkspaceState,
) {
    answer_unheld(holder, mounted, state, &newest_parts(holder, state));
}

/// Tell `mounted` the workspace holds none of each part of its chains it asks for past the
/// newest the workspace lists, and no part at all of a chain whose head it asks for and of which
/// `newest` names none. Its store asks the workspace for what it does not hold, so a part
/// nobody holds — the first of a new chain, or the next one a full chain starts — stayed on
/// its way, and the write that started it waited for it until it gave up.
fn answer_unheld(
    holder: &DocumentRoute,
    mounted: &Mounted,
    state: &WorkspaceState,
    newest: &std::collections::HashMap<ui::resource::DocumentId, (u64, &DocumentRoute)>,
) {
    for wanted in mounted.app.wanted_resources() {
        let unheld = if wanted.is_chain_head() {
            !newest.keys().any(|chain| chain.chain_head() == wanted)
        } else if let Some((chain, index)) = wanted.chain_part() {
            // A part before the newest the workspace lists is the workspace's, asked for
            // where this service does not hold it yet ([`hold_wanted`]); one past it is the
            // part a write starts.
            let route = DocumentRoute {
                app_id: holder.app_id.clone(),
                cluster_id: holder.cluster_id.clone(),
                document: wanted.as_str().to_owned(),
            };
            !state.holds(&route) && newest.get(&chain).is_none_or(|(at, _)| index > *at)
        } else {
            false
        };
        if !unheld {
            continue;
        }
        if let Err(error) = mounted
            .app
            .workspace_answered(&wanted, ui::resource::WorkspaceAnswer::HoldsNone)
        {
            tracing::debug!(%error, wanted = %wanted.as_str(), "workspace actions: an app was not told the workspace holds none of a part");
        }
    }
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

/// The rounds of [`hold_parts`] from `asked`, the parts first handed, calling `told` after
/// each. The parts of its chains the document wants that the replica does not hold.
///
/// Each round hands the newest parts first, a few read at a time: a chain's listing shows its
/// parts newest first, so the parts a first page shows are in the app's worker before those
/// it reaches only further on, rather than after every part of the chain was read.
fn hold_wanted(
    holder: &DocumentRoute,
    mounted: &mut Mounted,
    state: &WorkspaceState,
    (newest, mut asked): (
        &std::collections::HashMap<ui::resource::DocumentId, (u64, &DocumentRoute)>,
        Vec<DocumentRoute>,
    ),
    told: &dyn Fn(),
) -> Vec<DocumentRoute> {
    let mut missing = Vec::new();
    for _ in 0..HOLD_ROUNDS {
        newest_first(&mut asked);
        // One part read first, then twice as many each time, up to as many as are read at
        // once: the newest is in the worker after one read, not after the slowest of a batch.
        let mut rest = std::mem::take(&mut asked);
        let mut some = 1;
        while !rest.is_empty() {
            let after = rest.split_off(some.min(rest.len()));
            for (route, saved) in read_parts(std::mem::replace(&mut rest, after), state) {
                hold(route, saved, mounted);
                told();
            }
            some = some.saturating_mul(2).min(PART_READERS);
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
/// Each of `routes` as the workspace holds it, read side by side: a part opened first
/// after the service started is read from its sealed writes, which costs about a second
/// for a part of a few hundred issues, and the parts of one chain do not wait on each other.
pub(super) fn read_parts(
    routes: Vec<DocumentRoute>,
    state: &WorkspaceState,
) -> Vec<(DocumentRoute, Result<super::Saved, String>)> {
    let read = |route: DocumentRoute| {
        let saved = state.document(&route);
        (route, saved)
    };
    if routes.len() < 2 {
        return routes.into_iter().map(read).collect();
    }
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, PART_READERS);
    let per_thread = routes.len().div_ceil(threads);
    let mut routes = routes.into_iter();
    let chunks: Vec<Vec<DocumentRoute>> = std::iter::from_fn(|| {
        let chunk: Vec<_> = routes.by_ref().take(per_thread).collect();
        (!chunk.is_empty()).then_some(chunk)
    })
    .collect();
    std::thread::scope(|scope| {
        #[expect(
            clippy::needless_collect,
            reason = "every reader starts before the first is joined"
        )]
        let readers: Vec<_> = chunks
            .into_iter()
            .map(|chunk| scope.spawn(move || chunk.into_iter().map(read).collect::<Vec<_>>()))
            .collect();
        readers
            .into_iter()
            .flat_map(|reader| reader.join().unwrap_or_default())
            .collect()
    })
}

/// How many parts [`read_parts`] reads at once at most.
const PART_READERS: usize = 8;

/// `routes` in the order a chain's listing shows them: the parts of each chain newest first.
fn newest_first(routes: &mut [DocumentRoute]) {
    routes.sort_by_cached_key(|route| {
        let part = ui::resource::DocumentId::parse(&route.document).and_then(|id| id.chain_part());
        let (chain, index) = part.map_or((String::new(), 0), |(chain, index)| {
            (chain.as_str().to_owned(), index)
        });
        (chain, std::cmp::Reverse(index))
    });
}

pub(super) fn hold(
    route: DocumentRoute,
    saved: Result<super::Saved, String>,
    mounted: &mut Mounted,
) {
    let Ok(record) = reads::document_id(&route.document) else {
        return;
    };
    let Ok((bytes, heads)) = saved else {
        return;
    };
    match mounted
        .app
        .apply_document(&record, &collab_types::DocumentMutation::Snapshot(bytes))
    {
        Ok(()) => {
            mounted.parts.insert(route, Some(heads));
        }
        Err(error) => {
            tracing::debug!(%error, part = %route.document, "workspace actions: a part is not held here");
            mounted.foreign.insert(route);
        }
    }
}

/// The documents the view opened from its workspace that `holder` holds not yet, as routes:
/// everything its store asked its workspace for, other than the document itself and the parts
/// of its chains this replica holds ([`hold_parts`] hands those). A Files window opens the
/// workspace's folder and each chunk it lists this way; a chain this replica holds no part of
/// is named by its head, and a part it holds none of by the part; a window whose app keeps
/// nothing beside its own asks for nothing.
pub(super) fn wanted_untold(
    holder: &DocumentRoute,
    mounted: Option<&Mounted>,
    state: &WorkspaceState,
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
        // What the app keeps for its whole workspace is wanted until it is told of it, which
        // [`kept_untold`] does.
        .filter(|document| {
            collab_types::workspace_document(&holder.app_id, document.as_str()).is_none()
        })
        .map(|document| DocumentRoute {
            app_id: holder.app_id.clone(),
            cluster_id: holder.cluster_id.clone(),
            document: document.as_str().to_owned(),
        })
        // A chain's head is asked for by that name when this replica holds no
        // part of the chain: nothing else tells this device which part is the
        // newest, and the workspace's answer that it holds none of the chain
        // is what lets the document start its first part.
        .filter(|route| !is_head(route) || !holds_a_part_of(holder, state, &route.document))
        // A part this replica holds is handed to the document by [`hold_parts`];
        // one it holds none of is asked for, and its answer is what tells the
        // document the part is new here.
        .filter(|route| !is_part(&route.document) || !state.holds(route))
        .filter(|route| !mounted.parts.contains_key(route) && !mounted.foreign.contains(route))
        .collect()
}

/// Whether `route` names a chain's head, which holds no document of its own.
fn is_head(route: &DocumentRoute) -> bool {
    ui::resource::DocumentId::parse(&route.document)
        .is_some_and(|document| document.is_chain_head())
}

/// Whether this member's replica holds a part of the chain whose head is
/// `head`: what [`hold_parts`] hands the document as the chain's newest part,
/// so its head is not asked for by name.
fn holds_a_part_of(holder: &DocumentRoute, state: &WorkspaceState, head: &str) -> bool {
    state.documents.iter().any(|route| {
        route.app_id == holder.app_id
            && route.cluster_id == holder.cluster_id
            && ui::resource::DocumentId::parse(&route.document)
                .is_some_and(|part| part.chain_head().as_str() == head)
    })
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
            let saved = state.document(&route);
            hold(route, saved, mounted);
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
        let Some(mounted) = self.mounted.get_mut(holder) else {
            return;
        };
        if let Some(behind) = mounted.behind.take() {
            catch_up(holder, mounted, state, Some(&behind));
        }
        // The parts of its chains, those the workspace still has to send among them, and
        // what its app keeps for the workspace.
        self.hold_chains(&Writer::Mounted(holder.clone()), holder, state);
        // What the wait above left untold, as the workspace answers for it now.
        let kept = self.kept_held(kept_untold(holder, self.mounted.get(holder)));
        let Some(mounted) = self.mounted.get_mut(holder) else {
            return;
        };
        tell_kept(mounted, state, kept);
        // What the view opened from the workspace, after the chains are handed over: a part
        // of one is left to [`hold_parts`].
        self.hand_wanted(holder, state);
    }

    /// Hand `mounted` each document its view opens from the workspace and does not hold yet,
    /// until it asks for none: one it was handed opens a part of itself, or names the chunks of
    /// a tree, which it then asks for ([`wanted_untold`]). The rounds are bounded so a view
    /// that keeps asking for what the workspace holds none of does not hold the action.
    fn hand_wanted(&mut self, holder: &DocumentRoute, state: &WorkspaceState) {
        for _ in 0..WANTED_ROUNDS {
            let wanted = self.kept_held(wanted_untold(holder, self.mounted.get(holder), state));
            if wanted.is_empty() {
                return;
            }
            let Some(mounted) = self.mounted.get_mut(holder) else {
                return;
            };
            tell_kept(mounted, state, wanted);
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

/// How long a request waits for the parts of a chain its view reads that the workspace has
/// still to send ([`Worker::hold_chains`]): inside the wait the API's own caller makes.
const CHAIN_WAIT: Duration = Duration::from_secs(20);
/// The first pause between looking whether those parts arrived: short, as a part asked for
/// usually arrives at once.
const CHAIN_LOOK: Duration = Duration::from_millis(5);
/// The longest pause between those looks.
const CHAIN_LOOK_MAX: Duration = Duration::from_millis(100);

/// The documents of `mounted`'s chains its view wants and was not handed, as routes: the parts
/// the workspace lists, which are handed at once, and those it lists none of here, which are
/// asked of it ([`Worker::hold_chains`]): a part before the oldest it was sent, and the head
/// of a chain the workspace lists no part of.
/// The heads of the chains of `holder`'s app that the workspace lists a part of.
fn chains_listed(
    holder: &DocumentRoute,
    state: &WorkspaceState,
) -> HashSet<ui::resource::DocumentId> {
    state
        .documents
        .iter()
        .filter(|held| held.app_id == holder.app_id && held.cluster_id == holder.cluster_id)
        .filter_map(|held| ui::resource::DocumentId::parse(&held.document))
        .filter_map(|id| id.chain_part().map(|(chain, _)| chain.chain_head()))
        .collect()
}

/// A head held while its chain had no part listed gives way to the parts once one is:
/// [`hold_parts`] hands those, and a head kept beside them would be read again on every
/// write to the chain.
fn release_heads(holder: &DocumentRoute, mounted: &mut Mounted, state: &WorkspaceState) {
    if !mounted.parts.keys().any(|route| {
        ui::resource::DocumentId::parse(&route.document).is_some_and(|id| id.is_chain_head())
    }) {
        return;
    }
    let chains = chains_listed(holder, state);
    mounted.parts.retain(|route, _| {
        ui::resource::DocumentId::parse(&route.document)
            .is_none_or(|id| !id.is_chain_head() || !chains.contains(&id))
    });
}

pub(super) fn chains_untold(
    holder: &DocumentRoute,
    mounted: &Mounted,
    state: &WorkspaceState,
) -> (Vec<DocumentRoute>, Vec<DocumentRoute>) {
    let wanted = mounted.app.wanted_resources();
    if wanted.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let route = |document: &ui::resource::DocumentId| DocumentRoute {
        app_id: holder.app_id.clone(),
        cluster_id: holder.cluster_id.clone(),
        document: document.as_str().to_owned(),
    };
    let listed: HashSet<&DocumentRoute> = state.documents.iter().collect();
    let mut chains: Option<HashSet<ui::resource::DocumentId>> = None;
    let mut listed_untold = Vec::new();
    let mut untold = Vec::new();
    for wanted_id in wanted {
        let asked = if wanted_id.is_chain_head() {
            // A chain the workspace lists a part of is handed its newest by [`hold_parts`].
            let chains = chains.get_or_insert_with(|| chains_listed(holder, state));
            if chains.contains(&wanted_id) {
                continue;
            }
            route(&wanted_id)
        } else if wanted_id.chain_part().is_some() {
            route(&wanted_id)
        } else {
            continue;
        };
        if mounted.parts.contains_key(&asked) || mounted.foreign.contains(&asked) {
            continue;
        }
        if listed.contains(&asked) {
            listed_untold.push(asked);
        } else {
            untold.push(asked);
        }
    }
    (listed_untold, untold)
}

impl Worker {
    /// The replica `at` holds, mutably.
    fn held_mut(&mut self, at: &Writer) -> Option<&mut Mounted> {
        match at {
            Writer::Mounted(holder) => self.mounted.get_mut(holder),
            Writer::Bounded(key) => self.bounded.get_mut(key).map(|replica| &mut replica.held),
        }
    }

    /// Hand the replica `at`, the document at `holder`, the parts of its chains: those the
    /// workspace lists ([`hold_parts`]), and those its view reads that the workspace has
    /// not sent here, which it is asked for and waited on, bounded ([`CHAIN_WAIT`]), and so
    /// are the documents its app keeps for the workspace ([`kept_untold`]). A chain the
    /// workspace holds none of is told so, and starts its first part.
    ///
    /// A service that restarted holds only the parts it was sent since: without asking for
    /// the others, a tracker of thousands of issues listed the newest part's few dozen and
    /// said that was all, and a chain it never held waited for an answer nobody asked for.
    pub(super) fn hold_chains(
        &mut self,
        at: &Writer,
        holder: &DocumentRoute,
        state: &WorkspaceState,
    ) {
        // Parts taken in off this thread are handed there ([`super::part_load`]), not again here.
        if self
            .held_mut(at)
            .map(|held| Arc::clone(&held.app))
            .is_some_and(|app| self.part_loads.running(&app))
        {
            return;
        }
        let until = Instant::now().checked_add(CHAIN_WAIT);
        let mut look = CHAIN_LOOK;
        let mut fresh: Option<WorkspaceState> = None;
        loop {
            let now = fresh.as_ref().unwrap_or(state);
            let Some(mounted) = self.held_mut(at) else {
                return;
            };
            let missing = hold_parts(holder, mounted, now);
            release_heads(holder, mounted, now);
            // A part that arrived since its chain last asked is handed here: [`hold_parts`]
            // asks a chain what it wants only once what it holds moved.
            let mut untold = Vec::new();
            for _ in 0..HOLD_ROUNDS {
                let (listed, asked) = chains_untold(holder, mounted, now);
                untold = asked;
                if listed.is_empty() {
                    break;
                }
                for (route, saved) in read_parts(listed, now) {
                    hold(route, saved, mounted);
                }
            }
            // What the app keeps for the whole workspace (Chat's skills, emoji and moderation)
            // is waited for like a part: a record answered before the workspace said whether
            // it holds them offered none of their actions, and its first caller was told so.
            for route in missing
                .into_iter()
                .chain(kept_untold(holder, Some(&*mounted)))
            {
                if !untold.contains(&route) {
                    untold.push(route);
                }
            }
            if untold.is_empty() {
                return;
            }
            let (answered, owed) = self.chains_answered(untold);
            let Some(mounted) = self.held_mut(at) else {
                return;
            };
            let told = answered.len();
            tell_kept(mounted, now, answered);
            // Nothing owed: what was answered is handed above, from the workspace as it
            // stands once it was sent.
            if !owed && (told == 0 || fresh.is_some()) {
                return;
            }
            if until.is_none_or(|until| Instant::now() >= until) {
                tracing::warn!(app = %holder.app_id, document = %holder.document,
                    "workspace actions: parts of a chain did not arrive in time");
                return;
            }
            if owed {
                // A write or a listing waits off the thread: every other request of the
                // workspace waited behind one parked here for as long as the parts took.
                match self.part_waits.chain_wait(CHAIN_WAIT) {
                    super::part_wait::ChainWait::Here => {}
                    super::part_wait::ChainWait::Held => return,
                    super::part_wait::ChainWait::GaveUp => {
                        tracing::warn!(app = %holder.app_id, document = %holder.document,
                            "workspace actions: parts of a chain did not arrive in time");
                        return;
                    }
                }
                std::thread::park_timeout(look);
                look = look.saturating_mul(2).min(CHAIN_LOOK_MAX);
            }
            let Some(workspace_id) = WorkspaceId::parse(&holder.cluster_id) else {
                return;
            };
            let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
                return;
            };
            workspace.pump(workspace_id);
            fresh = Some(workspace.state());
        }
    }

    /// Whether the workspace holds each of `routes`, as far as it answered, and whether an
    /// answer is still owed for any: one owed is asked for ([`Self::held_or_owed`]).
    fn chains_answered(
        &mut self,
        routes: Vec<DocumentRoute>,
    ) -> (Vec<(DocumentRoute, bool)>, bool) {
        let mut owed = false;
        let mut answered = Vec::new();
        for route in routes {
            let Some(workspace) = WorkspaceId::parse(&route.cluster_id) else {
                continue;
            };
            match self.held_or_owed(workspace, &route) {
                Ok(held) => answered.push((route, held)),
                Err(super::Outcome::Rejected(tilo_actions::remote::ActionCallError::NotReady)) => {
                    owed = true;
                }
                Err(_) => {}
            }
        }
        (answered, owed)
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
        hold(route.clone(), state.document(route), mounted);
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
                    app: std::sync::Arc::new(app),
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
