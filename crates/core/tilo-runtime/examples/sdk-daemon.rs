//! A daemon of this computer's in a profile of its own, for a program's suite to call over its
//! native endpoint: what the SDKs' native suites run against (`tools/ci/sdk-native.sh`).
//!
//! ```sh
//! sdk-daemon <tilo-runtime> -- <command> [args...]
//! ```
//!
//! Sets Tilo up in a new private profile (the person's identity, kept in files there), starts
//! `<tilo-runtime>` serving that profile, waits until it answers on its native endpoint, and runs
//! `<command>` with `TILO_RUNTIME_PROFILE` naming the profile, so `daemon_socket()` finds the
//! daemon and the command's connections are admitted as the person. The daemon is stopped and the
//! profile removed when the command ends, whose exit status this answers.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::Value;

/// Names the profile this program, started again, sets Tilo up in.
const SET_UP: &str = "TILO_SDK_DAEMON_SET_UP";
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
    if std::env::var_os(SET_UP).is_some() {
        set_up_here()?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut args = std::env::args_os().skip(1);
    let runtime = args.next().context("usage: sdk-daemon <tilo-runtime> -- <command...>")?;
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--")) {
        bail!("usage: sdk-daemon <tilo-runtime> -- <command...>");
    }
    let command: Vec<_> = args.collect();
    let (program, rest) = command
        .split_first()
        .context("usage: sdk-daemon <tilo-runtime> -- <command...>")?;

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
    let status = with_profile(&mut Command::new(std::env::current_exe()?), profile)
        .env(SET_UP, profile)
        .status()?;
    ensure!(status.success(), "setting Tilo up in the profile failed: {status}");

    let log = std::fs::File::create(profile.join("daemon.log"))?;
    let mut daemon = with_profile(&mut Command::new(&runtime), profile)
        .env(tilo_paths::machine::DAEMON_PROFILE_VAR, profile)
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .env_remove("TILO_SVC_NATIVE_FD")
        .env_remove("TILO_RUNTIME_STANDALONE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .with_context(|| format!("starting {}", Path::new(&runtime).display()))?;
    let served = serving(profile);
    let code = match served {
        Ok(()) => {
            let status = with_profile(&mut Command::new(program), profile)
                .args(rest)
                .env(tilo_paths::machine::DAEMON_PROFILE_VAR, profile)
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

/// Set Tilo up as this process's environment names it: the person's identity, in files.
fn set_up_here() -> Result<()> {
    let registry = tilo_identity::store::IdentityRegistryStore::from_env()?;
    tilo_identity::provisioning::ensure_collaboration_identity(
        registry.identity_dir(),
        &tilo_identity::provisioning::collaboration_identity_paths(),
        tilo_identity::provisioning::IdentityStorage::Files,
    )?;
    Ok(())
}

/// Wait until the daemon in `profile` answers on its native endpoint. Its person's workspace is
/// founded the first time a call asks for it, which the suite's first connection does.
fn serving(profile: &Path) -> Result<()> {
    let deadline = Instant::now()
        .checked_add(START_CAP)
        .context("the start's deadline")?;
    loop {
        let last = status(profile);
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

/// The daemon's answer to a `status` request on its native endpoint.
fn status(profile: &Path) -> Result<Value> {
    let mut stream = tilo_socket::connect(&profile.join("native.sock"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.write_all(b"{\"native\":\"status\"}\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}
