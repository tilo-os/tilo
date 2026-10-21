//! Pairing a person's new device through a host, as the page does it: the
//! new device holds no root, asks the host to certify it for the person
//! (`certify`) and shows its pairing code; a device of theirs that holds
//! the person's root lists the waiting requests (`pending_pairings`) and
//! approves or declines one (`answer_pairing`), both over HTTPS. The host
//! only carries and checks: it hands on nothing a root did not sign for
//! exactly that request, and only to requests naming a person it serves.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; each test is one pairing, in order"
)]

#[path = "support/steward.rs"]
mod steward;
mod support;

// This binary re-execs as the automerge worker (a governed room uses it).
tilo_automerge_isolate::test_worker!();

use std::collections::BTreeSet;
use std::sync::Arc;

use collab_types::DocumentMutation;
use ed25519_dalek::{Signer as _, SigningKey};
use getrandom::{SysRng, rand_core::UnwrapErr};
use sha2::{Digest as _, Sha256};
use tilo_browser_peer::invite::{ask_to_join_pre_admitted, guest_certificate};
use tilo_browser_peer::{
    CertifyError, Host, HttpClient, LinkError, MemoryPeerStore, PeerHandle, PeerStore as _,
    answer_pairing, certify, pending_pairings,
};
use tilo_relay::rooms::PreAdmitRequest;
use tilo_relay_client::https as carrier;
use tilo_relay_protocol::certify::{
    ApproverDecision, ApproverReply, ApproverRequest, CertifyReply, MemberApproval,
    PendingCertification,
};
use tilo_relay_protocol::rooms::{JoinReply, PreAdmission, RoomRole};
use tilo_relay_protocol::web::HereRequest;
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationPolicy, CertificationRequest, DeviceCapabilities,
    DeviceCert, RevocationList, UserIdentity,
};
use tokio::task::JoinHandle;

use support::{
    Device, TestHost, enrol, next_document, ready, route, start_host, start_host_admitting,
    start_peer, start_peer_with_store, start_room_host, wait_for, workspace,
};

fn new_key() -> SigningKey {
    SigningKey::generate(&mut UnwrapErr(SysRng))
}

/// A new device asking to become `person`'s: the code it shows, and its
/// certification once answered.
struct Asking {
    key: SigningKey,
    code: tokio::sync::oneshot::Receiver<String>,
    answer: JoinHandle<Result<(DeviceCert, DeviceCert), CertifyError>>,
}

fn ask_to_pair(host: &Host, person: [u8; 32], label: &str) -> Asking {
    let key = new_key();
    let (shown, code) = tokio::sync::oneshot::channel();
    let answer = tokio::spawn({
        let host = host.clone();
        let key = key.clone();
        let label = label.to_owned();
        async move {
            certify(&host, person, &label, [0; 16], &key, |code| {
                let _ = shown.send(code.to_owned());
            })
            .await
        }
    });
    Asking { key, code, answer }
}

impl Asking {
    async fn code(&mut self) -> String {
        tilo_test_env::within("the pairing code", &mut self.code)
            .await
            .expect("the host answered with a pairing code")
    }

    async fn answer(self) -> Result<(DeviceCert, DeviceCert), CertifyError> {
        tilo_test_env::within("the certification answer", self.answer)
            .await
            .unwrap()
    }
}

/// The one request waiting for `root` at the host, polled as the "Pair a
/// new device" menu polls it.
async fn waiting_for(https: &dyn HttpClient, root: &SigningKey) -> PendingCertification {
    wait_for("the request to wait for its root", || async {
        let mut pending = pending_pairings(https, root).await.unwrap();
        assert!(pending.len() <= 1, "{pending:?}");
        pending.pop()
    })
    .await
}

/// `request` signed by `root`, as a page holding it signs approver commands.
fn approval(root: &SigningKey, request: ApproverRequest) -> MemberApproval {
    let user_public_key = root.verifying_key().to_bytes();
    let issued_at_ns = tilo_clock::unix_now_ns();
    let message = MemberApproval::signed_message(&user_public_key, issued_at_ns, &request).unwrap();
    MemberApproval {
        user_public_key,
        issued_at_ns,
        request,
        signature: root.sign(&message).to_bytes(),
    }
}

async fn approve(https: &dyn HttpClient, approval: &MemberApproval) -> ApproverReply {
    carrier::certify_approve(https, approval).await.unwrap()
}

fn refused(reply: &ApproverReply) -> bool {
    matches!(reply, ApproverReply::Refused { .. })
}

/// Who `peer` sees in `workspace_id` after it says it is here.
async fn here(peer: &PeerHandle, workspace_id: u64, label: &str) -> Vec<([u8; 32], [u8; 32])> {
    let (send, recv) = tokio::sync::oneshot::channel();
    peer.here_then(
        HereRequest {
            workspace_id,
            label: label.into(),
            window: None,
            actions: Vec::new(),
        },
        Box::new(move |reply| {
            let _ = send.send(reply);
        }),
    )
    .unwrap();
    tilo_test_env::within("who is here", recv)
        .await
        .unwrap()
        .unwrap()
        .people
        .into_iter()
        .map(|person| (person.user, person.device))
        .collect()
}

/// Both devices say they are here; `peer` then sees two devices of one
/// person, never a second person.
async fn one_person_on_two_devices(
    workspace_id: u64,
    first: &PeerHandle,
    second: &PeerHandle,
    person: [u8; 32],
    devices: [[u8; 32]; 2],
) {
    here(first, workspace_id, "Laptop").await;
    let seen = here(second, workspace_id, "Phone").await;
    let people: BTreeSet<_> = seen.iter().map(|(user, _)| *user).collect();
    let shown: BTreeSet<_> = seen.iter().map(|(_, device)| *device).collect();
    assert_eq!(people, BTreeSet::from([person]), "{seen:?}");
    assert_eq!(shown, BTreeSet::from(devices), "{seen:?}");
}

/// Check what `certify` handed back: a browser of `person` for exactly the
/// asking device, and the host to pin.
fn assert_paired(cert: &DeviceCert, host_cert: &DeviceCert, person: [u8; 32], key: &SigningKey) {
    assert_eq!(cert.user_public_key, person);
    assert_eq!(cert.device_public_key, key.verifying_key().to_bytes());
    assert_eq!(cert.device_label, "Phone");
    assert_eq!(cert.capabilities, DeviceCapabilities::member());
    assert!(!cert.capabilities.can_admit_devices);
    assert_eq!(
        cert.expires_at_ns.unwrap().checked_sub(cert.issued_at_ns),
        Some(CertificationPolicy::browser().lifetime_ns)
    );
    tilo_user_identity::verify_device_cert(cert).unwrap();
    assert!(host_cert.capabilities.can_act_as_host);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paired_device_is_the_same_person_and_keeps_working_after_reconnecting() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let user = root.verifying_key().to_bytes();
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let laptop = enrol(&root, "laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let https = host.https();
    let page = Host::Https(https.clone());

    // The laptop page holds Ada's root and is in the workspace.
    let laptop_peer = start_peer(
        page.clone(),
        laptop.cert.clone(),
        host_device.cert.clone(),
        laptop.key.clone(),
    );
    laptop_peer.join(workspace()).unwrap();
    ready(&[&laptop_peer]).await;

    // The phone opens the pairing link: it names Ada, and nothing else.
    let mut phone = ask_to_pair(&page, user, "Phone");
    let shown = phone.code().await;
    let waiting = waiting_for(&*https, &root).await;
    assert_eq!(waiting.pairing_code, shown, "both pages show one code");
    assert_eq!(
        waiting.request.device_public_key,
        phone.key.verifying_key().to_bytes()
    );
    assert_eq!(waiting.request.device_label, "Phone");
    answer_pairing(&*https, &root, waiting.request_digest, true)
        .await
        .unwrap();
    let phone_key = phone.key.clone();
    let (cert, host_cert) = phone.answer().await.unwrap();
    assert_paired(&cert, &host_cert, user, &phone_key);
    assert_eq!(host_cert, host_device.cert);
    assert!(
        pending_pairings(&*https, &root).await.unwrap().is_empty(),
        "an answered request no longer waits"
    );

    // The phone is Ada's: it enters her workspace and reads her sealed
    // notes, which only her devices are given the keys of.
    let store = Arc::new(MemoryPeerStore::default());
    let phone_peer = start_peer_with_store(
        page.clone(),
        cert.clone(),
        host_cert.clone(),
        phone_key.clone(),
        store.clone(),
    );
    phone_peer.join(workspace()).unwrap();
    ready(&[&phone_peer]).await;
    let snapshot = DocumentMutation::Snapshot(b"written on the laptop".to_vec());
    laptop_peer.publish(route(), &snapshot).unwrap();
    let mut events = Vec::new();
    assert_eq!(next_document(&phone_peer, &mut events).await, snapshot);
    one_person_on_two_devices(
        workspace(),
        &laptop_peer,
        &phone_peer,
        user,
        [laptop.cert.device_public_key, cert.device_public_key],
    )
    .await;

    // It reconnects as the same device and still follows the workspace.
    phone_peer.reconnect().unwrap();
    ready(&[&phone_peer]).await;
    let change = DocumentMutation::Incremental(b"after the reconnect".to_vec());
    laptop_peer.publish(route(), &change).unwrap();
    assert_eq!(next_document(&phone_peer, &mut events).await, change);

    // A reload starts from the kept record: the same certificate, no new
    // pairing.
    phone_peer.close();
    assert_eq!(store.load().unwrap().cert.as_ref(), Some(&cert));
    let reloaded = start_peer_with_store(page, cert, host_cert, phone_key, store);
    reloaded.join(workspace()).unwrap();
    ready(&[&reloaded]).await;
    let later = DocumentMutation::Incremental(b"after the reload".to_vec());
    laptop_peer.publish(route(), &later).unwrap();
    let mut events = Vec::new();
    let mut seen = next_document(&reloaded, &mut events).await;
    while seen != later {
        seen = next_document(&reloaded, &mut events).await;
    }
    reloaded.close();
    laptop_peer.close();
    host.relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_declined_device_gets_no_certificate() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let user = root.verifying_key().to_bytes();
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let https = host.https();

    let mut phone = ask_to_pair(&Host::Https(https.clone()), user, "Phone");
    let shown = phone.code().await;
    let waiting = waiting_for(&*https, &root).await;
    assert_eq!(waiting.pairing_code, shown);
    answer_pairing(&*https, &root, waiting.request_digest, false)
        .await
        .unwrap();
    let answer = phone.answer().await;
    assert!(
        matches!(&answer, Err(CertifyError::Denied(reason)) if reason.contains("declined")),
        "{answer:?}"
    );
    assert!(pending_pairings(&*https, &root).await.unwrap().is_empty());
    // A decision is final: approving it afterwards finds nothing to approve.
    assert!(matches!(
        answer_pairing(&*https, &root, waiting.request_digest, true).await,
        Err(CertifyError::Denied(_))
    ));
    host.relay.shutdown().await;
}

/// A request for `person` from a new `device` that waits `wait_ns` at most.
fn short_request(person: [u8; 32], device: &SigningKey, wait_ns: u64) -> CertificationRequest {
    let now = tilo_clock::unix_now_ns();
    CertificationRequest {
        version: CERTIFICATION_SCHEMA_VERSION,
        user_public_key: person,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Phone".into(),
        requested: DeviceCapabilities::member(),
        nonce: rand::random(),
        invite: [0; 16],
        issued_at_ns: now,
        expires_at_ns: now.checked_add(wait_ns).unwrap(),
        signature: [0; 64],
    }
    .sign(device)
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_a_holder_of_the_named_root_answers_and_an_unanswered_request_expires() {
    // Alice's host serves a workspace Bob, Dave and Erin are guests of.
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let bob = new_key();
    let dave = new_key();
    let erin = new_key();
    let person = |key: &SigningKey| key.verifying_key().to_bytes();
    let state = tempfile::tempdir().unwrap();
    let host = start_host_admitting(
        &alice,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
        None,
        &[person(&bob), person(&dave), person(&erin)],
    )
    .await;
    let https = host.https();
    let page = Host::Https(https.clone());

    // Nobody here is Carol: a request naming her is refused at once, and
    // her root answers nothing here.
    let carol = new_key();
    let Asking {
        mut code, answer, ..
    } = ask_to_pair(&page, person(&carol), "Phone");
    let answer = tilo_test_env::within("the refusal", answer).await.unwrap();
    assert!(
        matches!(&answer, Err(CertifyError::Denied(_))),
        "{answer:?}"
    );
    assert!(code.try_recv().is_err(), "a refused request shows no code");
    assert!(matches!(
        pending_pairings(&*https, &carol).await,
        Err(CertifyError::Transport(LinkError::Refused(_)))
    ));

    // Bob's phone asks to be his.
    let mut phone = ask_to_pair(&page, person(&bob), "Phone");
    phone.code().await;
    let waiting = waiting_for(&*https, &bob).await;
    let digest = waiting.request_digest;

    // Alice owns the host and Erin is a member, but neither holds Bob's
    // root: neither sees his request, nor answers it, even with a
    // certificate their own root signed.
    for other in [&alice, &erin] {
        assert!(pending_pairings(&*https, other).await.unwrap().is_empty());
        assert!(matches!(
            answer_pairing(&*https, other, digest, true).await,
            Err(CertifyError::Denied(_))
        ));
        let own = tilo_user_identity::issue_device_cert(
            &waiting.request,
            other,
            &CertificationPolicy::browser(),
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        );
        assert!(own.is_err(), "a root certifies only requests naming it");
        let reply = approve(
            &*https,
            &approval(
                other,
                ApproverRequest::Decide {
                    request_digest: digest,
                    decision: ApproverDecision::Deny,
                },
            ),
        )
        .await;
        assert!(refused(&reply), "{reply:?}");
    }
    // Erin cannot answer in Bob's name either.
    let mut forged = approval(&erin, ApproverRequest::ListPending);
    forged.user_public_key = person(&bob);
    assert!(refused(&approve(&*https, &forged).await));
    // Nor Carol, who is no member here.
    assert!(matches!(
        carrier::certify_approve(
            &*https,
            &approval(
                &carol,
                ApproverRequest::Decide {
                    request_digest: digest,
                    decision: ApproverDecision::Deny,
                },
            ),
        )
        .await,
        Err(LinkError::Refused(_))
    ));

    // Bob's request still waits, and Bob answers it.
    assert_eq!(waiting_for(&*https, &bob).await, waiting);
    answer_pairing(&*https, &bob, digest, true).await.unwrap();
    let phone_key = phone.key.clone();
    let (cert, host_cert) = phone.answer().await.unwrap();
    assert_paired(&cert, &host_cert, person(&bob), &phone_key);

    // Dave's root is on no device here: a request naming him waits for
    // its lifetime and then expires, and is gone from his list. The wall
    // clock is held while it is asked and then moved past its lifetime, so
    // a slow run neither expires it early nor waits for it.
    let asked_at = tilo_clock::wall();
    tilo_clock::freeze_wall(asked_at);
    let device = new_key();
    let request = short_request(person(&dave), &device, 3_000_000_000);
    let CertifyReply::Pending { pairing_code } =
        carrier::certify_request(&*https, &request).await.unwrap()
    else {
        panic!("a member's request waits");
    };
    assert_eq!(pairing_code, request.pairing_code().unwrap());
    let digest = request.digest().unwrap();
    assert_eq!(
        carrier::certify_wait(&*https, digest).await.unwrap(),
        CertifyReply::Pending {
            pairing_code: pairing_code.clone()
        },
        "a request waits through its lifetime"
    );
    tilo_clock::freeze_wall(asked_at + std::time::Duration::from_secs(3));
    let reply = wait_for("the request to expire", || async {
        match carrier::certify_wait(&*https, digest).await.unwrap() {
            CertifyReply::Pending { pairing_code: code } => {
                assert_eq!(code, pairing_code);
                None
            }
            reply => Some(reply),
        }
    })
    .await;
    assert_eq!(reply, CertifyReply::Expired);
    assert!(pending_pairings(&*https, &dave).await.unwrap().is_empty());
    host.relay.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn approvals_for_requests_that_do_not_wait_are_refused() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let user = root.verifying_key().to_bytes();
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let https = host.https();

    // Nothing waits under a made-up digest.
    let made_up = [7; 32];
    assert!(matches!(
        answer_pairing(&*https, &root, made_up, true).await,
        Err(CertifyError::Denied(_))
    ));
    let reply = approve(
        &*https,
        &approval(
            &root,
            ApproverRequest::Decide {
                request_digest: made_up,
                decision: ApproverDecision::Deny,
            },
        ),
    )
    .await;
    assert!(refused(&reply), "{reply:?}");

    let mut phone = ask_to_pair(&Host::Https(https.clone()), user, "Phone");
    phone.code().await;
    let waiting = waiting_for(&*https, &root).await;
    let digest = waiting.request_digest;

    // A certificate for another key, or with more than was asked, is not
    // the answer to this request, even signed by the right root.
    let issue = |cert: DeviceCert| {
        approval(
            &root,
            ApproverRequest::Decide {
                request_digest: digest,
                decision: ApproverDecision::Issue(cert),
            },
        )
    };
    let honest = tilo_user_identity::issue_device_cert(
        &waiting.request,
        &root,
        &CertificationPolicy::browser(),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let mut other_key = honest.clone();
    other_key.device_public_key = new_key().verifying_key().to_bytes();
    tilo_user_identity::sign_device_cert(&mut other_key, &root).unwrap();
    let mut wider = honest.clone();
    wider.capabilities = DeviceCapabilities::primary();
    tilo_user_identity::sign_device_cert(&mut wider, &root).unwrap();
    for spoofed in [other_key, wider] {
        let reply = approve(&*https, &issue(spoofed)).await;
        assert!(refused(&reply), "{reply:?}");
    }
    assert_eq!(
        waiting_for(&*https, &root).await,
        waiting,
        "a refused answer leaves the request waiting"
    );

    // The honest answer is taken once: neither a replay of it nor a second
    // answer changes it.
    let answered = issue(honest.clone());
    assert_eq!(approve(&*https, &answered).await, ApproverReply::Decided);
    let replayed = approve(&*https, &answered).await;
    assert!(refused(&replayed), "{replayed:?}");
    let deny = approval(
        &root,
        ApproverRequest::Decide {
            request_digest: digest,
            decision: ApproverDecision::Deny,
        },
    );
    let second = approve(&*https, &deny).await;
    assert!(refused(&second), "{second:?}");
    assert!(matches!(
        answer_pairing(&*https, &root, digest, false).await,
        Err(CertifyError::Denied(_))
    ));
    let (cert, _) = phone.answer().await.unwrap();
    assert_eq!(cert, honest, "the device holds the first answer");
    host.relay.shutdown().await;
}

/// The room's one-time pre-admission for `person`, as the room service's
/// door asks the room for it: the owner's grant for a token's digest,
/// issued as an invitation only `person` redeems. Returns the invitation
/// and the token.
async fn door(
    host: &TestHost,
    owner: &SigningKey,
    owner_host: &Device,
    room: &str,
    person: [u8; 32],
) -> (Vec<u8>, Vec<u8>) {
    let token: [u8; 32] = rand::random();
    let token_sha256: [u8; 32] = Sha256::digest(token).into();
    let mut grant = PreAdmission {
        room: room.to_owned(),
        role: RoomRole::Editor,
        token_sha256,
        label: "Guest".into(),
        expires_at_ns: tilo_clock::unix_now_ns()
            .checked_add(600_000_000_000)
            .unwrap(),
        owner_user_public_key: owner.verifying_key().to_bytes(),
        signature: [0; 64],
    };
    grant.signature = owner.sign(&grant.digest().unwrap()).to_bytes();
    let issued = host
        .relay
        .rooms()
        .unwrap()
        .pre_admit(
            &owner_host.cert,
            PreAdmitRequest {
                request_id: *blake3::hash(&token_sha256).as_bytes(),
                grant,
                recipient_user_public_key: Some(person),
                inviter_name: Some("Tilo".into()),
            },
        )
        .await
        .unwrap();
    (issued.signed_invitation, token.to_vec())
}

/// Let `cert`'s device in through `door`, then start its page in the room.
async fn enter(
    host: &TestHost,
    (invitation, token): (Vec<u8>, Vec<u8>),
    cert: &DeviceCert,
    key: &SigningKey,
    host_cert: &DeviceCert,
    workspace_id: u64,
) -> PeerHandle {
    let https = host.https();
    let admitted = ask_to_join_pre_admitted(
        &*https,
        &invitation,
        cert,
        key,
        rand::random(),
        "Bob",
        &token,
    )
    .await
    .unwrap();
    assert!(
        matches!(
            &admitted,
            JoinReply::Admitted {
                role: RoomRole::Editor,
                ..
            }
        ),
        "{admitted:?}"
    );
    let peer = start_peer(
        Host::Https(https),
        cert.clone(),
        host_cert.clone(),
        key.clone(),
    );
    peer.join(workspace_id).unwrap();
    in_room(&peer, workspace_id).await;
    peer
}

async fn in_room(peer: &PeerHandle, workspace_id: u64) {
    ready(&[peer]).await;
    wait_for("the governed room to let the page in", || async {
        (peer.status().rooms.get(&workspace_id) == Some(&true)).then_some(())
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_paired_through_the_door_enters_the_room_as_the_person() {
    // Alice's host keeps a public room; its door lets guests in.
    let (alice, _) = UserIdentity::generate("Alice", 1);
    let host_device = enrol(&alice, "alice-home", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_room_host(&alice, &host_device, state.path(), false).await;
    let room = host.relay.rooms().unwrap().create("Lounge").await.unwrap();
    let https = host.https();

    // Bob came in through the door on his laptop, whose page holds his root.
    let bob = new_key();
    let user = bob.verifying_key().to_bytes();
    let laptop_key = new_key();
    let laptop_cert = guest_certificate(
        &bob,
        laptop_key.verifying_key().to_bytes(),
        "Bob's laptop",
        tilo_clock::unix_now_ns(),
    )
    .await
    .unwrap();
    let laptop = enter(
        &host,
        door(&host, &alice, &host_device, &room.room, user).await,
        &laptop_cert,
        &laptop_key,
        &host_device.cert,
        room.workspace_id,
    )
    .await;

    // His phone opens the pairing link and the laptop approves it.
    let mut phone = ask_to_pair(&Host::Https(https.clone()), user, "Phone");
    let shown = phone.code().await;
    let waiting = waiting_for(&*https, &bob).await;
    assert_eq!(waiting.pairing_code, shown);
    answer_pairing(&*https, &bob, waiting.request_digest, true)
        .await
        .unwrap();
    let phone_key = phone.key.clone();
    let (cert, host_cert) = phone.answer().await.unwrap();
    assert_paired(&cert, &host_cert, user, &phone_key);
    assert_eq!(
        host_cert, host_device.cert,
        "the host is Alice's, not Bob's"
    );

    // The door lets the phone in as Bob: one member, on two devices.
    let phone_peer = enter(
        &host,
        door(&host, &alice, &host_device, &room.room, user).await,
        &cert,
        &phone_key,
        &host_cert,
        room.workspace_id,
    )
    .await;
    let summary = host
        .relay
        .rooms()
        .unwrap()
        .summary(&room.room)
        .await
        .unwrap();
    let bobs: Vec<_> = summary
        .members
        .iter()
        .filter(|member| member.user_public_key == user)
        .collect();
    assert_eq!(bobs.len(), 1, "{:?}", summary.members);
    assert!(!bobs[0].removed);
    for device in [laptop_cert.device_public_key, cert.device_public_key] {
        assert!(
            bobs[0].active_device_keys.contains(&device),
            "{:?}",
            bobs[0]
        );
    }
    one_person_on_two_devices(
        room.workspace_id,
        &laptop,
        &phone_peer,
        user,
        [laptop_cert.device_public_key, cert.device_public_key],
    )
    .await;

    // The phone reconnects and is let in again, still as Bob.
    phone_peer.reconnect().unwrap();
    in_room(&phone_peer, room.workspace_id).await;
    phone_peer.close();
    laptop.close();
    drop(host.web);
    host.relay.shutdown().await;
}
