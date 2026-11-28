//! The first-login identity ceremony the session runs, which the setup app's view
//! (`apps/tl/onboarding`) shows.
//!
//! The session keeps the ceremony: the decisions ([`crate::identity::Onboarding`]), the identity
//! work, how far setup got, adding this computer with a code, the media codecs install and every
//! secret's ownership. It publishes setup as the resource [`RESOURCE`]: the page the view reads
//! ([`Page::reading`]) and the actions it takes ([`OnboardingEffect`]), the same actions a person
//! searching, a script or an agent here drive setup with. Secrets are typed into buffers the
//! ceremony owns and zeroizes ([`OnboardingEffect::EnterSecret`]); no reading carries them back.
//! A step that needs a file from the person has the session ask for it, as the system does: the
//! view names the file it works with and never draws a picker ([`file_ask`]).
//!
//! Setup stays trusted chrome: the session draws its view in its own process, above every
//! application surface, before the desktop exists. No application can draw over it, read it or
//! answer for it.
//!
//! On a Tilo system that has not opted in yet, setup also offers to play more video and audio
//! formats after the backup step ([`CodecSetup`]): a page with one large Install button that
//! downloads the media codecs extension only when pressed; nothing is chosen in advance. It runs
//! through the same polkit-allowed unit as Settings and never blocks setup: the download
//! continues in the background and the result arrives as a notification.

mod asks;
mod codecs;
mod effect;
pub mod file_ask;
mod page;
mod progress;
mod recovery_key;
mod session;
#[cfg(test)]
mod tests;
pub use effect::{CHOICES, FIELDS, OnboardingEffect, SECRET_FIELDS};
pub use page::{
    Codecs, IdentityProblem as PageProblem, JoinedDevice, KeyCopy, Nearby, OnboardingContext,
    PackState as CodecsState, Page, Screen as PageScreen, SecretText, Step, TourPage, WordsCheck,
};
pub use progress::SECTION as PROGRESS_SECTION;
use progress::Stage;
use recovery_key::{RecoveryKeyCopy, import_recovery_words};
use session::SessionPaths;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{cell::RefCell, rc::Rc};
use zeroize::{Zeroize, Zeroizing};

use crate::identity::{MintedIdentity, Onboarding, Screen};
use crate::{t, t_args};
use file_ask::AskPurpose;
pub use file_ask::FileAsk;
use os_actions::codec_pack::{CodecAction, CodecPackStatus};

/// The resource setup is published as, which the setup app's view reads as `session.setup`.
pub const RESOURCE: &str = "setup";

/// Its kind, which a package names as the type `session.Setup`.
pub const KIND: &str = "setup";

/// The setup app, whose view draws the ceremony.
pub const APP: &str = "so.tilo.onboarding";

/// The view of [`APP`] that draws it, over the reading of [`RESOURCE`].
pub const VIEW: &str = "setup";

/// Setup as the session publishes it: the ceremony's page, and what moves it on.
#[derive(tilo_resource::Row)]
#[row(kind = "setup", label = "Setup")]
pub struct SetupRow;

/// The resource setup is published as: its actions, each the press of the control that shows it.
#[must_use]
pub fn resource() -> tilo_actions::ResourceSpec {
    let mut resource = tilo_resource::offer::<OnboardingEffect>(
        tilo_resource::LiveRow::typed::<SetupRow>(RESOURCE, t("onboarding-resource"))
            .into_resource(),
    );
    // Setup's actions are its app's, wherever the session draws it.
    resource.app_id = Some(APP.to_owned());
    resource
}

/// How often setup looks for news from its background work (codec install,
/// a code being redeemed) while that work runs.
const BACKGROUND_POLL: Duration = Duration::from_millis(250);

/// Whether this device's setup is under way and unfinished, as the person's settings hold it
/// ([`progress`]): a starting session opens setup again at the step it reached. A device that
/// holds a key nobody can read has a step nobody can name, and its setup opens again at the
/// earliest step an identity has.
#[must_use]
pub fn setup_unfinished() -> bool {
    progress::unfinished(&crate::firstboot::collaboration_identity_paths())
}

/// Record that this device's setup reached its backup step: what startup writes after this
/// computer's daemon made the account's person here, so a computer set up for one person still
/// offers the recovery key and the tour rather than opening straight into the desktop.
///
/// # Errors
/// This device's key or the person's settings could not be read or written.
pub fn record_backup_step() -> Result<(), String> {
    progress::record(
        &crate::firstboot::collaboration_identity_paths(),
        Some(Stage::IdentityReady),
    )
    .map_err(|error| error.to_string())
}

pub const KEY_BEGIN: &str = "onboarding.begin";
pub const KEY_EXISTING: &str = "onboarding.existing";
pub const KEY_CHOOSE_NEARBY: &str = "onboarding.existing.nearby";
pub const KEY_CHOOSE_RESTORE: &str = "onboarding.existing.restore";
pub const KEY_CHOOSE_RECOVERY_KEY: &str = "onboarding.existing.recovery-key";
pub const KEY_RECOVERY_KEY_RESTORE: &str = "onboarding.recovery-key.restore";
pub const KEY_RECOVERY_KEY_SHOW: &str = "onboarding.backup.recovery-key";
pub const KEY_RECOVERY_KEY_CONFIRM: &str = "onboarding.backup.recovery-key.confirm";
pub const KEY_RECOVERY_KEY_CLOSE: &str = "onboarding.backup.recovery-key.close";
pub const KEY_BACK: &str = "onboarding.back";
pub const KEY_CONNECT_BROWSE: &str = "onboarding.connect.browse";
pub const KEY_IMPORT: &str = "onboarding.import";
pub const KEY_CONNECT_NEARBY: &str = "onboarding.connect.nearby";
pub const KEY_CONNECT_CANCEL: &str = "onboarding.connect.cancel";
pub const KEY_UNDERSTOOD: &str = "onboarding.understood";
pub const KEY_CREATE: &str = "onboarding.create";
pub const KEY_BACKUP_BROWSE: &str = "onboarding.backup.browse";
pub const KEY_BACKUP_ANOTHER: &str = "onboarding.backup.another";
pub const KEY_BACKUP_CONTINUE: &str = "onboarding.backup.continue";
pub const KEY_CONNECT_RETRY: &str = "onboarding.connect.retry";
pub const KEY_BACKUP_SKIP_ACKNOWLEDGED: &str = "onboarding.backup.skip-acknowledged";
pub const KEY_TOUR_START: &str = "onboarding.tour.start";
pub const KEY_TOUR_BACK: &str = "onboarding.tour.back";
pub const KEY_TOUR_NEXT: &str = "onboarding.tour.next";
pub const KEY_TOUR_SKIP: &str = "onboarding.tour.skip";
pub const KEY_FINISH: &str = "onboarding.finish";
pub const KEY_CODECS_INSTALL: &str = "onboarding.codecs.install";
pub const KEY_CODECS_SKIP: &str = "onboarding.codecs.skip";
pub const KEY_CODECS_CONTINUE: &str = "onboarding.codecs.continue";

/// The media codecs step's connection to the system: the helper's status and
/// the polkit-allowed unit runner ([`os_actions::codec_pack`]). Tests and the
/// review renderer supply fakes.
pub struct CodecSetup {
    status: Arc<dyn Fn() -> CodecPackStatus + Send + Sync>,
    runner: Arc<dyn Fn(CodecAction) -> Result<(), String> + Send + Sync>,
    /// Follow the real status file (production only).
    watch_system: bool,
}

impl CodecSetup {
    /// The pack on this machine, when it can be installed here.
    #[must_use]
    pub fn system() -> Option<Self> {
        if cfg!(test) || !os_actions::codec_pack::available() {
            return None;
        }
        Some(Self {
            status: Arc::new(CodecPackStatus::read),
            runner: Arc::new(os_actions::codec_pack::run),
            watch_system: true,
        })
    }

    /// Injected status and runner.
    #[must_use]
    pub fn with(
        status: Arc<dyn Fn() -> CodecPackStatus + Send + Sync>,
        runner: Arc<dyn Fn(CodecAction) -> Result<(), String> + Send + Sync>,
    ) -> Self {
        Self {
            status,
            runner,
            watch_system: false,
        }
    }
}

type Minter = Box<dyn Fn() -> Result<MintedIdentity, String> + Send>;
/// Have this computer's daemon restore the person from the backup at `source`, sealed with
/// `passphrase`, and take them up as this account's identity in `import_dir`.
fn import_identity(
    import_dir: &Path,
    source: &Path,
    passphrase: &[u8],
) -> Result<MintedIdentity, String> {
    let backup = std::fs::read(source).map_err(|error| error.to_string())?;
    let passphrase = std::str::from_utf8(passphrase).map_err(|error| error.to_string())?;
    crate::identity::restore_by_daemon(
        import_dir,
        serde_json::json!({ "backup": hex::encode(backup), "passphrase": passphrase }),
    )
    .map_err(|error| error.to_string())
}

/// Write a backup of the person's root, which this computer's daemon keeps, sealed with
/// `passphrase`, to `destination`, a new file, and prove it opens.
fn export_backup(destination: &Path, passphrase: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let passphrase_text = std::str::from_utf8(passphrase).map_err(|error| error.to_string())?;
    let answer = tilo_api::daemon::this_computer()?.act(
        tilo_device_identity::login::BACKUP,
        serde_json::json!({ "passphrase": passphrase_text }),
    )?;
    let backup = answer
        .get("backup")
        .and_then(serde_json::Value::as_str)
        .and_then(|backup| hex::decode(backup).ok())
        .ok_or("this computer's daemon answered no backup")?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .map_err(|error| error.to_string())?;
    file.write_all(&backup)
        .and_then(|()| file.sync_all())
        .map_err(|error| error.to_string())?;
    tilo_identity::store::verify_portable_backup(destination, passphrase)
        .map(drop)
        .map_err(|error| error.to_string())
}

type Importer = Box<dyn Fn(&PathBuf, &[u8]) -> Result<MintedIdentity, String> + Send>;
/// Restores an identity from the words of a recovery key.
type WordsImporter = Box<dyn Fn(&str) -> Result<MintedIdentity, String> + Send>;

type BackupCapability = Box<dyn Fn(&Path) -> Result<bool, String> + Send>;
/// Whether this computer's daemon keeps the person's root, which a backup is made of.
fn daemon_backup_capability(_: &Path) -> Result<bool, String> {
    tilo_api::daemon::who::who().map(|who| who.holds_root)
}
type Exporter = Box<dyn Fn(&PathBuf, &PathBuf, &[u8]) -> Result<(), String> + Send>;

/// An identity handler, called on the identity worker's thread.
type Shared<T> = Arc<Mutex<T>>;

/// The outcome of identity work, which says what the work was.
enum IdentityDone {
    Mint(Result<MintedIdentity, String>),
    RetryMint(Result<MintedIdentity, String>),
    Import(Result<MintedIdentity, String>),
    /// The destination the backup was written to.
    Export(Result<PathBuf, String>),
    /// The words of this identity's recovery key, to write down.
    RecoveryWords(Result<Zeroizing<String>, String>),
}

/// Identity work running off the compositor's loop.
///
/// Minting, importing and exporting the identity generate or unwrap keys,
/// talk to the secret store, stretch a passphrase on purpose and fsync the
/// records they write: seconds under I/O pressure, at first boot when the
/// disk is busiest. The work runs on its own thread and `sync` takes its
/// outcome on a later frame ([`Ceremony::settle_identity_work`]), and
/// records the step it reached then.
struct IdentityWork {
    /// Set once by the worker, taken by `sync`.
    outcome: Arc<Mutex<Option<IdentityDone>>>,
}

impl IdentityWork {
    fn reported(&self) -> bool {
        self.outcome.lock().is_ok_and(|outcome| outcome.is_some())
    }
}

/// Mint (or re-open) the identity with `minter`, on the identity worker.
fn mint_with(minter: &Shared<Minter>) -> Result<MintedIdentity, String> {
    minter
        .lock()
        .map_err(|_| "identity creation is unavailable".to_string())
        .and_then(|minter| (*minter)())
}

/// The setup actions a person has taken (through the setup app's view, or
/// the session's API) that [`Ceremony::step`] has not handled yet.
#[derive(Clone, Default)]
struct PressedActions(Rc<RefCell<HashSet<&'static str>>>);

impl PressedActions {
    /// Record `action` for the next `sync`.
    fn press(&self, action: &'static str) {
        self.0.borrow_mut().insert(action);
    }

    /// Whether `action` was taken since the last look, consuming it.
    fn take(&self, action: &str) -> bool {
        self.0.borrow_mut().remove(action)
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "recovery mode, backup verification/acknowledgement, modal entry points and presentation preferences are separate state flags"
)]
pub struct Ceremony {
    flow: Onboarding,
    /// Whether the identity ceremony is under way, for what may only
    /// happen while it is ([`Self::authentication_first_use_scope`]).
    first_use: Rc<std::cell::Cell<bool>>,
    /// What was asked of setup since [`Self::step`] last looked, oldest first.
    asked: Vec<OnboardingEffect>,
    context: OnboardingContext,
    /// Existing metadata is authoritative when its keyring item is
    /// unavailable or its collaboration chain cannot be verified. Recovery
    /// mode starts on restore and never exposes the create-new branch, while
    /// still allowing a keyring unlock retry.
    recovery_only: bool,
    /// Whether a sign-in opened setup for the person it just made or joined here: with no step
    /// recorded, setup starts at the backup rather than asking for an identity.
    after_sign_in: bool,
    minter: Shared<Minter>,
    importer: Shared<Importer>,
    words_importer: Shared<WordsImporter>,
    /// The recovery key being typed. A secret: zeroized when it is used or
    /// the screen is left.
    recovery_words: Arc<Mutex<String>>,
    recovery_check: WordsCheck,
    /// The identity this computer belongs to, when its public record is
    /// still readable: a recovery key for any other identity is refused.
    expected_identity: Option<[u8; 32]>,
    /// Why startup could not use this computer's identity, when it opened
    /// recovery: the explanation and the step that may fix it differ.
    recovery_problem: Option<crate::identity::IdentityProblem>,
    /// The recovery key shown on the backup step, while it is written down.
    recovery_key_copy: Option<RecoveryKeyCopy>,
    /// The backup step was completed by writing down the recovery key.
    recovery_key_written: bool,
    /// The typed-back words match, as of the last frame.
    recovery_key_copy_confirmed: bool,
    exporter: Shared<Exporter>,
    /// Identity work the worker thread is doing, if any. While it runs,
    /// actions that move setup on wait, still pressed, and Back and the
    /// acknowledgement hold still, so its outcome lands on the step that
    /// asked for it.
    identity_work: Option<IdentityWork>,
    /// Set when minting failed, shown in place of the success screen — loud,
    /// since booting on into a desktop that silently has no identity is what this flow replaces.
    error: Option<String>,
    backup_error: Option<String>,
    backup_saved: Option<PathBuf>,
    backup_was_verified: bool,
    backup_export_available: bool,
    backup_capability: BackupCapability,
    /// Once a backup is verified, keep its success explanation and the
    /// Continue action in view. The passphrase form returns only when the
    /// person explicitly asks to make another copy.
    backup_copy_form_open: bool,
    /// An unprotected identity is a consequential, but supported, choice.
    /// Keep the explicit acknowledgement process-local: it only checks this
    /// one screen, while the recorded step keeps the resulting choice.
    backup_skip_acknowledged: bool,
    /// `false` shows the post-mint backup step; `true` shows the concise
    /// desktop hand-off. Recorded apart from the backup so a restart cannot
    /// skip it.
    identity_ready_page: bool,
    /// The paired device holds the public record and goes straight to the desktop.
    joined_device: Option<JoinedDevice>,
    /// Settings can reopen the proven backup ceremony for an existing
    /// identity. That is a short modal task, not a second first-login flow.
    portable_backup_only: bool,
    /// A step that could not be recorded is distinct from identity creation
    /// failure: the current screen remains valid, but advancing is blocked
    /// until the step a restart resumes at can be written or removed.
    progress_error: Option<String>,
    tour_page: Option<TourPage>,
    /// A tour reopened from the desktop has no identity ceremony behind it.
    /// Back on its first page and Done both dismiss the modal directly.
    tour_only: bool,
    /// Raised when the user has finished and the desktop may start.
    /// The person saw the result and dismissed it; shared with the host,
    /// which takes setup down then ([`Ceremony::finished_signal`]).
    finished: Rc<std::cell::Cell<bool>>,
    /// Actions the person took since `sync` last looked: view state, never
    /// written to the document.
    pressed: PressedActions,
    /// Moves each time the ceremony forgets what was typed into its secret fields.
    epoch: Arc<AtomicU64>,
    /// The file the person is being asked for, while its question stands.
    file_ask: Option<FileAsk>,
    /// The words of a written-down recovery key typed back: the buffers the
    /// app's fields write into, cleared with the copy.
    key_answers: [Arc<Mutex<String>>; 3],
    connect_source_selected: bool,
    connect_passphrase: Arc<Mutex<String>>,
    /// The code another of the person's devices printed, as typed or pasted.
    ticket: Arc<Mutex<String>>,
    /// The backup file to restore from, as typed or chosen.
    connect_path: String,
    /// How many files the person picked in the picker to restore from: the view takes the
    /// path into its field each time this moves, and never while the person types it.
    connect_picked: u64,
    backup_passphrase: Arc<Mutex<String>>,
    backup_confirmation: Arc<Mutex<String>>,
    identity_dir: Option<PathBuf>,
    session: SessionPaths,
    nearby: Option<crate::nearby::NearbyJoin>,
    /// Why the last attempt ended without joining. The nearby screen titles
    /// its error with this cause; `error` holds its message.
    nearby_failure: Option<crate::nearby::NearbyFailure>,
    codec_setup: Option<CodecSetup>,
    /// Whether this run offers the media codecs step: the pack is available
    /// and nobody has opted in yet (the installer may already have asked).
    /// Fixed at construction so the step count never changes mid-setup.
    codec_step_offered: bool,
    /// The media codecs page is showing (between backup and the hand-off).
    codec_page: bool,
    codec_status: CodecPackStatus,
    /// The install started from this page, until its unit returns.
    codec_running: Option<Receiver<Result<(), String>>>,
    /// Terminal result of the actual setup worker, used by semantic receipts.
    codec_worker_succeeded: Option<bool>,
    /// A refusal from systemd or polkit (the helper's own errors arrive in
    /// its status).
    codec_error: Option<String>,
    /// Raised by the status watcher and the install worker; the next frame
    /// re-reads the status.
    codec_changed: Arc<AtomicBool>,
    /// The watch on the codecs status, while setup follows it.
    codec_watching: Option<tilo_files::native::ChangeWatch>,
    /// When `sync` last looked at the background work above.
    synced_at: Instant,
    /// The old system found on this computer's disks (`Windows`), which the hand-off offers to
    /// bring the person's things over from.
    old_system: Option<String>,
    /// What the person chose to open as they left setup: the migration app, Settings' accounts
    /// or the Store, which the session opens once setup is down.
    then_open: Option<String>,
}

impl Ceremony {
    /// Build a ceremony that mints into `identity_dir`.
    pub fn new(identity_dir: PathBuf) -> Self {
        Self::new_in_context(identity_dir, OnboardingContext::FullSystem, false)
    }

    /// Build the reduced identity-only ceremony for Tilo installed as a
    /// desktop session on an existing Linux host.
    pub fn new_for_host_desktop(identity_dir: PathBuf) -> Self {
        Self::new_in_context(identity_dir, OnboardingContext::HostDesktop, false)
    }

    /// Build recovery-only trusted chrome for an existing identity whose
    /// Secret Service private share is unavailable.
    pub fn new_for_recovery(identity_dir: PathBuf) -> Self {
        Self::new_in_context(identity_dir, OnboardingContext::FullSystem, true)
    }

    /// Build recovery-only trusted chrome for a host desktop session.
    pub fn new_for_host_desktop_recovery(identity_dir: PathBuf) -> Self {
        Self::new_in_context(identity_dir, OnboardingContext::HostDesktop, true)
    }

    /// Open the ceremony a sign-in made or joined this account's person for: with no step
    /// recorded, it starts at the backup, as an identity this computer already holds leaves
    /// nothing to choose before it.
    #[must_use]
    pub fn after_sign_in(mut self) -> Self {
        self.after_sign_in = true;
        if self.flow.minted().is_none() {
            self.restore_progress();
            if self.flow.minted().is_some() && self.refresh_backup_capability().is_err() {
                self.error = Some(t("onboarding-connect-recovery-message"));
            }
        }
        self
    }

    /// Reopen the introduction after first login without repeating or
    /// weakening any identity ceremony.
    pub fn new_tour() -> Self {
        let mut view = Self::with_all_handlers(
            Box::new(|| Err("identity creation is unavailable in the desktop tour".into())),
            Box::new(|_, _| Err("identity import is unavailable in the desktop tour".into())),
            Box::new(|_, _, _| Err("identity backup is unavailable in the desktop tour".into())),
        );
        view.tour_page = Some(TourPage::Navigate);
        view.tour_only = true;
        view
    }

    fn new_in_context(
        identity_dir: PathBuf,
        context: OnboardingContext,
        recovery_only: bool,
    ) -> Self {
        Self::new_in_session(
            identity_dir,
            SessionPaths::from_process(),
            context,
            recovery_only,
        )
    }

    fn new_in_session(
        identity_dir: PathBuf,
        session: SessionPaths,
        context: OnboardingContext,
        recovery_only: bool,
    ) -> Self {
        let mint_dir = identity_dir.clone();
        let import_dir = identity_dir.clone();
        let words_dir = identity_dir.clone();
        // This computer's daemon makes or restores the person and keeps their root; the account
        // takes them up.
        let minter: Minter = Box::new(move || {
            crate::identity::found_by_daemon(&mint_dir).map_err(|e| e.to_string())
        });
        let mut view = Self::with_all_handlers_in_context(
            minter,
            Box::new(move |source, passphrase| import_identity(&import_dir, source, passphrase)),
            Box::new(|_identity, destination, passphrase| export_backup(destination, passphrase)),
            context,
        );
        view.backup_capability = Box::new(daemon_backup_capability);
        view.words_importer = Arc::new(Mutex::new(Box::new(move |words: &str| {
            import_recovery_words(&words_dir, words)
        })));
        if recovery_only {
            view.recovery_problem = crate::identity::IdentityProblem::from_recovery_env();
        }
        view.expected_identity = tilo_identity::store::recovery_expected_identity(&identity_dir)
            .ok()
            .flatten();
        view.session = session;
        view.recovery_only = recovery_only;
        if context == OnboardingContext::FullSystem
            && !recovery_only
            && let Some(setup) = CodecSetup::system()
        {
            view = view.with_codec_setup(setup);
        }
        if recovery_only && view.flow.screen() == Screen::Intro {
            view.flow.begin_existing();
        }
        view.identity_dir = Some(identity_dir);
        view.restore_progress();
        if view.flow.minted().is_some() && view.refresh_backup_capability().is_err() {
            view.error = Some(t("onboarding-connect-recovery-message"));
        }
        view
    }

    /// Build with an injected minter, so tests drive the whole widget without writing a key to disk.
    pub fn with_minter(minter: Minter) -> Self {
        Self::with_handlers(
            minter,
            Box::new(|_, _| Err("identity import is unavailable in this test".to_string())),
        )
    }

    /// Build the production host-session composition with an injected minter.
    /// Kept for deterministic visual/interaction review: only identity I/O is
    /// replaced; copy, state transitions, layout and controls are unchanged.
    #[doc(hidden)]
    pub fn with_minter_for_host_desktop(minter: Minter) -> Self {
        Self::with_all_handlers_in_context(
            minter,
            Box::new(|_, _| Err("identity import is unavailable in this test".to_string())),
            Box::new(|_, _, _| Err("identity backup is unavailable in this test".to_string())),
            OnboardingContext::HostDesktop,
        )
    }

    fn with_handlers(minter: Minter, importer: Importer) -> Self {
        Self::with_all_handlers(
            minter,
            importer,
            Box::new(|_, _, _| Err("identity backup is unavailable in this test".to_string())),
        )
    }

    fn with_all_handlers(minter: Minter, importer: Importer, exporter: Exporter) -> Self {
        Self::with_all_handlers_in_context(
            minter,
            importer,
            exporter,
            OnboardingContext::FullSystem,
        )
    }

    fn with_all_handlers_in_context(
        minter: Minter,
        importer: Importer,
        exporter: Exporter,
        context: OnboardingContext,
    ) -> Self {
        Self {
            flow: Onboarding::new(),
            first_use: Rc::default(),
            asked: Vec::new(),
            context,
            recovery_only: false,
            after_sign_in: false,
            minter: Arc::new(Mutex::new(minter)),
            importer: Arc::new(Mutex::new(importer)),
            words_importer: Arc::new(Mutex::new(Box::new(|_: &str| {
                Err("recovery keys are unavailable in this test".to_string())
            }) as WordsImporter)),
            recovery_words: Arc::new(Mutex::new(String::new())),
            recovery_check: WordsCheck::Empty,
            expected_identity: None,
            recovery_problem: None,
            recovery_key_copy: None,
            recovery_key_written: false,
            recovery_key_copy_confirmed: false,
            exporter: Arc::new(Mutex::new(exporter)),
            identity_work: None,
            error: None,
            backup_error: None,
            backup_saved: None,
            backup_was_verified: false,
            backup_export_available: true,
            // Injected handlers describe a supported local identity by default;
            // the real system constructor always installs the canonical reader.
            backup_capability: Box::new(|_| Ok(true)),
            backup_copy_form_open: false,
            backup_skip_acknowledged: false,
            identity_ready_page: false,
            joined_device: None,
            portable_backup_only: false,
            progress_error: None,
            tour_page: None,
            tour_only: false,
            finished: Rc::default(),
            pressed: PressedActions::default(),
            epoch: Arc::default(),
            file_ask: None,
            key_answers: std::array::from_fn(|_| Arc::new(Mutex::new(String::new()))),
            connect_source_selected: false,
            connect_passphrase: Arc::new(Mutex::new(String::new())),
            ticket: Arc::new(Mutex::new(String::new())),
            connect_path: String::new(),
            connect_picked: 0,
            backup_passphrase: Arc::new(Mutex::new(String::new())),
            backup_confirmation: Arc::new(Mutex::new(String::new())),
            identity_dir: None,
            session: SessionPaths::default(),
            nearby: None,
            nearby_failure: None,
            codec_setup: None,
            codec_step_offered: false,
            codec_page: false,
            codec_status: CodecPackStatus::default(),
            codec_running: None,
            codec_worker_succeeded: None,
            codec_error: None,
            codec_changed: Arc::new(AtomicBool::new(false)),
            codec_watching: None,
            synced_at: Instant::now(),
            old_system: None,
            then_open: None,
        }
    }

    /// Whether the identity ceremony is under way, asked again on every
    /// call. Authentication prompts that belong to first use are answered
    /// only while it is; the session still authenticates what it delivers.
    pub fn authentication_first_use_scope(&self) -> Rc<dyn Fn() -> bool> {
        self.publish_first_use();
        let first_use = Rc::downgrade(&self.first_use);
        Rc::new(move || first_use.upgrade().is_some_and(|active| active.get()))
    }

    /// Every step of the ceremony up to entering the desktop, except typing
    /// a recovery key in.
    fn publish_first_use(&self) {
        let active = !self.finished.get()
            && (self.tour_page.is_some() || self.flow.screen() != Screen::RecoveryKey);
        self.first_use.set(active);
    }

    /// Take `action` as though its button had been clicked. Tests walk the
    /// ceremony this way; the next [`Self::step`] handles it.
    pub fn press(&self, action: &'static str) {
        self.pressed.press(action);
    }

    /// The tour page on screen, counted from 1.
    #[cfg(any(test, feature = "test-support"))]
    pub fn tour_step(&self) -> Option<u8> {
        self.tour_page.map(TourPage::number)
    }

    /// Offer the media codecs step through `setup`, unless the owner already
    /// opted in (then setup does not ask again).
    #[must_use]
    pub fn with_codec_setup(mut self, setup: CodecSetup) -> Self {
        self.codec_status = (setup.status)();
        self.codec_step_offered = !self.codec_status.opted_in();
        self.codec_setup = Some(setup);
        self
    }

    /// Offer, on the hand-off, to bring the person's things over from `system`, an old system
    /// found on this computer's disks (`Windows`).
    #[must_use]
    pub fn with_old_system(mut self, system: Option<String>) -> Self {
        self.old_system = system;
        self
    }

    /// What the person chose to open as they left setup, taken once: a `tilo://open#…` link or a
    /// `settings://` page.
    pub fn take_then_open(&mut self) -> Option<String> {
        if self.finished() {
            self.then_open.take()
        } else {
            None
        }
    }

    /// Whether the media codecs page is showing.
    pub fn codec_page_visible(&self) -> bool {
        self.codec_page
    }

    /// The helper status the codecs page last read.
    pub fn codec_status(&self) -> &CodecPackStatus {
        &self.codec_status
    }

    /// Setup's total step count: the codecs step adds one.
    fn total_steps(&self) -> u8 {
        if self.recovery_only {
            // Recovery is choosing a way back, then taking it.
            2
        } else if self.codec_step_offered {
            5
        } else {
            4
        }
    }

    pub fn flow(&self) -> &Onboarding {
        &self.flow
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// True once the user has seen the result and dismissed it.
    pub fn finished(&self) -> bool {
        self.finished.get()
    }

    /// Whether the person finished setup, asked again on every call: the
    /// session hosting this view takes it down once it holds.
    pub fn finished_signal(&self) -> Rc<dyn Fn() -> bool> {
        let finished = Rc::clone(&self.finished);
        Rc::new(move || finished.get())
    }

    /// A tour is trusted modal chrome, but it deliberately keeps the real
    /// desktop visible underneath so each lesson points at its context.
    /// First-login identity setup remains an opaque machine surface until the
    /// person explicitly starts the tour.
    pub fn presents_desktop_context(&self) -> bool {
        self.tour_page.is_some()
    }

    /// The identity backup a desktop already in use opens: the export, the file it is written to
    /// and the proof it opens that first use offers, and nothing that creates or restores an
    /// identity. It ends back at the desktop the person came from.
    ///
    /// # Errors
    /// This computer keeps no identity to back up.
    pub fn portable_backup(identity_dir: PathBuf) -> Result<Self, String> {
        Self::portable_backup_in(identity_dir, OnboardingContext::FullSystem)
    }

    /// [`portable_backup`] on Tilo installed as a desktop session on an existing Linux host.
    ///
    /// # Errors
    /// This computer keeps no identity to back up.
    pub fn portable_backup_for_host_desktop(identity_dir: PathBuf) -> Result<Self, String> {
        Self::portable_backup_in(identity_dir, OnboardingContext::HostDesktop)
    }

    fn portable_backup_in(
        identity_dir: PathBuf,
        context: OnboardingContext,
    ) -> Result<Self, String> {
        let record = tilo_identity::store::resolve_active_identity(&identity_dir)
            .ok_or_else(|| "this computer keeps no identity to back up".to_owned())?;
        let identity = crate::identity::existing_identity_at(&record)?;
        match daemon_backup_capability(&identity.path) {
            Ok(true) => {}
            Ok(false) => {
                return Err(
                    "this computer entered another computer's identity: the computer that keeps \
                     it writes the backup"
                        .to_owned(),
                );
            }
            Err(why) => return Err(why),
        }
        let mut view = Self::new_in_context(identity_dir, context, false);
        view.begin_portable_backup(identity);
        Ok(view)
    }

    /// Reset this trusted view to the existing-identity backup step, the same
    /// export, picker, and verification flow as first-login onboarding, with nothing before or
    /// after it.
    fn begin_portable_backup(&mut self, identity: MintedIdentity) {
        self.flow = Onboarding::new();
        self.flow.resume_created(identity);
        self.error = None;
        self.backup_error = None;
        self.backup_saved = None;
        self.backup_was_verified = false;
        self.backup_copy_form_open = false;
        self.backup_skip_acknowledged = false;
        self.identity_ready_page = false;
        self.joined_device = None;
        self.portable_backup_only = true;
        self.progress_error = None;
        // The outcome of identity work another flow started no longer applies.
        self.identity_work = None;
        self.tour_page = None;
        self.tour_only = false;
        self.finished.set(false);
    }

    /// The backup step alone, for an identity a test already holds.
    #[cfg(test)]
    fn begin_portable_backup_for(&mut self, identity: MintedIdentity) {
        self.begin_portable_backup(identity);
    }

    /// Open setup again at the step the person's settings hold for this device, when a restart
    /// interrupted it ([`progress`]).
    fn restore_progress(&mut self) {
        // Recovery mode was selected because the existing identity could not
        // be used safely. A stale post-mint step must not turn that state
        // into a Done screen that assumes signing works; the user must unlock
        // the keyring or restore first.
        if self.recovery_only {
            return;
        }
        let (Some(identity_dir), Some(paths)) =
            (self.identity_dir.as_ref(), self.session.identity_paths())
        else {
            return;
        };
        let (stage, unreadable) = match progress::recorded(&paths) {
            // A person a sign-in has just taken up has no step: their identity is here, so setup
            // offers its backup and the tour rather than asking for an identity this computer
            // already holds.
            None if self.after_sign_in => (Stage::IdentityReady, None),
            None => return,
            Some(Ok(stage)) => (stage, None),
            // A step this build cannot name resumes at the earliest one an
            // identity has, so nothing the person had not finished is skipped.
            Some(Err(error)) => (Stage::IdentityReady, Some(error)),
        };
        let identity_path = tilo_identity::store::resolve_active_identity(identity_dir)
            .unwrap_or_else(|| identity_dir.join("primary.json"));
        match crate::identity::existing_identity_at(&identity_path) {
            Ok(identity) => {
                self.joined_device = self.read_joined_device(&identity);
                self.flow.resume_created(identity);
                self.identity_ready_page = matches!(
                    stage,
                    Stage::ReadyToEnter
                        | Stage::ReadyToEnterBackedUp
                        | Stage::TourNavigate
                        | Stage::TourOrganise
                        | Stage::TourCollaborate
                        | Stage::TourStart
                );
                self.backup_was_verified = matches!(
                    stage,
                    Stage::IdentityReadyBackedUp | Stage::ReadyToEnterBackedUp
                );
                self.tour_page = match stage {
                    Stage::IdentityReady
                    | Stage::IdentityReadyBackedUp
                    | Stage::ReadyToEnter
                    | Stage::ReadyToEnterBackedUp => None,
                    Stage::TourNavigate => Some(TourPage::Navigate),
                    Stage::TourOrganise => Some(TourPage::Organise),
                    Stage::TourCollaborate => Some(TourPage::Collaborate),
                    Stage::TourStart => Some(TourPage::Start),
                };
                if let Some(error) = unreadable {
                    self.progress_error = Some(t_args(
                        "onboarding-progress-unreadable",
                        &[("error", error.into())],
                    ));
                }
            }
            Err(error) => {
                // A step kept for an identity this device no longer holds
                // leaves setup to start over: the identity is made again and
                // its step written anew. One that is here but does not read
                // was already refused by startup and is said here too.
                if std::fs::symlink_metadata(&identity_path).is_ok() {
                    self.progress_error = Some(t_args(
                        "onboarding-progress-invalid-identity",
                        &[("error", error.into())],
                    ));
                }
            }
        }
    }

    /// Record `stage` as the step a restart opens setup at, or with `None`
    /// that setup on this device is finished, before the step it names is
    /// shown: whether it was recorded. The settings take the write without
    /// waiting for the disk, so the step follows on the same frame.
    ///
    /// Only steps an identity exists for are recorded: before it there is no
    /// person whose settings could keep them ([`progress`]). A view with no
    /// system identity behind it (a test, a review renderer, the tour
    /// reopened from the desktop) records nothing. A write the settings
    /// refuse is shown, and the step waits for the person to try again.
    fn record(&mut self, stage: Option<Stage>) -> bool {
        let Some(paths) = self.session.identity_paths() else {
            return true;
        };
        match progress::record(&paths, stage) {
            Ok(()) => {
                self.progress_error = None;
                true
            }
            Err(error) => {
                let key = if stage.is_some() {
                    "onboarding-progress-save-failed"
                } else {
                    "onboarding-progress-clear-failed"
                };
                self.progress_error = Some(t_args(key, &[("error", error.to_string().into())]));
                false
            }
        }
    }

    /// The hand-off's step, with whether a backup was verified.
    fn ready_stage(&self) -> Stage {
        if self.backup_was_verified {
            Stage::ReadyToEnterBackedUp
        } else {
            Stage::ReadyToEnter
        }
    }

    /// Leave the backup step for the hand-off once a restart would open it
    /// there. The codecs offer between them is not a step of its own: a
    /// restart during it resumes at the hand-off, where Settings still offers
    /// the codecs.
    fn enter_ready(&mut self) {
        if self.record(Some(self.ready_stage())) {
            self.identity_ready_page = true;
            self.codec_page = self.codec_step_offered;
            clear_secret(&self.backup_passphrase, &self.epoch);
            clear_secret(&self.backup_confirmation, &self.epoch);
        }
    }

    /// Show tour page `page`, or with `None` the hand-off it started from,
    /// once a restart would open it there. A tour reopened from the desktop
    /// is no setup and records nothing.
    fn show_tour(&mut self, page: Option<TourPage>) {
        let stage = match page {
            None => self.ready_stage(),
            Some(TourPage::Navigate) => Stage::TourNavigate,
            Some(TourPage::Organise) => Stage::TourOrganise,
            Some(TourPage::Collaborate) => Stage::TourCollaborate,
            Some(TourPage::Start) => Stage::TourStart,
        };
        if self.tour_only || self.record(Some(stage)) {
            self.tour_page = page;
        }
    }

    /// End setup once its step is removed, so a restart goes straight to the
    /// desktop.
    fn finish_setup(&mut self) {
        if self.record(None) {
            self.finish_ceremony();
        }
    }

    /// Whether `action` was pressed and may run now: while identity work
    /// runs, an action that moves setup on waits, still pressed, so the
    /// work's outcome lands on the step that asked for it.
    fn take_idle(&self, action: &str) -> bool {
        self.identity_idle() && self.pressed.take(action)
    }

    /// Whether no identity work is running.
    fn identity_idle(&self) -> bool {
        self.identity_work.is_none()
    }

    /// Run `work` on the identity worker; a later `sync` takes its outcome.
    /// `unstarted` is the outcome when the worker cannot start or the work
    /// panics: an error the person sees, never a step taken without it.
    ///
    /// Without an identity directory nothing behind this view is on disk
    /// (an injected handler, as in review renderers), and the work runs now.
    fn start_identity_work(
        &mut self,
        work: impl FnOnce() -> IdentityDone + Send + 'static,
        unstarted: fn(String) -> IdentityDone,
    ) {
        if self.identity_dir.is_none() {
            let done = work();
            self.finish_identity_work(done);
            return;
        }
        if !self.identity_idle() {
            tracing::error!(target: "tilo::onboarding", "identity work asked for while other identity work runs");
            return;
        }
        let outcome = Arc::new(Mutex::new(None));
        let reported = Arc::clone(&outcome);
        // Made on the loop's thread, so it wakes the loop the outcome waits
        // for; a no-op where no loop installed a wake.
        let wake = crate::services::loop_waker();
        let spawned = std::thread::Builder::new()
            .name("tilo-identity".into())
            .spawn(move || {
                let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                    .unwrap_or_else(|_| unstarted("identity work failed".into()));
                if let Ok(mut outcome) = reported.lock() {
                    *outcome = Some(done);
                }
                wake.wake();
            });
        match spawned {
            Ok(_detached) => {
                self.identity_work = Some(IdentityWork { outcome });
            }
            Err(error) => {
                tracing::error!(target: "tilo::onboarding", %error, "could not start identity work");
                self.finish_identity_work(unstarted(error.to_string()));
            }
        }
    }

    /// Take the outcome of identity work that has reported since the last
    /// frame.
    fn settle_identity_work(&mut self) {
        let done = self
            .identity_work
            .as_ref()
            .and_then(|work| work.outcome.lock().ok()?.take());
        if let Some(done) = done {
            self.identity_work = None;
            self.finish_identity_work(done);
        }
    }

    fn finish_identity_work(&mut self, done: IdentityDone) {
        match done {
            IdentityDone::Mint(minted) => self.finish_mint(minted),
            IdentityDone::RetryMint(identity) => self.finish_retry(identity),
            IdentityDone::Import(identity) => self.finish_import(identity),
            IdentityDone::Export(exported) => self.finish_export(exported),
            IdentityDone::RecoveryWords(words) => match words {
                Ok(words) => {
                    self.backup_error = None;
                    self.recovery_key_copy = Some(RecoveryKeyCopy::new(
                        words,
                        self.key_answers.clone(),
                        Arc::clone(&self.epoch),
                    ));
                }
                Err(error) => self.backup_error = Some(error),
            },
        }
    }

    fn finish_ceremony(&mut self) {
        self.finished.set(true);
        if let Some(copy) = self.recovery_key_copy.take() {
            copy.clear();
        }
        clear_secret(&self.connect_passphrase, &self.epoch);
        clear_secret(&self.ticket, &self.epoch);
        clear_secret(&self.recovery_words, &self.epoch);
        clear_secret(&self.backup_passphrase, &self.epoch);
        clear_secret(&self.backup_confirmation, &self.epoch);
    }

    /// Take the code the person typed into the nearby screen and redeem it.
    /// A code this computer cannot use is
    /// said here, without calling any device.
    fn start_nearby_join(&mut self) {
        if self.identity_dir.is_none() || self.session.identity.is_none() {
            self.fail_nearby(crate::nearby::NearbyFailure::new(
                crate::nearby::NearbyCause::Unavailable,
            ));
            return;
        }
        if self
            .ticket
            .lock()
            .is_ok_and(|buffer| buffer.trim().is_empty())
        {
            self.error = Some(t("onboarding-nearby-ticket-empty"));
            self.nearby_failure = None;
            return;
        }
        // A code this computer cannot use is said here: nothing was attempted,
        // so nothing is recorded and no device is called.
        if let Err(failure) = self.typed_ticket() {
            clear_secret(&self.ticket, &self.epoch);
            self.fail_nearby(failure);
            return;
        }
        self.redeem_nearby_code();
    }

    /// The code the person typed, with the rest of the line `tilo device add`
    /// printed taken off, when it is a code this computer can use.
    fn typed_ticket(&self) -> Result<Zeroizing<String>, crate::nearby::NearbyFailure> {
        let typed = self
            .ticket
            .lock()
            .ok()
            .map(|buffer| Zeroizing::new(buffer.trim().to_owned()))
            .unwrap_or_default();
        let ticket = typed.strip_prefix("tilo login ").unwrap_or(&typed).trim();
        if ticket.is_empty() || tilo_device_identity::login::Ticket::parse(ticket).is_err() {
            return Err(crate::nearby::NearbyFailure::new(
                crate::nearby::NearbyCause::Wrong,
            ));
        }
        Ok(Zeroizing::new(ticket.to_owned()))
    }

    fn fail_nearby(&mut self, failure: crate::nearby::NearbyFailure) {
        self.error = Some(failure.message());
        self.nearby_failure = Some(failure);
    }

    /// Redeem the code the nearby screen holds on the device that printed it.
    ///
    /// A restart before this computer took up the identity starts setup over,
    /// and the person asks their other device for a new code: a code is
    /// redeemed once, so this is the one step a restart can ask for again.
    /// One after it finds the joined identity and opens at the hand-off, or
    /// at the desktop when it came before that step was recorded.
    fn redeem_nearby_code(&mut self) {
        let (Some(identity_dir), Some(system)) = (&self.identity_dir, &self.session.identity)
        else {
            self.fail_nearby(crate::nearby::NearbyFailure::new(
                crate::nearby::NearbyCause::Unavailable,
            ));
            return;
        };
        let ticket = match self.typed_ticket() {
            Ok(ticket) => ticket,
            Err(failure) => {
                clear_secret(&self.ticket, &self.epoch);
                self.fail_nearby(failure);
                return;
            }
        };
        clear_secret(&self.ticket, &self.epoch);
        match crate::nearby::NearbyJoin::start(
            ticket.to_string(),
            identity_dir,
            &system.paths,
            &self.session.device_label,
        ) {
            Ok(join) => {
                self.nearby = Some(join);
                self.error = None;
                self.nearby_failure = None;
            }
            Err(failure) => self.fail_nearby(failure),
        }
    }

    /// Restore the identity from the backup at `source` on the identity
    /// worker. A restart while it runs is the mint's ([`Self::mint_identity`]):
    /// no identity yet and setup starts over, or the restored one and its
    /// desktop; restoring the same backup again gives the same identity.
    fn import_identity(&mut self, source: &std::path::Path) {
        let passphrase = Zeroizing::new(
            self.connect_passphrase
                .lock()
                .map(|value| value.as_bytes().to_vec())
                .unwrap_or_default(),
        );
        let importer = Arc::clone(&self.importer);
        let source = source.to_path_buf();
        self.start_identity_work(
            move || {
                // The passphrase copy is zeroized when the work drops it.
                let result = importer
                    .lock()
                    .map_err(|_| "identity import is unavailable".to_string())
                    .and_then(|importer| (*importer)(&source, &passphrase));
                IdentityDone::Import(result)
            },
            |error| IdentityDone::Import(Err(error)),
        );
    }

    fn finish_import(&mut self, imported: Result<MintedIdentity, String>) {
        match imported {
            Ok(identity) => {
                self.error = None;
                clear_secret(&self.recovery_words, &self.epoch);
                self.recovery_check = WordsCheck::Empty;
                self.error = self
                    .accept_connected_identity(identity)
                    .err()
                    .map(|()| t("onboarding-connect-recovery-message"));
                clear_secret(&self.connect_passphrase, &self.epoch);
            }
            Err(error) => {
                tracing::error!(target: "tilo::onboarding", %error, "identity import failed");
                self.error = Some(error);
            }
        }
    }

    fn retry_existing_identity(&mut self) {
        let minter = Arc::clone(&self.minter);
        self.start_identity_work(
            move || IdentityDone::RetryMint(mint_with(&minter)),
            |error| IdentityDone::RetryMint(Err(error)),
        );
    }

    fn finish_retry(&mut self, identity: Result<MintedIdentity, String>) {
        match identity {
            Ok(identity) => {
                self.error = None;
                self.error = self
                    .accept_connected_identity(identity)
                    .err()
                    .map(|()| t("onboarding-connect-recovery-message"));
            }
            Err(error) => {
                self.error = Some(t_args(
                    "onboarding-keyring-unavailable",
                    &[("error", error.into())],
                ));
            }
        }
    }

    /// Make the person's identity on the identity worker.
    ///
    /// Nothing is recorded ahead of it: before the identity exists there is
    /// no person to keep a step for. A restart while it runs finds either no
    /// identity, and setup starts over at its first page with nothing made,
    /// or the identity written before its step was; the desktop then opens
    /// as for any identity with no step left, and Settings still offers its
    /// backup. The minter opens an identity that is already there rather than
    /// making another, so a Create taken again never makes a second one.
    fn mint_identity(&mut self) {
        self.error = None;
        let minter = Arc::clone(&self.minter);
        self.start_identity_work(
            move || IdentityDone::Mint(mint_with(&minter)),
            |error| IdentityDone::Mint(Err(error)),
        );
    }

    fn finish_mint(&mut self, minted: Result<MintedIdentity, String>) {
        match minted {
            Ok(minted) => {
                self.identity_ready_page = false;
                // The acknowledgement and Back hold still while the minter
                // runs, so `complete`'s own `can_create` check still holds.
                if self.flow.complete(minted) {
                    if self.refresh_backup_capability().is_err() {
                        self.error = Some(t("onboarding-connect-recovery-message"));
                        return;
                    }
                    // The identity is what a restart from here finds; its step
                    // says which of the steps after it the person reached.
                    self.record(Some(if self.identity_ready_page {
                        Stage::ReadyToEnter
                    } else {
                        Stage::IdentityReady
                    }));
                }
            }
            Err(e) => {
                tracing::error!(target: "tilo::onboarding", error = %e, "identity minting failed");
                self.error = Some(e);
                // Stay on the protected step. A failed write is not a
                // completed identity and must never release the desktop.
            }
        }
    }

    /// Write the portable backup of the minted identity to `destination`.
    fn export_identity(&mut self, destination: PathBuf) {
        let passphrase = Zeroizing::new(
            self.backup_passphrase
                .lock()
                .map(|value| value.as_bytes().to_vec())
                .unwrap_or_default(),
        );
        let _ = self.start_backup_export(destination, passphrase);
    }

    fn start_backup_export(
        &mut self,
        destination: PathBuf,
        passphrase: Zeroizing<Vec<u8>>,
    ) -> Result<(), ()> {
        if !self.backup_export_available || !self.identity_idle() {
            return Err(());
        }
        let Some(identity) = self.flow.minted().map(|identity| identity.path.clone()) else {
            self.backup_error = Some("the identity is not ready to back up".to_string());
            return Err(());
        };
        let exporter = Arc::clone(&self.exporter);
        self.start_identity_work(
            move || {
                // The passphrase copy is zeroized when the work drops it.
                let result = exporter
                    .lock()
                    .map_err(|_| "identity backup is unavailable".to_string())
                    .and_then(|exporter| (*exporter)(&identity, &destination, &passphrase));
                IdentityDone::Export(result.map(|()| destination))
            },
            |error| IdentityDone::Export(Err(error)),
        );
        Ok(())
    }

    /// Take the outcome of a backup's export, which the exporter wrote and
    /// read back. A restart before its step is recorded opens the backup step
    /// again: the copy was never confirmed to the person, so asking again is
    /// the honest step, and the next export replaces what was written.
    fn finish_export(&mut self, exported: Result<PathBuf, String>) {
        match exported {
            Ok(destination) => {
                self.backup_saved = Some(destination);
                self.backup_was_verified = true;
                backup_left(true);
                self.backup_copy_form_open = false;
                self.record(Some(Stage::IdentityReadyBackedUp));
                self.backup_error = None;
                clear_secret(&self.backup_passphrase, &self.epoch);
                clear_secret(&self.backup_confirmation, &self.epoch);
            }
            Err(error) => {
                self.backup_error = Some(error);
            }
        }
    }

    /// Pairing with a nearby device is under way.
    fn nearby_active(&self) -> bool {
        self.nearby.is_some()
    }

    fn refresh_backup_capability(&mut self) -> Result<(), ()> {
        let path = &self.flow.minted().ok_or(())?.path;
        self.backup_export_available = (self.backup_capability)(path).map_err(|_| ())?;
        if !self.backup_export_available && !self.identity_ready_page && self.tour_page.is_none() {
            // No recovery share is available here: omit the irrelevant form.
            self.identity_ready_page = true;
            self.codec_page = self.codec_step_offered;
            self.record(Some(Stage::ReadyToEnter));
        }
        Ok(())
    }
    /// Read display names only from the device certificate and its matching public record.
    fn read_joined_device(&self, identity: &MintedIdentity) -> Option<JoinedDevice> {
        use tilo_device_identity::persist::read_device_cert;

        if !crate::identity::holds_no_identity_secret(&identity.path) {
            return None;
        }
        let mut joined = JoinedDevice::default();
        if let Some(paths) = self.session.identity_paths()
            && let Ok(profile) = tilo_identity_client::load_metadata_from_path(&identity.path)
            && hex::encode(profile.group_public_key) == identity.fingerprint
            && let Ok(cert) =
                read_device_cert(&paths.device_cert_for_user(&profile.group_public_key))
            && cert.user_public_key == profile.group_public_key
            && tilo_user_identity::verify_device_cert(&cert).is_ok()
        {
            joined.label = tilo_user_identity::device_name(&cert.device_label).map(str::to_owned);
            let name = &profile.display_name;
            if tilo_identity::store::is_chosen_display_name(name)
                && !tilo_identity::provisioning::is_throwaway_test_display_name(
                    &identity.path,
                    name,
                )
            {
                joined.person = tilo_user_identity::display_name(name)
                    .ok()
                    .map(str::to_owned);
            }
        }
        Some(joined)
    }

    fn accept_connected_identity(&mut self, identity: MintedIdentity) -> Result<(), ()> {
        let joined = self.read_joined_device(&identity);
        if !self.flow.complete_connected(identity) {
            return Err(());
        }
        self.joined_device = joined;
        self.identity_ready_page = false;
        if self.joined_device.is_some() {
            // A joined member holds no identity secret, so there is nothing
            // to back up. The hand-off is recorded first and the ready screen
            // follows it, so a restart resumes there.
            self.backup_export_available = false;
            self.codec_page = false;
            if self.record(Some(Stage::ReadyToEnter)) {
                self.identity_ready_page = true;
                self.codec_page = self.codec_step_offered;
            }
        } else {
            self.refresh_backup_capability()?;
            self.record(Some(if self.identity_ready_page {
                Stage::ReadyToEnter
            } else {
                Stage::IdentityReady
            }));
            self.codec_page = self.identity_ready_page && self.codec_step_offered;
        }
        Ok(())
    }
    pub fn backup_status(&self) -> Option<serde_json::Value> {
        if !self.flow.is_done()
            || self.identity_ready_page
            || self.tour_page.is_some()
            || self.finished.get()
        {
            return None;
        }
        Some(serde_json::json!({
            "state": if self.backup_was_verified { "verified" }
                else if self.backup_error.is_some() { "failed" } else { "ready" },
            "can_save": self.backup_export_available && self.identity_idle()
                && self.file_ask.is_none() && self.error.is_none(),
        }))
    }
    /// Whether the identity ceremony still runs. This is readiness only;
    /// native admission still checks current durable keys.
    pub fn identity_setup_active(&self) -> bool {
        self.recovery_only
            || !self.flow.is_done()
            || self
                .flow
                .minted()
                .is_none_or(|minted| !crate::identity::canonical_native_identity_ready(minted))
    }
}

/// Forget what `buffer` holds. A secret field that held something starts empty again in the view
/// too: `epoch` moves.
fn clear_secret(buffer: &Arc<Mutex<String>>, epoch: &AtomicU64) {
    if let Ok(mut value) = buffer.lock() {
        if !value.is_empty() {
            epoch.fetch_add(1, Ordering::AcqRel);
        }
        value.zeroize();
    }
}

impl Drop for Ceremony {
    fn drop(&mut self) {
        self.first_use.set(false);
        self.nearby = None;
        clear_secret(&self.connect_passphrase, &self.epoch);
        clear_secret(&self.ticket, &self.epoch);
        clear_secret(&self.backup_passphrase, &self.epoch);
        clear_secret(&self.backup_confirmation, &self.epoch);
    }
}

impl Ceremony {
    /// Ask the person for the file setup needs, in the system's picker: the
    /// backup to restore from, or where a new one goes.
    fn ask_for_file(&mut self, purpose: AskPurpose) {
        let (initial, roots) = self.session.picker_scope();
        let question = file_ask::question(purpose, &initial);
        self.file_ask = Some(FileAsk::new(purpose, question, &initial, roots));
    }

    /// The system's question the person is answering now, as they are asked
    /// it: the files setup needs, where they are.
    #[must_use]
    pub fn file_ask(&self) -> Option<&FileAsk> {
        self.file_ask.as_ref()
    }

    /// What the person answered the file question with: the files they chose,
    /// or none when they dismissed it. Nothing changes on a dismissal.
    pub fn files_answered(&mut self, files: Vec<String>) {
        let Some(ask) = self.file_ask.take() else {
            return;
        };
        let purpose = ask.purpose();
        if let Some(path) = files.into_iter().next() {
            let path = PathBuf::from(path);
            match purpose {
                AskPurpose::Restore => {
                    self.connect_path = path.to_string_lossy().into_owned();
                    self.connect_picked = self.connect_picked.wrapping_add(1);
                    self.connect_source_selected = true;
                    self.error = None;
                }
                AskPurpose::Save => self.export_identity(path),
            }
        }
    }
}

impl Ceremony {
    /// Take what was asked since the last look, and what the background work reported, and move
    /// setup on: the session's pass calls it before it publishes the page again.
    #[allow(
        clippy::too_many_lines,
        reason = "one press after another in the order setup takes them"
    )]
    pub fn step(&mut self) {
        self.synced_at = Instant::now();
        self.refresh_codec_state();

        // What was asked of setup, before interpreting the action keys of
        // the same pass. Typed secrets are already in the ceremony's buffers.
        self.take_asks();
        self.connect_source_selected = !self.connect_path.trim().is_empty();

        // Identity work that has reported, before this frame's actions.
        self.settle_identity_work();

        if self.pressed.take(KEY_BEGIN) && self.identity_idle() {
            self.flow.begin();
        }

        if self.pressed.take(KEY_EXISTING) && self.identity_idle() {
            self.flow.begin_existing();
        }

        if self.pressed.take(KEY_CHOOSE_RESTORE) && self.identity_idle() {
            self.error = None;
            self.flow.choose_restore();
        }

        self.sync_recovery_key();

        // Choosing the nearby flow opens the code's screen; nothing is sent
        // until the person has typed the code and chosen Join. The backup step
        // a person made on this computer offers it too: they join the person
        // they already have instead of keeping the one made here.
        if self.pressed.take(KEY_CHOOSE_NEARBY) && self.identity_idle() {
            let from_backup = self.flow.screen() == Screen::Done && !self.identity_ready_page;
            match self.flow.screen() {
                Screen::Existing => {
                    clear_secret(&self.ticket, &self.epoch);
                    self.error = None;
                    self.nearby_failure = None;
                    self.flow.choose_nearby();
                }
                Screen::Done if from_backup => {
                    clear_secret(&self.ticket, &self.epoch);
                    self.error = None;
                    self.nearby_failure = None;
                    self.flow.choose_nearby_from_backup();
                }
                _ => {}
            }
        }

        if self.pressed.take(KEY_CONNECT_BROWSE)
            && self.flow.screen() == Screen::Restore
            && self.file_ask.is_none()
        {
            self.ask_for_file(AskPurpose::Restore);
        }

        if self.pressed.take(KEY_CONNECT_NEARBY)
            && self.flow.screen() == Screen::Nearby
            && self.nearby.is_none()
            && self.identity_idle()
        {
            self.start_nearby_join();
        }

        if self.pressed.take(KEY_CONNECT_CANCEL) {
            self.nearby = None;
            clear_secret(&self.ticket, &self.epoch);
            self.nearby_failure = None;
            self.error = Some(t("onboarding-nearby-cancelled"));
        }

        let nearby = self.nearby.as_mut().map(|join| join.poll(Instant::now()));
        match nearby {
            None | Some(crate::nearby::NearbyEvent::Idle) => {}
            Some(crate::nearby::NearbyEvent::Joined(identity)) => {
                self.nearby = None;
                self.error = self
                    .accept_connected_identity(identity)
                    .err()
                    .map(|()| t("onboarding-connect-recovery-message"));
            }
            Some(crate::nearby::NearbyEvent::Failed(failure)) => {
                self.nearby = None;
                self.nearby_failure = Some(failure.clone());
                self.error = Some(failure.message());
            }
        }

        self.sync_recovery_key_copy();

        if self.pressed.take(KEY_BACKUP_ANOTHER)
            && self.flow.screen() == Screen::Done
            && !self.identity_ready_page
            && !self.portable_backup_only
            && self.backup_was_verified
        {
            self.backup_copy_form_open = true;
            self.backup_error = None;
        }

        if self.pressed.take(KEY_BACKUP_BROWSE)
            && self.flow.screen() == Screen::Done
            && !self.identity_ready_page
            && self.file_ask.is_none()
            && self.backup_export_available
            && self.identity_idle()
        {
            self.backup_error = match (
                self.backup_passphrase.lock(),
                self.backup_confirmation.lock(),
            ) {
                (Ok(passphrase), Ok(_)) if passphrase.chars().count() < 12 => {
                    Some(t("onboarding-passphrase-too-short"))
                }
                (Ok(passphrase), Ok(confirmation)) if *passphrase != *confirmation => {
                    Some(t("onboarding-passphrase-mismatch"))
                }
                (Ok(_), Ok(_)) => None,
                _ => Some(t("onboarding-passphrase-unavailable")),
            };
            if self.backup_error.is_none() {
                self.ask_for_file(AskPurpose::Save);
            }
        }

        if self.pressed.take(KEY_BACKUP_CONTINUE) {
            let skip_acknowledged = self.backup_skip_acknowledged;
            if self.portable_backup_only && self.flow.is_done() && self.backup_was_verified {
                if self.record(None) {
                    self.finished.set(true);
                    clear_secret(&self.backup_passphrase, &self.epoch);
                    clear_secret(&self.backup_confirmation, &self.epoch);
                }
            } else if !self.portable_backup_only
                && self.flow.is_done()
                && self.tour_page.is_none()
                && !self.identity_ready_page
                && (self.backup_was_verified || skip_acknowledged)
            {
                self.enter_ready();
            }
        }

        if self.pressed.take(KEY_CODECS_INSTALL) && self.codec_page {
            self.start_codec_install();
        }

        for key in [KEY_CODECS_SKIP, KEY_CODECS_CONTINUE] {
            if self.pressed.take(key) {
                self.codec_page = false;
            }
        }

        // Back is ignored while identity work runs: its outcome belongs to
        // the step that asked for it.
        if self.pressed.take(KEY_BACK) && self.identity_idle() {
            self.error = None;
            clear_secret(&self.connect_passphrase, &self.epoch);
            clear_secret(&self.ticket, &self.epoch);
            self.nearby = None;
            self.nearby_failure = None;
            if !(self.recovery_only && self.flow.screen() == Screen::Existing) {
                self.flow.back();
            }
        }

        if self.take_idle(KEY_IMPORT) {
            let source = PathBuf::from(&self.connect_path);
            if self.flow.screen() == Screen::Restore && source.as_os_str().is_empty() {
                self.error = Some(t("onboarding-connect-choose-file-first"));
            } else if self.flow.screen() == Screen::Restore {
                self.import_identity(&source);
            }
        }

        // A locked keyring may become available after the compositor has
        // presented trusted chrome (for example after the host keyring's
        // unlock prompt completes). Retry the existing record in place; the
        // minter is idempotent and cannot overwrite metadata.
        if self.take_idle(KEY_CONNECT_RETRY)
            && self.recovery_only
            && self.flow.screen() == Screen::Existing
        {
            self.retry_existing_identity();
        }

        if self.take_idle(KEY_CREATE) && self.flow.can_create() {
            self.mint_identity();
        }

        if self.take_idle(KEY_TOUR_START)
            && self.flow.is_done()
            && self.identity_ready_page
            && !self.codec_page
        {
            self.show_tour(Some(TourPage::Navigate));
        }

        if self.take_idle(KEY_TOUR_BACK) {
            match self.tour_page {
                Some(TourPage::Navigate) if self.tour_only => self.finished.set(true),
                Some(TourPage::Navigate) => self.show_tour(None),
                Some(TourPage::Organise) => self.show_tour(Some(TourPage::Navigate)),
                Some(TourPage::Collaborate) => self.show_tour(Some(TourPage::Organise)),
                Some(TourPage::Start) => self.show_tour(Some(TourPage::Collaborate)),
                None => {}
            }
        }

        if self.take_idle(KEY_TOUR_NEXT) {
            match self.tour_page {
                Some(TourPage::Navigate) => self.show_tour(Some(TourPage::Organise)),
                Some(TourPage::Organise) => self.show_tour(Some(TourPage::Collaborate)),
                Some(TourPage::Collaborate) => self.show_tour(Some(TourPage::Start)),
                Some(TourPage::Start) | None => {}
            }
        }

        if self.take_idle(KEY_TOUR_SKIP) {
            // This action exists only while a tour page is visible. During
            // first use, removing the step is the record that the person
            // deliberately skipped; a reopened tour has no setup behind it
            // and can close immediately.
            if self.tour_page.is_some() && self.tour_only {
                self.finish_ceremony();
            } else if self.tour_page.is_some() && self.flow.is_done() {
                self.finish_setup();
            } else {
                self.finished.set(false);
            }
        }

        if self.take_idle(KEY_FINISH) {
            let at_tour_end = self.tour_page == Some(TourPage::Start);
            let skipping_tour = self.tour_page.is_none()
                && self.flow.is_done()
                && self.identity_ready_page
                && !self.codec_page;
            let knows_where_it_stands = self.backup_was_verified || self.backup_skip_acknowledged;
            // A reopened, tour-only flow exposes “Skip tour” on every page.
            // Its action is KEY_FINISH, so it must dismiss from every page,
            // not only from the final hand-off page.
            if self.portable_backup_only || self.tour_only {
                self.finish_ceremony();
            } else if (at_tour_end || skipping_tour) && self.flow.is_done() {
                self.finish_setup();
            } else if self.tour_page.is_none()
                && self.flow.is_done()
                && !self.identity_ready_page
                && knows_where_it_stands
            {
                // Ready and Backup are the same hand-off: at the end of setup
                // there is only one choice left, to enter the desktop, and a
                // restart in the middle of first login must still reach it.
                // Backup therefore never refuses the way out, and the Ready
                // page steps in exactly where the recorded step says so.
                self.enter_ready();
            } else {
                self.finished.set(false);
            }
        }

        self.publish_first_use();
    }

    /// When the session should look at setup again with nothing asked: while background work
    /// runs, setup polls what it reports.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        // The codecs worker and status watcher only raise `codec_changed`,
        // and a code being redeemed reports through its own thread; nothing
        // wakes the compositor for them, so look while they run.
        let polling = self.codec_running.is_some()
            || self.codec_watching.is_some()
            || self.nearby.is_some()
            || !self.identity_idle();
        if polling {
            self.synced_at.checked_add(BACKGROUND_POLL)
        } else {
            None
        }
    }

    /// Whether setup has something to take now: an ask, or background work that reported.
    #[must_use]
    pub fn needs_step(&self) -> bool {
        // The codecs worker or status watcher reported a change: one pass
        // picks it up.
        !self.asked.is_empty()
            || self.codec_changed.load(Ordering::Acquire)
            || self
                .identity_work
                .as_ref()
                .is_some_and(IdentityWork::reported)
    }
}

/// Keep in the person's settings whether their identity is backed up: `false` when they left the
/// backup for later, which the session's reminder offers again, and `true` once a backup is
/// verified, which ends the reminders.
fn backup_left(backed_up: bool) {
    if let Err(error) = settings_store::identity::BACKED_UP.set(Some(backed_up)) {
        tracing::warn!(%error, backed_up, "whether the identity is backed up is not kept");
    }
}
