//! Another of the person's devices boots early and asks this one for its
//! disk-unlock half; the person allows it on the session's sheet, and the
//! half the boot could not read alone is released to it.
//!
//! The enrolment is the phone's own, published in its fabric record as the
//! phone's session publishes it: the desk's session merges it from the
//! person's fabric, so what happens here is the whole path — the sibling
//! exchange, the question, and the sealed half.
//!
//! An answer is a fact the answering device keeps, so a boot that loses its
//! stream before reading it hears the same answer when it comes back, with
//! the half sealed to the key it now holds.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::net::Ipv4Addr;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::RelayMode;
use iroh::endpoint::{Endpoint, RecvStream, SendStream};
use tilo_compositor::OutputConfig;
use tilo_device_fabric::link::{FabricLink as _, SharedLink};
use tilo_device_fabric::unlock::{UnlockEnrolment, UnlockFabricState};
use tilo_device_fabric::{DeviceId, FabricStamp};
use tilo_identity_host::device_fleet::DeviceFleetStore;
use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_network_actor::HostIdentity;
use tilo_service::{Answer, Question, Questions};
use tilo_session::fabric::Continuity;
use tilo_session::sync::{Client, OwnPresence};
use tilo_session::{Options, Session};
use tilo_unlock_protocol::{
    BootId, BootKeyCert, ClientFrame, Half, PROTOCOL_VERSION, SiblingFrame, SlotId, UNLOCK_ALPN,
    UnlockHello, UnlockRequest, decode_frame, encode_frame, frame_len, open_release,
    seal_sibling_half,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, IdentityPaths};

/// How long the loopback sessions have to come up and carry what they carry.
const DEADLINE: Duration = Duration::from_secs(30);
/// One pass of the sessions' loop.
const PASS: Duration = Duration::from_millis(20);
/// What the sheet lists a disk-unlock request as.
const KIND: &str = "network.device-unlock";
/// The boot asking, the same boot across reconnects.
const BOOT: BootId = BootId([0x33; 16]);

/// The person's identity in a directory of its own, and the other device
/// that asks this one for its half.
struct Person {
    identity: String,
    user_public_key: [u8; 32],
    phone_cert: DeviceCert,
    phone_key: ed25519_dalek::SigningKey,
    store: DeviceFleetStore,
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
}

fn person() -> Person {
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
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let registry = tilo_identity_host::registry::IdentityRegistryStore::from_env()
        .expect("the identity registry")
        .load_reconciled_cached()
        .expect("the registry reads");
    let profile = registry
        .person_identity()
        .expect("this device's person is known");
    let store = DeviceFleetStore::from_env().expect("the device fleet");
    let now = tilo_clock::unix_now_ns();
    let token = store
        .mint_enrollment(
            &profile.id,
            DeviceCapabilities::member(),
            now,
            now.saturating_add(3_600_000_000_000),
        )
        .expect("this device may enroll another of the person's");
    let phone_key = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
    let phone_cert = store
        .complete_enrollment(
            &profile.id,
            &token,
            tilo_identity_host::enrollment::EnrollmentRole::PersonalDevice,
            phone_key.verifying_key().to_bytes(),
            "The phone",
            now,
        )
        .expect("the second device is enrolled");
    Person {
        identity: profile.id,
        user_public_key: profile.user_public_key,
        phone_cert,
        phone_key,
        store,
        _env: env,
        _root: root,
    }
}

/// The public half of the person's secret-sync key: what a sibling half is
/// sealed to.
fn seal_key(person: &Person) -> tilo_seal::PublicKey {
    person
        .store
        .secret_sync_key(&person.identity)
        .expect("this device holds the person's root")
        .public_key()
        .expect("a secret-sync public key")
}

/// The desk's session, the phone's, and the boot early boot plays.
struct Devices {
    _person: Person,
    half: Half,
    cert: BootKeyCert,
    boot_key: ed25519_dalek::SigningKey,
    questions: Questions,
    desk: Rc<Client>,
    phone: Client,
    session: Session,
    runtime: tokio::runtime::Runtime,
    boot_endpoint: Endpoint,
}

impl Devices {
    fn new() -> Self {
        let person = person();
        let half = Half::generate().expect("a fresh half");
        let slot = SlotId {
            volume_uuid: [0x11; 16],
            keyslot: 1,
        };
        // The phone's boot key, certified by the person's root as enrolment
        // certifies it. The desk only ever sees the published enrolment.
        let boot_key = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
        let mut cert = BootKeyCert {
            version: PROTOCOL_VERSION,
            user_public_key: person.user_public_key,
            device_public_key: person.phone_key.verifying_key().to_bytes(),
            boot_public_key: boot_key.verifying_key().to_bytes(),
            slot,
            attestation_key: None,
            issued_at_ns: tilo_clock::unix_now_ns(),
            signature: [0; 64],
        };
        person
            .store
            .certify_boot_key(&person.identity, &mut cert)
            .expect("the person's root certifies the boot key");
        let phone_id = DeviceId::new(hex::encode(person.phone_key.verifying_key().to_bytes()))
            .expect("a device id");
        let state = UnlockFabricState {
            enrolments: vec![UnlockEnrolment {
                cert: cert.clone(),
                sealed_half: seal_sibling_half(&seal_key(&person), &cert, &half)
                    .expect("the half is sealed to the person's secret-sync key"),
                stamp: FabricStamp {
                    counter: tilo_clock::unix_now_ms(),
                    device: phone_id,
                },
            }],
            auto_approvals: Vec::new(),
            decisions: Vec::new(),
        };

        let questions = Questions::new();
        let desk = Rc::new(
            Client::start(
                tilo_session::sync::device_identity(),
                None,
                OwnPresence::default(),
                Vec::new(),
                Continuity::default(),
                Arc::new(|| {}),
                Some(questions.clone()),
                Vec::new(),
            )
            .expect("this device's endpoint binds"),
        );
        let desk_addr = desk.endpoint_addr();
        // The phone's session, on its own key, publishing what its record
        // holds.
        let phone = Client::start(
            Some(HostIdentity {
                device_cert: person.phone_cert.clone(),
                device_signing_key: person.phone_key.clone(),
                workspace_device_certs: std::collections::HashMap::default(),
                workspace_revocations: std::collections::HashMap::default(),
                identity_paths: None,
            }),
            None,
            OwnPresence::default(),
            Vec::new(),
            Continuity::default(),
            Arc::new(|| {}),
            None,
            Vec::new(),
        )
        .expect("the phone's endpoint binds");
        phone.connect(desk_addr);
        let session = Session::headless(
            Options {
                presentation: ui::headless::fixture_runner_inputs,
                collab: false,
                sync: Some(Rc::clone(&desk)),
                questions: Some(questions.clone()),
                ..Options::default()
            },
            &[OutputConfig::new("HEADLESS-1", 1280, 720)],
        )
        .expect("session");
        phone
            .fabric()
            .expect("the phone's session keeps a fabric")
            .share_unlock(state);
        let runtime = tokio::runtime::Runtime::new().expect("a runtime");
        let boot_endpoint = runtime.block_on(async {
            Endpoint::builder(iroh::endpoint::presets::Minimal)
                .relay_mode(RelayMode::Disabled)
                .bind_addr((Ipv4Addr::LOCALHOST, 0))
                .expect("a loopback address")
                .bind()
                .await
                .expect("the boot's endpoint binds")
        });
        Self {
            _person: person,
            half,
            cert,
            boot_key,
            questions,
            desk,
            phone,
            session,
            runtime,
            boot_endpoint,
        }
    }

    fn desk_link(&self) -> SharedLink {
        self.desk
            .fabric()
            .expect("a certified device's session keeps a fabric")
    }

    /// Let both sessions run until `done`, and fail the test when it takes
    /// longer than the loopback devices should need.
    fn carry(&mut self, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            self.session
                .step(Duration::from_millis(1))
                .expect("the session steps");
            self.phone.follow();
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the devices' sessions carried it"
            );
            std::thread::sleep(PASS);
        }
    }

    /// Wait until the desk's fabric carries the phone's enrolment.
    fn enrolled(&mut self) {
        self.carry(|devices| {
            devices
                .desk_link()
                .device_fabric()
                .snapshots()
                .values()
                .any(|snapshot| {
                    snapshot
                        .unlock
                        .enrolments
                        .iter()
                        .any(|enrolment| enrolment.cert == devices.cert)
                })
        });
    }

    /// The one question the desk's sheet lists for this boot.
    fn questioned(&mut self) -> Question {
        let mut found = None;
        self.carry(|devices| {
            found = devices
                .questions
                .pending()
                .into_iter()
                .find(|question| question.kind == KIND);
            found.is_some()
        });
        found.expect("the request reached the person")
    }

    /// The boot dials the desk and sends its signed request; what it holds to
    /// read the answer is [`Dial`].
    fn dial(&self, id: BootId, ephemeral: tilo_seal::SecretKey) -> Dial {
        let endpoint = self.boot_endpoint.clone();
        let desk_addr = self.desk.endpoint_addr();
        let cert = self.cert.clone();
        let boot_key = self.boot_key.clone();
        let (send, recv, digest) = self.runtime.block_on(async {
            let connection = endpoint
                .connect(desk_addr, UNLOCK_ALPN)
                .await
                .expect("the boot reaches its sibling");
            let (mut send, mut recv) = connection.open_bi().await.expect("a stream");
            let hello = UnlockHello {
                version: PROTOCOL_VERSION,
                boot_cert: cert.clone(),
            };
            write_client(&mut send, &ClientFrame::Hello(hello.clone())).await;
            let Some(SiblingFrame::Challenge(challenge)) = read_sibling(&mut recv).await else {
                panic!("the sibling issued a challenge");
            };
            assert_eq!(challenge.sibling_public_key, desk_key());
            let request = UnlockRequest::sign(
                &boot_key,
                &cert,
                &challenge,
                id,
                &ephemeral
                    .public_key()
                    .expect("the ephemeral key's public half"),
                None,
            )
            .expect("the request is signed");
            write_client(&mut send, &ClientFrame::Request(request.clone())).await;
            let digest = request
                .digest(&cert, &challenge.sibling_public_key)
                .expect("the request's digest");
            (send, recv, digest)
        });
        Dial {
            send,
            recv,
            digest,
            ephemeral,
            runtime: self.runtime.handle().clone(),
        }
    }
}

/// What one boot keeps between the frames it sends and reads.
struct Dial {
    send: SendStream,
    recv: RecvStream,
    digest: [u8; 32],
    ephemeral: tilo_seal::SecretKey,
    runtime: tokio::runtime::Handle,
}

impl Dial {
    fn read(&mut self) -> Option<SiblingFrame> {
        self.runtime.block_on(read_sibling(&mut self.recv))
    }

    /// The half this boot's own key opens out of the sibling's release.
    fn open(&self, frame: &SiblingFrame) -> Half {
        let SiblingFrame::Released(release) = frame else {
            panic!("expected a release, got {frame:?}");
        };
        open_release(&self.ephemeral, &self.digest, release).expect("the half opens")
    }

    /// The boot writes its request and keeps waiting.
    fn finish(&mut self) {
        let _ = self.send.finish();
    }
}

#[test]
fn a_boot_receives_the_half_the_person_allowed() {
    let mut devices = Devices::new();
    devices.enrolled();
    let half = devices.half.clone();

    let mut boot = devices.dial(BOOT, tilo_seal::SecretKey::generate().expect("a key"));
    let Some(SiblingFrame::Pending { approval_code }) = boot.read() else {
        panic!("the sibling asks its person");
    };
    // The person, at the sheet: the six digits the boot screen shows are the
    // ones the question shows back.
    let shown = devices.questioned();
    assert_eq!(shown.details["device"], "The phone");
    assert_eq!(shown.details["approval_code"], approval_code);
    assert!(devices.questions.answer(&shown.handle, Answer::default()));
    let Some(frame) = boot.read() else {
        panic!("the person released the half");
    };
    assert_eq!(boot.open(&frame), half, "the boot's own half, released");
    assert!(
        devices.questions.pending().is_empty(),
        "the sheet holds no answered question"
    );
}

#[test]
fn a_boot_hears_the_same_denial_without_a_second_question() {
    let mut devices = Devices::new();
    devices.enrolled();

    let mut boot = devices.dial(BOOT, tilo_seal::SecretKey::generate().expect("a key"));
    assert!(matches!(boot.read(), Some(SiblingFrame::Pending { .. })));
    let shown = devices.questioned();
    assert!(devices.questions.decline(&shown.handle));
    assert_eq!(
        boot.read(),
        Some(SiblingFrame::Denied),
        "the boot learns the person's answer"
    );
    // The answer never reaches the boot: its stream and connection go away.
    drop(boot);

    // It comes back as the same boot with a key of its own, and hears the
    // same answer instead of asking the person again.
    let mut again = devices.dial(BOOT, tilo_seal::SecretKey::generate().expect("a key"));
    assert_eq!(
        again.read(),
        Some(SiblingFrame::Denied),
        "the denial it missed the first time"
    );
    assert!(
        devices.questions.pending().is_empty(),
        "the person is asked once"
    );
}

#[test]
fn a_boot_that_reconnects_gets_the_half_for_the_key_it_now_holds() {
    let mut devices = Devices::new();
    devices.enrolled();
    let half = devices.half.clone();

    let mut boot = devices.dial(BOOT, tilo_seal::SecretKey::generate().expect("a key"));
    assert!(matches!(boot.read(), Some(SiblingFrame::Pending { .. })));
    let shown = devices.questioned();
    assert!(devices.questions.answer(&shown.handle, Answer::default()));
    let Some(frame) = boot.read() else {
        panic!("the decision reached the boot");
    };
    assert_eq!(boot.open(&frame), half);
    let unreachable = boot.ephemeral.clone();
    drop(boot);

    // The next attempt draws a new ephemeral key; the half is sealed to that
    // one, not to the key whose release never arrived.
    let mut again = devices.dial(BOOT, tilo_seal::SecretKey::generate().expect("a key"));
    let Some(frame) = again.read() else {
        panic!("the person's answer reached the boot again");
    };
    assert_eq!(
        again.open(&frame),
        half,
        "the same half, sealed to the boot's own new key"
    );
    if let SiblingFrame::Released(release) = &frame {
        assert!(
            open_release(&unreachable, &again.digest, release).is_err(),
            "the earlier key cannot open it"
        );
    }
    assert!(
        devices.questions.pending().is_empty(),
        "the person is asked once"
    );
}

#[test]
fn a_boot_that_went_away_takes_its_question_with_it() {
    let mut devices = Devices::new();
    devices.enrolled();

    let mut boot = devices.dial(BOOT, tilo_seal::SecretKey::generate().expect("a key"));
    assert!(matches!(boot.read(), Some(SiblingFrame::Pending { .. })));
    let shown = devices.questioned();
    assert_eq!(shown.details["device"], "The phone");
    boot.finish();
    drop(boot);

    devices.carry(|devices| devices.questions.pending().is_empty());
    assert!(
        devices.questions.pending().is_empty(),
        "nobody is left waiting for a boot that stopped waiting"
    );
}

/// The desk's fleet key: the sibling the boot is answered by.
fn desk_key() -> [u8; 32] {
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    let key = tilo_user_identity::persist::load_device_key(&paths).expect("this device's key");
    key.verifying_key().to_bytes()
}

async fn write_client(send: &mut SendStream, frame: &ClientFrame) {
    let bytes = encode_frame(frame).expect("a frame");
    send.write_all(&bytes).await.expect("the frame is written");
}

async fn read_sibling(recv: &mut RecvStream) -> Option<SiblingFrame> {
    let mut prefix = [0_u8; 4];
    recv.read_exact(&mut prefix).await.ok()?;
    let mut body = vec![0_u8; frame_len(prefix).ok()?];
    recv.read_exact(&mut body).await.ok()?;
    decode_frame(&body).ok()
}
