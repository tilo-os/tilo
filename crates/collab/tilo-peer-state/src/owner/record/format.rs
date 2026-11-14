//! The layouts a workspace's entry in the owner record has had, and how an
//! older one comes up to the one this build writes.
//!
//! Version 4 ([`WORKSPACE_VERSION`]) is this build's: the entry names its
//! workspace once, by its id ([`WorkspaceId`]) under `workspace`, and so does
//! every grant and bootstrap in its join ledger.
//!
//! Versions 2 and 3 are what the 0.1 releases wrote (v0.1.5 among them), one
//! layout: 3 marked an entry holding an effect history root, so a build that
//! read no root refused it. That layout named the workspace twice: by its UUID
//! as text under `room`, and under `workspace` by a 64-bit number derived from
//! it. Every grant, challenge and bootstrap in its join ledger carried the
//! number and the text too, and spelled a role `"Viewer"` where this build
//! writes `"viewer"`. [`v3_to_v4`] reads such an entry in that layout and
//! writes it in this build's, the workspace named by the UUID its text gave.
//!
//! The bytes a signature covers stay exactly as they were: a signed
//! invitation, the enrollment's signed join, an issued invitation. Nothing
//! here can sign for them again, and the version inside them is what
//! `tilo_workspace_join` reads their layout by. A membership answer a host
//! vouched for is no signed message, so it is written again in this build's
//! layout.
//!
//! A join or membership challenge waiting for its proof is dropped: a
//! challenge lives for minutes, its version now names this build's layout so
//! no proof of it would verify, and the joining device asks for a new one.
//!
//! The entry this build's development wrote under version 2 with the
//! workspace as a UUID was never released and is not read.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Map, Value};
use tilo_desktop_model::WorkspaceId;
use tilo_files::native::migration::Format;
use tilo_user_identity::{DeviceCert, RevocationEntry};

use super::{NativeAuthorityStorage, WORKSPACE_VERSION};
use crate::join::invitations::IssuedInvitation;
use crate::join::{
    DocumentRow, InviteGrant, JoinBootstrap, JoinTransportResponse, OwnerBootstrapV1, Role,
    TransportHint,
};
use crate::join_ledger::{
    AdministrationRecord, CancellationRecord, InvitationRecord, JoinCompletionRecord, JoinLedger,
};

/// A workspace's entry: versions 2 and 3 (the 0.1 releases') and 4 (this
/// build's). The oldest version is written out rather than derived from
/// [`WORKSPACE_VERSION`]: that a build still brings a version up is a promise
/// about the files devices hold, and it never moves with the version written.
pub(crate) const FORMAT: Format = Format {
    name: "owner workspace record",
    version: WORKSPACE_VERSION,
    oldest: 2,
    steps: &[v2_to_v3, v3_to_v4],
};

/// The version an entry's bytes name.
pub(crate) fn stored_version(bytes: &[u8]) -> Result<u32, String> {
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    serde_json::from_slice::<Versioned>(bytes)
        .map(|entry| entry.version)
        .map_err(|error| format!("{}: {error}", FORMAT.name))
}

/// A record a build before the split wrote as one file, each workspace's
/// entry brought up to this build's layout as it is read. The file itself is
/// left as it is (`file_store` never writes it), and is the copy of the old
/// layout a build that wrote it reads again.
pub(crate) fn read_whole(bytes: &[u8]) -> Result<NativeAuthorityStorage, String> {
    let mut record: Map<String, Value> =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    for key in ["rooms", "workspaces"] {
        let Some(Value::Object(entries)) = record.get_mut(key) else {
            continue;
        };
        for entry in entries.values_mut() {
            let bytes = serde_json::to_vec(entry).map_err(|error| error.to_string())?;
            let stored = stored_version(&bytes)?;
            *entry =
                serde_json::from_slice(&up(bytes, stored)?).map_err(|error| error.to_string())?;
        }
    }
    serde_json::from_value(Value::Object(record)).map_err(|error| error.to_string())
}

/// `bytes` of version `stored` through [`FORMAT`]'s steps from there on.
fn up(mut bytes: Vec<u8>, stored: u32) -> Result<Vec<u8>, String> {
    if stored > FORMAT.version {
        return Err(format!(
            "{} version {stored} was written by a newer build than this one ({})",
            FORMAT.name, FORMAT.version
        ));
    }
    let first = stored
        .checked_sub(FORMAT.oldest)
        .and_then(|offset| usize::try_from(offset).ok())
        .ok_or_else(|| {
            format!(
                "{} version {stored} is older than the oldest this build reads ({})",
                FORMAT.name, FORMAT.oldest
            )
        })?;
    for step in FORMAT.steps.iter().skip(first) {
        bytes = step(bytes)?;
    }
    Ok(bytes)
}

/// An entry's JSON object.
fn entry_of(bytes: &[u8], version: u32) -> Result<Map<String, Value>, String> {
    serde_json::from_slice(bytes)
        .map_err(|error| format!("{} version {version}: {error}", FORMAT.name))
}

fn bytes_of(entry: &Map<String, Value>) -> Result<Vec<u8>, String> {
    serde_json::to_vec(entry).map_err(|error| error.to_string())
}

/// Versions 2 and 3 are one layout: only the number moves.
#[expect(
    clippy::needless_pass_by_value,
    reason = "a migration step takes its bytes by value"
)]
fn v2_to_v3(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut entry = entry_of(&bytes, 2)?;
    entry.insert("version".into(), Value::from(3_u32));
    bytes_of(&entry)
}

/// The 0.1 releases' entry in this build's layout: the workspace named by the
/// UUID its `room` gave, there and in its join ledger and vouched answer.
#[expect(
    clippy::needless_pass_by_value,
    reason = "a migration step takes its bytes by value"
)]
fn v3_to_v4(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut entry = entry_of(&bytes, 3)?;
    let room = entry
        .remove("room")
        .and_then(|room| room.as_str().map(str::to_owned))
        .ok_or("a version 3 workspace record names no room")?;
    let workspace = workspace(&room)?;
    entry.insert("version".into(), Value::from(4_u32));
    entry.insert(
        "workspace".into(),
        serde_json::to_value(workspace).map_err(|error| error.to_string())?,
    );
    if let Some(ledger) = entry.get_mut("join_ledger") {
        let released: Ledger = serde_json::from_value(ledger.take())
            .map_err(|error| format!("a version 3 join ledger: {error}"))?;
        *ledger = serde_json::to_value(released.current()?).map_err(|error| error.to_string())?;
    }
    if let Some(Value::String(answer)) = entry
        .get_mut("vouched")
        .and_then(|vouched| vouched.get_mut("bootstrap"))
    {
        let bytes = STANDARD
            .decode(answer.as_bytes())
            .map_err(|error| error.to_string())?;
        *answer = STANDARD.encode(vouched_answer(&bytes)?);
    }
    bytes_of(&entry)
}

/// The workspace an entry of the releases' layout names by `room`.
fn workspace(room: &str) -> Result<WorkspaceId, String> {
    WorkspaceId::parse(room).ok_or_else(|| format!("version 3 record names {room:?}, not a UUID"))
}

/// A role as the releases spelled it, in the order postcard numbers it.
#[derive(Deserialize)]
enum ReleasedRole {
    Viewer,
    Commenter,
    Editor,
    Moderator,
}

impl From<ReleasedRole> for Role {
    fn from(role: ReleasedRole) -> Self {
        match role {
            ReleasedRole::Viewer => Self::Viewer,
            ReleasedRole::Commenter => Self::Commenter,
            ReleasedRole::Editor => Self::Editor,
            ReleasedRole::Moderator => Self::Moderator,
        }
    }
}

#[derive(Deserialize)]
struct Ledger {
    invitations: Vec<Invitation>,
    #[serde(rename = "join_challenges")]
    _join_challenges: serde::de::IgnoredAny,
    join_completions: Vec<JoinCompletion>,
    #[serde(rename = "membership_challenges")]
    _membership_challenges: serde::de::IgnoredAny,
    administration: Vec<AdministrationRecord>,
}

impl Ledger {
    fn current(self) -> Result<JoinLedger, String> {
        Ok(JoinLedger {
            invitations: self
                .invitations
                .into_iter()
                .map(Invitation::current)
                .collect::<Result<_, _>>()?,
            // Waiting for proofs no build verifies any more; see the module.
            join_challenges: Vec::new(),
            join_completions: self
                .join_completions
                .into_iter()
                .map(JoinCompletion::current)
                .collect::<Result<_, _>>()?,
            membership_challenges: Vec::new(),
            administration: self.administration,
        })
    }
}

#[derive(Deserialize)]
struct Invitation {
    request_id: [u8; 32],
    request_digest: [u8; 32],
    /// As its issuer signed it: its id is the digest of these bytes.
    issued: IssuedInvitation,
    grant: Grant,
    cancellation: Option<CancellationRecord>,
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
                role: grant.role.into(),
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

#[derive(Deserialize)]
struct Grant {
    #[serde(rename = "workspace_id")]
    _number: u64,
    room: String,
    issuer: DeviceCert,
    role: ReleasedRole,
    nonce: [u8; 16],
    recipient_user_public_key: Option<[u8; 32]>,
    invitee_name: Option<String>,
    expires_at_ns: u64,
    transport: Vec<TransportHint>,
}

#[derive(Deserialize)]
struct JoinCompletion {
    remote_endpoint: [u8; 32],
    request_id: [u8; 32],
    proof_digest: [u8; 32],
    bootstrap: Bootstrap,
}

impl JoinCompletion {
    /// A receipt keeps the joiner's own fields and the generation it was
    /// delivered at. The digest the releases kept for the governance it
    /// delivered goes: a replay rebuilds that from the workspace while it is
    /// still at that generation.
    fn current(self) -> Result<JoinCompletionRecord, String> {
        let mut bootstrap = self.bootstrap.current()?;
        bootstrap.governance_messages = Vec::new();
        Ok(JoinCompletionRecord {
            remote_endpoint: self.remote_endpoint,
            request_id: self.request_id,
            proof_digest: self.proof_digest,
            bootstrap,
        })
    }
}

/// A join bootstrap of the releases' layout, as JSON in a ledger and as
/// postcard in a vouched answer: the field order is postcard's.
#[derive(Deserialize)]
struct Bootstrap {
    version: u32,
    invite_id: [u8; 32],
    device_cert: DeviceCert,
    role: ReleasedRole,
    room: String,
    #[serde(rename = "workspace_id")]
    _number: u64,
    #[serde(alias = "authority")]
    host: DeviceCert,
    certified_roster: Vec<DeviceCert>,
    revocations: Vec<RevocationEntry>,
    records: Vec<DocumentRow>,
    governance_messages: Vec<Vec<u8>>,
    generation: u64,
}

impl Bootstrap {
    fn current(self) -> Result<JoinBootstrap, String> {
        Ok(JoinBootstrap {
            version: self.version,
            invite_id: self.invite_id,
            device_cert: self.device_cert,
            role: self.role.into(),
            workspace_id: workspace(&self.room)?,
            host: self.host,
            certified_roster: self.certified_roster,
            revocations: self.revocations,
            records: self.records,
            governance_messages: self.governance_messages,
            generation: self.generation,
        })
    }
}

/// An owner's membership answer of the releases' layout, postcard.
#[derive(Deserialize)]
struct OwnerBootstrap {
    version: u32,
    device_cert: DeviceCert,
    room: String,
    _number: u64,
    host: DeviceCert,
    certified_roster: Vec<DeviceCert>,
    revocations: Vec<RevocationEntry>,
    records: Vec<DocumentRow>,
    governance_messages: Vec<Vec<u8>>,
    generation: u64,
}

impl OwnerBootstrap {
    fn current(self) -> Result<OwnerBootstrapV1, String> {
        Ok(OwnerBootstrapV1 {
            version: self.version,
            device_cert: self.device_cert,
            workspace_id: workspace(&self.room)?,
            host: self.host,
            certified_roster: self.certified_roster,
            revocations: self.revocations,
            records: self.records,
            governance_messages: self.governance_messages,
            generation: self.generation,
        })
    }
}

/// [`JoinTransportResponse::MembershipComplete`]'s place among the response's
/// variants, the same in both layouts.
const MEMBERSHIP_COMPLETE: u32 = 4;
/// [`JoinTransportResponse::OwnerMembershipComplete`]'s.
const OWNER_MEMBERSHIP_COMPLETE: u32 = 6;

/// A vouched membership answer (a postcard [`JoinTransportResponse`]) of the
/// releases' layout, in this build's.
fn vouched_answer(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let malformed = |error: postcard::Error| format!("a version 3 vouched answer: {error}");
    let (variant, rest) = postcard::take_from_bytes::<u32>(bytes).map_err(malformed)?;
    let answer = match variant {
        MEMBERSHIP_COMPLETE => JoinTransportResponse::MembershipComplete(
            postcard::from_bytes::<Bootstrap>(rest)
                .map_err(malformed)?
                .current()?,
        ),
        OWNER_MEMBERSHIP_COMPLETE => JoinTransportResponse::OwnerMembershipComplete(
            postcard::from_bytes::<OwnerBootstrap>(rest)
                .map_err(malformed)?
                .current()?,
        ),
        _ => return Err("a version 3 vouched answer is no membership answer".into()),
    };
    postcard::to_stdvec(&answer).map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
