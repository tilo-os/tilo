//! Real launcher gate and held-descriptor custody across accepted revocation.
use super::super::{Authority, Export, lock};
use super::*;
use crate::shell::state::WorkspaceTree;
use collab_types::caps::Access;
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MODE: &str = "TILO_REVOCATION_PROBE";
const ROOT: &str = "TILO_REVOCATION_ROOT";
const LIMIT: Duration = Duration::from_secs(5);

fn command(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &format!(
                "{}::network_withdrawal_closes_live_gate_fresh_connections_and_descendants",
                module_path!().split_once("::").unwrap().1
            ),
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MODE, mode)
        .env(ROOT, root)
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    command
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn bounded(stream: &UnixStream) {
    stream.set_read_timeout(Some(LIMIT)).unwrap();
    stream.set_write_timeout(Some(LIMIT)).unwrap();
}

/// Re-enter the test executable rather than putting an async gate in the
/// compositor test process: revocation must close the actual gate's sockets.
fn sandbox_process_probe(mode: &str) {
    let root = std::path::PathBuf::from(std::env::var_os(ROOT).unwrap());
    let directory = File::open(root.join("documents")).unwrap();
    let _gate = (mode == "gate").then(|| {
        launcher::netgate::NetGate::start(
            &root.join("gate.sock"),
            launcher::netgate::EgressPolicy::from_grants(["localhost"]),
        )
        .unwrap()
    });
    let _child =
        (mode != "descendant").then(|| Process(command(&root, "descendant").spawn().unwrap()));
    let mut control = UnixStream::connect(root.join("control.sock")).unwrap();
    bounded(&control);
    control
        .write_all(&std::process::id().to_ne_bytes())
        .unwrap();
    loop {
        let mut request = [0];
        if control.read_exact(&mut request).is_err() {
            break;
        }
        assert_eq!(request, *b"r");
        // openat uses the descriptor held since before publication, including
        // in the descendant. Removing a policy row alone cannot close it.
        // SAFETY: the directory fd is live and the relative name is terminated.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), c"secret".as_ptr(), libc::O_RDONLY) };
        assert!(fd >= 0, "{}", io::Error::last_os_error());
        // SAFETY: successful openat returned a fresh exclusively owned fd.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let mut byte = [0];
        file.read_exact(&mut byte).unwrap();
        control.write_all(&byte).unwrap();
    }
}

struct LiveTree {
    root: Process,
    descendant: OwnedFd,
    controls: Vec<UnixStream>,
    scratch: tempfile::TempDir,
}

impl LiveTree {
    fn start(gate: bool) -> Self {
        let scratch = tempfile::tempdir().unwrap();
        std::fs::create_dir(scratch.path().join("documents")).unwrap();
        std::fs::write(scratch.path().join("documents/secret"), b"s").unwrap();
        let listener = UnixListener::bind(scratch.path().join("control.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut root = Process(
            command(scratch.path(), if gate { "gate" } else { "directory" })
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now()
            .checked_add(LIMIT)
            .unwrap_or_else(Instant::now);
        let mut controls = Vec::new();
        let mut descendant = None;
        while controls.len() < 2 {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    bounded(&stream);
                    let mut pid = [0; 4];
                    stream.read_exact(&mut pid).unwrap();
                    let pid = u32::from_ne_bytes(pid);
                    if pid != root.0.id() {
                        let (_, start) = super::super::super::proc_stat(pid).unwrap();
                        descendant = Some(process_handle(pid, start).unwrap());
                    }
                    controls.push(stream);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(root.0.try_wait().unwrap().is_none(), "probe exited early");
                    assert!(Instant::now() < deadline, "probe readiness timed out");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("probe readiness: {error}"),
            }
        }
        Self {
            root,
            descendant: descendant.expect("descendant handshake"),
            controls,
            scratch,
        }
    }

    fn readable(&mut self) {
        assert!(self.root.0.try_wait().unwrap().is_none());
        for control in &mut self.controls {
            control.write_all(b"r").unwrap();
            let mut byte = [0];
            control.read_exact(&mut byte).unwrap();
            assert_eq!(byte, *b"s", "held directory descriptor remains usable");
        }
    }

    fn interrupted(&mut self) {
        let deadline = Instant::now()
            .checked_add(LIMIT)
            .unwrap_or_else(Instant::now);
        while self.root.0.try_wait().unwrap().is_none() {
            assert!(
                Instant::now() < deadline,
                "sandbox root survived withdrawal"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        for control in &mut self.controls {
            let mut byte = [0];
            assert_eq!(
                control.read(&mut byte).unwrap(),
                0,
                "descriptor holder survived"
            );
        }
        let mut poll = libc::pollfd {
            fd: self.descendant.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll points to one initialized pollfd with a live pidfd.
        assert_eq!(unsafe { libc::poll(&raw mut poll, 1, 5000) }, 1);
        assert_ne!(poll.revents & libc::POLLIN, 0, "descendant must exit");
    }

    fn gate_connection(&self, target: &TcpListener) -> (UnixStream, TcpStream) {
        let mut stream = UnixStream::connect(self.scratch.path().join("gate.sock")).unwrap();
        bounded(&stream);
        stream.write_all(&[5, 1, 0]).unwrap();
        let mut greeting = [0; 2];
        stream.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting, [5, 0]);
        let mut request = vec![5, 1, 0, 3, 9];
        request.extend_from_slice(b"localhost");
        request.extend_from_slice(&target.local_addr().unwrap().port().to_be_bytes());
        stream.write_all(&request).unwrap();
        let mut reply = [0; 4];
        stream.read_exact(&mut reply).unwrap();
        assert_eq!(&reply[..3], &[5, 0, 0]);
        let remaining = match reply[3] {
            1 => 6,
            4 => 18,
            other => panic!("unexpected SOCKS address type {other}"),
        };
        stream.read_exact(&mut vec![0; remaining]).unwrap();
        let deadline = Instant::now()
            .checked_add(LIMIT)
            .unwrap_or_else(Instant::now);
        let mut remote = loop {
            match target.accept() {
                Ok((remote, _)) => break remote,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "upstream accept timed out");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("upstream accept: {error}"),
            }
        };
        remote.set_read_timeout(Some(LIMIT)).unwrap();
        remote.set_write_timeout(Some(LIMIT)).unwrap();
        remote.write_all(b"n").unwrap();
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"n", "real gate forwards upstream bytes");
        (stream, remote)
    }
}

impl Drop for LiveTree {
    fn drop(&mut self) {
        signal(&self.descendant, libc::SIGKILL);
        // Process's Drop reaps the root even when an assertion panics.
    }
}

fn publish_fixture(live: &LiveTree, scope: Scope) -> (WorkspaceTree, Arc<Authority>) {
    let mut tree = WorkspaceTree::bootstrap_default();
    let room = tree.desktop.workspaces[0].room;
    let grant = CapOverride::grant(Subject::app("so.tilo.reader"), scope, Access::Write)
        .with_id("live-grant");
    tree.desktop.workspaces[0].sandbox.push(grant.clone());
    Export::of(&tree).publish();
    let authority = tree.native_runtime.action_authority();
    let mut published = lock(&authority.published);
    let sandbox = Sandbox::capture(
        live.root.0.id(),
        (room, "so.tilo.reader".into(), "person".into()),
        &[grant],
    )
    .unwrap();
    assert!(sandbox.authorization_status(&published).is_ok());
    published.sandboxes.insert(live.root.0.id(), sandbox);
    drop(published);
    (tree, authority)
}

fn add_and_reorder(tree: &mut WorkspaceTree, authority: &Authority) {
    let caps = &mut tree.desktop.workspaces[0].sandbox;
    caps.push(CapOverride::grant(
        Subject::app("so.tilo.reader"),
        Scope::NetHost("example.org".into()),
        Access::Write,
    ));
    caps.reverse();
    Export::of(tree).publish();
    assert_eq!(
        lock(&authority.published).sandboxes.len(),
        1,
        "additions are not withdrawal"
    );
}

#[test]
fn network_withdrawal_closes_live_gate_fresh_connections_and_descendants() {
    if let Ok(mode) = std::env::var(MODE) {
        sandbox_process_probe(&mode);
        return;
    }
    let mut live = LiveTree::start(true);
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    let (mut held, _upstream) = live.gate_connection(&target);
    let (mut tree, authority) = publish_fixture(&live, Scope::NetHost("localhost".into()));
    add_and_reorder(&mut tree, &authority);
    live.readable();
    // A new connection, not just the pre-publication tunnel, still works.
    let (_fresh, _fresh_upstream) = live.gate_connection(&target);
    tree.desktop.workspaces[0]
        .sandbox
        .retain(|cap| cap.id != "live-grant");
    Export::of(&tree).publish();
    assert!(lock(&authority.published).sandboxes.is_empty());
    live.interrupted();
    let mut byte = [0];
    assert_eq!(
        held.read(&mut byte).unwrap(),
        0,
        "existing tunnel must close"
    );
    assert!(
        UnixStream::connect(live.scratch.path().join("gate.sock")).is_err(),
        "a fresh connection must not reach a revoked gate"
    );
}

#[test]
fn directory_narrowing_interrupts_holders_of_directory_descriptors() {
    directory_revocation(true);
}

#[test]
fn directory_withdrawal_interrupts_holders_of_directory_descriptors() {
    directory_revocation(false);
}

fn directory_revocation(narrow: bool) {
    let mut live = LiveTree::start(false);
    let (mut tree, authority) = publish_fixture(
        &live,
        Scope::FsPath(
            live.scratch
                .path()
                .join("documents")
                .to_str()
                .unwrap()
                .into(),
        ),
    );
    live.readable();
    add_and_reorder(&mut tree, &authority);
    live.readable();
    let caps = &mut tree.desktop.workspaces[0].sandbox;
    if narrow {
        caps.push(CapOverride::new(
            Subject::app("so.tilo.reader"),
            Scope::FsPath(
                live.scratch
                    .path()
                    .join("documents/secret")
                    .to_str()
                    .unwrap()
                    .into(),
            ),
            Effect::Restrict,
            Access::Read,
        ));
    } else {
        caps.retain(|cap| cap.id != "live-grant");
    }
    Export::of(&tree).publish();
    assert!(lock(&authority.published).sandboxes.is_empty());
    live.interrupted();
}
