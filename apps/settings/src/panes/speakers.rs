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

mod room_canvas;

use std::rc::Rc;
use std::sync::Arc;

use ui::collaboration::{
    CalibrationMemberState, CalibrationMicrophoneState, CalibrationPhaseInfo, SpeakerCommand,
    SpeakerMemberSpec, SpeakerReply, SpeakerRoleSpec, SpeakerSnapshot, SpeakerTrimInfo,
};
use ui::framework::{EffectAnswer, EffectCx, EffectReply, Effected, LiveRow};
use ui::tilo_resource::{self, Effect};
use ui::widget::{Button, Dropdown, TextInput, Toggle, Widget};

use room_canvas::{RoomCanvas, RoomGesture};

use crate::channel::{Receiver, Sender, channel};
use crate::effects::SettingsEffect;
use crate::host::PaneCx;
use crate::i18n::{t, t_args};
use crate::panes::model::{PaneModel, Replies, Shared};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::speaker_setup::{
    Arrangement, BrokerSpeakers, SpeakerService, SpeakerSetup, Step, member_status_text,
    playback_text, role_answer, role_text,
};
use crate::widgets::{
    Item, SettingsSection, StatusTone, block, note, page, row, rows, section, semantic_status_pill,
    setting_row, setting_row_described, thing_row, value_row,
};

crate::effects::effect_family!(SpeakerFlow => answer_flow);

crate::effects::effect_family!(SpeakerEffect => answer);

crate::area::pane_area! {
    SpeakersArea;
    fn poll(&self) {
        SpeakersArea::poll(self);
    }

    fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        if let Some(model) = self.model.get() {
            model.borrow_mut().handle_collab_event(event);
        }
    }
}

/// The window's [`SpeakersArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&SpeakersArea> {
    app.areas().find()
}

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
    crate::panes::speakers::area(app)
        .ok_or_else(crate::area::missing)?
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
    let job = crate::panes::speakers::area(app)
        .ok_or_else(crate::area::missing)?
        .model()
        .borrow_mut()
        .flow(step)?;
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

/// Where the ask for the speaker sets stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refresh {
    Idle,
    Asked,
    /// The compositor said the sets changed while the ask was unanswered:
    /// its answer may show them from before, so one more ask follows it.
    AskedBeforeChange,
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
                !self.cx.reaches_devices(),
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
    /// its first frame ([`crate::panes::LIVE_AREAS`]).
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
    sample: bool,
    finished_tx: Sender<Finished>,
    finished_rx: Receiver<Finished>,
    in_flight: usize,
    refresh: Refresh,
    /// What a shown pane holds; a pane shown anew asks at once.
    shown: std::rc::Weak<()>,
    arrangement_state: ui::LocalState<String>,
    name_state: ui::LocalState<String>,
    listening_state: ui::LocalState<String>,
    microphone_states: Vec<(String, ui::LocalState<bool>)>,
    chosen_states: Vec<(String, String, ui::LocalState<bool>)>,
    /// Each room speaker's place, as the canvas stores it on release.
    room_points: ui::LocalState<Vec<(i32, i32)>>,
    last_room_points: Vec<(i32, i32)>,
    /// The canvas's drag or settle, kept across recompositions.
    room_gesture: ui::LocalState<RoomGesture>,
    /// The speaker chosen on the canvas or in the picker, by its place in
    /// the chosen list: one selection for both, and only its settings show.
    room_selected: ui::LocalState<String>,
    room_selected_seen: u64,
    /// The layout capture this model shows, if it is one.
    capture: Option<crate::scenarios::SpeakersSample>,
    replies: Replies,
    changed: bool,
}

impl SpeakersModel {
    fn new(cx: PaneCx, service: Arc<dyn SpeakerService>, sample: bool) -> Self {
        let (finished_tx, finished_rx) = channel();
        let mut model = Self {
            cx,
            setup: SpeakerSetup::default(),
            service,
            sample,
            finished_tx,
            finished_rx,
            in_flight: 0,
            refresh: Refresh::Idle,
            shown: std::rc::Weak::new(),
            arrangement_state: ui::LocalState::new(Arrangement::Stereo.label()),
            name_state: ui::LocalState::new(t("speakers-default-name")),
            listening_state: ui::LocalState::new(String::new()),
            microphone_states: Vec::new(),
            chosen_states: Vec::new(),
            room_points: ui::LocalState::new(Vec::new()),
            last_room_points: Vec::new(),
            room_gesture: ui::LocalState::new(RoomGesture::default()),
            room_selected: ui::LocalState::new(String::new()),
            room_selected_seen: 0,
            capture: None,
            replies: Replies::default(),
            changed: false,
        };
        model.refresh();
        if sample {
            model.poll_finished();
            model.stage_capture();
        }
        model
    }

    /// A capture of the room layout step: four speakers chosen as a room,
    /// each told apart, and the selection or drag the scenario shows.
    fn stage_capture(&mut self) {
        use crate::scenarios::SpeakersSample;
        let Some(sample) = self.cx.scene().and_then(|scene| scene.speakers) else {
            return;
        };
        self.setup.begin();
        self.setup.set_arrangement(Arrangement::Room);
        self.arrangement_state.set(Arrangement::Room.label());
        for candidate in self.setup.candidates() {
            self.setup.toggle(&candidate);
        }
        if self.setup.confirm_outputs().is_err() {
            return;
        }
        while matches!(self.setup.step(), Step::Identify { .. }) {
            self.setup.skip_identify();
        }
        self.sync_bound_states();
        self.capture = Some(sample);
        match sample {
            SpeakersSample::Layout => {}
            SpeakersSample::Picked => self.room_selected.set("2".into()),
            SpeakersSample::Dragging => {
                self.room_selected.set("2".into());
                self.room_gesture.set(RoomGesture::held(2, (1_500, 1_700)));
            }
        }
        self.room_selected_seen = self.room_selected.revision();
        self.changed = true;
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
        // Only the in-memory screenshot/test service runs here. Real broker
        // calls keep their worker so device I/O never blocks the UI thread.
        if self.sample {
            let _ = finished.send(Finished {
                purpose,
                result: service.call(command),
            });
            return;
        }
        let spawned = crate::device_read::thread(
            "settings-speakers",
            Box::new(move || {
                let result = service.call(command);
                let _ = finished.send(Finished { purpose, result });
            }),
        );
        if let Err(error) = spawned {
            self.in_flight = self.in_flight.saturating_sub(1);
            self.setup.set_message(Some(t_args(
                "speakers-service-unreachable",
                &[("error", error.to_string().into())],
            )));
            self.changed = true;
        }
    }

    /// Ask for the sets, unless an ask is already out.
    fn refresh(&mut self) {
        if self.refresh == Refresh::Idle {
            self.refresh = Refresh::Asked;
            self.call(Purpose::Refresh, SpeakerCommand::Snapshot);
        }
    }

    /// The compositor says what the sets show may have changed (a device came
    /// or went, a member answered, a calibration moved on): a shown pane asks
    /// once, a burst of changes during that ask once more. Nothing asks on a
    /// timer; a hidden pane asks when it is shown again.
    pub(crate) fn handle_collab_event(&mut self, event: &ui::collaboration::CollabEvent) {
        if !matches!(event, ui::collaboration::CollabEvent::SpeakersChanged)
            || self.shown.upgrade().is_none()
        {
            return;
        }
        if self.refresh == Refresh::Idle {
            self.refresh();
        } else {
            // The ask that is out may show the sets from before the change.
            self.refresh = Refresh::AskedBeforeChange;
        }
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
                    let again = self.refresh == Refresh::AskedBeforeChange;
                    self.refresh = Refresh::Idle;
                    match finished.result {
                        Ok(SpeakerReply::Snapshot { snapshot }) => self.adopt(snapshot),
                        Ok(SpeakerReply::Rejected { reason }) | Err(reason) => {
                            self.setup.set_message(Some(reason));
                            self.changed = true;
                        }
                        Ok(SpeakerReply::Done { .. }) => {}
                    }
                    if again {
                        self.refresh();
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
                    "speakers-done-created",
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
            self.room_points.set(points);
        }
        self.normalize_room_selection();
    }

    /// Keep the shared selection on a chosen speaker: the first one when
    /// none (or one no longer chosen) is selected, quietly.
    fn normalize_room_selection(&mut self) {
        let count = self.setup.chosen().len();
        let valid = self
            .room_selected
            .with(|selected| selected.parse::<usize>().is_ok_and(|index| index < count));
        if !valid && count > 0 {
            self.room_selected.set("0".into());
            self.room_selected_seen = self.room_selected.revision();
        }
    }

    /// The speaker selected on the canvas or in the picker.
    fn room_selection(&self) -> Option<usize> {
        self.room_selected
            .with(|selected| selected.parse::<usize>().ok())
            .filter(|index| *index < self.setup.chosen().len())
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
            // The canvas stores a place once, when a drag is let go.
            let placed = self.room_points.with(Clone::clone);
            for (index, point) in placed.iter().enumerate() {
                if self.last_room_points.get(index) != Some(point) {
                    self.setup.place(index, point.0, point.1);
                }
            }
            if self.setup.room_points() != self.last_room_points {
                self.changed = true;
            }
            // A choice on the canvas or in the picker shows that speaker's
            // settings, once the hand lets go of the canvas.
            let revision = self.room_selected.revision();
            if revision != self.room_selected_seen && !self.room_gesture.with(RoomGesture::holding)
            {
                self.room_selected_seen = revision;
                self.changed = true;
            }
        }
    }

    /// "Where you sit" and "Also listen with" for the calibrate step.
    fn microphone_rows(&self, group: &str) -> Vec<Item> {
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
        body.push(setting_row_described(
            &t("speakers-listening-title"),
            &t("speakers-listening-description"),
            Box::new(chooser),
        ));
        let extras = self.setup.extra_microphones(group);
        if !extras.is_empty() {
            body.push(note(&t("speakers-also-listen-caption")));
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

    fn nav_buttons(&self, back: bool, next: Option<(&str, &str, bool)>) -> Item {
        let mut buttons = ui::widget::Row::new();
        if back {
            buttons = buttons.push(Box::new(self.cx.bind_invocation(
                Button::new("speakers.back", &t("speakers-back")),
                SpeakerFlow::SpeakerSetupBack,
            )));
        }
        buttons = buttons.push(Box::new(ui::widget::Spacer::new()));
        if let Some((id, label, enabled)) = next {
            let step = match id {
                "speakers.save" => SpeakerFlow::SaveSpeakerSet,
                _ => SpeakerFlow::ContinueSpeakerSetup,
            };
            buttons = buttons.push(Box::new(
                self.cx.bind_invocation(
                    Button::new(id, label)
                        .role(ui::widget::ButtonRole::Primary)
                        .with_enabled(enabled),
                    step,
                ),
            ));
        }
        row(Box::new(buttons))
    }

    fn message_rows(&self) -> Vec<Item> {
        self.setup.message().map(note).into_iter().collect()
    }

    /// One saved set: its members, readiness, playback and actions.
    /// The controls of a set led here: calibrate it from its row; deleting
    /// it is in its menu.
    fn led_set_controls(&self, group: &ui::collaboration::SpeakerGroupInfo) -> Box<dyn Widget> {
        Box::new(
            ui::widget::Row::new()
                .no_wrap()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .with_align_items(ui::taffy::AlignItems::Center)
                .push(Box::new(
                    self.cx.bind_invocation(
                        Button::new(
                            &format!("speakers.group.{}.calibrate", group.id),
                            &t("speakers-calibrate-button"),
                        )
                        .control_size(ui::widget::ControlSize::Small),
                        SpeakerFlow::OpenCalibration {
                            group: group.id.clone(),
                        },
                    ),
                ))
                .push(Box::new(
                    ui::widget::RowMenu::new(
                        format!("speakers.group.{}.more", group.id),
                        &t_args("settings-row-more", &[("name", group.name.clone().into())]),
                    )
                    .destructive(
                        t("speakers-delete"),
                        self.cx.invocation_binding(SpeakerEffect::DeleteSpeakerSet {
                            group: group.id.clone(),
                        }),
                    ),
                )),
        )
    }

    fn group_row(&self, group: &ui::collaboration::SpeakerGroupInfo) -> Item {
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
        let mut tags = vec![(
            t_args(
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
        )];
        if let Some(playback) = &group.playback {
            tags.push((
                playback_text(playback),
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
        let trailing = group.led_here.then(|| self.led_set_controls(group));
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
        let tags: Vec<(&str, ui::widget::ChipStyle)> = tags
            .iter()
            .map(|(text, tone)| (text.as_str(), ui::widget::ChipStyle::Tinted(*tone)))
            .collect();
        row(Box::new(
            thing_row(
                &format!("speakers.group.{}", group.id),
                &group.name,
                &[description],
                trailing,
            )
            .icon(ui::icons::Icon::Speaker)
            .tags(&tags),
        ))
    }

    fn build_overview(&self) -> Vec<SettingsSection> {
        let mut sections = Vec::new();
        let group_rows = if self.setup.groups().is_empty() {
            vec![note(&t("speakers-overview-empty"))]
        } else {
            self.setup
                .groups()
                .iter()
                .map(|group| self.group_row(group))
                .collect()
        };
        let mut body = group_rows;
        body.extend(self.message_rows());
        body.push(row(Box::new(
            self.cx.bind_invocation(
                Button::new("speakers.begin", &t("speakers-begin"))
                    .role(ui::widget::ButtonRole::Primary),
                SpeakerFlow::BeginSpeakerSetup,
            ),
        )));
        sections.push(section(&t("speakers-overview-title"), rows(body)));
        sections
    }

    fn build_choose(&self) -> Vec<SettingsSection> {
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
        let mut body = vec![setting_row_described(
            &t("speakers-arrangement"),
            &self.setup.arrangement().description(),
            Box::new(arrangement),
        )];
        if candidates.is_empty() {
            body.push(note(&t("speakers-no-outputs")));
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
            body.push(note(&problem));
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
        vec![section(&t("speakers-choose-title"), rows(body))]
    }

    fn build_identify(&self, index: usize, played: bool) -> Vec<SettingsSection> {
        let count = self.setup.chosen().len();
        let mut body = vec![note(&t_args(
            "speakers-identify-intro",
            &[
                ("index", index.saturating_add(1).to_string().into()),
                ("count", count.to_string().into()),
            ],
        ))];
        body.push(row(Box::new(
            ui::widget::Row::new().push(Box::new(
                self.cx.bind_invocation(
                    Button::new("speakers.play", &t("speakers-play-sound"))
                        .with_icon(ui::icons::Icon::Volume2),
                    SpeakerFlow::PlayIdentifySound,
                ),
            )),
        )));
        if self.setup.arrangement() == Arrangement::Room {
            body.push(note(&t("speakers-identify-room-next")));
            body.push(self.nav_buttons(true, None));
            body.push(row(Box::new(
                ui::widget::Row::new()
                    .push(Box::new(ui::widget::Spacer::new()))
                    .push(Box::new(
                        self.cx.bind_invocation(
                            Button::new("speakers.skip", &t("speakers-next-speaker"))
                                .role(ui::widget::ButtonRole::Primary),
                            SpeakerFlow::SkipSpeaker,
                        ),
                    )),
            )));
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
                body.push(note(&t("speakers-identify-play-first")));
            }
            body.extend(self.message_rows());
            body.push(self.nav_buttons(true, None));
        }
        vec![section(&t("speakers-identify-title"), rows(body))]
    }

    /// The room layout: the canvas, directly under it the speaker picker
    /// (one selection for both), then the selected speaker's settings and
    /// nothing else of any other speaker.
    fn build_layout(&self) -> Vec<SettingsSection> {
        let chosen = self.setup.chosen();
        let labels = chosen
            .iter()
            .map(|candidate| candidate.device_label.clone())
            .collect::<Vec<_>>();
        let canvas = RoomCanvas::new(self.room_points.clone(), labels, self.room_gesture.clone())
            .with_selection(self.room_selected.clone());
        let mut picker = Dropdown::from_state(Vec::new(), &self.room_selected)
            .with_id("speakers.layout.speaker")
            .with_accessible_name(t("speakers-layout-speaker"))
            .open_if(self.capture == Some(crate::scenarios::SpeakersSample::Picked));
        picker.set_value_options(
            chosen
                .iter()
                .enumerate()
                .map(|(index, candidate)| (index.to_string(), speaker_name(chosen, candidate)))
                .collect(),
        );
        let mut body = vec![
            note(&t("speakers-layout-intro")),
            block(Box::new(canvas)),
            setting_row(&t("speakers-layout-speaker"), Box::new(picker)),
        ];
        if let Some(index) = self.room_selection() {
            let point = self
                .setup
                .room_points()
                .get(index)
                .copied()
                .unwrap_or_default();
            body.push(value_row(
                &t("speakers-layout-position"),
                &position_text(point),
            ));
            let label = chosen
                .get(index)
                .map(|candidate| speaker_name(chosen, candidate))
                .unwrap_or_default();
            body.push(setting_row(
                &t("speakers-layout-identify"),
                Box::new(
                    self.cx.bind_invocation(
                        Button::new(
                            "speakers.layout.play",
                            &t_args(
                                "speakers-layout-play-on",
                                &[("label", label.as_str().into())],
                            ),
                        )
                        .with_icon(ui::icons::Icon::Volume2),
                        SpeakerFlow::PlaySpeaker { speaker: index },
                    ),
                ),
            ));
        }
        if let Some(problem) = self.setup.layout_problem() {
            body.push(note(&problem));
        }
        body.push(self.nav_buttons(
            true,
            Some((
                "speakers.next",
                &t("speakers-continue"),
                self.setup.layout_problem().is_none(),
            )),
        ));
        vec![section(&t("speakers-layout-title"), rows(body))]
    }

    fn build_name(&self) -> Vec<SettingsSection> {
        let name = TextInput::from_state(&self.name_state)
            .with_id("speakers.name")
            .with_placeholder(t("speakers-name-placeholder"))
            .with_accessible_name(t("speakers-name"));
        let mut body = vec![setting_row_described(
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
        body.push(note(&t("speakers-name-join-note")));
        body.extend(self.message_rows());
        body.push(self.nav_buttons(
            true,
            Some(("speakers.save", &t("speakers-save"), self.in_flight == 0)),
        ));
        vec![section(&t("speakers-name-title"), rows(body))]
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one card lists members, measurement progress and the proposal actions together"
    )]
    fn build_calibrate(&self, group: &str) -> Vec<SettingsSection> {
        let Some(known) = self.setup.group(group) else {
            let mut body = self.message_rows();
            body.push(note(&t("speakers-saving")));
            body.push(row(Box::new(
                ui::widget::Row::new()
                    .push(Box::new(ui::widget::Spacer::new()))
                    .push(Box::new(self.cx.bind_invocation(
                        Button::new("speakers.done", &t("speakers-done")),
                        SpeakerFlow::CloseSpeakerSetup,
                    ))),
            )));
            return vec![section(&t("speakers-saved-title"), rows(body))];
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
                body.push(note(&t_args(
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
                body.push(note(&t("speakers-run-waiting-for-microphones")));
            }
            if let Some(message) = &run.message {
                body.push(note(message));
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
                    body.push(note(problem));
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
        body.push(row(Box::new(actions)));
        vec![section(
            &t_args(
                "speakers-calibrate-title",
                &[("name", known.name.as_str().into())],
            ),
            rows(body),
        )]
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        if !self.setup.snapshot().media_enabled && self.refresh == Refresh::Idle {
            return page(vec![section(
                &t("speakers-overview-title"),
                rows(vec![note(&t("speakers-media-off"))]),
            )]);
        }
        page(match self.setup.step().clone() {
            Step::Overview => self.build_overview(),
            Step::ChooseOutputs => self.build_choose(),
            Step::Identify { index, played } => self.build_identify(index, played),
            Step::Layout => self.build_layout(),
            Step::Name => self.build_name(),
            Step::Calibrate { group } => self.build_calibrate(&group),
        })
    }
}

/// What a chosen speaker is called: its device's name, with the output
/// only where one device gives the set two speakers.
fn speaker_name(
    chosen: &[crate::speaker_setup::Candidate],
    candidate: &crate::speaker_setup::Candidate,
) -> String {
    let shared = chosen
        .iter()
        .filter(|other| other.device == candidate.device)
        .nth(1)
        .is_some();
    if shared {
        candidate.label()
    } else {
        candidate.device_label.clone()
    }
}

/// Where a room speaker stands, as a person reads it: "1.2 m to your
/// left, 2.0 m in front of you".
fn position_text((x_mm, y_mm): (i32, i32)) -> String {
    let metres = |mm: i32| one_decimal(f64::from(mm.unsigned_abs()) / 1_000.0);
    // Within 5 cm reads as level: a tenth of a metre is what is shown.
    let across = match x_mm {
        x if x <= -50 => t_args(
            "speakers-layout-across-left",
            &[("metres", metres(x).into())],
        ),
        x if x >= 50 => t_args(
            "speakers-layout-across-right",
            &[("metres", metres(x).into())],
        ),
        _ => t("speakers-layout-across-centre"),
    };
    let depth = match y_mm {
        y if y >= 50 => t_args(
            "speakers-layout-depth-front",
            &[("metres", metres(y).into())],
        ),
        y if y <= -50 => t_args(
            "speakers-layout-depth-behind",
            &[("metres", metres(y).into())],
        ),
        _ => t("speakers-layout-depth-level"),
    };
    t_args(
        "speakers-layout-position-value",
        &[("across", across.into()), ("depth", depth.into())],
    )
}

impl PaneModel for SpeakersModel {
    fn compose(&self) -> Box<dyn Widget> {
        self.build_composition()
    }

    fn poll(&mut self) -> bool {
        self.poll_finished();
        self.read_controls();
        self.sync_bound_states();
        std::mem::take(&mut self.changed)
    }

    fn name(&self) -> &'static str {
        "SpeakersPane"
    }

    fn busy(&self) -> bool {
        self.in_flight > 0
    }

    fn follow(&mut self) -> Option<Rc<dyn std::any::Any>> {
        let anew = self.shown.upgrade().is_none();
        let shown = crate::watch::shared(&mut self.shown, || ());
        if anew {
            // Changes while no pane showed the model were not asked for.
            self.refresh();
        }
        Some(shown)
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
                    devices: vec![
                        device("Desk", true),
                        device("Shelf", false),
                        device("Bookcase", false),
                        device("Kitchen", false),
                    ],
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
                        output_here: false,
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use ui::collaboration::{
        SpeakerDeviceInfo, SpeakerEndpointInfo, SpeakerGroupInfo, SpeakerMemberInfo,
        SpeakerMemberStatus,
    };

    use ui::time::Duration;

    use super::*;

    #[test]
    fn sample_area_rows_do_not_wait_for_workers() {
        let profile = tempfile::tempdir().unwrap();
        let area = SpeakersArea::new(&PaneCx::in_file(profile.path().join("settings.json")));
        assert_eq!(
            area.rows().len(),
            1,
            "the speaker group publishes on the first read"
        );
        let model = area.model().borrow();
        assert_eq!(
            model.in_flight, 0,
            "sample rows are ready when the area reads"
        );
        assert_eq!(model.setup.groups().len(), 1);
    }

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
                    output_here: false,
                }],
                calibrations: Vec::new(),
            }),
            commands: Mutex::new(Vec::new()),
        })
    }

    fn model(service: Arc<FakeService>) -> SpeakersModel {
        let profile = tempfile::tempdir().unwrap();
        SpeakersModel::new(
            PaneCx::in_file(profile.path().join("settings.json")),
            service,
            false,
        )
    }

    /// Run passes until the model has no call in flight.
    fn settle(model: &mut SpeakersModel) {
        let deadline = ui::time::now() + tilo_test_env::hang_bound();
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

    /// The pane never asks on a timer, not even while something moves on
    /// its own (a setup flow, a device yet to answer): it asks when shown
    /// anew, once per change the compositor announces while shown, and once
    /// more for changes announced during an unanswered ask.
    #[test]
    fn a_shown_pane_asks_once_per_announced_change_and_never_on_a_timer() {
        use ui::collaboration::CollabEvent;
        let service = fake();
        service.snapshot.lock().unwrap().groups[0].members[0].status =
            SpeakerMemberStatus::AwaitingConsent;
        let mut pane = model(service.clone());
        settle(&mut pane);
        let asked = || {
            service
                .commands
                .lock()
                .unwrap()
                .iter()
                .filter(|command| matches!(command, SpeakerCommand::Snapshot))
                .count()
        };
        let shown = pane.follow();
        settle(&mut pane);
        let first = asked();
        assert_eq!(
            pane.next_wake(),
            None,
            "a device yet to answer sets no wake"
        );
        press(&mut pane, SpeakerFlow::BeginSpeakerSetup);
        assert_eq!(pane.next_wake(), None, "nor does an open setup flow");
        for _ in 0..3 {
            let _ = pane.poll();
        }
        assert_eq!(asked(), first, "and nothing is asked meanwhile");

        pane.handle_collab_event(&CollabEvent::SpeakersChanged);
        settle(&mut pane);
        assert_eq!(asked(), first + 1, "a change asks once");

        for _ in 0..3 {
            pane.handle_collab_event(&CollabEvent::SpeakersChanged);
        }
        settle(&mut pane);
        assert_eq!(
            asked(),
            first + 3,
            "a burst asks once, and once more for what came during that ask"
        );
        assert_eq!(pane.next_wake(), None, "after which nothing is due");

        drop(shown);
        pane.handle_collab_event(&CollabEvent::SpeakersChanged);
        settle(&mut pane);
        assert_eq!(asked(), first + 3, "a hidden pane does not ask");
        let _shown = pane.follow();
        settle(&mut pane);
        assert_eq!(asked(), first + 4, "it asks when shown again");
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
        app.harness()
            .wait_until(tilo_test_env::hang_bound(), |harness| {
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

    /// The pane walked to the room layout of every offered speaker.
    fn at_room_layout(service: Arc<FakeService>) -> SpeakersModel {
        let mut pane = model(service);
        settle(&mut pane);
        press(&mut pane, SpeakerFlow::BeginSpeakerSetup);
        pane.arrangement_state.set(Arrangement::Room.label());
        settle(&mut pane);
        for (_, _, state) in &pane.chosen_states {
            state.set(true);
        }
        settle(&mut pane);
        press(&mut pane, SpeakerFlow::ContinueSpeakerSetup);
        while matches!(pane.setup().step(), Step::Identify { .. }) {
            press(&mut pane, SpeakerFlow::SkipSpeaker);
        }
        assert_eq!(pane.setup().step(), &Step::Layout);
        settle(&mut pane);
        pane
    }

    #[test]
    fn the_layout_shows_one_canvas_one_picker_and_only_the_selected_speaker() {
        fn named(widget: &dyn Widget, name: &str) -> usize {
            usize::from(widget.name() == name)
                + widget
                    .children()
                    .into_iter()
                    .map(|child| named(child, name))
                    .sum::<usize>()
        }
        let mut pane = at_room_layout(fake());
        let said = |pane: &SpeakersModel| {
            crate::widgets::announced_text(pane.build_composition().as_ref())
        };
        let text = said(&pane);
        let count = |text: &[String], needle: &str| {
            text.iter().filter(|line| line.contains(needle)).count()
        };
        assert_eq!(count(&text, "Play a sound on "), 1, "{text:#?}");
        assert_eq!(count(&text, "Play a sound on desk"), 1, "{text:#?}");
        assert_eq!(count(&text, "Position"), 1, "{text:#?}");
        assert_eq!(count(&text, "left to right"), 0, "no X/Y rows: {text:#?}");
        assert_eq!(count(&text, "behind to in front"), 0, "{text:#?}");
        assert_eq!(
            count(&text, "Speakers in the room"),
            1,
            "one canvas: {text:#?}"
        );
        let tree = pane.build_composition();
        assert_eq!(named(tree.as_ref(), "Slider"), 0, "no slider on the step");
        assert_eq!(named(tree.as_ref(), "RoomCanvas"), 1, "one canvas");
        // The picker and the canvas hold the same selection: choosing the
        // other speaker shows its settings, and only its.
        pane.room_selected.set("1".into());
        assert!(pane.poll(), "a new choice shows that speaker");
        let text = said(&pane);
        assert_eq!(count(&text, "Play a sound on "), 1, "{text:#?}");
        assert_eq!(count(&text, "Play a sound on shelf"), 1, "{text:#?}");
    }

    #[test]
    fn a_room_speakers_place_reads_as_distances_from_the_listener() {
        assert_eq!(
            position_text((-1_234, 2_000)),
            "1.2 m to your left, 2.0 m in front of you"
        );
        assert_eq!(position_text((20, -30)), "Centred, level with you");
        assert_eq!(
            position_text((1_500, -2_500)),
            "1.5 m to your right, 2.5 m behind you"
        );
    }
}
