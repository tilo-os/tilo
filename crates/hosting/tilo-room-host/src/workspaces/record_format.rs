//! A governed workspace record: one postcard struct whose field order is the
//! format, holding what the files beside it do not.

use serde::{Deserialize, Serialize};
use tilo_files::native::migration::Format;
use tilo_peer_state::join_ledger::{
    AdministrationRecord, InvitationRecord, JoinChallengeRecord, JoinCompletionRecord, JoinLedger,
    MembershipChallengeRecord,
};
use tilo_relay_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::{Publication, RECORD_VERSION};

/// The record's one layout.
pub(super) const FORMAT: Format = Format {
    name: "governed workspace record",
    version: RECORD_VERSION,
    oldest: RECORD_VERSION,
    steps: &[],
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

