use crate::{
    InviteGrant, Inviter, JoinChallenge, JoinCompleteRequest, JoinTransportRequest,
    MembershipChallenge, Role, TRANSPORT_IROH_OWNER_ENDPOINT_ADDR_V1, decode_and_verify_invitation,
    decode_and_verify_invitation_text, encode_invitation_text, issue_invitation,
    issue_invitation_from, sign_join_proof, sign_membership_proof, sign_transport_hint,
    verify_join_proof, verify_membership_proof, verify_stored_complete,
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
        issuer: None,
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
    let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let grant = InviteGrant {
        workspace_id: crate::WorkspaceId::parse(workspace).unwrap(),
        issuer: authority.clone(),
        role: Role::Editor,
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
    assert_eq!(
        signed.first(),
        Some(&5),
        "this build issues the layout that names the workspace by its id alone"
    );
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
        crate::WorkspaceId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap(),
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
    assert_eq!(verified.workspace_id(), challenge.workspace_id);
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
        workspace_id: crate::WorkspaceId::parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap(),
        issuer: authority,
        role: Role::Editor,
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
    assert_eq!(signed.first(), Some(&6));
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

/// A member's device, an invitee's device and a grant one issues, for the
/// layouts earlier builds wrote.
struct Layouts {
    issuer_device: SigningKey,
    issuer: DeviceCert,
    guest_device: SigningKey,
    guest: DeviceCert,
    grant: InviteGrant,
}

fn layouts() -> Layouts {
    let issuer_device = SigningKey::from_bytes(&[22; 32]);
    let issuer = certificate(
        &SigningKey::from_bytes(&[21; 32]),
        &issuer_device,
        "Owner",
        1,
    );
    let guest_device = SigningKey::from_bytes(&[24; 32]);
    let guest = certificate(
        &SigningKey::from_bytes(&[23; 32]),
        &guest_device,
        "Guest",
        1,
    );
    let grant = InviteGrant {
        workspace_id: crate::WorkspaceId::parse("abee87a1-91ca-46b7-9233-8ab47f9ec3e7").unwrap(),
        issuer: issuer.clone(),
        role: Role::Commenter,
        nonce: [25; 16],
        recipient_user_public_key: Some(guest.user_public_key),
        invitee_name: Some("Guest person".into()),
        expires_at_ns: 10_000,
        transport: vec![
            sign_transport_hint(
                TRANSPORT_IROH_OWNER_ENDPOINT_ADDR_V1,
                r#"{"id":"owner-endpoint"}"#,
                issuer.clone(),
                issuer.device_public_key,
                &issuer_device,
            )
            .unwrap(),
        ],
    };
    Layouts {
        issuer_device,
        issuer,
        guest_device,
        guest,
        grant,
    }
}

/// The Complete a device stores for `signed`, in this build's join layout.
fn current_complete(layouts: &Layouts, signed: &[u8]) -> (JoinChallenge, Vec<u8>) {
    let join = decode_and_verify_invitation(signed, None, &RevocationList::new(), 2).unwrap();
    let challenge =
        JoinChallenge::new(&join, &layouts.guest, [26; 32], [27; 32], 9_000, 2).unwrap();
    let proof = sign_join_proof(challenge.clone(), &layouts.guest_device).unwrap();
    let complete = postcard::to_stdvec(&JoinTransportRequest::Complete(JoinCompleteRequest {
        proof,
    }))
    .unwrap();
    (challenge, complete)
}

#[test]
fn the_number_the_releases_carried_is_the_one_they_derived() {
    // Pinned in the releases' own tests.
    let workspace = crate::WorkspaceId::parse("0b7e7dfa-3ad5-4b43-9d6c-5f0d7c1f6a11").unwrap();
    assert_eq!(
        crate::room_layout::workspace_number(workspace),
        3_029_697_355_180_468_570
    );
}

/// `layouts`'s grant issued in every invitation layout, each with its
/// version and the inviter it names.
fn every_invitation_layout<'a>(
    layouts: &Layouts,
    inviter: &'a Inviter,
) -> [(Vec<u8>, u8, Option<&'a Inviter>); 6] {
    [
        (
            crate::issue_previous_invitation(
                &layouts.grant,
                None,
                &layouts.issuer,
                &layouts.issuer_device,
            )
            .unwrap(),
            1,
            None,
        ),
        (
            crate::issue_previous_invitation(
                &layouts.grant,
                Some(inviter),
                &layouts.issuer,
                &layouts.issuer_device,
            )
            .unwrap(),
            2,
            Some(inviter),
        ),
        (
            crate::issue_room_layout_invitation(&layouts.grant, None, &layouts.issuer_device)
                .unwrap(),
            3,
            None,
        ),
        (
            crate::issue_room_layout_invitation(
                &layouts.grant,
                Some(inviter),
                &layouts.issuer_device,
            )
            .unwrap(),
            4,
            Some(inviter),
        ),
        (
            issue_invitation(layouts.grant.clone(), &layouts.issuer_device).unwrap(),
            5,
            None,
        ),
        (
            issue_invitation_from(
                layouts.grant.clone(),
                inviter.clone(),
                &layouts.issuer_device,
            )
            .unwrap(),
            6,
            Some(inviter),
        ),
    ]
}

#[test]
fn every_released_invitation_layout_reads_and_verifies_by_its_version() {
    let layouts = layouts();
    let inviter = Inviter {
        user_public_key: layouts.issuer.user_public_key,
        name: "Ada Lovelace".into(),
    };
    let revocations = RevocationList::new();
    for (signed, version, inviter) in every_invitation_layout(&layouts, &inviter) {
        assert_eq!(signed.first(), Some(&version));
        let join = decode_and_verify_invitation(
            &signed,
            Some(layouts.guest.user_public_key),
            &revocations,
            2,
        )
        .unwrap_or_else(|error| panic!("version {version}: {error}"));
        assert_eq!(join.grant(), &layouts.grant, "version {version}");
        assert_eq!(join.inviter(), inviter, "version {version}");
        assert_eq!(join.workspace_authority(), &layouts.issuer);
        assert_eq!(join.invite_id(), *blake3::hash(&signed).as_bytes());
        assert_eq!(join.signed_invitation(), signed.as_slice());
        // Each layout's signature covers its own bytes.
        let mut forged = signed.clone();
        let at = forged
            .windows(12)
            .position(|bytes| bytes == b"Guest person")
            .unwrap();
        forged
            .get_mut(at..at + 5)
            .unwrap()
            .copy_from_slice(b"Mallo");
        assert!(
            decode_and_verify_invitation(&forged, None, &revocations, 2)
                .unwrap_err()
                .contains("signature"),
            "version {version}"
        );
        // A released invitation shares its issuer and transport like today's.
        if version >= 3 {
            assert_eq!(
                crate::invitation_shared_runs(&signed),
                vec![
                    postcard::to_stdvec(&layouts.issuer).unwrap(),
                    postcard::to_stdvec(&layouts.grant.transport).unwrap(),
                ],
                "version {version}"
            );
        }
    }
}

#[test]
fn a_released_grant_that_names_no_canonical_workspace_is_refused() {
    let layouts = layouts();
    let signed =
        crate::issue_room_layout_invitation(&layouts.grant, None, &layouts.issuer_device).unwrap();
    let canonical = layouts.grant.workspace_id.to_string();
    let at = signed
        .windows(canonical.len())
        .position(|bytes| bytes == canonical.as_bytes())
        .unwrap();
    let mut upper = signed;
    upper
        .get_mut(at..at + canonical.len())
        .unwrap()
        .copy_from_slice(canonical.to_uppercase().as_bytes());
    assert!(
        decode_and_verify_invitation(&upper, None, &RevocationList::new(), 2)
            .unwrap_err()
            .contains("canonical")
    );
}

#[test]
fn a_stored_complete_verifies_in_the_join_layout_its_version_names() {
    let layouts = layouts();
    let revocations = RevocationList::new();
    let room_layout_invitation =
        crate::issue_room_layout_invitation(&layouts.grant, None, &layouts.issuer_device).unwrap();
    let previous_invitation = crate::issue_previous_invitation(
        &layouts.grant,
        None,
        &layouts.issuer,
        &layouts.issuer_device,
    )
    .unwrap();
    let current_invitation =
        issue_invitation(layouts.grant.clone(), &layouts.issuer_device).unwrap();

    // This build's layout, over an invitation of every layout it reads.
    for signed in [
        &current_invitation,
        &room_layout_invitation,
        &previous_invitation,
    ] {
        let (_, complete) = current_complete(&layouts, signed);
        assert_eq!(
            complete.get(1),
            Some(&3),
            "this build writes join version 3"
        );
        let redemption =
            verify_stored_complete(&complete, &layouts.guest, &revocations, 3).unwrap();
        assert_eq!(redemption.join().signed_invitation(), signed.as_slice());
        assert_eq!(redemption.request_id(), [26; 32]);
    }

    // The releases' layout, over the invitations they issued.
    for signed in [&room_layout_invitation, &previous_invitation] {
        let (challenge, _) = current_complete(&layouts, signed);
        let complete = crate::complete_room_layout(&challenge, &layouts.guest_device).unwrap();
        assert_eq!(complete.get(1), Some(&2));
        let redemption =
            verify_stored_complete(&complete, &layouts.guest, &revocations, 3).unwrap();
        assert_eq!(redemption.join().grant(), &layouts.grant);
        assert_eq!(redemption.certificate(), &layouts.guest);
        // Another device's certificate does not take its proof.
        assert!(verify_stored_complete(&complete, &layouts.issuer, &revocations, 3).is_err());
        // Its signature covers the layout it was signed in.
        let mut forged = complete.clone();
        let last = forged.len() - 1;
        *forged.get_mut(last).unwrap() ^= 1;
        assert!(verify_stored_complete(&forged, &layouts.guest, &revocations, 3).is_err());
    }

    // The layout before that, which named the authority.
    let (challenge, _) = current_complete(&layouts, &previous_invitation);
    let complete =
        crate::complete_previous_layout(&challenge, &layouts.issuer, &layouts.guest_device)
            .unwrap();
    assert_eq!(complete.get(1), Some(&1));
    let redemption = verify_stored_complete(&complete, &layouts.guest, &revocations, 3).unwrap();
    assert_eq!(redemption.join().workspace_authority(), &layouts.issuer);

    // A version no build wrote is refused, not read as another layout.
    let mut unknown = complete;
    *unknown.get_mut(1).unwrap() = 9;
    assert!(verify_stored_complete(&unknown, &layouts.guest, &revocations, 3).is_err());
}

/// The invitation the Lounge's owner left open on the live relay, as its
/// release (v0.1.5) signed and kept it: invitation version 3, which named
/// the workspace by a number and its UUID as text.
/// (Kept as hex text: two lowercase digits per byte, 16 bytes per line.)
static RELEASED_OPEN_INVITATION: std::sync::LazyLock<Vec<u8>> = std::sync::LazyLock::new(|| {
    include_str!("v0.1.5-lounge-open-invitation.v3.hex")
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("a hex byte"))
        .collect()
});

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|byte| [byte >> 4, byte & 0x0f])
        .filter_map(|nibble| char::from_digit(u32::from(nibble), 16))
        .collect()
}

/// A time inside that invitation's window, before it lapses on 2026-11-20.
const RELEASED_NOW_NS: u64 = 1_795_000_000_000_000_000;

#[test]
fn the_invitation_the_release_left_open_reads_and_verifies() {
    let revocations = RevocationList::new();
    assert_eq!(RELEASED_OPEN_INVITATION.as_slice().first(), Some(&3));
    let join = decode_and_verify_invitation(
        RELEASED_OPEN_INVITATION.as_slice(),
        None,
        &revocations,
        RELEASED_NOW_NS,
    )
    .unwrap();
    assert_eq!(
        join.grant().workspace_id,
        crate::WorkspaceId::parse("abee87a1-91ca-46b7-9233-8ab47f9ec3e7").unwrap()
    );
    assert_eq!(
        hex(&join.invite_id()),
        "c1c38450577ca739ef152e72b7300099e32ef135bf39919bbde5b16a19c2d071"
    );
    assert_eq!(
        join.invite_id(),
        *blake3::hash(RELEASED_OPEN_INVITATION.as_slice()).as_bytes()
    );
    assert_eq!(
        join.signed_invitation(),
        RELEASED_OPEN_INVITATION.as_slice()
    );
    assert_eq!(join.workspace_authority(), &join.grant().issuer);
    assert_eq!(join.grant().recipient_user_public_key, None);
    assert_eq!(
        crate::invitation_shared_runs(RELEASED_OPEN_INVITATION.as_slice()).len(),
        2
    );

    // Its link is still redeemed: a device it never met answers a challenge
    // over it, in this build's join layout.
    let guest_device = SigningKey::from_bytes(&[31; 32]);
    let guest = certificate(
        &SigningKey::from_bytes(&[32; 32]),
        &guest_device,
        "Guest",
        1,
    );
    let text = encode_invitation_text(RELEASED_OPEN_INVITATION.as_slice());
    let join = decode_and_verify_invitation_text(
        &text,
        Some(guest.user_public_key),
        &revocations,
        RELEASED_NOW_NS,
    )
    .unwrap();
    let challenge = JoinChallenge::new(
        &join,
        &guest,
        [33; 32],
        [34; 32],
        RELEASED_NOW_NS + 1_000,
        RELEASED_NOW_NS,
    )
    .unwrap();
    let proof = sign_join_proof(challenge.clone(), &guest_device).unwrap();
    let redemption =
        verify_join_proof(&proof, &challenge, &guest, &revocations, RELEASED_NOW_NS).unwrap();
    assert_eq!(redemption.join().invite_id(), join.invite_id());

    // Every byte is under its issuer's signature.
    let mut forged = RELEASED_OPEN_INVITATION.as_slice().to_vec();
    let last = forged.len() - 1;
    *forged.get_mut(last).unwrap() ^= 1;
    assert!(decode_and_verify_invitation(&forged, None, &revocations, RELEASED_NOW_NS).is_err());
}
