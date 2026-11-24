//! A newly installed device enrols its boot key with the person's identity:
//! `tilo-disk-encrypt enrol --sibling` reaches the running session on its socket, the person's root
//! certifies the boot key, and the half that boots the disk is sealed to the person's secret-sync
//! key and published to their other devices.
//! A device that holds no root of the person's asks a device that does, over the peer network: that
//! device's session asks its person on the sheet, then certifies the key the same way.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::io::{Read as _, Write as _};
use std::net::Ipv4Addr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::endpoint::{Endpoint, presets};
use tilo_compositor::OutputConfig;
use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_network_actor::DeviceIdentity;
use tilo_service::{Answer, AskReceiver, AskSender, Question};
use tilo_session::Options;
use tilo_session::platform::fabric::Continuity;
use tilo_session::sync::{Client, Entry, OwnPresence};
use tilo_unlock_protocol::{
    ENROL_ALPN, ENROL_SOCKET, EnrolReply, EnrolRequest, Half, SlotId, decode_frame, encode_frame,
    frame_len,
};
use tilo_user_identity::{DeviceCapabilities, verify_device_cert};

/// The kind the sheet lists a peer's enrolment as.
const ENROL_KIND: &str = "network.device-enrol";

/// The person's identity in a directory of its own, as the desktop's own start reads it.
fn person() -> (
    std::path::PathBuf,
    tilo_test_env::ScopedEnv,
    tempfile::TempDir,
) {
    let root = tempfile::tempdir().expect("a directory for the identity");
    let xdg = root.path().join("config");
    let system = root.path().join("system");
    let runtime = root.path().join("run");
    std::fs::create_dir_all(&runtime).expect("a runtime directory");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", &xdg)
        .set("TILO_IDENTITY_ROOT", &system)
        .set("XDG_RUNTIME_DIR", &runtime)
        .set(FILE_SECRETS_ENV, "1");
    let personal = xdg.join("tilo/identity");
    let paths = IdentityPaths::under(&system);
    tilo_identity::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    (runtime, env, root)
}

#[test]
fn a_boot_key_enrols_through_the_sessions_socket() {
    let (runtime, _env, _root) = person();
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", 1280, 720)],
        Options {
            collab: true,
            ..Options::default()
        },
    );
    let socket = runtime.join(ENROL_SOCKET);
    let boot_key = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
    let half = Half::generate().expect("a fresh half");
    let slot = SlotId {
        volume_uuid: [0x11; 16],
        keyslot: 2,
    };
    let request =
        EnrolRequest::new(&boot_key, slot, None, None, half).expect("an enrolment request");
    let frame = encode_frame(&request).expect("a frame");

    // The session serves its socket once it has started.
    let deadline = tilo_test_env::hang_deadline();
    let reply = loop {
        if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket) {
            stream
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .expect("a read timeout");
            stream.write_all(&frame).expect("the request is written");
            let mut prefix = [0_u8; 4];
            stream.read_exact(&mut prefix).expect("the reply's prefix");
            let mut body = vec![0_u8; frame_len(prefix).expect("a frame length")];
            stream.read_exact(&mut body).expect("the reply");
            break decode_frame::<EnrolReply>(&body).expect("an enrolment reply");
        }
        session
            .step(Duration::from_millis(20))
            .expect("the session steps");
        assert!(
            Instant::now() < deadline,
            "the session serves its enrolment socket"
        );
    };

    let EnrolReply::Enrolled(response) = reply else {
        panic!("the session enrols the boot key: {reply:?}");
    };
    assert_eq!(
        response.cert.device_public_key,
        boot_key.verifying_key().to_bytes()
    );
    assert_eq!(response.cert.capabilities, DeviceCapabilities::nothing());
    verify_device_cert(&response.cert).expect("the person's root signed the boot key");
    assert!(
        !response.cert.signature.iter().all(|byte| *byte == 0),
        "the certificate carries the root's signature"
    );
}

/// The person's identity here, the device that holds its root, and a device
/// that holds none of it.
struct TwoDevices {
    questions: AskReceiver,
    shell: Rc<Client>,
    /// The person's other device, which holds the half the enrolment is
    /// registered with.
    phone: Rc<Client>,
    /// That device's certified key.
    phone_key: [u8; 32],
    /// The device asking, which is one of the person's and holds no root.
    enrollee: ed25519_dalek::SigningKey,
    session: tilo_session::Session,
    runtime: tokio::runtime::Runtime,
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
}

impl TwoDevices {
    fn new() -> Self {
        let (_runtime, env, root) = person();
        let registry = tilo_identity::store::IdentityRegistryStore::from_env()
            .expect("the identity registry")
            .load_reconciled_cached()
            .expect("the registry reads");
        let profile = registry
            .certified_identity()
            .expect("this device's person is known");
        let _ = profile;
        let now = tilo_clock::unix_now_ns();
        let phone_key = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
        let phone_public = phone_key.verifying_key().to_bytes();
        let phone_cert = cert_under_root(phone_public, "The phone", DeviceCapabilities::member(), now);
        let enrollee = ed25519_dalek::SigningKey::from_bytes(&[0x53; 32]);
        let _enrollee_cert = cert_under_root(
            enrollee.verifying_key().to_bytes(),
            "The laptop",
            DeviceCapabilities::member(),
            now,
        );
        let (asks, questions): (AskSender, AskReceiver) = tilo_service::asks::channel();
        let shell = Rc::new(
            Client::start(
                Entry {
                    identity: tilo_session::sync::device_identity(),
                    own: OwnPresence::default(),
                    hosts: Vec::new(),
                    continuity: Continuity::default(),
                    wake: Arc::new(|| {}),
                    relays: test_server::loopback(),
                    lan: None,
                    records: tilo_session::platform::records::Changes::default(),
                },
                None,
                Some(asks.clone()),
                Vec::new(),
            )
            .expect("this device's endpoint binds"),
        );
        let phone = Rc::new(
            Client::start(
                Entry {
                    identity: Some(DeviceIdentity::kept(phone_cert, phone_key)),
                    own: OwnPresence {
                        label: "The phone".into(),
                        ..OwnPresence::default()
                    },
                    hosts: Vec::new(),
                    continuity: Continuity::default(),
                    wake: Arc::new(|| {}),
                    relays: test_server::loopback(),
                    lan: None,
                    records: tilo_session::platform::records::Changes::default(),
                },
                None,
                None,
                Vec::new(),
            )
            .expect("the phone's endpoint binds"),
        );
        phone.connect(shell.endpoint_addr());
        let session = crate::common::headless_on(
            &[OutputConfig::new("HEADLESS-1", 1280, 720)],
            Options {
                collab: false,
                sync: Some(Rc::clone(&shell)),
                asks: Some((asks, questions.clone())),
                ..Options::default()
            },
        );
        Self {
            questions,
            shell,
            phone,
            phone_key: phone_public,
            enrollee,
            session,
            runtime: tokio::runtime::Runtime::new().expect("a runtime"),
            _env: env,
            _root: root,
        }
    }

    /// Let the session step until `done`.
    fn carry(&mut self, mut done: impl FnMut(&Self) -> bool) {
        tilo_test_env::wait_until("the devices' sessions carried it", || {
            self.session
                .step(Duration::from_millis(1))
                .expect("the session steps");
            self.phone.follow();
            done(self)
        });
    }

    /// The device holding no root enrols its boot key over the peer network,
    /// with the person's answer `approve`.
    fn exchange(&mut self, request: &EnrolRequest, approve: bool) -> EnrolReply {
        let frame = encode_frame(request).expect("a frame");
        let shell_addr = self.shell.endpoint_addr();
        let enrollee = self.enrollee.clone();
        let (endpoint, connection) = self.runtime.block_on(async {
            let endpoint = Endpoint::builder(presets::Minimal)
                .secret_key(iroh::SecretKey::from_bytes(&enrollee.to_bytes()))
                .relay_mode(test_server::relay_mode())
                .bind_addr((Ipv4Addr::LOCALHOST, 0))
                .expect("a loopback address")
                .bind()
                .await
                .expect("the enrolling device's endpoint binds");
            let connection = tokio::time::timeout(
                Duration::from_secs(20),
                endpoint.connect(shell_addr, ENROL_ALPN),
            )
            .await
            .expect("the owner's device answers")
            .expect("the enrolling device reaches it");
            (endpoint, connection)
        });
        let (mut send, mut recv) = self
            .runtime
            .block_on(connection.open_bi())
            .expect("a stream");
        self.runtime
            .block_on(send.write_all(&frame))
            .expect("the request is written");
        let shown = self.questioned();
        let answered = if approve {
            self.questions.answer(&shown.handle, Answer::default())
        } else {
            self.questions.decline(&shown.handle)
        };
        assert!(answered, "the person answers once");
        let reply = self.runtime.block_on(async {
            let mut prefix = [0_u8; 4];
            tokio::time::timeout(Duration::from_secs(20), recv.read_exact(&mut prefix))
                .await
                .expect("the answer comes")
                .expect("the answer's prefix");
            let mut body = vec![0_u8; frame_len(prefix).expect("a frame length")];
            tokio::time::timeout(Duration::from_secs(20), recv.read_exact(&mut body))
                .await
                .expect("the answer comes")
                .expect("the answer");
            decode_frame::<EnrolReply>(&body).expect("an enrolment reply")
        });
        self.runtime.block_on(endpoint.close());
        reply
    }

    /// The one enrolment question the shell's sheet lists.
    fn questioned(&mut self) -> Question {
        let mut found = None;
        self.carry(|devices| {
            found = devices
                .questions
                .pending()
                .into_iter()
                .find(|question| question.kind == ENROL_KIND);
            found.is_some()
        });
        found.expect("the enrolment reached the person")
    }
}

/// A boot key and the half this device would keep, as the enrolling command
/// draws them.
fn request(slot: SlotId) -> (EnrolRequest, ed25519_dalek::SigningKey) {
    let boot_key = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
    let half = Half::generate().expect("a fresh half");
    (
        EnrolRequest::new(&boot_key, slot, None, None, half).expect("an enrolment request"),
        boot_key,
    )
}

#[test]
fn a_device_without_the_root_enrols_when_the_person_allows_it() {
    let mut devices = TwoDevices::new();
    let slot = SlotId {
        volume_uuid: [0x11; 16],
        keyslot: 3,
    };
    let (request, boot_key) = request(slot);
    let reply = devices.exchange(&request, true);
    let EnrolReply::Enrolled(response) = reply else {
        panic!("the owner's device enrols the boot key: {reply:?}");
    };
    // The certificate names the boot key and grants nothing; the enrolment
    // names the device that asked and is kept by the device that answered.
    assert_eq!(
        response.cert.device_public_key,
        boot_key.verifying_key().to_bytes()
    );
    assert_eq!(response.cert.capabilities, DeviceCapabilities::nothing());
    verify_device_cert(&response.cert).expect("the person's root signed the boot key");
    assert!(
        !response.siblings.is_empty(),
        "the person's other devices hold the half"
    );
    // The device that signed the enrolment and the phone both hold the half;
    // only the device being enrolled is not asked, so a person with two
    // devices still has one to ask at boot.
    let asked: std::collections::BTreeSet<[u8; 32]> = response
        .siblings
        .iter()
        .map(|sibling| sibling.device_public_key)
        .collect();
    let signing = tilo_session::sync::device_identity()
        .expect("this session's own device")
        .device_cert
        .device_public_key;
    assert_eq!(
        asked,
        std::collections::BTreeSet::from([signing, devices.phone_key]),
        "the signing device and the phone hold the half"
    );
    assert!(
        !asked.contains(&devices.enrollee.verifying_key().to_bytes()),
        "the device being enrolled does not hold its own half"
    );
}

#[test]
fn a_refused_enrolment_signs_nothing() {
    let mut devices = TwoDevices::new();
    let (request, _boot_key) = request(SlotId {
        volume_uuid: [0x11; 16],
        keyslot: 4,
    });
    assert_eq!(devices.exchange(&request, false), EnrolReply::Denied);
    assert!(
        devices.questions.pending().is_empty(),
        "the sheet holds no answered question"
    );
}

/// The certificate the person's root kept here gives `key`.
fn cert_under_root(
    key: [u8; 32],
    label: &str,
    capabilities: DeviceCapabilities,
    now: u64,
) -> DeviceCert {
    let dir = tilo_identity::store::identity_dir_from_env().expect("this account's identity");
    let record = tilo_identity::store::resolve_active_identity(&dir).expect("the person's record");
    let root = tilo_identity::store::load_from_path(record).expect("the person's root");
    let mut cert = DeviceCert {
        user_public_key: root.group_public_key(),
        device_public_key: key,
        device_label: label.to_owned(),
        capabilities,
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).expect("the person's root signs");
    cert
}
