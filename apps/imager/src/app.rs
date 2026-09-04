//! Root widget for the Disk Imager (imager) app.
//!
//! `ImagerApp` is a single self-drawing "canvas" widget that renders a guided
//! **wizard** with a six-station step indicator —
//! Source → Drive → Customize → Confirm → Write → Done — and a Back /
//! primary-action footer for creating a verified USB/SD installation medium.
//!
//! * **Source** — pick a local `.iso` / `.img` through the host XDG portal
//!   (never a bespoke picker, per HIG §5), or a known qOS release.
//! * **Drive** — choose external media from the *real* block devices (`lsblk`);
//!   internal and system disks are never eligible. Installation onto those is
//!   handled after boot by the live qOS installer.
//! * **Customize** (qOS only) — the shared guided-customization step, delegated
//!   in full to [`CustomizePanel`](crate::customize); a plain ISO skips it.
//! * **Confirm** — a read-only review of every choice, an erase warning, and
//!   the typed device-name gate.
//! * **Write** — a real, byte-for-byte streaming copy with live progress and a
//!   read-back **verify** pass.
//! * **Done** — the verified / failed outcome with a fresh-start affordance.
//!
//! Every write is doubly gated: the typed confirmation at Confirm, then native
//! host authentication for a short-lived privileged writer. The wizard itself
//! stays unprivileged; tests and shots cannot start the helper.
//!
//! All persistent state (selected source/target, the live profile, the typed
//! confirmation, the current step, job progress) lives in the Automerge
//! document so it is undoable and replicates to collaborators. The transient
//! job/portal channels are the only non-CRDT state.

use std::collections::HashMap;
use std::fmt::Write as _;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::WindowEvent;

use ui::ColorCompat;
use ui::permissions::PermissionContext;
#[cfg(target_os = "linux")]
use ui::portal::{FileFilter, PickRequest};
use ui::portal::{PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

use crate::customize::{CustomizePanel, CzField};
use crate::doc::ImagerDoc;
use crate::export_dialog::ExportDialog;
use crate::model::{
    Action, InstallProfile, JobPhase, Progress, QosRelease, Source, SourceKind, Step, TargetDrive,
    TargetKind, fmt_eta, fmt_rate, fmt_size,
};
use crate::write::{WriteRequest, spawn_job};

// ---- layout constants (logical px) -------------------------------------
const OUTER: f64 = 28.0;
// The compositor draws the window chrome + app name (OS owns chrome), so the
// app itself opens straight into the step indicator — no in-app title bar.
const CRUMB_TOP: f64 = 26.0;
const CRUMB_H: f64 = 34.0;
const FOOTER_H: f64 = 78.0;
const CONTENT_MAX_W: f64 = 900.0;
/// Height of the Source step's "no qOS releases" card. Shared by the draw and
/// layout passes so the two stay in step.
const RELEASE_ZERO_H: f64 = 84.0;

/// A clickable hit-target for one frame. Pure function of the viewport +
/// state, so `draw` and `handle_event` agree without ordering hazards. The
/// Customize step's own affordances live inside [`CustomizePanel`], not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    PickFile,
    Release(usize),
    Recent(usize),
    Target(usize),
    Rescan,
    ConfirmField,
    StepJump(Step),
    Back,
    Next,
    StartWrite,
    WriteAnother,
}

struct Layout {
    crumbs: Vec<(Rect, Step)>,
    /// At compact widths the six-step rail shows numbered/checkmarked discs
    /// without labels. The page heading still names the current station, and
    /// this keeps every station visible instead of clipping the first/last
    /// controls outside the window.
    compact_crumbs: bool,
    back: Option<Rect>,
    primary: Option<Rect>,
    hits: Vec<(Rect, Hit)>,
    viewport_content: Rect,
    content: Rect,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "the bools split across unrelated concerns (internal-disk visibility, the system-disk destructive-write ack, and seed/repaint bookkeeping), so grouping them would need a real design pass rather than a mechanical lint fix"
)]
pub struct ImagerApp {
    // ---- CRDT-backed (cached each `sync`) ----
    source: Option<Source>,
    recent: Vec<Source>,
    targets: Vec<TargetDrive>,
    /// qOS releases offered on the Source step. Like the drive list this is
    /// read from the document and never invented: an offered release is a
    /// write source for a job that erases the target, so one that does not
    /// exist on this machine must not be clickable.
    releases: Vec<QosRelease>,
    selected: Option<usize>,
    action: Action,
    profile: InstallProfile,
    progress: Progress,
    /// Peer-local destructive-write arming field. This must never replicate:
    /// a collaborator may review the plan, but cannot type consent for us.
    confirm_input: TextInput,
    status: String,
    step: Step,
    // ---- transient job / portal plumbing (not in the doc) ----
    job_rx: Option<std::sync::mpsc::Receiver<Progress>>,
    portal_rx: Option<PortalChannel>,
    release_rx: Option<std::sync::mpsc::Receiver<Result<Source, String>>>,
    export_dialog: ExportDialog,
    /// Stages the read-only export modal after the demo document has synced.
    /// Used only by the deterministic `export` screenshot scenario.
    export_after_sync: bool,
    /// Makes the destructive action look armed in an offscreen documentation
    /// frame without enabling (or bypassing) the real write backend.
    shot_confirmation_ready: bool,
    // ---- render caches ----
    seeded: bool,
    rect: Rect,
    dirty: bool,
    last_mouse: Point,
    hover_target: Option<usize>,
    /// Vertical offset for the step body. The step rail and footer stay put.
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
    /// Framework-owned action dispatch (the only action-dispatch state the app
    /// holds). Every guided-customization profile edit dispatches its
    /// [`ImagerAction`](crate::actions::ImagerAction) through this host, which
    /// brackets the undo boundary and applies it through the executor so it
    /// records one Ctrl+Z step and replays byte-identically on peers.
    actions: ui::ActionHost,
}

impl Default for ImagerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ImagerApp {
    pub fn new() -> Self {
        Self {
            source: None,
            recent: Vec::new(),
            targets: Vec::new(),
            releases: Vec::new(),
            selected: None,
            action: Action::Flash,
            profile: InstallProfile::default(),
            progress: Progress::default(),
            confirm_input: TextInput::local()
                .with_id("imager_device_confirmation")
                .with_font_size(14.0)
                .with_plain_chrome()
                .with_enter_submits(true),
            status: String::new(),
            step: Step::Source,
            job_rx: None,
            portal_rx: None,
            release_rx: None,
            export_dialog: ExportDialog::new(),
            export_after_sync: false,
            shot_confirmation_ready: false,
            seeded: false,
            rect: Rect::ZERO,
            dirty: true,
            last_mouse: Point::ZERO,
            hover_target: None,
            scroll_y: 0.0,
            keyboard_hit: None,
            customize: CustomizePanel::new(),
            actions: ui::ActionHost::new(),
        }
    }

    fn target(&self) -> Option<&TargetDrive> {
        self.selected.and_then(|i| self.targets.get(i))
    }

    /// The releases the document actually knows about — empty on the live
    /// path, seeded content under `--demo` / `--shots`.
    fn releases(&self) -> &[QosRelease] {
        &self.releases
    }

    /// Only external installation media is shown. Internal installation and
    /// dual boot belong to the installer on the booted qOS medium.
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
            Step::Confirm => self.confirm_matches() && self.image_fits() && writer_available,
            // Write is in-progress and Done is terminal — neither advances.
            Step::Write | Step::Done => false,
        }
    }

    /// Whether `s` is a completed station (drawn with a green check). A step is
    /// completed once the flow has moved past it — except a Customize station
    /// that was *skipped* (a plain ISO carries no profile), which reads as
    /// not-engaged rather than done.
    fn step_completed(&self, s: Step) -> bool {
        s.index() < self.step.index() && (s != Step::Customize || self.customizable())
    }

    /// Whether a step can be jumped to by clicking its indicator station. The
    /// execution stations (Write, Done) are never jump targets.
    fn crumb_reachable(&self, s: Step) -> bool {
        match s {
            Step::Source => true,
            Step::Drive => self.source.is_some(),
            Step::Customize => self.selected.is_some() && self.customizable(),
            Step::Confirm => self.selected.is_some(),
            Step::Write | Step::Done => false,
        }
    }

    /// Only qOS images can be pre-configured; a plain ISO is written as-is.
    fn customizable(&self) -> bool {
        self.source.as_ref().is_some_and(|s| s.installable)
    }

    /// The label for the customization resource Ctrl+K publishes, or `None` when
    /// the current source has no profile to configure (a plain ISO, or nothing
    /// selected) so the palette shows nothing to act on. Used by the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    pub(crate) fn customization_source_label(&self) -> Option<String> {
        if !self.customizable() {
            return None;
        }
        Some(
            self.source
                .as_ref()
                .map_or_else(|| "qOS image".into(), |s| s.name.clone()),
        )
    }

    /// The last profile [`EncodedAction`](qos_actions::EncodedAction) an
    /// on-screen control dispatched, for the affordance == `ActionSpec` covenant
    /// test.
    #[doc(hidden)]
    pub fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
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
                "Choose the source image",
                if self.releases().is_empty() {
                    "Pick a local ISO or IMG file to write.".into()
                } else {
                    "Pick a local ISO or IMG file, or choose a qOS release.".into()
                },
            ),
            Step::Drive => (
                "Select the target drive",
                "Only external installation media is available here. The selected drive will be erased."
                    .into(),
            ),
            Step::Customize => (
                "Prepare your setup",
                if self.customizable() {
                    "These non-secret choices travel with the installer and shorten setup on the device."
                        .into()
                } else {
                    "This image will be written unchanged.".into()
                },
            ),
            Step::Confirm => (
                if self.action == Action::Install {
                    "Confirm and install"
                } else {
                    "Confirm and write"
                },
                "Review the destructive operation, then type the drive name to unlock it."
                    .into(),
            ),
            Step::Write => (
                if self.progress.phase == JobPhase::Verifying {
                    "Verifying the drive"
                } else {
                    "Writing the drive"
                },
                "Keep the drive connected until writing and verification finish.".into(),
            ),
            Step::Done => {
                let success = self.progress.phase == JobPhase::Success;
                (
                    if success {
                        "Drive is ready"
                    } else {
                        "Write failed"
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
            Hit::Recent(index) => format!("imager:hit:recent:{index}"),
            Hit::Target(index) => format!("imager:hit:target:{index}"),
            Hit::Rescan => "imager:hit:rescan".into(),
            Hit::ConfirmField => "imager:hit:confirm-field".into(),
            Hit::StepJump(step) => format!("imager:hit:step:{}", step.key()),
            Hit::Back => "imager:hit:back".into(),
            Hit::Next => "imager:hit:next".into(),
            Hit::StartWrite => "imager:hit:start-write".into(),
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
            "write-another" => Some(Hit::WriteAnother),
            _ => {
                let (kind, value) = suffix.split_once(':')?;
                match kind {
                    "step" => match value {
                        "source" => Some(Hit::StepJump(Step::Source)),
                        "drive" => Some(Hit::StepJump(Step::Drive)),
                        "customize" => Some(Hit::StepJump(Step::Customize)),
                        "confirm" => Some(Hit::StepJump(Step::Confirm)),
                        "write" => Some(Hit::StepJump(Step::Write)),
                        "done" => Some(Hit::StepJump(Step::Done)),
                        _ => None,
                    },
                    _ => None,
                }
            }
        }
    }

    fn a11y_hit_enabled(&self, hit: Hit) -> bool {
        match hit {
            Hit::StepJump(step) => self.crumb_reachable(step),
            Hit::Next => match self.step {
                Step::Source | Step::Drive | Step::Customize => self.can_advance(self.step),
                Step::Confirm | Step::Write | Step::Done => false,
            },
            Hit::StartWrite => self.can_advance(Step::Confirm),
            Hit::Release(index) => self.releases.get(index).is_some(),
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
        if let Some(rect) = CustomizePanel::keyboard_slot_rect(lay.content, slot) {
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

    fn a11y_step_navigation(&self, lay: &Layout) -> ui::a11y::AccessNode {
        let children = lay
            .crumbs
            .iter()
            .map(|(rect, step)| {
                let reachable = self.crumb_reachable(*step);
                self.a11y_control(
                    ui::a11y::Role::Button,
                    format!("Step {} of 6: {}", step.index() + 1, step.title()),
                    *rect,
                    Hit::StepJump(*step),
                    !reachable,
                )
                .with_state(ui::a11y::AccessState {
                    focused: self.keyboard_hit == Some(Hit::StepJump(*step)),
                    focusable: reachable,
                    selected: *step == self.step,
                    disabled: !reachable,
                    ..Default::default()
                })
                .with_position(step.index() + 1, Step::all().len())
            })
            .collect();
        ui::a11y::AccessNode::new(ui::a11y::Role::Group)
            .with_name("Setup steps")
            .with_children(children)
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
                        format!("Download qOS {}", release.version),
                        *rect,
                        *hit,
                        false,
                    )
                    .with_description(format!("{} image", fmt_size(release.size)))
                    .with_action_target(format!("imager:release:{}", release.version))
                }),
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
                Step::Write => return controls,
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
        let mut children = vec![
            self.a11y_step_navigation(&lay)
                .with_hashed_key("step-navigation"),
            AccessNode::new(Role::Heading { level: 1 })
                .with_name(title)
                .with_hashed_key("step-heading")
                .with_bounds(Rect::new(
                    lay.content.x0,
                    lay.content.y0,
                    lay.content.x1,
                    lay.content.y0 + 24.0,
                )),
            AccessNode::new(Role::Paragraph)
                .with_value(description)
                .with_hashed_key("step-description")
                .with_bounds(Rect::new(
                    lay.content.x0,
                    lay.content.y0 + 24.0,
                    lay.content.x1,
                    lay.content.y0 + 52.0,
                )),
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
                            .with_value(format!("{}, {}", source.name, fmt_size(source.size)))
                            .with_hashed_key("selected-image"),
                    );
                }
                children.push(
                    AccessNode::new(Role::Group)
                        .with_name("Image sources")
                        .with_hashed_key("source-controls")
                        .with_children(self.a11y_source_controls(&lay)),
                );
            }
            Step::Drive => children.push(
                AccessNode::new(Role::Group)
                    .with_name("External destination drives")
                    .with_hashed_key("drive-controls")
                    .with_children(self.a11y_drive_controls(&lay)),
            ),
            Step::Customize if self.customizable() => children.push(
                AccessNode::new(Role::Group)
                    .with_name("Setup profile")
                    .with_hashed_key("profile-controls")
                    .with_children(self.customize.accessibility(lay.content)),
            ),
            Step::Customize => children.push(
                AccessNode::new(Role::StaticText)
                    .with_value("Pre-configuration is only available for qOS images.")
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
            children.push(
                AccessNode::new(Role::Group)
                    .with_name("Wizard actions")
                    .with_hashed_key("wizard-actions")
                    .with_children(footer),
            );
        }
        AccessNode::new(Role::Window)
            .with_name("Start Media Creator")
            .with_children(children)
    }
}

/// Whether this process should enumerate real block devices. The live binary
/// sets `IMAGER_LIVE=1`; `--shots`, `--demo` and tests leave it unset so the
/// document's seeded/posed state is authoritative and no `lsblk` runs.
fn live_probe_enabled() -> bool {
    std::env::var("IMAGER_LIVE").is_ok()
}

// ---- theme-derived colour helpers --------------------------------------
fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

fn source_color(theme: &Theme, kind: SourceKind) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        SourceKind::QosImage => c.accent[5],
        SourceKind::IsoFile => c.semantic.info,
    }
}

fn kind_color(theme: &Theme, kind: TargetKind) -> Color {
    let c = &theme.tokens.colors;
    match kind {
        TargetKind::Usb => c.semantic.success,
        TargetKind::Nvme => c.accent[5],
        TargetKind::Ssd => c.semantic.info,
        TargetKind::Hdd => c.neutral[5],
        TargetKind::SdCard => c.accent[7],
        TargetKind::Other => c.neutral[4],
    }
}

// =========================================================================
// Drawing primitives
// =========================================================================
impl ImagerApp {
    #[allow(clippy::too_many_arguments)]
    fn txt(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        size: f32,
        color: Color,
        x: f64,
        top: f64,
        max_w: Option<f32>,
        weight: TextWeight,
    ) {
        tr.draw_text_weighted(
            scene,
            s,
            size,
            color,
            Affine::translate((x, top)),
            max_w,
            weight,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn txt_right(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        size: f32,
        color: Color,
        right: f64,
        top: f64,
        weight: TextWeight,
    ) {
        let (w, _) = tr.measure_weighted(s, size, None, weight);
        Self::txt(
            scene,
            tr,
            s,
            size,
            color,
            right - f64::from(w),
            top,
            None,
            weight,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn txt_center(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        s: &str,
        size: f32,
        color: Color,
        cx: f64,
        top: f64,
        weight: TextWeight,
    ) {
        let (w, _) = tr.measure_weighted(s, size, None, weight);
        Self::txt(
            scene,
            tr,
            s,
            size,
            color,
            cx - f64::from(w) / 2.0,
            top,
            None,
            weight,
        );
    }

    /// Wrap a terminal failure into a small, measured block. Error strings can
    /// contain long device paths or platform diagnostics, so character-count
    /// truncation is not enough: every returned line is measured in the same
    /// font used to paint it and is guaranteed to fit `max_width`.
    fn failure_detail_lines(
        tr: &mut TextRenderer,
        detail: &str,
        max_width: f32,
        max_lines: usize,
    ) -> Vec<String> {
        const SIZE: f32 = 13.5;
        const WEIGHT: TextWeight = TextWeight::Regular;
        let mut remaining = detail.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut lines = Vec::new();
        while !remaining.is_empty() && lines.len() < max_lines {
            if tr.measure_weighted(&remaining, SIZE, None, WEIGHT).0 <= max_width {
                lines.push(remaining);
                break;
            }

            let last_line = lines.len() + 1 == max_lines;
            let ellipsis_width = if last_line {
                tr.measure_weighted("…", SIZE, None, WEIGHT).0
            } else {
                0.0
            };
            let allowed = (max_width - ellipsis_width).max(1.0);
            let mut fitting_end = 0;
            let mut whitespace_end = None;
            for (index, character) in remaining.char_indices() {
                let end = index + character.len_utf8();
                let Some(candidate) = remaining.get(..end) else {
                    break;
                };
                if tr.measure_weighted(candidate, SIZE, None, WEIGHT).0 > allowed {
                    break;
                }
                fitting_end = end;
                if character.is_whitespace() {
                    whitespace_end = Some(index);
                }
            }
            if fitting_end == 0 {
                // Even one glyph does not fit; returning no line is safer than
                // painting outside the card at extreme accessibility scales.
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

    fn fill_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color) {
        scene.fill(
            Fill::NonZero,
            t,
            c,
            None,
            &RoundedRect::from_rect(r, radius),
        );
    }
    fn stroke_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color, w: f64) {
        scene.stroke(
            &Stroke::new(w),
            t,
            c,
            None,
            &RoundedRect::from_rect(r, radius),
        );
    }
    fn dashed_rr(scene: &mut Scene, t: Affine, r: Rect, radius: f64, c: Color, w: f64) {
        let stroke = Stroke::new(w).with_dashes(0.0, [7.0, 6.0]);
        scene.stroke(&stroke, t, c, None, &RoundedRect::from_rect(r, radius));
    }
    fn bar(scene: &mut Scene, t: Affine, r: Rect, frac: f64, track: Color, fill: Color) {
        let radius = (r.height() / 2.0).min(6.0);
        Self::fill_rr(scene, t, r, radius, track);
        let frac = frac.clamp(0.0, 1.0);
        if frac > 0.0 {
            let w = (r.width() * frac).max(radius * 2.0).min(r.width());
            let fr = Rect::new(r.x0, r.y0, r.x0 + w, r.y1);
            Self::fill_rr(scene, t, fr, radius, fill);
        }
    }

    /// A small check-mark glyph inside a filled disc of radius `rad` at `c`.
    fn check_disc(scene: &mut Scene, t: Affine, center: Point, rad: f64, disc: Color, tick: Color) {
        scene.fill(Fill::NonZero, t, disc, None, &Circle::new(center, rad));
        let mut p = BezPath::new();
        p.move_to((center.x - rad * 0.42, center.y + rad * 0.02));
        p.line_to((center.x - rad * 0.10, center.y + rad * 0.34));
        p.line_to((center.x + rad * 0.46, center.y - rad * 0.34));
        scene.stroke(&Stroke::new(rad * 0.24), t, tick, None, &p);
    }

    fn section_label(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        s: &str,
        x: f64,
        y: f64,
    ) {
        Self::txt(
            scene,
            tr,
            s,
            11.0,
            theme.tokens.colors.text_dim,
            x,
            y,
            None,
            TextWeight::Semibold,
        );
    }
}

// =========================================================================
// Layout
// =========================================================================
impl ImagerApp {
    fn compact_content(content: Rect) -> bool {
        content.width() < 520.0
    }

    fn content_viewport_rect(&self) -> Rect {
        let v = self.rect;
        let top = CRUMB_TOP + CRUMB_H + 30.0;
        let bottom = v.y1 - FOOTER_H - 10.0;
        let margin = if v.width() < 600.0 { 16.0 } else { OUTER };
        let avail_w = v.width() - margin * 2.0;
        let w = avail_w.min(CONTENT_MAX_W);
        let x0 = v.x0 + (v.width() - w) / 2.0;
        Rect::new(x0, v.y0 + top, x0 + w, v.y0 + bottom)
    }

    fn content_extent(&self, viewport: Rect) -> f64 {
        let compact = Self::compact_content(viewport);
        let requested = match self.step {
            Step::Source => 650.0,
            Step::Drive => {
                if self.visible_targets().is_empty() {
                    if compact { 350.0 } else { 320.0 }
                } else {
                    (if compact { 94.0 } else { 52.0 })
                        + self.visible_targets().len() as f64 * 124.0
                }
            }
            Step::Customize => 462.0,
            Step::Confirm => {
                if compact {
                    440.0
                } else {
                    360.0
                }
            }
            Step::Write => 410.0,
            Step::Done => 400.0,
        };
        viewport.height().max(requested)
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
        let v = self.rect;
        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        let content = self.content_rect();

        // ---- Step-indicator pills ----
        let steps = Step::all();
        let full_crumb_w = 132.0f64;
        let full_gap = 14.0f64;
        let full_total = full_crumb_w * steps.len() as f64 + full_gap * (steps.len() as f64 - 1.0);
        let compact_crumbs = v.width() - OUTER * 2.0 < full_total;
        let rail_margin = if compact_crumbs { 16.0 } else { OUTER };
        let rail_w = (v.width() - rail_margin * 2.0).max(0.0);
        let (crumb_w, gap, total) = if compact_crumbs {
            // Six controls remain inside the content margins down to the
            // supported 320 px compact viewport. Prefer useful hit targets to
            // labels which cannot fit; the screen heading supplies the name.
            let gap = if rail_w >= 280.0 { 8.0 } else { 4.0 };
            let gaps = gap * (steps.len() as f64 - 1.0);
            let crumb_w = ((rail_w - gaps) / steps.len() as f64).max(24.0);
            (crumb_w, gap, crumb_w * steps.len() as f64 + gaps)
        } else {
            (full_crumb_w, full_gap, full_total)
        };
        let mut cx = v.x0 + (v.width() - total) / 2.0;
        let cy = v.y0 + CRUMB_TOP;
        let mut crumbs = Vec::new();
        for s in steps {
            let r = Rect::new(cx, cy, cx + crumb_w, cy + CRUMB_H);
            crumbs.push((r, s));
            if self.crumb_reachable(s) {
                hits.push((r, Hit::StepJump(s)));
            }
            cx += crumb_w + gap;
        }

        // ---- Per-step content hit-targets ----
        match self.step {
            Step::Source => self.layout_source(content, &mut hits),
            Step::Drive => self.layout_drive(content, &mut hits),
            Step::Confirm => Self::layout_confirm(content, &mut hits),
            // Customize's fields/toggles are owned by `CustomizePanel`, which
            // hit-tests its own rects in `handle_event`; Write (in progress)
            // and Done have no interactive content beyond the footer. No app
            // hits for any of the three.
            Step::Customize | Step::Write | Step::Done => {}
        }

        // ---- Footer (Back + primary) ----
        let fy = v.y1 - FOOTER_H;
        let by = fy + (FOOTER_H - 44.0) / 2.0;
        let (mut back, mut primary) = (None, None);
        let show_back = matches!(self.step, Step::Drive | Step::Customize | Step::Confirm);
        if show_back {
            let width = if Self::compact_content(content) {
                (content.width() - 12.0) / 2.0
            } else {
                120.0
            };
            let r = Rect::new(content.x0, by, content.x0 + width, by + 44.0);
            hits.push((r, Hit::Back));
            back = Some(r);
        }
        // The Write station is a live progress screen with no primary action.
        if let Some(p) = self.primary_button_rect(content, by) {
            let hit = match self.step {
                Step::Confirm => Hit::StartWrite,
                Step::Done => Hit::WriteAnother,
                _ => Hit::Next,
            };
            hits.push((p, hit));
            primary = Some(p);
        }

        Layout {
            crumbs,
            compact_crumbs,
            back,
            primary,
            hits,
            viewport_content: self.content_viewport_rect(),
            content,
        }
    }

    /// The primary action button rect, right-aligned in the footer — or `None`
    /// on the Write station (a live-progress screen with no primary).
    fn primary_button_rect(&self, content: Rect, by: f64) -> Option<Rect> {
        if self.step == Step::Write {
            return None;
        }
        let has_back = matches!(self.step, Step::Drive | Step::Customize | Step::Confirm);
        let w = if has_back && Self::compact_content(content) {
            (content.width() - 12.0) / 2.0
        } else {
            match self.step {
                Step::Confirm => 250.0,
                Step::Done => 190.0,
                _ => 170.0,
            }
        };
        Some(Rect::new(content.x1 - w, by, content.x1, by + 44.0))
    }

    fn layout_source(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        let x0 = content.x0;
        let x1 = content.x1;
        let compact = Self::compact_content(content);
        let mut y = content.y0 + if compact { 64.0 } else { 44.0 };
        // Browse drop-zone.
        let browse = Rect::new(x0, y, x1, y + 96.0);
        hits.push((browse, Hit::PickFile));
        y = browse.y1 + (if self.source.is_some() { 16.0 } else { 28.0 });
        if self.source.is_some() {
            y += if compact { 108.0 } else { 92.0 } + 24.0; // selected card
        }
        // qOS releases list. With none known, the zero-state card takes the
        // space and there is nothing here to click.
        y += 22.0; // section label
        if self.releases().is_empty() {
            y += RELEASE_ZERO_H + 10.0;
        }
        for (i, _) in self.releases().iter().enumerate() {
            let r = Rect::new(x0, y, x1, y + 54.0);
            hits.push((r, Hit::Release(i)));
            y += 54.0 + 10.0;
        }
        // Recent list.
        if !self.recent.is_empty() {
            y += 12.0 + 20.0; // gap + section label, as drawn
            for (i, _) in self.recent.iter().enumerate().take(3) {
                let r = Rect::new(x0, y, x1, y + 44.0);
                if y + 44.0 > content.y1 {
                    break;
                }
                hits.push((r, Hit::Recent(i)));
                y += 44.0 + 8.0;
            }
        }
    }

    fn layout_drive(&self, content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        let x0 = content.x0;
        let x1 = content.x1;
        // Rescan button top-right of the header row.
        let compact = Self::compact_content(content);
        let rescan_y = content.y0 + if compact { 52.0 } else { 0.0 };
        let rescan = Rect::new(x1 - 104.0, rescan_y, x1, rescan_y + 30.0);
        hits.push((rescan, Hit::Rescan));
        let mut y = content.y0 + if compact { 94.0 } else { 52.0 };
        let vis = self.visible_targets();
        if vis.is_empty() {
            // Empty state occupies the body; Rescan remains available above.
        } else {
            for (idx, _) in &vis {
                let r = Rect::new(x0, y, x1, y + 112.0);
                hits.push((r, Hit::Target(*idx)));
                y += 112.0 + 12.0;
                if y + 112.0 > content.y1 {
                    break;
                }
            }
        }
    }

    fn layout_confirm(content: Rect, hits: &mut Vec<(Rect, Hit)>) {
        // The typed device-name confirmation field sits under the review card.
        let field = Rect::new(
            content.x0,
            content.y1 - 96.0,
            content.x1,
            content.y1 - 96.0 + 46.0,
        );
        hits.push((field, Hit::ConfirmField));
    }
}

// =========================================================================
// Header + breadcrumb
// =========================================================================
impl ImagerApp {
    fn draw_scrollbar(&self, scene: &mut Scene, theme: &Theme, t: Affine, lay: &Layout) {
        let max = self.max_scroll();
        if max <= 0.0 {
            return;
        }
        let viewport = lay.viewport_content;
        let track = Rect::new(
            viewport.x1 + 6.0,
            viewport.y0,
            viewport.x1 + 9.0,
            viewport.y1,
        );
        let thumb_h = (viewport.height() * viewport.height() / lay.content.height()).max(32.0);
        let travel = (track.height() - thumb_h).max(0.0);
        let y = track.y0 + travel * (self.scroll_y / max);
        Self::fill_rr(
            scene,
            t,
            Rect::new(track.x0, y, track.x1, y + thumb_h),
            2.0,
            theme.tokens.colors.neutral[5],
        );
    }

    fn draw_header(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let v = self.rect;
        // No in-app app-name header: the compositor owns the window chrome and
        // shows the app's name. The app opens straight into the step indicator,
        // with a slim status line tucked into the top-right margin.
        if !self.status.is_empty() {
            let col = match self.progress.phase {
                JobPhase::Failed => c.semantic.danger,
                JobPhase::Success => c.semantic.success,
                _ => c.text_dim,
            };
            Self::txt_right(
                scene,
                tr,
                &self.status,
                11.5,
                col,
                v.x1 - OUTER,
                v.y0 + 8.0,
                TextWeight::Medium,
            );
        }

        // ---- Step indicator ----
        for (i, (r, s)) in lay.crumbs.iter().enumerate() {
            let done = self.step_completed(*s);
            let current = *s == self.step;
            let hov = r.contains(self.last_mouse) && self.crumb_reachable(*s);
            let bg = if current {
                with_alpha(c.accent[5], 40)
            } else if hov {
                c.neutral[2]
            } else {
                Color::TRANSPARENT
            };
            if bg != Color::TRANSPARENT {
                Self::fill_rr(scene, t, *r, 10.0, bg);
            }
            if current {
                Self::stroke_rr(scene, t, *r, 10.0, c.accent[5], 1.4);
            }
            // Number disc / check. Compact layouts center the disc in its
            // station and omit the label, keeping all six stations on-screen.
            let disc_c = Point::new(
                if lay.compact_crumbs {
                    f64::midpoint(r.x0, r.x1)
                } else {
                    r.x0 + 22.0
                },
                f64::midpoint(r.y0, r.y1),
            );
            if done {
                Self::check_disc(scene, t, disc_c, 11.0, c.semantic.success, Color::WHITE);
            } else {
                let ring = if current { c.accent[5] } else { c.neutral[4] };
                scene.fill(Fill::NonZero, t, ring, None, &Circle::new(disc_c, 11.0));
                Self::txt_center(
                    scene,
                    tr,
                    &format!("{}", s.index() + 1),
                    12.0,
                    Color::WHITE,
                    disc_c.x,
                    disc_c.y - 8.0,
                    TextWeight::Bold,
                );
            }
            if !lay.compact_crumbs {
                let label_c = if current || done {
                    c.text_primary
                } else {
                    c.text_dim
                };
                Self::txt(
                    scene,
                    tr,
                    s.title(),
                    13.5,
                    label_c,
                    r.x0 + 40.0,
                    f64::midpoint(r.y0, r.y1) - 9.0,
                    None,
                    if current {
                        TextWeight::Semibold
                    } else {
                        TextWeight::Medium
                    },
                );
            }
            // Connector to the next pill.
            if let Some(next) = lay.crumbs.get(i + 1) {
                let nx0 = next.0.x0;
                let midy = f64::midpoint(r.y0, r.y1);
                let cc = if s.index() < self.step.index() {
                    c.semantic.success
                } else {
                    c.neutral[3]
                };
                scene.stroke(
                    &Stroke::new(2.0),
                    t,
                    cc,
                    None,
                    &Line::new(Point::new(r.x1 + 4.0, midy), Point::new(nx0 - 4.0, midy)),
                );
            }
        }
    }

    fn draw_footer(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let v = self.rect;
        let fy = v.y1 - FOOTER_H;
        scene.stroke(
            &Stroke::new(1.0),
            t,
            c.border_standard,
            None,
            &Line::new(Point::new(v.x0, fy), Point::new(v.x1, fy)),
        );

        // Back.
        if let Some(r) = lay.back {
            let hov = r.contains(self.last_mouse);
            Self::fill_rr(
                scene,
                t,
                r,
                10.0,
                if hov { c.neutral[3] } else { c.neutral[2] },
            );
            Self::stroke_rr(scene, t, r, 10.0, c.border_standard, 1.0);
            Self::txt_center(
                scene,
                tr,
                "‹  Back",
                14.0,
                c.text_primary,
                f64::midpoint(r.x0, r.x1),
                r.y0 + 12.0,
                TextWeight::Medium,
            );
        }

        // Primary.
        if let Some(r) = lay.primary {
            self.draw_primary(scene, tr, theme, t, r);
        }

        // A hint between them (center) — e.g. what's still needed.
        if !Self::compact_content(lay.content)
            && let Some(hint) = self.footer_hint()
        {
            Self::txt_center(
                scene,
                tr,
                &hint,
                12.0,
                c.text_dim,
                f64::midpoint(v.x0, v.x1),
                fy + FOOTER_H / 2.0 - 8.0,
                TextWeight::Regular,
            );
        }
    }

    fn footer_hint(&self) -> Option<String> {
        match self.step {
            Step::Source if self.source.is_none() => Some("Choose an image to continue".into()),
            Step::Drive if self.selected.is_none() => Some("Select a drive to continue".into()),
            Step::Confirm if !self.confirm_matches() => self
                .target()
                .map(|d| format!("Type “{}” to unlock", d.name)),
            Step::Write => Some("Keep the drive connected until this finishes…".into()),
            _ => None,
        }
    }

    fn draw_primary(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        r: Rect,
    ) {
        let c = &theme.tokens.colors;
        let hov = r.contains(self.last_mouse);
        let (label, enabled, destructive) = match self.step {
            Step::Source => (
                "Continue  ›".to_string(),
                self.can_advance(Step::Source),
                false,
            ),
            Step::Drive => (
                "Continue  ›".to_string(),
                self.can_advance(Step::Drive),
                false,
            ),
            Step::Customize => ("Continue  ›".to_string(), true, false),
            Step::Confirm => {
                let dev = self
                    .target()
                    .map(super::model::TargetDrive::dev_path)
                    .unwrap_or_default();
                // "Erase & write /dev/sdb" for a stick; "Erase & install
                // /dev/nvme0n1" for a disk install — the verb reads plainly.
                let label = if r.width() < 220.0 {
                    format!("Erase & {}", self.action.verb().to_ascii_lowercase())
                } else {
                    format!("Erase & {}  {dev}", self.action.verb().to_ascii_lowercase())
                };
                (
                    label,
                    self.can_advance(Step::Confirm) || self.shot_confirmation_ready,
                    true,
                )
            }
            // The Write station has no primary (returned None above); Done
            // offers a fresh start.
            Step::Write => ("Writing…".to_string(), false, false),
            Step::Done => match self.progress.phase {
                JobPhase::Failed => ("Review & retry".to_string(), true, false),
                _ => ("Start over".to_string(), true, false),
            },
        };
        let bg = if !enabled {
            c.neutral[3]
        } else if destructive {
            if hov {
                c.semantic.danger
            } else {
                with_alpha(c.semantic.danger, 220)
            }
        } else if hov {
            c.accent[6]
        } else {
            c.accent[5]
        };
        Self::fill_rr(scene, t, r, 11.0, bg);
        let fg = if enabled { Color::WHITE } else { c.neutral[6] };
        Self::txt_center(
            scene,
            tr,
            &label,
            14.0,
            fg,
            f64::midpoint(r.x0, r.x1),
            r.y0 + 13.0,
            TextWeight::Bold,
        );
    }
}

// =========================================================================
// Step bodies
// =========================================================================
impl ImagerApp {
    fn draw_step_title(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        title: &str,
        sub: &str,
    ) {
        let c = &theme.tokens.colors;
        Self::txt(
            scene,
            tr,
            title,
            19.0,
            c.text_primary,
            content.x0,
            content.y0,
            None,
            TextWeight::Bold,
        );
        Self::txt(
            scene,
            tr,
            sub,
            13.0,
            c.text_dim,
            content.x0,
            content.y0 + 26.0,
            Some(content.width() as f32),
            TextWeight::Regular,
        );
    }

    fn draw_source_step(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let content = lay.content;
        let sub = if self.releases().is_empty() {
            "The image to write. Pick a local .iso or .img file."
        } else {
            "The image to write. Pick a local .iso or .img file, or a qOS release below."
        };
        Self::draw_step_title(scene, tr, theme, content, "Choose the source image", sub);
        let browse = self.draw_source_dropzone(scene, tr, theme, t, content);
        let y = self.draw_source_selected_card(scene, tr, theme, t, content, browse);
        let y = self.draw_source_releases(scene, tr, theme, t, content, y);
        self.draw_source_recent(scene, tr, theme, t, content, y);
    }

    /// Draws the "Browse for an image…" drop-zone and returns its rect (the
    /// selected-source card below it is anchored off `browse.y1`).
    fn draw_source_dropzone(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let compact = Self::compact_content(content);
        let y = content.y0 + if compact { 64.0 } else { 44.0 };
        let browse = Rect::new(content.x0, y, content.x1, y + 96.0);
        let hov = browse.contains(self.last_mouse);
        Self::fill_rr(
            scene,
            t,
            browse,
            14.0,
            if hov {
                c.neutral[2]
            } else {
                with_alpha(c.accent[5], 16)
            },
        );
        Self::dashed_rr(
            scene,
            t,
            browse,
            14.0,
            if hov {
                c.accent[6]
            } else {
                with_alpha(c.accent[5], 160)
            },
            1.6,
        );
        // Download/arrow glyph.
        let gx = browse.x0 + if compact { 30.0 } else { 46.0 };
        let gy = f64::midpoint(browse.y0, browse.y1);
        let mut arr = BezPath::new();
        arr.move_to((gx, gy - 14.0));
        arr.line_to((gx, gy + 8.0));
        scene.stroke(&Stroke::new(2.4), t, c.accent[6], None, &arr);
        let mut head = BezPath::new();
        head.move_to((gx - 7.0, gy + 1.0));
        head.line_to((gx, gy + 9.0));
        head.line_to((gx + 7.0, gy + 1.0));
        scene.stroke(&Stroke::new(2.4), t, c.accent[6], None, &head);
        scene.stroke(
            &Stroke::new(2.4),
            t,
            c.accent[6],
            None,
            &Line::new(
                Point::new(gx - 12.0, gy + 15.0),
                Point::new(gx + 12.0, gy + 15.0),
            ),
        );
        let text_x = browse.x0 + if compact { 58.0 } else { 84.0 };
        let text_w = (browse.x1 - text_x - 12.0).max(0.0) as f32;
        Self::txt(
            scene,
            tr,
            "Browse for an image…",
            16.0,
            c.text_primary,
            text_x,
            browse.y0 + 30.0,
            Some(text_w),
            TextWeight::Semibold,
        );
        Self::txt(
            scene,
            tr,
            if compact {
                "Choose an .iso, .img, or .raw file"
            } else {
                "Opens the system file picker · .iso, .img, .raw"
            },
            12.5,
            c.text_dim,
            text_x,
            browse.y0 + 54.0,
            Some(text_w),
            TextWeight::Regular,
        );
        browse
    }

    /// Draws the selected-source summary card below the drop-zone (or draws
    /// nothing, if no source is selected yet), returning the y for the next
    /// section.
    #[allow(
        clippy::too_many_lines,
        reason = "the compact and regular source-card variants intentionally share one renderer so their visual state cannot drift"
    )]
    fn draw_source_selected_card(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        browse: Rect,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let y = browse.y1 + 16.0;
        let Some(src) = &self.source else {
            return browse.y1 + 28.0;
        };
        let compact = Self::compact_content(content);
        let card_h = if compact { 108.0 } else { 92.0 };
        let card = Rect::new(content.x0, y, content.x1, y + card_h);
        Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
        Self::stroke_rr(
            scene,
            t,
            card,
            14.0,
            with_alpha(c.semantic.success, 150),
            1.4,
        );
        Self::check_disc(
            scene,
            t,
            Point::new(
                card.x0 + if compact { 22.0 } else { 28.0 },
                if compact {
                    card.y0 + 29.0
                } else {
                    f64::midpoint(card.y0, card.y1)
                },
            ),
            13.0,
            c.semantic.success,
            Color::WHITE,
        );
        let badge = Rect::new(
            card.x0 + if compact { 42.0 } else { 52.0 },
            card.y0 + 18.0,
            card.x0 + if compact { 88.0 } else { 98.0 },
            card.y0 + 18.0 + 22.0,
        );
        Self::fill_rr(scene, t, badge, 6.0, source_color(theme, src.kind));
        Self::txt_center(
            scene,
            tr,
            src.kind.tag(),
            12.0,
            Color::WHITE,
            f64::midpoint(badge.x0, badge.x1),
            badge.y0 + 4.0,
            TextWeight::Bold,
        );
        let detail_x = card.x0 + if compact { 42.0 } else { 108.0 };
        let title_x = card.x0 + if compact { 98.0 } else { 108.0 };
        Self::txt(
            scene,
            tr,
            &src.name,
            15.5,
            c.text_primary,
            title_x,
            card.y0 + 18.0,
            Some((card.x1 - title_x - 16.0).max(0.0) as f32),
            TextWeight::Semibold,
        );
        Self::txt(
            scene,
            tr,
            &if compact {
                format!("{}  ·  {}", src.label, fmt_size(src.size))
            } else {
                src.summary()
            },
            12.0,
            c.text_dim,
            detail_x,
            card.y0 + if compact { 52.0 } else { 42.0 },
            Some((card.x1 - detail_x - 16.0).max(0.0) as f32),
            TextWeight::Regular,
        );
        if src.installable {
            Self::txt(
                scene,
                tr,
                "● Can be pre-configured",
                11.5,
                c.semantic.success,
                detail_x,
                card.y0 + if compact { 78.0 } else { 62.0 },
                Some((card.x1 - detail_x - 16.0).max(0.0) as f32),
                TextWeight::Medium,
            );
        } else {
            Self::txt(
                scene,
                tr,
                "Written as-is",
                11.5,
                c.text_dim,
                detail_x,
                card.y0 + if compact { 78.0 } else { 62.0 },
                Some((card.x1 - detail_x - 16.0).max(0.0) as f32),
                TextWeight::Medium,
            );
        }
        card.y1 + 24.0
    }

    /// Draws the "qOS release" heading, its zero-state, and the list of known
    /// releases, returning the y for the next section.
    fn draw_source_releases(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        mut y: f64,
    ) -> f64 {
        // qOS releases. With none to choose between, the heading names the
        // section rather than offering a choice that is not there.
        let heading = if self.releases().is_empty() {
            "qOS RELEASES"
        } else {
            "OR CHOOSE A qOS RELEASE"
        };
        Self::section_label(scene, tr, theme, heading, content.x0, y);
        y += 22.0;
        if self.releases().is_empty() {
            // Nothing is known to be on this machine, so nothing is offered.
            // The honest zero-state says so rather than leaving a bare heading.
            y = Self::draw_no_releases_card(scene, tr, theme, t, content, y);
        }
        for rel in self.releases() {
            y = self.draw_release_row(scene, tr, theme, t, content, y, rel);
        }
        y
    }

    /// The empty-state card shown in place of the release list when nothing
    /// is known to be on this machine. Returns the `y` to continue drawing
    /// from below the card.
    fn draw_no_releases_card(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        y: f64,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let card = Rect::new(content.x0, y, content.x1, y + RELEASE_ZERO_H);
        Self::fill_rr(scene, t, card, 12.0, c.surfaces.raised);
        Self::dashed_rr(scene, t, card, 12.0, c.border_standard, 1.2);
        let cx = f64::midpoint(card.x0, card.x1);
        Self::txt_center(
            scene,
            tr,
            "No qOS releases found",
            14.0,
            c.text_primary,
            cx,
            card.y0 + 24.0,
            TextWeight::Semibold,
        );
        Self::txt_center(
            scene,
            tr,
            "Releases stored on this machine appear here.",
            12.0,
            c.text_dim,
            cx,
            card.y0 + 47.0,
            TextWeight::Regular,
        );
        card.y1 + 10.0
    }

    /// One release row: selection badge, name, channel/size/location, and the
    /// "installable" tag. Returns the `y` to continue drawing from below it.
    #[allow(clippy::too_many_arguments)]
    fn draw_release_row(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        y: f64,
        rel: &QosRelease,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let r = Rect::new(content.x0, y, content.x1, y + 54.0);
        let selected = self.source.as_ref().is_some_and(|s| {
            s.kind == SourceKind::QosImage && s.name == format!("{} {}", rel.name, rel.version)
        });
        let hov = r.contains(self.last_mouse);
        Self::fill_rr(
            scene,
            t,
            r,
            11.0,
            if selected {
                with_alpha(c.accent[5], 34)
            } else if hov {
                c.neutral[2]
            } else {
                c.surfaces.raised
            },
        );
        Self::stroke_rr(
            scene,
            t,
            r,
            11.0,
            if selected {
                c.accent[5]
            } else {
                c.border_standard
            },
            if selected { 1.4 } else { 1.0 },
        );
        let badge = Rect::new(
            r.x0 + 14.0,
            r.y0 + 15.0,
            r.x0 + 14.0 + 42.0,
            r.y0 + 15.0 + 22.0,
        );
        Self::fill_rr(scene, t, badge, 6.0, c.accent[5]);
        Self::txt_center(
            scene,
            tr,
            "qOS",
            11.0,
            Color::WHITE,
            f64::midpoint(badge.x0, badge.x1),
            badge.y0 + 5.0,
            TextWeight::Bold,
        );
        Self::txt(
            scene,
            tr,
            &format!("qOS {}", rel.version),
            14.5,
            c.text_primary,
            r.x0 + 68.0,
            r.y0 + 9.0,
            Some((r.width() - 160.0).max(72.0) as f32),
            TextWeight::Semibold,
        );
        let where_ = if rel.local_path.is_empty() {
            "download"
        } else {
            "on disk"
        };
        Self::txt(
            scene,
            tr,
            &format!("{}  ·  {}  ·  {}", rel.channel, fmt_size(rel.size), where_),
            11.5,
            c.text_dim,
            r.x0 + 68.0,
            r.y0 + 30.0,
            Some((r.width() - 84.0).max(0.0) as f32),
            TextWeight::Regular,
        );
        Self::txt_right(
            scene,
            tr,
            "installable",
            11.0,
            c.semantic.success,
            r.x1 - 16.0,
            r.y0 + 9.0,
            TextWeight::Medium,
        );
        y + 54.0 + 10.0
    }

    /// Draws the "Recent" section listing up to the three most recent sources.
    fn draw_source_recent(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        mut y: f64,
    ) {
        let c = &theme.tokens.colors;
        if self.recent.is_empty() {
            return;
        }
        y += 12.0;
        Self::section_label(scene, tr, theme, "RECENT", content.x0, y);
        y += 20.0;
        for src in self.recent.iter().take(3) {
            if y + 44.0 > content.y1 {
                break;
            }
            let r = Rect::new(content.x0, y, content.x1, y + 44.0);
            let hov = r.contains(self.last_mouse);
            if hov {
                Self::fill_rr(scene, t, r, 9.0, c.neutral[2]);
            }
            let sw = Rect::new(r.x0 + 6.0, r.y0 + 10.0, r.x0 + 11.0, r.y1 - 10.0);
            Self::fill_rr(scene, t, sw, 2.0, source_color(theme, src.kind));
            Self::txt(
                scene,
                tr,
                &src.name,
                13.0,
                c.text_primary,
                r.x0 + 22.0,
                r.y0 + 6.0,
                Some((r.width() - 120.0) as f32),
                TextWeight::Medium,
            );
            Self::txt(
                scene,
                tr,
                &fmt_size(src.size),
                11.0,
                c.text_dim,
                r.x0 + 22.0,
                r.y0 + 24.0,
                None,
                TextWeight::Regular,
            );
            Self::txt_right(
                scene,
                tr,
                src.kind.tag(),
                10.5,
                c.neutral[6],
                r.x1 - 12.0,
                r.y0 + 15.0,
                TextWeight::Semibold,
            );
            y += 44.0 + 8.0;
        }
    }

    fn draw_drive_step(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        Self::draw_step_title(
            scene,
            tr,
            theme,
            content,
            "Select the target drive",
            "The drive to write to — everything on it will be erased.",
        );
        let compact = Self::compact_content(content);
        // Detected count (to the left of the Rescan button).
        let detected = format!("{} removable", self.removable_count());
        if compact {
            Self::txt(
                scene,
                tr,
                &detected,
                11.5,
                c.text_dim,
                content.x0,
                content.y0 + 60.0,
                None,
                TextWeight::Medium,
            );
        } else {
            Self::txt_right(
                scene,
                tr,
                &detected,
                11.5,
                c.text_dim,
                content.x1 - 116.0,
                content.y0 + 8.0,
                TextWeight::Medium,
            );
        }
        // Rescan button (top-right).
        let rescan_y = content.y0 + if compact { 52.0 } else { 0.0 };
        let rescan = Rect::new(content.x1 - 104.0, rescan_y, content.x1, rescan_y + 30.0);
        let hov = rescan.contains(self.last_mouse);
        Self::fill_rr(
            scene,
            t,
            rescan,
            9.0,
            if hov { c.neutral[3] } else { c.neutral[2] },
        );
        Self::stroke_rr(scene, t, rescan, 9.0, c.border_standard, 1.0);
        Self::txt_center(
            scene,
            tr,
            "⟳ Rescan",
            12.5,
            c.text_primary,
            f64::midpoint(rescan.x0, rescan.x1),
            rescan.y0 + 7.0,
            TextWeight::Medium,
        );

        let vis = self.visible_targets();
        let mut y = content.y0 + if compact { 94.0 } else { 52.0 };
        if vis.is_empty() {
            Self::draw_drive_empty(scene, tr, theme, t, content, compact);
        } else {
            for (idx, drive) in &vis {
                if y + 112.0 > content.y1 {
                    break;
                }
                let r = Rect::new(content.x0, y, content.x1, y + 112.0);
                self.draw_drive_card(scene, tr, theme, t, r, *idx, drive);
                y += 112.0 + 12.0;
            }
        }
    }

    fn draw_drive_empty(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        compact: bool,
    ) {
        let c = &theme.tokens.colors;
        let card = Rect::new(
            content.x0,
            content.y0 + if compact { 94.0 } else { 60.0 },
            content.x1,
            content.y0 + if compact { 334.0 } else { 300.0 },
        );
        Self::fill_rr(scene, t, card, 16.0, c.surfaces.raised);
        Self::dashed_rr(scene, t, card, 16.0, c.border_standard, 1.4);
        let cx = f64::midpoint(card.x0, card.x1);
        // A simple USB-stick glyph.
        let gy = card.y0 + 66.0;
        let body = Rect::new(cx - 16.0, gy, cx + 16.0, gy + 54.0);
        Self::fill_rr(scene, t, body, 6.0, c.neutral[4]);
        let cap = Rect::new(cx - 10.0, gy - 16.0, cx + 10.0, gy);
        Self::fill_rr(scene, t, cap, 3.0, c.neutral[5]);
        Self::txt_center(
            scene,
            tr,
            "Insert a drive",
            17.0,
            c.text_primary,
            cx,
            card.y0 + 140.0,
            TextWeight::Semibold,
        );
        if compact {
            for (line, y) in [
                ("Plug in a USB stick or SD card,", card.y0 + 166.0),
                ("then press Rescan.", card.y0 + 184.0),
                ("Internal disks are shown after boot.", card.y0 + 212.0),
            ] {
                Self::txt_center(
                    scene,
                    tr,
                    line,
                    if y < card.y0 + 200.0 { 12.5 } else { 12.0 },
                    if y < card.y0 + 200.0 {
                        c.text_dim
                    } else {
                        c.neutral[6]
                    },
                    cx,
                    y,
                    TextWeight::Regular,
                );
            }
        } else {
            Self::txt_center(
                scene,
                tr,
                "Plug in a USB stick or SD card, then press Rescan.",
                13.0,
                c.text_dim,
                cx,
                card.y0 + 168.0,
                TextWeight::Regular,
            );
            Self::txt_center(
                scene,
                tr,
                "Internal disks are available after booting this installation medium.",
                12.0,
                c.neutral[6],
                cx,
                card.y0 + 196.0,
                TextWeight::Regular,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_drive_card(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        r: Rect,
        idx: usize,
        drive: &TargetDrive,
    ) {
        let c = &theme.tokens.colors;
        let selected = self.selected == Some(idx);
        let hover = self.hover_target == Some(idx) && !selected;
        let danger = !drive.removable;
        let bg = if selected {
            with_alpha(c.accent[5], 30)
        } else if hover {
            c.neutral[2]
        } else {
            c.surfaces.raised
        };
        Self::fill_rr(scene, t, r, 14.0, bg);
        let border = if selected {
            c.accent[5]
        } else if danger {
            with_alpha(c.semantic.danger, 150)
        } else {
            c.border_standard
        };
        Self::stroke_rr(scene, t, r, 14.0, border, if selected { 2.0 } else { 1.0 });

        // Selection check on the left.
        if selected {
            Self::check_disc(
                scene,
                t,
                Point::new(r.x0 + 28.0, f64::midpoint(r.y0, r.y1)),
                13.0,
                c.accent[5],
                Color::WHITE,
            );
        }
        let left = if selected { r.x0 + 54.0 } else { r.x0 + 18.0 };

        // Kind badge.
        let badge = Rect::new(left, r.y0 + 18.0, left + 54.0, r.y0 + 18.0 + 24.0);
        Self::fill_rr(scene, t, badge, 6.0, kind_color(theme, drive.kind));
        Self::txt_center(
            scene,
            tr,
            drive.kind.tag(),
            12.0,
            Color::WHITE,
            f64::midpoint(badge.x0, badge.x1),
            badge.y0 + 5.0,
            TextWeight::Bold,
        );

        Self::draw_drive_card_details(scene, tr, theme, t, r, drive, left);
    }

    /// Draws the removable/fixed/system chip, the model + capacity line, and
    /// the destructive-erase (or locked) note for a drive card.
    #[allow(
        clippy::too_many_arguments,
        reason = "shares the (scene, tr, theme, t) render context plus per-card geometry with every draw_* helper in this file; bundling that context is a wider render-context refactor out of scope here"
    )]
    fn draw_drive_card_details(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        r: Rect,
        drive: &TargetDrive,
        left: f64,
    ) {
        let c = &theme.tokens.colors;
        // Removable / fixed / system chip on the right.
        let (chip_label, chip_c) = if drive.is_system {
            ("SYSTEM DISK", c.semantic.danger)
        } else if drive.removable {
            ("REMOVABLE", c.semantic.success)
        } else {
            ("INTERNAL DISK", c.semantic.warning)
        };
        let (cw, _) = tr.measure_weighted(chip_label, 10.5, None, TextWeight::Bold);
        let chip = Rect::new(
            r.x1 - 16.0 - f64::from(cw) - 18.0,
            r.y0 + 18.0,
            r.x1 - 16.0,
            r.y0 + 18.0 + 24.0,
        );
        Self::fill_rr(scene, t, chip, 6.0, with_alpha(chip_c, 40));
        Self::txt_center(
            scene,
            tr,
            chip_label,
            10.5,
            chip_c,
            f64::midpoint(chip.x0, chip.x1),
            chip.y0 + 6.0,
            TextWeight::Bold,
        );

        // Model + capacity.
        Self::txt(
            scene,
            tr,
            &drive.model,
            16.0,
            c.text_primary,
            left,
            r.y0 + 50.0,
            Some((r.width() - (left - r.x0) - 30.0) as f32),
            TextWeight::Semibold,
        );
        Self::txt(
            scene,
            tr,
            &format!("{}  ·  {}", drive.dev_path(), fmt_size(drive.size)),
            12.5,
            c.text_dim,
            left,
            r.y0 + 72.0,
            None,
            TextWeight::Regular,
        );

        let line = format!("⚠ Erases {}", drive.contents_summary());
        Self::txt(
            scene,
            tr,
            &line,
            11.5,
            c.semantic.danger,
            left,
            r.y1 - 24.0,
            Some((r.width() - (left - r.x0) - 30.0) as f32),
            TextWeight::Medium,
        );
    }

    fn draw_customize_step(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        let content = lay.content;
        if !self.customizable() {
            Self::draw_step_title(scene, tr, theme, content, "Prepare your setup", "");
            let card = Rect::new(
                content.x0,
                content.y0 + 60.0,
                content.x1,
                content.y0 + 60.0 + 150.0,
            );
            Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
            Self::stroke_rr(scene, t, card, 14.0, c.border_standard, 1.0);
            Self::txt_center(
                scene,
                tr,
                "This image is written unchanged",
                16.0,
                c.text_primary,
                f64::midpoint(card.x0, card.x1),
                card.y0 + 46.0,
                TextWeight::Semibold,
            );
            Self::txt_center(
                scene,
                tr,
                "Pre-configuration (hostname, user, region) is available for qOS",
                13.0,
                c.text_dim,
                f64::midpoint(card.x0, card.x1),
                card.y0 + 76.0,
                TextWeight::Regular,
            );
            Self::txt_center(
                scene,
                tr,
                "images. Press Continue to write it as-is.",
                13.0,
                c.text_dim,
                f64::midpoint(card.x0, card.x1),
                card.y0 + 96.0,
                TextWeight::Regular,
            );
            return;
        }
        let installing = self.action == Action::Install;
        let sub = if installing {
            "Set up the machine qOS will install — you can change any of this later."
        } else {
            "These choices travel with the installer, so setup on the device is shorter."
        };
        Self::draw_step_title(scene, tr, theme, content, "Prepare your setup", sub);

        // The whole form is the shared guided-customization panel.
        let removable = self.target().is_some_and(|d| d.removable);
        self.customize.draw(
            scene,
            t,
            tr,
            theme,
            presence,
            content,
            &self.profile,
            removable,
        );
    }

    fn draw_confirm_step(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let content = lay.content;
        let (title, sub) = match self.action {
            Action::Install => (
                "Confirm & install",
                "Review the install, then type the drive's name to unlock it.",
            ),
            Action::Flash => (
                "Confirm & write",
                "Review the write, then type the drive's name to unlock it.",
            ),
        };
        Self::draw_step_title(scene, tr, theme, content, title, sub);
        let card_bottom = self.draw_confirm_review_card(scene, tr, theme, t, content);
        self.draw_confirm_danger_banner(scene, tr, theme, t, content, card_bottom);
        self.draw_confirm_field(scene, tr, theme, t, content);
    }

    /// The review card: Image → Drive rows, capacity fit, and the action
    /// summary. Returns the card's bottom edge, so the danger banner below it
    /// can be positioned.
    fn draw_confirm_review_card(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let compact = Self::compact_content(content);
        let top = content.y0 + if compact { 64.0 } else { 46.0 };
        let height = if compact { 188.0 } else { 150.0 };
        let card = Rect::new(content.x0, top, content.x1, top + height);
        Self::fill_rr(scene, t, card, 14.0, c.surfaces.raised);
        Self::stroke_rr(scene, t, card, 14.0, c.border_standard, 1.0);
        self.draw_confirm_review_rows(scene, tr, theme, card);
        self.draw_confirm_capacity_and_action(scene, tr, theme, card);
        card.y1
    }

    /// The Image and Drive rows inside the review card.
    fn draw_confirm_review_rows(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
    ) {
        let c = &theme.tokens.colors;
        let compact = card.width() < 520.0;
        let row =
            |scene: &mut Scene, tr: &mut TextRenderer, y: f64, k: &str, v: String, ok: bool| {
                Self::txt(
                    scene,
                    tr,
                    k,
                    12.0,
                    c.text_dim,
                    card.x0 + 20.0,
                    y,
                    None,
                    TextWeight::Semibold,
                );
                let col = if ok {
                    c.text_primary
                } else {
                    c.semantic.danger
                };
                Self::txt(
                    scene,
                    tr,
                    &v,
                    14.0,
                    col,
                    card.x0 + 96.0,
                    y - 1.0,
                    Some((card.width() - 116.0) as f32),
                    TextWeight::Semibold,
                );
            };
        let src_v = self.source.as_ref().map_or_else(
            || "—".into(),
            |s| format!("{}   ({})", s.name, fmt_size(s.size)),
        );
        row(
            scene,
            tr,
            card.y0 + 20.0,
            "Image",
            src_v,
            self.source.is_some(),
        );
        let tgt_v = self.target().map_or_else(
            || "—".into(),
            |d| format!("{} · {} · {}", d.model, d.dev_path(), fmt_size(d.size)),
        );
        row(
            scene,
            tr,
            card.y0 + if compact { 54.0 } else { 58.0 },
            "Drive",
            tgt_v,
            self.target().is_some(),
        );
    }

    /// The capacity-fit line and the action summary inside the review card.
    fn draw_confirm_capacity_and_action(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
    ) {
        let c = &theme.tokens.colors;
        let compact = card.width() < 520.0;
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            let fit = if tgt.fits(src.size) {
                (
                    format!(
                        "fits — {} free afterwards",
                        fmt_size(tgt.size.saturating_sub(src.size))
                    ),
                    c.semantic.success,
                )
            } else {
                ("image is larger than the drive".into(), c.semantic.danger)
            };
            Self::txt(
                scene,
                tr,
                "Capacity",
                12.0,
                c.text_dim,
                card.x0 + 20.0,
                card.y0 + if compact { 98.0 } else { 92.0 },
                None,
                TextWeight::Semibold,
            );
            Self::txt(
                scene,
                tr,
                &fit.0,
                13.0,
                fit.1,
                card.x0 + 96.0,
                card.y0 + if compact { 97.0 } else { 91.0 },
                Some((card.width() - 116.0).max(0.0) as f32),
                TextWeight::Medium,
            );
        }
        let act = match self.action {
            Action::Install => format!(
                "Install qOS with your settings (host {}, user {})",
                self.profile.hostname, self.profile.user.username
            ),
            Action::Flash if self.customizable() => format!(
                "Write qOS, pre-configured (host {}, user {})",
                self.profile.hostname, self.profile.user.username
            ),
            Action::Flash => "Write the image, then read-back verify".to_string(),
        };
        Self::txt(
            scene,
            tr,
            "Action",
            12.0,
            c.text_dim,
            card.x0 + 20.0,
            card.y0 + if compact { 132.0 } else { 122.0 },
            None,
            TextWeight::Semibold,
        );
        Self::txt(
            scene,
            tr,
            &act,
            13.0,
            c.text_primary,
            card.x0 + 96.0,
            card.y0 + if compact { 131.0 } else { 121.0 },
            Some((card.width() - 116.0) as f32),
            TextWeight::Medium,
        );
    }

    /// Danger banner below the review card — louder for a fixed/internal
    /// (install) target.
    fn draw_confirm_danger_banner(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        card_bottom: f64,
    ) {
        let c = &theme.tokens.colors;
        let internal = self.target().is_some_and(|d| !d.removable);
        let system = self.target().is_some_and(|d| d.is_system);
        let compact = Self::compact_content(content);
        let banner_h = if compact { 58.0 } else { 44.0 };
        let banner = Rect::new(
            content.x0,
            card_bottom + 16.0,
            content.x1,
            card_bottom + 16.0 + banner_h,
        );
        Self::fill_rr(
            scene,
            t,
            banner,
            11.0,
            with_alpha(c.semantic.danger, if internal { 40 } else { 28 }),
        );
        Self::stroke_rr(
            scene,
            t,
            banner,
            11.0,
            with_alpha(c.semantic.danger, if internal { 170 } else { 130 }),
            if internal { 1.4 } else { 1.0 },
        );
        let warn = if system {
            "⚠  This is your system disk. Everything on it — the running OS included — is erased. This cannot be undone."
        } else if internal {
            "⚠  This erases the entire internal disk. This cannot be undone."
        } else {
            "⚠  This permanently erases the drive. This cannot be undone."
        };
        if compact {
            let (first, second) = if system {
                (
                    "⚠ This is your system disk; it will be erased.",
                    "This cannot be undone.",
                )
            } else if internal {
                (
                    "⚠ This erases the entire internal disk.",
                    "This cannot be undone.",
                )
            } else {
                (
                    "⚠ This permanently erases the drive.",
                    "This cannot be undone.",
                )
            };
            for (line, y) in [(first, banner.y0 + 10.0), (second, banner.y0 + 31.0)] {
                Self::txt(
                    scene,
                    tr,
                    line,
                    12.5,
                    c.semantic.danger,
                    banner.x0 + 14.0,
                    y,
                    Some((banner.width() - 28.0) as f32),
                    TextWeight::Semibold,
                );
            }
        } else {
            Self::txt(
                scene,
                tr,
                warn,
                13.0,
                c.semantic.danger,
                banner.x0 + 16.0,
                banner.y0 + 14.0,
                Some((banner.width() - 32.0) as f32),
                TextWeight::Semibold,
            );
        }
    }

    /// The typed confirmation field and the write-gate note below it.
    fn draw_confirm_field(
        &mut self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        let field = Rect::new(
            content.x0,
            content.y1 - 96.0,
            content.x1,
            content.y1 - 96.0 + 46.0,
        );
        let focused = self.confirm_input.is_focused();
        let matches = self.confirm_matches();
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        Self::txt(
            scene,
            tr,
            "CONFIRM THE TARGET DEVICE",
            11.0,
            c.text_dim,
            field.x0 + 2.0,
            field.y0 - 17.0,
            None,
            TextWeight::Semibold,
        );
        Self::fill_rr(scene, t, field, 11.0, c.neutral[2]);
        Self::stroke_rr(
            scene,
            t,
            field,
            11.0,
            if matches {
                c.semantic.success
            } else if focused {
                c.border_focus
            } else {
                with_alpha(c.semantic.danger, 160)
            },
            if focused || matches { 1.6 } else { 1.2 },
        );
        self.confirm_input
            .set_placeholder(&format!("type “{want}” here"));
        let text_right = if matches {
            field.x1 - 94.0
        } else {
            field.x1 - 12.0
        };
        self.confirm_input.apply_layout_external(Rect::new(
            field.x0 + 12.0,
            field.y0 + 6.0,
            text_right,
            field.y1 - 6.0,
        ));
        self.confirm_input
            .draw(scene, t, tr, theme, &PresenceCtx::empty());
        if matches {
            Self::txt_right(
                scene,
                tr,
                "✓ armed",
                12.5,
                c.semantic.success,
                field.x1 - 14.0,
                field.y0 + 14.0,
                TextWeight::Bold,
            );
        }
        // Keep implementation switches out of the product language. Shot mode
        // poses this as ready for documentation, but never enables the backend.
        let note = if crate::write::write_available() || self.shot_confirmation_ready {
            "Ready to write. Check the selected device before continuing."
        } else {
            "Writing is disabled in this preview."
        };
        Self::txt(
            scene,
            tr,
            note,
            11.0,
            c.neutral[6],
            field.x0 + 2.0,
            field.y1 + 8.0,
            None,
            TextWeight::Regular,
        );
    }

    fn draw_write_progress(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
    ) {
        let content = lay.content;
        let p = &self.progress;
        let writing = p.phase == JobPhase::Writing;
        let verb_ing = match self.action {
            Action::Install => "Installing",
            Action::Flash => "Writing",
        };
        let title = if writing {
            format!("{verb_ing}…")
        } else {
            "Verifying…".to_string()
        };
        Self::draw_step_title(scene, tr, theme, content, &title, "");
        self.draw_write_ring(scene, tr, theme, t, content);
        self.draw_write_rate_eta(scene, tr, theme, content);
        let yy = self.draw_write_phase_bars(scene, tr, theme, t, content);
        self.draw_write_source_target_line(scene, tr, theme, content, yy);
    }

    /// The big overall-progress ring with its percentage and phase caption.
    fn draw_write_ring(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        let p = &self.progress;
        let writing = p.phase == JobPhase::Writing;
        let verb_ing = match self.action {
            Action::Install => "Installing",
            Action::Flash => "Writing",
        };
        let cxp = f64::midpoint(content.x0, content.x1);
        let cyp = content.y0 + 150.0;
        let radius = 76.0;
        let overall = p.overall();
        scene.stroke(
            &Stroke::new(12.0),
            t,
            c.neutral[3],
            None,
            &Circle::new(Point::new(cxp, cyp), radius),
        );
        if overall > 0.0 {
            let mut arc = BezPath::new();
            let start = -std::f64::consts::FRAC_PI_2;
            let sweep = overall * std::f64::consts::TAU;
            let steps = (sweep.abs() / 0.08).ceil().max(1.0) as usize;
            arc.move_to((cxp + radius * start.cos(), cyp + radius * start.sin()));
            for i in 1..=steps {
                let a = start + sweep * (i as f64 / steps as f64);
                arc.line_to((cxp + radius * a.cos(), cyp + radius * a.sin()));
            }
            let ring_c = if writing {
                c.semantic.info
            } else {
                c.accent[6]
            };
            scene.stroke(&Stroke::new(12.0), t, ring_c, None, &arc);
        }
        Self::txt_center(
            scene,
            tr,
            &format!("{:.0}%", overall * 100.0),
            30.0,
            c.text_primary,
            cxp,
            cyp - 22.0,
            TextWeight::Bold,
        );
        let caption = if writing {
            verb_ing.to_ascii_lowercase()
        } else {
            "verifying".to_string()
        };
        Self::txt_center(
            scene,
            tr,
            &caption,
            12.0,
            c.text_dim,
            cxp,
            cyp + 14.0,
            TextWeight::Medium,
        );
    }

    /// The rate + ETA line below the ring, when either is known.
    fn draw_write_rate_eta(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
    ) {
        let c = &theme.tokens.colors;
        let p = &self.progress;
        let cxp = f64::midpoint(content.x0, content.x1);
        let cyp = content.y0 + 150.0;
        let radius = 76.0;
        let mut meta = String::new();
        if p.rate_bps > 0 {
            meta.push_str(&fmt_rate(p.rate_bps));
        }
        if let Some(src) = &self.source
            && let Some(eta) = p.eta_secs(src.size)
        {
            if !meta.is_empty() {
                meta.push_str("   ·   ");
            }
            let _ = write!(meta, "~{} left", fmt_eta(eta));
        }
        if !meta.is_empty() {
            Self::txt_center(
                scene,
                tr,
                &meta,
                13.0,
                c.text_dim,
                cxp,
                cyp + radius + 20.0,
                TextWeight::Medium,
            );
        }
    }

    /// The Write / Verify phase bars. Returns the `y` below them, for the
    /// image → drive line drawn after.
    fn draw_write_phase_bars(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let p = &self.progress;
        let writing = p.phase == JobPhase::Writing;
        let cyp = content.y0 + 150.0;
        let radius = 76.0;
        let bx0 = content.x0 + 40.0;
        let bx1 = content.x1 - 40.0;
        let mut yy = cyp + radius + 56.0;
        for (label, frac, active, fill) in [
            ("Write", p.write_frac, writing, c.semantic.info),
            ("Verify", p.verify_frac, !writing, c.semantic.success),
        ] {
            Self::txt(
                scene,
                tr,
                label,
                12.0,
                if active { c.text_primary } else { c.text_dim },
                bx0,
                yy,
                None,
                TextWeight::Semibold,
            );
            Self::txt_right(
                scene,
                tr,
                &format!("{:.0}%", frac * 100.0),
                12.0,
                c.text_dim,
                bx1,
                yy,
                TextWeight::Regular,
            );
            let bar = Rect::new(bx0, yy + 20.0, bx1, yy + 20.0 + 10.0);
            Self::bar(scene, t, bar, frac, c.neutral[3], fill);
            yy += 48.0;
        }
        yy
    }

    /// The "image → drive" caption below the phase bars.
    fn draw_write_source_target_line(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        content: Rect,
        yy: f64,
    ) {
        let c = &theme.tokens.colors;
        let cxp = f64::midpoint(content.x0, content.x1);
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            Self::txt_center(
                scene,
                tr,
                &format!("{}  →  {}", src.name, tgt.dev_path()),
                12.0,
                c.neutral[6],
                cxp,
                yy + 4.0,
                TextWeight::Regular,
            );
        }
    }

    fn draw_write_done(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        lay: &Layout,
        ok: bool,
    ) {
        let content = lay.content;
        let cyp = content.y0 + 120.0;
        let install = self.action == Action::Install;
        if ok {
            Self::draw_write_done_ok_hero(scene, tr, theme, t, content, cyp, install);
        } else {
            let headline = if install {
                "Install failed"
            } else {
                "Write failed"
            };
            let reason = self
                .progress
                .error
                .as_deref()
                .unwrap_or("The drive may be incomplete. Review the plan before trying again.");
            Self::draw_write_done_failed_hero(
                scene,
                tr,
                theme,
                t,
                content,
                cyp,
                (headline, reason),
            );
        }
        self.draw_write_done_summary(scene, tr, theme, t, content, ok);
    }

    /// The success hero: check disc + headline + sub for the completed step.
    fn draw_write_done_ok_hero(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        cyp: f64,
        install: bool,
    ) {
        let c = &theme.tokens.colors;
        let cxp = f64::midpoint(content.x0, content.x1);
        let compact = Self::compact_content(content);
        Self::check_disc(
            scene,
            t,
            Point::new(cxp, cyp),
            46.0,
            c.semantic.success,
            Color::WHITE,
        );
        let (headline, sub): (&str, &str) = if install {
            (
                "qOS is installed",
                "The disk was written and verified — remove the media and reboot.",
            )
        } else {
            (
                "Drive is ready",
                "The image was written and verified — you can safely remove the drive.",
            )
        };
        Self::txt_center(
            scene,
            tr,
            headline,
            26.0,
            c.text_primary,
            cxp,
            cyp + 66.0,
            TextWeight::Bold,
        );
        if compact {
            let lines = if install {
                ["Written and verified.", "Remove the media and reboot."]
            } else {
                ["Written and verified.", "You can safely remove the drive."]
            };
            for (line, y) in [(lines[0], cyp + 100.0), (lines[1], cyp + 120.0)] {
                Self::txt_center(
                    scene,
                    tr,
                    line,
                    13.5,
                    c.text_dim,
                    cxp,
                    y,
                    TextWeight::Regular,
                );
            }
        } else {
            Self::txt_center(
                scene,
                tr,
                sub,
                14.0,
                c.text_dim,
                cxp,
                cyp + 100.0,
                TextWeight::Regular,
            );
        }
    }

    /// The failure hero: danger disc with an "!", headline + sub.
    fn draw_write_done_failed_hero(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        cyp: f64,
        copy: (&str, &str),
    ) {
        let c = &theme.tokens.colors;
        let cxp = f64::midpoint(content.x0, content.x1);
        let compact = Self::compact_content(content);
        scene.fill(
            Fill::NonZero,
            t,
            c.semantic.danger,
            None,
            &Circle::new(Point::new(cxp, cyp), 46.0),
        );
        Self::txt_center(
            scene,
            tr,
            "!",
            44.0,
            Color::WHITE,
            cxp,
            cyp - 34.0,
            TextWeight::Bold,
        );
        Self::txt_center(
            scene,
            tr,
            copy.0,
            26.0,
            c.text_primary,
            cxp,
            cyp + 66.0,
            TextWeight::Bold,
        );
        let max_width = (content.width() - if compact { 20.0 } else { 80.0 }).max(40.0) as f32;
        for (index, line) in Self::failure_detail_lines(tr, copy.1, max_width, 2)
            .iter()
            .enumerate()
        {
            Self::txt_center(
                scene,
                tr,
                line,
                13.5,
                c.text_dim,
                cxp,
                cyp + 100.0 + index as f64 * 20.0,
                TextWeight::Regular,
            );
        }
    }

    /// The Image/Drive summary card below the hero, when both are known.
    fn draw_write_done_summary(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        theme: &Theme,
        t: Affine,
        content: Rect,
        ok: bool,
    ) {
        let c = &theme.tokens.colors;
        let cyp = content.y0 + 120.0;
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            let compact = Self::compact_content(content);
            let inset = if compact { 0.0 } else { 80.0 };
            let top = cyp + if compact { 154.0 } else { 140.0 };
            let card = Rect::new(content.x0 + inset, top, content.x1 - inset, top + 84.0);
            Self::fill_rr(scene, t, card, 12.0, c.surfaces.raised);
            Self::stroke_rr(scene, t, card, 12.0, c.border_standard, 1.0);
            Self::txt(
                scene,
                tr,
                "Image",
                12.0,
                c.text_dim,
                card.x0 + 18.0,
                card.y0 + 18.0,
                None,
                TextWeight::Semibold,
            );
            Self::txt(
                scene,
                tr,
                &src.name,
                13.5,
                c.text_primary,
                card.x0 + if compact { 76.0 } else { 90.0 },
                card.y0 + 17.0,
                Some(
                    if compact && ok {
                        card.width() - 176.0
                    } else if compact {
                        card.width() - 94.0
                    } else {
                        card.width() - 110.0
                    }
                    .max(0.0) as f32,
                ),
                TextWeight::Medium,
            );
            Self::txt(
                scene,
                tr,
                "Drive",
                12.0,
                c.text_dim,
                card.x0 + 18.0,
                card.y0 + 48.0,
                None,
                TextWeight::Semibold,
            );
            Self::txt(
                scene,
                tr,
                &format!("{} · {}", tgt.model, tgt.dev_path()),
                13.5,
                c.text_primary,
                card.x0 + if compact { 76.0 } else { 90.0 },
                card.y0 + 47.0,
                Some((card.width() - if compact { 94.0 } else { 110.0 }).max(0.0) as f32),
                TextWeight::Medium,
            );
            if ok {
                Self::txt_right(
                    scene,
                    tr,
                    "✓ verified",
                    12.5,
                    c.semantic.success,
                    card.x1 - 18.0,
                    card.y0 + 18.0,
                    TextWeight::Bold,
                );
            }
        }
    }
}

// =========================================================================
// Interaction / state mutation
// =========================================================================
impl ImagerApp {
    fn select_source(&mut self, state: &mut AutoCommit, src: &Source) {
        if let Some(prev) = &self.source
            && !prev.path.is_empty()
            && !self.recent.iter().any(|s| s.path == prev.path)
        {
            self.recent.insert(0, prev.clone());
            self.recent.truncate(6);
            ImagerDoc::set_recent(state, &self.recent);
        }
        // A non-installable source cannot carry a pre-config profile.
        if !src.installable {
            self.action = Action::Flash;
            ImagerDoc::set_action(state, Action::Flash);
        }
        self.source = Some(src.clone());
        ImagerDoc::set_source(state, Some(src));
        self.set_status(state, &format!("Selected {}", src.name));
        self.dirty = true;
    }

    fn open_file_picker(&mut self, state: &mut AutoCommit) {
        #[cfg(target_os = "linux")]
        {
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
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.portal_rx = Some(crate::probe::pick_image_native());
        }
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

    fn download_release(&mut self, state: &mut AutoCommit, release: QosRelease) {
        if self.release_rx.is_some() {
            self.set_status(state, "A qOS installer download is already in progress.");
            return;
        }
        self.set_status(
            state,
            &format!("Downloading and verifying qOS {}…", release.version),
        );
        self.release_rx = Some(crate::release::spawn_download(release));
        self.dirty = true;
    }

    fn drain_release(&mut self, state: &mut AutoCommit) {
        let Some(rx) = &self.release_rx else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("The qOS installer download stopped unexpectedly.".into())
            }
        };
        self.release_rx = None;
        match result {
            Ok(source) => {
                if let Some(release) = self
                    .releases
                    .iter_mut()
                    .find(|release| source.name == format!("{} {}", release.name, release.version))
                {
                    release.local_path.clone_from(&source.path);
                }
                ImagerDoc::set_releases(state, &self.releases);
                self.select_source(state, &source);
                self.set_status(
                    state,
                    "qOS installer downloaded and cryptographically verified.",
                );
            }
            Err(error) => self.set_status(state, &error),
        }
        self.dirty = true;
    }

    fn select_target(&mut self, state: &mut AutoCommit, idx: usize) {
        let Some(drive) = self.targets.get(idx) else {
            return;
        };
        // Hard guard repeated below the visible-list filter: stale persisted or
        // collaborative state cannot turn the media creator into an internal-
        // disk raw writer.
        if !drive.is_media_target() {
            self.set_status(
                state,
                "Internal and system disks can only be installed from the booted qOS medium.",
            );
            self.dirty = true;
            return;
        }
        self.selected = Some(idx);
        ImagerDoc::set_selected(state, Some(idx));
        self.confirm_input.set_text("");
        self.dirty = true;
    }

    fn set_status(&mut self, state: &mut AutoCommit, s: &str) {
        self.status = s.to_string();
        ImagerDoc::set_status(state, s);
    }

    fn goto(&mut self, state: &mut AutoCommit, s: Step) {
        self.step = s;
        self.scroll_y = 0.0;
        self.keyboard_hit = None;
        ImagerDoc::set_step(state, s);
        self.confirm_input.set_focus(false);
        self.customize.clear_focus();
        self.dirty = true;
    }

    fn advance(&mut self, state: &mut AutoCommit) {
        match self.step {
            Step::Source if self.can_advance(Step::Source) => self.goto(state, Step::Drive),
            Step::Drive if self.can_advance(Step::Drive) => {
                self.action = Action::Flash;
                ImagerDoc::set_action(state, Action::Flash);
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
    fn perform_operation(
        &mut self,
        operation: &crate::actions::ImagerOperation,
        state: &mut AutoCommit,
    ) -> Result<(), String> {
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
                    Err("The raw-device writer cannot yet stop safely between write chunks".into())
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
                crate::actions::ImagerOperation::Write { .. } => {
                    Err("Invalid image-write operation dispatch".into())
                }
            };
        };
        let (Some(src), Some(tgt)) = (self.source.clone(), self.target().cloned()) else {
            return Err("Select a source and drive first.".into());
        };
        if src.installable {
            self.profile.validate_media_profile()?;
        }
        if *device != tgt.dev_path() {
            return Err("The selected target changed; review the write plan again.".into());
        }
        let req = WriteRequest {
            source: src,
            target: tgt.clone(),
            action: self.action,
            profile: self.profile.clone(),
        };
        let (start, rx) = spawn_job(req, confirmation);
        match start {
            crate::write::JobStart::Started => {
                self.job_rx = rx;
                self.progress = Progress {
                    phase: JobPhase::Writing,
                    write_frac: 0.0,
                    verify_frac: 0.0,
                    rate_bps: 0,
                    error: None,
                };
                ImagerDoc::set_progress(state, &self.progress);
                self.set_status(
                    state,
                    &format!(
                        "{} → {} (writing)",
                        self.source
                            .as_ref()
                            .map(|s| s.name.clone())
                            .unwrap_or_default(),
                        tgt.dev_path()
                    ),
                );
                // The write has begun: move onto the live-progress station.
                self.goto(state, Step::Write);
            }
            crate::write::JobStart::ConfirmationMismatch => {
                return Err("Confirmation does not match — write refused.".into());
            }
            crate::write::JobStart::TooSmall => {
                return Err("Image is larger than the drive — write refused.".into());
            }
            crate::write::JobStart::UnsafeTarget => {
                return Err(
                    "Internal and system disks can only be installed from the booted qOS medium."
                        .into(),
                );
            }
            crate::write::JobStart::Incomplete => {
                return Err("Select a source and drive before writing.".into());
            }
            crate::write::JobStart::InvalidProfile(message) => return Err(message),
            crate::write::JobStart::WriteDisabled => {
                return Err(
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
                        ImagerDoc::set_progress(state, &self.progress);
                        self.set_status(
                            state,
                            "The writer stopped unexpectedly. Reconnect the drive, then review and retry.",
                        );
                        self.goto(state, Step::Done);
                    }
                    self.job_rx = None;
                    self.dirty = true;
                    return;
                }
            }
        }
        if let Some(p) = latest {
            let terminal_phase = p.phase;
            let terminal_error = p.error.clone();
            ImagerDoc::set_progress(state, &p);
            self.progress = p;
            self.dirty = true;
            if matches!(terminal_phase, JobPhase::Success | JobPhase::Failed) {
                self.job_rx = None;
                let name = self
                    .source
                    .as_ref()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                let msg = if terminal_phase == JobPhase::Success {
                    format!("Done — {name} {} and verified", self.action.past())
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
            ImagerDoc::set_progress(state, &self.progress);
            self.confirm_input.set_text("");
            self.goto(state, Step::Confirm);
            self.set_status(
                state,
                "Review the image and drive, then confirm the target again to retry.",
            );
            return;
        }
        self.progress = Progress::default();
        ImagerDoc::set_progress(state, &self.progress);
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

    fn do_rescan(&mut self, state: &mut AutoCommit) {
        if live_probe_enabled() {
            // Re-read the live block-device inventory. `probe()` returns `None`
            // only when `lsblk` is unavailable — a transient failure — so in
            // that case we keep the last-known list rather than blanking it.
            // A real (possibly empty) result is authoritative and replaces it,
            // so unplugging the selected drive genuinely drops it from the list.
            let remembered = self.target().map(|d| d.name.clone());
            if let Some(t) = crate::probe::probe() {
                self.targets.clone_from(&t);
                ImagerDoc::write_targets(state, &t);
                // Keep the same physical drive selected across a rescan by name,
                // not by index (indices shift as drives come and go).
                self.selected =
                    remembered.and_then(|name| self.targets.iter().position(|d| d.name == name));
            }
            let n = self.targets.len();
            let removable = self.removable_count();
            self.set_status(
                state,
                &format!("Rescanned — {removable} removable, {n} total"),
            );
        } else {
            self.set_status(state, "Rescanned drives");
        }
        self.selected = self.selected.filter(|i| *i < self.targets.len());
        ImagerDoc::set_selected(state, self.selected);
        self.dirty = true;
    }

    fn dispatch_hit(&mut self, state: &mut AutoCommit, hit: Hit) {
        match hit {
            Hit::PickFile => self.open_file_picker(state),
            Hit::Release(i) => {
                if let Some(release) = self.releases.get(i).cloned() {
                    if release.url.is_empty() {
                        self.select_source(state, &release.to_source());
                    } else {
                        self.download_release(state, release);
                    }
                }
            }
            Hit::Recent(i) => {
                if let Some(src) = self.recent.get(i).cloned() {
                    self.select_source(state, &src);
                }
            }
            Hit::Target(i) => self.select_target(state, i),
            Hit::Rescan => self.do_rescan(state),
            Hit::ConfirmField => {
                self.confirm_input.set_focus(true);
                self.customize.clear_focus();
                self.dirty = true;
            }
            Hit::StepJump(s) => {
                if self.crumb_reachable(s) {
                    self.goto(state, s);
                }
            }
            Hit::Back => self.back(state),
            Hit::Next => self.advance(state),
            Hit::StartWrite => self.start_write(state),
            Hit::WriteAnother => self.write_another(state),
        }
    }
}

// =========================================================================
// Widget impl
// =========================================================================
impl Widget for ImagerApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::IMAGER_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            if ImagerDoc::is_empty(state) {
                // Start genuinely blank. In the live app a real `lsblk` probe
                // fills the drive list; if it finds nothing (or is unavailable)
                // the list stays empty and the Drive step shows its honest
                // "insert a drive" state — the imager never invents drives.
                // Sample content is opt-in (`--demo` / `--shots`), which seeds
                // the document *before* the first sync so this branch is skipped.
                let targets = if live_probe_enabled() {
                    crate::probe::probe().unwrap_or_default()
                } else {
                    Vec::new()
                };
                ImagerDoc::seed(state, &targets);
            }
            self.seeded = true;
        }

        if live_probe_enabled() && ImagerDoc::releases(state).is_empty() {
            match crate::release::bundled_releases() {
                Ok(releases) => ImagerDoc::set_releases(state, &releases),
                Err(error) => ImagerDoc::set_status(state, &error),
            }
        }

        // Fold in any async work before reading the doc back.
        self.drain_portal(state);
        self.drain_release(state);
        self.drain_job(state);

        self.targets = ImagerDoc::targets(state);
        self.source = ImagerDoc::source(state);
        self.recent = ImagerDoc::recent(state);
        self.releases = ImagerDoc::releases(state);
        self.profile = ImagerDoc::profile(state);
        self.progress = ImagerDoc::progress(state);
        self.action = Action::Flash;
        if ImagerDoc::action(state) != Action::Flash {
            ImagerDoc::set_action(state, Action::Flash);
        }
        self.status = ImagerDoc::status(state);
        self.step = ImagerDoc::step(state);
        self.selected = ImagerDoc::selected(state).filter(|i| {
            self.targets
                .get(*i)
                .is_some_and(TargetDrive::is_media_target)
        });
        if self.selected != ImagerDoc::selected(state) {
            ImagerDoc::set_selected(state, self.selected);
        }

        // Refresh the shared customization panel's fields from the profile
        // (each unfocused field mirrors the stored value; a focused one stays
        // the source of truth).
        self.customize.sync(&self.profile);
        if self.export_after_sync {
            self.export_after_sync = false;
            self.export_config(state);
        }
        self.dirty = true;
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, transform, c.surfaces.base, None, &self.rect);
        let lay = self.compute_layout();
        let tr = text_renderer;

        self.draw_header(scene, tr, theme, transform, &lay);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &lay.viewport_content,
        );
        match self.step {
            Step::Source => self.draw_source_step(scene, tr, theme, transform, &lay),
            Step::Drive => self.draw_drive_step(scene, tr, theme, transform, &lay),
            Step::Customize => {
                self.draw_customize_step(scene, tr, theme, transform, &lay, presence);
            }
            Step::Confirm => self.draw_confirm_step(scene, tr, theme, transform, &lay),
            Step::Write => self.draw_write_progress(scene, tr, theme, transform, &lay),
            Step::Done => self.draw_write_done(
                scene,
                tr,
                theme,
                transform,
                &lay,
                self.progress.phase == JobPhase::Success,
            ),
        }
        scene.pop_layer();
        self.draw_scrollbar(scene, theme, transform, &lay);
        self.draw_footer(scene, tr, theme, transform, &lay);
        if let Some(focused) = self.keyboard_hit
            && let Some((rect, _)) = lay.hits.iter().find(|(_, hit)| *hit == focused)
        {
            Self::stroke_rr(
                scene,
                transform,
                rect.inflate(2.0, 2.0),
                12.0,
                theme.tokens.colors.border_focus,
                2.0,
            );
        }

        if self.export_dialog.is_open() {
            self.export_dialog.layout_in(self.rect, theme, tr);
            self.export_dialog
                .draw_detached(scene, transform, tr, theme, presence);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the canvas wizard routes modal, text-field, profile-panel, pointer, scroll and keyboard capture in one explicitly ordered event transaction"
    )]
    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        use winit::event::{ElementState, KeyEvent, MouseButton};

        let lay = self.compute_layout();

        // The detached shared modal owns capture and dismissal before any
        // wizard field or shortcut sees the event.
        if self
            .export_dialog
            .handle_detached(event, scale_factor, *modifiers, state, path)
        {
            *handled = true;
            self.dirty = true;
            return;
        }

        if self.handle_tab_event(event, *modifiers, &lay) {
            *handled = true;
            self.dirty = true;
            return;
        }

        // Destructive consent is peer-local and goes through the toolkit's
        // real editor before any wizard shortcut sees the event.
        if self.step == Step::Confirm && !self.export_dialog.is_open() {
            let mut input_handled = false;
            let mut focus_target = None;
            self.confirm_input.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                &mut input_handled,
                &mut focus_target,
                node_id,
                taffy,
            );
            if focus_target.is_some() {
                self.confirm_input.set_focus(true);
            }
            if self.confirm_input.take_dirty().is_some() {
                self.dirty = true;
            }
            if self.confirm_input.take_submit() {
                if self.can_advance(Step::Confirm) {
                    self.start_write(state);
                }
                self.confirm_input.set_focus(false);
                *handled = true;
                self.dirty = true;
                return;
            }
            if input_handled {
                *handled = true;
                self.dirty = true;
                return;
            }
        }

        // The Customize step is delegated in full to the shared customization
        // panel, which owns its fields, toggles and export affordance. Only
        // events it does not consume (footer clicks, global keys) fall through
        // to the wizard chrome below.
        let consumed = self.handle_customize_event(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            &lay,
            node_id,
            taffy,
        );
        if consumed {
            *handled = true;
            self.dirty = true;
            return;
        }

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x, position.y, scale_factor, &lay);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(state, &lay, handled);
            }
            WindowEvent::MouseWheel { delta, .. }
                if lay.viewport_content.contains(self.last_mouse) =>
            {
                use winit::event::MouseScrollDelta;
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 52.0,
                    MouseScrollDelta::PixelDelta(point) => point.y / scale_factor,
                };
                let next = (self.scroll_y - dy).clamp(0.0, self.max_scroll());
                if (next - self.scroll_y).abs() > f64::EPSILON {
                    self.scroll_y = next;
                    self.dirty = true;
                }
                *handled = true;
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
                self.handle_key_pressed(logical_key, state, handled);
            }
            _ => {}
        }
    }

    /// The resource tree the shell's Ctrl+K palette walks — delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`]. Empty unless an installable qOS image is selected.
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        let mut resources = <Self as qos_actions::ResourceProvider>::resources(self, doc);
        let available = self.source.is_some() && self.target().is_some();
        let mut job = crate::actions::operation_resource(
            self.target().map(TargetDrive::dev_path).as_deref(),
            available,
            self.job_rx.is_some(),
            crate::write::write_available(),
        );
        job.actions.push(
            crate::actions::operation_spec(
                "export-config",
                "Export headless configuration",
                qos_actions::OperationKind::View,
                crate::actions::ImagerOperation::ExportConfig,
            )
            .with_contract(qos_actions::ActionContract {
                availability: if available {
                    qos_actions::Availability::Available
                } else {
                    qos_actions::Availability::Unavailable {
                        code: "incomplete_plan".into(),
                        reason: "Select an image source and target drive first".into(),
                    }
                },
                result: qos_actions::ResultSpec::Json {
                    schema: serde_json::json!({ "type": "string" }),
                },
                discovery: qos_actions::DiscoveryScope::Launchable,
                ..qos_actions::ActionContract::view()
            }),
        );
        resources.push(job);
        resources
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
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
            | crate::actions::ImagerOperation::ExportConfig => qos_actions::OperationKind::View,
            _ => qos_actions::OperationKind::Effect,
        };
        if kind != expected_kind {
            return Err("Imager operation kind does not match its contract".into());
        }
        let mut local_doc = doc.clone();
        self.perform_operation(&operation, &mut local_doc)?;
        Ok(true)
    }

    fn invoke_resource_operation_v2(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<qos_actions::ActionOutput>, String> {
        match crate::actions::decode_operation(type_id, payload)? {
            Some(crate::actions::ImagerOperation::Status) => {
                if kind != qos_actions::OperationKind::View {
                    return Err("Imager status is a view operation".into());
                }
                return Ok(Some(qos_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "phase": self.progress.phase.key(),
                        "progress": self.progress.overall(),
                        "message": self.status,
                    })),
                    resources: Vec::new(),
                }));
            }
            Some(crate::actions::ImagerOperation::ExportConfig) => {
                if kind != qos_actions::OperationKind::View {
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
                return Ok(Some(qos_actions::ActionOutput {
                    value: Some(serde_json::Value::String(text)),
                    resources: Vec::new(),
                }));
            }
            _ => {}
        }
        self.invoke_resource_operation(kind, type_id, payload, doc, path)
            .map(|handled| handled.then(qos_actions::ActionOutput::default))
    }

    /// The framework host drains the undo boundary an on-screen profile control
    /// dispatched, so it records exactly one boundary bracketing it — the same
    /// path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
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
        state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        use ui::a11y::Action as AccessAction;

        if self.export_dialog.is_open() {
            return false;
        }
        if let Some(field_key) = target.strip_prefix("imager:profile:") {
            if self.step != Step::Customize || !self.customizable() {
                return false;
            }
            if field_key == "preview" {
                return match action {
                    AccessAction::Focus => {
                        self.focus_accessible_profile_slot(6);
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
                        qos_actions::ActionContext::for_action(
                            "org.qos.Imager",
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
            .and_then(|version| {
                self.releases
                    .iter()
                    .position(|release| release.version == version)
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

impl ImagerApp {
    /// The Customize step's delegated handling. Returns `true` when the panel
    /// consumed the event, so the caller stops before the wizard chrome below
    /// ever sees it.
    #[allow(clippy::too_many_arguments)]
    fn handle_customize_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        lay: &Layout,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if !(self.step == Step::Customize && self.customizable() && !self.export_dialog.is_open()) {
            return false;
        }
        let removable = self.target().is_some_and(|d| d.removable);
        let profile = self.profile.clone();
        let resp = self.customize.handle_event(
            event,
            scale_factor,
            &modifiers,
            state,
            path,
            &mut self.actions,
            &profile,
            removable,
            lay.content,
            node_id,
            taffy,
        );
        if resp.export_requested {
            self.export_config(state);
        }
        resp.handled
    }

    /// Track the pointer and which drive card (if any) is under it.
    fn handle_cursor_moved(&mut self, px: f64, py: f64, scale_factor: f64, lay: &Layout) {
        let p = Point::new(px / scale_factor, py / scale_factor);
        self.last_mouse = p;
        let hover_now = lay.hits.iter().find_map(|(r, h)| match h {
            Hit::Target(i) if r.contains(p) => Some(*i),
            _ => None,
        });
        self.hover_target = hover_now;
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
                if let Some(rect) = CustomizePanel::keyboard_slot_rect(lay.content, next) {
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
        // qOS image + recent history) — the same content `--shots`/`--demo` use.
        let mut app = ImagerApp::new();
        let mut doc = AutoCommit::new();
        ImagerDoc::seed_demo(&mut doc);
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        (app, doc)
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
            1.0,
            &m,
            doc,
            &automerge::ROOT,
            &mut h,
            &mut ft,
            node,
            &taffy,
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

    #[test]
    fn main_canvas_semantics_expose_heading_sources_and_keyboard_order() {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);

        let tree = app.accessibility_tree();
        assert_eq!(tree.role, ui::a11y::Role::Window);
        assert_eq!(tree.name.as_deref(), Some("Start Media Creator"));
        let heading = accessible_named(&tree, "Choose the source image").expect("step heading");
        assert_eq!(heading.role, ui::a11y::Role::Heading { level: 1 });
        let selected = accessible_named(&tree, "Selected image").expect("selected image");
        assert!(
            selected
                .value
                .as_deref()
                .is_some_and(|value| value.contains("qOS"))
        );

        let mut order = Vec::new();
        accessible_focus_order(&tree, &mut order);
        assert_eq!(
            order.first().map(String::as_str),
            Some("Step 1 of 6: Source")
        );
        let browse = order
            .iter()
            .position(|name| name == "Browse for an image")
            .expect("browse in focus order");
        let continue_button = order
            .iter()
            .position(|name| name == "Continue")
            .expect("continue in focus order");
        assert!(browse < continue_button);
        assert!(
            accessible_named(&tree, "Step 3 of 6: Customize")
                .expect("future customize step")
                .state
                .disabled
        );
    }

    #[test]
    fn profile_and_confirmation_semantics_are_editable_and_focusable() {
        use ui::a11y::Action as AccessAction;

        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1180.0, 820.0);
        app.select_target(&mut doc, 0);
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
            "Preview embedded setup",
        ];
        let positions = profile_order.map(|name| {
            order
                .iter()
                .position(|candidate| candidate == name)
                .unwrap_or_else(|| panic!("missing accessible profile control {name}"))
        });
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        for name in ["Language and locale", "Time zone", "Keyboard layout"] {
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
        assert!(ImagerDoc::status(&doc).contains("Choose a listed Time zone"));

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

    #[test]
    fn write_progress_and_status_are_live_regions() {
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

    #[test]
    fn sync_reads_demo_state_at_source_step() {
        let (app, _doc) = seeded_app();
        assert_eq!(app.targets.len(), 2);
        assert!(app.source.is_some());
        assert_eq!(app.step, Step::Source);
        assert_eq!(app.recent.len(), 3);
    }

    #[test]
    fn live_app_starts_blank_with_no_fabricated_content() {
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

    /// A release row is a *write source for erasing a drive*, so the live app
    /// must offer none of its own invention: the list comes from the document,
    /// and a live (unseeded) document has nothing in it. Sample releases are
    /// opt-in via `--demo` / `--shots` like every other sample surface here.
    #[test]
    fn live_app_offers_no_qos_release_to_write() {
        let mut app = ImagerApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.releases().is_empty(), "no fabricated qOS releases");

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

    /// The gate is a gate, not a deletion: the demo document still offers them.
    #[test]
    fn demo_document_offers_its_seeded_releases() {
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

    #[test]
    fn layout_has_six_step_stations() {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let lay = app.compute_layout();
        assert_eq!(lay.crumbs.len(), 6);
        assert!(!lay.compact_crumbs);
    }

    #[test]
    fn compact_layout_keeps_every_step_station_inside_the_viewport() {
        let (mut app, _doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 320.0, 640.0);
        let lay = app.compute_layout();

        assert!(lay.compact_crumbs);
        assert_eq!(lay.crumbs.len(), Step::all().len());
        assert!(lay.crumbs.iter().all(|(rect, _)| {
            rect.x0 >= app.rect.x0
                && rect.x1 <= app.rect.x1
                && rect.width() >= 40.0
                && rect.height() >= 34.0
        }));
        assert_eq!(
            lay.crumbs.first().map(|(_, step)| *step),
            Some(Step::Source)
        );
        assert_eq!(lay.crumbs.last().map(|(_, step)| *step), Some(Step::Done));
    }

    #[test]
    fn compact_step_targets_use_the_same_on_screen_rects_as_the_rail() {
        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 320.0, 640.0);
        app.select_target(&mut doc, 0);
        app.goto(&mut doc, Step::Confirm);
        let lay = app.compute_layout();

        for (rect, step) in &lay.crumbs {
            if app.crumb_reachable(*step) {
                assert!(lay.hits.iter().any(|(hit_rect, hit)| {
                    hit_rect == rect && matches!(hit, Hit::StepJump(hit_step) if hit_step == step)
                }));
            }
        }
    }

    #[test]
    fn compact_body_controls_reserve_space_for_wrapped_headings() {
        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 320.0, 640.0);

        let source = app.compute_layout();
        let browse = source
            .hits
            .iter()
            .find_map(|(rect, hit)| matches!(hit, Hit::PickFile).then_some(*rect))
            .unwrap();
        assert!(browse.y0 >= source.content.y0 + 64.0);
        assert!(browse.x0 >= source.content.x0 && browse.x1 <= source.content.x1);

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
        assert!(rescan.y0 >= drive.content.y0 + 52.0);
        assert!(target.y0 >= rescan.y1 + 12.0);
    }

    #[test]
    fn compact_footer_actions_never_overlap() {
        let (mut app, mut doc) = seeded_app();
        app.rect = Rect::new(0.0, 0.0, 320.0, 640.0);

        for step in [Step::Drive, Step::Customize, Step::Confirm] {
            app.goto(&mut doc, step);
            let lay = app.compute_layout();
            let back = lay.back.unwrap();
            let primary = lay.primary.unwrap();
            assert!(back.x0 >= lay.content.x0);
            assert!(primary.x1 <= lay.content.x1);
            assert!(back.x1 + 12.0 <= primary.x0);
            assert!(back.width() >= 130.0 && primary.width() >= 130.0);
        }
    }

    #[test]
    fn short_viewport_scrolls_body_without_moving_sticky_footer() {
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

    #[test]
    fn tab_reaches_every_profile_control_and_both_footer_actions() {
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

    #[test]
    fn advancing_requires_selections() {
        let (mut app, mut doc) = seeded_app();
        // Source is seeded → can advance to Drive.
        app.advance(&mut doc);
        assert_eq!(app.step, Step::Drive);
        // No target selected yet → stays on Drive.
        app.advance(&mut doc);
        assert_eq!(app.step, Step::Drive);
        app.select_target(&mut doc, 0);
        app.advance(&mut doc);
        // Seeded source is an installable qOS image → goes through Customize.
        assert_eq!(app.step, Step::Customize);
        app.advance(&mut doc);
        assert_eq!(app.step, Step::Confirm);
    }

    #[test]
    fn privileged_helper_capability_enables_confirm_but_disabled_backend_does_not() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0);
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

    #[test]
    fn pending_release_download_keeps_the_event_loop_awake() {
        let (mut app, _doc) = seeded_app();
        assert!(!app.needs_animation_frame());

        let (_tx, rx) = std::sync::mpsc::channel();
        app.release_rx = Some(rx);
        assert!(app.needs_animation_frame());

        app.release_rx = None;
        assert!(!app.needs_animation_frame());
    }

    #[test]
    fn invalid_media_profile_blocks_confirm_and_direct_write() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0);
        app.profile.hostname = "bad host".into();
        app.goto(&mut doc, Step::Customize);

        app.advance(&mut doc);
        assert_eq!(app.step, Step::Customize);
        assert!(ImagerDoc::status(&doc).contains("Hostname"));

        // A direct operation dispatch must repeat the gate rather than relying
        // only on wizard navigation.
        app.goto(&mut doc, Step::Confirm);
        app.confirm_input.set_text("sdb");
        app.start_write(&mut doc);
        assert_eq!(app.step, Step::Confirm);
        assert!(app.job_rx.is_none());
        assert!(ImagerDoc::status(&doc).contains("Hostname"));
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

        // Drive → Customize (the seeded qOS image is customizable).
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
    fn plain_iso_skips_customize() {
        let (mut app, mut doc) = seeded_app();
        let iso = app
            .recent
            .iter()
            .find(|s| s.kind == SourceKind::IsoFile)
            .cloned()
            .unwrap();
        app.select_source(&mut doc, &iso);
        app.advance(&mut doc); // → Drive
        app.select_target(&mut doc, 0);
        app.advance(&mut doc); // installable == false → straight to Confirm
        assert_eq!(app.step, Step::Confirm);
        assert!(!app.customizable());
    }

    #[test]
    fn write_refused_without_matching_confirmation() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0); // sdb
        app.goto(&mut doc, Step::Confirm);
        app.confirm_input.set_text("wrong");
        app.start_write(&mut doc);
        assert_eq!(app.progress.phase, JobPhase::Idle, "no job started");
        assert!(app.job_rx.is_none());
        assert_eq!(app.step, Step::Confirm, "stays on the Confirm station");
        assert!(ImagerDoc::status(&doc).to_lowercase().contains("confirm"));
    }

    #[test]
    #[cfg(not(feature = "host-media-writer"))]
    fn matching_confirmation_does_not_simulate_a_disabled_write() {
        if crate::write::allow_write() {
            return;
        }
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0); // sdb
        app.goto(&mut doc, Step::Confirm);
        app.confirm_input.set_text("sdb");
        app.start_write(&mut doc);
        assert_eq!(app.progress.phase, JobPhase::Idle);
        assert_eq!(app.step, Step::Confirm);
        assert!(app.job_rx.is_none());
        assert!(ImagerDoc::status(&doc).contains("disabled"));
    }

    #[test]
    fn portal_open_selects_inspected_source() {
        let (mut app, mut doc) = seeded_app();
        // Simulate the portal returning a real qOS-looking image.
        let f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
        std::fs::write(f.path(), b"qos payload for the imager test").unwrap();
        app.portal_rx = Some(ui::portal::inject_for_test(PortalOutcome::Opened(
            f.path().to_path_buf(),
        )));
        app.drain_portal(&mut doc);
        // A `.iso` file → a valid source is selected.
        assert!(app.source.is_some());
        assert_eq!(app.source.as_ref().unwrap().kind, SourceKind::IsoFile);
    }

    #[test]
    fn drive_step_only_offers_external_installation_media() {
        let (app, _doc) = seeded_app();
        // Sample has one USB (removable) + one NVMe (fixed).
        assert_eq!(app.visible_targets().len(), 1);
        assert_eq!(app.removable_count(), 1);
    }

    #[test]
    fn internal_and_system_disks_can_never_be_selected() {
        let mut app = ImagerApp::new();
        let mut doc = AutoCommit::new();
        let mut targets = crate::model::sample_targets();
        targets[1].is_system = true; // the internal NVMe is the system disk
        ImagerDoc::seed(&mut doc, &targets);
        ImagerDoc::set_source(&mut doc, Some(&crate::model::sample_source()));
        app.sync(&mut doc, &automerge::ROOT);

        app.dispatch_hit(&mut doc, Hit::Target(1));
        assert_eq!(app.selected, None, "the system disk is never selectable");
        assert!(
            ImagerDoc::status(&doc)
                .to_lowercase()
                .contains("system disk")
        );
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
        ImagerDoc::set_step(&mut doc, Step::Customize);
        app.sync(&mut doc, &automerge::ROOT); // reads the profile + step back
        assert_eq!(
            app.step,
            Step::Customize,
            "the panel only handles events on this step"
        );
        assert!(app.customizable(), "the seeded qOS image is customizable");
        app.customize.focus_for_test(field);
        (app, doc)
    }

    #[test]
    fn typing_into_hostname_persists_to_profile_through_the_panel() {
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

    #[test]
    fn keyboard_chooser_filters_and_commits_through_the_real_event_path() {
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

    #[test]
    fn preview_shows_the_embedded_profile() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 1);
        app.export_config(&mut doc);
        let text = app.export_dialog.text();
        assert!(text.contains("\"schema\": 1"));
        assert!(text.contains("\"hostname\""));
        assert!(!text.contains("password"));
        assert!(app.export_dialog.is_open());
    }

    #[test]
    fn write_another_resets_to_source() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0);
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

    #[test]
    fn failed_write_keeps_its_cause_and_returns_to_a_fresh_review() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(&mut doc, 0);
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
            error: Some("Could not write the drive: the selected drive was disconnected.".into()),
        })
        .unwrap();

        app.drain_job(&mut doc);
        assert_eq!(app.step, Step::Done);
        assert_eq!(app.progress.phase, JobPhase::Failed);
        assert!(
            ImagerDoc::status(&doc).contains("disconnected"),
            "the privileged writer's actionable cause reaches the UI"
        );
        let tree = app.accessibility_tree();
        let result =
            accessible_named(&tree, "Write failure details").expect("accessible terminal result");
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

    #[test]
    fn failure_detail_is_measured_bounded_and_visible_at_compact_width() {
        let mut renderer = TextRenderer::new();
        let detail = "Could not start the authorized writer because the selected removable drive disappeared while the operating system was preparing it. Reconnect it and try again.";
        let max_width = 280.0;
        let lines = ImagerApp::failure_detail_lines(&mut renderer, detail, max_width, 2);

        assert_eq!(
            lines.len(),
            2,
            "compact failure copy uses both visible lines"
        );
        assert!(
            lines.iter().all(|line| {
                renderer
                    .measure_weighted(line, 13.5, None, TextWeight::Regular)
                    .0
                    <= max_width
            }),
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
