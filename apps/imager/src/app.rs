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
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use automerge::ObjId;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use ui::icons::Icon;
use ui::portal::{FileFilter, PickRequest};
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
use crate::export_dialog::ExportDialog;
use crate::model::{
    Action, InstallProfile, JobPhase, Progress, Source, SourceKind, Step, TargetDrive, TiloRelease,
    fmt_eta, fmt_rate, fmt_size,
};
use crate::page::{self, Frame};
use crate::resource::{Imager, ImagerEffect};
use crate::write::{JobControl, WriteRequest, spawn_job};
use crate::write_setup::SetupField;
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectReply, EffectStatus, Effected, JobProgress, Jobs,
    LiveRow, RunMode,
};
use ui::tilo_resource::Effect as _;

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
    /// Go back to the step at this station of the stepper.
    Station(usize),
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
    Button::new(id, label).on_effect(move || sink.borrow_mut().push(command.clone()))
}

pub struct ImagerApp {
    // ---- collaborative records (cached each `sync`) ----
    source: Option<Source>,
    recent: Vec<Source>,
    profile: InstallProfile,
    /// Host-session Wi-Fi passphrase. Never enters the collaborative document.
    wifi_psk: String,
    ssh_public_key: String,
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
    job_control: Option<JobControl>,
    write_mark: Option<(u64, ui::time::Instant)>,
    write_reply: Option<EffectReply>,
    release_download: Option<ReleaseDownload>,
    jobs: Option<Jobs<Imager>>,
    export_dialog: ExportDialog,
    export_after_sync: bool,
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
    /// The page the controls below were composed for.
    composed: ui::widget::Composed,
    /// What the composed page shows; a change composes the page again.
    view_key: Option<String>,
    structure_dirty: bool,
    commands: Commands,
    /// The step body's scroll offset, kept while the page is composed again
    /// and reset on every step change.
    scroll: Rc<Cell<(f32, f32)>>,
    /// Framework-owned action dispatch (the only action-dispatch state the app
    /// holds). Every profile edit is a field edit submitted through it, so it
    /// is one Ctrl+Z step and replays on peers.
    dispatcher: Option<Dispatcher<Imager>>,
    control_actions: Option<ui::framework::Actions<Imager>>,
    live: bool,
    reaches_machine: bool,
    dirty: bool,
}

impl Default for ImagerApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ImagerApp {
    #[cfg(test)]
    pub(crate) fn sample() -> Self {
        let mut app = Self::new();
        app.targets = crate::model::sample_targets();
        app.releases = crate::model::sample_releases();
        app.startup_write_state_reconciled = true;
        app.seeded = true;
        app.reaches_machine = false;
        app
    }

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
            ssh_public_key: String::new(),
            progress: Progress::default(),
            confirm_text: String::new(),
            status: String::new(),
            status_is_error: false,
            step: Step::Source,
            job_control: None,
            write_mark: None,
            write_reply: None,
            release_download: None,
            jobs: None,
            export_dialog: ExportDialog::new(),
            export_after_sync: false,
            shot_confirmation_ready: false,
            shot_timezone_open: false,
            shot_focus: None,
            startup_write_state_reconciled: false,
            seeded: false,
            composed: ui::widget::Composed::default(),
            view_key: None,
            structure_dirty: true,
            commands: Rc::new(RefCell::new(Vec::new())),
            scroll: Rc::new(Cell::new((0.0, 0.0))),
            dispatcher: None,
            control_actions: None,
            live: false,
            reaches_machine: true,
            dirty: true,
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
        self.can_advance_with_writer(from, self.writer().is_available())
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

    #[cfg(test)]
    pub(crate) fn ssh_key_for_test(&self) -> &str {
        &self.ssh_public_key
    }

    #[cfg(test)]
    pub(crate) fn resource_source(&self) -> Option<&Source> {
        self.source.as_ref()
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

    /// The last profile [`EncodedAction`](tilo_actions::EncodedAction) an
    /// on-screen control dispatched, for the affordance == `ActionSpec` covenant
    /// test.
    #[doc(hidden)]
    #[cfg(test)]
    pub(crate) fn show_customize_for_test(&mut self) {
        self.select_target(0);
        self.goto(Step::Customize);
    }
    #[cfg(test)]
    pub(crate) fn forget_target_for_test(&mut self) {
        self.selected = None;
        self.dirty = true;
    }
    #[cfg(test)]
    pub(crate) fn profile_for_test(&self) -> &InstallProfile {
        &self.profile
    }

    /// Show the selected image and install profile the framework resource holds.
    /// An empty projection does not wipe a selection that has not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &Imager) {
        self.source.clone_from(&state.source);
        self.recent = state.recent_sources();
        self.profile = state.profile.clone();

        self.dirty = true;
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: Dispatcher<Imager>,
        jobs: Jobs<Imager>,
        mode: RunMode,
    ) {
        self.dispatcher = Some(dispatcher);
        self.jobs = Some(jobs);
        self.live = self.reaches_machine && mode.is_live();
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

/// The step titles the stepper shows.
fn stepper_labels() -> [&'static str; 6] {
    Step::all().map(|step| step.title())
}

// =========================================================================
// Composition
// =========================================================================
impl ImagerApp {
    pub(crate) fn attach_control_actions(&mut self, actions: ui::framework::Actions<Imager>) {
        self.control_actions = Some(actions);
        self.dirty = true;
    }

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
            .release_progress()
            .and_then(|progress| progress.completed.zip(progress.total))
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
            self.downloading_release(),
            self.release_progress()
                .as_ref()
                .map(|progress| progress.phase.as_str()),
            download,
            self.release_download
                .as_ref()
                .map(|download| &download.control)
                .map(crate::release::DownloadControl::is_cancelled),
            self.progress.phase,
            self.progress.error,
            self.progress.overall() * 100.0,
            self.job_control.as_ref().map(JobControl::is_cancelled),
            self.status,
            self.status_is_error,
            self.confirm_matches(),
            self.customizable(),
            self.writer().is_available() || self.shot_confirmation_ready,
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
        // Before the write, a done step's station takes the person back to
        // it; once writing, the stepper only says where it is.
        let mut stepper = ui::widget::Stepper::new(
            stepper_labels()
                .iter()
                .map(|step| (*step).to_string())
                .collect(),
        )
        .with_id("imager.stepper")
        .current(self.stepper_current());
        if matches!(self.step, Step::Drive | Step::Customize | Step::Confirm) {
            let sink = Rc::clone(&self.commands);
            stepper = stepper.on_station(move |index| {
                sink.borrow_mut().push(Command::Station(index));
            });
        }
        Frame::new(body)
            .stepper_widget(stepper)
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
                    .with_icon(Icon::ChevronLeft)
                    .control_size(ControlSize::Large),
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
        row.push(Box::new(primary.control_size(ControlSize::Large)))
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
                        .icon(Icon::Disc3)
                        .secondary(self.source_secondary(source))
                        .meta(fmt_size(source.size))
                        .selected(true)
                        .checked(true),
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
                            .icon(Icon::Disc3)
                            .secondary(self.source_secondary(source))
                            .meta(fmt_size(source.size))
                            .selected(self.recent_selected(index))
                            .checked(self.recent_selected(index))
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
        let downloading = self.downloading_release() == Some(index);
        let secondary = if downloading {
            let progress = self.release_progress();
            let stage = progress
                .as_ref()
                .map_or("Preparing", |progress| progress.phase.as_str());
            match self
                .release_progress()
                .and_then(|progress| progress.completed.zip(progress.total))
            {
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
            Icon::Disc3
        })
        .secondary(secondary)
        .meta(fmt_size(release.size))
        .selected(self.release_selected(index))
        .checked(self.release_selected(index))
        .on_activate(move || sink.borrow_mut().push(command.clone()))
        .with_enabled(self.release_download.is_none() || downloading);
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
                        .icon(drive.icon())
                        .secondary(format!(
                            "{}, {}",
                            drive.dev_path(),
                            drive.contents_summary()
                        ))
                        .meta(fmt_size(drive.size))
                        .selected(self.selected == Some(index))
                        .checked(self.selected == Some(index))
                        .on_activate(move || sink.borrow_mut().push(Command::Target(index))),
                ) as Box<dyn Widget>
            })
            .collect();
        body.push(Box::new(Section::new(rows)))
    }

    // ---- Customize ---------------------------------------------------------

    fn customize_body(&self) -> Column {
        let mut body = page::body("Prepare the setup");
        if let Some(source) = &self.source {
            body = body.push(Box::new(
                Text::new(&format!("Selected image: {}", source.name)).with_role(TextRole::Caption),
            ));
        }
        if let Some(binding) = self
            .control_actions
            .as_ref()
            .and_then(|actions| actions.effect(&ImagerEffect::ExportConfig))
        {
            body = body.push(Box::new(
                Button::new("imager.export-config", "Preview embedded setup").on_action(binding),
            ));
        }
        if !self.customizable() {
            return body.push(Box::new(Text::new(PLAIN_BODY).dimmed()));
        }
        body.push(page::form_block(crate::customize::form(
            &self.profile,
            &self.commands,
            self.control_actions.as_ref(),
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
        } else if let Some(hostname) = crate::handoff::chosen_hostname(&self.profile) {
            crate::setup::strings::t_args(
                "imager-review-setup-hostname",
                &[("hostname", ui::i18n::ArgValue::from(hostname))],
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
        if !(self.writer().is_available() || self.shot_confirmation_ready) {
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
                &self.ssh_public_key,
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
            .with_rhythm(ui::tokens::Rhythm::Group)
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
            .with_rhythm(ui::tokens::Rhythm::Group)
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

const WRITE_JOB: &str = "image-write";
const RELEASE_DOWNLOAD: &str = "release-download";
struct ReleaseDownload {
    index: usize,
    control: crate::release::DownloadControl,
}
impl ImagerApp {
    fn downloading_release(&self) -> Option<usize> {
        self.release_download
            .as_ref()
            .map(|download| download.index)
    }
    fn writer(&self) -> crate::write::WriteBackend {
        crate::write::WriteBackend::for_window(self.live)
    }
    fn submit(&self, submit: impl ui::framework::Submit<Imager>) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.submit(submit);
        }
    }
    fn select_source(&mut self, src: &Source) {
        // A source outside the authenticated catalog cannot carry a profile.
        if !self.source_is_official(src) {
            self.action = Action::Flash;
        }
        self.submit(Imager::SELECT_SOURCE.with(src.clone()));
        self.source = Some(src.clone());
        self.set_status(&format!(
            "Selected {} — {}",
            src.name,
            if self.source_is_official(src) {
                "official release verified"
            } else {
                "local image, written as-is"
            }
        ));
        self.dirty = true;
    }
    fn image_request() -> PickRequest {
        PickRequest::open("Choose a disk image", "Choose").filter(
            FileFilter::new("Disk images")
                .glob("*.iso")
                .glob("*.img")
                .glob("*.raw")
                .mimetype("application/x-iso9660-image"),
        )
    }
    fn take_chosen_image(&mut self, path: Option<std::path::PathBuf>) -> EffectAnswer {
        self.dirty = true;
        let Some(path) = path else {
            self.set_status("File selection cancelled.");
            return Ok(Effected::done());
        };
        if let Some(source) = crate::probe::inspect_source(&path) {
            self.select_source(&source);
            Ok(Effected::done())
        } else {
            let message = "That file is not a readable disk image.";
            self.set_status(message);
            Err(message.into())
        }
    }
    fn download_release(&mut self, index: usize, release: TiloRelease) {
        if self.release_download.is_some() {
            self.set_status("A Tilo installer download is already in progress.");
            return;
        }
        let Some(jobs) = self.jobs.clone() else {
            self.set_status("Tilo installers download in a window someone uses.");
            return;
        };
        self.set_status(&format!(
            "Downloading and verifying {}…",
            release.display_name()
        ));
        let control = crate::release::DownloadControl::default();
        let worker = control.clone();
        jobs.start::<Self>(RELEASE_DOWNLOAD, move |cx| {
            Ok(cx.thread(
                move |progress| {
                    crate::release::verified_image_observed(&release, &worker, &mut |event| {
                        if progress.is_cancelled() {
                            worker.cancel();
                        }
                        match event {
                            crate::release::DownloadEvent::Downloading { received, total } => {
                                progress.report("downloading", Some(received), Some(total));
                            }
                            crate::release::DownloadEvent::Verifying => {
                                progress.report("verifying", None, None);
                            }
                            crate::release::DownloadEvent::Expanding { written, total } => {
                                progress.report("preparing image", Some(written), Some(total));
                            }
                        }
                    })
                },
                |app: &mut Self, result, _| {
                    app.finish_release_download(result);
                    Ok(Effected::done())
                },
            ))
        });
        self.release_download = Some(ReleaseDownload { index, control });
        self.dirty = true;
    }
    fn cancel_release_download(&mut self) -> Result<(), String> {
        let Some(download) = &self.release_download else {
            return Err("No Tilo installer download is running".into());
        };
        download.control.cancel();
        self.set_status(
            "Cancelling safely… The verified partial download will be kept for resume.",
        );
        self.dirty = true;
        Ok(())
    }
    fn release_progress(&self) -> Option<JobProgress> {
        self.jobs.as_ref()?.progress(RELEASE_DOWNLOAD)
    }
    fn download_status(progress: &JobProgress) -> String {
        match progress.phase.as_str() {
            "downloading" => {
                let percent = progress
                    .completed
                    .zip(progress.total)
                    .and_then(|(done, total)| done.saturating_mul(100).checked_div(total))
                    .unwrap_or(0);
                format!("Downloading Tilo installer… {percent}%")
            }
            "verifying" => "Cryptographically verifying the Tilo installer…".into(),
            _ => "Preparing the verified Tilo installer image…".into(),
        }
    }
    fn finish_release_download(&mut self, result: Result<Source, String>) {
        self.release_download = None;
        match result {
            Ok(source) => {
                if let Some(release) = self
                    .releases
                    .iter_mut()
                    .find(|release| release.matches_source(&source))
                {
                    release.local_path.clone_from(&source.path);
                }
                self.select_source(&source);
                self.set_status("Tilo installer downloaded and cryptographically verified.");
            }
            Err(error) => self.set_status(&error),
        }
        self.dirty = true;
    }
    fn select_target(&mut self, idx: usize) {
        let Some(drive) = self.targets.get(idx) else {
            return;
        };
        // Hard guard repeated below the visible-list filter: stale persisted or
        // collaborative state cannot turn the media creator into an internal-
        // disk raw writer.
        if !drive.is_media_target() {
            self.set_status(
                "Internal and system disks can only be installed from the booted Tilo medium.",
            );
            self.dirty = true;
            return;
        }
        self.selected = Some(idx);
        self.confirm_text.clear();
        self.dirty = true;
    }
    fn set_status(&mut self, s: &str) {
        self.status = s.to_string();
    }
    fn goto(&mut self, s: Step) {
        self.step = s;
        self.scroll.set((0.0, 0.0));

        self.dirty = true;
    }
    fn advance(&mut self) {
        match self.step {
            Step::Source if self.can_advance(Step::Source) => self.goto(Step::Drive),
            Step::Drive if self.can_advance(Step::Drive) => {
                self.action = Action::Flash;
                if self.customizable() {
                    self.goto(Step::Customize);
                } else {
                    self.goto(Step::Confirm);
                }
            }
            Step::Customize => match self.profile.validate_media_profile() {
                Ok(()) => self.goto(Step::Confirm),
                Err(message) => self.set_status(&message),
            },
            // Confirm advances into the write only when the typed-name gate is
            // armed; that same gesture kicks off the (external) job.
            Step::Confirm if self.can_advance(Step::Confirm) => self.start_write(),
            _ => {}
        }
    }
    fn back(&mut self) {
        match self.step {
            Step::Confirm if self.customizable() => self.goto(Step::Customize),
            Step::Confirm | Step::Customize => self.goto(Step::Drive),
            Step::Drive => self.goto(Step::Source),
            // No going back from the source, an in-flight write, or the outcome.
            Step::Source | Step::Write | Step::Done => {}
        }
    }
    fn start_write(&mut self) {
        let device = self.target().map(TargetDrive::dev_path).unwrap_or_default();
        if let Err(reason) = self.write_refusal(&device) {
            self.set_status(&reason);
            self.dirty = true;
            return;
        }
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(&ImagerEffect::Write {
                device,
                confirmation: self.confirm_text.clone(),
            });
        }
    }
    fn writer_refusal(&self) -> Result<(), (&'static str, String)> {
        if !self.startup_write_state_reconciled {
            return Err(("restoring_write_state", "Restoring the previous image-write state; wait for drive review before starting another write".into()));
        }
        if self.job_control.is_some() {
            return Err((
                "write_in_progress",
                "An image write is already in progress".into(),
            ));
        }
        if !self.writer().is_available() {
            return Err((
                "write_backend_disabled",
                "Raw-device writing is disabled; enable the privileged Imager backend first".into(),
            ));
        }
        if self.source.is_none() {
            return Err((
                "incomplete_plan",
                "Select an image source and target drive before writing".into(),
            ));
        }
        Ok(())
    }
    fn media_target(&self, device: &str) -> Option<usize> {
        self.targets
            .iter()
            .position(|target| target.is_media_target() && target.dev_path() == device)
    }
    pub(crate) fn choose_for_write(&mut self, device: &str) {
        if let Some(index) = self.media_target(device)
            && self.selected != Some(index)
        {
            self.select_target(index);
        }
    }
    fn write_refusal(&self, device: &str) -> Result<(), String> {
        self.writer_refusal().map_err(|(_, reason)| reason)?;
        if self.media_target(device).is_none() {
            return Err("Select an image source and target drive before writing".into());
        }
        Ok(())
    }
    fn cancel_write(&mut self) -> Result<(), String> {
        let Some(control) = self.job_control.clone() else {
            return Err("No image write is currently running".into());
        };
        control.cancel();
        self.set_status("Stopping safely… Keep the drive connected until cancellation finishes.");
        self.dirty = true;
        Ok(())
    }
    fn begin_write(&mut self, device: &str, confirmation: &str) -> Result<(), String> {
        if !self.startup_write_state_reconciled {
            return Err("Restoring the previous image-write state; wait for drive review before starting another write".into());
        }
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
        if device != tgt.dev_path() {
            return Err("The selected target changed; review the write plan again.".into());
        }
        // Checked before the identity lookup, so a setup machine without an
        // identity does not hide the missing passphrase behind that error.
        if !self.profile.wifi_ssid.trim().is_empty() && self.wifi_psk.is_empty() {
            return Err("A Wi-Fi password is required when a network name is set.".into());
        }
        crate::handoff::validate_bringup_ssh_key(&self.ssh_public_key)?;
        // Stage Wi-Fi, the owner SSH key, and enrollment for any image the
        // operator configured on Confirm. Official media still embeds only the
        // secret-free profile block; the passphrase, key, and new device key
        // stay in the staged tree and are copied onto the installed root
        // after verification. Only enrollment needs this machine's identity.
        let handoff_stage = if self.profile.enroll_local_identity {
            let local = crate::handoff::local_identity_from_env()?;
            let staged =
                crate::handoff::stage_setup_handoff(&self.profile, &self.wifi_psk, &local)?;
            if let Some(stage) = staged.as_ref()
                && let Err(error) =
                    crate::handoff::install_bringup_ssh_key(stage.path(), &self.ssh_public_key)
            {
                let _ = std::fs::remove_dir_all(stage.path());
                return Err(error);
            }
            staged.map(|stage| stage.root)
        } else {
            crate::handoff::stage_access_handoff(
                &self.profile,
                &self.wifi_psk,
                &self.ssh_public_key,
                !src.installable,
            )?
            .map(|stage| stage.root)
        };
        let src_size = src.size;
        let req = WriteRequest {
            source: src,
            target: tgt.clone(),
            action: self.action,
            profile: self.profile.clone(),
            handoff_stage: handoff_stage.clone(),
        };
        let (start, rx, control) = spawn_job(req, confirmation, self.writer());
        let refuse = |message: String| -> Result<(), String> {
            if let Some(stage) = handoff_stage.as_ref() {
                let _ = std::fs::remove_dir_all(stage);
            }
            Err(message)
        };
        match start {
            crate::write::JobStart::Started => {
                let (Some(rx), Some(control), Some(jobs)) = (rx, control, self.jobs.clone()) else {
                    return refuse("The writer could not start in this window.".into());
                };
                Self::start_write_job(&jobs, rx, control.clone(), src_size);
                self.job_control = Some(control);
                self.write_mark = None;
                self.progress = Progress {
                    phase: JobPhase::Writing,
                    write_frac: 0.0,
                    verify_frac: 0.0,
                    rate_bps: 0,
                    error: None,
                };
                self.set_status(&format!(
                    "{} → {} (writing)",
                    self.source
                        .as_ref()
                        .map(|s| s.name.clone())
                        .unwrap_or_default(),
                    tgt.dev_path()
                ));
                // The write has begun: move onto the live-progress station.
                self.goto(Step::Write);
            }
            crate::write::JobStart::ConfirmationMismatch => {
                return refuse("Confirmation does not match — write refused.".into());
            }
            crate::write::JobStart::TooSmall => {
                return refuse("Image is larger than the drive — write refused.".into());
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
    fn start_write_job(
        jobs: &Jobs<Imager>,
        events: std::sync::mpsc::Receiver<Progress>,
        control: JobControl,
        size: u64,
    ) {
        jobs.start::<Self>(WRITE_JOB, move |cx| {
            Ok(cx.thread(
                move |progress| {
                    let mut last = None;
                    for event in &events {
                        if progress.is_cancelled() {
                            control.cancel();
                        }
                        let fraction = match event.phase {
                            JobPhase::Verifying => event.verify_frac,
                            _ => event.write_frac,
                        };
                        progress.report(
                            event.phase.key(),
                            Some((fraction.clamp(0.0, 1.0) * size as f64) as u64),
                            Some(size),
                        );
                        let ended = matches!(event.phase, JobPhase::Success | JobPhase::Failed);
                        last = Some(event);
                        if ended {
                            break;
                        }
                    }
                    last
                },
                |app: &mut Self, last, _| {
                    app.finish_write(last);
                    Ok(Effected::done())
                },
            ))
        });
    }
    fn drain_job(&mut self) {
        if self
            .write_reply
            .as_ref()
            .is_some_and(EffectReply::is_cancelled)
            && self
                .job_control
                .as_ref()
                .is_some_and(|control| !control.is_cancelled())
        {
            let _ = self.cancel_write();
        }
        let Some(progress) = self.jobs.as_ref().and_then(|jobs| jobs.progress(WRITE_JOB)) else {
            return;
        };
        self.follow_write(&progress, ui::time::now());
        if let Some(reply) = &self.write_reply {
            reply.progress().report(
                self.progress.phase.key(),
                Some((self.progress.overall().clamp(0.0, 1.0) * 1000.0) as u64),
                Some(1000),
            );
        }
    }
    fn follow_write(&mut self, progress: &JobProgress, at: ui::time::Instant) {
        let phase = JobPhase::from_key(&progress.phase);
        if !phase.is_active() {
            return;
        }
        let done = progress.completed.unwrap_or(0);
        let fraction = progress
            .total
            .filter(|total| *total > 0)
            .map_or(0.0, |total| done as f64 / total as f64);
        let (write_frac, verify_frac) = if phase == JobPhase::Verifying {
            (1.0, fraction)
        } else {
            (fraction, 0.0)
        };
        let rate_bps = match self.write_mark {
            Some((before, then)) if done >= before && phase == self.progress.phase => {
                let seconds = at.saturating_duration_since(then).as_secs_f64();
                if seconds > 0.0 {
                    ((done - before) as f64 / seconds) as u64
                } else {
                    self.progress.rate_bps
                }
            }
            _ => 0,
        };
        if self.write_mark.is_none_or(|(before, _)| before != done) {
            self.write_mark = Some((done, at));
        }
        self.progress = Progress {
            phase,
            write_frac,
            verify_frac,
            rate_bps,
            error: None,
        };
        self.dirty = true;
    }
    fn finish_write(&mut self, last: Option<Progress>) {
        let cancelled = self
            .job_control
            .as_ref()
            .is_some_and(JobControl::is_cancelled);
        self.job_control = None;
        self.write_mark = None;
        let mut outcome = last
            .filter(|last| !last.phase.is_active())
            .unwrap_or_else(|| Progress {
                phase: JobPhase::Failed,
                error: Some(
                    "The writer stopped unexpectedly. Reconnect the drive, then review and retry."
                        .into(),
                ),
                ..self.progress.clone()
            });
        // Cancellation may race with a terminal helper event already in
        // the pipe. Honour the user's stop request conservatively: never
        // turn that race into a claim that the medium is ready to use.
        if outcome.phase == JobPhase::Success && cancelled {
            outcome.phase = JobPhase::Failed;
            outcome.error = Some(
                "Image writing was cancelled. The drive may be incomplete; review and retry before using it."
                    .into(),
            );
        }
        let name = self
            .source
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let message = if outcome.phase == JobPhase::Success {
            format!("Done — {name} {} and verified", self.action.past())
        } else {
            outcome.error.clone().unwrap_or_else(|| {
                format!(
                    "{} failed. Reconnect the drive, then review and retry.",
                    self.action.title()
                )
            })
        };
        self.progress = outcome;
        self.set_status(&message);
        self.goto(Step::Done);
        self.dirty = true;
        if let Some(reply) = self.write_reply.take() {
            reply.finish(match self.progress.phase {
                JobPhase::Success => Ok(tilo_actions::ActionOutput {
                    value: Some(self.status_value()),
                    resources: Vec::new(),
                }),
                _ => Err(self
                    .progress
                    .error
                    .clone()
                    .unwrap_or_else(|| self.status.clone())),
            });
        }
    }
    fn status_value(&self) -> serde_json::Value {
        serde_json::json!({
            "phase": self.progress.phase.key(),
            "progress": self.progress.overall(),
            "message": self.status,
        })
    }
    fn write_another(&mut self) {
        if self.progress.phase == JobPhase::Failed {
            // Preserve the reviewed source, target, and customized setup. A
            // retry still requires a fresh peer-local destructive confirmation
            // and passes every target/source validation again in the helper.
            self.progress = Progress::default();
            self.confirm_text.clear();
            self.goto(Step::Confirm);
            self.set_status("Review the image and drive, then confirm the target again to retry.");
            return;
        }
        self.progress = Progress::default();
        self.confirm_text.clear();
        self.goto(Step::Source);
        self.set_status("");
    }
    fn export_config(&mut self) {
        let text = self
            .profile
            .to_headless_config(self.source.as_ref(), self.target());
        self.export_dialog.show(text);
        self.dirty = true;
    }
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
    fn do_rescan(&mut self) -> bool {
        if self.live {
            // Re-read the live block-device inventory. `probe()` returns `None`
            // only when the platform probe is unavailable or malformed, so in
            // that case we keep the last-known list rather than blanking it.
            // A real (possibly empty) result is authoritative and replaces it,
            // so unplugging the selected drive genuinely drops it from the list.
            let Some(t) = crate::probe::probe() else {
                self.set_status("Drive rescan failed. Check the system disk tools and try again.");
                self.dirty = true;
                return false;
            };
            self.apply_drive_inventory(t);
            let n = self.targets.len();
            let removable = self.removable_count();
            self.set_status(&format!("Rescanned — {removable} removable, {n} total"));
        } else {
            self.set_status("Rescanned drives");
        }
        self.selected = self.selected.filter(|i| *i < self.targets.len());
        self.dirty = true;
        true
    }
    fn reconcile_persisted_write_state(&mut self) {
        self.startup_write_state_reconciled = true;
    }
    pub(crate) fn answer_effect(
        &mut self,
        effect: ImagerEffect,
        cx: &mut EffectCx<Imager, Self>,
    ) -> EffectAnswer {
        match effect {
            ImagerEffect::Write {
                device,
                confirmation,
            } => {
                self.choose_for_write(&device);
                self.write_refusal(&device)?;
                self.begin_write(&device, &confirmation)?;
                self.write_reply = Some(cx.reply());
                Ok(Effected::Pending)
            }
            ImagerEffect::Status => Ok(Effected::value(self.status_value())),
            ImagerEffect::CustomizeImage {
                image,
                hostname,
                wifi_ssid,
                wifi_psk_file,
                ssh_keys_file,
                home_files,
            } => {
                let mut args = vec!["imager".into(), "--customize-image".into(), image];
                for (flag, value) in [
                    ("--hostname", hostname),
                    ("--wifi-ssid", wifi_ssid),
                    ("--wifi-psk-file", wifi_psk_file),
                    ("--ssh-keys-file", ssh_keys_file),
                ] {
                    if let Some(value) = value {
                        args.extend([flag.into(), value]);
                    }
                }
                for path in home_files {
                    args.extend(["--home-file".into(), path]);
                }
                Ok(cx.thread(
                    move |_| {
                        crate::customize_image::maybe_run(&args).map_err(|error| error.to_string())
                    },
                    |app: &mut Self, result, _| {
                        let code = result?.ok_or(
                            "the image customization helper did not recognize its request",
                        )?;
                        app.set_status("Image customization complete");
                        app.dirty = true;
                        Ok(Effected::value(serde_json::json!({ "exit_code": code })))
                    },
                ))
            }
            ImagerEffect::ChooseImage => {
                self.set_status("Opening the file picker…");
                self.dirty = true;
                Ok(
                    cx.choose_path(Self::image_request(), |app: &mut Self, path, _| {
                        app.take_chosen_image(path)
                    }),
                )
            }
            ImagerEffect::CancelDownload => {
                self.cancel_release_download()?;
                Ok(Effected::done())
            }
            ImagerEffect::ExportConfig => {
                if self.source.is_none() || self.target().is_none() {
                    return Err("Select an image source and target drive first".into());
                }
                let text = self
                    .profile
                    .to_headless_config(self.source.as_ref(), self.target());
                self.export_dialog.show(text.clone());
                self.dirty = true;
                Ok(Effected::value(text))
            }
            ImagerEffect::SetWifiPsk { value } => {
                self.wifi_psk = value;

                self.dirty = true;
                Ok(Effected::done())
            }
            ImagerEffect::SetSshKey { value } => {
                crate::handoff::validate_bringup_ssh_key(&value)?;
                self.ssh_public_key = value;
                self.dirty = true;
                Ok(Effected::done())
            }
            ImagerEffect::Rescan => {
                if self.do_rescan() {
                    Ok(Effected::done())
                } else {
                    Err(self.status.clone())
                }
            }
            ImagerEffect::ChooseDrive { drive } => {
                let Some(index) = self.targets.iter().position(|target| target.name == drive)
                else {
                    return Err(format!("No drive named {drive} is connected"));
                };
                self.select_target(index);
                if self.selected == Some(index) {
                    Ok(Effected::done())
                } else {
                    Err(self.status.clone())
                }
            }
        }
    }
    pub(crate) fn effect_statuses(&self) -> Vec<EffectStatus> {
        let mut statuses = Vec::new();
        if let Err((code, reason)) = self.writer_refusal() {
            statuses.push(EffectStatus::unavailable(ImagerEffect::WRITE, code, reason));
        }
        if self.release_download.is_none() {
            statuses.push(EffectStatus::unavailable(
                ImagerEffect::CANCEL_DOWNLOAD,
                "no_download",
                "No Tilo installer download is running",
            ));
        }
        if self.source.is_none() || self.target().is_none() {
            statuses.push(EffectStatus::unavailable(
                ImagerEffect::EXPORT_CONFIG,
                "incomplete_plan",
                "Select an image source and target drive first",
            ));
        }
        statuses
    }
    pub(crate) fn published_rows(&self) -> Vec<LiveRow> {
        let mut rows = self
            .targets
            .iter()
            .filter(|target| target.is_media_target())
            .map(|target| self.target_row(target))
            .collect::<Vec<_>>();
        let phase = self.progress.phase.key();
        rows.push(
            LiveRow::new("imager.job", "imager.job", format!("Image write: {phase}"))
                .effect(&ImagerEffect::Status),
        );
        if let Some(download) = &self.release_download {
            let name = self
                .releases
                .get(download.index)
                .map(TiloRelease::display_name)
                .unwrap_or_default();
            let detail = self.release_progress().map_or_else(
                || "Preparing".to_owned(),
                |progress| Self::download_status(&progress),
            );
            rows.push(
                LiveRow::new(
                    "imager.download",
                    "imager.download",
                    format!("Download {name}"),
                )
                .presentation(tilo_actions::ResourcePresentation {
                    detail: Some(detail),
                    ..tilo_actions::ResourcePresentation::default()
                })
                .effect(&ImagerEffect::CancelDownload),
            );
        }
        rows
    }
    fn target_row(&self, target: &TargetDrive) -> LiveRow {
        let identity = target.stable_id.as_deref().unwrap_or(&target.name);
        let device = target.dev_path();
        let mut write = ImagerEffect::Write {
            device: device.clone(),
            confirmation: target.name.clone(),
        }
        .bound();
        write.contract.risk = tilo_actions::RiskLevel::Destructive;
        write.contract.confirmation = tilo_actions::ConfirmationPolicy::Phrase {
            phrase: target.name.clone(),
        };
        let write = match self.writer_refusal() {
            Err((code, reason)) => write.unavailable(code, reason),
            Ok(()) => write,
        };
        LiveRow::new(format!("imager.target:{identity}"), "imager.target", device)
            .effect(&ImagerEffect::ChooseDrive {
                drive: target.name.clone(),
            })
            .operation(write)
    }
}

impl ImagerApp {
    pub(crate) fn present_state(&mut self, state: &Imager) {
        self.adopt_resource_state(state);
    }
    fn go_to_station(&mut self, index: usize) {
        if let Some(step) = Step::all().get(index).copied() {
            if step.index() < self.step.index() && self.step != Step::Write {
                self.goto(step);
            }
        }
    }
    fn apply_command(&mut self, command: Command) {
        match command {
            Command::PickFile => {
                if let Some(dispatcher) = &self.dispatcher {
                    dispatcher.effect(&ImagerEffect::ChooseImage);
                }
            }
            Command::Dropped(path) => match crate::probe::inspect_source(&path) {
                Some(source) => self.select_source(&source),
                None => self.fail("That file is not a readable disk image."),
            },
            Command::Release(index) => {
                if self.release_download.is_none() {
                    if let Some(release) = self.releases.get(index).cloned() {
                        if release.url.is_empty() {
                            self.select_source(&release.to_source())
                        } else {
                            self.download_release(index, release)
                        }
                    }
                }
            }
            Command::CancelDownload => {
                if let Err(error) = self.cancel_release_download() {
                    self.fail(&error)
                }
            }
            Command::Recent(index) => {
                if let Some(source) = self.recent.get(index).cloned() {
                    self.select_source(&source)
                }
            }
            Command::Target(index) => self.select_target(index),
            Command::Rescan => {
                self.do_rescan();
            }
            Command::Back => self.back(),
            Command::Station(index) => self.go_to_station(index),
            Command::Next => self.advance(),
            Command::StartWrite => self.start_write(),
            Command::CancelWrite => {
                let _ = self.cancel_write();
            }
            Command::WriteAnother => self.write_another(),
            Command::Confirm(value) => {
                if self.step == Step::Confirm {
                    self.confirm_text = value
                }
            }
            Command::Profile(field, shown) => {
                if self.step == Step::Customize && self.customizable() {
                    match crate::customize::profile_value(field, &shown) {
                        Some(value) => match crate::customize::field_edit(field, value) {
                            Some(edit) => self.submit(edit),
                            None => self.fail(&format!("Choose a listed {}", field.label())),
                        },
                        None => self.fail(&format!("Choose a listed {}", field.label())),
                    }
                }
            }
            Command::WriteSetup(field, value) => {
                if self.step == Step::Confirm && self.offers_write_setup() {
                    match crate::write_setup::field_edit(field, value.clone()) {
                        Some(edit) => self.submit(edit),
                        None => {
                            if let Some(dispatcher) = &self.dispatcher {
                                match field {
                                    SetupField::WifiPsk => {
                                        dispatcher.effect(&ImagerEffect::SetWifiPsk { value });
                                    }
                                    SetupField::SshKey => {
                                        self.ssh_public_key = value;
                                        self.dirty = true;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
            Command::Enroll(on) => {
                if self.step == Step::Confirm && self.offers_write_setup() {
                    self.submit(Imager::paths().profile.enroll_local_identity.set(on));
                }
            }
        }
    }
    fn fail(&mut self, message: &str) {
        self.status_is_error = true;
        self.set_status(message);
    }
    fn drain_commands(&mut self) -> bool {
        let commands = std::mem::take(&mut *self.commands.borrow_mut());
        let changed = !commands.is_empty();
        for command in commands {
            self.apply_command(command);
        }
        changed
    }
    pub(crate) fn stage_scenario(&mut self, scenario: &str) {
        self.seed_local_shot_state(scenario);
    }
}
impl Widget for ImagerApp {
    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        if !self.seeded {
            // Start genuinely blank. In the live app a real `lsblk` probe
            // fills the drive list; if it finds nothing (or is unavailable)
            // the list stays empty and the Drive step shows its honest
            // "insert a drive" state — the imager never invents drives.
            if self.live {
                self.targets = crate::probe::probe().unwrap_or_default();
            }
            if self.live {
                self.reconcile_persisted_write_state();
            } else {
                // Screenshots and tests pose worker-less progress frames.
                // They still complete startup so operations cannot be held
                // unavailable forever outside a live window.
                self.startup_write_state_reconciled = true;
            }
            self.seeded = true;
        }

        if self.live && self.releases.is_empty() {
            match crate::release::bundled_releases() {
                Ok(releases) => self.releases = releases,
                Err(error) => self.status = error,
            }
        }

        // Fold in any async work.
        if let Some(progress) = self.release_progress() {
            self.status = Self::download_status(&progress);
        }
        self.drain_job();

        self.action = Action::Flash;
        self.selected = self.selected.filter(|i| {
            self.targets
                .get(*i)
                .is_some_and(TargetDrive::is_media_target)
        });

        // Refresh the shared customization panel's fields from the profile
        // (each unfocused field mirrors the stored value; a focused one stays
        // the source of truth).

        if self.export_after_sync {
            self.export_after_sync = false;
            self.export_config();
        }
        self.refresh_view();
        self.composed.sync(_state, _path);
        self.dirty = true;
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.refresh_view();
        self.composed
            .element_mut()
            .is_some_and(|view| view.update(state, path))
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
            .element()
            .and_then(page::focused_id)
            .or_else(|| self.shot_focus.take().map(str::to_string));
        let mut composed = self.build_composition();
        if let Some(id) = focused {
            page::refocus(composed.as_mut(), &id);
        }
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed.replace(composed);
        self.view_key = Some(self.view_key());
        self.structure_dirty = false;
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            flex_grow: 1.0,
            ..Default::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.composed.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.export_dialog.is_open() {
            self.export_dialog
                .layout_in(self.composed.rect(), theme, text_renderer);
        }
        // The framework paints the window ground; the page is toolkit widgets.
        if let Some(composed) = self.composed.element_mut() {
            composed.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.export_dialog.handle_detached(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            &mut ctx.state,
            ctx.path,
        ) {
            *ctx.handled = true;
            return;
        }
        if let Some(composed) = self.composed.element_mut()
            && let Ok(children) = ctx.taffy.children(ctx.node_id)
            && let Some(&child) = children.first()
        {
            composed.handle_event(event, &mut ctx.reborrow(ctx.path, child));
        }
        self.drain_commands();
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
            if matches!(
                logical_key,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape)
            ) {
                self.back();
                *ctx.handled = true;
            }
            self.refresh_view();
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.export_dialog.is_open() {
            self.export_dialog
                .draw_detached(scene, transform, text, theme, presence);
        }
    }
    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &ObjId) {
        if let Some(view) = self.composed.element_mut() {
            view.settle(edits, path);
        }
        if self.drain_commands() {
            self.refresh_view();
        }
    }
    fn children(&self) -> Vec<&dyn Widget> {
        self.composed.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed.children_mut()
    }

    fn name(&self) -> &'static str {
        "ImagerApp"
    }

    fn get_rect(&self) -> Rect {
        self.composed.rect()
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.structure_dirty) || self.composed.take_structure_dirty()
    }

    /// Keep the event loop ticking while a job or file picker is in flight, so
    /// the progress shows and the portal result is drained promptly.
    fn needs_animation_frame(&self) -> bool {
        self.job_control.is_some()
            || false
            || self.release_download.is_some()
            || self
                .composed
                .element()
                .is_some_and(|child| child.needs_animation_frame())
    }

    fn needs_idle_sync(&self) -> bool {
        self.job_control.is_some() || false || self.release_download.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use ui::tilo_resource::Fields as _;

    fn harness(
        app: ImagerApp,
        doc: AutoCommit,
        viewport: (u32, u32),
    ) -> ui::headless::FrameHarness {
        let mut state = ui::State::new();
        assert!(state.adopt_doc(doc));
        ui::headless::FrameHarness::for_root(state, app, viewport, Theme::dark())
    }
    fn ids(app: &ImagerApp) -> Vec<String> {
        fn walk(w: &dyn Widget, out: &mut Vec<String>) {
            if let Some(id) = w.test_id() {
                out.push(id.into());
            }
            for c in w.children() {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        walk(app.build_composition().as_ref(), &mut out);
        out
    }

    /// Present `doc` as the framework presents the imager resource, then run
    /// the widget's own sync.
    fn present(app: &mut ImagerApp, doc: &AutoCommit) {
        use ui::tilo_resource::Resource as _;
        app.present_state(&Imager::read(doc).expect("imager state"));
        let mut model = doc.clone();
        app.sync(&mut model, &automerge::ROOT);
    }

    /// A document holding `state`.
    fn document(state: &Imager) -> AutoCommit {
        let mut doc = AutoCommit::new();
        state.write_fields(&mut doc, &automerge::ROOT).unwrap();
        doc
    }

    /// The demo record: a pre-selected Tilo image and a short recent-sources
    /// history.
    fn demo_state() -> Imager {
        Imager {
            source: Some(crate::model::sample_source()),
            recent: crate::model::sample_recent_sources()
                .into_iter()
                .enumerate()
                .map(|(rank, source)| crate::model::RecentSource {
                    id: ui::tilo_resource::Id::from_raw(source.id()),
                    rank: rank as i64,
                    source,
                })
                .collect(),
            profile: InstallProfile::default(),
        }
    }

    fn seeded_app() -> (ImagerApp, AutoCommit) {
        // Tests drive the wizard against the demo document (sample drives + a
        // Tilo image + recent history) — the same content the screenshot scenarios stage.
        let mut app = ImagerApp::new();
        app.targets = crate::model::sample_targets();
        app.releases = crate::model::sample_releases();
        let doc = document(&demo_state());
        present(&mut app, &doc);
        (app, doc)
    }

    #[test]
    fn unchanged_disk_review_discloses_the_authored_hostname() {
        use ui::tilo_resource::Resource as _;

        let (mut app, mut doc) = seeded_app();
        let mut source = app
            .recent
            .iter()
            .find(|source| source.kind == SourceKind::IsoFile)
            .unwrap()
            .clone();
        source.installable = false;
        let mut state = Imager::read(&doc).unwrap();
        state.select_source(source);
        state.write_fields(&mut doc, &automerge::ROOT).unwrap();
        present(&mut app, &doc);
        app.goto(Step::Confirm);
        assert!(!app.customizable());
        assert_eq!(app.review_setup(), "Written unchanged, then verified");

        Imager::paths()
            .profile
            .hostname
            .replace("reviewed-host")
            .apply_local(&mut doc)
            .unwrap();
        present(&mut app, &doc);
        assert_eq!(
            crate::handoff::chosen_hostname(&app.profile),
            Some("reviewed-host")
        );
        let setup = app
            .review_rows()
            .into_iter()
            .find(|(label, _)| *label == "Setup")
            .unwrap()
            .1;
        assert!(setup.contains("reviewed-host"), "{setup}");
        assert!(setup.contains("then verified"), "{setup}");
        assert!(!setup.contains("Pre-configured"), "{setup}");
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
            app.goto(Step::Confirm);
            app.progress = Progress {
                phase: JobPhase::Writing,
                write_frac: 0.5,
                verify_frac: 0.0,
                rate_bps: 1,
                error: None,
            };
            app.set_status("local writer status");
            assert_eq!(
                doc.save(),
                before,
                "selection, station, status, and job progress are local"
            );
            let peer = AutoCommit::load(&before).unwrap();
            let mut peer_app = ImagerApp::new();
            present(&mut peer_app, &peer);
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
            let doc = AutoCommit::new();
            present(&mut app, &doc);
            assert!(app.targets.is_empty(), "no fabricated drives");
            assert!(app.source.is_none(), "no fabricated image");
            assert!(app.recent.is_empty(), "no fabricated recent history");
            assert_eq!(app.step, Step::Source);
        }

        // Phase `restart_drops_unsupervised_local_write_state_and_requires_new_review`.
        {
            let (mut app, mut doc) = seeded_app();
            app.select_target(0);
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
            let restored_doc = AutoCommit::load(&saved).unwrap();
            present(&mut restarted, &restored_doc);
            assert_eq!(restarted.step, Step::Source);
            assert_eq!(restarted.progress.phase, JobPhase::Idle);
            assert!(restarted.selected.is_none());
            assert!(restarted.status.is_empty());
            assert!(!restarted.can_advance_with_writer(Step::Drive, true));
        }

        // Phase `write_operations_are_blocked_until_startup_state_is_reconciled`.
        {
            let (mut app, _doc) = seeded_app();
            app.select_target(0);
            app.startup_write_state_reconciled = false;
            app.confirm_text = String::from("sdb");

            assert!(!app.can_advance_with_writer(Step::Confirm, true));
            let error = app
                .begin_write(&app.target().unwrap().dev_path(), "sdb")
                .expect_err("unreconciled startup must not expose a write");
            assert!(error.contains("Restoring the previous image-write state"));
            assert!(
                app.effect_statuses()
                    .iter()
                    .any(|status| format!("{status:?}").contains("id: \"write\"")),
                "the write effect is unavailable"
            );
        }

        // Phase `live_app_offers_no_tilo_release_to_write`.
        // A release row is a *write source for erasing a drive*, so the live app
        // must offer none of its own invention: the list comes from the document,
        // and a live (unseeded) document has nothing in it. Sample releases are
        // staged only by screenshot scenarios, like every other sample surface here.
        {
            let mut app = ImagerApp::new();
            let doc = AutoCommit::new();
            present(&mut app, &doc);
            assert!(app.releases().is_empty(), "no fabricated Tilo releases");

            // ...and nothing on the Source step is clickable that would select one.
            let offered = ids(&app).iter().any(|id| id.starts_with("imager.release."));
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
            let (app, _doc) = seeded_app();
            assert_eq!(app.releases().len(), 2);
            let offered = ids(&app)
                .iter()
                .filter(|id| id.starts_with("imager.release."))
                .count();
            assert_eq!(offered, 2, "each seeded release has a row to click");
        }
    }

    /// Accessible semantics, selection labels, live regions and tab order.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn wizard_semantics_labels_live_regions_and_keyboard_order() {
        use ui::a11y::Action as AccessAction;
        // Architecture and board profile remain part of the stable choice,
        // and the actual release control retains that identity in its ID.
        let (mut app, _) = seeded_app();
        let mut generic = crate::model::sample_releases().remove(0);
        generic.architecture = "aarch64".into();
        let mut board = generic.clone();
        board.hardware_profile = "lenovo-83ed".into();
        assert_ne!(generic.selection_id(), board.selection_id());
        app.releases = vec![generic, board.clone()];
        assert!(ids(&app).contains(&format!("imager.release.{}", board.selection_id())));
        app.source = Some(board.to_source());
        assert!(board.matches_source(app.source.as_ref().unwrap()));
        assert!(!app.releases[0].matches_source(app.source.as_ref().unwrap()));
        assert!(
            app.review_rows()
                .into_iter()
                .find(|(label, _)| *label == "Image")
                .unwrap()
                .1
                .contains("lenovo-83ed only")
        );
        let mut host =
            ui::prelude::TestApp::<crate::resource::ImagerShell>::capture(Some("source")).unwrap();
        let tree = host.harness().a11y_nodes();
        assert!(
            tree.iter()
                .any(|(_, n)| n.role() == ui::accesskit::Role::Heading
                    && n.label() == Some("Choose an image"))
        );
        let stepper = tree
            .iter()
            .find(|(_, n)| {
                n.role() == ui::accesskit::Role::ProgressIndicator && n.label() == Some("Source")
            })
            .unwrap();
        assert_eq!(stepper.1.value(), Some("Step 1 of 6"));
        let mut order = Vec::new();
        for _ in 0..16 {
            host.harness().tab(false);
            if let Some(n) = host.harness().focused_access_node() {
                order.push(n.name.unwrap_or_default());
            }
        }
        let browse = order
            .iter()
            .position(|n| n == "Browse…")
            .expect("browse is keyboard reachable");
        let next = order
            .iter()
            .position(|n| n == "Continue")
            .expect("continue is keyboard reachable");
        assert!(browse < next);
        let mut host =
            ui::prelude::TestApp::<crate::resource::ImagerShell>::capture(Some("customize"))
                .unwrap();
        for (id, role) in [
            ("imager.profile.hostname", ui::a11y::Role::TextInput),
            ("imager.profile.locale", ui::a11y::Role::ComboBox),
            ("imager.profile.timezone", ui::a11y::Role::ComboBox),
            ("imager.profile.keyboard", ui::a11y::Role::ComboBox),
            ("imager.profile.filesystem", ui::a11y::Role::ComboBox),
        ] {
            host.harness()
                .expect(&ui::headless::SemanticExpectation::named(id).role(role))
                .unwrap();
        }
        host.harness()
            .perform_named(
                "imager.profile.hostname",
                &AccessAction::SetValue("accessible-host".into()),
            )
            .unwrap();
        assert_eq!(host.state().profile.hostname, "accessible-host");
        let mut order = Vec::new();
        for _ in 0..24 {
            host.harness().tab(false);
            if let Some(n) = host.harness().focused_access_node() {
                order.push(n.name.unwrap_or_default());
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
            assert!(order.iter().any(|n| n == name), "{name}: {order:?}");
        }
        let mut host =
            ui::prelude::TestApp::<crate::resource::ImagerShell>::capture(Some("confirm")).unwrap();
        host.harness()
            .perform_named("imager.confirm", &AccessAction::SetValue("sdb".into()))
            .unwrap();
        assert!(
            host.with_widget::<ImagerApp, _>(|app| app.confirm_matches())
                .unwrap()
        );
        let mut host =
            ui::prelude::TestApp::<crate::resource::ImagerShell>::capture(Some("progress"))
                .unwrap();
        assert!(host.harness().a11y_nodes().iter().any(|(_, n)| n.role()
            == ui::accesskit::Role::ProgressIndicator
            && n.value().is_some()));
    }

    /// Step stations and headings at compact and wide layouts.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn step_rail_keeps_six_stations_inside_the_viewport() {
        assert_eq!(stepper_labels().len(), 6);
        assert_eq!(stepper_labels().first(), Some(&"Source"));
        assert_eq!(stepper_labels().last(), Some(&"Done"));
        for viewport in [(1280, 800), (320, 640)] {
            let (app, doc) = seeded_app();
            let h = harness(app, doc, viewport);
            let stepper = h
                .a11y_nodes()
                .into_iter()
                .find(|(_, n)| {
                    n.role() == ui::accesskit::Role::ProgressIndicator
                        && n.label() == Some("Source")
                })
                .unwrap()
                .1;
            assert_eq!(stepper.value(), Some("Step 1 of 6"));
            let bounds = stepper.bounds().unwrap();
            assert!(bounds.x0 >= 0.0 && bounds.x1 <= f64::from(viewport.0));
            assert!(
                bounds.height()
                    >= f64::from(Theme::dark().text(ui::TextRole::Caption).line_height_px()),
                "the passive progress label fits its measured line"
            );
            assert!(
                h.targets()["imager.next"].height() >= 40.0,
                "wizard navigation retains its accessible target floor"
            );
        }
    }

    /// Body blocks, banners, footers, scrolling and failure detail layout.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn step_body_and_footer_reflow_without_overlap() {
        let mut heading_heights = Vec::new();
        for scale in [1.0, 2.0] {
            let (app, doc) = seeded_app();
            let mut h = harness(app, doc, (1200, 800));
            h.relayout_themed((1200, 800), Theme::dark().with_text_scale(scale))
                .unwrap();
            let heading = h
                .a11y_nodes()
                .into_iter()
                .find(|(_, n)| {
                    n.role() == ui::accesskit::Role::Heading && n.label() == Some("Choose an image")
                })
                .unwrap()
                .1
                .bounds()
                .unwrap();
            heading_heights.push(heading.height());
            let browse = h.targets()["imager.browse"];
            assert!(
                browse.y0 >= heading.y1,
                "content clears the measured heading at {scale}x"
            );
        }
        assert!(
            heading_heights[1] > heading_heights[0] * 1.5,
            "the heading's measured height responds to larger type"
        );

        for viewport in [(1280, 800), (800, 600), (320, 640)] {
            for scenario in [
                "source",
                "drive",
                "drive-empty",
                "customize",
                "confirm",
                "progress",
                "done",
                "done-failed",
            ] {
                let mut host =
                    ui::prelude::TestApp::<crate::resource::ImagerShell>::capture(Some(scenario))
                        .unwrap();
                host.harness().resize(viewport).unwrap();
                let mut back = None;
                let mut primary = None;
                let mut stepper = None;
                for (_, n) in host.harness().a11y_nodes() {
                    if n.role() == ui::accesskit::Role::Window {
                        continue;
                    }
                    let Some(r) = n.bounds() else {
                        continue;
                    };
                    assert!(
                        r.x0 >= -0.5 && r.x1 <= f64::from(viewport.0) + 0.5,
                        "{scenario} {viewport:?} {:?}: {r:?}",
                        n.label()
                    );
                    if n.label() == Some("Back") {
                        back = Some(r);
                    }
                    if matches!(
                        n.label(),
                        Some("Continue" | "Write image" | "Write another")
                    ) {
                        primary = Some(r);
                    }
                    if n.role() == ui::accesskit::Role::ProgressIndicator
                        && matches!(
                            n.label(),
                            Some("Source" | "Drive" | "Customize" | "Confirm" | "Write" | "Done")
                        )
                    {
                        stepper = Some(r);
                    }
                }
                let stepper = stepper.expect("each step retains its position indicator");
                assert!(stepper.y0 >= -0.5 && stepper.y1 <= f64::from(viewport.1) + 0.5);
                if let (Some(back), Some(primary)) = (back, primary) {
                    assert!(back.x1 <= primary.x0 || primary.x1 <= back.x0);
                }
            }
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
            let (mut app, _doc) = seeded_app();
            // Source is seeded → can advance to Drive.
            app.advance();
            assert_eq!(app.step, Step::Drive);
            // No target selected yet → stays on Drive.
            app.advance();
            assert_eq!(app.step, Step::Drive);
            app.select_target(0);
            app.advance();
            // Seeded source is an installable Tilo image → goes through Customize.
            assert_eq!(app.step, Step::Customize);
            app.advance();
            assert_eq!(app.step, Step::Confirm);
        }

        // Phase `privileged_helper_capability_enables_confirm_but_disabled_backend_does_not`.
        {
            let (mut app, _doc) = seeded_app();
            app.select_target(0);
            app.goto(Step::Confirm);
            app.confirm_text = String::from("sdb");

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
            let (mut app, _doc) = seeded_app();
            app.select_target(0);
            app.profile.hostname = "bad host".into();
            app.goto(Step::Customize);

            app.advance();
            assert_eq!(app.step, Step::Customize);
            assert!(app.status.contains("Hostname"));

            // A direct operation dispatch must repeat the gate rather than relying
            // only on wizard navigation.
            app.goto(Step::Confirm);
            app.confirm_text = String::from("sdb");
            let error = app
                .begin_write(&app.target().unwrap().dev_path(), "sdb")
                .expect_err("an invalid profile refuses the write");
            assert_eq!(app.step, Step::Confirm);
            assert!(app.job_control.is_none());
            assert!(error.contains("Hostname"));
        }

        // Phase `plain_iso_skips_customize`.
        {
            let (mut app, _doc) = seeded_app();
            let iso = app
                .recent
                .iter()
                .find(|s| s.kind == SourceKind::IsoFile)
                .cloned()
                .unwrap();
            app.select_source(&iso);
            app.advance(); // → Drive
            app.select_target(0);
            app.advance(); // installable == false → straight to Confirm
            assert_eq!(app.step, Step::Confirm);
            assert!(!app.customizable());
        }

        // Phase `local_tilo_claim_cannot_unlock_preconfiguration`.
        {
            let (mut app, _doc) = seeded_app();
            let official = crate::model::sample_source();
            let mut local = official.clone();
            local.kind = SourceKind::IsoFile;
            local.path = "/tmp/tilo-official-looking.iso".into();
            local.identity = None;
            app.select_source(&local);
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
            let (mut app, _doc) = seeded_app();
            assert!(app.customizable(), "seeded source is official");
            assert!(
                app.offers_write_setup(),
                "official Confirm must collect Wi-Fi and enrollment"
            );
            app.select_target(0);
            app.goto(Step::Confirm);
            app.confirm_text = String::from("sdb");
            app.profile.wifi_ssid = "studio".into();
            app.wifi_psk = "correct-horse".into();
            let err = app
                .begin_write(&app.target().unwrap().dev_path(), "sdb")
                .expect_err("privileged writer is off in unit tests");
            assert!(
                !err.to_lowercase().contains("installer stick"),
                "official installable write must not refuse handoff: {err}"
            );
            assert!(
                err.to_lowercase().contains("disabled") || err.to_lowercase().contains("backend"),
                "expected write-disabled after staging handoff, got: {err}"
            );
            assert!(app.job_control.is_none());
            assert_eq!(app.step, Step::Confirm);
        }

        // Phase `write_refused_without_matching_confirmation`.
        {
            let (mut app, _doc) = seeded_app();
            app.select_target(0); // sdb
            app.goto(Step::Confirm);
            app.confirm_text = String::from("wrong");
            let error = app
                .begin_write(&app.target().unwrap().dev_path(), "wrong")
                .expect_err("a mistyped device name refuses the write");
            assert_eq!(app.progress.phase, JobPhase::Idle, "no job started");
            assert!(app.job_control.is_none());
            assert_eq!(app.step, Step::Confirm, "stays on the Confirm station");
            assert!(error.to_lowercase().contains("confirm"));
        }
    }

    #[test]
    fn write_setup_wifi_and_owner_key_stage_without_identity_enrollment() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(0);
        app.goto(Step::Confirm);
        app.confirm_text = "sdb".into();
        Imager::paths()
            .profile
            .wifi_ssid
            .replace("studio")
            .apply_local(&mut doc)
            .unwrap();
        Imager::paths()
            .profile
            .enroll_local_identity
            .set(false)
            .apply_local(&mut doc)
            .unwrap();
        present(&mut app, &doc);
        app.wifi_psk = "correct-horse".into();
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA owner@example";
        app.ssh_public_key = key.into();
        assert!(!app.profile.enroll_local_identity);
        let error = app
            .begin_write(&app.target().unwrap().dev_path(), "sdb")
            .expect_err("access staging reaches the disabled writer without enrollment");
        assert!(
            error.to_lowercase().contains("disabled") || error.to_lowercase().contains("backend"),
            "expected the disabled write backend after staging access: {error}"
        );
        assert!(app.job_control.is_none());
        assert_eq!(app.step, Step::Confirm);
        // The shared Confirm form replaced the old Action summary. It must
        // still expose the accepted owner key for review on the setup machine.
        let view = harness(app, doc, (1180, 820));
        assert!(
            view.a11y_nodes()
                .iter()
                .any(|(_, node)| { node.value() == Some(key) }),
            "the owner SSH key remains visible in the production Confirm form"
        );
    }

    #[test]
    fn write_setup_missing_wifi_password_is_reported_before_identity_lookup() {
        let (mut app, mut doc) = seeded_app();
        app.select_target(0);
        app.goto(Step::Confirm);
        app.confirm_text = "sdb".into();
        Imager::paths()
            .profile
            .wifi_ssid
            .replace("studio")
            .apply_local(&mut doc)
            .unwrap();
        Imager::paths()
            .profile
            .enroll_local_identity
            .set(true)
            .apply_local(&mut doc)
            .unwrap();
        present(&mut app, &doc);
        let error = app
            .begin_write(&app.target().unwrap().dev_path(), "sdb")
            .expect_err("Wi-Fi without a password must be refused before enrollment");
        assert!(error.contains("Wi-Fi password is required"), "{error}");
        assert!(app.job_control.is_none());
        assert_eq!(app.step, Step::Confirm);
    }

    #[test]
    fn write_setup_uses_the_newly_published_document_without_a_render_frame() {
        use ui::tilo_resource::Resource as _;

        let (mut app, mut doc) = seeded_app();
        app.select_target(0);
        app.goto(Step::Confirm);
        app.confirm_text = "sdb".into();
        assert!(app.profile.wifi_ssid.is_empty());
        Imager::paths()
            .profile
            .wifi_ssid
            .replace("studio")
            .apply_local(&mut doc)
            .unwrap();
        assert!(
            app.profile.wifi_ssid.is_empty(),
            "the previous projection is still cached"
        );
        // Present the document through the actual ResourceSurface hook. No
        // layout, draw, animation tick or test-only profile assignment intervenes.
        app.present_state(&Imager::read(&doc).expect("the updated resource"));
        let error = app
            .begin_write(&app.target().unwrap().dev_path(), "sdb")
            .expect_err("the newly published network requires its password");
        assert!(error.contains("Wi-Fi password is required"), "{error}");
        assert_eq!(app.profile.wifi_ssid, "studio");
        assert!(app.job_control.is_none());
        assert_eq!(app.step, Step::Confirm);
    }

    /// Release download job presentation.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn release_download_is_local_visible_and_cancellable() {
        // Phase `active_release_download_replaces_selection_with_an_accessible_cancel_action`.
        {
            let (mut app, _doc) = seeded_app();
            app.release_download = Some(ReleaseDownload {
                index: 0,
                control: crate::release::DownloadControl::default(),
            });

            let release_names = app
                .releases
                .iter()
                .map(|release| release.display_name())
                .collect::<Vec<_>>();
            let mut view = harness(app, AutoCommit::new(), (1180, 820));
            assert!(
                view.a11y_nodes()
                    .iter()
                    .any(|(_, n)| n.label() == Some("Cancel download"))
            );
            let release_nodes = view
                .controls()
                .into_iter()
                .filter(|control| {
                    control
                        .name
                        .as_ref()
                        .is_some_and(|name| release_names.contains(name))
                })
                .collect::<Vec<_>>();
            assert_eq!(
                release_nodes.len(),
                release_names.len(),
                "every offered release has an actual accessible row"
            );
            assert_eq!(
                release_nodes
                    .iter()
                    .filter(|control| !control.disabled)
                    .count(),
                1,
                "only the active release remains enabled during its download"
            );
            let app = view.root_mut::<ImagerApp>();
            assert!(
                app.published_rows()
                    .iter()
                    .any(|row| row.id() == "imager.download"),
                "the download is published with its cancel"
            );

            app.cancel_release_download().unwrap();
            assert!(
                app.release_download
                    .as_ref()
                    .is_some_and(|download| download.control.is_cancelled())
            );
        }

        // Phase `release_progress_reads_as_a_status_and_the_end_clears_the_job`.
        {
            let (mut app, _doc) = seeded_app();
            let progress = JobProgress {
                phase: "downloading".into(),
                completed: Some(25),
                total: Some(100),
            };
            assert!(ImagerApp::download_status(&progress).contains("25%"));

            app.release_download = Some(ReleaseDownload {
                index: 0,
                control: crate::release::DownloadControl::default(),
            });
            app.finish_release_download(Err("Tilo installer download cancelled.".into()));
            assert!(app.release_download.is_none());
            assert!(app.status.contains("cancelled"));
            assert!(
                !app.published_rows()
                    .iter()
                    .any(|row| row.id() == "imager.download")
            );
        }
    }

    #[test]
    fn owner_ssh_key_can_be_typed_before_the_complete_key_is_validated() {
        let mut host =
            ui::prelude::TestApp::<crate::resource::ImagerShell>::capture(Some("confirm")).unwrap();
        host.harness()
            .perform_named(
                "imager.write-setup.ssh-key",
                &ui::a11y::Action::SetValue("ssh-".into()),
            )
            .unwrap();
        assert_eq!(
            host.with_widget::<ImagerApp, _>(|app| app.ssh_public_key.clone())
                .unwrap(),
            "ssh-"
        );
        host.with_widget::<ImagerApp, _>(|app| {
            let error = app
                .begin_write(&app.target().unwrap().dev_path(), "sdb")
                .expect_err("an incomplete key must refuse the write");
            assert!(error.contains("public key"), "unexpected refusal: {error}");
            assert!(app.job_control.is_none());
        })
        .expect("the hosted wizard");
    }

    /// The primary interaction, proven end to end by clicking the real
    /// on-screen targets: walk Source → Drive → Customize → Confirm. Every transition
    /// goes through the actual hit-testing + dispatch path, not a shortcut.
    #[test]
    #[cfg(not(feature = "host-media-writer"))]
    fn clicking_through_the_wizard_refuses_a_fake_write_when_backend_is_disabled() {
        let mut host =
            ui::prelude::TestApp::<crate::resource::ImagerShell>::capture(Some("source")).unwrap();
        host.harness().click_named("imager.next").unwrap();
        assert_eq!(
            host.with_widget::<ImagerApp, _>(|a| a.step).unwrap(),
            Step::Drive
        );
        host.harness().click_named("imager.drive.sdb").unwrap();
        assert_eq!(
            host.with_widget::<ImagerApp, _>(|a| a.selected).unwrap(),
            Some(0)
        );
        host.harness().click_named("imager.next").unwrap();
        assert_eq!(
            host.with_widget::<ImagerApp, _>(|a| a.step).unwrap(),
            Step::Customize
        );
        host.harness().click_named("imager.next").unwrap();
        assert_eq!(
            host.with_widget::<ImagerApp, _>(|a| a.step).unwrap(),
            Step::Confirm
        );
        host.harness()
            .perform_named("imager.confirm", &ui::a11y::Action::SetValue("sdb".into()))
            .unwrap();
        assert!(
            host.with_widget::<ImagerApp, _>(|a| a.confirm_matches())
                .unwrap()
        );
        use automerge::ReadDoc as _;
        assert!(
            host.harness()
                .state()
                .doc
                .get(automerge::ROOT, "confirm")
                .unwrap()
                .is_none(),
            "destructive consent never enters the shared document"
        );
        host.harness().click_named("imager.write").unwrap();
        host.with_widget::<ImagerApp, _>(|a| {
            assert_eq!(a.step, Step::Confirm);
            assert_eq!(a.progress.phase, JobPhase::Idle);
            assert!(a.job_control.is_none());
        })
        .unwrap();
    }

    #[test]
    #[cfg(not(feature = "host-media-writer"))]
    fn matching_confirmation_does_not_simulate_a_disabled_write() {
        if crate::write::allow_write() {
            return;
        }
        let (mut app, _doc) = seeded_app();
        app.select_target(0); // sdb
        app.goto(Step::Confirm);
        app.confirm_text = String::from("sdb");
        app.start_write();
        assert_eq!(app.progress.phase, JobPhase::Idle);
        assert_eq!(app.step, Step::Confirm);
        assert!(app.job_control.is_none());
        assert!(app.status.contains("disabled"));
    }

    /// Portal sources, external drives, rescans and forbidden disks.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn source_and_target_selection_accept_only_inspected_external_media() {
        // Phase `a_chosen_image_is_inspected_before_it_is_reviewed`.
        {
            let (mut app, _doc) = seeded_app();
            let f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
            std::fs::write(f.path(), b"tilo payload for the imager test").unwrap();
            app.take_chosen_image(Some(f.path().to_path_buf())).unwrap();
            assert_eq!(app.source.as_ref().unwrap().kind, SourceKind::IsoFile);
            app.take_chosen_image(None).unwrap();
            assert!(app.status.contains("cancelled"));
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
            let (mut app, _doc) = seeded_app();
            app.select_target(0);
            app.confirm_text = String::from("sdb");

            app.apply_drive_inventory(Vec::new());

            assert!(app.targets.is_empty());
            assert_eq!(app.selected, None);
            assert!(app.confirm_text.is_empty());
        }

        // Phase `rescan_preserves_only_the_same_physical_target`.
        {
            let (mut app, _doc) = seeded_app();
            app.select_target(0);
            app.confirm_text = String::from("sdb");
            let selected = app.target().unwrap().clone();
            let mut reordered = crate::model::sample_targets();
            reordered.swap(0, 1);

            app.apply_drive_inventory(reordered);

            assert_eq!(app.selected, Some(1));
            assert_eq!(
                app.target().and_then(|drive| drive.stable_id.as_deref()),
                selected.stable_id.as_deref()
            );
            assert_eq!(app.confirm_text.as_str(), "sdb");

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
            app.confirm_text = String::from(&unidentified_before.name);
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
            let mut targets = crate::model::sample_targets();
            targets[1].is_system = true; // the internal NVMe is the system disk
            app.targets = targets.clone();
            let doc = document(&Imager {
                source: Some(crate::model::sample_source()),
                ..Imager::default()
            });
            present(&mut app, &doc);

            app.select_target(1);
            assert_eq!(app.selected, None, "the system disk is never selectable");
            assert!(app.status.to_lowercase().contains("system disk"));
        }
    }

    /// Hostname typing, keyboard chooser and preview.
    #[test]
    fn profile_panel_edits_persist_through_the_real_event_path() {
        // Phase `preview_shows_the_embedded_profile`.
        {
            let (mut app, _doc) = seeded_app();
            app.select_target(1);
            app.export_config();
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
            let (mut app, _doc) = seeded_app();
            app.select_target(0);
            app.progress = Progress {
                phase: JobPhase::Success,
                write_frac: 1.0,
                verify_frac: 1.0,
                rate_bps: 0,
                error: None,
            };
            app.write_another();
            assert_eq!(app.step, Step::Source);
            assert_eq!(app.progress.phase, JobPhase::Idle);
        }

        // Phase `failed_write_keeps_its_cause_and_returns_to_a_fresh_review`.
        {
            let (mut app, _doc) = seeded_app();
            app.select_target(0);
            app.goto(Step::Write);
            app.progress = Progress {
                phase: JobPhase::Writing,
                write_frac: 0.4,
                verify_frac: 0.0,
                rate_bps: 0,
                error: None,
            };
            app.job_control = Some(JobControl::new());
            app.finish_write(Some(Progress {
                phase: JobPhase::Failed,
                write_frac: 0.4,
                verify_frac: 0.0,
                rate_bps: 0,
                error: Some(
                    "Could not write the drive: the selected drive was disconnected.".into(),
                ),
            }));
            assert_eq!(app.step, Step::Done);
            assert_eq!(app.progress.phase, JobPhase::Failed);
            assert!(
                app.status.contains("disconnected"),
                "the privileged writer's actionable cause reaches the UI"
            );
            let h = harness(app, AutoCommit::new(), (1180, 820));
            assert!(
                h.inspection_json().unwrap().contains("disconnected"),
                "the writer's actionable cause is exposed by the retained result"
            );
        }

        // Phase `active_write_cancel_operation_requests_a_safe_stop`.
        {
            let (mut app, _doc) = seeded_app();
            let control = JobControl::new();
            app.job_control = Some(control.clone());
            app.goto(Step::Write);

            app.cancel_write().unwrap();

            assert!(control.is_cancelled());
            assert!(app.status.contains("Keep the drive connected"));
            let mut h = harness(app, AutoCommit::new(), (1180, 820));
            h.expect(
                &ui::headless::SemanticExpectation::named("imager.cancel-write").enabled(false),
            )
            .unwrap();
        }

        // Phase `the_job_reports_bytes_of_the_active_phase_and_its_rate_follows`.
        {
            let (mut app, _doc) = seeded_app();
            let start = ui::time::now();
            let report = |phase: &str, completed| JobProgress {
                phase: phase.into(),
                completed: Some(completed),
                total: Some(1_000),
            };
            app.follow_write(&report("writing", 100), start);
            app.follow_write(
                &report("writing", 300),
                start + std::time::Duration::from_secs(2),
            );
            assert_eq!(app.progress.phase, JobPhase::Writing);
            assert!((app.progress.write_frac - 0.3).abs() < 1e-9);
            assert_eq!(app.progress.rate_bps, 100, "200 bytes in two seconds");
            app.follow_write(
                &report("verifying", 500),
                start + std::time::Duration::from_secs(3),
            );
            assert_eq!(app.progress.phase, JobPhase::Verifying);
            assert!((app.progress.write_frac - 1.0).abs() < 1e-9);
            assert!((app.progress.verify_frac - 0.5).abs() < 1e-9);

            app.job_control = Some(JobControl::new());
            app.finish_write(None);
            assert_eq!(app.progress.phase, JobPhase::Failed);
            assert!(app.status.contains("stopped unexpectedly"));
            assert!(app.job_control.is_none());
        }

        // Phase `cancel_wins_a_race_with_an_already_queued_success`.
        {
            let (mut app, _doc) = seeded_app();
            let control = JobControl::new();
            control.cancel();
            app.job_control = Some(control);
            app.progress.phase = JobPhase::Verifying;
            app.goto(Step::Write);
            app.finish_write(Some(Progress {
                phase: JobPhase::Success,
                write_frac: 1.0,
                verify_frac: 1.0,
                rate_bps: 0,
                error: None,
            }));

            assert_eq!(app.progress.phase, JobPhase::Failed);
            assert!(app.progress.error.as_deref().is_some_and(|message| {
                message.contains("cancelled") && message.contains("incomplete")
            }));
            assert_eq!(app.step, Step::Done);
        }
    }
}
