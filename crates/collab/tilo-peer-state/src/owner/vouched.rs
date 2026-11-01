//! Rooms a device holds on its host's word rather than an enrollment of its
//! own: a page reaches its rooms through its person's host, which answers
//! each subscription with the room's membership answer
//! ([`tilo_relay_protocol::Envelope::RoomBootstrap`]) over the session the
//! page authenticated to it. The page holds the room from that answer as a
//! native device holds a room it enrolled in; the room's governance arrives
//! with the replay that follows and is admitted like any other.
use crate::StatePeer;
use crate::join::JoinTransportResponse;
use crate::owner::record::{StoredRoom, StoredVouch};
use crate::owner::{NativeCommittedView, NativeWorkspaceOwner, RoomId, publication};
use tilo_user_identity::{DeviceCert, RevocationEntry};
use web_time::SystemTime;

/// What a membership answer says: the device it answers, the room and its
/// workspace, the host that answered, roster and revocations.
struct Membership {
    device: DeviceCert,
    room: String,
    workspace: u64,
    host: DeviceCert,
    roster: Vec<DeviceCert>,
    revocations: Vec<RevocationEntry>,
}

fn membership(bytes: &[u8]) -> Result<Membership, String> {
    let (device, room, workspace, host, roster, revocations) =
        match postcard::from_bytes(bytes).map_err(|error| error.to_string())? {
            JoinTransportResponse::MembershipComplete(answer) => (
                answer.device_cert,
                answer.room,
                answer.workspace_id,
                answer.host,
                answer.certified_roster,
                answer.revocations,
            ),
            JoinTransportResponse::OwnerMembershipComplete(answer) => (
                answer.device_cert,
                answer.room,
                answer.workspace_id,
                answer.host,
                answer.certified_roster,
                answer.revocations,
            ),
            _ => return Err("the room's bootstrap is no membership answer".into()),
        };
    Ok(Membership {
        device,
        room,
        workspace,
        host,
        roster,
        revocations,
    })
}

/// Check a restored record against the membership answer it was installed
/// from.
pub(crate) fn validate(room: RoomId, record: &StoredRoom, bytes: &[u8]) -> Result<(), String> {
    let answer = membership(bytes)?;
    if RoomId::parse(&answer.room) != Some(room)
        || answer.workspace != record.workspace
        || answer.host != record.room_host
        || answer.device.device_public_key != record.device.device_public_key
    {
        return Err("native committed room differs from the membership its host vouched".into());
    }
    Ok(())
}

impl NativeWorkspaceOwner {
    /// Hold the room of `workspace_id` as this device's host vouched for
    /// it: `bootstrap` is the membership answer (a postcard
    /// [`JoinTransportResponse`]) the host sent `device` over its
    /// authenticated session. Answers whether that installed the room: a
    /// room already held through the same host stays as it is, so a
    /// resumed subscription keeps what it admitted, while one the room's
    /// governance moved to another host starts again from the answer.
    ///
    /// # Errors
    /// The answer is not a membership answer for `device` and this room, a
    /// room held through an enrollment would be replaced, or the record
    /// cannot be published.
    pub fn install_vouched(
        &self,
        workspace_id: u64,
        bootstrap: &[u8],
        device: &DeviceCert,
        now: SystemTime,
    ) -> Result<bool, String> {
        let answer = membership(bootstrap)?;
        if answer.workspace != workspace_id {
            return Err("the room's bootstrap names another room".into());
        }
        if answer.device.device_public_key != device.device_public_key {
            return Err("the room's bootstrap names another device".into());
        }
        let room = RoomId::parse(&answer.room).ok_or("noncanonical vouched room")?;
        let _turn = self.0.mutation.lock();
        if let Some(held) = self.0.authority.read().rooms.get(&room) {
            if held.enrollment.is_some() {
                return Err("the room is held through an enrollment".into());
            }
            if held.room_host == answer.host && held.workspace == workspace_id {
                return Ok(false);
            }
        }
        let peer = StatePeer::from_join(
            &answer.room,
            answer.host.clone(),
            answer.roster,
            &answer.revocations,
            now,
        )?;
        let record = StoredRoom {
            version: 2,
            room,
            workspace: workspace_id,
            room_host: answer.host,
            device: device.clone(),
            enrollment: None,
            vouched: Some(StoredVouch {
                bootstrap: bootstrap.to_vec(),
            }),
            peer: peer.durable()?,
            private_direct_conversations: crate::owner::PrivateDirectConversationStore::default(),
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
        self.publish_room(room, NativeCommittedView { peer, record }, None)?;
        Ok(true)
    }
}
