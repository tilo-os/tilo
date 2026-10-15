use crate::{
    InviteGrant, Inviter, JoinChallenge, MemberRole, MembershipChallenge,
    TRANSPORT_IROH_OWNER_ENDPOINT_ADDR_V1, decode_and_verify_invitation,
    decode_and_verify_invitation_text, encode_invitation_text, issue_invitation,
    issue_invitation_from, sign_join_proof, sign_membership_proof, sign_transport_hint,
    verify_join_proof, verify_membership_proof,
};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

fn certificate(
    root: &SigningKey,
    device: &SigningKey,
    label: &str,
    issued_at_ns: u64,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

#[test]
fn signed_invitation_and_join_proof_bind_the_exact_ticket_and_independent_identity() {
    let authority_root = SigningKey::from_bytes(&[1; 32]);
    let authority_device = SigningKey::from_bytes(&[2; 32]);
    let authority = certificate(&authority_root, &authority_device, "Owner", 1);
    let guest_root = SigningKey::from_bytes(&[3; 32]);
    let guest_device = SigningKey::from_bytes(&[4; 32]);
    let guest = certificate(&guest_root, &guest_device, "Guest", 1);
    let room = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let grant = InviteGrant {
        workspace_id: 42,
        room: room.into(),
        authority: authority.clone(),
        issuer: authority.clone(),
        delegation: None,
        role: MemberRole::Editor,
        nonce: [5; 16],
        recipient_user_public_key: Some(guest.user_public_key),
        invitee_name: Some("Guest person".into()),
        expires_at_ns: 10_000,
        transport: vec![
            sign_transport_hint(
                TRANSPORT_IROH_OWNER_ENDPOINT_ADDR_V1,
                r#"{"id":"owner-endpoint"}"#,
                authority.clone(),
                authority.device_public_key,
                &authority_device,
            )
            .unwrap(),
        ],
    };

    let signed = issue_invitation(grant.clone(), &authority_device).unwrap();
    let text = encode_invitation_text(&signed);
    assert!(
        !text.contains('='),
        "the canonical text envelope is base64url/no-pad"
    );
    let verified = decode_and_verify_invitation_text(
        &text,
        Some(guest.user_public_key),
        &RevocationList::new(),
        2,
    )
    .unwrap();
    assert_eq!(verified.grant(), &grant);
    assert_eq!(verified.signed_invitation(), signed.as_slice());
    assert_eq!(verified.invite_id(), *blake3::hash(&signed).as_bytes());
    assert!(
        decode_and_verify_invitation(&signed, Some([99; 32]), &RevocationList::new(), 2,)
            .unwrap_err()
            .contains("recipient mismatch")
    );

    let challenge = JoinChallenge::new(&verified, &guest, [6; 32], [7; 32], 9_000, 2).unwrap();
    let proof = sign_join_proof(challenge.clone(), &guest_device).unwrap();
    let redemption =
        verify_join_proof(&proof, &challenge, &guest, &RevocationList::new(), 3).unwrap();
    assert_eq!(redemption.join().invite_id(), verified.invite_id());
    assert_eq!(redemption.certificate(), &guest);
    assert_eq!(redemption.request_id(), [6; 32]);

    let decoded = decode_and_verify_invitation(
        redemption.join().signed_invitation(),
        Some(guest.user_public_key),
        &RevocationList::new(),
        3,
    )
    .unwrap();
    assert_eq!(decoded.invite_id(), verified.invite_id());
}

#[test]
fn durable_membership_proof_is_independent_of_the_original_invitation_lifetime() {
    let authority_root = SigningKey::from_bytes(&[11; 32]);
    let authority_device = SigningKey::from_bytes(&[12; 32]);
    let authority = certificate(&authority_root, &authority_device, "Owner", 1);
    let guest_root = SigningKey::from_bytes(&[13; 32]);
    let guest_device = SigningKey::from_bytes(&[14; 32]);
    let guest = certificate(&guest_root, &guest_device, "Guest", 1);
    let challenge = MembershipChallenge::new(
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        42,
        &authority,
        &guest,
        [15; 32],
        [16; 32],
        10_000,
        2,
    )
    .unwrap();
    let proof = sign_membership_proof(challenge.clone(), &guest_device).unwrap();
    let verified =
        verify_membership_proof(&proof, &challenge, &guest, &RevocationList::new(), 3).unwrap();
    assert_eq!(verified.room(), challenge.room);
    assert_eq!(verified.device_cert(), &guest);
    let mut wrong = challenge;
    wrong.nonce[0] ^= 1;
    assert!(verify_membership_proof(&proof, &wrong, &guest, &RevocationList::new(), 3).is_err());
}

#[test]
fn an_invitation_names_its_inviter_under_the_issuer_signature() {
    let root = SigningKey::from_bytes(&[1; 32]);
    let device = SigningKey::from_bytes(&[2; 32]);
    let authority = certificate(&root, &device, "Node", 1);
    let grant = InviteGrant {
        workspace_id: 42,
        room: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
        authority: authority.clone(),
        issuer: authority,
        delegation: None,
        role: MemberRole::Editor,
        nonce: [5; 16],
        recipient_user_public_key: None,
        invitee_name: None,
        expires_at_ns: 10_000,
        transport: Vec::new(),
    };
    let inviter = Inviter {
        user_public_key: root.verifying_key().to_bytes(),
        name: "Ada Lovelace".into(),
    };
    let signed = issue_invitation_from(grant.clone(), inviter.clone(), &device).unwrap();
    let verified = decode_and_verify_invitation(&signed, None, &RevocationList::new(), 2).unwrap();
    assert_eq!(verified.grant(), &grant);
    assert_eq!(verified.inviter(), Some(&inviter));

    let plain = issue_invitation(grant.clone(), &device).unwrap();
    let verified = decode_and_verify_invitation(&plain, None, &RevocationList::new(), 2).unwrap();
    assert_eq!(
        verified.inviter(),
        None,
        "an invitation without one still reads"
    );

    // The name is covered by the signature.
    let mut forged = signed;
    let at = forged.windows(3).position(|bytes| bytes == b"Ada").unwrap();
    forged.get_mut(at..at + 3).unwrap().copy_from_slice(b"Eve");
    assert!(
        decode_and_verify_invitation(&forged, None, &RevocationList::new(), 2)
            .unwrap_err()
            .contains("signature")
    );

    for name in ["", " Ada", &"x".repeat(81)] {
        let inviter = Inviter {
            user_public_key: [0; 32],
            name: name.into(),
        };
        assert!(issue_invitation_from(grant.clone(), inviter, &device).is_err());
    }
}

#[test]
fn room_workspace_ids_are_stable() {
    // Every peer must derive the same relay shard for a room, so the
    // derivation is pinned to a literal.
    assert_eq!(
        crate::room_workspace_id("0b7e7dfa-3ad5-4b43-9d6c-5f0d7c1f6a11").unwrap(),
        3_029_697_355_180_468_570
    );
    // Parsing normalises the textual form before hashing.
    assert_eq!(
        crate::room_workspace_id("0B7E7DFA-3AD5-4B43-9D6C-5F0D7C1F6A11").unwrap(),
        3_029_697_355_180_468_570
    );
    assert!(crate::room_workspace_id("not a room").is_err());
}
