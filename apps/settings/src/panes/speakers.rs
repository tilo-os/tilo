//! Speakers — play through several of your devices as one stereo pair,
//! surround set, or set of speakers around a room.
//!
//! The pane walks [`SpeakerSetup`]: choose outputs, listen to each and say
//! which one it was, optionally place room speakers, name and save. Saving
//! asks the other devices to join; once every speaker is ready the set can
//! be calibrated with any microphone, and the result is a before/after
//! proposal that changes nothing until it is applied.
//!
//! The steps are [`SpeakerFlow`] views; what reaches the compositor is a
//! [`SpeakerEffect`], run through [`SpeakerService`] on a worker thread,
//! so agents and automation use the same door as the buttons.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use taffy::{NodeId, TaffyTree};
use ui::collaboration::{
    CalibrationMemberState, CalibrationMicrophoneState, CalibrationPhaseInfo, SpeakerCommand,
    SpeakerMemberSpec, SpeakerReply, SpeakerRoleSpec, SpeakerSnapshot, SpeakerTrimInfo,
};
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, LiveRow};
use ui::theme::Theme;
use ui::tilo_resource::{self, Effect};
use ui::time::{Duration, Instant};
use ui::widget::{Button, Dropdown, Slider, TextInput, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::effects::SettingsEffect;
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Replies, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::speaker_setup::{
    Arrangement, BrokerSpeakers, ROOM_EXTENT_MM, SpeakerService, SpeakerSetup, Step,
    member_status_text, playback_text, role_answer, role_text,
};
use crate::widgets::{
    StatusTone, caption, card, cards, choice_row_described, rows, semantic_status_pill,
    setting_row, setting_row_described, value_row,
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

/// What reaches the compositor's speaker sets.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum SpeakerEffect {
    /// Save a speaker set; the other devices are asked to join.
    #[effect(job, label = "Create speaker set")]
    CreateSpeakerSet {
        #[param("The set's name")]
        name: String,
        #[param("Each speaker: its device, output and role")]
        members: Vec<SpeakerMemberSpec>,
    },
    /// Play a short sound on one output, to tell which speaker it is.
    #[effect(job, label = "Play a sound")]
    IdentifySpeaker {
        #[param("The device the output is on")]
        device: String,
        #[param("The output's selector")]
        output: String,
    },
    /// Measure a set with the offered microphones: the one at `listening`
    /// (where the person sits), and every other except `excluded`.
    #[effect(job, row, label = "Calibrate")]
    CalibrateSpeakerSet {
        #[param("The speaker set's id")]
        group: String,
        #[param("Devices whose microphones are not used")]
        excluded: Vec<String>,
        #[param("The device whose microphone is where the person sits")]
        listening: Option<String>,
    },
    /// Stop measuring a set.
    #[effect(job, row, label = "Cancel calibration")]
    CancelCalibration {
        #[param("The speaker set's id")]
        group: String,
    },
    /// Apply a measured proposal.
    #[effect(job, row, label = "Apply calibration")]
    ApplyCalibration {
        #[param("The speaker set's id")]
        group: String,
    },
    /// Go back to how the set sounded before the last calibration.
    #[effect(job, row, label = "Revert calibration")]
    RevertCalibration {
        #[param("The speaker set's id")]
        group: String,
    },
    /// Discard a measured proposal.
    #[effect(job, row, label = "Discard calibration")]
    DismissCalibration {
        #[param("The speaker set's id")]
        group: String,
    },
    /// Delete a speaker set; its devices play on their own again.
    #[effect(job, row, destructive, label = "Delete speaker set")]
    DeleteSpeakerSet {
        #[param("The speaker set's id")]
        group: String,
    },
}

impl SpeakerEffect {
    /// The shell command this effect asks for.
    fn command(&self) -> SpeakerCommand {
        match self.clone() {
            Self::CreateSpeakerSet { name, members } => SpeakerCommand::Create { name, members },
            Self::IdentifySpeaker { device, output } => SpeakerCommand::Identify { device, output },
            Self::CalibrateSpeakerSet {
                group,
                excluded,
                listening,
            } => SpeakerCommand::StartCalibration {
                group,
                excluded,
                listening,
            },
            Self::CancelCalibration { group } => SpeakerCommand::CancelCalibration { group },
            Self::ApplyCalibration { group } => SpeakerCommand::ApplyCalibration { group },
            Self::RevertCalibration { group } => SpeakerCommand::RevertCalibration { group },
            Self::DismissCalibration { group } => SpeakerCommand::DismissCalibration { group },
            Self::DeleteSpeakerSet { group } => SpeakerCommand::Delete { group },
        }
    }
}

/// The steps of setting up and calibrating a speaker set.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum SpeakerFlow {
    /// Start setting up a new speaker set.
    #[effect(view)]
    BeginSpeakerSetup,
    /// Go back one step.
    #[effect(view)]
    SpeakerSetupBack,
    /// Go on to the next step with what is chosen.
    #[effect(view)]
    ContinueSpeakerSetup,
    /// Play the sound of the speaker being identified.
    #[effect(view)]
    PlayIdentifySound,
    /// Play a sound on one chosen speaker, by its place in the list.
    #[effect(view)]
    PlaySpeaker { speaker: usize },
    /// Leave the speaker being identified without a role.
    #[effect(view)]
    SkipSpeaker,
    /// Say which speaker played: `left`, `right`, `center`, `lfe`,
    /// `surround-left` or `surround-right`.
    #[effect(view)]
    AssignSpeakerRole { role: String },
    /// Save the set as named.
    #[effect(view)]
    SaveSpeakerSet,
    /// Calibrate the set shown with the microphones chosen.
    #[effect(view)]
    StartCalibration,
    /// Open a saved set's calibration.
    #[effect(view)]
    OpenCalibration { group: String },
    /// Close the flow and show the saved sets.
    #[effect(view)]
    CloseSpeakerSetup,
}

/// Run `effect` through the speaker service; `cx` is told how it ends.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: SpeakerEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    if !effect.command().valid() {
        return Err(
            "that speaker request is incomplete: check the names, members and ids".to_owned(),
        );
    }
    let reply = cx.reply();
    app.speakers()
        .model()
        .borrow_mut()
        .start(effect, Some(reply));
    Ok(Effected::Pending)
}

/// Take a step of the flow; a step that reaches the compositor invokes its
/// [`SpeakerEffect`].
pub(crate) fn answer_flow(
    app: &SettingsApp,
    step: SpeakerFlow,
    _cx: &mut EffectCx<Settings, SettingsApp>,
) -> EffectAnswer {
    let job = app.speakers().model().borrow_mut().flow(step)?;
    if let Some(job) = job {
        app.host().invoke(job);
    }
    Ok(Effected::done())
}

const fn role_slug(role: SpeakerRoleSpec) -> &'static str {
    match role {
        SpeakerRoleSpec::Left => "left",
        SpeakerRoleSpec::Right => "right",
        SpeakerRoleSpec::Center => "center",
        SpeakerRoleSpec::Lfe => "lfe",
        SpeakerRoleSpec::SurroundLeft => "surround-left",
        SpeakerRoleSpec::SurroundRight => "surround-right",
        SpeakerRoleSpec::Room { .. } => "room",
    }
}

fn trim_summary(trim: &SpeakerTrimInfo) -> String {
    format!(
        "{:.1} ms delay, {:+.1} dB gain",
        trim.delay_ms, trim.gain_db
    )
}

/// Metres sound travels per millisecond at room temperature.
const SOUND_METRES_PER_MS: f64 = 0.343;

fn one_decimal(value: f64) -> String {
    format!("{value:.1}")
}

/// "Arrives after 48.2 ms at your seat · its output takes 41.0 ms · about
/// 2.4 m away".
fn measured_text(
    delay_ms: f64,
    output_latency_ms: Option<f64>,
    acoustic_ms: Option<f64>,
) -> String {
    let mut parts = vec![t_args(
        "speakers-run-member-arrives",
        &[("delay", one_decimal(delay_ms).into())],
    )];
    if let Some(latency) = output_latency_ms {
        parts.push(t_args(
            "speakers-run-member-output-latency",
            &[("latency", one_decimal(latency).into())],
        ));
    }
    if let Some(acoustic) = acoustic_ms {
        parts.push(t_args(
            "speakers-run-member-distance",
            &[("metres", one_decimal(acoustic * SOUND_METRES_PER_MS).into())],
        ));
    }
    parts.join("\n")
}

fn microphone_run_text(state: &CalibrationMicrophoneState) -> String {
    match state {
        CalibrationMicrophoneState::AwaitingConsent => t("speakers-run-microphone-awaiting"),
        CalibrationMicrophoneState::Declined => t("speakers-run-microphone-declined"),
        CalibrationMicrophoneState::Listening => t("speakers-run-microphone-listening"),
        CalibrationMicrophoneState::Used { heard, rejected } if *rejected > 0 => t_args(
            "speakers-run-microphone-used-rejected",
            &[("heard", (*heard).into()), ("rejected", (*rejected).into())],
        ),
        CalibrationMicrophoneState::Used { heard, .. } => t_args(
            "speakers-run-microphone-used",
            &[("heard", (*heard).into())],
        ),
        CalibrationMicrophoneState::NotHeard => t("speakers-run-microphone-not-heard"),
        CalibrationMicrophoneState::Failed { reason } => t_args(
            "speakers-run-microphone-failed",
            &[("reason", reason.as_str().into())],
        ),
    }
}

fn role_from_slug(slug: &str) -> Option<SpeakerRoleSpec> {
    [
        SpeakerRoleSpec::Left,
        SpeakerRoleSpec::Right,
        SpeakerRoleSpec::Center,
        SpeakerRoleSpec::Lfe,
        SpeakerRoleSpec::SurroundLeft,
        SpeakerRoleSpec::SurroundRight,
    ]
    .into_iter()
    .find(|role| role_slug(*role) == slug)
}

/// What a finished background call was for.
enum Purpose {
    Refresh,
    Effect(SpeakerEffect),
}

struct Finished {
    purpose: Purpose,
    result: Result<SpeakerReply, String>,
}

/// What the speakers area keeps for the window: the model, asked for the
/// sets when first needed.
pub struct SpeakersArea {
    cx: PaneCx,
    model: std::cell::OnceCell<Shared<SpeakersModel>>,
}

impl SpeakersArea {
    pub fn new(cx: &PaneCx) -> Self {
        Self {
            cx: cx.clone(),
            model: std::cell::OnceCell::new(),
        }
    }

    /// The model, over the compositor, or over sample sets for a
    /// screenshot.
    pub fn model(&self) -> &Shared<SpeakersModel> {
        self.model.get_or_init(|| {
            let service: Arc<dyn SpeakerService> = if self.cx.reaches_devices() {
                Arc::new(BrokerSpeakers)
            } else {
                Arc::new(SampleSpeakers)
            };
            Rc::new(std::cell::RefCell::new(SpeakersModel::new(
                self.cx.clone(),
                service,
            )))
        })
    }

    /// Take in finished calls, if the model exists. Only an open pane
    /// keeps asking for the sets again.
    pub(crate) fn poll(&self) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().poll_finished();
        }
    }

    /// Whether the area has read what its rows show: its pane was shown,
    /// an effect or a row lookup reached it, or the window read it after
    /// its first frame ([`crate::effects::LIVE_AREAS`]).
    pub(crate) fn is_open(&self) -> bool {
        self.model.get().is_some()
    }

    /// One row per set this device leads, offering its calibration and
    /// deletion.
    pub(crate) fn rows(&self) -> Vec<LiveRow> {
        let model = self.model().borrow();
        model
            .setup
            .groups()
            .iter()
            .filter(|group| group.led_here)
            .map(|group| {
                LiveRow::new(
                    format!("settings.speaker-group:{}", group.id),
                    "settings.speaker-group",
                    group.name.clone(),
                )
                .operation(
                    SettingsEffect::from(SpeakerEffect::DeleteSpeakerSet {
                        group: group.id.clone(),
                    })
                    .bound(),
                )
            })
            .collect()
    }
}

pub struct SpeakersModel {
    cx: PaneCx,
    setup: SpeakerSetup,
    service: Arc<dyn SpeakerService>,
    finished_tx: Sender<Finished>,
    finished_rx: Receiver<Finished>,
    in_flight: usize,
    refreshing: bool,
    next_refresh: Instant,
    arrangement_state: ui::LocalState<String>,
    name_state: ui::LocalState<String>,
    listening_state: ui::LocalState<String>,
    microphone_states: Vec<(String, ui::LocalState<bool>)>,
    chosen_states: Vec<(String, String, ui::LocalState<bool>)>,
    room_points: ui::LocalState<Vec<(i32, i32)>>,
    placement_states: Vec<(ui::LocalState<f64>, ui::LocalState<f64>)>,
    last_room_points: Vec<(i32, i32)>,
    replies: Replies,
    changed: bool,
}

impl SpeakersModel {
    fn new(cx: PaneCx, service: Arc<dyn SpeakerService>) -> Self {
        let (finished_tx, finished_rx) = channel();
        let mut model = Self {
            cx,
            setup: SpeakerSetup::default(),
            service,
            finished_tx,
            finished_rx,
            in_flight: 0,
            refreshing: false,
            next_refresh: ui::time::now(),
            arrangement_state: ui::LocalState::new(Arrangement::Stereo.label()),
            name_state: ui::LocalState::new(t("speakers-default-name")),
            listening_state: ui::LocalState::new(String::new()),
            microphone_states: Vec::new(),
            chosen_states: Vec::new(),
            room_points: ui::LocalState::new(Vec::new()),
            placement_states: Vec::new(),
            last_room_points: Vec::new(),
            replies: Replies::default(),
            changed: false,
        };
        model.refresh();
        model
    }

    /// The flow state (tests read this).
    #[must_use]
    pub fn setup(&self) -> &SpeakerSetup {
        &self.setup
    }

    fn call(&mut self, purpose: Purpose, command: SpeakerCommand) {
        let service = Arc::clone(&self.service);
        let finished = self.finished_tx.clone();
        self.in_flight = self.in_flight.saturating_add(1);
        let spawned = std::thread::Builder::new()
            .name("settings-speakers".into())
            .spawn(move || {
                let result = service.call(command);
                let _ = finished.send(Finished { purpose, result });
            });
        if let Err(error) = spawned {
            self.in_flight = self.in_flight.saturating_sub(1);
            self.setup.set_message(Some(t_args(
                "speakers-service-unreachable",
                &[("error", error.to_string().into())],
            )));
            self.changed = true;
        }
    }

    fn refresh(&mut self) {
        if !self.refreshing {
            self.refreshing = true;
            self.call(Purpose::Refresh, SpeakerCommand::Snapshot);
        }
        self.next_refresh = ui::time::now() + REFRESH_INTERVAL;
    }

    /// Ask the compositor for `effect`; `reply` is told how it ends.
    fn start(&mut self, effect: SpeakerEffect, reply: Option<EffectReply>) {
        if let Some(reply) = reply {
            self.replies.wait(effect.id(), reply);
        }
        let command = effect.command();
        self.call(Purpose::Effect(effect), command);
    }

    /// Take `step`; the effect it starts, if it reaches the compositor.
    fn flow(&mut self, step: SpeakerFlow) -> Result<Option<SpeakerEffect>, String> {
        self.changed = true;
        let job = match step {
            SpeakerFlow::BeginSpeakerSetup => {
                self.setup.begin();
                self.arrangement_state.set(Arrangement::Stereo.label());
                self.name_state.set(self.setup.name().to_string());
                self.chosen_states.clear();
                None
            }
            SpeakerFlow::SpeakerSetupBack => {
                self.setup.back();
                None
            }
            SpeakerFlow::CloseSpeakerSetup => {
                self.setup.close();
                None
            }
            SpeakerFlow::ContinueSpeakerSetup => {
                match self.setup.step() {
                    Step::ChooseOutputs => {
                        let problem = self.setup.confirm_outputs().err();
                        self.setup.set_message(problem);
                    }
                    Step::Layout => self.setup.finish_layout(),
                    _ => {}
                }
                None
            }
            SpeakerFlow::PlayIdentifySound => match self.setup.identify_command() {
                Some(SpeakerCommand::Identify { device, output }) => {
                    Some(SpeakerEffect::IdentifySpeaker { device, output })
                }
                _ => None,
            },
            SpeakerFlow::PlaySpeaker { speaker } => {
                let candidate = self
                    .setup
                    .chosen()
                    .get(speaker)
                    .cloned()
                    .ok_or_else(|| format!("there is no speaker {speaker}"))?;
                Some(SpeakerEffect::IdentifySpeaker {
                    device: candidate.device,
                    output: candidate.output,
                })
            }
            SpeakerFlow::SkipSpeaker => {
                self.setup.skip_identify();
                None
            }
            SpeakerFlow::AssignSpeakerRole { role } => {
                let role =
                    role_from_slug(&role).ok_or_else(|| format!("`{role}` is not a role"))?;
                let problem = self.setup.assign(role).err();
                self.setup.set_message(problem);
                None
            }
            SpeakerFlow::SaveSpeakerSet => match self.setup.save_command() {
                Ok(SpeakerCommand::Create { name, members }) => {
                    Some(SpeakerEffect::CreateSpeakerSet { name, members })
                }
                Ok(_) => None,
                Err(problem) => {
                    self.setup.set_message(Some(problem.clone()));
                    return Err(problem);
                }
            },
            SpeakerFlow::StartCalibration => {
                let Step::Calibrate { group } = self.setup.step().clone() else {
                    return Err("no speaker set is being calibrated".into());
                };
                match self.setup.calibrate_command(&group) {
                    Ok(SpeakerCommand::StartCalibration {
                        group,
                        excluded,
                        listening,
                    }) => Some(SpeakerEffect::CalibrateSpeakerSet {
                        group,
                        excluded,
                        listening,
                    }),
                    Ok(_) => None,
                    Err(problem) => {
                        self.setup.set_message(Some(problem.clone()));
                        return Err(problem);
                    }
                }
            }
            SpeakerFlow::OpenCalibration { group } => {
                self.setup.open_calibration(&group);
                None
            }
        };
        Ok(job)
    }

    fn adopt(&mut self, snapshot: SpeakerSnapshot) {
        let before = self.setup.snapshot().clone();
        self.setup.update(snapshot);
        if self.setup.snapshot() != &before {
            self.changed = true;
        }
    }

    fn poll_finished(&mut self) {
        while let Ok(finished) = self.finished_rx.try_recv() {
            self.in_flight = self.in_flight.saturating_sub(1);
            match finished.purpose {
                Purpose::Refresh => {
                    self.refreshing = false;
                    match finished.result {
                        Ok(SpeakerReply::Snapshot { snapshot }) => self.adopt(snapshot),
                        Ok(SpeakerReply::Rejected { reason }) | Err(reason) => {
                            self.setup.set_message(Some(reason));
                            self.changed = true;
                        }
                        Ok(SpeakerReply::Done { .. }) => {}
                    }
                }
                Purpose::Effect(effect) => {
                    let outcome = match finished.result {
                        Ok(SpeakerReply::Done { group }) => Ok(group),
                        Ok(SpeakerReply::Rejected { reason }) | Err(reason) => Err(reason),
                        Ok(SpeakerReply::Snapshot { .. }) => Err(t("speakers-service-unexpected")),
                    };
                    self.after_effect(&effect, &outcome);
                    self.refresh();
                }
            }
        }
    }

    fn after_effect(&mut self, effect: &SpeakerEffect, outcome: &Result<Option<String>, String>) {
        let message = match (outcome, effect) {
            (Err(reason), _) => Err(reason.clone()),
            (Ok(group), SpeakerEffect::CreateSpeakerSet { name, .. }) => {
                if let Some(group) = group {
                    self.setup.saved(group.clone());
                }
                Ok(t_args(
                    "speakers-done-saved",
                    &[("name", name.as_str().into())],
                ))
            }
            (Ok(_), SpeakerEffect::IdentifySpeaker { .. }) => Ok(t("speakers-done-identify")),
            (Ok(_), SpeakerEffect::CalibrateSpeakerSet { .. }) => Ok(t("speakers-done-calibrate")),
            (Ok(_), SpeakerEffect::CancelCalibration { .. }) => {
                Ok(t("speakers-calibration-cancelled"))
            }
            (Ok(_), SpeakerEffect::ApplyCalibration { .. }) => Ok(t("speakers-done-apply")),
            (Ok(_), SpeakerEffect::RevertCalibration { .. }) => Ok(t("speakers-done-revert")),
            (Ok(_), SpeakerEffect::DismissCalibration { .. }) => Ok(t("speakers-done-dismiss")),
            (Ok(_), SpeakerEffect::DeleteSpeakerSet { .. }) => Ok(t("speakers-done-delete")),
        };
        match &message {
            Err(reason) => self.setup.set_message(Some(reason.clone())),
            Ok(_) if !matches!(effect, SpeakerEffect::CreateSpeakerSet { .. }) => {
                self.setup.set_message(None);
            }
            Ok(_) => {}
        }
        self.replies.finish(effect.id(), message);
        self.changed = true;
    }

    /// Reflect flow state into the widget-bound local states, so a rebuilt
    /// composition shows the model rather than stale control values.
    fn sync_bound_states(&mut self) {
        let candidates = self.setup.candidates();
        let keys = candidates
            .iter()
            .map(|candidate| (candidate.device.clone(), candidate.output.clone()))
            .collect::<Vec<_>>();
        let current = self
            .chosen_states
            .iter()
            .map(|(device, output, _)| (device.clone(), output.clone()))
            .collect::<Vec<_>>();
        if keys != current {
            self.chosen_states = candidates
                .iter()
                .map(|candidate| {
                    (
                        candidate.device.clone(),
                        candidate.output.clone(),
                        ui::LocalState::new(self.setup.is_chosen(candidate)),
                    )
                })
                .collect();
            self.changed = true;
        }
        let calibrating = match self.setup.step() {
            Step::Calibrate { group } => Some(group.clone()),
            _ => None,
        };
        let extras = calibrating
            .as_deref()
            .map(|group| self.setup.extra_microphones(group))
            .unwrap_or_default();
        let devices = extras
            .iter()
            .map(|microphone| microphone.device.clone())
            .collect::<Vec<_>>();
        let current = self
            .microphone_states
            .iter()
            .map(|(device, _)| device.clone())
            .collect::<Vec<_>>();
        if devices != current {
            self.microphone_states = extras
                .iter()
                .map(|microphone| {
                    (
                        microphone.device.clone(),
                        ui::LocalState::new(self.setup.also_listens(microphone)),
                    )
                })
                .collect();
            self.changed = true;
        }
        if let Some(listening) = calibrating
            .as_deref()
            .and_then(|group| self.setup.listening(group))
            && self.listening_state.with(Clone::clone) != listening.label
        {
            self.listening_state.set(listening.label);
            self.changed = true;
        }
        let points = self.setup.room_points();
        if points != self.last_room_points {
            self.last_room_points.clone_from(&points);
            self.room_points.set(points.clone());
            self.placement_states = points
                .iter()
                .map(|(x, y)| {
                    (
                        ui::LocalState::new(to_unit(*x)),
                        ui::LocalState::new(to_unit(*y)),
                    )
                })
                .collect();
        }
    }

    /// Apply control edits made since the last pass.
    fn read_controls(&mut self) {
        let arrangement = self.arrangement_state.with(Clone::clone);
        if let Some(arrangement) = Arrangement::from_label(&arrangement)
            && arrangement != self.setup.arrangement()
        {
            self.setup.set_arrangement(arrangement);
            self.changed = true;
        }
        let name = self.name_state.with(Clone::clone);
        if name != self.setup.name() {
            self.setup.set_name(&name);
        }
        let candidates = self.setup.candidates();
        for (device, output, state) in &self.chosen_states {
            let wanted = state.with(|value| *value);
            if let Some(candidate) = candidates
                .iter()
                .find(|candidate| &candidate.device == device && &candidate.output == output)
                && self.setup.is_chosen(candidate) != wanted
            {
                self.setup.toggle(candidate);
                self.changed = true;
            }
        }
        if let Step::Calibrate { group } = self.setup.step().clone() {
            let chosen = self.listening_state.with(Clone::clone);
            if let Some(microphone) = self
                .setup
                .listening_choices(&group)
                .into_iter()
                .find(|microphone| microphone.label == chosen)
                && self
                    .setup
                    .listening(&group)
                    .is_none_or(|listening| listening.device != microphone.device)
            {
                self.setup.set_listening(&microphone.device);
                self.changed = true;
            }
            let extras = self.setup.extra_microphones(&group);
            for (device, state) in &self.microphone_states {
                let wanted = state.with(|value| *value);
                if let Some(microphone) = extras
                    .iter()
                    .find(|microphone| &microphone.device == device)
                    && self.setup.also_listens(microphone) != wanted
                {
                    self.setup.set_also_listens(device, wanted);
                    self.changed = true;
                }
            }
        }
        if self.setup.step() == &Step::Layout {
            let dragged = self.room_points.with(Clone::clone);
            for (index, point) in dragged.iter().enumerate() {
                if self.last_room_points.get(index) != Some(point) {
                    self.setup.place(index, point.0, point.1);
                }
            }
            for (index, (x, y)) in self.placement_states.iter().enumerate() {
                let point = (from_unit(x.with(|v| *v)), from_unit(y.with(|v| *v)));
                if self.last_room_points.get(index).is_some_and(|last| {
                    (last.0 - point.0).abs() > 50 || (last.1 - point.1).abs() > 50
                }) && dragged.get(index) == self.last_room_points.get(index)
                {
                    self.setup.place(index, point.0, point.1);
                }
            }
            if self.setup.room_points() != self.last_room_points {
                self.changed = true;
            }
        }
    }

    /// "Where you sit" and "Also listen with" for the calibrate step.
    fn microphone_rows(&self, group: &str) -> Vec<Box<dyn Widget>> {
        let mut body = Vec::new();
        let choices = self.setup.listening_choices(group);
        let Some(listening) = self.setup.listening(group) else {
            return body;
        };
        let chooser = Dropdown::from_state(
            choices
                .iter()
                .map(|microphone| microphone.label.clone())
                .collect(),
            &self.listening_state,
        )
        .with_id("speakers.listening")
        .with_accessible_name(t("speakers-listening-title"))
        .with_placeholder(&listening.label);
        body.push(choice_row_described(
            &t("speakers-listening-title"),
            &t("speakers-listening-description"),
            Box::new(chooser),
        ));
        let extras = self.setup.extra_microphones(group);
        if !extras.is_empty() {
            body.push(caption(&t("speakers-also-listen-caption")));
        }
        for microphone in &extras {
            let title = if microphone.local {
                t_args(
                    "speakers-microphone-this-device-title",
                    &[("label", microphone.label.as_str().into())],
                )
            } else {
                microphone.label.clone()
            };
            let state = self
                .microphone_states
                .iter()
                .find(|(device, _)| device == &microphone.device)
                .map(|(_, state)| state);
            match state {
                Some(state) if microphone.status.usable() => {
                    body.push(setting_row_described(
                        &title,
                        &microphone.status_text(),
                        Box::new(
                            Toggle::from_state(state)
                                .with_id(&format!("speakers.microphone.{}", microphone.device))
                                .with_accessible_name(&t_args(
                                    "speakers-microphone-use",
                                    &[("label", microphone.label.as_str().into())],
                                )),
                        ),
                    ));
                }
                _ => body.push(value_row(&title, &microphone.status_text())),
            }
        }
        body
    }

    fn nav_buttons(&self, back: bool, next: Option<(&str, &str, bool)>) -> Box<dyn Widget> {
        let mut row = ui::widget::Row::new();
        if back {
            row = row.push(Box::new(self.cx.bind_invocation(
                Button::new("speakers.back", &t("speakers-back")),
                SpeakerFlow::SpeakerSetupBack,
            )));
        }
        row = row.push(Box::new(ui::widget::Spacer::new()));
        if let Some((id, label, enabled)) = next {
            let step = match id {
                "speakers.save" => SpeakerFlow::SaveSpeakerSet,
                _ => SpeakerFlow::ContinueSpeakerSetup,
            };
            row = row.push(Box::new(
                self.cx.bind_invocation(
                    Button::new(id, label)
                        .role(ui::widget::ButtonRole::Primary)
                        .with_enabled(enabled),
                    step,
                ),
            ));
        }
        Box::new(row)
    }

    fn message_rows(&self) -> Vec<Box<dyn Widget>> {
        self.setup.message().map(caption).into_iter().collect()
    }

    /// One saved set: its members, readiness, playback and actions.
    fn group_row(&self, group: &ui::collaboration::SpeakerGroupInfo) -> Box<dyn Widget> {
        let ready = group
            .members
            .iter()
            .filter(|member| member.status == ui::collaboration::SpeakerMemberStatus::Ready)
            .count();
        let summary = group
            .members
            .iter()
            .map(|member| {
                t_args(
                    "speakers-member-summary",
                    &[
                        ("device", member.device_label.as_str().into()),
                        ("role", role_text(member.role).into()),
                    ],
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut actions = ui::widget::Row::new().push(semantic_status_pill(
            &t_args(
                "speakers-ready-count",
                &[
                    ("ready", ready.to_string().into()),
                    ("total", group.members.len().to_string().into()),
                ],
            ),
            if ready == group.members.len() {
                StatusTone::Success
            } else {
                StatusTone::Warning
            },
        ));
        if let Some(playback) = &group.playback {
            actions = actions.push(semantic_status_pill(
                &playback_text(playback),
                match playback {
                    ui::collaboration::SpeakerPlaybackInfo::Playing => StatusTone::Success,
                    ui::collaboration::SpeakerPlaybackInfo::Connecting => StatusTone::Neutral,
                    ui::collaboration::SpeakerPlaybackInfo::SyncingClocks
                    | ui::collaboration::SpeakerPlaybackInfo::ClockFailed { .. } => {
                        StatusTone::Warning
                    }
                },
            ));
        }
        if group.led_here {
            actions = actions
                .push(Box::new(self.cx.bind_invocation(
                    Button::new(
                        &format!("speakers.group.{}.calibrate", group.id),
                        &t("speakers-calibrate-button"),
                    ),
                    SpeakerFlow::OpenCalibration {
                        group: group.id.clone(),
                    },
                )))
                .push(Box::new(
                    self.cx.bind_invocation(
                        Button::new(
                            &format!("speakers.group.{}.delete", group.id),
                            &t("speakers-delete"),
                        )
                        .role(ui::widget::ButtonRole::Destructive),
                        SpeakerEffect::DeleteSpeakerSet {
                            group: group.id.clone(),
                        },
                    ),
                ));
        }
        let mut description = if group.led_here {
            summary
        } else {
            t_args(
                "speakers-set-up-elsewhere",
                &[
                    ("summary", summary.as_str().into()),
                    ("leader", group.leader_label.as_str().into()),
                ],
            )
        };
        if let Some(ui::collaboration::SpeakerPlaybackInfo::ClockFailed { reason }) =
            &group.playback
        {
            description = t_args(
                "speakers-clock-failed-detail",
                &[
                    ("summary", description.as_str().into()),
                    ("reason", reason.as_str().into()),
                ],
            );
        }
        setting_row_described(&group.name, &description, Box::new(actions))
    }

    fn build_overview(&self) -> Vec<Box<dyn Widget>> {
        let mut sections = Vec::new();
        let group_rows = if self.setup.groups().is_empty() {
            vec![caption(&t("speakers-overview-empty"))]
        } else {
            self.setup
                .groups()
                .iter()
                .map(|group| self.group_row(group))
                .collect()
        };
        let mut body = group_rows;
        body.extend(self.message_rows());
        body.push(Box::new(
            ui::widget::Row::new()
                .push(Box::new(ui::widget::Spacer::new()))
                .push(Box::new(
                    self.cx.bind_invocation(
                        Button::new("speakers.begin", &t("speakers-begin"))
                            .role(ui::widget::ButtonRole::Primary),
                        SpeakerFlow::BeginSpeakerSetup,
                    ),
                )),
        ));
        sections.push(card(&t("speakers-overview-title"), rows(body)));
        sections
    }

    fn build_choose(&self) -> Vec<Box<dyn Widget>> {
        let arrangement = Dropdown::from_state(
            Arrangement::ALL
                .iter()
                .map(|arrangement| arrangement.label())
                .collect(),
            &self.arrangement_state,
        )
        .with_id("speakers.arrangement")
        .with_accessible_name(t("speakers-arrangement"));
        let candidates = self.setup.candidates();
        let mut body = vec![choice_row_described(
            &t("speakers-arrangement"),
            &self.setup.arrangement().description(),
            Box::new(arrangement),
        )];
        if candidates.is_empty() {
            body.push(caption(&t("speakers-no-outputs")));
        }
        for (candidate, (_, _, state)) in candidates.iter().zip(&self.chosen_states) {
            body.push(setting_row_described(
                &candidate.label(),
                &t(if candidate.local {
                    "speakers-output-this-device"
                } else {
                    "speakers-output-asks"
                }),
                Box::new(
                    Toggle::from_state(state)
                        .with_id(&format!(
                            "speakers.choose.{}.{}",
                            candidate.device, candidate.output
                        ))
                        .with_accessible_name(&t_args(
                            "speakers-output-use",
                            &[("label", candidate.label().into())],
                        )),
                ),
            ));
        }
        if let Some(problem) = self.setup.outputs_problem() {
            body.push(caption(&problem));
        }
        body.extend(self.message_rows());
        body.push(self.nav_buttons(
            true,
            Some((
                "speakers.next",
                &t("speakers-continue"),
                self.setup.outputs_problem().is_none(),
            )),
        ));
        vec![card(&t("speakers-choose-title"), rows(body))]
    }

    fn build_identify(&self, index: usize, played: bool) -> Vec<Box<dyn Widget>> {
        let count = self.setup.chosen().len();
        let mut body = vec![caption(&t_args(
            "speakers-identify-intro",
            &[
                ("index", index.saturating_add(1).to_string().into()),
                ("count", count.to_string().into()),
            ],
        ))];
        body.push(Box::new(
            ui::widget::Row::new()
                .push(Box::new(
                    self.cx.bind_invocation(
                        Button::new("speakers.play", &t("speakers-play-sound"))
                            .with_icon(ui::icons::Icon::Volume2),
                        SpeakerFlow::PlayIdentifySound,
                    ),
                ))
                .push(Box::new(ui::widget::Spacer::new())),
        ));
        if self.setup.arrangement() == Arrangement::Room {
            body.push(caption(&t("speakers-identify-room-next")));
            body.push(self.nav_buttons(true, None));
            body.push(Box::new(
                ui::widget::Row::new()
                    .push(Box::new(ui::widget::Spacer::new()))
                    .push(Box::new(
                        self.cx.bind_invocation(
                            Button::new("speakers.skip", &t("speakers-next-speaker"))
                                .role(ui::widget::ButtonRole::Primary),
                            SpeakerFlow::SkipSpeaker,
                        ),
                    )),
            ));
        } else {
            let mut answers = ui::widget::Row::new();
            for role in self.setup.remaining_roles() {
                answers = answers.push(Box::new(
                    self.cx.bind_invocation(
                        Button::new(
                            &format!("speakers.role.{}", role_slug(role)),
                            &role_answer(role),
                        )
                        .with_enabled(played)
                        .with_accessible_name(&t_args(
                            "speakers-identify-answer",
                            &[("role", role_text(role).into())],
                        )),
                        SpeakerFlow::AssignSpeakerRole {
                            role: role_slug(role).to_owned(),
                        },
                    ),
                ));
            }
            body.push(setting_row(
                &t("speakers-identify-title"),
                Box::new(answers),
            ));
            if !played {
                body.push(caption(&t("speakers-identify-play-first")));
            }
            body.extend(self.message_rows());
            body.push(self.nav_buttons(true, None));
        }
        vec![card(&t("speakers-identify-title"), rows(body))]
    }

    fn build_layout(&self) -> Vec<Box<dyn Widget>> {
        let labels = self
            .setup
            .chosen()
            .iter()
            .map(|candidate| candidate.device_label.clone())
            .collect::<Vec<_>>();
        let mut body = vec![
            caption(&t("speakers-layout-intro")),
            Box::new(RoomCanvas::new(self.room_points.clone(), labels.clone())),
        ];
        for (index, ((x, y), label)) in self.placement_states.iter().zip(&labels).enumerate() {
            body.push(setting_row(
                label,
                Box::new(
                    ui::widget::Row::new()
                        .push(Box::new(
                            Slider::from_state(x)
                                .with_id(&format!("speakers.layout.{index}.x"))
                                .with_accessible_name(&t_args(
                                    "speakers-layout-x",
                                    &[("label", label.as_str().into())],
                                )),
                        ))
                        .push(Box::new(
                            Slider::from_state(y)
                                .with_id(&format!("speakers.layout.{index}.y"))
                                .with_accessible_name(&t_args(
                                    "speakers-layout-y",
                                    &[("label", label.as_str().into())],
                                )),
                        ))
                        .push(Box::new(
                            self.cx.bind_invocation(
                                Button::new(
                                    &format!("speakers.layout.{index}.play"),
                                    &t("speakers-layout-play"),
                                )
                                .with_accessible_name(&t_args(
                                    "speakers-layout-play-on",
                                    &[("label", label.as_str().into())],
                                )),
                                SpeakerFlow::PlaySpeaker { speaker: index },
                            ),
                        )),
                ),
            ));
        }
        if let Some(problem) = self.setup.layout_problem() {
            body.push(caption(&problem));
        }
        body.push(self.nav_buttons(
            true,
            Some((
                "speakers.next",
                &t("speakers-continue"),
                self.setup.layout_problem().is_none(),
            )),
        ));
        vec![card(&t("speakers-layout-title"), rows(body))]
    }

    fn build_name(&self) -> Vec<Box<dyn Widget>> {
        let name = TextInput::from_state(&self.name_state)
            .with_id("speakers.name")
            .with_placeholder(&t("speakers-name-placeholder"))
            .with_accessible_name(t("speakers-name"));
        let mut body = vec![choice_row_described(
            &t("speakers-name"),
            &t("speakers-name-description"),
            Box::new(name),
        )];
        for (candidate, role) in self.setup.chosen().iter().zip(self.setup.roles()) {
            body.push(value_row(
                &candidate.label(),
                &role.map_or_else(|| "—".to_string(), role_text),
            ));
        }
        body.push(caption(&t("speakers-name-join-note")));
        body.extend(self.message_rows());
        body.push(self.nav_buttons(
            true,
            Some(("speakers.save", &t("speakers-save"), self.in_flight == 0)),
        ));
        vec![card(&t("speakers-name-title"), rows(body))]
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one card lists members, measurement progress and the proposal actions together"
    )]
    fn build_calibrate(&self, group: &str) -> Vec<Box<dyn Widget>> {
        let Some(known) = self.setup.group(group) else {
            let mut body = self.message_rows();
            body.push(caption(&t("speakers-saving")));
            body.push(Box::new(
                ui::widget::Row::new()
                    .push(Box::new(ui::widget::Spacer::new()))
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new("speakers.done", &t("speakers-done")),
                        SpeakerFlow::CloseSpeakerSetup,
                    ))),
            ));
            return vec![card(&t("speakers-saved-title"), rows(body))];
        };
        let mut body = Vec::new();
        for member in &known.members {
            body.push(setting_row_described(
                &member.device_label,
                &t_args(
                    "speakers-member-role-trim",
                    &[
                        ("role", role_text(member.role).into()),
                        ("trim", trim_summary(&member.trim).into()),
                    ],
                ),
                semantic_status_pill(
                    &member_status_text(member.status),
                    if member.status == ui::collaboration::SpeakerMemberStatus::Ready {
                        StatusTone::Success
                    } else {
                        StatusTone::Warning
                    },
                ),
            ));
        }
        let run = self.setup.calibration(group);
        let phase = run.map(|run| run.phase);
        if !matches!(
            phase,
            Some(
                CalibrationPhaseInfo::WaitingForMicrophones
                    | CalibrationPhaseInfo::Measuring
                    | CalibrationPhaseInfo::Proposed
            )
        ) {
            body.extend(self.microphone_rows(group));
        }
        if let Some(run) = run {
            if let Some(label) = &run.listening_label {
                body.push(caption(&t_args(
                    "speakers-run-aligned-for",
                    &[("label", label.as_str().into())],
                )));
            }
            for member in &run.members {
                let status = match &member.state {
                    CalibrationMemberState::Waiting => t("speakers-run-member-waiting"),
                    CalibrationMemberState::Measuring => t("speakers-run-member-measuring"),
                    CalibrationMemberState::Measured {
                        delay_ms,
                        output_latency_ms,
                        acoustic_ms,
                        ..
                    } => measured_text(*delay_ms, *output_latency_ms, *acoustic_ms),
                    CalibrationMemberState::Failed { reason } => reason.clone(),
                };
                let detail = member.after.as_ref().map_or_else(
                    || status.clone(),
                    |after| {
                        t_args(
                            "speakers-trim-change",
                            &[
                                ("before", trim_summary(&member.before).into()),
                                ("after", trim_summary(after).into()),
                            ],
                        )
                    },
                );
                body.push(value_row(&member.label, &detail));
            }
            for microphone in &run.microphones {
                let label = if microphone.listening {
                    t_args(
                        "speakers-run-microphone-listening-position",
                        &[("label", microphone.label.as_str().into())],
                    )
                } else {
                    microphone.label.clone()
                };
                body.push(value_row(&label, &microphone_run_text(&microphone.state)));
            }
            if run.phase == CalibrationPhaseInfo::WaitingForMicrophones {
                body.push(caption(&t("speakers-run-waiting-for-microphones")));
            }
            if let Some(message) = &run.message {
                body.push(caption(message));
            }
        }
        body.extend(self.message_rows());
        let mut actions = ui::widget::Row::new()
            .push(Box::new(self.cx.bind_invocation(
                Button::new("speakers.done", &t("speakers-done")),
                SpeakerFlow::CloseSpeakerSetup,
            )))
            .push(Box::new(ui::widget::Spacer::new()));
        match phase {
            Some(CalibrationPhaseInfo::Proposed) => {
                actions = actions
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new("speakers.dismiss", &t("speakers-discard")),
                        SpeakerEffect::DismissCalibration {
                            group: group.to_owned(),
                        },
                    )))
                    .push(Box::new(
                        self.cx.bind_invocation(
                            Button::new("speakers.apply", &t("speakers-apply"))
                                .role(ui::widget::ButtonRole::Primary),
                            SpeakerEffect::ApplyCalibration {
                                group: group.to_owned(),
                            },
                        ),
                    ));
            }
            Some(CalibrationPhaseInfo::Applied) => {
                actions = actions.push(Box::new(self.cx.bind_invocation(
                    Button::new("speakers.revert", &t("speakers-revert")),
                    SpeakerEffect::RevertCalibration {
                        group: group.to_owned(),
                    },
                )));
            }
            Some(CalibrationPhaseInfo::Measuring | CalibrationPhaseInfo::WaitingForMicrophones) => {
                actions = actions.push(Box::new(self.cx.bind_invocation(
                    Button::new("speakers.cancel", &t("speakers-calibration-cancel")),
                    SpeakerEffect::CancelCalibration {
                        group: group.to_owned(),
                    },
                )));
            }
            Some(CalibrationPhaseInfo::Failed) | None => {
                let problem = self.setup.calibration_problem(group);
                if let Some(problem) = &problem {
                    body.push(caption(problem));
                }
                actions = actions.push(Box::new(
                    self.cx.bind_invocation(
                        Button::new(
                            "speakers.calibrate",
                            &t(if phase.is_some() {
                                "speakers-try-again"
                            } else {
                                "speakers-calibrate-button"
                            }),
                        )
                        .role(ui::widget::ButtonRole::Primary)
                        .with_enabled(problem.is_none()),
                        SpeakerFlow::StartCalibration,
                    ),
                ));
            }
        }
        body.push(Box::new(actions));
        vec![card(
            &t_args(
                "speakers-calibrate-title",
                &[("name", known.name.as_str().into())],
            ),
            rows(body),
        )]
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        if !self.setup.snapshot().media_enabled && !self.refreshing {
            return cards(vec![card(
                &t("speakers-overview-title"),
                rows(vec![caption(&t("speakers-media-off"))]),
            )]);
        }
        cards(match self.setup.step().clone() {
            Step::Overview => self.build_overview(),
            Step::ChooseOutputs => self.build_choose(),
            Step::Identify { index, played } => self.build_identify(index, played),
            Step::Layout => self.build_layout(),
            Step::Name => self.build_name(),
            Step::Calibrate { group } => self.build_calibrate(&group),
        })
    }
}

/// Slider units (0..1) ↔ room millimetres (±`ROOM_EXTENT_MM`).
fn to_unit(mm: i32) -> f64 {
    (f64::from(mm) / f64::from(ROOM_EXTENT_MM))
        .mul_add(0.5, 0.5)
        .clamp(0.0, 1.0)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is clamped to the room extent"
)]
fn from_unit(unit: f64) -> i32 {
    ((unit.clamp(0.0, 1.0) - 0.5) * 2.0 * f64::from(ROOM_EXTENT_MM)).round() as i32
}

impl PaneModel for SpeakersModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        self.poll_finished();
        self.read_controls();
        self.sync_bound_states();
        if ui::time::now() >= self.next_refresh {
            self.refresh();
        }
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "SpeakersPane"
    }

    fn busy(&self) -> bool {
        self.in_flight > 0
    }

    fn next_wake(&self) -> Option<Instant> {
        Some(self.next_refresh)
    }
}

/// The speaker sets a screenshot and the crate's tests show: one stereo
/// pair led here, and two devices that could join. Every command succeeds.
struct SampleSpeakers;

impl SpeakerService for SampleSpeakers {
    fn call(&self, command: SpeakerCommand) -> Result<SpeakerReply, String> {
        use ui::collaboration::{
            SpeakerDeviceInfo, SpeakerEndpointInfo, SpeakerGroupInfo, SpeakerMemberInfo,
            SpeakerMemberStatus,
        };
        let device = |id: &str, local: bool| SpeakerDeviceInfo {
            id: id.into(),
            label: id.into(),
            online: true,
            local,
            can_join: true,
            outputs: vec![SpeakerEndpointInfo {
                selector: "0a".repeat(16),
                label: "Speakers".into(),
                is_default: true,
            }],
            microphones: Vec::new(),
        };
        let member = |device: &str, role| SpeakerMemberInfo {
            device: device.into(),
            device_label: device.into(),
            output: "0a".repeat(16),
            output_label: None,
            role,
            status: SpeakerMemberStatus::Ready,
            trim: SpeakerTrimInfo::default(),
        };
        Ok(match command {
            SpeakerCommand::Snapshot => SpeakerReply::Snapshot {
                snapshot: SpeakerSnapshot {
                    media_enabled: true,
                    devices: vec![device("Desk", true), device("Shelf", false)],
                    groups: vec![SpeakerGroupInfo {
                        id: "cd".repeat(16),
                        name: "Living room".into(),
                        leader_label: "Desk".into(),
                        led_here: true,
                        members: vec![
                            member("Desk", SpeakerRoleSpec::Left),
                            member("Shelf", SpeakerRoleSpec::Right),
                        ],
                        playback: None,
                        microphones: Vec::new(),
                    }],
                    calibrations: Vec::new(),
                },
            },
            SpeakerCommand::Create { .. } => SpeakerReply::Done {
                group: Some("ab".repeat(16)),
            },
            _ => SpeakerReply::Done { group: None },
        })
    }
}

/// A top-down room with the listener near the bottom centre and one dot per
/// speaker, dragged with the pointer. The sliders beside it are the
/// keyboard, screen-reader and automation path to the same positions.
struct RoomCanvas {
    points: ui::LocalState<Vec<(i32, i32)>>,
    labels: Vec<String>,
    rect: Rect,
    cursor: Point,
    dragging: Option<usize>,
    marker_radius: f64,
}

impl RoomCanvas {
    fn new(points: ui::LocalState<Vec<(i32, i32)>>, labels: Vec<String>) -> Self {
        Self {
            points,
            labels,
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            dragging: None,
            marker_radius: f64::from(ui::tokens::Metrics::BASE.icon_md) / 2.0,
        }
    }

    /// Millimetres per logical pixel, fitting the room extent in the shorter side.
    fn scale(&self) -> f64 {
        let half = (self.rect.width() / 2.0)
            .min(self.rect.height() / 2.0)
            .max(1.0);
        f64::from(ROOM_EXTENT_MM) / (half - self.marker_radius).max(1.0)
    }

    fn origin(&self) -> Point {
        self.rect.center()
    }

    fn to_screen(&self, (x, y): (i32, i32)) -> Point {
        let scale = self.scale();
        let origin = self.origin();
        Point::new(
            origin.x + f64::from(x) / scale,
            origin.y - f64::from(y) / scale,
        )
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "the result is clamped to the room extent"
    )]
    fn to_room(&self, point: Point) -> (i32, i32) {
        let scale = self.scale();
        let origin = self.origin();
        let extent = f64::from(ROOM_EXTENT_MM);
        (
            ((point.x - origin.x) * scale)
                .clamp(-extent, extent)
                .round() as i32,
            ((origin.y - point.y) * scale)
                .clamp(-extent, extent)
                .round() as i32,
        )
    }
}

impl Widget for RoomCanvas {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.marker_radius = f64::from(theme.tokens.metrics.icon_md) / 2.0;
        let node = ui::widget::TaffyTreeExt::leaf_node(
            taffy,
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::length(
                        theme.tokens.metrics.panel_width_utility * 13.0 / 16.0,
                    ),
                },
                ..Default::default()
            },
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.panel);
        ui::surface::card(scene, transform, self.rect, radius, colors.overlay_subtle);
        let origin = self.origin();
        for ring in [1_000, 2_000, 3_000, 4_000] {
            let ring_radius = f64::from(ring) / self.scale();
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                colors.overlay_border_subtle,
                None,
                &Circle::new(origin, ring_radius),
            );
        }
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.overlay_border_subtle,
            None,
            &Line::new(
                Point::new(origin.x, self.rect.y0),
                Point::new(origin.x, self.rect.y1),
            ),
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.text_dim,
            None,
            &Circle::new(origin, 5.0),
        );
        text_renderer.draw_style(
            scene,
            &t("speakers-room-you"),
            colors.text_dim,
            transform.pre_translate((origin.x + 8.0, origin.y + 4.0).into()),
            None,
            &theme.text(ui::TextRole::Caption),
        );
        let points = self.points.with(Clone::clone);
        for (index, point) in points.iter().enumerate() {
            let center = self.to_screen(*point);
            let fill = colors.presence.series(index);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                fill,
                None,
                &Circle::new(center, self.marker_radius),
            );
            if self.dragging == Some(index) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    colors.border_focus,
                    None,
                    &Circle::new(center, self.marker_radius + 3.0),
                );
            }
            if let Some(label) = self.labels.get(index) {
                text_renderer.draw_style(
                    scene,
                    label,
                    colors.text_primary,
                    transform.pre_translate(
                        (center.x + self.marker_radius + 4.0, center.y - 7.0).into(),
                    ),
                    Some(theme.tokens.metrics.chooser_width),
                    &theme.text(ui::TextRole::Caption),
                );
            }
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if let Some(index) = self.dragging {
                    let moved = self.to_room(self.cursor);
                    self.points.update(|points| {
                        if let Some(point) = points.get_mut(index) {
                            *point = moved;
                        }
                    });
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.cursor) => {
                let points = self.points.with(Clone::clone);
                self.dragging = points
                    .iter()
                    .enumerate()
                    .map(|(index, point)| (index, self.to_screen(*point).distance(self.cursor)))
                    .filter(|(_, distance)| *distance <= self.marker_radius * 1.6)
                    .min_by(|left, right| left.1.total_cmp(&right.1))
                    .map(|(index, _)| index);
                *ctx.handled |= self.dragging.is_some();
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                *ctx.handled |= self.dragging.take().is_some();
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "RoomCanvas"
    }

    fn test_id(&self) -> Option<&str> {
        Some("speakers.room")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ui::collaboration::{
        SpeakerDeviceInfo, SpeakerEndpointInfo, SpeakerGroupInfo, SpeakerMemberInfo,
        SpeakerMemberStatus,
    };

    use super::*;

    /// Answers from a fixed snapshot and records every command.
    struct FakeService {
        snapshot: Mutex<SpeakerSnapshot>,
        commands: Mutex<Vec<SpeakerCommand>>,
    }

    impl SpeakerService for FakeService {
        fn call(&self, command: SpeakerCommand) -> Result<SpeakerReply, String> {
            self.commands.lock().unwrap().push(command.clone());
            Ok(match command {
                SpeakerCommand::Snapshot => SpeakerReply::Snapshot {
                    snapshot: self.snapshot.lock().unwrap().clone(),
                },
                SpeakerCommand::Create { .. } => SpeakerReply::Done {
                    group: Some("ab".repeat(16)),
                },
                SpeakerCommand::Delete { .. } => SpeakerReply::Rejected {
                    reason: "that speaker set is gone".into(),
                },
                _ => SpeakerReply::Done { group: None },
            })
        }
    }

    fn device(id: &str) -> SpeakerDeviceInfo {
        SpeakerDeviceInfo {
            id: id.into(),
            label: id.into(),
            online: true,
            local: id == "desk",
            can_join: true,
            outputs: vec![SpeakerEndpointInfo {
                selector: "0a".repeat(16),
                label: "Speakers".into(),
                is_default: true,
            }],
            microphones: Vec::new(),
        }
    }

    fn fake() -> Arc<FakeService> {
        Arc::new(FakeService {
            snapshot: Mutex::new(SpeakerSnapshot {
                media_enabled: true,
                devices: vec![device("desk"), device("shelf")],
                groups: vec![SpeakerGroupInfo {
                    id: "cd".repeat(16),
                    name: "Kitchen".into(),
                    leader_label: "desk".into(),
                    led_here: true,
                    members: vec![SpeakerMemberInfo {
                        device: "shelf".into(),
                        device_label: "shelf".into(),
                        output: "0a".repeat(16),
                        output_label: None,
                        role: SpeakerRoleSpec::Room {
                            x_mm: 0,
                            y_mm: 1000,
                            z_mm: 0,
                        },
                        status: SpeakerMemberStatus::Ready,
                        trim: SpeakerTrimInfo::default(),
                    }],
                    playback: None,
                    microphones: Vec::new(),
                }],
                calibrations: Vec::new(),
            }),
            commands: Mutex::new(Vec::new()),
        })
    }

    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: Duration = Duration::from_secs(30);

    fn model(service: Arc<FakeService>) -> SpeakersModel {
        let profile = tempfile::tempdir().unwrap();
        SpeakersModel::new(
            PaneCx::in_file(profile.path().join("settings.json")),
            service,
        )
    }

    /// Run passes until the model has no call in flight.
    fn settle(model: &mut SpeakersModel) {
        let deadline = ui::time::now() + HANG_BOUND;
        while ui::time::now() < deadline {
            let _ = model.poll();
            if model.in_flight == 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("speaker calls did not settle");
    }

    /// Take `step` as its button does: the effect it starts runs too.
    fn press(model: &mut SpeakersModel, step: SpeakerFlow) {
        if let Some(job) = model.flow(step).unwrap() {
            model.start(job, None);
        }
        settle(model);
    }

    #[test]
    fn the_buttons_walk_a_stereo_pair_from_choice_to_saved() {
        let service = fake();
        let mut pane = model(service.clone());
        settle(&mut pane);
        assert_eq!(pane.setup().groups().len(), 1);
        press(&mut pane, SpeakerFlow::BeginSpeakerSetup);
        assert_eq!(pane.setup().step(), &Step::ChooseOutputs);
        settle(&mut pane);
        for (_, _, state) in &pane.chosen_states {
            state.set(true);
        }
        settle(&mut pane);
        press(&mut pane, SpeakerFlow::ContinueSpeakerSetup);
        assert!(matches!(
            pane.setup().step(),
            Step::Identify { index: 0, .. }
        ));
        press(&mut pane, SpeakerFlow::PlayIdentifySound);
        assert!(
            service
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|command| matches!(
                    command,
                    SpeakerCommand::Identify { device, .. } if device == "desk"
                ))
        );
        press(
            &mut pane,
            SpeakerFlow::AssignSpeakerRole {
                role: "left".into(),
            },
        );
        assert_eq!(pane.setup().step(), &Step::Name);
        pane.name_state.set("Living room".into());
        settle(&mut pane);
        press(&mut pane, SpeakerFlow::SaveSpeakerSet);
        let created = service
            .commands
            .lock()
            .unwrap()
            .iter()
            .find_map(|command| match command {
                SpeakerCommand::Create { name, members } => Some((name.clone(), members.clone())),
                _ => None,
            })
            .unwrap();
        assert_eq!(created.0, "Living room");
        assert_eq!(created.1[0].role, SpeakerRoleSpec::Left);
        assert_eq!(created.1[1].role, SpeakerRoleSpec::Right);
        assert_eq!(
            pane.setup().step(),
            &Step::Calibrate {
                group: "ab".repeat(16)
            }
        );
    }

    /// A refused effect reports the compositor's reason; deleting a set
    /// asks first.
    #[test]
    fn effects_report_the_compositors_answer() {
        use crate::resource::SettingsShell;
        use ui::prelude::TestApp;

        let service = fake();
        let mut pane = model(service.clone());
        settle(&mut pane);
        pane.start(
            SpeakerEffect::DeleteSpeakerSet {
                group: "cd".repeat(16),
            },
            None,
        );
        settle(&mut pane);
        assert!(
            service
                .commands
                .lock()
                .unwrap()
                .contains(&SpeakerCommand::Delete {
                    group: "cd".repeat(16)
                })
        );
        assert_eq!(pane.setup().message(), Some("that speaker set is gone"));

        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let group = "cd".repeat(16);
        app.harness().wait_until(HANG_BOUND, |harness| {
            has_speaker_group(&harness.resources())
        });
        app.effect_on(
            &format!("settings.speaker-group:{group}"),
            &SettingsEffect::from(SpeakerEffect::DeleteSpeakerSet { group }),
        )
        .expect_err("deleting a set asks first");
    }

    fn has_speaker_group(resources: &[ui::tilo_actions::Resource]) -> bool {
        resources.iter().any(|resource| {
            resource.id.as_str().starts_with("settings.speaker-group:")
                || has_speaker_group(&resource.children)
        })
    }

    fn microphone_switch(pane: &SpeakersModel, device: &str) -> ui::LocalState<bool> {
        pane.microphone_states
            .iter()
            .find(|(known, _)| known == device)
            .map(|(_, state)| state.clone())
            .unwrap()
    }

    /// The set's own shelf listens too by default; the phone is switched on,
    /// the listening position moves to it, and the shelf is switched off.
    fn sit_at_the_phone_without_the_shelf(pane: &mut SpeakersModel) {
        let phone = microphone_switch(pane, "phone");
        assert!(!phone.with(|on| *on));
        assert!(microphone_switch(pane, "shelf").with(|on| *on));
        phone.set(true);
        settle(pane);
        let Step::Calibrate { group } = pane.setup.step() else {
            panic!("the speaker set must be ready to calibrate");
        };
        let phone_label = pane
            .setup
            .listening_choices(group)
            .into_iter()
            .find(|microphone| microphone.device == "phone")
            .expect("the phone microphone is available")
            .label;
        pane.listening_state.set(phone_label);
        settle(pane);
        microphone_switch(pane, "shelf").set(false);
        settle(pane);
    }

    #[test]
    fn calibration_is_a_proposal_until_applied() {
        use ui::collaboration::{CalibrationInfo, CalibrationMemberInfo};
        let service = fake();
        {
            let mut snapshot = service.snapshot.lock().unwrap();
            for (device, local) in [("desk", true), ("shelf", false), ("phone", false)] {
                snapshot.groups[0]
                    .microphones
                    .push(ui::collaboration::CalibrationMicrophoneInfo {
                        device: device.into(),
                        device_label: device.into(),
                        label: "Microphone".into(),
                        local,
                        status: ui::collaboration::MicrophoneStatus::Ready,
                    });
            }
        }
        let mut pane = model(service.clone());
        settle(&mut pane);
        let group = "cd".repeat(16);
        press(
            &mut pane,
            SpeakerFlow::OpenCalibration {
                group: group.clone(),
            },
        );
        settle(&mut pane);
        assert_eq!(
            pane.setup().step(),
            &Step::Calibrate {
                group: group.clone()
            }
        );
        sit_at_the_phone_without_the_shelf(&mut pane);
        press(&mut pane, SpeakerFlow::StartCalibration);
        assert!(
            service
                .commands
                .lock()
                .unwrap()
                .contains(&SpeakerCommand::StartCalibration {
                    group: group.clone(),
                    excluded: vec!["desk".into(), "shelf".into()],
                    listening: Some("phone".into()),
                })
        );
        // The measurement finishes: the shell reports a proposal.
        service
            .snapshot
            .lock()
            .unwrap()
            .calibrations
            .push(CalibrationInfo {
                group: group.clone(),
                phase: CalibrationPhaseInfo::Proposed,
                members: vec![CalibrationMemberInfo {
                    device: "shelf".into(),
                    label: "shelf".into(),
                    state: CalibrationMemberState::Measured {
                        delay_ms: 4.0,
                        level_db: -6.0,
                        output_latency_ms: Some(1.0),
                        acoustic_ms: Some(3.0),
                    },
                    before: SpeakerTrimInfo::default(),
                    after: Some(SpeakerTrimInfo {
                        delay_ms: 2.5,
                        gain_db: -1.5,
                        eq_bands: 2,
                    }),
                }],
                microphones: Vec::new(),
                listening_label: Some("phone".into()),
                message: None,
            });
        pane.refresh();
        settle(&mut pane);
        assert!(
            !service
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|command| matches!(command, SpeakerCommand::ApplyCalibration { .. })),
            "a proposal is never applied implicitly"
        );
        pane.start(
            SpeakerEffect::ApplyCalibration {
                group: group.clone(),
            },
            None,
        );
        settle(&mut pane);
        assert!(
            service
                .commands
                .lock()
                .unwrap()
                .contains(&SpeakerCommand::ApplyCalibration { group })
        );
    }

    #[test]
    fn room_slider_units_round_trip_millimetres() {
        for mm in [-ROOM_EXTENT_MM, -1234, 0, 2000, ROOM_EXTENT_MM] {
            assert!((from_unit(to_unit(mm)) - mm).abs() <= 1, "{mm}");
        }
    }

    #[test]
    fn the_canvas_maps_screen_and_room_both_ways() {
        let mut canvas = RoomCanvas::new(ui::LocalState::new(vec![(0, 2000)]), vec!["A".into()]);
        canvas.rect = Rect::new(0.0, 0.0, 400.0, 260.0);
        let screen = canvas.to_screen((1500, -1000));
        let back = canvas.to_room(screen);
        assert!(
            (back.0 - 1500).abs() <= 20 && (back.1 + 1000).abs() <= 20,
            "{back:?}"
        );
        assert!(
            canvas.to_screen((0, 2000)).y < canvas.origin().y,
            "ahead is up"
        );
    }
}
