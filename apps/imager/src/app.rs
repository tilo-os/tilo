//! Root widget for the Disk Imager (imager) app.
//!
//! `ImagerApp` holds the state of a guided **wizard** — Image → Drive →
//! Customize → Confirm → Write → Done — for creating a verified USB or SD
//! installation medium, and composes each step from toolkit widgets in the
//! shared [`page`](crate::page) frame:
//!
//! * **Image** — pick a local `.iso` / `.img` through the host XDG portal
//!   (never a bespoke picker, per HIG §5), drop one on the drop zone, or
//!   choose a known Tilo release.
//! * **Drive** — choose external media from the *real* block devices (`lsblk`);
//!   internal and system disks are never eligible. Installation onto those is
//!   handled after boot by the live Tilo installer.
//! * **Customize** (Tilo only) — the guided-customization form shared by the
//!   write and install flows ([`crate::customize`]); a plain ISO skips it.
//! * **Confirm** — a read-only review of every choice, the optional network
//!   and identity setup, the erase warning and the typed device-name gate.
//! * **Write** — a real, byte-for-byte streaming copy with live progress and a
//!   read-back **verify** pass.
//! * **Done** — the verified / failed outcome with a fresh-start affordance.
//!
//! Every control is a toolkit widget that owns its hit-testing, focus, states
//! and accessibility; a control reports what the person asked for as a
//! [`Command`], which the wizard applies with the document in hand. When what
//! the page shows changes, the page is composed again.
//!
//! Every write is doubly gated: the typed confirmation at Confirm, then native
//! host authentication for a short-lived privileged writer. The wizard itself
//! stays unprivileged; tests and shots cannot start the helper.
//!
//! Reviewed sources and non-secret install preferences are collaborative
//! Automerge resources. Drive selection, typed confirmation, progress and
//! worker channels are local capabilities: another peer can inspect a plan,
//! but never select/arm/write this machine's device.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::icons::Icon;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{ParagraphAlign, TextRenderer};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Banner, Button, ButtonRole, Column, Container, ControlSize, DropZone, EmptyState, Field,
    ListRow, ProgressRing, ProgressRingSize, Property, PropertyList, Row, Section, Spacer, Text,
    TextInput, Tone, Widget,
};

use crate::customize::CzField;
use crate::doc::ImagerDoc;
use crate::model::{
    Action, InstallProfile, JobPhase, Progress, Source, SourceKind, Step, TargetDrive, TiloRelease,
    fmt_eta, fmt_rate, fmt_size,
};
use crate::page::{self, Frame};
use crate::write::{JobControl, WriteRequest, spawn_job};
use crate::write_setup::SetupField;

const DROP_TITLE: &str = "Choose a disk image";
const DROP_MESSAGE: &str = "Drop an ISO, IMG or RAW file here";
const DROP_ACTION: &str = "Browse…";
const EMPTY_DRIVES: &str = "Insert a USB drive or SD card";
const NO_RELEASES: &str = "No Tilo releases are stored on this computer.";
const PLAIN_BODY: &str =
    "Setup options are only available for Tilo images. This image is written unchanged.";

/// What a control on the wizard asked for. Controls only record the
/// request; the wizard applies it with the document in hand.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Command {
    PickFile,
    Dropped(PathBuf),
    Release(usize),
    CancelDownload,
    Recent(usize),
    Target(usize),
    Rescan,
    Back,
    Next,
    StartWrite,
    CancelWrite,
    WriteAnother,
    /// The typed device-name confirmation changed.
    Confirm(String),
    /// A guided-customization field changed (the value as the form shows it).
    Profile(CzField, String),
    /// A network or identity field on Confirm changed.
    WriteSetup(SetupField, String),
    /// Enrolling the new device as one of yours was switched.
    Enroll(bool),
}

/// Where the controls of the composed page leave their requests.
pub(crate) type Commands = Rc<RefCell<Vec<Command>>>;

/// A button that records `command` when activated.
pub(crate) fn command_button(
    commands: &Commands,
    id: &str,
    label: &str,
    command: Command,
) -> Button {
    let sink = Rc::clone(commands);
    Button::new(id, label, None).on_effect(move || sink.borrow_mut().push(command.clone()))
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
    /// Peer-local destructive-write arming text. This must never replicate:
    /// a collaborator may review the plan, but cannot type consent for us.
    confirm_text: String,
    /// The latest outcome the person should know about, and whether it is a
    /// problem they have to act on (only those are shown on the page).
    status: String,
    status_is_error: bool,
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
    /// Makes the destructive action look armed in an offscreen documentation
    /// frame without enabling (or bypassing) the real write backend.
    shot_confirmation_ready: bool,
    /// Opens the time-zone chooser in the posed Customize frame.
    shot_timezone_open: bool,
    /// The control a posed frame shows focused (and scrolled into view).
    shot_focus: Option<&'static str>,
    /// False until the first live sync has reconciled any persisted active
    /// phase with this process's necessarily empty worker channels.
    startup_write_state_reconciled: bool,
    // ---- composition ----
    seeded: bool,
    rect: Rect,
    /// The page the controls below were composed for.
    composed: Option<Box<dyn Widget>>,
    /// What the composed page shows; a change composes the page again.
    view_key: Option<String>,
    structure_dirty: bool,
    commands: Commands,
    /// The step body's scroll offset, kept while the page is composed again
    /// and reset on every step change.
    scroll: Rc<Cell<(f32, f32)>>,
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
        Self {
            source: None,
            recent: Vec::new(),
            targets: Vec::new(),
            releases: Vec::new(),
            selected: None,
            action: Action::Flash,
            profile: InstallProfile::default(),
            wifi_psk: String::new(),
            progress: Progress::default(),
            confirm_text: String::new(),
            status: String::new(),
            status_is_error: false,
            step: Step::Source,
            job_rx: None,
            job_control: None,
            portal_rx: None,
            release_rx: None,
            release_control: None,
            release_download_index: None,
            release_download_progress: None,
            release_download_stage: None,
            shot_confirmation_ready: false,
            shot_timezone_open: false,
            shot_focus: None,
            startup_write_state_reconciled: false,
            seeded: false,
            rect: Rect::ZERO,
            composed: None,
            view_key: None,
            structure_dirty: true,
            commands: Rc::new(RefCell::new(Vec::new())),
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            actions: crate::resource::ActionSink::default(),
        }
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

    /// Can the flow move forward from `from`?
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
        }
        self.refresh_view();
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
        self.confirm_text = value.to_string();
        self.shot_confirmation_ready = true;
        // The posed review shows the consent the write waits for.
        self.shot_focus = Some("imager.confirm");
    }

    /// Headless-shot only: expose the long, searchable time-zone chooser so
    /// its overlay placement is part of visual review.
    pub fn shot_open_customize_timezone(&mut self) {
        self.shot_timezone_open = true;
        // The open chooser's field is scrolled into view with it.
        self.shot_focus = Some("imager.profile.timezone");
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
            "customize" | "customize-timezone-open" => {
                self.selected = Some(0);
                self.step = Step::Customize;
            }
            "confirm" => {
                self.selected = Some(0);
                self.step = Step::Confirm;
                self.wifi_psk = "correct horse battery".into();
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
            "drive-empty" | "drive-internal" => {
                // The Drive step at rest: nothing selected yet.
                self.step = Step::Drive;
            }
            _ => {}
        }
    }

    /// Local demo inventory/catalogue used by `--demo`; the collaborative
    /// document still carries only the reviewed source/profile records.
    pub fn seed_local_demo_state(&mut self) {
        self.seed_local_shot_state("source");
    }

    fn confirm_matches(&self) -> bool {
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        let confirm = self.confirm_text.as_str();
        !confirm.is_empty() && (confirm == want || confirm == format!("/dev/{want}"))
    }

    fn image_fits(&self) -> bool {
        match (&self.source, self.target()) {
            (Some(s), Some(t)) => t.fits(s.size),
            _ => false,
        }
    }
}

/// Whether this process should enumerate real block devices. The live binary
/// sets `IMAGER_LIVE=1`; `--shots`, `--demo` and tests leave it unset so the
/// document's seeded/posed state is authoritative and no `lsblk` runs.
fn live_probe_enabled() -> bool {
    std::env::var("IMAGER_LIVE").is_ok()
}

/// The step titles the stepper shows.
fn stepper_labels() -> [&'static str; 6] {
    Step::all().map(|step| step.title())
}

// =========================================================================
// Composition
// =========================================================================
impl ImagerApp {
    fn button(&self, id: &str, label: &str, command: Command) -> Button {
        command_button(&self.commands, id, label, command)
    }

    /// Everything the composed page shows that can change while the step
    /// stays: a change composes the page again. Text the person is typing
    /// into is left out, so a field keeps its caret while they type.
    fn view_key(&self) -> String {
        let profile = match self.step {
            // The form's own fields show these while they are edited.
            Step::Customize => String::new(),
            _ => format!(
                "{}|{}|{}|{}",
                self.profile.user.username,
                self.profile.hostname,
                self.profile.wifi_ssid.trim().is_empty(),
                self.profile.enroll_local_identity
            ),
        };
        let download = self
            .release_download_progress
            .map(|(done, total)| done.saturating_mul(100).checked_div(total).unwrap_or(0));
        format!(
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:.0}|{:?}|{}|{}|{}|{}|{}|{}|{}",
            self.step,
            self.source.as_ref().map(|s| (&s.name, &s.path)),
            self.recent.iter().map(|s| &s.path).collect::<Vec<_>>(),
            self.releases
                .iter()
                .map(TiloRelease::selection_id)
                .collect::<Vec<_>>(),
            self.targets
                .iter()
                .map(|t| (&t.name, &t.model, t.size))
                .collect::<Vec<_>>(),
            self.selected,
            self.release_download_index,
            self.release_download_stage,
            download,
            self.release_control
                .as_ref()
                .map(crate::release::DownloadControl::is_cancelled),
            self.progress.phase,
            self.progress.error,
            self.progress.overall() * 100.0,
            self.job_control.as_ref().map(JobControl::is_cancelled),
            self.status,
            self.status_is_error,
            self.confirm_matches(),
            self.customizable(),
            crate::write::write_available() || self.shot_confirmation_ready,
            self.shot_timezone_open,
            profile,
        )
    }

    /// Compose the page again when what it shows changed.
    fn refresh_view(&mut self) {
        let key = self.view_key();
        if self.view_key.as_deref() != Some(key.as_str()) {
            self.view_key = Some(key);
            self.structure_dirty = true;
        }
    }

    /// The station the stepper marks: every step before it is done. A
    /// verified result completes the last station too.
    fn stepper_current(&self) -> usize {
        if self.step == Step::Done && self.progress.phase == JobPhase::Success {
            Step::all().len()
        } else {
            self.step.index()
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        let body = match self.step {
            Step::Source => self.source_body(),
            Step::Drive => self.drive_body(),
            Step::Customize => self.customize_body(),
            Step::Confirm => self.confirm_body(),
            Step::Write => self.write_body(),
            Step::Done => self.done_body(),
        };
        let notice = (self.status_is_error && !self.status.is_empty())
            .then(|| Box::new(Banner::error(self.status.clone())) as Box<dyn Widget>);
        Frame::new(body)
            .stepper(&stepper_labels(), self.stepper_current())
            .offset(Rc::clone(&self.scroll))
            .notice(notice)
            .actions(self.footer())
            .build()
    }

    /// Back at the leading edge, the step's main action at the trailing one.
    fn footer(&self) -> Row {
        let mut row = Row::new().no_wrap();
        if matches!(self.step, Step::Drive | Step::Customize | Step::Confirm) {
            row = row.push(Box::new(
                self.button("imager.back", "Back", Command::Back)
                    .with_icon(Icon::ChevronLeft),
            ));
        }
        row = row.push(Box::new(Spacer::new()));
        let primary = match self.step {
            Step::Source | Step::Drive => self
                .button("imager.next", "Continue", Command::Next)
                .role(ButtonRole::Primary)
                .with_enabled(self.can_advance(self.step)),
            Step::Customize => self
                .button("imager.next", "Continue", Command::Next)
                .role(ButtonRole::Primary),
            Step::Confirm => self
                .button("imager.write", "Erase and write", Command::StartWrite)
                .role(ButtonRole::Destructive)
                .with_accessible_name(&self.target().map_or_else(
                    || "Erase and write the selected drive".to_string(),
                    |drive| format!("Erase and write {}", drive.dev_path()),
                ))
                .with_enabled(self.can_advance(Step::Confirm) || self.shot_confirmation_ready),
            Step::Write => {
                let stopping = self
                    .job_control
                    .as_ref()
                    .is_some_and(JobControl::is_cancelled);
                self.button(
                    "imager.cancel-write",
                    if stopping {
                        "Stopping…"
                    } else {
                        "Cancel write"
                    },
                    Command::CancelWrite,
                )
                .with_enabled(self.job_control.is_some() && !stopping)
            }
            Step::Done => self
                .button(
                    "imager.start-over",
                    if self.progress.phase == JobPhase::Failed {
                        "Review and retry"
                    } else {
                        "Start over"
                    },
                    Command::WriteAnother,
                )
                .role(ButtonRole::Primary),
        };
        row.push(Box::new(primary))
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

    /// The one fact under an image's name: what it is and whether it was
    /// verified.
    fn source_secondary(&self, source: &Source) -> &'static str {
        if self.source_is_official(source) {
            "Official Tilo release, verified"
        } else {
            "Local image, written unchanged"
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

    fn source_body(&self) -> Column {
        let sink = Rc::clone(&self.commands);
        let mut body = page::body("Choose an image").push(Box::new(
            DropZone::new("imager.drop", DROP_TITLE)
                .message(DROP_MESSAGE)
                .icon(Icon::Download)
                .with_action(self.button("imager.browse", DROP_ACTION, Command::PickFile))
                .on_files(move |files| {
                    if let Some(file) = files.into_iter().next() {
                        sink.borrow_mut().push(Command::Dropped(file));
                    }
                }),
        ));
        if let Some(source) = &self.source
            && !self.source_listed(source)
        {
            body = body.push(Box::new(Section::titled(
                "Selected image",
                vec![Box::new(
                    ListRow::new("imager.selected", &source.name)
                        .icon(Icon::File)
                        .secondary(self.source_secondary(source))
                        .meta(fmt_size(source.size))
                        .selected(true),
                )],
            )));
        }
        let releases: Vec<Box<dyn Widget>> = if self.releases().is_empty() {
            vec![Box::new(Text::new(NO_RELEASES).dimmed())]
        } else {
            (0..self.releases().len())
                .filter_map(|index| self.release_row(index))
                .collect()
        };
        body = body.push(Box::new(Section::titled("Tilo releases", releases)));
        let recent = self.shown_recent();
        if !recent.is_empty() {
            let rows = recent
                .into_iter()
                .filter_map(|index| {
                    let source = self.recent.get(index)?;
                    let sink = Rc::clone(&self.commands);
                    Some(Box::new(
                        ListRow::new(format!("imager.recent.{index}"), &source.name)
                            .icon(Icon::File)
                            .secondary(self.source_secondary(source))
                            .meta(fmt_size(source.size))
                            .selected(self.recent_selected(index))
                            .on_activate(move || sink.borrow_mut().push(Command::Recent(index))),
                    ) as Box<dyn Widget>)
                })
                .collect();
            body = body.push(Box::new(Section::titled("Recent", rows)));
        }
        body
    }

    /// One release: its name, where it runs, and its size; a download in
    /// flight says how far it is and can be stopped.
    fn release_row(&self, index: usize) -> Option<Box<dyn Widget>> {
        let release = self.releases().get(index)?;
        let downloading = self.release_download_index == Some(index);
        let secondary = if downloading {
            let stage = self.release_download_stage.unwrap_or("Preparing");
            match self.release_download_progress {
                Some((done, total)) if total > 0 => format!("{stage}, {}%", done * 100 / total),
                _ => stage.to_string(),
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
        let sink = Rc::clone(&self.commands);
        let command = if downloading {
            Command::CancelDownload
        } else {
            Command::Release(index)
        };
        let mut row = ListRow::new(
            format!("imager.release.{}", release.selection_id()),
            release.display_name(),
        )
        .icon(if release.local_path.is_empty() {
            Icon::CloudDownload
        } else {
            Icon::File
        })
        .secondary(secondary)
        .meta(fmt_size(release.size))
        .selected(self.release_selected(index))
        .on_activate(move || sink.borrow_mut().push(command.clone()));
        if downloading {
            let sink = Rc::clone(&self.commands);
            row = row.action(Icon::X, "Cancel download", move || {
                sink.borrow_mut().push(Command::CancelDownload);
            });
        }
        Some(Box::new(row))
    }

    // ---- Drive -------------------------------------------------------------

    fn drive_body(&self) -> Column {
        let header = Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(Box::new(page::title("Choose a drive"))),
            ))
            .push(Box::new(
                self.button("imager.rescan", "Rescan", Command::Rescan)
                    .with_icon(Icon::Refresh)
                    .control_size(ControlSize::Small)
                    .with_accessible_name("Rescan drives"),
            ));
        let body = page::body_with(Box::new(header));
        let visible = self.visible_targets();
        if visible.is_empty() {
            return body.push(Box::new(
                EmptyState::new(EMPTY_DRIVES).with_icon(Icon::HardDrive),
            ));
        }
        let rows = visible
            .into_iter()
            .map(|(index, drive)| {
                let sink = Rc::clone(&self.commands);
                Box::new(
                    ListRow::new(format!("imager.drive.{}", drive.name), &drive.model)
                        .icon(Icon::HardDrive)
                        .secondary(format!(
                            "{}, {}",
                            drive.dev_path(),
                            drive.contents_summary()
                        ))
                        .meta(fmt_size(drive.size))
                        .selected(self.selected == Some(index))
                        .on_activate(move || sink.borrow_mut().push(Command::Target(index))),
                ) as Box<dyn Widget>
            })
            .collect();
        body.push(Box::new(Section::new(rows)))
    }

    // ---- Customize ---------------------------------------------------------

    fn customize_body(&self) -> Column {
        let body = page::body("Prepare the setup");
        if !self.customizable() {
            return body.push(Box::new(Text::new(PLAIN_BODY).dimmed()));
        }
        body.push(page::form_block(crate::customize::form(
            &self.profile,
            &self.commands,
            self.shot_timezone_open,
        )))
    }

    // ---- Confirm -----------------------------------------------------------

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
        if self.customizable() {
            format!(
                "Pre-configured for {} on {}",
                self.profile.user.username, self.profile.hostname
            )
        } else {
            "Written unchanged, then verified".to_string()
        }
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

    /// The help under the confirmation field. Shot mode poses the field as
    /// ready for documentation, but never enables the backend.
    fn confirm_help(&self) -> &'static str {
        if !(crate::write::write_available() || self.shot_confirmation_ready) {
            "This computer can't write drives yet."
        } else if self.confirm_matches() {
            "The name matches. Erase and write when you are ready."
        } else {
            "Type the device name exactly as it is shown in the review."
        }
    }

    fn confirm_body(&self) -> Column {
        let title = match self.action {
            Action::Install => "Review and install",
            Action::Flash => "Review and write",
        };
        let review = self
            .review_rows()
            .into_iter()
            .map(|(label, value)| Property::new(label, value))
            .collect();
        let mut body = page::body(title).push(Box::new(PropertyList::new(review)));
        if self.offers_write_setup() {
            body = body.push(page::form_block(crate::write_setup::form(
                &self.profile,
                &self.wifi_psk,
                &self.commands,
            )));
        }
        let want = self.target().map(|d| d.name.clone()).unwrap_or_default();
        let (warning, consequence) = self.confirm_warning();
        let sink = Rc::clone(&self.commands);
        let input = TextInput::local()
            .with_id("imager.confirm")
            .with_kind(ui::widget::TextInputKind::Path)
            .with_initial_value(&self.confirm_text)
            .with_placeholder(&want)
            .on_change(move |value| sink.borrow_mut().push(Command::Confirm(value.to_string())));
        let field = Field::new(format!("Type {want} to confirm"), input);
        let field = if !self.confirm_text.is_empty() && !self.confirm_matches() {
            field.with_error(format!("This is not {want}."))
        } else {
            field.with_help(self.confirm_help())
        };
        body.push(Box::new(
            page::related()
                .push(Box::new(Banner::error(warning).with_message(consequence)))
                .push(Box::new(field)),
        ))
    }

    // ---- Write -------------------------------------------------------------

    /// The active phase, its share done, and what it is called.
    fn phase(&self) -> (f64, &'static str) {
        match (self.progress.phase, self.action) {
            (JobPhase::Verifying, _) => (self.progress.verify_frac, "Verifying"),
            (_, Action::Install) => (self.progress.write_frac, "Installing"),
            (_, Action::Flash) => (self.progress.write_frac, "Writing"),
        }
    }

    fn centred(text: &str, role: TextRole) -> Text {
        Text::new(text)
            .with_role(role)
            .with_align(ParagraphAlign::Center)
    }

    fn ring_row(ring: ProgressRing) -> Row {
        Row::new()
            .with_justify(taffy::JustifyContent::Center)
            .push(Box::new(ring))
    }

    fn write_body(&self) -> Column {
        let (frac, caption) = self.phase();
        let ring = ProgressRing::new(frac)
            .value(format!("{:.0}%", frac * 100.0))
            .caption(caption)
            .size(ProgressRingSize::Large);
        let mut meta = Vec::new();
        if self.progress.rate_bps > 0 {
            meta.push(fmt_rate(self.progress.rate_bps));
        }
        if let Some(src) = &self.source
            && let Some(eta) = self.progress.eta_secs(src.size)
        {
            meta.push(format!("about {} left", fmt_eta(eta)));
        }
        let next = if self.progress.phase == JobPhase::Verifying {
            "Reading the drive back to check every byte."
        } else {
            "Then the drive is read back and checked."
        };
        let mut lines = page::related();
        if !meta.is_empty() {
            lines = lines.push(Box::new(
                Self::centred(&meta.join(", "), TextRole::Body).numeric(),
            ));
        }
        lines = lines.push(Box::new(Self::centred(next, TextRole::Body).dimmed()));
        let mut body = page::stack()
            .with_gap(crate::rhythm::px::GROUP)
            .push(Box::new(Self::ring_row(ring)))
            .push(Box::new(lines));
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            body = body.push(Box::new(
                Self::centred(
                    &format!(
                        "{} to {}. Keep the drive connected until this finishes.",
                        src.name, tgt.model
                    ),
                    TextRole::Caption,
                )
                .dimmed(),
            ));
        }
        body
    }

    // ---- Done --------------------------------------------------------------

    /// The success hero's headline and explanation.
    fn done_ok_copy(&self) -> (&'static str, String) {
        let image = self
            .source
            .as_ref()
            .map_or("The image", |s| s.name.as_str());
        let drive = self.target().map_or("the drive", |d| d.model.as_str());
        match self.action {
            Action::Install => (
                "Tilo is installed",
                format!(
                    "{image} was written to {drive} and verified. Remove the installation media and restart."
                ),
            ),
            Action::Flash => (
                "The drive is ready",
                format!(
                    "{image} was written to {drive} and verified. You can safely remove the drive."
                ),
            ),
        }
    }

    fn done_body(&self) -> Column {
        let ok = self.progress.phase == JobPhase::Success;
        let (ring, headline, detail) = if ok {
            let (headline, detail) = self.done_ok_copy();
            (
                ProgressRing::new(1.0)
                    .value("100%")
                    .caption("Verified")
                    .size(ProgressRingSize::Large)
                    .tone(Tone::Success),
                headline,
                detail,
            )
        } else {
            let frac = self.progress.write_frac;
            (
                ProgressRing::new(frac)
                    .value(format!("{:.0}%", frac * 100.0))
                    .caption("Stopped")
                    .size(ProgressRingSize::Large)
                    .tone(Tone::Danger),
                if self.action == Action::Install {
                    "The installation failed"
                } else {
                    "Writing failed"
                },
                self.progress.error.clone().unwrap_or_else(|| {
                    "The drive may be incomplete. Review the plan before trying again.".into()
                }),
            )
        };
        page::stack()
            .with_gap(crate::rhythm::px::GROUP)
            .push(Box::new(Self::ring_row(ring)))
            .push(Box::new(
                page::related()
                    .push(Box::new(
                        Self::centred(headline, TextRole::Title).with_heading_level(1),
                    ))
                    .push(Box::new(
                        Self::centred(&detail, TextRole::Body)
                            .dimmed()
                            .with_id("imager.result")
                            .with_live(if ok {
                                ui::a11y::Politeness::Polite
                            } else {
                                ui::a11y::Politeness::Assertive
                            }),
                    )),
            ))
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
    }

    fn open_file_picker(&mut self, _state: &AutoCommit) {
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
                None => self.fail("That file is not a readable disk image."),
            },
            PortalOutcome::Cancelled => self.set_status(state, "File selection cancelled."),
            PortalOutcome::Failed(reason) => {
                self.fail(&format!("File picker unavailable: {reason}"));
            }
        }
    }

    fn download_release(&mut self, state: &AutoCommit, index: usize, release: TiloRelease) {
        if self.release_rx.is_some() {
            self.fail("A Tilo installer download is already in progress.");
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
                        Err(error) => self.fail(&error),
                    }
                }
            }
        }
    }

    fn select_target(&mut self, _state: &AutoCommit, idx: usize) {
        let Some(drive) = self.targets.get(idx) else {
            return;
        };
        // Hard guard repeated below the visible-list filter: stale persisted or
        // collaborative state cannot turn the media creator into an internal-
        // disk raw writer.
        if !drive.is_media_target() {
            self.fail(
                "Internal and system disks can only be installed from the booted Tilo medium.",
            );
            return;
        }
        self.selected = Some(idx);
        self.confirm_text.clear();
    }

    /// Record what just happened. The agent-facing status reports it; the
    /// page shows only problems, through [`Self::fail`].
    fn set_status(&mut self, _state: &AutoCommit, s: &str) {
        self.status = s.to_string();
        self.status_is_error = false;
    }

    /// Record a problem the person has to act on; the page shows it above
    /// its actions until the next outcome replaces it.
    fn fail(&mut self, s: &str) {
        self.status = s.to_string();
        self.status_is_error = true;
    }

    fn goto(&mut self, _state: &mut AutoCommit, s: Step) {
        self.step = s;
        self.scroll.set((0.0, 0.0));
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
                Err(message) => self.fail(&message),
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
            confirmation: self.confirm_text.clone(),
        };
        if let Err(reason) = self.perform_operation(&operation, state) {
            self.fail(&reason);
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
            self.confirm_text.clear();
            self.goto(state, Step::Confirm);
            self.set_status(
                state,
                "Review the image and drive, then confirm the target again to retry.",
            );
            return;
        }
        self.progress = Progress::default();
        self.confirm_text.clear();
        self.goto(state, Step::Source);
        self.set_status(state, "");
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
            self.confirm_text.clear();
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
                self.fail("Drive rescan failed. Check the system disk tools and try again.");
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
        true
    }

    /// Apply what a control on the page asked for.
    fn apply_command(&mut self, state: &mut AutoCommit, command: Command) {
        match command {
            Command::PickFile => self.open_file_picker(state),
            Command::Dropped(path) => match crate::probe::inspect_source(&path) {
                Some(src) => self.select_source(state, &src),
                None => self.fail("That file is not a readable disk image."),
            },
            Command::Release(i) => {
                if self.release_rx.is_some() {
                    return;
                }
                if let Some(release) = self.releases.get(i).cloned() {
                    if release.url.is_empty() {
                        self.select_source(state, &release.to_source());
                    } else {
                        self.download_release(state, i, release);
                    }
                }
            }
            Command::CancelDownload => {
                if let Some(control) = self.release_control.clone() {
                    control.cancel();
                    self.release_download_stage = Some("Cancelling");
                    self.set_status(
                        state,
                        "Cancelling safely… The verified partial download will be kept for resume.",
                    );
                }
            }
            Command::Recent(i) => {
                if let Some(src) = self.recent.get(i).cloned() {
                    self.select_source(state, &src);
                }
            }
            Command::Target(i) => self.select_target(state, i),
            Command::Rescan => {
                self.do_rescan(state);
            }
            Command::Back => self.back(state),
            Command::Next => self.advance(state),
            Command::StartWrite => self.start_write(state),
            Command::CancelWrite => {
                let _ = self.perform_operation(&crate::actions::ImagerOperation::Cancel, state);
            }
            Command::WriteAnother => self.write_another(state),
            Command::Confirm(value) => {
                if self.step == Step::Confirm {
                    self.confirm_text = value;
                }
            }
            Command::Profile(field, shown) => {
                if self.step != Step::Customize || !self.customizable() {
                    return;
                }
                match crate::customize::profile_value(field, &shown) {
                    Some(value) => {
                        self.dispatch(state, &crate::customize::field_action(field, value));
                    }
                    None => self.fail(&format!("Choose a listed {}", field.label())),
                }
            }
            Command::WriteSetup(field, value) => {
                if self.step != Step::Confirm || !self.offers_write_setup() {
                    return;
                }
                match crate::write_setup::field_action(field, value.clone()) {
                    Some(action) => self.dispatch(state, &action),
                    None => self.wifi_psk = value,
                }
            }
            Command::Enroll(on) => {
                if self.step == Step::Confirm && self.offers_write_setup() {
                    self.dispatch(
                        state,
                        &crate::actions::ImagerAction::SetEnrollLocalIdentity { on },
                    );
                }
            }
        }
    }

    /// Dispatch one profile action through the framework's action host, so it
    /// is one undo step and replays byte-identically on peers.
    fn dispatch(&mut self, state: &mut AutoCommit, action: &crate::actions::ImagerAction) {
        self.actions.dispatch_with_context(
            state,
            crate::actions::IMAGER_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.imager", "Imager", action),
        );
        self.profile = ImagerDoc::profile(state);
    }

    /// Apply every request the page's controls left since the last call.
    /// Returns whether there were any.
    fn drain_commands(&mut self, state: &mut AutoCommit) -> bool {
        let commands = std::mem::take(&mut *self.commands.borrow_mut());
        let any = !commands.is_empty();
        for command in commands {
            self.apply_command(state, command);
        }
        if any {
            self.refresh_view();
        }
        any
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
                Err(error) => self.fail(&error),
            }
        }

        // Fold in any async work and any request a control left before
        // reading the doc back.
        self.drain_commands(state);
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
        self.refresh_view();
        if let Some(composed) = &mut self.composed {
            composed.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        let (doc, _) = ui::doc::document(state, path);
        let mut changed = self.drain_commands(doc);
        if let Some(composed) = &mut self.composed {
            changed |= composed.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // A page composed again keeps keyboard focus on the same control.
        let focused = self
            .composed
            .as_deref()
            .and_then(page::focused_id)
            .or_else(|| self.shot_focus.take().map(str::to_string));
        let mut composed = self.build_composition();
        if let Some(id) = focused {
            page::refocus(composed.as_mut(), &id);
        }
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        self.view_key = Some(self.view_key());
        self.structure_dirty = false;
        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    flex_grow: 1.0,
                    ..Default::default()
                },
                &[child],
            )
            .unwrap_or(child);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node)
            && let Some(&child) = children.first()
        {
            composed.apply_layout(taffy, child, Point::new(self.rect.x0, self.rect.y0));
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The framework paints the window ground; the page is toolkit widgets.
        if let Some(composed) = &mut self.composed {
            composed.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if let Some(composed) = &mut self.composed
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&child) = children.first()
        {
            composed.handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
        self.drain_commands(ctx.state);
        if !*ctx.handled
            && let WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key,
                        state: winit::event::ElementState::Pressed,
                        ..
                    },
                ..
            } = event
        {
            self.handle_global_shortcut_key(logical_key, ctx.state, ctx.handled);
            self.refresh_view();
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
        let answered = self
            .answer_resource_operation_v2(
                request.kind,
                request.type_id,
                request.payload,
                doc,
                path,
            )?
            .map(ui::widget::ResourceOperationStart::Completed);
        self.refresh_view();
        Ok(answered)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|child| child.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|child| child.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &'static str {
        "ImagerApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty)
            || self
                .composed
                .as_mut()
                .is_some_and(|child| child.take_structure_dirty())
    }

    /// Keep the event loop ticking while a job or file picker is in flight, so
    /// the progress shows and the portal result is drained promptly.
    fn needs_animation_frame(&self) -> bool {
        self.job_rx.is_some()
            || self.portal_rx.is_some()
            || self.release_rx.is_some()
            || self
                .composed
                .as_ref()
                .is_some_and(|child| child.needs_animation_frame())
    }

    fn needs_idle_sync(&self) -> bool {
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
    /// Keys that act on the whole wizard when no control took them: Enter
    /// moves on, Escape stops a running write, R rescans on the Drive step.
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
            Key::Named(NamedKey::Escape)
                if self.step == Step::Write && self.job_control.is_some() =>
            {
                let _ = self.perform_operation(&crate::actions::ImagerOperation::Cancel, state);
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

#[cfg(test)]
mod tests {
    use super::*;
    use ui::a11y::Action as AccessAction;
    use ui::accesskit;
    use ui::headless::{FrameHarness, SemanticExpectation};

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

    /// The wizard in a real frame: layout, focus, pointer, keys and
    /// assistive actions all take the host's own routes.
    fn harness(app: ImagerApp, doc: AutoCommit, viewport: (u32, u32)) -> FrameHarness {
        let mut state = ui::State::new();
        state.doc = doc;
        FrameHarness::for_root(state, app, viewport, Theme::dark())
    }

    /// The posed `--shots` frame of `scenario`.
    fn scenario(name: &str, viewport: (u32, u32)) -> FrameHarness {
        let mut state = ui::State::new();
        crate::seed_scenario(&mut state.doc, name);
        FrameHarness::new(state, crate::app_root_for_shot(name), viewport)
    }

    /// Every control id the page composed for `app`'s current state.
    fn ids(app: &ImagerApp) -> Vec<String> {
        fn walk(widget: &dyn Widget, out: &mut Vec<String>) {
            if let Some(id) = widget.test_id() {
                out.push(id.to_string());
            }
            for child in widget.children() {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        walk(app.build_composition().as_ref(), &mut out);
        out
    }

    fn labelled(h: &FrameHarness, label: &str) -> Option<accesskit::Node> {
        h.a11y_nodes()
            .into_iter()
            .map(|(_, node)| node)
            .find(|node| node.label() == Some(label))
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
            let _ = &mut doc;
        }

        // Phase `write_operations_are_blocked_until_startup_state_is_reconciled`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.startup_write_state_reconciled = false;
            app.confirm_text = "sdb".into();

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
            assert!(
                !ids(&app).iter().any(|id| id.starts_with("imager.release.")),
                "no release row is offered on the live path"
            );
            assert!(
                app.source.is_none(),
                "and nothing is pre-selected as the source"
            );
        }

        // Phase `demo_document_offers_its_seeded_releases`.
        // The gate is a gate, not a deletion: the demo document still offers them.
        {
            let (app, _doc) = seeded_app();
            assert_eq!(app.releases().len(), 2);
            let offered = ids(&app)
                .iter()
                .filter(|id| id.starts_with("imager.release."))
                .count();
            assert_eq!(offered, 2, "each seeded release has a row to choose");
        }
    }

    /// Accessible semantics, selection labels, live regions and tab order.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wizard_semantics_labels_live_regions_and_keyboard_order() {
        // Phase `the_page_exposes_its_heading_stepper_and_keyboard_order`.
        {
            let (app, doc) = seeded_app();
            let mut h = harness(app, doc, (1180, 820));
            let heading = labelled(&h, "Choose an image").expect("step heading");
            assert_eq!(heading.role(), accesskit::Role::Heading);
            let stepper = labelled(&h, "Source").expect("stepper");
            assert_eq!(stepper.role(), accesskit::Role::ProgressIndicator);

            let mut order = Vec::new();
            for _ in 0..12 {
                h.tab(false);
                if let Some(node) = h.focused_access_node() {
                    order.push(node.name.unwrap_or_default());
                }
            }
            let browse = order
                .iter()
                .position(|name| name == "Browse…")
                .expect("browse in focus order");
            let continue_button = order
                .iter()
                .position(|name| name == "Continue")
                .expect("continue in focus order");
            assert!(browse < continue_button);
        }

        // Phase `hardware_variants_have_distinct_rows_and_review_labels`.
        {
            let (mut app, doc) = seeded_app();
            let mut generic = crate::model::sample_releases().remove(0);
            generic.architecture = "aarch64".into();
            let mut board = generic.clone();
            board.hardware_profile = "lenovo-83ed".into();
            assert_ne!(generic.selection_id(), board.selection_id());
            app.releases = vec![generic, board.clone()];
            let mut h = harness(app, doc, (1180, 820));
            h.perform_named(
                &format!("imager.release.{}", board.selection_id()),
                &AccessAction::Activate,
            )
            .unwrap();
            let app = h.root::<ImagerApp>();
            assert!(board.matches_source(app.source.as_ref().unwrap()));
            assert!(!app.releases[0].matches_source(app.source.as_ref().unwrap()));
            let image = app
                .review_rows()
                .into_iter()
                .find(|(label, _)| *label == "Image")
                .unwrap()
                .1;
            assert!(image.contains("lenovo-83ed only"));
        }

        // Phase `profile_and_confirmation_controls_are_editable`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Customize);
            let mut h = harness(app, doc, (1180, 820));
            for (id, role) in [
                ("imager.profile.hostname", ui::a11y::Role::TextInput),
                ("imager.profile.locale", ui::a11y::Role::ComboBox),
                ("imager.profile.timezone", ui::a11y::Role::ComboBox),
                ("imager.profile.keyboard", ui::a11y::Role::ComboBox),
                ("imager.profile.filesystem", ui::a11y::Role::ComboBox),
            ] {
                h.expect(&SemanticExpectation::named(id).role(role))
                    .unwrap_or_else(|error| panic!("{id}: {error}"));
            }
            h.perform_named(
                "imager.profile.hostname",
                &AccessAction::SetValue("accessible-host".into()),
            )
            .unwrap();
            assert_eq!(
                ImagerDoc::profile(&h.state().doc).hostname,
                "accessible-host"
            );
        }

        // Phase `choices_take_only_listed_values`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Customize);
            for (field, chosen, expected) in [
                (CzField::Locale, "de_DE.UTF-8", "de_DE.UTF-8"),
                (CzField::Timezone, "Europe/Berlin", "Europe/Berlin"),
                (CzField::Keyboard, "German (QWERTZ)", "de"),
            ] {
                app.apply_command(&mut doc, Command::Profile(field, chosen.into()));
                let profile = ImagerDoc::profile(&doc);
                let actual = match field {
                    CzField::Locale => profile.locale,
                    CzField::Timezone => profile.timezone,
                    _ => profile.keyboard,
                };
                assert_eq!(actual, expected);
            }
            let timezone_before = ImagerDoc::profile(&doc).timezone;
            app.apply_command(
                &mut doc,
                Command::Profile(CzField::Timezone, "Europe/Not_A_Real_Zone".into()),
            );
            assert_eq!(ImagerDoc::profile(&doc).timezone, timezone_before);
            assert!(app.status.contains("Choose a listed Time zone"));
            assert!(app.status_is_error);
        }

        // Phase `the_confirmation_field_arms_the_write`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Confirm);
            let mut h = harness(app, doc, (1180, 820));
            h.perform_named("imager.confirm", &AccessAction::SetValue("sdb".into()))
                .unwrap();
            assert!(h.root::<ImagerApp>().confirm_matches());
        }

        // Phase `write_progress_is_a_live_progress_indicator`.
        {
            let (mut app, doc) = seeded_app();
            app.selected = Some(0);
            app.step = Step::Write;
            app.progress = Progress {
                phase: JobPhase::Verifying,
                write_frac: 1.0,
                verify_frac: 0.4,
                rate_bps: 1,
                error: None,
            };
            let h = harness(app, doc, (1180, 820));
            let ring = labelled(&h, "Verifying").expect("progress ring");
            assert_eq!(ring.role(), accesskit::Role::ProgressIndicator);
            assert_eq!(ring.value(), Some("40%"));
        }

        // Phase `tab_reaches_every_profile_control_and_both_footer_actions`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Customize);
            let mut h = harness(app, doc, (320, 480));
            let mut order = Vec::new();
            for _ in 0..16 {
                h.tab(false);
                if let Some(node) = h.focused_access_node() {
                    order.push(node.name.unwrap_or_default());
                }
            }
            for name in [
                "Hostname",
                "Full name",
                "Username",
                "Time zone",
                "Language and locale",
                "Keyboard layout",
                "Filesystem",
                "Back",
                "Continue",
            ] {
                assert!(
                    order.iter().any(|candidate| candidate == name),
                    "{name} is reachable by Tab: {order:?}"
                );
            }
        }
    }

    /// Every step at a wide, a narrow and a phone-sized window: the stepper
    /// on top, the actions at the bottom, nothing outside the window and the
    /// two actions apart.
    #[test]
    fn every_step_stays_inside_the_window_at_every_width() {
        for viewport in [(1280, 800), (800, 600), (320, 640)] {
            for name in [
                "source",
                "drive",
                "drive-empty",
                "customize",
                "confirm",
                "progress",
                "done",
                "done-failed",
            ] {
                let h = scenario(name, viewport);
                let width = f64::from(viewport.0);
                let mut back = None;
                let mut primary = None;
                let mut stepper = None;
                for (_, node) in h.a11y_nodes() {
                    let Some(bounds) = node.bounds() else {
                        continue;
                    };
                    if node.role() == accesskit::Role::Window {
                        continue;
                    }
                    assert!(
                        bounds.x0 >= -0.5 && bounds.x1 <= width + 0.5,
                        "{name} at {viewport:?}: {:?} {:?} leaves the window: {bounds:?}",
                        node.role(),
                        node.label()
                    );
                    match (node.role(), node.label()) {
                        (accesskit::Role::ProgressIndicator, _) if stepper.is_none() => {
                            stepper = Some(bounds);
                        }
                        (accesskit::Role::Button, Some("Back")) => back = Some(bounds),
                        (
                            accesskit::Role::Button,
                            Some("Continue" | "Cancel write" | "Start over" | "Review and retry"),
                        ) => primary = Some(bounds),
                        (accesskit::Role::Button, Some(label))
                            if label.starts_with("Erase and write") =>
                        {
                            primary = Some(bounds);
                        }
                        _ => {}
                    }
                }
                let stepper = stepper.unwrap_or_else(|| panic!("{name}: stepper"));
                let primary = primary.unwrap_or_else(|| panic!("{name}: primary action"));
                assert!(stepper.y1 < primary.y0, "{name}: stepper above the actions");
                if let Some(back) = back {
                    assert!(
                        back.x1 < primary.x0,
                        "{name} at {viewport:?}: actions apart"
                    );
                }
            }
        }
    }

    #[test]
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
            app.confirm_text = "sdb".into();

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
            assert!(app.status_is_error, "the page shows the problem");

            // A direct operation dispatch must repeat the gate rather than relying
            // only on wizard navigation.
            app.goto(&mut doc, Step::Confirm);
            app.confirm_text = "sdb".into();
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
            local.path = "/home/ada/Downloads/tilo-official-looking.iso".into();
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
            app.confirm_text = "sdb".into();
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
            app.confirm_text = "wrong".into();
            app.start_write(&mut doc);
            assert_eq!(app.progress.phase, JobPhase::Idle, "no job started");
            assert!(app.job_rx.is_none());
            assert_eq!(app.step, Step::Confirm, "stays on the Confirm station");
            assert!(app.status.to_lowercase().contains("confirm"));
        }
    }

    /// Release download job presentation.
    #[test]
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

        // Phase `an_active_download_can_be_stopped_and_blocks_other_releases`.
        {
            let (mut app, mut doc) = seeded_app();
            let (_tx, rx) = std::sync::mpsc::channel();
            let control = crate::release::DownloadControl::default();
            app.release_rx = Some(rx);
            app.release_control = Some(control.clone());
            app.release_download_index = Some(0);
            app.release_download_progress = Some((25, 100));
            app.release_download_stage = Some("Downloading");

            let source = app.source.clone();
            app.apply_command(&mut doc, Command::Release(1));
            assert_eq!(app.source, source, "a second release waits for the first");
            app.apply_command(&mut doc, Command::CancelDownload);
            assert!(control.is_cancelled());
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
    /// on-screen controls: walk Source → Drive → Customize → Confirm.
    #[test]
    #[cfg(not(feature = "host-media-writer"))]
    fn clicking_through_the_wizard_refuses_a_fake_write_when_backend_is_disabled() {
        let (app, doc) = seeded_app();
        let mut h = harness(app, doc, (1180, 820));
        assert_eq!(h.root::<ImagerApp>().step, Step::Source);

        h.click_named("imager.next").unwrap();
        assert_eq!(h.root::<ImagerApp>().step, Step::Drive);

        h.click_named("imager.drive.sdb").unwrap();
        assert_eq!(h.root::<ImagerApp>().selected, Some(0));

        // Drive → Customize (the seeded Tilo image is customizable).
        h.click_named("imager.next").unwrap();
        assert_eq!(h.root::<ImagerApp>().step, Step::Customize);

        h.click_named("imager.next").unwrap();
        assert_eq!(h.root::<ImagerApp>().step, Step::Confirm);

        // Arm the write: Tab to the typed-name field at the end of the review
        // (focus scrolls it into view), then type the device name.
        for _ in 0..30 {
            if h.focused_access_node()
                .and_then(|node| node.name)
                .as_deref()
                == Some("Type sdb to confirm")
            {
                break;
            }
            h.tab(false);
        }
        h.type_text("sdb");
        h.expect(&SemanticExpectation::named("imager.confirm").focused(true))
            .unwrap();
        assert!(
            h.root::<ImagerApp>().confirm_matches(),
            "typing into the field armed the write"
        );
        assert!(
            automerge::ReadDoc::get(&h.state().doc, &automerge::ROOT, "confirm")
                .unwrap()
                .is_none(),
            "destructive consent is never written to the shared document"
        );

        if crate::write::allow_write() {
            // Never point this UI test at a real block device.
            return;
        }

        // The write stays disabled: a matching phrase cannot turn an
        // unavailable backend into a synthetic success.
        h.expect(&SemanticExpectation::named("imager.write").enabled(false))
            .unwrap();
        let _ = h.click_named("imager.write");
        let app = h.root::<ImagerApp>();
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
        app.confirm_text = "sdb".into();
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

        // Phase `a_dropped_image_is_inspected_like_a_picked_one`.
        {
            let (mut app, mut doc) = seeded_app();
            let f = tempfile::Builder::new().suffix(".img").tempfile().unwrap();
            std::fs::write(f.path(), b"raw image bytes for the imager test").unwrap();
            app.apply_command(&mut doc, Command::Dropped(f.path().to_path_buf()));
            assert_eq!(
                app.source.as_ref().map(|source| source.path.clone()),
                Some(f.path().display().to_string())
            );
            app.apply_command(
                &mut doc,
                Command::Dropped(PathBuf::from("/nonexistent/not-an-image.iso")),
            );
            assert!(app.status_is_error, "an unreadable file is reported");
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
            app.confirm_text = "sdb".into();

            app.apply_drive_inventory(Vec::new());

            assert!(app.targets.is_empty());
            assert_eq!(app.selected, None);
            assert!(app.confirm_text.is_empty());
        }

        // Phase `rescan_preserves_only_the_same_physical_target`.
        {
            let (mut app, doc) = seeded_app();
            app.select_target(&doc, 0);
            app.confirm_text = "sdb".into();
            let selected = app.target().unwrap().clone();
            let mut reordered = crate::model::sample_targets();
            reordered.swap(0, 1);

            app.apply_drive_inventory(reordered);

            assert_eq!(app.selected, Some(1));
            assert_eq!(
                app.target().and_then(|drive| drive.stable_id.as_deref()),
                selected.stable_id.as_deref()
            );
            assert_eq!(app.confirm_text, "sdb");

            let mut replacement = selected;
            replacement.stable_id = Some("different-usb-stick".into());
            app.apply_drive_inventory(vec![replacement]);

            assert_eq!(
                app.selected, None,
                "a reused OS device slot is not the reviewed drive"
            );
            assert!(
                app.confirm_text.is_empty(),
                "consent never carries to replacement hardware"
            );

            let mut unidentified_before = crate::model::sample_targets()[0].clone();
            unidentified_before.stable_id = None;
            app.targets = vec![unidentified_before.clone()];
            app.selected = Some(0);
            app.confirm_text.clone_from(&unidentified_before.name);
            app.apply_drive_inventory(vec![unidentified_before]);
            assert_eq!(
                app.selected, None,
                "a drive without stable identity must be chosen again after rescan"
            );
            assert!(app.confirm_text.is_empty());
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

            app.apply_command(&mut doc, Command::Target(1));
            assert_eq!(app.selected, None, "the system disk is never selectable");
            assert!(app.status.to_lowercase().contains("system disk"));
            assert!(app.status_is_error);
        }
    }

    /// Typing into the customization form and the profile export.
    #[test]
    fn profile_edits_persist_through_the_real_event_path() {
        // Phase `typing_into_hostname_persists_to_profile`.
        {
            let (mut app, mut doc) = seeded_app();
            let mut profile = ImagerDoc::profile(&doc);
            profile.hostname.clear();
            ImagerDoc::set_profile(&mut doc, &profile);
            app.sync(&mut doc, &automerge::ROOT);
            app.select_target(&doc, 0);
            app.goto(&mut doc, Step::Customize);
            let mut h = harness(app, doc, (1280, 800));
            h.click_named("imager.profile.hostname").unwrap();
            h.type_text("lab01");
            assert_eq!(
                ImagerDoc::profile(&h.state().doc).hostname,
                "lab01",
                "persisted to the doc"
            );
            h.expect(&SemanticExpectation::named("imager.profile.hostname").focused(true))
                .unwrap();
        }

        // Phase `the_export_view_returns_the_secret_free_profile`.
        {
            let (mut app, doc) = seeded_app();
            app.select_target(&doc, 1);
            let text = app
                .profile
                .to_headless_config(app.source.as_ref(), app.target());
            assert!(text.contains("\"schema\": 1"));
            assert!(text.contains("\"hostname\""));
            assert!(!text.contains("password"));
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

            app.confirm_text = "sdb".into();
            let mut h = harness(app, doc, (1180, 820));
            h.expect(&SemanticExpectation::named("imager.result").name_contains("disconnected"))
                .unwrap();
            h.click_named("imager.start-over").unwrap();
            let app = h.root::<ImagerApp>();
            assert_eq!(app.step, Step::Confirm, "failure returns to review");
            assert!(app.source.is_some(), "the verified source is preserved");
            assert_eq!(app.selected, Some(0), "the selected drive is preserved");
            assert!(
                app.confirm_text.is_empty(),
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
            let mut h = harness(app, doc, (1180, 820));
            h.expect(
                &SemanticExpectation::named("imager.cancel-write")
                    .name("Stopping…")
                    .enabled(false),
            )
            .unwrap();
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
