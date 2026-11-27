//! Setup's ceremony, driven as its view and an API caller drive it: an effect in through
//! [`Ceremony::act`], the page read through [`Ceremony::page`] and [`Page::reading`] after
//! [`Ceremony::step`]. Nothing here reaches into a widget: what a page shows and what its
//! controls mean is the setup app's, and the ceremony is what a press does.

mod person_made_elsewhere;

use super::*;
use os_actions::codec_pack::{CodecAction, CodecPackStatus};
use std::sync::atomic::AtomicUsize;
use tempfile::TempDir;
use tilo_actions::remote::asked::FilesMode;
use tilo_device_identity::fixture::{DeviceDaemon, Login};
use tilo_identity::store::{IdentityRecord, SerializableIdentity};
use tilo_test_env::SpawnUnderEnvLock as _;
use zeroize::Zeroizing;

/// A ceremony whose minter counts its calls, and mints a fixed identity.
fn counted() -> (Ceremony, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let ceremony = Ceremony::with_minter(Box::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        Ok(MintedIdentity {
            fingerprint: "ab".repeat(32),
            path: PathBuf::from("/home/user/.config/tilo/identity/primary.json"),
        })
    }));
    (ceremony, calls)
}

/// `effect` taken as the page shows it now, then the page after one frame.
fn act(ceremony: &mut Ceremony, effect: OnboardingEffect) {
    ceremony.act(effect).expect("setup takes the effect");
    ceremony.step();
}

/// `effect`, then frames until the identity work it started reported and the page shows its
/// outcome.
fn settled(ceremony: &mut Ceremony, effect: OnboardingEffect) {
    ceremony.act(effect).expect("setup takes the effect");
    settle(ceremony);
}

/// One frame, then the frames identity work it started needs, as the loop runs them when the
/// worker wakes it.
fn settle(ceremony: &mut Ceremony) {
    ceremony.step();
    for _ in 0..16 {
        if ceremony.identity_work.is_none() {
            return;
        }
        wait_for_identity_work(ceremony);
        ceremony.step();
    }
    panic!("identity work kept coming");
}

/// Wait until the identity worker reported, if one runs.
fn wait_for_identity_work(ceremony: &Ceremony) {
    let deadline = Instant::now()
        .checked_add(tilo_test_env::hang_bound())
        .expect("deadline");
    while ceremony
        .identity_work
        .as_ref()
        .is_some_and(|work| !work.reported())
    {
        assert!(Instant::now() < deadline, "identity work never reported");
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// Step until `done`, letting identity work and a code's redemption report between frames.
fn step_until(ceremony: &mut Ceremony, what: &str, done: impl Fn(&Ceremony) -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !done(ceremony) {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(20));
        ceremony.step();
    }
}

/// A secret typed into the field `field` names, then one frame.
fn enter_secret(ceremony: &mut Ceremony, field: &str, value: &str) {
    act(
        ceremony,
        OnboardingEffect::EnterSecret {
            field: field.to_owned(),
            value: value.to_owned(),
        },
    );
}

/// A value typed into the field `field` names, then one frame.
fn enter(ceremony: &mut Ceremony, field: &str, value: &str) {
    act(
        ceremony,
        OnboardingEffect::Enter {
            field: field.to_owned(),
            value: value.to_owned(),
        },
    );
}

/// Continue from the backup step without a backup, saying so first.
fn continue_without_backup(ceremony: &mut Ceremony) {
    settled(ceremony, OnboardingEffect::Acknowledge { checked: true });
    settled(ceremony, OnboardingEffect::Continue);
}

/// The words of a recovery key that encodes `seed`.
fn recovery_words(seed: [u8; 32]) -> Zeroizing<String> {
    Zeroizing::new(
        tilo_user_identity::recovery_words::encode(&seed)
            .expect("32 bytes encode")
            .to_string(),
    )
}

/// The recovery key the backup step shows, installed as the identity worker's report installs
/// it, without this computer's daemon to ask.
fn show_recovery_key(ceremony: &mut Ceremony, words: Zeroizing<String>) {
    ceremony.finish_identity_work(IdentityDone::RecoveryWords(Ok(words)));
}

/// The device setup runs on, as its steps are recorded for it: a device key of its own, a
/// daemon that answers what the device is and keeps this account's person, and settings of the
/// person's of its own for this test's thread.
struct Device {
    /// This test's own environment, swept before the home it lives under.
    _env: tilo_test_env::ScopedEnv,
    _home: tilo_test_env::TestHome,
    paths: tilo_device_identity::IdentityPaths,
    _settings: settings_store::SourceScope,
    /// The daemon this device's programs ask, answering the device's public key, until it stops.
    daemon: Option<DeviceDaemon>,
}

impl Device {
    fn new() -> Self {
        Self::with_settings(Arc::new(settings_store::HostDocument::new(
            serde_json::Map::new(),
            |_| Ok(()),
        )))
    }

    fn with_settings(settings: Arc<dyn settings_store::SettingsSource>) -> Self {
        let home = tilo_test_env::TestHome::new().expect("a test home");
        let root = home.root().join("system-identity");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_IDENTITY_ROOT", &root);
        let paths = tilo_device_identity::IdentityPaths::under(&root);
        tilo_device_identity::persist::adopt_device_key(&paths, &[5; 32])
            .expect("this device's key");
        let daemon = DeviceDaemon::holding_the_root(&paths).expect("this device's daemon");
        Self {
            _env: env,
            _home: home,
            paths,
            _settings: settings_store::scoped_source(Some(settings)),
            daemon: Some(daemon),
        }
    }

    /// This device's daemon stops answering, and no process here holds the device's key.
    fn daemon_stops(&mut self) {
        self.daemon = None;
    }

    fn identity(&self) -> session::SystemIdentity {
        session::SystemIdentity {
            paths: self.paths.clone(),
        }
    }

    /// Run `ceremony`'s setup on this device.
    fn runs(&self, ceremony: &mut Ceremony) {
        ceremony.session.identity = Some(self.identity());
    }

    /// Setup as a restart opens it on this device, over the identity `identity_dir` holds.
    fn restarted(&self, identity_dir: &Path) -> Ceremony {
        Ceremony::new_in_session(
            identity_dir.to_path_buf(),
            SessionPaths {
                identity: Some(self.identity()),
                ..SessionPaths::default()
            },
            OnboardingContext::FullSystem,
            false,
        )
    }

    /// The step the person's settings hold for this device.
    fn step(&self) -> Option<Stage> {
        progress::recorded(&self.paths).map(|step| step.expect("a step this build names"))
    }

    /// Record `stage` as setup on this device does when it reaches it.
    fn reached(&self, stage: Stage) {
        progress::record(&self.paths, Some(stage)).expect("the step is recorded");
    }

    /// Keep `value` as this device's step, whatever it holds.
    fn holds(&self, value: serde_json::Value) {
        settings_store::update_section(
            progress::SECTION,
            serde_json::Map::from_iter([(
                progress::leaf(&self.paths).expect("this device's leaf"),
                value,
            )]),
        )
        .expect("the step is kept");
    }
}

/// Setup on a logged-out account that can join: the identity directory it writes into, this
/// computer's device key and certificates.
fn joining(account: &crate::nearby::tests::Account) -> (Ceremony, PathBuf) {
    let identity_dir = account.identity_dir();
    let (mut ceremony, _) = counted();
    ceremony.identity_dir = Some(identity_dir.clone());
    ceremony.session.identity = Some(session::SystemIdentity {
        paths: account.paths.clone(),
    });
    (ceremony, identity_dir)
}

/// A code in the form `tilo device add` prints it.
fn a_code() -> String {
    let issuer = tilo_device_identity::login::Reached {
        endpoint: *ed25519_dalek::SigningKey::from_bytes(&[9; 32])
            .verifying_key()
            .as_bytes(),
        relay: None,
        direct: Vec::new(),
    };
    tilo_device_identity::login::Ticket::new(issuer).encode()
}

/// An identity record whose secret derives its public key, so it loads and signs the way one
/// setup made does. Returns its fingerprint.
fn write_signing_identity(directory: &std::path::Path) -> String {
    let public = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32])
        .verifying_key()
        .to_bytes();
    write_identity_record(directory, public);
    hex::encode(public)
}

/// An identity record at `directory`, with `group_public_key`.
fn write_fixture_identity(directory: &std::path::Path) {
    write_identity_record(directory, [0xab; 32]);
}

fn write_identity_record(directory: &std::path::Path, group_public_key: [u8; 32]) {
    let record = IdentityRecord {
        display_name: "Test user".into(),
        avatar_color: [20, 120, 220],
        profile_photo: None,
        threshold_identity: SerializableIdentity {
            single_device: true,
            group_public_key,
            threshold: 1,
            total_shares: 1,
            share_index: 1,
            secret_bytes: Zeroizing::new(vec![0x42; 32]),
        },
        created_at: "2026-01-01T00:00:00Z".into(),
        schema_version: 1,
        secret_ref: None,
    };
    tilo_files::native::atomic::write_with_mode(
        &directory.join("primary.json"),
        &record.to_json_bytes().expect("fixture identity JSON"),
        0o600,
    )
    .expect("write fixture identity");
}

/// A handler that blocks until the returned sender releases it, and records the thread it ran
/// on: the test's stand-in for a mint, import or export that waits seconds for the disk or a
/// passphrase stretch.
fn blocking_handler() -> (
    impl Fn() + Send + Sync + 'static,
    std::sync::mpsc::Sender<()>,
    Arc<Mutex<Option<std::thread::ThreadId>>>,
) {
    let (release, released) = std::sync::mpsc::channel::<()>();
    let released = Mutex::new(released);
    let ran_on = Arc::new(Mutex::new(None));
    let recorded = Arc::clone(&ran_on);
    let wait = move || {
        *recorded.lock().unwrap() = Some(std::thread::current().id());
        released
            .lock()
            .unwrap()
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("the test releases the handler");
    };
    (wait, release, ran_on)
}

/// Continue from the intro to the backup step, minting the identity `counting_minter` makes.
fn finish_identity(ceremony: &mut Ceremony) {
    for effect in [
        OnboardingEffect::Continue,
        OnboardingEffect::Acknowledge { checked: true },
        OnboardingEffect::Continue,
    ] {
        settled(ceremony, effect);
    }
    assert_eq!(ceremony.page().screen, Screen::Done);
}

fn counting_minter(calls: &Arc<AtomicUsize>, identity_dir: &std::path::Path) -> Minter {
    let calls = Arc::clone(calls);
    let path = identity_dir.join("primary.json");
    Box::new(move || {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(MintedIdentity {
            fingerprint: "ab".repeat(32),
            path: path.clone(),
        })
    })
}

tilo_test_env::scenario! {
    fn the_nearby_card_asks_for_a_code_and_adds_this_computer;

    phase the_nearby_page_waits_for_a_code_and_sends_nothing_before_join;
    {
        let account = crate::nearby::tests::Account::new();
        let (mut ceremony, _) = joining(&account);
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        act(&mut ceremony, OnboardingEffect::choose("nearby"));
        assert_eq!(ceremony.page().screen, Screen::Nearby);
        assert_eq!(ceremony.page().step(), Step::Nearby);
        assert!(ceremony.nearby.is_none(), "nothing is sent before Join");
        assert_eq!(ceremony.error(), None, "opening the screen is not an error");

        // Nothing typed: the person is told what to type, and no device is called.
        act(&mut ceremony, OnboardingEffect::Retry);
        assert_eq!(
            ceremony.error(),
            Some(t("onboarding-nearby-ticket-empty").as_str())
        );
        assert!(ceremony.nearby.is_none());

        // A mistyped code is named as one this computer cannot use, and the field it was typed
        // into starts empty again.
        enter_secret(&mut ceremony, "nearby-code", "K7Q-MX2P");
        let epoch = ceremony.page().epoch;
        act(&mut ceremony, OnboardingEffect::Retry);
        let failure = ceremony.nearby_failure.clone().expect("a designed failure");
        assert_eq!(failure.cause, crate::nearby::NearbyCause::Wrong);
        assert_eq!(ceremony.error(), Some(failure.message().as_str()));
        assert!(ceremony.ticket.lock().unwrap().is_empty());
        assert!(ceremony.nearby.is_none());
        assert!(
            ceremony.page().epoch > epoch,
            "the code's field starts empty again"
        );
        assert_eq!(
            ceremony.page().reading(false)["nearby_failure_title"],
            failure.title()
        );

        // Back leaves the flow's words behind.
        act(&mut ceremony, OnboardingEffect::Back);
        assert_eq!(ceremony.page().screen, Screen::Existing);
        assert!(ceremony.error().is_none());
        assert!(ceremony.nearby_failure.is_none());
    }

    phase a_used_code_says_to_make_a_new_one_and_offers_join_again;
    {
        let account = crate::nearby::tests::Account::new();
        let _daemon = DeviceDaemon::logging_in(
            &account.paths,
            Login::Refuse(
                "Your device didn't take this ticket: this ticket was used already: ask for a new one.",
            ),
            false,
        )
        .expect("the daemon's socket");
        let (mut ceremony, identity_dir) = joining(&account);
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        act(&mut ceremony, OnboardingEffect::choose("nearby"));
        enter_secret(&mut ceremony, "nearby-code", &a_code());
        act(&mut ceremony, OnboardingEffect::Retry);
        step_until(&mut ceremony, "the join to start", |ceremony| {
            ceremony.nearby.is_some() || ceremony.nearby_failure.is_some()
        });
        step_until(&mut ceremony, "the refusal to arrive", |ceremony| {
            ceremony.nearby.is_none()
        });
        let failure = ceremony
            .nearby_failure
            .clone()
            .expect("the refusal is one a person acts on");
        assert_eq!(failure.cause, crate::nearby::NearbyCause::Used);
        assert_eq!(ceremony.error(), Some(failure.message().as_str()));
        assert_eq!(ceremony.page().screen, Screen::Nearby);
        assert_eq!(
            ceremony.page().reading(false)["nearby_failure_title"],
            failure.title()
        );
        assert!(
            failure.title().contains("already used"),
            "the cause is what the page leads with: {}",
            failure.title()
        );
        assert!(
            failure.message().contains("tilo device add"),
            "the message says how to get a new code: {}",
            failure.message()
        );
        assert_eq!(
            ceremony.page().step(),
            Step::Nearby,
            "Join is offered again"
        );
        assert!(
            tilo_identity::store::resolve_active_identity(&identity_dir).is_none(),
            "a refused code adds nothing"
        );
    }

    phase cancelling_a_join_stops_setup_waiting;
    {
        let account = crate::nearby::tests::Account::new();
        let _daemon = DeviceDaemon::logging_in(&account.paths, Login::Wait, false)
            .expect("the daemon's socket");
        let (mut ceremony, _) = joining(&account);
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        act(&mut ceremony, OnboardingEffect::choose("nearby"));
        enter_secret(&mut ceremony, "nearby-code", &a_code());
        let epoch = ceremony.page().epoch;
        act(&mut ceremony, OnboardingEffect::Retry);
        step_until(&mut ceremony, "the join to start", |ceremony| {
            ceremony.nearby.is_some()
        });
        assert!(ceremony.page().nearby.active, "the wait is on the page");
        assert!(
            ceremony.next_deadline().is_some(),
            "setup keeps looking while the join runs"
        );
        assert_eq!(ceremony.page().error, None);

        act(&mut ceremony, OnboardingEffect::Cancel);
        assert!(ceremony.nearby.is_none());
        assert_eq!(
            ceremony.error(),
            Some(t("onboarding-nearby-cancelled").as_str())
        );
        assert!(ceremony.ticket.lock().unwrap().is_empty());
        assert!(!ceremony.page().nearby.active);
        assert!(
            ceremony.page().epoch >= epoch,
            "the code's field starts empty again"
        );
    }

    phase a_good_code_ends_first_use_on_the_ready_screen;
    {
        let account = crate::nearby::tests::Account::new();
        let _daemon = DeviceDaemon::logging_in(
            &account.paths,
            Login::Certify,
            false,
        )
        .expect("the daemon's socket");
        let (mut ceremony, identity_dir) = joining(&account);
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        act(&mut ceremony, OnboardingEffect::choose("nearby"));

        // The whole line `tilo device add` prints is accepted, not only the code.
        enter_secret(
            &mut ceremony,
            "nearby-code",
            &format!("tilo login {}", a_code()),
        );
        act(&mut ceremony, OnboardingEffect::Retry);
        step_until(&mut ceremony, "the join to start", |ceremony| {
            ceremony.nearby.is_some() || ceremony.nearby_failure.is_some()
        });
        assert!(
            ceremony.ticket.lock().unwrap().is_empty(),
            "the code does not stay in an editable field"
        );
        step_until(
            &mut ceremony,
            "the join to end on the ready screen",
            |ceremony| ceremony.nearby.is_none() && ceremony.identity_ready_page,
        );
        assert_eq!(
            ceremony.nearby_failure, None,
            "the join ended well: {:?}",
            ceremony.error()
        );
        assert_eq!(ceremony.page().screen, Screen::Done);
        assert_eq!(ceremony.page().step(), Step::Ready);
        assert_eq!(ceremony.error(), None);
        assert_eq!(
            progress::recorded(&account.paths),
            Some(Ok(Stage::ReadyToEnter)),
            "a restart resumes at the ready screen"
        );
        let page = ceremony.page();
        assert_eq!(page.reading(false)["joined"], true);
        assert!(
            page.joined_device.is_some(),
            "the ready screen names the device that joined"
        );
        assert!(
            !page.backup_export_available,
            "a joined device is not asked to back up an identity it does not hold"
        );

        // The joined identity is the account's, so a restart resumes there.
        drop(ceremony);
        let mut resumed = Ceremony::new_in_session(
            identity_dir,
            SessionPaths {
                identity: Some(session::SystemIdentity {
                    paths: account.paths.clone(),
                }),
                ..SessionPaths::default()
            },
            OnboardingContext::FullSystem,
            false,
        );
        assert_eq!(resumed.page().screen, Screen::Done);
        assert_eq!(resumed.page().step(), Step::Ready);
        settled(&mut resumed, OnboardingEffect::Finish);
        assert!(resumed.finished(), "{:?}", resumed.page().progress_error);
        assert!(!progress::unfinished(&account.paths));
    }
}

tilo_test_env::scenario! {
    fn identity_work_never_runs_on_the_frame_thread;

    phase minting_never_runs_on_the_frame_thread;
    // Minting, importing and exporting the identity wait for the disk, the secret store and a
    // passphrase stretch. They are taken from a frame on the compositor's loop, so they must
    // never run there: with a handler that never finishes, every frame still returns and the
    // step waits.
    {
        let directory = TempDir::new().expect("identity directory");
        let identity_dir = directory.path().join("identity");
        let (wait, release, ran_on) = blocking_handler();
        let record = identity_dir.join("primary.json");
        let mut ceremony = Ceremony::with_minter(Box::new(move || {
            wait();
            Ok(MintedIdentity {
                fingerprint: "ab".repeat(32),
                path: record.clone(),
            })
        }));
        ceremony.identity_dir = Some(identity_dir);
        let device = Device::new();
        device.runs(&mut ceremony);
        settled(&mut ceremony, OnboardingEffect::Continue);
        settled(
            &mut ceremony,
            OnboardingEffect::Acknowledge { checked: true },
        );
        act(&mut ceremony, OnboardingEffect::Continue);
        assert!(ceremony.identity_work.is_some(), "the mint is running");
        let deadline = Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .expect("deadline");
        while ran_on.lock().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the minter never started");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_ne!(
            *ran_on.lock().unwrap(),
            Some(std::thread::current().id()),
            "the minter runs off the frame thread"
        );
        // Frames keep coming while it runs; the step it belongs to holds.
        act(&mut ceremony, OnboardingEffect::Back);
        for _ in 0..3 {
            ceremony.step();
        }
        assert_eq!(ceremony.page().screen, Screen::NoRecovery);
        assert!(
            ceremony.next_deadline().is_some(),
            "the loop looks again"
        );
        assert_eq!(
            device.step(),
            None,
            "nothing is kept before the identity exists"
        );

        release.send(()).unwrap();
        settle(&mut ceremony);
        assert_eq!(ceremony.page().screen, Screen::Done);
        assert_eq!(
            device.step(),
            Some(Stage::IdentityReady),
            "the identity's step is recorded once the mint reports"
        );
    }

    phase importing_never_runs_on_the_frame_thread;
    // See [`minting_never_runs_on_the_frame_thread`].
    {
        let directory = TempDir::new().expect("identity directory");
        let identity_dir = directory.path().join("identity");
        let (wait, release, ran_on) = blocking_handler();
        let record = identity_dir.join("primary.json");
        let mut ceremony = Ceremony::with_handlers(
            Box::new(|| Err("create should not run".to_string())),
            Box::new(move |_, passphrase| {
                assert_eq!(passphrase, b"restore passphrase");
                wait();
                Ok(MintedIdentity {
                    fingerprint: "cd".repeat(32),
                    path: record.clone(),
                })
            }),
        );
        ceremony.identity_dir = Some(identity_dir);
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        act(&mut ceremony, OnboardingEffect::choose("backup-file"));
        enter(&mut ceremony, "backup-file", "/media/backup/primary.json");
        enter_secret(&mut ceremony, "backup-passphrase", "restore passphrase");
        settle(&mut ceremony);
        act(&mut ceremony, OnboardingEffect::Continue);
        assert!(ceremony.identity_work.is_some(), "the import is running");
        for _ in 0..3 {
            ceremony.step();
        }
        assert_eq!(ceremony.page().screen, Screen::Restore);

        release.send(()).unwrap();
        settle(&mut ceremony);
        assert_ne!(*ran_on.lock().unwrap(), Some(std::thread::current().id()));
        assert_eq!(ceremony.page().screen, Screen::Done);
        assert!(
            ceremony.connect_passphrase.lock().unwrap().is_empty(),
            "the passphrase is cleared once the import reports"
        );
    }

    phase exporting_never_runs_on_the_frame_thread;
    // See [`minting_never_runs_on_the_frame_thread`].
    {
        let progress = TempDir::new().expect("progress directory");
        let (wait, release, ran_on) = blocking_handler();
        let mut ceremony = Ceremony::with_all_handlers(
            Box::new(|| {
                Ok(MintedIdentity {
                    fingerprint: "ef".repeat(32),
                    path: PathBuf::from("/identity/primary.json"),
                })
            }),
            Box::new(|_, _| Err("unused".to_string())),
            Box::new(move |_, _, _| {
                wait();
                Ok(())
            }),
        );
        finish_backup_step(&mut ceremony);
        ceremony.identity_dir = Some(progress.path().to_path_buf());
        let device = Device::new();
        device.runs(&mut ceremony);
        enter_secret(
            &mut ceremony,
            "new-passphrase",
            "correct horse battery staple",
        );
        enter_secret(
            &mut ceremony,
            "new-passphrase-confirmation",
            "correct horse battery staple",
        );
        act(&mut ceremony, OnboardingEffect::Browse);
        assert_eq!(
            ceremony.file_ask().map(|asked| asked.question().mode),
            Some(FilesMode::Save)
        );
        // The person picks where the backup goes in the system's picker; the frame hands the
        // export to its own worker.
        ceremony.files_answered(vec![
            progress
                .path()
                .join("tilo-identity-backup.tilo-identity")
                .to_string_lossy()
                .into_owned(),
        ]);
        ceremony.step();
        assert!(ceremony.file_ask().is_none());
        assert!(ceremony.identity_work.is_some(), "the export is running");
        for _ in 0..3 {
            ceremony.step();
        }
        assert!(ceremony.page().backup_saved.is_none());

        release.send(()).unwrap();
        settle(&mut ceremony);
        assert_ne!(*ran_on.lock().unwrap(), Some(std::thread::current().id()));
        assert!(ceremony.page().backup_saved.is_some());
        assert_eq!(device.step(), Some(Stage::IdentityReadyBackedUp));
    }
}

/// Mint the identity and reach its backup step, with the ceremony's own minter.
fn finish_backup_step(ceremony: &mut Ceremony) {
    for effect in [
        OnboardingEffect::Continue,
        OnboardingEffect::Acknowledge { checked: true },
        OnboardingEffect::Continue,
    ] {
        settled(ceremony, effect);
    }
    assert_eq!(ceremony.page().step(), Step::Backup);
}

tilo_test_env::scenario! {
    fn setup_resumes_where_the_person_was_and_fails_closed;

    phase each_step_is_recorded_once_its_identity_exists_and_removed_at_finish;
    {
        let directory = TempDir::new().expect("identity directory");
        let identity_dir = directory.path().join("identity");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ceremony = Ceremony::with_minter(counting_minter(&calls, &identity_dir));
        ceremony.identity_dir = Some(identity_dir);
        let device = Device::new();
        device.runs(&mut ceremony);
        let mut steps = Vec::new();
        // The keyboard/action route carries the same guard as the disabled button: a stale or
        // malicious action cannot skip the only recovery decision.
        for effect in [
            OnboardingEffect::Continue,
            OnboardingEffect::Acknowledge { checked: true },
            OnboardingEffect::Continue,
            OnboardingEffect::Continue,
            OnboardingEffect::Acknowledge { checked: true },
            OnboardingEffect::Continue,
            OnboardingEffect::Finish,
        ] {
            settled(&mut ceremony, effect);
            assert_eq!(ceremony.page().progress_error, None);
            steps.push(device.step());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            steps,
            [
                None,
                None,
                Some(Stage::IdentityReady),
                Some(Stage::IdentityReady),
                Some(Stage::IdentityReady),
                Some(Stage::ReadyToEnter),
                None,
            ],
            "nothing is kept before the identity exists, and finishing removes the step"
        );
        assert!(ceremony.finished());
        assert!(!progress::unfinished(&device.paths));
    }

    phase a_person_restarted_at_each_step_carries_on_where_they_were;
    // The person makes their identity, the computer restarts, and setup shows the backup choice
    // again without making another identity; they skip the backup, it restarts again, and the
    // ready screen is what they see; a restart mid-tour reopens the page they were reading, and
    // once they are through, a restart goes to the desktop.
    {
        let directory = TempDir::new().expect("identity directory");
        let identity_dir = directory.path().to_path_buf();
        let calls = Arc::new(AtomicUsize::new(0));
        let minted = Arc::clone(&calls);
        let written = identity_dir.clone();
        let mut ceremony = Ceremony::with_minter(Box::new(move || {
            minted.fetch_add(1, Ordering::SeqCst);
            Ok(MintedIdentity {
                fingerprint: write_signing_identity(&written),
                path: written.join("primary.json"),
            })
        }));
        ceremony.identity_dir = Some(identity_dir.clone());
        let device = Device::new();
        device.runs(&mut ceremony);
        finish_identity(&mut ceremony);
        drop(ceremony);

        let mut backup = device.restarted(&identity_dir);
        assert_eq!(backup.error(), None);
        assert_eq!(
            backup.page().step(),
            Step::Backup,
            "the backup choice is offered again"
        );
        assert!(!backup.flow().can_create(), "the identity is not made again");
        for effect in [
            OnboardingEffect::Acknowledge { checked: true },
            OnboardingEffect::Continue,
        ] {
            settled(&mut backup, effect);
        }
        assert!(backup.page().identity_ready_page);
        drop(backup);

        let mut ready = device.restarted(&identity_dir);
        assert_eq!(
            ready.page().step(),
            Step::Ready,
            "the backup choice the person made is not asked again"
        );
        for effect in [OnboardingEffect::choose("tour"), OnboardingEffect::Continue] {
            settled(&mut ready, effect);
        }
        assert_eq!(ready.page().tour_page, Some(TourPage::Organise));
        drop(ready);

        let mut touring = device.restarted(&identity_dir);
        assert_eq!(
            touring.page().tour_page,
            Some(TourPage::Organise),
            "the tour opens on the page they were reading"
        );
        settled(&mut touring, OnboardingEffect::Skip);
        assert!(touring.finished());
        drop(touring);

        assert_eq!(
            device.restarted(&identity_dir).page().screen,
            Screen::Intro,
            "a finished setup leaves the person at their desktop"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one identity, made once");
    }

    phase a_restart_resumes_the_backup_and_ready_steps_without_making_the_identity_again;
    {
        let directory = TempDir::new().expect("identity directory");
        write_fixture_identity(directory.path());
        let device = Device::new();

        device.reached(Stage::IdentityReady);
        let mut backup = device.restarted(directory.path());
        assert_eq!(backup.page().screen, Screen::Done);
        assert!(!backup.page().identity_ready_page);
        settled(&mut backup, OnboardingEffect::Finish);
        assert!(!backup.finished(), "backup choice cannot be bypassed");

        device.reached(Stage::IdentityReadyBackedUp);
        let backed_up = device.restarted(directory.path());
        assert!(!backed_up.page().identity_ready_page);
        assert!(
            backed_up.page().backup_was_verified,
            "a verified backup is not asked for again"
        );

        device.reached(Stage::ReadyToEnter);
        let mut done = device.restarted(directory.path());
        assert_eq!(done.page().screen, Screen::Done);
        assert!(done.flow().is_done());
        assert_eq!(
            done.flow()
                .minted()
                .map(|identity| identity.fingerprint.as_str()),
            Some("abababababababababababababababababababababababababababababababab")
        );
        assert!(!done.flow().can_create(), "resume never reopens creation");

        // The ready hand-off is one action; a first session teaches itself on the desktop
        // instead of through pages read in advance.
        settled(&mut done, OnboardingEffect::Finish);
        assert!(done.finished());
        assert!(
            !done.presents_desktop_context(),
            "nothing of first login is left over the desktop"
        );
        assert!(!progress::unfinished(&device.paths));

        let established = device.restarted(directory.path());
        assert_eq!(
            established.page().screen,
            Screen::Intro,
            "without a step to resume, startup leaves an established person alone"
        );
    }

    phase the_setup_another_device_of_theirs_is_in_never_opens_here;
    {
        let directory = TempDir::new().expect("identity directory");
        write_fixture_identity(directory.path());
        let device = Device::new();
        settings_store::update_section(
            progress::SECTION,
            serde_json::Map::from_iter([("cd".repeat(32), serde_json::json!("identity-ready"))]),
        )
        .expect("another device's step");
        assert!(!progress::unfinished(&device.paths));
        assert_eq!(
            device.restarted(directory.path()).page().screen,
            Screen::Intro
        );
    }

    phase a_person_made_here_lands_on_the_backup_and_may_join_the_person_they_had;
    {
        let directory = TempDir::new().expect("identity directory");
        write_fixture_identity(directory.path());
        let device = Device::new();
        // Startup made this account's person through the daemon, and recorded the step that
        // leaves the person at their backup: a computer set up for one person still gets it.
        record_backup_step().expect("the setup step is kept");
        assert!(progress::unfinished(&device.paths));
        let mut backup = device.restarted(directory.path());
        assert_eq!(backup.page().screen, Screen::Done);
        assert!(
            !backup.page().identity_ready_page,
            "the backup step, not the ready page"
        );
        // The person who already uses Tilo on another device joins it here.
        settled(&mut backup, OnboardingEffect::choose("nearby"));
        assert_eq!(backup.page().screen, Screen::Nearby);
        // Back returns to the backup it left, not to a choice an identity already settles.
        settled(&mut backup, OnboardingEffect::Back);
        assert_eq!(backup.page().screen, Screen::Done);
        assert_eq!(backup.page().step(), Step::Backup);
    }

    phase recovery_never_resumes_a_step_that_assumes_the_identity_signs;
    // Recovery opens because the identity cannot be used; a step recorded while it could must
    // not put the ready screen over that.
    {
        let directory = TempDir::new().expect("identity directory");
        write_fixture_identity(directory.path());
        let device = Device::new();
        device.reached(Stage::ReadyToEnter);
        let recovery = Ceremony::new_in_session(
            directory.path().to_path_buf(),
            SessionPaths {
                identity: Some(device.identity()),
                ..SessionPaths::default()
            },
            OnboardingContext::FullSystem,
            true,
        );
        assert_ne!(recovery.page().screen, Screen::Done);
        assert!(!recovery.page().identity_ready_page);
        assert_eq!(
            device.step(),
            Some(Stage::ReadyToEnter),
            "the step waits for the identity to be restored"
        );
    }

    phase a_step_this_build_cannot_name_resumes_at_the_backup;
    {
        let directory = TempDir::new().expect("identity directory");
        let device = Device::new();
        device.holds(serde_json::json!("a-step-of-a-later-build"));
        assert!(
            progress::unfinished(&device.paths),
            "a step nobody can name is setup under way, never a way around it"
        );
        write_fixture_identity(directory.path());
        let resumed = device.restarted(directory.path());
        assert_eq!(resumed.page().screen, Screen::Done);
        assert!(
            !resumed.page().identity_ready_page,
            "nothing the person had not finished is skipped"
        );
        assert!(resumed.page().progress_error.is_some());
    }

    phase a_restart_mid_setup_with_no_daemon_to_name_the_device_keeps_setup_open;
    {
        let directory = TempDir::new().expect("identity directory");
        write_fixture_identity(directory.path());
        let mut device = Device::new();
        device.reached(Stage::TourOrganise);
        device.daemon_stops();
        assert!(
            progress::unfinished(&device.paths),
            "a step nobody can look up is setup under way, never setup finished"
        );
        let resumed = device.restarted(directory.path());
        assert_eq!(resumed.page().screen, Screen::Done);
        assert!(
            !resumed.page().identity_ready_page,
            "setup resumes at its earliest step, so nothing unfinished is skipped"
        );
        assert!(
            resumed.page().progress_error.is_some(),
            "the person is told the step could not be read"
        );
    }

    phase a_device_with_no_key_and_no_daemon_has_no_step_to_resume;
    {
        let mut device = Device::new();
        device.daemon_stops();
        std::fs::remove_file(&device.paths.device_privkey).expect("the device's key goes");
        assert!(
            !progress::unfinished(&device.paths),
            "before its identity a device starts setup over from its first page"
        );
    }

    phase a_step_the_settings_refuse_holds_setup_where_it_is;
    {
        let directory = TempDir::new().expect("identity directory");
        let identity_dir = directory.path().join("identity");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ceremony = Ceremony::with_minter(counting_minter(&calls, &identity_dir));
        ceremony.identity_dir = Some(identity_dir);
        let device = Device::with_settings(Arc::new(settings_store::HostDocument::new(
            serde_json::Map::new(),
            |_| Err(std::io::Error::other("the disk is full")),
        )));
        device.runs(&mut ceremony);
        finish_identity(&mut ceremony);
        assert!(
            ceremony.page().progress_error.is_some(),
            "the refused step is said"
        );
        continue_without_backup(&mut ceremony);
        assert!(
            !ceremony.page().identity_ready_page,
            "the hand-off waits until a restart would open it there"
        );
        assert!(ceremony.page().progress_error.is_some());
    }
}

tilo_test_env::scenario! {
    fn identity_creation_requires_acknowledgement_and_mints_once;

    phase pressing_create_without_acknowledging_mints_nothing;
    // The whole point: clicking through without ticking the box must not produce a key.
    {
        let (mut ceremony, calls) = counted();
        act(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(ceremony.page().screen, Screen::NoRecovery);
        assert_eq!(ceremony.page().step(), Step::Protect);
        assert!(!ceremony.page().can_create);

        act(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "no key was minted");
        assert_eq!(
            ceremony.page().screen,
            Screen::NoRecovery,
            "still on the check"
        );
    }

    phase acknowledging_then_creating_mints_exactly_once;
    {
        let (mut ceremony, calls) = counted();
        act(&mut ceremony, OnboardingEffect::Continue);
        act(
            &mut ceremony,
            OnboardingEffect::Acknowledge { checked: true },
        );
        assert!(ceremony.page().can_create);
        settled(&mut ceremony, OnboardingEffect::Continue);
        // A second step must not re-fire: the effect is consumed.
        ceremony.step();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(ceremony.page().screen, Screen::Done);
        assert!(ceremony.flow().is_done());
        assert_eq!(ceremony.page().step(), Step::Backup);
    }

    phase unticking_the_box_blocks_creation_again;
    {
        let (mut ceremony, calls) = counted();
        act(&mut ceremony, OnboardingEffect::Continue);
        act(
            &mut ceremony,
            OnboardingEffect::Acknowledge { checked: true },
        );
        act(
            &mut ceremony,
            OnboardingEffect::Acknowledge { checked: false },
        );
        assert!(!ceremony.page().can_create);
        act(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    phase a_failed_mint_is_surfaced_rather_than_hidden;
    // A minting failure must be shown, not swallowed into a desktop that silently has no
    // identity.
    {
        let mut ceremony =
            Ceremony::with_minter(Box::new(|| Err("disk full".to_string())));
        act(&mut ceremony, OnboardingEffect::Continue);
        act(
            &mut ceremony,
            OnboardingEffect::Acknowledge { checked: true },
        );
        settled(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(ceremony.page().error.as_deref(), Some("disk full"));
        assert_eq!(
            ceremony.page().screen,
            Screen::NoRecovery,
            "a failed write is not a completed identity"
        );
        assert!(!ceremony.flow().is_done());
        act(&mut ceremony, OnboardingEffect::Finish);
        assert!(!ceremony.finished(), "failure must not release the desktop");
    }
}

tilo_test_env::scenario! {
    fn desktop_tour_is_navigable_skippable_and_dismissable;

    phase desktop_tour_is_complete_backtrackable_and_finishes_only_at_the_end;
    {
        let directory = TempDir::new().expect("identity directory");
        write_fixture_identity(directory.path());
        let device = Device::new();
        device.reached(Stage::TourNavigate);
        let mut ceremony = device.restarted(directory.path());
        assert_eq!(ceremony.page().tour_page, Some(TourPage::Navigate));
        act(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(ceremony.page().tour_page, Some(TourPage::Organise));
        act(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(ceremony.page().tour_page, Some(TourPage::Collaborate));
        act(&mut ceremony, OnboardingEffect::Finish);
        assert!(
            !ceremony.finished(),
            "the tour cannot finish before its hand-off page"
        );
        act(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(ceremony.page().tour_page, Some(TourPage::Start));
        act(&mut ceremony, OnboardingEffect::Back);
        assert_eq!(ceremony.page().tour_page, Some(TourPage::Collaborate));
        act(&mut ceremony, OnboardingEffect::Continue);
        settled(&mut ceremony, OnboardingEffect::Finish);
        assert!(ceremony.finished());
        assert!(!progress::unfinished(&device.paths));
    }

    phase a_reopened_tour_needs_no_identity_and_closes_from_its_first_and_last_page;
    {
        let mut ceremony = Ceremony::new_tour();
        ceremony.step();
        assert_eq!(ceremony.page().tour_page, Some(TourPage::Navigate));
        assert!(
            !ceremony.flow.is_done(),
            "reopening the tour forges no identity"
        );
        act(&mut ceremony, OnboardingEffect::Back);
        assert!(
            ceremony.finished(),
            "Back on the first page closes a reopened tour"
        );

        let mut ceremony = Ceremony::new_tour();
        for _ in 0..3 {
            act(&mut ceremony, OnboardingEffect::Continue);
        }
        assert_eq!(ceremony.page().tour_page, Some(TourPage::Start));
        act(&mut ceremony, OnboardingEffect::Finish);
        assert!(ceremony.finished(), "Enter Tilo closes the reopened tour");
    }

    phase first_use_skip_works_on_every_tour_page_and_clears_progress;
    {
        for (stage, page) in [
            (Stage::TourNavigate, TourPage::Navigate),
            (Stage::TourOrganise, TourPage::Organise),
            (Stage::TourCollaborate, TourPage::Collaborate),
            (Stage::TourStart, TourPage::Start),
        ] {
            let directory = TempDir::new().expect("identity directory");
            write_fixture_identity(directory.path());
            let device = Device::new();
            device.reached(stage);
            let mut ceremony = device.restarted(directory.path());

            assert_eq!(ceremony.page().tour_page, Some(page));
            assert_eq!(ceremony.page().step(), Step::Tour(page));
            assert_eq!(
                ceremony.page().reading(false)["tour"],
                serde_json::json!(page.number())
            );
            settled(&mut ceremony, OnboardingEffect::Skip);

            assert!(ceremony.finished(), "Skip tour must work from {page:?}");
            assert!(
                !progress::unfinished(&device.paths),
                "skipping {page:?} must complete first use for good"
            );
        }
    }
}

tilo_test_env::scenario! {
    fn screen_navigation_follows_context_and_direction;

    phase host_desktop_context_keeps_the_identity_creation_check;
    {
        let mut ceremony = Ceremony::with_all_handlers_in_context(
            Box::new(|| {
                Ok(MintedIdentity {
                    fingerprint: "ab".repeat(32),
                    path: PathBuf::from("/identity/primary.json"),
                })
            }),
            Box::new(|_, _| Err("unused".into())),
            Box::new(|_, _, _| Err("unused".into())),
            OnboardingContext::HostDesktop,
        );
        assert_eq!(ceremony.page().context, OnboardingContext::HostDesktop);
        assert_eq!(ceremony.page().reading(false)["host_desktop"], true);
        assert!(!ceremony.page().can_create);
        act(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(ceremony.page().screen, Screen::NoRecovery);
        assert!(!ceremony.page().can_create);
        act(
            &mut ceremony,
            OnboardingEffect::Acknowledge { checked: true },
        );
        assert!(ceremony.page().can_create);
    }

    phase recovery_mode_starts_on_restore_and_never_offers_creation;
    {
        let identity = tempfile::tempdir().expect("identity directory");
        let mut recovery = Ceremony::isolated(identity.path().to_path_buf(), true);
        let page = recovery.page();
        assert!(page.recovery_only);
        assert_eq!(page.reading(false)["recovery_only"], true);
        assert_eq!(page.screen, Screen::Existing);
        act(&mut recovery, OnboardingEffect::Back);
        assert_eq!(recovery.page().screen, Screen::Existing);
        act(&mut recovery, OnboardingEffect::choose("backup-file"));
        assert_eq!(recovery.page().screen, Screen::Restore);
        act(&mut recovery, OnboardingEffect::Back);
        assert_eq!(
            recovery.page().screen,
            Screen::Existing,
            "recovery flows still return to their choice"
        );
        // Making an identity here is never a way on.
        act(&mut recovery, OnboardingEffect::choose("create"));
        assert_eq!(recovery.page().screen, Screen::Existing);
        assert!(!recovery.page().can_create);
    }
}

tilo_test_env::scenario! {
    fn connecting_an_existing_identity_imports_without_minting;

    phase use_existing_opens_each_choice_on_its_own_screen;
    {
        let (mut ceremony, _) = counted();
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        assert_eq!(ceremony.page().screen, Screen::Existing);
        assert_eq!(ceremony.page().step(), Step::Existing);
        act(&mut ceremony, OnboardingEffect::choose("backup-file"));
        assert_eq!(ceremony.page().screen, Screen::Restore);
        act(&mut ceremony, OnboardingEffect::Back);
        assert_eq!(ceremony.page().screen, Screen::Existing);

        // The code's screen opens on its own; without an identity directory to write into,
        // Join says that and nothing is called.
        act(&mut ceremony, OnboardingEffect::choose("nearby"));
        assert_eq!(ceremony.page().screen, Screen::Nearby);
        assert_eq!(ceremony.error(), None, "opening the screen is not a failure");
        enter_secret(&mut ceremony, "nearby-code", "K7Q-MX2P");
        act(&mut ceremony, OnboardingEffect::Retry);
        assert_eq!(
            ceremony.error(),
            Some(t("onboarding-nearby-unavailable").as_str()),
            "this session cannot add a computer"
        );
        act(&mut ceremony, OnboardingEffect::Back);
        assert_eq!(ceremony.page().screen, Screen::Existing);
        assert!(
            ceremony.error().is_none(),
            "back leaves the flow's error behind"
        );
        act(&mut ceremony, OnboardingEffect::Back);
        assert_eq!(ceremony.page().screen, Screen::Intro);
    }

    phase connect_existing_imports_without_minting;
    {
        let imported = Arc::new(AtomicUsize::new(0));
        let imported_for_handler = imported.clone();
        let mut ceremony = Ceremony::with_handlers(
            Box::new(|| Err("create should not run".to_string())),
            Box::new(move |path, _| {
                imported_for_handler.fetch_add(1, Ordering::SeqCst);
                assert_eq!(path, &PathBuf::from("/media/backup/primary.json"));
                Ok(MintedIdentity {
                    fingerprint: "cd".repeat(32),
                    path: PathBuf::from("/home/user/.config/tilo/identity/cd.json"),
                })
            }),
        );
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        act(&mut ceremony, OnboardingEffect::choose("backup-file"));
        enter(&mut ceremony, "backup-file", "/media/backup/primary.json");
        assert_eq!(ceremony.page().screen, Screen::Restore);
        settled(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(imported.load(Ordering::SeqCst), 1);
        assert_eq!(ceremony.page().screen, Screen::Done);
        assert!(ceremony.flow().is_done());
    }

    phase connect_records_the_identity_once_imported_and_keeps_setup_pending;
    {
        let directory = TempDir::new().expect("identity directory");
        let identity_dir = directory.path().join("identity");
        let progress_dir = identity_dir.clone();
        let mut ceremony = Ceremony::with_handlers(
            Box::new(|| Err("create should not run".to_string())),
            Box::new(move |_, _| {
                Ok(MintedIdentity {
                    fingerprint: "cd".repeat(32),
                    path: progress_dir.join("primary.json"),
                })
            }),
        );
        ceremony.identity_dir = Some(identity_dir);
        let device = Device::new();
        device.runs(&mut ceremony);

        act(&mut ceremony, OnboardingEffect::choose("existing"));
        act(&mut ceremony, OnboardingEffect::choose("backup-file"));
        enter(&mut ceremony, "backup-file", "/media/backup/primary.json");
        settle(&mut ceremony);
        assert_eq!(
            device.step(),
            None,
            "nothing is kept before the identity exists"
        );
        settled(&mut ceremony, OnboardingEffect::Continue);

        assert_eq!(device.step(), Some(Stage::IdentityReady));
        assert_eq!(ceremony.page().screen, Screen::Done);
        assert_eq!(ceremony.page().step(), Step::Backup);
    }

    phase failed_connect_stays_on_connect_screen_and_shows_error;
    {
        let mut ceremony = Ceremony::with_handlers(
            Box::new(|| Err("create should not run".to_string())),
            Box::new(|_, _| Err("not a Tilo identity".to_string())),
        );
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        act(&mut ceremony, OnboardingEffect::choose("backup-file"));
        enter(&mut ceremony, "backup-file", "/media/identity.tilo-identity");
        settled(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(ceremony.page().screen, Screen::Restore);
        assert_eq!(ceremony.error(), Some("not a Tilo identity"));
    }

    phase connect_requires_a_selected_file_before_importing;
    {
        let imports = Arc::new(AtomicUsize::new(0));
        let counted_imports = Arc::clone(&imports);
        let mut ceremony = Ceremony::with_handlers(
            Box::new(|| Err("create should not run".to_string())),
            Box::new(move |_, _| {
                counted_imports.fetch_add(1, Ordering::SeqCst);
                Err("not a Tilo identity".to_string())
            }),
        );
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        act(&mut ceremony, OnboardingEffect::choose("backup-file"));
        assert_eq!(ceremony.page().connect_path, "");
        settled(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(
            ceremony.page().screen,
            Screen::Restore,
            "nothing moves without a file"
        );
        assert_eq!(
            ceremony.error(),
            Some(t("onboarding-connect-choose-file-first").as_str())
        );
        assert_eq!(imports.load(Ordering::SeqCst), 0, "nothing is imported");

        // A file chosen enables the restore, which then runs. A path typed by hand is no pick:
        // the view keeps what the person types and takes the reading only after a pick.
        enter(&mut ceremony, "backup-file", "/media/identity.tilo-identity");
        assert_eq!(ceremony.page().connect_path, "/media/identity.tilo-identity");
        assert_eq!(ceremony.page().connect_picked, 0);
        settled(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(imports.load(Ordering::SeqCst), 1);
    }

    phase connect_browse_asks_for_the_backup_file;
    {
        let (mut ceremony, _) = counted();
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        act(&mut ceremony, OnboardingEffect::choose("backup-file"));
        act(&mut ceremony, OnboardingEffect::Browse);
        let asked = ceremony.file_ask().expect("the person is asked for a file");
        assert_eq!(
            asked.question().mode,
            FilesMode::Open,
            "the backup to open, not a place to write one"
        );

        // Dismissing the question leaves the step where it was, nothing chosen.
        ceremony.files_answered(Vec::new());
        ceremony.step();
        assert!(ceremony.file_ask().is_none());
        assert_eq!(ceremony.page().screen, Screen::Restore);
        assert_eq!(ceremony.page().connect_path, "");
        assert_eq!(ceremony.page().connect_picked, 0, "nothing was picked");

        act(&mut ceremony, OnboardingEffect::Browse);
        ceremony.files_answered(vec!["/home/person/backup.tilo-identity".into()]);
        ceremony.step();
        assert!(ceremony.file_ask().is_none());
        assert_eq!(
            ceremony.page().connect_path,
            "/home/person/backup.tilo-identity"
        );
        assert_eq!(
            ceremony.page().reading(false)["connect_picked"],
            1,
            "the view is told a file was picked"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one named scenario walked through in order"
)]
fn identity_ready_does_not_complete_visible_first_use() {
    const CHILD: &str = "TILO_ONBOARDING_CONTINUATION_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        struct OwnedChild {
            child: Option<std::process::Child>,
            fixture: Option<tempfile::TempDir>,
            executable: Option<std::fs::File>,
        }
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let Some(mut child) = self.child.take() else {
                    return;
                };
                let _ = child.kill();
                let deadline = tilo_test_env::hang_deadline();
                loop {
                    if matches!(child.try_wait(), Ok(Some(_))) {
                        return;
                    }
                    if Instant::now() >= deadline {
                        // Unknown task retirement keeps its fixture and exact image.
                        std::mem::forget(child);
                        if let Some(fixture) = self.fixture.take() {
                            std::mem::forget(fixture);
                        }
                        if let Some(executable) = self.executable.take() {
                            std::mem::forget(executable);
                        }
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        let fixture = tempfile::tempdir().unwrap();
        let executable = std::fs::File::open("/proc/self/exe").unwrap();
        let fd = executable.as_raw_fd();
        let mut command = std::process::Command::new(format!("/proc/self/fd/{fd}"));
        command
            .args([
                "ceremony::tests::identity_ready_does_not_complete_visible_first_use",
                "--exact",
            ])
            .env(CHILD, "1")
            .env("HOME", fixture.path())
            .env("XDG_CONFIG_HOME", fixture.path().join("config"))
            .env("XDG_STATE_HOME", fixture.path().join("state"))
            .env("TILO_IDENTITY_ROOT", fixture.path().join("system"))
            .env(tilo_identity::store::FILE_SECRETS_ENV, "1");
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn_locked().unwrap();
        let mut owned = OwnedChild {
            child: Some(child),
            fixture: Some(fixture),
            executable: Some(executable),
        };
        let status = tilo_test_env::wait_for("certified identity continuation child exit", || {
            owned.child.as_mut().unwrap().try_wait().unwrap()
        });
        owned.child.take();
        assert!(status.success());
        return;
    }
    // The person is made by the device's daemon, which holds the device key: this device's daemon
    // took it, so the person is made here as the daemon would make it.
    let device = Device::new();
    let paths = device.paths.clone();
    let directory = tilo_identity::store::account_identity_dir_from_env().unwrap();
    tilo_identity::provisioning::create_person(
        &directory,
        &paths,
        "",
        "",
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let minted = crate::identity::existing_identity_at(
        &tilo_identity::store::resolve_active_identity(&directory).unwrap(),
    )
    .unwrap();
    let original = std::fs::read(&minted.path).unwrap();
    let view = real_backup_view(&directory, &device);
    assert!(
        view.flow().is_done(),
        "the genuine identity ceremony is complete"
    );
    assert!(
        !view.finished(),
        "first-use continuation still owns the desktop"
    );
    assert!(first_use(&view));
    assert!(
        !view.identity_setup_active(),
        "the actual certified identity may activate transport"
    );
    assert_eq!(std::fs::read(&minted.path).unwrap(), original);
    let person: [u8; 32] = hex::decode(&minted.fingerprint)
        .unwrap()
        .try_into()
        .unwrap();
    let certificate = paths.device_cert_for_user(&person);
    let held = certificate.with_extension("continuation-held");
    std::fs::rename(&certificate, &held).unwrap();
    assert!(
        view.identity_setup_active(),
        "missing certificate cannot activate transport"
    );
    assert!(!view.finished());
    assert!(first_use(&view));
    std::fs::rename(&held, &certificate).unwrap();
    assert!(!view.identity_setup_active());
    assert!(!view.finished());
    assert_eq!(std::fs::read(&minted.path).unwrap(), original);
}

/// Whether first use is still under way, as authentication prompts ask.
fn first_use(ceremony: &Ceremony) -> bool {
    let scope = ceremony.authentication_first_use_scope();
    scope()
}

/// Whether the record at `path` holds a root: a fixture keeps it in the account's own record,
/// where a daemon keeps it on a computer.
fn fixture_backup_capability(path: &Path) -> Result<bool, String> {
    tilo_identity::store::load_from_path(path.to_owned())
        .map(|identity| identity.is_single_device())
        .map_err(|error| error.to_string())
}

/// Setup over the identity in `directory`, reached as a restart of this device opens it, with
/// the real portable-backup export.
fn real_backup_view(directory: &Path, device: &Device) -> Ceremony {
    let identity = crate::identity::existing_identity_at(&directory.join("primary.json")).unwrap();
    let mut ceremony = Ceremony::with_all_handlers(
        Box::new(|| Err("unused minter".into())),
        Box::new(|_, _| Err("unused importer".into())),
        Box::new(|identity, destination, passphrase| {
            tilo_identity::store::export_portable_backup(identity, destination, passphrase)
                .map(|_| ())
                .map_err(|error| error.to_string())
        }),
    );
    ceremony.identity_dir = Some(directory.to_owned());
    device.runs(&mut ceremony);
    ceremony.backup_capability = Box::new(fixture_backup_capability);
    if progress::unfinished(&device.paths) {
        ceremony.restore_progress();
        ceremony.refresh_backup_capability().unwrap();
    } else {
        ceremony.flow.begin_existing();
        ceremony.accept_connected_identity(identity).unwrap();
    }
    // Identity work runs off the loop; settle it as a frame loop would before an agent acts.
    settle(&mut ceremony);
    ceremony
}

/// Save a backup through setup's API as the person does: type the new passphrase twice, ask for
/// a file, name where it goes.
fn save_backup(
    ceremony: &mut Ceremony,
    destination: &Path,
    passphrase: &str,
    confirmation: &str,
) -> bool {
    for (field, value) in [
        ("new-passphrase", passphrase),
        ("new-passphrase-confirmation", confirmation),
    ] {
        enter_secret(ceremony, field, value);
    }
    act(ceremony, OnboardingEffect::Browse);
    if ceremony
        .file_ask()
        .is_none_or(|asked| asked.question().mode != FilesMode::Save)
    {
        return false;
    }
    ceremony.files_answered(vec![destination.to_string_lossy().into_owned()]);
    true
}

#[test]
fn first_login_backup_semantics_save_verified_ciphertext_and_retire_at_continue() {
    let root = TempDir::new().unwrap();
    let directory = root.path().join("identity");
    tilo_identity::provisioning::create_primary_identity(
        &directory,
        "Backup fixture",
        [128; 3],
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let original = std::fs::read(directory.join("primary.json")).unwrap();
    let device = Device::new();
    let mut ceremony = real_backup_view(&directory, &device);
    assert!(
        first_use(&ceremony),
        "first-use service survives identity activation"
    );
    assert_eq!(ceremony.page().step(), Step::Backup);
    let backup = root.path().join("recovery.tilo-identity");
    let passphrase = "synthetic private backup passphrase";
    assert!(!save_backup(
        &mut ceremony,
        &backup,
        passphrase,
        "different"
    ));
    assert!(!backup.exists());
    assert!(
        ceremony
            .page()
            .backup_error
            .as_deref()
            .is_some_and(|error| error.contains("do not match")),
        "{:?}",
        ceremony.page().backup_error
    );
    let epoch = ceremony.page().epoch;
    assert!(save_backup(&mut ceremony, &backup, passphrase, passphrase));
    // The backup is written off the loop; take its report as a frame would.
    settle(&mut ceremony);
    let proof =
        tilo_identity::store::verify_portable_backup(&backup, passphrase.as_bytes()).unwrap();
    assert_eq!(
        proof.group_public_key,
        tilo_identity::store::load_from_path(directory.join("primary.json"))
            .unwrap()
            .group_public_key()
    );
    assert_eq!(device.step(), Some(Stage::IdentityReadyBackedUp));
    assert!(
        ceremony.page().epoch > epoch,
        "the passphrase fields start empty again"
    );
    let status = ceremony.backup_status().unwrap();
    assert_eq!(status["state"], "verified");
    assert!(!status.to_string().contains(passphrase));
    assert!(!status.to_string().contains(backup.to_str().unwrap()));
    for reading in [
        ceremony.page().reading(false),
        ceremony.page().reading(true),
    ] {
        assert!(
            !reading.to_string().contains(passphrase),
            "no reading carries the passphrase: {reading}"
        );
    }
    settled(&mut ceremony, OnboardingEffect::Continue);
    assert!(ceremony.page().identity_ready_page);
    assert!(first_use(&ceremony));
    assert_eq!(device.step(), Some(Stage::ReadyToEnterBackedUp));
    assert_eq!(
        std::fs::read(directory.join("primary.json")).unwrap(),
        original
    );
    let before = std::fs::read(&backup).unwrap();
    assert!(!save_backup(&mut ceremony, &backup, passphrase, passphrase));
    assert_eq!(std::fs::read(&backup).unwrap(), before);
    // Actual loaded recovery state preserves the verified backup after restart.
    let reopened = real_backup_view(&directory, &device);
    assert!(reopened.page().backup_was_verified);
    assert!(reopened.page().identity_ready_page);
}

#[test]
fn first_login_backup_semantic_deferral_requires_explicit_acknowledgement() {
    let root = TempDir::new().unwrap();
    let directory = root.path().join("identity");
    tilo_identity::provisioning::create_primary_identity(
        &directory,
        "Deferral fixture",
        [128; 3],
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let original = std::fs::read(directory.join("primary.json")).unwrap();
    let device = Device::new();
    let mut ceremony = real_backup_view(&directory, &device);
    act(&mut ceremony, OnboardingEffect::Continue);
    assert!(!ceremony.page().identity_ready_page);
    assert!(first_use(&ceremony));
    continue_without_backup(&mut ceremony);
    assert!(ceremony.page().identity_ready_page);
    assert!(!ceremony.page().backup_was_verified);
    assert!(first_use(&ceremony));
    assert_eq!(device.step(), Some(Stage::ReadyToEnter));
    assert_eq!(
        std::fs::read(directory.join("primary.json")).unwrap(),
        original
    );
}

fn write_private_member_fixture(directory: &Path, record: &[u8]) {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(directory)
        .unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(directory.join("primary.json"))
        .unwrap();
    file.write_all(record).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn first_login_footer_semantics_enter_the_desktop_and_complete_first_use() {
    let root = TempDir::new().unwrap();
    let directory = root.path().join("identity");
    let public = ed25519_dalek::SigningKey::from_bytes(&[19; 32])
        .verifying_key()
        .to_bytes();
    let record =
        tilo_identity::store::public_member_record("Tour person", [128; 3], public).unwrap();
    write_private_member_fixture(&directory, &record);
    let device = Device::new();
    let mut ceremony = real_backup_view(&directory, &device);
    assert!(ceremony.page().identity_ready_page);
    assert!(first_use(&ceremony));
    // The ready screen hands off to the desktop and nothing else. A first session learns the
    // desktop by using it; the overlay hints and the desktop tour reopen from Search when
    // someone asks for them.
    settled(&mut ceremony, OnboardingEffect::Finish);
    assert!(ceremony.finished());
    assert!(!first_use(&ceremony));
    assert_eq!(device.step(), None);
    assert_eq!(
        std::fs::read(directory.join("primary.json")).unwrap(),
        record
    );
    settled(&mut ceremony, OnboardingEffect::Finish);
    assert!(ceremony.finished());
    assert!(!first_use(&ceremony));
}

#[test]
fn first_login_codec_semantics_skip_to_ready_and_retire_only_at_finish() {
    let root = TempDir::new().unwrap();
    let directory = root.path().join("identity");
    tilo_identity::provisioning::create_primary_identity(
        &directory,
        "Codec person",
        [128; 3],
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let original = std::fs::read(directory.join("primary.json")).unwrap();
    let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
    let device = Device::new();
    let mut ceremony = real_backup_view(&directory, &device)
        .with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
    continue_without_backup(&mut ceremony);
    assert!(ceremony.page().codec_page);
    assert_eq!(ceremony.page().step(), Step::Codecs);
    assert!(first_use(&ceremony));
    act(&mut ceremony, OnboardingEffect::Skip);
    assert!(!ceremony.page().codec_page);
    assert_eq!(ceremony.page().step(), Step::Ready);
    assert!(fake.runs().is_empty());
    assert!(first_use(&ceremony));
    act(&mut ceremony, OnboardingEffect::choose("install-codecs"));
    assert!(
        fake.runs().is_empty(),
        "an install from a page no longer shown starts nothing"
    );
    settled(&mut ceremony, OnboardingEffect::Finish);
    assert!(ceremony.finished());
    assert!(!first_use(&ceremony));
    assert_eq!(device.step(), None);
    assert_eq!(
        std::fs::read(directory.join("primary.json")).unwrap(),
        original
    );
}

#[test]
fn first_login_codec_semantics_start_actual_worker_then_continue_and_finish() {
    let root = TempDir::new().unwrap();
    let directory = root.path().join("identity");
    tilo_identity::provisioning::create_primary_identity(
        &directory,
        "Codec worker person",
        [128; 3],
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let record = std::fs::read(directory.join("primary.json")).unwrap();
    let (release, check) = std::sync::mpsc::channel::<()>();
    let check = Mutex::new(check);
    let (started_tx, started) = std::sync::mpsc::channel();
    let started_tx = Mutex::new(started_tx);
    let setup = CodecSetup::with(
        Arc::new(|| CodecPackStatus::parse(r#"{"state":"not-installed"}"#)),
        Arc::new(move |action| {
            started_tx.lock().unwrap().send(action).unwrap();
            check.lock().unwrap().recv().unwrap();
            Ok(())
        }),
    );
    let device = Device::new();
    let mut ceremony = real_backup_view(&directory, &device).with_codec_setup(setup);
    continue_without_backup(&mut ceremony);
    assert!(ceremony.page().codec_page);
    act(&mut ceremony, OnboardingEffect::choose("install-codecs"));
    assert_eq!(
        tilo_test_env::recv("the codec install to start", &started),
        CodecAction::Install
    );
    assert!(ceremony.codec_running.is_some());
    act(&mut ceremony, OnboardingEffect::choose("install-codecs"));
    assert!(
        started.try_recv().is_err(),
        "a running install is not started again"
    );
    act(&mut ceremony, OnboardingEffect::Continue);
    assert!(!ceremony.page().codec_page);
    assert!(first_use(&ceremony));
    release.send(()).unwrap();
    settled(&mut ceremony, OnboardingEffect::Finish);
    assert!(ceremony.finished());
    assert!(!first_use(&ceremony));
    assert_eq!(
        std::fs::read(directory.join("primary.json")).unwrap(),
        record
    );
}

tilo_test_env::scenario! {
    fn a_public_member_identity_skips_the_backup_step_and_keeps_its_record;

    phase keyring_retry_public_member_uses_actual_acquisition_reducer_and_skips_backup;
    {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("identity");
        let public = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
            .verifying_key()
            .to_bytes();
        let record =
            tilo_identity::store::public_member_record("Existing person", [128; 3], public)
                .unwrap();
        let path = directory.join("primary.json");
        write_private_member_fixture(&directory, &record);
        let mut ceremony = Ceremony::with_all_handlers(
            Box::new(move || crate::identity::existing_identity_at(&path)),
            Box::new(|_, _| Err("unused importer".into())),
            Box::new(|_, _, _| panic!("a public member must never export a personal backup")),
        );
        ceremony.identity_dir = Some(directory.clone());
        let device = Device::new();
        device.runs(&mut ceremony);
        ceremony.backup_capability = Box::new(fixture_backup_capability);
        ceremony.recovery_only = true;
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        assert_eq!(ceremony.page().screen, Screen::Existing);
        settled(&mut ceremony, OnboardingEffect::Retry);
        assert!(ceremony.flow().is_done());
        assert!(ceremony.page().identity_ready_page);
        assert_eq!(ceremony.page().step(), Step::Ready);
        assert!(!ceremony.page().backup_export_available);
        assert!(!ceremony.page().backup_was_verified);
        assert!(ceremony.page().error.is_none());
        assert_eq!(device.step(), Some(Stage::ReadyToEnter));
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            record
        );
    }

    phase first_login_public_member_skips_backup_without_reminting_or_losing_progress;
    {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("identity");
        let public = ed25519_dalek::SigningKey::from_bytes(&[7; 32])
            .verifying_key()
            .to_bytes();
        let record =
            tilo_identity::store::public_member_record("Paired person", [128; 3], public)
                .unwrap();
        write_private_member_fixture(&directory, &record);
        let device = Device::new();
        let view = real_backup_view(&directory, &device);
        assert!(!view.page().backup_export_available);
        assert!(
            view.page().identity_ready_page,
            "actual connected acquisition skips the irrelevant backup step"
        );
        assert!(!view.page().backup_was_verified);
        assert!(first_use(&view));
        assert!(view.backup_status().is_none());
        assert_eq!(device.step(), Some(Stage::ReadyToEnter));
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            record
        );
        let mut reopened = real_backup_view(&directory, &device);
        reopened.restore_progress();
        reopened.refresh_backup_capability().unwrap();
        assert!(reopened.page().identity_ready_page);
        assert!(!reopened.page().backup_was_verified);
        // Existing tutorial progress survives the same canonical capability refresh.
        reopened.tour_page = Some(TourPage::Organise);
        reopened.refresh_backup_capability().unwrap();
        assert_eq!(reopened.page().tour_page, Some(TourPage::Organise));
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            record
        );
    }
}

tilo_test_env::scenario! {
    fn portable_backup_verifies_passphrases_and_returns_to_the_desktop;

    phase portable_backup_requires_matching_strong_passphrases_and_reports_success;
    {
        let progress = TempDir::new().expect("backup progress directory");
        let exports = Arc::new(AtomicUsize::new(0));
        let exports_for_handler = Arc::clone(&exports);
        let mut ceremony = Ceremony::with_all_handlers(
            Box::new(|| {
                Ok(MintedIdentity {
                    fingerprint: "ef".repeat(32),
                    path: PathBuf::from("/identity/primary.json"),
                })
            }),
            Box::new(|_, _| Err("unused".to_string())),
            Box::new(move |identity, destination, passphrase| {
                assert_eq!(identity, &PathBuf::from("/identity/primary.json"));
                assert_eq!(
                    destination.file_name().and_then(|name| name.to_str()),
                    Some("tilo-identity-backup.tilo-identity")
                );
                assert_eq!(passphrase, b"correct horse battery staple");
                exports_for_handler.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
        );
        finish_backup_step(&mut ceremony);
        ceremony.identity_dir = Some(progress.path().to_path_buf());
        let device = Device::new();
        device.runs(&mut ceremony);

        enter_secret(
            &mut ceremony,
            "new-passphrase",
            "correct horse battery staple",
        );
        enter_secret(
            &mut ceremony,
            "new-passphrase-confirmation",
            "does not match",
        );
        act(&mut ceremony, OnboardingEffect::Browse);
        assert!(ceremony.file_ask().is_none());
        assert!(
            ceremony
                .page()
                .backup_error
                .as_deref()
                .unwrap()
                .contains("do not match")
        );

        enter_secret(
            &mut ceremony,
            "new-passphrase-confirmation",
            "correct horse battery staple",
        );
        act(&mut ceremony, OnboardingEffect::Browse);
        ceremony.files_answered(Vec::new());
        settle(&mut ceremony);
        assert!(ceremony.file_ask().is_none());
        assert_eq!(exports.load(Ordering::SeqCst), 0);

        act(&mut ceremony, OnboardingEffect::Browse);
        ceremony.files_answered(vec![
            progress
                .path()
                .join("tilo-identity-backup.tilo-identity")
                .to_string_lossy()
                .into_owned(),
        ]);
        settle(&mut ceremony);
        assert!(ceremony.file_ask().is_none());
        assert_eq!(exports.load(Ordering::SeqCst), 1);
        assert!(ceremony.page().backup_saved.is_some());
        assert!(ceremony.page().backup_error.is_none());
        assert_eq!(device.step(), Some(Stage::IdentityReadyBackedUp));
        assert!(ceremony.backup_passphrase.lock().unwrap().is_empty());
        assert!(ceremony.backup_confirmation.lock().unwrap().is_empty());

        settled(&mut ceremony, OnboardingEffect::Continue);
        assert!(ceremony.page().identity_ready_page);
        assert_eq!(device.step(), Some(Stage::ReadyToEnterBackedUp));
    }

    phase existing_identity_backup_entry_reuses_the_verified_backup_screen;
    {
        let mut ceremony = Ceremony::with_all_handlers(
            Box::new(|| Err("unused".to_string())),
            Box::new(|_, _| Err("unused".to_string())),
            Box::new(|_, _, _| Ok(())),
        );

        ceremony.begin_portable_backup_for(MintedIdentity {
            fingerprint: "ab".repeat(32),
            path: PathBuf::from("/identity/primary.json"),
        });

        let page = ceremony.page();
        assert_eq!(page.screen, Screen::Done);
        assert!(!page.identity_ready_page);
        assert!(!page.backup_was_verified);
        assert!(!ceremony.finished());
        assert_eq!(
            page.step(),
            Step::Backup,
            "the existing identity route reaches the same backup step"
        );
        assert!(page.backup_export_available);
        assert_eq!(
            ceremony.backup_status().map(|status| status["state"].clone()),
            Some(serde_json::json!("ready"))
        );
    }

    phase verified_existing_identity_backup_returns_directly_to_desktop;
    {
        let progress = TempDir::new().expect("backup progress directory");
        let mut ceremony = Ceremony::with_all_handlers(
            Box::new(|| Err("unused".to_string())),
            Box::new(|_, _| Err("unused".to_string())),
            Box::new(|_, _, _| Ok(())),
        );
        ceremony.begin_portable_backup_for(MintedIdentity {
            fingerprint: "cd".repeat(32),
            path: PathBuf::from("/identity/primary.json"),
        });
        ceremony.identity_dir = Some(progress.path().to_path_buf());
        let device = Device::new();
        device.runs(&mut ceremony);
        ceremony.backup_was_verified = true;
        device.reached(Stage::IdentityReadyBackedUp);

        settled(&mut ceremony, OnboardingEffect::Continue);

        assert!(ceremony.finished());
        assert!(!ceremony.page().identity_ready_page);
        assert!(ceremony.page().tour_page.is_none());
        assert!(
            !progress::unfinished(&device.paths),
            "leaving Settings after verification must not resurrect first-login setup"
        );
    }

    phase verified_backup_reveals_copy_form_only_after_the_optional_action;
    {
        let (mut ceremony, _) = counted();
        finish_backup_step(&mut ceremony);
        assert!(!ceremony.page().backup_copy_form_open);
        assert!(
            !ceremony.page().backup_was_verified,
            "the copy form waits for the verified state as well as the action"
        );

        // A verified backup, as an export that read back leaves it.
        ceremony.backup_was_verified = true;
        act(&mut ceremony, OnboardingEffect::choose("another-copy"));

        assert!(
            ceremony.page().backup_copy_form_open,
            "the optional copy action, rather than the verified state itself, reveals passphrase inputs"
        );
    }
}

/// A fake helper: `status` is the status file, and the runner rewrites it the way the unit would
/// before returning `outcome`.
struct FakeCodecs {
    status: Arc<Mutex<String>>,
    runs: Arc<Mutex<Vec<CodecAction>>>,
}

impl FakeCodecs {
    fn new(initial: &str) -> Self {
        Self {
            status: Arc::new(Mutex::new(initial.to_string())),
            runs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn setup(&self, after: &'static str, outcome: Result<(), String>) -> CodecSetup {
        let reader = Arc::clone(&self.status);
        let writer = Arc::clone(&self.status);
        let runs = Arc::clone(&self.runs);
        CodecSetup::with(
            Arc::new(move || CodecPackStatus::parse(&reader.lock().unwrap())),
            Arc::new(move |action| {
                runs.lock().unwrap().push(action);
                *writer.lock().unwrap() = after.to_string();
                outcome.clone()
            }),
        )
    }

    fn runs(&self) -> Vec<CodecAction> {
        self.runs.lock().unwrap().clone()
    }
}

/// Continue from the intro to the media formats page, skipping the backup.
fn walk_to_codecs(ceremony: &mut Ceremony) {
    for effect in [
        OnboardingEffect::Continue,
        OnboardingEffect::Acknowledge { checked: true },
        OnboardingEffect::Continue,
        OnboardingEffect::Acknowledge { checked: true },
        OnboardingEffect::Continue,
    ] {
        settled(ceremony, effect);
    }
}

tilo_test_env::scenario! {
    fn the_codecs_step_installs_skips_and_reports_what_the_person_chose;

    phase the_codecs_step_offers_an_install_and_nothing_is_chosen;
    {
        let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
        let (ceremony, _) = counted();
        let mut ceremony = ceremony.with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
        walk_to_codecs(&mut ceremony);
        assert!(ceremony.page().codec_page);
        assert_eq!(ceremony.page().step(), Step::Codecs);
        assert_eq!(ceremony.page().total_steps, 5);
        let page = ceremony.page();
        assert_eq!(page.reading(false)["codecs_state"], "not-installed");
        assert!(!page.codecs.running);
        assert!(page.codecs.error.is_none());
        assert!(fake.runs().is_empty(), "reaching the step installs nothing");
        // The step blocks neither the tour nor finishing only by accident: both wait until the
        // person chose.
        act(&mut ceremony, OnboardingEffect::Finish);
        assert!(!ceremony.finished());
        assert!(ceremony.page().codec_page);
    }

    phase installing_from_setup_records_the_choice_and_shows_the_result;
    {
        let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
        let (ceremony, _) = counted();
        let mut ceremony = ceremony.with_codec_setup(fake.setup(
            r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"enabled"},"desired":true}"#,
            Ok(()),
        ));
        walk_to_codecs(&mut ceremony);
        act(&mut ceremony, OnboardingEffect::choose("install-codecs"));
        ceremony.wait_for_codec_install();
        ceremony.step();
        assert_eq!(fake.runs(), vec![CodecAction::Install]);
        assert!(
            ceremony.page().codec_page,
            "Install never moves past the page by itself"
        );
        assert_eq!(ceremony.page().codecs.state, CodecsState::Installed);
        assert_eq!(ceremony.page().reading(false)["codecs_state"], "installed");
        assert!(
            ceremony
                .page()
                .codecs
                .when_it_applies
                .as_deref()
                .is_some_and(|when| when.contains("Sign out and back in")),
            "an installed pack says when it takes effect"
        );
        act(&mut ceremony, OnboardingEffect::Continue);
        assert!(!ceremony.page().codec_page);
        assert_eq!(ceremony.page().step(), Step::Ready);
        assert_eq!(ceremony.page().total_steps, 5);
        act(&mut ceremony, OnboardingEffect::Finish);
        assert!(ceremony.finished());
    }

    phase not_now_installs_nothing_and_continues;
    {
        let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
        let (ceremony, _) = counted();
        let mut ceremony = ceremony.with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
        walk_to_codecs(&mut ceremony);
        act(&mut ceremony, OnboardingEffect::Skip);
        assert!(!ceremony.page().codec_page);
        assert!(fake.runs().is_empty());
        act(&mut ceremony, OnboardingEffect::Finish);
        assert!(ceremony.finished());
    }

    phase setup_continues_while_the_download_runs;
    {
        let (release, check) = std::sync::mpsc::channel::<()>();
        let check = Mutex::new(check);
        let (started_tx, started) = std::sync::mpsc::channel::<()>();
        let started_tx = Mutex::new(started_tx);
        let status = Arc::new(Mutex::new(
            r#"{"state":"not-installed"}"#.to_string(),
        ));
        let reader = Arc::clone(&status);
        let writer = Arc::clone(&status);
        let (ceremony, _) = counted();
        let mut ceremony = ceremony.with_codec_setup(CodecSetup::with(
            Arc::new(move || CodecPackStatus::parse(&reader.lock().unwrap())),
            Arc::new(move |_| {
                *writer.lock().unwrap() = r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":7,"label":"Downloading"}}"#.to_string();
                let _ = started_tx.lock().unwrap().send(());
                let _ = check.lock().unwrap().recv();
                Ok(())
            }),
        ));
        walk_to_codecs(&mut ceremony);
        act(&mut ceremony, OnboardingEffect::choose("install-codecs"));
        // Nothing wakes the compositor for the worker, so the ceremony asks to be looked at.
        let limit = Instant::now()
            .checked_add(BACKGROUND_POLL)
            .unwrap();
        assert!(
            ceremony
                .next_deadline()
                .is_some_and(|deadline| deadline <= limit),
            "a running install keeps looking for its result"
        );
        // The helper reports progress through its status file.
        started.recv().unwrap();
        ceremony.codec_changed.store(true, Ordering::Release);
        assert!(ceremony.needs_step(), "a status change asks for a frame");
        ceremony.step();
        assert_eq!(ceremony.page().codecs.state, CodecsState::Installing);
        assert_eq!(
            ceremony.page().reading(false)["codecs_state"],
            "installing",
            "the page shows what the helper reports"
        );
        let fraction = ceremony
            .page()
            .codecs
            .fraction
            .expect("how far the install got");
        assert!(
            (fraction - 2.0 / 7.0).abs() < 1e-6,
            "step 3 of 7 is where the bar stands: {fraction}"
        );
        assert!(
            ceremony.page().codecs.summary.contains("Downloading"),
            "{:?}",
            ceremony.page().codecs.summary
        );
        assert_eq!(ceremony.page().step(), Step::Codecs);
        act(&mut ceremony, OnboardingEffect::Continue);
        assert!(
            !ceremony.page().codec_page,
            "the download does not hold setup"
        );
        act(&mut ceremony, OnboardingEffect::Finish);
        assert!(ceremony.finished());
        release.send(()).unwrap();
        assert!(!ceremony.page().codec_page);
    }

    phase an_offline_choice_is_kept_and_installs_later;
    {
        let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
        let (ceremony, _) = counted();
        let mut ceremony = ceremony.with_codec_setup(
            fake.setup(r#"{"state":"waiting-for-network","desired":true}"#, Ok(())),
        );
        walk_to_codecs(&mut ceremony);
        act(&mut ceremony, OnboardingEffect::choose("install-codecs"));
        ceremony.wait_for_codec_install();
        ceremony.step();
        assert_eq!(
            ceremony.page().codecs.state,
            CodecsState::WaitingForNetwork
        );
        assert_eq!(
            ceremony.page().reading(false)["codecs_state"],
            "waiting-for-network"
        );
        assert!(ceremony.page().codec_page);
    }

    phase a_refused_install_is_shown_inline_with_retry_and_skip;
    {
        let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
        let (ceremony, _) = counted();
        let mut ceremony = ceremony.with_codec_setup(fake.setup(
            r#"{"state":"not-installed"}"#,
            Err("Access denied: not an administrator".into()),
        ));
        walk_to_codecs(&mut ceremony);
        act(&mut ceremony, OnboardingEffect::choose("install-codecs"));
        ceremony.wait_for_codec_install();
        ceremony.step();
        assert_eq!(
            ceremony.page().codecs.error.as_deref(),
            Some("Access denied: not an administrator")
        );
        assert!(ceremony.page().codec_page);
        // Try again installs again, and leaves the page where it is.
        act(&mut ceremony, OnboardingEffect::Retry);
        ceremony.wait_for_codec_install();
        assert_eq!(
            fake.runs(),
            vec![CodecAction::Install, CodecAction::Install]
        );
    }

    phase a_choice_made_in_the_installer_is_not_asked_again;
    {
        let fake = FakeCodecs::new(r#"{"state":"waiting-for-network","desired":true}"#);
        let (ceremony, _) = counted();
        let mut ceremony =
            ceremony.with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
        walk_to_codecs(&mut ceremony);
        assert!(!ceremony.page().codec_page);
        assert_eq!(ceremony.page().step(), Step::Ready);
        assert_eq!(ceremony.page().total_steps, 4);
    }
}

tilo_test_env::scenario! {
    fn a_recovery_key_is_checked_as_typed_and_as_written_down;

    phase a_recovery_key_is_checked_while_typed_and_restores_only_when_valid;
    {
        let seed = [0x3f_u8; 32];
        let words = tilo_user_identity::recovery_words::encode(&seed)
            .expect("32 bytes encode")
            .to_string();
        let restored = Arc::new(Mutex::new(Vec::<String>::new()));
        let received = Arc::clone(&restored);
        let mut ceremony =
            Ceremony::with_minter(Box::new(|| Err("create should not run".to_string())));
        ceremony.words_importer = Arc::new(Mutex::new(Box::new(move |words: &str| {
            received.lock().expect("record").push(words.to_owned());
            Ok(MintedIdentity {
                fingerprint: "cd".repeat(32),
                path: PathBuf::from("/home/user/.config/tilo/identity/cd.json"),
            })
        })));
        act(&mut ceremony, OnboardingEffect::choose("existing"));
        act(&mut ceremony, OnboardingEffect::choose("recovery-key"));
        assert_eq!(ceremony.page().screen, Screen::RecoveryKey);
        assert_eq!(ceremony.page().reading(false)["words_check"], "empty");

        // A word still being typed is not flagged; a finished unknown one is.
        enter_secret(&mut ceremony, "recovery-key", "1. abandon 2. abil");
        assert_eq!(
            ceremony.page().recovery_check,
            WordsCheck::Typing { words: 2 }
        );
        assert_eq!(ceremony.page().reading(false)["words_typed"], 2);
        enter_secret(&mut ceremony, "recovery-key", "abandon abil ");
        let WordsCheck::Problem(problem) = ceremony.page().recovery_check else {
            panic!(
                "a finished unknown word is a problem: {:?}",
                ceremony.page().recovery_check
            );
        };
        assert!(
            problem.contains("abil") && problem.contains("ability"),
            "names the word as typed and the word probably meant: {problem}"
        );
        // The page says where in what was typed the word stands.
        let typed = ceremony.recovery_words.lock().unwrap().clone();
        assert!(
            ceremony.page().recovery_word_mistakes.iter().any(|range| {
                typed.get(range.clone()) == Some("abil")
            }),
            "the mistake is marked where it was typed: {:?}",
            ceremony.page().recovery_word_mistakes
        );

        // Restore is refused until the words are a valid key.
        act(&mut ceremony, OnboardingEffect::Continue);
        assert!(restored.lock().expect("record").is_empty());

        // A valid key for another identity than this computer's is refused.
        ceremony.expected_identity = Some([0x11; 32]);
        enter_secret(&mut ceremony, "recovery-key", &words.to_uppercase());
        assert!(matches!(
            ceremony.page().recovery_check,
            WordsCheck::Different { .. }
        ));
        act(&mut ceremony, OnboardingEffect::Continue);
        assert!(restored.lock().expect("record").is_empty());

        ceremony.expected_identity = None;
        enter_secret(&mut ceremony, "recovery-key", &format!("{words}\n"));
        assert!(matches!(
            ceremony.page().recovery_check,
            WordsCheck::Ready { .. }
        ));
        let epoch = ceremony.page().epoch;
        act(&mut ceremony, OnboardingEffect::Continue);
        assert_eq!(restored.lock().expect("record").len(), 1);
        assert!(ceremony.flow().is_done());
        assert!(
            ceremony.recovery_words.lock().expect("words").is_empty(),
            "the typed words are wiped once used"
        );
        assert_eq!(
            ceremony.page().epoch,
            epoch + 1,
            "the field starts empty again"
        );
        assert_eq!(ceremony.page().recovery_check, WordsCheck::Empty);
        assert_eq!(ceremony.page().reading(false)["words_check"], "empty");
    }

    phase writing_down_the_recovery_key_counts_as_a_verified_backup;
    {
        let (mut ceremony, _) = counted();
        finish_backup_step(&mut ceremony);
        // The words the daemon hands the backup step, as its worker reports them.
        show_recovery_key(&mut ceremony, recovery_words([0x3f; 32]));
        assert_eq!(ceremony.page().step(), Step::RecoveryKeyCopy);
        let reading = ceremony.page().reading(true);
        assert_eq!(reading["key_shown"], true);
        assert_eq!(reading["key_words"].as_array().map(Vec::len), Some(24));
        assert_eq!(reading["key_checks"].as_array().map(Vec::len), Some(3));
        assert!(
            ceremony.page().reading(false)["key_words"]
                .as_array()
                .is_some_and(Vec::is_empty),
            "the key's words never reach the reading an API caller reads"
        );

        // Two words are still missing, so the copy is not confirmed.
        act(&mut ceremony, OnboardingEffect::Continue);
        assert!(ceremony.page().recovery_key_copy.is_some());

        // A slip in one answer shows before the words are all there.
        enter_secret(&mut ceremony, "recovery-key-word-2", "zoo");
        assert_eq!(
            ceremony.page().reading(false)["key_wrong"],
            serde_json::json!([false, true, false]),
            "the word that can no longer be right is named"
        );

        let copy = ceremony.page().recovery_key_copy.expect("copy");
        for (at, position) in copy.checks.iter().enumerate() {
            let word = copy.word(*position).to_uppercase();
            enter_secret(
                &mut ceremony,
                &format!("recovery-key-word-{}", at.saturating_add(1)),
                &format!(" {word} "),
            );
        }
        assert!(ceremony.recovery_key_copy_confirmed);
        assert_eq!(ceremony.page().reading(false)["key_confirmed"], true);
        assert_eq!(
            ceremony.page().reading(false)["key_wrong"],
            serde_json::json!([false, false, false])
        );

        let epoch = ceremony.page().epoch;
        act(&mut ceremony, OnboardingEffect::Continue);
        assert!(
            ceremony.page().recovery_key_copy.is_none(),
            "the words leave the screen"
        );
        assert!(ceremony.page().recovery_key_written);
        assert!(ceremony.page().backup_was_verified);
        assert_eq!(ceremony.page().step(), Step::Backup);
        assert_eq!(ceremony.page().reading(false)["key_written"], true);
        assert_eq!(ceremony.page().reading(false)["backup_verified"], true);
        assert!(
            ceremony.page().epoch > epoch,
            "the typed-back fields start empty again"
        );
        for answer in &ceremony.key_answers {
            assert!(answer.lock().unwrap().is_empty());
        }
    }

    phase recovery_key_checks_take_one_word_from_each_third;
    {
        let (ceremony, _) = counted();
        for _ in 0..64 {
            let copy = RecoveryKeyCopy::new(
                recovery_words([0x3f; 32]),
                ceremony.key_answers.clone(),
                Arc::clone(&ceremony.epoch),
            );
            let checks = copy.checks;
            assert!((1..=8).contains(&checks[0]), "{checks:?}");
            assert!((9..=16).contains(&checks[1]), "{checks:?}");
            assert!((17..=24).contains(&checks[2]), "{checks:?}");
        }
    }
}

/// Whoever acts for the person (an agent, a script) drives setup with the same actions, except a
/// secret: typing one needs the person there, so the system asks them first, and what was typed
/// is never printed or read back. Where a backup goes is the person's answer in the file picker,
/// which no action stands in for.
#[test]
fn only_the_person_hands_setup_a_secret_and_it_is_never_printed() {
    use tilo_resource::Effect as _;
    for declaration in OnboardingEffect::declarations() {
        assert_eq!(
            declaration.attended,
            declaration.id == "enter-secret",
            "{}",
            declaration.id
        );
    }
    let secret = OnboardingEffect::EnterSecret {
        field: "backup-passphrase".to_owned(),
        value: "hunter2hunter2".to_owned(),
    };
    assert!(!format!("{secret:?}").contains("hunter2"));
    let (mut ceremony, _) = counted();
    act(&mut ceremony, OnboardingEffect::choose("existing"));
    act(&mut ceremony, OnboardingEffect::choose("backup-file"));
    act(&mut ceremony, secret);
    let page = ceremony.page();
    let shown = format!("{page:?} {} {}", page.reading(false), page.reading(true));
    assert!(!shown.contains("hunter2"), "{shown}");
}
