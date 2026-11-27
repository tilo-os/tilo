//! The files mount serves the workspace this device's session shows, not the one the device was
//! founded with: a session whose workspace differs from the device's own finds its files under
//! `.workspaces/<its id>`, and a file written there is the workspace's.
//!
//! The daemon is the shipped `fs serve`, run as a device runs it, with a stand-in for its device
//! daemon and for the workspace service that serves the trees. The service answers the workspace
//! the session shows, as a device's daemon keeps it in the session's record; the mount names that
//! workspace when it opens its tree.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::io::{BufRead as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tilo_fs::tree_sync::remote::{Ask, FRAMING, Told};
use tilo_fs::tree_sync::{Reading, Wake};
use tilo_server_protocol::DocumentAnswer;
use tilo_test_env::SpawnUnderEnvLock as _;

/// The workspace the session shows: a work workspace, minted on the device.
const SESSION_WORKSPACE: &str = "207953f8-1cd4-47a9-9b7e-c0c49d69934d";

/// How long the mount may take to answer as a mount.
const MOUNT_WITHIN: Duration = Duration::from_secs(60);
/// How long the workspace's folder may take to show.
const FOLDER_WITHIN: Duration = Duration::from_secs(60);

/// A certified device of a person, written under `root`, as a machine that joined leaves it.
fn certified_identity(root: &Path) -> [u8; 32] {
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
    let root_key = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
    let paths = tilo_device_identity::IdentityPaths::under(root);
    paths.ensure_directories().expect("identity directories");
    let person = root_key.verifying_key().to_bytes();
    tilo_device_identity::persist::write_public_key(&paths.user_pubkey, &person)
        .expect("write person");
    let mut cert = DeviceCert {
        user_public_key: person,
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "fixture".into(),
        capabilities: DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root_key).expect("sign the device");
    tilo_device_identity::persist::write_device_cert(&paths.device_cert_for_user(&person), &cert)
        .expect("write the certificate");
    person
}

/// The device's daemon stand-in at `socket`, answering `info` with the fixture device's key, and
/// stopping when dropped.
struct DeviceDaemon {
    socket: PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DeviceDaemon {
    fn start(socket: PathBuf) -> Self {
        let device = ed25519_dalek::SigningKey::from_bytes(&[52; 32])
            .verifying_key()
            .to_bytes();
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("daemon socket");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = std::sync::Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                if stopping.load(Ordering::Relaxed) {
                    return;
                }
                let mut line = String::new();
                let _ = std::io::BufReader::new(stream.try_clone().expect("daemon stream"))
                    .read_line(&mut line);
                let answer = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "content": [{"type": "text", "text": ""}],
                        "structuredContent": {
                            "status": "ok",
                            "result": {"device": hex::encode(device)},
                        },
                    },
                });
                let _ = writeln!(stream, "{answer}");
            }
        });
        Self {
            socket,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for DeviceDaemon {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::os::unix::net::UnixStream::connect(&self.socket);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The device's workspace service stand-in: it answers the workspace the session shows, and
/// serves an empty tree for it, as a workspace this device has not written to yet does.
fn serve_trees(socket: PathBuf, hello: mpsc::Sender<String>) {
    std::thread::spawn(move || {
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("trees socket");
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let mut out = stream.try_clone().expect("a writer for the daemon");
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("a reader"));
            loop {
                let asked = FRAMING.read_value::<_, Ask>(&mut reader);
                let Ok(asked) = asked else { break };
                let Some(asked) = asked else { break };
                let told = match asked {
                    Ask::Shown => Some(Told::Shown(Some(SESSION_WORKSPACE.to_owned()))),
                    Ask::Hello { workspace } => {
                        let _ = hello.send(workspace);
                        None
                    }
                    Ask::Open(chunk) => Some(Told::Wake(Wake::Opened {
                        chunk,
                        answer: DocumentAnswer::Sent { frames: 0 },
                    })),
                    Ask::Read { id, .. } => Some(Told::Read {
                        id,
                        reading: Ok(Reading::Absent),
                    }),
                    Ask::Served { id, .. } => Some(Told::Served { id, served: Ok(()) }),
                    Ask::Want { id, .. } => Some(Told::Content { id, bytes: None }),
                    _ => None,
                };
                if let Some(told) = told
                    && FRAMING.write_value(&mut out, &told).is_err()
                {
                    break;
                }
            }
        }
    });
}

/// The daemon the test runs, and the mount it leaves: both go when the test ends.
struct Mounted {
    daemon: Child,
    mount: PathBuf,
}

impl Drop for Mounted {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = Command::new("fusermount3")
            .args(["-u", "-z"])
            .arg(&self.mount)
            .status();
    }
}

fn wait_until(what: &str, within: Duration, check: impl Fn() -> bool) {
    let started = Instant::now();
    while !check() {
        assert!(
            started.elapsed() < within,
            "{what} did not come within {within:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_mount_serves_the_workspace_the_session_shows() {
    tilo_test_env::require_capability!(
        Fuse,
        Path::new("/dev/fuse").exists(),
        "requires /dev/fuse and fusermount3"
    );
    let dir = tempfile::tempdir().expect("isolated fixture");
    let root = dir.path();
    let identity = root.join("identity");
    let person = certified_identity(&identity);
    let own = tilo_action_policy::roles::person_workspace_id(&person);
    assert_ne!(own, SESSION_WORKSPACE, "the session shows a work workspace");
    let run = root.join("run");
    std::fs::create_dir_all(run.join("tilo")).expect("the daemon's sockets");
    let state = root.join("state");
    std::fs::create_dir_all(&state).expect("the mount's state");
    let mount = root.join("mount");
    std::fs::create_dir_all(&mount).expect("the mount point");
    // What a session sends a mount whose person has write access to the workspace's folder by
    // their role: the folder bounded, and the person granted inside it
    // (`sessions_list` in the daemon's own tests).
    let acl = root.join("fs-capabilities.json");
    {
        use collab_types::caps::{Access, CapOverride, Effect, FsAuthority, Scope};
        use tilo_action_policy::{Grantee, Role};
        let scope = Scope::FsPath(format!("/var/workspace/.workspaces/{SESSION_WORKSPACE}"));
        let capabilities = vec![
            CapOverride::new(
                Grantee::role(Role::Viewer),
                scope.clone(),
                Effect::WorkspaceFileBoundary,
                Access::Read,
            ),
            CapOverride::new(
                Grantee::person(hex::encode(person)),
                scope,
                Effect::WorkspaceFileAccess,
                Access::Write,
            ),
        ];
        std::fs::write(
            &acl,
            serde_json::to_vec(&FsAuthority { capabilities }).expect("the access list"),
        )
        .expect("the session's access list");
    }
    let native = root.join("native.sock");
    let _device = DeviceDaemon::start(native.clone());
    let (hello, heard) = mpsc::channel();
    serve_trees(run.join("tilo/files-tree.sock"), hello);

    let daemon = Command::new(tilo_test_env::package_binary!("fs"))
        .args(["serve", "--mount-point"])
        .arg(&mount)
        .args(["--cache-dir"])
        .arg(&state)
        .args(["--blob-sock"])
        .arg(run.join("fs-blobs.sock"))
        .args(["--acl"])
        .arg(&acl)
        .args(["--backend", "fuse"])
        .env("HOME", root)
        .env("XDG_STATE_HOME", root.join("xdg-state"))
        .env("XDG_RUNTIME_DIR", &run)
        .env("TILO_IDENTITY_ROOT", &identity)
        .env("TILO_DAEMON_NATIVE", &native)
        .env("RUST_LOG", "warn,tilo_fs=info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn_locked()
        .expect("run the shipped Files daemon");
    let _daemon = Mounted {
        daemon,
        mount: mount.clone(),
    };

    let outside = std::fs::metadata(root).expect("the fixture dir").dev();
    wait_until("the mount to answer", MOUNT_WITHIN, || {
        std::fs::metadata(&mount).is_ok_and(|meta| meta.dev() != outside)
    });
    // The daemon names the workspace the session shows, not the device's own.
    let named = heard
        .recv_timeout(MOUNT_WITHIN)
        .expect("the daemon names the tree it opens");
    assert_eq!(named, SESSION_WORKSPACE);
    let folder = mount.join(".workspaces").join(SESSION_WORKSPACE);
    wait_until("the workspace's folder in the mount", FOLDER_WITHIN, || {
        folder.is_dir()
    });
    assert!(
        !mount.join(".workspaces").join(&own).exists(),
        "the device's own workspace is not the session's, and is not served"
    );
    // A file written where the person's files of the workspace are is the workspace's tree.
    let file = folder.join("Lisbon.md");
    std::fs::write(&file, b"# Lisbon in May\n").expect("write the workspace's file");
    assert_eq!(
        std::fs::read(&file).expect("read it back"),
        b"# Lisbon in May\n"
    );
    assert!(
        folder.join("Lisbon.md").is_file(),
        "the mount lists the file the workspace holds"
    );
}
