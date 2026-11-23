//! A test's session as the device's daemon's shell, run in the test's own process: the daemon
//! starts over a profile and makes the session's link as it makes the link of a shell it starts
//! itself ([`tilo_runtime::daemon::ShellSession`] starts one as a process of its own), and the
//! session takes it in place of `TILO_SHELL_FD` ([`crate::Options::shell_link`]).

use std::path::Path;
use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_runtime::daemon::Running;

use crate::{Options, Session};

/// Start the daemon over `profile` and, once the person's services run (within `within`), a
/// headless session on `outputs` as their shell, on the link the daemon made for it. The profile's
/// person is founded already, as a device a person signed in on is. Stop the session before the
/// daemon.
///
/// # Errors
/// The daemon or the session did not start, or the person's services did not within `within`.
pub fn start_in_process(
    profile: &Path,
    mut options: Options,
    outputs: &[OutputConfig],
    within: Duration,
) -> Result<(Running, Session), crate::Error> {
    let running = Running::start(profile).map_err(crate::Error::host)?;
    options.shell_link = Some(running.link_shell(within).map_err(crate::Error::host)?);
    let session = Session::headless(options, outputs)?;
    Ok((running, session))
}
