use crate::{
    Change, NativeAuthorityStorage, NativeCommittedView, NativeWorkspaceOwner, NeighbourMsg,
    PendingEnrollmentId, PendingEnrollmentStatus, RoomId, StatePeer, SystemTime, TransportOrigin,
    VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent, WorkspaceId, limits, publication,
    record,
};
use record::{StoredPendingEnrollment, StoredRoom};
use tilo_user_identity::RevocationList;
mod sanitized_anchor;
use sanitized_anchor::sanitized_anchor_from_authenticated_bootstrap;

impl NativeWorkspaceOwner {
    /// Enroll a protected managed actor through the actual governing writer.
    /// The certificate and signed delegation become visible only after durable publication.
    /// # Errors
    /// Refuses foreign signing custody, invalid/revoked actors or failed publication.
    pub fn admit_managed_agent(
        &self,
        room: RoomId,
        certificate: tilo_user_identity::DeviceCert,
        delegation: tilo_relay_protocol::delegation::Delegation,
        governor: &tilo_user_identity::DeviceCert,
        key: &ed25519_dalek::SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if candidate.governor() != governor
            || candidate.device() != governor
            || key.verifying_key().to_bytes() != governor.device_public_key
        {
            return Err("managed admission lacks exact current governor signing custody".into());
        }
        let device = certificate.device_public_key;
        let request_digest = *blake3::hash(
            &postcard::to_allocvec(&(&certificate, &delegation))
                .map_err(|error| error.to_string())?,
        )
        .as_bytes();
        let mut peer = candidate.peer.clone();
        if !peer
            .governance_mut()
            .admit_agent(certificate, delegation, key, now)?
        {
            return Ok(());
        }
        let generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("native membership generation exhausted")?;
        let messages = peer.durable_mutation_messages(&candidate.peer, key, now, generation)?;
        for message in &messages {
            if matches!(message, neighbour_wire::NeighbourMsg::AppDocument { .. }) {
                let outgoing = crate::publication::prepare(&crate::publication::request(message)?)?;
                crate::publication::reserve(&candidate, &outgoing)?;
                crate::publication::append_local(&mut candidate, outgoing)?;
            }
        }
        candidate.peer = peer;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = generation;
        // Actor enrollment does not create a content checkpoint index. Using
        // NativeCheckpointRoot here makes publication require an unrelated root
        // that an empty, newly enrolled workspace legitimately does not have.
        let binding = tilo_owner_commit::CommitBinding {
            room: room.to_string(),
            target: tilo_owner_commit::CommitTarget::NativeManagedAgentAdmission { device },
            request_digest,
            basis: *blake3::hash(&candidate.record.peer).as_bytes(),
        };
        crate::publication::replace_governance_checkpoint(&mut candidate, binding, now)?;
        self.publish_room(room, candidate, None)
    }
}

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
    restore_inner(room, record, now, None)
}

pub(crate) fn restore_private_recovery(
    room: RoomId,
    record: &StoredRoom,
    now: SystemTime,
    key: &ed25519_dalek::SigningKey,
) -> Result<NativeCommittedView, String> {
    restore_inner(room, record, now, Some(key))
}

fn restore_inner(
    room: RoomId,
    record: &StoredRoom,
    now: SystemTime,
    recovery_key: Option<&ed25519_dalek::SigningKey>,
) -> Result<NativeCommittedView, String> {
    if !matches!(record.version, 2..=5)
        || (record.version == 2 && record.effect_history_root.is_some())
        || (record.version == 3 && record.effect_history_root.is_none())
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
        let descriptor_digest = metadata.descriptor_digest;
        if record.floor.as_ref().is_some_and(|floor| {
            floor.generation != metadata.generation || floor.digest != descriptor_digest
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
    let mut view = NativeCommittedView {
        peer,
        record: record.clone(),
    };
    if let Some(key) = recovery_key {
        publication::validate_private_recovery(&view, key)?;
    } else {
        publication::validate(&view)?;
    }
    view.record.private_task_text.validate()?;
    view.record.private_task_mailboxes.validate()?;
    view.record.private_direct_conversations.validate()?;
    crate::effect_receipt::validate(&view)?;
    crate::task_admission::validate(&view)?;
    if let Some(key) = recovery_key
        && ensure_private_task_baseline(&mut view, key, now)?
    {
        view.record.peer = view.peer.durable()?;
        view.record.generation = view
            .record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        publication::refresh_governance_checkpoint(&mut view)?;
        publication::validate(&view)?;
    }
    if view.record.sanitized_baseline.is_some()
        && (recovery_key.is_some() || view.device() == view.governor())
    {
        crate::PrivateTaskConfirmationStore::from_confirmations(
            view.record.private_task_text.confirmations.clone(),
        )?
        .validate_document(
            view.peer
                .admitted_room()
                .ok_or("private task room absent")?,
        )?;
    }
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
        now: SystemTime,
    ) -> Result<PendingEnrollmentId, String> {
        self.prepare_enrollment(intent, local, now, false)
    }

    /// Persist an explicit recovery enrollment for an existing lagging replica.
    /// Identity, governor and delivery pins cannot change through this operation.
    /// Completion requires an authenticated newer sanitized current-chain anchor.
    /// # Errors
    /// Refuses changed custody, missing enrollment, fencing or conflicting proofs.
    pub fn prepare_reenrollment(
        &self,
        intent: &VerifiedNativeEnrollmentIntent,
        now: SystemTime,
    ) -> Result<PendingEnrollmentId, String> {
        self.prepare_enrollment(intent, None, now, true)
    }

    fn prepare_enrollment(
        &self,
        intent: &VerifiedNativeEnrollmentIntent,
        local: Option<WorkspaceId>,
        _now: SystemTime,
        recovery: bool,
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
        if recovery {
            let old = next
                .rooms
                .get(&room)
                .ok_or("re-enrollment requires an enrolled room")?;
            let grant = intent.verified_join().grant();
            if old.device != *intent.local_device()
                || old.governor != grant.authority
                || old.workspace != grant.workspace_id
                || old.host.as_ref() != intent.delivery_host()
                || old.locator.as_ref() != intent.delivery_locator()
            {
                return Err("re-enrollment cannot replace identity or authority custody".into());
            }
        } else if next.rooms.contains_key(&room) {
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
    #[allow(
        clippy::too_many_lines,
        reason = "keep enrollment verification and publication together in one serialized custody transaction"
    )]
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
        let existing = self.0.current.read().rooms.get(&room).cloned();
        let mut peer = if let Some(view) = &existing {
            if view.record.device != *enrollment.local_device()
                || view.record.governor != bootstrap.authority
                || view.record.workspace != bootstrap.workspace_id
                || view.record.host.as_ref() != intent.delivery_host()
                || view.record.locator.as_ref() != intent.delivery_locator()
            {
                return Err("re-enrollment Complete changed pinned custody".into());
            }
            for entry in view.peer.revocations() {
                revocations.insert(entry).map_err(|e| e.to_string())?;
            }
            let mut certificates = std::collections::BTreeMap::new();
            for cert in view
                .peer
                .certificates()
                .into_iter()
                .chain(bootstrap.certified_roster.clone())
            {
                if let Some(old) = certificates.insert(cert.device_public_key, cert.clone())
                    && old != cert
                {
                    return Err("re-enrollment certificate equivocation".into());
                }
            }
            StatePeer::restore(
                &view.peer.durable()?,
                &bootstrap.room,
                bootstrap.authority.clone(),
                certificates.into_values().collect(),
                revocations,
                now,
            )?
        } else {
            StatePeer::new(
                &bootstrap.room,
                bootstrap.authority.clone(),
                bootstrap.certified_roster.clone(),
                revocations,
                now,
            )?
        };
        peer.select_admission_v5().map_err(|e| e.to_string())?;
        // Only B's closed, pinned Complete evidence selects historical bootstrap
        // admission. Ongoing relay input uses strict receive_from_report instead.
        // This legacy enum labels canonical admission context, not fabricated
        // gossip provenance or a claim that Complete arrived as a relay frame.
        let anchor = sanitized_anchor_from_authenticated_bootstrap(
            &bootstrap.room,
            &bootstrap.authority,
            &bootstrap.governance_messages,
        )?;
        let mut sanitized_baseline = None;
        // Admit the authenticated replacement before any subsequent room deltas.
        for bytes in &bootstrap.governance_messages {
            let message: NeighbourMsg = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
            if let NeighbourMsg::SanitizedRoomBaseline { baseline, .. } = message {
                let (baseline, tail) =
                    postcard::take_from_bytes(&baseline).map_err(|e| e.to_string())?;
                if !tail.is_empty() {
                    return Err("trailing bootstrap baseline bytes".into());
                }
                let anchor = anchor
                    .as_ref()
                    .ok_or("missing authenticated baseline anchor")?;
                if existing.is_some() {
                    peer.reenroll_sanitized_room_baseline(&baseline, anchor, now)?;
                } else {
                    peer.admit_sanitized_room_baseline_bootstrap(&baseline, anchor, now)?;
                }
                sanitized_baseline = Some(baseline);
            }
        }
        if existing.is_some() && sanitized_baseline.is_none() {
            return Err("re-enrollment requires an authenticated current baseline".into());
        }
        for bytes in &bootstrap.governance_messages {
            let message: NeighbourMsg = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
            if matches!(message, NeighbourMsg::SanitizedRoomBaseline { .. }) {
                continue;
            }
            peer.receive_from(
                TransportOrigin::RetainedRelayDelta(bootstrap.authority.device_public_key),
                message,
                now,
            )?;
        }
        if !peer.has_governance() {
            return Err("native enrollment is missing real admitted governance".into());
        }
        let role = peer
            .permissions(enrollment.local_device().device_public_key, now)?
            .role;
        // A device of the governing person is the room's owner whatever its
        // invitation granted: the owner holds its room like any member.
        let expected =
            if enrollment.local_device().user_public_key == bootstrap.authority.user_public_key {
                collab_types::Role::Owner
            } else {
                enrollment.verified_join().grant().role.into()
            };
        if role != expected {
            return Err("actual admitted role differs from signed enrollment role".into());
        }

        if let Some(view) = existing {
            let mut candidate = view.as_ref().clone();
            candidate.peer = peer;
            candidate.record.version = candidate.record.version.max(4);
            candidate.record.sanitized_baseline = sanitized_baseline;
            candidate
                .record
                .enrollment_intent
                .clone_from(&pending.intent);
            candidate.record.invitation = enrollment.verified_join().signed_invitation().to_vec();
            candidate.record.peer = candidate.peer.durable()?;
            candidate.record.generation = candidate
                .record
                .generation
                .checked_add(1)
                .ok_or("native owner generation exhausted")?;
            publication::retire(&mut candidate)?;
            return self.publish_room(room, candidate, Some(&pending));
        }
        let record = StoredRoom {
            version: if sanitized_baseline.is_some() { 4 } else { 2 },
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
            sanitized_baseline,
            task_admission: crate::task_admission::TaskAdmissionLedger::default(),
            private_task_text: crate::PrivateTaskTextStore::default(),
            private_task_mailboxes: crate::PrivateTaskMailboxStore::default(),
            private_direct_conversations: crate::PrivateDirectConversationStore::default(),
            generation: 1,
            continuity: 0,
            floor: None,
            publication: Some(publication::StoredPublication::default()),
            workspace_memberships: std::collections::BTreeMap::new(),
            effect_receipts: std::collections::BTreeMap::new(),
            effect_history_root: None,
        };
        self.publish_room(room, NativeCommittedView { peer, record }, Some(&pending))
    }
    /// Admit and durably commit a document write signed by this device,
    /// with its outgoing custody, before any receipt.
    ///
    /// # Sequence contract
    /// The envelope is signed before this owner turn starts, so its sequence
    /// must come from [`collab_types::next_unordered_document_sequence`] (as
    /// every SDK document signer does). A dense `floor + 1` read through
    /// [`Self::with_current`] races [`Self::reconcile_outgoing`], which
    /// reserves `floor + 1` for a recovery projection without exact evidence
    /// in its own turn; the write is then refused as stale. A caller that
    /// needs the dense sequence prepares it inside the turn with
    /// [`Self::commit_document_with_current`].
    ///
    /// # Errors
    /// The request is not an admissible local document write, its signed
    /// sequence is stale, or the record cannot be published.
    pub fn commit_document(
        &self,
        request: tilo_owner_commit::OwnerCommitRequest,
        now: SystemTime,
    ) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
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
        ) -> Result<tilo_owner_commit::OwnerCommitRequest, String>,
        now: SystemTime,
    ) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
        use tilo_owner_commit::{CommitOperation, OwnerCommitReceipt};
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
        request: tilo_owner_commit::OwnerCommitRequest,
        now: SystemTime,
    ) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
        self.commit_workspace_task_command_inner(request, None, None, now)
    }

    /// Ensure public history has a signed privacy baseline before publication.
    /// The extracted private text, replacement document and retired outgoing
    /// custody are persisted together; failure leaves the predecessor untouched.
    ///
    /// # Errors
    /// A missing baseline requires the exact governor key, settled history and
    /// successful durable storage. Members must install an authenticated baseline.
    pub fn ensure_private_task_baseline(
        &self,
        room: RoomId,
        key: &ed25519_dalek::SigningKey,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(room, |view| Ok(view.clone()))?;
        if key.verifying_key().to_bytes() != candidate.device().device_public_key {
            return Err("native privacy signer differs from selected device".into());
        }
        candidate
            .peer
            .permissions(candidate.device().device_public_key, now)?;
        if !ensure_private_task_baseline(&mut candidate, key, now)? {
            return Ok(());
        }
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_room(room, candidate, None)
    }

    /// Commit private task text and, when needed, migrate historical room text
    /// into a signed sanitized replacement in the same durable owner turn.
    ///
    /// # Errors
    /// Refuses invalid governor signing custody, unsettled migration input,
    /// unauthorized commands or failed durable publication.
    pub fn commit_workspace_task_command_with_key(
        &self,
        request: tilo_owner_commit::OwnerCommitRequest,
        key: &ed25519_dalek::SigningKey,
        now: SystemTime,
    ) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
        self.commit_workspace_task_command_inner(request, Some(key), None, now)
    }

    /// Commit a conscious person start with a host-selected MODEL ceiling.
    /// The shell hook runs under the same owner turn as the actual approval;
    /// neither the request nor a later worker claim can choose the profile.
    /// # Errors
    /// Returns an error if current authority, run binding or durable custody cannot be verified.
    pub fn commit_workspace_task_command_with_model_start(
        &self,
        request: tilo_owner_commit::OwnerCommitRequest,
        key: &ed25519_dalek::SigningKey,
        model_start: &dyn crate::task_admission::model::NativeModelStartAuthority,
        now: SystemTime,
    ) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
        self.commit_workspace_task_command_inner(request, Some(key), Some(model_start), now)
    }

    fn commit_workspace_task_command_inner(
        &self,
        request: tilo_owner_commit::OwnerCommitRequest,
        key: Option<&ed25519_dalek::SigningKey>,
        model_start: Option<&dyn crate::task_admission::model::NativeModelStartAuthority>,
        now: SystemTime,
    ) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
        use tilo_owner_commit::{CommitOperation, OwnerCommitReceipt};
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
        let migrated = match key {
            Some(key) => ensure_private_task_baseline(&mut candidate, key, now)?,
            None => false,
        };
        let source = TaskCommandSource {
            command: &command,
            authority: &authority,
            invocation,
            request_digest: binding.request_digest,
            now,
        };
        let Some(prepared) =
            PreparedTaskCommand::prepare(&candidate, &source, model_start, &request.room)?
        else {
            return self.with_current(room, |view| {
                Ok(OwnerCommitReceipt {
                    binding,
                    generation: view.generation(),
                    aggregate_hash: *blake3::hash(&view.record.peer).as_bytes(),
                })
            });
        };
        // Only the scrubbed command reaches the reducer and its audit/history.
        // The original request survives outside private storage only as a digest.
        let (changed, result) = candidate
            .peer
            .workspace_mut()
            .apply_workspace_task_command_with_owner_preparation(
                invocation,
                binding.request_digest,
                &authority,
                &prepared.public_command,
                now,
                tilo_peer_state::workspace_task::WorkspaceTaskOwnerPreparation {
                    model_start: prepared.model_snapshot.as_ref(),
                    private_retry: prepared.private_retry.as_deref(),
                },
            )?;
        prepared.retain(&mut candidate, &source, changed, &result, key)?;
        let admission_changed = stop_task_admission(&mut candidate, room, &command, key)?;
        if changed || migrated || admission_changed {
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
struct TaskCommandSource<'a> {
    command: &'a tilo_peer_state::workspace_task::WorkspaceTaskCommand,
    authority: &'a tilo_peer_state::workspace_task::WorkspaceTaskAuthority,
    invocation: uuid::Uuid,
    request_digest: [u8; 32],
    now: SystemTime,
}

struct PreparedTaskCommand {
    public_command: tilo_peer_state::workspace_task::WorkspaceTaskCommand,
    private_confirmation: Option<(String, tilo_desktop_model::WorkspaceTaskConfirmation)>,
    private_report: Option<crate::private_task_outputs::NativePrivateTaskReport>,
    private_write: Option<crate::private_task_text::NativePrivateInstructionWrite>,
    private_retry: Option<String>,
    model_snapshot: Option<tilo_peer_state::workspace_task::WorkspaceTaskModelStart>,
}

impl PreparedTaskCommand {
    fn prepare(
        candidate: &NativeCommittedView,
        source: &TaskCommandSource<'_>,
        model_start: Option<&dyn crate::task_admission::model::NativeModelStartAuthority>,
        room: &str,
    ) -> Result<Option<Self>, String> {
        let (confirmation_command, private_confirmation) =
            crate::private_task_confirmation::prepare(candidate, source.command)?;
        let (report_command, mut private_report) = crate::private_task_outputs::prepare_report(
            &confirmation_command,
            &candidate.peer,
            source.authority,
            source.invocation,
        )?;
        if private_report.is_some()
            && candidate
                .record
                .private_task_text
                .reports
                .get(&source.invocation.to_string())
                .is_some_and(|report| {
                    report
                        .request_digest
                        .is_some_and(|digest| digest != source.request_digest)
                })
        {
            return Err("private report invocation already belongs to another source".into());
        }
        if let Some(report) = private_report.as_mut() {
            report.request_digest = Some(source.request_digest);
            if let Some(existing) = candidate
                .record
                .private_task_text
                .reports
                .get(&source.invocation.to_string())
                && existing.request_digest == report.request_digest
            {
                report.accepted_at_unix_ns = existing.accepted_at_unix_ns;
                report.sequence = existing.sequence;
                if report != existing {
                    return Err(
                        "private report invocation already belongs to another source".into(),
                    );
                }
                // This exact accepted report is a replay, not fresh run authority.
                return Ok(None);
            }
        }
        if let tilo_peer_state::workspace_task::WorkspaceTaskCommand::RequestConfirmation {
            task_id,
            run_epoch,
            confirmation,
        } = &report_command
        {
            candidate
                .record
                .task_admission
                .validate_confirmation_request(task_id, *run_epoch, confirmation)?;
        }
        let private_retry = candidate
            .record
            .private_task_text
            .retained_chat_retry(
                &report_command,
                &candidate.peer,
                source.authority,
                source.now,
            )?
            .map(str::to_owned);
        let (public_command, private_write) = candidate
            .record
            .private_task_text
            .prepare_private_command(&report_command, &candidate.peer)?;

        let model_snapshot =
            prepare_task_model_start(candidate, source, &public_command, model_start, room)?;
        Ok(Some(Self {
            public_command,
            private_confirmation,
            private_report,
            private_write,
            private_retry,
            model_snapshot,
        }))
    }

    fn retain(
        mut self,
        candidate: &mut NativeCommittedView,
        source: &TaskCommandSource<'_>,
        changed: bool,
        result: &tilo_desktop_model::WorkspaceTaskMutationResult,
        key: Option<&ed25519_dalek::SigningKey>,
    ) -> Result<(), String> {
        // The reducer validates exact command retries before private invocation
        // collision checks. A new command must never overwrite another report.
        if changed
            && let Some(report) = self.private_report.as_mut()
            && let Some(existing) = candidate
                .record
                .private_task_text
                .reports
                .get(&source.invocation.to_string())
        {
            // Acceptance time and order belong to the first owner receipt.
            // Every source, task, actor and private body must still match.
            report.accepted_at_unix_ns = existing.accepted_at_unix_ns;
            report.sequence = existing.sequence;
            report.request_digest = existing.request_digest;
            if report != existing {
                return Err("private report invocation already belongs to another source".into());
            }
            self.private_report = None;
        }
        if changed && let Some(model) = self.model_snapshot {
            let tilo_peer_state::workspace_task::WorkspaceTaskCommand::ApproveRun {
                task_id, ..
            } = &self.public_command
            else {
                return Err("private model snapshot lacks person start".into());
            };
            let state = tilo_peer_state::workspace_task::snapshot(
                candidate
                    .peer
                    .admitted_room()
                    .ok_or("model start room absent")?,
            )?;
            let mut full = state
                .controls
                .get(task_id)
                .and_then(|control| control.model_authority.clone())
                .ok_or("approved model authority absent")?;
            full.contract = model.contract;
            candidate
                .record
                .private_task_text
                .model_authorities
                .insert(task_id.clone(), full);
            candidate.record.private_task_text.validate()?;
        }
        if changed && let Some(mut report) = self.private_report {
            report.sequence = u64::try_from(candidate.record.private_task_text.reports.len())
                .ok()
                .and_then(|sequence| sequence.checked_add(1))
                .ok_or("private task report sequence exhausted")?;
            candidate
                .record
                .private_task_text
                .reports
                .insert(source.invocation.to_string(), report);
            candidate.record.private_task_text.validate()?;
        }
        if changed && let Some(write) = self.private_write {
            if let Some(reference) = write.instruction_reference() {
                candidate
                    .peer
                    .workspace_mut()
                    .bind_workspace_task_private_instruction(result, &reference)?;
            }
            candidate
                .record
                .private_task_text
                .retain_private_instruction(result, write)?;
        }
        if changed {
            crate::private_task_confirmation::retain(candidate, self.private_confirmation)?;
            if let Some(key) = key {
                // A receiver may recover this delta without any private bodies.
                // Authenticate retained direction references independently of
                // the older signed history checkpoint, in the same owner commit.
                candidate.peer.attest_native_task_private_directions(
                    key,
                    &candidate.record.private_task_text.private_references(),
                )?;
            }
        }

        Ok(())
    }
}

fn prepare_task_model_start(
    candidate: &NativeCommittedView,
    source: &TaskCommandSource<'_>,
    public_command: &tilo_peer_state::workspace_task::WorkspaceTaskCommand,
    model_start: Option<&dyn crate::task_admission::model::NativeModelStartAuthority>,
    room: &str,
) -> Result<Option<tilo_peer_state::workspace_task::WorkspaceTaskModelStart>, String> {
    Ok({
        match (source.authority, public_command, model_start) {
            (
                tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Person { caller },
                tilo_peer_state::workspace_task::WorkspaceTaskCommand::ApproveRun {
                    task_id, ..
                },
                Some(hook),
            ) => {
                let state = tilo_peer_state::workspace_task::snapshot(
                    candidate
                        .peer
                        .admitted_room()
                        .ok_or("model start room absent")?,
                )?;
                let task = state
                    .tasks
                    .iter()
                    .find(|task| task.id == *task_id)
                    .ok_or("model start task absent")?;
                if task.route == tilo_desktop_model::WorkspaceTaskRoute::Model {
                    let contract =
                        hook.approved_model_start(candidate, task_id, caller, source.now)?;
                    contract.source_for(room, task_id)?;
                    if contract.selection.app.is_empty()
                        || contract.selection.provider.is_empty()
                        || contract.selection.model.is_empty()
                        || !contract.selection.contract.is_object()
                    {
                        return Err("approved MODEL full selection snapshot incomplete".into());
                    }
                    Some(tilo_peer_state::workspace_task::WorkspaceTaskModelStart {
                        request_model: contract.request_model.clone(),
                        kind: contract.kind,
                        contract: serde_json::to_value(contract).map_err(|e| e.to_string())?,
                    })
                } else {
                    None
                }
            }
            _ => None,
        }
    })
}

fn stop_task_admission(
    candidate: &mut NativeCommittedView,
    room: RoomId,
    command: &tilo_peer_state::workspace_task::WorkspaceTaskCommand,
    key: Option<&ed25519_dalek::SigningKey>,
) -> Result<bool, String> {
    if let tilo_peer_state::workspace_task::WorkspaceTaskCommand::CancelForRun {
        task_id,
        run_epoch,
        ..
    } = command
    {
        let key = key.ok_or("Stop requires exact governor custody")?;
        let was_fenced = candidate
            .record
            .task_admission
            .is_fenced(task_id, *run_epoch);
        let local_host = (candidate.device() == candidate.governor())
            .then_some(candidate.device().device_public_key);
        crate::task_admission::NativeGovernorAdmission {
            room,
            peer: &candidate.peer,
            ledger: &mut candidate.record.task_admission,
            continuity: candidate.record.continuity,
            key,
        }
        .stop_with_local_host(task_id, *run_epoch, local_host)?;
        candidate.record.version = 5;
        return Ok(!was_fenced);
    }
    Ok(false)
}

/// Merge only exact references from a signature-verified clean checkpoint.
/// Read the direction schema alone so older unrelated control fields remain valid.
fn merge_signed_direction_references(
    document: &automerge::AutoCommit,
    references: &mut tilo_peer_state::workspace_task::NativeTaskPrivateReferences,
) -> Result<(), String> {
    use automerge::{ObjType, ROOT, ReadDoc as _, ScalarValue, Value};
    let root = match document
        .get(ROOT, tilo_peer_state::workspace_task::TASK_CONTROL_KEY)
        .map_err(|error| error.to_string())?
    {
        None => return Ok(()),
        Some((Value::Object(ObjType::Map), root)) => root,
        Some(_) => return Err("signed task control root is not a map".into()),
    };
    for row in document.map_range(&root, ..) {
        let Value::Scalar(value) = row.value.into_value() else {
            return Err("signed task control is not bytes".into());
        };
        let ScalarValue::Bytes(bytes) = value.as_ref() else {
            return Err("signed task control is not bytes".into());
        };
        let control: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let Some(directions) = control.get("directions") else {
            continue;
        };
        let known = references.entry(row.key.into_owned()).or_default();
        for direction in directions
            .as_array()
            .ok_or("signed directions are not a list")?
        {
            let revision = direction
                .get("revision")
                .and_then(serde_json::Value::as_u64)
                .ok_or("signed direction revision absent")?;
            let text = direction
                .get("text")
                .and_then(serde_json::Value::as_str)
                .ok_or("signed direction reference absent")?;
            if known.get(&revision).is_some_and(|value| value != text) {
                return Err("private direction differs from signed baseline".into());
            }
            known.insert(revision, text.to_owned());
        }
    }
    Ok(())
}

pub(crate) fn ensure_private_task_baseline(
    candidate: &mut NativeCommittedView,
    key: &ed25519_dalek::SigningKey,
    now: SystemTime,
) -> Result<bool, String> {
    // A checkpoint produced before automatic report privacy was enforced may
    // still contain report bodies. Its presence alone is not migration proof.
    let mut clean_baseline = false;
    let mut references = candidate.record.private_task_text.private_references();
    if let Some(baseline) = &candidate.record.sanitized_baseline {
        match baseline.verify_anchored_document(
            &candidate.record.room.to_string(),
            candidate.governor().device_public_key,
            &baseline.anchor(),
        ) {
            Ok(document) => {
                clean_baseline = true;
                // The independently signed checkpoint authenticates these exact
                // references even on a host that has never held their bodies.
                // A reference-shaped string in ordinary sync is not sufficient.
                merge_signed_direction_references(&document, &mut references)?;
            }
            // Authentication and checkpoint framing passed before this check.
            // Other failures must not be disguised as privacy upgrades.
            Err(error) if publication::recoverable_private_baseline(&error) => {
                publication::validate_private_recovery(candidate, key)?;
            }
            Err(error) => return Err(error),
        }
    }
    if key.verifying_key().to_bytes() != candidate.governor().device_public_key {
        return Err("task history requires a signed privacy migration by its governor".into());
    }
    let mut peer = candidate.peer.clone();
    let (text, baseline) = peer.extract_native_task_private_baseline(key, now, &references)?;
    if clean_baseline && !crate::PrivateTaskTextStore::migration_has_private_text(&text) {
        crate::PrivateTaskConfirmationStore::from_confirmations(
            candidate.record.private_task_text.confirmations.clone(),
        )?
        .validate_document(
            candidate
                .peer
                .admitted_room()
                .ok_or("private task room absent")?,
        )?;
        return Ok(false);
    }
    candidate.peer = peer;
    candidate
        .record
        .private_task_text
        .retain_migrated_text(text)?;
    candidate.record.sanitized_baseline = Some(baseline);
    // A scrubbed placeholder is not recoverable private custody. Refuse rather
    // than silently replacing a live review whose original body is unavailable.
    crate::PrivateTaskConfirmationStore::from_confirmations(
        candidate.record.private_task_text.confirmations.clone(),
    )?
    .validate_document(
        candidate
            .peer
            .admitted_room()
            .ok_or("private task room absent")?,
    )?;
    candidate.record.version = candidate.record.version.max(4);
    publication::retire(candidate)?;
    Ok(true)
}

pub(crate) fn ns(now: SystemTime) -> Result<u64, String> {
    u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos(),
    )
    .map_err(|e| e.to_string())
}
