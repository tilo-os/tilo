#![allow(
    clippy::too_many_lines,
    reason = "one authenticated cross-carrier scenario"
)]

use super::*;
use crate::device::keep::server::KeptServer;
use ed25519_dalek::SigningKey;
use std::sync::Arc;
use tilo_peer_state::join::Role;
use tilo_server_protocol::config::ServerConfig;
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
        issuer: None,
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

/// The node a test's server runs as: device `seed` of `root`, certified with
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
    /// What the server presents as its host node.
    fn identity(&self) -> crate::device::keep::server::ServerIdentity {
        crate::device::keep::server::ServerIdentity {
            user_public_key: self.cert.user_public_key,
            device_signing_key: self.key.clone(),
            device_cert: self.cert.clone(),
            revocations: RevocationList::new(),
        }
    }
}

/// A server keeping its state under `state`, identified as `identity`.
async fn server_at(
    state: &tempfile::TempDir,
    identity: crate::device::keep::server::ServerIdentity,
    config: ServerConfig,
) -> KeptServer {
    KeptServer::with_relay_mode(
        ServerConfig {
            state_root: state.path().into(),
            ..config
        }
        .on_loopback(),
        identity,
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap()
}

/// The web `server` serves its pages, with every entry a test does not open
/// closed.
fn web_of(server: &Server, host: &Host) -> Web {
    web_of_at(server, host, None)
}

/// [`web_of`] for a host that belongs to `site`.
fn web_of_at(server: &Server, host: &Host, site: Option<&str>) -> Web {
    Web {
        context: server.web_context().clone(),
        server_info: ServerInfo {
            server_cert: host.cert.clone(),
            address: None,
            origin: None,
        },
        endpoint: None,
        allowed_origins: vec![],
        workspaces: server.workspaces().cloned(),
        envelopes: envelopes::Envelopes::new(server.state_root().join("envelopes")),
        profiles: Arc::new(envelopes::Envelopes::of(
            envelopes::Kind::Profile,
            server.state_root().join("profiles"),
        )),
        fetch: None,
        grants: Arc::default(),
        presence: server
            .live_session()
            .expect("the server runs a live session")
            .shared(),
        page_errors: page_errors::PageErrors::default(),
        site: site.and_then(Site::new),
        state_root: server.state_root().to_path_buf(),
        served_lists: ServedLists::default(),
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

/// Stop serving `serving` and shut `server` down.
async fn stop(server: KeptServer, serving: tokio::task::JoinHandle<()>) {
    serving.abort();
    server.shutdown().await;
}

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

tilo_test_env::scenario! {
    fn the_web_listener_names_its_origin_from_what_is_configured;

    phase a_link_origin_naming_port_zero_takes_the_port_the_listener_bound;
    {
        assert_eq!(
            named_port("https://127.0.0.1:0", 43123),
            "https://127.0.0.1:43123"
        );
        assert_eq!(
            named_port("https://[::1]:0/some/path", 443),
            "https://[::1]:443/some/path"
        );
        // A published origin is reached exactly as written, whatever port the
        // listener behind the ingress bound.
        assert_eq!(
            named_port("https://link.tilo.so", 43123),
            "https://link.tilo.so"
        );
        assert_eq!(
            named_port("https://link.tilo.so:8443", 43123),
            "https://link.tilo.so:8443"
        );
    }

    phase resource_locator_uses_configured_https_host_and_refuses_wildcards;
    {
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
        let local = TlsSource::Local {
            hosts: vec!["0.0.0.0".into(), "workspace.example".into()],
            store: None,
        };
        assert_eq!(
            browser_locator_origin(&local, wildcard).unwrap(),
            Some("https://workspace.example".into())
        );
    }
}

/// No route hands a page the content keys of a record's channel. A page takes
/// part in its records' encrypted channels like any device: the keys it opens
/// them with are the ones the group admits it with, and the host hands none.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_route_hands_a_page_the_keys_of_a_channels_record() {
    let signer = SigningKey::from_bytes(&[71; 32]);
    // A workspace service's node, which takes part in its workspaces' channels.
    let host = host(
        &signer,
        73,
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let (owner_key, owner) = device(&signer, 74, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(&state, host.identity(), ServerConfig::default()).await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Plaza").await.unwrap();
    workspaces
        .enter(&workspace.workspace, (&owner, &owner_key))
        .await
        .unwrap();

    let (addr, serving) = serve_web(Arc::new(web_of(&server, &host))).await;
    let (code, body) = post(addr, "/channel-keys", Vec::new()).await;
    assert_eq!(
        code,
        404,
        "a page is handed no content keys over HTTPS: {}",
        String::from_utf8_lossy(&body)
    );

    stop(server, serving).await;
}

/// A workspace its owner deleted is said to be deleted to any device that
/// names it, though the host no longer lets in who was in it; a workspace that
/// stays, or that the host never held, is not.
#[tokio::test]
async fn the_host_says_a_workspace_was_deleted_to_whoever_names_it() {
    let root = SigningKey::from_bytes(&[131; 32]);
    let host = host(&root, 132, tilo_user_identity::DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(&state, host.identity(), ServerConfig::default()).await;
    let workspaces = server.workspaces().unwrap().clone();
    let deleted = workspaces.create("Applicant").await.unwrap().workspace;
    let kept = workspaces
        .create("Other applicant")
        .await
        .unwrap()
        .workspace;
    workspaces.delete(&deleted).await.unwrap();
    let (addr, serving) = serve_web(Arc::new(web_of(&server, &host))).await;

    let path = tilo_server_protocol::web::WORKSPACE_DELETED_PATH;
    let asked = |workspace: &str| post(addr, path, postcard::to_allocvec(workspace).unwrap());
    for (workspace, said) in [
        (deleted.as_str(), true),
        (kept.as_str(), false),
        ("00000000-0000-4000-8000-000000000000", false),
        ("not a workspace", false),
    ] {
        let (status, body) = asked(workspace).await;
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            postcard::from_bytes::<bool>(&body).unwrap(),
            said,
            "{workspace}"
        );
    }
    let (status, _) = post(addr, path, vec![0xff]).await;
    assert_eq!(status, 400, "a body that names nothing is malformed");

    stop(server, serving).await;
}

/// The public directory and its entries answer any site's page without
/// credentials: what a page on another origin reads and does without going
/// through a proxy. Nobody is let in over HTTP: a device enters through the
/// entry on its session with the host, so the entry has no `join` route.
#[tokio::test]
async fn the_public_directory_answers_any_site_and_lets_nobody_in_over_http() {
    let root = SigningKey::from_bytes(&[121; 32]);
    let host = host(&root, 122, tilo_user_identity::DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(&state, host.identity(), ServerConfig::default()).await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    workspaces
        .publish(
            &workspace,
            &host.cert,
            tilo_server_protocol::directory::Listing {
                description: "Coffee and tea".into(),
                default_role: tilo_server_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    let (addr, serving) = serve_web(Arc::new(web_of(&server, &host))).await;

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
    let page: tilo_server_protocol::directory::DirectoryPage =
        serde_json::from_slice(&body).unwrap();
    assert_eq!(page.workspaces.len(), 1);
    assert_eq!(page.workspaces[0].description, "Coffee and tea");

    // Asking the entry to let someone in over HTTP finds no such route.
    let body = serde_json::json!({
        "userPublicKey": hex::encode([31; 32]),
        "displayName": "Cy",
    })
    .to_string();
    let head = format!(
        "POST {API_PREFIX}/directory/{workspace}/join HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://someone.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let (status, head, _) = exchange(addr, &head, body.as_bytes()).await;
    assert_eq!(status, 404, "{head}");
    assert!(
        !workspaces
            .membership(&workspace)
            .await
            .unwrap()
            .1
            .contains_key(&hex::encode([31; 32])),
        "nobody became a member over HTTP"
    );

    // The preflight a page on another site sends before reporting.
    let head = format!(
        "OPTIONS {API_PREFIX}/directory/{workspace}/report HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://someone.example\r\nAccess-Control-Request-Method: POST\r\nAccess-Control-Request-Headers: content-type\r\n\r\n"
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

    // A page's own uncaught error goes to the entry that let it in: the host
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
        "any site's page reports to its own entry"
    );
    let head = format!(
        "POST {API_PREFIX}/directory/{workspace}/errors HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: 8\r\n\r\n"
    );
    let (status, head, _) = exchange(addr, &head, b"not json").await;
    assert_eq!(status, 204, "{head}");

    stop(server, serving).await;
}

/// The operator's kill switch on the host's public listing: a watch-only host still lets any
/// site's page read who is here, a host paused entirely answers that nobody reads it and when
/// to come back, and the mode file the operator writes takes effect without a restart.
#[tokio::test]
async fn a_back_soon_host_serves_its_public_listing_to_nobody() {
    let root = SigningKey::from_bytes(&[126; 32]);
    let host = host(&root, 127, tilo_user_identity::DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(&state, host.identity(), ServerConfig::default()).await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    workspaces
        .publish(
            &workspace,
            &host.cert,
            tilo_server_protocol::directory::Listing {
                description: "Coffee and tea".into(),
                default_role: tilo_server_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    workspaces.hold_mode(tilo_workspace_service::workspaces::mode::HostMode::held(
        state.path().to_owned(),
        tilo_workspace_service::workspaces::mode::Mode::Normal,
    ));
    let (addr, serving) = serve_web(Arc::new(web_of(&server, &host))).await;

    let ask = format!(
        "POST {API_PREFIX}/directory HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://someone.example\r\nContent-Length: 0\r\n\r\n"
    );
    for (written, served) in [
        ("normal", 200_u16),
        ("spectator-only", 200),
        ("back-soon", 503),
        ("normal", 200),
    ] {
        std::fs::write(state.path().join("mode"), written).unwrap();
        let (status, head, _) = exchange(addr, &ask, b"").await;
        assert_eq!(status, served, "{written}: {head}");
        if served == 503 {
            assert_eq!(
                header_value(&head, "retry-after"),
                Some("60"),
                "a page is told when to come back: {head}"
            );
        }
    }

    stop(server, serving).await;
}

/// A page on any site that stops on its failure screen reaches its host's log
/// with the cause it showed, through the entry's `errors` route, as a browser
/// whose beacons send nothing sends it: a plain `no-cors` POST from another
/// origin, its body as large as a page sends.
#[tokio::test]
async fn a_page_that_could_not_open_its_workspace_reaches_the_host_log() {
    #[derive(Clone, Default)]
    struct Sink(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let sink = Sink::default();
    let writer = sink.clone();
    // The server serves on the test's own thread: its log goes to the sink.
    let _log = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish(),
    );
    let root = SigningKey::from_bytes(&[141; 32]);
    let host = host(&root, 142, tilo_user_identity::DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(&state, host.identity(), ServerConfig::default()).await;
    let workspace = server
        .workspaces()
        .unwrap()
        .create("Lounge")
        .await
        .unwrap()
        .workspace;
    let (addr, serving) = serve_web(Arc::new(web_of(&server, &host))).await;

    // What a page sends (`reportBody` in entry-page.mjs): the cause first,
    // a stack cut so the whole fits the 4 KiB the entry reads.
    let mut report = serde_json::json!({
        "kind": "failed", "message": "loading: Error: Shared resource connection closed",
        "source": "https://lounge.example", "line": 0, "column": 0, "stack": "",
        "page": "/", "build": "7",
    });
    let spare = tilo_server_protocol::directory::MAX_PAGE_ERROR_BYTES - report.to_string().len();
    report["stack"] = serde_json::Value::String(
        "@https://lounge.example/v/7/peer.mjs:1:1 ".repeat(200)[..spare].to_owned(),
    );
    let report = report.to_string();
    assert_eq!(
        report.len(),
        tilo_server_protocol::directory::MAX_PAGE_ERROR_BYTES
    );
    let head = format!(
        "POST {API_PREFIX}/directory/{workspace}/errors HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://lounge.example\r\nSec-Fetch-Mode: no-cors\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n",
        report.len()
    );
    let (status, head, _) = exchange(addr, &head, report.as_bytes()).await;
    assert_eq!(status, 204, "{head}");

    stop(server, serving).await;
    let logged = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    let line = logged
        .lines()
        .find(|line| line.contains("a page could not open its workspace"))
        .unwrap_or_else(|| panic!("no failure line in the host's log: {logged}"));
    for said in [
        "cause=\"loading: Error: Shared resource connection closed\"",
        "host=\"https://lounge.example\"",
        &format!("workspace=\"{workspace}\""),
    ] {
        assert!(line.contains(said), "no {said} in {line}");
    }
}

/// A short link names an invitation its workspace keeps: whoever follows it
/// asks this host for the invitation by the link's secret, before they are
/// anyone the host knows, from any site's page. The host answers it while it
/// stands, and once it is taken back answers what it answers for a secret it
/// never issued.
#[tokio::test]
async fn an_invitation_its_workspace_keeps_is_opened_by_its_links_secret() {
    use tilo_server_protocol::web::{KeptInvitation, OPEN_INVITATION_PATH, OpenedInvitation};

    let root = SigningKey::from_bytes(&[131; 32]);
    let host = host(&root, 132, tilo_user_identity::DeviceCapabilities::member());
    let (owner_key, owner) = device(&root, 133, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(&state, host.identity(), ServerConfig::default()).await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    workspaces
        .enter(&workspace, (&owner, &owner_key))
        .await
        .unwrap();
    let invitation = workspaces
        .issue(
            &workspace,
            &owner,
            tilo_workspace_service::workspaces::IssueInvitationRequest {
                request_id: [1; 32],
                role: Role::Editor,
                recipient_user_public_key: None,
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns() + 600_000_000_000,
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let secret = tilo_peer_state::join::native::bearer_text(&invitation.signed_invitation);
    let (addr, serving) = serve_web(Arc::new(web_of(&server, &host))).await;

    let ask = |workspace: &str, secret: &str| {
        let body = serde_json::to_vec(&KeptInvitation {
            workspace: workspace.to_owned(),
            secret: secret.to_owned(),
        })
        .unwrap();
        let head = format!(
            "POST /peer/v1{OPEN_INVITATION_PATH} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        async move { exchange(addr, &head, &body).await }
    };

    let (status, head, body) = ask(&workspace, &secret).await;
    assert_eq!(status, 200, "{head}");
    let opened: OpenedInvitation = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        tilo_peer_state::join::invitation_bytes(&opened.invitation).unwrap(),
        invitation.signed_invitation,
        "the host answers the invitation the workspace issued, byte for byte"
    );

    // A secret it never issued, and the right secret in another workspace,
    // name nothing.
    let other = workspaces.create("Elsewhere").await.unwrap().workspace;
    for (workspace, secret) in [
        (workspace.as_str(), "a".repeat(43)),
        (other.as_str(), secret.clone()),
        (workspace.as_str(), "short".to_owned()),
    ] {
        let (status, head, _) = ask(workspace, &secret).await;
        assert_eq!(status, 410, "{head}");
    }

    // Taken back, the invitation is gone for whoever holds the link.
    workspaces
        .cancel(
            &workspace,
            &owner,
            tilo_workspace_service::workspaces::CancelInvitationRequest {
                request_id: [2; 32],
                invite_id: invitation.invite_id,
            },
        )
        .await
        .unwrap();
    let (status, head, body) = ask(&workspace, &secret).await;
    assert_eq!(status, 410, "{head}");
    assert!(
        String::from_utf8_lossy(&body).contains("gone"),
        "the refusal says the invitation is gone: {}",
        String::from_utf8_lossy(&body)
    );

    stop(server, serving).await;
}

/// A site named with the port its scheme implies is the same site a browser
/// names without it, so a request that came through the site is still known
/// to have: sending it on would be a redirect to itself.
#[test]
fn a_site_named_with_its_default_port_is_the_same_site() {
    assert_eq!(
        Site::new("https://tilo.sh:443/").unwrap().authority,
        "tilo.sh"
    );
    assert_eq!(
        Site::new("http://tilo.localhost:24820/").unwrap().authority,
        "tilo.localhost:24820",
        "a port that is not the default is part of the site"
    );
    assert!(Site::new("not an address").is_none());
}

/// A browser that opens an address this host does not serve asks for a page,
/// not for a text body: it is sent on to the site the host belongs to. A
/// caller that asks for data keeps its answer.
#[tokio::test]
async fn a_browser_that_opens_an_unknown_address_is_sent_to_the_site() {
    let root = SigningKey::from_bytes(&[151; 32]);
    let host = host(&root, 152, tilo_user_identity::DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(&state, host.identity(), ServerConfig::default()).await;
    let site = "https://app.example";
    let (addr, serving) = serve_web(Arc::new(web_of_at(
        &server,
        &host,
        Some(&format!("{site}/")),
    )))
    .await;

    let browser = "GET /somewhere HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAccept: text/html\r\n\r\n";
    let (status, head, _) = exchange(addr, browser, b"").await;
    assert_eq!(status, 302, "{head}");
    assert_eq!(
        header_value(&head, "location"),
        Some(site),
        "the browser lands on the site, which has a page for it: {head}"
    );

    // A request for data is answered as a host answers one it cannot take.
    let data = "GET /somewhere HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nAccept: application/json\r\n\r\n";
    let (status, head, _) = exchange(addr, data, b"").await;
    assert_eq!(status, 405, "{head}");

    // A host that knows no site answers as it always has.
    let (unhosted, second) = serve_web(Arc::new(web_of(&server, &host))).await;
    let (status, head, _) = exchange(unhosted, browser, b"").await;
    assert_eq!(status, 405, "{head}");
    second.abort();

    stop(server, serving).await;
}

/// The readiness probe a deployment's ingress and the local stack ask, `GET /mode`, says the
/// server is up, also on a host that sends a browser's other addresses to its site.
#[tokio::test]
async fn the_readiness_probe_answers_a_get() {
    let root = SigningKey::from_bytes(&[153; 32]);
    let host = host(&root, 154, tilo_user_identity::DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(&state, host.identity(), ServerConfig::default()).await;
    let (addr, serving) = serve_web(Arc::new(web_of_at(
        &server,
        &host,
        Some("https://app.example/"),
    )))
    .await;

    let probe = "GET /mode HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let (status, head, body) = exchange(addr, probe, b"").await;
    assert_eq!(status, 200, "{head}");
    let mode: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(mode["server"], "up", "{mode}");

    stop(server, serving).await;
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
    reason = "the server and its workspaces serve the page until the server shuts down at the end; only some feature sets see the drop"
)]
async fn the_fetch_proxy_answers_admitted_people_and_refuses_everyone_else() {
    let signer = SigningKey::from_bytes(&[81; 32]);
    let watcher = SigningKey::from_bytes(&[82; 32]);
    let host = host(
        &signer,
        83,
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let (owner_key, owner) = device(&signer, 84, DeviceCapabilities::member());
    let (joined_key, joined) = device(&watcher, 85, DeviceCapabilities::member());
    let (_, watch) = device(&watcher, 86, DeviceCapabilities::member());
    let (_, laptop) = device(&signer, 87, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(&state, host.identity(), ServerConfig::default()).await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Lounge").await.unwrap();
    workspaces
        .enter(&workspace.workspace, (&owner, &owner_key))
        .await
        .unwrap();
    // Another person is a viewer of the workspace, as an invitation makes
    // them, and one of their devices joins; their other device does not.
    let invitation = workspaces
        .issue(
            &workspace.workspace,
            &owner,
            tilo_workspace_service::workspaces::IssueInvitationRequest {
                request_id: [1; 32],
                role: Role::Viewer,
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
    tilo_peer_state::join_transport::WorkspaceJoinClient::new(endpoint(&joined_key).await)
        .redeem_hint(
            &verified.grant().transport[0],
            invitation.signed_invitation,
            joined.clone(),
            &joined_key,
            [2; 32],
        )
        .await
        .unwrap();

    let mut web = web_of(&server, &host);
    web.allowed_origins = vec!["https://app.example".into()];
    // The target is on loopback, which a host's proxy never reaches; the
    // refusal of its own network is `tilo_net`'s to test. The workspace may
    // receive what the two answers below carry, together.
    web.fetch = Some(
        tilo_net::proxy::host::FetchProxy::new(tilo_net::proxy::host::Config {
            destinations: tilo_net::proxy::host::Destinations::Any,
            budgets: tilo_net::proxy::host::Budgets {
                visitor_bytes: None,
                workspace_bytes: Some(52),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap(),
    );
    // The tokens a page asks its host for: one per grant, and the person it
    // names is what the proxy admits.
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
            format!("{}: {token}\r\n", tilo_server_protocol::web::SESSION_HEADER)
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

    stop(server, serving).await;
}

/// The secret `mail` carries for `parameter`, as the page it lands on reads
/// it: the hex after `?<parameter>=` (or `&<parameter>=`) in the mail's text.
fn link_secret(mail: &crate::device::keep::entry::mail::Mail, parameter: &str) -> String {
    let at = mail
        .text
        .find(&format!("{parameter}="))
        .unwrap_or_else(|| panic!("no {parameter} in the mail: {}", mail.text));
    let rest = &mail.text[at.saturating_add(parameter.len()).saturating_add(1)..];
    let end = rest
        .find(|character: char| !character.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    rest[..end].to_owned()
}

/// The mails `mailer` took, once it has `count` of them: the entry's outbox
/// sends in its own task, so a check waits for it rather than reading once.
async fn mails_taken(
    mailer: &crate::device::keep::entry::mail::FakeMailer,
    count: usize,
) -> Vec<crate::device::keep::entry::mail::Mail> {
    for _ in 0..200 {
        let taken = mailer.sent.lock().unwrap().clone();
        if taken.len() >= count {
            return taken;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    mailer.sent.lock().unwrap().clone()
}

/// A server serving one workspace, listed and taking people by invitation
/// only. [`waiting_entry`] settles the wait list its entry keeps.
async fn invitation_only_host(
    root: &SigningKey,
    seed: u8,
) -> (KeptServer, Host, String, tempfile::TempDir) {
    let host = host(root, seed, tilo_user_identity::DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(&state, host.identity(), ServerConfig::default()).await;
    let workspaces = server.workspaces().unwrap().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    workspaces
        .publish(
            &workspace,
            &host.cert,
            tilo_server_protocol::directory::Listing {
                description: "Coffee and tea".into(),
                default_role: tilo_server_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    workspaces
        .list_invitation_only(&workspace, &host.cert, true)
        .await
        .unwrap();
    (server, host, workspace, state)
}

/// `workspace`'s entry as its founder settles it: listed, taking people by
/// invitation only, keeping `max` addresses, naming a contact and mailing them.
async fn waiting_entry(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    founder: &DeviceCert,
    workspace: &str,
    max: usize,
) {
    use tilo_action_policy::{EntryMail, EntryPolicy, ListingPolicy};

    workspaces
        .found_entry(
            workspace,
            founder,
            EntryPolicy {
                listing: Some(ListingPolicy {
                    description: "Coffee and tea".into(),
                    default_role: tilo_action_policy::Role::Editor,
                }),
                invitation_only: true,
                wait_list_max: max,
                contact: Some("hello@tilo.example".into()),
                mail: Some(EntryMail {
                    from: "wait-list@tilo.example".into(),
                    templates: None,
                }),
            },
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the server and its workspaces serve the entry until the server shuts down at the end"
)]
async fn an_invitation_only_entry_keeps_an_address_confirms_it_and_takes_it_off() {
    use tilo_server_protocol::directory::{EntryRefusalCode, EntryRefused};

    let root = SigningKey::from_bytes(&[150; 32]);
    let (server, host, workspace, _state) = invitation_only_host(&root, 151).await;
    let mailer = Arc::new(crate::device::keep::entry::mail::FakeMailer::default());
    let workspaces = server.workspaces().unwrap();
    // The entry the founder settles names the wait list the flow serves.
    waiting_entry(workspaces, &host.cert, &workspace, 4).await;
    let mut web = web_of_at(&server, &host, Some("https://site.example/"));
    web.served_lists = ServedLists::with_mailer(mailer.clone());
    let (addr, serving) = serve_web(Arc::new(web)).await;

    // Someone the entry turns away is told it keeps a wait list, where the
    // contact is, and that it mails: the refusal their entry hears on its
    // session.
    let refusal = workspaces
        .admit_listed(
            &workspace,
            &tilo_server_protocol::directory::EntryJoin {
                user_public_key: hex::encode([41u8; 32]),
                display_name: "Ada".into(),
            },
        )
        .await
        .expect_err("an invitation-only entry turns a stranger away");
    let refused: EntryRefused =
        refusal.answer_with(workspaces.waiting_offer(&workspace).await.as_ref());
    assert_eq!(refused.code, EntryRefusalCode::InvitationOnly);
    assert!(refused.wait_list, "{refused:?}");
    assert_eq!(refused.contact.as_deref(), Some("hello@tilo.example"));
    assert!(refused.mail, "{refused:?}");

    let ask = |step: &str, body: serde_json::Value| {
        let body = body.to_string();
        let head = format!(
            "POST {API_PREFIX}/directory/{workspace}/{step} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        async move { exchange(addr, &head, body.as_bytes()).await }
    };

    // The address is kept, and the confirmation link mailed to it.
    let (status, head, body) = ask(
        "wait-list",
        serde_json::json!({"email": "Ada@Example.org", "language": "de-DE"}),
    )
    .await;
    assert_eq!(status, 200, "{head} {}", String::from_utf8_lossy(&body));
    let kept: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(kept["email"], "Ada@Example.org", "kept as typed");
    assert_eq!(kept["mail"], true);
    let removal = kept["removal"].as_str().unwrap().to_owned();
    assert_eq!(removal.len(), 64);
    let sent = mails_taken(&mailer, 1).await;
    assert_eq!(sent.len(), 1, "one confirmation: {sent:?}");
    let (to, confirm, remove_link) = (
        sent[0].to.clone(),
        link_secret(&sent[0], "wait-list-confirm"),
        link_secret(&sent[0], "wait-list-remove"),
    );
    assert_eq!(to, "Ada@Example.org");
    assert_eq!(
        remove_link, removal,
        "the mail's removal link carries the secret the entry handed out"
    );
    // The mail is in the language the entry was read in.
    assert_eq!(confirm.len(), 64);

    // Until the link is opened the address waits, and is listed to nobody.
    let (status, head, body) =
        ask("wait-list/status", serde_json::json!({"secret": removal})).await;
    assert_eq!(status, 200, "{head}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["status"],
        "pending"
    );

    let (status, head, body) =
        ask("wait-list/confirm", serde_json::json!({"token": confirm})).await;
    assert_eq!(status, 200, "{head}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["confirmed"],
        true
    );
    let (_, _, body) = ask("wait-list/status", serde_json::json!({"secret": removal})).await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["status"],
        "listed"
    );
    // Opened again, the same link confirms nothing new and is no error.
    let (status, _, _) = ask("wait-list/confirm", serde_json::json!({"token": confirm})).await;
    assert_eq!(status, 200);

    // Whoever may invite reads it on the workspace's wait list, as the member
    // panel does.
    let listed = server
        .workspaces()
        .unwrap()
        .page_workspaces(
            &host.cert,
            tilo_server_protocol::workspaces::WorkspacesRequest::WaitList {
                workspace: workspace.clone(),
            },
        )
        .await;
    let tilo_server_protocol::workspaces::WorkspacesReply::WaitList(entries) = listed else {
        panic!("the owner reads the wait list: {listed:?}");
    };
    assert_eq!(
        entries.len(),
        1,
        "the confirmed address is listed: {entries:?}"
    );
    assert_eq!(entries[0].contact, "Ada@Example.org");

    // Taking the address off answers alike, and mails word that it left.
    let (status, head, body) =
        ask("wait-list/remove", serde_json::json!({"secret": removal})).await;
    assert_eq!(status, 200, "{head} {}", String::from_utf8_lossy(&body));
    let left = mails_taken(&mailer, 2).await;
    assert_eq!(left.len(), 2, "a removal is mailed: {left:?}");
    assert_eq!(left[1].to, "Ada@Example.org");
    let (_, _, body) = ask("wait-list/status", serde_json::json!({"secret": removal})).await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["status"],
        "gone"
    );
    // A secret no entry has is answered as one that took an entry off.
    let (status, _, _) = ask(
        "wait-list/remove",
        serde_json::json!({"secret": hex::encode([9u8; 32])}),
    )
    .await;
    assert_eq!(status, 200);

    // A key this entry never handed out is refused, not guessed at.
    let (status, _, body) = ask(
        "wait-list/status",
        serde_json::json!({"secret": "not-a-secret"}),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["code"],
        "invalid-secret"
    );

    // Nothing that is not an address is no address.
    let (status, _, body) = ask("wait-list", serde_json::json!({"email": "nope"})).await;
    assert_eq!(status, 400);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["code"],
        "invalid-email"
    );

    stop(server, serving).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the server and its workspaces keep their state until the server shuts down at the end"
)]
async fn the_wait_list_is_bounded_by_the_workspace_and_by_the_visitors_network() {
    let root = SigningKey::from_bytes(&[152; 32]);
    let (server, host, workspace, _state) = invitation_only_host(&root, 153).await;
    let mailer = Arc::new(crate::device::keep::entry::mail::FakeMailer::default());
    waiting_entry(server.workspaces().unwrap(), &host.cert, &workspace, 1).await;
    let mut web = web_of_at(&server, &host, Some("https://site.example/"));
    web.served_lists = ServedLists::with_mailer(mailer);
    let (addr, serving) = serve_web(Arc::new(web)).await;

    let ask = |step: &str, body: serde_json::Value| {
        let body = body.to_string();
        let head = format!(
            "POST {API_PREFIX}/directory/{workspace}/{step} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        async move { exchange(addr, &head, body.as_bytes()).await }
    };

    // The workspace keeps one address: a second one is told the list is full.
    let (status, head, _) = ask("wait-list", serde_json::json!({"email": "ada@example.org"})).await;
    assert_eq!(status, 200, "{head}");
    let (status, head, body) =
        ask("wait-list", serde_json::json!({"email": "bo@example.org"})).await;
    assert_eq!(status, 503, "{head}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["code"],
        "wait-list-full"
    );

    // One address cannot fill the entry's windows either: six asks in the
    // window are one too many, and the answer says when to come back.
    let mut limited = None;
    for index in 0..6 {
        let (status, head, body) = ask(
            "wait-list",
            serde_json::json!({"email": format!("someone{index}@example.org")}),
        )
        .await;
        if status == 429 {
            limited = Some((head, body));
            break;
        }
    }
    let (head, body) = limited.expect("the sixth ask from one address is refused");
    assert!(head.contains("retry-after:"), "{head}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["code"],
        "too-many"
    );

    // A workspace this entry does not serve keeps no wait list.
    let other = server
        .workspaces()
        .unwrap()
        .create("Elsewhere")
        .await
        .unwrap()
        .workspace;
    let body = serde_json::json!({"email": "cy@example.org"}).to_string();
    let head = format!(
        "POST {API_PREFIX}/directory/{other}/wait-list HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let (status, head, _) = exchange(addr, &head, body.as_bytes()).await;
    assert_eq!(status, 404, "{head}");

    stop(server, serving).await;
}

/// `/host` follows the endpoint: a relay reached after the listener started is named (the host
/// whose first network report found none is dialled through it from then on), and an endpoint
/// that lost its relays keeps naming the one it had.
#[test]
fn the_host_address_follows_the_endpoint_relay() {
    let id = iroh::SecretKey::from_bytes(&[3; 32]).public();
    let relay: iroh_base::RelayUrl = "https://live.example/".parse().unwrap();
    let relayless = iroh_base::EndpointAddr::new(id);
    let relayed = iroh_base::EndpointAddr::from_parts(id, [iroh_base::TransportAddr::Relay(relay)]);
    let mut address = ServerAddress {
        endpoint_addr: relayless.clone(),
        relay_url: None,
        web: None,
    };

    follow_endpoint(&mut address, relayed.clone());
    assert_eq!(address.relay_url.as_deref(), Some("https://live.example/"));
    assert_eq!(address.endpoint_addr, relayed);

    follow_endpoint(&mut address, relayless);
    assert_eq!(
        address.relay_url.as_deref(),
        Some("https://live.example/"),
        "a lost relay stays named"
    );
    assert_eq!(address.endpoint_addr, relayed);
}

/// The other origins a server is told it is reached under are origins as its
/// public one is, and stand beside one: anything else stops the start.
#[test]
fn another_origin_a_server_is_reached_under_is_an_origin_beside_its_public_one() {
    let config = |public_origin: Option<&str>| WebConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        tls: TlsSource::Local {
            hosts: vec!["localhost".into()],
            store: None,
        },
        allowed_origins: Vec::new(),
        plain_listen: Some("127.0.0.1:0".parse().unwrap()),
        public_origin: public_origin.map(str::to_owned),
        fetch_proxy: None,
        site: None,
    };
    let also = |origin: &str| vec![origin.to_owned()];
    let published = config(Some("http://127.0.0.1:24500"));
    assert!(checked_origins(&published, &[]).is_ok());
    assert!(checked_origins(&published, &also("https://tilo.wifi.example:24500")).is_ok());
    assert!(
        checked_origins(&published, &also("http://tilo.wifi.example:24500")).is_err(),
        "plain http beyond this machine"
    );
    assert!(
        checked_origins(&published, &also("tilo.wifi.example:24500")).is_err(),
        "a name alone is no origin"
    );
    assert!(
        checked_origins(&config(None), &also("https://tilo.wifi.example:24500")).is_err(),
        "no second name without a first"
    );
}

/// A person's picture is served to the outside at an unguessable public address, and the
/// address serves nothing from the moment they replace it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the server and its listener serve the picture until the server shuts down at the end"
)]
async fn a_persons_picture_is_served_at_a_public_address_until_they_replace_it() {
    use tilo_user_identity::root_statement;
    const PHOTO: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAMAAAAoyzS7AAAAA1BMVEX///+nxBvIAAAAAXRSTlMAQObYZgAAAAxJREFUeNoBgAEAAP//AwABWgH+AAAAAElFTkSuQmCC";

    let signer = SigningKey::from_bytes(&[61; 32]);
    let host = host(&signer, 62, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(&state, host.identity(), ServerConfig::default()).await;
    let web = Arc::new(web_of(&server, &host));
    let root = signer.verifying_key().to_bytes();
    let hex_root = hex::encode(root);
    let photo = collab_types::photo_bytes(PHOTO).expect("the picture's bytes");
    let at = 1_000_u64;
    let profile = serde_json::json!({
        "version": 1,
        "user_public_key": hex_root,
        "display_name": "Ada",
        "photo": PHOTO,
    })
    .to_string();
    let body = serde_json::to_vec(&serde_json::json!({
        "profile": profile,
        "issued_at_ms": at,
        "signature": hex::encode(root_statement::sign(&signer, root_statement::Kind::Profile, at, &profile).unwrap()),
    }))
    .unwrap();
    web.profiles
        .publish(&body, at, |_| async { true })
        .await
        .unwrap();

    let token = tilo_server_protocol::public_avatars::token(&root, &photo);
    let head =
        format!("GET /avatars/{token} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    let (addr, serving) = serve_web(Arc::clone(&web)).await;
    let (status, headers, body) = exchange(addr, &head, b"").await;
    assert_eq!(status, 200, "{headers}");
    assert_eq!(header_value(&headers, "content-type"), Some("image/png"));
    assert_eq!(body, photo);

    // They replace their picture with none: the old address names a picture no profile keeps
    // any more, and serves nothing.
    let later = at + 1;
    let replaced = serde_json::json!({
        "version": 1,
        "user_public_key": hex_root,
        "display_name": "Ada",
    })
    .to_string();
    let body = serde_json::to_vec(&serde_json::json!({
        "profile": replaced,
        "issued_at_ms": later,
        "signature": hex::encode(root_statement::sign(&signer, root_statement::Kind::Profile, later, &replaced).unwrap()),
    }))
    .unwrap();
    web.profiles
        .publish(&body, later, |_| async { true })
        .await
        .unwrap();
    let (status, _, _) = exchange(addr, &head, b"").await;
    assert_eq!(status, 404, "a picture they replaced serves nothing");
    stop(server, serving).await;
}

/// A server whose configuration names a key for Apple's push service tells a person's closed
/// iPhone app that something waits for them: the key goes from the configuration the daemon
/// reads to the sender its web start opens, with nothing else naming it.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closed_iphone_app_is_told_through_the_key_the_configuration_names() {
    use crate::device::keep::apple_push::tests as apple;
    use tilo_person_services::PersonServices;

    let apple_service = apple::StandIn::start().await;
    let keys = tempfile::tempdir().unwrap();
    let (key, signs_with) = apple::made_up_key(keys.path());
    let signer = SigningKey::from_bytes(&[81; 32]);
    let host = host(&signer, 82, DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = server_at(
        &state,
        host.identity(),
        ServerConfig {
            apple_push: Some(key),
            ..ServerConfig::default()
        },
    )
    .await;
    assert!(
        server
            .web_context()
            .apple_through
            .set(apple_service.service())
            .is_ok()
    );
    let web = web_of(&server, &host);
    web.serve_pushes(
        state.path(),
        None,
        None,
        (std::net::Ipv4Addr::LOCALHOST, 0).into(),
    );

    // Her phone handed the server its address; no device of hers is looking.
    let workspaces = server.workspaces().unwrap().clone();
    let phone = apple::Device::new(1);
    apple::reach(&workspaces, &apple::ADA, &phone);
    let reminded = PersonServices::of(&workspaces)
        .tell(
            &apple::ADA,
            apple::reminder("The landlord writes about the lease.", "lease-1"),
        )
        .unwrap();
    assert!(reminded.reaches);

    // The teller the server runs pushes it to her phone, through the key it was configured with.
    let heard = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let heard = apple_service.heard();
            if !heard.is_empty() {
                return heard;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the server pushed nothing to Apple's service for her closed app");
    assert_eq!(heard.len(), 1);
    let push = heard.first().unwrap();
    assert_eq!(push.token, phone.token);
    let (header, claims) = apple::signed_in(push.header("authorization").unwrap(), &signs_with);
    assert_eq!(header["kid"], "MADEUP0001");
    assert_eq!(claims["iss"], "MADEUPTEAM");

    crate::device::keep::server::Server::shutdown(server).await;
}
