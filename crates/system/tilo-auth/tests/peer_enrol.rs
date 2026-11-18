//! A machine with no session, or one that holds none of the person's root,
//! enrols its disk-unlock boot key by asking the person's devices over the peer
//! network: the one that holds the root answers after its person approved, and
//! a device that holds none says so and the next is asked.
//!
//! The devices here are the two sides of the peer network on a loopback relay:
//! what the answering device does with the request is the session's
//! (`crates/session/session/tests/network_unlock_enrolment.rs` proves that).

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ed25519_dalek::SigningKey;
use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{RelayMap, RelayMode};
use tilo_auth::sibling::enrol::{NotEnrolled, PeerRegistrar, Registrar};
use tilo_device_identity::{IdentityPaths, persist};
use tilo_identity::device_fleet::{DeviceFleet, DeviceFleetStore};
use tilo_unlock_protocol::{
    BootKeyCert, ENROL_ALPN, EnrolReply, EnrolRequest, EnrolResponse, Half, PROTOCOL_VERSION,
    SiblingContact, SlotId, decode_frame, encode_frame, frame_len,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

/// The person's root, which only their own devices hold.
fn person() -> SigningKey {
    SigningKey::from_bytes(&[0x21; 32])
}

fn device_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn certify(root: &SigningKey, device: &SigningKey, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        issuer: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("the root certifies the device");
    cert
}

/// What the enrolling machine keeps: this device's key and certificate, and
/// the fleet that names the person's devices.
struct Enrolling {
    pubkey: [u8; 32],
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
}

fn enrolling(root: &SigningKey, certs: &[DeviceCert]) -> Enrolling {
    let dir = tempfile::tempdir().expect("a directory for the identity");
    let config = dir.path().join("config");
    let system = dir.path().join("system");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", &config)
        .set("TILO_IDENTITY_ROOT", &system);
    let paths = IdentityPaths::under(&system);
    paths.ensure_directories().expect("the identity layout");
    let key = device_key(0x61);
    persist::write_device_key(&paths.device_privkey, &key).expect("this device's key");
    let cert = certs
        .iter()
        .find(|cert| cert.device_public_key == key.verifying_key().to_bytes())
        .expect("this device's certificate");
    persist::write_device_cert(&paths.device_cert, cert).expect("this device's certificate");
    let identity_dir = config.join("tilo/identity");
    let fleet = DeviceFleet {
        schema_version: 1,
        identity_id: hex::encode(root.verifying_key().to_bytes()),
        certificates: certs.to_vec(),
        revocations: Vec::new(),
    };
    let store = DeviceFleetStore::at(&identity_dir, paths);
    let file = store.fleet_file(&fleet.identity_id);
    std::fs::create_dir_all(file.parent().expect("the fleet's directory"))
        .expect("the fleet's directory");
    std::fs::write(&file, serde_json::to_vec(&fleet).expect("the fleet"))
        .expect("the fleet is written");
    Enrolling {
        pubkey: key.verifying_key().to_bytes(),
        _env: env,
        _root: dir,
    }
}

/// What an answering device does with a request.
#[derive(Clone, Copy, Debug)]
enum Answer {
    /// It holds no root of the person's.
    NotKeyHolder,
    /// It holds the root and its person allowed the enrolment.
    Enrol,
}

/// One of the person's devices, answering [`ENROL_ALPN`] as itself.
#[derive(Debug)]
struct Answering {
    answer: Answer,
    root: SigningKey,
    person: [u8; 32],
    /// Who it is, as the sheet would name it.
    label: &'static str,
    asked: Arc<AtomicUsize>,
}

impl Answering {
    fn reply(&self, request: &EnrolRequest, device: [u8; 32]) -> EnrolReply {
        match self.answer {
            Answer::NotKeyHolder => EnrolReply::NotKeyHolder,
            Answer::Enrol => {
                let mut cert = BootKeyCert {
                    version: PROTOCOL_VERSION,
                    user_public_key: self.person,
                    device_public_key: device,
                    boot_public_key: request.boot_public_key,
                    slot: request.slot,
                    attestation_key: request.attestation_key.clone(),
                    issued_at_ns: tilo_clock::unix_now_ns(),
                    signature: [0; 64],
                };
                cert.sign(&self.root).expect("the person's root signs");
                EnrolReply::Enrolled(Box::new(EnrolResponse {
                    cert,
                    siblings: vec![SiblingContact {
                        device_public_key: self.root.verifying_key().to_bytes(),
                        label: self.label.into(),
                        endpoint: String::new(),
                    }],
                }))
            }
        }
    }
}

impl ProtocolHandler for Answering {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let Ok((mut send, mut recv)) = connection.accept_bi().await else {
            return Ok(());
        };
        self.asked.fetch_add(1, Ordering::SeqCst);
        let Some(request) = read_frame::<EnrolRequest>(&mut recv).await else {
            return Ok(());
        };
        let device = *connection.remote_id().as_bytes();
        write_reply(&mut send, &self.reply(&request, device)).await;
        Ok(())
    }
}

async fn read_frame<T: serde::de::DeserializeOwned>(recv: &mut RecvStream) -> Option<T> {
    let mut prefix = [0_u8; 4];
    recv.read_exact(&mut prefix).await.ok()?;
    let mut body = vec![0_u8; frame_len(prefix).ok()?];
    recv.read_exact(&mut body).await.ok()?;
    decode_frame(&body).ok()
}

async fn write_reply(send: &mut SendStream, reply: &EnrolReply) {
    if let Ok(frame) = encode_frame(reply) {
        let _ = send.write_all(&frame).await;
    }
    let _ = send.finish();
    // The answer is read before the connection goes away.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), send.stopped()).await;
}

/// The person's devices on a loopback relay: the laptop, then the phone, and a
/// server that is not among the enrolling machine's candidates.
struct Devices {
    /// The relay both sides reach each other through.
    relays: RelayMap,
    laptop: Arc<AtomicUsize>,
    phone: Arc<AtomicUsize>,
    server: Arc<AtomicUsize>,
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The relay is served on this runtime, which must outlive the exchange, and
    /// the server itself, which closes when dropped.
    _relay: tokio::runtime::Runtime,
    _relay_server: tilo_test_env::relay::Relay,
}

impl Devices {
    fn new(root: &SigningKey, laptop: Answer, phone: Answer) -> Self {
        let relay = tokio::runtime::Runtime::new().expect("a runtime for the relay");
        let server = relay.block_on(tilo_test_env::relay::relay());
        let relays = server.relays();
        let person = root.verifying_key().to_bytes();
        let laptop_asked = Arc::new(AtomicUsize::new(0));
        let phone_asked = Arc::new(AtomicUsize::new(0));
        let server_asked = Arc::new(AtomicUsize::new(0));
        let (ready, bound) = std::sync::mpsc::channel();
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let root = root.clone();
        let bound_relays = relays.clone();
        let (laptop_for_thread, phone_for_thread, server_for_thread) = (
            Arc::clone(&laptop_asked),
            Arc::clone(&phone_asked),
            Arc::clone(&server_asked),
        );
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("a runtime for the person's devices");
            runtime.block_on(async move {
                let mut routers = Vec::new();
                for (seed, answer, label, asked) in [
                    (0x31_u8, laptop, "The laptop", laptop_for_thread),
                    (0x41, phone, "The phone", phone_for_thread),
                    (0x51, Answer::Enrol, "The server", server_for_thread),
                ] {
                    let endpoint = Endpoint::builder(presets::Minimal)
                        .secret_key(iroh::SecretKey::from_bytes(&device_key(seed).to_bytes()))
                        .relay_mode(RelayMode::Custom(bound_relays.clone()))
                        .alpns(vec![ENROL_ALPN.to_vec()])
                        .bind_addr((Ipv4Addr::LOCALHOST, 0))
                        .expect("a loopback address")
                        .bind()
                        .await
                        .expect("the device's endpoint binds");
                    tilo_test_env::within("the device's home relay", async {
                        while !endpoint
                            .addr()
                            .relay_urls()
                            .any(|named| Some(named) == bound_relays.urls::<Vec<_>>().first())
                        {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                    })
                    .await;
                    routers.push(
                        Router::builder(endpoint)
                            .accept(
                                ENROL_ALPN,
                                Answering {
                                    answer,
                                    root: root.clone(),
                                    person,
                                    label,
                                    asked,
                                },
                            )
                            .spawn(),
                    );
                }
                ready.send(()).expect("the test waits for the devices");
                let _ = stopped.recv();
                for router in &routers {
                    let _ = router.shutdown().await;
                }
            });
        });
        bound.recv().expect("the person's devices are up");
        Self {
            relays,
            laptop: laptop_asked,
            phone: phone_asked,
            server: server_asked,
            stop: Some(stop),
            thread: Some(thread),
            _relay: relay,
            _relay_server: server,
        }
    }
}

impl Drop for Devices {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The request a machine draws when it enrols `--sibling`.
fn request() -> (EnrolRequest, SigningKey) {
    let boot_key = device_key(0x71);
    let half = Half::generate().expect("a fresh half");
    let slot = SlotId {
        volume_uuid: [0x11; 16],
        keyslot: 1,
    };
    (
        EnrolRequest::new(&boot_key, slot, None, None, half).expect("an enrolment request"),
        boot_key,
    )
}

#[test]
fn a_device_without_the_root_enrols_by_asking_one_that_holds_it() {
    let root = person();
    let laptop = certify(&root, &device_key(0x31), "The laptop");
    let phone = certify(&root, &device_key(0x41), "The phone");
    let server = certify(&root, &device_key(0x51), "The server");
    let me = certify(&root, &device_key(0x61), "The desktop");
    let enrolled = enrolling(&root, &[laptop, phone, server, me]);
    let devices = Devices::new(&root, Answer::NotKeyHolder, Answer::Enrol);

    // This machine has no session at all: it reads its own key and the person's
    // devices from its identity and asks them.
    let registrar = PeerRegistrar::find(None, Some(devices.relays.clone()))
        .expect("this device can ask the person's devices");
    let (request, boot_key) = request();
    let response = registrar
        .register(&request)
        .expect("the device that holds the root enrols it");
    assert_eq!(
        response.cert.device_public_key,
        device_key(0x61).verifying_key().to_bytes()
    );
    assert_eq!(
        response.cert.boot_public_key,
        boot_key.verifying_key().to_bytes()
    );
    assert_eq!(response.cert.slot, request.slot);
    response.cert.verify().expect("the person's root signed");
    assert!(!response.siblings.is_empty());
    assert_eq!(
        devices.laptop.load(Ordering::SeqCst),
        1,
        "the laptop was asked"
    );
    assert_eq!(
        devices.phone.load(Ordering::SeqCst),
        1,
        "the laptop holds no root, so the phone was asked"
    );
    assert_eq!(
        devices.server.load(Ordering::SeqCst),
        0,
        "the enrolment answered before the server was needed"
    );
    assert_eq!(response.cert.device_public_key, enrolled.pubkey);
}

#[test]
fn a_device_that_holds_no_root_says_so_and_the_next_is_asked() {
    let root = person();
    let laptop = certify(&root, &device_key(0x31), "The laptop");
    let phone = certify(&root, &device_key(0x41), "The phone");
    let me = certify(&root, &device_key(0x61), "The desktop");
    let _enrolled = enrolling(&root, &[laptop, phone, me]);
    let devices = Devices::new(&root, Answer::NotKeyHolder, Answer::NotKeyHolder);
    let registrar = PeerRegistrar::new(
        device_key(0x61),
        vec![
            device_key(0x31).verifying_key().to_bytes(),
            device_key(0x41).verifying_key().to_bytes(),
        ],
        Some(devices.relays.clone()),
    );
    let (request, _boot_key) = request();
    assert_eq!(registrar.register(&request), Err(NotEnrolled::NotKeyHolder));
    assert_eq!(
        devices.laptop.load(Ordering::SeqCst),
        1,
        "the laptop was asked"
    );
    assert_eq!(
        devices.phone.load(Ordering::SeqCst),
        1,
        "the phone was asked"
    );
    assert_eq!(
        devices.server.load(Ordering::SeqCst),
        0,
        "no other device was named"
    );
}
