//! Actual kernel peer-process observation; this does not fabricate launcher admission.
use super::*;
use std::sync::atomic::AtomicUsize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn native_media_kernel_peer_death_fences_even_while_descendant_retains_socket() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("peer.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let script = r"import os, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall(b'R')
s.recv(1)
if os.fork() == 0:
    s.sendall(b'H')
    s.settimeout(5)
    try: s.recv(1)
    except TimeoutError: pass
os._exit(0)
";
    let mut child = std::process::Command::new("python3")
        .args(["-c", script])
        .arg(&socket)
        .spawn()
        .expect(
            "Linux kernel qualification requires python3 for the real socket descendant fixture",
        );
    let (mut stream, _) = tilo_test_env::within("`listener.accept()`", listener.accept())
        .await
        .unwrap();
    assert_eq!(
        u32::try_from(stream.peer_cred().unwrap().pid().unwrap()).unwrap(),
        child.id()
    );
    let mut byte = [0u8; 1];
    stream.read_exact(&mut byte).await.unwrap();
    assert_eq!(byte, *b"R");
    let process = PeerProcess::from_socket(&stream).expect("native media qualification requires actual Linux SO_PEERPIDFD support; no numeric PID fallback");
    assert!(process.alive());
    let stopped = Arc::new(AtomicUsize::new(0));
    let observed = stopped.clone();
    let watcher = watch_death(
        process.clone(),
        Arc::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();
    stream.write_all(b"G").await.unwrap();
    tilo_test_env::within("`watcher`", watcher).await.unwrap();
    assert_eq!(stopped.load(Ordering::SeqCst), 1);
    assert!(!process.alive());
    stream.read_exact(&mut byte).await.unwrap();
    assert_eq!(
        byte, *b"H",
        "descendant still owns the connected socket after exact peer task death"
    );
    stream.write_all(b"X").await.unwrap();
    assert!(child.wait().unwrap().success());
}

#[tokio::test]
async fn native_media_executing_peer_death_fences_while_original_launcher_lives() {
    let directory = tempfile::tempdir().expect("private socket directory");
    let socket = directory.path().join("executing.sock");
    let listener = tokio::net::UnixListener::bind(&socket).expect("service listener");
    let (root, _retained) = std::os::unix::net::UnixStream::pair().expect("original launch socket");
    root.set_nonblocking(true)
        .expect("async original launch socket");
    let root = tokio::net::UnixStream::from_std(root).expect("original launch transport");
    let launcher = PeerProcess::from_socket(&root).expect("kernel original launch pidfd");
    let script = r"import array, os, socket, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.recv(1)
a, b = socket.socketpair()
s.sendmsg([bytes([51]) * 32], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, array.array('i', [b.fileno()]))])
s.recv(1)
if os.fork() == 0:
    s.sendall(b'H')
    s.settimeout(5)
    try: s.recv(1)
    except TimeoutError: pass
os._exit(0)
";
    let mut child = std::process::Command::new("python3")
        .args(["-c", script])
        .arg(&socket)
        .spawn()
        .expect("actual executing child");
    let (mut stream, _) = tilo_test_env::within("executing child accept", listener.accept())
        .await
        .expect("child service");
    runtime_portal::process_binding::arm(&stream).expect("kernel sender credentials");
    stream.write_all(b"G").await.expect("proof go");
    let mut receiver = runtime_portal::process_binding::Receiver::new(
        [51; 32],
        rustix::process::geteuid().as_raw(),
    );
    let peer = loop {
        if let Some(peer) = stream
            .async_io(tokio::io::Interest::READABLE, || receiver.receive(&stream))
            .await
            .expect("kernel proof")
        {
            break peer;
        }
    };
    assert_eq!(peer.pid(), child.id());
    let process = launcher
        .clone()
        .with_executing(peer)
        .expect("two exact live process handles");
    let stopped = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&stopped);
    let watcher = watch_death(
        process.clone(),
        Arc::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .expect("bounded death watcher");
    stream.write_all(b"X").await.expect("child exit go");
    tilo_test_env::within("exact executing death watcher", watcher)
        .await
        .expect("watcher completion");
    assert_eq!(stopped.load(Ordering::SeqCst), 1);
    assert!(
        !process.alive(),
        "the launcher's live pidfd cannot revive executing custody"
    );
    assert!(launcher.alive(), "the original launcher remains alive");
    let mut byte = [0];
    stream
        .read_exact(&mut byte)
        .await
        .expect("retaining descendant");
    assert_eq!(byte, *b"H");
    stream
        .write_all(b"X")
        .await
        .expect("retaining descendant exit");
    assert!(
        child
            .wait()
            .expect("reap actual executing process")
            .success()
    );
}
