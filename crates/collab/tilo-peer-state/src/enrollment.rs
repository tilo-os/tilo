use crate::owner::stored::{StoredPendingEnrollment, StoredWorkspace};
use crate::owner::{
    Change, NativeAuthorityStorage, NativeCommittedView, NativeWorkspaceOwner, PendingEnrollmentId,
    PendingEnrollmentStatus, VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent, limits,
    stored,
};
use crate::{NeighbourMsg, StatePeer, TransportOrigin};
use tilo_desktop_model::WorkspaceId;
use web_time::SystemTime;

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
    if record.version != crate::owner::stored::WORKSPACE_VERSION
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
            crate::vouched::validate(workspace, record, &vouched.bootstrap)?;
        }
        // A workspace this device's own host keeps was founded here
        // ([`NativeWorkspaceOwner::create_workspace`]): no other device
        // admitted this one into it, so there is neither an enrollment nor
        // a vouch to hold, and where its own host is reached is the hint
        // the record carries.
        (None, None) if record.device == record.workspace_host => {
            if let Some(hint) = &record.host_hint {
                crate::owner::validate_host_hint(hint, record.workspace, &record.device)?;
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
    if !peer.exact_admissions_selected() {
        return Err("native committed workspace requires exact admissions".into());
    }
    let view = NativeCommittedView::new(peer, record.clone());
    Ok(view)
}
fn validate_enrollment(
    workspace: WorkspaceId,
    record: &StoredWorkspace,
    enrollment: &crate::owner::stored::StoredEnrollment,
) -> Result<(), String> {
    let intent = VerifiedNativeEnrollmentIntent::restore(&enrollment.enrollment_intent)?;
    let join = intent.verified_join();
    let grant = join.grant();
    if intent.workspace() != workspace
        || record.workspace_host != *join.workspace_authority()
        || record.workspace != grant.workspace_id
        || (record.device != *intent.local_device()
            && !crate::owner::renewal::renews(&record.device, intent.local_device()))
        || enrollment.transport_hint != *intent.transport_hint()
        || enrollment.invitation != join.signed_invitation()
    {
        return Err("native committed workspace differs from its authenticated enrollment".into());
    }
    Ok(())
}

impl NativeWorkspaceOwner {
    /// Persist the signed Complete proof before it is sent, so a lost
    /// response is retried with the exact same proof. `local` is the
    /// workspace on the host's shell the enrollment installs into, made for it.
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
                return Err("native enrollment ownership is fenced".into());
            }
        }
        let mut next = self.0.authority.read().clone();
        if next.workspaces.contains_key(&workspace) {
            return Err(
                "workspace is already enrolled; identity replacement requires explicit ownership handoff"
                    .into(),
            );
        }
        if local.is_some_and(|local| local != workspace) {
            return Err("enrollment workspace is another workspace's".into());
        }
        let shown = local.is_some();
        if shown && !self.0.store.shows(workspace) {
            return Err("the workspace the enrollment installs into is gone".into());
        }
        if let Some(pending) = next.pending.get(&workspace) {
            if pending.intent == bytes
                && pending.status == PendingEnrollmentStatus::Active
                && pending.shown == shown
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
                shown,
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
    /// The in-flight enrollment redeeming `invitation`, into no workspace on the host's shell.
    pub fn pending_enrollment_for_invitation(
        &self,
        invitation: &str,
    ) -> Result<Option<StoredPendingEnrollment>, String> {
        if !self.0.store.healthy() {
            return Err("native state writer is fenced".into());
        }
        let unshown: Vec<_> = self
            .0
            .authority
            .read()
            .pending
            .values()
            .filter(|pending| !pending.shown)
            .cloned()
            .collect();
        for pending in unshown {
            let intent = VerifiedNativeEnrollmentIntent::restore(&pending.intent)?;
            if crate::join::encode_invitation_text(intent.verified_join().signed_invitation())
                == invitation
            {
                return Ok(Some(pending));
            }
        }
        Ok(None)
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
    ) -> Result<(), crate::owner::InstallError> {
        let _turn = self.0.mutation.lock();
        let workspace = enrollment.bootstrap().workspace_id;
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
            &bootstrap.workspace_id.to_string(),
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
            .role(&enrollment.local_device().device_public_key, now)
            .map_err(|error| error.to_string())?;
        // A device of the governing person is the workspace's owner whatever its
        // invitation granted: the owner holds its workspace like any member.
        let expected =
            if enrollment.local_device().user_public_key == bootstrap.host.user_public_key {
                collab_types::Role::Owner
            } else {
                enrollment.verified_join().grant().role
            };
        if role != expected {
            return Err("actual admitted role differs from signed enrollment role".into());
        }

        let record = StoredWorkspace {
            version: crate::owner::stored::WORKSPACE_VERSION,
            workspace,
            workspace_host: bootstrap.host.clone(),
            device: enrollment.local_device().clone(),
            enrollment: Some(crate::owner::stored::StoredEnrollment {
                invitation: enrollment.verified_join().signed_invitation().to_vec(),
                enrollment_intent: pending.intent.clone(),
                enrolled_at_ns: ns(now)?,
                transport_hint: enrollment.transport_hint().clone(),
            }),
            vouched: None,
            host_hint: None,
            peer: peer.durable_layout()?.into(),
            generation: 1,
            continuity: 0,
            publication: None,
            frames: Vec::new(),
        };
        Ok(self.publish_workspace(
            workspace,
            NativeCommittedView::new(peer, record),
            Some(&pending),
        )?)
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
) -> Result<usize, crate::AdmissionError> {
    let origin = TransportOrigin::RetainedServerDelta(bootstrap.host.device_public_key);
    let mut app_messages = 0usize;
    for bytes in &bootstrap.governance_messages {
        let message: NeighbourMsg = neighbour_wire::NeighbourMsg::decode(bytes)
            .map_err(|e| e.to_string())
            .map_err(crate::AdmissionError::malformed)?;
        if matches!(message, NeighbourMsg::AppDocument { .. }) {
            app_messages = app_messages.saturating_add(1);
            continue;
        }
        // Only the closed, pinned Complete evidence selects historical
        // admission here; ongoing relay input uses strict receive_from_report.
        // The origin labels that admission context, not a server frame.
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
