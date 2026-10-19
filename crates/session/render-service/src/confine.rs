//! The service's own sandbox, entered once its device is open and its
//! pipelines are built.
//!
//! What it still needs after that: its render node (read-write: every
//! submission is an ioctl on it), the font directories (read), the driver's
//! libraries for anything it loads late (read), and the descriptors it holds
//! or is handed. So:
//!
//! - **Landlock** grants exactly those paths and denies every TCP bind and
//!   connect; as a hard requirement, so a kernel that cannot enforce all of
//!   it stops the service rather than running it with less.
//! - **seccomp** (`no_new_privs`) adds the application baseline and denies
//!   creating sockets, connecting and executing anything
//!   (`sandbox_core::seccomp::parser_program`): channels arrive over the
//!   broker socket it already holds.
//!
//! Every descriptor the service did not start with on purpose is closed
//! first, so nothing the compositor held leaks into it.

use std::path::PathBuf;

use sandbox_core::landlock::{self, LandlockOutcome, NetRestriction, RulesetSpec};
use sandbox_core::seccomp;

/// Read-only system paths a GPU driver may still load from after start.
const DRIVER_PATHS: &[&str] = &[
    "/usr/lib64",
    "/usr/lib",
    "/usr/share/vulkan",
    "/usr/share/drirc.d",
    "/etc/drirc",
];

/// Close every descriptor above `keep`.
pub fn close_inherited(keep: i32) {
    let first = u32::try_from(keep).map_or(3, |keep| keep.saturating_add(1));
    // SAFETY: close_range only closes descriptors; nothing in this process
    // holds one above `keep` yet (this runs first thing in the service).
    let _ = unsafe { libc::syscall(libc::SYS_close_range, first, u32::MAX, 0_u32) };
}

/// Why the sandbox could not be entered.
#[derive(Debug, thiserror::Error)]
pub enum ConfineError {
    #[error("landlock: {0}")]
    Landlock(String),
    #[error("landlock is not fully enforced: {0}")]
    Partial(&'static str),
    #[error("seccomp: {0}")]
    Seccomp(String),
}

/// Enter the sandbox. Irreversible.
///
/// # Errors
///
/// When either layer cannot be installed in full.
pub fn confine(render_node: Option<&PathBuf>, font_roots: &[PathBuf]) -> Result<(), ConfineError> {
    let read: Vec<PathBuf> = font_roots
        .iter()
        .cloned()
        .chain(DRIVER_PATHS.iter().map(PathBuf::from))
        .collect();
    let write: Vec<PathBuf> = render_node.into_iter().cloned().collect();
    let rules = landlock::access_plan(&read, &write, landlock::FS_ABI);
    let spec = RulesetSpec::application(rules, NetRestriction::DenyAll);
    let status = landlock::restrict_self(&spec, |_, _| {})
        .map_err(|error| ConfineError::Landlock(error.to_string()))?;
    let outcome = landlock::outcome_from_status(&status.ruleset);
    if outcome != LandlockOutcome::FullyEnforced {
        return Err(ConfineError::Partial(outcome.describe()));
    }
    let clone3 = seccomp::clone3_compat_program()
        .map_err(|error| ConfineError::Seccomp(error.to_string()))?;
    seccomp::install(&clone3).map_err(|error| ConfineError::Seccomp(error.to_string()))?;
    let program =
        seccomp::parser_program().map_err(|error| ConfineError::Seccomp(error.to_string()))?;
    seccomp::install(&program).map_err(|error| ConfineError::Seccomp(error.to_string()))?;
    Ok(())
}
