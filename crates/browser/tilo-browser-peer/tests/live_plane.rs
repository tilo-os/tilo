//! The page's device in the live plane: it joins a real relay listener over
//! WebTransport, seals its own presence and audio under the scope's keys, and
//! opens what a native participant publishes — the same path a native device
//! takes, with the page doing the sealing itself.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::string_slice,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use std::time::Duration;
use tokio::sync::watch;

use channel_crypto::{ChannelGroup, MemberKey};
use tilo_browser_peer::ChannelContentKeys;
use tilo_browser_peer::live::{LiveError, UNSUPPORTED};
use tilo_call_lane::{Opener, Roster, Sealer, Stream, TrackName};
use tilo_live::admit::{Connected, OriginTask, origin_config};
use tilo_live::live::{Namespace, Track};
use tilo_live::plane::{Plane, PlaneError};
use tilo_live::relay::{
    Denied, Direction, LoopbackRelay, Peer, RelayAuthority, SessionAuthorization,
};
use tilo_live::scope::Scope;
use tilo_live::voice::{FRAME_SAMPLES, VoiceObject};
use tilo_relay_protocol::web::LiveGrant;

/// The workspace the participants share.
const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
/// The page's device.
const PAGE: [u8; 32] = [0x21; 32];
/// The native participant's device.
const NATIVE: [u8; 32] = [0x42; 32];

/// The scope's keys, as a channel's members derive them.
fn keys() -> ChannelContentKeys {
    ChannelGroup::create(
        collab_types::ChannelId::new(),
        MemberKey::generate("alice").unwrap(),
    )
    .unwrap()
    .content_keys()
    .unwrap()
}

/// Whether the devices of this test are still members, and the change signal
/// the relay re-observes on.
#[derive(Clone)]
struct Admission {
    admitted: Arc<AtomicBool>,
    changed: watch::Sender<u64>,
}

impl Admission {
    fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            admitted: Arc::new(AtomicBool::new(true)),
            changed,
        }
    }

    fn revoke(&self) {
        self.admitted.store(false, Ordering::Release);
        let next = *self.changed.borrow() + 1;
        let _ = self.changed.send(next);
    }
}

/// What a relay authority grants one participant: its own namespace to publish
/// in, and the scope's subtree to read, while `admitted` holds.
struct Permit {
    namespace: String,
    scope: String,
    admission: Admission,
}

impl SessionAuthorization for Permit {
    fn publication(&self) -> Option<&str> {
        Some(&self.namespace)
    }
    fn subscriptions(&self) -> &[String] {
        &[]
    }
    fn subscription_prefix(&self) -> Option<&str> {
        Some(&self.scope)
    }
    fn current(&self, direction: Direction, namespace: &str) -> bool {
        if !self.admission.admitted.load(Ordering::Acquire) {
            return false;
        }
        match direction {
            Direction::Publish => namespace == self.namespace,
            Direction::Subscribe => namespace
                .strip_prefix(&self.scope)
                .is_some_and(|rest| rest.starts_with("/participant/")),
        }
    }
    fn changed(&self) -> Option<watch::Receiver<u64>> {
        Some(self.admission.changed.subscribe())
    }
}

/// Admits the devices of this test. A page's grant is its workspace grant; a
/// native device presents its device key.
struct Admits {
    scope: Scope,
    roster: Roster,
    admission: Admission,
}

impl RelayAuthority for Admits {
    fn admit(&self, _peer: Peer, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let device = match LiveGrant::decode(proof) {
            Some(grant) => grant.token,
            None => <[u8; 32]>::try_from(proof).map_err(|_| Denied)?,
        };
        if !self.roster.admits(device) {
            return Err(Denied);
        }
        Ok(Arc::new(Permit {
            namespace: Namespace::new(&self.scope, device).path(),
            scope: Namespace::new(&self.scope, device).scope_path(),
            admission: self.admission.clone(),
        }))
    }
}

/// The page's grant: its own device's session token in the workspace the
/// relay's page authority numbers 7, as `/live` issued it.
fn page_grant(token: [u8; 32]) -> LiveGrant {
    LiveGrant {
        workspace_id: 7,
        token,
    }
}

/// The pinned certificate of a loopback listener, base64 as `/live` reports it.
fn certificate(relay: &LoopbackRelay) -> String {
    use base64::Engine as _;
    let hex = relay.fingerprints.first().unwrap();
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
        .collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A page that joins the relay's listener as its own device.
async fn page(relay: &LoopbackRelay, keys: ChannelContentKeys, roster: Roster) -> Plane {
    let origin = relay
        .endpoint
        .strip_suffix(tilo_live::preface::PATH)
        .expect("the loopback relay serves the carrier's path");
    tilo_browser_peer::live::join(
        origin,
        &certificate(relay),
        page_grant(PAGE),
        Scope::workspace(WORKSPACE),
        PAGE,
        keys,
        roster,
    )
    .await
    .unwrap()
}

/// Publish one 20 ms Opus frame `opus` as voice object `sequence`, as a page
/// builds it before its plane seals the payload.
fn voice_object(opus: &[u8], sequence: u32) -> Vec<u8> {
    let timestamp = u64::from(sequence).saturating_mul(u64::from(FRAME_SAMPLES));
    VoiceObject {
        sequence: u64::from(sequence),
        timestamp,
        duration: FRAME_SAMPLES,
        payload: opus.to_vec(),
    }
    .encode()
    .unwrap()
}

/// The native participant: the carrier client a device runs, sealing and
/// opening its own tracks with no page code involved.
struct Native {
    presence: Sealer,
    audio: Sealer,
    opener: Opener,
    _broadcast: moq_net::broadcast::Producer,
    presence_track: moq_net::track::Producer,
    audio_track: Mutex<moq_net::track::Producer>,
    reading: moq_net::origin::Consumer,
    _producers: (moq_net::origin::Producer, moq_net::origin::Producer),
    _publishing: OriginTask,
    _reading: OriginTask,
    _connected: Connected,
}

impl Native {
    async fn join(relay: &LoopbackRelay, keys: ChannelContentKeys, roster: Roster) -> Self {
        let scope = Scope::workspace(WORKSPACE);
        let namespace = Namespace::new(&scope, NATIVE);
        let (publishing, publishing_task) =
            OriginTask::produce(origin_config(1 << 20, Duration::from_secs(30)));
        let broadcast = publishing
            .create_broadcast(namespace.path().as_str())
            .unwrap();
        let presence_track = broadcast
            .create_track(
                "presence",
                moq_net::track::Info::default().with_max_age(Duration::from_secs(30)),
            )
            .unwrap();
        broadcast
            .announce(moq_net::origin::Route::default())
            .unwrap();
        let publish = publishing
            .consume()
            .scope(
                "",
                &moq_net::Patterns::from(
                    moq_net::Pattern::literal(namespace.path().as_str()).unwrap(),
                ),
            )
            .unwrap();
        let (reading, reading_task) =
            OriginTask::produce(origin_config(1 << 20, Duration::from_secs(2)));
        let subscribe = reading
            .consume()
            .scope(
                "",
                &moq_net::Patterns::from(
                    moq_net::Pattern::subtree(namespace.scope_path().as_str()).unwrap(),
                ),
            )
            .unwrap();
        let url: url::Url = relay.endpoint.parse().unwrap();
        let connected = tilo_live::carrier::connect(
            &url,
            &tilo_live::carrier::Trust::Fingerprints(relay.fingerprints.clone()),
            &NATIVE,
            Some(publish),
            Some(reading.clone()),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        Self {
            presence: Sealer::new(
                &keys,
                NATIVE,
                Stream::Presence,
                TrackName::new(&scope, "presence"),
            )
            .unwrap(),
            audio: Sealer::new(
                &keys,
                NATIVE,
                Stream::Microphone,
                TrackName::new(&scope, "audio"),
            )
            .unwrap(),
            opener: Opener::new(keys, roster),
            _broadcast: broadcast.clone(),
            presence_track,
            audio_track: Mutex::new(
                broadcast
                    .create_track(
                        "audio",
                        moq_net::track::Info::default().with_max_age(Duration::from_millis(200)),
                    )
                    .unwrap(),
            ),
            reading: subscribe,
            _producers: (publishing, reading),
            _publishing: publishing_task,
            _reading: reading_task,
            _connected: connected,
        }
    }

    fn publish_presence(&self, presence: &[u8]) {
        let sealed = self.presence.seal(0, presence).unwrap();
        let mut group = self.presence_track.append_group().unwrap();
        group
            .write_frame(moq_net::Timestamp::ZERO, bytes::Bytes::from(sealed))
            .unwrap();
        group.finish().unwrap();
    }

    fn publish_audio(&self, opus: &[u8], seq: u64) {
        let sealed = self.audio.seal(u32::try_from(seq).unwrap(), opus).unwrap();
        let timestamp = seq * u64::from(tilo_live::voice::FRAME_SAMPLES);
        let object = VoiceObject {
            sequence: seq,
            timestamp,
            duration: tilo_live::voice::FRAME_SAMPLES,
            payload: sealed,
        }
        .encode()
        .unwrap();
        let at =
            moq_net::Timestamp::from_scale(timestamp, u64::from(tilo_live::voice::SAMPLE_RATE))
                .unwrap();
        self.audio_track
            .lock()
            .unwrap()
            .insert_datagram(seq, at, bytes::Bytes::from(object))
            .unwrap();
    }

    /// The page's sealed presence, opened as the page's device.
    async fn presence_from_page(&self) -> Vec<u8> {
        let scope = Namespace::new(&Scope::workspace(WORKSPACE), PAGE);
        let broadcast = tokio::time::timeout(
            Duration::from_secs(10),
            self.reading.routed_broadcast(scope.path().as_str()),
        )
        .await
        .unwrap()
        .unwrap();
        let track = broadcast.track("presence").unwrap();
        let mut subscriber = tokio::time::timeout(
            Duration::from_secs(10),
            track.subscribe(
                moq_net::track::Subscription::default().with_max_age(Duration::from_secs(30)),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let mut group = tokio::time::timeout(Duration::from_secs(10), subscriber.recv_group())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(10), group.read_frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        self.opener
            .open(
                PAGE,
                Stream::Presence,
                &TrackName::new(&Scope::workspace(WORKSPACE), "presence"),
                &frame.payload,
            )
            .unwrap()
    }

    /// The page's sealed audio objects, opened in order.
    async fn audio_from_page(&self, count: usize) -> Vec<Vec<u8>> {
        let scope = Namespace::new(&Scope::workspace(WORKSPACE), PAGE);
        let broadcast = tokio::time::timeout(
            Duration::from_secs(10),
            self.reading.routed_broadcast(scope.path().as_str()),
        )
        .await
        .unwrap()
        .unwrap();
        let track = broadcast.track("audio").unwrap();
        let mut subscriber = tokio::time::timeout(
            Duration::from_secs(10),
            track.subscribe(
                moq_net::track::Subscription::default().with_max_age(Duration::from_millis(200)),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let mut opened = Vec::new();
        while opened.len() < count {
            let datagram =
                tokio::time::timeout(Duration::from_secs(10), subscriber.recv_datagram())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
            for bytes in tilo_live::voice::bundle_objects(&datagram.payload).unwrap() {
                let object = VoiceObject::decode(bytes).unwrap();
                let opus = self
                    .opener
                    .open(
                        PAGE,
                        Stream::Microphone,
                        &TrackName::new(&Scope::workspace(WORKSPACE), "audio"),
                        &object.payload,
                    )
                    .unwrap();
                if opened.len() < count {
                    opened.push(opus);
                }
            }
        }
        opened
    }
}

/// Publish with `publish` until `opened` reports the frame, or the deadline
/// passes: a relay's copy subscribes to a publisher's track after its first
/// write, so the first objects of a burst are not carried.
async fn published_until<T>(
    what: &str,
    mut publish: impl FnMut(),
    mut opened: impl FnMut() -> Option<T>,
) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        publish();
        if let Some(opened) = opened() {
            return opened;
        }
        assert!(std::time::Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_and_a_native_device_exchange_sealed_presence_and_audio_through_a_relay() {
    let roster = Roster::new([PAGE, NATIVE]);
    let keys = keys();
    let relay = LoopbackRelay::start(Arc::new(Admits {
        scope: Scope::workspace(WORKSPACE),
        roster: roster.clone(),
        admission: Admission::new(),
    }))
    .unwrap();
    let native = Native::join(&relay, keys.clone(), roster.clone()).await;
    let page = page(&relay, keys, roster).await;

    // Presence the native device seals opens as that device, in the page.
    let mut presence_sequence = 0_u32;
    let opened = published_until(
        "the native device's presence never reached the page",
        || native.publish_presence(b"role=member"),
        || {
            page.try_recv()
                .filter(|heard| heard.sender == NATIVE && heard.track == Track::Presence)
        },
    )
    .await;
    assert_eq!(opened.body, b"role=member");

    // Presence the page seals opens as the page's own device, natively: the
    // page does its own sealing, never a bridging device's.
    let from_page = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            page.publish(Track::Presence, presence_sequence, b"role=member")
                .unwrap();
            presence_sequence = presence_sequence.wrapping_add(1);
            if let Ok(presence) =
                tokio::time::timeout(Duration::from_millis(100), native.presence_from_page()).await
            {
                break presence;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the page's presence never reached the native device");
    assert_eq!(from_page, b"role=member");

    // Audio both ways: the page sends datagram bundles, the native device
    // groups, and each opens the other's objects.
    let tone: Vec<u8> = (0..160).map(|byte| byte as u8).collect();
    let heard = tokio::time::timeout(Duration::from_secs(10), async {
        let mut sequence = 0_u32;
        loop {
            let object = voice_object(&tone, sequence);
            page.publish(Track::Audio, sequence, &object).unwrap();
            sequence = sequence.wrapping_add(1);
            if let Ok(opened) =
                tokio::time::timeout(Duration::from_millis(100), native.audio_from_page(1)).await
            {
                break opened;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the page's audio never reached the native device");
    assert_eq!(heard, vec![tone]);

    let voiced: Vec<u8> = (100..260).map(|byte| byte as u8).collect();
    let mut sequence = 0_u64;
    let opened = published_until(
        "the native device's audio never reached the page",
        || {
            native.publish_audio(&voiced, sequence);
            sequence = sequence.wrapping_add(1);
        },
        || {
            page.try_recv()
                .filter(|heard| heard.sender == NATIVE && heard.track == Track::Audio)
        },
    )
    .await;
    assert_eq!(opened.body, voiced);
}

/// A page's presence reaches a device across one relay hop, sealed and
/// opened on the way, inside the budget a pointer needs to feel immediate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pages_presence_reaches_a_device_inside_the_interactive_budget() {
    let roster = Roster::new([PAGE, NATIVE]);
    let keys = keys();
    let relay = LoopbackRelay::start(Arc::new(Admits {
        scope: Scope::workspace(WORKSPACE),
        roster: roster.clone(),
        admission: Admission::new(),
    }))
    .unwrap();
    let native = Native::join(&relay, keys.clone(), roster.clone()).await;
    let page = page(&relay, keys, roster).await;

    // The device's reader stays subscribed to the page's presence, as a
    // device's plane keeps one open for the whole session.
    let address = Namespace::new(&Scope::workspace(WORKSPACE), PAGE);
    let broadcast = tokio::time::timeout(
        Duration::from_secs(10),
        native.reading.routed_broadcast(address.path().as_str()),
    )
    .await
    .unwrap()
    .unwrap();
    let track = broadcast.track("presence").unwrap();
    let mut subscriber = tokio::time::timeout(
        Duration::from_secs(10),
        track.subscribe(
            moq_net::track::Subscription::default().with_max_age(Duration::from_secs(30)),
        ),
    )
    .await
    .unwrap()
    .unwrap();

    let mut sequence = 0_u32;

    // The relay's copy subscribes to a publisher's track after its first
    // write, and a new subscriber starts from what the relay already holds:
    // both are warmed past before anything is measured.
    for _ in 0..20 {
        page.publish(Track::Presence, sequence, b"role=member")
            .unwrap();
        sequence = sequence.wrapping_add(1);
        let mut group = tokio::time::timeout(Duration::from_secs(5), subscriber.recv_group())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), group.read_frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    let mut latencies = Vec::new();
    for _ in 0..200 {
        let sent = std::time::Instant::now();
        page.publish(Track::Presence, sequence, b"role=member")
            .unwrap();
        sequence = sequence.wrapping_add(1);
        let mut group = tokio::time::timeout(Duration::from_secs(5), subscriber.recv_group())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame = tokio::time::timeout(Duration::from_secs(5), group.read_frame())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(!frame.payload.is_empty(), "a sealed presence object");
        latencies.push(sent.elapsed());
    }
    latencies.sort_unstable();
    let p95 = latencies
        .get(latencies.len().saturating_mul(95) / 100)
        .copied()
        .unwrap();
    eprintln!(
        "page to device presence: p50 {:?}, p95 {p95:?} over {} objects",
        latencies.get(latencies.len() / 2).copied().unwrap(),
        latencies.len()
    );
    assert!(
        p95 < Duration::from_millis(50),
        "page to device presence p95 {p95:?} is outside the interactive budget"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_whose_device_is_removed_is_cut_and_publishes_nothing_more() {
    let roster = Roster::new([PAGE, NATIVE]);
    let admission = Admission::new();
    let relay = LoopbackRelay::start(Arc::new(Admits {
        scope: Scope::workspace(WORKSPACE),
        roster: roster.clone(),
        admission: admission.clone(),
    }))
    .unwrap();
    let page = page(&relay, keys(), roster).await;
    page.publish(Track::Presence, 0, b"role=member").unwrap();

    // The roster change reports itself; the relay re-observes and cuts.
    admission.revoke();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !page.ended() && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(page.ended(), "the removed page's session stayed open");
    assert_eq!(
        page.publish(Track::Presence, 1, b"role=member"),
        Err(PlaneError::Ended)
    );
    assert_eq!(page.publish(Track::Audio, 0, &[1]), Err(PlaneError::Ended));
}

#[tokio::test]
async fn a_browser_without_webtransport_is_told_so_and_offers_no_fallback() {
    assert_eq!(LiveError::Unsupported.to_string(), UNSUPPORTED);
    assert_eq!(UNSUPPORTED, "this browser cannot join live");
    // No fallback: an origin that cannot be opened is reported, not retried
    // over any other transport.
    let refused = tilo_browser_peer::live::join(
        "not-a-webtransport-origin",
        &certificate_of(&[7; 32]),
        page_grant(PAGE),
        Scope::workspace(WORKSPACE),
        PAGE,
        keys(),
        Roster::new([PAGE]),
    )
    .await;
    assert_eq!(refused.err(), Some(LiveError::Unavailable));
}

/// The base64 form of a pinned certificate digest.
fn certificate_of(sha256: &[u8; 32]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(sha256)
}
