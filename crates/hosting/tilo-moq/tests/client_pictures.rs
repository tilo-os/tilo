//! A call's pictures over the relay: a participant is asked for exactly the
//! picture layers someone shows, the relay forwards only those, and a viewer
//! that shows a smaller tile switches to the smaller layer.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion; fixture arithmetic is small and bounded"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};
use tilo_moq::client::{Client, ClientAuthority, Connection, ConnectionScope, Status};
use tilo_moq::relay::{Denied, Direction, LoopbackRelay, RelayAuthority, SessionAuthorization};
use tilo_moq::video::{Codec, Layer, Source, VideoObject};

fn call() -> String {
    "a".repeat(64)
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

/// `camera` publishes `b`; `viewer` publishes `c` and hears `b`.
struct Roles;
impl RelayAuthority for Roles {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let (publish, subscribe) = match proof {
            b"camera" => (name('b'), vec![]),
            b"viewer" => (name('c'), vec![name('b')]),
            _ => return Err(Denied),
        };
        Ok(Arc::new(Permit { publish, subscribe }))
    }
}

struct Owner;
impl ClientAuthority for Owner {
    fn current(&self, _: &ConnectionScope, _: Direction, _: &str) -> bool {
        true
    }
}

fn start(relay: &LoopbackRelay, proof: &[u8]) -> Client {
    let (publication, subscription_prefix) = if proof == b"camera" {
        (Some(name('b')), None)
    } else {
        (Some(name('c')), Some(call()))
    };
    Client::start(
        Connection {
            scope: ConnectionScope {
                endpoint: relay.endpoint.clone(),
                fingerprints: relay.fingerprints.clone(),
                owner_epoch: [1; 32],
                carrier_generation: 1,
                publication,
                subscription_prefix,
            },
            authorization: proof.to_vec(),
        },
        Arc::new(Owner),
    )
    .unwrap()
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"));
}

fn picture(layer: Layer, sequence: u64, keyframe: bool) -> VideoObject {
    let (width, height) = layer.size(Source::Camera);
    VideoObject {
        keyframe,
        codec: Codec::H264,
        layer,
        sequence,
        timestamp_us: sequence * 33_333,
        width,
        height,
        payload: vec![0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_participant_sends_only_the_layers_shown_and_a_viewer_switches_layers() {
    let relay = LoopbackRelay::start(Arc::new(Roles)).unwrap();
    let camera = start(&relay, b"camera");
    let viewer = start(&relay, b"viewer");
    until("both live", || {
        camera.status() == Status::Live && viewer.status() == Status::Live
    })
    .await;
    assert!(
        camera.wanted_pictures().is_empty(),
        "nobody shows the camera yet: nothing is encoded"
    );

    // The viewer shows the camera in a grid tile.
    viewer.show(&name('b'), &[(Source::Camera, Layer::Mid)]);
    until("the middle layer is asked for", || {
        camera.wanted_pictures() == [(Source::Camera, Layer::Mid)]
    })
    .await;
    let mut sequence = 0;
    let received = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let _ = camera.send_picture(Source::Camera, &picture(Layer::Mid, sequence, true));
            sequence += 1;
            if let Some(picture) = viewer.try_recv_picture() {
                return picture;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no picture: relay {:?}", relay.counters()));
    assert_eq!(received.source, Source::Camera);
    assert_eq!(received.object.layer, Layer::Mid);
    assert!(received.object.keyframe);
    assert_eq!(received.publisher.namespace, name('b'));

    // The tile shrinks to a filmstrip face: the small layer replaces it.
    viewer.show(&name('b'), &[(Source::Camera, Layer::Low)]);
    until(
        "the small layer is asked for and the middle one no longer",
        || camera.wanted_pictures() == [(Source::Camera, Layer::Low)],
    )
    .await;
    let started = Instant::now();
    let small = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let _ = camera.send_picture(Source::Camera, &picture(Layer::Low, sequence, true));
            sequence += 1;
            if let Some(picture) = viewer.try_recv_picture()
                && picture.object.layer == Layer::Low
            {
                return picture;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no small picture after {:?}", started.elapsed()));
    assert_eq!(small.object.width, 320);

    // A picture of a layer nobody shows is not sent.
    assert_eq!(
        camera.send_picture(Source::Camera, &picture(Layer::High, sequence, true)),
        Err(tilo_moq::client::ClientError::Muted)
    );
    // Showing nothing stops the camera's last layer.
    viewer.show(&name('b'), &[]);
    until("nothing is asked for", || {
        camera.wanted_pictures().is_empty()
    })
    .await;
}
