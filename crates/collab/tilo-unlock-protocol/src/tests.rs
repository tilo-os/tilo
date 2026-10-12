use ed25519_dalek::SigningKey;
use tilo_seal::SecretKey;

use super::*;

fn root() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

fn boot_key() -> SigningKey {
    SigningKey::from_bytes(&[2; 32])
}

fn slot() -> SlotId {
    SlotId {
        volume_uuid: [7; 16],
        keyslot: 3,
    }
}

fn cert() -> BootKeyCert {
    let mut cert = BootKeyCert {
        version: PROTOCOL_VERSION,
        user_public_key: root().verifying_key().to_bytes(),
        device_public_key: [4; 32],
        boot_public_key: boot_key().verifying_key().to_bytes(),
        slot: slot(),
        attestation_key: None,
        issued_at_ns: 10,
        signature: [0; 64],
    };
    cert.sign(&root()).unwrap();
    cert
}

fn challenge() -> Challenge {
    Challenge {
        version: PROTOCOL_VERSION,
        sibling_public_key: [9; 32],
        nonce: [11; 32],
        expires_at_ns: 1_000,
    }
}

#[test]
fn certificates_are_root_signed_and_tamper_evident() {
    let cert = cert();
    assert_eq!(cert.verify(), Ok(()));
    let mut other = cert.clone();
    other.slot.keyslot = 4;
    assert_eq!(other.verify(), Err(BootKeyCertError::BadSignature));
    let mut wrong = cert;
    assert_eq!(
        wrong.sign(&SigningKey::from_bytes(&[3; 32])),
        Err(BootKeyCertError::WrongSigner)
    );
}

#[test]
fn enrol_request_proves_the_boot_key() {
    let request = EnrolRequest::new(&boot_key(), slot(), None, Half::from_bytes([5; 32])).unwrap();
    assert_eq!(request.verify(), Ok(()));
    let mut swapped = request.clone();
    swapped.boot_public_key = SigningKey::from_bytes(&[6; 32]).verifying_key().to_bytes();
    assert_eq!(swapped.verify(), Err(BootKeyCertError::BadSignature));
    let mut other_half = request;
    other_half.sibling_half = Half::from_bytes([6; 32]);
    assert_eq!(other_half.verify(), Err(BootKeyCertError::BadSignature));
}

#[test]
fn a_full_exchange_releases_the_half_to_that_request_only() {
    let cert = cert();
    let secret_sync = SecretKey::from_bytes([12; 32]);
    let half = Half::from_bytes([5; 32]);
    let sealed = seal_sibling_half(&secret_sync.public_key().unwrap(), &cert, &half).unwrap();

    // A half sealed for one certificate opens for no other.
    let mut other = cert.clone();
    other.slot.keyslot = 9;
    other.sign(&root()).unwrap();
    assert!(open_sibling_half(&secret_sync, &other, &sealed).is_err());
    let opened = open_sibling_half(&secret_sync, &cert, &sealed).unwrap();
    assert_eq!(opened, half);

    let ephemeral = SecretKey::from_bytes([13; 32]);
    let request = UnlockRequest::sign(
        &boot_key(),
        &cert,
        &challenge(),
        BootId([14; 16]),
        &ephemeral.public_key().unwrap(),
        None,
    )
    .unwrap();
    let frame = encode_frame(&ClientFrame::Request(request.clone())).unwrap();
    let len = frame_len(frame[..4].try_into().unwrap()).unwrap();
    let decoded: ClientFrame = decode_frame(&frame[4..4 + len]).unwrap();
    assert_eq!(decoded, ClientFrame::Request(request.clone()));

    let digest = request.verify(&cert, &challenge(), 500).unwrap();
    let release = seal_release(&request.ephemeral_public, &digest, &opened).unwrap();
    assert_eq!(open_release(&ephemeral, &digest, &release).unwrap(), half);
    // A recorded release opens nothing without that request's ephemeral key.
    assert!(open_release(&SecretKey::from_bytes([15; 32]), &digest, &release).is_err());
    assert!(open_release(&ephemeral, &[0; 32], &release).is_err());
    assert_eq!(approval_code(&digest).len(), 6);
}

#[test]
fn requests_are_bound_to_challenge_slot_and_boot_key() {
    let cert = cert();
    let ephemeral = SecretKey::from_bytes([13; 32]).public_key().unwrap();
    let request = UnlockRequest::sign(
        &boot_key(),
        &cert,
        &challenge(),
        BootId([14; 16]),
        &ephemeral,
        None,
    )
    .unwrap();
    assert_eq!(
        request.verify(&cert, &challenge(), 1_000),
        Err(RefusalReason::ChallengeExpired)
    );
    let mut fresh = challenge();
    fresh.nonce = [1; 32];
    assert_eq!(
        request.verify(&cert, &fresh, 500),
        Err(RefusalReason::UnknownChallenge)
    );
    let mut other_sibling = challenge();
    other_sibling.sibling_public_key = [1; 32];
    assert_eq!(
        request.verify(&cert, &other_sibling, 500),
        Err(RefusalReason::BadSignature)
    );
    let mut wrong_slot = request.clone();
    wrong_slot.slot.keyslot = 1;
    assert_eq!(
        wrong_slot.verify(&cert, &challenge(), 500),
        Err(RefusalReason::WrongSlot)
    );
    let mut other_boot = request;
    other_boot.boot_id = BootId([1; 16]);
    assert_eq!(
        other_boot.verify(&cert, &challenge(), 500),
        Err(RefusalReason::BadSignature)
    );
    let forged = UnlockRequest::sign(
        &SigningKey::from_bytes(&[3; 32]),
        &cert,
        &challenge(),
        BootId([14; 16]),
        &ephemeral,
        None,
    )
    .unwrap();
    assert_eq!(
        forged.verify(&cert, &challenge(), 500),
        Err(RefusalReason::BadSignature)
    );
}

#[test]
fn slot_keys_need_both_halves() {
    let device = Half::from_bytes([1; 32]);
    let sibling = Half::from_bytes([2; 32]);
    let key = derive_slot_key(&device, &sibling, &[3; 16]);
    assert_eq!(key.len(), 64);
    assert_ne!(
        key,
        derive_slot_key(&device, &Half::from_bytes([4; 32]), &[3; 16])
    );
    assert_ne!(key, derive_slot_key(&device, &sibling, &[4; 16]));
    assert_eq!(format!("{device:?}"), "Half(..)");
}

#[test]
fn frames_are_bounded() {
    assert_eq!(
        frame_len(u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes()),
        Err(FrameError::TooLarge)
    );
    let frame = encode_frame(&SiblingFrame::Denied).unwrap();
    let mut body = frame[4..].to_vec();
    body.push(0);
    assert_eq!(
        decode_frame::<SiblingFrame>(&body),
        Err(FrameError::Malformed)
    );
}
