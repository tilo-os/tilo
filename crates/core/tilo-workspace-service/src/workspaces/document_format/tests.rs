use super::*;
use tilo_files::native::migration::assert_golden_bytes;
use tilo_user_identity::DeviceCapabilities;

const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";

/// The record this build writes: a workspace its host keeps, one
/// administration record not filed beside it yet. Kept so a workspace's file
/// goes on reading the same way; a field added, removed or reordered changes
/// these bytes.
const GOLDEN: &str = include_str!("../../../golden/hosted-workspace-document/v6.hex");

/// The same record as version 5 wrote it, with one change its host published
/// in its own name.
const VERSION_5: &str = include_str!("../../../golden/hosted-workspace-document/v5.hex");

fn golden() -> Vec<u8> {
    hex::decode(GOLDEN.split_whitespace().collect::<String>()).unwrap()
}

fn kept() -> StoredResource {
    StoredResource {
        version: DOCUMENT_VERSION,
        workspace: WORKSPACE.into(),
        workspace_id: WorkspaceId::parse(WORKSPACE).unwrap(),
        owner: DeviceCert {
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
        collaboration: JoinLog {
            administration: vec![AdministrationRecord {
                request_id: [7; 32],
                request_digest: [8; 32],
                generation: 9,
            }],
            ..JoinLog::default()
        },
        filed_receipts: 0,
    }
}

/// `record` as the workspace file's writer encodes it.
fn written(record: &StoredResource) -> Vec<u8> {
    postcard::to_stdvec(&StoredDocumentRef {
        version: record.version,
        workspace: &record.workspace,
        workspace_id: record.workspace_id,
        owner: &record.owner,
        peer: &record.peer,
        collaboration: LogRef {
            invitations: &record.collaboration.invitations,
            join_challenges: &record.collaboration.join_challenges,
            join_completions: &record.collaboration.join_completions,
            membership_challenges: &record.collaboration.membership_challenges,
            administration: &record.collaboration.administration,
        },
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
            let read: StoredResource = postcard::from_bytes(bytes).unwrap();
            assert_eq!(read.version, FORMAT.version);
            assert_eq!(read.workspace_id, kept.workspace_id);
            assert_eq!(read.owner.device_public_key, [2; 32]);
            assert_eq!(read.peer.len(), 1);
            assert_eq!(read.collaboration.administration[0].generation, 9);
        },
        || written(&kept),
    );
}

/// A record version 5 kept, which held what its host published in its own
/// name, reads as this build writes it, with that left behind.
#[test]
fn a_record_that_kept_what_its_host_sent_reads_without_it() {
    let kept = hex::decode(VERSION_5.split_whitespace().collect::<String>()).unwrap();
    assert_eq!(FORMAT.migrate(kept, 5).unwrap(), golden());
}

/// A record of another version is refused, whichever build wrote it.
#[test]
fn a_record_of_another_version_is_refused() {
    assert!(FORMAT.migrate(golden(), 4).is_err());
    assert!(FORMAT.migrate(golden(), 7).is_err());
}
