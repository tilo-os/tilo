//! Two devices of one call, in one process, with no workspace host, relay or
//! session between them: a tone one device's microphone sends reaches the
//! other device's ear over the call's lane, a device the workspace never
//! certified gets no lane at all, and audio on a lossy path still plays
//! because nothing behind a lost datagram waits for it.
//!
//! The tone is real Opus through libopus, as a call's microphone sends it; the
//! playout is the call's own jitter buffer; what is analysed instead of being
//! played is the decoded PCM, by Goertzel's single-bin DFT against the block's
//! energy.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "in a test these are the assertion; the arithmetic and casts are frame counters over \
              bounded test sample buffers"
)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::ChannelId;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use opus::{Application, Channels, Decoder, Encoder};
use tilo_call_lane::{
    ALPN, Calls, Failed, Hello, Lane, Microphone, Opener, Peer, Refusal, Roster, heard,
};
use tilo_media::voice::{self, JitterBuffer, PublicationId, ReceivedOpus};
use tokio::sync::mpsc;

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const CALL: &str = "voice";
/// 20 ms of audio at 48 kHz.
const FRAME_TIME: Duration = Duration::from_millis(20);
const RATE: f32 = 48_000.0;
const FRAME_SAMPLES: usize = 960;
const AMPLITUDE: f32 = 0.3;
const MAX_PACKET: usize = 1275;

// ---------------------------------------------------------------------------
// The call's keys, as the channel's members hold them.
// ---------------------------------------------------------------------------

/// One channel Alice owns and Bob joined, with each member's own copy of its
/// content keys: the keys a call in it seals its frames with.
fn call_keys() -> (ChannelId, ChannelContentKeys, ChannelContentKeys) {
    let channel = ChannelId::new();
    let mut alice = ChannelGroup::create(channel, MemberKey::generate("alice").unwrap()).unwrap();
    let bob_key = MemberKey::generate("bob").unwrap();
    let addition = alice.admit(bob_key.fresh_key_package().unwrap()).unwrap();
    let bob = ChannelGroup::process_welcome_with_history(
        channel,
        &addition.welcome,
        &addition.key_history,
        bob_key,
    )
    .unwrap();
    (
        channel,
        alice.content_keys().unwrap(),
        bob.content_keys().unwrap(),
    )
}

// ---------------------------------------------------------------------------
// The devices of a call.
// ---------------------------------------------------------------------------

/// One device in a call: its endpoint, the lanes it accepts, and the lanes
/// that arrive on it.
struct Side {
    endpoint: Endpoint,
    /// The device's public key: who its frames say they are from.
    id: [u8; 32],
    calls: Calls,
    arrivals: mpsc::UnboundedReceiver<Peer>,
    /// Dropped with the side: dropping it stops the endpoint accepting lanes.
    _router: Router,
}

/// A localhost endpoint whose identity is the device key `secret`, accepting
/// the call `channel` names.
async fn side(secret: [u8; 32], channel: ChannelId) -> Side {
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(SecretKey::from_bytes(&secret))
        .bind_addr((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .transport_config(tilo_call_lane::carrier::transport_config())
        .bind()
        .await
        .unwrap();
    let id = *endpoint.id().as_bytes();
    let (calls, arrivals) = Calls::new(
        Hello {
            workspace: WORKSPACE.to_owned(),
            channel,
            call: CALL.to_owned(),
            lane: Lane::Audio,
        },
        Roster::default(),
    );
    let router = Router::builder(endpoint.clone())
        .accept(ALPN, calls.clone())
        .spawn();
    Side {
        endpoint,
        id,
        calls,
        arrivals,
        _router: router,
    }
}

impl Side {
    /// Where the other device reaches this one.
    fn addr(&self) -> EndpointAddr {
        EndpointAddr::new(self.endpoint.id()).with_ip_addr(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            self.endpoint.bound_sockets().first().unwrap().port(),
        )))
    }
}

/// Both devices certify the other, as a workspace that has admitted both.
fn trust(a: &Side, b: &Side) {
    let roster = Roster::new([a.id, b.id]);
    a.calls.set_roster(roster.clone());
    b.calls.set_roster(roster);
}

/// B dials A and both ends of the one lane between them result, the first as
/// B holds it and the second as A does.
async fn lane(b: &Side, a: &mut Side) -> (Peer, Peer) {
    let (dialled, accepted) = tokio::join!(
        b.calls.dial(&b.endpoint, a.addr()),
        tilo_test_env::hang::within("the other device takes the lane", a.arrivals.recv()),
    );
    let from_alice = dialled.expect("a device the workspace admitted reaches the call");
    let from_bob = accepted.expect("A takes B's lane");
    assert_eq!(from_alice.remote(), a.id);
    assert_eq!(from_bob.remote(), b.id);
    (from_alice, from_bob)
}

// ---------------------------------------------------------------------------
// A tone, and what the other device hears of it.
// ---------------------------------------------------------------------------

/// `frames` 20 ms Opus frames (48 kHz mono) of a sine at `hz`.
fn tone(hz: f32, frames: usize) -> Vec<Vec<u8>> {
    let mut encoder = Encoder::new(48_000, Channels::Mono, Application::Audio).unwrap();
    let step = hz / RATE * std::f32::consts::TAU;
    let mut phase = 0.0_f32;
    let mut pcm = [0.0_f32; FRAME_SAMPLES];
    (0..frames)
        .map(|_| {
            for sample in &mut pcm {
                *sample = phase.sin() * AMPLITUDE;
                phase = (phase + step) % std::f32::consts::TAU;
            }
            encoder.encode_vec_float(&pcm, MAX_PACKET).unwrap()
        })
        .collect()
}

/// The samples one played frame decodes to; a frame the path lost decodes as
/// the codec's own concealment of it.
fn decode(decoder: &mut Decoder, opus: Option<&[u8]>) -> Vec<f32> {
    let mut samples = vec![0.0_f32; FRAME_SAMPLES];
    let count = decoder
        .decode_float(opus.unwrap_or(&[]), &mut samples, false)
        .unwrap();
    samples.truncate(count);
    samples
}

/// How much of the block's energy is a tone at `hz`: about 1 for that tone
/// alone, near 0 for a tone that is not there (Goertzel's single-bin DFT,
/// against the total energy).
fn share(samples: &[f32], hz: f32) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let coefficient = 2.0 * (std::f32::consts::TAU * hz / RATE).cos();
    let (mut previous, mut before) = (0.0_f32, 0.0_f32);
    for &sample in samples {
        let current = sample + coefficient * previous - before;
        before = previous;
        previous = current;
    }
    let power = previous * previous + before * before - coefficient * previous * before;
    let energy: f32 = samples.iter().map(|sample| sample * sample).sum();
    if energy <= f32::EPSILON {
        return 0.0;
    }
    power / (samples.len() as f32 / 2.0 * energy)
}

// ---------------------------------------------------------------------------
// One device's ear.
// ---------------------------------------------------------------------------

/// A frame played, with what it decoded to.
struct Played {
    sequence: u64,
    at: Instant,
    /// `false` when the path lost the frame and the codec concealed it.
    received: bool,
    samples: Vec<f32>,
}

/// One device's ear: the frames the other device's microphone sends, opened,
/// jitter-buffered and decoded.
struct Ear {
    opener: Opener,
    sender: [u8; 32],
    jitter: JitterBuffer,
    decoder: Decoder,
    publisher: PublicationId,
    /// The sequences that crossed the path, however many times each came.
    opened: BTreeSet<u64>,
    played: Vec<Played>,
    datagrams: u64,
    dropped: u64,
}

impl Ear {
    fn new(keys: ChannelContentKeys, sender: [u8; 32], roster: Roster) -> Self {
        Self {
            opener: Opener::new(keys, roster),
            sender,
            jitter: JitterBuffer::default(),
            decoder: Decoder::new(48_000, Channels::Mono).unwrap(),
            publisher: PublicationId {
                namespace: sender.iter().fold(String::new(), |mut hex, byte| {
                    let _ = write!(hex, "{byte:02x}");
                    hex
                }),
                carrier_generation: 1,
                incarnation: 1,
            },
            opened: BTreeSet::new(),
            played: Vec::new(),
            datagrams: 0,
            dropped: 0,
        }
    }

    /// Take one datagram off the lane.
    fn hear(&mut self, datagram: &[u8]) {
        self.datagrams += 1;
        let now = Instant::now();
        for frame in heard(&self.opener, self.sender, datagram).expect("the call's frames open") {
            self.opened.insert(frame.sequence);
            self.jitter
                .push(
                    ReceivedOpus {
                        publisher: self.publisher.clone(),
                        sequence: frame.sequence,
                        timestamp_48k: frame.timestamp_48k,
                        duration_samples: voice::FRAME_SAMPLES as u16,
                        payload: frame.opus,
                        received_at: now,
                        expires_at: now + voice::RECEIVE_LIFETIME,
                    },
                    now,
                )
                .expect("the frame is playable");
        }
    }

    /// Play whatever the buffer says is due.
    fn play(&mut self, now: Instant) {
        for frame in self.jitter.poll(now) {
            let received = frame.payload.is_some();
            let samples = decode(&mut self.decoder, frame.payload.as_deref());
            self.played.push(Played {
                sequence: frame.sequence,
                at: now,
                received,
                samples,
            });
        }
    }

    /// The frames that made it across the path, in the order they played.
    fn heard_frames(&self) -> impl Iterator<Item = &Played> {
        self.played.iter().filter(|played| played.received)
    }
}

/// The newest sequence the playout produced from the path, or 0 for none.
fn newest(ear: &Ear) -> u64 {
    ear.heard_frames()
        .map(|played| played.sequence)
        .max()
        .unwrap_or(0)
}

/// How many of `played` played within the call's voice budget of being sent:
/// what a call that never waits behind a lost frame shows, since the buffer
/// plays a frame while it is still current or not at all.
fn prompt(played: &[&Played], sent: &Sent) -> usize {
    played
        .iter()
        .filter(|played| {
            played
                .at
                .saturating_duration_since(sent.at[played.sequence as usize])
                < voice::RECEIVE_LIFETIME
        })
        .count()
}

/// What one device's microphone handed to the lane: when each frame was sent,
/// and how many datagrams carried them.
struct Sent {
    at: Vec<Instant>,
    datagrams: u64,
}

// ---------------------------------------------------------------------------
// Sending and listening.
// ---------------------------------------------------------------------------

/// Send `frames` at the rate a microphone captures them.
///
/// A capture device hands over the frames that accumulated while its thread
/// was away rather than spacing them out afterwards, so a late tick sends the
/// frames it owes at once: the media timeline keeps up with the wall clock,
/// which is what the playout on the other side measures itself against.
async fn speak(peer: &Peer, microphone: &mut Microphone, frames: &[Vec<u8>]) -> Sent {
    let mut ticker = tokio::time::interval(FRAME_TIME);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    let mut sent = Sent {
        at: Vec::with_capacity(frames.len()),
        datagrams: 0,
    };
    for (index, frame) in frames.iter().enumerate() {
        ticker.tick().await;
        let timestamp = (index as u64) * voice::FRAME_SAMPLES as u64;
        let datagram = microphone
            .frame(timestamp, frame)
            .expect("the frame seals under the call's key");
        sent.at.push(Instant::now());
        // A frame too large for a call's datagram is dropped, not delayed.
        if let Some(datagram) = datagram {
            peer.send(datagram).expect("the lane carries the frame");
            sent.datagrams += 1;
        }
    }
    sent
}

/// Listen on `peer` until the lane has been quiet for longer than the playout
/// window, letting `path` say whether each datagram reaches the device at all,
/// and play what does.
async fn listen(peer: &Peer, ear: &mut Ear, path: impl Fn(u64) -> bool) {
    let mut ticker = tokio::time::interval(Duration::from_millis(5));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last = None;
    let mut carried = 0_u64;
    loop {
        tokio::select! {
            datagram = peer.received() => {
                let datagram = datagram.expect("the lane stays up");
                carried += 1;
                last = Some(Instant::now());
                if path(carried) {
                    ear.hear(&datagram);
                } else {
                    ear.dropped += 1;
                }
            }
            _ = ticker.tick() => {
                ear.play(Instant::now());
                let quiet = last.is_some_and(|at: Instant| at.elapsed() > voice::RECEIVE_LIFETIME);
                if quiet || Instant::now() > tilo_test_env::hang::hang_deadline() {
                    break;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The tests.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tone_one_device_sends_reaches_the_other() {
    let (channel, alice_keys, bob_keys) = call_keys();
    let mut alice = side([1; 32], channel).await;
    let bob = side([2; 32], channel).await;
    trust(&alice, &bob);
    let (from_alice, from_bob) = lane(&bob, &mut alice).await;

    let frames = tone(440.0, 60);
    let mut ear = Ear::new(bob_keys, alice.id, Roster::new([alice.id]));
    // A's microphone on its own task, as a call's capture runs on a thread of
    // its own: B's playout never waits on it.
    let sending = tokio::spawn({
        let peer = from_alice.clone();
        let frames = frames.clone();
        let keys = alice_keys.clone();
        let sender = alice.id;
        async move {
            let mut microphone = Microphone::new(&keys, sender, 0).unwrap();
            speak(&peer, &mut microphone, &frames).await
        }
    });
    listen(&from_bob, &mut ear, |_| true).await;
    let sent = sending.await.expect("the microphone finished");

    assert_eq!(
        (ear.datagrams, ear.dropped),
        (sent.datagrams, 0),
        "every datagram of the tone arrived"
    );
    assert_eq!(
        ear.opened.len(),
        frames.len(),
        "B opened every frame A sent"
    );
    // A stalled playout thread drops what it could not play in its 200 ms
    // window (the production buffer's own rule); what it does play is the
    // newest audio, so it keeps up with A rather than draining a backlog.
    let played = ear.heard_frames().count();
    assert!(
        played * 100 >= frames.len() * 50,
        "B's playout kept producing A's voice: {played} of {} frames ({:?})",
        frames.len(),
        ear.jitter.stats()
    );
    assert!(
        newest(&ear) + 3 >= frames.len() as u64,
        "B played A's newest frames, not a backlog: up to {} of {} ({:?})",
        newest(&ear),
        frames.len(),
        ear.jitter.stats()
    );
    // The first frames carry the encoder's warm-up; the rest are the tone.
    let steady: Vec<&Played> = ear
        .played
        .iter()
        .filter(|played| played.sequence < frames.len() as u64)
        .skip(5)
        .collect();
    let shares: Vec<f32> = steady
        .iter()
        .map(|played| share(&played.samples, 440.0))
        .collect();
    let heard = shares.iter().filter(|share| **share > 0.5).count();
    assert!(
        heard * 100 >= steady.len() * 90,
        "B heard A's 440 Hz tone in {heard} of {} played frames",
        steady.len()
    );
    let mean = shares.iter().sum::<f32>() / shares.len() as f32;
    assert!(mean > 0.7, "the tone holds the blocks' energy: {mean}");
    let other = steady
        .iter()
        .map(|played| share(&played.samples, 660.0))
        .fold(0.0_f32, f32::max);
    assert!(other < 0.1, "B hears no 660 Hz tone: {other}");
    // A's frames reached B's ear as they were sent: nothing waited.
    let on_time = prompt(&steady, &sent);
    assert!(
        on_time * 100 >= steady.len() * 95,
        "B heard A live: {on_time} of {} played frames played within the voice budget of being \
         sent",
        steady.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_off_the_workspace_roster_gets_no_lane() {
    let (channel, alice_keys, _) = call_keys();
    let mut alice = side([1; 32], channel).await;
    let bob = side([2; 32], channel).await;
    let carol = side([3; 32], channel).await;
    // Alice's workspace has admitted Alice and Bob. Carol is a device of the
    // same channel, holding the same keys, that it never admitted.
    trust(&alice, &bob);
    carol.calls.set_roster(Roster::new([carol.id, alice.id]));

    let refused = carol.calls.dial(&carol.endpoint, alice.addr()).await;
    assert!(
        matches!(&refused, Err(Failed::Refused(reason)) if reason.contains("roster")),
        "alice refuses a device the workspace never admitted: {refused:?}"
    );
    assert!(
        alice.arrivals.try_recv().is_err(),
        "alice's call has no lane for the refused device"
    );
    // Nor does a frame claiming to be from Carol open anything, however well
    // it is sealed: the roster is checked before anything is decrypted.
    let mut microphone = Microphone::new(&alice_keys, alice.id, 0).unwrap();
    let datagram = microphone.frame(0, b"an opus frame").unwrap().unwrap();
    assert_eq!(
        heard(
            &Opener::new(alice_keys.clone(), Roster::new([bob.id])),
            carol.id,
            &datagram
        ),
        Err(Refusal::OffRoster)
    );

    // Once the workspace admits Carol, the same dial opens Alice's lane.
    alice
        .calls
        .set_roster(Roster::new([alice.id, bob.id, carol.id]));
    let (from_carol, from_alice) = lane(&carol, &mut alice).await;
    assert_eq!(from_carol.remote(), alice.id);
    assert_eq!(from_alice.remote(), carol.id);
    assert_eq!(from_alice.lane(), Lane::Audio);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn audio_plays_through_a_lossy_path_without_waiting_behind_it() {
    let (channel, alice_keys, bob_keys) = call_keys();
    let mut alice = side([1; 32], channel).await;
    let bob = side([2; 32], channel).await;
    trust(&alice, &bob);
    let (from_alice, from_bob) = lane(&bob, &mut alice).await;

    let frames = tone(440.0, 100);
    let mut ear = Ear::new(bob_keys, alice.id, Roster::new([alice.id]));
    let sending = tokio::spawn({
        let peer = from_alice.clone();
        let frames = frames.clone();
        let keys = alice_keys.clone();
        let sender = alice.id;
        async move {
            // Every datagram repeats the two frames before its newest one:
            // what a sender on a path it has measured loss on sends.
            let mut microphone = Microphone::new(&keys, sender, 2).unwrap();
            speak(&peer, &mut microphone, &frames).await
        }
    });
    // The path loses every fourth datagram.
    listen(&from_bob, &mut ear, |carried| carried % 4 != 3).await;
    let sent = sending.await.expect("the microphone finished");

    assert!(
        ear.dropped >= 20,
        "the path really lost datagrams: {} of {}",
        ear.dropped,
        sent.datagrams
    );
    // The repeats rode along: the frames the lost datagrams carried reached
    // the device anyway, every one of them.
    assert_eq!(
        ear.opened.len(),
        frames.len(),
        "B opened every frame A sent, though the path lost {} datagrams",
        ear.dropped
    );
    assert!(
        ear.jitter.stats().duplicates >= 20,
        "the repeats arrived and were folded in, not played again: {:?}",
        ear.jitter.stats()
    );
    // A stalled playout thread drops what it could not play in its 200 ms
    // window (the production buffer's own rule); what it does play is the
    // newest audio, so the loss never turned into a backlog.
    let played = ear.heard_frames().count();
    assert!(
        played * 100 >= frames.len() * 50,
        "B's playout kept producing A's voice: {played} of {} frames ({:?})",
        frames.len(),
        ear.jitter.stats()
    );
    assert!(
        newest(&ear) + 3 >= frames.len() as u64,
        "B played A's newest frames, not a backlog: up to {} of {} ({:?})",
        newest(&ear),
        frames.len(),
        ear.jitter.stats()
    );
    // Only the frames A really sent say anything about the tone.
    let steady: Vec<&Played> = ear
        .played
        .iter()
        .filter(|played| played.sequence < frames.len() as u64)
        .skip(5)
        .collect();
    let heard = steady
        .iter()
        .filter(|played| share(&played.samples, 440.0) > 0.5)
        .count();
    assert!(
        heard * 100 >= steady.len() * 90,
        "B kept hearing the tone through the loss: {heard} of {} played frames",
        steady.len()
    );
    // Nothing waited behind a lost frame: a frame plays as soon as its media
    // time is due, so its playout follows its send closely, whether or not a
    // datagram in between was lost and a later one repaired it.
    let on_time = prompt(&steady, &sent);
    assert!(
        on_time * 100 >= steady.len() * 95,
        "the loss cost no waiting: {on_time} of {} frames played within the voice budget of \
         being sent",
        steady.len()
    );
}
