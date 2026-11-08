//! The machine's first boot: which binary the unit starts, what that binary does with the flag it
//! is given, and what it settles about the identity the machine already holds before it opens a
//! desk.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::path::PathBuf;
use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_onboarding_host::identity::IdentityProblem;
use tilo_onboarding_host::identity_provisioning::IDENTITY_RECOVERY_ENV;
use tilo_session::mount::firstboot::{OneShot, one_shot};
use tilo_session::startup::{self, Identity};
use tilo_session::{BeforeDesk, Options, Session};
use tilo_user_identity::{DeviceCertError, IdentityPaths, persist};

/// The unit the image ships for the machine's own first-boot setup.
const UNIT: &str = include_str!("../../../../os/usr/lib/systemd/system/tilo-firstboot.service");

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
    // The live installer's boots are disposable and must not consume the one-shot marker of the
    // machine it installs onto.
    assert!(
        UNIT.lines()
            .any(|line| line == "ConditionKernelCommandLine=!tilo.install"),
        "the live medium still skips the installed machine's first boot"
    );
}

/// A machine with an identity of its own: the person's trees, this device's enrolled key and
/// certificate, and nothing else on it.
struct Machine {
    // The environment the identity lives in, restored before the directories it names are removed.
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
    system: PathBuf,
    runtime: PathBuf,
    paths: IdentityPaths,
}

impl Machine {
    /// This device's certificate as it is on disk.
    fn certificate(&self) -> Vec<u8> {
        std::fs::read(&self.paths.device_cert).expect("this device's certificate")
    }
}

/// The record the session resolves for this account: what it reads before it decides whether the
/// desk opens.
fn record() -> PathBuf {
    tilo_onboarding_host::identity_provisioning::DiskIdentityStore::from_env()
        .expect("the identity store")
        .path()
        .to_path_buf()
}

/// A machine whose person enrolled this device: an identity provisioned the way the installer and
/// the host package provision one.
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

/// The resource publishing `action` , while the session publishes it at all.
fn publisher(session: &Session, action: &str) -> Option<String> {
    fn find(resources: &[tilo_actions::ResourceSpec], action: &str) -> Option<String> {
        resources.iter().find_map(|resource| {
            if resource.actions.iter().any(|offered| offered.id == action) {
                Some(resource.id.as_str().to_owned())
            } else {
                find(&resource.children, action)
            }
        })
    }
    find(&session.resources(), action)
}

#[test]
fn a_verified_identity_opens_the_desk() {
    let _machine = machine();
    assert_eq!(startup::identity(), Identity::Desk);
}

/// Date this device's certificate `ahead` from now, signed by the person's own root as the
/// identity's issuer dates one: the machine's clock is the only thing that is off. Returns the
/// instant the certificate becomes valid.
fn certificate_dated_ahead(machine: &Machine, ahead: Duration) -> u64 {
    let issued_at_ns = tilo_clock::unix_now_ns()
        .saturating_add(u64::try_from(ahead.as_nanos()).expect("a wait in nanoseconds"));
    let mut cert =
        persist::read_device_cert(&machine.paths.device_cert).expect("this device's certificate");
    cert.issued_at_ns = issued_at_ns;
    let person = runtime_identity::load_from_path(record()).expect("the person's root key");
    cert.signature = person
        .sign(
            &tilo_user_identity::device_cert_signing_digest(&cert)
                .expect("the certificate's signing digest"),
        )
        .expect("the person's signature");
    persist::write_device_cert(&machine.paths.device_cert, &cert).expect("the certificate");
    issued_at_ns
}

/// A certificate the clock reads as not yet valid leaves the identity intact: startup answers at
/// once and hands the session the wait for the clock, which opens in front of the desk instead of
/// the desk.
#[test]
fn a_certificate_the_clock_reads_as_not_yet_valid_waits_for_the_clock() {
    let machine = machine();
    let issued_at_ns = certificate_dated_ahead(&machine, Duration::from_secs(3));
    assert!(tilo_clock::unix_now_ns() < issued_at_ns);

    let identity = startup::identity();
    let Identity::WaitForClock { refusal, .. } = &identity else {
        panic!("startup does not wait for the clock: {identity:?}");
    };
    assert_eq!(refusal.reason, DeviceCertError::NotYetValid);
    assert_eq!(refusal.valid_at_ns(), Some(issued_at_ns));
}

/// The waiting screen stands in front of the desk while the clock is behind the certificate, says
/// so, and goes away by itself once the clock passes.
#[test]
fn the_wait_for_the_clock_shows_until_the_desk_can_open() {
    let machine = machine();
    // Far enough ahead that the session is up and drawn before it passes.
    let issued_at_ns = certificate_dated_ahead(&machine, Duration::from_secs(3));
    let identity = startup::identity();
    assert!(matches!(identity, Identity::WaitForClock { .. }));
    let before =
        persist::read_device_cert(&machine.paths.device_cert).expect("this device's certificate");

    let setup = startup::before_desk(&identity, false).expect("the wait stands before the desk");
    assert!(matches!(setup, BeforeDesk::WaitForClock(_)));
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            setup: Some(setup),
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).expect("a frame");
    assert!(
        session.setup_up(),
        "the desk stays off until the clock passes"
    );
    let waiting = session.accessible_labels();
    assert!(
        waiting
            .iter()
            .any(|label| label.contains("Waiting for the clock")),
        "the wait says what it waits for: {waiting:?}"
    );
    assert!(
        publisher(&session, "overview").is_none(),
        "the wait withholds the desk's actions"
    );

    let deadline = tilo_test_env::hang_deadline();
    while session.setup_up() {
        assert!(
            std::time::Instant::now() < deadline,
            "the desk never opened once the clock passed"
        );
        session.step(Duration::from_millis(100)).expect("a frame");
    }
    assert!(
        tilo_clock::unix_now_ns() >= issued_at_ns,
        "the desk opened before the clock reached the certificate"
    );
    let opened = session.accessible_labels();
    assert!(
        !opened
            .iter()
            .any(|label| label.contains("Waiting for the clock")),
        "the wait left the desk: {opened:?}"
    );
    assert!(
        publisher(&session, "overview").is_some(),
        "the desk's actions are back"
    );
    assert_eq!(
        persist::read_device_cert(&machine.paths.device_cert).expect("this device's certificate"),
        before,
        "waiting for the clock replaced the certificate instead"
    );
}

#[test]
fn a_chain_that_does_not_verify_opens_trusted_recovery_and_is_never_replaced() {
    let machine = machine();
    let record_path = record();
    let record_before = std::fs::read(&record_path).expect("the identity record");
    // The markers this leaves in the environment are this test's alone.
    let mut markers = tilo_test_env::ScopedEnv::new();
    markers
        .remove(IDENTITY_RECOVERY_ENV)
        .remove(IdentityProblem::RECOVERY_ENV);
    // A certificate whose signature does not hold: the record and every key are this person's, and
    // the copy of the identity is what is damaged.
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
        std::fs::read(&record_path).expect("the identity record"),
        record_before,
        "recovery replaced the identity record"
    );
    assert_eq!(
        machine.certificate(),
        damaged,
        "recovery reissued the certificate instead of leaving it to its person"
    );

    drop(markers);
    assert_eq!(IdentityProblem::from_recovery_env(), None);
}

#[test]
fn a_record_that_does_not_even_parse_starts_setup_without_replacing_it() {
    let _machine = machine();
    let record_path = record();
    let damaged = b"{ not an identity".to_vec();
    std::fs::write(&record_path, &damaged).expect("the damaged record");

    assert_eq!(
        startup::identity(),
        Identity::Desk,
        "setup validates what is there and asks its person, and is not what starts a session"
    );
    assert_eq!(
        std::fs::read(&record_path).expect("the identity record"),
        damaged,
        "setup replaced the record it could not read"
    );
}

#[test]
fn an_enrolled_device_takes_up_its_persons_identity_at_login() {
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

    let machine = machine();
    // A node's session, certified when the node was installed: a member certificate its person
    // signed, this device's key, no root beside it and no record of the person yet.
    let person = SigningKey::from_bytes(&[7; 32]);
    let (device, _) =
        persist::load_or_create_device_key(&machine.paths).expect("this device's key");
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "node".to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now,
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, &person).expect("the person's signature");
    // The node was enrolled from the factory, never set up here: no root and no key of an identity
    // of its own beside the device key the imager left.
    for path in [
        &machine.paths.user_privkey,
        &machine.paths.user_pubkey,
        &machine.paths.device_cert,
    ] {
        if path.exists() {
            std::fs::remove_file(path).expect("the node holds no identity of its own yet");
        }
    }
    persist::install_device_cert(&machine.paths, &cert, now).expect("the node's enrollment");
    let personal = machine.config.join("tilo").join("identity");
    std::fs::remove_dir_all(&personal).expect("the node holds no record of its person yet");

    assert_eq!(
        startup::identity(),
        Identity::Desk,
        "a login that holds its person's enrollment opens that person's desk"
    );

    // The record the login took up is the person's, selected as the one this session acts as, and
    // nothing minted a root or a second identity beside it.
    let active = std::fs::read_to_string(personal.join("active"))
        .expect("the login selected the record it took up");
    let taken_up = personal.join(format!("{}.json", active.trim()));
    assert!(
        taken_up.is_file(),
        "the selected record {} is not here",
        taken_up.display()
    );
    assert!(
        !personal.join("primary.json").exists(),
        "taking up an enrollment minted a primary identity"
    );
    assert!(
        !machine.paths.user_privkey.exists(),
        "taking up an enrollment minted the person's root on this device"
    );
    assert_eq!(
        record(),
        taken_up,
        "the session resolves the record it took up on the next login"
    );
    assert_eq!(startup::identity(), Identity::Desk, "and it still verifies");
    assert_eq!(
        persist::read_device_cert(&machine.paths.device_cert).expect("this device's certificate"),
        cert,
        "taking up the enrollment reissued the certificate instead of keeping it"
    );
}

/// The session binary's answer to `flags` on `machine` , as the login runs it.
fn run_session(machine: &Machine, flags: &[&str]) -> std::process::Output {
    // Started under the environment lock, which closes this test's scope while the child copies the
    // environment.
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
    let record_path = record();
    assert!(record_path.is_file(), "provisioning wrote no record");

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

    // A second provisioning proves the identity that is there, and leaves it as it is: nobody is
    // minted a second identity onto a used machine.
    let record_before = std::fs::read(&record_path).expect("the identity record");
    let certificate_before = machine.certificate();
    let again = run_session(&machine, &["--host-desktop", "--provision-identity"]);
    assert!(
        again.status.success(),
        "provisioning an established identity failed: {}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_eq!(
        std::fs::read(&record_path).expect("the identity record"),
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
