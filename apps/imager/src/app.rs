//! Root widget for the Disk Imager (imager) app.
//!
//! `ImagerApp` is a single painted widget that renders a guided **wizard**:
//! a stepper (Image → Drive → Customize → Confirm → Write → Done) over the
//! step's content and a Back / primary-action footer, for creating a
//! verified USB or SD installation medium.
//!
//! * **Image** — pick a local `.iso` / `.img` through the host XDG portal
//!   (never a bespoke picker, per HIG §5), or a known Tilo release.
//! * **Drive** — choose external media from the *real* block devices (`lsblk`);
//!   internal and system disks are never eligible. Installation onto those is
//!   handled after boot by the live Tilo installer.
//! * **Customize** (Tilo only) — the shared guided-customization step, delegated
//!   in full to [`CustomizePanel`](crate::customize); a plain ISO skips it.
//! * **Confirm** — a read-only review of every choice, an erase warning, and
//!   the typed device-name gate.
//! * **Write** — a real, byte-for-byte streaming copy with live progress and a
//!   read-back **verify** pass.
//! * **Done** — the verified / failed outcome with a fresh-start affordance.
//!
//! Every block is a toolkit composite painted through its face — the
//! stepper, the drop zone, list rows, the property list, the banner, the
//! progress ring and bars, the empty state, the buttons — so the wizard
//! chooses where things go and never how they look. The framework paints the
//! window ground.
//!
//! Every write is doubly gated: the typed confirmation at Confirm, then native
//! host authentication for a short-lived privileged writer. The wizard itself
//! stays unprivileged; tests and shots cannot start the helper.
//!
//! Reviewed sources and non-secret install preferences are collaborative
//! Automerge resources. Drive selection, typed confirmation, progress and
//! worker channels are local capabilities: another peer can inspect a plan,
//! but never select/arm/write this machine's device.

use std::cell::RefCell;
use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::WindowEvent;

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::surface;
use ui::text_metrics::centered_text_top;
use ui::text_render::{ParagraphAlign, TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Banner, BannerContent, BannerKind, Button, ButtonFace, ButtonRole, ControlSize, DropZoneFace,
    EmptyState, EmptyStateContent, Interaction, ListRowFace, ProgressBar, ProgressRingFace,
    ProgressRingSize, PropertyListFace, PropertyRow, RowLeading, ScrollMetrics, Scrollbar,
    StepperFace, TextInput, Widget, paint_focus_ring,
};

use crate::Gaps;
use crate::customize::{CustomizePanel, CzField};
use crate::doc::ImagerDoc;
use crate::export_dialog::ExportDialog;
use crate::model::{
    Action, InstallProfile, JobPhase, Progress, Source, SourceKind, Step, TargetDrive, TiloRelease,
    fmt_eta, fmt_rate, fmt_size,
};
use crate::write::{JobControl, WriteRequest, spawn_job};
use crate::write_setup::WriteSetupPanel;

/// The widest the wizard's content column grows: a form and its review
/// read best at a bounded measure (HIG §7a), the same one `FormPage` keeps.
pub(crate) const READABLE_WIDTH: f64 = 720.0;

thread_local! {
    /// The renderer that measures the wizard before the host's renderer has:
    /// a fresh window lays every block out on real line boxes, so a probe
    /// frame never places a control on a zero-height band.
    static PROBE_RENDERER: RefCell<TextRenderer> = RefCell::new(TextRenderer::new());
}

/// The labels of every footer primary action, so the button keeps one width
/// while its label changes (Cancel write → Stopping…).
const PRIMARY_LABELS: [&str; 6] = [
    "Continue",
    "Erase and write",
    "Cancel write",
    "Stopping…",
    "Start over",
    "Review and retry",
];
const RESCAN_LABEL: &str = "Rescan";
const DROP_TITLE: &str = "Choose a disk image";
const DROP_MESSAGE: &str = "ISO, IMG or RAW file";
const DROP_ACTION: &str = "Browse…";
const EMPTY_DRIVES: &str = "Insert a USB drive or SD card";
const NO_RELEASES: &str = "No Tilo releases are stored on this computer.";
const PLAIN_BODY: &str =
    "Setup options are only available for Tilo images. This image is written unchanged.";

/// Every text-derived band of the wizard, measured together from the
/// renderer and theme that paint it: one comparison decides whether the
/// layout re-solves and one assignment stores the result, so a band added
/// later cannot be left out of either.
#[derive(Clone, Debug, PartialEq, Default)]
struct Bands {
    gaps: Gaps,
    /// The theme's text scale; the compact breakpoint scales by it.
    text_scale: f64,
    /// The theme's UI scale; the readable width scales by it.
    ui_scale: f64,
    /// The stepper's height at the content width it was measured for.
    stepper_h: f64,
    /// One line of the step title.
    title_h: f64,
    /// A regular and a small control.
    control_h: f64,
    small_control_h: f64,
    /// The footer's Back button and its widest primary action.
    back_w: f64,
    primary_w: f64,
    /// The Drive step's Rescan button.
    rescan_w: f64,
    /// One line of `Caption` (the footer hint, the field note) and `Label`
    /// (a field caption).
    caption_h: f64,
    label_h: f64,
    /// One line of `Body`.
    body_h: f64,
    /// A section header line (`Subheading`).
    section_h: f64,
    /// A list row with a secondary line.
    row_h: f64,
    /// The drop zone with its Browse button.
    dropzone_h: f64,
    /// The plain-image explanation on Customize.
    plain_h: f64,
    /// The Confirm review and its erase warning.
    review_h: f64,
    banner_h: f64,
    /// The Write step's progress ring.
    ring_d: f64,
    /// The Done hero: its mark, headline and explanation, and the summary.
    headline_h: f64,
    done_sub_h: f64,
    summary_h: f64,
}

/// A clickable hit-target for one frame. Pure function of the viewport +
/// state, so `draw` and `handle_event` agree without ordering hazards. The
/// Customize step's own affordances live inside [`CustomizePanel`], not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    PickFile,
    Release(usize),
    CancelDownload,
    Recent(usize),
    Target(usize),
    Rescan,
    ConfirmField,
    Back,
    Next,
    StartWrite,
    CancelWrite,
    WriteAnother,
}

struct Layout {
    /// Where the stepper sits above the content.
    stepper: Rect,
    back: Option<Rect>,
    primary: Option<Rect>,
    hits: Vec<(Rect, Hit)>,
    /// The clip of the scrolling step body.
    viewport_content: Rect,
    /// The step body, scrolled: its full extent from the scrolled top.
    content: Rect,
}

/// One block of the Image step, in reading order, with the band it fills.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SourceBlock {
    DropZone,
    /// A section header line.
    Header(&'static str),
    /// The selected image when no list below shows it.
    Selected,
    Release(usize),
    NoReleases,
    Recent(usize),
}

pub struct ImagerApp {
    // ---- collaborative records (cached each `sync`) ----
    source: Option<Source>,
    recent: Vec<Source>,
    profile: InstallProfile,
    /// Host-session Wi-Fi passphrase. Never enters the collaborative document.
    wifi_psk: String,
    // ---- local machine/window state ----
    targets: Vec<TargetDrive>,
    /// Tilo releases offered on the Source step are a bounded local catalogue;
    /// they are never collaborative state. An offered release is a
    /// write source for a job that erases the target, so one that does not
    /// exist on this machine must not be clickable.
    releases: Vec<TiloRelease>,
    selected: Option<usize>,
    action: Action,
    progress: Progress,
    /// Peer-local destructive-write arming field. This must never replicate:
    /// a collaborator may review the plan, but cannot type consent for us.
    confirm_input: TextInput,
    status: String,
    step: Step,
    // ---- transient job / portal plumbing (not in the doc) ----
    job_rx: Option<std::sync::mpsc::Receiver<Progress>>,
    job_control: Option<JobControl>,
    portal_rx: Option<PortalChannel>,
    release_rx: Option<std::sync::mpsc::Receiver<crate::release::DownloadEvent>>,
    release_control: Option<crate::release::DownloadControl>,
    release_download_index: Option<usize>,
    release_download_progress: Option<(u64, u64)>,
    release_download_stage: Option<&'static str>,
    export_dialog: ExportDialog,
    /// Stages the read-only export modal after the demo document has synced.
    /// Used only by the deterministic `export` screenshot scenario.
    export_after_sync: bool,
    /// Makes the destructive action look armed in an offscreen documentation
    /// frame without enabling (or bypassing) the real write backend.
    shot_confirmation_ready: bool,
    /// False until the first live sync has reconciled any persisted active
    /// phase with this process's necessarily empty worker channels.
    startup_write_state_reconciled: bool,
    // ---- render caches ----
    seeded: bool,
    rect: Rect,
    dirty: bool,
    last_mouse: Point,
    /// The theme of the last layout pass, so the measuring hook (which
    /// receives no `Theme`) resolves the same roles and spacing the painters
    /// use.
    theme: Theme,
    /// Every step's text-derived bands; see [`Bands`].
    bands: Bands,
    /// The Write step's two phase bars.
    phase_bars: [ProgressBar; 2],
    /// Vertical offset for the step body. The stepper and footer stay put.
    scroll_y: f64,
    /// Keyboard focus for canvas controls. Profile fields retain their focus
    /// in `CustomizePanel`, using the same Tab sequence.
    keyboard_hit: Option<Hit>,
    /// The shared guided-customization panel — the one component the write and
    /// install flows both use to collect the install profile. It owns its own
    /// fields (real [`ui::widget::TextInput`]s, the password masked + a11y
    /// protected) and dispatches each edit through [`Self::actions`]. See
    /// [`crate::customize`].
    customize: CustomizePanel,
    /// Wi-Fi and identity applied onto the written root by this machine.
    write_setup: WriteSetupPanel,
    /// Framework-owned action dispatch (the only action-dispatch state the app
    /// holds). Every guided-customization profile edit dispatches its
    /// [`ImagerAction`](crate::actions::ImagerAction) through this host, which
    /// brackets the undo boundary and applies it through the executor so it
    /// records one Ctrl+Z step and replays byte-identically on peers.
    actions: crate::resource::ActionSink,
}

impl Default for ImagerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ImagerApp {
    pub fn new() -> Self {
        let mut app = Self {
            source: None,
            recent: Vec::new(),
            targets: Vec::new(),
            releases: Vec::new(),
            selected: None,
            action: Action::Flash,
            profile: InstallProfile::default(),
            wifi_psk: String::new(),
            progress: Progress::default(),
            confirm_input: TextInput::local()
                .with_id("imager_device_confirmation")
                .with_kind(ui::widget::TextInputKind::Path)
                .with_enter_submits(true),
            status: String::new(),
            step: Step::Source,
            job_rx: None,
            job_control: None,
            portal_rx: None,
            release_rx: None,
            release_control: None,
            release_download_index: None,
            release_download_progress: None,
            release_download_stage: None,
            export_dialog: ExportDialog::new(),
            export_after_sync: false,
            shot_confirmation_ready: false,
            startup_write_state_reconciled: false,
            seeded: false,
            rect: Rect::ZERO,
            dirty: true,
            last_mouse: Point::ZERO,
            theme: Theme::dark(),
            bands: Bands::default(),
            phase_bars: [
                ProgressBar::new(0.0).with_label("Write"),
                ProgressBar::new(0.0).with_label("Verify"),
            ],
            scroll_y: 0.0,
            keyboard_hit: None,
            customize: CustomizePanel::new(),
            write_setup: WriteSetupPanel::new(),
            actions: crate::resource::ActionSink::default(),
        };
        app.estimate_bands();
        app
    }

    pub(crate) fn target(&self) -> Option<&TargetDrive> {
        self.selected.and_then(|i| self.targets.get(i))
    }

    /// The releases the document actually knows about — empty on the live
    /// path, seeded content under `--demo` / `--shots`.
    fn releases(&self) -> &[TiloRelease] {
        &self.releases
    }

    /// Only external installation media is shown. Internal installation and
    /// dual boot belong to the installer on the booted Tilo medium.
    fn visible_targets(&self) -> Vec<(usize, &TargetDrive)> {
        self.targets
            .iter()
            .enumerate()
            .filter(|(_, drive)| drive.is_media_target())
            .collect()
    }

    fn removable_count(&self) -> usize {
        self.targets
            .iter()
            .filter(|drive| drive.is_media_target())
            .count()
    }

    /// Can the flow move forward from `from`? (Also gates the step-indicator.)
    fn can_advance(&self, from: Step) -> bool {
        self.can_advance_with_writer(from, crate::write::write_available())
    }

    /// Pure half of the advance gate, split so the live-helper capability is
    /// covered without mutating process-wide environment variables in UI
    /// tests that run concurrently.
    fn can_advance_with_writer(&self, from: Step, writer_available: bool) -> bool {
        match from {
            Step::Source => self.source.is_some(),
            Step::Drive => self.selected.is_some(),
            Step::Customize => true,
            // Confirm advances into the (destructive) write only once armed.
            Step::Confirm => {
                self.startup_write_state_reconciled
                    && self.confirm_matches()
                    && self.image_fits()
                    && writer_available
            }
            // Write is in-progress and Done is terminal — neither advances.
            Step::Write | Step::Done => false,
        }
    }

    /// Only a Tilo image authenticated by the bundled release catalog can be
    /// pre-configured; every arbitrary local image is written as-is.
    fn customizable(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| self.source_is_official(source))
    }

    /// Confirm can collect Wi-Fi and enrollment for any selected image. Official
    /// installable media still uses Customize for the secret-free embedded
    /// profile; on-write handoff is staged separately and applied after the
    /// verified write onto the installed root.
    fn offers_write_setup(&self) -> bool {
        self.source.is_some()
    }

    fn source_is_official(&self, source: &Source) -> bool {
        source.kind == SourceKind::TiloImage
            && source.installable
            && source.expected_sha256.as_deref().is_some_and(|digest| {
                digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            && self.releases().iter().any(|release| {
                release.image_sha256 == source.expected_sha256.as_deref().unwrap_or_default()
                    && release.size == source.size
                    && format!("{} {}", release.name, release.version) == source.name
            })
    }

    /// Read-only projections used to build the searchable resource hierarchy.
    /// The target list and job state describe this machine, so publishing them
    /// does not turn them into shared authority.
    pub(crate) fn resource_source(&self) -> Option<&Source> {
        self.source.as_ref()
    }
    pub(crate) fn resource_targets(&self) -> &[TargetDrive] {
        &self.targets
    }
    pub(crate) fn resource_progress(&self) -> &Progress {
        &self.progress
    }

    /// The last profile [`EncodedAction`](tilo_actions::EncodedAction) an
    /// on-screen control dispatched, for the affordance == `ActionSpec` covenant
    /// test.
    #[doc(hidden)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Show the selected image and install profile the framework resource holds.
    /// An empty projection does not wipe a selection that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::ImagerState) {
        if state.source.is_none() && self.source.is_some() {
            return;
        }
        if state.source.is_some() {
            self.source.clone_from(&state.source);
        }
        let profile_changed = state.profile.hostname != self.profile.hostname
            || state.profile.keyboard != self.profile.keyboard
            || state.profile.timezone != self.profile.timezone
            || state.profile.locale != self.profile.locale
            || state.profile.user.username != self.profile.user.username
            || state.profile.user.full_name != self.profile.user.full_name
            || state.profile.encrypt != self.profile.encrypt;
        if profile_changed {
            self.profile = state.profile.clone();
            self.customize.sync(&self.profile);
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Imager>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    /// Whether startup has finished reconciling an in-flight write helper.
    pub(crate) fn startup_write_reconciled(&self) -> bool {
        self.startup_write_state_reconciled
    }

    /// Whether a write/verify worker channel is open on this surface.
    pub(crate) fn job_active(&self) -> bool {
        self.job_rx.is_some()
    }

    /// Stage the peer-local confirmation for a deterministic offscreen frame.
    #[doc(hidden)]
    pub fn seed_confirmation_for_shot(&mut self, value: &str) {
        self.confirm_input.set_text(value);
        self.shot_confirmation_ready = true;
    }

    /// Headless-shot only: expose the long, searchable time-zone chooser so
    /// its overlay placement is part of visual review.
    pub fn shot_open_customize_timezone(&mut self) {
        self.customize.open_timezone_for_shot();
    }

    /// Prime local-only state for deterministic offscreen review. None of this
    /// device/job presentation data is written into the collaborative doc.
    pub fn seed_local_shot_state(&mut self, scenario: &str) {
        self.targets = if scenario == "drive-empty" || scenario == "source-empty" {
            Vec::new()
        } else {
            crate::model::sample_targets()
        };
        self.releases = crate::model::sample_releases();
        match scenario {
            "drive" => {
                self.selected = Some(0);
                self.step = Step::Drive;
            }
            "customize" | "customize-timezone-open" | "export" => {
                self.selected = Some(0);
                self.step = Step::Customize;
            }
            "confirm" => {
                self.selected = Some(0);
                self.step = Step::Confirm;
            }
            "progress" => {
                self.selected = Some(0);
                self.step = Step::Write;
                self.progress = Progress {
                    phase: JobPhase::Writing,
                    write_frac: 0.62,
                    verify_frac: 0.0,
                    rate_bps: 176_000_000,
                    error: None,
                };
                // A running write can be cancelled; the posed frame shows the
                // control as it is while the helper runs.
                self.job_control = Some(JobControl::new());
            }
            "done" => {
                self.selected = Some(0);
                self.step = Step::Done;
                self.progress = Progress {
                    phase: JobPhase::Success,
                    write_frac: 1.0,
                    verify_frac: 1.0,
                    rate_bps: 0,
                    error: None,
                };
            }
            "done-failed" => {
                self.selected = Some(0);
                self.step = Step::Done;
                self.progress = Progress {
                    phase: JobPhase::Failed,
                    write_frac: 0.36,
                    verify_frac: 0.0,
                    rate_bps: 0,
                    error: Some(
                        "The drive was disconnected while it was being written. Reconnect it, then review and retry."
                            .into(),
                    ),
                };
            }
            "drive-empty" => {
                self.step = Step::Drive;
            }
            "drive-internal" => {
                // The Drive step at rest: nothing selected yet, so the card
                // shows its rest edge rather than the selection ring.
                self.step = Step::Drive;
            }
            _ => {}
        }
        // The unmeasured bands still describe this posed state (the review
        // rows, the failure copy) until the renderer measures them.
        self.estimate_bands();
    }

    /// Local demo inventory/catalogue used by `--demo`; the collaborative
    /// document still carries only the reviewed source/profile records.
    pub fn seed_local_demo_state(&mut self) {
        self.seed_local_shot_state("source");
    }

    fn confirm_matches(&self) -> bool {
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        let confirm = self.confirm_input.value();
        !confirm.is_empty() && (confirm == want || confirm == format!("/dev/{want}"))
    }

    fn image_fits(&self) -> bool {
        match (&self.source, self.target()) {
            (Some(s), Some(t)) => t.fits(s.size),
            _ => false,
        }
    }
}

// =========================================================================
// Accessibility semantics for the immediate-mode canvas
// =========================================================================
impl ImagerApp {
    fn accessible_step_copy(&self) -> (&'static str, String) {
        match self.step {
            Step::Source => (
                "Choose an image",
                if self.releases().is_empty() {
                    "Pick a local ISO or IMG file to write.".into()
                } else {
                    "Pick a local ISO or IMG file, or choose a Tilo release.".into()
                },
            ),
            Step::Drive => (
                "Choose a drive",
                "Only external installation media is available here. The selected drive will be erased."
                    .into(),
            ),
            Step::Customize => (
                "Prepare the setup",
                if self.customizable() {
                    "These non-secret choices travel with the installer and shorten setup on the device."
                        .into()
                } else {
                    "This image will be written unchanged.".into()
                },
            ),
            Step::Confirm => (
                if self.action == Action::Install {
                    "Review and install"
                } else {
                    "Review and write"
                },
                "Review the destructive operation, then type the drive name to unlock it."
                    .into(),
            ),
            Step::Write => (
                if self.progress.phase == JobPhase::Verifying {
                    "Verifying"
                } else {
                    "Writing"
                },
                "Keep the drive connected until writing and verification finish.".into(),
            ),
            Step::Done => {
                let success = self.progress.phase == JobPhase::Success;
                (
                    if success {
                        "The drive is ready"
                    } else {
                        "Writing failed"
                    },
                    if success {
                        "The image was written and verified. You can safely remove the drive."
                            .into()
                    } else {
                        "The drive may be incomplete. Retry before using it.".into()
                    },
                )
            }
        }
    }

    fn hit_target(hit: Hit) -> String {
        match hit {
            Hit::PickFile => "imager:hit:pick-file".into(),
            Hit::Release(index) => format!("imager:hit:release:{index}"),
            Hit::CancelDownload => "imager:hit:cancel-download".into(),
            Hit::Recent(index) => format!("imager:hit:recent:{index}"),
            Hit::Target(index) => format!("imager:hit:target:{index}"),
            Hit::Rescan => "imager:hit:rescan".into(),
            Hit::ConfirmField => "imager:hit:confirm-field".into(),
            Hit::Back => "imager:hit:back".into(),
            Hit::Next => "imager:hit:next".into(),
            Hit::StartWrite => "imager:hit:start-write".into(),
            Hit::CancelWrite => "imager:hit:cancel-write".into(),
            Hit::WriteAnother => "imager:hit:write-another".into(),
        }
    }

    fn hit_from_target(target: &str) -> Option<Hit> {
        let suffix = target.strip_prefix("imager:hit:")?;
        match suffix {
            "pick-file" => Some(Hit::PickFile),
            "rescan" => Some(Hit::Rescan),
            "confirm-field" => Some(Hit::ConfirmField),
            "back" => Some(Hit::Back),
            "next" => Some(Hit::Next),
            "start-write" => Some(Hit::StartWrite),
            "cancel-write" => Some(Hit::CancelWrite),
            "cancel-download" => Some(Hit::CancelDownload),
            "write-another" => Some(Hit::WriteAnother),
            _ => None,
        }
    }

    fn a11y_hit_enabled(&self, hit: Hit) -> bool {
        match hit {
            Hit::Next => match self.step {
                Step::Source | Step::Drive | Step::Customize => self.can_advance(self.step),
                Step::Confirm | Step::Write | Step::Done => false,
            },
            Hit::StartWrite => self.can_advance(Step::Confirm),
            Hit::CancelWrite => self
                .job_control
                .as_ref()
                .is_some_and(|control| !control.is_cancelled()),
            Hit::CancelDownload => self
                .release_control
                .as_ref()
                .is_some_and(|control| !control.is_cancelled()),
            Hit::Release(index) => self.release_rx.is_none() && self.releases.get(index).is_some(),
            Hit::Recent(index) => self.recent.get(index).is_some(),
            Hit::Target(index) => self
                .targets
                .get(index)
                .is_some_and(TargetDrive::is_media_target),
            Hit::PickFile | Hit::Rescan | Hit::ConfirmField | Hit::Back | Hit::WriteAnother => true,
        }
    }

    fn focus_accessible_profile_slot(&mut self, slot: usize) {
        let lay = self.compute_layout();
        self.keyboard_hit = None;
        self.confirm_input.set_focus(false);
        self.customize.focus_keyboard_slot(slot);
        if let Some(rect) = self.customize.keyboard_slot_rect(lay.content, slot) {
            self.ensure_visible(rect, lay.viewport_content);
        }
        self.dirty = true;
    }

    fn a11y_control(
        &self,
        role: ui::a11y::Role,
        name: impl Into<String>,
        rect: Rect,
        hit: Hit,
        disabled: bool,
    ) -> ui::a11y::AccessNode {
        ui::a11y::AccessNode::new(role)
            .with_name(name)
            .with_state(ui::a11y::AccessState {
                focused: self.keyboard_hit == Some(hit)
                    || (hit == Hit::ConfirmField && self.confirm_input.is_focused()),
                focusable: !disabled,
                disabled,
                ..Default::default()
            })
            .with_bounds(rect)
            .with_action_target(Self::hit_target(hit))
    }

    /// A semantic group whose bounds are the union of its children's, so
    /// assistive technology (and the geometry lint) can locate the group and
    /// walk into it instead of meeting a bound-less container.
    fn a11y_group(name: &str, children: Vec<ui::a11y::AccessNode>) -> ui::a11y::AccessNode {
        let bounds = children
            .iter()
            .filter_map(|child| child.bounds.map(ui::a11y::AccessBounds::to_rect))
            .reduce(|a, b| a.union(b));
        let node = ui::a11y::AccessNode::new(ui::a11y::Role::Group)
            .with_name(name)
            .with_children(children);
        match bounds {
            Some(rect) => node.with_bounds(rect),
            None => node,
        }
    }

    /// The stepper: where the wizard is, as a progress indicator.
    fn a11y_stepper(&self, lay: &Layout) -> ui::a11y::AccessNode {
        let labels = stepper_labels();
        let face = StepperFace::new(&labels, self.stepper_current());
        ui::a11y::AccessNode::new(ui::a11y::Role::ProgressBar)
            .with_name(self.step.title())
            .with_value(face.position_text())
            .with_bounds(lay.stepper)
    }

    fn a11y_source_controls(&self, lay: &Layout) -> Vec<ui::a11y::AccessNode> {
        lay.hits
            .iter()
            .filter_map(|(rect, hit)| match *hit {
                Hit::PickFile => Some(self.a11y_control(
                    ui::a11y::Role::Button,
                    "Browse for an image",
                    *rect,
                    *hit,
                    false,
                )),
                Hit::Release(index) => self.releases.get(index).map(|release| {
                    self.a11y_control(
                        ui::a11y::Role::Button,
                        format!("Download {}", release.display_name()),
                        *rect,
                        *hit,
                        false,
                    )
                    .with_description(format!(
                        "{}, {} image",
                        release.target_label(),
                        fmt_size(release.size)
                    ))
                    .with_action_target(format!("imager:release:{}", release.selection_id()))
                }),
                Hit::CancelDownload => Some(
                    self.a11y_control(
                        ui::a11y::Role::Button,
                        "Cancel Tilo installer download",
                        *rect,
                        *hit,
                        self.release_control
                            .as_ref()
                            .is_none_or(crate::release::DownloadControl::is_cancelled),
                    )
                    .with_description("Keeps the verified partial download for a safe resume"),
                ),
                Hit::Recent(index) => self.recent.get(index).map(|source| {
                    self.a11y_control(
                        ui::a11y::Role::Button,
                        format!("Use recent image {}", source.name),
                        *rect,
                        *hit,
                        false,
                    )
                    .with_action_target(format!("imager:recent:{}", source.path))
                }),
                _ => None,
            })
            .collect()
    }

    fn a11y_drive_controls(&self, lay: &Layout) -> Vec<ui::a11y::AccessNode> {
        lay.hits
            .iter()
            .filter_map(|(rect, hit)| match *hit {
                Hit::Rescan => Some(self.a11y_control(
                    ui::a11y::Role::Button,
                    "Rescan drives",
                    *rect,
                    *hit,
                    false,
                )),
                Hit::Target(index) => self.targets.get(index).map(|drive| {
                    self.a11y_control(
                        ui::a11y::Role::RadioButton,
                        format!(
                            "{} on {}, {}",
                            drive.model,
                            drive.dev_path(),
                            fmt_size(drive.size)
                        ),
                        *rect,
                        *hit,
                        false,
                    )
                    .with_description(format!(
                        "External drive. Erases {}",
                        drive.contents_summary()
                    ))
                    .with_action_target(format!("imager:drive:{}", drive.name))
                    .with_state(ui::a11y::AccessState {
                        focused: self.keyboard_hit == Some(*hit),
                        focusable: true,
                        checked: Some(self.selected == Some(index)),
                        selected: self.selected == Some(index),
                        ..Default::default()
                    })
                }),
                _ => None,
            })
            .collect()
    }

    fn a11y_footer_controls(&self, lay: &Layout) -> Vec<ui::a11y::AccessNode> {
        let mut controls = Vec::new();
        if let Some(rect) = lay.back {
            controls.push(self.a11y_control(
                ui::a11y::Role::Button,
                "Back",
                rect,
                Hit::Back,
                false,
            ));
        }
        if let Some(rect) = lay.primary {
            let (hit, name, disabled) = match self.step {
                Step::Source => (
                    Hit::Next,
                    "Continue".into(),
                    !self.can_advance(Step::Source),
                ),
                Step::Drive => (Hit::Next, "Continue".into(), !self.can_advance(Step::Drive)),
                Step::Customize => (Hit::Next, "Continue".into(), false),
                Step::Confirm => (
                    Hit::StartWrite,
                    self.target().map_or_else(
                        || "Erase and write selected drive".into(),
                        |drive| format!("Erase and write {}", drive.dev_path()),
                    ),
                    !self.can_advance(Step::Confirm),
                ),
                Step::Done => (
                    Hit::WriteAnother,
                    if self.progress.phase == JobPhase::Failed {
                        "Review and retry".into()
                    } else {
                        "Start over".into()
                    },
                    false,
                ),
                Step::Write => (
                    Hit::CancelWrite,
                    "Cancel image write".into(),
                    self.job_control
                        .as_ref()
                        .is_none_or(JobControl::is_cancelled),
                ),
            };
            controls.push(self.a11y_control(ui::a11y::Role::Button, name, rect, hit, disabled));
        }
        controls
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the semantic tree mirrors the six mutually exclusive immediate-mode wizard screens in one auditable order"
    )]
    fn accessibility_tree(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};

        let lay = self.compute_layout();
        let (title, description) = self.accessible_step_copy();
        let status_lower = self.status.to_ascii_lowercase();
        let status_is_error = self.progress.phase == JobPhase::Failed
            || [
                "failed",
                "unavailable",
                "invalid",
                "refused",
                "must ",
                "larger",
            ]
            .iter()
            .any(|word| status_lower.contains(word));
        // The step's title line; the spoken description shares its band.
        let title_band = Rect::new(
            lay.content.x0,
            lay.content.y0,
            lay.content.x1,
            lay.content.y0 + self.bands.title_h,
        );
        let mut children = vec![
            self.a11y_stepper(&lay).with_hashed_key("step-navigation"),
            AccessNode::new(Role::Heading { level: 1 })
                .with_name(title)
                .with_hashed_key("step-heading")
                .with_bounds(title_band),
            AccessNode::new(Role::Paragraph)
                .with_value(description)
                .with_hashed_key("step-description")
                .with_bounds(title_band),
        ];
        if !self.status.is_empty() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Status")
                    .with_value(self.status.clone())
                    .with_hashed_key("status")
                    .with_live(if status_is_error {
                        Politeness::Assertive
                    } else {
                        Politeness::Polite
                    }),
            );
        }

        match self.step {
            Step::Source => {
                if let Some(source) = &self.source {
                    children.push(
                        AccessNode::new(Role::StaticText)
                            .with_name("Selected image")
                            .with_value(format!(
                                "{}, {}, {}",
                                source.name,
                                fmt_size(source.size),
                                if self.source_is_official(source) {
                                    "Official Tilo release, verified"
                                } else {
                                    "Local image, written unchanged"
                                }
                            ))
                            .with_hashed_key("selected-image"),
                    );
                }
                children.push(
                    Self::a11y_group("Image sources", self.a11y_source_controls(&lay))
                        .with_hashed_key("source-controls"),
                );
            }
            Step::Drive => children.push(
                Self::a11y_group(
                    "External destination drives",
                    self.a11y_drive_controls(&lay),
                )
                .with_hashed_key("drive-controls"),
            ),
            Step::Customize if self.customizable() => children.push(
                Self::a11y_group("Setup profile", self.customize.accessibility(lay.content))
                    .with_hashed_key("profile-controls"),
            ),
            Step::Customize => children.push(
                AccessNode::new(Role::StaticText)
                    .with_value("Pre-configuration is only available for Tilo images.")
                    .with_hashed_key("profile-unavailable"),
            ),
            Step::Confirm => {
                if let (Some(source), Some(target)) = (&self.source, self.target()) {
                    children.push(
                        AccessNode::new(Role::Group)
                            .with_name("Write review")
                            .with_hashed_key("write-review")
                            .with_children(vec![
                                AccessNode::new(Role::StaticText)
                                    .with_name("Image")
                                    .with_value(format!("{}, {}", source.name, fmt_size(source.size))),
                                AccessNode::new(Role::StaticText)
                                    .with_name("Drive")
                                    .with_value(format!("{} on {}, {}", target.model, target.dev_path(), fmt_size(target.size))),
                                AccessNode::new(Role::StaticText)
                                    .with_name("Warning")
                                    .with_value("This permanently erases the selected drive and cannot be undone."),
                            ]),
                    );
                }
                if self.offers_write_setup() {
                    children.push(
                        Self::a11y_group(
                            "Setup on write",
                            self.write_setup.accessibility(
                                lay.content,
                                self.write_setup_top(lay.content),
                                self.profile.enroll_local_identity,
                            ),
                        )
                        .with_hashed_key("write-setup"),
                    );
                }
                if let Some((rect, _)) = lay.hits.iter().find(|(_, hit)| *hit == Hit::ConfirmField)
                {
                    let expected = self
                        .target()
                        .map_or("the drive name", |drive| drive.name.as_str());
                    children.push(
                        AccessNode::new(Role::TextInput)
                            .with_name("Confirm the target device")
                            .with_description(format!(
                                "Type {expected} to unlock the destructive write"
                            ))
                            .with_value(self.confirm_input.value())
                            .with_state(AccessState {
                                focused: self.confirm_input.is_focused(),
                                focusable: true,
                                required: true,
                                invalid: !self.confirm_input.value().is_empty()
                                    && !self.confirm_matches(),
                                ..Default::default()
                            })
                            .with_bounds(*rect)
                            .with_action_target(Self::hit_target(Hit::ConfirmField)),
                    );
                }
            }
            Step::Write => children.push(
                AccessNode::new(Role::Group)
                    .with_name("Write progress")
                    .with_hashed_key("write-progress")
                    .with_children(vec![
                        AccessNode::new(Role::ProgressBar)
                            .with_name(if self.progress.phase == JobPhase::Verifying {
                                "Verifying image"
                            } else {
                                "Writing image"
                            })
                            .with_value(format!("{:.0} percent", self.progress.overall() * 100.0))
                            .with_state(AccessState::new().busy(true))
                            .with_hashed_key("overall")
                            .with_live(Politeness::Polite),
                        AccessNode::new(Role::ProgressBar)
                            .with_name("Write phase")
                            .with_value(format!("{:.0} percent", self.progress.write_frac * 100.0))
                            .with_hashed_key("write"),
                        AccessNode::new(Role::ProgressBar)
                            .with_name("Verify phase")
                            .with_value(format!("{:.0} percent", self.progress.verify_frac * 100.0))
                            .with_hashed_key("verify"),
                    ]),
            ),
            Step::Done => children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(if self.progress.phase == JobPhase::Success {
                        "Write completed and verified"
                    } else {
                        "Write failure details"
                    })
                    .with_value(
                        self.progress
                            .error
                            .clone()
                            .unwrap_or_else(|| self.status.clone()),
                    )
                    .with_hashed_key("write-result")
                    .with_live(if self.progress.phase == JobPhase::Success {
                        Politeness::Polite
                    } else {
                        Politeness::Assertive
                    }),
            ),
        }
        let footer = self.a11y_footer_controls(&lay);
        if !footer.is_empty() {
            children
                .push(Self::a11y_group("Wizard actions", footer).with_hashed_key("wizard-actions"));
        }
        AccessNode::new(Role::Window)
            .with_name("Disk Imager")
            .with_children(children)
    }
}

/// Whether this process should enumerate real block devices. The live binary
/// sets `IMAGER_LIVE=1`; `--shots`, `--demo` and tests leave it unset so the
/// Whether this process should enumerate real block devices. The live binary
/// sets `IMAGER_LIVE=1`; `--shots`, `--demo` and tests leave it unset so the
/// document's seeded/posed state is authoritative and no `lsblk` runs.
fn live_probe_enabled() -> bool {
    std::env::var("IMAGER_LIVE").is_ok()
}

/// One line of `style`, as the renderer that paints it measures it.
fn line_of(tr: &mut TextRenderer, style: &TextStyle) -> f64 {
    f64::from(tr.measure_style("Ag", None, style).1)
}

/// The height of `text` in `style` wrapped at `width`.
fn para_of(tr: &mut TextRenderer, text: &str, style: &TextStyle, width: f64) -> f64 {
    f64::from(tr.measure_style(text, Some(width.max(1.0) as f32), style).1)
}

/// The step titles the stepper shows.
fn stepper_labels() -> [&'static str; 6] {
    Step::all().map(|step| step.title())
}

// =========================================================================
// Measurement
// =========================================================================
impl ImagerApp {
    /// Every band at its first, unscaled measurement, so a layout taken
    /// before the host's renderer has measured still places every block on a
    /// real band.
    fn estimate_bands(&mut self) {
        let width = if self.rect.width() > 0.0 {
            self.rect.width()
        } else {
            READABLE_WIDTH
        };
        let bands = PROBE_RENDERER
            .with(|renderer| self.measure_bands(&mut renderer.borrow_mut(), &self.theme, width));
        self.bands = bands;
    }

    /// Every band for a window `width` wide, from the renderer and theme that
    /// paint them.
    fn measure_bands(&self, tr: &mut TextRenderer, theme: &Theme, width: f64) -> Bands {
        let gaps = Gaps::of(&theme.tokens.spacing);
        let mut bands = Bands {
            gaps,
            text_scale: f64::from(theme.text_scale),
            ui_scale: f64::from(theme.ui_scale),
            ..Bands::default()
        };
        // The column width depends on the scales measured above.
        let probe = Self::column_width(&bands, width);
        let content_w = probe;
        let compact = crate::compact_content(content_w, bands.text_scale);
        let labels = stepper_labels();
        bands.stepper_h = StepperFace::new(&labels, self.step.index()).height(tr, theme, content_w);
        bands.title_h = line_of(tr, &theme.text(TextRole::Title));
        bands.control_h = ButtonFace::height(ControlSize::Regular, theme);
        bands.small_control_h = ButtonFace::height(ControlSize::Small, theme);
        bands.back_w = ButtonFace::new(Rect::ZERO, "Back")
            .icon(Icon::ChevronLeft)
            .intrinsic_width(tr, theme);
        bands.primary_w = PRIMARY_LABELS
            .iter()
            .map(|label| ButtonFace::new(Rect::ZERO, label).intrinsic_width(tr, theme))
            .fold(0.0, f64::max);
        bands.rescan_w = ButtonFace::new(Rect::ZERO, RESCAN_LABEL)
            .icon(Icon::Refresh)
            .control_size(ControlSize::Small)
            .intrinsic_width(tr, theme);
        bands.caption_h = line_of(tr, &theme.text(TextRole::Caption));
        bands.label_h = line_of(tr, &theme.text(TextRole::Label));
        bands.body_h = line_of(tr, &theme.text(TextRole::Body));
        bands.section_h = line_of(tr, &theme.text(TextRole::Subheading));
        bands.row_h = ListRowFace::new("Ag").secondary("Ag").height(tr, theme);
        bands.dropzone_h = DropZoneFace::new(DROP_TITLE).message(DROP_MESSAGE).height(
            tr,
            theme,
            content_w,
            bands.control_h,
        );
        bands.plain_h = para_of(tr, PLAIN_BODY, &theme.text(TextRole::Body), content_w);
        let review = self.review_rows();
        let rows: Vec<PropertyRow<'_>> = review
            .iter()
            .map(|(label, value)| PropertyRow::new(label, value))
            .collect();
        bands.review_h = PropertyListFace::new(&rows).height(tr, theme, content_w);
        bands.banner_h = Self::banner_height(tr, theme, content_w, &self.confirm_warning());
        bands.ring_d = ProgressRingSize::Large.diameter(theme);
        bands.headline_h = line_of(tr, &theme.text(TextRole::LargeTitle));
        let body = theme.text(TextRole::Body);
        bands.done_sub_h = if self.progress.phase == JobPhase::Success {
            let (_, sub) = Self::done_ok_copy(self.action == Action::Install, compact);
            para_of(tr, sub, &body, content_w)
        } else {
            2.0 * line_of(tr, &body)
        };
        let summary = self.summary_rows();
        let rows: Vec<PropertyRow<'_>> = summary
            .iter()
            .map(|(label, value)| PropertyRow::new(label, value))
            .collect();
        bands.summary_h = PropertyListFace::new(&rows).height(tr, theme, content_w);
        bands
    }

    /// The erase warning's banner height at `width`: the toolkit banner's own
    /// measure of its title, message and insets.
    fn banner_height(
        tr: &mut TextRenderer,
        theme: &Theme,
        width: f64,
        warning: &(String, &str),
    ) -> f64 {
        let mut banner = Banner::new(BannerKind::Error, warning.0.clone()).with_message(warning.1);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut nodes = HashMap::new();
        let _ = banner.build_layout(&mut taffy, theme, &mut nodes);
        let size = Widget::measure(
            &mut banner,
            taffy::Size {
                width: Some(width as f32),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width as f32),
                height: taffy::AvailableSpace::MaxContent,
            },
            tr,
        );
        f64::from(size.height)
    }
}

// =========================================================================
// Layout
// =========================================================================
impl ImagerApp {
    /// The page gutter for a window `width` wide.
    fn gutter(bands: &Bands, width: f64) -> f64 {
        let g = bands.gaps;
        if crate::compact_content(width - 2.0 * g.group, bands.text_scale) {
            g.section
        } else {
            g.group
        }
    }

    /// The content column's width for a window `width` wide.
    fn column_width(bands: &Bands, width: f64) -> f64 {
        (width - 2.0 * Self::gutter(bands, width))
            .min(READABLE_WIDTH * bands.ui_scale.max(1.0) * bands.text_scale.max(1.0))
            .max(0.0)
    }

    /// The content column of the window, left to right.
    fn column(&self) -> (f64, f64) {
        let v = self.rect;
        let w = Self::column_width(&self.bands, v.width());
        let x0 = v.x0 + (v.width() - w) / 2.0;
        (x0, x0 + w)
    }

    /// Whether `content` takes the steps' compact form; see
    /// [`crate::compact_content`].
    fn compact_content(&self, content: Rect) -> bool {
        crate::compact_content(content.width(), self.bands.text_scale)
    }

    /// How far a list row reaches past the content column, so its title
    /// lines up with the column's headings while its lift has room.
    fn row_outset(&self, content: Rect) -> f64 {
        if self.compact_content(content) {
            0.0
        } else {
            self.bands.gaps.sibling
        }
    }

    fn row_rect(&self, content: Rect, y: f64) -> Rect {
        let out = self.row_outset(content);
        Rect::new(content.x0 - out, y, content.x1 + out, y + self.bands.row_h)
    }

    fn stepper_rect(&self) -> Rect {
        let (x0, x1) = self.column();
        let y0 = self.rect.y0 + self.bands.gaps.group;
        Rect::new(x0, y0, x1, y0 + self.bands.stepper_h)
    }

    fn footer_h(&self) -> f64 {
        self.bands.control_h + 2.0 * self.bands.gaps.section
    }

    fn footer_top(&self) -> f64 {
        self.rect.y1 - self.footer_h()
    }

    fn content_viewport_rect(&self) -> Rect {
        let (x0, x1) = self.column();
        let top = self.stepper_rect().y1 + self.bands.gaps.region;
        let bottom = (self.footer_top() - self.bands.gaps.section).max(top);
        Rect::new(x0, top, x1, bottom)
    }

    /// Where the step's first block starts below its title.
    fn body_top(&self, content: Rect) -> f64 {
        content.y0 + self.bands.title_h + self.bands.gaps.section
    }

    /// The step body's height at `viewport`'s width: every block measured,
    /// plus the bottom margin, never shorter than the viewport.
    fn content_extent(&self, viewport: Rect) -> f64 {
        let at = Rect::new(viewport.x0, 0.0, viewport.x1, f64::INFINITY);
        let bottom = match self.step {
            Step::Source => self.layout_source(at, &mut Vec::new()),
            Step::Drive => self.layout_drive(at, &mut Vec::new()),
            Step::Customize => {
                if self.customizable() {
                    self.customize.extent(at)
                } else {
                    self.body_top(at) + self.bands.plain_h
                }
            }
            Step::Confirm => self.confirm_note_top(at) + self.bands.caption_h,
            Step::Write => self.write_foot_top(at) + self.bands.caption_h,
            Step::Done => self.done_summary_rect(at).y1,
        };
        viewport.height().max(bottom + self.bands.gaps.section)
    }

    fn max_scroll(&self) -> f64 {
        let viewport = self.content_viewport_rect();
        (self.content_extent(viewport) - viewport.height()).max(0.0)
    }

    fn content_rect(&self) -> Rect {
        let viewport = self.content_viewport_rect();
        let extent = self.content_extent(viewport);
        Rect::new(
            viewport.x0,
            viewport.y0 - self.scroll_y,
            viewport.x1,
            viewport.y0 - self.scroll_y + extent,
        )
    }

    fn compute_layout(&self) -> Layout {
        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        let content = self.content_rect();
        match self.step {
            Step::Source => {
                self.layout_source(content, &mut hits);
            }
            Step::Drive => {
                self.layout_drive(content, &mut hits);
            }
            Step::Confirm => hits.push((self.confirm_field_rect(content), Hit::ConfirmField)),
            // Customize's fields are owned by `CustomizePanel`, which
            // hit-tests its own rects in `handle_event`; Write (in progress)
            // and Done have no interactive content beyond the footer.
            Step::Customize | Step::Write | Step::Done => {}
        }
        let (back, primary) = self.footer_buttons(content);
        if let Some(back) = back {
            hits.push((back, Hit::Back));
        }
        hits.push((
            primary,
            match self.step {
                Step::Confirm => Hit::StartWrite,
                Step::Write => Hit::CancelWrite,
                Step::Done => Hit::WriteAnother,
                _ => Hit::Next,
            },
        ));
        Layout {
            stepper: self.stepper_rect(),
            back,
            primary: Some(primary),
            hits,
            viewport_content: self.content_viewport_rect(),
            content,
        }
    }

    fn has_back(&self) -> bool {
        matches!(self.step, Step::Drive | Step::Customize | Step::Confirm)
    }

    /// The footer's Back and primary buttons: as wide as their labels at the
    /// column's edges, or sharing the column when it is compact.
    fn footer_buttons(&self, content: Rect) -> (Option<Rect>, Rect) {
        let b = &self.bands;
        let y0 = self.footer_top() + b.gaps.section;
        let y1 = y0 + b.control_h;
        if self.has_back() && self.compact_content(content) {
            let half = (content.width() - b.gaps.sibling) / 2.0;
            return (
                Some(Rect::new(content.x0, y0, content.x0 + half, y1)),
                Rect::new(content.x1 - half, y0, content.x1, y1),
            );
        }
        let primary_w = b.primary_w.min(content.width());
        let back = self
            .has_back()
            .then(|| Rect::new(content.x0, y0, content.x0 + b.back_w, y1));
        (back, Rect::new(content.x1 - primary_w, y0, content.x1, y1))
    }

    // ---- Image -------------------------------------------------------------

    /// Whether a release or a shown recent entry already presents the
    /// selected image, so it is not shown twice.
    fn source_listed(&self, source: &Source) -> bool {
        self.releases().iter().any(|r| r.matches_source(source))
            || self.shown_recent().iter().any(|i| {
                self.recent
                    .get(*i)
                    .is_some_and(|r| r.path == source.path && r.name == source.name)
            })
    }

    /// The recent images worth offering: the last few that are not one of
    /// the releases listed above them.
    fn shown_recent(&self) -> Vec<usize> {
        self.recent
            .iter()
            .enumerate()
            .filter(|(_, recent)| !self.releases().iter().any(|r| r.matches_source(recent)))
            .map(|(i, _)| i)
            .take(3)
            .collect()
    }

    /// The Image step's blocks in reading order.
    fn source_blocks(&self) -> Vec<SourceBlock> {
        let mut blocks = vec![SourceBlock::DropZone];
        if let Some(source) = &self.source
            && !self.source_listed(source)
        {
            blocks.push(SourceBlock::Header("Selected image"));
            blocks.push(SourceBlock::Selected);
        }
        blocks.push(SourceBlock::Header("Tilo releases"));
        if self.releases().is_empty() {
            blocks.push(SourceBlock::NoReleases);
        }
        blocks.extend((0..self.releases().len()).map(SourceBlock::Release));
        let recent = self.shown_recent();
        if !recent.is_empty() {
            blocks.push(SourceBlock::Header("Recent"));
            blocks.extend(recent.into_iter().map(SourceBlock::Recent));
        }
        blocks
    }

    /// Every block of the Image step with its band, from `content`'s top.
    fn source_bands(&self, content: Rect) -> Vec<(SourceBlock, Rect)> {
        let b = &self.bands;
        let mut y = self.body_top(content);
        let mut previous: Option<SourceBlock> = None;
        let mut out = Vec::new();
        for block in self.source_blocks() {
            y += match (previous, block) {
                (Some(_), SourceBlock::Header(_)) => b.gaps.group,
                (Some(SourceBlock::Header(_)), _) => b.gaps.related,
                _ => 0.0,
            };
            let rect = match block {
                SourceBlock::DropZone => Rect::new(content.x0, y, content.x1, y + b.dropzone_h),
                SourceBlock::Header(_) => Rect::new(content.x0, y, content.x1, y + b.section_h),
                SourceBlock::NoReleases => Rect::new(content.x0, y, content.x1, y + b.body_h),
                SourceBlock::Selected | SourceBlock::Release(_) | SourceBlock::Recent(_) => {
                    self.row_rect(content, y)
                }
            };
            y = rect.y1;
            out.push((block, rect));
            previous = Some(block);
        }
        out
    }

    /// Hit targets of the Image step on the bands the painters fill.
    /// Returns the y just under the last block.
    fn layout_source(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) -> f64 {
        let mut bottom = self.body_top(content);
        for (block, rect) in self.source_bands(content) {
            match block {
                SourceBlock::DropZone => hits.push((rect, Hit::PickFile)),
                SourceBlock::Release(i) => {
                    if self.release_download_index == Some(i) {
                        hits.push((rect, Hit::CancelDownload));
                    } else if self.release_rx.is_none() {
                        hits.push((rect, Hit::Release(i)));
                    }
                }
                SourceBlock::Recent(i) => hits.push((rect, Hit::Recent(i))),
                SourceBlock::Header(_) | SourceBlock::Selected | SourceBlock::NoReleases => {}
            }
            bottom = rect.y1;
        }
        bottom
    }

    // ---- Drive -------------------------------------------------------------

    /// The Rescan button, on the title's line at the column's trailing edge.
    fn drive_rescan_rect(&self, content: Rect) -> Rect {
        let b = &self.bands;
        let cy = content.y0 + b.title_h / 2.0;
        Rect::new(
            content.x1 - b.rescan_w,
            cy - b.small_control_h / 2.0,
            content.x1,
            cy + b.small_control_h / 2.0,
        )
    }

    /// The empty state's area: the rest of the visible body.
    fn drive_empty_rect(&self, content: Rect) -> Rect {
        let top = self.body_top(content);
        let viewport = self.content_viewport_rect();
        let bottom =
            (viewport.y1 - self.scroll_y - (viewport.y0 - content.y0 - self.scroll_y).max(0.0))
                .max(top + self.bands.title_h * 4.0);
        Rect::new(
            content.x0,
            top,
            content.x1,
            bottom.min(top + viewport.height()),
        )
    }

    fn layout_drive(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) -> f64 {
        hits.push((self.drive_rescan_rect(content), Hit::Rescan));
        let visible = self.visible_targets();
        if visible.is_empty() {
            return self.body_top(content) + self.bands.title_h * 4.0;
        }
        let mut y = self.body_top(content);
        for (idx, _) in visible {
            let r = self.row_rect(content, y);
            hits.push((r, Hit::Target(idx)));
            y = r.y1;
        }
        y
    }

    // ---- Confirm -----------------------------------------------------------

    fn confirm_review_rect(&self, content: Rect) -> Rect {
        let top = self.body_top(content);
        Rect::new(content.x0, top, content.x1, top + self.bands.review_h)
    }

    fn confirm_banner_rect(&self, content: Rect) -> Rect {
        let top = self.confirm_review_rect(content).y1 + self.bands.gaps.group;
        Rect::new(content.x0, top, content.x1, top + self.bands.banner_h)
    }

    /// Origin of the optional on-write setup form (Wi-Fi / enrollment).
    fn write_setup_top(&self, content: Rect) -> f64 {
        self.confirm_banner_rect(content).y1 + self.bands.gaps.group
    }

    /// The typed device-name confirmation field. It follows the warning (and
    /// the optional write-setup form) in reading order — review, warning,
    /// optional network/identity, type the name, then the footer's primary
    /// action — under its caption.
    fn confirm_field_rect(&self, content: Rect) -> Rect {
        let b = &self.bands;
        let after = if self.offers_write_setup() {
            let top = self.write_setup_top(content);
            top + self.write_setup.extent(content, top)
        } else {
            self.confirm_banner_rect(content).y1 + b.gaps.group
        };
        let top = after + b.label_h + b.gaps.tight;
        Rect::new(content.x0, top, content.x1, top + b.control_h)
    }

    fn confirm_note_top(&self, content: Rect) -> f64 {
        self.confirm_field_rect(content).y1 + self.bands.gaps.tight
    }

    /// The facts the Confirm step reviews, in reading order.
    fn review_rows(&self) -> Vec<(&'static str, String)> {
        let mut rows = vec![(
            "Image",
            self.source
                .as_ref()
                .map_or_else(|| "None selected".into(), |s| s.name.clone()),
        )];
        if let Some(source) = &self.source {
            rows.push(("Image size", fmt_size(source.size)));
        }
        rows.push((
            "Drive",
            self.target()
                .map_or_else(|| "None selected".into(), |d| d.model.clone()),
        ));
        if let Some(drive) = self.target() {
            rows.push(("Device", drive.dev_path()));
        }
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            rows.push((
                "Space",
                if tgt.fits(src.size) {
                    format!(
                        "{} left on the drive afterwards",
                        fmt_size(tgt.size.saturating_sub(src.size))
                    )
                } else {
                    "The image is larger than the drive".into()
                },
            ));
        }
        rows.push(("Setup", self.review_setup()));
        rows
    }

    /// What the write sets up, as one sentence.
    fn review_setup(&self) -> String {
        let mut parts = if self.customizable() {
            vec![format!(
                "Pre-configured for {} on {}",
                self.profile.user.username, self.profile.hostname
            )]
        } else {
            vec!["Written unchanged, then verified".to_string()]
        };
        if !self.profile.wifi_ssid.trim().is_empty() {
            parts.push(format!("joins {}", self.profile.wifi_ssid.trim()));
        }
        if self.profile.enroll_local_identity {
            let label = self.profile.device_label.trim();
            if label.is_empty() {
                parts.push("enrolled as a new device".into());
            } else {
                parts.push(format!("enrolled as {label}"));
            }
        }
        parts.join(", ")
    }

    /// The erase warning: louder for a fixed or system disk.
    fn confirm_warning(&self) -> (String, &'static str) {
        let device = self
            .target()
            .map_or_else(|| "the drive".to_string(), TargetDrive::dev_path);
        let system = self.target().is_some_and(|d| d.is_system);
        let internal = self.target().is_some_and(|d| !d.removable);
        let title = if system {
            "This is your system disk".to_string()
        } else if internal {
            format!("Everything on the internal disk {device} is erased")
        } else {
            format!("Everything on {device} is erased")
        };
        let message = if system {
            "Everything on it, the running system included, is erased. This cannot be undone."
        } else {
            "This cannot be undone."
        };
        (title, message)
    }

    /// The note under the confirmation field. Shot mode poses the field as
    /// ready for documentation, but never enables the backend.
    fn confirm_note(&self) -> &'static str {
        if !(crate::write::write_available() || self.shot_confirmation_ready) {
            "Writing is turned off in this preview."
        } else if self.confirm_matches() {
            "The name matches. Erase and write when you are ready."
        } else {
            "Type the device name exactly as shown above."
        }
    }

    // ---- Write -------------------------------------------------------------

    fn write_ring_rect(&self, content: Rect) -> Rect {
        let d = self.bands.ring_d;
        let top = self.body_top(content) + self.bands.gaps.group;
        let cx = content.center().x;
        Rect::new(cx - d / 2.0, top, cx + d / 2.0, top + d)
    }

    fn write_meta_top(&self, content: Rect) -> f64 {
        self.write_ring_rect(content).y1 + self.bands.gaps.section
    }

    /// The top of the first phase row (its label line, then its bar).
    fn write_bars_top(&self, content: Rect) -> f64 {
        self.write_meta_top(content) + self.bands.caption_h + self.bands.gaps.region
    }

    fn write_bar_pitch(&self) -> f64 {
        let b = &self.bands;
        b.label_h + b.gaps.related + b.gaps.related + b.gaps.group
    }

    fn write_foot_top(&self, content: Rect) -> f64 {
        self.write_bars_top(content) + 2.0 * self.write_bar_pitch()
    }

    // ---- Done --------------------------------------------------------------

    fn done_icon_rect(&self, content: Rect) -> Rect {
        let side = self.bands.title_h * 2.0;
        let top = content.y0 + self.bands.gaps.region;
        let cx = content.center().x;
        Rect::new(cx - side / 2.0, top, cx + side / 2.0, top + side)
    }

    fn done_headline_top(&self, content: Rect) -> f64 {
        self.done_icon_rect(content).y1 + self.bands.gaps.group
    }

    fn done_sub_top(&self, content: Rect) -> f64 {
        self.done_headline_top(content) + self.bands.headline_h + self.bands.gaps.related
    }

    fn done_summary_rect(&self, content: Rect) -> Rect {
        let top = self.done_sub_top(content) + self.bands.done_sub_h + self.bands.gaps.region;
        Rect::new(content.x0, top, content.x1, top + self.bands.summary_h)
    }

    /// The Done step's summary of what was written where.
    fn summary_rows(&self) -> Vec<(&'static str, String)> {
        let (Some(src), Some(tgt)) = (&self.source, self.target()) else {
            return Vec::new();
        };
        vec![
            ("Image", src.name.clone()),
            ("Drive", tgt.model.clone()),
            ("Device", tgt.dev_path()),
        ]
    }

    /// The success hero's headline and explanation, shared by the painter and
    /// the band measurement.
    fn done_ok_copy(install: bool, compact: bool) -> (&'static str, &'static str) {
        match (install, compact) {
            (true, false) => (
                "Tilo is installed",
                "The disk was written and verified. Remove the installation media and restart.",
            ),
            (true, true) => (
                "Tilo is installed",
                "Written and verified. Remove the media and restart.",
            ),
            (false, false) => (
                "The drive is ready",
                "The image was written and verified. You can safely remove the drive.",
            ),
            (false, true) => (
                "The drive is ready",
                "Written and verified. You can safely remove the drive.",
            ),
        }
    }

    /// Wrap a terminal failure into at most `max_lines` measured lines of
    /// `style`. Error strings can contain long device paths or platform
    /// diagnostics, so every returned line is measured in the style that
    /// paints it and is guaranteed to fit `max_width`.
    fn failure_detail_lines(
        tr: &mut TextRenderer,
        style: &TextStyle,
        detail: &str,
        max_width: f32,
        max_lines: usize,
    ) -> Vec<String> {
        let width = |tr: &mut TextRenderer, s: &str| tr.measure_style(s, None, style).0;
        let mut remaining = detail.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut lines = Vec::new();
        while !remaining.is_empty() && lines.len() < max_lines {
            if width(tr, &remaining) <= max_width {
                lines.push(remaining);
                break;
            }
            let last_line = lines.len() + 1 == max_lines;
            let ellipsis_width = if last_line { width(tr, "…") } else { 0.0 };
            let allowed = (max_width - ellipsis_width).max(1.0);
            let mut fitting_end = 0;
            let mut whitespace_end = None;
            for (index, character) in remaining.char_indices() {
                let end = index + character.len_utf8();
                let Some(candidate) = remaining.get(..end) else {
                    break;
                };
                if width(tr, candidate) > allowed {
                    break;
                }
                fitting_end = end;
                if character.is_whitespace() {
                    whitespace_end = Some(index);
                }
            }
            if fitting_end == 0 {
                // Even one glyph does not fit; returning no line is safer than
                // painting outside the column at extreme accessibility scales.
                break;
            }
            let split_at = whitespace_end
                .filter(|index| *index > 0)
                .unwrap_or(fitting_end);
            let Some(prefix) = remaining.get(..split_at) else {
                break;
            };
            let mut line = prefix.trim_end().to_string();
            if last_line {
                line.push('…');
                lines.push(line);
                break;
            }
            lines.push(line);
            let Some(rest) = remaining.get(split_at..) else {
                break;
            };
            remaining = rest.trim_start().to_string();
        }
        lines
    }
}

// =========================================================================
// Chrome: stepper, footer, scroll bar
// =========================================================================
impl ImagerApp {
    /// The station the stepper marks: every step before it is done. A
    /// verified result completes the last station too.
    fn stepper_current(&self) -> usize {
        if self.step == Step::Done && self.progress.phase == JobPhase::Success {
            Step::all().len()
        } else {
            self.step.index()
        }
    }

    fn draw_stepper(&self, ctx: &mut PaintCtx<'_>, lay: &Layout) {
        let labels = stepper_labels();
        StepperFace::new(&labels, self.stepper_current()).paint(ctx, lay.stepper);
    }

    fn draw_scrollbar(&self, scene: &mut Scene, theme: &Theme, t: Affine, lay: &Layout) {
        let max = self.max_scroll();
        if max <= 0.0 {
            return;
        }
        let viewport = lay.viewport_content;
        let b = &self.bands;
        let x0 = viewport.x1 + self.row_outset(viewport) + b.gaps.tight;
        let track = Rect::new(
            x0,
            viewport.y0,
            x0 + f64::from(theme.tokens.metrics.scrollbar_gutter) / 2.0,
            viewport.y1,
        );
        Scrollbar::vertical().draw(
            scene,
            t,
            track,
            ScrollMetrics::new(viewport.height() + max, viewport.height(), self.scroll_y),
            theme,
            false,
        );
    }

    fn draw_footer(&self, ctx: &mut PaintCtx<'_>, lay: &Layout) {
        let v = self.rect;
        let fy = self.footer_top();
        let ground = ctx.theme.tokens.colors.surfaces.content;
        surface::separator(ctx.scene, ctx.transform, v.x0, v.x1, fy, 0.0, ground);

        if let Some(r) = lay.back {
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &ButtonFace::new(r, "Back")
                    .icon(Icon::ChevronLeft)
                    .hovered(r.contains(self.last_mouse))
                    .focused(self.keyboard_hit == Some(Hit::Back)),
            );
        }
        if let Some(r) = lay.primary {
            self.draw_primary(ctx, r);
        }

        // Between the two: what is still needed, or what just happened.
        let content = lay.content;
        if self.compact_content(content) {
            return;
        }
        let text = if self.status.is_empty() {
            self.footer_hint()
        } else {
            Some(self.status.clone())
        };
        let Some(text) = text else {
            return;
        };
        let b = &self.bands;
        let left = lay.back.map_or(content.x0, |r| r.x1 + b.gaps.group);
        let right = lay.primary.map_or(content.x1, |r| r.x0 - b.gaps.group);
        let room = right - left;
        if room <= 0.0 {
            return;
        }
        let style = ctx.theme.text(TextRole::Caption);
        let shown = ctx.text.elide_to_width_style(&text, room as f32, &style);
        let w = f64::from(ctx.text.measure_style(&shown, None, &style).0);
        let top = centered_text_top(fy, self.footer_h(), b.caption_h);
        let dim = ctx.theme.text_dim;
        ctx.text(
            &shown,
            &style,
            dim,
            Point::new(left + (room - w) / 2.0, top),
            None,
        );
    }

    fn footer_hint(&self) -> Option<String> {
        match self.step {
            Step::Source if self.source.is_none() => Some("Choose an image to continue".into()),
            Step::Drive if self.selected.is_none() => Some("Choose a drive to continue".into()),
            Step::Confirm if !self.confirm_matches() => self
                .target()
                .map(|d| format!("Type {} to unlock writing", d.name)),
            Step::Write => Some("Keep the drive connected until this finishes".into()),
            _ => None,
        }
    }

    fn draw_primary(&self, ctx: &mut PaintCtx<'_>, r: Rect) {
        let (label, enabled, role) = match self.step {
            Step::Source => (
                "Continue",
                self.can_advance(Step::Source),
                ButtonRole::Primary,
            ),
            Step::Drive => (
                "Continue",
                self.can_advance(Step::Drive),
                ButtonRole::Primary,
            ),
            Step::Customize => ("Continue", true, ButtonRole::Primary),
            Step::Confirm => (
                "Erase and write",
                self.can_advance(Step::Confirm) || self.shot_confirmation_ready,
                ButtonRole::Destructive,
            ),
            Step::Write => {
                let stopping = self
                    .job_control
                    .as_ref()
                    .is_some_and(JobControl::is_cancelled);
                (
                    if stopping {
                        "Stopping…"
                    } else {
                        "Cancel write"
                    },
                    self.job_control
                        .as_ref()
                        .is_some_and(|control| !control.is_cancelled()),
                    ButtonRole::Secondary,
                )
            }
            Step::Done => (
                if self.progress.phase == JobPhase::Failed {
                    "Review and retry"
                } else {
                    "Start over"
                },
                true,
                ButtonRole::Primary,
            ),
        };
        let hit = match self.step {
            Step::Confirm => Hit::StartWrite,
            Step::Write => Hit::CancelWrite,
            Step::Done => Hit::WriteAnother,
            _ => Hit::Next,
        };
        let face = ButtonFace::new(r, label)
            .role(role)
            .hovered(r.contains(self.last_mouse))
            .focused(self.keyboard_hit == Some(hit))
            .disabled(!enabled);
        Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
    }
}

// =========================================================================
// Step bodies
// =========================================================================
impl ImagerApp {
    fn draw_step_title(ctx: &mut PaintCtx<'_>, content: Rect, title: &str, max_w: f64) {
        let style = ctx.theme.text(TextRole::Title);
        let shown = ctx
            .text
            .elide_to_width_style(title, max_w.max(1.0) as f32, &style);
        let ink = ctx.theme.text_primary;
        ctx.text(&shown, &style, ink, content.origin(), None);
    }

    /// The step's title; the Drive step leaves room for its Rescan button.
    fn step_title(&self) -> &'static str {
        match self.step {
            Step::Source => "Choose an image",
            Step::Drive => "Choose a drive",
            Step::Customize => "Prepare the setup",
            Step::Confirm => match self.action {
                Action::Install => "Review and install",
                Action::Flash => "Review and write",
            },
            Step::Write => match (self.progress.phase == JobPhase::Verifying, self.action) {
                (true, _) => "Verifying",
                (false, Action::Install) => "Installing",
                (false, Action::Flash) => "Writing",
            },
            Step::Done => "",
        }
    }

    fn row_interaction(&self, rect: Rect, hit: Hit) -> Interaction {
        Interaction {
            hovered: rect.contains(self.last_mouse),
            focused: self.keyboard_hit == Some(hit),
            ..Interaction::default()
        }
    }

    // ---- Image -------------------------------------------------------------

    fn draw_source_step(&self, ctx: &mut PaintCtx<'_>, lay: &Layout) {
        let content = lay.content;
        Self::draw_step_title(ctx, content, self.step_title(), content.width());
        let bands = self.source_bands(content);
        for (i, (block, rect)) in bands.iter().enumerate() {
            // Rows of one list run edge to edge with inset separators.
            let next_row = bands.get(i + 1).and_then(|(next, _)| match next {
                SourceBlock::Release(j) => Some(self.release_selected(*j)),
                SourceBlock::Recent(j) => Some(self.recent_selected(*j)),
                _ => None,
            });
            match *block {
                SourceBlock::DropZone => self.draw_source_dropzone(ctx, *rect),
                SourceBlock::Header(title) => {
                    let ink = ctx.theme.text_primary;
                    ctx.text_role(title, TextRole::Subheading, ink, rect.origin(), None);
                }
                SourceBlock::NoReleases => {
                    let dim = ctx.theme.text_dim;
                    ctx.text_role(
                        NO_RELEASES,
                        TextRole::Body,
                        dim,
                        rect.origin(),
                        Some(rect.width() as f32),
                    );
                }
                SourceBlock::Selected => {
                    if let Some(source) = &self.source {
                        let secondary = self.source_secondary(source);
                        let size = fmt_size(source.size);
                        ListRowFace::new(&source.name)
                            .leading(RowLeading::Icon(Icon::File))
                            .secondary(&secondary)
                            .meta(&size)
                            .selected(true)
                            .paint(ctx, *rect);
                    }
                }
                SourceBlock::Release(index) => {
                    self.draw_release_row(ctx, *rect, index, next_row);
                }
                SourceBlock::Recent(index) => {
                    if let Some(source) = self.recent.get(index) {
                        let secondary = self.source_secondary(source);
                        let size = fmt_size(source.size);
                        let face = ListRowFace::new(&source.name)
                            .leading(RowLeading::Icon(Icon::File))
                            .secondary(&secondary)
                            .meta(&size)
                            .selected(self.recent_selected(index))
                            .interaction(self.row_interaction(*rect, Hit::Recent(index)));
                        let face = match next_row {
                            Some(next) => face.separator(next),
                            None => face,
                        };
                        face.paint(ctx, *rect);
                    }
                }
            }
        }
    }

    /// The one fact under an image's name: what it is and whether it was
    /// verified.
    fn source_secondary(&self, source: &Source) -> String {
        if self.source_is_official(source) {
            "Official Tilo release, verified".into()
        } else {
            "Local image, written unchanged".into()
        }
    }

    fn release_selected(&self, index: usize) -> bool {
        self.releases()
            .get(index)
            .zip(self.source.as_ref())
            .is_some_and(|(release, source)| release.matches_source(source))
    }

    fn recent_selected(&self, index: usize) -> bool {
        self.recent
            .get(index)
            .zip(self.source.as_ref())
            .is_some_and(|(recent, source)| {
                recent.path == source.path && recent.name == source.name
            })
    }

    fn draw_source_dropzone(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        let b = &self.bands;
        let face = DropZoneFace::new(DROP_TITLE)
            .message(DROP_MESSAGE)
            .icon(Icon::Download);
        let slot = face.paint(ctx, rect, b.control_h);
        let probe = ButtonFace::new(Rect::ZERO, DROP_ACTION);
        let w = probe.intrinsic_width(ctx.text, ctx.theme);
        let button = Rect::new(
            slot.center().x - w / 2.0,
            slot.y0,
            slot.center().x + w / 2.0,
            slot.y0 + b.control_h,
        );
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &ButtonFace::new(button, DROP_ACTION)
                .hovered(rect.contains(self.last_mouse))
                .focused(self.keyboard_hit == Some(Hit::PickFile)),
        );
    }

    /// One release: its name, where it runs, and its size; a download in
    /// flight says how far it is.
    fn draw_release_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        index: usize,
        next_selected: Option<bool>,
    ) {
        let Some(release) = self.releases().get(index) else {
            return;
        };
        let downloading = self.release_download_index == Some(index);
        let secondary = if downloading {
            let stage = self.release_download_stage.unwrap_or("Preparing");
            match self.release_download_progress {
                Some((done, total)) if total > 0 => {
                    format!("{stage} {}%, select to cancel", done * 100 / total)
                }
                _ => format!("{stage}, select to cancel"),
            }
        } else if release.local_path.is_empty() {
            format!(
                "{}, {} channel, downloads first",
                release.target_label(),
                release.channel
            )
        } else {
            format!("{}, {} channel", release.target_label(), release.channel)
        };
        let title = release.display_name();
        let size = fmt_size(release.size);
        let hit = if downloading {
            Hit::CancelDownload
        } else {
            Hit::Release(index)
        };
        let face = ListRowFace::new(&title)
            .leading(RowLeading::Icon(if release.local_path.is_empty() {
                Icon::CloudDownload
            } else {
                Icon::File
            }))
            .secondary(&secondary)
            .meta(&size)
            .selected(self.release_selected(index))
            .interaction(self.row_interaction(rect, hit));
        let face = match next_selected {
            Some(next) => face.separator(next),
            None => face,
        };
        face.paint(ctx, rect);
    }

    // ---- Drive -------------------------------------------------------------

    fn draw_drive_step(&self, ctx: &mut PaintCtx<'_>, lay: &Layout) {
        let content = lay.content;
        let b = &self.bands;
        let rescan = self.drive_rescan_rect(content);
        Self::draw_step_title(
            ctx,
            content,
            self.step_title(),
            rescan.x0 - b.gaps.sibling - content.x0,
        );
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &ButtonFace::new(rescan, RESCAN_LABEL)
                .icon(Icon::Refresh)
                .control_size(ControlSize::Small)
                .hovered(rescan.contains(self.last_mouse))
                .focused(self.keyboard_hit == Some(Hit::Rescan)),
        );

        let visible = self.visible_targets();
        if visible.is_empty() {
            let content = EmptyStateContent {
                icon: Some(Icon::HardDrive),
                ..EmptyStateContent::new(EMPTY_DRIVES)
            };
            EmptyState::paint(
                ctx,
                self.drive_empty_rect(lay.content),
                &content,
                self.last_mouse,
            );
            return;
        }
        let mut y = self.body_top(content);
        for (n, (idx, drive)) in visible.iter().enumerate() {
            let r = self.row_rect(content, y);
            let secondary = format!("{}, {}", drive.dev_path(), drive.contents_summary());
            let size = fmt_size(drive.size);
            let face = ListRowFace::new(&drive.model)
                .leading(RowLeading::Icon(Icon::HardDrive))
                .secondary(&secondary)
                .meta(&size)
                .selected(self.selected == Some(*idx))
                .interaction(self.row_interaction(r, Hit::Target(*idx)));
            let face = match visible.get(n + 1) {
                Some((next, _)) => face.separator(self.selected == Some(*next)),
                None => face,
            };
            face.paint(ctx, r);
            y = r.y1;
        }
    }

    // ---- Customize ---------------------------------------------------------

    fn draw_customize_step(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        lay: &Layout,
        presence: &PresenceCtx,
    ) {
        let content = lay.content;
        Self::draw_step_title(ctx, content, self.step_title(), content.width());
        if !self.customizable() {
            let top = self.body_top(content);
            let dim = ctx.theme.text_dim;
            ctx.text_role(
                PLAIN_BODY,
                TextRole::Body,
                dim,
                Point::new(content.x0, top),
                Some(content.width() as f32),
            );
            return;
        }
        self.customize.draw(ctx, presence, content);
    }

    // ---- Confirm -----------------------------------------------------------

    fn draw_confirm_step(&mut self, ctx: &mut PaintCtx<'_>, lay: &Layout, presence: &PresenceCtx) {
        let content = lay.content;
        Self::draw_step_title(ctx, content, self.step_title(), content.width());

        let review = self.review_rows();
        let rows: Vec<PropertyRow<'_>> = review
            .iter()
            .map(|(label, value)| PropertyRow::new(label, value))
            .collect();
        PropertyListFace::new(&rows).paint(ctx, self.confirm_review_rect(content));

        let warning = self.confirm_warning();
        let _ = Banner::paint(
            ctx,
            self.confirm_banner_rect(content),
            &BannerContent::new(BannerKind::Error, &warning.0).with_message(Some(warning.1)),
            self.last_mouse,
        );

        if self.offers_write_setup() {
            let top = self.write_setup_top(content);
            let profile = self.profile.clone();
            self.write_setup.draw(ctx, presence, content, top, &profile);
        }
        self.draw_confirm_field(ctx, content);
    }

    /// The typed confirmation field under its caption, and its note.
    fn draw_confirm_field(&mut self, ctx: &mut PaintCtx<'_>, content: Rect) {
        let b = self.bands.clone();
        let field = self.confirm_field_rect(content);
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        let dim = ctx.theme.text_dim;
        ctx.text_role(
            &format!("Type {want} to confirm"),
            TextRole::Label,
            dim,
            Point::new(field.x0, field.y0 - b.gaps.tight - b.label_h),
            None,
        );
        self.confirm_input.set_placeholder(&want);
        self.confirm_input.apply_layout_external(field);
        self.confirm_input.draw(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &PresenceCtx::empty(),
        );

        let note_top = self.confirm_note_top(content);
        let matches = self.confirm_matches();
        let mut x = field.x0;
        if matches {
            let side = b.caption_h;
            Icon::Check.draw(
                ctx.scene,
                ctx.transform,
                Rect::new(x, note_top, x + side, note_top + side),
                ctx.theme.tokens.colors.semantic.success,
            );
            x += side + b.gaps.tight;
        }
        let tertiary = ctx.theme.text_tertiary;
        ctx.text_role(
            self.confirm_note(),
            TextRole::Caption,
            tertiary,
            Point::new(x, note_top),
            Some((field.x1 - x).max(1.0) as f32),
        );
    }

    // ---- Write -------------------------------------------------------------

    fn draw_write_step(&mut self, ctx: &mut PaintCtx<'_>, lay: &Layout) {
        let content = lay.content;
        Self::draw_step_title(ctx, content, self.step_title(), content.width());
        let p = self.progress.clone();
        let writing = p.phase == JobPhase::Writing;
        let percent = format!("{:.0}%", p.overall() * 100.0);
        let caption = match (writing, self.action) {
            (true, Action::Install) => "Installing",
            (true, Action::Flash) => "Writing",
            (false, _) => "Verifying",
        };
        ProgressRingFace::new(p.overall())
            .value(&percent)
            .caption(caption)
            .size(ProgressRingSize::Large)
            .paint(ctx, self.write_ring_rect(content));

        // Rate and time left, when either is known.
        let mut meta = Vec::new();
        if p.rate_bps > 0 {
            meta.push(fmt_rate(p.rate_bps));
        }
        if let Some(src) = &self.source
            && let Some(eta) = p.eta_secs(src.size)
        {
            meta.push(format!("about {} left", fmt_eta(eta)));
        }
        let style = ctx.theme.text(TextRole::Caption).numeric();
        let dim = ctx.theme.text_dim;
        if !meta.is_empty() {
            Self::text_centered(
                ctx,
                &meta.join(", "),
                &style,
                dim,
                content,
                self.write_meta_top(content),
            );
        }

        let b = self.bands.clone();
        let mut y = self.write_bars_top(content);
        let label_style = ctx.theme.text(TextRole::Label);
        for (index, (label, frac, active)) in [
            ("Write", p.write_frac, writing),
            ("Verify", p.verify_frac, !writing),
        ]
        .into_iter()
        .enumerate()
        {
            let ink = if active {
                ctx.theme.text_primary
            } else {
                ctx.theme.text_dim
            };
            ctx.text(label, &label_style, ink, Point::new(content.x0, y), None);
            let pct = format!("{:.0}%", frac * 100.0);
            let w = f64::from(ctx.text.measure_style(&pct, None, &style).0);
            let pct_top = y + (b.label_h - b.caption_h) / 2.0;
            ctx.text(&pct, &style, dim, Point::new(content.x1 - w, pct_top), None);
            let bar_top = y + b.label_h + b.gaps.related;
            let bar = Rect::new(content.x0, bar_top, content.x1, bar_top + b.gaps.related);
            let phase_bar = &mut self.phase_bars[index];
            phase_bar.set_progress(frac as f32);
            phase_bar.apply_layout_external(bar);
            phase_bar.draw(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &PresenceCtx::empty(),
            );
            y += self.write_bar_pitch();
        }

        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            let line = format!("{} to {}", src.name, tgt.dev_path());
            let caption = ctx.theme.text(TextRole::Caption);
            let tertiary = ctx.theme.text_tertiary;
            Self::text_centered(
                ctx,
                &line,
                &caption,
                tertiary,
                content,
                self.write_foot_top(content),
            );
        }
    }

    /// One line centred on `content`, elided to its width.
    fn text_centered(
        ctx: &mut PaintCtx<'_>,
        text: &str,
        style: &TextStyle,
        ink: vello::peniko::Color,
        content: Rect,
        top: f64,
    ) {
        let shown = ctx
            .text
            .elide_to_width_style(text, content.width().max(1.0) as f32, style);
        let w = f64::from(ctx.text.measure_style(&shown, None, style).0);
        ctx.text(
            &shown,
            style,
            ink,
            Point::new(content.center().x - w / 2.0, top),
            None,
        );
    }

    // ---- Done --------------------------------------------------------------

    fn draw_done_step(&self, ctx: &mut PaintCtx<'_>, lay: &Layout) {
        let content = lay.content;
        let ok = self.progress.phase == JobPhase::Success;
        let install = self.action == Action::Install;
        let colors = &ctx.theme.tokens.colors;
        let (icon, tone) = if ok {
            (Icon::Check, colors.semantic.success)
        } else {
            (Icon::TriangleAlert, colors.semantic.danger)
        };
        icon.draw(ctx.scene, ctx.transform, self.done_icon_rect(content), tone);

        let compact = self.compact_content(content);
        let headline = if ok {
            Self::done_ok_copy(install, compact).0
        } else if install {
            "The installation failed"
        } else {
            "Writing failed"
        };
        let large = ctx.theme.text(TextRole::LargeTitle);
        let primary = ctx.theme.text_primary;
        Self::text_centered(
            ctx,
            headline,
            &large,
            primary,
            content,
            self.done_headline_top(content),
        );

        let body = ctx.theme.text(TextRole::Body);
        let dim = ctx.theme.text_dim;
        let sub_top = self.done_sub_top(content);
        if ok {
            let (_, sub) = Self::done_ok_copy(install, compact);
            let centred = body.clone().with_align(ParagraphAlign::Center);
            ctx.text(
                sub,
                &centred,
                dim,
                Point::new(content.x0, sub_top),
                Some(content.width() as f32),
            );
        } else {
            let reason = self
                .progress
                .error
                .as_deref()
                .unwrap_or("The drive may be incomplete. Review the plan before trying again.");
            let line_h = self.bands.body_h;
            for (index, line) in
                Self::failure_detail_lines(ctx.text, &body, reason, content.width() as f32, 2)
                    .iter()
                    .enumerate()
            {
                Self::text_centered(
                    ctx,
                    line,
                    &body,
                    dim,
                    content,
                    sub_top + index as f64 * line_h,
                );
            }
        }

        let summary = self.summary_rows();
        if !summary.is_empty() {
            let rows: Vec<PropertyRow<'_>> = summary
                .iter()
                .map(|(label, value)| PropertyRow::new(label, value))
                .collect();
            PropertyListFace::new(&rows).paint(ctx, self.done_summary_rect(content));
        }
    }
}

impl ImagerApp {
    fn select_source(&mut self, state: &mut AutoCommit, src: &Source) {
        let previous = self
            .source
            .as_ref()
            .filter(|previous| !previous.path.is_empty())
            .cloned()
            .map(Box::new);
        // A source outside the authenticated catalog cannot carry a profile.
        if !self.source_is_official(src) {
            self.action = Action::Flash;
        }
        let action = crate::actions::ImagerAction::SelectSource {
            source: Box::new(src.clone()),
            previous,
            recent_rank: ImagerDoc::next_recent_rank(state),
        };
        self.actions.dispatch_with_context(
            state,
            crate::actions::IMAGER_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.imager", "Imager", &action),
        );
        self.source = Some(src.clone());
        self.set_status(state, "");
        self.dirty = true;
    }

    fn open_file_picker(&mut self, state: &AutoCommit) {
        self.portal_rx = Some(
            PickRequest::open("Choose a disk image", "Choose")
                .filter(
                    FileFilter::new("Disk images")
                        .glob("*.iso")
                        .glob("*.img")
                        .glob("*.raw")
                        .mimetype("application/x-iso9660-image"),
                )
                .thread_name("imager-portal")
                .send(),
        );
        self.set_status(state, "Opening the file picker…");
        self.dirty = true;
    }

    /// Drain a pending portal result, if any. Called each `sync`.
    fn drain_portal(&mut self, state: &mut AutoCommit) {
        let Some(rx) = &self.portal_rx else { return };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.portal_rx = None;
                return;
            }
        };
        self.portal_rx = None;
        match outcome {
            PortalOutcome::Opened(path) => match crate::probe::inspect_source(&path) {
                Some(src) => self.select_source(state, &src),
                None => self.set_status(state, "That file is not a readable disk image."),
            },
            PortalOutcome::Cancelled => self.set_status(state, "File selection cancelled."),
            PortalOutcome::Failed(reason) => {
                self.set_status(state, &format!("File picker unavailable: {reason}"));
            }
        }
        self.dirty = true;
    }

    fn download_release(&mut self, state: &AutoCommit, index: usize, release: TiloRelease) {
        if self.release_rx.is_some() {
            self.set_status(state, "A Tilo installer download is already in progress.");
            return;
        }
        self.set_status(
            state,
            &format!("Downloading and verifying {}…", release.display_name()),
        );
        let job = crate::release::spawn_download(release);
        self.release_rx = Some(job.events);
        self.release_control = Some(job.control);
        self.release_download_index = Some(index);
        self.release_download_progress = None;
        self.release_download_stage = Some("Preparing");
        self.dirty = true;
    }

    fn drain_release(&mut self, state: &mut AutoCommit) {
        let Some(rx) = &self.release_rx else { return };
        let mut events = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(event) => events.push(event),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    if !events
                        .iter()
                        .any(|event| matches!(event, crate::release::DownloadEvent::Finished(_)))
                    {
                        events.push(crate::release::DownloadEvent::Finished(Err(
                            "The Tilo installer download stopped unexpectedly.".into(),
                        )));
                    }
                    break;
                }
            }
        }
        if events.is_empty() {
            return;
        }
        for event in events {
            match event {
                crate::release::DownloadEvent::Downloading { received, total } => {
                    self.release_download_progress = Some((received, total));
                    self.release_download_stage = Some("Downloading");
                    let percent = received.saturating_mul(100).checked_div(total).unwrap_or(0);
                    self.set_status(state, &format!("Downloading Tilo installer… {percent}%"));
                }
                crate::release::DownloadEvent::Verifying => {
                    self.release_download_progress = None;
                    self.release_download_stage = Some("Verifying");
                    self.set_status(state, "Cryptographically verifying the Tilo installer…");
                }
                crate::release::DownloadEvent::Expanding { written, total } => {
                    self.release_download_progress = Some((written, total));
                    self.release_download_stage = Some("Preparing the image");
                    self.set_status(state, "Preparing the verified Tilo installer image…");
                }
                crate::release::DownloadEvent::Finished(result) => {
                    self.release_rx = None;
                    self.release_control = None;
                    self.release_download_index = None;
                    self.release_download_progress = None;
                    self.release_download_stage = None;
                    match result {
                        Ok(source) => {
                            if let Some(release) = self
                                .releases
                                .iter_mut()
                                .find(|release| release.matches_source(&source))
                            {
                                release.local_path.clone_from(&source.path);
                            }
                            self.select_source(state, &source);
                            self.set_status(
                                state,
                                "Tilo installer downloaded and cryptographically verified.",
                            );
                        }
                        Err(error) => self.set_status(state, &error),
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn select_target(&mut self, state: &AutoCommit, idx: usize) {
        let Some(drive) = self.targets.get(idx) else {
            return;
        };
        // Hard guard repeated below the visible-list filter: stale persisted or
        // collaborative state cannot turn the media creator into an internal-
        // disk raw writer.
        if !drive.is_media_target() {
            self.set_status(
                state,
                "Internal and system disks can only be installed from the booted Tilo medium.",
            );
            self.dirty = true;
            return;
        }
        self.selected = Some(idx);
        self.confirm_input.set_text("");
        self.dirty = true;
    }

    fn set_status(&mut self, _state: &AutoCommit, s: &str) {
        self.status = s.to_string();
    }

    fn goto(&mut self, _state: &mut AutoCommit, s: Step) {
        self.step = s;
        self.scroll_y = 0.0;
        self.keyboard_hit = None;
        self.confirm_input.set_focus(false);
        self.customize.clear_focus();
        self.write_setup.clear_focus();
        self.dirty = true;
    }

    fn advance(&mut self, state: &mut AutoCommit) {
        match self.step {
            Step::Source if self.can_advance(Step::Source) => self.goto(state, Step::Drive),
            Step::Drive if self.can_advance(Step::Drive) => {
                self.action = Action::Flash;
                if self.customizable() {
                    self.goto(state, Step::Customize);
                } else {
                    self.goto(state, Step::Confirm);
                }
            }
            Step::Customize => match self.profile.validate_media_profile() {
                Ok(()) => self.goto(state, Step::Confirm),
                Err(message) => self.set_status(state, &message),
            },
            // Confirm advances into the write only when the typed-name gate is
            // armed; that same gesture kicks off the (external) job.
            Step::Confirm if self.can_advance(Step::Confirm) => self.start_write(state),
            _ => {}
        }
    }

    fn back(&mut self, state: &mut AutoCommit) {
        match self.step {
            Step::Confirm if self.customizable() => self.goto(state, Step::Customize),
            Step::Confirm | Step::Customize => self.goto(state, Step::Drive),
            Step::Drive => self.goto(state, Step::Source),
            // No going back from the source, an in-flight write, or the outcome.
            Step::Source | Step::Write | Step::Done => {}
        }
    }

    fn start_write(&mut self, state: &mut AutoCommit) {
        let operation = crate::actions::ImagerOperation::Write {
            device: self.target().map(TargetDrive::dev_path).unwrap_or_default(),
            confirmation: self.confirm_input.value().to_string(),
        };
        if let Err(reason) = self.perform_operation(&operation, state) {
            self.set_status(state, &reason);
            self.dirty = true;
        }
    }

    /// One semantic implementation shared by the direct Confirm control and
    /// the resource-operation route.
    #[allow(
        clippy::too_many_lines,
        reason = "write, cancellation, status, and export share one operation-dispatch boundary"
    )]
    fn perform_operation(
        &mut self,
        operation: &crate::actions::ImagerOperation,
        state: &mut AutoCommit,
    ) -> Result<(), String> {
        if matches!(operation, crate::actions::ImagerOperation::Write { .. })
            && !self.startup_write_state_reconciled
        {
            return Err("Restoring the previous image-write state; wait for drive review before starting another write".into());
        }
        if matches!(operation, crate::actions::ImagerOperation::Cancel) {
            let Some(control) = self.job_control.clone() else {
                return Err("No image write is currently running".into());
            };
            control.cancel();
            self.set_status(
                state,
                "Stopping safely… Keep the drive connected until cancellation finishes.",
            );
            self.dirty = true;
            return Ok(());
        }
        let crate::actions::ImagerOperation::Write {
            device,
            confirmation,
        } = operation
        else {
            return match operation {
                crate::actions::ImagerOperation::Verify => Err(
                    "Read-back verification runs automatically after every successful write".into(),
                ),
                crate::actions::ImagerOperation::Cancel => {
                    Err("No image write is currently running".into())
                }
                crate::actions::ImagerOperation::Status => Ok(()),
                crate::actions::ImagerOperation::ExportConfig => {
                    if self.source.is_none() || self.target().is_none() {
                        Err("Select an image source and target drive first".into())
                    } else {
                        self.export_config(state);
                        Ok(())
                    }
                }
                crate::actions::ImagerOperation::SetWifiPsk { value } => {
                    self.wifi_psk.clone_from(value);
                    Ok(())
                }
                crate::actions::ImagerOperation::Write { .. } => {
                    Err("Invalid image-write operation dispatch".into())
                }
            };
        };
        let (Some(mut src), Some(tgt)) = (self.source.clone(), self.target().cloned()) else {
            return Err("Select a source and drive first.".into());
        };
        // Treat the collaborative source record as a claim, never as trust.
        // Only an exact bundled-catalog match may transport a setup profile.
        src.installable = self.source_is_official(&src);
        if !src.installable {
            src.kind = SourceKind::IsoFile;
            src.expected_sha256 = None;
        }
        if src.installable {
            self.profile.validate_media_profile()?;
        }
        if *device != tgt.dev_path() {
            return Err("The selected target changed; review the write plan again.".into());
        }
        // Stage Wi-Fi / enrollment for any image the operator configured on
        // Confirm. Official media still embeds only the secret-free profile
        // block; the passphrase and new device key stay in the staged tree
        // and are copied onto the installed root after verification.
        let handoff_stage =
            if !self.profile.wifi_ssid.trim().is_empty() || self.profile.enroll_local_identity {
                let local = crate::handoff::local_identity_from_env()?;
                crate::handoff::stage_setup_handoff(&self.profile, &self.wifi_psk, &local)?
                    .map(|stage| stage.root)
            } else {
                None
            };
        let req = WriteRequest {
            source: src,
            target: tgt,
            action: self.action,
            profile: self.profile.clone(),
            handoff_stage: handoff_stage.clone(),
        };
        let (start, rx, control) = spawn_job(req, confirmation);
        let refuse = |message: String| -> Result<(), String> {
            if let Some(stage) = handoff_stage.as_ref() {
                let _ = std::fs::remove_dir_all(stage);
            }
            Err(message)
        };
        match start {
            crate::write::JobStart::Started => {
                self.job_rx = rx;
                self.job_control = control;
                self.progress = Progress {
                    phase: JobPhase::Writing,
                    write_frac: 0.0,
                    verify_frac: 0.0,
                    rate_bps: 0,
                    error: None,
                };
                self.set_status(state, "");
                // The write has begun: move onto the live-progress station.
                self.goto(state, Step::Write);
            }
            crate::write::JobStart::ConfirmationMismatch => {
                return refuse(
                    "The confirmation does not match the drive name. Nothing was written.".into(),
                );
            }
            crate::write::JobStart::TooSmall => {
                return refuse("The image is larger than the drive. Nothing was written.".into());
            }
            crate::write::JobStart::UnsafeTarget => {
                return refuse(
                    "Internal and system disks can only be installed from the booted Tilo medium."
                        .into(),
                );
            }
            crate::write::JobStart::Incomplete => {
                return refuse("Select a source and drive before writing.".into());
            }
            crate::write::JobStart::InvalidProfile(message) => return refuse(message),
            crate::write::JobStart::WriteDisabled => {
                return refuse(
                    "Raw-device writing is disabled; enable the privileged Imager backend first."
                        .into(),
                );
            }
        }
        self.dirty = true;
        Ok(())
    }

    /// Drain live job progress, folding it into the doc. Called each `sync`.
    fn drain_job(&mut self, state: &mut AutoCommit) {
        let Some(rx) = &self.job_rx else { return };
        let mut latest = None;
        loop {
            match rx.try_recv() {
                Ok(p) => latest = Some(p),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // The worker finished; if we never saw a terminal phase,
                    // treat a non-success as a failure.
                    if self.progress.phase.is_active() {
                        self.progress.phase = JobPhase::Failed;
                        self.progress.error = Some(
                            "The writer stopped unexpectedly. Reconnect the drive, then review and retry."
                                .into(),
                        );
                        self.set_status(
                            state,
                            "The writer stopped unexpectedly. Reconnect the drive, then review and retry.",
                        );
                        self.goto(state, Step::Done);
                    }
                    self.job_rx = None;
                    self.job_control = None;
                    self.dirty = true;
                    return;
                }
            }
        }
        if let Some(mut p) = latest {
            // Cancellation may race with a terminal helper event already in
            // the pipe. Honour the user's stop request conservatively: never
            // turn that race into a claim that the medium is ready to use.
            if p.phase == JobPhase::Success
                && self
                    .job_control
                    .as_ref()
                    .is_some_and(JobControl::is_cancelled)
            {
                p.phase = JobPhase::Failed;
                p.error = Some(
                    "Image writing was cancelled. The drive may be incomplete; review and retry before using it."
                        .into(),
                );
            }
            let terminal_phase = p.phase;
            let terminal_error = p.error.clone();
            self.progress = p;
            self.dirty = true;
            if matches!(terminal_phase, JobPhase::Success | JobPhase::Failed) {
                self.job_rx = None;
                self.job_control = None;
                let name = self
                    .source
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                let msg = if terminal_phase == JobPhase::Success {
                    format!("{name} {} and verified", self.action.past())
                } else {
                    terminal_error.unwrap_or_else(|| {
                        format!(
                            "{} failed. Reconnect the drive, then review and retry.",
                            self.action.title()
                        )
                    })
                };
                self.set_status(state, &msg);
                // Terminal phase → show the outcome station.
                self.goto(state, Step::Done);
            }
        }
    }

    fn write_another(&mut self, state: &mut AutoCommit) {
        if self.progress.phase == JobPhase::Failed {
            // Preserve the reviewed source, target, and customized setup. A
            // retry still requires a fresh peer-local destructive confirmation
            // and passes every target/source validation again in the helper.
            self.progress = Progress::default();
            self.confirm_input.set_text("");
            self.goto(state, Step::Confirm);
            self.set_status(
                state,
                "Review the image and drive, then confirm the target again to retry.",
            );
            return;
        }
        self.progress = Progress::default();
        self.confirm_input.set_text("");
        self.goto(state, Step::Source);
        self.set_status(state, "");
    }

    fn export_config(&mut self, _state: &mut AutoCommit) {
        let text = self
            .profile
            .to_headless_config(self.source.as_ref(), self.target());
        self.export_dialog.show(text);
        self.dirty = true;
    }

    pub(crate) fn seed_export_for_shot(&mut self) {
        self.export_after_sync = true;
    }

    /// Replace the last probe snapshot without carrying destructive consent
    /// across an unplug/replug substitution. Device names such as `disk4`,
    /// `sdb`, and `PhysicalDrive2` are reusable slots, not hardware identity.
    fn apply_drive_inventory(&mut self, targets: Vec<TargetDrive>) {
        let remembered = self.target().cloned();
        self.targets = targets;
        self.selected = remembered.as_ref().and_then(|expected| {
            self.targets
                .iter()
                .position(|actual| Self::same_rescanned_target(expected, actual))
        });
        if self.selected.is_none() {
            self.confirm_input.set_text("");
        }
    }

    fn same_rescanned_target(expected: &TargetDrive, actual: &TargetDrive) -> bool {
        // Slot names, model strings and capacities are not physical identity.
        // Two identical sticks can reuse `sdb`, `disk4`, or `PhysicalDrive2`;
        // without a stable ID, require a fresh choice and destructive
        // confirmation after every rescan.
        let stable_identity_matches = match (&expected.stable_id, &actual.stable_id) {
            (Some(expected), Some(actual)) => expected == actual,
            _ => false,
        };
        stable_identity_matches
            && expected.name == actual.name
            && expected.model == actual.model
            && expected.size == actual.size
            && expected.removable == actual.removable
            && expected.is_system == actual.is_system
    }

    fn do_rescan(&mut self, state: &AutoCommit) -> bool {
        if live_probe_enabled() {
            // Re-read the live block-device inventory. `probe()` returns `None`
            // only when the platform probe is unavailable or malformed, so in
            // that case we keep the last-known list rather than blanking it.
            // A real (possibly empty) result is authoritative and replaces it,
            // so unplugging the selected drive genuinely drops it from the list.
            let Some(t) = crate::probe::probe() else {
                self.set_status(
                    state,
                    "Drive rescan failed. Check the system disk tools and try again.",
                );
                self.dirty = true;
                return false;
            };
            self.apply_drive_inventory(t);
            let removable = self.removable_count();
            self.set_status(
                state,
                &match removable {
                    0 => "No USB drives or SD cards found".to_string(),
                    1 => "Found 1 drive".to_string(),
                    count => format!("Found {count} drives"),
                },
            );
        } else {
            self.set_status(state, "Rescanned drives");
        }
        self.selected = self.selected.filter(|i| *i < self.targets.len());
        self.dirty = true;
        true
    }

    fn dispatch_hit(&mut self, state: &mut AutoCommit, hit: Hit) {
        match hit {
            Hit::PickFile => self.open_file_picker(state),
            Hit::Release(i) => {
                if let Some(release) = self.releases.get(i).cloned() {
                    if release.url.is_empty() {
                        self.select_source(state, &release.to_source());
                    } else {
                        self.download_release(state, i, release);
                    }
                }
            }
            Hit::CancelDownload => {
                if let Some(control) = self.release_control.clone() {
                    control.cancel();
                    self.set_status(
                        state,
                        "Cancelling safely… The verified partial download will be kept for resume.",
                    );
                    self.dirty = true;
                }
            }
            Hit::Recent(i) => {
                if let Some(src) = self.recent.get(i).cloned() {
                    self.select_source(state, &src);
                }
            }
            Hit::Target(i) => self.select_target(state, i),
            Hit::Rescan => {
                self.do_rescan(state);
            }
            Hit::ConfirmField => {
                self.confirm_input.set_focus(true);
                self.customize.clear_focus();
                self.dirty = true;
            }
            Hit::Back => self.back(state),
            Hit::Next => self.advance(state),
            Hit::StartWrite => self.start_write(state),
            Hit::CancelWrite => {
                let _ = self.perform_operation(&crate::actions::ImagerOperation::Cancel, state);
            }
            Hit::WriteAnother => self.write_another(state),
        }
    }

    /// Jobs are supervised only by this process. A restart cannot make a
    /// remote/persisted progress claim look trustworthy, so every fresh window
    /// begins with an idle local job surface and requires a new review.
    fn reconcile_persisted_write_state(&mut self, _state: &mut AutoCommit) {
        self.startup_write_state_reconciled = true;
    }
}

// =========================================================================
// Widget impl
// =========================================================================
impl Widget for ImagerApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::IMAGER_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        if !self.seeded {
            if ImagerDoc::is_empty(state) {
                // Start genuinely blank. In the live app a real `lsblk` probe
                // fills the drive list; if it finds nothing (or is unavailable)
                // the list stays empty and the Drive step shows its honest
                // "insert a drive" state — the imager never invents drives.
                // Sample content is opt-in (`--demo` / `--shots`), which seeds
                // the document *before* the first sync so this branch is skipped.
                let action = crate::actions::ImagerAction::EnsureRecords;
                self.actions.dispatch_with_context(
                    state,
                    crate::actions::IMAGER_ACTION_TYPE,
                    &action,
                    tilo_actions::ActionContext::for_action("so.tilo.imager", "Imager", &action),
                );
            }
            if live_probe_enabled() {
                self.targets = crate::probe::probe().unwrap_or_default();
            }
            if live_probe_enabled() {
                self.reconcile_persisted_write_state(state);
            } else {
                // Demo/tests/shots intentionally pose worker-less progress
                // frames. They still complete startup so operations cannot be
                // held unavailable forever outside the live binary.
                self.startup_write_state_reconciled = true;
            }
            self.seeded = true;
        }

        if live_probe_enabled() && self.releases.is_empty() {
            match crate::release::bundled_releases() {
                Ok(releases) => self.releases = releases,
                Err(error) => self.status = error,
            }
        }

        // Fold in any async work before reading the doc back.
        self.drain_portal(state);
        self.drain_release(state);
        self.drain_job(state);

        self.source = ImagerDoc::source(state);
        self.recent = ImagerDoc::recent(state);
        self.profile = ImagerDoc::profile(state);
        self.action = Action::Flash;
        self.selected = self.selected.filter(|i| {
            self.targets
                .get(*i)
                .is_some_and(TargetDrive::is_media_target)
        });

        // Refresh the shared customization panel's fields from the profile
        // (each unfocused field mirrors the stored value; a focused one stays
        // the source of truth).
        self.customize.sync(&self.profile);
        self.write_setup.sync(&self.profile, &self.wifi_psk);
        if self.export_after_sync {
            self.export_after_sync = false;
            self.export_config(state);
        }
        self.dirty = true;
    }

    /// Measure every band with the renderer that will paint it, so each
    /// block sits on its real (scaled, wrapped) line boxes.
    fn adjust_layout_with_measure(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        tr: &mut TextRenderer,
    ) -> bool {
        // Prefer the allocation being solved; before the first solve (or on a
        // probe without one) fall back to the last applied rect.
        let width = taffy
            .layout(node)
            .ok()
            .map(|layout| f64::from(layout.size.width))
            .filter(|w| *w > 0.0)
            .unwrap_or(self.rect.width());
        let theme = self.theme.clone();
        let bands = self.measure_bands(tr, &theme, width);
        let mut changed = bands != self.bands;
        self.bands = bands;
        // The customization panel hangs off the step title and measures its
        // own captions and Preview button.
        let head = self.bands.title_h + self.bands.gaps.section;
        changed |= self.customize.measure_layout(tr, &theme, head);
        changed |= self.write_setup.measure_layout(tr, &theme);
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The measuring hook receives no `Theme`; keep the one this pass
        // lays out with.
        self.theme = theme.clone();
        // `new_leaf` is typed `-> TaffyResult<NodeId>` for API generality but
        // cannot actually return `Err` for a fresh leaf (see
        // `ui::widget::TaffyTreeExt`'s doc comment), and `Widget::build_layout`
        // returns a bare `NodeId` across every implementor, so there is
        // nowhere to surface a theoretical error.
        #[allow(
            clippy::unwrap_used,
            reason = "taffy::TaffyTree::new_leaf cannot return Err; see ui::widget::TaffyTreeExt"
        )]
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // A missing layout entry for our own node would be a taffy bug, not a
        // recoverable app condition — skip this frame's rect update rather
        // than panic; the next frame's layout pass will have one.
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        self.scroll_y = self.scroll_y.min(self.max_scroll());
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The framework paints the window ground; the wizard paints only its
        // composites on it.
        let lay = self.compute_layout();
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        self.draw_stepper(&mut ctx, &lay);
        // The step body scrolls under a clip that spans the window's width,
        // so row lifts and focus rings beside the column stay whole.
        let clip = Rect::new(
            self.rect.x0,
            lay.viewport_content.y0,
            self.rect.x1,
            lay.viewport_content.y1,
        );
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
        match self.step {
            Step::Source => self.draw_source_step(&mut ctx, &lay),
            Step::Drive => self.draw_drive_step(&mut ctx, &lay),
            Step::Customize => self.draw_customize_step(&mut ctx, &lay, presence),
            Step::Confirm => self.draw_confirm_step(&mut ctx, &lay, presence),
            Step::Write => self.draw_write_step(&mut ctx, &lay),
            Step::Done => self.draw_done_step(&mut ctx, &lay),
        }
        ctx.scene.pop_layer();
        if self.keyboard_hit == Some(Hit::PickFile)
            && let Some((rect, _)) = lay.hits.iter().find(|(_, hit)| *hit == Hit::PickFile)
        {
            paint_focus_ring(
                ctx.scene,
                transform,
                *rect,
                f64::from(theme.tokens.radii.tile),
                theme,
            );
        }
        self.draw_scrollbar(ctx.scene, theme, transform, &lay);
        self.draw_footer(&mut ctx, &lay);
    }

    /// The top layer: an open choice list on Customize and the export sheet.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.step == Step::Customize && self.customizable() {
            self.customize
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        if self.export_dialog.is_open() {
            self.export_dialog
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the canvas wizard routes modal, text-field, profile-panel, pointer, scroll and keyboard capture in one explicitly ordered event transaction"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        use winit::event::{ElementState, KeyEvent, MouseButton};

        let lay = self.compute_layout();

        // The detached shared modal owns capture and dismissal before any
        // wizard field or shortcut sees the event.
        if self.export_dialog.handle_detached(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            ctx.state,
            ctx.path,
        ) {
            *ctx.handled = true;
            self.dirty = true;
            return;
        }

        if self.handle_tab_event(event, *ctx.modifiers, &lay) {
            *ctx.handled = true;
            self.dirty = true;
            return;
        }

        // Destructive consent is peer-local and goes through the toolkit's
        // real editor before any wizard shortcut sees the event. On-write
        // Wi-Fi / enrollment fields sit above it and take events first.
        if self.step == Step::Confirm && !self.export_dialog.is_open() {
            if self.handle_write_setup_event(event, ctx, &lay) {
                *ctx.handled = true;
                self.dirty = true;
                return;
            }
            let mut input_handled = false;
            let mut focus_target = None;
            self.confirm_input.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    handled: &mut input_handled,
                    focus_target: &mut focus_target,
                    ..ctx.reborrow(ctx.path, ctx.node_id)
                },
            );
            if focus_target.is_some() {
                self.confirm_input.set_focus(true);
                self.write_setup.clear_focus();
            }
            if self.confirm_input.take_dirty().is_some() {
                self.dirty = true;
            }
            if self.confirm_input.take_submit() {
                if self.can_advance(Step::Confirm) {
                    self.start_write(ctx.state);
                }
                self.confirm_input.set_focus(false);
                *ctx.handled = true;
                self.dirty = true;
                return;
            }
            if input_handled {
                *ctx.handled = true;
                self.dirty = true;
                return;
            }
        }

        // The Customize step is delegated in full to the shared customization
        // panel, which owns its fields, toggles and export affordance. Only
        // events it does not consume (footer clicks, global keys) fall through
        // to the wizard chrome below.
        let consumed = self.handle_customize_event(event, ctx, &lay);
        if consumed {
            *ctx.handled = true;
            self.dirty = true;
            return;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x, position.y, ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(ctx.state, &lay, ctx.handled);
            }
            WindowEvent::MouseWheel { delta, .. }
                if lay.viewport_content.contains(self.last_mouse) =>
            {
                use winit::event::MouseScrollDelta;
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * self.bands.row_h,
                    MouseScrollDelta::PixelDelta(point) => point.y / ctx.scale_factor,
                };
                let next = (self.scroll_y - dy).clamp(0.0, self.max_scroll());
                if (next - self.scroll_y).abs() > f64::EPSILON {
                    self.scroll_y = next;
                    self.dirty = true;
                }
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.handle_key_pressed(logical_key, ctx.state, ctx.handled);
            }
            _ => {}
        }
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation_v2(
                request.kind,
                request.type_id,
                request.payload,
                doc,
                path,
            )?
            .map(ui::widget::ResourceOperationStart::Completed))
    }

    fn name(&self) -> &'static str {
        "ImagerApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.export_dialog
            .accessibility()
            .or_else(|| Some(self.accessibility_tree()))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "this is the single assistive-action router for canvas hits, editable profile fields, and peer-local destructive confirmation"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        use ui::a11y::Action as AccessAction;
        let (state, _) = ui::doc::document(state, path);

        if self.export_dialog.is_open() {
            return false;
        }
        if let Some(field_key) = target.strip_prefix("imager:write-setup:") {
            if self.step != Step::Confirm || !self.offers_write_setup() {
                return false;
            }
            return match (field_key, action) {
                ("enroll-identity", AccessAction::Activate | AccessAction::Focus) => {
                    let on = !self.profile.enroll_local_identity;
                    let field_action = crate::actions::ImagerAction::SetEnrollLocalIdentity { on };
                    self.actions.dispatch_with_context(
                        state,
                        crate::actions::IMAGER_ACTION_TYPE,
                        &field_action,
                        tilo_actions::ActionContext::for_action(
                            "so.tilo.imager",
                            "Imager",
                            &field_action,
                        ),
                    );
                    self.profile = ImagerDoc::profile(state);
                    true
                }
                ("wifi-ssid", AccessAction::SetValue(value)) => {
                    let field_action = crate::actions::ImagerAction::SetWifiSsid {
                        value: value.clone(),
                    };
                    self.actions.dispatch_with_context(
                        state,
                        crate::actions::IMAGER_ACTION_TYPE,
                        &field_action,
                        tilo_actions::ActionContext::for_action(
                            "so.tilo.imager",
                            "Imager",
                            &field_action,
                        ),
                    );
                    self.profile = ImagerDoc::profile(state);
                    true
                }
                ("device-label", AccessAction::SetValue(value)) => {
                    let field_action = crate::actions::ImagerAction::SetDeviceLabel {
                        value: value.clone(),
                    };
                    self.actions.dispatch_with_context(
                        state,
                        crate::actions::IMAGER_ACTION_TYPE,
                        &field_action,
                        tilo_actions::ActionContext::for_action(
                            "so.tilo.imager",
                            "Imager",
                            &field_action,
                        ),
                    );
                    self.profile = ImagerDoc::profile(state);
                    true
                }
                ("wifi-psk", AccessAction::SetValue(value)) => {
                    self.wifi_psk.clone_from(value);
                    true
                }
                _ => false,
            };
        }
        if let Some(field_key) = target.strip_prefix("imager:profile:") {
            if self.step != Step::Customize || !self.customizable() {
                return false;
            }
            if field_key == "preview" {
                return match action {
                    AccessAction::Focus => {
                        self.focus_accessible_profile_slot(7);
                        true
                    }
                    AccessAction::Activate => {
                        self.export_config(state);
                        true
                    }
                    _ => false,
                };
            }
            let Some(field) = CzField::from_key(field_key) else {
                return false;
            };
            let Some(slot) = [
                CzField::Hostname,
                CzField::FullName,
                CzField::Username,
                CzField::Locale,
                CzField::Timezone,
                CzField::Keyboard,
                CzField::Filesystem,
            ]
            .iter()
            .position(|candidate| *candidate == field) else {
                return false;
            };
            return match action {
                AccessAction::Focus => {
                    self.focus_accessible_profile_slot(slot);
                    true
                }
                AccessAction::SetValue(value) => {
                    let Some(value) = self.customize.canonical_value(field, value) else {
                        self.set_status(state, &format!("Choose a listed {}", field.label()));
                        return true;
                    };
                    let field_action = CustomizePanel::field_action(field, value);
                    self.actions.dispatch_with_context(
                        state,
                        crate::actions::IMAGER_ACTION_TYPE,
                        &field_action,
                        tilo_actions::ActionContext::for_action(
                            "so.tilo.imager",
                            "Imager",
                            &field_action,
                        ),
                    );
                    self.focus_accessible_profile_slot(slot);
                    true
                }
                _ => false,
            };
        }

        let identity_hit = target
            .strip_prefix("imager:release:")
            .and_then(|identity| {
                self.releases
                    .iter()
                    .position(|release| release.selection_id() == identity)
                    .map(Hit::Release)
            })
            .or_else(|| {
                target.strip_prefix("imager:recent:").and_then(|path| {
                    self.recent
                        .iter()
                        .position(|source| source.path == path)
                        .map(Hit::Recent)
                })
            })
            .or_else(|| {
                target.strip_prefix("imager:drive:").and_then(|name| {
                    self.targets
                        .iter()
                        .position(|drive| drive.name == name)
                        .map(Hit::Target)
                })
            });
        let Some(hit) = identity_hit.or_else(|| Self::hit_from_target(target)) else {
            return false;
        };
        if !self
            .compute_layout()
            .hits
            .iter()
            .any(|(_, candidate)| *candidate == hit)
        {
            return false;
        }
        if !self.a11y_hit_enabled(hit) {
            return false;
        }
        match action {
            AccessAction::Focus => {
                let lay = self.compute_layout();
                self.customize.clear_focus();
                self.keyboard_hit = Some(hit);
                self.confirm_input.set_focus(hit == Hit::ConfirmField);
                if let Some((rect, _)) = lay.hits.iter().find(|(_, candidate)| *candidate == hit) {
                    self.ensure_visible(*rect, lay.viewport_content);
                }
                self.dirty = true;
                true
            }
            AccessAction::SetValue(value) if hit == Hit::ConfirmField => {
                self.confirm_input.set_text(value);
                self.confirm_input.set_focus(true);
                self.keyboard_hit = Some(hit);
                self.dirty = true;
                true
            }
            AccessAction::Activate => {
                self.dispatch_hit(state, hit);
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Keep the event loop ticking while a job or file picker is in flight, so
    /// the progress bars animate and the portal result is drained promptly.
    fn needs_animation_frame(&self) -> bool {
        self.job_rx.is_some() || self.portal_rx.is_some() || self.release_rx.is_some()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

impl ui::resources::PublishesResources for ImagerApp {
    /// Standalone / `--shots` / unit tests publish the full tree. When the
    /// framework hosts the surface, [`tilo_resource::publish`] already owns
    /// the profile / write-plan door — keep only live extras (none today:
    /// job status is a View on the resource, not a second published tree).
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return crate::resource::live_extras(self);
        }
        self.publish_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.is_hosted() && id == "imager.sources" {
            return None;
        }
        fn find(resources: &[tilo_actions::Resource], id: &str) -> Option<tilo_actions::Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource.clone());
                }
                if let Some(found) = find(&resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        find(&self.publish_resources(doc), id)
    }
}

impl ui::undo::CommittedActions for ImagerApp {
    /// The framework host drains the undo boundary an on-screen profile control
    /// dispatched, so it records exactly one boundary bracketing it — the same
    /// path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl ImagerApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        let expected_kind = match operation {
            crate::actions::ImagerOperation::Status
            | crate::actions::ImagerOperation::ExportConfig => tilo_actions::OperationKind::View,
            crate::actions::ImagerOperation::SetWifiPsk { .. } => {
                tilo_actions::OperationKind::Effect
            }
            _ => tilo_actions::OperationKind::Effect,
        };
        if kind != expected_kind {
            return Err("Imager operation kind does not match its contract".into());
        }
        let mut local_doc = doc.clone();
        self.perform_operation(&operation, &mut local_doc)?;
        Ok(true)
    }

    fn answer_resource_operation_v2(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        match crate::actions::decode_operation(type_id, payload)? {
            Some(crate::actions::ImagerOperation::Status) => {
                if kind != tilo_actions::OperationKind::View {
                    return Err("Imager status is a view operation".into());
                }
                return Ok(Some(tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "phase": self.progress.phase.key(),
                        "progress": self.progress.overall(),
                        "message": self.status,
                    })),
                    resources: Vec::new(),
                }));
            }
            Some(crate::actions::ImagerOperation::ExportConfig) => {
                if kind != tilo_actions::OperationKind::View {
                    return Err("Imager config export is a view operation".into());
                }
                if self.source.is_none() || self.target().is_none() {
                    return Err("Select an image source and target drive first".into());
                }
                let text = self
                    .profile
                    .to_headless_config(self.source.as_ref(), self.target());
                self.export_dialog.show(text.clone());
                self.dirty = true;
                return Ok(Some(tilo_actions::ActionOutput {
                    value: Some(serde_json::Value::String(text)),
                    resources: Vec::new(),
                }));
            }
            _ => {}
        }
        self.answer_resource_operation(kind, type_id, payload, doc, path)
            .map(|handled| handled.then(tilo_actions::ActionOutput::default))
    }
}

impl ImagerApp {
    /// The Customize step's delegated handling. Returns `true` when the panel
    /// consumed the event, so the caller stops before the wizard chrome below
    /// ever sees it.
    fn handle_customize_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        lay: &Layout,
    ) -> bool {
        if !(self.step == Step::Customize && self.customizable() && !self.export_dialog.is_open()) {
            return false;
        }
        let removable = self.target().is_some_and(|d| d.removable);
        let profile = self.profile.clone();
        let resp = self.customize.handle_event(
            event,
            ctx,
            &mut self.actions,
            &profile,
            removable,
            lay.content,
        );
        if resp.export_requested {
            self.export_config(ctx.state);
        }
        resp.handled
    }

    /// Confirm-step Wi-Fi / enrollment form for the selected image.
    fn handle_write_setup_event(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        lay: &Layout,
    ) -> bool {
        if !self.offers_write_setup() {
            return false;
        }
        let profile = self.profile.clone();
        let top = self.write_setup_top(lay.content);
        let resp = self.write_setup.handle_event(
            event,
            ctx,
            &mut self.actions,
            &profile,
            lay.content,
            top,
        );
        if let Some(psk) = resp.wifi_psk {
            self.wifi_psk = psk;
        }
        if resp.handled {
            self.confirm_input.set_focus(false);
            self.profile = ImagerDoc::profile(ctx.state);
        }
        resp.handled
    }

    /// Track the pointer and which drive card (if any) is under it.
    fn handle_cursor_moved(&mut self, px: f64, py: f64, scale_factor: f64) {
        let p = Point::new(px / scale_factor, py / scale_factor);
        self.last_mouse = p;
        // Repaint on move so buttons/cards light up under the pointer.
        self.dirty = true;
    }

    /// A left click in the content area dispatches a hit-tested affordance or,
    /// on empty space, drops the confirm field's focus.
    fn handle_left_click(&mut self, state: &mut AutoCommit, lay: &Layout, handled: &mut bool) {
        self.keyboard_hit = None;
        let p = self.last_mouse;
        let hit = lay
            .hits
            .iter()
            .find(|(r, _)| r.contains(p))
            .map(|(_, h)| *h);
        if let Some(hit) = hit {
            self.dispatch_hit(state, hit);
            *handled = true;
            self.dirty = true;
            return;
        }
        self.confirm_input.set_focus(false);
    }

    /// Keyboard handling for the Confirm step's typed device-name field and
    /// global shortcuts. The export dialog captures input before this path.
    fn handle_key_pressed(
        &mut self,
        logical_key: &winit::keyboard::Key,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        use winit::keyboard::{Key, NamedKey};

        if matches!(logical_key, Key::Named(NamedKey::Enter))
            && let Some(hit) = self.keyboard_hit
        {
            self.dispatch_hit(state, hit);
            *handled = true;
            self.dirty = true;
            return;
        }

        // The Confirm step's typed device-name field (a transient arming
        // gesture, not a profile edit).
        if self.confirm_input.is_focused() && self.step == Step::Confirm {
            if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                self.confirm_input.set_focus(false);
                *handled = true;
                self.dirty = true;
            }
            return;
        }
        self.handle_global_shortcut_key(logical_key, state, handled);
    }

    fn ensure_visible(&mut self, rect: Rect, viewport: Rect) {
        let padding = 10.0;
        if rect.y1 > viewport.y1 - padding {
            self.scroll_y += rect.y1 - (viewport.y1 - padding);
        } else if rect.y0 < viewport.y0 + padding {
            self.scroll_y -= viewport.y0 + padding - rect.y0;
        }
        self.scroll_y = self.scroll_y.clamp(0.0, self.max_scroll());
    }

    fn handle_tab_event(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        lay: &Layout,
    ) -> bool {
        let WindowEvent::KeyboardInput {
            event:
                winit::event::KeyEvent {
                    logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab),
                    state: winit::event::ElementState::Pressed,
                    ..
                },
            ..
        } = event
        else {
            return false;
        };
        self.handle_tab_navigation(modifiers.state().shift_key(), lay);
        true
    }

    fn handle_tab_navigation(&mut self, reverse: bool, lay: &Layout) {
        if self.step == Step::Customize && self.customizable() {
            const CUSTOM_SLOTS: usize = 7;
            let back_slot = CUSTOM_SLOTS;
            let primary_slot = CUSTOM_SLOTS + 1;
            let current = self.customize.keyboard_slot().or(match self.keyboard_hit {
                Some(Hit::Back) => Some(back_slot),
                Some(Hit::Next) => Some(primary_slot),
                _ => None,
            });
            let count = CUSTOM_SLOTS + 2;
            let next = match (current, reverse) {
                (Some(0) | None, true) => count - 1,
                (Some(index), true) => index - 1,
                (Some(index), false) => (index + 1) % count,
                (None, false) => 0,
            };
            self.customize.clear_focus();
            self.keyboard_hit = None;
            if next < CUSTOM_SLOTS {
                self.customize.focus_keyboard_slot(next);
                if let Some(rect) = self.customize.keyboard_slot_rect(lay.content, next) {
                    self.ensure_visible(rect, lay.viewport_content);
                }
            } else {
                self.keyboard_hit = Some(if next == back_slot {
                    Hit::Back
                } else {
                    Hit::Next
                });
            }
            return;
        }

        let focusable = lay.hits.iter().map(|(_, hit)| *hit).collect::<Vec<_>>();
        if focusable.is_empty() {
            return;
        }
        let current = self
            .keyboard_hit
            .and_then(|selected| focusable.iter().position(|hit| *hit == selected));
        let next = match (current, reverse) {
            (Some(0) | None, true) => focusable.len() - 1,
            (Some(index), true) => index - 1,
            (Some(index), false) => (index + 1) % focusable.len(),
            (None, false) => 0,
        };
        self.customize.clear_focus();
        self.confirm_input.set_focus(false);
        self.keyboard_hit = focusable.get(next).copied();
        if self.keyboard_hit == Some(Hit::ConfirmField) {
            self.confirm_input.set_focus(true);
        }
        if let Some((rect, _)) = lay.hits.get(next) {
            self.ensure_visible(*rect, lay.viewport_content);
        }
    }

    fn handle_global_shortcut_key(
        &mut self,
        logical_key: &winit::keyboard::Key,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        use winit::keyboard::{Key, NamedKey};

        match logical_key {
            Key::Named(NamedKey::Enter) => {
                self.advance(state);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.back(state);
                *handled = true;
            }
            Key::Character(s) if matches!(s.as_str(), "r" | "R") && self.step == Step::Drive => {
                self.do_rescan(state);
                *handled = true;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded_app() -> (ImagerApp, AutoCommit) {
        // Tests drive the wizard against the demo document (sample drives + a
        // Tilo image + recent history) — the same content `--shots`/`--demo` use.
        let mut app = ImagerApp::new();
        app.targets = crate::model::sample_targets();
        app.releases = crate::model::sample_releases();
        let mut doc = AutoCommit::new();
        ImagerDoc::seed_demo(&mut doc);
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
    }

    /// Blank start, demo seeding, local state and restart reconciliation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn startup_state_is_blank_local_and_reconciled_before_writes() {
        // Phase `local_wizard_state_never_leaks_through_the_collaborative_document`.
        {
            let (mut app, mut doc) = seeded_app();
            let before = doc.save();
            app.selected = Some(0);
            app.goto(&mut doc, Step::Confirm);
            app.progress = Progress {
                phase: JobPhase::Writing,
                write_frac: 0.5,
                verify_frac: 0.0,
                rate_bps: 1,
                error: None,
            };
            app.set_status(&doc, "local writer status");
            assert_eq!(
                doc.save(),
                before,
                "selection, station, status, and job progress are local"
            );
            let mut peer = AutoCommit::load(&before).unwrap();
            let mut peer_app = ImagerApp::new();
            peer_app.sync(&mut peer, &automerge::ROOT);
            assert_eq!(peer_app.step, Step::Source);
            assert_eq!(peer_app.selected, None);
            assert_eq!(peer_app.progress.phase, JobPhase::Idle);
            assert!(peer_app.status.is_empty());
        }

        // Phase `sync_reads_demo_state_at_source_step`.
        {
            let (app, _doc) = seeded_app();
            assert_eq!(app.targets.len(), 2);
            assert!(app.source.is_some());
            assert_eq!(app.step, Step::Source);
            assert_eq!(app.recent.len(), 3);
        }

        // Phase `live_app_starts_blank_with_no_fabricated_content`.
        {
            // The live path (no demo seed) starts genuinely empty: no drives are
            // invented and no image is pre-selected.
            let mut app = ImagerApp::new();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            assert!(app.targets.is_empty(), "no fabricated drives");
            assert!(app.source.is_none(), "no fabricated image");
            assert!(app.recent.is_empty(), "no fabricated recent history");
            assert_eq!(app.step, Step::Source);
        }

        // Phase `restart_drops_unsupervised_local_write_state_and_requires_new_review`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.step = Step::Write;
            app.progress = Progress {
                phase: JobPhase::Writing,
                write_frac: 0.8,
                verify_frac: 0.0,
                rate_bps: 64_000_000,
                error: None,
            };
            app.status = "Writing locally".into();
            let saved = doc.save();
            let mut restarted = ImagerApp::new();
            let mut restored_doc = AutoCommit::load(&saved).unwrap();
            restarted.sync(&mut restored_doc, &automerge::ROOT);
            assert_eq!(restarted.step, Step::Source);
            assert_eq!(restarted.progress.phase, JobPhase::Idle);
            assert!(restarted.selected.is_none());
            assert!(restarted.status.is_empty());
            assert!(!restarted.can_advance_with_writer(Step::Drive, true));
        }

        // Phase `write_operations_are_blocked_until_startup_state_is_reconciled`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.startup_write_state_reconciled = false;
            app.confirm_input.set_text("sdb");

            assert!(!app.can_advance_with_writer(Step::Confirm, true));
            let error = app
                .perform_operation(
                    &crate::actions::ImagerOperation::Write {
                        device: app.target().unwrap().dev_path(),
                        confirmation: "sdb".into(),
                    },
                    &mut doc,
                )
                .expect_err("unreconciled startup must not expose a write");
            assert!(error.contains("Restoring the previous image-write state"));

            let root = ui::resources::resources(&app, &doc)
                .into_iter()
                .find(|resource| resource.kind == "imager.sources")
                .expect("imager sources resource owns the write Effect");
            let write = root
                .actions
                .iter()
                .find(|action| action.id == "write")
                .expect("write action");
            assert!(!write.contract.availability.is_available());
        }

        // Phase `live_app_offers_no_tilo_release_to_write`.
        // A release row is a *write source for erasing a drive*, so the live app
        // must offer none of its own invention: the list comes from the document,
        // and a live (unseeded) document has nothing in it. Sample releases are
        // opt-in via `--demo` / `--shots` like every other sample surface here.
        {
            let mut app = ImagerApp::new();
            let mut doc = AutoCommit::new();
            app.sync(&mut doc, &automerge::ROOT);
            assert!(app.releases().is_empty(), "no fabricated Tilo releases");

            // ...and nothing on the Source step is clickable that would select one.
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            let offered = app
                .compute_layout()
                .hits
                .iter()
                .any(|(_, h)| matches!(h, Hit::Release(_)));
            assert!(
                !offered,
                "no release row may be hit-testable on the live path"
            );
            assert!(
                app.source.is_none(),
                "and nothing is pre-selected as the source"
            );
        }

        // Phase `demo_document_offers_its_seeded_releases`.
        // The gate is a gate, not a deletion: the demo document still offers them.
        {
            let (mut app, _doc) = seeded_app();
            assert_eq!(app.releases().len(), 2);
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            let offered = app
                .compute_layout()
                .hits
                .iter()
                .filter(|(_, h)| matches!(h, Hit::Release(_)))
                .count();
            assert_eq!(offered, 2, "each seeded release has a row to click");
        }
    }

    // ---- synthetic winit events for driving the real event path -----------

    fn char_ev(s: &str) -> WindowEvent {
        use winit::event::KeyEvent;
        use winit::keyboard::{Key, SmolStr};
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(SmolStr::new(s));
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = winit::event::ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(SmolStr::new(s));
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn named_key_ev(key: winit::keyboard::NamedKey) -> WindowEvent {
        use winit::event::KeyEvent;
        use winit::keyboard::{Key, KeyLocation, NativeKeyCode, PhysicalKey};
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(key);
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = winit::event::ElementState::Pressed;
        ev.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn wheel_ev(lines: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: unsafe { std::mem::zeroed() },
            delta: winit::event::MouseScrollDelta::LineDelta(0.0, lines),
            phase: winit::event::TouchPhase::Moved,
        }
    }

    fn cursor_ev(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    #[cfg(not(feature = "host-media-writer"))]
    fn click_ev() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: winit::event::ElementState::Pressed,
            button: winit::event::MouseButton::Left,
        }
    }

    /// Feed one event through the real `handle_event` path with throwaway
    /// framework out-params.
    fn feed(app: &mut ImagerApp, doc: &mut AutoCommit, ev: &WindowEvent) {
        let m = winit::event::Modifiers::default();
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let (mut h, mut ft) = (false, None);
        app.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &m,
                state: doc,
                path: &automerge::ROOT,
                handled: &mut h,
                focus_target: &mut ft,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    /// Click at a point: move the pointer there (so hover state settles), then
    /// press — exactly the real winit sequence the compositor delivers.
    #[cfg(not(feature = "host-media-writer"))]
    fn click_at(app: &mut ImagerApp, doc: &mut AutoCommit, p: Point) {
        feed(app, doc, &cursor_ev(p.x, p.y));
        feed(app, doc, &click_ev());
    }

    #[cfg(not(feature = "host-media-writer"))]
    fn center(r: Rect) -> Point {
        Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
    }

    /// The rect of the first hit matching `pred`, from this frame's layout.
    #[cfg(not(feature = "host-media-writer"))]
    fn hit_rect(app: &ImagerApp, pred: impl Fn(&Hit) -> bool) -> Rect {
        app.compute_layout()
            .hits
            .into_iter()
            .find(|(_, h)| pred(h))
            .unwrap_or_else(|| panic!("expected a matching hit target this frame"))
            .0
    }

    fn accessible_named<'a>(
        node: &'a ui::a11y::AccessNode,
        name: &str,
    ) -> Option<&'a ui::a11y::AccessNode> {
        if node.name.as_deref() == Some(name) {
            return Some(node);
        }
        node.children
            .iter()
            .find_map(|child| accessible_named(child, name))
    }

    fn accessible_focus_order(node: &ui::a11y::AccessNode, names: &mut Vec<String>) {
        if node.state.focusable {
            names.push(node.name.clone().unwrap_or_default());
        }
        for child in &node.children {
            accessible_focus_order(child, names);
        }
    }

    /// Accessible semantics, selection labels, live regions and tab order.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wizard_semantics_labels_live_regions_and_keyboard_order() {
        // Phase `main_canvas_semantics_expose_heading_sources_and_keyboard_order`.
        {
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);

            let tree = app.accessibility_tree();
            assert_eq!(tree.role, ui::a11y::Role::Window);
            assert_eq!(tree.name.as_deref(), Some("Disk Imager"));
            let heading = accessible_named(&tree, "Choose an image").expect("step heading");
            assert_eq!(heading.role, ui::a11y::Role::Heading { level: 1 });
            let selected = accessible_named(&tree, "Selected image").expect("selected image");
            assert!(
                selected
                    .value
                    .as_deref()
                    .is_some_and(|value| value.contains("Tilo"))
            );

            let stepper = accessible_named(&tree, "Source").expect("stepper");
            assert_eq!(stepper.role, ui::a11y::Role::ProgressBar);
            assert_eq!(stepper.value.as_deref(), Some("Step 1 of 6"));
            let mut order = Vec::new();
            accessible_focus_order(&tree, &mut order);
            let browse = order
                .iter()
                .position(|name| name == "Browse for an image")
                .expect("browse in focus order");
            let continue_button = order
                .iter()
                .position(|name| name == "Continue")
                .expect("continue in focus order");
            assert!(browse < continue_button);
        }

        // Phase `hardware_variants_have_distinct_semantic_selection_and_review_labels`.
        {
            use ui::a11y::Action as AccessAction;

            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            let mut generic = crate::model::sample_releases().remove(0);
            generic.architecture = "aarch64".into();
            let mut board = generic.clone();
            board.hardware_profile = "lenovo-83ed".into();
            let target = format!("imager:release:{}", board.selection_id());
            assert_ne!(generic.selection_id(), board.selection_id());
            app.releases = vec![generic, board.clone()];
            let tree = app.accessibility_tree();
            assert!(
                accessible_named(&tree, &format!("Download {}", board.display_name())).is_some()
            );
            assert!(app.perform_semantic_action(
                &target,
                &AccessAction::Activate,
                &mut doc,
                &automerge::ROOT
            ));
            assert!(board.matches_source(app.source.as_ref().unwrap()));
            assert!(!app.releases[0].matches_source(app.source.as_ref().unwrap()));
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Confirm);
            let tree = app.accessibility_tree();
            assert!(
                accessible_named(&tree, "Image")
                    .unwrap()
                    .value
                    .as_deref()
                    .unwrap()
                    .contains("lenovo-83ed only")
            );
        }

        // Phase `profile_and_confirmation_semantics_are_editable_and_focusable`.
        {
            use ui::a11y::Action as AccessAction;

            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Customize);

            let tree = app.accessibility_tree();
            let mut order = Vec::new();
            accessible_focus_order(&tree, &mut order);
            let profile_order = [
                "Hostname",
                "Full name",
                "Username",
                "Language and locale",
                "Time zone",
                "Keyboard layout",
                "Filesystem",
                "Preview embedded setup",
            ];
            let positions = profile_order.map(|name| {
                order
                    .iter()
                    .position(|candidate| candidate == name)
                    .unwrap_or_else(|| panic!("missing accessible profile control {name}"))
            });
            assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
            for name in [
                "Language and locale",
                "Time zone",
                "Keyboard layout",
                "Filesystem",
            ] {
                assert_eq!(
                    accessible_named(&tree, name).expect("profile chooser").role,
                    ui::a11y::Role::ComboBox,
                    "{name} is exposed as a chooser, not unrestricted text"
                );
            }

            assert!(app.perform_semantic_action(
                "imager:profile:hostname",
                &AccessAction::SetValue("accessible-host".into()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(ImagerDoc::profile(&doc).hostname, "accessible-host");

            for (target, chosen, expected) in [
                ("imager:profile:locale", "de_DE.UTF-8", "de_DE.UTF-8"),
                ("imager:profile:timezone", "Europe/Berlin", "Europe/Berlin"),
                ("imager:profile:keyboard", "German (QWERTZ)", "de"),
            ] {
                assert!(app.perform_semantic_action(
                    target,
                    &AccessAction::SetValue(chosen.into()),
                    &mut doc,
                    &automerge::ROOT,
                ));
                let profile = ImagerDoc::profile(&doc);
                let actual = match target {
                    "imager:profile:locale" => profile.locale,
                    "imager:profile:timezone" => profile.timezone,
                    _ => profile.keyboard,
                };
                assert_eq!(actual, expected);
            }
            let timezone_before = ImagerDoc::profile(&doc).timezone;
            assert!(app.perform_semantic_action(
                "imager:profile:timezone",
                &AccessAction::SetValue("Europe/Not_A_Real_Zone".into()),
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(ImagerDoc::profile(&doc).timezone, timezone_before);
            assert!(app.status.contains("Choose a listed Time zone"));

            app.rect = Rect::new(0.0, 0.0, 320.0, 480.0);
            app.scroll_y = 0.0;
            assert!(app.perform_semantic_action(
                "imager:profile:keyboard",
                &AccessAction::Focus,
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(app.scroll_y > 0.0, "assistive focus scrolls into view");

            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            app.goto(&mut doc, Step::Confirm);
            let target_name = app.target().expect("target").name.clone();
            let confirm = accessible_named(&app.accessibility_tree(), "Confirm the target device")
                .expect("confirmation input")
                .clone();
            assert_eq!(confirm.role, ui::a11y::Role::TextInput);
            assert!(confirm.state.required);
            assert!(app.perform_semantic_action(
                "imager:hit:confirm-field",
                &AccessAction::SetValue(target_name),
                &mut doc,
                &automerge::ROOT,
            ));
            assert!(app.confirm_matches());
            assert!(app.confirm_input.is_focused());
        }

        // Phase `write_progress_and_status_are_live_regions`.
        {
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            app.step = Step::Write;
            app.status = "Verifying written data".into();
            app.progress = Progress {
                phase: JobPhase::Verifying,
                write_frac: 1.0,
                verify_frac: 0.4,
                rate_bps: 1,
                error: None,
            };

            let tree = app.accessibility_tree();
            let status = accessible_named(&tree, "Status").expect("live status");
            assert_eq!(status.live, Some(ui::a11y::Politeness::Polite));
            let progress = accessible_named(&tree, "Verifying image").expect("live progress");
            assert_eq!(progress.role, ui::a11y::Role::ProgressBar);
            assert!(progress.state.busy);
            assert_eq!(progress.live, Some(ui::a11y::Politeness::Polite));
            assert!(
                progress
                    .value
                    .as_deref()
                    .is_some_and(|value| value.contains("percent"))
            );
            assert_eq!(
                accessible_named(&tree, "Write phase")
                    .expect("write phase progress")
                    .value
                    .as_deref(),
                Some("100 percent")
            );
            assert_eq!(
                accessible_named(&tree, "Verify phase")
                    .expect("verify phase progress")
                    .value
                    .as_deref(),
                Some("40 percent")
            );
        }

        // Phase `tab_reaches_every_profile_control_and_both_footer_actions`.
        {
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 320.0, 480.0);
            app.goto(&mut doc, Step::Customize);
            let tab = named_key_ev(winit::keyboard::NamedKey::Tab);

            for slot in 0..7 {
                feed(&mut app, &mut doc, &tab);
                assert_eq!(app.customize.keyboard_slot(), Some(slot));
            }
            assert!(app.scroll_y > 0.0, "lower controls are scrolled into view");

            feed(&mut app, &mut doc, &tab);
            assert_eq!(app.keyboard_hit, Some(Hit::Back));
            feed(&mut app, &mut doc, &tab);
            assert_eq!(app.keyboard_hit, Some(Hit::Next));
            feed(
                &mut app,
                &mut doc,
                &named_key_ev(winit::keyboard::NamedKey::Enter),
            );
            assert_eq!(app.step, Step::Confirm, "focused Continue activates");
        }
    }

    /// The stepper, the step body and the footer at compact and wide layouts.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn stepper_body_and_footer_stay_inside_the_viewport() {
        // Phase `the_stepper_sits_in_the_content_column_at_every_width`.
        for width in [1280.0, 800.0, 320.0] {
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, width, 640.0);
            let lay = app.compute_layout();
            assert!(lay.stepper.x0 >= app.rect.x0 && lay.stepper.x1 <= app.rect.x1);
            assert!(lay.stepper.height() > 0.0);
            assert!(
                lay.viewport_content.y0 >= lay.stepper.y1,
                "the body starts under the stepper at width {width}"
            );
            assert!(lay.content.width() <= READABLE_WIDTH + 1e-6);
        }

        // Phase `step_content_starts_below_the_measured_title`.
        // The Image drop zone starts below the measured title line plus the
        // title-to-content gap, at 1× and at 2×, and the Confirm review
        // follows the same rule.
        {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut tr = TextRenderer::new();
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);

            let browse_top = |app: &ImagerApp| {
                let lay = app.compute_layout();
                let browse = lay
                    .hits
                    .iter()
                    .find_map(|(rect, hit)| matches!(hit, Hit::PickFile).then_some(*rect))
                    .unwrap();
                browse.y0 - lay.content.y0
            };
            app.adjust_layout_with_measure(&mut taffy, node, &mut tr);
            let title = f64::from(
                tr.measure_style("Ag", None, &app.theme.text(TextRole::Title))
                    .1,
            );
            let at_1x = browse_top(&app);
            assert!(
                (at_1x - (title + app.bands.gaps.section)).abs() < 1e-6,
                "drop zone starts after the measured title"
            );

            app.theme = Theme::dark().with_text_scale(2.0);
            assert!(app.adjust_layout_with_measure(&mut taffy, node, &mut tr));
            let at_2x = browse_top(&app);
            assert!(at_2x > at_1x * 1.5, "doubled text moves the content down");

            app.theme = Theme::dark();
            app.goto(&mut doc, Step::Confirm);
            app.adjust_layout_with_measure(&mut taffy, node, &mut tr);
            let lay = app.compute_layout();
            assert!(
                (app.confirm_review_rect(lay.content).y0 - app.body_top(lay.content)).abs() < 1e-6
            );
            // The scroll extent covers the field and its note, so the
            // confirmation field cannot ride up into the review.
            let extent = app.content_extent(app.content_viewport_rect());
            let confirm_field = lay
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::ConfirmField).then_some(*rect))
                .unwrap();
            assert!(
                lay.content.y0 + extent >= app.confirm_note_top(lay.content) + app.bands.caption_h
            );
            assert!(confirm_field.y0 >= app.confirm_banner_rect(lay.content).y1);
        }

        // Phase `compact_footer_actions_share_the_column_without_touching`.
        {
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 320.0, 640.0);
            for step in [Step::Drive, Step::Customize, Step::Confirm] {
                app.goto(&mut doc, step);
                let lay = app.compute_layout();
                let back = lay.back.unwrap();
                let primary = lay.primary.unwrap();
                assert!(back.x0 >= lay.content.x0);
                assert!(primary.x1 <= lay.content.x1);
                assert!(back.x1 + app.bands.gaps.sibling <= primary.x0 + 1e-6);
                assert!(back.width() >= 130.0 && primary.width() >= 130.0);
            }
        }

        // Phase `wide_footer_actions_are_as_wide_as_their_labels`.
        {
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            app.goto(&mut doc, Step::Drive);
            let lay = app.compute_layout();
            let back = lay.back.unwrap();
            let primary = lay.primary.unwrap();
            assert!((back.width() - app.bands.back_w).abs() < 1e-6);
            assert!((primary.width() - app.bands.primary_w).abs() < 1e-6);
            assert!((back.height() - app.bands.control_h).abs() < 1e-6);
        }
    }

    /// Body blocks, footers, scrolling and failure detail layout.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn step_body_and_footer_reflow_without_overlap() {
        // Phase `image_blocks_follow_the_text_scale_without_overlapping`.
        // Every hit rect is a measured band, consecutive blocks never
        // overlap, the selected release is not shown twice and the scroll
        // extent covers the taller content.
        {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut tr = TextRenderer::new();
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);

            let blocks = |app: &ImagerApp| {
                let lay = app.compute_layout();
                let mut v: Vec<(Rect, Hit)> = lay
                    .hits
                    .iter()
                    .filter(|(_, h)| matches!(h, Hit::PickFile | Hit::Release(_) | Hit::Recent(_)))
                    .copied()
                    .collect();
                v.sort_by(|a, b| a.0.y0.total_cmp(&b.0.y0));
                v
            };
            for scale in [1.0, 2.0] {
                app.theme = Theme::dark().with_text_scale(scale);
                app.adjust_layout_with_measure(&mut taffy, node, &mut tr);
                let all = blocks(&app);
                assert_eq!(
                    all.iter()
                        .filter(|(_, h)| matches!(h, Hit::Release(_)))
                        .count(),
                    2
                );
                // The recent list leaves out the release that is listed
                // above it.
                assert_eq!(
                    all.iter()
                        .filter(|(_, h)| matches!(h, Hit::Recent(_)))
                        .count(),
                    2
                );
                let target = f64::from(app.theme.tokens.metrics.min_pointer_target);
                for (rect, hit) in &all {
                    if !matches!(hit, Hit::PickFile) {
                        assert!((rect.height() - app.bands.row_h).abs() < 1e-6);
                    }
                    assert!(rect.height() >= target);
                }
                for pair in all.windows(2) {
                    assert!(
                        pair[1].0.y0 >= pair[0].0.y1 - 1e-6,
                        "Image blocks never overlap at {scale}×"
                    );
                }
                let viewport = app.content_viewport_rect();
                let last = all.last().unwrap().0;
                let lay = app.compute_layout();
                assert!(
                    lay.content.y0 + app.content_extent(viewport)
                        >= last.y1 + app.bands.gaps.section - 1e-6
                );
            }
        }

        // Phase `confirm_field_follows_the_warning`.
        // The Confirm blocks stack on their measured bands at both content
        // widths: the warning follows the review, the field follows the
        // warning and the write-setup form under its caption, and the hit
        // rect is the painted rect.
        {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut tr = TextRenderer::new();
            for width in [1200.0, 320.0] {
                let (mut app, mut doc) = seeded_app();
                app.rect = Rect::new(0.0, 0.0, width, 800.0);
                app.select_target(&doc, 0);
                app.goto(&mut doc, Step::Confirm);
                app.adjust_layout_with_measure(&mut taffy, node, &mut tr);
                let lay = app.compute_layout();
                let field = lay
                    .hits
                    .iter()
                    .find_map(|(rect, hit)| matches!(hit, Hit::ConfirmField).then_some(*rect))
                    .unwrap();
                let review = app.confirm_review_rect(lay.content);
                let banner = app.confirm_banner_rect(lay.content);
                let g = app.bands.gaps;
                assert!((banner.y0 - (review.y1 + g.group)).abs() < 1e-6);
                let setup = app
                    .write_setup
                    .extent(lay.content, app.write_setup_top(lay.content));
                assert!(
                    (field.y0 - (banner.y1 + g.group + setup + app.bands.label_h + g.tight)).abs()
                        < 1e-6,
                    "field follows the warning and write-setup form at width {width}"
                );
                assert!((field.height() - app.bands.control_h).abs() < 1e-6);
                assert_eq!(field, app.confirm_field_rect(lay.content));
                assert_eq!(
                    app.review_rows().len(),
                    6,
                    "image, size, drive, device, space, setup"
                );
            }
        }

        // Phase `compact_drive_controls_never_touch`.
        {
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 320.0, 640.0);
            app.goto(&mut doc, Step::Drive);
            let drive = app.compute_layout();
            let rescan = drive
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::Rescan).then_some(*rect))
                .unwrap();
            let target = drive
                .hits
                .iter()
                .find_map(|(rect, hit)| matches!(hit, Hit::Target(_)).then_some(*rect))
                .unwrap();
            assert!(rescan.x1 <= drive.content.x1 + 1e-6);
            assert!(target.y0 >= rescan.y1);
        }

        // Phase `short_viewport_scrolls_body_without_moving_sticky_footer`.
        {
            let (mut app, mut doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 320.0, 480.0);
            app.goto(&mut doc, Step::Customize);
            let before = app.compute_layout();
            let footer_before = before.primary.unwrap();
            assert!(app.max_scroll() > 100.0);

            feed(&mut app, &mut doc, &cursor_ev(160.0, 220.0));
            feed(&mut app, &mut doc, &wheel_ev(-3.0));
            assert!(app.scroll_y > 0.0);
            let after = app.compute_layout();
            assert!(after.content.y0 < before.content.y0);
            assert_eq!(after.primary, Some(footer_before));
            assert_eq!(after.viewport_content, before.viewport_content);
        }

        // Phase `failure_detail_is_measured_bounded_and_visible_at_compact_width`.
        {
            let mut renderer = TextRenderer::new();
            let style = Theme::dark().text(TextRole::Body);
            let detail = "Could not start the authorized writer because the selected removable drive disappeared while the operating system was preparing it. Reconnect it and try again.";
            let max_width = 280.0;
            let lines =
                ImagerApp::failure_detail_lines(&mut renderer, &style, detail, max_width, 2);

            assert_eq!(
                lines.len(),
                2,
                "compact failure copy uses both visible lines"
            );
            assert!(
                lines
                    .iter()
                    .all(|line| renderer.measure_style(line, None, &style).0 <= max_width),
                "no failure-detail line paints beyond its content bounds"
            );
            assert!(
                lines.join(" ").contains("authorized writer"),
                "the actionable cause is visible rather than replaced by generic compact copy"
            );
            assert!(
                lines.last().is_some_and(|line| line.ends_with('…')),
                "bounded overflow is signalled"
            );
        }
    }

    /// Selection, capability, profile validity and confirmation gating.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wizard_advance_and_confirm_require_valid_choices_and_capability() {
        // Phase `advancing_requires_selections`.
        {
            let (mut app, mut doc) = seeded_app();
            // Source is seeded → can advance to Drive.
            app.advance(&mut doc);
            assert_eq!(app.step, Step::Drive);
            // No target selected yet → stays on Drive.
            app.advance(&mut doc);
            assert_eq!(app.step, Step::Drive);
            app.select_target(&doc, 0);
            app.advance(&mut doc);
            // Seeded source is an installable Tilo image → goes through Customize.
            assert_eq!(app.step, Step::Customize);
            app.advance(&mut doc);
            assert_eq!(app.step, Step::Confirm);
        }

        // Phase `privileged_helper_capability_enables_confirm_but_disabled_backend_does_not`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Confirm);
            app.confirm_input.set_text("sdb");

            assert!(crate::write::WriteBackend::PrivilegedHelper.is_available());
            assert!(app.can_advance_with_writer(
                Step::Confirm,
                crate::write::WriteBackend::PrivilegedHelper.is_available()
            ));
            assert!(!app.can_advance_with_writer(
                Step::Confirm,
                crate::write::WriteBackend::Disabled.is_available()
            ));
        }

        // Phase `invalid_media_profile_blocks_confirm_and_direct_write`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.profile.hostname = "bad host".into();
            app.goto(&mut doc, Step::Customize);

            app.advance(&mut doc);
            assert_eq!(app.step, Step::Customize);
            assert!(app.status.contains("Hostname"));

            // A direct operation dispatch must repeat the gate rather than relying
            // only on wizard navigation.
            app.goto(&mut doc, Step::Confirm);
            app.confirm_input.set_text("sdb");
            app.start_write(&mut doc);
            assert_eq!(app.step, Step::Confirm);
            assert!(app.job_rx.is_none());
            assert!(app.status.contains("Hostname"));
        }

        // Phase `plain_iso_skips_customize`.
        {
            let (mut app, mut doc) = seeded_app();
            let iso = app
                .recent
                .iter()
                .find(|s| s.kind == SourceKind::IsoFile)
                .cloned()
                .unwrap();
            app.select_source(&mut doc, &iso);
            app.advance(&mut doc); // → Drive
            app.select_target(&doc, 0);
            app.advance(&mut doc); // installable == false → straight to Confirm
            assert_eq!(app.step, Step::Confirm);
            assert!(!app.customizable());
        }

        // Phase `local_tilo_claim_cannot_unlock_preconfiguration`.
        {
            let (mut app, mut doc) = seeded_app();
            let official = crate::model::sample_source();
            let mut local = official.clone();
            local.kind = SourceKind::IsoFile;
            local.path = "/tmp/tilo-official-looking.iso".into();
            local.identity = None;
            app.select_source(&mut doc, &local);
            assert!(!app.customizable());
            assert_eq!(app.action, Action::Flash);

            app.source = Some(official);
            assert!(app.customizable());
        }

        // Phase `official_installable_write_stages_handoff_instead_of_refusing`.
        // Catalog-authenticated images used to refuse Wi-Fi / enrollment on
        // Confirm. They must stage the same handoff local images do; tests
        // then stop at the disabled write backend, never at an installer-stick
        // refusal.
        {
            let (mut app, mut doc) = seeded_app();
            assert!(app.customizable(), "seeded source is official");
            assert!(
                app.offers_write_setup(),
                "official Confirm must collect Wi-Fi and enrollment"
            );
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Confirm);
            app.confirm_input.set_text("sdb");
            app.profile.wifi_ssid = "studio".into();
            app.wifi_psk = "correct-horse".into();
            let err = app
                .perform_operation(
                    &crate::actions::ImagerOperation::Write {
                        device: "/dev/sdb".into(),
                        confirmation: "sdb".into(),
                    },
                    &mut doc,
                )
                .expect_err("privileged writer is off in unit tests");
            assert!(
                !err.to_lowercase().contains("installer stick"),
                "official installable write must not refuse handoff: {err}"
            );
            assert!(
                err.to_lowercase().contains("disabled") || err.to_lowercase().contains("backend"),
                "expected write-disabled after staging handoff, got: {err}"
            );
            assert!(app.job_rx.is_none());
            assert_eq!(app.step, Step::Confirm);
        }

        // Phase `write_refused_without_matching_confirmation`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0); // sdb
            app.goto(&mut doc, Step::Confirm);
            app.confirm_input.set_text("wrong");
            app.start_write(&mut doc);
            assert_eq!(app.progress.phase, JobPhase::Idle, "no job started");
            assert!(app.job_rx.is_none());
            assert_eq!(app.step, Step::Confirm, "stays on the Confirm station");
            assert!(app.status.to_lowercase().contains("confirm"));
        }
    }

    /// Release download job presentation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn release_download_is_local_visible_cancellable_and_keeps_the_loop_awake() {
        // Phase `pending_release_download_keeps_the_event_loop_awake`.
        {
            let (mut app, _doc) = seeded_app();
            assert!(!app.needs_animation_frame());

            let (_tx, rx) = std::sync::mpsc::channel();
            app.release_rx = Some(rx);
            assert!(app.needs_animation_frame());

            app.release_rx = None;
            assert!(!app.needs_animation_frame());
        }

        // Phase `active_release_download_replaces_selection_with_an_accessible_cancel_action`.
        {
            let (mut app, _doc) = seeded_app();
            app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
            let (_tx, rx) = std::sync::mpsc::channel();
            app.release_rx = Some(rx);
            app.release_control = Some(crate::release::DownloadControl::default());
            app.release_download_index = Some(0);
            app.release_download_progress = Some((25, 100));
            app.release_download_stage = Some("Downloading");

            let layout = app.compute_layout();
            assert!(
                layout
                    .hits
                    .iter()
                    .any(|(_, hit)| *hit == Hit::CancelDownload)
            );
            assert!(
                !layout
                    .hits
                    .iter()
                    .any(|(_, hit)| matches!(hit, Hit::Release(_)))
            );
            let tree = app.accessibility_tree();
            assert!(accessible_named(&tree, "Cancel Tilo installer download").is_some());
        }

        // Phase `release_progress_is_local_visible_and_terminal_events_clear_the_job`.
        {
            let (mut app, mut doc) = seeded_app();
            let (tx, rx) = std::sync::mpsc::channel();
            app.release_rx = Some(rx);
            app.release_control = Some(crate::release::DownloadControl::default());
            app.release_download_index = Some(0);
            tx.send(crate::release::DownloadEvent::Downloading {
                received: 25,
                total: 100,
            })
            .unwrap();
            app.drain_release(&mut doc);
            assert_eq!(app.release_download_progress, Some((25, 100)));
            assert!(app.status.contains("25%"));

            tx.send(crate::release::DownloadEvent::Finished(Err(
                "Tilo installer download cancelled.".into(),
            )))
            .unwrap();
            app.drain_release(&mut doc);
            assert!(app.release_rx.is_none());
            assert!(app.release_control.is_none());
            assert!(app.status.contains("cancelled"));
        }
    }

    /// The primary interaction, proven end to end by clicking the real
    /// on-screen targets: walk Source → Drive → Customize → Confirm. Every transition
    /// goes through the actual hit-testing + dispatch path, not a shortcut.
    #[test]
    #[cfg(not(feature = "host-media-writer"))]
    fn clicking_through_the_wizard_refuses_a_fake_write_when_backend_is_disabled() {
        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
        assert_eq!(app.step, Step::Source);

        // Source → Drive: click Continue.
        let primary = app.compute_layout().primary.unwrap();
        click_at(&mut app, &mut doc, center(primary));
        assert_eq!(app.step, Step::Drive);

        // Pick the USB stick by clicking its card.
        let usb = hit_rect(&app, |h| matches!(h, Hit::Target(0)));
        click_at(&mut app, &mut doc, center(usb));
        assert_eq!(app.selected, Some(0));

        // Drive → Customize (the seeded Tilo image is customizable).
        let primary = app.compute_layout().primary.unwrap();
        click_at(&mut app, &mut doc, center(primary));
        assert_eq!(app.step, Step::Customize);

        // Customize → Confirm.
        let primary = app.compute_layout().primary.unwrap();
        click_at(&mut app, &mut doc, center(primary));
        assert_eq!(app.step, Step::Confirm);

        // Arm the write: click the typed-name field, then type the device name.
        let field = hit_rect(&app, |h| matches!(h, Hit::ConfirmField));
        click_at(&mut app, &mut doc, center(field));
        assert!(app.confirm_input.is_focused());
        app.confirm_input.set_text("wrong target");
        app.confirm_input.select_all();
        for s in ["s", "d", "b"] {
            feed(&mut app, &mut doc, &char_ev(s));
        }
        assert!(
            app.confirm_matches(),
            "selection replacement through TextInput armed the write"
        );
        assert!(
            automerge::ReadDoc::get(&doc, &automerge::ROOT, "confirm")
                .unwrap()
                .is_none(),
            "destructive consent is never written to the shared document"
        );

        if crate::write::allow_write() {
            // Never point this UI test at a real block device.
            return;
        }

        // The primary control stays disabled: a matching phrase cannot turn an
        // unavailable backend into a synthetic success.
        let primary = app.compute_layout().primary.unwrap();
        click_at(&mut app, &mut doc, center(primary));
        assert_eq!(app.step, Step::Confirm);
        assert_eq!(app.progress.phase, JobPhase::Idle);
        assert!(app.job_rx.is_none());
    }

    #[test]
    #[cfg(not(feature = "host-media-writer"))]
    fn matching_confirmation_does_not_simulate_a_disabled_write() {
        if crate::write::allow_write() {
            return;
        }
        let (mut app, mut doc) = seeded_app();
        app.select_target(&doc, 0); // sdb
        app.goto(&mut doc, Step::Confirm);
        app.confirm_input.set_text("sdb");
        app.start_write(&mut doc);
        assert_eq!(app.progress.phase, JobPhase::Idle);
        assert_eq!(app.step, Step::Confirm);
        assert!(app.job_rx.is_none());
        assert!(app.status.contains("disabled"));
    }

    /// Portal sources, external drives, rescans and forbidden disks.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn source_and_target_selection_accept_only_inspected_external_media() {
        // Phase `portal_open_selects_inspected_source`.
        {
            let (mut app, mut doc) = seeded_app();
            // The recording host answers with a real Tilo-looking image.
            let f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
            std::fs::write(f.path(), b"tilo payload for the imager test").unwrap();
            let (services, recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);
            recorder.respond_to_pick(PortalOutcome::Opened(f.path().to_path_buf()));
            app.open_file_picker(&doc);
            assert_eq!(recorder.pick_requests()[0].title(), "Choose a disk image");
            app.drain_portal(&mut doc);
            // A `.iso` file → a valid source is selected.
            assert!(app.source.is_some());
            assert_eq!(app.source.as_ref().unwrap().kind, SourceKind::IsoFile);
        }

        // Phase `drive_step_only_offers_external_installation_media`.
        {
            let (app, _doc) = seeded_app();
            // Sample has one USB (removable) + one NVMe (fixed).
            assert_eq!(app.visible_targets().len(), 1);
            assert_eq!(app.removable_count(), 1);
        }

        // Phase `authoritative_empty_rescan_clears_stale_target_and_consent`.
        {
            let (mut app, doc) = seeded_app();
            app.select_target(&doc, 0);
            app.confirm_input.set_text("sdb");

            app.apply_drive_inventory(Vec::new());

            assert!(app.targets.is_empty());
            assert_eq!(app.selected, None);
            assert!(app.confirm_input.value().is_empty());
        }

        // Phase `rescan_preserves_only_the_same_physical_target`.
        {
            let (mut app, doc) = seeded_app();
            app.select_target(&doc, 0);
            app.confirm_input.set_text("sdb");
            let selected = app.target().unwrap().clone();
            let mut reordered = crate::model::sample_targets();
            reordered.swap(0, 1);

            app.apply_drive_inventory(reordered);

            assert_eq!(app.selected, Some(1));
            assert_eq!(
                app.target().and_then(|drive| drive.stable_id.as_deref()),
                selected.stable_id.as_deref()
            );
            assert_eq!(app.confirm_input.value(), "sdb");

            let mut replacement = selected;
            replacement.stable_id = Some("different-usb-stick".into());
            app.apply_drive_inventory(vec![replacement]);

            assert_eq!(
                app.selected, None,
                "a reused OS device slot is not the reviewed drive"
            );
            assert!(
                app.confirm_input.value().is_empty(),
                "consent never carries to replacement hardware"
            );

            let mut unidentified_before = crate::model::sample_targets()[0].clone();
            unidentified_before.stable_id = None;
            app.targets = vec![unidentified_before.clone()];
            app.selected = Some(0);
            app.confirm_input.set_text(&unidentified_before.name);
            app.apply_drive_inventory(vec![unidentified_before]);
            assert_eq!(
                app.selected, None,
                "a drive without stable identity must be chosen again after rescan"
            );
            assert!(app.confirm_input.value().is_empty());
        }

        // Phase `internal_and_system_disks_can_never_be_selected`.
        {
            let mut app = ImagerApp::new();
            let mut doc = AutoCommit::new();
            let mut targets = crate::model::sample_targets();
            targets[1].is_system = true; // the internal NVMe is the system disk
            app.targets = targets.clone();
            ImagerDoc::seed(&mut doc, &targets);
            ImagerDoc::set_source(&mut doc, Some(&crate::model::sample_source()));
            app.sync(&mut doc, &automerge::ROOT);

            app.dispatch_hit(&mut doc, Hit::Target(1));
            assert_eq!(app.selected, None, "the system disk is never selectable");
            assert!(app.status.to_lowercase().contains("system disk"));
        }
    }

    /// Set the Customize step up with one field cleared and focused, ready for
    /// the shared panel to receive keystrokes through the real event path.
    fn customize_with_field_focused(field: crate::customize::CzField) -> (ImagerApp, AutoCommit) {
        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        // Clear the target field so the typed assertion is exact.
        let mut p = ImagerDoc::profile(&doc);
        if field == crate::customize::CzField::Hostname {
            p.hostname.clear();
        }
        ImagerDoc::set_profile(&mut doc, &p);
        app.step = Step::Customize;
        app.sync(&mut doc, &automerge::ROOT); // reads the profile + step back
        assert_eq!(
            app.step,
            Step::Customize,
            "the panel only handles events on this step"
        );
        assert!(app.customizable(), "the seeded Tilo image is customizable");
        app.customize.focus_for_test(field);
        (app, doc)
    }

    /// Hostname typing, keyboard chooser and preview.
    #[test]
    fn profile_panel_edits_persist_through_the_real_event_path() {
        // Phase `typing_into_hostname_persists_to_profile_through_the_panel`.
        {
            use crate::customize::CzField;
            let (mut app, mut doc) = customize_with_field_focused(CzField::Hostname);
            for s in ["l", "a", "b", "0", "1"] {
                feed(&mut app, &mut doc, &char_ev(s));
            }
            assert_eq!(
                ImagerDoc::profile(&doc).hostname,
                "lab01",
                "persisted to the doc"
            );
            assert_eq!(
                app.customize.field_value(CzField::Hostname),
                "lab01",
                "the input holds it"
            );
        }

        // Phase `keyboard_chooser_filters_and_commits_through_the_real_event_path`.
        {
            use crate::customize::CzField;
            use winit::keyboard::NamedKey;

            let (mut app, mut doc) = customize_with_field_focused(CzField::Keyboard);
            feed(&mut app, &mut doc, &named_key_ev(NamedKey::Enter));
            for character in ["g", "e", "r", "m", "a", "n"] {
                feed(&mut app, &mut doc, &char_ev(character));
            }
            feed(&mut app, &mut doc, &named_key_ev(NamedKey::Enter));

            assert_eq!(ImagerDoc::profile(&doc).keyboard, "de");
            assert_eq!(
                app.customize.field_value(CzField::Keyboard),
                "German (QWERTZ)"
            );
        }

        // Phase `preview_shows_the_embedded_profile`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 1);
            app.export_config(&mut doc);
            let text = app.export_dialog.text();
            assert!(text.contains("\"schema\": 1"));
            assert!(text.contains("\"hostname\""));
            assert!(!text.contains("password"));
            assert!(app.export_dialog.is_open());
        }
    }

    /// Write another, failure, cancellation and cancel races.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn write_completion_failure_and_cancel_return_to_a_truthful_state() {
        // Phase `write_another_resets_to_source`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.progress = Progress {
                phase: JobPhase::Success,
                write_frac: 1.0,
                verify_frac: 1.0,
                rate_bps: 0,
                error: None,
            };
            app.write_another(&mut doc);
            assert_eq!(app.step, Step::Source);
            assert_eq!(app.progress.phase, JobPhase::Idle);
        }

        // Phase `failed_write_keeps_its_cause_and_returns_to_a_fresh_review`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Write);
            app.progress = Progress {
                phase: JobPhase::Writing,
                write_frac: 0.4,
                verify_frac: 0.0,
                rate_bps: 0,
                error: None,
            };
            let (tx, rx) = std::sync::mpsc::channel();
            app.job_rx = Some(rx);
            tx.send(Progress {
                phase: JobPhase::Failed,
                write_frac: 0.4,
                verify_frac: 0.0,
                rate_bps: 0,
                error: Some(
                    "Could not write the drive: the selected drive was disconnected.".into(),
                ),
            })
            .unwrap();

            app.drain_job(&mut doc);
            assert_eq!(app.step, Step::Done);
            assert_eq!(app.progress.phase, JobPhase::Failed);
            assert!(
                app.status.contains("disconnected"),
                "the privileged writer's actionable cause reaches the UI"
            );
            let tree = app.accessibility_tree();
            let result = accessible_named(&tree, "Write failure details")
                .expect("accessible terminal result");
            assert!(
                result
                    .value
                    .as_deref()
                    .is_some_and(|value| value.contains("disconnected"))
            );

            app.confirm_input.set_text("sdb");
            app.write_another(&mut doc);
            assert_eq!(app.step, Step::Confirm, "failure returns to review");
            assert!(app.source.is_some(), "the verified source is preserved");
            assert_eq!(app.selected, Some(0), "the selected drive is preserved");
            assert!(
                app.confirm_input.value().is_empty(),
                "retry requires fresh destructive consent"
            );
            assert_eq!(app.progress.phase, JobPhase::Idle);
        }

        // Phase `active_write_cancel_operation_requests_a_safe_stop`.
        {
            let (mut app, mut doc) = seeded_app();
            let control = JobControl::new();
            app.job_control = Some(control.clone());
            app.goto(&mut doc, Step::Write);

            app.perform_operation(&crate::actions::ImagerOperation::Cancel, &mut doc)
                .unwrap();

            assert!(control.is_cancelled());
            assert!(app.status.contains("Keep the drive connected"));
            let footer = app.a11y_footer_controls(&app.compute_layout());
            let cancel = footer
                .iter()
                .find(|node| node.name.as_deref() == Some("Cancel image write"))
                .expect("write screen exposes a cancel button");
            assert!(cancel.state.disabled);
        }

        // Phase `cancel_wins_a_race_with_an_already_queued_success`.
        {
            let (mut app, mut doc) = seeded_app();
            let control = JobControl::new();
            control.cancel();
            app.job_control = Some(control);
            app.progress.phase = JobPhase::Verifying;
            app.goto(&mut doc, Step::Write);
            let (tx, rx) = std::sync::mpsc::channel();
            app.job_rx = Some(rx);
            tx.send(Progress {
                phase: JobPhase::Success,
                write_frac: 1.0,
                verify_frac: 1.0,
                rate_bps: 0,
                error: None,
            })
            .unwrap();

            app.drain_job(&mut doc);

            assert_eq!(app.progress.phase, JobPhase::Failed);
            assert!(app.progress.error.as_deref().is_some_and(|message| {
                message.contains("cancelled") && message.contains("incomplete")
            }));
            assert_eq!(app.step, Step::Done);
        }
    }
}
