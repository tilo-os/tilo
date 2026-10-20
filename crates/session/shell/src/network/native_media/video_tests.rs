//! A call's pictures end to end on this machine: two participants, each
//! with the moving test picture as their camera, over a loopback relay. Each
//! captures through `GStreamer`, encodes the layer the other shows with the
//! machine's encoder (hardware first), sends it through the relay, and the
//! other decodes it back into a picture with the test picture's colours.
//! Each also sees its own camera, encoded on the device for its own tile.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion; fixture arithmetic is small and bounded"
)]

use super::video::{CameraChoice, Publisher};
use std::sync::Arc;
use std::time::Duration;
use tilo_moq::client::{Client, ClientAuthority, Connection, ConnectionScope, Status};
use tilo_moq::relay::{Denied, Direction, LoopbackRelay, RelayAuthority, SessionAuthorization};
use tilo_moq::video::{Layer, Source, VideoObject};

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

/// Ana publishes `b` and hears `c`; Ben publishes `c` and hears `b`.
struct Roles;
impl RelayAuthority for Roles {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let (publish, subscribe) = match proof {
            b"ana" => (name('b'), vec![name('c')]),
            b"ben" => (name('c'), vec![name('b')]),
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

/// Decode `client`'s received pictures until one comes out.
async fn decoded_picture(client: &Client, what: &str) -> (VideoObject, media::Frame) {
    let mut decoder = None;
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            while let Some(received) = client.try_recv_picture() {
                let object = received.object;
                if decoder.is_none() {
                    if !object.keyframe {
                        continue;
                    }
                    let mut config = media::DecoderConfig::new(media::StreamCodec::H264);
                    config.output = media::DecodeOutput::Rgba;
                    decoder = Some(media::RealtimeDecoder::new(config).expect("an H.264 decoder"));
                }
                let frame = decoder
                    .as_mut()
                    .unwrap()
                    .decode(&object.payload, Some(object.keyframe))
                    .expect("the picture decodes");
                if let Some(frame) = frame {
                    return (object, frame.frame);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}: no picture decoded"))
}

/// Whether `rgba` (`width` wide) shows the colour bars' yellow, cyan and
/// magenta somewhere along its row `y`: the test picture, wherever it has
/// scrolled to.
fn shows_colour_bars(rgba: &[u8], width: u32, y: u32) -> bool {
    let row = (y * width * 4) as usize;
    let pixels: Vec<[u8; 3]> = rgba[row..row + (width * 4) as usize]
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect();
    let has = |test: &dyn Fn([u8; 3]) -> bool| pixels.iter().any(|pixel| test(*pixel));
    has(&|[r, g, b]| r > 140 && g > 140 && b < 70)
        && has(&|[r, g, b]| r < 70 && g > 140 && b > 140)
        && has(&|[r, g, b]| r > 140 && g < 70 && b > 140)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_people_see_each_others_camera_decoded_and_their_own_on_their_device() {
    let relay = LoopbackRelay::start(Arc::new(Roles)).unwrap();
    let ana = join(&relay, b"ana", 'b');
    let ben = join(&relay, b"ben", 'c');
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        while ana.status() != Status::Live || ben.status() != Status::Live {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("both carriers live");

    let ana_carrier = ana.clone();
    let ana_camera = Publisher::camera(
        &CameraChoice::TestPattern,
        Arc::new(move || Some(ana_carrier.clone())),
    )
    .expect("Ana's camera starts");
    let ben_carrier = ben.clone();
    let ben_camera = Publisher::camera(
        &CameraChoice::TestPattern,
        Arc::new(move || Some(ben_carrier.clone())),
    )
    .expect("Ben's camera starts");
    // Ana sees herself in her own tile.
    ana_camera.preview().set(Some(Layer::Low));

    // Each shows the other: Ana in a grid tile, Ben as a filmstrip face.
    ben.show(&name('b'), &[(Source::Camera, Layer::Mid)]);
    ana.show(&name('c'), &[(Source::Camera, Layer::Low)]);

    let (object, frame) = decoded_picture(&ben, "Ben sees Ana").await;
    assert_eq!(object.layer, Layer::Mid);
    assert_eq!((object.width, object.height), (640, 360));
    assert_eq!((frame.width, frame.height), (640, 360));
    let rgba = frame.to_rgba().expect("an RGBA picture");
    assert!(
        shows_colour_bars(&rgba, frame.width, 60),
        "Ana's picture arrives with the test picture's colours"
    );

    let (object, frame) = decoded_picture(&ana, "Ana sees Ben").await;
    assert_eq!(object.layer, Layer::Low);
    assert_eq!((frame.width, frame.height), (320, 180));
    assert!(shows_colour_bars(
        &frame.to_rgba().unwrap(),
        frame.width,
        30
    ));

    // Only the layers someone shows were encoded and sent.
    assert_eq!(ana.wanted_pictures(), [(Source::Camera, Layer::Mid)]);
    assert_eq!(ben.wanted_pictures(), [(Source::Camera, Layer::Low)]);

    // Ana's own picture, for her own tile, starts at a keyframe.
    let own = tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            let (objects, _) = ana_camera.preview().take();
            if let Some(first) = objects.into_iter().next() {
                return first;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Ana's own picture");
    assert!(own.keyframe);
    assert_eq!((own.width, own.height), (320, 180));

    // Ben turns his camera off: Ana's pictures stop coming.
    drop(ben_camera);
    tokio::time::sleep(Duration::from_millis(300)).await;
    while ana.try_recv_picture().is_some() {}
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        ana.try_recv_picture().is_none(),
        "no picture after the camera stopped"
    );
    drop(ana_camera);
}
