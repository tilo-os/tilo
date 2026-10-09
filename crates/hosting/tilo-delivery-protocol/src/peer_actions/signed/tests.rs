use super::*;
use crate::peer_actions::ActionMessage;
use crate::transport_v2::{EnvelopeV2, decode, encode};
use uuid::Uuid;

const NOW: u64 = 1_800_000_000_000_000_000;

fn key(seed: u8) -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
}

fn invoke(source: &ed25519_dalek::SigningKey, argument: &str) -> DirectedAction {
    DirectedAction {
        room: Uuid::from_u128(1),
        request_id: Uuid::from_u128(2),
        source: source.verifying_key().to_bytes(),
        target: [9; 32],
        message: ActionMessage::InvokeBound {
            host: Uuid::from_u128(3),
            invocation: tilo_actions::InvocationId(Uuid::from_u128(4)),
            app_peer: "7".into(),
            app_id: "so.tilo.files".into(),
            record_id: String::new(),
            resource: "file".into(),
            action: "delete".into(),
            argument: Some(serde_json::json!({ "path": argument })),
            confirmation: None,
            revision: None,
        },
    }
}

fn wire(signed: &SignedDirectedAction) -> Vec<u8> {
    encode(&EnvelopeV2::SignedDirected(signed.clone())).unwrap()[4..].to_vec()
}

fn decoded(payload: &[u8]) -> SignedDirectedAction {
    let EnvelopeV2::SignedDirected(signed) = decode(payload).unwrap() else {
        panic!("signed frame expected");
    };
    signed
}

#[test]
fn valid_signature_round_trips_the_wire_and_verifies() {
    let device = key(1);
    let action = invoke(&device, "notes.txt");
    let signed = SignedDirectedAction::sign(&action, NOW, &device).unwrap();
    let received = decoded(&wire(&signed));
    assert_eq!(received, signed);
    assert_eq!(received.action(), &action);
    assert_eq!(received.issued_at_ns(), NOW);
    received.verify(&action.source).unwrap();
}

#[test]
fn signature_by_other_device_is_refused() {
    let signer = key(1);
    let claimed = key(2);
    // A device cannot sign an action naming another source...
    assert_eq!(
        SignedDirectedAction::sign(&invoke(&claimed, "a"), NOW, &signer),
        Err(ActionSignatureError::Source)
    );
    // ...and a signature by device A does not verify as device B.
    let signed = SignedDirectedAction::sign(&invoke(&signer, "a"), NOW, &signer).unwrap();
    assert_eq!(
        signed.verify(&claimed.verifying_key().to_bytes()),
        Err(ActionSignatureError::Source)
    );
    let mut forged = signed;
    forged.action.source = claimed.verifying_key().to_bytes();
    forged.encoded = postcard::to_allocvec(&forged.action).unwrap();
    let forged = decoded(&wire(&forged));
    assert_eq!(
        forged.verify(&claimed.verifying_key().to_bytes()),
        Err(ActionSignatureError::Signature)
    );
}

#[test]
fn tampered_message_is_refused() {
    let device = key(1);
    let source = device.verifying_key().to_bytes();
    let signed = SignedDirectedAction::sign(&invoke(&device, "notes.txt"), NOW, &device).unwrap();
    let original = wire(&signed);
    // Flip one byte of the signed argument in the frame as a host would see it.
    let at = original.windows(9).position(|w| w == b"notes.txt").unwrap();
    let mut tampered = original.clone();
    tampered[at] = b'm';
    let received = decoded(&tampered);
    assert_ne!(received.action(), signed.action());
    assert_eq!(
        received.verify(&source),
        Err(ActionSignatureError::Signature)
    );
    // The issue time is signed too.
    let mut retimed = signed.clone();
    retimed.issued_at_ns += 1;
    assert_eq!(
        retimed.verify(&source),
        Err(ActionSignatureError::Signature)
    );
    // A different target is a different signed statement.
    let mut redirected = signed;
    redirected.action.target = [8; 32];
    redirected.encoded = postcard::to_allocvec(&redirected.action).unwrap();
    assert_eq!(
        redirected.verify(&source),
        Err(ActionSignatureError::Signature)
    );
}

#[test]
fn stale_or_future_issued_at_is_refused() {
    let device = key(1);
    let action = invoke(&device, "a");
    let mut window = ActionReplayWindow::default();
    for issued in [
        NOW - ACTION_SIGNATURE_TOLERANCE_NS - 1,
        NOW + ACTION_SIGNATURE_TOLERANCE_NS + 1,
    ] {
        let signed = SignedDirectedAction::sign(&action, issued, &device).unwrap();
        signed.verify(&action.source).unwrap();
        assert_eq!(signed.check_fresh(NOW), Err(ActionSignatureError::Stale));
        assert_eq!(window.admit(&signed, NOW), Err(ActionSignatureError::Stale));
    }
    for issued in [
        NOW - ACTION_SIGNATURE_TOLERANCE_NS,
        NOW + ACTION_SIGNATURE_TOLERANCE_NS,
    ] {
        let signed = SignedDirectedAction::sign(&action, issued, &device).unwrap();
        window.admit(&signed, NOW).unwrap();
    }
}

#[test]
fn replayed_signed_action_is_refused_within_window() {
    let device = key(1);
    let action = invoke(&device, "a");
    let signed = SignedDirectedAction::sign(&action, NOW, &device).unwrap();
    let mut window = ActionReplayWindow::default();
    window.admit(&signed, NOW).unwrap();
    // The exact frame again, re-decoded as a host would resend it.
    assert_eq!(
        window.admit(&decoded(&wire(&signed)), NOW + 1),
        Err(ActionSignatureError::Replayed)
    );
    // The device re-signing the same action later is a new statement.
    let resent = SignedDirectedAction::sign(&action, NOW + 1, &device).unwrap();
    window.admit(&resent, NOW + 2).unwrap();
    // Once outside its window the old frame is refused as stale, not admitted.
    assert_eq!(
        window.admit(&signed, NOW + ACTION_SIGNATURE_TOLERANCE_NS + 1),
        Err(ActionSignatureError::Stale)
    );
}

#[test]
fn full_replay_window_refuses_rather_than_forgets() {
    let device = key(1);
    let mut window = ActionReplayWindow::with_capacity(8);
    let mut action = invoke(&device, "a");
    for index in 0..8 {
        action.request_id = Uuid::from_u128(index + 10);
        let signed = SignedDirectedAction::sign(&action, NOW, &device).unwrap();
        window.admit(&signed, NOW).unwrap();
    }
    action.request_id = Uuid::from_u128(1);
    let one_more = SignedDirectedAction::sign(&action, NOW, &device).unwrap();
    assert_eq!(
        window.admit(&one_more, NOW),
        Err(ActionSignatureError::ReplayCapacity)
    );
    // Expired entries make room again.
    let later = NOW + 2 * ACTION_SIGNATURE_TOLERANCE_NS + 1;
    let fresh = SignedDirectedAction::sign(&action, later, &device).unwrap();
    window.admit(&fresh, later).unwrap();
}

#[test]
fn only_effect_free_reads_may_be_unsigned() {
    let invocation = tilo_actions::InvocationId(Uuid::from_u128(4));
    assert!(!ActionMessage::CatalogueRefresh { scope: None }.requires_signature());
    assert!(!ActionMessage::Status { invocation }.requires_signature());
    for message in [
        invoke(&key(1), "a").message,
        ActionMessage::Cancel { invocation },
        ActionMessage::RecordEffectReceipt {
            invocation,
            redacted: "{}".into(),
        },
    ] {
        assert!(message.requires_signature());
    }
}

#[test]
fn signed_frames_extend_the_v2_envelope_after_every_frozen_variant() {
    let device = key(1);
    let signed = SignedDirectedAction::sign(&invoke(&device, "a"), NOW, &device).unwrap();
    let directed = wire(&signed);
    assert_eq!(directed[0], 9);
    let delivered = encode(&EnvelopeV2::CertifiedSignedDelivered {
        action: signed.clone(),
        source_certificate: b"{}".to_vec(),
    })
    .unwrap();
    assert_eq!(delivered[4], 10);
    assert_eq!(
        decode(&delivered[4..]).unwrap(),
        EnvelopeV2::CertifiedSignedDelivered {
            action: signed,
            source_certificate: b"{}".to_vec(),
        }
    );
}
