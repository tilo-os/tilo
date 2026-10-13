//! Golden wire bytes for the filesystem control protocols, the signed file
//! invite and the signed file-mutation transaction. The hex constants were
//! captured from `collab-types` before these types moved to `fs-protocol`;
//! the move must not change a single byte of any of them.

use collab_types::canonical::from_canonical_bytes;
use collab_types::{Ed25519PublicKey, canonical_bytes};
use ed25519_dalek::SigningKey;
use fs_protocol::{
    CONTENT_REVISION_VERSION, ContentHash, ContentRevisionV1, FILE_BINDING_VERSION,
    FILE_MUTATION_TRANSACTION_VERSION, FileActor, FileBindingV1, FileInviteSubject, FileInviteV1,
    FileMutationOperation, FileMutationParticipant, FileMutationParticipantRole,
    FileMutationTransaction, FileMutationTransactionManifest, FileNodeKind, FilePermission,
    FileProvenance, FileRecordId, FileVersionId, FileVersionV1, RECIPIENT_CONTROL_VERSION,
    RecipientControlOperationV1, RecipientControlRequestV1, RecipientSubscriptionKeyV1,
    SHARE_CONTROL_VERSION, ShareControlOperationV1, ShareControlRequestV1,
};
use uuid::Uuid;

const RECIPIENT_ADMIT: &str = "01001e74696c6f2d73686172653a2f2f76322f6578616d706c6523736563726574";
const RECIPIENT_STATUS: &str =
    "010110000000000000000000000000000000111000000000000000000000000000000022";
const SHARE_CREATE_NAMED: &str = "0200100000000000000000000000000000003304040404040404040404040404040404040404040404040404040404040404040205050505050505050505050505050505050505050505050505050505050505050606060606060606060606060606060606060606060606060606060606060606010180d095ffbc31";
const SHARE_CREATE_BEARER: &str = "020110000000000000000000000000000000440080a0f1c2b1340103";
const SIGNED_FILE_INVITE: &str = "10239e704421dc400e996582460f84f7821000000000000000000000000000000055100000000000000000000000000000006600080808080808080808080808080808080808080808080808080808080808080800000102ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c400ebcb47465cda5f984f6fe5f53e1682991c7af8a3e4259192aff6b2bdfaadabf7435cf2991ec1fd40f50882a1d55eb09f8aeda2c2aca5948927e7c5663cea006";
const SIGNED_FILE_MUTATION_TRANSACTION: &str = "01100000000000000000000000000000007710000000000000000000000000000000880909090909090909090909090909090909090909090909090909090909090909197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d612c0110000000000000000000000000000001010a736f757263652e74787410000000000000000000000000000001020f64657374696e6174696f6e2e747874100000000000000000000000000000020110000000000000000000000000000002020210000000000000000000000000000002010010000000000000000000000000000004010110000000000000000000000000000002010110000000000000000000000000000001020f64657374696e6174696f6e2e74787400000909090909090909090909090909090909090909090909090909090909090909197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d612c100000000000000000000000000000030110000000000000000000000000000002010110000000000000000000000000000004010909090909090909090909090909090909090909090909090909090909090909197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d612c00020202020202020202020202020202020202020202020202020202020202020202010110000000000000000000000000000002011000000000000000000000000000000301000002020202020202020202020202020202020202020202020202020202020202026372e7c0a384d60423a44e8d984aee843f9eabb8ce17a08999f45a4021a8611510000000000000000000000000000002020110000000000000000000000000000004020110000000000000000000000000000002020110000000000000000000000000000001020f64657374696e6174696f6e2e74787400010909090909090909090909090909090909090909090909090909090909090909197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d612c100000000000000000000000000000030210000000000000000000000000000002020110000000000000000000000000000004020909090909090909090909090909090909090909090909090909090909090909197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d612c00020202020202020202020202020202020202020202020202020202020202020204010110000000000000000000000000000002021000000000000000000000000000000302000002020202020202020202020202020202020202020202020202020202020202025c9e7e00e83e89e8ef5f7d7a6d4d0c98dc5c3e2055eac5d56fcb8348a1376a44401b08cbada9efeef494077db1bafbf511fb483616bb433cc24af7fea00ff7dd195fff0db6fee026f36e77a9fa7fd0ec03e7ab758eb56b525c4ccb0cf6a17cfd0a";

fn record(n: u128) -> FileRecordId {
    FileRecordId(Uuid::from_u128(n))
}

fn version(n: u128) -> FileVersionId {
    FileVersionId(Uuid::from_u128(n))
}

fn assert_golden(name: &str, bytes: &[u8], golden: &str) {
    assert_eq!(hex::encode(bytes), golden, "{name} wire bytes changed");
}

#[test]
fn recipient_control_requests_keep_their_wire_bytes() {
    let admit = RecipientControlRequestV1 {
        protocol_version: RECIPIENT_CONTROL_VERSION,
        operation: RecipientControlOperationV1::AdmitV2 {
            uri: "tilo-share://v2/example#secret".into(),
        },
    };
    assert_golden(
        "RECIPIENT_ADMIT",
        &canonical_bytes(&admit).unwrap(),
        RECIPIENT_ADMIT,
    );
    let status = RecipientControlRequestV1 {
        protocol_version: RECIPIENT_CONTROL_VERSION,
        operation: RecipientControlOperationV1::Status {
            key: RecipientSubscriptionKeyV1 {
                workspace_id: Uuid::from_u128(0x11),
                record_id: record(0x22),
            },
        },
    };
    assert_golden(
        "RECIPIENT_STATUS",
        &canonical_bytes(&status).unwrap(),
        RECIPIENT_STATUS,
    );
}

#[test]
fn share_control_requests_keep_their_wire_bytes() {
    let named = ShareControlRequestV1 {
        protocol_version: SHARE_CONTROL_VERSION,
        operation: ShareControlOperationV1::CreateNamed {
            record_id: record(0x33),
            person: Ed25519PublicKey([4; 32]),
            recipient_devices: vec![Ed25519PublicKey([5; 32]), Ed25519PublicKey([6; 32])],
            permission: FilePermission::Edit,
            expires_at_ms: Some(1_700_000_000_000),
        },
    };
    assert_golden(
        "SHARE_CREATE_NAMED",
        &canonical_bytes(&named).unwrap(),
        SHARE_CREATE_NAMED,
    );
    let bearer = ShareControlRequestV1 {
        protocol_version: SHARE_CONTROL_VERSION,
        operation: ShareControlOperationV1::CreateBearerV2 {
            record_id: record(0x44),
            permission: FilePermission::View,
            expires_at_ms: 1_800_000_000_000,
            max_uses: Some(3),
        },
    };
    assert_golden(
        "SHARE_CREATE_BEARER",
        &canonical_bytes(&bearer).unwrap(),
        SHARE_CREATE_BEARER,
    );
}

/// A minted invite carries a random id, so the golden is one minted token: it
/// must still decode, verify under the same signature domain and re-encode to
/// the identical bytes.
#[test]
fn signed_file_invite_keeps_its_wire_bytes_and_signature() {
    let bytes = hex::decode(SIGNED_FILE_INVITE).unwrap();
    let invite: FileInviteV1 = from_canonical_bytes(&bytes).unwrap();
    invite.verify_at(0).unwrap();
    assert_eq!(invite.record_id, record(0x66));
    assert_eq!(invite.permission, FilePermission::View);
    assert_eq!(
        invite.subject,
        FileInviteSubject::Person(Ed25519PublicKey([8; 32]))
    );
    assert_eq!(canonical_bytes(&invite).unwrap(), bytes);
}

#[test]
fn signed_file_mutation_transaction_keeps_its_wire_bytes() {
    let key = SigningKey::from_bytes(&[42; 32]);
    let actor = FileActor {
        person: Ed25519PublicKey([9; 32]),
        device: Ed25519PublicKey(key.verifying_key().to_bytes()),
    };
    let source_parent = record(0x101);
    let destination_parent = record(0x102);
    let participants = [
        (record(0x201), FileMutationParticipantRole::Source),
        (record(0x202), FileMutationParticipantRole::Replaced),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (record_id, role))| {
        let offset = u128::try_from(index).unwrap();
        let proposed_version = FileVersionV1 {
            id: version(0x301 + offset),
            record_id,
            parents: vec![version(0x401 + offset)],
            actor,
            timestamp_ms: 44,
            content_hash: None,
            metadata_hash: ContentHash([2; 32]),
            provenance: if role == FileMutationParticipantRole::Source {
                FileProvenance::Rename
            } else {
                FileProvenance::Delete
            },
        };
        let mut participant = FileMutationParticipant {
            record_id,
            role,
            expected_version: version(0x401 + offset),
            proposed_binding: FileBindingV1 {
                schema_version: FILE_BINDING_VERSION,
                record_id,
                parent: Some(destination_parent),
                name: "destination.txt".into(),
                kind: FileNodeKind::File,
                tombstoned: role == FileMutationParticipantRole::Replaced,
                actor,
                observed_at_ms: 44,
            },
            proposed_content_revision: Some(ContentRevisionV1 {
                schema_version: CONTENT_REVISION_VERSION,
                record_id,
                version_id: proposed_version.id,
                content_hash: proposed_version.content_hash,
                byte_len: 0,
                metadata_hash: proposed_version.metadata_hash,
            }),
            proposed_version,
            projection_hash: ContentHash([0; 32]),
        };
        participant.projection_hash = participant.computed_projection_hash().unwrap();
        participant
    })
    .collect();
    let manifest = FileMutationTransactionManifest {
        schema_version: FILE_MUTATION_TRANSACTION_VERSION,
        transaction_id: Uuid::from_u128(0x77),
        workspace_id: Uuid::from_u128(0x88),
        actor,
        timestamp_ms: 44,
        operation: FileMutationOperation::ReplacementRename {
            source_parent,
            source_name: "source.txt".into(),
            destination_parent,
            destination_name: "destination.txt".into(),
            source_record: record(0x201),
            replaced_record: record(0x202),
        },
        participants,
    };
    let signed = FileMutationTransaction::signed(manifest, &key).unwrap();
    signed.verify().unwrap();
    assert_golden(
        "SIGNED_FILE_MUTATION_TRANSACTION",
        &signed.canonical_bytes().unwrap(),
        SIGNED_FILE_MUTATION_TRANSACTION,
    );
}
