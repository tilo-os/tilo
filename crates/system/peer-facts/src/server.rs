//! The listening socket and request loop shared by root services: one
//! connection at a time, one newline-delimited JSON request per connection,
//! caller identified from the kernel before a byte is read.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Bounds reading a request and writing its reply on one client connection.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound on how long the poll loop sleeps between checks of `stop` and
/// `idle_exit`, even when the housekeeping interval is longer.
const MAX_POLL_WAIT: Duration = Duration::from_millis(500);

#[derive(Deserialize)]
struct IncomingEnvelope<R> {
    protocol: u32,
    request: R,
}

#[derive(Clone, Copy, Debug)]
pub struct Pacing {
    pub housekeeping: Duration,
    /// Return after this long without a request while idle; the socket unit
    /// starts the service again on the next connection.
    pub idle_exit: Option<Duration>,
}

/// A root service plugged into [`serve`] and [`serve_one`].
pub trait Handler {
    type Caller;
    type Request: DeserializeOwned;
    type Response: Serialize;
    /// Optional side-channel payload sent with the JSON reply line (such as a
    /// file descriptor passed with `SCM_RIGHTS`).
    type Attachment;

    const PROTOCOL_VERSION: u32;
    const MAX_LINE_BYTES: u64;

    fn refused(reason: String) -> Self::Response;

    fn handle(
        &mut self,
        caller: Self::Caller,
        request: Self::Request,
    ) -> (Self::Response, Option<Self::Attachment>);

    fn housekeeping(&mut self);

    fn is_idle(&self) -> bool;

    /// Write one newline-terminated reply line and any optional attachment to
    /// `stream`.
    ///
    /// The default implementation sends only the line and returns
    /// [`io::ErrorKind::Unsupported`] without writing anything when an
    /// attachment is present, so a handler that produces attachments cannot
    /// drop them silently by forgetting to override this method.
    fn write_reply(
        mut stream: &UnixStream,
        line: &[u8],
        attachment: Option<Self::Attachment>,
    ) -> io::Result<()> {
        if attachment.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "handler returned an attachment without overriding write_reply",
            ));
        }
        stream.write_all(line)
    }
}

/// The listening socket: the one systemd socket activation passed for `socket`,
/// or a fresh socket bound there when started by hand.
///
/// Standalone starts never create the socket's parent directory as root: in
/// production systemd's tmpfiles and socket unit own the runtime directory.
/// When not socket-activated, the parent directory must already exist, must not
/// be a symbolic link, and must be owned by the effective user and not writable
/// by group or others. Any stale socket at `socket` is replaced; a non-socket
/// entry is refused rather than removed.
///
/// # Safety
///
/// Calls [`tilo_files::native::private::activated_socket`], which clears
/// `LISTEN_*` from the environment; no other thread may read or write the
/// environment while this runs.
pub unsafe fn listen(socket: &Path) -> io::Result<UnixListener> {
    // SAFETY: forwarded from caller: no concurrent environment access.
    if let Some(listener) = unsafe { tilo_files::native::private::activated_socket(socket)? } {
        return Ok(listener);
    }
    bind_socket(socket)
}

/// Bind a standalone root-service socket at `socket` with mode `0o666` inside
/// an existing, owner-controlled parent directory.
pub fn bind_socket(socket: &Path) -> io::Result<UnixListener> {
    if socket.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} has no socket filename", socket.display()),
        ));
    }
    let parent = socket
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let dir = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)?;
    tilo_files::native::atomic::StorageDirectory::from_file(dir)?.validate_namespace_control()?;
    match fs::symlink_metadata(socket) {
        Ok(metadata) if !metadata.file_type().is_socket() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", socket.display()),
            ));
        }
        Ok(_) => match fs::remove_file(socket) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = tilo_files::native::socket::bind(socket)?;
    // Who may ask what is decided per connection, from the kernel's account of
    // the caller; the socket itself is open to every local process.
    if let Err(error) = fs::set_permissions(socket, fs::Permissions::from_mode(0o666)) {
        let _ = fs::remove_file(socket);
        return Err(error);
    }
    Ok(listener)
}

/// Identify `stream` from the kernel's peer facts and apply `decide`.
#[cfg(target_os = "linux")]
pub fn identify_peer<C>(
    stream: &UnixStream,
    decide: impl FnOnce(&crate::PeerFacts) -> Result<C, String>,
) -> Result<C, String> {
    crate::facts(stream)
        .map_err(|error| format!("unidentified caller: {error}"))
        .and_then(|facts| decide(&facts))
}

fn answer<H: Handler>(
    stream: &UnixStream,
    response: &H::Response,
    attachment: Option<H::Attachment>,
) {
    if let Ok(mut line) = serde_json::to_vec(response) {
        line.push(b'\n');
        let _ = H::write_reply(stream, &line, attachment);
    }
}

/// Serve one connection: identify the caller before reading a byte of its
/// request, then read one bounded JSON envelope and answer it.
pub fn serve_one<H: Handler>(
    stream: &UnixStream,
    handler: &mut H,
    identify: &mut impl FnMut(&UnixStream) -> Result<H::Caller, String>,
) {
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(CLIENT_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(CLIENT_TIMEOUT)).is_err()
    {
        return;
    }
    let caller = match identify(stream) {
        Ok(caller) => caller,
        Err(reason) => return answer::<H>(stream, &H::refused(reason), None),
    };
    let mut line = String::new();
    if BufReader::new(stream)
        .take(H::MAX_LINE_BYTES)
        .read_line(&mut line)
        .is_err()
    {
        return;
    }
    let (response, attachment) = match serde_json::from_str::<IncomingEnvelope<H::Request>>(&line) {
        Ok(envelope) if envelope.protocol == H::PROTOCOL_VERSION => {
            handler.handle(caller, envelope.request)
        }
        Ok(_) => (H::refused("unsupported protocol version".into()), None),
        Err(_) => (H::refused("malformed request".into()), None),
    };
    answer::<H>(stream, &response, attachment);
}

fn readable(listener: &UnixListener, timeout: Duration) -> bool {
    let mut poll = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: one valid pollfd for the duration of the call.
    unsafe { libc::poll(&raw mut poll, 1, millis) > 0 && (poll.revents & libc::POLLIN) != 0 }
}

/// Serve `listener` until `stop` is set or the service has been idle long
/// enough to exit.
///
/// Housekeeping runs once at start and then at least every
/// `pacing.housekeeping`. Polling wakes at least every 500 ms — and no later
/// than `pacing.idle_exit` when one is set, so an idle exit shorter than
/// 500 ms is noticed on time. Once `stop` is set, no queued connection is
/// accepted and no due housekeeping runs, keeping shutdown prompt without
/// starting further privileged work.
pub fn serve<H: Handler>(
    listener: &UnixListener,
    handler: &mut H,
    mut identify: impl FnMut(&UnixStream) -> Result<H::Caller, String>,
    pacing: Pacing,
    stop: &AtomicBool,
) {
    handler.housekeeping();
    let mut tidied = Instant::now();
    let mut last_request = Instant::now();
    let mut poll_wait = pacing.housekeeping.min(MAX_POLL_WAIT);
    if let Some(idle) = pacing.idle_exit {
        poll_wait = poll_wait.min(idle);
    }
    while !stop.load(Ordering::Acquire) {
        if readable(listener, poll_wait)
            && !stop.load(Ordering::Acquire)
            && let Ok((stream, _)) = listener.accept()
        {
            serve_one(&stream, handler, &mut identify);
            last_request = Instant::now();
        }
        if stop.load(Ordering::Acquire) {
            return;
        }
        if tidied.elapsed() >= pacing.housekeeping {
            handler.housekeeping();
            tidied = Instant::now();
        }
        if pacing
            .idle_exit
            .is_some_and(|idle| handler.is_idle() && last_request.elapsed() >= idle)
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::IntoRawFd;
    use std::os::unix::fs::symlink;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(tag = "op", rename_all = "snake_case")]
    enum TestRequest {
        Ping { value: String },
    }

    #[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(tag = "result", rename_all = "snake_case")]
    enum TestResponse {
        Pong { value: String, caller: u32 },
        Refused { reason: String },
    }

    struct DummyHandler {
        idle: bool,
        housekeeping_runs: Arc<AtomicUsize>,
        attachment: Option<Vec<u8>>,
    }

    impl Handler for DummyHandler {
        type Caller = u32;
        type Request = TestRequest;
        type Response = TestResponse;
        type Attachment = Vec<u8>;

        const PROTOCOL_VERSION: u32 = 1;
        const MAX_LINE_BYTES: u64 = 1024;

        fn refused(reason: String) -> Self::Response {
            TestResponse::Refused { reason }
        }

        fn handle(
            &mut self,
            caller: Self::Caller,
            request: Self::Request,
        ) -> (Self::Response, Option<Self::Attachment>) {
            let TestRequest::Ping { value } = request;
            (TestResponse::Pong { value, caller }, self.attachment.take())
        }

        fn housekeeping(&mut self) {
            self.housekeeping_runs.fetch_add(1, Ordering::Relaxed);
        }

        fn is_idle(&self) -> bool {
            self.idle
        }
    }

    struct RestoreDir {
        previous: std::path::PathBuf,
    }

    impl RestoreDir {
        fn enter(dir: &Path) -> Self {
            let previous = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            Self { previous }
        }
    }

    impl Drop for RestoreDir {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.previous);
        }
    }

    struct FdThreeGuard {
        saved: libc::c_int,
    }

    impl FdThreeGuard {
        fn save() -> Self {
            // SAFETY: fcntl on scalar fd 3 with F_DUPFD_CLOEXEC; returns -1 if fd 3 is closed.
            let saved = unsafe { libc::fcntl(3, libc::F_DUPFD_CLOEXEC, 10) };
            Self { saved }
        }
    }

    impl Drop for FdThreeGuard {
        fn drop(&mut self) {
            if self.saved >= 0 {
                // SAFETY: restore the original descriptor 3 and close the temporary duplicate.
                unsafe {
                    libc::dup2(self.saved, 3);
                    libc::close(self.saved);
                }
            }
        }
    }

    fn install_as_fd_three(listener: UnixListener) {
        let raw = listener.into_raw_fd();
        if raw == 3 {
            // SAFETY: clear close-on-exec to match systemd socket activation.
            assert_eq!(unsafe { libc::fcntl(3, libc::F_SETFD, 0) }, 0);
        } else {
            // SAFETY: move the listener onto descriptor 3 (dup2 clears FD_CLOEXEC).
            assert_eq!(unsafe { libc::dup2(raw, 3) }, 3);
            unsafe { libc::close(raw) };
        }
    }

    #[test]
    fn standalone_bind_fails_closed_on_missing_or_uncontrolled_parent_and_non_socket() {
        let root = tempfile::tempdir().unwrap();

        // Missing parent directory is never created.
        let missing_parent = root.path().join("missing");
        let missing_socket = missing_parent.join("control.sock");
        assert_eq!(
            bind_socket(&missing_socket).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(
            !missing_parent.exists(),
            "missing parent must not be created"
        );

        // Symlinked parent directory is refused.
        let real_dir = root.path().join("real");
        fs::create_dir(&real_dir).unwrap();
        let link_dir = root.path().join("link");
        symlink(&real_dir, &link_dir).unwrap();
        assert!(bind_socket(&link_dir.join("control.sock")).is_err());

        // Group- or world-writable parent directory is refused.
        for bad_mode in [0o775, 0o757, 0o777] {
            let loose = root.path().join(format!("loose-{bad_mode:o}"));
            fs::create_dir(&loose).unwrap();
            fs::set_permissions(&loose, fs::Permissions::from_mode(bad_mode)).unwrap();
            assert_eq!(
                bind_socket(&loose.join("control.sock")).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied,
                "mode {bad_mode:o} must be refused"
            );
        }

        // Valid owner-controlled parent (`0755`): binds with mode `0666`,
        // replaces a stale socket, and refuses a non-socket without removing it.
        let good = root.path().join("good");
        fs::create_dir(&good).unwrap();
        fs::set_permissions(&good, fs::Permissions::from_mode(0o755)).unwrap();

        let regular = good.join("keep.txt");
        fs::write(&regular, b"do not delete").unwrap();
        assert_eq!(
            bind_socket(&regular).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&regular).unwrap(), b"do not delete");

        let socket = good.join("control.sock");
        let first = bind_socket(&socket).unwrap();
        assert_eq!(
            fs::symlink_metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o666
        );
        drop(first);
        let second = bind_socket(&socket).unwrap();
        assert_eq!(
            fs::symlink_metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o666
        );
        drop(second);
    }

    #[test]
    fn listen_adopts_systemd_activated_socket_and_rejects_mismatches() {
        tilo_test_env::in_own_process(
            module_path!(),
            "listen_adopts_systemd_activated_socket_and_rejects_mismatches",
            || {
                let root = tempfile::tempdir().unwrap();
                // Bind relative names inside the isolated process's temp dir so
                // `tilo_socket::bind` never has to fall back to a `/proc/self/fd`
                // alias when the runner's `TMPDIR` is longer than `sun_path`.
                let _cwd = RestoreDir::enter(root.path());
                let _guard = FdThreeGuard::save();
                let expected = Path::new("control.sock");
                let other = Path::new("other.sock");

                // 1. Valid activation: adopts fd 3, sets FD_CLOEXEC, clears LISTEN_*.
                let raw_listener = tilo_files::native::socket::bind(expected).unwrap();
                install_as_fd_three(raw_listener);
                {
                    let mut env = tilo_test_env::ScopedEnv::new();
                    env.set("LISTEN_PID", std::process::id().to_string());
                    env.set("LISTEN_FDS", "1");
                    env.set("LISTEN_FDNAMES", "control");

                    // SAFETY: running alone inside `in_own_process` with no other threads.
                    let adopted = unsafe { listen(expected) }.unwrap();
                    assert_eq!(adopted.as_raw_fd(), 3);
                    // SAFETY: scalar F_GETFD on the adopted descriptor.
                    let flags = unsafe { libc::fcntl(3, libc::F_GETFD) };
                    assert!(flags >= 0 && (flags & libc::FD_CLOEXEC) != 0);
                    assert!(std::env::var_os("LISTEN_PID").is_none());
                    assert!(std::env::var_os("LISTEN_FDS").is_none());
                    assert!(std::env::var_os("LISTEN_FDNAMES").is_none());

                    let _client = tilo_files::native::socket::connect(expected).unwrap();
                    let _accepted = adopted.accept().unwrap();
                }

                // 2. Wrong path on fd 3 is refused.
                let wrong_listener = tilo_files::native::socket::bind(other).unwrap();
                install_as_fd_three(wrong_listener);
                {
                    let mut env = tilo_test_env::ScopedEnv::new();
                    env.set("LISTEN_PID", std::process::id().to_string());
                    env.set("LISTEN_FDS", "1");
                    // SAFETY: running alone inside `in_own_process`.
                    let err = unsafe { listen(expected) }.unwrap_err();
                    assert!(err.to_string().contains("expected"), "unexpected: {err}");
                }

                // 3. LISTEN_FDS != 1 fails closed instead of falling through to bind.
                let fresh = Path::new("should-not-bind.sock");
                {
                    let mut env = tilo_test_env::ScopedEnv::new();
                    env.set("LISTEN_PID", std::process::id().to_string());
                    env.set("LISTEN_FDS", "2");
                    // SAFETY: running alone inside `in_own_process`.
                    assert!(unsafe { listen(fresh) }.is_err());
                    assert!(
                        !fresh.exists(),
                        "bad LISTEN_FDS must fail closed rather than binding a fresh socket"
                    );
                }
            },
        );
    }

    #[test]
    fn default_write_reply_refuses_attachments_without_writing_misleading_bytes() {
        let (server_end, client_end) = UnixStream::pair().unwrap();
        let err = DummyHandler::write_reply(&server_end, b"{\"result\":\"pong\"}\n", Some(vec![1]))
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        drop(server_end);
        let mut received = Vec::new();
        (&client_end).read_to_end(&mut received).unwrap();
        assert!(
            received.is_empty(),
            "failed default write_reply must write nothing to the peer"
        );

        let (server_end, mut client_end) = UnixStream::pair().unwrap();
        let mut handler = DummyHandler {
            idle: true,
            housekeeping_runs: Arc::new(AtomicUsize::new(0)),
            attachment: Some(vec![1]),
        };
        client_end
            .write_all(b"{\"protocol\":1,\"request\":{\"op\":\"ping\",\"value\":\"hi\"}}\n")
            .unwrap();
        serve_one(&server_end, &mut handler, &mut |_| Ok(7));
        drop(server_end);
        let mut reply = Vec::new();
        client_end.read_to_end(&mut reply).unwrap();
        assert!(
            reply.is_empty(),
            "an unoverridden handler returning an attachment must write no reply line"
        );
    }

    #[test]
    fn serve_runs_housekeeping_at_start_refuses_before_reading_and_exits_when_idle() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("control.sock");
        let listener = bind_socket(&socket).unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let mut handler = DummyHandler {
            idle: true,
            housekeeping_runs: Arc::clone(&runs),
            attachment: None,
        };

        // Connect before spawning so the client already waits in the listen
        // backlog when `serve` starts and cannot race with `idle_exit`.
        let stream = tilo_files::native::socket::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();

        let started = Instant::now();
        let thread = std::thread::spawn({
            let stop = Arc::clone(&stop);
            move || {
                serve(
                    &listener,
                    &mut handler,
                    |_| Err("denied".into()),
                    Pacing {
                        housekeeping: Duration::from_secs(60),
                        idle_exit: Some(Duration::from_millis(120)),
                    },
                    &stop,
                );
            }
        });

        // Write NOTHING: identify must refuse immediately without waiting on
        // the 5 s client read timeout.
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        assert_eq!(
            serde_json::from_str::<TestResponse>(&line).unwrap(),
            TestResponse::Refused {
                reason: "denied".into()
            }
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "refusal waited for client bytes: {:?}",
            started.elapsed()
        );
        assert!(
            runs.load(Ordering::Relaxed) >= 1,
            "housekeeping must have run once at start"
        );

        // Idle exit returns promptly without needing `stop` to be set.
        let waiting = Instant::now();
        thread.join().unwrap();
        assert!(
            waiting.elapsed() < Duration::from_secs(2),
            "idle exit took {:?}",
            waiting.elapsed()
        );
    }
}
