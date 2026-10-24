//! The path `apps/terminal` takes: ask the supervisor for a session on a
//! target over the service socket, receive a pty master, and drive a shell
//! through it.
//!
//! The guest is built from an environment record whose images are fixture
//! trees: a materialized native shell/runtime closure and a tool that
//! adds one command. The assertions are on bytes that came back from `bash`
//! running inside it, and on what it left on the host.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::missing_panics_doc,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
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

pub const RUNTIME: [u8; 32] = [0x11; 32];
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
            relay_session: None,
            runs: None,
            editor_protocol: None,
            end_report: false,
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
        let seen = read_until(a.as_raw_fd(), "never", Duration::from_millis(200));
        assert!(seen.is_empty());
        let mut sink = Vec::new();
        let _ = (&a).read_to_end(&mut sink);
    }
}

/// A device with no `env-runtime` installed: the default environment cannot
/// be made, so the supervisor says so instead of handing out a pty whose
/// guest prints the reason and exits — and keeps the connection open for the
/// app to ask for something else on it.
#[test]
fn without_a_runtime_the_open_is_refused_in_words_and_the_connection_stays_usable() {
    let _guest = GUEST_ENV.lock().expect("guest env lock");
    let dir = tempfile::tempdir().expect("tempdir");
    let images = dir.path().join("images");
    std::fs::create_dir_all(&images).unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_USH_IMAGES", &images);

    let store = Store::at(dir.path().join("store"));
    store.ensure().expect("store");
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

    let stream = UnixStream::connect(&socket).expect("connect");
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .expect("read timeout");
    let ask = |request: &ServeRequest| {
        let mut line = serde_json::to_vec(request).unwrap();
        line.push(b'\n');
        (&stream).write_all(&line).expect("write request");
        let (payload, fd) = recv_with_fd(&stream).expect("recvmsg");
        let text = String::from_utf8_lossy(&payload).into_owned();
        let response: ServeResponse =
            serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("bad reply {text:?}: {e}"));
        (response, fd)
    };
    let open = ServeRequest::OpenSession {
        target: Target::default_environment(),
        cols: 80,
        rows: 24,
        term: None,
        command: Vec::new(),
        requested_by: ush::host::RequestedBy::Person,
        relay_session: None,
        runs: None,
        editor_protocol: None,
        end_report: false,
    };

    let (response, fd) = ask(&open);
    assert!(
        fd.is_none(),
        "a pty was handed out for a guest that cannot be built"
    );
    let ServeResponse::NoRuntime { reason } = response else {
        panic!("expected a no-runtime refusal, got {response:?}");
    };
    assert!(
        reason.contains("env-runtime"),
        "the reason names what is missing: {reason}"
    );

    // The refusal spent nothing: the same connection is answered again.
    let (again, fd) = ask(&open);
    assert!(fd.is_none());
    assert!(
        matches!(again, ServeResponse::NoRuntime { .. }),
        "{again:?}"
    );
    assert!(
        Records::at(store.records_root())
            .load(DEFAULT_RECORD)
            .unwrap()
            .is_none(),
        "no half-made default environment is left behind"
    );
}

/// Lay out the fixture images under `root` the way the supervisor finds
/// them: `<hex id>/` trees and the current runtime's id.
pub fn fixture_images(root: &Path) {
    let runtime = root.join(PackageId::from_bytes(RUNTIME).to_hex());
    std::fs::create_dir_all(runtime.join("etc")).unwrap();
    for program in ["bash", "id", "touch", "cat"] {
        let source = PathBuf::from("/usr/bin").join(program);
        fixture_file(&runtime, &source);
        if let Some(closure) = ush::helper_runtime::HelperRuntime::resolve(&source).unwrap() {
            fixture_file(&runtime, &closure.loader);
            for library in closure.libraries.values() {
                fixture_file(&runtime, library);
            }
        }
    }
    std::os::unix::fs::symlink("bash", runtime.join("usr/bin/sh")).unwrap();
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

// Copy bytes into the fixture instead of putting another overlay-backed
// host /usr underneath it. Guest root aliases resolve /lib* into /usr/lib*.
fn fixture_file(runtime: &Path, source: &Path) {
    let relative = source.strip_prefix("/").unwrap();
    let relative = if relative.starts_with("lib") || relative.starts_with("lib64") {
        PathBuf::from("usr").join(relative)
    } else {
        relative.to_path_buf()
    };
    let destination = runtime.join(relative);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::copy(source, destination).unwrap();
}

/// Can this machine build the guest at all? The probe performs the mounts
/// the runtime performs — a user namespace mapping the real uid, `/usr` as a
/// read-only overlay of the materialized runtime and tool lowers. It probes
/// the subject the tests actually use, including on an overlay-backed host.
/// Whether this process can prepare a guest's network namespace: make one
/// and enter it, as the supervisor does before it starts any guest.
pub fn namespace_entry() -> std::io::Result<()> {
    tilo_egress::Netns::create()?.open_inside().map(drop)
}

/// Why no guest can run on this machine, or `None`.
pub fn guest_unavailable() -> Option<String> {
    let Some(bwrap) = which_bwrap() else {
        return Some("bwrap is not installed".into());
    };
    let dir = tempfile::tempdir().ok()?;
    fixture_images(dir.path());
    let lower = dir
        .path()
        .join(PackageId::from_bytes(TOOL).to_hex())
        .join("usr");
    let runtime = dir
        .path()
        .join(PackageId::from_bytes(RUNTIME).to_hex())
        .join("usr");
    let (uid, gid) = (
        rustix::process::geteuid().as_raw(),
        rustix::process::getegid().as_raw(),
    );
    let probe = std::process::Command::new(bwrap)
        .arg("--unshare-user")
        .args(["--uid", &uid.to_string(), "--gid", &gid.to_string()])
        .arg("--overlay-src")
        .arg(&lower)
        .arg("--overlay-src")
        .arg(&runtime)
        .args(["--ro-overlay", "/usr"])
        .args(["--symlink", "usr/bin", "/bin"])
        .args(["--symlink", "usr/sbin", "/sbin"])
        .args(["--symlink", "usr/lib", "/lib"])
        .args(["--symlink", "usr/lib64", "/lib64"])
        .args(["--proc", "/proc", "--dev", "/dev"])
        .args(["/usr/bin/bash", "-c", "exit 0"])
        .output();
    match probe {
        Ok(o) if o.status.success() => namespace_entry()
            .err()
            .map(|e| format!("the guest's network namespace cannot be prepared: {e}")),
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
/// A machine that cannot build the actual guest must fail this check.
#[test]
fn a_terminal_gets_a_shell_in_its_environment_over_the_service_socket() {
    let _guest = GUEST_ENV.lock().expect("guest env lock");
    tilo_test_env::require_capability!(
        NamespaceEntry,
        namespace_entry(),
        "the guest's network namespace"
    );
    assert_eq!(
        guest_unavailable(),
        None,
        "the native guest must be observable"
    );
    run_handoff();
}

/// A guest's shell starts as the Host's does: it is the session's login
/// shell, so it reads the environment home's `~/.bash_profile`; Tab completes
/// a unique path prefix on its pty; and it marks each run with the
/// session's key.
#[test]
fn a_guest_shell_is_a_login_shell_that_marks_runs_and_tab_completes() {
    let _guest = GUEST_ENV.lock().expect("guest env lock");
    tilo_test_env::require_capability!(
        NamespaceEntry,
        namespace_entry(),
        "the guest's network namespace"
    );
    assert_eq!(
        guest_unavailable(),
        None,
        "the native guest must be observable"
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let (_env, store, socket, _granted) = serve_fixture(dir.path());
    let home = store.environment_home(DEFAULT_RECORD);
    std::fs::create_dir_all(home.join("fixture-unique-directory")).unwrap();
    std::fs::write(
        home.join(".bash_profile"),
        "export FROM_PROFILE=login\n[ -f ~/.bashrc ] && . ~/.bashrc\n",
    )
    .unwrap();

    let key = "k3y0fth1ss3ss10n";
    let (_stream, master) = open_marked_session(&socket, Some(key));
    let fd = master.as_raw_fd();
    let prompt = read_until(fd, "Fixture", tilo_test_env::hang_bound());
    assert!(prompt.contains("Fixture"), "no prompt appeared:\n{prompt}");

    let line = "echo \"[$FROM_PROFILE]\" fixture-uni\t\r";
    // SAFETY: `fd` is a live pty master.
    let written = unsafe { libc::write(fd, line.as_ptr().cast::<libc::c_void>(), line.len()) };
    assert!(written > 0, "could not write to the pty");
    let expected = "[login] fixture-unique-directory/";
    let seen = read_until(fd, expected, tilo_test_env::hang_bound());
    assert!(
        seen.contains(expected),
        "the profile was not read or Tab did not complete:\n{seen}"
    );
    let mark = format!("\x1b]133;D;0;k={key}\x07");
    let seen = format!(
        "{seen}{}",
        read_until(fd, &mark, tilo_test_env::hang_bound())
    );
    assert!(
        seen.contains(&mark),
        "the guest's shell marks the run it finished:\n{seen:?}"
    );
}

/// The installed public spelling creates Default on first use and runs a
/// command through the actual native guest, without a host-shell grant.
#[test]
fn the_public_ush_alias_creates_default_and_runs_a_confined_command() {
    let _guest = GUEST_ENV.lock().expect("guest env lock");
    tilo_test_env::require_capability!(
        NamespaceEntry,
        namespace_entry(),
        "the guest's network namespace"
    );
    assert_eq!(
        guest_unavailable(),
        None,
        "the native guest must be observable"
    );
    let dir = tempfile::tempdir().unwrap();
    let images = dir.path().join("images");
    fixture_images(&images);
    let usr = dir.path().join("usr");
    std::fs::create_dir_all(usr.join("bin")).unwrap();
    std::fs::create_dir_all(usr.join("libexec")).unwrap();
    std::os::unix::fs::symlink(
        tilo_test_env::package_binary!("tilo-ush"),
        usr.join("libexec/tilo-ush"),
    )
    .unwrap();
    std::os::unix::fs::symlink("../libexec/tilo-ush", usr.join("bin/ush")).unwrap();
    let store = Store::at(dir.path().join("store"));
    let records = Records::at(store.records_root());
    assert!(records.load(DEFAULT_RECORD).unwrap().is_none());
    let output = std::process::Command::new("timeout")
        .arg("--kill-after=5s")
        .arg("30s")
        .arg(usr.join("bin/ush"))
        .args(["-c", "touch /usr/public-ush-probe 2>/dev/null || printf 'READ_ONLY\\n'; printf 'PUBLIC_%s\\n' DEFAULT"])
        .env(ush::store::STORE_ENV, store.root())
        .env_remove(ush::store::RECORDS_ENV)
        .env_remove("TILO_USH_BIN")
        .env("TILO_USH_IMAGES", &images)
        .env("SHELL", "/usr/bin/bash")
        .output().unwrap();
    assert!(
        output.status.success(),
        "public ush failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout, "READ_ONLY\nPUBLIC_DEFAULT\n");
    assert!(
        !images
            .join(PackageId::from_bytes(RUNTIME).to_hex())
            .join("usr/public-ush-probe")
            .exists()
    );
    let default = records
        .load(DEFAULT_RECORD)
        .unwrap()
        .expect("first use creates Default");
    assert_eq!(default.name, tilo_environment::DEFAULT_NAME);
    assert_eq!(
        default
            .runtime_image(tilo_environment::host_arch())
            .unwrap(),
        PackageId::from_bytes(RUNTIME)
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
fn open_default_session(socket: &Path) -> (UnixStream, OwnedFd) {
    open_marked_session(socket, None)
}

/// [`open_default_session`], whose shell marks its runs with `runs`.
fn open_marked_session(socket: &Path, runs: Option<&str>) -> (UnixStream, OwnedFd) {
    let stream = UnixStream::connect(socket).expect("connect");
    let mut request = serde_json::to_vec(&ServeRequest::OpenSession {
        target: Target::default_environment(),
        cols: 100,
        rows: 30,
        term: Some("xterm-256color".into()),
        command: Vec::new(),
        requested_by: ush::host::RequestedBy::Person,
        relay_session: None,
        runs: runs.map(str::to_owned),
        editor_protocol: None,
        end_report: false,
    })
    .unwrap();
    request.push(b'\n');
    (&stream).write_all(&request).expect("write request");
    (&stream).flush().expect("flush");

    let (payload, fd) = recv_with_fd(&stream).expect("recvmsg");
    let text = String::from_utf8_lossy(&payload);
    let response: ServeResponse =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("bad reply {text:?}: {e}"));
    match &response {
        ServeResponse::Session { environment, .. } => assert_eq!(environment, DEFAULT_RECORD),
        ServeResponse::Denied { reason }
        | ServeResponse::HostNotAllowed { reason }
        | ServeResponse::NoRuntime { reason } => {
            panic!("the service refused the session: {reason}")
        }
        ServeResponse::Environments { .. }
        | ServeResponse::Allowed
        | ServeResponse::Relays { .. } => {
            panic!("a session request answered with something else")
        }
    }
    let fd = fd.expect("no pty descriptor arrived with the reply");
    (stream, fd)
}

/// A supervisor serving the fixture environment from `dir`: the scoped
/// variables its sessions inherit, its store, its socket, and the directory
/// the record grants.
fn serve_fixture(dir: &Path) -> (tilo_test_env::ScopedEnv, Store, PathBuf, PathBuf) {
    let images = dir.join("images");
    fixture_images(&images);

    let mut env = tilo_test_env::ScopedEnv::new();
    // The supervisor re-execs itself to start a session. Under `cargo test`
    // `current_exe` is this test binary — point it at the real one.
    env.set("TILO_USH_BIN", tilo_test_env::package_binary!("tilo-ush"));
    env.set("TILO_USH_IMAGES", &images);
    env.set("SHELL", "/usr/bin/bash");

    let store = Store::at(dir.join("store"));
    store.ensure().expect("store");
    let granted = dir.join("granted");
    std::fs::create_dir_all(&granted).unwrap();
    let granted = granted.canonicalize().unwrap();

    fixture_environment(&store, &granted);

    let socket = dir.join("ush.sock");
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
    (env, store, socket, granted)
}

fn run_handoff() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (_env, store, socket, granted) = serve_fixture(dir.path());

    // The connection is the session's lifetime: it stays open until the end.
    let (_stream, master) = open_default_session(&socket);
    let fd = master.as_raw_fd();

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
    let uid = rustix::process::geteuid().as_raw();
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
}

/// A guest whose sandbox cannot be built says why in words under `bwrap`'s
/// own one-line reason; one that can be built runs without that notice.
/// Both halves assert: which one runs depends on the machine, as probed.
#[test]
fn a_sandbox_that_cannot_be_built_is_explained() {
    let _guest = GUEST_ENV.lock().expect("guest env lock");
    assert!(
        which_bwrap().is_some(),
        "the sandbox diagnostic test requires bwrap"
    );
    let unavailable = guest_unavailable();
    let dir = tempfile::tempdir().expect("tempdir");
    let images = dir.path().join("images");
    fixture_images(&images);
    let public_command = dir.path().join("ush");
    std::os::unix::fs::symlink(tilo_test_env::package_binary!("tilo-ush"), &public_command)
        .unwrap();
    let out = std::process::Command::new(&public_command)
        .args(["-c", "printf ush-command-output"])
        .env(ush::store::STORE_ENV, dir.path().join("store"))
        .env("TILO_USH_IMAGES", &images)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("tilo-ush runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let explained = stderr.contains("sandbox could not be set up");
    assert!(
        !stderr.contains("usually means"),
        "generic overlay advice must not mask the actual cause: {stderr}"
    );
    if let Some(reason) = unavailable {
        assert!(!out.status.success(), "{reason}; yet the guest ran");
        assert!(
            explained,
            "a guest that cannot mount ({reason}) must say so, not leave only \
             bwrap's line: {stderr}"
        );
    } else {
        assert!(out.status.success(), "the guest should run here: {stderr}");
        assert_eq!(
            out.stdout, b"ush-command-output",
            "ush -c output must be observable"
        );
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
    env.set("TILO_USH_BIN", tilo_test_env::package_binary!("tilo-ush"));
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
    let (_stream, master) = open_default_session(&socket);
    let fd = master.as_raw_fd();
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

struct ReexecSupervisor(std::process::Child);
impl Drop for ReexecSupervisor {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_reexec_supervisor(
    binary: &Path,
    stable: &Path,
    socket: &Path,
    store: &Store,
    images: &Path,
) -> ReexecSupervisor {
    let unit = include_str!("../../../../os/usr/lib/systemd/user/tilo-ush.service");
    let pin = unit
        .lines()
        .find_map(|line| line.strip_prefix("Environment=TILO_USH_BIN="));
    let mut command = std::process::Command::new(binary);
    command.env_remove("TILO_USH_BIN");
    if let Some(pin) = pin {
        assert_eq!(
            pin, "/usr/libexec/tilo-ush",
            "the service pins its installed executable"
        );
        command.env("TILO_USH_BIN", stable);
    }
    ReexecSupervisor(
        command
            .args(["serve", "--socket"])
            .arg(socket)
            .env("TILO_USH_HOME", store.root())
            .env("TILO_USH_IMAGES", images)
            .env("SHELL", "/usr/bin/bash")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    )
}

#[test]
fn the_shipped_service_reexecs_the_same_elf_when_its_launch_path_is_unlinked() {
    tilo_test_env::require_capability!(
        NamespaceEntry,
        namespace_entry(),
        "the guest's network namespace"
    );
    assert!(
        guest_unavailable().is_none(),
        "this regression must observe a real confined guest"
    );
    let dir = tempfile::tempdir().unwrap();
    let images = dir.path().join("images");
    fixture_images(&images);
    let store = Store::at(dir.path().join("store"));
    store.ensure().unwrap();
    let granted = dir.path().join("granted");
    std::fs::create_dir_all(&granted).unwrap();
    fixture_environment(&store, &granted);
    let binary = dir.path().join("tilo-ush");
    std::fs::copy(tilo_test_env::package_binary!("tilo-ush"), &binary).unwrap();
    let stable = dir.path().join("usr/libexec/tilo-ush");
    std::fs::create_dir_all(stable.parent().unwrap()).unwrap();
    std::fs::hard_link(&binary, &stable).unwrap();
    let socket = dir.path().join("ush.sock");
    let mut supervisor = start_reexec_supervisor(&binary, &stable, &socket, &store, &images);
    let deadline = tilo_test_env::hang_deadline();
    while !socket.exists() && Instant::now() < deadline {
        assert!(supervisor.0.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "actual supervisor must start");
    let running = PathBuf::from(format!("/proc/{}/exe", supervisor.0.id()));
    let original = std::fs::metadata(&binary).unwrap();
    let observed = std::fs::metadata(&running).unwrap();
    let pinned = std::fs::metadata(&stable).unwrap();
    assert_eq!(
        (pinned.dev(), pinned.ino()),
        (original.dev(), original.ino()),
        "the pin must retain the exact running ELF, not another build"
    );
    assert_eq!(
        (observed.dev(), observed.ino()),
        (original.dev(), original.ino())
    );
    std::fs::remove_file(&binary).unwrap();
    let misleading = std::fs::read_link(&running).unwrap();
    assert!(
        !misleading.exists(),
        "current_exe cannot be reexecuted in this fixture"
    );
    let (stream, fd) = open_default_session(&socket);
    let prompt = read_until(fd.as_raw_fd(), "Fixture", tilo_test_env::hang_bound());
    assert!(
        prompt.contains("Fixture"),
        "same-build reexec must return an actual shell: {prompt}"
    );
    let script =
        b"printf 'REEXEC_%s\\n' SAME_ELF; test ! -w /usr && printf 'USR_%s\\n' READ_ONLY; exit\r";
    // SAFETY: the owned master is live and script is a bounded byte slice.
    assert_eq!(
        unsafe { libc::write(fd.as_raw_fd(), script.as_ptr().cast(), script.len()) },
        isize::try_from(script.len()).unwrap()
    );
    let seen = read_until(fd.as_raw_fd(), "USR_READ_ONLY", tilo_test_env::hang_bound());
    assert!(
        seen.contains("REEXEC_SAME_ELF"),
        "guest answered through the real PTY: {seen}"
    );
    assert!(
        seen.contains("USR_READ_ONLY"),
        "guest confinement remains active: {seen}"
    );
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut lifetime = &stream;
    assert_eq!(
        lifetime.read(&mut [0_u8; 1]).unwrap(),
        0,
        "the service retires and reaps the exited guest before teardown"
    );
}
