//! Server-mediated device certification over `tilo-device-certify/1`: a
//! device holding only its own key is certified by a same-user device that
//! holds the root, through the host, and the host enforces request binding,
//! policy, approver capability and revocation.

use std::time::Duration;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_server::ServerClient;
use tilo_server::certify::{ApproverDecision, ApproverReply};
use tilo_server::daemon::{Server, ServerIdentity};
use tilo_server_client::{Approver, CertificationOutcome, CertifyError, request_certification};
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationPolicy, CertificationRequest, DeviceCapabilities,
    RevocationEntry, RevocationList, RevocationReason, issue_device_cert, prepare_device_cert,
    sign_device_cert, sign_revocation_entry,
};

use crate::common::{TestDevice, TestUser, client_endpoint};

fn browser_request(user: [u8; 32], device: &SigningKey) -> CertificationRequest {
    let now = tilo_clock::unix_now_ns();
    CertificationRequest {
        version: CERTIFICATION_SCHEMA_VERSION,
        user_public_key: user,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Browser".into(),
        requested: DeviceCapabilities::member(),
        nonce: rand::random(),
        invite: [0; 16],
        issued_at_ns: now.saturating_sub(1_000_000_000),
        expires_at_ns: now.saturating_add(60_000_000_000),
        signature: [0; 64],
    }
    .sign(device)
    .unwrap()
}

async fn start_server(user: &TestUser, revocations: RevocationList) -> (Server, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let host = user.enrol("home-server", DeviceCapabilities::member());
    let cfg = tilo_server::ServerConfig::loopback(tmp.path().to_path_buf());
    let server = crate::common::start_server(
        cfg,
        ServerIdentity {
            user_public_key: user.user_pk,
            device_signing_key: host.sk,
            device_cert: host.cert,
            revocations,
        },
    )
    .await
    .unwrap();
    (server, tmp)
}

/// Wait until the host lists exactly one pending request.
async fn one_pending(approver: &mut Approver) -> tilo_server::certify::PendingCertification {
    for _ in 0..200 {
        let mut pending = approver.list_pending().await.unwrap();
        if pending.len() == 1 {
            return pending.remove(0);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the request never reached the approver");
}

/// `device`'s session with `server` as an approver of its person's requests,
/// and the endpoint that session lives on.
async fn approver_at(server: &Server, device: &TestDevice) -> (iroh::endpoint::Endpoint, Approver) {
    let endpoint = client_endpoint().await;
    let approver = Approver::connect(
        &endpoint,
        server.node_addr(),
        device.cert.clone(),
        &device.sk,
        &RevocationList::new(),
    )
    .await
    .unwrap();
    (endpoint, approver)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_holder_certifies_a_browser_device_through_the_host() {
    let user = TestUser::fresh("Ada");
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let (server, _tmp) = start_server(&user, RevocationList::new()).await;
    let host = server.node_addr();

    let browser_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let request = browser_request(user.user_pk, &browser_key);
    let (code_tx, code_rx) = tokio::sync::oneshot::channel();
    let browser_endpoint = client_endpoint().await;
    let waiting = tokio::spawn({
        let host = host.clone();
        let request = request.clone();
        async move {
            request_certification(&browser_endpoint, host, request, |code| {
                let _ = code_tx.send(code.to_string());
            })
            .await
        }
    });
    let shown_in_browser = code_rx.await.unwrap();

    let approver_endpoint = client_endpoint().await;
    let mut approver = Approver::connect(
        &approver_endpoint,
        host.clone(),
        laptop.cert.clone(),
        &laptop.sk,
        &RevocationList::new(),
    )
    .await
    .unwrap();
    let pending = one_pending(&mut approver).await;
    assert_eq!(pending.pairing_code, shown_in_browser);
    assert_eq!(pending.request, request);

    let cert = issue_device_cert(
        &pending.request,
        &user.user_sk,
        &CertificationPolicy::browser(),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    assert_eq!(
        approver
            .decide(
                pending.request_digest,
                ApproverDecision::Issue(cert.clone())
            )
            .await
            .unwrap(),
        ApproverReply::Decided
    );
    let CertificationOutcome::Issued {
        cert: issued,
        server_cert,
        label,
    } = waiting.await.unwrap().unwrap()
    else {
        panic!("the browser was not certified");
    };
    assert_eq!(label, "Browser", "the host keeps the name it was asked for");
    assert_eq!(issued, cert);
    assert!(server_cert.capabilities.can_own_workspaces);
    assert!(!issued.capabilities.can_admit_devices);

    // The new certificate authenticates on the server ALPN with a signer.
    let client = ServerClient::connect_with_signer(
        client_endpoint().await,
        host,
        Some(user.user_pk),
        Some(server_cert.device_public_key),
        tilo_server_session::ClientCredentials {
            cert: issued,
            signer: &browser_key,
            revocations: RevocationList::new(),
            entering: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        client.server_peer.device_public_key,
        server_cert.device_public_key
    );
    approver.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_devices_asking_under_one_name_are_named_apart() {
    let user = TestUser::fresh("Ada");
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let (server, _tmp) = start_server(&user, RevocationList::new()).await;
    let host = server.node_addr();
    let approver_endpoint = client_endpoint().await;
    let mut approver = Approver::connect(
        &approver_endpoint,
        host.clone(),
        laptop.cert.clone(),
        &laptop.sk,
        &RevocationList::new(),
    )
    .await
    .unwrap();
    let mut named = Vec::new();
    for _ in 0..2 {
        let browser_key = SigningKey::generate(&mut UnwrapErr(SysRng));
        let request = browser_request(user.user_pk, &browser_key);
        let browser_endpoint = client_endpoint().await;
        let waiting = tokio::spawn({
            let host = host.clone();
            async move { request_certification(&browser_endpoint, host, request, |_| {}).await }
        });
        let pending = one_pending(&mut approver).await;
        let cert = issue_device_cert(
            &pending.request,
            &user.user_sk,
            &CertificationPolicy::browser(),
            &RevocationList::new(),
            tilo_clock::unix_now_ns(),
        )
        .unwrap();
        approver
            .decide(pending.request_digest, ApproverDecision::Issue(cert))
            .await
            .unwrap();
        let CertificationOutcome::Issued { label, .. } = waiting.await.unwrap().unwrap() else {
            panic!("the browser was not certified");
        };
        named.push(label);
    }
    assert_eq!(
        named,
        vec!["Browser".to_owned(), "Browser 2".to_owned()],
        "the host answers each device the name it gave it, two devices set up alike apart"
    );
    approver.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_refuses_escalation_and_relays_a_denial() {
    let user = TestUser::fresh("Ada");
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let (server, _tmp) = start_server(&user, RevocationList::new()).await;
    let host = server.node_addr();
    let browser_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let request = browser_request(user.user_pk, &browser_key);
    let browser_endpoint = client_endpoint().await;
    let waiting = tokio::spawn({
        let host = host.clone();
        async move { request_certification(&browser_endpoint, host, request, |_| {}).await }
    });

    let approver_endpoint = client_endpoint().await;
    let mut approver = Approver::connect(
        &approver_endpoint,
        host,
        laptop.cert.clone(),
        &laptop.sk,
        &RevocationList::new(),
    )
    .await
    .unwrap();
    let pending = one_pending(&mut approver).await;
    let mut escalated = prepare_device_cert(
        &pending.request,
        user.user_pk,
        &CertificationPolicy::browser(),
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    escalated.capabilities = DeviceCapabilities::primary();
    sign_device_cert(&mut escalated, &user.user_sk).unwrap();
    assert!(matches!(
        approver
            .decide(pending.request_digest, ApproverDecision::Issue(escalated))
            .await
            .unwrap(),
        ApproverReply::Refused { .. }
    ));
    // Still pending after the refused decision; a denial ends it.
    let pending = one_pending(&mut approver).await;
    assert_eq!(
        approver
            .decide(pending.request_digest, ApproverDecision::Deny)
            .await
            .unwrap(),
        ApproverReply::Decided
    );
    assert!(matches!(
        waiting.await.unwrap().unwrap(),
        CertificationOutcome::Denied(_)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_root_holders_approve_and_revoked_keys_are_refused() {
    let user = TestUser::fresh("Ada");
    let phone = user.enrol("phone", DeviceCapabilities::member());
    let browser_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let mut entry = RevocationEntry {
        user_public_key: user.user_pk,
        revoked_device_public_key: browser_key.verifying_key().to_bytes(),
        revoked_at_ns: 1,
        reason: RevocationReason::Stolen,
        signature: [0; 64],
    };
    sign_revocation_entry(&mut entry, &user.user_sk).unwrap();
    let mut revocations = RevocationList::new();
    revocations.insert(entry).unwrap();
    let (server, _tmp) = start_server(&user, revocations).await;
    let host = server.node_addr();

    let approver_endpoint = client_endpoint().await;
    let refused: Result<Vec<_>, CertifyError> = async {
        let mut approver = Approver::connect(
            &approver_endpoint,
            host.clone(),
            phone.cert.clone(),
            &phone.sk,
            &RevocationList::new(),
        )
        .await?;
        approver.list_pending().await
    }
    .await;
    assert!(refused.is_err(), "a member device must not approve");

    let outcome = request_certification(
        &client_endpoint().await,
        host,
        browser_request(user.user_pk, &browser_key),
        |_| panic!("a revoked key must not be queued"),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, CertificationOutcome::Denied(_)));
}

/// A request as a device whose clock reads `device_now_ns` dates it: issued
/// at that reading, as `tilo-server-client` signs one, and answering `invite`.
fn request_dated(
    user: [u8; 32],
    device: &SigningKey,
    device_now_ns: u64,
    invite: [u8; 16],
) -> CertificationRequest {
    CertificationRequest {
        version: CERTIFICATION_SCHEMA_VERSION,
        user_public_key: user,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Browser".into(),
        requested: DeviceCapabilities::member(),
        nonce: rand::random(),
        invite,
        issued_at_ns: device_now_ns,
        expires_at_ns: device_now_ns.saturating_add(60_000_000_000),
        signature: [0; 64],
    }
    .sign(device)
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn devices_whose_clocks_run_ahead_of_the_hosts_pair_and_their_link_works_once() {
    // Further ahead than anything here takes to arrive, so the host's clock
    // is behind every date it reads. 20 ms between a laptop and the machine
    // beside it did this to half of all pairings.
    const AHEAD_NS: u64 = 2_000_000_000;
    let invite = [9; 16];
    let user = TestUser::fresh("Ada");
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let (server, _tmp) = start_server(&user, RevocationList::new()).await;
    let host = server.node_addr();

    let browser_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let request = request_dated(
        user.user_pk,
        &browser_key,
        tilo_clock::unix_now_ns().saturating_add(AHEAD_NS),
        invite,
    );
    let (code_tx, code_rx) = tokio::sync::oneshot::channel();
    let browser_endpoint = client_endpoint().await;
    let waiting = tokio::spawn({
        let host = host.clone();
        let request = request.clone();
        async move {
            request_certification(&browser_endpoint, host, request, |code| {
                let _ = code_tx.send(code.to_string());
            })
            .await
        }
    });
    let Ok(shown_in_browser) = code_rx.await else {
        panic!(
            "the host did not queue a request dated two seconds ahead of its clock: {:?}",
            waiting.await.unwrap()
        );
    };

    let (_approver_endpoint, mut approver) = approver_at(&server, &laptop).await;
    let pending = one_pending(&mut approver).await;
    assert_eq!(pending.pairing_code, shown_in_browser);
    assert_eq!(pending.request, request);

    // The approving device's clock runs ahead of the host's too.
    let cert = issue_device_cert(
        &pending.request,
        &user.user_sk,
        &CertificationPolicy::browser(),
        &RevocationList::new(),
        tilo_clock::unix_now_ns().saturating_add(AHEAD_NS),
    )
    .unwrap();
    assert_eq!(
        approver
            .decide(
                pending.request_digest,
                ApproverDecision::Issue(cert.clone())
            )
            .await
            .unwrap(),
        ApproverReply::Decided,
        "the host reads the certificate by its own clock, which is behind the approver's"
    );
    let (issued, server_cert) = match waiting.await.unwrap().unwrap() {
        CertificationOutcome::Issued {
            cert, server_cert, ..
        } => (cert, server_cert),
        other => panic!("the browser was not certified: {other:?}"),
    };
    assert_eq!(issued, cert);

    // The host lets the new device in with it at once: its handshake reads
    // the certificate by the host's own clock, exactly.
    let client = ServerClient::connect_with_signer(
        client_endpoint().await,
        host.clone(),
        Some(user.user_pk),
        Some(server_cert.device_public_key),
        tilo_server_session::ClientCredentials {
            cert: issued,
            signer: &browser_key,
            revocations: RevocationList::new(),
            entering: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        client.server_peer.device_public_key,
        server_cert.device_public_key
    );

    // The allowance lets no link work twice: a second device that answers
    // the same invite, its clock ahead as well, is turned away.
    let second_key = SigningKey::generate(&mut UnwrapErr(SysRng));
    let second = request_dated(
        user.user_pk,
        &second_key,
        tilo_clock::unix_now_ns().saturating_add(AHEAD_NS),
        invite,
    );
    let second_endpoint = client_endpoint().await;
    assert_eq!(
        request_certification(&second_endpoint, host, second, |_| {})
            .await
            .unwrap(),
        CertificationOutcome::Denied("this pairing link was used already; make a new one".into())
    );
    approver.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_dated_far_ahead_is_refused_for_its_clock_and_leaves_its_link_unspent() {
    let invite = [3; 16];
    let user = TestUser::fresh("Ada");
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let (server, _tmp) = start_server(&user, RevocationList::new()).await;
    let host = server.node_addr();
    let browser_key = SigningKey::generate(&mut UnwrapErr(SysRng));

    // Five minutes ahead: no clock that keeps time is that far from another.
    let far = request_dated(
        user.user_pk,
        &browser_key,
        tilo_clock::unix_now_ns().saturating_add(5 * 60 * 1_000_000_000),
        invite,
    );
    let far_endpoint = client_endpoint().await;
    let reason = match request_certification(&far_endpoint, host.clone(), far, |_| {})
        .await
        .unwrap()
    {
        CertificationOutcome::Denied(reason) => reason,
        other => panic!("a request dated five minutes ahead was taken: {other:?}"),
    };
    assert_eq!(
        reason,
        tilo_user_identity::CertificationError::NotYetValid.to_string(),
        "the refusal is about the request's date"
    );
    assert!(
        reason.contains("the clocks of the two devices differ"),
        "it names the clocks, never a link that expired: {reason}"
    );

    // Nothing was spent: dated by a clock that is right, the same link is
    // queued and waits for the person.
    let again = request_dated(
        user.user_pk,
        &browser_key,
        tilo_clock::unix_now_ns(),
        invite,
    );
    let browser_endpoint = client_endpoint().await;
    let waiting = tokio::spawn({
        let again = again.clone();
        async move { request_certification(&browser_endpoint, host, again, |_| {}).await }
    });
    let (_approver_endpoint, mut approver) = approver_at(&server, &laptop).await;
    let pending = one_pending(&mut approver).await;
    assert_eq!(pending.request, again);
    assert_eq!(
        approver
            .decide(pending.request_digest, ApproverDecision::Deny)
            .await
            .unwrap(),
        ApproverReply::Decided
    );
    assert_eq!(
        waiting.await.unwrap().unwrap(),
        CertificationOutcome::Denied("declined by the approving device".into())
    );
    approver.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_dates_its_request_a_minute_back_and_waits_as_long_as_before() {
    // A host from before hosts allowed the clocks of a pairing to differ
    // checks a request's date exactly, and refused one dated now whenever its
    // own clock ran a little behind the device's. Such hosts stay in use
    // while their pages move on, so the device does its part too.
    let user = TestUser::fresh("Ada");
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let (server, _tmp) = start_server(&user, RevocationList::new()).await;
    let host = server.node_addr();
    let device = SigningKey::generate(&mut UnwrapErr(SysRng));

    let before = tilo_clock::unix_now_ns();
    let asking = tokio::spawn({
        let host = tilo_server_client::ServerAddress::at(host.clone());
        let device = device.clone();
        let person = user.user_pk;
        async move {
            tilo_server_client::certify::certify(&host, person, "Phone", [0; 16], &device, |_| {})
                .await
        }
    });
    let approver_endpoint = client_endpoint().await;
    let mut approver = Approver::connect(
        &approver_endpoint,
        host,
        laptop.cert.clone(),
        &laptop.sk,
        &RevocationList::new(),
    )
    .await
    .unwrap();
    let pending = one_pending(&mut approver).await;
    let after = tilo_clock::unix_now_ns();
    let request = &pending.request;
    assert_eq!(request.device_public_key, device.verifying_key().to_bytes());

    let allowance = tilo_user_identity::CERTIFICATION_CLOCK_ALLOWANCE_NS;
    assert!(
        request.issued_at_ns <= after.saturating_sub(allowance),
        "the request is dated {} ns before the host read it, less than the allowance",
        after.saturating_sub(request.issued_at_ns)
    );
    assert!(
        request.issued_at_ns >= before.saturating_sub(allowance),
        "and no further back than the allowance"
    );
    // The wait for an approver is counted from the device's clock, as before.
    let wait = tilo_server_client::CERTIFICATION_WAIT_NS;
    assert!(
        request.expires_at_ns >= before.saturating_add(wait)
            && request.expires_at_ns <= after.saturating_add(wait),
        "the request waits five minutes from when it was signed"
    );

    assert_eq!(
        approver
            .decide(pending.request_digest, ApproverDecision::Deny)
            .await
            .unwrap(),
        ApproverReply::Decided
    );
    assert!(matches!(
        asking.await.unwrap(),
        Err(CertifyError::Denied(_))
    ));
    approver.close();
}
