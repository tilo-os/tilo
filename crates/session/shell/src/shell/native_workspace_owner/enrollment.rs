use super::*;
use crate::network::invite::{VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent};
use record::{StoredPendingEnrollment, StoredRoom};
use tilo_user_identity::RevocationList;

pub(super) fn validate_pending(
    room: RoomId,
    pending: &StoredPendingEnrollment,
) -> Result<(), String> {
    let intent = VerifiedNativeEnrollmentIntent::restore(&pending.intent)?;
    if room != pending.id.room()
        || room != intent.room()
        || pending.id.digest() != intent.proof_digest()
        || pending.id.generation() == 0
    {
        return Err("pending enrollment scope mismatch".into());
    }
    Ok(())
}
pub(super) fn restore(
    room: RoomId,
    record: &StoredRoom,
    now: SystemTime,
) -> Result<NativeCommittedView, String> {
    if !matches!(record.version, 1 | 2)
        || room != record.room
        || record.generation == 0
        || record.continuity == 0
    {
        return Err("invalid native committed room record".into());
    }
    let intent = VerifiedNativeEnrollmentIntent::restore(&record.enrollment_intent)?;
    let grant = intent.verified_join().grant();
    if intent.room() != room
        || record.governor != grant.authority
        || record.workspace != grant.workspace_id
        || record.device != *intent.local_device()
        || record.transport_hint != *intent.transport_hint()
        || record.invitation != intent.verified_join().signed_invitation()
    {
        return Err("native committed room differs from its authenticated enrollment".into());
    }
    if record.host.as_ref() != intent.delivery_host()
        || record.locator.as_ref() != intent.delivery_locator()
    {
        return Err("native service pin differs from its unique signed enrollment locator".into());
    }
    if record.host.is_none() && (record.hint.is_some() || record.floor.is_some()) {
        return Err("native descriptor has no independent service pin".into());
    }
    if record.hint.is_some() != record.floor.is_some() {
        return Err("native persisted descriptor acknowledgement is incomplete".into());
    }
    if let (Some(hint), Some(host)) = (&record.hint, &record.host) {
        let metadata = tilo_relay::delivery_v3::descriptor::inspect_persisted_hint(hint, host)
            .map_err(|e| e.to_string())?;
        if record.floor.as_ref().is_some_and(|floor| {
            floor.generation != metadata.generation || floor.digest != metadata.descriptor_digest
        }) {
            return Err("native persisted descriptor/floor mismatch".into());
        }
    }
    let peer = StatePeer::restore(
        &record.peer,
        &room.to_string(),
        record.governor.clone(),
        vec![record.device.clone()],
        RevocationList::new(),
        now,
    )?;
    if !peer.admission_v5_selected() {
        return Err("native committed room requires durable admission v5".into());
    }
    let view = NativeCommittedView {
        peer,
        record: record.clone(),
    };
    publication::validate(&view, now)?;
    Ok(view)
}
impl NativeWorkspaceOwner {
    pub(crate) fn prepare_enrollment(
        &self,
        intent: &VerifiedNativeEnrollmentIntent,
        now: SystemTime,
    ) -> Result<PendingEnrollmentId, String> {
        let local = self
            .0
            .tree
            .read()
            .workspaces
            .iter()
            .find(|slot| slot.room == intent.room())
            .map(|slot| slot.id)
            .ok_or("enrollment requires an explicit existing local slot")?;
        self.prepare_enrollment_into(intent, local, now)
    }
    pub(crate) fn prepare_enrollment_into(
        &self,
        intent: &VerifiedNativeEnrollmentIntent,
        local: super::super::state::WorkspaceId,
        _now: SystemTime,
    ) -> Result<PendingEnrollmentId, String> {
        let bytes = intent.durable_bytes()?;
        if intent.complete_bytes().len()
            > tilo_relay::workspace_join::MAX_WORKSPACE_JOIN_REQUEST_BYTES
        {
            return Err("native Complete exceeds enrollment refusal cap".into());
        }
        // Round trip only through B's checked historical proof decoder.
        let checked = VerifiedNativeEnrollmentIntent::restore(&bytes)?;
        if checked.proof_digest() != intent.proof_digest() || checked.room() != intent.room() {
            return Err("native intent round-trip mismatch".into());
        }
        let _turn = self.0.mutation.lock();
        let room = intent.room();
        {
            let current = self.0.current.read();
            if current.fenced || current.denied.contains(&room) || !self.0.runtime.healthy() {
                return Err("native enrollment custody is fenced".into());
            }
        }
        let mut tree = self.0.tree.write();
        if tree.native_authority.rooms.contains_key(&room) {
            return Err(
                "room is already enrolled; identity replacement requires explicit custody handoff"
                    .into(),
            );
        }
        let prior_room = tree
            .room_of(local)
            .ok_or("enrollment local slot no longer exists")?;
        if tree
            .native_authority
            .pending
            .values()
            .any(|pending| pending.target.workspace == local && pending.id.room() != room)
        {
            return Err("local slot already has an unresolved enrollment proof".into());
        }

        if let Some(pending) = tree.native_authority.pending.get(&room) {
            if pending.intent == bytes
                && pending.status == PendingEnrollmentStatus::Active
                && pending.target.workspace == local
                && pending.target.prior_room == prior_room
            {
                return Ok(pending.id.clone());
            }
            return Err("room already has an unresolved enrollment proof".into());
        }
        if tree.native_authority.pending.len() >= store::MAX_PENDING {
            return Err("native pending enrollment quota exhausted".into());
        }
        let generation = tree
            .native_authority
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        let id = PendingEnrollmentId::new(room, generation, intent.proof_digest());
        store::preflight(&*tree, store::MAX_STATE_BYTES).map_err(|e| e.to_string())?;
        let mut next = tree.clone();
        next.native_authority.revision = generation;
        next.native_authority.pending.insert(
            room,
            StoredPendingEnrollment {
                target: record::EnrollmentTarget {
                    workspace: local,
                    prior_room,
                },
                id: id.clone(),
                intent: bytes,
                status: PendingEnrollmentStatus::Active,
            },
        );
        self.publish_storage(&mut tree, next)?;
        self.0.current.write().required.insert(room);
        Ok(id)
    }
    pub(crate) fn pending_enrollment(
        &self,
        room: RoomId,
    ) -> Result<Option<StoredPendingEnrollment>, String> {
        if !self.0.runtime.healthy() {
            return Err("native state writer is fenced".into());
        }
        Ok(self
            .0
            .tree
            .read()
            .native_authority
            .pending
            .get(&room)
            .cloned())
    }
    pub(crate) fn pending_enrollment_for_slot(
        &self,
        local: super::super::state::WorkspaceId,
    ) -> Result<Option<StoredPendingEnrollment>, String> {
        if !self.0.runtime.healthy() {
            return Err("native state writer is fenced".into());
        }
        let tree = self.0.tree.read();
        if tree.native_authority.pending.len() > store::MAX_PENDING {
            return Err("native pending enrollment quota exceeded".into());
        }
        let mut found = tree
            .native_authority
            .pending
            .values()
            .filter(|pending| pending.target.workspace == local);
        let result = found.next().cloned();
        if found.next().is_some() {
            return Err("ambiguous native pending enrollment slot".into());
        }
        Ok(result)
    }
    pub(crate) fn cancel_enrollment(
        &self,
        room: RoomId,
        id: &PendingEnrollmentId,
    ) -> Result<bool, String> {
        let _turn = self.0.mutation.lock();
        let mut tree = self.0.tree.write();
        let Some(pending) = tree.native_authority.pending.get(&room) else {
            return Ok(false);
        };
        if &pending.id != id {
            return Err("enrollment cancellation generation mismatch".into());
        }
        if pending.status == PendingEnrollmentStatus::Cancelled {
            return Ok(false);
        }
        store::preflight(&*tree, store::MAX_STATE_BYTES).map_err(|e| e.to_string())?;
        let mut next = tree.clone();
        next.native_authority.revision = next
            .native_authority
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        next.native_authority
            .pending
            .get_mut(&room)
            .expect("checked pending")
            .status = PendingEnrollmentStatus::Cancelled;
        self.publish_storage(&mut tree, next)?;
        Ok(true)
    }
    fn publish_storage(
        &self,
        tree: &mut super::super::state::WorkspaceTree,
        next: super::super::state::WorkspaceTree,
    ) -> Result<(), String> {
        let path = tree
            .persist_path
            .as_ref()
            .ok_or("native owner state-file path removed")?;
        let bytes = persist::encode_tree(&next).map_err(|e| e.to_string())?;
        tree.native_runtime
            .publish(path, &tree.native_authority, &next.native_authority, &bytes)
            .map_err(|e| e.to_string())?;
        *tree = next;
        Ok(())
    }
    pub(crate) fn install(
        &self,
        enrollment: &VerifiedNativeEnrollment,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let room = RoomId::parse(&enrollment.bootstrap().room)
            .ok_or("noncanonical native enrollment room")?;
        let pending = self
            .0
            .tree
            .read()
            .native_authority
            .pending
            .get(&room)
            .cloned()
            .ok_or("native Complete proof was not persisted before transmission")?;
        if pending.id != *enrollment.pending_id() {
            return Err("native Complete belongs to a replaced enrollment intent".into());
        }
        if pending.status != PendingEnrollmentStatus::Active {
            return Err("cancelled enrollment cannot install a late Complete".into());
        }
        let intent = VerifiedNativeEnrollmentIntent::restore(&pending.intent)?;
        if intent.verified_join() != enrollment.verified_join()
            || intent.local_device() != enrollment.local_device()
            || intent.transport_hint() != enrollment.transport_hint()
        {
            return Err("native Complete differs from persisted enrollment intent".into());
        }
        let bootstrap = enrollment.bootstrap();
        let mut revocations = RevocationList::new();
        for entry in &bootstrap.revocations {
            revocations
                .insert(entry.clone())
                .map_err(|e| e.to_string())?;
        }
        let mut peer = StatePeer::new(
            &bootstrap.room,
            bootstrap.authority.clone(),
            bootstrap.certified_roster.clone(),
            revocations,
            now,
        )?;
        peer.select_admission_v5().map_err(|e| e.to_string())?;
        // Only B's closed, pinned Complete evidence selects historical bootstrap
        // admission. Ongoing relay input uses strict receive_from_report instead.
        // This legacy enum labels canonical admission context, not fabricated
        // gossip provenance or a claim that Complete arrived as a relay frame.
        for bytes in &bootstrap.governance_messages {
            let message: NeighbourMsg = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
            peer.receive_from(
                TransportOrigin::RetainedRelayDelta(bootstrap.authority.device_public_key),
                message,
                now,
            )?;
        }
        if !peer.has_governance() {
            return Err("native enrollment is missing real admitted governance".into());
        }
        let (role, _, _) = peer.permissions(enrollment.local_device().device_public_key, now)?;
        let expected = match enrollment.verified_join().grant().role {
            qos_peer_state::join::MemberRole::Editor => collab_types::Role::Editor,
            qos_peer_state::join::MemberRole::Commenter => collab_types::Role::Commenter,
            qos_peer_state::join::MemberRole::Viewer => collab_types::Role::Viewer,
        };
        if role != expected {
            return Err("actual admitted role differs from signed enrollment role".into());
        }

        let record = StoredRoom {
            version: 2,
            room,
            workspace: bootstrap.workspace_id,
            governor: bootstrap.authority.clone(),
            device: enrollment.local_device().clone(),
            invitation: enrollment.verified_join().signed_invitation().to_vec(),
            enrollment_intent: pending.intent.clone(),
            enrolled_at_ns: ns(now)?,
            transport_hint: enrollment.transport_hint().clone(),
            host: intent.delivery_host().cloned(),
            locator: intent.delivery_locator().cloned(),
            hint: enrollment.delivery_hint().cloned(),
            peer: peer.durable()?,
            generation: 1,
            continuity: 0,
            floor: None,
            publication: Some(publication::StoredPublication::default()),
        };
        self.publish_room(room, NativeCommittedView { peer, record }, Some(&pending))
    }
    pub(crate) fn commit_document(
        &self,
        request: qos_peer_state::owner_commit::OwnerCommitRequest,
        now: SystemTime,
    ) -> Result<qos_peer_state::owner_commit::OwnerCommitReceipt, String> {
        use qos_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt};
        let outgoing = publication::prepare(&request)?;
        let binding = request.binding()?;
        let room = RoomId::parse(&request.room).ok_or("invalid native document room")?;
        let CommitOperation::Document {
            app,
            record,
            envelope,
        } = request.operation
        else {
            return Err("unsupported native owner commit family".into());
        };
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        let device = candidate.device().device_public_key;
        if envelope.author.as_bytes() != &device {
            return Err("native local publication signer differs from selected device".into());
        }
        if candidate
            .peer
            .lookup_exact_local_document(&app, &record, &envelope, now)
            .map_err(|e| e.to_string())?
            .is_some()
        {
            if !publication::has_exact(&candidate, &outgoing)?
                && !publication::retry_covered(&candidate, &outgoing)?
            {
                return Err("exact local admission has no retained publication custody".into());
            }
            return Ok(OwnerCommitReceipt {
                binding,
                generation: candidate.generation(),
                aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
            });
        }
        publication::reserve(&candidate, &outgoing)?;
        let before = candidate.peer.clone();
        let outcome = candidate
            .peer
            .admit_local_app_document(&app, &record, *envelope, now)?;
        if outcome.deferred
            || outcome.incomplete.is_some()
            || outcome.rejected != 0
            || outcome.duplicate
        {
            return Err("native local document lacks a newly admitted exact receipt".into());
        }
        let admissions = candidate
            .peer
            .admissions_since(&before)
            .map_err(|e| e.to_string())?;
        if !admissions.iter().any(|entry| {
            entry.room() == room.to_string()
                && entry.app() == app
                && entry.record() == record
                && entry.author() == device
        }) {
            return Err("native local document has no exact admission evidence".into());
        }
        publication::append_local(&mut candidate, outgoing, now)?;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = candidate
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        let receipt = OwnerCommitReceipt {
            binding,
            generation: candidate.generation(),
            aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
        };
        self.publish_room(room, candidate, None)?;
        Ok(receipt)
    }
}
pub(super) fn ns(now: SystemTime) -> Result<u64, String> {
    u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos(),
    )
    .map_err(|e| e.to_string())
}
