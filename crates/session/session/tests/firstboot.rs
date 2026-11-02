//! The machine's first boot: which binary the unit starts, what that binary
//! does with the flag it is given, and what it settles about the identity the
//! machine already holds before it opens a desk.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::path::PathBuf;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_onboarding_host::identity::IdentityProblem;
use tilo_onboarding_host::identity_provisioning::IDENTITY_RECOVERY_ENV;
use tilo_session::mount::firstboot::{OneShot, one_shot};
use tilo_session::startup::{self, Identity};
use tilo_user_identity::{IdentityPaths, persist};

/// The unit the image ships for the machine's own first-boot setup.
const UNIT: &str = include_str!("../../../../os/usr/lib/systemd/system/shell-firstboot.service");

/// The image's components, which say where each binary is installed.
const COMPONENTS: &str = include_str!("../../../../os/components.toml");

fn exec_line() -> &'static str {
    UNIT.lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .expect("the first-boot unit starts something")
}

#[test]
fn the_machines_first_boot_sets_up_and_stops_in_the_new_session() {
    let mut words = exec_line().split_whitespace();
    let program = words.next().expect("a program to run");
    let args: Vec<String> = words.map(str::to_owned).collect();
    assert_eq!(
        program, "/usr/bin/tilo-session",
        "first boot no longer starts the shell it replaces"
    );
    assert!(
        COMPONENTS.contains(&format!("to = \"{program}\"")),
        "the image installs the binary the unit starts"
    );
    assert_eq!(
        one_shot(&args),
        Ok(Some(OneShot::SetupOnly)),
        "setup, then the process exits: this unit is not a session"
    );
    // The live installer's boots are disposable and must not consume the
    // one-shot marker of the machine it installs onto.
    assert!(
        UNIT.lines()
            .any(|line| line == "ConditionKernelCommandLine=!tilo.install"),
        "the live medium still skips the installed machine's first boot"
    );
}

/// A machine with an identity of its own: the person's trees, this device's
/// enrolled key and certificate, and nothing else on it.
struct Machine {
    // The environment the identity lives in, restored before the directories
    // it names are removed.
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
    system: PathBuf,
    runtime: PathBuf,
    paths: IdentityPaths,
}

impl Machine {
    /// The record the session resolves for this account: what it reads before
    /// it decides whether the desk opens.
    fn record(&self) -> PathBuf {
        tilo_onboarding_host::identity_provisioning::DiskIdentityStore::from_env()
            .expect("the identity store")
            .path()
            .to_path_buf()
    }

    /// This device's certificate as it is on disk.
    fn certificate(&self) -> Vec<u8> {
        std::fs::read(&self.paths.device_cert).expect("this device's certificate")
    }
}

/// A machine whose person enrolled this device: an identity provisioned the
/// way the installer and the host package provision one.
fn machine() -> Machine {
    let root = tempfile::tempdir().expect("a directory for this machine");
    let config = root.path().join("config");
    let system = root.path().join("system");
    let runtime = root.path().join("runtime");
    std::fs::create_dir_all(&runtime).expect("the login's runtime directory");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", root.path())
        .set("XDG_CONFIG_HOME", &config)
        .set("XDG_RUNTIME_DIR", &runtime)
        .set("TILO_IDENTITY_ROOT", &system)
        .set(FILE_SECRETS_ENV, "1");
    let personal = config.join("tilo/identity");
    let paths = IdentityPaths::under(&system);
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this machine's identity");
    Machine {
        home: root.path().to_path_buf(),
        config,
        system,
        runtime,
        paths,
        _env: env,
        _root: root,
    }
}

#[test]
fn a_verified_identity_opens_the_desk() {
    let _machine = machine();
    assert_eq!(startup::identity(), Identity::Desk);
}

#[test]
fn a_certificate_the_clock_reads_as_not_yet_valid_keeps_the_desk_off_until_it_passes() {
    let machine = machine();
    // Dated far enough ahead that writing it cannot make it valid already.
    let issued_at_ns = tilo_clock::unix_now_ns() + 400_000_000;
    let mut cert =
        persist::read_device_cert(&machine.paths.device_cert).expect("this device's certificate");
    cert.issued_at_ns = issued_at_ns;
    // The person's own key dates the certificate, as the identity's root does
    // when it issues one: this machine's clock is the only thing that is off.
    let person = runtime_identity::load_from_path(machine.record()).expect("the person's root key");
    cert.signature = person
        .sign(
            &tilo_user_identity::device_cert_signing_digest(&cert)
                .expect("the certificate's signing digest"),
        )
        .expect("the person's signature");
    persist::write_device_cert(&machine.paths.device_cert, &cert).expect("the certificate");
    assert!(tilo_clock::unix_now_ns() < issued_at_ns);

    let (report, answers) = channel();
    std::thread::spawn(move || report.send(startup::identity()).expect("the answer"));
    assert!(
        matches!(
            answers.recv_timeout(Duration::from_millis(100)),
            Err(RecvTimeoutError::Timeout)
        ),
        "the desk stays off while the certificate is not valid yet"
    );
    assert_eq!(
        answers
            .recv_timeout(Duration::from_secs(30))
            .expect("the desk opens once the clock passes"),
        Identity::Desk
    );
    assert!(
        tilo_clock::unix_now_ns() >= issued_at_ns,
        "the desk opened before the clock reached the certificate"
    );
    assert_eq!(
        persist::read_device_cert(&machine.paths.device_cert).expect("this device's certificate"),
        cert,
        "waiting for the clock replaced the certificate instead"
    );
}

#[test]
fn a_chain_that_does_not_verify_opens_trusted_recovery_and_is_never_replaced() {
    let machine = machine();
    let record = machine.record();
    let record_before = std::fs::read(&record).expect("the identity record");
    // A certificate whose signature does not hold: the record and every key
    // are this person's, and the copy of the identity is what is damaged.
    let mut cert =
        persist::read_device_cert(&machine.paths.device_cert).expect("this device's certificate");
    cert.signature[0] ^= 0xff;
    persist::write_device_cert(&machine.paths.device_cert, &cert).expect("the damaged certificate");
    let damaged = machine.certificate();

    assert_eq!(
        startup::identity(),
        Identity::Recovery(IdentityProblem::ChainInvalid)
    );
    assert!(
        std::env::var_os(IDENTITY_RECOVERY_ENV).is_some(),
        "trusted recovery is what opens"
    );
    assert_eq!(
        IdentityProblem::from_recovery_env(),
        Some(IdentityProblem::ChainInvalid),
        "recovery offers the step that repairs a damaged chain"
    );
    assert_eq!(
        std::fs::read(&record).expect("the identity record"),
        record_before,
        "recovery replaced the identity record"
    );
    assert_eq!(
        machine.certificate(),
        damaged,
        "recovery reissued the certificate instead of leaving it to its person"
    );

    // Nothing below runs in the person's session with these set.
    // SAFETY: this test's own process, and no thread of it reads them after.
    unsafe {
        std::env::remove_var(IDENTITY_RECOVERY_ENV);
        std::env::remove_var(IdentityProblem::RECOVERY_ENV);
    }
    assert_eq!(IdentityProblem::from_recovery_env(), None);
}

#[test]
fn a_record_that_does_not_even_parse_starts_setup_without_replacing_it() {
    let machine = machine();
    let record = machine.record();
    let damaged = b"{ not an identity".to_vec();
    std::fs::write(&record, &damaged).expect("the damaged record");

    assert_eq!(
        startup::identity(),
        Identity::Desk,
        "setup validates what is there and asks its person, and is not what starts a session"
    );
    assert_eq!(
        std::fs::read(&record).expect("the identity record"),
        damaged,
        "setup replaced the record it could not read"
    );
}

/// The session binary's answer to `flags` on `machine`, as the login runs it.
fn run_session(machine: &Machine, flags: &[&str]) -> std::process::Output {
    // Started under the environment lock, which closes this test's scope while
    // the child copies the environment.
    use tilo_test_env::SpawnUnderEnvLock;
    std::process::Command::new(tilo_test_env::package_binary!("tilo-session"))
        .args(flags)
        .env_clear()
        .env("HOME", &machine.home)
        .env("XDG_CONFIG_HOME", &machine.config)
        .env("XDG_RUNTIME_DIR", &machine.runtime)
        .env("TILO_IDENTITY_ROOT", &machine.system)
        .env(FILE_SECRETS_ENV, "1")
        .env("PATH", "/usr/bin:/bin")
        .output_locked()
        .expect("the session binary runs")
}

#[test]
fn the_session_binary_provisions_and_verifies_the_machines_identity() {
    let machine = machine();
    // The host package's installer and its tests start the session this way,
    // so both commands have to answer on the new binary rather than the shell.
    let provisioned = run_session(&machine, &["--host-desktop", "--provision-identity"]);
    assert!(
        provisioned.status.success(),
        "provisioning failed: {}",
        String::from_utf8_lossy(&provisioned.stderr)
    );
    assert!(
        String::from_utf8_lossy(&provisioned.stdout).contains("provisioned"),
        "provisioning said: {}",
        String::from_utf8_lossy(&provisioned.stdout)
    );
    let record = machine.record();
    assert!(record.is_file(), "provisioning wrote no record");

    let verified = run_session(
        &machine,
        &["--host-desktop", "--verify-collaboration-identity"],
    );
    assert!(
        verified.status.success(),
        "verification failed: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&verified.stdout).trim(),
        "Tilo collaboration identity verified"
    );

    // A second provisioning proves the identity that is there, and leaves it
    // as it is: nobody is minted a second identity onto a used machine.
    let record_before = std::fs::read(&record).expect("the identity record");
    let certificate_before = machine.certificate();
    let again = run_session(&machine, &["--host-desktop", "--provision-identity"]);
    assert!(
        again.status.success(),
        "provisioning an established identity failed: {}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_eq!(
        std::fs::read(&record).expect("the identity record"),
        record_before,
        "a second provisioning replaced the identity record"
    );
    assert_eq!(
        machine.certificate(),
        certificate_before,
        "a second provisioning replaced this device's certificate"
    );
}

#[test]
fn the_session_binary_refuses_two_identity_commands_at_once() {
    let machine = machine();
    let both = run_session(
        &machine,
        &[
            "--host-desktop",
            "--provision-identity",
            "--verify-collaboration-identity",
        ],
    );
    assert_eq!(
        both.status.code(),
        Some(2),
        "two identity commands at once are a usage error, not a run"
    );
    assert!(
        String::from_utf8_lossy(&both.stderr).contains("mutually exclusive"),
        "the refusal says what is wrong: {}",
        String::from_utf8_lossy(&both.stderr)
    );
}
