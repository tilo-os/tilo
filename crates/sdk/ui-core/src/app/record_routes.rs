//! Records a host holds without a window, kept in step over their routes.
//!
//! A window's records sync through its App (`super::document_routes`). A
//! host that holds records of its own (the session keeps the person's
//! settings sections) syncs them the same way through a
//! [`RecordRouteParticipant`]: each record joins its route on the broker, its
//! changes go out as the window's do, and what the person's other devices
//! (or any other holder) send is merged in. Signing, encryption and the
//! person room's admission stay the SDK's, as for a window.

use crate::collaboration::{CollabEvent, CollaborationManager, HostSessionConnector, RouteSpec};
use crate::history_mode::HistoryOwner;
use crate::history_mode::routes::OwnerDocumentRoutes;
use automerge::AutoCommit;

/// Keeps the records a host holds without a window in step with every other
/// holder over their routes, through `manager`.
pub struct RecordRouteParticipant {
    manager: CollaborationManager,
    routes: OwnerDocumentRoutes,
    /// Where the broker's traffic wakes the holder; kept as long as the
    /// manager it was captured by.
    _wake: crate::WakeScope,
    #[cfg(test)]
    witness_for_test: Option<crate::collaboration::PrimaryRouteWitness>,
}

impl RecordRouteParticipant {
    /// A participant for the records `owners` hold, reaching the broker
    /// through the host's `connector` as the app whose records they are,
    /// joined first on the first record's route. `wake` runs, on whatever
    /// thread, whenever the broker sent something: the holder then pumps.
    /// `None` when no record has a route to join.
    #[must_use]
    pub fn connect(
        connector: HostSessionConnector,
        owners: &[HistoryOwner],
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Option<Self> {
        let (route, kind) = owners.iter().find_map(|owner| {
            let route = owner.broker_route()?;
            Some((route, owner.store_checkout().1.kind.as_str().to_owned()))
        })?;
        let mut scope = crate::WakeScope::new();
        if let Err(error) = scope.bind(move |_| wake()) {
            tracing::warn!(%error, "held records are not woken by the broker");
        }
        let manager = {
            let _entry = scope.enter();
            CollaborationManager::for_route_host_in_background(connector, route, Some(kind))
        };
        Some(Self {
            manager,
            routes: OwnerDocumentRoutes::default(),
            _wake: scope,
            #[cfg(test)]
            witness_for_test: None,
        })
    }

    /// Hold the routes of `owners`, merge into them what peers sent, and
    /// publish what changed here: call it whenever a record changed here and
    /// whenever the manager wakes its owner. Whether a peer's change was
    /// merged into one of them.
    pub fn pump(&mut self, owners: &[HistoryOwner]) -> bool {
        // The holder's records are their own documents; none reads the
        // window document an App would pass.
        let outer = automerge::AutoCommit::new();
        let events = self.manager.poll();
        if self.manager.document_publication_failed() {
            for entry in &mut self.routes.owners {
                entry.sent_heads.clear();
                entry.needs_snapshot = true;
            }
        }
        self.reconcile(owners, &outer);
        for event in events {
            match event {
                CollabEvent::Document {
                    envelope,
                    decrypted_payload,
                    ..
                } => {
                    let _ = self
                        .routes
                        .admit_owned(&envelope, decrypted_payload.as_deref());
                }
                CollabEvent::ScrollbackBatch {
                    ops,
                    decrypted_payloads,
                    ..
                } => {
                    for (index, envelope) in ops.iter().enumerate() {
                        let decrypted = decrypted_payloads
                            .get(index)
                            .and_then(|payload| payload.as_deref());
                        let _ = self.routes.admit_owned(envelope, decrypted);
                    }
                }
                // No host replays these routes: a late joiner gets the
                // whole record from a holder.
                CollabEvent::DocumentSnapshotRequested { route } => {
                    if let Some(entry) = self
                        .routes
                        .owners
                        .iter_mut()
                        .find(|entry| entry.route == route)
                    {
                        entry.needs_snapshot = true;
                    }
                }
                _ => {}
            }
        }
        self.manager.finish_event_batch();
        let merged = self.routes.merge_owned();
        let published = self.routes.publish_owned(&mut self.manager, &outer);
        if let Some(reason) = published.blocked {
            tracing::debug!(reason, "held records not published yet");
        }
        merged
    }

    /// Join the routes of `owners` not yet joined, once the primary route
    /// is acknowledged.
    fn reconcile(&mut self, owners: &[HistoryOwner], outer: &automerge::AutoCommit) {
        #[cfg(test)]
        let witness = self
            .witness_for_test
            .clone()
            .or_else(|| self.manager.primary_route().cloned());
        #[cfg(not(test))]
        let witness = self.manager.primary_route().cloned();
        let primary = witness.as_ref().map(|witness| witness.route().clone());
        let joins = self.routes.reconcile_owners(owners, witness, outer);
        let routes: Vec<RouteSpec> = primary
            .iter()
            .cloned()
            .chain(
                self.routes
                    .owners
                    .iter()
                    .map(|entry| entry.route.clone())
                    .filter(|route| Some(route) != primary.as_ref()),
            )
            .collect();
        // A windowless holder dispatches no actions; its document never
        // changes generation.
        self.manager.set_document_owner_routes(routes, 0);
        for join in joins {
            // The primary route was joined with the manager.
            if Some(&join.route) != primary.as_ref() {
                self.manager.join_route(join.route, join.record_kind);
            }
        }
    }
}

/// A record a host holds by its route alone, as a plain document of its
/// own, without a window or a record store: a workspace's shared desktop,
/// which its windows' host keeps.
///
/// Its changes go out on the route as a window's do, signed, end-to-end
/// encrypted where the route is, and retained by the room's hosts for
/// devices that join later: the route's first publication in a session is
/// the whole document, every later one what changed since. What other
/// holders write comes back in, each write once, with its author, for the
/// holder to decide before it is merged ([`RouteDocument::pump`]); what a
/// room's governor already decided (its committed history) is merged as
/// it comes. A write that arrives before the changes it builds on waits
/// until they are here. Nothing here waits on a timer: a write the route
/// did not take is offered again when the route takes writes again, and
/// the whole document again once the connection was lost.
pub struct RouteDocument {
    manager: CollaborationManager,
    route: RouteSpec,
    /// The heads the route holds from this holder; `None` until it has
    /// published its whole document since it connected.
    sent: Option<Vec<automerge::ChangeHash>>,
    /// Committed history the route delivered before the document could
    /// take it.
    committed: Vec<crate::collaboration::CommittedDocument>,
    /// Writes that arrived before the changes they build on, oldest first,
    /// with who signed each: offered again after anything else merged.
    waiting: std::collections::VecDeque<([u8; 32], collab_types::DocumentMutation)>,
    _wake: crate::WakeScope,
    /// What this holder published, in order.
    #[cfg(test)]
    published: Vec<collab_types::DocumentMutation>,
}

/// One write another holder made, as it arrived on the route.
pub struct Arrival<'a> {
    /// The device that signed it.
    pub author: [u8; 32],
    pub mutation: &'a collab_types::DocumentMutation,
}

impl Arrival<'_> {
    /// Merge the write into `doc`, whole or not at all, as a holder that
    /// takes every write does.
    ///
    /// # Errors
    /// The write is not changes `doc` can take.
    pub fn merge_into(&self, doc: &mut AutoCommit) -> Result<Admission, String> {
        crate::history_mode::routes::merge_committed(doc, self.mutation)
            .map(Admission::Merged)
            .map_err(|error| error.to_string())
    }
}

/// What a holder did with a write another holder made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Taken in; whether it changed the document.
    Merged(bool),
    /// It builds on changes the document does not hold yet: nothing of it
    /// was taken in, and it is offered again once something else merged.
    Waiting,
}

/// How many writes wait for the changes they build on, at most. Past this
/// the oldest is let go: the route delivers the record whole to a holder
/// that lacks so much.
const MAX_WAITING: usize = 256;

/// Decides a write another holder made: merges it into the document it is
/// given, waits for what it builds on, or refuses it and leaves the
/// document as it was.
pub type Admit<'a> = dyn FnMut(&mut AutoCommit, Arrival<'_>) -> Result<Admission, String> + 'a;

/// What one [`RouteDocument::pump`] did to the document.
#[derive(Debug, Default)]
pub struct Pumped {
    /// Whether what arrived changed the document.
    pub merged: bool,
    /// Why each write the holder refused was refused.
    pub refused: Vec<String>,
}

impl RouteDocument {
    /// Hold `route`, reaching the broker through the host's `connector` as
    /// the app whose record it is, joined as a record of `kind`. `wake`
    /// runs, on whatever thread, whenever the broker sent something: the
    /// holder then pumps.
    #[must_use]
    pub fn connect(
        connector: HostSessionConnector,
        route: RouteSpec,
        kind: Option<String>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let mut scope = crate::WakeScope::new();
        if let Err(error) = scope.bind(move |_| wake()) {
            tracing::warn!(%error, "a held route is not woken by the broker");
        }
        let manager = {
            let _entry = scope.enter();
            CollaborationManager::for_route_host_in_background(connector, route.clone(), kind)
        };
        Self::with_manager(manager, route, scope)
    }

    /// [`Self::connect`] for a host that reaches the broker its own way:
    /// `manager` makes the connection to `route`, inside the scope `wake`
    /// runs from, as [`Self::connect`]'s connector's is.
    #[must_use]
    pub fn connect_with(
        manager: impl FnOnce() -> CollaborationManager,
        route: RouteSpec,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let mut scope = crate::WakeScope::new();
        if let Err(error) = scope.bind(move |_| wake()) {
            tracing::warn!(%error, "a held route is not woken by the broker");
        }
        let manager = {
            let _entry = scope.enter();
            manager()
        };
        Self::with_manager(manager, route, scope)
    }

    fn with_manager(
        manager: CollaborationManager,
        route: RouteSpec,
        wake: crate::WakeScope,
    ) -> Self {
        let mut held = Self {
            manager,
            route,
            sent: None,
            committed: Vec::new(),
            waiting: std::collections::VecDeque::new(),
            _wake: wake,
            #[cfg(test)]
            published: Vec::new(),
        };
        let routes = vec![held.route.clone()];
        held.manager.set_document_owner_routes(routes, 0);
        held
    }

    /// The route held.
    #[must_use]
    pub const fn route(&self) -> &RouteSpec {
        &self.route
    }

    /// Whether the route takes this holder's writes now.
    #[must_use]
    pub fn publishable(&self) -> bool {
        self.manager.document_route_publishable(&self.route)
    }

    /// Merge into `doc` what arrived on the route, each write another
    /// holder made only as `admit` decides (it merges the write into the
    /// document it is given, or refuses it and leaves that as it was), and
    /// publish what `doc` holds that the route does not. Call it whenever
    /// `doc` changed here and whenever the broker woke the holder.
    pub fn pump(&mut self, doc: &mut AutoCommit, admit: &mut Admit<'_>) -> Pumped {
        let mut pumped = Pumped::default();
        let events = self.manager.poll();
        if self.manager.document_publication_failed() {
            self.sent = None;
        }
        for event in events {
            match event {
                CollabEvent::Document {
                    envelope,
                    decrypted_payload,
                    ..
                } => self.arrived(
                    doc,
                    &envelope,
                    decrypted_payload.as_deref(),
                    admit,
                    &mut pumped,
                ),
                CollabEvent::ScrollbackBatch {
                    ops,
                    decrypted_payloads,
                    ..
                } => {
                    for (index, envelope) in ops.iter().enumerate() {
                        let decrypted = decrypted_payloads
                            .get(index)
                            .and_then(|payload| payload.as_deref());
                        self.arrived(doc, envelope, decrypted, admit, &mut pumped);
                    }
                }
                CollabEvent::DocumentSnapshotRequested { route } if route == self.route => {
                    self.sent = None;
                }
                CollabEvent::CommittedDocument { route, read } if route == self.route => {
                    self.committed.push(read);
                }
                _ => {}
            }
        }
        self.manager.finish_event_batch();
        pumped.merged |= self.apply_committed(doc);
        if pumped.merged {
            self.offer_waiting(doc, admit, &mut pumped);
        }
        self.publish(doc);
        pumped
    }

    /// Offer the writes that waited for the changes they build on again,
    /// oldest first, for as long as one of them merges.
    fn offer_waiting(&mut self, doc: &mut AutoCommit, admit: &mut Admit<'_>, pumped: &mut Pumped) {
        loop {
            let mut progressed = false;
            for (author, mutation) in std::mem::take(&mut self.waiting) {
                progressed |= self.offer(doc, author, mutation, admit, pumped);
            }
            if !progressed || self.waiting.is_empty() {
                return;
            }
        }
    }

    /// Offer one write `author` made to `admit`: merged, kept waiting, or
    /// refused. Whether it merged.
    fn offer(
        &mut self,
        doc: &mut AutoCommit,
        author: [u8; 32],
        mutation: collab_types::DocumentMutation,
        admit: &mut Admit<'_>,
        pumped: &mut Pumped,
    ) -> bool {
        let before = doc.get_heads();
        match admit(
            doc,
            Arrival {
                author,
                mutation: &mutation,
            },
        ) {
            Ok(Admission::Merged(changed)) => {
                pumped.merged |= changed;
                self.echoed(&before, doc);
                true
            }
            Ok(Admission::Waiting) => {
                if self.waiting.len() >= MAX_WAITING {
                    self.waiting.pop_front();
                    tracing::warn!("a held route let go of a write that waited too long");
                }
                self.waiting.push_back((author, mutation));
                false
            }
            Err(reason) => {
                pumped.refused.push(reason);
                false
            }
        }
    }

    /// Take in one write another holder made, as `admit` decides; when it
    /// merged, what waited for it may merge now too.
    fn arrived(
        &mut self,
        doc: &mut AutoCommit,
        envelope: &collab_types::SignedOp,
        decrypted: Option<&[u8]>,
        admit: &mut Admit<'_>,
        pumped: &mut Pumped,
    ) {
        let Some(payload) =
            super::document_routes::validated_route_payload_with(&self.route, envelope, decrypted)
        else {
            return;
        };
        let mutation =
            match super::document_routes::decode_routed_mutation(&self.route, payload, decrypted) {
                Ok(mutation) => mutation,
                Err(error) => {
                    tracing::warn!(%error, "a held route delivered an unreadable write");
                    return;
                }
            };
        if self.offer(doc, *envelope.author.as_bytes(), mutation, admit, pumped) {
            self.offer_waiting(doc, admit, pumped);
        }
    }

    /// Merge committed history the route delivered: a room's governor
    /// decided it already. Changes whose base the document lacks are
    /// refused, and the route then delivers the document whole.
    fn apply_committed(&mut self, doc: &mut AutoCommit) -> bool {
        let mut merged = false;
        for read in std::mem::take(&mut self.committed) {
            if !read.is_current() {
                continue;
            }
            if !read.applies_to(&*doc) {
                read.refuse();
                continue;
            }
            let before = doc.get_heads();
            let applied = read.with_mutation(|mutation| {
                crate::history_mode::routes::merge_committed(doc, mutation)
            });
            match applied {
                Ok(Ok(changed)) => {
                    read.applied();
                    merged |= changed;
                    self.echoed(&before, doc);
                }
                Ok(Err(error)) => {
                    tracing::warn!(%error, "a held route delivered unreadable committed history");
                }
                Err(error) => {
                    tracing::debug!(%error, "a held route's committed history was replaced")
                }
            }
        }
        merged
    }

    /// What arrived is held by the route already: when everything of this
    /// holder's was published before it merged, the merged document counts
    /// as published.
    fn echoed(&mut self, before: &[automerge::ChangeHash], doc: &mut AutoCommit) {
        if self.sent.as_deref() == Some(before) {
            self.sent = Some(doc.get_heads());
        }
    }

    /// Offer the route what `doc` holds that it does not: the whole
    /// document first, then what changed since.
    fn publish(&mut self, doc: &mut AutoCommit) {
        if !self.publishable() {
            return;
        }
        let heads = doc.get_heads();
        let mutation = match &self.sent {
            Some(sent) if *sent == heads => return,
            Some(sent) => collab_types::DocumentMutation::Incremental(doc.save_after(sent)),
            None if heads.is_empty() => return,
            None => collab_types::DocumentMutation::Snapshot(doc.save()),
        };
        #[cfg(test)]
        let observed = mutation.clone();
        let request = crate::collaboration::CollabRequest::Document {
            mutation,
            authorization: None,
        };
        match self.manager.try_send_to_route(self.route.clone(), request) {
            Ok(()) => {
                #[cfg(test)]
                self.published.push(observed);
                self.sent = Some(heads);
            }
            Err(error) => tracing::debug!(%error, "a held route takes no writes yet"),
        }
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use crate::collaboration::CollabRequest;
    use crate::record::person::{PersonRecords, StoredRecord};
    use crate::record::{RecordKind, RecordOwners};
    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable as _;
    use std::rc::Rc;

    const APP: &str = "so.tilo.test-person";
    const ROOT_KEY: [u8; 32] = [42; 32];

    /// One device's copy of the person's watched list, held by a host
    /// without a window.
    fn device() -> StoredRecord {
        let mut owners = RecordOwners::with_store(Rc::new(durable_store::MemoryStore::new()));
        PersonRecords::open(
            &mut owners,
            APP,
            crate::permissions::UserId(hex::encode(ROOT_KEY)),
        )
        .unwrap()
        .open_record(RecordKind::new("test.watched"), &|doc| {
            doc.put_object(automerge::ROOT, "watched", automerge::ObjType::Map)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .unwrap()
    }

    fn watch(record: &StoredRecord, video: &str) {
        record
            .edit(true, |doc| {
                let watched = doc.get(automerge::ROOT, "watched").unwrap().unwrap().1;
                doc.put(&watched, video, true)
                    .map_err(|error| error.to_string())
            })
            .unwrap();
    }

    fn watched(record: &StoredRecord) -> Vec<String> {
        record
            .read(|doc| {
                let watched = doc.get(automerge::ROOT, "watched").unwrap().unwrap().1;
                doc.keys(&watched).collect()
            })
            .unwrap()
    }

    /// A participant holding `record`, its route open.
    fn holding(record: &StoredRecord) -> RecordRouteParticipant {
        let mut participant = RecordRouteParticipant {
            manager: CollaborationManager::connected_for_lifecycle_test(),
            routes: OwnerDocumentRoutes::default(),
            _wake: crate::WakeScope::new(),
            // The manager's own route is another record's.
            witness_for_test: Some(crate::collaboration::PrimaryRouteWitness {
                route: RouteSpec::record(APP, uuid::Uuid::new_v4(), "held-first"),
                epoch: uuid::Uuid::new_v4(),
            }),
        };
        let owners = [record.history_owner()];
        participant.reconcile(&owners, &automerge::AutoCommit::new());
        let deadline = tilo_test_env::hang_deadline();
        while !participant.routes.owners.iter().all(|entry| {
            participant
                .manager
                .document_route_publishable_for_test(&entry.route)
        }) {
            assert!(
                crate::time::now() < deadline,
                "the record's route never opened"
            );
            std::thread::sleep(crate::time::Duration::from_millis(5));
        }
        participant
    }

    fn snapshots(participant: &mut RecordRouteParticipant) -> Vec<Vec<u8>> {
        std::mem::take(&mut participant.routes.sent)
            .into_iter()
            .filter_map(|(_, request)| match request {
                CollabRequest::Document {
                    mutation: collab_types::DocumentMutation::Snapshot(bytes),
                    ..
                } => Some(bytes),
                _ => None,
            })
            .collect()
    }

    fn from_device(route: &RouteSpec, document: Vec<u8>) -> collab_types::SignedOp {
        let record = route.record.as_deref().unwrap_or("");
        collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(document)
                .encode()
                .unwrap(),
            collab_types::document_artifact_id(
                &route.app_id,
                &route.cluster_id.to_string(),
                record,
            ),
            collab_types::Ed25519PublicKey::from_bytes([0; 32]),
            collab_types::document_attestation_id(
                &route.app_id,
                &route.cluster_id.to_string(),
                record,
            ),
            collab_types::OpKind::Change,
            crate::time::SystemTime::UNIX_EPOCH,
            0,
        )
    }

    /// Two hosts holding the person's record without a window publish it on
    /// its route and converge on what each other sent.
    #[test]
    fn records_held_without_a_window_converge_over_their_route() {
        let laptop = device();
        let phone = device();
        watch(&laptop, "on-the-laptop");
        watch(&phone, "on-the-phone");
        let mut on_laptop = holding(&laptop);
        let mut on_phone = holding(&phone);
        let route = laptop.route().unwrap();

        on_laptop.pump(&[laptop.history_owner()]);
        let sent = snapshots(&mut on_laptop);
        assert_eq!(sent.len(), 1, "the laptop publishes its whole record once");

        on_phone.pump(&[phone.history_owner()]);
        let _ = snapshots(&mut on_phone);
        assert_eq!(
            on_phone
                .routes
                .admit_owned(&from_device(&route, sent.into_iter().next().unwrap()), None),
            Some(false),
            "the phone takes what came on its record's route"
        );
        assert!(on_phone.pump(&[phone.history_owner()]), "and merges it");
        assert_eq!(watched(&phone), ["on-the-laptop", "on-the-phone"]);
        let answer = snapshots(&mut on_phone);
        assert_eq!(answer.len(), 1, "it answers with what the laptop lacks");

        on_laptop.routes.admit_owned(
            &from_device(&route, answer.into_iter().next().unwrap()),
            None,
        );
        on_laptop.pump(&[laptop.history_owner()]);
        assert_eq!(watched(&laptop), watched(&phone));
        assert!(
            snapshots(&mut on_laptop).is_empty(),
            "level with what it received, the laptop sends nothing more"
        );
    }

    /// A signed write of `mutation` on `route` from the device `author`.
    fn write_from(
        route: &RouteSpec,
        author: u8,
        mutation: &collab_types::DocumentMutation,
    ) -> collab_types::SignedOp {
        let record = route.record.as_deref().unwrap_or("");
        collab_types::SignedOp::new_unsigned(
            mutation.encode().unwrap(),
            collab_types::document_artifact_id(
                &route.app_id,
                &route.cluster_id.to_string(),
                record,
            ),
            collab_types::Ed25519PublicKey::from_bytes([author; 32]),
            collab_types::document_attestation_id(
                &route.app_id,
                &route.cluster_id.to_string(),
                record,
            ),
            collab_types::OpKind::Change,
            crate::time::SystemTime::UNIX_EPOCH,
            0,
        )
    }

    /// A holder of `route` whose route takes writes.
    fn holding_route(route: &RouteSpec) -> RouteDocument {
        let mut manager = CollaborationManager::connected_for_lifecycle_test();
        manager.join_route(route.clone(), crate::record::RecordKind::new(""));
        let held = RouteDocument::with_manager(manager, route.clone(), crate::WakeScope::new());
        let deadline = tilo_test_env::hang_deadline();
        while !held.publishable() {
            assert!(crate::time::now() < deadline, "the route never opened");
            std::thread::sleep(crate::time::Duration::from_millis(5));
        }
        held
    }

    fn set(doc: &mut AutoCommit, key: &str, value: &str) {
        doc.put(automerge::ROOT, key, value).unwrap();
        doc.commit();
    }

    fn get(doc: &AutoCommit, key: &str) -> Option<String> {
        doc.get(automerge::ROOT, key)
            .unwrap()
            .map(|(value, _)| value.to_string())
    }

    fn take_everything(doc: &mut AutoCommit, arrival: Arrival<'_>) -> Result<Admission, String> {
        arrival.merge_into(doc)
    }

    /// Two hosts holding one route by itself, as plain documents: each
    /// publishes its whole document once and then only what changed, takes
    /// in what the other wrote, and never sends that back.
    #[test]
    fn documents_held_by_route_converge_and_send_each_change_once() {
        let route = RouteSpec::record("so.tilo.test-held", uuid::Uuid::new_v4(), "desktop");
        let mut laptop = holding_route(&route);
        let mut phone = holding_route(&route);
        let (mut on_laptop, mut on_phone) = (AutoCommit::new(), AutoCommit::new());
        set(&mut on_laptop, "laptop", "1");
        set(&mut on_phone, "phone", "1");

        laptop.pump(&mut on_laptop, &mut take_everything);
        let first = std::mem::take(&mut laptop.published);
        assert!(
            matches!(
                first.as_slice(),
                [collab_types::DocumentMutation::Snapshot(_)]
            ),
            "the first publication is the whole document: {first:?}"
        );
        laptop.pump(&mut on_laptop, &mut take_everything);
        assert!(laptop.published.is_empty(), "nothing changed, nothing goes");

        set(&mut on_laptop, "laptop", "2");
        laptop.pump(&mut on_laptop, &mut take_everything);
        let change = std::mem::take(&mut laptop.published);
        assert!(
            matches!(
                change.as_slice(),
                [collab_types::DocumentMutation::Incremental(_)]
            ),
            "a change goes as itself: {change:?}"
        );

        phone.pump(&mut on_phone, &mut take_everything);
        let phone_first = std::mem::take(&mut phone.published);
        let mut pumped = Pumped::default();
        for mutation in first.iter().chain(&change) {
            phone.arrived(
                &mut on_phone,
                &write_from(&route, 1, mutation),
                None,
                &mut take_everything,
                &mut pumped,
            );
        }
        assert!(pumped.merged);
        assert_eq!(get(&on_phone, "laptop").as_deref(), Some("\"2\""));
        phone.pump(&mut on_phone, &mut take_everything);
        assert!(
            phone.published.is_empty(),
            "what came from the laptop is not sent back: {:?}",
            phone.published
        );

        for mutation in &phone_first {
            laptop.arrived(
                &mut on_laptop,
                &write_from(&route, 2, mutation),
                None,
                &mut take_everything,
                &mut pumped,
            );
        }
        assert_eq!(
            on_laptop.get_heads(),
            on_phone.get_heads(),
            "both hold the same"
        );
    }

    /// A write the holder refuses leaves its document as it was, and says
    /// why; who signed it is what the holder decides by.
    #[test]
    fn a_write_the_holder_refuses_is_not_merged() {
        let route = RouteSpec::record("so.tilo.test-held", uuid::Uuid::new_v4(), "desktop");
        let mut held = holding_route(&route);
        let mut doc = AutoCommit::new();
        set(&mut doc, "kept", "yes");
        let mut other = doc.fork();
        let heads = other.get_heads();
        set(&mut other, "kept", "no");
        let change = collab_types::DocumentMutation::Incremental(other.save_after(&heads));

        let mut pumped = Pumped::default();
        let mut only_from_one = |doc: &mut AutoCommit, arrival: Arrival<'_>| {
            if arrival.author == [1; 32] {
                arrival.merge_into(doc)
            } else {
                Err("not one of ours".to_owned())
            }
        };
        held.arrived(
            &mut doc,
            &write_from(&route, 9, &change),
            None,
            &mut only_from_one,
            &mut pumped,
        );
        assert_eq!(pumped.refused, ["not one of ours"]);
        assert_eq!(get(&doc, "kept").as_deref(), Some("\"yes\""));
        held.arrived(
            &mut doc,
            &write_from(&route, 1, &change),
            None,
            &mut only_from_one,
            &mut pumped,
        );
        assert_eq!(get(&doc, "kept").as_deref(), Some("\"no\""));
    }

    /// Takes a write only once the document holds every change it builds on.
    fn complete_writes_only(
        doc: &mut AutoCommit,
        arrival: Arrival<'_>,
    ) -> Result<Admission, String> {
        let mut next = doc.clone();
        let collab_types::DocumentMutation::Incremental(bytes) = arrival.mutation else {
            return arrival.merge_into(doc);
        };
        tilo_automerge::ApplyIncoming::load_incoming(&mut next, bytes)
            .map_err(|error| error.to_string())?;
        if !next.get_missing_deps(&[]).is_empty() {
            return Ok(Admission::Waiting);
        }
        let changed = next.get_heads() != doc.get_heads();
        *doc = next;
        Ok(Admission::Merged(changed))
    }

    /// A write that arrives before the one it builds on waits, and is
    /// taken in once that one is.
    #[test]
    fn a_write_ahead_of_what_it_builds_on_waits_for_it() {
        let route = RouteSpec::record("so.tilo.test-held", uuid::Uuid::new_v4(), "desktop");
        let mut held = holding_route(&route);
        let mut doc = AutoCommit::new();
        let mut other = AutoCommit::new();
        set(&mut other, "first", "1");
        let first = collab_types::DocumentMutation::Incremental(other.save_after(&[]));
        let heads = other.get_heads();
        set(&mut other, "second", "2");
        let second = collab_types::DocumentMutation::Incremental(other.save_after(&heads));

        let mut pumped = Pumped::default();
        held.arrived(
            &mut doc,
            &write_from(&route, 1, &second),
            None,
            &mut complete_writes_only,
            &mut pumped,
        );
        assert_eq!(get(&doc, "second"), None, "it waits");
        assert_eq!(held.waiting.len(), 1);
        held.arrived(
            &mut doc,
            &write_from(&route, 1, &first),
            None,
            &mut complete_writes_only,
            &mut pumped,
        );
        assert_eq!(get(&doc, "first").as_deref(), Some("\"1\""));
        assert_eq!(get(&doc, "second").as_deref(), Some("\"2\""), "and follows");
        assert!(held.waiting.is_empty());
    }
}
