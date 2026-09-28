use super::*;
use crate::delivery_v3::commit::CommitRef;
use ed25519_dalek::SigningKey;

fn issue() -> CursorIssue {
    CursorIssue {
        room: "018f0f9e-11aa-7c42-8000-000000000001".into(),
        workspace: 7,
        workspace_root: [1; 32],
        host_device: [2; 32],
        host_root: [3; 32],
        consumer_device: [4; 32],
        consumer_root: [5; 32],
        consumer: [6; 16],
        store_generation: [7; 16],
        authorization_view: [8; 32],
        committed: CommitRef {
            sequence: 9_007_199_254_740_993,
            digest: [9; 32],
        },
    }
}

#[test]
fn cursor_is_canonical_signed_and_opaque() {
    let key = SigningKey::from_bytes(&[42; 32]);
    let token = CursorToken::issue(issue(), &key).unwrap();
    assert!(token.as_bytes().len() <= MAX_CURSOR_BYTES);
    let signed = token.authenticate(&key.verifying_key()).unwrap();
    assert_eq!(signed.committed().sequence, 9_007_199_254_740_993);
    assert_eq!(signed.consumer(), [6; 16]);
    assert_eq!(CursorToken::from_bytes(token.as_bytes()).unwrap(), token);
}

#[test]
fn cursor_rejects_tampering_trailing_and_wrong_key() {
    let key = SigningKey::from_bytes(&[42; 32]);
    let token = CursorToken::issue(issue(), &key).unwrap();
    let mut changed = token.as_bytes().to_vec();
    changed[10] ^= 1;
    assert!(
        CursorToken::from_bytes(&changed)
            .unwrap()
            .authenticate(&key.verifying_key())
            .is_err()
    );
    let mut trailing = token.as_bytes().to_vec();
    trailing.push(0);
    assert!(
        CursorToken::from_bytes(&trailing)
            .unwrap()
            .authenticate(&key.verifying_key())
            .is_err()
    );
    assert!(
        token
            .authenticate(&SigningKey::from_bytes(&[43; 32]).verifying_key())
            .is_err()
    );
}

#[test]
fn cursor_literal_fixture_exceeds_js_safe_integer() {
    let key = SigningKey::from_bytes(&[42; 32]);
    let token = CursorToken::issue(issue(), &key).unwrap();
    let claims = canonical_claims_for_test(issue()).unwrap();
    let digest = cursor_digest_for_test(&claims);
    let signature = key.sign(&digest).to_bytes();
    assert_eq!(
        hex::encode(&claims),
        "01032430313866306639652d313161612d376334322d383030302d30303030303030303030303107010101010101010101010101010101010101010101010101010101010101010102020202020202020202020202020202020202020202020202020202020202020303030303030303030303030303030303030303030303030303030303030303040404040404040404040404040404040404040404040404040404040404040405050505050505050505050505050505050505050505050505050505050505050606060606060606060606060606060607070707070707070707070707070707080808080808080808080808080808080808080808080808080808080808080881808080808080100909090909090909090909090909090909090909090909090909090909090909"
    );
    assert_eq!(
        hex::encode(digest),
        "a34267be65fee4b7478ea5b92f2efb22c80009aa0587896d0fc2f300649c601b"
    );
    assert_eq!(
        hex::encode(signature),
        "cea243ab5fff8b5fc9238a7e071cb9785027a55b9d25bbfedec754e3d38b5ddfe6e0607185f1d24011dd344707624dd6878fd5a376b41580e72ea7ed502a6b06"
    );
    assert_eq!(token.as_bytes().len(), claims.len() + 64);
    assert_eq!(
        hex::encode(token.as_bytes()),
        "01032430313866306639652d313161612d376334322d383030302d30303030303030303030303107010101010101010101010101010101010101010101010101010101010101010102020202020202020202020202020202020202020202020202020202020202020303030303030303030303030303030303030303030303030303030303030303040404040404040404040404040404040404040404040404040404040404040405050505050505050505050505050505050505050505050505050505050505050606060606060606060606060606060607070707070707070707070707070707080808080808080808080808080808080808080808080808080808080808080881808080808080100909090909090909090909090909090909090909090909090909090909090909cea243ab5fff8b5fc9238a7e071cb9785027a55b9d25bbfedec754e3d38b5ddfe6e0607185f1d24011dd344707624dd6878fd5a376b41580e72ea7ed502a6b06"
    );
}

#[test]
fn alternate_domain_signature_does_not_authenticate() {
    let key = SigningKey::from_bytes(&[42; 32]);
    let claims = canonical_claims_for_test(issue()).unwrap();
    let mut preimage = b"tilo-delivery-v3-cursor-alternate-v1\0".to_vec();
    preimage.extend_from_slice(&claims);
    let signature = key.sign(blake3::hash(&preimage).as_bytes()).to_bytes();
    let mut token = claims;
    token.extend_from_slice(&signature);
    assert!(
        CursorToken::from_bytes(&token)
            .unwrap()
            .authenticate(&key.verifying_key())
            .is_err()
    );
}

#[test]
fn overlong_room_rejects_before_owned_copy() {
    reset_room_owned_copies();
    let wire = Claims {
        version: 1,
        protocol: crate::delivery_v3::framing::VERSION,
        issue: CursorIssue {
            room: "x".repeat(129),
            ..issue()
        },
    };
    let mut token = postcard::to_stdvec(&wire).unwrap();
    token.extend_from_slice(&[0; 64]);
    assert!(
        CursorToken::from_bytes(&token)
            .unwrap()
            .authenticate(&SigningKey::from_bytes(&[42; 32]).verifying_key())
            .is_err()
    );
    assert_eq!(room_owned_copies(), 0);
}

#[test]
fn cursor_rejects_every_truncation_oversize_and_noncanonical_room() {
    let key = SigningKey::from_bytes(&[42; 32]);
    let token = CursorToken::issue(issue(), &key).unwrap();
    for end in 0..token.as_bytes().len() {
        let parsed = CursorToken::from_bytes(&token.as_bytes()[..end]);
        assert!(parsed.is_err() || parsed.unwrap().authenticate(&key.verifying_key()).is_err());
    }
    assert!(CursorToken::from_bytes(&vec![0; MAX_CURSOR_BYTES + 1]).is_err());
    let mut invalid = issue();
    invalid.room = "018F0F9E-11AA-7C42-8000-000000000001".into();
    assert!(CursorToken::issue(invalid, &key).is_err());
}
