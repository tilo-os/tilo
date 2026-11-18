//! Two machines of one person, enrollment over iroh through a loopback packet
//! relay and no local network between them: a machine handed a token dials the
//! device it names, and a machine showing a code is dialed by one.

use crate::JoinOptions;
use crate::join_transport::{self, qr};
use tilo_enrollment::approval::ApprovalOptions;
use tilo_enrollment::protocol::EnrollmentKind;
use tilo_enrollment::transport;
use tilo_enrollment::{Decision, Person, WaitingMachine, fleet_store, now_ns};
use tilo_identity::certification_ceremony::pairing_code;
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, Dial, IdentityPaths, sign_device_cert, verify_device_cert,
};

fn runtime() -> tokio::runtime::Runtime {
    transport::runtime().expect("a runtime for the enrollment")
}

/// The packet relay these tests reach both machines through: one on this
/// machine's loopback, answering on a port it chose, with no QUIC address
/// discovery, so an endpoint takes it as home from the HTTP probe alone.
struct Loopback {
    _server: iroh_relay::server::Server,
    url: iroh::RelayUrl,
}

fn loopback_relay(runtime: &tokio::runtime::Runtime) -> Loopback {
    runtime.block_on(async {
        let mut config = iroh_relay::server::ServerConfig::default();
        config.relay = Some(iroh_relay::server::RelayConfig::new((
            std::net::Ipv4Addr::LOCALHOST,
            0,
        )));
        let server = iroh_relay::server::Server::spawn(config)
            .await
            .expect("a loopback relay starts");
        let addr = server.http_addr().expect("the relay serves HTTP");
        Loopback {
            _server: server,
            url: format!("http://{addr}").parse().expect("a relay URL"),
        }
    })
}

impl Loopback {
    fn url(&self) -> &iroh::RelayUrl {
        &self.url
    }

    /// The relays an endpoint binds with to take this one as home: no QUIC
    /// address discovery, so the HTTP probe alone settles it.
    fn relays(&self) -> iroh::RelayMap {
        let mut config = iroh::RelayConfig::from(self.url.clone());
        config.quic = None;
        std::iter::once(config).collect()
    }
}

struct Approver {
    root: tempfile::TempDir,
    identities: tempfile::TempDir,
    root_key: ed25519_dalek::SigningKey,
    device: ed25519_dalek::SigningKey,
}

impl Approver {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let identities = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(root.path());
        paths.ensure_directories().unwrap();
        let root_key = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
        tilo_user_identity::persist::write_signing_key(&paths.user_privkey, &root_key).unwrap();
        tilo_user_identity::persist::write_public_key(
            &paths.user_pubkey,
            &root_key.verifying_key().to_bytes(),
        )
        .unwrap();
        tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device).unwrap();
        let mut cert = DeviceCert {
            user_public_key: root_key.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Trusted laptop".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root_key).unwrap();
        tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert).unwrap();
        tilo_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&root_key.verifying_key().to_bytes()),
            &cert,
        )
        .unwrap();
        fleet_store(root.path(), identities.path(), None)
            .remember_certificate(&hex::encode(root_key.verifying_key().to_bytes()), cert)
            .unwrap();
        Self {
            root,
            identities,
            root_key,
            device,
        }
    }

    /// A token this device mints for the machine, naming where this device is
    /// dialed.
    fn token(&self, dial: &[Dial]) -> String {
        let now = now_ns().unwrap();
        tilo_identity::enrollment::mint_token(
            self.root_key.verifying_key().to_bytes(),
            DeviceCapabilities::member(),
            now,
            now.saturating_add(600 * 1_000_000_000),
            dial,
            &self.device,
        )
        .unwrap()
    }
}

/// The machine that enrolls: its own key, and an identity directory a personal
/// enrollment writes.
struct Machine {
    root: tempfile::TempDir,
    identities: tempfile::TempDir,
    device: ed25519_dalek::SigningKey,
}

impl Machine {
    fn new(seed: u8) -> Self {
        let root = tempfile::tempdir().unwrap();
        let identities = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(root.path());
        paths.ensure_directories().unwrap();
        let device = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device).unwrap();
        Self {
            root,
            identities,
            device,
        }
    }

    fn options(&self, label: &'static str) -> JoinOptions<'_> {
        JoinOptions {
            identity_root: self.root.path(),
            label,
            timeout: 60,
            kind: EnrollmentKind::PersonalDevice,
            home_peer_output: None,
            identity_dir: Some(self.identities.path()),
            requester_lease: None,
            port: 0,
        }
    }

    fn enrolled(&self) -> DeviceCert {
        let paths = IdentityPaths::under(self.root.path());
        let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert).unwrap();
        verify_device_cert(&cert).expect("the certificate chains to the identity");
        cert
    }
}

#[test]
fn a_token_enrolls_a_machine_over_a_packet_relay() {
    let runtime = runtime();
    let relay = loopback_relay(&runtime);
    let approver = Approver::new();
    let machine = Machine::new(44);
    let expected_device = hex::encode(machine.device.verifying_key().to_bytes());
    let expected_service = hex::encode(tilo_server::daemon::server_node_id(&machine.device));
    // Nowhere on a local network: the token names the relay that reaches the
    // approving device, and the machine dials it there.
    let token = approver.token(&[Dial {
        endpoint: tilo_user_identity::enrollment_endpoint(
            &approver.device.verifying_key().to_bytes(),
        ),
        relay_url: relay.url().to_string(),
    }]);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let approver_thread = {
        let root = approver.root.path().to_path_buf();
        let identities = approver.identities.path().to_path_buf();
        let relays = relay.relays();
        let expected_device = expected_device.clone();
        std::thread::spawn(move || {
            let options = ApprovalOptions {
                identity_root: &root,
                identity_dir: Some(&identities),
                timeout: 60,
                workspace_cache: std::path::Path::new(""),
                acl: std::path::Path::new(""),
                server_workspace_ids: &[],
                expected_device: Some(&expected_device),
                expected_service: Some(&expected_service),
                kind: EnrollmentKind::PersonalDevice,
                port: 0,
                ready: Some(&ready_tx),
                approved: None,
                person_uid: None,
            };
            transport::serve(&options, Some(&relays), None, None, false)
        })
    };
    ready_rx
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the approving device's endpoint came up");
    let joined = join_transport::join(&token, machine.options("New laptop"));
    let joined = joined.unwrap_or_else(|error| panic!("the machine enrolls: {error:#}"));
    approver_thread.join().unwrap().expect("the approval");
    assert!(
        joined.is_some(),
        "a personal device gets the identity it joined"
    );
    let cert = machine.enrolled();
    assert_eq!(
        cert.device_public_key,
        machine.device.verifying_key().to_bytes()
    );
    assert_eq!(
        cert.user_public_key,
        approver.root_key.verifying_key().to_bytes()
    );
    // The approving device recorded it as one of the person's.
    let personal: Vec<String> = serde_json::from_slice(
        &std::fs::read(tilo_server_protocol::personal_device_peer_path(
            approver.root.path(),
        ))
        .expect("pairing recorded the personal devices"),
    )
    .unwrap();
    assert!(personal.contains(&expected_device), "{personal:?}");
}

/// A person answering the machines that dial their device: what a session's
/// sheet makes of the ask.
struct Answers(Decision);

impl Person for Answers {
    fn decide(&mut self, _machine: &WaitingMachine) -> anyhow::Result<Decision> {
        Ok(self.0)
    }
}

/// Serve one enrollment with `answering` deciding the machine that dials, and
/// wait until the endpoint is up.
fn serve_answered(
    approver: &Approver,
    relay: &Loopback,
    expected_device: String,
    expected_service: String,
    answering: Decision,
) -> std::thread::JoinHandle<anyhow::Result<()>> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let root = approver.root.path().to_path_buf();
    let identities = approver.identities.path().to_path_buf();
    let relays = relay.relays();
    let serving = std::thread::spawn(move || {
        let options = ApprovalOptions {
            identity_root: &root,
            identity_dir: Some(&identities),
            timeout: 5,
            workspace_cache: std::path::Path::new(""),
            acl: std::path::Path::new(""),
            server_workspace_ids: &[],
            expected_device: Some(&expected_device),
            expected_service: Some(&expected_service),
            kind: EnrollmentKind::PersonalDevice,
            port: 0,
            ready: Some(&ready_tx),
            approved: None,
            person_uid: None,
        };
        let mut answers = Answers(answering);
        transport::serve_with(&options, Some(&relays), None, None, Some(&mut answers))
    });
    ready_rx
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the approving device's endpoint came up");
    serving
}

#[test]
fn a_machine_is_admitted_only_when_the_person_answers_yes() {
    for answering in [Decision::Approve, Decision::Decline] {
        let runtime = runtime();
        let relay = loopback_relay(&runtime);
        let approver = Approver::new();
        let machine = Machine::new(45);
        let expected_device = hex::encode(machine.device.verifying_key().to_bytes());
        let expected_service = hex::encode(tilo_server::daemon::server_node_id(&machine.device));
        let token = approver.token(&[Dial {
            endpoint: tilo_user_identity::enrollment_endpoint(
                &approver.device.verifying_key().to_bytes(),
            ),
            relay_url: relay.url().to_string(),
        }]);
        let serving = serve_answered(
            &approver,
            &relay,
            expected_device.clone(),
            expected_service,
            answering,
        );
        let joined = join_transport::join(&token, machine.options("New laptop"));
        // The person answers machines for the whole window, which then ends.
        let served = serving.join().unwrap();
        match answering {
            Decision::Approve => {
                let cert = machine.enrolled();
                assert_eq!(
                    cert.device_public_key,
                    machine.device.verifying_key().to_bytes()
                );
                assert!(joined.expect("the machine enrolled").is_some());
            }
            Decision::Decline => {
                assert!(served.is_err(), "the window that admitted nobody says so");
                assert!(
                    joined.is_err(),
                    "a machine the person declined learns nothing of the identity"
                );
                assert!(
                    !IdentityPaths::under(machine.root.path())
                        .device_cert
                        .exists(),
                    "a declined machine keeps no certificate"
                );
            }
        }
    }
}

#[test]
fn a_code_shown_on_a_machine_enrolls_it_over_a_packet_relay() {
    let runtime = runtime();
    let relay = loopback_relay(&runtime);
    let approver = Approver::new();
    let machine = Machine::new(45);
    let expected_device = hex::encode(machine.device.verifying_key().to_bytes());
    let expected_service = hex::encode(tilo_server::daemon::server_node_id(&machine.device));
    let code = pairing_code(
        machine.device.verifying_key().to_bytes(),
        tilo_server::daemon::server_node_id(&machine.device),
    );
    let machine_addr = iroh_base::EndpointAddr::new(
        iroh_base::PublicKey::from_bytes(&machine.device.verifying_key().to_bytes()).unwrap(),
    )
    .with_relay_url(relay.url().clone());
    let (machine_ready_tx, machine_ready_rx) = std::sync::mpsc::channel();
    let machine_thread = {
        let root = machine.root.path().to_path_buf();
        let identities = machine.identities.path().to_path_buf();
        let relays = relay.relays();
        std::thread::spawn(move || {
            join_transport::wait_for_approval(
                JoinOptions {
                    identity_root: &root,
                    label: "New laptop",
                    timeout: 60,
                    kind: EnrollmentKind::PersonalDevice,
                    home_peer_output: None,
                    identity_dir: Some(&identities),
                    requester_lease: None,
                    port: 0,
                },
                Some(&relays),
                Some(&machine_ready_tx),
            )
        })
    };
    machine_ready_rx
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the machine's endpoint came up");
    let options = ApprovalOptions {
        identity_root: approver.root.path(),
        identity_dir: Some(approver.identities.path()),
        timeout: 60,
        workspace_cache: std::path::Path::new(""),
        acl: std::path::Path::new(""),
        server_workspace_ids: &[],
        expected_device: Some(&expected_device),
        expected_service: Some(&expected_service),
        kind: EnrollmentKind::PersonalDevice,
        port: 0,
        ready: None,
        approved: None,
        person_uid: None,
    };
    // The approving device dials the machine the code belongs to, through the
    // relay only.
    let relays = relay.relays();
    let served = transport::serve(
        &options,
        Some(&relays),
        Some(&machine_addr),
        Some(&code),
        false,
    );
    // Whichever side finished first, the machine's result is what is checked.
    let answered = served;
    let joined = machine_thread.join().unwrap();
    answered.expect("the approving device answered");
    joined.expect("the machine enrolled");
    let cert = machine.enrolled();
    assert_eq!(
        cert.device_public_key,
        machine.device.verifying_key().to_bytes()
    );
    assert_eq!(
        cert.user_public_key,
        approver.root_key.verifying_key().to_bytes()
    );
}

#[test]
fn the_machines_line_is_drawn_as_a_code_a_camera_reads() {
    let line = "tilo://enroll?endpoint=00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff&relay=https%3A%2F%2Frelay.example.&code=123456";
    let drawn = qr(line).expect("the line fits a code");
    // Every line of the drawing is as wide as the code it draws, and the
    // drawing is taller than wide because two module rows share a line.
    let width = drawn.lines().next().expect("a first line").chars().count();
    assert!(drawn.lines().all(|row| row.chars().count() == width));
    assert!(drawn.lines().count() > 10, "{drawn}");
    assert!(drawn.contains('\u{2584}') && drawn.contains(' '), "{drawn}");
    // A payload no code carries is left to the line itself.
    assert!(qr(&"x".repeat(10_000)).is_none());
}
