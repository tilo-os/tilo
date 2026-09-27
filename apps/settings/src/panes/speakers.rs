//! Speakers — play through several of your devices as one stereo pair,
//! surround set, or set of speakers around a room.
//!
//! The pane walks [`SpeakerSetup`]: choose outputs, listen to each and say
//! which one it was, optionally place room speakers, name and save. Saving
//! asks the other devices to join; once every speaker is ready the set can
//! be calibrated with any microphone, and the result is a before/after
//! proposal that changes nothing until it is applied.
//!
//! Every effect goes to the compositor through [`SpeakerService`] on a
//! worker thread, and every effect is also a typed `settings.speaker-*`
//! action, so agents and automation use the same door as the buttons.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId, transaction::Transactable};
use taffy::{NodeId, TaffyTree};
use ui::collaboration::{
    CalibrationMemberState, CalibrationPhaseInfo, SpeakerCommand, SpeakerReply, SpeakerRoleSpec,
    SpeakerSnapshot, SpeakerTrimInfo,
};
use ui::doc::read_bool;
use ui::theme::Theme;
use ui::widget::{Button, Dropdown, Slider, TextInput, Toggle, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};

use crate::actions::{LocalOperationStatus, SPEAKER_OPERATION_TYPE, SpeakerOperation};
use crate::speaker_setup::{
    Arrangement, BrokerSpeakers, ROOM_EXTENT_MM, SpeakerService, SpeakerSetup, Step,
};
use crate::widgets::{
    StatusTone, caption, card, cards, choice_row_described, rows, semantic_status_pill,
    setting_row, setting_row_described, value_row,
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

const BEGIN_KEY: &str = "speakers.begin.edit";
const BACK_KEY: &str = "speakers.back.edit";
const NEXT_KEY: &str = "speakers.next.edit";
const PLAY_KEY: &str = "speakers.play.edit";
const SKIP_KEY: &str = "speakers.skip.edit";
const SAVE_KEY: &str = "speakers.save.edit";
const CALIBRATE_KEY: &str = "speakers.calibrate.edit";
const APPLY_KEY: &str = "speakers.apply.edit";
const REVERT_KEY: &str = "speakers.revert.edit";
const DISMISS_KEY: &str = "speakers.dismiss.edit";
const DONE_KEY: &str = "speakers.done.edit";

fn role_key(role: SpeakerRoleSpec) -> String {
    format!("speakers.role.{}.edit", role_slug(role))
}

fn group_key(action: &str, group: &str) -> String {
    format!("speakers.group.{group}.{action}.edit")
}

fn speaker_play_key(index: usize) -> String {
    format!("speakers.layout.{index}.play.edit")
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

/// "Left", "Subwoofer", …, as a button answers "Which one just played?".
fn role_answer(role: SpeakerRoleSpec) -> String {
    let text = role.describe().trim_end_matches(" speaker");
    let mut characters = text.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(characters).collect()
    })
}

fn trim_summary(trim: &SpeakerTrimInfo) -> String {
    format!("{:.1} ms · {:+.1} dB", trim.delay_ms, trim.gain_db)
}

/// What a finished background call was for.
#[derive(Clone, Debug)]
enum Purpose {
    Refresh,
    /// A typed action whose admission token must be completed.
    Operation(SpeakerOperation),
    /// A button in the flow.
    Flow(SpeakerOperation),
}

struct Finished {
    purpose: Purpose,
    result: Result<SpeakerReply, String>,
}

pub struct SpeakersPane {
    setup: SpeakerSetup,
    service: Arc<dyn SpeakerService>,
    finished_tx: Sender<Finished>,
    finished_rx: Receiver<Finished>,
    in_flight: usize,
    refreshing: bool,
    next_refresh: Instant,
    delete_armed: HashSet<String>,
    arrangement_state: ui::LocalState<String>,
    name_state: ui::LocalState<String>,
    microphone_state: ui::LocalState<String>,
    chosen_states: Vec<(String, String, ui::LocalState<bool>)>,
    room_points: ui::LocalState<Vec<(i32, i32)>>,
    placement_states: Vec<(ui::LocalState<f64>, ui::LocalState<f64>)>,
    last_room_points: Vec<(i32, i32)>,
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
    command_state: Option<ui::LocalState<Option<SpeakerOperation>>>,
    operation_status: Option<ui::LocalState<LocalOperationStatus>>,
}

impl Default for SpeakersPane {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeakersPane {
    #[must_use]
    pub fn new() -> Self {
        Self::with_service(Arc::new(BrokerSpeakers))
    }

    #[must_use]
    pub fn with_service(service: Arc<dyn SpeakerService>) -> Self {
        let (finished_tx, finished_rx) = channel();
        let mut pane = Self {
            setup: SpeakerSetup::default(),
            service,
            finished_tx,
            finished_rx,
            in_flight: 0,
            refreshing: false,
            next_refresh: Instant::now(),
            delete_armed: HashSet::new(),
            arrangement_state: ui::LocalState::new(Arrangement::Stereo.label().to_string()),
            name_state: ui::LocalState::new("Speakers".into()),
            microphone_state: ui::LocalState::new(String::new()),
            chosen_states: Vec::new(),
            room_points: ui::LocalState::new(Vec::new()),
            placement_states: Vec::new(),
            last_room_points: Vec::new(),
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
            command_state: None,
            operation_status: None,
        };
        pane.refresh();
        pane
    }

    #[must_use]
    pub fn with_command_state(mut self, state: &ui::LocalState<Option<SpeakerOperation>>) -> Self {
        self.command_state = Some(state.clone());
        self
    }

    #[must_use]
    pub fn with_operation_status(mut self, status: &ui::LocalState<LocalOperationStatus>) -> Self {
        self.operation_status = Some(status.clone());
        self
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
            self.setup.set_message(Some(format!(
                "Could not reach the speaker service: {error}"
            )));
            self.structure_dirty = true;
        }
    }

    fn refresh(&mut self) {
        if !self.refreshing {
            self.refreshing = true;
            self.call(Purpose::Refresh, SpeakerCommand::Snapshot);
        }
        self.next_refresh = Instant::now() + REFRESH_INTERVAL;
    }

    fn run(&mut self, operation: SpeakerOperation, typed: bool) {
        let command = operation.command();
        self.call(
            if typed {
                Purpose::Operation(operation)
            } else {
                Purpose::Flow(operation)
            },
            command,
        );
    }

    fn finish_operation(&self, operation: &SpeakerOperation, result: Result<String, String>) {
        let Some(status) = &self.operation_status else {
            return;
        };
        let token = crate::actions::local_operation_token_for(SPEAKER_OPERATION_TYPE, operation);
        status.set(match result {
            Ok(message) => LocalOperationStatus::Succeeded { token, message },
            Err(message) => LocalOperationStatus::Failed { token, message },
        });
    }

    fn adopt(&mut self, snapshot: SpeakerSnapshot) {
        crate::speaker_setup::remember_snapshot(&snapshot);
        let before = self.setup.snapshot().clone();
        self.setup.update(snapshot);
        if self.setup.snapshot() != &before {
            self.structure_dirty = true;
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
                            self.structure_dirty = true;
                        }
                        Ok(SpeakerReply::Done { .. }) => {}
                    }
                }
                Purpose::Operation(operation) | Purpose::Flow(operation) => {
                    let outcome = match finished.result {
                        Ok(SpeakerReply::Done { group }) => Ok(group),
                        Ok(SpeakerReply::Rejected { reason }) | Err(reason) => Err(reason),
                        Ok(SpeakerReply::Snapshot { .. }) => {
                            Err("the speaker service answered unexpectedly".into())
                        }
                    };
                    self.after_operation(&operation, &outcome);
                    self.refresh();
                }
            }
        }
    }

    fn after_operation(
        &mut self,
        operation: &SpeakerOperation,
        outcome: &Result<Option<String>, String>,
    ) {
        let message = match (outcome, operation) {
            (Err(reason), _) => Err(reason.clone()),
            (Ok(group), SpeakerOperation::Create { name, .. }) => {
                if let Some(group) = group {
                    self.setup.saved(group.clone());
                }
                Ok(format!("Saved {name}"))
            }
            (Ok(_), SpeakerOperation::Identify { .. }) => Ok("Playing a sound".into()),
            (Ok(_), SpeakerOperation::Calibrate { .. }) => Ok("Calibrating".into()),
            (Ok(_), SpeakerOperation::ApplyCalibration { .. }) => Ok("Calibration applied".into()),
            (Ok(_), SpeakerOperation::RevertCalibration { .. }) => {
                Ok("Previous calibration restored".into())
            }
            (Ok(_), SpeakerOperation::DismissCalibration { .. }) => {
                Ok("Calibration discarded".into())
            }
            (Ok(_), SpeakerOperation::Delete { .. }) => Ok("Speaker set deleted".into()),
        };
        match &message {
            Err(reason) => self.setup.set_message(Some(reason.clone())),
            Ok(_) if !matches!(operation, SpeakerOperation::Create { .. }) => {
                self.setup.set_message(None);
            }
            Ok(_) => {}
        }
        self.finish_operation(operation, message);
        self.structure_dirty = true;
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
            self.structure_dirty = true;
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
            self.structure_dirty = true;
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
                self.structure_dirty = true;
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
                self.structure_dirty = true;
            }
        }
    }

    fn microphone(&self) -> Option<crate::speaker_setup::Microphone> {
        let chosen = self.microphone_state.with(Clone::clone);
        let microphones = self.setup.microphones();
        microphones
            .iter()
            .find(|microphone| microphone.label == chosen)
            .or_else(|| microphones.first())
            .cloned()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one pass consumes every latch of the flow in step order"
    )]
    fn read_latches(&mut self, state: &mut AutoCommit, root: &ObjId) {
        let mut take = |key: &str| {
            let pressed = read_bool(state, root, key) == Some(true);
            if pressed {
                let _ = state.put(root, key, false);
            }
            pressed
        };
        if take(BEGIN_KEY) {
            self.setup.begin();
            self.arrangement_state
                .set(Arrangement::Stereo.label().to_string());
            self.name_state.set(self.setup.name().to_string());
            self.chosen_states.clear();
            self.structure_dirty = true;
        }
        if take(BACK_KEY) {
            self.setup.back();
            self.structure_dirty = true;
        }
        if take(DONE_KEY) {
            self.setup.close();
            self.structure_dirty = true;
        }
        if take(NEXT_KEY) {
            match self.setup.step() {
                Step::ChooseOutputs => {
                    if let Err(problem) = self.setup.confirm_outputs() {
                        self.setup.set_message(Some(problem));
                    } else {
                        self.setup.set_message(None);
                    }
                }
                Step::Layout => self.setup.finish_layout(),
                _ => {}
            }
            self.structure_dirty = true;
        }
        if take(PLAY_KEY)
            && let Some(SpeakerCommand::Identify { device, output }) = self.setup.identify_command()
        {
            self.run(SpeakerOperation::Identify { device, output }, false);
            self.structure_dirty = true;
        }
        if take(SKIP_KEY) {
            self.setup.skip_identify();
            self.structure_dirty = true;
        }
        for role in [
            SpeakerRoleSpec::Left,
            SpeakerRoleSpec::Right,
            SpeakerRoleSpec::Center,
            SpeakerRoleSpec::Lfe,
            SpeakerRoleSpec::SurroundLeft,
            SpeakerRoleSpec::SurroundRight,
        ] {
            if take(&role_key(role)) {
                if let Err(problem) = self.setup.assign(role) {
                    self.setup.set_message(Some(problem));
                } else {
                    self.setup.set_message(None);
                }
                self.structure_dirty = true;
            }
        }
        for index in 0..self.setup.chosen().len() {
            if take(&speaker_play_key(index))
                && let Some(candidate) = self.setup.chosen().get(index).cloned()
            {
                self.run(
                    SpeakerOperation::Identify {
                        device: candidate.device,
                        output: candidate.output,
                    },
                    false,
                );
            }
        }
        if take(SAVE_KEY) {
            match self.setup.save_command() {
                Ok(SpeakerCommand::Create { name, members }) => {
                    self.run(SpeakerOperation::Create { name, members }, false);
                }
                Ok(_) => {}
                Err(problem) => self.setup.set_message(Some(problem)),
            }
            self.structure_dirty = true;
        }
        let calibrating = match self.setup.step() {
            Step::Calibrate { group } => Some(group.clone()),
            _ => None,
        };
        if let Some(group) = calibrating {
            if take(CALIBRATE_KEY) {
                match self
                    .microphone()
                    .ok_or_else(|| "This device has no microphone to calibrate with".to_string())
                    .and_then(|microphone| self.setup.calibrate_command(&group, &microphone))
                {
                    Ok(SpeakerCommand::StartCalibration {
                        group,
                        microphone_device,
                        microphone,
                    }) => self.run(
                        SpeakerOperation::Calibrate {
                            group,
                            microphone_device,
                            microphone,
                        },
                        false,
                    ),
                    Ok(_) => {}
                    Err(problem) => self.setup.set_message(Some(problem)),
                }
                self.structure_dirty = true;
            }
            for (key, operation) in [
                (
                    APPLY_KEY,
                    SpeakerOperation::ApplyCalibration {
                        group: group.clone(),
                    },
                ),
                (
                    REVERT_KEY,
                    SpeakerOperation::RevertCalibration {
                        group: group.clone(),
                    },
                ),
                (DISMISS_KEY, SpeakerOperation::DismissCalibration { group }),
            ] {
                if take(key) {
                    self.run(operation, false);
                }
            }
        }
        for group in self.setup.groups().to_vec() {
            if take(&group_key("calibrate", &group.id)) {
                self.setup.open_calibration(&group.id);
                self.structure_dirty = true;
            }
            if take(&group_key("delete", &group.id)) {
                if self.delete_armed.remove(&group.id) {
                    self.run(
                        SpeakerOperation::Delete {
                            group: group.id.clone(),
                        },
                        false,
                    );
                } else {
                    self.delete_armed.insert(group.id.clone());
                }
                self.structure_dirty = true;
            }
            if take(&group_key("keep", &group.id)) {
                self.delete_armed.remove(&group.id);
                self.structure_dirty = true;
            }
        }
    }

    fn nav_buttons(back: bool, next: Option<(&str, &str, bool)>) -> Box<dyn Widget> {
        let mut row = ui::widget::Row::new();
        if back {
            row = row.push(Box::new(Button::new(
                "speakers.back",
                "Back",
                Some(BACK_KEY),
            )));
        }
        row = row.push(Box::new(ui::widget::Spacer::new()));
        if let Some((id, label, enabled)) = next {
            let key = match id {
                "speakers.save" => SAVE_KEY,
                _ => NEXT_KEY,
            };
            row = row.push(Box::new(
                Button::new(id, label, Some(key))
                    .primary()
                    .with_enabled(enabled),
            ));
        }
        Box::new(row)
    }

    fn message_rows(&self) -> Vec<Box<dyn Widget>> {
        self.setup.message().map(caption).into_iter().collect()
    }

    fn build_overview(&self) -> Vec<Box<dyn Widget>> {
        let mut sections = Vec::new();
        let group_rows = if self.setup.groups().is_empty() {
            vec![caption(
                "Use speakers on several of your devices together: a stereo pair, surround, or speakers around the room.",
            )]
        } else {
            self.setup
                .groups()
                .iter()
                .map(|group| {
                    let ready = group
                        .members
                        .iter()
                        .filter(|member| {
                            member.status == ui::collaboration::SpeakerMemberStatus::Ready
                        })
                        .count();
                    let summary = group
                        .members
                        .iter()
                        .map(|member| {
                            format!("{} ({})", member.device_label, member.role.describe())
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let mut actions = ui::widget::Row::new().push(semantic_status_pill(
                        &format!("{ready} of {} ready", group.members.len()),
                        if ready == group.members.len() {
                            StatusTone::Success
                        } else {
                            StatusTone::Warning
                        },
                    ));
                    if group.led_here {
                        let calibrate = group_key("calibrate", &group.id);
                        actions = actions.push(Box::new(Button::new(
                            &calibrate,
                            "Calibrate",
                            Some(&calibrate),
                        )));
                        let delete = group_key("delete", &group.id);
                        if self.delete_armed.contains(&group.id) {
                            let keep = group_key("keep", &group.id);
                            actions = actions
                                .push(Box::new(Button::new(&keep, "Keep", Some(&keep))))
                                .push(Box::new(
                                    Button::new(&delete, "Delete", Some(&delete)).danger(),
                                ));
                        } else {
                            actions = actions.push(Box::new(Button::new(
                                &delete,
                                "Delete…",
                                Some(&delete),
                            )));
                        }
                    }
                    setting_row_described(
                        &group.name,
                        &if group.led_here {
                            summary
                        } else {
                            format!("{summary} · set up on {}", group.leader_label)
                        },
                        Box::new(actions),
                    )
                })
                .collect()
        };
        let mut body = group_rows;
        body.extend(self.message_rows());
        body.push(Box::new(
            ui::widget::Row::new()
                .push(Box::new(ui::widget::Spacer::new()))
                .push(Box::new(
                    Button::new("speakers.begin", "Set up speakers", Some(BEGIN_KEY)).primary(),
                )),
        ));
        sections.push(card("Speakers across your devices", rows(body)));
        sections
    }

    fn build_choose(&self) -> Vec<Box<dyn Widget>> {
        let arrangement = Dropdown::from_state(
            Arrangement::ALL
                .iter()
                .map(|arrangement| arrangement.label().to_string())
                .collect(),
            &self.arrangement_state,
        )
        .with_id("speakers.arrangement")
        .with_accessible_name("Arrangement");
        let candidates = self.setup.candidates();
        let mut body = vec![choice_row_described(
            "Arrangement",
            match self.setup.arrangement() {
                Arrangement::Stereo => "Two speakers play left and right",
                Arrangement::Surround => "Three to six speakers in named positions",
                Arrangement::Room => "Speakers placed anywhere around you",
            },
            Box::new(arrangement),
        )];
        if candidates.is_empty() {
            body.push(caption(
                "No speakers are available. Turn on Media in Devices & Sync on each device you want to use.",
            ));
        }
        for (candidate, (_, _, state)) in candidates.iter().zip(&self.chosen_states) {
            body.push(setting_row_described(
                &candidate.label(),
                if candidate.local {
                    "This device"
                } else {
                    "Asks for approval on that device when you save"
                },
                Box::new(
                    Toggle::from_state(state)
                        .with_id(&format!(
                            "speakers.choose.{}.{}",
                            candidate.device, candidate.output
                        ))
                        .with_accessible_name(&format!("Use {}", candidate.label())),
                ),
            ));
        }
        if let Some(problem) = self.setup.outputs_problem() {
            body.push(caption(&problem));
        }
        body.extend(self.message_rows());
        body.push(Self::nav_buttons(
            true,
            Some((
                "speakers.next",
                "Continue",
                self.setup.outputs_problem().is_none(),
            )),
        ));
        vec![card("Choose speakers", rows(body))]
    }

    fn build_identify(&self, index: usize, played: bool) -> Vec<Box<dyn Widget>> {
        let count = self.setup.chosen().len();
        let mut body = vec![caption(&format!(
            "Speaker {} of {count}. Play a sound and listen for which speaker it comes from.",
            index.saturating_add(1)
        ))];
        body.push(Box::new(
            ui::widget::Row::new()
                .push(Box::new(
                    Button::new("speakers.play", "Play sound", Some(PLAY_KEY))
                        .with_icon(ui::icons::Icon::Volume2),
                ))
                .push(Box::new(ui::widget::Spacer::new())),
        ));
        if self.setup.arrangement() == Arrangement::Room {
            body.push(caption(
                "You will place it on the room layout next; each speaker starts on an arc in front of you.",
            ));
            body.push(Self::nav_buttons(true, None));
            body.push(Box::new(
                ui::widget::Row::new()
                    .push(Box::new(ui::widget::Spacer::new()))
                    .push(Box::new(
                        Button::new("speakers.skip", "Next speaker", Some(SKIP_KEY)).primary(),
                    )),
            ));
        } else {
            let mut answers = ui::widget::Row::new();
            for role in self.setup.remaining_roles() {
                let key = role_key(role);
                answers = answers.push(Box::new(
                    Button::new(
                        &format!("speakers.role.{}", role_slug(role)),
                        &role_answer(role),
                        Some(&key),
                    )
                    .with_enabled(played)
                    .with_accessible_name(&format!("It was the {}", role.describe())),
                ));
            }
            body.push(setting_row("Which one just played?", Box::new(answers)));
            if !played {
                body.push(caption("Play the sound first."));
            }
            body.extend(self.message_rows());
            body.push(Self::nav_buttons(true, None));
        }
        vec![card("Which one just played?", rows(body))]
    }

    fn build_layout(&self) -> Vec<Box<dyn Widget>> {
        let labels = self
            .setup
            .chosen()
            .iter()
            .map(|candidate| candidate.device_label.clone())
            .collect::<Vec<_>>();
        let mut body = vec![
            caption("Drag each speaker to where it stands. You sit at the marker."),
            Box::new(RoomCanvas::new(self.room_points.clone(), labels.clone())),
        ];
        for (index, ((x, y), label)) in self.placement_states.iter().zip(&labels).enumerate() {
            let play = speaker_play_key(index);
            body.push(setting_row(
                label,
                Box::new(
                    ui::widget::Row::new()
                        .push(Box::new(
                            Slider::from_state(x)
                                .with_id(&format!("speakers.layout.{index}.x"))
                                .with_accessible_name(&format!("{label}: left to right")),
                        ))
                        .push(Box::new(
                            Slider::from_state(y)
                                .with_id(&format!("speakers.layout.{index}.y"))
                                .with_accessible_name(&format!("{label}: behind to in front")),
                        ))
                        .push(Box::new(
                            Button::new(&play, "Play", Some(&play))
                                .with_accessible_name(&format!("Play a sound on {label}")),
                        )),
                ),
            ));
        }
        if let Some(problem) = self.setup.layout_problem() {
            body.push(caption(&problem));
        }
        body.push(Self::nav_buttons(
            true,
            Some((
                "speakers.next",
                "Continue",
                self.setup.layout_problem().is_none(),
            )),
        ));
        vec![card("Room layout", rows(body))]
    }

    fn build_name(&self) -> Vec<Box<dyn Widget>> {
        let name = TextInput::from_state(&self.name_state)
            .with_id("speakers.name")
            .with_placeholder("Living room")
            .with_accessible_name("Name");
        let mut body = vec![choice_row_described(
            "Name",
            "Shown wherever you can play to these speakers",
            Box::new(name),
        )];
        for (candidate, role) in self.setup.chosen().iter().zip(self.setup.roles()) {
            body.push(value_row(
                &candidate.label(),
                role.map_or("—", SpeakerRoleSpec::describe),
            ));
        }
        body.push(caption(
            "Your other devices each ask once whether to join. Nothing plays there until you approve.",
        ));
        body.extend(self.message_rows());
        body.push(Self::nav_buttons(
            true,
            Some(("speakers.save", "Save", self.in_flight == 0)),
        ));
        vec![card("Name your speakers", rows(body))]
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one card lists members, measurement progress and the proposal actions together"
    )]
    fn build_calibrate(&self, group: &str) -> Vec<Box<dyn Widget>> {
        let Some(known) = self.setup.group(group) else {
            let mut body = self.message_rows();
            body.push(caption("Saving your speakers…"));
            body.push(Box::new(
                ui::widget::Row::new()
                    .push(Box::new(ui::widget::Spacer::new()))
                    .push(Box::new(Button::new(
                        "speakers.done",
                        "Done",
                        Some(DONE_KEY),
                    ))),
            ));
            return vec![card("Speakers saved", rows(body))];
        };
        let mut body = Vec::new();
        for member in &known.members {
            body.push(setting_row_described(
                &member.device_label,
                &format!(
                    "{} · {}",
                    member.role.describe(),
                    trim_summary(&member.trim)
                ),
                semantic_status_pill(
                    member.status.label(),
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
        let microphones = self.setup.microphones();
        if !matches!(
            phase,
            Some(CalibrationPhaseInfo::Measuring | CalibrationPhaseInfo::Proposed)
        ) && !microphones.is_empty()
        {
            let chooser = Dropdown::from_state(
                microphones
                    .iter()
                    .map(|microphone| microphone.label.clone())
                    .collect(),
                &self.microphone_state,
            )
            .with_id("speakers.microphone")
            .with_accessible_name("Microphone")
            .with_placeholder(
                microphones
                    .first()
                    .map_or("Microphone", |microphone| microphone.label.as_str()),
            );
            body.push(choice_row_described(
                "Microphone",
                "Put it where you usually sit",
                Box::new(chooser),
            ));
        }
        if let Some(run) = run {
            for member in &run.members {
                let status = match &member.state {
                    CalibrationMemberState::Waiting => "Waiting".to_string(),
                    CalibrationMemberState::Measuring => "Listening…".to_string(),
                    CalibrationMemberState::Measured { delay_ms, level_db } => {
                        format!("Heard after {delay_ms:.1} ms at {level_db:+.1} dB")
                    }
                    CalibrationMemberState::Failed { reason } => reason.clone(),
                };
                let detail = member.after.as_ref().map_or_else(
                    || status.clone(),
                    |after| {
                        format!(
                            "Before {} → after {}",
                            trim_summary(&member.before),
                            trim_summary(after)
                        )
                    },
                );
                body.push(value_row(&member.label, &detail));
            }
            if let Some(message) = &run.message {
                body.push(caption(message));
            }
        }
        body.extend(self.message_rows());
        let mut actions = ui::widget::Row::new()
            .push(Box::new(Button::new(
                "speakers.done",
                "Done",
                Some(DONE_KEY),
            )))
            .push(Box::new(ui::widget::Spacer::new()));
        match phase {
            Some(CalibrationPhaseInfo::Proposed) => {
                actions = actions
                    .push(Box::new(Button::new(
                        "speakers.dismiss",
                        "Discard",
                        Some(DISMISS_KEY),
                    )))
                    .push(Box::new(
                        Button::new("speakers.apply", "Apply", Some(APPLY_KEY)).primary(),
                    ));
            }
            Some(CalibrationPhaseInfo::Applied) => {
                actions = actions.push(Box::new(Button::new(
                    "speakers.revert",
                    "Revert",
                    Some(REVERT_KEY),
                )));
            }
            Some(CalibrationPhaseInfo::Measuring) => {}
            Some(CalibrationPhaseInfo::Failed) | None => {
                let problem = self.setup.calibration_problem(group);
                if let Some(problem) = &problem {
                    body.push(caption(problem));
                }
                actions = actions.push(Box::new(
                    Button::new(
                        "speakers.calibrate",
                        if phase.is_some() {
                            "Try again"
                        } else {
                            "Calibrate"
                        },
                        Some(CALIBRATE_KEY),
                    )
                    .primary()
                    .with_enabled(problem.is_none()),
                ));
            }
        }
        body.push(Box::new(actions));
        vec![card(&format!("Calibrate {}", known.name), rows(body))]
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        if !self.setup.snapshot().media_enabled && !self.refreshing {
            return cards(vec![card(
                "Speakers across your devices",
                rows(vec![caption(
                    "Turn on Media in Devices & Sync to play through speakers on several of your devices.",
                )]),
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

impl Widget for SpeakersPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        self.poll_finished();
        if let Some(command_state) = &self.command_state
            && let Some(operation) = command_state.get()
        {
            command_state.set(None);
            self.run(operation, true);
        }
        self.read_controls();
        self.read_latches(state, root);
        self.sync_bound_states();
        if Instant::now() >= self.next_refresh {
            self.refresh();
        }
        if let Some(composed) = &mut self.composed {
            composed.sync(state, root);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut composed = self.build_composition();
        let child = composed.build_layout(taffy, theme, node_map);
        self.composed = Some(composed);
        let node = taffy
            .new_with_children(
                taffy::Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    size: taffy::Size {
                        width: taffy::Dimension::percent(1.0),
                        height: taffy::Dimension::auto(),
                    },
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
            && let Some(child) = children.first()
        {
            composed.apply_layout(taffy, *child, Point::new(self.rect.x0, self.rect.y0));
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
        if let Some(composed) = &mut self.composed {
            composed.draw(scene, transform, text_renderer, theme, presence);
        }
    }

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
        if let Some(composed) = &mut self.composed
            && let Ok(children) = taffy.children(node_id)
            && let Some(child) = children.first()
        {
            composed.handle_event(
                event,
                scale_factor,
                modifiers,
                state,
                path,
                handled,
                focus_target,
                *child,
                taffy,
            );
        }
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

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        own || self
            .composed
            .as_mut()
            .is_some_and(|composed| composed.take_structure_dirty())
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        if self.in_flight > 0 {
            Some(Instant::now() + POLL_INTERVAL)
        } else {
            Some(self.next_refresh)
        }
    }

    fn name(&self) -> &'static str {
        "SpeakersPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
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
}

const CANVAS_HEIGHT: f32 = 260.0; // floor-plan height, not a spacing token
const DOT_RADIUS: f64 = 11.0; // speaker marker size on the floor plan

impl RoomCanvas {
    fn new(points: ui::LocalState<Vec<(i32, i32)>>, labels: Vec<String>) -> Self {
        Self {
            points,
            labels,
            rect: Rect::ZERO,
            cursor: Point::ZERO,
            dragging: None,
        }
    }

    /// Millimetres per logical pixel, fitting the room extent in the shorter side.
    fn scale(&self) -> f64 {
        let half = (self.rect.width() / 2.0)
            .min(self.rect.height() / 2.0)
            .max(1.0);
        f64::from(ROOM_EXTENT_MM) / (half - DOT_RADIUS).max(1.0)
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
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = ui::widget::TaffyTreeExt::leaf_node(
            taffy,
            taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::length(CANVAS_HEIGHT),
                },
                ..Default::default()
            },
        );
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
        let radius = f64::from(theme.tokens.radii.md);
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colors.overlay_subtle,
            None,
            &RoundedRect::from_rect(self.rect, radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            colors.border_standard,
            None,
            &RoundedRect::from_rect(self.rect, radius),
        );
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
        text_renderer.draw_text(
            scene,
            "You",
            theme.tokens.typography.scale.xs,
            colors.text_dim,
            transform.pre_translate((origin.x + 8.0, origin.y + 4.0).into()),
            None,
        );
        let points = self.points.with(Clone::clone);
        for (index, point) in points.iter().enumerate() {
            let center = self.to_screen(*point);
            let fill = colors.categorical(index);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                fill,
                None,
                &Circle::new(center, DOT_RADIUS),
            );
            if self.dragging == Some(index) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    colors.border_focus,
                    None,
                    &Circle::new(center, DOT_RADIUS + 3.0),
                );
            }
            if let Some(label) = self.labels.get(index) {
                text_renderer.draw_text(
                    scene,
                    label,
                    theme.tokens.typography.scale.xs,
                    colors.text_primary,
                    transform.pre_translate((center.x + DOT_RADIUS + 4.0, center.y - 7.0).into()),
                    Some(140.0),
                );
            }
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                if let Some(index) = self.dragging {
                    let moved = self.to_room(self.cursor);
                    self.points.update(|points| {
                        if let Some(point) = points.get_mut(index) {
                            *point = moved;
                        }
                    });
                    *handled = true;
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
                    .filter(|(_, distance)| *distance <= DOT_RADIUS * 1.6)
                    .min_by(|left, right| left.1.total_cmp(&right.1))
                    .map(|(index, _)| index);
                *handled |= self.dragging.is_some();
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                *handled |= self.dragging.take().is_some();
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
                }],
                calibrations: Vec::new(),
            }),
            commands: Mutex::new(Vec::new()),
        })
    }

    /// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
    const HANG_BOUND: Duration = Duration::from_secs(30);

    /// Run sync passes until the pane has no call in flight.
    fn settle(pane: &mut SpeakersPane, doc: &mut AutoCommit) {
        let deadline = std::time::Instant::now() + HANG_BOUND;
        while std::time::Instant::now() < deadline {
            pane.sync(doc, &automerge::ROOT);
            if pane.in_flight == 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("speaker calls did not settle");
    }

    fn press(pane: &mut SpeakersPane, doc: &mut AutoCommit, key: &str) {
        doc.put(automerge::ROOT, key, true).unwrap();
        settle(pane, doc);
    }

    #[test]
    fn the_buttons_walk_a_stereo_pair_from_choice_to_saved() {
        let service = fake();
        let mut pane = SpeakersPane::with_service(service.clone());
        let mut doc = AutoCommit::new();
        settle(&mut pane, &mut doc);
        assert_eq!(pane.setup().groups().len(), 1);
        press(&mut pane, &mut doc, BEGIN_KEY);
        assert_eq!(pane.setup().step(), &Step::ChooseOutputs);
        settle(&mut pane, &mut doc);
        for (_, _, state) in &pane.chosen_states {
            state.set(true);
        }
        press(&mut pane, &mut doc, NEXT_KEY);
        assert!(matches!(
            pane.setup().step(),
            Step::Identify { index: 0, .. }
        ));
        press(&mut pane, &mut doc, PLAY_KEY);
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
        press(&mut pane, &mut doc, &role_key(SpeakerRoleSpec::Left));
        assert_eq!(pane.setup().step(), &Step::Name);
        pane.name_state.set("Living room".into());
        press(&mut pane, &mut doc, SAVE_KEY);
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

    #[test]
    fn a_typed_action_runs_through_the_same_service_and_reports_its_outcome() {
        let service = fake();
        let commands = ui::LocalState::new(None);
        let status = ui::LocalState::new(LocalOperationStatus::Idle);
        let mut pane = SpeakersPane::with_service(service.clone())
            .with_command_state(&commands)
            .with_operation_status(&status);
        let mut doc = AutoCommit::new();
        settle(&mut pane, &mut doc);
        let operation = SpeakerOperation::Delete {
            group: "cd".repeat(16),
        };
        commands.set(Some(operation.clone()));
        settle(&mut pane, &mut doc);
        assert!(
            service
                .commands
                .lock()
                .unwrap()
                .contains(&SpeakerCommand::Delete {
                    group: "cd".repeat(16)
                })
        );
        assert_eq!(
            status.with(Clone::clone),
            LocalOperationStatus::Failed {
                token: crate::actions::local_operation_token_for(
                    SPEAKER_OPERATION_TYPE,
                    &operation
                ),
                message: "that speaker set is gone".into(),
            }
        );
    }

    #[test]
    fn deleting_from_the_overview_asks_for_confirmation_first() {
        let service = fake();
        let mut pane = SpeakersPane::with_service(service.clone());
        let mut doc = AutoCommit::new();
        settle(&mut pane, &mut doc);
        let delete = group_key("delete", &"cd".repeat(16));
        press(&mut pane, &mut doc, &delete);
        assert!(
            !service
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|command| matches!(command, SpeakerCommand::Delete { .. }))
        );
        press(&mut pane, &mut doc, &delete);
        assert!(
            service
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|command| matches!(command, SpeakerCommand::Delete { .. }))
        );
    }

    #[test]
    fn calibration_is_a_proposal_until_applied() {
        use ui::collaboration::{CalibrationInfo, CalibrationMemberInfo};
        let service = fake();
        {
            let mut snapshot = service.snapshot.lock().unwrap();
            snapshot.devices[0].microphones.push(SpeakerEndpointInfo {
                selector: "ee".repeat(16),
                label: "Microphone".into(),
                is_default: true,
            });
        }
        let mut pane = SpeakersPane::with_service(service.clone());
        let mut doc = AutoCommit::new();
        settle(&mut pane, &mut doc);
        let group = "cd".repeat(16);
        press(&mut pane, &mut doc, &group_key("calibrate", &group));
        assert_eq!(
            pane.setup().step(),
            &Step::Calibrate {
                group: group.clone()
            }
        );
        press(&mut pane, &mut doc, CALIBRATE_KEY);
        assert!(
            service
                .commands
                .lock()
                .unwrap()
                .contains(&SpeakerCommand::StartCalibration {
                    group: group.clone(),
                    microphone_device: "desk".into(),
                    microphone: "ee".repeat(16),
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
                    },
                    before: SpeakerTrimInfo::default(),
                    after: Some(SpeakerTrimInfo {
                        delay_ms: 2.5,
                        gain_db: -1.5,
                        eq_bands: 2,
                    }),
                }],
                message: None,
            });
        pane.refresh();
        settle(&mut pane, &mut doc);
        assert!(
            !service
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|command| matches!(command, SpeakerCommand::ApplyCalibration { .. })),
            "a proposal is never applied implicitly"
        );
        press(&mut pane, &mut doc, APPLY_KEY);
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
