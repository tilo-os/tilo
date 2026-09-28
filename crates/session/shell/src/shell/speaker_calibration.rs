//! Acoustic calibration of a speaker group, as a reversible proposal.
//!
//! One microphone on any of the person's devices listens while each member
//! plays a logarithmic sweep in turn. [`media::calibration`] turns each
//! recording into an arrival delay, a level and a room EQ; aligning them
//! gives every member the delay and cut that make the group sound like one
//! speaker at the listening position.
//!
//! Nothing is applied implicitly. A finished run is a proposal showing each
//! member's trims before and after; applying stores the new trims on the
//! group and keeps the old ones for one explicit revert, the same shape as
//! the spatial display calibration.
//!
//! Measuring needs audio played and captured at sample-accurate times on
//! different devices. [`TimedMeasurement`] does that with the timed audio
//! path: this device schedules the sweep on its own clock, converts that to
//! the member's clock through its fabric clock estimate of the member, and
//! records around it. Without an estimate within the clock's deadline the
//! member fails with the reason, never a silent skip.

use crate::shell::personal_continuity::DeviceId;
use crate::shell::speaker_group::{EndpointSelector, SpeakerGroup, SpeakerGroupId, SpeakerTrim};

/// Sweep used for every member: 20 Hz to 20 kHz over three seconds.
pub const SWEEP_START_HZ: f64 = 20.0; // a frequency, not a pixel length
pub const SWEEP_END_HZ: f64 = 20_000.0;
pub const SWEEP_SECONDS: f64 = 3.0; // a duration, not a pixel length
pub const SWEEP_RATE: u32 = 48_000;
/// How far ahead a sweep is scheduled: time for the request to cross the
/// fabric and for both streams to open.
const SWEEP_LEAD_NS: i64 = 1_200_000_000;
/// Recording starts this long before the sweep so its onset is captured.
const CAPTURE_PREROLL_NS: i64 = 200_000_000;
/// Recording continues this long after the sweep for late arrivals and the
/// room's decay.
const CAPTURE_TAIL_NS: i64 = 700_000_000;

/// The sweep every device plays and measures against.
///
/// # Errors
///
/// Only if the fixed parameters were invalid.
pub fn sweep() -> Result<media::calibration::LogSweep, String> {
    media::calibration::LogSweep::new(SWEEP_RATE, SWEEP_START_HZ, SWEEP_END_HZ, SWEEP_SECONDS)
        .map_err(|error| error.to_string())
}

/// One member to measure with one microphone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SweepRequest {
    pub group: SpeakerGroupId,
    pub member: DeviceId,
    pub output: EndpointSelector,
    pub microphone_device: DeviceId,
    pub microphone: EndpointSelector,
}

/// Plays a sweep on a member and measures it at the microphone.
pub trait MeasurementSource: Send {
    /// Start measuring one member.
    ///
    /// # Errors
    ///
    /// A reason this member cannot be measured at all.
    fn begin(&mut self, request: &SweepRequest) -> Result<(), String>;
    /// The in-flight measurement's result, once it has one.
    fn poll(&mut self) -> Option<Result<media::calibration::SpeakerMeasurement, String>>;
    /// Abandon the in-flight measurement.
    fn cancel(&mut self);
}

/// Asks a remote member to play the sweep at a time on this device's clock.
pub type SweepSender = Box<dyn Fn(&SweepRequest, i64) -> Result<(), String> + Send>;

/// Measures with this device's microphone and the timed audio path: the
/// member plays the sweep at a scheduled instant (on its own output, or
/// asked over the fabric), while the microphone records around it.
pub struct TimedMeasurement {
    local: DeviceId,
    send: SweepSender,
    result:
        Option<std::sync::mpsc::Receiver<Result<media::calibration::SpeakerMeasurement, String>>>,
    clock_sync: Option<crate::shell::device_fabric_runtime::FabricClockSync>,
    /// A remote member's sweep, waiting for this device's clock of it.
    awaiting_clock: Option<(SweepRequest, media::audio::AudioDevice)>,
}

impl TimedMeasurement {
    #[must_use]
    pub fn new(local: DeviceId, send: SweepSender) -> Self {
        Self {
            local,
            send,
            result: None,
            clock_sync: None,
            awaiting_clock: None,
        }
    }

    /// Record around `play_at` on this device's clock and measure the sweep.
    fn listen(
        &mut self,
        microphone: &media::audio::AudioDevice,
        play_at: i64,
    ) -> Result<(), String> {
        let sweep = sweep()?;
        let (sender, receiver) = std::sync::mpsc::channel();
        self.result = Some(receiver);
        let duration_ns = i64::try_from(sweep.signal().len())
            .unwrap_or(i64::MAX)
            .saturating_mul(1_000_000_000)
            .checked_div(i64::from(SWEEP_RATE))
            .unwrap_or(0)
            .saturating_add(CAPTURE_PREROLL_NS)
            .saturating_add(CAPTURE_TAIL_NS);
        let serial = microphone.serial;
        std::thread::Builder::new()
            .name("calibration-listen".into())
            .spawn(move || {
                let result = media::pw_stream::TimedAudio::native()
                    .capture_window(
                        media::pw_stream::CaptureTarget {
                            serial,
                            monitor_sink: false,
                        },
                        SWEEP_RATE,
                        1,
                        play_at.saturating_sub(CAPTURE_PREROLL_NS),
                        duration_ns,
                    )
                    .map_err(|error| error.to_string())
                    .and_then(|window| {
                        let capture = window
                            .samples
                            .iter()
                            .copied()
                            .map(to_f32)
                            .collect::<Vec<_>>();
                        sweep
                            .measure(&capture, window.first_sample_ns, play_at)
                            .map_err(|error| error.to_string())
                    });
                let _ = sender.send(result);
            })
            .map_err(|error| {
                crate::shell::i18n::t_args(
                    "speaker-calibration-reason-listen-failed",
                    &[("error", error.to_string().into())],
                )
            })?;
        Ok(())
    }

    /// Once the member's clock is known, ask it to play and start
    /// listening. `None` while the clock is still syncing.
    fn start_remote_sweep(&mut self) -> Option<Result<(), String>> {
        use crate::shell::fabric_clock::ClockSyncState;
        use media::sync_playout::ClockMapping as _;
        let sync = self.clock_sync.as_ref()?;
        let estimate = match sync.state() {
            ClockSyncState::Syncing => return None,
            ClockSyncState::Failed(reason) => return Some(Err(clock_reason(reason))),
            ClockSyncState::Ready => sync.estimate()?,
        };
        let (request, microphone) = self.awaiting_clock.take()?;
        let play_at = media::pw_stream::monotonic_now_ns().saturating_add(SWEEP_LEAD_NS);
        let member_start = estimate.playout_clock().local_to_leader_ns(play_at);
        Some((self.send)(&request, member_start).and_then(|()| self.listen(&microphone, play_at)))
    }
}

fn clock_reason(reason: crate::shell::fabric_clock::ClockSyncFailure) -> String {
    crate::shell::i18n::t_args(
        "speaker-calibration-reason-clock",
        &[("reason", crate::shell::i18n::t(reason.message_key()).into())],
    )
}

fn to_f32(sample: i16) -> f32 {
    f32::from(sample) / 32_768.0
}

impl MeasurementSource for TimedMeasurement {
    fn begin(&mut self, request: &SweepRequest) -> Result<(), String> {
        use crate::shell::speaker_group_runtime::{resolve_local_microphone, resolve_local_output};
        if request.microphone_device != self.local {
            return Err(crate::shell::i18n::t(
                "speaker-calibration-reason-remote-microphone",
            ));
        }
        let microphone = resolve_local_microphone(&self.local, request.microphone)
            .ok_or_else(|| crate::shell::i18n::t("speaker-calibration-reason-microphone-gone"))?;
        if request.member != self.local {
            let sync =
                crate::shell::device_fabric_runtime::hold_fabric_clock_sync(request.member.clone());
            if let crate::shell::fabric_clock::ClockSyncState::Failed(reason) = sync.state() {
                return Err(clock_reason(reason));
            }
            self.clock_sync = Some(sync);
            self.awaiting_clock = Some((request.clone(), microphone));
            return Ok(());
        }
        let sweep = sweep()?;
        let play_at = media::pw_stream::monotonic_now_ns().saturating_add(SWEEP_LEAD_NS);
        let sink = resolve_local_output(&self.local, request.output)
            .ok_or_else(|| crate::shell::i18n::t("speaker-calibration-reason-output-gone"))?;
        let signal = sweep.signal().to_vec();
        std::thread::Builder::new()
            .name("calibration-play".into())
            .spawn(move || {
                let _ = media::pw_stream::TimedAudio::native().play_at(
                    sink.serial,
                    SWEEP_RATE,
                    1,
                    signal,
                    play_at,
                );
            })
            .map_err(|error| {
                crate::shell::i18n::t_args(
                    "speaker-calibration-reason-play-failed",
                    &[("error", error.to_string().into())],
                )
            })?;
        self.listen(&microphone, play_at)
    }

    fn poll(&mut self) -> Option<Result<media::calibration::SpeakerMeasurement, String>> {
        if self.awaiting_clock.is_some() {
            if let Err(reason) = self.start_remote_sweep()? {
                self.cancel();
                return Some(Err(reason));
            }
            return None;
        }
        let result = self.result.as_ref()?.try_recv().ok()?;
        self.result = None;
        self.clock_sync = None;
        Some(result)
    }

    fn cancel(&mut self) {
        self.result = None;
        self.awaiting_clock = None;
        self.clock_sync = None;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MemberProgress {
    Waiting,
    Measuring,
    Measured(media::calibration::SpeakerMeasurement),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalibrationPhase {
    Measuring,
    /// Every member was measured; nothing has changed yet.
    Proposed,
    /// The proposal was applied; the previous trims can be restored once.
    Applied,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalibrationMember {
    pub device: DeviceId,
    pub output: EndpointSelector,
    pub label: String,
}

/// One calibration of one group.
pub struct CalibrationRun {
    group: SpeakerGroupId,
    microphone_device: DeviceId,
    microphone: EndpointSelector,
    members: Vec<CalibrationMember>,
    progress: Vec<MemberProgress>,
    before: Vec<SpeakerTrim>,
    after: Option<Vec<SpeakerTrim>>,
    phase: CalibrationPhase,
    message: Option<String>,
    source: Box<dyn MeasurementSource>,
}

impl CalibrationRun {
    /// Start measuring every member of `group`, in member order.
    #[must_use]
    pub fn start(
        group: &SpeakerGroup,
        labels: &[String],
        microphone_device: DeviceId,
        microphone: EndpointSelector,
        source: Box<dyn MeasurementSource>,
    ) -> Self {
        let members = group
            .members
            .iter()
            .enumerate()
            .map(|(index, member)| CalibrationMember {
                device: member.device.clone(),
                output: member.output,
                label: labels
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| member.device.as_str().to_string()),
            })
            .collect::<Vec<_>>();
        let mut run = Self {
            group: group.id,
            microphone_device,
            microphone,
            progress: vec![MemberProgress::Waiting; members.len()],
            before: group
                .members
                .iter()
                .map(|member| member.trim.clone())
                .collect(),
            members,
            after: None,
            phase: CalibrationPhase::Measuring,
            message: None,
            source,
        };
        run.advance();
        run
    }

    #[must_use]
    pub fn group(&self) -> SpeakerGroupId {
        self.group
    }

    #[must_use]
    pub fn phase(&self) -> CalibrationPhase {
        self.phase
    }

    #[must_use]
    pub fn members(&self) -> &[CalibrationMember] {
        &self.members
    }

    #[must_use]
    pub fn progress(&self) -> &[MemberProgress] {
        &self.progress
    }

    #[must_use]
    pub fn before(&self) -> &[SpeakerTrim] {
        &self.before
    }

    #[must_use]
    pub fn after(&self) -> Option<&[SpeakerTrim]> {
        self.after.as_deref()
    }

    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Collect the in-flight result and start the next member. Returns
    /// whether anything changed.
    pub fn tick(&mut self) -> bool {
        if self.phase != CalibrationPhase::Measuring {
            return false;
        }
        let Some(index) = self
            .progress
            .iter()
            .position(|progress| *progress == MemberProgress::Measuring)
        else {
            return self.advance();
        };
        let Some(result) = self.source.poll() else {
            return false;
        };
        if let Some(slot) = self.progress.get_mut(index) {
            *slot = match result {
                Ok(measurement) => MemberProgress::Measured(measurement),
                Err(reason) => MemberProgress::Failed(reason),
            };
        }
        self.advance();
        true
    }

    /// Begin the next waiting member, or finish.
    fn advance(&mut self) -> bool {
        let mut changed = false;
        while let Some(index) = self
            .progress
            .iter()
            .position(|progress| *progress == MemberProgress::Waiting)
        {
            let Some(member) = self.members.get(index) else {
                break;
            };
            let request = SweepRequest {
                group: self.group,
                member: member.device.clone(),
                output: member.output,
                microphone_device: self.microphone_device.clone(),
                microphone: self.microphone,
            };
            changed = true;
            match self.source.begin(&request) {
                Ok(()) => {
                    if let Some(slot) = self.progress.get_mut(index) {
                        *slot = MemberProgress::Measuring;
                    }
                    return true;
                }
                Err(reason) => {
                    if let Some(slot) = self.progress.get_mut(index) {
                        *slot = MemberProgress::Failed(reason);
                    }
                }
            }
        }
        if self.progress.contains(&MemberProgress::Measuring) {
            return changed;
        }
        self.finish();
        true
    }

    fn finish(&mut self) {
        let failed = self
            .members
            .iter()
            .zip(&self.progress)
            .find_map(|(member, progress)| match progress {
                MemberProgress::Failed(reason) => Some((member.label.clone(), reason.clone())),
                _ => None,
            });
        if let Some((label, reason)) = failed {
            self.phase = CalibrationPhase::Failed;
            self.message = Some(crate::shell::i18n::t_args(
                "speaker-calibration-failed",
                &[("speaker", label.into()), ("reason", reason.into())],
            ));
            return;
        }
        let measurements = self
            .progress
            .iter()
            .filter_map(|progress| match progress {
                MemberProgress::Measured(measurement) => Some(measurement.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let aligned = media::calibration::CalibrationResult::align(&measurements);
        self.after = Some(
            aligned
                .per_member
                .iter()
                .map(SpeakerTrim::from_calibration)
                .collect(),
        );
        self.phase = CalibrationPhase::Proposed;
        self.message = None;
    }

    /// Stop measuring. A run that already finished is unaffected.
    pub fn cancel(&mut self) {
        if self.phase == CalibrationPhase::Measuring {
            self.source.cancel();
            self.phase = CalibrationPhase::Failed;
            self.message = Some(crate::shell::i18n::t("speaker-calibration-cancelled"));
        }
    }

    /// The trims applying the proposal would store, per member.
    #[must_use]
    pub fn proposed_trims(&self) -> Option<Vec<(DeviceId, EndpointSelector, SpeakerTrim)>> {
        (self.phase == CalibrationPhase::Proposed)
            .then(|| self.with_members(self.after.as_deref()?))
            .flatten()
    }

    /// The trims reverting an applied proposal would restore.
    #[must_use]
    pub fn previous_trims(&self) -> Option<Vec<(DeviceId, EndpointSelector, SpeakerTrim)>> {
        (self.phase == CalibrationPhase::Applied).then(|| self.with_members(&self.before))?
    }

    fn with_members(
        &self,
        trims: &[SpeakerTrim],
    ) -> Option<Vec<(DeviceId, EndpointSelector, SpeakerTrim)>> {
        (trims.len() == self.members.len()).then(|| {
            self.members
                .iter()
                .zip(trims)
                .map(|(member, trim)| (member.device.clone(), member.output, trim.clone()))
                .collect()
        })
    }

    pub fn mark_applied(&mut self) {
        if self.phase == CalibrationPhase::Proposed {
            self.phase = CalibrationPhase::Applied;
        }
    }

    /// Whether this run still describes `group` as it stands: the same
    /// members on the same outputs.
    #[must_use]
    pub fn matches(&self, group: &SpeakerGroup) -> bool {
        let same_group = self.group == group.id;
        same_group
            && group.members.len() == self.members.len()
            && group
                .members
                .iter()
                .zip(&self.members)
                .all(|(member, run)| member.device == run.device && member.output == run.output)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::shell::device_fabric::FabricStamp;
    use crate::shell::speaker_group::{SpeakerGroupMember, SpeakerRole, StereoSide};

    fn id(value: &str) -> DeviceId {
        DeviceId::new(value).unwrap()
    }

    fn measurement(arrival_ns: i64, level_db: f64) -> media::calibration::SpeakerMeasurement {
        media::calibration::SpeakerMeasurement {
            arrival_samples: 0.0,
            arrival_ns,
            level_db,
            response: media::calibration::MagnitudeResponse::default(),
            eq: Vec::new(),
        }
    }

    type Script = VecDeque<Result<media::calibration::SpeakerMeasurement, String>>;

    /// A measurement source that answers from a script, one poll late, and
    /// records every request it was given.
    struct Fake {
        script: Script,
        pending: bool,
        requests: Arc<Mutex<Vec<SweepRequest>>>,
    }

    impl MeasurementSource for Fake {
        fn begin(&mut self, request: &SweepRequest) -> Result<(), String> {
            self.requests.lock().unwrap().push(request.clone());
            self.pending = true;
            Ok(())
        }

        fn poll(&mut self) -> Option<Result<media::calibration::SpeakerMeasurement, String>> {
            if !self.pending {
                return None;
            }
            self.pending = false;
            self.script.pop_front()
        }

        fn cancel(&mut self) {
            self.pending = false;
        }
    }

    fn pair() -> SpeakerGroup {
        let member = |device: &str, side| SpeakerGroupMember {
            device: id(device),
            output: EndpointSelector::mint(&id(device), "out"),
            role: SpeakerRole::Stereo(side),
            trim: SpeakerTrim {
                delay_ns: 1_000,
                gain_mb: -50,
                eq: Vec::new(),
            },
        };
        SpeakerGroup {
            id: SpeakerGroupId([3; 16]),
            name: "Living room".into(),
            leader: id("desk"),
            members: vec![
                member("desk", StereoSide::Left),
                member("shelf", StereoSide::Right),
            ],
            stamp: FabricStamp {
                counter: 1,
                device: id("desk"),
            },
        }
    }

    fn run_with(script: Script) -> (CalibrationRun, Arc<Mutex<Vec<SweepRequest>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let run = CalibrationRun::start(
            &pair(),
            &["Desk".into(), "Shelf".into()],
            id("phone"),
            EndpointSelector::mint(&id("phone"), "mic"),
            Box::new(Fake {
                script,
                pending: false,
                requests: Arc::clone(&requests),
            }),
        );
        (run, requests)
    }

    #[test]
    fn members_are_measured_one_at_a_time_then_proposed() {
        let (mut run, requests) = run_with(VecDeque::from([
            Ok(measurement(4_000_000, -6.0)),
            Ok(measurement(7_000_000, -9.0)),
        ]));
        assert_eq!(run.phase(), CalibrationPhase::Measuring);
        assert_eq!(run.progress()[0], MemberProgress::Measuring);
        assert_eq!(run.progress()[1], MemberProgress::Waiting);
        assert_eq!(requests.lock().unwrap().len(), 1);
        assert!(run.tick());
        assert_eq!(run.progress()[1], MemberProgress::Measuring);
        let asked = requests.lock().unwrap().clone();
        assert_eq!(asked[1].member, id("shelf"));
        assert_eq!(asked[1].microphone_device, id("phone"));
        assert!(run.tick());
        assert_eq!(run.phase(), CalibrationPhase::Proposed);
        let after = run.after().unwrap();
        // The earlier, louder desk waits for the shelf and is cut to match it.
        assert_eq!(after[0].delay_ns, 3_000_000);
        assert_eq!(after[0].gain_mb, -300);
        assert_eq!(after[1].delay_ns, 0);
        assert_eq!(after[1].gain_mb, 0);
        assert_eq!(
            run.before()[0].delay_ns,
            1_000,
            "before keeps current trims"
        );
    }

    #[test]
    fn nothing_is_applied_until_asked_and_revert_restores_the_old_trims() {
        let (mut run, _) = run_with(VecDeque::from([
            Ok(measurement(0, 0.0)),
            Ok(measurement(2_000_000, 0.0)),
        ]));
        run.tick();
        run.tick();
        assert!(run.previous_trims().is_none(), "nothing applied yet");
        let proposed = run.proposed_trims().unwrap();
        assert_eq!(proposed[0].2.delay_ns, 2_000_000);
        run.mark_applied();
        assert!(run.proposed_trims().is_none());
        let previous = run.previous_trims().unwrap();
        assert_eq!(previous[0].2.delay_ns, 1_000);
        assert_eq!(previous[1].0, id("shelf"));
    }

    #[test]
    fn a_member_that_cannot_be_heard_fails_the_run_with_its_name() {
        let (mut run, _) = run_with(VecDeque::from([
            Ok(measurement(0, 0.0)),
            Err("no arrival stood out of the noise".into()),
        ]));
        run.tick();
        run.tick();
        assert_eq!(run.phase(), CalibrationPhase::Failed);
        assert_eq!(
            run.message(),
            Some("Could not measure Shelf: no arrival stood out of the noise")
        );
        assert!(run.proposed_trims().is_none());
    }

    #[test]
    fn timed_measurement_listens_only_on_this_device() {
        let mut source = TimedMeasurement::new(id("desk"), Box::new(|_, _| Ok(())));
        let request = SweepRequest {
            group: SpeakerGroupId([3; 16]),
            member: id("shelf"),
            output: EndpointSelector::mint(&id("shelf"), "out"),
            microphone_device: id("phone"),
            microphone: EndpointSelector::mint(&id("phone"), "mic"),
        };
        assert_eq!(
            source.begin(&request),
            Err("calibrate with a microphone on the device that set up the speakers".into())
        );
        assert!(source.poll().is_none());
    }

    #[test]
    fn cancelling_stops_measuring_and_a_changed_group_no_longer_matches() {
        let (mut run, _) = run_with(VecDeque::new());
        assert!(run.matches(&pair()));
        run.cancel();
        assert_eq!(run.phase(), CalibrationPhase::Failed);
        assert!(!run.tick());
        let mut changed = pair();
        changed.members.pop();
        assert!(!run.matches(&changed));
    }
}
