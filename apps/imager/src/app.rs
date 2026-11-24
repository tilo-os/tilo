//! The Disk Imager window: choose an image, choose a drive, write it, and let
//! go of it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

use automerge::ObjId;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect};
use winit::event::WindowEvent;

use ui::i18n::ArgValue;
use ui::icons::Icon;
use ui::local_state::LocalState;
use ui::portal::{FileFilter, PickRequest};

use ui::text_render::ParagraphAlign;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Banner, Button, ButtonRole, Column, ControlSize, DropZone, EmptyState, ListRow, ProgressRing,
    ProgressRingSize, Property, PropertyList, Row, Section, Spacer, Text, Tone, Widget,
};

use crate::export_dialog::ExportDialog;
use crate::hosting::{EffectAnswer, EffectCx, Effected, JobProgress, Opened, Port, Reply, Store};
use crate::model::{
    Action, InstallProfile, JobPhase, Progress, Source, SourceKind, Step, TargetDrive,
    TargetDriveIcon, TargetName, TiloRelease, fmt_eta, fmt_rate, fmt_size,
};
use crate::page::{self, Frame};
use crate::resource::CATALOG;
use crate::resource::{ImageWrite, Imager, ImagerEffect, InstallerDownload};
use crate::write::{JobControl, WriteRequest, spawn_job};
use tilo_resource::{EffectStatus, LiveRow};
use ui::widget::DrawChild;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Command {
    PickFile,
    Dropped(PathBuf),
    Release(usize),
    Recent(usize),
    Back,
    Station(usize),
    Next,
    CancelWrite,
    WriteAnother,
    Eject,
}

pub(crate) type Commands = Rc<RefCell<Vec<Command>>>;

pub(crate) fn command_button(commands: &Commands, id: &str, key: &str, command: Command) -> Button {
    let sink = Rc::clone(commands);
    Button::new(id, &CATALOG.t(key)).on_click(move || sink.borrow_mut().push(command.clone()))
}

struct PendingWriterResult {
    progress: Option<Progress>,
}

pub struct ImagerApp {
    source: Option<Source>,
    recent: Vec<Source>,
    profile: InstallProfile,
    /// The host session's Wi-Fi passphrase: it never enters the document.
    wifi_psk: String,
    ssh_public_key: LocalState<String>,
    targets: Vec<TargetDrive>,
    releases: Vec<TiloRelease>,
    selected: Option<usize>,
    action: Action,
    progress: Progress,
    ejected: bool,
    status: String,
    status_is_error: bool,
    step: Step,
    job_control: Option<JobControl>,
    pending_writer_result: Option<PendingWriterResult>,
    close_requested: bool,
    close_intent: bool,
    write_mark: Option<(u64, ui::time::Instant)>,
    write_reply: Option<Reply>,
    release_download: Option<ReleaseDownload>,
    export_dialog: ExportDialog,
    export_after_sync: bool,
    shot_confirmation_ready: bool,
    shot_timezone_open: bool,
    shot_focus: Option<&'static str>,
    startup_write_state_reconciled: bool,
    seeded: bool,
    composed: ui::widget::Composed,
    view_key: Option<String>,
    structure_dirty: bool,
    commands: Commands,
    scroll: Rc<Cell<(f32, f32)>>,
    port: Port<ImagerApp>,
    /// The scenario a shot shows, staged once the window first shows the resource.
    shot: Option<String>,
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
            dirty: true,
            source: None,
            recent: Vec::default(),
            targets: Vec::new(),
            releases: Vec::new(),
            selected: None,
            action: Action::Flash,
            profile: InstallProfile::default(),
            wifi_psk: String::new(),
            ssh_public_key: LocalState::new(String::new()),
            progress: Progress::default(),
            ejected: false,
            status: String::new(),
            status_is_error: false,
            step: Step::Source,
            job_control: None,
            pending_writer_result: None,
            close_requested: false,
            close_intent: false,
            write_mark: None,
            write_reply: None,
            release_download: None,
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
            port: Port::detached(),
            shot: None,
            live: false,
            reaches_machine: true,
        }
    }

    pub(crate) fn target(&self) -> Option<&TargetDrive> {
        self.selected.and_then(|i| self.targets.get(i))
    }
    fn releases(&self) -> &[TiloRelease] {
        &self.releases
    }

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

    fn can_advance(&self, from: Step) -> bool {
        self.can_advance_with_writer(from, self.writer().is_available())
    }

    fn can_advance_with_writer(&self, from: Step, writer_available: bool) -> bool {
        match from {
            Step::Source => self.source.is_some(),
            Step::Drive => self.selected.is_some() && self.image_fits(),
            Step::Customize => true,
            Step::Confirm => {
                self.startup_write_state_reconciled && self.image_fits() && writer_available
            }
            Step::Write | Step::Done => false,
        }
    }

    fn customizable(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|source| self.source_is_official(source))
    }

    fn needs_manual_customization(&self) -> bool {
        self.customizable()
    }

    fn offers_write_setup(&self) -> bool {
        self.source.is_some()
    }

    #[cfg(test)]
    pub(crate) fn ssh_key_for_test(&self) -> String {
        self.ssh_public_key.with(Clone::clone)
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

    pub(crate) fn adopt_resource_state(&mut self, state: &Imager) {
        self.source.clone_from(&state.source);
        self.recent = state.recent_sources();
        self.profile = state.profile.clone();

        self.dirty = true;
    }

    /// The writer on `port`, as `opened` says: reaching this computer's drives when live.
    pub(crate) fn opened(port: Port<Self>, opened: Opened<'_>) -> Self {
        let mut app = Self::new();
        app.port = port;
        app.attach(opened);
        app
    }

    /// Take up how the window is opened: a live writer reaches this computer's drives; a shot
    /// stages its scenario.
    pub(crate) fn attach(&mut self, opened: Opened<'_>) {
        self.live = self.reaches_machine && opened.is_live();
        if let Opened::Shot(scenario) = opened {
            self.shot = Some(scenario.unwrap_or("source").to_owned());
        }
        self.dirty = true;
    }

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
                let control = JobControl::new();
                if let Err(error) = control.bind_capture_worker() {
                    self.progress.phase = JobPhase::Failed;
                    self.progress.error = Some(error.to_string());
                }
                self.job_control = Some(control);
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
                self.step = Step::Drive;
            }
            _ => {}
        }
    }

    fn required_capacity(&self) -> Option<u64> {
        let source = self.source.as_ref()?;
        if source.size == 0 {
            return None;
        }
        // An official image also carries the install profile block at the drive's tail.
        let profile = if self.source_is_official(source) {
            imager_backend::media_model::PROFILE_BLOCK_SIZE as u64
        } else {
            0
        };
        source.size.checked_add(profile)
    }
    fn capacity_message(&self, drive: &TargetDrive) -> String {
        use crate::setup::strings::{t, t_args};
        self.required_capacity().map_or_else(
            || t("installer-media-capacity-checking"),
            |required| {
                t_args(
                    "installer-media-capacity-required",
                    &[
                        ("required", fmt_size(required).into()),
                        ("available", fmt_size(drive.size).into()),
                    ],
                )
            },
        )
    }
    fn image_fits(&self) -> bool {
        self.required_capacity()
            .zip(self.target())
            .is_some_and(|(bytes, drive)| drive.fits(bytes))
    }
}

fn step_message(step: Step) -> &'static str {
    match step {
        Step::Source => "imager-step-source",
        Step::Drive => "imager-step-drive",
        Step::Customize => "imager-step-customize",
        Step::Confirm => "imager-step-confirm",
        Step::Write => "imager-step-write",
        Step::Done => "imager-step-done",
    }
}

#[cfg(test)]
fn stepper_labels() -> [String; 6] {
    Step::all().map(|step| crate::setup::strings::t(step_message(step)))
}

fn target_name(release: &TiloRelease) -> String {
    match release.target_name() {
        TargetName::Arm64 => CATALOG.t("imager-release-target-arm64"),
        TargetName::X86_64 => CATALOG.t("imager-release-target-x86-64"),
        TargetName::Media => CATALOG.t("imager-release-target-media"),
        TargetName::Profile(profile) => CATALOG.t_args(
            "imager-release-target-profile",
            &[("profile", ui::i18n::ArgValue::Text(profile))],
        ),
    }
}

impl ImagerApp {
    /// A closure for a control's press that runs `effect`.
    fn presses(&self, effect: ImagerEffect) -> impl Fn() + use<> {
        self.port.presses().on(effect)
    }

    fn button(&self, id: &str, key: &str, command: Command) -> Button {
        command_button(&self.commands, id, key, command)
    }

    fn view_key(&self) -> String {
        let profile = match self.step {
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
            "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{:.0}|{:?}|{}|{}|{}|{}|{}|{}",
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
            self.customizable(),
            self.writer().is_available() || self.shot_confirmation_ready,
            self.shot_timezone_open,
            profile,
        )
    }

    fn refresh_view(&mut self) {
        let key = self.view_key();
        if self.view_key.as_deref() != Some(key.as_str()) {
            self.view_key = Some(key);
            self.structure_dirty = true;
        }
    }

    fn visible_steps(&self) -> Vec<Step> {
        Step::all().into_iter().collect()
    }

    fn stepper_current(&self) -> usize {
        let steps = self.visible_steps();
        if self.step == Step::Done && self.progress.phase == JobPhase::Success {
            steps.len()
        } else {
            steps
                .iter()
                .filter(|step| step.index() < self.step.index())
                .count()
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
        let mut stepper = ui::widget::Stepper::new(
            self.visible_steps()
                .iter()
                .map(|step| crate::setup::strings::t(step_message(*step)))
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

    fn footer(&self) -> Row {
        let mut row = Row::new().no_wrap();
        if matches!(self.step, Step::Drive | Step::Customize | Step::Confirm) {
            row = row.push(Box::new(
                self.button("imager.back", "installer-preview-back", Command::Back)
                    .with_icon(Icon::ChevronLeft)
                    .control_size(ControlSize::Large),
            ));
        }
        row = row.push(Box::new(Spacer::new()));
        let primary = match self.step {
            Step::Source | Step::Drive => self
                .button("imager.next", "installer-preview-continue", Command::Next)
                .role(ButtonRole::Primary)
                .with_enabled(self.can_advance(self.step)),
            Step::Customize => self
                .button("imager.next", "installer-preview-continue", Command::Next)
                .role(ButtonRole::Primary),
            Step::Confirm => {
                let write = Button::new("imager.write", &CATALOG.t("imager-write"))
                    .role(ButtonRole::Destructive)
                    .with_accessible_name(&self.target().map_or_else(
                        || CATALOG.t("imager-write-driving"),
                        |drive| {
                            CATALOG.t_args(
                                "imager-a11y-erase-drive",
                                &[("drive", ArgValue::Text(drive.dev_path()))],
                            )
                        },
                    ));
                // The Confirm step is the confirmation: the write names the drive it erases.
                match self.target() {
                    Some(drive) => write
                        .on_click(self.presses(ImagerEffect::Write {
                            device: drive.dev_path(),
                            confirmation: drive.name.clone(),
                        }))
                        .with_enabled(
                            self.can_advance(Step::Confirm) || self.shot_confirmation_ready,
                        ),
                    None => write.with_enabled(false),
                }
            }
            Step::Write => {
                let stopping = self
                    .job_control
                    .as_ref()
                    .is_some_and(JobControl::is_cancelled);
                self.button(
                    "imager.cancel-write",
                    if stopping {
                        "imager-write-cancelling"
                    } else {
                        "imager-write-cancel"
                    },
                    Command::CancelWrite,
                )
                .with_enabled(self.job_control.is_some() && !stopping)
            }
            Step::Done => {
                if self.offers_eject() {
                    let eject = self.button("imager.eject", "imager-eject", Command::Eject);
                    row = row.push(Box::new(eject));
                }
                self.button(
                    "imager.start-over",
                    if self.progress.phase == JobPhase::Failed {
                        "imager-review-and-retry"
                    } else {
                        "imager-start-over"
                    },
                    Command::WriteAnother,
                )
                .role(ButtonRole::Primary)
            }
        };
        row.push(Box::new(primary.control_size(ControlSize::Large)))
    }

    fn offers_eject(&self) -> bool {
        self.progress.phase == JobPhase::Success
            && !self.ejected
            && self.target().is_some()
            && imager_backend::write::can_eject()
    }

    fn source_listed(&self, source: &Source) -> bool {
        self.releases().iter().any(|r| r.matches_source(source))
            || self.shown_recent().iter().any(|i| {
                self.recent
                    .get(*i)
                    .is_some_and(|r| r.path == source.path && r.name == source.name)
            })
    }

    fn shown_recent(&self) -> Vec<usize> {
        self.recent
            .iter()
            .enumerate()
            .filter(|(_, recent)| !self.releases().iter().any(|r| r.matches_source(recent)))
            .map(|(i, _)| i)
            .take(3)
            .collect()
    }

    fn source_secondary(&self, source: &Source) -> String {
        let message = if self.source_is_official(source) {
            "installer-image-official"
        } else {
            "installer-image-local"
        };
        CATALOG.t(message)
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
        let mut body =
            page::body(&crate::setup::strings::t("imager-page-choose-image")).push(Box::new(
                DropZone::new("imager.drop", crate::setup::strings::t("imager-drop-title"))
                    .message(crate::setup::strings::t("imager-drop-message"))
                    .icon(Icon::Download)
                    .with_action(self.button("imager.browse", "imager-browse", Command::PickFile))
                    .on_files(move |files| {
                        if let Some(file) = files.into_iter().next() {
                            sink.borrow_mut().push(Command::Dropped(file));
                        }
                    }),
            ));
        if let Some(source) = &self.source
            && !self.source_listed(source)
        {
            let secondary = self.source_secondary(source);
            body = body.push(Box::new(Section::titled(
                CATALOG.t("imager-section-selected-image"),
                vec![Box::new(
                    ListRow::new("imager.selected", &source.name)
                        .icon(Icon::Disc3)
                        .secondary(&secondary)
                        .meta(fmt_size(source.size))
                        .selected(true)
                        .checked(true),
                )],
            )));
        }
        let releases: Vec<Box<dyn Widget>> = if self.releases().is_empty() {
            vec![Box::new(
                Text::new(&crate::setup::strings::t("imager-no-releases")).dimmed(),
            )]
        } else {
            (0..self.releases().len())
                .filter_map(|index| self.release_row(index))
                .collect()
        };
        body = body.push(Box::new(Section::titled(
            CATALOG.t("imager-section-releases"),
            releases,
        )));
        let recent = self.shown_recent();
        if !recent.is_empty() {
            let rows = recent
                .into_iter()
                .filter_map(|index| {
                    let source = self.recent.get(index)?;
                    let sink = Rc::clone(&self.commands);
                    let secondary = self.source_secondary(source);
                    Some(Box::new(
                        ListRow::new(format!("imager.recent.{index}"), &source.name)
                            .icon(Icon::Disc3)
                            .secondary(&secondary)
                            .meta(fmt_size(source.size))
                            .selected(self.recent_selected(index))
                            .checked(self.recent_selected(index))
                            .on_activate(move || sink.borrow_mut().push(Command::Recent(index))),
                    ) as Box<dyn Widget>)
                })
                .collect();
            body = body.push(Box::new(Section::titled(
                CATALOG.t("imager-section-recent"),
                rows,
            )));
        }
        body
    }

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
                Some((done, total)) if total > 0 => CATALOG.t_args(
                    "imager-release-progress",
                    &[
                        ("stage", ui::i18n::ArgValue::Text(stage.to_string())),
                        (
                            "percent",
                            ui::i18n::ArgValue::Number((done * 100 / total) as f64),
                        ),
                    ],
                ),
                _ => stage.to_string(),
            }
        } else if release.local_path.is_empty() {
            CATALOG.t_args(
                "imager-release-channel-downloads-first",
                &[
                    ("target", ui::i18n::ArgValue::Text(target_name(release))),
                    ("channel", ui::i18n::ArgValue::Text(release.channel.clone())),
                ],
            )
        } else {
            CATALOG.t_args(
                "imager-release-channel",
                &[
                    ("target", ui::i18n::ArgValue::Text(target_name(release))),
                    ("channel", ui::i18n::ArgValue::Text(release.channel.clone())),
                ],
            )
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
        .with_enabled(self.release_download.is_none() || downloading);
        row = if downloading {
            row.on_activate(self.presses(ImagerEffect::CancelDownload))
                .action(
                    Icon::X,
                    CATALOG.t("imager-release-cancel"),
                    self.presses(ImagerEffect::CancelDownload),
                )
        } else {
            let sink = Rc::clone(&self.commands);
            row.on_activate(move || sink.borrow_mut().push(Command::Release(index)))
        };
        Some(Box::new(row))
    }

    fn drive_body(&self) -> Column {
        let body = page::body(&crate::setup::strings::t("imager-page-choose-drive")).push(
            Box::new(Text::new(&self.required_capacity().map_or_else(
                || crate::setup::strings::t("installer-media-capacity-checking"),
                |required| {
                    crate::setup::strings::t_args(
                        "installer-media-capacity-estimate",
                        &[("required", fmt_size(required).into())],
                    )
                },
            ))),
        );
        let visible = self.visible_targets();
        if visible.is_empty() {
            return body.push(Box::new(
                EmptyState::new(crate::setup::strings::t("imager-empty-drives"))
                    .with_icon(Icon::HardDrive),
            ));
        }
        let required = self.required_capacity();
        let rows = visible
            .into_iter()
            .map(|(index, drive)| {
                let fits = required.is_some_and(|required| drive.fits(required));
                let contents = crate::contents::drive_contents(
                    drive.partitions.len(),
                    &drive.partition_names(),
                );
                let secondary = match required.filter(|required| !drive.fits(*required)) {
                    Some(required) => crate::setup::strings::t_args(
                        "installer-media-drive-too-small",
                        &[
                            ("contents", contents.into()),
                            ("required", fmt_size(required).into()),
                        ],
                    ),
                    None => ui::format::list(&[drive.dev_path(), contents]),
                };
                let row = ListRow::new(format!("imager.drive.{}", drive.name), &drive.model)
                    .with_enabled(fits)
                    .icon(drive.icon())
                    .secondary(secondary)
                    .meta(fmt_size(drive.size))
                    .selected(self.selected == Some(index))
                    .checked(self.selected == Some(index));
                let row = row.on_activate(self.presses(ImagerEffect::ChooseDrive {
                    drive: drive.name.clone(),
                }));
                Box::new(row) as Box<dyn Widget>
            })
            .collect();
        body.push(Box::new(Section::new(rows)))
    }

    fn customize_body(&self) -> Column {
        let mut body = page::body(&crate::setup::strings::t("imager-page-prepare-setup"));
        if let Some(source) = &self.source {
            body = body.push(Box::new(
                Text::new(&CATALOG.t_args(
                    "imager-selected-image",
                    &[("name", ArgValue::Text(source.name.clone()))],
                ))
                .with_role(TextRole::Caption),
            ));
        }
        body = body.push(Box::new(
            Button::new(
                "imager.export-config",
                &CATALOG.t("imager-preview-embedded"),
            )
            .on_click(self.presses(ImagerEffect::ExportConfig)),
        ));
        if !self.needs_manual_customization() {
            return body.push(Box::new(
                Text::new(&crate::setup::strings::t("imager-plain-body")).dimmed(),
            ));
        }
        body.push(page::form_block(crate::customize::form(
            &self.profile,
            Some(self.port.store()),
            self.shot_timezone_open,
        )))
    }

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
        if let Some(target) = self.target() {
            rows.push(("Space", self.capacity_message(target)));
        }
        rows.push(("Setup", self.review_setup()));
        rows
    }

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

    fn confirm_warning(&self) -> (String, String) {
        let t = crate::setup::strings::t;
        let device = self
            .target()
            .map_or_else(|| t("imager-drive-unnamed"), TargetDrive::dev_path);
        let system = self.target().is_some_and(|d| d.is_system);
        let internal = self.target().is_some_and(|d| !d.removable);
        let title = if system {
            t("imager-confirm-system-disk")
        } else if internal {
            crate::setup::strings::t_args(
                "imager-confirm-internal-disk",
                &[("device", ArgValue::Text(device))],
            )
        } else {
            crate::setup::strings::t_args(
                "imager-confirm-disk",
                &[("device", ArgValue::Text(device))],
            )
        };
        let message = if system {
            t("imager-confirm-system-consequence")
        } else {
            t("imager-confirm-consequence")
        };
        (title, message)
    }

    fn confirm_body(&self) -> Column {
        let title_key = match self.action {
            Action::Install => "imager-page-review-install",
            Action::Flash => "imager-page-review-write",
        };
        let title = crate::setup::strings::t(title_key);
        let review = self
            .review_rows()
            .into_iter()
            .map(|(label, value)| Property::new(label, value))
            .collect();
        let mut body = page::body(&title).push(Box::new(PropertyList::new(review)));
        if self.offers_write_setup() {
            body = body.push(page::form_block(crate::write_setup::form(
                &self.profile,
                &self.wifi_psk,
                &self.ssh_public_key,
                Some(&crate::write_setup::Bound {
                    store: self.port.store().clone(),
                    presses: self.port.presses(),
                }),
            )));
        }
        let (warning, consequence) = self.confirm_warning();
        let consequence = if self.writer().is_available() || self.shot_confirmation_ready {
            consequence
        } else {
            crate::setup::strings::t("imager-writer-unavailable")
        };
        body.push(Box::new(
            page::related().push(Box::new(
                Banner::error(warning)
                    .with_message(consequence)
                    .arrives_in_place(),
            )),
        ))
    }

    fn phase(&self) -> (f64, String) {
        match (self.progress.phase, self.action) {
            (JobPhase::Verifying, _) => (
                self.progress.verify_frac,
                crate::setup::strings::t("imager-phase-verifying"),
            ),
            (_, Action::Install) => (
                self.progress.write_frac,
                crate::setup::strings::t("imager-phase-installing"),
            ),
            (_, Action::Flash) => (
                self.progress.write_frac,
                crate::setup::strings::t("imager-phase-writing"),
            ),
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
            meta.push(crate::setup::strings::t_args(
                "imager-writing-eta",
                &[("time", ui::i18n::ArgValue::Text(fmt_eta(eta)))],
            ));
        }
        let next = CATALOG.t(if self.progress.phase == JobPhase::Verifying {
            "imager-writing-verify-now"
        } else {
            "imager-writing-verify-next"
        });
        let mut lines = page::related();
        if !meta.is_empty() {
            lines = lines.push(Box::new(
                Self::centred(&meta.join(", "), TextRole::Body).numeric(),
            ));
        }
        lines = lines.push(Box::new(Self::centred(&next, TextRole::Body).dimmed()));
        let mut body = page::stack()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .push(Box::new(Self::ring_row(ring)))
            .push(Box::new(lines));
        if let (Some(src), Some(tgt)) = (&self.source, self.target()) {
            body = body.push(Box::new(
                Self::centred(
                    &CATALOG.t_args(
                        "imager-writing-from-to",
                        &[
                            ("source", ui::i18n::ArgValue::Text(src.name.clone())),
                            ("target", ui::i18n::ArgValue::Text(tgt.model.clone())),
                        ],
                    ),
                    TextRole::Caption,
                )
                .dimmed(),
            ));
        }
        body
    }

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
                if self.offers_eject() {
                    format!(
                        "{image} was written to {drive} and verified. Eject it before you take it out."
                    )
                } else {
                    format!(
                        "{image} was written to {drive} and verified. You can safely remove the drive."
                    )
                },
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
                    .caption(crate::setup::strings::t("installer-state-verified"))
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
                    .caption(crate::setup::strings::t("installer-state-stopped"))
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
    fn submit(&mut self, invoke: tilo_resource::Invoke<Imager>) {
        if let Err(reason) = self.port.store().apply(invoke) {
            self.fail(&reason);
        }
    }
    fn select_source(&mut self, src: &Source) {
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
        if let Some(source) = imager_backend::probe::inspect_source(&path) {
            self.select_source(&source);
            Ok(Effected::done())
        } else {
            let message = crate::setup::strings::t("imager-image-unreadable");
            self.set_status(&message);
            Err(message)
        }
    }
    fn download_release(&mut self, index: usize, release: TiloRelease) {
        self.dirty = true;
        if self.release_download.is_some() {
            self.set_status("A Tilo installer download is already in progress.");
            return;
        }
        self.set_status(&format!(
            "Downloading and verifying {}…",
            release.display_name()
        ));
        let control = crate::release::DownloadControl::default();
        let worker = control.clone();
        self.port.start_job(
            RELEASE_DOWNLOAD,
            move |progress| {
                crate::release::verified_image_observed(&release, &worker, &mut |event| match event
                {
                    crate::release::DownloadEvent::Downloading { received, total } => {
                        progress.report("downloading", Some(received), Some(total));
                    }
                    crate::release::DownloadEvent::Verifying => {
                        progress.report("verifying", None, None);
                    }
                    crate::release::DownloadEvent::Expanding { written, total } => {
                        progress.report("preparing image", Some(written), Some(total));
                    }
                })
            },
            |app: &mut Self, result| app.finish_release_download(result),
        );
        self.release_download = Some(ReleaseDownload { index, control });
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
        self.port.job(RELEASE_DOWNLOAD)
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
        if !drive.is_media_target() {
            self.set_status(
                "Internal and system disks can only be installed from the booted Tilo medium.",
            );
            self.dirty = true;
            return;
        }
        if self
            .required_capacity()
            .is_none_or(|required| !drive.fits(required))
        {
            let message = self.capacity_message(drive);
            self.set_status(&message);
            self.dirty = true;
            return;
        }
        self.selected = Some(idx);
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
                if self.needs_manual_customization() {
                    self.goto(Step::Customize);
                } else {
                    self.goto(Step::Confirm);
                }
            }
            Step::Customize => match self.profile.validate_media_profile() {
                Ok(()) => self.goto(Step::Confirm),
                Err(message) => self.set_status(&message),
            },
            _ => {}
        }
    }
    fn back(&mut self) {
        match self.step {
            Step::Confirm if self.needs_manual_customization() => self.goto(Step::Customize),
            Step::Confirm | Step::Customize => self.goto(Step::Drive),
            Step::Drive => self.goto(Step::Source),
            Step::Source | Step::Write | Step::Done => {}
        }
    }
    fn writer_refusal(&self) -> Result<(), (&'static str, String)> {
        if self.close_requested {
            return Err((
                "window_closing",
                crate::setup::strings::t("imager-write-stopping"),
            ));
        }
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
        let target = self
            .media_target(device)
            .and_then(|index| self.targets.get(index))
            .ok_or("Select an image source and target drive before writing")?;
        let required = self.required_capacity().ok_or_else(|| {
            crate::setup::strings::t(
                if self.source.as_ref().is_none_or(|source| source.size == 0) {
                    "installer-media-capacity-not-ready"
                } else {
                    "installer-media-capacity-invalid"
                },
            )
        })?;
        if !target.fits(required) {
            return Err(self.capacity_message(target));
        }
        self.writer_refusal().map_err(|(_, reason)| reason)?;
        Ok(())
    }
    fn cancel_write(&mut self) -> Result<(), String> {
        let Some(control) = self.job_control.clone() else {
            return Err("No image write is currently running".into());
        };
        control.cancel();
        self.set_status(&crate::setup::strings::t("imager-write-stopping"));
        self.dirty = true;
        Ok(())
    }
    pub(crate) fn write_request(
        &self,
        source: Source,
        target: TargetDrive,
        handoff_stage: Option<PathBuf>,
    ) -> Result<WriteRequest, String> {
        if self.source_is_official(&source) {
            self.profile.validate_media_profile()?;
        }
        Ok(WriteRequest {
            source,
            target,
            action: self.action,
            profile: self.profile.clone(),
            handoff_stage,
        })
    }

    fn begin_write(&mut self, device: &str, confirmation: &str) -> Result<(), String> {
        if !self.startup_write_state_reconciled {
            return Err("Restoring the previous image-write state; wait for drive review before starting another write".into());
        }
        let (Some(mut src), Some(tgt)) = (self.source.clone(), self.target().cloned()) else {
            return Err("Select a source and drive first.".into());
        };
        src.installable = self.source_is_official(&src);
        if !src.installable {
            src.kind = SourceKind::IsoFile;
            src.expected_sha256 = None;
        }
        if src.installable {
            self.profile.validate_media_profile()?;
        }
        if self
            .required_capacity()
            .is_none_or(|required| !tgt.fits(required))
        {
            return Err(self.capacity_message(&tgt));
        }
        if device != tgt.dev_path() {
            return Err("The selected target changed; review the write plan again.".into());
        }
        let ssh_public_key = self.ssh_public_key.with(Clone::clone);
        if !self.profile.wifi_ssid.trim().is_empty() && self.wifi_psk.is_empty() {
            return Err("A Wi-Fi password is required when a network name is set.".into());
        }
        crate::handoff::validate_bringup_ssh_key(&ssh_public_key)?;
        self.writer_refusal().map_err(|(_, reason)| reason)?;
        let handoff_stage = if self.profile.enroll_local_identity {
            let local = crate::handoff::local_identity_from_env()?;
            let staged =
                crate::handoff::stage_setup_handoff(&self.profile, &self.wifi_psk, &local)?;
            if let Some(stage) = staged.as_ref()
                && let Err(error) =
                    crate::handoff::install_bringup_ssh_key(stage.path(), &ssh_public_key)
            {
                let _ = std::fs::remove_dir_all(stage.path());
                return Err(error);
            }
            staged.map(|stage| stage.root)
        } else {
            crate::handoff::stage_access_handoff(
                &self.profile,
                &self.wifi_psk,
                &ssh_public_key,
                !src.installable,
            )?
            .map(|stage| stage.root)
        };
        let src_size = src.size;
        let req = self.write_request(src, tgt.clone(), handoff_stage.clone())?;
        let (start, rx, control) = spawn_job(req, confirmation, self.writer());
        let refuse = |message: String| -> Result<(), String> {
            if let Some(stage) = handoff_stage.as_ref() {
                let _ = std::fs::remove_dir_all(stage);
            }
            Err(message)
        };
        match start {
            crate::write::JobStart::Started => {
                let (Some(rx), Some(control)) = (rx, control) else {
                    return refuse("The writer could not start in this window.".into());
                };
                Self::start_write_job(&self.port, rx, control.clone(), src_size);
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
        port: &Port<Self>,
        events: std::sync::mpsc::Receiver<Progress>,
        control: JobControl,
        size: u64,
    ) {
        // The guard exists before the worker first runs: a job dropped before it is polled must
        // also cancel its native writer.
        struct WriteJobOwnership(JobControl);
        impl Drop for WriteJobOwnership {
            fn drop(&mut self) {
                if !self.0.observed_stopped() {
                    self.0.cancel();
                }
            }
        }
        let ownership = WriteJobOwnership(control);
        port.start_job(
            WRITE_JOB,
            move |progress| {
                let mut last = None;
                let mut ended = false;
                let mut disconnected = false;
                loop {
                    if !ended && !disconnected {
                        loop {
                            let event = match events.try_recv() {
                                Ok(event) => event,
                                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                    disconnected = true;
                                    break;
                                }
                            };
                            let fraction = match event.phase {
                                JobPhase::Verifying => event.verify_frac,
                                _ => event.write_frac,
                            };
                            progress.report(
                                event.phase.key(),
                                Some((fraction.clamp(0.0, 1.0) * size as f64) as u64),
                                Some(size),
                            );
                            ended = matches!(event.phase, JobPhase::Success | JobPhase::Failed);
                            last = Some(event);
                            if ended {
                                break;
                            }
                        }
                    }
                    // Progress is not completion: the job ends only after the same native stop
                    // proof that guards the window's state and its close.
                    if (ended || disconnected) && ownership.0.observed_stopped() {
                        return last;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
            },
            |app: &mut Self, last| app.finish_write(last),
        );
    }
    fn drain_job(&mut self) {
        if self
            .job_control
            .as_ref()
            .is_some_and(JobControl::observed_stopped)
            && let Some(last) = self.pending_writer_result.take()
        {
            self.finish_write(last.progress);
        }

        let Some(progress) = self.port.job(WRITE_JOB) else {
            return;
        };
        self.follow_write(&progress, ui::time::now());
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
        self.dirty = true;
        // A terminal progress packet can precede helper wait and native exit.
        // Keep the cancellation channel and authority until that owned proof.
        if self
            .job_control
            .as_ref()
            .is_some_and(|control| !control.observed_stopped())
        {
            self.pending_writer_result = Some(PendingWriterResult { progress: last });
            return;
        }
        self.pending_writer_result = None;

        let cancelled = self
            .job_control
            .as_ref()
            .is_some_and(JobControl::is_cancelled);
        let failed = self
            .job_control
            .as_ref()
            .is_some_and(JobControl::observed_failure);
        self.job_control = None;
        self.write_mark = None;
        let mut outcome = last
            .filter(|last| !last.phase.is_active())
            .unwrap_or_else(|| Progress {
                phase: JobPhase::Failed,
                error: Some(crate::setup::strings::t("imager-write-worker-stopped")),
                ..self.progress.clone()
            });
        if failed {
            if outcome.phase == JobPhase::Success || outcome.error.is_none() {
                outcome.error = Some(crate::setup::strings::t("imager-write-worker-stopped"));
            }
            outcome.phase = JobPhase::Failed;
        }
        // Cancellation may race with a terminal helper event already in
        // the pipe. Honour the user's stop request conservatively: never
        // turn that race into a claim that the medium is ready to use.
        if outcome.phase == JobPhase::Success && cancelled {
            outcome.phase = JobPhase::Failed;
            outcome.error = Some(crate::setup::strings::t("imager-write-cancelled"));
        }
        let name = self
            .source
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let message = if outcome.phase == JobPhase::Success {
            crate::setup::strings::t_args(
                match self.action {
                    Action::Install => "imager-install-done",
                    Action::Flash => "imager-write-done",
                },
                &[("name", ArgValue::Text(name))],
            )
        } else {
            outcome.error.clone().unwrap_or_else(|| {
                crate::setup::strings::t_args(
                    match self.action {
                        Action::Install => "imager-install-failed",
                        Action::Flash => "imager-write-failed",
                    },
                    &[("name", ArgValue::Text(name))],
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
        let mut status = serde_json::json!({
            "phase": self.progress.phase.key(),
            "progress": self.progress.overall(),
            "message": self.status,
        });
        status["capacity"] = serde_json::json!({"state":if self.required_capacity().is_some(){"ready"}else{"checking"},"required":self.required_capacity(),"available":self.target().map(|drive|drive.size),"estimated":true});
        status
    }
    fn write_another(&mut self) {
        if self.progress.phase == JobPhase::Failed {
            self.progress = Progress::default();
            self.goto(Step::Confirm);
            self.set_status("Review the image and drive, then confirm the target again to retry.");
            return;
        }
        self.progress = Progress::default();
        self.ejected = false;
        self.goto(Step::Source);
        self.set_status("");
    }
    fn eject_written_drive(&mut self) {
        let Some(target) = self.target().cloned() else {
            return;
        };
        match imager_backend::write::eject_target(&target) {
            Ok(()) => {
                self.status_is_error = false;
                self.set_status(&crate::setup::strings::t_args(
                    "installer-media-ejected",
                    &[("drive", target.model.clone().into())],
                ));
                self.ejected = true;
            }
            Err(error) => self.fail(&crate::setup::strings::t_args(
                "installer-media-eject-failed",
                &[("reason", error.to_string().into())],
            )),
        }
        self.dirty = true;
    }

    fn export_config(&mut self) {
        let text = self.profile.to_headless_config();
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
            let Some(t) = imager_backend::probe::probe() else {
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
        cx: &mut EffectCx<'_, Self>,
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
                let path = |path: tilo_resource::AbsolutePath| path.to_string_lossy();
                let mut args = vec!["imager".into(), "--customize-image".into(), path(image)];
                for (flag, value) in [
                    ("--hostname", hostname),
                    ("--wifi-ssid", wifi_ssid),
                    ("--wifi-psk-file", wifi_psk_file.map(path)),
                    ("--ssh-keys-file", ssh_keys_file.map(path)),
                ] {
                    if let Some(value) = value {
                        args.extend([flag.into(), value]);
                    }
                }
                for file in home_files {
                    args.extend(["--home-file".into(), path(file)]);
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
                let text = self.profile.to_headless_config();
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
                self.ssh_public_key.set(value);
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
        rows.push(LiveRow::typed::<ImageWrite>(
            "imager.job",
            format!("Image write: {phase}"),
        ));
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
                LiveRow::typed::<InstallerDownload>("imager.download", format!("Download {name}"))
                    .presentation(tilo_actions::ResourcePresentation {
                        detail: Some(detail),
                        ..tilo_actions::ResourcePresentation::default()
                    }),
            );
        }
        rows
    }
    fn target_row(&self, target: &TargetDrive) -> LiveRow {
        rows::target_row(target)
    }
}

impl ImagerApp {
    pub(crate) fn present_state(&mut self, state: &Imager) {
        self.adopt_resource_state(state);
    }
}

impl crate::hosting::Window for ImagerApp {
    type Effect = ImagerEffect;

    fn port(&self) -> &Port<Self> {
        &self.port
    }

    fn effect(&mut self, effect: ImagerEffect, cx: &mut EffectCx<'_, Self>) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn refused(&mut self, reason: &str) {
        self.fail(reason);
    }

    fn state(&self) -> serde_json::Value {
        self.status_value()
    }

    fn rows(&self) -> Vec<LiveRow> {
        self.published_rows()
    }

    fn statuses(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    fn row_statuses(&self) -> Vec<(String, Vec<EffectStatus>)> {
        rows::target_row_statuses(self)
    }

    fn present(&mut self, state: &Imager) {
        self.present_state(state);
    }
}

/// The image writer's window on `store`, as `opened` says.
pub(crate) fn window(store: &Store, wake: &ui::WakeHandle, opened: Opened<'_>) -> Box<dyn Widget> {
    Box::new(crate::hosting::Hosted(ImagerApp::opened(
        Port::new(store.clone(), wake.clone()),
        opened,
    )))
}

/// The USB media writer the guided setup hands its reviewed names to: live when the setup is.
#[cfg(feature = "installation")]
pub(crate) fn media_writer(port: Port<ImagerApp>, live: bool) -> Box<dyn Widget> {
    let mut app = ImagerApp::new();
    app.port = port;
    app.live = app.reaches_machine && live;
    app.dirty = true;
    Box::new(crate::hosting::Hosted(app))
}

/// The sample image writer a test opens, as a person opens it.
#[cfg(test)]
pub(crate) fn sample_host() -> crate::hosting::testing::TestHost {
    crate::hosting::testing::TestHost::hosting(|store, wake| {
        let mut app = ImagerApp::sample();
        app.port = Port::new(store.clone(), wake.clone());
        app.attach(Opened::Live { demo: false });
        Box::new(crate::hosting::Hosted(app))
    })
}

/// The scenarios a shot of the image writer shows.
pub(crate) const SCENARIOS: &[&str] = &[
    "source",
    "source-empty",
    "drive",
    "drive-empty",
    "drive-internal",
    "customize",
    "customize-timezone-open",
    "export",
    "confirm",
    "progress",
    "done",
    "done-failed",
];

impl ImagerApp {
    fn go_to_station(&mut self, index: usize) {
        if let Some(step) = self.visible_steps().get(index).copied() {
            if step == Step::Customize && !self.needs_manual_customization() {
                return;
            }
            if step.index() < self.step.index() && self.step != Step::Write {
                self.goto(step);
            }
        }
    }
    fn apply_command(&mut self, command: Command) {
        match command {
            Command::PickFile => {
                self.port.presses().press(ImagerEffect::ChooseImage);
            }
            Command::Dropped(path) => match imager_backend::probe::inspect_source(&path) {
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
            Command::Recent(index) => {
                if let Some(source) = self.recent.get(index).cloned() {
                    self.select_source(&source)
                }
            }
            Command::Back => self.back(),
            Command::Station(index) => self.go_to_station(index),
            Command::Next => self.advance(),
            Command::CancelWrite => {
                let _ = self.cancel_write();
            }
            Command::WriteAnother => self.write_another(),
            Command::Eject => self.eject_written_drive(),
        }
    }
    fn fail(&mut self, message: &str) {
        self.status_is_error = true;
        self.set_status(message);
        self.dirty = true;
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
impl ImagerApp {
    fn close_work_active(&self) -> bool {
        self.job_control.is_some()
    }
}
impl ui::component_lifecycle::ComponentLifecycle for ImagerApp {
    fn on_window_close_requested(&mut self) -> ui::widget::WindowCloseAdmission {
        if !self.close_work_active() {
            return ui::widget::WindowCloseAdmission::Allow;
        }
        self.close_requested = true;
        self.close_intent = false;
        if let Some(control) = &self.job_control {
            control.cancel();
        }
        self.set_status(&crate::setup::strings::t("imager-write-stopping"));
        self.dirty = true;
        ui::widget::WindowCloseAdmission::Defer
    }
    fn take_window_close_intent(&mut self) -> bool {
        let intent = std::mem::take(&mut self.close_intent);
        if intent {
            self.close_requested = false;
        }
        intent
    }
    fn close_components(&mut self) -> Vec<ui::application::ComponentCloseReport> {
        // Host teardown has stopped frames. This synchronous path also serves
        // headless component removal; normal close first defers above.
        self.close_requested = true;
        let mut errors = Vec::new();
        if let Some(control) = self.job_control.take() {
            control.cancel();
            if let Err(error) = control.wait_stopped() {
                errors.push(error.to_string());
            }
        }
        let mut reports: Vec<_> = self
            .children_mut()
            .into_iter()
            .flat_map(ui::component_lifecycle::close_components)
            .collect();
        for error in errors {
            reports.push(ui::application::ComponentCloseReport {
                recovery: None,
                view_error: None,
                view_retry: None,
                resource_error: None,
                record_retry: None,
                record_retry_target: None,
                teardown_error: Some(error),
            });
        }
        reports
    }
}
impl Widget for ImagerApp {
    ui::provides!(dyn ui::component_lifecycle::ComponentLifecycle);

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.dirty = true;
        if let Some(scenario) = self.shot.take() {
            self.stage_scenario(&scenario);
        }
        self.take_window_toolbar_events();
        if !self.seeded {
            if self.live {
                self.targets = imager_backend::probe::probe().unwrap_or_default();
            }
            if self.live {
                self.reconcile_persisted_write_state();
            } else {
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

        if let Some(progress) = self.release_progress() {
            self.status = Self::download_status(&progress);
        }
        self.drain_job();

        if self.close_requested && !self.close_work_active() {
            self.close_intent = true;
        }

        self.action = Action::Flash;
        self.selected = self.selected.filter(|i| {
            self.targets
                .get(*i)
                .is_some_and(TargetDrive::is_media_target)
        });

        if self.export_after_sync {
            self.export_after_sync = false;
            self.export_config();
        }
        self.refresh_view();
        self.declare_window_toolbar();
        self.composed.sync(_state, _path);
        self.dirty = true;
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let asked = self.take_window_toolbar_events();
        if asked {
            self.drain_commands();
        }
        self.refresh_view();
        self.declare_window_toolbar();
        let changed = self
            .composed
            .element_mut()
            .is_some_and(|view| view.update(state, path));
        changed || asked
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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

    fn draw(&mut self, ctx: &mut ui::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if self.export_dialog.is_open() {
            self.export_dialog
                .layout_in(self.composed.rect(), theme, text_renderer);
        }
        if let Some(composed) = self.composed.element_mut() {
            composed.draw_child(scene, transform, text_renderer, theme, &env);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.dirty = true;
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

    fn draw_overlay(&mut self, ctx: &mut ui::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        if self.export_dialog.is_open() {
            self.export_dialog.draw_detached(&mut ui::DrawCtx::new(
                scene,
                transform,
                text,
                theme,
                env.clone(),
            ));
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

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
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

    fn needs_animation_frame(&self) -> bool {
        self.close_requested
            || self.job_control.is_some()
            || self.release_download.is_some()
            || self
                .composed
                .element()
                .is_some_and(|child| child.needs_animation_frame())
    }

    fn needs_idle_sync(&self) -> bool {
        self.close_requested || self.job_control.is_some() || self.release_download.is_some()
    }
}

mod rows;
mod window_bar;

#[cfg(test)]
mod tests;
