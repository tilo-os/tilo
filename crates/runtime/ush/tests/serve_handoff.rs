//! The path `apps/terminal` takes: ask the supervisor for a session on a
//! target over the service socket, receive a pty master, and drive a shell
//! through it.
//!
//! The guest is built from an environment record whose images are fixture
//! trees: a runtime whose `usr` is the machine's own `/usr` and a tool that
//! adds one command. The assertions are on bytes that came back from `bash`
//! running inside it, and on what it left on the host.

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
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tilo_environment::{DEFAULT_RECORD, Environment, PackageId, Target};
use ush::records::Records;
use ush::store::Store;
use ush::supervisor::{ServeRequest, ServeResponse, decide_serve, recv_with_fd};

/// Tests that spawn a guest set process-wide variables the guest inherits.
static GUEST_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

const RUNTIME: [u8; 32] = [0x11; 32];
const TOOL: [u8; 32] = [0x22; 32];
const TOOL_COMMAND: &str = "ush-fixture-tool";

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
fn the_service_protocol_refuses_the_host_target_answers_garbage_and_never_hangs() {
    // phase: the_host_target_is_refused_with_its_reason
    {
        let err = decide_serve(&ServeRequest::OpenSession {
            target: Target::Host,
            cols: 80,
            rows: 24,
            term: None,
            command: Vec::new(),
            requested_by: ush::host::RequestedBy::Person,
        })
        .expect_err("the host target was served");
        assert!(err.starts_with("not available"), "{err}");
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

/// Lay out the fixture images under `root` the way the supervisor finds
/// them: `<hex id>/` trees and the current runtime's id.
fn fixture_images(root: &Path) {
    let runtime = root.join(PackageId::from_bytes(RUNTIME).to_hex());
    std::fs::create_dir_all(runtime.join("etc")).unwrap();
    std::os::unix::fs::symlink("/usr", runtime.join("usr")).unwrap();
    std::fs::write(
        runtime.join("etc/passwd"),
        "root:x:0:0:root:/root:/bin/bash\n",
    )
    .unwrap();
    std::fs::write(runtime.join("etc/group"), "root:x:0:\n").unwrap();

    let tool = root.join(PackageId::from_bytes(TOOL).to_hex());
    std::fs::create_dir_all(tool.join("usr/bin")).unwrap();
    let command = tool.join("usr/bin").join(TOOL_COMMAND);
    std::fs::write(&command, "#!/bin/sh\necho \"from the tool image\"\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        root.join("current-runtime"),
        PackageId::from_bytes(RUNTIME).to_string(),
    )
    .unwrap();
}

/// Can this machine build the guest at all? The probe performs the mounts
/// the runtime performs — a user namespace mapping the real uid, `/usr` as a
/// read-only overlay of two lowers — not something that merely correlates
/// with them. Inside a build container whose own `/usr` is an overlay, the
/// kernel refuses to stack on it, and the guest tests say so and stop.
fn guest_unavailable() -> Option<String> {
    let Some(bwrap) = which_bwrap() else {
        return Some("bwrap is not installed".into());
    };
    let dir = tempfile::tempdir().ok()?;
    let lower = dir.path().join("lower");
    std::fs::create_dir_all(&lower).ok()?;
    // SAFETY: `geteuid`/`getegid` cannot fail.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    let probe = std::process::Command::new(bwrap)
        .arg("--unshare-user")
        .args(["--uid", &uid.to_string(), "--gid", &gid.to_string()])
        .arg("--overlay-src")
        .arg(&lower)
        .args(["--overlay-src", "/usr", "--ro-overlay", "/usr"])
        .args(["--symlink", "usr/bin", "/bin"])
        .args(["--symlink", "usr/sbin", "/sbin"])
        .args(["--symlink", "usr/lib", "/lib"])
        .args(["--symlink", "usr/lib64", "/lib64"])
        .args(["--proc", "/proc", "--dev", "/dev"])
        .arg("/usr/bin/true")
        .output();
    match probe {
        Ok(o) if o.status.success() => None,
        Ok(o) => Some(format!(
            "this machine cannot stack the guest's images: {}",
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

/// The whole handoff against a real kernel: the supervisor serves, a client
/// asks for the default environment, a descriptor arrives, and a shell in a
/// guest built from the record answers on it — with the tool the record
/// lists, at the real uid, writing into the directory the record grants.
/// Skipped, loudly and with the reason, where the machine can't build a
/// guest at all.
#[test]
fn a_terminal_gets_a_shell_in_its_environment_over_the_service_socket() {
    let _guest = GUEST_ENV.lock().expect("guest env lock");
    let Some(reason) = guest_unavailable() else {
        run_handoff();
        return;
    };
    eprintln!(
        "serve_handoff: not exercising the guest on this machine — {reason}. \
         The protocol half is still covered by the tests above."
    );
}

/// The record: the default environment, one tool, one granted directory.
fn fixture_environment(store: &Store, granted: &Path) {
    let records = Records::at(store.records_root());
    records
        .create(
            DEFAULT_RECORD,
            &Environment::new(
                "Fixture",
                "2026.10",
                tilo_environment::host_arch(),
                &PackageId::from_bytes(RUNTIME),
            ),
        )
        .unwrap();
    records
        .apply(
            DEFAULT_RECORD,
            Environment::ADD_TOOL.with((
                vec!["fixture-tool".into()],
                tilo_environment::host_arch().to_owned(),
                PackageId::from_bytes(TOOL).to_string(),
            )),
        )
        .unwrap();
    records
        .apply(
            DEFAULT_RECORD,
            Environment::GRANT_DIR.with((granted.display().to_string(), true)),
        )
        .unwrap();
}

/// Ask for the default environment and receive its pty master.
fn open_default_session(socket: &Path) -> (UnixStream, i32) {
    let stream = UnixStream::connect(socket).expect("connect");
    let mut request = serde_json::to_vec(&ServeRequest::OpenSession {
        target: Target::default_environment(),
        cols: 100,
        rows: 30,
        term: Some("xterm-256color".into()),
        command: Vec::new(),
        requested_by: ush::host::RequestedBy::Person,
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
        ServeResponse::Session { environment, .. } => assert_eq!(environment, DEFAULT_RECORD),
        ServeResponse::Denied { reason } | ServeResponse::HostNotAllowed { reason } => {
            panic!("the service refused the session: {reason}")
        }
        ServeResponse::Environments { .. } => panic!("a session request answered with a list"),
    }
    let fd = fd.expect("no pty descriptor arrived with the reply");
    (stream, fd)
}

fn run_handoff() {
    let dir = tempfile::tempdir().expect("tempdir");
    let images = dir.path().join("images");
    fixture_images(&images);

    let mut env = tilo_test_env::ScopedEnv::new();
    // The supervisor re-execs itself to start a session. Under `cargo test`
    // `current_exe` is this test binary — point it at the real one.
    env.set("TILO_USH_BIN", env!("CARGO_BIN_EXE_tilo-ush"));
    env.set("TILO_USH_IMAGES", &images);
    env.set("SHELL", "/usr/bin/bash");

    let store = Store::at(dir.path().join("store"));
    store.ensure().expect("store");
    let granted = dir.path().join("granted");
    std::fs::create_dir_all(&granted).unwrap();
    let granted = granted.canonicalize().unwrap();

    fixture_environment(&store, &granted);

    let socket = dir.path().join("ush.sock");
    let serve_socket = socket.clone();
    let serve_store = store.clone();
    std::thread::spawn(move || {
        let _ = ush::supervisor::serve(&serve_socket, &serve_store, &PathBuf::from("/"));
    });
    let deadline = tilo_test_env::hang_deadline();
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "the supervisor never bound its socket");

    // The connection is the session's lifetime: it stays open until the end.
    let (_stream, fd) = open_default_session(&socket);

    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is the pty master just received.
    assert_eq!(unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) }, 0);
    assert_eq!((ws.ws_col, ws.ws_row), (100, 30));

    // The seeded prompt names the environment.
    let prompt = read_until(fd, "Fixture", tilo_test_env::hang_bound());
    assert!(
        prompt.contains("Fixture"),
        "no prompt naming the environment appeared. What came back:\n{prompt}"
    );

    // Markers are assembled by the shell, not typed, so the pty's own echo
    // can't satisfy a wait.
    let script = format!(
        "pkg list; {TOOL_COMMAND}; touch /usr/ush-probe 2>/dev/null || echo USR_READ_ONLY; \
         echo hi > {}/from-guest; printf 'UID_%s_ \\n' \"$(id -u)\"; printf 'NAME_%s_ \\n' \"$(id -un)\"; printf 'DO%s\\n' NE\r",
        granted.display()
    );
    // SAFETY: `fd` is a live pty master.
    let written = unsafe { libc::write(fd, script.as_ptr().cast::<libc::c_void>(), script.len()) };
    assert!(written > 0, "could not write to the pty");

    let seen = read_until(fd, "DONE", tilo_test_env::hang_bound());
    assert!(seen.contains("DONE"), "the shell never finished:\n{seen}");
    assert!(
        seen.contains("fixture-tool") && seen.contains(&PackageId::from_bytes(TOOL).to_string()),
        "`pkg list` must list the record's tool:\n{seen}"
    );
    assert!(
        seen.contains("from the tool image"),
        "the tool image's command must be on the guest's PATH:\n{seen}"
    );
    assert!(
        seen.contains("USR_READ_ONLY"),
        "/usr must be read-only in the guest:\n{seen}"
    );
    // SAFETY: `geteuid` cannot fail.
    let uid = unsafe { libc::geteuid() };
    assert!(
        seen.contains(&format!("UID_{uid}_")),
        "the guest must run as the real uid {uid}:\n{seen}"
    );
    let name = ush::guest::GuestUser::current().name;
    assert!(
        seen.contains(&format!("NAME_{name}_")),
        "the guest's passwd must name the real user, over the image's:\n{seen}"
    );
    let made = granted.join("from-guest");
    let meta = std::fs::metadata(&made).unwrap_or_else(|e| {
        panic!("the guest's write to the granted directory is missing: {e}\n{seen}")
    });
    assert_eq!(
        meta.uid(),
        uid,
        "a file written to a granted directory is the user's"
    );
    assert_eq!(std::fs::read_to_string(&made).unwrap(), "hi\n");
    assert!(
        store
            .environment_home(DEFAULT_RECORD)
            .join(".bashrc")
            .is_file(),
        "the environment's home lives in the store"
    );

    // SAFETY: this test owns the descriptor.
    unsafe { libc::close(fd) };
}

/// A guest whose sandbox cannot be built says why in words under `bwrap`'s
/// own one-line reason; one that can be built runs without that notice.
/// Both halves assert: which one runs depends on the machine, as probed.
#[test]
fn a_sandbox_that_cannot_be_built_is_explained() {
    let _guest = GUEST_ENV.lock().expect("guest env lock");
    if which_bwrap().is_none() {
        eprintln!("serve_handoff: bwrap is not installed; no sandbox to fail");
        return;
    }
    let unavailable = guest_unavailable();
    let dir = tempfile::tempdir().expect("tempdir");
    let images = dir.path().join("images");
    fixture_images(&images);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_tilo-ush"))
        .args(["run", "-c", "true"])
        .env(ush::store::STORE_ENV, dir.path().join("store"))
        .env("TILO_USH_IMAGES", &images)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("tilo-ush runs");
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

/// `pkg install` end to end in a real guest: the broker asks the tool
/// builder, which resolves and builds `tree` against a real env-runtime in
/// its build sandbox; the record gains the tool; after `restart` the command
/// is on the guest's PATH. Needs a materialised env-runtime tree
/// (`TILO_ENV_RUNTIME_TREE`) and the network for the package download.
#[test]
fn pkg_install_builds_a_tool_that_is_on_path_after_restart() {
    let Some(runtime_tree) = std::env::var_os("TILO_ENV_RUNTIME_TREE").map(PathBuf::from) else {
        eprintln!("pkg_install: TILO_ENV_RUNTIME_TREE names no env-runtime tree; not exercised");
        return;
    };
    let _guest = GUEST_ENV.lock().expect("guest env lock");
    if let Some(reason) = guest_unavailable() {
        eprintln!("pkg_install: not exercising the guest on this machine — {reason}");
        return;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    // The build's overlay upper wants ordinary disk; the socket a short path.
    let build_state = tempfile::tempdir_in(
        std::env::var_os("TILO_ENV_TOOLS_SCRATCH").map_or_else(std::env::temp_dir, PathBuf::from),
    )
    .expect("build state");
    let images = dir.path().join("images");
    std::fs::create_dir_all(&images).unwrap();
    std::os::unix::fs::symlink(
        &runtime_tree,
        images.join(PackageId::from_bytes(RUNTIME).to_hex()),
    )
    .unwrap();
    std::fs::write(
        images.join("current-runtime"),
        PackageId::from_bytes(RUNTIME).to_string(),
    )
    .unwrap();

    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_USH_BIN", env!("CARGO_BIN_EXE_tilo-ush"));
    env.set("TILO_USH_IMAGES", &images);
    env.set("TILO_ENV_TOOLS_STATE", build_state.path());
    env.set("SHELL", "/usr/bin/bash");

    let store = Store::at(dir.path().join("store"));
    store.ensure().expect("store");
    Records::at(store.records_root())
        .create(
            DEFAULT_RECORD,
            &Environment::new(
                "Fixture",
                "2026.10",
                tilo_environment::host_arch(),
                &PackageId::from_bytes(RUNTIME),
            ),
        )
        .unwrap();

    let socket = dir.path().join("ush.sock");
    let serve_socket = socket.clone();
    let serve_store = store.clone();
    std::thread::spawn(move || {
        let _ = ush::supervisor::serve(&serve_socket, &serve_store, &PathBuf::from("/"));
    });
    let deadline = tilo_test_env::hang_deadline();
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let (_stream, fd) = open_default_session(&socket);
    let type_line = |line: &str| {
        // SAFETY: `fd` is a live pty master.
        let n = unsafe { libc::write(fd, line.as_ptr().cast::<libc::c_void>(), line.len()) };
        assert!(n > 0, "could not write to the pty");
    };
    let prompt = read_until(fd, "Fixture", tilo_test_env::hang_bound());
    assert!(
        prompt.contains("Fixture"),
        "no shell came up in the guest:\n{prompt}"
    );

    type_line("command -v tree || printf 'NO_%s\\n' TREE; pkg install tree\r");
    // The next prompt: the install is done, one way or the other.
    let installed = read_until(fd, "]$ ", Duration::from_secs(600));
    assert!(
        installed.contains("NO_TREE"),
        "tree must not be there before:\n{installed}"
    );
    assert!(
        installed.contains("Run `restart`"),
        "`pkg install tree` must build and add the tool:\n{installed}"
    );
    assert_tool_recorded(&store, &images);

    type_line("restart\r");
    let restarted = read_until(fd, "Fixture", tilo_test_env::hang_bound());
    type_line("tree --version; printf 'DO%s\\n' NE\r");
    let after = read_until(fd, "DONE", tilo_test_env::hang_bound());
    assert!(
        after.contains("tree v"),
        "after `restart` the tool's command is on the PATH:\n{restarted}\n{after}"
    );
    // SAFETY: this test owns the descriptor.
    unsafe { libc::close(fd) };
}

/// The record names the built tool, and its image holds the command.
fn assert_tool_recorded(store: &Store, images: &Path) {
    let record = Records::at(store.records_root())
        .resolve(Some(DEFAULT_RECORD), &ush::images::DirImages::at(images))
        .unwrap()
        .1;
    assert_eq!(record.tools.len(), 1, "the record gains the tool");
    assert_eq!(record.tools[0].packages, ["tree"]);
    let tool = record.tools[0]
        .image(tilo_environment::host_arch())
        .unwrap()
        .expect("the tool has an image for this architecture");
    assert!(
        images.join(tool.to_hex()).join("usr/bin/tree").is_file(),
        "the tool image holds the command"
    );
}
