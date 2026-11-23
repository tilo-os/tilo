//! The daemon and its client entry points on Windows.
//!
//! The Windows bodies do not exist yet: a profile's services are reached over
//! named pipes, an app inherits open handles, and a launch is confined by an
//! AppContainer. Until those are here, every entry point refuses in words
//! instead of starting half a daemon, and the command line is the same one
//! every host parses ([`crate::daemon_args`]).
use std::{ffi::OsString, io, path::Path};

pub use crate::daemon_args::{Mode, default_profile, parse_args, validate_profile};

const NOT_YET: &str = "the Tilo daemon does not run on Windows yet";

/// The refusal every daemon entry point makes until it has a Windows body.
fn not_yet() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, NOT_YET)
}

/// Start this computer's daemon over `profile` and serve it.
///
/// # Errors
/// Always [`io::ErrorKind::Unsupported`]: the daemon has no Windows body yet.
pub fn serve(_profile: &Path, _dev: bool) -> io::Result<()> {
    Err(not_yet())
}

/// Carry an MCP client to this computer's runtime, or to a device it reaches.
///
/// # Errors
/// Always [`io::ErrorKind::Unsupported`]: the daemon has no Windows body yet.
pub fn connect(
    _profile: &Path,
    _chose_own_profile: bool,
    _forward: Option<&tilo_mcp::devices::Forward>,
    _workspace: Option<&str>,
) -> io::Result<()> {
    Err(not_yet())
}

/// Attach connected service descriptors and replace this host with the app.
///
/// # Errors
/// Always [`io::ErrorKind::Unsupported`]: an app launch inherits no Windows
/// handles yet.
pub fn run_app(
    _profile: &Path,
    _app_id: &str,
    _workspace: Option<&str>,
    _executable: &OsString,
    _args: &[OsString],
) -> io::Result<()> {
    Err(not_yet())
}
