#![allow(
    clippy::too_many_lines,
    reason = "one authenticated cross-carrier scenario"
)]

use super::*;
use crate::RelayConfig;
use ed25519_dalek::{Signer as _, SigningKey};
use tilo_peer_state::join::MemberRole;
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
    browser_as(addr, key, cert, false).await
}

/// A browser session, in the background when `background`.
async fn browser_as(
    addr: SocketAddr,
    key: &SigningKey,
    cert: &DeviceCert,
    background: bool,
) -> [u8; 32] {
    let (status, body) = post(
        addr,
        "/session",
        None,
        postcard::to_allocvec(&crate::auth::Hello::new(cert.clone(), [9; 32])).unwrap(),
    )
    .await;
    assert_eq!(status, 200);
    let challenge: protocol::SessionChallenge = postcard::from_bytes(&body).unwrap();
    let finish = SessionFinish {
        handshake: challenge.handshake,
        ack2: crate::auth::HelloAck2 {
            sig_over_server_nonce: key
                .sign(&crate::auth::client_challenge_digest(
                    &challenge.ack.server_nonce,
                ))
                .to_bytes(),
        },
        background,
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

/// A page takes its seat in a room by subscribing to it over its session, as
/// the page's own client does once it joined: the room's bootstrap and replay
/// reach the page, then its `Ready`.
#[cfg(feature = "agents")]
async fn subscribe(addr: SocketAddr, token: [u8; 32], workspace_id: u64) {
    let (status, _) = post(
        addr,
        "/send",
        Some(token),
        postcard::to_allocvec(&vec![crate::Envelope::Subscribe { workspace_id }]).unwrap(),
    )
    .await;
    assert_eq!(status, 204, "the page's subscription");
    let mut ack = None;
    let mut ready = false;
    for _ in 0..10 {
        let (status, body) = post(
            addr,
            "/events",
            Some(token),
            postcard::to_allocvec(&protocol::EventsRequest {
                ack,
                wait_ms: 5_000,
            })
            .unwrap(),
        )
        .await;
        assert_eq!(status, 200, "the page's events");
        let page: protocol::EventsPage = postcard::from_bytes(&body).unwrap();
        if page
            .envelopes
            .iter()
            .any(|envelope| matches!(envelope, crate::Envelope::Ready { .. }))
        {
            ready = true;
            break;
        }
        ack = page.cursor;
    }
    assert!(ready, "the room admitted the page");
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

/// A public room's spectator takes a channel's content keys from the node
/// and opens what was sealed there; nobody else does: not before it
/// holds a seat, not a member, not in a room that is not public, and never
/// for a channel nobody created.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay and its rooms serve the page until the relay shuts down at the end"
)]
async fn a_public_rooms_seated_spectator_takes_its_channels_content_keys() {
    let signer = SigningKey::from_bytes(&[71; 32]);
    let watcher = SigningKey::from_bytes(&[72; 32]);
    // A room service's node, which takes part in its rooms' channels.
    let (host_key, host) = device(
        &signer,
        73,
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let (owner_key, owner) = device(&signer, 74, DeviceCapabilities::member());
    let (joined_key, joined) = device(&watcher, 75, DeviceCapabilities::member());
    let (watch_key, watch) = device(&watcher, 76, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
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
    let room = rooms.create("Plaza").await.unwrap();
    rooms.enter(&room.room, &owner).await.unwrap();
    // The spectator person is a viewer of the room, as its host makes it.
    let invitation = rooms
        .issue(
            &room.room,
            &owner,
            crate::rooms::IssueInvitationRequest {
                request_id: [1; 32],
                role: MemberRole::Viewer,
                recipient_user_public_key: Some(joined.user_public_key),
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
    crate::workspace_join::WorkspaceJoinClient::new(endpoint(&joined_key).await)
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation,
            joined.clone(),
            &joined_key,
            [2; 32],
        )
        .await
        .unwrap();
    // A member wrote in the chat's channel.
    let record = "@tilo:package-primary";
    let node = rooms
        .own_route_keys(
            &tilo_peer_state::document_frame::DocumentRoute {
                app_id: "so.tilo.chat".into(),
                cluster_id: room.room.clone(),
                record: record.into(),
            },
            true,
        )
        .await
        .unwrap()
        .unwrap();
    let artifact = collab_types::ArtifactId::new();
    let (metadata, sealed) = node
        .seal_content(artifact, collab_types::ContentRetention::Delta, b"hello")
        .unwrap();

    let sessions = Sessions::new(relay.web_context().clone());
    let web = Arc::new(Web {
        sessions,
        host_info: HostInfo {
            host_cert: host.clone(),
        },
        allowed_origins: vec![],
        features: 0,
        rooms: relay.rooms().cloned(),
        envelopes: envelopes::Envelopes::new(relay.state_root().join("envelopes")),
        profiles: envelopes::Envelopes::of(
            envelopes::Kind::Profile,
            relay.state_root().join("profiles"),
        ),
        here: Arc::new(here::Here::new(None)),
        country: None,
        fetch: None,
        calls: Arc::new(call_lane::CallLane::new(host.device_public_key, None)),
        live: None,
    });
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(plain_accept_loop(listener, web));
    let ask = |token, record: &str| {
        post(
            addr,
            protocol::CHANNEL_KEYS_PATH,
            Some(token),
            postcard::to_allocvec(&protocol::ChannelKeysRequest {
                app_id: "so.tilo.chat".into(),
                room: room.room.clone(),
                record: record.into(),
            })
            .unwrap(),
        )
    };
    let spectators = |public| crate::room_access::Spectators {
        root: watcher.verifying_key().to_bytes(),
        seats: 4,
        public,
    };
    let watching = browser(addr, &watch_key, &watch).await;
    let owner_token = browser(addr, &owner_key, &owner).await;

    // Before the room lets anyone watch, and before the page holds a seat.
    assert_eq!(ask(watching, record).await.0, 403);
    relay
        .admit_spectators(room.workspace_id, spectators(true))
        .await
        .unwrap();
    assert_eq!(
        ask(watching, record).await.0,
        403,
        "a spectator without a seat"
    );
    subscribe(addr, watching, room.workspace_id).await;
    let (code, body) = ask(watching, record).await;
    assert_eq!(code, 200, "{}", String::from_utf8_lossy(&body));
    let keys: protocol::ChannelKeys = postcard::from_bytes(&body).unwrap();
    let keys = channel_crypto::ChannelContentKeys::from_bytes(&keys.keys).unwrap();
    assert_eq!(
        &keys.open_content(artifact, &metadata, &sealed).unwrap()[..],
        b"hello",
        "the spectator opens what a member sealed"
    );
    assert_eq!(
        ask(watching, "@tilo:never-written").await.0,
        403,
        "no channel is created for a spectator"
    );
    assert_eq!(
        ask(owner_token, record).await.0,
        403,
        "a member takes keys over the channel"
    );
    // A room that is not public keeps its members' writing from spectators.
    relay
        .admit_spectators(room.workspace_id, spectators(false))
        .await
        .unwrap();
    assert_eq!(ask(watching, record).await.0, 403);

    serving.abort();
    crate::daemon::Relay::shutdown(relay).await;
}

/// A browser handshake the host turns away says why, in a body the device
/// decodes: a stranger is not admitted, a revoked device is revoked, and a
/// handshake the host does not know is to be run again.
#[tokio::test]
async fn refused_handshakes_carry_their_typed_reason() {
    let root = SigningKey::from_bytes(&[101; 32]);
    let (host_key, host) = device(&root, 102, DeviceCapabilities::host());
    let (_, member) = device(&root, 103, DeviceCapabilities::member());
    let (_, revoked) = device(&root, 104, DeviceCapabilities::member());
    let stranger_root = SigningKey::from_bytes(&[105; 32]);
    let (_, stranger) = device(&stranger_root, 106, DeviceCapabilities::member());
    let mut entry = tilo_user_identity::RevocationEntry {
        user_public_key: root.verifying_key().to_bytes(),
        revoked_device_public_key: revoked.device_public_key,
        revoked_at_ns: 1,
        reason: tilo_user_identity::RevocationReason::Lost,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
    let mut revocations = RevocationList::new();
    revocations.insert(entry).unwrap();
    let state = tempfile::tempdir().unwrap();
    exercise_handshake_refusal_reasons(
        Relay::start_with_relay_mode(
            RelayConfig {
                state_root: state.path().into(),
                ..Default::default()
            },
            crate::daemon::RelayIdentity {
                user_public_key: host.user_public_key,
                device_signing_key: host_key,
                device_cert: host,
                revocations,
            },
            iroh::RelayMode::Disabled,
        )
        .await
        .unwrap(),
        &member,
        &revoked,
        &stranger,
    )
    .await;
}

async fn exercise_handshake_refusal_reasons(
    relay: Relay,
    member: &DeviceCert,
    revoked: &DeviceCert,
    stranger: &DeviceCert,
) {
    let sessions = Sessions::new(relay.web_context().clone());
    let hello = |cert: &DeviceCert| crate::auth::Hello::new(cert.clone(), [7; 32]);
    let refusal = |error: SessionError| {
        // On the wire the reason is the 403's postcard body.
        let response = session_status(&error);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        match error {
            SessionError::Denied(refusal) => refusal,
            other => panic!("not a typed refusal: {other:?}"),
        }
    };
    assert!(sessions.hello(&hello(member)).is_ok(), "the owner's device");
    assert_eq!(
        refusal(sessions.hello(&hello(stranger)).unwrap_err()),
        protocol::SessionRefusal::NotAdmitted
    );
    assert_eq!(
        refusal(sessions.hello(&hello(revoked)).unwrap_err()),
        protocol::SessionRefusal::DeviceRevoked
    );
    let unknown = sessions
        .finish(
            &[0; 16],
            &crate::auth::HelloAck2 {
                sig_over_server_nonce: [0; 64],
            },
            false,
        )
        .map(|_| ())
        .unwrap_err();
    assert_eq!(refusal(unknown), protocol::SessionRefusal::Handshake);
    drop(sessions);
    relay.shutdown().await;
}

/// One request to the listener at `addr`: its status, its header block
/// (lowercased) and its body.
async fn exchange(addr: SocketAddr, head: &str, body: &[u8]) -> (u16, String, Vec<u8>) {
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    socket.write_all(head.as_bytes()).await.unwrap();
    socket.write_all(body).await.unwrap();
    let mut reply = Vec::new();
    tilo_test_env::within("HTTP reply", socket.read_to_end(&mut reply))
        .await
        .unwrap();
    let split = reply
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .unwrap();
    let header = std::str::from_utf8(&reply[..split])
        .unwrap()
        .to_ascii_lowercase();
    let status = header.split_whitespace().nth(1).unwrap().parse().unwrap();
    let mut body = reply[split.checked_add(4).unwrap()..].to_vec();
    if header.contains("transfer-encoding: chunked") {
        let mut decoded = Vec::new();
        let mut rest = body.as_slice();
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
        body = decoded;
    }
    (status, header, body)
}

/// The value of `name` in a lowercased header block.
fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == name).then(|| value.trim())
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the relay and its rooms serve the page until the relay shuts down at the end; only some feature sets see the drop"
)]
async fn the_fetch_proxy_answers_admitted_people_and_refuses_everyone_else() {
    let signer = SigningKey::from_bytes(&[81; 32]);
    let watcher = SigningKey::from_bytes(&[82; 32]);
    let (host_key, host) = device(
        &signer,
        83,
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let (owner_key, owner) = device(&signer, 84, DeviceCapabilities::member());
    let (joined_key, joined) = device(&watcher, 85, DeviceCapabilities::member());
    let (watch_key, watch) = device(&watcher, 86, DeviceCapabilities::member());
    let (laptop_key, laptop) = device(&signer, 87, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
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
    let room = rooms.create("Lounge").await.unwrap();
    rooms.enter(&room.room, &owner).await.unwrap();
    // The spectator person is a viewer of the room, as its host makes it,
    // and every watching page is one of its devices.
    let invitation = rooms
        .issue(
            &room.room,
            &owner,
            crate::rooms::IssueInvitationRequest {
                request_id: [1; 32],
                role: MemberRole::Viewer,
                recipient_user_public_key: Some(joined.user_public_key),
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
    crate::workspace_join::WorkspaceJoinClient::new(endpoint(&joined_key).await)
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation,
            joined.clone(),
            &joined_key,
            [2; 32],
        )
        .await
        .unwrap();

    let sessions = Sessions::new(relay.web_context().clone());
    let web = Arc::new(Web {
        sessions,
        host_info: HostInfo {
            host_cert: host.clone(),
        },
        allowed_origins: vec!["https://app.example".into()],
        features: protocol::FEATURE_FETCH_PROXY,
        rooms: relay.rooms().cloned(),
        envelopes: envelopes::Envelopes::new(relay.state_root().join("envelopes")),
        profiles: envelopes::Envelopes::of(
            envelopes::Kind::Profile,
            relay.state_root().join("profiles"),
        ),
        here: Arc::new(here::Here::new(None)),
        country: None,
        // The target is on loopback, which a host's proxy never reaches; the
        // refusal of its own network is `tilo_net`'s to test. The room may
        // receive what the two answers below carry, together.
        fetch: Some(
            tilo_net::proxy::host::FetchProxy::new(tilo_net::proxy::host::Config {
                destinations: tilo_net::proxy::host::Destinations::Any,
                budgets: tilo_net::proxy::host::Budgets {
                    visitor_bytes: None,
                    room_bytes: Some(26),
                    ..Default::default()
                },
                ..Default::default()
            })
            .unwrap(),
        ),
        calls: Arc::new(call_lane::CallLane::new(host.device_public_key, None)),
        live: None,
    });
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(plain_accept_loop(listener, web));
    let owner_token = browser(addr, &owner_key, &owner).await;
    let watching = browser(addr, &watch_key, &watch).await;
    relay
        .admit_spectators(
            room.workspace_id,
            crate::room_access::Spectators {
                root: watcher.verifying_key().to_bytes(),
                seats: 4,
                public: true,
            },
        )
        .await
        .unwrap();

    let target = test_http::Responder::start(vec![
        test_http::Reply::status(200)
            .header("Content-Type", "application/json")
            .header("Set-Cookie", "tracker=1")
            .body("[{\"title\":\"Trending\"}]"),
        test_http::Reply::status(206)
            .header("Content-Type", "video/mp4")
            .header("Content-Range", "bytes 0-3/9000000")
            .body("mp4!"),
    ]);
    let mut route = tilo_net::Url::parse("http://host/peer/v1/fetch").unwrap();
    route
        .query_pairs_mut()
        .append_pair("url", &format!("{}/api/v1/trending", target.url()));
    let path = format!("{}?{}", route.path(), route.query().unwrap());
    let fetch = |token: Option<String>, origin: &'static str| {
        let token = token.map_or_else(String::new, |token| {
            format!("{SESSION_HEADER}: {token}\r\n")
        });
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: {origin}\r\nx-tilo-proxy: 1\r\n{token}\r\n"
        );
        async move { exchange(addr, &head, b"").await }
    };

    // Nobody signed in, a made-up session and a watching page are refused,
    // in a way the page can read.
    for token in [
        None,
        Some(hex::encode([5; 32])),
        Some(hex::encode(watching)),
    ] {
        let (status, head, _) = fetch(token, "https://app.example").await;
        assert_eq!(status, 401, "{head}");
        assert_eq!(
            header_value(&head, "x-tilo-proxy-error"),
            Some("unauthorized")
        );
        assert_eq!(
            header_value(&head, "access-control-allow-origin"),
            Some("https://app.example")
        );
    }
    // Never `assert_untouched` here: it turns the listener the responder
    // still answers on non-blocking, and its next accept would fail.
    assert!(target.drain_requests().is_empty());

    // A member's page reads the target, every header readable cross-origin
    // and none the host's origin would act on.
    let (status, head, body) = fetch(Some(hex::encode(owner_token)), "https://app.example").await;
    assert_eq!(status, 200, "{head}");
    assert_eq!(body, b"[{\"title\":\"Trending\"}]");
    assert_eq!(
        header_value(&head, "access-control-allow-origin"),
        Some("https://app.example")
    );
    assert_eq!(header_value(&head, "set-cookie"), None);
    assert_eq!(
        header_value(&head, "x-tilo-forward-set-cookie"),
        Some("tracker=1")
    );
    let exposed = header_value(&head, "access-control-expose-headers").unwrap();
    for name in [
        "x-tilo-proxy-final-url",
        "content-type",
        "x-tilo-forward-set-cookie",
    ] {
        assert!(exposed.contains(name), "{name} is not exposed: {exposed}");
    }
    let seen = target.next_request();
    assert_eq!(seen.target, "/api/v1/trending");
    assert_eq!(seen.header("x-tilo-session"), None);
    assert_eq!(seen.header("origin"), None);

    // A media element sends no headers of its own: its GET names the proxy
    // and the session in the query, and only a member's session is served.
    let media = |token: [u8; 32]| {
        let proxy = tilo_net::proxy::Proxy {
            endpoint: tilo_net::Url::parse("http://host/peer/v1/fetch").unwrap(),
            session: Some(hex::encode(token)),
        };
        let url = tilo_net::proxy::media_url(
            &proxy,
            &tilo_net::Url::parse(&format!("{}/videoplayback", target.url())).unwrap(),
        );
        let head = format!(
            "GET {}?{} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://app.example\r\nRange: bytes=0-\r\n\r\n",
            url.path(),
            url.query().unwrap()
        );
        async move { exchange(addr, &head, b"").await }
    };
    let (status, _, _) = media(watching).await;
    assert_eq!(
        status, 401,
        "a watching page streams nothing through the host"
    );
    let (status, head, body) = media(owner_token).await;
    assert_eq!(status, 206, "{head}");
    assert_eq!(body, b"mp4!");
    assert_eq!(
        header_value(&head, "access-control-allow-origin"),
        Some("https://app.example")
    );
    assert_eq!(
        target.next_request().header("range"),
        Some("bytes=0-2097151"),
        "an open-ended range is asked a piece at a time"
    );

    // The room received its budget: another of its people's devices is
    // refused, in a way the page can read, and the target never asked.
    let laptop_token = browser(addr, &laptop_key, &laptop).await;
    let (status, head, _) = fetch(Some(hex::encode(laptop_token)), "https://app.example").await;
    assert_eq!(status, 429, "{head}");
    assert_eq!(
        header_value(&head, "x-tilo-proxy-error"),
        Some("over-budget")
    );
    assert!(header_value(&head, "retry-after").is_some(), "{head}");
    assert!(
        header_value(&head, "access-control-expose-headers")
            .unwrap()
            .contains("x-tilo-proxy-error")
    );
    assert!(target.drain_requests().is_empty());

    // The preflight a page's WebDAV request sends.
    let head = format!(
        "OPTIONS {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://app.example\r\nAccess-Control-Request-Method: PROPFIND\r\nAccess-Control-Request-Headers: depth, x-tilo-proxy, x-tilo-session\r\n\r\n"
    );
    let (status, head, _) = exchange(addr, &head, b"").await;
    assert_eq!(status, 204);
    assert!(
        header_value(&head, "access-control-allow-methods")
            .unwrap()
            .contains("propfind")
    );
    assert_eq!(
        header_value(&head, "access-control-allow-headers"),
        Some("depth, x-tilo-proxy, x-tilo-session")
    );
    // Another site's page gets no CORS answer to read.
    let (_, head, _) = fetch(None, "https://other.example").await;
    assert_eq!(header_value(&head, "access-control-allow-origin"), None);

    serving.abort();
    crate::daemon::Relay::shutdown(relay).await;
}

/// A page's call turn reaches the bridge its host carries, and only a device
/// the workspace admits is answered: the route the deployed page posts to.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay carries the page's calls until it shuts down at the end"
)]
async fn a_page_turns_its_call_at_the_host_and_only_an_admitted_device_is_answered() {
    const CALLER: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
    let signer = SigningKey::from_bytes(&[91; 32]);
    let outsider_root = SigningKey::from_bytes(&[92; 32]);
    let (host_key, host) = device(&signer, 93, DeviceCapabilities::host());
    let (member_key, member) = device(&signer, 94, DeviceCapabilities::member());
    let (outsider_key, outsider) = device(&outsider_root, 95, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let workspace = tilo_relay_protocol::room_workspace_id(CALLER).unwrap();
    let relay = Relay::start_with_relay_mode(
        RelayConfig {
            state_root: state.path().into(),
            // A guest of the host who is not a member of the call's workspace
            // still opens a session: the route, not the handshake, is what
            // keeps them out.
            trust_policy: crate::policy::TrustPolicy::AllowList {
                users: vec![
                    hex::encode(signer.verifying_key().to_bytes()),
                    hex::encode(outsider_root.verifying_key().to_bytes()),
                ],
            },
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
    relay
        .replace_governed_workspace_access(
            workspace,
            true,
            tilo_peer_state::GovernedAccess {
                member_user_roots: vec![signer.verifying_key().to_bytes()],
                member_devices: vec![member.device_public_key],
            },
            state.path().into(),
        )
        .await
        .unwrap();
    let sessions = Sessions::new(relay.web_context().clone());
    // The lanes' endpoint keeps the route offline: a call with no other
    // device's lane open answers the page's turn with nothing carried.
    let lane = endpoint(&SigningKey::from_bytes(&[96; 32])).await;
    let web = Arc::new(Web {
        sessions,
        host_info: HostInfo {
            host_cert: host.clone(),
        },
        allowed_origins: vec![],
        features: 0,
        rooms: relay.rooms().cloned(),
        envelopes: envelopes::Envelopes::new(relay.state_root().join("envelopes")),
        profiles: envelopes::Envelopes::of(
            envelopes::Kind::Profile,
            relay.state_root().join("profiles"),
        ),
        here: Arc::new(here::Here::new(None)),
        country: None,
        fetch: None,
        calls: Arc::new(call_lane::CallLane::at(lane)),
        live: None,
    });
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(plain_accept_loop(listener, web.clone()));
    drop(web);

    let turn = |workspace: &str| {
        postcard::to_allocvec(&tilo_call_lane::page::Up {
            workspace: workspace.into(),
            call: "the call".into(),
            channel: collab_types::ChannelId::default(),
            audio: vec![],
            pictures: vec![],
            shows: vec![],
            keyframes: vec![],
        })
        .unwrap()
    };
    let token = browser(addr, &member_key, &member).await;
    let (code, body) = post(addr, tilo_call_lane::page::PATH, Some(token), turn(CALLER)).await;
    assert_eq!(code, 200);
    let down: tilo_call_lane::page::Down = postcard::from_bytes(&body).unwrap();
    assert!(down.audio.is_empty());
    assert!(down.pictures.is_empty());

    assert_eq!(
        post(addr, tilo_call_lane::page::PATH, None, turn(CALLER))
            .await
            .0,
        401
    );
    let outsider_token = browser(addr, &outsider_key, &outsider).await;
    assert_eq!(
        post(
            addr,
            tilo_call_lane::page::PATH,
            Some(outsider_token),
            turn(CALLER)
        )
        .await
        .0,
        403
    );
    assert_eq!(
        post(
            addr,
            tilo_call_lane::page::PATH,
            Some(token),
            turn("Somewhere else")
        )
        .await
        .0,
        400
    );

    serving.abort();
    crate::daemon::Relay::shutdown(relay).await;
}

/// The live plane a page joins: the relay's own WebTransport listener, a grant
/// issued only to a page whose device the workspace admits, and a session cut
/// when that device is removed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay carries the page's live session until it shuts down at the end"
)]
async fn a_pages_live_grant_is_issued_to_an_admitted_device_and_cut_on_removal() {
    use base64::Engine as _;
    use channel_crypto::{ChannelGroup, MemberKey};
    use sha2::Digest as _;
    use tilo_call_lane::Roster;
    use tilo_live::live::Track;
    use tilo_live::plane::{Join, Plane, PlaneError};
    use tilo_live::scope::Scope;
    use tilo_relay_protocol::web::{LiveInfo, LiveRequest};

    const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
    let signer = SigningKey::from_bytes(&[71; 32]);
    let outsider_root = SigningKey::from_bytes(&[72; 32]);
    let (host_key, host) = device(&signer, 73, DeviceCapabilities::host());
    let (member_key, member) = device(&signer, 74, DeviceCapabilities::member());
    let (outsider_key, outsider) = device(&outsider_root, 75, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let workspace = tilo_relay_protocol::room_workspace_id(WORKSPACE).unwrap();
    let relay = Relay::start_with_relay_mode(
        RelayConfig {
            state_root: state.path().into(),
            // A guest of the host who is not a member of the workspace still
            // opens a session: the route, not the handshake, keeps them out.
            trust_policy: crate::policy::TrustPolicy::AllowList {
                users: vec![
                    hex::encode(signer.verifying_key().to_bytes()),
                    hex::encode(outsider_root.verifying_key().to_bytes()),
                ],
            },
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
    let admit = |devices: Vec<[u8; 32]>| tilo_peer_state::GovernedAccess {
        member_user_roots: vec![signer.verifying_key().to_bytes()],
        member_devices: devices,
    };
    relay
        .replace_governed_workspace_access(
            workspace,
            true,
            admit(vec![member.device_public_key]),
            state.path().into(),
        )
        .await
        .unwrap();

    // The daemon's own live listener, a development certificate as a host on
    // its own machine has, serving its routes on a loopback port.
    let sessions = Sessions::new(relay.web_context().clone());
    let config = WebConfig {
        listen: (std::net::Ipv4Addr::LOCALHOST, 0).into(),
        tls: TlsSource::Development {
            hosts: vec!["localhost".into(), "127.0.0.1".into()],
            store: None,
        },
        allowed_origins: Vec::new(),
        plain_listen: None,
        public_origin: None,
        limits: super::WebLimits::default(),
        country: None,
        fetch_proxy: None,
    };
    let identity = tls::Identity::load(&config.tls).unwrap();
    let plane = relay.plane.as_ref().expect("the relay runs a live plane");
    plane.pages.serve_pages(sessions.clone());
    let live = wt::Live::start(plane, &identity, &config, "https://127.0.0.1").unwrap();
    let answered = live.info.clone();
    let lane = endpoint(&SigningKey::from_bytes(&[76; 32])).await;
    let web = Arc::new(Web {
        sessions,
        host_info: HostInfo {
            host_cert: host.clone(),
        },
        allowed_origins: vec![],
        features: 0,
        rooms: relay.rooms().cloned(),
        envelopes: envelopes::Envelopes::new(relay.state_root().join("envelopes")),
        profiles: envelopes::Envelopes::of(
            envelopes::Kind::Profile,
            relay.state_root().join("profiles"),
        ),
        here: Arc::new(here::Here::new(None)),
        country: None,
        fetch: None,
        calls: Arc::new(call_lane::CallLane::at(lane)),
        live: Some(Arc::new(live)),
    });
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(plain_accept_loop(listener, web.clone()));
    drop(web);

    let ask = |workspace_of: &str| {
        postcard::to_allocvec(&LiveRequest {
            workspace_id: workspace,
            workspace: workspace_of.into(),
        })
        .unwrap()
    };

    // No session, no grant; a device the workspace does not admit gets none
    // either, though its session is open; and not in a workspace this relay
    // does not number.
    assert_eq!(post(addr, "/live", None, ask(WORKSPACE)).await.0, 401);
    let outsider_token = browser(addr, &outsider_key, &outsider).await;
    assert_eq!(
        post(addr, "/live", Some(outsider_token), ask(WORKSPACE))
            .await
            .0,
        403
    );
    let member_token = browser(addr, &member_key, &member).await;
    assert_eq!(
        post(addr, "/live", Some(member_token), ask("Somewhere else"))
            .await
            .0,
        400
    );

    // The admitted device's page is told where the listener is and which
    // certificate it presents.
    let (code, body) = post(addr, "/live", Some(member_token), ask(WORKSPACE)).await;
    assert_eq!(code, 200);
    let info: LiveInfo = postcard::from_bytes(&body).unwrap();
    assert_eq!(info, answered);
    assert!(info.webtransport_port > 0);
    assert_eq!(
        info.webtransport_origin,
        format!("https://127.0.0.1:{}", info.webtransport_port)
    );
    let leaf = sha2::Sha256::digest(identity.chain.first().unwrap().as_ref());
    assert_eq!(
        info.certificate_sha256,
        base64::engine::general_purpose::STANDARD.encode(leaf)
    );

    // The page joins that listener with its grant and publishes presence
    // under the scope's keys, as the page's own carrier does.
    let certificate: Vec<u8> = base64::engine::general_purpose::STANDARD
        .decode(&info.certificate_sha256)
        .unwrap();
    let carrier = tilo_live::carrier::WebTransport::new(
        &format!("{}{}", info.webtransport_origin, tilo_live::preface::PATH),
        &[hex::encode(certificate)],
    )
    .unwrap();
    let grant = tilo_relay_protocol::web::LiveGrant {
        workspace_id: workspace,
        token: member_token,
    };
    let keys = ChannelGroup::create(
        collab_types::ChannelId::new(),
        MemberKey::generate("alice").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap();
    let plane = Plane::join(
        &carrier,
        &grant.encode(),
        Join {
            scope: Scope::workspace(WORKSPACE),
            device: member.device_public_key,
            keys,
            roster: Roster::new([member.device_public_key]),
            publish: true,
        },
    )
    .await
    .unwrap();
    plane.publish(Track::Presence, 0, b"role=member").unwrap();

    // Removing the device from the workspace cuts its page's session, and the
    // route refuses it from then on.
    relay
        .replace_governed_workspace_access(workspace, true, admit(Vec::new()), state.path().into())
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !plane.ended() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(
        plane.ended(),
        "the removed device's live session stayed open"
    );
    assert_eq!(
        plane.publish(Track::Presence, 1, b"role=member"),
        Err(PlaneError::Ended)
    );
    assert_eq!(
        post(addr, "/live", Some(member_token), ask(WORKSPACE))
            .await
            .0,
        403
    );

    serving.abort();
    crate::daemon::Relay::shutdown(relay).await;
}
