//! Golden wire bytes for the signed file invite: a change of them is a change of
//! the wire, made on purpose or not at all.

use collab_types::canonical::from_canonical_bytes;
use collab_types::{Ed25519PublicKey, canonical_bytes};
use fs_protocol::{FileDocumentId, FileInviteSubject, FileInviteV1, FilePermission};
use uuid::Uuid;

const SIGNED_FILE_INVITE: &str = "10239e704421dc400e996582460f84f7821000000000000000000000000000000055100000000000000000000000000000006600080808080808080808080808080808080808080808080808080808080808080800000102ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c400ebcb47465cda5f984f6fe5f53e1682991c7af8a3e4259192aff6b2bdfaadabf7435cf2991ec1fd40f50882a1d55eb09f8aeda2c2aca5948927e7c5663cea006";

fn record(n: u128) -> FileDocumentId {
    FileDocumentId(Uuid::from_u128(n))
}

/// A minted invite carries a random id, so the golden is one minted token: it
/// must still decode, verify under the same signature domain and re-encode to
/// the identical bytes.
#[test]
fn signed_file_invite_keeps_its_wire_bytes_and_signature() {
    let bytes = hex::decode(SIGNED_FILE_INVITE).unwrap();
    let invite: FileInviteV1 = from_canonical_bytes(&bytes).unwrap();
    invite.verify_at(0).unwrap();
    assert_eq!(invite.document_id, record(0x66));
    assert_eq!(invite.permission, FilePermission::View);
    assert_eq!(
        invite.subject,
        FileInviteSubject::Person(Ed25519PublicKey([8; 32]))
    );
    assert_eq!(canonical_bytes(&invite).unwrap(), bytes);
}
