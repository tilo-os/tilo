//! The person's speakers and remote audio between their devices, carried by the device's daemon:
//! a session asks its daemon to hand one envelope to another device, the daemon writes it on the
//! one connection it keeps to that device, and each device hands what arrives to its session.
//!
//! The other devices here are endpoints of the person's on this machine's loopback, running the
//! speakers protocol as a device's daemon does: they are kept in the record of the person's
//! devices, which is where this device's daemon finds them and who may take part.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iroh::endpoint::Connection;
use iroh::{Endpoint, RelayMode};
use serde_json::Value;
use tilo_actions::ActionAddress;
use tilo_actions::remote::ReceiptStatus;
use tilo_network_actor::fabric::clock::{self, ClockSyncState, hold_fabric_clock_sync};
use tilo_network_actor::fabric::{DeviceId, PersonalFabricAction, PersonalFabricActionEnvelope};
use tilo_session_client::speakers::{self, Frame};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use tilo_workspace_service::workspaces::person_workspace::devices::{self, Devices};

use crate::language::host::{self, Host};

/// A cancellation the source sends, before anything was approved.
const CANCEL: [u8; 16] = [1; 16];
/// What the other device sends back.
const BACK: [u8; 16] = [2; 16];

/// This device's certificate, as its daemon presents it: the key every envelope from this
/// device's session is signed as.
fn this_device() -> DeviceCert {
    let paths = tilo_identity::provisioning::collaboration_identity_paths();
    tilo_device_identity::persist::read_owner_cert(&paths)
        .expect("this device's daemon holds its certificate")
        .expect("the owner's certificate")
}

/// `invoke`, a typed action on the record of the person's devices, run by their daemon as them.
async fn record(host: &Host, invoke: &tilo_resource::Invoke<Devices>) {
    let provider = format!(
        "{}:{}:{}",
        tilo_actions::remote::RESOURCE_PROVIDER,
        devices::DEVICES.0,
        devices::DEVICES.1
    );
    let request = host::call(
        &provider,
        ActionAddress::new(<Devices as tilo_resource::Resource>::KIND, invoke.id),
        &Value::Object(invoke.arguments.clone()),
    );
    // The session's own way to the record: the shell link, which the daemon answers as its
    // person.
    let stream = host
        .daemon()
        .shell_link()
        .expect("the daemon hands its shell a link");
    stream
        .set_nonblocking(true)
        .expect("the link is made nonblocking");
    let (_, _, shell) = tilo_session_client::daemon_documents::on_daemon(
        Ok(runtime_collab::ShellClient::open(
            tokio::net::UnixStream::from_std(stream).expect("a socket"),
        )
        .0),
        tokio::runtime::Handle::current(),
    );
    let receipt = shell
        .invoke_receipt(Some(host.workspace()), request)
        .await
        .expect("the daemon answers");
    assert!(
        matches!(receipt.status, ReceiptStatus::Completed { .. }),
        "the record takes it: {:?}",
        receipt.status
    );
}

/// An envelope from this device's session to `target`, carrying `action`.
fn envelope(target: &DeviceId, action: PersonalFabricAction) -> PersonalFabricActionEnvelope {
    let source = this_device();
    PersonalFabricActionEnvelope {
        request_id: [0x6b; 16],
        source: DeviceId::new(hex::encode(source.device_public_key)).expect("a device id"),
        target: target.clone(),
        issued_unix_ms: tilo_clock::unix_now_ms(),
        action,
    }
}

/// The session's end of its daemon's speakers link, as the session opens it.
async fn link(host: &Host) -> tokio::net::UnixStream {
    let stream = host
        .daemon()
        .shell_link()
        .expect("the daemon hands its shell a link");
    stream
        .set_nonblocking(true)
        .expect("the link is made nonblocking");
    let shell = runtime_collab::ShellClient::open(
        tokio::net::UnixStream::from_std(stream).expect("a socket"),
    )
    .0;
    shell
        .speakers()
        .await
        .expect("the daemon serves the speakers link")
}

/// One of the person's other devices, on this machine's loopback: its endpoint answers the
/// speakers protocol, keeping what it is sent, and may answer in turn.
struct Peer {
    cert: DeviceCert,
    at: SocketAddr,
    heard: Arc<Mutex<Vec<PersonalFabricActionEnvelope>>>,
    _router: iroh::protocol::Router,
    _endpoint: Endpoint,
}

impl Peer {
    /// `label`'s device, certified by the person's root, answering with `answer` when one is
    /// given.
    async fn start(
        host: &Host,
        label: &str,
        answer: Option<Arc<dyn Fn(&PersonalFabricActionEnvelope) -> Option<Frame> + Send + Sync>>,
    ) -> Self {
        let keyed = host.certify(label, DeviceCapabilities::member());
        Self::with_key(keyed, answer).await
    }

    /// The device `keyed` is, of whichever root certified it.
    async fn with_key(
        (key, cert): (ed25519_dalek::SigningKey, DeviceCert),
        answer: Option<Arc<dyn Fn(&PersonalFabricActionEnvelope) -> Option<Frame> + Send + Sync>>,
    ) -> Self {
        let endpoint = Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
            .relay_mode(RelayMode::Disabled)
            .bind_addr((Ipv4Addr::LOCALHOST, 0))
            .expect("a loopback address")
            .bind()
            .await
            .expect("the other device's endpoint binds");
        let at = {
            let port = endpoint
                .bound_sockets()
                .first()
                .expect("a bound socket")
                .port();
            SocketAddr::from((Ipv4Addr::LOCALHOST, port))
        };
        let heard = Arc::new(Mutex::new(Vec::new()));
        let router = iroh::protocol::Router::builder(endpoint.clone())
            .accept(
                speakers::ALPN,
                Serving {
                    heard: Arc::clone(&heard),
                    answer,
                },
            )
            .spawn();
        Self {
            cert,
            at,
            heard,
            _router: router,
            _endpoint: endpoint,
        }
    }

    /// Keep this device in the person's record, where their daemon reaches it: what it is, and
    /// where its endpoint answers.
    async fn kept(&self, host: &Host) {
        record(
            host,
            &Devices::keeping(&self.cert).expect("the certificate encodes"),
        )
        .await;
        let addrs = vec![self.at];
        record(
            host,
            &Devices::KEEP_FACT.with((
                hex::encode(self.cert.device_public_key),
                hex::encode(postcard::to_allocvec(&self.cert).expect("the certificate encodes")),
                devices::ENDPOINT_FACT.to_owned(),
                hex::encode(postcard::to_allocvec(&addrs).expect("the addresses encode")),
            )),
        )
        .await;
    }

    fn id(&self) -> DeviceId {
        DeviceId::new(hex::encode(self.cert.device_public_key)).expect("a device id")
    }

    fn heard(&self) -> Vec<PersonalFabricActionEnvelope> {
        self.heard.lock().expect("the heard envelopes").clone()
    }
}

/// The other device's daemon end: it keeps the envelopes it is sent and answers with `answer`.
struct Serving {
    heard: Arc<Mutex<Vec<PersonalFabricActionEnvelope>>>,
    answer: Option<Arc<dyn Fn(&PersonalFabricActionEnvelope) -> Option<Frame> + Send + Sync>>,
}

impl std::fmt::Debug for Serving {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Serving")
    }
}

impl iroh::protocol::ProtocolHandler for Serving {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        let Ok((mut send, mut recv)) = connection.accept_bi().await else {
            return Ok(());
        };
        while let Ok(frame) = speakers::read(&mut recv).await {
            let Frame::Send { envelope, .. } = frame else {
                continue;
            };
            let Ok(envelope) = postcard::from_bytes::<PersonalFabricActionEnvelope>(&envelope)
            else {
                break;
            };
            self.heard
                .lock()
                .expect("the heard envelopes")
                .push(envelope.clone());
            if let Some(reply) = self.answer.as_ref().and_then(|answer| answer(&envelope))
                && speakers::write(&mut send, &reply).await.is_err()
            {
                break;
            }
        }
        Ok(())
    }
}

/// The session's link, as the pass drives it: what it sends, and what came back.
struct Carriage {
    out: tokio::sync::mpsc::UnboundedSender<Frame>,
    heard: Arc<Mutex<Vec<(DeviceId, PersonalFabricActionEnvelope, u64)>>>,
}

impl Carriage {
    async fn open(host: &Host) -> Self {
        let (mut reader, mut writer) = tokio::io::split(link(host).await);
        let (out, mut queue) = tokio::sync::mpsc::unbounded_channel();
        let heard = Arc::new(Mutex::new(Vec::new()));
        let pushing = Arc::clone(&heard);
        tokio::spawn(async move {
            while let Some(frame) = queue.recv().await {
                if speakers::write(&mut writer, &frame).await.is_err() {
                    return;
                }
            }
        });
        tokio::spawn(async move {
            while let Ok(frame) = speakers::read(&mut reader).await {
                let Frame::Heard {
                    peer,
                    envelope,
                    received_us,
                } = frame
                else {
                    continue;
                };
                let Ok(envelope) = postcard::from_bytes(&envelope) else {
                    continue;
                };
                let Ok(peer) = DeviceId::new(hex::encode(peer)) else {
                    continue;
                };
                pushing
                    .lock()
                    .expect("the heard envelopes")
                    .push((peer, envelope, received_us));
            }
        });
        Self { out, heard }
    }

    fn tell(&self, envelope: &PersonalFabricActionEnvelope) -> Result<(), ()> {
        let Ok(target) = hex::decode(envelope.target.as_str()) else {
            return Err(());
        };
        let target: [u8; 32] = target.try_into().map_err(|_| ())?;
        let bytes = postcard::to_allocvec(envelope).map_err(|_| ())?;
        self.out
            .send(Frame::Send {
                peer: target,
                envelope: bytes,
            })
            .map_err(|_| ())
    }

    fn take(&self) -> Vec<(DeviceId, PersonalFabricActionEnvelope, u64)> {
        std::mem::take(&mut *self.heard.lock().expect("the heard envelopes"))
    }
}

/// What a session on the other device answers a clock probe with: its own clock.
fn clock_answer(
    peer: &DeviceCert,
) -> Arc<dyn Fn(&PersonalFabricActionEnvelope) -> Option<Frame> + Send + Sync> {
    let device_public_key = peer.device_public_key;
    Arc::new(move |envelope: &PersonalFabricActionEnvelope| {
        let PersonalFabricAction::FabricClockProbe { nonce, sent_us } = envelope.action else {
            return None;
        };
        let received_us = clock::fabric_monotonic_us();
        let action = clock::with_fabric_clock(|core| {
            core.answer(nonce, sent_us, received_us, clock::fabric_monotonic_us())
        })?;
        let reply = PersonalFabricActionEnvelope {
            request_id: [0x77; 16],
            source: DeviceId::new(hex::encode(device_public_key)).ok()?,
            target: envelope.source.clone(),
            issued_unix_ms: tilo_clock::unix_now_ms(),
            action,
        };
        Some(Frame::Send {
            peer: [0; 32],
            envelope: postcard::to_allocvec(&reply).ok()?,
        })
    })
}

fn cancel(session_id: [u8; 16]) -> PersonalFabricAction {
    PersonalFabricAction::RemoteAudioCancel { session_id }
}

/// A remote audio route learns the clock of the device it plays the other device by, over the
/// daemon: what a route holds while it runs.
#[test]
fn a_remote_audio_route_gets_the_clock_it_plays_the_other_device_by() {
    host::run(|host| async move {
        let keyed = host.certify("Laptop", DeviceCapabilities::member());
        let answer = clock_answer(&keyed.1);
        let peer = Peer::with_key(keyed, Some(answer)).await;
        peer.kept(&host).await;
        let carriage = Carriage::open(&host).await;
        let daemon = this_device();
        let daemon_id = DeviceId::new(hex::encode(daemon.device_public_key)).expect("a device id");
        let peer_id = peer.id();
        let clock = hold_fabric_clock_sync(peer_id.clone());
        let online = BTreeSet::from([peer_id]);
        let send =
            |peer: &DeviceId, action: PersonalFabricAction| carriage.tell(&envelope(peer, action));
        // A pass with Media continuity on starts the clock; nothing is known of the other
        // device's clock yet.
        clock::pump(true, Some(&daemon_id), &online, &send);
        assert_eq!(
            clock.state(),
            ClockSyncState::Syncing,
            "nothing is known of the other device's clock yet"
        );
        let deadline = tilo_test_env::hang_deadline();
        loop {
            clock::pump(true, Some(&daemon_id), &online, &send);
            for (source, envelope, received_us) in carriage.take() {
                clock::arrived_at(&source, &envelope.action, received_us, &send);
            }
            if clock.state() == ClockSyncState::Ready
                && clock.estimate().is_some_and(|estimate| estimate.synced)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the other device's clock never read: {:?}",
                clock.state()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Both devices are on this machine, so the other one's clock is this one's.
        let now_us = clock::fabric_monotonic_us();
        let offset_us = clock
            .estimate()
            .and_then(|estimate| estimate.offset_us(now_us))
            .expect("an estimate of the other device's clock");
        assert!(
            offset_us.abs() < 1_000,
            "the other device's clock is this device's: {offset_us} us off"
        );
    });
}

/// Two of the person's devices on one network reach each other without a relay, and a device that
/// is not the person's is neither reached nor heard: the daemon dials where the record says the
/// other one answers, and only the devices the record lists take part.
#[test]
fn the_persons_devices_on_one_network_reach_each_other_without_a_relay() {
    host::run(|host| async move {
        let peer = Peer::start(&host, "Laptop", None).await;
        peer.kept(&host).await;
        let carriage = Carriage::open(&host).await;
        let peer_id = peer.id();

        // The action reaches the other device at the address its record keeps, with no relay.
        carriage
            .tell(&envelope(&peer_id, cancel(CANCEL)))
            .expect("the link takes the action");
        let deadline = tilo_test_env::hang_deadline();
        while peer.heard().is_empty() {
            assert!(Instant::now() < deadline, "the other device heard nothing");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let heard = peer.heard();
        assert_eq!(heard[0].action, cancel(CANCEL));

        // A key the record does not list names no device this daemon reaches: the envelope goes
        // nowhere, and the one device that is kept hears no more than it did.
        let unlisted = DeviceId::new(hex::encode([0x44; 32])).expect("a device id");
        assert!(
            carriage.tell(&envelope(&unlisted, cancel([9; 16]))).is_ok(),
            "the link takes the envelope, and the daemon reaches no such device"
        );
        let quiet = Instant::now() + Duration::from_millis(500);
        while Instant::now() < quiet {
            assert_eq!(
                peer.heard().len(),
                1,
                "a device the record does not list is not reached"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
}

/// An action reaches the other device's session, and what it sends reaches this one: both
/// directions of the one link.
#[test]
fn an_action_reaches_the_other_devices_session() {
    host::run(|host| async move {
        let ours = this_device();
        let answer: Arc<dyn Fn(&PersonalFabricActionEnvelope) -> Option<Frame> + Send + Sync> =
            Arc::new(move |envelope: &PersonalFabricActionEnvelope| {
                assert_eq!(envelope.action, cancel(CANCEL), "the action as it was sent");
                let reply = PersonalFabricActionEnvelope {
                    request_id: [0x33; 16],
                    source: envelope.target.clone(),
                    target: envelope.source.clone(),
                    issued_unix_ms: tilo_clock::unix_now_ms(),
                    action: cancel(BACK),
                };
                let _ = &ours;
                Some(Frame::Send {
                    peer: [0; 32],
                    envelope: postcard::to_allocvec(&reply).ok()?,
                })
            });
        let peer = Peer::start(&host, "Laptop", Some(answer)).await;
        peer.kept(&host).await;
        let carriage = Carriage::open(&host).await;
        let peer_id = peer.id();
        carriage
            .tell(&envelope(&peer_id, cancel(CANCEL)))
            .expect("the link takes the action");

        let deadline = tilo_test_env::hang_deadline();
        let mut heard = Vec::new();
        while heard.is_empty() {
            heard.extend(carriage.take());
            assert!(
                Instant::now() < deadline,
                "the other device's answer never reached this session"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(heard[0].0, peer_id, "the other device sent it");
        assert_eq!(heard[0].1.action, cancel(BACK), "the answer as it was sent");
        assert_eq!(peer.heard()[0].action, cancel(CANCEL));
    });
}

/// An action addressed to a third device is handed to that one alone.
#[test]
fn an_action_addressed_to_a_third_device_is_not_delivered() {
    host::run(|host| async move {
        let laptop = Peer::start(&host, "Laptop", None).await;
        let phone = Peer::start(&host, "Phone", None).await;
        let pod = Peer::start(&host, "Pod", None).await;
        for peer in [&laptop, &phone, &pod] {
            peer.kept(&host).await;
        }
        let carriage = Carriage::open(&host).await;
        carriage
            .tell(&envelope(&pod.id(), cancel(CANCEL)))
            .expect("the link takes the action");

        let deadline = tilo_test_env::hang_deadline();
        while pod.heard().is_empty() {
            assert!(
                Instant::now() < deadline,
                "the pod was handed the action addressed to it"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Whatever the daemon did with the action would have happened by now.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(laptop.heard().is_empty(), "the laptop was handed nothing");
        assert!(phone.heard().is_empty(), "the phone was handed nothing");
        assert_eq!(pod.heard().len(), 1, "the pod was handed it once");
    });
}
