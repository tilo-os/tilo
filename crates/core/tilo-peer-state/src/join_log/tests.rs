use super::*;
use crate::admission_tests::seeded;
use crate::join::{
    JoinChallengeRequest, JoinCompleteRequest, MembershipCompleteRequest, sign_join_proof,
    sign_membership_proof,
};
use ed25519_dalek::SigningKey;
use tilo_user_identity::{
    DeviceCapabilities, EndpointDelegation, RevocationList, sign_device_cert,
};

fn ns(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

fn guest() -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[41; 32]);
    let key = SigningKey::from_bytes(&[42; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "Guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

fn invitation(recipient: Option<[u8; 32]>, now: u64) -> IssueInvitationRequest {
    IssueInvitationRequest {
        request_id: [7; 32],
        role: Role::Editor,
        recipient_user_public_key: recipient,
        invitee_name: Some("Guest".into()),
        expires_at_ns: now.saturating_add(10_000_000_000),
        inviter_name: None,
    }
}

/// One log serves every governor: issuing, challenging, redeeming and
/// completing a join through it admits the device into the workspace, answers
/// a retry of the same Complete with the same bootstrap, and spends the
/// invitation.
#[test]
#[expect(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "one join read top to bottom, through each step of the log"
)]
fn a_join_through_the_log_admits_once_and_replays_its_answer() {
    let (peer, authority_key, _, _, now) = seeded();
    let now_ns = ns(now);
    let messages = || Ok(vec![b"governance".to_vec()]);
    let server_cert = peer.owner_certificates()[0].clone();
    let workspace = Governed {
        peer: (&peer).into(),
        host: &server_cert,
        workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
        generation: 1,
        governance_messages: Some(&messages),
    };
    let (guest_key, guest_cert) = guest();
    let mut log = JoinLog::default();
    assert!(log.is_empty());

    let request = invitation(Some(guest_cert.user_public_key), now_ns);
    let expiry = request.expires_at_ns;
    let issued = log
        .issue(
            &workspace,
            &peer.owner_certificates()[0],
            request,
            Vec::new(),
            Signer {
                key: &authority_key,
                nonce: [43; 16],
            },
            now_ns,
        )
        .unwrap();
    let again = log
        .issue(
            &workspace,
            &peer.owner_certificates()[0],
            invitation(Some(guest_cert.user_public_key), now_ns),
            Vec::new(),
            Signer {
                key: &authority_key,
                nonce: [99; 16],
            },
            now_ns,
        )
        .unwrap();
    assert_eq!(issued, again, "the same request answers what it issued");
    assert_eq!(log.pending(now_ns).len(), 1);
    let verified = log
        .inspect(&workspace, &issued.signed_invitation, now_ns)
        .unwrap();
    let reference = crate::join::native::InvitationRef::of(&verified);
    let resolve =
        |log: &JoinLog,
         reference: &crate::join::native::InvitationRef,
         cert: &DeviceCert,
         endpoint,
         at| { log.resolve_invitation(&workspace, reference, cert, endpoint, at) };
    assert_eq!(
        resolve(
            &log,
            &reference,
            &guest_cert,
            guest_cert.device_public_key,
            now_ns
        )
        .unwrap(),
        issued.signed_invitation
    );
    let bearer = crate::join::native::parse_bearer(&crate::join::native::bearer_text(
        &issued.signed_invitation,
    ))
    .unwrap();
    assert_eq!(
        log.invitation_by_bearer(&bearer, now_ns),
        Some(issued.signed_invitation.clone())
    );
    assert!(
        log.invitation_by_bearer(&bearer, expiry).is_none(),
        "a lapsed invitation is not handed out"
    );
    assert!(log.invitation_by_bearer(&[9; 32], now_ns).is_none());
    assert!(
        resolve(
            &log,
            &reference,
            &guest_cert,
            guest_cert.device_public_key,
            expiry
        )
        .is_err()
    );
    assert!(
        resolve(
            &log,
            &reference,
            workspace.host,
            workspace.host.device_public_key,
            now_ns
        )
        .is_err()
    );
    assert!(resolve(&log, &reference, &guest_cert, [9; 32], now_ns).is_err());
    let mut wrong = reference.clone();
    wrong.workspace = WorkspaceId::new();
    assert!(
        resolve(
            &log,
            &wrong,
            &guest_cert,
            guest_cert.device_public_key,
            now_ns
        )
        .is_err()
    );
    wrong.workspace = reference.workspace;
    wrong.invite_id = [9; 32];
    assert!(
        resolve(
            &log,
            &wrong,
            &guest_cert,
            guest_cert.device_public_key,
            now_ns
        )
        .is_err()
    );
    let other_holder = Governed {
        peer: (&peer).into(),
        host: &guest_cert,
        workspace_id: workspace.workspace_id,
        generation: 1,
        governance_messages: Some(&messages),
    };
    assert!(
        log.resolve_invitation(
            &other_holder,
            &reference,
            &guest_cert,
            guest_cert.device_public_key,
            now_ns
        )
        .is_err()
    );

    let challenge = log
        .challenge(
            &workspace,
            guest_cert.device_public_key,
            JoinChallengeRequest {
                request_id: [44; 32],
                signed_invitation: issued.signed_invitation.clone(),
                device_cert: guest_cert.clone(),
            },
            [45; 32],
            now_ns,
        )
        .unwrap();
    let complete = JoinCompleteRequest {
        proof: sign_join_proof(challenge, &guest_key).unwrap(),
        admission: Vec::new(),
    };
    assert!(
        log.redeem(&workspace, [9; 32], &complete.proof, now_ns)
            .is_err(),
        "a proof is redeemed only from the endpoint it was challenged on"
    );
    let redemption = log
        .redeem(
            &workspace,
            guest_cert.device_public_key,
            &complete.proof,
            now_ns,
        )
        .unwrap();
    let proof_digest = request_digest(&complete.proof).unwrap();
    // The guest's device signs its own admission, naming its person.
    let admission = crate::own_admission::own_admission(
        peer.workspace_id(),
        &[postcard::to_stdvec(&peer.workspace_message()).unwrap()],
        &redemption,
        Some("Calm Owl"),
        &guest_key,
        now,
    )
    .unwrap();
    let joins = [Redeeming {
        remote_endpoint: guest_cert.device_public_key,
        request_id: [44; 32],
        proof_digest,
        redemption: &redemption,
        admission: &admission,
    }];
    let mut candidate = peer.clone();
    let refusals = log.admit(&mut candidate, &joins, now_ns);
    assert!(refusals.iter().all(Option::is_none), "{refusals:?}");
    let guest_member = candidate
        .governance()
        .governed_members(now)
        .unwrap()
        .members
        .into_iter()
        .find(|member| member.root() == Some(guest_cert.user_public_key))
        .unwrap();
    assert_eq!(
        guest_member.name.as_str(),
        "Calm Owl",
        "the workspace names the guest as they named themself, not by their device"
    );
    let server_cert = candidate.owner_certificates()[0].clone();
    let committed = Governed {
        peer: (&candidate).into(),
        host: &server_cert,
        workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
        generation: 2,
        governance_messages: Some(&messages),
    };
    let delivered = log.complete(&committed, &joins, &refusals).unwrap();
    let bootstrap = delivered.into_iter().next().unwrap().unwrap();
    assert_eq!(bootstrap.device_cert, guest_cert);
    assert_eq!(bootstrap.generation, 2);
    assert!(
        candidate
            .permissions(guest_cert.device_public_key, now)
            .is_ok(),
        "the guest is a member of the workspace"
    );
    assert!(log.redeemed(issued.invite_id));
    assert_eq!(
        log.admitted_device([44; 32]),
        Some(guest_cert.device_public_key)
    );
    assert!(log.pending(now_ns).is_empty(), "the invitation is spent");
    assert!(
        log.invitation_by_bearer(&bearer, now_ns).is_none(),
        "a spent invitation is not handed to whoever holds its link"
    );
    assert!(
        resolve(
            &log,
            &reference,
            &guest_cert,
            guest_cert.device_public_key,
            now_ns
        )
        .is_err()
    );

    let replayed = log
        .completed(
            &committed,
            guest_cert.device_public_key,
            [44; 32],
            proof_digest,
            now_ns,
        )
        .unwrap();
    assert_eq!(
        replayed,
        Some(committed.filled(bootstrap).unwrap()),
        "a retry hears the answer the device was sent"
    );
    let later = || Ok(vec![b"later governance".to_vec()]);
    let moved = Governed {
        peer: (&candidate).into(),
        host: &server_cert,
        workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
        generation: 3,
        governance_messages: Some(&later),
    };
    let refresh = log
        .completed(
            &moved,
            guest_cert.device_public_key,
            [44; 32],
            proof_digest,
            now_ns,
        )
        .unwrap_err();
    assert_eq!(refresh.code(), crate::join::JOIN_COMPLETED_REFRESH);
    assert!(
        log.redeem(
            &committed,
            guest_cert.device_public_key,
            &complete.proof,
            now_ns
        )
        .is_err(),
        "a spent invitation admits nobody again"
    );
}

/// `cert`, going by its own label, joins `workspace` through `log` under a
/// bearer invitation, saying `name` as it comes in: the workspace after it.
fn joined(
    log: &mut JoinLog,
    workspace: &Governed<'_>,
    authority_key: &SigningKey,
    (key, cert): (&SigningKey, &DeviceCert),
    name: &str,
    seed: u8,
    now_ns: u64,
) -> StatePeer {
    let issued = log
        .issue(
            workspace,
            &workspace.peer.owner_certificates()[0],
            IssueInvitationRequest {
                request_id: [seed; 32],
                role: Role::Editor,
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: now_ns.saturating_add(10_000_000_000),
                inviter_name: None,
            },
            Vec::new(),
            Signer {
                key: authority_key,
                nonce: [seed.wrapping_add(1); 16],
            },
            now_ns,
        )
        .unwrap();
    let challenge = log
        .challenge(
            workspace,
            cert.device_public_key,
            JoinChallengeRequest {
                request_id: [seed.wrapping_add(2); 32],
                signed_invitation: issued.signed_invitation,
                device_cert: cert.clone(),
            },
            [seed.wrapping_add(3); 32],
            now_ns,
        )
        .unwrap();
    let proof = sign_join_proof(challenge, key).unwrap();
    let redemption = log
        .redeem(workspace, cert.device_public_key, &proof, now_ns)
        .unwrap();
    let admission = crate::own_admission::own_admission(
        workspace.peer.workspace_id(),
        &[postcard::to_stdvec(&workspace.peer.workspace_message()).unwrap()],
        &redemption,
        Some(name),
        key,
        system_time(now_ns),
    )
    .unwrap();
    let joins = [Redeeming {
        remote_endpoint: cert.device_public_key,
        request_id: [seed.wrapping_add(2); 32],
        proof_digest: request_digest(&proof).unwrap(),
        redemption: &redemption,
        admission: &admission,
    }];
    let mut candidate = (*workspace.peer).clone();
    let refusals = log.admit(&mut candidate, &joins, now_ns);
    assert!(refusals.iter().all(Option::is_none), "{refusals:?}");
    candidate
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    clippy::too_many_lines,
    reason = "the governed workspace reads the peer through each step of the decision, an admission and the invitation a key of the same person asks for"
)]
fn a_key_made_to_act_for_a_person_never_names_them() {
    let (peer, authority_key, _, _, now) = seeded();
    let now_ns = ns(now);
    let messages = || Ok(Vec::<Vec<u8>>::new());
    let server_cert = peer.owner_certificates()[0].clone();
    let workspace = Governed {
        peer: (&peer).into(),
        host: &server_cert,
        workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
        generation: 1,
        governance_messages: Some(&messages),
    };
    let root = SigningKey::from_bytes(&[51; 32]);
    let mut log = JoinLog::default();
    // Ada joins on a device of her own, going by her own name.
    let ada_key = SigningKey::from_bytes(&[52; 32]);
    let mut ada = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: ada_key.verifying_key().to_bytes(),
        device_label: "Ada".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut ada, &root).unwrap();
    let joined_ada = joined(
        &mut log,
        &workspace,
        &authority_key,
        (&ada_key, &ada),
        "Ada",
        60,
        now_ns,
    );
    let named = |member_of: &StatePeer| {
        member_of
            .governance()
            .governed_members(now)
            .unwrap()
            .members
            .into_iter()
            .find(|member| member.root() == Some(root.verifying_key().to_bytes()))
            .unwrap()
            .name
    };
    assert_eq!(named(&joined_ada), "Ada");
    // A key made to act for her joins too, saying what the key goes by, as
    // the API hands it over: it names the key, never Ada.
    let backend_key = SigningKey::from_bytes(&[53; 32]);
    let mut backend = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: backend_key.verifying_key().to_bytes(),
        device_label: "Ada's backend".into(),
        capabilities: DeviceCapabilities::server_key_grant(),
        issued_at_ns: 2,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut backend, &root).unwrap();
    let again = Governed {
        peer: (&joined_ada).into(),
        host: &server_cert,
        workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
        generation: 2,
        governance_messages: Some(&messages),
    };
    let joined_both = joined(
        &mut log,
        &again,
        &authority_key,
        (&backend_key, &backend),
        "Jane Doe's agent",
        70,
        now_ns,
    );
    assert_eq!(
        named(&joined_both),
        "Ada",
        "a key of hers names its key, never the person it acts for"
    );
    // An invitation her key asks for is signed as her, with no name rather
    // than one cut from her key: whoever opens it reads a person, and the
    // page names them its own way.
    let asked = log
        .issue_as(
            &again,
            &backend,
            IssueInvitationRequest {
                request_id: [80; 32],
                role: Role::Editor,
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: now_ns.saturating_add(10_000_000_000),
                inviter_name: None,
            },
            true,
            Vec::new(),
            Signer {
                key: &authority_key,
                nonce: [81; 16],
            },
            now_ns,
        )
        .unwrap();
    let join = crate::join::decode_and_verify_invitation(
        &asked.signed_invitation,
        None,
        &RevocationList::new(),
        now_ns,
    )
    .unwrap();
    assert!(join.administrator(), "an owner link makes an owner");
    let from = join.inviter().expect("the invitation names who asked");
    assert_eq!(from.user_public_key, root.verifying_key().to_bytes());
    assert_eq!(from.name, "", "a key is no name of its person");
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the governed workspace reads the peer through each step of the decision"
)]
fn a_cancelled_invitation_is_not_redeemable_and_cancelling_again_changes_nothing() {
    let (peer, authority_key, _, _, now) = seeded();
    let now_ns = ns(now);
    let messages = || Ok(Vec::<Vec<u8>>::new());
    let server_cert = peer.owner_certificates()[0].clone();
    let workspace = Governed {
        peer: (&peer).into(),
        host: &server_cert,
        workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
        generation: 1,
        governance_messages: Some(&messages),
    };
    let mut log = JoinLog::default();
    let issued = log
        .issue(
            &workspace,
            &peer.owner_certificates()[0],
            invitation(None, now_ns),
            Vec::new(),
            Signer {
                key: &authority_key,
                nonce: [43; 16],
            },
            now_ns,
        )
        .unwrap();
    let cancel = CancelInvitationRequest {
        request_id: [8; 32],
        invite_id: issued.invite_id,
    };
    let verified = log
        .inspect(&workspace, &issued.signed_invitation, now_ns)
        .unwrap();
    let reference = crate::join::native::InvitationRef::of(&verified);
    let bearer = crate::join::native::parse_bearer(&crate::join::native::bearer_text(
        &issued.signed_invitation,
    ))
    .unwrap();
    assert_eq!(
        log.invitation_by_bearer(&bearer, now_ns),
        Some(issued.signed_invitation.clone()),
        "a web link's secret names the invitation while it stands"
    );
    assert!(log.cancel(&cancel).unwrap());
    assert!(
        log.invitation_by_bearer(&bearer, now_ns).is_none(),
        "an invitation taken back is no longer handed to whoever holds its link"
    );
    assert!(!log.cancel(&cancel).unwrap());
    assert!(
        log.cancel(&CancelInvitationRequest {
            request_id: [8; 32],
            invite_id: [1; 32],
        })
        .is_err()
    );
    assert!(log.pending(now_ns).is_empty());
    let (_, guest) = guest();
    assert!(
        log.resolve_invitation(
            &workspace,
            &reference,
            &guest,
            guest.device_public_key,
            now_ns
        )
        .is_err()
    );
    assert!(
        log.inspect(&workspace, &issued.signed_invitation, now_ns)
            .is_err()
    );
}

#[test]
fn an_administration_request_is_answered_once_and_refused_for_another_operation() {
    let mut log = JoinLog::default();
    assert!(matches!(
        log.administered([1; 32], [2; 32]).unwrap(),
        Administered::New
    ));
    log.record_administration([1; 32], [2; 32], 5);
    assert!(matches!(
        log.administered([1; 32], [2; 32]).unwrap(),
        Administered::Done(5)
    ));
    assert!(log.administered([1; 32], [3; 32]).is_err());
}

#[test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the governed workspace reads the peer through each step of the decision"
)]
fn a_join_receipt_keeps_none_of_the_workspaces_state_and_replays_while_it_is_unchanged() {
    let (peer, _, _, _, _) = seeded();
    let messages = || Ok(vec![b"governance".to_vec()]);
    let server_cert = peer.owner_certificates()[0].clone();
    let workspace = Governed {
        peer: (&peer).into(),
        host: &server_cert,
        workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
        generation: 1,
        governance_messages: Some(&messages),
    };
    let (_, cert) = guest();
    let admitted = workspace.admission([9; 32], cert, Role::Editor);
    let delivered = workspace.filled(admitted.clone()).unwrap();
    assert!(!delivered.certified_roster.is_empty());
    let receipt = JoinCompletionRecord::receipt([1; 32], [2; 32], [3; 32], admitted);
    assert!(receipt.bootstrap.certified_roster.is_empty());
    assert!(receipt.bootstrap.records.is_empty());
    assert!(receipt.bootstrap.governance_messages.is_empty());
    assert_eq!(receipt.replay(&workspace).unwrap(), delivered);
    // Any governance change since moves the workspace's generation on: the
    // reply delivered then is not the workspace now, and the member refreshes.
    let moved = Governed {
        generation: 2,
        ..workspace
    };
    assert!(receipt.replay(&moved).is_err());
}

/// How long a delegation in these tests lasts: ten seconds of the seeded
/// workspace's own clock.
const DELEGATION_LIFETIME_NS: u64 = 10_000_000_000;

/// A workspace with an editor in it, and a page: a second device of that
/// editor's person, certified by the editor's root, which the workspace admits
/// as that member's device. The page's device key never leaves it, so it dials
/// from a key of its own.
struct Client {
    peer: StatePeer,
    workspace_id: WorkspaceId,
    host: DeviceCert,
    page_key: SigningKey,
    page_cert: DeviceCert,
    now_ns: u64,
}

impl Client {
    fn seeded_page() -> Self {
        let (mut peer, owner_key, _, _, now) = seeded();
        let now_ns = ns(now);
        let editor_root = SigningKey::from_bytes(&[5; 32]);
        crate::admission_tests::add_member_with(&mut peer, &owner_key, now, 5, 6);
        let page_key = SigningKey::from_bytes(&[9; 32]);
        let mut page_cert = DeviceCert {
            user_public_key: editor_root.verifying_key().to_bytes(),
            device_public_key: page_key.verifying_key().to_bytes(),
            device_label: "Page".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut page_cert, &editor_root).unwrap();
        assert!(
            peer.governance_mut()
                .admit_member_device(&page_cert, &page_key, now)
                .unwrap(),
            "a member's further device lists itself"
        );
        let host = peer.owner_certificates()[0].clone();
        Self {
            workspace_id: WorkspaceId::parse(peer.workspace_id()).unwrap(),
            peer,
            host,
            page_key,
            page_cert,
            now_ns,
        }
    }

    fn workspace(&self) -> Governed<'_> {
        Governed {
            peer: (&self.peer).into(),
            host: &self.host,
            workspace_id: self.workspace_id,
            generation: 1,
            governance_messages: None,
        }
    }

    fn request(&self, delegation: Option<EndpointDelegation>) -> MembershipChallengeRequest {
        MembershipChallengeRequest {
            workspace_id: self.workspace_id,
            device_cert: self.page_cert.clone(),
            request_id: [17; 32],
            delegation,
        }
    }

    /// The delegation this page's device signs for the endpoint seeded `endpoint`,
    /// issued `issued_at_ns` and lasting [`DELEGATION_LIFETIME_NS`].
    fn delegation_for(&self, endpoint: u8, issued_at_ns: u64) -> EndpointDelegation {
        tilo_user_identity::sign_endpoint_delegation(
            &self.page_key,
            [endpoint; 32],
            issued_at_ns,
            DELEGATION_LIFETIME_NS,
        )
        .unwrap()
    }
}

/// A page joins over the endpoint its device delegated: the host challenges
/// the certificate and answers the membership the proof carries.
#[test]
fn a_page_joins_over_the_endpoint_its_device_delegated() {
    let client = Client::seeded_page();
    let endpoint = [7; 32];
    let mut log = JoinLog::default();
    let challenge = log
        .membership_challenge(
            &client.workspace(),
            endpoint,
            client.request(Some(client.delegation_for(7, client.now_ns))),
            [21; 32],
            client.now_ns,
        )
        .unwrap();
    let proof =
        collab_types::signer::answered_now(sign_membership_proof(challenge, &client.page_key))
            .unwrap()
            .unwrap();
    let bootstrap = log
        .membership_complete(
            &client.workspace(),
            endpoint,
            &MembershipCompleteRequest {
                proof,
                admission: Vec::new(),
            },
            client.now_ns,
        )
        .unwrap();
    assert_eq!(bootstrap.device_cert, client.page_cert);
    assert_eq!(bootstrap.workspace_id, client.workspace_id);
}

/// A device that dials as its own key still joins with no delegation at all.
#[test]
fn a_device_joins_over_its_own_key() {
    let client = Client::seeded_page();
    let endpoint = client.page_cert.device_public_key;
    let mut log = JoinLog::default();
    let challenge = log
        .membership_challenge(
            &client.workspace(),
            endpoint,
            client.request(None),
            [22; 32],
            client.now_ns,
        )
        .unwrap();
    let proof =
        collab_types::signer::answered_now(sign_membership_proof(challenge, &client.page_key))
            .unwrap()
            .unwrap();
    let bootstrap = log
        .membership_complete(
            &client.workspace(),
            endpoint,
            &MembershipCompleteRequest {
                proof,
                admission: Vec::new(),
            },
            client.now_ns,
        )
        .unwrap();
    assert_eq!(bootstrap.device_cert, client.page_cert);
}

/// A delegation another device signed speaks for that device, never for the
/// page the request certifies.
#[test]
fn a_delegation_signed_by_another_device_is_refused() {
    let client = Client::seeded_page();
    let endpoint = [7; 32];
    let stranger = SigningKey::from_bytes(&[3; 32]);
    let forged = tilo_user_identity::sign_endpoint_delegation(
        &stranger,
        endpoint,
        client.now_ns,
        DELEGATION_LIFETIME_NS,
    )
    .unwrap();
    let mut log = JoinLog::default();
    let refused = log
        .membership_challenge(
            &client.workspace(),
            endpoint,
            client.request(Some(forged)),
            [23; 32],
            client.now_ns,
        )
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("does not match authenticated workspace endpoint"),
        "{refused}"
    );
}

/// A delegation that has run out admits nothing, though its signature is the
/// device's own.
#[test]
fn an_expired_delegation_is_refused() {
    let client = Client::seeded_page();
    let endpoint = [7; 32];
    let expired =
        client.delegation_for(7, client.now_ns.saturating_sub(2 * DELEGATION_LIFETIME_NS));
    let mut log = JoinLog::default();
    let refused = log
        .membership_challenge(
            &client.workspace(),
            endpoint,
            client.request(Some(expired)),
            [24; 32],
            client.now_ns,
        )
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("does not match authenticated workspace endpoint"),
        "{refused}"
    );
}

/// A delegation names exactly one transport key: one for another key does not
/// let this endpoint in.
#[test]
fn a_delegation_naming_another_endpoint_is_refused() {
    let client = Client::seeded_page();
    let mut log = JoinLog::default();
    let refused = log
        .membership_challenge(
            &client.workspace(),
            [7; 32],
            client.request(Some(client.delegation_for(8, client.now_ns))),
            [25; 32],
            client.now_ns,
        )
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("does not match authenticated workspace endpoint"),
        "{refused}"
    );
}

/// A page that dials from a key of its own without saying whose it is gets no
/// challenge.
#[test]
fn a_derived_endpoint_without_a_delegation_is_refused() {
    let client = Client::seeded_page();
    let mut log = JoinLog::default();
    let refused = log
        .membership_challenge(
            &client.workspace(),
            [7; 32],
            client.request(None),
            [26; 32],
            client.now_ns,
        )
        .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("does not match authenticated workspace endpoint"),
        "{refused}"
    );
}
