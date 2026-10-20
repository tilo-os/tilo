//! The rendered first-login identity ceremony.
//!
//! Chrome, not an app: the compositor draws this before the desktop
//! exists, since the screen that creates an identity cannot be something
//! an application could imitate.
//!
//! The decisions all live in [`crate::onboarding::Onboarding`]; this
//! file is only the composition, built from shared `ui` widgets (`Section`,
//! `Banner`, `Text`, `Button`, `Checkbox`) so the ceremony inherits the
//! system theme, focus order and accessibility like the rest of the desktop.
//!
//! On a Tilo system that has not opted in yet, setup also offers to play more
//! video and audio formats after the backup step ([`CodecSetup`]): a page
//! with one large Install button that downloads the media codecs extension
//! only when pressed; nothing is chosen in advance. It runs
//! through the same polkit-allowed unit as Settings and never blocks setup:
//! the download continues in the background and the result arrives as a
//! notification.

mod prepared;
mod semantic;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value};
use taffy::{NodeId, TaffyTree};
use ui::color_compat::ColorCompat;
use ui::theme::Theme;
use ui::widget::{
    Banner, Button, ButtonRole, Checkbox, Column, FilePicker, FilePickerMode, IconWidget, ListRow,
    ProgressBar, Row, Scrollable, Section, Spacer, Spinner, Text, TextInput, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use zeroize::{Zeroize, Zeroizing};

use crate::onboarding::{
    MintedIdentity, Onboarding, Screen, acknowledgement, done_body, done_title, intro_body,
    intro_points, intro_title, no_recovery_points, no_recovery_title,
};
use crate::shell::i18n::{t, t_args};
use os_actions::codec_pack::{CodecAction, CodecPackStatus, PackState};

/// How often setup looks for news from its background work (codec install,
/// nearby pairing) while that work runs.
const BACKGROUND_POLL: Duration = Duration::from_millis(250);
const PANEL_MAX_WIDTH: f32 = 760.0;
// Longer translated/large-text pages scroll inside this limit. Short pages
// retain their intrinsic height so setup never becomes a mostly empty modal.
const PANEL_MAX_HEIGHT: f32 = 640.0;
/// Large text and high contrast need additional vertical room before a dense
/// first-use page falls back to scrolling. The viewport wrapper still caps
/// this to its own height minus the safety gutters.
const ACCESSIBLE_PANEL_HEIGHT: f32 = 672.0;
/// Restore, protection and backup contain dense decisions and controls. Give
/// them enough preferred height to avoid needless scrolling on a wide laptop;
/// flex shrink still preserves the safety gutter on a compact display.
const DENSE_PANEL_PREFERRED_HEIGHT: f32 = 560.0;
/// The two choice screens get a lower cap than the recovery and completion
/// stages; they still shrink to their contents.
const INTRO_PANEL_HEIGHT: f32 = 520.0;
/// Completion is a hand-off surface, not a compact alert. Give its title,
/// confirmation, next-step explanation, and backup guidance a stable canvas
/// on ordinary laptop displays; the inner viewport still scrolls when large
/// text or a short window cannot fit that content.
const READY_PANEL_PREFERRED_HEIGHT: f32 = 552.0;
/// The formats step is a full page like the hand-off: a title, a line and one
/// large Install button; its results add a banner and a progress bar. One
/// height for all of them, so choosing does not make the card jump.
const CODECS_PANEL_PREFERRED_HEIGHT: f32 = READY_PANEL_PREFERRED_HEIGHT;
/// The formats step's Install button: the page's single, deliberate action.
const VIEWPORT_GUTTER: f32 = 24.0;
/// A 40 px action plus the footer's 17 px bottom inset. The gap above the
/// footer belongs to the parent column and must not be reserved twice; doing
/// so made short pages acquire a pointless scrollbar and hid useful recovery
/// copy on laptop-height displays.
const FOOTER_HEIGHT: f32 = 57.0;
/// A labelled checkbox has a 44 px minimum target. Decision pages pin that
/// acknowledgement above the ordinary action row so large text and compact
/// displays never hide the gate inside scrolling explanatory copy.
const ACKNOWLEDGEMENT_FOOTER_EXTRA_HEIGHT: f32 = 72.0;
/// `s3` top pad, eyebrow row, gap, padded 8 px progress track, and gap below
/// the sticky header. The pad keeps the eyebrow off the panel's top edge
/// (HIG §7: nothing touches the container it sits in).
const PROGRESS_HEADER_HEIGHT: f32 = 70.0;
const PROGRESS_FILE: &str = ".onboarding-progress.json";
const PROGRESS_VERSION: u32 = 1;
const PROGRESS_MAX_BYTES: u64 = 4096;

/// Secret-free crash journal for the small interval between identity
/// acquisition and explicit first-use completion. It lives beside the
/// identity so it follows the same per-user lifecycle and trust boundary.
#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum PersistedStage {
    AcquiringIdentity,
    /// The identity exists and setup must still offer a portable backup.
    IdentityReady,
    /// A portable backup was verified, but setup has not reached its hand-off.
    IdentityReadyBackedUp,
    /// The person handled the backup choice and reached the final hand-off.
    ReadyToEnter,
    /// The hand-off was reached after a portable backup was written and
    /// verified. Only this fact is journaled, never the backup's path.
    ReadyToEnterBackedUp,
    TourNavigate,
    TourOrganise,
    TourCollaborate,
    TourStart,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct PersistedProgress {
    version: u32,
    stage: PersistedStage,
}

fn progress_path(identity_dir: &std::path::Path) -> PathBuf {
    identity_dir.join(PROGRESS_FILE)
}

/// A change to the crash journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JournalChange {
    Store(PersistedStage),
    Clear,
}

/// What the ceremony does once a journal change is on disk: the step the
/// change was written ahead of. Each re-checks the guard its action had, as
/// the person may have moved on while the write was outstanding.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AfterJournal {
    /// The change records a step already taken.
    Nothing,
    StartNearby,
    /// A nearby join reached the hand-off.
    EnterReadyAfterJoin,
    /// The backup step reached the hand-off.
    EnterReady,
    FinishPortableBackup,
    Import(PathBuf),
    RetryMint,
    Mint,
    Tour(Option<TourPage>),
    Finish,
}

/// A journal change the state writer is making. The journal is written
/// ahead of the step it guards, but never on the compositor's loop, where
/// its fsyncs can take seconds under I/O pressure: the step waits here until
/// the write reports, and `sync` takes it on the frame after.
struct JournalWrite {
    change: JournalChange,
    then: AfterJournal,
    /// The write's result, set on the writer's thread.
    outcome: Arc<std::sync::OnceLock<Result<(), String>>>,
}

/// Whether startup must retain trusted first-use chrome even though an
/// identity already exists. Any object or unreadable entry at the journal
/// path counts as pending: malformed recovery state must never become a
/// shortcut around completion.
pub fn has_pending_progress(identity_dir: &std::path::Path) -> bool {
    match std::fs::symlink_metadata(progress_path(identity_dir)) {
        Ok(_) => true,
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Clear the crash journal left immediately before an identity acquisition
/// only after startup has fully loaded and verified the resulting identity.
///
/// `AcquiringIdentity` protects the write transaction: before a valid
/// identity exists it must keep first-use chrome active. Once the complete
/// identity chain verifies, however, that marker can only be residue from a
/// process interruption between the identity commit and the next journal
/// update. Treating it as post-identity setup would incorrectly send an
/// established user back through recovery on every login.
///
/// # Errors
///
/// Fails when the journal cannot be read, is malformed, or cannot be removed.
pub fn clear_verified_pre_acquisition_progress(
    identity_dir: &std::path::Path,
) -> std::io::Result<bool> {
    if load_progress(identity_dir)? != Some(PersistedStage::AcquiringIdentity) {
        return Ok(false);
    }
    clear_progress(identity_dir)?;
    Ok(true)
}

fn load_progress(identity_dir: &std::path::Path) -> std::io::Result<Option<PersistedStage>> {
    let path = progress_path(identity_dir);
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.len() > PROGRESS_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "onboarding progress must be a small regular file",
        ));
    }
    #[cfg(unix)]
    {
        if !tilo_files::native::private::private_to_me(&metadata) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "onboarding progress must be private and owned by this user",
            ));
        }
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(PROGRESS_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let stored: PersistedProgress = serde_json::from_slice(&bytes).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid onboarding progress: {error}"),
        )
    })?;
    if stored.version != PROGRESS_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unsupported onboarding progress version {}", stored.version),
        ));
    }
    Ok(Some(stored.stage))
}

fn store_progress(identity_dir: &std::path::Path, stage: PersistedStage) -> std::io::Result<()> {
    crate::firstboot::prepare_identity_directory(identity_dir)?;
    let bytes = serde_json::to_vec_pretty(&PersistedProgress {
        version: PROGRESS_VERSION,
        stage,
    })
    .map_err(|error| std::io::Error::other(error.to_string()))?;
    tilo_files::native::atomic::write_with_mode(&progress_path(identity_dir), &bytes, 0o600)
}

fn clear_progress(identity_dir: &std::path::Path) -> std::io::Result<()> {
    let path = progress_path(identity_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        // Still sync the directory after NotFound. A previous Finish attempt
        // may have removed the entry but failed while making that removal
        // durable; letting the retry finish without fsync could resurrect it
        // after a power loss.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Make journal removal durable too. Otherwise a power loss immediately
    // after Finish could resurrect a completed tour on the next login.
    std::fs::File::open(identity_dir)?.sync_all()?;
    Ok(())
}

/// Context for the compositor-owned identity ceremony. Both variants keep
/// the same security gates; the host version only makes clear that Tilo is not
/// replacing the login/account setup the surrounding Linux system completed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnboardingContext {
    #[default]
    FullSystem,
    HostDesktop,
}

/// Deterministic states used only by the offscreen production renderer.
/// They exercise the composition without performing identity I/O.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardingVisualState {
    Intro,
    Existing,
    Nearby,
    NearbyCode,
    Restore,
    Protect,
    Backup,
    BackupVerified,
    Ready,
    TourNavigate,
    TourOrganise,
    TourCollaborate,
    TourStart,
    CreateError,
    RestoreError,
    Codecs,
    CodecsInstalling,
    CodecsWaitingForNetwork,
    CodecsInstalled,
    CodecsFailed,
}

#[must_use]
pub fn host_desktop_intro_title() -> String {
    t("onboarding-host-intro-title")
}

#[must_use]
pub fn host_desktop_intro_body() -> String {
    t("onboarding-host-intro-body")
}

pub const KEY_BEGIN: &str = "onboarding.begin";
pub const KEY_EXISTING: &str = "onboarding.existing";
pub const KEY_CHOOSE_NEARBY: &str = "onboarding.existing.nearby";
pub const KEY_CHOOSE_RESTORE: &str = "onboarding.existing.restore";
pub const KEY_BACK: &str = "onboarding.back";
pub const KEY_CONNECT_BROWSE: &str = "onboarding.connect.browse";
pub const KEY_IMPORT: &str = "onboarding.import";
pub const KEY_PREPARED_IMPORT: &str = "onboarding.prepared.import";
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

#[must_use]
pub fn codecs_title() -> String {
    t("onboarding-codecs-title")
}

/// The one short line under the offer: where the formats come from, and that
/// the choice can be undone.
#[must_use]
pub fn codecs_note() -> String {
    t("onboarding-codecs-note")
}

#[must_use]
pub fn codecs_background() -> String {
    t("onboarding-codecs-background")
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TourPage {
    Navigate,
    Organise,
    Collaborate,
    Start,
}

// Keep this copy beside the tour rather than duplicating a second conceptual
// keyboard map in prose. These labels mirror `docs/shell-navigation-keyboard-map.md`
// and the gesture contract in `compositor/backend/drm/gestures.rs`; in
// particular, Super+W closes a tab and must never be advertised as Overview.
/// Catalogue keys of each tour card's (title, body).
const NAVIGATE_CARDS: &[(&str, &str)] = &[
    (
        if cfg!(target_os = "macos") {
            "onboarding-tour-search-title-mac"
        } else {
            "onboarding-tour-search-title"
        },
        "onboarding-tour-search-body",
    ),
    (
        "onboarding-tour-overview-title",
        "onboarding-tour-overview-body",
    ),
    ("onboarding-tour-move-title", "onboarding-tour-move-body"),
];

const ORGANISE_CARDS: &[(&str, &str)] = &[
    (
        "onboarding-tour-workspace-title",
        "onboarding-tour-workspace-body",
    ),
    ("onboarding-tour-strip-title", "onboarding-tour-strip-body"),
    (
        "onboarding-tour-window-group-title",
        "onboarding-tour-window-group-body",
    ),
    ("onboarding-tour-tab-title", "onboarding-tour-tab-body"),
];

const COLLABORATE_CARDS: &[(&str, &str)] = &[
    (
        "onboarding-tour-invite-settings-title",
        "onboarding-tour-invite-settings-body",
    ),
    (
        "onboarding-tour-follow-title",
        "onboarding-tour-follow-body",
    ),
    (
        "onboarding-tour-system-features-title",
        "onboarding-tour-system-features-body",
    ),
];

/// A concrete hand-off from trusted setup into the otherwise-empty first
/// workspace.  The earlier tour pages explain the model; this one answers the
/// more important first-use question: "what should I do now?"  Keep these
/// labels aligned with real shell surfaces so the checklist never teaches a
/// product that does not exist.
const START_CARDS: &[(&str, &str)] = &[
    ("onboarding-tour-name-title", "onboarding-tour-name-body"),
    ("onboarding-tour-open-title", "onboarding-tour-open-body"),
    (
        "onboarding-tour-invite-title",
        "onboarding-tour-invite-body",
    ),
    ("onboarding-tour-check-title", "onboarding-tour-check-body"),
];

/// The keyboard and gesture route a navigation card teaches, shown under
/// its title.
fn tour_shortcut(card_title: &str) -> Option<&'static str> {
    match card_title {
        "onboarding-tour-search-title" => Some("onboarding-tour-search-keys"),
        "onboarding-tour-search-title-mac" => Some("onboarding-tour-search-keys-mac"),
        "onboarding-tour-overview-title" => Some("onboarding-tour-overview-keys"),
        "onboarding-tour-move-title" => Some("onboarding-tour-move-keys"),
        _ => None,
    }
}

impl TourPage {
    fn number(self) -> u8 {
        match self {
            Self::Navigate => 1,
            Self::Organise => 2,
            Self::Collaborate => 3,
            Self::Start => 4,
        }
    }
}

type Minter = Box<dyn Fn() -> Result<MintedIdentity, String> + Send>;
/// Restore an identity from the backup at `source` into `import_dir` and
/// make it active, with the same root to device chain minting provisions.
fn import_identity(
    import_dir: &Path,
    source: &Path,
    passphrase: &[u8],
) -> Result<MintedIdentity, String> {
    let store = crate::identity_registry::IdentityRegistryStore::at(import_dir);
    let profile = if passphrase.is_empty() {
        // Explicit compatibility path for schema-1 exports from
        // older Tilo builds. New backups always require a
        // passphrase and never expose plaintext JSON.
        store.import(source).map_err(|error| error.to_string())?
    } else {
        store
            .import_portable(source, passphrase)
            .map_err(|error| error.to_string())?
    };
    let runtime_id = profile
        .record_path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| "imported identity has no valid runtime id".to_string())?;
    runtime_identity::set_active_identity(import_dir, runtime_id)
        .map_err(|error| error.to_string())?;
    // Restoring on a fresh machine must provision the exact same
    // complete root -> device chain as minting. The narrower
    // identity-keyed certificate helper leaves the legacy system
    // owner/certificate absent until a later login repair.
    crate::firstboot::ensure_collaboration_identity(
        import_dir,
        &crate::firstboot::collaboration_identity_paths(),
    )
    .map_err(|error| error.to_string())?;
    crate::device_fleet::DeviceFleetStore::at(
        import_dir,
        crate::firstboot::collaboration_identity_paths(),
    )
    .register_current_device(&profile.id)
    .map_err(|error| error.to_string())?;
    Ok(MintedIdentity {
        fingerprint: profile.id,
        path: profile.record_path,
    })
}

type Importer = Box<dyn Fn(&PathBuf, &[u8]) -> Result<MintedIdentity, String> + Send>;
type BackupCapability = Box<dyn Fn(&Path) -> Result<bool, String> + Send>;
fn canonical_backup_capability(path: &Path) -> Result<bool, String> {
    runtime_identity::load_from_path(path.to_owned())
        .map(|identity| identity.is_single_device())
        .map_err(|error| error.to_string())
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
}

/// Identity work running off the compositor's loop.
///
/// Minting, importing and exporting the identity generate or unwrap keys,
/// talk to the secret store, stretch a passphrase on purpose and fsync the
/// records they write: seconds under I/O pressure, at first boot when the
/// disk is busiest. The work runs on its own thread and `sync` takes its
/// outcome on a later frame ([`OnboardingView::settle_identity_work`]); the
/// crash journal is still written ahead of it and after it, in order.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PickerPurpose {
    ImportIdentity,
    BackupIdentity,
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

    fn focus(&mut self) {
        *self = match *self {
            Self::Clean | Self::Focus => Self::Focus,
            Self::Structure | Self::StructureAndFocus => Self::StructureAndFocus,
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

/// Trusted setup owns these document gates; controls only queue the person's intent.
#[derive(Clone, Copy, Debug, serde::Serialize)]
enum AcknowledgementKind {
    IdentityProtection,
    SkipBackup,
}

#[derive(Debug, serde::Serialize)]
struct AcknowledgementAction {
    kind: AcknowledgementKind,
    path: ObjId,
    checked: bool,
}

impl tilo_actions::Action for AcknowledgementAction {
    fn apply(&self, doc: &mut AutoCommit) -> Result<(), tilo_actions::ActionError> {
        use automerge::transaction::Transactable as _;
        let key = match self.kind {
            AcknowledgementKind::IdentityProtection => KEY_UNDERSTOOD,
            AcknowledgementKind::SkipBackup => KEY_BACKUP_SKIP_ACKNOWLEDGED,
        };
        doc.put(&self.path, key, self.checked)?;
        Ok(())
    }
}

/// The setup actions a person has taken (a button click or its keyboard
/// shortcut) that `sync` has not handled yet. Buttons record into it from
/// their click handler; the document never sees a click.
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

    /// A button whose click takes `action`.
    fn button(&self, id: &str, label: &str, action: &'static str) -> Button {
        let pressed = self.clone();
        Button::new(id, label).on_click(move || pressed.press(action))
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "recovery mode, backup verification/acknowledgement, modal entry points and presentation preferences are separate state flags"
)]
pub struct OnboardingView {
    flow: Onboarding,
    semantic: Rc<RefCell<semantic::Snapshot>>,
    semantic_requests: Rc<RefCell<Vec<semantic::Request>>>,
    context: OnboardingContext,
    /// Existing metadata is authoritative when its keyring item is
    /// unavailable or its collaboration chain cannot be verified. Recovery
    /// mode starts on restore and never exposes the create-new branch, while
    /// still allowing a keyring unlock retry.
    recovery_only: bool,
    prepared_pending: bool,
    migration: Option<crate::onboarding_migration::Migration>,
    portable_home: Option<crate::onboarding_portable_home::Pending>,
    portable_applications: Option<crate::onboarding_portable_applications::Pending>,
    browser: Option<crate::onboarding_browser::Pending>,
    prepared_secret_queued: bool,
    prepared_worker: Option<crate::onboarding_prepared::Worker>,
    prepared_home_password: Option<([u8; 32], Zeroizing<Vec<u8>>)>,
    prepared_review: Option<crate::onboarding_prepared::Pending>,
    #[cfg(target_os = "linux")]
    peer: Option<crate::onboarding_peer::Peer>,
    #[cfg(target_os = "linux")]
    peer_received: Option<crate::onboarding_peer::Received>,
    #[cfg(target_os = "linux")]
    peer_attempted: bool,
    minter: Shared<Minter>,
    importer: Shared<Importer>,
    exporter: Shared<Exporter>,
    /// Identity work the worker thread is doing, if any. While it runs,
    /// actions that write the journal wait, still pressed, and Back and the
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
    backup_continue_ack: Option<bool>,
    /// Once a backup is verified, keep its success explanation and the
    /// Continue action in view. The passphrase form returns only when the
    /// person explicitly asks to make another copy.
    backup_copy_form_open: bool,
    /// An unprotected identity is a consequential, but supported, choice.
    /// Keep the explicit acknowledgement process-local: it only gates this
    /// one screen, while the durable journal records the resulting choice.
    backup_skip_acknowledged: bool,
    /// `false` shows the post-mint backup step; `true` shows the concise
    /// desktop hand-off. Persisted separately so a crash cannot skip backup.
    identity_ready_page: bool,
    /// This device joined the identity from another one and holds only its
    /// public record: the root key stays on the approving device, so the
    /// ready screen recommends no backup of this one.
    joined_member: bool,
    /// Settings can reopen the proven backup ceremony for an existing
    /// identity. That is a short modal task, not a second first-login flow.
    portable_backup_only: bool,
    /// A persistence error is distinct from identity creation failure: the
    /// current screen remains valid, but advancing is blocked until crash
    /// recovery state can be written or removed safely.
    progress_error: Option<String>,
    /// Journal changes handed to the state writer, oldest first, each with
    /// the step it was written ahead of. While any is outstanding, actions
    /// that write the journal wait, still pressed, for the next frame.
    journal: std::collections::VecDeque<JournalWrite>,
    /// Writes the journal off the compositor's loop.
    journal_writer: &'static crate::shell::durable_writer::DurableWriter,
    tour_page: Option<TourPage>,
    /// A tour reopened from the desktop has no identity ceremony behind it.
    /// Back on its first page and Done both dismiss the modal directly.
    tour_only: bool,
    /// Raised when the user has finished and the desktop may start.
    finished: bool,
    /// Actions the person took since `sync` last looked: view state, never
    /// written to the document.
    pressed: PressedActions,
    acknowledgements: Rc<RefCell<Vec<(AcknowledgementKind, bool)>>>,
    acknowledgement_actions: ui::action_host::ActionHost,
    invalidation: Invalidation,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
    /// A step arriving: forward from the trailing side, back from the
    /// leading side (`ui::motion::Drill`).
    transition: ui::motion::Presence,
    transition_direction: f64,
    reduce_motion: bool,
    /// The motion the person allows, as of the last layout.
    motion: ui::tokens::EffectiveMotion,
    picker: Option<(PickerPurpose, FilePicker)>,
    connect_source_selected: bool,
    connect_passphrase: Arc<Mutex<String>>,
    /// The backup file to restore from, as typed or chosen.
    connect_path: ui::LocalState<String>,
    backup_passphrase: Arc<Mutex<String>>,
    backup_confirmation: Arc<Mutex<String>>,
    identity_dir: Option<PathBuf>,
    nearby: Option<crate::onboarding_nearby::NearbyPairing>,
    nearby_status: Option<String>,
    /// Why the last pairing attempt ended without joining. The nearby
    /// screen titles its error with this cause; `error` holds its message.
    nearby_failure: Option<crate::onboarding_nearby::NearbyFailure>,
    /// The verification code both devices show while pairing waits for
    /// approval.
    nearby_code: Option<String>,
    /// Review renderer only: draw the nearby screen as if pairing were
    /// running, without starting the privileged helper.
    nearby_active_for_snapshot: bool,
    /// The keys of the choice rows activated since the last sync. A row
    /// reports its activation here; sync turns each into the same model key
    /// a button would have set.
    nearby_status_path: Option<PathBuf>,
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
    codec_watching: bool,
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

    /// Build the trusted export-and-verify page for the active identity.
    ///
    /// Settings is deliberately only a discovery surface: the passphrase,
    /// destination picker, and portable-backup cryptography remain in this
    /// shell-owned ceremony.
    ///
    /// # Errors
    ///
    /// Returns an error when the active identity cannot be read or validated.
    pub fn new_for_portable_backup(identity_dir: PathBuf) -> Result<Self, String> {
        let identity_path = runtime_identity::resolve_active_identity(&identity_dir)
            .unwrap_or_else(|| identity_dir.join("primary.json"));
        let identity = crate::onboarding::existing_identity_at(&identity_path)?;
        let mut view = Self::new_in_context(identity_dir, OnboardingContext::FullSystem, false);
        view.begin_portable_backup_for(identity);
        view.refresh_backup_capability()
            .map_err(|()| t("onboarding-connect-recovery-message"))?;
        if !view.backup_export_available {
            return Err(t("onboarding-backup-unavailable"));
        }
        Ok(view)
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
        let mint_dir = identity_dir.clone();
        let import_dir = identity_dir.clone();
        let minter: Minter = match context {
            OnboardingContext::FullSystem => {
                Box::new(move || crate::onboarding::mint_into(&mint_dir).map_err(|e| e.to_string()))
            }
            OnboardingContext::HostDesktop => Box::new(move || {
                crate::onboarding::mint_into_for_legacy_linux_host(&mint_dir)
                    .map_err(|e| e.to_string())
            }),
        };
        let mut view = Self::with_all_handlers_in_context(
            minter,
            Box::new(move |source, passphrase| import_identity(&import_dir, source, passphrase)),
            Box::new(move |identity, destination, passphrase| {
                runtime_identity::export_portable_backup(identity, destination, passphrase)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }),
            context,
        );
        view.backup_capability = Box::new(canonical_backup_capability);
        view.nearby_status_path = Some(identity_dir.join(".nearby-enrollment-status.json"));
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
        view.load_prepared_setup(&identity_dir);
        if context == OnboardingContext::FullSystem {
            view.migration = crate::onboarding_prepared::session_home()
                .map(crate::onboarding_migration::Migration::open);
        }
        view.identity_dir = Some(identity_dir);
        if !view.prepared_pending {
            view.restore_pending_progress_from_disk();
            if (view
                .portable_home
                .as_ref()
                .is_some_and(crate::onboarding_portable_home::Pending::pending)
                || view
                    .portable_applications
                    .as_ref()
                    .is_some_and(crate::onboarding_portable_applications::Pending::pending)
                || view
                    .browser
                    .as_ref()
                    .is_some_and(crate::onboarding_browser::Pending::pending)
                || view
                    .migration
                    .as_ref()
                    .is_some_and(crate::onboarding_migration::Migration::pending))
                && !view.recovery_only
                && !view.flow.is_done()
                && let Some(directory) = view.identity_dir.as_ref()
            {
                let active = runtime_identity::resolve_active_identity(directory)
                    .unwrap_or_else(|| directory.join("primary.json"));
                if let Ok(identity) = crate::onboarding::existing_identity_at(&active) {
                    view.flow.resume_created(identity);
                }
            }
        }
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
            semantic: Rc::default(),
            semantic_requests: Rc::default(),
            context,
            recovery_only: false,
            prepared_pending: false,
            migration: None,
            portable_home: None,
            portable_applications: None,
            browser: None,
            prepared_secret_queued: false,
            prepared_worker: None,
            prepared_home_password: None,
            prepared_review: None,
            #[cfg(target_os = "linux")]
            peer: None,
            #[cfg(target_os = "linux")]
            peer_received: None,
            #[cfg(target_os = "linux")]
            peer_attempted: false,
            minter: Arc::new(Mutex::new(minter)),
            importer: Arc::new(Mutex::new(importer)),
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
            backup_continue_ack: None,
            backup_copy_form_open: false,
            backup_skip_acknowledged: false,
            identity_ready_page: false,
            joined_member: false,
            portable_backup_only: false,
            progress_error: None,
            journal: std::collections::VecDeque::new(),
            journal_writer: crate::shell::durable_writer::global(),
            tour_page: None,
            tour_only: false,
            finished: false,
            pressed: PressedActions::default(),
            acknowledgements: Rc::default(),
            acknowledgement_actions: ui::action_host::ActionHost::new(),
            invalidation: Invalidation::Focus,
            composed: None,
            rect: Rect::ZERO,
            transition: step_arriving(ui::tokens::EffectiveMotion::default()),
            transition_direction: 1.0,
            reduce_motion: false,
            motion: ui::tokens::EffectiveMotion::default(),
            picker: None,
            connect_source_selected: false,
            connect_passphrase: Arc::new(Mutex::new(String::new())),
            connect_path: ui::LocalState::new(String::new()),
            backup_passphrase: Arc::new(Mutex::new(String::new())),
            backup_confirmation: Arc::new(Mutex::new(String::new())),
            identity_dir: None,
            nearby: None,
            nearby_status: None,
            nearby_failure: None,
            nearby_code: None,
            nearby_active_for_snapshot: false,
            nearby_status_path: None,
            codec_setup: None,
            codec_step_offered: false,
            codec_page: false,
            codec_status: CodecPackStatus::default(),
            codec_running: None,
            codec_worker_succeeded: None,
            codec_error: None,
            codec_changed: Arc::new(AtomicBool::new(false)),
            codec_watching: false,
            synced_at: Instant::now(),
        }
    }

    pub(crate) fn semantic_provider(&self) -> Rc<dyn crate::shell::search::SearchProvider> {
        self.publish_semantic_state();
        Rc::new(semantic::Provider::new(self))
    }

    pub(crate) fn authentication_first_use_scope(&self) -> Rc<dyn Fn() -> bool> {
        semantic::authentication_first_use_scope(self)
    }

    fn publish_semantic_state(&self) {
        *self.semantic.borrow_mut() = semantic::Snapshot::from_view(self);
    }

    /// Take `action` as though its button had been clicked. Review renderers
    /// and tests walk the ceremony this way; the next `sync` handles it.
    pub fn press(&self, action: &'static str) {
        self.pressed.press(action);
    }

    /// Takes a setup action as its button would, for tests that hand the view
    /// to a host they then drive.
    #[cfg(test)]
    pub(crate) fn presser(&self) -> Box<dyn Fn(&'static str)> {
        let pressed = self.pressed.clone();
        Box::new(move |action| pressed.press(action))
    }

    /// The tour page on screen, counted from 1.
    #[cfg(test)]
    pub(crate) fn tour_step(&self) -> Option<u8> {
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

    /// The media codecs page's text, in reading order (headless tests).
    pub fn codec_page_lines(&self) -> Vec<String> {
        let mut lines = vec![codecs_title(), codecs_note()];
        if let Some((title, message)) = self.codec_outcome() {
            lines.push(title);
            lines.extend(message);
        }
        if let Some(fraction) = self.codec_progress() {
            lines.push(t_args(
                "onboarding-codecs-progress",
                &[("percent", format!("{:.0}", fraction * 100.0).into())],
            ));
        }
        match self.codec_choice() {
            Some(choice) => lines.extend(choice.into_iter().map(|(_, label, _)| label)),
            None => lines.push(t("onboarding-continue")),
        }
        lines
    }

    /// Setup's total step count: the codecs step adds one.
    fn total_steps(&self) -> u8 {
        if self.codec_step_offered { 5 } else { 4 }
    }

    fn codec_install_requested(&self) -> bool {
        self.codec_running.is_some()
            || self.codec_error.is_some()
            || self.codec_status.state != PackState::NotInstalled
    }

    fn codec_progress(&self) -> Option<f32> {
        match self.codec_status.state {
            PackState::Installing => Some(
                self.codec_status
                    .progress
                    .as_ref()
                    .map_or(0.0, os_actions::codec_pack::Progress::fraction),
            ),
            PackState::NotInstalled if self.codec_running.is_some() => Some(0.0),
            _ => None,
        }
    }

    /// Banner title and message for what happened to the request, if any.
    fn codec_outcome(&self) -> Option<(String, Option<String>)> {
        if let Some(error) = &self.codec_error {
            return Some((t("onboarding-codecs-failed-title"), Some(error.clone())));
        }
        match self.codec_status.state {
            PackState::NotInstalled if self.codec_running.is_some() => {
                Some((t("onboarding-codecs-starting"), Some(codecs_background())))
            }
            PackState::NotInstalled | PackState::Removing => None,
            PackState::Installing => Some((
                t("onboarding-codecs-installing-title"),
                Some(t_args(
                    "onboarding-codecs-installing-message",
                    &[("status", self.codec_status.summary().into())],
                )),
            )),
            PackState::WaitingForNetwork => Some((
                t("onboarding-codecs-offline-title"),
                Some(t("onboarding-codecs-offline-message")),
            )),
            PackState::Installed => Some((
                t("onboarding-codecs-installed-title"),
                self.codec_status.when_it_applies().map(str::to_string),
            )),
            PackState::Failed => Some((
                t("onboarding-codecs-failed-title"),
                Some(if self.codec_status.last_error.is_empty() {
                    t("onboarding-codecs-failed-message")
                } else {
                    t_args(
                        "onboarding-codecs-failed-message-detail",
                        &[("error", self.codec_status.last_error.clone().into())],
                    )
                }),
            )),
        }
    }

    /// The page's own choice while nothing runs: (id, label, key) of the
    /// one large Install (or Try again) button, then the quiet way past it.
    /// `None` once a request is underway or done; the footer then offers
    /// Continue.
    fn codec_choice(&self) -> Option<[(&'static str, String, &'static str); 2]> {
        let failed = self.codec_error.is_some() || self.codec_status.state == PackState::Failed;
        if failed && self.codec_running.is_none() {
            return Some([
                (
                    "onboarding.codecs.install.button",
                    t("onboarding-codecs-retry"),
                    KEY_CODECS_INSTALL,
                ),
                (
                    "onboarding.codecs.skip.button",
                    t("onboarding-codecs-skip"),
                    KEY_CODECS_SKIP,
                ),
            ]);
        }
        if self.codec_install_requested() {
            return None;
        }
        Some([
            (
                "onboarding.codecs.install.button",
                t("onboarding-codecs-install"),
                KEY_CODECS_INSTALL,
            ),
            (
                "onboarding.codecs.skip.button",
                t("onboarding-codecs-not-now"),
                KEY_CODECS_SKIP,
            ),
        ])
    }

    fn start_codec_install(&mut self) {
        let Some(setup) = self.codec_setup.as_ref() else {
            return;
        };
        if self.codec_running.is_some() {
            return;
        }
        if setup.watch_system && !self.codec_watching {
            let changed = Arc::downgrade(&self.codec_changed);
            self.codec_watching = os_actions::codec_pack::watch(move || {
                changed.upgrade().is_some_and(|changed| {
                    changed.store(true, Ordering::Release);
                    true
                })
            })
            .is_ok();
        }
        let runner = Arc::clone(&setup.runner);
        let changed = Arc::clone(&self.codec_changed);
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("onboarding-codecs".into())
            .spawn(move || {
                // The install outlives setup if the person moves on; its
                // result then reaches them as a notification instead.
                let _ = tx.send(runner(CodecAction::Install));
                changed.store(true, Ordering::Release);
            });
        match spawned {
            Ok(_) => {
                self.codec_error = None;
                self.codec_worker_succeeded = None;
                self.codec_running = Some(rx);
            }
            Err(error) => {
                self.codec_error = Some(t_args(
                    "onboarding-codecs-installer-failed",
                    &[("error", error.to_string().into())],
                ));
            }
        }
        self.invalidation.structure();
    }

    /// Pick up the worker's result and status-file changes.
    fn refresh_codec_state(&mut self) {
        let Some(setup) = self.codec_setup.as_ref() else {
            return;
        };
        let mut changed = self.codec_changed.swap(false, Ordering::AcqRel);
        if let Some(events) = self.codec_running.as_ref() {
            match events.try_recv() {
                Ok(result) => {
                    self.codec_worker_succeeded = Some(result.is_ok());
                    self.codec_error = result.err();
                    self.codec_running = None;
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    self.codec_worker_succeeded = Some(false);
                    self.codec_running = None;
                    changed = true;
                }
            }
        }
        if changed {
            let status = (setup.status)();
            if status != self.codec_status {
                self.codec_status = status;
            }
            self.invalidation.structure();
        }
    }

    /// Block until the install started from the page returns (tests).
    #[cfg(test)]
    fn wait_for_codec_install(&mut self) {
        if let Some(events) = self.codec_running.as_ref()
            && let Ok(result) = events.recv()
        {
            self.codec_worker_succeeded = Some(result.is_ok());
            self.codec_error = result.err();
            self.codec_running = None;
            self.codec_changed.store(true, Ordering::Release);
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
        self.finished
    }

    /// A tour is trusted modal chrome, but it deliberately keeps the real
    /// desktop visible underneath so each lesson points at its context.
    /// First-login identity setup remains an opaque machine surface until the
    /// person explicitly starts the tour.
    pub fn presents_desktop_context(&self) -> bool {
        self.tour_page.is_some()
    }

    /// Where this view shows a secret, if it shows one now: the restore step
    /// (the backup's passphrase and a nearby device's pairing code) and the
    /// backup step (the new passphrase and its confirmation). A capture draws
    /// this as the placeholder — see [`crate::shell::capture_protection`].
    ///
    /// The whole view, not just its card: the card slides as a step
    /// reveals, and the view is an opaque surface outside the tour anyway.
    pub fn secret_rect(&self) -> Option<Rect> {
        let shows_secret = self.tour_page.is_none()
            && match self.flow.screen() {
                Screen::Nearby | Screen::Restore => true,
                Screen::Done => !self.identity_ready_page,
                Screen::Intro | Screen::Existing | Screen::NoRecovery => false,
            };
        (shows_secret && self.rect.area() > 0.0).then_some(self.rect)
    }

    /// Reopen an explicitly deferred, source-attested migration for the active identity.
    ///
    /// # Errors
    ///
    /// Refuses missing, changed or completed declarations and unusable identities.
    pub fn new_for_migration_resume(identity_dir: PathBuf) -> Result<Self, String> {
        let active = runtime_identity::resolve_active_identity(&identity_dir)
            .ok_or_else(|| t("onboarding-portable-home-unavailable"))?;
        let identity = crate::onboarding::existing_identity_at(&active)
            .map_err(|_| t("onboarding-portable-home-unavailable"))?;
        let mut view = Self::new_in_context(identity_dir, OnboardingContext::FullSystem, false);
        if view.prepared_pending
            || !view
                .portable_home
                .as_ref()
                .is_some_and(crate::onboarding_portable_home::Pending::deferred)
        {
            return Err(t("onboarding-migration-resume-unavailable"));
        }
        view.flow.resume_created(identity);
        view.identity_ready_page = false;
        view.tour_page = None;
        view.invoke_migration("resume", None)
            .map_err(|()| t("onboarding-portable-home-unavailable"))?;
        Ok(view)
    }

    /// Reset this trusted view to the existing-identity backup step. Kept in
    /// one place so Settings enters the exact same export, picker, and
    /// verification flow as first-login onboarding.
    fn begin_portable_backup_for(&mut self, identity: MintedIdentity) {
        self.flow = Onboarding::new();
        self.flow.resume_created(identity);
        self.error = None;
        self.backup_error = None;
        self.backup_saved = None;
        self.backup_was_verified = false;
        self.backup_copy_form_open = false;
        self.backup_skip_acknowledged = false;
        self.identity_ready_page = false;
        self.joined_member = false;
        self.portable_backup_only = true;
        self.progress_error = None;
        // Steps another flow wrote the journal ahead of no longer apply, nor
        // does the outcome of identity work it started.
        self.journal.clear();
        self.identity_work = None;
        self.tour_page = None;
        self.tour_only = false;
        self.finished = false;
        self.invalidation.both();
    }

    fn restore_pending_progress_from_disk(&mut self) {
        // Recovery mode was selected because the existing identity could not
        // be used safely. A stale post-mint journal must not turn that state
        // into a Done screen that assumes signing works; the user must unlock
        // the keyring or restore first.
        if self.recovery_only {
            return;
        }
        let Some(identity_dir) = self.identity_dir.as_ref() else {
            return;
        };
        if !has_pending_progress(identity_dir) {
            return;
        }

        let (stage, load_error) = match load_progress(identity_dir) {
            Ok(Some(stage)) => (stage, None),
            Ok(None) => return,
            Err(error) => (PersistedStage::IdentityReady, Some(error.to_string())),
        };
        let identity_path = runtime_identity::resolve_active_identity(identity_dir)
            .unwrap_or_else(|| identity_dir.join("primary.json"));
        match crate::onboarding::existing_identity_at(&identity_path) {
            Ok(identity) => {
                self.joined_member = crate::onboarding::holds_no_identity_secret(&identity_path);
                self.flow.resume_created(identity);
                self.identity_ready_page = matches!(
                    stage,
                    PersistedStage::ReadyToEnter
                        | PersistedStage::ReadyToEnterBackedUp
                        | PersistedStage::TourNavigate
                        | PersistedStage::TourOrganise
                        | PersistedStage::TourCollaborate
                        | PersistedStage::TourStart
                );
                self.backup_was_verified = matches!(
                    stage,
                    PersistedStage::IdentityReadyBackedUp | PersistedStage::ReadyToEnterBackedUp
                );
                self.tour_page = match stage {
                    PersistedStage::AcquiringIdentity
                    | PersistedStage::IdentityReady
                    | PersistedStage::IdentityReadyBackedUp
                    | PersistedStage::ReadyToEnter
                    | PersistedStage::ReadyToEnterBackedUp => None,
                    PersistedStage::TourNavigate => Some(TourPage::Navigate),
                    PersistedStage::TourOrganise => Some(TourPage::Organise),
                    PersistedStage::TourCollaborate => Some(TourPage::Collaborate),
                    PersistedStage::TourStart => Some(TourPage::Start),
                };
                if let Some(error) = load_error {
                    self.progress_error = Some(t_args(
                        "onboarding-progress-unreadable",
                        &[("error", error.into())],
                    ));
                }
            }
            Err(error) => {
                // A pre-acquisition journal with no identity is the expected
                // result of a crash before the key transaction committed:
                // restart the creation flow. Anything present-but-invalid was
                // already rejected by normal startup and is surfaced here too.
                if std::fs::symlink_metadata(&identity_path).is_ok() {
                    self.progress_error = Some(t_args(
                        "onboarding-progress-invalid-identity",
                        &[("error", error.into())],
                    ));
                }
            }
        }
    }

    /// Write `change` to the crash journal, then take step `then`.
    ///
    /// The write runs on the state writer, never here on the compositor's
    /// loop. `then` waits in [`Self::journal`] and a later `sync` takes it
    /// once the change is on disk ([`Self::settle_journal`]); a failed write
    /// shows the journal error and drops it, so the step still never runs
    /// ahead of its journal. Without an identity directory there is no
    /// journal and `then` runs now.
    fn journal_then(&mut self, change: JournalChange, then: AfterJournal) {
        let Some(identity_dir) = self.identity_dir.clone() else {
            self.after_journal(then);
            return;
        };
        let outcome = Arc::new(std::sync::OnceLock::new());
        let written = Arc::clone(&outcome);
        // Made on the loop's thread, so it wakes the loop the step waits on;
        // a no-op where no loop installed a wake.
        let wake = crate::shell::search::invocation_wake::waker();
        let write = move || {
            let result = match change {
                JournalChange::Store(stage) => store_progress(&identity_dir, stage),
                JournalChange::Clear => clear_progress(&identity_dir),
            };
            let _ = written.set(result.map_err(|error| error.to_string()));
            wake.wake();
        };
        match self.journal_writer.submit_ordered(write) {
            Ok(()) => self.journal.push_back(JournalWrite {
                change,
                then,
                outcome,
            }),
            Err(error) => self.journal_failed(change, &error.to_string()),
        }
    }

    fn journal_failed(&mut self, change: JournalChange, error: &str) {
        let key = match change {
            JournalChange::Store(_) => "onboarding-progress-save-failed",
            JournalChange::Clear => "onboarding-progress-clear-failed",
        };
        self.progress_error = Some(t_args(key, &[("error", error.to_string().into())]));
        self.invalidation.structure();
    }

    /// Whether no journal change is outstanding, so an action that writes
    /// the journal may run now.
    fn journal_idle(&self) -> bool {
        self.journal.is_empty()
    }

    /// Take, oldest first, the steps whose journal changes have reported.
    fn settle_journal(&mut self) {
        while let Some(outcome) = self
            .journal
            .front()
            .and_then(|write| write.outcome.get().cloned())
        {
            let Some(write) = self.journal.pop_front() else {
                break;
            };
            match outcome {
                Ok(()) => {
                    self.progress_error = None;
                    self.after_journal(write.then);
                }
                Err(error) => self.journal_failed(write.change, &error),
            }
            self.invalidation.structure();
        }
    }

    /// Take the step a journal change was written ahead of.
    fn after_journal(&mut self, then: AfterJournal) {
        match then {
            AfterJournal::Nothing => {}
            AfterJournal::StartNearby => {
                if self.flow.screen() == Screen::Nearby && self.nearby.is_none() {
                    self.start_nearby_pairing();
                }
            }
            AfterJournal::EnterReadyAfterJoin => {
                if self.flow.is_done() && self.joined_member {
                    self.identity_ready_page = true;
                    self.codec_page = self.codec_step_offered;
                    self.invalidation.structure();
                }
            }
            AfterJournal::EnterReady => {
                if !self.portable_backup_only
                    && self.flow.is_done()
                    && self.tour_page.is_none()
                    && !self.identity_ready_page
                {
                    self.identity_ready_page = true;
                    // The journal already records the hand-off: a restart
                    // during the optional codecs offer resumes at the
                    // hand-off, where Settings still offers the codecs.
                    self.codec_page = self.codec_step_offered;
                    clear_secret(&self.backup_passphrase);
                    clear_secret(&self.backup_confirmation);
                    self.invalidation.both();
                }
            }
            AfterJournal::FinishPortableBackup => {
                if self.portable_backup_only && self.flow.is_done() && self.backup_was_verified {
                    self.finished = true;
                    clear_secret(&self.backup_passphrase);
                    clear_secret(&self.backup_confirmation);
                    self.invalidation.both();
                }
            }
            AfterJournal::Import(source) => {
                if self.flow.screen() == Screen::Restore {
                    self.import_identity(&source);
                }
            }
            AfterJournal::RetryMint => {
                if self.recovery_only && self.flow.screen() == Screen::Existing {
                    self.retry_existing_identity();
                }
            }
            AfterJournal::Mint => {
                // `can_create` is checked inside `complete` too: this is the
                // outer guard, that one is the invariant.
                if self.flow.can_create() {
                    self.mint_identity();
                }
            }
            AfterJournal::Tour(page) => self.tour_page = page,
            AfterJournal::Finish => {
                if self.flow.is_done() {
                    self.finish_ceremony();
                }
            }
        }
    }

    /// Whether `action` was pressed and may run now: an action that writes
    /// the journal waits, still pressed, while an earlier change is
    /// outstanding, so journal changes and their steps stay in order.
    fn take_journaled(&self, action: &str) -> bool {
        self.journal_idle() && self.identity_idle() && self.pressed.take(action)
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
    /// (an injected handler with no journal, as in review renderers), and
    /// the work runs now, as journal changes do there.
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
        let wake = crate::shell::search::invocation_wake::waker();
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
        }
        self.invalidation.structure();
    }

    fn finish_ceremony(&mut self) {
        self.finished = true;
        clear_secret(&self.connect_passphrase);
        clear_secret(&self.backup_passphrase);
        clear_secret(&self.backup_confirmation);
    }

    fn start_nearby_pairing(&mut self) {
        if let (Some(identity_dir), Some(status_path)) =
            (&self.identity_dir, &self.nearby_status_path)
        {
            let label = std::env::var("HOSTNAME").unwrap_or_else(|_| "Tilo device".into());
            let identity_root = std::env::var_os("TILO_IDENTITY_ROOT")
                .map_or_else(|| PathBuf::from("/var/lib/tilo"), PathBuf::from);
            match crate::onboarding_nearby::NearbyPairing::start(
                &identity_root,
                identity_dir,
                status_path,
                &label,
            ) {
                Ok(pairing) => {
                    self.nearby = Some(pairing);
                    self.nearby_status = Some(t("onboarding-nearby-waiting"));
                    self.nearby_code = None;
                    self.error = None;
                    self.nearby_failure = None;
                }
                Err(failure) => {
                    self.error = Some(failure.message());
                    self.nearby_failure = Some(failure);
                }
            }
        } else {
            self.error = Some(t("onboarding-nearby-unavailable"));
            self.nearby_failure = None;
        }
        self.invalidation.structure();
    }

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
                    self.journal_then(
                        JournalChange::Store(if self.identity_ready_page {
                            PersistedStage::ReadyToEnter
                        } else {
                            PersistedStage::IdentityReady
                        }),
                        AfterJournal::Nothing,
                    );
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

    fn finish_export(&mut self, exported: Result<PathBuf, String>) {
        match exported {
            Ok(destination) => {
                self.backup_saved = Some(destination);
                self.backup_was_verified = true;
                self.backup_copy_form_open = false;
                self.journal_then(
                    JournalChange::Store(PersistedStage::IdentityReadyBackedUp),
                    AfterJournal::Nothing,
                );
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

    fn progress_notice(&self, theme: &Theme) -> Option<Box<dyn Widget>> {
        self.progress_error.as_ref().map(|error| {
            spaced_banner(
                Banner::error(t("onboarding-progress-attention")).with_message(error.clone()),
                theme,
            )
        })
    }

    /// Put the entrance transition in its steady state for deterministic
    /// offscreen review. Production never calls this; the live compositor
    /// advances the exact same transition on animation frames.
    #[doc(hidden)]
    pub fn settle_motion_for_snapshot(&mut self) {
        self.transition = ui::motion::Presence::opened();
    }

    #[doc(hidden)]
    #[allow(
        clippy::too_many_lines,
        reason = "one fixture arm per reviewed onboarding state"
    )]
    pub fn set_visual_state_for_snapshot(&mut self, state: OnboardingVisualState) {
        self.flow = Onboarding::new();
        self.error = None;
        self.backup_error = None;
        self.backup_saved = None;
        self.backup_was_verified = false;
        self.backup_copy_form_open = false;
        self.identity_ready_page = false;
        self.joined_member = false;
        self.portable_backup_only = false;
        self.tour_page = None;
        self.tour_only = false;
        self.codec_page = false;
        self.codec_error = None;
        self.nearby_status = None;
        self.nearby_failure = None;
        self.nearby_code = None;
        self.nearby_active_for_snapshot = false;
        let minted = MintedIdentity {
            fingerprint: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            path: PathBuf::from("/var/lib/tilo/identities/primary.json"),
        };
        match state {
            OnboardingVisualState::Intro => {}
            OnboardingVisualState::Existing => self.flow.begin_existing(),
            OnboardingVisualState::Nearby | OnboardingVisualState::NearbyCode => {
                self.flow.begin_existing();
                self.flow.choose_nearby();
                self.nearby_active_for_snapshot = true;
                if state == OnboardingVisualState::NearbyCode {
                    self.nearby_code = Some("482913".into());
                    self.nearby_status = Some(t("onboarding-nearby-code"));
                } else {
                    self.nearby_status = Some(t("onboarding-nearby-waiting"));
                }
            }
            OnboardingVisualState::Restore | OnboardingVisualState::RestoreError => {
                self.flow.begin_existing();
                self.flow.choose_restore();
                if state == OnboardingVisualState::RestoreError {
                    self.error = Some(t("onboarding-snapshot-restore-error"));
                }
            }
            OnboardingVisualState::Protect | OnboardingVisualState::CreateError => {
                self.flow.begin();
                if state == OnboardingVisualState::CreateError {
                    self.error = Some(t("onboarding-snapshot-create-error"));
                }
            }
            OnboardingVisualState::Backup
            | OnboardingVisualState::BackupVerified
            | OnboardingVisualState::Ready => {
                self.flow.begin();
                self.flow.set_acknowledged(true);
                let _ = self.flow.complete(minted);
                self.backup_was_verified = state == OnboardingVisualState::BackupVerified;
                self.backup_saved = self.backup_was_verified.then(|| {
                    PathBuf::from("/home/quinten/Documents/tilo-identity-backup.tilo-identity")
                });
                self.identity_ready_page = state == OnboardingVisualState::Ready;
            }
            OnboardingVisualState::TourNavigate => {
                self.set_tour_visual_state_for_snapshot(minted, TourPage::Navigate);
            }
            OnboardingVisualState::TourOrganise => {
                self.set_tour_visual_state_for_snapshot(minted, TourPage::Organise);
            }
            OnboardingVisualState::TourCollaborate => {
                self.set_tour_visual_state_for_snapshot(minted, TourPage::Collaborate);
            }
            OnboardingVisualState::TourStart => {
                self.set_tour_visual_state_for_snapshot(minted, TourPage::Start);
            }
            OnboardingVisualState::Codecs
            | OnboardingVisualState::CodecsInstalling
            | OnboardingVisualState::CodecsWaitingForNetwork
            | OnboardingVisualState::CodecsInstalled
            | OnboardingVisualState::CodecsFailed => {
                self.flow.begin();
                self.flow.set_acknowledged(true);
                let _ = self.flow.complete(minted);
                self.backup_was_verified = true;
                self.identity_ready_page = true;
                self.codec_step_offered = true;
                self.codec_page = true;
                self.codec_status = CodecPackStatus::parse(match state {
                    OnboardingVisualState::CodecsInstalling => {
                        r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":5,"label":"Downloading","bytes":14000000,"total":38000000}}"#
                    }
                    OnboardingVisualState::CodecsWaitingForNetwork => {
                        r#"{"state":"waiting-for-network","desired":true}"#
                    }
                    OnboardingVisualState::CodecsInstalled => {
                        r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"enabled"},"desired":true}"#
                    }
                    OnboardingVisualState::CodecsFailed => {
                        r#"{"state":"failed","desired":true,"last_error":"Could not download the manifest (connection reset)."}"#
                    }
                    _ => r#"{"state":"not-installed"}"#,
                });
            }
        }
        self.composed = None;
        self.invalidation.both();
        self.settle_motion_for_snapshot();
    }

    fn set_tour_visual_state_for_snapshot(&mut self, minted: MintedIdentity, page: TourPage) {
        self.flow.begin();
        self.flow.set_acknowledged(true);
        let _ = self.flow.complete(minted);
        self.backup_was_verified = true;
        self.identity_ready_page = true;
        self.tour_page = Some(page);
    }

    #[expect(
        clippy::too_many_lines,
        reason = "a declarative page composition covering every onboarding screen"
    )]
    fn build_composition(&self, theme: &Theme, panel_height: f32) -> Box<dyn Widget> {
        if self.migration_active() {
            return self.migration_composition(theme);
        }
        if let Some(page) = self.tour_page {
            return self.build_tour_composition(theme, panel_height, page);
        }
        let compact_ready = self.flow.screen() == Screen::Done
            && self.identity_ready_page
            && theme.text_scale <= 1.0
            && !theme.name.eq_ignore_ascii_case("high contrast");
        let total = self.total_steps();
        let (step, stage) = if self.prepared_pending {
            (2, t("onboarding-stage-restore"))
        } else {
            match self.flow.screen() {
                Screen::Intro => (1, t("onboarding-stage-choose")),
                Screen::Existing | Screen::Nearby => (2, t("onboarding-stage-connect")),
                Screen::Restore => (2, t("onboarding-stage-restore")),
                Screen::NoRecovery => (2, t("onboarding-stage-protect")),
                Screen::Done if self.identity_ready_page && self.codec_page => {
                    (4, t("onboarding-stage-media"))
                }
                Screen::Done if self.identity_ready_page => (total, t("onboarding-stage-ready")),
                Screen::Done => (3, t("onboarding-stage-backup")),
            }
        };
        let body: Box<dyn Widget> = if self.prepared_pending {
            self.prepared_body(theme)
        } else {
            match self.flow.screen() {
                Screen::Intro => self.intro(theme),
                Screen::Existing => self.existing(theme),
                Screen::Nearby => self.nearby(theme),
                Screen::Restore => self.restore(theme),
                Screen::NoRecovery => self.no_recovery(theme),
                Screen::Done if self.identity_ready_page && self.codec_page => self.codecs(theme),
                Screen::Done if self.identity_ready_page => self.done(theme, compact_ready),
                Screen::Done => self.backup(theme),
            }
        };
        let spacing = &theme.tokens.spacing;
        let progress_value = f32::from(step) / f32::from(total);
        let progress = Column::new()
            .with_no_padding()
            // The header clears the panel's top edge by `s3`, and the
            // progress track stays `s3` inside both panel edges (HIG §9.5: a
            // rule inside a Panel never runs full-bleed into the corner
            // radius). The labels add their own `s3` so they sit on the
            // body's `s6` gutter.
            .with_extra_top_padding(spacing.s3)
            .with_horizontal_padding(spacing.s3, spacing.s3)
            .with_intrinsic_height()
            .with_gap(spacing.s3)
            .push(Box::new(
                Row::new()
                    .with_padding(spacing.s3, 0.0)
                    .push(Box::new(
                        Text::new(&t("onboarding-progress-eyebrow"))
                            .with_id("onboarding.progress.eyebrow")
                            .with_role(ui::tokens::typography::TextRole::Label)
                            .with_color(theme.text_dim),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Text::new(&t_args(
                            "onboarding-progress-stage",
                            &[("step", step.into()), ("total", total.into())],
                        ))
                        .with_id("onboarding.progress.stage")
                        .with_role(ui::tokens::typography::TextRole::Label)
                        .with_color(theme.text_dim),
                    )),
            ))
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_vertical_padding(spacing.s2)
                    .push(Box::new(ProgressBar::new(progress_value).with_label(
                        t_args(
                            "onboarding-progress-a11y",
                            &[
                                ("step", step.into()),
                                ("total", total.into()),
                                ("stage", stage.into()),
                            ],
                        ),
                    ))),
            ));
        let mut content = Column::new().with_intrinsic_height().with_gap(spacing.s4);
        if let Some(notice) = self.progress_notice(theme) {
            content = content.push(notice);
        }
        content = content.push(body);
        // Keep progression outside the scroll region; the backup explanation
        // and form share one body, with acknowledgment and Continue anchored.
        // Ready is exactly the full safe height on an 800x600 display. Its
        // two column gaps use s2 so the complete hand-off fits without an
        // ornamental 8 px scroll range; the footer still retains the 8 px
        // separation required by the interaction contract.
        let composition_gap = if self.identity_ready_page {
            spacing.s2
        } else {
            spacing.s3
        };
        let footer_gap = composition_gap;
        let backup_in_scroll = self.flow.screen() == Screen::Done && !self.identity_ready_page;
        let acknowledgement_footer = matches!(self.flow.screen(), Screen::NoRecovery);
        let choice_footer = self.choice_footer_height(theme);
        let backup_acknowledgement_visible =
            backup_in_scroll && !self.portable_backup_only && !self.backup_was_verified;
        let footer_height = if backup_in_scroll {
            FOOTER_HEIGHT
                + if backup_acknowledgement_visible {
                    ACKNOWLEDGEMENT_FOOTER_EXTRA_HEIGHT
                } else {
                    0.0
                }
        } else if let Some(height) = choice_footer {
            height
        } else {
            FOOTER_HEIGHT
                + if acknowledgement_footer {
                    ACKNOWLEDGEMENT_FOOTER_EXTRA_HEIGHT
                } else {
                    0.0
                }
        };
        let scroll_height =
            (panel_height - PROGRESS_HEADER_HEIGHT - footer_height - footer_gap).max(120.0);
        let closing_ready =
            self.flow.screen() == Screen::Done && self.identity_ready_page && !self.codec_page;
        let fill_panel = !closing_ready
            && (theme.text_scale > 1.0
                || theme.name.eq_ignore_ascii_case("high contrast")
                || matches!(
                    self.flow.screen(),
                    Screen::Intro | Screen::Nearby | Screen::Restore | Screen::NoRecovery
                )
                || self.flow.screen() == Screen::Done);
        let mut scroll = Scrollable::new_vertical();
        if backup_in_scroll {
            // Backup is a form: Tab should enter its first field/control, and
            // focus-follow scrolling reveals later controls. Explanatory
            // pages retain the viewport stop for Page/Home/End navigation.
            scroll = scroll.without_sequential_focus().with_focus_reveal();
        }
        let mut scroll = scroll;
        if !backup_in_scroll {
            // This is first-run guidance, not an incidental list. Keep the
            // thumb visible whenever content overflows so compact and large
            // text users have an unmistakable cue that more explanation is
            // available below the sticky footer.
            scroll = scroll.always_visible();
        }
        let bottom_content_inset = if compact_ready { 0.0 } else { spacing.s3 };
        let scroll = scroll
            // Keep a clean terminal edge even on Backup's in-flow decision
            // block, so the last control is not painted against the clip.
            // Normal-text Ready already has the parent's `s3` footer gap;
            // repeating it inside the scroller was the last 12 px of a
            // needless 800×600 overflow.
            .with_bottom_content_inset(bottom_content_inset)
            // A 960×540 large-text/high-contrast dialog resolves to a 492 px
            // scroll viewport. Keep its first frame's edge in the gap between
            // the form card and recovery copy; the full-height 672 px dialog
            // retains room for the acknowledgement and Continue action.
            .with_compact_bottom_content_inset(520.0, 0.0)
            .with_child(Box::new(content));
        let scroll = if fill_panel {
            scroll.fill_parent_height()
        } else {
            scroll.with_max_height(scroll_height)
        };
        let composition = Column::new()
            .with_no_padding()
            .with_gap(composition_gap)
            .push(Box::new(progress))
            .push(Box::new(scroll));
        if backup_in_scroll {
            Box::new(composition.push(self.backup_decision_footer(theme)))
        } else {
            Box::new(composition.push(self.identity_footer(theme)))
        }
    }

    /// The recovery form lives with its explanation in one scrolling body.
    fn backup_form(&self, theme: &Theme) -> Box<dyn Widget> {
        let explanation = paragraph(&t("onboarding-backup-explanation"), theme);
        let fields = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s2)
            .push(labelled_secret_field(
                "onboarding.backup.passphrase",
                &t("onboarding-backup-passphrase"),
                &self.backup_passphrase,
                &t("onboarding-backup-passphrase"),
                &t("onboarding-backup-passphrase-placeholder"),
            ))
            .push(labelled_secret_field(
                "onboarding.backup.confirmation",
                &t("onboarding-backup-confirmation"),
                &self.backup_confirmation,
                &t("onboarding-backup-confirmation"),
                &t("onboarding-backup-confirmation-placeholder"),
            ));
        let action = self
            .pressed
            .button(
                "onboarding.backup.button",
                &if self.backup_was_verified {
                    t("onboarding-backup-save-another")
                } else {
                    t("onboarding-backup-save")
                },
                KEY_BACKUP_BROWSE,
            )
            .role(if self.backup_was_verified {
                // Continue remains the clear completion action after a verified
                // backup; making another copy is an optional follow-up task.
                ui::widget::ButtonRole::Secondary
            } else {
                ui::widget::ButtonRole::Primary
            });
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(theme.tokens.spacing.s2)
                .push(explanation)
                .push(Box::new(fields))
                .push(footer_actions(vec![Box::new(action)])),
        )
    }

    fn backup_decision_footer(&self, theme: &Theme) -> Box<dyn Widget> {
        if self.portable_backup_only || self.backup_was_verified {
            return self.identity_actions();
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_horizontal_padding(theme.tokens.spacing.s3, theme.tokens.spacing.s3)
                .with_vertical_padding(theme.tokens.spacing.s2)
                .with_gap(theme.tokens.spacing.s3)
                .push(Box::new(
                    Checkbox::new(
                        "onboarding.backup.skip-acknowledged.box",
                        KEY_BACKUP_SKIP_ACKNOWLEDGED,
                    )
                    .with_id("onboarding.backup.skip-acknowledged.row")
                    .with_label(t("onboarding-backup-skip-acknowledgement"))
                    .on_change({
                        let pending = Rc::clone(&self.acknowledgements);
                        move |checked| {
                            pending
                                .borrow_mut()
                                .push((AcknowledgementKind::SkipBackup, checked));
                        }
                    }),
                ))
                .push(self.identity_actions()),
        )
    }

    /// Pairing with a nearby device is under way.
    fn nearby_active(&self) -> bool {
        self.nearby.is_some() || self.nearby_active_for_snapshot
    }

    /// The ways forward from a choice screen, one row per flow.
    fn choices(&self) -> Option<[Choice; 2]> {
        if self.prepared_pending {
            return None;
        }
        match self.flow.screen() {
            Screen::Intro => Some([
                Choice {
                    id: "onboarding.existing.card",
                    icon: ui::icons::Icon::User,
                    title: "onboarding-choice-existing",
                    detail: "onboarding-choice-existing-detail",
                    key: KEY_EXISTING,
                },
                Choice {
                    id: "onboarding.begin.card",
                    icon: ui::icons::Icon::Plus,
                    title: "onboarding-choice-create",
                    detail: "onboarding-choice-create-detail",
                    key: KEY_BEGIN,
                },
            ]),
            Screen::Existing => Some([
                Choice {
                    id: "onboarding.existing.nearby.card",
                    icon: ui::icons::Icon::Monitor,
                    title: "onboarding-choice-nearby",
                    detail: "onboarding-choice-nearby-detail",
                    key: KEY_CHOOSE_NEARBY,
                },
                Choice {
                    id: "onboarding.existing.restore.card",
                    icon: ui::icons::Icon::Archive,
                    title: "onboarding-choice-restore",
                    detail: "onboarding-choice-restore-detail",
                    key: KEY_CHOOSE_RESTORE,
                },
            ]),
            _ => None,
        }
    }

    /// The pinned footer of a choice screen: its rows stay in view at every
    /// size, like the action row of the other screens. Intro has nothing to
    /// go back to; the use-existing choice keeps its Back (or keyring retry)
    /// row below the rows.
    fn choice_footer_height(&self, theme: &Theme) -> Option<f32> {
        let choices = self.choices()?;
        let count = f32::from(u8::try_from(choices.len()).unwrap_or(u8::MAX));
        let stack = count * choice_row_height(theme);
        let spacing = &theme.tokens.spacing;
        Some(if self.flow.screen() == Screen::Intro {
            stack + spacing.s4
        } else {
            stack + spacing.s2 + FOOTER_HEIGHT
        })
    }

    fn choice_footer(&self, theme: &Theme, choices: [Choice; 2]) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let rows = choices
            .into_iter()
            .map(|choice| {
                let pressed = self.pressed.clone();
                Box::new(
                    ListRow::new(choice.id, t(choice.title))
                        .icon(choice.icon)
                        .secondary(t(choice.detail))
                        .disclosure()
                        .on_activate(move || pressed.press(choice.key)),
                ) as Box<dyn Widget>
            })
            .collect();
        let stack = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_horizontal_padding(spacing.s3, spacing.s3)
            .push(Box::new(Section::new(rows)));
        if self.flow.screen() == Screen::Intro {
            return Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(0.0)
                    .push(Box::new(stack))
                    // The intro's rows have no action row below them; keep
                    // the action row's visible inset from the panel edge.
                    .push(Box::new(Spacer::fixed(spacing.s4))),
            );
        }
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(spacing.s2)
                .push(Box::new(stack))
                .push(self.identity_actions()),
        )
    }

    fn identity_footer(&self, theme: &Theme) -> Box<dyn Widget> {
        if let Some(choices) = self.choices() {
            return self.choice_footer(theme, choices);
        }
        let acknowledgement = match self.flow.screen() {
            Screen::NoRecovery => Some(
                Checkbox::new("onboarding.understood.box", KEY_UNDERSTOOD)
                    .with_id("onboarding.understood.row")
                    .with_label(acknowledgement())
                    .on_change({
                        let pending = Rc::clone(&self.acknowledgements);
                        move |checked| {
                            pending
                                .borrow_mut()
                                .push((AcknowledgementKind::IdentityProtection, checked));
                        }
                    }),
            ),
            _ => None,
        };
        let Some(acknowledgement) = acknowledgement else {
            return self.identity_actions();
        };
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                // Match the scroll content gutter so a long acknowledgement
                // measures and wraps inside the panel rather than running
                // into its edge. The vertical inset is part of the sticky
                // footer's reserved height and keeps the label distinct from
                // the action row below it.
                .with_horizontal_padding(theme.tokens.spacing.s3, theme.tokens.spacing.s3)
                .with_vertical_padding(theme.tokens.spacing.s2)
                .with_gap(theme.tokens.spacing.s3)
                .push(Box::new(acknowledgement))
                .push(self.identity_actions()),
        )
    }

    #[expect(
        clippy::too_many_lines,
        reason = "a declarative footer builder with one arm per onboarding screen"
    )]
    fn identity_actions(&self) -> Box<dyn Widget> {
        if self.prepared_pending {
            return footer_actions(vec![Box::new(
                self.pressed
                    .button(
                        "onboarding.prepared.import.button",
                        &t("onboarding-continue"),
                        KEY_PREPARED_IMPORT,
                    )
                    .role(ui::widget::ButtonRole::Primary)
                    .with_enabled(self.prepared_worker.is_none()),
            )]);
        }
        match self.flow.screen() {
            Screen::Intro => footer_actions(Vec::new()),
            Screen::Existing if self.recovery_only => footer_actions(vec![Box::new(
                self.pressed
                    .button(
                        "onboarding.connect.retry.button",
                        &t("onboarding-connect-retry-keyring"),
                        KEY_CONNECT_RETRY,
                    )
                    .with_icon(ui::icons::Icon::Refresh),
            )]),
            Screen::Existing => footer_actions(vec![back_button(&self.pressed)]),
            Screen::Nearby => {
                let mut actions = vec![back_button(&self.pressed)];
                if self.nearby_active() {
                    actions.push(Box::new(self.pressed.button(
                        "onboarding.connect.cancel.button",
                        &t("onboarding-cancel"),
                        KEY_CONNECT_CANCEL,
                    )));
                } else {
                    actions.push(Box::new(
                        self.pressed
                            .button(
                                "onboarding.connect.nearby.button",
                                &t("onboarding-nearby-try-again"),
                                KEY_CONNECT_NEARBY,
                            )
                            .with_icon(ui::icons::Icon::Refresh)
                            .role(ui::widget::ButtonRole::Primary),
                    ));
                }
                footer_actions(actions)
            }
            Screen::Restore => footer_actions(vec![
                back_button(&self.pressed),
                Box::new(
                    self.pressed
                        .button(
                            "onboarding.import.button",
                            &t("onboarding-restore-backup"),
                            KEY_IMPORT,
                        )
                        .with_icon(ui::icons::Icon::ArrowRight)
                        .role(ui::widget::ButtonRole::Primary)
                        .with_enabled(self.connect_source_selected),
                ),
            ]),
            Screen::NoRecovery => footer_actions(vec![
                back_button(&self.pressed),
                Box::new(
                    self.pressed
                        .button(
                            "onboarding.create.button",
                            &t("onboarding-create-local-identity"),
                            KEY_CREATE,
                        )
                        .with_icon(ui::icons::Icon::Shield)
                        .role(ui::widget::ButtonRole::Primary)
                        .with_enabled(self.flow.can_create()),
                ),
            ]),
            Screen::Done if self.identity_ready_page && self.codec_page => self.codec_actions(),
            Screen::Done if self.identity_ready_page => self.finish_action(),
            Screen::Done if self.portable_backup_only => footer_actions(vec![
                Box::new(self.pressed.button(
                    "onboarding.backup.cancel.button",
                    &t("onboarding-cancel"),
                    KEY_FINISH,
                )),
                Box::new(
                    self.pressed
                        .button(
                            "onboarding.backup.done.button",
                            &t("onboarding-done"),
                            KEY_BACKUP_CONTINUE,
                        )
                        .role(ui::widget::ButtonRole::Primary)
                        .with_enabled(self.backup_was_verified),
                ),
            ]),
            Screen::Done if self.backup_was_verified && self.backup_copy_form_open => {
                footer_actions(vec![Box::new(
                    self.pressed
                        .button(
                            "onboarding.backup.continue.button",
                            &t("onboarding-continue"),
                            KEY_BACKUP_CONTINUE,
                        )
                        .role(ui::widget::ButtonRole::Primary),
                )])
            }
            Screen::Done if self.backup_was_verified => footer_actions(vec![
                Box::new(self.pressed.button(
                    "onboarding.backup.another.button",
                    &t("onboarding-backup-save-another"),
                    KEY_BACKUP_ANOTHER,
                )),
                Box::new(
                    self.pressed
                        .button(
                            "onboarding.backup.continue.button",
                            &t("onboarding-continue"),
                            KEY_BACKUP_CONTINUE,
                        )
                        .role(ui::widget::ButtonRole::Primary),
                ),
            ]),
            Screen::Done => footer_actions(vec![Box::new(
                self.pressed
                    .button(
                        "onboarding.backup.continue.button",
                        &t("onboarding-backup-continue-without"),
                        KEY_BACKUP_CONTINUE,
                    )
                    .role(ui::widget::ButtonRole::Secondary)
                    .with_enabled(self.backup_skip_acknowledged),
            )]),
        }
    }

    fn build_tour_composition(
        &self,
        theme: &Theme,
        panel_height: f32,
        page: TourPage,
    ) -> Box<dyn Widget> {
        let number = page.number();
        let stage = match page {
            TourPage::Navigate => t("onboarding-tour-stage-move"),
            TourPage::Organise => t("onboarding-tour-stage-arrange"),
            TourPage::Collaborate => t("onboarding-tour-stage-share"),
            TourPage::Start => t("onboarding-tour-stage-begin"),
        };
        let progress = Column::new()
            .with_no_padding()
            // Same header rhythm as the identity pages: `s3` off the panel's
            // top edge, the track `s3` inside both edges, labels on the `s6`
            // gutter.
            .with_extra_top_padding(theme.tokens.spacing.s3)
            .with_horizontal_padding(theme.tokens.spacing.s3, theme.tokens.spacing.s3)
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s3)
            .push(Box::new(
                Row::new()
                    .with_padding(theme.tokens.spacing.s3, 0.0)
                    .push(Box::new(
                        Text::new(&t("onboarding-tour-eyebrow"))
                            .with_role(ui::tokens::typography::TextRole::Label)
                            .with_color(theme.text_dim),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Text::new(&t_args("onboarding-tour-stage", &[("step", number.into())]))
                            .with_role(ui::tokens::typography::TextRole::Label)
                            .with_color(theme.text_dim),
                    )),
            ))
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_vertical_padding(theme.tokens.spacing.s2)
                    .push(Box::new(
                        ProgressBar::new(f32::from(number) / 4.0).with_label(t_args(
                            "onboarding-tour-progress-a11y",
                            &[("stage", stage.into()), ("step", number.into())],
                        )),
                    )),
            ));
        let mut content = Column::new()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s6);
        if let Some(notice) = self.progress_notice(theme) {
            content = content.push(notice);
        }
        content = content.push(Self::tour_page(theme, page));
        // Keep navigation out of the scrolling lesson. On a compact laptop,
        // portrait display, or large-text setup the cards can be much taller
        // than the panel; making Continue part of that content forced people
        // to discover scrolling before they could finish the introduction.
        // The identity Done screen uses the same sticky-footer contract.
        let footer_gap = theme.tokens.spacing.s3;
        let scroll_height =
            (panel_height - PROGRESS_HEADER_HEIGHT - FOOTER_HEIGHT - footer_gap).max(120.0);
        Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(footer_gap)
                .push(Box::new(progress))
                .push(Box::new(
                    Scrollable::new_vertical()
                        .with_max_height(scroll_height)
                        // Tour pages intentionally teach several concepts;
                        // expose overflow persistently instead of making a
                        // first-time user wait for a transient wheel hint.
                        .always_visible()
                        .with_child(Box::new(content)),
                ))
                .push(self.tour_actions(page)),
        )
    }

    fn tour_page(theme: &Theme, page: TourPage) -> Box<dyn Widget> {
        let (title, intro, cards): (&str, &str, &[(&str, &str)]) = match page {
            TourPage::Navigate => (
                "onboarding-tour-navigate-title",
                "onboarding-tour-navigate-intro",
                NAVIGATE_CARDS,
            ),
            TourPage::Organise => (
                "onboarding-tour-organise-title",
                "onboarding-tour-organise-intro",
                ORGANISE_CARDS,
            ),
            TourPage::Collaborate => (
                "onboarding-tour-collaborate-title",
                "onboarding-tour-collaborate-intro",
                COLLABORATE_CARDS,
            ),
            TourPage::Start => (
                "onboarding-tour-start-title",
                "onboarding-tour-start-intro",
                START_CARDS,
            ),
        };
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            // The final page is an actionable checklist, not four unrelated
            // ideas. A single grouped checklist preserves the hierarchy and
            // keeps every first-session task above the sticky navigation on a
            // laptop-height display.
            .with_gap(if page == TourPage::Start {
                theme.tokens.spacing.s4
            } else {
                theme.tokens.spacing.s6
            })
            .push(heading(&t(title), theme))
            .push(paragraph(&t(intro), theme));
        if page == TourPage::Start {
            let mut checklist = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(theme.tokens.spacing.s2);
            for (number, (card_title, copy)) in (1_usize..).zip(cards) {
                checklist = checklist.push(Box::new(
                    Row::new()
                        .no_wrap()
                        .align_start()
                        .with_gap(theme.tokens.spacing.s3)
                        .push(Box::new(
                            Text::new(&number.to_string())
                                .with_role(ui::tokens::typography::TextRole::Label)
                                .strong()
                                .numeric()
                                .with_color(theme.text_dim),
                        ))
                        .push(Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .with_gap(theme.tokens.spacing.s1)
                                .push(Box::new(
                                    Text::new(&t(card_title))
                                        .with_role(ui::tokens::typography::TextRole::Label)
                                        .with_color(theme.text_primary),
                                ))
                                .push(paragraph(&t(copy), theme)),
                        )),
                ));
            }
            // The step numbers already provide the grouping and order. A
            // second titled/padded card spent nearly a full row of a laptop
            // viewport on decoration and pushed the last action below the
            // sticky footer.
            body = body.push(Box::new(checklist));
        } else {
            for (card_title, copy) in cards {
                let content: Box<dyn Widget> = match tour_shortcut(card_title) {
                    Some(keys) => Box::new(
                        Column::new()
                            .with_no_padding()
                            .with_intrinsic_height()
                            .with_gap(theme.tokens.spacing.s1)
                            .push(Box::new(
                                Text::new(&t(keys))
                                    .with_role(ui::tokens::typography::TextRole::Label)
                                    .with_color(theme.text_primary),
                            ))
                            .push(paragraph(&t(copy), theme)),
                    ),
                    None => paragraph(&t(copy), theme),
                };
                body = body.push(Box::new(Section::titled(t(card_title), vec![content])));
            }
        }
        Box::new(body)
    }

    /// Sticky tour navigation, kept outside the scrollable lesson body.
    fn tour_actions(&self, page: TourPage) -> Box<dyn Widget> {
        let (primary_label, primary_key) = if page == TourPage::Start {
            (t("onboarding-tour-enter"), KEY_FINISH)
        } else {
            (t("onboarding-continue"), KEY_TOUR_NEXT)
        };
        footer_actions(vec![
            Box::new(self.pressed.button(
                "onboarding.tour.skip.button",
                &t("onboarding-tour-skip"),
                KEY_TOUR_SKIP,
            )),
            Box::new(self.pressed.button(
                "onboarding.tour.back.button",
                &t("onboarding-back"),
                KEY_TOUR_BACK,
            )),
            Box::new(
                self.pressed
                    .button("onboarding.tour.next.button", &primary_label, primary_key)
                    .role(ui::widget::ButtonRole::Primary),
            ),
        ])
    }

    fn intro(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let (title, body) = match self.context {
            OnboardingContext::FullSystem => (intro_title(), intro_body()),
            OnboardingContext::HostDesktop => {
                (host_desktop_intro_title(), host_desktop_intro_body())
            }
        };
        let icons = [
            ui::icons::Icon::Users,
            ui::icons::Icon::HardDrive,
            ui::icons::Icon::Lock,
        ];
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(spacing.s4)
                .push(heading(&title, theme))
                .push(paragraph(&body, theme))
                .push(fact_list(icons.into_iter().zip(intro_points()), theme)),
        )
    }

    /// How to bring an identity that already exists to this computer. Each
    /// way opens its own screen from the cards in the footer.
    fn existing(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s4)
            .push(heading(&t("onboarding-existing-title"), theme))
            .push(paragraph(&t("onboarding-existing-body"), theme));
        if self.recovery_only {
            body = body.push(spaced_banner(
                Banner::warning(t("onboarding-connect-recovery-title"))
                    .with_message(t("onboarding-connect-recovery-message"))
                    .with_icon(ui::icons::Icon::Shield),
                theme,
            ));
        }
        if let Some(error) = &self.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-connect-failed")).with_message(error.clone()),
                theme,
            ));
        }
        Box::new(body)
    }

    /// Receive the identity from a trusted device: what to do on the other
    /// device, then the shared verification code once it appears.
    fn nearby(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let mut steps = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s3);
        for (number, key) in (1_usize..).zip([
            "onboarding-nearby-step-open",
            "onboarding-nearby-step-add",
            "onboarding-nearby-step-compare",
        ]) {
            steps = steps.push(numbered_step(number, &t(key), theme));
        }
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s4)
            .push(heading(&t("onboarding-nearby-title"), theme))
            .push(paragraph(&t("onboarding-nearby-body"), theme));
        if let Some(error) = &self.error {
            // The banner's title is what went wrong; a screen reader speaks
            // it first, so it names the cause rather than only the outcome.
            let banner = match &self.nearby_failure {
                Some(failure) => Banner::error(failure.title()).with_message(failure.message()),
                None => Banner::error(t("onboarding-connect-failed")).with_message(error.clone()),
            };
            body = body.push(spaced_banner(banner, theme));
        }
        let mut status = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s2);
        if let Some(code) = &self.nearby_code {
            status = status
                .push(Box::new(
                    Text::new(&t("onboarding-nearby-code-label"))
                        .with_role(ui::tokens::typography::TextRole::Label)
                        .with_color(theme.text_dim),
                ))
                .push(Box::new(
                    Text::new(&grouped_code(code))
                        .with_id("onboarding.nearby.code")
                        .with_role(ui::tokens::typography::TextRole::Display)
                        .numeric(),
                ));
        }
        if let Some(message) = &self.nearby_status {
            let text = Text::new(message)
                // A stable semantic address lets the accessibility host
                // recognise verification-code and completion updates as
                // changes to one status region across composition rebuilds.
                .with_id("onboarding.nearby.status")
                .with_role(ui::tokens::typography::TextRole::Body)
                .with_color(theme.text_dim)
                .with_live(ui::a11y::Politeness::Polite);
            status = if self.nearby_active() && self.nearby_code.is_none() {
                status.push(Box::new(
                    Row::new()
                        .no_wrap()
                        .with_gap(theme.tokens.spacing.s2)
                        .push(Box::new(Spinner::with_size(theme.tokens.metrics.icon_md)))
                        .push(Box::new(text)),
                ))
            } else {
                status.push(Box::new(text))
            };
        }
        Box::new(
            body.push(Box::new(Section::new(vec![Box::new(steps)])))
                .push(Box::new(status)),
        )
    }

    /// Restore the identity from an encrypted backup file.
    fn prepared_body(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s4)
            .push(heading(&t("onboarding-prepared-title"), theme))
            .push(paragraph(&t("onboarding-prepared-body"), theme));
        if let Some(error) = &self.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-prepared-failed")).with_message(error.clone()),
                theme,
            ));
        }
        if self.prepared_worker.is_some() {
            body = body.push(paragraph(&t("onboarding-prepared-working"), theme));
        } else if let Some(peer) = &self.peer {
            // The package arrives over the peer transport, so there is no
            // password to type. Show the same code the preparing computer
            // shows; a person compares them before approving this computer
            // there. A code appears only once this receiver has bound its own
            // endpoint and key.
            if let Some(code) = peer.verification_code() {
                body = body.push(paragraph(
                    &t_args("onboarding-prepared-peer-code", &[("code", code.into())]),
                    theme,
                ));
            } else {
                body = body.push(paragraph(&t("onboarding-prepared-peer-waiting"), theme));
            }
        } else {
            body = body.push(Box::new(secret_input(
                "onboarding.prepared.password",
                &self.connect_passphrase,
                &t("onboarding-prepared-password"),
                &t("onboarding-prepared-password"),
            )));
        }
        Box::new(body)
    }

    fn migration_active(&self) -> bool {
        !self.prepared_pending
            && !self.recovery_only
            && !self.portable_backup_only
            && self.flow.is_done()
            && (self
                .portable_home
                .as_ref()
                .is_some_and(crate::onboarding_portable_home::Pending::pending)
                || (self
                    .portable_applications
                    .as_ref()
                    .is_some_and(crate::onboarding_portable_applications::Pending::pending)
                    && !self
                        .portable_home
                        .as_ref()
                        .is_some_and(crate::onboarding_portable_home::Pending::deferred))
                || self
                    .browser
                    .as_ref()
                    .is_some_and(crate::onboarding_browser::Pending::pending)
                || self
                    .migration
                    .as_ref()
                    .is_some_and(crate::onboarding_migration::Migration::pending))
    }
    pub(crate) fn migration_status(&self) -> Option<serde_json::Value> {
        if !self.prepared_pending
            && self.flow.is_done()
            && let Some(portable) = self.portable_home.as_ref()
            && portable.deferred()
        {
            return Some(portable.status());
        }
        if !self.prepared_pending
            && self.flow.is_done()
            && !self
                .portable_home
                .as_ref()
                .is_some_and(|home| home.pending() || home.deferred())
            && let Some(applications) = self
                .portable_applications
                .as_ref()
                .filter(|pending| pending.deferred())
        {
            return Some(applications.status());
        }
        if !self.prepared_pending
            && self.flow.is_done()
            && !self.migration_active()
            && let Some(browser) = self.browser.as_ref().filter(|pending| pending.deferred())
        {
            return Some(browser.status());
        }
        self.migration_active()
            .then(|| {
                if let Some(portable) = self
                    .portable_home
                    .as_ref()
                    .filter(|pending| pending.pending())
                {
                    Some(portable.status())
                } else if let Some(applications) = self
                    .portable_applications
                    .as_ref()
                    .filter(|pending| pending.pending())
                {
                    Some(applications.status())
                } else if let Some(browser) =
                    self.browser.as_ref().filter(|pending| pending.pending())
                {
                    Some(browser.status())
                } else {
                    self.migration
                        .as_ref()
                        .map(crate::onboarding_migration::Migration::status)
                }
            })
            .flatten()
    }
    fn refresh_backup_capability(&mut self) -> Result<(), ()> {
        let path = &self.flow.minted().ok_or(())?.path;
        self.backup_export_available = (self.backup_capability)(path).map_err(|_| ())?;
        if !self.backup_export_available && !self.identity_ready_page && self.tour_page.is_none() {
            // No recovery share is available here: omit the irrelevant form.
            // Migration composition still takes precedence over this hand-off.
            self.identity_ready_page = true;
            self.codec_page = self.codec_step_offered;
            self.journal_then(
                JournalChange::Store(PersistedStage::ReadyToEnter),
                AfterJournal::Nothing,
            );
        }
        Ok(())
    }
    fn accept_connected_identity(&mut self, identity: MintedIdentity) -> Result<(), ()> {
        if !self.flow.complete_connected(identity) {
            return Err(());
        }
        self.joined_member =
            crate::onboarding::holds_no_identity_secret(&self.flow.minted().ok_or(())?.path);
        self.identity_ready_page = false;
        if self.joined_member {
            // A joined member holds no identity secret, so there is nothing
            // to back up. The journal records the hand-off first and the
            // ready screen follows it, so a restart resumes there.
            self.backup_export_available = false;
            self.codec_page = false;
            self.journal_then(
                JournalChange::Store(PersistedStage::ReadyToEnter),
                AfterJournal::EnterReadyAfterJoin,
            );
        } else {
            self.refresh_backup_capability()?;
            let stage = if self.identity_ready_page {
                PersistedStage::ReadyToEnter
            } else {
                PersistedStage::IdentityReady
            };
            self.journal_then(JournalChange::Store(stage), AfterJournal::Nothing);
            self.codec_page = self.identity_ready_page && self.codec_step_offered;
        }
        self.invalidation.both();
        Ok(())
    }
    pub(crate) fn backup_status(&self) -> Option<serde_json::Value> {
        if self.prepared_pending
            || self.migration_active()
            || !self.flow.is_done()
            || self.identity_ready_page
            || self.tour_page.is_some()
            || self.finished
        {
            return None;
        }
        Some(serde_json::json!({
            "state": if self.backup_was_verified { "verified" }
                else if self.backup_error.is_some() { "failed" } else { "ready" },
            "can_save": self.backup_export_available && self.identity_idle() && self.journal_idle()
                && self.picker.is_none() && self.error.is_none(),
        }))
    }
    pub(crate) fn submit_backup(
        &mut self,
        destination: &str,
        passphrase: &[u8],
        confirmation: &[u8],
    ) -> Result<(), ()> {
        if !self
            .backup_status()
            .is_some_and(|status| json_flag(&status, "can_save"))
            || destination.len() > 4096
            || !Path::new(destination).is_absolute()
            || passphrase.len() > 4096
            || passphrase != confirmation
            || std::str::from_utf8(passphrase)
                .map_err(|_| ())?
                .chars()
                .count()
                < 12
        {
            return Err(());
        }
        self.start_backup_export(
            PathBuf::from(destination),
            Zeroizing::new(passphrase.to_vec()),
        )
    }
    pub(crate) fn continue_backup(&mut self, acknowledge_without_backup: bool) -> Result<(), ()> {
        if self.backup_status().is_none()
            || self.error.is_some()
            || self.picker.is_some()
            || !self.identity_idle()
            || !self.journal_idle()
            || (self.backup_export_available
                && !self.backup_was_verified
                && !acknowledge_without_backup)
        {
            return Err(());
        }
        self.backup_continue_ack = Some(acknowledge_without_backup);
        self.press(KEY_BACKUP_CONTINUE);
        self.invalidation.both();
        Ok(())
    }
    /// Resume the deferred migration `operation` names (or the first one
    /// deferred): the portable home, then its applications, then the
    /// browser. `None` when nothing deferred is waiting.
    fn resume_deferred(&mut self, operation: Option<&str>) -> Option<Result<(), ()>> {
        if !self.prepared_pending
            && self.flow.is_done()
            && let Some(portable) = self.portable_home.as_mut()
        {
            if operation.is_some_and(|selected| portable.operation() != selected) {
                return Some(Err(()));
            }
            let result = portable.invoke("resume", b"").map_err(|_| ());
            self.invalidation.both();
            return Some(result);
        }
        if !self.prepared_pending
            && self.flow.is_done()
            && !self
                .portable_home
                .as_ref()
                .is_some_and(|home| home.pending() || home.deferred())
            && let Some(applications) = self
                .portable_applications
                .as_mut()
                .filter(|pending| pending.deferred())
        {
            if operation.is_some_and(|selected| applications.operation() != selected) {
                return Some(Err(()));
            }
            let result = applications.invoke("resume", b"").map_err(|_| ());
            self.invalidation.both();
            return Some(result);
        }
        if !self.prepared_pending
            && self.flow.is_done()
            && let Some(browser) = self.browser.as_mut().filter(|pending| pending.deferred())
        {
            if operation.is_some_and(|selected| browser.operation() != selected) {
                return Some(Err(()));
            }
            let result = browser.invoke("resume", b"").map_err(|_| ());
            self.invalidation.both();
            return Some(result);
        }
        None
    }
    pub(crate) fn invoke_migration(&mut self, op: &str, operation: Option<&str>) -> Result<(), ()> {
        if op == "resume"
            && let Some(resumed) = self.resume_deferred(operation)
        {
            return resumed;
        }
        if !self.migration_active() {
            return Err(());
        }
        if let Some(portable) = self
            .portable_home
            .as_mut()
            .filter(|pending| pending.pending())
        {
            if operation.is_some_and(|selected| portable.operation() != selected) {
                return Err(());
            }
            let op = if op == "next" {
                portable.next_action()
            } else {
                op
            };
            let password = typed_passphrase(&self.connect_passphrase)?;
            let result = portable.invoke(op, &password).map_err(|_| ());
            clear_secret(&self.connect_passphrase);
            self.error = result
                .as_ref()
                .err()
                .map(|()| t("onboarding-portable-home-unavailable"));
            self.invalidation.both();
            return result;
        }
        if let Some(applications) = self
            .portable_applications
            .as_mut()
            .filter(|pending| pending.pending())
        {
            if operation.is_some_and(|selected| applications.operation() != selected) {
                return Err(());
            }
            let op = if op == "next" {
                applications.next_action()
            } else {
                op
            };
            let password = typed_passphrase(&self.connect_passphrase)?;
            let result = applications.invoke(op, &password).map_err(|_| ());
            clear_secret(&self.connect_passphrase);
            self.error = result
                .as_ref()
                .err()
                .map(|()| t("onboarding-portable-applications-unavailable"));
            self.invalidation.both();
            return result;
        }
        if let Some(browser) = self.browser.as_mut().filter(|pending| pending.pending()) {
            if operation.is_some_and(|selected| browser.operation() != selected) {
                return Err(());
            }
            let op = if op == "next" {
                browser.next_action()
            } else {
                op
            };
            let password = typed_passphrase(&self.connect_passphrase)?;
            let result = browser.invoke(op, &password).map_err(|_| ());
            crate::shell::launcher::set_browser_continuity(
                browser.verified_projection().map(Path::to_owned),
            );
            clear_secret(&self.connect_passphrase);
            self.error = result
                .as_ref()
                .err()
                .map(|()| t("onboarding-browser-unavailable"));
            self.invalidation.both();
            return result;
        }
        let migration = self.migration.as_mut().ok_or(())?;
        let op = if op == "next" {
            migration.next_action()
        } else {
            op
        };
        migration.invoke(op, operation).map_err(|_| ())?;
        self.error = None;
        self.invalidation.both();
        Ok(())
    }
    pub(crate) fn take_original_copy_request(
        &mut self,
    ) -> Option<crate::onboarding_migration::OriginalCopyRequest> {
        self.migration.as_mut()?.take_original_copy_request()
    }
    /// The identity ceremony can be complete while migration remains visible.
    /// This is readiness only; native admission still checks current durable keys.
    pub(crate) fn identity_setup_active(&self) -> bool {
        self.prepared_pending
            || self.recovery_only
            || !self.flow.is_done()
            || self
                .flow
                .minted()
                .is_none_or(|minted| !crate::onboarding::canonical_native_identity_ready(minted))
    }
    pub(crate) fn take_existing_files_request(&mut self) -> Option<crate::retained_files::Request> {
        self.migration.as_mut()?.take_access_request()
    }
    pub(crate) fn take_existing_files_release(&mut self) -> Option<String> {
        self.migration.as_mut()?.take_access_release()
    }
    pub(crate) fn existing_files_outcome(&mut self, operation: &str, available: bool) {
        if let Some(migration) = self.migration.as_mut() {
            migration.access_outcome(operation, available);
            self.invalidation.both();
        }
    }
    pub(crate) fn submit_migration_secret(&self, password: &[u8]) -> Result<(), ()> {
        if !self
            .portable_home
            .as_ref()
            .is_some_and(|pending| pending.pending() && !pending.busy())
            && !self
                .portable_applications
                .as_ref()
                .is_some_and(|pending| pending.pending() && !pending.busy())
            && !self
                .browser
                .as_ref()
                .is_some_and(|pending| pending.pending() && !pending.busy())
        {
            return Err(());
        }
        let text = std::str::from_utf8(password).map_err(|_| ())?;
        if text.is_empty() || text.len() > 4096 {
            return Err(());
        }
        let mut secret = self.connect_passphrase.lock().map_err(|_| ())?;
        secret.zeroize();
        secret.push_str(text);
        drop(secret);
        Ok(())
    }

    /// What the migration page is titled: the step in hand.
    fn migration_title_key(&self) -> &'static str {
        if self
            .portable_home
            .as_ref()
            .is_some_and(crate::onboarding_portable_home::Pending::pending)
        {
            "onboarding-portable-home-title"
        } else if self
            .portable_applications
            .as_ref()
            .is_some_and(crate::onboarding_portable_applications::Pending::pending)
        {
            "onboarding-portable-applications-title"
        } else if self
            .browser
            .as_ref()
            .is_some_and(crate::onboarding_browser::Pending::pending)
        {
            "onboarding-browser-title"
        } else {
            "onboarding-migration-title"
        }
    }

    fn migration_composition(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s4)
            .push(heading(&t(self.migration_title_key()), theme));
        let status = self.migration_status().unwrap_or_default();
        let state = status
            .get("state")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unavailable");
        let retained = status
            .get("retained_access")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        let text = migration_body_key(&status, state, retained);
        body = body.push(paragraph(&t(text), theme));
        if self
            .portable_home
            .as_ref()
            .is_some_and(crate::onboarding_portable_home::Pending::needs_password)
            || self
                .portable_applications
                .as_ref()
                .is_some_and(crate::onboarding_portable_applications::Pending::needs_password)
            || self
                .browser
                .as_ref()
                .is_some_and(crate::onboarding_browser::Pending::needs_password)
        {
            body = body.push(Box::new(secret_input(
                "onboarding.portable-home.password",
                &self.connect_passphrase,
                &t("onboarding-prepared-password"),
                &t("onboarding-prepared-password"),
            )));
        }
        if let Some(files) = status
            .get("result")
            .and_then(|result| result.get("verified_files"))
            .and_then(serde_json::Value::as_u64)
        {
            body = body.push(paragraph(
                &t_args(
                    "onboarding-migration-progress",
                    &[("files", files.to_string().into())],
                ),
                theme,
            ));
        }

        if let Some(error) = &self.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-migration-failed")).with_message(error.clone()),
                theme,
            ));
        }
        body = body.push(self.migration_actions(state));
        Box::new(
            Scrollable::new_vertical()
                .without_sequential_focus()
                .with_child(Box::new(body)),
        )
    }

    /// What the migration's main button says in `state`.
    fn migration_next_label(&self, state: &str, cleanup_pending: bool) -> &'static str {
        match state {
            _ if cleanup_pending => "onboarding-migration-cleanup-resume",
            "pending" | "unavailable" | "failed" | "cancelled" => "onboarding-migration-review",
            "reviewed"
                if self.migration_status().and_then(|value| {
                    value
                        .get("retained_access")
                        .and_then(serde_json::Value::as_bool)
                }) == Some(true) =>
            {
                "onboarding-existing-files-prepare-apps"
            }
            "reviewed"
                if self
                    .migration_status()
                    .is_some_and(|value| json_flag(&value, "portable_applications")) =>
            {
                "onboarding-portable-applications-start"
            }
            "reviewed"
                if self
                    .migration_status()
                    .is_some_and(|value| json_flag(&value, "browser")) =>
            {
                "onboarding-browser-start"
            }
            "reviewed" => "onboarding-migration-start",
            "ready-to-adopt" => "onboarding-migration-adopt",
            "complete" | "existing-files-available" => "onboarding-continue",
            "incomplete" => "onboarding-migration-retry",
            _ => "onboarding-migration-working",
        }
    }

    /// Whether the migration under way in `state` can still be cancelled.
    fn migration_cancellable(&self, state: &str) -> bool {
        state != "cancelling"
            && (state != "reviewing"
                || self
                    .portable_home
                    .as_ref()
                    .is_some_and(crate::onboarding_portable_home::Pending::busy)
                || self
                    .portable_applications
                    .as_ref()
                    .is_some_and(crate::onboarding_portable_applications::Pending::busy)
                || self
                    .browser
                    .as_ref()
                    .is_some_and(crate::onboarding_browser::Pending::busy)
                || self.migration_status().and_then(|value| {
                    value
                        .get("original_copy")
                        .and_then(serde_json::Value::as_bool)
                }) == Some(true)
                || self.migration_status().and_then(|value| {
                    value
                        .get("retained_access")
                        .and_then(serde_json::Value::as_bool)
                }) == Some(true))
    }

    fn migration_actions(&self, state: &str) -> Box<dyn Widget> {
        let cleanup = self.migration_status().unwrap_or_default();
        let cleanup_pending = json_flag(&cleanup, "transfer_complete")
            && json_text(&cleanup, "cleanup_state") == Some("pending");
        let cleanup_blocked = json_flag(&cleanup, "transfer_complete")
            && json_text(&cleanup, "cleanup_state") != Some("finished");
        let busy = !cleanup_pending
            && [
                "copying",
                "verifying",
                "retirement-pending",
                "restoring-applications",
                "adopting",
                "reviewing",
                "cancelling",
                "releasing-applications",
                "activating-browser",
            ]
            .contains(&state);
        let label = self.migration_next_label(state, cleanup_pending);
        let mut actions: Vec<Box<dyn Widget>> = vec![Box::new(
            self.pressed
                .button(
                    "onboarding.migration.next.button",
                    &t(label),
                    "onboarding.migration.next",
                )
                .role(ui::widget::ButtonRole::Primary)
                .with_enabled(!busy && (!cleanup_blocked || cleanup_pending)),
        )];
        if busy {
            actions.push(Box::new(
                self.pressed
                    .button(
                        "onboarding.migration.cancel.button",
                        &t("onboarding-migration-cancel"),
                        "onboarding.migration.cancel",
                    )
                    .with_enabled(self.migration_cancellable(state)),
            ));
        } else if !cleanup_blocked {
            actions.push(Box::new(
                self.pressed
                    .button(
                        "onboarding.migration.later.button",
                        &t("onboarding-migration-later"),
                        "onboarding.migration.later",
                    )
                    .with_enabled(state != "unavailable"),
            ));
            if state == "incomplete" {
                actions.push(Box::new(self.pressed.button(
                    "onboarding.migration.continue.button",
                    &t("onboarding-continue"),
                    "onboarding.migration.continue",
                )));
            }
        }
        footer_actions(actions)
    }

    fn persist_progress(&mut self, stage: PersistedStage) -> bool {
        let Some(identity_dir) = self.identity_dir.as_ref() else {
            return true;
        };
        match store_progress(identity_dir, stage) {
            Ok(()) => {
                self.progress_error = None;
                true
            }
            Err(error) => {
                self.progress_error = Some(t_args(
                    "onboarding-progress-save-failed",
                    &[("error", error.to_string().into())],
                ));
                self.invalidation.structure();
                false
            }
        }
    }

    fn restore(&self, theme: &Theme) -> Box<dyn Widget> {
        let compact = self.rect.height() <= 540.0;
        let spacing = &theme.tokens.spacing;
        let path = TextInput::from_state(&self.connect_path)
            .with_id("onboarding.connect.path.field")
            .with_accessible_name(t("onboarding-connect-file"))
            .with_placeholder(t("onboarding-connect-file-placeholder"));
        let passphrase = secret_input(
            "onboarding.connect.passphrase.field",
            &self.connect_passphrase,
            &t("onboarding-connect-passphrase"),
            &t("onboarding-connect-passphrase-placeholder"),
        );
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(if compact { spacing.s3 } else { spacing.s4 })
            .push(heading(&t("onboarding-restore-title"), theme))
            .push(paragraph(&t("onboarding-restore-body"), theme));
        if let Some(error) = &self.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-connect-failed")).with_message(error.clone()),
                theme,
            ));
        }
        let file = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s1)
            .push(Box::new(
                Text::new(&t("onboarding-restore-file-label"))
                    .with_role(ui::tokens::typography::TextRole::Label),
            ))
            .push(Box::new(
                Row::new().no_wrap().push(Box::new(path)).push(Box::new(
                    self.pressed
                        .button(
                            "onboarding.connect.browse.button",
                            &t("onboarding-connect-browse"),
                            KEY_CONNECT_BROWSE,
                        )
                        .with_icon(ui::icons::Icon::FolderOpen),
                )),
            ));
        let secret = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s1)
            .push(Box::new(
                Text::new(&t("onboarding-connect-passphrase"))
                    .with_role(ui::tokens::typography::TextRole::Label),
            ))
            .push(Box::new(passphrase));
        Box::new(
            body.push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(if compact { spacing.s3 } else { spacing.s4 })
                    .push(Box::new(file))
                    .push(Box::new(secret)),
            )),
        )
    }

    fn no_recovery(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let icons = [ui::icons::Icon::Ban, ui::icons::Icon::Archive];
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s4)
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(spacing.s3)
                    .push(heading(&t("onboarding-protect-title"), theme))
                    .push(spaced_banner(
                        Banner::warning(no_recovery_title())
                            .with_message(t("onboarding-protect-message"))
                            .with_icon(ui::icons::Icon::Shield),
                        theme,
                    )),
            ));
        if let Some(error) = &self.error {
            body = body.push(spaced_banner(
                Banner::error(t("onboarding-create-failed")).with_message(error.clone()),
                theme,
            ));
        }
        Box::new(body.push(fact_list(
            icons.into_iter().zip(no_recovery_points()),
            theme,
        )))
    }

    fn backup(&self, theme: &Theme) -> Box<dyn Widget> {
        if let Some(err) = &self.error {
            return Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(theme.tokens.spacing.s6)
                    .push(spaced_banner(
                        Banner::error(t("onboarding-create-failed-title"))
                            .with_message(err.clone()),
                        theme,
                    ))
                    .push(paragraph(&t("onboarding-create-failed-message"), theme))
                    .push(actions(vec![back_button(&self.pressed)])),
            );
        }
        if !self.backup_export_available {
            return self.done(theme, false);
        }
        let compact = self.rect.height() <= 540.0;
        let mut backup = Column::new().with_no_padding().with_intrinsic_height();
        if let Some(error) = &self.backup_error {
            backup = backup.push(spaced_banner(
                Banner::error(t("onboarding-backup-failed")).with_message(error.clone()),
                theme,
            ));
        }
        if let Some(path) = &self.backup_saved {
            backup = backup.push(spaced_banner(
                Banner::success(t("onboarding-backup-verified"))
                    .with_message(path.display().to_string())
                    .with_icon(ui::icons::Icon::Check),
                theme,
            ));
        } else if self.backup_was_verified {
            backup = backup.push(spaced_banner(
                Banner::success(t("onboarding-backup-verified"))
                    .with_message(t("onboarding-backup-verified-earlier"))
                    .with_icon(ui::icons::Icon::Check),
                theme,
            ));
        }
        let mut page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(if compact {
                theme.tokens.spacing.s3
            } else {
                theme.tokens.spacing.s4
            })
            .push(Box::new(
                Text::new(&t("onboarding-backup-title"))
                    .with_id("onboarding.backup.heading")
                    .with_role(ui::tokens::typography::TextRole::LargeTitle),
            ));
        if self.backup_error.is_some() || self.backup_saved.is_some() || self.backup_was_verified {
            page = page.push(Box::new(backup));
        }
        if !self.backup_was_verified || self.backup_copy_form_open {
            page = page.push(self.backup_form(theme));
        }
        Box::new(page)
    }

    fn done(&self, theme: &Theme, compact: bool) -> Box<dyn Widget> {
        if !self.joined_member {
            return Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(if compact {
                        theme.tokens.spacing.s3
                    } else {
                        theme.tokens.spacing.s4
                    })
                    .push(Box::new(
                        Text::new(&t("onboarding-ready-welcome"))
                            .with_id("onboarding.done.heading")
                            .with_role(ui::tokens::typography::TextRole::LargeTitle),
                    ))
                    .push(paragraph_with_id(
                        &t("onboarding-ready-next"),
                        theme,
                        "onboarding.done.body",
                    )),
            );
        }
        let (title, body, ready) = match self.context {
            OnboardingContext::FullSystem => {
                (done_title(), done_body(), t("onboarding-done-ready"))
            }
            OnboardingContext::HostDesktop => (
                t("onboarding-host-done-title"),
                t("onboarding-host-done-body"),
                t("onboarding-host-done-ready"),
            ),
        };
        let mut page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            // Keep every hand-off block, with a full token between them. The
            // ordinary-text surface uses `s3` so its complete hand-off
            // decision fits a safe 800×600 panel without scrolling; large
            // text and high contrast retain the roomier scrollable rhythm.
            .with_gap(if compact {
                theme.tokens.spacing.s3
            } else {
                theme.tokens.spacing.s4
            })
            .push(heading(&title, theme))
            .push(spaced_banner(
                Banner::success(ready).with_icon(ui::icons::Icon::Check),
                theme,
            ))
            .push(paragraph(&body, theme))
            .push(Box::new(Section::titled(
                t("onboarding-done-next-title"),
                vec![paragraph(&t("onboarding-done-next-body"), theme)],
            )))
            .push(Box::new(Section::titled(
                t(done_backup_title_key(
                    self.joined_member,
                    self.backup_was_verified,
                )),
                vec![paragraph_with_id(
                    &t(done_backup_body_key(
                        self.joined_member,
                        self.backup_was_verified,
                    )),
                    theme,
                    "onboarding.done.backup-guidance",
                )],
            )));

        if self.context == OnboardingContext::FullSystem {
            page = page.push(Box::new(Section::titled(
                t("onboarding-done-system-care-title"),
                vec![paragraph(&t("onboarding-done-system-care-body"), theme)],
            )));
        }
        Box::new(page)
    }

    /// The offer to play more formats: a centred title and one short line
    /// over a single large Install button, with a quiet way past it. Nothing
    /// is chosen until the person presses Install; once they did, the live
    /// result takes the button's place.
    fn codecs(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let centred = |widget: Box<dyn Widget>| -> Box<dyn Widget> {
            Box::new(
                Row::new()
                    .with_padding(0.0, 0.0)
                    .no_wrap()
                    .push(Box::new(Spacer::new()))
                    .push(widget)
                    .push(Box::new(Spacer::new())),
            )
        };
        let outcome = self.codec_outcome();
        // The bare offer sits in the middle of the page; a result needs the
        // room for its banner.
        let top = if outcome.is_some() {
            spacing.s2
        } else if theme.text_scale > 1.0 {
            spacing.s6
        } else {
            spacing.s8 * 2.0
        };
        let mut page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_extra_top_padding(top)
            .with_gap(spacing.s4)
            .push(centred(Box::new(
                ui::widget::IconWidget::new(ui::icons::Icon::Film)
                    .sized(ui::widget::IconSize::Large)
                    .accent(),
            )))
            .push(Box::new(
                Text::new(&codecs_title())
                    .with_role(ui::tokens::typography::TextRole::Display)
                    .with_align(ui::ParagraphAlign::Center),
            ))
            .push(Box::new(
                Text::new(&codecs_note())
                    .with_id("onboarding.codecs.note")
                    .with_role(ui::tokens::typography::TextRole::Body)
                    .with_color(theme.text_dim)
                    .with_align(ui::ParagraphAlign::Center),
            ));
        if let Some((title, message)) = outcome {
            let failed = self.codec_error.is_some() || self.codec_status.state == PackState::Failed;
            let banner = match self.codec_status.state {
                _ if failed => Banner::error(title),
                PackState::Installed => Banner::success(title).with_icon(ui::icons::Icon::Check),
                PackState::WaitingForNetwork => Banner::warning(title),
                _ => Banner::info(title),
            };
            let banner = match message {
                Some(message) => banner.with_message(message),
                None => banner,
            };
            page = page.push(spaced_banner(banner, theme));
        }
        if let Some(fraction) = self.codec_progress() {
            page = page.push(Box::new(
                ProgressBar::new(fraction).with_label(t("onboarding-codecs-installing-title")),
            ));
        }
        if let Some([(id, label, key), (quiet_id, quiet_label, quiet_key)]) = self.codec_choice() {
            // The one way in is this button. It holds keyboard focus so
            // Enter is the same deliberate press; it starts the download and
            // never moves past the page by itself.
            let install = self
                .pressed
                .button(id, &label, key)
                .control_size(ui::widget::ControlSize::Large)
                .with_icon(ui::icons::Icon::Download)
                .role(ButtonRole::Primary)
                .with_default_focus();
            let skip = self
                .pressed
                .button(quiet_id, &quiet_label, quiet_key)
                .role(ButtonRole::Plain);
            if self.codec_outcome().is_none() {
                page = page.push(Box::new(Spacer::fixed(spacing.s2)));
            }
            page = page
                .push(centred(Box::new(install)))
                .push(centred(Box::new(skip)));
        }
        Box::new(page)
    }

    fn codec_actions(&self) -> Box<dyn Widget> {
        if self.codec_choice().is_some() {
            return footer_actions(Vec::new());
        }
        footer_actions(vec![Box::new(
            self.pressed
                .button(
                    "onboarding.codecs.continue.button",
                    &t("onboarding-continue"),
                    KEY_CODECS_CONTINUE,
                )
                .control_size(ui::widget::ControlSize::Large)
                .with_icon(ui::icons::Icon::ArrowRight)
                .role(ButtonRole::Primary)
                .with_default_focus(),
        )])
    }

    /// Sticky final action, deliberately outside the Done screen's scrollable
    /// backup content so it remains visible at narrow and large-text sizes.
    fn finish_action(&self) -> Box<dyn Widget> {
        // Continuing to the desktop is the primary hand-off. The optional
        // tour remains explicit rather than making “Skip” the first desktop
        // action a new user has to decipher.
        let tour = self.pressed.button(
            "onboarding.tour.start.button",
            &t("onboarding-tour-take"),
            KEY_TOUR_START,
        );
        let enter = self
            .pressed
            .button(
                "onboarding.finish.button",
                &t("onboarding-finish"),
                KEY_FINISH,
            )
            .with_icon(ui::icons::Icon::ArrowRight)
            .role(ui::widget::ButtonRole::Primary);
        footer_actions(vec![Box::new(tour), Box::new(enter)])
    }
}

/// The ready screen's backup heading. A device that joined from another one
/// holds no root key, so nothing on it needs a backup.
fn done_backup_title_key(joined_member: bool, backup_verified: bool) -> &'static str {
    if joined_member {
        "onboarding-done-joined-key-elsewhere"
    } else if backup_verified {
        "onboarding-done-backup-verified"
    } else {
        "onboarding-done-backup-recommended"
    }
}

/// The ready screen's backup guidance under [`done_backup_title_key`].
fn done_backup_body_key(joined_member: bool, backup_verified: bool) -> &'static str {
    if joined_member {
        "onboarding-done-joined-key-elsewhere-body"
    } else if backup_verified {
        "onboarding-done-backup-verified-body"
    } else {
        "onboarding-done-backup-recommended-body"
    }
}

/// The title at the top of an onboarding page.
fn heading(text: &str, _theme: &Theme) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_role(ui::tokens::typography::TextRole::LargeTitle))
}

/// Secondary body copy. Takes the live theme rather than a hard-coded
/// one — a dark-theme dim grey on a light background is barely legible.
fn paragraph_with_id(text: &str, theme: &Theme, id: &str) -> Box<dyn Widget> {
    Box::new(
        Text::new(text)
            .with_id(id)
            .with_role(ui::tokens::typography::TextRole::Body)
            .with_color(theme.text_dim),
    )
}

fn paragraph(text: &str, theme: &Theme) -> Box<dyn Widget> {
    paragraph_with_id(text, theme, "onboarding.paragraph")
}

/// Give status surfaces deliberate breathing room independent of the parent
/// composition's gap calculation. This also keeps consecutive warning/error
/// banners from visually merging into one block.
fn spaced_banner(banner: Banner, theme: &Theme) -> Box<dyn Widget> {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_vertical_padding(theme.tokens.spacing.s2)
            .push(Box::new(banner)),
    )
}

/// Short statements, each led by an icon that names its subject. The icon
/// sits in its own column, top-aligned with the first line, so wrapped lines
/// keep a clean left edge.
fn fact_list(
    facts: impl IntoIterator<Item = (ui::icons::Icon, String)>,
    theme: &Theme,
) -> Box<dyn Widget> {
    let mut list = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(theme.tokens.spacing.s3);
    for (icon, text) in facts {
        list = list.push(Box::new(
            Row::new()
                .no_wrap()
                .align_start()
                .with_gap(theme.tokens.spacing.s3)
                .push(Box::new(IconWidget::new(icon).dim()))
                .push(Box::new(
                    Text::new(&text).with_role(ui::tokens::typography::TextRole::Body),
                )),
        ));
    }
    Box::new(list)
}

/// One step of a real sequence: its number in quiet ink, then the
/// instruction.
fn numbered_step(number: usize, text: &str, theme: &Theme) -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .no_wrap()
            .align_start()
            .with_gap(theme.tokens.spacing.s3)
            .push(Box::new(
                Text::new(&number.to_string())
                    .with_role(ui::tokens::typography::TextRole::Body)
                    .strong()
                    .numeric()
                    .with_color(theme.text_dim),
            ))
            .push(Box::new(
                Text::new(text).with_role(ui::tokens::typography::TextRole::Body),
            )),
    )
}

/// A six-digit verification code in two groups of three, the way people
/// read a code aloud and compare it between two screens.
fn grouped_code(code: &str) -> String {
    let digits: Vec<char> = code.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.len() == 6 {
        let (head, tail) = digits.split_at(3);
        format!(
            "{} {}",
            head.iter().collect::<String>(),
            tail.iter().collect::<String>()
        )
    } else {
        digits.into_iter().collect()
    }
}

fn back_button(pressed: &PressedActions) -> Box<dyn Widget> {
    Box::new(
        pressed
            .button("onboarding.back.button", &t("onboarding-back"), KEY_BACK)
            .with_icon(ui::icons::Icon::ArrowLeft),
    )
}

/// A right-aligned action row, so the primary button lands where the eye finishes reading.
fn actions(buttons: Vec<Box<dyn Widget>>) -> Box<dyn Widget> {
    let mut row = Row::new().push(Box::new(Spacer::new()));
    for b in buttons {
        row = row.push(b);
    }
    Box::new(row.no_wrap())
}

/// Sticky page navigation with a deliberate 20 px side and 16 px bottom
/// inset, independent of the scrollable page body.
fn footer_actions(buttons: Vec<Box<dyn Widget>>) -> Box<dyn Widget> {
    let mut row = Row::new().push(Box::new(Spacer::new()));
    for button in buttons {
        row = row.push(button);
    }
    // Taffy rounds logical positions at the output edge; 17 keeps the
    // promised visible 16 px bottom inset after that rounding.
    Box::new(row.no_wrap().with_padding(20.0, 17.0))
}

fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    matches!(
        state.get(root, key),
        Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), ScalarValue::Boolean(true))
    )
}

fn secret_input(
    id: &str,
    buffer: &Arc<Mutex<String>>,
    accessible_name: &str,
    placeholder: &str,
) -> TextInput {
    let input = TextInput::local()
        .with_id(id)
        .with_external_buffer(Arc::clone(buffer))
        .with_accessible_name(accessible_name)
        .with_placeholder(placeholder)
        .with_secret(true);
    match buffer.lock() {
        Ok(value) => input.with_initial_value(&value),
        Err(_) => input,
    }
}

fn labelled_secret_field(
    id: &str,
    label: &str,
    buffer: &Arc<Mutex<String>>,
    accessible_name: &str,
    placeholder: &str,
) -> Box<dyn Widget> {
    let label_id = format!("{id}.label");
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(4.0)
            .push(Box::new(
                Text::new(label)
                    .with_id(label_id)
                    .with_role(ui::tokens::typography::TextRole::Label),
            ))
            .push(Box::new(secret_input(
                id,
                buffer,
                accessible_name,
                placeholder,
            ))),
    )
}

/// The passphrase typed into `buffer`, copied out to be cleared after its
/// one use.
fn typed_passphrase(buffer: &Arc<Mutex<String>>) -> Result<Zeroizing<Vec<u8>>, ()> {
    Ok(Zeroizing::new(
        buffer.lock().map_err(|_| ())?.as_bytes().to_vec(),
    ))
}

fn clear_secret(buffer: &Arc<Mutex<String>>) {
    if let Ok(mut value) = buffer.lock() {
        value.zeroize();
    }
}

impl OnboardingView {
    fn draw_backdrop(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        if self.tour_page.is_some() {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.neutral[9].with_alpha_factor(
                    if theme.name.eq_ignore_ascii_case("high contrast") {
                        0.72
                    } else {
                        0.48
                    },
                ),
                None,
                &self.rect,
            );
            // Keep the real desktop legible while the tour is modal. Do not
            // paint a focus target from the covered desktop behind the card:
            // a full-width ring reads as a second, interactive surface and
            // competes with the tour's own keyboard focus.
            return;
        }
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        if theme.name.eq_ignore_ascii_case("high contrast") {
            return;
        }

        // Quiet, theme-derived depth: two broad accent blooms make the
        // trusted setup surface distinct without becoming illustration.
        let bloom = colors.text_primary.with_alpha_factor(0.10);
        let radius = self.rect.width().max(self.rect.height()) * 0.34;
        scene.fill(
            Fill::NonZero,
            transform,
            bloom,
            None,
            &Circle::new(Point::new(self.rect.x0, self.rect.y0), radius),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            colors.intent.you.with_alpha_factor(0.07),
            None,
            &Circle::new(Point::new(self.rect.x1, self.rect.y1), radius * 0.82),
        );
    }

    /// The arriving step's pose: it comes in from the side it lives on.
    fn transition_pose(&mut self, theme: &Theme) -> ui::motion::Pose {
        self.reduce_motion = theme.reduce_motion;
        // Under reduced motion a step is simply there: no drill, no fade.
        let arrived = if self.reduce_motion {
            1.0
        } else {
            self.transition.progress(ui::motion::frame_now())
        };
        ui::motion::Drill::pose(
            self.transition_direction * (1.0 - arrived),
            self.rect.width(),
            theme.motion(),
        )
    }

    fn draw_composed(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.composed.is_none() {
            return;
        }
        let pose = self.transition_pose(theme);
        let Some(composed) = self.composed.as_mut() else {
            return;
        };
        let panel = composed.get_rect();
        if panel.area() > 0.0 {
            // Panel role (HIG §9.1–§9.3): the onboarding card floats over the
            // backdrop, so it casts the same Panel shadow every other Panel
            // casts, from the elevation token; on the high-contrast palette
            // that black shadow lands on a black ground and contributes
            // nothing — the edge does the separating (§9.3) — so the former
            // by-name high-contrast guard is not needed. Ground is
            // `panel_ground`, edge the derived Panel border.
            ui::chrome::shell::paint_panel(scene, transform, panel, theme, 1.0);
        }

        let mut local = Scene::new();
        composed.draw(&mut local, Affine::IDENTITY, text_renderer, theme, presence);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            pose.opacity,
            transform,
            &self.rect,
        );
        scene.append(&local, Some(pose.applied_to(transform)));
        scene.pop_layer();
    }

    fn draw_picker(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        let Some((_, picker)) = self.picker.as_mut() else {
            return;
        };
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.scrim.with_alpha_factor(0.56),
            None,
            &self.rect,
        );
        picker.set_rect(onboarding_picker_rect(self.rect));
        picker.draw(scene, transform, text_renderer, theme, presence);
    }
}

impl Drop for OnboardingView {
    fn drop(&mut self) {
        let mut snapshot = self.semantic.borrow_mut();
        snapshot.phase = if self.finished { "finished" } else { "closed" };
        snapshot.failed = !self.finished;
        snapshot.operations.clear();
        snapshot.revision = snapshot.revision.wrapping_add(1);
        drop(snapshot);
        self.nearby = None;
        clear_secret(&self.connect_passphrase);
        clear_secret(&self.backup_passphrase);
        clear_secret(&self.backup_confirmation);
    }
}

impl Widget for OnboardingView {
    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
        for (kind, checked) in self.acknowledgements.take() {
            let action = AcknowledgementAction {
                kind,
                path: path.clone(),
                checked,
            };
            if let Err(error) = edits.apply(
                &mut self.acknowledgement_actions,
                "tilo.shell.onboarding.acknowledgement",
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
        self.synced_at = Instant::now();
        self.refresh_codec_state();

        // Pull widget-local edits (notably passphrases) into their process-local
        // buffers before interpreting action keys from the same input frame.
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
        }
        semantic::drain(self);
        if self.prepared_pending {
            self.pressed
                .0
                .borrow_mut()
                .retain(|key| *key == KEY_PREPARED_IMPORT);
            self.sync_prepared();
            self.publish_semantic_state();
            return;
        }
        if self.migration_active() {
            let home_changed = self
                .portable_home
                .as_mut()
                .is_some_and(crate::onboarding_portable_home::Pending::continue_automatic);
            let home_ready = !self
                .portable_home
                .as_ref()
                .is_some_and(|home| home.pending() || home.deferred());
            let apps_changed = home_ready
                && self.portable_applications.as_mut().is_some_and(
                    crate::onboarding_portable_applications::Pending::continue_automatic,
                );
            let browser_changed = self
                .browser
                .as_mut()
                .is_some_and(crate::onboarding_browser::Pending::continue_automatic);
            if home_changed || apps_changed || browser_changed {
                self.invalidation.both();
            }
            for (key, op) in [
                ("onboarding.migration.next", "next"),
                ("onboarding.migration.later", "later"),
                ("onboarding.migration.cancel", "cancel"),
                ("onboarding.migration.continue", "continue"),
            ] {
                if self.pressed.take(key) && self.invoke_migration(op, None).is_err() {
                    self.error = Some(t("onboarding-migration-failed"));
                }
            }
            self.pressed.0.borrow_mut().clear();
            if self
                .portable_home
                .as_mut()
                .is_some_and(crate::onboarding_portable_home::Pending::poll)
                || self
                    .portable_applications
                    .as_mut()
                    .is_some_and(crate::onboarding_portable_applications::Pending::poll)
                || self
                    .browser
                    .as_mut()
                    .is_some_and(crate::onboarding_browser::Pending::poll)
                || self
                    .migration
                    .as_mut()
                    .is_some_and(crate::onboarding_migration::Migration::poll)
            {
                self.invalidation.both();
            }
            if self.browser.is_some() {
                crate::shell::launcher::set_browser_continuity(
                    self.verified_browser_projection().map(Path::to_owned),
                );
            }
            self.publish_semantic_state();
            return;
        }
        let connect_source_selected = !self.connect_path.with(|path| path.trim().is_empty());
        if connect_source_selected != self.connect_source_selected {
            self.connect_source_selected = connect_source_selected;
            self.invalidation.structure();
        }

        // Steps whose journal changes have reached the disk since the last
        // frame, and identity work that has reported, before this frame's
        // actions.
        self.settle_journal();
        self.settle_identity_work();

        let picker_outcome = self
            .picker
            .as_mut()
            .and_then(|(purpose, picker)| picker.take_outcome().map(|outcome| (*purpose, outcome)));
        if let Some((purpose, outcome)) = picker_outcome {
            if let ui::widget::FilePickerOutcome::Selected(paths) = outcome
                && let Some(path) = paths.into_iter().next()
            {
                match purpose {
                    PickerPurpose::ImportIdentity => {
                        self.connect_path.set(path.to_string_lossy().into_owned());
                        self.connect_source_selected = true;
                        self.error = None;
                    }
                    PickerPurpose::BackupIdentity => self.export_identity(path),
                }
            }
            self.picker = None;
            self.invalidation.both();
        }

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

        // Choosing the nearby flow is the explicit request to pair, so the
        // screen opens with pairing already under way.
        let mut start_nearby = self.pressed.take(KEY_CHOOSE_NEARBY)
            && self.flow.screen() == Screen::Existing
            && self.identity_idle();
        if start_nearby {
            self.error = None;
            self.nearby_failure = None;
            self.nearby_status = None;
            self.nearby_code = None;
            self.flow.choose_nearby();
        }

        if self.pressed.take(KEY_CONNECT_BROWSE)
            && self.flow.screen() == Screen::Restore
            && self.picker.is_none()
        {
            let (initial, roots) = onboarding_picker_scope();
            self.picker = Some((
                PickerPurpose::ImportIdentity,
                FilePicker::new(t("onboarding-picker-import-title"), initial, roots).with_filters(
                    vec![
                        ui::widget::file_picker::FileFilter::new(
                            t("onboarding-picker-import-filter"),
                            &["tilo-identity", "tilo-identity-backup", "json"],
                        ),
                        ui::widget::file_picker::FileFilter::all(),
                    ],
                ),
            ));
            self.invalidation.focus();
        }

        if self.pressed.take(KEY_CONNECT_NEARBY) {
            start_nearby = true;
        }
        if start_nearby && !self.journal_idle() {
            // Pairing starts once the outstanding journal change is written.
            self.pressed.press(KEY_CONNECT_NEARBY);
        } else if start_nearby && self.flow.screen() == Screen::Nearby && self.nearby.is_none() {
            self.journal_then(
                JournalChange::Store(PersistedStage::AcquiringIdentity),
                AfterJournal::StartNearby,
            );
        }

        if self.pressed.take(KEY_CONNECT_CANCEL) {
            self.nearby = None;
            if let Some(path) = &self.nearby_status_path {
                let _ = std::fs::remove_file(path);
            }
            self.nearby_status = Some(t("onboarding-nearby-cancelled"));
            self.nearby_code = None;
            self.invalidation.structure();
        }

        let nearby = self
            .nearby
            .as_mut()
            .map(|pairing| pairing.poll(Instant::now()));
        match nearby {
            None | Some(crate::onboarding_nearby::NearbyEvent::Idle) => {}
            Some(crate::onboarding_nearby::NearbyEvent::Code(code)) => {
                self.nearby_code = Some(code);
                self.nearby_status = Some(t("onboarding-nearby-code"));
                self.invalidation.structure();
            }
            Some(crate::onboarding_nearby::NearbyEvent::Joined(identity)) => {
                self.nearby = None;
                self.nearby_code = None;
                self.error = self
                    .accept_connected_identity(identity)
                    .err()
                    .map(|()| t("onboarding-connect-recovery-message"));
                self.nearby_status = Some(t("onboarding-nearby-connected"));
                self.invalidation.structure();
            }
            Some(crate::onboarding_nearby::NearbyEvent::Failed(failure)) => {
                self.nearby = None;
                self.nearby_code = None;
                self.nearby_status = None;
                self.error = Some(failure.message());
                self.nearby_failure = Some(failure);
                self.invalidation.structure();
            }
        }

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
            && self.picker.is_none()
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
                let (initial, roots) = onboarding_picker_scope();
                self.picker = Some((
                    PickerPurpose::BackupIdentity,
                    FilePicker::new(t("onboarding-picker-backup-title"), initial, roots)
                        .with_mode(FilePickerMode::Save)
                        .with_filters(vec![ui::widget::file_picker::FileFilter::new(
                            t("onboarding-picker-backup-filter"),
                            &["tilo-identity"],
                        )])
                        .with_filename("tilo-identity-backup.tilo-identity"),
                ));
                self.invalidation.focus();
            } else {
                self.invalidation.structure();
            }
        }

        if self.take_journaled(KEY_BACKUP_CONTINUE) {
            let skip_acknowledged = self
                .backup_continue_ack
                .take()
                .unwrap_or(self.backup_skip_acknowledged);
            if self.portable_backup_only && self.flow.is_done() && self.backup_was_verified {
                self.journal_then(JournalChange::Clear, AfterJournal::FinishPortableBackup);
            } else if !self.portable_backup_only
                && self.flow.is_done()
                && self.tour_page.is_none()
                && !self.identity_ready_page
                && (self.backup_was_verified || skip_acknowledged)
            {
                self.journal_then(
                    JournalChange::Store(if self.backup_was_verified {
                        PersistedStage::ReadyToEnterBackedUp
                    } else {
                        PersistedStage::ReadyToEnter
                    }),
                    AfterJournal::EnterReady,
                );
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
            self.nearby = None;
            self.nearby_failure = None;
            self.nearby_status = None;
            self.nearby_code = None;
            if !(self.recovery_only && self.flow.screen() == Screen::Existing) {
                self.flow.back();
            }
        }

        if self.take_journaled(KEY_IMPORT) {
            let source = PathBuf::from(self.connect_path.get());
            if self.flow.screen() == Screen::Restore && source.as_os_str().is_empty() {
                self.error = Some(t("onboarding-connect-choose-file-first"));
                self.invalidation.structure();
            } else if self.flow.screen() == Screen::Restore {
                self.journal_then(
                    JournalChange::Store(PersistedStage::AcquiringIdentity),
                    AfterJournal::Import(source),
                );
            }
        }

        // A locked keyring may become available after the compositor has
        // presented trusted chrome (for example after the host keyring's
        // unlock prompt completes). Retry the existing record in place; the
        // minter is idempotent and cannot overwrite metadata.
        if self.take_journaled(KEY_CONNECT_RETRY)
            && self.recovery_only
            && self.flow.screen() == Screen::Existing
        {
            self.journal_then(
                JournalChange::Store(PersistedStage::AcquiringIdentity),
                AfterJournal::RetryMint,
            );
        }

        // The checkbox drives the gate directly rather than being latched, so unticking really does close it again.
        // The acknowledgement a running mint was started with holds until
        // the mint reports.
        let understood = read_bool(state, root, KEY_UNDERSTOOD);
        if understood != self.flow.acknowledged() && self.identity_idle() {
            self.flow.set_acknowledged(understood);
            self.invalidation.structure();
        }

        let skip_acknowledged = read_bool(state, root, KEY_BACKUP_SKIP_ACKNOWLEDGED);
        if skip_acknowledged != self.backup_skip_acknowledged {
            self.backup_skip_acknowledged = skip_acknowledged;
            self.invalidation.structure();
        }

        if self.take_journaled(KEY_CREATE) && self.flow.can_create() {
            self.journal_then(
                JournalChange::Store(PersistedStage::AcquiringIdentity),
                AfterJournal::Mint,
            );
        }

        if self.take_journaled(KEY_TOUR_START)
            && self.flow.is_done()
            && self.identity_ready_page
            && !self.codec_page
        {
            self.journal_then(
                JournalChange::Store(PersistedStage::TourNavigate),
                AfterJournal::Tour(Some(TourPage::Navigate)),
            );
        }

        if self.take_journaled(KEY_TOUR_BACK) {
            let next = match self.tour_page {
                Some(TourPage::Navigate) if self.tour_only => {
                    self.finished = true;
                    self.tour_page
                }
                Some(TourPage::Navigate) | None => None,
                Some(TourPage::Organise) => Some(TourPage::Navigate),
                Some(TourPage::Collaborate) => Some(TourPage::Organise),
                Some(TourPage::Start) => Some(TourPage::Collaborate),
            };
            let progress_stage = match next {
                None => Some(if self.backup_was_verified {
                    PersistedStage::ReadyToEnterBackedUp
                } else {
                    PersistedStage::ReadyToEnter
                }),
                Some(TourPage::Navigate) => Some(PersistedStage::TourNavigate),
                Some(TourPage::Organise) => Some(PersistedStage::TourOrganise),
                Some(TourPage::Collaborate) => Some(PersistedStage::TourCollaborate),
                Some(TourPage::Start) => Some(PersistedStage::TourStart),
            };
            match progress_stage {
                Some(progress) if !self.tour_only => {
                    self.journal_then(JournalChange::Store(progress), AfterJournal::Tour(next));
                }
                _ => self.tour_page = next,
            }
        }

        if self.take_journaled(KEY_TOUR_NEXT) {
            let next = match self.tour_page {
                Some(TourPage::Navigate) => Some(TourPage::Organise),
                Some(TourPage::Organise) => Some(TourPage::Collaborate),
                Some(TourPage::Collaborate) => Some(TourPage::Start),
                page => page,
            };
            let progress_stage = match next {
                Some(TourPage::Navigate) => Some(PersistedStage::TourNavigate),
                Some(TourPage::Organise) => Some(PersistedStage::TourOrganise),
                Some(TourPage::Collaborate) => Some(PersistedStage::TourCollaborate),
                Some(TourPage::Start) => Some(PersistedStage::TourStart),
                None => None,
            };
            match progress_stage {
                Some(progress) if !self.tour_only => {
                    self.journal_then(JournalChange::Store(progress), AfterJournal::Tour(next));
                }
                _ => self.tour_page = next,
            }
        }

        if self.take_journaled(KEY_TOUR_SKIP) {
            // This action exists only while a tour page is visible. During
            // first use, clearing the crash journal is the durable record that
            // the person deliberately skipped; a reopened tour has no setup
            // journal and can close immediately.
            if self.tour_page.is_some() && self.tour_only {
                self.finish_ceremony();
            } else if self.tour_page.is_some() && self.flow.is_done() {
                self.journal_then(JournalChange::Clear, AfterJournal::Finish);
            } else {
                self.finished = false;
            }
        }

        if self.take_journaled(KEY_FINISH) {
            let at_tour_end = self.tour_page == Some(TourPage::Start);
            let skipping_tour = self.tour_page.is_none()
                && self.flow.is_done()
                && self.identity_ready_page
                && !self.codec_page;
            // A reopened, tour-only flow exposes “Skip tour” on every page.
            // Its action is KEY_FINISH, so it must dismiss from every page,
            // not only from the final hand-off page.
            if self.portable_backup_only || self.tour_only {
                self.finish_ceremony();
            } else if (at_tour_end || skipping_tour) && self.flow.is_done() {
                self.journal_then(JournalChange::Clear, AfterJournal::Finish);
            } else {
                self.finished = false;
            }
        }

        if self.flow.screen() != before
            || self.tour_page != tour_before
            || self.identity_ready_page != ready_before
            || self.codec_page != codec_page_before
        {
            self.transition_direction = if self.identity_ready_page != ready_before
                || self.codec_page != codec_page_before
            {
                1.0
            } else if view_order(self.flow.screen(), self.tour_page)
                < view_order(before, tour_before)
            {
                -1.0
            } else {
                1.0
            };
            self.invalidation.both();
            self.transition = step_arriving(self.motion);
        }
        self.publish_semantic_state();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.reduce_motion = theme.reduce_motion;
        self.motion = theme.motion();
        let accessibility_expanded =
            theme.text_scale > 1.0 || theme.name.eq_ignore_ascii_case("high contrast");
        let panel_height = if accessibility_expanded {
            ACCESSIBLE_PANEL_HEIGHT
        } else if self.tour_page.is_none()
            && matches!(self.flow.screen(), Screen::Intro | Screen::Existing)
        {
            INTRO_PANEL_HEIGHT
        } else {
            PANEL_MAX_HEIGHT
        };
        let ready_identity_page = self.tour_page.is_none()
            && self.flow.screen() == Screen::Done
            && self.identity_ready_page
            && !self.codec_page;
        // The introduction's pinned cards take a fixed share of the panel, so
        // it keeps one stable height instead of measuring its explanation.
        let intro_page = self.tour_page.is_none() && self.flow.screen() == Screen::Intro;
        let codecs_page = ready_identity_page && self.codec_page;
        let dense_identity_page = self.tour_page.is_none()
            && (accessibility_expanded
                || matches!(
                    self.flow.screen(),
                    Screen::Nearby | Screen::Restore | Screen::NoRecovery
                )
                || (self.flow.screen() == Screen::Done
                    && (!self.identity_ready_page || self.codec_page)));
        // The scroll/footer budget must use the same preferred height as the
        // composition itself. Previously dense normal-text pages measured a
        // 640 px scroll region and were then placed in a 560 px card, putting
        // the sticky action on top of its scrolling sibling.
        let composition_height = if ready_identity_page {
            panel_height
        } else if accessibility_expanded {
            ACCESSIBLE_PANEL_HEIGHT
        } else if dense_identity_page {
            DENSE_PANEL_PREFERRED_HEIGHT
        } else if codecs_page {
            CODECS_PANEL_PREFERRED_HEIGHT
        } else {
            panel_height
        };
        let mut composed = self.build_composition(theme, composition_height);
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        // Keep a readable measure on large displays and shrink all the way to
        // a phone-like viewport. The body scrolls vertically when its copy is
        // taller than the bounded setup surface.
        if let Ok(mut cs) = taffy.style(child).cloned() {
            cs.size.width = taffy::Dimension::percent(1.0);
            // Pages with little content should own only the space they need.
            // The scroll viewport above caps longer pages, so this still
            // cannot exceed the safe laptop-height panel.
            cs.size.height = if ready_identity_page {
                taffy::Dimension::auto()
            } else if accessibility_expanded {
                taffy::Dimension::length(ACCESSIBLE_PANEL_HEIGHT)
            } else if dense_identity_page {
                taffy::Dimension::length(DENSE_PANEL_PREFERRED_HEIGHT)
            } else if codecs_page {
                taffy::Dimension::length(CODECS_PANEL_PREFERRED_HEIGHT)
            } else if intro_page {
                taffy::Dimension::length(INTRO_PANEL_HEIGHT)
            } else {
                taffy::Dimension::auto()
            };
            // The default automatic minimum is the content's min-height and
            // therefore prevents the scroll viewport from shrinking. This
            // previously put tall restore pages flush against (or beyond)
            // the physical display edge despite the parent's safety gutter.
            cs.min_size.height = taffy::Dimension::length(0.0);
            cs.max_size.width = taffy::Dimension::length(PANEL_MAX_WIDTH);
            cs.max_size.height = taffy::Dimension::length(panel_height);
            cs.flex_basis = taffy::Dimension::auto();
            cs.flex_grow = 0.0;
            cs.flex_shrink = 1.0;
            let _ = taffy.set_style(child, cs);
        }

        let style = taffy::Style {
            display: taffy::Display::Flex,
            // The viewport gutter is part of the viewport-sized shell, not
            // extra width outside it. This keeps the panel inside narrow
            // displays instead of overflowing by both side gutters.
            box_sizing: taffy::BoxSizing::BorderBox,
            flex_direction: taffy::FlexDirection::Column,
            justify_content: Some(taffy::JustifyContent::Center),
            align_items: Some(taffy::AlignItems::Center),
            size: taffy::Size::from_percent(1.0, 1.0),
            padding: taffy::Rect {
                left: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                right: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                top: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                bottom: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&cn) = children.first()
        {
            c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.draw_backdrop(scene, transform, theme);
        self.draw_composed(scene, transform, text_renderer, theme, presence);
        self.draw_picker(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some((_, picker)) = self.picker.as_mut() {
            picker.set_rect(onboarding_picker_rect(self.rect));
            let mut picker_taffy = TaffyTree::<()>::new();
            if let Ok(picker_node) = picker_taffy.new_leaf(taffy::Style::default()) {
                picker.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        taffy: &picker_taffy,
                        ..ctx.reborrow(ctx.path, picker_node)
                    },
                );
            }
            *ctx.handled = true;
            return;
        }
        if self.tour_page.is_some()
            && matches!(
                event,
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && matches!(event.logical_key, Key::Named(NamedKey::Escape))
            )
        {
            self.pressed.press(KEY_TOUR_SKIP);
            *ctx.handled = true;
            return;
        }
        // Escape follows the visible Back action while the identity ceremony
        // is still reversible. This is especially important on Nearby,
        // where a pairing attempt may be running: keyboard users need a
        // conventional way to leave the branch, and KEY_BACK shares the exact
        // same cleanup path that terminates that process group and clears its
        // transient status. Intro and Done deliberately remain non-dismissible
        // because setup has either not established an identity yet or still
        // needs an explicit hand-off into the desktop.
        if (self.flow.is_existing_branch() || self.flow.screen() == Screen::NoRecovery)
            && matches!(
                event,
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && matches!(event.logical_key, Key::Named(NamedKey::Escape))
            )
        {
            self.pressed.press(KEY_BACK);
            *ctx.handled = true;
            return;
        }
        if let Some(c) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(event, &mut ctx.reborrow(ctx.path, cn));
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        if let Some((_, picker)) = self.picker.as_ref() {
            return vec![picker];
        }
        self.composed
            .as_ref()
            .map(std::convert::AsRef::as_ref)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        if let Some((_, picker)) = self.picker.as_mut() {
            return vec![picker];
        }
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
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

    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        // Background work (nearby pairing, the codecs worker) changes the
        // page from `sync` with no input behind it. The compositor presents
        // only what `update` reports, so a page that is not yet laid out is
        // a change: otherwise a pairing that fails on the same screen stays
        // on its code until something unrelated repaints.
        self.invalidation.structure_pending()
    }

    fn take_focus_reset(&mut self) -> bool {
        self.invalidation.take_focus()
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = self.invalidation.take_structure();
        let child = self
            .composed
            .as_mut()
            .is_some_and(|c| c.take_structure_dirty());
        own || child
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        // The codecs worker and status watcher only raise `codec_changed`,
        // and nearby pairing reports through its child and a status file;
        // nothing wakes the compositor for them, so look while they run.
        let polling = self.prepared_worker.is_some()
            || self
                .migration
                .as_ref()
                .is_some_and(crate::onboarding_migration::Migration::busy)
            || self
                .portable_home
                .as_ref()
                .is_some_and(crate::onboarding_portable_home::Pending::busy)
            || self
                .portable_applications
                .as_ref()
                .is_some_and(crate::onboarding_portable_applications::Pending::busy)
            || self
                .browser
                .as_ref()
                .is_some_and(crate::onboarding_browser::Pending::busy)
            || self.codec_running.is_some()
            || self.codec_watching
            || self.nearby.is_some()
            || !self.journal_idle()
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
                .journal
                .front()
                .is_some_and(|write| write.outcome.get().is_some())
            || self
                .identity_work
                .as_ref()
                .is_some_and(IdentityWork::reported)
            || (!self.reduce_motion && self.transition.is_moving(ui::motion::frame_now()))
            || self
                .picker
                .as_ref()
                .is_some_and(|(_, picker)| picker.needs_animation_frame())
            || self
                .composed
                .as_ref()
                .is_some_and(|child| child.needs_animation_frame())
    }
}

fn screen_order(screen: Screen) -> u8 {
    match screen {
        Screen::Intro => 0,
        Screen::Existing | Screen::NoRecovery => 1,
        Screen::Nearby | Screen::Restore => 2,
        Screen::Done => 3,
    }
}

fn view_order(screen: Screen, tour: Option<TourPage>) -> u8 {
    match tour {
        Some(TourPage::Navigate) => 4,
        Some(TourPage::Organise) => 5,
        Some(TourPage::Collaborate) => 6,
        Some(TourPage::Start) => 7,
        None => screen_order(screen),
    }
}

/// OOBE is trusted compositor chrome, so its chooser follows the user rather
/// than an application's capability scope. Keep roots narrow: the home tree
/// plus conventional removable-media mount roots, never the whole filesystem.
fn onboarding_picker_scope() -> (PathBuf, Vec<PathBuf>) {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir());
    let mut roots = Vec::new();
    if let Some(home) = home.as_ref() {
        roots.push(home.clone());
    }
    let user = std::env::var_os("USER").filter(|value| !value.is_empty());
    let mut media_roots = Vec::new();
    if let Some(user) = user {
        for base in [PathBuf::from("/run/media"), PathBuf::from("/media")] {
            let candidate = base.join(&user);
            if candidate.is_dir() {
                media_roots.push(candidate);
            }
        }
    }
    if media_roots.is_empty() && PathBuf::from("/media").is_dir() {
        media_roots.push(PathBuf::from("/media"));
    }
    let mounted = media_roots
        .iter()
        .find(|root| directory_has_subdirectory(root))
        .cloned();
    roots.extend(media_roots);
    let initial = mounted
        .or_else(|| home.clone())
        .unwrap_or_else(std::env::temp_dir);
    if roots.is_empty() {
        roots.push(initial.clone());
    }
    (initial, roots)
}

fn directory_has_subdirectory(path: &std::path::Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
        })
    })
}

fn onboarding_picker_rect(viewport: Rect) -> Rect {
    let width = (viewport.width() * 0.78)
        .clamp(420.0, 900.0)
        .min(viewport.width());
    let height = (viewport.height() * 0.78)
        .clamp(320.0, 640.0)
        .min(viewport.height());
    let x0 = viewport.x0 + (viewport.width() - width) / 2.0;
    let y0 = viewport.y0 + (viewport.height() - height) / 2.0;
    Rect::new(x0, y0, x0 + width, y0 + height)
}

fn choice_row_height(theme: &Theme) -> f32 {
    let spacing = &theme.tokens.spacing;
    let text = theme
        .text(ui::tokens::typography::TextRole::Body)
        .line_height_px()
        + theme
            .text(ui::tokens::typography::TextRole::Caption)
            .line_height_px();
    (text + 2.0 * spacing.s2)
        .max(theme.tokens.metrics.min_pointer_target + spacing.s2)
        .ceil()
}

struct Choice {
    id: &'static str,
    icon: ui::icons::Icon,
    title: &'static str,
    detail: &'static str,
    /// The model key the row sets, read by sync like a button's.
    key: &'static str,
}

/// What the migration page says for its `status` in `state`; `retained`
/// when this device keeps the old computer's files where they are.
fn migration_body_key(status: &serde_json::Value, state: &str, retained: bool) -> &'static str {
    let apps = status
        .get("applications_only")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        || retained && matches!(state, "reviewed" | "ready-to-adopt");
    if json_flag(status, "browser") {
        "onboarding-browser-body"
    } else if status
        .get("portable_applications")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && matches!(state, "pending" | "failed" | "unavailable" | "cancelled")
    {
        "onboarding-portable-applications-body"
    } else if retained && state == "pending" {
        "onboarding-existing-files-body"
    } else if apps && state == "reviewed" {
        "onboarding-existing-files-apps-reviewed"
    } else if apps && state == "ready-to-adopt" {
        "onboarding-existing-files-apps-adopt"
    } else if status
        .get("portable_home")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && matches!(state, "pending" | "failed" | "unavailable")
    {
        "onboarding-portable-home-body"
    } else {
        match state {
            "pending" => "onboarding-migration-body",
            "reviewed" => "onboarding-migration-reviewed",
            "ready-to-adopt" => "onboarding-migration-adopt-body",
            _ if json_flag(status, "transfer_complete")
                && json_text(status, "cleanup_state") == Some("running") =>
            {
                "onboarding-migration-cleanup-running"
            }
            _ if json_flag(status, "transfer_complete")
                && json_text(status, "cleanup_state") == Some("pending") =>
            {
                "onboarding-migration-cleanup-pending"
            }
            _ if json_flag(status, "transfer_complete")
                && json_text(status, "cleanup_state") == Some("unavailable") =>
            {
                "onboarding-migration-cleanup-unavailable"
            }
            "complete" => "onboarding-migration-complete",
            "existing-files-available" => "onboarding-existing-files-available",
            "incomplete" => "onboarding-migration-incomplete",
            "failed" | "unavailable" => "onboarding-migration-failed",
            "cancelled" => "onboarding-migration-cancelled",
            _ => "onboarding-migration-working",
        }
    }
}

/// Whether `value`'s `key` is `true`.
fn json_flag(value: &serde_json::Value, key: &str) -> bool {
    value.get(key).and_then(serde_json::Value::as_bool) == Some(true)
}

/// `value`'s `key` as text.
fn json_text<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(serde_json::Value::as_str)
}

/// A step arriving now, on the drill.
fn step_arriving(motion: ui::tokens::EffectiveMotion) -> ui::motion::Presence {
    let mut arriving = ui::motion::Presence::closed();
    arriving.show(
        true,
        ui::motion::Drill::SPEC,
        motion,
        ui::tokens::MotionSource::Pointer,
        ui::time::now(),
    );
    arriving
}

#[cfg(test)]
mod tests {
    #[cfg(all(target_os = "linux", feature = "native-current-device-qualification"))]
    mod current_identity {
        include!("onboarding_current_identity_tests.rs");
    }
    #[test]
    fn completed_browser_continuation_skips_future_first_use_and_rejects_tampering() {
        let (home, directory, declaration) = crate::onboarding_browser::tests::fixture();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path());
        env.set("XDG_CONFIG_HOME", home.path().join(".config"));
        env.set("TILO_IDENTITY_ROOT", home.path().join("system"));
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let projection = crate::onboarding_browser::tests::certified_projection(
            home.path(),
            &directory,
            &declaration,
        );
        let original = std::fs::read(directory.join("primary.json")).unwrap();
        assert!(!crate::onboarding_prepared::session_pending(&directory));
        assert!(crate::onboarding_browser::validate_published_projection(
            &projection
        ));
        let mut view = OnboardingView::with_minter(Box::new(|| Err("must not mint".into())));
        view.identity_dir = Some(directory.clone());
        view.load_prepared_setup(&directory);
        assert!(!view.prepared_pending);
        assert_eq!(
            view.verified_browser_projection(),
            Some(projection.as_path())
        );
        view.flow.resume_created(MintedIdentity {
            fingerprint: hex::encode(declaration.plan.identity_public_key),
            path: directory.join("primary.json"),
        });
        assert!(!setup_actions_pending(&view));
        let catalog = semantic::test_dispatch(br#"{"op":"actions_enumerate"}"#, &mut view, None);
        assert!(
            !catalog.contains("migration.review"),
            "finished migration must not ask again"
        );
        assert!(
            view.invoke_migration("review", Some(&declaration.payload.operation))
                .is_err()
        );
        drop(view);
        assert!(
            !crate::onboarding_prepared::session_pending(&directory),
            "cold reopen must reuse independently verified completion without password"
        );
        let (_other_home, other_identity, _) = crate::onboarding_browser::tests::fixture();
        assert!(
            crate::onboarding_prepared::session_pending(&other_identity),
            "completion belongs to the original canonical person"
        );
        let mut other_view = OnboardingView::with_minter(Box::new(|| Err("must not mint".into())));
        other_view.identity_dir = Some(other_identity.clone());
        other_view.load_prepared_setup(&other_identity);
        assert!(other_view.prepared_pending);
        assert!(other_view.verified_browser_projection().is_none());
        let bookmarks = projection
            .join("profiles")
            .join(&declaration.plan.profiles.first().unwrap().id)
            .join("Bookmarks");
        std::fs::write(&bookmarks, b"changed after completion").unwrap();
        assert!(crate::onboarding_prepared::session_pending(&directory));
        assert!(!crate::onboarding_browser::validate_published_projection(
            &projection
        ));
        assert!(
            crate::onboarding_browser::Pending::open(
                home.path().into(),
                directory.clone(),
                declaration
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            original
        );
    }
    #[test]
    fn authenticated_browser_activation_restores_separate_first_use_semantic_intent() {
        let (home, directory, declaration) = crate::onboarding_browser::tests::fixture();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path());
        let before = std::fs::read(directory.join("primary.json")).unwrap();
        assert!(crate::onboarding_prepared::session_pending(&directory));
        let mut view = OnboardingView::with_minter(Box::new(|| Err("must not mint".into())));
        view.identity_dir = Some(directory.clone());
        view.load_prepared_setup(&directory);
        assert!(!view.prepared_pending);
        assert!(view.browser.is_some());
        assert!(view.portable_home.is_none());
        assert!(view.portable_applications.is_none());
        view.flow.resume_created(MintedIdentity {
            fingerprint: "existing".into(),
            path: directory.join("primary.json"),
        });
        assert!(setup_actions_pending(&view));
        assert_eq!(view.migration_status().unwrap()["browser"], true);
        let catalog = semantic::test_dispatch(br#"{"op":"actions_enumerate"}"#, &mut view, None);
        assert!(catalog.contains("migration.review"));
        assert!(catalog.contains("password"));
        assert!(
            view.invoke_migration("review", Some("different-operation"))
                .is_err()
        );
        let accepted = semantic::test_dispatch(
            br#"{"op":"actions_invoke","provider":"onboarding","id":"migration.later","args":{}}"#,
            &mut view,
            None,
        );
        assert!(accepted.contains("accepted"), "{accepted}");
        assert!(!setup_actions_pending(&view));
        assert!(view.verified_browser_projection().is_none());
        assert!(
            view.invoke_migration("resume", Some(&declaration.payload.operation))
                .is_ok()
        );
        assert!(setup_actions_pending(&view));
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            before
        );
    }
    #[test]
    fn authenticated_portable_apps_activation_resumes_actual_first_use_semantic_intent() {
        let (home, directory, declaration) =
            crate::onboarding_portable_applications::tests::fixture();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home.path());
        let receipt = home
            .path()
            .join(".local/state/tilo/prepared-setup/activated.json");
        assert!(
            std::fs::metadata(&receipt).unwrap().len() > 1024,
            "actual declaration must exercise prior too-small receipt bound"
        );
        let before = std::fs::read(directory.join("primary.json")).unwrap();
        let mut view = OnboardingView::with_minter(Box::new(|| Err("must not mint".into())));
        view.identity_dir = Some(directory.clone());
        view.load_prepared_setup(&directory);
        assert!(!view.prepared_pending);
        assert!(
            view.portable_home.is_none(),
            "apps-only grants no document/home migration"
        );
        assert!(view.portable_applications.is_some());
        view.flow.resume_created(MintedIdentity {
            fingerprint: "existing".into(),
            path: directory.join("primary.json"),
        });
        assert!(setup_actions_pending(&view));
        assert_eq!(view.migration_status().unwrap()["requires_password"], true);
        let catalog = semantic::test_dispatch(br#"{"op":"actions_enumerate"}"#, &mut view, None);
        assert!(catalog.contains("migration.review"));
        assert!(catalog.contains("password"));
        assert!(
            view.invoke_migration("review", Some("different-operation"))
                .is_err()
        );
        assert!(view.portable_applications.as_ref().unwrap().pending());
        let accepted = semantic::test_dispatch(
            br#"{"op":"actions_invoke","provider":"onboarding","id":"migration.later","args":{}}"#,
            &mut view,
            None,
        );
        assert!(accepted.contains("accepted"), "{accepted}");
        assert!(!setup_actions_pending(&view));
        assert_eq!(view.migration_status().unwrap()["deferred"], true);
        assert!(
            semantic::test_dispatch(br#"{"op":"actions_enumerate"}"#, &mut view, None)
                .contains("migration.resume")
        );
        assert!(
            view.invoke_migration("resume", Some(&declaration.operation))
                .is_ok()
        );
        assert!(setup_actions_pending(&view));
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            before
        );
        assert_eq!(view.migration_status().unwrap()["complete"], false);
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
    fn migration_first_use_semantic_actions_are_bound_to_pending_operation_and_defer_truthfully() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let plans = home.path().join(".local/state/tilo/migration-handoff");
        std::fs::create_dir_all(&plans).unwrap();
        std::fs::set_permissions(&plans, std::fs::Permissions::from_mode(0o700)).unwrap();
        let plan = plans.join("setup.migration.json");
        std::fs::write(&plan, b"{}").unwrap();
        std::fs::set_permissions(&plan, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut view = OnboardingView::with_minter(Box::new(|| Err("must not mint".into())));
        view.migration = Some(crate::onboarding_migration::Migration::open(
            home.path().into(),
        ));
        view.flow.resume_created(MintedIdentity {
            fingerprint: "existing".into(),
            path: home.path().join("selected.json"),
        });
        assert!(setup_actions_pending(&view));
        let catalog = semantic::test_dispatch(br#"{"op":"actions_enumerate"}"#, &mut view, None);
        assert!(catalog.contains("migration.review"));
        assert!(catalog.contains("migration.later"));
        let refused = semantic::test_dispatch(
            br#"{"op":"actions_invoke","provider":"onboarding","id":"migration.start","args":{}}"#,
            &mut view,
            None,
        );
        assert!(refused.contains("action_failed"));
        view.prepared_pending = true;
        assert!(view.migration_status().is_none());
        let refused = semantic::test_dispatch(
            br#"{"op":"actions_invoke","provider":"onboarding","id":"migration.later","args":{}}"#,
            &mut view,
            None,
        );
        assert!(refused.contains("action_failed"));
        view.prepared_pending = false;
        let accepted = semantic::test_dispatch(
            br#"{"op":"actions_invoke","provider":"onboarding","id":"migration.later","args":{}}"#,
            &mut view,
            None,
        );
        assert!(accepted.contains("accepted"));
        assert!(!setup_actions_pending(&view));
        let receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(plans.join("setup.acknowledged.json")).unwrap())
                .unwrap();
        assert_eq!(receipt["outcome"], "deferred");
        assert_eq!(std::fs::read(plan).unwrap(), b"{}");
        assert!(!home.path().join(".local/state/tilo/migration").exists());
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
    fn composite_cleanup_retry_is_visible_semantic_and_retains_rw_fence() {
        use launcher::retained_view::installed_copy::{CopyOwnerCommand, CopyOwnerEvent};
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        let home = tempfile::tempdir().unwrap();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        env.set("XDG_CONFIG_HOME", home.path().join(".config"));
        let identity = home.path().join(".config/tilo/identity");
        std::fs::create_dir_all(&identity).unwrap();
        for part in [".config", ".config/tilo", ".config/tilo/identity"] {
            std::fs::set_permissions(
                home.path().join(part),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        tilo_identity_host::provisioning::create_primary_identity(
            &identity,
            "Source owner",
            [20, 30, 40],
            tilo_identity_host::provisioning::IdentityStorage::for_session(),
        )
        .unwrap();
        runtime_identity::set_active_identity(&identity, "primary").unwrap();
        let source = uuid::Uuid::new_v4().to_string();
        let stage = uuid::Uuid::new_v4().to_string();
        let plans = home.path().join(".local/state/tilo/migration-handoff");
        std::fs::create_dir_all(&plans).unwrap();
        for part in [
            ".local",
            ".local/state",
            ".local/state/tilo",
            ".local/state/tilo/migration-handoff",
        ] {
            std::fs::set_permissions(
                home.path().join(part),
                std::fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let person = runtime_identity::load_from_path(identity.join("primary.json"))
            .unwrap()
            .group_public_key();
        let plan = serde_json::json!({"schema":1,"operation_id":source,
            "categories":{"home":true,"flatpak":false,"accounts":false},"home":{},
            "home_policy":"access-read-write","pending_computer_applications_scope":{
                "schema":1,"source_operation":source,"applications_operation":uuid::Uuid::new_v4().to_string(),
                "staging_operation":stage,"staging_plan_sha256":"a".repeat(64),
                "setup_binding":"owned setup fixture","identity_public_key":person}});
        let path = plans.join(format!("{source}.migration.json"));
        std::fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut view = OnboardingView::with_minter(Box::new(|| Err("must not mint".into())));
        view.flow.resume_created(MintedIdentity {
            fingerprint: "existing".into(),
            path: identity.join("primary.json"),
        });
        let mut migration = crate::onboarding_migration::Migration::open(home.path().into());
        migration.invoke("review", Some(&source)).unwrap();
        let copy = migration.take_original_copy_request().unwrap();
        copy.events
            .send(CopyOwnerEvent::Complete {
                operation: stage.clone(),
                home_cipher_sha256: None,
                applications_cipher_sha256: Some("b".repeat(64)),
            })
            .unwrap();
        assert!(migration.poll());
        view.migration = Some(migration);
        let running = accessibility_for_id(
            &*view.migration_actions("releasing-applications"),
            "onboarding.migration.next.button",
        )
        .unwrap();
        assert!(running.state.disabled);
        copy.events
            .send(CopyOwnerEvent::CleanupPending {
                operation: stage.clone(),
                message: "actual retirement pending".into(),
                resumable: true,
            })
            .unwrap();
        assert!(view.migration.as_mut().unwrap().poll());
        let actions = view.migration_actions("releasing-applications");
        let retry = accessibility_for_id(&*actions, "onboarding.migration.next.button").unwrap();
        assert!(!retry.state.disabled);
        assert_eq!(
            retry.name.as_deref(),
            Some(t("onboarding-migration-cleanup-resume").as_str())
        );
        let catalog = semantic::test_dispatch(br#"{"op":"actions_enumerate"}"#, &mut view, None);
        assert!(catalog.contains("migration.cleanup-resume"));
        let mut doc = AutoCommit::new();
        press(&view, &mut doc, "onboarding.migration.next");
        view.sync(&doc, &automerge::ROOT);
        assert!(
            matches!(copy.commands.try_recv().unwrap(),CopyOwnerCommand::ResumeCleanup{operation} if operation==stage)
        );
        assert_eq!(view.migration_status().unwrap()["cleanup_state"], "running");
        assert!(
            view.migration
                .as_mut()
                .unwrap()
                .take_access_request()
                .is_none()
        );
        assert!(
            accessibility_for_id(
                &*view.migration_actions("releasing-applications"),
                "onboarding.migration.next.button"
            )
            .unwrap()
            .state
            .disabled
        );
        copy.events
            .send(CopyOwnerEvent::CleanupPending {
                operation: stage,
                message: "nonresumable actual cleanup".into(),
                resumable: false,
            })
            .unwrap();
        assert!(view.migration.as_mut().unwrap().poll());
        assert!(
            accessibility_for_id(
                &*view.migration_actions("releasing-applications"),
                "onboarding.migration.next.button"
            )
            .unwrap()
            .state
            .disabled
        );
        assert!(
            view.migration
                .as_mut()
                .unwrap()
                .take_access_request()
                .is_none()
        );
        // Events here exercise the production controller/UI, not actual source
        // authority or observed native retirement, which the driver owns.
    }

    #[test]
    fn deferred_portable_home_remains_semantically_resumable_without_blocking_first_use() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let (home, directory, declaration) = crate::onboarding_portable_home::tests::fixture();
        let signer = runtime_identity::load_from_path(directory.join("primary.json")).unwrap();
        let mut view = OnboardingView::with_minter(Box::new(|| Err("must not mint".into())));
        view.portable_home = Some(
            crate::onboarding_portable_home::Pending::open(
                home.path().into(),
                directory.clone(),
                declaration,
            )
            .unwrap(),
        );
        view.flow.resume_created(MintedIdentity {
            fingerprint: hex::encode(signer.group_public_key()),
            path: directory.join("primary.json"),
        });
        assert!(setup_actions_pending(&view));
        view.invoke_migration("later", None).unwrap();
        assert!(!setup_actions_pending(&view));
        assert_eq!(view.migration_status().unwrap()["state"], "deferred");
        assert!(
            view.invoke_migration("resume", Some("wrong-operation"))
                .is_err()
        );
        let catalog = semantic::test_dispatch(br#"{"op":"actions_enumerate"}"#, &mut view, None);
        assert!(catalog.contains("migration.resume"));
        let answer = semantic::test_dispatch(
            br#"{"op":"actions_invoke","provider":"onboarding","id":"migration.resume","args":{}}"#,
            &mut view,
            None,
        );
        assert!(answer.contains("accepted"), "{answer}");
        assert!(setup_actions_pending(&view));
        assert_eq!(view.migration_status().unwrap()["state"], "pending");
        assert_eq!(
            runtime_identity::load_from_path(directory.join("primary.json"))
                .unwrap()
                .group_public_key(),
            signer.group_public_key()
        );
    }

    #[test]
    fn settings_reentry_resumes_attested_deferred_files_for_the_same_identity() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let (home, directory, declaration) = crate::onboarding_portable_home::tests::fixture();
        env.set("HOME", home.path());
        let before = std::fs::read(directory.join("primary.json")).unwrap();
        let mut pending = crate::onboarding_portable_home::Pending::open(
            home.path().into(),
            directory.clone(),
            declaration,
        )
        .unwrap();
        assert_eq!(
            OnboardingView::new_for_migration_resume(directory.clone())
                .err()
                .unwrap(),
            t("onboarding-migration-resume-unavailable"),
        );
        pending.invoke("later", b"").unwrap();
        let mut reopened = OnboardingView::new_for_migration_resume(directory.clone()).unwrap();
        assert!(setup_actions_pending(&reopened));
        assert_eq!(reopened.migration_status().unwrap()["state"], "pending");
        assert!(reopened.invoke_migration("review", None).is_err());
        assert!(
            setup_actions_pending(&reopened),
            "missing archive must remain pending"
        );
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            before
        );
        let receipt = home
            .path()
            .join(".local/state/tilo/prepared-home/choice.json");
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
        value["declaration"]["setup_binding"] = "other-setup".into();
        std::fs::write(receipt, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(OnboardingView::new_for_migration_resume(directory).is_err());
    }

    #[test]
    fn prepared_semantic_action_queues_same_intent_without_document_or_secret_receipts() {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let root = home.path().join(".local/state/tilo/prepared-setup");
        std::fs::create_dir_all(&root).unwrap();
        for path in [
            home.path().join(".local"),
            home.path().join(".local/state"),
            home.path().join(".local/state/tilo"),
            root.clone(),
        ] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let bytes = b"fixture encrypted transport bytes";
        for (name, data) in [("prepared-setup.sealed", bytes.to_vec()), ("receipt.json", serde_json::to_vec(&serde_json::json!({"schema":1,"sealed_bytes":bytes.len(),"ciphertext_sha256":hex::encode(Sha256::digest(bytes))})).unwrap())] {
            let path = root.join(name);
            std::fs::write(&path, data).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut view = OnboardingView::with_minter(Box::new(|| Err("must not mint".into())));
        view.prepared_pending = true;
        view.prepared_review =
            Some(crate::onboarding_prepared::Pending::open(home.path()).unwrap());
        let enumerate = semantic::test_dispatch(br#"{"op":"actions_enumerate"}"#, &mut view, None);
        assert!(enumerate.contains("password"));
        assert!(!enumerate.contains("shortpass"));
        let refused = semantic::test_dispatch(br#"{"op":"actions_invoke","provider":"onboarding","id":"prepared.import","args":{"password":"shortpass","extra":true}}"#, &mut view, None);
        assert!(refused.contains("invalid_arguments"));
        assert_eq!(view.prepared_setup_status(), Some("ready"));
        let accepted = semantic::test_dispatch(br#"{"op":"actions_invoke","provider":"onboarding","id":"prepared.import","args":{"password":"shortpass"}}"#, &mut view, None);
        assert!(accepted.contains("accepted"));
        assert!(!accepted.contains("shortpass"));
        assert_eq!(view.prepared_setup_status(), Some("busy"));
        assert!(view.pressed.take(KEY_PREPARED_IMPORT));
        assert_eq!(
            view.connect_passphrase.lock().unwrap().as_str(),
            "shortpass"
        );
        let refused =
            semantic::test_dispatch(br#"{"op":"click","target":"anything"}"#, &mut view, None);
        assert!(refused.contains("action_failed"));
        view.prepared_pending = false;
        let after = semantic::test_dispatch(br#"{"op":"actions_invoke","provider":"onboarding","id":"prepared.import","args":{"password":"shortpass"}}"#, &mut view, None);
        assert!(after.contains("action_failed"));
        clear_secret(&view.connect_passphrase);
    }
    #[test]
    fn pending_prepared_setup_refuses_all_identity_creation_intents() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        let mut view = OnboardingView::with_minter(Box::new(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("must not mint".into())
        }));
        view.prepared_pending = true;
        let mut doc = AutoCommit::new();
        for key in [
            KEY_BEGIN,
            KEY_EXISTING,
            KEY_CHOOSE_RESTORE,
            KEY_CREATE,
            KEY_CONNECT_RETRY,
        ] {
            press(&view, &mut doc, key);
            view.sync(&doc, &automerge::ROOT);
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(view.prepared_pending);
        assert!(view.choices().is_none());
        assert!(!view.finished);
    }

    use super::*;
    use automerge::transaction::Transactable as _;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use tilo_identity_ceremony::{IdentityRecord, SerializableIdentity};
    use winit::event::Modifiers;

    fn view() -> (OnboardingView, Arc<AtomicUsize>) {
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

    #[test]
    fn cancelling_nearby_pairing_revokes_the_entire_process_group() {
        let child = Command::new("sh")
            .args(["-c", "trap '' TERM; sleep 30 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("spawn pairing-shaped process tree");
        let pgid = i32::try_from(child.id()).expect("test process ID");
        // Give the shell enough time to create the descendant that used to
        // survive when only the group leader was killed.
        std::thread::sleep(Duration::from_millis(40));
        let mut child = Some(child);

        crate::onboarding_nearby::terminate(&mut child);

        assert!(child.is_none());
        let deadline = tilo_test_env::hang_deadline();
        loop {
            // A killed grandchild can remain as a zombie until the host's PID
            // 1 reaps it. `kill(-pgid, 0)` reports that inert bookkeeping
            // entry as present, so inspect procfs and require that the group
            // has no process capable of executing instead.
            if !process_group_has_live_member(pgid) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "a descendant survived cancellation in process group {pgid}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn nearby_pairing_news_asks_the_compositor_to_present() {
        // The helper reports through its status file with no input behind
        // it. The compositor paints only what `update` reports, so every
        // change the poll makes must be reported there, or a failure on the
        // same screen stays hidden behind the code.
        let dir = tempfile::tempdir().expect("tempdir");
        let identity_dir = dir.path().join("identity");
        std::fs::create_dir(&identity_dir).expect("identity dir");
        let status = identity_dir.join(".nearby-enrollment-status.json");
        let report_code = dir.path().join("report-code");
        let report_failure = dir.path().join("report-failure");
        let mut helper = Command::new("sh");
        helper
            .args([
                "-c",
                r#"while [ ! -e "$2" ]; do sleep 0.02; done
                   printf '%s' '{"state":"waiting_for_approval","verification_code":"424242"}' > "$1"
                   while [ ! -e "$3" ]; do sleep 0.02; done
                   printf '%s' '{"state":"failed","error":"Permission denied (os error 13)"}' > "$1"
                   exit 1"#,
                "helper",
            ])
            .arg(&status)
            .arg(&report_code)
            .arg(&report_failure);
        let (mut view, _) = view();
        view.flow.begin_existing();
        view.flow.choose_nearby();
        assert_eq!(view.flow.screen(), Screen::Nearby);
        view.nearby = Some(
            crate::onboarding_nearby::NearbyPairing::spawn(
                helper,
                &identity_dir,
                &status,
                Duration::from_secs(600),
            )
            .expect("spawn helper"),
        );
        view.nearby_status_path = Some(status);
        let doc = AutoCommit::new();
        // The page as it was laid out when pairing started.
        view.sync(&doc, &automerge::ROOT);
        let _ = view.take_structure_dirty();
        assert!(!view.update(&doc, &automerge::ROOT), "nothing new yet");

        std::fs::write(&report_code, b"").expect("let the helper show its code");
        let deadline = tilo_test_env::hang_deadline();
        while view.nearby_code.is_none() {
            assert!(Instant::now() < deadline, "the code never arrived");
            std::thread::sleep(Duration::from_millis(20));
            view.sync(&doc, &automerge::ROOT);
        }
        assert!(
            view.update(&doc, &automerge::ROOT),
            "the verification code is presented"
        );
        assert!(view.take_structure_dirty());
        assert!(
            !view.update(&doc, &automerge::ROOT),
            "a laid-out page is not presented again"
        );

        std::fs::write(&report_failure, b"").expect("let the helper fail");
        while view.nearby.is_some() {
            assert!(Instant::now() < deadline, "the failure never arrived");
            std::thread::sleep(Duration::from_millis(20));
            view.sync(&doc, &automerge::ROOT);
        }
        assert!(
            view.error()
                .is_some_and(|error| error.contains("Permission denied")),
            "{:?}",
            view.error()
        );
        assert!(
            view.update(&doc, &automerge::ROOT),
            "the failure and its Try again are presented"
        );
        let composition = view.build_composition(&Theme::dark(), PANEL_MAX_HEIGHT);
        assert!(
            accessibility_for_id(&*composition, "onboarding.connect.nearby.button").is_some(),
            "a failed pairing offers Try again"
        );
        assert!(
            accessibility_for_id(&*composition, "onboarding.nearby.code").is_none(),
            "the stale code is gone"
        );
        // What went wrong is the error's accessible name, not only its
        // description: a screen reader speaks it first, and the generic
        // outcome alone would hide that the commit was refused.
        let mut names = Vec::new();
        accessible_names(&*composition, &mut names);
        assert!(
            names.iter().any(|name| name.contains("Permission denied")),
            "the cause is named: {names:?}"
        );
        assert!(
            !names.contains(&t("onboarding-connect-failed")),
            "a refused commit is not reported generically: {names:?}"
        );
    }

    #[test]
    fn a_joined_identity_ends_first_use_on_the_ready_screen() {
        // A device that joined from a nearby one holds a public record and a
        // member certificate; the root key never leaves the approving device.
        // Its first use ends where the person can enter the desktop, not on a
        // backup step warning that losing this device loses the identity.
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let identity_dir = dir.path().join("identity");
        std::fs::create_dir(&identity_dir).expect("identity dir");
        std::fs::set_permissions(&identity_dir, std::fs::Permissions::from_mode(0o700))
            .expect("private identity dir");
        let record = identity_dir.join("identity-joined.json");
        std::fs::write(
            &record,
            runtime_identity::public_member_record("Member", [1, 2, 3], [7; 32]).expect("record"),
        )
        .expect("write record");
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600))
            .expect("private record");
        let status = identity_dir.join(".nearby-enrollment-status.json");
        let complete = serde_json::json!({
            "state": "complete",
            "verification_code": "123456",
            "identity": hex::encode([7; 32]),
            "record_path": record,
        });
        let mut helper = Command::new("sh");
        helper
            .args(["-c", r#"printf '%s' "$2" > "$1"; exec sleep 30"#, "helper"])
            .arg(&status)
            .arg(complete.to_string());
        let (mut view, _) = view();
        view.identity_dir = Some(identity_dir.clone());
        view.flow.begin_existing();
        view.flow.choose_nearby();
        view.nearby = Some(
            crate::onboarding_nearby::NearbyPairing::spawn(
                helper,
                &identity_dir,
                &status,
                Duration::from_secs(600),
            )
            .expect("spawn helper"),
        );
        view.nearby_status_path = Some(status);
        // Hold the journal write that records the join, so the assertion
        // below sees the hand-off waiting for it rather than racing it.
        let (release_journal, journal_gate) = std::sync::mpsc::channel::<()>();
        view.journal_writer = writer_held_until(journal_gate);
        let doc = AutoCommit::new();
        let deadline = tilo_test_env::hang_deadline();
        while view.nearby.is_some() {
            assert!(Instant::now() < deadline, "the join never arrived");
            std::thread::sleep(Duration::from_millis(20));
            view.sync(&doc, &automerge::ROOT);
        }
        assert!(
            !view.identity_ready_page,
            "the hand-off waits for the journal that records it"
        );
        release_journal
            .send(())
            .expect("the writer waits for its release");
        sync_journaled(&mut view, &doc);

        assert_eq!(view.flow.screen(), Screen::Done);
        assert_eq!(view.error(), None);
        assert!(view.identity_ready_page);
        assert_eq!(
            load_progress(&identity_dir).expect("load progress"),
            Some(PersistedStage::ReadyToEnter),
            "a restart resumes at the ready screen"
        );
        let composition = view.build_composition(&Theme::dark(), PANEL_MAX_HEIGHT);
        let mut names = Vec::new();
        accessible_names(&*composition, &mut names);
        assert!(
            names.contains(&t("onboarding-done-title")),
            "the joined device ends on the ready screen: {names:?}"
        );
        assert!(
            accessibility_for_id(&*composition, "onboarding.backup.skip-acknowledged.box")
                .is_none(),
            "a joined device is not asked to back up an identity it does not hold"
        );
        let joined_guidance = |names: &[String]| {
            assert!(
                names.contains(&t("onboarding-done-joined-key-elsewhere"))
                    && names.contains(&t("onboarding-done-joined-key-elsewhere-body")),
                "the ready screen says the key stays on the approving device: {names:?}"
            );
            assert!(
                !names.contains(&t("onboarding-done-backup-recommended")),
                "a joined device is not told a backup is still recommended: {names:?}"
            );
        };
        joined_guidance(&names);

        // A restart resumes on the same ready screen, knowing the device
        // joined from its identity record alone.
        runtime_identity::set_active_identity(&identity_dir, "identity-joined")
            .expect("select the joined identity");
        let (mut resumed, _) = self::view();
        resumed.identity_dir = Some(identity_dir);
        resumed.restore_pending_progress_from_disk();
        assert_eq!(resumed.flow.screen(), Screen::Done);
        let composition = resumed.build_composition(&Theme::dark(), PANEL_MAX_HEIGHT);
        let mut names = Vec::new();
        accessible_names(&*composition, &mut names);
        joined_guidance(&names);
    }

    fn process_group_has_live_member(pgid: i32) -> bool {
        std::fs::read_dir("/proc").is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                let Ok(process_id) = entry.file_name().to_string_lossy().parse::<u32>() else {
                    return false;
                };
                let Ok(stat) = std::fs::read_to_string(format!("/proc/{process_id}/stat")) else {
                    return false;
                };
                let Some((_, tail)) = stat.rsplit_once(')') else {
                    return false;
                };
                let mut fields = tail.split_whitespace();
                let state = fields.next();
                let _parent = fields.next();
                let process_group = fields.next().and_then(|field| field.parse::<i32>().ok());
                process_group == Some(pgid) && !matches!(state, Some("Z" | "X"))
            })
        })
    }

    fn press(view: &OnboardingView, doc: &mut AutoCommit, key: &'static str) {
        // Checkboxes are bound to the document; every other action is a
        // click on the view's button.
        if matches!(key, KEY_UNDERSTOOD | KEY_BACKUP_SKIP_ACKNOWLEDGED) {
            let _ = doc.put(&automerge::ROOT, key, ScalarValue::Boolean(true));
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

    fn accessibility_for_id(widget: &dyn Widget, id: &str) -> Option<ui::a11y::AccessNode> {
        ui::widget::find_test_id(widget, id)?.accessibility()
    }

    fn write_fixture_identity(directory: &std::path::Path) {
        let record = IdentityRecord {
            display_name: "Test user".into(),
            avatar_color: [20, 120, 220],
            profile_photo: None,
            threshold_identity: SerializableIdentity {
                single_device: true,
                group_public_key: [0xab; 32],
                threshold: 1,
                total_shares: 1,
                share_index: 1,
                secret_bytes: vec![0x42; 32],
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

    /// `sync`, then let the state writer finish each journal change the frame
    /// handed it and sync again, as the loop does when the write wakes it.
    /// Identity work is waited for the same way: the worker reports and the
    /// loop syncs again.
    fn sync_journaled(view: &mut OnboardingView, document: &AutoCommit) {
        view.sync(document, &automerge::ROOT);
        for _ in 0..16 {
            if view.journal_idle() && view.identity_idle() {
                return;
            }
            view.journal_writer.flush();
            wait_for_identity_work(view);
            view.sync(document, &automerge::ROOT);
        }
        panic!("journal changes and identity work kept coming");
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

    /// Minting, importing and exporting the identity wait for the disk, the
    /// secret store and a passphrase stretch. They are taken from a frame on
    /// the compositor's loop, so they must never run there: with a handler
    /// that never finishes, every frame still returns and the step waits.
    #[test]
    fn minting_never_runs_on_the_frame_thread() {
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
            view.identity_dir = Some(identity_dir.clone());
            let mut document = AutoCommit::new();
            for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
                press(&view, &mut document, key);
                view.sync(&document, &automerge::ROOT);
            }
            // The journal reaches the disk; the next frame starts the mint.
            view.journal_writer.flush();
            view.sync(&document, &automerge::ROOT);
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
                load_progress(&identity_dir).expect("load progress"),
                Some(PersistedStage::AcquiringIdentity)
            );

            release.send(()).unwrap();
            sync_journaled(&mut view, &document);
            assert_eq!(
                view.flow().screen(),
                Screen::Done,
                "Back waited out the mint"
            );
            assert_eq!(
                load_progress(&identity_dir).expect("load progress"),
                Some(PersistedStage::IdentityReady),
                "the journal records the identity after the mint reports"
            );
        }
    }

    /// See [`minting_never_runs_on_the_frame_thread`].
    #[test]
    fn importing_never_runs_on_the_frame_thread() {
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
            sync_journaled(&mut view, &document);
            press(&view, &mut document, KEY_IMPORT);
            view.sync(&document, &automerge::ROOT);
            view.journal_writer.flush();
            view.sync(&document, &automerge::ROOT);
            assert!(!view.identity_idle(), "the import is running");
            for _ in 0..3 {
                view.sync(&document, &automerge::ROOT);
            }
            assert_eq!(view.flow().screen(), Screen::Restore);

            release.send(()).unwrap();
            sync_journaled(&mut view, &document);
            assert_ne!(*ran_on.lock().unwrap(), Some(std::thread::current().id()));
            assert_eq!(view.flow().screen(), Screen::Done);
            assert!(
                view.connect_passphrase.lock().unwrap().is_empty(),
                "the passphrase is cleared once the import reports"
            );
        }
    }

    /// See [`minting_never_runs_on_the_frame_thread`].
    #[test]
    fn exporting_never_runs_on_the_frame_thread() {
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
            let mut document = AutoCommit::new();
            *view.backup_passphrase.lock().unwrap() = "correct horse battery staple".into();
            *view.backup_confirmation.lock().unwrap() = "correct horse battery staple".into();
            press(&view, &mut document, KEY_BACKUP_BROWSE);
            sync_journaled(&mut view, &document);
            view.picker.as_mut().expect("save picker").1.click_open();
            view.sync(&document, &automerge::ROOT);
            assert!(!view.identity_idle(), "the export is running");
            for _ in 0..3 {
                view.sync(&document, &automerge::ROOT);
            }
            assert!(view.backup_saved.is_none());

            release.send(()).unwrap();
            sync_journaled(&mut view, &document);
            assert_ne!(*ran_on.lock().unwrap(), Some(std::thread::current().id()));
            assert!(view.backup_saved.is_some());
            assert_eq!(
                load_progress(progress.path()).expect("backed-up progress"),
                Some(PersistedStage::IdentityReadyBackedUp)
            );
        }
    }

    fn finish_identity(view: &mut OnboardingView, document: &mut AutoCommit) {
        for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
            press(view, document, key);
            sync_journaled(view, document);
        }
        assert_eq!(view.flow().screen(), Screen::Done);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn setup_journal_is_durable_resumable_and_fails_closed() {
        // Phase `journal_is_durable_before_identity_action_and_removed_only_after_finish`.
        {
            let directory = TempDir::new().expect("progress directory");
            let identity_dir = directory.path().join("fresh").join("identity");
            let observed_precommit = Arc::new(AtomicBool::new(false));
            let observed_in_minter = Arc::clone(&observed_precommit);
            let progress_dir = identity_dir.clone();
            let mut view = OnboardingView::with_minter(Box::new(move || {
                observed_in_minter.store(
                    matches!(
                        load_progress(&progress_dir),
                        Ok(Some(PersistedStage::AcquiringIdentity))
                    ),
                    Ordering::SeqCst,
                );
                Ok(MintedIdentity {
                    fingerprint: "ab".repeat(32),
                    path: progress_dir.join("primary.json"),
                })
            }));
            view.identity_dir = Some(identity_dir.clone());
            let mut document = AutoCommit::new();

            finish_identity(&mut view, &mut document);
            assert!(
                observed_precommit.load(Ordering::SeqCst),
                "the crash journal must reach disk before the minter can write an identity"
            );
            assert_eq!(
                load_progress(&identity_dir).expect("load progress"),
                Some(PersistedStage::IdentityReady)
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert_eq!(
                    std::fs::metadata(progress_path(&identity_dir))
                        .expect("progress metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
                assert_eq!(
                    std::fs::metadata(&identity_dir)
                        .expect("identity directory metadata")
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
            }

            // The keyboard/action route carries the same guard as the disabled
            // button. A stale or malicious document action cannot silently skip
            // the only recovery decision.
            press(&view, &mut document, KEY_BACKUP_CONTINUE);
            sync_journaled(&mut view, &document);
            assert!(
                !view.identity_ready_page,
                "continuing without a backup requires an explicit acknowledgement"
            );
            assert_eq!(
                load_progress(&identity_dir).expect("load progress"),
                Some(PersistedStage::IdentityReady)
            );

            press(&view, &mut document, KEY_BACKUP_SKIP_ACKNOWLEDGED);
            sync_journaled(&mut view, &document);
            press(&view, &mut document, KEY_BACKUP_CONTINUE);
            sync_journaled(&mut view, &document);
            assert_eq!(
                load_progress(&identity_dir).expect("load progress"),
                Some(PersistedStage::ReadyToEnter)
            );
            press(&view, &mut document, KEY_FINISH);
            sync_journaled(&mut view, &document);
            assert!(view.finished());
            assert!(!has_pending_progress(&identity_dir));
        }
        // Phase `verified_pre_acquisition_journal_is_cleared_without_discarding_later_progress`.
        {
            let directory = TempDir::new().expect("progress directory");
            let identity_dir = directory.path().join("identity");

            store_progress(&identity_dir, PersistedStage::AcquiringIdentity)
                .expect("store pre-acquisition progress");
            assert!(
                clear_verified_pre_acquisition_progress(&identity_dir)
                    .expect("clear verified pre-acquisition progress")
            );
            assert_eq!(load_progress(&identity_dir).expect("load progress"), None);

            store_progress(&identity_dir, PersistedStage::IdentityReady)
                .expect("store post-acquisition progress");
            assert!(
                !clear_verified_pre_acquisition_progress(&identity_dir)
                    .expect("retain post-acquisition progress")
            );
            assert_eq!(
                load_progress(&identity_dir).expect("load progress"),
                Some(PersistedStage::IdentityReady),
                "successful verification must not skip backup or tour completion"
            );
        }
        // Phase `restart_resumes_backup_ready_and_exact_tour_page_without_recreating_identity`.
        {
            let directory = TempDir::new().expect("progress directory");
            write_fixture_identity(directory.path());

            store_progress(directory.path(), PersistedStage::IdentityReady)
                .expect("backup journal");
            let mut backup = OnboardingView::new(directory.path().to_path_buf());
            assert_eq!(backup.flow().screen(), Screen::Done);
            assert!(!backup.identity_ready_page);
            let mut blocked_finish = AutoCommit::new();
            press(&backup, &mut blocked_finish, KEY_FINISH);
            sync_journaled(&mut backup, &blocked_finish);
            assert!(!backup.finished(), "backup choice cannot be bypassed");

            store_progress(directory.path(), PersistedStage::IdentityReadyBackedUp)
                .expect("verified-backup journal");
            let backed_up = OnboardingView::new(directory.path().to_path_buf());
            assert!(!backed_up.identity_ready_page);
            assert!(backed_up.backup_was_verified);

            store_progress(directory.path(), PersistedStage::ReadyToEnter).expect("ready journal");

            let mut done = OnboardingView::new(directory.path().to_path_buf());
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
            press(&done, &mut document, KEY_TOUR_START);
            sync_journaled(&mut done, &document);
            assert_eq!(done.tour_page, Some(TourPage::Navigate));
            assert!(
                done.presents_desktop_context(),
                "the first-login tour must explain the desktop over the real desktop"
            );
            assert_eq!(
                load_progress(directory.path()).expect("tour journal"),
                Some(PersistedStage::TourNavigate)
            );

            let mut navigate_restart = OnboardingView::new(directory.path().to_path_buf());
            assert_eq!(navigate_restart.tour_page, Some(TourPage::Navigate));
            press(&navigate_restart, &mut document, KEY_TOUR_NEXT);
            sync_journaled(&mut navigate_restart, &document);
            assert_eq!(navigate_restart.tour_page, Some(TourPage::Organise));

            let mut organise_restart = OnboardingView::new(directory.path().to_path_buf());
            assert_eq!(organise_restart.tour_page, Some(TourPage::Organise));
            press(&organise_restart, &mut document, KEY_TOUR_NEXT);
            sync_journaled(&mut organise_restart, &document);
            assert_eq!(organise_restart.tour_page, Some(TourPage::Collaborate));

            let mut collaborate_restart = OnboardingView::new(directory.path().to_path_buf());
            assert_eq!(collaborate_restart.tour_page, Some(TourPage::Collaborate));
            press(&collaborate_restart, &mut document, KEY_TOUR_NEXT);
            sync_journaled(&mut collaborate_restart, &document);
            assert_eq!(collaborate_restart.tour_page, Some(TourPage::Start));

            let mut start_restart = OnboardingView::new(directory.path().to_path_buf());
            assert_eq!(start_restart.tour_page, Some(TourPage::Start));
            press(&start_restart, &mut document, KEY_FINISH);
            sync_journaled(&mut start_restart, &document);
            assert!(start_restart.finished());
            assert!(!has_pending_progress(directory.path()));

            let established = OnboardingView::new(directory.path().to_path_buf());
            assert_eq!(
                established.flow().screen(),
                Screen::Intro,
                "without a pending journal startup leaves an established user alone"
            );
        }
        // Phase `unsupported_or_unsafe_journal_stays_pending_instead_of_bypassing_setup`.
        {
            let directory = TempDir::new().expect("progress directory");
            tilo_files::native::atomic::write_with_mode(
                &progress_path(directory.path()),
                br#"{"version":99,"stage":"identity-ready"}"#,
                0o600,
            )
            .expect("future-version journal");
            assert!(has_pending_progress(directory.path()));
            assert!(load_progress(directory.path()).is_err());
            write_fixture_identity(directory.path());
            let resumed = OnboardingView::new(directory.path().to_path_buf());
            assert_eq!(resumed.flow().screen(), Screen::Done);
            assert!(resumed.progress_error.is_some());

            #[cfg(unix)]
            {
                use std::os::unix::fs::symlink;
                std::fs::remove_file(progress_path(directory.path())).expect("remove journal");
                symlink("primary.json", progress_path(directory.path())).expect("journal symlink");
                assert!(has_pending_progress(directory.path()));
                assert!(load_progress(directory.path()).is_err());
            }
        }
        // Phase `a_journal_write_failure_prevents_identity_creation`.
        {
            let calls = Arc::new(AtomicUsize::new(0));
            let called = Arc::clone(&calls);
            let mut view = OnboardingView::with_minter(Box::new(move || {
                called.fetch_add(1, Ordering::SeqCst);
                Ok(MintedIdentity {
                    fingerprint: "ab".repeat(32),
                    path: PathBuf::from("/identity/primary.json"),
                })
            }));
            let directory = TempDir::new().expect("progress directory");
            let blocked_parent = directory.path().join("not-a-directory");
            std::fs::write(&blocked_parent, b"block directory creation").expect("write blocker");
            view.identity_dir = Some(blocked_parent.join("identity"));
            let mut document = AutoCommit::new();
            for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
                press(&view, &mut document, key);
                sync_journaled(&mut view, &document);
            }

            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(view.flow().screen(), Screen::NoRecovery);
            assert!(view.progress_error.is_some());
        }
    }

    /// A state writer whose thread is parked in a job from now on, for the
    /// life of the process: the test's stand-in for a crash before the
    /// journal changes queued behind it reach the disk.
    /// A journal writer whose first job waits until `release` is signalled,
    /// so a test can look at the view while a journal change is pending.
    fn writer_held_until(
        release: std::sync::mpsc::Receiver<()>,
    ) -> &'static crate::shell::durable_writer::DurableWriter {
        let writer = Box::leak(Box::new(
            crate::shell::durable_writer::DurableWriter::spawn("onboarding-journal-held"),
        ));
        writer
            .submit_ordered(move || {
                let _ = release.recv();
            })
            .expect("the held writer takes its gate");
        writer
    }

    fn writer_that_never_finishes() -> &'static crate::shell::durable_writer::DurableWriter {
        let writer = Box::leak(Box::new(
            crate::shell::durable_writer::DurableWriter::spawn("onboarding-journal-test"),
        ));
        let (started, running) = std::sync::mpsc::channel::<()>();
        writer.submit(PathBuf::from("parked"), move || {
            started.send(()).expect("test alive");
            loop {
                std::thread::park();
            }
        });
        running.recv().expect("the writer parked");
        writer
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

    #[test]
    fn no_identity_is_acquired_before_its_journal_reaches_the_disk() {
        let directory = TempDir::new().expect("progress directory");
        let identity_dir = directory.path().join("identity");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut view = OnboardingView::with_minter(counting_minter(&calls, &identity_dir));
        view.identity_dir = Some(identity_dir.clone());
        view.journal_writer = writer_that_never_finishes();
        let mut document = AutoCommit::new();
        for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
            press(&view, &mut document, key);
            view.sync(&document, &automerge::ROOT);
        }
        for _ in 0..3 {
            view.sync(&document, &automerge::ROOT);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the minter waits for the journal written ahead of it"
        );
        assert!(!view.journal_idle());
        assert_ne!(view.flow().screen(), Screen::Done);
        assert!(view.next_wake_deadline().is_some(), "the loop looks again");

        // A crash now leaves no journal and no identity: setup starts over.
        drop(view);
        assert_eq!(load_progress(&identity_dir).expect("load progress"), None);
        assert!(!has_pending_progress(&identity_dir));

        // With a writer that finishes, the identity follows its journal.
        let mut view = OnboardingView::with_minter(counting_minter(&calls, &identity_dir));
        view.identity_dir = Some(identity_dir.clone());
        for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
            press(&view, &mut document, key);
            view.sync(&document, &automerge::ROOT);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "not on the frame it was asked"
        );
        sync_journaled(&mut view, &document);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(view.flow().screen(), Screen::Done);
        assert_eq!(
            load_progress(&identity_dir).expect("load progress"),
            Some(PersistedStage::IdentityReady)
        );
    }

    /// Setup's actions run on the compositor's loop: none of its journal
    /// changes may wait for the disk there.
    #[cfg(target_os = "linux")]
    #[test]
    fn setup_stage_transitions_never_wait_for_the_disk() {
        let directory = TempDir::new().expect("progress directory");
        let identity_dir = directory.path().join("identity");
        let calls = Arc::new(AtomicUsize::new(0));
        let minter = counting_minter(&calls, &identity_dir);
        let journal_dir = identity_dir.clone();
        let stages = crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(move || {
            let mut view = OnboardingView::with_minter(minter);
            view.identity_dir = Some(journal_dir.clone());
            let mut document = AutoCommit::new();
            let mut stages = Vec::new();
            for key in [
                KEY_BEGIN,
                KEY_UNDERSTOOD,
                KEY_CREATE,
                KEY_BACKUP_SKIP_ACKNOWLEDGED,
                KEY_BACKUP_CONTINUE,
                KEY_TOUR_START,
                KEY_TOUR_NEXT,
                KEY_TOUR_SKIP,
            ] {
                press(&view, &mut document, key);
                sync_journaled(&mut view, &document);
                assert_eq!(view.progress_error, None, "after {key}");
                stages.push(load_progress(&journal_dir).expect("load progress"));
            }
            assert!(view.finished(), "setup finished");
            stages
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            stages[2..],
            [
                Some(PersistedStage::IdentityReady),
                Some(PersistedStage::IdentityReady),
                Some(PersistedStage::ReadyToEnter),
                Some(PersistedStage::TourNavigate),
                Some(PersistedStage::TourOrganise),
                None,
            ]
        );
        assert!(!has_pending_progress(&identity_dir));
    }

    #[test]
    fn identity_creation_is_gated_on_acknowledgement_and_mints_once() {
        // Phase `structure_and_focus_invalidations_are_consumed_independently`.
        {
            let mut invalidation = Invalidation::default();
            invalidation.both();

            assert!(invalidation.take_structure());
            assert!(!invalidation.take_structure());
            assert!(invalidation.take_focus());
            assert!(!invalidation.take_focus());

            invalidation.both();
            assert!(invalidation.take_focus());
            assert!(invalidation.take_structure());
        }
        // Phase `pressing_create_without_acknowledging_mints_nothing`.
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
            assert_eq!(v.flow().screen(), Screen::NoRecovery, "still on the gate");
        }
        // Phase `acknowledging_then_creating_mints_exactly_once`.
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
        // Phase `unticking_the_box_closes_the_gate_again`.
        {
            let (mut v, calls) = view();
            let mut doc = AutoCommit::new();
            press(&v, &mut doc, KEY_BEGIN);
            v.sync(&doc, &automerge::ROOT);
            press(&v, &mut doc, KEY_UNDERSTOOD);
            v.sync(&doc, &automerge::ROOT);
            let _ = doc.put(
                &automerge::ROOT,
                KEY_UNDERSTOOD,
                ScalarValue::Boolean(false),
            );
            v.sync(&doc, &automerge::ROOT);
            assert!(!v.flow().can_create());
            press(&v, &mut doc, KEY_CREATE);
            v.sync(&doc, &automerge::ROOT);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        // Phase `a_failed_mint_is_surfaced_rather_than_hidden`.
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
        // Phase `finishing_releases_the_desktop`.
        {
            let (mut v, _) = view();
            let mut doc = AutoCommit::new();
            assert!(!v.finished());
            press(&v, &mut doc, KEY_BEGIN);
            v.sync(&doc, &automerge::ROOT);
            press(&v, &mut doc, KEY_UNDERSTOOD);
            v.sync(&doc, &automerge::ROOT);
            press(&v, &mut doc, KEY_CREATE);
            v.sync(&doc, &automerge::ROOT);
            press(&v, &mut doc, KEY_BACKUP_SKIP_ACKNOWLEDGED);
            v.sync(&doc, &automerge::ROOT);
            press(&v, &mut doc, KEY_BACKUP_CONTINUE);
            v.sync(&doc, &automerge::ROOT);
            press(&v, &mut doc, KEY_FINISH);
            v.sync(&doc, &automerge::ROOT);
            assert!(v.finished());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn desktop_tour_is_navigable_skippable_and_dismissable() {
        // Phase `desktop_tour_is_complete_backtrackable_and_finishes_only_at_the_end`.
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
        // Phase `every_tour_page_exposes_an_explicit_skip_action`.
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
                let actions = view.tour_actions(page);
                let node = accessibility_for_id(actions.as_ref(), "onboarding.tour.skip.button")
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
        // Phase `first_use_skip_works_on_every_tour_page_and_clears_progress`.
        {
            for (stage, page) in [
                (PersistedStage::TourNavigate, TourPage::Navigate),
                (PersistedStage::TourOrganise, TourPage::Organise),
                (PersistedStage::TourCollaborate, TourPage::Collaborate),
                (PersistedStage::TourStart, TourPage::Start),
            ] {
                let directory = TempDir::new().expect("progress directory");
                write_fixture_identity(directory.path());
                store_progress(directory.path(), stage).expect("tour progress");
                let mut view = OnboardingView::new(directory.path().to_path_buf());
                let mut document = AutoCommit::new();

                assert_eq!(view.tour_page, Some(page));
                press(&view, &mut document, KEY_TOUR_SKIP);
                sync_journaled(&mut view, &document);

                assert!(view.finished(), "Skip tour must work from {page:?}");
                assert!(
                    !has_pending_progress(directory.path()),
                    "skipping {page:?} must durably complete first use"
                );
            }
        }
        // Phase `escape_dismisses_a_reopened_tour_from_every_page`.
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
        // Phase `reopened_tour_has_no_identity_gate_and_can_be_dismissed`.
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
        // Phase `resumed_tour_preserves_focus_reset_and_reduced_motion_navigation`.
        {
            let directory = TempDir::new().expect("progress directory");
            write_fixture_identity(directory.path());
            store_progress(directory.path(), PersistedStage::TourOrganise)
                .expect("organise journal");
            let mut view = OnboardingView::new(directory.path().to_path_buf());

            assert_eq!(view.tour_page, Some(TourPage::Organise));
            assert!(
                view.take_focus_reset(),
                "a restored page must place keyboard focus inside its rebuilt controls"
            );
            assert!(!view.take_focus_reset(), "the focus edge is one-shot");

            let theme = Theme::dark().with_reduce_motion(true);
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = view.build_layout(&mut taffy, &theme, &mut node_map);
            assert!(view.reduce_motion);
            assert!(
                !view.needs_animation_frame(),
                "restoring a tour must not restart entrance animation under reduced motion"
            );

            let mut document = AutoCommit::new();
            press(&view, &mut document, KEY_TOUR_NEXT);
            sync_journaled(&mut view, &document);
            assert_eq!(view.tour_page, Some(TourPage::Collaborate));
            assert_eq!(
                load_progress(directory.path()).expect("updated tour journal"),
                Some(PersistedStage::TourCollaborate)
            );
            assert!(
                view.take_focus_reset(),
                "keyboard navigation must move focus into the newly restored stage"
            );
            assert!(!view.needs_animation_frame());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn onboarding_copy_and_live_regions_speak_the_real_contract() {
        // Phase `desktop_tour_teaches_the_real_keyboard_and_gesture_contract`.
        {
            let navigation = NAVIGATE_CARDS
                .iter()
                .flat_map(|(title, body)| {
                    [
                        t(title),
                        tour_shortcut(title).map(t).unwrap_or_default(),
                        t(body),
                    ]
                })
                .collect::<Vec<_>>()
                .join(" ");

            if cfg!(target_os = "macos") {
                assert!(navigation.contains("Command+K"));
                assert!(navigation.contains("Option+Space"));
            } else {
                assert!(navigation.contains("Super+Space"));
            }
            assert!(navigation.contains("Start here, Everyday, Work, Media, and System tools"));
            assert!(navigation.contains("Super+Tab"));
            assert!(navigation.contains("Super+Tab or pinch"));
            assert!(navigation.contains("Super+arrow keys"));
            assert!(navigation.contains("three-finger swipe"));
            assert!(navigation.contains("up maximizes the window and down restores it"));
            assert!(navigation.contains("Ctrl+Super+Up and Down"));
            assert!(navigation.contains("Super+1 to 9 straight to a strip"));
            assert!(navigation.contains("a four-finger swipe, switch workspaces"));
            assert!(navigation.contains("Left and right step between tiled windows"));
            assert!(
                !navigation.contains("Super+W"),
                "Super+W closes the focused tab; teaching it as Overview would be destructive"
            );

            assert_eq!(
                ORGANISE_CARDS
                    .iter()
                    .map(|(title, _)| t(title))
                    .collect::<Vec<_>>(),
                ["Workspace", "Strip", "Window group", "Tab"],
                "the tour must name every level of the desktop hierarchy"
            );
        }
        // Phase `desktop_tour_ends_with_a_concrete_collaboration_handoff`.
        {
            let handoff = START_CARDS
                .iter()
                .flat_map(|(title, body)| [t(title), t(body)])
                .collect::<Vec<_>>()
                .join(" ");

            for real_surface in [
                "Workspace Settings",
                "Notes",
                "Calendar",
                "Chat",
                "Invite people",
                "Activity",
            ] {
                assert!(
                    handoff.contains(real_surface),
                    "first-use handoff must name the real {real_surface} surface"
                );
            }
            assert!(handoff.contains("undo only the change"));
        }
        // Phase `onboarding_secret_fields_have_stable_spoken_names`.
        {
            let buffer = Arc::new(Mutex::new(String::new()));
            let input = secret_input(
                "onboarding.test.secret",
                &buffer,
                "Backup passphrase",
                "Twelve or more characters",
            );
            let node = Widget::accessibility(&input).expect("text input accessibility node");

            assert_eq!(node.name.as_deref(), Some("Backup passphrase"));
            assert!(
                node.state.protected,
                "passphrases must be exposed as protected"
            );
        }
        // Phase `nearby_pairing_updates_share_one_polite_live_region`.
        {
            let (mut view, _) = view();
            view.flow.begin_existing();
            view.flow.choose_nearby();
            let theme = Theme::dark();

            for status in [
                "Waiting for your other device.",
                "Check that your other device shows the same code, then approve it there.",
                "Connected. Your workspaces will appear automatically.",
            ] {
                view.nearby_status = Some(status.into());
                let mut taffy = TaffyTree::<()>::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let _ = view.build_layout(&mut taffy, &theme, &mut node_map);
                let node = accessibility_for_id(&view, "onboarding.nearby.status")
                    .expect("nearby status accessibility node");

                assert_eq!(node.role, ui::a11y::Role::StaticText);
                assert_eq!(node.name.as_deref(), Some(status));
                assert_eq!(node.live, Some(ui::a11y::Politeness::Polite));
            }

            view.nearby_code = Some("123456".into());
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = view.build_layout(&mut taffy, &theme, &mut node_map);
            let code = accessibility_for_id(&view, "onboarding.nearby.code")
                .expect("the verification code is shown on its own");
            assert_eq!(code.name.as_deref(), Some("123 456"));
        }
        // Phase `nearby_steps_say_this_device_waits_for_the_other_one`.
        {
            let waiting = t("onboarding-nearby-waiting").to_lowercase();
            assert!(waiting.contains("other device"), "{waiting}");
            assert!(!waiting.contains("trusted device nearby"), "{waiting}");
            let steps = [
                "onboarding-nearby-step-open",
                "onboarding-nearby-step-add",
                "onboarding-nearby-step-compare",
            ]
            .map(t)
            .join(" ");
            for surface in ["Settings", "Devices & Sync", "Add device", "six-digit code"] {
                assert!(
                    steps.contains(surface),
                    "nearby steps must name {surface}: {steps}"
                );
            }
        }
        // Phase `onboarding_copy_uses_no_typed_glyphs_as_layout`.
        {
            let copy = [
                "onboarding-intro-point-collaboration",
                "onboarding-intro-point-local",
                "onboarding-intro-point-not-account",
                "onboarding-nearby-step-open",
                "onboarding-nearby-step-add",
                "onboarding-nearby-step-compare",
                "onboarding-nearby-code",
                "onboarding-progress-stage",
                "onboarding-tour-eyebrow",
                "onboarding-tour-stage",
                "onboarding-tour-search-title",
                "onboarding-tour-overview-title",
                "onboarding-tour-move-title",
                "onboarding-tour-name-title",
            ]
            .map(t)
            .join(" ");
            for glyph in ['•', '·', '→', '—', '✓', '★'] {
                assert!(!copy.contains(glyph), "{glyph:?} used as layout in {copy}");
            }
        }
        // Phase `host_desktop_copy_does_not_claim_to_replace_the_linux_login`.
        {
            let copy = host_desktop_intro_body().to_ascii_lowercase();
            assert!(copy.contains("already signed in"));
            assert!(copy.contains("does not replace your linux login"));
            assert!(copy.contains("does not") && copy.contains("online account"));
            assert!(copy.contains("not all cryptographically signed"));
        }
    }

    #[test]
    fn screen_navigation_follows_context_direction_escape_and_motion_preferences() {
        // Phase `host_desktop_context_keeps_the_identity_creation_gate`.
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
        // Phase `screen_transitions_track_forward_and_back_direction`.
        {
            let (mut view, _) = view();
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            view.sync(&doc, &root);
            view.pressed.press(KEY_BEGIN);
            view.sync(&doc, &root);
            assert_eq!(view.transition_direction, 1.0);
            view.pressed.press(KEY_BACK);
            view.sync(&doc, &root);
            assert_eq!(view.transition_direction, -1.0);
        }
        // Phase `escape_follows_back_on_reversible_identity_steps_only`.
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
        // Phase `reduced_motion_does_not_request_transition_frames`.
        {
            let (mut view, _) = view();
            view.reduce_motion = true;
            view.composed = None;
            view.picker = None;
            assert!(!view.needs_animation_frame());
        }
        // Phase `recovery_mode_starts_on_restore_and_never_offers_creation`.
        {
            let identity = tempfile::tempdir().expect("identity directory");
            let mut recovery = OnboardingView::new_for_recovery(identity.path().to_path_buf());
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
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = recovery.build_layout(&mut taffy, &theme, &mut node_map);
            assert!(
                accessibility_for_id(&recovery, "onboarding.connect.retry.button").is_some(),
                "recovery must expose a keyring retry action"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn connecting_an_existing_identity_journals_and_imports_without_minting() {
        // Phase `connect_existing_imports_without_minting`.
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
        // Phase `connect_journals_before_import_and_keeps_setup_pending_afterward`.
        {
            let directory = TempDir::new().expect("progress directory");
            let identity_dir = directory.path().join("identity");
            let observed_precommit = Arc::new(AtomicBool::new(false));
            let observed_in_importer = Arc::clone(&observed_precommit);
            let progress_dir = identity_dir.clone();
            let mut view = OnboardingView::with_handlers(
                Box::new(|| Err("create should not run".to_string())),
                Box::new(move |_, _| {
                    observed_in_importer.store(
                        matches!(
                            load_progress(&progress_dir),
                            Ok(Some(PersistedStage::AcquiringIdentity))
                        ),
                        Ordering::SeqCst,
                    );
                    Ok(MintedIdentity {
                        fingerprint: "cd".repeat(32),
                        path: progress_dir.join("primary.json"),
                    })
                }),
            );
            view.identity_dir = Some(identity_dir.clone());
            let mut document = AutoCommit::new();

            press(&view, &mut document, KEY_EXISTING);
            press(&view, &mut document, KEY_CHOOSE_RESTORE);
            view.connect_path.set("/media/backup/primary.json".into());
            sync_journaled(&mut view, &document);
            press(&view, &mut document, KEY_IMPORT);
            sync_journaled(&mut view, &document);

            assert!(
                observed_precommit.load(Ordering::SeqCst),
                "the crash journal must reach disk before an import can write the identity"
            );
            assert_eq!(
                load_progress(&identity_dir).expect("load progress"),
                Some(PersistedStage::IdentityReady)
            );
            assert_eq!(view.flow().screen(), Screen::Done);
        }
        // Phase `failed_connect_stays_on_connect_screen_and_shows_error`.
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
        // Phase `connect_requires_a_selected_file_before_importing`.
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
                &*view.build_composition(&theme, PANEL_MAX_HEIGHT),
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
                &*view.build_composition(&theme, PANEL_MAX_HEIGHT),
                "onboarding.import.button",
            )
            .expect("restore action remains exposed");
            assert!(
                !enabled.state.disabled,
                "selecting a source must enable the restore action"
            );
        }
        // Phase `connect_browse_opens_a_shell_owned_modal_picker`.
        {
            let (mut view, _) = view();
            let mut doc = AutoCommit::new();
            press(&view, &mut doc, KEY_EXISTING);
            press(&view, &mut doc, KEY_CHOOSE_RESTORE);
            view.sync(&doc, &automerge::ROOT);
            press(&view, &mut doc, KEY_CONNECT_BROWSE);
            view.sync(&doc, &automerge::ROOT);
            assert!(view.picker.is_some());

            view.picker.as_mut().expect("picker").1.click_cancel();
            view.sync(&doc, &automerge::ROOT);
            assert!(view.picker.is_none());
            assert_eq!(view.flow().screen(), Screen::Restore);
        }
    }

    fn setup_actions_pending(view: &OnboardingView) -> bool {
        semantic::test_observe(view).is_some_and(|status| {
            matches!(status["phase"].as_str(), Some("prepared" | "migration"))
        })
    }

    fn backup_console(view: &mut OnboardingView, request: &serde_json::Value) -> serde_json::Value {
        serde_json::from_str(&semantic::test_dispatch(
            &serde_json::to_vec(request).unwrap(),
            view,
            None,
        ))
        .unwrap()
    }
    fn backup_action_id(view: &mut OnboardingView, operation: &str) -> String {
        let catalog = backup_console(view, &serde_json::json!({"op":"actions_enumerate"}));
        catalog["answer"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["operation"] == operation)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn real_backup_view(directory: &Path) -> OnboardingView {
        let identity =
            crate::onboarding::existing_identity_at(&directory.join("primary.json")).unwrap();
        let mut view = OnboardingView::with_all_handlers(
            Box::new(|| Err("unused minter".into())),
            Box::new(|_, _| Err("unused importer".into())),
            Box::new(|identity, destination, passphrase| {
                runtime_identity::export_portable_backup(identity, destination, passphrase)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }),
        );
        view.identity_dir = Some(directory.to_owned());
        view.backup_capability = Box::new(canonical_backup_capability);
        if has_pending_progress(directory) {
            view.restore_pending_progress_from_disk();
            view.refresh_backup_capability().unwrap();
        } else {
            view.flow.begin_existing();
            view.accept_connected_identity(identity).unwrap();
        }
        // Identity work and the progress journal run off the loop; settle
        // them as a frame loop would before an agent acts.
        sync_journaled(&mut view, &AutoCommit::new());
        view
    }
    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
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
                    let deadline = Instant::now() + Duration::from_secs(5);
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
                .env(runtime_identity::FILE_SECRETS_ENV, "1");
            unsafe {
                command.pre_exec(move || {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let child = command.spawn().unwrap();
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
        let directory = runtime_identity::identity_dir_from_env().unwrap();
        let minted = crate::onboarding::mint_into(&directory).unwrap();
        let original = std::fs::read(&minted.path).unwrap();
        let view = real_backup_view(&directory);
        assert!(
            view.flow().is_done(),
            "the genuine identity ceremony is complete"
        );
        assert!(
            !view.finished(),
            "first-use continuation still owns the desktop"
        );
        assert!(semantic::test_observe(&view).is_some());
        assert!(
            !view.identity_setup_active(),
            "the actual certified identity may activate transport"
        );
        assert_eq!(std::fs::read(&minted.path).unwrap(), original);
        let person: [u8; 32] = hex::decode(&minted.fingerprint)
            .unwrap()
            .try_into()
            .unwrap();
        let certificate =
            crate::firstboot::collaboration_identity_paths().device_cert_for_user(&person);
        let held = certificate.with_extension("continuation-held");
        std::fs::rename(&certificate, &held).unwrap();
        assert!(
            view.identity_setup_active(),
            "missing certificate cannot activate transport"
        );
        assert!(!view.finished());
        assert!(semantic::test_observe(&view).is_some());
        std::fs::rename(&held, &certificate).unwrap();
        assert!(!view.identity_setup_active());
        assert!(!view.finished());
        assert_eq!(std::fs::read(&minted.path).unwrap(), original);
    }
    #[test]
    fn first_login_backup_semantics_save_verified_ciphertext_and_retire_at_continue() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("identity");
        tilo_identity_host::provisioning::create_primary_identity(
            &directory,
            "Backup fixture",
            [128; 3],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let original = std::fs::read(directory.join("primary.json")).unwrap();
        let mut view = real_backup_view(&directory);
        assert!(!setup_actions_pending(&view));
        assert!(
            semantic::test_observe(&view).is_some(),
            "first-use service survives identity activation"
        );
        let save = backup_action_id(&mut view, "backup.save");
        let next = backup_action_id(&mut view, "backup.continue");
        let backup = root.path().join("recovery.tilo-identity");
        let passphrase = "synthetic private backup passphrase";
        let mismatch = backup_console(
            &mut view,
            &serde_json::json!({"op":"actions_invoke", "provider":"onboarding", "id":save,
            "args":{"destination":backup,"passphrase":passphrase,"confirmation":"different"}}),
        );
        assert!(mismatch.get("err").is_some());
        assert!(!backup.exists());
        let answer = backup_console(
            &mut view,
            &serde_json::json!({"op":"actions_invoke", "provider":"onboarding", "id":save,
            "args":{"destination":backup,"passphrase":passphrase,"confirmation":passphrase}}),
        );
        assert_eq!(answer["answer"]["status"], "accepted");
        // The backup is written off the loop; take its report as a frame would.
        sync_journaled(&mut view, &AutoCommit::new());
        let proof =
            runtime_identity::verify_portable_backup(&backup, passphrase.as_bytes()).unwrap();
        assert_eq!(
            proof.group_public_key,
            runtime_identity::load_from_path(directory.join("primary.json"))
                .unwrap()
                .group_public_key()
        );
        assert_eq!(
            load_progress(&directory).unwrap(),
            Some(PersistedStage::IdentityReadyBackedUp)
        );
        let status = backup_console(&mut view, &serde_json::json!({"op":"actions_status"}));
        assert_eq!(status["answer"]["backup"]["state"], "verified");
        assert!(!status.to_string().contains(passphrase));
        assert!(!status.to_string().contains(backup.to_str().unwrap()));
        let doc = AutoCommit::new();
        assert_eq!(
            backup_console(
                &mut view,
                &serde_json::json!({"op":"actions_invoke","provider":"onboarding","id":next,"args":{}})
            )["answer"]["status"],
            "accepted"
        );
        sync_journaled(&mut view, &doc);
        assert!(view.identity_ready_page);
        assert!(semantic::test_observe(&view).is_some());
        assert_eq!(
            load_progress(&directory).unwrap(),
            Some(PersistedStage::ReadyToEnterBackedUp)
        );
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            original
        );
        assert!(backup_console(&mut view, &serde_json::json!({"op":"actions_invoke","provider":"onboarding","id":save,"args":{"destination":backup,"passphrase":passphrase,"confirmation":passphrase}})).get("err").is_some());
        // Actual loaded recovery state preserves the verified backup after restart.
        let mut reopened = real_backup_view(&directory);
        reopened.restore_pending_progress_from_disk();
        assert!(reopened.backup_was_verified);
        assert!(reopened.identity_ready_page);
    }
    #[test]
    fn first_login_backup_semantic_deferral_requires_explicit_acknowledgement() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("identity");
        tilo_identity_host::provisioning::create_primary_identity(
            &directory,
            "Deferral fixture",
            [128; 3],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let original = std::fs::read(directory.join("primary.json")).unwrap();
        let mut view = real_backup_view(&directory);
        let next = backup_action_id(&mut view, "backup.continue");
        assert!(backup_console(&mut view, &serde_json::json!({"op":"actions_invoke","provider":"onboarding","id":next,"args":{}})).get("err").is_some());
        assert!(!view.identity_ready_page);
        assert!(semantic::test_observe(&view).is_some());
        assert_eq!(
            backup_console(
                &mut view,
                &serde_json::json!({"op":"actions_invoke","provider":"onboarding","id":next,"args":{"acknowledge_without_backup":true}})
            )["answer"]["status"],
            "accepted"
        );
        let doc = AutoCommit::new();
        sync_journaled(&mut view, &doc);
        assert!(view.identity_ready_page);
        assert!(!view.backup_was_verified);
        assert!(semantic::test_observe(&view).is_some());
        assert_eq!(
            load_progress(&directory).unwrap(),
            Some(PersistedStage::ReadyToEnter)
        );
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

    fn invoke_footer(view: &mut OnboardingView, operation: &str, doc: &AutoCommit) -> String {
        let id = backup_action_id(view, operation);
        let reply = backup_console(
            view,
            &serde_json::json!({
                "op":"actions_invoke", "provider":"onboarding", "id":id, "args":{}
            }),
        );
        assert_eq!(reply["answer"]["status"], "accepted");
        sync_journaled(view, doc);
        id
    }

    #[test]
    fn first_login_footer_semantics_preserve_progress_through_tour_and_finish() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("identity");
        let public = ed25519_dalek::SigningKey::from_bytes(&[19; 32])
            .verifying_key()
            .to_bytes();
        let record =
            runtime_identity::public_member_record("Tour person", [128; 3], public).unwrap();
        write_private_member_fixture(&directory, &record);
        let mut view = real_backup_view(&directory);
        let doc = AutoCommit::new();
        assert!(view.identity_ready_page);
        assert!(semantic::test_observe(&view).is_some());
        let ready_finish = backup_action_id(&mut view, "onboarding.finish");
        invoke_footer(&mut view, "onboarding.tour.start", &doc);
        assert_eq!(view.tour_step(), Some(1));
        assert_eq!(
            load_progress(&directory).unwrap(),
            Some(PersistedStage::TourNavigate)
        );
        assert!(backup_console(&mut view, &serde_json::json!({"op":"actions_invoke","provider":"onboarding","id":ready_finish,"args":{}})).get("err").is_some());
        let first_next = invoke_footer(&mut view, "onboarding.tour.next", &doc);
        assert_eq!(view.tour_step(), Some(2));
        assert!(backup_console(&mut view, &serde_json::json!({"op":"actions_invoke","provider":"onboarding","id":first_next,"args":{}})).get("err").is_some());
        invoke_footer(&mut view, "onboarding.tour.back", &doc);
        assert_eq!(view.tour_step(), Some(1));
        for step in [2, 3, 4] {
            invoke_footer(&mut view, "onboarding.tour.next", &doc);
            assert_eq!(view.tour_step(), Some(step));
            assert!(semantic::test_observe(&view).is_some());
        }
        let last = invoke_footer(&mut view, "onboarding.finish", &doc);
        assert!(view.finished());
        assert!(semantic::test_observe(&view).is_none());
        assert_eq!(load_progress(&directory).unwrap(), None);
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            record
        );
        assert!(backup_console(&mut view, &serde_json::json!({"op":"actions_invoke","provider":"onboarding","id":last,"args":{}})).get("err").is_some());
    }

    #[test]
    fn first_login_codec_semantics_skip_to_ready_and_retire_only_at_finish() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("identity");
        tilo_identity_host::provisioning::create_primary_identity(
            &directory,
            "Codec person",
            [128; 3],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let original = std::fs::read(directory.join("primary.json")).unwrap();
        let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
        let mut view = real_backup_view(&directory)
            .with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
        let continue_id = backup_action_id(&mut view, "backup.continue");
        assert_eq!(
            backup_console(
                &mut view,
                &serde_json::json!({"op":"actions_invoke","provider":"onboarding","id":continue_id,"args":{"acknowledge_without_backup":true}})
            )["answer"]["status"],
            "accepted"
        );
        let doc = AutoCommit::new();
        sync_journaled(&mut view, &doc);
        assert!(view.codec_page_visible());
        assert!(semantic::test_observe(&view).is_some());
        let stale_install = backup_action_id(&mut view, "onboarding.codecs.install");
        invoke_footer(&mut view, "onboarding.codecs.skip", &doc);
        assert!(!view.codec_page_visible());
        assert!(fake.runs().is_empty());
        assert!(semantic::test_observe(&view).is_some());
        assert!(backup_console(&mut view, &serde_json::json!({"op":"actions_invoke","provider":"onboarding","id":stale_install,"args":{}})).get("err").is_some());
        invoke_footer(&mut view, "onboarding.finish", &doc);
        assert!(view.finished());
        assert!(semantic::test_observe(&view).is_none());
        assert_eq!(load_progress(&directory).unwrap(), None);
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            original
        );
    }

    #[test]
    fn first_login_codec_semantics_start_actual_worker_then_continue_and_finish() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("identity");
        tilo_identity_host::provisioning::create_primary_identity(
            &directory,
            "Codec worker person",
            [128; 3],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let record = std::fs::read(directory.join("primary.json")).unwrap();
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let (started_tx, started) = std::sync::mpsc::channel();
        let started_tx = Mutex::new(started_tx);
        let setup = CodecSetup::with(
            Arc::new(|| CodecPackStatus::parse(r#"{"state":"not-installed"}"#)),
            Arc::new(move |action| {
                started_tx.lock().unwrap().send(action).unwrap();
                gate.lock().unwrap().recv().unwrap();
                Ok(())
            }),
        );
        let mut view = real_backup_view(&directory).with_codec_setup(setup);
        let continue_id = backup_action_id(&mut view, "backup.continue");
        assert_eq!(
            backup_console(
                &mut view,
                &serde_json::json!({"op":"actions_invoke","provider":"onboarding","id":continue_id,"args":{"acknowledge_without_backup":true}})
            )["answer"]["status"],
            "accepted"
        );
        let doc = AutoCommit::new();
        sync_journaled(&mut view, &doc);
        assert!(view.codec_page_visible());
        let install = invoke_footer(&mut view, "onboarding.codecs.install", &doc);
        assert_eq!(
            started
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            CodecAction::Install
        );
        assert!(view.codec_running.is_some());
        assert!(backup_console(&mut view, &serde_json::json!({"op":"actions_invoke","provider":"onboarding","id":install,"args":{}})).get("err").is_some());
        invoke_footer(&mut view, "onboarding.codecs.continue", &doc);
        assert!(!view.codec_page_visible());
        assert!(semantic::test_observe(&view).is_some());
        release.send(()).unwrap();
        invoke_footer(&mut view, "onboarding.finish", &doc);
        assert!(view.finished());
        assert!(semantic::test_observe(&view).is_none());
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            record
        );
    }

    #[test]
    fn keyring_retry_public_member_uses_actual_acquisition_reducer_and_skips_backup() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("identity");
        let public = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
            .verifying_key()
            .to_bytes();
        let record =
            runtime_identity::public_member_record("Existing person", [128; 3], public).unwrap();
        let path = directory.join("primary.json");
        write_private_member_fixture(&directory, &record);
        let mut view = OnboardingView::with_all_handlers(
            Box::new(move || crate::onboarding::existing_identity_at(&path)),
            Box::new(|_, _| Err("unused importer".into())),
            Box::new(|_, _, _| panic!("a public member must never export a personal backup")),
        );
        view.identity_dir = Some(directory.clone());
        view.backup_capability = Box::new(canonical_backup_capability);
        view.recovery_only = true;
        view.flow.begin_existing();
        view.press(KEY_CONNECT_RETRY);
        let doc = AutoCommit::new();
        sync_journaled(&mut view, &doc);
        assert!(view.flow.is_done());
        assert!(view.identity_ready_page);
        assert!(!view.backup_export_available);
        assert!(!view.backup_was_verified);
        assert!(view.error.is_none());
        assert_eq!(
            load_progress(&directory).unwrap(),
            Some(PersistedStage::ReadyToEnter)
        );
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            record
        );
    }

    #[test]
    fn first_login_public_member_skips_backup_without_reminting_or_losing_progress() {
        let root = TempDir::new().unwrap();
        let directory = root.path().join("identity");
        let public = ed25519_dalek::SigningKey::from_bytes(&[7; 32])
            .verifying_key()
            .to_bytes();
        let record =
            runtime_identity::public_member_record("Paired person", [128; 3], public).unwrap();
        write_private_member_fixture(&directory, &record);
        let mut view = real_backup_view(&directory);
        assert!(!view.backup_export_available);
        assert!(
            view.identity_ready_page,
            "actual connected acquisition skips the irrelevant backup step"
        );
        assert!(!view.backup_was_verified);
        assert!(semantic::test_observe(&view).is_some());
        let catalog = backup_console(&mut view, &serde_json::json!({"op":"actions_enumerate"}));
        assert!(
            !catalog["answer"]["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["operation"]
                    .as_str()
                    .is_some_and(|op| op.starts_with("backup.")))
        );
        let theme = Theme::dark();
        let composition = view.build_composition(&theme, PANEL_MAX_HEIGHT);
        assert!(accessibility_for_id(&*composition, "onboarding.backup.passphrase").is_none());
        assert!(accessibility_for_id(&*composition, "onboarding.backup.button").is_none());
        assert_eq!(
            load_progress(&directory).unwrap(),
            Some(PersistedStage::ReadyToEnter)
        );
        assert_eq!(
            std::fs::read(directory.join("primary.json")).unwrap(),
            record
        );
        let mut reopened = real_backup_view(&directory);
        reopened.restore_pending_progress_from_disk();
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn portable_backup_verifies_passphrases_and_returns_to_the_desktop() {
        // Phase `portable_backup_requires_matching_strong_passphrases_and_reports_success`.
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
            let mut doc = AutoCommit::new();

            *view.backup_passphrase.lock().unwrap() = "correct horse battery staple".into();
            *view.backup_confirmation.lock().unwrap() = "does not match".into();
            press(&view, &mut doc, KEY_BACKUP_BROWSE);
            sync_journaled(&mut view, &doc);
            assert!(view.picker.is_none());
            assert!(
                view.backup_error
                    .as_deref()
                    .unwrap()
                    .contains("do not match")
            );

            *view.backup_confirmation.lock().unwrap() = "correct horse battery staple".into();
            press(&view, &mut doc, KEY_BACKUP_BROWSE);
            sync_journaled(&mut view, &doc);
            view.picker.as_mut().expect("save picker").1.click_cancel();
            sync_journaled(&mut view, &doc);
            assert!(view.picker.is_none());
            assert_eq!(exports.load(Ordering::SeqCst), 0);

            press(&view, &mut doc, KEY_BACKUP_BROWSE);
            sync_journaled(&mut view, &doc);
            view.picker.as_mut().expect("save picker").1.click_open();
            sync_journaled(&mut view, &doc);
            assert_eq!(exports.load(Ordering::SeqCst), 1);
            assert!(view.backup_saved.is_some());
            assert!(view.backup_error.is_none());
            assert_eq!(
                load_progress(progress.path()).expect("verified backup progress"),
                Some(PersistedStage::IdentityReadyBackedUp)
            );
            assert!(view.backup_passphrase.lock().unwrap().is_empty());
            assert!(view.backup_confirmation.lock().unwrap().is_empty());

            press(&view, &mut doc, KEY_BACKUP_CONTINUE);
            sync_journaled(&mut view, &doc);
            assert!(view.identity_ready_page);
            assert_eq!(
                load_progress(progress.path()).expect("ready progress"),
                Some(PersistedStage::ReadyToEnterBackedUp)
            );
        }
        // Phase `existing_identity_backup_entry_reuses_the_verified_backup_screen`.
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
            let composition = view.build_composition(&theme, PANEL_MAX_HEIGHT);
            assert!(
                accessibility_for_id(&*composition, "onboarding.backup.button").is_some(),
                "the existing identity route must expose the same shell-owned export control"
            );
        }
        // Phase `verified_existing_identity_backup_returns_directly_to_desktop`.
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
            view.backup_was_verified = true;
            store_progress(progress.path(), PersistedStage::IdentityReadyBackedUp)
                .expect("verified backup progress");

            let mut doc = AutoCommit::new();
            press(&view, &mut doc, KEY_BACKUP_CONTINUE);
            sync_journaled(&mut view, &doc);

            assert!(view.finished());
            assert!(!view.identity_ready_page);
            assert!(view.tour_page.is_none());
            assert!(
                !has_pending_progress(progress.path()),
                "leaving Settings after verification must not resurrect first-login setup"
            );
        }
        // Phase `verified_backup_reveals_copy_form_only_after_the_optional_action`.
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

    fn layout(view: &mut OnboardingView) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = view.build_layout(&mut taffy, &theme, &mut node_map);
    }

    /// The label of the control a focus reset lands on.
    fn default_focus_label(view: &OnboardingView) -> Option<String> {
        fn find(widget: &dyn Widget) -> Option<String> {
            if widget.is_default_keyboard_focus() {
                return widget.accessibility().and_then(|node| node.name);
            }
            widget.children().into_iter().find_map(find)
        }
        view.composed.as_deref().and_then(find)
    }

    fn stage_label(view: &OnboardingView) -> Option<String> {
        view.composed
            .as_deref()
            .and_then(|composed| accessibility_for_id(composed, "onboarding.progress.stage"))
            .and_then(|node| node.name)
    }

    #[test]
    fn the_codecs_step_is_one_install_button_with_nothing_chosen() {
        let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
        let (view, _) = view();
        let mut view = view.with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
        let mut doc = AutoCommit::new();
        walk_to_codecs(&mut view, &mut doc);
        assert!(view.codec_page_visible());
        layout(&mut view);
        assert_eq!(stage_label(&view).as_deref(), Some("Step 4 of 5"));
        let lines = view.codec_page_lines();
        let note = accessibility_for_id(
            view.composed.as_deref().expect("rendered codecs page"),
            "onboarding.codecs.note",
        )
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
            default_focus_label(&view).as_deref(),
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

    #[test]
    fn installing_from_setup_records_the_choice_and_shows_the_result() {
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
        assert_eq!(stage_label(&view).as_deref(), Some("Step 5 of 5"));
        press(&view, &mut doc, KEY_FINISH);
        view.sync(&doc, &automerge::ROOT);
        assert!(view.finished());
    }

    #[test]
    fn not_now_installs_nothing_and_continues() {
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

    #[test]
    fn setup_continues_while_the_download_runs() {
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let gate = Mutex::new(gate);
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
                let _ = gate.lock().unwrap().recv();
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
        assert!(lines.contains(&"Progress: 29%".to_string()), "{lines:?}");
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

    #[test]
    fn an_offline_choice_is_kept_and_installs_later() {
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

    #[test]
    fn a_refused_install_is_shown_inline_with_retry_and_skip() {
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

    #[test]
    fn a_choice_made_in_the_installer_is_not_asked_again() {
        let fake = FakeCodecs::new(r#"{"state":"waiting-for-network","desired":true}"#);
        let (view, _) = view();
        let mut view = view.with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
        let mut doc = AutoCommit::new();
        walk_to_codecs(&mut view, &mut doc);
        assert!(!view.codec_page_visible());
        layout(&mut view);
        assert_eq!(stage_label(&view).as_deref(), Some("Step 4 of 4"));
    }

    #[test]
    fn every_codecs_visual_state_builds_a_layout() {
        let (mut view, _) = view();
        for state in [
            OnboardingVisualState::Codecs,
            OnboardingVisualState::CodecsInstalling,
            OnboardingVisualState::CodecsWaitingForNetwork,
            OnboardingVisualState::CodecsInstalled,
            OnboardingVisualState::CodecsFailed,
        ] {
            view.set_visual_state_for_snapshot(state);
            assert!(view.codec_page_visible());
            layout(&mut view);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn every_screen_lays_out_inside_the_viewport() {
        // Phase `every_screen_builds_a_layout`.
        // Every screen must lay out — a ceremony that panics on the screen after the one under test is worse than no ceremony.
        {
            let (mut v, _) = view();
            let mut doc = AutoCommit::new();
            for step in [
                None,
                Some(KEY_BEGIN),
                Some(KEY_UNDERSTOOD),
                Some(KEY_CREATE),
                Some(KEY_BACKUP_SKIP_ACKNOWLEDGED),
                Some(KEY_BACKUP_CONTINUE),
            ] {
                if let Some(k) = step {
                    press(&v, &mut doc, k);
                    v.sync(&doc, &automerge::ROOT);
                }
                let theme = Theme::dark();
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let _ = v.build_layout(&mut taffy, &theme, &mut node_map);
            }

            for keys in [
                &[KEY_EXISTING][..],
                &[KEY_EXISTING, KEY_CHOOSE_NEARBY],
                &[KEY_EXISTING, KEY_CHOOSE_RESTORE],
            ] {
                let (mut connect, _) = view();
                let mut connect_doc = AutoCommit::new();
                for key in keys {
                    press(&connect, &mut connect_doc, key);
                    connect.sync(&connect_doc, &automerge::ROOT);
                }
                let theme = Theme::dark();
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let _ = connect.build_layout(&mut taffy, &theme, &mut node_map);
            }
        }
        // Phase `narrow_layout_keeps_the_setup_surface_inside_the_viewport`.
        {
            let (mut view, _) = view();
            let theme = Theme::dark();
            let mut text = ui::text_render::TextRenderer::new();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = view.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout_with_measure(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(420.0),
                        height: taffy::AvailableSpace::Definite(640.0),
                    },
                    |known, available, node_id, _, _| {
                        node_map
                            .get(&node_id)
                            .map_or(taffy::Size::ZERO, |&ptr| unsafe {
                                (*ptr).measure(known, available, &mut text)
                            })
                    },
                )
                .expect("narrow setup layout");
            view.apply_layout(&taffy, node, Point::ZERO);
            let panel = view.composed.as_ref().expect("composition").get_rect();
            assert!(panel.x0 >= 0.0 && panel.x1 <= 420.0, "panel={panel:?}");
            assert!(panel.y0 >= 0.0 && panel.y1 <= 640.0, "panel={panel:?}");
        }
        // Phase `tour_backdrop_only_scrims_the_covered_desktop`.
        {
            for theme in [Theme::dark(), Theme::high_contrast()] {
                let mut view = OnboardingView::new_tour();
                view.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
                let mut scene = Scene::new();
                view.draw_backdrop(&mut scene, Affine::IDENTITY, &theme);

                assert_eq!(
                    scene.encoding().n_paths,
                    1,
                    "a modal tour backdrop must not add a detached desktop focus target"
                );
            }
        }
        // Phase `introduction_uses_a_compact_panel_on_a_large_display`.
        {
            let (mut view, _) = view();
            let theme = Theme::dark();
            let mut text = ui::text_render::TextRenderer::new();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = view.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout_with_measure(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(1600.0),
                        height: taffy::AvailableSpace::Definite(900.0),
                    },
                    |known, available, node_id, _, _| {
                        node_map
                            .get(&node_id)
                            .map_or(taffy::Size::ZERO, |&ptr| unsafe {
                                (*ptr).measure(known, available, &mut text)
                            })
                    },
                )
                .expect("large setup layout");
            view.apply_layout(&taffy, node, Point::ZERO);
            let panel = view.composed.as_ref().expect("composition").get_rect();
            assert!(
                panel.height() <= f64::from(INTRO_PANEL_HEIGHT) + 0.5,
                "intro should not inherit the taller recovery panel: {panel:?}"
            );
            assert!(
                panel.height() >= 300.0,
                "intro content must retain a readable surface: {panel:?}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "the fixture crosses identity pages and error states with laptop sizes, text scaling and high contrast to check action alignment"
    )]
    fn host_identity_actions_stay_visible_and_aligned_at_supported_laptop_sizes() {
        let scenarios: &[(&[&str], &[&str], bool)] = &[
            (&[KEY_EXISTING], &["onboarding.back.button"], false),
            (
                &[KEY_EXISTING, KEY_CHOOSE_NEARBY],
                &["onboarding.back.button", "onboarding.connect.nearby.button"],
                false,
            ),
            (
                &[KEY_EXISTING, KEY_CHOOSE_RESTORE],
                &["onboarding.back.button", "onboarding.import.button"],
                false,
            ),
            (
                &[KEY_EXISTING, KEY_CHOOSE_RESTORE],
                &["onboarding.back.button", "onboarding.import.button"],
                true,
            ),
            (
                &[KEY_BEGIN],
                &["onboarding.back.button", "onboarding.create.button"],
                false,
            ),
            (
                &[KEY_BEGIN],
                &["onboarding.back.button", "onboarding.create.button"],
                true,
            ),
            (
                &[KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE],
                &["onboarding.backup.continue.button"],
                false,
            ),
            (
                &[
                    KEY_BEGIN,
                    KEY_UNDERSTOOD,
                    KEY_CREATE,
                    KEY_BACKUP_SKIP_ACKNOWLEDGED,
                    KEY_BACKUP_CONTINUE,
                ],
                &["onboarding.tour.start.button", "onboarding.finish.button"],
                false,
            ),
        ];

        for (width, height, theme) in vec![
            (1600.0, 900.0, Theme::dark()),
            (1366.0, 768.0, Theme::dark()),
            (1280.0, 720.0, Theme::dark()),
            (800.0, 600.0, Theme::dark()),
            (1280.0, 720.0, Theme::dark().with_text_scale(1.3)),
            (1280.0, 720.0, Theme::high_contrast().with_ui_scale(1.25)),
        ]
        .into_boxed_slice()
        {
            for &(keys, action_ids, error_state) in scenarios {
                let mut view = OnboardingView::with_minter_for_host_desktop(Box::new(|| {
                    Ok(MintedIdentity {
                        fingerprint: "ab".repeat(32),
                        path: PathBuf::from("/identity/primary.json"),
                    })
                }));
                let mut document = AutoCommit::new();
                for key in keys {
                    press(&view, &mut document, key);
                    view.sync(&document, &automerge::ROOT);
                }
                if error_state {
                    view.error =
                        Some("A deterministic error for responsive layout coverage".into());
                }

                let mut text = ui::text_render::TextRenderer::new();
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let node = view.build_layout(&mut taffy, &theme, &mut node_map);
                let measure_map = node_map.clone();
                taffy
                    .compute_layout_with_measure(
                        node,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(width),
                            height: taffy::AvailableSpace::Definite(height),
                        },
                        |known, available, node_id, _, _| {
                            measure_map
                                .get(&node_id)
                                .map_or(taffy::Size::ZERO, |&pointer| unsafe {
                                    (*pointer).measure(known, available, &mut text)
                                })
                        },
                    )
                    .expect("host identity layout");
                view.apply_layout(&taffy, node, Point::ZERO);
                let panel = view.composed.as_ref().expect("composition").get_rect();
                assert!(
                    panel.x0 >= f64::from(VIEWPORT_GUTTER) - 0.5
                        && panel.x1 <= f64::from(width - VIEWPORT_GUTTER) + 0.5
                        && panel.y0 >= f64::from(VIEWPORT_GUTTER) - 0.5
                        && panel.y1 <= f64::from(height - VIEWPORT_GUTTER) + 0.5,
                    "setup panel must preserve its viewport gutter at {width}x{height}: {panel:?}"
                );

                let widget_rect = |id: &str| {
                    node_map
                        .values()
                        .find_map(|&pointer| unsafe {
                            ((*pointer).test_id() == Some(id)).then(|| (*pointer).get_rect())
                        })
                        .unwrap_or_else(|| panic!("host identity page exposes {id}"))
                };
                let eyebrow = widget_rect("onboarding.progress.eyebrow");
                let stage = widget_rect("onboarding.progress.stage");
                let progress = node_map
                    .values()
                    .find_map(|&pointer| unsafe {
                        ((*pointer).name() == "ProgressBar").then(|| (*pointer).get_rect())
                    })
                    .expect("identity page progress bar");
                assert!(
                    eyebrow.x0 >= panel.x0 + f64::from(theme.tokens.spacing.s3) - 0.5
                        && stage.x1 <= panel.x1 - f64::from(theme.tokens.spacing.s3) + 0.5,
                    "setup header labels need an inset from both panel edges at {width}x{height}: panel={panel:?}, eyebrow={eyebrow:?}, stage={stage:?}"
                );
                for header in [eyebrow, stage, progress] {
                    assert!(
                        header.y0 >= panel.y0 && header.y1 <= panel.y1,
                        "setup header must never be clipped at {width}x{height}: panel={panel:?}, header={header:?}, keys={keys:?}, error={error_state}"
                    );
                }
                assert!(
                    progress.y0 >= eyebrow.y1.max(stage.y1) + 4.0,
                    "progress rule needs a deliberate gap below both labels: eyebrow={eyebrow:?}, stage={stage:?}, progress={progress:?}"
                );

                let scroll_pointer = node_map
                    .values()
                    .copied()
                    .find(|&pointer| unsafe { (*pointer).name() == "Scrollable" })
                    .expect("identity page scroll viewport");
                let scroll = unsafe {
                    (*scroll_pointer)
                        .as_any_mut()
                        .and_then(|widget| widget.downcast_mut::<Scrollable>())
                        .expect("Scrollable exposes geometry to the regression harness")
                };
                let content_viewport = scroll.content_viewport_rect();
                // The progress header is intentionally sticky above the body
                // viewport; only the scrolling content belongs inside this
                // clip. Geometry above already proves the header remains
                // inside the panel and separated from the progress rule.

                let backup_page = view.flow().screen() == Screen::Done
                    && !view.identity_ready_page
                    // The identity-mint failure variant deliberately replaces
                    // the passphrase form with its recovery action.
                    && node_map.values().any(|&pointer| unsafe {
                        (*pointer).test_id() == Some("onboarding.backup.passphrase")
                    });
                if backup_page {
                    assert!(
                        content_viewport.height() >= panel.height() * 0.45,
                        "backup body must retain a usable share of its panel: panel={panel:?}, viewport={content_viewport:?}"
                    );
                    let heading = widget_rect("onboarding.backup.heading");
                    assert!(
                        heading.y0 >= content_viewport.y0 && heading.y1 <= content_viewport.y1,
                        "backup title must be fully visible in the shared body: heading={heading:?}, viewport={content_viewport:?}"
                    );
                    for control_id in [
                        "onboarding.backup.passphrase",
                        "onboarding.backup.confirmation",
                        "onboarding.backup.button",
                    ] {
                        let control = widget_rect(control_id);
                        let earliest_offset = (control.y1 - content_viewport.y1).max(0.0);
                        let latest_offset = (control.y0 - content_viewport.y0)
                            .min(f64::from(scroll.max_offset_y()));
                        assert!(
                            control.height() <= content_viewport.height()
                                && control.x0 >= content_viewport.x0 - 0.5
                                && control.x1 <= content_viewport.x1 + 0.5
                                && earliest_offset <= latest_offset + 0.5,
                            "backup form control must be wholly reachable inside the same viewport: id={control_id}, rect={control:?}, viewport={content_viewport:?}"
                        );
                    }
                    // Backup acknowledgement and progression are deliberately
                    // persistent footer controls now; they must be visible in
                    // the panel without scrolling, not reachable through the
                    // scroll viewport above them.
                    let footer_rects = [
                        widget_rect("onboarding.backup.skip-acknowledged.row"),
                        widget_rect("onboarding.backup.continue.button"),
                    ];
                    for control in footer_rects {
                        assert!(
                            // Footer uses the same s3 horizontal gutter as
                            // the scrolling content above it.
                            control.x0 >= panel.x0 + f64::from(theme.tokens.spacing.s3) - 0.5
                                && control.x1
                                    <= panel.x1 - f64::from(theme.tokens.spacing.s3) + 0.5
                                && control.y0
                                    >= content_viewport.y1 + f64::from(theme.tokens.spacing.s3)
                                        - 0.5
                                && control.y1 <= panel.y1 - 16.0,
                            "Backup footer control must stay persistently visible and clear of scrolling content at {width}x{height}: control={control:?}, viewport={content_viewport:?}, panel={panel:?}, theme={}",
                            theme.name,
                        );
                    }
                    continue;
                }

                let rects = action_ids
                    .iter()
                    .map(|id| widget_rect(id))
                    .collect::<Vec<_>>();
                for rect in &rects {
                    assert!(rect.area() > 0.0, "action={rect:?}");
                    assert!(
                        rect.x0 >= 0.0
                            && rect.x1 <= f64::from(width)
                            && rect.y0 >= 0.0
                            && rect.y1 <= f64::from(height),
                        "action must remain inside {width}x{height}: {rect:?}"
                    );
                    assert!(
                        panel.x1 - rect.x1 >= 16.0 && panel.y1 - rect.y1 >= 16.0,
                        "sticky action needs a visible right/bottom inset: panel={panel:?}, action={rect:?}"
                    );
                }
                let first_center = rects[0].center().y;
                assert!(
                    rects
                        .iter()
                        .all(|rect| (rect.center().y - first_center).abs() <= 0.5),
                    "footer actions must share a baseline: {rects:?}"
                );
                assert!(
                    scroll.get_rect().y1 + 8.0 <= rects[0].y0,
                    "the sticky footer must reserve layout space below scrolling content at {width}x{height}: scroll={:?}, actions={rects:?}, keys={keys:?}, error={error_state}, theme={}",
                    scroll.get_rect(),
                    theme.name,
                );

                if view.flow().screen() == Screen::Restore {
                    let viewport = content_viewport;
                    let max_offset = f64::from(scroll.max_offset_y());
                    for field_id in [
                        "onboarding.connect.path.field",
                        "onboarding.connect.passphrase.field",
                    ] {
                        let field = widget_rect(field_id);
                        let earliest_offset = (field.y1 - viewport.y1).max(0.0);
                        let latest_offset = (field.y0 - viewport.y0).min(max_offset);
                        assert!(
                            field.height() <= viewport.height()
                                && earliest_offset <= latest_offset + 0.5,
                            "required restore field must be fully reachable by scrolling at {width}x{height}: field={field:?}, viewport={viewport:?}, max_offset={max_offset}, error={error_state}"
                        );
                    }
                }

                if view.flow().screen() == Screen::Done
                    && view.identity_ready_page
                    && !view.codec_page
                {
                    assert_eq!(
                        scroll.max_offset_y(),
                        0.0,
                        "the brief Ready invitation must be wholly visible at {width}x{height}, theme={}",
                        theme.name
                    );
                    let heading = widget_rect("onboarding.done.heading");
                    let invitation = widget_rect("onboarding.done.body");
                    let footer_top = rects
                        .iter()
                        .map(|rect| rect.y0)
                        .fold(f64::INFINITY, f64::min);
                    let gap = footer_top - invitation.y1;
                    let ordered = heading.y1 <= invitation.y0;
                    assert!(
                        heading.area() > 0.0
                            && invitation.area() > 0.0
                            && heading.y0 >= content_viewport.y0
                            && ordered
                            && invitation.y1 <= content_viewport.y1,
                        "Ready title and invitation must be wholly visible, ordered and clear of the footer: heading={heading:?}, invitation={invitation:?}, viewport={content_viewport:?}"
                    );
                    assert!(
                        gap >= f64::from(theme.tokens.spacing.s2) - 0.5
                            && gap
                                <= f64::from(
                                    theme.tokens.spacing.s6 * 2.0 + theme.tokens.spacing.s3
                                ),
                        "Ready actions must follow their invitation without an empty canvas: gap={gap}, invitation={invitation:?}, actions={rects:?}, panel={panel:?}, theme={}",
                        theme.name
                    );
                }

                if view.flow().screen() == Screen::Done && view.identity_ready_page {
                    let guidance = widget_rect("onboarding.done.body");
                    let max_offset = f64::from(scroll.max_offset_y());
                    let earliest_offset = (guidance.y1 - content_viewport.y1).max(0.0);
                    let latest_offset = (guidance.y0 - content_viewport.y0).min(max_offset);
                    assert!(
                        guidance.height() <= content_viewport.height()
                            && earliest_offset <= latest_offset + 0.5,
                        "Ready-page next actions must be wholly reachable rather than clipped behind the footer at {width}x{height}: guidance={guidance:?}, viewport={content_viewport:?}, max_offset={max_offset}, theme={}",
                        theme.name,
                    );
                }
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one walk through every choice screen, its cards, and the flows they open"
    )]
    fn choices_open_separate_flows_from_pinned_focusable_cards() {
        // Phase `each_choice_screen_offers_two_cards_that_stay_in_view`.
        for (keys, cards) in [
            (
                &[][..],
                ["onboarding.existing.card", "onboarding.begin.card"],
            ),
            (
                &[KEY_EXISTING][..],
                [
                    "onboarding.existing.nearby.card",
                    "onboarding.existing.restore.card",
                ],
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
                let mut text = ui::text_render::TextRenderer::new();
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let node = view.build_layout(&mut taffy, &theme, &mut node_map);
                let measure_map = node_map.clone();
                taffy
                    .compute_layout_with_measure(
                        node,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(width),
                            height: taffy::AvailableSpace::Definite(height),
                        },
                        |known, available, node_id, _, _| {
                            measure_map
                                .get(&node_id)
                                .map_or(taffy::Size::ZERO, |&pointer| unsafe {
                                    (*pointer).measure(known, available, &mut text)
                                })
                        },
                    )
                    .expect("choice layout");
                view.apply_layout(&taffy, node, Point::ZERO);
                let panel = view.composed.as_ref().expect("composition").get_rect();
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
                    let node = accessibility_for_id(&view, id).expect("row semantics");
                    assert_eq!(node.role, ui::a11y::Role::ListItem);
                    assert!(
                        node.value.as_deref().is_some_and(|value| !value.is_empty()),
                        "{id} announces its description"
                    );
                }
            }
        }
        // Phase `use_existing_opens_a_choice_and_each_choice_its_own_screen`.
        {
            let (mut view, _) = view();
            let mut document = AutoCommit::new();
            press(&view, &mut document, KEY_EXISTING);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Existing);
            let theme = Theme::dark();
            let composition = view.build_composition(&theme, PANEL_MAX_HEIGHT);
            assert!(
                accessibility_for_id(&*composition, "onboarding.connect.path.field").is_none(),
                "the choice screen is not the restore form"
            );

            press(&view, &mut document, KEY_CHOOSE_RESTORE);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Restore);
            let composition = view.build_composition(&theme, PANEL_MAX_HEIGHT);
            assert!(accessibility_for_id(&*composition, "onboarding.connect.path.field").is_some());
            assert!(
                accessibility_for_id(&*composition, "onboarding.nearby.status").is_none(),
                "restore does not carry the nearby flow"
            );
            press(&view, &mut document, KEY_BACK);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Existing);

            // Without an identity directory the helper cannot start; the
            // nearby screen still opens and reports why, with a retry.
            press(&view, &mut document, KEY_CHOOSE_NEARBY);
            view.sync(&document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Nearby);
            assert_eq!(
                view.error(),
                Some(t("onboarding-nearby-unavailable").as_str()),
                "choosing nearby is the request to pair"
            );
            let composition = view.build_composition(&theme, PANEL_MAX_HEIGHT);
            assert!(
                accessibility_for_id(&*composition, "onboarding.connect.path.field").is_none(),
                "nearby does not carry the restore form"
            );
            assert!(
                accessibility_for_id(&*composition, "onboarding.connect.nearby.button").is_some(),
                "a failed start offers Try again"
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
        // Phase `nearby_shows_cancel_while_pairing_runs`.
        {
            let (mut view, _) = view();
            view.set_visual_state_for_snapshot(OnboardingVisualState::NearbyCode);
            let theme = Theme::dark();
            let composition = view.build_composition(&theme, PANEL_MAX_HEIGHT);
            assert!(
                accessibility_for_id(&*composition, "onboarding.connect.cancel.button").is_some()
            );
            assert!(
                accessibility_for_id(&*composition, "onboarding.connect.nearby.button").is_none()
            );
            assert_eq!(
                accessibility_for_id(&*composition, "onboarding.nearby.code")
                    .and_then(|node| node.name),
                Some("482 913".to_string())
            );
        }
        // Phase `codes_are_grouped_for_comparison`.
        {
            assert_eq!(grouped_code("123456"), "123 456");
            assert_eq!(grouped_code("12 34 56"), "123 456");
            assert_eq!(grouped_code("1234"), "1234");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn pointer_targets_hit_the_acknowledgement_and_continue_actions() {
        // Phase `acknowledgement_label_is_the_checkbox_click_target`.
        {
            let (mut view, _) = view();
            let mut document = AutoCommit::new();
            press(&view, &mut document, KEY_BEGIN);
            view.sync(&document, &automerge::ROOT);

            let theme = Theme::dark();
            let mut text = ui::text_render::TextRenderer::new();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = view.build_layout(&mut taffy, &theme, &mut node_map);
            let measure_map = node_map.clone();
            taffy
                .compute_layout_with_measure(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(800.0),
                        height: taffy::AvailableSpace::Definite(600.0),
                    },
                    |known, available, node_id, _, _| {
                        measure_map
                            .get(&node_id)
                            .map_or(taffy::Size::ZERO, |&pointer| unsafe {
                                (*pointer).measure(known, available, &mut text)
                            })
                    },
                )
                .expect("host acknowledgement layout");
            view.apply_layout(&taffy, node, Point::ZERO);
            let (row_node, row) = node_map
                .iter()
                .find_map(|(&node, &pointer)| unsafe {
                    ((*pointer).test_id() == Some("onboarding.understood.row"))
                        .then(|| (node, (*pointer).get_rect()))
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
                    assert_eq!(
                        focus,
                        Some(row_node),
                        "the label press must focus the acknowledgement checkbox"
                    );
                }
            }
            view.settle(
                &mut ui::widget::Edits::lend(&mut document),
                &automerge::ROOT,
            );
            assert!(read_bool(&document, &automerge::ROOT, KEY_UNDERSTOOD));
        }
        // Phase `continue_without_backup_pointer_target_advances_to_ready_page`.
        {
            let (mut view, _) = view();
            let mut document = AutoCommit::new();
            for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
                press(&view, &mut document, key);
                view.sync(&document, &automerge::ROOT);
            }
            document
                .put(
                    &automerge::ROOT,
                    KEY_BACKUP_SKIP_ACKNOWLEDGED,
                    ScalarValue::Boolean(true),
                )
                .expect("acknowledge backup risk");
            view.sync(&document, &automerge::ROOT);

            let theme = Theme::dark();
            let mut text = ui::text_render::TextRenderer::new();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = view.build_layout(&mut taffy, &theme, &mut node_map);
            let measure_map = node_map.clone();
            taffy
                .compute_layout_with_measure(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(1280.0),
                        height: taffy::AvailableSpace::Definite(800.0),
                    },
                    |known, available, node_id, _, _| {
                        measure_map
                            .get(&node_id)
                            .map_or(taffy::Size::ZERO, |&pointer| unsafe {
                                (*pointer).measure(known, available, &mut text)
                            })
                    },
                )
                .expect("backup continuation layout");
            view.apply_layout(&taffy, node, Point::ZERO);
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn backup_and_recovery_tasks_share_a_reachable_scroll_body_at_review_sizes() {
        // Phase `first_frames_do_not_hide_backup_recovery_or_final_tour_tasks`.
        {
            fn initial_scroll_offset(view: &mut OnboardingView) -> f32 {
                let theme = Theme::dark();
                let mut text = ui::text_render::TextRenderer::new();
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let node = view.build_layout(&mut taffy, &theme, &mut node_map);
                let measure_map = node_map.clone();
                taffy
                    .compute_layout_with_measure(
                        node,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(1280.0),
                            height: taffy::AvailableSpace::Definite(800.0),
                        },
                        |known, available, node_id, _, _| {
                            measure_map
                                .get(&node_id)
                                .map_or(taffy::Size::ZERO, |&pointer| unsafe {
                                    (*pointer).measure(known, available, &mut text)
                                })
                        },
                    )
                    .expect("laptop-height onboarding layout");
                view.apply_layout(&taffy, node, Point::ZERO);
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
        // Phase `backup_recovery_controls_share_a_reachable_body_at_review_sizes`.
        {
            for (width, height, theme) in [
                (1280.0, 800.0, Theme::dark()),
                (960.0, 540.0, Theme::dark().with_text_scale(1.3)),
            ] {
                let (mut view, _) = view();
                view.set_visual_state_for_snapshot(OnboardingVisualState::Backup);
                let mut text = ui::text_render::TextRenderer::new();
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let node = view.build_layout(&mut taffy, &theme, &mut node_map);
                let measure_map = node_map.clone();
                taffy
                    .compute_layout_with_measure(
                        node,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(width),
                            height: taffy::AvailableSpace::Definite(height),
                        },
                        |known, available, node_id, _, _| {
                            measure_map
                                .get(&node_id)
                                .map_or(taffy::Size::ZERO, |&pointer| unsafe {
                                    (*pointer).measure(known, available, &mut text)
                                })
                        },
                    )
                    .expect("backup review-size layout");
                view.apply_layout(&taffy, node, Point::ZERO);
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
                    viewport.height() >= view.composed.as_ref().unwrap().get_rect().height() * 0.45,
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
                // Exercise the real shared scroller's reveal and layout, rather
                // than treating an off-clip widget rectangle as visible.
                unsafe {
                    let scroll = (*scroll_pointer)
                        .as_any_mut()
                        .unwrap()
                        .downcast_mut::<Scrollable>()
                        .unwrap();
                    scroll.scroll_into_view(
                        ui::num::f32_of_f64(save.x0 - viewport.x0),
                        ui::num::f32_of_f64(save.y0 - viewport.y0),
                        ui::num::f32_of_f64(save.width()),
                        ui::num::f32_of_f64(save.height()),
                    );
                }
                view.apply_layout(&taffy, node, Point::ZERO);
                let revealed_save = rect("onboarding.backup.button");
                assert!(
                    revealed_save.y0 >= viewport.y0 - 0.5 && revealed_save.y1 <= viewport.y1 + 0.5,
                    "actual reveal must bring the whole Save target into the same clip: save={revealed_save:?}, viewport={viewport:?}"
                );
            }
        }
        // Phase `verified_backup_status_and_completion_are_visible_at_review_sizes`.
        {
            for (width, height) in [(1280.0, 800.0), (800.0, 600.0)] {
                let (mut view, _) = view();
                view.set_visual_state_for_snapshot(OnboardingVisualState::BackupVerified);
                let theme = Theme::dark();
                let mut text = ui::text_render::TextRenderer::new();
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let node = view.build_layout(&mut taffy, &theme, &mut node_map);
                let measure_map = node_map.clone();
                taffy
                    .compute_layout_with_measure(
                        node,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(width),
                            height: taffy::AvailableSpace::Definite(height),
                        },
                        |known, available, node_id, _, _| {
                            measure_map
                                .get(&node_id)
                                .map_or(taffy::Size::ZERO, |&pointer| unsafe {
                                    (*pointer).measure(known, available, &mut text)
                                })
                        },
                    )
                    .expect("verified backup review-size layout");
                view.apply_layout(&taffy, node, Point::ZERO);
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn large_text_keeps_every_primary_action_visible() {
        // Phase `done_exit_action_stays_visible_at_laptop_height_with_large_text`.
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
            sync_journaled(&mut view, &doc);

            let theme = Theme::dark().with_text_scale(1.3);
            let mut text = ui::text_render::TextRenderer::new();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = view.build_layout(&mut taffy, &theme, &mut node_map);
            let measure_map = node_map.clone();
            taffy
                .compute_layout_with_measure(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(1000.0),
                        height: taffy::AvailableSpace::Definite(720.0),
                    },
                    |known, available, node_id, _, _| {
                        measure_map
                            .get(&node_id)
                            .map_or(taffy::Size::ZERO, |&ptr| unsafe {
                                (*ptr).measure(known, available, &mut text)
                            })
                    },
                )
                .expect("large-text done layout");
            view.apply_layout(&taffy, node, Point::ZERO);

            // The ready hand-off owns only the height its content needs
            // (61034652be); at large text it must still fit the viewport
            // and hold its actions whole rather than clip them.
            let panel = view.composed.as_ref().expect("composition").get_rect();
            assert!(
                panel.y0 >= 0.0 && panel.y1 <= 720.0 && panel.height() > 0.0,
                "large-text ready screen must fit the laptop-height viewport: {panel:?}",
            );

            for id in ["onboarding.finish.button", "onboarding.tour.start.button"] {
                let action = node_map
                    .values()
                    .find_map(|&ptr| unsafe {
                        ((*ptr).test_id() == Some(id)).then(|| (*ptr).get_rect())
                    })
                    .unwrap_or_else(|| panic!("Done screen exposes {id}"));
                assert!(action.area() > 0.0, "action={action:?}");
                assert!(
                    action.x0 >= 0.0
                        && action.x1 <= 1000.0
                        && action.y0 >= 0.0
                        && action.y1 <= 720.0,
                    "the action must stay fully in the viewport, id={id}, action={action:?}"
                );
                assert!(
                    action.y0 >= panel.y0 && action.y1 <= panel.y1,
                    "the panel must hold the action unclipped, id={id}, action={action:?}, panel={panel:?}"
                );
            }
        }
        // Phase `large_text_backup_action_keeps_a_clear_gap_below_confirmation`.
        {
            let (mut view, _) = view();
            let mut document = AutoCommit::new();
            for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
                press(&view, &mut document, key);
                view.sync(&document, &automerge::ROOT);
            }
            assert_eq!(view.flow().screen(), Screen::Done);

            let theme = Theme::dark().with_text_scale(1.3);
            let mut text = ui::text_render::TextRenderer::new();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = view.build_layout(&mut taffy, &theme, &mut node_map);
            let measure_map = node_map.clone();
            taffy
                .compute_layout_with_measure(
                    node,
                    taffy::Size {
                        width: taffy::AvailableSpace::Definite(1280.0),
                        height: taffy::AvailableSpace::Definite(720.0),
                    },
                    |known, available, node_id, _, _| {
                        measure_map
                            .get(&node_id)
                            .map_or(taffy::Size::ZERO, |&ptr| unsafe {
                                (*ptr).measure(known, available, &mut text)
                            })
                    },
                )
                .expect("large-text backup layout");
            view.apply_layout(&taffy, node, Point::ZERO);
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
        // Phase `tour_navigation_stays_visible_on_every_page_at_narrow_large_text_size`.
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
                let mut text = ui::text_render::TextRenderer::new();
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let node = view.build_layout(&mut taffy, &theme, &mut node_map);
                let measure_map = node_map.clone();
                taffy
                    .compute_layout_with_measure(
                        node,
                        taffy::Size {
                            width: taffy::AvailableSpace::Definite(WIDTH),
                            height: taffy::AvailableSpace::Definite(HEIGHT),
                        },
                        |known, available, node_id, _, _| {
                            measure_map
                                .get(&node_id)
                                .map_or(taffy::Size::ZERO, |&ptr| unsafe {
                                    (*ptr).measure(known, available, &mut text)
                                })
                        },
                    )
                    .expect("narrow large-text tour layout");
                view.apply_layout(&taffy, node, Point::ZERO);

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
