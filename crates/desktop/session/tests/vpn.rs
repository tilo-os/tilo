//! The device VPN: a `NetworkManager` VPN profile the person set a kill switch on, which is set to
//! connect, drops — nothing leaves this device while it is down, and what Settings shows next to
//! the profile's switch says so.
//! The profile, the helper and the other device are a test's: `NetworkManager` hands the reading
//! in, and the root helper is a socket that records what it was asked and answers as the real one
//! does.
//!
//! The route through another of the person's devices, and this device as an exit for theirs, run
//! in the device's daemon, which holds the endpoint and the key: their tests are the daemon's own
//! (`crates/os/daemon/tests/language/device_vpn.rs`), and this session's half of them is covered
//! by `crate::vpn`'s own test.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use device_vpn::profile_block::{Sources, Vpns};
use device_vpn::protocol::{Envelope, PROTOCOL_VERSION, Request};
use settings_store::device_vpn::{
    PROFILE_KILL_SWITCHES, ProfileBlockStatus, ProfileKillSwitch, status_path,
};
use tilo_compositor::OutputConfig;
use tilo_nm::{ActiveState, ActiveVpn, VpnReading, VpnWatch};
use tilo_session::{Options, Vpn};

const PROFILE: &str = "9b1c6f42-7c0a-4b0f-9a1c-2d3e4f5a6b7c";
const WIDTH: i32 = 1024;
const HEIGHT: i32 = 700;

/// The helper as a test knows it: the requests it was asked, in order.
#[derive(Clone, Default)]
struct Helper {
    asked: Arc<Mutex<Vec<Request>>>,
}

impl Helper {
    /// A helper listening at `socket` , answering every request as carried out.
    fn at(socket: &Path) -> Self {
        let listener = std::os::unix::net::UnixListener::bind(socket).expect("the helper listens");
        let helper = Self::default();
        let asked = Arc::clone(&helper.asked);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut reader = BufReader::new(stream.try_clone().expect("a stream"));
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let envelope: Envelope = serde_json::from_str(&line).expect("a request");
                assert_eq!(envelope.protocol, PROTOCOL_VERSION);
                asked.lock().unwrap().push(envelope.request);
                std::io::Write::write_all(&mut stream, b"{\"result\":\"done\"}\n")
                    .expect("the answer");
            }
        });
        helper
    }

    /// Whether a block was asked for that keeps the traffic that is not the profile's own in.
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

/// What Settings shows next to the profiles' switches.
fn published() -> Option<ProfileBlockStatus> {
    settings_store::device_vpn::DeviceVpnStatus::load().map(|status| status.profile_block)
}

/// The person turned the profile's kill switch on and it drops: the helper is told to block
/// everything but the profile's servers, and what Settings shows says the profile blocks until it
/// is back.
#[test]
fn a_profile_that_drops_blocks_traffic_and_says_so() {
    let _home = tilo_test_env::TestHome::new().expect("a home of its own");
    let runtime = tilo_test_env::runtime_tempdir().expect("a runtime directory");
    let socket: PathBuf = runtime.path().join("helper.sock");
    let helper = Helper::at(&socket);

    let (sender, receiver) = tokio::sync::watch::channel(reading(Some(ActiveState::Activated)));
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            collab: false,
            vpn: Some(Vpn::with(Sources {
                helper: socket,
                vpns: Vpns::Fed(VpnWatch::following(receiver)),
            })),
            ..Options::default()
        },
    );
    // The settings are the session's own: it installs their source as it starts, and writes a
    // setting through it rather than through a desktop's socket, which a test has none of.
    PROFILE_KILL_SWITCHES
        .set(BTreeMap::from([(
            PROFILE.to_owned(),
            ProfileKillSwitch { allow_local: false },
        )]))
        .expect("the kill switch is on");
    session.step(Duration::from_millis(1)).expect("a frame");
    assert!(status_path().is_some(), "this session can publish");
    tilo_test_env::wait_until("the session to publish its status", || {
        published().is_some()
    });
    assert_eq!(
        published(),
        Some(ProfileBlockStatus::default()),
        "at first the profile is up: nothing blocks, and Settings knows the session runs"
    );
    assert_eq!(helper.holds_a_block(), None, "so the helper is not asked");

    // It drops: `NetworkManager` says it is gone, and the person did not turn it off.
    sender.send(reading(None)).expect("the reading");

    tilo_test_env::wait_until("the helper to hold the block", || {
        helper.holds_a_block().is_some()
    });
    assert_eq!(
        helper.holds_a_block(),
        Some(false),
        "everything but the tunnel's own traffic is blocked"
    );
    tilo_test_env::wait_until("Settings to see what blocks", || {
        published().is_some_and(|block| block.blocking == [PROFILE])
    });
    let block = published().expect("a published block");
    assert!(
        !block.failed,
        "the helper holds it, so the device is not left open: {block:?}"
    );

    // The profile comes back: the block lifts and Settings stops saying it blocks.
    sender
        .send(reading(Some(ActiveState::Activated)))
        .expect("the reading");
    tilo_test_env::wait_until("the block to lift", || {
        helper
            .asked
            .lock()
            .unwrap()
            .iter()
            .any(|request| matches!(request, Request::ReleaseProfileBlock))
    });
    tilo_test_env::wait_until("Settings to see the block lifted", || {
        published() == Some(ProfileBlockStatus::default())
    });
    drop(session);
}
