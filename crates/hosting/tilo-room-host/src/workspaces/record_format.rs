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

/// The record's one layout. The oldest version is written out rather than
/// named by [`RECORD_VERSION`], which rises with every layout: that the build
/// still brings up a version is a promise about the files machines hold, and
/// it only ever falls, so it may not move with the number this build writes.
pub(super) const FORMAT: Format = Format {
    name: "governed workspace record",
    version: RECORD_VERSION,
    oldest: 4,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tilo_files::native::migration::assert_golden_bytes;
    use tilo_peer_state::join_ledger::AdministrationRecord;
    use tilo_user_identity::DeviceCapabilities;

    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

    /// The record this build writes: a workspace its host governs, one
    /// publication and one administration record not filed beside it yet.
    /// Kept so a workspace's file goes on reading the same way; a field
    /// added, removed or reordered changes these bytes.
    const GOLDEN: &str = include_str!("../../golden/governed-workspace-record/v4.hex");

    fn golden() -> Vec<u8> {
        hex::decode(GOLDEN.split_whitespace().collect::<String>()).unwrap()
    }

    fn kept() -> StoredRecord {
        StoredRecord {
            version: RECORD_VERSION,
            workspace: WORKSPACE.into(),
            workspace_id: WorkspaceId::parse(WORKSPACE).unwrap(),
            name: "Lounge".into(),
            authority: DeviceCert {
                user_public_key: [1; 32],
                device_public_key: [2; 32],
                device_label: "host".into(),
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 3,
                expires_at_ns: None,
                signature: [4; 64],
                issuer: None,
            },
            peer: vec![super::super::peer_parts::PeerPart::Inline(vec![1, 2, 3])],
            generation: 5,
            outgoing: vec![Publication {
                bytes: Arc::from([6, 7, 8]),
                checkpoint: false,
            }],
            published: 0,
            collaboration: JoinLedger {
                administration: vec![AdministrationRecord {
                    request_id: [7; 32],
                    request_digest: [8; 32],
                    generation: 9,
                }],
                ..JoinLedger::default()
            },
            filed_publications: 0,
            filed_receipts: 0,
        }
    }

    /// `record` as the workspace file's writer encodes it.
    fn written(record: &StoredRecord) -> Vec<u8> {
        postcard::to_stdvec(&StoredRecordRef {
            version: record.version,
            workspace: &record.workspace,
            workspace_id: record.workspace_id,
            name: &record.name,
            authority: &record.authority,
            peer: &record.peer,
            generation: record.generation,
            outgoing: &record.outgoing,
            published: record.published,
            collaboration: LedgerRef {
                invitations: &record.collaboration.invitations,
                join_challenges: &record.collaboration.join_challenges,
                join_completions: &record.collaboration.join_completions,
                membership_challenges: &record.collaboration.membership_challenges,
                administration: &record.collaboration.administration,
            },
            filed_publications: record.filed_publications,
            filed_receipts: record.filed_receipts,
        })
        .unwrap()
    }

    #[test]
    fn the_record_kept_here_reads_and_writes_as_it_is() {
        let kept = kept();
        assert_golden_bytes(
            FORMAT.name,
            &golden(),
            |bytes| {
                let read: StoredRecord = postcard::from_bytes(bytes).unwrap();
                assert_eq!(read.version, FORMAT.version);
                assert_eq!(read.workspace_id, kept.workspace_id);
                assert_eq!(read.name, "Lounge");
                assert_eq!(read.authority.device_public_key, [2; 32]);
                assert_eq!(read.peer.len(), 1);
                assert_eq!(read.outgoing[0].bytes.as_ref(), [6, 7, 8]);
                assert_eq!(read.collaboration.administration[0].generation, 9);
            },
            || written(&kept),
        );
    }
}
