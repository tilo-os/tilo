//! A session bus of the test's own, as `dbus-run-session` starts one, stopped
//! on drop, so the code under test never reaches the caller's real bus.
//!
//! The daemon gets an explicit configuration instead of `--session`: the
//! session configuration expands the standard service directories, which reads
//! the caller's home directory out of the user database, and a test image may
//! run the tests as a user ID that database does not know. The daemon then
//! exits before it prints an address.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test support: failing to set up the fixture is the test's answer"
)]

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use tempfile::TempDir;

/// The daemon's policy when it is not the session bus of a real desktop: any
/// connection may own names and monitor messages, as on a session bus.
const CONFIG: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path=SOCKET</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

/// A `dbus-daemon` serving one socket, killed and reaped on drop.
pub struct PrivateBus {
    daemon: Child,
    address: String,
    socket: PathBuf,
    /// Holds the configuration, and the socket too unless the caller named it.
    _directory: TempDir,
}

impl PrivateBus {
    /// A bus whose socket is a file in a temporary directory of its own.
    pub fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        Self::start_in(directory, None)
    }

    /// A bus listening on `socket`, for a test whose subject is that path.
    pub fn start_at(socket: &Path) -> Self {
        let directory = tempfile::tempdir().unwrap();
        Self::start_in(directory, Some(socket))
    }

    fn start_in(directory: TempDir, socket: Option<&Path>) -> Self {
        let socket = socket.map_or_else(|| directory.path().join("bus"), Path::to_path_buf);
        let config = directory.path().join("bus.conf");
        std::fs::write(
            &config,
            CONFIG.replace("SOCKET", &socket.display().to_string()),
        )
        .unwrap();
        let mut daemon = Command::new("dbus-daemon")
            .arg("--config-file")
            .arg(&config)
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("the session bus needs dbus-daemon, which the builder image installs");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty(), "dbus-daemon printed no address");
        Self {
            daemon,
            address: address.trim().to_owned(),
            socket,
            _directory: directory,
        }
    }

    /// What a client passes as `DBUS_SESSION_BUS_ADDRESS`.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The socket the daemon listens on, which a test that binds that path
    /// itself has to keep clear.
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        // A killed daemon leaves its socket behind, and a test that binds a
        // real path starts by checking that nothing is there.
        let _ = std::fs::remove_file(&self.socket);
    }
}
