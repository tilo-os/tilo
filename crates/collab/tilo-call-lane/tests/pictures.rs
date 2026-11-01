//! A call's pictures between two devices of one channel: the camera one
//! device sends is encoded once for the layer the other shows, sealed under
//! the channel's keys, and read by the other as a picture of that layer and
//! not of the camera's capture size.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion"
)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::ChannelId;
use iroh::endpoint::presets;
use iroh::protocol::Router;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use tilo_call_lane::video::{Asking, Picture, Sink};
use tilo_call_lane::{
    CAMERA_SIZE, Calls, Frame, Hello, Inbox, Lane, Roster, Sending, Video, even_rgba, fitted,
};
use tilo_lock::MutexRecover as _;
use tilo_moq::video::{Layer, Source, VideoObject};

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const CALL: &str = "voice";
const ALICE: [u8; 32] = [1; 32];
const BOB: [u8; 32] = [2; 32];

// ---------------------------------------------------------------------------
// The call's keys, as the channel's members hold them.
// ---------------------------------------------------------------------------

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
// Two devices of one call, in one process.
// ---------------------------------------------------------------------------

struct Side {
    endpoint: Endpoint,
    id: [u8; 32],
    calls: Calls,
    video: Arc<Video>,
    keys: ChannelContentKeys,
    arrivals: tokio::sync::mpsc::UnboundedReceiver<tilo_call_lane::Peer>,
    _router: Router,
}

async fn side(secret: [u8; 32], channel: ChannelId, keys: &ChannelContentKeys) -> Side {
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
            lane: Lane::Video,
        },
        Roster::default(),
    );
    let video = Video::new(id);
    video.set_keys(keys, Roster::new([id]));
    let router = Router::builder(endpoint.clone())
        .accept(tilo_call_lane::ALPN, calls.clone())
        .spawn();
    Side {
        endpoint,
        id,
        calls,
        video,
        keys: keys.clone(),
        arrivals,
        _router: router,
    }
}

impl Side {
    fn addr(&self) -> EndpointAddr {
        EndpointAddr::new(self.endpoint.id()).with_ip_addr(SocketAddr::from((
            Ipv4Addr::LOCALHOST,
            // The lane's connections run on the endpoint's bound socket.
            self.endpoint.bound_sockets().first().unwrap().port(),
        )))
    }
}

/// Both devices certify the other, as a workspace that admitted both: the
/// lanes accept each other, and the pictures each opens are the other's.
fn trust(a: &Side, b: &Side) {
    let roster = Roster::new([a.id, b.id]);
    a.calls.set_roster(roster.clone());
    b.calls.set_roster(roster.clone());
    a.video.set_keys(&a.keys, roster.clone());
    b.video.set_keys(&b.keys, roster);
}

/// Bob dials Alice, and both sides take the lane that results.
async fn lane(b: &Side, a: &mut Side) {
    let (dialled, accepted) = tokio::join!(
        b.calls.dial(&b.endpoint, a.addr()),
        tilo_test_env::hang::within("the other device takes the lane", a.arrivals.recv()),
    );
    let from_alice = dialled.expect("a device the workspace admitted reaches the call");
    let from_bob = accepted.expect("Alice takes Bob's lane");
    assert_eq!(from_alice.lane(), Lane::Video);
    b.video.add(from_alice);
    a.video.add(from_bob);
}

// ---------------------------------------------------------------------------
// What a camera sends.
// ---------------------------------------------------------------------------

/// The device's camera, as a call's capture hands it to the encoder: one
/// full-size frame, shared by every picture published from it.
fn camera_frame(fill: u8) -> Frame {
    let (width, height) = CAMERA_SIZE;
    let pixels = Arc::from(vec![fill; (width * height * 4) as usize]);
    Frame {
        pixels,
        width,
        height,
        format: media::realtime::InputFormat::Rgba,
        captured_at: Instant::now(),
    }
}

fn has_h264_encoder() -> bool {
    media::realtime::local_capabilities().can_encode(media::StreamCodec::H264)
}

/// Wait for a picture the app can collect, or fail saying what was waited
/// for.
async fn picture(video: &Video, what: &str, after: u64) -> (Picture, u64) {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("a deadline");
    loop {
        let (taken, cursor) = video.take(after, usize::MAX);
        if let Some(first) = taken.into_iter().next() {
            return (first, cursor);
        }
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

// ---------------------------------------------------------------------------
// The layers.
// ---------------------------------------------------------------------------

#[test]
fn a_layer_keeps_its_sources_shape_within_its_box_and_never_grows_it() {
    assert_eq!(fitted((1280, 720), (320, 180)), (320, 180));
    assert_eq!(fitted((1920, 1200), (1920, 1080)), (1728, 1080));
    assert_eq!(fitted((2560, 1080), (640, 360)), (640, 270));
    assert_eq!(fitted((800, 600), (1920, 1080)), (800, 600));
    assert_eq!(fitted((1366, 768), (320, 180)), (320, 178));
}

#[test]
fn an_odd_screen_is_cropped_to_even_rows_and_columns() {
    let rgba: Arc<[u8]> = Arc::from(vec![9; 5 * 3 * 4]);
    let frame = even_rgba(5, 3, rgba).unwrap();
    assert_eq!((frame.width, frame.height), (4, 2));
    assert_eq!(frame.pixels.len(), 4 * 2 * 4);
    assert!(even_rgba(1, 1, Arc::from(vec![0; 4])).is_none());
    assert!(even_rgba(4, 2, Arc::from(vec![0; 3])).is_none());
}

fn picture_of(stream: [u8; 32], size: usize, keyframe: bool) -> Picture {
    Picture {
        from: stream,
        source: Source::Camera,
        keyframe,
        object: vec![1; size],
    }
}

#[test]
fn the_app_collects_pictures_in_order_and_acknowledges_with_its_cursor() {
    let mut inbox = Inbox::default();
    for _ in 0..3 {
        inbox.admit(picture_of([1; 32], 10, true));
    }
    let (first, cursor) = inbox.take(0, 15);
    assert_eq!(
        first.len(),
        1,
        "a budget smaller than two pictures takes one"
    );
    let (again, same) = inbox.take(0, 15);
    assert_eq!(
        (again.len(), same),
        (1, cursor),
        "a lost answer is answered again"
    );
    let (rest, end) = inbox.take(cursor, 1024);
    assert_eq!(rest.len(), 2);
    assert_eq!(inbox.take(end, 1024).0.len(), 0);
}

#[test]
fn a_stream_that_falls_behind_starts_over_at_its_next_keyframe() {
    let mut inbox = Inbox::default();
    let big = tilo_call_lane::video::INBOX_BYTES / 4;
    let slow = [7; 32];
    let fast = [8; 32];
    inbox.admit(picture_of(slow, big, true));
    inbox.admit(picture_of(slow, big, false));
    inbox.admit(picture_of(fast, big, true));
    inbox.admit(picture_of(fast, big, false));
    // One more: the oldest stream gives way whole, and says so.
    assert_eq!(
        inbox.admit(picture_of(fast, big, false)),
        vec![(slow, Source::Camera)]
    );
    let (queued, cursor) = inbox.take(0, usize::MAX);
    assert!(queued.iter().all(|picture| picture.from == fast));
    inbox.take(cursor, usize::MAX);
    inbox.admit(picture_of(slow, 10, false));
    assert!(
        inbox.take(cursor, usize::MAX).0.is_empty(),
        "not without its keyframe"
    );
    inbox.admit(picture_of(slow, 10, true));
    inbox.admit(picture_of(slow, 10, false));
    assert_eq!(inbox.take(cursor, usize::MAX).0.len(), 2);
}

// ---------------------------------------------------------------------------
// Encoding.
// ---------------------------------------------------------------------------

/// What the encoder thread sent, as the call's lanes would have told it to.
#[derive(Default)]
struct Witness {
    asking: Mutex<Asking>,
    sent: Mutex<Vec<VideoObject>>,
    own: Mutex<Vec<VideoObject>>,
}

impl Sink for Witness {
    fn asking(&self, _source: Source) -> Asking {
        self.asking.lock_recover().clone()
    }

    fn sent(&self, _source: Source, object: &VideoObject) {
        self.sent.lock_recover().push(object.clone());
    }

    fn own(&self, object: &VideoObject) {
        self.own.lock_recover().push(object.clone());
    }
}

/// Wait until `count` pictures were sent, or fail saying what was waited for.
async fn sent(witness: &Arc<Witness>, count: usize, what: &str) -> Vec<VideoObject> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .expect("a deadline");
    loop {
        let sent = witness.sent.lock_recover().clone();
        if sent.len() >= count {
            return sent;
        }
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_the_layers_someone_shows_are_encoded_and_each_starts_at_a_keyframe() {
    if !has_h264_encoder() {
        eprintln!("no H.264 encoder on this machine: nothing could encode the pictures");
        return;
    }
    let witness = Arc::new(Witness::default());
    {
        let mut asking = witness.asking.lock_recover();
        asking
            .wanted
            .insert(Source::Camera, [Layer::Low].into_iter().collect());
    }
    let sink: Arc<dyn Sink> = witness.clone();
    let sending =
        Arc::new(Sending::start(Source::Camera, 30, &sink).expect("a camera encoder thread"));
    let publisher = tokio::spawn(publish_frames(sending));
    let only_low = sent(&witness, 3, "the shown layer's pictures").await;
    assert!(
        only_low.iter().all(|object| object.layer == Layer::Low),
        "a layer nobody shows is not encoded"
    );
    assert!(
        only_low
            .iter()
            .any(|object| object.keyframe && (object.width, object.height) == (320, 180)),
        "the picture is the layer's size, not the camera's"
    );

    // A layer that starts being shown starts at a keyframe, from the last
    // frame, without a new one arriving.
    {
        let mut asking = witness.asking.lock_recover();
        asking
            .wanted
            .get_mut(&Source::Camera)
            .unwrap()
            .insert(Layer::Mid);
    }
    let marked = witness.sent.lock_recover().len();
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let sent = witness.sent.lock_recover().clone();
            if sent.len() > marked
                && sent[marked..]
                    .iter()
                    .any(|object| object.layer == Layer::Mid && object.keyframe)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        witness
            .sent
            .lock_recover()
            .iter()
            .any(|object| object.layer == Layer::Mid && (object.width, object.height) == (640, 360)),
        "a new reader's layer is encoded as a keyframe of its own size"
    );
    publisher.abort();
}

/// A still screen is encoded once, and a device that starts showing it later
/// is answered with the keyframe it already has, not a new encode.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_still_screen_is_encoded_once_and_a_new_reader_starts_at_its_standing_keyframe() {
    if !has_h264_encoder() {
        eprintln!("no H.264 encoder on this machine: nothing could encode the pictures");
        return;
    }
    let witness = Arc::new(Witness::default());
    witness
        .asking
        .lock_recover()
        .wanted
        .insert(Source::Screen, [Layer::Low].into_iter().collect());
    let sink: Arc<dyn Sink> = witness.clone();
    let sending = Arc::new(Sending::start(Source::Screen, 15, &sink).expect("a screen encoder"));
    // One frame, which is all a still screen draws.
    sending.publish(camera_frame(7));
    let first = sent(&witness, 1, "the still screen's one picture").await;
    assert_eq!(first.len(), 1, "a still screen is encoded once");
    assert!(first[0].keyframe, "a reader starts at a keyframe");
    assert_eq!(
        (first[0].width, first[0].height),
        Layer::Low.size(Source::Screen),
        "the layer the reader shows"
    );

    // No frame arrives and no one new shows it: nothing more is encoded or
    // sent.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        witness.sent.lock_recover().len(),
        1,
        "a still screen sends nothing while nobody new shows it"
    );

    // A device starts showing the screen afresh: it is answered with the
    // keyframe the encoder stands on, the same picture, costing no encode.
    witness.asking.lock_recover().restart.insert(Source::Screen);
    sending.ask_keyframe();
    let again = sent(&witness, 2, "the standing keyframe for a new reader").await;
    assert_eq!(again.len(), 2, "the new reader is answered once");
    assert_eq!(
        again[1].sequence, first[0].sequence,
        "the picture the encoder stands on, not a new one"
    );
    assert_eq!(again[1].layer, Layer::Low);
}

/// Publish frames until the task is aborted, as a camera running during the
/// test does.
async fn publish_frames(sending: Arc<Sending>) {
    let mut step = 0_u8;
    loop {
        sending.publish(camera_frame(step));
        step = step.wrapping_add(1);
        tokio::time::sleep(Duration::from_millis(33)).await;
    }
}

// ---------------------------------------------------------------------------
// Two devices.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_shown_at_low_reaches_the_other_device_as_that_layer_alone() {
    if !has_h264_encoder() {
        eprintln!("no H.264 encoder on this machine: nothing could encode the pictures");
        return;
    }
    let (channel, alice_keys, bob_keys) = call_keys();
    let mut alice = side(ALICE, channel, &alice_keys).await;
    let bob = side(BOB, channel, &bob_keys).await;
    trust(&alice, &bob);
    lane(&bob, &mut alice).await;
    // Bob's tile shows Alice's camera at the small layer, and none of her
    // screen: what Alice encodes is exactly that.
    bob.video.show(alice.id, &[Layer::Low], &[]);
    // Alice's own tile shows her camera at the same layer.
    alice.video.own_tile(Some(Layer::Low));

    let sending = alice
        .video
        .start(Source::Camera, 30)
        .expect("a camera encoder thread");
    let publisher = tokio::spawn(publish_frames(Arc::clone(&sending)));

    let (received, _cursor) = picture(&bob.video, "Bob's tile shows Alice's camera", 0).await;
    assert_eq!(received.from, alice.id, "the keys say whose picture it is");
    assert_eq!(received.source, Source::Camera);
    let object = VideoObject::decode(&received.object).expect("a tilo-video/1 object");
    assert_eq!(object.layer, Layer::Low, "the layer Bob shows, alone");
    assert_eq!(
        (object.width, object.height),
        (320, 180),
        "the picture is the layer's size, not the camera's"
    );
    assert!(object.keyframe, "a stream starts at a keyframe");
    assert!(
        received.object.len() < 100_000,
        "the picture's bytes are the layer's, not the 720p capture's: {} bytes",
        received.object.len()
    );

    // Alice's own tile is fed on her device, not over the lane.
    let (own, _) = picture(&alice.video, "Alice's own tile shows her camera", 0).await;
    assert_eq!(own.from, alice.id);
    assert_eq!(
        VideoObject::decode(&own.object).unwrap().layer,
        Layer::Low,
        "the own picture is the layer her tile shows"
    );

    // Every picture Bob receives is of the one layer he shows.
    let mut cursor = 0;
    for _ in 0..5 {
        let (received, next) = picture(&bob.video, "another picture of the layer", cursor).await;
        cursor = next;
        assert_eq!(
            VideoObject::decode(&received.object).unwrap().layer,
            Layer::Low
        );
    }
    publisher.abort();
    alice.video.close();
    bob.video.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lane_that_joins_after_the_app_showed_a_picture_hears_it() {
    if !has_h264_encoder() {
        eprintln!("no H.264 encoder on this machine: nothing could encode the pictures");
        return;
    }
    let (channel, alice_keys, bob_keys) = call_keys();
    let mut alice = side(ALICE, channel, &alice_keys).await;
    let bob = side(BOB, channel, &bob_keys).await;
    trust(&alice, &bob);
    // Bob's tile already shows Alice's camera when the lane opens, and his
    // own pictures with it.
    bob.video.show(alice.id, &[Layer::Low], &[]);
    let own = bob.video.take(0, usize::MAX).0;
    assert!(own.is_empty(), "nothing arrived before the lane did");

    lane(&bob, &mut alice).await;
    let sending = alice
        .video
        .start(Source::Camera, 30)
        .expect("a camera encoder thread");
    let publisher = tokio::spawn(publish_frames(Arc::clone(&sending)));
    let (received, _) = picture(
        &bob.video,
        "the lane that joined hears what the app shows",
        0,
    )
    .await;
    assert_eq!(received.from, alice.id);
    publisher.abort();
    alice.video.close();
    bob.video.close();
}
