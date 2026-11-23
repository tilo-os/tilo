//! A session that runs inside another desktop reaches no machine of it
//! (`tilo_paths::machine`).
//!
//! `dist/dev/nested-session.sh` starts its session with `TILO_MACHINE=hosted`,
//! and this is what that one declaration buys: the services that act on the
//! machine the session runs on build no client for that machine's daemons, and
//! so never reach the host's. Before the switch existed, the VPN kill switch
//! read the machine's real helper socket at
//! `/run/tilo-device-vpn/control.sock`, which answered `Refused`, and it asked
//! again every few seconds for the rest of the run.
//!
//! This is the fast, in-process form of the headless run
//! (`dist/dev/nested-session.sh --headless --software`, whose log carries none
//! of these attempts). The first test uses only API the tree had before the
//! switch, so it runs — and fails — against that tree too: with
//! `TILO_MACHINE=hosted`, `Sources::system()` named the host's helper and
//! followed the machine's `NetworkManager` before; now it names none and
//! follows none.

#![cfg(target_os = "linux")]

use device_vpn::profile_block::{Sources, Vpns};

/// The machine's own helper socket, which a session that owns the machine
/// reads.
const HOST_HELPER: &str = "/run/tilo-device-vpn/control.sock";

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
}

/// A session told it is hosted names no helper of the host's and follows no
/// `NetworkManager`, so the kill switch never reaches the host's daemons. The
/// assertions here are the ones that failed before the switch and pass after
/// it, and they compile against either tree.
#[test]
fn a_hosted_session_names_no_host_helper_and_follows_no_networkmanager() {
    // The switch is read from the process's environment, so this test has the process to itself:
    // no other test sees it, and it sees no other test's.
    tilo_test_env::in_own_process(
        module_path!(),
        "a_hosted_session_names_no_host_helper_and_follows_no_networkmanager",
        || {
            let mut env = tilo_test_env::ScopedEnv::new();
            // Exactly what `dist/dev/nested-session.sh` declares.
            env.set("TILO_MACHINE", "hosted");

            let sources = Sources::system();
            assert_ne!(
                sources.helper,
                std::path::PathBuf::from(HOST_HELPER),
                "a hosted session names no host helper socket (it had {})",
                sources.helper.display()
            );
            assert!(
                !matches!(sources.vpns, Vpns::System),
                "a hosted session follows no NetworkManager"
            );
        },
    );
}

/// A session that owns its machine is unchanged, under the switch saying
/// `owned` and under a session that says nothing at all: it names the machine's
/// helper and follows its `NetworkManager`.
#[test]
fn an_owned_session_keeps_its_machine() {
    tilo_test_env::in_own_process(module_path!(), "an_owned_session_keeps_its_machine", || {
        for value in [None, Some("owned")] {
            let mut env = tilo_test_env::ScopedEnv::new();
            match value {
                Some(value) => {
                    env.set("TILO_MACHINE", value);
                }
                None => {
                    env.remove("TILO_MACHINE");
                }
            }
            let sources = Sources::system();
            assert_eq!(
                sources.helper,
                std::path::PathBuf::from(HOST_HELPER),
                "an owned session asks the machine's helper ({value:?})"
            );
            assert!(
                matches!(sources.vpns, Vpns::System),
                "an owned session follows the machine's NetworkManager ({value:?})"
            );
        }
    });
}

/// The client that would reach the machine's daemons refuses to construct at
/// all in a hosted session — the one choke point every caller of the system bus
/// for those services shares: `NetworkManager`, `BlueZ` and logind build no
/// connection, whether or not a system bus happens to answer.
#[test]
fn a_hosted_session_builds_no_client_for_the_machines_daemons() {
    tilo_test_env::in_own_process(
        module_path!(),
        "a_hosted_session_builds_no_client_for_the_machines_daemons",
        || {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("TILO_MACHINE", "hosted");

            let refused = runtime().block_on(tilo_nm::NmClient::system());
            assert!(
                matches!(refused, Err(tilo_nm::NmError::Hosted)),
                "a hosted session reaches no NetworkManager: {refused:?}"
            );

            let refused = runtime()
                .block_on(tilo_bluez::BluezClient::connect())
                .err()
                .map(|error| error.to_string());
            assert!(refused.is_some(), "a hosted session reaches no BlueZ");

            let refused = runtime()
                .block_on(tilo_power_mgmt::LogindManager::connect())
                .err()
                .map(|error| error.to_string());
            assert!(refused.is_some(), "a hosted session holds no logind");
        },
    );
}
