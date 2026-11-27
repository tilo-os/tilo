//! Exact installed document routing. No transport, signing or action log lives here.
use super::HistoryOwner;
use automerge::{AutoCommit, ChangeHash, transaction::Transactable};
use collab_types::DocumentMutation;
use std::collections::{HashSet, VecDeque};
use tilo_automerge::ApplyIncoming;
use tilo_ui_base::collaboration::Incoming;
use tilo_ui_base::{collaboration::AcceptedRoute, collaboration::RouteSpec, widget::Widget};

const MAX_PENDING_ITEMS: usize = 64;
/// A single remote change can be as large as the transport carries, so a deferred queue must hold at
/// least one of them.
const MAX_PENDING_BYTES: usize = collab_types::MAX_DOCUMENT_TRANSFER_BYTES;

/// Only invalid mutation data may be quarantined.
#[derive(Debug, thiserror::Error)]
pub enum RemoteApplyError {
    #[error("{0}")]
    InvalidMutation(String),
    #[error("{0}")]
    Unavailable(String),
}
impl RemoteApplyError {
    pub fn is_invalid_mutation(&self) -> bool {
        matches!(self, Self::InvalidMutation(_))
    }
}

/// What merging a peer's write into an owned document did.
pub struct MergedRemote {
    /// Whether the document changed; `None` while it cannot take the write yet (an unfinished edit, a
    /// restore in flight).
    pub changed: Option<bool>,
    /// The changes the merge brought in. The route delivered them, so they are never published back on
    /// it.
    pub observed: Vec<ChangeHash>,
}

impl MergedRemote {
    /// A merge the document cannot take yet: an unfinished edit, a restore in flight.
    pub(crate) fn deferred() -> Self {
        Self {
            changed: None,
            observed: Vec::new(),
        }
    }
}

/// `mutation`, as a peer's route delivered it, handed to `apply`: its whole
/// document loaded here.
pub fn with_mutation<T>(
    mutation: &DocumentMutation,
    apply: impl FnOnce(Incoming<'_>) -> Result<T, RemoteApplyError>,
) -> Result<T, RemoteApplyError> {
    let _phase = tilo_actions::profiling::Phase::start("committed.load");
    Incoming::of(mutation, apply)
        .map_err(|error| RemoteApplyError::InvalidMutation(error.to_string()))?
}

/// Apply what the host has already committed to `candidate`: changes whole or not at all, a whole
/// document by merging only what it adds. Bytes from peers reach an app only after the host admitted
/// them in its sandboxed parser, so this applies in the app's own process.
pub(crate) fn merge_committed(
    candidate: &mut AutoCommit,
    incoming: Incoming<'_>,
) -> Result<bool, RemoteApplyError> {
    let _phase = tilo_actions::profiling::Phase::start("committed.merge");
    ready(candidate)?;
    let before = super::sorted_heads(candidate.base_heads());
    match incoming {
        Incoming::Whole(loaded) => {
            candidate
                .take_loaded(loaded)
                .map_err(|error| RemoteApplyError::InvalidMutation(error.to_string()))?;
        }
        Incoming::Changes(bytes) => load_incremental(candidate, bytes)?,
    }
    Ok(before != super::sorted_heads(candidate.base_heads()))
}

fn ready(doc: &AutoCommit) -> Result<(), RemoteApplyError> {
    if doc.pending_ops() == 0 {
        Ok(())
    } else {
        Err(RemoteApplyError::Unavailable(
            tilo_ui_base::i18n::I18nManager::lookup_plain("history-sync-finish-edit"),
        ))
    }
}

/// Apply an incremental stream to `doc`, whole or not at all.
pub(crate) fn load_incremental(doc: &mut AutoCommit, bytes: &[u8]) -> Result<(), RemoteApplyError> {
    let _phase = tilo_actions::profiling::Phase::start("committed.changes");
    let changes = automerge::Automerge::parse_changes(bytes)
        .map_err(|error| RemoteApplyError::InvalidMutation(error.to_string()))?;
    doc.apply_incoming(changes)
        .map_err(|error| RemoteApplyError::InvalidMutation(error.to_string()))
}
fn mutation_bytes(mutation: &DocumentMutation) -> usize {
    match mutation {
        DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => bytes.len(),
    }
}
#[derive(Default)]
pub struct PendingRemote {
    queue: VecDeque<DocumentMutation>,
    bytes: usize,
    pub failure: Option<String>,
    invalid_mutation_pending: bool,
    recovery_snapshot_pending: bool,
}
impl PendingRemote {
    pub fn admit(&mut self, mutation: DocumentMutation) -> bool {
        // Retaining visible failure during deferred recovery must not discard
        // traffic received after S. Queue it in order behind the admitted S.
        if self.failure.is_some() && !self.recovery_snapshot_pending {
            let accepted = self.recover_snapshot(mutation);
            if !accepted {
                // An older snapshot already behind a quarantined item also
                // cannot cover traffic refused after that snapshot arrived.
                self.invalid_mutation_pending = false;
            }
            accepted
        } else {
            self.push(mutation)
        }
    }
    pub(crate) fn push(&mut self, mutation: DocumentMutation) -> bool {
        let size = mutation_bytes(&mutation);
        if self.failure.is_some() && !self.recovery_snapshot_pending {
            return false;
        }
        if self.queue.len() >= MAX_PENDING_ITEMS
            || size > MAX_PENDING_BYTES.saturating_sub(self.bytes)
        {
            // A refused later update is absent from every older queued snapshot.
            // Neither the original recovery S nor an older ordinary snapshot
            // may clear this new loss obligation when the queue drains.
            self.invalid_mutation_pending = false;
            self.recovery_snapshot_pending = false;
            self.failure = Some(tilo_ui_base::i18n::I18nManager::lookup_plain(
                "history-sync-queue-full",
            ));
            return false;
        }
        self.bytes = self.bytes.saturating_add(size);
        self.queue.push_back(mutation);
        true
    }
    pub(crate) fn recover_snapshot(&mut self, mutation: DocumentMutation) -> bool {
        let DocumentMutation::Snapshot(bytes) = &mutation else {
            return false;
        };
        if bytes.len() > MAX_PENDING_BYTES
            || !self.queue.is_empty()
            || AutoCommit::load(bytes).is_err()
        {
            return false;
        }
        // Retain the failure and publication stop until the valid snapshot is
        // actually merged; an unfinished local edit can defer that boundary.
        self.bytes = bytes.len();
        self.queue.push_back(mutation);
        self.recovery_snapshot_pending = true;
        true
    }
    pub fn quarantine_invalid_front(&mut self, reason: String) {
        self.pop();
        // A malformed item discovered while an overflow is outstanding must
        // not weaken its requirement for a newly received recovery snapshot.
        if self.failure.is_none() || self.invalid_mutation_pending || self.recovery_snapshot_pending
        {
            self.failure = Some(reason);
            self.invalid_mutation_pending = true;
        }
        self.recovery_snapshot_pending = false;
    }
    pub fn pop_applied(&mut self) {
        // A snapshot already admitted behind bad bytes can also recover once
        // actually merged. Never clear overflow merely by draining an old item.
        if (self.invalid_mutation_pending || self.recovery_snapshot_pending)
            && matches!(self.front(), Some(DocumentMutation::Snapshot(_)))
        {
            self.failure = None;
            self.invalid_mutation_pending = false;
            self.recovery_snapshot_pending = false;
        }
        self.pop();
    }
    pub fn front(&self) -> Option<&DocumentMutation> {
        self.queue.front()
    }
    pub(crate) fn pop(&mut self) {
        if let Some(mutation) = self.queue.pop_front() {
            self.bytes = self.bytes.saturating_sub(mutation_bytes(&mutation));
        }
    }
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}
/// A route an owner holds that the transport is not yet joined to, and the kind its record holds.
pub struct RouteJoin {
    pub route: RouteSpec,
    pub resource_type: crate::resource::ResourceType,
}

pub struct OwnerRoute {
    pub route: RouteSpec,
    pub owners: Vec<HistoryOwner>,
    /// Whether this app publishes what changes in the record.
    pub publishes: bool,
    /// Heads the transport holds from this app: the changes it accepted.
    pub sent: crate::state::DocumentFrontier,
    pub needs_snapshot: bool,
    pub pending: PendingRemote,
    /// Why local changes on this route are not reaching peers.
    pub outbound_failure: Option<String>,
    /// A record kept beside the shown document
    /// ([`crate::bound_dispatch::BoundWidget::resource_owners`]): no host replays its route, so a
    /// snapshot that leaves this replica ahead of its sender is answered with this replica's own.
    pub answers_snapshots: bool,
    /// Joined, and not yet open for documents: its first publication waits for the route instead of
    /// failing.
    pub opening: bool,
}
struct TreeRecord {
    store: crate::resource::ResourceStore,
    checkout: crate::resource::DocumentCheckout,
    lifetime: std::rc::Rc<super::owner::ResourceOwnerLifetime>,
}

#[derive(Default)]
pub struct OwnerDocumentRoutes {
    pub observation: Option<AcceptedRoute>,
    pub owners: Vec<OwnerRoute>,
    pub primary: PendingRemote,
    pub(crate) primary_generation: u64,
    pub status: Option<String>,
    /// Whether the primary route was given the window's document whole since it opened on this observation:
    /// a route's first publication is the document whole, and only the changes made after it go out one
    /// by one ([`Self::observe_observation`] starts a new route over).
    pub primary_whole: bool,
    /// Why local changes on the primary or workspace-index route are not reaching peers.
    pub outbound: Option<String>,
    retired: HashSet<RouteSpec>,
    /// The records held as part of a live owner's tree ([`DocumentId::tree`]) that no view owns: each
    /// keeps its route while its store has it open.
    tree: Vec<TreeRecord>,
    #[cfg(any(test, feature = "test-support"))]
    pub sent: Vec<(RouteSpec, tilo_ui_base::collaboration::CollabRequest)>,
    /// Transport outcome the recording test transport reports for owned routes; `None` accepts every
    /// change.
    #[cfg(any(test, feature = "test-support"))]
    pub refuse_for_test: Option<tilo_ui_base::collaboration::CollabSendError>,
}
impl OwnerDocumentRoutes {
    /// Give every record of a live owner's tree its own owner: a chat's message parts, a container's
    /// child records. Without this a part only one device wrote never left it.
    fn hold_trees(&mut self, current: &mut Vec<(HistoryOwner, bool)>) {
        let mut stores: Vec<crate::resource::ResourceStore> = Vec::new();
        for (owner, _) in current.iter() {
            let (store, _) = owner.store_checkout();
            if !stores.iter().any(|known| known.same_owner(store)) {
                stores.push(store.clone());
            }
        }
        let mut held = Vec::new();
        for store in &stores {
            for checkout in store.open_tree_records() {
                let owned = current.iter().any(|(owner, _)| {
                    let (other, owned) = owner.store_checkout();
                    other.same_owner(store) && owned == &checkout
                });
                if owned {
                    continue;
                }
                let index = self
                    .tree
                    .iter()
                    .position(|entry| entry.store.same_owner(store) && entry.checkout == checkout);
                let entry = match index {
                    Some(index) => self.tree.swap_remove(index),
                    None => TreeRecord {
                        store: store.clone(),
                        checkout,
                        lifetime: super::owner::ResourceOwnerLifetime::new(),
                    },
                };
                held.push(entry);
            }
        }
        for gone in self.tree.drain(..) {
            gone.lifetime.active.set(false);
        }
        for entry in &held {
            current.push((
                HistoryOwner::record(
                    entry.store.clone(),
                    entry.checkout.clone(),
                    &entry.lifetime,
                    true,
                ),
                true,
            ));
        }
        self.tree = held;
    }

    /// Whether `entry` syncs only records held for a live owner's tree ([`Self::hold_trees`]): no view
    /// shows them and no published action names them, so the window's published surface never waits for
    /// them.
    pub fn holds_only_tree_records(&self, entry: &OwnerRoute) -> bool {
        entry.owners.iter().all(|owner| {
            let (store, checkout) = owner.store_checkout();
            self.tree
                .iter()
                .any(|held| held.store.same_owner(store) && &held.checkout == checkout)
        })
    }

    pub fn retired_route(&self, route: &RouteSpec) -> bool {
        self.retired.contains(route)
    }
    pub fn reconcile(
        &mut self,
        root: &dyn Widget,
        observation: Option<AcceptedRoute>,
        generation: u64,
        outer: &AutoCommit,
    ) -> Vec<RouteJoin> {
        fn collect(root: &dyn Widget, owners: &mut Vec<(HistoryOwner, bool)>) {
            let held = crate::bound_dispatch::history_owner(root).map(|owner| (owner, false));
            let beside = crate::bound_dispatch::resource_owners(root)
                .into_iter()
                .map(|owner| (owner, true));
            for (owner, answers) in held.into_iter().chain(beside) {
                if !owners
                    .iter()
                    .any(|(existing, _)| existing.same_owner(&owner))
                {
                    owners.push((owner, answers));
                }
            }
            for child in root.children() {
                collect(child, owners);
            }
        }
        if !self.observe_observation(observation, generation) {
            return vec![];
        }
        let mut current = vec![];
        collect(root, &mut current);
        self.hold_trees(&mut current);
        crate::history_mode::capability::visit_framework_history(root, &mut |_, live| {
            collect(live, &mut current)
        });
        self.install(current, outer)
    }

    /// Follow the primary route `observation` (of `generation`): another one starts every route over.
    fn observe_observation(&mut self, observation: Option<AcceptedRoute>, generation: u64) -> bool {
        if self.observation != observation || self.primary_generation != generation {
            if !self.primary.is_empty() || self.owners.iter().any(|route| !route.pending.is_empty())
            {
                self.status = Some(tilo_ui_base::i18n::I18nManager::lookup_plain(
                    "history-sync-owner-changed",
                ));
            }
            self.owners.clear();
            self.retired.clear();
            self.primary = PendingRemote::default();
            self.primary_whole = false;
            self.observation = observation;
            self.primary_generation = generation;
        }
        self.observation.is_some()
    }

    /// Hold the routes of the `current` owners (each with whether it answers snapshot requests), giving
    /// up those no owner holds any more.
    fn install(
        &mut self,
        current: Vec<(HistoryOwner, bool)>,
        outer: &AutoCommit,
    ) -> Vec<RouteJoin> {
        self.owners.retain_mut(|entry| {
            entry.owners.retain(|owner| {
                current.iter().any(|(actual, _)| actual.same_owner(owner))
                    && owner.validate().is_ok()
            });
            if entry.owners.is_empty() {
                self.retired.insert(entry.route.clone());
                self.status = Some(tilo_ui_base::i18n::I18nManager::lookup_plain(
                    "history-sync-owner-closed",
                ));
            }
            !entry.owners.is_empty()
        });
        let mut joins = vec![];
        for (owner, answers_snapshots) in current {
            let Some(route) = owner.broker_route() else {
                continue;
            };
            if self.retired.contains(&route) {
                continue;
            }
            let resource_type = owner.resource_type();
            if let Some(entry) = self.owners.iter_mut().find(|entry| entry.route == route) {
                if !entry
                    .owners
                    .iter()
                    .any(|existing| existing.same_owner(&owner))
                {
                    entry.owners.push(owner);
                }
            } else {
                // A record this owner cannot read is not joined.
                if owner.document(outer).is_err() {
                    continue;
                }
                joins.push(RouteJoin {
                    route: route.clone(),
                    resource_type: resource_type.clone(),
                });
                // A store that writes its records through its port itself is the one path its
                // changes take. Publishing them on a route too would send a second copy, and a
                // route that cannot confirm the publication settles the store's writer for a
                // write the workspace never took: the change would reach neither.
                let publishes =
                    owner.publishes_resource() && !owner.store_checkout().0.writes_for_self();
                self.owners.push(OwnerRoute {
                    route,
                    owners: vec![owner],
                    publishes,
                    // The whole document goes out first, so nothing is held
                    // yet.
                    sent: crate::state::DocumentFrontier::default(),
                    needs_snapshot: true,
                    pending: PendingRemote::default(),
                    outbound_failure: None,
                    answers_snapshots,
                    opening: true,
                });
            }
        }
        joins
    }
}

#[cfg(test)]
mod round1_tests {
    use super::*;
    #[test]
    fn round1_bad_front_does_not_relax_overflow_recovery_or_lose_queued_bytes() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "value", "snapshot").unwrap();
        doc.commit();
        let snapshot = DocumentMutation::Snapshot(doc.save());
        let mut queue = PendingRemote::default();
        assert!(queue.push(DocumentMutation::Snapshot(vec![1])));
        for _ in 1..MAX_PENDING_ITEMS {
            assert!(queue.push(snapshot.clone()));
        }
        assert!(!queue.push(snapshot.clone()));
        let overflow = queue.failure.clone();
        queue.quarantine_invalid_front("invalid front".into());
        assert_eq!(queue.failure, overflow);
        assert_eq!(queue.queue.len(), MAX_PENDING_ITEMS - 1);
        while !queue.is_empty() {
            queue.pop_applied();
        }
        assert_eq!(queue.bytes, 0);
        assert_eq!(
            queue.failure, overflow,
            "old accepted snapshots cannot satisfy a later overflow"
        );
        assert!(!queue.recover_snapshot(DocumentMutation::Snapshot(vec![1])));
        assert_eq!(queue.failure, overflow);
        assert!(queue.recover_snapshot(snapshot));
        assert_eq!(
            queue.failure, overflow,
            "valid recovery remains pending until successful merge"
        );
        queue.pop_applied();
        assert!(queue.failure.is_none());
        assert_eq!(queue.bytes, 0);
    }
}

#[cfg(test)]
mod round2_tests {
    use super::*;
    #[test]
    fn round2_refused_traffic_also_retires_recovery_by_an_older_queued_snapshot() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "snapshot", "accepted before failure")
            .unwrap();
        doc.commit();
        let heads = doc.base_heads();
        let snapshot = DocumentMutation::Snapshot(doc.save());
        doc.put(automerge::ROOT, "later", "not covered by older snapshot")
            .unwrap();
        doc.commit();
        let delta = DocumentMutation::Incremental(doc.save_after(&heads));
        let fresh = DocumentMutation::Snapshot(doc.save());
        let mut queue = PendingRemote::default();
        assert!(queue.admit(DocumentMutation::Snapshot(vec![1])));
        assert!(queue.admit(snapshot.clone()));
        queue.quarantine_invalid_front("invalid front requires recovery".into());
        // S was admitted before discovering the bad front. A temporarily
        // unavailable owner may still leave it queued when later traffic arrives.
        assert!(!queue.admit(delta));
        queue.pop_applied();
        assert!(
            queue.failure.is_some(),
            "the older queued S cannot cover a later refused message"
        );
        assert!(queue.admit(fresh));
        queue.pop_applied();
        assert!(queue.failure.is_none());
    }
}

#[cfg(test)]
mod committed_changes_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// What a window holds, and the workspace's next committed change as the owner hands it over:
    /// changes that build on what the window holds.
    fn held_and_delta() -> (AutoCommit, AutoCommit, Vec<u8>) {
        let mut workspace = AutoCommit::new();
        workspace.put(automerge::ROOT, "title", "notes").unwrap();
        workspace.commit();
        let held = workspace.fork();
        let before = workspace.get_heads();
        workspace.put(automerge::ROOT, "note", "new").unwrap();
        workspace.commit();
        let delta = workspace.save_after(&before);
        (workspace, held, delta)
    }

    fn errors_during(run: impl FnOnce()) -> String {
        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink::default();
        let writer = sink.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::ERROR)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, run);
        String::from_utf8(sink.0.lock().unwrap().clone()).unwrap()
    }

    tilo_test_env::scenario! {
        fn committed_changes_apply_whole_or_not_at_all;

        phase committed_changes_that_build_on_held_history_apply_and_log_nothing;
        {
            let (mut workspace, mut held, delta) = held_and_delta();
            let mutation = DocumentMutation::Incremental(delta);
            let mut changed = None;
            let errors = errors_during(|| {
                changed = Some(with_mutation(&mutation, |incoming| merge_committed(&mut held, incoming)).unwrap());
            });
            assert_eq!(errors, "", "a valid delta logged an error");
            assert_eq!(changed, Some(true));
            assert!(held.get_missing_deps(&[]).is_empty());
            assert_eq!(
                super::super::sorted_heads(held.get_heads()),
                super::super::sorted_heads(workspace.get_heads())
            );
        }

        phase a_truncated_delta_is_refused_whole;
        {
            let (_, mut held, delta) = held_and_delta();
            let before = held.get_heads();
            for end in 1..delta.len() {
                let mutation = DocumentMutation::Incremental(delta[..end].to_vec());
                let refused = with_mutation(&mutation, |incoming| merge_committed(&mut held, incoming));
                assert!(
                    matches!(refused, Err(RemoteApplyError::InvalidMutation(_))),
                    "truncated at {end}"
                );
                assert_eq!(
                    held.get_heads(),
                    before,
                    "truncated at {end}: nothing applied"
                );
            }
        }
    }
}

/// The user-visible status a transport outcome implies for a route: `Some`
/// replaces the current one (clearing it on success), `None` leaves it (a
/// transient refusal is retried silently).
#[allow(
    clippy::option_option,
    reason = "three outcomes: replace with a message, clear it, or leave the current status"
)]
pub fn outbound_failure(
    result: &Result<(), tilo_ui_base::collaboration::CollabSendError>,
) -> Option<Option<String>> {
    match result {
        Ok(()) => Some(None),
        Err(tilo_ui_base::collaboration::CollabSendError::TooLarge { actual, max }) => {
            Some(Some(format!(
                "Changes are no longer shared: one change is {}, above the {} collaboration limit. \
             They remain on this device.",
                tilo_ui_base::format::size(u64::try_from(*actual).unwrap_or(u64::MAX)),
                tilo_ui_base::format::size(u64::try_from(*max).unwrap_or(u64::MAX)),
            )))
        }
        Err(_) => None,
    }
}

/// What one part of the pass sends: its changes under the commit of the
/// action that made them, or alone when the part is no action's.
pub fn part_request(
    mutation: collab_types::DocumentMutation,
    commit: Option<collab_types::WorkspaceCommit>,
) -> tilo_ui_base::collaboration::CollabRequest {
    match commit {
        Some(commit) => {
            tilo_ui_base::collaboration::CollabRequest::document_action(mutation, commit)
        }
        None => tilo_ui_base::collaboration::CollabRequest::Document {
            mutation,
            commit: None,
        },
    }
}

impl OwnerDocumentRoutes {
    /// Publish what changed in every held record through `manager`: a
    /// route's whole document once it opens or a peer asks, its changes
    /// since what the transport last accepted otherwise. `outer` is the
    /// window's document, which an owner of the window's own record reads.
    pub fn publish_owned(
        &mut self,
        manager: &mut tilo_ui_base::collaboration::CollaborationManager,
        outer: &automerge::AutoCommit,
    ) -> Published {
        let mut blocked = None;
        let mut status_changed = false;
        let mut opening = false;
        let mut parts = vec![];
        for entry in &mut self.owners {
            if !entry.publishes {
                // A record this app only reads: what changes in it here —
                // the in-memory replica's genesis among it — is never sent.
                // The identity service signs no change of another app's
                // record, and a refused signature would stop the whole
                // bridge instead of this record alone.
                continue;
            }
            if manager.take_unpublished_route(&entry.route) {
                // The route was not open for what was queued for it: the
                // record goes out whole once it is.
                entry.sent = crate::state::DocumentFrontier::default();
                entry.needs_snapshot = true;
                entry.opening = true;
            }
            if entry.opening {
                if !manager.document_route_publishable(&entry.route) {
                    // Its whole document goes out once the route opens. An
                    // answer waits for that while the route still joins (a
                    // record the action created opens as its join ends). A
                    // route that found history to read first waits on that,
                    // possibly on another device, and a failed join opens on
                    // no pass: that record alone waits, and says why.
                    if let Some(failure) = manager.route_join_failure(&entry.route) {
                        if entry.outbound_failure.as_ref() != Some(&failure) {
                            entry.outbound_failure = Some(failure);
                            status_changed = true;
                        }
                    } else if manager.document_route_joining(&entry.route) {
                        opening = true;
                    }
                    continue;
                }
                entry.opening = false;
            }
            let Some(owner) = entry.owners.first() else {
                blocked.get_or_insert("a document route has no owner");
                continue;
            };
            if entry.owners.iter().any(|other| !owner.same_document(other)) {
                entry.pending.failure = Some("Multiple different document owners claim this collaboration route. Synchronization is stopped.".into());
                blocked.get_or_insert("document owners conflict on the publication route");
                continue;
            }
            let publication = owner.prepare_publication(
                outer,
                &mut entry.sent,
                entry.needs_snapshot,
                entry.pending.failure.is_some(),
            );
            let (mutation, heads, document) = match publication {
                Ok(crate::history_mode::owner::OwnedDocumentPublication::Unchanged) => continue,
                Ok(crate::history_mode::owner::OwnedDocumentPublication::Unfinished) => {
                    blocked.get_or_insert("an owned document has unfinished changes");
                    continue;
                }
                Ok(crate::history_mode::owner::OwnedDocumentPublication::Blocked) => {
                    blocked.get_or_insert("a changed document requires synchronization recovery");
                    continue;
                }
                Ok(crate::history_mode::owner::OwnedDocumentPublication::Changed {
                    mutation,
                    heads,
                    document,
                }) => (mutation, heads, document),
                Err(_) => {
                    blocked.get_or_insert("an owned document is unreadable");
                    continue;
                }
            };
            parts.push(PublishedPart {
                route: entry.route.clone(),
                mutation,
                heads,
                document,
            });
        }
        let commits = publication_commits(&mut parts);
        for (part, commit) in parts.into_iter().zip(commits) {
            let route = part.route;
            let heads = part.heads;
            let request = part_request(part.mutation, commit);
            #[cfg(any(test, feature = "test-support"))]
            let result = if let Some(refusal) = self.refuse_for_test.clone() {
                Err(refusal)
            } else {
                let observation = (route.clone(), request.clone());
                let result = manager.try_send_to_route(route.clone(), request);
                if result.is_ok() {
                    self.sent.push(observation);
                }
                result
            };
            #[cfg(not(any(test, feature = "test-support")))]
            let result = manager.try_send_to_route(route.clone(), request);
            let failure = outbound_failure(&result);
            if result.is_err() {
                blocked.get_or_insert("the document command could not enter the transport queue");
            }
            let Some(entry) = self.owners.iter_mut().find(|entry| entry.route == route) else {
                continue;
            };
            // Only an accepted change moves the route's frontier; a refused
            // one is offered again on the next publication.
            if result.is_ok() {
                entry.sent.advance(heads);
                entry.needs_snapshot = false;
            }
            if let Some(failure) = failure
                && entry.outbound_failure.as_ref() != failure.as_ref()
            {
                entry.outbound_failure = failure;
                status_changed = true;
            }
        }
        Published {
            blocked,
            status_changed,
            opening,
        }
    }

    /// Merge what peers sent into every held record, each as an action boundary of the window's
    /// document `outer` ([`crate::history_mode::HistoryOwner::merge_remote`]). Whether a record
    /// changed.
    pub fn merge_owned(&mut self, outer: &mut automerge::AutoCommit) -> bool {
        let mut changed = false;
        for entry in &mut self.owners {
            let Some(owner) = entry.owners.first() else {
                continue;
            };
            if entry.owners.iter().any(|other| !owner.same_document(other)) {
                entry.pending.failure = Some("Multiple different document owners claim this collaboration route. Synchronization is stopped.".into());
                continue;
            }
            while let Some(mutation) = entry.pending.front() {
                // A document that cannot take a peer's changes yet must not load — and, on
                // bytes that do not load, quarantine — them now: hold them queued, closing an
                // unfinished named step when that is what holds it.
                if owner.defers_remote() && !owner.close_named_step(outer) {
                    break;
                }
                use automerge::transaction::Transactable as _;
                let (store, checkout) = owner.store_checkout();
                // A peer's whole document that this replica merges and is
                // still ahead of: the sender lacks something only we hold.
                let sender_heads = match mutation {
                    collab_types::DocumentMutation::Snapshot(bytes) if entry.answers_snapshots => {
                        automerge::AutoCommit::load(bytes)
                            .ok()
                            .map(|mut sent| crate::history_mode::sorted_heads(sent.get_heads()))
                    }
                    _ => None,
                };
                match crate::history_mode::routes::with_mutation(mutation, |incoming| {
                    owner.merge_remote(incoming, outer)
                }) {
                    Ok(merged) => {
                        // The route delivered these changes, so they are never
                        // published back on it; this app's own unsent changes
                        // stay unsent and go out on the next publication.
                        entry.sent.observe(merged.observed);
                        let Some(applied) = merged.changed else {
                            break;
                        };
                        entry.pending.pop_applied();
                        changed |= applied;
                        if let Some(sender_heads) = sender_heads
                            && store
                                .read_checked(&checkout.id, |doc| {
                                    crate::history_mode::sorted_heads(doc.base_heads())
                                })
                                .is_ok_and(|heads| heads != sender_heads)
                        {
                            entry.needs_snapshot = true;
                        }
                        if applied
                            && let Err(error) = store.schedule_committed_history(&checkout.id)
                        {
                            tracing::error!(%error, "an applied remote change was not saved");
                        }
                    }
                    Err(error) => {
                        let reason = format!(
                            "Remote document could not be applied: {error}. Synchronization needs recovery."
                        );
                        if error.is_invalid_mutation() {
                            entry.pending.quarantine_invalid_front(reason);
                            continue;
                        }
                        entry.pending.failure = Some(reason);
                        break;
                    }
                }
            }
        }
        changed
    }

    /// Queue `mutation` for the owned record whose route it came on, if any:
    /// `Ok` whether its queue refused it, `Err` giving it back when no owned
    /// record's route carried it.
    pub fn admit_owned(
        &mut self,
        route: &tilo_ui_base::collaboration::RouteSpec,
        mutation: collab_types::DocumentMutation,
    ) -> Result<bool, collab_types::DocumentMutation> {
        let Some(entry) = self.owners.iter_mut().find(|entry| entry.route == *route) else {
            return Err(mutation);
        };
        let refused = !entry.pending.admit(mutation);
        if refused {
            tracing::warn!(status = ?entry.pending.failure, "collab: routed document deferred queue refused update; resynchronization required");
        }
        Ok(refused)
    }
}

/// What publishing a holder's records reported.
pub struct Published {
    /// Why some record's changes are not reaching peers, if any.
    pub blocked: Option<&'static str>,
    /// A route's user-visible status changed.
    pub status_changed: bool,
    /// A record's route is still joining: its whole document waits for it.
    pub opening: bool,
}

/// One unit's part of a publication pass: what changed in it, and the route
/// it travels on.
pub struct PublishedPart {
    pub route: tilo_ui_base::collaboration::RouteSpec,
    pub mutation: collab_types::DocumentMutation,
    pub heads: Vec<automerge::ChangeHash>,
    pub document: automerge::AutoCommit,
}

/// The commit each part of the pass travels under: the units one action
/// wrote share one, so every device applies all of it or none. `None` for a
/// part that carries no commit of its own — a whole document, or a change the
/// record's own ownership carries.
///
/// A commit names one workspace, so the parts of another workspace's route
/// are committed to apart. The author's device is left as zeros: the
/// device's store stamps the device it writes with.
pub fn publication_commits(
    parts: &mut [PublishedPart],
) -> Vec<Option<collab_types::WorkspaceCommit>> {
    let mut commits = vec![None; parts.len()];
    // Every unit of the pass, by the workspace it belongs to.
    let mut groups: Vec<(uuid::Uuid, Vec<(usize, collab_types::UnitMutation)>)> = Vec::new();
    for (index, part) in parts.iter_mut().enumerate() {
        let workspace = part.route.cluster_id;
        let unit = collab_types::UnitMutation {
            app: &part.route.app_id,
            document_id: part.route.document.as_deref().unwrap_or(""),
            mutation: &part.mutation,
            document: Some(part.document.document()),
        };
        match groups.iter_mut().find(|(known, _)| *known == workspace) {
            Some((_, group)) => group.push((index, unit)),
            None => groups.push((workspace, vec![(index, unit)])),
        }
    }
    for (workspace, group) in groups {
        let units: Vec<collab_types::UnitMutation> = group.iter().map(|(_, unit)| *unit).collect();
        for ((index, _), commit) in group.iter().zip(unit_commits(workspace, &units)) {
            if let Some(slot) = commits.get_mut(*index) {
                *slot = commit;
            }
        }
    }
    commits
}

/// The commit each of `units`, all of `workspace`, travels under: one entry
/// per unit, `None` for one that carries no change or names no action.
pub fn unit_commits(
    workspace: uuid::Uuid,
    units: &[collab_types::UnitMutation],
) -> Vec<Option<collab_types::WorkspaceCommit>> {
    let author = collab_types::CommitAuthor {
        actor: None,
        device: [0; 32],
    };
    collab_types::WorkspaceCommit::of_units(workspace, &author, units).unwrap_or_else(|error| {
        tracing::warn!(%error, "collab: a written change commits to no action");
        vec![None; units.len()]
    })
}
