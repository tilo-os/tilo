use super::*;
use std::{
    io::{BufRead, BufReader, Read},
    sync::atomic::AtomicBool,
};
const ACI: &str = "11111111-1111-4111-8111-111111111111";
const URI: &str =
    "sgnl://linkdevice?uuid=22222222-2222-4222-8222-222222222222&pub_key=synthetic-private-key";
struct Grant(AtomicBool);
impl AccountAuthority for Grant {
    fn check(&self) -> Result<(), SendError> {
        if self.0.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(SendError::Revoked)
        }
    }
}
fn setup() -> (Arc<SignalLinking>, Arc<Grant>, UnixStream) {
    let (stream, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let grant = Arc::new(Grant(AtomicBool::new(true)));
    let owner = Owner {
        principal: "11".repeat(32),
        device: "synthetic-sdk-device".into(),
        service: crate::Service::Signal,
        account: "synthetic-account".into(),
        linking_generation: 1,
    };
    let linking = SignalLinking::new(owner, ACI.into(), grant.clone(), stream).unwrap();
    (linking, grant, peer)
}
fn request(reader: &mut BufReader<UnixStream>) -> serde_json::Value {
    let mut line = Zeroizing::new(String::new());
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}
fn reply(peer: &mut UnixStream, request: &serde_json::Value, result: &serde_json::Value) {
    writeln!(
        peer,
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":result})
    )
    .unwrap();
}
fn provisioning(peer: UnixStream, completed: &'static str) -> std::thread::JoinHandle<()> {
    provisioning_rows(
        peer,
        serde_json::json!([{"number":"+15555550101"}]),
        serde_json::json!([{"uuid":completed,"number":"+15555550101","trustLevel":"TRUSTED_VERIFIED","fingerprint":format!("05 {}", vec!["aa"; 32].join(" "))}]),
        None,
    )
}
fn provisioning_rows(
    mut peer: UnixStream,
    rows: serde_json::Value,
    identities: serde_json::Value,
    revoke_at_identity: Option<Arc<Grant>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let start = request(&mut reader);
        assert_eq!(start["method"], "startLink");
        assert_eq!(start["params"], serde_json::json!({}));
        reply(&mut peer, &start, &serde_json::json!({"deviceLinkUri":URI}));
        let finish = request(&mut reader);
        assert_eq!(finish["method"], "finishLink");
        assert_eq!(finish["params"]["deviceLinkUri"], URI);
        assert_eq!(finish["params"]["deviceName"], "Tilo");
        assert_ne!(start["id"], finish["id"]);
        reply(
            &mut peer,
            &finish,
            &serde_json::json!({"number":"+15555550101"}),
        );
        let accounts = request(&mut reader);
        assert_eq!(accounts["method"], "listAccounts");
        reply(&mut peer, &accounts, &rows);
        if rows
            .as_array()
            .is_some_and(|rows| rows.len() == 1 && rows[0]["number"] == "+15555550101")
        {
            let own_identity = request(&mut reader);
            assert_eq!(own_identity["method"], "listIdentities");
            assert_eq!(
                own_identity["params"],
                serde_json::json!({"account":"+15555550101","number":"+15555550101"})
            );
            if let Some(grant) = revoke_at_identity {
                grant.0.store(false, Ordering::Release);
            }
            reply(&mut peer, &own_identity, &identities);
        }
        assert_eq!(
            reader.read(&mut [0]).unwrap(),
            0,
            "only the owned setup stream closes"
        );
    })
}
fn enrollment() -> (SignalEnrollment, Arc<Grant>, UnixStream) {
    let (known, grant, _) = setup();
    let owner = known.owner().clone();
    drop(known);
    let (stream, peer) = UnixStream::pair().unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    (
        SignalEnrollment::new(owner, grant.clone(), stream).unwrap(),
        grant,
        peer,
    )
}
#[tokio::test]
async fn first_account_requires_verified_receipt_and_remains_unpublished() {
    let (enrollment, _, peer) = enrollment();
    let provider = provisioning(peer, ACI);
    assert_eq!(
        enrollment.state(),
        tilo_messaging::AccountState::Unavailable
    );
    let pairing = enrollment.begin().await.unwrap();
    assert_eq!(pairing.qr_bytes(), URI.as_bytes());
    assert!(enrollment.wake_after().is_some());
    let verified = enrollment.finish(pairing.attempt).await.unwrap();
    assert_eq!(verified.account().aci, ACI);
    assert_eq!(verified.account().number.as_deref(), Some("+15555550101"));
    assert_eq!(verified.owner().account, "synthetic-account");
    assert_eq!(enrollment.state(), tilo_messaging::AccountState::Offline);
    assert!(enrollment.wake_after().is_none());
    let (owner, account) = verified.accept().unwrap();
    assert_eq!(owner.linking_generation, 1);
    assert_eq!(account.scope(), format!("signal:{ACI}"));
    assert_eq!(enrollment.state(), tilo_messaging::AccountState::Offline);
    assert!(matches!(
        enrollment.begin().await,
        Err(SendError::AlreadyReserved)
    ));
    enrollment.cancel();
    provider.join().unwrap();
}
#[tokio::test]
async fn first_account_receipt_rechecks_original_authority_at_acceptance() {
    let (enrollment, grant, peer) = enrollment();
    let provider = provisioning(peer, ACI);
    let pairing = enrollment.begin().await.unwrap();
    let verified = enrollment.finish(pairing.attempt).await.unwrap();
    grant.0.store(false, Ordering::Release);
    assert!(matches!(verified.accept(), Err(SendError::Revoked)));
    assert_eq!(enrollment.state(), tilo_messaging::AccountState::Locked);
    provider.join().unwrap();
}
#[tokio::test]
async fn first_account_cancel_retires_an_already_completed_receipt() {
    let (enrollment, _, peer) = enrollment();
    let provider = provisioning(peer, ACI);
    let pairing = enrollment.begin().await.unwrap();
    let verified = enrollment.finish(pairing.attempt).await.unwrap();
    enrollment.cancel();
    assert!(matches!(verified.accept(), Err(SendError::AlreadyReserved)));
    provider.join().unwrap();
}
#[tokio::test]
async fn first_account_rejects_noncanonical_provider_identity() {
    let (enrollment, _, peer) = enrollment();
    let provider = provisioning(peer, "NOT-A-SIGNAL-ACI");
    let pairing = enrollment.begin().await.unwrap();
    assert!(matches!(
        enrollment.finish(pairing.attempt).await,
        Err(SendError::InvalidRequest)
    ));
    assert_eq!(
        enrollment.state(),
        tilo_messaging::AccountState::NeedsRelink
    );
    assert!(enrollment.wake_after().is_none());
    provider.join().unwrap();
}
#[tokio::test]
async fn explicit_link_matches_frozen_numberless_account_and_never_publishes_ready() {
    let (linking, _, peer) = setup();
    let daemon = provisioning(peer, ACI);
    let private = linking.begin().await.unwrap();
    assert_eq!(private.qr_bytes(), URI.as_bytes());
    assert!(!format!("{private:?}").contains("synthetic-private-key"));
    assert_eq!(linking.state(), tilo_messaging::AccountState::Linking);
    assert!(matches!(
        linking.begin().await,
        Err(SendError::AlreadyReserved)
    ));
    assert!(matches!(
        linking.finish("another-attempt".into()).await,
        Err(SendError::InvalidRequest)
    ));
    let account = linking.finish(private.attempt.clone()).await.unwrap();
    assert_eq!(account.aci, ACI);
    assert_eq!(account.number.as_deref(), Some("+15555550101"));
    assert_eq!(linking.state(), tilo_messaging::AccountState::Offline);
    assert!(matches!(
        linking.finish(private.attempt.clone()).await,
        Err(SendError::InvalidRequest)
    ));
    linking.cancel();
    daemon.join().unwrap();
}
#[tokio::test]
async fn another_primary_account_cannot_complete_the_frozen_native_binding() {
    let (linking, _, peer) = setup();
    let daemon = provisioning(peer, "33333333-3333-4333-8333-333333333333");
    let private = linking.begin().await.unwrap();
    assert!(matches!(
        linking.finish(private.attempt).await,
        Err(SendError::Revoked)
    ));
    assert_eq!(linking.state(), tilo_messaging::AccountState::NeedsRelink);
    assert!(matches!(
        linking.begin().await,
        Err(SendError::AlreadyReserved)
    ));
    daemon.join().unwrap();
}
#[tokio::test]
async fn revoked_original_grant_never_starts_provisioning() {
    let (linking, grant, mut peer) = setup();
    grant.0.store(false, Ordering::Release);
    assert!(matches!(linking.begin().await, Err(SendError::Revoked)));
    assert_eq!(linking.state(), tilo_messaging::AccountState::Locked);
    assert_eq!(peer.read(&mut [0]).unwrap(), 0);
}
#[tokio::test]
async fn revocation_after_qr_closes_only_setup_and_refuses_completion() {
    let (linking, grant, mut peer) = setup();
    let daemon = std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let start = request(&mut reader);
        reply(&mut peer, &start, &serde_json::json!({"deviceLinkUri":URI}));
        assert_eq!(
            reader.read(&mut [0]).unwrap(),
            0,
            "no finishLink after revoke"
        );
    });
    let (mut other, mut other_peer) = UnixStream::pair().unwrap();
    let private = linking.begin().await.unwrap();
    grant.0.store(false, Ordering::Release);
    assert_eq!(linking.state(), tilo_messaging::AccountState::Locked);
    assert!(matches!(
        linking.finish(private.attempt).await,
        Err(SendError::Revoked)
    ));
    other.write_all(b"x").unwrap();
    let mut observed = [0];
    other_peer.read_exact(&mut observed).unwrap();
    assert_eq!(observed, [b'x']);
    daemon.join().unwrap();
}
#[tokio::test]
async fn interrupted_finish_retires_setup_without_retrying_or_other_stream_shutdown() {
    let (linking, _, mut peer) = setup();
    let (observed, observation) = tokio::sync::oneshot::channel();
    let daemon = std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let start = request(&mut reader);
        reply(&mut peer, &start, &serde_json::json!({"deviceLinkUri":URI}));
        let finish = request(&mut reader);
        assert_eq!(finish["method"], "finishLink");
        observed.send(()).unwrap();
        assert_eq!(
            reader.read(&mut [0]).unwrap(),
            0,
            "one finish request, then cancellation"
        );
    });
    let private = linking.begin().await.unwrap();
    let worker = linking.clone();
    let job = tokio::spawn(async move { worker.finish(private.attempt).await });
    observation.await.unwrap();
    job.abort();
    assert!(matches!(job.await, Err(error) if error.is_cancelled()));
    assert_eq!(linking.state(), tilo_messaging::AccountState::NeedsRelink);
    assert!(matches!(
        linking.begin().await,
        Err(SendError::AlreadyReserved)
    ));
    daemon.join().unwrap();
}
#[tokio::test]
async fn malformed_private_uri_retires_before_any_completion_request() {
    for uri in [
        "https://example.test/link",
        "sgnl://linkdevice?uuid=bad&pub_key=x",
        "sgnl://linkdevice?uuid=22222222-2222-4222-8222-222222222222&pub_key=x&pub_key=y",
    ] {
        let (linking, _, mut peer) = setup();
        let daemon = std::thread::spawn(move || {
            let mut reader = BufReader::new(peer.try_clone().unwrap());
            let start = request(&mut reader);
            reply(&mut peer, &start, &serde_json::json!({"deviceLinkUri":uri}));
            assert_eq!(reader.read(&mut [0]).unwrap(), 0);
        });
        assert!(matches!(
            linking.begin().await,
            Err(SendError::InvalidRequest)
        ));
        assert_eq!(linking.state(), tilo_messaging::AccountState::NeedsRelink);
        daemon.join().unwrap();
    }
}

#[tokio::test]
async fn expired_private_code_never_submits_finish_and_clears_pending_material() {
    let (linking, _, mut peer) = setup();
    let daemon = std::thread::spawn(move || {
        let mut reader = BufReader::new(peer.try_clone().unwrap());
        let start = request(&mut reader);
        reply(&mut peer, &start, &serde_json::json!({"deviceLinkUri":URI}));
        assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    });
    let private = linking.begin().await.unwrap();
    let client = linking.client();
    let remaining = client.wake_after().unwrap();
    assert!(remaining > Duration::ZERO && remaining <= LINK_LIFETIME);
    linking.pending.lock().unwrap().as_mut().unwrap().expires = tilo_clock::now();
    assert_eq!(client.wake_after(), Some(Duration::ZERO));
    assert!(matches!(
        linking.finish(private.attempt).await,
        Err(SendError::InvalidRequest)
    ));
    assert_eq!(linking.state(), tilo_messaging::AccountState::NeedsRelink);
    assert!(linking.pending.lock().unwrap().is_none());
    assert_eq!(client.wake_after(), None);
    daemon.join().unwrap();
}

#[tokio::test]
async fn first_account_refuses_ambiguous_or_unrelated_local_account_evidence() {
    for rows in [
        serde_json::json!([]),
        serde_json::json!([{"aci":ACI,"number":"+15555550102"}]),
        serde_json::json!([{"aci":ACI,"number":"+15555550101"},{"aci":ACI,"number":"+15555550101"}]),
        // Stock number-only output alone is not self-ACI evidence.
        serde_json::json!([{"number":"+15555550101"}]),
    ] {
        let (enrollment, _, peer) = enrollment();
        let provider = provisioning_rows(peer, rows, serde_json::json!([]), None);
        let pairing = enrollment.begin().await.unwrap();
        assert!(matches!(
            enrollment.finish(pairing.attempt).await,
            Err(SendError::InvalidRequest)
        ));
        assert_eq!(
            enrollment.state(),
            tilo_messaging::AccountState::NeedsRelink
        );
        provider.join().unwrap();
    }
}

#[tokio::test]
async fn first_account_revocation_during_local_own_identity_probe_stops_acceptance() {
    let (enrollment, grant, peer) = enrollment();
    let provider = provisioning_rows(
        peer,
        serde_json::json!([{"number":"+15555550101"}]),
        serde_json::json!([{"uuid":ACI,"number":"+15555550101","trustLevel":"TRUSTED_VERIFIED",
            "fingerprint":format!("05 {}", vec!["aa"; 32].join(" "))}]),
        Some(grant),
    );
    let pairing = enrollment.begin().await.unwrap();
    assert!(matches!(
        enrollment.finish(pairing.attempt).await,
        Err(SendError::Revoked)
    ));
    assert_eq!(enrollment.state(), tilo_messaging::AccountState::Locked);
    provider.join().unwrap();
}
