//! Where keepers live, and what whoever starts one writes down about it.
//!
//! Each keeper has a private directory under [`root`]: its sockets, the record of how its app
//! ended, and the launch it keeps ([`KeptLaunch`], also the keeper's `--launch`). That directory
//! is the registry of kept apps: the session adopts every keeper it finds there, whoever started
//! it. The session starts keepers for the apps it launches; a service that outlives the session
//! (the Linux VM's window forwarder) starts its own, marked [`KeptLaunch::service`].

use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The version of [`KeptLaunch`]: a keeper started by a session that wrote another is not taken
/// back (its launch cannot be read), and is left running.
pub const LAUNCH_VERSION: u32 = 1;

/// What a session knows of a kept launch, written where the keeper keeps it (its `--launch`, and
/// `launch` in its directory): everything a later run needs to treat the running app as the same
/// launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptLaunch {
    pub version: u32,
    /// The app's desktop-file id, as tabs and notices name it.
    pub app: String,
    /// The name the app is shown by.
    pub title: String,
    /// The workspace it was launched into; empty for a service's.
    pub workspace: String,
    /// The launch as it was asked for (what a notice's Reopen asks again), when one was.
    pub asked: Option<String>,
    /// Every string the app's windows may announce themselves by.
    pub window_identities: Vec<String>,
    /// Started by a service that outlives the session, which ends it and restarts it: the session
    /// shows its windows, and never ends it, signals it or reports its end.
    #[serde(default)]
    pub service: bool,
}

impl KeptLaunch {
    /// As the keeper is handed it.
    #[must_use]
    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The launch `text` names, when it was written at this version.
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        serde_json::from_str::<Self>(text)
            .ok()
            .filter(|launch| launch.version == LAUNCH_VERSION)
    }
}

/// Where the keepers of the person's session keep their directories.
#[must_use]
pub fn root() -> PathBuf {
    tilo_paths::xdg::runtime_dir_or_default()
        .join("tilo-session")
        .join("kept")
}

/// The keeper program installed here: beside this executable first, as a build tree has it, then
/// where the image installs it.
#[must_use]
pub fn find_keeper() -> Option<PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("tilo-app-keeper")));
    beside
        .into_iter()
        .chain([PathBuf::from("/usr/libexec/tilo-app-keeper")])
        .find(|path| path.is_file())
}

/// A new keeper directory under `root`, private to this user, with `launch` written in it.
///
/// # Errors
/// When the directory cannot be made or written.
pub fn new_dir(root: &Path, launch: &KeptLaunch) -> io::Result<PathBuf> {
    let dir = make_dir(root)?;
    write_launch(&dir, launch)?;
    Ok(dir)
}

/// A new keeper directory under `root`, private to this user, with no launch in it yet: one
/// whose display must be known before its launch is (a sandbox binds the keeper's socket).
///
/// # Errors
/// When the directory cannot be made.
pub fn make_dir(root: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(root)?;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    let dir = root.join(uuid::Uuid::new_v4().simple().to_string());
    std::fs::create_dir(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

/// Write `launch` into the keeper directory `dir`.
///
/// # Errors
/// When the file cannot be written.
pub fn write_launch(dir: &Path, launch: &KeptLaunch) -> io::Result<()> {
    std::fs::write(dir.join("launch"), launch.encode())
}
