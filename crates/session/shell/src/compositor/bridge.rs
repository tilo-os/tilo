//! Raw Wayland proxy retained for relay tests and compatibility experiments.
//!
//! This is **not** the production app-isolation boundary. Production accepts
//! apps on the compositor's public socket, authenticates each connection from
//! kernel `SO_PEERCRED` plus the live launcher registry, and stores package and
//! workspace attribution in `ClientState`. A byte proxy cannot tag smithay
//! clients and must never be treated as workspace authorization.
//!
//! This utility spawns a UNIX listener at `$XDG_RUNTIME_DIR/tilo-ws-<ws_id>` and,
//! for each accepted connection, opens a parallel connection to the main
//! Wayland socket and shovels bytes both ways.

use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// A bound UNIX listener that proxies a workspace's bridge socket to the
/// upstream Wayland socket.
///
/// Lifetime: created via [`BridgeSocket::bind`], started via
/// [`BridgeSocket::start`]. Drop signals the accept loop to shut down
/// (via [`Self::shutdown`]) and joins the loop's thread with a short
/// timeout before unlinking the bridge socket file on the host. The
/// accept loop itself is non-blocking, polling the shutdown flag between
/// `accept()` attempts, so nothing needs to be woken up.
pub struct BridgeSocket {
    pub workspace_id: u64,
    pub bridge_path: PathBuf,
    pub upstream: PathBuf,
    listener: UnixListener,
    /// Shutdown flag observed by the accept loop. Set in
    /// [`Self::shutdown`] / [`Drop`]; the loop bails on the next
    /// iteration.
    shutdown: Arc<AtomicBool>,
    /// `JoinHandle` for the accept-loop thread. Joined in Drop with a
    /// short timeout so workspace deletion doesn't block on a hung
    /// upstream.
    accept_thread: Option<thread::JoinHandle<()>>,
}

impl BridgeSocket {
    /// Bind a fresh listening socket for `workspace_id`.
    ///
    /// A stale socket owned by this uid is removed. Other filesystem objects
    /// and foreign-owned sockets are refused, so a predictable bridge name
    /// cannot unlink an attacker-chosen file.
    ///
    /// # Errors
    ///
    /// Returns an error if binding the UNIX listener at the bridge path
    /// fails (e.g. permission denied, or the path is occupied by something
    /// that isn't a stale socket file).
    pub fn bind(
        workspace_id: u64,
        runtime_dir: &Path,
        upstream_wayland_socket: &Path,
    ) -> io::Result<Self> {
        let bridge_path = runtime_dir.join(format!("tilo-ws-{workspace_id}"));
        if let Ok(metadata) = std::fs::symlink_metadata(&bridge_path) {
            // SAFETY: `geteuid` has no failure mode.
            let owner = unsafe { libc::geteuid() };
            if !metadata.file_type().is_socket() || metadata.uid() != owner {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "bridge path is not a stale socket owned by this uid",
                ));
            }
            std::fs::remove_file(&bridge_path)?;
        }
        let listener = UnixListener::bind(&bridge_path)?;
        std::fs::set_permissions(&bridge_path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            workspace_id,
            bridge_path,
            upstream: upstream_wayland_socket.to_path_buf(),
            listener,
            shutdown: Arc::new(AtomicBool::new(false)),
            accept_thread: None,
        })
    }

    /// Start the accept loop on a background thread.
    ///
    /// Each accepted connection spawns two relay threads (one each
    /// direction) and the accept loop returns to wait on the next client.
    /// The loop runs until the listener errors (which happens when the
    /// socket file is unlinked or the FD is closed).
    ///
    /// # Errors
    ///
    /// Returns an error if cloning the listener or switching it to
    /// non-blocking mode fails.
    pub fn start(&mut self) -> io::Result<()> {
        let listener = self.listener.try_clone()?;
        // Make the listener non-blocking so the accept loop can poll
        // the shutdown flag on a steady cadence rather than blocking
        // forever on an idle workspace.
        listener.set_nonblocking(true)?;
        let upstream = self.upstream.clone();
        let shutdown = Arc::clone(&self.shutdown);
        let handle = thread::spawn(move || {
            loop {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                match listener.accept() {
                    Ok((client, _addr)) => {
                        let Ok(upstream_stream) = UnixStream::connect(&upstream) else {
                            // Upstream connect failed — drop this client and keep
                            // serving others. Production would log here.
                            continue;
                        };
                        if relay(client, upstream_stream).is_err() {
                            // Failed to spawn relay threads; drop both ends.
                            // (Falling through here already loops back to
                            // `accept()`, so there is nothing further to do.)
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // No pending connection; sleep briefly and
                        // re-check the shutdown flag. 20ms is small
                        // enough to make `Drop` feel instant and big
                        // enough that an idle bridge doesn't burn CPU.
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => {
                        // Hard accept error generally means the listener
                        // has been closed; bail rather than spin.
                        break;
                    }
                }
            }
        });
        self.accept_thread = Some(handle);
        Ok(())
    }

    /// Signal the accept loop to exit. Idempotent. Does not wait for
    /// the thread to actually finish — see [`Drop`] for the join.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
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
        // Signal the accept loop and join its thread before unlinking the
        // socket file. Join budget (1s) is long enough for an idle
        // non-blocking accept loop to notice the flag at its 20ms
        // cadence, short enough that an unresponsive relay thread can't
        // block workspace teardown.
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = self.accept_thread.take() {
            // `Instant + Duration` can theoretically panic on overflow;
            // fall back to "now" (i.e. an immediate deadline) rather than
            // let teardown abort the session over a clock edge case.
            let join_deadline = std::time::Instant::now()
                .checked_add(Duration::from_secs(1))
                .unwrap_or_else(std::time::Instant::now);
            // Poll-join: the std `JoinHandle::join` blocks indefinitely
            // and has no timeout API, so we sample `is_finished` and
            // either join cleanly or detach if the budget is blown.
            while std::time::Instant::now() < join_deadline {
                if handle.is_finished() {
                    let _ = handle.join();
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            // If still running, drop the handle (detach). The leak is
            // bounded — the loop will exit on its next poll because
            // `shutdown` is already set.
        }
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
/// Uses [`std::io::copy`] per direction — a plain relay, not a
/// production-grade buffered one with timeouts.
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
    fn bind_creates_a_fresh_socket_and_never_unlinks_a_non_socket() {
        // Phase `bind_creates_bridge_socket_file`.
        {
            let runtime = tempfile::tempdir().unwrap();
            let upstream = runtime.path().join("wayland-0");
            // Bind-only verification: the upstream doesn't need to be a real
            // listener because we never call `start`.
            std::fs::File::create(&upstream).unwrap();

            let bridge = BridgeSocket::bind(42, runtime.path(), &upstream).unwrap();
            assert!(
                bridge.path().exists(),
                "bridge socket file should exist after bind"
            );
            let path = bridge.path().to_path_buf();
            drop(bridge);
            assert!(!path.exists(), "bridge cleans up its socket on drop");
        }
        // Phase `bind_clears_stale_socket_file`.
        {
            let runtime = tempfile::tempdir().unwrap();
            let upstream = runtime.path().join("wayland-0");
            std::fs::File::create(&upstream).unwrap();

            // Plant a stale same-uid socket at the bridge path.
            let stale_path = runtime.path().join("tilo-ws-7");
            let stale = UnixListener::bind(&stale_path).unwrap();
            drop(stale);
            assert!(stale_path.exists());

            // bind should unlink the stale and re-bind cleanly.
            let bridge = BridgeSocket::bind(7, runtime.path(), &upstream).unwrap();
            assert_eq!(bridge.path(), stale_path);
            assert!(bridge.path().exists());
        }
        // Phase `bind_never_unlinks_a_non_socket_at_the_predictable_path`.
        {
            let runtime = tempfile::tempdir().unwrap();
            let upstream = runtime.path().join("wayland-0");
            std::fs::File::create(&upstream).unwrap();
            let occupied = runtime.path().join("tilo-ws-8");
            std::fs::write(&occupied, b"keep me").unwrap();

            let error = BridgeSocket::bind(8, runtime.path(), &upstream)
                .err()
                .expect("regular file must be refused");
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(std::fs::read(&occupied).unwrap(), b"keep me");
        }
    }

    #[test]
    fn relay_round_trips_bytes_and_shutdown_joins_the_accept_loop() {
        // Phase `relay_round_trips_bytes`.
        // End-to-end round trip: upstream listener → bridge → client.
        // Bytes a client sends should hit the upstream, and bytes the
        // upstream replies with should come back to the client.
        //
        // Polls for up to 30s on each step before giving up and surfacing
        // the failure rather than hanging.
        {
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
            let deadline = tilo_test_env::hang_deadline();
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
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();

            client.write_all(b"hello").expect("client write");

            let mut reply = [0u8; 5];
            client.read_exact(&mut reply).expect("client read");
            assert_eq!(&reply, b"world");

            upstream_thread.join().expect("upstream thread");
        }
        // Phase `shutdown_flag_breaks_accept_loop`.
        // Signalling shutdown causes the accept loop to exit promptly
        // without waiting for an external client.
        {
            let runtime = tempfile::tempdir().unwrap();
            let upstream = runtime.path().join("wayland-0");
            std::fs::File::create(&upstream).unwrap();

            let mut bridge = BridgeSocket::bind(11, runtime.path(), &upstream).unwrap();
            bridge.start().unwrap();

            // Steal the handle so we can join it after signalling — we
            // don't want Drop to fire mid-test.
            let handle = bridge.accept_thread.take().expect("accept thread present");
            // Signal shutdown.
            bridge.shutdown();
            // The non-blocking accept polls every ~20ms; the thread should
            // exit well within 100ms of the flag flip.
            let deadline = tilo_test_env::hang_deadline();
            while Instant::now() < deadline {
                if handle.is_finished() {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            assert!(
                handle.is_finished(),
                "accept loop should exit within 500ms of shutdown flag"
            );
            handle.join().expect("clean join after shutdown");
        }
        // Phase `drop_joins_accept_thread_when_shutdown_signalled`.
        // Drop joins the accept-loop thread once shutdown is signalled
        // (set automatically inside Drop), within its 1s budget.
        {
            let runtime = tempfile::tempdir().unwrap();
            let upstream = runtime.path().join("wayland-0");
            std::fs::File::create(&upstream).unwrap();

            let mut bridge = BridgeSocket::bind(12, runtime.path(), &upstream).unwrap();
            bridge.start().unwrap();
            // Sanity-check the thread is alive before we drop.
            assert!(
                bridge
                    .accept_thread
                    .as_ref()
                    .is_some_and(|h| !h.is_finished()),
                "accept thread should be running before drop"
            );
            let started = Instant::now();
            drop(bridge);
            let elapsed = started.elapsed();
            // Drop's join budget is 1s; in practice the loop notices in
            // ~20ms. Cap the assertion at 900ms so we're testing the
            // happy path, not the detach fallback.
            assert!(
                elapsed < Duration::from_millis(900),
                "drop should complete promptly via join; took {elapsed:?}"
            );
        }
    }
}
