//! Per-workspace Wayland bridge socket — v1.
//!
//! Spawns a UNIX listener at `$XDG_RUNTIME_DIR/nirios-ws-<ws_id>` and,
//! for each accepted connection, opens a parallel connection to the
//! main wayland socket and shovels bytes both ways. Future Phase 3+
//! work tags each bridge connection with a workspace id (smithay
//! `Client::UserData`) and filters protocol messages — v1 just gives
//! us the socket-path infrastructure so apps inside the bwrap sandbox
//! see a `WAYLAND_DISPLAY` they can bind to.
//!
//! See `docs/superpowers/plans/2026-05-02-per-workspace-sandbox-permissions.md`
//! Phase 3 (lines 231-291). The full Phase 3 implements message-filtering at
//! the bridge boundary with smithay `Client` UserData tagging — this v1
//! delivers only listen+proxy plus the spawn-helper integration so the bwrap
//! sandbox argv is wired end-to-end.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;

/// A bound UNIX listener that proxies a workspace's bridge socket to the
/// upstream Wayland socket.
///
/// Lifetime: created via [`BridgeSocket::bind`], started via
/// [`BridgeSocket::start`]. Drop unlinks the bridge socket file on the
/// host. The accept loop thread is detached on drop — v1 has no explicit
/// shutdown channel because the listener simply errors out once the file
/// is removed and the OS reclaims the FD.
pub struct BridgeSocket {
    pub workspace_id: u64,
    pub bridge_path: PathBuf,
    pub upstream: PathBuf,
    listener: UnixListener,
    /// JoinHandle for the accept-loop thread. Detached on drop; v1 has
    /// no explicit shutdown — the listener closes when its socket file
    /// is removed. Future work adds a shutdown channel.
    accept_thread: Option<thread::JoinHandle<()>>,
}

impl BridgeSocket {
    /// Bind a fresh listening socket for `workspace_id`.
    ///
    /// If a stale socket file exists at the bridge path (e.g., previous
    /// session crashed before [`Drop`] could unlink), it is removed
    /// best-effort before the bind. Failures of the unlink are swallowed
    /// because they may simply mean "no stale file" — only a real
    /// `bind(2)` failure surfaces as an error.
    pub fn bind(
        workspace_id: u64,
        runtime_dir: &Path,
        upstream_wayland_socket: &Path,
    ) -> io::Result<Self> {
        let bridge_path = runtime_dir.join(format!("nirios-ws-{}", workspace_id));
        // Best-effort cleanup of stale.
        let _ = std::fs::remove_file(&bridge_path);
        let listener = UnixListener::bind(&bridge_path)?;
        Ok(Self {
            workspace_id,
            bridge_path,
            upstream: upstream_wayland_socket.to_path_buf(),
            listener,
            accept_thread: None,
        })
    }

    /// Start the accept loop on a background thread.
    ///
    /// Each accepted connection spawns two relay threads (one each
    /// direction) and the accept loop returns to wait on the next client.
    /// The loop runs until the listener errors (which happens when the
    /// socket file is unlinked or the FD is closed).
    pub fn start(&mut self) -> io::Result<()> {
        let listener = self.listener.try_clone()?;
        let upstream = self.upstream.clone();
        let handle = thread::spawn(move || {
            for client_stream in listener.incoming() {
                let Ok(client) = client_stream else {
                    // Accept errors generally mean the listener has been
                    // shut down; bail rather than spin.
                    break;
                };
                let Ok(upstream_stream) = UnixStream::connect(&upstream) else {
                    // Upstream connect failed — drop this client and keep
                    // serving others. Production would log here.
                    continue;
                };
                if relay(client, upstream_stream).is_err() {
                    // Failed to spawn relay threads; drop both ends.
                    continue;
                }
            }
        });
        self.accept_thread = Some(handle);
        Ok(())
    }

    /// Filesystem path of the bridge socket on the host.
    ///
    /// This is the value that needs to be bind-mounted into the sandbox
    /// (and pointed at by `WAYLAND_DISPLAY`) so apps inside the sandbox
    /// can connect to the compositor without seeing the global socket.
    pub fn path(&self) -> &Path {
        &self.bridge_path
    }
}

impl Drop for BridgeSocket {
    fn drop(&mut self) {
        // Unlink the socket file so the next session's bind doesn't see
        // a stale entry. Errors here are not actionable.
        let _ = std::fs::remove_file(&self.bridge_path);
    }
}

/// Spawn two threads: one copies bytes client→upstream, one upstream→client.
///
/// Returns once both threads are spawned (does NOT wait). When either
/// thread hits EOF or error, both directions tear down naturally because
/// the dropped `UnixStream` closes its half of the connection, which
/// surfaces to the other relay thread as EOF.
///
/// v1 uses [`std::io::copy`] per direction because the bar for this
/// commit is "weston-info round-trips through the bridge", not a
/// production-grade buffered relay with timeouts.
fn relay(client: UnixStream, upstream: UnixStream) -> io::Result<()> {
    let client2 = client.try_clone()?;
    let upstream2 = upstream.try_clone()?;
    thread::spawn(move || {
        let mut c = client;
        let mut u = upstream;
        let _ = std::io::copy(&mut c, &mut u);
    });
    thread::spawn(move || {
        let mut u = upstream2;
        let mut c = client2;
        let _ = std::io::copy(&mut u, &mut c);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    #[test]
    fn bind_creates_bridge_socket_file() {
        let runtime = tempfile::tempdir().unwrap();
        let upstream = runtime.path().join("wayland-0");
        // Bind-only verification: the upstream doesn't need to be a real
        // listener because we never call `start`.
        std::fs::File::create(&upstream).unwrap();

        let bridge = BridgeSocket::bind(42, runtime.path(), &upstream).unwrap();
        assert!(bridge.path().exists(), "bridge socket file should exist after bind");
        let path = bridge.path().to_path_buf();
        drop(bridge);
        assert!(!path.exists(), "bridge cleans up its socket on drop");
    }

    #[test]
    fn bind_clears_stale_socket_file() {
        let runtime = tempfile::tempdir().unwrap();
        let upstream = runtime.path().join("wayland-0");
        std::fs::File::create(&upstream).unwrap();

        // Plant a stale file at the bridge path.
        let stale_path = runtime.path().join("nirios-ws-7");
        std::fs::write(&stale_path, b"stale").unwrap();
        assert!(stale_path.exists());

        // bind should unlink the stale and re-bind cleanly.
        let bridge = BridgeSocket::bind(7, runtime.path(), &upstream).unwrap();
        assert_eq!(bridge.path(), stale_path);
        assert!(bridge.path().exists());
    }

    /// End-to-end round trip: upstream listener → bridge → client.
    /// Bytes a client sends should hit the upstream, and bytes the
    /// upstream replies with should come back to the client.
    ///
    /// Polls for up to 30s on each step before giving up and surfacing
    /// the failure rather than hanging.
    #[test]
    fn relay_round_trips_bytes() {
        let runtime = tempfile::tempdir().unwrap();
        let upstream_path = runtime.path().join("wayland-0");

        // Bring up an upstream listener that echoes whatever it reads.
        let upstream_listener = UnixListener::bind(&upstream_path).unwrap();
        let upstream_thread = thread::spawn(move || {
            let (mut conn, _) = upstream_listener.accept().expect("upstream accept");
            let mut buf = [0u8; 5];
            conn.read_exact(&mut buf).expect("upstream read");
            assert_eq!(&buf, b"hello");
            conn.write_all(b"world").expect("upstream write");
        });

        let mut bridge = BridgeSocket::bind(99, runtime.path(), &upstream_path).unwrap();
        bridge.start().unwrap();

        // Connect through the bridge — small retry loop because the
        // accept thread may not have entered `accept` yet.
        let bridge_path = bridge.path().to_path_buf();
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut client = loop {
            match UnixStream::connect(&bridge_path) {
                Ok(s) => break s,
                Err(_) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("client connect to bridge failed: {e}"),
            }
        };

        client
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();

        client.write_all(b"hello").expect("client write");

        let mut reply = [0u8; 5];
        client.read_exact(&mut reply).expect("client read");
        assert_eq!(&reply, b"world");

        upstream_thread.join().expect("upstream thread");
    }
}
