//! Windows transport and launch edge.
//!
//! A profile's services are reached over named pipes, an app inherits open
//! handles, and a launch is confined by an AppContainer. None of that is here
//! yet: this is the first part of the daemon's Windows port, so every function
//! a Unix host fills in refuses in words. The daemon's own entry points refuse
//! before reaching them ([`crate::daemon`]).
use std::{
    fs::File,
    io,
    path::Path,
    process::{Child, Command},
};

const NOT_YET: &str = "the Tilo daemon does not run on Windows yet";

/// The refusal every part of the Windows host makes until it has a body.
fn not_yet() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, NOT_YET)
}

/// Create `path` as a directory only this person can enter.
pub(crate) fn private_directory(_path: &Path) -> io::Result<()> {
    Err(not_yet())
}

/// Refuse a profile an earlier Tilo left reachable by others, and create it as
/// its owner's private directory.
pub(crate) fn check_private_profile(_path: &Path) -> io::Result<()> {
    Err(not_yet())
}

/// Open or create a private file, refusing one another user can reach.
#[expect(
    dead_code,
    reason = "the daemon's Windows profile is not opened until its services have bodies"
)]
pub(crate) fn private_file(_path: &Path) -> io::Result<File> {
    Err(not_yet())
}

/// The host's service endpoints, as one app connects to them.
pub struct ServiceSockets<'a> {
    pub collab: &'a Path,
    pub identity: &'a Path,
    pub blobs: &'a Path,
}

/// One app's connections to the host's services, handshaken with its identity
/// before the app holds any of them.
#[expect(
    dead_code,
    reason = "an app gains its Windows connections once handle passing exists"
)]
pub(crate) struct ServiceConnections;

/// Own the connected endpoints until spawn finishes.
#[expect(
    dead_code,
    reason = "the daemon prepares an app's Windows connections once handle passing exists"
)]
pub(crate) struct Connections {
    _services: ServiceConnections,
}

impl Connections {
    #[expect(
        dead_code,
        reason = "the daemon prepares an app's Windows connections once handle passing exists"
    )]
    pub(crate) fn prepare(
        _command: &mut Command,
        _app_id: &str,
        _workspace: &str,
        _sockets: &ServiceSockets<'_>,
    ) -> io::Result<Self> {
        Err(not_yet())
    }
}

/// Spawn `command` as `app_id` with its service connections inherited.
///
/// # Errors
/// Always [`io::ErrorKind::Unsupported`]: an app launch inherits no Windows
/// handles yet.
pub fn launch(
    _command: Command,
    _app_id: &str,
    _workspace: &str,
    _sockets: &ServiceSockets<'_>,
) -> io::Result<Child> {
    Err(not_yet())
}
