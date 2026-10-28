use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::{fs::MetadataExt, net::UnixListener, process::CommandExt};
use std::process::{Command, Stdio};

#[test]
fn consumes_systemd_listener_without_unlinking_or_rebinding() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let socket = temp.path().join("ush.sock");
    let store = temp.path().join("store");
    let listener = UnixListener::bind(&socket).expect("activation listener");
    let original_inode = std::fs::metadata(&socket).expect("socket metadata").ino();

    // Keep the source away from fd 3 so dup2 also clears close-on-exec.
    // SAFETY: fcntl duplicates a live descriptor and returns independent ownership.
    let inherited = unsafe { libc::fcntl(listener.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 10) };
    assert!(inherited >= 10, "could not duplicate activation listener");
    // SAFETY: `inherited` is a newly allocated descriptor owned by this test.
    let inherited = unsafe { OwnedFd::from_raw_fd(inherited) };
    let inherited_fd = inherited.as_raw_fd();

    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg("LISTEN_PID=$$; export LISTEN_PID; exec \"$@\"")
        .arg("tilo-ush-activation-test")
        .arg(tilo_test_env::package_binary!("tilo-ush"))
        .arg("serve")
        .arg("--socket")
        .arg(&socket)
        .env("LISTEN_FDS", "1")
        .env("LISTEN_FDNAMES", "ush")
        .env("TILO_USH_HOME", &store)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: after fork and before exec this invokes only async-signal-safe dup2.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(inherited_fd, 3) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn socket-activated ush");
    drop(inherited);
    drop(listener);

    let mut stream = std::os::unix::net::UnixStream::connect(&socket)
        .expect("connect through inherited listener");
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .expect("bound the answer");
    stream.write_all(b"{}\n").expect("write request");
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .expect("read refusal");
    assert!(
        response.contains("denied"),
        "unexpected response: {response}"
    );
    // Serving the request means the daemon finished starting: it kept the
    // inherited socket instead of replacing it, and runs on.
    assert_eq!(child.try_wait().expect("poll child"), None);
    assert_eq!(
        std::fs::metadata(&socket)
            .expect("activated socket remains")
            .ino(),
        original_inode,
        "daemon replaced the systemd-owned socket"
    );

    child.kill().expect("stop daemon");
    child.wait().expect("reap daemon");
}
