//! Relay-mediated device certification over `tilo-device-certify/1`: a
//! device holding only its own key is certified by a same-user device that
//! holds the root, through the host, and the host enforces request binding,
//! policy, approver capability and revocation.

use std::time::Duration;

use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use tilo_relay::RelayClient;
use tilo_relay::certify::{ApproverDecision, ApproverReply};
use tilo_relay::daemon::{Relay, RelayIdentity};
use tilo_relay_client::{Approver, CertificationOutcome, CertifyError, request_certification};
use tilo_user_identity::{
    CERTIFICATION_SCHEMA_VERSION, CertificationPolicy, CertificationRequest, DeviceCapabilities,
    RevocationEntry, RevocationList, RevocationReason, issue_device_cert, prepare_device_cert,
    sign_device_cert, sign_revocation_entry,
};

use crate::common::{TestUser, client_endpoint};

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

async fn start_relay(
    user: &TestUser,
    revocations: RevocationList,
    workspaces: &[u64],
) -> (Relay, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let host = user.enrol("home-server", DeviceCapabilities::host());
    let mut cfg = tilo_relay::RelayConfig {
        state_root: tmp.path().to_path_buf(),
        ..Default::default()
    };
    for id in workspaces {
        cfg.workspaces.push(tilo_relay::WorkspaceBinding {
            id: *id,
            pin: true,
            replication: tilo_relay::ReplicationIntent::FullWorkspace,
            admitted_users: vec![hex::encode(user.user_pk)],
            checkpoint_prefix_recovery: tilo_relay::config::CheckpointPrefixRecovery::Disabled,
            shell_authority: None,
        });
    }
    let relay = Relay::start(
        cfg,
        RelayIdentity {
            user_public_key: user.user_pk,
            device_signing_key: host.sk,
            device_cert: host.cert,
            revocations,
        },
    )
    .await
    .unwrap();
    (relay, tmp)
}

/// Wait until the host lists exactly one pending request.
async fn one_pending(approver: &mut Approver) -> tilo_relay::certify::PendingCertification {
    for _ in 0..200 {
        let mut pending = approver.list_pending().await.unwrap();
        if pending.len() == 1 {
            return pending.remove(0);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the request never reached the approver");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn root_holder_certifies_a_browser_device_through_the_host() {
    let user = TestUser::fresh("Ada");
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let (relay, _tmp) = start_relay(&user, RevocationList::new(), &[7]).await;
    let host = relay.node_addr();

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
        host_cert,
    } = waiting.await.unwrap().unwrap()
    else {
        panic!("the browser was not certified");
    };
    assert_eq!(issued, cert);
    assert!(host_cert.capabilities.can_act_as_host);
    assert!(!issued.capabilities.can_admit_devices);

    // The new certificate authenticates on the relay ALPN with a signer.
    let client = RelayClient::connect_with_signer(
        client_endpoint().await,
        host,
        Some(user.user_pk),
        Some(host_cert.device_public_key),
        issued,
        &browser_key,
        RevocationList::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        client.relay_peer.device_public_key,
        host_cert.device_public_key
    );
    approver.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_refuses_escalation_and_relays_a_denial() {
    let user = TestUser::fresh("Ada");
    let laptop = user.enrol("laptop", DeviceCapabilities::primary());
    let (relay, _tmp) = start_relay(&user, RevocationList::new(), &[]).await;
    let host = relay.node_addr();
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
    let (relay, _tmp) = start_relay(&user, revocations, &[]).await;
    let host = relay.node_addr();

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
