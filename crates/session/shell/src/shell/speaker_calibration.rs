//! Acoustic calibration of a speaker group, as a reversible proposal.
//!
//! Calibration measures each speaker from where the person sits. Each
//! member plays a logarithmic sweep in turn at an instant scheduled on the
//! calibrating device's clock (the group's leader, the "source"), while a
//! microphone at the listening position records around it.
//! [`media::calibration`] turns the recording into an arrival time and a
//! level. That arrival is the speaker's whole delay as the listener hears
//! it: the member's output pipeline plus the sound's flight to the seat.
//! Delaying every member to the latest arrival and cutting it to the
//! quietest level makes the group sound like one speaker there.
//!
//! The listening microphone can be on any of the person's devices: the one
//! running the wizard by default, or a phone put on the sofa. Microphones on
//! other devices may listen too, but only as extras:
//!
//! * a member's own microphone hears that member from a few centimetres, so
//!   its arrival is the member's output latency alone. Play and record share
//!   that device's clock, so fabric-clock error cancels. Subtracting it from
//!   the arrival at the seat leaves the time of flight, which says how far
//!   the speaker is from the listener;
//! * any microphone cross-checks the listening one: a reading that disagrees
//!   with every other microphone by more than a room could explain is set
//!   aside, and a reading that is missing or buried in noise is estimated
//!   from the others ([`media::calibration_fusion`]).
//!
//! A microphone on another device is recorded only with that device's own
//! consent ([`crate::shell::speaker_group::SpeakerListener`]); the device
//! measures its recording itself and sends back only the arrival, level and
//! smoothed response. The calibrating device schedules each recording on the
//! listening device's clock through the fabric clock estimate it holds for
//! that device ([`crate::shell::fabric_clock`]), so capture timestamps and
//! the sweep share one timeline; that estimate's error bound weights the
//! reading.
//!
//! Nothing is applied implicitly. A finished run is a proposal showing each
//! member's trims before and after; applying stores the new trims on the
//! group and keeps the old ones for one explicit revert, the same shape as
//! the spatial display calibration.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::shell::device_fabric::PersonalFabricAction;
use crate::shell::personal_continuity::DeviceId;
use crate::shell::speaker_group::{EndpointSelector, SpeakerGroup, SpeakerGroupId, SpeakerTrim};

/// Sweep used for every member: 20 Hz to 20 kHz over three seconds.
pub const SWEEP_START_HZ: f64 = 20.0; // a frequency, not a pixel length
pub const SWEEP_END_HZ: f64 = 20_000.0;
pub const SWEEP_SECONDS: f64 = 3.0; // a duration, not a pixel length
pub const SWEEP_RATE: u32 = 48_000;
/// Points of a smoothed response a listening device may send back.
pub const MAX_RESPONSE_POINTS: usize = 512;
/// How far ahead a sweep is scheduled: time for the requests to cross the
/// fabric and for every stream to open.
const SWEEP_LEAD_NS: i64 = 1_200_000_000;
/// Recording starts this long before the sweep so its onset is captured.
const CAPTURE_PREROLL_NS: i64 = 200_000_000;
/// Recording continues this long after the sweep for late arrivals and the
/// room's decay.
const CAPTURE_TAIL_NS: i64 = 700_000_000;
/// How long after the recording ends a listening device may take to
/// measure and answer.
const ANSWER_GRACE_NS: i64 = 3_000_000_000;
/// A listening request further ahead than this is refused.
const MAX_LISTEN_LEAD_NS: i64 = 10_000_000_000;
/// How long a run waits for devices to answer the microphone prompt before
/// measuring without the ones that did not.
pub const MICROPHONE_CONSENT_WAIT: Duration = Duration::from_secs(60);
/// Largest arrival a listening device may report.
const MAX_REPORTED_ARRIVAL_NS: i64 = 1_000_000_000;

/// The sweep every device plays and measures against.
pub fn sweep() -> Result<media::calibration::LogSweep, String> {
    media::calibration::LogSweep::new(SWEEP_RATE, SWEEP_START_HZ, SWEEP_END_HZ, SWEEP_SECONDS)
        .map_err(|error| error.to_string())
}

fn sweep_capture_ns() -> i64 {
    let seconds_ns = SWEEP_SECONDS * 1e9;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a three-second sweep is far inside i64 nanoseconds"
    )]
    let sweep_ns = seconds_ns as i64;
    sweep_ns
        .saturating_add(CAPTURE_PREROLL_NS)
        .saturating_add(CAPTURE_TAIL_NS)
}

/// Why a microphone has no reading of a sweep. Typed, so no process detail
/// of the listening device crosses the fabric.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HearingFailure {
    /// Nothing stood out of the noise: too far away, or muted.
    NoArrival,
    /// That device has not agreed to listen for this group.
    NotAllowed,
    MicrophoneGone,
    /// The request arrived too late, or too far ahead, to record around.
    OutOfWindow,
    /// That device is already recording another sweep.
    Busy,
    CaptureFailed,
    /// No estimate of that device's clock yet.
    ClockUnknown,
    /// No answer before the sweep's deadline.
    NoAnswer,
}

impl HearingFailure {
    #[must_use]
    pub fn describe(self) -> String {
        crate::shell::i18n::t(match self {
            Self::NoArrival => "speaker-calibration-reason-not-heard",
            Self::NotAllowed => "speaker-calibration-reason-not-allowed",
            Self::MicrophoneGone => "speaker-calibration-reason-microphone-gone",
            Self::OutOfWindow => "speaker-calibration-reason-out-of-window",
            Self::Busy => "speaker-calibration-reason-busy",
            Self::CaptureFailed => "speaker-calibration-reason-capture-failed",
            Self::ClockUnknown => "speaker-calibration-reason-clock-unknown",
            Self::NoAnswer => "speaker-calibration-reason-no-answer",
        })
    }
}

/// What one microphone heard of one sweep, as the listening device reports
/// it: numbers measured from the recording, never the recording.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpeakerHearing {
    Heard {
        /// Direct sound after the scheduled start, on the listening device's
        /// clock.
        arrival_ns: i64,
        /// Level and peak-to-noise ratio in hundredths of a dB.
        level_cdb: i32,
        snr_cdb: i32,
        /// The smoothed magnitude response on the sweep's fixed grid, in
        /// hundredths of a dB.
        response_cdb: Vec<i16>,
    },
    NotHeard {
        failure: HearingFailure,
    },
}

fn to_centi(value: f64) -> i32 {
    let scaled = (value * 100.0).round().clamp(-1_000_000.0, 1_000_000.0);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to a range i32 holds exactly"
    )]
    let centi = scaled as i32;
    centi
}

fn to_centi_i16(value: f64) -> i16 {
    let scaled = (value * 100.0)
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX));
    #[expect(clippy::cast_possible_truncation, reason = "clamped to the i16 range")]
    let centi = scaled as i16;
    centi
}

impl SpeakerHearing {
    /// A heard sweep, from the listening device's own measurement.
    #[must_use]
    pub fn from_measurement(measurement: &media::calibration::SpeakerMeasurement) -> Self {
        Self::Heard {
            arrival_ns: measurement.arrival_ns,
            level_cdb: to_centi(measurement.level_db),
            snr_cdb: to_centi(measurement.snr_db),
            response_cdb: measurement
                .response
                .points
                .iter()
                .take(MAX_RESPONSE_POINTS)
                .map(|point| to_centi_i16(point.db))
                .collect(),
        }
    }

    /// Bounded and plausible, as the fabric admits it.
    #[must_use]
    pub fn valid(&self) -> bool {
        match self {
            Self::Heard {
                arrival_ns,
                response_cdb,
                ..
            } => {
                (-MAX_REPORTED_ARRIVAL_NS..=MAX_REPORTED_ARRIVAL_NS).contains(arrival_ns)
                    && response_cdb.len() <= MAX_RESPONSE_POINTS
            }
            Self::NotHeard { .. } => true,
        }
    }

    #[must_use]
    pub const fn failure(&self) -> Option<HearingFailure> {
        match self {
            Self::Heard { .. } => None,
            Self::NotHeard { failure } => Some(*failure),
        }
    }

    /// The reading as a measurement the calibration maths consumes. Its
    /// room EQ is fitted later, from whichever reading the run keeps.
    #[must_use]
    pub fn measurement(&self) -> Option<media::calibration::SpeakerMeasurement> {
        let Self::Heard {
            arrival_ns,
            level_cdb,
            snr_cdb,
            response_cdb,
        } = self
        else {
            return None;
        };
        let db = response_cdb
            .iter()
            .map(|value| f64::from(*value) / 100.0)
            .collect::<Vec<_>>();
        let response =
            media::calibration::MagnitudeResponse::from_grid_db(SWEEP_START_HZ, SWEEP_END_HZ, &db)
                .unwrap_or_default();
        #[expect(
            clippy::cast_precision_loss,
            reason = "arrivals are bounded to a second"
        )]
        let arrival_samples = *arrival_ns as f64 * f64::from(SWEEP_RATE) / 1e9;
        Some(media::calibration::SpeakerMeasurement {
            arrival_samples,
            arrival_ns: *arrival_ns,
            level_db: f64::from(*level_cdb) / 100.0,
            snr_db: f64::from(*snr_cdb) / 100.0,
            response,
            eq: Vec::new(),
        })
    }
}

fn to_f32(sample: i16) -> f32 {
    f32::from(sample) / 32_768.0
}

/// Measure one recording of the sweep that was scheduled to leave the
/// speaker at `play_at_ns`, on the same clock as `first_sample_ns`.
#[must_use]
pub fn hear(samples: &[i16], first_sample_ns: i64, play_at_ns: i64) -> SpeakerHearing {
    let Ok(sweep) = sweep() else {
        return SpeakerHearing::NotHeard {
            failure: HearingFailure::CaptureFailed,
        };
    };
    let capture = samples.iter().copied().map(to_f32).collect::<Vec<_>>();
    match sweep.measure(&capture, first_sample_ns, play_at_ns) {
        Ok(measurement) => SpeakerHearing::from_measurement(&measurement),
        Err(media::calibration::CalibrationError::NoArrival) => SpeakerHearing::NotHeard {
            failure: HearingFailure::NoArrival,
        },
        Err(_) => SpeakerHearing::NotHeard {
            failure: HearingFailure::CaptureFailed,
        },
    }
}

/// Record `serial` around a sweep leaving at `play_at_ns` on this device's
/// clock and measure it. Blocks for the length of the recording.
fn capture_hearing(serial: u32, play_at_ns: i64) -> SpeakerHearing {
    media::pw_stream::TimedAudio::native()
        .capture_window(
            media::pw_stream::CaptureTarget {
                serial,
                monitor_sink: false,
            },
            SWEEP_RATE,
            1,
            play_at_ns.saturating_sub(CAPTURE_PREROLL_NS),
            sweep_capture_ns(),
        )
        .map_or(
            SpeakerHearing::NotHeard {
                failure: HearingFailure::CaptureFailed,
            },
            |window| hear(&window.samples, window.first_sample_ns, play_at_ns),
        )
}

// ---------------------------------------------------------------------------
// The listening device's side.

/// Answers this device owes calibrating leaders, sent by the next pump.
fn outbox() -> &'static Mutex<Vec<(DeviceId, PersonalFabricAction)>> {
    static OUTBOX: OnceLock<Mutex<Vec<(DeviceId, PersonalFabricAction)>>> = OnceLock::new();
    OUTBOX.get_or_init(|| Mutex::new(Vec::new()))
}

fn answer(leader: &DeviceId, group: SpeakerGroupId, sweep: [u8; 16], hearing: SpeakerHearing) {
    if let Ok(mut outbox) = outbox().lock() {
        outbox.push((
            leader.clone(),
            PersonalFabricAction::SpeakerHeard {
                group,
                sweep,
                hearing,
            },
        ));
    }
}

/// Answers finished since the last call.
pub(crate) fn drain_answers() -> Vec<(DeviceId, PersonalFabricAction)> {
    outbox()
        .lock()
        .map(|mut outbox| std::mem::take(&mut *outbox))
        .unwrap_or_default()
}

/// One listening request, already admitted by consent.
pub(crate) struct ListenRequest {
    pub leader: DeviceId,
    pub group: SpeakerGroupId,
    pub sweep: [u8; 16],
    pub microphone: EndpointSelector,
    /// When the sweep leaves the speaker, on this device's clock.
    pub start_ns: i64,
}

/// Record around a leader's sweep with one of this device's microphones and
/// answer with what it heard. Refusals answer at once.
pub(crate) fn listen_for_leader(local: &DeviceId, allowed: bool, request: ListenRequest) {
    static LISTENING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let refuse = |failure| {
        answer(
            &request.leader,
            request.group,
            request.sweep,
            SpeakerHearing::NotHeard { failure },
        );
    };
    if !allowed {
        return refuse(HearingFailure::NotAllowed);
    }
    let lead = request
        .start_ns
        .saturating_sub(media::pw_stream::monotonic_now_ns());
    if !(CAPTURE_PREROLL_NS..=MAX_LISTEN_LEAD_NS).contains(&lead) {
        return refuse(HearingFailure::OutOfWindow);
    }
    let Some(microphone) =
        crate::shell::speaker_group_runtime::resolve_local_microphone(local, request.microphone)
    else {
        return refuse(HearingFailure::MicrophoneGone);
    };
    if LISTENING.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return refuse(HearingFailure::Busy);
    }
    let ListenRequest {
        leader,
        group,
        sweep,
        start_ns,
        ..
    } = request;
    let spawned = std::thread::Builder::new()
        .name("calibration-listen".into())
        .spawn(move || {
            let hearing = capture_hearing(microphone.serial, start_ns);
            LISTENING.store(false, std::sync::atomic::Ordering::Release);
            answer(&leader, group, sweep, hearing);
        });
    if spawned.is_err() {
        LISTENING.store(false, std::sync::atomic::Ordering::Release);
    }
}

// ---------------------------------------------------------------------------
// The calibrating device's side.

/// Sweeps this device is waiting to hear about, by sweep id.
#[derive(Default)]
struct PendingSweep {
    group: Option<SpeakerGroupId>,
    answers: BTreeMap<DeviceId, Option<SpeakerHearing>>,
}

fn pending_sweeps() -> &'static Mutex<BTreeMap<[u8; 16], PendingSweep>> {
    static PENDING: OnceLock<Mutex<BTreeMap<[u8; 16], PendingSweep>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Record one device's answer for a sweep this device asked it to hear.
/// Answers from devices that were not asked, for another group, or twice,
/// are dropped.
pub(crate) fn deliver_hearing(
    source: &DeviceId,
    group: SpeakerGroupId,
    sweep: [u8; 16],
    hearing: SpeakerHearing,
) {
    let Ok(mut pending) = pending_sweeps().lock() else {
        return;
    };
    let Some(entry) = pending.get_mut(&sweep) else {
        return;
    };
    if entry.group != Some(group) {
        return;
    }
    if let Some(slot) = entry.answers.get_mut(source)
        && slot.is_none()
    {
        *slot = Some(hearing);
    }
}

/// One microphone's reading of one sweep, with the error bound of the
/// clock mapping the recording was scheduled through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MicrophoneHearing {
    pub hearing: SpeakerHearing,
    pub clock_error_ns: i64,
}

impl MicrophoneHearing {
    #[must_use]
    pub const fn failed(failure: HearingFailure) -> Self {
        Self {
            hearing: SpeakerHearing::NotHeard { failure },
            clock_error_ns: 0,
        }
    }
}

/// One member to measure with the run's microphones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SweepRequest {
    pub group: SpeakerGroupId,
    /// Random id binding every answer to this sweep.
    pub sweep: [u8; 16],
    pub member: DeviceId,
    pub output: EndpointSelector,
    /// Device and microphone of every listener, the listening one included.
    pub microphones: Vec<(DeviceId, EndpointSelector)>,
}

/// `play_at_ns` on this device's clock as the peer's clock reads it, with
/// the estimate's error bound there, both in nanoseconds.
#[must_use]
pub fn schedule_on_peer(
    clock: &crate::shell::fabric_clock::ClockEstimate,
    play_at_ns: i64,
) -> Option<(i64, i64)> {
    let mapping = clock.local_to_peer(u64::try_from(play_at_ns / 1_000).ok()?)?;
    let start_ns = i64::try_from(mapping.timestamp_us)
        .ok()?
        .checked_mul(1_000)?
        .checked_add(play_at_ns.rem_euclid(1_000))?;
    let error_ns = i64::try_from(mapping.error_us)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000);
    Some((start_ns, error_ns))
}

/// Plays a sweep on a member and has every microphone listen.
pub trait MeasurementSource: Send {
    /// Start measuring one member.
    ///
    /// # Errors
    ///
    /// A reason this member cannot be measured at all.
    fn begin(&mut self, request: &SweepRequest) -> Result<(), String>;
    /// Once every microphone answered or the sweep's deadline passed: one
    /// reading per requested microphone, in request order; or why the
    /// member could not be asked to play at all.
    fn poll(&mut self) -> Option<Result<Vec<MicrophoneHearing>, String>>;
    /// Abandon the in-flight measurement and release what the run held.
    fn cancel(&mut self);
}

pub type FabricSender = Box<dyn Fn(&DeviceId, PersonalFabricAction) -> Result<(), String> + Send>;

struct InFlight {
    sweep: [u8; 16],
    microphones: Vec<DeviceId>,
    /// Readings known without waiting: local failures and remote refusals
    /// decided here.
    immediate: BTreeMap<usize, MicrophoneHearing>,
    clock_error_ns: BTreeMap<usize, i64>,
    deadline_ns: i64,
}

/// Measures with the timed audio path: the member plays the sweep at a
/// scheduled instant (on this device's output, or asked over the fabric),
/// while each microphone records around it, here or on its own device.
pub struct TimedMeasurement {
    local: DeviceId,
    send: FabricSender,
    in_flight: Option<InFlight>,
    /// A sweep waiting for the clocks it is scheduled through.
    waiting: Option<SweepRequest>,
    clock_syncs: Vec<crate::shell::device_fabric_runtime::FabricClockSync>,
}

impl TimedMeasurement {
    /// Holds the fabric clock of every device in `peers` in fast sync for
    /// the life of the run, so each recording is scheduled on a fresh
    /// estimate.
    #[must_use]
    pub fn new(local: DeviceId, send: FabricSender, peers: &[DeviceId]) -> Self {
        let mut peers = peers
            .iter()
            .filter(|peer| **peer != local)
            .cloned()
            .collect::<Vec<_>>();
        peers.sort();
        peers.dedup();
        Self {
            clock_syncs: peers
                .into_iter()
                .map(crate::shell::device_fabric_runtime::hold_fabric_clock_sync)
                .collect(),
            local,
            send,
            in_flight: None,
            waiting: None,
        }
    }

    fn play_locally(&self, output: EndpointSelector, play_at: i64) -> Result<(), String> {
        let sink =
            crate::shell::speaker_group_runtime::resolve_local_output(&self.local, output)
                .ok_or_else(|| crate::shell::i18n::t("speaker-calibration-reason-output-gone"))?;
        let signal = sweep()?.signal().to_vec();
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
            .map(|_| ())
            .map_err(|error| {
                crate::shell::i18n::t_args(
                    "speaker-calibration-reason-play-failed",
                    &[("error", error.to_string().into())],
                )
            })
    }

    fn listen_locally(
        &self,
        request: &SweepRequest,
        microphone: EndpointSelector,
        play_at: i64,
    ) -> Result<(), HearingFailure> {
        let microphone =
            crate::shell::speaker_group_runtime::resolve_local_microphone(&self.local, microphone)
                .ok_or(HearingFailure::MicrophoneGone)?;
        let (local, group, sweep) = (self.local.clone(), request.group, request.sweep);
        std::thread::Builder::new()
            .name("calibration-listen".into())
            .spawn(move || {
                let hearing = capture_hearing(microphone.serial, play_at);
                deliver_hearing(&local, group, sweep, hearing);
            })
            .map(|_| ())
            .map_err(|_| HearingFailure::CaptureFailed)
    }
}

fn clock_reason(reason: crate::shell::fabric_clock::ClockSyncFailure) -> String {
    crate::shell::i18n::t_args(
        "speaker-calibration-reason-clock",
        &[("reason", crate::shell::i18n::t(reason.message_key()).into())],
    )
}

impl TimedMeasurement {
    fn clock_state(&self, device: &DeviceId) -> Option<crate::shell::fabric_clock::ClockSyncState> {
        self.clock_syncs
            .iter()
            .find(|sync| sync.peer() == device)
            .map(crate::shell::device_fabric_runtime::FabricClockSync::state)
    }

    /// Start the waiting sweep once the member's clock and every listening
    /// device's clock have settled. `None` while any is still syncing; a
    /// member whose clock failed fails the sweep with the reason, a
    /// listening device whose clock failed just has no reading.
    fn start_waiting(&mut self) -> Option<Result<(), String>> {
        use crate::shell::fabric_clock::ClockSyncState;
        let request = self.waiting.as_ref()?;
        let syncing = std::iter::once(&request.member)
            .chain(request.microphones.iter().map(|(device, _)| device))
            .filter(|device| **device != self.local)
            .any(|device| self.clock_state(device) == Some(ClockSyncState::Syncing));
        if syncing {
            return None;
        }
        let request = self.waiting.take()?;
        if request.member != self.local
            && let Some(ClockSyncState::Failed(reason)) = self.clock_state(&request.member)
        {
            return Some(Err(clock_reason(reason)));
        }
        Some(self.start(&request))
    }

    fn start(&mut self, request: &SweepRequest) -> Result<(), String> {
        let play_at = media::pw_stream::monotonic_now_ns().saturating_add(SWEEP_LEAD_NS);
        let mut immediate = BTreeMap::new();
        let mut clock_error_ns = BTreeMap::new();
        let mut asked = Vec::new();
        let mut remote = Vec::new();
        for (index, (device, microphone)) in request.microphones.iter().enumerate() {
            asked.push(device.clone());
            if device == &self.local {
                continue;
            }
            // Schedule the recording on that device's own clock, through the
            // estimate this device holds of it.
            let scheduled = crate::shell::device_fabric_runtime::fabric_peer_clock(device)
                .and_then(|clock| schedule_on_peer(&clock, play_at));
            let Some((start_ns, error_ns)) = scheduled else {
                immediate.insert(
                    index,
                    MicrophoneHearing::failed(HearingFailure::ClockUnknown),
                );
                continue;
            };
            clock_error_ns.insert(index, error_ns);
            remote.push((index, device.clone(), *microphone, start_ns));
        }
        if request.member == self.local {
            self.play_locally(request.output, play_at)?;
        } else {
            // The member plays on its own clock, converted the same way.
            let (start_ns, _) =
                crate::shell::device_fabric_runtime::fabric_peer_clock(&request.member)
                    .and_then(|clock| schedule_on_peer(&clock, play_at))
                    .ok_or_else(|| {
                        clock_reason(crate::shell::fabric_clock::ClockSyncFailure::NoReply)
                    })?;
            (self.send)(
                &request.member,
                PersonalFabricAction::SpeakerSweep {
                    group: request.group,
                    start_ns,
                },
            )?;
        }
        if let Ok(mut pending) = pending_sweeps().lock() {
            pending.insert(
                request.sweep,
                PendingSweep {
                    group: Some(request.group),
                    answers: asked.iter().map(|device| (device.clone(), None)).collect(),
                },
            );
        }
        for (index, (device, microphone)) in request.microphones.iter().enumerate() {
            if device == &self.local
                && let Err(failure) = self.listen_locally(request, *microphone, play_at)
            {
                immediate.insert(index, MicrophoneHearing::failed(failure));
            }
        }
        for (index, device, microphone, start_ns) in remote {
            let sent = (self.send)(
                &device,
                PersonalFabricAction::SpeakerListen {
                    group: request.group,
                    sweep: request.sweep,
                    microphone,
                    start_ns,
                },
            );
            if sent.is_err() {
                immediate.insert(index, MicrophoneHearing::failed(HearingFailure::NoAnswer));
            }
        }
        self.in_flight = Some(InFlight {
            sweep: request.sweep,
            microphones: asked,
            immediate,
            clock_error_ns,
            deadline_ns: play_at
                .saturating_add(sweep_capture_ns())
                .saturating_add(ANSWER_GRACE_NS),
        });
        Ok(())
    }
}

impl MeasurementSource for TimedMeasurement {
    fn begin(&mut self, request: &SweepRequest) -> Result<(), String> {
        self.waiting = Some(request.clone());
        match self.start_waiting() {
            Some(Err(reason)) => Err(reason),
            Some(Ok(())) | None => Ok(()),
        }
    }

    fn poll(&mut self) -> Option<Result<Vec<MicrophoneHearing>, String>> {
        if self.waiting.is_some() {
            if let Err(reason) = self.start_waiting()? {
                return Some(Err(reason));
            }
            return None;
        }
        let in_flight = self.in_flight.as_ref()?;
        let expired = media::pw_stream::monotonic_now_ns() >= in_flight.deadline_ns;
        let mut pending = pending_sweeps().lock().ok()?;
        let answers = pending.get(&in_flight.sweep).map(|entry| &entry.answers);
        let readings = in_flight
            .microphones
            .iter()
            .enumerate()
            .map(|(index, device)| {
                in_flight.immediate.get(&index).cloned().or_else(|| {
                    answers?
                        .get(device)
                        .cloned()
                        .flatten()
                        .map(|hearing| MicrophoneHearing {
                            hearing,
                            clock_error_ns: in_flight
                                .clock_error_ns
                                .get(&index)
                                .copied()
                                .unwrap_or(0),
                        })
                })
            })
            .collect::<Vec<_>>();
        if !expired && readings.iter().any(Option::is_none) {
            return None;
        }
        pending.remove(&in_flight.sweep);
        drop(pending);
        self.in_flight = None;
        Some(Ok(readings
            .into_iter()
            .map(|reading| {
                reading.unwrap_or_else(|| MicrophoneHearing::failed(HearingFailure::NoAnswer))
            })
            .collect()))
    }

    fn cancel(&mut self) {
        self.waiting = None;
        if let Some(in_flight) = self.in_flight.take()
            && let Ok(mut pending) = pending_sweeps().lock()
        {
            pending.remove(&in_flight.sweep);
        }
        self.clock_syncs.clear();
    }
}

// ---------------------------------------------------------------------------
// One run.

/// A member's fused result at the listening position.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberResult {
    /// Arrival at the listening position after the scheduled start.
    pub arrival_ns: i64,
    pub level_db: f64,
    pub heard_by: usize,
    /// This member's output latency, heard by its own microphone.
    pub output_latency_ns: Option<i64>,
    /// Arrival minus output latency: the flight time to the listener.
    pub acoustic_ns: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MemberProgress {
    Waiting,
    Measuring,
    Measured(MemberResult),
    Failed(String),
}

/// Whether a device agreed to listen, as the run learns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MicrophoneConsent {
    Given,
    Asked,
    /// Declined, or no longer able to listen.
    Refused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MicrophoneProgress {
    AwaitingConsent,
    Declined,
    Listening,
    Used {
        heard: usize,
        rejected: usize,
    },
    /// Heard none of the speakers: too far away to help.
    NotHeard,
    Failed(String),
}

/// One microphone taking part in a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunMicrophone {
    pub device: DeviceId,
    pub microphone: EndpointSelector,
    pub label: String,
    pub consent: MicrophoneConsent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalibrationPhase {
    /// Devices were asked to let their microphones listen.
    WaitingForMicrophones,
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

pub struct CalibrationRun {
    group: SpeakerGroupId,
    members: Vec<CalibrationMember>,
    progress: Vec<MemberProgress>,
    microphones: Vec<RunMicrophone>,
    microphone_progress: Vec<MicrophoneProgress>,
    listening: usize,
    /// Every reading so far: `[member][microphone]`.
    readings: Vec<Vec<Option<MicrophoneHearing>>>,
    before: Vec<SpeakerTrim>,
    after: Option<Vec<SpeakerTrim>>,
    phase: CalibrationPhase,
    message: Option<String>,
    consent_deadline: Instant,
    source: Box<dyn MeasurementSource>,
    mint: Box<dyn FnMut() -> [u8; 16] + Send>,
}

impl CalibrationRun {
    /// Start calibrating every member of `group`, in member order, from
    /// the microphone `listening` indexes. Microphones still awaiting
    /// consent are waited for first, at most [`MICROPHONE_CONSENT_WAIT`].
    pub fn start(
        group: &SpeakerGroup,
        labels: &[String],
        microphones: Vec<RunMicrophone>,
        listening: usize,
        source: Box<dyn MeasurementSource>,
        mint: Box<dyn FnMut() -> [u8; 16] + Send>,
        now: Instant,
    ) -> Result<Self, String> {
        if microphones
            .get(listening)
            .is_none_or(|microphone| microphone.consent == MicrophoneConsent::Refused)
        {
            return Err(crate::shell::i18n::t("speaker-error-listening-unavailable"));
        }
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
        let microphone_progress = microphones
            .iter()
            .map(|microphone| match microphone.consent {
                MicrophoneConsent::Given => MicrophoneProgress::Listening,
                MicrophoneConsent::Asked => MicrophoneProgress::AwaitingConsent,
                MicrophoneConsent::Refused => MicrophoneProgress::Declined,
            })
            .collect::<Vec<_>>();
        let waiting = microphone_progress.contains(&MicrophoneProgress::AwaitingConsent);
        let mut run = Self {
            group: group.id,
            progress: vec![MemberProgress::Waiting; members.len()],
            readings: vec![vec![None; microphones.len()]; members.len()],
            before: group
                .members
                .iter()
                .map(|member| member.trim.clone())
                .collect(),
            members,
            microphones,
            microphone_progress,
            listening,
            after: None,
            phase: if waiting {
                CalibrationPhase::WaitingForMicrophones
            } else {
                CalibrationPhase::Measuring
            },
            message: None,
            consent_deadline: now.checked_add(MICROPHONE_CONSENT_WAIT).unwrap_or(now),
            source,
            mint,
        };
        if !waiting {
            run.advance();
        }
        Ok(run)
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
    pub fn microphones(&self) -> &[RunMicrophone] {
        &self.microphones
    }

    #[must_use]
    pub fn microphone_progress(&self) -> &[MicrophoneProgress] {
        &self.microphone_progress
    }

    /// Index of the microphone at the listening position.
    #[must_use]
    pub fn listening(&self) -> usize {
        self.listening
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

    /// Take each device's latest answer to the microphone prompt; once
    /// every one answered, or the wait ran out, measure with those that
    /// agreed. Returns whether anything changed.
    pub fn update_consent(
        &mut self,
        consent: impl Fn(&DeviceId) -> MicrophoneConsent,
        now: Instant,
    ) -> bool {
        if self.phase != CalibrationPhase::WaitingForMicrophones {
            return false;
        }
        let mut changed = false;
        for (microphone, progress) in self
            .microphones
            .iter_mut()
            .zip(&mut self.microphone_progress)
        {
            if microphone.consent != MicrophoneConsent::Asked {
                continue;
            }
            let answer = consent(&microphone.device);
            if answer != MicrophoneConsent::Asked {
                microphone.consent = answer;
                *progress = if answer == MicrophoneConsent::Given {
                    MicrophoneProgress::Listening
                } else {
                    MicrophoneProgress::Declined
                };
                changed = true;
            }
        }
        let waiting = self
            .microphones
            .iter()
            .any(|microphone| microphone.consent == MicrophoneConsent::Asked);
        if waiting && now < self.consent_deadline {
            return changed;
        }
        for (microphone, progress) in self
            .microphones
            .iter_mut()
            .zip(&mut self.microphone_progress)
        {
            if microphone.consent == MicrophoneConsent::Asked {
                microphone.consent = MicrophoneConsent::Refused;
                *progress = MicrophoneProgress::Declined;
            }
        }
        let listening_refused = self
            .microphones
            .get(self.listening)
            .is_none_or(|microphone| microphone.consent != MicrophoneConsent::Given);
        if listening_refused {
            self.source.cancel();
            self.phase = CalibrationPhase::Failed;
            let label = self
                .microphones
                .get(self.listening)
                .map_or_else(String::new, |microphone| microphone.label.clone());
            self.message = Some(crate::shell::i18n::t_args(
                "speaker-calibration-listening-refused",
                &[("microphone", label.into())],
            ));
            return true;
        }
        self.phase = CalibrationPhase::Measuring;
        self.advance();
        true
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
        let readings = match self.source.poll() {
            None => return false,
            Some(Ok(readings)) => readings,
            Some(Err(reason)) => {
                if let Some(slot) = self.progress.get_mut(index) {
                    *slot = MemberProgress::Failed(reason);
                }
                self.advance();
                return true;
            }
        };
        let listening_failure = readings
            .get(self.listening)
            .and_then(|reading| reading.hearing.failure());
        let heard = readings
            .iter()
            .filter_map(|reading| reading.hearing.measurement())
            .collect::<Vec<_>>();
        let listening_reading = readings
            .get(self.listening)
            .and_then(|reading| reading.hearing.measurement());
        let progress = match (
            listening_reading.or_else(|| heard.first().cloned()),
            heard.len(),
        ) {
            (Some(measurement), heard_by) => MemberProgress::Measured(MemberResult {
                arrival_ns: measurement.arrival_ns,
                level_db: measurement.level_db,
                heard_by,
                output_latency_ns: None,
                acoustic_ns: None,
            }),
            (None, _) => MemberProgress::Failed(
                listening_failure
                    .unwrap_or(HearingFailure::NoArrival)
                    .describe(),
            ),
        };
        if let Some(row) = self.readings.get_mut(index) {
            for (slot, reading) in row.iter_mut().zip(readings) {
                *slot = Some(reading);
            }
        }
        if let Some(slot) = self.progress.get_mut(index) {
            *slot = progress;
        }
        self.advance();
        true
    }

    /// The microphones that may listen, in run order.
    fn listeners(&self) -> Vec<(DeviceId, EndpointSelector)> {
        self.microphones
            .iter()
            .filter(|microphone| microphone.consent == MicrophoneConsent::Given)
            .map(|microphone| (microphone.device.clone(), microphone.microphone))
            .collect()
    }

    /// Begin the next waiting member, or finish.
    fn advance(&mut self) -> bool {
        let mut changed = false;
        let listeners = self.listeners();
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
                sweep: (self.mint)(),
                member: member.device.clone(),
                output: member.output,
                microphones: listeners.clone(),
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

    /// Readings per member, re-indexed from the run's microphones to the
    /// ones that listened.
    fn fusion_hearings(&self, listened: &[usize]) -> Vec<media::calibration_fusion::Hearing> {
        let mut hearings = Vec::new();
        for (speaker, row) in self.readings.iter().enumerate() {
            for (microphone, run_index) in listened.iter().enumerate() {
                let Some(reading) = row.get(*run_index).cloned().flatten() else {
                    continue;
                };
                let Some(measurement) = reading.hearing.measurement() else {
                    continue;
                };
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "arrivals and clock errors are bounded to seconds"
                )]
                hearings.push(media::calibration_fusion::Hearing {
                    speaker,
                    microphone,
                    arrival_ns: measurement.arrival_ns as f64,
                    level_db: measurement.level_db,
                    snr_db: measurement.snr_db,
                    clock_error_ns: reading.clock_error_ns as f64,
                });
            }
        }
        hearings
    }

    #[expect(
        clippy::too_many_lines,
        reason = "fusion, EQ choice and per-microphone accounting read best in one pass"
    )]
    fn finish(&mut self) {
        self.source.cancel();
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
        let listened = self
            .microphones
            .iter()
            .enumerate()
            .filter(|(_, microphone)| microphone.consent == MicrophoneConsent::Given)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let Some(listening) = listened.iter().position(|index| *index == self.listening) else {
            self.phase = CalibrationPhase::Failed;
            self.message = Some(crate::shell::i18n::t("speaker-error-listening-unavailable"));
            return;
        };
        let own_microphone = self
            .members
            .iter()
            .map(|member| {
                listened.iter().position(|index| {
                    self.microphones
                        .get(*index)
                        .is_some_and(|microphone| microphone.device == member.device)
                })
            })
            .collect::<Vec<_>>();
        let hearings = self.fusion_hearings(&listened);
        let fusion = media::calibration_fusion::fuse(&media::calibration_fusion::FusionInput {
            speakers: self.members.len(),
            microphones: listened.len(),
            hearings: &hearings,
            listening,
            own_microphone: &own_microphone,
        });
        let mut measurements = Vec::new();
        for (speaker, fused) in fusion.speakers.iter().enumerate() {
            let label = self
                .members
                .get(speaker)
                .map_or_else(String::new, |member| member.label.clone());
            let Some(fused) = fused else {
                self.phase = CalibrationPhase::Failed;
                self.message = Some(crate::shell::i18n::t_args(
                    "speaker-calibration-failed",
                    &[
                        ("speaker", label.into()),
                        (
                            "reason",
                            crate::shell::i18n::t("speaker-calibration-reason-not-at-seat").into(),
                        ),
                    ],
                ));
                return;
            };
            // Room EQ from the listening microphone when it heard this
            // member; otherwise from the clearest reading kept.
            let readings = self.readings.get(speaker);
            let reading_at = |microphone: usize| {
                listened
                    .get(microphone)
                    .and_then(|index| readings?.get(*index)?.as_ref())
                    .and_then(|reading| reading.hearing.measurement())
            };
            let eq_source = if fused.at_listening {
                reading_at(listening)
            } else {
                (0..listened.len())
                    .filter(|microphone| !fusion.rejected.contains(&(speaker, *microphone)))
                    .filter_map(reading_at)
                    .max_by(|a, b| a.snr_db.total_cmp(&b.snr_db))
            };
            let eq = eq_source
                .map(|measurement| {
                    media::calibration::room_eq(
                        &measurement.response,
                        measurement.level_db,
                        SWEEP_RATE,
                    )
                })
                .unwrap_or_default();
            let round = |value: f64| {
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "arrivals are bounded to a second"
                )]
                let rounded = value.round().clamp(-1e12, 1e12) as i64;
                rounded
            };
            let arrival_ns = round(fused.arrival_ns);
            if let Some(slot) = self.progress.get_mut(speaker) {
                let heard_by = match slot {
                    MemberProgress::Measured(result) => result.heard_by,
                    _ => 0,
                };
                *slot = MemberProgress::Measured(MemberResult {
                    arrival_ns,
                    level_db: fused.level_db,
                    heard_by,
                    output_latency_ns: fused.output_latency_ns.map(round),
                    acoustic_ns: fused.acoustic_ns.map(round),
                });
            }
            measurements.push(media::calibration::SpeakerMeasurement {
                arrival_samples: fused.arrival_ns * f64::from(SWEEP_RATE) / 1e9,
                arrival_ns,
                level_db: fused.level_db,
                snr_db: 0.0,
                response: media::calibration::MagnitudeResponse::default(),
                eq,
            });
        }
        for (run_index, progress) in self.microphone_progress.iter_mut().enumerate() {
            let Some(microphone) = listened.iter().position(|index| *index == run_index) else {
                continue;
            };
            let heard = self
                .readings
                .iter()
                .filter(|row| {
                    row.get(run_index)
                        .and_then(Option::as_ref)
                        .is_some_and(|reading| reading.hearing.failure().is_none())
                })
                .count();
            let rejected = fusion
                .rejected
                .iter()
                .filter(|(_, rejected)| *rejected == microphone)
                .count();
            let failure = self.readings.iter().find_map(|row| {
                row.get(run_index)
                    .and_then(Option::as_ref)
                    .and_then(|reading| reading.hearing.failure())
                    .filter(|failure| *failure != HearingFailure::NoArrival)
            });
            *progress = match (heard, failure) {
                (0, Some(failure)) => MicrophoneProgress::Failed(failure.describe()),
                (0, None) => MicrophoneProgress::NotHeard,
                (heard, _) => MicrophoneProgress::Used { heard, rejected },
            };
        }
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
        if matches!(
            self.phase,
            CalibrationPhase::Measuring | CalibrationPhase::WaitingForMicrophones
        ) {
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
mod tests;
