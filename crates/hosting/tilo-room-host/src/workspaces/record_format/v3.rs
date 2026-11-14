//! The version 3 record, as the 0.1 releases wrote it: read, never written.
//!
//! Every type here is the layout of that day, field for field. Where it named
//! the workspace it held the 64-bit number the releases derived from its
//! UUID, and where it named it again, the UUID as text; [`Record::current`]
//! keeps the UUID as this build's [`WorkspaceId`] and drops the number.

use serde::{Deserialize, Serialize};
use tilo_peer_state::join::invitations::IssuedInvitation;
use tilo_peer_state::join::{DocumentRow, InviteGrant, JoinBootstrap, Role, TransportHint};
use tilo_peer_state::join_ledger::{
    AdministrationRecord, CancellationRecord, InvitationRecord, JoinCompletionRecord, JoinLedger,
};
use tilo_relay_protocol::WorkspaceId;
use tilo_user_identity::{DeviceCert, RevocationEntry};

use super::super::Publication;
use super::super::peer_parts::PeerPart;

/// The workspace a version 3 record names by `room`, its UUID as text.
fn workspace(room: &str) -> Result<WorkspaceId, String> {
    WorkspaceId::parse(room).ok_or_else(|| format!("version 3 record names {room:?}, not a UUID"))
}

/// A governed workspace record, version 3.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Record {
    pub(super) version: u32,
    pub(super) workspace: String,
    /// The number the releases derived from `workspace`.
    pub(super) workspace_id: u64,
    pub(super) name: String,
    pub(super) authority: DeviceCert,
    pub(super) peer: Vec<PeerPart>,
    pub(super) generation: u64,
    pub(super) outgoing: Vec<Publication>,
    pub(super) published: usize,
    pub(super) collaboration: Ledger,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Ledger {
    pub(super) invitations: Vec<Invitation>,
    pub(super) join_challenges: Vec<JoinChallengeEntry>,
    pub(super) join_completions: Vec<JoinCompletion>,
    pub(super) membership_challenges: Vec<MembershipChallengeEntry>,
    pub(super) administration: Vec<AdministrationRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Invitation {
    pub(super) request_id: [u8; 32],
    pub(super) request_digest: [u8; 32],
    /// The invitation as its issuer signed it, in the releases' layout: its
    /// bytes are what its id is the digest of, and stay as they are.
    pub(super) issued: IssuedInvitation,
    pub(super) grant: Grant,
    pub(super) cancellation: Option<CancellationRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Grant {
    pub(super) workspace_id: u64,
    pub(super) room: String,
    pub(super) issuer: DeviceCert,
    pub(super) role: Role,
    pub(super) nonce: [u8; 16],
    pub(super) recipient_user_public_key: Option<[u8; 32]>,
    pub(super) invitee_name: Option<String>,
    pub(super) expires_at_ns: u64,
    pub(super) transport: Vec<TransportHint>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct JoinChallengeEntry {
    pub(super) remote_endpoint: [u8; 32],
    pub(super) request_digest: [u8; 32],
    pub(super) device_cert: DeviceCert,
    pub(super) challenge: Challenge,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Challenge {
    pub(super) version: u32,
    pub(super) signed_invitation: Vec<u8>,
    pub(super) room: String,
    pub(super) workspace_id: u64,
    pub(super) user_public_key: [u8; 32],
    pub(super) device_public_key: [u8; 32],
    pub(super) request_id: [u8; 32],
    pub(super) nonce: [u8; 32],
    pub(super) expires_at_ns: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct JoinCompletion {
    pub(super) remote_endpoint: [u8; 32],
    pub(super) request_id: [u8; 32],
    pub(super) proof_digest: [u8; 32],
    pub(super) bootstrap: Bootstrap,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Bootstrap {
    pub(super) version: u32,
    pub(super) invite_id: [u8; 32],
    pub(super) device_cert: DeviceCert,
    pub(super) role: Role,
    pub(super) room: String,
    pub(super) workspace_id: u64,
    pub(super) host: DeviceCert,
    pub(super) certified_roster: Vec<DeviceCert>,
    pub(super) revocations: Vec<RevocationEntry>,
    pub(super) records: Vec<DocumentRow>,
    /// One entry: the digest of the governance the reply delivered.
    pub(super) governance_messages: Vec<Vec<u8>>,
    pub(super) generation: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct MembershipChallengeEntry {
    pub(super) remote_endpoint: [u8; 32],
    pub(super) request_digest: [u8; 32],
    pub(super) device_cert: DeviceCert,
    pub(super) challenge: Membership,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Membership {
    pub(super) version: u32,
    pub(super) room: String,
    pub(super) workspace_id: u64,
    pub(super) host_device_public_key: [u8; 32],
    pub(super) user_public_key: [u8; 32],
    pub(super) device_public_key: [u8; 32],
    pub(super) request_id: [u8; 32],
    pub(super) nonce: [u8; 32],
    pub(super) expires_at_ns: u64,
}

/// What a version 3 record holds, as this build holds it.
pub(super) struct Current {
    pub(super) workspace: String,
    pub(super) workspace_id: WorkspaceId,
    pub(super) name: String,
    pub(super) authority: DeviceCert,
    pub(super) peer: Vec<PeerPart>,
    pub(super) generation: u64,
    pub(super) outgoing: Vec<Publication>,
    pub(super) published: usize,
    pub(super) collaboration: JoinLedger,
}

impl Record {
    /// The record in this build's types, each ledger entry naming its
    /// workspace by the UUID it carried as text.
    ///
    /// # Errors
    ///
    /// The record or one of its ledger entries names no UUID.
    pub(super) fn current(self) -> Result<Current, String> {
        let workspace_id = workspace(&self.workspace)?;
        let ledger = self.collaboration;
        Ok(Current {
            workspace: self.workspace,
            workspace_id,
            name: self.name,
            authority: self.authority,
            peer: self.peer,
            generation: self.generation,
            outgoing: self.outgoing,
            published: self.published,
            collaboration: JoinLedger {
                invitations: each(ledger.invitations, Invitation::current)?,
                // A challenge waits minutes for its proof, and a device answers
                // it in the layout its version names; the version these were
                // issued under now names this build's layout, so no proof of
                // them would verify. The device asks for a new one.
                join_challenges: Vec::new(),
                join_completions: each(ledger.join_completions, JoinCompletion::current)?,
                membership_challenges: Vec::new(),
                administration: ledger.administration,
            },
        })
    }
}

/// Each of `entries` in this build's types.
fn each<T, U>(entries: Vec<T>, current: fn(T) -> Result<U, String>) -> Result<Vec<U>, String> {
    entries.into_iter().map(current).collect()
}

impl Invitation {
    fn current(self) -> Result<InvitationRecord, String> {
        let grant = self.grant;
        Ok(InvitationRecord {
            request_id: self.request_id,
            request_digest: self.request_digest,
            issued: self.issued,
            grant: InviteGrant {
                workspace_id: workspace(&grant.room)?,
                issuer: grant.issuer,
                role: grant.role,
                nonce: grant.nonce,
                recipient_user_public_key: grant.recipient_user_public_key,
                invitee_name: grant.invitee_name,
                expires_at_ns: grant.expires_at_ns,
                transport: grant.transport,
            },
            cancellation: self.cancellation,
        })
    }
}

impl JoinCompletion {
    /// A receipt keeps the joiner's own fields and the generation it was
    /// delivered at; the workspace's state a replay sends is rebuilt from the
    /// workspace, and the digest that stood for its governance goes.
    fn current(self) -> Result<JoinCompletionRecord, String> {
        let bootstrap = self.bootstrap;
        Ok(JoinCompletionRecord {
            remote_endpoint: self.remote_endpoint,
            request_id: self.request_id,
            proof_digest: self.proof_digest,
            bootstrap: JoinBootstrap {
                version: bootstrap.version,
                invite_id: bootstrap.invite_id,
                device_cert: bootstrap.device_cert,
                role: bootstrap.role,
                workspace_id: workspace(&bootstrap.room)?,
                host: bootstrap.host,
                certified_roster: bootstrap.certified_roster,
                revocations: bootstrap.revocations,
                records: bootstrap.records,
                governance_messages: Vec::new(),
                generation: bootstrap.generation,
            },
        })
    }
}
