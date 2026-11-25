//! A governed workspace record: one postcard struct whose field order is the
//! format, holding what the files beside it do not.
//!
//! The record ([`StoredResource`]) holds the join receipts not in the file
//! beside it yet ([`super::appended`]), and how many that file holds, its
//! workspace named by its id as a UUID ([`WorkspaceId`]) throughout.

use serde::{Deserialize, Serialize};
use tilo_files::native::migration::Format;
use tilo_peer_state::join_log::{
    AdministrationRecord, InvitationEntry, JoinChallengeRecord, JoinCompletionRecord, JoinLog,
    MembershipChallengeRecord,
};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::{DOCUMENT_VERSION, WorkspaceDocumentEntry};

/// The record's layout.
pub(super) const FORMAT: Format = Format {
    name: "governed workspace document",
    version: DOCUMENT_VERSION,
    oldest: 5,
    steps: &[without_what_it_sent],
};

/// Version 5 of the record, which kept the changes the workspace's host
/// published in its own name.
#[derive(Deserialize)]
struct Version5 {
    workspace: String,
    workspace_id: WorkspaceId,
    owner: DeviceCert,
    peer: Vec<super::peer_parts::PeerPart>,
    _generation: u64,
    _outgoing: Vec<(Vec<u8>, bool)>,
    _published: usize,
    collaboration: JoinLog,
    _filed_sends: u64,
    filed_receipts: u64,
}

/// Version 5 brought up to 6: what the host published in its own name, and
/// how far, goes. Each device sends what it wrote itself, and a member's
/// change reaches the others as its device sent it.
fn without_what_it_sent(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let (_, rest) = postcard::take_from_bytes::<u32>(&bytes).map_err(|error| error.to_string())?;
    let kept: Version5 = postcard::from_bytes(rest).map_err(|error| error.to_string())?;
    drop(bytes);
    postcard::to_stdvec(&StoredResource {
        version: 6,
        workspace: kept.workspace,
        workspace_id: kept.workspace_id,
        owner: kept.owner,
        peer: kept.peer,
        collaboration: kept.collaboration,
        filed_receipts: kept.filed_receipts,
    })
    .map_err(|error| error.to_string())
}

/// The record as it is written: the join receipts not in the file beside it
/// yet, and how many that holds.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredResource {
    pub(super) version: u32,
    pub(super) workspace: String,
    pub(super) workspace_id: WorkspaceId,
    pub(super) owner: DeviceCert,
    pub(super) peer: Vec<super::peer_parts::PeerPart>,
    /// The log, its join receipts those after the receipts file's.
    pub(super) collaboration: JoinLog,
    /// How many join receipts the receipts file holds, the first of the
    /// log's.
    pub(super) filed_receipts: u64,
}

impl WorkspaceDocumentEntry {
    /// The record `stored` names: `receipted`, the receipts file's, before
    /// the receipts its log holds.
    pub(super) fn read(stored: StoredResource, mut receipted: Vec<JoinCompletionRecord>) -> Self {
        let mut collaboration = stored.collaboration;
        receipted.append(&mut collaboration.join_completions);
        collaboration.join_completions = receipted;
        Self {
            version: DOCUMENT_VERSION,
            workspace: stored.workspace,
            workspace_id: stored.workspace_id,
            owner: stored.owner,
            peer: stored.peer,
            collaboration,
        }
    }
}

/// [`StoredResource`] borrowed from the record held, as it is written.
#[derive(Serialize)]
pub(super) struct StoredDocumentRef<'a> {
    pub(super) version: u32,
    pub(super) workspace: &'a str,
    pub(super) workspace_id: WorkspaceId,
    pub(super) owner: &'a DeviceCert,
    pub(super) peer: &'a [super::peer_parts::PeerPart],
    pub(super) collaboration: LogRef<'a>,
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

/// Why the workspace kept at `path` under the state directory `dir` does not open: its own words,
/// which say what to do with the directory when the state is of a format this build does not read,
/// and the directory they mean.
pub(super) fn unopened(path: &std::path::Path, dir: &std::path::Path, error: &str) -> String {
    format!(
        "{}: record: {error} (the state directory is {})",
        path.display(),
        dir.display()
    )
}

#[cfg(test)]
mod tests;
