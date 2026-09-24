//! LIVE_V1 on the installed Cloud host: the proof-required grant route over
//! the actual Cloud Store owner, the descriptor live bit, and the live relay on
//! the real shared QUIC endpoint (pinned loopback TLS leaf, native MoQ clients).
use super::*;
use qos_relay::delivery_v3::live_wire::*;
use rustls::pki_types::CertificateDer;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio::time::timeout;

const T: Duration = Duration::from_secs(10);

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap()
}
fn owner_client(origin: &str) -> Client {
    Client {
        http: http(),
        origin: origin.to_owned(),
        key: SigningKey::from_bytes(&[8; 32]),
        session: None,
    }
}
fn wanted(client: &Client, root: &Path, participation: u8, publish: bool) -> LiveGrantRequest {
    LiveGrantRequest {
        version: 1,
        device: client.key.verifying_key().to_bytes(),
        room: peer(root).room().to_owned(),
        participation_id: [participation; 32],
        directions: LiveDirections {
            publish,
            subscribe: true,
        },
    }
}
async fn grant(client: &Client, wanted: &LiveGrantRequest) -> (reqwest::StatusCode, Vec<u8>) {
    client
        .post(LIVE_GRANT_TARGET, wanted.encode().unwrap())
        .await
}
fn host_key(root: &Path) -> ed25519_dalek::VerifyingKey {
    ed25519_dalek::VerifyingKey::from_bytes(&peer(root).authority().device_public_key).unwrap()
}
fn leaf(transport_root: &tempfile::TempDir) -> CertificateDer<'static> {
    CertificateDer::from(std::fs::read(transport_root.path().join("cert.der")).unwrap())
}
fn namespace(root: &Path, device: [u8; 32]) -> String {
    let [first, room, publisher] = LiveNamespace::new(peer(root).room(), device).components();
    format!("{first}/{room}/{publisher}")
}

/// Invite, onboard and authenticate an independent-root member.
async fn invite(
    root: &Path,
    origin: &str,
    enrollment: &qos_user_identity::cloud_enrollment::EnrollmentRequest,
    role: qos_action_broker::collaboration_admin::CollaborationRole,
    seed: u8,
) -> (Client, SigningKey) {
    use qos_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest as Join,
        JoinTransportResponse as Reply, decode_and_verify_invitation_text, sign_join_proof,
    };
    use qos_relay::delivery_v3::onboarding_wire::{OnboardingOperation, OnboardingScope};
    let admin =
        crate::broker::CloudCollaborationAdmin::new(root, MACHINE, &enrollment.challenge.room);
    let issued = qos_action_broker::collaboration_admin::CollaborationAdmin::execute(
        &admin,
        qos_action_broker::collaboration_admin::CollaborationAdminRequest {
            room: enrollment.challenge.room.clone(),
            invocation: qos_actions::InvocationId::new(),
            caller: enrollment.browser_certificate.clone(),
            operation: qos_action_broker::collaboration_admin::CollaborationAdminOperation::Issue(
                qos_action_broker::collaboration_admin::IssueInvitationInput {
                    role,
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
    let signed_invitation = verified.signed_invitation().to_vec();
    let guest_root = SigningKey::from_bytes(&[seed; 32]);
    let guest_device = SigningKey::from_bytes(&[seed + 1; 32]);
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
        http: http(),
        origin: origin.to_owned(),
        key: guest_device.clone(),
        session: None,
    };
    let host = verified.grant().authority.clone();
    let scope = OnboardingScope {
        origin,
        host: host.device_public_key,
        workspace: verified.grant().workspace_id,
        root: host.user_public_key,
        room: verified.grant().room.parse().unwrap(),
    };
    let Reply::Challenge(challenge) = onboard(
        &guest,
        scope,
        &guest_cert,
        OnboardingOperation::Challenge,
        &Join::Challenge(JoinChallengeRequest {
            signed_invitation,
            device_cert: guest_cert.clone(),
            request_id: [seed; 32],
        }),
    )
    .await
    else {
        panic!("join challenge required")
    };
    let proof = sign_join_proof(challenge.challenge, &guest_device).unwrap();
    let Reply::Complete(_) = onboard(
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
    handshake(&mut guest, root).await;
    (guest, guest_root)
}

async fn administer(
    root: &Path,
    enrollment: &qos_user_identity::cloud_enrollment::EnrollmentRequest,
    operation: qos_action_broker::collaboration_admin::CollaborationAdminOperation,
) {
    let admin =
        crate::broker::CloudCollaborationAdmin::new(root, MACHINE, &enrollment.challenge.room);
    qos_action_broker::collaboration_admin::CollaborationAdmin::execute(
        &admin,
        qos_action_broker::collaboration_admin::CollaborationAdminRequest {
            room: enrollment.challenge.room.clone(),
            invocation: qos_actions::InvocationId::new(),
            caller: enrollment.browser_certificate.clone(),
            operation,
        },
    )
    .await
    .unwrap();
}

struct LiveClient {
    session: moq_net::Session,
    _driver: tokio::task::JoinHandle<()>,
    incoming: moq_net::origin::Producer,
    _outgoing: moq_net::origin::Producer,
    _broadcast: moq_net::broadcast::Producer,
    presence: moq_net::track::Producer,
}
async fn live_connect(
    origin: &str,
    certificate: &CertificateDer<'static>,
    own: &str,
    token: Vec<u8>,
) -> anyhow::Result<LiveClient> {
    let outgoing = moq_net::Origin::random().produce();
    let incoming = moq_net::Origin::random().produce();
    let mut broadcast =
        outgoing.create_broadcast(own, moq_net::broadcast::Route::new().with_announce(true))?;
    let presence = broadcast.create_track(PRESENCE_TRACK, None)?;
    let wt = web_transport_quinn::ClientBuilder::new()
        .with_server_certificates(vec![certificate.clone()])?;
    let transport = timeout(
        T,
        wt.connect(
            web_transport_quinn::proto::ConnectRequest::new(
                format!("{origin}{LIVE_WEBTRANSPORT_PATH}").parse::<url::Url>()?,
            )
            .with_protocol(LIVE_PROTOCOL),
        ),
    )
    .await??;
    let (session, driver) = timeout(
        T,
        moq_net::Client::new()
            .with_versions(
                "moq-transport-21"
                    .parse::<moq_net::Version>()
                    .map_err(anyhow::Error::msg)?
                    .into(),
            )
            .with_authorization(token)?
            .with_publisher(&outgoing)
            .with_subscriber(incoming.clone())
            .connect(transport),
    )
    .await??;
    let driver = tokio::spawn(async move {
        let _ = driver.await;
    });
    Ok(LiveClient {
        session,
        _driver: driver,
        incoming,
        _outgoing: outgoing,
        _broadcast: broadcast,
        presence,
    })
}
async fn announced(client: &LiveClient, path: &str) -> moq_net::broadcast::Consumer {
    timeout(T, client.incoming.consume().announced_broadcast(path))
        .await
        .expect("announce deadline")
        .expect("announced")
}
async fn presence_of(client: &LiveClient, path: &str) -> moq_net::track::Subscriber {
    announced(client, path)
        .await
        .track(PRESENCE_TRACK)
        .unwrap()
        .subscribe(None)
        .await
        .unwrap()
}
async fn ops_of(
    client: &LiveClient,
    path: &str,
    record: &LiveRecord,
) -> moq_net::track::Subscriber {
    let mut subscription = moq_net::track::Subscription::default();
    subscription.ordered = true;
    announced(client, path)
        .await
        .track(&ops_track(record))
        .unwrap()
        .subscribe(subscription)
        .await
        .unwrap()
}
fn presence(seq: u64) -> Vec<u8> {
    PresenceFrame {
        seq,
        sent_at_ms: 1,
        focus: None,
        pointer: Some([seq as f32, 2.0]),
        selection: None,
        drag: None,
    }
    .encode()
    .unwrap()
}
/// Offer presence until one arrives (the relay's upstream subscription
/// attaches asynchronously); returns the received sequence.
async fn first_presence(
    from: &mut LiveClient,
    at: &mut moq_net::track::Subscriber,
    seq: &mut u64,
) -> u64 {
    timeout(T, async {
        loop {
            *seq += 1;
            from.presence
                .append_datagram(moq_net::Timestamp::ZERO, presence(*seq))
                .unwrap();
            if let Ok(Ok(Some(datagram))) =
                timeout(Duration::from_millis(50), at.recv_datagram()).await
            {
                return PresenceFrame::decode(&datagram.payload).unwrap().seq;
            }
        }
    })
    .await
    .expect("presence delivery deadline")
}
const APP: &str = "dev.qos.word-processor";
const RECORD: &str = "tab:0000000000000001";
/// Canonical incremental word-processor delta authored by `author`'s device.
fn delta_by(root: &Path, author: &SigningKey, text: &str) -> Vec<u8> {
    use qos_actions::Action;
    let store = crate::Store::open(root, MACHINE).unwrap();
    let now = qos_relay::clock::now_ns();
    let mut peer = store.peer_at(now).unwrap();
    drop(store);
    let mut doc = automerge::AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    doc.set_actor(automerge::ActorId::from(
        author.verifying_key().to_bytes().to_vec(),
    ));
    let model = qdoc::model::open(&doc, &automerge::ROOT).unwrap();
    let block = qdoc::model::blocks_in_order(&doc, &model)[0].clone();
    let heads = doc.get_heads();
    word_processor::actions::WordProcessorAction::SpliceText {
        region: word_processor::actions::RegionKind::Body,
        block,
        at: 0,
        del: 0,
        ins: text.into(),
    }
    .apply(&mut doc)
    .unwrap();
    doc.commit();
    let message = peer
        .publish_incremental(
            APP,
            RECORD,
            doc.save_after(&heads),
            author,
            crate::now_time(now),
        )
        .unwrap();
    postcard::to_stdvec(&message).unwrap()
}
/// Upload one fresh edit by `author` and require its durable acceptance.
async fn upload(author: &Client, root: &Path, delivery: u8) -> Vec<u8> {
    let canonical = delta_by(root, &author.key, "live ");
    let (status, body) = author
        .post(
            "/peer/v3/state",
            chunk([delivery; 16], &canonical, 0, canonical.len()),
        )
        .await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&body)
    );
    accepted(&body);
    canonical
}
/// Upload fresh edits until one is forwarded on `ops`; returns the uploaded
/// canonical bytes and the forwarded frame.
async fn forwarded(
    author: &Client,
    root: &Path,
    ops: &mut moq_net::track::Subscriber,
    delivery: &mut u8,
) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut uploaded = Vec::new();
    let frame = timeout(T, async {
        loop {
            *delivery += 1;
            uploaded.push(upload(author, root, *delivery).await);
            if let Ok(Ok(Some(mut group))) =
                timeout(Duration::from_millis(300), ops.recv_group()).await
            {
                if let Ok(Some(frame)) = group.read_frame().await {
                    return frame.payload.to_vec();
                }
            }
        }
    })
    .await
    .expect("forwarded signed change deadline");
    (uploaded, frame)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_live_grant_route_issues_renews_refuses_and_advertises_live() {
    let (_transport_root, store_root, config, ready) = deployment().await;
    let root = store_root.path().to_owned();
    let (stop, server, origin) = started(&config, &ready).await;
    let mut owner = owner_client(&origin);
    let descriptor = handshake(&mut owner, &root).await;
    assert_eq!(
        descriptor.media_capabilities(),
        descriptor::MEDIA_VOICE_V1 | MEDIA_LIVE_V1,
        "the installed live relay is advertised beside voice"
    );
    let request = wanted(&owner, &root, 5, true);
    let (status, bytes) = grant(&owner, &request).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(bytes.len() <= MAX_GRANT_BYTES);
    let first = SignedLiveGrant::decode(&bytes).unwrap();
    first
        .verify(&host_key(&root), qos_relay::clock::now_ns())
        .unwrap();
    let m = &first.metadata;
    assert_eq!(
        m.request, request,
        "the owner device may publish and subscribe"
    );
    assert_eq!(m.audience, format!("{origin}{LIVE_WEBTRANSPORT_PATH}"));
    assert_eq!(
        m.namespace,
        LiveNamespace::new(peer(&root).room(), request.device)
    );
    assert_eq!(m.host, peer(&root).authority().device_public_key);
    assert_eq!(
        m.root,
        device_cert(&peer(&root), &owner.key).user_public_key
    );
    assert!(m.expires_at_ns - m.issued_at_ns <= GRANT_LIFETIME_NS);
    // Renewal: same participation, later expiry, same host boot.
    tokio::time::sleep(Duration::from_millis(5)).await;
    let (status, renewed) = grant(&owner, &request).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let renewed = SignedLiveGrant::decode(&renewed).unwrap();
    assert!(renewed.metadata.expires_at_ns > first.metadata.expires_at_ns);
    assert_eq!(renewed.metadata.host_boot, first.metadata.host_boot);

    // Refusals.
    let unproved = owner_client(&origin);
    let (status, _) = grant(&unproved, &request).await;
    assert_eq!(
        status,
        reqwest::StatusCode::UNAUTHORIZED,
        "the route is proof-required"
    );
    let mut foreign = wanted(&owner, &root, 6, true);
    foreign.device = [9; 32];
    assert_eq!(
        grant(&owner, &foreign).await.0,
        reqwest::StatusCode::FORBIDDEN
    );
    let mut room = wanted(&owner, &root, 7, true);
    room.room = "00000000-0000-4000-8000-000000000000".into();
    assert_eq!(grant(&owner, &room).await.0, reqwest::StatusCode::FORBIDDEN);
    let (status, _) = owner.post(LIVE_GRANT_TARGET, b"garbage".to_vec()).await;
    assert_eq!(status, reqwest::StatusCode::FORBIDDEN);
    // A capability-zero (media-only) session is not a data session.
    let mut media_only = owner_client(&origin);
    handshake_with(&mut media_only, &root, 0).await;
    assert_eq!(
        grant(&media_only, &wanted(&media_only, &root, 8, true))
            .await
            .0,
        reqwest::StatusCode::FORBIDDEN
    );
    // A valid request still succeeds afterwards.
    assert_eq!(grant(&owner, &request).await.0, reqwest::StatusCode::OK);
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_live_relays_presence_between_two_native_clients_and_forwards_signed_changes() {
    use qos_action_broker::collaboration_admin::CollaborationRole;
    let (transport_root, store_root, config, ready, enrollment) = enrolled_deployment().await;
    let root = store_root.path().to_owned();
    let certificate = leaf(&transport_root);
    let (stop, server, origin) = started(&config, &ready).await;
    let mut owner = owner_client(&origin);
    handshake(&mut owner, &root).await;
    let (guest, _) = invite(&root, &origin, &enrollment, CollaborationRole::Editor, 9).await;
    let owner_path = namespace(&root, owner.key.verifying_key().to_bytes());
    let guest_path = namespace(&root, guest.key.verifying_key().to_bytes());

    // Forged tokens are refused before any origin is attached.
    if let Ok(forged) = live_connect(&origin, &certificate, &owner_path, b"forged".to_vec()).await {
        timeout(T, forged.session.closed())
            .await
            .expect("forged token closes");
    }
    let (status, owner_token) = grant(&owner, &wanted(&owner, &root, 11, true)).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    let (status, guest_token) = grant(&guest, &wanted(&guest, &root, 12, true)).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&guest_token)
    );
    let mut a = live_connect(&origin, &certificate, &owner_path, owner_token)
        .await
        .unwrap();
    let mut b = live_connect(&origin, &certificate, &guest_path, guest_token)
        .await
        .unwrap();
    let mut a_at_b = presence_of(&b, &owner_path).await;
    let mut b_at_a = presence_of(&a, &guest_path).await;
    let (mut sa, mut sb) = (0, 0);
    first_presence(&mut a, &mut a_at_b, &mut sa).await;
    first_presence(&mut b, &mut b_at_a, &mut sb).await;

    // One-way latency, 100 Hz each way (under the 120/s ceiling), measured on
    // one clock from publisher append to subscriber receive.
    const FRAMES: usize = 200;
    let sent: Arc<Mutex<HashMap<(u8, u64), Instant>>> = Arc::default();
    let receive = |mut subscriber: moq_net::track::Subscriber, from: u8| {
        let sent = sent.clone();
        async move {
            let mut samples = Vec::new();
            while samples.len() < FRAMES {
                let Ok(Ok(Some(datagram))) =
                    timeout(Duration::from_secs(2), subscriber.recv_datagram()).await
                else {
                    break;
                };
                let at = Instant::now();
                let seq = PresenceFrame::decode(&datagram.payload).unwrap().seq;
                if let Some(start) = sent.lock().unwrap().get(&(from, seq)) {
                    samples.push(at.duration_since(*start));
                }
            }
            samples
        }
    };
    let rx_b = tokio::spawn(receive(a_at_b, 0));
    let rx_a = tokio::spawn(receive(b_at_a, 1));
    for _ in 0..FRAMES {
        sa += 1;
        sb += 1;
        let (fa, fb) = (presence(sa), presence(sb));
        sent.lock().unwrap().insert((0, sa), Instant::now());
        a.presence
            .append_datagram(moq_net::Timestamp::ZERO, fa)
            .unwrap();
        sent.lock().unwrap().insert((1, sb), Instant::now());
        b.presence
            .append_datagram(moq_net::Timestamp::ZERO, fb)
            .unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut samples = rx_b.await.unwrap();
    samples.extend(rx_a.await.unwrap());
    assert!(
        samples.len() >= FRAMES * 2 * 9 / 10,
        "only {} of {} frames arrived",
        samples.len(),
        FRAMES * 2
    );
    samples.sort_unstable();
    let pct = |p: usize| samples[(samples.len() - 1) * p / 100];
    eprintln!(
        "CLOUD_LIVE_LOOPBACK_LATENCY samples={} p50={:?} p90={:?} p99={:?} max={:?}",
        samples.len(),
        pct(50),
        pct(90),
        pct(99),
        pct(100)
    );
    assert!(
        pct(50) < Duration::from_millis(10),
        "median one-way {:?}",
        pct(50)
    );
    assert!(
        pct(90) < Duration::from_millis(10),
        "p90 one-way {:?}",
        pct(90)
    );

    // Author live: an accepted owner edit is forwarded on the owner's
    // namespace as the exact canonical delivery item bytes.
    let record = LiveRecord {
        app: APP.into(),
        record: RECORD.into(),
    };
    let mut ops = ops_of(&b, &owner_path, &record).await;
    let mut delivery = 100;
    let (uploaded, frame) = forwarded(&owner, &root, &mut ops, &mut delivery).await;
    let OpsFrame::SignedChange {
        record: named,
        digest,
        canonical,
    } = OpsFrame::decode(&frame).unwrap()
    else {
        panic!("signed change expected")
    };
    assert_eq!(named, record);
    assert!(
        uploaded.contains(&canonical),
        "exact canonical delivery item bytes"
    );
    assert_eq!(digest, *blake3::hash(&canonical).as_bytes());
    // A lost-reply re-upload of the same item is not forwarded again.
    let last = uploaded.last().unwrap();
    let (status, _) = owner
        .post("/peer/v3/state", chunk([delivery; 16], last, 0, last.len()))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    if canonical == *last {
        assert!(
            timeout(Duration::from_millis(300), ops.recv_group())
                .await
                .is_err(),
            "duplicates are not re-forwarded"
        );
    }

    // Author not live: once the guest's live session is gone, its accepted
    // edits make the host announce and feed the guest's namespace itself.
    let guest_at_a = announced(&a, &guest_path).await;
    drop(ops);
    b.session.abort(moq_net::Error::Cancel);
    timeout(T, guest_at_a.closed())
        .await
        .expect("guest namespace withdrawn");
    delivery += 1;
    upload(&guest, &root, delivery).await;
    let mut ops = ops_of(&a, &guest_path, &record).await;
    let (uploaded, frame) = forwarded(&guest, &root, &mut ops, &mut delivery).await;
    let OpsFrame::SignedChange { canonical, .. } = OpsFrame::decode(&frame).unwrap() else {
        panic!("signed change expected")
    };
    assert!(
        uploaded.contains(&canonical),
        "host-fed exact canonical bytes"
    );
    a.session.abort(moq_net::Error::Cancel);
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cloud_live_viewer_subscribes_only_and_revocation_aborts_only_that_publisher() {
    use qos_action_broker::collaboration_admin::{
        CollaborationAdminOperation, CollaborationRole, RemoveMemberInput, SetMemberRoleInput,
    };
    let (transport_root, store_root, config, ready, enrollment) = enrolled_deployment().await;
    let root = store_root.path().to_owned();
    let certificate = leaf(&transport_root);
    let (stop, server, origin) = started(&config, &ready).await;
    let mut owner = owner_client(&origin);
    handshake(&mut owner, &root).await;
    let (viewer, _) = invite(&root, &origin, &enrollment, CollaborationRole::Viewer, 13).await;
    let (editor, editor_root) =
        invite(&root, &origin, &enrollment, CollaborationRole::Editor, 9).await;
    let (demoted, demoted_root) =
        invite(&root, &origin, &enrollment, CollaborationRole::Editor, 15).await;

    // A viewer asking to publish receives a subscribe-only grant.
    let (status, viewer_token) = grant(&viewer, &wanted(&viewer, &root, 21, true)).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&viewer_token)
    );
    let viewer_grant = SignedLiveGrant::decode(&viewer_token).unwrap();
    assert_eq!(
        viewer_grant.metadata.request.directions,
        LiveDirections {
            publish: false,
            subscribe: true
        }
    );

    let owner_path = namespace(&root, owner.key.verifying_key().to_bytes());
    let editor_path = namespace(&root, editor.key.verifying_key().to_bytes());
    let viewer_path = namespace(&root, viewer.key.verifying_key().to_bytes());
    let (_, owner_token) = grant(&owner, &wanted(&owner, &root, 22, true)).await;
    let (_, editor_token) = grant(&editor, &wanted(&editor, &root, 23, true)).await;
    let mut a = live_connect(&origin, &certificate, &owner_path, owner_token)
        .await
        .unwrap();
    let mut e = live_connect(&origin, &certificate, &editor_path, editor_token)
        .await
        .unwrap();
    let mut v = live_connect(&origin, &certificate, &viewer_path, viewer_token)
        .await
        .unwrap();
    let mut a_at_v = presence_of(&v, &owner_path).await;
    let mut e_at_v = presence_of(&v, &editor_path).await;
    let (mut sa, mut se, mut sv) = (0, 0, 0);
    first_presence(&mut a, &mut a_at_v, &mut sa).await;
    first_presence(&mut e, &mut e_at_v, &mut se).await;
    // The viewer's own presence is never relayed.
    let mut v_at_a = timeout(Duration::from_millis(500), presence_of(&a, &viewer_path)).await;
    if let Ok(subscriber) = v_at_a.as_mut() {
        for _ in 0..10 {
            sv += 1;
            v.presence
                .append_datagram(moq_net::Timestamp::ZERO, presence(sv))
                .unwrap();
        }
        assert!(
            !matches!(
                timeout(Duration::from_millis(300), subscriber.recv_datagram()).await,
                Ok(Ok(Some(_)))
            ),
            "viewer presence relayed"
        );
    }

    // Demoting a live publisher to viewer ends its publication; its next
    // grant is subscribe-only.
    let demoted_path = namespace(&root, demoted.key.verifying_key().to_bytes());
    let (_, demoted_token) = grant(&demoted, &wanted(&demoted, &root, 25, true)).await;
    let mut d = live_connect(&origin, &certificate, &demoted_path, demoted_token)
        .await
        .unwrap();
    let mut d_at_v = presence_of(&v, &demoted_path).await;
    let mut sd = 0;
    first_presence(&mut d, &mut d_at_v, &mut sd).await;
    let demoted_at_v = announced(&v, &demoted_path).await;
    administer(
        &root,
        &enrollment,
        CollaborationAdminOperation::SetRole(SetMemberRoleInput {
            participant_id: hex::encode(demoted_root.verifying_key().to_bytes()),
            role: CollaborationRole::Viewer,
        }),
    )
    .await;
    timeout(T, async {
        tokio::select! {
            _ = demoted_at_v.closed() => {}
            _ = async { while let Ok(Some(_)) = d_at_v.recv_datagram().await {} } => {}
        }
    })
    .await
    .expect("demoted publisher's namespace ends at the viewer");
    let (status, regrant) = grant(&demoted, &wanted(&demoted, &root, 26, true)).await;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&regrant)
    );
    assert!(
        !SignedLiveGrant::decode(&regrant)
            .unwrap()
            .metadata
            .request
            .directions
            .publish
    );
    d.session.abort(moq_net::Error::Cancel);
    // The other publishers were not affected.
    first_presence(&mut e, &mut e_at_v, &mut se).await;

    // Removing the editor aborts only the editor's tracks.
    let editor_at_v = announced(&v, &editor_path).await;
    let removed = Instant::now();
    administer(
        &root,
        &enrollment,
        CollaborationAdminOperation::Remove(RemoveMemberInput {
            participant_id: hex::encode(editor_root.verifying_key().to_bytes()),
        }),
    )
    .await;
    let committed = removed.elapsed();
    timeout(T, async {
        tokio::select! {
            _ = editor_at_v.closed() => {}
            _ = async { while let Ok(Some(_)) = e_at_v.recv_datagram().await {} } => {}
        }
    })
    .await
    .expect("removed publisher's namespace ends at the viewer");
    eprintln!(
        "CLOUD_LIVE_REVOCATION commit={committed:?} tracks_ended={:?}",
        removed.elapsed()
    );
    timeout(T, e.session.closed())
        .await
        .expect("revoked session closes");
    assert!(
        timeout(Duration::from_millis(100), v.session.closed())
            .await
            .is_err(),
        "the viewer's session stays open"
    );
    // The owner keeps flowing to the viewer; the removed editor is refused.
    first_presence(&mut a, &mut a_at_v, &mut sa).await;
    let (status, _) = grant(&editor, &wanted(&editor, &root, 24, true)).await;
    assert!(
        matches!(
            status,
            reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::UNAUTHORIZED
        ),
        "removed member refused: {status}"
    );
    a.session.abort(moq_net::Error::Cancel);
    v.session.abort(moq_net::Error::Cancel);
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}
