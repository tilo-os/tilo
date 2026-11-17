//! A governed workspace record: one postcard struct whose field order is the
//! format, holding what the files beside it do not.
//!
//! The record ([`StoredResource`]) holds the publications and join receipts not
//! in the files beside it yet ([`super::appended`]), and how many each file
//! holds, its workspace named by its id as a UUID ([`WorkspaceId`])
//! throughout.

use serde::{Deserialize, Serialize};
use tilo_files::native::migration::Format;
use tilo_peer_state::join_log::{
    AdministrationRecord, InvitationEntry, JoinChallengeRecord, JoinCompletionRecord, JoinLog,
    MembershipChallengeRecord,
};
use tilo_relay_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::{DOCUMENT_VERSION, Publication};

/// The record's layout.
pub(super) const FORMAT: Format = Format {
    name: "governed workspace document",
    version: DOCUMENT_VERSION,
    oldest: 4,
    steps: &[without_name],
};

/// Version 4 to 5: the record keeps no name of its own. The workspace's name
/// is its governance's, which the record's peer state holds and which version
/// 4 kept a copy of beside it; the copy goes, and every byte after it stays.
fn without_name(mut bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let read = |error: postcard::Error| format!("a version 4 record: {error}");
    let (_version, rest) = postcard::take_from_bytes::<u32>(&bytes).map_err(read)?;
    let (workspace, rest) = postcard::take_from_bytes::<String>(rest).map_err(read)?;
    let (workspace_id, rest) = postcard::take_from_bytes::<WorkspaceId>(rest).map_err(read)?;
    let (_name, rest) = postcard::take_from_bytes::<String>(rest).map_err(read)?;
    let named = bytes.len().saturating_sub(rest.len());
    let head = postcard::to_stdvec(&(5_u32, workspace, workspace_id))
        .map_err(|error| error.to_string())?;
    bytes.splice(..named, head);
    Ok(bytes)
}

/// The record as it is written: the publications and join receipts not in the
/// files beside it yet, and how many those hold.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredResource {
    pub(super) version: u32,
    pub(super) workspace: String,
    pub(super) workspace_id: WorkspaceId,
    pub(super) authority: DeviceCert,
    pub(super) peer: Vec<super::peer_parts::PeerPart>,
    pub(super) generation: u64,
    /// The publications after the ones the publications file holds.
    pub(super) outgoing: Vec<Publication>,
    pub(super) published: usize,
    /// The log, its join receipts those after the receipts file's.
    pub(super) collaboration: JoinLog,
    /// How many publications the publications file holds, the first of the
    /// workspace's.
    pub(super) filed_publications: u64,
    /// How many join receipts the receipts file holds, the first of the
    /// log's.
    pub(super) filed_receipts: u64,
}

/// [`StoredResource`] borrowed from the record held, as it is written.
#[derive(Serialize)]
pub(super) struct StoredDocumentRef<'a> {
    pub(super) version: u32,
    pub(super) workspace: &'a str,
    pub(super) workspace_id: WorkspaceId,
    pub(super) authority: &'a DeviceCert,
    pub(super) peer: &'a [super::peer_parts::PeerPart],
    pub(super) generation: u64,
    pub(super) outgoing: &'a [Publication],
    pub(super) published: usize,
    pub(super) collaboration: LogRef<'a>,
    pub(super) filed_publications: u64,
    pub(super) filed_receipts: u64,
}

/// The join log as a written record holds it, borrowed: its receipts
/// those the receipts file does not hold. The log's own layout.
#[derive(Serialize)]
pub(super) struct LogRef<'a> {
    pub(super) invitations: &'a [InvitationEntry],
    pub(super) join_challenges: &'a [JoinChallengeRecord],
    pub(super) join_completions: &'a [JoinCompletionRecord],
    pub(super) membership_challenges: &'a [MembershipChallengeRecord],
    pub(super) administration: &'a [AdministrationRecord],
}

#[cfg(test)]
mod tests;
