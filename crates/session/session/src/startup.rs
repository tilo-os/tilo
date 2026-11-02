//! What the session settles about this device's identity before it opens a
//! display: whether the identity the machine already holds verifies, whether
//! its owner must unlock or restore it first, and whether the clock reads far
//! enough for the certificate the device carries.
//!
//! Nothing here replaces an identity. A record whose chain does not verify
//! opens trusted recovery, where only the person can unlock the store holding
//! the key or bring the identity back from another of their devices; a
//! certificate the clock reads as not yet valid keeps the desk off until the
//! clock passes, because the identity itself is intact and waiting is all that
//! is asked of it. Anything else that fails (a store that cannot be read, a
//! permission this account does not have) stops the session instead of
//! guessing, so a machine whose person cannot be established never reaches a
//! desktop under an identity that is not theirs.
//!
//! A verified identity is also kept alive: this device's expiring certificates
//! are renewed in the background for as long as the session runs.
//!
//! The two headless commands run here as well: they settle the same identity
//! in a login that starts no display, which is how an installer and a test
//! provision and verify one.

use std::path::Path;

use tilo_onboarding_host::firstboot;
use tilo_onboarding_host::identity::IdentityProblem;
use tilo_onboarding_host::identity_provisioning::{
    DiskIdentityStore, IDENTITY_RECOVERY_ENV, is_secret_service_unavailable, needs_first_run_flow,
};
use tilo_user_identity::{
    CertificateClockError, IdentityPaths, WallClockWait, wait_for_wall_clock,
};

/// How many times startup asks for the identity's key before giving up on the
/// Secret Service: a login starts the keyring daemon and this session at the
/// same moment, so the first asks come before it answers.
const SECRET_SERVICE_ATTEMPTS: usize = 8;

/// How long to wait before asking again.
const SECRET_SERVICE_RETRY: std::time::Duration = std::time::Duration::from_millis(250);

/// What this device's identity admits before a display opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// The desk opens: the identity the device holds verifies, or it holds
    /// none yet and setup asks its person for one before the desk is used.
    Desk,
    /// The record is here but its chain does not verify. Its owner unlocks
    /// what holds its key or restores the identity; trusted recovery opens in
    /// place of the desk.
    Recovery(IdentityProblem),
    /// Nothing proved the stored identity invalid, so the session stops
    /// rather than start a desk under an identity nobody could check.
    Refused(String),
}

/// What a verification that did not pass calls for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The identity is intact and its certificate is outside its validity at
    /// the clock's reading: the desk waits for the clock, and offers neither
    /// recovery nor a replacement.
    WaitForClock(CertificateClockError),
    /// The record is here but does not verify: its owner unlocks or restores
    /// it. Never a reason to make a second identity.
    Recovery(IdentityProblem),
    /// The error proves nothing about the identity (a store or permission
    /// failure, a keyring that never came up): stop.
    Refused,
}

/// The headless identity command a `tilo-session` command line asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// `--provision-identity`: establish the machine's identity, then stop.
    Provision,
    /// `--verify-collaboration-identity`: prove the identity this device
    /// holds verifies, then stop.
    Verify,
}

/// What the identity flags among `args` (the program's arguments, without the
/// program) ask for, and `None` for a command line that starts a session.
///
/// # Errors
/// Both flags together, which would say two things at once.
pub fn command(args: &[String]) -> Result<Option<Command>, String> {
    let asked = |flag: &str| args.iter().any(|arg| arg == flag);
    match (
        asked("--provision-identity"),
        asked("--verify-collaboration-identity"),
    ) {
        (true, true) => Err(
            "--provision-identity and --verify-collaboration-identity are mutually exclusive"
                .to_owned(),
        ),
        (true, false) => Ok(Some(Command::Provision)),
        (false, true) => Ok(Some(Command::Verify)),
        (false, false) => Ok(None),
    }
}

/// Carry out `command`, which opens no display: what it prints and the code
/// the process stops with are its whole answer. `host_desktop` is a session
/// added to an operating system Tilo does not control, whose identity setup
/// differs from the image's.
pub fn run(command: Command, host_desktop: bool) -> std::process::ExitCode {
    use std::process::ExitCode;
    let store = match DiskIdentityStore::from_env() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("identity verification failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(identity_dir) = store.path().parent() else {
        eprintln!("identity verification failed: primary identity has no parent directory");
        return ExitCode::FAILURE;
    };
    let paths = firstboot::collaboration_identity_paths();
    match command {
        Command::Verify => verify_only(identity_dir, &paths),
        Command::Provision => provision(&store, identity_dir, &paths, host_desktop),
    }
}

/// Prove the identity this device holds verifies, as a release gate runs it on
/// an installed machine.
fn verify_only(identity_dir: &Path, paths: &IdentityPaths) -> std::process::ExitCode {
    use std::process::ExitCode;
    match firstboot::verify_collaboration_identity(identity_dir, paths, tilo_clock::unix_now_ns()) {
        Ok(()) => {
            println!("Tilo collaboration identity verified");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("identity verification failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Establish the machine's identity, or prove the one it holds verifies. An
/// identity already here is never replaced: this repairs a missing or damaged
/// chain, it does not mint a second person onto a used machine.
fn provision(
    store: &DiskIdentityStore,
    identity_dir: &Path,
    paths: &IdentityPaths,
    host_desktop: bool,
) -> std::process::ExitCode {
    use std::process::ExitCode;
    if needs_first_run_flow(store) {
        let provisioned = if host_desktop {
            firstboot::ensure_collaboration_identity_for_legacy_linux_host(identity_dir, paths)
        } else {
            firstboot::ensure_collaboration_identity(identity_dir, paths)
        };
        return match provisioned {
            Ok(_) => {
                println!("Tilo collaboration identity is provisioned");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("could not provision collaboration identity: {error}");
                ExitCode::FAILURE
            }
        };
    }
    match verify(identity_dir, paths) {
        Ok(()) => {
            println!("Tilo collaboration identity is provisioned");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("could not provision collaboration identity: {error}");
            ExitCode::FAILURE
        }
    }
}

/// What this device's identity admits now, waiting for the clock while its
/// certificate is not valid yet and settling the setup a verified identity
/// leaves behind.
///
/// Trusted recovery and a certificate that waits for the clock both leave the
/// process running: what opens in place of the desk is the session's own, once
/// this returns.
#[must_use]
pub fn identity() -> Identity {
    let store = match DiskIdentityStore::from_env() {
        Ok(store) => store,
        Err(error) => return Identity::Refused(format!("no identity store: {error}")),
    };
    if needs_first_run_flow(&store) {
        // No record at all, or one that does not even parse: setup asks the
        // person for an identity and validates what is there before it writes
        // anything, so nothing here has to decide for them.
        return Identity::Desk;
    }
    let Some(identity_dir) = store.path().parent() else {
        return Identity::Refused("primary identity has no parent directory".to_owned());
    };
    let paths = firstboot::collaboration_identity_paths();
    loop {
        match verify(identity_dir, &paths) {
            Ok(()) => {
                if let Err(said) = reconcile(identity_dir) {
                    return Identity::Refused(said);
                }
                // Certificates that expire would take this device's identity
                // with them: renewed on a thread of its own for as long as the
                // session runs, so nobody has to remember to do it.
                firstboot::keep_device_certificates_renewed(identity_dir);
                return Identity::Desk;
            }
            Err(error) => match failure(&error) {
                Failure::Recovery(problem) => {
                    tracing::warn!(
                        %error,
                        problem = problem.as_str(),
                        "existing Tilo identity needs recovery; opening trusted recovery"
                    );
                    open_recovery(Some(problem));
                    return Identity::Recovery(problem);
                }
                Failure::WaitForClock(refusal) => {
                    tracing::warn!(
                        %error,
                        valid_from_ns = refusal.valid_from_ns,
                        clock_ns = refusal.clock_ns,
                        "this device's certificate is outside its validity at the clock's \
                         reading; the desk waits for the clock"
                    );
                    if let Some(said) = wait_for_clock(&refusal) {
                        return Identity::Refused(said);
                    }
                }
                Failure::Refused => return Identity::Refused(error.to_string()),
            },
        }
    }
}

/// How startup handles a verification that did not pass.
///
/// The clock's reading is asked first because a refusal for it alone is
/// reported as [`std::io::ErrorKind::InvalidData`] too, and it is the one
/// failure that leaves the identity intact. A chain that does not verify is
/// the person's to unlock or restore. Everything else — a store or permission
/// failure, a keyring that never came up — proves nothing about the stored
/// identity and is not readable as permission to replace it, so the session
/// stops and the login is tried again rather than open a desk nobody could
/// establish.
#[must_use]
pub fn failure(error: &std::io::Error) -> Failure {
    if let Some(refusal) = CertificateClockError::in_io(error) {
        return Failure::WaitForClock(refusal.clone());
    }
    if error.kind() == std::io::ErrorKind::InvalidData {
        return Failure::Recovery(
            IdentityProblem::of(error).unwrap_or(IdentityProblem::ChainInvalid),
        );
    }
    Failure::Refused
}

/// Verify the identity this device already holds, asking again while the
/// Secret Service comes up.
fn verify(identity_dir: &Path, paths: &IdentityPaths) -> std::io::Result<()> {
    let mut attempt = 1;
    loop {
        match firstboot::verify_existing_collaboration_identity(identity_dir, paths) {
            Err(error)
                if is_secret_service_unavailable(&error) && attempt < SECRET_SERVICE_ATTEMPTS =>
            {
                tracing::info!(
                    attempt,
                    max_attempts = SECRET_SERVICE_ATTEMPTS,
                    "Secret Service is not ready; retrying existing identity verification"
                );
                std::thread::sleep(SECRET_SERVICE_RETRY);
                attempt = attempt.saturating_add(1);
            }
            result => return result.map(|_| ()),
        }
    }
}

/// Wait until the clock can admit this device's certificate: the instant it
/// becomes valid, or any step of the clock, which is what an expired
/// certificate needs for a clock moved back to admit it again. `Some` says the
/// wait itself failed, which no retry can tell apart from a clock that never
/// comes.
fn wait_for_clock(refusal: &CertificateClockError) -> Option<String> {
    // An expired certificate has no instant a forward-running clock reaches:
    // the timer is armed as far out as it goes, so only a step of the clock
    // ends that wait.
    let instant = refusal.valid_at_ns().unwrap_or(u64::MAX);
    match wait_for_wall_clock(instant) {
        Ok(WallClockWait::Reached | WallClockWait::Changed) => None,
        Err(error) => Some(format!("could not wait for the clock: {error}")),
    }
}

/// A setup journal left behind when the identity was written is residue once
/// the identity verifies, not a reason to send an established person through
/// recovery again. `Err` says the person's own setup cannot be told apart from
/// an interrupted one, which is theirs to keep: the session stops rather than
/// open a desk that hides it.
fn reconcile(identity_dir: &Path) -> Result<(), String> {
    match tilo_onboarding_host::view::clear_verified_pre_acquisition_progress(identity_dir) {
        Ok(true) => {
            tracing::info!(
                "cleared interrupted pre-acquisition onboarding journal after identity verification"
            );
            Ok(())
        }
        Ok(false) => Ok(()),
        Err(error) => {
            tracing::error!(%error, "could not reconcile verified identity onboarding progress");
            Err(format!(
                "could not reconcile verified identity onboarding progress: {error}"
            ))
        }
    }
}

/// Point setup at trusted recovery for `problem`, which this session's view
/// reads from its own environment.
#[allow(
    clippy::disallowed_methods,
    reason = "the marker is how the session's own setup learns which recovery to open, and it is set before any thread that reads the environment is started"
)]
fn open_recovery(problem: Option<IdentityProblem>) {
    // SAFETY: called before the session starts any thread of its own; the
    // walk to this point reads the identity on this thread only, and the
    // markers only select a stricter setup surface and the step it offers
    // first.
    unsafe {
        std::env::set_var(IDENTITY_RECOVERY_ENV, "1");
        if let Some(problem) = problem {
            std::env::set_var(IdentityProblem::RECOVERY_ENV, problem.as_str());
        }
    }
}
