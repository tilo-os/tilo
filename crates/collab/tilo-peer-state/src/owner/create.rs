//! Founding a workspace: one this device hosts itself, with no host of
//! anyone else's to enroll with. The workspace starts from the canonical seed
//! (its identity, its governance and its name) and is published like any
//! other change this device makes, so whoever holds a copy of it — a store
//! device that keeps only its ciphertext, another member's device — has it
//! from the moment it exists.
use crate::join::TransportHint;
use crate::owner::record::StoredWorkspace;
use crate::owner::{NativeCommittedView, NativeWorkspaceOwner, WorkspaceId, publication};
use crate::workspace_membership::valid_workspace_name;
use crate::{StatePeer, workspace_seed};
use collab_types::signer::DeviceSigner;
use neighbour_wire::NeighbourMsg;
use tilo_user_identity::{DeviceCert, RevocationList};
use web_time::SystemTime;

impl NativeWorkspaceOwner {
    /// Found `workspace` — the identity the workspace was made with, before
    /// anything of it was held here — named `name`, on `device`, signed by
    /// `signer`, its custody, outside the owner's turn. The workspace is new, its host is this
    /// device, and its governance admits any other device the owner invites.
    /// `host_hint` is where this device's host keeps the workspace's records,
    /// signed by `device` itself: a workspace founded here is reached at its
    /// own host, and the device that can act as its own host says where. A
    /// device with no address of its own founds the workspace without one,
    /// and names it in the record once it has one ([`Self::name_host`]).
    /// Nothing of it is its founder's private state: it is the workspace's,
    /// published like every other change this device makes.
    ///
    /// # Errors
    /// The workspace has no identity, the name is not 1–80 characters, `signer`
    /// is not `device`'s, signing failed, `device` may not own workspaces, `host_hint` is not
    /// this workspace's address signed by `device`, or the record cannot be
    /// published.
    pub fn create_workspace(
        &self,
        workspace: WorkspaceId,
        name: &str,
        device: &DeviceCert,
        signer: &dyn DeviceSigner,
        host_hint: Option<&TransportHint>,
        now: SystemTime,
    ) -> Result<WorkspaceId, String> {
        let name = valid_workspace_name(name)?;
        if device.device_public_key != signer.device_public_key() {
            return Err("a workspace is founded with its own device's key".into());
        }
        if !device.capabilities.can_own_workspaces {
            return Err("this device may not own workspaces".into());
        }
        if workspace.is_nil() {
            return Err("a founded workspace has a canonical workspace".into());
        }
        crate::owner::write::signed_turn(signer, |key| {
            let _turn = self.0.mutation.lock();
            let id = workspace;
            if self.0.authority.read().workspaces.contains_key(&id) {
                return Err("this device holds that workspace already".into());
            }
            let workspace = id.to_string();
            let workspace_id =
                crate::join::room_workspace_id(&workspace).map_err(|error| error.to_string())?;
            if let Some(host_hint) = &host_hint {
                crate::owner::validate_host_hint(host_hint, &workspace, workspace_id, device)?;
            }
            let roster = vec![device.clone()];
            let mut peer = StatePeer::new(
                &workspace,
                device.clone(),
                roster.clone(),
                RevocationList::new(),
                now,
            )?;
            peer.select_admission_v5().map_err(|error| {
                format!("the workspace exceeds exact admission bounds: {error}")
            })?;
            let mut seed = workspace_seed(&workspace, device, &roster, name)?;
            // A device that takes part in a workspace's channels opens its records
            // as sealed channels: a record seeded in the clear would be refused
            // by every member that joins after the channel opened, and a store
            // device keeps only what is sealed. The workspace starts with its
            // governance; its records arrive sealed, on the channels its
            // members open.
            seed.documents.clear();
            peer.install_seed(seed, key, now)?;
            peer.governance_mut()
                .reconcile_administrator_grants(key, now)?;
            peer.media_mut().reconcile_media_governance(key, now)?;
            peer.media().validate_media_governance_for_publication()?;
            // What the workspace holds of this device's signing, sent before it is
            // referred to: a member that joins from this device's copy later
            // has it from the workspace itself.
            let messages = peer
                .publication_mut()
                .durable_bootstrap_messages(key, now, 1)?;
            let mut view = NativeCommittedView::new(
                peer.clone(),
                StoredWorkspace {
                    version: 2,
                    workspace: id,
                    workspace_id,
                    workspace_host: device.clone(),
                    device: device.clone(),
                    // No other device admitted this one into its own workspace, so
                    // there is neither an enrollment nor a vouch to hold; where its
                    // own host is reached is what `host_hint` says.
                    enrollment: None,
                    vouched: None,
                    host_hint: host_hint.cloned(),
                    peer: peer.durable()?,
                    generation: 1,
                    continuity: 0,
                    publication: Some(publication::StoredPublication::default()),
                    workspace_memberships: std::collections::BTreeMap::new(),
                    effect_receipts: std::collections::BTreeMap::new(),
                    shortcut_change: None,
                    effect_history_root: None,
                    join_ledger: crate::join_ledger::JoinLedger::default(),
                    join_admissions: std::collections::BTreeMap::new(),
                },
            );
            for message in messages {
                // The workspace's own part of the seed is its governance, which
                // every joiner is answered with; what else was published — the
                // sealed records and their keys — waits here like any other
                // outgoing publication.
                if matches!(message, NeighbourMsg::AppDocument { .. }) {
                    let bytes = publication::prepare(&publication::request(&message)?)?;
                    publication::append_local(&mut view.record, bytes)?;
                }
            }
            key.settled()?;
            self.publish_workspace(id, view, None)?;
            Ok(id)
        })
    }

    /// Record where this device's own host of `workspace` is reached: the
    /// hint [`Self::create_workspace`] writes as it founds one, for a
    /// workspace founded before its record said, and again once the relays it
    /// answers through changed — those are what its people dial, and an
    /// address naming the network the host stood in a moment ago reaches
    /// nobody. A record naming another address on the same relays keeps it: a
    /// net report moves the addresses beside them, and its people pay for
    /// every record published. A workspace this device does not host, or one
    /// whose record reaches its host through an enrollment or a vouch, keeps
    /// what it has. Answers whether it wrote.
    ///
    /// # Errors
    /// The workspace is not held here, `hint` is not this workspace's address
    /// signed by this device, or the record cannot be published.
    pub fn name_host(&self, workspace: WorkspaceId, hint: TransportHint) -> Result<bool, String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        // What the record names is this device's own host only when this
        // device hosts it: a record another device hosts, or one that reaches
        // its host through an enrollment or a vouch, says where that host is
        // reached and is left as it is.
        if candidate.record.device != candidate.record.workspace_host
            || candidate.record.enrollment.is_some()
            || candidate
                .record
                .vouched
                .as_ref()
                .is_some_and(|vouched| vouched.hint.is_some())
        {
            return Ok(false);
        }
        if let Some(named) = &candidate.record.host_hint
            && host_relays(named)? == host_relays(&hint)?
        {
            return Ok(false);
        }
        crate::owner::validate_host_hint(
            &hint,
            &workspace.to_string(),
            candidate.record.workspace_id,
            &candidate.record.device,
        )?;
        let mut record = candidate.record.clone();
        record.host_hint = Some(hint);
        self.publish_record(workspace, &candidate, record, None)?;
        Ok(true)
    }
}

/// The relays `hint` says a host is reached through, which are what reaches
/// it from outside its own network.
fn host_relays(hint: &TransportHint) -> Result<std::collections::BTreeSet<String>, String> {
    let (payload, _) =
        tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1::authenticate_transport_hint(
            hint,
        )
        .map_err(|error| error.to_string())?;
    Ok(payload
        .endpoint_addr
        .relay_urls()
        .map(ToString::to_string)
        .collect())
}
