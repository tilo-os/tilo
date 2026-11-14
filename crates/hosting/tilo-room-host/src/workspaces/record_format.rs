//! A governed workspace record: one postcard struct whose field order is the
//! format, holding what the files beside it do not.
//!
//! Version 4 is the record this build writes ([`StoredRecord`]): the
//! publications and join receipts not in the files beside it yet
//! ([`super::appended`]), and how many each file holds, its workspace named
//! by its id as a UUID ([`WorkspaceId`]) throughout.
//!
//! Version 3 is the record the 0.1 releases wrote (the live relay's v0.1.5
//! among them). It held every publication and join receipt inline, and named
//! its workspace twice: by the UUID text, and by a 64-bit number derived from
//! it (`room_workspace_id`), which every grant, challenge and bootstrap in its
//! join ledger carried too, beside the UUID text again. [`v3_to_v4`] reads such
//! a record field by field in that layout ([`v3`]) and writes it as version 4:
//! every publication and receipt in the record and none in the files, each
//! ledger entry naming the workspace by the UUID it carried as text. A join or
//! membership challenge still waiting for its proof is dropped: its version
//! now names this build's layout, so no proof of it would verify, and the
//! device asks for a new one. A
//! receipt's bootstrap held one digest entry for the governance it delivered;
//! this build replays a receipt while the workspace's generation is the one
//! it was delivered at, and keeps no governance in it. Nothing else changed.
//! `Format::bring_up` keeps the file as it was beside it as `<record>.v3`, and
//! the peer's large parts, which the record names by digest, are untouched.
//!
//! The version 4 layout naming the workspace by its number was never released
//! and is not read. Version 2, before the 0.1 releases' grants stopped naming
//! the workspace's authority, is not read either: a release that writes
//! version 3 opens every record it governs when it starts, and brought each
//! up to version 3 then.

use serde::{Deserialize, Serialize};
use tilo_files::native::migration::Format;
use tilo_peer_state::join_ledger::{
    AdministrationRecord, InvitationRecord, JoinChallengeRecord, JoinCompletionRecord, JoinLedger,
    MembershipChallengeRecord,
};
use tilo_relay_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::{Publication, RECORD_VERSION};

mod v3;

/// The record's versions: 3 (the 0.1 releases') and 4 (this build's). The
/// oldest version is written out rather than named by [`RECORD_VERSION`],
/// which rises with every layout: that the build still brings up a version is
/// a promise about the files machines hold, and it only ever falls, so it may
/// not move with the number this build writes.
pub(super) const FORMAT: Format = Format {
    name: "governed workspace record",
    version: RECORD_VERSION,
    oldest: 3,
    steps: &[v3_to_v4],
};

/// The record as it is written: the publications and join receipts not in the
/// files beside it yet, and how many those hold.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct StoredRecord {
    pub(super) version: u32,
    pub(super) workspace: String,
    pub(super) workspace_id: WorkspaceId,
    pub(super) name: String,
    pub(super) authority: DeviceCert,
    pub(super) peer: Vec<super::peer_parts::PeerPart>,
    pub(super) generation: u64,
    /// The publications after the ones the publications file holds.
    pub(super) outgoing: Vec<Publication>,
    pub(super) published: usize,
    /// The ledger, its join receipts those after the receipts file's.
    pub(super) collaboration: JoinLedger,
    /// How many publications the publications file holds, the first of the
    /// workspace's.
    pub(super) filed_publications: u64,
    /// How many join receipts the receipts file holds, the first of the
    /// ledger's.
    pub(super) filed_receipts: u64,
}

/// [`StoredRecord`] borrowed from the record held, as it is written.
#[derive(Serialize)]
pub(super) struct StoredRecordRef<'a> {
    pub(super) version: u32,
    pub(super) workspace: &'a str,
    pub(super) workspace_id: WorkspaceId,
    pub(super) name: &'a str,
    pub(super) authority: &'a DeviceCert,
    pub(super) peer: &'a [super::peer_parts::PeerPart],
    pub(super) generation: u64,
    pub(super) outgoing: &'a [Publication],
    pub(super) published: usize,
    pub(super) collaboration: LedgerRef<'a>,
    pub(super) filed_publications: u64,
    pub(super) filed_receipts: u64,
}

/// The join ledger as a written record holds it, borrowed: its receipts
/// those the receipts file does not hold. The ledger's own layout.
#[derive(Serialize)]
pub(super) struct LedgerRef<'a> {
    pub(super) invitations: &'a [InvitationRecord],
    pub(super) join_challenges: &'a [JoinChallengeRecord],
    pub(super) join_completions: &'a [JoinCompletionRecord],
    pub(super) membership_challenges: &'a [MembershipChallengeRecord],
    pub(super) administration: &'a [AdministrationRecord],
}

/// Bring a version 3 record, as the 0.1 releases wrote it, up to version 4:
/// every publication and receipt in the record, none in the files beside it,
/// the workspace named by its UUID throughout.
///
/// # Errors
///
/// The bytes are not a version 3 record, or it names no UUID.
#[expect(
    clippy::needless_pass_by_value,
    reason = "a migration step takes its bytes by value"
)]
fn v3_to_v4(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let record: v3::Record =
        postcard::from_bytes(&bytes).map_err(|error| format!("version 3 record: {error}"))?;
    let record = record.current()?;
    postcard::to_stdvec(&StoredRecord {
        version: RECORD_VERSION,
        workspace: record.workspace,
        workspace_id: record.workspace_id,
        name: record.name,
        authority: record.authority,
        peer: record.peer,
        generation: record.generation,
        outgoing: record.outgoing,
        published: record.published,
        collaboration: record.collaboration,
        filed_publications: 0,
        filed_receipts: 0,
    })
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests;
