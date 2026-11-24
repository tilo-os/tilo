//! Where a desktop's own workspace service kept its state, before the device's daemon kept its
//! person's workspaces: what the session hands over once, and what root reads at sign-in to tell
//! the daemon that the session is about to.

use std::path::{Path, PathBuf};

/// The service's directory, below the state directory (`$XDG_STATE_HOME`).
pub const DIR: &str = "tilo/workspace-service";
/// The directory of the workspaces it kept, below its directory.
const WORKSPACES: &str = "workspaces";
/// The mark that the desktop's session handed what its service kept over, below its directory.
pub const MOVED_OUT: &str = "moved-out";

/// The desktop service's directory below the state directory `state_home`.
#[must_use]
pub fn dir(state_home: &Path) -> PathBuf {
    state_home.join(DIR)
}

/// Whether the desktop service whose directory is `dir` kept workspaces it has not handed over.
#[must_use]
pub fn kept(dir: &Path) -> bool {
    dir.join(WORKSPACES).is_dir() && !dir.join(MOVED_OUT).exists()
}
