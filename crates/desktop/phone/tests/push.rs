//! A phone is told what its person's assistant tells them, while it runs and while it is closed.
//!
//! The phone signs in with a ticket another of the person's devices printed, and hands its host the
//! web push endpoint a UnifiedPush distributor gave it, with a key of its own. While it runs, its
//! daemon reads the person's log through the same action every device of theirs reads it by, and
//! each thing that waits becomes a notification on the phone; answering one elsewhere takes it
//! away. With the daemon stopped, the host's teller pushes to that endpoint through the sender it
//! pushes to browsers with, and the phone's own key opens what arrives: the notification's title
//! and body.
//!
//! The push service here is on this machine, and the sender is told to trust its certificate:
//! nothing else reaches an address of its own network, and the phone's address is an `https` one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

// The server admits the phone's records through an Automerge worker: this binary is that
// worker, as the server host's own is.
tilo_automerge_isolate::test_worker!();

mod server;
mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use server::Server;
use tilo_actions::remote::person_telling::{self, Kind, Tell};
use tilo_phone::telling::Watching;
use tilo_phone::unified_push::{Key, Push};
use tilo_workspace_service::agents::telling::{Pace, Teller};
use tilo_workspace_service::workspaces::ToTell;

/// The push service on this machine: TLS with a certificate of its own, which the sender is told
/// to trust, and every body it is handed kept.
struct PushService {
    port: u16,
    /// The DER of the certificate it answers with.
    certificate: Vec<u8>,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl PushService {
    fn start(runtime: &tokio::runtime::Runtime) -> Self {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let certificate = certified.cert.der().to_vec();
        let key =
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certified.cert.der().clone()], key.into())
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind(("127.0.0.1", 0)))
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let bodies: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let kept = Arc::clone(&bodies);
        runtime.spawn(async move {
            while let Ok((connection, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let kept = Arc::clone(&kept);
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(connection).await else {
                        return;
                    };
                    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 4096];
                    // The body is one record, encrypted: read the head, then its length.
                    let (head, mut body) = loop {
                        if let Some(end) =
                            request.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            break (request[..end].to_vec(), request[end + 4..].to_vec());
                        }
                        let Ok(read) = stream.read(&mut chunk).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&chunk[..read]);
                    };
                    let length: usize = String::from_utf8_lossy(&head)
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().ok())
                                .flatten()
                        })
                        .unwrap_or_default();
                    while body.len() < length {
                        let Ok(read) = stream.read(&mut chunk).await else {
                            return;
                        };
                        if read == 0 {
                            break;
                        }
                        body.extend_from_slice(&chunk[..read]);
                    }
                    kept.lock().unwrap().push(body);
                    let _ = stream
                        .write_all(b"HTTP/1.1 201 Created\r\ncontent-length: 0\r\n\r\n")
                        .await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        Self {
            port,
            certificate,
            bodies,
        }
    }

    fn endpoint(&self, device: &str) -> String {
        format!("https://localhost:{}/push/{device}", self.port)
    }

    fn bodies(&self) -> Vec<Vec<u8>> {
        self.bodies.lock().unwrap().clone()
    }

    /// The push the service was handed last.
    fn last(&self) -> Vec<u8> {
        self.bodies()
            .pop()
            .expect("the push service was handed a push")
    }
}

/// The things a test tells the person, in their own words.
fn to_tell(words: &str, title: &str) -> ToTell {
    ToTell {
        kind: Kind::Question,
        tell: Tell {
            words: words.to_owned(),
            title: title.to_owned(),
            ..Tell::default()
        },
        ..ToTell::default()
    }
}

#[test]
fn a_phone_is_told_what_waits_while_it_runs_and_through_its_push_endpoint_while_it_is_closed() {
    let mut home = support::Home::new();
    let server = Server::start();
    home.relay(&server.relay_url());
    let laptop = support::Issuing::start(
        server.runtime.handle(),
        &server.person,
        server.entry(),
        server.workspaces(),
    );
    let person = server.person.verifying_key().to_bytes();
    let push = PushService::start(&server.runtime);

    let mut phone =
        tilo_runtime::in_process::Device::start(&support::profile()).expect("the phone's daemon");
    phone
        .log_in(&laptop.ticket(), "Pixel")
        .expect("the phone redeems the ticket its person's laptop printed");
    support::until("the phone lists the person's workspace", || {
        let devices = phone.devices().map_err(|error| format!("{error:#}"))?;
        devices
            .workspaces
            .iter()
            .any(|workspace| workspace.key == server.workspace)
            .then_some(devices.clone())
            .ok_or_else(|| format!("{devices:?}"))
    });

    // The phone's own key, kept in its files, is what it hands its host and what opens its pushes.
    let data = tilo_paths::data_dir().expect("the phone's data directory");
    let key = Key::load_or_create(&data).expect("the phone's push key");
    let (p256dh, auth) = key.handed_over();
    let endpoint = push.endpoint("pixel");
    let addresses = phone
        .register_web_push_address(&server.workspace, &endpoint, &p256dh, &auth)
        .expect("the host keeps where the phone is reached");
    let kept = addresses
        .addresses
        .iter()
        .find(|address| address.this_device)
        .expect("the address is the asking device's");
    assert_eq!(
        kept.kind,
        tilo_workspace_service::workspaces::push_addresses::PushKind::WebPush
    );

    // What no address is: one naming another device, and one that is no `https` endpoint.
    let named = serde_json::json!({
        "kind": "web-push",
        "endpoint": endpoint,
        "key": { "p256dh": p256dh, "auth": auth },
        "device": "aa".repeat(32),
    });
    let refused = phone.person_action(
        &server.workspace,
        person_telling::PROVIDER,
        tilo_workspace_service::workspaces::push_addresses::RESOURCE,
        tilo_workspace_service::workspaces::push_addresses::REGISTER,
        named.as_object().cloned().unwrap(),
    );
    assert!(
        refused.is_err(),
        "an address naming another device was kept: {refused:?}"
    );
    let plain = phone.register_web_push_address(
        &server.workspace,
        &format!("http://localhost:{}/push/pixel", push.port),
        &p256dh,
        &auth,
    );
    assert!(plain.is_err(), "an address of no https was kept: {plain:?}");

    // While the phone runs, its daemon reads the person's log, and what waits becomes a
    // notification with its words and its buttons.
    let told = server
        .runtime
        .block_on(
            server
                .workspaces()
                .tell(&person, to_tell("The deploy is waiting", "Deploy")),
        )
        .expect("the person is told");
    let (services, recorder) = tilo_process_services::process::ProcessServices::recording();
    let mut watching = Watching::new(Some(key.auth_bytes().unwrap()));
    watching
        .catch_up(&phone, &server.workspace, services.notifications.as_ref())
        .expect("the phone reads what its person was told");
    let shown = recorder.notifications();
    assert_eq!(shown.len(), 1, "one thing waits: {shown:?}");
    assert_eq!(shown[0].title, "Deploy");
    assert_eq!(shown[0].body, "The deploy is waiting");
    let tag = shown[0].tag.clone();
    assert!(
        tag.starts_with("tilo-told-"),
        "a notification is named as a push names it: {tag}"
    );

    // Answered on another device of theirs, it is taken away from the phone.
    server
        .workspaces()
        .acknowledge_told(
            &person,
            &told.id,
            "Ada's laptop",
            person_telling::BY_ANSWERED,
        )
        .expect("the answer is written down");
    watching
        .catch_up(&phone, &server.workspace, services.notifications.as_ref())
        .expect("the phone reads the log again");
    assert!(
        recorder.notifications().is_empty(),
        "a thing answered elsewhere waits no more: {:?}",
        recorder.notifications()
    );

    // The app closes; what waits for the person is pushed to the endpoint it handed over.
    let id = "cause:deploy:7";
    let told = server
        .runtime
        .block_on(server.workspaces().tell(
            &person,
            ToTell {
                id: Some(id.to_owned()),
                ..to_tell("The build is red", "Build")
            },
        ))
        .expect("the person is told while no device of theirs looks");
    phone.stop().expect("the phone's daemon stops");
    let host = data.join("host");
    std::fs::create_dir_all(&host).expect("a state directory for the host's sender");
    let trusted = push.certificate.clone();
    let sender = server
        .runtime
        .block_on(async {
            tilo_server::web_push::WebPush::reaching_here(&host, "mailto:ops@example.com", &trusted)
        })
        .expect("the host's sender")
        .telling(server.workspaces());
    let teller = Teller::new(server.workspaces(), Arc::new(sender)).pacing(Pace {
        after: Duration::ZERO,
        watching: Duration::ZERO,
    });
    // What the teller answers is when it looks again; this test drives its looks itself.
    let _ = server
        .runtime
        .block_on(teller.step(tilo_clock::unix_now_ms()));
    let body = push.last();
    let plain = key
        .open(&body)
        .expect("the push opens with the phone's key");
    let told_push = Push::read(&plain).expect("the push says what it is");
    assert_eq!(told_push.title, "Build");
    assert_eq!(told_push.body, "The build is red");
    assert_eq!(
        told_push.tag,
        person_telling::known_as(&key.auth_bytes().unwrap(), &told.id),
        "the push names the thing as the phone does"
    );
    assert!(
        !plain
            .windows(told.id.len())
            .any(|window| window == told.id.as_bytes()),
        "the thing's id is in no push: {}",
        String::from_utf8_lossy(&plain)
    );

    // Answered, a push that shows nothing names it as gone.
    server
        .workspaces()
        .acknowledge_told(
            &person,
            &told.id,
            "Ada's laptop",
            person_telling::BY_ANSWERED,
        )
        .expect("the answer is written down");
    let _ = server
        .runtime
        .block_on(teller.step(tilo_clock::unix_now_ms()));
    let gone = key
        .open(&push.last())
        .expect("the push opens with the phone's key");
    let gone = Push::read(&gone).expect("the push says what it is");
    assert!(gone.gone, "a thing taken back is pushed as gone");
    assert_eq!(gone.tag, told_push.tag);
}
