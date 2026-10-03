//! The rendered first-login identity ceremony.
//!
//! Chrome, not an app: the compositor draws this before the desktop
//! exists, since the screen that creates an identity cannot be something
//! an application could imitate.
//!
//! The decisions all live in [`crate::onboarding::Onboarding`]; this
//! file is only the composition, built from shared `ui` widgets (`Card`,
//! `Banner`, `Text`, `Button`, `Checkbox`) so the ceremony inherits the
//! system theme, focus order and accessibility like the rest of the desktop.
//!
//! On a Tilo system that has not opted in yet, setup also offers the media
//! codecs pack after the backup step ([`CodecSetup`]). It is an explicit
//! opt-in (a button, nothing pre-selected), runs through the same polkit-
//! allowed unit as Settings, and never blocks setup: the download continues in
//! the background and the result arrives as a notification.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Read as _;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, Value, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use ui::color_compat::ColorCompat;
use ui::theme::Theme;
use ui::widget::{
    Banner, Button, Card, Checkbox, Column, FilePicker, FilePickerMode, ProgressBar, Row,
    Scrollable, Spacer, Text, TextInput, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
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
use ui::widget::ButtonRole;

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
/// The introduction gets a lower cap than the recovery and completion stages;
/// it still shrinks to its contents.
const INTRO_PANEL_HEIGHT: f32 = 500.0;
/// Completion is a hand-off surface, not a compact alert. Give its title,
/// confirmation, next-step explanation, and backup guidance a stable canvas
/// on ordinary laptop displays; the inner viewport still scrolls when large
/// text or a short window cannot fit that content.
const READY_PANEL_PREFERRED_HEIGHT: f32 = 552.0;
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
/// The recovery form is the primary task on Backup. Reserve its explanation,
/// two labelled 40 px fields, action row, and compact gaps outside the
/// explanatory scroller so a short display never lets the pinned form clip
/// the created-identity banner above it.
const BACKUP_FORM_HEIGHT: f32 = 264.0;
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
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
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
    Connect,
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
pub const KEY_CONNECT: &str = "onboarding.connect";
pub const KEY_BACK: &str = "onboarding.back";
pub const KEY_CONNECT_PATH: &str = "onboarding.connect.path";
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

#[must_use]
pub fn codecs_title() -> String {
    t("onboarding-codecs-title")
}

#[must_use]
pub fn codecs_body() -> String {
    t("onboarding-codecs-body")
}

/// What the pack enables, in the owner's terms.
#[must_use]
pub fn codecs_enables() -> Vec<String> {
    vec![
        t("onboarding-codecs-enables-hardware"),
        t("onboarding-codecs-enables-playback"),
        t("onboarding-codecs-enables-encoding"),
    ]
}

/// Where it comes from, and what Tilo does not do.
#[must_use]
pub fn codecs_source() -> String {
    t("onboarding-codecs-source")
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
type Importer = Box<dyn Fn(&PathBuf, &[u8]) -> Result<MintedIdentity, String> + Send>;
type Exporter = Box<dyn Fn(&PathBuf, &PathBuf, &[u8]) -> Result<(), String> + Send>;

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

#[allow(
    clippy::struct_excessive_bools,
    reason = "recovery mode, backup verification/acknowledgement, modal entry points and presentation preferences are separate state flags"
)]
pub struct OnboardingView {
    flow: Onboarding,
    context: OnboardingContext,
    /// Existing metadata is authoritative when its keyring item is
    /// unavailable or its collaboration chain cannot be verified. Recovery
    /// mode starts on restore and never exposes the create-new branch, while
    /// still allowing a keyring unlock retry.
    recovery_only: bool,
    minter: Minter,
    importer: Importer,
    exporter: Exporter,
    /// Set when minting failed, shown in place of the success screen — loud,
    /// since booting on into a desktop that silently has no identity is what this flow replaces.
    error: Option<String>,
    backup_error: Option<String>,
    backup_saved: Option<PathBuf>,
    backup_was_verified: bool,
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
    /// Settings can reopen the proven backup ceremony for an existing
    /// identity. That is a short modal task, not a second first-login flow.
    portable_backup_only: bool,
    /// A persistence error is distinct from identity creation failure: the
    /// current screen remains valid, but advancing is blocked until crash
    /// recovery state can be written or removed safely.
    progress_error: Option<String>,
    tour_page: Option<TourPage>,
    /// A tour reopened from the desktop has no identity ceremony behind it.
    /// Back on its first page and Done both dismiss the modal directly.
    tour_only: bool,
    /// Raised when the user has finished and the desktop may start.
    finished: bool,
    invalidation: Invalidation,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
    transition_started: Instant,
    transition_duration: Duration,
    transition_direction: f64,
    reduce_motion: bool,
    picker: Option<(PickerPurpose, FilePicker)>,
    connect_source_selected: bool,
    connect_passphrase: Arc<Mutex<String>>,
    backup_passphrase: Arc<Mutex<String>>,
    backup_confirmation: Arc<Mutex<String>>,
    identity_dir: Option<PathBuf>,
    nearby_child: Option<Child>,
    nearby_status: Option<String>,
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
            Box::new(move |source, passphrase| {
                let store = crate::identity_registry::IdentityRegistryStore::at(&import_dir);
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
                runtime_identity::set_active_identity(&import_dir, runtime_id)
                    .map_err(|error| error.to_string())?;
                // Restoring on a fresh machine must provision the exact same
                // complete root -> device chain as minting. The narrower
                // identity-keyed certificate helper leaves the legacy system
                // owner/certificate absent until a later login repair.
                crate::firstboot::ensure_collaboration_identity(
                    &import_dir,
                    &crate::firstboot::collaboration_identity_paths(),
                )
                .map_err(|error| error.to_string())?;
                crate::device_fleet::DeviceFleetStore::at(
                    &import_dir,
                    crate::firstboot::collaboration_identity_paths(),
                )
                .register_current_device(&profile.id)
                .map_err(|error| error.to_string())?;
                Ok(MintedIdentity {
                    fingerprint: profile.id,
                    path: profile.record_path,
                })
            }),
            Box::new(move |identity, destination, passphrase| {
                runtime_identity::export_portable_backup(identity, destination, passphrase)
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }),
            context,
        );
        view.nearby_status_path = Some(identity_dir.join(".nearby-enrollment-status.json"));
        view.identity_dir = Some(identity_dir);
        view.recovery_only = recovery_only;
        if context == OnboardingContext::FullSystem
            && !recovery_only
            && let Some(setup) = CodecSetup::system()
        {
            view = view.with_codec_setup(setup);
        }
        if recovery_only && view.flow.screen() == Screen::Intro {
            view.flow.begin_connect();
        }
        view.restore_pending_progress_from_disk();
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
            context,
            recovery_only: false,
            minter,
            importer,
            exporter,
            error: None,
            backup_error: None,
            backup_saved: None,
            backup_was_verified: false,
            backup_copy_form_open: false,
            backup_skip_acknowledged: false,
            identity_ready_page: false,
            portable_backup_only: false,
            progress_error: None,
            tour_page: None,
            tour_only: false,
            finished: false,
            invalidation: Invalidation::Focus,
            composed: None,
            rect: Rect::ZERO,
            transition_started: Instant::now(),
            transition_duration: ui::tokens::motion::Motion::default().durations.slow(),
            transition_direction: 1.0,
            reduce_motion: false,
            picker: None,
            connect_source_selected: false,
            connect_passphrase: Arc::new(Mutex::new(String::new())),
            backup_passphrase: Arc::new(Mutex::new(String::new())),
            backup_confirmation: Arc::new(Mutex::new(String::new())),
            identity_dir: None,
            nearby_child: None,
            nearby_status: None,
            nearby_status_path: None,
            codec_setup: None,
            codec_step_offered: false,
            codec_page: false,
            codec_status: CodecPackStatus::default(),
            codec_running: None,
            codec_error: None,
            codec_changed: Arc::new(AtomicBool::new(false)),
            codec_watching: false,
            synced_at: Instant::now(),
        }
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
        let mut lines = vec![codecs_title(), codecs_body()];
        lines.extend(codecs_enables());
        lines.push(codecs_source());
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
        lines.extend(
            self.codec_page_buttons()
                .into_iter()
                .map(|(_, label, _, _)| label),
        );
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

    /// (id, label, key, primary) of the page's footer actions.
    fn codec_page_buttons(&self) -> Vec<(&'static str, String, &'static str, bool)> {
        let failed = self.codec_error.is_some() || self.codec_status.state == PackState::Failed;
        if failed && self.codec_running.is_none() {
            return vec![
                (
                    "onboarding.codecs.skip.button",
                    t("onboarding-codecs-skip"),
                    KEY_CODECS_SKIP,
                    false,
                ),
                (
                    "onboarding.codecs.install.button",
                    t("onboarding-codecs-retry"),
                    KEY_CODECS_INSTALL,
                    true,
                ),
            ];
        }
        if self.codec_install_requested() {
            return vec![(
                "onboarding.codecs.continue.button",
                t("onboarding-continue"),
                KEY_CODECS_CONTINUE,
                true,
            )];
        }
        vec![
            (
                "onboarding.codecs.skip.button",
                t("onboarding-codecs-not-now"),
                KEY_CODECS_SKIP,
                false,
            ),
            (
                "onboarding.codecs.install.button",
                t("onboarding-codecs-install"),
                KEY_CODECS_INSTALL,
                true,
            ),
        ]
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
                    self.codec_error = result.err();
                    self.codec_running = None;
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
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
                Screen::Connect => true,
                Screen::Done => !self.identity_ready_page,
                Screen::Intro | Screen::NoRecovery => false,
            };
        (shows_secret && self.rect.area() > 0.0).then_some(self.rect)
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
        self.portable_backup_only = true;
        self.progress_error = None;
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

    fn finish_persisted_progress(&mut self) -> bool {
        let Some(identity_dir) = self.identity_dir.as_ref() else {
            return true;
        };
        match clear_progress(identity_dir) {
            Ok(()) => {
                self.progress_error = None;
                true
            }
            Err(error) => {
                self.progress_error = Some(t_args(
                    "onboarding-progress-clear-failed",
                    &[("error", error.to_string().into())],
                ));
                self.invalidation.structure();
                false
            }
        }
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
        let now = Instant::now();
        self.transition_started = now.checked_sub(Duration::from_secs(1)).unwrap_or(now);
    }

    #[doc(hidden)]
    pub fn set_visual_state_for_snapshot(&mut self, state: OnboardingVisualState) {
        self.flow = Onboarding::new();
        self.error = None;
        self.backup_error = None;
        self.backup_saved = None;
        self.backup_was_verified = false;
        self.backup_copy_form_open = false;
        self.identity_ready_page = false;
        self.portable_backup_only = false;
        self.tour_page = None;
        self.tour_only = false;
        self.codec_page = false;
        self.codec_error = None;
        let minted = MintedIdentity {
            fingerprint: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            path: PathBuf::from("/var/lib/tilo/identities/primary.json"),
        };
        match state {
            OnboardingVisualState::Intro => {}
            OnboardingVisualState::Connect | OnboardingVisualState::RestoreError => {
                self.flow.begin_connect();
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
                    PathBuf::from("/home/user/Documents/tilo-identity-backup.tilo-identity")
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
                        r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":7,"label":"Downloading codecs"}}"#
                    }
                    OnboardingVisualState::CodecsWaitingForNetwork => {
                        r#"{"state":"waiting-for-network","desired":true}"#
                    }
                    OnboardingVisualState::CodecsInstalled => {
                        r#"{"state":"installed","mesa":"enabled","desired":true}"#
                    }
                    OnboardingVisualState::CodecsFailed => {
                        r#"{"state":"failed","desired":true,"last_error":"rpmfusion-free: could not download repository metadata (connection reset)."}"#
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
        if let Some(page) = self.tour_page {
            return self.build_tour_composition(theme, panel_height, page);
        }
        let compact_ready = self.flow.screen() == Screen::Done
            && self.identity_ready_page
            && theme.text_scale <= 1.0
            && !theme.name.eq_ignore_ascii_case("high contrast");
        let total = self.total_steps();
        let (step, stage) = match self.flow.screen() {
            Screen::Intro => (1, t("onboarding-stage-choose")),
            Screen::Connect => (2, t("onboarding-stage-restore")),
            Screen::NoRecovery => (2, t("onboarding-stage-protect")),
            Screen::Done if self.identity_ready_page && self.codec_page => {
                (4, t("onboarding-stage-media"))
            }
            Screen::Done if self.identity_ready_page => (total, t("onboarding-stage-ready")),
            Screen::Done => (3, t("onboarding-stage-backup")),
        };
        let body: Box<dyn Widget> = match self.flow.screen() {
            Screen::Intro => self.intro(theme),
            Screen::Connect => self.connect(theme),
            Screen::NoRecovery => self.no_recovery(theme),
            Screen::Done if self.identity_ready_page && self.codec_page => self.codecs(theme),
            Screen::Done if self.identity_ready_page => self.done(theme, compact_ready),
            Screen::Done => self.backup(theme),
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
                            &[
                                ("step", step.into()),
                                ("total", total.into()),
                                ("stage", stage.clone().into()),
                            ],
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
        // Every identity page keeps its primary progression choice outside
        // the scroll region. Backup used to be the exception; on 960×540 that
        // hid the acknowledgement/Continue action below a subtle scrollbar,
        // making a fresh login look stuck. Reserve the same persistent footer
        // contract here and keep only explanatory/form content scrollable.
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
        let backup_form_visible =
            backup_in_scroll && (!self.backup_was_verified || self.backup_copy_form_open);
        let backup_acknowledgement_visible =
            backup_in_scroll && !self.portable_backup_only && !self.backup_was_verified;
        let footer_height = if backup_in_scroll {
            FOOTER_HEIGHT
                + if backup_form_visible {
                    BACKUP_FORM_HEIGHT
                } else {
                    0.0
                }
                + if backup_acknowledgement_visible {
                    ACKNOWLEDGEMENT_FOOTER_EXTRA_HEIGHT
                } else {
                    0.0
                }
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
        let fill_panel = !backup_in_scroll
            && (theme.text_scale > 1.0
                || theme.name.eq_ignore_ascii_case("high contrast")
                || matches!(self.flow.screen(), Screen::Connect | Screen::NoRecovery)
                || self.flow.screen() == Screen::Done);
        let mut scroll = Scrollable::new_vertical();
        if backup_in_scroll {
            // Backup is a form: Tab should enter its first field/control, and
            // focus-follow scrolling reveals later controls. Explanatory
            // pages retain the viewport stop for Page/Home/End navigation.
            scroll = scroll.without_sequential_focus();
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
            let composition = if backup_form_visible {
                composition.push(self.backup_form(theme))
            } else {
                composition
            };
            Box::new(composition.push(self.backup_decision_footer(theme)))
        } else {
            Box::new(composition.push(self.identity_footer(theme)))
        }
    }

    /// Keep the real recovery operation above the skip acknowledgement and
    /// outside Backup's explanatory scroller so it is present on frame one.
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
        let action = Button::new(
            "onboarding.backup.button",
            &if self.backup_was_verified {
                t("onboarding-backup-save-another")
            } else {
                t("onboarding-backup-save")
            },
            Some(KEY_BACKUP_BROWSE),
        )
        .role(if self.backup_was_verified {
            // Continue remains the clear completion action after a verified
            // backup; making another copy is an optional follow-up task.
            ButtonRole::Secondary
        } else {
            ButtonRole::Primary
        });
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_horizontal_padding(theme.tokens.spacing.s3, theme.tokens.spacing.s3)
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
                    .with_label(t("onboarding-backup-skip-acknowledgement")),
                ))
                .push(self.identity_actions()),
        )
    }

    fn identity_footer(&self, theme: &Theme) -> Box<dyn Widget> {
        let acknowledgement = match self.flow.screen() {
            Screen::NoRecovery => Some(
                Checkbox::new("onboarding.understood.box", KEY_UNDERSTOOD)
                    .with_id("onboarding.understood.row")
                    .with_label(acknowledgement()),
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
        match self.flow.screen() {
            Screen::Intro => footer_actions(vec![
                Box::new(
                    Button::new(
                        "onboarding.connect.button",
                        &t("onboarding-use-existing"),
                        Some(KEY_CONNECT),
                    )
                    .with_icon(ui::icons::Icon::ExternalLink),
                ),
                Box::new(
                    Button::new(
                        "onboarding.begin.button",
                        &t("onboarding-create-identity"),
                        Some(KEY_BEGIN),
                    )
                    .with_icon(ui::icons::Icon::ArrowRight)
                    .role(ButtonRole::Primary),
                ),
            ]),
            Screen::Connect => {
                let mut actions = Vec::new();
                if !self.recovery_only {
                    actions.push(back_button());
                }
                actions.push(Box::new(
                    Button::new(
                        "onboarding.import.button",
                        &t("onboarding-restore-backup"),
                        Some(KEY_IMPORT),
                    )
                    .with_icon(ui::icons::Icon::ArrowRight)
                    .role(ButtonRole::Primary)
                    .with_enabled(self.connect_source_selected),
                ));
                footer_actions(actions)
            }
            Screen::NoRecovery => footer_actions(vec![
                back_button(),
                Box::new(
                    Button::new(
                        "onboarding.create.button",
                        &t("onboarding-create-local-identity"),
                        Some(KEY_CREATE),
                    )
                    .with_icon(ui::icons::Icon::Shield)
                    .role(ButtonRole::Primary)
                    .with_enabled(self.flow.can_create()),
                ),
            ]),
            Screen::Done if self.identity_ready_page && self.codec_page => self.codec_actions(),
            Screen::Done if self.identity_ready_page => Self::finish_action(),
            Screen::Done if self.portable_backup_only => footer_actions(vec![
                Box::new(Button::new(
                    "onboarding.backup.cancel.button",
                    &t("onboarding-cancel"),
                    Some(KEY_FINISH),
                )),
                Box::new(
                    Button::new(
                        "onboarding.backup.done.button",
                        &t("onboarding-done"),
                        Some(KEY_BACKUP_CONTINUE),
                    )
                    .role(ButtonRole::Primary)
                    .with_enabled(self.backup_was_verified),
                ),
            ]),
            Screen::Done if self.backup_was_verified && self.backup_copy_form_open => {
                footer_actions(vec![Box::new(
                    Button::new(
                        "onboarding.backup.continue.button",
                        &t("onboarding-continue"),
                        Some(KEY_BACKUP_CONTINUE),
                    )
                    .role(ButtonRole::Primary),
                )])
            }
            Screen::Done if self.backup_was_verified => footer_actions(vec![
                Box::new(Button::new(
                    "onboarding.backup.another.button",
                    &t("onboarding-backup-save-another"),
                    Some(KEY_BACKUP_ANOTHER),
                )),
                Box::new(
                    Button::new(
                        "onboarding.backup.continue.button",
                        &t("onboarding-continue"),
                        Some(KEY_BACKUP_CONTINUE),
                    )
                    .role(ButtonRole::Primary),
                ),
            ]),
            Screen::Done => footer_actions(vec![Box::new(
                Button::new(
                    "onboarding.backup.continue.button",
                    &t("onboarding-backup-continue-without"),
                    Some(KEY_BACKUP_CONTINUE),
                )
                .role(ButtonRole::Secondary)
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
                        Text::new(&t_args(
                            "onboarding-tour-stage",
                            &[("stage", stage.clone().into()), ("step", number.into())],
                        ))
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
                .push(Self::tour_actions(page)),
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
            for (card_title, copy) in cards {
                checklist = checklist.push(Box::new(
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
                ));
            }
            // The numbered labels already provide the grouping and order. A
            // second titled/padded card spent nearly a full row of a laptop
            // viewport on decoration and pushed the last action below the
            // sticky footer.
            body = body.push(Box::new(checklist));
        } else {
            for (card_title, copy) in cards {
                body = body.push(Box::new(Card::titled(
                    t(card_title),
                    paragraph(&t(copy), theme),
                )));
            }
        }
        Box::new(body)
    }

    /// Sticky tour navigation, kept outside the scrollable lesson body.
    fn tour_actions(page: TourPage) -> Box<dyn Widget> {
        let (primary_label, primary_key) = if page == TourPage::Start {
            (t("onboarding-tour-enter"), KEY_FINISH)
        } else {
            (t("onboarding-continue"), KEY_TOUR_NEXT)
        };
        footer_actions(vec![
            Box::new(Button::new(
                "onboarding.tour.skip.button",
                &t("onboarding-tour-skip"),
                Some(KEY_TOUR_SKIP),
            )),
            Box::new(Button::new(
                "onboarding.tour.back.button",
                &t("onboarding-back"),
                Some(KEY_TOUR_BACK),
            )),
            Box::new(
                Button::new(
                    "onboarding.tour.next.button",
                    &primary_label,
                    Some(primary_key),
                )
                .role(ButtonRole::Primary),
            ),
        ])
    }

    fn intro(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let mut list = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s3);
        for point in intro_points() {
            list = list.push(bullet(&point, theme));
        }
        let (title, body) = match self.context {
            OnboardingContext::FullSystem => (intro_title(), intro_body()),
            OnboardingContext::HostDesktop => {
                (host_desktop_intro_title(), host_desktop_intro_body())
            }
        };
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(spacing.s4)
                .push(heading(&title, theme))
                .push(paragraph(&body, theme))
                .push(Box::new(Card::titled(
                    t("onboarding-intro-card-title"),
                    Box::new(list),
                ))),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the connect page keeps recovery warnings, nearby-device status/actions and encrypted-backup inputs in one composition"
    )]
    fn connect(&self, theme: &Theme) -> Box<dyn Widget> {
        let compact = self.rect.height() <= 540.0;
        let path = TextInput::new(Vec::new())
            .with_id("onboarding.connect.path.field")
            .with_on_change_path(KEY_CONNECT_PATH)
            .with_accessible_name(t("onboarding-connect-file"))
            .with_placeholder(&t("onboarding-connect-file-placeholder"));
        let passphrase = secret_input(
            "onboarding.connect.passphrase.field",
            &self.connect_passphrase,
            &t("onboarding-connect-passphrase"),
            &t("onboarding-connect-passphrase-placeholder"),
        );
        let spacing = &theme.tokens.spacing;
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(if compact { spacing.s3 } else { spacing.s4 })
            .push(heading(&t("onboarding-connect-title"), theme));
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
        let nearby_action = if self.nearby_child.is_some() {
            Button::new(
                "onboarding.connect.cancel.button",
                &t("onboarding-cancel"),
                Some(KEY_CONNECT_CANCEL),
            )
        } else {
            Button::new(
                "onboarding.connect.nearby.button",
                &t("onboarding-connect-nearby"),
                Some(KEY_CONNECT_NEARBY),
            )
        };
        let mut nearby = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(if compact { spacing.s2 } else { spacing.s3 })
            .push(paragraph(
                &t("onboarding-connect-nearby-instructions"),
                theme,
            ));
        if let Some(status) = &self.nearby_status {
            nearby = nearby.push(Box::new(
                Text::new(status)
                    // A stable semantic address lets the accessibility host
                    // recognise verification-code and completion updates as
                    // changes to one status region across composition rebuilds.
                    .with_id("onboarding.nearby.status")
                    .with_role(ui::tokens::typography::TextRole::Body)
                    .with_color(theme.text_dim)
                    .with_live(ui::a11y::Politeness::Polite),
            ));
        }
        nearby = nearby.push(Box::new(nearby_action));
        if self.recovery_only {
            nearby = nearby.push(Box::new(Button::new(
                "onboarding.connect.retry.button",
                &t("onboarding-connect-retry-keyring"),
                Some(KEY_CONNECT_RETRY),
            )));
        }
        let file_row = Row::new().push(Box::new(path)).push(Box::new(
            Button::new(
                "onboarding.connect.browse.button",
                &t("onboarding-connect-browse"),
                Some(KEY_CONNECT_BROWSE),
            )
            .with_icon(ui::icons::Icon::Folder),
        ));
        let backup = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(if compact { spacing.s2 } else { spacing.s3 })
            .push(Box::new(file_row))
            .push(Box::new(
                Text::new(&t("onboarding-connect-passphrase"))
                    .with_role(ui::tokens::typography::TextRole::Label),
            ))
            .push(Box::new(passphrase));
        Box::new(
            body.push(Box::new(Card::titled(
                t("onboarding-connect-nearby-card"),
                Box::new(nearby),
            )))
            .push(Box::new(Card::titled(
                t("onboarding-connect-backup-card"),
                Box::new(backup),
            ))),
        )
    }

    fn no_recovery(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let mut list = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s4);
        for point in no_recovery_points() {
            list = list.push(bullet(&point, theme));
        }
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
        Box::new(body.push(Box::new(Card::new(Box::new(list)))))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the backup page keeps identity-creation errors and current or restored backup-verification feedback in one composition"
    )]
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
                    .push(actions(vec![back_button()])),
            );
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
        let page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(if compact {
                theme.tokens.spacing.s3
            } else {
                theme.tokens.spacing.s4
            })
            .push(heading(&t("onboarding-backup-title"), theme))
            .push(spaced_banner(
                Banner::success(t("onboarding-backup-identity-created"))
                    .with_icon(ui::icons::Icon::Check),
                theme,
            ))
            .push(Box::new(backup));
        Box::new(page)
    }

    fn done(&self, theme: &Theme, compact: bool) -> Box<dyn Widget> {
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
            .push(Box::new(Card::titled(
                t("onboarding-done-next-title"),
                paragraph(&t("onboarding-done-next-body"), theme),
            )))
            .push(Box::new(Card::titled(
                if self.backup_was_verified {
                    t("onboarding-done-backup-verified")
                } else {
                    t("onboarding-done-backup-recommended")
                },
                paragraph_with_id(
                    &if self.backup_was_verified {
                        t("onboarding-done-backup-verified-body")
                    } else {
                        t("onboarding-done-backup-recommended-body")
                    },
                    theme,
                    "onboarding.done.backup-guidance",
                ),
            )));

        if self.context == OnboardingContext::FullSystem {
            page = page.push(Box::new(Card::titled(
                t("onboarding-done-system-care-title"),
                paragraph(&t("onboarding-done-system-care-body"), theme),
            )));
        }
        Box::new(page)
    }

    /// The media codecs offer: what it enables, where it comes from, and the
    /// live result once the owner chose to install.
    fn codecs(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let mut enables = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s3);
        for point in codecs_enables() {
            enables = enables.push(bullet(&point, theme));
        }
        // Once the owner chose, the live result leads the page, above the
        // explanation, so it is visible without scrolling at every size.
        let mut page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s4)
            .push(heading(&codecs_title(), theme));
        if let Some((title, message)) = self.codec_outcome() {
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
        page = page
            .push(paragraph_with_id(
                &codecs_body(),
                theme,
                "onboarding.codecs.body",
            ))
            .push(Box::new(Card::titled(
                t("onboarding-codecs-enables-title"),
                Box::new(enables),
            )))
            .push(Box::new(Card::titled(
                t("onboarding-codecs-source-title"),
                paragraph_with_id(&codecs_source(), theme, "onboarding.codecs.source"),
            )));
        Box::new(page)
    }

    fn codec_actions(&self) -> Box<dyn Widget> {
        footer_actions(
            self.codec_page_buttons()
                .into_iter()
                .map(|(id, label, key, primary)| {
                    let button = Button::new(id, &label, Some(key));
                    let button = if primary {
                        button
                            .with_icon(ui::icons::Icon::ArrowRight)
                            .role(ButtonRole::Primary)
                    } else {
                        button
                    };
                    Box::new(button) as Box<dyn Widget>
                })
                .collect(),
        )
    }

    /// Sticky final action, deliberately outside the Done screen's scrollable
    /// backup content so it remains visible at narrow and large-text sizes.
    fn finish_action() -> Box<dyn Widget> {
        // Continuing to the desktop is the primary hand-off. The optional
        // tour remains explicit rather than making “Skip” the first desktop
        // action a new user has to decipher.
        let tour = Button::new(
            "onboarding.tour.start.button",
            &t("onboarding-tour-take"),
            Some(KEY_TOUR_START),
        );
        let enter = Button::new(
            "onboarding.finish.button",
            &t("onboarding-finish"),
            Some(KEY_FINISH),
        )
        .with_icon(ui::icons::Icon::ArrowRight)
        .role(ButtonRole::Primary);
        footer_actions(vec![Box::new(tour), Box::new(enter)])
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

/// A list item.
///
/// The marker is part of the same `Text` rather than a sibling in a
/// `Row`: these lines wrap, and a marker in its own column detaches from its sentence.
fn bullet(text: &str, _theme: &Theme) -> Box<dyn Widget> {
    Box::new(Text::new(&format!("•   {text}")).with_role(ui::tokens::typography::TextRole::Body))
}

fn back_button() -> Box<dyn Widget> {
    Box::new(
        Button::new(
            "onboarding.back.button",
            &t("onboarding-back"),
            Some(KEY_BACK),
        )
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

fn read_string(state: &AutoCommit, root: &ObjId, key: &str) -> String {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
            ScalarValue::Str(text) => text.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
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

    fn transition_reveal(&mut self, theme: &Theme) -> f32 {
        self.reduce_motion = theme.reduce_motion;
        self.transition_duration = theme.tokens.motion.durations.slow();
        let duration = self.transition_duration;
        let elapsed = self.transition_started.elapsed();
        let raw = if duration.is_zero() {
            1.0
        } else {
            (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the eased value is clamped to 0..=1 before conversion for vello opacity"
        )]
        if theme.reduce_motion {
            1.0
        } else {
            ui::ease_out(f64::from(raw)) as f32
        }
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
        let reveal = self.transition_reveal(theme);
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
            let radius = ui::chrome::ChromeRole::Panel.radius(theme, panel.height());
            ui::chrome::shell::panel_shadow(scene, transform, panel, radius, theme, 1.0);
            let shape = RoundedRect::from_rect(panel, radius);
            scene.fill(
                Fill::NonZero,
                transform,
                ui::chrome::shell::panel_ground(theme),
                None,
                &shape,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                ui::chrome::shell::panel_border(theme),
                None,
                &shape,
            );
        }

        let mut local = Scene::new();
        composed.draw(&mut local, Affine::IDENTITY, text_renderer, theme, presence);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            reveal,
            transform,
            &self.rect,
        );
        scene.append(
            &local,
            Some(
                transform.pre_translate(
                    (
                        self.transition_direction * f64::from(1.0 - reveal) * 22.0,
                        f64::from(1.0 - reveal) * 8.0,
                    )
                        .into(),
                ),
            ),
        );
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

fn terminate_nearby_pairing(child: &mut Option<Child>) {
    let Some(mut child) = child.take() else {
        return;
    };
    let Ok(pid) = i32::try_from(child.id()) else {
        let _ = child.kill();
        let _ = child.wait();
        return;
    };
    let group = pid.saturating_neg();
    // SAFETY: the process is spawned with `process_group(0)`, making its
    // validated PID the process-group ID. A negative target is confined to
    // that group and cannot signal the compositor's own group.
    let _ = unsafe { libc::kill(group, libc::SIGTERM) };
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(750))
        .unwrap_or_else(Instant::now);
    loop {
        let leader_finished = child.try_wait().ok().flatten().is_some();
        // Signal 0 only checks whether any descendant still occupies the
        // dedicated group. Do not return just because pkexec itself exited:
        // tilo-home-peer-control or tilo-enroll may still be winding down.
        let group_gone = unsafe { libc::kill(group, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        if leader_finished && group_gone {
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: same dedicated process group as above. This is the bounded
    // fallback for a broker or enrollment worker that ignored SIGTERM.
    let _ = unsafe { libc::kill(group, libc::SIGKILL) };
    let _ = child.wait();
}

impl Drop for OnboardingView {
    fn drop(&mut self) {
        terminate_nearby_pairing(&mut self.nearby_child);
        clear_secret(&self.connect_passphrase);
        clear_secret(&self.backup_passphrase);
        clear_secret(&self.backup_confirmation);
    }
}

impl Widget for OnboardingView {
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered reducer owns all modal OOBE actions and secret cleanup transitions"
    )]
    fn sync(&mut self, state: &mut AppModel, root: &ModelPath) {
        let (state, root) = ui::doc::document(state, root);
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
        let connect_source_selected = !read_string(state, root, KEY_CONNECT_PATH).trim().is_empty();
        if connect_source_selected != self.connect_source_selected {
            self.connect_source_selected = connect_source_selected;
            self.invalidation.structure();
        }

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
                        let _ = state.put(
                            root,
                            KEY_CONNECT_PATH,
                            ScalarValue::Str(path.to_string_lossy().into_owned().into()),
                        );
                        self.connect_source_selected = true;
                        self.error = None;
                    }
                    PickerPurpose::BackupIdentity => {
                        let passphrase = Zeroizing::new(
                            self.backup_passphrase
                                .lock()
                                .map(|value| value.as_bytes().to_vec())
                                .unwrap_or_default(),
                        );
                        let export = self.flow.minted().map_or_else(
                            || Err("the identity is not ready to back up".to_string()),
                            |identity| (self.exporter)(&identity.path, &path, &passphrase),
                        );
                        match export {
                            Ok(()) => {
                                self.backup_saved = Some(path);
                                self.backup_was_verified = true;
                                self.backup_copy_form_open = false;
                                let _ =
                                    self.persist_progress(PersistedStage::IdentityReadyBackedUp);
                                self.backup_error = None;
                                clear_secret(&self.backup_passphrase);
                                clear_secret(&self.backup_confirmation);
                            }
                            Err(error) => {
                                self.backup_error = Some(error);
                            }
                        }
                    }
                }
            }
            self.picker = None;
            self.invalidation.both();
        }

        if read_bool(state, root, KEY_BEGIN) {
            let _ = state.put(root, KEY_BEGIN, ScalarValue::Boolean(false));
            self.flow.begin();
        }

        if read_bool(state, root, KEY_CONNECT) {
            let _ = state.put(root, KEY_CONNECT, ScalarValue::Boolean(false));
            self.flow.begin_connect();
        }

        if read_bool(state, root, KEY_CONNECT_BROWSE) {
            let _ = state.put(root, KEY_CONNECT_BROWSE, ScalarValue::Boolean(false));
            if self.flow.screen() == Screen::Connect && self.picker.is_none() {
                let (initial, roots) = onboarding_picker_scope();
                self.picker = Some((
                    PickerPurpose::ImportIdentity,
                    FilePicker::new(t("onboarding-picker-import-title"), initial, roots)
                        .with_filters(vec![
                            ui::widget::file_picker::FileFilter::new(
                                t("onboarding-picker-import-filter"),
                                &["tilo-identity", "tilo-identity-backup", "json"],
                            ),
                            ui::widget::file_picker::FileFilter::all(),
                        ]),
                ));
                self.invalidation.focus();
            }
        }

        if read_bool(state, root, KEY_CONNECT_NEARBY) {
            let _ = state.put(root, KEY_CONNECT_NEARBY, ScalarValue::Boolean(false));
            if self.flow.screen() == Screen::Connect
                && self.nearby_child.is_none()
                && self.persist_progress(PersistedStage::AcquiringIdentity)
            {
                match (&self.identity_dir, &self.nearby_status_path) {
                    (Some(identity_dir), Some(status_path)) => {
                        let _ = std::fs::remove_file(status_path);
                        let label =
                            std::env::var("HOSTNAME").unwrap_or_else(|_| "Tilo device".into());
                        let identity_root = std::env::var_os("TILO_IDENTITY_ROOT")
                            .map_or_else(|| PathBuf::from("/var/lib/tilo"), PathBuf::from);
                        let requester =
                            tilo_relay_protocol::ProcessLease::capture(std::process::id());
                        let spawn =
                            requester
                                .map_err(|error| error.to_string())
                                .and_then(|lease| {
                                    Command::new("pkexec")
                                        .args([
                                            "/usr/libexec/tilo-home-peer-control",
                                            "pair-device",
                                            "--identity-root",
                                        ])
                                        .arg(identity_root)
                                        .arg("--identity-dir")
                                        .arg(identity_dir)
                                        .args(["--label", &label, "--status-file"])
                                        .arg(status_path)
                                        .arg("--requester-pid")
                                        .arg(lease.pid().to_string())
                                        .arg("--requester-start-time")
                                        .arg(lease.start_time().to_string())
                                        .stdin(Stdio::null())
                                        .stdout(Stdio::piped())
                                        .stderr(Stdio::piped())
                                        // pkexec, the privileged broker and tilo-enroll must
                                        // share a revocable lifetime.  A dedicated process
                                        // group lets Cancel, Back and compositor teardown
                                        // stop the whole transaction instead of merely
                                        // orphaning a privileged descendant.
                                        .process_group(0)
                                        .spawn()
                                        .map_err(|error| error.to_string())
                                });
                        match spawn {
                            Ok(child) => {
                                self.nearby_child = Some(child);
                                self.nearby_status = Some(t("onboarding-nearby-searching"));
                                self.error = None;
                            }
                            Err(error) => {
                                self.error = Some(t_args(
                                    "onboarding-nearby-start-failed",
                                    &[("error", error.into())],
                                ));
                            }
                        }
                    }
                    _ => self.error = Some(t("onboarding-nearby-unavailable")),
                }
                self.invalidation.structure();
            }
        }

        if read_bool(state, root, KEY_CONNECT_CANCEL) {
            let _ = state.put(root, KEY_CONNECT_CANCEL, ScalarValue::Boolean(false));
            terminate_nearby_pairing(&mut self.nearby_child);
            if let Some(path) = &self.nearby_status_path {
                let _ = std::fs::remove_file(path);
            }
            self.nearby_status = Some(t("onboarding-nearby-cancelled"));
            self.invalidation.structure();
        }

        if let Some(path) = &self.nearby_status_path
            && let Ok(bytes) = std::fs::read(path)
            && let Ok(status) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Some(code) = status
                .get("verification_code")
                .and_then(serde_json::Value::as_str)
        {
            let message = t_args("onboarding-nearby-code", &[("code", code.into())]);
            if self.nearby_status.as_deref() != Some(&message) {
                self.nearby_status = Some(message);
                self.invalidation.structure();
            }
        }
        let finished_nearby_child = match self.nearby_child.take() {
            Some(mut child) => {
                if let Some(status) = child.try_wait().ok().flatten() {
                    Some((child, status))
                } else {
                    self.nearby_child = Some(child);
                    None
                }
            }
            None => None,
        };
        if let Some((mut child, status)) = finished_nearby_child {
            // `try_wait` reaps the leader when it has exited. Keep the
            // explicit wait as a defensive lifecycle guarantee if platform
            // behavior changes, and never leave a completed broker child in
            // the compositor.
            let _ = child.wait();
            let mut stdout = String::new();
            let mut stderr = String::new();
            if let Some(mut stream) = child.stdout.take() {
                let _ = stream.read_to_string(&mut stdout);
            }
            if let Some(mut stream) = child.stderr.take() {
                let _ = stream.read_to_string(&mut stderr);
            }
            if status.success() {
                match serde_json::from_str::<serde_json::Value>(
                    stdout.lines().last().unwrap_or_default(),
                ) {
                    Ok(result) => {
                        let fingerprint = result
                            .get("identity")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let path = result
                            .get("record_path")
                            .and_then(serde_json::Value::as_str)
                            .map(PathBuf::from);
                        if let Some(path) = path.filter(|_| fingerprint.len() == 64) {
                            self.flow
                                .complete_connected(MintedIdentity { fingerprint, path });
                            self.identity_ready_page = false;
                            let _ = self.persist_progress(PersistedStage::IdentityReady);
                            self.error = None;
                            self.nearby_status = Some(t("onboarding-nearby-connected"));
                        } else {
                            self.error = Some(t("onboarding-nearby-invalid-identity"));
                        }
                    }
                    Err(_) => self.error = Some(t("onboarding-nearby-invalid-identity")),
                }
            } else {
                self.error = Some(match stderr.lines().last() {
                    Some(detail) => t_args(
                        "onboarding-nearby-failed-detail",
                        &[("detail", detail.into())],
                    ),
                    None => t("onboarding-nearby-failed"),
                });
            }
            self.invalidation.structure();
        }

        if read_bool(state, root, KEY_BACKUP_ANOTHER) {
            let _ = state.put(root, KEY_BACKUP_ANOTHER, ScalarValue::Boolean(false));
            if self.flow.screen() == Screen::Done
                && !self.identity_ready_page
                && !self.portable_backup_only
                && self.backup_was_verified
            {
                self.backup_copy_form_open = true;
                self.backup_error = None;
                self.invalidation.both();
            }
        }

        if read_bool(state, root, KEY_BACKUP_BROWSE) {
            let _ = state.put(root, KEY_BACKUP_BROWSE, ScalarValue::Boolean(false));
            if self.flow.screen() == Screen::Done
                && !self.identity_ready_page
                && self.picker.is_none()
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
        }

        if read_bool(state, root, KEY_BACKUP_CONTINUE) {
            let _ = state.put(root, KEY_BACKUP_CONTINUE, ScalarValue::Boolean(false));
            if self.portable_backup_only
                && self.flow.is_done()
                && self.backup_was_verified
                && self.finish_persisted_progress()
            {
                self.finished = true;
                clear_secret(&self.backup_passphrase);
                clear_secret(&self.backup_confirmation);
                self.invalidation.both();
            } else if !self.portable_backup_only
                && self.flow.is_done()
                && self.tour_page.is_none()
                && !self.identity_ready_page
                && (self.backup_was_verified || self.backup_skip_acknowledged)
                && self.persist_progress(if self.backup_was_verified {
                    PersistedStage::ReadyToEnterBackedUp
                } else {
                    PersistedStage::ReadyToEnter
                })
            {
                self.identity_ready_page = true;
                // The journal already records the hand-off: a restart during
                // the optional codecs offer resumes at the hand-off, where
                // Settings still offers the codecs.
                self.codec_page = self.codec_step_offered;
                clear_secret(&self.backup_passphrase);
                clear_secret(&self.backup_confirmation);
                self.invalidation.both();
            }
        }

        if read_bool(state, root, KEY_CODECS_INSTALL) {
            let _ = state.put(root, KEY_CODECS_INSTALL, ScalarValue::Boolean(false));
            if self.codec_page {
                self.start_codec_install();
            }
        }

        for key in [KEY_CODECS_SKIP, KEY_CODECS_CONTINUE] {
            if read_bool(state, root, key) {
                let _ = state.put(root, key, ScalarValue::Boolean(false));
                self.codec_page = false;
            }
        }

        if read_bool(state, root, KEY_BACK) {
            let _ = state.put(root, KEY_BACK, ScalarValue::Boolean(false));
            self.error = None;
            clear_secret(&self.connect_passphrase);
            terminate_nearby_pairing(&mut self.nearby_child);
            self.nearby_status = None;
            if !(self.recovery_only && self.flow.screen() == Screen::Connect) {
                self.flow.back();
            }
        }

        if read_bool(state, root, KEY_IMPORT) {
            let _ = state.put(root, KEY_IMPORT, ScalarValue::Boolean(false));
            let source = PathBuf::from(read_string(state, root, KEY_CONNECT_PATH));
            if self.flow.screen() == Screen::Connect && source.as_os_str().is_empty() {
                self.error = Some(t("onboarding-connect-choose-file-first"));
                self.invalidation.structure();
            } else if self.flow.screen() == Screen::Connect
                && self.persist_progress(PersistedStage::AcquiringIdentity)
            {
                let passphrase = Zeroizing::new(
                    self.connect_passphrase
                        .lock()
                        .map(|value| value.as_bytes().to_vec())
                        .unwrap_or_default(),
                );
                let import = (self.importer)(&source, &passphrase);
                match import {
                    Ok(identity) => {
                        self.error = None;
                        self.flow.complete_connected(identity);
                        self.identity_ready_page = false;
                        let _ = self.persist_progress(PersistedStage::IdentityReady);
                        clear_secret(&self.connect_passphrase);
                    }
                    Err(error) => {
                        tracing::error!(target: "tilo::onboarding", %error, "identity import failed");
                        self.error = Some(error);
                        self.invalidation.structure();
                    }
                }
            }
        }

        // A locked keyring may become available after the compositor has
        // presented trusted chrome (for example after the host keyring's
        // unlock prompt completes). Retry the existing record in place; the
        // minter is idempotent and cannot overwrite metadata.
        if read_bool(state, root, KEY_CONNECT_RETRY) {
            let _ = state.put(root, KEY_CONNECT_RETRY, ScalarValue::Boolean(false));
            if self.recovery_only
                && self.flow.screen() == Screen::Connect
                && self.persist_progress(PersistedStage::AcquiringIdentity)
            {
                match (self.minter)() {
                    Ok(identity) => {
                        self.error = None;
                        self.flow.complete_connected(identity);
                        let _ = self.persist_progress(PersistedStage::IdentityReady);
                        self.invalidation.both();
                    }
                    Err(error) => {
                        self.error = Some(t_args(
                            "onboarding-keyring-unavailable",
                            &[("error", error.into())],
                        ));
                        self.invalidation.structure();
                    }
                }
            }
        }

        // The checkbox drives the gate directly rather than being latched, so unticking really does close it again.
        let understood = read_bool(state, root, KEY_UNDERSTOOD);
        if understood != self.flow.acknowledged() {
            self.flow.set_acknowledged(understood);
            self.invalidation.structure();
        }

        let skip_acknowledged = read_bool(state, root, KEY_BACKUP_SKIP_ACKNOWLEDGED);
        if skip_acknowledged != self.backup_skip_acknowledged {
            self.backup_skip_acknowledged = skip_acknowledged;
            self.invalidation.structure();
        }

        if read_bool(state, root, KEY_CREATE) {
            let _ = state.put(root, KEY_CREATE, ScalarValue::Boolean(false));
            // `can_create` is checked inside `complete` too — this is the outer guard, that one is the invariant.
            if self.flow.can_create() && self.persist_progress(PersistedStage::AcquiringIdentity) {
                self.error = None;
                match (self.minter)() {
                    Ok(minted) => {
                        self.flow.complete(minted);
                        self.identity_ready_page = false;
                        let _ = self.persist_progress(PersistedStage::IdentityReady);
                    }
                    Err(e) => {
                        tracing::error!(target: "tilo::onboarding", error = %e, "identity minting failed");
                        self.error = Some(e);
                        // Stay on the protected step. A failed write is not a
                        // completed identity and must never release the desktop.
                        self.invalidation.structure();
                    }
                }
            }
        }

        if read_bool(state, root, KEY_TOUR_START) {
            let _ = state.put(root, KEY_TOUR_START, ScalarValue::Boolean(false));
            if self.flow.is_done()
                && self.identity_ready_page
                && !self.codec_page
                && self.persist_progress(PersistedStage::TourNavigate)
            {
                self.tour_page = Some(TourPage::Navigate);
            }
        }

        if read_bool(state, root, KEY_TOUR_BACK) {
            let _ = state.put(root, KEY_TOUR_BACK, ScalarValue::Boolean(false));
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
            if self.tour_only
                || progress_stage.is_some_and(|progress| self.persist_progress(progress))
            {
                self.tour_page = next;
            }
        }

        if read_bool(state, root, KEY_TOUR_NEXT) {
            let _ = state.put(root, KEY_TOUR_NEXT, ScalarValue::Boolean(false));
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
            if self.tour_only
                || progress_stage.is_none_or(|progress| self.persist_progress(progress))
            {
                self.tour_page = next;
            }
        }

        if read_bool(state, root, KEY_TOUR_SKIP) {
            let _ = state.put(root, KEY_TOUR_SKIP, ScalarValue::Boolean(false));
            // This action exists only while a tour page is visible. During
            // first use, clearing the crash journal is the durable record that
            // the person deliberately skipped; a reopened tour has no setup
            // journal and can close immediately.
            self.finished = self.tour_page.is_some()
                && (self.tour_only || (self.flow.is_done() && self.finish_persisted_progress()));
            if self.finished {
                clear_secret(&self.connect_passphrase);
                clear_secret(&self.backup_passphrase);
                clear_secret(&self.backup_confirmation);
            }
        }

        if read_bool(state, root, KEY_FINISH) {
            let _ = state.put(root, KEY_FINISH, ScalarValue::Boolean(false));
            let at_tour_end = self.tour_page == Some(TourPage::Start);
            let skipping_tour = self.tour_page.is_none()
                && self.flow.is_done()
                && self.identity_ready_page
                && !self.codec_page;
            // A reopened, tour-only flow exposes “Skip tour” on every page.
            // Its action is KEY_FINISH, so it must dismiss from every page,
            // not only from the final hand-off page.
            self.finished = self.portable_backup_only
                || self.tour_only
                || ((at_tour_end || skipping_tour)
                    && self.flow.is_done()
                    && self.finish_persisted_progress());
            if self.finished {
                clear_secret(&self.connect_passphrase);
                clear_secret(&self.backup_passphrase);
                clear_secret(&self.backup_confirmation);
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
            self.transition_started = Instant::now();
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.reduce_motion = theme.reduce_motion;
        self.transition_duration = theme.tokens.motion.durations.slow();
        let accessibility_expanded =
            theme.text_scale > 1.0 || theme.name.eq_ignore_ascii_case("high contrast");
        let panel_height = if accessibility_expanded {
            ACCESSIBLE_PANEL_HEIGHT
        } else if self.tour_page.is_none() && self.flow.screen() == Screen::Intro {
            INTRO_PANEL_HEIGHT
        } else {
            PANEL_MAX_HEIGHT
        };
        let ready_identity_page = self.tour_page.is_none()
            && self.flow.screen() == Screen::Done
            && self.identity_ready_page;
        let dense_identity_page = self.tour_page.is_none()
            && (accessibility_expanded
                || matches!(self.flow.screen(), Screen::Connect | Screen::NoRecovery)
                || (self.flow.screen() == Screen::Done && !self.identity_ready_page));
        // The scroll/footer budget must use the same preferred height as the
        // composition itself. Previously dense normal-text pages measured a
        // 640 px scroll region and were then placed in a 560 px card, putting
        // the sticky action on top of its scrolling sibling.
        let composition_height = if accessibility_expanded {
            ACCESSIBLE_PANEL_HEIGHT
        } else if dense_identity_page {
            DENSE_PANEL_PREFERRED_HEIGHT
        } else if ready_identity_page {
            READY_PANEL_PREFERRED_HEIGHT
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
            cs.size.height = if accessibility_expanded {
                taffy::Dimension::length(ACCESSIBLE_PANEL_HEIGHT)
            } else if dense_identity_page {
                taffy::Dimension::length(DENSE_PANEL_PREFERRED_HEIGHT)
            } else if ready_identity_page {
                taffy::Dimension::length(READY_PANEL_PREFERRED_HEIGHT)
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
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            padding: taffy::Rect {
                left: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                right: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                top: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
                bottom: taffy::LengthPercentage::length(VIEWPORT_GUTTER),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
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
            let _ = ctx
                .state
                .put(ctx.path, KEY_TOUR_SKIP, ScalarValue::Boolean(true));
            *ctx.handled = true;
            return;
        }
        // Escape follows the visible Back action while the identity ceremony
        // is still reversible. This is especially important on Connect,
        // where a nearby-device attempt may be running: keyboard users need a
        // conventional way to leave the branch, and KEY_BACK shares the exact
        // same cleanup path that terminates that process group and clears its
        // transient status. Intro and Done deliberately remain non-dismissible
        // because setup has either not established an identity yet or still
        // needs an explicit hand-off into the desktop.
        if matches!(self.flow.screen(), Screen::Connect | Screen::NoRecovery)
            && matches!(
                event,
                WindowEvent::KeyboardInput { event, .. }
                    if event.state == ElementState::Pressed
                        && matches!(event.logical_key, Key::Named(NamedKey::Escape))
            )
        {
            let _ = ctx
                .state
                .put(ctx.path, KEY_BACK, ScalarValue::Boolean(true));
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
        let polling =
            self.codec_running.is_some() || self.codec_watching || self.nearby_child.is_some();
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
            || (!self.reduce_motion && self.transition_started.elapsed() < self.transition_duration)
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
        Screen::Connect | Screen::NoRecovery => 1,
        Screen::Done => 2,
    }
}

fn view_order(screen: Screen, tour: Option<TourPage>) -> u8 {
    match tour {
        Some(TourPage::Navigate) => 3,
        Some(TourPage::Organise) => 4,
        Some(TourPage::Collaborate) => 5,
        Some(TourPage::Start) => 6,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;
    use ui::widget::identity_flow::{IdentityRecord, SerializableIdentity};
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
        // survive when only the immediate pkexec process was killed.
        std::thread::sleep(Duration::from_millis(40));
        let mut child = Some(child);

        terminate_nearby_pairing(&mut child);

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

    fn press(doc: &mut AutoCommit, key: &str) {
        let _ = doc.put(&automerge::ROOT, key, ScalarValue::Boolean(true));
    }

    fn escape_event() -> WindowEvent {
        use winit::event::KeyEvent;
        use winit::keyboard::{KeyLocation, NativeKeyCode, PhysicalKey};

        // winit keeps the platform field private. These inert placeholders
        // match the toolkit and greeter keyboard-event tests.
        let mut event: KeyEvent = unsafe { std::mem::zeroed() };
        event.logical_key = Key::Named(NamedKey::Escape);
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.location = KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        event.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        }
    }

    fn dispatch_escape(view: &mut OnboardingView, document: &mut AutoCommit) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy
            .new_leaf(taffy::Style::default())
            .expect("event target");
        let mut handled = false;
        let mut focus_target = None;
        view.handle_event(
            &escape_event(),
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: document,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    fn pointer_event(position: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(position.x, position.y),
        }
    }

    fn primary_press_event() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        }
    }

    fn primary_release_event() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: winit::event::MouseButton::Left,
        }
    }

    fn accessibility_for_id(widget: &dyn Widget, id: &str) -> Option<ui::a11y::AccessNode> {
        if widget.test_id() == Some(id) {
            return widget.accessibility();
        }
        widget
            .children()
            .into_iter()
            .find_map(|child| accessibility_for_id(child, id))
    }

    fn write_fixture_identity(directory: &std::path::Path) {
        let record = IdentityRecord {
            display_name: "Test user".into(),
            avatar_color: [20, 120, 220],
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

    fn finish_identity(view: &mut OnboardingView, document: &mut AutoCommit) {
        for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
            press(document, key);
            view.sync(document, &automerge::ROOT);
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
            press(&mut document, KEY_BACKUP_CONTINUE);
            view.sync(&mut document, &automerge::ROOT);
            assert!(
                !view.identity_ready_page,
                "continuing without a backup requires an explicit acknowledgement"
            );
            assert_eq!(
                load_progress(&identity_dir).expect("load progress"),
                Some(PersistedStage::IdentityReady)
            );

            press(&mut document, KEY_BACKUP_SKIP_ACKNOWLEDGED);
            view.sync(&mut document, &automerge::ROOT);
            press(&mut document, KEY_BACKUP_CONTINUE);
            view.sync(&mut document, &automerge::ROOT);
            assert_eq!(
                load_progress(&identity_dir).expect("load progress"),
                Some(PersistedStage::ReadyToEnter)
            );
            press(&mut document, KEY_FINISH);
            view.sync(&mut document, &automerge::ROOT);
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
            press(&mut blocked_finish, KEY_FINISH);
            backup.sync(&mut blocked_finish, &automerge::ROOT);
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
            press(&mut document, KEY_TOUR_START);
            done.sync(&mut document, &automerge::ROOT);
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
            press(&mut document, KEY_TOUR_NEXT);
            navigate_restart.sync(&mut document, &automerge::ROOT);
            assert_eq!(navigate_restart.tour_page, Some(TourPage::Organise));

            let mut organise_restart = OnboardingView::new(directory.path().to_path_buf());
            assert_eq!(organise_restart.tour_page, Some(TourPage::Organise));
            press(&mut document, KEY_TOUR_NEXT);
            organise_restart.sync(&mut document, &automerge::ROOT);
            assert_eq!(organise_restart.tour_page, Some(TourPage::Collaborate));

            let mut collaborate_restart = OnboardingView::new(directory.path().to_path_buf());
            assert_eq!(collaborate_restart.tour_page, Some(TourPage::Collaborate));
            press(&mut document, KEY_TOUR_NEXT);
            collaborate_restart.sync(&mut document, &automerge::ROOT);
            assert_eq!(collaborate_restart.tour_page, Some(TourPage::Start));

            let mut start_restart = OnboardingView::new(directory.path().to_path_buf());
            assert_eq!(start_restart.tour_page, Some(TourPage::Start));
            press(&mut document, KEY_FINISH);
            start_restart.sync(&mut document, &automerge::ROOT);
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
                press(&mut document, key);
                view.sync(&mut document, &automerge::ROOT);
            }

            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(view.flow().screen(), Screen::NoRecovery);
            assert!(view.progress_error.is_some());
        }
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
            press(&mut doc, KEY_BEGIN);
            v.sync(&mut doc, &automerge::ROOT);
            assert_eq!(v.flow().screen(), Screen::NoRecovery);

            press(&mut doc, KEY_CREATE);
            v.sync(&mut doc, &automerge::ROOT);
            assert_eq!(calls.load(Ordering::SeqCst), 0, "no key was minted");
            assert_eq!(v.flow().screen(), Screen::NoRecovery, "still on the gate");
        }
        // Phase `acknowledging_then_creating_mints_exactly_once`.
        {
            let (mut v, calls) = view();
            let mut doc = AutoCommit::new();
            press(&mut doc, KEY_BEGIN);
            v.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_UNDERSTOOD);
            v.sync(&mut doc, &automerge::ROOT);
            assert!(v.flow().can_create());
            press(&mut doc, KEY_CREATE);
            v.sync(&mut doc, &automerge::ROOT);
            // A second sync must not re-fire — the key is consumed.
            v.sync(&mut doc, &automerge::ROOT);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(v.flow().screen(), Screen::Done);
            assert!(v.flow().is_done());
        }
        // Phase `unticking_the_box_closes_the_gate_again`.
        {
            let (mut v, calls) = view();
            let mut doc = AutoCommit::new();
            press(&mut doc, KEY_BEGIN);
            v.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_UNDERSTOOD);
            v.sync(&mut doc, &automerge::ROOT);
            let _ = doc.put(
                &automerge::ROOT,
                KEY_UNDERSTOOD,
                ScalarValue::Boolean(false),
            );
            v.sync(&mut doc, &automerge::ROOT);
            assert!(!v.flow().can_create());
            press(&mut doc, KEY_CREATE);
            v.sync(&mut doc, &automerge::ROOT);
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        // Phase `a_failed_mint_is_surfaced_rather_than_hidden`.
        // A minting failure must be shown, not swallowed into a desktop that silently has no identity.
        {
            let mut v = OnboardingView::with_minter(Box::new(|| Err("disk full".to_string())));
            let mut doc = AutoCommit::new();
            press(&mut doc, KEY_BEGIN);
            v.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_UNDERSTOOD);
            v.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_CREATE);
            v.sync(&mut doc, &automerge::ROOT);
            assert_eq!(v.error(), Some("disk full"));
            assert_eq!(
                v.flow().screen(),
                Screen::NoRecovery,
                "a failed write is not a completed identity"
            );
            assert!(!v.flow().is_done());
            press(&mut doc, KEY_FINISH);
            v.sync(&mut doc, &automerge::ROOT);
            assert!(!v.finished(), "failure must not release the desktop");
        }
        // Phase `finishing_releases_the_desktop`.
        {
            let (mut v, _) = view();
            let mut doc = AutoCommit::new();
            assert!(!v.finished());
            press(&mut doc, KEY_BEGIN);
            v.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_UNDERSTOOD);
            v.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_CREATE);
            v.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_BACKUP_SKIP_ACKNOWLEDGED);
            v.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_BACKUP_CONTINUE);
            v.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_FINISH);
            v.sync(&mut doc, &automerge::ROOT);
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
            press(&mut doc, KEY_BEGIN);
            view.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_UNDERSTOOD);
            view.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_CREATE);
            view.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_BACKUP_SKIP_ACKNOWLEDGED);
            view.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_BACKUP_CONTINUE);
            view.sync(&mut doc, &automerge::ROOT);

            press(&mut doc, KEY_TOUR_START);
            view.sync(&mut doc, &automerge::ROOT);
            assert_eq!(view.tour_page, Some(TourPage::Navigate));
            assert!(!view.finished());

            press(&mut doc, KEY_TOUR_NEXT);
            view.sync(&mut doc, &automerge::ROOT);
            assert_eq!(view.tour_page, Some(TourPage::Organise));
            press(&mut doc, KEY_TOUR_NEXT);
            view.sync(&mut doc, &automerge::ROOT);
            assert_eq!(view.tour_page, Some(TourPage::Collaborate));

            press(&mut doc, KEY_FINISH);
            view.sync(&mut doc, &automerge::ROOT);
            assert!(
                !view.finished(),
                "the tour cannot finish before its hand-off page"
            );

            press(&mut doc, KEY_TOUR_NEXT);
            view.sync(&mut doc, &automerge::ROOT);
            assert_eq!(view.tour_page, Some(TourPage::Start));

            press(&mut doc, KEY_TOUR_BACK);
            view.sync(&mut doc, &automerge::ROOT);
            assert_eq!(view.tour_page, Some(TourPage::Collaborate));
            press(&mut doc, KEY_TOUR_NEXT);
            view.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_FINISH);
            view.sync(&mut doc, &automerge::ROOT);
            assert!(view.finished());
        }
        // Phase `every_tour_page_exposes_an_explicit_skip_action`.
        {
            let mut view = OnboardingView::new_tour();
            let mut doc = AutoCommit::new();
            view.sync(&mut doc, &automerge::ROOT);

            for page in [
                TourPage::Navigate,
                TourPage::Organise,
                TourPage::Collaborate,
            ] {
                assert_eq!(view.tour_page, Some(page));
                let actions = OnboardingView::tour_actions(page);
                let node = accessibility_for_id(actions.as_ref(), "onboarding.tour.skip.button")
                    .expect("tour page exposes Skip tour");
                assert_eq!(node.name.as_deref(), Some("Skip tour"));
                if page != TourPage::Collaborate {
                    press(&mut doc, KEY_TOUR_NEXT);
                    view.sync(&mut doc, &automerge::ROOT);
                }
            }

            press(&mut doc, KEY_TOUR_SKIP);
            view.sync(&mut doc, &automerge::ROOT);
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
                press(&mut document, KEY_TOUR_SKIP);
                view.sync(&mut document, &automerge::ROOT);

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
                    press(&mut document, KEY_TOUR_NEXT);
                    view.sync(&mut document, &automerge::ROOT);
                }

                assert!(dispatch_escape(&mut view, &mut document));
                view.sync(&mut document, &automerge::ROOT);
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

            press(&mut doc, KEY_TOUR_BACK);
            view.sync(&mut doc, &automerge::ROOT);
            assert!(
                view.finished(),
                "Back on the first page closes a reopened tour"
            );

            let mut view = OnboardingView::new_tour();
            press(&mut doc, KEY_TOUR_NEXT);
            view.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_TOUR_NEXT);
            view.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_TOUR_NEXT);
            view.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_FINISH);
            view.sync(&mut doc, &automerge::ROOT);
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
            press(&mut document, KEY_TOUR_NEXT);
            view.sync(&mut document, &automerge::ROOT);
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
    fn onboarding_copy_and_live_regions_speak_the_real_contract() {
        // Phase `desktop_tour_teaches_the_real_keyboard_and_gesture_contract`.
        {
            let navigation = NAVIGATE_CARDS
                .iter()
                .flat_map(|(title, body)| [t(title), t(body)])
                .collect::<Vec<_>>()
                .join(" ");

            if cfg!(target_os = "macos") {
                assert!(navigation.contains("Command+K"));
                assert!(navigation.contains("Option+Space"));
            } else {
                assert!(navigation.contains("Super+K"));
                assert!(navigation.contains("Super+Space"));
            }
            assert!(navigation.contains("Start here, Everyday, Work, Media, and System tools"));
            assert!(navigation.contains("Super+Tab"));
            assert!(navigation.contains("Super+Tab or pinch"));
            assert!(navigation.contains("Super+arrow keys"));
            assert!(navigation.contains("three-finger swipe"));
            assert!(navigation.contains("Up and down go to the strip above or below"));
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
            view.flow.begin_connect();
            let theme = Theme::dark();

            for status in [
                "Looking for a trusted device nearby…",
                "Code 123456 · waiting for approval on your trusted device",
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
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            view.sync(&mut doc, &root);
            let _ = doc.put(&root, KEY_BEGIN, true);
            view.sync(&mut doc, &root);
            assert_eq!(view.transition_direction, 1.0);
            let _ = doc.put(&root, KEY_BACK, true);
            view.sync(&mut doc, &root);
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
            press(&mut document, KEY_CONNECT);
            view.sync(&mut document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Connect);
            assert!(dispatch_escape(&mut view, &mut document));
            view.sync(&mut document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::Intro);

            press(&mut document, KEY_BEGIN);
            view.sync(&mut document, &automerge::ROOT);
            assert_eq!(view.flow.screen(), Screen::NoRecovery);
            assert!(dispatch_escape(&mut view, &mut document));
            view.sync(&mut document, &automerge::ROOT);
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
            assert_eq!(recovery.flow.screen(), Screen::Connect);
            let mut document = AutoCommit::new();
            press(&mut document, KEY_BACK);
            recovery.sync(&mut document, &automerge::ROOT);
            assert_eq!(recovery.flow.screen(), Screen::Connect);
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
            press(&mut doc, KEY_CONNECT);
            let _ = doc.put(
                &automerge::ROOT,
                KEY_CONNECT_PATH,
                ScalarValue::Str("/media/backup/primary.json".into()),
            );
            view.sync(&mut doc, &automerge::ROOT);
            assert_eq!(view.flow().screen(), Screen::Connect);
            press(&mut doc, KEY_IMPORT);
            view.sync(&mut doc, &automerge::ROOT);
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

            press(&mut document, KEY_CONNECT);
            let _ = document.put(
                &automerge::ROOT,
                KEY_CONNECT_PATH,
                ScalarValue::Str("/media/backup/primary.json".into()),
            );
            view.sync(&mut document, &automerge::ROOT);
            press(&mut document, KEY_IMPORT);
            view.sync(&mut document, &automerge::ROOT);

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
            press(&mut doc, KEY_CONNECT);
            view.sync(&mut doc, &automerge::ROOT);
            let _ = doc.put(
                &automerge::ROOT,
                KEY_CONNECT_PATH,
                ScalarValue::Str("/media/identity.tilo-identity".into()),
            );
            press(&mut doc, KEY_IMPORT);
            view.sync(&mut doc, &automerge::ROOT);
            assert_eq!(view.flow().screen(), Screen::Connect);
            assert_eq!(view.error(), Some("not a Tilo identity"));
        }
        // Phase `connect_requires_a_selected_file_before_importing`.
        {
            let mut view = OnboardingView::with_handlers(
                Box::new(|| Err("create should not run".to_string())),
                Box::new(|_, _| Err("importer must not run without a path".to_string())),
            );
            let mut doc = AutoCommit::new();
            press(&mut doc, KEY_CONNECT);
            view.sync(&mut doc, &automerge::ROOT);
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
            press(&mut doc, KEY_IMPORT);
            view.sync(&mut doc, &automerge::ROOT);
            assert_eq!(view.flow().screen(), Screen::Connect);
            assert_eq!(
                view.error(),
                Some("Choose an encrypted backup or legacy identity file first.")
            );

            let _ = doc.put(
                &automerge::ROOT,
                KEY_CONNECT_PATH,
                ScalarValue::Str("/media/identity.tilo-identity".into()),
            );
            view.sync(&mut doc, &automerge::ROOT);
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
            press(&mut doc, KEY_CONNECT);
            view.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_CONNECT_BROWSE);
            view.sync(&mut doc, &automerge::ROOT);
            assert!(view.picker.is_some());

            view.picker.as_mut().expect("picker").1.click_cancel();
            view.sync(&mut doc, &automerge::ROOT);
            assert!(view.picker.is_none());
            assert_eq!(view.flow().screen(), Screen::Connect);
        }
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
            press(&mut doc, KEY_BACKUP_BROWSE);
            view.sync(&mut doc, &automerge::ROOT);
            assert!(view.picker.is_none());
            assert!(
                view.backup_error
                    .as_deref()
                    .unwrap()
                    .contains("do not match")
            );

            *view.backup_confirmation.lock().unwrap() = "correct horse battery staple".into();
            press(&mut doc, KEY_BACKUP_BROWSE);
            view.sync(&mut doc, &automerge::ROOT);
            view.picker.as_mut().expect("save picker").1.click_cancel();
            view.sync(&mut doc, &automerge::ROOT);
            assert!(view.picker.is_none());
            assert_eq!(exports.load(Ordering::SeqCst), 0);

            press(&mut doc, KEY_BACKUP_BROWSE);
            view.sync(&mut doc, &automerge::ROOT);
            view.picker.as_mut().expect("save picker").1.click_open();
            view.sync(&mut doc, &automerge::ROOT);
            assert_eq!(exports.load(Ordering::SeqCst), 1);
            assert!(view.backup_saved.is_some());
            assert!(view.backup_error.is_none());
            assert_eq!(
                load_progress(progress.path()).expect("verified backup progress"),
                Some(PersistedStage::IdentityReadyBackedUp)
            );
            assert!(view.backup_passphrase.lock().unwrap().is_empty());
            assert!(view.backup_confirmation.lock().unwrap().is_empty());

            press(&mut doc, KEY_BACKUP_CONTINUE);
            view.sync(&mut doc, &automerge::ROOT);
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
            press(&mut doc, KEY_BACKUP_CONTINUE);
            view.sync(&mut doc, &automerge::ROOT);

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
            press(&mut document, KEY_BACKUP_ANOTHER);
            view.sync(&mut document, &automerge::ROOT);

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
            press(doc, key);
            view.sync(doc, &automerge::ROOT);
        }
    }

    fn layout(view: &mut OnboardingView) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let _ = view.build_layout(&mut taffy, &theme, &mut node_map);
    }

    fn stage_label(view: &OnboardingView) -> Option<String> {
        view.composed
            .as_deref()
            .and_then(|composed| accessibility_for_id(composed, "onboarding.progress.stage"))
            .and_then(|node| node.name)
    }

    #[test]
    fn the_codecs_step_explains_the_choice_and_nothing_is_preselected() {
        let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
        let (view, _) = view();
        let mut view = view.with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
        let mut doc = AutoCommit::new();
        walk_to_codecs(&mut view, &mut doc);
        assert!(view.codec_page_visible());
        layout(&mut view);
        assert_eq!(stage_label(&view).as_deref(), Some("Step 4 of 5  ·  Media"));
        let lines = view.codec_page_lines();
        for needle in [
            "AMD and Intel",
            "screen sharing, recording and calls",
            "RPM Fusion",
            "Cisco (OpenH264)",
            "Tilo does not host or distribute them",
        ] {
            assert!(
                lines.iter().any(|line| line.contains(needle)),
                "{needle}: {lines:?}"
            );
        }
        assert_eq!(
            &lines[lines.len() - 2..],
            ["Not now", "Install media codecs"],
            "an explicit button, not a pre-ticked box"
        );
        assert!(fake.runs().is_empty(), "reaching the step installs nothing");
        // The step blocks neither the tour nor finishing only by accident:
        // both wait until the person chose.
        press(&mut doc, KEY_FINISH);
        view.sync(&mut doc, &automerge::ROOT);
        assert!(!view.finished());
        assert!(view.codec_page_visible());
    }

    #[test]
    fn installing_from_setup_records_the_choice_and_shows_the_result() {
        let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
        let (view, _) = view();
        let mut view = view.with_codec_setup(fake.setup(
            r#"{"state":"installed","mesa":"enabled","desired":true}"#,
            Ok(()),
        ));
        let mut doc = AutoCommit::new();
        walk_to_codecs(&mut view, &mut doc);
        press(&mut doc, KEY_CODECS_INSTALL);
        view.sync(&mut doc, &automerge::ROOT);
        view.wait_for_codec_install();
        view.sync(&mut doc, &automerge::ROOT);
        assert_eq!(fake.runs(), vec![CodecAction::Install]);
        assert_eq!(view.codec_status().state, PackState::Installed);
        layout(&mut view);
        let lines = view.codec_page_lines();
        assert!(lines.contains(&"Media codecs installed".to_string()));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("sign out and back in")),
            "{lines:?}"
        );
        assert_eq!(lines.last().map(String::as_str), Some("Continue"));
        press(&mut doc, KEY_CODECS_CONTINUE);
        view.sync(&mut doc, &automerge::ROOT);
        assert!(!view.codec_page_visible());
        layout(&mut view);
        assert_eq!(stage_label(&view).as_deref(), Some("Step 5 of 5  ·  Ready"));
        press(&mut doc, KEY_FINISH);
        view.sync(&mut doc, &automerge::ROOT);
        assert!(view.finished());
    }

    #[test]
    fn not_now_installs_nothing_and_continues() {
        let fake = FakeCodecs::new(r#"{"state":"not-installed"}"#);
        let (view, _) = view();
        let mut view = view.with_codec_setup(fake.setup(r#"{"state":"installed"}"#, Ok(())));
        let mut doc = AutoCommit::new();
        walk_to_codecs(&mut view, &mut doc);
        press(&mut doc, KEY_CODECS_SKIP);
        view.sync(&mut doc, &automerge::ROOT);
        assert!(!view.codec_page_visible());
        assert!(fake.runs().is_empty());
        press(&mut doc, KEY_FINISH);
        view.sync(&mut doc, &automerge::ROOT);
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
                *writer.lock().unwrap() = r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":7,"label":"Downloading codecs"}}"#.to_string();
                let _ = started_tx.lock().unwrap().send(());
                let _ = gate.lock().unwrap().recv();
                Ok(())
            }),
        ));
        let mut doc = AutoCommit::new();
        walk_to_codecs(&mut view, &mut doc);
        press(&mut doc, KEY_CODECS_INSTALL);
        view.sync(&mut doc, &automerge::ROOT);
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
        view.sync(&mut doc, &automerge::ROOT);
        let lines = view.codec_page_lines();
        assert_eq!(view.codec_status().state, PackState::Installing);
        assert!(lines.contains(&"Installing media codecs".to_string()));
        assert!(lines.contains(&"Progress: 29%".to_string()), "{lines:?}");
        assert!(
            lines.iter().any(|line| line.contains("Setup can continue")),
            "{lines:?}"
        );
        assert_eq!(lines.last().map(String::as_str), Some("Continue"));
        press(&mut doc, KEY_CODECS_CONTINUE);
        view.sync(&mut doc, &automerge::ROOT);
        assert!(
            !view.codec_page_visible(),
            "the download does not hold setup"
        );
        press(&mut doc, KEY_FINISH);
        view.sync(&mut doc, &automerge::ROOT);
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
        press(&mut doc, KEY_CODECS_INSTALL);
        view.sync(&mut doc, &automerge::ROOT);
        view.wait_for_codec_install();
        view.sync(&mut doc, &automerge::ROOT);
        let lines = view.codec_page_lines();
        assert!(lines.contains(&"Media codecs will install once you are online".to_string()));
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
        press(&mut doc, KEY_CODECS_INSTALL);
        view.sync(&mut doc, &automerge::ROOT);
        view.wait_for_codec_install();
        view.sync(&mut doc, &automerge::ROOT);
        let lines = view.codec_page_lines();
        assert!(lines.contains(&"Media codecs could not be installed".to_string()));
        assert!(lines.contains(&"Access denied: not an administrator".to_string()));
        assert_eq!(&lines[lines.len() - 2..], ["Skip", "Try again"]);
        layout(&mut view);
        press(&mut doc, KEY_CODECS_INSTALL);
        view.sync(&mut doc, &automerge::ROOT);
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
        assert_eq!(stage_label(&view).as_deref(), Some("Step 4 of 4  ·  Ready"));
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
                    press(&mut doc, k);
                    v.sync(&mut doc, &automerge::ROOT);
                }
                let theme = Theme::dark();
                let mut taffy: TaffyTree<()> = TaffyTree::new();
                let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
                let _ = v.build_layout(&mut taffy, &theme, &mut node_map);
            }

            let (mut connect, _) = view();
            let mut connect_doc = AutoCommit::new();
            press(&mut connect_doc, KEY_CONNECT);
            connect.sync(&mut connect_doc, &automerge::ROOT);
            let theme = Theme::dark();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let _ = connect.build_layout(&mut taffy, &theme, &mut node_map);
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
            (
                &[],
                &["onboarding.connect.button", "onboarding.begin.button"],
                false,
            ),
            (
                &[KEY_CONNECT],
                &["onboarding.back.button", "onboarding.import.button"],
                false,
            ),
            (
                &[KEY_CONNECT],
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

        for (width, height, theme) in [
            (1600.0, 900.0, Theme::dark()),
            (1366.0, 768.0, Theme::dark()),
            (1280.0, 720.0, Theme::dark()),
            (800.0, 600.0, Theme::dark()),
            (1280.0, 720.0, Theme::dark().with_text_scale(1.3)),
            (1280.0, 720.0, Theme::high_contrast().with_ui_scale(1.25)),
        ] {
            for &(keys, action_ids, error_state) in scenarios {
                let mut view = OnboardingView::with_minter_for_host_desktop(Box::new(|| {
                    Ok(MintedIdentity {
                        fingerprint: "ab".repeat(32),
                        path: PathBuf::from("/identity/primary.json"),
                    })
                }));
                let mut document = AutoCommit::new();
                for key in keys {
                    press(&mut document, key);
                    view.sync(&mut document, &automerge::ROOT);
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
                    // The recovery form is deliberately pinned below the
                    // explanatory viewport, so its controls must be judged
                    // against the panel rather than as scroll descendants.
                    for control_id in [
                        "onboarding.backup.passphrase",
                        "onboarding.backup.confirmation",
                        "onboarding.backup.button",
                    ] {
                        let control = widget_rect(control_id);
                        assert!(
                            control.x0 >= panel.x0 + f64::from(theme.tokens.spacing.s3) - 0.5
                                && control.x1
                                    <= panel.x1 - f64::from(theme.tokens.spacing.s3) + 0.5
                                && control.y0
                                    >= content_viewport.y1 + f64::from(theme.tokens.spacing.s3)
                                        - 0.5
                                && control.y1 <= panel.y1 - 16.0,
                            "Pinned backup control must remain wholly visible below its explanation at {width}x{height}: control={control_id} {control:?}, viewport={content_viewport:?}, panel={panel:?}, theme={}",
                            theme.name,
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

                if view.flow().screen() == Screen::Connect {
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

                if ((width == 800.0 && height == 600.0) || (width == 1280.0 && height == 800.0))
                    && view.flow().screen() == Screen::Done
                    && view.identity_ready_page
                    && theme.text_scale <= 1.0
                    && !theme.name.eq_ignore_ascii_case("high contrast")
                {
                    assert_eq!(
                        scroll.max_offset_y(),
                        0.0,
                        "the normal-text Ready page must present every decision before the person discovers scrolling at {width}x{height}"
                    );
                    let heading = node_map
                        .values()
                        .find_map(|&pointer| unsafe {
                            ((*pointer).name() == "Text"
                                && (*pointer).accessibility().is_some_and(|node| {
                                    node.name.as_deref() == Some("Your Tilo identity is ready")
                                }))
                            .then(|| (*pointer).get_rect())
                        })
                        .expect("Ready page title");
                    assert!(
                        panel.height()
                            >= f64::from(READY_PANEL_PREFERRED_HEIGHT)
                                .min(f64::from(height) - 2.0 * f64::from(VIEWPORT_GUTTER))
                                - 0.5
                            && heading.area() > 0.0
                            && heading.y0 >= content_viewport.y0
                            && heading.y1 <= content_viewport.y1,
                        "Ready must render as a complete hand-off surface, not a collapsed action strip at {width}x{height}: panel={panel:?}, heading={heading:?}, viewport={content_viewport:?}"
                    );
                }

                if view.flow().screen() == Screen::Done && view.identity_ready_page {
                    let guidance = widget_rect("onboarding.done.backup-guidance");
                    let max_offset = f64::from(scroll.max_offset_y());
                    let earliest_offset = (guidance.y1 - content_viewport.y1).max(0.0);
                    let latest_offset = (guidance.y0 - content_viewport.y0).min(max_offset);
                    assert!(
                        guidance.height() <= content_viewport.height()
                            && earliest_offset <= latest_offset + 0.5,
                        "Ready-page backup guidance must be wholly reachable rather than clipped behind the footer at {width}x{height}: guidance={guidance:?}, viewport={content_viewport:?}, max_offset={max_offset}, theme={}",
                        theme.name,
                    );
                }
            }
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
            press(&mut document, KEY_BEGIN);
            view.sync(&mut document, &automerge::ROOT);

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
            for event in [pointer_event(label_point), primary_press_event()] {
                let mut handled = false;
                view.handle_event(
                    &event,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &Modifiers::default(),
                        state: &mut document,
                        path: &automerge::ROOT,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
                assert!(handled, "the labelled checkbox row must handle {event:?}");
            }
            assert!(read_bool(&document, &automerge::ROOT, KEY_UNDERSTOOD));
        }
        // Phase `continue_without_backup_pointer_target_advances_to_ready_page`.
        {
            let (mut view, _) = view();
            let mut document = AutoCommit::new();
            for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
                press(&mut document, key);
                view.sync(&mut document, &automerge::ROOT);
            }
            document
                .put(
                    &automerge::ROOT,
                    KEY_BACKUP_SKIP_ACKNOWLEDGED,
                    ScalarValue::Boolean(true),
                )
                .expect("acknowledge backup risk");
            view.sync(&mut document, &automerge::ROOT);

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
                button.width() >= 44.0 && button.height() >= 40.0,
                "backup continuation needs a practical pointer target: {button:?}"
            );

            let mut focus = None;
            for event in [
                pointer_event(button.center()),
                primary_press_event(),
                primary_release_event(),
            ] {
                let mut handled = false;
                view.handle_event(
                    &event,
                    &mut ui::widget::EventCtx {
                        scale_factor: 1.0,
                        modifiers: &Modifiers::default(),
                        state: &mut document,
                        path: &automerge::ROOT,
                        handled: &mut handled,
                        focus_target: &mut focus,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
                if !matches!(event, WindowEvent::CursorMoved { .. }) {
                    assert!(
                        handled,
                        "the backup continuation button must handle {event:?}"
                    );
                }
            }
            view.sync(&mut document, &automerge::ROOT);
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
    fn backup_and_recovery_tasks_are_visible_in_the_first_frame_at_review_sizes() {
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
        // Phase `backup_recovery_controls_are_first_frame_visible_at_review_sizes`.
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
                            && label.y0 >= 0.0
                            && label.y1 <= f64::from(height)
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
                            && control.y1 <= f64::from(height),
                        "{id} must be fully visible at {width}x{height}: {control:?}"
                    );
                }
                assert!(passphrase.y1 <= confirmation.y0);
                assert!(confirmation.y1 <= save.y0);
                assert!(
                    save.y1 <= skip.y0,
                    "recovery stays primary and must not overlap skip at {width}x{height}: save={save:?}, skip={skip:?}"
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
                press(&mut doc, key);
                view.sync(&mut doc, &automerge::ROOT);
            }
            assert_eq!(view.flow().screen(), Screen::Done);
            press(&mut doc, KEY_BACKUP_SKIP_ACKNOWLEDGED);
            view.sync(&mut doc, &automerge::ROOT);
            press(&mut doc, KEY_BACKUP_CONTINUE);
            view.sync(&mut doc, &automerge::ROOT);

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

            let panel = view.composed.as_ref().expect("composition").get_rect();
            assert!(
                panel.height() > f64::from(PANEL_MAX_HEIGHT),
                "large-text setup should use otherwise-empty viewport height before clipping: {panel:?}",
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
            }
        }
        // Phase `large_text_backup_action_keeps_a_clear_gap_below_confirmation`.
        {
            let (mut view, _) = view();
            let mut document = AutoCommit::new();
            for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
                press(&mut document, key);
                view.sync(&mut document, &automerge::ROOT);
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
                    press(&mut doc, key);
                    view.sync(&mut doc, &automerge::ROOT);
                }
                for _ in 0..next_presses {
                    press(&mut doc, KEY_TOUR_NEXT);
                    view.sync(&mut doc, &automerge::ROOT);
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
