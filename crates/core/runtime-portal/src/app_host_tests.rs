#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures"
)]

use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;

/// An app host listening at `<dir>/host.sock`, serving each connection on a thread of its own as
/// `tilo-launcher host` serves the one systemd hands it.
fn host(dir: &Path) -> std::path::PathBuf {
    let socket = dir.join("host.sock");
    let listener = listening(&socket);
    std::thread::spawn(move || {
        while let Ok(link) = rustix::net::accept_with(&listener, rustix::net::SocketFlags::CLOEXEC)
        {
            std::thread::spawn(move || {
                let _ = serve(&link);
            });
        }
    });
    socket
}

/// A socket listening at `socket` as the app host's does: one packet a message.
fn listening(socket: &Path) -> OwnedFd {
    let listener = rustix::net::socket_with(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::SEQPACKET,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    rustix::net::bind(
        &listener,
        &rustix::net::SocketAddrUnix::new(socket).unwrap(),
    )
    .unwrap();
    rustix::net::listen(&listener, 4).unwrap();
    listener
}

/// A launcher stand-in: it waits at its launch hold, says what it was handed, then runs `then`.
fn launcher(dir: &Path, then: &str) -> std::path::PathBuf {
    let path = dir.join("tilo-launcher");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             head -c 1 >/dev/null\n\
             printf '%s\\n' \"$@\" > '{out}/args.tmp' && mv '{out}/args.tmp' '{out}/args'\n\
             echo \"runtime $XDG_RUNTIME_DIR\" >&2\n\
             {then}\n",
            out = dir.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    path
}

fn wait_for(path: &Path) -> String {
    let until = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
    loop {
        if let Ok(read) = std::fs::read_to_string(path) {
            return read;
        }
        assert!(Instant::now() < until, "{} was written", path.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn hearing() -> (Arc<Mutex<String>>, impl FnMut(&[u8]) + Send + 'static) {
    let said = Arc::new(Mutex::new(String::new()));
    let into = Arc::clone(&said);
    (said, move |bytes: &[u8]| {
        into.lock()
            .unwrap()
            .push_str(&String::from_utf8_lossy(bytes));
    })
}

/// The host starts the launcher held, tells its pid pinned by a pidfd, lets it go only once
/// released, hands it its grants in a runtime directory of its own and no path to any service,
/// carries back what it says, and ends it when the daemon closes its side.
#[test]
fn a_hosted_launch_is_held_released_heard_and_stopped() {
    let temp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(temp.path()).unwrap();
    let socket = host(&dir);
    let program = launcher(&dir, "echo said-by-the-app >&2\nexec sleep 600");
    let mut order = Order::new(
        &program,
        ["run", "--app-id", "so.tilo.bridge", "--", "tail"],
    );
    order.grants = Some(b"{\"granted\":true}".to_vec());
    order.env = vec![(b"TILO_NO_WINDOW".to_vec(), Some(b"1".to_vec()))];
    let (heard, said) = hearing();
    let hosted = super::order(&socket, order, said).expect("the host starts it");
    assert!(runs(&hosted.process()), "its pidfd pins a running process");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !dir.join("args").exists(),
        "it waits at its launch hold until the daemon recorded it"
    );
    hosted.release(Handed::default(), None).unwrap();
    let args: Vec<String> = wait_for(&dir.join("args"))
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(args.first().map(String::as_str), Some("run"));
    assert_eq!(
        args.get(1).map(String::as_str),
        Some("--capabilities"),
        "the grants come right after the subcommand: {args:?}"
    );
    let grants = args.get(2).unwrap();
    assert_eq!(std::fs::read(grants).unwrap(), b"{\"granted\":true}");
    assert_eq!(
        args.get(3..).unwrap(),
        ["--app-id", "so.tilo.bridge", "--", "tail"],
        "the rest of its command is the daemon's, in order"
    );
    let until = Instant::now() + Duration::from_secs(30);
    let runtime = loop {
        let heard = heard.lock().unwrap().clone();
        if heard.contains("said-by-the-app")
            && let Some(runtime) = heard.lines().find_map(|line| line.strip_prefix("runtime "))
        {
            break runtime.to_owned();
        }
        assert!(Instant::now() < until, "what it said came back: {heard:?}");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(
        Path::new(&runtime).is_dir(),
        "its runtime directory is the host's"
    );
    assert!(
        !Path::new(&runtime).join("tilo").exists(),
        "nothing there leads to a service of the daemon's"
    );
    assert!(hosted.try_wait().is_none(), "it runs until it is stopped");
    hosted.stop();
    let status = hosted.wait().expect("the host says how it ended");
    assert_eq!(
        std::process::ExitStatus::from_raw(status).signal(),
        Some(libc::SIGTERM),
        "the daemon's stop ended it"
    );
    assert!(!runs(&hosted.process()), "its process is gone");
    assert!(
        !Path::new(&runtime).exists(),
        "the host's runtime directory goes with it"
    );
}

/// A launch that ends by itself is said to have ended, with its status and its last words; one the
/// daemon never records never goes on.
#[test]
fn a_hosted_launch_says_how_it_ended() {
    let temp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(temp.path()).unwrap();
    let socket = host(&dir);
    let program = launcher(&dir, "echo no-runtime-here >&2\nexit 3");
    let (heard, said) = hearing();
    let hosted = super::order(&socket, Order::new(&program, ["run"]), said).unwrap();
    hosted.release(Handed::default(), None).unwrap();
    let status = hosted.wait().expect("its status");
    assert_eq!(std::process::ExitStatus::from_raw(status).code(), Some(3));
    assert!(heard.lock().unwrap().contains("no-runtime-here"));

    // Dropped unreleased: the daemon recorded nothing, so nothing of it runs.
    let _ = std::fs::remove_file(dir.join("args"));
    let (_, said) = hearing();
    let hosted = super::order(&socket, Order::new(&program, ["run"]), said).unwrap();
    let process = hosted.process();
    hosted.stop();
    let _ = hosted.wait();
    assert!(!runs(&process));
    assert!(
        !dir.join("args").exists(),
        "an unreleased launch never went on"
    );

    // A program the host cannot start is refused with its reason.
    let (_, said) = hearing();
    let refused =
        super::order(&socket, Order::new(&dir.join("missing"), ["run"]), said).unwrap_err();
    assert!(refused.to_string().contains("could not start"), "{refused}");
}

/// Set only in the launcher stand-in [`launcher_child`] runs as: where it says what it was handed.
const LAUNCHER_ENV: &str = "TILO_APP_HOST_TEST_LAUNCHER";

/// A launcher stand-in, run in a copy of this test binary: it waits at its launch hold as the
/// launcher does, then reads the first line of each connection its release handed, in order.
#[test]
fn launcher_child() {
    use std::io::BufRead as _;
    let Some(out) = std::env::var_os(LAUNCHER_ENV) else {
        return;
    };
    crate::launch_hold::wait_on_stdin().expect("released");
    let mut heard = Vec::new();
    while let Some(link) = crate::launch_hold::take_window_link() {
        let mut line = String::new();
        std::io::BufReader::new(std::os::unix::net::UnixStream::from(link))
            .read_line(&mut line)
            .unwrap();
        heard.push(line.trim().to_owned());
    }
    let tmp = Path::new(&out).with_extension("tmp");
    std::fs::write(&tmp, heard.join(" ")).unwrap();
    std::fs::rename(tmp, out).unwrap();
}

/// The release hands the launch the connections the daemon made for it, and the connection its
/// launch names, in the order the launcher takes them; one the daemon made none of is skipped.
#[test]
fn a_hosted_launch_is_handed_the_daemons_connections_in_order() {
    use std::io::Write as _;
    let temp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(temp.path()).unwrap();
    let socket = host(&dir);
    let out = dir.join("heard");
    let mut order = Order::new(
        &std::env::current_exe().unwrap(),
        ["--exact", "app_host::tests::launcher_child", "--nocapture"],
    );
    order.env = vec![(
        LAUNCHER_ENV.as_bytes().to_vec(),
        Some(out.as_os_str().as_bytes().to_vec()),
    )];
    let (_, said) = hearing();
    let hosted = super::order(&socket, order, said).unwrap();
    // Each says which it is, and is read after this end closed.
    let made = |name: &str| {
        let (mut mine, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        writeln!(mine, "{name}").unwrap();
        Descriptor::from(theirs)
    };
    let handed = Handed {
        native: Some(made("native")),
        collab: None,
        identity: Some(made("identity")),
        blobs: Some(made("blobs")),
    };
    let link = made("link");
    hosted
        .release(handed, Some(std::os::fd::AsFd::as_fd(&link)))
        .unwrap();
    assert_eq!(wait_for(&out), "native identity blobs link");
    let status = hosted.wait().expect("it ends by itself");
    assert_eq!(std::process::ExitStatus::from_raw(status).code(), Some(0));
}

/// Set only in the host this module's tests start as an account of its own.
const HOST_ENV: &str = "TILO_APP_HOST_TEST_HOST";

/// The host side, run in a copy of this test binary as systemd runs `tilo-launcher host`: the
/// order on its standard input. Without [`HOST_ENV`] it is not the host and has nothing to do.
#[test]
fn host_child() {
    if std::env::var_os(HOST_ENV).is_none() {
        return;
    }
    let link = link_on_stdin().expect("the connection on its standard input");
    serve(&link).expect("the host serves its order");
}

/// The host runs as the account systemd gives it (`User=tilo-app`), never the daemon's: what it
/// starts runs as that account, and the daemon's state, its account's alone, is out of its reach.
/// The daemon still stops it.
#[test]
fn a_hosted_launch_runs_as_the_hosts_account_and_reaches_nothing_of_the_daemons() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::CommandExt as _;
    use tilo_test_env::SpawnUnderEnvLock as _;
    const APP_ACCOUNT: u32 = 65534;
    tilo_test_env::require_capability!(
        Chown,
        tilo_test_env::switch_identity(APP_ACCOUNT),
        "the host runs as an account of its own, which only root starts it as"
    );
    let daemon = rustix::process::geteuid().as_raw();
    let temp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(temp.path()).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The daemon's state: its account's alone, as /var/lib/tilo/state is `tilo`'s.
    let state = dir.join("state");
    std::fs::create_dir(&state).unwrap();
    std::fs::write(state.join("keypair.bin"), b"the device key").unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    // The daemon's service sockets, in a directory only its account enters.
    let services = dir.join("services");
    std::fs::create_dir(&services).unwrap();
    let _collab = std::os::unix::net::UnixListener::bind(services.join("collab.sock")).unwrap();
    std::fs::set_permissions(&services, std::fs::Permissions::from_mode(0o700)).unwrap();
    let said = dir.join("said");
    std::fs::create_dir(&said).unwrap();
    std::fs::set_permissions(&said, std::fs::Permissions::from_mode(0o777)).unwrap();
    let program = dir.join("tilo-launcher");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\n\
             head -c 1 >/dev/null\n\
             {{ id -u\n\
             if cat '{state}/keypair.bin' >/dev/null 2>&1; then echo reads-state; \
             else echo no-state; fi\n\
             if [ -e '{services}/collab.sock' ]; then echo sees-sockets; \
             else echo no-route; fi\n\
             }} > '{said}/who.tmp' && mv '{said}/who.tmp' '{said}/who'\n\
             exec sleep 600\n",
            state = state.display(),
            services = services.display(),
            said = said.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The app host's socket, served as systemd serves it: each connection a host of its own, run
    // as the app account with the connection as its standard input.
    let socket = dir.join("host.sock");
    let listener = listening(&socket);
    // The runtime directory systemd makes the host (`RuntimeDirectory=`), its account's own.
    let runtime = dir.join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    std::os::unix::fs::chown(&runtime, Some(APP_ACCOUNT), Some(APP_ACCOUNT)).unwrap();
    // A copy of this test binary the app account can run, wherever the build left it.
    let exe = dir.join("host");
    std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let hosts = Arc::new(Mutex::new(Vec::new()));
    let started = Arc::clone(&hosts);
    std::thread::spawn(move || {
        while let Ok(link) = rustix::net::accept_with(&listener, rustix::net::SocketFlags::CLOEXEC)
        {
            let host = std::process::Command::new(&exe)
                .args(["--exact", "app_host::tests::host_child", "--nocapture"])
                .env(HOST_ENV, "1")
                .env("RUNTIME_DIRECTORY", &runtime)
                .stdin(std::process::Stdio::from(link))
                .uid(APP_ACCOUNT)
                .gid(APP_ACCOUNT)
                .spawn_locked()
                .expect("the host starts as the app account");
            started.lock().unwrap().push(host);
        }
    });
    let (_, heard) = hearing();
    let hosted = super::order(&socket, Order::new(&program, ["run"]), heard).unwrap();
    hosted.release(Handed::default(), None).unwrap();
    let who = wait_for(&said.join("who"));
    let mut lines = who.lines();
    let uid: u32 = lines.next().unwrap().parse().unwrap();
    assert_eq!(uid, APP_ACCOUNT, "it runs as the host's account");
    assert_ne!(uid, 0, "never as root");
    assert_ne!(uid, daemon, "never as the daemon's account");
    assert_eq!(
        lines.next(),
        Some("no-state"),
        "the daemon's state is out of its reach"
    );
    assert_eq!(
        lines.next(),
        Some("no-route"),
        "it finds no path to the daemon's sockets: the daemon hands it what it connects"
    );
    hosted.stop();
    let status = hosted.wait().expect("the host says how it ended");
    assert_eq!(
        std::process::ExitStatus::from_raw(status).signal(),
        Some(libc::SIGTERM)
    );
    for mut host in hosts.lock().unwrap().drain(..) {
        assert!(host.wait().unwrap().success(), "the host ends with its app");
    }
}
