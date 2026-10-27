//! Chat's semantic call commands (the exact `Command` values its `chat.call`
//! actions emit: Join, `SetMuted`, Leave) driven through the real service
//! lifecycle owner, the real local consent prompt queue and the real capture
//! lease broker, into a backend that produces actual call audio: real libopus
//! (shell `GStreamer` codec) over the real native `moq-lite` client and relay
//! on loopback, decoded and tone-checked by a second native participant.
//! SYNTHETIC device: the capture lease hands a generated tone source to the
//! backend instead of a `PipeWire` microphone (no audio device on this host).
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "tone synthesis, Goertzel analysis and frame counters over bounded test sample buffers"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
use super::super::collab_broker_capnp::LocalSessionAuthority;
use super::*;
use crate::portal::{LocalCaptureHandle, PromptOutcome, PromptResolver};
use media::voice::gst::{OpusDecoder, OpusEncoder};
use std::sync::atomic::{AtomicU64, AtomicUsize};
use tilo_moq::client::{self, CapturedOpus, Client, ClientAuthority, ConnectionScope};
use tilo_moq::relay::{Denied, Direction, LoopbackRelay, RelayAuthority, SessionAuthorization};

const CALL: &str = "d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1";

/// Polls `condition` on the runtime until it holds; only a hang fails it.
async fn settle(what: &str, mut condition: impl FnMut() -> bool) {
    tilo_test_env::within(what, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}
fn namespace(who: char) -> String {
    format!("{CALL}/{}", who.to_string().repeat(64))
}
struct Scopes(String, Vec<String>);
impl SessionAuthorization for Scopes {
    fn publication(&self) -> Option<&str> {
        Some(&self.0)
    }
    fn subscriptions(&self) -> &[String] {
        &self.1
    }
    fn current(&self, direction: Direction, name: &str) -> bool {
        match direction {
            Direction::Publish => name == self.0,
            Direction::Subscribe => self.1.iter().any(|value| value == name),
        }
    }
}
struct Identities;
impl RelayAuthority for Identities {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let (own, other) = match proof {
            b"shell-caller" => ('a', 'b'),
            b"remote-listener" => ('b', 'a'),
            _ => return Err(Denied),
        };
        Ok(Arc::new(Scopes(namespace(own), vec![namespace(other)])))
    }
}
/// Exactly this scope, hearing exactly the one other participant.
struct Exact(ConnectionScope, String);
impl ClientAuthority for Exact {
    fn current(&self, scope: &ConnectionScope, direction: Direction, name: &str) -> bool {
        scope == &self.0
            && match direction {
                Direction::Publish => self.0.publication.as_deref() == Some(name),
                Direction::Subscribe => {
                    self.0.subscription_prefix.as_deref() == Some(name) || self.1 == name
                }
            }
    }
}
fn connect(relay: &LoopbackRelay, own: char, other: char, proof: &[u8]) -> Client {
    let scope = ConnectionScope {
        endpoint: relay.endpoint.clone(),
        fingerprints: relay.fingerprints.clone(),
        owner_epoch: [3; 32],
        carrier_generation: 1,
        publication: Some(namespace(own)),
        subscription_prefix: Some(CALL.to_owned()),
    };
    Client::start(
        client::Connection {
            scope: scope.clone(),
            authorization: proof.to_vec(),
        },
        Arc::new(Exact(scope, namespace(other))),
    )
    .unwrap()
}
fn tone_packets() -> Vec<Vec<u8>> {
    let mut encoder = OpusEncoder::new().expect("GStreamer opusenc");
    let pcm: Vec<i16> = (0..48_000)
        .map(|i| {
            ((f64::from(i) / 48_000.0 * 700.0 * std::f64::consts::TAU).sin() * 16_000.0) as i16
        })
        .collect();
    encoder.encode(&pcm)
}
fn tone_share(samples: &[i16], frequency: f64) -> f64 {
    let c = 2.0 * (std::f64::consts::TAU * frequency / 48_000.0).cos();
    let (mut s1, mut s2, mut energy) = (0.0f64, 0.0f64, 0.0f64);
    for &sample in samples {
        let x = f64::from(sample) / 32768.0;
        energy += x * x;
        let s0 = x + c * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    if energy < 1e-4 {
        0.0
    } else {
        (2.0 * (s1 * s1 + s2 * s2 - c * s1 * s2)) / (samples.len() as f64 * energy)
    }
}

#[derive(Default)]
struct Evidence {
    starts: AtomicUsize,
    stops: AtomicUsize,
    closes: AtomicUsize,
    sent: AtomicU64,
    /// Held across one packet's check, send and count, and by every stop, so
    /// a packet that passed the check before a stop is counted before it.
    send: Mutex<()>,
}
/// Real transport/codec backend. Capture state is one atomic fence that the
/// broker's synchronous stop handle flips before any packet can follow.
struct AudioBackend {
    client: Arc<Client>,
    capturing: Arc<AtomicBool>,
    evidence: Arc<Evidence>,
    packets: Arc<Vec<Vec<u8>>>,
}
impl MediaSession for AudioBackend {
    fn tick(&mut self, _: Instant) -> Result<State, Issue> {
        Ok(match self.client.status() {
            client::Status::Live => State::Live,
            client::Status::Connecting => State::Joining,
            _ => State::Reconnecting,
        })
    }
    fn capture_ready(&self) -> bool {
        self.capturing.load(Ordering::SeqCst) && self.evidence.sent.load(Ordering::SeqCst) > 0
    }
    fn inventory(&self) -> Inventory {
        Inventory::default()
    }
    #[allow(
        clippy::panic_in_result_fn,
        reason = "the test backend asserts the broker handed it exactly the leased synthetic source"
    )]
    fn start_capture(
        &mut self,
        source_id: &str,
        source: BorrowedCaptureHandle<'_>,
        current: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<(), Issue> {
        // Only the broker's leased synthetic source can start capture.
        assert_eq!(source_id, "mic");
        assert!(matches!(source, BorrowedCaptureHandle::PipeWireNode(23)));
        self.client
            .enable_publication()
            .map_err(|_| Issue::TransportFailed)?;
        self.capturing.store(true, Ordering::SeqCst);
        self.evidence.starts.fetch_add(1, Ordering::SeqCst);
        let (client, capturing, evidence, packets) = (
            self.client.clone(),
            self.capturing.clone(),
            self.evidence.clone(),
            self.packets.clone(),
        );
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(20));
            let mut index = 0u64;
            loop {
                ticker.tick().await;
                let _send = evidence.send.lock_recover();
                if !capturing.load(Ordering::SeqCst) || !current() {
                    break;
                }
                let payload = packets[(index as usize) % packets.len()].clone();
                if client
                    .try_send(CapturedOpus {
                        timestamp_48k: index * 960,
                        duration_samples: 960,
                        payload,
                        captured_at: Instant::now(),
                    })
                    .is_ok()
                {
                    evidence.sent.fetch_add(1, Ordering::SeqCst);
                }
                index += 1;
            }
        });
        Ok(())
    }
    fn capture_stop(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let (capturing, client, evidence) = (
            self.capturing.clone(),
            self.client.clone(),
            self.evidence.clone(),
        );
        Some(Arc::new(move || {
            let _send = evidence.send.lock_recover();
            capturing.store(false, Ordering::SeqCst);
            client.stop_publication();
            evidence.stops.fetch_add(1, Ordering::SeqCst);
        }))
    }
    fn pause_capture(&mut self) {
        self.stop_capture();
    }
    fn stop_capture(&mut self) {
        let _send = self.evidence.send.lock_recover();
        self.capturing.store(false, Ordering::SeqCst);
        self.client.stop_publication();
        self.evidence.stops.fetch_add(1, Ordering::SeqCst);
    }
    fn close(&mut self) {
        self.stop_capture();
        self.client.close();
        self.evidence.closes.fetch_add(1, Ordering::SeqCst);
    }
}
struct AudioFactory {
    relay: Arc<LoopbackRelay>,
    evidence: Arc<Evidence>,
    packets: Arc<Vec<Vec<u8>>>,
}
impl MediaFactory for AudioFactory {
    fn prepare(&self, target: CallTarget) -> Prepare {
        assert_eq!(
            (target.app.as_str(), target.channel.as_str()),
            ("so.tilo.chat", "voice")
        );
        let backend = AudioBackend {
            client: Arc::new(connect(&self.relay, 'a', 'b', b"shell-caller")),
            capturing: Arc::new(AtomicBool::new(false)),
            evidence: self.evidence.clone(),
            packets: self.packets.clone(),
        };
        Box::pin(async move { Ok(Box::new(backend) as Box<dyn MediaSession>) })
    }
}

struct Listener {
    client: Client,
    decoder: OpusDecoder,
    frames: usize,
    tone: usize,
}
impl Listener {
    fn drain(&mut self) {
        while let Some(frame) = self.client.try_recv() {
            self.frames += 1;
            let pcm = self.decoder.decode(&frame.object.payload);
            if pcm.len() == 960 && tone_share(&pcm, 700.0) > 0.5 {
                self.tone += 1;
            }
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end audio scenario over live loopback endpoints; each phase asserts on \
              state the previous phase left behind"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_semantic_call_commands_drive_consented_real_native_call_audio() {
    assert!(
        OpusEncoder::new().is_some(),
        "native call proof requires GStreamer opusenc"
    );
    let relay = Arc::new(LoopbackRelay::start(Arc::new(Identities)).unwrap());
    let owner = LocalSessionAuthority::new();
    let lifecycle = owner.lifecycle();
    lifecycle.publish_native_state(false, false);
    let workspace = super::super::state::WorkspaceTree::bootstrap_demo()
        .desktop
        .workspaces
        .remove(0);
    let room = workspace.room().to_string();
    let (grants, _) = GrantStoreHandle::test_with_workspace(workspace);
    let prompts = PromptQueueHandle::new(PromptResolver::Channel);
    let mut capture = CaptureBroker::new();
    capture
        .register_source(
            "mic",
            "Synthetic tone microphone",
            CaptureKind::Microphone,
            LocalCaptureHandle::PipeWireNode(23),
        )
        .unwrap();
    let service = NativeMediaService::new(
        lifecycle.clone(),
        Arc::new(Mutex::new(capture)),
        super::super::personal_continuity::DeviceId::new("device-audio").unwrap(),
    );
    assert!(service.install_consent(grants, prompts.clone()));
    let evidence = Arc::new(Evidence::default());
    assert!(service.install_factory(Arc::new(AudioFactory {
        relay: relay.clone(),
        evidence: evidence.clone(),
        packets: Arc::new(tone_packets()),
    })));
    let caller = Caller {
        peer: 21,
        app: "so.tilo.chat".into(),
        workspace: room,
        record: String::new(),
        current: Arc::new(|| Ok(())),
    };
    let intent = AtomicU64::new(0);
    let command = |command: Command| {
        let mut request = Request::new(command);
        if !matches!(request.command, Command::Status) {
            request.intent = intent.fetch_add(1, Ordering::SeqCst) + 1;
        }
        service.command_for(caller.clone(), request).unwrap()
    };
    let tick = || {
        let generation = {
            let mut calls = service.0.calls.lock().unwrap();
            let Some(entry) = calls.entries.get_mut(&caller.key()) else {
                return;
            };
            if let Some(worker) = entry.worker.take() {
                worker.abort();
            }
            entry.generation
        };
        service.0.tick(&caller.key(), generation);
    };
    let mut listener = Listener {
        client: connect(&relay, 'b', 'a', b"remote-listener"),
        decoder: OpusDecoder::new().unwrap(),
        frames: 0,
        tone: 0,
    };
    // "Join muted": listening only, no consent requested, nothing published.
    let joined = command(Command::Join {
        channel_id: "voice".into(),
    });
    assert!(joined.muted);
    settle("the native call to go live", || {
        command(Command::Status).state == State::Live
    })
    .await;
    // Nothing captured and nothing sent: no audio can reach the listener.
    tick();
    listener.drain();
    assert!(
        prompts.drain().is_empty(),
        "joining never prompts for the microphone"
    );
    assert_eq!(
        (
            listener.frames,
            evidence.starts.load(Ordering::SeqCst),
            evidence.sent.load(Ordering::SeqCst)
        ),
        (0, 0, 0)
    );

    // A declined "Request microphone" never starts capture or audio.
    command(Command::SetMuted { muted: false });
    prompts
        .drain()
        .pop()
        .expect("local consent prompt")
        .reply
        .send(PromptOutcome::Deny)
        .unwrap();
    settle("the call to take the declined consent", || {
        command(Command::Status).issue == Some(Issue::PermissionDenied)
    })
    .await;
    tick();
    listener.drain();
    assert_eq!(
        (
            listener.frames,
            evidence.starts.load(Ordering::SeqCst),
            evidence.sent.load(Ordering::SeqCst)
        ),
        (0, 0, 0)
    );
    assert!(!command(Command::Status).capture_active);

    // Approved "Request microphone": real Opus reaches the other participant.
    command(Command::SetMuted { muted: false });
    let approved_at = Instant::now();
    prompts
        .drain()
        .pop()
        .expect("local consent prompt")
        .reply
        .send(PromptOutcome::AllowOnce)
        .unwrap();
    let mut first_audio_ms = None;
    settle("fifty decoded tone frames at the listener", || {
        tick();
        listener.drain();
        if first_audio_ms.is_none() && listener.tone > 0 {
            first_audio_ms = Some(approved_at.elapsed().as_millis());
        }
        listener.tone >= 50
    })
    .await;
    assert_eq!(evidence.starts.load(Ordering::SeqCst), 1);
    assert!(command(Command::Status).capture_active);
    let audible = (listener.frames, listener.tone);

    // "Mute microphone": capture/publication stop before the command returns.
    command(Command::SetMuted { muted: true });
    assert!(evidence.stops.load(Ordering::SeqCst) >= 1);
    let sent_at_mute = evidence.sent.load(Ordering::SeqCst);
    // The stop took the send lock, so the capture loop has sent its last
    // packet: the listener can hear nothing beyond what was sent before it.
    tick();
    listener.drain();
    assert!(!command(Command::Status).capture_active);
    assert_eq!(
        evidence.sent.load(Ordering::SeqCst),
        sent_at_mute,
        "no capture after mute"
    );
    assert!(
        u64::try_from(listener.frames).unwrap() <= sent_at_mute,
        "no audio after mute: heard {} frames of {sent_at_mute} sent",
        listener.frames
    );

    // "Leave call": the backend and its transport are closed.
    command(Command::Leave);
    tick();
    assert!(!command(Command::Status).capture_active);
    settle("leave to close the native backend", || {
        evidence.closes.load(Ordering::SeqCst) > 0
    })
    .await;
    assert!(
        evidence.closes.load(Ordering::SeqCst) >= 1,
        "leave closes the native backend"
    );
    println!(
        "SEMANTIC_CALL_AUDIO_REPORT {}",
        serde_json::json!({
            "evidenceClass": "synthetic capture source; real consent prompt queue, capture lease broker and service lifecycle; real libopus and native moq-lite client/relay on loopback",
            "joinedMutedFramesReceived": 0,
            "declinedConsentFramesReceived": 0,
            "afterConsent": {"framesReceived": audible.0, "toneFrames": audible.1, "firstToneMsAfterConsent": first_audio_ms, "packetsSent": sent_at_mute},
            "framesHeardAfterMute": listener.frames - audible.0,
            "captureStarts": evidence.starts.load(Ordering::SeqCst),
            "captureStops": evidence.stops.load(Ordering::SeqCst),
            "backendCloses": evidence.closes.load(Ordering::SeqCst),
            "relay": format!("{:?}", relay.counters()),
        })
    );
}
