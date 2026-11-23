//! The runtime executable's command line and its private profile, shared by
//! every host that runs one: the modes an executable is started in, and the
//! profile each one holds.
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    /// This computer's daemon, which plain `tilo-runtime` runs: in the person's own environment
    /// over the default profile, or, over a profile named apart (`--profile`,
    /// `TILO_RUNTIME_PROFILE`), in an environment kept inside it: a VM host's, a test's.
    Daemon,
    Connect,
    RunApp {
        app_id: String,
        /// A workspace this profile joined; the profile's own workspace when `None`.
        workspace: Option<String>,
        executable: OsString,
        args: Vec<OsString>,
    },
}
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub profile: PathBuf,
    pub mode: Mode,
    /// `--dev`, for the daemon only: serve builds under development, which are not installed.
    pub dev: bool,
    /// The client chose a standalone profile itself (`--standalone`,
    /// `--profile` or `TILO_RUNTIME_PROFILE`), so `--connect` attaches to
    /// that profile even inside a Tilo login, where it otherwise relays to
    /// the desktop's MCP server ([`crate::session_api`]).
    pub standalone: bool,
    /// `--device NAME [--workspace ID]`, for `--connect` only: carry the
    /// client to that device instead of working here ([`crate::other_devices`]).
    pub forward: Option<tilo_mcp::devices::Forward>,
    /// `--workspace ID` without `--device`, for `--connect` only: the
    /// workspace the client asks this desktop's MCP server to let it work in,
    /// rather than the one in front of the person. The person decides; it is
    /// never authority.
    pub workspace: Option<String>,
}
pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// Validate the private profile before any startup side effects.
pub fn validate_profile(profile: &Path) -> io::Result<()> {
    if !profile.is_absolute() {
        return Err(invalid("runtime profile must be absolute"));
    }
    crate::host::check_private_profile(profile)
}

/// The same persistent profile is used by daemon, app attachment and MCP proxy.
pub fn default_profile() -> io::Result<PathBuf> {
    let path = match std::env::var_os("TILO_RUNTIME_PROFILE") {
        Some(value) => PathBuf::from(value),
        None => own_profile()?,
    };
    if !path.is_absolute() {
        return Err(invalid("runtime profile must be absolute"));
    }
    Ok(path)
}

/// This computer's own profile, whatever a process names: the system unit's, or its account's.
pub(crate) fn own_profile() -> io::Result<PathBuf> {
    // Tilo OS: the system unit's daemon is this computer's, for every account of it.
    #[cfg(target_os = "linux")]
    if tilo_api::daemon::system_daemon() {
        return Ok(PathBuf::from(tilo_api::daemon::SYSTEM_PROFILE));
    }
    let home =
        tilo_paths::home().ok_or_else(|| invalid("HOME is missing; set TILO_RUNTIME_PROFILE"))?;
    #[cfg(target_os = "macos")]
    let suffix = "Library/Application Support/Tilo/runtime";
    #[cfg(target_os = "windows")]
    let suffix = "AppData/Local/Tilo/runtime";
    #[cfg(all(unix, not(target_os = "macos")))]
    let suffix = ".local/state/tilo/runtime";
    Ok(home.join(suffix))
}

/// Parse native modes while preserving opaque application arguments.
pub fn parse_args(arguments: impl IntoIterator<Item = OsString>) -> io::Result<Args> {
    let mut arguments = arguments.into_iter();
    let mut profile = None;
    let mut mode = None;
    let mut dev = false;
    let mut standalone = false;
    let mut device = None;
    let mut workspace = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--dev") if !dev => dev = true,
            Some("--device") if device.is_none() => device = Some(device_named(arguments.next())?),
            Some("--workspace") if workspace.is_none() => {
                workspace = Some(workspace_named(arguments.next())?);
            }
            Some("--standalone") if !standalone => standalone = true,
            Some("--profile") if profile.is_none() => {
                profile = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or_else(|| invalid("--profile requires a path"))?,
                ));
            }
            Some("--connect") if mode.is_none() => mode = Some(Mode::Connect),
            Some("--run-app") if mode.is_none() => {
                mode = Some(parse_run_app(&mut arguments)?);
                break;
            }
            _ => {
                return Err(invalid(format!(
                    "unexpected argument: {}",
                    argument.to_string_lossy()
                )));
            }
        }
    }
    let explicit_profile = profile.is_some() || std::env::var_os("TILO_RUNTIME_PROFILE").is_some();
    let profile = match profile {
        Some(path) => path,
        None => default_profile()?,
    };
    if !profile.is_absolute() {
        return Err(invalid("runtime profile must be absolute"));
    }
    let mode = mode.unwrap_or(Mode::Daemon);
    if dev && mode != Mode::Daemon {
        return Err(invalid(
            "--dev starts the daemon: use it without --connect or --run-app",
        ));
    }
    if standalone && mode != Mode::Connect {
        return Err(invalid(
            "--standalone chooses where --connect goes: use it with --connect",
        ));
    }
    let local = device.is_none();
    let forward = forward_of(&mode, device, workspace.clone())?;
    let workspace = workspace.filter(|_| local);
    if workspace.is_some() && (standalone || explicit_profile) {
        return Err(invalid(
            "--workspace asks the desktop's MCP server for a workspace, not a standalone runtime",
        ));
    }
    Ok(Args {
        profile,
        mode,
        dev,
        standalone: standalone || explicit_profile,
        forward,
        workspace,
    })
}

/// Where `--connect --device NAME [--workspace ID]` goes: `None` without a
/// device, where `--workspace` asks this desktop's own server for the workspace.
fn forward_of(
    mode: &Mode,
    device: Option<String>,
    workspace: Option<String>,
) -> io::Result<Option<tilo_mcp::devices::Forward>> {
    if (device.is_some() || workspace.is_some()) && *mode != Mode::Connect {
        return Err(invalid(
            "--device and --workspace choose where --connect goes: use them with --connect",
        ));
    }
    Ok(device.map(|device| tilo_mcp::devices::Forward { device, workspace }))
}

/// The device `--device` names, by its name or its endpoint id.
fn device_named(argument: Option<OsString>) -> io::Result<String> {
    argument
        .and_then(|arg| arg.into_string().ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && name.len() <= 256)
        .ok_or_else(|| invalid("--device requires a device's name or endpoint id"))
}

/// The workspace `--workspace` names, by its id.
fn workspace_named(argument: Option<OsString>) -> io::Result<String> {
    argument
        .and_then(|arg| arg.into_string().ok())
        .filter(|workspace| uuid::Uuid::parse_str(workspace).is_ok())
        .ok_or_else(|| invalid("--workspace requires a workspace id"))
}

/// `--run-app APP_ID [--workspace WORKSPACE] -- EXECUTABLE ARGS...`, after `--run-app`:
/// the rest of the command line is the app's.
fn parse_run_app(arguments: &mut impl Iterator<Item = OsString>) -> io::Result<Mode> {
    let app_id = arguments
        .next()
        .and_then(|arg| arg.into_string().ok())
        .ok_or_else(|| invalid("--run-app requires an application ID"))?;
    if app_id.trim().is_empty() || app_id.len() > 4096 {
        return Err(invalid("invalid application ID"));
    }
    let mut next = arguments.next();
    let mut workspace = None;
    if next.as_deref().and_then(std::ffi::OsStr::to_str) == Some("--workspace") {
        workspace = Some(
            arguments
                .next()
                .and_then(|arg| arg.into_string().ok())
                .filter(|workspace| uuid::Uuid::parse_str(workspace).is_ok())
                .ok_or_else(|| invalid("--workspace requires a workspace id"))?,
        );
        next = arguments.next();
    }
    if next.as_deref() != Some(std::ffi::OsStr::new("--")) {
        return Err(invalid(
            "--run-app APP_ID [--workspace WORKSPACE] requires -- EXECUTABLE ARGS...",
        ));
    }
    let executable = arguments
        .next()
        .ok_or_else(|| invalid("missing app executable"))?;
    Ok(Mode::RunApp {
        app_id,
        workspace,
        executable,
        args: arguments.collect(),
    })
}
