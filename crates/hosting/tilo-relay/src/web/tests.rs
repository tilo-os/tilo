#![allow(
    clippy::too_many_lines,
    reason = "one authenticated cross-carrier scenario"
)]

use super::*;
use crate::RelayConfig;
use crate::peer::FetchGrants as _;
use ed25519_dalek::SigningKey;
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

/// The node a test's relay runs as: device `seed` of `root`, certified with
/// `capabilities`.
struct Host {
    key: SigningKey,
    cert: DeviceCert,
}

fn host(root: &SigningKey, seed: u8, capabilities: DeviceCapabilities) -> Host {
    let (key, cert) = device(root, seed, capabilities);
    Host { key, cert }
}

impl Host {
    /// What the relay presents as its host node.
    fn identity(&self) -> crate::daemon::RelayIdentity {
        crate::daemon::RelayIdentity {
            user_public_key: self.cert.user_public_key,
            device_signing_key: self.key.clone(),
            device_cert: self.cert.clone(),
            revocations: RevocationList::new(),
        }
    }
}

/// A relay keeping its state under `state`, identified as `identity`.
async fn relay_at(
    state: &tempfile::TempDir,
    identity: crate::daemon::RelayIdentity,
    config: RelayConfig,
) -> Relay {
    Relay::start_with_relay_mode(
        RelayConfig {
            state_root: state.path().into(),
            ..config
        },
        identity,
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap()
}

/// The web `relay` serves its pages, with every door a test does not open
/// closed.
fn web_of(relay: &Relay, host: &Host) -> Web {
    Web {
        context: relay.web_context().clone(),
        host_info: HostInfo {
            host_cert: host.cert.clone(),
            address: None,
        },
        allowed_origins: vec![],
        workspaces: relay.workspaces().cloned(),
        envelopes: envelopes::Envelopes::new(relay.state_root().join("envelopes")),
        profiles: Arc::new(envelopes::Envelopes::of(
            envelopes::Record::Profile,
            relay.state_root().join("profiles"),
        )),
        here: Arc::new(here::Here::new(None)),
        fetch: None,
        grants: Arc::default(),
        presence: Arc::new(plane::Authority::new(relay.web_context().relay.clone())),
        page_errors: page_errors::PageErrors::default(),
        shelf: shelf::Shelf::default(),
    }
}

/// Serve `web` on a loopback port in plain HTTP, as a page reaches it.
async fn serve_web(web: Arc<Web>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    (addr, tokio::spawn(plain_accept_loop(listener, web)))
}

/// Stop serving `serving` and shut `relay` down.
async fn stop(relay: Relay, serving: tokio::task::JoinHandle<()>) {
    serving.abort();
    crate::daemon::Relay::shutdown(relay).await;
}

#[cfg(feature = "agents")]
async fn post(addr: SocketAddr, path: &str, body: Vec<u8>) -> (u16, Vec<u8>) {
    let mut socket = tokio::net::TcpStream::connect(addr).await.unwrap();
    let header = format!(
        "POST {API_PREFIX}{path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
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
        browser_locator_origin(&files(Some("workspace.example".into())), wildcard).unwrap(),
        Some("https://workspace.example".into())
    );
    assert_eq!(
        browser_locator_origin(&files(None), "[::1]:8443".parse().unwrap()).unwrap(),
        Some("https://[::1]:8443".into())
    );
    for host in [
        "user@workspace.example",
        "workspace.example/path",
        "bad host",
    ] {
        assert!(browser_locator_origin(&files(Some(host.into())), wildcard).is_err());
    }
    let development = TlsSource::Development {
        hosts: vec!["0.0.0.0".into(), "workspace.example".into()],
        store: None,
    };
    assert_eq!(
        browser_locator_origin(&development, wildcard).unwrap(),
        Some("https://workspace.example".into())
    );
}

/// No route hands a page the content keys of a record's channel. A page takes
/// part in its records' encrypted channels like any device: the keys it opens
/// them with are the ones the group admits it with, and the host hands none.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay and its workspaces serve the page until the relay shuts down at the end"
)]
async fn no_route_hands_a_page_the_keys_of_a_channels_record() {
    let signer = SigningKey::from_bytes(&[71; 32]);
    // A workspace service's node, which takes part in its workspaces' channels.
    let host = host(
        &signer,
        73,
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let (_, owner) = device(&signer, 74, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = relay_at(&state, host.identity(), RelayConfig::default()).await;
    let workspaces = relay.workspaces().unwrap().clone();
    let workspace = workspaces.create("Plaza").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &owner)
        .await
        .unwrap();

    let (addr, serving) = serve_web(Arc::new(web_of(&relay, &host))).await;
    let (code, body) = post(addr, "/channel-keys", Vec::new()).await;
    assert_eq!(
        code,
        404,
        "a page is handed no content keys over HTTPS: {}",
        String::from_utf8_lossy(&body)
    );

    stop(relay, serving).await;
}

/// The public directory and its doors answer any site's page without
/// credentials, and a full door says when to come back: what a page on
/// another origin reads and does without going through a proxy.
#[tokio::test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay governs the workspace until it shuts down at the end"
)]
async fn the_public_directory_answers_any_site_and_a_full_door_says_when() {
    let root = SigningKey::from_bytes(&[121; 32]);
    let host = host(
        &root,
        122,
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let state = tempfile::tempdir().unwrap();
    let relay = relay_at(
        &state,
        host.identity(),
        RelayConfig {
            max_workspace_grants: Some(1),
            ..Default::default()
        },
    )
    .await;
    let workspaces = relay.workspaces().unwrap().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    workspaces
        .publish(
            &workspace,
            &host.cert,
            tilo_relay_protocol::directory::Listing {
                description: "Coffee and tea".into(),
                default_role: tilo_relay_protocol::workspaces::WorkspaceRole::Editor,
            },
        )
        .await
        .unwrap();
    let (addr, serving) = serve_web(Arc::new(web_of(&relay, &host))).await;

    // The directory, from a site the host never configured.
    let head = format!(
        "POST {API_PREFIX}/directory HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://someone.example\r\nContent-Length: 0\r\n\r\n"
    );
    let (status, head, body) = exchange(addr, &head, b"").await;
    assert_eq!(status, 200, "{head}");
    assert_eq!(
        header_value(&head, "access-control-allow-origin"),
        Some("*")
    );
    assert_eq!(
        header_value(&head, "access-control-allow-credentials"),
        None,
        "anyone reads it, nobody is signed in by it"
    );
    let page: tilo_relay_protocol::directory::DirectoryPage =
        serde_json::from_slice(&body).unwrap();
    assert_eq!(page.workspaces.len(), 1);
    assert_eq!(page.workspaces[0].description, "Coffee and tea");

    let ask = |person: [u8; 32]| {
        let body = serde_json::json!({
            "userPublicKey": hex::encode(person),
            "displayName": "Cy",
        })
        .to_string();
        let head = format!(
            "POST {API_PREFIX}/directory/{workspace}/join HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://someone.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        async move { exchange(addr, &head, body.as_bytes()).await }
    };
    let (status, head, body) = ask([31; 32]).await;
    assert_eq!(status, 200, "{head}");
    assert_eq!(
        header_value(&head, "access-control-allow-origin"),
        Some("*")
    );
    let answer: tilo_relay_protocol::directory::DoorAnswer = serde_json::from_slice(&body).unwrap();
    assert!(!answer.invitation.is_empty());

    // The door keeps one open grant: the next person is told when to come back,
    // and the page may read that header cross-origin.
    let (status, head, body) = ask([32; 32]).await;
    assert_eq!(status, 503, "{head}");
    assert!(
        header_value(&head, "retry-after").is_some(),
        "no Retry-After: {head}"
    );
    assert!(
        header_value(&head, "access-control-expose-headers")
            .unwrap_or_default()
            .contains("retry-after"),
        "{head}"
    );
    assert_eq!(
        header_value(&head, "access-control-allow-origin"),
        Some("*")
    );
    eprintln!("full door: {}", String::from_utf8_lossy(&body));

    // The preflight a page on another site sends before asking.
    let head = format!(
        "OPTIONS {API_PREFIX}/directory/{workspace}/join HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://someone.example\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: content-type\r\n\r\n"
    );
    let (status, head, _) = exchange(addr, &head, b"").await;
    assert_eq!(status, 204, "{head}");
    assert_eq!(
        header_value(&head, "access-control-allow-origin"),
        Some("*")
    );
    assert!(
        header_value(&head, "access-control-allow-methods")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("post"),
        "{head}"
    );

    // A page's own uncaught error goes to the door that let it in: the host
    // logs it, and answers the same whether or not it was a report, so a
    // failing page never asks twice.
    let report = serde_json::json!({
        "kind": "error", "message": "x is not a function", "source": "https://someone.example/v/7/page.js",
        "line": 12, "column": 4, "stack": "at page.js:12", "page": "/", "build": "7",
    })
    .to_string();
    let head = format!(
        "POST {API_PREFIX}/directory/{workspace}/errors HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://someone.example\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
        report.len()
    );
    let (status, head, body) = exchange(addr, &head, report.as_bytes()).await;
    assert_eq!(status, 204, "{head}");
    assert!(body.is_empty(), "a report is logged here, not stored");
    assert_eq!(
        header_value(&head, "access-control-allow-origin"),
        Some("*"),
        "any site's page reports to its own door"
    );
    let head = format!(
        "POST {API_PREFIX}/directory/{workspace}/errors HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 8\r\n\r\n"
    );
    let (status, head, _) = exchange(addr, &head, b"not json").await;
    assert_eq!(status, 204, "{head}");

    stop(relay, serving).await;
}

/// The shelf answers a site's own `/i/<handle>`: an inviter leaves sealed
/// bytes, a redeemer fetches exactly those, and the inviter takes them back.
/// The site reads no key, because no request carries one.
#[tokio::test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay holds the shelf until it shuts down at the end"
)]
async fn a_sealed_invitation_is_left_fetched_and_taken_back() {
    use tilo_relay_protocol::sealed_invitations::{Fetched, Stored, TakeBack};

    let root = SigningKey::from_bytes(&[131; 32]);
    let host = host(
        &root,
        132,
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let state = tempfile::tempdir().unwrap();
    let relay = relay_at(&state, host.identity(), RelayConfig::default()).await;
    let (addr, serving) = serve_web(Arc::new(web_of(&relay, &host))).await;

    let handle = "abcdefghijklmnop";
    let delete = "a".repeat(43);
    let sealed = "c2VhbGVkIGludml0YXRpb24";
    let stored = serde_json::to_vec(&Stored {
        sealed: sealed.to_owned(),
        expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
        delete: delete.clone(),
        host: None,
    })
    .unwrap();
    // Below the site's own path, as `entry.json` names it, and with no
    // credentials: the inviter's device signs nothing at the site.
    let head = format!(
        "POST /i/{handle} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        stored.len()
    );
    let (status, head, _) = exchange(addr, &head, &stored).await;
    assert_eq!(status, 204, "{head}");
    assert_eq!(
        header_value(&head, "access-control-allow-origin"),
        Some("*"),
        "any site's page may leave one"
    );

    let (status, head, body) = exchange(
        addr,
        &format!(
            "GET /i/{handle} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://app.example\r\n\r\n"
        ),
        b"",
    )
    .await;
    assert_eq!(status, 200, "{head}");
    assert_eq!(
        header_value(&head, "content-type"),
        Some("application/json")
    );
    let fetched: Fetched = serde_json::from_slice(&body).unwrap();
    assert_eq!(fetched.sealed, sealed);
    assert!(
        !String::from_utf8_lossy(&body).contains(&delete),
        "the site gives back no delete secret"
    );

    // A preflight from a page on another site.
    let (status, head, _) = exchange(
        addr,
        &format!(
            "OPTIONS /i/{handle} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://app.example\r\nAccess-Control-Request-Method: DELETE\r\n\r\n"
        ),
        b"",
    )
    .await;
    assert_eq!(status, 204, "{head}");
    assert!(
        header_value(&head, "access-control-allow-methods")
            .unwrap_or_default()
            .contains("delete"),
        "{head}"
    );

    // A stranger holding the link can fetch, but takes nothing back.
    let wrong = serde_json::to_vec(&TakeBack {
        delete: "b".repeat(43),
    })
    .unwrap();
    let head = format!(
        "DELETE /i/{handle} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        wrong.len()
    );
    let (status, head, _) = exchange(addr, &head, &wrong).await;
    assert_eq!(status, 403, "{head}");

    let right = serde_json::to_vec(&TakeBack {
        delete: delete.clone(),
    })
    .unwrap();
    let head = format!(
        "DELETE /i/{handle} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        right.len()
    );
    let (status, head, _) = exchange(addr, &head, &right).await;
    assert_eq!(status, 204, "{head}");

    // The link now names an invitation that is gone, and says so.
    let (status, head, body) = exchange(
        addr,
        &format!("GET /i/{handle} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"),
        b"",
    )
    .await;
    assert_eq!(status, 410, "{head}");
    assert!(
        String::from_utf8_lossy(&body).contains("gone"),
        "the refusal says the invitation is gone: {}",
        String::from_utf8_lossy(&body)
    );

    stop(relay, serving).await;
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
    reason = "the relay and its workspaces serve the page until the relay shuts down at the end; only some feature sets see the drop"
)]
async fn the_fetch_proxy_answers_admitted_people_and_refuses_everyone_else() {
    let signer = SigningKey::from_bytes(&[81; 32]);
    let watcher = SigningKey::from_bytes(&[82; 32]);
    let host = host(
        &signer,
        83,
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let (_, owner) = device(&signer, 84, DeviceCapabilities::member());
    let (joined_key, joined) = device(&watcher, 85, DeviceCapabilities::member());
    let (_, watch) = device(&watcher, 86, DeviceCapabilities::member());
    let (_, laptop) = device(&signer, 87, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = relay_at(&state, host.identity(), RelayConfig::default()).await;
    let workspaces = relay.workspaces().unwrap().clone();
    let workspace = workspaces.create("Lounge").await.unwrap();
    workspaces
        .enter(&workspace.workspace, &owner)
        .await
        .unwrap();
    // Another person is a viewer of the workspace, as an invitation makes
    // them, and one of their devices joins; their other device does not.
    let invitation = workspaces
        .issue(
            &workspace.workspace,
            &owner,
            crate::workspaces::IssueInvitationRequest {
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

    let mut web = web_of(&relay, &host);
    web.allowed_origins = vec!["https://app.example".into()];
    // The target is on loopback, which a host's proxy never reaches; the
    // refusal of its own network is `tilo_net`'s to test. The workspace may
    // receive what the two answers below carry, together.
    web.fetch = Some(
        tilo_net::proxy::host::FetchProxy::new(tilo_net::proxy::host::Config {
            destinations: tilo_net::proxy::host::Destinations::Any,
            budgets: tilo_net::proxy::host::Budgets {
                visitor_bytes: None,
                room_bytes: Some(52),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap(),
    );
    // The tokens a page's sessions take as they start, over their own
    // carrier: one per connection, and the person it names is what the proxy
    // admits.
    let owner_token = web
        .grants
        .grant(owner.user_public_key, owner.device_public_key)
        .unwrap();
    let visiting = web
        .grants
        .grant(watch.user_public_key, watch.device_public_key)
        .unwrap();
    let laptop_token = web
        .grants
        .grant(laptop.user_public_key, laptop.device_public_key)
        .unwrap();
    let (addr, serving) = serve_web(Arc::new(web)).await;

    let target = test_http::Responder::start(vec![
        test_http::Reply::status(200)
            .header("Content-Type", "application/json")
            .header("Set-Cookie", "tracker=1")
            .body("[{\"title\":\"Trending\"}]"),
        test_http::Reply::status(200)
            .header("Content-Type", "application/json")
            .header("Set-Cookie", "tracker=1")
            .body("[{\"title\":\"Trending\"}]"),
        test_http::Reply::status(206)
            .header("Content-Type", "video/mp4")
            .header("Content-Range", "bytes 0-3/9000000")
            .body("mp4!"),
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
            format!("{}: {token}\r\n", tilo_relay_protocol::web::SESSION_HEADER)
        });
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: {origin}\r\nx-tilo-proxy: 1\r\n{token}\r\n"
        );
        async move { exchange(addr, &head, b"").await }
    };

    // Nobody signed in and a made-up session are refused, in a way the page
    // can read.
    for token in [None, Some(hex::encode([5; 32]))] {
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

    // A page of a member with the Viewer role reads through the host like any
    // other: what it may load is bounded by its device's and its workspace's
    // budgets, never by its role.
    let (status, head, body) = fetch(Some(hex::encode(visiting)), "https://app.example").await;
    assert_eq!(status, 200, "{head}");
    assert_eq!(body, b"[{\"title\":\"Trending\"}]");
    assert_eq!(
        header_value(&head, "access-control-allow-origin"),
        Some("https://app.example")
    );
    let seen = target.next_request();
    assert_eq!(seen.target, "/api/v1/trending");
    assert_eq!(seen.header("x-tilo-session"), None);

    // A media element sends no headers of its own: its GET names the proxy
    // and the session in the query.
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
    let (status, head, body) = media(visiting).await;
    assert_eq!(status, 206, "{head}");
    assert_eq!(body, b"mp4!");
    assert_eq!(
        target.next_request().header("range"),
        Some("bytes=0-2097151"),
        "an open-ended range is asked a piece at a time"
    );

    // The workspace received its budget: another of its people's devices is
    // refused, in a way the page can read, and the target never asked.
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

    stop(relay, serving).await;
}
