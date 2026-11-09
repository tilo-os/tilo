//! The document media a session started from a build under test serves its apps
//! (`fs serve-media`): the connector puts a picture as this device's content, and a second
//! connection — the app's window — reads it back by its address.
//!
//! This is the seam a live tilolang app's pictures travel: the connector is the process with
//! the network, and the window is the one that draws. Neither ever speaks to the image's
//! filesystem daemon: a session started from a build under test serves them this one instead
//! (`dist/dev/shell-run.sh`), because the image's daemon keeps a different wire and every call
//! to it dies on the first frame.
//!
//! Before this, no such server existed and both halves reached `/run/tilo/fs-blobs.sock`: the
//! put failed with `blob service unavailable: the filesystem connection is closed`, and every
//! still drew its placeholder.
//!
//! The server here is the shipped binary, started with the arguments the development session's
//! script starts it with, and the two callers are the launcher's app-scoped connections.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions: a failed step is the assertion"
)]

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use runtime_fs::blobs::{BlobClient, BlobPrincipal, BlobScope, ReadOutcome, write_handoff};
use tilo_test_env::SpawnUnderEnvLock as _;

/// The shipped `fs` binary, which serves a session's document media.
fn fs_binary() -> PathBuf {
    tilo_test_env::package_binary!("fs")
}

/// A session's media server at `socket`, keeping content in `store`, as
/// `dist/dev/shell-run.sh` starts one.
struct Server {
    child: Child,
    socket: PathBuf,
}

impl Server {
    fn start(root: &Path) -> Self {
        let store = root.join("content");
        let socket = root.join("fs-blobs.sock");
        let mut child = Command::new(fs_binary())
            .args(["serve-media", "--store"])
            .arg(&store)
            .arg("--blob-sock")
            .arg(&socket)
            .env_remove("TILO_FS_BLOB_SOCKET")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn_locked()
            .expect("the session's document media starts");
        // Readiness is a connection the server answers, not the file it leaves: a session
        // killed outright leaves a socket behind that no one listens on, and a later start of
        // the same path binds over it.
        let started = Instant::now();
        loop {
            if UnixStream::connect(&socket).is_ok() {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "the session's document media answers no one at {}",
                socket.display()
            );
            assert!(
                child.try_wait().expect("the server's state").is_none(),
                "the session's document media ended as it started"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        Self { child, socket }
    }

    /// A connection in `app`'s scope in `workspace`, as the launcher hands one into a sandbox.
    fn app(&self, workspace: &str, app: &str) -> BlobClient {
        let mut stream = UnixStream::connect(&self.socket).expect("the media socket answers");
        write_handoff(
            &mut stream,
            &BlobPrincipal::App(BlobScope::new(workspace, app).unwrap()),
        )
        .expect("the launcher's handoff");
        BlobClient::from_app_stream(stream).expect("the app's connection")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A picture, as the connector fetched it from `YouTube`'s CDN.
const PICTURE: &[u8] = b"\x89PNG\r\n\x1a\n a still, as the connector fetched it";

#[test]
fn a_session_s_media_server_keeps_what_the_connector_puts_for_the_window_to_read() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let server = Server::start(dir.path());

    // The connector: the app's own scope, as the launcher fixes it on the connection.
    let connector = server.app("workspace-1", "so.tilo.lang.tube");
    let kept = connector
        .put(PICTURE, "image/png")
        .expect("the connector keeps a picture as this device's content");

    // The window: a connection of its own in the same app's scope, which the answer's
    // `tilo-blob://<hash>` address is read through.
    let window = server.app("workspace-1", "so.tilo.lang.tube");
    assert_eq!(
        window
            .read_all(&kept.hash)
            .expect("the window reads the address"),
        ReadOutcome::Ready {
            bytes: PICTURE.to_vec(),
            total_len: PICTURE.len() as u64,
        },
        "the picture the connector kept is the one the window draws"
    );
}

/// The session's own process draws a place over an app's record without any app window, and the
/// launcher hands no app's capability to it: its store is the daemon's, opened as the session
/// itself and scoped to the workspaces it draws (`tilo_content_device::session`). A still the
/// app's connector kept under its own scope in that workspace is read through it, which is what
/// fills a place's cards — without it every still stayed a placeholder though the bytes were
/// kept.
#[test]
fn the_session_reads_what_the_connector_kept_through_its_own_store() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let server = Server::start(dir.path());

    // The connector, in the app's own scope: the still goes into the workspace's reachability
    // index, which is per workspace and not per app.
    let connector = server.app("workspace-1", "so.tilo.lang.tube");
    let kept = connector
        .put(PICTURE, "image/png")
        .expect("the connector keeps a still");

    // The session's process: a plain store over the daemon's socket, as the shell part's is.
    let session = session_store(&server.socket, "workspace-1")
        .expect("the session reads the workspace's content through its own media");
    assert_eq!(
        session.read(&kept.hash).as_deref(),
        Some(PICTURE),
        "a place of that workspace draws the still the connector kept"
    );

    // A workspace the still was never put in holds none of it: the session's store serves what
    // each workspace references, never another's.
    let elsewhere =
        session_store(&server.socket, "workspace-2").expect("a second workspace's media");
    assert_eq!(
        elsewhere.read(&kept.hash),
        None,
        "a workspace that never had the still reads none of it"
    );
}

/// The store a session's own process reads a workspace's pictures through: the daemon at
/// `socket`, with `workspace` one of the workspaces it draws. This is exactly what the shell part
/// is lent (`ui::content::lend_content`).
fn session_store(socket: &Path, workspace: &str) -> Option<tilo_content::ContentStore> {
    let saved = std::env::var_os("TILO_FS_BLOB_SOCKET");
    // SAFETY: the test runs alone under the environment lock every test holds.
    unsafe { std::env::set_var("TILO_FS_BLOB_SOCKET", socket) };
    tilo_content_device::set_session_workspaces(vec![workspace.to_owned()]);
    let store = tilo_content_device::session(workspace);
    match saved {
        Some(value) => {
            // SAFETY: as above.
            unsafe { std::env::set_var("TILO_FS_BLOB_SOCKET", value) };
        }
        None => {
            // SAFETY: as above.
            unsafe { std::env::remove_var("TILO_FS_BLOB_SOCKET") };
        }
    }
    store
}

#[test]
fn a_server_replaces_a_socket_a_killed_session_left_behind() {
    let dir = tempfile::tempdir().expect("a scratch directory");
    let socket = dir.path().join("fs-blobs.sock");
    {
        // A session killed outright leaves its socket file, and nothing listens behind it.
        let server = Server::start(dir.path());
        drop(server);
    }
    assert!(
        socket.exists(),
        "the killed server's socket is what a later session finds"
    );
    let server = Server::start(dir.path());
    let app = server.app("workspace-1", "so.tilo.lang.notes");
    let kept = app
        .put(b"kept after a restart", "image/png")
        .expect("a put");
    assert_eq!(
        app.read_all(&kept.hash).expect("a read"),
        ReadOutcome::Ready {
            bytes: b"kept after a restart".to_vec(),
            total_len: 20,
        }
    );
}
