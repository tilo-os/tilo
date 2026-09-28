//! The "Set up speakers" flow, as a state machine independent of widgets.
//!
//! Choose outputs on any of your devices, tell which one is which by
//! listening to each in turn, optionally place them around the room, name
//! the set and save it. Saving asks each other device to join; once every
//! speaker is ready the set can be calibrated: a microphone where the person
//! sits hears each speaker's sweep, the set's own devices' microphones and
//! any others switched on listen too, and a calibration is only a proposal
//! until it is applied.
//!
//! The shell owns speaker groups; this module only builds the commands the
//! Speakers pane sends and interprets the snapshots it gets back.

use std::collections::BTreeMap;

use ui::collaboration::{
    CalibrationInfo, CalibrationPhaseInfo, MicrophoneStatus, SpeakerCommand, SpeakerDeviceInfo,
    SpeakerGroupInfo, SpeakerMemberSpec, SpeakerMemberStatus, SpeakerPlaybackInfo, SpeakerRoleSpec,
    SpeakerSnapshot,
};

use crate::i18n::{t, t_args};

/// Default distance of a new room point from the listening position.
pub const ROOM_RADIUS_MM: i32 = 2_000;
/// Room points are placed within this many millimetres of the listener.
pub const ROOM_EXTENT_MM: i32 = 5_000;

/// How the chosen speakers are arranged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrangement {
    /// Two speakers, left and right.
    Stereo,
    /// Three to six speakers in named channels (5.1 and its subsets).
    Surround,
    /// Any number of speakers placed freely around the room.
    Room,
}

impl Arrangement {
    pub const ALL: [Self; 3] = [Self::Stereo, Self::Surround, Self::Room];

    /// The arrangement's name, as the chooser lists it.
    #[must_use]
    pub fn label(self) -> String {
        t(match self {
            Self::Stereo => "speakers-arrangement-stereo",
            Self::Surround => "speakers-arrangement-surround",
            Self::Room => "speakers-arrangement-room",
        })
    }

    /// What the arrangement means, beside the chooser.
    #[must_use]
    pub fn description(self) -> String {
        t(match self {
            Self::Stereo => "speakers-arrangement-stereo-description",
            Self::Surround => "speakers-arrangement-surround-description",
            Self::Room => "speakers-arrangement-room-description",
        })
    }

    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|arrangement| arrangement.label() == label)
    }

    const fn member_range(self) -> (usize, usize) {
        match self {
            Self::Stereo => (2, 2),
            Self::Surround => (3, 6),
            Self::Room => (1, 8),
        }
    }
}

/// A speaker's role as a phrase inside a sentence: "left speaker".
#[must_use]
pub fn role_text(role: SpeakerRoleSpec) -> String {
    t(match role {
        SpeakerRoleSpec::Left => "speakers-role-left",
        SpeakerRoleSpec::Right => "speakers-role-right",
        SpeakerRoleSpec::Center => "speakers-role-center",
        SpeakerRoleSpec::Lfe => "speakers-role-lfe",
        SpeakerRoleSpec::SurroundLeft => "speakers-role-surround-left",
        SpeakerRoleSpec::SurroundRight => "speakers-role-surround-right",
        SpeakerRoleSpec::Room { .. } => "speakers-role-room",
    })
}

/// A role as a button answers "Which one just played?": "Left".
#[must_use]
pub fn role_answer(role: SpeakerRoleSpec) -> String {
    t(match role {
        SpeakerRoleSpec::Left => "speakers-answer-left",
        SpeakerRoleSpec::Right => "speakers-answer-right",
        SpeakerRoleSpec::Center => "speakers-answer-center",
        SpeakerRoleSpec::Lfe => "speakers-answer-lfe",
        SpeakerRoleSpec::SurroundLeft => "speakers-answer-surround-left",
        SpeakerRoleSpec::SurroundRight => "speakers-answer-surround-right",
        SpeakerRoleSpec::Room { .. } => "speakers-answer-room",
    })
}

/// Where one member of a set stands.
#[must_use]
pub fn member_status_text(status: SpeakerMemberStatus) -> String {
    t(match status {
        SpeakerMemberStatus::Ready => "speakers-member-ready",
        SpeakerMemberStatus::AwaitingConsent => "speakers-member-awaiting-consent",
        SpeakerMemberStatus::Offline => "speakers-member-offline",
        SpeakerMemberStatus::OutputMissing => "speakers-member-output-missing",
        SpeakerMemberStatus::Unavailable => "speakers-member-unavailable",
        SpeakerMemberStatus::NotCertified => "speakers-member-not-certified",
    })
}

/// A session playing on a set, as its status pill reads.
#[must_use]
pub fn playback_text(playback: &SpeakerPlaybackInfo) -> String {
    t(match playback {
        SpeakerPlaybackInfo::Playing => "speakers-playback-playing",
        SpeakerPlaybackInfo::Connecting => "speakers-playback-connecting",
        SpeakerPlaybackInfo::SyncingClocks => "speakers-playback-syncing",
        SpeakerPlaybackInfo::ClockFailed { .. } => "speakers-playback-clock-failed",
    })
}

/// One output that can join a speaker set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub device: String,
    pub device_label: String,
    pub output: String,
    pub output_label: String,
    pub local: bool,
}

impl Candidate {
    /// "Shelf · Speakers".
    #[must_use]
    pub fn label(&self) -> String {
        format!("{} · {}", self.device_label, self.output_label)
    }
}

/// One device's microphone that can listen while a set is calibrated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Microphone {
    pub device: String,
    pub device_label: String,
    /// "Phone · Microphone".
    pub label: String,
    pub local: bool,
    pub status: MicrophoneStatus,
    /// The device also plays in the set, so its microphone hears that
    /// speaker up close and measures its own output latency.
    pub member: bool,
}

impl Microphone {
    /// What this microphone would add, or why it cannot, as its row
    /// describes it.
    #[must_use]
    pub fn status_text(&self) -> String {
        let role = t(if self.member {
            "speakers-microphone-role-member"
        } else {
            "speakers-microphone-role-cross-check"
        });
        let status = t(match self.status {
            MicrophoneStatus::Ready if self.local => "speakers-microphone-this-device",
            MicrophoneStatus::Ready => "speakers-microphone-ready",
            MicrophoneStatus::NotAsked => "speakers-microphone-not-asked",
            MicrophoneStatus::AwaitingConsent => "speakers-microphone-awaiting",
            MicrophoneStatus::Declined => "speakers-microphone-declined",
            MicrophoneStatus::Offline => "speakers-microphone-offline",
            MicrophoneStatus::Unavailable => "speakers-microphone-unavailable",
        });
        if self.status.usable() {
            format!("{role} · {status}")
        } else {
            status
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Existing speaker sets and "Set up speakers".
    Overview,
    ChooseOutputs,
    /// "Which one just played?" for the speaker at `index`.
    Identify {
        index: usize,
        played: bool,
    },
    /// Drag each speaker to where it stands (room arrangement only).
    Layout,
    Name,
    /// Measure a saved set with a microphone.
    Calibrate {
        group: String,
    },
}

/// The whole flow.
#[derive(Clone, Debug, PartialEq)]
pub struct SpeakerSetup {
    step: Step,
    arrangement: Arrangement,
    chosen: Vec<Candidate>,
    roles: Vec<Option<SpeakerRoleSpec>>,
    name: String,
    snapshot: SpeakerSnapshot,
    message: Option<String>,
    /// The calibrated group has appeared in a snapshot. A just-saved group
    /// reaches the fabric a moment after the save returns, so only a group
    /// that was seen and then vanished closes the calibration step.
    calibrate_seen: bool,
    /// Extra microphones the person switched on or off, overriding the
    /// default (on for the set's own devices that already allowed it).
    microphone_choices: BTreeMap<String, bool>,
    /// The device at the listening position the person picked, if any.
    listening: Option<String>,
}

impl Default for SpeakerSetup {
    fn default() -> Self {
        Self {
            step: Step::Overview,
            arrangement: Arrangement::Stereo,
            chosen: Vec::new(),
            roles: Vec::new(),
            name: t("speakers-default-name"),
            snapshot: SpeakerSnapshot::default(),
            message: None,
            calibrate_seen: false,
            microphone_choices: BTreeMap::new(),
            listening: None,
        }
    }
}

fn candidates_of(device: &SpeakerDeviceInfo) -> impl Iterator<Item = Candidate> + '_ {
    device
        .outputs
        .iter()
        .filter(|_| device.online && device.can_join)
        .map(|output| Candidate {
            device: device.id.clone(),
            device_label: device.label.clone(),
            output: output.selector.clone(),
            output_label: output.label.clone(),
            local: device.local,
        })
}

/// A default spot on an arc in front of the listener, spreading `count`
/// speakers from left to right.
#[must_use]
pub fn default_room_point(index: usize, count: usize) -> SpeakerRoleSpec {
    let as_f64 = |value: usize| f64::from(u32::try_from(value).unwrap_or(u32::MAX));
    let fraction = if count <= 1 {
        0.5
    } else {
        as_f64(index) / as_f64(count.saturating_sub(1))
    };
    let angle = (fraction - 0.5) * std::f64::consts::FRAC_PI_2 * 1.5;
    let radius = f64::from(ROOM_RADIUS_MM);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the point lies within the room radius"
    )]
    let (x_mm, y_mm) = (
        (radius * angle.sin()).round() as i32,
        (radius * angle.cos()).round() as i32,
    );
    SpeakerRoleSpec::Room {
        x_mm,
        y_mm,
        z_mm: 0,
    }
}

impl SpeakerSetup {
    #[must_use]
    pub fn step(&self) -> &Step {
        &self.step
    }

    #[must_use]
    pub fn arrangement(&self) -> Arrangement {
        self.arrangement
    }

    #[must_use]
    pub fn chosen(&self) -> &[Candidate] {
        &self.chosen
    }

    #[must_use]
    pub fn roles(&self) -> &[Option<SpeakerRoleSpec>] {
        &self.roles
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn snapshot(&self) -> &SpeakerSnapshot {
        &self.snapshot
    }

    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    pub fn set_message(&mut self, message: Option<String>) {
        self.message = message;
    }

    /// Adopt a fresh snapshot from the shell. Choices whose output
    /// disappeared are dropped.
    pub fn update(&mut self, snapshot: SpeakerSnapshot) {
        self.snapshot = snapshot;
        let available = self.candidates();
        let keep = self
            .chosen
            .iter()
            .map(|chosen| {
                available.iter().any(|candidate| {
                    candidate.device == chosen.device && candidate.output == chosen.output
                })
            })
            .collect::<Vec<_>>();
        let mut flags = keep.iter();
        self.chosen
            .retain(|_| flags.next().copied().unwrap_or(false));
        let mut flags = keep.iter();
        self.roles.retain(|_| flags.next().copied().unwrap_or(true));
        if let Step::Calibrate { group } = &self.step {
            if self.snapshot.groups.iter().any(|known| &known.id == group) {
                self.calibrate_seen = true;
            } else if self.calibrate_seen {
                self.step = Step::Overview;
            }
        }
    }

    /// Every output on an online device that can join.
    #[must_use]
    pub fn candidates(&self) -> Vec<Candidate> {
        self.snapshot
            .devices
            .iter()
            .flat_map(candidates_of)
            .collect()
    }

    /// Every device's microphone the shell offers for calibrating `group`.
    #[must_use]
    pub fn microphones(&self, group: &str) -> Vec<Microphone> {
        let Some(known) = self.group(group) else {
            return Vec::new();
        };
        known
            .microphones
            .iter()
            .map(|microphone| Microphone {
                device: microphone.device.clone(),
                device_label: microphone.device_label.clone(),
                label: format!("{} · {}", microphone.device_label, microphone.label),
                local: microphone.local,
                status: microphone.status,
                member: known
                    .members
                    .iter()
                    .any(|member| member.device == microphone.device),
            })
            .collect()
    }

    /// Microphones that can hear at the listening position: every usable one.
    #[must_use]
    pub fn listening_choices(&self, group: &str) -> Vec<Microphone> {
        self.microphones(group)
            .into_iter()
            .filter(|microphone| microphone.status.usable())
            .collect()
    }

    /// Pick the device whose microphone sits where the person listens.
    pub fn set_listening(&mut self, device: &str) {
        self.listening = Some(device.to_string());
    }

    /// The microphone at the listening position: the picked one while it is
    /// usable, otherwise this device's, otherwise the first that is ready.
    #[must_use]
    pub fn listening(&self, group: &str) -> Option<Microphone> {
        let choices = self.listening_choices(group);
        self.listening
            .as_ref()
            .and_then(|device| {
                choices
                    .iter()
                    .find(|microphone| &microphone.device == device)
            })
            .or_else(|| choices.iter().find(|microphone| microphone.local))
            .or_else(|| {
                choices
                    .iter()
                    .find(|microphone| microphone.status == MicrophoneStatus::Ready)
            })
            .cloned()
    }

    /// Whether `microphone` also listens, besides the one where the person
    /// sits. Unless switched, the set's own devices that already allowed it
    /// do: they measure their own speaker's output latency.
    #[must_use]
    pub fn also_listens(&self, microphone: &Microphone) -> bool {
        microphone.status.usable()
            && self
                .microphone_choices
                .get(&microphone.device)
                .copied()
                .unwrap_or(microphone.member && microphone.status == MicrophoneStatus::Ready)
    }

    /// Switch `device`'s microphone on or off as an extra listener.
    pub fn set_also_listens(&mut self, device: &str, on: bool) {
        self.microphone_choices.insert(device.to_string(), on);
    }

    /// The microphones besides the listening one, in the shell's order.
    #[must_use]
    pub fn extra_microphones(&self, group: &str) -> Vec<Microphone> {
        let listening = self.listening(group).map(|microphone| microphone.device);
        self.microphones(group)
            .into_iter()
            .filter(|microphone| Some(&microphone.device) != listening.as_ref())
            .collect()
    }

    #[must_use]
    pub fn groups(&self) -> &[SpeakerGroupInfo] {
        &self.snapshot.groups
    }

    #[must_use]
    pub fn group(&self, id: &str) -> Option<&SpeakerGroupInfo> {
        self.snapshot.groups.iter().find(|group| group.id == id)
    }

    #[must_use]
    pub fn calibration(&self, group: &str) -> Option<&CalibrationInfo> {
        self.snapshot
            .calibrations
            .iter()
            .find(|calibration| calibration.group == group)
    }

    /// Start a new set.
    pub fn begin(&mut self) {
        *self = Self {
            snapshot: std::mem::take(&mut self.snapshot),
            step: Step::ChooseOutputs,
            ..Self::default()
        };
    }

    pub fn set_arrangement(&mut self, arrangement: Arrangement) {
        self.arrangement = arrangement;
        self.roles = vec![None; self.chosen.len()];
    }

    #[must_use]
    pub fn is_chosen(&self, candidate: &Candidate) -> bool {
        self.chosen
            .iter()
            .any(|chosen| chosen.device == candidate.device && chosen.output == candidate.output)
    }

    /// Add or remove one output.
    pub fn toggle(&mut self, candidate: &Candidate) {
        if let Some(index) = self.chosen.iter().position(|chosen| {
            chosen.device == candidate.device && chosen.output == candidate.output
        }) {
            self.chosen.remove(index);
        } else {
            self.chosen.push(candidate.clone());
        }
        self.roles = vec![None; self.chosen.len()];
    }

    /// Why the chosen outputs cannot continue yet, if they cannot.
    #[must_use]
    pub fn outputs_problem(&self) -> Option<String> {
        let (low, high) = self.arrangement.member_range();
        let count = self.chosen.len();
        if count < low {
            return Some(t(match self.arrangement {
                Arrangement::Stereo => "speakers-problem-stereo-too-few",
                Arrangement::Surround => "speakers-problem-surround-too-few",
                Arrangement::Room => "speakers-problem-room-too-few",
            }));
        }
        (count > high).then(|| {
            t(match self.arrangement {
                Arrangement::Stereo => "speakers-problem-stereo-too-many",
                Arrangement::Surround => "speakers-problem-surround-too-many",
                Arrangement::Room => "speakers-problem-room-too-many",
            })
        })
    }

    /// Move from choosing outputs to telling them apart.
    ///
    /// # Errors
    ///
    /// The reason the chosen outputs cannot form this arrangement.
    pub fn confirm_outputs(&mut self) -> Result<(), String> {
        if let Some(problem) = self.outputs_problem() {
            return Err(problem);
        }
        self.roles = vec![None; self.chosen.len()];
        if self.arrangement == Arrangement::Room {
            let count = self.chosen.len();
            self.roles = (0..count)
                .map(|index| Some(default_room_point(index, count)))
                .collect();
        }
        self.step = Step::Identify {
            index: 0,
            played: false,
        };
        Ok(())
    }

    /// The sound to play so the person can tell which speaker is which.
    #[must_use]
    pub fn identify_command(&mut self) -> Option<SpeakerCommand> {
        let Step::Identify { index, played } = &mut self.step else {
            return None;
        };
        let candidate = self.chosen.get(*index)?;
        *played = true;
        Some(SpeakerCommand::Identify {
            device: candidate.device.clone(),
            output: candidate.output.clone(),
        })
    }

    /// Roles the current speaker may still take.
    #[must_use]
    pub fn remaining_roles(&self) -> Vec<SpeakerRoleSpec> {
        let all: &[SpeakerRoleSpec] = match self.arrangement {
            Arrangement::Stereo => &[SpeakerRoleSpec::Left, SpeakerRoleSpec::Right],
            Arrangement::Surround => &[
                SpeakerRoleSpec::Left,
                SpeakerRoleSpec::Right,
                SpeakerRoleSpec::Center,
                SpeakerRoleSpec::Lfe,
                SpeakerRoleSpec::SurroundLeft,
                SpeakerRoleSpec::SurroundRight,
            ],
            Arrangement::Room => &[],
        };
        let current = match self.step {
            Step::Identify { index, .. } => Some(index),
            _ => None,
        };
        all.iter()
            .copied()
            .filter(|role| {
                !self
                    .roles
                    .iter()
                    .enumerate()
                    .any(|(index, taken)| Some(index) != current && *taken == Some(*role))
            })
            .collect()
    }

    /// "It was the left one." Assigning the first speaker of a stereo pair
    /// settles the other one too.
    ///
    /// # Errors
    ///
    /// When the role is taken or does not fit the arrangement.
    pub fn assign(&mut self, role: SpeakerRoleSpec) -> Result<(), String> {
        let Step::Identify { index, .. } = self.step else {
            return Err(t("speakers-assign-listen-first"));
        };
        if self.arrangement == Arrangement::Room {
            if !matches!(role, SpeakerRoleSpec::Room { .. }) {
                return Err(t("speakers-assign-room-on-layout"));
            }
        } else if !self.remaining_roles().contains(&role) {
            return Err(t_args(
                "speakers-assign-taken",
                &[("role", role_text(role).into())],
            ));
        }
        if let Some(slot) = self.roles.get_mut(index) {
            *slot = Some(role);
        }
        if self.arrangement == Arrangement::Stereo {
            let other = if role == SpeakerRoleSpec::Left {
                SpeakerRoleSpec::Right
            } else {
                SpeakerRoleSpec::Left
            };
            for (slot_index, slot) in self.roles.iter_mut().enumerate() {
                if slot_index != index {
                    *slot = Some(other);
                }
            }
        }
        self.advance_identify(index);
        Ok(())
    }

    /// Move past the current speaker without changing its role (room
    /// speakers keep their default spot until placed on the layout).
    pub fn skip_identify(&mut self) {
        if let Step::Identify { index, .. } = self.step
            && self.roles.get(index).is_some_and(Option::is_some)
        {
            self.advance_identify(index);
        }
    }

    fn advance_identify(&mut self, index: usize) {
        let next = self
            .roles
            .iter()
            .enumerate()
            .skip(index.saturating_add(1))
            .find(|(_, role)| role.is_none() || self.arrangement == Arrangement::Room)
            .map(|(next, _)| next);
        self.step = match next {
            Some(next) => Step::Identify {
                index: next,
                played: false,
            },
            None if self.arrangement == Arrangement::Room => Step::Layout,
            None => Step::Name,
        };
    }

    /// Place a room speaker, clamped to the room.
    pub fn place(&mut self, index: usize, x_mm: i32, y_mm: i32) {
        if self.arrangement != Arrangement::Room {
            return;
        }
        if let Some(slot) = self.roles.get_mut(index) {
            *slot = Some(SpeakerRoleSpec::Room {
                x_mm: x_mm.clamp(-ROOM_EXTENT_MM, ROOM_EXTENT_MM),
                y_mm: y_mm.clamp(-ROOM_EXTENT_MM, ROOM_EXTENT_MM),
                z_mm: 0,
            });
        }
    }

    /// Room points in member order, for the layout canvas.
    #[must_use]
    pub fn room_points(&self) -> Vec<(i32, i32)> {
        self.roles
            .iter()
            .map(|role| match role {
                Some(SpeakerRoleSpec::Room { x_mm, y_mm, .. }) => (*x_mm, *y_mm),
                _ => (0, 0),
            })
            .collect()
    }

    /// Why the layout cannot be saved yet, if it cannot.
    #[must_use]
    pub fn layout_problem(&self) -> Option<String> {
        let points = self.room_points();
        let crowded = points.iter().enumerate().any(|(index, point)| {
            points
                .iter()
                .skip(index.saturating_add(1))
                .any(|other| other == point)
        });
        crowded.then(|| t("speakers-layout-crowded"))
    }

    pub fn finish_layout(&mut self) {
        if self.step == Step::Layout && self.layout_problem().is_none() {
            self.step = Step::Name;
        }
    }

    pub fn set_name(&mut self, name: &str) {
        self.name = name.to_string();
    }

    /// The create command for the finished draft.
    ///
    /// # Errors
    ///
    /// When a speaker has no role yet or the name is empty.
    pub fn save_command(&self) -> Result<SpeakerCommand, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(t("speakers-name-empty"));
        }
        if name.chars().count() > ui::collaboration::MAX_SPEAKER_NAME_CHARS {
            return Err(t("speakers-name-too-long"));
        }
        let members = self
            .chosen
            .iter()
            .zip(&self.roles)
            .map(|(candidate, role)| {
                role.map(|role| SpeakerMemberSpec {
                    device: candidate.device.clone(),
                    output: candidate.output.clone(),
                    role,
                })
                .ok_or_else(|| {
                    t_args(
                        "speakers-name-unidentified",
                        &[("speaker", candidate.label().into())],
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SpeakerCommand::Create {
            name: name.to_string(),
            members,
        })
    }

    /// The shell saved the set as `group`: go on to calibration.
    pub fn saved(&mut self, group: String) {
        self.calibrate_seen = self.group(&group).is_some();
        self.step = Step::Calibrate { group };
        self.message = Some(t("speakers-saved-message"));
    }

    pub fn open_calibration(&mut self, group: &str) {
        if self.group(group).is_some_and(|known| known.led_here) {
            self.step = Step::Calibrate {
                group: group.to_string(),
            };
            self.calibrate_seen = true;
            self.message = None;
        }
    }

    /// Why `group` cannot be calibrated yet, if it cannot.
    #[must_use]
    pub fn calibration_problem(&self, group: &str) -> Option<String> {
        let Some(known) = self.group(group) else {
            return Some(t("speakers-calibrate-gone"));
        };
        if !known.led_here {
            return Some(t_args(
                "speakers-calibrate-elsewhere",
                &[
                    ("name", known.name.as_str().into()),
                    ("leader", known.leader_label.as_str().into()),
                ],
            ));
        }
        if let Some(member) = known
            .members
            .iter()
            .find(|member| member.status != SpeakerMemberStatus::Ready)
        {
            return Some(t_args(
                "speakers-calibrate-member-not-ready",
                &[
                    ("device", member.device_label.as_str().into()),
                    ("status", member_status_text(member.status).into()),
                ],
            ));
        }
        self.listening(group)
            .is_none()
            .then(|| t("speakers-calibrate-no-microphone"))
    }

    /// Start measuring `group` from where the person sits, with every extra
    /// microphone that is switched on.
    ///
    /// # Errors
    ///
    /// The reason calibration cannot start.
    pub fn calibrate_command(&self, group: &str) -> Result<SpeakerCommand, String> {
        if let Some(problem) = self.calibration_problem(group) {
            return Err(problem);
        }
        if self.calibration(group).is_some_and(|run| {
            matches!(
                run.phase,
                CalibrationPhaseInfo::Measuring | CalibrationPhaseInfo::WaitingForMicrophones
            )
        }) {
            return Err(t("speakers-calibrate-already-running"));
        }
        let listening = self.listening(group).map(|microphone| microphone.device);
        let excluded = self
            .extra_microphones(group)
            .into_iter()
            .filter(|microphone| !self.also_listens(microphone))
            .map(|microphone| microphone.device)
            .collect();
        Ok(SpeakerCommand::StartCalibration {
            group: group.to_string(),
            excluded,
            listening,
        })
    }

    /// Step back.
    pub fn back(&mut self) {
        self.message = None;
        self.step = match &self.step {
            Step::Overview | Step::ChooseOutputs | Step::Calibrate { .. } => Step::Overview,
            Step::Identify { index: 0, .. } => Step::ChooseOutputs,
            Step::Identify { index, .. } => Step::Identify {
                index: index.saturating_sub(1),
                played: false,
            },
            Step::Layout => Step::Identify {
                index: self.chosen.len().saturating_sub(1),
                played: false,
            },
            Step::Name if self.arrangement == Arrangement::Room => Step::Layout,
            Step::Name => Step::Identify {
                index: 0,
                played: false,
            },
        };
    }

    /// Return to the overview.
    pub fn close(&mut self) {
        self.step = Step::Overview;
    }
}

/// Where the Speakers pane sends its commands. The live implementation is
/// the shell's broker lane; tests substitute a fake.
pub trait SpeakerService: Send + Sync {
    /// Send one command and wait for the shell's answer.
    ///
    /// # Errors
    ///
    /// A message when the shell cannot be reached.
    fn call(&self, command: SpeakerCommand) -> Result<ui::collaboration::SpeakerReply, String>;
}

/// The shell's speaker authority over the authenticated Settings route.
pub struct BrokerSpeakers;

impl SpeakerService for BrokerSpeakers {
    fn call(&self, command: SpeakerCommand) -> Result<ui::collaboration::SpeakerReply, String> {
        ui::collaboration::speaker_command(command)
    }
}

fn snapshot_cache() -> &'static std::sync::Mutex<SpeakerSnapshot> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<SpeakerSnapshot>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(SpeakerSnapshot::default()))
}

/// The last snapshot the Speakers pane received, for the action tree.
#[must_use]
pub fn cached_snapshot() -> SpeakerSnapshot {
    snapshot_cache()
        .lock()
        .map(|snapshot| snapshot.clone())
        .unwrap_or_default()
}

pub(crate) fn remember_snapshot(snapshot: &SpeakerSnapshot) {
    if let Ok(mut cached) = snapshot_cache().lock() {
        cached.clone_from(snapshot);
    }
}

#[cfg(test)]
mod tests {
    use ui::collaboration::{
        CalibrationMemberInfo, CalibrationMemberState, SpeakerEndpointInfo, SpeakerMemberInfo,
        SpeakerTrimInfo,
    };

    use super::*;

    fn device(id: &str, outputs: &[&str], microphone: bool) -> SpeakerDeviceInfo {
        SpeakerDeviceInfo {
            id: id.into(),
            label: id.into(),
            online: true,
            local: id == "desk",
            can_join: true,
            outputs: outputs
                .iter()
                .enumerate()
                .map(|(index, label)| SpeakerEndpointInfo {
                    selector: format!("{index:02x}").repeat(16),
                    label: (*label).into(),
                    is_default: index == 0,
                })
                .collect(),
            microphones: if microphone {
                vec![SpeakerEndpointInfo {
                    selector: "ee".repeat(16),
                    label: "Microphone".into(),
                    is_default: true,
                }]
            } else {
                Vec::new()
            },
        }
    }

    fn snapshot() -> SpeakerSnapshot {
        SpeakerSnapshot {
            media_enabled: true,
            devices: vec![
                device("desk", &["Speakers"], true),
                device("shelf", &["Line out"], false),
            ],
            groups: Vec::new(),
            calibrations: Vec::new(),
        }
    }

    fn setup() -> SpeakerSetup {
        let mut setup = SpeakerSetup::default();
        setup.update(snapshot());
        setup
    }

    #[test]
    fn a_stereo_pair_is_set_up_by_listening_to_one_speaker() {
        let mut setup = setup();
        setup.begin();
        assert_eq!(setup.step(), &Step::ChooseOutputs);
        let candidates = setup.candidates();
        assert_eq!(candidates.len(), 2);
        setup.toggle(&candidates[0]);
        assert_eq!(
            setup.confirm_outputs(),
            Err("Choose two speakers for a stereo pair".into())
        );
        setup.toggle(&candidates[1]);
        setup.confirm_outputs().unwrap();
        assert_eq!(
            setup.identify_command(),
            Some(SpeakerCommand::Identify {
                device: "desk".into(),
                output: "00".repeat(16),
            })
        );
        assert_eq!(
            setup.step(),
            &Step::Identify {
                index: 0,
                played: true
            }
        );
        setup.assign(SpeakerRoleSpec::Right).unwrap();
        assert_eq!(setup.step(), &Step::Name, "the other side is implied");
        setup.set_name("Living room");
        let SpeakerCommand::Create { name, members } = setup.save_command().unwrap() else {
            panic!("not a create command");
        };
        assert_eq!(name, "Living room");
        assert_eq!(
            members.iter().map(|member| member.role).collect::<Vec<_>>(),
            vec![SpeakerRoleSpec::Right, SpeakerRoleSpec::Left]
        );
        setup.saved("ab".repeat(16));
        assert_eq!(
            setup.step(),
            &Step::Calibrate {
                group: "ab".repeat(16)
            }
        );
    }

    #[test]
    fn surround_roles_are_offered_only_once() {
        let mut setup = SpeakerSetup::default();
        let mut snap = snapshot();
        snap.devices.push(device("tv", &["HDMI"], false));
        setup.update(snap);
        setup.begin();
        setup.set_arrangement(Arrangement::Surround);
        for candidate in setup.candidates() {
            setup.toggle(&candidate);
        }
        setup.confirm_outputs().unwrap();
        setup.assign(SpeakerRoleSpec::Left).unwrap();
        assert!(!setup.remaining_roles().contains(&SpeakerRoleSpec::Left));
        assert!(setup.assign(SpeakerRoleSpec::Left).is_err());
        setup.assign(SpeakerRoleSpec::Right).unwrap();
        setup.assign(SpeakerRoleSpec::Lfe).unwrap();
        assert_eq!(setup.step(), &Step::Name);
        assert!(setup.save_command().is_ok());
    }

    #[test]
    fn room_speakers_start_on_an_arc_and_can_be_placed() {
        let mut setup = setup();
        setup.begin();
        setup.set_arrangement(Arrangement::Room);
        for candidate in setup.candidates() {
            setup.toggle(&candidate);
        }
        setup.confirm_outputs().unwrap();
        let points = setup.room_points();
        assert!(points[0].0 < 0 && points[1].0 > 0, "{points:?}");
        setup.skip_identify();
        setup.skip_identify();
        assert_eq!(setup.step(), &Step::Layout);
        setup.place(1, points[0].0, points[0].1);
        assert!(setup.layout_problem().is_some());
        setup.finish_layout();
        assert_eq!(setup.step(), &Step::Layout, "overlapping speakers block");
        setup.place(1, 99_000, 1_000);
        assert_eq!(setup.room_points()[1], (ROOM_EXTENT_MM, 1_000));
        setup.finish_layout();
        assert_eq!(setup.step(), &Step::Name);
    }

    #[test]
    fn back_walks_the_flow_in_reverse() {
        let mut setup = setup();
        setup.begin();
        for candidate in setup.candidates() {
            setup.toggle(&candidate);
        }
        setup.confirm_outputs().unwrap();
        setup.back();
        assert_eq!(setup.step(), &Step::ChooseOutputs);
        setup.back();
        assert_eq!(setup.step(), &Step::Overview);
    }

    #[test]
    fn an_unplugged_output_leaves_the_draft() {
        let mut setup = setup();
        setup.begin();
        for candidate in setup.candidates() {
            setup.toggle(&candidate);
        }
        let mut snap = snapshot();
        snap.devices[1].online = false;
        setup.update(snap);
        assert_eq!(setup.chosen().len(), 1);
        assert_eq!(setup.roles().len(), 1);
    }

    fn saved_group(status: SpeakerMemberStatus, led_here: bool) -> SpeakerGroupInfo {
        SpeakerGroupInfo {
            id: "ab".repeat(16),
            name: "Living room".into(),
            leader_label: "desk".into(),
            led_here,
            members: vec![SpeakerMemberInfo {
                device: "shelf".into(),
                device_label: "Shelf".into(),
                output: "00".repeat(16),
                output_label: Some("Line out".into()),
                role: SpeakerRoleSpec::Right,
                status,
                trim: SpeakerTrimInfo::default(),
            }],
            playback: None,
            microphones: vec![
                microphone("desk", true, MicrophoneStatus::Ready),
                microphone("shelf", false, MicrophoneStatus::Ready),
                microphone("phone", false, MicrophoneStatus::NotAsked),
                microphone("attic", false, MicrophoneStatus::Declined),
            ],
        }
    }

    fn microphone(
        device: &str,
        local: bool,
        status: MicrophoneStatus,
    ) -> ui::collaboration::CalibrationMicrophoneInfo {
        ui::collaboration::CalibrationMicrophoneInfo {
            device: device.into(),
            device_label: device.into(),
            label: "Microphone".into(),
            local,
            status,
        }
    }

    #[test]
    fn calibration_listens_where_you_sit_with_the_set_s_own_microphones() {
        let mut setup = SpeakerSetup::default();
        let mut snap = snapshot();
        snap.groups
            .push(saved_group(SpeakerMemberStatus::Ready, true));
        setup.update(snap);
        let group = "ab".repeat(16);
        assert_eq!(
            setup.listening(&group).map(|microphone| microphone.device),
            Some("desk".into()),
            "this device is where you sit by default"
        );
        let extras = setup.extra_microphones(&group);
        let listens = extras
            .iter()
            .map(|microphone| (microphone.device.as_str(), setup.also_listens(microphone)))
            .collect::<Vec<_>>();
        assert_eq!(
            listens,
            [("shelf", true), ("phone", false), ("attic", false)],
            "only the set's own devices listen too unless switched"
        );
        assert_eq!(
            setup.calibrate_command(&group),
            Ok(SpeakerCommand::StartCalibration {
                group: group.clone(),
                excluded: vec!["phone".into(), "attic".into()],
                listening: Some("desk".into()),
            })
        );
        setup.set_listening("phone");
        setup.set_also_listens("shelf", false);
        setup.set_also_listens("desk", true);
        assert_eq!(
            setup.calibrate_command(&group),
            Ok(SpeakerCommand::StartCalibration {
                group: group.clone(),
                excluded: vec!["shelf".into(), "attic".into()],
                listening: Some("phone".into()),
            })
        );
        setup.set_listening("attic");
        assert_eq!(
            setup.listening(&group).map(|microphone| microphone.device),
            Some("desk".into()),
            "a declined microphone cannot be where you sit"
        );
    }

    #[test]
    fn calibration_waits_for_every_speaker_and_a_microphone() {
        let mut setup = SpeakerSetup::default();
        let mut snap = snapshot();
        snap.groups
            .push(saved_group(SpeakerMemberStatus::AwaitingConsent, true));
        setup.update(snap.clone());
        let group = "ab".repeat(16);
        assert_eq!(
            setup.calibrate_command(&group),
            Err("Shelf: Waiting for approval on that device".into())
        );
        snap.groups[0] = saved_group(SpeakerMemberStatus::Ready, false);
        setup.update(snap.clone());
        assert_eq!(
            setup.calibration_problem(&group),
            Some("Calibrate Living room on desk, where it was set up".into())
        );
        snap.groups[0] = saved_group(SpeakerMemberStatus::Ready, true);
        setup.update(snap.clone());
        assert!(setup.calibrate_command(&group).is_ok());
        snap.calibrations.push(CalibrationInfo {
            group: group.clone(),
            phase: CalibrationPhaseInfo::WaitingForMicrophones,
            members: vec![CalibrationMemberInfo {
                device: "shelf".into(),
                label: "Shelf".into(),
                state: CalibrationMemberState::Waiting,
                before: SpeakerTrimInfo::default(),
                after: None,
            }],
            microphones: Vec::new(),
            listening_label: None,
            message: None,
        });
        setup.update(snap.clone());
        assert!(setup.calibrate_command(&group).is_err());
        snap.calibrations.clear();
        snap.groups[0].microphones.clear();
        setup.update(snap);
        assert_eq!(
            setup.calibrate_command(&group),
            Err("None of your devices offers a microphone to calibrate with".into())
        );
    }

    #[test]
    fn a_deleted_group_closes_its_calibration() {
        let mut setup = SpeakerSetup::default();
        let mut snap = snapshot();
        snap.groups
            .push(saved_group(SpeakerMemberStatus::Ready, true));
        setup.update(snap.clone());
        setup.open_calibration(&"ab".repeat(16));
        assert!(matches!(setup.step(), Step::Calibrate { .. }));
        snap.groups.clear();
        setup.update(snap);
        assert_eq!(setup.step(), &Step::Overview);
    }
}
