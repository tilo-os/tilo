use crate::{
    InviteGrant, Inviter, JoinChallenge, JoinCompleteRequest, JoinTransportRequest,
    MembershipChallenge, Role, SignedAdministratorInvitation,
    TRANSPORT_IROH_OWNER_ENDPOINT_ADDR_V1, decode_and_verify_invitation,
    decode_and_verify_invitation_text, encode_invitation_text, issue_administrator_invitation,
    issue_invitation, issue_invitation_from, sign_join_proof, sign_membership_proof,
    sign_transport_hint, verify_join_proof, verify_membership_proof, verify_stored_complete,
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
    let proof =
        collab_types::signer::answered_now(sign_membership_proof(challenge.clone(), &guest_device))
            .unwrap()
            .unwrap();
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

    for name in [" Ada", &"x".repeat(81), "\t"] {
        let inviter = Inviter {
            user_public_key: [0; 32],
            name: name.into(),
        };
        assert!(issue_invitation_from(grant.clone(), inviter, &device).is_err());
    }

    // A person whose name the workspace knows none of is named by their key
    // alone: the invitation carries no name rather than one made of it.
    let signed = issue_invitation_from(
        grant,
        Inviter {
            user_public_key: [0; 32],
            name: String::new(),
        },
        &device,
    )
    .unwrap();
    let verified = decode_and_verify_invitation(&signed, None, &RevocationList::new(), 2).unwrap();
    let unnamed = verified
        .inviter()
        .expect("a key names who asked, with no name");
    assert_eq!(unnamed.user_public_key, [0; 32]);
    assert_eq!(unnamed.name, "");
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
        admission: Vec::new(),
    }))
    .unwrap();
    (challenge, complete)
}

/// `layouts`'s grant issued in every invitation layout this build writes,
/// each with its version and the inviter it names.
fn every_invitation_layout<'a>(
    layouts: &Layouts,
    inviter: &'a Inviter,
) -> [(Vec<u8>, u8, Option<&'a Inviter>); 2] {
    [
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
fn every_invitation_layout_reads_and_verifies_by_its_version() {
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

#[test]
fn a_stored_complete_verifies_in_the_join_layout_its_version_names() {
    let layouts = layouts();
    let revocations = RevocationList::new();
    let invitation = issue_invitation(layouts.grant.clone(), &layouts.issuer_device).unwrap();
    let (_, complete) = current_complete(&layouts, &invitation);
    assert_eq!(
        complete.get(1),
        Some(&4),
        "this build writes join version 4"
    );
    let redemption = verify_stored_complete(&complete, &layouts.guest, &revocations, 3).unwrap();
    assert_eq!(redemption.join().signed_invitation(), invitation.as_slice());
    assert_eq!(redemption.request_id(), [26; 32]);

    // A version no build wrote is refused, not read as another layout.
    let mut unknown = complete;
    *unknown.get_mut(1).unwrap() = 9;
    assert!(verify_stored_complete(&unknown, &layouts.guest, &revocations, 3).is_err());
}

/// An invitation in each layout a build signs, from fixed keys: the bytes
/// kept for its version. Signing is deterministic, so this build signing the
/// same grant gives the same bytes, unless the layout changed under the
/// version.
fn kept_grant() -> (SigningKey, InviteGrant) {
    let root = SigningKey::from_bytes(&[41; 32]);
    let device = SigningKey::from_bytes(&[42; 32]);
    let issuer = certificate(&root, &device, "Owner", 1);
    let grant = InviteGrant {
        workspace_id: crate::WorkspaceId::parse("6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71").unwrap(),
        issuer: issuer.clone(),
        role: Role::Editor,
        nonce: [43; 16],
        recipient_user_public_key: Some([44; 32]),
        invitee_name: Some("Grace".into()),
        expires_at_ns: 10_000,
        transport: vec![
            sign_transport_hint(
                TRANSPORT_IROH_OWNER_ENDPOINT_ADDR_V1,
                r#"{"id":"owner-endpoint"}"#,
                issuer.clone(),
                issuer.device_public_key,
                &device,
            )
            .unwrap(),
        ],
    };
    (device, grant)
}

fn kept_inviter() -> Inviter {
    Inviter {
        user_public_key: SigningKey::from_bytes(&[41; 32]).verifying_key().to_bytes(),
        name: "Ada Lovelace".into(),
    }
}

fn golden(text: &str) -> Vec<u8> {
    text.split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).expect("a hex byte"))
        .collect()
}

/// Every signed invitation layout a build keeps reads the bytes kept for it
/// and verifies them, and this build writes them back exactly: versions 5, 6
/// and 7 signed again. Invitations are never rewritten (their issuer's
/// signature covers every byte), so each version a build signed is read in
/// its own layout for good.
#[test]
fn each_signed_invitation_layout_reads_and_writes_as_it_was_kept() {
    let revocations = RevocationList::new();
    let (device, grant) = kept_grant();
    tilo_files::native::migration::assert_golden_bytes(
        "signed invitation, version 5",
        &golden(include_str!("../golden/signed-invitation/v5.hex")),
        |bytes| {
            assert_eq!(bytes.first(), Some(&5));
            let join = decode_and_verify_invitation(bytes, None, &revocations, 2).unwrap();
            assert_eq!(join.grant(), &grant);
            assert_eq!(join.inviter(), None);
        },
        || issue_invitation(grant.clone(), &device).unwrap(),
    );
    tilo_files::native::migration::assert_golden_bytes(
        "signed invitation, version 6",
        &golden(include_str!("../golden/signed-invitation/v6.hex")),
        |bytes| {
            assert_eq!(bytes.first(), Some(&6));
            let join = decode_and_verify_invitation(bytes, None, &revocations, 2).unwrap();
            assert_eq!(join.grant(), &grant);
            assert_eq!(join.inviter(), Some(&kept_inviter()));
        },
        || issue_invitation_from(grant.clone(), kept_inviter(), &device).unwrap(),
    );
    tilo_files::native::migration::assert_golden_bytes(
        "signed invitation, version 7",
        &golden(include_str!("../golden/signed-invitation/v7.hex")),
        |bytes| {
            assert_eq!(bytes.first(), Some(&7));
            let join = decode_and_verify_invitation(bytes, None, &revocations, 2).unwrap();
            assert_eq!(join.grant(), &grant);
            assert_eq!(join.inviter(), Some(&kept_inviter()));
            assert!(join.administrator());
        },
        || issue_administrator_invitation(grant.clone(), Some(kept_inviter()), &device).unwrap(),
    );
}

/// An administrator's invitation says so under its issuer's signature: the
/// flag cannot be added to, or taken from, an invitation its issuer signed
/// without it, and it names whose authority it carries.
#[test]
fn an_administrator_invitation_is_signed_as_one() {
    let revocations = RevocationList::new();
    let (device, grant) = kept_grant();
    let plain = issue_invitation_from(grant.clone(), kept_inviter(), &device).unwrap();
    assert!(
        !decode_and_verify_invitation(&plain, None, &revocations, 2)
            .unwrap()
            .administrator()
    );
    // Its issuer's own person asked: theirs alone is the authority it carries.
    let own = issue_administrator_invitation(grant.clone(), Some(kept_inviter()), &device).unwrap();
    let join = decode_and_verify_invitation(&own, None, &revocations, 2).unwrap();
    assert!(join.administrator());
    assert_eq!(join.granted_by(), [grant.issuer.user_public_key]);
    // A host signs for someone else: both carry it.
    let asker = Inviter {
        user_public_key: [7; 32],
        name: "Ada".into(),
    };
    let signed = issue_administrator_invitation(grant.clone(), Some(asker), &device).unwrap();
    let join = decode_and_verify_invitation(&signed, None, &revocations, 2).unwrap();
    assert_eq!(join.granted_by(), [grant.issuer.user_public_key, [7; 32]]);
    let mut decoded: SignedAdministratorInvitation = postcard::from_bytes(&signed).unwrap();
    decoded.inviter = Some(Inviter {
        user_public_key: [9; 32],
        name: "Mallory".into(),
    });
    let forged = postcard::to_stdvec(&decoded).unwrap();
    assert!(decode_and_verify_invitation(&forged, None, &revocations, 2).is_err());
    // An administrator joins as an Editor, and no other role.
    let viewer = InviteGrant {
        role: Role::Viewer,
        ..grant
    };
    assert!(issue_administrator_invitation(viewer, None, &device).is_err());
}

#[test]
fn a_member_list_with_devices_crosses_as_json_and_postcard() {
    let list = crate::MemberList {
        members: Vec::new(),
        devices: [
            (crate::MemberId::agent("planner", "ada"), vec![[7; 32]]),
            (crate::MemberId::Bot("mail".into()), vec![[1; 32], [2; 32]]),
        ]
        .into(),
    };
    let json = serde_json::to_string(&list).expect("a member list is JSON");
    assert_eq!(
        serde_json::from_str::<crate::MemberList>(&json).expect("it reads back"),
        list
    );
    let bytes = postcard::to_allocvec(&list).expect("a member list is postcard");
    assert_eq!(
        postcard::from_bytes::<crate::MemberList>(&bytes).expect("it reads back"),
        list
    );
}
