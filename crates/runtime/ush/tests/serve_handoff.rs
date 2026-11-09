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
use tilo_test_env::SpawnUnderEnvLock as _;

use tilo_environment::{DEFAULT_RECORD, Environment, PackageId, Target};
use ush::records::Records;
use ush::store::Store;
use ush::supervisor::{ServeRequest, ServeResponse, decide_serve, recv_with_fd};

/// Whether this machine runs the helper from a native loader it may trust: a
/// nested runner that maps the host's root-owned files to its caller owns them
/// as root-equivalent, and the helper refuses every host loader there.
macro_rules! require_trusted_loader {
    () => {
        tilo_test_env::require_capability!(
            TrustedLoader,
            ush::helper_runtime::native_loader_trusted(),
            "the helper's native loader"
        );
    };
}
pub(crate) use require_trusted_loader;

/// The prologue of a test that drives a guest: the environment lock its
/// children inherit, the sandbox it runs in (a user namespace, the permission
/// to enter it, and a native loader the helper may trust), and proof that this
/// machine builds the guest.
macro_rules! require_guest {
    () => {
        let _guest = GUEST_ENV.lock().expect("guest env lock");
        tilo_test_env::require_capability!(
            UserNamespace,
            tilo_test_env::bubblewrap(),
            "the guest's sandbox"
        );
        tilo_test_env::require_capability!(
            NamespaceEntry,
            namespace_entry(),
            "the guest's network namespace"
        );
        require_trusted_loader!();
        assert_eq!(
            guest_unavailable(),
            None,
            "the native guest must be observable"
        );
    };
}

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

        serve_in_background(&socket, &store);

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
    serve_in_background(&socket, &store);

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
/// them: `<hex id>/` trees and the current runtime's id. The closures come
/// from the helper's own resolution, so a machine whose host loader the
/// helper refuses fails here with that reason.
pub fn fixture_images(root: &Path) -> std::io::Result<()> {
    let runtime = root.join(PackageId::from_bytes(RUNTIME).to_hex());
    std::fs::create_dir_all(runtime.join("etc"))?;
    // base64 and tr: the run marks encode a command line and its directory.
    // stat: what a guest's own directories are.
    for program in ["bash", "id", "touch", "cat", "base64", "tr", "stat"] {
        let source = PathBuf::from("/usr/bin").join(program);
        fixture_file(&runtime, &source)?;
        if let Some(closure) = ush::helper_runtime::HelperRuntime::resolve(&source)? {
            fixture_file(&runtime, &closure.loader)?;
            for library in closure.libraries.values() {
                fixture_file(&runtime, library)?;
            }
        }
    }
    std::os::unix::fs::symlink("bash", runtime.join("usr/bin/sh"))?;
    std::fs::write(
        runtime.join("etc/passwd"),
        "root:x:0:0:root:/root:/bin/bash\n",
    )?;
    std::fs::write(runtime.join("etc/group"), "root:x:0:\n")?;

    let tool = root.join(PackageId::from_bytes(TOOL).to_hex());
    std::fs::create_dir_all(tool.join("usr/bin"))?;
    let command = tool.join("usr/bin").join(TOOL_COMMAND);
    std::fs::write(&command, "#!/bin/sh\necho \"from the tool image\"\n")?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::write(
        root.join("current-runtime"),
        PackageId::from_bytes(RUNTIME).to_string(),
    )?;
    Ok(())
}

// Copy bytes into the fixture instead of putting another overlay-backed
// host /usr underneath it. Guest root aliases resolve /lib* into /usr/lib*.
fn fixture_file(runtime: &Path, source: &Path) -> std::io::Result<()> {
    let relative = source.strip_prefix("/").map_err(std::io::Error::other)?;
    let relative = if relative.starts_with("lib") || relative.starts_with("lib64") {
        PathBuf::from("usr").join(relative)
    } else {
        relative.to_path_buf()
    };
    let destination = runtime.join(relative);
    std::fs::create_dir_all(destination.parent().unwrap_or(runtime))?;
    std::fs::copy(source, destination).map(drop)
}

/// Whether this process can prepare a guest's network namespace: make one
/// and enter it, as the supervisor does before it starts any guest.
pub fn namespace_entry() -> std::io::Result<()> {
    tilo_egress::Netns::create()?.open_inside().map(drop)
}

/// Why no guest can run on this machine, or `None`: the probe performs the
/// mounts the runtime performs — a user namespace mapping the real uid, `/usr`
/// as a read-only overlay of the materialized runtime and tool lowers. It
/// probes the subject the tests actually use, including on an overlay-backed
/// host.
pub fn guest_unavailable() -> Option<String> {
    let Some(bwrap) = which_bwrap() else {
        return Some("bwrap is not installed".into());
    };
    let dir = tempfile::tempdir().ok()?;
    if let Err(error) = fixture_images(dir.path()) {
        return Some(format!(
            "the guest's fixture images cannot be built: {error}"
        ));
    }
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
        .output_locked();
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
    require_guest!();
    run_handoff();
}

/// A new session's shell is ush's own, on the Tilo side: `ls | wc -l` runs
/// as two of Tilo's own commands over the granted files, and the run is
/// marked with its line, directory and status.
#[test]
fn a_new_session_runs_ushs_own_shell_and_marks_its_runs() {
    require_guest!();
    let dir = tempfile::tempdir().expect("tempdir");
    let (_env, _store, socket, granted) = serve_fixture(dir.path());
    for name in ["one", "two", "three"] {
        std::fs::write(granted.join(name), name).unwrap();
    }
    let key = "k3y0fth1ss3ss10n";
    let (_stream, master) = open_marked_session(&socket, Some(key));
    let fd = master.as_raw_fd();
    let prompt = format!("\x1b]133;A;k={key};in=1\x07");
    let seen = read_until(fd, &prompt, tilo_test_env::hang_bound());
    assert!(seen.contains(&prompt), "no prompt mark:\n{seen:?}");

    let line = format!("cd {} && ls | wc -l\r", granted.display());
    // SAFETY: `fd` is a live pty master.
    let written = unsafe { libc::write(fd, line.as_ptr().cast::<libc::c_void>(), line.len()) };
    assert!(written > 0, "could not write to the pty");
    let ended = format!("\x1b]133;D;0;k={key};t=1\x07");
    let seen = read_until(fd, &ended, tilo_test_env::hang_bound());
    assert!(
        seen.contains(&ended),
        "the run is marked as it ends:\n{seen:?}"
    );
    assert!(seen.contains("133;C;"), "and as it starts:\n{seen:?}");
    let printed = seen.rsplit('\x07').nth(1).unwrap_or_default();
    assert!(printed.contains('3'), "three files in the grant:\n{seen:?}");
}

/// The sessions' directories in which a guest was set up: a guest's root
/// gets its generated `/etc` there when it starts.
fn guests_started(store: &Store) -> usize {
    let sessions = store.session_dir("probe");
    let Some(sessions) = sessions.parent() else {
        return 0;
    };
    std::fs::read_dir(sessions).map_or(0, |entries| {
        entries
            .flatten()
            .filter(|entry| entry.path().join("root/etc/passwd").is_file())
            .count()
    })
}

/// A new session's shell runs on the Tilo side: Tilo's own commands and a
/// redirect into the grant need no guest, and the redirect is the
/// environment's action. The first program starts the session's guest and
/// runs in it, its output piped back; the next ones run in the same guest.
#[test]
fn a_tilo_side_session_starts_its_guest_for_the_first_program_only() {
    require_guest!();
    let dir = tempfile::tempdir().expect("tempdir");
    let (_env, store, socket, granted) = serve_fixture(dir.path());
    std::fs::write(granted.join("notes.md"), "one\ntwo\n").unwrap();
    let key = "k3y0fth1ss3ss10n";
    let (_stream, master) = open_marked_session(&socket, Some(key));
    let fd = master.as_raw_fd();
    let prompt = format!("\x1b]133;A;k={key};in=1\x07");
    let seen = read_until(fd, &prompt, tilo_test_env::hang_bound());
    assert!(seen.contains(&prompt), "no prompt mark:\n{seen:?}");

    let type_line = |line: &str| {
        // SAFETY: `fd` is a live pty master.
        let written = unsafe { libc::write(fd, line.as_ptr().cast::<libc::c_void>(), line.len()) };
        assert!(written > 0, "could not write to the pty");
    };
    type_line(&format!(
        "cd {} && grep -c . notes.md && echo hi > made.txt\r",
        granted.display()
    ));
    let ended = format!("\x1b]133;D;0;k={key};t=1\x07");
    let seen = read_until(fd, &ended, tilo_test_env::hang_bound());
    assert!(seen.contains(&ended), "the first line ended:\n{seen:?}");
    assert_eq!(
        std::fs::read_to_string(granted.join("made.txt")).unwrap(),
        "hi\n"
    );
    let records = Records::at(store.records_root());
    let writes = records
        .history(DEFAULT_RECORD)
        .unwrap()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|context| {
            matches!(
                &context.what,
                Some(tilo_actions::Did::Verb(call)) if call.action.action == "write"
            )
        })
        .count();
    assert_eq!(writes, 1, "the redirect is the environment's write");
    assert_eq!(
        guests_started(&store),
        0,
        "no guest for Tilo's own commands"
    );

    type_line(&format!(
        "id -u; {TOOL_COMMAND}; sh -c 'echo PARENT_$PPID'; sh -c 'echo PARENT_$PPID'\r"
    ));
    let ended = format!("\x1b]133;D;0;k={key};t=2\x07");
    let seen = read_until(fd, &ended, tilo_test_env::hang_bound());
    assert!(seen.contains(&ended), "the programs ended:\n{seen:?}");
    let uid = rustix::process::geteuid().as_raw();
    assert!(seen.contains(&uid.to_string()), "`id -u` ran:\n{seen}");
    assert!(
        seen.contains("from the tool image"),
        "the environment's tool ran in its guest:\n{seen}"
    );
    let parents: Vec<&str> = seen
        .split("PARENT_")
        .skip(1)
        .filter_map(|rest| rest.split_whitespace().next())
        .filter(|pid| pid.chars().all(|c| c.is_ascii_digit()))
        .collect();
    assert_eq!(parents.len(), 2, "{seen}");
    assert_eq!(
        parents[0], parents[1],
        "one guest serves the session:\n{seen}"
    );
    assert_eq!(
        guests_started(&store),
        1,
        "started once, for the first program"
    );
}

/// The installed public spelling creates Default on first use and runs a
/// command through the actual native guest, without a host-shell grant.
#[test]
fn the_public_ush_alias_creates_default_and_runs_a_confined_command() {
    require_guest!();
    let dir = tempfile::tempdir().unwrap();
    let images = dir.path().join("images");
    fixture_images(&images).expect("the guest's fixture images");
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
        .output_locked().unwrap();
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

/// Ask for the default environment running `bash` and receive its pty
/// master: bash as an ordinary program in the session's guest, which is
/// what these tests type at.
fn open_default_session(socket: &Path) -> (UnixStream, OwnedFd) {
    open_session(socket, None, vec!["bash".into()])
}

/// The default environment in ush's shell, marking its runs with `runs`.
fn open_marked_session(socket: &Path, runs: Option<&str>) -> (UnixStream, OwnedFd) {
    open_session(socket, runs, Vec::new())
}

/// The default environment running `command` (ush's shell when empty).
fn open_session(socket: &Path, runs: Option<&str>, command: Vec<String>) -> (UnixStream, OwnedFd) {
    let stream = UnixStream::connect(socket).expect("connect");
    let mut request = serde_json::to_vec(&ServeRequest::OpenSession {
        target: Target::default_environment(),
        cols: 100,
        rows: 30,
        term: Some("xterm-256color".into()),
        command,
        requested_by: ush::host::RequestedBy::Person,
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
        | ServeResponse::Branched { .. } => {
            panic!("a session request answered with something else")
        }
    }
    let fd = fd.expect("no pty descriptor arrived with the reply");
    (stream, fd)
}

/// Serve the supervisor on `socket` with `store` in the background, and wait
/// until it bound the socket.
fn serve_in_background(socket: &Path, store: &Store) {
    let (serve_socket, serve_store) = (socket.to_owned(), store.clone());
    std::thread::spawn(move || {
        let _ = ush::supervisor::serve(&serve_socket, &serve_store, &PathBuf::from("/"));
    });
    let deadline = tilo_test_env::hang_deadline();
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "the supervisor never bound its socket");
}

/// A supervisor serving the fixture environment from `dir`: the scoped
/// variables its sessions inherit, its store, its socket, and the directory
/// the record grants.
fn serve_fixture(dir: &Path) -> (tilo_test_env::ScopedEnv, Store, PathBuf, PathBuf) {
    let images = dir.join("images");
    fixture_images(&images).expect("the guest's fixture images");

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
    serve_in_background(&socket, &store);
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

const W1: &str = "3f2504e0-4f89-11d3-9a0c-0305e82c3301";
const W2: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";

/// A supervisor over the fixture images whose device files are `<dir>/files`:
/// its scoped variables, its store, its socket and the files folder.
fn serve_with_files(dir: &Path) -> (tilo_test_env::ScopedEnv, Store, PathBuf, PathBuf) {
    let images = dir.join("images");
    fixture_images(&images).expect("the guest's fixture images");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_USH_BIN", tilo_test_env::package_binary!("tilo-ush"));
    env.set("TILO_USH_IMAGES", &images);
    env.set("SHELL", "/usr/bin/bash");
    let files = dir.join("files");
    std::fs::create_dir_all(&files).unwrap();
    let files = files.canonicalize().unwrap();
    let store = Store::at(dir.join("store")).with_workspace(&files);
    store.ensure().expect("store");
    let socket = dir.join("ush.sock");
    serve_in_background(&socket, &store);
    (env, store, socket, files)
}

/// A connection as `tilo-launcher` hands Terminal launched in `workspace`
/// its `ush` service.
fn launched_in(socket: &Path, workspace: Option<&str>) -> UnixStream {
    let stream = UnixStream::connect(socket).expect("connect");
    (&stream)
        .write_all(&ush::host::encode_launch_handoff("so.tilo.terminal", workspace).unwrap())
        .unwrap();
    stream
}

/// One request line on `stream`, answered in one line.
fn ask_line(stream: &UnixStream, request: &serde_json::Value) -> ServeResponse {
    let mut line = serde_json::to_vec(request).unwrap();
    line.push(b'\n');
    (&*stream).write_all(&line).unwrap();
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).unwrap();
    serde_json::from_str(reply.trim()).unwrap_or_else(|e| panic!("bad reply {reply:?}: {e}"))
}

fn listed(response: &ServeResponse) -> Vec<String> {
    match response {
        ServeResponse::Environments { environments } => {
            environments.iter().map(|e| e.name.clone()).collect()
        }
        other => panic!("not a listing: {other:?}"),
    }
}

/// A Terminal opened in a workspace lists and makes that workspace's
/// environments only, kept in its room; it cannot name another workspace,
/// and the environments made before workspaces stay the primary
/// workspace's, listed where no workspace is named.
#[test]
fn each_workspace_lists_its_own_environments_and_an_app_cannot_name_another() {
    let _guest = GUEST_ENV.lock().expect("guest env lock");
    require_trusted_loader!();
    let dir = tempfile::tempdir().expect("tempdir");
    let (_env, store, socket, _files) = serve_with_files(dir.path());
    let legacy = Records::at(store.records_root());
    legacy
        .create(
            "env-from-before",
            &Environment::new(
                "From before",
                "2026.10",
                tilo_environment::host_arch(),
                &PackageId::from_bytes(RUNTIME),
            ),
        )
        .unwrap();

    let create = |workspace: Option<&str>, name: &str| {
        ask_line(
            &launched_in(&socket, workspace),
            &serde_json::json!({"op": "create-environment", "name": name}),
        )
    };
    assert_eq!(listed(&create(Some(W1), "Agent one")), vec!["Agent one"]);
    assert_eq!(listed(&create(Some(W2), "Agent two")), vec!["Agent two"]);
    let list = serde_json::json!({"op": "list-environments"});
    assert_eq!(
        listed(&ask_line(&launched_in(&socket, Some(W1)), &list)),
        vec!["Agent one"]
    );
    // The shell's own process names the workspace on the request.
    let session = UnixStream::connect(&socket).unwrap();
    assert_eq!(
        listed(&ask_line(
            &session,
            &serde_json::json!({"op": "list-environments", "workspace": W2})
        )),
        vec!["Agent two"]
    );
    assert_eq!(
        listed(&ask_line(&launched_in(&socket, None), &list)),
        vec!["From before"],
        "the primary workspace's, unmoved"
    );

    // Terminal launched in W1 asks for W2's: refused, in words.
    let refused = ask_line(
        &launched_in(&socket, Some(W1)),
        &serde_json::json!({"op": "list-environments", "workspace": W2}),
    );
    assert!(
        matches!(&refused, ServeResponse::Denied { reason } if reason.contains(W2)),
        "{refused:?}"
    );
    let refused = ask_line(&launched_in(&socket, Some("../personal")), &list);
    assert!(
        matches!(refused, ServeResponse::Denied { .. }),
        "{refused:?}"
    );

    // Each in its own room, naming its workspace.
    for (workspace, name) in [(W1, "Agent one"), (W2, "Agent two")] {
        let scoped =
            store.for_workspace(&ush::workspace::WorkspaceId::parse(Some(workspace)).unwrap());
        let records = Records::of(&scoped);
        let keys = records.keys().unwrap();
        assert_eq!(keys.len(), 1, "{workspace}: {keys:?}");
        let env = records.load(&keys[0]).unwrap().unwrap();
        assert_eq!(
            (env.name.as_str(), env.workspace.as_str()),
            (name, workspace)
        );
        assert!(
            records.record_dir(&keys[0]).unwrap().starts_with(
                store
                    .records_root()
                    .join("so.tilo.terminal/rooms")
                    .join(workspace)
            )
        );
    }
    assert_eq!(legacy.keys().unwrap(), vec!["env-from-before"]);
}

/// Open the default environment of `workspace`, as Terminal launched there,
/// running `bash`.
fn open_in_workspace(socket: &Path, workspace: &str) -> (UnixStream, OwnedFd) {
    let stream = launched_in(socket, Some(workspace));
    let mut request = serde_json::to_vec(&ServeRequest::OpenSession {
        target: Target::default_environment(),
        cols: 100,
        rows: 30,
        term: Some("xterm-256color".into()),
        command: vec!["bash".into()],
        requested_by: ush::host::RequestedBy::Person,
        runs: None,
        editor_protocol: None,
        end_report: false,
    })
    .unwrap();
    request.push(b'\n');
    (&stream).write_all(&request).unwrap();
    let (payload, fd) = recv_with_fd(&stream).expect("recvmsg");
    let text = String::from_utf8_lossy(&payload);
    let response: ServeResponse = serde_json::from_str(text.trim()).unwrap();
    assert!(
        matches!(&response, ServeResponse::Session { environment, .. } if environment == DEFAULT_RECORD),
        "{response:?}"
    );
    (
        stream,
        fd.expect("no pty descriptor arrived with the reply"),
    )
}

/// Two Terminals in two workspaces: two environments running side by side,
/// each with its own workspace's files as `~`, told which workspace and
/// environment it is, and unable to read the other's files or the device's
/// files around them.
#[test]
fn terminals_in_two_workspaces_run_two_environments_each_with_its_own_files() {
    require_guest!();
    let dir = tempfile::tempdir().expect("tempdir");
    let (_env, _store, socket, files) = serve_with_files(dir.path());
    let tree = |id: &str| files.join(".workspaces").join(id);
    std::fs::create_dir_all(files.join("Documents")).unwrap();
    std::fs::write(files.join("Documents/private.txt"), "DEVICE_FILE\n").unwrap();

    // Both open before either is used: they run at once.
    let (_one, w1) = open_in_workspace(&socket, W1);
    let (_two, w2) = open_in_workspace(&socket, W2);
    for (fd, own, other) in [(&w1, W1, W2), (&w2, W2, W1)] {
        std::fs::create_dir_all(tree(other)).unwrap();
        std::fs::write(
            tree(other).join("secret.txt"),
            format!("SECRET_OF_{other}\n"),
        )
        .unwrap();
        let script = format!(
            "printf 'WS_%s_\\n' \"$TILO_WORKSPACE\"; printf 'ENV_%s_\\n' \"$TILO_ENVIRONMENT\"; \
             printf 'HOME_%s_\\n' \"$HOME\"; echo mine-{own} > ~/mine; \
             cat {other_secret} 2>/dev/null || printf 'NO_%s\\n' OTHER; \
             cat {device} 2>/dev/null || printf 'NO_%s\\n' DEVICE; printf 'DO%s\\n' NE\r",
            other_secret = tree(other).join("secret.txt").display(),
            device = files.join("Documents/private.txt").display(),
        );
        let fd = fd.as_raw_fd();
        // SAFETY: `fd` is a live pty master.
        let written =
            unsafe { libc::write(fd, script.as_ptr().cast::<libc::c_void>(), script.len()) };
        assert!(written > 0, "could not write to the pty");
        let seen = read_until(fd, "DONE", tilo_test_env::hang_bound());
        assert!(
            seen.contains("DONE"),
            "{own}: the shell never finished:\n{seen}"
        );
        assert!(seen.contains(&format!("WS_{own}_")), "{own}:\n{seen}");
        assert!(
            seen.contains(&format!("ENV_{own}/default_")),
            "{own}:\n{seen}"
        );
        assert!(
            seen.contains(&format!("HOME_{}_", tree(own).display())),
            "{own}: ~ is the workspace's own files:\n{seen}"
        );
        assert!(
            seen.contains("NO_OTHER"),
            "{own} read {other}'s files:\n{seen}"
        );
        assert!(
            !seen.contains(&format!("SECRET_OF_{other}\r")),
            "{own}:\n{seen}"
        );
        assert!(
            seen.contains("NO_DEVICE"),
            "{own} read the device's files:\n{seen}"
        );
        assert!(!seen.contains("DEVICE_FILE\r"), "{own}:\n{seen}");
    }
    for own in [W1, W2] {
        assert_eq!(
            std::fs::read_to_string(tree(own).join("mine")).unwrap(),
            format!("mine-{own}\n"),
            "written into the workspace's own files"
        );
    }
}

/// A session whose guest cannot be set up writes nothing on its terminal:
/// the reason is the supervisor's to log, and the app reads on the
/// session's lease that the guest was not set up, not that a shell exited.
#[test]
fn a_guest_that_cannot_be_set_up_says_so_on_its_lease_and_nothing_on_its_terminal() {
    require_guest!();
    let dir = tempfile::tempdir().expect("tempdir");
    let (mut env, store, socket, _granted) = serve_fixture(dir.path());
    // A record that grants a host cannot start where the kernel refuses the
    // packet filter: setup fails after the terminal was handed over.
    env.set("TILO_USH_EGRESS_REFUSE_NETFILTER", "1");
    Records::at(store.records_root())
        .apply(
            DEFAULT_RECORD,
            Environment::ALLOW_HOST.with("granted.test".to_owned()),
        )
        .unwrap();

    let stream = UnixStream::connect(&socket).expect("connect");
    (&stream)
        .write_all(b"{\"op\":\"hello\",\"protocol\":2}\n")
        .unwrap();
    let (ready, _) = runtime_portal::fd_transport::recv_with_fds(&stream).expect("hello answer");
    assert!(
        String::from_utf8_lossy(&ready).contains("\"ready\""),
        "{}",
        String::from_utf8_lossy(&ready)
    );
    let mut request = serde_json::to_vec(&ServeRequest::OpenSession {
        target: Target::default_environment(),
        cols: 100,
        rows: 30,
        term: Some("xterm-256color".into()),
        // A program: ush's own shell starts the guest only for the first
        // program it runs, so a session without one never sets a guest up.
        command: vec!["bash".into()],
        requested_by: ush::host::RequestedBy::Person,
        runs: None,
        editor_protocol: None,
        end_report: true,
    })
    .unwrap();
    request.push(b'\n');
    (&stream).write_all(&request).unwrap();
    let (payload, mut rights) =
        runtime_portal::fd_transport::recv_with_fds(&stream).expect("session answer");
    assert_eq!(
        rights.len(),
        2,
        "a pty and a lease: {}",
        String::from_utf8_lossy(&payload)
    );
    let lease = UnixStream::from(rights.pop().unwrap());
    let master = rights.pop().unwrap();

    // The lease says how the session ended, then closes.
    lease
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .unwrap();
    let mut report = String::new();
    (&lease)
        .read_to_string(&mut report)
        .expect("the lease closes after its report");
    let report: serde_json::Value = serde_json::from_str(report.trim())
        .unwrap_or_else(|e| panic!("not one report line {report:?}: {e}"));
    assert_eq!(report["ended"], "failed", "{report}");
    assert!(
        report["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("network filter")),
        "the reason, for the log, names the cause: {report}"
    );

    // The guest's terminal shows none of it.
    let seen = read_until(master.as_raw_fd(), "\u{0}", Duration::from_millis(500));
    assert!(
        seen.trim().is_empty(),
        "setup errors are never written on the person's terminal:\n{seen}"
    );
}

/// A guest whose sandbox cannot be built says why in words under `bwrap`'s
/// own one-line reason; one that can be built runs without that notice.
/// Both halves assert: which one runs depends on the machine, as probed.
///
/// The command runs a program, not a built-in: the session's shell answers
/// `printf` itself, on the Tilo side, and only a program starts the guest
/// whose sandbox this test is about.
#[test]
fn a_sandbox_that_cannot_be_built_is_explained() {
    let _guest = GUEST_ENV.lock().expect("guest env lock");
    require_trusted_loader!();
    assert!(
        which_bwrap().is_some(),
        "the sandbox diagnostic test requires bwrap"
    );
    let unavailable = guest_unavailable();
    let dir = tempfile::tempdir().expect("tempdir");
    let images = dir.path().join("images");
    fixture_images(&images).expect("the guest's fixture images");
    let public_command = dir.path().join("ush");
    std::os::unix::fs::symlink(tilo_test_env::package_binary!("tilo-ush"), &public_command)
        .unwrap();
    let out = std::process::Command::new(&public_command)
        .args(["-c", "bash -c 'printf ush-command-output'"])
        .env(ush::store::STORE_ENV, dir.path().join("store"))
        .env("TILO_USH_IMAGES", &images)
        .stdin(std::process::Stdio::null())
        .output_locked()
        .expect("tilo-ush runs");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let explained = stderr.contains("sandbox could not be set up");
    assert!(
        !stderr.contains("usually means"),
        "generic overlay advice must not mask the actual cause: {stderr}"
    );
    if let Some(reason) = unavailable {
        assert!(
            !out.status.success(),
            "{reason}; yet the guest ran: {stderr}"
        );
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
/// its build sandbox; the record gains the tool; the next session's guest has
/// its command on the PATH. Needs a materialised env-runtime tree
/// (`TILO_ENV_RUNTIME_TREE`) and the network for the package download.
#[test]
fn pkg_install_builds_a_tool_that_is_on_path_in_the_next_session() {
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
    serve_in_background(&socket, &store);
    let (stream, master) = open_default_session(&socket);
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

    // The next session's guest has the tool.
    drop(stream);
    drop(master);
    let (_stream, master) = open_default_session(&socket);
    let fd = master.as_raw_fd();
    let type_line = |line: &str| {
        // SAFETY: `fd` is a live pty master.
        let n = unsafe { libc::write(fd, line.as_ptr().cast::<libc::c_void>(), line.len()) };
        assert!(n > 0, "could not write to the pty");
    };
    let reopened = read_until(fd, "Fixture", tilo_test_env::hang_bound());
    type_line("tree --version; printf 'DO%s\\n' NE\r");
    let after = read_until(fd, "DONE", tilo_test_env::hang_bound());
    assert!(
        after.contains("tree v"),
        "in the next session the tool's command is on the PATH:\n{reopened}\n{after}"
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
            .spawn_locked()
            .unwrap(),
    )
}

#[test]
fn the_shipped_service_reexecs_the_same_elf_when_its_launch_path_is_unlinked() {
    require_guest!();
    let dir = tempfile::tempdir().unwrap();
    let images = dir.path().join("images");
    fixture_images(&images).expect("the guest's fixture images");
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

/// A guest reaches the desktop through its environment's own agent door: `ush` asks the session
/// for it as the environment opens, binds the socket the session answered at
/// `/run/tilo/agent.sock` and names it in `TILO_AGENT_SOCK`, and the guest's `tilo-runtime
/// --connect` relays to it. The connection comes from a descendant of the process holding the
/// door's lease, the host path is not in the guest, and the lease closes when the environment
/// ends.
#[test]
fn a_guest_reaches_the_session_through_its_own_agent_door() {
    require_guest!();
    let dir = tempfile::tempdir().unwrap();
    let images = dir.path().join("images");
    fixture_images(&images).expect("the guest's fixture images");
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

    // The session: its own door, which opens the environment's.
    let runtime = dir.path().join("rt");
    let session_dir = runtime.join(tilo_paths::login::SESSION_DIR);
    std::fs::create_dir_all(session_dir.join(tilo_paths::login::ENVIRONMENT_DOORS_DIR)).unwrap();
    let door = session_dir.join(tilo_paths::login::AGENT_SOCKET_FILE);
    let environment_door = session_dir
        .join(tilo_paths::login::ENVIRONMENT_DOORS_DIR)
        .join("e1.sock");
    let session = fake_session(&door, environment_door.clone());

    let command = format!(
        "printf 'SOCK=%s\\n' \"$TILO_AGENT_SOCK\"; test -e '{}' || printf 'UNSEEN\\n'; \
         printf '{{\"hello\":1}}\\n' | tilo-runtime --connect",
        environment_door.display()
    );
    let output = std::process::Command::new("timeout")
        .arg("--kill-after=5s")
        .arg("30s")
        .arg(usr.join("bin/ush"))
        .args(["-c", &command])
        .env(ush::store::STORE_ENV, store.root())
        .env_remove(ush::store::RECORDS_ENV)
        .env_remove("TILO_USH_BIN")
        .env("TILO_USH_IMAGES", &images)
        .env("SHELL", "/usr/bin/bash")
        .env("XDG_RUNTIME_DIR", &runtime)
        .output_locked()
        .unwrap();
    assert!(
        output.status.success(),
        "ush failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout,
        format!(
            "SOCK={}\nUNSEEN\n{{\"answered\":1}}\n",
            tilo_paths::login::GUEST_AGENT_SOCKET
        ),
        "the guest's own door, not the host's path, answered"
    );
    let (asked, inside, said) = session.join().unwrap();
    assert_eq!(
        asked.environment,
        format!("{}/{DEFAULT_RECORD}", ush::workspace::PRIMARY)
    );
    assert_eq!(asked.workspace, None, "the primary workspace names none");
    assert!(
        inside,
        "the guest's program descends from the lease's holder"
    );
    assert_eq!(said, "{\"hello\":1}\n");
}

/// A session's agent door at `door`, as the session serves it to one environment: it answers the
/// environment's request with a door at `serving`, answers the one program that connects there,
/// and waits for the lease to close. What the environment asked, whether the program descended
/// from the lease's holder, and the line it sent.
fn fake_session(
    door: &Path,
    serving: PathBuf,
) -> std::thread::JoinHandle<(runtime_portal::environment_door::Request, bool, String)> {
    use runtime_portal::environment_door::{Reply, Request};
    let listener = tilo_files::native::socket::bind(door).unwrap();
    std::thread::spawn(move || {
        let (lease, _) = listener.accept().unwrap();
        let holder = peer_pid(&lease);
        let mut line = Vec::new();
        BufReader::new(lease.try_clone().unwrap())
            .read_until(b'\n', &mut line)
            .unwrap();
        let asked = Request::of(line.trim_ascii_end()).expect("an environment's door request");
        let own = tilo_files::native::socket::bind(&serving).unwrap();
        (&lease)
            .write_all(
                &Reply::Opened {
                    path: serving,
                    workspace: "front".into(),
                }
                .line(),
            )
            .unwrap();
        let (client, _) = own.accept().unwrap();
        let inside = descends_from(peer_pid(&client), holder);
        let mut said = String::new();
        BufReader::new(client.try_clone().unwrap())
            .read_line(&mut said)
            .unwrap();
        (&client).write_all(b"{\"answered\":1}\n").unwrap();
        drop(client);
        // The lease stays open while the environment runs, and closes as it ends.
        let mut rest = Vec::new();
        let _ = (&lease).read_to_end(&mut rest);
        (asked, inside, said)
    })
}

/// The pid the kernel names for the other end of `stream`.
fn peer_pid(stream: &UnixStream) -> u32 {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).unwrap();
    // SAFETY: `stream` owns the descriptor, and the kernel writes at most `length` bytes.
    let read = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    assert_eq!(read, 0, "the peer's credentials");
    u32::try_from(credentials.pid).unwrap()
}

/// Whether `pid` descends from `ancestor`, by the parents `/proc` names.
fn descends_from(pid: u32, ancestor: u32) -> bool {
    let mut current = pid;
    for _ in 0..128 {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{current}/stat")) else {
            return false;
        };
        let Some(parent) = stat
            .rsplit_once(") ")
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|parent| parent.parse::<u32>().ok())
        else {
            return false;
        };
        if parent == ancestor {
            return true;
        }
        if parent <= 1 {
            return false;
        }
        current = parent;
    }
    false
}
