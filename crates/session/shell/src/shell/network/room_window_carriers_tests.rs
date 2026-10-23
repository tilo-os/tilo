#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion; fixture arithmetic is small and bounded"
)]

use super::{Inbound, Outbound, carrier_capabilities, carrier_peer, windows_channel};
use crate::collab::flatpak_share::SharedFlatpakSession;
use crate::config::{EncoderChoice, FlatpakShareConfig};
use crate::network::protocol::NeighbourMsg;
use crate::shell::state::RoomId;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tilo_moq::client::{Client, ClientAuthority, Connection, ConnectionScope, Status};
use tilo_moq::relay::{Denied, Direction, LoopbackRelay, RelayAuthority, SessionAuthorization};

const TAB: u64 = 0x5EED_0000_0000_0007;
const SURFACE: u64 = 31;
const APP: &str = "xterm";
const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const FRAMES: u32 = 90;
const DAMAGE_INTERVAL: Duration = Duration::from_micros(16_667);
/// The host's device key.
const HOST: [u8; 32] = [0xA1; 32];

fn call() -> String {
    "c".repeat(64)
}

fn name(byte: char) -> String {
    format!("{}/{}", call(), byte.to_string().repeat(64))
}

struct Permit {
    publish: String,
    subscribe: Vec<String>,
}
impl SessionAuthorization for Permit {
    fn publication(&self) -> Option<&str> {
        Some(&self.publish)
    }
    fn subscriptions(&self) -> &[String] {
        &self.subscribe
    }
    fn current(&self, _: Direction, _: &str) -> bool {
        true
    }
}

/// The governor's grant: each member device publishes its own windows and
/// reads every other's.
struct Members;
impl RelayAuthority for Members {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let own = match proof {
            b"host" => 'a',
            b"viewer" => 'b',
            b"late" => 'd',
            _ => return Err(Denied),
        };
        let subscribe = ['a', 'b', 'd']
            .into_iter()
            .filter(|other| *other != own)
            .map(name)
            .collect();
        Ok(Arc::new(Permit {
            publish: name(own),
            subscribe,
        }))
    }
}

struct Owner;
impl ClientAuthority for Owner {
    fn current(&self, _: &ConnectionScope, _: Direction, _: &str) -> bool {
        true
    }
}

fn join(relay: &LoopbackRelay, proof: &[u8], publication: char) -> Arc<Client> {
    Arc::new(
        Client::start(
            Connection {
                scope: ConnectionScope {
                    endpoint: relay.endpoint.clone(),
                    fingerprints: relay.fingerprints.clone(),
                    owner_epoch: [1; 32],
                    carrier_generation: 1,
                    publication: Some(name(publication)),
                    subscription_prefix: Some(call()),
                },
                authorization: proof.to_vec(),
            },
            Arc::new(Owner),
        )
        .unwrap(),
    )
}

/// Content keys of `channel` at epoch 1, as each member device holds them.
fn keys_of(channel: collab_types::ChannelId, key: u8) -> channel_crypto::ChannelContentKeys {
    let wire = (1_u8, channel, 1_u64, vec![(1_u64, [key; 32])]);
    channel_crypto::ChannelContentKeys::from_bytes(&postcard::to_stdvec(&wire).unwrap()).unwrap()
}

/// A terminal-like picture with its caret at a place `index` moves.
fn window_frame(index: u32) -> Vec<u8> {
    let caret = index.saturating_mul(7) % WIDTH;
    let mut rgba = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let line = (y / 24) % 2 == 0 && x % 97 < 80;
            let shade: u8 = if x.abs_diff(caret) < 3 {
                20
            } else if line {
                90
            } else {
                240
            };
            rgba.extend_from_slice(&[shade, shade, shade.saturating_add(10), 255]);
        }
    }
    rgba
}

async fn live(client: &Client, what: &str) {
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while client.status() != Status::Live {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what} never went live"));
}

fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    sorted[(sorted.len() - 1) * percent / 100]
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// One viewer's decoding of the stream its inbound bridge makes.
struct Viewing {
    inbound: Inbound,
    decoder: Option<(media::StreamCodec, media::RealtimeDecoder)>,
    started: Option<(u32, u32)>,
    decoded: u32,
    latencies: Vec<Duration>,
}

impl Viewing {
    fn new() -> Self {
        Self {
            inbound: Inbound::default(),
            decoder: None,
            started: None,
            decoded: 0,
            latencies: Vec::new(),
        }
    }

    /// Open and decode what `client` received; returns the payloads the
    /// relay forwarded, as it saw them.
    fn drain(
        &mut self,
        client: &Client,
        keys: &channel_crypto::ChannelContentKeys,
        room: RoomId,
        origin: Instant,
    ) -> Vec<Vec<u8>> {
        let mut forwarded = Vec::new();
        while let Some(picture) = client.try_recv_picture() {
            assert_eq!(picture.window, Some(TAB), "only the shown window arrives");
            forwarded.push(picture.object.payload.clone());
            for message in
                self.inbound
                    .receive((&HOST, 7), TAB, APP, keys, windows_channel(room), &picture)
            {
                match message {
                    NeighbourMsg::SurfaceStreamStart {
                        surface,
                        codec,
                        width,
                        height,
                        app_id,
                        tab,
                        source_peer,
                        ..
                    } => {
                        assert_eq!((surface, tab, source_peer), (TAB, TAB, 7));
                        assert_eq!(app_id, APP, "the stream names the tile's app");
                        let codec = media::StreamCodec::from_wire_label(&codec).unwrap();
                        let mut config = media::DecoderConfig::new(codec);
                        config.output = media::DecodeOutput::Rgba;
                        self.decoder = Some((codec, media::RealtimeDecoder::new(config).unwrap()));
                        self.started = Some((width, height));
                    }
                    NeighbourMsg::SurfaceFrame {
                        encoded_h264, ts, ..
                    } => {
                        let (codec, decoder) = self.decoder.as_mut().expect("started first");
                        let keyframe = media::realtime::is_keyframe(*codec, &encoded_h264);
                        let picture = decoder
                            .decode(&encoded_h264, Some(keyframe))
                            .expect("every opened picture decodes");
                        if picture.is_some() {
                            self.decoded += 1;
                            self.latencies
                                .push((origin + Duration::from_nanos(ts)).elapsed());
                        }
                    }
                    other => panic!("unexpected {}", other.kind()),
                }
            }
        }
        forwarded
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one scenario: show, stream, measure, a late reader, a still window"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_members_see_each_others_window_through_the_relay_alone() {
    let room = RoomId::new();
    let channel = windows_channel(room);
    let keys = keys_of(channel, 7);
    let relay = LoopbackRelay::start(Arc::new(Members)).unwrap();
    let host = join(&relay, b"host", 'a');
    let viewer = join(&relay, b"viewer", 'b');
    live(&host, "the host").await;
    live(&viewer, "the viewer").await;
    assert!(
        host.wanted_windows().is_empty(),
        "nobody shows the window yet, so nothing asks for it"
    );

    viewer.show_windows(&name('a'), &[TAB]);
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while host.wanted_windows() != [TAB] {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the viewer's showing asks the host for the window");

    // The host's share, encoding for its carrier as for any viewer.
    let session = SharedFlatpakSession::start_with_metrics(
        1,
        APP,
        SURFACE,
        WIDTH,
        HEIGHT,
        &FlatpakShareConfig {
            max_bitrate_kbps: 4000,
            target_fps: 60,
            encoder: EncoderChoice::Software,
            hdr: None,
        },
        &[carrier_capabilities()],
        None,
    )
    .expect("a software encoder for the window")
    .with_tab(TAB);
    session.add_viewer(carrier_peer(room), true);
    let origin = session.origin();
    let codec = session.chosen_codec().wire_label().to_owned();
    let mut outbound = Outbound::default();
    assert!(outbound.send(&host, &HOST, &keys, &session.stream_start_msg()));

    let pictures = (0..8).map(|i| window_frame(i * 23)).collect::<Vec<_>>();
    let mut viewing = Viewing::new();
    let mut plaintexts = Vec::new();
    let mut forwarded = Vec::new();
    let started = Instant::now();
    for index in 0..FRAMES {
        let due = started + DAMAGE_INTERVAL * index;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            tokio::time::sleep(wait).await;
        }
        session.deposit_captured_rgba(pictures[(index % 8) as usize].clone());
        if let Some(frame) = session.encode_next_frame(Instant::now()) {
            plaintexts.push(frame.nalus.clone());
            assert!(
                outbound.send(&host, &HOST, &keys, &frame.into_neighbour_msg()),
                "each frame goes onto the window's track"
            );
        }
        forwarded.extend(viewing.drain(&viewer, &keys, room, origin));
    }
    let sent = u32::try_from(plaintexts.len()).unwrap();
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while viewing.decoded < sent {
            forwarded.extend(viewing.drain(&viewer, &keys, room, origin));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("decoded {} of {sent} frames", viewing.decoded));
    assert_eq!(
        viewing.started,
        Some((WIDTH, HEIGHT)),
        "the window at its own size"
    );

    // What the relay forwarded is sealed: no frame's bytes appear in it, and
    // a device holding other keys opens none of it.
    for (plain, sealed) in plaintexts.iter().zip(&forwarded) {
        let probe = &plain[..plain.len().min(24)];
        assert!(
            !sealed.windows(probe.len()).any(|window| window == probe),
            "the relay never sees a frame's bytes"
        );
    }
    let mut outsider = Viewing::new();
    let stranger = keys_of(channel, 9);
    let picture_count = forwarded.len();
    assert!(picture_count >= plaintexts.len());
    let late = join(&relay, b"late", 'd');
    live(&late, "the late member").await;

    // The window goes still; a member who starts showing it now is sent a
    // keyframe of it as it stands, and keys from outside the room open
    // nothing of it.
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    let _ = host.take_window_keyframes();
    late.show_windows(&name('a'), &[TAB]);
    let mut late_viewing = Viewing::new();
    let still = pictures[((FRAMES - 1) % 8) as usize].clone();
    let answer = || {
        session.start_reader().or_else(|| {
            session.deposit_captured_rgba(still.clone());
            session.encode_next_frame(Instant::now())
        })
    };
    let mut answers = Vec::new();
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let started = host.take_window_keyframes();
            if !outbound.readers_started(&started).is_empty() {
                let frame = answer().expect("a started reader gets a frame of the still window");
                answers.push(frame.seq);
                assert!(
                    outbound.send(&host, &HOST, &keys, &frame.into_neighbour_msg()),
                    "the keyframe goes onto the window's track"
                );
            }
            let mut copy = Vec::new();
            while let Some(picture) = late.try_recv_picture() {
                copy.push(picture);
            }
            for picture in copy {
                assert!(
                    outsider
                        .inbound
                        .receive((&HOST, 7), TAB, APP, &stranger, channel, &picture)
                        .is_empty(),
                    "other keys open nothing"
                );
                for message in
                    late_viewing
                        .inbound
                        .receive((&HOST, 7), TAB, APP, &keys, channel, &picture)
                {
                    if let NeighbourMsg::SurfaceStreamStart { .. } = message {
                        late_viewing.started = Some((WIDTH, HEIGHT));
                    }
                    if let NeighbourMsg::SurfaceFrame { .. } = message {
                        late_viewing.decoded += 1;
                    }
                }
            }
            if late_viewing.started.is_some() && late_viewing.decoded > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a member who starts showing a still window gets a keyframe of it");

    assert!(
        answers.len() <= 1,
        "one reader started, at most one keyframe"
    );

    tokio::time::sleep(Duration::from_millis(200)).await;
    while viewer.try_recv_picture().is_some() || late.try_recv_picture().is_some() {}
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(
        host.take_window_keyframes().is_empty(),
        "no keyframe is asked for again while nobody new joins"
    );
    assert!(
        viewer.try_recv_picture().is_none() && late.try_recv_picture().is_none(),
        "a still window sends nothing more"
    );

    viewer.show_windows(&name('a'), &[]);
    late.show_windows(&name('a'), &[]);
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while !host.wanted_windows().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("once nobody shows the window, nothing asks the host to encode it");

    let mut reshown = Vec::new();
    for _ in 0..2 {
        late.show_windows(&name('a'), &[TAB]);
        let mut again = Viewing::new();
        tokio::time::timeout(tilo_test_env::hang_bound(), async {
            loop {
                let started = host.take_window_keyframes();
                if !outbound.readers_started(&started).is_empty() {
                    let standing = session.start_reader();
                    let frame = standing.clone().or_else(&answer).expect("a keyframe");
                    reshown.push((standing.is_some(), frame.seq));
                    assert!(outbound.send(&host, &HOST, &keys, &frame.into_neighbour_msg()));
                }
                let mut copy = Vec::new();
                while let Some(picture) = late.try_recv_picture() {
                    copy.push(picture);
                }
                for picture in copy {
                    for message in
                        again
                            .inbound
                            .receive((&HOST, 7), TAB, APP, &keys, channel, &picture)
                    {
                        if let NeighbourMsg::SurfaceFrame { .. } = message {
                            again.decoded += 1;
                        }
                    }
                }
                if again.decoded > 0 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a member who shows the still window again sees it");
        late.show_windows(&name('a'), &[]);
        tokio::time::timeout(tilo_test_env::hang_bound(), async {
            while !host.wanted_windows().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("nobody shows the window");
    }
    let first = reshown.first().copied().expect("the host is asked afresh");
    assert!(
        reshown
            .iter()
            .skip(1)
            .all(|answer| *answer == (true, first.1)),
        "showing a still window again encodes nothing: {reshown:?}"
    );

    let mut latencies = viewing.latencies;
    latencies.sort_unstable();
    eprintln!(
        "room-window-relay encoder={:?} codec={codec} size={WIDTH}x{HEIGHT} sent={sent} \
         decoded={} pictures={picture_count} \
         frame_to_decode_ms(p50={:.2} p95={:.2} max={:.2})",
        session.chosen_encoder(),
        viewing.decoded,
        millis(percentile(&latencies, 50)),
        millis(percentile(&latencies, 95)),
        millis(percentile(&latencies, 100)),
    );
    assert!(
        percentile(&latencies, 50) < Duration::from_millis(250),
        "live through the relay"
    );
}

/// A picture opened as another window's, another host's or another place
/// in the stream does not authenticate.
#[test]
fn a_sealed_picture_opens_only_as_the_window_and_place_it_was_sealed_for() {
    let room = RoomId::new();
    let channel = windows_channel(room);
    let keys = keys_of(channel, 7);
    let sealed = super::seal(&keys, &HOST, TAB, 5, b"frame").unwrap();
    assert_eq!(
        super::open(&keys, channel, &HOST, TAB, 5, &sealed).as_deref(),
        Some(b"frame".as_slice())
    );
    assert!(super::open(&keys, channel, &HOST, TAB + 1, 5, &sealed).is_none());
    assert!(super::open(&keys, channel, &HOST, TAB, 6, &sealed).is_none());
    assert!(super::open(&keys, channel, &[0xB2; 32], TAB, 5, &sealed).is_none());
    let other_room = windows_channel(RoomId::new());
    assert!(super::open(&keys_of(other_room, 7), other_room, &HOST, TAB, 5, &sealed).is_none());
}

/// The carrier stands among a window's viewers under a handle of its own,
/// one per room.
#[test]
fn each_rooms_carrier_has_a_viewer_handle_of_its_own() {
    let (one, two) = (RoomId::new(), RoomId::new());
    assert_eq!(carrier_peer(one), carrier_peer(one));
    assert_ne!(carrier_peer(one), carrier_peer(two));
}
