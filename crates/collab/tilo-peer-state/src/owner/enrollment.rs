use crate::owner::{
    Change, NativeAuthorityStorage, NativeCommittedView, NativeWorkspaceOwner, NeighbourMsg,
    PendingEnrollmentId, PendingEnrollmentStatus, StatePeer, SystemTime, TransportOrigin,
    VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent, WorkspaceId, WorkspaceSlot, limits,
    publication, record,
};
use record::{StoredPendingEnrollment, StoredWorkspace};

pub(crate) fn validate_pending(
    workspace: WorkspaceId,
    pending: &StoredPendingEnrollment,
) -> Result<(), String> {
    let intent = VerifiedNativeEnrollmentIntent::restore(&pending.intent)?;
    if workspace != pending.id.workspace()
        || workspace != intent.workspace()
        || pending.id.digest() != intent.proof_digest()
        || pending.id.generation() == 0
    {
        return Err("pending enrollment scope mismatch".into());
    }
    Ok(())
}
pub(crate) fn restore(
    workspace: WorkspaceId,
    record: &StoredWorkspace,
    now: SystemTime,
) -> Result<NativeCommittedView, String> {
    if !matches!(record.version, 2 | 3)
        || (record.version == 3) != record.effect_history_root.is_some()
        || workspace != record.workspace
        || record.generation == 0
        || record.continuity == 0
    {
        return Err("invalid native committed workspace record".into());
    }
    match (&record.enrollment, &record.vouched) {
        (Some(enrollment), None) => {
            if record.host_hint.is_some() {
                return Err("native workspace record holds an enrollment and a host hint".into());
            }
            validate_enrollment(workspace, record, enrollment)?;
        }
        (None, Some(vouched)) => {
            if record.host_hint.is_some() {
                return Err("native workspace record holds a vouch and a host hint".into());
            }
            crate::owner::vouched::validate(workspace, record, &vouched.bootstrap)?;
        }
        // A workspace this device's own host keeps was founded here
        // ([`NativeWorkspaceOwner::create_workspace`]): no other device
        // admitted this one into it, so there is neither an enrollment nor
        // a vouch to hold, and where its own host is reached is the hint
        // the record carries.
        (None, None) if record.device == record.workspace_host => {
            if let Some(hint) = &record.host_hint {
                crate::owner::validate_host_hint(
                    hint,
                    &workspace.to_string(),
                    record.workspace_id,
                    &record.device,
                )?;
            }
        }
        (None, None) => {
            return Err("a native workspace record holds no single enrollment".into());
        }
        (Some(_), Some(_)) => {
            return Err("a native workspace record holds both an enrollment and a vouch".into());
        }
    }
    let peer = StatePeer::restore(
        &record.peer.to_vec(),
        &workspace.to_string(),
        record.workspace_host.clone(),
        vec![record.device.clone()],
        tilo_user_identity::RevocationList::new(),
        now,
    )?;
    if !peer.admission_v5_selected() {
        return Err("native committed workspace requires durable admission v5".into());
    }
    let view = NativeCommittedView::new(peer, record.clone());
    publication::validate(&view)?;
    crate::owner::effect_receipt::validate(&view)?;
    Ok(view)
}
fn validate_enrollment(
    workspace: WorkspaceId,
    record: &StoredWorkspace,
    enrollment: &crate::owner::record::StoredEnrollment,
) -> Result<(), String> {
    let intent = VerifiedNativeEnrollmentIntent::restore(&enrollment.enrollment_intent)?;
    let join = intent.verified_join();
    let grant = join.grant();
    if intent.workspace() != workspace
        || record.workspace_host != *join.workspace_authority()
        || record.workspace_id != grant.workspace_id
        || (record.device != *intent.local_device()
            && !crate::owner::renewal::renews(&record.device, intent.local_device()))
        || enrollment.transport_hint != *intent.transport_hint()
        || enrollment.invitation != join.signed_invitation()
    {
        return Err("native committed workspace differs from its authenticated enrollment".into());
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
/// Rewrite every stored enrollment intent a build wrote before the join
/// version bump in this build's form. The signed Complete stays exactly as
/// the device signed it - nothing here can sign for it again - and the
/// record around it takes this build's version, so what is left on disk is
/// the shape this build writes.
pub(crate) fn migrate_intents(storage: &mut NativeAuthorityStorage) -> Result<bool, String> {
    let mut changed = false;
    for record in storage.workspaces.values_mut() {
        if let Some(enrollment) = &mut record.enrollment {
            let bytes = VerifiedNativeEnrollmentIntent::restore(&enrollment.enrollment_intent)?
                .durable_bytes()?;
            if bytes != enrollment.enrollment_intent {
                enrollment.enrollment_intent = bytes;
                changed = true;
            }
        }
    }
    for pending in storage.pending.values_mut() {
        let bytes = VerifiedNativeEnrollmentIntent::restore(&pending.intent)?.durable_bytes()?;
        if bytes != pending.intent {
            pending.intent = bytes;
            changed = true;
        }
    }
    Ok(changed)
}
impl NativeWorkspaceOwner {
    /// Persist the signed Complete proof before it is sent, so a lost
    /// response is retried with the exact same proof. `local` is the
    /// local slot the enrolled workspace will be shown in, where the host has
    /// slots.
    pub fn prepare_enrollment_into(
        &self,
        intent: &VerifiedNativeEnrollmentIntent,
        local: Option<WorkspaceSlot>,
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
        if checked.proof_digest() != intent.proof_digest()
            || checked.workspace() != intent.workspace()
        {
            return Err("native intent round-trip mismatch".into());
        }
        let _turn = self.0.mutation.lock();
        let workspace = intent.workspace();
        {
            let current = self.0.current.read();
            if current.fenced || current.denied.contains(&workspace) || !self.0.store.healthy() {
                return Err("native enrollment custody is fenced".into());
            }
        }
        let mut next = self.0.authority.read().clone();
        if next.workspaces.contains_key(&workspace) {
            return Err(
                "workspace is already enrolled; identity replacement requires explicit custody handoff"
                    .into(),
            );
        }
        // The slot a join installs into is the workspace's own, made for it:
        // never a slot of another workspace, which would have to be re-pointed
        // at this one.
        let target = local
            .map(|slot| match self.0.store.slot_workspace(slot) {
                Some(shown) if shown == workspace => Ok(record::EnrollmentTarget {
                    workspace: slot,
                    prior_workspace: workspace,
                }),
                Some(_) => Err("enrollment workspace is another workspace's"),
                None => Err("enrollment local slot no longer exists"),
            })
            .transpose()?;
        if let Some(target) = &target
            && next.pending.values().any(|pending| {
                pending
                    .target
                    .as_ref()
                    .is_some_and(|other| other.workspace == target.workspace)
                    && pending.id.workspace() != workspace
            })
        {
            return Err("local slot already has an unresolved enrollment proof".into());
        }

        if let Some(pending) = next.pending.get(&workspace) {
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
            return Err("workspace already has an unresolved enrollment proof".into());
        }
        if next.pending.len() >= limits::MAX_PENDING {
            return Err("native pending enrollment quota exhausted".into());
        }
        let generation = next
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        let id = PendingEnrollmentId::new(workspace, generation, intent.proof_digest());
        next.revision = generation;
        next.pending.insert(
            workspace,
            StoredPendingEnrollment {
                target,
                id: id.clone(),
                intent: bytes,
                status: PendingEnrollmentStatus::Active,
            },
        );
        self.publish_pending(&next, Some(workspace))?;
        Ok(id)
    }
    pub fn pending_enrollment(
        &self,
        workspace: WorkspaceId,
    ) -> Result<Option<StoredPendingEnrollment>, String> {
        if !self.0.store.healthy() {
            return Err("native state writer is fenced".into());
        }
        Ok(self.0.authority.read().pending.get(&workspace).cloned())
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
            if crate::join::encode_invitation_text(intent.verified_join().signed_invitation())
                == invitation
            {
                return Ok(Some(pending));
            }
        }
        Ok(None)
    }
    pub fn pending_enrollment_for_slot(
        &self,
        local: WorkspaceSlot,
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
        workspace: WorkspaceId,
        id: &PendingEnrollmentId,
    ) -> Result<bool, String> {
        let _turn = self.0.mutation.lock();
        let mut next = self.0.authority.read().clone();
        let Some(pending) = next.pending.get(&workspace) else {
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
            .get_mut(&workspace)
            .ok_or("native pending enrollment disappeared")?
            .status = PendingEnrollmentStatus::Cancelled;
        self.publish_pending(&next, None)?;
        Ok(true)
    }
    /// Drop the proof of attempt `id` into `workspace`, which its host answered
    /// it never admitted and no longer can
    /// ([`crate::join::JOIN_PROOF_LAPSED`]): resending it can only
    /// be refused again, and while it is kept no new proof for the workspace can
    /// be made. Answers whether it was dropped.
    ///
    /// # Errors
    /// `id` names another attempt, or the record cannot be published.
    pub fn drop_lapsed_enrollment(
        &self,
        workspace: WorkspaceId,
        id: &PendingEnrollmentId,
    ) -> Result<bool, String> {
        let _turn = self.0.mutation.lock();
        let mut next = self.0.authority.read().clone();
        let Some(pending) = next.pending.get(&workspace) else {
            return Ok(false);
        };
        if &pending.id != id {
            return Err("lapsed enrollment generation mismatch".into());
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or("native storage generation exhausted")?;
        next.pending.remove(&workspace);
        self.publish_pending(&next, None)?;
        Ok(true)
    }
    /// `required` is a workspace the owner must answer for from now on.
    fn publish_pending(
        &self,
        next: &NativeAuthorityStorage,
        required: Option<WorkspaceId>,
    ) -> Result<(), String> {
        self.0.publish(next, Change::Pending, true, &mut || {
            *self.0.authority.write() = next.clone();
            if let Some(workspace) = required {
                self.0.current.write().required.insert(workspace);
            }
        })
    }
    /// Install a completed enrollment: the workspace's first committed replica.
    pub fn install(
        &self,
        enrollment: &VerifiedNativeEnrollment,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let workspace = WorkspaceId::parse(&enrollment.bootstrap().room)
            .ok_or("noncanonical native enrollment workspace")?;
        let pending = self
            .0
            .authority
            .read()
            .pending
            .get(&workspace)
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
                && !crate::owner::renewal::renews(enrollment.local_device(), intent.local_device()))
            || intent.transport_hint() != enrollment.transport_hint()
        {
            return Err("native Complete differs from persisted enrollment intent".into());
        }
        let bootstrap = enrollment.bootstrap();
        let mut peer = StatePeer::from_join(
            &bootstrap.room,
            bootstrap.host.clone(),
            bootstrap.certified_roster.clone(),
            &bootstrap.revocations,
            now,
        )?;
        let app_messages = install_governance(&mut peer, bootstrap, now)?;
        tracing::debug!(
            %workspace,
            app_messages,
            "a workspace was installed on its governance; its app data follows through the workspace's sync"
        );
        if !peer.has_governance() {
            return Err("native enrollment is missing real admitted governance".into());
        }
        let role = peer
            .permissions(enrollment.local_device().device_public_key, now)
            .map_err(|error| error.to_string())?
            .role;
        // A device of the governing person is the workspace's owner whatever its
        // invitation granted: the owner holds its workspace like any member.
        let expected =
            if enrollment.local_device().user_public_key == bootstrap.host.user_public_key {
                collab_types::Role::Owner
            } else {
                enrollment.verified_join().grant().role.into()
            };
        if role != expected {
            return Err("actual admitted role differs from signed enrollment role".into());
        }

        let record = StoredWorkspace {
            version: 2,
            workspace,
            workspace_id: bootstrap.workspace_id,
            workspace_host: bootstrap.host.clone(),
            device: enrollment.local_device().clone(),
            enrollment: Some(crate::owner::record::StoredEnrollment {
                invitation: enrollment.verified_join().signed_invitation().to_vec(),
                enrollment_intent: pending.intent.clone(),
                enrolled_at_ns: ns(now)?,
                transport_hint: enrollment.transport_hint().clone(),
            }),
            vouched: None,
            host_hint: None,
            peer: peer.durable_parts()?.into(),
            generation: 1,
            continuity: 0,
            publication: Some(publication::StoredPublication::default()),
            workspace_memberships: std::collections::BTreeMap::new(),
            effect_receipts: std::collections::BTreeMap::new(),
            shortcut_change: None,
            effect_history_root: None,
            join_ledger: crate::join_ledger::JoinLedger::default(),
            join_admissions: std::collections::BTreeMap::new(),
        };
        self.publish_workspace(
            workspace,
            NativeCommittedView::new(peer, record),
            Some(&pending),
        )
    }
}
/// Admit the workspace's governance from `bootstrap` into `peer`, in the order
/// the host published it, and answer how many app messages it left out.
///
/// Becoming a member takes the workspace's governance alone: its committed
/// document, holding its membership, roles and policy and the certificates
/// they import, which are everything an app message is authorized against. An
/// app message is not part of it: a host's record publications reach the
/// device through the workspace's sync once the workspace is installed, where
/// each is authorized exactly as any later write is.
pub(crate) fn install_governance(
    peer: &mut StatePeer,
    bootstrap: &crate::join::JoinBootstrap,
    now: SystemTime,
) -> Result<usize, String> {
    let origin = TransportOrigin::RetainedRelayDelta(bootstrap.host.device_public_key);
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
