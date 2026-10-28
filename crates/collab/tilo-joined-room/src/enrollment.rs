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
    match &record.enrollment {
        Some(enrollment) => validate_enrollment(room, record, enrollment)?,
        // A room this device created and governs: no join to check it against.
        None if record.governor != record.device => {
            return Err("a room governed here is governed by this device".into());
        }
        None => {}
    }
    let (governor, revocations) = record.current_governor(now);
    let peer = StatePeer::restore(
        &record.peer,
        &room.to_string(),
        governor,
        vec![record.device.clone()],
        revocations,
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
    view.record.private_direct_conversations.validate()?;
    crate::effect_receipt::validate(&view)?;
    Ok(view)
}
#[allow(
    clippy::suspicious_operation_groupings,
    reason = "a persisted floor stores the descriptor digest as `digest`"
)]
fn validate_enrollment(
    room: RoomId,
    record: &StoredRoom,
    enrollment: &crate::record::StoredEnrollment,
) -> Result<(), String> {
    let intent = VerifiedNativeEnrollmentIntent::restore(&enrollment.enrollment_intent)?;
    let grant = intent.verified_join().grant();
    if intent.room() != room
        || record.governor != grant.authority
        || record.workspace != grant.workspace_id
        || (record.device != *intent.local_device()
            && !crate::renewal::renews(&record.device, intent.local_device()))
        || enrollment.transport_hint != *intent.transport_hint()
        || enrollment.invitation != intent.verified_join().signed_invitation()
    {
        return Err("native committed room differs from its authenticated enrollment".into());
    }
    if enrollment.host.as_ref() != intent.delivery_host()
        || enrollment.locator.as_ref() != intent.delivery_locator()
    {
        return Err("native service pin differs from its unique signed enrollment locator".into());
    }
    if enrollment.host.is_none() && (enrollment.hint.is_some() || enrollment.floor.is_some()) {
        return Err("native descriptor has no independent service pin".into());
    }
    if enrollment.hint.is_some() != enrollment.floor.is_some() {
        return Err("native persisted descriptor acknowledgement is incomplete".into());
    }
    if let (Some(hint), Some(host)) = (&enrollment.hint, &enrollment.host) {
        let metadata = tilo_delivery_protocol::descriptor::inspect_persisted_hint(hint, host)
            .map_err(|e| e.to_string())?;
        if enrollment.floor.as_ref().is_some_and(|floor| {
            floor.generation != metadata.generation || floor.digest != metadata.descriptor_digest
        }) {
            return Err("native persisted descriptor/floor mismatch".into());
        }
    }
    Ok(())
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
        // The workspace a join installs into is the room's own, made for
        // it: never a workspace of another room, which would have to be
        // re-pointed at this one.
        let target = local
            .map(|workspace| match self.0.store.slot_room(workspace) {
                Some(shown) if shown == room => Ok(record::EnrollmentTarget { workspace, room }),
                Some(_) => Err("enrollment workspace is another room's"),
                None => Err("enrollment local slot no longer exists"),
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
                (Some(stored), Some(target)) => stored.workspace == target.workspace,
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
    /// Drop the proof of attempt `id` into `room`, which its host answered
    /// it never admitted and no longer can
    /// ([`tilo_peer_state::join::JOIN_PROOF_LAPSED`]): resending it can only
    /// be refused again, and while it is kept no new proof for the room can
    /// be made. Answers whether it was dropped.
    ///
    /// # Errors
    /// `id` names another attempt, or the record cannot be published.
    pub fn drop_lapsed_enrollment(
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
            return Err("lapsed enrollment generation mismatch".into());
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        next.pending.remove(&room);
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
            || (intent.local_device() != enrollment.local_device()
                && !crate::renewal::renews(enrollment.local_device(), intent.local_device()))
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
        let mut peer = StatePeer::joined(
            &bootstrap.room,
            bootstrap.authority.clone(),
            bootstrap.certified_roster.clone(),
            revocations,
            now,
        )?;
        peer.select_admission_v5().map_err(|e| e.to_string())?;
        let app_messages = install_governance(&mut peer, bootstrap, now)?;
        tracing::debug!(
            %room,
            app_messages,
            "a room was installed on its governance; its app data follows through the room's sync"
        );
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

        let record = StoredRoom {
            version: 2,
            room,
            workspace: bootstrap.workspace_id,
            governor: bootstrap.authority.clone(),
            device: enrollment.local_device().clone(),
            enrollment: Some(crate::record::StoredEnrollment {
                invitation: enrollment.verified_join().signed_invitation().to_vec(),
                enrollment_intent: pending.intent.clone(),
                enrolled_at_ns: ns(now)?,
                transport_hint: enrollment.transport_hint().clone(),
                host: intent.delivery_host().cloned(),
                locator: intent.delivery_locator().cloned(),
                hint: enrollment.delivery_hint().cloned(),
                floor: None,
            }),
            peer: peer.durable()?,
            private_direct_conversations: crate::PrivateDirectConversationStore::default(),
            generation: 1,
            continuity: 0,
            publication: Some(publication::StoredPublication::default()),
            workspace_memberships: std::collections::BTreeMap::new(),
            effect_receipts: std::collections::BTreeMap::new(),
            effect_history_root: None,
            successions: Vec::new(),
            join_ledger: tilo_peer_state::join_ledger::JoinLedger::default(),
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
            if !candidate.governed_here()
                && !publication::has_exact(&candidate, &outgoing)?
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
}
/// Admit the room's governance from `bootstrap` into `peer`, in the order
/// the host published it, and answer how many app messages it left out.
///
/// Becoming a member takes the room's governance alone: its membership,
/// roles and policy, and the certificates they import, which are everything
/// an app message is authorized against. An app message is not part of it.
/// A host's bootstrap carries its own record publications as well (every
/// checkpoint it published since its last full one), and a device that does
/// not hold the room yet cannot decide many of them: the app surface a change
/// names is read from the room's own content, and a sealed write needs its
/// record's checkpoint. Admitted here, each one waited in the peer's bounded
/// prerequisite queue, so a busy room's bootstrap overflowed it and the join
/// failed however often it was tried. The host retains every one of those
/// publications on its record's channel, and the room's sync delivers them
/// once the room is installed, where each is authorized exactly as any later
/// write is.
pub(crate) fn install_governance(
    peer: &mut StatePeer,
    bootstrap: &tilo_peer_state::join::JoinBootstrap,
    now: SystemTime,
) -> Result<usize, String> {
    let origin = TransportOrigin::RetainedRelayDelta(bootstrap.authority.device_public_key);
    let mut app_messages = 0usize;
    for bytes in &bootstrap.governance_messages {
        let message: NeighbourMsg = postcard::from_bytes(bytes).map_err(|e| e.to_string())?;
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            app_messages = app_messages.saturating_add(1);
            continue;
        }
        // Only the closed, pinned Complete evidence selects historical
        // admission here; ongoing relay input uses strict receive_from_report.
        // The origin labels that admission context, not a relay frame.
        peer.receive_from(origin, message, now)?;
    }
    Ok(app_messages)
}
pub(crate) fn ns(now: SystemTime) -> Result<u64, String> {
    u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos(),
    )
    .map_err(|e| e.to_string())
}
