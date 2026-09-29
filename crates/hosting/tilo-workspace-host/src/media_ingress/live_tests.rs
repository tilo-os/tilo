//! LIVE_V1 on the installed ingress: the proof-required grant route, descriptor
//! advertisement, and the live relay on the real shared H3 endpoint (loopback
//! TLS with the pinned leaf certificate, a real attached owner).
use super::{data_tests::Harness, *};
use hyper::StatusCode;
use tilo_delivery_protocol::delivery_v3::{
    capabilities::PRODUCT_DATA_V1, live_wire::*, state::AdmissionOutcome,
};
use tilo_moq::live::LiveAuthority;
use tokio::time::timeout;
use ui::automerge::{ROOT, transaction::Transactable};

const WP: &str = "so.tilo.word-processor";
const TAB: &str = "tab:0000000000000001";

pub(super) fn request(h: &Harness, participation: u8, publish: bool) -> LiveGrantRequest {
    LiveGrantRequest {
        version: 1,
        device: h.f.owner.identity.native_cert.device_public_key,
        room: h.f.owner.peer().room().into(),
        participation_id: [participation; 32],
        directions: LiveDirections {
            publish,
            subscribe: true,
        },
    }
}
pub(super) async fn grant(h: &mut Harness, wanted: &LiveGrantRequest) -> (StatusCode, Vec<u8>) {
    h.signed("POST", LIVE_GRANT_TARGET, wanted.encode().unwrap())
        .await
}
fn host_key(h: &Harness) -> ed25519_dalek::VerifyingKey {
    ed25519_dalek::VerifyingKey::from_bytes(&h.f.owner.host_certificate().device_public_key)
        .unwrap()
}
fn edit(owner: &SessionOwner, value: u64) -> Vec<u8> {
    let mut author = owner.peer().clone();
    let mut document = ui::automerge::AutoCommit::load(&author.document(WP, TAB).unwrap()).unwrap();
    let heads = document.get_heads();
    document.put(ROOT, "live", value).unwrap();
    document.commit();
    let message = author
        .publish_incremental(
            WP,
            TAB,
            document.save_after(&heads),
            &owner.identity.native,
            web_time::SystemTime::now(),
        )
        .unwrap();
    postcard::to_stdvec(&message).unwrap()
}

#[tokio::test]
async fn live_grant_route_issues_host_signed_grants_advertises_live_and_renews() {
    let mut h = Harness::start().await;
    {
        let guard = h.f.current.read().unwrap();
        let descriptor = h
            .f
            .owner
            .verified_delivery_transport(guard.view(), web_time::SystemTime::now(), PRODUCT_DATA_V1)
            .unwrap();
        assert_eq!(
            descriptor.media_capabilities(),
            tilo_delivery_protocol::delivery_v3::descriptor::MEDIA_VOICE_V1 | MEDIA_LIVE_V1,
            "an installed live relay is advertised beside voice"
        );
    }
    assert!(h.ingress.live_installed());
    let wanted = request(&h, 5, true);
    let (status, bytes) = grant(&mut h, &wanted).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    assert!(bytes.len() <= MAX_GRANT_BYTES);
    let first = SignedLiveGrant::decode(&bytes).unwrap();
    let now = tilo_delivery_protocol::clock::now_ns();
    first.verify(&host_key(&h), now).unwrap();
    let m = &first.metadata;
    assert_eq!(
        m.request, wanted,
        "the owner device may publish and subscribe"
    );
    assert_eq!(
        m.audience,
        format!("{}{LIVE_WEBTRANSPORT_PATH}", h.ingress.origin())
    );
    assert_eq!(
        m.namespace,
        LiveNamespace::new(h.f.owner.peer().room(), wanted.device)
    );
    assert!(m.expires_at_ns - m.issued_at_ns <= GRANT_LIFETIME_NS);
    let live = &h.ingress.live.as_ref().unwrap().service;
    let session = live
        .try_admit(&bytes)
        .expect("the relay admits the issued grant");
    assert!(session.publish() && session.subscribe());
    assert_eq!(session.publisher(), hex::encode(wanted.device));
    assert!(matches!(
        session.status(),
        tilo_moq::relay::AuthorizationStatus::Current
    ));
    // Tampered grants and foreign audiences never admit.
    let mut tampered = first.clone();
    tampered.metadata.request.directions.publish = false;
    assert!(live.try_admit(&tampered.encode().unwrap()).is_err());
    assert!(live.try_admit(b"not a grant").is_err());
    // Renewal: same participation, later expiry, no reconnect needed.
    tokio::time::sleep(Duration::from_millis(5)).await;
    let (status, renewed) = grant(&mut h, &wanted).await;
    assert_eq!(status, StatusCode::OK);
    let renewed = SignedLiveGrant::decode(&renewed).unwrap();
    assert!(renewed.metadata.expires_at_ns > first.metadata.expires_at_ns);
    assert_eq!(renewed.metadata.host_boot, first.metadata.host_boot);
}

#[tokio::test]
async fn live_grant_route_refuses_unproved_foreign_device_wrong_room_and_malformed_requests() {
    let mut h = Harness::start().await;
    let wanted = request(&h, 6, true);
    let (status, _) = h
        .call("POST", LIVE_GRANT_TARGET, wanted.encode().unwrap(), None)
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the route is proof-required"
    );
    let mut foreign = request(&h, 7, true);
    foreign.device = [9; 32];
    assert_eq!(
        grant(&mut h, &foreign).await.0,
        StatusCode::FORBIDDEN,
        "device must equal the session device"
    );
    let mut room = request(&h, 8, true);
    room.room = "00000000-0000-4000-8000-000000000000".into();
    assert_eq!(grant(&mut h, &room).await.0, StatusCode::FORBIDDEN);
    let (status, _) = h
        .signed("POST", LIVE_GRANT_TARGET, b"garbage".to_vec())
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = h
        .signed("POST", "/peer/v3/live/grant?x=1", wanted.encode().unwrap())
        .await;
    assert!(!status.is_success());
    // A valid request still succeeds afterwards.
    assert_eq!(grant(&mut h, &wanted).await.0, StatusCode::OK);
}

pub(super) type LiveClient = tilo_moq::live::peer::Peer;
pub(super) async fn live_connect(h: &Harness, token: Vec<u8>) -> anyhow::Result<LiveClient> {
    let path = format!(
        "tilo-live/{}/{}",
        hex::encode(room_digest(h.f.owner.peer().room())),
        hex::encode(h.f.owner.identity.native_cert.device_public_key)
    );
    let wt = web_transport_quinn::ClientBuilder::new()
        .with_server_certificates(vec![h.certificate.clone()])?;
    let transport = tilo_test_env::within(
        "the live WebTransport connection",
        wt.connect(
            web_transport_quinn::proto::ConnectRequest::new(
                format!("{}{LIVE_WEBTRANSPORT_PATH}", h.ingress.origin()).parse::<url::Url>()?,
            )
            .with_protocol(LIVE_PROTOCOL),
        ),
    )
    .await?;
    let client = tilo_test_env::within(
        "the live client to start",
        LiveClient::start(transport, &token, Some(&path)),
    )
    .await
    .map_err(anyhow::Error::from)?;
    Ok(client)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn installed_endpoint_serves_live_presence_and_forwards_admitted_signed_changes() {
    let mut h = Harness::start().await;
    let wanted = request(&h, 11, true);
    let (status, token) = grant(&mut h, &wanted).await;
    assert_eq!(status, StatusCode::OK);
    // A forged token is refused before any origin is attached.
    if let Ok(forged) = live_connect(&h, b"forged-token".to_vec()).await {
        tilo_test_env::within(
            "the forged-token session to close",
            forged.session().closed(),
        )
        .await;
    }
    let mut client = live_connect(&h, token).await.unwrap();
    let path = format!(
        "tilo-live/{}/{}",
        hex::encode(room_digest(h.f.owner.peer().room())),
        hex::encode(wanted.device)
    );
    let own = tilo_test_env::within("the own namespace to be relayed", client.member(&path))
        .await
        .expect("own namespace is relayed back to the room scope");
    // Presence with the real codec, relayed through the host relay.
    let mut presence = own
        .track(PRESENCE_TRACK)
        .unwrap()
        .subscribe(None)
        .await
        .unwrap();
    let frame = |seq: u64| {
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
    };
    let received = tilo_test_env::within("presence through the installed relay", async {
        let mut seq = 0;
        loop {
            seq += 1;
            assert!(client.publish_presence(&frame(seq)));
            // An invalid frame is dropped at the relay, never forwarded.
            assert!(client.publish_presence(&[0xff; 40]));
            if let Ok(Ok(Some(datagram))) =
                timeout(Duration::from_millis(50), presence.recv_datagram()).await
            {
                return datagram.payload.to_vec();
            }
        }
    })
    .await;
    assert!(
        PresenceFrame::decode(&received).is_ok(),
        "only codec-valid presence is relayed"
    );

    // Subscribe to this record's ops track, then durably admit a change for it.
    let record = LiveRecord {
        app: WP.into(),
        record: TAB.into(),
    };
    // Receivers keep late groups (stream reordering) for the relay window.
    let subscription = moq_net::track::Subscription::default().with_max_age(Duration::from_secs(2));
    let mut ops = own
        .track(&ops_track(&record))
        .unwrap()
        .subscribe(subscription)
        .await
        .unwrap();
    // A change admitted before the relay serves this subscription (the first
    // one also creates its egress ops track) reaches it only by durable
    // replay. Admit fresh changes until one is forwarded live, a relay window
    // apart, as the live-forward measurement's warm-up does.
    let mut issued = Vec::new();
    let (id, canonical, forwarded) =
        tilo_test_env::within("an admitted signed change on the live ops track", async {
            for attempt in 1u8.. {
                let canonical = edit(&h.f.owner, u64::from(attempt));
                let id = [60 + attempt; 16];
                let (admission, _) = h.upload(id, &canonical).await;
                assert!(
                    matches!(admission.outcome(), AdmissionOutcome::Accepted { .. }),
                    "{admission:?}"
                );
                issued.push((id, canonical));
                let next = timeout(Duration::from_secs(2), async {
                    // Groups travel on separate streams: a seal appended after
                    // the change can overtake it. Readers skip seals (one
                    // empty frame).
                    loop {
                        let mut group = ops.recv_group().await.unwrap().unwrap();
                        let frame = group.read_frame().await.unwrap().unwrap();
                        if !frame.payload.is_empty() {
                            break frame.payload.to_vec();
                        }
                    }
                });
                if let Ok(forwarded) = next.await {
                    let digest = match OpsFrame::decode(&forwarded).unwrap() {
                        OpsFrame::SignedChange { digest, .. } => digest,
                        other => panic!("expected a forwarded signed change: {other:?}"),
                    };
                    let (id, canonical) = issued
                        .iter()
                        .find(|(_, canonical)| *blake3::hash(canonical).as_bytes() == digest)
                        .cloned()
                        .expect("a forwarded change is one this test admitted");
                    return (id, canonical, forwarded);
                }
            }
            unreachable!("attempts are unbounded until the hang bound")
        })
        .await;
    let OpsFrame::SignedChange {
        record: named,
        digest,
        canonical: bytes,
    } = OpsFrame::decode(&forwarded).unwrap()
    else {
        panic!("expected a forwarded signed change");
    };
    assert_eq!(named, record);
    assert_eq!(bytes, canonical, "exact canonical delivery item bytes");
    assert_eq!(digest, *blake3::hash(&canonical).as_bytes());
    // A duplicate upload is not forwarded again.
    let (duplicate, _) = h.upload(id, &canonical).await;
    assert!(matches!(
        duplicate.outcome(),
        AdmissionOutcome::Accepted { .. } | AdmissionOutcome::Duplicate { .. }
    ));
    // Only empty seal groups may follow; no frame is forwarded again.
    let again = timeout(Duration::from_millis(400), async {
        loop {
            let Ok(Some(mut group)) = ops.recv_group().await else {
                return None;
            };
            // Seal groups hold one empty frame.
            if let Ok(Some(frame)) = group.read_frame().await
                && !frame.payload.is_empty()
            {
                return Some(frame.payload.to_vec());
            }
        }
    })
    .await;
    assert!(
        !matches!(again, Ok(Some(_))),
        "duplicates are not re-forwarded"
    );
    drop(client);
}

#[test]
fn relay_bounds_match_the_frozen_wire_contract() {
    use tilo_moq::live;
    assert_eq!(live::LIVE_PATH, LIVE_WEBTRANSPORT_PATH);
    assert_eq!(live::LIVE_PROTOCOL, LIVE_PROTOCOL);
    assert_eq!(live::NAMESPACE_ROOT, LIVE_NAMESPACE_ROOT);
    assert_eq!(live::PRESENCE_TRACK, PRESENCE_TRACK);
    assert_eq!(live::OPS_PREFIX, OPS_TRACK_PREFIX);
    assert_eq!(live::MAX_TOKEN_BYTES, MAX_GRANT_BYTES);
    assert_eq!(live::MAX_PRESENCE_RATE, MAX_PRESENCE_RATE);
    assert_eq!(live::MAX_OPS_RATE, MAX_OPS_RATE);
    assert_eq!(live::MAX_OPEN_RECORDS, MAX_OPEN_RECORDS);
    assert_eq!(live::MAX_DATAGRAM_FRAME_BYTES, MAX_DATAGRAM_FRAME_BYTES);
    assert_eq!(live::MAX_STREAM_FRAME_BYTES, MAX_STREAM_FRAME_BYTES);
    let ns = LiveNamespace::new("room", [7; 32]);
    let [root, room, publisher] = ns.components();
    assert_eq!(
        live::namespace_path(&room, &publisher).unwrap(),
        format!("{root}/{room}/{publisher}")
    );
    let record = LiveRecord {
        app: "a".into(),
        record: "r".into(),
    };
    assert_eq!(
        live::ops_record(&ops_track(&record)),
        Some(hex::encode(record.digest()).as_str())
    );
}

#[test]
fn only_record_scoped_app_documents_become_signed_change_frames() {
    assert!(crate::live_service::signed_change(b"not a message").is_none());
    let message = tilo_peer_state::NeighbourMsg::SyncReq {
        since: vec![],
        room: "room".into(),
        invite_id: None,
    };
    assert!(crate::live_service::signed_change(&postcard::to_stdvec(&message).unwrap()).is_none());
}

/// Regression: sustained owner commits, including a single
/// commit that holds the committed authority for longer than the former
/// 900 ms outage fence, must never revoke the author's live session, and
/// every admitted change must still be forwarded on the author's ops track.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sustained_commits_never_revoke_the_author_and_forwarding_continues() {
    let mut h = Harness::start().await;
    let wanted = request(&h, 21, true);
    let (status, token) = grant(&mut h, &wanted).await;
    assert_eq!(status, StatusCode::OK);
    let client = live_connect(&h, token).await.unwrap();
    let path = format!(
        "tilo-live/{}/{}",
        hex::encode(room_digest(h.f.owner.peer().room())),
        hex::encode(wanted.device)
    );
    let own = tilo_test_env::within("the own namespace to be relayed", client.member(&path))
        .await
        .unwrap();
    let record = LiveRecord {
        app: WP.into(),
        record: TAB.into(),
    };
    // Receivers keep late groups (stream reordering) for the relay window.
    let subscription = moq_net::track::Subscription::default().with_max_age(Duration::from_secs(2));
    let mut ops = own
        .track(&ops_track(&record))
        .unwrap()
        .subscribe(subscription)
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let relay = h.ingress.transport.live.get().cloned().unwrap();
    // Read concurrently, as a browser does: a receiver's own cache ages out
    // groups nobody reads (the long owner turns below block this task).
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
        while let Ok(Some(mut group)) = ops.recv_group().await {
            while let Ok(Some(frame)) = group.read_frame().await {
                if let Ok(OpsFrame::SignedChange { digest, .. }) = OpsFrame::decode(&frame.payload)
                {
                    let _ = seen_tx.send(digest);
                }
            }
        }
    });
    let mut expected = Vec::new();
    for round in 0..24u64 {
        if round % 8 == 4 {
            // One long owner turn: every relay observation is contended.
            h.f.current
                .with_owner(&mut h.f.owner, |_| {
                    std::thread::sleep(Duration::from_millis(1300))
                })
                .unwrap();
        }
        let canonical = edit(&h.f.owner, 100 + round);
        let (admission, _) = h.upload([(round + 1) as u8; 16], &canonical).await;
        assert!(
            matches!(admission.outcome(), AdmissionOutcome::Accepted { .. }),
            "{admission:?}"
        );
        expected.push(*blake3::hash(&canonical).as_bytes());
    }
    let mut seen = std::collections::HashSet::new();
    timeout(tilo_test_env::hang_bound(), async {
        while !expected.iter().all(|digest| seen.contains(digest)) {
            seen.insert(seen_rx.recv().await.expect("ops reader"));
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "forwarded {}/{} changes: {:?}",
            seen.len(),
            expected.len(),
            relay.counters()
        )
    });
    let counters = relay.counters();
    assert_eq!(
        counters.revoked, 0,
        "busy owner revoked a live session: {counters:?}"
    );
    assert_eq!(counters.injected_dropped, 0, "{counters:?}");
    assert!(
        timeout(Duration::from_millis(50), client.session().closed())
            .await
            .is_err(),
        "the author's live session stays open"
    );
    reader.abort();
    drop(client);
}

/// A state restored from an older copy while the host was stopped still
/// lists a member the owner has since removed, while the relay retains the
/// owner-signed removal. No grant is issued before that retained state is
/// merged, and once it is merged the removed member holds no permission.
#[tokio::test]
async fn rolled_back_state_issues_no_grant_before_retained_governance_is_merged() {
    use crate::collaboration::{CollaborationService, IssueInvitationRequest, RemoveMemberRequest};
    use tilo_peer_state::join::{
        JoinChallengeRequest, JoinCompleteRequest, MemberRole, sign_join_proof,
    };
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
    let mut h = Harness::start().await;
    let root_key = ed25519_dalek::SigningKey::from_bytes(&[71; 32]);
    let device_key = ed25519_dalek::SigningKey::from_bytes(&[72; 32]);
    let now = tilo_delivery_protocol::clock::now_ns();
    let mut guest = DeviceCert {
        user_public_key: root_key.verifying_key().to_bytes(),
        device_public_key: device_key.verifying_key().to_bytes(),
        device_label: "Rolled-back guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now - 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut guest, &root_key).unwrap();
    let service = CollaborationService::new(vec![]);
    let administrator = h.f.owner.peer().authority().device_public_key;
    h.f.current
        .with_owner(&mut h.f.owner, |owner| {
            let issued = service
                .issue(
                    owner,
                    administrator,
                    IssueInvitationRequest {
                        request_id: [71; 32],
                        role: MemberRole::Editor,
                        recipient_user_public_key: Some(guest.user_public_key),
                        invitee_name: Some("Guest".into()),
                        expires_at_ns: now + 120_000_000_000,
                    },
                    now,
                )
                .unwrap();
            let challenge = service
                .challenge(
                    owner,
                    guest.device_public_key,
                    JoinChallengeRequest {
                        signed_invitation: issued.signed_invitation,
                        device_cert: guest.clone(),
                        request_id: [72; 32],
                    },
                    now,
                )
                .unwrap();
            service
                .complete(
                    owner,
                    guest.device_public_key,
                    JoinCompleteRequest {
                        proof: sign_join_proof(challenge.challenge, &device_key).unwrap(),
                    },
                    now,
                )
                .unwrap();
        })
        .unwrap();
    let at = web_time::SystemTime::now();
    assert!(
        h.f.owner
            .peer()
            .permissions(guest.device_public_key, at)
            .is_ok()
    );
    // The copy an attacker later restores: the guest is still a member.
    let root = h.f.owner.root().to_owned();
    let saved = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_file())
        .map(|path| (path.clone(), std::fs::read(&path).unwrap()))
        .collect::<Vec<_>>();
    h.f.current
        .with_owner(&mut h.f.owner, |owner| {
            service
                .remove_member(
                    owner,
                    administrator,
                    RemoveMemberRequest {
                        request_id: [73; 32],
                        user_public_key: guest.user_public_key,
                    },
                    tilo_delivery_protocol::clock::now_ns(),
                )
                .unwrap();
        })
        .unwrap();
    // What the relay retains: every owner publication after the removal.
    let retained =
        h.f.owner
            .outgoing()
            .iter()
            .map(|publication| publication.bytes.to_vec())
            .collect::<Vec<_>>();
    let mut h = h
        .restart_restoring(
            |root| {
                for entry in std::fs::read_dir(root).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_file() && !saved.iter().any(|(saved, _)| saved == &path) {
                        std::fs::remove_file(&path).unwrap();
                    }
                }
                for (path, bytes) in &saved {
                    std::fs::write(path, bytes).unwrap();
                }
            },
            false,
        )
        .await;
    assert!(
        h.f.owner
            .peer()
            .permissions(guest.device_public_key, web_time::SystemTime::now())
            .is_ok(),
        "the restored state really predates the removal"
    );
    let wanted = request(&h, 9, false);
    let (status, _) = grant(&mut h, &wanted).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "no grant before the relay-retained state is merged"
    );
    // The relay replays its retained state before Ready; the host merges it.
    let authority = h.f.owner.peer().authority().device_public_key;
    h.f.owner.permit_unscoped_legacy_commits().unwrap();
    for bytes in retained {
        let message = postcard::from_bytes(&bytes).unwrap();
        h.f.current
            .with_owner(&mut h.f.owner, |owner| {
                owner.receive(
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(authority),
                    message,
                )
            })
            .unwrap()
            .unwrap();
    }
    h.f.current.mark_governance_reconciled();
    assert!(
        h.f.owner
            .peer()
            .permissions(guest.device_public_key, web_time::SystemTime::now())
            .is_err(),
        "the retained removal is merged before any grant"
    );
    let (status, bytes) = grant(&mut h, &wanted).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
}
