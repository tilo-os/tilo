//! Local worker document admission under Cloud's existing Store barrier.
use crate::*;
use collab_types::{DocumentMutation, SignedOp};
use qos_action_broker::host_documents::HostDocumentTurnError;
use qos_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};
use std::collections::BTreeSet;
use ui::{collaboration::RouteSpec, invocation_scope::DocumentAuthorization};

/// Bound on the change chunks one worker publication may contain.
const MAX_BODY_CHANGES: usize = 65_536;

/// Complete change set parsed from the exact mutation body, never from the
/// captured metadata. Snapshot bodies contain every change of the document;
/// incremental bodies are concatenated raw change chunks (`save_after`).
pub(crate) fn mutation_change_hashes(
    mutation: &DocumentMutation,
) -> Result<BTreeSet<automerge::ChangeHash>, String> {
    match mutation {
        DocumentMutation::Snapshot(bytes) => {
            let mut document = automerge::AutoCommit::load(bytes)
                .map_err(|_| "scoped snapshot body does not parse")?;
            let changes = document.get_changes(&[]);
            if changes.len() > MAX_BODY_CHANGES {
                return Err("scoped snapshot body has too many changes".into());
            }
            Ok(changes.into_iter().map(|change| change.hash()).collect())
        }
        DocumentMutation::Incremental(bytes) => {
            const MAGIC: [u8; 4] = [0x85, 0x6f, 0x4a, 0x83];
            let mut hashes = BTreeSet::new();
            let mut rest = bytes.as_slice();
            while !rest.is_empty() {
                if hashes.len() >= MAX_BODY_CHANGES {
                    return Err("scoped incremental body has too many changes".into());
                }
                // magic(4) checksum(4) type(1) uleb128(len) data(len)
                if rest.len() < 9 || rest[..4] != MAGIC {
                    return Err("scoped incremental body has an invalid chunk header".into());
                }
                let (mut length, mut shift, mut at) = (0_u64, 0_u32, 9_usize);
                loop {
                    let byte = *rest.get(at).ok_or("scoped incremental chunk truncated")?;
                    at += 1;
                    if shift >= 63 {
                        return Err("scoped incremental chunk length overflow".into());
                    }
                    length |= u64::from(byte & 0x7f) << shift;
                    shift += 7;
                    if byte & 0x80 == 0 {
                        break;
                    }
                }
                let end = usize::try_from(length)
                    .ok()
                    .and_then(|length| at.checked_add(length))
                    .filter(|end| *end <= rest.len())
                    .ok_or("scoped incremental chunk truncated")?;
                let change = automerge::Change::try_from(&rest[..end])
                    .map_err(|_| "scoped incremental body contains a non-change chunk")?;
                if !hashes.insert(change.hash()) {
                    return Err("scoped incremental body repeats a change".into());
                }
                rest = &rest[end..];
            }
            Ok(hashes)
        }
    }
}

fn refused(reason: impl Into<String>) -> HostDocumentTurnError {
    HostDocumentTurnError::Refused(reason.into())
}

impl Store {
    pub(crate) fn commit_agent_document(
        &mut self,
        route: &RouteSpec,
        certificate: &DeviceCert,
        mutation: DocumentMutation,
    ) -> Result<(SignedOp, OwnerCommitReceipt), String> {
        self.commit_agent_document_checked(route, certificate, mutation, |_, _, _, _| Ok(()))
            .map_err(|error| match error {
                HostDocumentTurnError::Refused(reason)
                | HostDocumentTurnError::Uncertain(reason) => reason,
            })
    }

    /// Final scoped evaluator for a worker publication that carries original
    /// invocation custody. Every check runs under this held Store guard with
    /// its registered policy time, before any persistence write:
    ///
    /// - exact route/body binding of the captured authorization;
    /// - the change set parsed from the actual body equals the captured export,
    ///   and every unattributed change is already in committed owner history;
    /// - each contributor token is this host's original invocation, from the
    ///   same registered policy origin, with current worker custody, current
    ///   certificate, current workspace role, and for enrolled callers the
    ///   current task ledger grant (checked directly, no Store reopen).
    ///
    /// Refusals are definite zero-change outcomes. Only the durability barrier
    /// is uncertain, and that latches the handoff service.
    pub(crate) fn commit_scoped_agent_document(
        &mut self,
        route: &RouteSpec,
        certificate: &DeviceCert,
        mutation: DocumentMutation,
        authorization: &DocumentAuthorization,
    ) -> Result<(SignedOp, OwnerCommitReceipt), HostDocumentTurnError> {
        authorization
            .check_binding(route, &mutation)
            .map_err(HostDocumentTurnError::Refused)?;
        let actual = mutation_change_hashes(&mutation).map_err(HostDocumentTurnError::Refused)?;
        let store_time = self
            .policy_time
            .clone()
            .ok_or_else(|| refused("scoped document requires registered owner policy time"))?;
        let record = route.record.as_deref().unwrap_or("").to_owned();
        self.commit_agent_document_checked(
            route,
            certificate,
            mutation,
            |store, before, candidate, now_ns| {
                let bytes = before
                    .document(&route.app_id, &record)
                    .ok_or("committed worker document unavailable")?;
                let mut committed = automerge::AutoCommit::load(&bytes)
                    .map_err(|_| "committed worker document unavailable")?;
                let actual = actual.iter().copied().collect::<Vec<_>>();
                authorization.validate_owner_base(&actual, |hash| {
                    committed.get_change_by_hash(hash).is_some()
                })?;
                let now = crate::now_time(now_ns);
                authorization.validate_owner_contributors(|request, exact_route, token| {
                    validate_contributor(
                        store,
                        candidate,
                        route,
                        &record,
                        certificate,
                        &store_time,
                        now_ns,
                        now,
                        request,
                        exact_route,
                        token,
                    )
                })
            },
        )
    }

    fn commit_agent_document_checked(
        &mut self,
        route: &RouteSpec,
        certificate: &DeviceCert,
        mutation: DocumentMutation,
        check: impl FnOnce(&Store, &StatePeer, &StatePeer, u64) -> Result<(), String>,
    ) -> Result<(SignedOp, OwnerCommitReceipt), HostDocumentTurnError> {
        // Everything before the Store commit is a definite refusal with no write.
        let prepared = (|| -> Result<_, String> {
            let now = self.policy_now_ns().map_err(|e| e.to_string())?;
            let (before, mut candidate) =
                self.peer_transition_at(now).map_err(|e| e.to_string())?;
            let record = route.record.as_deref().unwrap_or("");
            if !candidate.admission_v5_selected()
                || candidate.incomplete_reason().is_some()
                || candidate.room() != route.cluster_id.to_string()
                || certificate != candidate.authority()
                || certificate.device_public_key != self.key().verifying_key().to_bytes()
                || !candidate.certificates().contains(certificate)
                || qos_session_peer::canonical_records::resolve(&route.app_id).is_none()
                || candidate.document(&route.app_id, record).is_none()
            {
                return Err(
                    "worker route, exact-admission writer or host certificate unavailable".into(),
                );
            }
            candidate.permissions(
                certificate.device_public_key,
                self.policy_now().map_err(|e| e.to_string())?,
            )?;
            check(self, &before, &candidate, now)?;
            // A snapshot is republished below as the route's single covered
            // checkpoint; publishing it here too would put an uncovered
            // checkpoint ahead of the covered one and stall members.
            let snapshot = matches!(mutation, DocumentMutation::Snapshot(_));
            let message = candidate.signed_mutation(
                &route.app_id,
                record,
                mutation,
                &self.key(),
                self.policy_now().map_err(|e| e.to_string())?,
            )?;
            let exact = candidate
                .lookup_exact_document(
                    TransportOrigin::RelayDelta(certificate.device_public_key),
                    &message,
                    self.policy_now().map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?
                .ok_or("worker mutation was not exactly admitted")?;
            if !candidate
                .admissions_since(&before)
                .map_err(|e| e.to_string())?
                .contains(&exact)
            {
                return Err("worker signer did not produce a new exact admission".into());
            }
            let NeighbourMsg::AppDocument { envelope, .. } = &message else {
                return Err("worker signer did not produce an application document".into());
            };
            let envelope = envelope.clone();
            crate::agent_live::capture_worker_commit(route, record, &message, &before, &candidate);
            let request = OwnerCommitRequest {
                room: self.state.room.clone(),
                operation: CommitOperation::Document {
                    app: route.app_id.clone(),
                    record: record.into(),
                    envelope: envelope.clone(),
                },
            };
            let binding = request.binding()?;
            let generation = self.next_generation().map_err(|e| e.to_string())?;
            let mut state = self.state.clone();
            if !snapshot {
                state
                    .outbox
                    .push(StoredPublication::mutation(message).map_err(|e| e.to_string())?);
            }
            state.outbox.extend(
                durable_mutation_publications(
                    &mut candidate,
                    &before,
                    &self.key(),
                    self.policy_now().map_err(|e| e.to_string())?,
                    generation,
                )
                .map_err(|e| e.to_string())?,
            );
            state
                .bound
                .as_mut()
                .ok_or("workspace no longer enrolled")?
                .peer = candidate.durable()?;
            Ok((state, request, binding, envelope))
        })();
        let (state, request, binding, envelope) =
            prepared.map_err(HostDocumentTurnError::Refused)?;
        let committed = (|| -> Result<_, String> {
            self.commit(state).map_err(|e| e.to_string())?;
            let receipt = OwnerCommitReceipt {
                binding,
                generation: self.state.generation,
                aggregate_hash: *blake3::hash(
                    &serde_json::to_vec(&self.state).map_err(|e| e.to_string())?,
                )
                .as_bytes(),
            };
            receipt.validate_for(&request)?;
            Ok((*envelope, receipt))
        })();
        committed.map_err(HostDocumentTurnError::Uncertain)
    }
}

/// The Cloud runtime's single final document door for queued worker handoffs.
/// Opens Store only inside the owner turn and releases it before completion.
pub(crate) fn apply_worker_document(
    owner: &crate::agent_runtime_adapter::RuntimeOwner,
    pending: qos_action_broker::host_documents::PendingHostDocument,
    deadline: std::time::Instant,
) -> (Box<dyn FnOnce() + Send>, Result<(), String>) {
    pending.apply_scoped_fenced(deadline, |route, certificate, mutation, authorization| {
        let began = std::time::Instant::now();
        let mut store = owner.store().map_err(HostDocumentTurnError::Refused)?;
        let open_us = qos_action_broker::agent_timing::us(began);
        let committed =
            crate::agent_live::on_worker_commit(owner, authorization, &mut store, |store| {
                match authorization {
                    Some(authorization) => store.commit_scoped_agent_document(
                        route,
                        certificate,
                        mutation,
                        authorization,
                    ),
                    None => store.commit_agent_document_checked(
                        route,
                        certificate,
                        mutation,
                        |_, _, _, _| Ok(()),
                    ),
                }
            });
        drop(store);
        qos_action_broker::agent_timing::span(
            "worker_document_commit",
            None,
            began.elapsed(),
            &[
                ("open_us", open_us),
                ("scoped", u64::from(authorization.is_some())),
            ],
        );
        committed
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "explicit protocol inputs stay separate parameters"
)]
fn validate_contributor(
    store: &Store,
    candidate: &StatePeer,
    route: &RouteSpec,
    record: &str,
    certificate: &DeviceCert,
    store_time: &qos_action_broker::peer_actions::PolicyTime,
    now_ns: u64,
    now: web_time::SystemTime,
    request: &ui::invocation_scope::PublishedInvocationRequest,
    exact_route: &RouteSpec,
    token: &(dyn std::any::Any + Send + Sync),
) -> Result<(), String> {
    let original = token
        .downcast_ref::<qos_action_broker::collab_broker_capnp::OriginalInvocation>()
        .ok_or("contributor custody was not minted by this Cloud host")?;
    if original.request() != request
        || exact_route != route
        || original.route().app_id != route.app_id
        || original.route().cluster_id != route.cluster_id.to_string()
        || original.route().record_id != record
    {
        return Err("contributor custody differs from its original invocation".into());
    }
    if !original.policy_time().same_origin(store_time) {
        return Err("contributor policy origin differs from the Store owner".into());
    }
    if original.worker_certificate() != certificate {
        return Err("contributor worker certificate differs from host signer".into());
    }
    // Pure custody: broker accepting, same private worker registration,
    // certificate valid at the original clock, enrolled connection current.
    original.check_custody()?;
    let caller = original.certificate()?;
    qos_user_identity::verify_device_cert_at(caller, now_ns)
        .map_err(|_| "contributor certificate expired")?;
    if let Some(dispatch) = original.enrolled_dispatch() {
        // The task ledger in this held Store is the enrolled authority.
        return store
            .authorize_agent_task_action(
                dispatch,
                &route.app_id,
                record,
                &request.address.resource,
                &request.address.action,
            )
            .map_err(|error| error.to_string());
    }
    if !candidate.certificates().contains(caller) {
        return Err("contributor certificate is no longer admitted".into());
    }
    let (role, _, _) = candidate.permissions(caller.device_public_key, now)?;
    use collab_types::Role;
    use qos_actions::PermissionRequirement as P;
    let allowed = match original.contract().permission {
        P::None | P::Read => !matches!(role, Role::Denied),
        P::Comment => matches!(role, Role::Owner | Role::Editor | Role::Commenter),
        P::Write => matches!(role, Role::Owner | Role::Editor),
        P::Invite | P::Admin => matches!(role, Role::Owner),
    };
    if !allowed {
        return Err("contributor role no longer permits this invocation".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use qos_peer_state::owner_commit::OwnerCommitService;
    use std::sync::Arc;

    #[test]
    fn scoped_body_change_set_is_parsed_from_exact_bytes() {
        use automerge::{ROOT, transaction::Transactable};
        let mut document = automerge::AutoCommit::new();
        document.put(ROOT, "a", 1).unwrap();
        document.commit();
        let base = document.get_heads();
        document.put(ROOT, "b", 2).unwrap();
        document.commit();
        document.put(ROOT, "c", 3).unwrap();
        document.commit();
        let all: BTreeSet<_> = document.get_changes(&[]).iter().map(|c| c.hash()).collect();
        let after: BTreeSet<_> = document
            .get_changes(&base)
            .iter()
            .map(|c| c.hash())
            .collect();
        assert_eq!(after.len(), 2);
        assert_eq!(
            mutation_change_hashes(&DocumentMutation::Snapshot(document.save())).unwrap(),
            all
        );
        let incremental = document.save_after(&base);
        assert_eq!(
            mutation_change_hashes(&DocumentMutation::Incremental(incremental.clone())).unwrap(),
            after
        );
        assert!(
            mutation_change_hashes(&DocumentMutation::Incremental(vec![]))
                .unwrap()
                .is_empty()
        );
        for broken in [
            incremental[..incremental.len() - 1].to_vec(),
            [incremental.clone(), vec![0x85]].concat(),
            [incremental.clone(), incremental.clone()].concat(),
            document.save(),
            b"not automerge".to_vec(),
        ] {
            assert!(mutation_change_hashes(&DocumentMutation::Incremental(broken)).is_err());
        }
        assert!(mutation_change_hashes(&DocumentMutation::Snapshot(b"junk".to_vec())).is_err());
    }

    fn selected() -> (tempfile::TempDir, Store, RouteSpec, DeviceCert) {
        let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
        let mut peer = store.peer().unwrap();
        peer.select_admission_v5().unwrap();
        let mut state = store.state.clone();
        state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
        store.commit(state).unwrap();
        let route = RouteSpec::app("so.tilo.chat", store.state.room.parse().unwrap());
        let cert = store.state.bound.as_ref().unwrap().authority.clone();
        (dir, store, route, cert)
    }

    #[test]
    fn cloud_policy_clock_retired_source_cannot_commit_pending_document() {
        let (dir, mut store, route, cert) = selected();
        let mutation =
            DocumentMutation::Snapshot(store.peer().unwrap().document("so.tilo.chat", "").unwrap());
        let clock = qos_action_broker::peer_actions::OwnerPolicyClock::new().unwrap();
        let origin = clock.new_origin();
        store.bind_policy_time(origin.capture().unwrap()).unwrap();
        let before = std::fs::read(dir.path().join("state.json")).unwrap();
        let generation = store.state.generation;
        origin.invalidate();
        assert!(
            store
                .commit_agent_document(&route, &cert, mutation)
                .is_err(),
            "retired queued document performed its durable effect"
        );
        assert_eq!(store.state.generation, generation);
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            before
        );
        assert!(
            store.peer().is_err(),
            "failed Some context fell back to raw wall"
        );
    }

    #[test]
    fn cloud_policy_clock_store_cannot_replace_retired_origin() {
        let (_dir, mut store, _, _) = selected();
        let clock = qos_action_broker::peer_actions::OwnerPolicyClock::new().unwrap();
        let original = clock.new_origin();
        store.bind_policy_time(original.capture().unwrap()).unwrap();
        original.invalidate();
        let replacement = clock.new_origin();
        assert!(
            store
                .bind_policy_time(replacement.capture().unwrap())
                .is_err(),
            "retired Store context was replaced by a new origin"
        );
        assert!(store.peer().is_err());
    }

    fn timed_writer() -> (tempfile::TempDir, Store, DeviceCert, u64, u64) {
        use automerge::{ROOT, ReadDoc, ScalarValue, transaction::Transactable};
        use collab_types::caps::{CapOverride, Expiry, Subject, Verb};
        let (dir, mut store, _, cert) = selected();
        let raw = qos_relay::clock::now_ns();
        let expiry = raw / 1_000_000_000 + 100;
        let peer = store.peer().unwrap();
        let mut doc = peer.admitted_room().unwrap().clone();
        doc.set_actor(automerge::ActorId::from(
            store.key().verifying_key().to_bytes().to_vec(),
        ));
        doc.put(ROOT, "owner", hex::encode([9; 32])).unwrap();
        let caps = doc.get(ROOT, "capabilities").unwrap().unwrap().1;
        let keys: Vec<_> = doc.keys(&caps).collect();
        for key in keys {
            doc.delete(&caps, key).unwrap();
        }
        for (id, cap) in [
            (
                "membership",
                CapOverride::membership(
                    Subject::User(collab_types::Ed25519PublicKey(cert.user_public_key)),
                    &store.state.room,
                    Verb::Read,
                ),
            ),
            (
                "temporary-writer",
                CapOverride::membership(
                    Subject::User(collab_types::Ed25519PublicKey(cert.user_public_key)),
                    &store.state.room,
                    Verb::Write,
                )
                .expiring(Expiry::AtUnix(expiry)),
            ),
        ] {
            doc.put(
                &caps,
                id,
                ScalarValue::Bytes(serde_json::to_vec(&cap).unwrap()),
            )
            .unwrap();
        }
        doc.commit();
        store
            .receive(
                TransportOrigin::RelayDelta(store.key().verifying_key().to_bytes()),
                NeighbourMsg::SyncResp {
                    room: store.state.room.clone(),
                    changes: doc
                        .get_changes(&[])
                        .into_iter()
                        .map(|c| c.raw_bytes().to_vec())
                        .collect(),
                },
                raw,
            )
            .unwrap();
        let peer = store.peer_at(raw).unwrap();
        assert_eq!(
            peer.permissions(cert.device_public_key, now_time(raw))
                .unwrap()
                .0,
            collab_types::Role::Editor
        );
        assert_eq!(
            peer.permissions(
                cert.device_public_key,
                now_time((expiry + 50) * 1_000_000_000)
            )
            .unwrap()
            .0,
            collab_types::Role::Viewer
        );
        (dir, store, cert, raw, expiry)
    }

    #[test]
    fn cloud_policy_clock_expired_writer_raw_rollback_causes_no_effect() {
        let (dir, mut store, cert, raw, expiry) = timed_writer();
        let (clock, control) = qos_action_broker::peer_actions::OwnerPolicyClock::for_test(
            (expiry + 50) * 1_000_000_000,
        );
        let source = clock.new_origin();
        store.bind_policy_time(source.capture().unwrap()).unwrap();
        control.set_wall_ns(raw); // Raw backend wall is before the grant expiry.
        let before = std::fs::read(dir.path().join("state.json")).unwrap();
        let action = ActionRequest {
            version: 1,
            room: store.state.room.clone(),
            invocation_id: "expired-original-invocation".into(),
            operation: NativeOperation::WriterInsert {
                text: "MUST NOT WRITE".into(),
            },
        };
        let result = store.action(cert.device_public_key, &action, raw);
        control.set_wall_ns((expiry + 51) * 1_000_000_000); // Wall recovers before final output.
        assert!(
            result.is_err(),
            "expired permission performed actual Writer effect: {result:?}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            before
        );
        assert!(
            !store
                .state
                .receipts
                .keys()
                .any(|key| key.contains("expired-original-invocation"))
        );
    }

    #[test]
    fn cloud_policy_clock_writer_progress_then_queued_expiry() {
        let (dir, mut store, cert, raw, expiry) = timed_writer();
        let (clock, control) = qos_action_broker::peer_actions::OwnerPolicyClock::for_test(raw);
        let source = clock.new_origin();
        store.bind_policy_time(source.capture().unwrap()).unwrap();
        let before = store
            .peer()
            .unwrap()
            .document("so.tilo.word-processor", "tab:0000000000000001")
            .unwrap();
        let mut action = ActionRequest {
            version: 1,
            room: store.state.room.clone(),
            invocation_id: "valid-clock-progress".into(),
            operation: NativeOperation::WriterInsert {
                text: "allowed".into(),
            },
        };
        assert_eq!(
            store
                .action(cert.device_public_key, &action, raw)
                .unwrap()
                .status,
            "completed"
        );
        assert_ne!(
            store
                .peer()
                .unwrap()
                .document("so.tilo.word-processor", "tab:0000000000000001")
                .unwrap(),
            before
        );
        let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
        action.invocation_id = "expired-queued-writer".into();
        control.advance_monotonic(std::time::Duration::from_nanos(
            (expiry + 1) * 1_000_000_000 - raw,
        ));
        control.set_wall_ns(raw);
        assert!(store.action(cert.device_public_key, &action, raw).is_err());
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
    }

    #[tokio::test]
    async fn cloud_policy_clock_actual_document_queue_retirement() {
        let (dir, store, route, cert) = selected();
        let mutation =
            DocumentMutation::Snapshot(store.peer().unwrap().document("so.tilo.chat", "").unwrap());
        let bytes = std::fs::read(dir.path().join("state.json")).unwrap();
        drop(store);
        let owner = Arc::new(
            crate::agent_runtime_adapter::RuntimeOwner::open(dir.path(), "agent-admission")
                .unwrap(),
        );
        let source = owner.policy_context().clock().new_origin();
        owner
            .policy_context()
            .bind_source(source.capture().unwrap())
            .unwrap();
        let (documents, mut requests) =
            qos_action_broker::host_documents::HostDocuments::new(Arc::new(owner.ordinary()));
        let signer = documents.bind(route.clone(), cert);
        let task = tokio::spawn(signer.commit(&route, mutation));
        let pending = requests.recv().await.unwrap();
        assert_eq!(documents.outstanding_handoffs(), 1);
        source.invalidate();
        let (complete, outcome) = pending.apply(
            std::time::Instant::now() + std::time::Duration::from_secs(5),
            |route, cert, mutation| owner.store()?.commit_agent_document(route, cert, mutation),
        );
        assert!(outcome.is_err());
        complete();
        assert!(task.await.unwrap().is_err());
        assert_eq!(std::fs::read(dir.path().join("state.json")).unwrap(), bytes);
        assert_eq!(documents.outstanding_handoffs(), 0);
        assert_eq!(documents.pending_exact_receipts().unwrap(), 0);
    }

    #[test]
    fn agent_runtime_document_refuses_unselected_writer_and_wrong_host_without_write() {
        let (dir, mut store, device) = crate::agent_owner_tests::enrolled();
        let route = RouteSpec::app("so.tilo.chat", store.state.room.parse().unwrap());
        let cert = store.state.bound.as_ref().unwrap().authority.clone();
        let mutation =
            DocumentMutation::Snapshot(store.peer().unwrap().document("so.tilo.chat", "").unwrap());
        let before = std::fs::read(dir.path().join("state.json")).unwrap();
        assert!(
            store
                .commit_agent_document(&route, &cert, mutation)
                .is_err()
        );
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            before
        );
        drop(store);
        let (dir, mut store, route, _) = selected();
        let caller = store.agent_caller(device).unwrap().certificate().clone();
        let mutation =
            DocumentMutation::Snapshot(store.peer().unwrap().document("so.tilo.chat", "").unwrap());
        let before = std::fs::read(dir.path().join("state.json")).unwrap();
        assert!(
            store
                .commit_agent_document(&route, &caller, mutation)
                .is_err()
        );
        assert_eq!(
            std::fs::read(dir.path().join("state.json")).unwrap(),
            before
        );
    }

    #[test]
    fn agent_runtime_document_uncertain_barrier_has_no_receipt_and_latches_owner() {
        use atomic_file::test_support::Checkpoint;
        for checkpoint in [Checkpoint::BeforePublish, Checkpoint::BeforeCommit] {
            let (dir, mut store, route, cert) = selected();
            let mutation = DocumentMutation::Snapshot(
                store.peer().unwrap().document("so.tilo.chat", "").unwrap(),
            );
            let result =
                crate::storage::test_support::with_save_failure(dir.path(), checkpoint, || {
                    store.commit_agent_document(&route, &cert, mutation)
                });
            assert!(result.is_err());
            assert!(store.peer().is_err(), "uncertain owner became usable");
        }
    }

    #[tokio::test]
    async fn agent_runtime_document_real_store_barrier_and_shared_handoff() {
        let (dir, store, route, cert) = selected();
        let mutation =
            DocumentMutation::Snapshot(store.peer().unwrap().document("so.tilo.chat", "").unwrap());
        let before = store.state.generation;
        drop(store);
        let ordinary =
            crate::agent_owner::CloudAgentAuthority::open(dir.path(), "agent-admission").unwrap();
        let (documents, mut requests) =
            qos_action_broker::host_documents::HostDocuments::new(Arc::new(ordinary));
        let signer = documents.bind(route.clone(), cert.clone());
        let task = tokio::spawn(signer.commit(&route, mutation));
        let pending = requests.recv().await.unwrap();
        let (complete, outcome) = pending.apply(
            std::time::Instant::now() + std::time::Duration::from_secs(5),
            |route, cert, mutation| {
                Store::open(dir.path(), "agent-admission")
                    .map_err(|e| e.to_string())?
                    .commit_agent_document(route, cert, mutation)
            },
        );
        outcome.unwrap();
        assert!(
            !task.is_finished(),
            "deferred response precedes host refresh"
        );
        complete();
        let envelope = task.await.unwrap().unwrap();
        let request = OwnerCommitRequest {
            room: route.cluster_id.to_string(),
            operation: CommitOperation::Document {
                app: route.app_id,
                record: String::new(),
                envelope: Box::new(envelope),
            },
        };
        let receipt = documents.commit(request.clone()).await.unwrap();
        receipt.validate_for(&request).unwrap();
        assert!(
            documents.commit(request).await.is_err(),
            "one handoff was consumed twice"
        );
        let store = Store::open(dir.path(), "agent-admission").unwrap();
        assert_eq!(store.state.generation, before + 1);
        assert_eq!(receipt.generation, store.state.generation);
        assert_eq!(
            receipt.aggregate_hash,
            *blake3::hash(&serde_json::to_vec(&store.state).unwrap()).as_bytes()
        );
        let peer = store.state.bound.as_ref().unwrap().peer.clone();
        assert!(
            store
                .state
                .outbox
                .iter()
                .any(|p| matches!(p.message, NeighbourMsg::AppDocument { .. })
                    && matches!(p.kind, PublicationKind::Delta))
        );
        drop(store);
        let reopened = Store::open(dir.path(), "agent-admission").unwrap();
        assert_eq!(reopened.state.bound.as_ref().unwrap().peer, peer);
        assert_eq!(reopened.state.generation, receipt.generation);
    }

    /// A worker's snapshot turn hands members exactly one checkpoint of the
    /// record, the covered one: a member without any skip for superseded
    /// checkpoints admits every publication and reaches the new state.
    /// An incremental turn still publishes its delta.
    #[test]
    fn worker_snapshot_turn_publishes_one_covered_checkpoint() {
        use automerge::{ROOT, transaction::Transactable};
        let (_dir, mut store, route, cert) = selected();
        let now = store.policy_now_ns().unwrap();
        let host = store.peer().unwrap();
        let authority = host.authority().clone();
        let mut member = StatePeer::new(
            &store.state.room,
            authority.clone(),
            host.certificates(),
            qos_user_identity::RevocationList::new(),
            now_time(now),
        )
        .unwrap();
        // The member starts from the room this Store committed (its older
        // enrollment publications are outside their attestation window).
        for publication in &store.state.outbox {
            let _ = member.receive_from(
                TransportOrigin::RelayDelta(authority.device_public_key),
                publication.message.clone(),
                now_time(now),
            );
        }
        let checkpoints = |published: &[NeighbourMsg]| {
            published
                .iter()
                .filter(|m| matches!(m, NeighbourMsg::AppDocument { app_id, .. } if app_id == "so.tilo.chat"))
                .count()
        };
        let mut edit = |store: &mut Store, snapshot: bool, value: &str| {
            let bytes = store.peer().unwrap().document("so.tilo.chat", "").unwrap();
            let mut doc = automerge::AutoCommit::load(&bytes).unwrap();
            let heads = doc.get_heads();
            doc.put(ROOT, "x_regression", value).unwrap();
            let mutation = if snapshot {
                DocumentMutation::Snapshot(doc.save())
            } else {
                DocumentMutation::Incremental(doc.save_after(&heads))
            };
            let before = store.state.outbox.len();
            store
                .commit_agent_document(&route, &cert, mutation)
                .unwrap();
            store.state.outbox[before..]
                .iter()
                .map(|publication| publication.message.clone())
                .collect::<Vec<_>>()
        };
        for (snapshot, value, most) in [(true, "a", 1), (false, "b", 2), (true, "c", 1)] {
            let published = edit(&mut store, snapshot, value);
            assert!(
                checkpoints(&published) <= most,
                "snapshot={snapshot}: {} chat publications",
                checkpoints(&published)
            );
            let at = store.policy_now_ns().unwrap();
            // Strict: every publication of the turn is admitted.
            for message in published {
                member
                    .receive_from(
                        TransportOrigin::RelayDelta(authority.device_public_key),
                        message,
                        now_time(at),
                    )
                    .unwrap();
            }
            assert_eq!(
                member.durable_covers_current("so.tilo.chat", ""),
                Ok(true),
                "member reaches the covered state after snapshot={snapshot}"
            );
            assert_eq!(
                member.document("so.tilo.chat", ""),
                store.peer().unwrap().document("so.tilo.chat", "")
            );
        }
    }
}
