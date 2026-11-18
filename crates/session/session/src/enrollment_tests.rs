//! Two of the person's devices and a machine asking to join, in one process:
//! a machine's request shows on both devices, answering on the one that is not
//! serving the connection admits it, and no sheet has to be open.
//!
//! The devices' stores are linked by hand ([`crate::personal::linked`]), the
//! way the session's link carries them; the serving device's session answers
//! the machine over a packet relay on this machine's loopback.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed step in a test is the assertion"
)]

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use tilo_enrollment::approval::ApprovalOptions;
use tilo_enrollment::protocol::{
    EnrollmentKind, Request, decrypt_response, encrypt_request, make_ack, verify_finalized,
};
use tilo_enrollment::transport::{
    ALPN, address_of, endpoint_for, receive, runtime, send, serve_for_lifetime,
};
use tilo_enrollment::{now_ns, verify_response};
use tilo_peer_state::{PeerPersonal, PersonalStore, PersonalSync};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, Dial, IdentityPaths, decode_enrollment_token, sign_device_cert,
    verify_device_cert,
};

use super::{Asked, answer, held};

/// One of the person's devices that holds their root: its identity as the
/// session's device has it.
struct Keeper {
    root: tempfile::TempDir,
    identities: tempfile::TempDir,
    root_key: SigningKey,
    device: SigningKey,
    cert: DeviceCert,
}

fn certificate(root: &SigningKey, device: &SigningKey, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

impl Keeper {
    fn new(seed: u8) -> Self {
        let root = tempfile::tempdir().unwrap();
        let identities = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(root.path());
        paths.ensure_directories().unwrap();
        let root_key = SigningKey::from_bytes(&[seed; 32]);
        let device = SigningKey::from_bytes(&[seed.wrapping_add(100); 32]);
        tilo_user_identity::persist::write_signing_key(&paths.user_privkey, &root_key).unwrap();
        tilo_user_identity::persist::write_public_key(
            &paths.user_pubkey,
            &root_key.verifying_key().to_bytes(),
        )
        .unwrap();
        tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device).unwrap();
        let cert = certificate(&root_key, &device, "Trusted laptop");
        tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert).unwrap();
        tilo_user_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&root_key.verifying_key().to_bytes()),
            &cert,
        )
        .unwrap();
        tilo_enrollment::fleet_store(root.path(), identities.path(), None)
            .remember_certificate(
                &hex::encode(root_key.verifying_key().to_bytes()),
                cert.clone(),
            )
            .unwrap();
        Self {
            root,
            identities,
            root_key,
            device,
            cert,
        }
    }
}

/// A machine's own key, as the image it boots hands it one.
struct Machine {
    device: SigningKey,
}

/// One of the person's devices' stores, holding the certificates of both so
/// each admits what the other writes.
fn store_of(cert: &DeviceCert, other: &DeviceCert, signer: &SigningKey) -> PersonalSync {
    let now = SystemTime::now();
    let mut store =
        PersonalStore::open(cert.user_public_key, cert.clone(), now).expect("a store for a device");
    store
        .admit_device(other, now)
        .expect("their other device is one of the person's");
    let signer: Arc<dyn collab_types::signer::DeviceSigner> = Arc::new(signer.clone());
    PersonalSync::over(PeerPersonal::new(store), signer)
}

/// A packet relay on this machine's loopback, so both endpoints reach each
/// other with no local network between them.
struct Loopback {
    _server: iroh_relay::server::Server,
    url: iroh::RelayUrl,
}

impl Loopback {
    fn start(runtime: &tokio::runtime::Runtime) -> Self {
        runtime.block_on(async {
            let mut config = iroh_relay::server::ServerConfig::default();
            config.relay = Some(iroh_relay::server::RelayConfig::new((
                Ipv4Addr::LOCALHOST,
                0,
            )));
            let server = iroh_relay::server::Server::spawn(config)
                .await
                .expect("a loopback relay starts");
            let addr = server.http_addr().expect("the relay serves HTTP");
            Self {
                _server: server,
                url: format!("http://{addr}").parse().expect("a relay URL"),
            }
        })
    }

    fn relays(&self) -> iroh::RelayMap {
        let mut config = iroh::RelayConfig::from(self.url.clone());
        config.quic = None;
        std::iter::once(config).collect()
    }
}

/// What `read` answers, once it does, within the test's hang bound.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test arithmetic: the bound is a small duration"
)]
fn wait_for<T>(mut read: impl FnMut() -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        if let Some(value) = read() {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "what the test waited for did not happen in time"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one test walks an enrollment from the machine's dial to the answer taken on another device"
)]
fn a_machine_answered_on_another_device_is_admitted_with_no_sheet_open() {
    let driven = runtime().expect("a runtime for the enrollment");
    let relay = Loopback::start(&driven);
    let serving = Keeper::new(31);
    // The other device is the same person's: its certificate is the serving
    // device's root's, and its store is the person's own.
    let answering_device = SigningKey::from_bytes(&[141; 32]);
    let answering_cert = certificate(&serving.root_key, &answering_device, "Other laptop");
    let serve_store = store_of(&serving.cert, &answering_cert, &serving.device);
    let answer_store = store_of(&answering_cert, &serving.cert, &answering_device);

    // The serving device's session answers its enrollment endpoint for as
    // long as the session is up, noting each machine in the person's store.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let relay_map = relay.relays();
    let root = serving.root.path().to_path_buf();
    let identities = serving.identities.path().to_path_buf();
    let serving_store = serve_store.clone();
    let served = std::thread::spawn(move || {
        let mut asked = Asked {
            personal: serving_store,
        };
        let options = ApprovalOptions {
            identity_root: &root,
            identity_dir: Some(&identities),
            timeout: 60,
            workspace_cache: std::path::Path::new(""),
            acl: std::path::Path::new(""),
            server_workspace_ids: &[],
            expected_device: None,
            expected_service: None,
            kind: EnrollmentKind::PersonalDevice,
            port: 0,
            ready: None,
            approved: None,
            person_uid: None,
        };
        let _ = serve_for_lifetime(
            std::slice::from_ref(&options),
            Some(&relay_map),
            &mut asked,
            Some(&ready_tx),
        );
    });
    ready_rx
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the serving device's endpoint came up");

    // A machine is handed the token the person made for it.
    let machine = Machine {
        device: SigningKey::from_bytes(&[55; 32]),
    };
    let machine_key = machine.device.verifying_key().to_bytes();
    let now = now_ns().unwrap();
    let token = tilo_identity::enrollment::mint_token(
        serving.root_key.verifying_key().to_bytes(),
        DeviceCapabilities::member(),
        now,
        now.saturating_add(600 * 1_000_000_000),
        &[Dial {
            endpoint: tilo_user_identity::enrollment_endpoint(
                &serving.device.verifying_key().to_bytes(),
            ),
            relay_url: relay.url.to_string(),
        }],
        &serving.device,
    )
    .unwrap();
    let decoded = decode_enrollment_token(&token).unwrap();
    let request = Request {
        token,
        device_public_key: machine_key,
        server_node_id: tilo_server_protocol::server_node_id(&machine.device.to_bytes()),
        label: "Cupboard".into(),
        hardware: "8 cores".into(),
        kind: EnrollmentKind::PersonalDevice,
    };
    let (envelope, response_secret) =
        encrypt_request(&request, serving.device.verifying_key().to_bytes()).unwrap();
    let address = address_of(&Dial {
        endpoint: tilo_user_identity::enrollment_endpoint(
            &serving.device.verifying_key().to_bytes(),
        ),
        relay_url: relay.url.to_string(),
    })
    .unwrap();
    let machine_relays = relay.relays();
    let joining = std::thread::spawn(move || {
        let machine_runtime = runtime().expect("a runtime for the machine");
        machine_runtime.block_on(async move {
            let endpoint = endpoint_for(&machine.device, Some(&machine_relays))
                .await
                .expect("the machine's endpoint");
            let connection = endpoint
                .connect(address, ALPN)
                .await
                .expect("the machine dials the serving device");
            send(&connection, &envelope).await.expect("the request");
            let response = receive(&connection).await.expect("the answer");
            let plaintext = decrypt_response(
                &response,
                decoded.nonce,
                machine.device.verifying_key().to_bytes(),
                &response_secret,
            )
            .expect("the answer is sealed to this machine");
            let (payload, approver) = verify_response(&plaintext, &decoded).expect("the answer");
            let ack = make_ack(&response, decoded.nonce, &machine.device).expect("an ack");
            send(&connection, &ack).await.expect("the ack");
            let finalized = receive(&connection).await.expect("the Finalized");
            verify_finalized(
                &finalized,
                &response,
                decoded.nonce,
                machine.device.verifying_key().to_bytes(),
                approver,
            )
            .expect("the Finalized");
            payload.cert
        })
    });

    // The ask is the person's, in their store: it shows on both their devices,
    // and the one that is not serving the connection answers it.
    let ask = wait_for(|| {
        held(&serve_store)
            .waiting
            .first()
            .map(|machine| machine.ask.clone())
    });
    crate::personal::linked::hand(&serve_store, &answer_store);
    let shown = held(&answer_store).waiting;
    assert_eq!(shown.len(), 1, "the machine shows on the other device too");
    assert_eq!(shown[0].label, "Cupboard");
    assert_eq!(shown[0].ask, ask);
    answer(&answer_store, &ask, true).expect("answered on the other device");
    crate::personal::linked::hand(&answer_store, &serve_store);
    assert!(
        held(&serve_store).waiting.is_empty(),
        "an answered machine waits no more on the serving device either"
    );

    let cert = joining.join().expect("the machine's exchange");
    assert_eq!(cert.device_public_key, machine_key);
    verify_device_cert(&cert).expect("the certificate the machine gained chains to the person");
    assert_eq!(
        cert.user_public_key,
        serving.root_key.verifying_key().to_bytes()
    );
    let _ = std::thread::spawn(move || drop(served));
}
