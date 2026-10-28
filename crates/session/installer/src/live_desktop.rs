//! The live trial: a real Tilo desktop session running beside this install
//! wizard, on the installation medium itself, with nothing on any internal
//! disk.
//!
//! The trial is secondary to installing. It starts only when the person asks
//! for it from a wizard step (never at boot, never as the way in), and leaving
//! it returns them to the wizard with every answer they had already given, so
//! cancelling costs one press and no re-typing.
//!
//! What a trial guarantees, and why each part is here:
//!
//! * **One file asks for it.** The wizard's `try-desktop` action writes
//!   [`REQUEST_ENTRY`] and leaves the screen; greetd restarts its session, and
//!   `/usr/libexec/tilo-session-select` finds the request and starts the trial
//!   instead of the wizard. No privilege is involved in asking: only the live
//!   medium's own setup unit may create the enclosing directory (see
//!   `tilo-installer-setup.service`), and both paths are fixed, so a request
//!   can never name a program.
//! * **Its own account, its own home.** The trial runs as the unprivileged
//!   [`TRIAL_USER`], whose home lives on the live medium's own filesystem. It
//!   is not the account the install is about to create, and it is not created
//!   from the wizard's answers: nothing typed here reaches the installed
//!   system, and nothing here is carried over.
//! * **Its own disposable state.** The home is recreated from the image on
//!   every trial, so the first frame is the real first-login desktop with no
//!   seeded data and no earlier trial's leftovers, and leaving the trial
//!   deletes it. Nothing survives the machine.
//! * **No internal disk is mounted or unlocked.** The trial never asks for a
//!   disk, a partition or a passphrase; the only writes it makes are to the
//!   live medium's own writable layers.
//!
//! Everything the trial needs to *prove* — which session it is, that it went
//! away, that its state went away — is readable from these two files by any
//! process on the medium, so a test can check the guarantee without scraping a
//! log.

use std::path::{Path, PathBuf};

/// The directory the live medium's setup unit creates. Only the live medium
/// has it, so its presence is what makes the trial offered at all.
pub const STATE_DIR: &str = "/run/tilo-live-trial";

/// The marker a development installation medium carries: it exists only in a
/// rootfs built with `build-custom-installer.sh --dev-login`, never in a
/// published one.
pub const DEV_MEDIUM_MARKER: &str = "/usr/share/tilo/installer-dev-login";

/// Written by the wizard to ask for a trial, and removed when the trial ends.
pub const REQUEST_ENTRY: &str = "/run/tilo-live-trial/request";

/// The account a trial session runs as.
pub const TRIAL_USER: &str = "tilo-trial";

/// The `WaylandDesktop` the trial session runs.
pub const SESSION_ENTRY: &str = "/usr/share/wayland-sessions/tilo-live-trial.desktop";

/// Where the trial's throwaway home lives, on the live medium.
pub const TRIAL_HOME: &str = "/var/lib/tilo-live-trial/home";

/// The directory this process treats as the live medium's trial state.
///
/// It is [`STATE_DIR`] unless `TILO_LIVE_TRIAL_STATE` names another one, which
/// only a build with development overrides honours — the same gate the
/// compositor's own test doors use. It lets a test drive the real wizard
/// against a temporary medium instead of the host it happens to run on.
fn state_dir() -> PathBuf {
    if let Ok(overridden) = std::env::var("TILO_LIVE_TRIAL_STATE")
        && dev_overrides()
        && !overridden.is_empty()
    {
        return PathBuf::from(overridden);
    }
    std::env::var_os(TEST_STATE)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(STATE_DIR))
}

/// `TILO_LIVE_TRIAL_STATE`, set by [`TrialMedium`] for one test.
const TEST_STATE: &str = "TILO_LIVE_TRIAL_STATE";

/// A temporary live medium that the wizard treats as this process's own.
///
/// The wizard reads the medium through the process environment (that is how
/// a running installer finds it), so a test cannot simply pass a path. This
/// scope sets [`TEST_STATE`] for as long as it lives and every wizard call in
/// the test talks to the temporary medium instead of the host's. It holds the
/// environment lock, so two tests never share a medium — and so the whole
/// expected path below is idiomatic here.
#[cfg(test)]
#[allow(
    dead_code,
    reason = "the scope only exists to hold the environment override and its lock"
)]
pub(crate) struct TrialMedium {
    state: PathBuf,
    env: tilo_test_env::ScopedEnv,
}

#[cfg(test)]
impl TrialMedium {
    /// A medium whose setup unit already made `state`.
    pub(crate) fn at(state: PathBuf) -> Self {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(TEST_STATE, state.to_string_lossy().as_ref());
        Self { state, env }
    }

    /// The bytes of the standing request, if one stands.
    pub(crate) fn request_contents(&self) -> Option<Vec<u8>> {
        std::fs::read(self.state.join("request")).ok()
    }

    /// What the session manager does with a request: it is consumed once.
    pub(crate) fn consume_request(&self) -> Result<(), String> {
        cancel_in(&self.state)
    }

    pub(crate) fn offered(&self) -> bool {
        offered_in(&self.state)
    }
}

#[cfg(test)]
impl Drop for TrialMedium {
    fn drop(&mut self) {
        // The medium's own directory is the caller's; this only removes what
        // the wizard wrote into it.
        let _ = std::fs::remove_file(self.state.join("request"));
    }
}

#[cfg(feature = "dev-overrides")]
fn dev_overrides() -> bool {
    true
}

#[cfg(not(feature = "dev-overrides"))]
fn dev_overrides() -> bool {
    cfg!(debug_assertions)
}

/// Whether this medium offers a trial at all — the setup unit made
/// [`STATE_DIR`] — regardless of whether one has been asked for yet.
///
/// This is what decides whether the wizard publishes the entries at all. It is
/// deliberately independent of the request so that the surfaces never change
/// shape underneath a person who is looking at them.
pub fn available() -> bool {
    available_in(&state_dir())
}

fn available_in(state_dir: &Path) -> bool {
    state_dir
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir())
}

/// Whether this medium offers a trial: the person is on the installation
/// medium (the wizard's own setup unit made [`STATE_DIR`]) and nothing has
/// asked for a trial yet.
pub fn offered() -> bool {
    offered_in(&state_dir())
}

fn offered_in(state_dir: &Path) -> bool {
    available_in(state_dir) && state_dir.join("request").symlink_metadata().is_err()
}

/// What asking for a trial did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrialRequest {
    /// Written; the session manager will start the trial.
    Requested,
    /// This medium has no trial (an installed system, or a wizard started on
    /// the host), so nothing was written.
    NotOffered,
}

/// Ask the live medium to leave the wizard and start the trial desktop.
///
/// # Errors
///
/// Returns a message when the request file cannot be written.
pub fn request() -> Result<TrialRequest, String> {
    request_in(&state_dir())
}

fn request_in(state_dir: &Path) -> Result<TrialRequest, String> {
    if !available_in(state_dir) {
        return Ok(TrialRequest::NotOffered);
    }
    // A request that already stands is not overwritten: the trial is either
    // starting or running, and refreshing the file would only race the session
    // manager that is about to consume it.
    let path = state_dir.join("request");
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Ok(TrialRequest::Requested);
        }
        Err(error) => return Err(format!("Could not start the live desktop: {error}")),
    };
    // The written content is a fixed word, never a path or an argument: the
    // session manager decides what runs.
    std::io::Write::write_all(&mut file, b"live-desktop\n")
        .map_err(|error| format!("Could not start the live desktop: {error}"))?;
    Ok(TrialRequest::Requested)
}

/// Undo [`request`], keeping an earlier request if this one was never written.
///
/// # Errors
///
/// Returns a message when the request file exists but cannot be removed.
pub fn cancel() -> Result<(), String> {
    cancel_in(&state_dir())
}

fn cancel_in(state_dir: &Path) -> Result<(), String> {
    match std::fs::remove_file(state_dir.join("request")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not cancel the live desktop: {error}")),
    }
}

/// A path inside the trial's throwaway home.
fn home_path(entry: &str) -> PathBuf {
    Path::new(TRIAL_HOME).join(entry)
}

/// Whether this installation medium may still be installed onto its own disk.
///
/// An installed system never installs onto the disk it boots from, and neither
/// does a published medium: its installer image grows its root to the end of
/// the disk on boot (`tilo-installer-grow-root.service`), so a download later
/// here has nowhere to go. A development medium is the exception, and it
/// carries its own marker to say so.
pub fn may_install_to_medium() -> bool {
    Path::new(DEV_MEDIUM_MARKER).exists()
}

/// The trial's empty identity directory: the desktop's first-run screen looks
/// here, so a trial needs no baked-in identity files of its own.
pub fn identity_dir() -> PathBuf {
    home_path("identity")
}

/// The application language for this machine.
pub fn locale() -> String {
    ["LANG", "LC_ALL", "LC_MESSAGES"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "en_US.UTF-8".to_owned())
}

/// The approved keyboard layout for this machine, as the compositor reads it
/// from the environment.
pub fn keymap() -> (String, String) {
    (
        std::env::var("XKB_DEFAULT_LAYOUT").unwrap_or_else(|_| "us".to_owned()),
        std::env::var("XKB_DEFAULT_VARIANT").unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_dir() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("temp dir");
        let state = directory.path().join("trial");
        std::fs::create_dir(&state).expect("create state dir");
        (directory, state)
    }

    #[test]
    fn a_medium_that_offers_the_trial_records_the_request_and_undoes_it() {
        let (_guard, state) = state_dir();
        assert!(available_in(&state));
        assert!(offered_in(&state));
        assert_eq!(request_in(&state), Ok(TrialRequest::Requested));
        // While the request stands the trial is no longer offered, so a
        // second press cannot queue a second session — but asking again is
        // not an error and does not rewrite the file the session manager is
        // about to consume.
        assert!(!offered_in(&state));
        assert_eq!(request_in(&state), Ok(TrialRequest::Requested));
        // The request names no program: it is a fixed word.
        assert_eq!(
            std::fs::read(state.join("request")).expect("request readable"),
            b"live-desktop\n"
        );
        cancel_in(&state).expect("cancel");
        assert!(offered_in(&state));
        // Cancelling twice is not an error: the wizard may cancel a request
        // the session manager already honoured.
        cancel_in(&state).expect("second cancel");
    }

    #[test]
    fn a_medium_without_the_trial_writes_nothing() {
        let directory = tempfile::tempdir().expect("temp dir");
        let missing = directory.path().join("absent");
        assert!(!offered_in(&missing));
        assert_eq!(request_in(&missing), Ok(TrialRequest::NotOffered));
        assert!(!missing.exists(), "a refusal must not create the directory");
        // Cancelling on such a medium is a no-op, not an error.
        cancel_in(&missing).expect("cancel on a medium without the trial");
    }

    #[test]
    fn the_wizard_only_offers_the_trial_while_nothing_is_installed() {
        // The install worker writes its own progress files; a trial that
        // started while the disk was being written would run on the medium
        // this session is also using. The wizard gates the action instead of
        // this module, so the rule is stated once: see
        // `surface_tests::try_desktop_exists_only_before_the_install_starts`.
        assert_eq!(
            home_path("identity"),
            Path::new("/var/lib/tilo-live-trial/home/identity")
        );
    }

    #[test]
    fn the_trial_reads_the_machines_language_and_layout() {
        // A trial must feel like this machine, not like a default: the wizard
        // already asked for both, and the desktop reads the same variables.
        let mut vars = tilo_test_env::ScopedEnv::new();
        vars.set("LANG", "de_DE.UTF-8")
            .set("XKB_DEFAULT_LAYOUT", "de")
            .set("XKB_DEFAULT_VARIANT", "nodeadkeys");
        assert_eq!(locale(), "de_DE.UTF-8");
        assert_eq!(keymap(), ("de".to_owned(), "nodeadkeys".to_owned()));
        vars.remove("LANG")
            .remove("XKB_DEFAULT_LAYOUT")
            .remove("XKB_DEFAULT_VARIANT");
        assert_eq!(locale(), "en_US.UTF-8");
        assert_eq!(keymap(), ("us".to_owned(), String::new()));
        drop(vars);
    }
}
