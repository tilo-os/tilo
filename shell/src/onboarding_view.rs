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

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Read as _;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
use winit::event::{ElementState, Modifiers, WindowEvent};
use winit::keyboard::{Key, NamedKey};
use zeroize::{Zeroize, Zeroizing};

use crate::onboarding::{
    ACKNOWLEDGEMENT, DONE_BODY, DONE_TITLE, INTRO_BODY, INTRO_POINTS, INTRO_TITLE, MintedIdentity,
    NO_RECOVERY_POINTS, NO_RECOVERY_TITLE, Onboarding, Screen,
};

const PANEL_MAX_WIDTH: f32 = 720.0;
const PANEL_MAX_HEIGHT: f32 = 620.0;
const VIEWPORT_GUTTER: f32 = 32.0;
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
    IdentityReady,
    TourNavigate,
    TourOrganise,
    TourCollaborate,
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
    atomic_file::write_with_mode(&progress_path(identity_dir), &bytes, 0o600)
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
/// the same security gates; the host version only makes clear that qOS is not
/// replacing the login/account setup the surrounding Linux system completed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OnboardingContext {
    #[default]
    FullSystem,
    HostDesktop,
}

pub const HOST_DESKTOP_INTRO_TITLE: &str = "Set up your qOS identity";
pub const HOST_DESKTOP_INTRO_BODY: &str = "You are already signed in to this computer. Before the qOS desktop opens, it needs a local cryptographic identity for collaboration and peer invitations. This does not replace your Linux login or create an online account. Individual app edits are not all cryptographically signed yet.";

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
pub const KEY_TOUR_START: &str = "onboarding.tour.start";
pub const KEY_TOUR_BACK: &str = "onboarding.tour.back";
pub const KEY_TOUR_NEXT: &str = "onboarding.tour.next";
pub const KEY_FINISH: &str = "onboarding.finish";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TourPage {
    Navigate,
    Organise,
    Collaborate,
}

// Keep this copy beside the tour rather than duplicating a second conceptual
// keyboard map in prose. These labels mirror `docs/shell-navigation-keyboard-map.md`
// and the gesture contract in `compositor/backend/drm/gestures.rs`; in
// particular, Super+W closes a tab and must never be advertised as Overview.
const NAVIGATE_CARDS: &[(&str, &str)] = &[
    (
        "Search  ·  Super+K or Super+Space",
        "Open it without typing to browse Start here, Everyday, Work, Media, and System tools; type to find any app, file, setting, or action.",
    ),
    (
        "Overview  ·  Ctrl+Tab or four-finger swipe",
        "See every workspace and window group, then use Up, Down, and Enter to choose one.",
    ),
    (
        "Move  ·  Super+arrow keys or three-finger swipe",
        "Up and down switch workspaces. Left and right step between tiled windows along the current strip.",
    ),
];

const ORGANISE_CARDS: &[(&str, &str)] = &[
    (
        "Workspace",
        "A durable place for a project, its people, permissions, apps, and history.",
    ),
    (
        "Strip",
        "A horizontal run of window groups inside a workspace. Add another strip when work needs a second lane.",
    ),
    (
        "Window group",
        "A tiled frame for tabs that belong together. Move between groups with Super+Left or Super+Right.",
    ),
    (
        "Tab",
        "An app or document inside a window group. Drag tabs to arrange them without overlapping windows.",
    ),
];

impl TourPage {
    fn number(self) -> u8 {
        match self {
            Self::Navigate => 1,
            Self::Organise => 2,
            Self::Collaborate => 3,
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

pub struct OnboardingView {
    flow: Onboarding,
    context: OnboardingContext,
    minter: Minter,
    importer: Importer,
    exporter: Exporter,
    /// Set when minting failed, shown in place of the success screen — loud,
    /// since booting on into a desktop that silently has no identity is what this flow replaces.
    error: Option<String>,
    backup_error: Option<String>,
    backup_saved: Option<PathBuf>,
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
    connect_passphrase: Arc<Mutex<String>>,
    backup_passphrase: Arc<Mutex<String>>,
    backup_confirmation: Arc<Mutex<String>>,
    identity_dir: Option<PathBuf>,
    nearby_child: Option<Child>,
    nearby_status: Option<String>,
    nearby_status_path: Option<PathBuf>,
}

impl OnboardingView {
    /// Build a ceremony that mints into `identity_dir`.
    pub fn new(identity_dir: PathBuf) -> Self {
        Self::new_in_context(identity_dir, OnboardingContext::FullSystem)
    }

    /// Build the reduced identity-only ceremony for qOS installed as a
    /// desktop session on an existing Linux host.
    pub fn new_for_host_desktop(identity_dir: PathBuf) -> Self {
        Self::new_in_context(identity_dir, OnboardingContext::HostDesktop)
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

    fn new_in_context(identity_dir: PathBuf, context: OnboardingContext) -> Self {
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
                    // older qOS builds. New backups always require a
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
            minter,
            importer,
            exporter,
            error: None,
            backup_error: None,
            backup_saved: None,
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
            connect_passphrase: Arc::new(Mutex::new(String::new())),
            backup_passphrase: Arc::new(Mutex::new(String::new())),
            backup_confirmation: Arc::new(Mutex::new(String::new())),
            identity_dir: None,
            nearby_child: None,
            nearby_status: None,
            nearby_status_path: None,
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

    fn restore_pending_progress_from_disk(&mut self) {
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
                self.tour_page = match stage {
                    PersistedStage::AcquiringIdentity | PersistedStage::IdentityReady => None,
                    PersistedStage::TourNavigate => Some(TourPage::Navigate),
                    PersistedStage::TourOrganise => Some(TourPage::Organise),
                    PersistedStage::TourCollaborate => Some(TourPage::Collaborate),
                };
                if let Some(error) = load_error {
                    self.progress_error = Some(format!(
                        "The saved setup position could not be read ({error}). Your identity is safe; setup resumed at the completion step."
                    ));
                }
            }
            Err(error) => {
                // A pre-acquisition journal with no identity is the expected
                // result of a crash before the key transaction committed:
                // restart the creation flow. Anything present-but-invalid was
                // already rejected by normal startup and is surfaced here too.
                if std::fs::symlink_metadata(&identity_path).is_ok() {
                    self.progress_error = Some(format!(
                        "Setup found unfinished progress but could not validate the existing identity: {error}"
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
                self.progress_error = Some(format!(
                    "Setup could not save its restart position: {error}. No identity action was repeated."
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
                self.progress_error = Some(format!(
                    "Setup finished, but its restart marker could not be removed: {error}. Try Finish again so this screen does not return next login."
                ));
                self.invalidation.structure();
                false
            }
        }
    }

    fn progress_notice(&self) -> Option<Box<dyn Widget>> {
        self.progress_error.as_ref().map(|error| {
            Box::new(Banner::error("Setup progress needs attention").with_message(error.clone()))
                as Box<dyn Widget>
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

    fn build_composition(&self, theme: &Theme, panel_height: f32) -> Box<dyn Widget> {
        if let Some(page) = self.tour_page {
            return self.build_tour_composition(theme, panel_height, page);
        }
        let (step, stage) = match self.flow.screen() {
            Screen::Intro => (1, "Choose"),
            Screen::Connect | Screen::NoRecovery => (2, "Protect"),
            Screen::Done => (3, "Ready"),
        };
        let body: Box<dyn Widget> = match self.flow.screen() {
            Screen::Intro => self.intro(theme),
            Screen::Connect => self.connect(theme),
            Screen::NoRecovery => self.no_recovery(theme),
            Screen::Done => self.done(theme),
        };
        let spacing = &theme.tokens.spacing;
        let progress_value = match step {
            1 => 1.0 / 3.0,
            2 => 2.0 / 3.0,
            _ => 1.0,
        };
        let progress = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s2)
            .push(Box::new(
                Row::new()
                    .push(Box::new(
                        Text::new("qOS setup")
                            .with_role(ui::tokens::typography::TextRole::Label)
                            .with_color(theme.text_dim),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Text::new(&format!("{stage}  ·  {step} of 3"))
                            .with_role(ui::tokens::typography::TextRole::Label)
                            .with_color(theme.text_dim),
                    )),
            ))
            .push(Box::new(ProgressBar::new(progress_value).with_label(
                format!("Identity setup, step {step} of 3: {stage}"),
            )));
        let mut content = Column::new()
            .with_intrinsic_height()
            .with_gap(spacing.s6)
            .push(Box::new(progress));
        if let Some(notice) = self.progress_notice() {
            content = content.push(notice);
        }
        content = content.push(body);
        if self.flow.screen() == Screen::Done {
            // The backup card can be taller than a laptop-height viewport,
            // especially with large text. Keep the only route into the
            // desktop outside that scroll region so setup never becomes a
            // visual dead end.
            let footer_height = 56.0;
            // The outer Column owns `padding_standard * 2` on both edges plus
            // the gap between the scroll region and footer. Account for those
            // explicitly; otherwise large text pushes the sticky actions below
            // a laptop-height viewport even though the panel itself is bounded.
            let outer_vertical_padding = theme.padding_standard * 4.0;
            let footer_gap = spacing.s3;
            let scroll_height =
                (panel_height - footer_height - footer_gap - outer_vertical_padding).max(120.0);
            return Box::new(
                Column::new()
                    .with_no_padding()
                    .with_gap(spacing.s3)
                    .push(Box::new(
                        Scrollable::new_vertical()
                            .with_height(scroll_height)
                            .with_content_gutter(spacing.s3)
                            .with_child(Box::new(content)),
                    ))
                    .push(Self::finish_action()),
            );
        }
        Box::new(
            Scrollable::new_vertical()
                .with_height(panel_height)
                .with_content_gutter(spacing.s3)
                .with_child(Box::new(content)),
        )
    }

    fn build_tour_composition(
        &self,
        theme: &Theme,
        panel_height: f32,
        page: TourPage,
    ) -> Box<dyn Widget> {
        let number = page.number();
        let stage = match page {
            TourPage::Navigate => "Move",
            TourPage::Organise => "Arrange",
            TourPage::Collaborate => "Share",
        };
        let progress = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s2)
            .push(Box::new(
                Row::new()
                    .push(Box::new(
                        Text::new("qOS  •  DESKTOP TOUR")
                            .with_role(ui::tokens::typography::TextRole::Label)
                            .with_color(theme.text_dim),
                    ))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        Text::new(&format!("{stage}  ·  {number} of 3"))
                            .with_role(ui::tokens::typography::TextRole::Label)
                            .with_color(theme.text_dim),
                    )),
            ))
            .push(Box::new(
                ProgressBar::new(f32::from(number) / 3.0)
                    .with_label(format!("Desktop tour, step {number} of 3: {stage}")),
            ));
        let mut content = Column::new()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s6)
            .push(Box::new(progress));
        if let Some(notice) = self.progress_notice() {
            content = content.push(notice);
        }
        content = content.push(Self::tour_page(theme, page));
        // Keep navigation out of the scrolling lesson. On a compact laptop,
        // portrait display, or large-text setup the cards can be much taller
        // than the panel; making Continue part of that content forced people
        // to discover scrolling before they could finish the introduction.
        // The identity Done screen uses the same sticky-footer contract.
        let footer_height = 48.0;
        let footer_gap = theme.tokens.spacing.s3;
        let outer_vertical_padding = theme.padding_standard * 4.0;
        let scroll_height =
            (panel_height - footer_height - footer_gap - outer_vertical_padding).max(120.0);
        Box::new(
            Column::new()
                .with_no_padding()
                .with_gap(footer_gap)
                .push(Box::new(
                    Scrollable::new_vertical()
                        .with_height(scroll_height)
                        .with_content_gutter(theme.tokens.spacing.s3)
                        .with_child(Box::new(content)),
                ))
                .push(Self::tour_actions(page)),
        )
    }

    fn tour_page(theme: &Theme, page: TourPage) -> Box<dyn Widget> {
        let (title, intro, cards): (&str, &str, &[(&str, &str)]) = match page {
            TourPage::Navigate => (
                "Everything is close at hand",
                "qOS keeps the desktop calm until you ask for something. Every gesture here has a keyboard route too.",
                NAVIGATE_CARDS,
            ),
            TourPage::Organise => (
                "A place for each kind of work",
                "The layout is simple once its four levels have names. qOS remembers the arrangement for you.",
                ORGANISE_CARDS,
            ),
            TourPage::Collaborate => (
                "Sharing stays deliberate",
                "Collaboration is built into the workspace, not bolted onto individual apps. Nothing becomes shared merely because it is open.",
                &[
                    (
                        "Invite from Workspace Settings",
                        "Choose a role and review exactly which workspace the invitation opens.",
                    ),
                    (
                        "Follow only when you choose",
                        "People keep their own focus and view. Follow is an explicit, reversible action.",
                    ),
                    (
                        "Undo and permissions are system features",
                        "History follows authorship, and apps ask qOS for capabilities instead of receiving ambient access.",
                    ),
                ],
            ),
        };
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s6)
            .push(heading(title, theme))
            .push(paragraph(intro, theme));
        for (card_title, copy) in cards {
            body = body.push(Box::new(Card::titled(*card_title, paragraph(copy, theme))));
        }
        Box::new(body)
    }

    /// Sticky tour navigation, kept outside the scrollable lesson body.
    fn tour_actions(page: TourPage) -> Box<dyn Widget> {
        let (primary_label, primary_key) = if page == TourPage::Collaborate {
            ("Enter qOS", KEY_FINISH)
        } else {
            ("Continue", KEY_TOUR_NEXT)
        };
        actions(vec![
            Box::new(
                Button::new("onboarding.tour.skip.button", "Skip tour", Some(KEY_FINISH))
                    .fit_content(40.0),
            ),
            Box::new(
                Button::new("onboarding.tour.back.button", "Back", Some(KEY_TOUR_BACK))
                    .fit_content(40.0),
            ),
            Box::new(
                Button::new(
                    "onboarding.tour.next.button",
                    primary_label,
                    Some(primary_key),
                )
                .fit_content(40.0)
                .with_style(ui::widget::ButtonStyle::Primary),
            ),
        ])
    }

    fn intro(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let mut list = Column::new().with_no_padding().with_gap(spacing.s3);
        for point in INTRO_POINTS {
            list = list.push(bullet(point, theme));
        }
        let (title, body) = match self.context {
            OnboardingContext::FullSystem => (INTRO_TITLE, INTRO_BODY),
            OnboardingContext::HostDesktop => (HOST_DESKTOP_INTRO_TITLE, HOST_DESKTOP_INTRO_BODY),
        };
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(spacing.s6)
                .push(heading(title, theme))
                .push(paragraph(body, theme))
                .push(Box::new(Card::titled("What it is for", Box::new(list))))
                .push(actions(vec![
                    Box::new(
                        Button::new(
                            "onboarding.connect.button",
                            "Use my existing qOS identity",
                            Some(KEY_CONNECT),
                        )
                        .with_icon(ui::icons::Icon::ExternalLink),
                    ),
                    Box::new(
                        Button::new(
                            "onboarding.begin.button",
                            "Create identity",
                            Some(KEY_BEGIN),
                        )
                        .with_icon(ui::icons::Icon::ArrowRight)
                        .with_style(ui::widget::ButtonStyle::Primary),
                    ),
                ])),
        )
    }

    fn connect(&self, theme: &Theme) -> Box<dyn Widget> {
        let path = TextInput::new(Vec::new())
            .with_id("onboarding.connect.path.field")
            .with_on_change_path(KEY_CONNECT_PATH)
            .with_accessible_name("Identity backup file")
            .with_placeholder("Choose an encrypted backup or legacy identity JSON")
            .with_font_size(theme.tokens.typography.scale.base);
        let passphrase = secret_input(
            "onboarding.connect.passphrase.field",
            &self.connect_passphrase,
            "Backup passphrase",
            "Passphrase for encrypted backups",
        )
        .with_font_size(theme.tokens.typography.scale.base);
        let spacing = &theme.tokens.spacing;
        let mut body = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(spacing.s6)
                .push(heading("Connect an existing identity", theme))
                .push(paragraph(
                    "Bring an encrypted backup from another qOS device. It is authenticated and validated locally before this machine is certified. For a legacy unencrypted JSON export, leave the passphrase empty.",
                    theme,
                ));
        if let Some(error) = &self.error {
            body = body.push(Box::new(
                Banner::error("That identity could not be connected").with_message(error.clone()),
            ));
        }
        let nearby_action = if self.nearby_child.is_some() {
            Button::new(
                "onboarding.connect.cancel.button",
                "Cancel",
                Some(KEY_CONNECT_CANCEL),
            )
        } else {
            Button::new(
                "onboarding.connect.nearby.button",
                "Connect nearby",
                Some(KEY_CONNECT_NEARBY),
            )
            .with_style(ui::widget::ButtonStyle::Primary)
        };
        let mut nearby = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s3)
            .push(paragraph(
                "On a trusted qOS device, open Settings → Devices & Sync → Add a personal device. Compare the six-digit code shown on both devices before approving.",
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
        let file_row = Row::new().push(Box::new(path)).push(Box::new(
            Button::new(
                "onboarding.connect.browse.button",
                "Browse…",
                Some(KEY_CONNECT_BROWSE),
            )
            .with_icon(ui::icons::Icon::Folder),
        ));
        Box::new(
            body.push(Box::new(Card::titled(
                "Nearby trusted device",
                Box::new(nearby),
            )))
            .push(Box::new(Card::titled(
                "Encrypted backup",
                Box::new(file_row),
            )))
            .push(Box::new(Card::titled(
                "Backup passphrase",
                Box::new(passphrase),
            )))
            .push(actions(vec![
                back_button(),
                Box::new(
                    Button::new("onboarding.import.button", "Connect", Some(KEY_IMPORT))
                        .with_icon(ui::icons::Icon::ArrowRight)
                        .with_style(ui::widget::ButtonStyle::Primary),
                ),
            ])),
        )
    }

    fn no_recovery(&self, theme: &Theme) -> Box<dyn Widget> {
        let spacing = &theme.tokens.spacing;
        let mut list = Column::new().with_no_padding().with_gap(spacing.s4);
        for point in NO_RECOVERY_POINTS {
            list = list.push(bullet(point, theme));
        }
        // The gate. Disabled until the box is ticked, so the primary action can't be clicked past the consequence.
        let create = Button::new("onboarding.create.button", "Create", Some(KEY_CREATE))
            .with_icon(ui::icons::Icon::Shield)
            .with_style(ui::widget::ButtonStyle::Primary)
            .with_enabled(self.flow.can_create());
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(spacing.s6)
            .push(heading("Keep the key to your identity", theme))
            .push(Box::new(
                Banner::warning(NO_RECOVERY_TITLE)
                    .with_message(
                        "There is no reset button. Read this once, then choose deliberately.",
                    )
                    .with_icon(ui::icons::Icon::Shield),
            ));
        if let Some(error) = &self.error {
            body = body.push(Box::new(
                Banner::error("Your identity was not created").with_message(error.clone()),
            ));
        }
        Box::new(
            body.push(Box::new(Card::new(Box::new(list))))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Checkbox::new("onboarding.understood.box", KEY_UNDERSTOOD)
                                .with_accessible_name(ACKNOWLEDGEMENT),
                        ))
                        .push(Box::new(
                            Text::new(ACKNOWLEDGEMENT)
                                .with_role(ui::tokens::typography::TextRole::Body),
                        )),
                ))
                .push(actions(vec![back_button(), Box::new(create)])),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the completion card is kept together so its backup and finish states remain coherent"
    )]
    fn done(&self, theme: &Theme) -> Box<dyn Widget> {
        if let Some(err) = &self.error {
            return Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(theme.tokens.spacing.s6)
                    .push(Box::new(
                        Banner::error("Your identity could not be created")
                            .with_message(err.clone()),
                    ))
                    .push(paragraph(
                        "Nothing was written and the desktop is still protected. Go back and try again after checking storage and permissions.",
                        theme,
                    ))
                    .push(actions(vec![back_button()])),
            );
        }
        let minted = self.flow.minted();
        let fingerprint = minted
            .map(super::onboarding::MintedIdentity::grouped_fingerprint)
            .unwrap_or_default();
        let detail = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s2)
            .push(Box::new(
                Text::new(&fingerprint).with_role(ui::tokens::typography::TextRole::Code),
            ))
            .push(Box::new(
                Text::new("Others may use this fingerprint to verify that this identity is yours.")
                    .with_role(ui::tokens::typography::TextRole::Caption)
                    .with_color(theme.text_dim),
            ));
        let backup_fields = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s3)
            .push(Box::new(secret_input(
                "onboarding.backup.passphrase",
                &self.backup_passphrase,
                "New backup passphrase",
                "Long, unique passphrase — 12+ characters",
            )))
            .push(Box::new(secret_input(
                "onboarding.backup.confirmation",
                &self.backup_confirmation,
                "Confirm new backup passphrase",
                "Confirm passphrase",
            )));
        let mut backup = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(theme.tokens.spacing.s3)
            .push(paragraph(
                "Save an encrypted copy to another drive. qOS verifies the finished file before calling it a backup; keep its passphrase somewhere separate.",
                theme,
            ))
            .push(Box::new(backup_fields));
        if let Some(error) = &self.backup_error {
            backup = backup.push(Box::new(
                Banner::error("The backup was not saved").with_message(error.clone()),
            ));
        }
        if let Some(path) = &self.backup_saved {
            backup = backup.push(Box::new(
                Banner::success("Encrypted backup verified")
                    .with_message(path.display().to_string())
                    .with_icon(ui::icons::Icon::Check),
            ));
        }
        let mut backup_button = Button::new(
            "onboarding.backup.button",
            if self.backup_saved.is_some() {
                "Save another copy"
            } else {
                "Save backup…"
            },
            Some(KEY_BACKUP_BROWSE),
        );
        if self.backup_saved.is_none() {
            backup_button = backup_button.with_style(ui::widget::ButtonStyle::Primary);
        }
        backup = backup.push(actions(vec![Box::new(backup_button)]));
        let system_care = match self.context {
            OnboardingContext::FullSystem => {
                "qOS checks signed system updates automatically after setup when the machine is \
                 online. You can check, stage, and roll back releases in Settings."
            }
            OnboardingContext::HostDesktop => {
                "Your Linux distribution remains responsible for OS updates and system time. \
                 This host-session package does not install or run qOS bootc update services."
            }
        };
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(theme.tokens.spacing.s6)
                .push(heading(DONE_TITLE, theme))
                .push(Box::new(
                    Banner::success("Identity stored on this device")
                        .with_icon(ui::icons::Icon::Check),
                ))
                .push(paragraph(DONE_BODY, theme))
                .push(paragraph(
                    "The desktop is ready. Sharing stays off until you turn it on in Settings.",
                    theme,
                ))
                .push(Box::new(Card::titled(
                    "System care",
                    paragraph(system_care, theme),
                )))
                .push(Box::new(Card::titled("Your fingerprint", Box::new(detail))))
                .push(Box::new(Card::titled(
                    "Make a portable backup",
                    Box::new(backup),
                )))
                // Repeated deliberately: the last moment the user is looking at the
                // subject is the one fact they need to carry away from the whole ceremony.
                .push(paragraph(
                    "There is no recovery service. If this device goes and you have no \
                     encrypted portable backup, this identity goes with it.",
                    theme,
                )),
        )
    }

    /// Sticky final action, deliberately outside the Done screen's scrollable
    /// backup content so it remains visible at narrow and large-text sizes.
    fn finish_action() -> Box<dyn Widget> {
        // The tour used to have a complete state machine but no UI route into
        // it. Keep skipping possible, while making the helpful path the clear
        // primary action at the moment setup hands over to the desktop.
        let skip = Button::new("onboarding.finish.button", "Skip tour", Some(KEY_FINISH));
        let tour = Button::new(
            "onboarding.tour.start.button",
            "Start tour",
            Some(KEY_TOUR_START),
        )
        .with_icon(ui::icons::Icon::ArrowRight)
        .with_style(ui::widget::ButtonStyle::Primary);
        actions(vec![Box::new(skip), Box::new(tour)])
    }
}

fn heading(text: &str, _theme: &Theme) -> Box<dyn Widget> {
    Box::new(Text::new(text).with_role(ui::tokens::typography::TextRole::Display))
}

/// Secondary body copy. Takes the live theme rather than a hard-coded
/// one — a dark-theme dim grey on a light background is barely legible.
fn paragraph(text: &str, theme: &Theme) -> Box<dyn Widget> {
    Box::new(
        Text::new(text)
            .with_role(ui::tokens::typography::TextRole::Body)
            .with_color(theme.text_dim),
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
        Button::new("onboarding.back.button", "Back", Some(KEY_BACK))
            .with_icon(ui::icons::Icon::ArrowLeft),
    )
}

/// A right-aligned action row, so the primary button lands where the eye finishes reading.
fn actions(buttons: Vec<Box<dyn Widget>>) -> Box<dyn Widget> {
    let mut row = Row::new().push(Box::new(Spacer::new()));
    for b in buttons {
        row = row.push(b);
    }
    Box::new(row)
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

fn clear_secret(buffer: &Arc<Mutex<String>>) {
    if let Ok(mut value) = buffer.lock() {
        value.zeroize();
    }
}

impl OnboardingView {
    fn draw_backdrop(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let colors = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, theme.bg_app, None, &self.rect);
        if theme.name.eq_ignore_ascii_case("high contrast") {
            return;
        }

        // Quiet, theme-derived depth: two broad accent blooms make the
        // trusted setup surface distinct without becoming illustration.
        let bloom = colors.accent[5].with_alpha_factor(0.10);
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
            colors.accent[3].with_alpha_factor(0.07),
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
        let colors = &theme.tokens.colors;
        let panel = composed.get_rect();
        if panel.area() > 0.0 {
            if !theme.name.eq_ignore_ascii_case("high contrast") {
                ui::surface::soft_shadow(
                    scene,
                    transform,
                    panel,
                    colors.neutral[0],
                    42.0,
                    18.0,
                    0.42,
                );
            }
            let shape = RoundedRect::from_rect(panel, f64::from(theme.tokens.radii.xl));
            scene.fill(
                Fill::NonZero,
                transform,
                colors.surfaces.raised,
                None,
                &shape,
            );
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                colors.border_standard,
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
    let deadline = Instant::now() + Duration::from_millis(750);
    loop {
        let leader_finished = child.try_wait().ok().flatten().is_some();
        // Signal 0 only checks whether any descendant still occupies the
        // dedicated group. Do not return just because pkexec itself exited:
        // qos-home-peer-control or qos-enroll may still be winding down.
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
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let before = self.flow.screen();
        let tour_before = self.tour_page;

        // Pull widget-local edits (notably passphrases) into their process-local
        // buffers before interpreting action keys from the same input frame.
        if let Some(c) = &mut self.composed {
            c.sync(state, root);
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
                    FilePicker::new("Choose a qOS identity", initial, roots).with_filters(vec![
                        ui::widget::file_picker::FileFilter::new(
                            "qOS identity",
                            &["qos-identity", "qos-identity-backup", "json"],
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
                            std::env::var("HOSTNAME").unwrap_or_else(|_| "qOS device".into());
                        let identity_root = std::env::var_os("QOS_IDENTITY_ROOT")
                            .map(PathBuf::from)
                            .unwrap_or_else(|| PathBuf::from("/var/lib/qos"));
                        let requester = qos_relay::ProcessLease::capture(std::process::id());
                        let spawn =
                            requester
                                .map_err(|error| error.to_string())
                                .and_then(|lease| {
                                    Command::new("pkexec")
                                        .args([
                                            "/usr/libexec/qos-home-peer-control",
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
                                        // pkexec, the privileged broker and qos-enroll must
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
                                self.nearby_status =
                                    Some("Looking for a trusted device nearby…".into());
                                self.error = None;
                            }
                            Err(error) => {
                                self.error = Some(format!("Could not start nearby setup: {error}"))
                            }
                        }
                    }
                    _ => self.error = Some("Nearby setup is unavailable in this session".into()),
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
            self.nearby_status =
                Some("Nearby setup cancelled. Choose Connect nearby to try again.".into());
            self.invalidation.structure();
        }

        if let Some(path) = &self.nearby_status_path
            && let Ok(bytes) = std::fs::read(path)
            && let Ok(status) = serde_json::from_slice::<serde_json::Value>(&bytes)
            && let Some(code) = status
                .get("verification_code")
                .and_then(serde_json::Value::as_str)
        {
            let message = format!("Code {code} · waiting for approval on your trusted device");
            if self.nearby_status.as_deref() != Some(&message) {
                self.nearby_status = Some(message);
                self.invalidation.structure();
            }
        }
        let nearby_finished = self
            .nearby_child
            .as_mut()
            .and_then(|child| child.try_wait().ok().flatten());
        if let Some(status) = nearby_finished {
            let mut child = self.nearby_child.take().expect("nearby child exists");
            let mut stdout = String::new();
            let mut stderr = String::new();
            if let Some(mut stream) = child.stdout.take() {
                let _ = stream.read_to_string(&mut stdout);
            }
            if let Some(mut stream) = child.stderr.take() {
                let _ = stream.read_to_string(&mut stderr);
            }
            if status.success() {
                match serde_json::from_str::<serde_json::Value>(stdout.lines().last().unwrap_or_default()) {
                    Ok(result) => {
                        let fingerprint = result.get("identity").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
                        let path = result.get("record_path").and_then(serde_json::Value::as_str).map(PathBuf::from);
                        if fingerprint.len() == 64 && path.is_some() {
                            self.flow.complete_connected(MintedIdentity { fingerprint, path: path.expect("checked") });
                            let _ = self.persist_progress(PersistedStage::IdentityReady);
                            self.error = None;
                            self.nearby_status = Some("Connected. Your workspaces will appear automatically.".into());
                        } else {
                            self.error = Some("The trusted device returned an invalid identity. Choose Connect nearby to retry.".into());
                        }
                    }
                    Err(_) => self.error = Some("The trusted device returned an invalid identity. Choose Connect nearby to retry.".into()),
                }
            } else {
                let detail = stderr
                    .lines()
                    .last()
                    .unwrap_or("the pairing window closed or approval was cancelled");
                self.error = Some(format!(
                    "Nearby setup did not finish: {detail}. Choose Connect nearby to retry."
                ));
            }
            self.invalidation.structure();
        }

        if read_bool(state, root, KEY_BACKUP_BROWSE) {
            let _ = state.put(root, KEY_BACKUP_BROWSE, ScalarValue::Boolean(false));
            if self.flow.screen() == Screen::Done && self.picker.is_none() {
                self.backup_error = match (
                    self.backup_passphrase.lock(),
                    self.backup_confirmation.lock(),
                ) {
                    (Ok(passphrase), Ok(_)) if passphrase.chars().count() < 12 => Some(
                        "Use at least 12 characters. Make it long and unique; length alone does not make a common passphrase safe."
                            .to_string(),
                    ),
                    (Ok(passphrase), Ok(confirmation)) if *passphrase != *confirmation => {
                        Some("The two passphrases do not match.".to_string())
                    }
                    (Ok(_), Ok(_)) => None,
                    _ => Some("The passphrase fields are temporarily unavailable.".to_string()),
                };
                if self.backup_error.is_none() {
                    let (initial, roots) = onboarding_picker_scope();
                    self.picker = Some((
                        PickerPurpose::BackupIdentity,
                        FilePicker::new("Save encrypted identity backup", initial, roots)
                            .with_mode(FilePickerMode::Save)
                            .with_filters(vec![ui::widget::file_picker::FileFilter::new(
                                "Encrypted qOS identity",
                                &["qos-identity"],
                            )])
                            .with_filename("qos-identity-backup.qos-identity"),
                    ));
                    self.invalidation.focus();
                } else {
                    self.invalidation.structure();
                }
            }
        }

        if read_bool(state, root, KEY_BACK) {
            let _ = state.put(root, KEY_BACK, ScalarValue::Boolean(false));
            self.error = None;
            clear_secret(&self.connect_passphrase);
            terminate_nearby_pairing(&mut self.nearby_child);
            self.nearby_status = None;
            self.flow.back();
        }

        if read_bool(state, root, KEY_IMPORT) {
            let _ = state.put(root, KEY_IMPORT, ScalarValue::Boolean(false));
            let source = PathBuf::from(read_string(state, root, KEY_CONNECT_PATH));
            if self.flow.screen() == Screen::Connect
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
                        let _ = self.persist_progress(PersistedStage::IdentityReady);
                        clear_secret(&self.connect_passphrase);
                    }
                    Err(error) => {
                        tracing::error!(target: "qos::onboarding", %error, "identity import failed");
                        self.error = Some(error);
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

        if read_bool(state, root, KEY_CREATE) {
            let _ = state.put(root, KEY_CREATE, ScalarValue::Boolean(false));
            // `can_create` is checked inside `complete` too — this is the outer guard, that one is the invariant.
            if self.flow.can_create() && self.persist_progress(PersistedStage::AcquiringIdentity) {
                self.error = None;
                match (self.minter)() {
                    Ok(minted) => {
                        self.flow.complete(minted);
                        let _ = self.persist_progress(PersistedStage::IdentityReady);
                    }
                    Err(e) => {
                        tracing::error!(target: "qos::onboarding", error = %e, "identity minting failed");
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
            if self.flow.is_done() && self.persist_progress(PersistedStage::TourNavigate) {
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
            };
            let progress_stage = match next {
                None => Some(PersistedStage::IdentityReady),
                Some(TourPage::Navigate) => Some(PersistedStage::TourNavigate),
                Some(TourPage::Organise) => Some(PersistedStage::TourOrganise),
                Some(TourPage::Collaborate) => Some(PersistedStage::TourCollaborate),
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
                page => page,
            };
            let progress_stage = match next {
                Some(TourPage::Navigate) => Some(PersistedStage::TourNavigate),
                Some(TourPage::Organise) => Some(PersistedStage::TourOrganise),
                Some(TourPage::Collaborate) => Some(PersistedStage::TourCollaborate),
                None => None,
            };
            if self.tour_only
                || progress_stage.is_none_or(|progress| self.persist_progress(progress))
            {
                self.tour_page = next;
            }
        }

        if read_bool(state, root, KEY_FINISH) {
            let _ = state.put(root, KEY_FINISH, ScalarValue::Boolean(false));
            self.finished =
                self.tour_only || (self.flow.is_done() && self.finish_persisted_progress());
            if self.finished {
                clear_secret(&self.connect_passphrase);
                clear_secret(&self.backup_passphrase);
                clear_secret(&self.backup_confirmation);
            }
        }

        if self.flow.screen() != before || self.tour_page != tour_before {
            self.transition_direction = if view_order(self.flow.screen(), self.tour_page)
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
        let mut composed = self.build_composition(theme, PANEL_MAX_HEIGHT);
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);

        // Keep a readable measure on large displays and shrink all the way to
        // a phone-like viewport. The body scrolls vertically when its copy is
        // taller than the bounded setup surface.
        if let Ok(mut cs) = taffy.style(child).cloned() {
            cs.size.width = taffy::Dimension::percent(1.0);
            cs.size.height = taffy::Dimension::percent(1.0);
            cs.max_size.width = taffy::Dimension::length(PANEL_MAX_WIDTH);
            cs.max_size.height = taffy::Dimension::length(PANEL_MAX_HEIGHT);
            cs.flex_grow = 0.0;
            cs.flex_shrink = 1.0;
            let _ = taffy.set_style(child, cs);
        }

        let style = taffy::Style {
            display: taffy::Display::Flex,
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

    #[allow(clippy::too_many_arguments)]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let Some((_, picker)) = self.picker.as_mut() {
            picker.set_rect(onboarding_picker_rect(self.rect));
            let mut picker_taffy = TaffyTree::<()>::new();
            if let Ok(picker_node) = picker_taffy.new_leaf(taffy::Style::default()) {
                picker.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    picker_node,
                    &picker_taffy,
                );
            }
            *handled = true;
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
            let _ = state.put(path, KEY_FINISH, ScalarValue::Boolean(true));
            *handled = true;
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
            let _ = state.put(path, KEY_BACK, ScalarValue::Boolean(true));
            *handled = true;
            return;
        }
        if let Some(c) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(&cn) = children.first()
        {
            c.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                cn,
                taffy,
            );
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

    fn needs_animation_frame(&self) -> bool {
        (!self.reduce_motion && self.transition_started.elapsed() < self.transition_duration)
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

    fn view() -> (OnboardingView, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let v = OnboardingView::with_minter(Box::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(MintedIdentity {
                fingerprint: "ab".repeat(32),
                path: PathBuf::from("/home/user/.config/qos/identity/primary.json"),
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
        let deadline = Instant::now() + Duration::from_secs(1);
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
            1.0,
            &Modifiers::default(),
            document,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
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
        atomic_file::write_with_mode(
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
    fn journal_is_durable_before_identity_action_and_removed_only_after_finish() {
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

        press(&mut document, KEY_FINISH);
        view.sync(&mut document, &automerge::ROOT);
        assert!(view.finished());
        assert!(!has_pending_progress(&identity_dir));
    }

    #[test]
    fn restart_resumes_done_and_exact_tour_page_without_recreating_identity() {
        let directory = TempDir::new().expect("progress directory");
        write_fixture_identity(directory.path());
        store_progress(directory.path(), PersistedStage::IdentityReady).expect("ready journal");

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
        press(&mut document, KEY_FINISH);
        collaborate_restart.sync(&mut document, &automerge::ROOT);
        assert!(collaborate_restart.finished());
        assert!(!has_pending_progress(directory.path()));

        let established = OnboardingView::new(directory.path().to_path_buf());
        assert_eq!(
            established.flow().screen(),
            Screen::Intro,
            "without a pending journal startup leaves an established user alone"
        );
    }

    #[test]
    fn unsupported_or_unsafe_journal_stays_pending_instead_of_bypassing_setup() {
        let directory = TempDir::new().expect("progress directory");
        atomic_file::write_with_mode(
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

    #[test]
    fn a_journal_write_failure_prevents_identity_creation() {
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

    #[test]
    fn structure_and_focus_invalidations_are_consumed_independently() {
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

    /// The whole point: clicking through without ticking the box must not produce a key.
    #[test]
    fn pressing_create_without_acknowledging_mints_nothing() {
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

    #[test]
    fn acknowledging_then_creating_mints_exactly_once() {
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

    #[test]
    fn unticking_the_box_closes_the_gate_again() {
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

    /// A minting failure must be shown, not swallowed into a desktop that silently has no identity.
    #[test]
    fn a_failed_mint_is_surfaced_rather_than_hidden() {
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

    #[test]
    fn finishing_releases_the_desktop() {
        let (mut v, _) = view();
        let mut doc = AutoCommit::new();
        assert!(!v.finished());
        press(&mut doc, KEY_BEGIN);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_UNDERSTOOD);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_CREATE);
        v.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_FINISH);
        v.sync(&mut doc, &automerge::ROOT);
        assert!(v.finished());
    }

    #[test]
    fn desktop_tour_is_complete_backtrackable_and_finishes_only_at_the_end() {
        let (mut view, _) = view();
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_BEGIN);
        view.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_UNDERSTOOD);
        view.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_CREATE);
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

        press(&mut doc, KEY_TOUR_BACK);
        view.sync(&mut doc, &automerge::ROOT);
        assert_eq!(view.tour_page, Some(TourPage::Organise));
        press(&mut doc, KEY_TOUR_NEXT);
        view.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_FINISH);
        view.sync(&mut doc, &automerge::ROOT);
        assert!(view.finished());
    }

    #[test]
    fn every_tour_page_exposes_an_explicit_skip_action() {
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

        press(&mut doc, KEY_FINISH);
        view.sync(&mut doc, &automerge::ROOT);
        assert!(view.finished(), "Skip tour dismisses a reopened tour");
    }

    #[test]
    fn reopened_tour_has_no_identity_gate_and_can_be_dismissed() {
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
        press(&mut doc, KEY_FINISH);
        view.sync(&mut doc, &automerge::ROOT);
        assert!(view.finished(), "Done closes the reopened tour");
    }

    #[test]
    fn desktop_tour_teaches_the_real_keyboard_and_gesture_contract() {
        let navigation = NAVIGATE_CARDS
            .iter()
            .flat_map(|(title, body)| [*title, *body])
            .collect::<Vec<_>>()
            .join(" ");

        assert!(navigation.contains("Super+K"));
        assert!(navigation.contains("Super+Space"));
        assert!(navigation.contains("Start here, Everyday, Work, Media, and System tools"));
        assert!(navigation.contains("Ctrl+Tab"));
        assert!(navigation.contains("four-finger swipe"));
        assert!(navigation.contains("Super+arrow keys"));
        assert!(navigation.contains("three-finger swipe"));
        assert!(navigation.contains("Up and down switch workspaces"));
        assert!(navigation.contains("Left and right step between tiled windows"));
        assert!(
            !navigation.contains("Super+W"),
            "Super+W closes the focused tab; teaching it as Overview would be destructive"
        );

        assert_eq!(
            ORGANISE_CARDS
                .iter()
                .map(|(title, _)| *title)
                .collect::<Vec<_>>(),
            ["Workspace", "Strip", "Window group", "Tab"],
            "the tour must name every level of the desktop hierarchy"
        );
    }

    #[test]
    fn onboarding_secret_fields_have_stable_spoken_names() {
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

    #[test]
    fn nearby_pairing_updates_share_one_polite_live_region() {
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

    #[test]
    fn resumed_tour_preserves_focus_reset_and_reduced_motion_navigation() {
        let directory = TempDir::new().expect("progress directory");
        write_fixture_identity(directory.path());
        store_progress(directory.path(), PersistedStage::TourOrganise).expect("organise journal");
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

    #[test]
    fn host_desktop_context_keeps_the_identity_creation_gate() {
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

    #[test]
    fn host_desktop_copy_does_not_claim_to_replace_the_linux_login() {
        let copy = HOST_DESKTOP_INTRO_BODY.to_ascii_lowercase();
        assert!(copy.contains("already signed in"));
        assert!(copy.contains("does not replace your linux login"));
        assert!(copy.contains("does not") && copy.contains("online account"));
        assert!(copy.contains("not all cryptographically signed"));
    }

    #[test]
    fn screen_transitions_track_forward_and_back_direction() {
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

    #[test]
    fn escape_follows_back_on_reversible_identity_steps_only() {
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

    #[test]
    fn reduced_motion_does_not_request_transition_frames() {
        let (mut view, _) = view();
        view.reduce_motion = true;
        view.composed = None;
        view.picker = None;
        assert!(!view.needs_animation_frame());
    }

    #[test]
    fn connect_existing_imports_without_minting() {
        let imported = Arc::new(AtomicUsize::new(0));
        let imported_for_handler = imported.clone();
        let mut view = OnboardingView::with_handlers(
            Box::new(|| Err("create should not run".to_string())),
            Box::new(move |path, _| {
                imported_for_handler.fetch_add(1, Ordering::SeqCst);
                assert_eq!(path, &PathBuf::from("/media/backup/primary.json"));
                Ok(MintedIdentity {
                    fingerprint: "cd".repeat(32),
                    path: PathBuf::from("/home/user/.config/qos/identity/cd.json"),
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

    #[test]
    fn connect_journals_before_import_and_keeps_setup_pending_afterward() {
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

    #[test]
    fn failed_connect_stays_on_connect_screen_and_shows_error() {
        let mut view = OnboardingView::with_handlers(
            Box::new(|| Err("create should not run".to_string())),
            Box::new(|_, _| Err("not a qOS identity".to_string())),
        );
        let mut doc = AutoCommit::new();
        press(&mut doc, KEY_CONNECT);
        view.sync(&mut doc, &automerge::ROOT);
        press(&mut doc, KEY_IMPORT);
        view.sync(&mut doc, &automerge::ROOT);
        assert_eq!(view.flow().screen(), Screen::Connect);
        assert_eq!(view.error(), Some("not a qOS identity"));
    }

    #[test]
    fn connect_browse_opens_a_shell_owned_modal_picker() {
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

    #[test]
    fn portable_backup_requires_matching_strong_passphrases_and_reports_success() {
        let exports = Arc::new(AtomicUsize::new(0));
        let exports_for_handler = Arc::clone(&exports);
        let mut view = OnboardingView::with_all_handlers(
            Box::new(|| Err("unused".to_string())),
            Box::new(|_, _| Err("unused".to_string())),
            Box::new(move |identity, destination, passphrase| {
                assert_eq!(identity, &PathBuf::from("/identity/primary.json"));
                assert_eq!(
                    destination.file_name().and_then(|name| name.to_str()),
                    Some("qos-identity-backup.qos-identity")
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
        view.picker.as_mut().expect("save picker").1.click_open();
        view.sync(&mut doc, &automerge::ROOT);
        assert_eq!(exports.load(Ordering::SeqCst), 1);
        assert!(view.backup_saved.is_some());
        assert!(view.backup_error.is_none());
        assert!(view.backup_passphrase.lock().unwrap().is_empty());
        assert!(view.backup_confirmation.lock().unwrap().is_empty());
    }

    /// Every screen must lay out — a ceremony that panics on the screen after the one under test is worse than no ceremony.
    #[test]
    fn every_screen_builds_a_layout() {
        let (mut v, _) = view();
        let mut doc = AutoCommit::new();
        for step in [
            None,
            Some(KEY_BEGIN),
            Some(KEY_UNDERSTOOD),
            Some(KEY_CREATE),
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

    #[test]
    fn narrow_layout_keeps_the_setup_surface_inside_the_viewport() {
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

    #[test]
    fn done_exit_action_stays_visible_at_laptop_height_with_large_text() {
        let (mut view, _) = view();
        let mut doc = AutoCommit::new();
        for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE] {
            press(&mut doc, key);
            view.sync(&mut doc, &automerge::ROOT);
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

        for id in ["onboarding.finish.button", "onboarding.tour.start.button"] {
            let action = node_map
                .values()
                .find_map(|&ptr| unsafe {
                    ((*ptr).test_id() == Some(id)).then(|| (*ptr).get_rect())
                })
                .unwrap_or_else(|| panic!("Done screen exposes {id}"));
            assert!(action.area() > 0.0, "action={action:?}");
            assert!(
                action.x0 >= 0.0 && action.x1 <= 1000.0 && action.y0 >= 0.0 && action.y1 <= 720.0,
                "the action must stay fully in the viewport, id={id}, action={action:?}"
            );
        }
    }

    #[test]
    fn tour_navigation_stays_visible_on_every_page_at_narrow_large_text_size() {
        const WIDTH: f32 = 420.0;
        const HEIGHT: f32 = 640.0;

        for next_presses in 0..=2 {
            let (mut view, _) = view();
            let mut doc = AutoCommit::new();
            for key in [KEY_BEGIN, KEY_UNDERSTOOD, KEY_CREATE, KEY_TOUR_START] {
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
                    .all(|pair| pair[0].x1 <= pair[1].x0 && (pair[0].y0 - pair[1].y0).abs() < 1.0),
                "compact tour actions must remain one ordered row, page={next_presses}, rects={action_rects:?}"
            );
        }
    }
}
