//! Host-side installer journey with explicitly granted, device-local data transfers.
//!
//! Kept behind --installer-preview until identity handoff and
//! installation qualification can fulfill this journey. Samples never enter the
//! live setup or its privileged workers. Every interaction is also an effect.

mod accounts;
mod belongings;
pub(crate) mod browser;
mod capabilities;
mod demo;
mod desktop;
mod diagram;
mod direct;
mod draft;
mod existing_identity;
#[cfg(target_os = "linux")]
mod live_inventory;
#[cfg(all(test, target_os = "linux"))]
pub(super) mod main_story_tests;
mod migration_selection;
mod model;
mod native_ownership;
mod pairing;
pub(in crate::setup) mod personal;
#[path = "portable_source/presentation.rs"]
pub(crate) mod portable_source;
#[cfg(target_os = "linux")]
#[path = "portable_source.rs"]
mod portable_source_native;
#[path = "prepared_media/presentation.rs"]
pub(crate) mod prepared_media;
#[cfg(target_os = "linux")]
#[path = "prepared_media.rs"]
mod prepared_media_native;
mod promotion;
mod recovery;
mod restart_controller;
#[cfg(all(test, target_os = "linux"))]
mod restart_tests;
mod retained_preview;
mod service_sessions;
#[cfg(all(test, target_os = "linux"))]
mod source_change_tests;
mod storage;
mod transfer;
#[cfg(test)]
mod visual_tests;
mod windows;

#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
pub(in crate::setup) fn paint_native_flatpak_inventory(
    home: &std::path::Path,
    inventory: &super::flatpak_transfer::Inventory,
) {
    visual_tests::paint_native_flatpak_inventory(home, inventory);
}

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::animation::{Animated, Curve};
use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{ParagraphAlign, TextRenderer};
use ui::theme::Theme;
use ui::tilo_resource::{self, ActionRef, Effect};
use ui::tokens::{Rhythm, TextRole};
use ui::widget::{
    AppModel, Avatar, AvatarSize, Button, ButtonRole, Card, Checkbox, Container, Field, IconWidget,
    LabelPlacement, ListRow, ModelPath, NumberInput, Row, Scrollable, Spacer, Text, TextInput,
    TextInputKind, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::WindowEvent;

use super::strings::{t, t_args};
use crate::page;
use crate::resource::Imager;
use model::{Destination, Identity, Journey, Page};

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "imager.installer-preview", device_only)]
pub enum InstallerPreviewEffect {
    /// Read the preview's choices, excluding secrets.
    #[effect(returns, kind = "read")]
    ReadPreview,
    /// Query native Windows root metadata without granting any folder.
    #[effect(returns)]
    FindWindowsRoots,
    /// Select an exact folder through the platform picker.
    #[effect(returns)]
    ChooseWindowsRoot { role: String },
    /// Independently grant one exact Windows root, including redirected libraries.
    #[effect(returns)]
    SelectWindowsRoot {
        #[param(secret, "Absolute independently selected directory")]
        source: String,
        role: String,
    },
    /// Open the exact granted source folder in the platform file manager.
    #[effect(returns, view)]
    OpenWindowsSource { source_id: String },
    /// Show or hide actual inventoried paths for one source category.
    #[effect(returns, view)]
    ShowWindowsCategory {
        source_id: String,
        category: String,
        expanded: bool,
    },
    /// Decide whether an inventoried root is copied, retained, or excluded.
    #[effect(returns)]
    ReviewWindowsRoot {
        source_id: String,
        decision: String,
        reason: Option<String>,
    },
    /// Decide how one inventoried category is preserved.
    #[effect(returns)]
    ReviewWindowsCategory {
        source_id: String,
        category: String,
        decision: String,
        reason: Option<String>,
    },
    /// Set the explicit reason used by exclusion controls.
    #[effect(returns)]
    SetWindowsExclusionReason { value: String },
    /// Set the private encryption password for offline sign-in suggestions.
    #[effect(returns)]
    SetWindowsPreparationPassword {
        #[param(secret, "Private export password, not a sign-in credential")]
        value: String,
    },
    /// Choose the mounted backup destination using the platform picker.
    #[effect(returns)]
    ChooseWindowsExport,
    /// Export verified local copies and encrypted suggestions to a granted directory.
    #[effect(returns)]
    ExportWindowsPreparation {
        #[param(
            secret,
            "Absolute selected backup destination, separate from the image target"
        )]
        destination: String,
    },
    /// Continue an unfinished preparation after verifying its exact identity custody.
    #[effect(returns)]
    ResumeInstallerPreparation,
    /// Retire the saved choices without deleting previous identity custody.
    #[effect(returns)]
    StartNewInstallerPreparation,
    /// Check retained preparation again; no identity is adopted by this action.
    #[effect(returns)]
    RetryInstallerPreparation,
    /// Choose your avatar with the system file picker.
    #[effect(returns)]
    ChoosePreviewAvatar,
    /// Select an already granted local avatar file without opening a picker.
    #[effect(returns)]
    SelectPreviewAvatar {
        #[param(secret, "Absolute selected image file")]
        source_file: String,
    },
    /// Choose the existing identity backup through the system file picker.
    #[effect(returns)]
    ChooseExistingIdentityBackup,
    /// Select an already granted backup file without opening a picker.
    #[effect(returns)]
    SelectExistingIdentityBackup {
        #[param(secret, "Absolute selected identity backup")]
        source_file: String,
    },
    /// Enter the backup passphrase in private device-local memory.
    #[effect(returns)]
    SetExistingIdentityPassword {
        #[param(secret, "Backup passphrase")]
        value: String,
    },
    /// Ask a nearby Tilo device to approve this named future device.
    #[effect(returns)]
    StartExistingDevicePairing,
    /// Stop local pairing; this does not revoke remotely approved enrollment.
    #[effect(returns)]
    CancelExistingDevicePairing,
    /// Choose an option on this page.
    #[effect(returns)]
    ChoosePreview {
        #[param(
            "Choice: computer, another, desktop; migrate, fresh; new, existing; device, backup"
        )]
        choice: String,
    },
    /// Set your display name.
    #[effect(returns)]
    SetPreviewName { value: String },
    /// Set this device's name.
    #[effect(returns)]
    SetPreviewDeviceName { value: String },
    /// Enter the destination unlock password, in private memory only.
    #[effect(returns)]
    SetPreviewPassword {
        #[param(secret, "Unlock password; never persisted or returned")]
        value: String,
    },
    /// Confirm the destination unlock password.
    #[effect(returns)]
    SetPreviewConfirmation {
        #[param(secret, "Unlock confirmation; never persisted or returned")]
        value: String,
    },
    /// Create the chosen new identity in fresh, durable private custody.
    #[effect(returns)]
    CreatePreparedIdentity {
        private_custody: String,
        avatar_color: String,
    },
    /// Update the prepared identity's public profile, retaining its signing key.
    #[effect(returns)]
    UpdatePreparedIdentity { avatar_color: String },
    /// Export that identity encrypted with the chosen destination password.
    #[effect(returns)]
    ExportPreparedIdentity { destination: String },
    /// Export selected prepared accounts in an encrypted, operation-bound artifact.
    #[effect(returns)]
    ExportPreparedAccounts {
        destination: String,
        setup_binding: String,
    },
    /// Package the chosen identity and selected accounts for installation.
    #[effect(returns)]
    ExportPreparedSetup {
        destination: String,
        setup_binding: String,
    },
    /// Export the encrypted, source-checked preparation without installing it.
    #[effect(returns)]
    ExportPreparedBelongings {
        #[param(secret, "Absolute private destination for the encrypted preparation")]
        destination: String,
    },
    /// Include a migration category.
    #[effect(returns)]
    SetPreviewCategory {
        #[param("Category: files, flatpak, accounts")]
        category: String,
        included: bool,
    },
    /// Include one discovered application in the selected migration plan.
    #[effect(returns)]
    SetPreviewFlatpakApp { app: String, included: bool },
    /// Include an explicitly selected website sign-in source.
    #[effect(returns)]
    SetPreviewServiceSession {
        source_id: String,
        service: String,
        included: bool,
    },
    /// Expand or collapse migration details.
    #[effect(returns)]
    TogglePreviewDetails,
    /// Show or hide the selected file source’s full location.
    #[effect(returns)]
    ToggleMigrationSourceLocation,
    /// Expand or collapse the selected apps’ custom access details.
    #[effect(returns)]
    ToggleFlatpakAccessDetails,
    /// Perform a decision in the account connection flow.
    #[effect(returns)]
    AccountStep {
        step: String,
        selection: Option<String>,
    },
    /// Edit one private guided account field; credentials never enter shared state.
    #[effect(returns)]
    SetAccountField {
        field: String,
        #[param(
            secret,
            "Private account field; never persisted in shared state or returned"
        )]
        value: String,
    },
    /// Import one reviewed reusable connection into an explicitly chosen target home.
    #[effect(returns)]
    ImportAccount {
        candidate: String,
        target_home: String,
    },
    /// Begin a provider's consent flow; returns its public consent URL.
    #[effect(returns)]
    ConnectAccount {
        provider: String,
        candidate: Option<String>,
        target_home: String,
    },
    /// Complete the consent flow using its private authorization handle.
    #[effect(returns)]
    CompleteAccountConnection { connection: String },
    /// Close an unfinished consent flow.
    #[effect(returns)]
    CancelAccountConnection { connection: String },
    /// Review exact source, target home and private journal grants. Does not copy.
    /// Retained copy receipts are revalidated after restarting the installer.
    #[effect(returns)]
    ReviewUserTransfer {
        source: String,
        destination: String,
        journals: String,
    },
    /// Grant the reviewed home directory for migration without opening a picker.
    #[effect(returns)]
    SelectMigrationHome {
        #[param(secret, "Explicit native home folder grant")]
        source: String,
    },
    /// Request a real platform grant for one discovered mounted Home location.
    #[effect(returns)]
    GrantMountedHomeLocation { location: String },
    /// Reattest all approved locations and retain original-source permission.
    #[effect(returns)]
    ContinueMountedHomeReview,
    /// Retire the active mounted-location chooser without accepting a late answer.
    #[effect(returns)]
    CancelMountedHomePermission,
    /// Re-discover mounted locations using the retained original Home grant.
    #[effect(returns)]
    RestartMountedHomeReview,
    /// Save consent and stable Linux source identities for installation; no copy or mount.
    #[effect(returns)]
    SaveMigrationPlan {
        source: String,
        private_plans: String,
        operation: String,
    },
    /// Mount the retained Linux source read-only and review exact target/journal grants.
    /// Retained copy receipts are revalidated after restarting the installer.
    #[effect(returns)]
    ReviewInstalledTransfer {
        private_plans: String,
        operation: String,
        destination: String,
        journals: String,
        /// Retained private original Flatpak inventory, when apps were selected.
        flatpak_inventory: Option<String>,
    },
    /// Execute the reviewed data transfer; does not install or boot an OS.
    #[effect(returns)]
    StartUserTransfer,
    /// Retry this reviewed transfer using its durable receipts.
    #[effect(returns)]
    RetryUserTransfer,
    /// Review adoption of this completed whole-home copy into the user's existing home.
    #[effect(returns)]
    ReviewHomePromotion {
        #[param(secret, "Explicit target user home grant")]
        target_home: String,
        #[param(secret, "Private durable adoption journal grant")]
        journal: String,
    },
    /// Adopt the reviewed copy without replacing existing files or selected identity.
    #[effect(returns)]
    StartHomePromotion,
    /// Resume this same reviewed adoption using its durable receipts.
    #[effect(returns)]
    RetryHomePromotion,
    /// Read a bounded page of this retained adoption result, including raw file names.
    #[effect(returns, kind = "read")]
    ReadHomePromotionResults {
        category: String,
        offset: u64,
        limit: u64,
    },
    /// Stop adoption and retain imported items and receipts for retry.
    #[effect(returns)]
    CancelHomePromotion,
    /// Stop the current transfer, retaining verified copies for retry.
    #[effect(returns)]
    CancelUserTransfer,
    /// Recheck belongings after migration consent, without copying or signing in.
    #[effect(returns)]
    CheckBelongings,
    /// Choose a recovery directory using the ordinary folder picker for an exact Windows root.
    #[effect(returns)]
    ChooseRecoveryDestination { source_id: String },
    /// Select an exact recovery directory without opening a picker; creates a private durable receipt directory beside the output.
    #[effect(returns)]
    SelectRecoveryDirectory {
        source_id: String,
        directory: String,
    },
    /// Select an exact external recovery directory and private journal for a granted Windows root.
    #[effect(returns)]
    ReviewRecoveryDestination {
        source_id: String,
        directory: String,
        journal: String,
    },
    /// Reattach exact source, saved output and journal grants after interruption or restart.
    #[effect(returns)]
    ReviewExistingRecoveryCopy {
        source_id: String,
        output: String,
        journal: String,
    },
    /// Begin the reviewed durable recovery copy without deleting its source.
    #[effect(returns)]
    StartRecoveryCopy,
    /// Resume the reviewed recovery output, rechecking hashes and source identity.
    #[effect(returns)]
    RetryRecoveryCopy,
    /// Stop recovery and keep verified partial output for explicit retry.
    #[effect(returns)]
    CancelRecoveryCopy,
    /// Inspect this computer's disk again. Never writes to disk.
    #[effect(returns)]
    CheckStorage,
    /// Show or hide the inspected disk allocation breakdown.
    #[effect(returns)]
    ToggleStorageDetails,
    /// Choose the space reserved for Tilo, within the inspected disk's bounds.
    #[effect(returns)]
    SetStorageSize { gib: u64 },
    /// Open the real USB media writer. Choosing and confirming a drive happens there.
    #[effect(returns)]
    PrepareUsb,
    /// Renew the selected home review after its contents changed; preserve identity and accounts.
    #[effect(returns)]
    ReviewChangedFiles,
    /// Stop preparation while retaining the chosen identity and account selection.
    #[effect(returns)]
    CancelMediaPreparation,
    /// Prepare reviewed identity, accounts and original-source consent before choosing installation media.
    #[effect(returns)]
    PrepareBelongings,
    /// Review actual system capacity and bind the prepared selected Apps
    /// declaration without admitting or starting installation.
    #[effect(device_only, returns)]
    ReviewPreparedBelongings { target_gib: u64 },

    /// Prepare the reviewed installation on this computer, without writing its disk.
    #[effect(returns)]
    PrepareDirectInstallation,
    /// Prepare the current user’s Tilo Desktop setup.
    #[effect(returns)]
    PrepareDesktopInstallation,
    /// Change a section of the reviewed setup.
    #[effect(returns)]
    EditPreview { section: String },
    /// Continue to the next relevant page.
    #[effect(returns)]
    ContinuePreview,
    /// Return to the preceding page.
    #[effect(returns)]
    BackInPreview,
    /// Start the preview again.
    #[effect(returns)]
    RestartPreview,
}

/// Public app names and the actual selected export decision.
#[derive(serde::Serialize)]
struct FlatpakAppChoice {
    id: String,
    name: String,
    selectable: bool,
    included: bool,
    with_home: bool,
    already_here: bool,
}

const CATEGORIES: [&str; 3] = ["files", "flatpak", "accounts"];
fn identity_color(value: &str) -> Result<[u8; 3], &'static str> {
    hex::decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or("installer-preview-identity-invalid-color")
}
pub const SCENARIOS: &[&str] = &[
    "installer-destination",
    "installer-migration",
    "installer-identity",
    "installer-profile",
    "installer-password",
    "installer-connect",
    "installer-desktop-connect",
    "installer-devices",
    "installer-purpose",
    "installer-belongings",
    "installer-accounts",
    "installer-belongings-details",
    "installer-storage",
    "installer-usb",
    "installer-desktop",
    "installer-review",
    "installer-ready",
];

type Commands = Rc<RefCell<Vec<InstallerPreviewEffect>>>;

fn queue_continue(commands: &Commands) {
    let mut pending = commands.borrow_mut();
    if !pending
        .iter()
        .any(|effect| matches!(effect, InstallerPreviewEffect::ContinuePreview))
    {
        pending.push(InstallerPreviewEffect::ContinuePreview);
    }
}

pub fn view(cx: &mut ViewCx<Imager>) -> ui::widget::Element {
    let media = cx.local(false);
    let prepared = cx.local(None::<std::sync::Arc<prepared_media::PreparedMedia>>);
    let navigation = prepared_media::Navigation {
        artifact: prepared.clone(),
        showing_media: media.clone(),
        reason: cx.local(None::<prepared_media::ReturnReason>),
    };
    let suspended = Rc::new(RefCell::new(None::<InstallerPreview>));
    let shown = media.clone();
    cx.switch(&shown, move |cx, showing_media| {
        if *showing_media {
            let app = crate::app::ImagerApp::new().with_windows_preparation(
                suspended
                    .borrow()
                    .as_ref()
                    .and_then(|preview| preview.windows.media_handoff()),
            );
            let app = app
                .with_prepared_media(prepared.get())
                .with_preparation_navigation(navigation.clone())
                .with_original_source(suspended.borrow().as_ref().and_then(|preview| {
                    (preview.journey.destination == Some(Destination::Another)
                        && preview.journey.migrate == Some(true)
                        && (preview.journey.categories[0] || preview.selected_applications()))
                    .then(|| preview.portable_source.reviewed_original())
                    .flatten()
                }));
            cx.surface(app)
        } else {
            let previous = suspended.borrow_mut().take();
            let returned = previous.is_some();
            let preview = previous.unwrap_or_else(|| InstallerPreview {
                media: Some(media.clone()),
                media_bundle: Some(prepared.clone()),
                media_return: Some(navigation.reason.clone()),
                ..InstallerPreview::default()
            });
            cx.surface(retained_preview::RetainedPreview::new(
                preview, &suspended, returned,
            ))
        }
    })
}

struct OriginalAvatar(Option<String>);

pub struct InstallerPreview {
    windows: windows::Preparation,
    journey: Journey,
    linux: bool,
    capabilities: capabilities::GuidedCapabilities,
    commands: Commands,
    composed: Option<Box<dyn Widget>>,
    dirty: bool,
    rect: Rect,
    error: Option<&'static str>,
    validation_focus: Option<String>,
    password_next_focus: Rc<Cell<bool>>,
    avatar: Option<String>,
    avatar_picker: Option<ui::PortalChannel>,
    profile_avatar_original: Option<OriginalAvatar>,
    flatpak_access_details: bool,
    migration_source_location: bool,
    #[cfg(not(unix))]
    images: ui::ImageStore,
    capture: bool,
    last_frame: Option<Scene>,
    outgoing: Option<Scene>,
    slide: Option<Animated<f64>>,
    pending_slide: Option<f64>,
    layout_metrics: Option<(f32, f32, f32)>,
    profile_breakpoint: f64,
    field_values: [ui::LocalState<String>; 4],
    storage: storage::Storage,
    install_preferences: imager_backend::imager_model::InstallProfile,
    direct: direct::Direct,
    desktop_pending: bool,
    desktop_review_pending: bool,
    user_transfer: transfer::Transfer,
    home_promotion: promotion::Promotion,
    account_actions: super::account_transfer::AccountActions,
    accounts: accounts::Accounts,
    personal: personal::Personal,
    restart: restart_controller::Restart,
    resume_draft: Option<draft::ReadyDraft>,
    checkpoint_restoring: bool,
    checkpoint_restored: bool,
    checkpoint_signature: Option<[u8; 32]>,
    checkpoint_pending: Option<[u8; 32]>,
    checkpoint_failed: bool,
    account_continue: bool,
    existing_identity: existing_identity::ExistingIdentity,
    pairing: pairing::Pairing,
    personal_continue: Option<Page>,
    account_answer: serde_json::Value,
    live: bool,
    demo: bool,
    media: Option<ui::LocalState<bool>>,
    media_bundle: Option<ui::LocalState<Option<std::sync::Arc<prepared_media::PreparedMedia>>>>,
    media_preparation: prepared_media::Controller,
    media_return: Option<ui::LocalState<Option<prepared_media::ReturnReason>>>,
    dispatcher: Option<ui::framework::Dispatcher<Imager>>,
    migration_selection: migration_selection::Selection,
    portable_source: portable_source::Selection,
    recovery: super::migration_recovery::Flow,
    belongings: belongings::Belongings,
    belongings_worker: Option<std::sync::mpsc::Receiver<belongings::Belongings>>,
}

impl Default for InstallerPreview {
    fn default() -> Self {
        let mut journey = Journey::new(String::new());
        journey.device_name = t("installer-preview-device-default");
        #[cfg(not(unix))]
        let fetch: ui::Fetcher = std::sync::Arc::new(|path| {
            use std::io::Read as _;
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .ok()?
                .take(10 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() <= 10 * 1024 * 1024).then_some(bytes)
        });
        Self {
            capabilities: capabilities::GuidedCapabilities::native(),
            avatar: None,
            avatar_picker: None,
            profile_avatar_original: None,
            flatpak_access_details: false,
            migration_source_location: false,
            #[cfg(not(unix))]
            images: ui::ImageStore::with_workers(&fetch, 1),
            capture: false,
            last_frame: None,
            outgoing: None,
            slide: None,
            pending_slide: None,
            layout_metrics: None,
            profile_breakpoint: 0.0,
            field_values: std::array::from_fn(|_| ui::LocalState::new(String::new())),
            storage: storage::Storage::default(),
            install_preferences: imager_backend::imager_model::InstallProfile::default(),
            direct: direct::Direct::default(),
            desktop_pending: false,
            desktop_review_pending: false,
            user_transfer: transfer::Transfer::default(),
            home_promotion: promotion::Promotion::default(),
            account_actions: super::account_transfer::AccountActions::default(),
            accounts: accounts::Accounts::default(),
            personal: personal::Personal::default(),
            restart: restart_controller::Restart::default(),
            resume_draft: None,
            checkpoint_restoring: false,
            checkpoint_restored: false,
            checkpoint_signature: None,
            checkpoint_pending: None,
            checkpoint_failed: false,
            account_continue: false,
            existing_identity: existing_identity::ExistingIdentity::default(),
            pairing: pairing::Pairing::default(),
            personal_continue: None,
            account_answer: serde_json::Value::Null,
            live: false,
            demo: false,
            media: None,
            media_bundle: None,
            media_return: None,
            media_preparation: prepared_media::Controller::default(),
            dispatcher: None,
            migration_selection: migration_selection::Selection::default(),
            portable_source: portable_source::Selection::default(),
            recovery: super::migration_recovery::Flow::default(),
            windows: windows::Preparation::default(),
            belongings: belongings::Belongings::default(),
            belongings_worker: None,
            journey,
            linux: cfg!(target_os = "linux"),
            commands: Rc::default(),
            composed: None,
            dirty: true,
            rect: Rect::ZERO,
            error: None,
            validation_focus: None,
            password_next_focus: Rc::default(),
        }
    }
}

impl InstallerPreview {
    fn unlocking_preparation(&self) -> bool {
        {
            self.live
                && self.journey.page == Page::Password
                && !self.checkpoint_restored
                && self
                    .resume_draft
                    .as_ref()
                    .is_some_and(|ready| ready.account_checkpoint().is_some())
        }
    }

    fn migration_busy(&self) -> bool {
        if self.migration_selection.busy() {
            return true;
        }
        self.portable_source.busy()
    }
    fn migration_choosing(&self) -> bool {
        if self.migration_selection.choosing() {
            return true;
        }
        self.portable_source.choosing()
    }
    fn existing_identity_active(&self) -> bool {
        self.existing_identity.active() || self.pairing.busy()
    }

    fn personal_busy(&self) -> bool {
        self.media_preparation.busy()
            || self.pairing.busy()
            || self.personal.busy()
            || self.existing_identity.busy()
    }
    fn guided_host_available(&self) -> bool {
        !self.live || self.capabilities.prepared_installation
    }

    fn direct_route_enabled(&self) -> bool {
        if !self.live {
            return true;
        }
        super::internal_install_flow::live_gate_open()
    }

    fn storage_content(&self) -> ui::widget::Column {
        let mut content = page::stack();
        if !self.direct_route_enabled() {
            return content;
        }
        if self.journey.migrate == Some(true) && self.journey.categories[0] {
            content = content.push(Box::new(
                Text::new(&t("installer-preview-storage-shared-files"))
                    .with_id("installer.storage.shared-files"),
            ));
        }
        if self.storage.checking() {
            return content.push(Box::new(
                Text::new(&t("installer-preview-storage-checking"))
                    .with_id("installer.storage.checking"),
            ));
        }
        if let Some(Ok(plan)) = &self.storage.plan {
            if let (Some(volume), Some(shrink)) = (&plan.basis.linux_btrfs_volume, &plan.shrink) {
                content = content.push(Box::new(
                    Text::new(&t_args(
                        "installer-storage-btrfs-resize",
                        &[
                            ("before", ui::format::bytes(shrink.from_bytes).into()),
                            ("after", ui::format::bytes(shrink.to_bytes).into()),
                            ("reserve", ui::format::bytes(volume.reserve_bytes).into()),
                        ],
                    ))
                    .with_id("installer.storage.retained-resize"),
                ));
            }

            if let Some(estimate) = self.storage.application_estimate() {
                content = content.push(Box::new(Text::new(&t_args(
                    "installer-preview-storage-app-estimate",
                    &[("size", ui::format::bytes(estimate).into())],
                ))));
            }
            if let Some((min, max)) = self.storage.bounds {
                content = content
                    .push(Box::new(
                        Field::new(
                            t("installer-preview-storage-size-label"),
                            NumberInput::from_state(&self.storage.size)
                                .with_id("installer.storage.size")
                                .with_range(min as f64, max as f64)
                                .with_precision(0)
                                .with_step(1.0)
                                .with_suffix("GiB")
                                .with_accessible_name(t("installer-preview-storage-size-label")),
                        )
                        .with_label_placement(Some(LabelPlacement::Above)),
                    ))
                    .push(Box::new(
                        Text::new(&t_args(
                            "installer-preview-storage-size-range",
                            &[("min", min.into()), ("max", max.into())],
                        ))
                        .with_role(TextRole::Caption),
                    ));
            }
            content = content.push(Box::new(
                self.button(
                    "installer.storage.details",
                    if self.storage.details {
                        "installer-preview-hide-details"
                    } else {
                        "installer-preview-storage-show-details"
                    },
                    InstallerPreviewEffect::ToggleStorageDetails,
                )
                .role(ButtonRole::Plain),
            ));
            if self.storage.details {
                content = content.push(Box::new(
                    Text::new(&t_args(
                        "installer-preview-storage-capacity",
                        &[(
                            "capacity",
                            ui::format::bytes(plan.basis.disk.size_bytes).into(),
                        )],
                    ))
                    .with_id("installer.storage.capacity"),
                ));
                let existing_bytes = plan.shrink.as_ref().map_or_else(
                    || {
                        plan.basis
                            .disk
                            .partitions
                            .iter()
                            .map(|p| (p.last_lba - p.first_lba + 1) * plan.basis.disk.sector_bytes)
                            .sum()
                    },
                    |shrink| shrink.to_bytes,
                );
                content = content
                    .push(Box::new(
                        ListRow::new(
                            "installer.storage.existing",
                            t("installer-preview-storage-existing"),
                        )
                        .secondary(ui::format::bytes(existing_bytes))
                        .icon(Icon::Monitor),
                    ))
                    .push(Box::new(
                        ListRow::new(
                            "installer.storage.tilo",
                            t("installer-preview-storage-tilo"),
                        )
                        .secondary(ui::format::bytes(self.storage.selected() << 30))
                        .icon(Icon::MonitorSmartphone),
                    ))
                    .push(Box::new(
                        ListRow::new(
                            "installer.storage.temporary",
                            t("installer-preview-storage-temporary"),
                        )
                        .secondary(ui::format::bytes(
                            (plan.staging_esp.extent().lbas() + plan.staging_root.extent().lbas())
                                * plan.basis.disk.sector_bytes,
                        ))
                        .icon(Icon::Download),
                    ));
                if plan.shrink.is_some() {
                    content =
                        content.push(Box::new(Text::new(&t("installer-preview-storage-shrink"))));
                }
            }
        } else if let Some(Err(super::staging::plan::PlanError::InsufficientSpace {
            needed_bytes,
            available_bytes,
        })) = &self.storage.plan
        {
            content = content.push(Box::new(Text::new(&t_args(
                "installer-preview-storage-shortfall",
                &[
                    ("needed", ui::format::bytes(*needed_bytes).into()),
                    ("available", ui::format::bytes(*available_bytes).into()),
                ],
            ))));
        } else {
            content = content.push(Box::new(Text::new(&t(
                "installer-preview-storage-unreadable",
            ))));
        }
        if let Some(facts) = &self.storage.facts {
            for blocker in &facts.blockers {
                content = content.push(Box::new(Text::new(
                    &super::internal_install_flow::blocker_text(
                        *blocker,
                        if self.linux {
                            super::model::HostOs::Linux
                        } else {
                            super::model::HostOs::Windows
                        },
                    ),
                )));
            }
        }
        if self.live {
            if self.media_preparation.busy() {
                content = content.push(Box::new(Text::new(&t(self
                    .media_preparation
                    .phase_key()
                    .as_deref()
                    .unwrap_or("installer-preview-install-preparing")))));
                if let Some(bytes) = self.media_preparation.progress_label() {
                    content =
                        content.push(Box::new(Text::new(&bytes).with_role(TextRole::Caption)));
                }
            }
            if !matches!(self.storage.plan, Some(Ok(_)))
                || self
                    .storage
                    .facts
                    .as_ref()
                    .is_none_or(|facts| !facts.blockers.is_empty())
            {
                content = content.push(Box::new(
                    self.button(
                        "installer.storage.recheck",
                        "installer-storage-check-capacity",
                        InstallerPreviewEffect::CheckStorage,
                    )
                    .role(ButtonRole::Plain),
                ));
            }
        }
        content.push(Box::new(self.recovery_content()))
    }

    fn invalidate_migration_preparation(&mut self) {
        self.storage
            .set_application_minimum(super::staging::MIN_TARGET_BYTES);
        self.migration_selection.invalidate();
        self.portable_source.invalidate();
    }

    // These actions operate on the current guided page. Explicit source grants,
    // exports and transfer operations remain model-level capabilities; they do
    // not become unavailable merely because their controls are offscreen.
    fn effect_page_admitted(&self, id: &str) -> bool {
        if recovery::is_recovery_action(id) {
            return self.recovery_admitted();
        }
        if matches!(
            id,
            "find-windows-roots"
                | "choose-windows-root"
                | "select-windows-root"
                | "open-windows-source"
                | "show-windows-category"
                | "review-windows-root"
                | "review-windows-category"
                | "set-windows-exclusion-reason"
                | "set-windows-preparation-password"
                | "choose-windows-export"
                | "export-windows-preparation"
        ) {
            return self.windows.enabled && self.live;
        }
        if self.windows.enabled && self.live {
            return matches!(
                id,
                "read-preview"
                    | "find-windows-roots"
                    | "choose-windows-root"
                    | "select-windows-root"
                    | "open-windows-source"
                    | "show-windows-category"
                    | "review-windows-root"
                    | "review-windows-category"
                    | "set-windows-exclusion-reason"
                    | "set-windows-preparation-password"
                    | "choose-windows-export"
                    | "export-windows-preparation"
                    | "prepare-usb"
            );
        }
        let page = self.journey.page;
        match id {
            "choose-preview-avatar" | "select-preview-avatar" | "set-preview-name" => {
                page == Page::Profile
            }
            "set-preview-password" => page == Page::Password,
            "set-preview-confirmation" => page == Page::Password && !self.unlocking_preparation(),
            "choose-existing-identity-backup"
            | "select-existing-identity-backup"
            | "set-existing-identity-password" => page == Page::Connect && self.live,
            "start-existing-device-pairing" => {
                page == Page::Connect
                    && self.live
                    && self.journey.transfer.as_deref() == Some("device")
            }
            "set-preview-device-name" => {
                page == Page::Devices
                    || (page == Page::Connect && self.journey.transfer.as_deref() == Some("device"))
            }
            "account-step" | "set-account-field" => page == Page::Accounts && self.live,
            "set-preview-category" => page == Page::Belongings && self.journey.details,
            "set-preview-flatpak-app" => {
                page == Page::Belongings
                    && self.live
                    && self.journey.details
                    && self.journey.migrate == Some(true)
                    && self.journey.categories[1]
            }
            "set-preview-service-session" => {
                page == Page::Belongings
                    && self.live
                    && self.journey.details
                    && self.journey.migrate == Some(true)
                    && self.journey.categories[2]
            }
            "toggle-flatpak-access-details" => {
                page == Page::Belongings && self.journey.details && self.journey.categories[1]
            }
            "toggle-migration-source-location" => {
                page == Page::Belongings
                    && self.live
                    && self.journey.details
                    && self.journey.categories[0]
                    && self.belongings.home.is_some()
            }
            "check-belongings" => {
                page == Page::Belongings
                    && self.live
                    && self.journey.migrate == Some(true)
                    && self.belongings_worker.is_none()
            }
            "toggle-preview-details"
            | "select-migration-home"
            | "grant-mounted-home-location"
            | "continue-mounted-home-review"
            | "cancel-mounted-home-permission"
            | "restart-mounted-home-review" => page == Page::Belongings,
            "check-storage" => page == Page::Storage && self.live,
            "toggle-storage-details" => page == Page::Storage && self.direct_route_enabled(),
            "set-storage-size" => page == Page::Storage,
            "choose-recovery-destination"
            | "select-recovery-directory"
            | "review-recovery-destination"
            | "review-existing-recovery-copy"
            | "start-recovery-copy"
            | "retry-recovery-copy"
            | "cancel-recovery-copy" => self.recovery_admitted(),
            "edit-preview"
            | "prepare-usb"
            | "prepare-direct-installation"
            | "prepare-desktop-installation"
            | "review-changed-files"
            | "cancel-media-preparation" => page == Page::Review,
            _ => true,
        }
    }

    fn apply(&mut self, effect: InstallerPreviewEffect) -> Result<(), &'static str> {
        if self.demo && !demo::admits(&effect) {
            return Err("installer-demo-unavailable");
        }
        if self.windows.enabled && self.live && !recovery::is_recovery_effect(&effect) {
            return self.apply_windows(effect);
        }
        if self.live && !self.direct.active {
            match effect {
                InstallerPreviewEffect::ResumeInstallerPreparation if self.restart.blocks() => {
                    return self.restart.resume();
                }
                InstallerPreviewEffect::StartNewInstallerPreparation
                    if matches!(self.restart.state(), "available" | "pending") =>
                {
                    return self.restart.retire();
                }
                InstallerPreviewEffect::RetryInstallerPreparation
                    if !self.personal_busy() && !self.user_transfer.busy() =>
                {
                    return self.restart.inspect();
                }
                _ => {}
            }
            if self.restart.blocks() && !matches!(effect, InstallerPreviewEffect::ReadPreview) {
                return Err("installer-restart-unavailable");
            }
        }
        let page = self.journey.page;
        if self.journey.destination == Some(Destination::Desktop)
            && page == Page::Review
            && !matches!(
                effect,
                InstallerPreviewEffect::ReadPreview
                    | InstallerPreviewEffect::BackInPreview
                    | InstallerPreviewEffect::RestartPreview
                    | InstallerPreviewEffect::EditPreview { .. }
                    | InstallerPreviewEffect::ContinuePreview
                    | InstallerPreviewEffect::PrepareDesktopInstallation
            )
        {
            return Err("installer-preview-wrong-page");
        }
        let profile_edit = self.journey.editing_profile();
        if profile_edit
            && !matches!(
                effect,
                InstallerPreviewEffect::ReadPreview
                    | InstallerPreviewEffect::SetPreviewName { .. }
                    | InstallerPreviewEffect::SelectPreviewAvatar { .. }
                    | InstallerPreviewEffect::ChoosePreviewAvatar
                    | InstallerPreviewEffect::ContinuePreview
                    | InstallerPreviewEffect::BackInPreview
            )
        {
            return Err("installer-preview-wrong-page");
        }
        let destination_edit = self.journey.editing_destination();
        let belongings_edit = self.journey.editing_belongings();
        let choices_edit = self.journey.editing_choices();
        if choices_edit
            && !matches!(
                effect,
                InstallerPreviewEffect::ReadPreview
                    | InstallerPreviewEffect::ChoosePreview { .. }
                    | InstallerPreviewEffect::ContinuePreview
                    | InstallerPreviewEffect::BackInPreview
                    | InstallerPreviewEffect::CheckStorage
                    // Read-only discovery does not export or grant access to
                    // the retained preparation being edited.
                    | InstallerPreviewEffect::CheckBelongings
                    | InstallerPreviewEffect::ToggleStorageDetails
                    | InstallerPreviewEffect::SetStorageSize { .. }
                    | InstallerPreviewEffect::SetPreviewDeviceName { .. }
                    | InstallerPreviewEffect::TogglePreviewDetails
                    | InstallerPreviewEffect::ToggleFlatpakAccessDetails
                    | InstallerPreviewEffect::ToggleMigrationSourceLocation
                    | InstallerPreviewEffect::SetPreviewCategory { .. }
                    | InstallerPreviewEffect::SetPreviewFlatpakApp { .. }
                    | InstallerPreviewEffect::SetPreviewServiceSession { .. }
            )
        {
            // Until this decision is saved, retained preparation belongs to
            // the original route and must not be used with tentative choices.
            return Err("installer-preview-wrong-page");
        }
        if matches!(
            effect,
            InstallerPreviewEffect::SetPreviewPassword { .. }
                | InstallerPreviewEffect::SetPreviewConfirmation { .. }
        ) && (self.checkpoint_pending.is_some() || self.checkpoint_restoring)
        {
            return Err("installer-accounts-busy");
        }
        if self.direct.active {
            match effect {
                InstallerPreviewEffect::ReadPreview => return Ok(()),
                InstallerPreviewEffect::BackInPreview if self.direct.navigation.get() => {
                    self.direct.active = false;
                    if self.journey.destination == Some(Destination::Desktop) {
                        self.journey.back()?;
                    }
                    self.composed = None;
                    self.dirty = true;
                    return Ok(());
                }
                _ => return Err("installer-preview-identity-busy"),
            }
        }
        if (self.media_preparation.busy()
            || (self.migration_selection.busy()
                && !(self.migration_selection.choosing()
                    && matches!(
                        effect,
                        InstallerPreviewEffect::SelectMigrationHome { .. }
                            | InstallerPreviewEffect::BackInPreview
                    ))))
            && !matches!(
                effect,
                InstallerPreviewEffect::ReadPreview
                    | InstallerPreviewEffect::CancelMediaPreparation
            )
        {
            return Err("installer-preview-transfer-busy");
        }
        if self.portable_source.busy()
            && !(matches!(
                effect,
                InstallerPreviewEffect::ReadPreview
                    | InstallerPreviewEffect::BackInPreview
                    | InstallerPreviewEffect::CancelMountedHomePermission
            ) || (self.portable_source.choosing()
                && matches!(effect, InstallerPreviewEffect::SelectMigrationHome { .. })))
        {
            return Err("installer-portable-source-busy");
        }
        let cancel_photo_read = profile_edit
            && matches!(effect, InstallerPreviewEffect::BackInPreview)
            && self.personal.cancellable_photo_read();
        if self.personal_busy()
            && !cancel_photo_read
            && !matches!(
                effect,
                InstallerPreviewEffect::ReadPreview
                    | InstallerPreviewEffect::CancelExistingDevicePairing
                    | InstallerPreviewEffect::CancelMediaPreparation
            )
        {
            return Err("installer-preview-identity-busy");
        }
        if self.home_promotion.busy()
            && !matches!(
                effect,
                InstallerPreviewEffect::ReadPreview | InstallerPreviewEffect::CancelHomePromotion
            )
        {
            return Err("installer-preview-transfer-busy");
        }
        let direction = if matches!(effect, InstallerPreviewEffect::BackInPreview) {
            -1.0
        } else {
            1.0
        };
        if self.user_transfer.busy() || self.account_actions.busy() || self.accounts.busy() {
            if matches!(
                effect,
                InstallerPreviewEffect::RestartPreview
                    | InstallerPreviewEffect::CreatePreparedIdentity { .. }
                    | InstallerPreviewEffect::UpdatePreparedIdentity { .. }
                    | InstallerPreviewEffect::ExportPreparedIdentity { .. }
                    | InstallerPreviewEffect::ExportPreparedAccounts { .. }
                    | InstallerPreviewEffect::ExportPreparedSetup { .. }
                    | InstallerPreviewEffect::SetPreviewCategory { .. }
                    | InstallerPreviewEffect::SetPreviewFlatpakApp { .. }
                    | InstallerPreviewEffect::SetPreviewServiceSession { .. }
                    | InstallerPreviewEffect::ReviewUserTransfer { .. }
                    | InstallerPreviewEffect::ReviewHomePromotion { .. }
                    | InstallerPreviewEffect::StartHomePromotion
                    | InstallerPreviewEffect::RetryHomePromotion
                    | InstallerPreviewEffect::ReviewInstalledTransfer { .. }
                    | InstallerPreviewEffect::SaveMigrationPlan { .. }
                    | InstallerPreviewEffect::CheckBelongings
            ) || (page == Page::Migration
                && matches!(effect, InstallerPreviewEffect::ChoosePreview { .. }))
            {
                return Err("installer-preview-transfer-busy");
            }
        }
        if !self.effect_page_admitted(effect.id()) {
            return Err("installer-preview-wrong-page");
        }
        let save_choices = (!choices_edit
            && matches!(
                effect,
                InstallerPreviewEffect::ChoosePreview { .. }
                    | InstallerPreviewEffect::SetPreviewCategory { .. }
                    | InstallerPreviewEffect::SetPreviewFlatpakApp { .. }
                    | InstallerPreviewEffect::SetPreviewServiceSession { .. }
            ))
            || (choices_edit
                && (self.journey.saving_edit()
                    || (destination_edit
                        && page == Page::Destination
                        && self.live
                        && !self.has_storage_decision()))
                && matches!(effect, InstallerPreviewEffect::ContinuePreview));
        if save_choices
            || matches!(
                effect,
                InstallerPreviewEffect::SetPreviewName { .. }
                    | InstallerPreviewEffect::SetPreviewDeviceName { .. }
                    | InstallerPreviewEffect::SetPreviewPassword { .. }
                    | InstallerPreviewEffect::SetPreviewConfirmation { .. }
                    | InstallerPreviewEffect::AccountStep { .. }
                    | InstallerPreviewEffect::SelectMigrationHome { .. }
                    | InstallerPreviewEffect::SetPreviewFlatpakApp { .. }
                    | InstallerPreviewEffect::SetPreviewServiceSession { .. }
                    | InstallerPreviewEffect::SelectPreviewAvatar { .. }
            )
        {
            if self.direct.prepared_belongings.take().is_some() {
                self.media_preparation = prepared_media::Controller::default();
            }
        }
        match effect {
            InstallerPreviewEffect::ReadPreview => return Ok(()),
            InstallerPreviewEffect::ExportPreparedSetup {
                destination,
                setup_binding,
            } if self.live && self.journey.identity.is_some() => {
                #[cfg(target_os = "linux")]
                {
                    let accounts =
                        if self.journey.migrate == Some(true) && self.journey.categories[2] {
                            Some(
                                self.accounts
                                    .prepared_handle()
                                    .ok_or("installer-preview-accounts-not-prepared")?,
                            )
                        } else {
                            None
                        };
                    self.personal.export_setup(
                        accounts,
                        destination.into(),
                        zeroize::Zeroizing::new(self.journey.password.clone()),
                        setup_binding,
                        if self.journey.migrate == Some(true) {
                            Some(
                                self.belongings
                                    .home
                                    .clone()
                                    .ok_or("installer-preview-import-unavailable")?,
                            )
                        } else {
                            None
                        },
                    )?;
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (destination, setup_binding);
                    return Err("installer-preview-identity-unavailable");
                }
            }
            InstallerPreviewEffect::ExportPreparedAccounts {
                destination,
                setup_binding,
            } if self.live && self.journey.categories[2] => {
                imager_backend::prepared_identity::require_supported()
                    .map_err(|_| "installer-preview-identity-unavailable")?;
                {
                    let prepared = self
                        .accounts
                        .prepared_handle()
                        .ok_or("installer-preview-accounts-not-prepared")?;
                    self.personal.export_accounts(
                        prepared,
                        destination.into(),
                        zeroize::Zeroizing::new(self.journey.password.clone()),
                        setup_binding,
                    )?;
                }
            }
            InstallerPreviewEffect::CreatePreparedIdentity {
                private_custody,
                avatar_color,
            } if self.live && self.journey.identity == Some(Identity::New) => {
                imager_backend::prepared_identity::require_supported()
                    .map_err(|_| "installer-preview-identity-unavailable")?;
                {
                    self.personal.create(
                        private_custody.into(),
                        self.journey.name.clone(),
                        identity_color(&avatar_color)?,
                    )?;
                }
            }
            InstallerPreviewEffect::UpdatePreparedIdentity { avatar_color }
                if self.live && self.journey.identity == Some(Identity::New) =>
            {
                imager_backend::prepared_identity::require_supported()
                    .map_err(|_| "installer-preview-identity-unavailable")?;
                {
                    self.personal
                        .update(self.journey.name.clone(), identity_color(&avatar_color)?)?;
                }
            }
            InstallerPreviewEffect::ExportPreparedIdentity { destination }
                if self.live && self.journey.identity.is_some() =>
            {
                imager_backend::prepared_identity::require_supported()
                    .map_err(|_| "installer-preview-identity-unavailable")?;
                {
                    self.personal.export(
                        destination.into(),
                        zeroize::Zeroizing::new(self.journey.password.clone()),
                    )?;
                }
            }
            InstallerPreviewEffect::AccountStep { step, selection }
                if page == Page::Accounts && self.live =>
            {
                if self.user_transfer.busy() {
                    return Err("installer-preview-transfer-busy");
                }
                let request = accounts::AccountRequest::from_parts(&step, selection)?;
                if matches!(
                    request,
                    accounts::AccountRequest::Confirm
                        | accounts::AccountRequest::SubmitCode
                        | accounts::AccountRequest::NextcloudBrowser
                        | accounts::AccountRequest::Cancel
                        | accounts::AccountRequest::Retry
                        | accounts::AccountRequest::ImportSaved
                        | accounts::AccountRequest::SkipSaved
                ) {
                    self.user_transfer.invalidate_accounts();
                }
                let before = self.accounts.navigation_step();
                let backwards = matches!(
                    request,
                    accounts::AccountRequest::Back
                        | accounts::AccountRequest::Overview
                        | accounts::AccountRequest::Cancel
                );
                self.accounts.apply(request)?;
                if self.accounts.navigation_step() != before {
                    self.pending_slide = Some(if backwards { -1.0 } else { 1.0 });
                }
            }
            InstallerPreviewEffect::SetAccountField { field, value }
                if page == Page::Accounts && self.live =>
            {
                if self.user_transfer.busy() {
                    return Err("installer-preview-transfer-busy");
                }
                self.accounts
                    .set_dav_field(&field, zeroize::Zeroizing::new(value))?;
            }
            InstallerPreviewEffect::ImportAccount {
                candidate,
                target_home,
            } if self.live
                && self.journey.migrate == Some(true)
                && self.journey.categories[2]
                && !self.user_transfer.busy() =>
            {
                let job = self
                    .account_actions
                    .import_selected(&candidate, std::path::Path::new(&target_home))
                    .map_err(|_| "installer-preview-account-action-failed")?;
                self.account_answer = serde_json::json!({"job":job});
            }
            InstallerPreviewEffect::ConnectAccount {
                provider,
                candidate,
                target_home,
            } if self.live
                && self.journey.categories[2]
                && !self.user_transfer.busy()
                && (candidate.is_none() || self.journey.migrate == Some(true)) =>
            {
                let provider = super::account_transfer::ConnectionProvider::from_id(&provider)
                    .ok_or("installer-preview-invalid-choice")?;
                let consent = self
                    .account_actions
                    .begin(
                        provider,
                        candidate.as_deref(),
                        std::path::Path::new(&target_home),
                    )
                    .map_err(|_| "installer-preview-account-action-failed")?;
                self.account_answer = serde_json::json!({"consent":consent});
            }
            InstallerPreviewEffect::CompleteAccountConnection { connection }
                if self.live && self.journey.categories[2] && !self.user_transfer.busy() =>
            {
                let job = self
                    .account_actions
                    .complete(&connection)
                    .map_err(|_| "installer-preview-account-action-failed")?;
                self.account_answer = serde_json::json!({"job":job});
            }
            InstallerPreviewEffect::CancelAccountConnection { connection } if self.live => {
                self.account_answer =
                    serde_json::json!({"cancelled":self.account_actions.cancel(&connection)});
            }
            InstallerPreviewEffect::ReviewUserTransfer {
                source,
                destination,
                journals,
            } if self.live && self.journey.migrate == Some(true) => {
                if self.journey.categories[1]
                    && (self.belongings.flatpak.is_none()
                        || self.belongings.flatpak_unavailable
                        || self.belongings.browser_unavailable
                        || self.belongings_worker.is_some())
                {
                    return Err("installer-preview-flatpak-unavailable");
                }
                let refs = self.selected_portable_apps();
                self.user_transfer.review(
                    source.into(),
                    destination.into(),
                    journals.into(),
                    self.migration_categories(),
                    refs,
                    self.accounts.prepared_handle(),
                )?;
            }
            InstallerPreviewEffect::GrantMountedHomeLocation { location }
                if self.live
                    && page == Page::Belongings
                    && self.journey.destination == Some(Destination::Another)
                    && self.journey.categories[0] =>
            {
                self.portable_source.request_mounted(location)?;
                self.error = None;
                self.dirty = true;
                return Ok(());
            }
            InstallerPreviewEffect::ContinueMountedHomeReview
                if self.live
                    && page == Page::Belongings
                    && self.journey.destination == Some(Destination::Another)
                    && self.journey.categories[0] =>
            {
                self.portable_source.finish_mounted()?;
                self.error = None;
                self.dirty = true;
                return Ok(());
            }
            InstallerPreviewEffect::CancelMountedHomePermission
                if self.live
                    && page == Page::Belongings
                    && self.portable_source.mounted_permission_active() =>
            {
                self.portable_source.cancel_mounted();
                self.dirty = true;
                return Ok(());
            }
            #[cfg(target_os = "linux")]
            InstallerPreviewEffect::RestartMountedHomeReview
                if self.live
                    && page == Page::Belongings
                    && self.journey.destination == Some(Destination::Another)
                    && self.journey.categories[0]
                    && self.portable_source.mounted_needs_restart() =>
            {
                let (home, system_refs) = self.migration_sources()?;
                let selected_applications = self.selected_applications();
                self.portable_source.refresh_mounted(
                    home,
                    portable_source::Choices {
                        home: self.journey.categories[0],
                        applications: selected_applications,
                        accounts: self.journey.categories[2],
                        system_refs,
                        preparation: Some(
                            self.personal
                                .prepared_handle()
                                .ok_or("installer-preview-identity-not-prepared")?,
                        ),
                    },
                )?;
                self.error = None;
                self.dirty = true;
                return Ok(());
            }
            InstallerPreviewEffect::SelectMigrationHome { source }
                if self.live
                    && page == Page::Belongings
                    && matches!(
                        self.journey.destination,
                        Some(Destination::Computer | Destination::Another)
                    ) =>
            {
                #[cfg(target_os = "linux")]
                {
                    let (home, refs) = self.migration_sources()?;
                    if self.journey.destination == Some(Destination::Another) {
                        let selected_applications = self.selected_applications();
                        self.portable_source.start_with_choices(
                            source.into(),
                            home,
                            portable_source::Choices {
                                home: self.journey.categories[0],
                                applications: selected_applications,
                                accounts: self.journey.categories[2],
                                system_refs: refs,
                                preparation: Some(
                                    self.personal
                                        .prepared_handle()
                                        .ok_or("installer-preview-identity-not-prepared")?,
                                ),
                            },
                        )?;
                    } else {
                        self.migration_selection.start(
                            source.into(),
                            home,
                            self.migration_categories(),
                            refs,
                        );
                    }
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = source;
                    return Err("installer-preview-import-unavailable");
                }
            }
            InstallerPreviewEffect::SaveMigrationPlan {
                source,
                private_plans,
                operation,
            } if self.live && self.journey.migrate == Some(true) => {
                if self.journey.categories[1]
                    && (self.belongings.flatpak.is_none() || self.belongings.flatpak_unavailable)
                {
                    return Err("installer-preview-flatpak-unavailable");
                }
                self.user_transfer.prepare_migration(
                    source.into(),
                    private_plans.into(),
                    operation,
                    self.migration_categories(),
                    self.selected_portable_apps(),
                )?;
            }
            InstallerPreviewEffect::ReviewInstalledTransfer {
                private_plans,
                operation,
                destination,
                journals,
                flatpak_inventory,
            } if self.live && self.journey.migrate == Some(true) => {
                self.user_transfer.review_installed(
                    private_plans.into(),
                    operation,
                    destination.into(),
                    journals.into(),
                    self.migration_categories(),
                    self.accounts.prepared_handle(),
                    flatpak_inventory.map(Into::into),
                )?;
            }
            InstallerPreviewEffect::StartUserTransfer
                if self.live && self.journey.migrate == Some(true) =>
            {
                if self.account_actions.busy() || self.accounts.busy() {
                    return Err("installer-preview-transfer-busy");
                }
                self.user_transfer
                    .start(false, self.migration_categories())?
            }
            InstallerPreviewEffect::RetryUserTransfer
                if self.live && self.journey.migrate == Some(true) =>
            {
                if self.account_actions.busy() || self.accounts.busy() {
                    return Err("installer-preview-transfer-busy");
                }
                self.user_transfer
                    .start(true, self.migration_categories())?
            }
            InstallerPreviewEffect::ReviewHomePromotion {
                target_home,
                journal,
            } if self.live => {
                #[cfg(target_os = "linux")]
                {
                    let copied = self
                        .user_transfer
                        .completed_home()
                        .ok_or("installer-preview-transfer-not-reviewed")?;
                    self.home_promotion
                        .review(copied, target_home.into(), journal.into())?;
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (target_home, journal);
                    return Err("installer-preview-import-unavailable");
                }
            }
            InstallerPreviewEffect::StartHomePromotion if self.live => {
                self.home_promotion.start(false)?
            }
            InstallerPreviewEffect::RetryHomePromotion if self.live => {
                self.home_promotion.start(true)?
            }
            InstallerPreviewEffect::CancelHomePromotion if self.live => {
                self.home_promotion.cancel()?
            }
            InstallerPreviewEffect::CancelUserTransfer if self.live => {
                self.user_transfer.cancel()?
            }
            InstallerPreviewEffect::SelectPreviewAvatar { source_file }
                if page == Page::Profile =>
            {
                if !std::path::Path::new(&source_file).is_absolute() {
                    return Err("installer-preview-avatar-failed");
                }
                if imager_backend::prepared_identity::require_supported().is_ok() {
                    self.personal.select_photo(source_file.clone())?;
                }
                self.avatar = Some(source_file);
            }
            InstallerPreviewEffect::ChoosePreviewAvatar
                if page == Page::Profile && self.avatar_picker.is_none() =>
            {
                if self.capture {
                    return Err("installer-preview-avatar-capture");
                }
                self.avatar_picker = Some(
                    ui::PickRequest::open(
                        &t("installer-preview-avatar-picker"),
                        &t("installer-preview-avatar-choose"),
                    )
                    .filter(
                        ui::FileFilter::new(&t("installer-preview-avatar-filter"))
                            .glob("*.png")
                            .glob("*.jpg")
                            .glob("*.jpeg")
                            .glob("*.webp"),
                    )
                    .send(),
                );
                return Ok(());
            }
            InstallerPreviewEffect::StartExistingDevicePairing
                if self.live
                    && page == Page::Connect
                    && self.journey.transfer.as_deref() == Some("device") =>
            {
                if !self.identity_route_available("device") {
                    return Err("installer-identity-route-unavailable");
                }
                self.pairing.start(&self.journey.device_name)?;
            }
            InstallerPreviewEffect::CancelExistingDevicePairing if self.live => {
                if imager_backend::pairing::Pairing::supported() {
                    self.pairing.cancel()?;
                }
            }
            InstallerPreviewEffect::ChooseExistingIdentityBackup
                if page == Page::Connect && self.live =>
            {
                imager_backend::prepared_identity::require_supported()
                    .map_err(|_| "installer-preview-identity-unavailable")?;
                self.existing_identity.choose()?;
            }
            InstallerPreviewEffect::SelectExistingIdentityBackup { source_file }
                if page == Page::Connect && self.live =>
            {
                imager_backend::prepared_identity::require_supported()
                    .map_err(|_| "installer-preview-identity-unavailable")?;
                self.existing_identity.select(source_file)?;
                self.journey.transfer = Some("backup".into());
            }
            InstallerPreviewEffect::SetExistingIdentityPassword { value }
                if page == Page::Connect && self.live =>
            {
                imager_backend::prepared_identity::require_supported()
                    .map_err(|_| "installer-preview-identity-unavailable")?;
                self.existing_identity
                    .set_password(zeroize::Zeroizing::new(value))?;
            }
            InstallerPreviewEffect::ChoosePreview { choice } => {
                if matches!(page, Page::Destination | Page::Migration)
                    && !self.guided_host_available()
                    && !(page == Page::Destination && choice == "desktop" && self.linux)
                {
                    return Err("installer-guided-host-unavailable");
                }
                if page == Page::Connect && !self.identity_route_available(&choice) {
                    return Err("installer-identity-route-unavailable");
                }
                if page == Page::Destination
                    && choice != "desktop"
                    && self.journey.transfer.as_deref() == Some("current")
                {
                    return Err("installer-identity-route-unavailable");
                }
                if self.live
                    && page == Page::Connect
                    && self.journey.transfer.as_deref() != Some(&choice)
                    && self.existing_identity.ready()
                {
                    self.restart.retire_for_choice()?;
                    self.personal.reset_preparation()?;
                    self.existing_identity = existing_identity::ExistingIdentity::default();
                }
                if page == Page::Destination
                    && choice == "desktop"
                    && self.journey.destination != Some(Destination::Desktop)
                    && self.linux
                {
                    self.retire_preparation();
                }
                self.journey.choose(&choice, self.linux)?;
                if page == Page::Destination
                    && self.journey.destination != Some(Destination::Desktop)
                    && self.journey.device_name.is_empty()
                {
                    self.journey.device_name = t("installer-preview-device-default");
                }
                if choice == "fresh" && !choices_edit {
                    self.belongings.sessions.clear();
                }

                if matches!(page, Page::Destination | Page::Migration) && !choices_edit {
                    self.invalidate_migration_preparation();
                }
                if self.live
                    && page == Page::Identity
                    && choice == "new"
                    && self.personal.imported()
                {
                    self.restart.retire_for_choice()?;
                    self.personal.reset_preparation()?;
                    self.existing_identity = existing_identity::ExistingIdentity::default();
                    self.pairing = pairing::Pairing::default();
                    self.avatar = None;
                }
            }
            InstallerPreviewEffect::CheckBelongings
                if page == Page::Belongings
                    && self.live
                    && self.journey.migrate == Some(true)
                    && self.belongings_worker.is_none() =>
            {
                let (tx, rx) = std::sync::mpsc::channel();
                self.belongings_worker = Some(rx);
                std::thread::spawn(move || {
                    let _ = tx.send(belongings::Belongings::discover());
                });
            }
            InstallerPreviewEffect::ContinuePreview => 'advance: {
                if matches!(page, Page::Destination | Page::Migration)
                    && !self.guided_host_available()
                    && self.journey.destination != Some(Destination::Desktop)
                {
                    return Err("installer-guided-host-unavailable");
                }
                if page == Page::Destination
                    && self.journey.destination == Some(Destination::Desktop)
                {
                    self.journey.next()?;
                    self.dirty = true;
                    self.desktop_review_pending = self.live;
                    self.error = None;
                    return Ok(());
                }
                if page == Page::Profile && self.avatar_picker.is_some() {
                    return Err("installer-preview-identity-busy");
                }
                if self.live && page == Page::Review && !choices_edit {
                    let preparation = match self.journey.destination {
                        Some(Destination::Desktop) => {
                            InstallerPreviewEffect::PrepareDesktopInstallation
                        }
                        Some(Destination::Computer) if self.direct_route_enabled() => {
                            InstallerPreviewEffect::PrepareDirectInstallation
                        }
                        Some(Destination::Computer) => {
                            // A fallback changes how installation starts. Only
                            // the explicitly named USB action can select it.
                            return Err(if self.usb_backend_available() {
                                "installer-review-usb-choice-required"
                            } else {
                                "installer-review-installation-unavailable"
                            });
                        }
                        Some(Destination::Another) => InstallerPreviewEffect::PrepareUsb,
                        None => return Err("installer-preview-choose-first"),
                    };
                    return self.apply(preparation);
                }
                if self.live && page == Page::Ready {
                    return Err("installer-preview-wrong-page");
                }
                if belongings_edit {
                    let commit = self.journey.saving_edit();
                    let changed = commit && self.journey.belongings_source_changed();
                    let account_change = commit
                        .then(|| self.journey.belongings_accounts_changed())
                        .flatten();
                    let replacement_accounts = if account_change == Some(false) {
                        let mut replacement = accounts::Accounts::default();
                        if let Some(roots) = self.belongings.roots.clone() {
                            replacement.update_sources(roots, self.belongings.accounts.clone())?;
                        }
                        Some(replacement)
                    } else {
                        None
                    };
                    self.advance_journey()?;
                    if let Some(replacement) = replacement_accounts {
                        self.accounts = replacement;
                        self.user_transfer.invalidate_accounts();
                        {
                            self.checkpoint_signature = None;
                            self.checkpoint_failed = false;
                        }
                    }
                    if changed {
                        self.invalidate_migration_preparation();
                        if self.live
                            && self.journey.migrate == Some(true)
                            && (self.journey.categories[0] || self.selected_applications())
                        {
                            // Changed selections need fresh source consent after
                            // Save. Tentative choices never replace held grants.
                            while self.journey.page != Page::Belongings {
                                self.journey.back()?;
                            }
                        }
                    }
                    if self.live
                        && account_change == Some(true)
                        && self.journey.page == Page::Review
                    {
                        self.accounts.require_saved_review();
                        while self.journey.page != Page::Accounts {
                            self.journey.back()?;
                        }
                    }
                    break 'advance;
                }
                if page == Page::Accounts
                    && self.live
                    && let Some((_, _, request)) = self.accounts.navigation_action()
                {
                    let (step, selection) = request.parts();
                    self.apply(InstallerPreviewEffect::AccountStep {
                        step: step.into(),
                        selection,
                    })?;
                    return Ok(());
                }
                if page == Page::Connect && self.live {
                    imager_backend::prepared_identity::require_supported()
                        .map_err(|_| "installer-preview-identity-unavailable")?;
                    if self
                        .journey
                        .transfer
                        .as_deref()
                        .is_some_and(|route| !self.identity_route_available(route))
                    {
                        return Err("installer-identity-route-unavailable");
                    }
                    {
                        if self.existing_identity.ready()
                            && matches!(
                                self.journey.transfer.as_deref(),
                                Some("backup" | "current")
                            )
                        {
                            self.advance_journey()?;
                        } else if self.journey.transfer.as_deref() == Some("device") {
                            if self.pairing.ready() {
                                self.advance_journey()?;
                            } else {
                                self.pairing.start(&self.journey.device_name)?;
                            }
                        } else if self.journey.transfer.as_deref() == Some("current") {
                            self.existing_identity.use_current()?;
                        } else if self.existing_identity.selected() {
                            self.existing_identity.restore()?;
                        } else if self.journey.transfer.as_deref() == Some("backup") {
                            self.existing_identity.choose()?;
                        } else {
                            return Err("installer-preview-choose-first");
                        }
                        self.dirty = true;
                        return Ok(());
                    }
                }
                if page == Page::Accounts && !self.accounts.can_continue() {
                    return Err("installer-accounts-busy");
                }
                if self.unlocking_preparation() {
                    if self.journey.password.is_empty() {
                        return Err("installer-restart-password-required");
                    }
                } else if let Some(error) = self.journey.validation() {
                    return Err(error);
                }
                if page == Page::Belongings
                    && !(self.journey.categories[0] || self.selected_applications())
                {
                    self.invalidate_migration_preparation();
                }
                #[cfg(target_os = "linux")]
                if page == Page::Belongings
                    && self.live
                    && self.journey.destination == Some(Destination::Another)
                    && (self.journey.categories[0] || self.selected_applications())
                {
                    if self.journey.categories[0]
                        && self.portable_source.mounted_permission_active()
                    {
                        return Err("installer-mounted-home-grants-required");
                    }
                    let (home, system_refs) = self.migration_sources()?;
                    let selected_applications = self.selected_applications();
                    self.portable_source.request_with_choices(
                        home,
                        portable_source::Choices {
                            home: self.journey.categories[0],
                            applications: selected_applications,
                            accounts: self.journey.categories[2],
                            system_refs,
                            preparation: Some(
                                self.personal
                                    .prepared_handle()
                                    .ok_or("installer-preview-identity-not-prepared")?,
                            ),
                        },
                    )?;
                    self.dirty = true;
                    return Ok(());
                }
                if page == Page::Belongings
                    && self.live
                    && self.journey.destination == Some(Destination::Computer)
                    && (self.journey.categories[0] || self.selected_applications())
                {
                    let (home, refs) = self.migration_sources()?;
                    self.migration_selection
                        .request(home, self.migration_categories(), refs);
                    self.dirty = true;
                    return Ok(());
                }
                if page == Page::Password
                    && self.live
                    && !self.checkpoint_restored
                    && let Some(ready) = &self.resume_draft
                    && ready.account_checkpoint().is_some()
                {
                    self.accounts.checkpoint_restore_ready(
                        ready.clone(),
                        zeroize::Zeroizing::new(self.journey.password.as_bytes().to_vec()),
                    )?;
                    self.checkpoint_restoring = true;
                    self.dirty = true;
                    return Ok(());
                }
                if page == Page::Accounts && self.live {
                    self.checkpoint_failed = false;
                    if self.save_account_checkpoint()? || self.checkpoint_pending.is_some() {
                        self.account_continue = true;
                        self.dirty = true;
                        return Ok(());
                    }
                }
                if (page == Page::Password || (page == Page::Profile && profile_edit))
                    && self.live
                    && self.journey.identity == Some(Identity::New)
                {
                    if self.restart.busy() {
                        return Err("installer-restart-busy");
                    }
                    if page == Page::Profile {
                        self.personal
                            .save_profile(self.journey.name.clone(), self.avatar.clone())?;
                    } else {
                        self.personal.prepare_for_journey(
                            self.journey.name.clone(),
                            self.avatar.clone(),
                            self.journey.draft_choices(),
                        )?;
                    }
                    self.personal_continue = Some(page);
                    self.error = None;
                    self.dirty = true;
                    return Ok(());
                }
                let renew_destination = destination_edit
                    && (page == Page::Storage
                        || (self.live
                            && page == Page::Destination
                            && !self.has_storage_decision()))
                    && self.journey.destination_edit_changed();
                if renew_destination {
                    self.invalidate_migration_preparation();
                }
                if self.journey.saving_edit() {
                    if self.belongings.sessions.commit_edit() {
                        self.invalidate_migration_preparation();
                    }
                    if self.journey.migrate == Some(false) {
                        self.belongings.sessions.clear();
                    }
                }
                self.advance_journey()?;
                if renew_destination
                    && self.journey.migrate == Some(true)
                    && (self.journey.categories[0] || self.selected_applications())
                {
                    // The changed route needs new source consent: sharing
                    // local files and packaging them for another device differ.
                    while self.journey.page != Page::Belongings {
                        self.journey.back()?;
                    }
                }
            }
            InstallerPreviewEffect::EditPreview { section } => {
                if section == "devices" && self.device_label_locked() {
                    return Err(self.device_label_locked_message());
                }
                if section == "profile"
                    && self.journey.page == Page::Review
                    && self.journey.identity == Some(Identity::New)
                {
                    self.personal.begin_photo_edit()?;
                }
                self.journey.edit(&section)?;
                if section == "profile" {
                    self.profile_avatar_original = Some(OriginalAvatar(self.avatar.clone()));
                }
                self.belongings.sessions.begin_edit();
            }
            InstallerPreviewEffect::ChooseRecoveryDestination { source_id }
                if self.recovery_admitted() =>
            {
                if !self.belongings.windows.as_ref().is_some_and(|inventory| {
                    inventory.roots.iter().any(|r| r.source_id == source_id)
                }) {
                    return Err("installer-recovery-source-required");
                }
                self.recovery
                    .choose(source_id)
                    .map_err(|_| "installer-recovery-review-failed")?;
                {
                    self.direct.prepared_belongings = None;
                }
            }
            InstallerPreviewEffect::SelectRecoveryDirectory {
                source_id,
                directory,
            } if self.recovery_admitted() => {
                let inventory = self
                    .belongings
                    .windows
                    .as_ref()
                    .ok_or("installer-recovery-source-required")?;
                self.recovery
                    .review_directory(inventory, &source_id, std::path::Path::new(&directory))
                    .map_err(|_| "installer-recovery-review-failed")?;
                {
                    self.direct.prepared_belongings = None;
                }
            }
            InstallerPreviewEffect::ReviewRecoveryDestination {
                source_id,
                directory,
                journal,
            } if self.recovery_admitted() => {
                let inventory = self
                    .belongings
                    .windows
                    .as_ref()
                    .ok_or("installer-recovery-source-required")?;
                self.recovery
                    .review(
                        inventory,
                        &source_id,
                        std::path::Path::new(&directory),
                        std::path::Path::new(&journal),
                    )
                    .map_err(|_| "installer-recovery-review-failed")?;
                {
                    self.direct.prepared_belongings = None;
                }
            }
            InstallerPreviewEffect::ReviewExistingRecoveryCopy {
                source_id,
                output,
                journal,
            } if self.recovery_admitted() => {
                let inventory = self
                    .belongings
                    .windows
                    .as_ref()
                    .ok_or("installer-recovery-source-required")?;
                self.recovery
                    .review_existing(
                        inventory,
                        &source_id,
                        std::path::Path::new(&output),
                        std::path::Path::new(&journal),
                    )
                    .map_err(|_| "installer-recovery-review-failed")?;
                {
                    self.direct.prepared_belongings = None;
                }
            }
            InstallerPreviewEffect::StartRecoveryCopy if self.recovery_admitted() => {
                self.recovery
                    .start(false)
                    .map_err(|_| "installer-recovery-copy-failed")?;
            }
            InstallerPreviewEffect::RetryRecoveryCopy if self.recovery_admitted() => {
                self.recovery
                    .start(true)
                    .map_err(|_| "installer-recovery-copy-failed")?;
            }
            InstallerPreviewEffect::CancelRecoveryCopy if self.recovery_admitted() => {
                self.recovery.cancel();
            }
            InstallerPreviewEffect::CheckStorage if page == Page::Storage && self.live => {
                self.storage.review_capacity();
            }
            InstallerPreviewEffect::ToggleStorageDetails
                if page == Page::Storage && self.direct_route_enabled() =>
            {
                self.storage.details = !self.storage.details;
            }
            InstallerPreviewEffect::SetStorageSize { gib } if page == Page::Storage => {
                self.storage.set_size(gib)?;
            }
            InstallerPreviewEffect::CancelMediaPreparation if page == Page::Review && self.live => {
                self.media_preparation.cancel()?;
                self.dirty = true;
            }
            InstallerPreviewEffect::ReviewChangedFiles if page == Page::Review && self.live => {
                if !self.media_preparation.needs_source_review()
                    || self.journey.destination != Some(Destination::Another)
                {
                    return Err("installer-preview-wrong-page");
                }
                self.portable_source.refresh()?;
                self.media_preparation = prepared_media::Controller::default();
                if let Some(bundle) = &self.media_bundle {
                    bundle.set(None);
                }
                while self.journey.page != Page::Belongings {
                    self.journey.back()?;
                }
                self.error = None;
                self.pending_slide = Some(-1.0);
                self.dirty = true;
                return Ok(());
            }
            InstallerPreviewEffect::PrepareDesktopInstallation
                if page == Page::Review
                    && self.live
                    && self.journey.destination == Some(Destination::Desktop)
                    && self.direct.context.is_some() =>
            {
                self.desktop_review_pending = true;
            }
            InstallerPreviewEffect::PrepareBelongings => {
                self.prepare_belongings()?;
            }
            InstallerPreviewEffect::ReviewPreparedBelongings { target_gib } if self.live => {
                if self.journey.destination != Some(Destination::Computer)
                    || !matches!(page, Page::Review | Page::Storage)
                    || self.journey.editing_choices()
                    || self.direct.active
                {
                    return Err("installer-preview-wrong-page");
                }
                let identity = self
                    .personal
                    .prepared_handle()
                    .ok_or("installer-preview-identity-not-prepared")?;
                let (intent, _) = self.reviewed_belongings()?;
                let intent = intent.ok_or("installer-preview-import-unavailable")?;
                if intent
                    .computer_applications()
                    .map_err(|_| "installer-preview-import-unavailable")?
                    .is_none()
                {
                    return Err("installer-preview-import-unavailable");
                }
                let prepared = self
                    .direct
                    .prepared_belongings
                    .clone()
                    .ok_or("installer-preview-identity-not-prepared")?;
                if !prepared.matches_reviewed_belongings(
                    &identity,
                    &self.public_install_profile()?,
                    Some(&intent),
                ) {
                    return Err("installer-preview-import-unavailable");
                }
                let bytes = target_gib
                    .checked_mul(1 << 30)
                    .ok_or("installer-preview-storage-unavailable")?;
                self.media_preparation
                    .review_computer_belongings(prepared, intent, bytes)?;
                self.direct.belongings_pending = true;
                self.direct.belongings_continue = false;
            }
            InstallerPreviewEffect::ExportPreparedBelongings { destination } if self.live => {
                let prepared = self
                    .direct
                    .prepared_belongings
                    .clone()
                    .ok_or("installer-media-not-prepared")?;
                self.media_preparation
                    .start_belongings_export(prepared, destination.into())?;
                self.direct.belongings_pending = true;
            }
            InstallerPreviewEffect::PrepareDirectInstallation
                if page == Page::Review && self.live =>
            {
                self.prepare_direct()?;
            }
            InstallerPreviewEffect::PrepareUsb
                if page == Page::Review
                    && self.live
                    && matches!(
                        self.journey.destination,
                        Some(Destination::Computer | Destination::Another)
                    ) =>
            {
                #[cfg(target_os = "linux")]
                {
                    if let Some(reason) = self.media_preparation.review_reason() {
                        return Err(reason.error_key());
                    }
                    if !self.usb_backend_available() {
                        return Err("installer-preview-storage-unavailable");
                    }
                    let identity = self
                        .personal
                        .prepared_handle()
                        .ok_or("installer-preview-identity-not-prepared")?;
                    let accounts = (self.journey.migrate == Some(true)
                        && self.journey.categories[2])
                        .then(|| self.accounts.prepared_handle())
                        .flatten();
                    let home = if self.journey.migrate == Some(true)
                        && self.journey.categories[0]
                        && self.journey.destination == Some(Destination::Another)
                    {
                        Some(
                            self.portable_source
                                .reviewed_handle()
                                .ok_or("installer-portable-source-failed")?,
                        )
                    } else {
                        None
                    };
                    let retained = if self.journey.migrate == Some(true)
                        && (self.journey.categories[0] || self.selected_applications())
                        && self.journey.destination == Some(Destination::Computer)
                    {
                        Some(
                            self.migration_selection
                                .reviewed_intent()
                                .ok_or("installer-preview-import-unavailable")?,
                        )
                    } else {
                        None
                    };
                    let original_applications = self
                        .portable_source
                        .reviewed_original()
                        .is_some_and(|source| source.applications_only());
                    let applications = if !original_applications
                        && self.journey.migrate == Some(true)
                        && self.selected_applications()
                        && self.journey.destination == Some(Destination::Another)
                    {
                        let requires_package =
                            !self.journey.categories[0] || !self.migration_sources()?.1.is_empty();
                        let source = self.portable_source.reviewed_applications();
                        if requires_package && source.is_none() {
                            return Err("installer-preview-flatpak-unavailable");
                        }
                        source.map(|source| prepared_media::PortableAppsSelection {
                            source,
                            include_user_state: !self.journey.categories[0],
                        })
                    } else {
                        None
                    };
                    let home_applications = self.selected_applications();
                    let install_profile = self.public_install_profile()?;
                    self.media_preparation.start_with_selections(
                        identity,
                        accounts,
                        prepared_media::MediaSelections {
                            planned_direct: None,
                            sessions: self.belongings.sessions.selected_for_accounts(
                                self.journey.migrate == Some(true) && self.journey.categories[2],
                            ),
                            home_applications,
                            home,
                            retained,
                            applications,
                            browser: (self.journey.migrate == Some(true)
                                && self.belongings.browser.available())
                            .then(|| self.belongings.browser.selected())
                            .filter(|selected| !selected.profiles().is_empty()),
                        },
                        zeroize::Zeroizing::new(self.journey.password.clone()),
                        Some(install_profile),
                    )?;
                }
                #[cfg(not(target_os = "linux"))]
                return Err("installer-preview-import-unavailable");
            }
            InstallerPreviewEffect::BackInPreview => {
                if page == Page::Belongings {
                    self.migration_selection.cancel_permission();
                }
                if self.live
                    && page == Page::Connect
                    && self.journey.transfer.as_deref() == Some("device")
                    && !self.pairing.ready()
                {
                    self.journey.transfer = None;
                    self.dirty = true;
                    return Ok(());
                }
                if page == Page::Belongings && !choices_edit {
                    self.portable_source.invalidate();
                }
                if self.live
                    && page == Page::Connect
                    && (self.existing_identity.selected()
                        || self.journey.transfer.as_deref() == Some("current"))
                    && !self.existing_identity.ready()
                {
                    self.existing_identity.back()?;
                    self.journey.transfer = None;
                    self.dirty = true;
                    return Ok(());
                }
                if page == Page::Accounts && !self.accounts.at_overview() {
                    let before = self.accounts.navigation_step();
                    self.accounts
                        .apply(accounts::AccountRequest::from_parts("back", None)?)?;
                    if self.accounts.navigation_step() != before {
                        self.pending_slide = Some(-1.0);
                    }
                } else {
                    let restore_browser = choices_edit && self.journey.cancels_edit();
                    let cancel_profile = profile_edit && self.journey.cancels_edit();
                    if cancel_profile {
                        self.personal.cancel_photo_edit()?;
                    }
                    self.journey.back()?;
                    if cancel_profile {
                        self.avatar_picker = None;
                        self.avatar = self
                            .profile_avatar_original
                            .take()
                            .and_then(|original| original.0);
                    }
                    if restore_browser {
                        self.belongings.sessions.cancel_edit();
                    }
                    if restore_browser {
                        self.belongings
                            .browser
                            .set(browser::Domain::Bookmarks, self.journey.browser_domains[0]);
                        self.belongings
                            .browser
                            .set(browser::Domain::History, self.journey.browser_domains[1]);
                    }
                }
            }
            InstallerPreviewEffect::RestartPreview => {
                let name = self.journey.name.clone();
                let avatar = self.avatar.clone();
                let capture = self.capture;
                let linux = self.linux;
                let live = self.live;
                let demo = self.demo;
                let media = self.media.clone();
                let media_bundle = self.media_bundle.clone();
                if let Some(bundle) = &media_bundle {
                    bundle.set(None);
                }
                let dispatcher = self.dispatcher.clone();
                let last_frame = self.last_frame.take();
                let personal = std::mem::take(&mut self.personal);
                *self = Self::default();
                {
                    self.personal = personal;
                }
                self.last_frame = last_frame;
                self.pending_slide = (page != Page::Destination).then_some(-1.0);
                self.journey.name = name;
                self.avatar = avatar;
                self.capture = capture;
                self.linux = linux;
                self.live = live;
                self.demo = demo;
                self.media = media;
                self.media_bundle = media_bundle;
                self.dispatcher = dispatcher;
                return Ok(());
            }
            InstallerPreviewEffect::SetPreviewName { value } if page == Page::Profile => {
                self.field_values[0].set(value.clone());
                self.journey.name = value;
                return Ok(());
            }
            InstallerPreviewEffect::SetPreviewDeviceName { value }
                if page == Page::Devices
                    || (page == Page::Connect
                        && self.journey.transfer.as_deref() == Some("device")) =>
            {
                if self.device_label_locked() {
                    return Err(self.device_label_locked_message());
                }
                if value.len() > 64 || value.chars().any(char::is_control) {
                    return Err("installer-preview-device-label-invalid");
                }
                self.field_values[1].set(value.clone());
                self.journey.device_name = value;
                if self.live
                    && !choices_edit
                    && let Some(prepared) = self.personal.prepared_handle()
                {
                    self.restart
                        .choices_changed(prepared, self.journey.draft_choices());
                }
                self.dirty = true;
                return Ok(());
            }
            InstallerPreviewEffect::SetPreviewPassword { value } if page == Page::Password => {
                let enabled = self.password_continue_enabled();
                if self.journey.password != value {
                    self.checkpoint_signature = None;
                }
                self.field_values[2].set(value.clone());
                self.journey.password = value;
                self.clear_password_validation();
                self.dirty |= enabled != self.password_continue_enabled();
                return Ok(());
            }
            InstallerPreviewEffect::SetPreviewConfirmation { value }
                if page == Page::Password && !self.unlocking_preparation() =>
            {
                let enabled = self.password_continue_enabled();
                self.field_values[3].set(value.clone());
                self.journey.confirmation = value;
                self.clear_password_validation();
                self.dirty |= enabled != self.password_continue_enabled();
                return Ok(());
            }
            InstallerPreviewEffect::SetPreviewCategory { category, included }
                if page == Page::Belongings && self.journey.details =>
            {
                if matches!(category.as_str(), "bookmarks" | "history") {
                    let (domain, index) = if category == "bookmarks" {
                        (browser::Domain::Bookmarks, 0)
                    } else {
                        (browser::Domain::History, 1)
                    };
                    if included && !self.belongings.browser.available() {
                        return Err("installer-preview-invalid-choice");
                    }
                    self.journey.browser_domains[index] = included;
                    self.belongings.browser.set(domain, included);
                    if !choices_edit {
                        self.invalidate_migration_preparation();
                    }
                } else {
                    self.set_category(&category, included)?;
                }
                if category == "files" && !included {
                    self.migration_source_location = false;
                }
            }
            InstallerPreviewEffect::SetPreviewFlatpakApp { app, included }
                if self.live
                    && page == Page::Belongings
                    && self.journey.details
                    && self.journey.migrate == Some(true)
                    && self.journey.categories[1] =>
            {
                if !self
                    .flatpak_apps()
                    .iter()
                    .any(|item| item.id == app && item.selectable)
                {
                    return Err("installer-preview-invalid-choice");
                }
                let changed = if included {
                    self.journey.excluded_flatpak_apps.remove(&app)
                } else {
                    self.journey.excluded_flatpak_apps.insert(app)
                };
                if changed && !choices_edit {
                    self.invalidate_migration_preparation();
                }
            }
            InstallerPreviewEffect::SetPreviewServiceSession {
                source_id,
                service,
                included,
            } if self.live
                && page == Page::Belongings
                && self.journey.details
                && self.journey.migrate == Some(true)
                && self.journey.categories[2] =>
            {
                if !imager_backend::service_selection::supported() {
                    return Err("installer-preview-import-unavailable");
                }
                let service = match service.as_str() {
                    "slack" => imager_backend::browser_selection::Service::Slack,
                    "discord" => imager_backend::browser_selection::Service::Discord,
                    _ => return Err("installer-preview-invalid-choice"),
                };
                let changed = self
                    .belongings
                    .sessions
                    .set(&source_id, service, included)?;
                if changed && !choices_edit {
                    self.invalidate_migration_preparation();
                }
            }
            InstallerPreviewEffect::ToggleFlatpakAccessDetails
                if page == Page::Belongings
                    && self.journey.details
                    && self.journey.categories[1] =>
            {
                self.flatpak_access_details = !self.flatpak_access_details;
            }
            InstallerPreviewEffect::ToggleMigrationSourceLocation
                if self.live
                    && page == Page::Belongings
                    && self.journey.details
                    && self.journey.categories[0]
                    && self.belongings.home.is_some() =>
            {
                self.migration_source_location = !self.migration_source_location;
            }
            InstallerPreviewEffect::TogglePreviewDetails if page == Page::Belongings => {
                self.journey.details = !self.journey.details;
                self.migration_source_location = false;
            }
            _ => return Err("installer-preview-wrong-page"),
        }
        if self.journey.page == Page::Ready {
            self.field_values[2].set(String::new());
            self.field_values[3].set(String::new());
        }
        if self.journey.page != page {
            self.migration_source_location = false;
            self.pending_slide = Some(direction);
            if self.journey.page == Page::Storage
                && self.live
                && self.journey.destination == Some(Destination::Computer)
                && self.storage.facts.is_none()
            {
                self.storage.check();
            }
        }
        if self.live
            && save_choices
            && let Some(prepared) = self.personal.prepared_handle()
        {
            self.restart
                .choices_changed(prepared, self.journey.draft_choices());
        }
        if profile_edit && self.journey.page == Page::Review {
            self.profile_avatar_original = None;
            self.personal.commit_photo_edit();
        }
        self.error = None;
        self.dirty = true;
        Ok(())
    }

    fn poll_avatar(&mut self) -> bool {
        let Some(picker) = &self.avatar_picker else {
            return false;
        };
        let result = match picker.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                ui::PortalOutcome::Failed(String::new())
            }
        };
        self.avatar_picker = None;
        if self.journey.page != Page::Profile {
            return false;
        }
        match result {
            ui::PortalOutcome::Opened(path) => {
                let path = path.to_string_lossy().into_owned();
                if imager_backend::prepared_identity::require_supported().is_ok()
                    && self.personal.select_photo(path.clone()).is_err()
                {
                    self.error = Some("installer-preview-avatar-failed");
                    return true;
                }
                self.avatar = Some(path);
            }
            ui::PortalOutcome::Cancelled => return false,
            ui::PortalOutcome::Failed(_) => self.error = Some("installer-preview-avatar-failed"),
        }
        self.dirty = true;
        true
    }

    fn avatar(&self, name: &str) -> Avatar {
        let avatar = Avatar::person(name, "installer-person").size(AvatarSize::Hero);
        if let Some(photo) = self.personal.photo() {
            return avatar.with_profile_photo(photo);
        }
        #[cfg(not(unix))]
        if let Some(path) = &self.avatar {
            return avatar.with_image_url(self.images.clone(), path);
        }
        avatar
    }

    fn flatpak_apps(&self) -> Vec<FlatpakAppChoice> {
        if self.belongings_worker.is_some() || self.belongings.flatpak_unavailable {
            return Vec::new();
        }
        let Some(inventory) = &self.belongings.flatpak else {
            return Vec::new();
        };
        inventory
            .refs
            .iter()
            .filter(|item| {
                item.app_id().is_some()
                    && (item.installation == super::flatpak_transfer::Installation::User
                        || !inventory.refs.iter().any(|existing| {
                            existing.installation == super::flatpak_transfer::Installation::User
                                && existing.reference == item.reference
                        }))
            })
            .map(|item| {
                let with_home = self.journey.migrate == Some(true)
                    && self.journey.categories[0]
                    && self.journey.destination == Some(Destination::Another)
                    && item.installation == super::flatpak_transfer::Installation::User;
                let label = if item.name.trim().is_empty() {
                    item.app_id().unwrap_or_default()
                } else {
                    item.name.trim()
                };
                let mut name: String = label.chars().take(160).collect();
                if label.chars().count() > 160 {
                    name.push('…');
                }
                let already_here = self.journey.destination == Some(Destination::Desktop);
                let selectable = !already_here
                    && !with_home
                    && (item.installation != super::flatpak_transfer::Installation::User
                        || matches!(
                            self.journey.destination,
                            Some(Destination::Another | Destination::Computer)
                        ));
                let id = format!("{}|{}", item.installation.argument(), item.reference);
                FlatpakAppChoice {
                    included: !already_here
                        && self.journey.migrate == Some(true)
                        && (with_home
                            || (selectable
                                && self.journey.categories[1]
                                && !self.journey.excluded_flatpak_apps.contains(&id))),
                    id,
                    name,
                    selectable,
                    with_home: !already_here && with_home,
                    already_here,
                }
            })
            .collect()
    }

    fn flatpak_count(&self) -> usize {
        self.flatpak_apps()
            .iter()
            .filter(|item| item.selectable)
            .count()
    }

    fn selected_portable_apps(&self) -> Vec<super::flatpak_transfer::InstalledRef> {
        use super::flatpak_transfer::Installation;
        if !self.journey.categories[1] || self.journey.destination == Some(Destination::Desktop) {
            return Vec::new();
        }
        let Some(inventory) = &self.belongings.flatpak else {
            return Vec::new();
        };
        inventory
            .refs
            .iter()
            .filter(|item| {
                item.app_id().is_some()
                    && (item.installation != Installation::User
                        || self.journey.destination == Some(Destination::Computer)
                        || (self.journey.destination == Some(Destination::Another)
                            && !self.journey.categories[0]))
                    && (item.installation == Installation::User
                        || !inventory.refs.iter().any(|existing| {
                            existing.installation == Installation::User
                                && existing.reference == item.reference
                        }))
                    && !self.journey.excluded_flatpak_apps.contains(&format!(
                        "{}|{}",
                        item.installation.argument(),
                        item.reference
                    ))
            })
            .cloned()
            .collect()
    }

    fn selected_applications(&self) -> bool {
        self.journey.categories[1]
            && (!self.selected_portable_apps().is_empty()
                || self.flatpak_apps().iter().any(|app| app.with_home))
    }

    fn migration_categories(&self) -> [bool; 3] {
        [
            self.journey.categories[0],
            self.selected_applications(),
            self.journey.categories[2],
        ]
    }

    fn set_category(&mut self, category: &str, included: bool) -> Result<(), &'static str> {
        let index = CATEGORIES
            .iter()
            .position(|key| *key == category)
            .ok_or("installer-preview-invalid-choice")?;
        if index < 2 && self.journey.destination == Some(Destination::Desktop) {
            return Err("installer-desktop-belongings-in-place");
        }
        if included && !self.category_available(index) {
            return Err("installer-preview-invalid-choice");
        }
        self.journey.categories[index] = included;
        self.reconcile_categories();
        if !self.journey.editing_choices() {
            self.invalidate_migration_preparation();
        }
        Ok(())
    }

    fn category_available(&self, index: usize) -> bool {
        if index < 2 && self.journey.destination == Some(Destination::Desktop) {
            return false;
        }
        if !self.live || self.belongings_worker.is_some() {
            return true;
        }
        match index {
            0 => self.belongings.home.is_some() && !self.belongings.home_unavailable,
            1 => !self.belongings.flatpak_unavailable && self.flatpak_count() > 0,
            _ => true,
        }
    }

    fn reconcile_categories(&mut self) {
        for index in 0..2 {
            if !self.category_available(index) {
                self.journey.categories[index] = false;
            }
        }
    }

    fn migration_sources(
        &self,
    ) -> Result<
        (
            std::path::PathBuf,
            Vec<super::flatpak_transfer::InstalledRef>,
        ),
        &'static str,
    > {
        if self.belongings_worker.is_some() {
            return Err("installer-preview-transfer-busy");
        }
        if !self.journey.categories[0] && !self.journey.categories[1] {
            return Err("installer-preview-transfer-needs-review");
        }
        let home = self
            .belongings
            .home
            .clone()
            .ok_or("installer-preview-home-unavailable")?;
        if self.journey.categories[1]
            && (self.belongings.flatpak_unavailable || self.belongings.flatpak.is_none())
        {
            return Err("installer-preview-flatpak-unavailable");
        }
        let refs = self.selected_portable_apps();
        Ok((home, refs))
    }

    fn save_account_checkpoint(&mut self) -> Result<bool, &'static str> {
        let Some(signature) = self.accounts.checkpoint_selection() else {
            return Ok(false);
        };
        if self.checkpoint_signature == Some(signature) || self.checkpoint_pending.is_some() {
            return Ok(false);
        }
        let prepared = self
            .personal
            .prepared_handle()
            .ok_or("installer-accounts-save-failed")?;
        let operation = prepared
            .custody_directory()
            .parent()
            .ok_or("installer-accounts-save-failed")?;
        let root = operation.join("accounts");
        let binding = operation
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("installer-accounts-save-failed")?
            .to_owned();
        let group = prepared.public_key();
        self.accounts.checkpoint_save(
            root,
            binding,
            group,
            zeroize::Zeroizing::new(self.journey.password.as_bytes().to_vec()),
        )?;
        self.checkpoint_pending = Some(signature);
        Ok(true)
    }

    fn poll_account_checkpoint(&mut self) -> bool {
        if let Some(ready) = self.restart.take_checkpoint() {
            self.resume_draft = Some(ready);
            self.checkpoint_restored = true;
            self.checkpoint_signature = self.checkpoint_pending.take();
            if self.account_continue {
                self.account_continue = false;
                if let Err(error) = self.apply(InstallerPreviewEffect::ContinuePreview) {
                    self.error = Some(error);
                }
            }
            return true;
        }
        let Some(result) = self.accounts.checkpoint_poll() else {
            return false;
        };
        match result {
            Ok(Some(receipt)) => {
                if let Err(error) = self.restart.record_checkpoint(
                    receipt,
                    self.resume_draft
                        .as_ref()
                        .and_then(|ready| ready.account_checkpoint().cloned()),
                ) {
                    self.checkpoint_failed = true;
                    self.checkpoint_pending = None;
                    self.account_continue = false;
                    self.error = Some(error);
                }
            }
            Ok(None) if self.checkpoint_restoring => {
                self.checkpoint_restoring = false;
                self.checkpoint_failed = false;
                self.checkpoint_restored = true;
                self.checkpoint_signature = self.accounts.checkpoint_selection();
                self.journey.confirmation.clone_from(&self.journey.password);
                self.field_values[3].set(self.journey.confirmation.clone());
                if let Err(error) = self.apply(InstallerPreviewEffect::ContinuePreview) {
                    self.error = Some(error);
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.checkpoint_restoring = false;
                self.checkpoint_failed = true;
                self.checkpoint_pending = None;
                self.account_continue = false;
                if error == "installer-restart-changed" {
                    self.restart.mark_changed();
                }
                self.error = Some(error);
            }
        }
        true
    }

    fn resume_preparation(
        &mut self,
        resumed: restart_controller::Resumed,
    ) -> Result<(), &'static str> {
        let metadata = resumed.metadata;
        if resumed.choices.destination == Some(draft::DestinationChoice::Desktop) {
            self.retire_preparation();
            self.journey
                .restore_draft(&resumed.choices, resumed.kind, String::new());
            self.prepare_desktop()?;
            return Ok(());
        }
        if resumed.kind == draft::IdentityKind::Enrolled {
            self.pairing = pairing::Pairing::resume(resumed.prepared.clone())
                .map_err(|_| "installer-restart-unavailable")?;
        }
        if matches!(
            resumed.kind,
            draft::IdentityKind::Backup | draft::IdentityKind::CurrentDevice
        ) {
            self.existing_identity =
                existing_identity::ExistingIdentity::resume(resumed.prepared.clone());
        }
        self.personal
            .resume(resumed.prepared, resumed.kind, metadata.clone())?;
        self.journey
            .restore_draft(&resumed.choices, resumed.kind, metadata.display_name);
        self.avatar = None;
        self.field_values[2].set(String::new());
        self.field_values[3].set(String::new());
        self.invalidate_migration_preparation();
        // Choices describe the previous intent; none are renewed source,
        // account or storage grants. These controllers start with no authority.
        self.belongings = belongings::Belongings::default();
        self.accounts = accounts::Accounts::default();
        self.accounts.require_saved_review();
        self.storage = storage::Storage::default();
        self.resume_draft = Some(resumed.draft);
        {
            self.checkpoint_restored = false;
            self.checkpoint_signature = None;
            self.checkpoint_pending = None;
            self.checkpoint_failed = false;
        }
        self.error = None;
        Ok(())
    }

    fn retire_preparation(&mut self) {
        self.personal = personal::Personal::default();
        self.existing_identity = existing_identity::ExistingIdentity::default();
        self.pairing = pairing::Pairing::default();
        self.journey = Journey::new(String::new());
        self.journey.device_name = t("installer-preview-device-default");
        self.avatar = None;
        self.resume_draft = None;
        self.accounts = accounts::Accounts::default();
        self.account_actions = super::account_transfer::AccountActions::default();
        self.account_answer = serde_json::Value::Null;
        self.belongings = belongings::Belongings::default();
        self.belongings_worker = None;
        self.storage = storage::Storage::default();
        self.invalidate_migration_preparation();
        {
            self.checkpoint_restoring = false;
            self.checkpoint_restored = false;
            self.checkpoint_signature = None;
            self.checkpoint_pending = None;
            self.checkpoint_failed = false;
            self.account_continue = false;
        }
        for value in &self.field_values {
            value.set(String::new());
        }
        self.error = None;
    }

    fn drain(&mut self) -> bool {
        // Register the child after the current semantic invocation has finished;
        // its published effects must not replace the executing surface's door.
        let desktop_review_changed = std::mem::take(&mut self.desktop_review_pending);
        if desktop_review_changed && let Err(error) = self.prepare_desktop() {
            self.error = Some(error);
            self.dirty = true;
        }
        if self.windows.enabled && self.live {
            let changed = self.poll_windows();
            self.dirty |= changed;
        }
        let navigation_changed = if let Some(returning) = &self.media_return
            && let Some(reason) = returning.get()
        {
            returning.set(None);
            self.media_preparation.require_source_review(reason);
            if let Err(error) = self.apply(InstallerPreviewEffect::ReviewChangedFiles) {
                self.error = Some(error);
            }
            self.dirty = true;
            true
        } else {
            false
        };
        let restart_changed = if self.live {
            if let Some(result) = self.restart.poll() {
                match result {
                    Ok(Some(resumed)) => {
                        if let Err(error) = self.resume_preparation(resumed) {
                            self.error = Some(error);
                        }
                    }
                    Ok(None) if self.restart.state() == "retired" => self.retire_preparation(),
                    Ok(None) => {}
                    Err(()) => {
                        self.error = Some("installer-restart-unavailable");
                        if self.checkpoint_pending.take().is_some() {
                            self.checkpoint_failed = true;
                            self.account_continue = false;
                            self.error = Some("installer-accounts-save-failed");
                        }
                    }
                }
                true
            } else {
                false
            }
        } else {
            false
        };

        let media_progress_changed = self.media_preparation.take_progress_changed();
        let media_changed = if let Some(result) = self.media_preparation.poll() {
            match result {
                Ok(prepared) if self.direct.belongings_pending => {
                    self.direct.belongings_pending = false;
                    self.direct.prepared_belongings = Some(prepared);
                    if self.direct.belongings_continue {
                        self.direct.belongings_continue = false;
                        match self.advance_journey() {
                            Ok(()) => {
                                self.pending_slide = Some(1.0);
                                if self.journey.page == Page::Storage
                                    && self.storage.facts.is_none()
                                {
                                    self.storage.check();
                                }
                            }
                            Err(error) => self.error = Some(error),
                        }
                    }
                }
                Ok(prepared) if self.desktop_pending => {
                    if let Err(error) = self.finish_desktop(prepared) {
                        self.error = Some(error);
                    }
                }
                Ok(prepared) if self.direct.pending.is_some() => {
                    if let Err(error) = self.finish_direct(prepared) {
                        self.error = Some(error);
                    }
                }
                Ok(prepared) => {
                    if let (Some(bundle), Some(media), Some(dispatcher)) =
                        (&self.media_bundle, &self.media, &self.dispatcher)
                    {
                        bundle.set(Some(prepared));
                        dispatcher.submit(
                            Imager::paths()
                                .profile
                                .user
                                .full_name
                                .replace(self.journey.name.clone()),
                        );
                        dispatcher.submit(
                            Imager::paths()
                                .profile
                                .device_label
                                .replace(self.journey.device_name.clone()),
                        );
                        media.set(true);
                    } else {
                        self.error = Some("installer-preview-storage-unavailable");
                    }
                }
                Err(error) => {
                    let error = {
                        self.direct.pending = None;
                        let belongings = self.direct.belongings_pending;
                        self.direct.belongings_pending = false;
                        self.direct.belongings_continue = false;
                        self.desktop_pending = false;
                        if belongings && error == "installer-media-prepare-failed" {
                            "installer-migration-prepare-failed"
                        } else {
                            error
                        }
                    };
                    self.error = Some(error);
                }
            }
            true
        } else {
            false
        };
        let media_changed = media_changed || media_progress_changed;
        let migration_changed = if let Some(result) = self.migration_selection.poll() {
            match result {
                Ok(true) if self.journey.page == Page::Belongings => match self.advance_journey() {
                    Ok(()) => {
                        self.storage.set_application_minimum(
                            self.migration_selection.application_minimum(),
                        );
                        self.pending_slide = Some(1.0);
                        if self.journey.page == Page::Storage && self.storage.facts.is_none() {
                            self.storage.check();
                        }
                    }
                    Err(error) => self.error = Some(error),
                },
                Err(error) => self.error = Some(error),
                _ => {}
            }
            self.dirty = true;
            true
        } else {
            false
        };
        let portable_changed = if let Some(result) = self.portable_source.poll() {
            match result {
                Ok(true) if self.journey.page == Page::Belongings => match self.advance_journey() {
                    Ok(()) => self.pending_slide = Some(1.0),
                    Err(error) => self.error = Some(error),
                },
                Err(error) => self.error = Some(error),
                _ => {}
            }
            self.dirty = true;
            true
        } else {
            false
        };
        let avatar_changed = self.poll_avatar();
        let storage_changed = self.storage.poll()
            | self.recovery.poll()
            | self
                .recovery
                .poll_selection(self.belongings.windows.as_ref());
        let transfer_changed = self.user_transfer.poll();
        let promotion_changed = self.home_promotion.poll();
        let checkpoint_changed = self.poll_account_checkpoint();
        let account_page = self.accounts.navigation_step();
        let accounts_changed = self.accounts.poll();
        if self.journey.page == Page::Accounts && self.accounts.navigation_step() != account_page {
            self.pending_slide = Some(if self.accounts.navigation_step() == "overview" {
                -1.0
            } else {
                1.0
            });
        }
        if self.live
            && self.journey.page == Page::Accounts
            && !self.checkpoint_failed
            && !self.accounts.active()
            && !self.restart.busy()
        {
            if let Err(error) = self.save_account_checkpoint() {
                self.checkpoint_failed = true;
                self.error = Some(error);
            }
        }
        let personal_changed = self.personal.poll();
        let existing_changed = self.existing_identity.poll();
        let pairing_changed = self.pairing.poll();
        if pairing_changed
            && self.journey.identity == Some(Identity::Existing)
            && self.journey.transfer.as_deref() == Some("device")
            && self.pairing.ready()
            && let Some(prepared) = self.pairing.prepared()
        {
            if let Ok(metadata) = prepared.metadata() {
                self.journey.name = metadata.display_name;
            }
            if let Err(error) = self.personal.adopt_for_journey(
                prepared,
                self.journey.draft_choices(),
                draft::IdentityKind::Enrolled,
            ) {
                self.error = Some(error);
            }
        }

        if existing_changed && self.existing_identity.ready() {
            if let Some(prepared) = self.existing_identity.prepared() {
                if let Ok(metadata) = prepared.metadata() {
                    self.journey.name = metadata.display_name;
                }
                if let Some(label) = prepared.enrolled_device_label() {
                    label.clone_into(&mut self.journey.device_name);
                }
                if let Err(error) = self.personal.adopt_for_journey(
                    prepared,
                    self.journey.draft_choices(),
                    if self.journey.transfer.as_deref() == Some("current") {
                        draft::IdentityKind::CurrentDevice
                    } else {
                        draft::IdentityKind::Backup
                    },
                ) {
                    self.error = Some(error);
                }
            }
        }

        if personal_changed {
            if let Some(page) = self.personal_continue.take() {
                if self.personal.ready() && self.journey.page == page {
                    match self.advance_journey() {
                        Ok(()) => {
                            if page == Page::Profile {
                                self.profile_avatar_original = None;
                                self.personal.commit_photo_edit();
                            }
                            self.pending_slide = Some(1.0);
                        }
                        Err(error) => self.error = Some(error),
                    }
                } else {
                    self.error = Some("installer-preview-identity-prepare-failed");
                }
            }
        }

        let folder_changed = if let Some(worker) = &self.belongings_worker {
            match worker.try_recv() {
                Ok(belongings) => {
                    if let Some(roots) = belongings.roots.clone() {
                        let _ = self
                            .accounts
                            .update_sources(roots.clone(), belongings.accounts.clone());
                        let _ = self
                            .account_actions
                            .update_sources(roots, belongings.accounts.clone());
                    }
                    self.belongings = belongings;
                    {
                        self.belongings
                            .browser
                            .set(browser::Domain::Bookmarks, self.journey.browser_domains[0]);
                        self.belongings
                            .browser
                            .set(browser::Domain::History, self.journey.browser_domains[1]);
                    }
                    self.belongings_worker = None;
                    self.reconcile_categories();
                    true
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.belongings_worker = None;
                    self.error = Some("installer-preview-belongings-check-failed");
                    true
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => false,
            }
        } else {
            false
        };
        self.dirty |= checkpoint_changed
            || restart_changed
            || migration_changed
            || storage_changed
            || portable_changed
            || media_changed
            || navigation_changed
            || folder_changed
            || transfer_changed
            || promotion_changed
            || accounts_changed
            || personal_changed
            || existing_changed
            || pairing_changed;
        let commands = std::mem::take(&mut *self.commands.borrow_mut());
        let changed = desktop_review_changed
            || checkpoint_changed
            || restart_changed
            || migration_changed
            || media_changed
            || navigation_changed
            || portable_changed
            || avatar_changed
            || storage_changed
            || folder_changed
            || transfer_changed
            || promotion_changed
            || accounts_changed
            || personal_changed
            || existing_changed
            || pairing_changed
            || !commands.is_empty();
        for command in commands {
            let submitted = matches!(command, InstallerPreviewEffect::ContinuePreview);
            if let Err(error) = self.apply(command) {
                self.show_refusal(error, submitted);
            }
        }
        let focus_confirmation = self.password_next_focus.replace(false)
            && self.journey.page == Page::Password
            && !self.unlocking_preparation()
            && !self.personal_busy()
            && !self.migration_busy();
        if focus_confirmation {
            self.validation_focus = Some("installer.confirmation".into());
            self.dirty = true;
        }
        changed || focus_confirmation
    }

    fn button(&self, id: &str, label: &str, effect: InstallerPreviewEffect) -> Button {
        let commands = self.commands.clone();
        let mut enabled = !self.personal_busy()
            && !self.migration_busy()
            && !(self.windows.enabled && self.windows.busy());
        if self.journey.page == Page::Password
            && matches!(effect, InstallerPreviewEffect::ContinuePreview)
        {
            enabled &= self.password_continue_enabled();
        }
        if self.journey.page == Page::Profile
            && matches!(effect, InstallerPreviewEffect::ContinuePreview)
            && self.avatar_picker.is_some()
        {
            enabled = false;
        }
        if self.journey.editing_profile()
            && matches!(effect, InstallerPreviewEffect::BackInPreview)
            && self.personal.cancellable_photo_read()
            && !self.migration_busy()
            && !self.existing_identity.busy()
        {
            enabled = true;
        }

        if matches!(effect, InstallerPreviewEffect::CancelMountedHomePermission)
            && self.live
            && self.journey.page == Page::Belongings
            && self.portable_source.mounted_permission_active()
            && self.portable_source.busy()
            && !self.personal_busy()
            && !self.migration_selection.busy()
        {
            enabled = true;
        }
        if matches!(effect, InstallerPreviewEffect::CancelMediaPreparation)
            && self.media_preparation.busy()
        {
            enabled = true;
        }
        if matches!(effect, InstallerPreviewEffect::BackInPreview)
            && self.migration_selection.choosing()
            && !self.media_preparation.busy()
            && !self.personal_busy()
        {
            enabled = true;
        }
        if matches!(effect, InstallerPreviewEffect::BackInPreview)
            && self.portable_source.busy()
            && !self.migration_selection.busy()
            && !self.personal_busy()
        {
            enabled = true;
        }
        Button::new(id, &t(label))
            .with_enabled(enabled)
            .on_effect(move || {
                if matches!(effect, InstallerPreviewEffect::ContinuePreview) {
                    queue_continue(&commands);
                } else {
                    commands.borrow_mut().push(effect.clone());
                }
            })
    }

    fn review_row(&self, section: &str, title: String, secondary: String) -> ListRow {
        self.review_selection_row(section, section, title, secondary)
    }

    fn review_identity_row(&self) -> ListRow {
        let j = &self.journey;
        let name = if j.identity == Some(Identity::Existing) && !self.live {
            t("installer-preview-existing-avatar")
        } else {
            j.name.clone()
        };
        let row = self
            .review_row(
                if j.identity == Some(Identity::Existing) {
                    "identity"
                } else {
                    "profile"
                },
                name.clone(),
                t("installer-review-person-body"),
            )
            .avatar(name, "installer-person");
        if let Some(photo) = self.personal.photo() {
            return row.with_profile_photo(photo);
        }
        row
    }

    fn device_label_locked(&self) -> bool {
        if self.journey.identity == Some(Identity::Existing)
            && self.journey.transfer.as_deref() == Some("device")
            && self.pairing.locked_label()
        {
            return true;
        }
        if self
            .personal
            .prepared_handle()
            .is_some_and(|prepared| prepared.enrolled_device_label().is_some())
        {
            return true;
        }
        false
    }

    fn device_label_locked_message(&self) -> &'static str {
        if self
            .personal
            .prepared_handle()
            .is_some_and(|prepared| prepared.current_source().is_some())
        {
            return "installer-current-device-name-locked";
        }
        "installer-pairing-label-locked"
    }

    fn review_device_row(&self) -> ListRow {
        if self.device_label_locked() {
            ListRow::new("installer.review.devices", self.journey.device_name.clone())
                .secondary(t(
                    if self.device_label_locked_message() == "installer-current-device-name-locked"
                    {
                        "installer-review-device-current"
                    } else {
                        "installer-review-device-enrolled"
                    },
                ))
                .two_line_secondary()
                .icon(Icon::Monitor)
                .with_enabled(false)
        } else {
            self.review_row(
                "devices",
                self.journey.device_name.clone(),
                t("installer-review-device-body"),
            )
            .icon(Icon::Monitor)
        }
    }

    fn review_selection_row(
        &self,
        id: &str,
        section: &str,
        title: String,
        secondary: String,
    ) -> ListRow {
        let commands = self.commands.clone();
        let section = section.to_string();
        ListRow::new(format!("installer.review.{id}"), title)
            .secondary(secondary)
            .two_line_secondary()
            .disclosure()
            .on_activate(move || {
                commands
                    .borrow_mut()
                    .push(InstallerPreviewEffect::EditPreview {
                        section: section.clone(),
                    })
            })
    }

    fn usb_backend_available(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.dispatcher.is_some() && self.media.is_some() && self.media_bundle.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    fn has_storage_decision(&self) -> bool {
        self.journey.destination == Some(Destination::Computer) && self.direct_route_enabled()
    }

    fn advance_journey(&mut self) -> Result<(), &'static str> {
        if self.live
            && self.journey.destination == Some(Destination::Computer)
            && (matches!(self.journey.page, Page::Belongings | Page::Accounts)
                || (self.journey.page == Page::Devices && self.journey.migrate == Some(false)))
            && !self.journey.saving_edit()
            && self.direct.prepared_belongings.is_none()
        {
            let mut next = self.journey.clone();
            next.next()?;
            if next.page != Page::Accounts {
                self.prepare_belongings()?;
                self.direct.belongings_continue = true;
                return Ok(());
            }
        }
        self.journey.next()?;
        if self.live && !self.has_storage_decision() {
            if self.journey.page == Page::Storage {
                self.journey.next()?;
            }
            // Back visits only pages with choices in the current live route.
            self.journey.history.retain(|page| *page != Page::Storage);
        }
        if self.live
            && self.journey.page == Page::Belongings
            && self.journey.migrate == Some(true)
            && self.belongings.roots.is_none()
            && self.belongings_worker.is_none()
        {
            // Choosing migration is a direction, not a reason to inspect Home
            // and account stores before the person finishes their identity.
            // Resumed choices also need fresh discovery, never restored grants.
            self.apply(InstallerPreviewEffect::CheckBelongings)?;
        }
        Ok(())
    }

    fn identity_route_available(&self, route: &str) -> bool {
        if route == "current" {
            return self.linux && self.journey.destination == Some(Destination::Desktop);
        }
        if !self.live {
            return true;
        }
        match route {
            "device" => cfg!(target_os = "linux"),
            "backup" => cfg!(unix),
            "current" => {
                cfg!(target_os = "linux") && self.journey.destination == Some(Destination::Desktop)
            }
            _ => false,
        }
    }

    fn choice(&self, key: &str, selected: bool) -> Card {
        let commands = self.commands.clone();
        let choice = key.strip_prefix("purpose-").unwrap_or(key).to_string();
        let icon = match key {
            "computer" | "purpose-computer" | "purpose-server" => Icon::Monitor,
            "another" => Icon::Usb,
            "desktop" | "device" | "current" => Icon::MonitorSmartphone,
            "migrate" => Icon::Folder,
            "fresh" => Icon::Sparkles,
            "new" => Icon::User,
            "existing" => Icon::Users,
            _ => Icon::Shield,
        };
        let title = t(&format!("installer-preview-{key}-title"));
        let description = if key == "computer" && !self.direct_route_enabled() {
            t("installer-preview-computer-usb-body")
        } else if key == "migrate" && self.journey.destination == Some(Destination::Computer) {
            t("installer-preview-migrate-shared-body")
        } else if key == "migrate" && self.journey.destination == Some(Destination::Another) {
            t("installer-preview-migrate-portable-body")
        } else if key == "device" && self.live && !cfg!(target_os = "linux") {
            t("installer-pairing-helper-unavailable")
        } else {
            t(&format!("installer-preview-{key}-body"))
        };
        let copy = page::related()
            .push(Box::new(Text::new(&title).with_role(TextRole::Title)))
            .push(Box::new(Text::new(&description)));
        let contents = Row::new()
            .with_rhythm(Rhythm::Group)
            .push(Box::new(
                IconWidget::new(icon)
                    .sized(ui::widget::IconSize::Large)
                    .accent(),
            ))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(copy)),
            ))
            .push(Box::new(IconWidget::new(Icon::ChevronRight)));
        Card::new(Box::new(contents))
            .with_accessible_name(format!("{title}. {description}"))
            .selected(selected)
            .on_activate(format!("installer.choice.{key}"), move || {
                let mut queue = commands.borrow_mut();
                queue.push(InstallerPreviewEffect::ChoosePreview {
                    choice: choice.clone(),
                });
                queue.push(InstallerPreviewEffect::ContinuePreview);
            })
    }

    fn clear_password_validation(&mut self) {
        if self.password_validation().is_some() {
            self.error = None;
            self.validation_focus = None;
            self.dirty = true;
        }
    }

    fn show_refusal(&mut self, error: &'static str, submitted: bool) {
        self.error = Some(error);
        self.dirty = true;
        // Only a refused submission moves focus. Read-only observations and
        // ordinary edits leave the person's typing and scroll position alone.
        if submitted && let Some((field, _)) = self.password_validation() {
            self.validation_focus = Some(format!("installer.{field}"));
        }
    }

    fn password_continue_enabled(&self) -> bool {
        if self.unlocking_preparation() {
            !self.journey.password.is_empty()
        } else {
            self.journey.validation().is_none()
        }
    }

    fn password_validation(&self) -> Option<(&'static str, &'static str)> {
        if self.journey.page != Page::Password {
            return None;
        }
        match self.error {
            Some(
                error @ ("installer-preview-password-short"
                | "installer-preview-password-spaces"
                | "installer-preview-password-common"
                | "installer-preview-password-numeric"
                | "installer-preview-password-repetitive"
                | "installer-preview-password-invalid"),
            ) => Some(("password", error)),
            Some("installer-preview-password-mismatch") => {
                Some(("confirmation", "installer-preview-password-mismatch"))
            }
            _ => None,
        }
    }

    fn field(&self, key: &str, value: &str, kind: TextInputKind) -> Field {
        let commands = self.commands.clone();
        let submits = self.commands.clone();
        let next_focus = self.password_next_focus.clone();
        let focus_confirmation = key == "password" && !self.unlocking_preparation();
        let submit_enabled = !self.personal_busy() && !self.migration_busy();
        let field = key.to_string();
        let index = match key {
            "name" => 0,
            "device-name" => 1,
            "password" => 2,
            _ => 3,
        };
        self.field_values[index].set(value.to_string());
        let mut widget = Field::new(
            t(&format!("installer-preview-{key}-label")),
            TextInput::from_state(&self.field_values[index])
                .with_id(&format!("installer.{key}"))
                .with_kind(kind)
                .with_secret(kind == TextInputKind::Password)
                .on_change(move |value| {
                    let value = value.to_string();
                    let effect = match field.as_str() {
                        "name" => InstallerPreviewEffect::SetPreviewName { value },
                        "device-name" => InstallerPreviewEffect::SetPreviewDeviceName { value },
                        "password" => InstallerPreviewEffect::SetPreviewPassword { value },
                        _ => InstallerPreviewEffect::SetPreviewConfirmation { value },
                    };
                    commands.borrow_mut().push(effect);
                })
                .on_submit(move |_| {
                    if submit_enabled {
                        if focus_confirmation {
                            next_focus.set(true);
                        } else {
                            queue_continue(&submits);
                        }
                    }
                }),
        )
        .with_label_placement(Some(LabelPlacement::Above));
        if let Some((target, error)) = self.password_validation()
            && target == key
        {
            widget = widget.with_error_live(t(error), ui::a11y::Politeness::Assertive);
        } else if key == "password" && !self.unlocking_preparation() {
            widget = widget.with_help(t("installer-preview-password-hint"));
        }
        widget
    }

    fn body(&self, theme: &Theme) -> ui::widget::Column {
        let j = &self.journey;
        let key = if j.page == Page::Storage {
            match j.destination {
                Some(Destination::Another) => "usb",
                Some(Destination::Desktop) => "desktop",
                _ if !self.direct_route_enabled() => "storage-usb-fallback",
                _ => "storage",
            }
        } else {
            j.page.key()
        };
        let heading_role = if matches!(j.page, Page::Destination | Page::Profile | Page::Password) {
            TextRole::Display
        } else {
            TextRole::LargeTitle
        };
        let choosing_pair_name = imager_backend::pairing::Pairing::supported()
            && self.live
            && j.page == Page::Connect
            && j.transfer.as_deref() == Some("device")
            && !self.pairing.started();
        let mounted_step = self.live
            && j.page == Page::Belongings
            && j.destination == Some(Destination::Another)
            && j.categories[0]
            && self.portable_source.mounted_permission_active();
        let heading = if mounted_step {
            t("installer-mounted-home-permission-heading")
        } else if self.unlocking_preparation() {
            t("installer-restart-unlock-heading")
        } else if choosing_pair_name {
            t("installer-pairing-name-heading")
        } else if j.page == Page::Accounts {
            self.accounts.heading()
        } else {
            t(&format!("installer-preview-{key}-heading"))
        };
        let intro = if matches!(j.page, Page::Destination | Page::Migration)
            && !self.guided_host_available()
        {
            t("installer-guided-host-unavailable")
        } else if mounted_step {
            t("installer-mounted-home-grant-intro")
        } else if self.unlocking_preparation() {
            t("installer-restart-unlock-intro")
        } else if j.page == Page::Accounts {
            self.accounts.subheading()
        } else if j.page == Page::Belongings && j.destination == Some(Destination::Desktop) {
            t("installer-desktop-belongings-intro")
        } else if j.page == Page::Belongings
            && j.destination == Some(Destination::Computer)
            && j.categories[0]
        {
            t("installer-preview-belongings-shared-intro")
        } else if j.page == Page::Migration {
            t("installer-preview-migration-intro")
        } else if matches!(j.page, Page::Identity | Page::Connect) {
            String::new()
        } else if j.page == Page::Password && j.destination == Some(Destination::Desktop) {
            t("installer-desktop-password-intro")
        } else {
            t(&format!("installer-preview-{key}-intro"))
        };
        let intro =
            if self.live && j.page == Page::Connect && j.transfer.as_deref() == Some("device") {
                self.pairing.intro()
            } else {
                intro
            };
        let heading = Text::new(&heading)
            .with_id("installer.heading")
            .with_role(heading_role)
            .with_balanced_wrap()
            .with_heading_level(1);
        let heading = if j.page == Page::Profile {
            heading.with_align(ParagraphAlign::Center)
        } else {
            heading
        };
        let mut title = page::stack().push(Box::new(heading));
        if j.page != Page::Profile && self.rect.width() < self.profile_breakpoint {
            // Give a compact page title breathing room before the scrolling
            // edge. The form and explanatory copy keep their full measure.
            title =
                title.with_horizontal_padding(0.0, theme.tokens.spacing.rhythm.get(Rhythm::Group));
        }
        let mut header = page::stack()
            .with_rhythm(Rhythm::Related)
            .push(Box::new(title));
        if !intro.is_empty() {
            let intro = Text::new(&intro).with_role(TextRole::Body);
            let intro = if j.page == Page::Profile {
                intro.with_align(ParagraphAlign::Center)
            } else {
                intro
            };
            header = header.push(Box::new(intro));
        }
        let mut body = page::stack()
            .with_rhythm(Rhythm::Sibling)
            .push(Box::new(header));
        if self.live && self.direct.belongings_pending && j.page != Page::Review {
            body = body.push(Box::new(
                Text::new(&t(self
                    .media_preparation
                    .phase_key()
                    .as_deref()
                    .unwrap_or("installer-media-phase-review")))
                .with_live(ui::a11y::Politeness::Polite),
            ));
        }
        match j.page {
            Page::Migration if !self.guided_host_available() => {}
            Page::Destination => {
                body =
                    body.push(Box::new(
                        self.choice("computer", j.destination == Some(Destination::Computer)),
                    ))
                    .push(Box::new(
                        self.choice("another", j.destination == Some(Destination::Another)),
                    ));
                if self.linux {
                    body = body.push(Box::new(
                        self.choice("desktop", j.destination == Some(Destination::Desktop)),
                    ));
                }
            }
            Page::Migration => {
                body = body
                    .push(Box::new(self.choice("migrate", j.migrate == Some(true))))
                    .push(Box::new(self.choice("fresh", j.migrate == Some(false))));
            }
            Page::Identity => {
                body = body
                    .push(Box::new(
                        self.choice("new", j.identity == Some(Identity::New)),
                    ))
                    .push(Box::new(
                        self.choice("existing", j.identity == Some(Identity::Existing)),
                    ));
            }
            Page::Profile => {
                // Bind the current name before constructing its retained avatar.
                let name = self.field("name", &j.name, TextInputKind::Name);
                let picture = page::related()
                    .push(Box::new(
                        Row::new()
                            .with_justify(taffy::JustifyContent::Center)
                            .push(Box::new(
                                Container::new()
                                    .with_no_padding()
                                    .hug()
                                    .with_id("installer.profile.avatar")
                                    .with_child(Box::new(
                                        self.avatar(&j.name)
                                            .with_name_state(&self.field_values[0])
                                            .size(AvatarSize::Portrait),
                                    )),
                            )),
                    ))
                    .push(Box::new(
                        Row::new()
                            .with_justify(taffy::JustifyContent::Center)
                            .push(Box::new(
                                self.button(
                                    "installer.avatar",
                                    "installer-preview-avatar-change",
                                    InstallerPreviewEffect::ChoosePreviewAvatar,
                                )
                                .role(ButtonRole::Plain),
                            )),
                    ))
                    .push(Box::new(
                        Text::new(&t("installer-preview-profile-avatar"))
                            .with_id("installer.profile.avatar-help")
                            .with_role(TextRole::Caption)
                            .with_align(ParagraphAlign::Center),
                    ));
                body = body
                    .with_rhythm(Rhythm::Section)
                    .push(Box::new(picture))
                    .push(Box::new(name));
            }
            Page::Password => {
                body = body.push(Box::new(self.field(
                    "password",
                    &j.password,
                    TextInputKind::Password,
                )));
                if !self.unlocking_preparation() {
                    body = body.push(Box::new(self.field(
                        "confirmation",
                        &j.confirmation,
                        TextInputKind::Password,
                    )));
                }
            }
            Page::Connect => {
                if self.live
                    && self.journey.transfer.as_deref() == Some("device")
                    && self.identity_route_available("device")
                {
                    if !self.pairing.started() {
                        body = body.push(Box::new(self.field(
                            "device-name",
                            &j.device_name,
                            TextInputKind::Name,
                        )));
                    }
                    let commands = self.commands.clone();
                    let cancel_commands = self.commands.clone();
                    body = body.push(self.pairing.view(
                        Rc::new(move || {
                            commands
                                .borrow_mut()
                                .push(InstallerPreviewEffect::StartExistingDevicePairing)
                        }),
                        Rc::new(move || {
                            cancel_commands
                                .borrow_mut()
                                .push(InstallerPreviewEffect::CancelExistingDevicePairing)
                        }),
                    ));
                    return body;
                }

                if self.live && self.journey.transfer.as_deref() == Some("backup") {
                    let commands = self.commands.clone();
                    let edit_commands = self.commands.clone();
                    let submits = self.commands.clone();
                    body = body.push(self.existing_identity.view(
                        Rc::new(move || {
                            commands
                                .borrow_mut()
                                .push(InstallerPreviewEffect::ChooseExistingIdentityBackup)
                        }),
                        Rc::new(move |value| {
                            edit_commands
                                .borrow_mut()
                                .push(InstallerPreviewEffect::SetExistingIdentityPassword { value })
                        }),
                        Rc::new(move || queue_continue(&submits)),
                    ));
                    return body;
                }
                if self.live && self.journey.transfer.as_deref() == Some("current") {
                    body = body.push(self.existing_identity.current_view());
                    return body;
                }
                if self.identity_route_available("current") {
                    body = body.push(Box::new(
                        self.choice("current", j.transfer.as_deref() == Some("current")),
                    ));
                }
                if self.identity_route_available("device") {
                    body = body.push(Box::new(
                        self.choice("device", j.transfer.as_deref() == Some("device")),
                    ));
                }
                if self.identity_route_available("backup") {
                    body = body.push(Box::new(
                        self.choice("backup", j.transfer.as_deref() == Some("backup")),
                    ));
                }
                if !self.identity_route_available("device")
                    && !self.identity_route_available("backup")
                    && !self.identity_route_available("current")
                {
                    body = body.push(Box::new(Text::new(&t(
                        "installer-identity-route-unavailable",
                    ))));
                }
            }
            Page::Purpose => {
                body = body
                    .push(Box::new(
                        self.choice("purpose-computer", j.purpose == "computer"),
                    ))
                    .push(Box::new(
                        self.choice("purpose-server", j.purpose == "server"),
                    ));
            }
            Page::Devices => {
                let identity_label = if j.identity == Some(Identity::Existing) && !self.live {
                    t("installer-preview-existing-avatar")
                } else {
                    j.name.clone()
                };
                let identity_avatar = if j.identity == Some(Identity::Existing) && !self.live {
                    Avatar::person(&identity_label, "existing-identity").size(AvatarSize::Hero)
                } else {
                    self.avatar(&identity_label)
                };
                let person = page::related()
                    .push(Box::new(
                        Row::new()
                            .with_justify(taffy::JustifyContent::Center)
                            .push(Box::new(identity_avatar)),
                    ))
                    .push(Box::new(
                        Row::new()
                            .with_justify(taffy::JustifyContent::Center)
                            .push(Box::new(
                                Text::new(&identity_label).with_role(TextRole::Heading),
                            )),
                    ));
                let device = |icon, label: String| {
                    Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_child(Box::new(
                            page::related()
                                .push(Box::new(
                                    Row::new().with_justify(taffy::JustifyContent::Center).push(
                                        Box::new(
                                            IconWidget::new(icon)
                                                .sized(ui::widget::IconSize::Large)
                                                .accent(),
                                        ),
                                    ),
                                ))
                                .push(Box::new(
                                    Row::new()
                                        .with_justify(taffy::JustifyContent::Center)
                                        .push(Box::new(Text::new(&label))),
                                )),
                        ))
                };
                body = body.push(Box::new(
                    page::stack()
                        .with_rhythm(Rhythm::Related)
                        .push(Box::new(person))
                        .push(Box::new(diagram::DeviceLinks::new(!self.capture)))
                        .push(Box::new(
                            Row::new()
                                .with_padding(0.0, 0.0)
                                .push(Box::new(device(Icon::Monitor, j.device_name.clone())))
                                .push(Box::new(device(
                                    Icon::MonitorSmartphone,
                                    t("installer-preview-more-devices"),
                                ))),
                        )),
                ));
                if !self.device_label_locked() {
                    body = body.push(Box::new(self.field(
                        "device-name",
                        &j.device_name,
                        TextInputKind::Name,
                    )));
                }
            }
            Page::Belongings => {
                let mounted_step = self.live
                    && j.destination == Some(Destination::Another)
                    && j.categories[0]
                    && self.portable_source.mounted_permission_active();
                if mounted_step {
                    {
                        let choices = self.portable_source.mounted_choices();
                        if self.portable_source.mounted_needs_restart() {
                            body = body.push(Box::new(self.button(
                                "installer.mounted-locations.restart",
                                "installer-mounted-home-restart",
                                InstallerPreviewEffect::RestartMountedHomeReview,
                            )));
                        } else if let Some((id, name, _)) =
                            choices.iter().find(|(_, _, approved)| !approved)
                        {
                            body = body.push(Box::new(
                                ListRow::new("installer.mounted-location.current", name.clone())
                                    .secondary(t("installer-mounted-home-needs-grant"))
                                    .two_line_secondary()
                                    .icon(Icon::Folder),
                            ));
                            body = body.push(Box::new(self.button(
                                "installer.mounted-location.current.grant",
                                "installer-mounted-home-grant",
                                InstallerPreviewEffect::GrantMountedHomeLocation {
                                    location: id.clone(),
                                },
                            )));
                        } else if !choices.is_empty() {
                            body = body.push(Box::new(Text::new(&t(
                                "installer-mounted-home-all-granted",
                            ))));
                            body = body.push(Box::new(self.button(
                                "installer.mounted-locations.continue",
                                "installer-mounted-home-review",
                                InstallerPreviewEffect::ContinueMountedHomeReview,
                            )));
                        }
                        if self.portable_source.busy() {
                            body = body.push(Box::new(
                                self.button(
                                    "installer.mounted-locations.cancel",
                                    "installer-mounted-home-cancel",
                                    InstallerPreviewEffect::CancelMountedHomePermission,
                                )
                                .role(ButtonRole::Plain),
                            ));
                        }
                    }
                } else {
                    if self.migration_busy() {
                        body = body.push(Box::new(Text::new(&t("installer-migration-preparing"))));
                    }
                    if j.destination == Some(Destination::Desktop) {
                        for (id, title, description, icon) in [
                            (
                                "files",
                                "installer-preview-files-shared-title",
                                "installer-desktop-files-in-place",
                                Icon::Folder,
                            ),
                            (
                                "apps",
                                "installer-flatpak-apps-existing-heading",
                                "installer-desktop-apps-in-place",
                                Icon::Monitor,
                            ),
                        ] {
                            body = body.push(Box::new(
                                ListRow::new(format!("installer.desktop.{id}.in-place"), t(title))
                                    .secondary(t(description))
                                    .two_line_secondary()
                                    .icon(icon),
                            ));
                        }
                    }
                    for (index, key) in CATEGORIES.iter().enumerate() {
                        if !self.category_available(index) || (!j.details && !j.categories[index]) {
                            continue;
                        }
                        let title = if index == 0 && j.destination == Some(Destination::Computer) {
                            t("installer-preview-files-shared-title")
                        } else if index == 1
                            && j.categories[0]
                            && j.destination == Some(Destination::Another)
                        {
                            t("installer-preview-flatpak-system-title")
                        } else {
                            t(&format!("installer-preview-{key}-title"))
                        };
                        let subtitle = if self.live && self.belongings_worker.is_some() {
                            t("installer-preview-folders-checking")
                        } else if self.live && index == 1 {
                            if self.belongings.flatpak_unavailable {
                                t("installer-preview-flatpak-unavailable")
                            } else {
                                let count = self.flatpak_count();
                                t_args(
                                    if j.categories[0]
                                        && j.destination == Some(Destination::Another)
                                    {
                                        "installer-preview-flatpak-system-found"
                                    } else if j.destination == Some(Destination::Computer) {
                                        "installer-preview-flatpak-computer-found"
                                    } else {
                                        "installer-preview-flatpak-found"
                                    },
                                    &[("count", count.to_string().into())],
                                )
                            }
                        } else if self.live && index == 2 {
                            if j.details
                                || (self.belongings.accounts.candidates.is_empty()
                                    && self.belongings.accounts.tilo_accounts.is_empty())
                            {
                                t("installer-preview-accounts-body")
                            } else {
                                let labels = self
                                    .belongings
                                    .accounts
                                    .candidates
                                    .iter()
                                    .filter(|item| {
                                        item.can_import_credentials
                                            || accounts::can_connect_candidate(item)
                                    })
                                    .map(accounts::candidate_label)
                                    .chain(
                                        self.belongings
                                            .accounts
                                            .tilo_accounts
                                            .iter()
                                            .map(accounts::tilo_candidate_label),
                                    )
                                    .collect::<Vec<_>>();
                                if labels.is_empty() {
                                    t("installer-preview-accounts-body")
                                } else {
                                    labels.join(", ")
                                }
                            }
                        } else if self.live && self.belongings.home_unavailable {
                            t("installer-preview-home-unavailable")
                        } else if index == 0 && j.destination == Some(Destination::Computer) {
                            t("installer-preview-files-shared-body")
                        } else if index == 1 && j.destination == Some(Destination::Computer) {
                            t("installer-preview-flatpak-computer-body")
                        } else if index == 1
                            && j.categories[0]
                            && j.destination == Some(Destination::Another)
                        {
                            t("installer-preview-flatpak-system-body")
                        } else {
                            t(&format!("installer-preview-{key}-body"))
                        };
                        if j.details {
                            let checked = ui::LocalState::new(j.categories[index]);
                            let commands = self.commands.clone();
                            let category = key.to_string();
                            let mut category_group = page::related()
                                .push(Box::new(
                                    Checkbox::from_state(&checked)
                                        .with_id(&format!("installer.category.{key}"))
                                        .with_label(title)
                                        .on_change(move |included| {
                                            commands.borrow_mut().push(
                                                InstallerPreviewEffect::SetPreviewCategory {
                                                    category: category.clone(),
                                                    included,
                                                },
                                            )
                                        }),
                                ))
                                .push(Box::new(Text::new(&subtitle)));
                            if self.live
                                && index == 0
                                && j.categories[0]
                                && let Some(home) = &self.belongings.home
                            {
                                category_group = category_group
                                    .push(Box::new(
                                        Text::new(&t("installer-files-source-home"))
                                            .with_role(TextRole::Caption),
                                    ))
                                    .push(Box::new(
                                        self.button(
                                            "installer.files.source-location",
                                            if self.migration_source_location {
                                                "installer-files-hide-location"
                                            } else {
                                                "installer-files-show-location"
                                            },
                                            InstallerPreviewEffect::ToggleMigrationSourceLocation,
                                        )
                                        .role(ButtonRole::Plain),
                                    ));
                                if self.migration_source_location {
                                    category_group = category_group.push(Box::new(
                                        Text::new(&t_args(
                                            "installer-preview-files-location",
                                            &[("path", home.display().to_string().into())],
                                        ))
                                        .with_role(TextRole::Caption),
                                    ));
                                }
                            }
                            body = body.push(Box::new(category_group));
                        } else {
                            body = body.push(Box::new(
                                ListRow::new(format!("installer.category.{key}"), title)
                                    .secondary(subtitle)
                                    .two_line_secondary()
                                    .icon(match index {
                                        0 => Icon::Folder,
                                        1 => Icon::Monitor,
                                        _ => Icon::Users,
                                    }),
                            ));
                        }
                    }
                    if self.live && j.details {
                        let apps = self.flatpak_apps();
                        if !apps.is_empty() {
                            let mut app_group = page::related().push(Box::new(
                                Text::new(&t(if j.destination == Some(Destination::Desktop) {
                                    "installer-flatpak-apps-existing-heading"
                                } else {
                                    "installer-flatpak-apps-heading"
                                }))
                                .with_role(TextRole::Title),
                            ));
                            for (index, app) in apps.iter().enumerate() {
                                if app.selectable {
                                    let checked = ui::LocalState::new(app.included);
                                    let commands = self.commands.clone();
                                    let app_id = app.id.clone();
                                    app_group = app_group.push(Box::new(
                                        Checkbox::from_state(&checked)
                                            .with_id(&format!("installer.flatpak.app.{index}"))
                                            .with_label(&app.name)
                                            .with_enabled(j.categories[1])
                                            .on_change(move |included| {
                                                commands.borrow_mut().push(
                                                    InstallerPreviewEffect::SetPreviewFlatpakApp {
                                                        app: app_id.clone(),
                                                        included,
                                                    },
                                                );
                                            }),
                                    ));
                                } else {
                                    app_group = app_group.push(Box::new(
                                        ListRow::new(
                                            format!("installer.flatpak.app.{index}"),
                                            &app.name,
                                        )
                                        .secondary(t(if app.already_here {
                                            "installer-flatpak-app-existing"
                                        } else if app.with_home {
                                            "installer-flatpak-app-with-home"
                                        } else if j.destination == Some(Destination::Another) {
                                            "installer-flatpak-app-needs-home"
                                        } else {
                                            "installer-flatpak-app-transfer-unavailable"
                                        }))
                                        .two_line_secondary()
                                        .icon(Icon::Monitor),
                                    ));
                                }
                            }
                            body = body.push(Box::new(app_group));
                        }
                    }
                    if self.live
                        && j.details
                        && j.categories[1]
                        && self.belongings.flatpak.as_ref().is_some_and(|inventory| {
                            inventory.refs.iter().any(|item| {
                                item.app_id().is_some()
                                    && item.installation
                                        != super::flatpak_transfer::Installation::User
                            })
                        })
                    {
                        if let Some(notices) = &self.belongings.administrator_settings {
                            let affected: Vec<_> = notices
                                .iter()
                                .filter(|notice| !notice.domains().is_empty())
                                .collect();
                            if !affected.is_empty() {
                                let commands = self.commands.clone();
                                body = body.push(Box::new(
                                    ListRow::new(
                                        "installer.flatpak.access-details",
                                        t("installer-flatpak-settings-title"),
                                    )
                                    .secondary(t("installer-flatpak-settings-note"))
                                    .two_line_secondary()
                                    .disclosure()
                                    .on_activate(move || {
                                        commands.borrow_mut().push(
                                            InstallerPreviewEffect::ToggleFlatpakAccessDetails,
                                        )
                                    }),
                                ));
                            }
                            for (index, notice) in affected
                                .iter()
                                .enumerate()
                                .filter(|_| self.flatpak_access_details)
                            {
                                let settings = notice
                                    .domains()
                                    .into_iter()
                                    .map(|domain| t(domain.key()))
                                    .collect::<Vec<_>>()
                                    .join(", ");
                                body = body.push(Box::new(
                                    ListRow::new(
                                        format!("installer.flatpak.settings.{index}"),
                                        notice.installed.name.clone(),
                                    )
                                    .secondary(settings)
                                    .two_line_secondary(),
                                ));
                            }
                        } else {
                            body = body.push(Box::new(Text::new(&t(
                                "installer-flatpak-settings-unchecked",
                            ))));
                        }
                    }
                    if self.live
                        && self.belongings.browser.available()
                        && (j.details || j.browser_domains.iter().any(|included| *included))
                    {
                        if j.details {
                            let commands = self.commands.clone();
                            body =
                                body.push(self.belongings.browser.view(move |domain, included| {
                                    let category = match domain {
                                        browser::Domain::Bookmarks => "bookmarks",
                                        browser::Domain::History => "history",
                                    };
                                    commands.borrow_mut().push(
                                        InstallerPreviewEffect::SetPreviewCategory {
                                            category: category.into(),
                                            included,
                                        },
                                    );
                                }));
                        } else {
                            body = body.push(Box::new(
                                ListRow::new(
                                    "installer.browser.overview",
                                    t("installer-browser-title"),
                                )
                                .secondary(t(
                                    match (
                                        self.belongings.browser.chosen(browser::Domain::Bookmarks),
                                        self.belongings.browser.chosen(browser::Domain::History),
                                    ) {
                                        (true, false) => "installer-browser-bookmarks-body",
                                        (false, true) => "installer-browser-history-body",
                                        _ => "installer-browser-body",
                                    },
                                ))
                                .two_line_secondary()
                                .icon(Icon::Globe),
                            ));
                        }
                    }
                    if self.live && self.belongings.sessions.available() {
                        let commands = self.commands.clone();
                        if j.details {
                            body = body.push(self.belongings.sessions.view(
                                j.categories[2],
                                move |source_id, service, included| {
                                    commands.borrow_mut().push(
                                        InstallerPreviewEffect::SetPreviewServiceSession {
                                            source_id,
                                            service: service.as_str().into(),
                                            included,
                                        },
                                    );
                                },
                            ));
                        } else {
                            body = body.push(Box::new(
                                ListRow::new(
                                    "installer.sessions.overview",
                                    t("installer-service-sessions-title"),
                                )
                                .secondary(self.belongings.sessions.overview(j.categories[2]))
                                .two_line_secondary()
                                .icon(Icon::Globe)
                                .disclosure()
                                .on_activate(move || {
                                    commands
                                        .borrow_mut()
                                        .push(InstallerPreviewEffect::TogglePreviewDetails)
                                }),
                            ));
                        }
                    }
                    if self.live && j.details {
                        if j.categories[0] {
                            let profiles = self
                                .belongings
                                .accounts
                                .candidates
                                .iter()
                                .filter(|candidate| {
                                    !candidate.can_import_credentials
                                        && !accounts::can_connect_candidate(candidate)
                                })
                                .collect::<Vec<_>>();
                            if !profiles.is_empty() {
                                body = body
                                    .push(Box::new(
                                        Text::new(&t("installer-preview-profiles-title"))
                                            .with_role(TextRole::Title),
                                    ))
                                    .push(Box::new(Text::new(&t(
                                        "installer-preview-profiles-body",
                                    ))));
                                for candidate in profiles {
                                    body = body.push(Box::new(
                                        ListRow::new(
                                            format!("installer.profile.{}", candidate.id),
                                            &candidate.label,
                                        )
                                        .secondary(t("installer-preview-account-profile-only"))
                                        .two_line_secondary()
                                        .icon(Icon::Folder),
                                    ));
                                }
                            }
                        }
                        if j.categories[2] {
                            for account in &self.belongings.accounts.tilo_accounts {
                                body = body.push(Box::new(
                                    ListRow::new(
                                        format!("installer.account.{}", account.source_id),
                                        accounts::tilo_candidate_label(account),
                                    )
                                    .secondary(t("installer-preview-account-tilo"))
                                    .icon(Icon::Users),
                                ));
                            }
                            for candidate in
                                self.belongings
                                    .accounts
                                    .candidates
                                    .iter()
                                    .filter(|candidate| {
                                        candidate.can_import_credentials
                                            || accounts::can_connect_candidate(candidate)
                                    })
                            {
                                body = body.push(Box::new(
                                    ListRow::new(
                                        format!("installer.account.{}", candidate.id),
                                        accounts::candidate_label(candidate),
                                    )
                                    .secondary(t(if candidate.can_import_credentials {
                                        "installer-preview-account-importable"
                                    } else {
                                        "installer-preview-account-reconnect"
                                    }))
                                    .two_line_secondary()
                                    .icon(Icon::Users),
                                ));
                            }
                        }
                    }
                    body = body.push(Box::new(
                        self.button(
                            "installer.details",
                            if j.details {
                                "installer-preview-hide-details"
                            } else {
                                "installer-preview-choose-belongings"
                            },
                            InstallerPreviewEffect::TogglePreviewDetails,
                        )
                        .with_icon(if j.details {
                            Icon::ChevronUp
                        } else {
                            Icon::ChevronDown
                        })
                        .role(ButtonRole::Plain),
                    ));
                    if self.live {
                        let locations = self.portable_source.mounted_locations();
                        if !locations.is_empty() {
                            body = body
                                .push(Box::new(Text::new(&t("installer-mounted-home-locations"))));
                            for (index, location) in locations.into_iter().enumerate() {
                                body = body.push(Box::new(
                                    ListRow::new(
                                        format!("installer.mounted-location.{index}"),
                                        location,
                                    )
                                    .secondary(t("installer-mounted-home-location-unavailable"))
                                    .two_line_secondary()
                                    .icon(Icon::Folder),
                                ));
                            }
                        }
                    }
                    if self.live && self.belongings_worker.is_none() {
                        let warnings = self.belongings.warning_keys();
                        if !warnings.is_empty() {
                            body = body.push(Box::new(ui::widget::Banner::warning(
                                warnings.into_iter().map(t).collect::<Vec<_>>().join("\n"),
                            )));
                        }
                        body = body.push(Box::new(
                            self.button(
                                "installer.belongings.recheck",
                                "imager-setup-internal-recheck",
                                InstallerPreviewEffect::CheckBelongings,
                            )
                            .role(ButtonRole::Plain),
                        ));
                    }
                }
            }
            Page::Accounts => {
                let commands = self.commands.clone();
                let edits = self.commands.clone();
                body = body.push(Box::new(self.accounts.view(
                    Rc::new(move |request| {
                        let (step, selection) = request.parts();
                        let mut pending = commands.borrow_mut();
                        if !pending.iter().any(|effect| matches!(effect,
                            InstallerPreviewEffect::AccountStep { step: queued, selection: selected }
                                if queued == step && selected == &selection)) {
                            pending.push(InstallerPreviewEffect::AccountStep {
                                step: step.to_string(), selection,
                            });
                        }
                    }),
                    Rc::new(move |field, value| {
                        edits
                            .borrow_mut()
                            .push(InstallerPreviewEffect::SetAccountField {
                                field: field.to_string(),
                                value,
                            });
                    }),
                )));
            }
            Page::Storage => {
                let destination = j.destination.unwrap_or(Destination::Computer);
                if destination == Destination::Computer {
                    body = body.push(Box::new(self.storage_content()));
                } else if destination == Destination::Another {
                    body = body.push(Box::new(Text::new(&t("installer-preview-storage-another"))));
                }
            }
            Page::Review => {
                let computer_usb_fallback =
                    j.destination == Some(Destination::Computer) && !self.direct_route_enabled();
                if self.live
                    && (computer_usb_fallback
                        || (j.destination == Some(Destination::Another)
                            && !self.usb_backend_available()))
                {
                    body = body.push(Box::new(
                        Text::new(&t(if self.usb_backend_available() {
                            "installer-review-usb-fallback"
                        } else {
                            "installer-review-installation-unavailable"
                        }))
                        .with_id("installer.review.usb-fallback"),
                    ));
                }
                if self.live && self.media_preparation.busy() {
                    body = body.push(Box::new(Text::new(&t(self
                        .media_preparation
                        .phase_key()
                        .as_deref()
                        .unwrap_or("installer-preview-install-preparing")))));
                    if let Some(bytes) = self.media_preparation.progress_label() {
                        body = body.push(Box::new(Text::new(&bytes).with_role(TextRole::Caption)));
                    }
                }
                let destination = j.destination.unwrap_or(Destination::Computer);
                if destination != Destination::Desktop {
                    body = body.push(Box::new(self.review_selection_row(
                        "purpose",
                        "purpose",
                        t(&format!("installer-preview-purpose-{}-title", j.purpose)),
                        t(&format!("installer-preview-purpose-{}-body", j.purpose)),
                    )));
                }
                let setup = page::related()
                    .push(Box::new(
                        Text::new(&t("installer-review-setup"))
                            .with_role(TextRole::Subheading)
                            .with_heading_level(2),
                    ))
                    .push(Box::new(self.review_identity_row()))
                    .push(Box::new(self.review_device_row()))
                    .push(Box::new(
                        self.review_row(
                            "destination",
                            t(&format!("installer-preview-{}-title", destination.key())),
                            t(
                                if destination == Destination::Computer
                                    && !self.direct_route_enabled()
                                {
                                    "installer-preview-computer-usb-body"
                                } else {
                                    match destination {
                                        Destination::Computer => "installer-preview-computer-body",
                                        Destination::Another => "installer-preview-another-body",
                                        Destination::Desktop => "installer-preview-desktop-body",
                                    }
                                },
                            ),
                        )
                        .icon(match destination {
                            Destination::Computer => Icon::Monitor,
                            Destination::Another => Icon::Usb,
                            Destination::Desktop => Icon::MonitorSmartphone,
                        }),
                    ));
                let mut belongings = page::related()
                    .push(Box::new(
                        Text::new(&t("installer-review-belongings-heading"))
                            .with_role(TextRole::Subheading)
                            .with_heading_level(2),
                    ))
                    .push(Box::new(self.review_selection_row(
                        "migration",
                        "belongings",
                        t(if j.migrate == Some(true) {
                            "installer-preview-migrate-title"
                        } else {
                            "installer-preview-fresh-title"
                        }),
                        t(
                            if j.migrate == Some(true)
                                && j.categories[0]
                                && j.destination == Some(Destination::Computer)
                            {
                                "installer-preview-review-shared"
                            } else if j.migrate == Some(true)
                                && j.destination == Some(Destination::Desktop)
                            {
                                "installer-desktop-review-migrate"
                            } else if j.migrate == Some(true) {
                                "installer-preview-review-migrate"
                            } else {
                                "installer-preview-review-fresh"
                            },
                        ),
                    )));
                if j.migrate == Some(true) {
                    for (index, key) in CATEGORIES.iter().enumerate() {
                        // Accounts also controls website data. Category consent
                        // alone does not mean any cloud connection was prepared.
                        if j.categories[index]
                            && (index >= 2 || j.destination != Some(Destination::Desktop))
                            && (index != 2 || self.accounts.has_prepared_connections())
                        {
                            let title =
                                if index == 0 && j.destination == Some(Destination::Computer) {
                                    t("installer-preview-files-shared-title")
                                } else {
                                    t(&format!("installer-preview-{key}-title"))
                                };
                            belongings = belongings.push(Box::new(self.review_selection_row(
                                key,
                                "belongings-details",
                                title,
                                String::new(),
                            )));
                        }
                    }
                    if self.live {
                        let selected = self.belongings.browser.selected();
                        for (id, key, bookmarks) in [
                            ("bookmarks", "installer-browser-bookmarks", true),
                            ("history", "installer-browser-history", false),
                        ] {
                            let labels = selected
                                .profiles()
                                .iter()
                                .filter(|(_, profile)| {
                                    if bookmarks {
                                        profile.bookmarks
                                    } else {
                                        profile.history
                                    }
                                })
                                .map(|(source, _)| review_source_label(&source.label))
                                .collect::<Vec<_>>();
                            if !labels.is_empty() {
                                belongings = belongings.push(Box::new(self.review_selection_row(
                                    id,
                                    "belongings-details",
                                    t(key),
                                    labels.join(", "),
                                )));
                            }
                        }
                    }
                    #[cfg(target_os = "linux")]
                    if self.live {
                        for (index, label) in self
                            .belongings
                            .sessions
                            .review_labels(j.categories[2])
                            .into_iter()
                            .enumerate()
                        {
                            belongings = belongings.push(Box::new(self.review_selection_row(
                                &format!("session.{index}"),
                                "belongings-details",
                                label,
                                t("installer-review-service-session-body"),
                            )));
                        }
                    }
                }
                body = body
                    .with_rhythm(Rhythm::Group)
                    .push(Box::new(setup))
                    .push(Box::new(belongings));
            }
            Page::Ready => {
                body = body.push(Box::new(IconWidget::new(Icon::Check).accent()));
            }
        }
        body
    }

    fn restart_view(&self) -> Box<dyn Widget> {
        let mut body = page::stack()
            .push(Box::new(
                Text::new(&t(if self.restart.state() == "stale" {
                    "installer-restart-changed-heading"
                } else {
                    "installer-restart-heading"
                }))
                .with_role(TextRole::LargeTitle)
                .with_heading_level(1),
            ))
            .push(Box::new(Text::new(&t(match self.restart.state() {
                "checking" => "installer-restart-checking",
                "pending" => "installer-restart-incomplete",
                "unavailable" => "installer-restart-unavailable",
                "stale" => "installer-restart-changed",
                "resuming" => "installer-restart-resuming",
                "retiring" => "installer-restart-retiring",
                _ => "installer-restart-intro",
            }))));
        if !self.restart.busy() {
            if matches!(self.restart.state(), "unavailable" | "stale") {
                body = body.push(Box::new(
                    self.button(
                        "installer.restart.retry",
                        if self.restart.state() == "stale" {
                            "installer-restart-review-latest"
                        } else {
                            "installer-restart-retry"
                        },
                        InstallerPreviewEffect::RetryInstallerPreparation,
                    )
                    .role(ButtonRole::Primary),
                ));
            } else {
                body = body
                    .push(Box::new(self.button(
                        "installer.restart.resume",
                        "installer-restart-resume",
                        InstallerPreviewEffect::ResumeInstallerPreparation,
                    )))
                    .push(Box::new(
                        self.button(
                            "installer.restart.new",
                            "installer-restart-new",
                            InstallerPreviewEffect::StartNewInstallerPreparation,
                        )
                        .role(ButtonRole::Plain),
                    ));
            }
        }
        Box::new(
            Scrollable::new_vertical()
                .with_focus_reveal()
                .with_child(Box::new(
                    Container::new()
                        .with_inset(Rhythm::Group)
                        .readable()
                        .centered()
                        .with_child(Box::new(body)),
                )),
        )
    }

    fn composition(&self, theme: &Theme) -> Box<dyn Widget> {
        if self.windows.enabled && self.live {
            return self.windows_view();
        }
        if self.live && self.restart.blocks() {
            return self.restart_view();
        }
        let mut actions = Row::new();
        if (self.journey.page != Page::Destination || self.journey.cancels_edit())
            && self.journey.page != Page::Ready
        {
            actions = actions.push(Box::new(
                self.button(
                    "installer.back",
                    if self.journey.cancels_edit() {
                        "installer-preview-cancel"
                    } else {
                        "installer-preview-back"
                    },
                    InstallerPreviewEffect::BackInPreview,
                )
                .role(ButtonRole::Plain),
            ));
        }
        actions = actions.push(Box::new(Spacer::new()));
        if self.live
            && self.journey.page == Page::Accounts
            && let Some((id, label, request)) = self.accounts.navigation_action()
        {
            let (step, selection) = request.parts();
            actions = actions.push(Box::new(
                self.button(
                    id,
                    label,
                    InstallerPreviewEffect::AccountStep {
                        step: step.into(),
                        selection,
                    },
                )
                .role(ButtonRole::Primary),
            ));
        }
        if self.live && self.journey.page == Page::Review && self.media_preparation.busy() {
            actions = actions.push(Box::new(self.button(
                "installer.cancel-preparation",
                "installer-media-cancel",
                InstallerPreviewEffect::CancelMediaPreparation,
            )));
        }
        let changed_source = self.live
            && self.journey.page == Page::Review
            && !self.journey.editing_destination()
            && self.media_preparation.needs_source_review();
        if changed_source {
            actions = actions.push(Box::new(
                self.button(
                    "installer.review-changed-files",
                    self.media_preparation.review_reason().map_or(
                        "installer-source-review",
                        prepared_media::ReturnReason::button_key,
                    ),
                    InstallerPreviewEffect::ReviewChangedFiles,
                )
                .role(ButtonRole::Primary),
            ));
        }
        if !changed_source
            && self.live
            && self.journey.page == Page::Review
            && !self.journey.editing_destination()
            && self.usb_backend_available()
            && matches!(
                self.journey.destination,
                Some(Destination::Computer | Destination::Another)
            )
        {
            actions = actions.push(Box::new(
                self.button(
                    "installer.prepare-usb",
                    "installer-preview-prepare-usb",
                    InstallerPreviewEffect::PrepareUsb,
                )
                .role(
                    if self.journey.destination == Some(Destination::Computer)
                        && self.direct_route_enabled()
                    {
                        ButtonRole::Plain
                    } else {
                        ButtonRole::Primary
                    },
                ),
            ));
        }
        if self.live
            && self.journey.page == Page::Review
            && !self.journey.editing_destination()
            && self.journey.destination == Some(Destination::Computer)
            && self.direct_route_enabled()
        {
            actions = actions.push(Box::new(
                self.button(
                    "installer.prepare-direct",
                    "installer-preview-prepare-direct",
                    InstallerPreviewEffect::PrepareDirectInstallation,
                )
                .role(ButtonRole::Primary)
                .with_enabled(self.direct_available()),
            ));
        }
        if self.live
            && self.journey.page == Page::Review
            && !self.journey.editing_destination()
            && self.journey.destination == Some(Destination::Desktop)
        {
            actions = actions.push(Box::new(
                self.button(
                    "installer.prepare-desktop",
                    "installer-desktop-prepare",
                    InstallerPreviewEffect::PrepareDesktopInstallation,
                )
                .role(ButtonRole::Primary)
                .with_enabled(!self.media_preparation.busy() && !self.personal_busy()),
            ));
        }
        let (label, effect) = match self.journey.page {
            Page::Password if self.personal_busy() => (
                "installer-preview-identity-preparing",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Accounts => (
                self.accounts.continue_label(),
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Belongings if self.live => (
                "installer-preview-continue",
                InstallerPreviewEffect::ContinuePreview,
            ),
            _ if self.journey.saving_edit() => (
                "installer-preview-save",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Ready => (
                "installer-preview-restart",
                InstallerPreviewEffect::RestartPreview,
            ),
            Page::Review if self.live => (
                "installer-preview-continue",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Review => (
                "installer-preview-finish",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Belongings if self.journey.details => (
                "installer-preview-bring-selected",
                InstallerPreviewEffect::ContinuePreview,
            ),
            Page::Belongings => (
                "installer-preview-bring-things",
                InstallerPreviewEffect::ContinuePreview,
            ),
            _ => (
                "installer-preview-continue",
                InstallerPreviewEffect::ContinuePreview,
            ),
        };
        let chooses_path = matches!(
            self.journey.page,
            Page::Destination | Page::Migration | Page::Identity | Page::Connect
        ) && !(self.journey.page == Page::Destination
            && self.journey.editing_destination())
            && !(self.live
                && self.journey.page == Page::Connect
                && matches!(self.journey.transfer.as_deref(), Some("backup" | "current")));
        let chooses_path = chooses_path
            && !(self.live
                && self.journey.page == Page::Connect
                && self.journey.transfer.as_deref() == Some("device")
                && self.pairing.ready());
        let accounts_waiting = self.journey.page == Page::Accounts && !self.accounts.can_continue();
        let mounted_step = self.live
            && self.journey.page == Page::Belongings
            && self.journey.destination == Some(Destination::Another)
            && self.journey.categories[0]
            && self.portable_source.mounted_permission_active();
        if !chooses_path
            && !mounted_step
            && !accounts_waiting
            && !(self.live && self.journey.page == Page::Review && !self.journey.editing_choices())
        {
            actions = actions.push(Box::new(
                self.button("installer.next", label, effect)
                    .role(ButtonRole::Primary),
            ));
        }
        let mut content = page::stack()
            .with_rhythm(Rhythm::Group)
            .push(Box::new(self.body(theme)));
        if let Some(error) = self.error
            && self.password_validation().is_none()
        {
            content = content.push(Box::new(
                ui::widget::Banner::error(t(error)).with_id("installer.error"),
            ));
        }
        // Keep navigation at one position while a page's body, errors and
        // accessible text scroll independently. Personal forms retain their
        // bounded measure inside the same readable-width page frame.
        if matches!(self.journey.page, Page::Profile | Page::Password) {
            content = page::stack().push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_centered_max_width(theme.tokens.metrics.content_min_width)
                    .with_child(Box::new(content)),
            ));
        }
        let frame = page::Frame::new(content)
            .center_short_body()
            .notice(self.demo.then(|| {
                Box::new(
                    ui::widget::Banner::info(t("installer-demo-notice"))
                        .with_id("installer.demo-notice"),
                ) as Box<dyn Widget>
            }));
        if self.journey.page != Page::Destination || self.journey.cancels_edit() {
            frame.actions(actions).build()
        } else {
            frame.build()
        }
    }
}

impl InstallerPreview {
    fn reattach_after_media(&mut self, cx: SurfaceCx<Imager>) {
        {
            self.direct.context = Some(cx.clone());
        }
        self.dispatcher = Some(cx.dispatcher());
        self.dirty = true;
    }
    fn answer_preview(&mut self, effect: InstallerPreviewEffect) -> EffectAnswer {
        self.drain();
        if let InstallerPreviewEffect::ReadHomePromotionResults {
            category,
            offset,
            limit,
        } = &effect
        {
            return self
                .home_promotion
                .page(category, *offset, *limit)
                .map(Effected::value)
                .map_err(t);
        }
        let submitted = matches!(effect, InstallerPreviewEffect::ContinuePreview);
        if let Err(error) = self.apply(effect) {
            // The semantic door and the button queue share visible feedback
            // and focus the field whose correction permits progress.
            self.show_refusal(error, submitted);
            return Err(t(error));
        }
        let mut status = self.journey.status();
        status["guided_capabilities"] = serde_json::json!({
            "prepared_installation": self.guided_host_available(),
        });
        status["windows_preparation"] = self.windows.projection();
        status["windows_review"] = self.windows_review_projection();
        status["accounts"] = self.accounts.projection();
        status["account_jobs"] = serde_json::json!(self.account_actions.status());
        status["account_answer"] = self.account_answer.clone();
        status["user_transfer"] = self.user_transfer.status();
        status["home_promotion"] = self.home_promotion.status();
        {
            status["restart"] = self.restart.projection();
            status["prepared_identity"] = self.personal.projection();
            status["existing_identity"] = self.existing_identity.projection();
        }
        status["pairing"] = self.pairing.projection();
        {
            status["account_checkpoint"] = serde_json::json!({
                "saving": self.checkpoint_pending.is_some(),
                "restoring": self.checkpoint_restoring,
                "failed": self.checkpoint_failed,
                "accounts": self.resume_draft.as_ref().and_then(|ready| ready.account_checkpoint().map(|receipt| receipt.accounts())),
            });
            status["migration_preparation"] = self.migration_selection.projection();
            status["belongings_preparation"] = serde_json::json!({
                "pending": self.direct.belongings_pending,
                "prepared": self.direct.prepared_belongings.is_some()
                    && !self.journey.editing_choices(),
            });
            status["direct_install"] = serde_json::json!({
                "active": self.direct.active,
                "preparing": self.direct.pending.is_some(),
                "can_return": self.direct.active && self.direct.navigation.get(),
            });
        }
        let mut portable = self.portable_source.projection();
        if let Some(active) = self.media_preparation.active_home_projection() {
            for field in ["capture", "files", "bytes"] {
                portable[field] = active[field].clone();
            }
        }
        status["portable_preparation"] = portable;
        status["media_preparation"] = self.media_preparation.projection();
        status["recovery"] = self.recovery.report();
        status["belongings"] = self.belongings.report();
        status["belongings"]["apps"] = serde_json::json!(self.flatpak_apps());
        status["flatpak_access_details"] = serde_json::json!(
            self.journey.page == Page::Belongings
                && self.journey.details
                && self.journey.categories[1]
                && self.flatpak_access_details
        );
        if self.journey.migrate != Some(true) || !self.journey.categories[2] {
            status["belongings"]["website_sessions"]["selected"] = serde_json::json!([]);
        }
        status["belongings_checking"] = self.belongings_worker.is_some().into();
        let direct_available = self.direct_available();
        let plan_digest = self
            .storage
            .plan
            .as_ref()
            .and_then(|plan| plan.as_ref().ok())
            .map(|plan| plan.digest())
            .transpose()
            .map_err(|error| error.to_string())?;
        status["storage"] = serde_json::json!({
            "checking": self.storage.checking(),
            "size_gib": self.storage.selected(),
            "bounds_gib": self.storage.bounds,
            "plan_digest": plan_digest,
            "direct_install_available": direct_available,
        });
        Ok(Effected::value(status))
    }
}

impl ResourceSurface<Imager, InstallerPreviewEffect> for InstallerPreview {
    fn present(&mut self, state: &Rc<Imager>) {
        self.install_preferences = state.profile.clone();
        self.install_preferences.user.password.clear();
        self.install_preferences.encryption_passphrase.clear();
        if self.install_preferences.user.username
            == imager_backend::imager_model::UserAccount::default().username
        {
            if let Ok(login) = std::env::var("USER") {
                let mut candidate = self.install_preferences.clone();
                candidate.user.username = login;
                if candidate.validate_media_profile().is_ok() {
                    self.install_preferences.user.username = candidate.user.username;
                }
            }
        }
    }
    fn attach(&mut self, cx: SurfaceCx<Imager>) {
        if cx.launch_flag("--demo") {
            // Reuse the capture fixture and production renderer without attaching
            // any native controllers. The effect door also enforces demo admission.
            self.stage(Some("installer-destination"));
            self.demo = true;
            return;
        }
        if !cx.mode().is_live() {
            return;
        }
        self.live = true;
        if imager_backend::prepared_identity::require_supported().is_ok() {
            let _ = self.restart.inspect();
        }
        {
            self.direct.context = Some(cx.clone());
        }
        self.dispatcher = Some(cx.dispatcher());
        self.journey.categories = [true; 3];
        // The OS profile is entered explicitly; Desktop never reads a host profile.
        self.journey.name.clear();
        self.dirty = true;
    }
    fn stage(&mut self, scenario: Option<&str>) {
        let Some(scenario) = scenario else {
            return;
        };
        if !SCENARIOS.contains(&scenario) {
            return;
        }
        self.linux = true;
        self.capture = true;
        self.live = false;
        self.storage.stage();
        self.journey = Journey::new("Ada Lovelace".into());
        self.journey.device_name = t("installer-preview-device-default");
        self.journey.destination = Some(Destination::Computer);
        self.journey.migrate = Some(true);
        self.journey.identity = Some(Identity::New);
        self.journey.page = match scenario {
            "installer-migration" => Page::Migration,
            "installer-identity" => Page::Identity,
            "installer-profile" => Page::Profile,
            "installer-password" => Page::Password,
            "installer-connect" => {
                self.journey.identity = Some(Identity::Existing);
                Page::Connect
            }
            "installer-desktop-connect" => {
                self.journey.destination = Some(Destination::Desktop);
                self.journey.migrate = Some(false);
                self.journey.identity = Some(Identity::Existing);
                Page::Connect
            }
            "installer-devices" => Page::Devices,
            "installer-purpose" => Page::Purpose,
            "installer-belongings" => Page::Belongings,
            "installer-belongings-details" => {
                self.journey.details = true;
                Page::Belongings
            }
            "installer-accounts" => Page::Accounts,
            "installer-storage" => Page::Storage,
            "installer-usb" => {
                self.journey.destination = Some(Destination::Another);
                Page::Storage
            }
            "installer-desktop" => {
                self.journey.destination = Some(Destination::Desktop);
                Page::Storage
            }
            "installer-review" => Page::Review,
            "installer-ready" => Page::Ready,
            _ => {
                self.journey.destination = None;
                Page::Destination
            }
        };
        if self.journey.destination == Some(Destination::Desktop) {
            self.journey.categories[0] = false;
            self.journey.categories[1] = false;
        }
        self.dirty = true;
    }
    fn effect(
        &mut self,
        effect: InstallerPreviewEffect,
        _cx: &mut EffectCx<Imager, Self>,
    ) -> EffectAnswer {
        self.answer_preview(effect)
    }
    fn effects(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        for declaration in InstallerPreviewEffect::declarations() {
            if self.demo && !demo::admits_id(declaration.id) {
                statuses.push(EffectStatus::unavailable(
                    ActionRef::<InstallerPreviewEffect, ()>::new(declaration.id, &[]),
                    "demo",
                    t("installer-demo-unavailable"),
                ));
                continue;
            }
            if !self.effect_page_admitted(declaration.id) {
                statuses.push(EffectStatus::unavailable(
                    ActionRef::<InstallerPreviewEffect, ()>::new(declaration.id, &[]),
                    "wrong_page",
                    t("installer-preview-wrong-page"),
                ));
            }
        }
        if self.windows.enabled && self.live {
            if self.windows.busy() {
                for declaration in InstallerPreviewEffect::declarations() {
                    if declaration.id != "read-preview" && self.effect_page_admitted(declaration.id)
                    {
                        statuses.push(EffectStatus::unavailable(
                            ActionRef::<InstallerPreviewEffect, ()>::new(declaration.id, &[]),
                            "exporting",
                            t(self.windows.busy_label()),
                        ));
                    }
                }
            }
            return statuses;
        }
        if self.journey.page == Page::Migration && !self.guided_host_available() {
            for action in ["choose-preview", "continue-preview"] {
                statuses.push(EffectStatus::unavailable(
                    ActionRef::<InstallerPreviewEffect, ()>::new(action, &[]),
                    "guided_host_unavailable",
                    t("installer-guided-host-unavailable"),
                ));
            }
        }
        if self.live && self.restart.blocks() {
            statuses.push(EffectStatus::unavailable(
                InstallerPreviewEffect::CONTINUE_PREVIEW,
                "restart-review",
                t("installer-restart-intro"),
            ));
        }
        {
            if !self.direct_available() {
                statuses.push(EffectStatus::unavailable(
                    InstallerPreviewEffect::PREPARE_DIRECT_INSTALLATION,
                    "installation_unavailable",
                    t("installer-preview-storage-unavailable"),
                ));
            }
            if self.direct.active && !self.direct.navigation.get() {
                statuses.push(EffectStatus::unavailable(
                    InstallerPreviewEffect::BACK_IN_PREVIEW,
                    "installation_busy",
                    t("installer-preview-identity-busy"),
                ));
            }
        }
        if self.journey.history.is_empty()
            && !self.journey.cancels_edit()
            && !(self.journey.page == Page::Accounts && !self.accounts.at_overview())
        {
            statuses.push(EffectStatus::unavailable(
                InstallerPreviewEffect::BACK_IN_PREVIEW,
                "at_start",
                t("installer-preview-at-start"),
            ));
        }
        if self.journey.page == Page::Ready {
            statuses.push(EffectStatus::unavailable(
                InstallerPreviewEffect::CONTINUE_PREVIEW,
                "finished",
                t("installer-preview-finished"),
            ));
        }
        statuses
    }
}

impl Widget for InstallerPreview {
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.drain();
        if let Some(child) = &mut self.composed {
            child.sync(state, path);
        }
    }
    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let changed = self.drain();
        self.composed
            .as_mut()
            .is_some_and(|child| child.update(state, path))
            || changed
    }
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The host has already applied accessibility scaling to the theme.
        // Window growth changes whitespace, never type or control dimensions.
        let metrics = (
            theme.tokens.metrics.readable_width,
            theme.tokens.metrics.content_min_width,
            theme.text(TextRole::LargeTitle).size,
        );
        if self.layout_metrics != Some(metrics) {
            self.dirty = true;
            self.last_frame = None;
            self.pending_slide = None;
            self.slide = None;
            self.outgoing = None;
        }
        self.layout_metrics = Some(metrics);
        self.profile_breakpoint = f64::from(theme.tokens.metrics.content_min_width);
        let showing_direct = self.direct.active;
        if !showing_direct && (self.dirty || self.composed.is_none()) {
            let focus = self.validation_focus.take().or_else(|| {
                self.composed
                    .as_ref()
                    .and_then(|root| page::focused_id(root.as_ref()))
            });
            self.composed = Some(self.composition(theme));
            if let (Some(focus), Some(root)) = (focus, &mut self.composed) {
                page::refocus(root.as_mut(), &focus);
            }
            self.dirty = false;
        }
        let child = if let Some(composed) = &mut self.composed {
            composed.build_layout(taffy, theme, nodes)
        } else {
            use ui::widget::TaffyTreeExt as _;
            taffy.leaf_node(Style::default())
        };
        if let Ok(style) = taffy.style(child) {
            let mut style = style.clone();
            style.size.height = Dimension::percent(1.0);
            style.max_size.height = Dimension::percent(1.0);
            style.min_size.height = Dimension::length(0.0);
            style.flex_shrink = 1.0;
            let _ = taffy.set_style(child, style);
        }
        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    justify_content: Some(taffy::JustifyContent::Center),
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        nodes.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let previous_rect = self.rect;
        let profile_compact = self.rect.width() < self.profile_breakpoint;
        if let Ok(layout) = taffy.layout(node) {
            self.rect = Rect::from_origin_size(
                (
                    origin.x + f64::from(layout.location.x),
                    origin.y + f64::from(layout.location.y),
                ),
                (f64::from(layout.size.width), f64::from(layout.size.height)),
            );
        }
        if profile_compact != (self.rect.width() < self.profile_breakpoint) {
            self.dirty = true;
        }
        if self.rect != previous_rect {
            // A cached departing scene belongs to its old layout. Ordinary
            // page navigation retains snapshots when the viewport is stable.
            self.last_frame = None;
            self.pending_slide = None;
            self.slide = None;
            self.outgoing = None;
        }
        if let Some(child) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&node) = children.first()
        {
            child.apply_layout(taffy, node, self.rect.origin());
        }
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let now = ui::time::now();
        if theme.reduce_motion || self.capture {
            self.pending_slide = None;
            self.slide = None;
            self.outgoing = None;
        } else if let Some(direction) = self.pending_slide.take() {
            // Only the new page is live; the departing page is a paint snapshot.
            self.outgoing = self.last_frame.take();
            self.slide = self.outgoing.as_ref().map(|_| {
                Animated::new(
                    direction,
                    0.0,
                    theme.tokens.motion.durations.base(),
                    Curve::EaseInOut,
                )
            });
        }
        if self.slide.as_ref().is_some_and(|slide| slide.done(now)) {
            self.slide = None;
            self.outgoing = None;
        }
        let mut current = Scene::new();
        if let Some(child) = &mut self.composed {
            child.draw(&mut current, Affine::IDENTITY, text, theme, presence);
        }
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        if let Some(slide) = &self.slide {
            let position = slide.current(now);
            let incoming_x = position * self.rect.width();
            let outgoing_x = (position - slide.from()) * self.rect.width();
            if let Some(outgoing) = &self.outgoing {
                scene.append(
                    outgoing,
                    Some(transform.pre_translate(Vec2::new(outgoing_x, 0.0))),
                );
            }
            scene.append(
                &current,
                Some(transform.pre_translate(Vec2::new(incoming_x, 0.0))),
            );
        } else {
            scene.append(&current, Some(transform));
        }
        scene.pop_layer();
        self.last_frame = Some(current);
    }
    fn handle_event(&mut self, event: &WindowEvent, cx: &mut ui::widget::EventCtx<'_>) {
        // Pointer geometry becomes actionable when the new page reaches its position.
        if (self.pending_slide.is_some() || self.slide.is_some())
            && matches!(
                event,
                WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::Touch(_)
            )
        {
            return;
        }
        if let Some(child) = &mut self.composed
            && let Ok(children) = cx.taffy.children(cx.node_id)
            && let Some(&node) = children.first()
        {
            child.handle_event(event, &mut cx.reborrow(cx.path, node));
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .iter()
            .map(|child| child.as_ref() as &dyn Widget)
            .collect()
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .iter_mut()
            .map(|child| child.as_mut() as &mut dyn Widget)
            .collect()
    }
    fn name(&self) -> &'static str {
        "InstallerPreview"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_animation_frame(&self) -> bool {
        self.dirty
            || self.windows.busy()
            || self.pending_slide.is_some()
            || self.slide.is_some()
            || self.storage.checking()
            || self.belongings_worker.is_some()
            || self.user_transfer.busy()
            || self.accounts.active()
            || self.existing_identity_active()
            || self.personal_busy()
            || self.migration_busy()
            || self.restart.busy()
    }
    fn needs_idle_sync(&self) -> bool {
        self.windows.choosing() || self.avatar_picker.is_some() || self.migration_choosing()
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.dirty
    }
}

/// Discovery labels are public product/profile metadata, never source paths or credentials.
/// Keep review copy bounded even when upstream profile metadata is unusual.
fn review_source_label(label: &str) -> String {
    label
        .chars()
        .filter(|character| !character.is_control())
        .take(128)
        .collect()
}

#[cfg(all(test, unix))]
mod desktop_model_tests;
#[cfg(all(test, unix))]
mod purpose_tests;

// This fixture exercises the Unix identity/restart controller. Windows has
// its own inventory and recovery journey tests in windows.rs.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::ImagerShell;
    use ui::framework::testing::TestApp;

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_choice_uses_native_review_without_account_or_migration() {
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.capabilities.prepared_installation = false;
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "desktop".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Review);
            assert_eq!(preview.journey.history, vec![Page::Destination]);
            assert!(preview.direct.active);
            assert!(preview.personal.prepared_handle().is_none());
            assert!(preview.journey.password.is_empty());
            assert!(preview.journey.identity.is_none());
            assert_eq!(preview.journey.categories, [false; 3]);
            assert!(!preview.media_preparation.busy());
            assert!(preview.belongings_worker.is_none());
            assert!(!preview.storage.checking());
        })
        .unwrap();
        for effect in [
            InstallerPreviewEffect::SetPreviewName {
                value: "Not a profile".into(),
            },
            InstallerPreviewEffect::SetPreviewPassword {
                value: "Not a password".into(),
            },
            InstallerPreviewEffect::SetPreviewCategory {
                category: "accounts".into(),
                included: true,
            },
            InstallerPreviewEffect::ChoosePreviewAvatar,
            InstallerPreviewEffect::CheckBelongings,
        ] {
            assert!(
                app.effect(&effect).is_err(),
                "Desktop must reject OS-only effects"
            );
        }
        let inspection = app.harness().inspection_json().unwrap();
        for id in [
            "installer.name",
            "installer.password",
            "installer.category.accounts",
        ] {
            assert!(!inspection.contains(id), "Desktop must not expose {id}");
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !app
            .with_widget::<InstallerPreview, _>(|preview| preview.direct.navigation.get())
            .unwrap()
        {
            assert!(
                std::time::Instant::now() < deadline,
                "native host check must complete"
            );
            app.settle();
        }
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Destination);
            assert!(preview.journey.history.is_empty());
        })
        .unwrap();
    }

    #[cfg(all(target_os = "linux", feature = "native-current-device-qualification"))]
    mod current_identity {
        include!("current_identity_tests.rs");
    }

    fn fixture() -> TestApp<ImagerShell> {
        let mut app = TestApp::<ImagerShell>::launched(&["--installer-preview"]).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = false;
            preview.restart = restart_controller::Restart::default();
            preview.storage.stage();
        })
        .unwrap();
        app
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn belongings_source_location_is_explicit_and_retires_with_details() {
        let source = tempfile::tempdir().unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("XDG_STATE_HOME", source.path().join("state"));
        environment.set("XDG_CONFIG_HOME", source.path().join("config"));
        let home = source.path().join("private-source-folder");
        std::fs::create_dir(&home).unwrap();
        let path = home.display().to_string();
        let mut app = TestApp::<ImagerShell>::launched(&["--installer-preview"]).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let completed = app
                .with_widget::<InstallerPreview, _>(|preview| {
                    let _ = preview.restart.poll();
                    !preview.restart.busy()
                })
                .unwrap();
            if completed {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "startup restart inspection must complete under owned state custody"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.restart.state(), "missing");
            preview.live = false;
            preview.restart = restart_controller::Restart::default();
            preview.storage.stage();
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.migrate = Some(true);
            preview.journey.details = true;
            preview.journey.categories = [true, false, false];
            preview.belongings.home = Some(home);
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        let hidden = app.harness().inspection_json().unwrap();
        assert!(hidden.contains(&t("installer-files-source-home")));
        assert!(hidden.contains("installer.files.source-location"));
        assert!(!hidden.contains(&path));
        let original = status(&mut app);
        app.effect(&InstallerPreviewEffect::ToggleMigrationSourceLocation)
            .unwrap();
        app.settle();
        assert!(app.harness().inspection_json().unwrap().contains(&path));
        assert_eq!(
            status(&mut app),
            original,
            "location disclosure cannot change consent"
        );
        app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
            .unwrap();
        app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
            .unwrap();
        app.settle();
        assert!(!app.harness().inspection_json().unwrap().contains(&path));
        app.effect(&InstallerPreviewEffect::ToggleMigrationSourceLocation)
            .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "files".into(),
            included: false,
        })
        .unwrap();
        app.settle();
        assert!(!app.harness().inspection_json().unwrap().contains(&path));
        assert!(
            app.effect(&InstallerPreviewEffect::ToggleMigrationSourceLocation)
                .is_err()
        );
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.page = Page::Devices;
            preview.dirty = true;
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::ToggleMigrationSourceLocation)
                .is_err()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn live_profile_starts_empty_and_remains_explicitly_editable() {
        let root = tempfile::tempdir().unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("XDG_STATE_HOME", root.path().join("state"));
        let mut original = fixture();
        let suggested = original
            .with_widget::<InstallerPreview, _>(|preview| preview.journey.name.clone())
            .unwrap();
        assert!(suggested.is_empty());
        environment.set("USER", "another-person-not-the-current-os-account");
        let mut changed = fixture();
        changed
            .with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(preview.journey.name, suggested);
                assert_ne!(
                    preview.journey.name,
                    "another-person-not-the-current-os-account"
                );
            })
            .unwrap();
        // The OS profile remains editable without reading the host account.
        // Entering a name does not prepare identity custody.
        choose(&mut changed, "computer");
        choose(&mut changed, "fresh");
        choose(&mut changed, "new");
        changed
            .effect(&InstallerPreviewEffect::SetPreviewName {
                value: "My chosen name".into(),
            })
            .unwrap();
        changed
            .with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(preview.journey.name, "My chosen name");
                assert!(preview.personal.prepared_handle().is_none());
            })
            .unwrap();
    }

    #[cfg(unix)]
    fn preparation_operations(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut operations = Vec::new();
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if name == "draft.json" || name == "draft.lock" {
                assert!(entry.file_type().unwrap().is_file());
            } else {
                assert!(entry.file_type().unwrap().is_dir());
                assert!(uuid::Uuid::parse_str(name.to_str().unwrap()).is_ok());
                operations.push(entry.path());
            }
        }
        operations
    }

    fn choose(app: &mut TestApp<ImagerShell>, choice: &str) {
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: choice.into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
    }
    fn password(app: &mut TestApp<ImagerShell>) {
        app.effect(&InstallerPreviewEffect::SetPreviewPassword {
            value: "preview-secret-only".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewConfirmation {
            value: "preview-secret-only".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
    }
    fn status(app: &mut TestApp<ImagerShell>) -> serde_json::Value {
        app.effect(&InstallerPreviewEffect::ReadPreview)
            .unwrap()
            .value
            .unwrap()
    }

    #[cfg(target_os = "linux")]
    fn wait_for_belongings(app: &mut TestApp<ImagerShell>) -> serde_json::Value {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let snapshot = status(app);
            if snapshot["belongings_preparation"]["prepared"] == true {
                assert_eq!(snapshot["belongings_preparation"]["pending"], false);
                assert_ne!(snapshot["media_preparation"]["state"], "failed");
                return snapshot;
            }
            assert_ne!(
                snapshot["media_preparation"]["state"], "failed",
                "{snapshot}"
            );
            assert!(std::time::Instant::now() < deadline, "{snapshot}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    fn wait_for_personal(app: &mut TestApp<ImagerShell>) -> serde_json::Value {
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            let snapshot = status(app);
            if !matches!(
                snapshot["prepared_identity"]["state"].as_str(),
                Some("preparing" | "updating" | "reading-avatar" | "saving")
            ) {
                return snapshot;
            }
            assert!(std::time::Instant::now() < deadline, "{snapshot}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(unix)]
    #[cfg(target_os = "linux")]
    #[test]
    fn approved_pairing_then_new_identity_releases_only_branch_guards_and_preserves_custody() {
        let (custody, pairing) = pairing::certified_fixture();
        let state_root = custody.path().canonicalize().unwrap();
        let root = pairing.fixture_operation_directory();
        let original_certificate =
            std::fs::read(root.join("future-device/device/cert.bin")).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        env.set("XDG_STATE_HOME", &state_root);
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "fresh");
        choose(&mut app, "existing");
        app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
            .unwrap();
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "device".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
            value: "Approved future computer".into(),
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| preview.pairing = pairing)
            .unwrap();
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            app.with_widget::<InstallerPreview, _>(|preview| preview.drain())
                .unwrap();
            if status(&mut app)["pairing"]["state"] == "ready" {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        wait_for_personal(&mut app);
        let approved = status(&mut app)["prepared_identity"]["public_key"].clone();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        password(&mut app);
        assert_eq!(status(&mut app)["page"], "devices");
        assert!(
            app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
                value: "Unapproved rename".into()
            })
            .is_err()
        );
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.device_label_locked());
            let mut enrolled_review = preview.journey.clone();
            enrolled_review.page = Page::Review;
            // Keep the actual signed retained identity while testing admission.
            preview.journey = enrolled_review;
            preview.dirty = true;
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::EditPreview {
                section: "devices".into()
            })
            .is_err()
        );
        assert_eq!(status(&mut app)["page"], "review");
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            let row = visual_tests::control(preview, "installer.review.devices").unwrap();
            assert!(row.accessibility().unwrap().state.disabled);
            preview.journey.page = Page::Devices;
            preview.dirty = true;
        })
        .unwrap();
        for _ in 0..3 {
            app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        }
        assert_eq!(status(&mut app)["page"], "identity");
        choose(&mut app, "new");
        assert_eq!(status(&mut app)["pairing"]["state"], "unconnected");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "New identity".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        password(&mut app);
        let state = wait_for_personal(&mut app);
        assert_eq!(state["page"], "devices");
        assert_ne!(state["prepared_identity"]["public_key"], approved);
        app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
            value: "My new computer".into(),
        })
        .unwrap();
        assert_eq!(status(&mut app)["device_name"], "My new computer");
        assert_eq!(
            std::fs::read(root.join("future-device/device/cert.bin")).unwrap(),
            original_certificate
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn existing_device_pairing_name_step_uses_public_actions_and_can_return_to_methods() {
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "fresh");
        choose(&mut app, "existing");
        app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
            .unwrap();
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "device".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
            value: String::new(),
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::StartExistingDevicePairing)
                .is_err()
        );
        assert_eq!(status(&mut app)["pairing"]["state"], "unconnected");
        assert_eq!(status(&mut app)["page"], "connect");
        assert!(
            app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
                value: "é".repeat(33)
            })
            .is_err()
        );
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(status(&mut app)["page"], "connect");
        assert!(status(&mut app)["transfer"].is_null());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn belongings_edit_cancel_retains_source_custody_and_persisted_consent() {
        let (root, intent) = super::super::migration_intent::reviewed_app_routing_fixture();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
        environment.set("XDG_STATE_HOME", root.path().join("state"));
        environment.set("XDG_CONFIG_HOME", root.path().join("config"));
        let minimum = intent.application_minimum();
        assert!(minimum > super::super::staging::MIN_TARGET_BYTES);
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Password;
            preview.journey.identity = Some(Identity::New);
            preview.journey.name = "Retained identity".into();
            preview.journey.password = "review route fixture password".into();
            preview.journey.confirmation = preview.journey.password.clone();
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        wait_for_personal(&mut app);
        let identity = app
            .with_widget::<InstallerPreview, _>(|preview| {
                preview.personal.prepared_handle().unwrap()
            })
            .unwrap();
        let accounts = accounts::checkpoint_fixture_for_journey(&root.path().join("accounts"));
        let account_ids = accounts.prepared_handle().unwrap().selected_ids();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.page = Page::Review;
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, true, true];
            preview.accounts = accounts;
            preview.belongings.home = Some(root.path().join("source"));
            // The source fixture contains user app data, with no system refs.
            preview.belongings.flatpak = Some(super::super::flatpak_transfer::Inventory::default());
            preview.storage.set_application_minimum(minimum);
            preview.storage.stage();
            preview
                .migration_selection
                .prepare_routing_fixture(intent.clone());
        })
        .unwrap();
        let original = app
            .with_widget::<InstallerPreview, _>(|preview| {
                preview.journey.history = vec![Page::Storage];
                let choices = preview.journey.draft_choices();
                preview
                    .restart
                    .choices_changed(identity.clone(), choices.clone());
                choices
            })
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            app.settle();
            if let draft::DraftStatus::Ready(ready) =
                draft::DraftStore::for_session().unwrap().review().unwrap()
                && ready.choices() == &original
                && status(&mut app)["restart"]["checking"] == false
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "baseline consent was not persisted"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(visual_tests::control(preview, "installer.review.migration").is_some());
        })
        .unwrap();
        for choice in ["fresh", "migrate"] {
            app.effect(&InstallerPreviewEffect::EditPreview {
                section: "belongings".into(),
            })
            .unwrap();
            app.effect(&InstallerPreviewEffect::ChoosePreview {
                choice: choice.into(),
            })
            .unwrap();
            assert!(
                app.effect(&InstallerPreviewEffect::ExportPreparedSetup {
                    destination: "tentative-belongings-must-not-export".into(),
                    setup_binding: "tentative-consent".into(),
                })
                .is_err()
            );
            if choice == "migrate" {
                app.effect(&InstallerPreviewEffect::ContinuePreview)
                    .unwrap();
                app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
                    .unwrap();
                app.effect(&InstallerPreviewEffect::SetPreviewCategory {
                    category: "files".into(),
                    included: false,
                })
                .unwrap();
                app.effect(&InstallerPreviewEffect::SetPreviewCategory {
                    category: "accounts".into(),
                    included: false,
                })
                .unwrap();
                app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
            }
            app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
            assert_eq!(status(&mut app)["page"], "review");
            app.settle();
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(preview.journey.draft_choices(), original);
                assert_eq!(preview.journey.password, "review route fixture password");
                assert_eq!(
                    preview.personal.prepared_handle().unwrap().public_key(),
                    identity.public_key()
                );
                assert_eq!(
                    preview.accounts.prepared_handle().unwrap().selected_ids(),
                    account_ids
                );
                assert_eq!(
                    preview.migration_selection.reviewed_intent(),
                    Some(intent.clone())
                );
                assert_eq!(preview.migration_selection.application_minimum(), minimum);
                assert_eq!(preview.storage.bounds.unwrap().0, minimum.div_ceil(1 << 30));
            })
            .unwrap();
            let draft::DraftStatus::Ready(ready) =
                draft::DraftStore::for_session().unwrap().review().unwrap()
            else {
                panic!("prepared consent was lost");
            };
            assert_eq!(ready.choices(), &original);
        }
        // Saving an unchanged review is also non-destructive.
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "belongings".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "review");
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(
                preview.migration_selection.reviewed_intent(),
                Some(intent.clone())
            );
            assert_eq!(
                preview.accounts.prepared_handle().unwrap().selected_ids(),
                account_ids
            );
        })
        .unwrap();
        assert_eq!(status(&mut app)["page"], "review");
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.migration_selection.reviewed_intent(), Some(intent));
            assert!(visual_tests::control(preview, "installer.prepare-usb").is_some());
            assert!(!preview.media_preparation.busy());
        })
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn enabling_accounts_from_review_requires_fresh_public_saved_connection_consent() {
        let (root, intent) = super::super::migration_intent::reviewed_app_routing_fixture();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
        environment.set("XDG_STATE_HOME", root.path().join("state"));
        environment.set("XDG_CONFIG_HOME", root.path().join("config"));
        let minimum = intent.application_minimum();
        assert!(minimum > super::super::staging::MIN_TARGET_BYTES);
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Password;
            preview.journey.identity = Some(Identity::New);
            preview.journey.name = "Retained identity".into();
            preview.journey.password = "review route fixture password".into();
            preview.journey.confirmation = preview.journey.password.clone();
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        wait_for_personal(&mut app);
        let identity = app
            .with_widget::<InstallerPreview, _>(|preview| {
                preview.personal.prepared_handle().unwrap()
            })
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.page = Page::Review;
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, true, true];
            preview.belongings.home = Some(root.path().join("source"));
            // The source fixture contains user app data, with no system refs.
            preview.belongings.flatpak = Some(super::super::flatpak_transfer::Inventory::default());
            preview.storage.set_application_minimum(minimum);
            preview.storage.stage();
            preview
                .migration_selection
                .prepare_routing_fixture(intent.clone());
        })
        .unwrap();
        use super::super::account_transfer::{SourceRoots, discover};
        use std::os::unix::fs::PermissionsExt;
        let home = root.path().join("source");
        let config = home.join(".config");
        std::fs::create_dir_all(config.join("rclone")).unwrap();
        for path in [&home, &config] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let source = config.join("rclone/rclone.conf");
        let original = b"[Files]\ntype=webdav\nurl=https://files.example.test/dav\nuser=ada\npass=AAECAwQFBgcICQoLDA0OD2f2H1FyoCWnrFF4SHSBvFFHjlw9\n";
        std::fs::write(&source, original).unwrap();
        let roots = SourceRoots { home, config };
        let inventory = discover(&roots);
        assert!(
            inventory
                .candidates
                .iter()
                .any(|candidate| candidate.can_import_credentials)
        );
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.accounts = accounts::Accounts::default();
            preview
                .accounts
                .update_sources(roots.clone(), inventory.clone())
                .unwrap();
            preview.belongings.roots = Some(roots);
            preview.belongings.accounts = inventory;
            preview.journey.categories = [true, false, false];
            assert!(preview.accounts.prepared_handle().is_none());
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "belongings".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
            .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "accounts".into(),
            included: true,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "accounts");
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.accounts.prepared_handle().is_none());
            assert!(!preview.accounts.can_continue());
            assert!(visual_tests::control(preview, "installer.account.review-saved").is_some());
            assert_eq!(preview.migration_selection.reviewed_intent(), Some(intent));
            assert_eq!(
                preview.personal.prepared_handle().unwrap().public_key(),
                identity.public_key()
            );
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::AccountStep {
                step: "import-saved".into(),
                selection: None
            })
            .is_err()
        );
        app.effect(&InstallerPreviewEffect::AccountStep {
            step: "review-saved".into(),
            selection: None,
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.accounts.navigation_step(), "review-saved");
            assert!(preview.accounts.prepared_handle().is_none());
            assert!(!preview.accounts.has_prepared_connections());
        })
        .unwrap();
        assert_eq!(std::fs::read(source).unwrap(), original);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disabling_accounts_in_review_releases_prepared_selection_only_on_save() {
        let (root, intent) = super::super::migration_intent::reviewed_app_routing_fixture();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
        environment.set("XDG_STATE_HOME", root.path().join("state"));
        environment.set("XDG_CONFIG_HOME", root.path().join("config"));
        let minimum = intent.application_minimum();
        assert!(minimum > super::super::staging::MIN_TARGET_BYTES);
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Password;
            preview.journey.identity = Some(Identity::New);
            preview.journey.name = "Retained identity".into();
            preview.journey.password = "review route fixture password".into();
            preview.journey.confirmation = preview.journey.password.clone();
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        wait_for_personal(&mut app);
        let identity = app
            .with_widget::<InstallerPreview, _>(|preview| {
                preview.personal.prepared_handle().unwrap()
            })
            .unwrap();
        let accounts = accounts::checkpoint_fixture_for_journey(&root.path().join("accounts"));
        let account_ids = accounts.prepared_handle().unwrap().selected_ids();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.page = Page::Review;
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, true, true];
            preview.accounts = accounts;
            preview.belongings.home = Some(root.path().join("source"));
            // The source fixture contains user app data, with no system refs.
            preview.belongings.flatpak = Some(super::super::flatpak_transfer::Inventory::default());
            preview.storage.set_application_minimum(minimum);
            preview.storage.stage();
            preview
                .migration_selection
                .prepare_routing_fixture(intent.clone());
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.categories[1] = false;
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains("installer.review.accounts")
        );
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "belongings".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
            .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "accounts".into(),
            included: false,
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(
                preview.accounts.prepared_handle().unwrap().selected_ids(),
                account_ids
            );
            assert_eq!(
                preview.migration_selection.reviewed_intent(),
                Some(intent.clone())
            );
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "review");
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.journey.categories[2]);
            assert!(!preview.accounts.has_prepared_connections());
            assert!(preview.accounts.prepared_handle().is_none());
            assert_eq!(preview.migration_selection.reviewed_intent(), Some(intent));
            assert_eq!(
                preview.personal.prepared_handle().unwrap().public_key(),
                identity.public_key()
            );
            assert_eq!(preview.journey.password, "review route fixture password");
        })
        .unwrap();
        app.settle();
        assert!(
            !app.harness()
                .inspection_json()
                .unwrap()
                .contains("installer.review.accounts")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn destination_edit_cancel_retains_reviewed_source_and_commit_discards_it() {
        let (root, intent) = super::super::migration_intent::reviewed_app_routing_fixture();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
        // Exercise the existing gallery's storage review route. This does not
        // mint live disk authority or invoke installation.
        environment.set(super::super::internal_install_flow::RUNTIME_GATE_VAR, "1");
        environment.set("XDG_STATE_HOME", root.path().join("state"));
        environment.set("XDG_CONFIG_HOME", root.path().join("config"));
        let minimum = intent.application_minimum();
        assert!(minimum > super::super::staging::MIN_TARGET_BYTES);
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Password;
            preview.journey.identity = Some(Identity::New);
            preview.journey.name = "Retained identity".into();
            preview.journey.password = "review route fixture password".into();
            preview.journey.confirmation = preview.journey.password.clone();
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        wait_for_personal(&mut app);
        let identity = app
            .with_widget::<InstallerPreview, _>(|preview| {
                preview.personal.prepared_handle().unwrap()
            })
            .unwrap();
        let accounts = accounts::checkpoint_fixture_for_journey(&root.path().join("accounts"));
        let account_ids = accounts.prepared_handle().unwrap().selected_ids();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.page = Page::Review;
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, true, true];
            preview.accounts = accounts;
            preview.belongings.home = Some(root.path().join("source"));
            // The source fixture contains user app data, with no system refs.
            preview.belongings.flatpak = Some(super::super::flatpak_transfer::Inventory::default());
            preview.storage.set_application_minimum(minimum);
            preview.storage.stage();
            preview
                .migration_selection
                .prepare_routing_fixture(intent.clone());
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "destination".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "review");
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(
                preview.migration_selection.reviewed_intent(),
                Some(intent.clone())
            );
        })
        .unwrap();
        let reviewed_choices = app
            .with_widget::<InstallerPreview, _>(|preview| preview.journey.clone())
            .unwrap();
        // Review Continue installs; use the ordinary destination edit to
        // inspect allocation while retaining the reviewed source and person.
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "destination".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "storage");
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.password, "review route fixture password");
            assert_eq!(
                preview.personal.prepared_handle().unwrap().public_key(),
                identity.public_key()
            );
            assert_eq!(
                preview.accounts.prepared_handle().unwrap().selected_ids(),
                account_ids
            );
            assert_eq!(
                preview.migration_selection.reviewed_intent(),
                Some(intent.clone())
            );
            assert!(visual_tests::control(preview, "installer.next").is_some());
            assert!(visual_tests::control(preview, "installer.prepare-usb").is_none());
            assert!(!preview.direct.active);
            assert!(preview.direct.pending.is_none());
            // Restore only this test's reviewed choices for the independent
            // cancel/change branches below; real custody and grants are retained.
            preview.journey = reviewed_choices;
            preview.dirty = true;
        })
        .unwrap();
        for cancel in [true, false] {
            app.effect(&InstallerPreviewEffect::EditPreview {
                section: "destination".into(),
            })
            .unwrap();
            app.effect(&InstallerPreviewEffect::ChoosePreview {
                choice: "another".into(),
            })
            .unwrap();
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(
                    preview.migration_selection.reviewed_intent(),
                    Some(intent.clone())
                );
                assert_eq!(preview.migration_selection.application_minimum(), minimum);
                assert_eq!(preview.storage.bounds.unwrap().0, minimum.div_ceil(1 << 30));
            })
            .unwrap();
            assert!(
                app.effect(&InstallerPreviewEffect::ExportPreparedSetup {
                    destination: "uncommitted-route-must-not-export".into(),
                    setup_binding: "uncommitted-route".into(),
                })
                .is_err()
            );
            if cancel {
                app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
                assert_eq!(status(&mut app)["page"], "review");
                assert_eq!(status(&mut app)["destination"], "computer");
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(
                        preview.migration_selection.reviewed_intent(),
                        Some(intent.clone())
                    );
                    assert_eq!(preview.migration_selection.application_minimum(), minimum);
                })
                .unwrap();
            } else {
                app.effect(&InstallerPreviewEffect::ContinuePreview)
                    .unwrap();
                assert_eq!(status(&mut app)["page"], "belongings");
                assert_eq!(status(&mut app)["destination"], "another");
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert!(preview.migration_selection.reviewed_intent().is_none());
                    assert_eq!(preview.storage.application_estimate(), None);
                    assert_eq!(
                        preview.storage.bounds.unwrap().0,
                        super::super::staging::MIN_TARGET_BYTES.div_ceil(1 << 30)
                    );
                })
                .unwrap();
            }
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(
                preview.personal.prepared_handle().unwrap().public_key(),
                identity.public_key()
            );
            assert_eq!(
                preview.accounts.prepared_handle().unwrap().selected_ids(),
                account_ids
            );
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SelectMigrationHome {
            source: root.path().join("source").to_str().unwrap().into(),
        })
        .unwrap();
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            app.settle();
            let snapshot = status(&mut app);
            if snapshot["page"] == "accounts"
                && snapshot["portable_preparation"]["prepared"] == true
                && snapshot["accounts"]["can_continue"] == true
                && snapshot["account_checkpoint"]["saving"] == false
                && snapshot["account_checkpoint"]["accounts"] == 2
            {
                break;
            }
            assert_ne!(
                snapshot["account_checkpoint"]["failed"], true,
                "retained account checkpoint failed: {snapshot}"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "fresh route unfinished: {snapshot}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview
                .portable_source
                .reviewed_handle()
                .unwrap()
                .validate()
                .unwrap();
            assert_eq!(
                preview.accounts.prepared_handle().unwrap().selected_ids(),
                account_ids
            );
            assert_eq!(
                preview.personal.prepared_handle().unwrap().public_key(),
                identity.public_key()
            );
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            app.settle();
            let snapshot = status(&mut app);
            assert_ne!(
                snapshot["account_checkpoint"]["failed"], true,
                "retained account checkpoint failed: {snapshot}"
            );
            assert_ne!(
                snapshot["media_preparation"]["state"], "failed",
                "retained preparation failed: {snapshot}"
            );
            if snapshot["page"] == "review" {
                assert_eq!(snapshot["destination"], "another");
                assert_eq!(snapshot["account_checkpoint"]["saving"], false);
                assert_eq!(snapshot["account_checkpoint"]["accounts"], 2);
                assert_eq!(snapshot["portable_preparation"]["prepared"], true);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "retained account checkpoint unfinished: {snapshot}"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.password, "review route fixture password");
            assert_eq!(
                preview.personal.prepared_handle().unwrap().public_key(),
                identity.public_key()
            );
            assert_eq!(
                preview.accounts.prepared_handle().unwrap().selected_ids(),
                account_ids
            );
            preview
                .portable_source
                .reviewed_handle()
                .unwrap()
                .validate()
                .unwrap();
            assert!(preview.accounts.has_prepared_connections());
            assert!(!preview.journey.history.contains(&Page::Storage));
            assert!(visual_tests::control(preview, "installer.prepare-usb").is_some());
        })
        .unwrap();
        assert_eq!(
            std::fs::read(root.path().join("source/original")).unwrap(),
            b"preserve"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn changing_routes_discards_previous_application_budget_and_migration_consent() {
        use super::super::migration_intent::reviewed_app_routing_fixture;
        for (page, effect) in [
            (
                Page::Migration,
                InstallerPreviewEffect::ChoosePreview {
                    choice: "fresh".into(),
                },
            ),
            (
                Page::Destination,
                InstallerPreviewEffect::ChoosePreview {
                    choice: "another".into(),
                },
            ),
            (
                Page::Belongings,
                InstallerPreviewEffect::SetPreviewCategory {
                    category: "flatpak".into(),
                    included: false,
                },
            ),
            (Page::Belongings, InstallerPreviewEffect::ContinuePreview),
        ] {
            let (_root, intent) = reviewed_app_routing_fixture();
            let minimum = intent.application_minimum();
            assert!(minimum > super::super::staging::MIN_TARGET_BYTES);
            let mut app = fixture();
            app.with_widget::<InstallerPreview, _>(|preview| {
                preview.journey.page = page;
                preview.journey.destination = Some(Destination::Computer);
                preview.journey.migrate = Some(true);
                preview.journey.details = true;
                preview.journey.categories =
                    if matches!(effect, InstallerPreviewEffect::ContinuePreview) {
                        [false, false, false]
                    } else {
                        [true, true, false]
                    };
                preview.storage.set_application_minimum(minimum);
                preview.storage.stage();
                preview.migration_selection.prepare_routing_fixture(intent);
                assert_eq!(preview.migration_selection.projection()["prepared"], true);
            })
            .unwrap();
            app.effect(&effect).unwrap();
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(preview.migration_selection.projection()["prepared"], false);
                assert_eq!(preview.storage.application_estimate(), None);
                assert_eq!(
                    preview.storage.bounds.unwrap().0,
                    (super::super::staging::MIN_TARGET_BYTES).div_ceil(1 << 30)
                );
            })
            .unwrap();
        }
    }

    #[test]
    fn existing_backup_recovery_preserves_identity_and_migration_choices() {
        use super::super::prepared_identity::PreparedIdentity;
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        env.set("XDG_STATE_HOME", root.join("state"));
        let donor = PreparedIdentity::create(
            &root.join("donor"),
            "Existing Ada",
            [25, 80, 120],
            tilo_identity_host::provisioning::IdentityStorage::for_session(),
        )
        .unwrap();
        let expected_photo =
            runtime_identity::ProfilePhoto::new(1, 1, vec![9, 20, 30, 255]).unwrap();
        donor.update_photo(Some(expected_photo.clone())).unwrap();
        let backup = root.join("original.sealed");
        donor
            .export(
                &backup,
                zeroize::Zeroizing::new("backup password for existing identity".into()),
            )
            .unwrap();
        let source_bytes = std::fs::read(&backup).unwrap();
        let donor_bytes = std::fs::read(root.join("donor/primary.json")).unwrap();
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "migrate");
        choose(&mut app, "existing");
        app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
            .unwrap();
        let accounts_before = status(&mut app)["accounts"].clone();
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "device".into(),
        })
        .unwrap();
        let intended_name = status(&mut app)["device_name"].as_str().unwrap().to_owned();
        app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
            value: String::new(),
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .is_err()
        );
        assert_eq!(status(&mut app)["pairing"]["state"], "unconnected");
        assert_eq!(status(&mut app)["device_name"], "");
        assert_eq!(status(&mut app)["page"], "connect");
        app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
            value: intended_name,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SelectExistingIdentityBackup {
            source_file: backup.to_string_lossy().into_owned(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetExistingIdentityPassword {
            value: "wrong backup password".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        let wait = |app: &mut TestApp<ImagerShell>| {
            let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
            loop {
                let state = status(app);
                if state["existing_identity"]["state"] != "restoring" {
                    break state;
                }
                assert!(std::time::Instant::now() < deadline, "{state}");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        let failed = wait(&mut app);
        assert_eq!(failed["existing_identity"]["state"], "failed");
        assert_eq!(failed["page"], "connect");
        assert!(failed["prepared_identity"]["public_key"].is_null());
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            let notice = visual_tests::control(preview, "installer.existing.failure")
                .expect("actual asynchronous recovery failure notice")
                .accessibility()
                .unwrap();
            assert_eq!(
                notice.name.as_deref(),
                Some(t("installer-existing-failed").as_str())
            );
            assert_eq!(notice.live, Some(ui::a11y::Politeness::Assertive));
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetExistingIdentityPassword {
            value: "backup password for existing identity".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        let accepted = wait(&mut app);
        wait_for_personal(&mut app);
        assert_eq!(accepted["existing_identity"]["state"], "ready");
        assert_eq!(
            accepted["page"], "connect",
            "recovery is reviewed before advancing"
        );
        assert_eq!(
            accepted["prepared_identity"]["public_key"],
            hex::encode(donor.public_key())
        );
        assert_eq!(accepted["accounts"], accounts_before);
        assert_eq!(accepted["migrate"], true);
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.name, "Existing Ada");
            let acknowledged = preview.personal.prepared_handle().unwrap();
            assert_eq!(acknowledged.public_key(), donor.public_key());
            assert_eq!(
                acknowledged.metadata().unwrap().profile_photo,
                Some(expected_photo.clone())
            );
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "password");
        password(&mut app);
        assert_eq!(status(&mut app)["page"], "devices");
        assert_eq!(std::fs::read(&backup).unwrap(), source_bytes);
        assert_eq!(
            std::fs::read(root.join("donor/primary.json")).unwrap(),
            donor_bytes
        );
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(status(&mut app)["page"], "identity");
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "new".into(),
        })
        .unwrap();
        assert!(
            status(&mut app)["prepared_identity"]["public_key"].is_null(),
            "first-time choice cannot reuse imported signing custody"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_password_continue_prepares_identity_and_profile_edits_keep_its_key() {
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let state_home = root.join("state");
        env.set("XDG_STATE_HOME", &state_home);
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
            .unwrap();
        password(&mut app);
        let created = wait_for_personal(&mut app);
        assert_eq!(created["page"], "devices");
        assert_eq!(created["prepared_identity"]["state"], "prepared");
        let preparations = state_home.join("tilo/installer/preparations");
        let entries = preparation_operations(&preparations);
        assert_eq!(entries.len(), 1);
        let record = entries.first().unwrap().join("identity/primary.json");
        let original = runtime_identity::load_from_path(record.clone()).unwrap();
        assert_eq!(original.display_name(), "Ada");
        let proof = original.sign(b"installer-profile-edit").unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Quinten".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        let edited = wait_for_personal(&mut app);
        assert_eq!(edited["page"], "devices");
        assert_eq!(
            edited["prepared_identity"]["public_key"],
            created["prepared_identity"]["public_key"]
        );
        let updated = runtime_identity::load_from_path(record).unwrap();
        assert_eq!(updated.display_name(), "Quinten");
        assert_eq!(updated.sign(b"installer-profile-edit").unwrap(), proof);
        assert_eq!(preparation_operations(&preparations).len(), 1);
        assert!(!edited.to_string().contains("preview-secret-only"));
        assert!(
            !edited
                .to_string()
                .contains(&root.to_string_lossy().to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn selected_avatar_is_cached_and_exported_without_rereading_originals() {
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let state_home = root.join("state");
        env.set("XDG_STATE_HOME", &state_home);
        let source = root.join("picture.png");
        let original = include!("../fixtures/avatar.rs");
        std::fs::write(&source, original).unwrap();
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SelectPreviewAvatar {
            source_file: source.to_string_lossy().into_owned(),
        })
        .unwrap();
        assert_eq!(
            wait_for_personal(&mut app)["prepared_identity"]["state"],
            "photo-selected"
        );
        assert_eq!(std::fs::read(&source).unwrap(), original);
        // A later original edit cannot change the already reviewed pixels.
        std::fs::write(&source, b"changed after selection").unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
            .unwrap();
        password(&mut app);
        let accepted = wait_for_personal(&mut app);
        assert_eq!(accepted["page"], "devices");
        let operations = preparation_operations(&state_home.join("tilo/installer/preparations"));
        assert_eq!(operations.len(), 1);
        let record = operations.first().unwrap().join("identity/primary.json");
        let identity = runtime_identity::load_from_path(record.clone()).unwrap();
        let expected = runtime_identity::ProfilePhoto::new(1, 1, vec![200, 100, 50, 255]).unwrap();
        assert_eq!(identity.profile_photo(), Some(&expected));
        let key = identity.group_public_key();
        let artifact = root.join("photo.sealed");
        runtime_identity::export_portable_backup(
            &record,
            &artifact,
            b"a sufficiently long photo password",
        )
        .unwrap();
        let restored = root.join("restored");
        std::fs::create_dir(&restored).unwrap();
        std::fs::set_permissions(&restored, std::fs::Permissions::from_mode(0o700)).unwrap();
        let imported = runtime_identity::import_portable_backup(
            &artifact,
            b"a sufficiently long photo password",
            &restored,
            "personal",
        )
        .unwrap();
        assert_eq!(
            runtime_identity::load_from_path(imported)
                .unwrap()
                .profile_photo(),
            Some(&expected)
        );
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        // Selecting the same path again explicitly consumes a new grant and
        // refuses the now-invalid file, without modifying the accepted identity.
        app.effect(&InstallerPreviewEffect::SelectPreviewAvatar {
            source_file: source.to_string_lossy().into_owned(),
        })
        .unwrap();
        assert_eq!(
            wait_for_personal(&mut app)["prepared_identity"]["state"],
            "failed"
        );
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(wait_for_personal(&mut app)["page"], "password");
        assert_eq!(
            runtime_identity::load_from_path(record)
                .unwrap()
                .group_public_key(),
            key
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"changed after selection");
    }

    #[cfg(unix)]
    #[test]
    fn ordinary_password_continue_refuses_symlink_custody_without_advancing() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let state_home = root.join("state");
        let outside = root.join("outside");
        std::fs::create_dir(&state_home).unwrap();
        std::fs::create_dir(&outside).unwrap();
        symlink(&outside, state_home.join("tilo")).unwrap();
        env.set("XDG_STATE_HOME", &state_home);
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
            .unwrap();
        password(&mut app);
        let failed = wait_for_personal(&mut app);
        assert_eq!(failed["page"], "password");
        assert_eq!(failed["prepared_identity"]["state"], "failed");
        assert!(failed["prepared_identity"]["public_key"].is_null());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn identity_preparation_and_export_use_device_local_public_effects() {
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let custody = root.join("identity");
        let artifact = root.join("identity.sealed");
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::CreatePreparedIdentity {
                private_custody: custody.to_string_lossy().into_owned(),
                avatar_color: "invalid".into(),
            })
            .is_err()
        );
        assert!(!custody.exists());
        app.effect(&InstallerPreviewEffect::CreatePreparedIdentity {
            private_custody: custody.to_string_lossy().into_owned(),
            avatar_color: "123456".into(),
        })
        .unwrap();
        let wait = |app: &mut TestApp<ImagerShell>| {
            let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
            loop {
                let snapshot = status(app);
                if !matches!(
                    snapshot["prepared_identity"]["state"].as_str(),
                    Some("preparing" | "updating" | "exporting" | "exporting-setup")
                ) {
                    return snapshot;
                }
                assert!(std::time::Instant::now() < deadline, "{snapshot}");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        let created = wait(&mut app);
        assert_eq!(
            created["prepared_identity"]["state"], "prepared",
            "{created}"
        );
        let key = created["prepared_identity"]["public_key"].clone();
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Quinten".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::UpdatePreparedIdentity {
            avatar_color: "abcdef".into(),
        })
        .unwrap();
        assert_eq!(wait(&mut app)["prepared_identity"]["public_key"], key);
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewPassword {
            value: "several unrelated installation words".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ExportPreparedIdentity {
            destination: artifact.to_string_lossy().into_owned(),
        })
        .unwrap();
        let exported = wait(&mut app);
        assert_eq!(
            exported["prepared_identity"]["state"], "exported",
            "{exported}"
        );
        assert_eq!(exported["prepared_identity"]["artifact"]["public_key"], key);
        assert!(artifact.is_file());
        let projection = exported.to_string();
        for private in [
            "several unrelated installation words",
            "secret_ref",
            "secret_bytes",
            "identity-secrets",
        ] {
            assert!(
                !projection.contains(private),
                "private value in public receipt"
            );
        }
        let metadata =
            runtime_identity::load_metadata_from_path(&custody.join("primary.json")).unwrap();
        assert_eq!(metadata.display_name, "Quinten");
        assert_eq!(metadata.avatar_color, [0xab, 0xcd, 0xef]);
        #[cfg(target_os = "linux")]
        {
            let package = root.join("setup.sealed");
            app.effect(&InstallerPreviewEffect::ExportPreparedSetup {
                destination: package.to_string_lossy().into_owned(),
                setup_binding: "chosen-installation-operation".into(),
            })
            .unwrap();
            let packaged = wait(&mut app);
            assert_eq!(
                packaged["prepared_identity"]["state"], "setup-exported",
                "{packaged}"
            );
            let reviewed = super::super::prepared_setup::PreparedSetupReview::open(
                &package,
                b"several unrelated installation words",
                &root.join("setup-quarantine"),
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(
                hex::encode(reviewed.manifest().identity_public_key),
                key.as_str().unwrap()
            );
            assert!(reviewed.manifest().account_ids.is_empty());
            assert_eq!(
                reviewed.manifest().setup_binding,
                "chosen-installation-operation"
            );
            assert!(
                !packaged
                    .to_string()
                    .contains("several unrelated installation words")
            );
        }
    }

    #[test]
    fn browser_choices_remain_available_when_files_and_accounts_are_off() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().canonicalize().unwrap();
        let config = home.join(".config");
        let profile = config.join("chromium/Default");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("Bookmarks"), b"bookmarks").unwrap();
        std::fs::write(profile.join("History"), b"private history").unwrap();
        std::fs::write(profile.join("Cookies"), b"original cookies").unwrap();
        let inventory = super::super::account_transfer::discover(
            &super::super::account_transfer::SourceRoots {
                home: home.clone(),
                config,
            },
        );
        let selection = browser::Selection::discover(&home, &inventory.candidates).unwrap();
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.journey.categories = [false; 3];
            preview.journey.details = true;
            preview.belongings.browser = selection;
            preview.dirty = true;
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "history".into(),
            included: false,
        })
        .unwrap();
        for (tag, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for (viewport, scale) in [((480, 800), 1.0), ((1180, 820), 1.0), ((2560, 1440), 2.0)] {
                app.harness()
                    .set_theme(theme.clone().with_ui_scale(scale).with_reduce_motion(true));
                app.harness().resize(viewport).unwrap();
                visual_tests::paint_app(
                    &mut app,
                    "installer-browser-selection",
                    &[
                        "installer.browser.bookmarks",
                        "installer.browser.history",
                        "installer.next",
                    ],
                    viewport,
                    tag,
                );
            }
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.categories, [false; 3]);
            assert_eq!(
                preview.journey.draft_choices().browser_domains,
                [true, false]
            );
            let selected = preview.belongings.browser.selected();
            assert_eq!(selected.profiles().len(), 1);
            let (source, profile) = &selected.profiles()[0];
            let snapshot = source.capture_files(profile).unwrap();
            assert_eq!(snapshot.files().len(), 1);
            assert_eq!(snapshot.files()[0].name(), "Bookmarks");
        })
        .unwrap();
        assert_eq!(
            std::fs::read(profile.join("Cookies")).unwrap(),
            b"original cookies"
        );
        assert_eq!(
            std::fs::read(profile.join("History")).unwrap(),
            b"private history"
        );
    }

    #[test]
    fn belongings_lists_saved_tilo_connections_without_projecting_configuration() {
        use super::super::account_transfer::{SourceRoots, discover};
        let source = tempfile::tempdir().unwrap();
        let config = source.path().join(".config");
        let mut store = ::accounts::AccountStore::new();
        let mut account = ::accounts::Account::new("work-files", "webdav");
        account.display_name = "Work files".into();
        account.config = serde_json::json!({
            "base_url": "https://files.example.test/dav",
            "username": "ada",
            "note": "private-configuration-not-for-projection"
        });
        store.upsert(account);
        store.save_in(&config.join("tilo")).unwrap();
        let metadata = ::accounts::store_path_in(&config.join("tilo"));
        let original = std::fs::read(&metadata).unwrap();
        let roots = SourceRoots {
            home: source.path().to_owned(),
            config,
        };
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.details = true;
            preview.belongings.accounts = discover(&roots);
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        let inspection = app.harness().inspection_json().unwrap();
        assert!(inspection.contains("Work files"));
        assert!(inspection.contains(&t("installer-accounts-webdav")));
        assert!(inspection.contains(&t("installer-preview-account-tilo")));
        assert!(!inspection.contains(&t("installer-preview-accounts-none")));
        assert!(!inspection.contains("private-configuration-not-for-projection"));
        let review = status(&mut app);
        assert_eq!(
            review["belongings"]["accounts"]["tilo_accounts"][0]["label"],
            "Work files"
        );
        assert!(
            !review
                .to_string()
                .contains("private-configuration-not-for-projection")
        );
        assert_eq!(std::fs::read(metadata).unwrap(), original);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn final_review_shows_independent_browser_and_selected_session_choices() {
        use super::super::account_transfer::{SourceRoots, discover};
        use std::os::unix::fs::PermissionsExt;
        let source = tempfile::tempdir().unwrap();
        std::fs::set_permissions(source.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let profile = source.path().join(".config/chromium/Default");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("Bookmarks"), br#"{"roots":{}}"#).unwrap();
        std::fs::write(profile.join("History"), b"private-history-never-in-review").unwrap();
        let cookies = rusqlite::Connection::open(profile.join("Cookies")).unwrap();
        cookies
            .execute_batch(
                "CREATE TABLE meta(key TEXT,value TEXT);
            INSERT INTO meta VALUES('version','24');
            CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,path TEXT,expires_utc INTEGER,
            is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,has_expires INTEGER,
            encrypted_value BLOB,top_frame_site_key TEXT,source_scheme INTEGER,source_port INTEGER,
            creation_utc INTEGER);
            INSERT INTO cookies VALUES('.slack.com','d','private-cookie-never-in-review','/',
                0,1,1,-1,0,X'','',2,443,1);",
            )
            .unwrap();
        drop(cookies);
        let original_cookies = std::fs::read(profile.join("Cookies")).unwrap();
        let roots = SourceRoots {
            home: source.path().to_owned(),
            config: source.path().join(".config"),
        };
        let inventory = discover(&roots);
        let browser = browser::Selection::discover(source.path(), &inventory.candidates).unwrap();
        let sessions =
            service_sessions::Selection::discover(source.path(), &inventory.candidates).unwrap();
        let report = sessions.report();
        let source_id = report["sources"][0]["id"].as_str().unwrap().to_owned();
        let source_label = report["sources"][0]["label"].as_str().unwrap().to_owned();
        let mut app = fixture();
        choose(&mut app, "another");
        choose(&mut app, "migrate");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Installer Review Person".into(),
        })
        .unwrap();
        let avatar = source.path().join("review-avatar.png");
        std::fs::write(&avatar, include!("../fixtures/avatar.rs")).unwrap();
        app.effect(&InstallerPreviewEffect::SelectPreviewAvatar {
            source_file: avatar.to_string_lossy().into_owned(),
        })
        .unwrap();
        assert_eq!(
            wait_for_personal(&mut app)["prepared_identity"]["state"],
            "photo-selected"
        );
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        password(&mut app);
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "belongings");
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.personal.photo().is_some());
            assert_eq!(preview.journey.name, "Installer Review Person");
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.journey.destination = Some(Destination::Another);
            preview.journey.categories = [false, false, true];
            preview.journey.details = true;
            preview.belongings.browser = browser;
            preview.belongings.sessions = sessions;
            preview.dirty = true;
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "history".into(),
            included: false,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewServiceSession {
            source_id: source_id.clone(),
            service: "slack".into(),
            included: true,
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.page = Page::Review;
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        let review = app.harness().inspection_json().unwrap();
        assert!(review.contains("Installer Review Person"));
        assert!(review.contains("installer.review.profile"));
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.journey.categories[2]);
            assert!(!preview.accounts.has_prepared_connections());
        })
        .unwrap();
        assert!(!review.contains("installer.review.accounts"));
        assert!(!review.contains(&t("installer-preview-accounts-title")));
        assert!(review.contains("installer.review.bookmarks"));
        assert!(!review.contains("installer.review.history"));
        assert!(review.contains("installer.review.session.0"));
        assert!(review.contains(&t_args(
            "installer-service-source-slack",
            &[("source", source_label.clone().into())]
        )));
        assert!(!review.contains(&t_args(
            "installer-service-source-discord",
            &[("source", source_label.into())]
        )));
        for private in [
            "private-history-never-in-review",
            "private-cookie-never-in-review",
            source.path().to_str().unwrap(),
        ] {
            assert!(!review.contains(private));
        }
        for (tag, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for (viewport, scale) in [((480, 800), 1.0), ((1180, 820), 1.0), ((2560, 1440), 2.0)] {
                app.harness().resize(viewport).unwrap();
                app.harness()
                    .set_theme(theme.clone().with_ui_scale(scale).with_reduce_motion(true));
                let body = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        visual_tests::body_scroll_geometry(preview).unwrap().0
                    })
                    .unwrap();
                app.harness().click_at(body.x0 + 4.0, body.y0 + 4.0);
                for _ in 0..40 {
                    app.harness().scroll(0.0, 10.0);
                }
                app.settle();
                let (footer, original_last) = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        (
                            visual_tests::control(preview, "installer.prepare-usb")
                                .unwrap()
                                .get_rect(),
                            visual_tests::control(preview, "installer.review.session.0")
                                .unwrap()
                                .get_rect(),
                        )
                    })
                    .unwrap();
                visual_tests::paint_app(
                    &mut app,
                    "installer-selected-session-review-top",
                    &["installer.review.profile", "installer.prepare-usb"],
                    viewport,
                    tag,
                );
                // Use the ordinary body wheel to reveal all final selections;
                // a computed offscreen row is not a visible review receipt.
                app.harness().click_at(body.x0 + 4.0, body.y0 + 4.0);
                for _ in 0..40 {
                    app.harness().scroll(0.0, -10.0);
                }
                app.settle();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(visual_tests::control(preview, "installer.prepare-usb").unwrap().get_rect(), footer);
                    let last = visual_tests::control(preview, "installer.review.session.0").unwrap().get_rect();
                    assert!(last.y0 >= body.y0 && last.y1 <= body.y1 && body.y1 <= footer.y0,
                        "selected review row must be revealed above the pinned footer: {last:?} {footer:?}; scroll={:?}", visual_tests::body_scroll_geometry(preview));
                    if original_last.y1 > footer.y0 {
                        assert!(last.y0 < original_last.y0,
                            "ordinary wheel must actually move the overflowing review body");
                    }
                }).unwrap();
                visual_tests::paint_app(
                    &mut app,
                    "installer-selected-session-review",
                    &[
                        "installer.review.bookmarks",
                        "installer.review.session.0",
                        "installer.prepare-usb",
                    ],
                    viewport,
                    tag,
                );
                let painted_review = app.harness().inspection_json().unwrap();
                assert!(!painted_review.contains("installer.review.accounts"));
                assert!(painted_review.contains("installer.review.bookmarks"));
                assert!(painted_review.contains("installer.review.session.0"));
                assert!(!painted_review.contains("installer.review.history"));
                for private in [
                    "private-history-never-in-review",
                    "private-cookie-never-in-review",
                    source.path().to_str().unwrap(),
                ] {
                    assert!(!painted_review.contains(private));
                }
            }
        }
        // Editing a selected item opens its choices directly, retaining the
        // same public edit action and cancellation semantics.
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "belongings-details".into(),
        })
        .unwrap();
        assert_eq!(status(&mut app)["page"], "belongings");
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "bookmarks".into(),
            included: false,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "history".into(),
            included: true,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewServiceSession {
            source_id,
            service: "slack".into(),
            included: false,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.settle();
        let restored = app.harness().inspection_json().unwrap();
        assert!(restored.contains("installer.review.bookmarks"));
        assert!(!restored.contains("installer.review.history"));
        assert!(restored.contains("installer.review.session.0"));
        assert_eq!(
            std::fs::read(profile.join("Cookies")).unwrap(),
            original_cookies
        );
        assert_eq!(
            std::fs::read(profile.join("History")).unwrap(),
            b"private-history-never-in-review"
        );
    }

    #[cfg(target_os = "linux")]
    struct SettingsLocations(std::path::PathBuf);
    #[cfg(target_os = "linux")]
    impl super::super::flatpak_transfer::Runner for SettingsLocations {
        fn run(&self, _: &std::path::Path, args: &[String]) -> Result<String, String> {
            assert_eq!(args.first().map(String::as_str), Some("info"));
            Ok(format!(
                "{}/app/org.tilo.Settings/x86_64/stable/{}\n",
                self.0.display(),
                "a".repeat(64)
            ))
        }
    }

    #[cfg(target_os = "linux")]
    fn settings_details_inventory() -> super::super::flatpak_transfer::Inventory {
        let installed = super::super::flatpak_transfer::InstalledRef {
            reference: "app/org.tilo.Settings/x86_64/stable".into(),
            origin: "approved".into(),
            installation: super::super::flatpak_transfer::Installation::System("named".into()),
            name: "Settings fixture".into(),
        };
        super::super::flatpak_transfer::Inventory {
            refs: vec![installed],
            remotes: Vec::new(),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn flatpak_custom_access_details_follow_selection_and_never_publish_private_values() {
        use super::super::flatpak_transfer::{self, Installation};
        use std::os::unix::fs::PermissionsExt;
        let source = tempfile::tempdir().unwrap();
        let root = source.path().join("installation");
        std::fs::create_dir_all(root.join("overrides")).unwrap();
        let file = root.join("overrides/org.tilo.Settings");
        std::fs::write(&file, "[Context]\nfilesystems=/private/source-value;\n[Environment]\nPRIVATE=private-environment-value\n").unwrap();
        let inventory = settings_details_inventory();
        let notices = flatpak_transfer::review_administrator_settings(
            &SettingsLocations(root.clone()),
            source.path(),
            &inventory,
        )
        .expect("actual held files reviewed");
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.details = true;
            preview.journey.categories = [false, true, false];
            preview.belongings.flatpak = Some(inventory);
            preview.belongings.administrator_settings = Some(notices);
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        let collapsed = app.harness().inspection_json().unwrap();
        assert!(collapsed.contains("installer.flatpak.access-details"));
        assert!(collapsed.contains(&t("installer-flatpak-settings-note")));
        assert!(!collapsed.contains("installer.flatpak.settings.0"));
        assert_eq!(status(&mut app)["flatpak_access_details"], false);
        app.effect(&InstallerPreviewEffect::ToggleFlatpakAccessDetails)
            .unwrap();
        app.settle();
        let view = app.harness().inspection_json().unwrap();
        assert!(view.contains("installer.flatpak.settings.0"));
        assert_eq!(status(&mut app)["flatpak_access_details"], true);
        assert!(view.contains(&t("installer-flatpak-settings-folders")));
        assert!(view.contains(&t("installer-flatpak-settings-environment")));
        let review = status(&mut app).to_string();
        for private in ["/private/source-value", "private-environment-value"] {
            assert!(!view.contains(private));
            assert!(!review.contains(private));
        }
        app.effect(&InstallerPreviewEffect::ToggleFlatpakAccessDetails)
            .unwrap();
        app.settle();
        assert!(
            !app.harness()
                .inspection_json()
                .unwrap()
                .contains("installer.flatpak.settings.0")
        );
        app.effect(&InstallerPreviewEffect::ToggleFlatpakAccessDetails)
            .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "flatpak".into(),
            included: false,
        })
        .unwrap();
        app.settle();
        assert!(
            !app.harness()
                .inspection_json()
                .unwrap()
                .contains("installer.flatpak.settings.0")
        );
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "flatpak".into(),
            included: true,
        })
        .unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.belongings.administrator_settings =
                flatpak_transfer::review_administrator_settings(
                    &SettingsLocations(root),
                    source.path(),
                    preview.belongings.flatpak.as_ref().unwrap(),
                );
            assert!(preview.belongings.administrator_settings.is_none());
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains(&t("installer-flatpak-settings-unchecked"))
        );
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview
                .belongings
                .flatpak
                .as_mut()
                .unwrap()
                .refs
                .first_mut()
                .unwrap()
                .installation = Installation::User;
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        assert!(
            !app.harness()
                .inspection_json()
                .unwrap()
                .contains(&t("installer-flatpak-settings-unchecked")),
            "user-only legacy source must not get administrator warning"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn named_app_action_drives_canonical_portable_selection_review() {
        use super::super::flatpak_transfer::{self, portable::PortableFlatpakSource};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_STATE_HOME", root.join("state"));
        env.set("XDG_CONFIG_HOME", root.join("config"));
        let runner = flatpak_transfer::portable::tests::fixture(&root);
        let inventory = flatpak_transfer::discover(&runner, &runner.source_home).unwrap();
        let selected = inventory
            .refs
            .iter()
            .find(|item| item.app_id() == Some("org.example.Paint"))
            .unwrap()
            .clone();
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.destination = Some(Destination::Another);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [false, true, false];
            preview.journey.details = true;
            preview.belongings.home = Some(runner.source_home.clone());
            preview.belongings.flatpak = Some(inventory);
            preview.dirty = true;
        })
        .unwrap();
        let before = status(&mut app);
        let id = before["belongings"]["apps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == selected.name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: id.clone(),
            included: false,
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.migration_sources().unwrap().1.is_empty());
            assert!(
                !preview.migration_categories()[1],
                "an empty application selection must not start export"
            );
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: id,
            included: true,
        })
        .unwrap();
        let (source_home, refs) = app
            .with_widget::<InstallerPreview, _>(|preview| preview.migration_sources().unwrap())
            .unwrap();
        assert_eq!(refs, vec![selected]);
        // Actual retained repository/deployment metadata and canonical closed
        // export review consume precisely the semantic action's selection.
        let source = PortableFlatpakSource::review(&runner, &source_home, refs).unwrap();
        source.validate().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn named_user_app_action_drives_actual_selected_data_stage_without_home() {
        use super::super::flatpak_transfer::{
            self,
            portable::{PortableFlatpakSource, StageGrant},
        };
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_STATE_HOME", root.join("state"));
        env.set("XDG_CONFIG_HOME", root.join("config"));
        let fake = flatpak_transfer::portable::tests::fixture(&root);
        let runner = flatpak_transfer::portable::tests::UserSubset(&fake);
        let chosen = fake.source_home.join(".var/app/org.example.Paint");
        let excluded = fake.source_home.join(".var/app/org.example.Excluded");
        for directory in [&chosen, &excluded] {
            std::fs::create_dir_all(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(chosen.join("settings"), b"selected account app data").unwrap();
        std::fs::write(excluded.join("settings"), b"unselected original data").unwrap();
        let inventory = flatpak_transfer::discover(&runner, &fake.source_home).unwrap();
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.destination = Some(Destination::Another);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [false, true, false];
            preview.journey.details = true;
            preview.belongings.home = Some(fake.source_home.clone());
            preview.belongings.flatpak = Some(inventory);
            preview.dirty = true;
        })
        .unwrap();
        let initial = status(&mut app);
        let row = &initial["belongings"]["apps"][0];
        assert_eq!(row["selectable"], true);
        assert_eq!(row["included"], true);
        assert_eq!(row["with_home"], false);
        let id = row["id"].as_str().unwrap().to_owned();
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: id.clone(),
            included: false,
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.migration_sources().unwrap().1.is_empty());
            assert_eq!(preview.migration_categories(), [false, false, false]);
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: id,
            included: true,
        })
        .unwrap();
        let (home, refs) = app
            .with_widget::<InstallerPreview, _>(|preview| preview.migration_sources().unwrap())
            .unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].installation, flatpak_transfer::Installation::User);
        let reviewed = PortableFlatpakSource::review_selected_state(&runner, &home, refs).unwrap();
        let stage = root.join("selected-user-stage");
        let journals = root.join("selected-user-journals");
        for directory in [&stage, &journals] {
            std::fs::create_dir(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        reviewed
            .stage(
                &runner,
                StageGrant {
                    root: &stage,
                    journals: &journals,
                    allocation_bytes: 64 << 20,
                    include_user_state: true,
                },
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(
            std::fs::read(stage.join("app-state/org.example.Paint/data/settings")).unwrap(),
            b"selected account app data"
        );
        assert!(!stage.join("app-state/org.example.Excluded").exists());
        assert!(!stage.join(".local/share/flatpak").exists());
        assert_eq!(
            std::fs::read(chosen.join("settings")).unwrap(),
            b"selected account app data"
        );
        assert_eq!(
            std::fs::read(excluded.join("settings")).unwrap(),
            b"unselected original data"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn expanded_app_actions_filter_actual_migration_requests_and_preserve_home() {
        use super::super::flatpak_transfer::{self, Runner};
        struct Discovery;
        impl Runner for Discovery {
            fn run(&self, _: &std::path::Path, args: &[String]) -> Result<String, String> {
                match args.first().map(String::as_str) {
                    Some("list") if args.iter().any(|arg| arg == "--app") => Ok(concat!(
                        "org.fixture.Player/x86_64/stable\tapproved\tuser\tPersonal Player\n",
                        "org.fixture.Player/x86_64/stable\tapproved\tsystem\tDuplicate Player\n",
                        "org.fixture.Editor/x86_64/stable\tapproved\tsystem\tFriendly Editor\n",
                        "org.fixture.Reader/x86_64/stable\tapproved\tsystem\tBook Reader\n"
                    )
                    .into()),
                    Some("list") => Ok(
                        "org.fixture.Runtime/x86_64/stable\tapproved\tsystem\tRuntime dependency\n"
                            .into(),
                    ),
                    Some("remotes") => Ok("approved\thttps://example.invalid/repository\n".into()),
                    _ => Err("unexpected discovery operation".into()),
                }
            }
        }
        let home = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_STATE_HOME", home.path().join("state"));
        env.set("XDG_CONFIG_HOME", home.path().join("config"));
        let inventory = flatpak_transfer::discover(&Discovery, home.path()).unwrap();
        let editor = inventory
            .refs
            .iter()
            .find(|item| item.app_id() == Some("org.fixture.Editor"))
            .unwrap()
            .clone();
        let reader = inventory
            .refs
            .iter()
            .find(|item| item.app_id() == Some("org.fixture.Reader"))
            .unwrap()
            .clone();
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.destination = Some(Destination::Another);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, true, false];
            preview.journey.details = true;
            preview.belongings.home = Some(home.path().to_owned());
            preview.belongings.flatpak = Some(inventory);
            preview.dirty = true;
        })
        .unwrap();
        let initial = status(&mut app);
        let rows = initial["belongings"]["apps"].as_array().unwrap();
        let editor_id = rows
            .iter()
            .find(|row| row["name"] == "Friendly Editor")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let reader_id = rows
            .iter()
            .find(|row| row["name"] == "Book Reader")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let home_id = rows.iter().find(|row| row["with_home"] == true).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: editor_id.clone(),
            included: false,
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            // These are the exact selections passed to original-source and
            // independent portable capture, not a list reconstructed by the test.
            let (source, selected) = preview.migration_sources().unwrap();
            assert_eq!(source, home.path());
            assert_eq!(selected, vec![reader.clone()]);
            assert_eq!(preview.migration_categories(), [true, true, false]);
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
                app: home_id.clone(),
                included: false
            })
            .is_err()
        );
        assert!(
            app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
                app: "unknown-app".into(),
                included: false
            })
            .is_err()
        );
        assert_eq!(status(&mut app)["belongings"]["apps"][0]["with_home"], true);
        // Category changes suspend inclusion, preserving individual exclusions.
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "flatpak".into(),
            included: false,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "flatpak".into(),
            included: true,
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.migration_sources().unwrap().1, vec![reader.clone()]);
            let choices = preview.journey.draft_choices();
            let encoded = serde_json::to_vec(&choices).unwrap();
            let restored: draft::PublicChoices = serde_json::from_slice(&encoded).unwrap();
            preview
                .journey
                .restore_draft(&restored, draft::IdentityKind::New, "Ada".into());
            preview.journey.page = Page::Belongings;
            preview.journey.details = true;
            assert_eq!(preview.migration_sources().unwrap().1, vec![reader.clone()]);
            preview.journey.page = Page::Review;
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "belongings-details".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: editor_id.clone(),
            included: true,
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(
                preview.migration_sources().unwrap().1,
                vec![editor.clone(), reader.clone()]
            );
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Review);
            assert_eq!(preview.migration_sources().unwrap().1, vec![reader.clone()]);
            preview.journey.page = Page::Belongings;
            preview.journey.details = true;
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "files".into(),
            included: false,
        })
        .unwrap();
        assert_eq!(status(&mut app)["belongings"]["apps"][0]["included"], true);
        assert_eq!(
            status(&mut app)["belongings"]["apps"][0]["selectable"],
            true
        );
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: home_id,
            included: false,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: reader_id,
            included: false,
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.migration_sources().unwrap().1.is_empty());
            assert_eq!(preview.migration_categories(), [false, false, false]);
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: editor_id,
            included: true,
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.migration_sources().unwrap().1, vec![editor.clone()]);
            assert_eq!(preview.migration_categories(), [false, true, false]);
        })
        .unwrap();
        app.settle();
        let painted = app.harness().inspection_json().unwrap();
        assert!(painted.contains("Friendly Editor"));
        assert!(!painted.contains(&t("installer-flatpak-app-needs-home")));
        assert!(painted.contains("Personal Player"));
        assert!(!painted.contains("Duplicate Player"));
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.categories[0] = true;
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        let painted = app.harness().inspection_json().unwrap();
        assert!(!painted.contains(&t("installer-flatpak-app-transfer-unavailable")));
        assert!(!painted.contains(&t("installer-flatpak-app-needs-home")));
        let choices = status(&mut app);
        assert_eq!(choices["belongings"]["apps"][0]["included"], false);
        assert_eq!(choices["belongings"]["apps"][0]["selectable"], true);
        assert_eq!(choices["belongings"]["apps"][0]["with_home"], false);
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: choices["belongings"]["apps"][0]["id"]
                .as_str()
                .unwrap()
                .to_owned(),
            included: true,
        })
        .unwrap();
        assert_eq!(status(&mut app)["belongings"]["apps"][0]["included"], true);
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.journey.categories[0]);
        })
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn expanded_flatpak_names_follow_actual_category_actions_and_discovery() {
        use super::super::flatpak_transfer::{self, Runner};
        // Synthetic CLI fixture exercises the production discovery parser and
        // public action path; the native fixture separately uses real installs.
        struct Discovery;
        impl Runner for Discovery {
            fn run(&self, _: &std::path::Path, args: &[String]) -> Result<String, String> {
                match args.first().map(String::as_str) {
                    Some("list") if args.iter().any(|arg| arg == "--app") => Ok(concat!(
                        "org.fixture.Player/x86_64/stable\tapproved\tuser\tPersonal Player\n",
                        "org.fixture.Player/x86_64/stable\tapproved\tsystem\tDuplicate Player\n",
                        "org.fixture.Editor/x86_64/stable\tapproved\tsystem\tFriendly Editor\n"
                    )
                    .into()),
                    Some("list") => Ok(
                        "org.fixture.Runtime/x86_64/stable\tapproved\tsystem\tRuntime dependency\n"
                            .into(),
                    ),
                    Some("remotes") => Ok("approved\thttps://example.invalid/repository\n".into()),
                    _ => Err("unexpected discovery operation".into()),
                }
            }
        }
        let home = tempfile::tempdir().unwrap();
        let inventory = flatpak_transfer::discover(&Discovery, home.path()).unwrap();
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.destination = Some(Destination::Another);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, false, false];
            preview.journey.details = false;
            preview.belongings.home = Some(home.path().to_owned());
            preview.belongings.flatpak = Some(inventory);
            preview.dirty = true;
        })
        .unwrap();
        let expanded = app
            .effect(&InstallerPreviewEffect::TogglePreviewDetails)
            .unwrap()
            .value
            .unwrap();
        let rows = expanded["belongings"]["apps"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            2,
            "runtime dependencies and duplicate system copies are not separate app choices"
        );
        assert_eq!(rows[0]["name"], "Personal Player");
        assert_eq!(rows[0]["included"], true);
        assert_eq!(rows[0]["with_home"], true);
        assert_eq!(rows[1]["name"], "Friendly Editor");
        assert_eq!(rows[1]["included"], false);
        app.settle();
        let painted = app.harness().inspection_json().unwrap();
        assert!(painted.contains("Personal Player"));
        assert!(painted.contains(&t("installer-flatpak-app-with-home")));
        assert!(painted.contains("Friendly Editor"));
        assert!(painted.contains("installer.flatpak.app.1"));
        assert!(!painted.contains("Duplicate Player"));
        assert!(!painted.contains("Runtime dependency"));
        assert!(!painted.contains("org.fixture."));
        let selected = app
            .effect(&InstallerPreviewEffect::SetPreviewCategory {
                category: "flatpak".into(),
                included: true,
            })
            .unwrap()
            .value
            .unwrap();
        assert_eq!(selected["belongings"]["apps"][1]["included"], true);
        app.settle();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains("Friendly Editor")
        );
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "files".into(),
            included: false,
        })
        .unwrap();
        let selected = status(&mut app);
        assert_eq!(selected["belongings"]["apps"][0]["with_home"], false);
        assert_eq!(selected["belongings"]["apps"][0]["included"], true);
        assert_eq!(selected["belongings"]["apps"][0]["selectable"], true);
        let user_id = selected["belongings"]["apps"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: user_id,
            included: false,
        })
        .unwrap();
        assert_eq!(status(&mut app)["belongings"]["apps"][0]["included"], false);
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.categories[0] = true;
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        let computer = status(&mut app);
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.journey.categories[0]);
        })
        .unwrap();
        for row in computer["belongings"]["apps"].as_array().unwrap() {
            assert_eq!(row["selectable"], true);
            assert_eq!(row["with_home"], false);
            assert_eq!(row["already_here"], false);
        }
        assert_eq!(
            computer["belongings"]["apps"][0]["included"], false,
            "explicitly excluded User copy remains excluded"
        );
        assert_eq!(
            computer["belongings"]["apps"][1]["included"], true,
            "selected System app remains independent"
        );
        let user = computer["belongings"]["apps"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let system = computer["belongings"]["apps"][1]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: user,
            included: true,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewFlatpakApp {
            app: system,
            included: false,
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.migration_categories(), [true, true, false]);
            let selected = preview.migration_sources().unwrap().1;
            assert_eq!(selected.len(), 1);
            assert_eq!(
                selected[0].installation,
                flatpak_transfer::Installation::User
            );
        })
        .unwrap();
        app.settle();
        let painted = app.harness().inspection_json().unwrap();
        assert!(!painted.contains(&t("installer-flatpak-app-with-home")));
        assert!(painted.contains("Personal Player"));
        assert!(!painted.contains("Duplicate Player"));
        assert!(painted.contains(&t_args(
            "installer-preview-flatpak-computer-found",
            &[("count", "2".into())]
        )));
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.destination = Some(Destination::Desktop);
            preview.dirty = true;
        })
        .unwrap();
        let existing = status(&mut app);
        for row in existing["belongings"]["apps"].as_array().unwrap() {
            assert_eq!(row["already_here"], true);
            assert_eq!(
                row["included"], false,
                "desktop does not export an app carrier"
            );
        }
        app.settle();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains(&t("installer-flatpak-app-existing"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn expanded_flatpak_details_scroll_with_pinned_actions_at_actual_device_scales() {
        use super::super::flatpak_transfer::{self, Installation, InstalledRef, Inventory, Runner};
        struct Locations(std::path::PathBuf);
        impl Runner for Locations {
            fn run(&self, _: &std::path::Path, args: &[String]) -> Result<String, String> {
                assert_eq!(args.first().map(String::as_str), Some("info"));
                Ok(format!(
                    "{}/{}/{}\n",
                    self.0.display(),
                    args.last().unwrap(),
                    "a".repeat(64)
                ))
            }
        }
        struct RestoreScale(u32);
        impl Drop for RestoreScale {
            fn drop(&mut self) {
                ui::offscreen::set_capture_device_scale(self.0);
            }
        }
        let _restore = RestoreScale(ui::offscreen::capture_device_scale());
        let source = tempfile::tempdir().unwrap();
        let root = source.path().join("installation");
        std::fs::create_dir_all(root.join("overrides")).unwrap();
        std::fs::write(root.join("overrides/global"), "[Context]\nfilesystems=/private/never-visible;\n[Environment]\nSECRET=never-visible-value\n").unwrap();
        let inventory = Inventory {
            refs: (0..24)
                .map(|index| InstalledRef {
                    reference: format!("app/org.tilo.Settings{index}/x86_64/stable"),
                    origin: "approved".into(),
                    installation: Installation::System("named".into()),
                    name: format!("Settings fixture {index}"),
                })
                .collect(),
            remotes: Vec::new(),
        };
        let notices = flatpak_transfer::review_administrator_settings(
            &Locations(root),
            source.path(),
            &inventory,
        )
        .unwrap();
        for (tag, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for (viewport, device_scale) in [((280, 800), 1), ((480, 800), 1), ((1280, 720), 2)] {
                ui::offscreen::set_capture_device_scale(device_scale);
                let mut app = fixture();
                app.harness().resize(viewport).unwrap();
                app.harness()
                    .set_input_scale(f64::from(device_scale))
                    .unwrap();
                app.harness()
                    .set_theme(theme.clone().with_reduce_motion(true));
                app.with_widget::<InstallerPreview, _>(|preview| {
                    preview.live = true;
                    preview.journey.page = Page::Belongings;
                    preview.journey.migrate = Some(true);
                    preview.journey.destination = Some(Destination::Computer);
                    preview.journey.details = true;
                    preview.journey.categories = [false, true, false];
                    preview.belongings.flatpak = Some(inventory.clone());
                    preview.belongings.administrator_settings = Some(notices.clone());
                    preview.flatpak_access_details = true;
                    preview.dirty = true;
                })
                .unwrap();
                app.settle();
                let capture = |app: &mut TestApp<ImagerShell>, phase: &str| {
                    let physical = (viewport.0 * device_scale, viewport.1 * device_scale);
                    let rgba = app.harness().render_rgba();
                    assert_eq!(
                        rgba.len(),
                        usize::try_from(physical.0).unwrap()
                            * usize::try_from(physical.1).unwrap()
                            * 4
                    );
                    let ground = &rgba[..4];
                    assert!(
                        rgba.chunks_exact(4)
                            .filter(|pixel| *pixel != ground)
                            .count()
                            > 200
                    );
                    if let Some(directory) = std::env::var_os("TILO_UI_SCREENSHOT_DIR") {
                        let path = std::path::PathBuf::from(directory).join("installer-journey").join(format!("installer-flatpak-expanded-{phase}-{tag}-{}x{}-{device_scale}x.png", physical.0, physical.1));
                        app.harness().render_png(&path);
                        let png = std::fs::read(&path).unwrap();
                        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
                        assert_eq!(
                            u32::from_be_bytes(png[16..20].try_into().unwrap()),
                            physical.0
                        );
                        assert_eq!(
                            u32::from_be_bytes(png[20..24].try_into().unwrap()),
                            physical.1
                        );
                        eprintln!("installer visual evidence: {}", path.display());
                    }
                };
                capture(&mut app, "top");
                let footer = app
                    .with_widget::<InstallerPreview, _>(|preview| {
                        let next = find_control(preview, "installer.next").get_rect();
                        let back = find_control(preview, "installer.back").get_rect();
                        let heading = find_control(preview, "installer.heading");
                        let heading_rect = heading.get_rect();
                        let heading_style = theme.text(TextRole::LargeTitle);
                        let heading_name = heading.accessibility().unwrap().name.unwrap();
                        assert_eq!(heading_name, t("installer-preview-belongings-heading"));
                        let mut renderer = ui::TextRenderer::new();
                        let (natural_width, _) = renderer.measure_style(&heading_name, None, &heading_style);
                        eprintln!("heading measurement {tag}/{device_scale}x: natural={natural_width}, allocated={heading_rect:?}, font={}", heading_style.size);
                        if viewport.0 == 280 {
                            assert!(f64::from(natural_width) > heading_rect.width() + 1.0,
                                "narrow viewport must actually wrap the canonical heading: {natural_width}, {heading_rect:?}");
                        } else if viewport.0 == 480 {
                            assert!(f64::from(natural_width) <= heading_rect.width(),
                                "normal compact viewport must fit the factual heading without forced wrapping: {natural_width}, {heading_rect:?}");
                        }
                        assert!(
                            heading_rect.x0 >= 0.0
                                && heading_rect.x1 <= f64::from(viewport.0) - 24.0,
                            "heading must fit inside page gutter: {heading_rect:?}"
                        );
                        if f64::from(natural_width) > heading_rect.width() + 1.0 {
                            assert!(
                                heading_rect.height() > f64::from(heading_style.size) * 1.5,
                                "long heading must wrap on the narrow screen: {heading_rect:?}"
                            );
                        }
                        for rect in [next, back] {
                            assert!(rect.width() >= 24.0 && rect.height() >= 20.0);
                            assert!(rect.x0 >= 0.0 && rect.x1 <= f64::from(viewport.0));
                            assert!(rect.y0 >= 0.0 && rect.y1 <= f64::from(viewport.1));
                        }
                        let last =
                            find_control(preview, "installer.flatpak.settings.23").get_rect();
                        assert!(
                            last.y0 >= next.y0,
                            "fixture must really overflow: {last:?} {next:?}"
                        );
                        (next, back)
                    })
                    .unwrap();
                // A blank part of the real scrolling body establishes pointer
                // location; ordinary wheel input must reveal the final notice.
                app.harness().click_at(f64::from(viewport.0) / 2.0, 100.0);
                for _ in 0..40 {
                    app.harness().scroll(0.0, -10.0);
                }
                app.settle();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    let last = find_control(preview, "installer.flatpak.settings.23").get_rect();
                    assert!(
                        last.y0 >= 0.0 && last.y1 <= footer.0.y0,
                        "last notice must be visible above footer: {last:?}"
                    );
                    assert_eq!(find_control(preview, "installer.next").get_rect(), footer.0);
                    assert_eq!(find_control(preview, "installer.back").get_rect(), footer.1);
                })
                .unwrap();
                capture(&mut app, "bottom");
                for private in ["/private/never-visible", "never-visible-value"] {
                    assert!(!app.harness().inspection_json().unwrap().contains(private));
                    assert!(!status(&mut app).to_string().contains(private));
                }
                for _ in 0..40 {
                    app.harness().scroll(0.0, 10.0);
                }
                app.settle();
                app.harness()
                    .click_named("installer.category.flatpak")
                    .unwrap();
                app.settle();
                assert_eq!(status(&mut app)["categories"][1], false);
                assert!(
                    !app.harness()
                        .inspection_json()
                        .unwrap()
                        .contains("installer.flatpak.settings.23")
                );
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(find_control(preview, "installer.next").get_rect(), footer.0);
                })
                .unwrap();
                capture(&mut app, "excluded");
            }
        }
    }

    #[test]
    fn belongings_review_uses_detected_profiles_without_exposing_credentials() {
        use super::super::account_transfer::{SourceRoots, discover};
        let source = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(
            "XDG_STATE_HOME",
            source.path().canonicalize().unwrap().join("state"),
        );
        let config = source.path().join(".config");
        std::fs::create_dir_all(config.join("discord")).unwrap();
        std::fs::write(config.join("discord/session"), "private-session-bytes").unwrap();
        let roots = SourceRoots {
            home: source.path().to_owned(),
            config,
        };
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.journey.details = true;
            preview.belongings = belongings::Belongings {
                home: Some(roots.home.clone()),
                accounts: discover(&roots),
                ..Default::default()
            };
            preview.dirty = true;
        })
        .unwrap();
        let review = status(&mut app);
        let candidates = review["belongings"]["accounts"]["candidates"]
            .as_array()
            .unwrap();
        assert!(candidates.iter().any(|item| item["provider"] == "discord"
            && item["needs_sign_in"] == true
            && item["can_import_credentials"] == false));
        assert!(!review.to_string().contains("private-session-bytes"));
        app.settle();
        let inspection = app.harness().inspection_json().unwrap();
        assert!(inspection.contains("installer.profile."));
        assert!(inspection.contains(&t("installer-preview-profiles-body")));
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "accounts".into(),
            included: false,
        })
        .unwrap();
        app.settle();
        let without_connections = app.harness().inspection_json().unwrap();
        assert!(
            without_connections.contains("installer.profile."),
            "skipping service connections doesn't exclude saved profile files"
        );
        for category in ["files", "flatpak", "accounts"] {
            app.effect(&InstallerPreviewEffect::SetPreviewCategory {
                category: category.into(),
                included: false,
            })
            .unwrap();
        }
        assert_eq!(status(&mut app)["selected_count"], 0);
        assert_eq!(
            std::fs::read_to_string(roots.config.join("discord/session")).unwrap(),
            "private-session-bytes"
        );
    }

    #[test]
    fn belongings_discovery_failures_identify_affected_choices_and_keep_recheck_action() {
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.page = Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.belongings.home_unavailable = true;
            preview.belongings.browser_unavailable = true;
            preview.belongings.accounts.unreadable_sources = vec!["Nextcloud"];
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        let view = app.harness().inspection_json().unwrap();
        for key in [
            "installer-belongings-files-unchecked",
            "installer-belongings-browser-unchecked",
            "installer-belongings-accounts-unchecked",
        ] {
            assert!(view.contains(&t(key)), "missing affected category {key}");
        }
        assert!(!view.contains(&t("installer-belongings-apps-unchecked")));
        assert!(view.contains("installer.belongings.recheck"));
        assert!(!view.contains(&t("installer-preview-belongings-incomplete")));
    }

    #[test]
    fn discovered_belongings_keep_unavailable_apps_and_foreign_logins_out_of_the_main_choices() {
        use super::super::account_transfer::{SourceRoots, discover};
        let source = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(
            "XDG_STATE_HOME",
            source.path().canonicalize().unwrap().join("state"),
        );
        let config = source.path().join(".config");
        std::fs::create_dir_all(config.join("discord")).unwrap();
        std::fs::write(config.join("discord/session"), "original-login").unwrap();
        let roots = SourceRoots {
            home: source.path().to_owned(),
            config,
        };
        let inventory = belongings::Belongings {
            home: Some(roots.home.clone()),
            roots: Some(roots.clone()),
            accounts: discover(&roots),
            flatpak_unavailable: true,
            ..Default::default()
        };
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.page = Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.belongings_worker = Some(receiver);
            preview.dirty = true;
        })
        .unwrap();
        sender.send(inventory).unwrap();
        app.settle();
        let overview = app.harness().inspection_json().unwrap();
        assert!(
            !overview.contains("Discord"),
            "a preserved profile is not a connected account"
        );
        assert!(!overview.contains("installer.category.flatpak"));
        assert!(overview.contains(&t("installer-belongings-apps-unchecked")));
        assert!(!overview.contains(&t("installer-belongings-files-unchecked")));
        assert!(!overview.contains(&t("installer-belongings-accounts-unchecked")));
        for (tag, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for (viewport, scale) in [((480, 800), 1.0), ((1180, 820), 1.0), ((2560, 1440), 2.0)] {
                app.harness().resize(viewport).unwrap();
                app.harness()
                    .set_theme(theme.clone().with_ui_scale(scale).with_reduce_motion(true));
                visual_tests::paint_app(
                    &mut app,
                    "installer-detected-belongings",
                    &[
                        "installer.category.files",
                        "installer.category.accounts",
                        "installer.details",
                        "installer.next",
                    ],
                    viewport,
                    tag,
                );
            }
        }
        app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
            .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::SetPreviewCategory {
                category: "flatpak".into(),
                included: true,
            })
            .is_err()
        );
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "files".into(),
            included: false,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "accounts");
        assert_eq!(
            std::fs::read_to_string(roots.config.join("discord/session")).unwrap(),
            "original-login"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn same_computer_permission_accepts_semantic_grant_and_persists_review_before_advancing() {
        if !prepared_media::native_retained::enter_owned_journey(
            "setup::journey::tests::same_computer_permission_accepts_semantic_grant_and_persists_review_before_advancing",
        ) {
            return;
        }
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        // Hold the real capture worker at its input boundary so refusal of
        // concurrent navigation is deterministic, without inventing its result.
        type Gate = (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);
        static GATE: std::sync::OnceLock<std::sync::Mutex<Option<Gate>>> =
            std::sync::OnceLock::new();
        fn gated_capture(
            home: &std::path::Path,
            operation: &str,
            categories: super::super::migration_handoff::Categories,
        ) -> std::io::Result<super::super::migration_handoff::Plan> {
            let (started, release) = GATE.get().unwrap().lock().unwrap().take().unwrap();
            started.send(()).map_err(std::io::Error::other)?;
            release
                .recv_timeout(tilo_test_env::hang_bound())
                .map_err(std::io::Error::other)?;
            // Discovery and registration must agree on the actual owned GPT.
            super::super::migration_handoff::capture(home, operation, categories)
        }
        let (started, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        *GATE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .unwrap() = Some((started, wait));
        let root = prepared_media::native_retained::journey_directory();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("original"), b"untouched").unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("XDG_STATE_HOME", root.path().join("state"));
        let (reply, receiver) = std::sync::mpsc::channel();
        environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
        environment.set(super::super::internal_install_flow::RUNTIME_GATE_VAR, "1");
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "migrate");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Source consent person".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
            .unwrap();
        password(&mut app);
        wait_for_personal(&mut app);
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "purpose");
        choose(&mut app, "computer");
        assert_eq!(status(&mut app)["purpose"], "computer");
        assert_eq!(status(&mut app)["page"], "belongings");
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, false, false];
            preview.belongings.home = Some(home.clone());
            preview.migration_selection.permission_reply = Some(receiver);
            preview.migration_selection.capture = Some(gated_capture);
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.migration_selection.choosing());
            assert_eq!(preview.journey.page, Page::Belongings);
            assert!(preview.migration_selection.reviewed_intent().is_none());
        })
        .unwrap();
        assert!(!root.path().join("state/tilo/installer/migrations").exists());
        app.effect(&InstallerPreviewEffect::SelectMigrationHome {
            source: home.to_string_lossy().into_owned(),
        })
        .unwrap();
        assert!(
            reply.send(ui::PortalOutcome::Opened(home.clone())).is_err(),
            "semantic completion must retire the pending platform reply"
        );
        ready.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::BackInPreview).is_err(),
            "Back must remain unavailable while the actual review worker runs"
        );
        assert!(
            app.effect(&InstallerPreviewEffect::SelectMigrationHome {
                source: home.to_string_lossy().into_owned(),
            })
            .is_err(),
            "a second grant cannot replace an active review worker"
        );
        release.send(()).unwrap();
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let snapshot = status(&mut app);
            if snapshot["migration_preparation"]["prepared"] == true
                && snapshot["belongings_preparation"]["prepared"] == true
            {
                assert_eq!(snapshot["page"], "storage");
                assert_eq!(snapshot["belongings_preparation"]["pending"], false);
                break;
            }
            assert_ne!(
                snapshot["media_preparation"]["state"], "failed",
                "The owned GPT consent must prepare through the actual standalone root helper. \
                 The worker cause is printed above; no substitute receipt is admitted. {snapshot}"
            );
            assert!(std::time::Instant::now() < deadline, "{snapshot}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.migration_selection.choosing());
            prepared_media::native_retained::assert_journey_registration(preview);
            let intent = preview.migration_selection.reviewed_intent().unwrap();
            let plan = intent.source_plan();
            let metadata = std::fs::metadata(&home).unwrap();
            assert_eq!(
                plan.home_policy,
                super::super::migration_handoff::HomePolicy::AccessReadWrite
            );
            let persisted =
                std::fs::read(intent.directory().join("migration-intent.json")).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&persisted).unwrap();
            assert_eq!(value["plan"]["home"]["inode"], metadata.ino());
            assert_eq!(value["plan"]["home"]["original_uid"], metadata.uid());
            assert_eq!(value["plan"]["operation_id"], plan.operation_id);
            assert!(uuid::Uuid::parse_str(&plan.operation_id).is_ok());
        })
        .unwrap();
        assert_eq!(std::fs::read(home.join("original")).unwrap(), b"untouched");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn same_computer_permission_back_retires_queued_grant_and_retains_identity_accounts() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("original"), b"untouched").unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
        environment.set("XDG_STATE_HOME", root.path().join("state"));
        environment.set("XDG_CONFIG_HOME", root.path().join("config"));
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Password;
            preview.journey.identity = Some(Identity::New);
            preview.journey.name = "Retained identity".into();
            preview.journey.password = "permission fixture password".into();
            preview.journey.confirmation = preview.journey.password.clone();
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        wait_for_personal(&mut app);
        let identity = app
            .with_widget::<InstallerPreview, _>(|preview| {
                preview.personal.prepared_handle().unwrap()
            })
            .unwrap();
        let accounts = accounts::checkpoint_fixture_for_journey(&root.path().join("accounts"));
        let account_ids = accounts.prepared_handle().unwrap().selected_ids();
        let (reply, receiver) = std::sync::mpsc::channel();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.page = Page::Belongings;
            preview.journey.history = vec![Page::Devices];
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, false, false];
            preview.accounts = accounts;
            preview.belongings.home = Some(home.clone());
            preview.migration_selection.permission_reply = Some(receiver);
            preview.migration_selection.capture =
                Some(super::super::migration_handoff::capture_fixture);
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.migration_selection.choosing());
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert!(reply.send(ui::PortalOutcome::Opened(home.clone())).is_err());
        assert_eq!(status(&mut app)["page"], "devices");
        let (reply, receiver) = std::sync::mpsc::channel();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(std::sync::Arc::ptr_eq(
                &identity,
                &preview.personal.prepared_handle().unwrap()
            ));
            assert_eq!(
                preview.accounts.prepared_handle().unwrap().selected_ids(),
                account_ids
            );
            preview.journey.page = Page::Belongings;
            preview.journey.history = vec![Page::Devices];
            preview.migration_selection.permission_reply = Some(receiver);
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.migration_selection.choosing());
            // Queue a valid grant immediately before Back; navigation must
            // retire it before a subsequent drain can create a review worker.
            reply.send(ui::PortalOutcome::Opened(home.clone())).unwrap();
            preview
                .apply(InstallerPreviewEffect::BackInPreview)
                .unwrap();
            assert_eq!(preview.journey.page, Page::Devices);
            assert!(!preview.migration_selection.busy());
            assert!(preview.migration_selection.reviewed_intent().is_none());
            assert!(std::sync::Arc::ptr_eq(
                &identity,
                &preview.personal.prepared_handle().unwrap()
            ));
            assert_eq!(
                preview.accounts.prepared_handle().unwrap().selected_ids(),
                account_ids
            );
        })
        .unwrap();
        assert!(reply.send(ui::PortalOutcome::Opened(home.clone())).is_err());
        app.settle();
        assert_eq!(status(&mut app)["page"], "devices");
        assert!(
            app.effect(&InstallerPreviewEffect::SelectMigrationHome {
                source: home.to_string_lossy().into_owned(),
            })
            .is_err()
        );
        assert!(!root.path().join("state/tilo/installer/migrations").exists());
        assert_eq!(std::fs::read(home.join("original")).unwrap(), b"untouched");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn another_computer_belongings_requires_actual_home_consent_before_advancing() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("original"), b"untouched").unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        env.set("XDG_STATE_HOME", root.path().join("state"));
        let mut app = fixture();
        choose(&mut app, "another");
        choose(&mut app, "migrate");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Portable consent person".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
            .unwrap();
        password(&mut app);
        wait_for_personal(&mut app);
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "belongings");
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.destination = Some(Destination::Another);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, false, false];
            preview.belongings.home = Some(home.clone());
        })
        .unwrap();
        assert_eq!(status(&mut app)["portable_preparation"]["prepared"], false);
        app.effect(&InstallerPreviewEffect::SelectMigrationHome {
            source: home.to_string_lossy().into_owned(),
        })
        .unwrap();
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            let snapshot = status(&mut app);
            if snapshot["portable_preparation"]["prepared"] == true {
                assert_eq!(snapshot["page"], "review");
                assert_eq!(snapshot["migration_preparation"]["prepared"], false);
                app.with_widget::<InstallerPreview, _>(|preview| {
                    let source = preview.portable_source.reviewed_handle().unwrap();
                    source.validate().unwrap();
                    if source.original_only() {
                        assert_eq!(
                            snapshot["portable_preparation"]["capture"],
                            "awaiting-original-boot"
                        );
                        assert!(snapshot["portable_preparation"]["bytes"].is_null());
                        assert!(snapshot["portable_preparation"]["files"].is_null());
                        assert!(preview.portable_source.reviewed_original().is_some());
                        assert!(!source.immutable_capture());
                        assert!(source.summary().is_err());
                        assert!(source.sealed_reservation().is_err());
                    } else {
                        assert!(preview.portable_source.reviewed_original().is_none());
                        assert_eq!(snapshot["portable_preparation"]["bytes"], 9);
                        assert_eq!(snapshot["portable_preparation"]["files"], 1);
                        assert_eq!(source.summary().unwrap().bytes, 9);
                        assert_eq!(
                            snapshot["portable_preparation"]["capture"],
                            if source.immutable_capture() {
                                "ready"
                            } else {
                                "live"
                            }
                        );
                    }
                    assert!(!preview.media_preparation.busy());
                })
                .unwrap();
                break;
            }
            assert!(std::time::Instant::now() < deadline, "{snapshot}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(std::fs::read(home.join("original")).unwrap(), b"untouched");
        assert_eq!(std::fs::read_dir(&home).unwrap().count(), 1);
        let mut children = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        children.sort();
        assert_eq!(
            children,
            [
                std::ffi::OsString::from("home"),
                std::ffi::OsString::from("state")
            ]
        );
        // A matching semantic grant is retained custody, not permission for a
        // replacement directory at the same spelling or a completed archive.
        let original = root.path().join("original-home");
        std::fs::rename(&home, &original).unwrap();
        std::fs::create_dir(&home).unwrap();
        std::fs::write(home.join("original"), b"replacement").unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(
                preview
                    .portable_source
                    .reviewed_handle()
                    .unwrap()
                    .validate()
                    .is_err()
            );
            if let Some(grant) = preview.portable_source.reviewed_original() {
                assert!(
                    grant
                        .capture_copy_intent(&std::sync::atomic::AtomicBool::new(false),)
                        .is_err()
                );
            }
            assert!(!preview.media_preparation.busy());
        })
        .unwrap();
        assert_eq!(
            std::fs::read(original.join("original")).unwrap(),
            b"untouched"
        );
        assert_eq!(
            std::fs::read(home.join("original")).unwrap(),
            b"replacement"
        );
        let mut children = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        children.sort();
        assert_eq!(
            children,
            [
                std::ffi::OsString::from("home"),
                std::ffi::OsString::from("original-home"),
                std::ffi::OsString::from("state"),
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn belongings_grant_refuses_unreviewed_home_and_keeps_the_user_on_review() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let wrong = root.path().join("wrong");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&wrong).unwrap();
        std::fs::write(home.join("original"), b"untouched").unwrap();
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, false, false];
            preview.belongings.home = Some(home.clone());
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SelectMigrationHome {
            source: wrong.to_string_lossy().into_owned(),
        })
        .unwrap();
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let snapshot = status(&mut app);
            if snapshot["migration_preparation"]["state"] == "failed" {
                assert_eq!(snapshot["page"], "belongings");
                assert_eq!(snapshot["migration_preparation"]["prepared"], false);
                break;
            }
            assert!(std::time::Instant::now() < deadline, "{snapshot}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(std::fs::read(home.join("original")).unwrap(), b"untouched");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn user_transfer_actions_review_then_copy_real_home_without_launching_apps() {
        use std::os::unix::{ffi::OsStringExt as _, fs::PermissionsExt};
        let volume = tempfile::tempdir().unwrap();
        let source = volume.path().join("source");
        let destination = volume.path().join("target");
        let journals = volume.path().join("receipts");
        std::fs::create_dir_all(source.join(".config/example")).unwrap();
        std::fs::create_dir(&journals).unwrap();
        std::fs::set_permissions(&journals, std::fs::Permissions::from_mode(0o700)).unwrap();
        let original = source.join(".config/example/settings");
        std::fs::write(&original, "real-settings").unwrap();
        let non_utf8 = std::ffi::OsString::from_vec(vec![b'd', 0xff]);
        std::fs::write(source.join(&non_utf8), b"non-UTF8 named file").unwrap();
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, false, false];
            preview.journey.details = true;
            preview.belongings.home = Some(source.clone());
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::StartUserTransfer)
                .is_err()
        );
        app.effect(&InstallerPreviewEffect::ReviewUserTransfer {
            source: source.to_string_lossy().into_owned(),
            destination: destination.to_string_lossy().into_owned(),
            journals: journals.to_string_lossy().into_owned(),
        })
        .unwrap();
        let wait = |app: &mut TestApp<ImagerShell>, expected: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                let state = status(app);
                if state["user_transfer"]["state"] == expected {
                    return state;
                }
                assert!(std::time::Instant::now() < deadline, "{state}");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        let reviewed = wait(&mut app, "reviewed");
        assert_eq!(reviewed["user_transfer"]["result"]["summary"]["files"], 2);
        assert!(!destination.exists());
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "files".into(),
            included: false,
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::StartUserTransfer)
                .is_err()
        );
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "files".into(),
            included: true,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::StartUserTransfer)
            .unwrap();
        wait(&mut app, "complete");
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "real-settings");
        assert_eq!(
            std::fs::read_to_string(destination.join(".config/example/settings")).unwrap(),
            "real-settings"
        );
        // A fresh resource instance represents a restarted installer: normal
        // Review and Start renew grants and revalidate canonical receipts.
        app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.journey.categories = [true, false, false];
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ReviewUserTransfer {
            source: source.to_string_lossy().into_owned(),
            destination: destination.to_string_lossy().into_owned(),
            journals: journals.to_string_lossy().into_owned(),
        })
        .unwrap();
        let recovered = wait(&mut app, "reviewed");
        assert_eq!(recovered["user_transfer"]["result"]["recovered"], true);
        app.effect(&InstallerPreviewEffect::StartUserTransfer)
            .unwrap();
        wait(&mut app, "complete");
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "real-settings");
        let active = volume.path().join("active");
        for custody in [".config/tilo", ".local/state/tilo"] {
            std::fs::create_dir_all(active.join(custody)).unwrap();
            std::fs::set_permissions(active.join(custody), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        std::fs::set_permissions(&active, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(active.join(".config/tilo/selected"), b"selected identity").unwrap();
        let wait_adoption = |app: &mut TestApp<ImagerShell>, expected: &str| {
            let deadline = tilo_test_env::hang_deadline();
            loop {
                let state = status(app);
                if state["home_promotion"]["state"] == expected {
                    return state;
                }
                assert!(std::time::Instant::now() < deadline, "{state}");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        assert!(
            app.effect(&InstallerPreviewEffect::StartHomePromotion)
                .is_err()
        );
        for (target, journal) in [
            (source.clone(), journals.join("adoption")),
            (source.join("child"), journals.join("adoption")),
            (volume.path().to_path_buf(), journals.join("adoption")),
            (active.clone(), source.join("adoption")),
        ] {
            app.effect(&InstallerPreviewEffect::ReviewHomePromotion {
                target_home: target.to_string_lossy().into_owned(),
                journal: journal.to_string_lossy().into_owned(),
            })
            .unwrap();
            wait_adoption(&mut app, "failed");
            assert_eq!(std::fs::read_to_string(&original).unwrap(), "real-settings");
            assert!(!source.join("adoption").exists());
        }
        app.effect(&InstallerPreviewEffect::ReviewHomePromotion {
            target_home: active.to_string_lossy().into_owned(),
            journal: journals.join("adoption").to_string_lossy().into_owned(),
        })
        .unwrap();
        let adoption_review = wait_adoption(&mut app, "reviewed");
        assert!(
            adoption_review["home_promotion"]["result"]["will_install"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["native"]
                    == serde_json::to_value(super::super::migration_store::NativePath::encode(
                        std::path::Path::new(&non_utf8)
                    ))
                    .unwrap())
        );
        // The public semantic surface exposes exact retained results without
        // materializing all imported paths into the resource document.
        let page = app
            .effect(&InstallerPreviewEffect::ReadHomePromotionResults {
                category: "will_install".into(),
                offset: 0,
                limit: 16,
            })
            .unwrap()
            .value
            .unwrap();
        assert_eq!(page["phase"], "reviewed");
        assert_eq!(page["verified"], false);
        assert_eq!(
            page["total"],
            adoption_review["home_promotion"]["result"]["will_install_count"]
        );
        assert!(page["items"].as_array().unwrap().iter().any(|item| {
            item["native"]
                == serde_json::to_value(super::super::migration_store::NativePath::encode(
                    std::path::Path::new(&non_utf8),
                ))
                .unwrap()
        }));
        for (category, offset, limit) in [
            ("installed", 0, 16),
            ("will_install", 0, 17),
            ("will_install", u64::MAX, 1),
        ] {
            assert!(
                app.effect(&InstallerPreviewEffect::ReadHomePromotionResults {
                    category: category.into(),
                    offset,
                    limit
                })
                .is_err()
            );
        }
        assert!(!active.join(".config/example/settings").exists());
        app.effect(&InstallerPreviewEffect::StartHomePromotion)
            .unwrap();
        wait_adoption(&mut app, "complete");
        let installed = app
            .effect(&InstallerPreviewEffect::ReadHomePromotionResults {
                category: "installed".into(),
                offset: 0,
                limit: 16,
            })
            .unwrap()
            .value
            .unwrap();
        assert_eq!(installed["phase"], "finished");
        assert_eq!(installed["verified"], true);
        assert_eq!(installed["items"], page["items"]);
        assert!(
            app.effect(&InstallerPreviewEffect::ReadHomePromotionResults {
                category: "will_install".into(),
                offset: 0,
                limit: 16
            })
            .is_err()
        );
        assert_eq!(
            std::fs::read(active.join(&non_utf8)).unwrap(),
            b"non-UTF8 named file"
        );
        assert_eq!(
            std::fs::read(source.join(&non_utf8)).unwrap(),
            b"non-UTF8 named file"
        );
        assert_eq!(
            std::fs::read_to_string(active.join(".config/example/settings")).unwrap(),
            "real-settings"
        );
        assert_eq!(
            std::fs::read(active.join(".config/tilo/selected")).unwrap(),
            b"selected identity"
        );
        assert_eq!(std::fs::read_to_string(&original).unwrap(), "real-settings");
        app.effect(&InstallerPreviewEffect::RetryHomePromotion)
            .unwrap();
        wait_adoption(&mut app, "complete");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn unsupported_usb_backend_has_no_preparation_control_or_false_promise() {
        let mut app = fixture();
        for destination in [Destination::Computer, Destination::Another] {
            app.with_widget::<InstallerPreview, _>(|preview| {
                preview.live = true;
                preview.journey.page = Page::Review;
                preview.journey.destination = Some(destination);
                preview.media = Some(ui::LocalState::new(false));
                preview.dirty = true;
                assert!(!preview.usb_backend_available());
            })
            .unwrap();
            app.settle();
            let inspection = app.harness().inspection_json().unwrap();
            assert!(inspection.contains(&t("installer-review-installation-unavailable")));
            assert!(!inspection.contains(&t("installer-review-usb-fallback")));
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert!(visual_tests::control(preview, "installer.prepare-usb").is_none());
            })
            .unwrap();
            assert!(app.effect(&InstallerPreviewEffect::PrepareUsb).is_err());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn belongings_preparation_and_export_use_real_owned_work_without_install_admission() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        env.set("XDG_STATE_HOME", root.path().join("state"));
        env.set(super::super::internal_install_flow::RUNTIME_GATE_VAR, "0");
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        env.set("HOME", &home);
        env.set("XDG_CONFIG_HOME", root.path().join("config"));
        env.set("XDG_DATA_HOME", root.path().join("data"));
        env.set("XDG_CACHE_HOME", root.path().join("cache"));
        let mut app = TestApp::<ImagerShell>::launched(&["--installer"]).unwrap();
        let startup_deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            let observed = status(&mut app);
            if observed["restart"]["state"] == "missing" {
                break;
            }
            assert!(std::time::Instant::now() < startup_deadline, "{observed}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        choose(&mut app, "computer");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Prepared person".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        password(&mut app);
        let identity = wait_for_personal(&mut app)["prepared_identity"]["public_key"].clone();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Devices);
            assert!(preview.storage.facts.is_none());
            assert!(preview.storage.plan.is_none());
            assert!(!preview.direct_available());
        })
        .unwrap();
        // The actual ordinary Continue queues preparation and stays on this
        // page until the real Controller worker has returned and been joined.
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        let immediate = status(&mut app);
        if immediate["belongings_preparation"]["pending"] == true {
            assert_eq!(immediate["page"], "devices");
            assert_eq!(immediate["belongings_preparation"]["prepared"], false);
        } else {
            // A fast real worker may finish before the next semantic read.
            assert_eq!(immediate["belongings_preparation"]["prepared"], true);
            assert_eq!(immediate["page"], "review");
        }
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        loop {
            let observed = status(&mut app);
            if observed["belongings_preparation"]["prepared"] == true {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "{observed}");
            assert_ne!(
                observed["media_preparation"]["state"], "failed",
                "{observed}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.page, Page::Review);
            assert!(!preview.direct.active);
            assert!(preview.direct.pending.is_none());
            // These are the actual outer media handoff bindings; composed
            // is only the ordinary page's render cache.
            assert!(!preview.media.as_ref().unwrap().get());
            assert!(preview.media_bundle.as_ref().unwrap().get().is_none());
            assert!(!preview.direct.navigation.get());
            assert!(!preview.desktop_pending);
            assert_eq!(
                preview.personal.prepared_handle().unwrap().public_key(),
                preview
                    .direct
                    .prepared_belongings
                    .as_ref()
                    .unwrap()
                    .desktop_manifest()
                    .unwrap()
                    .identity_public_key
            );
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::PrepareDirectInstallation)
                .is_err()
        );
        let package = root.path().join("belongings.sealed");
        app.effect(&InstallerPreviewEffect::ExportPreparedBelongings {
            destination: package.to_string_lossy().into_owned(),
        })
        .unwrap();
        loop {
            let observed = status(&mut app);
            if observed["media_preparation"]["exported"].is_object() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "{observed}");
            assert_ne!(
                observed["media_preparation"]["state"], "failed",
                "{observed}"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let authenticated = crate::setup::prepared_setup::PreparedSetupReview::open(
            &package,
            b"preview-secret-only",
            &root.path().join("checked"),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            serde_json::Value::String(hex::encode(authenticated.manifest().identity_public_key)),
            identity
        );
        // The normal capacity action cannot manufacture an Apps declaration
        // from an identity-only preparation or start a delegated installer.
        assert!(
            app.effect(&InstallerPreviewEffect::ReviewPreparedBelongings { target_gib: 35 })
                .is_err()
        );
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.direct.active);
            assert!(preview.direct.pending.is_none());
            assert!(!preview.media_preparation.busy());
        })
        .unwrap();
        assert!(authenticated.manifest().offline_home.is_none());
        assert!(authenticated.manifest().original_copy.is_none());
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "destination".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "another".into(),
        })
        .unwrap();
        assert_eq!(
            status(&mut app)["belongings_preparation"]["prepared"],
            false
        );
        assert_eq!(
            status(&mut app)["prepared_identity"]["public_key"],
            identity
        );
        assert!(
            app.effect(&InstallerPreviewEffect::ExportPreparedBelongings {
                destination: root
                    .path()
                    .join("stale.sealed")
                    .to_string_lossy()
                    .into_owned(),
            })
            .is_err()
        );
        assert!(!root.path().join("stale.sealed").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn gated_computer_route_reviews_explicit_usb_choice_without_empty_storage() {
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        env.set(super::super::internal_install_flow::RUNTIME_GATE_VAR, "0");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        env.set("XDG_STATE_HOME", root.join("state"));
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            assert!(
                !preview.direct_route_enabled(),
                "this qualification requires a closed release gate"
            );
        })
        .unwrap();
        password(&mut app);
        let prepared = wait_for_personal(&mut app);
        let key = prepared["prepared_identity"]["public_key"].clone();
        assert_eq!(prepared["page"], "devices");
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(wait_for_belongings(&mut app)["page"], "review");
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.journey.history.contains(&Page::Storage));
            assert!(visual_tests::control(preview, "installer.review.usb-fallback").is_some());
            assert!(visual_tests::control(preview, "installer.prepare-usb").is_some());
            assert!(visual_tests::control(preview, "installer.prepare-direct").is_none());
            assert!(visual_tests::control(preview, "installer.next").is_none());
            assert!(!preview.media_preparation.busy());
        })
        .unwrap();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains(&t("installer-review-usb-fallback"))
        );
        // Generic Continue must not silently choose a different installation route.
        let refused = app
            .effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap_err();
        assert!(
            refused
                .to_string()
                .contains(&t("installer-review-usb-choice-required"))
        );
        assert_eq!(status(&mut app)["page"], "review");
        assert_eq!(status(&mut app)["destination"], "computer");
        assert_eq!(status(&mut app)["prepared_identity"]["public_key"], key);
        let retained = status(&mut app);
        assert_eq!(retained["media_preparation"]["state"], "prepared");
        assert_eq!(retained["belongings_preparation"]["prepared"], true);
        assert!(retained["media_preparation"]["exported"].is_null());
        assert_eq!(retained["direct_install"]["active"], false);
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(status(&mut app)["page"], "devices");
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "review");
        for destination in ["another", "computer"] {
            app.effect(&InstallerPreviewEffect::EditPreview {
                section: "destination".into(),
            })
            .unwrap();
            choose(&mut app, destination);
            assert_eq!(status(&mut app)["page"], "review");
            assert_eq!(status(&mut app)["destination"], destination);
            assert_eq!(status(&mut app)["prepared_identity"]["public_key"], key);
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert!(!preview.journey.history.contains(&Page::Storage));
            })
            .unwrap();
        }
        let bindings = app
            .with_widget::<InstallerPreview, _>(|preview| {
                (
                    preview.media.clone(),
                    preview.media_bundle.clone(),
                    preview.dispatcher.clone(),
                )
            })
            .unwrap();
        for missing in ["media", "bundle", "dispatcher"] {
            app.with_widget::<InstallerPreview, _>(|preview| {
                preview.media = bindings.0.clone();
                preview.media_bundle = bindings.1.clone();
                preview.dispatcher = bindings.2.clone();
                match missing {
                    "media" => preview.media = None,
                    "bundle" => preview.media_bundle = None,
                    "dispatcher" => preview.dispatcher = None,
                    _ => unreachable!(),
                }
                preview.dirty = true;
            })
            .unwrap();
            app.settle();
            assert!(
                app.harness()
                    .inspection_json()
                    .unwrap()
                    .contains(&t("installer-review-installation-unavailable"))
            );
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert!(!preview.usb_backend_available());
                assert!(visual_tests::control(preview, "installer.prepare-usb").is_none());
                assert!(!preview.media_preparation.busy());
            })
            .unwrap();
            assert!(app.effect(&InstallerPreviewEffect::PrepareUsb).is_err());
            let refused = app
                .effect(&InstallerPreviewEffect::ContinuePreview)
                .unwrap_err();
            assert!(
                refused
                    .to_string()
                    .contains(&t("installer-review-installation-unavailable"))
            );
            assert_eq!(status(&mut app)["prepared_identity"]["public_key"], key);
            assert_eq!(status(&mut app)["media_preparation"]["state"], "idle");
            app.effect(&InstallerPreviewEffect::EditPreview {
                section: "destination".into(),
            })
            .unwrap();
            choose(&mut app, "another");
            assert_eq!(status(&mut app)["page"], "review");
            app.settle();
            let inspection = app.harness().inspection_json().unwrap();
            assert!(inspection.contains(&t("installer-review-installation-unavailable")));
            assert!(!inspection.contains(&t("installer-review-usb-fallback")));
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert!(visual_tests::control(preview, "installer.prepare-usb").is_none());
            })
            .unwrap();
            assert!(app.effect(&InstallerPreviewEffect::PrepareUsb).is_err());
            assert_eq!(status(&mut app)["prepared_identity"]["public_key"], key);
            assert_eq!(status(&mut app)["media_preparation"]["state"], "idle");
            app.effect(&InstallerPreviewEffect::EditPreview {
                section: "destination".into(),
            })
            .unwrap();
            choose(&mut app, "computer");
            assert_eq!(status(&mut app)["page"], "review");
        }
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.media = bindings.0;
            preview.media_bundle = bindings.1;
            preview.dispatcher = bindings.2;
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "destination".into(),
        })
        .unwrap();
        choose(&mut app, "another");
        app.settle();
        let inspection = app.harness().inspection_json().unwrap();
        assert!(!inspection.contains(&t("installer-review-installation-unavailable")));
        assert!(!inspection.contains(&t("installer-review-usb-fallback")));
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.usb_backend_available());
            assert!(visual_tests::control(preview, "installer.prepare-usb").is_some());
            assert!(!preview.media_preparation.busy());
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn review_profile_save_and_cancel_preserve_custody_and_exact_cached_photo() {
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        env.set("XDG_STATE_HOME", root.join("state"));
        let original_path = root.join("original.png");
        std::fs::write(&original_path, include!("../fixtures/avatar.rs")).unwrap();
        let changed_path = root.join("changed.png");
        // A distinct, valid one-pixel synthetic PNG exercises the actual decoder.
        std::fs::write(
            &changed_path,
            [
                137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0,
                1, 8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 144,
                140, 186, 244, 31, 0, 4, 26, 2, 69, 204, 253, 167, 130, 0, 0, 0, 0, 73, 69, 78, 68,
                174, 66, 96, 130,
            ],
        )
        .unwrap();
        let original_photo =
            runtime_identity::ProfilePhoto::new(1, 1, vec![200, 100, 50, 255]).unwrap();
        let changed_photo =
            runtime_identity::ProfilePhoto::new(1, 1, vec![25, 90, 210, 255]).unwrap();
        let mut app = fixture();
        choose(&mut app, "another");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SelectPreviewAvatar {
            source_file: original_path.to_string_lossy().into_owned(),
        })
        .unwrap();
        assert_eq!(
            wait_for_personal(&mut app)["prepared_identity"]["state"],
            "photo-selected"
        );
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
            .unwrap();
        password(&mut app);
        assert_eq!(wait_for_personal(&mut app)["page"], "devices");
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "purpose");
        choose(&mut app, "computer");
        assert_eq!(status(&mut app)["page"], "review");
        app.settle();
        let identity = app
            .with_widget::<InstallerPreview, _>(|preview| {
                assert!(visual_tests::control(preview, "installer.review.profile").is_some());
                preview.personal.prepared_handle().unwrap()
            })
            .unwrap();
        let record = identity.custody_directory().join("primary.json");
        let original_bytes = std::fs::read(&record).unwrap();
        let key = identity.public_key();
        assert_eq!(
            identity.metadata().unwrap().profile_photo,
            Some(original_photo.clone())
        );
        // Cancel must restore already normalized pixels without revisiting this source.
        std::fs::remove_file(&original_path).unwrap();
        for save in [false, true] {
            app.effect(&InstallerPreviewEffect::EditPreview {
                section: "profile".into(),
            })
            .unwrap();
            app.effect(&InstallerPreviewEffect::SetPreviewName {
                value: "Grace".into(),
            })
            .unwrap();
            app.effect(&InstallerPreviewEffect::SelectPreviewAvatar {
                source_file: changed_path.to_string_lossy().into_owned(),
            })
            .unwrap();
            assert_eq!(
                wait_for_personal(&mut app)["prepared_identity"]["state"],
                "prepared"
            );
            app.with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(preview.personal.photo(), Some(&changed_photo));
            })
            .unwrap();
            assert_eq!(std::fs::read(&record).unwrap(), original_bytes);
            if save {
                // Invoke the same production effect before polling completion.
                app.with_widget::<InstallerPreview, _>(|preview| {
                    preview
                        .apply(InstallerPreviewEffect::ContinuePreview)
                        .unwrap();
                    assert_eq!(preview.journey.page, Page::Profile);
                    assert!(preview.personal.busy());
                    assert!(
                        preview
                            .apply(InstallerPreviewEffect::BackInPreview)
                            .is_err()
                    );
                })
                .unwrap();
                assert_eq!(wait_for_personal(&mut app)["page"], "review");
            } else {
                app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
                assert_eq!(status(&mut app)["page"], "review");
                assert_eq!(std::fs::read(&record).unwrap(), original_bytes);
                app.with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(preview.avatar.as_deref(), original_path.to_str());
                    assert_eq!(preview.personal.photo(), Some(&original_photo));
                    assert!(preview.personal.ready());
                })
                .unwrap();
            }
            let metadata = identity.metadata().unwrap();
            assert_eq!(metadata.group_public_key, key);
            assert_eq!(metadata.display_name, if save { "Grace" } else { "Ada" });
            assert_eq!(
                metadata.profile_photo,
                Some(if save {
                    changed_photo.clone()
                } else {
                    original_photo.clone()
                })
            );
            assert_eq!(status(&mut app)["name"], if save { "Grace" } else { "Ada" });
        }
        let durable_saved = std::fs::read(&record).unwrap();
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "profile".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Unacknowledged".into(),
        })
        .unwrap();
        // Unsafe canonical metadata makes the actual owned writer refuse. The
        // framework must retain Profile and its edit snapshot instead of Review.
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o644)).unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        let failed = wait_for_personal(&mut app);
        assert_eq!(failed["page"], "profile");
        assert_eq!(failed["prepared_identity"]["state"], "failed");
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(
                preview.error,
                Some("installer-preview-identity-prepare-failed")
            );
            assert!(preview.profile_avatar_original.is_some());
        })
        .unwrap();
        assert_eq!(std::fs::read(&record).unwrap(), durable_saved);
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(status(&mut app)["name"], "Grace");
        assert_eq!(identity.metadata().unwrap().display_name, "Grace");
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "profile".into(),
        })
        .unwrap();
        // A pending readonly decoder can be cancelled; no late result may replace
        // the saved cache or detach a durable identity writer.
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview
                .apply(InstallerPreviewEffect::SelectPreviewAvatar {
                    source_file: changed_path.to_string_lossy().into_owned(),
                })
                .unwrap();
            assert!(preview.personal.busy());
            assert!(Widget::is_enabled(&preview.button(
                "installer.back",
                "installer-preview-cancel",
                InstallerPreviewEffect::BackInPreview,
            )));
            preview
                .apply(InstallerPreviewEffect::BackInPreview)
                .unwrap();
            assert_eq!(preview.journey.page, Page::Review);
            assert!(!preview.personal.busy());
            assert_eq!(preview.personal.photo(), Some(&changed_photo));
        })
        .unwrap();
        app.settle();
        assert_eq!(std::fs::read(&record).unwrap(), durable_saved);
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "profile".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: String::new(),
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .is_err()
        );
        assert_eq!(status(&mut app)["page"], "profile");
        assert_eq!(std::fs::read(&record).unwrap(), durable_saved);
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(status(&mut app)["name"], "Grace");
    }

    #[cfg(unix)]
    #[test]
    fn review_device_edit_preserves_live_identity_and_commits_only_on_save() {
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        env.set("XDG_STATE_HOME", root.join("state"));
        let mut app = fixture();
        choose(&mut app, "another");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
            .unwrap();
        password(&mut app);
        let created = wait_for_personal(&mut app);
        assert_eq!(created["page"], "devices");
        let key = created["prepared_identity"]["public_key"].clone();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "review");
        let original_name = status(&mut app)["device_name"].clone();
        app.settle();
        let identity = app
            .with_widget::<InstallerPreview, _>(|preview| {
                assert!(visual_tests::control(preview, "installer.review.devices").is_some());
                preview.personal.prepared_handle().unwrap()
            })
            .unwrap();
        let identity_bytes =
            std::fs::read(identity.custody_directory().join("primary.json")).unwrap();
        let persisted = |app: &mut TestApp<ImagerShell>| {
            let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
            loop {
                app.settle();
                let expected = app
                    .with_widget::<InstallerPreview, _>(|preview| preview.journey.draft_choices())
                    .unwrap();
                if let draft::DraftStatus::Ready(ready) =
                    draft::DraftStore::for_session().unwrap().review().unwrap()
                    && ready.choices() == &expected
                {
                    return expected;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "device choice did not persist"
                );
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        let original_choices = persisted(&mut app);
        for save in [false, true] {
            app.effect(&InstallerPreviewEffect::EditPreview {
                section: "devices".into(),
            })
            .unwrap();
            assert_eq!(status(&mut app)["page"], "devices");
            app.effect(&InstallerPreviewEffect::SetPreviewDeviceName {
                value: "Ada's laptop".into(),
            })
            .unwrap();
            assert!(
                app.effect(&InstallerPreviewEffect::ExportPreparedSetup {
                    destination: root.join("tentative.sealed").to_string_lossy().into_owned(),
                    setup_binding: "tentative-device-label".into(),
                })
                .is_err()
            );
            assert!(!root.join("tentative.sealed").exists());
            let draft::DraftStatus::Ready(ready) =
                draft::DraftStore::for_session().unwrap().review().unwrap()
            else {
                panic!("tentative device edit lost the persisted identity");
            };
            assert_eq!(ready.choices(), &original_choices);
            app.effect(&if save {
                InstallerPreviewEffect::ContinuePreview
            } else {
                InstallerPreviewEffect::BackInPreview
            })
            .unwrap();
            let restored = status(&mut app);
            assert_eq!(restored["page"], "review");
            assert_eq!(restored["name"], "Ada");
            assert_eq!(restored["prepared_identity"]["public_key"], key);
            assert_eq!(
                restored["device_name"],
                if save {
                    serde_json::json!("Ada's laptop")
                } else {
                    original_name.clone()
                }
            );
            assert_eq!(
                std::fs::read(identity.custody_directory().join("primary.json")).unwrap(),
                identity_bytes
            );
            identity.metadata().unwrap();
            let committed = persisted(&mut app);
            assert_eq!(
                committed.device_label,
                if save {
                    "Ada's laptop"
                } else {
                    original_choices.device_label.as_str()
                }
            );
        }
    }

    #[test]
    fn review_migration_row_edits_early_choice_and_cancellation_preserves_person() {
        let mut app = fixture();
        choose(&mut app, "another");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        password(&mut app);
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        let original = status(&mut app);
        assert_eq!(original["page"], "review");
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(visual_tests::control(preview, "installer.review.migration").is_some());
        })
        .unwrap();
        // The row uses the already registered EditPreview action and existing
        // early Migration page; it cannot grant source access or bypass consent.
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "belongings".into(),
        })
        .unwrap();
        assert_eq!(status(&mut app)["page"], "migration");
        choose(&mut app, "migrate");
        assert_eq!(status(&mut app)["page"], "belongings");
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(status(&mut app)["page"], "migration");
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(status(&mut app), original);
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.password, "preview-secret-only");
            assert!(!preview.migration_busy());
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "belongings".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "review");
        assert_eq!(status(&mut app)["migrate"], false);
        assert_eq!(status(&mut app)["name"], "Ada");
    }

    #[test]
    fn new_identity_migration_is_driven_through_public_effects() {
        let mut app = fixture();
        assert!(
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .is_err()
        );
        choose(&mut app, "computer");
        choose(&mut app, "migrate");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "My name".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .is_err()
        );
        password(&mut app);
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "belongings");
        app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
            .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "accounts".into(),
            included: true,
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "accounts");
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "storage");
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        let review = status(&mut app);
        assert_eq!(review["page"], "review");
        assert_eq!(review["name"], "My name");
        app.settle();
        let rendered_review = app.harness().inspection_json().unwrap();
        assert!(rendered_review.contains("My name"));
        assert!(rendered_review.contains(&t("installer-review-setup")));
        assert!(rendered_review.contains(&t("installer-review-belongings-heading")));
        assert!(rendered_review.contains(&t("installer-review-person-body")));
        assert!(rendered_review.contains(review["device_name"].as_str().unwrap()));
        assert!(rendered_review.contains("installer.review.devices"));
        assert!(rendered_review.contains("installer.review.profile"));
        assert!(rendered_review.contains("installer.review.destination"));
        assert!(!rendered_review.contains("preview-secret-only"));
        assert_eq!(review["categories"][2], true);
        assert!(!review.to_string().contains("preview-secret-only"));
        assert!(
            !app.state().profile.user.username.contains("My name"),
            "preview choices never mutate the installation profile"
        );
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "ready");
    }

    #[test]
    fn existing_identity_can_migrate_and_fresh_skips_belongings() {
        for migrate in [true, false] {
            let mut app = fixture();
            choose(&mut app, "another");
            choose(&mut app, if migrate { "migrate" } else { "fresh" });
            choose(&mut app, "existing");
            assert_eq!(status(&mut app)["page"], "connect");
            choose(&mut app, "device");
            password(&mut app);
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .unwrap();
            assert_eq!(
                status(&mut app)["page"],
                if migrate { "belongings" } else { "storage" }
            );
            if migrate {
                app.effect(&InstallerPreviewEffect::ContinuePreview)
                    .unwrap();
            }
            app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
            assert_eq!(
                status(&mut app)["page"],
                if migrate { "belongings" } else { "devices" }
            );
            assert_eq!(status(&mut app)["identity"], "existing");
            assert_eq!(status(&mut app)["destination"], "another");
        }
    }

    fn find_control<'a>(root: &'a dyn Widget, id: &str) -> &'a dyn Widget {
        fn search<'a>(root: &'a dyn Widget, id: &str) -> Option<&'a dyn Widget> {
            if root.test_id() == Some(id) {
                return Some(root);
            }
            root.children()
                .into_iter()
                .find_map(|child| search(child, id))
        }
        search(root, id).expect("the production view exposes the field")
    }

    #[test]
    fn profile_vertical_composition_centers_avatar_and_bounds_form_across_scales() {
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "migrate");
        choose(&mut app, "new");
        for theme in [Theme::dark(), Theme::light()] {
            for (viewport, scale) in [
                ((320, 640), 1.0),
                ((480, 800), 1.0),
                ((1180, 820), 1.0),
                ((2560, 1440), 2.0),
                ((2560, 1600), 2.0),
                ((3840, 2160), 1.5),
                ((5120, 2822), 2.0),
            ] {
                app.harness()
                    .set_theme(theme.clone().with_ui_scale(scale).with_reduce_motion(true));
                app.harness().resize(viewport).unwrap();
                app.settle();
                app.with_widget::<InstallerPreview, _>(|preview| {
                    let heading = find_control(preview, "installer.heading").get_rect();
                    let avatar = find_control(preview, "installer.profile.avatar").get_rect();
                    let picture = find_control(preview, "installer.avatar").get_rect();
                    let name = find_control(preview, "installer.name").get_rect();
                    let back = find_control(preview, "installer.back").get_rect();
                    let next = find_control(preview, "installer.next").get_rect();
                    assert!(name.width() >= 180.0, "usable name field: {name:?}");
                    if viewport.0 >= 1180 {
                        let effective = f64::from(scale);
                        assert!(
                            (name.width()
                                - f64::from(theme.tokens.metrics.content_min_width) * effective)
                                .abs()
                                < 1.0,
                            "one gutter and a bounded personal form: {name:?}"
                        );
                    } else if viewport.0 >= 480 {
                        assert!(name.width() >= 360.0, "usable narrow form: {name:?}");
                    }
                    assert!(
                        avatar.width() >= 120.0 * f64::from(scale),
                        "prominent shared avatar: {avatar:?}"
                    );
                    let (scroll, overflowing) =
                        visual_tests::body_scroll_geometry(preview).unwrap();
                    for rect in [heading, avatar, picture, name, back, next] {
                        assert!(
                            rect.x0 >= preview.rect.x0 && rect.x1 <= preview.rect.x1,
                            "horizontal clipping: {rect:?}"
                        );
                        if !overflowing || rect == back || rect == next {
                            assert!(
                                rect.y0 >= preview.rect.y0 && rect.y1 <= preview.rect.y1,
                                "vertical clipping: {rect:?}"
                            );
                        }
                    }
                    assert!(scroll.y1 <= next.y0, "body never scrolls behind footer");
                    for rect in [heading, avatar, picture] {
                        assert!(
                            (rect.center().x - name.center().x).abs() < 2.0,
                            "centered profile stack: {rect:?} vs {name:?}"
                        );
                    }
                    assert!(avatar.y0 > heading.y1);
                    assert!(picture.y0 >= avatar.y1);
                    assert!(name.y0 >= picture.y1 + 12.0 * f64::from(scale));
                    assert!(
                        name.y1 <= scroll.y1 || overflowing,
                        "a name below the viewport must be reachable by scrolling"
                    );
                    assert!(back.x0 <= name.x0, "footer spans the personal form");
                    assert!(next.x1 >= name.x1, "footer spans the personal form");
                })
                .unwrap();
            }
        }
        // Large accessible text can scroll in a narrow viewport without
        // reverting to a detached horizontal picture/name arrangement.
        app.harness()
            .set_theme(Theme::dark().with_ui_scale(2.0).with_reduce_motion(true));
        app.harness().resize((480, 900)).unwrap();
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            let name = find_control(preview, "installer.name").get_rect();
            let picture = find_control(preview, "installer.avatar").get_rect();
            assert!(name.width() >= 180.0);
            assert!(name.x0 >= preview.rect.x0 && name.x1 <= preview.rect.x1);
            assert!(name.y0 >= picture.y1);
        })
        .unwrap();
    }

    #[test]
    fn password_fields_are_masked_aligned_and_resize_with_the_page() {
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "migrate");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        let sample = "sample-never-visible";
        app.effect(&InstallerPreviewEffect::SetPreviewPassword {
            value: sample.into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::SetPreviewConfirmation {
            value: sample.into(),
        })
        .unwrap();
        app.harness()
            .set_theme(Theme::dark().with_reduce_motion(true));
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            for id in ["installer.password", "installer.confirmation"] {
                let input = find_control(preview, id);
                let node = input.accessibility().unwrap();
                assert!(node.state.protected);
                assert!(node.value.is_none());
                let reveal = find_control(input, &format!("{id}_reveal"));
                assert_eq!(
                    reveal.accessibility().unwrap().name.as_deref(),
                    Some("Show password")
                );
                assert!(reveal.get_rect().width() > 0.0);
            }
        })
        .unwrap();
        app.harness()
            .click_named("installer.password_reveal")
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            let input = find_control(preview, "installer.password");
            assert!(input.accessibility().unwrap().value.is_none());
            assert_eq!(
                find_control(input, "installer.password_reveal")
                    .accessibility()
                    .unwrap()
                    .name
                    .as_deref(),
                Some("Hide password")
            );
        })
        .unwrap();
        assert!(!app.harness().inspection_json().unwrap().contains(sample));
        let mut control_height = None;
        for viewport in [(1180, 820), (5120, 2822), (3840, 2160), (480, 720)] {
            app.harness().resize(viewport).unwrap();
            app.settle();
            app.with_widget::<InstallerPreview, _>(|preview| {
                let first = find_control(preview, "installer.password").get_rect();
                if let Some(height) = control_height {
                    assert_eq!(first.height(), height, "resize must not magnify controls");
                } else {
                    control_height = Some(first.height());
                }
                let second = find_control(preview, "installer.confirmation").get_rect();
                assert!(
                    (first.x0 - second.x0).abs() < 1.0,
                    "fields have one leading edge"
                );
                assert!(
                    (first.x1 - second.x1).abs() < 1.0,
                    "fields have one trailing edge"
                );
                assert!(first.width() > 0.0);
                assert!(first.x0 >= preview.rect.x0 && first.x1 <= preview.rect.x1);
            })
            .unwrap();
        }
        app.harness().set_theme(Theme::dark().with_ui_scale(2.0));
        app.harness().resize((1732, 906)).unwrap();
        app.settle();
        app.with_widget::<InstallerPreview, _>(|preview| {
            let field = find_control(preview, "installer.password").get_rect();
            assert_eq!(field.height(), control_height.unwrap() * 2.0);
            assert!(
                field.width() > 800.0,
                "form measure follows the host UI scale"
            );
            assert!(field.x0 >= preview.rect.x0 && field.x1 <= preview.rect.x1);
        })
        .unwrap();
    }

    #[test]
    fn page_motion_tracks_navigation_and_respects_reduced_motion() {
        let mut app = fixture();
        let paint = |preview: &mut InstallerPreview, theme: &Theme| {
            preview.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut TextRenderer::new(),
                theme,
                &PresenceCtx::default(),
            );
        };
        app.with_widget::<InstallerPreview, _>(|preview| paint(preview, &Theme::dark()))
            .unwrap();
        choose(&mut app, "computer");
        app.with_widget::<InstallerPreview, _>(|preview| {
            paint(preview, &Theme::dark());
            assert_eq!(preview.slide.as_ref().unwrap().from(), 1.0);
            assert!(preview.outgoing.is_some());
            assert!(preview.needs_animation_frame());
            let mut reduced = Theme::dark();
            reduced.reduce_motion = true;
            paint(preview, &reduced);
            assert!(preview.slide.is_none());
            assert!(preview.outgoing.is_none());
            assert!(!preview.needs_animation_frame());
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "migrate".into(),
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(
                preview.pending_slide.is_none(),
                "selection alone is not navigation"
            );
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            paint(preview, &Theme::dark());
            assert_eq!(preview.slide.as_ref().unwrap().from(), -1.0);
            let slide = preview.slide.as_ref().unwrap();
            preview.slide = Some(Animated::starting_at(
                -1.0,
                0.0,
                ui::time::now().checked_sub(slide.duration()).unwrap(),
                slide.duration(),
                Curve::EaseInOut,
            ));
            paint(preview, &Theme::dark());
            assert!(
                !preview.needs_animation_frame(),
                "settled pages stop requesting frames"
            );
            assert!(preview.outgoing.is_none());
        })
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_belongings_actions_keep_files_and_apps_in_place_and_select_connections() {
        let mut app = fixture();
        // Retained legacy preparations can still render their migration choices.
        // New Desktop choices enter native review instead.
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.destination = Some(Destination::Desktop);
            preview.journey.page = Page::Belongings;
            preview.journey.migrate = Some(true);
            preview.journey.categories = [false, false, true];
            preview.dirty = true;
        })
        .unwrap();
        assert_eq!(status(&mut app)["page"], "belongings");
        app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
            .unwrap();
        let inspection = app.harness().inspection_json().unwrap();
        for id in [
            "installer.desktop.files.in-place",
            "installer.desktop.apps.in-place",
            "installer.category.accounts",
        ] {
            assert!(inspection.contains(id), "Desktop exposes {id}");
        }
        assert!(!inspection.contains("installer.category.files"));
        assert!(!inspection.contains("installer.category.flatpak"));
        for category in ["files", "flatpak"] {
            for included in [false, true] {
                assert!(
                    app.effect(&InstallerPreviewEffect::SetPreviewCategory {
                        category: category.into(),
                        included
                    })
                    .is_err()
                );
            }
        }
        for category in ["bookmarks", "history", "accounts"] {
            app.effect(&InstallerPreviewEffect::SetPreviewCategory {
                category: category.into(),
                included: false,
            })
            .unwrap();
        }
        let snapshot = status(&mut app);
        assert_eq!(
            snapshot["categories"],
            serde_json::json!([false, false, false])
        );
        assert_eq!(
            snapshot["browser_domains"],
            serde_json::json!([false, false])
        );
        app.effect(&InstallerPreviewEffect::SetPreviewCategory {
            category: "accounts".into(),
            included: true,
        })
        .unwrap();
        assert_eq!(status(&mut app)["categories"][2], true);
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.selected_portable_apps().is_empty());
            assert_eq!(preview.migration_categories(), [false, false, true]);
        })
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_identity_routes_admit_device_without_pairing_and_keep_backup() {
        let mut app = fixture();
        // Exercise the retained legacy connection surface, not new Desktop navigation.
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.destination = Some(Destination::Desktop);
            preview.journey.migrate = Some(false);
            preview.journey.identity = Some(Identity::Existing);
            preview.journey.page = Page::Connect;
            preview.dirty = true;
        })
        .unwrap();
        assert_eq!(status(&mut app)["page"], "connect");
        let inspection = app.harness().inspection_json().unwrap();
        assert!(inspection.contains("installer.choice.device"));
        assert!(inspection.contains("installer.choice.backup"));
        assert!(inspection.contains("installer.choice.current"));
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "device".into(),
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.pairing.started());
            assert_eq!(preview.journey.transfer.as_deref(), Some("device"));
            assert!(preview.personal.prepared_handle().is_none());
            assert!(preview.identity_route_available("backup"));
            assert!(preview.identity_route_available("device"));
        })
        .unwrap();
        assert_eq!(status(&mut app)["page"], "connect");
        app.effect(&InstallerPreviewEffect::ChoosePreview {
            choice: "backup".into(),
        })
        .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.transfer.as_deref(), Some("backup"));
            assert!(!preview.existing_identity.selected());
        })
        .unwrap();
        assert_eq!(status(&mut app)["page"], "connect");
    }

    fn catalog_availability(
        app: &mut TestApp<ImagerShell>,
        id: &str,
    ) -> ui::tilo_actions::Availability {
        fn find(
            resources: &[ui::tilo_actions::Resource],
            id: &str,
        ) -> Option<ui::tilo_actions::Availability> {
            resources.iter().find_map(|resource| {
                resource
                    .actions
                    .iter()
                    .find(|action| action.id == id)
                    .map(|action| action.contract.availability.clone())
                    .or_else(|| find(&resource.children, id))
            })
        }
        find(&app.harness().resources(), id).expect("public effect must be published")
    }

    #[test]
    fn guided_catalog_page_actions_follow_dispatch_and_review_profile_edits() {
        let mut app = fixture();
        for (id, effect) in [
            (
                "set-preview-name",
                InstallerPreviewEffect::SetPreviewName {
                    value: "Wrong page".into(),
                },
            ),
            (
                "select-preview-avatar",
                InstallerPreviewEffect::SelectPreviewAvatar {
                    source_file: "/not-granted.png".into(),
                },
            ),
            (
                "select-existing-identity-backup",
                InstallerPreviewEffect::SelectExistingIdentityBackup {
                    source_file: "/not-granted.backup".into(),
                },
            ),
            (
                "set-preview-password",
                InstallerPreviewEffect::SetPreviewPassword {
                    value: "Not accepted offscreen".into(),
                },
            ),
            (
                "set-account-field",
                InstallerPreviewEffect::SetAccountField {
                    field: "password".into(),
                    value: "Not accepted offscreen".into(),
                },
            ),
        ] {
            assert!(matches!(catalog_availability(&mut app, id),
                ui::tilo_actions::Availability::Unavailable { code, .. } if code == "wrong_page"));
            assert!(
                app.effect(&effect).is_err(),
                "{id} cannot mutate another page"
            );
            assert_eq!(status(&mut app)["page"], "destination");
        }
        assert!(catalog_availability(&mut app, "read-preview").is_available());
        choose(&mut app, "another");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        assert!(catalog_availability(&mut app, "set-preview-name").is_available());
        assert!(catalog_availability(&mut app, "select-preview-avatar").is_available());
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert!(!catalog_availability(&mut app, "set-preview-name").is_available());
        assert!(catalog_availability(&mut app, "set-preview-password").is_available());
        password(&mut app);
        assert_eq!(status(&mut app)["page"], "devices");
        assert!(catalog_availability(&mut app, "set-preview-device-name").is_available());
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        if status(&mut app)["page"] == "storage" {
            assert!(catalog_availability(&mut app, "set-storage-size").is_available());
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .unwrap();
        }
        assert_eq!(status(&mut app)["page"], "review");
        assert!(catalog_availability(&mut app, "edit-preview").is_available());
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "profile".into(),
        })
        .unwrap();
        assert!(catalog_availability(&mut app, "set-preview-name").is_available());
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Reviewed Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "review");
        assert_eq!(status(&mut app)["name"], "Reviewed Ada");
        assert!(!catalog_availability(&mut app, "set-preview-name").is_available());
    }

    #[test]
    fn guided_catalog_belongings_controls_require_expanded_review() {
        let mut app = fixture();
        choose(&mut app, "another");
        choose(&mut app, "migrate");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        password(&mut app);
        assert_eq!(status(&mut app)["page"], "devices");
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "belongings");
        let effect = InstallerPreviewEffect::SetPreviewCategory {
            category: "accounts".into(),
            included: false,
        };
        assert!(!catalog_availability(&mut app, "set-preview-category").is_available());
        assert!(app.effect(&effect).is_err());
        assert_eq!(status(&mut app)["categories"][2], true);
        app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
            .unwrap();
        assert!(catalog_availability(&mut app, "set-preview-category").is_available());
        app.effect(&effect).unwrap();
        assert_eq!(status(&mut app)["categories"][2], false);
        app.effect(&InstallerPreviewEffect::TogglePreviewDetails)
            .unwrap();
        assert!(!catalog_availability(&mut app, "set-preview-category").is_available());
        assert_eq!(status(&mut app)["categories"][2], false);
    }

    #[test]
    fn guided_catalog_navigation_admission_tracks_start_and_completion() {
        let mut app = fixture();
        assert!(matches!(catalog_availability(&mut app, "back-in-preview"),
            ui::tilo_actions::Availability::Unavailable { code, .. } if code == "at_start"));
        assert!(app.effect(&InstallerPreviewEffect::BackInPreview).is_err());
        assert_eq!(status(&mut app)["page"], "destination");
        choose(&mut app, "another");
        assert!(catalog_availability(&mut app, "back-in-preview").is_available());
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(status(&mut app)["page"], "destination");
        choose(&mut app, "another");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "My name".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        password(&mut app);
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        // Follow the actual route through Review to completed illustrative setup.
        for _ in 0..4 {
            if status(&mut app)["page"] == "ready" {
                break;
            }
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .unwrap();
        }
        assert_eq!(status(&mut app)["page"], "ready");
        assert!(matches!(catalog_availability(&mut app, "continue-preview"),
            ui::tilo_actions::Availability::Unavailable { code, .. } if code == "finished"));
        assert!(
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .is_err()
        );
        assert_eq!(status(&mut app)["page"], "ready");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn guided_catalog_refuses_unavailable_installation_and_owned_busy_navigation() {
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.journey.page = Page::Review;
            preview.journey.destination = Some(Destination::Computer);
            preview.journey.history = vec![Page::Devices];
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        assert!(
            matches!(catalog_availability(&mut app, "prepare-direct-installation"),
            ui::tilo_actions::Availability::Unavailable { code, .. } if code == "installation_unavailable")
        );
        assert!(
            app.effect(&InstallerPreviewEffect::PrepareDirectInstallation)
                .is_err()
        );
        assert_eq!(status(&mut app)["page"], "review");
        // Model the canonical retained Full-OS surface while its owner withholds
        // return navigation; this test starts no installation or host worker.
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.direct.active = true;
            preview.direct.navigation.set(false);
        })
        .unwrap();
        app.settle();
        assert!(matches!(catalog_availability(&mut app, "back-in-preview"),
            ui::tilo_actions::Availability::Unavailable { code, .. } if code == "installation_busy"));
        assert!(app.effect(&InstallerPreviewEffect::BackInPreview).is_err());
        assert_eq!(status(&mut app)["direct_install"]["active"], true);
        app.with_widget::<InstallerPreview, _>(|preview| preview.direct.navigation.set(true))
            .unwrap();
        app.settle();
        assert!(catalog_availability(&mut app, "back-in-preview").is_available());
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(status(&mut app)["direct_install"]["active"], false);
        assert_eq!(status(&mut app)["page"], "review");
    }

    #[test]
    fn unsupported_guided_host_refuses_personal_setup_through_public_actions() {
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            // Downgrade to the same immutable model used by unsupported hosts.
            preview.capabilities = capabilities::GuidedCapabilities {
                prepared_installation: false,
            };
            preview.journey.destination = Some(Destination::Another);
            preview.journey.migrate = Some(true);
            preview.journey.name = "Retained name".into();
            preview.dirty = true;
        })
        .unwrap();
        let before = status(&mut app);
        assert_eq!(
            before["guided_capabilities"]["prepared_installation"],
            false
        );
        for choice in ["computer", "another", "desktop"] {
            assert!(
                app.effect(&InstallerPreviewEffect::ChoosePreview {
                    choice: choice.into(),
                })
                .is_err()
            );
        }
        assert!(
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .is_err()
        );
        let after = status(&mut app);
        assert_eq!(after["page"], "destination");
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.destination, Some(Destination::Another));
            assert_eq!(preview.journey.migrate, Some(true));
            assert_eq!(preview.journey.name, "Retained name");
            assert!(preview.belongings_worker.is_none());
            #[cfg(unix)]
            assert!(preview.personal.prepared_handle().is_none());
        })
        .unwrap();
        app.settle();
        fn unavailable(resources: &[ui::tilo_actions::Resource], id: &str) -> bool {
            resources.iter().any(|resource| {
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == id && !action.contract.availability.is_available())
                    || unavailable(&resource.children, id)
            })
        }
        let resources = app.harness().resources();
        for id in ["choose-preview", "continue-preview"] {
            assert!(
                unavailable(&resources, id),
                "{id} must match painted admission"
            );
        }
        let view = app.harness().inspection_json().unwrap();
        assert!(view.contains(&t("installer-guided-host-unavailable")));
        assert!(!view.contains("installer.choice.another"));
        assert!(!view.contains("installer.choice.computer"));
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.journey.page = Page::Migration;
            preview.dirty = true;
        })
        .unwrap();
        assert!(
            app.effect(&InstallerPreviewEffect::ContinuePreview)
                .is_err()
        );
        for choice in ["migrate", "fresh"] {
            assert!(
                app.effect(&InstallerPreviewEffect::ChoosePreview {
                    choice: choice.into(),
                })
                .is_err()
            );
        }
        assert_eq!(status(&mut app)["page"], "migration");
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(preview.belongings_worker.is_none());
            assert_eq!(preview.journey.migrate, Some(true));
        })
        .unwrap();
        // The same unsupported capability does not restrict illustrative mode.
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = false;
            preview.journey.page = Page::Destination;
            preview.dirty = true;
        })
        .unwrap();
        choose(&mut app, "another");
        assert_eq!(status(&mut app)["page"], "migration");
        choose(&mut app, "fresh");
        assert_eq!(status(&mut app)["page"], "identity");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn supported_guided_host_keeps_personal_setup_before_storage() {
        let mut app = fixture();
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.live = true;
            preview.dirty = true;
        })
        .unwrap();
        assert_eq!(
            status(&mut app)["guided_capabilities"]["prepared_installation"],
            true
        );
        choose(&mut app, "another");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        assert_eq!(status(&mut app)["page"], "profile");
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.journey.history.contains(&Page::Storage));
            assert!(preview.belongings_worker.is_none());
        })
        .unwrap();
    }

    #[test]
    fn identity_recovery_route_availability_matches_native_platform_before_choice() {
        let mut preview = InstallerPreview {
            live: true,
            ..Default::default()
        };
        for destination in [
            Destination::Computer,
            Destination::Another,
            Destination::Desktop,
        ] {
            for migrate in [false, true] {
                preview.journey.destination = Some(destination);
                preview.journey.migrate = Some(migrate);
                assert_eq!(
                    preview.identity_route_available("device"),
                    cfg!(target_os = "linux")
                );
                assert_eq!(preview.identity_route_available("backup"), cfg!(unix));
                assert_eq!(
                    preview.identity_route_available("current"),
                    cfg!(target_os = "linux") && destination == Destination::Desktop
                );
                assert!(!preview.identity_route_available("unknown"));
                assert!(preview.personal.prepared_handle().is_none());
                assert!(!preview.pairing.started());
                assert!(preview.journey.transfer.is_none());
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn assert_live_route_skips_empty_storage(app: &mut TestApp<ImagerShell>, choice: &str) {
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "destination".into(),
        })
        .unwrap();
        choose(app, choice);
        assert_eq!(status(app)["page"], "review");
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.journey.history.contains(&Page::Storage));
            assert!(preview.personal.prepared_handle().is_some());
            assert!(!preview.journey.password.is_empty());
            assert!(!preview.media_preparation.busy());
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(status(app)["page"], "devices");
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(app)["page"], "review");
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert_eq!(preview.journey.history.last(), Some(&Page::Devices));
        })
        .unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn inspected_storage_changes_the_real_plan_and_live_setup_cannot_fake_completion() {
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        env.set(super::super::internal_install_flow::RUNTIME_GATE_VAR, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        env.set("XDG_STATE_HOME", root.join("state"));
        let mut app = fixture();
        choose(&mut app, "computer");
        choose(&mut app, "fresh");
        choose(&mut app, "new");
        app.effect(&InstallerPreviewEffect::SetPreviewName {
            value: "Ada".into(),
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        app.with_widget::<InstallerPreview, _>(|preview| preview.live = true)
            .unwrap();
        password(&mut app);
        let prepared = wait_for_personal(&mut app);
        assert_eq!(prepared["page"], "devices");
        let identity_key = prepared["prepared_identity"]["public_key"].clone();
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(wait_for_belongings(&mut app)["page"], "storage");
        let before = status(&mut app)["storage"].clone();
        assert!(before["plan_digest"].is_string());
        app.effect(&InstallerPreviewEffect::SetStorageSize { gib: 80 })
            .unwrap();
        let after = status(&mut app)["storage"].clone();
        assert_eq!(after["size_gib"], 80);
        assert_ne!(before["plan_digest"], after["plan_digest"]);
        assert!(
            app.effect(&InstallerPreviewEffect::SetStorageSize { gib: 1 })
                .is_err()
        );
        assert_eq!(status(&mut app)["storage"]["size_gib"], 80);
        app.settle();
        let inspection = app.harness().inspection_json().unwrap();
        assert!(inspection.contains("Space for Tilo"));
        assert!(!inspection.contains("Temporary installer space"));
        assert!(!inspection.contains("installer.storage.recheck"));
        app.effect(&InstallerPreviewEffect::ToggleStorageDetails)
            .unwrap();
        app.settle();
        let inspection = app.harness().inspection_json().unwrap();
        assert!(inspection.contains("Temporary installer space"));
        assert!(!inspection.contains("personal choices come first"));
        assert!(!inspection.contains("installer.storage.recheck"));
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview
                .storage
                .facts
                .as_mut()
                .unwrap()
                .blockers
                .push(super::super::staging::probe::Blocker::OnBattery);
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains("installer.storage.recheck")
        );
        app.with_widget::<InstallerPreview, _>(|preview| {
            preview.storage.facts.as_mut().unwrap().blockers.clear();
            preview.dirty = true;
        })
        .unwrap();
        app.settle();
        assert!(
            !app.harness()
                .inspection_json()
                .unwrap()
                .contains("installer.storage.recheck")
        );
        app.with_widget::<InstallerPreview, _>(|preview| {
            let plan = preview.storage.plan.as_ref().unwrap().as_ref().unwrap();
            let original = &plan.basis.disk.partitions[0];
            assert!(plan.target_extent.first_lba > original.last_lba);
            assert!(plan.staging_root.last_lba <= plan.basis.disk.last_usable_lba);
            assert!(
                plan.shrink.is_none(),
                "Linux plans never resize the original partition"
            );
            preview.live = true;
        })
        .unwrap();
        assert!(app.effect(&InstallerPreviewEffect::PrepareUsb).is_err());
        assert_eq!(status(&mut app)["page"], "storage");
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "review");
        let inspection = app.harness().inspection_json().unwrap();
        assert!(inspection.contains("Ada"));
        assert!(inspection.contains(&t("installer-preview-review-heading")));
        assert!(!inspection.contains("installer.storage.recheck"));
        app.with_widget::<InstallerPreview, _>(|preview| {
            assert!(!preview.journey.password.is_empty());
            assert!(!preview.media_preparation.busy());
        })
        .unwrap();
        app.effect(&InstallerPreviewEffect::BackInPreview).unwrap();
        assert_eq!(status(&mut app)["page"], "storage");
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "review");
        for destination in ["another", "desktop"] {
            assert_live_route_skips_empty_storage(&mut app, destination);
            assert_eq!(
                status(&mut app)["prepared_identity"]["public_key"],
                identity_key
            );
        }
        app.effect(&InstallerPreviewEffect::EditPreview {
            section: "destination".into(),
        })
        .unwrap();
        choose(&mut app, "computer");
        assert_eq!(status(&mut app)["page"], "storage");
        app.effect(&InstallerPreviewEffect::ContinuePreview)
            .unwrap();
        assert_eq!(status(&mut app)["page"], "review");
        let bundle = app
            .with_widget::<InstallerPreview, _>(|preview| {
                preview.media_bundle.as_ref().unwrap().clone()
            })
            .unwrap();
        app.effect(&InstallerPreviewEffect::PrepareUsb).unwrap();
        let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
        let artifact = loop {
            app.settle();
            if let Some(artifact) = bundle.get() {
                break artifact;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "prepared media worker did not complete"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        app.settle();
        assert_eq!(artifact.projection()["public_key"], identity_key);
        assert!(artifact.expectation().valid());
        let media_status = app
            .effect(&crate::resource::ImagerEffect::Status)
            .unwrap()
            .value
            .unwrap();
        assert_eq!(media_status["prepared_setup"]["state"], "waiting-for-write");
        assert!(media_status["prepared_setup"]["receipt"].is_null());
        assert!(std::sync::Arc::ptr_eq(&bundle.get().unwrap(), &artifact));
        assert!(
            app.state().profile.prepared_setup.is_none(),
            "custody receipts remain device-local"
        );
        assert_eq!(app.state().profile.user.full_name, "Ada");
        assert_eq!(
            app.state().profile.device_label,
            t("installer-preview-device-default")
        );
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains("Choose an image")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_connect_capture_matches_semantic_route_without_accepting_identity() {
        let mut capture =
            TestApp::<ImagerShell>::capture(Some("installer-desktop-connect")).unwrap();
        let capture_inspection = capture.harness().inspection_json().unwrap();
        for migration in ["fresh", "migrate"] {
            let mut route = fixture();
            // Captures retain the legacy prepared migration surface. New
            // Desktop navigation is covered by the native-review route test.
            route
                .with_widget::<InstallerPreview, _>(|preview| {
                    preview.live = true;
                    preview.journey.destination = Some(Destination::Desktop);
                    preview.journey.migrate = Some(migration == "migrate");
                    preview.journey.identity = Some(Identity::Existing);
                    preview.journey.page = Page::Connect;
                    preview.dirty = true;
                })
                .unwrap();
            assert_eq!(status(&mut route)["page"], "connect");
            let route_inspection = route.harness().inspection_json().unwrap();
            for id in [
                "installer.choice.current",
                "installer.choice.backup",
                "installer.choice.device",
            ] {
                assert!(route_inspection.contains(id), "semantic route exposes {id}");
                assert!(capture_inspection.contains(id), "capture exposes {id}");
            }
            route
                .with_widget::<InstallerPreview, _>(|preview| {
                    assert!(preview.personal.prepared_handle().is_none());
                    assert!(!preview.pairing.started());
                    assert!(preview.journey.transfer.is_none());
                    assert_eq!(preview.journey.migrate, Some(migration == "migrate"));
                })
                .unwrap();
            route
                .effect(&InstallerPreviewEffect::ChoosePreview {
                    choice: "device".into(),
                })
                .unwrap();
            route
                .with_widget::<InstallerPreview, _>(|preview| {
                    assert_eq!(preview.journey.transfer.as_deref(), Some("device"));
                    assert_eq!(preview.journey.page, Page::Connect);
                    assert_eq!(preview.journey.destination, Some(Destination::Desktop));
                    assert_eq!(preview.journey.identity, Some(Identity::Existing));
                    assert_eq!(preview.journey.migrate, Some(migration == "migrate"));
                    assert!(preview.personal.prepared_handle().is_none());
                    assert!(!preview.pairing.started());
                })
                .unwrap();
        }
        capture
            .with_widget::<InstallerPreview, _>(|preview| {
                assert_eq!(preview.journey.destination, Some(Destination::Desktop));
                assert_eq!(preview.journey.migrate, Some(false));
                assert_eq!(preview.journey.identity, Some(Identity::Existing));
                assert_eq!(preview.journey.page, Page::Connect);
                assert!(preview.personal.prepared_handle().is_none());
                assert!(!preview.pairing.started());
                assert!(preview.journey.transfer.is_none());
            })
            .unwrap();
    }

    #[test]
    fn every_preview_screen_renders_without_reading_migration_sources() {
        for scenario in SCENARIOS {
            let mut app = TestApp::<ImagerShell>::capture(Some(scenario)).unwrap();
            let inspection = app.harness().inspection_json().unwrap();
            let page = match *scenario {
                "installer-belongings-details" => "belongings",
                "installer-desktop-connect" => "connect",
                other => other.strip_prefix("installer-").unwrap(),
            };
            let heading = if page == "accounts" {
                t("installer-accounts-heading")
            } else {
                t(&format!("installer-preview-{page}-heading"))
            };
            assert!(
                inspection.contains(&heading),
                "{scenario} exposes its heading"
            );
            assert!(
                !inspection.contains("installer-preview-")
                    && !inspection.contains("installer-accounts-"),
                "{scenario} resolves catalogue keys"
            );
        }
        let mut app = TestApp::<ImagerShell>::capture(Some("installer-identity")).unwrap();
        assert!(
            app.harness()
                .inspection_json()
                .unwrap()
                .contains("Welcome!")
        );
    }
}
