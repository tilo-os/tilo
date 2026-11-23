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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// `--provision-identity` : establish the machine's identity, then stop.
    Provision,
    /// `--verify-collaboration-identity` : prove the identity this device holds verifies, then
    /// stop.
    Verify,
    /// `--pair-device-link-file FILE`: enroll this new device, then stop.
    PairDevice(PathBuf),
    /// Private stdin from a first-use session, tied to that session's lifetime.
    PairDeviceFromOnboarding {
        requester: tilo_enrollment::client::ProcessLease,
        status: PathBuf,
    },
}

/// What the identity flags among `args` (the program's arguments, without the
/// program) ask for, and `None` for a command line that starts a session.
///
/// # Errors
/// Conflicting identity commands, or a pairing flag with no link file.
pub fn command(args: &[String]) -> Result<Option<Command>, String> {
    let asked = |flag: &str| args.iter().any(|arg| arg == flag);
    if let [flag, pid, start_time, status] = args
        && flag == "--pair-device-link-stdin"
    {
        let pid = pid.parse().map_err(|_| "invalid pairing requester PID")?;
        let start_time = start_time
            .parse()
            .map_err(|_| "invalid pairing requester start time")?;
        return Ok(Some(Command::PairDeviceFromOnboarding {
            requester: tilo_enrollment::client::ProcessLease::from_parts(pid, start_time),
            status: PathBuf::from(status),
        }));
    }
    if asked("--pair-device-link-stdin") {
        return Err("the pairing helper needs a requester lease and a progress path".to_owned());
    }
    if let Some(pairing) = args.iter().position(|arg| arg == "--pair-device-link-file") {
        if asked("--provision-identity") || asked("--verify-collaboration-identity") {
            return Err("device pairing cannot run with another identity command".to_owned());
        }
        let file = args
            .get(pairing.saturating_add(1))
            .filter(|arg| !arg.starts_with("--"))
            .ok_or_else(|| "--pair-device-link-file needs a file".to_owned())?;
        return Ok(Some(Command::PairDevice(PathBuf::from(file))));
    }
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
/// are its whole answer. `host_desktop` is a session added to an operating system Tilo does not
/// control, whose identity setup differs from the image's.
pub fn run(command: Command, host_desktop: bool) -> std::process::ExitCode {
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
        Command::Provision => provision(&identity_path, identity_dir, &paths, host_desktop),
        Command::PairDevice(file) => match pair_device_from_file(identity_dir, &file) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("could not pair this device: {error:#}");
                ExitCode::FAILURE
            }
        },
        Command::PairDeviceFromOnboarding { requester, status } => {
            match pair_device_from_onboarding(identity_dir, requester, &status) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    if let Err(write_error) =
                        tilo_enrollment::client::PairingStatus::failed(&error).write(&status)
                    {
                        eprintln!("could not report pairing progress: {write_error:#}");
                    }
                    eprintln!("could not pair this device: {error:#}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

/// This first-use command runs as the new device's own account, before it
/// opens a session. The signed answer selects the identity through the same
/// durable installer the nearby path uses.
fn pair_device_from_file(identity_dir: &Path, file: &Path) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use tilo_identity::enrollment::{given_token, spend_token};

    let link = given_token(file)?.context("the file contains no device pairing link")?;
    let link = zeroize::Zeroizing::new(link);
    let joined = pair_device(identity_dir, &link, None, None)?;
    spend_token(file)?;
    println!("Paired as {}", joined.label);
    Ok(())
}

fn pair_device_from_onboarding(
    identity_dir: &Path,
    requester: tilo_enrollment::client::ProcessLease,
    status: &Path,
) -> anyhow::Result<()> {
    use std::io::Read as _;
    use tilo_enrollment::client::PairingStatus;

    requester.validate()?;
    let mut link = zeroize::Zeroizing::new(String::new());
    let limit = tilo_enrollment::local_link::MAX_LINK_BYTES;
    std::io::stdin()
        .take(u64::try_from(limit)?.saturating_add(1))
        .read_to_string(&mut link)?;
    if link.len() > limit {
        anyhow::bail!("the device pairing link is too long");
    }
    let joined = pair_device(identity_dir, &link, Some(requester), Some(status))?;
    PairingStatus::complete(
        joined.code,
        hex::encode(joined.identity),
        joined.record_path,
    )
    .write(status)
}

fn pair_device(
    identity_dir: &Path,
    link: &str,
    requester_lease: Option<tilo_enrollment::client::ProcessLease>,
    status: Option<&Path>,
) -> anyhow::Result<tilo_enrollment::request::JoinedDevice> {
    use tilo_enrollment::client::PairingStatus;
    use tilo_enrollment::request::{JoinOptions, PairingProgress};
    use tilo_identity::enrollment::{DECLARATION, Method, Methods};

    Methods::of_image(Path::new(DECLARATION))?.ensure(Method::Token)?;
    let root = tilo_identity::provisioning::collaboration_identity_root();
    let name = tilo_paths::session::host_name()
        .filter(|name| tilo_server_protocol::valid_device_label(name));
    let options = JoinOptions {
        identity_root: &root,
        label: name.as_deref().unwrap_or("Tilo device"),
        timeout: 60,
        kind: tilo_enrollment::EnrollmentKind::PersonalDevice,
        identity_dir: Some(identity_dir),
        requester_lease,
        port: 0,
        person_uid: None,
    };
    tilo_enrollment::local_link::redeem(link, options, |progress| {
        if let PairingProgress::Waiting { code, label } = progress {
            if let Some(status) = status {
                PairingStatus::waiting(code, label).write(status)?;
            } else {
                println!("Pairing code: {code}");
            }
        }
        Ok(())
    })
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
    host_desktop: bool,
) -> std::process::ExitCode {
    use std::process::ExitCode;
    if needs_first_run_flow(identity_path) {
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
    let identity_path = match tilo_identity::store::active_identity_path() {
        Ok(path) => path,
        Err(error) => return Identity::Refused(format!("no identity store: {error}")),
    };
    if needs_first_run_flow(&identity_path) {
        // No record at all, or one that does not even parse: setup asks the person for an identity
        // and validates what is there before it writes anything, so nothing here has to decide for
        // them.
        return Identity::Shell;
    }
    let Some(identity_dir) = identity_path.parent() else {
        return Identity::Refused("primary identity has no parent directory".to_owned());
    };
    match verify_now(identity_dir) {
        Ok(()) => match settle(identity_dir) {
            Ok(()) => Identity::Shell,
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
