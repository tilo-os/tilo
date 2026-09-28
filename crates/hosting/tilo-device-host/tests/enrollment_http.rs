tilo_automerge_isolate::test_worker!();

use ed25519_dalek::SigningKey;
use std::{
    fs,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tilo_peer_state::enrollment::{self, ChallengeRequest, ChallengeResponse};
use tilo_workspace_host::SessionOwner;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct Host(std::process::Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
async fn request(
    origin: &str,
    route: &str,
    body: Option<String>,
    capability: Option<&str>,
) -> (u16, serde_json::Value) {
    let address = origin.strip_prefix("http://").unwrap();
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    let wire = match body {
        None => format!("GET {route} HTTP/1.1\r\nHost: {address}\r\n\r\n"),
        Some(body) => format!(
            "POST {route} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://127.0.0.1:9998\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            capability.unwrap_or("unauthorized"),
            body.len()
        ),
    };
    socket.write_all(wire.as_bytes()).await.unwrap();
    let mut bytes = vec![];
    tokio::time::timeout(tilo_test_env::hang_bound(), socket.read_to_end(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    let response = String::from_utf8(bytes).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").unwrap();
    (
        head.split_whitespace().nth(1).unwrap().parse().unwrap(),
        serde_json::from_str(body).unwrap(),
    )
}
async fn start(state: &Path, ready: &Path) -> (Host, serde_json::Value, String) {
    let child = Command::new(env!("CARGO_BIN_EXE_tilo-device-host"))
        .arg("run")
        .arg(state)
        .arg(ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let host = Host(child);
    let deadline = tilo_test_env::hang_deadline();
    let metadata = loop {
        assert!(
            Instant::now() < deadline,
            "isolated native host did not become ready"
        );
        if let Ok(bytes) = fs::read(ready)
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && value["pid"].as_u64() == Some(u64::from(pid))
        {
            break value;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let origin = metadata["fixture_origin"].as_str().unwrap();
    wait_subscribed(origin, deadline).await;
    let capability = fs::read_to_string(state.join("enrollment-capability")).unwrap();
    assert_eq!(metadata["version"], 2);
    assert_eq!(metadata["enrollment_version"], 2);
    assert!(!metadata.to_string().contains(&capability));
    (host, metadata, capability)
}
async fn wait_subscribed(origin: &str, deadline: Instant) {
    loop {
        assert!(Instant::now() < deadline, "native peer never subscribed");
        let (code, status) = request(origin, "/status", None, None).await;
        if code == 200 && status["native_peer"] == "subscribed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
/// The host answers enrollment with a retryable 503 while its own native
/// subscription reconnects (its relay drops a subscriber that falls behind,
/// as it can while a burst of enrollments publishes). Clients retry with the
/// same identity once the peer is subscribed again; so does this fixture.
fn reconnecting(code: u16, body: &serde_json::Value) -> bool {
    code == 503
        && body["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("native peer is reconnecting"))
}
async fn enroll(origin: &str, route: &str, body: String, cap: &str) -> (u16, serde_json::Value) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let (code, response) = request(origin, route, Some(body.clone()), Some(cap)).await;
        if !reconnecting(code, &response) {
            return (code, response);
        }
        wait_subscribed(origin, deadline).await;
    }
}
async fn proof(origin: &str, cap: &str, key: &SigningKey, id: u8) -> enrollment::CompleteRequest {
    let request_body = serde_json::to_string(&ChallengeRequest {
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        request_id: hex::encode([id; 32]),
    })
    .unwrap();
    let (code, response) = enroll(origin, "/enrollment/challenge", request_body, cap).await;
    assert_eq!(code, 200, "{response}");
    let challenge: ChallengeResponse = serde_json::from_value(response).unwrap();
    enrollment::sign(&enrollment::decode(&challenge.challenge).unwrap(), key).unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_http_enrollment_is_authorized_serialized_recoverable_and_public_only() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("owner");
    let ready = temp.path().join("ready.json");
    drop(SessionOwner::initialize(&state).unwrap());
    let (host, metadata, cap) = start(&state, &ready).await;
    let origin = metadata["fixture_origin"].as_str().unwrap().to_owned();
    let (code, _) = request(&origin, "/enrollment/challenge", Some("{}".into()), None).await;
    assert_eq!(code, 403);
    let mut tasks = vec![];
    for n in 1..=8u8 {
        let origin = origin.clone();
        let cap = cap.clone();
        tasks.push(tokio::spawn(async move {
            let key = SigningKey::from_bytes(&[n; 32]);
            let complete = proof(&origin, &cap, &key, n).await;
            let (code, response) = enroll(
                &origin,
                "/enrollment/complete",
                serde_json::to_string(&complete).unwrap(),
                &cap,
            )
            .await;
            assert_eq!(code, 200, "{response}");
            assert!(response.get("device_secret").is_none());
            let certificate: tilo_user_identity::DeviceCert =
                serde_json::from_value(response["certificate"].clone()).unwrap();
            assert_eq!(
                certificate.device_public_key,
                key.verifying_key().to_bytes()
            );
            certificate
        }));
    }
    let mut certificates = vec![];
    for task in tasks {
        certificates.push(task.await.unwrap());
    }
    let unique = certificates
        .iter()
        .map(|cert| cert.device_public_key)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(unique.len(), 8);
    let key = SigningKey::from_bytes(&[91; 32]);
    let membership = state
        .join("relay/workspaces")
        .join(metadata["workspace_id"].as_str().unwrap())
        .join(tilo_relay::store::GOVERNED_ACCESS_FILE);
    let saved = membership.with_extension("saved");
    // Break membership only while the peer is subscribed, so the completion
    // reaches reconciliation. A reconnect that is still settling from the
    // burst above answers before any commit; repair, resubscribe and retry.
    let deadline = tilo_test_env::hang_deadline();
    let (code, body) = loop {
        let complete = proof(&origin, &cap, &key, 91).await;
        fs::rename(&membership, &saved).unwrap();
        fs::create_dir(&membership).unwrap();
        let (code, body) = request(
            &origin,
            "/enrollment/complete",
            Some(serde_json::to_string(&complete).unwrap()),
            Some(&cap),
        )
        .await;
        if !reconnecting(code, &body) {
            break (code, body);
        }
        fs::remove_dir(&membership).unwrap();
        fs::rename(&saved, &membership).unwrap();
        wait_subscribed(&origin, deadline).await;
    };
    assert_eq!(code, 503, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("membership reconciliation"),
        "{body}"
    );
    assert!(body.get("certificate").is_none());
    fs::remove_dir(&membership).unwrap();
    fs::rename(saved, &membership).unwrap();
    // Invalid membership can correctly disconnect the native subscription. File
    // repair is not Ready: wait for the same observed readiness as initial start.
    wait_subscribed(&origin, tilo_test_env::hang_deadline()).await;
    let fresh = proof(&origin, &cap, &key, 91).await;
    let (code, recovered) = enroll(
        &origin,
        "/enrollment/complete",
        serde_json::to_string(&fresh).unwrap(),
        &cap,
    )
    .await;
    assert_eq!(code, 200, "{recovered}");
    let (code, _) = enroll(
        &origin,
        "/enrollment/complete",
        serde_json::to_string(&fresh).unwrap(),
        &cap,
    )
    .await;
    assert_eq!(code, 409);
    drop(host);
    let owner = SessionOwner::open(&state).unwrap();
    assert_eq!(owner.enrollment_count(), 9);
    let room = owner.peer().room().to_owned();
    drop(owner);
    let (host, current, current_cap) = start(&state, &ready).await;
    assert_eq!(cap, current_cap);
    assert_eq!(current["room"], room);
    let origin = current["fixture_origin"].as_str().unwrap();
    let refresh = proof(origin, &current_cap, &key, 91).await;
    let (code, updated) = enroll(
        origin,
        "/enrollment/complete",
        serde_json::to_string(&refresh).unwrap(),
        &current_cap,
    )
    .await;
    assert_eq!(code, 200, "{updated}");
    assert_eq!(updated["certificate"], recovered["certificate"]);
    assert_eq!(updated["packet_relay"], current["packet_relay"]);
    assert_eq!(updated["relay_address"], current["relay_address"]);
    drop(host);
}
