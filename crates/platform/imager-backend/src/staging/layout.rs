//! Where a staged plan's machine finds what the live installer staged there.
//!
//! These are paths of the machine being installed, not of the host that stages them, so they
//! live beside the staging protocol rather than in the installer app's own layout.

/// Where a staged plan's machine finds the single-use ticket it logs in with in the live
/// installer's own filesystem, owner-only: the path the installer's `tilo login` reads.
pub const STAGED_TOKEN_PATH: &str = "/etc/tilo/ticket";
/// Where a staged plan's machine finds the prebuilt layers a development install applies before
/// its first boot, owner-only.
pub const STAGED_DEVELOPMENT_LAYERS_DIR: &str = "/etc/tilo/development-layers";
