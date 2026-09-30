//! The path `apps/terminal` takes: ask the supervisor for a guest over the
//! service socket, receive a pty master, and drive a shell through it.
//!
//! The supervisor builds a kernel namespace with an overlay, and the assertions
//! are on bytes that came back from `bash`
//! running inside it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ush::store::Store;
use ush::supervisor::{ServeRequest, ServeResponse, decide_serve, recv_with_fd};

/// Read from a descriptor until `needle` shows up or the deadline passes.
fn read_until(fd: i32, needle: &str, timeout: Duration) -> String {
    let mut seen = String::new();
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    // SAFETY: `fd` is a pty master this test owns. Non-blocking so the loop can
    // give up rather than hang the suite.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let mut buf = [0u8; 4096];
    while Instant::now() < deadline {
        // SAFETY: as above; `buf` is a live local buffer.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len()) };
        if n > 0 {
            // `n > 0` was just checked, so the cast below cannot lose the sign.
            #[allow(clippy::cast_sign_loss, reason = "n > 0 was just checked above")]
            let n = n as usize;
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            if seen.contains(needle) {
                return seen;
            }
        } else {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    seen
}

#[test]
fn the_service_protocol_refuses_dsh_answers_garbage_and_never_hangs() {
    // phase: the_service_refuses_a_developer_session_to_a_caller
    // A `dsh` session is authority without a boundary. An app that was handed the
    // service socket at launch must not be able to ask for one, and this is the
    // function that decides it.
    {
        let err = decide_serve(&ServeRequest::OpenSession {
            profile: "dsh".into(),
            cols: 80,
            rows: 24,
            term: None,
            command: Vec::new(),
        })
        .expect_err("dsh was granted over the service socket");
        assert!(err.contains("not a sandbox"), "{err}");

        for name in ["ush", "", "something-else"] {
            assert!(
                decide_serve(&ServeRequest::OpenSession {
                    profile: name.into(),
                    cols: 80,
                    rows: 24,
                    term: None,
                    command: Vec::new(),
                })
                .is_ok(),
                "{name:?} should resolve to the sandboxed profile and be allowed"
            );
        }
    }
    // phase: a_malformed_request_is_answered
    // A malformed request must be refused with a reply, not by dropping the
    // connection — a caller that gets silence cannot tell "refused" from "the
    // supervisor is not running".
    {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("ush.sock");
        let store = Store::at(dir.path().join("store"));
        store.ensure().expect("store");

        let serve_socket = socket.clone();
        std::thread::spawn(move || {
            let _ = ush::supervisor::serve(&serve_socket, &store, &PathBuf::from("/"));
        });
        let deadline = tilo_test_env::hang_deadline();
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }

        let stream = UnixStream::connect(&socket).expect("connect");
        (&stream).write_all(b"{ not json }\n").expect("write");
        (&stream).flush().expect("flush");

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read reply");
        assert!(
            line.contains("denied"),
            "a malformed request must be refused in words: {line:?}"
        );
    }
    // phase: read_until_gives_up_rather_than_hanging
    // Kept so the unused-import warning cannot hide a reader that never reads.
    {
        let (a, _b) = UnixStream::pair().unwrap();
        let started = Instant::now();
        let seen = read_until(a.as_raw_fd(), "never", Duration::from_millis(200));
        assert!(seen.is_empty());
        assert!(started.elapsed() < Duration::from_secs(5));
        let mut sink = Vec::new();
        let _ = (&a).read_to_end(&mut sink);
    }
}

/// The whole handoff, against a real kernel: the supervisor serves, a
/// client asks, a descriptor arrives, and a shell answers on it. Skipped
/// (loudly, with the reason) where the machine can't build a guest at all —
/// a container with no `bwrap`, or unprivileged user namespaces disabled.
#[test]
fn a_terminal_gets_a_working_shell_over_the_service_socket() {
    let Some(reason) = guest_unavailable() else {
        run_handoff();
        return;
    };
    eprintln!(
        "serve_handoff: not exercising the guest on this machine — {reason}. \
         The protocol half is still covered by the tests above."
    );
}

/// A guest whose sandbox cannot be built says why in words under `bwrap`'s
/// own one-line reason; one that can be built runs without that notice.
/// Both halves assert: which one runs depends on the machine, as probed.
#[test]
fn a_sandbox_that_cannot_be_built_is_explained() {
    if which_bwrap().is_none() {
        eprintln!("serve_handoff: bwrap is not installed; no sandbox to fail");
        return;
    }
    let unavailable = guest_unavailable();
    let data = xdg_paths::data_dir().expect("a data directory for the store");
    let store = tempfile::TempDir::with_prefix_in("ush-explain-", data).expect("store dir");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_tilo-ush"))
        .args(["run", "--ephemeral", "-c", "true"])
        .env(ush::store::STORE_ENV, store.path())
        .stdin(std::process::Stdio::null())
        .output()
        .expect("tilo-ush runs");
    let path = store.keep();
    ush::guest::remove_dir_all_forced(&path);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let explained = stderr.contains("sandbox could not be set up");
    if let Some(reason) = unavailable {
        assert!(!out.status.success(), "{reason}; yet the guest ran");
        assert!(
            explained,
            "a guest that cannot mount ({reason}) must say so, not leave only \
             bwrap's line: {stderr}"
        );
    } else {
        assert!(out.status.success(), "the guest should run here: {stderr}");
        assert!(
            !explained,
            "a guest that ran is not a setup failure: {stderr}"
        );
    }
}

/// Can this machine build the guest's layered root at all? The probe
/// performs the actual overlay mount the runtime performs — a user
/// namespace, `/usr` as the lower, a directory on the store's filesystem as
/// the upper — not something that merely correlates with it. An earlier
/// version only checked that a user namespace could be created, which
/// passed in this repo's build container and then failed 180s later on a
/// mount the container can't make (its own `/usr` is a podman overlay,
/// which overlayfs refuses to stack on). A check that can't see the thing
/// it's checking is worse than no check.
fn guest_unavailable() -> Option<String> {
    if which_bwrap().is_none() {
        return Some("bwrap is not installed".into());
    }
    let dir = tempfile::TempDir::with_prefix_in("ush-probe-", xdg_paths::data_dir()?).ok()?;
    let upper = dir.path().join("upper");
    let work = dir.path().join("work");
    std::fs::create_dir_all(&upper).ok()?;
    std::fs::create_dir_all(&work).ok()?;

    let probe = std::process::Command::new("bwrap")
        .arg("--unshare-user")
        .args(["--uid", "0", "--gid", "0"])
        .arg("--overlay-src")
        .arg("/usr")
        .arg("--overlay")
        .arg(&upper)
        .arg(&work)
        .arg("/usr")
        // The loader lives at /lib64/ld-linux-*.so.2; a missing interpreter fails
        // `execvp` with ENOENT, same as a missing binary — how this first "failed"
        // on a machine that was perfectly capable.
        .args(["--symlink", "usr/bin", "/bin"])
        .args(["--symlink", "usr/sbin", "/sbin"])
        .args(["--symlink", "usr/lib", "/lib"])
        .args(["--symlink", "usr/lib64", "/lib64"])
        .args(["--proc", "/proc", "--dev", "/dev"])
        .arg("/usr/bin/true")
        .output();
    // The probe's overlay leaves its workdir at mode 000, which an ordinary
    // recursive delete can't enter — `TempDir`'s own cleanup would fail silently.
    let path = dir.keep();
    ush::guest::remove_dir_all_forced(&path);

    match probe {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(format!(
            "this machine cannot mount the guest's layered root: {}",
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Some(format!("bwrap could not be run: {e}")),
    }
}

fn which_bwrap() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join("bwrap"))
        .find(|p| p.exists())
}

fn run_handoff() {
    // The supervisor re-execs itself to start a session. In production
    // `current_exe` is `tilo-ush`; under `cargo test` it's this test binary,
    // which would treat `run` as a name filter and exit — point it at the real one.
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_USH_BIN", env!("CARGO_BIN_EXE_tilo-ush"));

    // On ordinary disk, not `/tmp`: a layer is an overlayfs upper and tmpfs
    // can't be one — putting the store there would test the refusal, not the handoff.
    let dir = tempfile::TempDir::with_prefix_in(
        "ush-handoff-",
        xdg_paths::data_dir().expect("no data directory on this machine"),
    )
    .expect("tempdir on disk");
    let socket = dir.path().join("ush.sock");
    let store = Store::at(dir.path().join("store"));
    store.ensure().expect("store");

    let serve_socket = socket.clone();
    let serve_store = store;
    std::thread::spawn(move || {
        let _ = ush::supervisor::serve(&serve_socket, &serve_store, &PathBuf::from("/"));
    });

    // Wait for the listener rather than sleeping a fixed amount.
    let deadline = tilo_test_env::hang_deadline();
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "the supervisor never bound its socket");

    let stream = UnixStream::connect(&socket).expect("connect");
    let mut request = serde_json::to_vec(&ServeRequest::OpenSession {
        profile: "ush".into(),
        cols: 100,
        rows: 30,
        term: Some("xterm-256color".into()),
        command: Vec::new(),
    })
    .unwrap();
    request.push(b'\n');
    (&stream).write_all(&request).expect("write request");
    (&stream).flush().expect("flush");

    let (payload, fd) = recv_with_fd(stream.as_raw_fd()).expect("recvmsg");
    let text = String::from_utf8_lossy(&payload);
    let response: ServeResponse =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("bad reply {text:?}: {e}"));
    match &response {
        ServeResponse::Session { profile, .. } => assert_eq!(profile, "ush"),
        ServeResponse::Denied { reason } => panic!("the service refused a ush session: {reason}"),
    }
    let fd = fd.expect("no pty descriptor arrived with the reply");

    // The descriptor is a terminal, and it is the size that was asked for.
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is the pty master just received.
    assert_eq!(
        unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) },
        0,
        "the descriptor that arrived is not a terminal"
    );
    assert_eq!(
        (ws.ws_col, ws.ws_row),
        (100, 30),
        "the pty was not sized to the caller's grid"
    );

    // Wait for a prompt before typing: the first layer mirrors ~22k
    // directories and runs `/etc/profile.d`, so early writes are lost.
    let prompt = read_until(fd, "]#", Duration::from_secs(240));
    assert!(
        prompt.contains("]#"),
        "no prompt appeared within the timeout. What came back:\n{prompt}"
    );

    // The marker is assembled by the shell, not typed, so the pty's own echo
    // can't satisfy the wait — without that this passes in 0.3s reading back
    // what it just wrote.
    let marker = "USH_HANDOFF_OK";
    let script = "printf 'USH%s_OK-%s\\n' _HANDOFF \"$(whoami)\"\n".to_string();
    // SAFETY: `fd` is a live pty master.
    let written = unsafe { libc::write(fd, script.as_ptr().cast::<libc::c_void>(), script.len()) };
    assert!(written > 0, "could not write to the pty");

    let seen = read_until(fd, marker, tilo_test_env::hang_bound());
    assert!(
        seen.contains(marker),
        "no shell answered on the pty within the timeout. What came back:\n{seen}"
    );
    assert!(
        seen.contains(&format!("{marker}-root")),
        "the guest should run as uid 0 inside its own user namespace, which is \
         what lets rpm own files in the layer. What came back:\n{seen}"
    );

    // SAFETY: this test owns the descriptor.
    unsafe { libc::close(fd) };

    // Overlayfs leaves the workdir's inner directory at mode 000, so
    // `TempDir`'s own cleanup fails silently — tear it down like the runtime does.
    let path = dir.keep();
    ush::guest::remove_dir_all_forced(&path);
    assert!(
        !path.exists(),
        "the test left its store behind at {}",
        path.display()
    );
}
