use std::{
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{fs::MetadataExt, process::CommandExt},
    },
    process::{Command, Stdio},
    time::Duration,
};

fn activation_child(
    listener: &std::os::unix::net::UnixListener,
    socket: &std::path::Path,
    store: &std::path::Path,
) -> std::process::Child {
    // Keep a known descriptor distinct from fd 3 so dup2 always clears
    // close-on-exec on the activation descriptor.
    // SAFETY: fcntl duplicates the live listener descriptor and returns a new,
    // independently owned descriptor on success.
    let inherited = unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    assert!(inherited >= 10, "could not duplicate activation listener");
    // SAFETY: `inherited` is a newly allocated descriptor owned by this test.
    let inherited = unsafe { OwnedFd::from_raw_fd(inherited) };
    let inherited_fd = inherited.as_raw_fd();

    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("LISTEN_PID=$$; export LISTEN_PID; exec \"$@\"")
        .arg("tilo-app-host-activation-test")
        .arg(env!("CARGO_BIN_EXE_tilo-app-host"))
        .arg("--socket")
        .arg(socket)
        .arg("--store")
        .arg(store)
        .arg("--max-store-bytes=0")
        .env("LISTEN_FDS", "1")
        .env("LISTEN_FDNAMES", "app-host")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: this closure runs after fork and before exec and calls only the
    // async-signal-safe dup2. The shell sets LISTEN_PID to its own unchanged
    // pid immediately before replacing itself with the daemon.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(inherited_fd, 3) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("spawn socket-activated app host");
    drop(inherited);
    child
}

#[test]
fn consumes_systemd_listener_without_rebinding_it() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let socket = temp.path().join("app-host.sock");
    let store = temp.path().join("store");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("activation listener");
    let original_inode = std::fs::metadata(&socket).expect("socket metadata").ino();
    let mut child = activation_child(&listener, &socket, &store);
    drop(listener);

    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(child.try_wait().expect("poll child"), None);
    assert_eq!(
        std::fs::metadata(&socket)
            .expect("activated socket remains")
            .ino(),
        original_inode,
        "daemon unlinked and rebound the systemd-owned listener"
    );
    let mut stream = std::os::unix::net::UnixStream::connect(&socket)
        .expect("connect through inherited listener");
    let handoff = br#"{"protocol":1,"app_id":"so.tilo.store"}"#;
    stream.write_all(b"QAH1").expect("write magic");
    stream
        .write_all(
            &u32::try_from(handoff.len())
                .expect("bounded handoff")
                .to_be_bytes(),
        )
        .expect("write length");
    stream.write_all(handoff).expect("write handoff");

    child.kill().expect("stop daemon");
    child.wait().expect("reap daemon");
}
