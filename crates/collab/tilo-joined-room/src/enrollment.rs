use crate::{
    Change, NativeAuthorityStorage, NativeCommittedView, NativeWorkspaceOwner, NeighbourMsg,
    PendingEnrollmentId, PendingEnrollmentStatus, RoomId, StatePeer, SystemTime, TransportOrigin,
    VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent, WorkspaceId, limits, publication,
    record,
};
use record::{StoredPendingEnrollment, StoredRoom};
use tilo_user_identity::RevocationList;

pub(crate) fn validate_pending(
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
#[allow(
    clippy::suspicious_operation_groupings,
    reason = "a persisted floor stores the descriptor digest as `digest`"
)]
pub(crate) fn restore(
    room: RoomId,
    record: &StoredRoom,
    now: SystemTime,
) -> Result<NativeCommittedView, String> {
    if !matches!(record.version, 2 | 3)
        || (record.version == 3) != record.effect_history_root.is_some()
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
        let metadata = tilo_delivery_protocol::descriptor::inspect_persisted_hint(hint, host)
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
    publication::validate(&view)?;
    crate::effect_receipt::validate(&view)?;
    Ok(view)
}
impl NativeWorkspaceOwner {
    /// Persist the signed Complete proof before it is sent, so a lost
    /// response is retried with the exact same proof. `local` is the
    /// workspace slot the enrolled room will be shown in, where the host has
    /// slots.
    ///
    /// # Errors
    /// The owner is fenced, the room is already enrolled or has another
    /// proof in flight, the slot is gone or the record cannot be published.
    pub fn prepare_enrollment_into(
        &self,
        intent: &VerifiedNativeEnrollmentIntent,
        local: Option<WorkspaceId>,
        _now: SystemTime,
    ) -> Result<PendingEnrollmentId, String> {
        let bytes = intent.durable_bytes()?;
        if intent.complete_bytes().len()
            > tilo_delivery_protocol::workspace_join::MAX_WORKSPACE_JOIN_REQUEST_BYTES
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
            if current.fenced || current.denied.contains(&room) || !self.0.store.healthy() {
                return Err("native enrollment custody is fenced".into());
            }
        }
        let mut next = self.0.authority.read().clone();
        if next.rooms.contains_key(&room) {
            return Err(
                "room is already enrolled; identity replacement requires explicit custody handoff"
                    .into(),
            );
        }
        let target = local
            .map(|workspace| {
                self.0
                    .store
                    .slot_room(workspace)
                    .map(|prior_room| record::EnrollmentTarget {
                        workspace,
                        prior_room,
                    })
                    .ok_or("enrollment local slot no longer exists")
            })
            .transpose()?;
        if let Some(target) = &target
            && next.pending.values().any(|pending| {
                pending
                    .target
                    .as_ref()
                    .is_some_and(|other| other.workspace == target.workspace)
                    && pending.id.room() != room
            })
        {
            return Err("local slot already has an unresolved enrollment proof".into());
        }

        if let Some(pending) = next.pending.get(&room) {
            let same_target = match (&pending.target, &target) {
                (Some(stored), Some(target)) => {
                    stored.workspace == target.workspace && stored.prior_room == target.prior_room
                }
                (None, None) => true,
                _ => false,
            };
            if pending.intent == bytes
                && pending.status == PendingEnrollmentStatus::Active
                && same_target
            {
                return Ok(pending.id.clone());
            }
            return Err("room already has an unresolved enrollment proof".into());
        }
        if next.pending.len() >= limits::MAX_PENDING {
            return Err("native pending enrollment quota exhausted".into());
        }
        let generation = next
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        let id = PendingEnrollmentId::new(room, generation, intent.proof_digest());
        next.revision = generation;
        next.pending.insert(
            room,
            StoredPendingEnrollment {
                target,
                id: id.clone(),
                intent: bytes,
                status: PendingEnrollmentStatus::Active,
            },
        );
        self.publish_pending(&next, Some(room))?;
        Ok(id)
    }
    /// # Errors
    /// The store is fenced.
    pub fn pending_enrollment(
        &self,
        room: RoomId,
    ) -> Result<Option<StoredPendingEnrollment>, String> {
        if !self.0.store.healthy() {
            return Err("native state writer is fenced".into());
        }
        Ok(self.0.authority.read().pending.get(&room).cloned())
    }
    /// The in-flight enrollment redeeming `invitation`, where the host has
    /// no slots to find it by.
    ///
    /// # Errors
    /// The store is fenced or a retained proof no longer decodes.
    pub fn pending_enrollment_for_invitation(
        &self,
        invitation: &str,
    ) -> Result<Option<StoredPendingEnrollment>, String> {
        if !self.0.store.healthy() {
            return Err("native state writer is fenced".into());
        }
        let slotless: Vec<_> = self
            .0
            .authority
            .read()
            .pending
            .values()
            .filter(|pending| pending.target.is_none())
            .cloned()
            .collect();
        for pending in slotless {
            let intent = VerifiedNativeEnrollmentIntent::restore(&pending.intent)?;
            if tilo_peer_state::join::encode_invitation_text(
                intent.verified_join().signed_invitation(),
            ) == invitation
            {
                return Ok(Some(pending));
            }
        }
        Ok(None)
    }
    /// # Errors
    /// The store is fenced or the slot is ambiguous.
    pub fn pending_enrollment_for_slot(
        &self,
        local: WorkspaceId,
    ) -> Result<Option<StoredPendingEnrollment>, String> {
        if !self.0.store.healthy() {
            return Err("native state writer is fenced".into());
        }
        let authority = self.0.authority.read();
        if authority.pending.len() > limits::MAX_PENDING {
            return Err("native pending enrollment quota exceeded".into());
        }
        let mut found = authority.pending.values().filter(|pending| {
            pending
                .target
                .as_ref()
                .is_some_and(|target| target.workspace == local)
        });
        let result = found.next().cloned();
        let ambiguous = found.next().is_some();
        drop(authority);
        if ambiguous {
            return Err("ambiguous native pending enrollment slot".into());
        }
        Ok(result)
    }
    /// # Errors
    /// `id` names another attempt, or the record cannot be published.
    pub fn cancel_enrollment(
        &self,
        room: RoomId,
        id: &PendingEnrollmentId,
    ) -> Result<bool, String> {
        let _turn = self.0.mutation.lock();
        let mut next = self.0.authority.read().clone();
        let Some(pending) = next.pending.get(&room) else {
            return Ok(false);
        };
        if &pending.id != id {
            return Err("enrollment cancellation generation mismatch".into());
        }
        if pending.status == PendingEnrollmentStatus::Cancelled {
            return Ok(false);
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        next.pending
            .get_mut(&room)
            .ok_or("native pending enrollment disappeared")?
            .status = PendingEnrollmentStatus::Cancelled;
        self.publish_pending(&next, None)?;
        Ok(true)
    }
    /// `required` is a room the owner must answer for from now on.
    fn publish_pending(
        &self,
        next: &NativeAuthorityStorage,
        required: Option<RoomId>,
    ) -> Result<(), String> {
        self.0.publish(next, Change::Pending, &mut || {
            *self.0.authority.write() = next.clone();
            if let Some(room) = required {
                self.0.current.write().required.insert(room);
            }
        })
    }
    /// Install a completed enrollment: the room's first committed replica.
    ///
    /// # Errors
    /// The Complete does not match the persisted proof, the admitted role
    /// differs from the signed one, or the record cannot be published.
    pub fn install(
        &self,
        enrollment: &VerifiedNativeEnrollment,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let room = RoomId::parse(&enrollment.bootstrap().room)
            .ok_or("noncanonical native enrollment room")?;
        let pending = self
            .0
            .authority
            .read()
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
        // A device of the governing person is the room's owner whatever its
        // invitation granted: the owner holds its room like any member.
        let expected =
            if enrollment.local_device().user_public_key == bootstrap.authority.user_public_key {
                collab_types::Role::Owner
            } else {
                match enrollment.verified_join().grant().role {
                    tilo_peer_state::join::MemberRole::Editor => collab_types::Role::Editor,
                    tilo_peer_state::join::MemberRole::Commenter => collab_types::Role::Commenter,
                    tilo_peer_state::join::MemberRole::Viewer => collab_types::Role::Viewer,
                }
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
            agent_sessions: std::collections::BTreeMap::new(),
            agent_session_history_root: None,
            agent_messages: std::collections::BTreeMap::new(),
            agent_message_history_root: None,
            workspace_memberships: std::collections::BTreeMap::new(),
            workspace_permission_profiles: std::collections::BTreeMap::new(),
            effect_receipts: std::collections::BTreeMap::new(),
            effect_history_root: None,
        };
        self.publish_room(room, NativeCommittedView { peer, record }, Some(&pending))
    }
    /// Admit and durably commit a document write signed by this device,
    /// with its outgoing custody, before any receipt.
    ///
    /// # Errors
    /// The request is not an admissible local document write, or the record
    /// cannot be published.
    pub fn commit_document(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
        now: SystemTime,
    ) -> Result<tilo_peer_state::owner_commit::OwnerCommitReceipt, String> {
        let room = RoomId::parse(&request.room).ok_or("invalid native document room")?;
        self.commit_document_with_current(room, |_| Ok(request), now)
    }

    /// Prepare a signed local document mutation from the current committed
    /// room and admit it in the same owner turn. This keeps its sequence and
    /// document basis current when other publications race the caller.
    ///
    /// # Errors
    /// The room is fenced, preparation fails, or exact admission and durable
    /// publication refuse the prepared write.
    pub fn commit_document_with_current(
        &self,
        room: RoomId,
        prepare: impl FnOnce(
            &crate::NativeCommittedView,
        )
            -> Result<tilo_peer_state::owner_commit::OwnerCommitRequest, String>,
        now: SystemTime,
    ) -> Result<tilo_peer_state::owner_commit::OwnerCommitReceipt, String> {
        use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt};
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        let request = prepare(&candidate)?;
        if RoomId::parse(&request.room) != Some(room) {
            return Err("prepared native document room differs from selected room".into());
        }
        let outgoing = publication::prepare(&request)?;
        let binding = request.binding()?;
        let CommitOperation::Document {
            app,
            record,
            envelope,
        } = request.operation
        else {
            return Err("unsupported native owner commit family".into());
        };
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
        candidate
            .peer
            .admit_new_local_document(&app, &record, *envelope, now)?;
        publication::append_local(&mut candidate, outgoing)?;
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

    /// Commit one typed Task Center command in the room owner's serialized
    /// turn. Unlike an app document, this mutates the admitted room task
    /// projection itself; it never creates a hidden application record.
    ///
    /// # Errors
    /// The authority is no longer current, the command is stale or malformed,
    /// or the durable owner cannot publish the resulting room generation.
    pub fn commit_workspace_task_command(
        &self,
        request: tilo_peer_state::owner_commit::OwnerCommitRequest,
        now: SystemTime,
    ) -> Result<tilo_peer_state::owner_commit::OwnerCommitReceipt, String> {
        use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitReceipt};
        let binding = request.binding()?;
        let room = RoomId::parse(&request.room).ok_or("invalid native task room")?;
        let CommitOperation::WorkspaceTaskCommand {
            invocation,
            authority,
            command,
        } = request.operation
        else {
            return Err("unsupported native task owner commit family".into());
        };
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if let tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Host { attestor }
        | tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Agent { attestor, .. } =
            &authority
        {
            let admitted = match candidate.host() {
                Some(host) => host == attestor && attestor.capabilities.can_act_as_host,
                None => candidate.governor() == attestor,
            };
            if !admitted {
                return Err("task command attestor is not the exact pinned workspace host".into());
            }
        }
        let (changed, _) = candidate.peer.apply_workspace_task_command(
            invocation,
            binding.request_digest,
            &authority,
            &command,
            now,
        )?;
        if changed {
            candidate.record.peer = candidate.peer.durable()?;
            candidate.record.generation = candidate
                .record
                .generation
                .checked_add(1)
                .ok_or("native owner generation exhausted")?;
            self.publish_room(room, candidate.clone(), None)?;
        }
        Ok(OwnerCommitReceipt {
            binding,
            generation: candidate.generation(),
            aggregate_hash: *blake3::hash(&candidate.record.peer).as_bytes(),
        })
    }
}
pub(crate) fn ns(now: SystemTime) -> Result<u64, String> {
    u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos(),
    )
    .map_err(|e| e.to_string())
}
