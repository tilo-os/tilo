//! What the session settles about this device's identity before it opens a display: whether the
//! identity the machine already holds verifies, whether its owner must unlock or restore it first,
//! and whether the clock reads far enough for the certificate the device carries.
//! Nothing here replaces an identity. A record whose chain does not verify opens trusted recovery,
//! where only the person can unlock the store holding the key or bring the identity back from
//! another of their devices; a certificate the clock reads as not yet valid opens the wait for the
//! clock ([`BeforeShell::WaitForClock`]), because the identity itself is intact and waiting is all
//! that is asked of it. Anything else that fails (a store that cannot be read, a permission this
//! account does not have) stops the session instead of guessing, so a machine whose person cannot
//! be established never reaches a desktop under an identity that is not theirs.
//! A verified identity is also kept alive: this device's expiring certificates are renewed in the
//! background for as long as the session runs.
//! The headless identity commands run here as well: they settle the same identity in a login that starts
//! no display, which is how an installer and a test provision and verify one.

use std::path::{Path, PathBuf};

use tilo_device_identity::IdentityPaths;
use tilo_onboarding_session::firstboot;
use tilo_onboarding_session::identity::IdentityProblem;
use tilo_onboarding_session::identity_provisioning::{
    IDENTITY_RECOVERY_ENV, is_secret_service_unavailable, needs_first_run_flow,
};
use tilo_onboarding_session::view::OnboardingView;
use tilo_user_identity::CertificateClockError;

use crate::clock_wait::ClockWait;
use crate::setup::BeforeShell;

/// How many times startup asks for the identity's key before giving up on the Secret Service: a
/// login starts the keyring daemon and this session at the same moment, so the first asks come
/// before it answers.
const SECRET_SERVICE_ATTEMPTS: usize = 8;

/// How long to wait before asking again.
const SECRET_SERVICE_RETRY: std::time::Duration = std::time::Duration::from_millis(250);

/// What this device's identity admits before a display opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// The shell opens: the identity the device holds verifies, or it holds none yet and setup asks
    /// its person for one before the shell is used.
    Shell,
    /// The record is intact but the certificate it carries is outside its validity at the clock's
    /// reading. The shell waits for the clock: what opens is the wait [`BeforeShell::WaitForClock`],
    /// and the shell takes its place once the clock admits the certificate.
    WaitForClock {
        /// The directory the identity this device holds lives in, which the wait verifies again.
        identity_dir: PathBuf,
        /// What the clock read and what the certificate needs instead.
        refusal: CertificateClockError,
    },
    /// The record is here but its chain does not verify. Its owner unlocks what holds its key or
    /// restores the identity; trusted recovery opens in place of the shell.
    Recovery(IdentityProblem),
    /// Nothing proved the stored identity invalid, so the session stops rather than start a shell
    /// under an identity nobody could check.
    Refused(String),
}

/// What a verification that did not pass calls for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The identity is intact and its certificate is outside its validity at the clock's reading:
    /// the shell waits for the clock, and offers neither recovery nor a replacement.
    WaitForClock(CertificateClockError),
    /// The record is here but does not verify: its owner unlocks or restores it. Never a reason to
    /// make a second identity.
    Recovery(IdentityProblem),
    /// The error proves nothing about the identity (a store or permission failure, a keyring that
    /// never came up): stop.
    Refused,
}

/// The headless identity command a `tilo-session` command line asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// `--provision-identity` : establish the machine's identity, then stop.
    Provision,
    /// `--verify-collaboration-identity` : prove the identity this device holds verifies, then
    /// stop.
    Verify,
}

/// What the identity flags among `args` (the program's arguments, without the
/// program) ask for, and `None` for a command line that starts a session.
///
/// # Errors
/// Conflicting identity commands.
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

/// Carry out `command` , which opens no display: what it prints and the code the process stops with
/// are its whole answer.
pub fn run(command: Command) -> std::process::ExitCode {
    use std::process::ExitCode;
    let identity_path = match tilo_identity::store::active_identity_path() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("identity verification failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(identity_dir) = identity_path.parent() else {
        eprintln!("identity verification failed: primary identity has no parent directory");
        return ExitCode::FAILURE;
    };
    let paths = firstboot::collaboration_identity_paths();
    match command {
        Command::Verify => verify_only(identity_dir, &paths),
        Command::Provision => provision(&identity_path, identity_dir, &paths),
    }
}

/// Prove the identity this device holds verifies, as a release check runs it on an installed
/// machine.
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

/// Establish the machine's identity, or prove the one it holds verifies. An identity already here
/// is never replaced: this repairs a missing or damaged chain, it does not mint a second person
/// onto a used machine.
fn provision(
    identity_path: &Path,
    identity_dir: &Path,
    paths: &IdentityPaths,
) -> std::process::ExitCode {
    use std::process::ExitCode;
    if needs_first_run_flow(identity_path) {
        // This computer's daemon makes the person, as every person is made, and keeps their root.
        return match tilo_onboarding_session::identity::found_by_daemon(identity_dir) {
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

/// What this device's identity admits now, settling the setup a verified
/// identity leaves behind.
///
/// Trusted recovery and a certificate the clock refuses both leave the
/// process running: what opens in place of the shell is the session's own,
/// once this returns.
#[must_use]
pub fn identity() -> Identity {
    // A device enrolled without the person's root (a node's session, certified when its node was
    // installed) takes up the identity it holds before the store below is asked what is here, so
    // this login reads the person's record rather than the absence setup would take as a first use.
    if let Err(error) = tilo_onboarding_session::identity_provisioning::complete_enrolled_device() {
        tracing::warn!(%error, "the enrolled device could not take up its identity");
    }
    // An image the person wrote as one of their devices carries a one-use ticket: its first boot
    // redeems it here, before setup would ask for an identity, with nothing typed.
    if tilo_identity::store::active_identity_path().is_err()
        && let Ok(identity_dir) = tilo_identity::store::account_identity_dir_from_env()
    {
        match tilo_onboarding_session::nearby::redeem_staged_ticket(
            &identity_dir,
            &firstboot::collaboration_identity_paths(),
        ) {
            Ok(true) => tracing::info!("this computer joined the person's devices from its image"),
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, "the ticket this image carried was not redeemed"),
        }
    }
    let identity_path = match tilo_identity::store::active_identity_path() {
        Ok(path) => path,
        Err(error) => return Identity::Refused(format!("no identity store: {error}")),
    };
    if needs_first_run_flow(&identity_path) {
        // An account signed in on this computer with no identity of its own makes one here, the
        // one way a person is made: the daemon founds them and keeps their root, and this
        // account takes up their public record. It then certifies the key the daemon made it,
        // as a person whose identity another device of theirs holds does.
        if let Some(identity_dir) = identity_path.parent() {
            found_here(identity_dir);
        }
        return Identity::Shell;
    }
    let Some(identity_dir) = identity_path.parent() else {
        return Identity::Refused("primary identity has no parent directory".to_owned());
    };
    match verify_now(identity_dir) {
        Ok(()) => match settle(identity_dir) {
            Ok(()) => {
                certify_this_computers_key(&identity_path);
                Identity::Shell
            }
            Err(said) => Identity::Refused(said),
        },
        Err(error) => match failure(&error) {
            Failure::Recovery(problem) => {
                tracing::warn!(
                    %error,
                    problem = problem.as_str(),
                    "existing Tilo identity needs recovery; opening trusted recovery"
                );
                open_recovery(Some(problem));
                Identity::Recovery(problem)
            }
            Failure::WaitForClock(refusal) => {
                tracing::warn!(
                    %error,
                    valid_from_ns = refusal.valid_from_ns,
                    clock_ns = refusal.clock_ns,
                    "this device's certificate is outside its validity at the clock's \
                     reading; the shell waits for the clock"
                );
                Identity::WaitForClock {
                    identity_dir: identity_dir.to_path_buf(),
                    refusal,
                }
            }
            Failure::Refused => Identity::Refused(error.to_string()),
        },
    }
}

/// What this session shows before its shell is used, given what `identity` settled: the wait for the
/// clock, the setup this device still needs, or nothing and the shell opens.
#[must_use]
pub fn before_shell(identity: &Identity, host_desktop: bool) -> Option<BeforeShell> {
    match identity {
        Identity::WaitForClock {
            identity_dir,
            refusal,
        } => Some(BeforeShell::WaitForClock(ClockWait::new(
            identity_dir.clone(),
            host_desktop,
            refusal.clone(),
        ))),
        Identity::Shell | Identity::Recovery(_) | Identity::Refused(_) => {
            setup_due_now(host_desktop).map(|view| BeforeShell::Ceremony(Box::new(view)))
        }
    }
}

/// The setup this device still needs once its identity verifies: the first use when it has no
/// identity yet, setup it started and did not finish, the recovery of its identity when startup
/// found it unusable; `None` when there is nothing left to set up.
#[must_use]
pub(crate) fn setup_due_now(host_desktop: bool) -> Option<OnboardingView> {
    use crate::firstboot::setup_due;
    let identity_path = match tilo_identity::store::active_identity_path() {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, "no identity record to set up");
            return None;
        }
    };
    let recovery = std::env::var_os(IDENTITY_RECOVERY_ENV).is_some();
    let identity_dir = identity_path.parent()?.to_path_buf();
    if !setup_due(
        needs_first_run_flow(&identity_path),
        recovery,
        &identity_dir,
    ) {
        return None;
    }
    Some(match (host_desktop, recovery) {
        (false, false) => OnboardingView::new(identity_dir),
        (false, true) => OnboardingView::new_for_recovery(identity_dir),
        (true, false) => OnboardingView::new_for_host_desktop(identity_dir),
        (true, true) => OnboardingView::new_for_host_desktop_recovery(identity_dir),
    })
}

/// How startup handles a verification that did not pass.
/// The clock's reading is asked first because a refusal for it alone is reported as
/// [`std::io::ErrorKind::InvalidData`] too, and it is the one failure that leaves the identity
/// intact. A chain that does not verify is the person's to unlock or restore. Everything else — a
/// store or permission failure, a keyring that never came up — proves nothing about the stored
/// identity and is not readable as permission to replace it, so the session stops and the login is
/// tried again rather than open a shell nobody could establish.
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

/// Verify the identity this device already holds, asking again while the Secret Service comes up.
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

/// Certify the key this computer's daemon made for this account when another person than the
/// device's signed in on it, with the identity at `identity_path`: the account acts as that key
/// from then on. An identity this account holds the root of signs the certificate itself; one it
/// holds the public record of makes the daemon sign it with the root kept here for the account.
/// The device's own person has no key to certify; a daemon that is not running, or refuses,
/// leaves the account served the device's own actions, and is logged.
fn certify_this_computers_key(identity_path: &Path) {
    let signer = match tilo_identity::store::load_from_path(identity_path.to_path_buf()) {
        Ok(signer) => signer,
        Err(error) => {
            tracing::warn!(%error, "this account's identity cannot certify its key on this computer");
            return;
        }
    };
    let certified = if signer.is_single_device() {
        tilo_api::daemon::who::certify_my_key(&signer)
    } else {
        // The root stays with the daemon: this account holds only its public record.
        tilo_api::daemon::who::certify_here()
    };
    match certified {
        Ok(true) => tracing::info!("this account acts on this computer as its own certified key"),
        Ok(false) => {}
        Err(error) => {
            tracing::warn!(%error, "this account's key on this computer is not certified")
        }
    }
}

/// Make this account's person here when it signed in with none: the daemon founds them, the one
/// way a person is made, and this account takes up their public record; then the key the daemon
/// made it is certified with the root the daemon keeps for the account. An account that signed
/// in as a person whose identity another device of theirs holds makes none.
fn found_here(identity_dir: &Path) {
    match found_and_certify(identity_dir) {
        Ok(true) => {
            tracing::info!("this account made its Tilo identity on this computer");
        }
        Ok(false) => {}
        Err(error) => {
            tracing::warn!(%error, "this account's identity was not made on this computer")
        }
    }
}

/// [`found_here`], answering whether an identity was made and its key certified, and why not.
pub(crate) fn found_and_certify(identity_dir: &Path) -> Result<bool, String> {
    if !tilo_api::daemon::who::founding()? {
        return Ok(false);
    }
    tilo_api::daemon::this_computer()?.found(None)?;
    let who = tilo_api::daemon::who::who()?;
    let person = who
        .certify_person
        .or(who.person)
        .ok_or("this computer's daemon made this account no person")?;
    let taken = tilo_identity::provisioning::take_up_certified_person(
        identity_dir,
        &firstboot::collaboration_identity_paths(),
        person,
        tilo_clock::unix_now_ns(),
        &tilo_identity::provisioning::login_account_display_name(),
    )
    .map_err(|error| error.to_string())?;
    if !taken {
        return Err("this computer's daemon certified no person for this account".to_owned());
    }
    // The account a person's own daemon runs as holds the root it just made, and certifies its
    // key itself; another account's session holds the public record, and the daemon signs with
    // the root it keeps for that account.
    let held = tilo_identity::store::resolve_active_identity(identity_dir)
        .and_then(|path| tilo_identity::store::load_from_path(path).ok())
        .filter(|identity| identity.is_single_device());
    match held {
        Some(signer) => {
            tilo_api::daemon::who::certify_my_key(&signer)?;
        }
        None => {
            tilo_api::daemon::who::certify_here()?;
        }
    }
    Ok(true)
}

/// Verify the identity `identity_dir` holds at the clock's reading now.
///
/// # Errors
/// What verification answered: a clock refusal, a chain that does not
/// verify, or a store this account cannot read.
pub(crate) fn verify_now(identity_dir: &Path) -> std::io::Result<()> {
    verify(identity_dir, &firstboot::collaboration_identity_paths())
}

/// What a verified identity leaves behind: the residue of an interrupted
/// setup cleared, and this device's expiring certificates renewed on a thread
/// of its own for as long as the session runs, so nobody has to remember to
/// do it.
///
/// # Errors
/// The person's own setup cannot be told apart from an interrupted one, which
/// is theirs to keep.
pub(crate) fn settle(identity_dir: &Path) -> Result<(), String> {
    reconcile(identity_dir)?;
    firstboot::keep_device_certificates_renewed(identity_dir);
    // A certificate that ran out while this device slept is renewed here,
    // at the start of the session, before anything presents it: a join, an
    // enrollment, a host asked to share a workspace. A renewal that fails
    // is tried again by the loop, and by the join that needs it.
    if let Err(error) = firstboot::renew_due_device_certificates() {
        tracing::warn!(
            target: tilo_startup_metrics::JOIN_FLOW_TARGET,
            %error,
            "renewal: this session starts with a certificate due and could not renew it yet"
        );
    }
    Ok(())
}

/// A setup journal left behind when the identity was written is residue once the identity verifies,
/// not a reason to send an established person through recovery again. `Err` says the person's own
/// setup cannot be told apart from an interrupted one, which is theirs to keep: the session stops
/// rather than open a shell that hides it.
fn reconcile(identity_dir: &Path) -> Result<(), String> {
    match tilo_onboarding_session::view::clear_verified_pre_acquisition_progress(identity_dir) {
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

/// Point setup at trusted recovery for `problem` , which this session's view reads from its own
/// environment.
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
