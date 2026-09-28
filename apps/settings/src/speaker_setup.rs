//! The "Set up speakers" flow, as a state machine independent of widgets.
//!
//! Choose outputs on any of your devices, tell which one is which by
//! listening to each in turn, optionally place them around the room, name
//! the set and save it. Saving asks each other device to join; once every
//! speaker is ready the set can be calibrated with a microphone, and a
//! calibration is only a proposal until it is applied.
//!
//! The shell owns speaker groups; this module only builds the commands the
//! Speakers pane sends and interprets the snapshots it gets back.

use ui::collaboration::{
    CalibrationInfo, CalibrationPhaseInfo, SpeakerCommand, SpeakerDeviceInfo, SpeakerGroupInfo,
    SpeakerMemberSpec, SpeakerMemberStatus, SpeakerRoleSpec, SpeakerSnapshot,
};

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

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stereo => "Stereo pair",
            Self::Surround => "Surround",
            Self::Room => "Around the room",
        }
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

/// A microphone that can listen during calibration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Microphone {
    pub device: String,
    pub selector: String,
    pub label: String,
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
}

impl Default for SpeakerSetup {
    fn default() -> Self {
        Self {
            step: Step::Overview,
            arrangement: Arrangement::Stereo,
            chosen: Vec::new(),
            roles: Vec::new(),
            name: "Speakers".into(),
            snapshot: SpeakerSnapshot::default(),
            message: None,
            calibrate_seen: false,
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

    /// Every microphone on an online device.
    #[must_use]
    pub fn microphones(&self) -> Vec<Microphone> {
        self.snapshot
            .devices
            .iter()
            .filter(|device| device.online)
            .flat_map(|device| {
                device.microphones.iter().map(|microphone| Microphone {
                    device: device.id.clone(),
                    selector: microphone.selector.clone(),
                    label: format!("{} · {}", device.label, microphone.label),
                })
            })
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
            return Some(match self.arrangement {
                Arrangement::Stereo => "Choose two speakers for a stereo pair".into(),
                Arrangement::Surround => "Choose at least three speakers for surround".into(),
                Arrangement::Room => "Choose at least one speaker".into(),
            });
        }
        (count > high).then(|| match self.arrangement {
            Arrangement::Stereo => "A stereo pair has exactly two speakers".into(),
            Arrangement::Surround => "Surround uses at most six speakers".into(),
            Arrangement::Room => "Use at most eight speakers".into(),
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
            return Err("listen to a speaker first".into());
        };
        if self.arrangement == Arrangement::Room {
            if !matches!(role, SpeakerRoleSpec::Room { .. }) {
                return Err("place room speakers on the layout".into());
            }
        } else if !self.remaining_roles().contains(&role) {
            return Err(format!(
                "another speaker is already the {}",
                role.describe()
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
        crowded.then(|| "Two speakers cannot stand in exactly the same spot".into())
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
            return Err("Give the speakers a name".into());
        }
        if name.chars().count() > ui::collaboration::MAX_SPEAKER_NAME_CHARS {
            return Err("Use a shorter name".into());
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
                .ok_or_else(|| format!("Tell which speaker {} is", candidate.label()))
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
        self.message = Some(
            "Saved. Your other devices ask once whether to join; approve there, then calibrate here."
                .into(),
        );
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
            return Some("That speaker set is gone".into());
        };
        if !known.led_here {
            return Some(format!(
                "Calibrate {} on {}, where it was set up",
                known.name, known.leader_label
            ));
        }
        if let Some(member) = known
            .members
            .iter()
            .find(|member| member.status != SpeakerMemberStatus::Ready)
        {
            return Some(format!(
                "{}: {}",
                member.device_label,
                member.status.label()
            ));
        }
        self.microphones()
            .is_empty()
            .then(|| "This device has no microphone to calibrate with".into())
    }

    /// Start measuring `group` with `microphone`.
    ///
    /// # Errors
    ///
    /// The reason calibration cannot start.
    pub fn calibrate_command(
        &self,
        group: &str,
        microphone: &Microphone,
    ) -> Result<SpeakerCommand, String> {
        if let Some(problem) = self.calibration_problem(group) {
            return Err(problem);
        }
        if self
            .calibration(group)
            .is_some_and(|run| run.phase == CalibrationPhaseInfo::Measuring)
        {
            return Err("Calibration is already measuring".into());
        }
        Ok(SpeakerCommand::StartCalibration {
            group: group.to_string(),
            microphone_device: microphone.device.clone(),
            microphone: microphone.selector.clone(),
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
        }
    }

    #[test]
    fn calibration_waits_for_every_speaker_and_a_microphone() {
        let mut setup = SpeakerSetup::default();
        let mut snap = snapshot();
        snap.groups
            .push(saved_group(SpeakerMemberStatus::AwaitingConsent, true));
        setup.update(snap.clone());
        let group = "ab".repeat(16);
        let microphone = setup.microphones().remove(0);
        assert_eq!(
            setup.calibrate_command(&group, &microphone),
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
        assert_eq!(
            setup.calibrate_command(&group, &microphone),
            Ok(SpeakerCommand::StartCalibration {
                group: group.clone(),
                microphone_device: "desk".into(),
                microphone: "ee".repeat(16),
            })
        );
        snap.calibrations.push(CalibrationInfo {
            group: group.clone(),
            phase: CalibrationPhaseInfo::Measuring,
            members: vec![CalibrationMemberInfo {
                device: "shelf".into(),
                label: "Shelf".into(),
                state: CalibrationMemberState::Measuring,
                before: SpeakerTrimInfo::default(),
                after: None,
            }],
            message: None,
        });
        setup.update(snap);
        assert!(setup.calibrate_command(&group, &microphone).is_err());
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
