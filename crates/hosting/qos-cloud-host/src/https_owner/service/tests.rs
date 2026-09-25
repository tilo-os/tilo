//! Real loopback: the actual `serve-https` startup over an enrolled Cloud
//! Store, a Rust client speaking the frozen delivery-v3 wire over TLS, and an
//! owner reopen after clean shutdown.
use crate::delivery::admission::tests::{browser_delta, document_text};
use ed25519_dalek::{Signer, SigningKey};
use qos_relay::delivery_v3::{
    auth,
    capabilities::PRODUCT_DATA_V1,
    cursor::CursorToken,
    descriptor::{self, DescriptorAuthority, GenerationExpectation, VerifiedDescriptor},
    framing::{self, MessageType},
    http_proof,
    media_wire::{self, BootstrapRequest, BootstrapResponse},
    private_wire::{self, PrivateOperation, PrivateScope},
    state::{
        AdmissionOutcome, CommitBoundary, CursorAck, FlowCredit, SnapshotChunk, SnapshotManifest,
        SnapshotOperation, SnapshotRequest, StateAdmission, StateItemChunk, encode_cursor_url,
    },
};
use qos_relay::peer_actions::{ActionMessage, DirectedAction};
use qos_user_identity::{DeviceCert, RevocationList};
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};

const MACHINE: &str = "https-owner";

mod live;

/// Client-side roster authority. The server's own authority checks are the
/// subject under test; the client only pins the enrolled host certificate.
struct Pinned;
impl auth::Authority for Pinned {
    fn authorize(
        &self,
        _: &auth::Binding,
        _: &DeviceCert,
        _: &DeviceCert,
        _: u64,
    ) -> Result<(), auth::AuthError> {
        Ok(())
    }
}

struct Client {
    http: reqwest::Client,
    origin: String,
    key: SigningKey,
    session: Option<([u8; 16], [u8; 32], u64)>,
}
impl Client {
    async fn send(
        &self,
        method: reqwest::Method,
        target: &str,
        body: Vec<u8>,
    ) -> (reqwest::StatusCode, Vec<u8>) {
        let mut request = self
            .http
            .request(method.clone(), format!("{}{target}", self.origin))
            .body(body.clone());
        if let Some((id, digest, _)) = self.session {
            let now = qos_relay::clock::now_ns();
            let exact = http_proof::Request::new(method.as_str(), target, &body).unwrap();
            let proof = http_proof::RequestProof::sign(
                id,
                digest,
                &exact,
                now,
                now + 30_000_000_000,
                &self.key,
            )
            .unwrap();
            request = request.header(
                "x-tilo-delivery-proof",
                hex::encode(proof.encode().unwrap()),
            );
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        (status, response.bytes().await.unwrap().to_vec())
    }
    async fn post(&self, target: &str, body: Vec<u8>) -> (reqwest::StatusCode, Vec<u8>) {
        self.send(reqwest::Method::POST, target, body).await
    }
    async fn get(&self, target: &str) -> (reqwest::StatusCode, Vec<u8>) {
        self.send(reqwest::Method::GET, target, Vec::new()).await
    }
}

fn peer(root: &Path) -> qos_peer_state::StatePeer {
    crate::Store::open(root, MACHINE)
        .unwrap()
        .peer_at(qos_relay::clock::now_ns())
        .unwrap()
}
fn authority<'a>(
    peer: &'a qos_peer_state::StatePeer,
    generation: GenerationExpectation,
) -> DescriptorAuthority<'a> {
    DescriptorAuthority {
        peer,
        pinned_governor: peer.authority(),
        pinned_host: peer.authority(),
        workspace: qos_relay::room_workspace_id(peer.room()).unwrap(),
        generation,
        allowed_delivery_capabilities: PRODUCT_DATA_V1,
        now: web_time::SystemTime::now(),
    }
}
fn device_cert(peer: &qos_peer_state::StatePeer, key: &SigningKey) -> DeviceCert {
    peer.certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == key.verifying_key().to_bytes())
        .unwrap()
}
fn framed(kind: MessageType, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    framing::append_frame(&mut out, kind, payload, 1024 * 1024).unwrap();
    out
}
fn frames(mut bytes: &[u8]) -> Vec<(MessageType, Vec<u8>)> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let (kind, payload, rest) = framing::decode_frame_prefix(bytes).unwrap();
        out.push((kind, payload.to_vec()));
        bytes = rest;
    }
    out
}
fn chunk(delivery: [u8; 16], canonical: &[u8], offset: usize, end: usize) -> Vec<u8> {
    framed(
        MessageType::StateItemChunk,
        &StateItemChunk::new(
            delivery,
            *blake3::hash(canonical).as_bytes(),
            canonical.len() as u64,
            offset as u64,
            &canonical[offset..end],
        )
        .unwrap()
        .encode()
        .unwrap(),
    )
}
fn accepted(bytes: &[u8]) -> qos_relay::delivery_v3::commit::CommitRef {
    let frames = frames(bytes);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, MessageType::StateAdmission);
    match StateAdmission::decode(&frames[0].1).unwrap().outcome() {
        AdmissionOutcome::Accepted { committed, .. } => committed.clone(),
        other => panic!("native admission required: {other:?}"),
    }
}
/// Poll one page; returns its boundary when a committed group was returned.
async fn poll(client: &Client, cursor: Option<&CursorToken>) -> Option<CommitBoundary> {
    let target = match cursor {
        Some(cursor) => format!(
            "/peer/v3/events?cursor={}&mode=poll",
            encode_cursor_url(cursor.as_bytes()).unwrap()
        ),
        None => "/peer/v3/events?mode=poll".into(),
    };
    let (status, body) = client.get(&target).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    if body.is_empty() {
        return None;
    }
    let frames = frames(&body);
    let (kind, boundary) = frames.last().unwrap();
    assert_eq!(*kind, MessageType::CommitBoundary);
    assert!(
        frames[..frames.len() - 1]
            .iter()
            .all(|(kind, _)| *kind == MessageType::EventChunk)
    );
    Some(CommitBoundary::decode(boundary).unwrap())
}

async fn handshake(client: &mut Client, root: &Path) -> VerifiedDescriptor {
    handshake_with(client, root, PRODUCT_DATA_V1).await
}
/// `capabilities` requested and mandatory; zero is a media-only session.
async fn handshake_with(client: &mut Client, root: &Path, capabilities: u64) -> VerifiedDescriptor {
    handshake_negotiating(
        client,
        root,
        (capabilities, capabilities),
        auth::CapabilityPolicy {
            supported: PRODUCT_DATA_V1,
            allowed: PRODUCT_DATA_V1,
        },
        capabilities,
    )
    .await
}
/// Explicit `(requested, mandatory)`, client policy and expected negotiation.
async fn handshake_negotiating(
    client: &mut Client,
    root: &Path,
    (capabilities, mandatory): (u64, u64),
    policy: auth::CapabilityPolicy,
    expected: u64,
) -> VerifiedDescriptor {
    let (status, hint) = client.get("/peer/v3/descriptor").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let hint: qos_peer_state::join::TransportHint = postcard::from_bytes(&hint).unwrap();
    let peer = peer(root);
    let first = authority(
        &peer,
        GenerationExpectation::FirstAccepted {
            minimum_generation: 1,
        },
    );
    let descriptor = descriptor::verify_transport_hint(&hint, &first).unwrap();
    assert_eq!(descriptor.delivery_capabilities(), PRODUCT_DATA_V1);
    let device = device_cert(&peer, &client.key);
    // Governor-signed first contact over the exact candidate descriptor.
    let mut request = BootstrapRequest {
        version: 1,
        device: device.device_public_key,
        nonce: media_wire::fresh_nonce().unwrap(),
        workspace: qos_relay::room_workspace_id(peer.room()).unwrap(),
        root: peer.authority().user_public_key,
        room: peer.room().to_owned(),
        candidate_digest: descriptor.descriptor_digest(),
        signature: [[0; 32]; 2],
    };
    request.signature = media_wire::split_signature(
        client
            .key
            .sign(&request.signing_digest().unwrap())
            .to_bytes(),
    );
    let (status, body) = client
        .post("/peer/v3/bootstrap", request.encode().unwrap())
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let response = BootstrapResponse::decode(&body).unwrap();
    assert_eq!(response.request, request);
    assert_eq!(response.descriptor_digest, descriptor.descriptor_digest());
    ed25519_dalek::VerifyingKey::from_bytes(&peer.authority().device_public_key)
        .unwrap()
        .verify_strict(
            &response.signing_digest().unwrap(),
            &ed25519_dalek::Signature::from_bytes(&media_wire::join_signature(response.signature)),
        )
        .unwrap();
    // Mutual authentication requiring every product data capability.
    let nonce = media_wire::fresh_nonce().unwrap();
    let body = postcard::to_stdvec(&(
        3_u16,
        device.device_public_key,
        nonce,
        capabilities,
        mandatory,
    ))
    .unwrap();
    let (status, body) = client.post("/peer/v3/challenge", body).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let proof = auth::ServerProof::decode(&body).unwrap();
    let revocations = RevocationList::new();
    let current = authority(
        &peer,
        GenerationExpectation::Current {
            generation: descriptor.generation(),
            descriptor_digest: descriptor.descriptor_digest(),
        },
    );
    let verified = auth::verify_server(
        &auth::ClientExpectations {
            scope: descriptor
                .client_scope(
                    &current,
                    &device,
                    auth::Carrier::Https,
                    capabilities,
                    mandatory,
                )
                .unwrap(),
            capabilities: policy,
            credentials: auth::Credentials {
                device: &device,
                host: peer.authority(),
                device_revocations: &revocations,
                host_revocations: &revocations,
            },
            authority: &Pinned,
            now_ns: qos_relay::clock::now_ns(),
        },
        nonce,
        &proof,
    )
    .unwrap();
    let transcript = &proof.transcript;
    assert_eq!(transcript.negotiated_capabilities, expected);
    let digest = http_proof::session_binding_digest(
        &transcript.binding,
        transcript.session_id,
        transcript.negotiated_capabilities,
        transcript.expires_at_ns,
    )
    .unwrap();
    let client_proof = verified
        .client_proof(&client.key)
        .unwrap()
        .encode()
        .unwrap();
    let (status, body) = client.post("/peer/v3/sessions", client_proof).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let ready: (u16, [u8; 16], [u8; 32], u64, u64) = postcard::from_bytes(&body).unwrap();
    assert_eq!(
        ready,
        (
            3,
            transcript.session_id,
            digest,
            transcript.expires_at_ns,
            expected
        )
    );
    client.session = Some((transcript.session_id, digest, transcript.expires_at_ns));
    descriptor
}

/// Enrolled Store at the operator root; no pinned root, so the bound one wins.
async fn deployment() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let (transport_root, store_root, config, ready, _) = enrolled_deployment().await;
    (transport_root, store_root, config, ready)
}
async fn enrolled_deployment() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    qos_user_identity::cloud_enrollment::EnrollmentRequest,
) {
    let (transport_root, store_root, config, ready) =
        crate::https_owner::serve::tests::fixture().await;
    let owner_root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = crate::Store::open(store_root.path(), MACHINE).unwrap();
    let request = crate::tests::approved(store.inspect(10).unwrap(), &owner_root, &device);
    store.apply(&request, 11).unwrap();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    drop(store);
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    json["initial_owner_root"] = serde_json::Value::Null;
    std::fs::write(&config, serde_json::to_vec(&json).unwrap()).unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    (transport_root, store_root, config, ready, request)
}
/// Start the composed service and wait for its Ready record.
async fn started(
    config: &Path,
    ready: &Path,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    String,
) {
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = {
        let (config, ready) = (config.to_owned(), ready.to_owned());
        tokio::spawn(async move {
            crate::https_owner::serve::serve_broker_only_until(&config, &ready, async {
                let _ = stopped.await;
            })
            .await
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !ready.exists() {
        assert!(
            !server.is_finished(),
            "server stopped before Ready: {:?}",
            server.await
        );
        assert!(std::time::Instant::now() < deadline, "Ready not published");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let record: serde_json::Value = serde_json::from_slice(&std::fs::read(ready).unwrap()).unwrap();
    assert_eq!(record["capabilities"], PRODUCT_DATA_V1);
    (stop, server, record["origin"].as_str().unwrap().to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_loopback_product_data_survives_owner_reopen() {
    let (_transport_root, store_root, config, ready) = deployment().await;
    let root = store_root.path().to_owned();
    // Ready is published only after the complete service is installed.
    let (stop, server, origin) = started(&config, &ready).await;
    let mut client = Client {
        http: reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
        origin,
        key: SigningKey::from_bytes(&[8; 32]),
        session: None,
    };
    handshake(&mut client, &root).await;

    // Small complete item: one inline request, one native receipt.
    let small = browser_delta(&root, MACHINE, "small ");
    let (status, body) = client
        .post("/peer/v3/state", chunk([1; 16], &small, 0, small.len()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let small_commit = accepted(&body);

    // Resumable item: the first range answers FlowCredit, the last admits.
    let large = browser_delta(&root, MACHINE, "resumable ");
    let half = large.len() / 2;
    let (status, body) = client
        .post("/peer/v3/state", chunk([2; 16], &large, 0, half))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let frames_ = frames(&body);
    assert_eq!(frames_[0].0, MessageType::FlowCredit);
    let credit = FlowCredit::decode(&frames_[0].1).unwrap();
    assert_eq!(credit.next_offset(), half as u64);
    let (status, body) = client
        .post("/peer/v3/state", chunk([2; 16], &large, half, large.len()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let large_commit = accepted(&body);
    assert!(large_commit.sequence > small_commit.sequence);
    // A lost final reply is answered from the durable receipt, not re-admitted.
    let (status, body) = client
        .post("/peer/v3/state", chunk([2; 16], &large, 0, half))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(accepted(&body), large_commit);

    // Replay every committed group in order through our admitted commits.
    let mut cursor = None;
    let mut seen = Vec::new();
    while seen
        .last()
        .is_none_or(|sequence| *sequence < large_commit.sequence)
    {
        let boundary = poll(&client, cursor.as_ref())
            .await
            .expect("committed group");
        seen.push(boundary.committed().sequence);
        cursor = Some(boundary.cursor().clone());
    }
    assert!(seen.contains(&small_commit.sequence) && seen.contains(&large_commit.sequence));
    let acknowledged = cursor.clone().unwrap();
    let (status, body) = client
        .post(
            "/peer/v3/acks",
            framed(
                MessageType::CursorAck,
                &CursorAck::new(acknowledged).encode().unwrap(),
            ),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(body.is_empty());

    // A held poll after the last group is woken by the next Store commit.
    let held = {
        let client = Client {
            http: client.http.clone(),
            origin: client.origin.clone(),
            key: client.key.clone(),
            session: client.session,
        };
        let cursor = cursor.clone();
        tokio::spawn(async move { poll(&client, cursor.as_ref()).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let later = browser_delta(&root, MACHINE, "held ");
    let (status, body) = client
        .post("/peer/v3/state", chunk([3; 16], &later, 0, later.len()))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let later_commit = accepted(&body);
    let woken = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .expect("held poll woken by commit")
        .unwrap()
        .expect("committed group");
    assert_eq!(woken.committed(), &later_commit);

    // Pinned baseline snapshot: signed manifest, every chunk, final boundary.
    let peer_now = peer(&root);
    let host_key =
        ed25519_dalek::VerifyingKey::from_bytes(&peer_now.authority().device_public_key).unwrap();
    let request = SnapshotRequest::new(SnapshotOperation::Baseline).unwrap();
    let (status, body) = client
        .post(
            "/peer/v3/snapshots",
            framed(MessageType::SnapshotManifest, &request.encode().unwrap()),
        )
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let manifest_frames = frames(&body);
    assert_eq!(manifest_frames.len(), 1);
    assert_eq!(manifest_frames[0].0, MessageType::SnapshotManifest);
    let manifest = SnapshotManifest::from_bytes(&manifest_frames[0].1).unwrap();
    let signed = manifest
        .authenticate(&host_key, qos_relay::clock::now_ns())
        .unwrap();
    let issue = signed.issue();
    let chunks = issue.chunk_digests.len();
    assert!(chunks >= 1);
    let mut boundary = None;
    for index in 0..chunks {
        let (status, body) = client
            .get(&format!(
                "/peer/v3/snapshots/{}/chunks/{index}",
                hex::encode(issue.pin)
            ))
            .await;
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&body)
        );
        let chunk_frames = frames(&body);
        assert_eq!(chunk_frames[0].0, MessageType::SnapshotChunk);
        signed
            .validate_chunk(&SnapshotChunk::decode(&chunk_frames[0].1).unwrap())
            .unwrap();
        if index + 1 == chunks {
            assert_eq!(chunk_frames[1].0, MessageType::CommitBoundary);
            boundary = Some(CommitBoundary::decode(&chunk_frames[1].1).unwrap());
        }
    }
    assert_eq!(boundary.unwrap().committed(), &issue.scope.committed);
    // Renewal after served progress re-signs the same durable pin.
    let renew = SnapshotRequest::new(SnapshotOperation::Renew {
        pin: issue.pin,
        manifest_digest: manifest.digest(),
    })
    .unwrap();
    let (status, body) = client
        .post(
            "/peer/v3/snapshots",
            framed(MessageType::SnapshotManifest, &renew.encode().unwrap()),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let renewed_frames = frames(&body);
    assert_eq!(renewed_frames[0].0, MessageType::SnapshotManifest);
    let renewed = SnapshotManifest::from_bytes(&renewed_frames[0].1)
        .unwrap()
        .authenticate(&host_key, qos_relay::clock::now_ns())
        .unwrap();
    assert_eq!(renewed.issue().pin, issue.pin);
    assert!(renewed.issue().expires_at_ns > issue.expires_at_ns);

    // Private command through the Store-backed broker, and an idle event poll.
    let action = DirectedAction {
        room: peer_now.room().parse().unwrap(),
        request_id: uuid::Uuid::new_v4(),
        source: client.key.verifying_key().to_bytes(),
        target: peer_now.authority().device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    let scope = PrivateScope {
        room: action.room,
        source: action.source,
        target: action.target,
    };
    let (status, body) = client
        .post(
            PrivateOperation::Catalogue.path(),
            private_wire::encode_request(PrivateOperation::Catalogue, scope, &action).unwrap(),
        )
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let reply = private_wire::decode_response(PrivateOperation::Catalogue, &action, &body).unwrap();
    assert!(matches!(
        reply.message,
        ActionMessage::BrokerCatalogue { .. }
    ));
    // Nothing pending: the events poll is held (long poll) and answers idle
    // only near its deadline, instead of an immediate empty reply.
    let started = std::time::Instant::now();
    let (status, body) = client.get(private_wire::PRIVATE_EVENTS_TARGET).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(body.is_empty(), "no watched transition is pending");
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(1),
        "idle events poll was answered after {:?}",
        started.elapsed()
    );

    // Detach, then a clean shutdown removes Ready after the final answers.
    let (id, _, _) = client.session.unwrap();
    let (status, _) = client
        .send(
            reqwest::Method::DELETE,
            &format!("/peer/v3/sessions/{}", hex::encode(id)),
            Vec::new(),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let (status, _) = client.get("/peer/v3/events?mode=poll").await;
    assert_eq!(
        status,
        reqwest::StatusCode::FORBIDDEN,
        "detached session is fenced"
    );
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(!ready.exists());

    // Owner reopen: receipts, the durable consumer ACK and document content.
    let store = crate::Store::open(&root, MACHINE).unwrap();
    let ledger = store.state.delivery.as_ref().unwrap().ledger();
    let device = client.key.verifying_key().to_bytes();
    let consumer = &ledger.consumers[&device];
    assert_eq!(
        consumer.acknowledged.as_ref().map(|cut| cut.sequence),
        Some(large_commit.sequence)
    );
    for delivery in [[1; 16], [2; 16], [3; 16]] {
        assert!(
            ledger
                .admissions
                .contains_key(&qos_relay::delivery_v3::commit::ReceiptKey { device, delivery })
        );
    }
    drop(store);
    let text = document_text(&root, MACHINE);
    for fragment in ["small", "resumable", "held"] {
        assert!(text.contains(fragment), "{fragment} missing from {text:?}");
    }
}

/// Staged upload work and private broker replies complete through their
/// wake-up events: with the idle tick disabled (60 s) and no busy tick, each
/// is still answered promptly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_upload_and_private_replies_are_answered_without_ticks() {
    let (_transport_root, store_root, config, ready) = deployment().await;
    let root = store_root.path().to_owned();
    super::quiet_ticks::quiet(&root);
    let (stop, server, origin) = started(&config, &ready).await;
    let mut client = Client {
        http: reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
        origin,
        key: SigningKey::from_bytes(&[8; 32]),
        session: None,
    };
    handshake(&mut client, &root).await;
    let bound = Duration::from_secs(10);
    let large = browser_delta(&root, MACHINE, "evented ");
    let (wakes_before, spent_before) = super::quiet_ticks::wakes(&root);
    let window = std::time::Instant::now();
    let third = large.len() / 3;
    let mut timings = Vec::new();
    for (start, end) in [(0, third), (third, 2 * third)] {
        let began = std::time::Instant::now();
        let (status, body) = tokio::time::timeout(
            bound,
            client.post("/peer/v3/state", chunk([4; 16], &large, start, end)),
        )
        .await
        .expect("staged chunk answered without a tick");
        timings.push(began.elapsed());
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&body)
        );
        assert_eq!(frames(&body)[0].0, MessageType::FlowCredit);
    }
    let began = std::time::Instant::now();
    let (status, body) = tokio::time::timeout(
        bound,
        client.post(
            "/peer/v3/state",
            chunk([4; 16], &large, 2 * third, large.len()),
        ),
    )
    .await
    .expect("final staged chunk admitted without a tick");
    timings.push(began.elapsed());
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    accepted(&body);

    let peer_now = peer(&root);
    let action = DirectedAction {
        room: peer_now.room().parse().unwrap(),
        request_id: uuid::Uuid::new_v4(),
        source: client.key.verifying_key().to_bytes(),
        target: peer_now.authority().device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    let scope = PrivateScope {
        room: action.room,
        source: action.source,
        target: action.target,
    };
    let began = std::time::Instant::now();
    let (status, body) = tokio::time::timeout(
        bound,
        client.post(
            PrivateOperation::Catalogue.path(),
            private_wire::encode_request(PrivateOperation::Catalogue, scope, &action).unwrap(),
        ),
    )
    .await
    .expect("private reply answered without a tick");
    timings.push(began.elapsed());
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let reply = private_wire::decode_response(PrivateOperation::Catalogue, &action, &body).unwrap();
    assert!(matches!(
        reply.message,
        ActionMessage::BrokerCatalogue { .. }
    ));
    let (wakes, spent) = super::quiet_ticks::wakes(&root);
    eprintln!(
        "evented completion latencies: {timings:?}; service wake-ups {} ({:?} in maintain) over {:?} (baseline tick: {})",
        wakes - wakes_before,
        spent - spent_before,
        window.elapsed(),
        crate::storage::host_perf_baseline(),
    );
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_serve_refuses_unactivated_store_without_ready() {
    let (_transport_root, store_root, config, ready) =
        crate::https_owner::serve::tests::fixture().await;
    let owner_root = SigningKey::from_bytes(&[7; 32]);
    let device = SigningKey::from_bytes(&[8; 32]);
    let mut store = crate::Store::open(store_root.path(), MACHINE).unwrap();
    let request = crate::tests::approved(store.inspect(10).unwrap(), &owner_root, &device);
    store.apply(&request, 11).unwrap();
    drop(store);
    let mut json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    json["initial_owner_root"] = serde_json::Value::Null;
    std::fs::write(&config, serde_json::to_vec(&json).unwrap()).unwrap();
    let before = std::fs::read(store_root.path().join("state.json")).unwrap();
    // The transport config has no explicit activation: no schema transition.
    let error = crate::https_owner::serve::serve_until(&config, &ready, std::future::pending())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("activate_delivery"), "{error}");
    assert_eq!(
        std::fs::read(store_root.path().join("state.json")).unwrap(),
        before
    );
    assert!(!ready.exists());
}

/// One signed onboarding exchange from an unenrolled device over HTTPS.
async fn onboard(
    client: &Client,
    scope: qos_relay::delivery_v3::onboarding_wire::OnboardingScope<'_>,
    certificate: &DeviceCert,
    operation: qos_relay::delivery_v3::onboarding_wire::OnboardingOperation,
    join: &qos_peer_state::join::JoinTransportRequest,
) -> qos_peer_state::join::JoinTransportResponse {
    use qos_relay::delivery_v3::onboarding_wire::{
        OnboardingRequest, OnboardingResponse, ReplyBodyKind,
    };
    let body = postcard::to_stdvec(join).unwrap();
    let certificate_bytes = postcard::to_stdvec(certificate).unwrap();
    let now = qos_relay::clock::now_ns();
    let mut request = OnboardingRequest {
        version: 3,
        operation,
        scope,
        certificate: &certificate_bytes,
        nonce: media_wire::fresh_nonce().unwrap(),
        issued_at_ns: now,
        expires_at_ns: now + 30_000_000_000,
        body_digest: *blake3::hash(&body).as_bytes(),
        body: &body,
        signature: [[0; 32]; 2],
    };
    request.signature = media_wire::split_signature(
        client
            .key
            .sign(&request.signing_digest().unwrap())
            .to_bytes(),
    );
    let (status, reply) = client
        .post(operation.path(), request.encode().unwrap())
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&reply)
    );
    let response = OnboardingResponse::decode(&reply).unwrap();
    response
        .verify_host_signature(&request, qos_relay::clock::now_ns())
        .unwrap();
    assert_eq!(response.body_kind, ReplyBodyKind::InlineLegacy);
    postcard::from_bytes(response.body).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_invited_device_onboards_then_authenticates_for_product_data() {
    use qos_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest as Join,
        JoinTransportResponse as Reply, decode_and_verify_invitation_text, sign_join_proof,
    };
    use qos_relay::delivery_v3::onboarding_wire::{OnboardingOperation, OnboardingScope};
    let (_transport_root, store_root, config, ready, enrollment) = enrolled_deployment().await;
    let root = store_root.path().to_owned();
    let (stop, server, origin) = started(&config, &ready).await;
    // The owner issues an invitation; the installed descriptor adds the one
    // exact HTTPS origin locator the browser uses for discovery.
    let admin =
        crate::broker::CloudCollaborationAdmin::new(&root, MACHINE, &enrollment.challenge.room);
    let issued = qos_action_broker::collaboration_admin::CollaborationAdmin::execute(
        &admin,
        qos_action_broker::collaboration_admin::CollaborationAdminRequest {
            room: enrollment.challenge.room.clone(),
            invocation: qos_actions::InvocationId::new(),
            caller: enrollment.browser_certificate.clone(),
            operation: qos_action_broker::collaboration_admin::CollaborationAdminOperation::Issue(
                qos_action_broker::collaboration_admin::IssueInvitationInput {
                    role: qos_action_broker::collaboration_admin::CollaborationRole::Editor,
                    expires_in_seconds: 600,
                    invitee_name: Some("Guest".into()),
                },
            ),
        },
    )
    .await
    .unwrap();
    let verified = decode_and_verify_invitation_text(
        issued["invite"].as_str().unwrap(),
        None,
        &RevocationList::new(),
        qos_relay::clock::now_ns(),
    )
    .unwrap();
    let locators = verified
        .grant()
        .transport
        .iter()
        .filter(|hint| hint.kind == qos_relay::workspace_join::TRANSPORT_DELIVERY_ORIGIN_V1)
        .collect::<Vec<_>>();
    assert_eq!(locators.len(), 1);
    let locator = qos_relay::workspace_join::DeliveryOriginLocatorV1::from_transport_hint(
        locators[0],
        &locators[0].host,
    )
    .unwrap();
    assert_eq!(locator.origin, origin);
    let signed_invitation = verified.signed_invitation().to_vec();

    let guest_root = SigningKey::from_bytes(&[9; 32]);
    let guest_device = SigningKey::from_bytes(&[10; 32]);
    let mut guest_cert = DeviceCert {
        user_public_key: guest_root.verifying_key().to_bytes(),
        device_public_key: guest_device.verifying_key().to_bytes(),
        device_label: "Guest device".into(),
        capabilities: qos_user_identity::DeviceCapabilities::member(),
        issued_at_ns: qos_relay::clock::now_ns() - 1_000_000_000,
        expires_at_ns: None,
        signature: [0; 64],
    };
    qos_user_identity::sign_device_cert(&mut guest_cert, &guest_root).unwrap();
    let mut guest = Client {
        http: reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
        origin: origin.clone(),
        key: guest_device.clone(),
        session: None,
    };
    let host = verified.grant().authority.clone();
    let scope = OnboardingScope {
        origin: &origin,
        host: host.device_public_key,
        workspace: verified.grant().workspace_id,
        root: host.user_public_key,
        room: verified.grant().room.parse().unwrap(),
    };
    let Reply::Inspection(inspection) = onboard(
        &guest,
        scope,
        &guest_cert,
        OnboardingOperation::Inspect,
        &Join::Inspect {
            signed_invitation: signed_invitation.clone(),
        },
    )
    .await
    else {
        panic!("inspection required")
    };
    assert_eq!(inspection.grant.room, enrollment.challenge.room);
    let Reply::Challenge(challenge) = onboard(
        &guest,
        scope,
        &guest_cert,
        OnboardingOperation::Challenge,
        &Join::Challenge(JoinChallengeRequest {
            signed_invitation,
            device_cert: guest_cert.clone(),
            request_id: [44; 32],
        }),
    )
    .await
    else {
        panic!("join challenge required")
    };
    let proof = sign_join_proof(challenge.challenge, &guest_device).unwrap();
    let Reply::Complete(bootstrap) = onboard(
        &guest,
        scope,
        &guest_cert,
        OnboardingOperation::Complete,
        &Join::Complete(JoinCompleteRequest { proof }),
    )
    .await
    else {
        panic!("join completion required")
    };
    assert_eq!(bootstrap.workspace_id, verified.grant().workspace_id);
    // The new member authenticates for every product data capability.
    handshake(&mut guest, &root).await;
    assert!(
        poll(&guest, None).await.is_some(),
        "baseline replay available"
    );
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    let store = crate::Store::open(&root, MACHINE).unwrap();
    let ledger = store.state.delivery.as_ref().unwrap().ledger();
    assert!(
        ledger
            .consumers
            .contains_key(&guest_device.verifying_key().to_bytes())
    );
}

/// The enrolled structural owner refreshes membership over HTTPS onboarding
/// and receives the owner bootstrap; a lost reply replays identical bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_structural_owner_membership_refresh_returns_owner_bootstrap() {
    use qos_peer_state::join::{
        JoinTransportRequest as Join, JoinTransportResponse as Reply, MembershipChallengeRequest,
        MembershipCompleteRequest, sign_membership_proof,
    };
    use qos_relay::delivery_v3::onboarding_wire::{OnboardingOperation, OnboardingScope};
    let (_transport_root, store_root, config, ready, enrollment) = enrolled_deployment().await;
    let root = store_root.path().to_owned();
    let (authority, room) = {
        let store = crate::Store::open(&root, MACHINE).unwrap();
        let peer = store.peer().unwrap();
        (peer.authority().clone(), peer.room().to_owned())
    };
    let (stop, server, origin) = started(&config, &ready).await;
    let device = SigningKey::from_bytes(&[8; 32]);
    let cert = enrollment.browser_certificate.clone();
    assert_eq!(cert.device_public_key, device.verifying_key().to_bytes());
    let owner = Client {
        http: reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
        origin: origin.clone(),
        key: device.clone(),
        session: None,
    };
    let workspace = qos_relay::room_workspace_id(&room).unwrap();
    let scope = OnboardingScope {
        origin: &origin,
        host: authority.device_public_key,
        workspace,
        root: authority.user_public_key,
        room: room.parse().unwrap(),
    };
    let Reply::MembershipChallenge(challenge) = onboard(
        &owner,
        scope,
        &cert,
        OnboardingOperation::MembershipChallenge,
        &Join::MembershipChallenge(MembershipChallengeRequest {
            room: room.clone(),
            workspace_id: workspace,
            device_cert: cert.clone(),
            request_id: [45; 32],
        }),
    )
    .await
    else {
        panic!("membership challenge required")
    };
    let complete = Join::MembershipComplete(MembershipCompleteRequest {
        proof: sign_membership_proof(challenge.challenge, &device).unwrap(),
    });
    let first = onboard(
        &owner,
        scope,
        &cert,
        OnboardingOperation::MembershipComplete,
        &complete,
    )
    .await;
    let Reply::OwnerMembershipComplete(bootstrap) = &first else {
        panic!("owner bootstrap required, got {first:?}")
    };
    assert_eq!(bootstrap.version, 1);
    assert_eq!(bootstrap.device_cert, cert);
    assert_eq!(bootstrap.authority, authority);
    assert_eq!(bootstrap.room, room);
    assert_eq!(bootstrap.workspace_id, workspace);
    assert_ne!(bootstrap.generation, 0);
    let replay = onboard(
        &owner,
        scope,
        &cert,
        OnboardingOperation::MembershipComplete,
        &complete,
    )
    .await;
    assert_eq!(
        postcard::to_stdvec(&replay).unwrap(),
        postcard::to_stdvec(&first).unwrap()
    );
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

/// Production composition: `serve-https` with the actual Cloud agent runtime
/// (not the broker-only test owner) over a Store with the V5 exact writer
/// selected, so the real certified workers mount and publish their startup
/// documents through the final scoped document evaluator. The runtime must
/// stay up, keep Ready, and shut down cleanly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_serve_with_actual_agent_runtime_stays_ready() {
    let (_transport_root, store_root, config, ready) = deployment().await;
    {
        let mut store = crate::Store::open(store_root.path(), MACHINE).unwrap();
        let mut peer = store.peer().unwrap();
        peer.select_admission_v5().unwrap();
        let mut state = store.state.clone();
        state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
        store.commit(state).unwrap();
    }
    let generation = crate::Store::open(store_root.path(), MACHINE)
        .unwrap()
        .state
        .generation;
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = {
        let (config, ready) = (config.clone(), ready.clone());
        tokio::spawn(async move {
            crate::https_owner::serve::serve_until(&config, &ready, async {
                let _ = stopped.await;
            })
            .await
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !ready.exists() {
        assert!(
            !server.is_finished(),
            "server stopped before Ready: {:?}",
            server.await
        );
        assert!(std::time::Instant::now() < deadline, "Ready not published");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
    assert_eq!(record["capabilities"], PRODUCT_DATA_V1);
    // The earlier production failure retired the runtime 1-2 s after startup.
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !server.is_finished(),
            "serve-https exited after Ready: {:?}",
            server.await
        );
        assert!(ready.exists(), "Ready record withdrawn while serving");
    }
    let _ = stop.send(());
    let served = tokio::time::timeout(Duration::from_secs(30), server)
        .await
        .expect("clean shutdown")
        .unwrap();
    assert!(served.is_ok(), "{served:?}");
    // Worker startup publications were admitted by the scoped evaluator: the
    // owner advanced and reopens cleanly with the exact writer still selected.
    let reopened = crate::Store::open(store_root.path(), MACHINE).unwrap();
    assert!(reopened.peer().unwrap().admission_v5_selected());
    // Delivery storage drains the outbox into its replay ledger, so count
    // owner commits: both mounted workers' startup publications committed.
    assert!(
        reopened.state.generation >= generation + 2,
        "worker startup documents did not reach the Store owner: {generation} -> {}",
        reopened.state.generation
    );
}

async fn descriptor_digest(client: &Client) -> [u8; 32] {
    // Unauthenticated discovery: no session proof on this route.
    let anonymous = Client {
        http: client.http.clone(),
        origin: client.origin.clone(),
        key: client.key.clone(),
        session: None,
    };
    let (status, hint) = anonymous.get("/peer/v3/descriptor").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let hint: qos_peer_state::join::TransportHint = postcard::from_bytes(&hint).unwrap();
    let peer = client_peer_host(&hint);
    descriptor::inspect_persisted_hint(&hint, &peer)
        .unwrap()
        .descriptor_digest
}
fn client_peer_host(hint: &qos_peer_state::join::TransportHint) -> DeviceCert {
    hint.host.clone()
}
async fn renewed(client: &Client, before: [u8; 32]) -> [u8; 32] {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let digest = descriptor_digest(client).await;
        if digest != before {
            return digest;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "descriptor was not renewed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Descriptor renewal must not end live sessions: a session authenticated
/// under the superseded descriptor keeps admitting state, polls and ACKs,
/// while new sessions authenticate against the renewed descriptor. A newer
/// data session for the same device still replaces the retained one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_descriptor_renewal_keeps_live_sessions() {
    let (_transport_root, store_root, config, ready) = deployment().await;
    let root = store_root.path().to_owned();
    let (stop, server, origin) = started(&config, &ready).await;
    let http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let mut first = Client {
        http: http.clone(),
        origin: origin.clone(),
        key: SigningKey::from_bytes(&[8; 32]),
        session: None,
    };
    let original = handshake(&mut first, &root).await;
    let small = browser_delta(&root, MACHINE, "before renewal ");
    let (status, body) = first
        .post("/peer/v3/state", chunk([1; 16], &small, 0, small.len()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let before = accepted(&body);

    super::context::forced_renewal::request(&origin, 1);
    let renewed_digest = renewed(&first, original.descriptor_digest()).await;
    assert_ne!(renewed_digest, original.descriptor_digest());

    // The superseded-descriptor session keeps working after renewal.
    let later = browser_delta(&root, MACHINE, "after renewal ");
    let (status, body) = first
        .post("/peer/v3/state", chunk([2; 16], &later, 0, later.len()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let after = accepted(&body);
    assert!(after.sequence > before.sequence);
    let mut cursor = None;
    let mut seen = Vec::new();
    while seen
        .last()
        .is_none_or(|sequence| *sequence < after.sequence)
    {
        let boundary = poll(&first, cursor.as_ref())
            .await
            .expect("committed group");
        seen.push(boundary.committed().sequence);
        cursor = Some(boundary.cursor().clone());
    }
    assert!(seen.contains(&before.sequence) && seen.contains(&after.sequence));
    let (status, _) = first
        .post(
            "/peer/v3/acks",
            framed(
                MessageType::CursorAck,
                &CursorAck::new(cursor.clone().unwrap()).encode().unwrap(),
            ),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);

    // A second renewal still retains it (two superseded generations).
    super::context::forced_renewal::request(&origin, 1);
    let third = renewed(&first, renewed_digest).await;
    assert_ne!(third, renewed_digest);
    let again = browser_delta(&root, MACHINE, "second renewal ");
    let (status, body) = first
        .post("/peer/v3/state", chunk([3; 16], &again, 0, again.len()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    accepted(&body);

    // A fresh handshake uses the current descriptor and replaces the device's
    // retained data session, which is then refused.
    let mut second = Client {
        http: http.clone(),
        origin: origin.clone(),
        key: SigningKey::from_bytes(&[8; 32]),
        session: None,
    };
    let current = handshake(&mut second, &root).await;
    assert_eq!(current.descriptor_digest(), third);
    let fresh = browser_delta(&root, MACHINE, "replacement ");
    let (status, _) = first
        .post("/peer/v3/state", chunk([4; 16], &fresh, 0, fresh.len()))
        .await;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    let (status, body) = second
        .post("/peer/v3/state", chunk([4; 16], &fresh, 0, fresh.len()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    accepted(&body);

    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

/// Take a complete baseline snapshot; returns its final boundary.
async fn baseline_snapshot(client: &Client, root: &Path) -> CommitBoundary {
    let host_key =
        ed25519_dalek::VerifyingKey::from_bytes(&peer(root).authority().device_public_key).unwrap();
    let request = SnapshotRequest::new(SnapshotOperation::Baseline).unwrap();
    let (status, body) = client
        .post(
            "/peer/v3/snapshots",
            framed(MessageType::SnapshotManifest, &request.encode().unwrap()),
        )
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let manifest_frames = frames(&body);
    assert_eq!(manifest_frames[0].0, MessageType::SnapshotManifest);
    let manifest = SnapshotManifest::from_bytes(&manifest_frames[0].1).unwrap();
    let signed = manifest
        .authenticate(&host_key, qos_relay::clock::now_ns())
        .unwrap();
    let issue = signed.issue();
    let chunks = issue.chunk_digests.len();
    let mut boundary = None;
    for index in 0..chunks {
        let (status, body) = client
            .get(&format!(
                "/peer/v3/snapshots/{}/chunks/{index}",
                hex::encode(issue.pin)
            ))
            .await;
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&body)
        );
        let chunk_frames = frames(&body);
        signed
            .validate_chunk(&SnapshotChunk::decode(&chunk_frames[0].1).unwrap())
            .unwrap();
        if index + 1 == chunks {
            assert_eq!(chunk_frames[1].0, MessageType::CommitBoundary);
            boundary = Some(CommitBoundary::decode(&chunk_frames[1].1).unwrap());
        }
    }
    boundary.expect("final chunk carries the boundary")
}

/// Retention rotation on the running service: a caught-up consumer's cursor
/// is answered SnapshotRequired(StoreRotated) instead of a refusal, the
/// snapshot + suffix restores it, admitted state is preserved, and a
/// retransmission whose receipt was released is a certified duplicate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_retention_rotation_recovers_consumers_by_snapshot_and_suffix() {
    use qos_relay::delivery_v3::state::{ResetReason, SnapshotRequired};
    let (_transport_root, store_root, config, ready) = deployment().await;
    let root = store_root.path().to_owned();
    let (stop, server, origin) = started(&config, &ready).await;
    let mut client = Client {
        http: reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
        origin: origin.clone(),
        key: SigningKey::from_bytes(&[8; 32]),
        session: None,
    };
    handshake(&mut client, &root).await;
    let first = browser_delta(&root, MACHINE, "kept-one ");
    let (status, body) = client
        .post("/peer/v3/state", chunk([1; 16], &first, 0, first.len()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let second = browser_delta(&root, MACHINE, "kept-two ");
    let (status, body) = client
        .post("/peer/v3/state", chunk([2; 16], &second, 0, second.len()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let last = accepted(&body);
    let mut cursor = None;
    loop {
        let boundary = poll(&client, cursor.as_ref())
            .await
            .expect("committed group");
        cursor = Some(boundary.cursor().clone());
        if boundary.committed().sequence >= last.sequence {
            break;
        }
    }
    let (status, _) = client
        .post(
            "/peer/v3/acks",
            framed(
                MessageType::CursorAck,
                &CursorAck::new(cursor.clone().unwrap()).encode().unwrap(),
            ),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let text = crate::delivery::admission::tests::document_text(&root, MACHINE);

    super::forced_rotation::request(&origin);
    // Held or immediate, the caught-up cursor is answered with a reset.
    let target = format!(
        "/peer/v3/events?cursor={}&mode=poll",
        encode_cursor_url(cursor.as_ref().unwrap().as_bytes()).unwrap()
    );
    let (status, body) = tokio::time::timeout(Duration::from_secs(10), client.get(&target))
        .await
        .expect("rotation wakes the held poll");
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let reset = frames(&body);
    assert_eq!(reset.len(), 1);
    assert_eq!(reset[0].0, MessageType::SnapshotRequired);
    let required = SnapshotRequired::decode(&reset[0].1).unwrap();
    assert_eq!(required.reason(), ResetReason::StoreRotated);

    // Snapshot + suffix: the baseline restores the consumer, then new commits
    // replay after its boundary.
    let boundary = baseline_snapshot(&client, &root).await;
    let after = browser_delta(&root, MACHINE, "after-rotation ");
    let (status, body) = client
        .post("/peer/v3/state", chunk([3; 16], &after, 0, after.len()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let suffix = accepted(&body);
    let next = poll(&client, Some(boundary.cursor()))
        .await
        .expect("suffix group");
    assert_eq!(next.committed(), &suffix);
    let current = crate::delivery::admission::tests::document_text(&root, MACHINE);
    assert!(
        current.contains("after-rotation")
            && text.lines().all(|line| current.contains(line.trim()))
    );

    // The acknowledged receipt was released; its retransmission is answered
    // as a certified duplicate of the original operation, not a new edit.
    let (status, body) = client
        .post("/peer/v3/state", chunk([1; 16], &first, 0, first.len()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let frames_ = frames(&body);
    match StateAdmission::decode(&frames_[0].1).unwrap().outcome() {
        AdmissionOutcome::Duplicate { .. } => {}
        other => panic!("retransmission must be a duplicate: {other:?}"),
    }
    assert_eq!(
        crate::delivery::admission::tests::document_text(&root, MACHINE)
            .matches("kept-one")
            .count(),
        1
    );
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

/// Opt-in diagnostics for loopback tests (`RUST_LOG=...`).
fn trace() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}
fn private_action(client: &Client, root: &Path, message: ActionMessage) -> DirectedAction {
    let peer = peer(root);
    DirectedAction {
        room: peer.room().parse().unwrap(),
        request_id: uuid::Uuid::new_v4(),
        source: client.key.verifying_key().to_bytes(),
        target: peer.authority().device_public_key,
        message,
    }
}
async fn private_call(
    client: &Client,
    operation: PrivateOperation,
    action: &DirectedAction,
) -> DirectedAction {
    let scope = PrivateScope {
        room: action.room,
        source: action.source,
        target: action.target,
    };
    let started = std::time::Instant::now();
    // 503 is the carrier's retryable answer (owner contended past the request
    // deadline, e.g. a loaded debug test process); retry like a client does.
    let mut attempts = 0;
    let (status, body) = loop {
        let (status, body) = client
            .post(
                operation.path(),
                private_wire::encode_request(operation, scope, action).unwrap(),
            )
            .await;
        attempts += 1;
        if status != reqwest::StatusCode::SERVICE_UNAVAILABLE || attempts == 5 {
            break (status, body);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    eprintln!(
        "private {operation:?} answered in {:?} ({attempts} attempt(s))",
        started.elapsed()
    );
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{operation:?}: {}",
        String::from_utf8_lossy(&body)
    );
    private_wire::decode_response(operation, action, &body).unwrap()
}

/// Installed private lane with the ACTUAL Cloud agent runtime (not the
/// broker-only owner): catalogue, agent read, status and cancel are answered
/// through the one queue consumer, and every answer releases its queue slot
/// with its response (more sequential calls than the queue has slots).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_private_lane_with_actual_agent_runtime_answers_every_operation() {
    trace();
    let (_transport_root, store_root, config, ready) = deployment().await;
    let root = store_root.path().to_owned();
    {
        let mut store = crate::Store::open(&root, MACHINE).unwrap();
        let mut peer = store.peer().unwrap();
        peer.select_admission_v5().unwrap();
        let mut state = store.state.clone();
        state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
        store.commit(state).unwrap();
    }
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = {
        let (config, ready) = (config.clone(), ready.clone());
        tokio::spawn(async move {
            crate::https_owner::serve::serve_until(&config, &ready, async {
                let _ = stopped.await;
            })
            .await
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !ready.exists() {
        assert!(
            !server.is_finished(),
            "server stopped before Ready: {:?}",
            server.await
        );
        assert!(std::time::Instant::now() < deadline, "Ready not published");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
    let mut client = Client {
        http: reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
        origin: record["origin"].as_str().unwrap().to_owned(),
        key: SigningKey::from_bytes(&[8; 32]),
        session: None,
    };
    handshake(&mut client, &root).await;

    let catalogue = private_action(
        &client,
        &root,
        ActionMessage::CatalogueRefresh { scope: None },
    );
    let reply = private_call(&client, PrivateOperation::Catalogue, &catalogue).await;
    assert!(
        matches!(reply.message, ActionMessage::BrokerCatalogue { .. }),
        "{:?}",
        reply.message
    );

    let read = private_action(
        &client,
        &root,
        ActionMessage::AgentReadV1 {
            host: catalogue.room,
            invocation: uuid::Uuid::new_v4(),
            request: qos_agent_control_protocol::AgentRequest::List {
                version: 1,
                cursor: None,
            },
        },
    );
    let reply = private_call(&client, PrivateOperation::AgentRead, &read).await;
    assert!(
        matches!(
            reply.message,
            ActionMessage::AgentReadResultV1 { .. } | ActionMessage::Rejected { .. }
        ),
        "agent read answered by the actual owner: {:?}",
        reply.message
    );

    // Unknown invocations are answered (never hang, never a transport error).
    let unknown = qos_actions::InvocationId(uuid::Uuid::new_v4());
    let status = private_action(
        &client,
        &root,
        ActionMessage::Status {
            invocation: unknown,
        },
    );
    let reply = private_call(&client, PrivateOperation::Status, &status).await;
    assert!(
        matches!(
            reply.message,
            ActionMessage::Rejected { .. } | ActionMessage::Observation { .. }
        ),
        "{:?}",
        reply.message
    );
    let cancel = private_action(
        &client,
        &root,
        ActionMessage::Cancel {
            invocation: unknown,
        },
    );
    let reply = private_call(&client, PrivateOperation::Cancel, &cancel).await;
    assert!(
        matches!(
            reply.message,
            ActionMessage::Rejected { .. } | ActionMessage::CancellationRequested { .. }
        ),
        "{:?}",
        reply.message
    );
    // 33 > the 32-slot queue: each answered response released its slot.
    for _ in 0..33 {
        let again = private_action(
            &client,
            &root,
            ActionMessage::CatalogueRefresh { scope: None },
        );
        let reply = private_call(&client, PrivateOperation::Catalogue, &again).await;
        assert!(matches!(
            reply.message,
            ActionMessage::BrokerCatalogue { .. }
        ));
    }
    let (status, body) = client.get(private_wire::PRIVATE_EVENTS_TARGET).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(body.is_empty());
    let _ = stop.send(());
    let served = tokio::time::timeout(Duration::from_secs(30), server)
        .await
        .expect("clean shutdown")
        .unwrap();
    assert!(served.is_ok(), "{served:?}");
}

/// Pinned snapshot survives an actual service restart over the same Store:
/// after re-authentication the recovered pin serves chunks, renews with the
/// same pin identity, and completes with its boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_snapshot_pin_recovers_after_actual_restart() {
    trace();
    let (_transport_root, store_root, config, ready) = deployment().await;
    let root = store_root.path().to_owned();
    let (stop, server, origin) = started(&config, &ready).await;
    let mut client = Client {
        http: reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
        origin,
        key: SigningKey::from_bytes(&[8; 32]),
        session: None,
    };
    handshake(&mut client, &root).await;
    let item = browser_delta(&root, MACHINE, "pinned ");
    let (status, body) = client
        .post("/peer/v3/state", chunk([1; 16], &item, 0, item.len()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let host_key =
        ed25519_dalek::VerifyingKey::from_bytes(&peer(&root).authority().device_public_key)
            .unwrap();
    let request = SnapshotRequest::new(SnapshotOperation::Baseline).unwrap();
    let (status, body) = client
        .post(
            "/peer/v3/snapshots",
            framed(MessageType::SnapshotManifest, &request.encode().unwrap()),
        )
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let manifest = SnapshotManifest::from_bytes(&frames(&body)[0].1).unwrap();
    let signed = manifest
        .authenticate(&host_key, qos_relay::clock::now_ns())
        .unwrap();
    let issue = signed.issue().clone();
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();

    // Actual restart: new process-local boot and registry, same Store.
    let (stop, server, origin) = started(&config, &ready).await;
    let mut client = Client {
        http: client.http.clone(),
        origin,
        key: client.key.clone(),
        session: None,
    };
    handshake(&mut client, &root).await;
    let chunk_target = |index: usize| {
        format!(
            "/peer/v3/snapshots/{}/chunks/{index}",
            hex::encode(issue.pin)
        )
    };
    let renew = |digest| {
        framed(
            MessageType::SnapshotManifest,
            &SnapshotRequest::new(SnapshotOperation::Renew {
                pin: issue.pin,
                manifest_digest: digest,
            })
            .unwrap()
            .encode()
            .unwrap(),
        )
    };
    // Reload Renew: the durable pin is reconciled from its signed sidecar.
    // With no progress served since the restart it answers the identical
    // current manifest (never SnapshotRequired, never a silent extension).
    let (status, body) = client
        .post("/peer/v3/snapshots", renew(manifest.digest()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let recovered_frames = frames(&body);
    assert_eq!(
        recovered_frames[0].0,
        MessageType::SnapshotManifest,
        "recovered pin renews instead of SnapshotRequired"
    );
    let recovered = SnapshotManifest::from_bytes(&recovered_frames[0].1).unwrap();
    assert_eq!(recovered.digest(), manifest.digest());
    let chunks = issue.chunk_digests.len();
    let mut boundary = None;
    for index in 0..chunks {
        let (status, body) = client.get(&chunk_target(index)).await;
        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&body)
        );
        let chunk_frames = frames(&body);
        signed
            .validate_chunk(&SnapshotChunk::decode(&chunk_frames[0].1).unwrap())
            .unwrap();
        if index + 1 == chunks {
            boundary = Some(CommitBoundary::decode(&chunk_frames[1].1).unwrap());
        }
    }
    // Progress served after the restart qualifies a real renewal: same pin
    // and content, strictly later expiry.
    let (status, body) = client
        .post("/peer/v3/snapshots", renew(manifest.digest()))
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let renewed_frames = frames(&body);
    assert_eq!(renewed_frames[0].0, MessageType::SnapshotManifest);
    let renewed = SnapshotManifest::from_bytes(&renewed_frames[0].1)
        .unwrap()
        .authenticate(&host_key, qos_relay::clock::now_ns())
        .unwrap();
    assert_eq!(renewed.issue().pin, issue.pin);
    assert_eq!(renewed.issue().chunk_digests, issue.chunk_digests);
    assert!(renewed.issue().expires_at_ns > issue.expires_at_ns);
    let boundary = boundary.expect("final chunk carries the boundary");
    assert_eq!(boundary.committed(), &issue.scope.committed);
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

/// Installed Cloud media control route: a signed grant challenge and
/// completion over the actual HTTPS listener and Cloud Store owner.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_media_control_route_grants_over_installed_listener() {
    trace();
    use qos_relay::delivery_v3::media_wire::{
        ChallengeTranscript, ControlCommand, ControlReply, ControlRequest, ControlResult,
        GrantOperation, GrantRequest, MediaDirections, SignedGrant,
    };
    let (_transport_root, store_root, config, ready) = deployment().await;
    let root = store_root.path().to_owned();
    let device = SigningKey::from_bytes(&[8; 32]);
    let call = {
        use automerge::{ObjType, ROOT, ReadDoc, transaction::Transactable};
        let mut store = crate::Store::open(&root, MACHINE).unwrap();
        let now = qos_relay::clock::now_ns();
        let (before, mut peer) = store.peer_transition_at(now).unwrap();
        let mut chat =
            automerge::AutoCommit::load(&peer.document("so.tilo.chat", "").unwrap()).unwrap();
        let channels = chat.get(ROOT, "channels").unwrap().unwrap().1;
        let channel = chat
            .put_object(&channels, "cloud_voice", ObjType::Map)
            .unwrap();
        chat.put(&channel, "kind", "voice").unwrap();
        chat.put(&channel, "workspace", "cloud").unwrap();
        peer.publish_document(
            "so.tilo.chat",
            "",
            chat.save(),
            &store.key(),
            crate::now_time(now),
        )
        .unwrap();
        let mut state = store.state.clone();
        state.outbox.extend(
            crate::durable_mutation_publications(
                &mut peer,
                &before,
                &store.key(),
                crate::now_time(now),
                store.next_generation().unwrap(),
            )
            .unwrap(),
        );
        state.bound.as_mut().unwrap().peer = peer.durable().unwrap();
        store.commit(state).unwrap();
        let receipt = store
            .action(
                device.verifying_key().to_bytes(),
                &crate::ActionRequest {
                    version: 1,
                    room: store.state.room.clone(),
                    invocation_id: "loopback-media-create".into(),
                    operation: crate::NativeOperation::MediaCreate(
                        qos_action_broker::collaboration_admin::MediaCreateInput {
                            app_id: "so.tilo.chat".into(),
                            record_id: "".into(),
                            channel_id: "cloud_voice".into(),
                            expected_authorization_revision: None,
                            allowed: qos_action_broker::collaboration_admin::MediaDirectionsInput {
                                publish: true,
                                subscribe: true,
                            },
                        },
                    ),
                },
                qos_relay::clock::now_ns(),
            )
            .unwrap();
        let result: qos_peer_state::room::media::MediaCallResultV1 =
            serde_json::from_value(receipt.output.unwrap()).unwrap();
        hex::encode(result.call_id)
    };
    let (stop, server, origin) = started(&config, &ready).await;
    let mut client = Client {
        http: reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
        origin,
        key: device.clone(),
        session: None,
    };
    // Media control uses its own capability-zero session, as the browser does.
    let descriptor = handshake_with(&mut client, &root, 0).await;
    let (session_id, _, _) = client.session.unwrap();
    let control = |command: ControlCommand| {
        let target = command.target().to_owned();
        let body = ControlRequest {
            version: 1,
            command,
        }
        .encode()
        .unwrap();
        (target, body)
    };
    let grant = GrantRequest {
        device: device.verifying_key().to_bytes(),
        room: peer(&root).room().to_owned(),
        call_id: call,
        participation_id: [32; 32],
        generation: 0,
        directions: MediaDirections {
            publish: true,
            subscribe: true,
        },
    };
    let (target, body) = control(ControlCommand::GrantChallenge {
        request: grant.clone(),
        operation: GrantOperation::Join,
    });
    let (status, reply) = client.post(&target, body).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&reply)
    );
    let ControlResult::Challenge(challenge) = ControlReply::decode(&reply).unwrap().result else {
        panic!("signed media challenge expected")
    };
    assert_eq!(challenge.context.delivery_session_id, session_id);
    assert_eq!(
        challenge.context.descriptor_digest,
        descriptor.descriptor_digest()
    );
    let ChallengeTranscript::Native(native) = &challenge.transcript else {
        panic!("native transcript expected")
    };
    let signature = media_wire::split_signature(
        device
            .sign(&challenge.transcript.signing_digest().unwrap())
            .to_bytes(),
    );
    let (target, body) = control(ControlCommand::GrantComplete {
        nonce: native.nonce,
        signature,
    });
    let (status, reply) = client.post(&target, body).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&reply)
    );
    let ControlResult::Granted { grant: signed } = ControlReply::decode(&reply).unwrap().result
    else {
        panic!("signed media grant expected")
    };
    let signed = SignedGrant::decode(&signed).unwrap();
    assert_eq!(signed.metadata.request.device, grant.device);
    assert_eq!(
        signed.metadata.host,
        peer(&root).authority().device_public_key
    );
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

/// Minimal HTTP/3 client: SETTINGS on a control stream, one request stream
/// with static-table/literal QPACK, then the response frames.
async fn h3_get(origin: &str, target: &str) -> (String, Vec<u8>) {
    use web_transport_quinn::{proto::QpackHeaders, quinn};
    fn varint(out: &mut Vec<u8>, value: u64) {
        match value {
            0..=63 => out.push(value as u8),
            64..=16383 => out.extend_from_slice(&((value as u16) | 0x4000).to_be_bytes()),
            _ => out.extend_from_slice(&((value as u32) | 0x8000_0000).to_be_bytes()),
        }
    }
    fn take(rest: &mut &[u8]) -> u64 {
        let extra = (1usize << (rest[0] >> 6)) - 1;
        let mut value = u64::from(rest[0] & 0x3f);
        for byte in &rest[1..=extra] {
            value = (value << 8) | u64::from(*byte);
        }
        *rest = &rest[1 + extra..];
        value
    }
    #[derive(Debug)]
    struct AnyCert;
    impl rustls::client::danger::ServerCertVerifier for AnyCert {
        fn verify_server_cert(
            &self,
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &[rustls::pki_types::CertificateDer<'_>],
            _: &rustls::pki_types::ServerName<'_>,
            _: &[u8],
            _: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &rustls::pki_types::CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }
    let url: url::Url = origin.parse().unwrap();
    let authority = format!("{}:{}", url.host_str().unwrap(), url.port().unwrap());
    let mut tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .dangerous()
    .with_custom_certificate_verifier(std::sync::Arc::new(AnyCert))
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(std::sync::Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
    )));
    let address: std::net::SocketAddr = format!("127.0.0.1:{}", url.port().unwrap())
        .parse()
        .unwrap();
    let conn = endpoint
        .connect(address, url.host_str().unwrap())
        .unwrap()
        .await
        .unwrap();
    let mut control = conn.open_uni().await.unwrap();
    let mut settings = Vec::new();
    web_transport_quinn::proto::Settings::default().encode(&mut settings);
    control.write_all(&settings).await.unwrap();
    let (mut tx, mut rx) = conn.open_bi().await.unwrap();
    let mut headers = QpackHeaders::default();
    for (name, value) in [
        (":method", "GET"),
        (":scheme", "https"),
        (":authority", authority.as_str()),
        (":path", target),
    ] {
        headers.set(name, value);
    }
    let mut block = Vec::new();
    headers.encode(&mut block);
    let mut out = Vec::new();
    varint(&mut out, 0x01);
    varint(&mut out, block.len() as u64);
    out.extend_from_slice(&block);
    tx.write_all(&out).await.unwrap();
    tx.finish().unwrap();
    let raw = rx.read_to_end(1024 * 1024).await.unwrap();
    let (mut rest, mut status, mut body) = (raw.as_slice(), String::new(), Vec::new());
    while !rest.is_empty() {
        let (kind, len) = (take(&mut rest), take(&mut rest) as usize);
        let (payload, tail) = rest.split_at(len);
        rest = tail;
        if kind == 0x01 {
            status = QpackHeaders::decode(&mut &payload[..])
                .unwrap()
                .get(":status")
                .unwrap()
                .to_owned();
        } else if kind == 0x00 {
            body.extend_from_slice(payload);
        }
    }
    drop(control);
    (status, body)
}

/// The Cloud host shares the native transport: the same routes are served
/// over HTTP/3 on its QUIC endpoint and advertised on HTTP/2 with Alt-Svc.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_routes_are_served_over_h3_and_advertised_with_alt_svc() {
    let (_transport_root, _store_root, config, ready) = deployment().await;
    let (stop, server, origin) = started(&config, &ready).await;
    let http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let response = http
        .get(format!("{origin}/peer/v3/descriptor"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let port = origin.rsplit(':').next().unwrap().to_owned();
    assert_eq!(
        response.headers().get("alt-svc").unwrap().to_str().unwrap(),
        format!("h3=\":{port}\"; ma=3600")
    );
    let over_h2 = response.bytes().await.unwrap().to_vec();
    let (status, over_h3) = tokio::time::timeout(
        Duration::from_secs(10),
        h3_get(&origin, "/peer/v3/descriptor"),
    )
    .await
    .unwrap();
    assert_eq!(status, "200");
    assert_eq!(over_h3, over_h2, "same signed descriptor bytes over HTTP/3");
    // Proof-required data routes keep their HTTP/2 semantics over HTTP/3.
    let (status, _) = h3_get(&origin, "/peer/v3/events?mode=poll").await;
    assert_eq!(status, "401");
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

/// Lane D2: a multi-item upload is one commit/replay group; a session that
/// negotiated MULTI_GROUP_REPLAY_V1 catches up in one page, an old one keeps
/// exactly one group per page.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_multi_item_upload_is_one_group_and_new_sessions_catch_up_in_one_page() {
    use qos_relay::delivery_v3::capabilities::{
        KNOWN_DATA, MULTI_GROUP_REPLAY_V1, client_allowed, handshake_policy,
    };
    let (_transport_root, store_root, config, ready) = deployment().await;
    let root = store_root.path().to_owned();
    let (stop, server, origin) = started(&config, &ready).await;
    let mut client = Client {
        http: reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
        origin,
        key: SigningKey::from_bytes(&[8; 32]),
        session: None,
    };
    let new_policy = auth::CapabilityPolicy {
        supported: KNOWN_DATA,
        allowed: client_allowed(PRODUCT_DATA_V1),
    };
    handshake_negotiating(
        &mut client,
        &root,
        (handshake_policy(PRODUCT_DATA_V1), PRODUCT_DATA_V1),
        new_policy,
        PRODUCT_DATA_V1 | MULTI_GROUP_REPLAY_V1,
    )
    .await;
    let deltas = crate::delivery::admission::tests::browser_deltas(
        &root,
        MACHINE,
        &["a ", "b ", "c ", "d ", "e "],
    );
    // One POST of three items: three receipts, one shared group.
    let mut body = Vec::new();
    for (i, delta) in deltas[..3].iter().enumerate() {
        body.extend(chunk([i as u8 + 1; 16], delta, 0, delta.len()));
    }
    let (status, body) = client.post("/peer/v3/state", body).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let receipts = frames(&body)
        .iter()
        .map(|(kind, payload)| {
            assert_eq!(*kind, MessageType::StateAdmission);
            match StateAdmission::decode(payload).unwrap().outcome() {
                AdmissionOutcome::Accepted { committed, .. } => committed.clone(),
                other => panic!("native admission required: {other:?}"),
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 3);
    assert!(receipts.iter().all(|commit| *commit == receipts[0]));
    // Two more groups, one per request.
    let mut last = receipts[0].clone();
    for (i, delta) in deltas[3..].iter().enumerate() {
        let (status, body) = client
            .post(
                "/peer/v3/state",
                chunk([i as u8 + 4; 16], delta, 0, delta.len()),
            )
            .await;
        assert_eq!(status, reqwest::StatusCode::OK);
        last = accepted(&body);
    }
    // One page carries every committed group, in commit order.
    let (status, page) = client.get("/peer/v3/events?mode=poll").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let mut sequences = Vec::new();
    for (kind, payload) in frames(&page) {
        if kind == MessageType::CommitBoundary {
            sequences.push(
                CommitBoundary::decode(&payload)
                    .unwrap()
                    .committed()
                    .sequence,
            );
        } else {
            assert_eq!(kind, MessageType::EventChunk);
        }
    }
    assert!(sequences.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(
        *sequences.last().unwrap(),
        last.sequence,
        "the whole suffix in one page"
    );
    assert!(sequences.contains(&receipts[0].sequence));
    let groups = sequences.len();
    assert!(groups >= 3);
    // An old client (product contract only) on the same device: one group per page.
    client.session = None;
    handshake(&mut client, &root).await;
    let mut cursor = None;
    let mut walked = 0;
    loop {
        let boundary = poll(&client, cursor.as_ref())
            .await
            .expect("committed group");
        walked += 1;
        let done = boundary.committed().sequence == last.sequence;
        cursor = Some(boundary.cursor().clone());
        if done {
            break;
        }
    }
    assert_eq!(
        walked, groups,
        "one round trip per group without the extension"
    );
    let _ = stop.send(());
    server.await.unwrap().unwrap();
}

/// A held poll is woken by a commit from another Cloud component (an agent
/// worker's document commit) even when that component still holds the Store
/// lock at the wake: the wake finds the Store busy, and the poll must be
/// retried on the short tick rather than wait for the next unrelated commit
/// or its 20 s idle deadline (the agent Chat delivery stall).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_held_poll_is_woken_by_a_commit_made_under_a_held_lock() {
    let (_transport_root, store_root, config, ready) = deployment().await;
    let root = store_root.path().to_owned();
    let (stop, server, origin) = started(&config, &ready).await;
    let mut client = Client {
        http: reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
        origin,
        key: SigningKey::from_bytes(&[8; 32]),
        session: None,
    };
    handshake(&mut client, &root).await;
    let first = browser_delta(&root, MACHINE, "first ");
    let (status, body) = client
        .post("/peer/v3/state", chunk([1; 16], &first, 0, first.len()))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let first_commit = accepted(&body);
    let mut cursor = None;
    loop {
        let boundary = poll(&client, cursor.as_ref())
            .await
            .expect("committed group");
        cursor = Some(boundary.cursor().clone());
        if boundary.committed().sequence >= first_commit.sequence {
            break;
        }
    }
    let held = {
        let client = Client {
            http: client.http.clone(),
            origin: client.origin.clone(),
            key: client.key.clone(),
            session: client.session,
        };
        let cursor = cursor.clone();
        tokio::spawn(async move { poll(&client, cursor.as_ref()).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Another component commits and keeps the lock across several service
    // ticks: every wake in that window finds the Store busy.
    let later = browser_delta(&root, MACHINE, "under lock ");
    let device = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
    let released = tokio::task::spawn_blocking(move || {
        let mut store = crate::delivery::access::open(&root, MACHINE).unwrap();
        store
            .admit_delivery_item(device, [2; 16], &later, 1024 * 1024)
            .unwrap();
        std::thread::sleep(Duration::from_millis(900));
        drop(store);
        std::time::Instant::now()
    })
    .await
    .unwrap();
    let woken = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .expect("held poll woken after the lock is released, not at its idle deadline")
        .unwrap()
        .expect("committed group");
    let latency = released.elapsed();
    assert!(woken.committed().sequence > first_commit.sequence);
    assert!(
        latency < Duration::from_secs(2),
        "held poll answered {latency:?} after the lock release"
    );
    let _ = stop.send(());
    server.await.unwrap().unwrap();
}

/// Lane D wake-on-commit: a commit by another Cloud component (an agent
/// worker, retention) wakes a held poll right after its durability barrier.
/// The idle tick is disabled (60 s), so without the commit signal the poll
/// would wait for its idle deadline; a single commit is answered at once,
/// with no batching delay.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_https_held_poll_is_woken_at_once_by_another_components_commit() {
    let (_transport_root, store_root, config, ready) = deployment().await;
    let root = store_root.path().to_owned();
    super::quiet_ticks::quiet(&root);
    let (stop, server, origin) = started(&config, &ready).await;
    let mut client = Client {
        http: reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap(),
        origin,
        key: SigningKey::from_bytes(&[8; 32]),
        session: None,
    };
    handshake(&mut client, &root).await;
    let first = browser_delta(&root, MACHINE, "first ");
    let (status, body) = client
        .post("/peer/v3/state", chunk([1; 16], &first, 0, first.len()))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let first_commit = accepted(&body);
    let mut cursor = None;
    loop {
        let boundary = poll(&client, cursor.as_ref())
            .await
            .expect("committed group");
        cursor = Some(boundary.cursor().clone());
        if boundary.committed().sequence >= first_commit.sequence {
            break;
        }
    }
    let held = {
        let client = Client {
            http: client.http.clone(),
            origin: client.origin.clone(),
            key: client.key.clone(),
            session: client.session,
        };
        let cursor = cursor.clone();
        tokio::spawn(async move { poll(&client, cursor.as_ref()).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let later = browser_delta(&root, MACHINE, "out of band ");
    let device = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
    let committed = tokio::task::spawn_blocking(move || {
        let mut store = crate::delivery::access::open(&root, MACHINE).unwrap();
        store
            .admit_delivery_item(device, [2; 16], &later, 1024 * 1024)
            .unwrap();
        drop(store);
        std::time::Instant::now()
    })
    .await
    .unwrap();
    let woken = tokio::time::timeout(Duration::from_secs(5), held)
        .await
        .expect("held poll woken by the commit, not at its idle deadline")
        .unwrap()
        .expect("committed group");
    let latency = committed.elapsed();
    assert!(woken.committed().sequence > first_commit.sequence);
    assert!(
        latency < Duration::from_millis(1000),
        "held poll answered {latency:?} after the commit"
    );
    let _ = stop.send(());
    server.await.unwrap().unwrap();
}

/// A Store lock held by another Cloud component costs a serving turn only
/// the short in-line wait (fix plan B3): the action is not run, the refusal
/// is deferrable (the service retries it on its short tick instead of
/// stalling the loop for seconds), and a turn after the release proceeds.
#[tokio::test]
async fn cloud_busy_store_turn_returns_quickly_and_is_deferrable() {
    use super::{
        CloudStoreSource, DeliveryOperation, OwnerBusyAfterEffect, deferrable, transient, with_turn,
    };
    let (_transport_dir, transport) = crate::https_owner::prepared_transport_test().await;
    let pool = transport.host_bytes().unwrap();
    let (dir, mut store, _) = crate::agent_owner_tests::enrolled();
    store
        .activate_delivery_storage(qos_relay::clock::now_ns())
        .unwrap();
    let source = CloudStoreSource::bind(&store).unwrap();
    drop(store);
    let held = crate::delivery::access::open(dir.path(), "agent-admission").unwrap();
    let started = std::time::Instant::now();
    let mut ran = false;
    let busy = with_turn(&source, &pool, DeliveryOperation::Replay, |_| {
        ran = true;
        Ok(())
    })
    .unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_millis(200));
    assert!(!ran, "a busy turn never runs its action");
    assert!(deferrable(&busy) && transient(&busy));
    // Busy after an earlier committed part: retryable 503, never re-dispatched.
    let partial = busy.context(OwnerBusyAfterEffect);
    assert!(transient(&partial) && !deferrable(&partial));
    drop(held);
    with_turn(&source, &pool, DeliveryOperation::Replay, |_| Ok(())).unwrap();
    assert!(super::ops_log::admit("test-reason").is_some());
    assert!(
        super::ops_log::admit("test-reason").is_none(),
        "rate limited"
    );
}
