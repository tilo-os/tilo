//! Exercise the production HTTP routes and authenticated native presence dispatcher.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in tests these are assertions"
)]
#![allow(
    clippy::too_many_lines,
    reason = "one authenticated cross-carrier scenario"
)]

use super::*;
use crate::{Envelope, RelayClient, RelayConfig};
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_peer_state::{NeighbourMsg, join::MemberRole};
use tilo_relay_protocol::legacy_window as wire;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn device(
    root: &SigningKey,
    seed: u8,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "legacy test".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

async fn endpoint(key: &SigningKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap()
}

async fn post(
    addr: SocketAddr,
    path: &str,
    token: Option<[u8; 32]>,
    body: Vec<u8>,
) -> (u16, Vec<u8>) {
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let token = token.map_or_else(String::new, |token| {
        format!("{SESSION_HEADER}: {}\r\n", hex::encode(token))
    });
    let header = format!(
        "POST {API_PREFIX}{path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n{token}Content-Length: {}\r\n\r\n",
        body.len()
    );
    socket.write_all(header.as_bytes()).await.unwrap();
    socket.write_all(&body).await.unwrap();
    let mut reply = Vec::new();
    tilo_test_env::within("HTTP reply", socket.read_to_end(&mut reply))
        .await
        .unwrap();
    let split = reply
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap();
    let header = std::str::from_utf8(&reply[..split]).unwrap();
    let status = header.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = &reply[split.checked_add(4).unwrap()..];
    if header
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut decoded = Vec::new();
        let mut rest = body;
        loop {
            let end = rest.windows(2).position(|bytes| bytes == b"\r\n").unwrap();
            let length =
                usize::from_str_radix(std::str::from_utf8(&rest[..end]).unwrap(), 16).unwrap();
            if length == 0 {
                break;
            }
            rest = &rest[end.checked_add(2).unwrap()..];
            decoded.extend_from_slice(&rest[..length]);
            rest = &rest[length.checked_add(2).unwrap()..];
        }
        (status, decoded)
    } else {
        (status, body.to_vec())
    }
}

async fn browser(addr: SocketAddr, key: &SigningKey, cert: &DeviceCert) -> [u8; 32] {
    let (status, body) = post(
        addr,
        "/session",
        None,
        postcard::to_allocvec(&crate::auth::Hello {
            cert: cert.clone(),
            client_nonce: [9; 32],
        })
        .unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    let challenge: protocol::SessionChallenge = postcard::from_bytes(&body).unwrap();
    let finish = SessionFinish {
        handshake: challenge.handshake,
        ack2: crate::auth::HelloAck2 {
            sig_over_server_nonce: key
                .sign(&crate::auth::challenge_digest(&challenge.ack.server_nonce))
                .to_bytes(),
        },
    };
    let (status, body) = post(
        addr,
        "/session/finish",
        None,
        postcard::to_allocvec(&finish).unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    protocol::decode_grant(&body).unwrap().0.token
}

async fn native(
    relay: &Relay,
    key: &SigningKey,
    cert: &DeviceCert,
    workspace_id: u64,
) -> RelayClient {
    let client = RelayClient::connect_to_pinned_host(
        endpoint(key).await,
        relay.node_addr(),
        cert.clone(),
        key.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("native subscription", async {
        loop {
            if matches!(client.recv().await, Some(Envelope::Ready { .. })) {
                break;
            }
        }
    })
    .await;
    client
}

fn publish(client: &RelayClient, workspace_id: u64, author: [u8; 32], message: &NeighbourMsg) {
    client
        .send(Envelope::Presence {
            workspace_id,
            payload: crate::AttributedPresence {
                author,
                body: postcard::to_allocvec(message).unwrap(),
            }
            .encode()
            .unwrap(),
        })
        .unwrap();
}

fn start(cert: &DeviceCert, surface: u64) -> NeighbourMsg {
    NeighbourMsg::SurfaceStreamStart {
        surface,
        source_peer: tilo_peer_state::peer_handle(&cert.device_public_key),
        width: 640,
        height: 480,
        codec: "h264".into(),
        ts: 1,
        sender_monotonic_ns_at_send: 0,
        colorimetry: None,
        hdr_static_metadata: None,
        bit_depth: 0,
        app_id: "org.chromium.Chromium".into(),
        tab: 0,
    }
}

#[tokio::test]
async fn live_observation_preserves_grants_issued_after_its_snapshot() {
    use std::future::{Future as _, poll_fn};
    use std::task::Poll;
    use tilo_moq::live::{AuthorizationStatus, Denied, LiveAuthority as _};

    let root = SigningKey::from_bytes(&[71; 32]);
    let (host_key, host) = device(&root, 72, DeviceCapabilities::host());
    let (key, cert) = device(&root, 73, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let (relay, sessions) = {
        let relay = Relay::start_with_relay_mode(
            RelayConfig {
                state_root: state.path().into(),
                ..Default::default()
            },
            crate::daemon::RelayIdentity {
                user_public_key: host.user_public_key,
                device_signing_key: host_key,
                device_cert: host,
                revocations: RevocationList::new(),
            },
            iroh::RelayMode::Disabled,
        )
        .await
        .unwrap();
        for id in [71, 72] {
            relay
                .set_workspace_membership(
                    id,
                    true,
                    vec![cert.device_public_key],
                    state.path().into(),
                )
                .await
                .unwrap();
        }
        let sessions = Sessions::new(relay.web_context().clone());
        (relay, sessions)
    };
    let challenge = sessions
        .hello(&crate::auth::Hello {
            cert: cert.clone(),
            client_nonce: [74; 32],
        })
        .unwrap();
    let session = sessions
        .finish(
            &challenge.handshake,
            &crate::auth::HelloAck2 {
                sig_over_server_nonce: key
                    .sign(&crate::auth::challenge_digest(&challenge.ack.server_nonce))
                    .to_bytes(),
            },
        )
        .unwrap();
    let grants = live::LiveGrants::new();
    let authority = live::Authority(grants.clone());
    let issue = |id| {
        grants.issue(
            &sessions,
            session.token,
            cert.device_public_key,
            cert.user_public_key,
            id,
            None,
        )
    };
    assert!(issue(73).await.is_none());
    let first = issue(71).await.unwrap();
    let first_live = authority.try_admit(&first.token).unwrap();
    let (second, second_live) = {
        let workspace = sessions.context().relay.workspaces.lock().await[&71].clone();
        let held = workspace.lock().await;
        let observation = grants.observe(&sessions);
        tokio::pin!(observation);
        // The only grant in the snapshot is now waiting for this workspace.
        assert!(
            poll_fn(|cx| Poll::Ready(observation.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        let second = issue(72).await.unwrap();
        let second_live = authority.try_admit(&second.token).unwrap();
        assert!(
            matches!(second_live.status(), AuthorizationStatus::Current),
            "second_live must remain Current"
        );
        drop(held);
        drop(workspace);
        observation.await;
        (second, second_live)
    };
    assert!(
        matches!(first_live.status(), AuthorizationStatus::Current),
        "first_live must remain Current"
    );
    assert!(
        matches!(second_live.status(), AuthorizationStatus::Current),
        "second_live must remain Current"
    );
    assert!(authority.try_admit(&second.token).is_ok());

    // A later observation must still replace a previously allowed decision.
    relay
        .set_workspace_membership(72, true, vec![], state.path().into())
        .await
        .unwrap();
    grants.observe(&sessions).await;
    assert!(
        matches!(second_live.status(), AuthorizationStatus::Denied),
        "second_live must remain Denied"
    );
    assert!(matches!(authority.try_admit(&second.token), Err(Denied)));
    assert!(issue(72).await.is_none());
    assert!(
        matches!(first_live.status(), AuthorizationStatus::Current),
        "first_live must remain Current"
    );
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: cert.device_public_key,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &root).unwrap();
    sessions
        .context()
        .relay
        .revocations
        .write()
        .unwrap()
        .insert(revoked)
        .unwrap();
    grants.observe(&sessions).await;
    assert!(matches!(first_live.status(), AuthorizationStatus::Denied));
    assert!(matches!(authority.try_admit(&first.token), Err(Denied)));
    assert!(issue(71).await.is_none());
    drop(sessions);
    crate::daemon::Relay::shutdown(relay).await;
}

/// The grant observer runs when an authority change is announced, not on a
/// clock: a member removed from a room loses its live grant at once.
#[tokio::test]
async fn live_grants_follow_announced_changes() {
    use tilo_moq::live::{AuthorizationStatus, LiveAuthority as _};

    let root = SigningKey::from_bytes(&[81; 32]);
    let (host_key, host) = device(&root, 82, DeviceCapabilities::host());
    let (key, cert) = device(&root, 83, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let (relay, sessions) = {
        let relay = Relay::start_with_relay_mode(
            RelayConfig {
                state_root: state.path().into(),
                ..Default::default()
            },
            crate::daemon::RelayIdentity {
                user_public_key: host.user_public_key,
                device_signing_key: host_key,
                device_cert: host,
                revocations: RevocationList::new(),
            },
            iroh::RelayMode::Disabled,
        )
        .await
        .unwrap();
        for id in [81, 82] {
            relay
                .set_workspace_membership(
                    id,
                    true,
                    vec![cert.device_public_key],
                    state.path().into(),
                )
                .await
                .unwrap();
        }
        let sessions = Sessions::new(relay.web_context().clone());
        (relay, sessions)
    };
    let challenge = sessions
        .hello(&crate::auth::Hello {
            cert: cert.clone(),
            client_nonce: [84; 32],
        })
        .unwrap();
    let session = sessions
        .finish(
            &challenge.handshake,
            &crate::auth::HelloAck2 {
                sig_over_server_nonce: key
                    .sign(&crate::auth::challenge_digest(&challenge.ack.server_nonce))
                    .to_bytes(),
            },
        )
        .unwrap();
    let grants = live::LiveGrants::new();
    let authority = live::Authority(grants.clone());
    let observer = tokio::spawn(grants.clone().follow(sessions.clone()));
    let mut held = Vec::new();
    for id in [81, 82] {
        let grant = grants
            .issue(
                &sessions,
                session.token,
                cert.device_public_key,
                cert.user_public_key,
                id,
                None,
            )
            .await
            .unwrap();
        held.push(authority.try_admit(&grant.token).unwrap());
    }
    // Removal announces itself; the observer runs well before its old 50 ms
    // tick plus the carrier's 100 ms recheck.
    relay
        .set_workspace_membership(81, true, vec![], state.path().into())
        .await
        .unwrap();
    let denied = tokio::time::timeout(Duration::from_millis(150), async {
        while matches!(held[0].status(), AuthorizationStatus::Current) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        denied.is_ok(),
        "a removed member's live grant is withdrawn on the announcement"
    );
    assert!(
        matches!(held[1].status(), AuthorizationStatus::Current),
        "the other room's grant is untouched"
    );

    observer.abort();
    drop(sessions);
    crate::daemon::Relay::shutdown(relay).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legacy_http_and_native_paths_enforce_authorship_roles_and_directed_input() {
    let signer = SigningKey::from_bytes(&[61; 32]);
    let other_root = SigningKey::from_bytes(&[62; 32]);
    let (host_key, host) = device(&signer, 63, DeviceCapabilities::host());
    let (owner_key, owner) = device(&signer, 64, DeviceCapabilities::member());
    let (guest_key, guest) = device(&other_root, 65, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let (relay, rooms) = {
        let relay = Relay::start_with_relay_mode(
            RelayConfig {
                state_root: state.path().into(),
                ..Default::default()
            },
            crate::daemon::RelayIdentity {
                user_public_key: host.user_public_key,
                device_signing_key: host_key,
                device_cert: host.clone(),
                revocations: RevocationList::new(),
            },
            iroh::RelayMode::Disabled,
        )
        .await
        .unwrap();
        let rooms = relay.rooms().unwrap().clone();
        (relay, rooms)
    };
    let room = rooms.create("Legacy window test").await.unwrap();
    rooms.enter(&room.room, &owner).await.unwrap();
    let invitation = rooms
        .issue(
            &room.room,
            &owner,
            crate::rooms::IssueInvitationRequest {
                request_id: [1; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: Some(guest.user_public_key),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let verified = tilo_peer_state::join::decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    crate::workspace_join::WorkspaceJoinClient::new(endpoint(&guest_key).await)
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation,
            guest.clone(),
            &guest_key,
            [2; 32],
        )
        .await
        .unwrap();

    let sessions = Sessions::new(relay.web_context().clone());
    let grants = live::LiveGrants::new();
    let live =
        tilo_moq::live::LoopbackLive::start(Arc::new(live::Authority(grants.clone()))).unwrap();
    let legacy = legacy::LegacyWindows::new(live.relay.clone(), host.device_public_key);
    relay
        .web_context()
        .relay
        .live_bridge
        .set(Arc::new(Bridge {
            live: live.relay.clone(),
            legacy: legacy.clone(),
        }))
        .ok()
        .unwrap();
    let web = Arc::new(Web {
        sessions,
        grants,
        legacy: legacy.clone(),
        live: live.relay.clone(),
        host_info: HostInfo {
            host_cert: host.clone(),
            webtransport_port: 0,
            certificate_sha256: None,
        },
        allowed_origins: vec![],
        features: 0,
        rooms: relay.rooms().cloned(),
    });
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(plain_accept_loop(listener, web.clone()));
    drop(web);
    let maintaining = tokio::spawn(maintain_legacy(legacy.clone(), relay.web_context().clone()));
    let token = browser(addr, &owner_key, &owner).await;
    let guest_token = browser(addr, &guest_key, &guest).await;
    // A member learns the device of the session beside the node, once the
    // relay names one.
    let node = |token| async move {
        let (code, body) = post(addr, "/node", Some(token), Vec::new()).await;
        assert_eq!(code, 200);
        postcard::from_bytes::<tilo_relay_protocol::web::NodeInfo>(&body).unwrap()
    };
    assert_eq!(node(token).await.session, None);
    assert_eq!(post(addr, "/node", None, Vec::new()).await.0, 401);
    relay.set_node_session(owner.device_public_key);
    assert_eq!(
        node(guest_token).await.session,
        Some(owner.device_public_key)
    );
    let owner_native = native(&relay, &owner_key, &owner, room.workspace_id).await;
    let guest_native = native(&relay, &guest_key, &guest, room.workspace_id).await;
    let grant = |surface| NeighbourMsg::PermissionGrant {
        peer: tilo_peer_state::peer_handle(&host.device_public_key),
        capability: format!("workspace-control:{surface}:granted"),
    };
    publish(
        &owner_native,
        room.workspace_id,
        owner.device_public_key,
        &grant(7),
    );
    publish(
        &owner_native,
        room.workspace_id,
        owner.device_public_key,
        &start(&owner, 7),
    );
    tilo_test_env::within("authorized native stream", async {
        while legacy
            .publisher_user(room.workspace_id, owner.device_public_key, 7)
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    publish(
        &guest_native,
        room.workspace_id,
        guest.device_public_key,
        &grant(8),
    );
    publish(
        &guest_native,
        room.workspace_id,
        guest.device_public_key,
        &start(&guest, 8),
    );
    publish(
        &owner_native,
        room.workspace_id,
        guest.device_public_key,
        &grant(9),
    );
    publish(
        &owner_native,
        room.workspace_id,
        guest.device_public_key,
        &start(&guest, 9),
    );
    tilo_test_env::within("editor's stream", async {
        while legacy
            .publisher_user(room.workspace_id, guest.device_public_key, 8)
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let request = |events| wire::InputRequest {
        workspace_id: room.workspace_id,
        publisher: owner.device_public_key,
        surface: 7,
        events,
    };
    let input = |events| postcard::to_allocvec(&request(events)).unwrap();
    assert_eq!(
        post(addr, wire::INPUT_ROUTE, None, input(vec![])).await.0,
        401
    );
    assert_eq!(
        post(addr, wire::INPUT_ROUTE, Some(token), input(vec![]))
            .await
            .0,
        204
    );
    assert_eq!(
        post(addr, wire::INPUT_ROUTE, Some(guest_token), input(vec![]))
            .await
            .0,
        204
    );
    assert_eq!(
        post(
            addr,
            wire::INPUT_ROUTE,
            Some(token),
            input(vec![wire::InputEvent::PointerAxis { dx: 0.0, dy: 1.0 }])
        )
        .await
        .0,
        204
    );
    assert_eq!(
        post(
            addr,
            wire::INPUT_ROUTE,
            Some(token),
            input(vec![wire::InputEvent::PointerEnter { surface: 999 }])
        )
        .await
        .0,
        204
    );
    let delivered = tilo_test_env::within("directed native input", async {
        let mut delivered = Vec::new();
        while delivered.len() < 2 {
            if let Some(Envelope::Presence { payload, .. }) = owner_native.recv().await {
                let presence = crate::AttributedPresence::decode(&payload).unwrap();
                if let Ok(NeighbourMsg::RemoteInput { surface, event, .. }) =
                    postcard::from_bytes(&presence.body)
                {
                    assert_eq!(presence.author, host.device_public_key);
                    assert_eq!(surface, 7);
                    delivered.push(postcard::from_bytes::<wire::InputEvent>(&event).unwrap());
                }
            }
        }
        delivered
    })
    .await;
    assert_eq!(
        delivered,
        [
            wire::InputEvent::PointerAxis { dx: 0.0, dy: 1.0 },
            wire::InputEvent::PointerEnter { surface: 7 }
        ]
    );
    // The other native member must never receive private keyboard/input payloads.
    while let Ok(Some(envelope)) =
        tokio::time::timeout(Duration::from_millis(50), guest_native.recv()).await
    {
        if let Envelope::Presence { payload, .. } = envelope {
            let presence = crate::AttributedPresence::decode(&payload).unwrap();
            assert!(!matches!(
                postcard::from_bytes::<NeighbourMsg>(&presence.body),
                Ok(NeighbourMsg::RemoteInput { .. })
            ));
        }
    }
    // A viewer cannot manufacture a stream or keep editor input after a role change.
    rooms
        .set_member_role(
            &room.room,
            &owner,
            crate::rooms::SetMemberRoleRequest {
                request_id: [3; 32],
                user_public_key: guest.user_public_key,
                role: MemberRole::Viewer,
            },
        )
        .await
        .unwrap();
    drop(rooms);
    assert_eq!(
        post(addr, wire::INPUT_ROUTE, Some(guest_token), input(vec![]))
            .await
            .0,
        403
    );
    publish(
        &guest_native,
        room.workspace_id,
        guest.device_public_key,
        &grant(8),
    );
    publish(
        &guest_native,
        room.workspace_id,
        guest.device_public_key,
        &start(&guest, 8),
    );
    publish(
        &guest_native,
        room.workspace_id,
        owner.device_public_key,
        &grant(9),
    );
    publish(
        &guest_native,
        room.workspace_id,
        owner.device_public_key,
        &start(&owner, 9),
    );
    // A successful ordered request is a barrier after the presence messages on this connection.
    owner_native
        .send(Envelope::LiveJoin {
            workspace_id: room.workspace_id,
        })
        .unwrap();
    let workspace = relay
        .web_context()
        .relay
        .workspaces
        .lock()
        .await
        .get(&room.workspace_id)
        .cloned()
        .unwrap();
    tilo_test_env::within("owner live join", async {
        while !workspace
            .lock()
            .await
            .is_live_member(&owner.device_public_key)
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    guest_native
        .send(Envelope::LiveJoin {
            workspace_id: room.workspace_id,
        })
        .unwrap();
    tilo_test_env::within("native presence barrier", async {
        loop {
            if matches!(guest_native.recv().await, Some(Envelope::LiveMember { .. })) {
                break;
            }
        }
    })
    .await;
    tilo_test_env::within("downgraded publisher's streams end", async {
        while legacy
            .publisher_user(room.workspace_id, guest.device_public_key, 8)
            .is_some()
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        legacy
            .publisher_user(room.workspace_id, guest.device_public_key, 8)
            .is_none()
    );
    assert!(
        legacy
            .publisher_user(room.workspace_id, owner.device_public_key, 9)
            .is_none()
    );
    tilo_test_env::within("downgraded publisher's streams end", async {
        while legacy
            .publisher_user(room.workspace_id, guest.device_public_key, 8)
            .is_some()
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        legacy
            .publisher_user(room.workspace_id, guest.device_public_key, 9)
            .is_none()
    );
    maintaining.abort();
    serving.abort();
    owner_native.close().await;
    guest_native.close().await;
    crate::daemon::Relay::shutdown(relay).await;
}

#[test]
fn resource_locator_uses_configured_https_host_and_refuses_wildcards() {
    let files = |host| TlsSource::Files {
        certificate_chain: "unused.pem".into(),
        private_key: "unused.key".into(),
        host,
    };
    let wildcard: SocketAddr = "0.0.0.0:443".parse().unwrap();
    assert_eq!(
        browser_locator_origin(&files(None), wildcard).unwrap(),
        None
    );
    assert_eq!(
        browser_locator_origin(&files(Some("0.0.0.0".into())), wildcard).unwrap(),
        None
    );
    assert_eq!(
        browser_locator_origin(&files(Some("room.example".into())), wildcard).unwrap(),
        Some("https://room.example".into())
    );
    assert_eq!(
        browser_locator_origin(&files(None), "[::1]:8443".parse().unwrap()).unwrap(),
        Some("https://[::1]:8443".into())
    );
    for host in ["user@room.example", "room.example/path", "bad host"] {
        assert!(browser_locator_origin(&files(Some(host.into())), wildcard).is_err());
    }
    let development = TlsSource::Development {
        hosts: vec!["0.0.0.0".into(), "room.example".into()],
        store: None,
    };
    assert_eq!(
        browser_locator_origin(&development, wildcard).unwrap(),
        Some("https://room.example".into())
    );
}
