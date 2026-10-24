//! Device-free native voice pipeline proof. SYNTHETIC capture/playback: a
//! generated tone-burst signal replaces the microphone and the decoded PCM is
//! analysed instead of being played (no `PipeWire` device on this host). Real
//! libopus encode/decode through the shell's `GStreamer` codec, and the real
//! native `moq-lite` client over real loopback WebTransport through the
//! host relay implementation; the relay authority here is a test identity map.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "tone synthesis, Goertzel analysis and frame counters over bounded test sample buffers"
)]
use media::voice::gst::{OpusDecoder, OpusEncoder};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tilo_moq::client::{
    CapturedOpus, Client, ClientAuthority, Connection, ConnectionScope, Status,
};
use tilo_moq::relay::{Denied, Direction, LoopbackRelay, RelayAuthority, SessionAuthorization};

use super::call_seal::{Sealing, open_audio, seal_audio};
use channel_crypto::ChannelContentKeys;
use collab_types::ChannelId;

const RATE: f64 = 48_000.0;
const FRAME: usize = 960;
/// A loop turn this late let a received frame wait past the client's
/// 200 ms receive budget, with margin for when in the turn it arrived.
const LOOP_STALL: Duration = Duration::from_millis(100);
/// How long before a stall a frame may have been sent and still be waiting.
const EXPOSURE_MARGIN: Duration = Duration::from_millis(200);
const CALL: &str = "c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0";

fn namespace(publisher: char) -> String {
    format!("{CALL}/{}", publisher.to_string().repeat(64))
}

struct Scopes {
    publish: String,
    subscribe: Vec<String>,
}
impl SessionAuthorization for Scopes {
    fn publication(&self) -> Option<&str> {
        Some(&self.publish)
    }
    fn subscriptions(&self) -> &[String] {
        &self.subscribe
    }
    fn current(&self, direction: Direction, name: &str) -> bool {
        match direction {
            Direction::Publish => name == self.publish,
            Direction::Subscribe => self.subscribe.iter().any(|value| value == name),
        }
    }
}
struct TestIdentities;
impl RelayAuthority for TestIdentities {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let (own, other) = match proof {
            b"native-proof-a" => ('a', 'b'),
            b"native-proof-b" => ('b', 'a'),
            _ => return Err(Denied),
        };
        Ok(Arc::new(Scopes {
            publish: namespace(own),
            subscribe: vec![namespace(other)],
        }))
    }
}
/// Exactly this scope, hearing exactly the one other participant.
struct ExactScope(ConnectionScope, String);
impl ClientAuthority for ExactScope {
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

/// 200 ms bursts once per second at `frequency`, i16 at 48 kHz.
fn bursts(frequency: f64, offset_ms: usize, seconds: usize) -> Vec<i16> {
    (0..48_000 * seconds)
        .map(|index| {
            let position = (index / 48 + 1000 - offset_ms % 1000) % 1000;
            if position < 200 {
                ((index as f64 / RATE * frequency * std::f64::consts::TAU).sin() * 16_000.0) as i16
            } else {
                0
            }
        })
        .collect()
}

/// Whether frame `sequence` of a burst track offset by `offset_ms` lies
/// wholly inside a 200 ms burst.
fn is_tone_frame(sequence: usize, offset_ms: usize) -> bool {
    let start = (sequence * 20 + 1000 - offset_ms % 1000) % 1000;
    start + 20 <= 200
}

/// Share of block energy at `frequency` (1.0 for a pure matching tone).
fn tone_share(samples: &[i16], frequency: f64) -> f64 {
    let c = 2.0 * (std::f64::consts::TAU * frequency / RATE).cos();
    let (mut s1, mut s2, mut energy) = (0.0f64, 0.0f64, 0.0f64);
    for &sample in samples {
        let x = f64::from(sample) / 32768.0;
        energy += x * x;
        let s0 = x + c * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    if energy < 1e-4 {
        return 0.0;
    }
    (2.0 * (s1 * s1 + s2 * s2 - c * s1 * s2)) / (samples.len() as f64 * energy)
}

struct Side {
    client: Client,
    frequency: f64,
    offset_ms: usize,
    packets: Vec<Vec<u8>>,
    sealing: Sealing,
}

fn keys_of(channel: ChannelId, key: u8) -> ChannelContentKeys {
    let wire = (1_u8, channel, 1_u64, vec![(1_u64, [key; 32])]);
    ChannelContentKeys::from_bytes(&postcard::to_stdvec(&wire).expect("keys encode"))
        .expect("keys decode")
}
fn start(relay: &LoopbackRelay, own: char, other: char, proof: &[u8]) -> Client {
    let scope = ConnectionScope {
        endpoint: relay.endpoint.clone(),
        fingerprints: relay.fingerprints.clone(),
        owner_epoch: [7; 32],
        carrier_generation: 1,
        publication: Some(namespace(own)),
        subscription_prefix: Some(CALL.to_owned()),
    };
    Client::start(
        Connection {
            scope: scope.clone(),
            authorization: proof.to_vec(),
        },
        Arc::new(ExactScope(scope, namespace(other))),
    )
    .expect("native media client")
}

fn percentile(values: &mut [f64], q: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * q).round() as usize]
}

#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end audio scenario over live loopback endpoints; each phase asserts on \
              state the previous phase left behind"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_opus_moq_voice_roundtrip_both_directions_with_tone_and_latency() {
    let mut encoder = OpusEncoder::new().expect("native voice proof requires GStreamer opusenc");
    let seconds = 5;
    let relay = LoopbackRelay::start(Arc::new(TestIdentities)).expect("loopback relay");
    let channel = collab_types::document_channel_id("so.tilo.chat", "room", "general");
    let keys = keys_of(channel, 7);
    let stranger = keys_of(channel, 9);
    let mut sides = Vec::new();
    for (own, other, proof, frequency, offset) in [
        ('a', 'b', &b"native-proof-a"[..], 700.0, 0),
        ('b', 'a', &b"native-proof-b"[..], 1100.0, 500),
    ] {
        let pcm = bursts(frequency, offset, seconds);
        let encode_started = Instant::now();
        let mut packets = Vec::new();
        for chunk in pcm.chunks(48_000) {
            packets.extend(encoder.encode(chunk));
        }
        eprintln!(
            "encoded {} Opus packets for {frequency} Hz in {:?}",
            packets.len(),
            encode_started.elapsed()
        );
        packets.truncate(seconds * 50);
        sides.push(Side {
            client: start(&relay, own, other, proof),
            frequency,
            offset_ms: offset,
            packets,
            sealing: Sealing {
                keys: keys.clone(),
                publication: namespace(own),
            },
        });
    }
    for side in &sides {
        tilo_test_env::within("native client connected", async {
            while side.client.status() == Status::Connecting {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert_eq!(side.client.status(), Status::Live);
    }
    // The relay counts a session once its SETUP completed after admission.
    tilo_test_env::within("both native clients admitted", async {
        while relay.counters().accepted < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    for side in &sides {
        side.client.enable_publication().unwrap();
    }
    let frames = sides[0].packets.len().min(sides[1].packets.len());
    let mut sent_at = vec![vec![None; frames]; 2];
    let side_packets: Vec<Vec<Vec<u8>>> = sides.iter().map(|side| side.packets.clone()).collect();
    let mut decoders = [
        OpusDecoder::new().expect("opusdec"),
        OpusDecoder::new().expect("opusdec"),
    ];
    let mut received = [Vec::new(), Vec::new()];
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    let bound = Instant::now() + tilo_test_env::hang_bound();
    // Once every frame is sent, a second for the last of them to arrive.
    let mut drain_until = None;
    // When this loop itself did not run for longer than a received frame
    // waits for its reader: frames arriving then expired unread, as a call's
    // audio does when its player cannot run. That is the scheduler, not the
    // transport this test measures.
    let mut stalls: Vec<(Instant, Instant)> = Vec::new();
    let mut last_turn = Instant::now();
    let mut next = 0;
    while drain_until.is_none_or(|until| Instant::now() < until) && Instant::now() < bound {
        ticker.tick().await;
        let turn = Instant::now();
        if turn.duration_since(last_turn) > LOOP_STALL {
            stalls.push((last_turn, turn));
        }
        last_turn = turn;
        if next < frames {
            for (index, side) in sides.iter().enumerate() {
                let now = Instant::now();
                let header = ((next * FRAME) as u64, FRAME as u16);
                side.client
                    .try_send_sealed(
                        CapturedOpus {
                            timestamp_48k: header.0,
                            duration_samples: header.1,
                            payload: side.packets[next].clone(),
                            captured_at: now,
                        },
                        |sequence, payload| seal_audio(&side.sealing, sequence, header, &payload),
                    )
                    .expect("native publication");
                sent_at[index][next] = Some(now);
            }
            next += 1;
            if next == frames {
                drain_until = Some(Instant::now() + Duration::from_secs(1));
            }
        }
        for (receiver, side) in sides.iter().enumerate() {
            while let Some(frame) = side.client.try_recv() {
                let sender = 1 - receiver;
                // Correlate by the captured media timestamp (the transport
                // assigns its own wire sequence numbers).
                let sequence = (frame.object.timestamp / FRAME as u64) as usize;
                let Some(Some(at)) = sent_at[sender].get(sequence) else {
                    continue;
                };
                let latency = if frame.received_at >= *at {
                    frame.received_at.duration_since(*at).as_secs_f64() * 1000.0
                } else {
                    -(at.duration_since(frame.received_at).as_secs_f64() * 1000.0)
                };
                let publication = &frame.publisher.namespace;
                assert!(
                    open_audio(&stranger, channel, publication, &frame.object).is_none(),
                    "other keys open nothing"
                );
                assert_ne!(frame.object.payload, side_packets[sender][sequence]);
                let opus = open_audio(&keys, channel, publication, &frame.object)
                    .expect("the call's keys open each frame");
                let pcm = decoders[receiver].decode(&opus);
                received[receiver].push((sequence, latency, pcm));
            }
        }
        if next >= frames && received.iter().all(|rows| rows.len() >= frames) {
            break;
        }
    }
    let mut report = serde_json::Map::new();
    for (receiver, rows) in received.iter().enumerate() {
        let sender = 1 - receiver;
        let own = sides[receiver].frequency;
        let expected = sides[sender].frequency;
        let mut latencies: Vec<f64> = rows.iter().map(|row| row.1).collect();
        // Frames sent while this loop was stalled may have expired unread.
        let exposed: Vec<usize> = (0..frames)
            .filter(|n| {
                sent_at[sender][*n].is_some_and(|at| {
                    stalls
                        .iter()
                        .any(|(from, to)| at + EXPOSURE_MARGIN >= *from && at <= *to)
                })
            })
            .collect();
        let delivered_unexposed = rows.iter().filter(|row| !exposed.contains(&row.0)).count();
        let unexposed = frames - exposed.len();
        let offset = sides[sender].offset_ms;
        let tone_expected = rows
            .iter()
            .filter(|row| is_tone_frame(row.0, offset))
            .count();
        let decoded_samples: usize = rows.iter().map(|row| row.2.len()).sum();
        let tone_frames = rows
            .iter()
            .filter(|row| row.2.len() == FRAME && tone_share(&row.2, expected) > 0.5)
            .count();
        let own_frames = rows
            .iter()
            .filter(|row| row.2.len() == FRAME && tone_share(&row.2, own) > 0.5)
            .count();
        // Bursts are 200 ms of every second: 10 of every 50 frames carry tone.
        let expected_tone_frames = frames / 5;
        let summary = serde_json::json!({
            "direction": format!("{}->{}", ['A', 'B'][sender], ['A', 'B'][receiver]),
            "frequencyHz": expected,
            "sentPackets": frames,
            "receivedPackets": rows.len(),
            "decodedSamples": decoded_samples,
            "toneFrames": tone_frames,
            "expectedToneFrames": expected_tone_frames,
            "ownToneFrames": own_frames,
            "senderCounters": format!("{:?}", sides[sender].client.counters()),
            "receiverCounters": format!("{:?}", sides[receiver].client.counters()),
            "deliveredTonePositions": tone_expected,
            "loopStalls": stalls.iter().map(|(from, to)| to.duration_since(*from).as_millis()).collect::<Vec<_>>(),
            "exposedFrames": exposed.len(),
            "missing": (0..frames).filter(|n| !rows.iter().any(|row| row.0 == *n)).collect::<Vec<_>>(),
            "transportLatencyMs": if latencies.is_empty() { serde_json::Value::Null } else { serde_json::json!({
                "p50": percentile(&mut latencies, 0.5), "p90": percentile(&mut latencies, 0.9), "max": percentile(&mut latencies, 1.0)}) },
        });
        eprintln!("{summary}");
        // The transport may drop a frame only by counting it: late to its
        // budget, over capacity or a duplicate. Anything else vanished.
        let counters = sides[receiver].client.counters();
        let accounted = counters.received
            + counters.late_dropped
            + counters.capacity_dropped
            + counters.duplicate_dropped;
        assert!(
            accounted >= frames as u64,
            "{} of {frames} frames vanished without a drop reason: {counters:?}",
            frames as u64 - accounted
        );
        // Of the frames sent while the reader could run, the transport
        // delivers nearly all; those it could not read were counted above.
        assert!(
            delivered_unexposed * 100 >= unexposed * 95,
            "delivered {delivered_unexposed} of {unexposed} frames sent while the reader ran \
             ({} more sent during loop stalls {stalls:?}): {counters:?}",
            exposed.len()
        );
        assert!(
            tone_frames * 10 >= tone_expected * 8,
            "tone frames {tone_frames} of {tone_expected} delivered at tone positions"
        );
        assert_eq!(own_frames, 0, "a receiver never hears its own publication");
        assert!(
            latencies.iter().all(|value| *value >= 0.0),
            "per-frame arrival follows publication"
        );
        report.insert(summary["direction"].as_str().unwrap().into(), summary);
    }
    report.insert(
        "relayCounters".into(),
        serde_json::json!(format!("{:?}", relay.counters())),
    );
    report.insert("evidenceClass".into(), serde_json::json!("synthetic tone, device-free; real libopus via GStreamer; real native moq-lite WebTransport client and relay on loopback; test relay identities"));
    println!("NATIVE_AUDIO_REPORT {}", serde_json::Value::Object(report));
    for side in &sides {
        side.client.close();
    }
}
