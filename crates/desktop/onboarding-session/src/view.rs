//! The first-login identity ceremony the shell runs, shown by the setup app
//! ([`tilo_onboarding`]).
//!
//! The shell keeps the ceremony: the decisions ([`crate::identity::Onboarding`]),
//! the identity work, how far setup got, adding this computer with a code, the media codecs
//! install and every secret's ownership.
//! The app shows the page this view projects ([`tilo_onboarding::Page`]) and
//! asks through the state this view lends it; secrets are typed into buffers
//! this view owns and zeroizes, never carried by what the app asks. A step
//! that needs a file from the person has this view ask for it, as the system
//! does: the app names the file it works with and never draws a picker
//! ([`file_ask`]).
//!
//! Setup stays trusted chrome: the app is mounted in the shell's own
//! process and drawn where this view stands, above every application
//! surface, before the desktop exists. No application can draw over it,
//! read it or answer for it.
//!
//! On a Tilo system that has not opted in yet, setup also offers to play more
//! video and audio formats after the backup step ([`CodecSetup`]): a page
//! with one large Install button that downloads the media codecs extension
//! only when pressed; nothing is chosen in advance. It runs
//! through the same polkit-allowed unit as Settings and never blocks setup:
//! the download continues in the background and the result arrives as a
//! notification.

mod codecs;
mod file_ask;
mod mounted;
mod progress;
mod recovery_key;
mod session;
mod snapshot;
use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value};
pub use progress::SECTION as PROGRESS_SECTION;
use progress::Stage;
use recovery_key::{RecoveryKeyCopy, import_recovery_words, words_check};
use session::SessionPaths;
pub use snapshot::OnboardingVisualState;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{cell::RefCell, rc::Rc};
use taffy::{NodeId, TaffyTree};
use ui::ColorCompat as _;
use ui::theme::Theme;
use ui::widget::{AppModel, ModelPath, TaffyTreeExt, Widget};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;
use zeroize::{Zeroize, Zeroizing};

use crate::identity::{MintedIdentity, Onboarding, Screen};
use file_ask::{AskPurpose, FileAsk};
use os_actions::codec_pack::{CodecAction, CodecPackStatus};
pub use tilo_onboarding::{OnboardingContext, TourPage, WordsCheck};
use tilo_onboarding::{t, t_args};

/// How often setup looks for news from its background work (codec install,
/// a code being redeemed) while that work runs.
const BACKGROUND_POLL: Duration = Duration::from_millis(250);

/// Whether this device's setup is under way and unfinished, as the person's settings hold it
/// ([`progress`]): a starting session opens setup again at the step it reached. A device whose key
/// cannot be read has no step to resume; its identity does not verify either, and startup opens
/// recovery for that instead.
#[must_use]
pub fn setup_unfinished() -> bool {
    progress::unfinished(&crate::firstboot::collaboration_identity_paths())
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
/// outcome on a later frame ([`OnboardingView::settle_identity_work`]), and
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Invalidation {
    #[default]
    Clean,
    Structure,
    Focus,
    StructureAndFocus,
}

impl Invalidation {
    fn structure(&mut self) {
        *self = match *self {
            Self::Clean | Self::Structure => Self::Structure,
            Self::Focus | Self::StructureAndFocus => Self::StructureAndFocus,
        };
    }

    fn both(&mut self) {
        *self = Self::StructureAndFocus;
    }

    fn structure_pending(self) -> bool {
        matches!(self, Self::Structure | Self::StructureAndFocus)
    }

    fn take_structure(&mut self) -> bool {
        let present = matches!(self, Self::Structure | Self::StructureAndFocus);
        if present {
            *self = match *self {
                Self::Structure => Self::Clean,
                Self::StructureAndFocus => Self::Focus,
                state => state,
            };
        }
        present
    }

    fn take_focus(&mut self) -> bool {
        let present = matches!(self, Self::Focus | Self::StructureAndFocus);
        if present {
            *self = match *self {
                Self::Focus => Self::Clean,
                Self::StructureAndFocus => Self::Structure,
                state => state,
            };
        }
        present
    }
}

/// Trusted setup owns these document checks; controls only queue the person's intent.
#[derive(Clone, Copy, Debug, serde::Serialize)]
enum AcknowledgementKind {
    IdentityProtection,
    SkipBackup,
}

/// The declared action that says the person acknowledged `kind`.
fn acknowledgement(kind: AcknowledgementKind, checked: bool) -> tilo_onboarding::OnboardingAction {
    use tilo_onboarding::onboarding;
    match kind {
        AcknowledgementKind::IdentityProtection => {
            tilo_onboarding::OnboardingAction::SetUnderstood(onboarding::SetUnderstood { checked })
        }
        AcknowledgementKind::SkipBackup => {
            tilo_onboarding::OnboardingAction::SetBackupSkipAcknowledged(
                onboarding::SetBackupSkipAcknowledged { checked },
            )
        }
    }
}

/// The setup actions a person has taken (through the setup app, or the
/// shell's own action surface) that `sync` has not handled yet; the
/// document never sees one.
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
pub struct OnboardingView {
    flow: Onboarding,
    /// Whether the identity ceremony is under way, for what may only
    /// happen while it is ([`Self::authentication_first_use_scope`]).
    first_use: Rc<std::cell::Cell<bool>>,
    /// What the setup app asked after an acknowledgement, for the frame
    /// after that acknowledgement settled.
    deferred_asks: Vec<tilo_onboarding::OnboardingEffect>,
    context: OnboardingContext,
    /// Existing metadata is authoritative when its keyring item is
    /// unavailable or its collaboration chain cannot be verified. Recovery
    /// mode starts on restore and never exposes the create-new branch, while
    /// still allowing a keyring unlock retry.
    recovery_only: bool,
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
    joined_device: Option<tilo_onboarding::JoinedDevice>,
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
    /// which takes setup down then ([`OnboardingView::finished_signal`]).
    finished: Rc<std::cell::Cell<bool>>,
    /// Actions the person took since `sync` last looked: view state, never
    /// written to the document.
    pressed: PressedActions,
    acknowledgements: Rc<RefCell<Vec<(AcknowledgementKind, bool)>>>,
    acknowledgement_actions: ui::local_dispatch::LocalDispatch,
    invalidation: Invalidation,
    /// The setup app this view shows the ceremony through.
    app: mounted::Mounted,
    rect: Rect,
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
    connect_path: ui::LocalState<String>,
    backup_passphrase: Arc<Mutex<String>>,
    backup_confirmation: Arc<Mutex<String>>,
    identity_dir: Option<PathBuf>,
    session: SessionPaths,
    nearby: Option<crate::nearby::NearbyJoin>,
    /// Why the last attempt ended without joining. The nearby screen titles
    /// its error with this cause; `error` holds its message.
    nearby_failure: Option<crate::nearby::NearbyFailure>,
    /// Review renderer only: draw the nearby screen as if a code were being
    /// redeemed, without calling any device.
    nearby_active_for_snapshot: bool,
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
}

impl OnboardingView {
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
        let mut view = Self {
            flow: Onboarding::new(),
            first_use: Rc::default(),
            deferred_asks: Vec::new(),
            context,
            recovery_only: false,
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
            acknowledgements: Rc::default(),
            acknowledgement_actions: ui::local_dispatch::LocalDispatch::new(),
            invalidation: Invalidation::Focus,
            app: mounted::Mounted::default(),
            rect: Rect::ZERO,
            file_ask: None,
            key_answers: std::array::from_fn(|_| Arc::new(Mutex::new(String::new()))),
            connect_source_selected: false,
            connect_passphrase: Arc::new(Mutex::new(String::new())),
            ticket: Arc::new(Mutex::new(String::new())),
            connect_path: ui::LocalState::new(String::new()),
            backup_passphrase: Arc::new(Mutex::new(String::new())),
            backup_confirmation: Arc::new(Mutex::new(String::new())),
            identity_dir: None,
            session: SessionPaths::default(),
            nearby: None,
            nearby_failure: None,
            nearby_active_for_snapshot: false,
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
        };
        view.app = mounted::Mounted::new(view.secrets());
        view
    }

    /// Setup's actions for this device: the setup app's, the way a person
    /// searching, a script or an agent here drives setup from its first
    /// frame, before any desktop or broker exists. A host lists them in this
    /// device's own catalogue and nowhere else; what is invoked through it
    /// runs on the next `sync`, as a press of the matching control would.
    pub fn device_local_api(&self) -> ui::device_local_api::SharedApi {
        self.app.api()
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

    /// Take `action` as though its button had been clicked. Review renderers
    /// and tests walk the ceremony this way; the next `sync` handles it.
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

    /// Whether the media codecs page is showing.
    pub fn codec_page_visible(&self) -> bool {
        self.codec_page
    }

    /// The helper status the codecs page last read.
    pub fn codec_status(&self) -> &CodecPackStatus {
        &self.codec_status
    }

    /// The names the setup app gives what it shows, in tree order.
    #[cfg(test)]
    /// From the page's title on: each control's name, then what it adds
    /// (a banner's message, a progress bar's percentage).
    pub fn codec_page_lines(&mut self) -> Vec<String> {
        fn names(widget: &dyn Widget, out: &mut Vec<String>) {
            if let Some(node) = widget.accessibility() {
                out.extend(node.name);
                out.extend(node.description);
                out.extend(node.value);
            }
            for child in widget.children() {
                names(child, out);
            }
        }
        let theme = Theme::dark();
        let mut text = ui::text_render::TextRenderer::new();
        let _ = crate::view::tests::layout::lay_out_as_root_with_text(
            self, 1280.0, 800.0, &theme, &mut text,
        );
        ui::widget::DrawChild::draw_child(
            self,
            &mut vello::Scene::new(),
            vello::kurbo::Affine::IDENTITY,
            &mut text,
            &theme,
            &ui::presence::PresenceCtx::empty().draw_env(),
        );
        let mut lines = Vec::new();
        if let Some(app) = self.app.root_mut() {
            names(app, &mut lines);
        }
        let title = tilo_onboarding::copy::codecs_title();
        let start = lines.iter().position(|line| *line == title).unwrap_or(0);
        lines.split_off(start)
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
        self.invalidation.both();
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
                self.invalidation.structure();
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
            clear_secret(&self.backup_passphrase);
            clear_secret(&self.backup_confirmation);
            self.invalidation.both();
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
                self.invalidation.structure();
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
                    self.recovery_key_copy =
                        Some(RecoveryKeyCopy::new(words, self.key_answers.clone()));
                }
                Err(error) => self.backup_error = Some(error),
            },
        }
        self.invalidation.structure();
    }

    fn finish_ceremony(&mut self) {
        self.finished.set(true);
        if let Some(copy) = self.recovery_key_copy.take() {
            copy.clear();
        }
        clear_secret(&self.connect_passphrase);
        clear_secret(&self.ticket);
        clear_secret(&self.recovery_words);
        clear_secret(&self.backup_passphrase);
        clear_secret(&self.backup_confirmation);
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
            self.invalidation.structure();
            return;
        }
        // A code this computer cannot use is said here: nothing was attempted,
        // so nothing is recorded and no device is called.
        if let Err(failure) = self.typed_ticket() {
            clear_secret(&self.ticket);
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
        self.invalidation.structure();
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
                clear_secret(&self.ticket);
                self.fail_nearby(failure);
                return;
            }
        };
        clear_secret(&self.ticket);
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
        self.invalidation.structure();
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
                clear_secret(&self.recovery_words);
                self.recovery_check = WordsCheck::Empty;
                self.error = self
                    .accept_connected_identity(identity)
                    .err()
                    .map(|()| t("onboarding-connect-recovery-message"));
                clear_secret(&self.connect_passphrase);
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
                self.invalidation.both();
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
                self.backup_copy_form_open = false;
                self.record(Some(Stage::IdentityReadyBackedUp));
                self.backup_error = None;
                clear_secret(&self.backup_passphrase);
                clear_secret(&self.backup_confirmation);
            }
            Err(error) => {
                self.backup_error = Some(error);
            }
        }
        self.invalidation.both();
    }

    /// Pairing with a nearby device is under way.
    fn nearby_active(&self) -> bool {
        self.nearby.is_some() || self.nearby_active_for_snapshot
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
    fn read_joined_device(
        &self,
        identity: &MintedIdentity,
    ) -> Option<tilo_onboarding::JoinedDevice> {
        use tilo_device_identity::persist::read_device_cert;

        if !crate::identity::holds_no_identity_secret(&identity.path) {
            return None;
        }
        let mut joined = tilo_onboarding::JoinedDevice::default();
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
        self.invalidation.both();
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

fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    matches!(
        state.get(root, key),
        Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), ScalarValue::Boolean(true))
    )
}

/// The key of the document's backup step, and what it holds.
const BACKUP: &str = "backup";
const SKIP_ACKNOWLEDGED: &str = "skip_acknowledged";
/// The document's key for the identity acknowledgement.
const UNDERSTOOD: &str = "understood";

/// [`read_bool`] of the key `key` in `root`'s nested map `map`.
fn read_nested_bool(state: &AutoCommit, root: &ObjId, map: &str, key: &str) -> bool {
    let Ok(Some((Value::Object(_), nested))) = state.get(root, map) else {
        return false;
    };
    read_bool(state, &nested, key)
}

fn clear_secret(buffer: &Arc<Mutex<String>>) {
    if let Ok(mut value) = buffer.lock() {
        value.zeroize();
    }
}

impl Drop for OnboardingView {
    fn drop(&mut self) {
        self.first_use.set(false);
        self.app.close_api();
        self.nearby = None;
        clear_secret(&self.connect_passphrase);
        clear_secret(&self.ticket);
        clear_secret(&self.backup_passphrase);
        clear_secret(&self.backup_confirmation);
    }
}

impl OnboardingView {
    /// Ask the person for the file setup needs, in the system's picker: the
    /// backup to restore from, or where a new one goes.
    fn ask_for_file(&mut self, purpose: AskPurpose) {
        let (initial, roots) = self.session.picker_scope();
        let question = file_ask::question(purpose, &initial);
        self.file_ask = Some(FileAsk::new(purpose, question, &initial, roots));
        self.invalidation.both();
    }

    /// The system's question the person is answering now, as they are asked
    /// it: the files setup needs, where they are.
    #[must_use]
    pub fn files_asked(&self) -> Option<&tilo_actions::remote::asked::FilesAsked> {
        self.file_ask.as_ref().map(FileAsk::question)
    }

    /// What the person answered the file question with: the files they chose,
    /// or none when they dismissed it. Nothing changes on a dismissal.
    pub(crate) fn files_answered(&mut self, files: Vec<String>) {
        let Some(ask) = self.file_ask.take() else {
            return;
        };
        let purpose = ask.purpose();
        if let Some(path) = files.into_iter().next() {
            let path = PathBuf::from(path);
            match purpose {
                AskPurpose::Restore => {
                    self.connect_path.set(path.to_string_lossy().into_owned());
                    self.connect_source_selected = true;
                    self.error = None;
                }
                AskPurpose::Save => self.export_identity(path),
            }
        }
        self.invalidation.both();
    }

    /// What the picker answered, once it did: the same answer `files_answered`
    /// takes, as the files it chose.
    fn follow_file_answer(&mut self) {
        let Some(ask) = self.file_ask.as_mut() else {
            return;
        };
        if ask.poll_lookups() {
            self.invalidation.structure();
        }
        let Some(answered) = ask.take() else {
            return;
        };
        let files = match answered {
            file_ask::Answered::Chosen(path) => vec![path.to_string_lossy().into_owned()],
            file_ask::Answered::Dismissed => Vec::new(),
        };
        self.files_answered(files);
    }
}

impl Widget for OnboardingView {
    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
        for (kind, checked) in self.acknowledgements.take() {
            let action = tilo_resource::Stamped {
                operation: acknowledgement(kind, checked),
                now_ms: 0,
            };
            if let Err(error) = self.acknowledgement_actions.apply(
                edits,
                <tilo_onboarding::Onboarding as tilo_resource::Resource>::ACTION_TYPE,
                &action,
                tilo_actions::ActionContext::default(),
            ) {
                tracing::warn!(%error, "onboarding acknowledgement refused");
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered reducer owns all modal OOBE actions and secret cleanup transitions"
    )]
    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document_ref(state, root);
        let before = self.flow.screen();
        let tour_before = self.tour_page;
        let ready_before = self.identity_ready_page;
        let codec_page_before = self.codec_page;
        // A verification pass replays the tree's sync without taking new
        // inputs. Requester progress can arrive after the frame made its
        // decision, so leave it for the next actual frame.
        let proving = ui::component_inputs::is_proving();
        if !proving {
            self.synced_at = Instant::now();
        }
        self.refresh_codec_state();

        // What the person asked through the setup app, before interpreting
        // the action keys of the same frame. Typed secrets are already in
        // this view's buffers.
        self.app.serve_api();
        self.app.update(state, root);
        self.take_app_asks();
        let connect_source_selected = !self.connect_path.with(|path| path.trim().is_empty());
        if connect_source_selected != self.connect_source_selected {
            self.connect_source_selected = connect_source_selected;
            self.invalidation.structure();
        }

        // Identity work that has reported, before this frame's actions.
        self.settle_identity_work();

        // What the person answered the file question with.
        self.follow_file_answer();

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
        // until the person has typed the code and chosen Join.
        if self.pressed.take(KEY_CHOOSE_NEARBY)
            && self.flow.screen() == Screen::Existing
            && self.identity_idle()
        {
            clear_secret(&self.ticket);
            self.error = None;
            self.nearby_failure = None;
            self.flow.choose_nearby();
            self.invalidation.structure();
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
            clear_secret(&self.ticket);
            self.nearby_failure = None;
            self.error = Some(t("onboarding-nearby-cancelled"));
            self.invalidation.structure();
        }

        let nearby = if proving {
            None
        } else {
            self.nearby.as_mut().map(|join| join.poll(Instant::now()))
        };
        match nearby {
            None | Some(crate::nearby::NearbyEvent::Idle) => {}
            Some(crate::nearby::NearbyEvent::Joined(identity)) => {
                self.nearby = None;
                self.error = self
                    .accept_connected_identity(identity)
                    .err()
                    .map(|()| t("onboarding-connect-recovery-message"));
                self.invalidation.structure();
            }
            Some(crate::nearby::NearbyEvent::Failed(failure)) => {
                self.nearby = None;
                self.nearby_failure = Some(failure.clone());
                self.error = Some(failure.message());
                self.invalidation.structure();
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
            self.invalidation.both();
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
            } else {
                self.invalidation.structure();
            }
        }

        if self.pressed.take(KEY_BACKUP_CONTINUE) {
            let skip_acknowledged = self.backup_skip_acknowledged;
            if self.portable_backup_only && self.flow.is_done() && self.backup_was_verified {
                if self.record(None) {
                    self.finished.set(true);
                    clear_secret(&self.backup_passphrase);
                    clear_secret(&self.backup_confirmation);
                    self.invalidation.both();
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
            clear_secret(&self.connect_passphrase);
            clear_secret(&self.ticket);
            self.nearby = None;
            self.nearby_failure = None;
            if !(self.recovery_only && self.flow.screen() == Screen::Existing) {
                self.flow.back();
            }
        }

        if self.take_idle(KEY_IMPORT) {
            let source = PathBuf::from(self.connect_path.get());
            if self.flow.screen() == Screen::Restore && source.as_os_str().is_empty() {
                self.error = Some(t("onboarding-connect-choose-file-first"));
                self.invalidation.structure();
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

        // The checkbox drives the check directly rather than being latched, so unticking really does close it again.
        // The acknowledgement a running mint was started with holds until
        // the mint reports.
        let understood = read_bool(state, root, UNDERSTOOD);
        if understood != self.flow.acknowledged() && self.identity_idle() {
            self.flow.set_acknowledged(understood);
            self.invalidation.structure();
        }

        let skip_acknowledged = read_nested_bool(state, root, BACKUP, SKIP_ACKNOWLEDGED);
        if skip_acknowledged != self.backup_skip_acknowledged {
            self.backup_skip_acknowledged = skip_acknowledged;
            self.invalidation.structure();
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

        if self.flow.screen() != before
            || self.tour_page != tour_before
            || self.identity_ready_page != ready_before
            || self.codec_page != codec_page_before
        {
            self.invalidation.both();
        }
        self.publish_first_use();
        self.show_page();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.show_page();
        let style = taffy::Style {
            size: taffy::Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        if self.file_ask.is_some() {
            // The question places itself over the whole surface.
            return taffy.widget_node(style, &[], node_map, self);
        }
        let child = self.app.build_layout(taffy, theme, node_map);
        let children: Vec<NodeId> = child.into_iter().collect();
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
        let resized = (rect.height() <= 540.0) != (self.rect.height() <= 540.0);
        self.rect = rect;
        if let Some(ask) = self.file_ask.as_mut() {
            ask.set_rect(file_ask::rect(rect));
            return;
        }
        self.app.apply_layout(taffy, Point::new(rect.x0, rect.y0));
        if resized {
            // A short display packs the page tighter.
            self.show_page();
        }
    }

    fn draw(&mut self, ctx: &mut ui::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if let Some(ask) = self.file_ask.as_mut() {
            // The trusted ground stays solid under the question: no
            // application may show through the surface setup stands on.
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.base,
                None,
                &self.rect,
            );
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.scrim.with_alpha_factor(0.56),
                None,
                &self.rect,
            );
            ask.set_rect(file_ask::rect(self.rect));
            ask.draw(&mut ui::DrawCtx::new(
                scene,
                transform,
                text_renderer,
                theme,
                env.clone(),
            ));
            return;
        }
        self.app.draw(&mut ui::DrawCtx::new(
            scene,
            transform,
            text_renderer,
            theme,
            env.clone(),
        ));
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(ask) = self.file_ask.as_mut() {
            ask.handle_event(event, ctx);
            *ctx.handled = true;
            self.follow_file_answer();
            return;
        }
        self.app.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if let Some(ask) = &self.file_ask {
            return vec![ask.picker()];
        }
        self.app.widget().into_iter().collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some(ask) = self.file_ask.as_mut() {
            return vec![ask.picker_mut()];
        }
        self.app.widget_mut().into_iter().collect()
    }

    fn name(&self) -> &'static str {
        "OnboardingView"
    }

    fn test_id(&self) -> Option<&str> {
        Some("onboarding")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        // The app's pump runs its handlers: what its person asked lands in
        // the state, for the next `sync`.
        let shown = self.app.update(state, path);
        let asked = self.app.state().has_asked();
        // Background work (a code being redeemed, the codecs worker) changes
        // the page from `sync` with no input behind it. The compositor
        // presents only what `update` reports, so a page that is not yet laid
        // out is a change: otherwise a join that fails on the same screen
        // stays on its wait until something unrelated repaints.
        shown || asked || self.invalidation.structure_pending()
    }

    fn take_focus_reset(&mut self) -> bool {
        self.invalidation.take_focus()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = self.invalidation.take_structure();
        let child = self
            .app
            .widget_mut()
            .is_some_and(Widget::take_structure_dirty);
        let asked = self
            .file_ask
            .as_mut()
            .is_some_and(FileAsk::take_structure_dirty);
        own || child || asked
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        // The codecs worker and status watcher only raise `codec_changed`,
        // and a code being redeemed reports through its own thread; nothing
        // wakes the compositor for them, so look while they run.
        let polling = self.codec_running.is_some()
            || self.codec_watching.is_some()
            || self.nearby.is_some()
            || !self.identity_idle();
        let mut earliest = if polling {
            self.synced_at.checked_add(BACKGROUND_POLL)
        } else {
            None
        };
        self.for_each_child(&mut |child| {
            if let Some(deadline) = child.next_wake_deadline() {
                earliest = Some(earliest.map_or(deadline, |old: Instant| old.min(deadline)));
            }
        });
        earliest
    }

    fn needs_animation_frame(&self) -> bool {
        // The codecs worker or status watcher reported a change: one frame
        // lets `sync` pick it up. Nothing is redrawn while nothing changes.
        self.codec_changed.load(Ordering::Acquire)
            || self
                .identity_work
                .as_ref()
                .is_some_and(IdentityWork::reported)
            || self.app.needs_redraw()
            || self.app.widget().is_some_and(Widget::needs_animation_frame)
            || self
                .file_ask
                .as_ref()
                .is_some_and(FileAsk::needs_animation_frame)
    }
}

#[cfg(test)]
mod tests {
    use tilo_test_env::SpawnUnderEnvLock as _;
    pub mod layout;
    mod person_made_elsewhere;
    mod ready_focus;
    mod tour_motion;
    mod viewport;
    use tilo_onboarding::OnboardingEffect;

    use super::*;
    use crate::view::tests::layout::LaidOut;
    use os_actions::codec_pack::PackState;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use tilo_identity::store::{IdentityRecord, SerializableIdentity};
    use tilo_onboarding::copy::{codecs_note, host_desktop_intro_body};
    use ui::widget::Scrollable;
    use winit::event::ElementState;
    use winit::event::Modifiers;
    use winit::keyboard::{Key, NamedKey};

    pub(super) fn view() -> (OnboardingView, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let v = OnboardingView::with_minter(Box::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(MintedIdentity {
                fingerprint: "ab".repeat(32),
                path: PathBuf::from("/home/user/.config/tilo/identity/primary.json"),
            })
        }));
        (v, calls)
    }

    /// The device setup runs on, as its steps are recorded for it: a device key of its own, a
    /// daemon that answers what the device is and keeps this account's person, and settings of the
    /// person's of its own for this test's thread.
    pub(super) struct Device {
        /// This test's own environment, swept before the home it lives under.
        _env: tilo_test_env::ScopedEnv,
        _home: tilo_test_env::TestHome,
        pub(super) paths: tilo_device_identity::IdentityPaths,
        _settings: settings_store::SourceScope,
        /// The daemon this device's programs ask, answering the device's public key.
        _daemon: crate::nearby::testing::FakeDaemon,
    }

    impl Device {
        pub(super) fn new() -> Self {
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
            let daemon = crate::nearby::testing::FakeDaemon::keeping_the_person(
                &paths,
                crate::nearby::testing::Answer::Certify,
            );
            Self {
                _env: env,
                _home: home,
                paths,
                _settings: settings_store::scoped_source(Some(settings)),
                _daemon: daemon,
            }
        }

        fn identity(&self) -> session::SystemIdentity {
            session::SystemIdentity {
                paths: self.paths.clone(),
            }
        }

        /// Run `view`'s setup on this device.
        pub(super) fn runs(&self, view: &mut OnboardingView) {
            view.session.identity = Some(self.identity());
        }

        /// Setup as a restart opens it on this device, over the identity
        /// `identity_dir` holds.
        pub(super) fn restarted(&self, identity_dir: &Path) -> OnboardingView {
            OnboardingView::new_in_session(
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
        pub(super) fn step(&self) -> Option<Stage> {
            progress::recorded(&self.paths).map(|step| step.expect("a step this build names"))
        }

        /// Record `stage` as setup on this device does when it reaches it.
        pub(super) fn reached(&self, stage: Stage) {
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

    /// Setup on a logged-out account that can join: the identity directory it
    /// writes into, this computer's device key and certificates.
    fn joining(account: &crate::nearby::tests::Account) -> (OnboardingView, PathBuf) {
        let identity_dir = account.identity_dir();
        let (mut view, _) = view();
        view.identity_dir = Some(identity_dir.clone());
        view.session.identity = Some(session::SystemIdentity {
            paths: account.paths.clone(),
        });
        (view, identity_dir)
    }

    /// Sync until `done`, letting identity work and a code's redemption
    /// report between frames.
    fn sync_until(
        view: &mut OnboardingView,
        document: &AutoCommit,
        what: &str,
        done: impl Fn(&OnboardingView) -> bool,
    ) {
        let deadline = tilo_test_env::hang_deadline();
        while !done(view) {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(20));
            view.sync(document, &automerge::ROOT);
        }
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

    tilo_test_env::scenario! {
        fn the_nearby_card_asks_for_a_code_and_adds_this_computer;

        phase the_card_opens_a_code_field_with_the_join_control;
        {
            let account = crate::nearby::tests::Account::new();
            let (mut view, _) = joining(&account);
            let mut document = AutoCommit::new();
            press(&view, &mut document, KEY_EXISTING);
            press(&view, &mut document, KEY_CHOOSE_NEARBY);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Nearby);
            assert!(view.nearby.is_none(), "nothing is sent before Join");
            assert_eq!(view.error(), None, "opening the screen is not an error");
            let composition = laid_out_themed(&mut view, &Theme::dark());
            assert!(
                accessibility_for_id(composition, "onboarding.nearby.ticket.field").is_some(),
                "the screen asks for the code"
            );
            assert!(
                accessibility_for_id(composition, "onboarding.connect.nearby.button").is_some(),
                "the Join control is there"
            );
            assert!(
                accessibility_for_id(composition, "onboarding.connect.cancel.button").is_none(),
                "nothing runs yet to cancel"
            );

            // Nothing typed: the person is told what to type, and no device is called.
            press(&view, &mut document, KEY_CONNECT_NEARBY);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.error(), Some(t("onboarding-nearby-ticket-empty").as_str()));
            assert!(view.nearby.is_none());

            // A mistyped code is named as one this computer cannot use, and is
            // not kept in the field.
            *view.ticket.lock().unwrap() = "K7Q-MX2P".to_owned();
            press(&view, &mut document, KEY_CONNECT_NEARBY);
            view.sync(&document, &automerge::ROOT);
            let failure = view.nearby_failure.clone().expect("a designed failure");
            assert_eq!(failure.cause, crate::nearby::NearbyCause::Wrong);
            assert_eq!(view.error(), Some(failure.message().as_str()));
            assert!(view.ticket.lock().unwrap().is_empty());
            assert!(view.nearby.is_none());
            let composition = laid_out(&mut view);
            let mut said = Vec::new();
            accessible_descriptions(composition, &mut said);
            assert!(
                said.iter().any(|words| words.contains("tilo1")),
                "the banner says what a code looks like: {said:?}"
            );

            // Back leaves the flow's words behind.
            press(&view, &mut document, KEY_BACK);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Existing);
            assert!(view.error().is_none());
            assert!(view.nearby_failure.is_none());
        }

        phase a_used_code_says_to_make_a_new_one_and_offers_join_again;
        {
            let account = crate::nearby::tests::Account::new();
            let _daemon = crate::nearby::testing::FakeDaemon::start(
                &account.paths,
                crate::nearby::testing::Answer::Refuse(
                    "Your device didn't take this ticket: this ticket was used already: ask for a new one.",
                ),
            );
            let (mut view, identity_dir) = joining(&account);
            let mut document = AutoCommit::new();
            press(&view, &mut document, KEY_EXISTING);
            press(&view, &mut document, KEY_CHOOSE_NEARBY);
            view.sync(&document, &automerge::ROOT);
            *view.ticket.lock().unwrap() = a_code();
            press(&view, &mut document, KEY_CONNECT_NEARBY);
            sync_until(&mut view, &document, "the join to start", |view| {
                view.nearby.is_some() || view.nearby_failure.is_some()
            });
            sync_until(&mut view, &document, "the refusal to arrive", |view| {
                view.nearby.is_none()
            });
            let failure = view
                .nearby_failure
                .clone()
                .expect("the refusal is one a person acts on");
            assert_eq!(failure.cause, crate::nearby::NearbyCause::Used);
            assert_eq!(view.error(), Some(failure.message().as_str()));
            assert_eq!(view.flow.screen(), Screen::Nearby);
            let composition = laid_out(&mut view);
            // The cause is the banner's accessible name, so a screen reader
            // speaks it first and the outcome alone does not hide it.
            let mut names = Vec::new();
            accessible_names(composition, &mut names);
            assert!(
                names.iter().any(|name| name.contains("already used")),
                "{names:?}"
            );
            assert!(
                failure.message().contains("tilo device add"),
                "the message says how to get a new code: {}",
                failure.message()
            );
            assert!(
                accessibility_for_id(composition, "onboarding.connect.nearby.button").is_some(),
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
            let _daemon = crate::nearby::testing::FakeDaemon::start(
                &account.paths,
                crate::nearby::testing::Answer::Wait,
            );
            let (mut view, _) = joining(&account);
            let mut document = AutoCommit::new();
            press(&view, &mut document, KEY_EXISTING);
            press(&view, &mut document, KEY_CHOOSE_NEARBY);
            view.sync(&document, &automerge::ROOT);
            *view.ticket.lock().unwrap() = a_code();
            press(&view, &mut document, KEY_CONNECT_NEARBY);
            sync_until(&mut view, &document, "the join to start", |view| {
                view.nearby.is_some()
            });
            let composition = laid_out(&mut view);
            assert!(
                accessibility_for_id(composition, "onboarding.connect.cancel.button").is_some(),
                "a join under way offers Cancel"
            );
            assert!(
                accessibility_for_id(composition, "onboarding.connect.nearby.button").is_none()
            );
            assert!(
                accessibility_for_id(composition, "onboarding.nearby.status").is_some(),
                "the wait is on screen"
            );
            press(&view, &mut document, KEY_CONNECT_CANCEL);
            view.sync(&document, &automerge::ROOT);
            assert!(view.nearby.is_none());
            assert_eq!(view.error(), Some(t("onboarding-nearby-cancelled").as_str()));
            assert!(view.ticket.lock().unwrap().is_empty());
        }

        phase a_good_code_ends_first_use_on_the_ready_screen;
        {
            let account = crate::nearby::tests::Account::new();
            let _daemon = crate::nearby::testing::FakeDaemon::start(
                &account.paths,
                crate::nearby::testing::Answer::Certify,
            );
            let (mut view, identity_dir) = joining(&account);
            let mut document = AutoCommit::new();
            press(&view, &mut document, KEY_EXISTING);
            press(&view, &mut document, KEY_CHOOSE_NEARBY);
            view.sync(&document, &automerge::ROOT);
            let _ = view.take_structure_dirty();
            assert!(!view.update(&document, &automerge::ROOT), "nothing new yet");

            // The whole line `tilo device add` prints is accepted, not only the code.
            *view.ticket.lock().unwrap() = format!("tilo login {}", a_code());
            press(&view, &mut document, KEY_CONNECT_NEARBY);
            sync_until(&mut view, &document, "the join to start", |view| {
                view.nearby.is_some() || view.nearby_failure.is_some()
            });
            assert!(
                view.ticket.lock().unwrap().is_empty(),
                "the code does not stay in an editable field"
            );
            sync_until(
                &mut view,
                &document,
                "the join to end on the ready screen",
                |view| view.nearby.is_none() && view.identity_ready_page,
            );
            assert_eq!(
                view.nearby_failure, None,
                "the join ended well: {:?}",
                view.error()
            );
            assert_eq!(view.flow.screen(), Screen::Done);
            assert_eq!(view.error(), None);
            assert_eq!(
                progress::recorded(&account.paths),
                Some(Ok(Stage::ReadyToEnter)),
                "a restart resumes at the ready screen"
            );
            let joined_ready = |composition: &dyn Widget| {
                let label = |id| accessibility_for_id(composition, id).and_then(|node| node.name);
                assert_eq!(
                    label("onboarding.done.heading"),
                    Some(tilo_onboarding::t("onboarding-device-added")),
                    "the ready screen confirms the added device"
                );
                assert_eq!(
                    label("onboarding.finish.button"),
                    Some(tilo_onboarding::t("onboarding-finish")),
                    "the ready screen offers the way into the desktop"
                );
                assert!(
                    ui::widget::find_test_id(composition, "onboarding.done.body").is_none(),
                    "the confirmation is one line"
                );
                assert!(
                    accessibility_for_id(composition, "onboarding.backup.skip-acknowledged.box")
                        .is_none(),
                    "a joined device is not asked to back up an identity it does not hold"
                );
            };
            joined_ready(laid_out(&mut view));

            // The joined identity is the account's, so a restart resumes there.
            drop(view);
            let mut resumed = OnboardingView::new_in_session(
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
            assert_eq!(resumed.flow.screen(), Screen::Done);
            joined_ready(laid_out(&mut resumed));
            assert!(act(&mut resumed, &OnboardingEffect::Finish));
            sync_settled(&mut resumed, &document);
            assert!(resumed.finished(), "{:?}", resumed.progress_error);
            assert!(!progress::unfinished(&account.paths));
        }
    }

    /// Set `key`'s acknowledgement to `checked`, through the declared
    /// action the shell applies.
    pub(super) fn acknowledge(doc: &mut AutoCommit, key: &'static str, checked: bool) {
        let kind = if key == KEY_UNDERSTOOD {
            AcknowledgementKind::IdentityProtection
        } else {
            AcknowledgementKind::SkipBackup
        };
        let action = tilo_resource::Stamped {
            operation: acknowledgement(kind, checked),
            now_ms: 0,
        };
        let mut dispatch = ui::local_dispatch::LocalDispatch::new();
        dispatch
            .try_dispatch_with_context(
                doc,
                <tilo_onboarding::Onboarding as tilo_resource::Resource>::ACTION_TYPE,
                &action,
                tilo_actions::ActionContext::default(),
            )
            .expect("an acknowledgement applies to the document");
    }

    pub(super) fn press(view: &OnboardingView, doc: &mut AutoCommit, key: &'static str) {
        // Checkboxes are bound to the document; every other action is a
        // click on the view's button.
        if matches!(key, KEY_UNDERSTOOD | KEY_BACKUP_SKIP_ACKNOWLEDGED) {
            acknowledge(doc, key, true);
        } else {
            view.press(key);
        }
    }

    fn escape_event() -> WindowEvent {
        // winit keeps the platform field private. These inert placeholders
        // match the toolkit and greeter keyboard-event tests.
        let event = ui::synthetic_input::key_event(
            Key::Named(NamedKey::Escape),
            None,
            ElementState::Pressed,
        );
        ui::synthetic_input::keyboard_input(event)
    }

    fn dispatch_escape(view: &mut OnboardingView, document: &mut AutoCommit) -> bool {
        // The setup app takes the key; it is mounted once laid out.
        layout(view);
        ui::testing::Dispatch::new()
            .doc(document)
            .send(view, &escape_event())
            .handled
    }

    fn pointer_event(position: Point) -> WindowEvent {
        ui::synthetic_input::cursor_moved(position.x, position.y)
    }

    use ui::synthetic_input::mouse_press;

    use ui::synthetic_input::mouse_release;

    fn accessible_names(widget: &dyn Widget, names: &mut Vec<String>) {
        if let Some(name) = widget.accessibility().and_then(|node| node.name) {
            names.push(name);
        }
        for child in widget.children() {
            accessible_names(child, names);
        }
    }

    fn accessible_descriptions(widget: &dyn Widget, said: &mut Vec<String>) {
        if let Some(description) = widget.accessibility().and_then(|node| node.description) {
            said.push(description);
        }
        for child in widget.children() {
            accessible_descriptions(child, said);
        }
    }

    fn accessibility_for_id(widget: &dyn Widget, id: &str) -> Option<ui::a11y::AccessNode> {
        ui::widget::find_test_id(widget, id)?.accessibility()
    }

    fn write_fixture_identity(directory: &std::path::Path) {
        write_identity_record(directory, [0xab; 32]);
    }

    /// An identity whose secret derives its public key, so it loads and signs
    /// the way one setup made does. Returns its fingerprint.
    fn write_signing_identity(directory: &std::path::Path) -> String {
        let public = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32])
            .verifying_key()
            .to_bytes();
        write_identity_record(directory, public);
        hex::encode(public)
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

    /// `sync`, then wait for the identity work the frame started and sync
    /// again, as the loop does when the worker wakes it.
    fn sync_settled(view: &mut OnboardingView, document: &AutoCommit) {
        view.sync(document, &automerge::ROOT);
        for _ in 0..16 {
            if view.identity_idle() {
                return;
            }
            wait_for_identity_work(view);
            view.sync(document, &automerge::ROOT);
        }
        panic!("identity work kept coming");
    }

    /// Wait until the identity worker has reported, if one runs.
    fn wait_for_identity_work(view: &OnboardingView) {
        let deadline = Instant::now()
            .checked_add(tilo_test_env::hang_bound())
            .expect("deadline");
        while view
            .identity_work
            .as_ref()
            .is_some_and(|work| !work.reported())
        {
            assert!(Instant::now() < deadline, "identity work never reported");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A handler that blocks until the returned sender releases it, and
    /// records the thread it ran on: the test's stand-in for a mint, import
    /// or export that waits seconds for the disk or a passphrase stretch.
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

    tilo_test_env::scenario! {
        fn identity_work_never_runs_on_the_frame_thread;

        phase minting_never_runs_on_the_frame_thread;
        // Minting, importing and exporting the identity wait for the disk, the
        // secret store and a passphrase stretch. They are taken from a frame on
        // the compositor's loop, so they must never run there: with a handler
        // that never finishes, every frame still returns and the step waits.
        {
            {
                let directory = TempDir::new().expect("identity directory");
                let identity_dir = directory.path().join("identity");
                let (wait, release, ran_on) = blocking_handler();
                let record = identity_dir.join("primary.json");
                let mut view = OnboardingView::with_minter(Box::new(move || {
                    wait();
                    Ok(MintedIdentity {
                        fingerprint: "ab".repeat(32),
                        path: record.clone(),
                    })
                }));
                view.identity_dir = Some(identity_dir);
                let device = Device::new();
                device.runs(&mut view);
                let mut document = AutoCommit::new();
                for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
                    press(&view, &mut document, key);
                    view.sync(&document, &automerge::ROOT);
                }
                assert!(!view.identity_idle(), "the mint is running");
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
                press(&view, &mut document, KEY_BACK);
                for _ in 0..3 {
                    view.sync(&document, &automerge::ROOT);
                }
                assert_eq!(view.flow().screen(), Screen::NoRecovery);
                assert!(view.next_wake_deadline().is_some(), "the loop looks again");
                assert_eq!(
                    device.step(),
                    None,
                    "nothing is kept before the identity exists"
                );

                release.send(()).unwrap();
                sync_settled(&mut view, &document);
                assert_eq!(
                    view.flow().screen(),
                    Screen::Done,
                    "Back waited out the mint"
                );
                assert_eq!(
                    device.step(),
                    Some(Stage::IdentityReady),
                    "the identity's step is recorded once the mint reports"
                );
            }
        }

        phase importing_never_runs_on_the_frame_thread;
        // See [`minting_never_runs_on_the_frame_thread`].
        {
            {
                let directory = TempDir::new().expect("identity directory");
                let identity_dir = directory.path().join("identity");
                let (wait, release, ran_on) = blocking_handler();
                let record = identity_dir.join("primary.json");
                let mut view = OnboardingView::with_handlers(
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
                view.identity_dir = Some(identity_dir);
                let mut document = AutoCommit::new();
                press(&view, &mut document, KEY_EXISTING);
                press(&view, &mut document, KEY_CHOOSE_RESTORE);
                view.connect_path.set("/media/backup/primary.json".into());
                *view.connect_passphrase.lock().unwrap() = "restore passphrase".into();
                sync_settled(&mut view, &document);
                press(&view, &mut document, KEY_IMPORT);
                view.sync(&document, &automerge::ROOT);
                assert!(!view.identity_idle(), "the import is running");
                for _ in 0..3 {
                    view.sync(&document, &automerge::ROOT);
                }
                assert_eq!(view.flow().screen(), Screen::Restore);

                release.send(()).unwrap();
                sync_settled(&mut view, &document);
                assert_ne!(*ran_on.lock().unwrap(), Some(std::thread::current().id()));
                assert_eq!(view.flow().screen(), Screen::Done);
                assert!(
                    view.connect_passphrase.lock().unwrap().is_empty(),
                    "the passphrase is cleared once the import reports"
                );
            }
        }

        phase exporting_never_runs_on_the_frame_thread;
        // See [`minting_never_runs_on_the_frame_thread`].
        {
            {
                let progress = TempDir::new().expect("progress directory");
                let (wait, release, ran_on) = blocking_handler();
                let mut view = OnboardingView::with_all_handlers(
                    Box::new(|| Err("unused".to_string())),
                    Box::new(|_, _| Err("unused".to_string())),
                    Box::new(move |_, _, _| {
                        wait();
                        Ok(())
                    }),
                );
                view.flow.begin();
                view.flow.set_acknowledged(true);
                assert!(view.flow.complete(MintedIdentity {
                    fingerprint: "ef".repeat(32),
                    path: PathBuf::from("/identity/primary.json"),
                }));
                view.identity_dir = Some(progress.path().to_path_buf());
                let device = Device::new();
                device.runs(&mut view);
                let mut document = AutoCommit::new();
                *view.backup_passphrase.lock().unwrap() = "correct horse battery staple".into();
                *view.backup_confirmation.lock().unwrap() = "correct horse battery staple".into();
                press(&view, &mut document, KEY_BACKUP_BROWSE);
                sync_settled(&mut view, &document);
                assert_eq!(
                    view.files_asked().map(|asked| asked.mode),
                    Some(tilo_actions::remote::asked::FilesMode::Save)
                );
                // The person picks where the backup goes in the system's picker;
                // the frame hands the export to its own worker.
                view.files_answered(vec![progress
                    .path()
                    .join("tilo-identity-backup.tilo-identity")
                    .to_string_lossy()
                    .into_owned()]);
                view.sync(&document, &automerge::ROOT);
                assert!(view.files_asked().is_none());
                assert!(!view.identity_idle(), "the export is running");
                for _ in 0..3 {
                    view.sync(&document, &automerge::ROOT);
                }
                assert!(view.backup_saved.is_none());

                release.send(()).unwrap();
                sync_settled(&mut view, &document);
                assert_ne!(*ran_on.lock().unwrap(), Some(std::thread::current().id()));
                assert!(view.backup_saved.is_some());
                assert_eq!(device.step(), Some(Stage::IdentityReadyBackedUp));
            }
        }
    }

    fn finish_identity(view: &mut OnboardingView, document: &mut AutoCommit) {
        for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
            press(view, document, key);
            sync_settled(view, document);
        }
        assert_eq!(view.flow().screen(), Screen::Done);
    }

    tilo_test_env::scenario! {
        fn setup_resumes_where_the_person_was_and_fails_closed;

        phase each_step_is_recorded_once_its_identity_exists_and_removed_at_finish;
        {
            let directory = TempDir::new().expect("identity directory");
            let identity_dir = directory.path().join("identity");
            let calls = Arc::new(AtomicUsize::new(0));
            let mut view = OnboardingView::with_minter(counting_minter(&calls, &identity_dir));
            view.identity_dir = Some(identity_dir);
            let device = Device::new();
            device.runs(&mut view);
            let mut document = AutoCommit::new();
            let mut steps = Vec::new();
            for key in [
                KEY_BEGIN,
                KEY_UNDERSTOOD,
                KEY_CREATE,
                // The keyboard/action route carries the same guard as the
                // disabled button: a stale or malicious document action
                // cannot skip the only recovery decision.
                KEY_BACKUP_CONTINUE,
                KEY_BACKUP_SKIP_ACKNOWLEDGED,
                KEY_BACKUP_CONTINUE,
                KEY_FINISH,
            ] {
                press(&view, &mut document, key);
                sync_settled(&mut view, &document);
                assert_eq!(view.progress_error, None, "after {key}");
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
            assert!(view.finished());
            assert!(!progress::unfinished(&device.paths));
        }

        phase a_person_restarted_at_each_step_carries_on_where_they_were;
        // The person makes their identity, the computer restarts, and setup
        // shows the backup choice again without making another identity; they
        // skip the backup, it restarts again, and the ready screen is what
        // they see; a restart mid-tour reopens the page they were reading,
        // and once they are through, a restart goes to the desktop.
        {
            let directory = TempDir::new().expect("identity directory");
            let identity_dir = directory.path().to_path_buf();
            let calls = Arc::new(AtomicUsize::new(0));
            let minted = Arc::clone(&calls);
            let written = identity_dir.clone();
            let mut view = OnboardingView::with_minter(Box::new(move || {
                minted.fetch_add(1, Ordering::SeqCst);
                Ok(MintedIdentity {
                    fingerprint: write_signing_identity(&written),
                    path: written.join("primary.json"),
                })
            }));
            view.identity_dir = Some(identity_dir.clone());
            let device = Device::new();
            device.runs(&mut view);
            let mut document = AutoCommit::new();
            finish_identity(&mut view, &mut document);
            drop(view);

            let mut backup = device.restarted(&identity_dir);
            assert_eq!(backup.error(), None);
            assert!(
                accessibility_for_id(laid_out(&mut backup), "onboarding.backup.heading").is_some(),
                "the backup choice is offered again"
            );
            assert!(!backup.flow().can_create(), "the identity is not made again");
            let mut document = AutoCommit::new();
            for key in [KEY_BACKUP_SKIP_ACKNOWLEDGED, KEY_BACKUP_CONTINUE] {
                press(&backup, &mut document, key);
                sync_settled(&mut backup, &document);
            }
            assert!(backup.identity_ready_page);
            drop(backup);

            let mut ready = device.restarted(&identity_dir);
            let shown = laid_out(&mut ready);
            assert!(
                accessibility_for_id(shown, "onboarding.backup.heading").is_none(),
                "the backup choice the person made is not asked again"
            );
            assert!(
                accessibility_for_id(shown, "onboarding.finish.button").is_some(),
                "the ready screen is where they left off"
            );
            let mut document = AutoCommit::new();
            for key in [KEY_TOUR_START, KEY_TOUR_NEXT] {
                press(&ready, &mut document, key);
                sync_settled(&mut ready, &document);
            }
            assert_eq!(ready.tour_page, Some(TourPage::Organise));
            drop(ready);

            let mut touring = device.restarted(&identity_dir);
            assert_eq!(
                touring.tour_page,
                Some(TourPage::Organise),
                "the tour opens on the page they were reading"
            );
            assert!(
                accessibility_for_id(laid_out(&mut touring), "onboarding.tour.skip.button")
                    .is_some()
            );
            let mut document = AutoCommit::new();
            press(&touring, &mut document, KEY_TOUR_SKIP);
            sync_settled(&mut touring, &document);
            assert!(touring.finished());
            drop(touring);

            assert_eq!(
                device.restarted(&identity_dir).flow().screen(),
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
            assert_eq!(backup.flow().screen(), Screen::Done);
            assert!(!backup.identity_ready_page);
            let mut blocked_finish = AutoCommit::new();
            press(&backup, &mut blocked_finish, KEY_FINISH);
            sync_settled(&mut backup, &blocked_finish);
            assert!(!backup.finished(), "backup choice cannot be bypassed");

            device.reached(Stage::IdentityReadyBackedUp);
            let backed_up = device.restarted(directory.path());
            assert!(!backed_up.identity_ready_page);
            assert!(
                backed_up.backup_was_verified,
                "a verified backup is not asked for again"
            );

            device.reached(Stage::ReadyToEnter);
            let mut done = device.restarted(directory.path());
            assert_eq!(done.flow().screen(), Screen::Done);
            assert!(done.flow().is_done());
            assert_eq!(
                done.flow()
                    .minted()
                    .map(|identity| identity.fingerprint.as_str()),
                Some("abababababababababababababababababababababababababababababababab")
            );
            assert!(!done.flow().can_create(), "resume never reopens creation");

            let mut document = AutoCommit::new();
            // The ready hand-off is one action; a first session teaches
            // itself on the desktop instead of through pages read in advance.
            press(&done, &mut document, KEY_FINISH);
            sync_settled(&mut done, &document);
            assert!(done.finished());
            assert!(
                !done.presents_desktop_context(),
                "nothing of first login is left over the desktop"
            );
            assert!(!progress::unfinished(&device.paths));

            let established = device.restarted(directory.path());
            assert_eq!(
                established.flow().screen(),
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
                serde_json::Map::from_iter([(
                    "cd".repeat(32),
                    serde_json::json!("identity-ready"),
                )]),
            )
            .expect("another device's step");
            assert!(!progress::unfinished(&device.paths));
            assert_eq!(device.restarted(directory.path()).flow().screen(), Screen::Intro);
        }

        phase recovery_never_resumes_a_step_that_assumes_the_identity_signs;
        // Recovery opens because the identity cannot be used; a step recorded
        // while it could must not put the ready screen over that.
        {
            let directory = TempDir::new().expect("identity directory");
            write_fixture_identity(directory.path());
            let device = Device::new();
            device.reached(Stage::ReadyToEnter);
            let recovery = OnboardingView::new_in_session(
                directory.path().to_path_buf(),
                SessionPaths {
                    identity: Some(device.identity()),
                    ..SessionPaths::default()
                },
                OnboardingContext::FullSystem,
                true,
            );
            assert_ne!(recovery.flow().screen(), Screen::Done);
            assert!(!recovery.identity_ready_page);
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
            assert_eq!(resumed.flow().screen(), Screen::Done);
            assert!(
                !resumed.identity_ready_page,
                "nothing the person had not finished is skipped"
            );
            assert!(resumed.progress_error.is_some());
        }

        phase a_step_the_settings_refuse_holds_setup_where_it_is;
        {
            let directory = TempDir::new().expect("identity directory");
            let identity_dir = directory.path().join("identity");
            let calls = Arc::new(AtomicUsize::new(0));
            let mut view = OnboardingView::with_minter(counting_minter(&calls, &identity_dir));
            view.identity_dir = Some(identity_dir);
            let device = Device::with_settings(Arc::new(settings_store::HostDocument::new(
                serde_json::Map::new(),
                |_| Err(std::io::Error::other("the disk is full")),
            )));
            device.runs(&mut view);
            let mut document = AutoCommit::new();
            finish_identity(&mut view, &mut document);
            assert!(view.progress_error.is_some(), "the refused step is said");
            for key in [KEY_BACKUP_SKIP_ACKNOWLEDGED, KEY_BACKUP_CONTINUE] {
                press(&view, &mut document, key);
                sync_settled(&mut view, &document);
            }
            assert!(
                !view.identity_ready_page,
                "the hand-off waits until a restart would open it there"
            );
            assert!(view.progress_error.is_some());
        }
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
        fn identity_creation_requires_acknowledgement_and_mints_once;

        phase pressing_create_without_acknowledging_mints_nothing;
        // The whole point: clicking through without ticking the box must not produce a key.
        {
            let (mut v, calls) = view();
            let mut doc = AutoCommit::new();
            press(&v, &mut doc, KEY_BEGIN);
            v.sync(&doc, &automerge::ROOT);
            assert_eq!(v.flow().screen(), Screen::NoRecovery);

            press(&v, &mut doc, KEY_CREATE);
            v.sync(&doc, &automerge::ROOT);
            assert_eq!(calls.load(Ordering::SeqCst), 0, "no key was minted");
            assert_eq!(v.flow().screen(), Screen::NoRecovery, "still on the check");
        }

        phase acknowledging_then_creating_mints_exactly_once;
        {
            let (mut v, calls) = view();
            let mut doc = AutoCommit::new();
            press(&v, &mut doc, KEY_BEGIN);
            v.sync(&doc, &automerge::ROOT);
            press(&v, &mut doc, KEY_UNDERSTOOD);
            v.sync(&doc, &automerge::ROOT);
            assert!(v.flow().can_create());
            press(&v, &mut doc, KEY_CREATE);
            v.sync(&doc, &automerge::ROOT);
            // A second sync must not re-fire — the key is consumed.
            v.sync(&doc, &automerge::ROOT);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(v.flow().screen(), Screen::Done);
            assert!(v.flow().is_done());
        }

        phase unticking_the_box_blocks_creation_again;
        {
            let (mut v, calls) = view();
            let mut doc = AutoCommit::new();
            press(&v, &mut doc, KEY_BEGIN);
            v.sync(&doc, &automerge::ROOT);
            press(&v, &mut doc, KEY_UNDERSTOOD);
            v.sync(&doc, &automerge::ROOT);
            acknowledge(&mut doc, KEY_UNDERSTOOD, false);
            v.sync(&doc, &automerge::ROOT);
            assert!(!v.flow().can_create());
            press(&v, &mut doc, KEY_CREATE);
            v.sync(&doc, &automerge::ROOT);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }

        phase a_failed_mint_is_surfaced_rather_than_hidden;
        // A minting failure must be shown, not swallowed into a desktop that silently has no identity.
        {
            let mut v = OnboardingView::with_minter(Box::new(|| Err("disk full".to_string())));
            let mut doc = AutoCommit::new();
            press(&v, &mut doc, KEY_BEGIN);
            v.sync(&doc, &automerge::ROOT);
            press(&v, &mut doc, KEY_UNDERSTOOD);
            v.sync(&doc, &automerge::ROOT);
            press(&v, &mut doc, KEY_CREATE);
            v.sync(&doc, &automerge::ROOT);
            assert_eq!(v.error(), Some("disk full"));
            assert_eq!(
                v.flow().screen(),
                Screen::NoRecovery,
                "a failed write is not a completed identity"
            );
            assert!(!v.flow().is_done());
            press(&v, &mut doc, KEY_FINISH);
            v.sync(&doc, &automerge::ROOT);
            assert!(!v.finished(), "failure must not release the desktop");
        }
    }

    tilo_test_env::scenario! {
        fn desktop_tour_is_navigable_skippable_and_dismissable;

        phase desktop_tour_is_complete_backtrackable_and_finishes_only_at_the_end;
        {
            let (mut view, _) = view();
            let mut doc = AutoCommit::new();
            press(&view, &mut doc, KEY_BEGIN);
            view.sync(&doc, &automerge::ROOT);
            press(&view, &mut doc, KEY_UNDERSTOOD);
            view.sync(&doc, &automerge::ROOT);
            press(&view, &mut doc, KEY_CREATE);
            view.sync(&doc, &automerge::ROOT);
            press(&view, &mut doc, KEY_BACKUP_SKIP_ACKNOWLEDGED);
            view.sync(&doc, &automerge::ROOT);
            press(&view, &mut doc, KEY_BACKUP_CONTINUE);
            view.sync(&doc, &automerge::ROOT);

            press(&view, &mut doc, KEY_TOUR_START);
            view.sync(&doc, &automerge::ROOT);
            assert_eq!(view.tour_page, Some(TourPage::Navigate));
            assert!(!view.finished());

            press(&view, &mut doc, KEY_TOUR_NEXT);
            view.sync(&doc, &automerge::ROOT);
            assert_eq!(view.tour_page, Some(TourPage::Organise));
            press(&view, &mut doc, KEY_TOUR_NEXT);
            view.sync(&doc, &automerge::ROOT);
            assert_eq!(view.tour_page, Some(TourPage::Collaborate));

            press(&view, &mut doc, KEY_FINISH);
            view.sync(&doc, &automerge::ROOT);
            assert!(
                !view.finished(),
                "the tour cannot finish before its hand-off page"
            );

            press(&view, &mut doc, KEY_TOUR_NEXT);
            view.sync(&doc, &automerge::ROOT);
            assert_eq!(view.tour_page, Some(TourPage::Start));

            press(&view, &mut doc, KEY_TOUR_BACK);
            view.sync(&doc, &automerge::ROOT);
            assert_eq!(view.tour_page, Some(TourPage::Collaborate));
            press(&view, &mut doc, KEY_TOUR_NEXT);
            view.sync(&doc, &automerge::ROOT);
            press(&view, &mut doc, KEY_FINISH);
            view.sync(&doc, &automerge::ROOT);
            assert!(view.finished());
        }

        phase every_tour_page_exposes_an_explicit_skip_action;
        {
            let mut view = OnboardingView::new_tour();
            let mut doc = AutoCommit::new();
            view.sync(&doc, &automerge::ROOT);

            for page in [
                TourPage::Navigate,
                TourPage::Organise,
                TourPage::Collaborate,
            ] {
                assert_eq!(view.tour_page, Some(page));
                let node = accessibility_for_id(laid_out(&mut view), "onboarding.tour.skip.button")
                    .expect("tour page exposes Skip tour");
                assert_eq!(node.name.as_deref(), Some("Skip tour"));
                if page != TourPage::Collaborate {
                    press(&view, &mut doc, KEY_TOUR_NEXT);
                    view.sync(&doc, &automerge::ROOT);
                }
            }

            press(&view, &mut doc, KEY_TOUR_SKIP);
            view.sync(&doc, &automerge::ROOT);
            assert!(view.finished(), "Skip tour dismisses a reopened tour");
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
                let mut view = device.restarted(directory.path());
                let mut document = AutoCommit::new();

                assert_eq!(view.tour_page, Some(page));
                press(&view, &mut document, KEY_TOUR_SKIP);
                sync_settled(&mut view, &document);

                assert!(view.finished(), "Skip tour must work from {page:?}");
                assert!(
                    !progress::unfinished(&device.paths),
                    "skipping {page:?} must complete first use for good"
                );
            }
        }

        phase escape_dismisses_a_reopened_tour_from_every_page;
        {
            for next_presses in 0..=3 {
                let mut view = OnboardingView::new_tour();
                let mut document = AutoCommit::new();
                for _ in 0..next_presses {
                    press(&view, &mut document, KEY_TOUR_NEXT);
                    view.sync(&document, &automerge::ROOT);
                }

                assert!(dispatch_escape(&mut view, &mut document));
                view.sync(&document, &automerge::ROOT);
                assert!(
                    view.finished(),
                    "Escape must close a reopened tour after {next_presses} advances"
                );
            }
        }

        phase reopened_tour_does_not_require_an_identity_and_can_be_dismissed;
        {
            let mut view = OnboardingView::new_tour();
            let mut doc = AutoCommit::new();
            assert_eq!(view.tour_page, Some(TourPage::Navigate));
            assert!(
                !view.flow().is_done(),
                "reopening must not forge identity state"
            );

            press(&view, &mut doc, KEY_TOUR_BACK);
            view.sync(&doc, &automerge::ROOT);
            assert!(
                view.finished(),
                "Back on the first page closes a reopened tour"
            );

            let mut view = OnboardingView::new_tour();
            press(&view, &mut doc, KEY_TOUR_NEXT);
            view.sync(&doc, &automerge::ROOT);
            press(&view, &mut doc, KEY_TOUR_NEXT);
            view.sync(&doc, &automerge::ROOT);
            press(&view, &mut doc, KEY_TOUR_NEXT);
            view.sync(&doc, &automerge::ROOT);
            press(&view, &mut doc, KEY_FINISH);
            view.sync(&doc, &automerge::ROOT);
            assert!(view.finished(), "Done closes the reopened tour");
        }
    }

    tilo_test_env::scenario! {
        fn onboarding_copy_and_live_regions_speak_the_real_contract;

        phase a_join_under_way_speaks_through_one_polite_live_region;
        {
            let (mut view, _) = view();
            view.flow.begin_existing();
            view.flow.choose_nearby();
            view.nearby_active_for_snapshot = true;
            let theme = Theme::dark();
            let node = accessibility_for_id(
                laid_out_themed(&mut view, &theme),
                "onboarding.nearby.status",
            )
            .expect("the join's wait has one status region");
            assert_eq!(node.role, ui::a11y::Role::StaticText);
            assert_eq!(node.name, Some(t("onboarding-nearby-joining")));
            assert_eq!(node.live, Some(ui::a11y::Politeness::Polite));
            assert!(
                accessibility_for_id(laid_out_themed(&mut view, &theme), "onboarding.nearby.code")
                    .is_none(),
                "no code from the other device is shown here"
            );
        }

        phase the_code_screen_says_what_to_run_on_the_other_device;
        {
            let body = tilo_onboarding::t("onboarding-nearby-body");
            assert!(body.contains("tilo device add"), "{body}");
            assert!(body.to_lowercase().contains("code"), "{body}");
            let field = tilo_onboarding::t("onboarding-nearby-ticket-field").to_lowercase();
            assert!(field.contains("code"), "{field}");
            assert!(!field.contains("link"), "{field}");
        }

        phase onboarding_copy_uses_no_typed_glyphs_as_layout;
        {
            let copy = [
                "onboarding-intro-point-collaboration",
                "onboarding-intro-point-local",
                "onboarding-intro-point-not-account",
                "onboarding-nearby-body",
                "onboarding-nearby-ticket-field",
                "onboarding-nearby-joining",
                "onboarding-progress-stage",
                "onboarding-tour-eyebrow",
                "onboarding-tour-stage",
                "onboarding-tour-search-title",
                "onboarding-tour-overview-title",
                "onboarding-tour-move-title",
                "onboarding-tour-name-title",
            ]
            .map(tilo_onboarding::t)
            .join(" ");
            for glyph in ['•', '·', '→', '—', '✓', '★'] {
                assert!(!copy.contains(glyph), "{glyph:?} used as layout in {copy}");
            }
        }

        phase host_desktop_copy_does_not_claim_to_replace_the_linux_login;
        {
            let copy = host_desktop_intro_body().to_ascii_lowercase();
            assert!(copy.contains("already signed in"));
            assert!(copy.contains("does not replace your linux login"));
            assert!(copy.contains("does not") && copy.contains("online account"));
            assert!(copy.contains("not all cryptographically signed"));
        }
    }

    tilo_test_env::scenario! {
        fn screen_navigation_follows_context_direction_escape_and_motion_preferences;

        phase host_desktop_context_keeps_the_identity_creation_check;
        {
            let mut view = OnboardingView::with_all_handlers_in_context(
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
            assert_eq!(view.context, OnboardingContext::HostDesktop);
            assert!(!view.flow.can_create());
            view.flow.begin();
            assert!(!view.flow.can_create());
            view.flow.set_acknowledged(true);
            assert!(view.flow.can_create());
        }

        phase escape_follows_back_on_reversible_identity_steps_only;
        {
            let (mut view, _) = view();
            let mut document = AutoCommit::new();

            assert!(
                !dispatch_escape(&mut view, &mut document),
                "intro must not be dismissible before an identity exists"
            );
            press(&view, &mut document, KEY_EXISTING);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Existing);
            press(&view, &mut document, KEY_CHOOSE_RESTORE);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Restore);
            assert!(dispatch_escape(&mut view, &mut document));
            view.sync(&document, &automerge::ROOT);
            assert_eq!(
                view.flow.screen(),
                Screen::Existing,
                "escape leaves a flow for the choice it was opened from"
            );
            assert!(dispatch_escape(&mut view, &mut document));
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Intro);

            press(&view, &mut document, KEY_BEGIN);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::NoRecovery);
            assert!(dispatch_escape(&mut view, &mut document));
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Intro);
        }

        phase recovery_mode_starts_on_restore_and_never_offers_creation;
        {
            let identity = tempfile::tempdir().expect("identity directory");
            let mut recovery = OnboardingView::isolated(identity.path().to_path_buf(), true);
            assert!(recovery.recovery_only);
            assert_eq!(recovery.flow.screen(), Screen::Existing);
            let mut document = AutoCommit::new();
            press(&recovery, &mut document, KEY_BACK);
            recovery.sync(&document, &automerge::ROOT);
            assert_eq!(recovery.flow.screen(), Screen::Existing);
            press(&recovery, &mut document, KEY_CHOOSE_RESTORE);
            recovery.sync(&document, &automerge::ROOT);
            assert_eq!(recovery.flow.screen(), Screen::Restore);
            press(&recovery, &mut document, KEY_BACK);
            recovery.sync(&document, &automerge::ROOT);
            assert_eq!(
                recovery.flow.screen(),
                Screen::Existing,
                "recovery flows still return to their choice"
            );
            assert!(
                accessibility_for_id(laid_out(&mut recovery), "onboarding.connect.retry.button")
                    .is_some(),
                "recovery must expose a keyring retry action"
            );
        }
    }

    tilo_test_env::scenario! {
        fn connecting_an_existing_identity_imports_without_minting;

        phase connect_existing_imports_without_minting;
        {
            let imported = Arc::new(AtomicUsize::new(0));
            let imported_for_handler = imported.clone();
            let mut view = OnboardingView::with_handlers(
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
            let mut doc = AutoCommit::new();
            press(&view, &mut doc, KEY_EXISTING);
            press(&view, &mut doc, KEY_CHOOSE_RESTORE);
            view.connect_path.set("/media/backup/primary.json".into());
            view.sync(&doc, &automerge::ROOT);
            assert_eq!(view.flow().screen(), Screen::Restore);
            press(&view, &mut doc, KEY_IMPORT);
            view.sync(&doc, &automerge::ROOT);
            assert_eq!(imported.load(Ordering::SeqCst), 1);
            assert_eq!(view.flow().screen(), Screen::Done);
            assert!(view.flow().is_done());
        }

        phase connect_records_the_identity_once_imported_and_keeps_setup_pending;
        {
            let directory = TempDir::new().expect("identity directory");
            let identity_dir = directory.path().join("identity");
            let progress_dir = identity_dir.clone();
            let mut view = OnboardingView::with_handlers(
                Box::new(|| Err("create should not run".to_string())),
                Box::new(move |_, _| {
                    Ok(MintedIdentity {
                        fingerprint: "cd".repeat(32),
                        path: progress_dir.join("primary.json"),
                    })
                }),
            );
            view.identity_dir = Some(identity_dir);
            let device = Device::new();
            device.runs(&mut view);
            let mut document = AutoCommit::new();

            press(&view, &mut document, KEY_EXISTING);
            press(&view, &mut document, KEY_CHOOSE_RESTORE);
            view.connect_path.set("/media/backup/primary.json".into());
            sync_settled(&mut view, &document);
            assert_eq!(
                device.step(),
                None,
                "nothing is kept before the identity exists"
            );
            press(&view, &mut document, KEY_IMPORT);
            sync_settled(&mut view, &document);

            assert_eq!(device.step(), Some(Stage::IdentityReady));
            assert_eq!(view.flow().screen(), Screen::Done);
        }

        phase failed_connect_stays_on_connect_screen_and_shows_error;
        {
            let mut view = OnboardingView::with_handlers(
                Box::new(|| Err("create should not run".to_string())),
                Box::new(|_, _| Err("not a Tilo identity".to_string())),
            );
            let mut doc = AutoCommit::new();
            press(&view, &mut doc, KEY_EXISTING);
            press(&view, &mut doc, KEY_CHOOSE_RESTORE);
            view.sync(&doc, &automerge::ROOT);
            view.connect_path
                .set("/media/identity.tilo-identity".into());
            press(&view, &mut doc, KEY_IMPORT);
            view.sync(&doc, &automerge::ROOT);
            assert_eq!(view.flow().screen(), Screen::Restore);
            assert_eq!(view.error(), Some("not a Tilo identity"));
        }

        phase connect_requires_a_selected_file_before_importing;
        {
            let mut view = OnboardingView::with_handlers(
                Box::new(|| Err("create should not run".to_string())),
                Box::new(|_, _| Err("importer must not run without a path".to_string())),
            );
            let mut doc = AutoCommit::new();
            press(&view, &mut doc, KEY_EXISTING);
            press(&view, &mut doc, KEY_CHOOSE_RESTORE);
            view.sync(&doc, &automerge::ROOT);
            let theme = Theme::dark();
            let disabled = accessibility_for_id(
                laid_out_themed(&mut view, &theme),
                "onboarding.import.button",
            )
            .expect("restore action is exposed to accessibility");
            assert!(
                disabled.state.disabled,
                "restore must be visibly and semantically disabled without a selected source"
            );
            press(&view, &mut doc, KEY_IMPORT);
            view.sync(&doc, &automerge::ROOT);
            assert_eq!(view.flow().screen(), Screen::Restore);
            assert_eq!(
                view.error(),
                Some("Choose an encrypted backup or legacy identity file first.")
            );

            view.connect_path
                .set("/media/identity.tilo-identity".into());
            view.sync(&doc, &automerge::ROOT);
            let enabled = accessibility_for_id(
                laid_out_themed(&mut view, &theme),
                "onboarding.import.button",
            )
            .expect("restore action remains exposed");
            assert!(
                !enabled.state.disabled,
                "selecting a source must enable the restore action"
            );
        }

        phase connect_browse_asks_for_the_backup_file;
        {
            let (mut view, _) = view();
            let mut doc = AutoCommit::new();
            press(&view, &mut doc, KEY_EXISTING);
            press(&view, &mut doc, KEY_CHOOSE_RESTORE);
            view.sync(&doc, &automerge::ROOT);
            press(&view, &mut doc, KEY_CONNECT_BROWSE);
            view.sync(&doc, &automerge::ROOT);
            let asked = view.files_asked().expect("the person is asked for a file");
            assert_eq!(
                asked.mode,
                tilo_actions::remote::asked::FilesMode::Open,
                "the backup to open, not a place to write one"
            );

            // Dismissing the question leaves the step where it was, nothing
            // chosen.
            view.files_answered(Vec::new());
            view.sync(&doc, &automerge::ROOT);
            assert!(view.files_asked().is_none());
            assert_eq!(view.flow().screen(), Screen::Restore);
            assert!(view.connect_path.get().is_empty());

            press(&view, &mut doc, KEY_CONNECT_BROWSE);
            view.sync(&doc, &automerge::ROOT);
            view.files_answered(vec!["/home/person/backup.tilo-identity".into()]);
            view.sync(&doc, &automerge::ROOT);
            assert!(view.files_asked().is_none());
            assert_eq!(view.connect_path.get(), "/home/person/backup.tilo-identity");
        }
    }

    thread_local! {
        /// The document a test's frames settle into, as a host's would.
        static FRAMES: RefCell<AutoCommit> = RefCell::new(AutoCommit::new());
    }

    /// One frame as a host runs it: what settled since the last frame goes
    /// into the document, then the view syncs with it.
    fn frame(view: &mut OnboardingView) {
        FRAMES.with(|doc| {
            let mut doc = doc.borrow_mut();
            view.settle(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT);
            view.sync(&*doc, &automerge::ROOT);
        });
    }

    /// What setup's API offers now, after a frame published it.
    fn published(view: &mut OnboardingView) -> Vec<tilo_actions::ActionDescriptor> {
        frame(view);
        view.device_local_api().actions()
    }

    /// Drive setup through its API as a script or an agent on this device
    /// would: queue `effect`, then run frames until the view served it and
    /// what it asked settled. Whether the API took it; what it means on the
    /// page is the ceremony's.
    fn act(view: &mut OnboardingView, effect: &OnboardingEffect) -> bool {
        use tilo_resource::Effect as _;
        let Some(found) = published(view).into_iter().find(|descriptor| {
            descriptor.address.action == effect.id()
                || descriptor
                    .address
                    .action
                    .ends_with(&format!(".{}", effect.id()))
        }) else {
            return false;
        };
        let answer = view
            .device_local_api()
            .invoke(found.address, effect.arguments());
        frame(view);
        frame(view);
        matches!(answer.borrow_mut().take(), Some(Ok(_)))
    }

    /// Whether first use is still under way, as authentication prompts ask.
    fn first_use(view: &OnboardingView) -> bool {
        (view.authentication_first_use_scope())()
    }

    /// The setup action a footer control stands for.
    fn footer(operation: &str) -> OnboardingEffect {
        match operation {
            "backup.continue" | "onboarding.codecs.continue" => OnboardingEffect::Continue,
            "onboarding.finish" => OnboardingEffect::Finish,
            "onboarding.codecs.install" => OnboardingEffect::choose("install-codecs"),
            "onboarding.codecs.skip" => OnboardingEffect::Skip,
            "onboarding.tour.start" => OnboardingEffect::choose("tour"),
            other => panic!("no footer action {other}"),
        }
    }

    /// Continue from the backup step without a backup, saying so first.
    fn continue_without_backup(view: &mut OnboardingView) {
        assert!(act(view, &OnboardingEffect::Acknowledge { checked: true }));
        assert!(act(view, &OnboardingEffect::Continue));
    }

    /// Whether the record at `path` holds a root: a fixture keeps it in the account's own record,
    /// where a daemon keeps it on a computer.
    fn fixture_backup_capability(path: &Path) -> Result<bool, String> {
        tilo_identity::store::load_from_path(path.to_owned())
            .map(|identity| identity.is_single_device())
            .map_err(|error| error.to_string())
    }

    fn real_backup_view(directory: &Path, device: &Device) -> OnboardingView {
        let identity =
            crate::identity::existing_identity_at(&directory.join("primary.json")).unwrap();
        let mut view = OnboardingView::with_all_handlers(
            Box::new(|| Err("unused minter".into())),
            Box::new(|_, _| Err("unused importer".into())),
            Box::new(|identity, destination, passphrase| {
                tilo_identity::store::export_portable_backup(identity, destination, passphrase)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }),
        );
        view.identity_dir = Some(directory.to_owned());
        device.runs(&mut view);
        view.backup_capability = Box::new(fixture_backup_capability);
        if progress::unfinished(&device.paths) {
            view.restore_progress();
            view.refresh_backup_capability().unwrap();
        } else {
            view.flow.begin_existing();
            view.accept_connected_identity(identity).unwrap();
        }
        // Identity work runs off the loop; settle it as a frame loop would
        // before an agent acts.
        sync_settled(&mut view, &AutoCommit::new());
        view
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
                    "onboarding_view::tests::identity_ready_does_not_complete_visible_first_use",
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
            let status =
                tilo_test_env::wait_for("certified identity continuation child exit", || {
                    owned.child.as_mut().unwrap().try_wait().unwrap()
                });
            owned.child.take();
            assert!(status.success());
            return;
        }
        let directory = tilo_identity::store::account_identity_dir_from_env().unwrap();
        let paths = crate::firstboot::collaboration_identity_paths();
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
        let device = Device::new();
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
        let mut view = real_backup_view(&directory, &device);
        assert!(
            first_use(&view),
            "first-use service survives identity activation"
        );
        let backup = root.path().join("recovery.tilo-identity");
        let passphrase = "synthetic private backup passphrase";
        assert!(!save_backup(&mut view, &backup, passphrase, "different"));
        assert!(!backup.exists());
        assert!(save_backup(&mut view, &backup, passphrase, passphrase));
        // The backup is written off the loop; take its report as a frame would.
        sync_settled(&mut view, &AutoCommit::new());
        let proof =
            tilo_identity::store::verify_portable_backup(&backup, passphrase.as_bytes()).unwrap();
        assert_eq!(
            proof.group_public_key,
            tilo_identity::store::load_from_path(directory.join("primary.json"))
                .unwrap()
                .group_public_key()
        );
        assert_eq!(device.step(), Some(Stage::IdentityReadyBackedUp));
        let status = view.backup_status().unwrap();
        assert_eq!(status["state"], "verified");
        assert!(!status.to_string().contains(passphrase));
        assert!(!status.to_string().contains(backup.to_str().unwrap()));
        assert!(!format!("{:?}", published(&mut view)).contains(passphrase));
        let doc = AutoCommit::new();
        assert!(act(&mut view, &OnboardingEffect::Continue));
        sync_settled(&mut view, &doc);
        assert!(view.identity_ready_page);
        assert!(first_use(&view));
        assert_eq!(device.step(), Some(Stage::ReadyToEnterBackedUp));
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            original
        );
        let before = std::fs::read(&backup).unwrap();
        save_backup(&mut view, &backup, passphrase, passphrase);
        assert_eq!(std::fs::read(&backup).unwrap(), before);
        // Actual loaded recovery state preserves the verified backup after restart.
        let mut reopened = real_backup_view(&directory, &device);
        reopened.restore_progress();
        assert!(reopened.backup_was_verified);
        assert!(reopened.identity_ready_page);
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
        let mut view = real_backup_view(&directory, &device);
        assert!(act(&mut view, &OnboardingEffect::Continue));
        assert!(!view.identity_ready_page);
        assert!(first_use(&view));
        continue_without_backup(&mut view);
        let doc = AutoCommit::new();
        sync_settled(&mut view, &doc);
        assert!(view.identity_ready_page);
        assert!(!view.backup_was_verified);
        assert!(first_use(&view));
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

    fn invoke_footer(view: &mut OnboardingView, operation: &str, doc: &AutoCommit) {
        assert!(act(view, &footer(operation)));
        sync_settled(view, doc);
    }

    /// Save a backup through setup's API as the person does: type the new
    /// passphrase twice, ask for a file, name where it goes.
    fn save_backup(
        view: &mut OnboardingView,
        destination: &Path,
        passphrase: &str,
        confirmation: &str,
    ) -> bool {
        for (field, value) in [
            ("new-passphrase", passphrase),
            ("new-passphrase-confirmation", confirmation),
        ] {
            assert!(act(
                view,
                &OnboardingEffect::EnterSecret {
                    field: field.into(),
                    value: value.into(),
                }
            ));
        }
        assert!(act(view, &OnboardingEffect::Browse));
        if view
            .files_asked()
            .is_none_or(|asked| asked.mode != tilo_actions::remote::asked::FilesMode::Save)
        {
            return false;
        }
        view.files_answered(vec![destination.to_string_lossy().into_owned()]);
        true
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
        let mut view = real_backup_view(&directory, &device);
        let doc = AutoCommit::new();
        assert!(view.identity_ready_page);
        assert!(first_use(&view));
        // The ready screen hands off to the desktop and nothing else. A first
        // session learns the desktop by using it; the overlay hints and the
        // desktop tour reopen from Search when someone asks for them.
        act(&mut view, &footer("onboarding.tour.start"));
        assert_eq!(
            view.tour_step(),
            None,
            "the ready screen offers no tour to press"
        );
        invoke_footer(&mut view, "onboarding.finish", &doc);
        assert!(view.finished());
        assert!(!first_use(&view));
        assert_eq!(device.step(), None);
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            record
        );
        act(&mut view, &footer("onboarding.finish"));
        assert!(view.finished());
        assert!(!first_use(&view));
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
        let mut view = real_backup_view(&directory, &device)
            .with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
        continue_without_backup(&mut view);
        let doc = AutoCommit::new();
        sync_settled(&mut view, &doc);
        assert!(view.codec_page_visible());
        assert!(first_use(&view));
        invoke_footer(&mut view, "onboarding.codecs.skip", &doc);
        assert!(!view.codec_page_visible());
        assert!(fake.runs().is_empty());
        assert!(first_use(&view));
        act(&mut view, &footer("onboarding.codecs.install"));
        assert!(
            fake.runs().is_empty(),
            "an install from a page no longer shown starts nothing"
        );
        invoke_footer(&mut view, "onboarding.finish", &doc);
        assert!(view.finished());
        assert!(!first_use(&view));
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
        let mut view = real_backup_view(&directory, &device).with_codec_setup(setup);
        continue_without_backup(&mut view);
        let doc = AutoCommit::new();
        sync_settled(&mut view, &doc);
        assert!(view.codec_page_visible());
        invoke_footer(&mut view, "onboarding.codecs.install", &doc);
        assert_eq!(
            tilo_test_env::recv("the codec install to start", &started),
            CodecAction::Install
        );
        assert!(view.codec_running.is_some());
        act(&mut view, &footer("onboarding.codecs.install"));
        assert!(
            started.try_recv().is_err(),
            "a running install is not started again"
        );
        invoke_footer(&mut view, "onboarding.codecs.continue", &doc);
        assert!(!view.codec_page_visible());
        assert!(first_use(&view));
        release.send(()).unwrap();
        invoke_footer(&mut view, "onboarding.finish", &doc);
        assert!(view.finished());
        assert!(!first_use(&view));
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
                tilo_identity::store::public_member_record("Existing person", [128; 3], public).unwrap();
            let path = directory.join("primary.json");
            write_private_member_fixture(&directory, &record);
            let mut view = OnboardingView::with_all_handlers(
                Box::new(move || crate::identity::existing_identity_at(&path)),
                Box::new(|_, _| Err("unused importer".into())),
                Box::new(|_, _, _| panic!("a public member must never export a personal backup")),
            );
            view.identity_dir = Some(directory.clone());
            let device = Device::new();
            device.runs(&mut view);
            view.backup_capability = Box::new(fixture_backup_capability);
            view.recovery_only = true;
            view.flow.begin_existing();
            view.press(KEY_CONNECT_RETRY);
            let doc = AutoCommit::new();
            sync_settled(&mut view, &doc);
            assert!(view.flow.is_done());
            assert!(view.identity_ready_page);
            assert!(!view.backup_export_available);
            assert!(!view.backup_was_verified);
            assert!(view.error.is_none());
            assert_eq!(
                device.step(),
                Some(Stage::ReadyToEnter)
            );
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
                tilo_identity::store::public_member_record("Paired person", [128; 3], public).unwrap();
            write_private_member_fixture(&directory, &record);
            let device = Device::new();
            let mut view = real_backup_view(&directory, &device);
            assert!(!view.backup_export_available);
            assert!(
                view.identity_ready_page,
                "actual connected acquisition skips the irrelevant backup step"
            );
            assert!(!view.backup_was_verified);
            assert!(first_use(&view));
            assert!(view.backup_status().is_none());
            let theme = Theme::dark();
            let composition = laid_out_themed(&mut view, &theme);
            assert!(accessibility_for_id(composition, "onboarding.backup.passphrase").is_none());
            assert!(accessibility_for_id(composition, "onboarding.backup.button").is_none());
            assert_eq!(
                device.step(),
                Some(Stage::ReadyToEnter)
            );
            assert_eq!(
                std::fs::read(directory.join("primary.json")).unwrap(),
                record
            );
            let mut reopened = real_backup_view(&directory, &device);
            reopened.restore_progress();
            reopened.refresh_backup_capability().unwrap();
            assert!(reopened.identity_ready_page);
            assert!(!reopened.backup_was_verified);
            // Existing tutorial progress survives the same canonical capability refresh.
            reopened.tour_page = Some(TourPage::Organise);
            reopened.refresh_backup_capability().unwrap();
            assert_eq!(reopened.tour_page, Some(TourPage::Organise));
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
            let mut view = OnboardingView::with_all_handlers(
                Box::new(|| Err("unused".to_string())),
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
            view.flow.begin();
            view.flow.set_acknowledged(true);
            assert!(view.flow.complete(MintedIdentity {
                fingerprint: "ef".repeat(32),
                path: PathBuf::from("/identity/primary.json"),
            }));
            view.identity_dir = Some(progress.path().to_path_buf());
            let device = Device::new();
            device.runs(&mut view);
            let mut doc = AutoCommit::new();

            *view.backup_passphrase.lock().unwrap() = "correct horse battery staple".into();
            *view.backup_confirmation.lock().unwrap() = "does not match".into();
            press(&view, &mut doc, KEY_BACKUP_BROWSE);
            sync_settled(&mut view, &doc);
            assert!(view.files_asked().is_none());
            assert!(
                view.backup_error
                    .as_deref()
                    .unwrap()
                    .contains("do not match")
            );

            *view.backup_confirmation.lock().unwrap() = "correct horse battery staple".into();
            press(&view, &mut doc, KEY_BACKUP_BROWSE);
            sync_settled(&mut view, &doc);
            view.files_answered(Vec::new());
            sync_settled(&mut view, &doc);
            assert!(view.files_asked().is_none());
            assert_eq!(exports.load(Ordering::SeqCst), 0);

            press(&view, &mut doc, KEY_BACKUP_BROWSE);
            sync_settled(&mut view, &doc);
            view.files_answered(vec![progress
                .path()
                .join("tilo-identity-backup.tilo-identity")
                .to_string_lossy()
                .into_owned()]);
            view.sync(&doc, &automerge::ROOT);
            assert!(view.files_asked().is_none());
            sync_settled(&mut view, &doc);
            assert_eq!(exports.load(Ordering::SeqCst), 1);
            assert!(view.backup_saved.is_some());
            assert!(view.backup_error.is_none());
            assert_eq!(
                device.step(),
                Some(Stage::IdentityReadyBackedUp)
            );
            assert!(view.backup_passphrase.lock().unwrap().is_empty());
            assert!(view.backup_confirmation.lock().unwrap().is_empty());

            press(&view, &mut doc, KEY_BACKUP_CONTINUE);
            sync_settled(&mut view, &doc);
            assert!(view.identity_ready_page);
            assert_eq!(
                device.step(),
                Some(Stage::ReadyToEnterBackedUp)
            );
        }

        phase existing_identity_backup_entry_reuses_the_verified_backup_screen;
        {
            let mut view = OnboardingView::with_all_handlers(
                Box::new(|| Err("unused".to_string())),
                Box::new(|_, _| Err("unused".to_string())),
                Box::new(|_, _, _| Ok(())),
            );

            view.begin_portable_backup_for(MintedIdentity {
                fingerprint: "ab".repeat(32),
                path: PathBuf::from("/identity/primary.json"),
            });

            assert_eq!(view.flow().screen(), Screen::Done);
            assert!(!view.identity_ready_page);
            assert!(!view.backup_was_verified);
            assert!(!view.finished());
            let theme = Theme::dark();
            let composition = laid_out_themed(&mut view, &theme);
            assert!(
                accessibility_for_id(composition, "onboarding.backup.button").is_some(),
                "the existing identity route must expose the same shell-owned export control"
            );
        }

        phase verified_existing_identity_backup_returns_directly_to_desktop;
        {
            let progress = TempDir::new().expect("backup progress directory");
            let mut view = OnboardingView::with_all_handlers(
                Box::new(|| Err("unused".to_string())),
                Box::new(|_, _| Err("unused".to_string())),
                Box::new(|_, _, _| Ok(())),
            );
            view.begin_portable_backup_for(MintedIdentity {
                fingerprint: "cd".repeat(32),
                path: PathBuf::from("/identity/primary.json"),
            });
            view.identity_dir = Some(progress.path().to_path_buf());
            let device = Device::new();
            device.runs(&mut view);
            view.backup_was_verified = true;
            device.reached(Stage::IdentityReadyBackedUp);

            let mut doc = AutoCommit::new();
            press(&view, &mut doc, KEY_BACKUP_CONTINUE);
            sync_settled(&mut view, &doc);

            assert!(view.finished());
            assert!(!view.identity_ready_page);
            assert!(view.tour_page.is_none());
            assert!(
                !progress::unfinished(&device.paths),
                "leaving Settings after verification must not resurrect first-login setup"
            );
        }

        phase verified_backup_reveals_copy_form_only_after_the_optional_action;
        {
            let (mut view, _) = view();
            view.set_visual_state_for_snapshot(OnboardingVisualState::BackupVerified);
            assert!(!view.backup_copy_form_open);

            let mut document = AutoCommit::new();
            press(&view, &mut document, KEY_BACKUP_ANOTHER);
            view.sync(&document, &automerge::ROOT);

            assert!(
                view.backup_copy_form_open,
                "the optional copy action, rather than the verified state itself, reveals passphrase inputs"
            );
        }
    }

    /// A fake helper: `status` is the status file, and the runner rewrites it
    /// the way the unit would before returning `outcome`.
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

    fn walk_to_codecs(view: &mut OnboardingView, doc: &mut AutoCommit) {
        for key in [
            KEY_BEGIN,
            KEY_UNDERSTOOD,
            KEY_CREATE,
            KEY_BACKUP_SKIP_ACKNOWLEDGED,
            KEY_BACKUP_CONTINUE,
        ] {
            press(view, doc, key);
            view.sync(doc, &automerge::ROOT);
        }
    }

    /// `view` laid out in a `width`×`height` window and drawn, as a frame
    /// shows it: the setup app has composed and laid out the page the
    /// ceremony stands on. Every rect is in the window's space.
    fn lay_out_as_root(
        view: &mut OnboardingView,
        width: f32,
        height: f32,
        theme: &Theme,
    ) -> LaidOut {
        fn collect(widget: &mut dyn Widget, out: &mut Vec<*mut dyn Widget>) {
            let pointer: *mut (dyn Widget + '_) = std::ptr::from_mut(widget);
            // SAFETY: only the pointer's lifetime bound changes; the map is
            // read while the tree it was taken from stands, as every
            // `build_layout` map is.
            #[allow(
                clippy::transmute_ptr_to_ptr,
                reason = "a cast cannot widen a trait object's lifetime bound"
            )]
            out.push(unsafe {
                std::mem::transmute::<*mut (dyn Widget + '_), *mut dyn Widget>(pointer)
            });
            for child in widget.children_mut() {
                collect(child, out);
            }
        }
        let mut text = ui::text_render::TextRenderer::new();
        let mut laid =
            crate::view::tests::layout::draw_until_rest(view, width, height, theme, &mut text, 6);
        // The app lays out its own tree in its own frame: its widgets join
        // the map under nodes of their own, at the window's origin as the
        // app is.
        let mut widgets = Vec::new();
        if let Some(root) = view.app.root_mut() {
            collect(root, &mut widgets);
        }
        for (index, widget) in (0_u64..).zip(widgets) {
            laid.nodes
                .insert(NodeId::from(u64::MAX.wrapping_sub(index)), widget);
        }
        laid
    }

    /// The setup app's root, as its last frame composed it.
    fn ceremony(view: &mut OnboardingView) -> &dyn Widget {
        view.app.root_mut().expect("the setup app is mounted")
    }

    /// The first widget under `widget` that `matches`, depth first.
    fn find_widget<'a>(
        widget: &'a dyn Widget,
        matches: &dyn Fn(&dyn Widget) -> bool,
    ) -> Option<&'a dyn Widget> {
        if matches(widget) {
            return Some(widget);
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| find_widget(child, matches))
    }

    fn layout(view: &mut OnboardingView) {
        let _ = lay_out_as_root(view, 1280.0, 800.0, &Theme::dark());
    }

    /// The first scroll region under `widget`.
    fn scroller(widget: &mut dyn Widget) -> Option<&mut Scrollable> {
        if widget.name() == "Scrollable" {
            return widget.as_any_mut()?.downcast_mut::<Scrollable>();
        }
        widget.children_mut().into_iter().find_map(scroller)
    }

    /// The setup card's own rect, inside the app's surface.
    fn panel_rect(view: &mut OnboardingView) -> Rect {
        ui::widget::find_test_id(ceremony(view), "onboarding")
            .and_then(|root| root.children().first().map(|card| card.get_rect()))
            .expect("the setup card is laid out")
    }

    /// The setup app's own layout of the page in `theme`.
    fn laid_out_themed<'a>(view: &'a mut OnboardingView, theme: &Theme) -> &'a dyn Widget {
        let _ = lay_out_as_root(view, 1280.0, 800.0, theme);
        ceremony(view)
    }

    /// The setup app's viewport gutter and the intro card's height cap.
    const VIEWPORT_GUTTER: f32 = 24.0;
    const INTRO_PANEL_HEIGHT: f32 = 520.0;

    /// The setup app composing the page the view stands on.
    fn laid_out(view: &mut OnboardingView) -> &dyn Widget {
        laid_out_themed(view, &Theme::dark())
    }

    /// The label of the control a focus reset lands on.
    #[allow(
        clippy::redundant_closure_for_method_calls,
        reason = "the method path is not general over the widget's lifetime"
    )]
    fn default_focus_label(view: &mut OnboardingView) -> Option<String> {
        find_widget(ceremony(view), &|widget| widget.is_default_keyboard_focus())
            .and_then(|widget| widget.accessibility())
            .and_then(|node| node.name)
    }

    fn stage_label(view: &mut OnboardingView) -> Option<String> {
        accessibility_for_id(ceremony(view), "onboarding.progress.stage").and_then(|node| node.name)
    }

    tilo_test_env::scenario! {
        fn the_codecs_step_installs_skips_and_reports_what_the_person_chose;

        phase the_codecs_step_is_one_install_button_with_nothing_chosen;
        {
            let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
            let (view, _) = view();
            let mut view = view.with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
            let mut doc = AutoCommit::new();
            walk_to_codecs(&mut view, &mut doc);
            assert!(view.codec_page_visible());
            layout(&mut view);
            assert_eq!(stage_label(&mut view).as_deref(), Some("Step 4 of 5"));
            let lines = view.codec_page_lines();
            let note = accessibility_for_id(ceremony(&mut view), "onboarding.codecs.note")
                .expect("the formats source explanation must be accessible");
            assert_eq!(note.name.as_deref(), Some(codecs_note().as_str()));
            assert_eq!(
                lines,
                [
                    "Play more video and audio formats",
                    "Downloaded to this device from the formats' publishers. You can turn it off in Settings.",
                    "Install",
                    "Not now",
                ],
                "a title, one short line, the Install button and a way past it; no essay"
            );
            assert_eq!(
                default_focus_label(&mut view).as_deref(),
                Some("Install"),
                "Enter is the same deliberate press as clicking Install"
            );
            assert!(fake.runs().is_empty(), "reaching the step installs nothing");
            // The step blocks neither the tour nor finishing only by accident:
            // both wait until the person chose.
            press(&view, &mut doc, KEY_FINISH);
            view.sync(&doc, &automerge::ROOT);
            assert!(!view.finished());
            assert!(view.codec_page_visible());
        }

        phase installing_from_setup_records_the_choice_and_shows_the_result;
        {
            let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
            let (view, _) = view();
            let mut view = view.with_codec_setup(fake.setup(
                r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"enabled"},"desired":true}"#,
                Ok(()),
            ));
            let mut doc = AutoCommit::new();
            walk_to_codecs(&mut view, &mut doc);
            press(&view, &mut doc, KEY_CODECS_INSTALL);
            view.sync(&doc, &automerge::ROOT);
            view.wait_for_codec_install();
            view.sync(&doc, &automerge::ROOT);
            assert_eq!(fake.runs(), vec![CodecAction::Install]);
            assert!(
                view.codec_page_visible(),
                "Install never moves past the page by itself"
            );
            assert_eq!(view.codec_status().state, PackState::Installed);
            layout(&mut view);
            let lines = view.codec_page_lines();
            assert!(lines.contains(&"More video and audio formats are on".to_string()));
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("Sign out and back in")),
                "{lines:?}"
            );
            assert_eq!(lines.last().map(String::as_str), Some("Continue"));
            press(&view, &mut doc, KEY_CODECS_CONTINUE);
            view.sync(&doc, &automerge::ROOT);
            assert!(!view.codec_page_visible());
            layout(&mut view);
            assert_eq!(stage_label(&mut view).as_deref(), Some("Step 5 of 5"));
            press(&view, &mut doc, KEY_FINISH);
            view.sync(&doc, &automerge::ROOT);
            assert!(view.finished());
        }

        phase not_now_installs_nothing_and_continues;
        {
            let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
            let (view, _) = view();
            let mut view = view.with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
            let mut doc = AutoCommit::new();
            walk_to_codecs(&mut view, &mut doc);
            press(&view, &mut doc, KEY_CODECS_SKIP);
            view.sync(&doc, &automerge::ROOT);
            assert!(!view.codec_page_visible());
            assert!(fake.runs().is_empty());
            press(&view, &mut doc, KEY_FINISH);
            view.sync(&doc, &automerge::ROOT);
            assert!(view.finished());
        }

        phase setup_continues_while_the_download_runs;
        {
            let (release, check) = std::sync::mpsc::channel::<()>();
            let check = Mutex::new(check);
            let (started_tx, started) = std::sync::mpsc::channel::<()>();
            let started_tx = Mutex::new(started_tx);
            let status = Arc::new(Mutex::new(r#"{"state":"not-installed"}"#.to_string()));
            let reader = Arc::clone(&status);
            let writer = Arc::clone(&status);
            let (view, _) = view();
            let mut view = view.with_codec_setup(CodecSetup::with(
                Arc::new(move || CodecPackStatus::parse(&reader.lock().unwrap())),
                Arc::new(move |_| {
                    *writer.lock().unwrap() = r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":7,"label":"Downloading"}}"#.to_string();
                    let _ = started_tx.lock().unwrap().send(());
                    let _ = check.lock().unwrap().recv();
                    Ok(())
                }),
            ));
            let mut doc = AutoCommit::new();
            walk_to_codecs(&mut view, &mut doc);
            press(&view, &mut doc, KEY_CODECS_INSTALL);
            view.sync(&doc, &automerge::ROOT);
            // Nothing wakes the compositor for the worker, so the view does.
            let limit = Instant::now().checked_add(BACKGROUND_POLL).unwrap();
            assert!(
                view.next_wake_deadline()
                    .is_some_and(|deadline| deadline <= limit),
                "a running install keeps looking for its result"
            );
            // The helper reports progress through its status file.
            started.recv().unwrap();
            view.codec_changed.store(true, Ordering::Release);
            assert!(
                view.needs_animation_frame(),
                "a status change asks for a frame"
            );
            view.sync(&doc, &automerge::ROOT);
            let lines = view.codec_page_lines();
            assert_eq!(view.codec_status().state, PackState::Installing);
            assert!(lines.contains(&"Installing more formats".to_string()));
            assert!(lines.contains(&"29%".to_string()), "{lines:?}");
            assert!(
                lines.iter().any(|line| line.contains("Setup can continue")),
                "{lines:?}"
            );
            assert_eq!(lines.last().map(String::as_str), Some("Continue"));
            press(&view, &mut doc, KEY_CODECS_CONTINUE);
            view.sync(&doc, &automerge::ROOT);
            assert!(
                !view.codec_page_visible(),
                "the download does not hold setup"
            );
            press(&view, &mut doc, KEY_FINISH);
            view.sync(&doc, &automerge::ROOT);
            assert!(view.finished());
            release.send(()).unwrap();
        }

        phase an_offline_choice_is_kept_and_installs_later;
        {
            let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
            let (view, _) = view();
            let mut view = view.with_codec_setup(
                fake.setup(r#"{"state":"waiting-for-network","desired":true}"#, Ok(())),
            );
            let mut doc = AutoCommit::new();
            walk_to_codecs(&mut view, &mut doc);
            press(&view, &mut doc, KEY_CODECS_INSTALL);
            view.sync(&doc, &automerge::ROOT);
            view.wait_for_codec_install();
            view.sync(&doc, &automerge::ROOT);
            let lines = view.codec_page_lines();
            assert!(lines.contains(&"Installs once you are online".to_string()));
            assert!(
                lines
                    .iter()
                    .any(|line| line.contains("Your choice is saved"))
            );
            assert_eq!(lines.last().map(String::as_str), Some("Continue"));
            layout(&mut view);
        }

        phase a_refused_install_is_shown_inline_with_retry_and_skip;
        {
            let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
            let (view, _) = view();
            let mut view = view.with_codec_setup(fake.setup(
                r#"{"state":"not-installed"}"#,
                Err("Access denied: not an administrator".into()),
            ));
            let mut doc = AutoCommit::new();
            walk_to_codecs(&mut view, &mut doc);
            press(&view, &mut doc, KEY_CODECS_INSTALL);
            view.sync(&doc, &automerge::ROOT);
            view.wait_for_codec_install();
            view.sync(&doc, &automerge::ROOT);
            let lines = view.codec_page_lines();
            assert!(lines.contains(&"Could not install more formats".to_string()));
            assert!(lines.contains(&"Access denied: not an administrator".to_string()));
            assert_eq!(&lines[lines.len() - 2..], ["Try again", "Skip"]);
            layout(&mut view);
            press(&view, &mut doc, KEY_CODECS_INSTALL);
            view.sync(&doc, &automerge::ROOT);
            view.wait_for_codec_install();
            assert_eq!(
                fake.runs(),
                vec![CodecAction::Install, CodecAction::Install]
            );
        }

        phase a_choice_made_in_the_installer_is_not_asked_again;
        {
            let fake = FakeCodecs::new(r#"{"state":"waiting-for-network","desired":true}"#);
            let (view, _) = view();
            let mut view = view.with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
            let mut doc = AutoCommit::new();
            walk_to_codecs(&mut view, &mut doc);
            assert!(!view.codec_page_visible());
            layout(&mut view);
            assert_eq!(stage_label(&mut view).as_deref(), Some("Step 4 of 4"));
        }
    }

    tilo_test_env::scenario! {
        fn the_existing_identity_choices_are_cards_in_view_that_open_their_own_screens;

        phase each_choice_screen_offers_cards_that_stay_in_view;
        {
            for (keys, cards) in [
                (
                    &[][..],
                    &["onboarding.existing.card", "onboarding.begin.card"][..],
                ),
                (
                    &[KEY_EXISTING][..],
                    &[
                        "onboarding.existing.nearby.card",
                        "onboarding.existing.recovery-key.card",
                        "onboarding.existing.restore.card",
                    ][..],
                ),
            ] {
                for (width, height, theme) in [
                    (1366.0, 768.0, Theme::dark()),
                    (800.0, 600.0, Theme::dark()),
                    (1280.0, 720.0, Theme::dark().with_text_scale(1.3)),
                    (1280.0, 720.0, Theme::high_contrast().with_ui_scale(1.25)),
                ] {
                    let (mut view, _) = view();
                    let mut document = AutoCommit::new();
                    for key in keys {
                        press(&view, &mut document, key);
                        view.sync(&document, &automerge::ROOT);
                    }
                    let LaidOut {
                        nodes: node_map, ..
                    } = lay_out_as_root(&mut view, width, height, &theme);
                    let panel = panel_rect(&mut view);
                    let scroll = node_map
                        .values()
                        .find_map(|&pointer| unsafe {
                            ((*pointer).name() == "Scrollable").then(|| (*pointer).get_rect())
                        })
                        .expect("choice page scroll viewport");
                    let mut previous: Option<Rect> = None;
                    for id in cards {
                        let (rect, focusable) = node_map
                            .values()
                            .find_map(|&pointer| unsafe {
                                ((*pointer).test_id() == Some(id))
                                    .then(|| ((*pointer).get_rect(), (*pointer).is_focusable()))
                            })
                            .unwrap_or_else(|| panic!("choice screen exposes {id}"));
                        assert!(focusable, "{id} must be a keyboard focus stop");
                        assert!(
                            rect.height() >= f64::from(theme.tokens.metrics.min_pointer_target)
                                && rect.x0 >= panel.x0 + 8.0
                                && rect.x1 <= panel.x1 - 8.0
                                && rect.y0 >= scroll.y1
                                && rect.y1 <= panel.y1 - 8.0,
                            "{id} must stay wholly visible below the explanation at {width}x{height}: card={rect:?}, scroll={scroll:?}, panel={panel:?}, theme={}",
                            theme.name,
                        );
                        if let Some(previous) = previous {
                            assert!(
                                (rect.y0 - previous.y1).abs() < 0.5,
                                "the choices are one list, edge to edge"
                            );
                            assert!((rect.x0 - previous.x0).abs() < 0.5, "rows share an edge");
                        }
                        previous = Some(rect);
                        let node =
                            accessibility_for_id(ceremony(&mut view), id).expect("row semantics");
                        assert_eq!(node.role, ui::a11y::Role::ListItem);
                        // The card's description is said once: as the row's description, or
                        // after its title in its name, and never again among its values.
                        let said = node
                            .description
                            .as_deref()
                            .filter(|text| !text.is_empty())
                            .or_else(|| {
                                node.name
                                    .as_deref()
                                    .and_then(|name| name.split_once(", "))
                                    .map(|(_, rest)| rest)
                            })
                            .unwrap_or_else(|| panic!("{id} announces its description"));
                        assert!(
                            !node.value.as_deref().unwrap_or_default().contains(said),
                            "{id} says its description once: {node:?}"
                        );
                    }
                }
            }
        }

        phase use_existing_opens_a_choice_and_each_choice_its_own_screen;
        {
            let (mut view, _) = view();
            let mut document = AutoCommit::new();
            press(&view, &mut document, KEY_EXISTING);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Existing);
            let theme = Theme::dark();
            let composition = laid_out_themed(&mut view, &theme);
            assert!(
                accessibility_for_id(composition, "onboarding.connect.path.field").is_none(),
                "the choice screen is not the restore form"
            );

            press(&view, &mut document, KEY_CHOOSE_RESTORE);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Restore);
            let composition = laid_out_themed(&mut view, &theme);
            assert!(accessibility_for_id(composition, "onboarding.connect.path.field").is_some());
            assert!(
                accessibility_for_id(composition, "onboarding.nearby.status").is_none(),
                "restore does not carry the nearby flow"
            );
            press(&view, &mut document, KEY_BACK);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Existing);

            // The code's screen opens on its own; without an identity
            // directory to write into, Join says that and nothing is called.
            press(&view, &mut document, KEY_CHOOSE_NEARBY);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Nearby);
            assert_eq!(view.error(), None, "opening the screen is not a failure");
            let composition = laid_out_themed(&mut view, &theme);
            assert!(
                accessibility_for_id(composition, "onboarding.connect.path.field").is_none(),
                "nearby does not carry the restore form"
            );
            assert!(
                accessibility_for_id(composition, "onboarding.nearby.ticket.field").is_some(),
                "the screen asks for the code"
            );
            assert!(
                accessibility_for_id(composition, "onboarding.connect.nearby.button").is_some(),
                "the Join control is offered"
            );
            *view.ticket.lock().unwrap() = "K7Q-MX2P".to_owned();
            press(&view, &mut document, KEY_CONNECT_NEARBY);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(
                view.error(),
                Some(tilo_onboarding::t("onboarding-nearby-unavailable").as_str()),
                "this session cannot add a computer"
            );
            press(&view, &mut document, KEY_BACK);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Existing);
            assert!(
                view.error().is_none(),
                "back leaves the flow's error behind"
            );
            press(&view, &mut document, KEY_BACK);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Intro);
        }

        phase nearby_shows_cancel_while_a_join_runs;
        {
            let (mut view, _) = view();
            view.set_visual_state_for_snapshot(OnboardingVisualState::NearbyJoining);
            let theme = Theme::dark();
            let composition = laid_out_themed(&mut view, &theme);
            assert!(accessibility_for_id(composition, "onboarding.connect.cancel.button").is_some());
            assert!(accessibility_for_id(composition, "onboarding.connect.nearby.button").is_none());
            assert_eq!(
                accessibility_for_id(composition, "onboarding.nearby.status").and_then(|node| node.name),
                Some(tilo_onboarding::t("onboarding-nearby-joining"))
            );
        }
    }

    tilo_test_env::scenario! {
        fn pointer_targets_hit_the_acknowledgement_and_continue_actions;

        phase acknowledgement_label_is_the_checkbox_click_target;
        {
            let (mut view, _) = view();
            let mut document = AutoCommit::new();
            press(&view, &mut document, KEY_BEGIN);
            view.sync(&document, &automerge::ROOT);

            let theme = Theme::dark();
            let LaidOut {
                taffy,
                node,
                nodes: node_map,
            } = lay_out_as_root(&mut view, 800.0, 600.0, &theme);
            let row = node_map
                .values()
                .find_map(|&pointer| unsafe {
                    ((*pointer).test_id() == Some("onboarding.understood.row"))
                        .then(|| (*pointer).get_rect())
                })
                .expect("labelled acknowledgement row");
            let label_point = Point::new(row.x1 - 8.0, row.center().y);
            assert!(
                label_point.x > row.x0 + 44.0,
                "test point must be in the label, not the checkbox face"
            );

            let mut focus = None;
            for event in [pointer_event(label_point), mouse_press()] {
                let mut handled = false;
                view.handle_event(
                    &event,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &Modifiers::default(),
                        state: ui::widget::Edits::lend(&mut document),
                        path: &automerge::ROOT,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
                if matches!(event, WindowEvent::CursorMoved { .. }) {
                    assert!(
                        !handled,
                        "hover movement must remain available to sibling controls"
                    );
                } else {
                    assert!(handled, "the labelled checkbox row must handle {event:?}");
                }
            }
            assert!(
                ui::widget::find_test_id(ceremony(&mut view), "onboarding.understood.row")
                    .is_some_and(Widget::is_focused),
                "the label press must focus the acknowledgement checkbox"
            );
            // What the person ticked reaches the ceremony on its next frame.
            view.sync(&document, &automerge::ROOT);
            view.settle(
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
            );
            assert!(read_bool(&document, &automerge::ROOT, UNDERSTOOD));
        }

        phase continue_without_backup_pointer_target_advances_to_ready_page;
        {
            let (mut view, _) = view();
            let mut document = AutoCommit::new();
            for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
                press(&view, &mut document, key);
                view.sync(&document, &automerge::ROOT);
            }
            acknowledge(&mut document, KEY_BACKUP_SKIP_ACKNOWLEDGED, true);
            view.sync(&document, &automerge::ROOT);

            let theme = Theme::dark();
            let LaidOut {
                taffy,
                node,
                nodes: node_map,
            } = lay_out_as_root(&mut view, 1280.0, 800.0, &theme);
            let button = node_map
                .values()
                .find_map(|&pointer| unsafe {
                    ((*pointer).test_id() == Some("onboarding.backup.continue.button"))
                        .then(|| (*pointer).get_rect())
                })
                .expect("continue without backup button");
            assert!(
                button.width() >= 44.0
                    && button.height() >= f64::from(theme.tokens.metrics.min_pointer_target),
                "backup continuation needs a practical pointer target: {button:?}"
            );

            let mut focus = None;
            for event in [
                pointer_event(button.center()),
                mouse_press(),
                mouse_release(),
            ] {
                let mut handled = false;
                view.handle_event(
                    &event,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &Modifiers::default(),
                        state: ui::widget::Edits::lend(&mut document),
                        path: &automerge::ROOT,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
                if matches!(event, WindowEvent::CursorMoved { .. }) {
                    assert!(
                        !handled,
                        "hover movement must remain available to sibling controls"
                    );
                } else {
                    assert!(
                        handled,
                        "the backup continuation button must handle {event:?}"
                    );
                }
            }
            view.sync(&document, &automerge::ROOT);
            assert!(
                view.identity_ready_page,
                "pointer activation must reach Ready"
            );
        }
    }

    tilo_test_env::scenario! {
        fn backup_and_recovery_tasks_share_a_reachable_scroll_body_at_review_sizes;

        phase first_frames_do_not_hide_backup_recovery_or_final_tour_tasks;
        {
            fn initial_scroll_offset(view: &mut OnboardingView) -> f32 {
                let theme = Theme::dark();
                let LaidOut {
                    nodes: node_map, ..
                } = lay_out_as_root(view, 1280.0, 800.0, &theme);
                let scroll_pointer = node_map
                    .values()
                    .copied()
                    .find(|&pointer| unsafe { (*pointer).name() == "Scrollable" })
                    .expect("onboarding body scroll viewport");
                unsafe {
                    (*scroll_pointer)
                        .as_any_mut()
                        .and_then(|widget| widget.downcast_mut::<Scrollable>())
                        .expect("onboarding scroll geometry")
                        .max_offset_y()
                }
            }

            let mut backup = OnboardingView::with_minter_for_host_desktop(Box::new(|| {
                Ok(MintedIdentity {
                    fingerprint: "ab".repeat(32),
                    path: PathBuf::from("/identity/primary.json"),
                })
            }));
            backup.set_visual_state_for_snapshot(OnboardingVisualState::Backup);
            assert!(
                initial_scroll_offset(&mut backup) <= 160.0,
                "Backup may scroll its secondary explanation, but the first frame must keep the recovery operation dominant and the overflow bounded"
            );

            let mut final_tour = OnboardingView::new_tour();
            final_tour.tour_page = Some(TourPage::Start);
            let final_tour_overflow = initial_scroll_offset(&mut final_tour);
            assert!(
                final_tour_overflow <= 20.0,
                "every first-five-minutes task must be visible before a person discovers scrolling; only the content gutter may extend below the viewport (overflow={final_tour_overflow})"
            );
        }

        phase backup_recovery_controls_share_a_reachable_body_at_review_sizes;
        {
            for (width, height, theme) in [
                (1280.0, 800.0, Theme::dark()),
                (960.0, 540.0, Theme::dark().with_text_scale(1.3)),
            ] {
                let (mut view, _) = view();
                view.set_visual_state_for_snapshot(OnboardingVisualState::Backup);
                let LaidOut {
                    nodes: node_map, ..
                } = lay_out_as_root(&mut view, width, height, &theme);
                let rect = |id: &str| {
                    node_map
                        .values()
                        .find_map(|&pointer| unsafe {
                            ((*pointer).test_id() == Some(id)).then(|| (*pointer).get_rect())
                        })
                        .unwrap_or_else(|| panic!("backup page exposes {id}"))
                };
                let scroll_pointer = node_map
                    .values()
                    .copied()
                    .find(|&pointer| unsafe { (*pointer).name() == "Scrollable" })
                    .unwrap();
                let scroll = unsafe {
                    (*scroll_pointer)
                        .as_any_mut()
                        .unwrap()
                        .downcast_mut::<Scrollable>()
                        .unwrap()
                };
                let viewport = scroll.content_viewport_rect();
                let max_offset = scroll.max_offset_y();
                let heading = rect("onboarding.backup.heading");
                assert!(
                    heading.y0 >= viewport.y0 && heading.y1 <= viewport.y1,
                    "backup heading must not be clipped: heading={heading:?}, viewport={viewport:?}"
                );
                assert!(
                    viewport.height() >= panel_rect(&mut view).height() * 0.45,
                    "backup viewport must not collapse around its title: {viewport:?}"
                );
                let passphrase = rect("onboarding.backup.passphrase");
                let confirmation = rect("onboarding.backup.confirmation");
                let passphrase_label = rect("onboarding.backup.passphrase.label");
                let confirmation_label = rect("onboarding.backup.confirmation.label");
                let save = rect("onboarding.backup.button");
                let skip = rect("onboarding.backup.skip-acknowledged.row");
                for (label_id, label, field_id, field) in [
                    (
                        "passphrase label",
                        passphrase_label,
                        "passphrase",
                        passphrase,
                    ),
                    (
                        "confirmation label",
                        confirmation_label,
                        "confirmation",
                        confirmation,
                    ),
                ] {
                    assert!(
                        label.area() > 0.0
                            && label.x0 >= 0.0
                            && label.x1 <= f64::from(width)
                            && label.y0 >= viewport.y0
                            && label.height() <= viewport.height()
                            && (label.y1 - viewport.y1).max(0.0) <= f64::from(max_offset)
                            && label.y1 <= field.y0,
                        "{label_id} must remain visible above {field_id} at {width}x{height}: label={label:?}, field={field:?}"
                    );
                }
                for (id, control) in [
                    ("passphrase", passphrase),
                    ("confirmation", confirmation),
                    ("save", save),
                    ("skip", skip),
                ] {
                    assert!(
                        control.area() > 0.0
                            && control.x0 >= 0.0
                            && control.x1 <= f64::from(width)
                            && control.y0 >= 0.0
                            && (id == "skip" && control.y1 <= f64::from(height)
                                || id != "skip"
                                    && control.height() <= viewport.height()
                                    && (control.y1 - viewport.y1).max(0.0)
                                        <= f64::from(max_offset)),
                        "{id} must be fully visible at {width}x{height}: {control:?}"
                    );
                }
                assert!(passphrase.y1 <= confirmation.y0);
                assert!(confirmation.y1 <= save.y0);
                assert!(
                    viewport.y1 <= skip.y0,
                    "scrolling recovery content must remain separate from anchored acknowledgment at {width}x{height}: viewport={viewport:?}, skip={skip:?}"
                );
                // Tab to Save as a person does, rather than treating an
                // off-clip widget rectangle as visible: the app's next frame
                // brings the focused control into the shared scroller.
                let mut document = AutoCommit::new();
                let save_focused = |view: &mut OnboardingView| {
                    ui::widget::find_test_id(ceremony(view), "onboarding.backup.button")
                        .is_some_and(Widget::is_focused)
                };
                for _ in 0..16 {
                    if save_focused(&mut view) {
                        break;
                    }
                    ui::testing::Dispatch::new()
                        .doc(&mut document)
                        .send(&mut view, &ui::synthetic_input::named_key(NamedKey::Tab));
                    let _ = lay_out_as_root(&mut view, width, height, &theme);
                }
                assert!(save_focused(&mut view), "Tab reaches Save");
                let revealed_save =
                    ui::widget::find_test_id(ceremony(&mut view), "onboarding.backup.button")
                        .expect("Save")
                        .get_rect();
                let viewport = view
                    .app
                    .root_mut()
                    .and_then(scroller)
                    .expect("the shared scroller")
                    .content_viewport_rect();
                assert!(
                    revealed_save.y0 >= viewport.y0 - 0.5 && revealed_save.y1 <= viewport.y1 + 0.5,
                    "actual reveal must bring the whole Save target into the same clip: save={revealed_save:?}, viewport={viewport:?}"
                );
            }
        }

        phase verified_backup_status_and_completion_are_visible_at_review_sizes;
        {
            for (width, height) in [(1280.0, 800.0), (800.0, 600.0)] {
                let (mut view, _) = view();
                view.set_visual_state_for_snapshot(OnboardingVisualState::BackupVerified);
                let theme = Theme::dark();
                let LaidOut {
                    nodes: node_map, ..
                } = lay_out_as_root(&mut view, width, height, &theme);
                let rect = |id: &str| {
                    node_map
                        .values()
                        .find_map(|&pointer| unsafe {
                            ((*pointer).test_id() == Some(id)).then(|| (*pointer).get_rect())
                        })
                        .unwrap_or_else(|| panic!("verified backup page exposes {id}"))
                };
                let another = rect("onboarding.backup.another.button");
                let continue_button = rect("onboarding.backup.continue.button");
                for (id, control) in [("another", another), ("continue", continue_button)] {
                    assert!(
                        control.area() > 0.0
                            && control.x0 >= 0.0
                            && control.x1 <= f64::from(width)
                            && control.y0 >= 0.0
                            && control.y1 <= f64::from(height),
                        "verified backup {id} action must be visible at {width}x{height}: {control:?}"
                    );
                }
                assert!(
                    (another.center().y - continue_button.center().y).abs() <= 0.5
                        && another.x1 <= continue_button.x0,
                    "optional copy action must precede the primary Continue action: another={another:?}, continue={continue_button:?}"
                );
                assert!(
                    !node_map.values().any(|&pointer| unsafe {
                        matches!(
                            (*pointer).test_id(),
                            Some("onboarding.backup.passphrase" | "onboarding.backup.confirmation")
                        )
                    }),
                    "verified backup should expose its confirmed state before asking for another passphrase"
                );
                let scroll_pointer = node_map
                    .values()
                    .copied()
                    .find(|&pointer| unsafe { (*pointer).name() == "Scrollable" })
                    .expect("verified backup scroll viewport");
                let scroll = unsafe {
                    (*scroll_pointer)
                        .as_any_mut()
                        .and_then(|widget| widget.downcast_mut::<Scrollable>())
                        .expect("Scrollable exposes geometry to the regression harness")
                };
                assert_eq!(
                    scroll.max_offset_y(),
                    0.0,
                    "verified backup explanation must be fully visible before scrolling at {width}x{height}"
                );
            }
        }
    }

    tilo_test_env::scenario! {
        fn large_text_keeps_every_primary_action_visible;

        phase done_exit_action_stays_visible_at_laptop_height_with_large_text;
        {
            let (mut view, _) = view();
            let mut doc = AutoCommit::new();
            for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
                press(&view, &mut doc, key);
                view.sync(&doc, &automerge::ROOT);
            }
            assert_eq!(view.flow().screen(), Screen::Done);
            press(&view, &mut doc, KEY_BACKUP_SKIP_ACKNOWLEDGED);
            view.sync(&doc, &automerge::ROOT);
            press(&view, &mut doc, KEY_BACKUP_CONTINUE);
            sync_settled(&mut view, &doc);

            let theme = Theme::dark().with_text_scale(1.3);
            let LaidOut {
                nodes: node_map, ..
            } = lay_out_as_root(&mut view, 1000.0, 720.0, &theme);

            // The ready hand-off owns only the height its content needs
            // (61034652be); at large text it must still fit the viewport
            // and hold its actions whole rather than clip them.
            let panel = panel_rect(&mut view);
            assert!(
                panel.y0 >= 0.0 && panel.y1 <= 720.0 && panel.height() > 0.0,
                "large-text ready screen must fit the laptop-height viewport: {panel:?}",
            );

            let enter = "onboarding.finish.button";
            let action = node_map
                .values()
                .find_map(|&ptr| unsafe {
                    ((*ptr).test_id() == Some(enter)).then(|| (*ptr).get_rect())
                })
                .unwrap_or_else(|| panic!("Done screen exposes {enter}"));
            assert!(action.area() > 0.0, "action={action:?}");
            assert!(
                action.x0 >= 0.0 && action.x1 <= 1000.0 && action.y0 >= 0.0 && action.y1 <= 720.0,
                "the action must stay fully in the viewport, action={action:?}"
            );
            assert!(
                action.y0 >= panel.y0 && action.y1 <= panel.y1,
                "the panel must hold the action unclipped, action={action:?}, panel={panel:?}"
            );
            assert!(
                !node_map.values().any(|&ptr| unsafe {
                    (*ptr).test_id() == Some("onboarding.tour.start.button")
                }),
                "the ready screen hands off to the desktop without offering a tour"
            );
        }

        phase large_text_backup_action_keeps_a_clear_gap_below_confirmation;
        {
            let (mut view, _) = view();
            let mut document = AutoCommit::new();
            for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
                press(&view, &mut document, key);
                view.sync(&document, &automerge::ROOT);
            }
            assert_eq!(view.flow().screen(), Screen::Done);

            let theme = Theme::dark().with_text_scale(1.3);
            let LaidOut {
                nodes: node_map, ..
            } = lay_out_as_root(&mut view, 1280.0, 720.0, &theme);
            let widget_rect = |id: &str| {
                node_map
                    .values()
                    .find_map(|&ptr| unsafe {
                        ((*ptr).test_id() == Some(id)).then(|| (*ptr).get_rect())
                    })
                    .unwrap_or_else(|| panic!("backup page exposes {id}"))
            };
            let confirmation = widget_rect("onboarding.backup.confirmation");
            let action = widget_rect("onboarding.backup.button");
            assert!(
                action.y0 - confirmation.y1 >= f64::from(theme.tokens.spacing.s2) - 0.5,
                "backup action must not visually merge with confirmation input: confirmation={confirmation:?}, action={action:?}"
            );
        }

        phase tour_navigation_stays_visible_on_every_page_at_narrow_large_text_size;
        {
            const WIDTH: f32 = 420.0;
            const HEIGHT: f32 = 640.0;

            for next_presses in 0..=3 {
                let (mut view, _) = view();
                let mut doc = AutoCommit::new();
                for key in [
                    KEY_BEGIN,
                    KEY_UNDERSTOOD,
                    KEY_CREATE,
                    KEY_BACKUP_SKIP_ACKNOWLEDGED,
                    KEY_BACKUP_CONTINUE,
                    KEY_TOUR_START,
                ] {
                    press(&view, &mut doc, key);
                    view.sync(&doc, &automerge::ROOT);
                }
                for _ in 0..next_presses {
                    press(&view, &mut doc, KEY_TOUR_NEXT);
                    view.sync(&doc, &automerge::ROOT);
                }

                let theme = Theme::dark().with_text_scale(1.3);
                let LaidOut {
                    nodes: node_map, ..
                } = lay_out_as_root(&mut view, WIDTH, HEIGHT, &theme);

                let mut action_rects = Vec::new();
                for id in [
                    "onboarding.tour.skip.button",
                    "onboarding.tour.back.button",
                    "onboarding.tour.next.button",
                ] {
                    let action = node_map
                        .values()
                        .find_map(|&ptr| unsafe {
                            ((*ptr).test_id() == Some(id)).then(|| (*ptr).get_rect())
                        })
                        .unwrap_or_else(|| panic!("tour page exposes {id}"));
                    assert!(
                        action.area() > 0.0,
                        "page={next_presses}, action={action:?}"
                    );
                    assert!(
                        action.x0 >= 0.0
                            && action.x1 <= f64::from(WIDTH)
                            && action.y0 >= 0.0
                            && action.y1 <= f64::from(HEIGHT),
                        "sticky tour action must stay in the viewport, page={next_presses}, id={id}, action={action:?}"
                    );
                    action_rects.push(action);
                }
                assert!(
                    action_rects
                        .windows(2)
                        .all(|pair| pair[0].x1 <= pair[1].x0
                            && (pair[0].y0 - pair[1].y0).abs() < 1.0),
                    "compact tour actions must remain one ordered row, page={next_presses}, rects={action_rects:?}"
                );
            }
        }
    }
}
