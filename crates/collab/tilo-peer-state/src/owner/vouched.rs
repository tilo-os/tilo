//! Workspaces a device holds on its host's word rather than an enrollment of its
//! own: a page reaches its workspaces through its person's host, which answers
//! each subscription with the workspace's membership answer
//! ([`tilo_relay_protocol::Envelope::WorkspaceBootstrap`]) over the session the
//! page authenticated to it. The page holds the workspace from that answer as a
//! native device holds a workspace it enrolled in; the workspace's governance arrives
//! with the replay that follows and is admitted like any other.
use crate::StatePeer;
use crate::join::JoinTransportResponse;
use crate::owner::record::{StoredVouch, StoredWorkspace};
use crate::owner::{NativeCommittedView, NativeWorkspaceOwner, WorkspaceId, publication};
use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
use tilo_user_identity::{DeviceCert, RevocationEntry};
use web_time::SystemTime;

/// What a membership answer says: the device it answers, the workspace and its
/// workspace, the host that answered, roster and revocations.
struct Membership {
    device: DeviceCert,
    workspace: WorkspaceId,
    host: DeviceCert,
    roster: Vec<DeviceCert>,
    revocations: Vec<RevocationEntry>,
}

fn membership(bytes: &[u8]) -> Result<Membership, String> {
    let (device, workspace, host, roster, revocations) =
        match postcard::from_bytes(bytes).map_err(|error| error.to_string())? {
            JoinTransportResponse::MembershipComplete(answer) => (
                answer.device_cert,
                answer.workspace_id,
                answer.host,
                answer.certified_roster,
                answer.revocations,
            ),
            JoinTransportResponse::OwnerMembershipComplete(answer) => (
                answer.device_cert,
                answer.workspace_id,
                answer.host,
                answer.certified_roster,
                answer.revocations,
            ),
            _ => return Err("the workspace's bootstrap is no membership answer".into()),
        };
    Ok(Membership {
        device,
        workspace,
        host,
        roster,
        revocations,
    })
}

/// Check a restored record against the membership answer it was installed
/// from.
pub(crate) fn validate(
    workspace: WorkspaceId,
    record: &StoredWorkspace,
    bytes: &[u8],
) -> Result<(), String> {
    let answer = membership(bytes)?;
    if answer.workspace != workspace
        || answer.host != record.workspace_host
        || answer.device.device_public_key != record.device.device_public_key
    {
        return Err(
            "native committed workspace differs from the membership its host vouched".into(),
        );
    }
    Ok(())
}

impl NativeWorkspaceOwner {
    /// Hold the workspace of `workspace_id` as this device's host vouched for
    /// it: `bootstrap` is the membership answer (a postcard
    /// [`JoinTransportResponse`]) the host sent `device` over its
    /// authenticated session. Answers whether that installed the workspace: a
    /// workspace already held through the same host stays as it is, so a
    /// resumed subscription keeps what it admitted, while one the workspace's
    /// governance moved to another host starts again from the answer.
    ///
    /// # Errors
    /// The answer is not a membership answer for `device` and this workspace, a
    /// workspace held through an enrollment would be replaced, or the record
    /// cannot be published.
    pub fn install_vouched(
        &self,
        workspace_id: WorkspaceId,
        bootstrap: &[u8],
        device: &DeviceCert,
        now: SystemTime,
    ) -> Result<bool, String> {
        let answer = membership(bootstrap)?;
        if answer.workspace != workspace_id {
            return Err("the workspace's bootstrap names another workspace".into());
        }
        if answer.device.device_public_key != device.device_public_key {
            return Err("the workspace's bootstrap names another device".into());
        }
        let workspace = answer.workspace;
        let _turn = self.0.mutation.lock();
        if let Some(held) = self.0.authority.read().workspaces.get(&workspace) {
            // An enrollment of another device is the one this machine was
            // copied from: never this device's to act with, so the membership
            // its host answered for this device takes its place.
            let this_device = held.device.device_public_key == device.device_public_key;
            if held.enrollment.is_some() && this_device {
                return Err("the workspace is held through an enrollment".into());
            }
            // The same host keeps what it admitted for this device; a record
            // another device held is replaced whatever its host.
            if this_device
                && held.workspace_host == answer.host
                && held.workspace == answer.workspace
            {
                return Ok(false);
            }
        }
        let peer = StatePeer::from_join(
            &answer.workspace.to_string(),
            answer.host.clone(),
            answer.roster,
            &answer.revocations,
            now,
        )?;
        let record = StoredWorkspace {
            version: 2,
            workspace,
            workspace_host: answer.host,
            device: device.clone(),
            enrollment: None,
            vouched: Some(StoredVouch {
                bootstrap: bootstrap.to_vec(),
                hint: None,
            }),
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
        self.publish_workspace(workspace, NativeCommittedView::new(peer, record), None)?;
        Ok(true)
    }

    /// Hold the workspace of `workspace_id` as one of this person's further
    /// devices: the workspace admits a device certified by a member's root as
    /// that member's device, so `bootstrap` — the host's membership answer —
    /// carries the workspace's governance and its roster, and this device
    /// takes part in the workspace through `hint`, where its host is reached.
    /// Answers whether that installed the workspace: one already held through
    /// the same host stays as it is, while one the workspace's governance
    /// moved to another host starts again from the answer.
    ///
    /// # Errors
    /// The answer is not a membership answer for `device` and this workspace,
    /// it names another host than the hint, `hint` is not the host's own
    /// signed address, the workspace's governance does not admit `device`, a
    /// workspace held through an enrollment would be replaced, or the record
    /// cannot be published.
    pub fn install_member(
        &self,
        workspace_id: WorkspaceId,
        bootstrap: &crate::join::JoinBootstrap,
        hint: crate::join::TransportHint,
        device: &DeviceCert,
        now: SystemTime,
    ) -> Result<bool, String> {
        if bootstrap.workspace_id != workspace_id || bootstrap.device_cert != *device {
            return Err("the workspace's membership answer names another device".into());
        }
        if hint.host.device_public_key != bootstrap.host.device_public_key {
            return Err("the workspace's host hint is not the host that answered".into());
        }
        WorkspaceDeviceHintV1::authenticate_transport_hint(&hint)
            .map_err(|error| error.to_string())?;
        let workspace = bootstrap.workspace_id;
        let _turn = self.0.mutation.lock();
        if let Some(held) = self.0.authority.read().workspaces.get(&workspace) {
            // An enrollment of another device is the one this machine was
            // copied from: never this device's to act with, so the membership
            // its host answered for this device takes its place.
            let this_device = held.device.device_public_key == device.device_public_key;
            if held.enrollment.is_some() && this_device {
                return Err("the workspace is held through an enrollment".into());
            }
            // The same host keeps what it admitted for this device; a record
            // another device held is replaced whatever its host.
            if this_device && held.workspace_host == bootstrap.host && held.workspace == workspace {
                return Ok(false);
            }
        }
        let mut peer = StatePeer::from_join(
            &bootstrap.workspace_id.to_string(),
            bootstrap.host.clone(),
            bootstrap.certified_roster.clone(),
            &bootstrap.revocations,
            now,
        )?;
        crate::owner::enrollment::install_governance(&mut peer, bootstrap, now)?;
        if !peer.has_governance() {
            return Err("a member's workspace arrived without its governance".into());
        }
        peer.permissions(device.device_public_key, now)
            .map_err(|error| error.to_string())?;
        if !peer.certificates().contains(device) {
            return Err("the workspace does not admit this device".into());
        }
        let answer = postcard::to_stdvec(&JoinTransportResponse::MembershipComplete(
            bootstrap.clone(),
        ))
        .map_err(|error| error.to_string())?;
        let record = StoredWorkspace {
            version: 2,
            workspace,
            workspace_host: bootstrap.host.clone(),
            device: device.clone(),
            enrollment: None,
            vouched: Some(StoredVouch {
                bootstrap: answer,
                hint: Some(hint),
            }),
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
        self.publish_workspace(workspace, NativeCommittedView::new(peer, record), None)?;
        Ok(true)
    }
}
