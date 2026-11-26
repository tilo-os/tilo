//! A daemon of this computer's in a profile of its own, for a program's suite to call over its
//! native endpoint: what the SDKs' native suites run against (`tools/ci/sdk-native.sh`).
//!
//! ```sh
//! sdk-daemon <tilo-runtime> -- <command> [args...]
//! ```
//!
//! Starts `<tilo-runtime>` serving a new private profile, waits until it answers on its native
//! endpoint, sets Tilo up there through it (the person's identity, kept in files in the profile),
//! as `tilo login --new-identity` does, and runs
//! `<command>` with `TILO_RUNTIME_PROFILE` naming the profile, so `daemon_socket()` finds the
//! daemon and the command's connections are admitted as the person. The daemon is stopped and the
//! profile removed when the command ends, whose exit status this answers.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::Value;

/// Names the profile whose daemon's native socket this program, started again, prints.
const SOCKET_OF: &str = "TILO_SDK_DAEMON_SOCKET_OF";
/// How long the daemon has to serve its person.
const START_CAP: Duration = Duration::from_secs(120);

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("sdk-daemon: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    if let Some(profile) = std::env::var_os(SOCKET_OF) {
        let socket = tilo_api::daemon::profile_sockets(Path::new(&profile))
            .map_err(anyhow::Error::msg)?
            .join(tilo_machine::DAEMON_NATIVE);
        println!("{}", socket.display());
        return Ok(ExitCode::SUCCESS);
    }
    let mut args = std::env::args_os().skip(1);
    let runtime = args
        .next()
        .context("usage: sdk-daemon <tilo-runtime> -- <command...>")?;
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        bail!("usage: sdk-daemon <tilo-runtime> -- <command...>");
    }
    let command: Vec<_> = args.collect();
    let (program, rest) = command
        .split_first()
        .context("usage: sdk-daemon <tilo-runtime> -- <command...>")?;

    // The runtime directory this run names is where the daemon's and its clients' sockets are,
    // when it exists: made now, before either looks, so neither finds it missing and falls back
    // to another place while a program started later finds it made.
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|runtime| runtime.is_absolute())
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&runtime)
            .with_context(|| format!("making the runtime directory {}", runtime.display()))?;
    }
    let base = std::env::var_os("TMPDIR").map_or_else(|| "/tmp".into(), std::path::PathBuf::from);
    let root = tempfile::Builder::new()
        .prefix("tilo-sdk-")
        .tempdir_in(std::fs::canonicalize(base)?)
        .context("making the profile")?;
    let profile = root.path();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(profile, std::fs::Permissions::from_mode(0o700))?;
    }
    // The daemon's native socket, named with the profile's own homes: a profile's sockets are
    // beside this account's own, named by its path, and finding them reads the login's
    // directory, which is the profile's here and never this account's.
    let named = with_profile(&mut Command::new(std::env::current_exe()?), profile)
        .env(SOCKET_OF, profile)
        .stderr(Stdio::inherit())
        .output()?;
    ensure!(
        named.status.success(),
        "naming the profile's native socket failed: {}",
        named.status
    );
    let socket = std::path::PathBuf::from(
        String::from_utf8(named.stdout)
            .context("the profile's native socket")?
            .trim_end(),
    );

    let log = std::fs::File::create(profile.join("daemon.log"))?;
    let mut daemon = with_profile(&mut Command::new(&runtime), profile)
        .env(tilo_machine::DAEMON_PROFILE_VAR, profile)
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .env_remove("TILO_SVC_NATIVE_FD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .with_context(|| format!("starting {}", Path::new(&runtime).display()))?;
    let served = serving(&socket).and_then(|()| set_up(&socket, profile));
    let code = match served {
        Ok(()) => {
            let status = with_profile(&mut Command::new(program), profile)
                .args(rest)
                .env(tilo_machine::DAEMON_PROFILE_VAR, profile)
                .env_remove("TILO_SVC_NATIVE_FD")
                .status()
                .with_context(|| format!("running {}", Path::new(program).display()))?;
            status.code().map_or(ExitCode::FAILURE, |code| {
                ExitCode::from(u8::try_from(code).unwrap_or(1))
            })
        }
        Err(error) => {
            eprintln!("sdk-daemon: {error:#}");
            ExitCode::FAILURE
        }
    };
    if code != ExitCode::SUCCESS
        && let Ok(log) = std::fs::read_to_string(profile.join("daemon.log"))
    {
        eprintln!("daemon.log:\n{log}");
    }
    let _ = daemon.kill();
    let _ = daemon.wait();
    // The profile's sockets are outside the profile, so they go with it here.
    if let Some(sockets) = socket.parent() {
        let _ = std::fs::remove_dir_all(sockets);
    }
    Ok(code)
}

/// `command` with the profile's own homes: its identity and the files Tilo keeps.
fn with_profile<'a>(command: &'a mut Command, profile: &Path) -> &'a mut Command {
    command
        .env("XDG_CONFIG_HOME", profile.join("config"))
        .env("XDG_DATA_HOME", profile.join("data"))
        .env("XDG_CACHE_HOME", profile.join("cache"))
        .env("XDG_STATE_HOME", profile.join("state"))
        .env("TILO_IDENTITY_ROOT", profile.join("identity"))
        .env(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
}

/// Set Tilo up in `profile` through its daemon, at `socket`, as `tilo login --new-identity`
/// does: the daemon takes the device's key and is the one process that signs as the device, so
/// its founding call makes the person and certifies the device. A daemon that serves a person
/// already says so, which is no failure here.
fn set_up(socket: &Path, profile: &Path) -> Result<()> {
    let sockets = socket.parent().context("the native socket's directory")?;
    tilo_api::daemon::DaemonPlace::at(sockets, profile)
        .found(&tilo_api::daemon::Founding::default())
        .map(drop)
        .map_err(|why| anyhow::anyhow!("setting Tilo up in the profile: {why}"))
}

/// Wait until the daemon answers on its native endpoint, `socket`. Its person's workspace is
/// founded the first time a call asks for it, which the suite's first connection does.
fn serving(socket: &Path) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(START_CAP)
        .context("the start's deadline")?;
    loop {
        let last = status(socket);
        if last.is_ok() {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "the daemon did not answer within {START_CAP:?}: {last:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The daemon's answer to a `status` request on its native endpoint, `socket`.
fn status(socket: &Path) -> Result<Value> {
    let mut stream = tilo_socket::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(b"{\"native\":\"status\"}\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}
