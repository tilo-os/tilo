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
        let primary = witness.as_ref().map(|witness| witness.route.clone());
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
        self.manager.set_document_owner_routes(routes);
        for route in joins {
            // The primary route was joined with the manager.
            if Some(&route) != primary.as_ref() {
                self.manager.join_route(route);
            }
        }
    }
}

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;
    use crate::collaboration::CollabRequest;
    use crate::record::person::{PersonRecord, PersonRecords};
    use crate::record::{RecordKind, RecordOwners};
    use automerge::ReadDoc as _;
    use automerge::transaction::Transactable as _;
    use std::rc::Rc;

    const APP: &str = "so.tilo.test-person";
    const ROOT_KEY: [u8; 32] = [42; 32];

    /// One device's copy of the person's watched list, held by a host
    /// without a window.
    fn device() -> PersonRecord {
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

    fn watch(record: &PersonRecord, video: &str) {
        record
            .edit(true, |doc| {
                let watched = doc.get(automerge::ROOT, "watched").unwrap().unwrap().1;
                doc.put(&watched, video, true)
                    .map_err(|error| error.to_string())
            })
            .unwrap();
    }

    fn watched(record: &PersonRecord) -> Vec<String> {
        record
            .read(|doc| {
                let watched = doc.get(automerge::ROOT, "watched").unwrap().unwrap().1;
                doc.keys(&watched).collect()
            })
            .unwrap()
    }

    /// A participant holding `record`, its route open.
    fn holding(record: &PersonRecord) -> RecordRouteParticipant {
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
}
