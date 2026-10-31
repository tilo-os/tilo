//! The VPN kill switch: a `NetworkManager` VPN profile the person set a
//! kill switch on, which is set to connect, drops. Nothing leaves this
//! device while it is down, and what Settings shows next to the profile's
//! switch says so.
//!
//! The profile and the helper are a test's: `NetworkManager` hands the
//! reading in, and the root helper is a socket that records what it was
//! asked and answers as the real one does.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use device_vpn::profile_block::{Sources, Vpns};
use device_vpn::protocol::{Envelope, PROTOCOL_VERSION, Request};
use settings_store::device_vpn::{
    DeviceVpnStatus, PROFILE_KILL_SWITCHES, ProfileBlockStatus, ProfileKillSwitch, status_path,
};
use tilo_compositor::OutputConfig;
use tilo_nm::{ActiveState, ActiveVpn, VpnReading, VpnWatch};
use tilo_session::{KillSwitch, Options, Session};

const PROFILE: &str = "9b1c6f42-7c0a-4b0f-9a1c-2d3e4f5a6b7c";
const WIDTH: i32 = 1024;
const HEIGHT: i32 = 700;

/// The helper as a test knows it: the requests it was asked, in order.
#[derive(Clone, Default)]
struct Helper {
    asked: Arc<Mutex<Vec<Request>>>,
}

impl Helper {
    /// A helper listening at `socket`, answering every request as carried
    /// out.
    fn at(socket: &Path) -> Self {
        let listener = UnixListener::bind(socket).expect("the helper listens");
        let helper = Self::default();
        let asked = Arc::clone(&helper.asked);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let envelope: Envelope = serde_json::from_str(&line).expect("a request");
                assert_eq!(envelope.protocol, PROTOCOL_VERSION);
                asked.lock().unwrap().push(envelope.request);
                let mut stream = stream;
                stream
                    .write_all(b"{\"result\":\"done\"}\n")
                    .expect("the answer");
            }
        });
        helper
    }

    /// Whether a block was asked for that keeps the traffic that is not the
    /// profile's own in.
    fn holds_a_block(&self) -> Option<bool> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .find_map(|request| match request {
                Request::HoldProfileBlock { block } => Some(block.allow_local),
                _ => None,
            })
    }
}

/// What `NetworkManager` says: the profile up, then gone.
fn reading(state: Option<ActiveState>) -> VpnReading {
    VpnReading {
        known: true,
        active: state.map_or_else(Vec::new, |state| {
            vec![ActiveVpn {
                uuid: PROFILE.to_owned(),
                state,
            }]
        }),
        ended_on_purpose: BTreeMap::new(),
    }
}

/// Wait for `condition`, or fail saying what was waited for.
fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(20))
        .expect("a deadline");
    while !condition() {
        assert!(Instant::now() < deadline, "waited for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What Settings reads: the block a running session published, read as the
/// VPN pane reads it, so a status Settings would take for a dead session's
/// is one this does not see either.
fn published() -> Option<ProfileBlockStatus> {
    DeviceVpnStatus::load().map(|status| status.profile_block)
}

/// The person turned the profile's kill switch on and it drops: the helper
/// is told to block everything but the profile's servers, and what Settings
/// shows says the profile blocks until it is back.
#[test]
fn a_profile_that_drops_blocks_traffic_and_says_so() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    PROFILE_KILL_SWITCHES
        .set(BTreeMap::from([(
            PROFILE.to_owned(),
            ProfileKillSwitch { allow_local: false },
        )]))
        .expect("the kill switch is on");
    let runtime = tilo_test_env::runtime_tempdir().expect("a runtime directory");
    let socket: PathBuf = runtime.path().join("helper.sock");
    let helper = Helper::at(&socket);

    let (sender, receiver) = tokio::sync::watch::channel(reading(Some(ActiveState::Activated)));
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            kill_switch: Some(KillSwitch::with(Sources {
                helper: socket,
                vpns: Vpns::Fed(VpnWatch::following(receiver)),
            })),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("a frame");
    assert!(status_path().is_some(), "this session can publish");
    wait_for("the session to publish its status", || {
        published().is_some()
    });
    assert_eq!(
        published(),
        Some(ProfileBlockStatus::default()),
        "at first the profile is up: nothing blocks, and Settings knows the session runs"
    );
    assert_eq!(helper.holds_a_block(), None, "so the helper is not asked");

    // It drops: `NetworkManager` says it is gone, and the person did not
    // turn it off.
    sender.send(reading(None)).expect("the reading");

    wait_for("the helper to hold the block", || {
        helper.holds_a_block().is_some()
    });
    assert_eq!(
        helper.holds_a_block(),
        Some(false),
        "everything but the tunnel's own traffic is blocked"
    );
    wait_for("Settings to see what blocks", || {
        published().is_some_and(|block| block.blocking == [PROFILE])
    });
    let block = published().expect("a published block");
    assert!(
        !block.failed,
        "the helper holds it, so the device is not left open: {block:?}"
    );

    // The profile comes back: the block lifts and Settings stops saying it
    // blocks.
    sender
        .send(reading(Some(ActiveState::Activated)))
        .expect("the reading");
    wait_for("the block to lift", || {
        helper
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, Request::ReleaseProfileBlock))
    });
    wait_for("Settings to see the block lifted", || {
        published() == Some(ProfileBlockStatus::default())
    });
}
