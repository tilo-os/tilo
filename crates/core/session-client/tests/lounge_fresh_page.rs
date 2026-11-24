//! A real Lounge's volume, hosted by the device's one daemon: the image's own
//! entrypoint with the deployment's environment, on a copy of the volume the
//! operator names, proving the daemon starts as the deployment runs it, joins
//! the person the volume signed in with, and keeps the workspaces placed on it.
//!
//! It runs only when the operator names the state volume, so an ordinary test
//! run skips it:
//!
//! ```text
//! TILO_LOUNGE_STATE=/path/to/copy cargo test -p tilo-session-client \
//!   --test it lounge_fresh_page -- --nocapture
//! ```
//!
//! Its pages are the Tilo app at the deployment's web entry, which the daemon
//! serves through the server configuration's web setting; a volume that has
//! never been signed in starts nothing, so the copy must be one the deployment
//! ran.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a rehearsal fixture: a failed step is the answer, and it says which check failed"
)]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tilo_test_env::SpawnUnderEnvLock as _;

/// The Lounge image's own entrypoint in this checkout: found from the crate's own sources, which
/// the run built from, then from the test binary's place. A build whose `target/` sits outside the
/// checkout (a builder's container) has the sources only where the crate was compiled.
fn entrypoint() -> std::path::PathBuf {
    let built = std::env::current_exe().expect("the test binary's own path");
    let sources = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    sources
        .ancestors()
        .chain(built.ancestors())
        .map(|dir| dir.join("os/containers/tilo-lounge/entrypoint.sh"))
        .find(|entrypoint| entrypoint.is_file())
        .unwrap_or_else(|| {
            panic!(
                "no Lounge image entrypoint above {} or {}",
                sources.display(),
                built.display()
            )
        })
}

/// A PATH holding the daemon this run built, so the image's `exec tilo-runtime`
/// runs it.
fn runtime_on_path() -> std::ffi::OsString {
    let runtime = tilo_test_env::sibling_binary("tilo-daemon", "tilo-runtime");
    let programs = runtime
        .parent()
        .expect("the daemon's directory")
        .to_path_buf();
    let mut paths = vec![programs];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).expect("a PATH")
}

/// The Lounge, as its deployment runs it.
struct Lounge {
    child: Child,
    /// The volume: the daemon's identity, profile and state live here.
    state: tempfile::TempDir,
}

impl Drop for Lounge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Lounge {
    /// The state volume the operator names: a copy of what the pod mounts
    /// (`state/` the volume, `secrets/` the mounted secrets), or the volume
    /// alone. A plain copy of the production volume is not this test's to
    /// write to, so it is copied.
    fn volume(from: &std::path::Path) -> tempfile::TempDir {
        let snapshot = if from.join("state").is_dir() {
            from.join("state")
        } else {
            from.to_path_buf()
        };
        let state = tempfile::tempdir().expect("a state volume for the run");
        let copied = Command::new("cp")
            .arg("-a")
            .arg(format!("{}/.", snapshot.display()))
            .arg(state.path())
            .status_locked()
            .expect("cp runs");
        assert!(copied.success(), "the snapshot copies into the volume");
        state
    }

    /// The Lounge started on `state`, as the deployment starts it. A volume the
    /// deployment signed in starts as it is; one that holds no identity needs
    /// the ticket the operator hands it (`TILO_LOUNGE_TICKET`).
    fn start(from: &std::path::Path) -> Self {
        let state = Self::volume(from);
        let entrypoint = entrypoint();
        // The deployment's server configuration: its web UI and packet relay on
        // ports the kernel picks, so a rehearsal beside another binds its own.
        std::fs::write(
            state.path().join("server.toml"),
            "web_entry = \"https://tilo.sh/\"\n\
             web_listen = \"127.0.0.1:0\"\n\
             packet_relay = \"127.0.0.1:0\"\n",
        )
        .expect("the server configuration");
        let mut command = Command::new("sh");
        command
            .arg(&entrypoint)
            .env("PATH", runtime_on_path())
            .env("TILO_WORKSPACES_STATE", state.path())
            .env("TILO_WORKSPACES_WEB_ENTRY", "https://tilo.sh/")
            .env("TILO_WORKSPACES_LIVE", "127.0.0.1:0")
            .env("TILO_SERVER_CONFIG", state.path().join("server.toml"))
            .env("TILO_SECRETS_DIR", state.path().join("secrets"))
            .env("TILO_SERVER_LOG", "info")
            .env(
                "TILO_SOURCE_REVISION",
                "0123456789abcdef0123456789abcdef01234567",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        if let Some(ticket) = std::env::var_os("TILO_LOUNGE_TICKET") {
            command.env("TILO_TICKET", ticket);
        }
        let child = command.spawn_locked().expect("the Lounge starts");
        Self { child, state }
    }

    /// Wait for the daemon to answer on its native endpoint: the device's
    /// actions are served there from its start, before anyone's certificate
    /// names it, and the person's services once it does.
    fn await_native(&mut self) {
        let socket = self.state.path().join("native.sock");
        let deadline = std::time::Instant::now() + Duration::from_secs(120);
        while std::time::Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().expect("the daemon's exit status") {
                panic!("the Lounge stopped before it answered (status {status})");
            }
            if socket.exists() && std::os::unix::net::UnixStream::connect(&socket).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!(
            "the Lounge did not answer on {}: is the volume one the deployment signed in?",
            socket.display()
        );
    }

    /// The server configuration the entrypoint wrote for the deployment.
    fn config(&self) -> String {
        std::fs::read_to_string(self.state.path().join("server.toml"))
            .expect("the entrypoint's server configuration")
    }

    /// The daemon's last lines, for a failure that says why it stopped.
    fn said(&self) -> String {
        let log = self.state.path().join("daemon.log");
        std::fs::read_to_string(&log).unwrap_or_default()
    }
}

#[test]
fn a_lounge_volume_runs_on_the_device_s_daemon() {
    let Some(volume) = std::env::var_os("TILO_LOUNGE_STATE") else {
        eprintln!("skipped: TILO_LOUNGE_STATE names a copy of the deployment's volume");
        return;
    };
    let mut lounge = Lounge::start(std::path::Path::new(&volume));
    lounge.await_native();
    let config = lounge.config();
    assert!(
        config.contains("web_entry = \"https://tilo.sh/\""),
        "the configuration names the deployment's web entry: {config}"
    );
    assert!(
        config.contains("packet_relay = \"127.0.0.1:0\""),
        "the configuration runs the packet relay the port picks: {config}"
    );
    // Its device identity is the volume's, which the deployment installed: the
    // entrypoint writes no certificate of its own.
    let identity = lounge.state.path().join("identity");
    assert!(
        identity.is_dir(),
        "the volume keeps the daemon's identity at {}: {}",
        identity.display(),
        lounge.said()
    );
}

#[test]
fn a_fresh_volume_says_how_a_machine_joins() {
    let state = tempfile::tempdir().expect("an empty volume");
    let entrypoint = entrypoint();
    let started = Command::new("sh")
        .arg(&entrypoint)
        .env("PATH", runtime_on_path())
        .env("TILO_WORKSPACES_STATE", state.path())
        .env("TILO_WORKSPACES_WEB_ENTRY", "https://tilo.sh/")
        .env("TILO_SERVER_CONFIG", state.path().join("server.toml"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_locked()
        .expect("the entrypoint runs");
    let output = started.wait_with_output().expect("the entrypoint's answer");
    assert_eq!(
        output.status.code(),
        Some(2),
        "an empty volume starts nothing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        said.contains("tilo device add"),
        "the refusal says how a machine joins: {said}"
    );
}
