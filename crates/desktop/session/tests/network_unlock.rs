//! Another of the person's devices boots early and asks this one for its disk-unlock half; the
//! person allows it on the session's sheet, and the half the boot could not read alone is released
//! to it.
//! The enrolment is the phone's own, kept in its part of the person's store and read from there by
//! the shell's session, so what happens here is the whole path — the sibling exchange, the
//! question, and the sealed half.
//! An answer is kept in the record of the person's devices, so a boot that loses its stream before
//! reading it hears the same answer when it comes back, with the half sealed to the key it now
//! holds.
//! The phone's enrolment reaches the shell through that record, which the device's daemon keeps:
//! the phone's session keeps it there as its enrolment socket does.

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
use std::time::Duration;

use iroh::RelayMode;
use iroh::endpoint::{Endpoint, RecvStream, SendStream};
use tilo_compositor::OutputConfig;
use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_network_actor::DeviceIdentity;
use tilo_service::{Answer, AskReceiver, AskSender, Question};
use tilo_session::platform::fabric::Continuity;
use tilo_session::platform::fabric::{FabricLink as _, SharedLink};
use tilo_session::sync::{Client, Entry, OwnPresence};
use tilo_session::{Options, Session};
use tilo_unlock_protocol::{
    BootId, ClientFrame, Half, PROTOCOL_VERSION, SiblingFrame, SlotId, UNLOCK_ALPN, UnlockHello,
    UnlockRequest, decode_frame, encode_frame, frame_len, open_release,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

/// What the sheet lists a disk-unlock request as.
const KIND: &str = "network.device-unlock";
/// The boot asking, the same boot across reconnects.
const BOOT: BootId = BootId([0x33; 16]);

/// The person's identity in a directory of its own, and the other device that asks this one for its
/// half.
struct Person {
    user_public_key: [u8; 32],
    phone_cert: DeviceCert,
    phone_key: ed25519_dalek::SigningKey,
    /// The device key this process took as the daemon's, which the session signs with here: let go
    /// of with the identity's files.
    _taken_key: tilo_device_identity::persist::DeviceKeyLock,
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
    // The account's own store below the identity directory, where every reader looks, and the
    // person's devices a certificate is checked against before anything signs with it.
    tilo_identity::sources::test_trust::install();
    let personal = tilo_identity::store::account_identity_dir(&xdg.join("tilo/identity"));
    let paths = IdentityPaths::under(&system);
    // This process stands in for the machine's daemon: it takes the device key first, and keeps it
    // while the session this person's tests run signs with it.
    let taken_key = tilo_identity::provisioning::provisioned_as_its_daemon(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let registry = tilo_identity::store::IdentityRegistryStore::from_env()
        .expect("the identity registry")
        .load_reconciled_cached()
        .expect("the registry reads");
    let profile = registry
        .certified_identity()
        .expect("this device's person is known");
    let now = tilo_clock::unix_now_ns();
    let phone_key = test_server::signing_key(0x52);
    let phone_cert = cert_under_root(
        &personal,
        phone_key.verifying_key().to_bytes(),
        "The phone",
        DeviceCapabilities::member(),
        now,
    );
    Person {
        user_public_key: profile.user_public_key,
        phone_cert,
        phone_key,
        _taken_key: taken_key,
        _env: env,
        _root: root,
    }
}

/// The shell's session, the phone's, and the boot early boot plays.
struct Devices {
    person: Person,
    half: Half,
    cert: DeviceCert,
    slot: SlotId,
    boot_key: ed25519_dalek::SigningKey,
    questions: AskReceiver,
    shell: Rc<Client>,
    phone: Rc<Client>,
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
        // The phone's boot key, certified by the person's root as enrolment certifies it: an
        // ordinary device certificate that grants nothing. The shell only ever sees the published
        // enrolment.
        let boot_key = ed25519_dalek::SigningKey::from_bytes(&[0x77; 32]);
        let mut cert = DeviceCert {
            user_public_key: person.user_public_key,
            device_public_key: boot_key.verifying_key().to_bytes(),
            device_label: "Boot key".into(),
            capabilities: DeviceCapabilities::nothing(),
            issued_at_ns: tilo_clock::unix_now_ns(),
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root_signer())
            .expect("the person's root signs the boot key");
        let (asks, questions): (AskSender, AskReceiver) = tilo_service::asks::channel();
        let runtime = tokio::runtime::Runtime::new().expect("a runtime");
        let shell_identity = tilo_session::sync::device_identity().expect("this device's identity");
        let shell = Rc::new(
            Client::start(
                Entry {
                    identity: Some(shell_identity),
                    own: OwnPresence::default(),
                    hosts: Vec::new(),
                    continuity: Continuity::default(),
                    wake: Arc::new(|| {}),
                    relays: test_server::loopback(),
                    lan: None,
                },
                Some(asks.clone()),
                Vec::new(),
            )
            .expect("this device's endpoint binds"),
        );
        let shell_addr = shell.endpoint_addr();
        // The phone's session, on its own key, publishing what its part of the store holds.
        let phone = Rc::new(
            Client::start(
                Entry {
                    identity: Some(DeviceIdentity::kept(
                        person.phone_cert.clone(),
                        person.phone_key.clone(),
                    )),
                    own: OwnPresence::default(),
                    hosts: Vec::new(),
                    continuity: Continuity::default(),
                    wake: Arc::new(|| {}),
                    relays: test_server::loopback(),
                    lan: None,
                },
                None,
                Vec::new(),
            )
            .expect("the phone's endpoint binds"),
        );
        phone.connect(shell_addr);
        let session = crate::common::headless_on(
            &[OutputConfig::new("HEADLESS-1", 1280, 720)],
            Options {
                collab: false,
                sync: Some(Rc::clone(&shell)),
                asks: Some((asks, questions.clone())),
                ..Options::default()
            },
        );
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
            person,
            half,
            cert,
            slot,
            boot_key,
            questions,
            shell,
            phone,
            session,
            runtime,
            boot_endpoint,
        }
    }

    fn shell_link(&self) -> SharedLink {
        self.shell
            .fabric()
            .expect("a certified device's session keeps a fabric")
    }

    /// Let both sessions run until `done` : the suite's one wait, whose bound is the suite's.
    fn carry(&mut self, mut done: impl FnMut(&Self) -> bool) {
        tilo_test_env::wait_until("the devices' sessions carried it", || {
            self.session
                .step(Duration::from_millis(1))
                .expect("the session steps");
            self.phone.follow();
            done(self)
        });
    }

    /// Wait until the shell knows the phone's enrolment.
    fn enrolled(&mut self) {
        self.carry(|devices| {
            devices
                .shell_link()
                .devices()
                .devices()
                .values()
                .any(|device| {
                    device
                        .unlock
                        .enrolments
                        .iter()
                        .any(|enrolment| enrolment.cert == devices.cert)
                })
        });
    }

    /// The one question the shell's sheet lists for this boot.
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

    /// The boot dials the shell and sends its signed request; what it holds to read the answer is
    /// [`Dial`].
    fn dial(&self, id: BootId, ephemeral: tilo_seal::SecretKey) -> Dial {
        let endpoint = self.boot_endpoint.clone();
        let shell_addr = self.shell.endpoint_addr();
        let cert = self.cert.clone();
        let boot_key = self.boot_key.clone();
        let (send, recv, digest) = self.runtime.block_on(async {
            let connection = endpoint
                .connect(shell_addr, UNLOCK_ALPN)
                .await
                .expect("the boot reaches its sibling");
            let (mut send, mut recv) = connection.open_bi().await.expect("a stream");
            let hello = UnlockHello {
                version: PROTOCOL_VERSION,
                boot_cert: cert.clone(),
            };
            write_client(&mut send, &ClientFrame::Hello(hello.clone())).await;
            let challenge = match read_sibling(&mut recv).await {
                Some(SiblingFrame::Challenge(challenge)) => challenge,
                other => panic!("the sibling issued a challenge, not {other:?}"),
            };
            assert_eq!(challenge.sibling_public_key, shell_key());
            let request = UnlockRequest::sign(
                &boot_key,
                &cert,
                self.slot,
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
    // The person, at the sheet: the six digits the boot screen shows are the ones the question
    // shows back.
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

    // It comes back as the same boot with a key of its own, and hears the same answer instead of
    // asking the person again.
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

    // The next attempt draws a new ephemeral key; the half is sealed to that one, not to the key
    // whose release never arrived.
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

/// The shell's fleet key: the sibling the boot is answered by.
fn shell_key() -> [u8; 32] {
    let paths = tilo_identity::provisioning::collaboration_identity_paths();
    tilo_device_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate")
        .device_public_key
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

/// The root this computer keeps, as a test holds it to mint what a pairing mints.
fn root_signer() -> tilo_identity::store::SigningIdentity {
    let dir =
        tilo_identity::store::account_identity_dir_from_env().expect("this account's identity");
    let record = tilo_identity::store::resolve_active_identity(&dir).expect("the person's record");
    tilo_identity::store::load_from_path(record).expect("the person's root")
}

/// The certificate the person's root kept here gives `key`.
fn cert_under_root(
    _identity_dir: &std::path::Path,
    key: [u8; 32],
    label: &str,
    capabilities: DeviceCapabilities,
    now: u64,
) -> DeviceCert {
    let root = root_signer();
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
