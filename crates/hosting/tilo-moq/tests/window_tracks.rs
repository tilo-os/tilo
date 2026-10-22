//! A member's windows over the relay: each window is a track of its own on
//! its host's publication, asked of the host only while someone shows it,
//! and every member who starts showing a window, still or not, makes the
//! host owe it a keyframe, since a reader can start at nothing else.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion; fixture arithmetic is small and bounded"
)]

use std::sync::Arc;
use std::time::Duration;
use tilo_moq::client::{Client, ClientAuthority, Connection, ConnectionScope, Status};
use tilo_moq::relay::{Denied, Direction, LoopbackRelay, RelayAuthority, SessionAuthorization};
use tilo_moq::video::{Codec, Layer, VideoObject};

const TAB: u64 = 7;

fn call() -> String {
    "e".repeat(64)
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

/// The host publishes `a`; each member publishes its own and reads `a`.
struct Members;
impl RelayAuthority for Members {
    fn admit(&self, proof: &[u8]) -> Result<Arc<dyn SessionAuthorization>, Denied> {
        let own = match proof {
            b"host" => 'a',
            b"first" => 'b',
            b"second" => 'c',
            b"third" => 'd',
            _ => return Err(Denied),
        };
        Ok(Arc::new(Permit {
            publish: name(own),
            subscribe: vec![name('a')],
        }))
    }
}

struct Owner;
impl ClientAuthority for Owner {
    fn current(&self, _: &ConnectionScope, _: Direction, _: &str) -> bool {
        true
    }
}

fn start(relay: &LoopbackRelay, proof: &[u8], own: char) -> Client {
    Client::start(
        Connection {
            scope: ConnectionScope {
                endpoint: relay.endpoint.clone(),
                fingerprints: relay.fingerprints.clone(),
                owner_epoch: [1; 32],
                carrier_generation: 1,
                publication: Some(name(own)),
                subscription_prefix: Some(call()),
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

fn picture(sequence: u64, keyframe: bool) -> VideoObject {
    VideoObject {
        keyframe,
        codec: Codec::H264,
        layer: Layer::High,
        sequence,
        timestamp_us: sequence * 33_333,
        width: 640,
        height: 360,
        payload: vec![0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }],
    }
}

/// A reader showing `TAB` until it has a picture that starts at a keyframe;
/// the host answers each keyframe it owes, as a window's share does.
async fn reader_gets_a_keyframe(host: &Client, reader: &Client, sequence: &mut u64, what: &str) {
    reader.show_windows(&name('a'), &[TAB]);
    tokio::time::timeout(tilo_test_env::hang_bound(), async {
        loop {
            for tab in host.take_window_keyframes() {
                assert_eq!(tab, TAB);
                let _ = host.send_window(TAB, &picture(*sequence, true));
                *sequence += 1;
            }
            while let Some(received) = reader.try_recv_picture() {
                assert_eq!(received.window, Some(TAB));
                if received.object.keyframe {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}: no keyframe reached the reader"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_member_who_starts_showing_a_window_gets_a_keyframe_of_it() {
    let relay = LoopbackRelay::start(Arc::new(Members)).unwrap();
    let host = start(&relay, b"host", 'a');
    let first = start(&relay, b"first", 'b');
    let second = start(&relay, b"second", 'c');
    let third = start(&relay, b"third", 'd');
    until("all live", || {
        [&host, &first, &second, &third]
            .iter()
            .all(|client| client.status() == Status::Live)
    })
    .await;
    assert!(
        host.wanted_windows().is_empty(),
        "nobody shows a window yet"
    );

    let mut sequence = 0;
    reader_gets_a_keyframe(&host, &first, &mut sequence, "the first reader").await;
    assert_eq!(host.wanted_windows(), [TAB]);
    // The window changes for a while.
    for _ in 0..5 {
        let _ = host.send_window(TAB, &picture(sequence, false));
        sequence += 1;
    }
    // A second member starts showing it at once, a third after it went
    // still for longer than a picture stays fresh.
    reader_gets_a_keyframe(
        &host,
        &second,
        &mut sequence,
        "a reader joining a live window",
    )
    .await;
    // The window changes for a long while, then goes still: a reader that
    // starts on it then is handed its whole newest group, and holds it.
    for _ in 0..90 {
        let _ = host.send_window(TAB, &picture(sequence, false));
        sequence += 1;
    }
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    reader_gets_a_keyframe(
        &host,
        &third,
        &mut sequence,
        "a reader joining a still window",
    )
    .await;

    // Nobody new: nothing is owed.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        host.take_window_keyframes().is_empty(),
        "no keyframe is owed while nobody new starts showing the window"
    );
    for reader in [&first, &second, &third] {
        reader.show_windows(&name('a'), &[]);
    }
    until("nobody shows the window: it is no longer asked for", || {
        host.wanted_windows().is_empty()
    })
    .await;
}
