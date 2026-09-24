//! Installed data routes end to end: loopback TLS, the real installed ingress,
//! a real attached owner and an actual reopen. Mirrors the browser's chunking.
use super::{tests::Client, *};
use crate::{delivery_http::tests::Fixture, owner::delivery::NativeDeliveryAuthority};
use hyper::StatusCode;
use rustls::pki_types::PrivatePkcs8KeyDer;
use std::future::Future;
use tilo_relay::delivery_v3::{
    auth,
    capabilities::PRODUCT_DATA_V1,
    descriptor::GenerationExpectation,
    framing::{self, MessageType},
    state::{
        AdmissionOutcome, CommitBoundary, CursorAck, EventChunk, FlowCredit, StateAdmission,
        StateItemChunk,
    },
};
use ui::automerge::{ROOT, ReadDoc, transaction::Transactable};

const CHUNK: usize = 240 * 1024;

pub(super) struct Harness {
    pub(super) f: Fixture,
    pub(super) ingress: InstalledIngress,
    pub(super) client: Client,
    pub(super) session: ([u8; 16], [u8; 32]),
    /// The ingress leaf certificate (pinned by native WebTransport clients).
    pub(super) certificate: rustls::pki_types::CertificateDer<'static>,
    handoffs: usize,
    handoff_ranges: usize,
}

fn at(now: u64) -> web_time::SystemTime {
    web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_nanos(now)
}

/// Serve the actor side until the client future completes: accept requests,
/// and run owner maintenance so handed-off work and held polls complete.
pub(super) async fn drive<T>(
    ingress: &mut InstalledIngress,
    owner: &mut SessionOwner,
    transport: impl Future<Output = T>,
) -> T {
    tokio::pin!(transport);
    let mut tick = tokio::time::interval(Duration::from_millis(5));
    loop {
        tokio::select! {
            out = &mut transport => return out,
            request = ingress.recv() => {
                if let Some(request) = request {
                    ingress.handle(owner, request);
                }
            }
            _ = tick.tick() => ingress.maintain(owner),
        }
    }
}

fn client_proof(f: &Fixture, server: &auth::ServerProof, nonce: [u8; 32]) -> auth::ClientProof {
    let now = tilo_relay::clock::now_ns();
    let guard = f.current.read().unwrap();
    let descriptor = f
        .owner
        .verified_delivery_transport(guard.view(), at(now), PRODUCT_DATA_V1)
        .unwrap();
    let authority = f.owner.transport_authority(
        guard.peer(),
        GenerationExpectation::Current {
            generation: descriptor.generation(),
            descriptor_digest: descriptor.descriptor_digest(),
        },
        at(now),
        PRODUCT_DATA_V1,
    );
    let scope = descriptor
        .client_scope(
            &authority,
            &f.owner.identity.native_cert,
            auth::Carrier::Https,
            PRODUCT_DATA_V1,
            PRODUCT_DATA_V1,
        )
        .unwrap();
    let native = NativeDeliveryAuthority {
        view: guard.view(),
        workspace: f.owner.workspace_id(),
        host: f.owner.host_certificate(),
    };
    let revocations = tilo_user_identity::RevocationList::new();
    auth::verify_server(
        &auth::ClientExpectations {
            scope,
            capabilities: auth::CapabilityPolicy {
                supported: PRODUCT_DATA_V1,
                allowed: PRODUCT_DATA_V1,
            },
            credentials: auth::Credentials {
                device: &f.owner.identity.native_cert,
                host: f.owner.host_certificate(),
                device_revocations: &revocations,
                host_revocations: &revocations,
            },
            authority: &native,
            now_ns: now,
        },
        nonce,
        server,
    )
    .unwrap()
    .client_proof(&f.owner.identity.native)
    .unwrap()
}

impl Harness {
    pub(super) async fn start() -> Self {
        Self::serve(Fixture::new()).await
    }
    /// Actual host restart on the same owner root: stop the listener, close
    /// and reopen the owner, then install a fresh ingress (new process-local
    /// registry, host work inventory and sessions) and authenticate again.
    pub(super) async fn restart(self) -> Self {
        let Harness { mut f, ingress, .. } = self;
        ingress.shutdown().await.unwrap();
        let path = f.owner.root().to_owned();
        let placeholder_root = tempfile::tempdir().unwrap();
        let placeholder = SessionOwner::initialize(&placeholder_root.path().join("owner")).unwrap();
        drop(std::mem::replace(&mut f.owner, placeholder));
        f.owner = SessionOwner::open(&path).unwrap();
        f.current = crate::committed_authority::CommittedAuthority::new(&f.owner);
        f.now = tilo_relay::clock::now_ns();
        Self::serve(f).await
    }
    async fn serve(mut f: Fixture) -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let origin = format!("https://127.0.0.1:{}", address.port());
        let now = f.now;
        f.current
            .with_owner(&mut f.owner, |owner| {
                owner.install_delivery_transport(&origin, PRODUCT_DATA_V1, at(now))
            })
            .unwrap()
            .unwrap();
        // Data turns reserve on the ingress pool: attach before serving.
        let channels = crate::https::try_ingress_channel().unwrap();
        let pool = channels.0.host_bytes();
        f.current
            .with_owner(&mut f.owner, |owner| owner.attach_data_resources(&pool))
            .unwrap()
            .unwrap();
        // As the production host does: descriptor refresh is not yet scoped.
        f.owner.permit_unscoped_legacy_commits().unwrap();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
            .unwrap();
        let certificate = cert.cert.der().clone();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate.clone()).unwrap();
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let assets = tempfile::tempdir().unwrap();
        std::fs::write(assets.path().join("index.html"), "isolated").unwrap();
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()));
        let endpoint = endpoint(address, vec![certificate.clone()], key.clone_key()).unwrap();
        let pinned = certificate.clone();
        let config = IngressConfig {
            origin,
            assets: AssetRoot::open(assets.path()).unwrap(),
            certificates: vec![certificate],
            key,
            local_enrollment: None,
        };
        let ingress = InstalledIngress::start_bound(
            &mut f.owner,
            f.current.clone(),
            listener,
            config,
            endpoint,
            channels,
        )
        .await
        .unwrap();
        let mut harness = Self {
            f,
            ingress,
            client: Client {
                address,
                tls: Arc::new(tls),
            },
            session: ([0; 16], [0; 32]),
            certificate: pinned,
            handoffs: 0,
            handoff_ranges: 0,
        };
        harness.session = harness.handshake([41; 32]).await;
        harness
    }

    pub(super) async fn call(
        &mut self,
        method: &str,
        target: &str,
        body: Vec<u8>,
        proof: Option<Vec<u8>>,
    ) -> (StatusCode, Vec<u8>) {
        let request = self.client.request_method(method, target, body, proof);
        drive(&mut self.ingress, &mut self.f.owner, request).await
    }

    async fn handshake(&mut self, nonce: [u8; 32]) -> ([u8; 16], [u8; 32]) {
        let device = self.f.owner.identity.native_cert.device_public_key;
        let body =
            postcard::to_stdvec(&(3_u16, device, nonce, PRODUCT_DATA_V1, PRODUCT_DATA_V1)).unwrap();
        let (status, bytes) = self.call("POST", "/peer/v3/challenge", body, None).await;
        assert_eq!(status, StatusCode::OK);
        let server = auth::ServerProof::decode(&bytes).unwrap();
        let finish = client_proof(&self.f, &server, nonce).encode().unwrap();
        let (status, ready) = self.call("POST", "/peer/v3/sessions", finish, None).await;
        assert_eq!(status, StatusCode::OK);
        let (_, id, digest, _, capabilities): (u16, [u8; 16], [u8; 32], u64, u64) =
            postcard::from_bytes(&ready).unwrap();
        assert_eq!(
            capabilities, PRODUCT_DATA_V1,
            "data session negotiates all 31"
        );
        (id, digest)
    }

    pub(super) fn proof(&self, method: &str, target: &str, body: &[u8]) -> Vec<u8> {
        let now = tilo_relay::clock::now_ns();
        RequestProof::sign(
            self.session.0,
            self.session.1,
            &Request::new(method, target, body).unwrap(),
            now,
            now + 30_000_000_000,
            &self.f.owner.identity.native,
        )
        .unwrap()
        .encode()
        .unwrap()
    }

    pub(super) async fn signed(
        &mut self,
        method: &str,
        target: &str,
        body: Vec<u8>,
    ) -> (StatusCode, Vec<u8>) {
        let proof = self.proof(method, target, &body);
        self.call(method, target, body, Some(proof)).await
    }

    /// One `POST /peer/v3/state` request carrying the given exact range.
    pub(super) async fn chunk(
        &mut self,
        id: [u8; 16],
        canonical: &[u8],
        range: std::ops::Range<usize>,
    ) -> (MessageType, Vec<u8>) {
        let chunk = StateItemChunk::new(
            id,
            *blake3::hash(canonical).as_bytes(),
            canonical.len() as u64,
            range.start as u64,
            &canonical[range],
        )
        .unwrap();
        let (status, bytes) = self
            .signed(
                "POST",
                "/peer/v3/state",
                frame(MessageType::StateItemChunk, &chunk.encode().unwrap()),
            )
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let (kind, payload, rest) = framing::decode_frame_prefix(&bytes).unwrap();
        assert!(rest.is_empty());
        (kind, payload.to_vec())
    }

    /// Upload like the browser: 240 KiB contiguous ranges, FlowCredit between.
    pub(super) async fn upload(
        &mut self,
        id: [u8; 16],
        canonical: &[u8],
    ) -> (StateAdmission, Vec<u64>) {
        let mut offset = 0;
        let mut credits = Vec::new();
        loop {
            let end = (offset + CHUNK).min(canonical.len());
            match self.chunk(id, canonical, offset..end).await {
                (MessageType::FlowCredit, payload) => {
                    let credit = FlowCredit::decode(&payload).unwrap();
                    assert!(credit.next_offset() as usize > offset, "stalled upload");
                    offset = credit.next_offset() as usize;
                    credits.push(credit.next_offset());
                }
                (MessageType::StateAdmission, payload) => {
                    return (StateAdmission::decode(&payload).unwrap(), credits);
                }
                (kind, _) => panic!("unexpected upload reply {kind:?}"),
            }
        }
    }
}

pub(super) fn frame(kind: MessageType, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    framing::append_frame(&mut out, kind, payload, 1024 * 1024).unwrap();
    out
}

pub(super) fn native_edit(owner: &SessionOwner, key: &str, value: Vec<u8>) -> Vec<u8> {
    let mut author = owner.peer().clone();
    let mut document =
        ui::automerge::AutoCommit::load(&author.document("so.tilo.chat", "").unwrap()).unwrap();
    let heads = document.get_heads();
    document
        .put(ROOT, key, ui::automerge::ScalarValue::Bytes(value))
        .unwrap();
    document.commit();
    let message = author
        .publish_incremental(
            "so.tilo.chat",
            "",
            document.save_after(&heads),
            &owner.identity.native,
            web_time::SystemTime::now(),
        )
        .unwrap();
    postcard::to_stdvec(&message).unwrap()
}

pub(super) fn committed(admission: &StateAdmission) -> tilo_relay::delivery_v3::commit::CommitRef {
    match admission.outcome() {
        AdmissionOutcome::Accepted { committed, .. }
        | AdmissionOutcome::Duplicate { committed, .. } => committed.clone(),
        outcome => panic!("not natively admitted: {outcome:?}"),
    }
}

/// Decode one complete replay page: its event chunks and final boundary.
pub(super) fn page(bytes: &[u8]) -> (Vec<EventChunk>, CommitBoundary) {
    let mut rest = bytes;
    let mut events = Vec::new();
    loop {
        let (kind, payload, tail) = framing::decode_frame_prefix(rest).unwrap();
        rest = tail;
        match kind {
            MessageType::EventChunk => events.push(EventChunk::decode(payload).unwrap()),
            MessageType::CommitBoundary => {
                assert!(rest.is_empty(), "boundary ends the page");
                return (events, CommitBoundary::decode(payload).unwrap());
            }
            kind => panic!("unexpected replay frame {kind:?}"),
        }
    }
}

/// Reassemble each native state event of one group from its ordered chunks.
fn state_events(events: &[EventChunk]) -> Vec<Vec<u8>> {
    let mut out: std::collections::BTreeMap<u32, Vec<u8>> = Default::default();
    for event in events {
        if event.kind() != tilo_relay::delivery_v3::state::EventKind::State {
            continue;
        }
        let bytes = out.entry(event.ordinal()).or_default();
        assert_eq!(event.offset(), bytes.len() as u64);
        bytes.extend_from_slice(event.bytes());
    }
    out.into_values().collect()
}

pub(super) fn poll_target(boundary: Option<&CommitBoundary>) -> String {
    match boundary {
        Some(boundary) => format!(
            "/peer/v3/events?cursor={}&mode=poll",
            tilo_relay::delivery_v3::state::encode_cursor_url(boundary.cursor().as_bytes())
                .unwrap()
        ),
        None => "/peer/v3/events?mode=poll".into(),
    }
}

#[tokio::test]
async fn installed_https_data_upload_poll_ack_and_owner_reopen() {
    let mut h = Harness::start().await;
    let device = h.f.owner.identity.native_cert.device_public_key;
    assert!(
        h.f.owner
            .delivery_ledger()
            .unwrap()
            .consumers
            .contains_key(&device),
        "data session registers its durable consumer before Ready"
    );

    // Small item: one complete offset-zero chunk, inline admission.
    let small = native_edit(&h.f.owner, "small", vec![7; 64]);
    let (first, credits) = h.upload([1; 16], &small).await;
    assert!(credits.is_empty());
    assert!(matches!(first.outcome(), AdmissionOutcome::Accepted { .. }));
    let generation = h.f.owner.committed_generation();
    // Lost reply: the identical complete range returns the original receipt.
    let (_, probe) = h.chunk([1; 16], &small, 0..small.len()).await;
    assert_eq!(StateAdmission::decode(&probe).unwrap(), first);
    assert_eq!(h.f.owner.committed_generation(), generation);

    // Large item: staged on disk across three chunks, then one native admission.
    let mut value = vec![0; 560 * 1024];
    blake3::Hasher::new()
        .update(b"resumable installed upload")
        .finalize_xof()
        .fill(&mut value);
    let large = native_edit(&h.f.owner, "large", value.clone());
    assert!(large.len() > 2 * CHUNK);
    let (second, credits) = h.upload([2; 16], &large).await;
    assert_eq!(credits, vec![CHUNK as u64, 2 * CHUNK as u64]);
    assert!(matches!(
        second.outcome(),
        AdmissionOutcome::Accepted { .. }
    ));
    assert_eq!(h.f.owner.committed_generation(), generation + 1);
    // Receipt probe: the final range again, after staging completed.
    let tail = (large.len() - 1) / CHUNK * CHUNK;
    let (kind, probe) = h.chunk([2; 16], &large, tail..large.len()).await;
    assert_eq!(kind, MessageType::StateAdmission);
    assert_eq!(StateAdmission::decode(&probe).unwrap(), second);
    assert_eq!(h.f.owner.committed_generation(), generation + 1);
    // A resumed duplicate prefix still yields credit, never a receipt.
    let (kind, _) = h.chunk([3; 16], &large, 0..CHUNK).await;
    assert_eq!(kind, MessageType::FlowCredit);
    let (kind, _) = h.chunk([3; 16], &large, 0..CHUNK).await;
    assert_eq!(kind, MessageType::FlowCredit);

    // Poll replays each committed group once, in order, with a signed cursor.
    // Earlier owner history (activation, governance) precedes our uploads.
    let mut previous: Option<CommitBoundary> = None;
    let (events, one) = loop {
        let (status, bytes) = h
            .signed("GET", &poll_target(previous.as_ref()), vec![])
            .await;
        assert_eq!(status, StatusCode::OK);
        let (events, boundary) = page(&bytes);
        if let Some(previous) = &previous {
            assert!(boundary.committed().sequence > previous.committed().sequence);
        }
        assert!(boundary.committed().sequence <= committed(&first).sequence);
        if boundary.committed() == &committed(&first) {
            break (events, boundary);
        }
        previous = Some(boundary);
    };
    assert!(state_events(&events).iter().any(|e| e.len() >= 64));
    let (status, bytes) = h.signed("GET", &poll_target(Some(&one)), vec![]).await;
    assert_eq!(status, StatusCode::OK);
    let (events, two) = page(&bytes);
    assert_eq!(two.committed(), &committed(&second));
    assert!(state_events(&events).iter().any(|e| e.len() > 2 * CHUNK));

    // ACK the second group: 200 with an empty body.
    let ack = frame(
        MessageType::CursorAck,
        &CursorAck::new(two.cursor().clone()).encode().unwrap(),
    );
    let (status, body) = h.signed("POST", "/peer/v3/acks", ack).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());

    // A poll past the tail is held, then answered by the next commit.
    let third = native_edit(&h.f.owner, "third", vec![9; 32]);
    let held_target = poll_target(Some(&two));
    let held_proof = h.proof("GET", &held_target, &[]);
    let chunk = StateItemChunk::new(
        [4; 16],
        *blake3::hash(&third).as_bytes(),
        third.len() as u64,
        0,
        &third,
    )
    .unwrap();
    let state_body = frame(MessageType::StateItemChunk, &chunk.encode().unwrap());
    let state_proof = h.proof("POST", "/peer/v3/state", &state_body);
    let client = Client {
        address: h.client.address,
        tls: h.client.tls.clone(),
    };
    let both = async {
        let poll = client.request_method("GET", &held_target, vec![], Some(held_proof));
        let state = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            client
                .request_method("POST", "/peer/v3/state", state_body, Some(state_proof))
                .await
        };
        tokio::join!(poll, state)
    };
    let ((poll_status, poll_bytes), (state_status, state_bytes)) =
        drive(&mut h.ingress, &mut h.f.owner, both).await;
    assert_eq!(state_status, StatusCode::OK);
    assert_eq!(poll_status, StatusCode::OK);
    let (_, admission, _) = framing::decode_frame_prefix(&state_bytes).unwrap();
    let admission = StateAdmission::decode(admission).unwrap();
    let (_, three) = page(&poll_bytes);
    assert_eq!(three.committed(), &committed(&admission));

    // Detach: 200 with an empty body; the session then refuses further use.
    let target = format!("/peer/v3/sessions/{}", hex::encode(h.session.0));
    let (status, body) = h.signed("DELETE", &target, vec![]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_empty());
    let (status, body) = h.signed("GET", &poll_target(None), vec![]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body, b"permission-denied");

    // Actual reopen: every admitted item and the durable consumer persist.
    let Harness { f, ingress, .. } = h;
    ingress.shutdown().await.unwrap();
    let path = f.owner.root().to_owned();
    drop(f.owner);
    let reopened = SessionOwner::open(&path).unwrap();
    let document =
        ui::automerge::AutoCommit::load(&reopened.peer().document("so.tilo.chat", "").unwrap())
            .unwrap();
    for key in ["small", "large", "third"] {
        assert!(
            document.get(ROOT, key).unwrap().is_some(),
            "{key} persisted"
        );
    }
    let ledger = reopened.delivery_ledger().unwrap();
    assert!(ledger.consumers.contains_key(&device));
    assert!(ledger.replay.contains_key(&committed(&admission).sequence));
}

/// A member admission changes the authorization view. The existing consumer's
/// cursor must restart: the poll answers the contract's SnapshotRequired reset
/// (never an opaque denial), so the client can renew and take a fresh baseline.
#[tokio::test]
async fn installed_https_poll_answers_authorization_change_with_snapshot_reset() {
    let mut h = Harness::start().await;
    let small = native_edit(&h.f.owner, "before-member", vec![5; 64]);
    let (first, _) = h.upload([9; 16], &small).await;
    let mut previous: Option<CommitBoundary> = None;
    let current = loop {
        let (status, bytes) = h
            .signed("GET", &poll_target(previous.as_ref()), vec![])
            .await;
        assert_eq!(status, StatusCode::OK);
        let (_, boundary) = page(&bytes);
        if boundary.committed() == &committed(&first) {
            break boundary;
        }
        previous = Some(boundary);
    };

    let key = ed25519_dalek::SigningKey::from_bytes(&[77; 32]);
    let mut certificate = h.f.owner.identity.native_cert.clone();
    certificate.device_public_key = key.verifying_key().to_bytes();
    certificate.device_label = "second member".into();
    tilo_user_identity::sign_device_cert(&mut certificate, &h.f.owner.identity.root).unwrap();
    h.f.current
        .with_owner(&mut h.f.owner, |owner| {
            let mut candidate = owner.peer().clone();
            candidate
                .admit_certificate(
                    certificate,
                    &owner.identity.native,
                    web_time::SystemTime::now(),
                )
                .unwrap();
            owner.commit_admission(candidate, owner.directory().to_vec())
        })
        .unwrap()
        .unwrap();

    let (status, bytes) = h.signed("GET", &poll_target(Some(&current)), vec![]).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "reset is a served response, not a denial"
    );
    let (kind, payload, rest) = framing::decode_frame_prefix(&bytes).unwrap();
    assert_eq!(kind, MessageType::SnapshotRequired);
    assert!(rest.is_empty(), "the reset is the only frame");
    let reset = tilo_relay::delivery_v3::state::SnapshotRequired::decode(payload).unwrap();
    assert_eq!(
        reset.reason(),
        tilo_relay::delivery_v3::state::ResetReason::AuthorizationChanged
    );
    assert_eq!(
        reset.store_generation(),
        h.f.owner.delivery_ledger().unwrap().store_generation
    );
}

fn snapshot_body(operation: tilo_relay::delivery_v3::state::SnapshotOperation) -> Vec<u8> {
    frame(
        MessageType::SnapshotManifest,
        &tilo_relay::delivery_v3::state::SnapshotRequest::new(operation)
            .unwrap()
            .encode()
            .unwrap(),
    )
}

#[tokio::test]
async fn installed_https_snapshot_baseline_chunks_replay_and_renewal() {
    use tilo_relay::delivery_v3::state::{
        SnapshotChunk, SnapshotKind, SnapshotManifest, SnapshotOperation, SnapshotRequired,
        TransferRecordHeader,
    };
    let mut h = Harness::start().await;
    let host =
        ed25519_dalek::VerifyingKey::from_bytes(&h.f.owner.host_certificate().device_public_key)
            .unwrap();
    let device = h.f.owner.identity.native_cert.device_public_key;
    let small = native_edit(&h.f.owner, "before", vec![5; 64]);
    let (first, _) = h.upload([1; 16], &small).await;
    let mut value = vec![0; 400 * 1024];
    blake3::Hasher::new()
        .update(b"snapshot payload")
        .finalize_xof()
        .fill(&mut value);
    let large = native_edit(&h.f.owner, "pinned", value);
    let (second, _) = h.upload([2; 16], &large).await;

    // Baseline: one signed manifest scoped to this consumer and the head cut.
    let (status, bytes) = h
        .signed(
            "POST",
            "/peer/v3/snapshots",
            snapshot_body(SnapshotOperation::Baseline),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let (kind, payload, rest) = framing::decode_frame_prefix(&bytes).unwrap();
    assert_eq!(kind, MessageType::SnapshotManifest);
    assert!(rest.is_empty());
    let manifest = SnapshotManifest::from_bytes(payload).unwrap();
    let signed = manifest
        .authenticate(&host, tilo_relay::clock::now_ns())
        .unwrap();
    let issue = signed.issue().clone();
    assert_eq!(issue.kind, SnapshotKind::Baseline);
    assert_eq!(issue.scope.consumer_device, device);
    assert_eq!(issue.scope.committed, committed(&second));
    assert!(
        issue.chunk_digests.len() >= 2,
        "stream spans several chunks"
    );

    // Every chunk validates against the manifest; the last carries the boundary.
    let mut stream = Vec::new();
    let mut boundary = None;
    for index in 0..issue.chunk_digests.len() {
        let target = format!(
            "/peer/v3/snapshots/{}/chunks/{index}",
            hex::encode(issue.pin)
        );
        let (status, bytes) = h.signed("GET", &target, vec![]).await;
        assert_eq!(status, StatusCode::OK);
        let (kind, payload, rest) = framing::decode_frame_prefix(&bytes).unwrap();
        assert_eq!(kind, MessageType::SnapshotChunk);
        let chunk = SnapshotChunk::decode(payload).unwrap();
        signed.validate_chunk(&chunk).unwrap();
        stream.extend_from_slice(chunk.bytes());
        if index + 1 == issue.chunk_digests.len() {
            let (kind, payload, tail) = framing::decode_frame_prefix(rest).unwrap();
            assert_eq!(kind, MessageType::CommitBoundary);
            assert!(tail.is_empty());
            boundary = Some(CommitBoundary::decode(payload).unwrap());
        } else {
            assert!(rest.is_empty());
        }
    }
    assert_eq!(stream.len() as u64, issue.stream_bytes);
    let boundary = boundary.unwrap();
    let claims = boundary.cursor().authenticate(&host).unwrap();
    assert_eq!(claims.issue(), &issue.scope);
    assert_eq!(boundary.visible_count(), issue.item_count);
    // The stream is exact records: fixed header, then the canonical bytes.
    let mut offset = 0;
    let mut order = Vec::new();
    while offset < stream.len() {
        let header = TransferRecordHeader::decode(&stream[offset..offset + 45]).unwrap();
        let body = &stream[offset + 45..offset + 45 + header.canonical_size() as usize];
        assert_eq!(blake3::hash(body).as_bytes(), &header.canonical_digest());
        order.push((header.ordinal(), header.canonical_digest()));
        offset += 45 + body.len();
    }
    assert_eq!(order.len() as u32, issue.item_count);
    assert_eq!(
        tilo_relay::delivery_v3::state::visible_order_digest(&order).unwrap(),
        boundary.visible_order_digest()
    );

    // Renewal of the live pin keeps every immutable claim.
    let (status, bytes) = h
        .signed(
            "POST",
            "/peer/v3/snapshots",
            snapshot_body(SnapshotOperation::Renew {
                pin: issue.pin,
                manifest_digest: manifest.digest(),
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (kind, payload, _) = framing::decode_frame_prefix(&bytes).unwrap();
    assert_eq!(kind, MessageType::SnapshotManifest);
    let renewed = SnapshotManifest::from_bytes(payload)
        .unwrap()
        .authenticate(&host, tilo_relay::clock::now_ns())
        .unwrap();
    assert!(renewed.same_content(&signed));
    assert!(renewed.issue().expires_at_ns >= issue.expires_at_ns);

    // An unknown pin (e.g. after a host restart) requires a fresh snapshot.
    let (status, bytes) = h
        .signed(
            "POST",
            "/peer/v3/snapshots",
            snapshot_body(SnapshotOperation::Renew {
                pin: [9; 16],
                manifest_digest: [9; 32],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (kind, payload, _) = framing::decode_frame_prefix(&bytes).unwrap();
    assert_eq!(kind, MessageType::SnapshotRequired);
    SnapshotRequired::decode(payload).unwrap();

    // Replay-after: the exact next group after a durable cursor.
    let mut previous: Option<CommitBoundary> = None;
    let one = loop {
        let (_, bytes) = h
            .signed("GET", &poll_target(previous.as_ref()), vec![])
            .await;
        let (_, boundary) = page(&bytes);
        if boundary.committed() == &committed(&first) {
            break boundary;
        }
        previous = Some(boundary);
    };
    let (status, bytes) = h
        .signed(
            "POST",
            "/peer/v3/snapshots",
            snapshot_body(SnapshotOperation::ReplayAfter {
                cursor: one.cursor().clone(),
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (kind, payload, _) = framing::decode_frame_prefix(&bytes).unwrap();
    assert_eq!(kind, MessageType::SnapshotManifest);
    let replay = SnapshotManifest::from_bytes(payload)
        .unwrap()
        .authenticate(&host, tilo_relay::clock::now_ns())
        .unwrap();
    assert_eq!(replay.issue().kind, SnapshotKind::Replay);
    assert_eq!(
        replay.issue().predecessor.as_ref(),
        Some(&committed(&first))
    );
    assert_eq!(replay.issue().scope.committed, committed(&second));

    // A pin is private to its device: a foreign pin id is refused.
    let target = format!("/peer/v3/snapshots/{}/chunks/0", hex::encode([7u8; 16]));
    let (status, _) = h.signed("GET", &target, vec![]).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    h.ingress.shutdown().await.unwrap();
}

#[tokio::test]
async fn installed_https_private_catalogue_over_actual_action_runtime() {
    use crate::{
        action_host::{ActionHost, Authority},
        host_signing::HostSigning,
        owner_commit::OwnerCommitMailbox,
    };
    use tilo_relay::{
        delivery_v3::private_wire::{self, PrivateOperation, PrivateScope},
        peer_actions::{ActionMessage, DirectedAction},
    };
    let mut h = Harness::start().await;
    let catalogue = PrivateOperation::Catalogue;
    let action = DirectedAction {
        room: h.f.owner.peer().room().parse().unwrap(),
        request_id: uuid::Uuid::new_v4(),
        source: h.f.owner.identity.native_cert.device_public_key,
        target: h.f.owner.peer().authority().device_public_key,
        message: ActionMessage::CatalogueRefresh { scope: None },
    };
    let body = private_wire::encode_request(
        catalogue,
        PrivateScope {
            room: action.room,
            source: action.source,
            target: action.target,
        },
        &action,
    )
    .unwrap();
    // Without an installed action runtime the route refuses.
    let (status, reply) = h.signed("POST", catalogue.path(), body.clone()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(reply, b"permission-denied");

    let authority = Authority::new(h.f.current.clone());
    authority.set_ready(true);
    let (mailbox, _commits) = OwnerCommitMailbox::bounded(8);
    let (signing, _signatures) = HostSigning::new(mailbox);
    let (sender, _network) = tokio::sync::mpsc::channel(32);
    let mut actions = ActionHost::start(&h.f.owner, authority, signing, sender).unwrap();
    h.ingress.sync_actions(Some(&actions));

    // The broker reply is asynchronous; the parked original is answered.
    let (status, reply) = h.signed("POST", catalogue.path(), body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&reply)
    );
    let response = private_wire::decode_response(catalogue, &action, &reply).unwrap();
    assert!(matches!(
        response.message,
        ActionMessage::BrokerCatalogue { .. }
    ));

    // A private events poll with nothing ready is held, not answered empty.
    let target = private_wire::PRIVATE_EVENTS_TARGET;
    let proof = h.proof("GET", target, &[]);
    let client = Client {
        address: h.client.address,
        tls: h.client.tls.clone(),
    };
    let held = drive(&mut h.ingress, &mut h.f.owner, async {
        tokio::time::timeout(
            Duration::from_millis(500),
            client.request_method("GET", target, vec![], Some(proof)),
        )
        .await
    })
    .await;
    assert!(held.is_err(), "idle private poll must be held");

    // Retiring the runtime closes the carrier: routes refuse again.
    h.ingress.sync_actions(None);
    let action = DirectedAction {
        request_id: uuid::Uuid::new_v4(),
        ..action
    };
    let body = private_wire::encode_request(
        catalogue,
        PrivateScope {
            room: action.room,
            source: action.source,
            target: action.target,
        },
        &action,
    )
    .unwrap();
    let (status, _) = h.signed("POST", catalogue.path(), body).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    actions.close(&h.f.owner).unwrap();
    h.ingress.shutdown().await.unwrap();
}

/// One joining device: its own root, a member certificate, and a signer for
/// the onboarding possession proof.
struct Joiner {
    device: ed25519_dalek::SigningKey,
    certificate: tilo_user_identity::DeviceCert,
}
impl Joiner {
    fn new(seed: u8, now: u64) -> Self {
        use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
        let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[seed + 1; 32]);
        let mut certificate = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "HTTPS joiner".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now - 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut certificate, &root).unwrap();
        Self {
            device,
            certificate,
        }
    }
}

impl Harness {
    /// Signed onboarding call through the installed route and actual owner.
    async fn onboard(
        &mut self,
        host: &crate::host::LocalHost,
        joiner: &Joiner,
        inner: qos_peer_state::join::JoinTransportRequest,
        nonce: u8,
    ) -> qos_peer_state::join::JoinTransportResponse {
        use ed25519_dalek::Signer;
        use qos_peer_state::join::JoinTransportRequest as J;
        use tilo_relay::delivery_v3::{
            media_wire::split_signature,
            onboarding_wire::{
                OnboardingOperation, OnboardingRequest, OnboardingResponse, OnboardingScope,
                ReplyBodyKind,
            },
        };
        let now = tilo_relay::clock::now_ns();
        let origin = self.ingress.origin().to_owned();
        let body = postcard::to_stdvec(&inner).unwrap();
        let certificate = postcard::to_stdvec(&joiner.certificate).unwrap();
        let mut request = OnboardingRequest {
            version: 3,
            operation: match inner {
                J::Inspect { .. } => OnboardingOperation::Inspect,
                J::Challenge(_) => OnboardingOperation::Challenge,
                J::Complete(_) => OnboardingOperation::Complete,
                J::MembershipChallenge(_) => OnboardingOperation::MembershipChallenge,
                J::MembershipComplete(_) => OnboardingOperation::MembershipComplete,
            },
            scope: OnboardingScope {
                origin: &origin,
                host: self.f.owner.host_certificate().device_public_key,
                workspace: self.f.owner.workspace_id(),
                root: self.f.owner.peer().authority().user_public_key,
                room: self.f.owner.peer().room().parse().unwrap(),
            },
            certificate: &certificate,
            nonce: [nonce; 32],
            issued_at_ns: now,
            expires_at_ns: now + 60_000_000_000,
            body_digest: *blake3::hash(&body).as_bytes(),
            body: &body,
            signature: [[0; 32]; 2],
        };
        request.signature = split_signature(
            joiner
                .device
                .sign(&request.signing_digest().unwrap())
                .to_bytes(),
        );
        let wire = request.encode().unwrap();
        let (status, bytes) = {
            let transport =
                self.client
                    .request_method("POST", request.operation.path(), wire, None);
            let ingress = &mut self.ingress;
            let owner = &mut self.f.owner;
            tokio::pin!(transport);
            loop {
                tokio::select! {
                    out = &mut transport => break out,
                    incoming = ingress.recv() => {
                        let incoming = incoming.unwrap();
                        assert!(matches!(incoming.route(), Route::Onboarding(_)));
                        ingress.onboard(owner, host, incoming).await;
                    }
                }
            }
        };
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let response = OnboardingResponse::decode(&bytes).unwrap();
        response
            .verify_host_signature(&request, tilo_relay::clock::now_ns())
            .unwrap();
        match response.body_kind {
            ReplyBodyKind::InlineLegacy => postcard::from_bytes(response.body).unwrap(),
            ReplyBodyKind::HandoffManifest => {
                let manifest = tilo_relay::delivery_v3::onboarding_wire::HandoffManifest::decode(
                    response.body,
                )
                .unwrap();
                manifest
                    .verify_host_signature(
                        request.scope,
                        joiner.certificate.device_public_key,
                        request.request_digest().unwrap(),
                        request.nonce,
                        tilo_relay::clock::now_ns(),
                    )
                    .unwrap();
                self.handoffs += 1;
                let bytes = self.hydrate(host, joiner, &manifest).await;
                postcard::from_bytes(&bytes).unwrap()
            }
        }
    }
    /// Fetch every signed range of a large join reply through the installed
    /// `/peer/v3/join/handoff` route and verify it against the manifest.
    async fn hydrate(
        &mut self,
        host: &crate::host::LocalHost,
        joiner: &Joiner,
        manifest: &tilo_relay::delivery_v3::onboarding_wire::HandoffManifest<'_>,
    ) -> Vec<u8> {
        use ed25519_dalek::Signer;
        use tilo_relay::delivery_v3::{
            media_wire::split_signature,
            onboarding_wire::{
                HANDOFF_CHUNKS_PATH, HandoffChunk, HandoffRequest, MAX_HANDOFF_RANGE_BYTES,
            },
        };
        let mut bytes = Vec::new();
        while bytes.len() < manifest.total_bytes as usize {
            let now = tilo_relay::clock::now_ns();
            let remaining = manifest.total_bytes as usize - bytes.len();
            let mut fetch = HandoffRequest {
                version: 1,
                manifest_digest: manifest.manifest_digest().unwrap(),
                device: joiner.certificate.device_public_key,
                offset: bytes.len() as u64,
                length: remaining.min(MAX_HANDOFF_RANGE_BYTES) as u32,
                nonce: *blake3::hash(&(bytes.len() as u64).to_le_bytes()).as_bytes(),
                issued_at_ns: now,
                expires_at_ns: now + 20_000_000_000,
                signature: [[0; 32]; 2],
            };
            fetch.signature = split_signature(
                joiner
                    .device
                    .sign(&fetch.signing_digest().unwrap())
                    .to_bytes(),
            );
            let transport = self.client.request_method(
                "POST",
                HANDOFF_CHUNKS_PATH,
                fetch.encode().unwrap(),
                None,
            );
            let ingress = &mut self.ingress;
            let owner = &mut self.f.owner;
            tokio::pin!(transport);
            let (status, encoded) = loop {
                tokio::select! {
                    out = &mut transport => break out,
                    incoming = ingress.recv() => {
                        let incoming = incoming.unwrap();
                        assert_eq!(incoming.route(), Route::OnboardingChunk);
                        ingress.onboard(owner, host, incoming).await;
                    }
                }
            };
            assert_eq!(
                status,
                StatusCode::OK,
                "{}",
                String::from_utf8_lossy(&encoded)
            );
            let chunk = HandoffChunk::decode(&encoded).unwrap();
            chunk
                .verify_host_signature(manifest, &fetch, tilo_relay::clock::now_ns())
                .unwrap();
            assert_eq!(chunk.offset, bytes.len() as u64);
            bytes.extend_from_slice(chunk.body);
            self.handoff_ranges += 1;
        }
        assert_eq!(blake3::hash(&bytes).as_bytes(), &manifest.content_digest);
        bytes
    }
}

#[tokio::test]
async fn installed_https_onboarding_invited_and_open_invitation_join() {
    use crate::collaboration::{CollaborationService, IssueInvitationRequest};
    use qos_peer_state::join::*;
    let mut h = Harness::start().await;
    let host = crate::host::LocalHost::start(&h.f.owner).await.unwrap();
    let now = tilo_relay::clock::now_ns();
    let invited = Joiner::new(171, now);
    let fresh = Joiner::new(181, now);

    let generation = h.f.owner.committed_generation();
    h.ingress
        .install_onboarding(CollaborationService::new(vec![]))
        .unwrap();

    let service = CollaborationService::new(vec![]);
    let administrator = h.f.owner.peer().authority().device_public_key;
    for (joiner, recipient, seed) in [
        (&invited, Some(invited.certificate.user_public_key), 10u8),
        (&fresh, None, 40u8),
    ] {
        let invitation =
            h.f.current
                .with_owner(&mut h.f.owner, |owner| {
                    service.issue(
                        owner,
                        administrator,
                        IssueInvitationRequest {
                            request_id: [seed; 32],
                            role: MemberRole::Editor,
                            recipient_user_public_key: recipient,
                            invitee_name: Some("HTTPS joiner".into()),
                            expires_at_ns: now + 120_000_000_000,
                        },
                        now,
                    )
                })
                .unwrap()
                .unwrap();
        assert!(matches!(
            h.onboard(
                &host,
                joiner,
                JoinTransportRequest::Inspect {
                    signed_invitation: invitation.signed_invitation.clone(),
                },
                seed,
            )
            .await,
            JoinTransportResponse::Inspection(_)
        ));
        let JoinTransportResponse::Challenge(challenge) = h
            .onboard(
                &host,
                joiner,
                JoinTransportRequest::Challenge(JoinChallengeRequest {
                    signed_invitation: invitation.signed_invitation,
                    device_cert: joiner.certificate.clone(),
                    request_id: [seed + 1; 32],
                }),
                seed + 1,
            )
            .await
        else {
            panic!("join challenge")
        };
        let proof = sign_join_proof(challenge.challenge, &joiner.device).unwrap();
        let JoinTransportResponse::Complete(joined) = h
            .onboard(
                &host,
                joiner,
                JoinTransportRequest::Complete(JoinCompleteRequest { proof }),
                seed + 2,
            )
            .await
        else {
            panic!("admitted join")
        };
        assert_eq!(joined.device_cert, joiner.certificate);
        let JoinTransportResponse::MembershipChallenge(challenge) = h
            .onboard(
                &host,
                joiner,
                JoinTransportRequest::MembershipChallenge(MembershipChallengeRequest {
                    room: h.f.owner.peer().room().into(),
                    workspace_id: h.f.owner.workspace_id(),
                    device_cert: joiner.certificate.clone(),
                    request_id: [seed + 3; 32],
                }),
                seed + 3,
            )
            .await
        else {
            panic!("membership challenge")
        };
        let proof = sign_membership_proof(challenge.challenge, &joiner.device).unwrap();
        assert!(matches!(
            h.onboard(
                &host,
                joiner,
                JoinTransportRequest::MembershipComplete(MembershipCompleteRequest { proof }),
                seed + 4,
            )
            .await,
            JoinTransportResponse::MembershipComplete(_)
        ));
        assert!(
            h.f.owner
                .peer()
                .permissions(joiner.certificate.device_public_key, at(now))
                .is_ok(),
            "joined device holds current membership"
        );
    }
    assert!(h.f.owner.committed_generation() > generation);
    host.shutdown().await.unwrap();
    h.ingress.shutdown().await.unwrap();
}

/// Every private operation route over the installed listener and the actual
/// action runtime: Status, Cancel and Invoke of an unknown/unavailable target
/// and an agent read without an attached provider are all answered as signed
/// closed replies through the private wire, never a hang or transport error.
#[tokio::test]
async fn installed_https_private_status_cancel_invoke_and_read_over_actual_action_runtime() {
    use crate::{
        action_host::{ActionHost, Authority},
        host_signing::HostSigning,
        owner_commit::OwnerCommitMailbox,
    };
    use tilo_relay::{
        delivery_v3::private_wire::{self, PrivateOperation, PrivateScope},
        peer_actions::{ActionMessage, DirectedAction},
    };
    let mut h = Harness::start().await;
    let authority = Authority::new(h.f.current.clone());
    authority.set_ready(true);
    let (mailbox, _commits) = OwnerCommitMailbox::bounded(8);
    let (signing, _signatures) = HostSigning::new(mailbox);
    let (sender, _network) = tokio::sync::mpsc::channel(32);
    let mut actions = ActionHost::start(&h.f.owner, authority, signing, sender).unwrap();
    h.ingress.sync_actions(Some(&actions));
    let room: uuid::Uuid = h.f.owner.peer().room().parse().unwrap();
    let source = h.f.owner.identity.native_cert.device_public_key;
    let target = h.f.owner.peer().authority().device_public_key;
    let unknown = tilo_actions::InvocationId(uuid::Uuid::new_v4());
    let cases = [
        (
            PrivateOperation::Status,
            ActionMessage::Status {
                invocation: unknown,
            },
        ),
        (
            PrivateOperation::Cancel,
            ActionMessage::Cancel {
                invocation: unknown,
            },
        ),
        (
            PrivateOperation::Invoke,
            ActionMessage::InvokeBound {
                host: room,
                invocation: tilo_actions::InvocationId(uuid::Uuid::new_v4()),
                app_peer: h.f.owner.workspace_id().to_string(),
                app_id: "so.tilo.word-processor".into(),
                record_id: "tab:0000000000000001".into(),
                resource: "document".into(),
                action: "read-text".into(),
                argument: None,
                confirmation: None,
                revision: None,
            },
        ),
        (
            PrivateOperation::AgentRead,
            ActionMessage::AgentReadV1 {
                host: room,
                invocation: uuid::Uuid::new_v4(),
                request: serde_json::from_value(serde_json::json!({
                    "kind": "agent_list",
                    "version": 1,
                    "cursor": null,
                }))
                .unwrap(),
            },
        ),
    ];
    for (operation, message) in cases {
        let action = DirectedAction {
            room,
            request_id: uuid::Uuid::new_v4(),
            source,
            target,
            message,
        };
        let body = private_wire::encode_request(
            operation,
            PrivateScope {
                room: action.room,
                source: action.source,
                target: action.target,
            },
            &action,
        )
        .unwrap();
        let (status, reply) = h.signed("POST", operation.path(), body).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "{operation:?}: {}",
            String::from_utf8_lossy(&reply)
        );
        let response = private_wire::decode_response(operation, &action, &reply).unwrap();
        match (operation, &response.message) {
            (PrivateOperation::Status, ActionMessage::Observation { invocation, .. }) => {
                assert_eq!(*invocation, unknown)
            }
            (_, ActionMessage::Rejected { .. })
            | (PrivateOperation::Cancel, ActionMessage::CancellationRequested { .. })
            | (PrivateOperation::Invoke, ActionMessage::Observation { .. })
            | (PrivateOperation::AgentRead, ActionMessage::AgentReadResultV1 { .. }) => {}
            other => panic!("unexpected private reply: {other:?}"),
        }
    }
    actions.close(&h.f.owner).unwrap();
    h.ingress.shutdown().await.unwrap();
}

/// A join reply above the inline limit (>1 MiB workspace bootstrap) is
/// answered with a signed handoff manifest and hydrated range by range
/// through the installed onboarding chunk route.
#[tokio::test]
async fn installed_https_large_join_reply_hydrates_through_the_chunk_route() {
    use crate::collaboration::{CollaborationService, IssueInvitationRequest};
    use qos_peer_state::join::*;
    let mut h = Harness::start().await;
    {
        use rand::{RngCore, SeedableRng};
        let mut input = vec![0u8; 1_200_000];
        rand::rngs::StdRng::seed_from_u64(9931).fill_bytes(&mut input);
        let text = hex::encode(input);
        h.f.current
            .with_owner(&mut h.f.owner, |owner| {
                crate::owner::mutate_owned_record_for_test(owner, "so.tilo.chat", "", |doc| {
                    doc.put(ROOT, "installed_handoff_fixture", text).unwrap();
                })
            })
            .unwrap()
            .unwrap();
    }
    let host = crate::host::LocalHost::start(&h.f.owner).await.unwrap();
    let now = tilo_relay::clock::now_ns();
    let joiner = Joiner::new(151, now);
    h.ingress
        .install_onboarding(CollaborationService::new(vec![]))
        .unwrap();
    let service = CollaborationService::new(vec![]);
    let administrator = h.f.owner.peer().authority().device_public_key;
    let invitation =
        h.f.current
            .with_owner(&mut h.f.owner, |owner| {
                service.issue(
                    owner,
                    administrator,
                    IssueInvitationRequest {
                        request_id: [61; 32],
                        role: MemberRole::Editor,
                        recipient_user_public_key: Some(joiner.certificate.user_public_key),
                        invitee_name: None,
                        expires_at_ns: now + 120_000_000_000,
                    },
                    now,
                )
            })
            .unwrap()
            .unwrap();
    let JoinTransportResponse::Challenge(challenge) = h
        .onboard(
            &host,
            &joiner,
            JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: invitation.signed_invitation,
                device_cert: joiner.certificate.clone(),
                request_id: [62; 32],
            }),
            62,
        )
        .await
    else {
        panic!("join challenge")
    };
    let proof = sign_join_proof(challenge.challenge, &joiner.device).unwrap();
    let JoinTransportResponse::Complete(bootstrap) = h
        .onboard(
            &host,
            &joiner,
            JoinTransportRequest::Complete(JoinCompleteRequest { proof }),
            63,
        )
        .await
    else {
        panic!("large admitted join")
    };
    assert_eq!(bootstrap.device_cert, joiner.certificate);
    assert_eq!(h.handoffs, 1, "the large reply used a signed handoff");
    assert!(
        h.handoff_ranges >= 2,
        "more than one installed range fetched"
    );
    assert!(
        bootstrap
            .governance_messages
            .iter()
            .map(Vec::len)
            .sum::<usize>()
            > 1024 * 1024
    );
    host.shutdown().await.unwrap();
    h.ingress.shutdown().await.unwrap();
}

/// Snapshot pins survive an actual host restart: a reload Renew of the
/// signed pin rebuilds the transfer from its durable sidecar and the
/// retained history (same pin and immutable content), and every chunk is
/// served again with its boundary.
#[tokio::test]
async fn installed_https_snapshot_pin_renews_after_actual_host_restart() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    use tilo_relay::delivery_v3::state::{
        SnapshotChunk, SnapshotManifest, SnapshotOperation, SnapshotRequired,
    };
    let mut h = Harness::start().await;
    let host =
        ed25519_dalek::VerifyingKey::from_bytes(&h.f.owner.host_certificate().device_public_key)
            .unwrap();
    let small = native_edit(&h.f.owner, "restart-small", vec![3; 64]);
    h.upload([1; 16], &small).await;
    let mut value = vec![0; 300 * 1024];
    blake3::Hasher::new()
        .update(b"restart snapshot payload")
        .finalize_xof()
        .fill(&mut value);
    let large = native_edit(&h.f.owner, "restart-large", value);
    let (second, _) = h.upload([2; 16], &large).await;
    let (status, bytes) = h
        .signed(
            "POST",
            "/peer/v3/snapshots",
            snapshot_body(SnapshotOperation::Baseline),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (kind, payload, _) = framing::decode_frame_prefix(&bytes).unwrap();
    assert_eq!(kind, MessageType::SnapshotManifest);
    let manifest = SnapshotManifest::from_bytes(payload).unwrap();
    let signed = manifest
        .authenticate(&host, tilo_relay::clock::now_ns())
        .unwrap();
    let issue = signed.issue().clone();
    assert_eq!(issue.scope.committed, committed(&second));

    let mut h = h.restart().await;
    // A commit after the restart extends the tail; the pinned cut is unchanged.
    let later = native_edit(&h.f.owner, "restart-later", vec![4; 64]);
    h.upload([3; 16], &later).await;
    let renew = |digest| {
        snapshot_body(SnapshotOperation::Renew {
            pin: issue.pin,
            manifest_digest: digest,
        })
    };
    let (status, bytes) = h
        .signed("POST", "/peer/v3/snapshots", renew(manifest.digest()))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (kind, payload, _) = framing::decode_frame_prefix(&bytes).unwrap();
    assert_eq!(
        kind,
        MessageType::SnapshotManifest,
        "restart recovery renews instead of SnapshotRequired"
    );
    let renewed_manifest = SnapshotManifest::from_bytes(payload).unwrap();
    let renewed = renewed_manifest
        .authenticate(&host, tilo_relay::clock::now_ns())
        .unwrap();
    assert!(renewed.same_content(&signed));
    assert_eq!(renewed.issue().pin, issue.pin);
    assert!(renewed.issue().expires_at_ns >= issue.expires_at_ns);
    let mut boundary = None;
    for index in 0..issue.chunk_digests.len() {
        let target = format!(
            "/peer/v3/snapshots/{}/chunks/{index}",
            hex::encode(issue.pin)
        );
        let (status, bytes) = h.signed("GET", &target, vec![]).await;
        assert_eq!(status, StatusCode::OK);
        let (kind, payload, rest) = framing::decode_frame_prefix(&bytes).unwrap();
        assert_eq!(kind, MessageType::SnapshotChunk);
        renewed
            .validate_chunk(&SnapshotChunk::decode(payload).unwrap())
            .unwrap();
        if index + 1 == issue.chunk_digests.len() {
            let (kind, payload, _) = framing::decode_frame_prefix(rest).unwrap();
            assert_eq!(kind, MessageType::CommitBoundary);
            boundary = Some(CommitBoundary::decode(payload).unwrap());
        }
    }
    assert_eq!(boundary.unwrap().committed(), &issue.scope.committed);
    // The pre-restart digest remains the previous anchor for a lost reply.
    let (status, bytes) = h
        .signed("POST", "/peer/v3/snapshots", renew(manifest.digest()))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        framing::decode_frame_prefix(&bytes).unwrap().0,
        MessageType::SnapshotManifest
    );
    // A pin never issued still requires a fresh snapshot.
    let (status, bytes) = h
        .signed(
            "POST",
            "/peer/v3/snapshots",
            snapshot_body(SnapshotOperation::Renew {
                pin: [9; 16],
                manifest_digest: [9; 32],
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (kind, payload, _) = framing::decode_frame_prefix(&bytes).unwrap();
    assert_eq!(kind, MessageType::SnapshotRequired);
    SnapshotRequired::decode(payload).unwrap();
    h.ingress.shutdown().await.unwrap();
}

/// A device's abandoned staging (a committed prefix whose inactivity expiry
/// lapsed, e.g. from before a restart) no longer blocks its two-stage bound:
/// a new upload reclaims it, and the admitted upload's own staging is
/// released after its durable receipt.
#[tokio::test]
async fn installed_https_abandoned_staging_is_reclaimed_for_a_new_upload() {
    use crate::ingress::{StageIdentity, UploadChunk, UploadStage};
    let mut h = Harness::start().await;
    let device = h.f.owner.identity.native_cert.device_public_key;
    let work = h.ingress.host_work();
    let pool = h.ingress.host_bytes();
    let past = tilo_relay::clock::now_ns() - 10 * 60 * 1_000_000_000;
    for id in [[21u8; 16], [22u8; 16]] {
        let canonical = vec![id[0]; 2 * CHUNK];
        let digest = *blake3::hash(&canonical).as_bytes();
        let identity = StageIdentity {
            workspace: h.f.owner.workspace_id(),
            device,
            delivery: id,
            digest,
            total: canonical.len() as u64,
        };
        let reservation = h.f.owner.reserve_upload_staging(&work, identity).unwrap();
        let chunk = StateItemChunk::new(id, digest, canonical.len() as u64, 0, &canonical[..CHUNK])
            .unwrap();
        let stage = UploadStage::open(reservation, &pool, past)
            .unwrap()
            .append(UploadChunk::copy(&chunk, &pool).unwrap(), past)
            .unwrap();
        drop(stage);
    }
    assert_eq!(
        h.f.owner.idle_upload_stages(&work, device).unwrap().len(),
        2
    );
    // A third stage for the device would exceed its bound without reclamation.
    let mut value = vec![0; 3 * CHUNK];
    blake3::Hasher::new()
        .update(b"after abandoned staging")
        .finalize_xof()
        .fill(&mut value);
    let fresh = native_edit(&h.f.owner, "after-abandoned", value);
    let (admission, credits) = h.upload([23; 16], &fresh).await;
    assert!(!credits.is_empty(), "the item was staged in ranges");
    assert!(matches!(
        admission.outcome(),
        AdmissionOutcome::Accepted { .. }
    ));
    // Abandoned stages were reclaimed; the admitted stage is removed after its
    // durable receipt (asynchronously), leaving no idle staging behind.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let idle = h.f.owner.idle_upload_stages(&work, device).unwrap();
        let remaining = idle
            .iter()
            .map(|stage| stage.identity().delivery)
            .collect::<Vec<_>>();
        drop(idle);
        if remaining.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "staging left behind: {remaining:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    h.ingress.shutdown().await.unwrap();
}
