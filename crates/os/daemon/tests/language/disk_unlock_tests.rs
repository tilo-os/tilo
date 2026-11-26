//! Another of the person's devices boots early and asks this one's daemon for its disk-unlock
//! half; the person answers the question their daemon raised as an asked record, and the half the
//! boot could not read alone is released to it, sealed to its own ephemeral key.
//! The enrolment is kept in the record of the person's devices, which the daemon reads for every
//! boot: what happens here is the whole path — the sibling exchange over the daemon's endpoint,
//! the asked record the person answers, and the sealed half.
//! An answer is kept in that record, so a boot that loses its stream before reading it hears the
//! same answer when it comes back, with the half sealed to the key it now holds.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use iroh::EndpointAddr;
use iroh::RelayMode;
use iroh::endpoint::{Endpoint, RecvStream, SendStream};
use serde_json::json;
use tilo_action_services::workspace_invocations;
use tilo_actions::ActionAddress;
use tilo_actions::InvocationId;
use tilo_actions::remote::asked::{Asked, AskedRecords as _, RESOLVE};
use tilo_actions::remote::{Caller, InvokeArgument, InvokeRequest, RemoteActionTarget};
use tilo_unlock_protocol::published::{UnlockEnrolment, UnlockFabricState};
use tilo_unlock_protocol::{
    BootId, ClientFrame, Half, PROTOCOL_VERSION, SiblingFrame, SlotId, UNLOCK_ALPN, UnlockHello,
    UnlockRequest, decode_frame, encode_frame, frame_len, open_release, seal_sibling_half,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use tilo_workspace_service::workspaces::person_workspace::devices;

use crate::language::host;
use crate::language::host::Host;

/// The boot asking, the same boot across reconnects.
const BOOT: BootId = BootId([0x33; 16]);
/// How long a question, an answer or the devices' records are given.
const WITHIN: Duration = Duration::from_secs(30);
/// How often the person's questions and the devices' records are read again while they move.
const EVERY: Duration = Duration::from_millis(20);

/// A boot of one of the person's devices, enrolled in their devices record, and the endpoint it
/// asks the daemon at.
struct Fixture {
    key: SigningKey,
    cert: DeviceCert,
    half: Half,
    slot: SlotId,
    endpoint: Endpoint,
    daemon: EndpointAddr,
    person: [u8; 32],
}

/// Enrol a boot in the record of the person's devices as the daemon keeps it: the boot key
/// certified by the person's root, the half sealed to their secret-sync key, and the phone's disk
/// it unlocks.
async fn enrolled(host: &Host) -> Fixture {
    let person = hex::decode(host.person()).expect("the person is their root key");
    let person: [u8; 32] = person.try_into().expect("a root key is 32 bytes");
    let daemon = host.daemon();
    let workspaces = daemon.workspaces();
    let this = workspaces.device_cert();
    let (key, cert) = host.certify("Boot key", DeviceCapabilities::nothing());
    // The disk it unlocks is the phone's, one of the person's devices the fleet knows.
    let (_, phone) = host.certify("The phone", DeviceCapabilities::member());
    devices::keep(workspaces, &phone)
        .await
        .expect("the phone is listed with its person");
    let half = Half::generate().expect("a fresh half");
    let slot = SlotId {
        volume_uuid: [0x11; 16],
        keyslot: 1,
    };
    // The person's root keeps the half sealed to their secret-sync key, and the daemon, which
    // keeps the root, is where it opens.
    let root = tilo_identity::store::load_from_path(
        tilo_runtime::device::services::device_root_record().expect("the person founded here"),
    )
    .expect("the person's root");
    let secret_sync = root
        .secret_sync_key()
        .expect("the person's secret-sync key");
    let sealed_half = seal_sibling_half(
        &secret_sync
            .public_key()
            .expect("the secret-sync public key"),
        &cert,
        &half,
    )
    .expect("the half seals to the person's secret-sync key");
    let state = UnlockFabricState {
        enrolments: vec![UnlockEnrolment {
            cert: cert.clone(),
            device_public_key: phone.device_public_key,
            sealed_half,
            slot,
            attestation_key: None,
            kept_by: this.device_public_key,
        }],
        ..UnlockFabricState::default()
    };
    state
        .validate(&this.device_public_key)
        .expect("the enrolment is this device's");
    devices::keep_own_fact(
        workspaces,
        this,
        "unlock",
        &hex::encode(postcard::to_allocvec(&state).expect("the enrolment encodes")),
    )
    .await
    .expect("the enrolment is kept in the person's devices");
    let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .expect("a loopback address")
        .bind()
        .await
        .expect("the boot's endpoint binds");
    Fixture {
        key,
        cert,
        half,
        slot,
        endpoint,
        daemon: daemon.endpoint_addr(),
        person,
    }
}

impl Fixture {
    /// Dial the daemon as this boot and send its signed request; what it holds to read the
    /// answer is [`Dial`].
    async fn dial(&self, id: BootId, ephemeral: &tilo_seal::SecretKey) -> Dial {
        let connection = self
            .endpoint
            .connect(self.daemon.clone(), UNLOCK_ALPN)
            .await
            .expect("the boot reaches its sibling");
        let (mut send, mut recv) = connection.open_bi().await.expect("a stream");
        let hello = UnlockHello {
            version: PROTOCOL_VERSION,
            boot_cert: self.cert.clone(),
        };
        write_client(&mut send, &ClientFrame::Hello(hello.clone())).await;
        let challenge = match read_sibling(&mut recv).await {
            Some(SiblingFrame::Challenge(challenge)) => challenge,
            other => panic!("the sibling issued a challenge, not {other:?}"),
        };
        let request = UnlockRequest::sign(
            &self.key,
            &self.cert,
            self.slot,
            &challenge,
            id,
            &ephemeral
                .public_key()
                .expect("the ephemeral key's public half"),
            None,
        )
        .expect("the request is signed");
        let digest = request
            .digest(&self.cert, &challenge.sibling_public_key)
            .expect("the request's digest");
        write_client(&mut send, &ClientFrame::Request(request)).await;
        Dial { send, recv, digest }
    }
}

/// What one boot holds between the frames it sends and reads.
struct Dial {
    send: SendStream,
    recv: RecvStream,
    digest: [u8; 32],
}

impl Dial {
    async fn read(&mut self) -> Option<SiblingFrame> {
        read_sibling(&mut self.recv).await
    }

    /// The half this boot's own key opens out of the sibling's release.
    fn open(&self, frame: &SiblingFrame, ephemeral: &tilo_seal::SecretKey) -> Half {
        let SiblingFrame::Released(release) = frame else {
            panic!("expected a release, got {frame:?}");
        };
        open_release(ephemeral, &self.digest, release).expect("the half opens")
    }

    /// The boot stops waiting.
    fn finish(&mut self) {
        let _ = self.send.finish();
    }
}

/// The one question the daemon raised for this boot: what the person reads and answers.
async fn questioned(host: &Host, person: [u8; 32]) -> Asked {
    let started = Instant::now();
    loop {
        let store = host
            .daemon()
            .workspaces()
            .asked_store()
            .expect("the daemon keeps the calls that wait for its person");
        if let Some(asked) = store.asked_of(&person).into_iter().next() {
            return asked;
        }
        assert!(
            started.elapsed() < WITHIN,
            "the boot's question reached the person"
        );
        tokio::time::sleep(EVERY).await;
    }
}

/// Answer the question `asked` on the device it reached, as its person: `approve` or `decline`.
async fn answer(host: &Host, asked: &Asked, resolution: &str) {
    let caller = Caller::local_person(host.person()).in_workspace(host.workspace());
    let argument = serde_json::from_value(json!({ "resolution": resolution }))
        .expect("a resolution is a JSON object");
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: workspace_invocations::PROVIDER.to_owned(),
            address: ActionAddress::new(&asked.record.record, RESOLVE),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    host.daemon()
        .workspaces()
        .resolve_asked(&caller, &request)
        .await
        .expect("the daemon answers the resolve")
        .expect("the person's answer is admitted");
}

/// Wait until nobody is asked about a boot any more.
async fn nobody_asked(host: &Host, person: [u8; 32]) {
    let started = Instant::now();
    loop {
        let asked = host
            .daemon()
            .workspaces()
            .asked_store()
            .expect("the daemon keeps the calls that wait for its person")
            .asked_of(&person);
        if asked.is_empty() {
            return;
        }
        assert!(
            started.elapsed() < WITHIN,
            "the person is still asked about a boot that ended: {asked:?}"
        );
        tokio::time::sleep(EVERY).await;
    }
}

#[test]
fn a_boot_receives_the_half_the_person_allowed() {
    host::run(|host| async move {
        let boot = enrolled(&host).await;
        let half = boot.half.clone();
        let ephemeral = tilo_seal::SecretKey::generate().expect("a key");
        let mut dial = boot.dial(BOOT, &ephemeral).await;
        let Some(SiblingFrame::Pending { approval_code }) = dial.read().await else {
            panic!("the sibling asks its person");
        };
        // The person, at the question the daemon raised: the six digits the boot screen shows are
        // among the facts it shows back, and the device that asks is named.
        let asked = questioned(&host, boot.person).await;
        let tilo_actions::remote::asked::AskedBy::Device(device) = &asked.facts.by else {
            panic!("a device of the person asks: {:?}", asked.facts.by);
        };
        assert_eq!(device.name, "The phone");
        assert!(
            asked
                .facts
                .facts
                .iter()
                .any(|fact| fact.value == approval_code),
            "the code the boot shows is the one the person reads: {:?}",
            asked.facts.facts
        );
        answer(&host, &asked, "approve").await;
        let Some(frame) = dial.read().await else {
            panic!("the person released the half");
        };
        assert_eq!(
            dial.open(&frame, &ephemeral),
            half,
            "the boot's own half, released"
        );
        nobody_asked(&host, boot.person).await;
    });
}

#[test]
fn a_boot_hears_the_same_denial_without_a_second_question() {
    host::run(|host| async move {
        let boot = enrolled(&host).await;
        let mut dial = boot
            .dial(BOOT, &tilo_seal::SecretKey::generate().expect("a key"))
            .await;
        assert!(matches!(
            dial.read().await,
            Some(SiblingFrame::Pending { .. })
        ));
        let asked = questioned(&host, boot.person).await;
        answer(&host, &asked, "decline").await;
        assert_eq!(
            dial.read().await,
            Some(SiblingFrame::Denied),
            "the boot learns the person's answer"
        );
        // The answer never reaches the boot: its stream and connection go away.
        drop(dial);

        // It comes back as the same boot with a key of its own, and hears the same answer instead
        // of asking the person again.
        let mut again = boot
            .dial(BOOT, &tilo_seal::SecretKey::generate().expect("a key"))
            .await;
        assert_eq!(
            again.read().await,
            Some(SiblingFrame::Denied),
            "the denial it missed the first time"
        );
        nobody_asked(&host, boot.person).await;
    });
}

#[test]
fn a_boot_that_reconnects_gets_the_half_for_the_key_it_now_holds() {
    host::run(|host| async move {
        let boot = enrolled(&host).await;
        let half = boot.half.clone();
        let unreachable = tilo_seal::SecretKey::generate().expect("a key");
        let mut dial = boot.dial(BOOT, &unreachable).await;
        assert!(matches!(
            dial.read().await,
            Some(SiblingFrame::Pending { .. })
        ));
        let asked = questioned(&host, boot.person).await;
        answer(&host, &asked, "approve").await;
        let Some(frame) = dial.read().await else {
            panic!("the decision reached the boot");
        };
        assert_eq!(dial.open(&frame, &unreachable), half);
        drop(dial);

        // The next attempt draws a new ephemeral key; the half is sealed to that one, not to the
        // key whose release never arrived.
        let now = tilo_seal::SecretKey::generate().expect("a key");
        let mut again = boot.dial(BOOT, &now).await;
        let Some(frame) = again.read().await else {
            panic!("the person's answer reached the boot again");
        };
        assert_eq!(
            again.open(&frame, &now),
            half,
            "the same half, sealed to the boot's own new key"
        );
        if let SiblingFrame::Released(release) = &frame {
            assert!(
                open_release(&unreachable, &again.digest, release).is_err(),
                "the earlier key cannot open it"
            );
        }
        nobody_asked(&host, boot.person).await;
    });
}

#[test]
fn a_boot_that_went_away_takes_its_question_with_it() {
    host::run(|host| async move {
        let boot = enrolled(&host).await;
        let mut dial = boot
            .dial(BOOT, &tilo_seal::SecretKey::generate().expect("a key"))
            .await;
        assert!(matches!(
            dial.read().await,
            Some(SiblingFrame::Pending { .. })
        ));
        questioned(&host, boot.person).await;
        dial.finish();
        drop(dial);
        nobody_asked(&host, boot.person).await;
    });
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
