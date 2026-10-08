use super::*;
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;

const CHILD: &str = "TILO_EXECUTING_PEER_TEST_FD";
const TEST: &str = "process_binding::linux::tests::executing_peer_child";

fn start() -> u64 {
    process_stat(std::process::id())
        .expect("current kernel process")
        .1
}
fn challenge(socket: &mut UnixStream, nonce: &[u8; 32]) {
    arm(socket).expect("kernel credential delivery");
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("bounded receive");
    socket
        .write_all(&super::super::MAGIC)
        .expect("challenge marker");
    socket.write_all(nonce).expect("challenge nonce");
}
#[test]
fn executing_peer_child() {
    let Ok(raw) = std::env::var(CHILD) else {
        return;
    };
    let raw: i32 = raw.parse().expect("owned inherited socket");
    // SAFETY: the parent transferred this one owned socket to this child.
    let mut socket = unsafe { UnixStream::from_raw_fd(raw) };
    if std::env::var_os("TILO_EXECUTING_PEER_PACKET").is_some() {
        socket.write_all(b"different-task").expect("child packet");
        return;
    }
    if let Ok(raw) = std::env::var("TILO_EXECUTING_PEER_FORGED_PROOF") {
        let raw: i32 = raw.parse().expect("foreign socket descriptor");
        // SAFETY: the fixture transferred a separate parent-created proof.
        let proof = unsafe { UnixStream::from_raw_fd(raw) };
        let mut header = [0; 36];
        socket.read_exact(&mut header).expect("challenge");
        let nonce: [u8; 32] = header[4..].try_into().expect("nonce");
        send_answer(&socket, &nonce, &proof).expect("foreign proof sent");
        return;
    }
    complete(&mut socket).expect("real executing child startup");
}

#[test]
fn producer_packets_require_the_original_task_and_no_extra_rights() {
    let (host, mut app) = UnixStream::pair().expect("private producer channel");
    arm(&host).expect("credential delivery");
    host.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let (_owned, proof) = UnixStream::pair().expect("current producer proof");
    let nonce = [83; 32];
    send_answer(&app, &nonce, &proof).unwrap();
    let peer = receive(&host, &nonce, unsafe { libc::geteuid() }).unwrap();
    app.write_all(b"original").unwrap();
    let mut bytes = [0; 64];
    assert_eq!(peer.receive_packet(&host, &mut bytes).unwrap(), 8);
    assert_eq!(&bytes[..8], b"original");

    // Even the original process may not smuggle a new capability with its data.
    send_answer(&app, &nonce, &proof).unwrap();
    assert!(peer.receive_packet(&host, &mut bytes).is_err());

    // A real exec child inherits the same channel but cannot impersonate its owner.
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, app.as_raw_fd().to_string())
        .env("TILO_EXECUTING_PEER_PACKET", "1");
    let fd = app.as_raw_fd();
    // SAFETY: only the already owned descriptor's child-side CLOEXEC is changed.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut other = command.spawn().unwrap();
    assert!(peer.receive_packet(&host, &mut bytes).is_err());
    assert!(other.wait().unwrap().success());
}
fn child(socket: &UnixStream, proof: Option<&UnixStream>) -> std::process::Child {
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
    command
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, socket.as_raw_fd().to_string());
    let mut descriptors = vec![socket.as_raw_fd()];
    if let Some(proof) = proof {
        command.env(
            "TILO_EXECUTING_PEER_FORGED_PROOF",
            proof.as_raw_fd().to_string(),
        );
        descriptors.push(proof.as_raw_fd());
    }
    // SAFETY: child pre-exec performs only async-signal-safe fcntl, preserving
    // exactly the fixture's owned descriptors, without touching the parent.
    unsafe {
        command.pre_exec(move || {
            for descriptor in &descriptors {
                if libc::fcntl(*descriptor, libc::F_SETFD, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command.spawn().expect("real child")
}
#[test]
fn executing_peer_pins_child_not_socket_launcher_and_rejects_reused_launch_start() {
    let (mut host, app) = UnixStream::pair().expect("inherited service");
    let nonce = [51; 32];
    challenge(&mut host, &nonce);
    let mut child = child(&app, None);
    let peer = receive(&host, &nonce, unsafe { libc::geteuid() }).expect("kernel-attested child");
    assert_eq!(peer.pid(), child.id());
    assert_ne!(peer.pid(), std::process::id());
    assert!(peer.current());
    assert!(peer.belongs_to(std::process::id(), start()));
    assert!(!peer.belongs_to(
        std::process::id(),
        start().checked_add(1).expect("different launch")
    ));
    assert!(!peer.belongs_to(u32::MAX, start()));
    host.write_all(&[1]).expect("host admission ack");
    assert!(child.wait().expect("reap child").success());
    assert!(
        !peer.current(),
        "a exited task cannot regain custody through numeric PID reuse"
    );
    let mut restarted = std::process::Command::new("/bin/true")
        .spawn()
        .expect("new process");
    assert!(
        !peer.current(),
        "a new application process cannot revive an old kernel handle"
    );
    assert!(restarted.wait().expect("reap new process").success());
}
#[test]
fn executing_peer_refuses_an_inherited_launcher_proof_from_a_different_sender() {
    let (mut host, app) = UnixStream::pair().expect("inherited service");
    let (_owned, proof) = UnixStream::pair().expect("foreign parent-created proof");
    let nonce = [71; 32];
    challenge(&mut host, &nonce);
    let mut child = child(&app, Some(&proof));
    let error = receive(&host, &nonce, unsafe { libc::geteuid() })
        .err()
        .expect("foreign creator refused");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(child.wait().expect("reap child").success());
}
#[test]
fn executing_peer_refuses_wrong_nonce_user_missing_extra_or_non_socket_rights() {
    for case in 0..5 {
        let (host, app) = UnixStream::pair().expect("service pair");
        arm(&host).expect("kernel sender credentials");
        host.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("bounded receive");
        let (_owned, proof) = UnixStream::pair().expect("proof pair");
        let file = std::fs::File::open("/dev/null").expect("non-socket proof");
        let rights = match case {
            2 => vec![],
            3 => vec![proof.as_fd(), app.as_fd()],
            4 => vec![file.as_fd()],
            _ => vec![proof.as_fd()],
        };
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
        let mut ancillary = SendAncillaryBuffer::new(&mut space);
        if !rights.is_empty() {
            assert!(ancillary.push(SendAncillaryMessage::ScmRights(&rights)));
        }
        let nonce = if case == 0 { [29; 32] } else { [31; 32] };
        assert_eq!(
            rustix::net::sendmsg(
                &app,
                &[IoSlice::new(&nonce)],
                &mut ancillary,
                SendFlags::NOSIGNAL
            )
            .expect("actual ancillary message"),
            32
        );
        let uid = unsafe { libc::geteuid() };
        let expected_uid = if case == 1 { uid.wrapping_add(1) } else { uid };
        assert!(
            receive(&host, &[31; 32], expected_uid).is_err(),
            "invalid case {case} must refuse"
        );
    }
}
