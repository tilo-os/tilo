//! Peer clock offset and skew over the personal device fabric.
//!
//! The clock has its own probe and report, which carry timestamps and
//! nothing else, and are part of Media continuity: speaker groups and remote
//! audio are what need it. It runs only toward peers something holds in sync
//! (see [`FabricClockCore::hold`]). Every round trip carries four
//! `CLOCK_MONOTONIC` microsecond timestamps: `t1` when this device sent the probe, `t2` when the peer
//! received it, `t3` when the peer sent its report and `t4` when this device
//! received that report. Per round trip:
//!
//! - offset `((t2 - t1) + (t3 - t4)) / 2` is how far the peer clock is ahead;
//! - delay `(t4 - t1) - (t3 - t2)` is the time spent on the network.
//!
//! An asymmetric path biases a single offset by up to half its delay, and
//! queueing inflates both. The estimator therefore keeps a sliding window,
//! keeps only the lowest-delay round trip from each stretch of that window,
//! drops stretches whose best round trip was still congested, and fits
//! `offset(local) = offset_at_reference + skew * (local - reference)` with a
//! Theil–Sen regression (median of pairwise slopes), which ignores the few
//! outliers that survive selection.
//!
//! Monotonic clocks stop while a device is suspended, so a peer that resumes
//! appears to jump. Several consistent low-delay round trips that disagree
//! with the model restart the window instead of being averaged in.
//!
//! Everything here is deterministic and free of I/O; the fabric runtime reads
//! the clock, sends the probes and feeds the reports in.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};
use ui::num::f64_of_i64;

use serde::{Deserialize, Serialize};

use crate::shell::device_fabric::PersonalFabricAction;
use crate::shell::personal_continuity::DeviceId;

/// Probe cadence toward a peer while something holds its clock in sync.
pub const CLOCK_SYNC_PROBE_INTERVAL: Duration = Duration::from_millis(250);
/// How long a held peer may go without one usable report before the holder
/// is told it cannot play in time, and why.
pub const CLOCK_SYNC_DEADLINE: Duration = Duration::from_secs(5);
/// A probe unanswered for this long no longer counts; longer round trips
/// carry no timing information anyway.
const CLOCK_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
/// Probes awaiting a report per peer: enough that the slowest usable round
/// trip still completes at the synchronised cadence.
const MAX_OUTSTANDING_CLOCK_PROBES: usize = 8;

/// Round trips kept per peer. At the 4 Hz synchronised cadence this spans a
/// minute, long enough to resolve skew to a few ppm.
pub const CLOCK_WINDOW: usize = 256;
/// The window is split into this many contiguous stretches and the
/// lowest-delay round trip of each becomes a regression point.
const SELECTION_STRETCHES: usize = 16;
/// A stretch whose best delay exceeds twice the window minimum plus this
/// slack was congested throughout and is not used.
const DELAY_SLACK_US: i64 = 500;
/// Skew is estimated only once the selected points span this long; shorter
/// spans amplify per-point noise into large rate errors.
const MIN_SKEW_SPAN_US: i64 = 8_000_000;
const MIN_SYNCED_POINTS: usize = 6;
/// The largest residual among the selected points for the estimate to count
/// as synchronised.
const MAX_SYNCED_JITTER_US: f64 = 1_000.0;
/// Crystal oscillators stay well inside this; a larger fitted rate is noise.
const MAX_SKEW: f64 = 500e-6;
/// How much the error bound widens per microsecond of extrapolation beyond
/// the newest sample, covering residual skew error and oscillator wander.
const DRIFT_ALLOWANCE: f64 = 20e-6;
/// An estimate whose newest sample is older than this is no longer synced.
const STALE_AFTER_US: i64 = 60_000_000;
/// Round trips slower than this carry no useful timing information.
const MAX_DELAY_US: i64 = 2_000_000;
/// A low-delay round trip further than this (plus its own half delay and the
/// model's jitter) from the prediction is evidence of a clock step.
const STEP_THRESHOLD_US: f64 = 5_000.0;
const STEP_CONFIRMATIONS: usize = 3;

/// One probe round trip, all in `CLOCK_MONOTONIC` microseconds. `t1`/`t4`
/// are on this device's clock, `t2`/`t3` on the peer's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockSample {
    pub t1: u64,
    pub t2: u64,
    pub t3: u64,
    pub t4: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "every field is in microseconds, matching the unit suffix used throughout"
)]
struct RoundTrip {
    /// Local midpoint of the round trip, where its offset applies.
    local_us: i64,
    offset_us: i64,
    delay_us: i64,
}

impl ClockSample {
    fn round_trip(self) -> Option<RoundTrip> {
        let t1 = i64::try_from(self.t1).ok()?;
        let t2 = i64::try_from(self.t2).ok()?;
        let t3 = i64::try_from(self.t3).ok()?;
        let t4 = i64::try_from(self.t4).ok()?;
        let elapsed = t4.checked_sub(t1)?;
        let turnaround = t3.checked_sub(t2)?;
        if elapsed < 0 || turnaround < 0 {
            return None;
        }
        let delay_us = elapsed.checked_sub(turnaround)?;
        if !(0..=MAX_DELAY_US).contains(&delay_us) {
            return None;
        }
        let offset_us = t2
            .checked_sub(t1)?
            .checked_add(t3.checked_sub(t4)?)?
            .checked_div(2)?;
        let local_us = t1.checked_add(elapsed.checked_div(2)?)?;
        Some(RoundTrip {
            local_us,
            offset_us,
            delay_us,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Fit {
    reference_local_us: i64,
    offset_at_reference_us: f64,
    skew: f64,
    min_delay_us: i64,
    jitter_us: f64,
    points: usize,
    span_us: i64,
    newest_local_us: i64,
}

impl Fit {
    fn offset_at(&self, local_us: i64) -> Option<f64> {
        let since = local_us.checked_sub(self.reference_local_us)?;
        Some(
            self.skew
                .mul_add(f64_of_i64(since), self.offset_at_reference_us),
        )
    }
}

/// A peer clock model at one moment, cheap to copy to consumers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockEstimate {
    reference_local_us: i64,
    offset_at_reference_us: f64,
    skew: f64,
    base_error_us: f64,
    newest_sample_local_us: i64,
    /// Enough well-spread, consistent, recent round trips back this model
    /// for scheduling against it. The error bound still says how tightly.
    pub synced: bool,
}

/// A timestamp translated between clock domains, with the bound on how far
/// it may be from the truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockMapping {
    pub timestamp_us: u64,
    pub error_us: u64,
}

impl ClockEstimate {
    /// Peer clock rate relative to this device, in parts per million.
    #[must_use]
    pub fn skew_ppm(&self) -> f64 {
        self.skew * 1e6
    }

    /// How far the peer clock is ahead of this device at `local_us`.
    #[must_use]
    pub fn offset_us(&self, local_us: u64) -> Option<i64> {
        let since = i64::try_from(local_us)
            .ok()?
            .checked_sub(self.reference_local_us)?;
        round_us(
            self.skew
                .mul_add(f64_of_i64(since), self.offset_at_reference_us),
        )
    }

    /// Error bound at `local_us`: half the best observed delay plus the
    /// model's residual, widened with distance from the newest sample.
    #[must_use]
    pub fn error_us(&self, local_us: u64) -> u64 {
        let distance = i64::try_from(local_us)
            .ok()
            .and_then(|local| local.checked_sub(self.newest_sample_local_us))
            .map_or(f64::INFINITY, |distance| f64_of_i64(distance).abs());
        let bound = DRIFT_ALLOWANCE.mul_add(distance, self.base_error_us).ceil();
        round_us(bound)
            .and_then(|bound| u64::try_from(bound).ok())
            .unwrap_or(u64::MAX)
    }

    #[must_use]
    pub fn local_to_peer(&self, local_us: u64) -> Option<ClockMapping> {
        let peer = i64::try_from(local_us)
            .ok()?
            .checked_add(self.offset_us(local_us)?)?;
        Some(ClockMapping {
            timestamp_us: u64::try_from(peer).ok()?,
            error_us: self.error_us(local_us),
        })
    }

    #[must_use]
    pub fn peer_to_local(&self, peer_us: u64) -> Option<ClockMapping> {
        // peer = local + offset + skew * (local - reference), solved for local.
        let peer_since = i64::try_from(peer_us)
            .ok()?
            .checked_sub(self.reference_local_us)?;
        let since = (f64_of_i64(peer_since) - self.offset_at_reference_us) / (1.0 + self.skew);
        let local = self.reference_local_us.checked_add(round_us(since)?)?;
        let local = u64::try_from(local).ok()?;
        Some(ClockMapping {
            timestamp_us: local,
            error_us: self.error_us(local),
        })
    }
}

/// A peer's clock model as the mapping a synchronized playout renders
/// against, with the peer as the group leader. Continuous in nanoseconds, so
/// the playout's drift loop sees a smooth line rather than microsecond steps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeerPlayoutClock {
    reference_local_ns: i64,
    /// How far the peer clock is ahead at the reference.
    offset_at_reference_ns: f64,
    /// Peer clock rate minus one, relative to this device.
    skew: f64,
}

impl PeerPlayoutClock {
    /// A peer clock `offset_ns` ahead of this one at local time
    /// `reference_local_ns`, running `skew_ppm` parts per million faster.
    #[must_use]
    pub fn new(reference_local_ns: i64, offset_ns: i64, skew_ppm: f64) -> Self {
        Self {
            reference_local_ns,
            offset_at_reference_ns: f64_of_i64(offset_ns),
            skew: skew_ppm * 1e-6,
        }
    }
}

impl media::sync_playout::ClockMapping for PeerPlayoutClock {
    fn leader_to_local_ns(&self, leader_ns: i64) -> i64 {
        // peer = local + offset + skew * (local - reference), solved for local.
        let peer_since = f64_of_i64(leader_ns.saturating_sub(self.reference_local_ns));
        let since = (peer_since - self.offset_at_reference_ns) / (1.0 + self.skew);
        self.reference_local_ns
            .saturating_add(round_us(since).unwrap_or(0))
    }

    fn local_to_leader_ns(&self, local_ns: i64) -> i64 {
        let since = local_ns.saturating_sub(self.reference_local_ns);
        let offset = self
            .skew
            .mul_add(f64_of_i64(since), self.offset_at_reference_ns);
        local_ns.saturating_add(round_us(offset).unwrap_or(0))
    }
}

impl ClockEstimate {
    /// This model as a playout clock mapping. Skew is trusted only once the
    /// estimate is synchronised; before that the fit spans too little time
    /// and only its offset is used.
    #[must_use]
    pub fn playout_clock(&self) -> PeerPlayoutClock {
        PeerPlayoutClock {
            reference_local_ns: self.reference_local_us.saturating_mul(1_000),
            offset_at_reference_ns: self.offset_at_reference_us * 1_000.0,
            skew: if self.synced { self.skew } else { 0.0 },
        }
    }
}

/// Sliding-window offset and skew estimator for one peer.
#[derive(Clone, Debug, Default)]
pub struct PeerClockEstimator {
    window: VecDeque<RoundTrip>,
    step_candidates: Vec<RoundTrip>,
}

impl PeerClockEstimator {
    /// Feed one round trip. Returns `false` when the timestamps cannot come
    /// from a real exchange (a clock running backwards, a negative or
    /// implausibly long network delay).
    pub fn observe(&mut self, sample: ClockSample) -> bool {
        let Some(round_trip) = sample.round_trip() else {
            return false;
        };
        if let Some(fit) = self.fit()
            && fit.points >= MIN_SYNCED_POINTS
            && round_trip.delay_us <= congestion_limit(fit.min_delay_us)
            && let Some(predicted) = fit.offset_at(round_trip.local_us)
        {
            let deviation = (f64_of_i64(round_trip.offset_us) - predicted).abs();
            let tolerance =
                STEP_THRESHOLD_US + f64_of_i64(round_trip.delay_us) / 2.0 + fit.jitter_us;
            if deviation > tolerance {
                self.step_candidates.push(round_trip);
                if self.step_candidates.len() >= STEP_CONFIRMATIONS {
                    self.window = std::mem::take(&mut self.step_candidates).into();
                }
                return true;
            }
            self.step_candidates.clear();
        }
        let position = self
            .window
            .partition_point(|held| held.local_us <= round_trip.local_us);
        self.window.insert(position, round_trip);
        while self.window.len() > CLOCK_WINDOW {
            self.window.pop_front();
        }
        true
    }

    /// The current model, or `None` before the first round trip.
    #[must_use]
    pub fn estimate(&self, now_local_us: u64) -> Option<ClockEstimate> {
        let fit = self.fit()?;
        let age = i64::try_from(now_local_us)
            .ok()
            .and_then(|now| now.checked_sub(fit.newest_local_us))
            .unwrap_or(i64::MAX);
        let synced = fit.points >= MIN_SYNCED_POINTS
            && fit.span_us >= MIN_SKEW_SPAN_US
            && fit.jitter_us <= MAX_SYNCED_JITTER_US
            && age <= STALE_AFTER_US;
        Some(ClockEstimate {
            reference_local_us: fit.reference_local_us,
            offset_at_reference_us: fit.offset_at_reference_us,
            skew: fit.skew,
            base_error_us: f64_of_i64(fit.min_delay_us) / 2.0 + fit.jitter_us,
            newest_sample_local_us: fit.newest_local_us,
            synced,
        })
    }

    #[must_use]
    pub fn samples(&self) -> usize {
        self.window.len()
    }

    fn fit(&self) -> Option<Fit> {
        let stretch = self.window.len().div_ceil(SELECTION_STRETCHES).max(1);
        let mut selected: Vec<RoundTrip> = Vec::with_capacity(SELECTION_STRETCHES);
        for (index, round_trip) in self.window.iter().enumerate() {
            if index.checked_rem(stretch) == Some(0) {
                selected.push(*round_trip);
            } else if let Some(best) = selected.last_mut()
                && round_trip.delay_us < best.delay_us
            {
                *best = *round_trip;
            }
        }
        let min_delay_us = selected.iter().map(|point| point.delay_us).min()?;
        selected.retain(|point| point.delay_us <= congestion_limit(min_delay_us));
        let first = selected.first()?.local_us;
        let newest_local_us = self.window.back()?.local_us;
        let reference_local_us = selected.last()?.local_us;
        let span_us = reference_local_us.checked_sub(first)?;
        let skew = if span_us >= MIN_SKEW_SPAN_US {
            let mut slopes = Vec::with_capacity(selected.len().saturating_mul(selected.len()) / 2);
            for (index, earlier) in selected.iter().enumerate() {
                for later in selected.iter().skip(index.saturating_add(1)) {
                    let run = later.local_us.checked_sub(earlier.local_us)?;
                    if run > 0 {
                        let rise = later.offset_us.checked_sub(earlier.offset_us)?;
                        slopes.push(f64_of_i64(rise) / f64_of_i64(run));
                    }
                }
            }
            median(&mut slopes)
                .unwrap_or(0.0)
                .clamp(-MAX_SKEW, MAX_SKEW)
        } else {
            0.0
        };
        let mut intercepts = selected
            .iter()
            .map(|point| {
                let since = point.local_us.checked_sub(reference_local_us)?;
                Some(f64_of_i64(point.offset_us) - skew * f64_of_i64(since))
            })
            .collect::<Option<Vec<_>>>()?;
        let offset_at_reference_us = median(&mut intercepts)?;
        let jitter_us = intercepts
            .iter()
            .map(|intercept| (intercept - offset_at_reference_us).abs())
            .fold(0.0, f64::max);
        Some(Fit {
            reference_local_us,
            offset_at_reference_us,
            skew,
            min_delay_us,
            jitter_us,
            points: selected.len(),
            span_us,
            newest_local_us,
        })
    }
}

fn congestion_limit(min_delay_us: i64) -> i64 {
    min_delay_us
        .saturating_mul(2)
        .saturating_add(DELAY_SLACK_US)
}

fn median(values: &mut [f64]) -> Option<f64> {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    let upper = *values.get(middle)?;
    if values.len() % 2 == 1 {
        return Some(upper);
    }
    let lower = *values.get(middle.checked_sub(1)?)?;
    Some(lower.midpoint(upper))
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "the value is finite, rounded and range-checked before the cast"
)]
fn round_us(value: f64) -> Option<i64> {
    let rounded = value.round();
    (rounded.is_finite() && rounded.abs() < 9.0e18).then_some(rounded as i64)
}

/// Reply to one clock probe, nothing but timestamps. `probe_sent_us`
/// echoes the prober's clock; `received_us` and `replied_us` are read from
/// the reporter's clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockReport {
    pub probe_nonce: [u8; 16],
    pub probe_sent_us: u64,
    pub received_us: u64,
    pub replied_us: u64,
}

impl ClockReport {
    #[must_use]
    pub fn valid(self) -> bool {
        self.probe_nonce != [0; 16] && self.replied_us >= self.received_us
    }
}

/// Why a held peer's clock is not known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockSyncFailure {
    /// Media continuity is off on this device, so no clock probe is sent or
    /// answered.
    MediaDisabled,
    /// The peer sent no usable clock report within [`CLOCK_SYNC_DEADLINE`].
    NoReply,
}

impl ClockSyncFailure {
    /// The shell string that explains this to a person.
    #[must_use]
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::MediaDisabled => "clock-sync-failure-media-disabled",
            Self::NoReply => "clock-sync-failure-no-reply",
        }
    }
}

/// Whether audio scheduled against a peer's clock can play.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockSyncState {
    /// No usable report yet; still inside [`CLOCK_SYNC_DEADLINE`].
    Syncing,
    /// An estimate exists and timed audio follows it.
    Ready,
    Failed(ClockSyncFailure),
}

#[derive(Clone, Copy, Debug)]
struct PendingClockProbe {
    nonce: [u8; 16],
    sent: Instant,
    sent_us: u64,
}

#[derive(Clone, Copy, Debug)]
struct HeldPeer {
    holders: usize,
    /// When the peer was first held, or when Media continuity came back on:
    /// the start of its [`CLOCK_SYNC_DEADLINE`].
    since: Instant,
}

/// The fabric clock for every peer something holds in sync: which probes
/// are due, which reports answer them, and each peer's estimate.
///
/// Probes go only to held peers, and only while Media continuity is on;
/// turning it off forgets every estimate. Reports are accepted only for an
/// outstanding nonce sent to that exact peer, and releasing the last hold on
/// a peer forgets its estimate too.
#[derive(Debug, Default)]
pub struct FabricClockCore {
    enabled: bool,
    held: BTreeMap<DeviceId, HeldPeer>,
    pending: BTreeMap<DeviceId, VecDeque<PendingClockProbe>>,
    clocks: BTreeMap<DeviceId, PeerClockEstimator>,
    last_round: Option<Instant>,
}

impl FabricClockCore {
    /// Apply the Media continuity switch. Turning it off drops every
    /// outstanding probe and estimate; turning it back on restarts each held
    /// peer's deadline.
    pub fn set_enabled(&mut self, enabled: bool, now: Instant) {
        if !enabled {
            self.pending.clear();
            self.clocks.clear();
            self.last_round = None;
        } else if !self.enabled {
            for held in self.held.values_mut() {
                held.since = now;
            }
        }
        self.enabled = enabled;
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Start (or count one more reason for) keeping `peer` in sync. The
    /// next [`Self::due_probes`] probes at once.
    pub fn hold(&mut self, peer: DeviceId, now: Instant) {
        let held = self.held.entry(peer).or_insert(HeldPeer {
            holders: 0,
            since: now,
        });
        held.holders = held.holders.saturating_add(1);
        self.last_round = None;
    }

    /// Drop one hold. The last one stops probing `peer` and forgets its
    /// estimate.
    pub fn release(&mut self, peer: &DeviceId) {
        let Some(held) = self.held.get_mut(peer) else {
            return;
        };
        held.holders = held.holders.saturating_sub(1);
        if held.holders == 0 {
            self.held.remove(peer);
            self.pending.remove(peer);
            self.clocks.remove(peer);
        }
    }

    #[must_use]
    pub fn is_held(&self, peer: &DeviceId) -> bool {
        self.held.contains_key(peer)
    }

    /// The probes due at `now` (`now_us` on the local monotonic clock): one
    /// to every held peer `online` accepts, once per
    /// [`CLOCK_SYNC_PROBE_INTERVAL`]. Each is recorded as outstanding.
    pub fn due_probes(
        &mut self,
        online: impl Fn(&DeviceId) -> bool,
        now: Instant,
        now_us: u64,
        mut nonce: impl FnMut() -> [u8; 16],
    ) -> Vec<(DeviceId, PersonalFabricAction)> {
        if !self.enabled || self.held.is_empty() {
            return Vec::new();
        }
        if self.last_round.is_some_and(|last| {
            now.checked_duration_since(last)
                .is_some_and(|age| age < CLOCK_SYNC_PROBE_INTERVAL)
        }) {
            return Vec::new();
        }
        self.last_round = Some(now);
        self.prune(now);
        let peers = self
            .held
            .keys()
            .filter(|peer| online(peer))
            .cloned()
            .collect::<Vec<_>>();
        let mut probes = Vec::with_capacity(peers.len());
        for peer in peers {
            let nonce = nonce();
            if nonce == [0; 16] {
                continue;
            }
            let outstanding = self.pending.entry(peer.clone()).or_default();
            if outstanding.len() >= MAX_OUTSTANDING_CLOCK_PROBES {
                outstanding.pop_front();
            }
            outstanding.push_back(PendingClockProbe {
                nonce,
                sent: now,
                sent_us: now_us,
            });
            probes.push((
                peer,
                PersonalFabricAction::FabricClockProbe {
                    nonce,
                    sent_us: now_us,
                },
            ));
        }
        probes
    }

    /// Forget one probe that never left this device.
    pub fn abandon(&mut self, peer: &DeviceId, nonce: [u8; 16]) {
        if let Some(outstanding) = self.pending.get_mut(peer) {
            outstanding.retain(|probe| probe.nonce != nonce);
            if outstanding.is_empty() {
                self.pending.remove(peer);
            }
        }
    }

    /// The report answering a peer's probe that arrived at `received_us` and
    /// is answered at `replied_us`, both on the local monotonic clock.
    /// `None` while Media continuity is off.
    #[must_use]
    pub fn answer(
        &self,
        nonce: [u8; 16],
        sent_us: u64,
        received_us: u64,
        replied_us: u64,
    ) -> Option<PersonalFabricAction> {
        (self.enabled && nonce != [0; 16]).then_some(PersonalFabricAction::FabricClockReport {
            report: ClockReport {
                probe_nonce: nonce,
                probe_sent_us: sent_us,
                received_us,
                replied_us: replied_us.max(received_us),
            },
        })
    }

    /// Feed a report from `peer` that arrived at `now`, `received_us` on the
    /// local monotonic clock. Only a reply to an outstanding probe to that
    /// exact peer counts, and each probe counts once.
    pub fn accept(
        &mut self,
        peer: &DeviceId,
        report: ClockReport,
        now: Instant,
        received_us: u64,
    ) -> bool {
        self.prune(now);
        if !self.enabled || !report.valid() || !self.held.contains_key(peer) {
            return false;
        }
        let Some(outstanding) = self.pending.get_mut(peer) else {
            return false;
        };
        let Some(index) = outstanding.iter().position(|probe| {
            probe.nonce == report.probe_nonce
                && probe.sent_us == report.probe_sent_us
                && now >= probe.sent
        }) else {
            return false;
        };
        let Some(probe) = outstanding.remove(index) else {
            return false;
        };
        if outstanding.is_empty() {
            self.pending.remove(peer);
        }
        self.clocks
            .entry(peer.clone())
            .or_default()
            .observe(ClockSample {
                t1: probe.sent_us,
                t2: report.received_us,
                t3: report.replied_us,
                t4: received_us,
            })
    }

    /// The peer's clock model as of `now_us` on the local monotonic clock.
    #[must_use]
    pub fn estimate(&self, peer: &DeviceId, now_us: u64) -> Option<ClockEstimate> {
        self.clocks.get(peer)?.estimate(now_us)
    }

    /// Whether audio timed against `peer` can play at `now`.
    #[must_use]
    pub fn state(&self, peer: &DeviceId, now: Instant, now_us: u64) -> ClockSyncState {
        if !self.enabled {
            return ClockSyncState::Failed(ClockSyncFailure::MediaDisabled);
        }
        if self.estimate(peer, now_us).is_some() {
            return ClockSyncState::Ready;
        }
        let expired = self.held.get(peer).is_some_and(|held| {
            now.checked_duration_since(held.since)
                .is_some_and(|waited| waited >= CLOCK_SYNC_DEADLINE)
        });
        if expired {
            ClockSyncState::Failed(ClockSyncFailure::NoReply)
        } else {
            ClockSyncState::Syncing
        }
    }

    /// Drop probes and estimates for peers the fabric no longer knows.
    pub fn retain_peers(&mut self, mut keep: impl FnMut(&DeviceId) -> bool) {
        self.pending.retain(|peer, _| keep(peer));
        self.clocks.retain(|peer, _| keep(peer));
    }

    fn prune(&mut self, now: Instant) {
        self.pending.retain(|_, outstanding| {
            outstanding.retain(|probe| {
                now.checked_duration_since(probe.sent)
                    .is_some_and(|age| age <= CLOCK_PROBE_TIMEOUT)
            });
            !outstanding.is_empty()
        });
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "the simulated network is plain floating-point and integer test arithmetic"
)]
mod tests {
    use super::*;

    const PEER_BOOT_OFFSET_US: f64 = 3_600_000_000.0;

    /// Deterministic xorshift so every run sees the same network.
    struct Rng(u64);

    impl Rng {
        fn unit(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1_u64 << 53) as f64
        }

        fn exponential(&mut self, mean: f64) -> f64 {
            -mean * (1.0 - self.unit()).ln()
        }
    }

    struct Network {
        rng: Rng,
        skew: f64,
        forward_base_us: f64,
        backward_base_us: f64,
        jitter_mean_us: f64,
        outlier_rate: f64,
    }

    impl Network {
        fn peer_clock(&self, local_us: f64) -> f64 {
            PEER_BOOT_OFFSET_US + local_us * (1.0 + self.skew)
        }

        fn true_offset(&self, local_us: f64) -> f64 {
            self.peer_clock(local_us) - local_us
        }

        fn leg(&mut self, base: f64) -> f64 {
            let mut delay = base + self.rng.exponential(self.jitter_mean_us);
            if self.rng.unit() < self.outlier_rate {
                delay += 5_000.0 + self.rng.unit() * 45_000.0;
            }
            delay
        }

        fn round_trip(&mut self, sent_us: f64) -> ClockSample {
            let forward = self.leg(self.forward_base_us);
            let turnaround = 50.0 + self.rng.unit() * 2_000.0;
            let backward = self.leg(self.backward_base_us);
            let arrive = sent_us + forward;
            ClockSample {
                t1: sent_us.round() as u64,
                t2: self.peer_clock(arrive).round() as u64,
                t3: self.peer_clock(arrive + turnaround).round() as u64,
                t4: (arrive + turnaround + backward).round() as u64,
            }
        }
    }

    fn network(seed: u64, skew_ppm: f64) -> Network {
        Network {
            rng: Rng(seed),
            skew: skew_ppm * 1e-6,
            // 0.6 ms of fixed path asymmetry biases every offset by 0.3 ms.
            forward_base_us: 2_000.0,
            backward_base_us: 1_400.0,
            jitter_mean_us: 300.0,
            outlier_rate: 0.1,
        }
    }

    const PERIOD_US: f64 = 250_000.0;
    const START_US: f64 = 5_000_000_000.0;

    #[test]
    fn converges_on_offset_and_skew_through_asymmetric_jittered_paths() {
        for seed in [0x9e37_79b9_7f4a_7c15, 7, 0xdead_beef, 42, 1234_5678] {
            for skew_ppm in [300.0, -300.0, 0.0, 120.0] {
                let mut net = network(seed, skew_ppm);
                let mut estimator = PeerClockEstimator::default();
                for round in 0..600_u32 {
                    let sent = START_US + f64::from(round) * PERIOD_US;
                    assert!(estimator.observe(net.round_trip(sent)));
                    if round < 300 {
                        continue;
                    }
                    let now = sent + 10_000.0;
                    let estimate = estimator.estimate(now as u64).expect("estimate");
                    assert!(estimate.synced, "seed {seed} skew {skew_ppm} round {round}");
                    let offset = estimate.offset_us(now as u64).expect("offset") as f64;
                    let error = (offset - net.true_offset(now)).abs();
                    assert!(
                        error < 500.0,
                        "offset error {error} us, seed {seed} skew {skew_ppm} round {round}"
                    );
                    let skew_error = (estimate.skew_ppm() - skew_ppm).abs();
                    assert!(
                        skew_error < 5.0,
                        "skew error {skew_error} ppm, seed {seed} skew {skew_ppm} round {round}"
                    );
                    assert!(
                        estimate.error_us(now as u64) as f64 >= error,
                        "the bound must cover the actual error"
                    );
                }
            }
        }
    }

    #[test]
    fn translations_round_trip_between_clock_domains() {
        let mut net = network(99, -300.0);
        let mut estimator = PeerClockEstimator::default();
        for round in 0..200_u32 {
            estimator.observe(net.round_trip(START_US + f64::from(round) * PERIOD_US));
        }
        let now = (START_US + 200.0 * PERIOD_US) as u64;
        let estimate = estimator.estimate(now).expect("estimate");
        let peer = estimate.local_to_peer(now).expect("peer");
        let truth = net.peer_clock(now as f64);
        assert!((peer.timestamp_us as f64 - truth).abs() < 500.0);
        let back = estimate.peer_to_local(peer.timestamp_us).expect("local");
        assert!(back.timestamp_us.abs_diff(now) <= 1);
        assert!(back.error_us >= 1 && peer.error_us >= 1);
        let later = now + 30_000_000;
        assert!(estimate.error_us(later) > estimate.error_us(now));
    }

    #[test]
    fn is_not_synced_before_skew_is_resolvable_or_once_stale() {
        let mut net = network(5, 300.0);
        let mut estimator = PeerClockEstimator::default();
        assert!(estimator.estimate(START_US as u64).is_none());
        for round in 0..8_u32 {
            estimator.observe(net.round_trip(START_US + f64::from(round) * PERIOD_US));
        }
        let now = (START_US + 8.0 * PERIOD_US) as u64;
        let early = estimator.estimate(now).expect("estimate");
        assert!(!early.synced);
        assert!(early.skew_ppm().abs() < f64::EPSILON);
        for round in 8..200_u32 {
            estimator.observe(net.round_trip(START_US + f64::from(round) * PERIOD_US));
        }
        let now = (START_US + 200.0 * PERIOD_US) as u64;
        assert!(estimator.estimate(now).expect("estimate").synced);
        let stale = now + 120_000_000;
        assert!(!estimator.estimate(stale).expect("estimate").synced);
    }

    #[test]
    fn rejects_impossible_round_trips() {
        let mut estimator = PeerClockEstimator::default();
        let valid = ClockSample {
            t1: 1_000,
            t2: 9_000_500,
            t3: 9_000_600,
            t4: 2_100,
        };
        assert!(!estimator.observe(ClockSample { t4: 900, ..valid }));
        assert!(!estimator.observe(ClockSample {
            t3: 9_000_400,
            ..valid
        }));
        assert!(!estimator.observe(ClockSample { t4: 1_050, ..valid }));
        assert!(!estimator.observe(ClockSample {
            t4: 5_000_000,
            ..valid
        }));
        assert!(!estimator.observe(ClockSample {
            t1: u64::MAX,
            ..valid
        }));
        assert_eq!(estimator.samples(), 0);
        assert!(estimator.observe(valid));
        let estimate = estimator.estimate(2_100).expect("estimate");
        // offset = ((9_000_500 - 1_000) + (9_000_600 - 2_100)) / 2; the 1 ms
        // delay bounds the error by half of it.
        assert_eq!(estimate.offset_us(1_550), Some(8_999_000));
        assert_eq!(estimate.error_us(2_100), 501);
    }

    #[test]
    fn restarts_after_the_peer_clock_steps() {
        let mut net = network(11, 50.0);
        let mut estimator = PeerClockEstimator::default();
        for round in 0..200_u32 {
            estimator.observe(net.round_trip(START_US + f64::from(round) * PERIOD_US));
        }
        // The peer slept: its monotonic clock paused for 40 s.
        net.outlier_rate = 0.0;
        let shifted = |sample: ClockSample| ClockSample {
            t2: sample.t2 - 40_000_000,
            t3: sample.t3 - 40_000_000,
            ..sample
        };
        for round in 200..203_u32 {
            let sample = shifted(net.round_trip(START_US + f64::from(round) * PERIOD_US));
            assert!(estimator.observe(sample));
        }
        assert_eq!(estimator.samples(), STEP_CONFIRMATIONS);
        for round in 203..300_u32 {
            estimator.observe(shifted(
                net.round_trip(START_US + f64::from(round) * PERIOD_US),
            ));
        }
        let now = START_US + 300.0 * PERIOD_US;
        let estimate = estimator.estimate(now as u64).expect("estimate");
        let offset = estimate.offset_us(now as u64).expect("offset") as f64;
        assert!((offset - (net.true_offset(now) - 40_000_000.0)).abs() < 500.0);
    }

    #[test]
    fn a_playout_clock_maps_both_ways_on_one_continuous_line() {
        use media::sync_playout::ClockMapping;
        // The peer booted an hour earlier and runs 40 ppm fast.
        let clock = PeerPlayoutClock::new(10_000_000_000, 3_600_000_000_000, 40.0);
        let local = 10_500_000_123;
        let leader = clock.local_to_leader_ns(local);
        assert_eq!(leader, local + 3_600_000_000_000 + 20_000);
        assert_eq!(clock.leader_to_local_ns(leader), local);
        // One local second is a second and 40 µs on the peer.
        assert_eq!(
            clock.local_to_leader_ns(local + 1_000_000_000) - leader,
            1_000_040_000
        );
    }

    #[test]
    fn an_unsynchronised_estimate_contributes_only_its_offset() {
        use media::sync_playout::ClockMapping;
        let mut estimator = PeerClockEstimator::default();
        // One round trip: the peer is 2 s ahead, symmetric 1 ms delay.
        assert!(estimator.observe(ClockSample {
            t1: 5_000_000,
            t2: 7_000_500,
            t3: 7_000_600,
            t4: 5_001_100,
        }));
        let estimate = estimator.estimate(5_001_100).unwrap();
        assert!(!estimate.synced);
        let clock = estimate.playout_clock();
        let leader = clock.local_to_leader_ns(6_000_000_000);
        assert_eq!(leader, 6_000_000_000 + 2_000_000_000);
    }

    fn device(name: &str) -> DeviceId {
        DeviceId::new(name).unwrap()
    }

    fn probe_of(action: &PersonalFabricAction) -> ([u8; 16], u64) {
        match action {
            PersonalFabricAction::FabricClockProbe { nonce, sent_us } => (*nonce, *sent_us),
            other => panic!("expected a clock probe, got {other:?}"),
        }
    }

    fn report_of(action: PersonalFabricAction) -> ClockReport {
        match action {
            PersonalFabricAction::FabricClockReport { report } => report,
            other => panic!("expected a clock report, got {other:?}"),
        }
    }

    #[test]
    fn the_clock_probes_only_held_peers_and_one_report_makes_it_ready() {
        let now = Instant::now();
        let later = |ms| now + Duration::from_millis(ms);
        let speaker = device("speaker");
        let idle = device("idle");
        let mut holder = FabricClockCore::default();
        let mut peer = FabricClockCore::default();
        holder.set_enabled(true, now);
        peer.set_enabled(true, now);
        let mut next = 0_u8;
        let mut nonce = || {
            next += 1;
            [next; 16]
        };
        assert!(
            holder
                .due_probes(|_| true, now, 1_000_000, &mut nonce)
                .is_empty(),
            "nothing is probed until something holds a peer"
        );

        holder.hold(speaker.clone(), now);
        assert_eq!(
            holder.state(&speaker, now, 1_000_000),
            ClockSyncState::Syncing
        );
        let probes = holder.due_probes(|_| true, now, 1_000_000, &mut nonce);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].0, speaker);
        assert!(!probes.iter().any(|(target, _)| target == &idle));
        assert!(
            holder
                .due_probes(|_| true, later(100), 1_100_000, &mut nonce)
                .is_empty(),
            "one round per cadence"
        );

        let (probe_nonce, sent_us) = probe_of(&probes[0].1);
        let report = report_of(
            peer.answer(probe_nonce, sent_us, 70_000_000, 70_000_200)
                .expect("an enabled peer answers"),
        );
        assert_eq!(report.probe_sent_us, 1_000_000);
        for forged in [
            ClockReport {
                probe_nonce: [0x7f; 16],
                ..report
            },
            ClockReport {
                probe_sent_us: 999_999,
                ..report
            },
            ClockReport {
                replied_us: 69_999_999,
                ..report
            },
        ] {
            assert!(!holder.accept(&speaker, forged, later(12), 1_012_000));
        }
        assert!(
            !holder.accept(&idle, report, later(12), 1_012_000),
            "a report from a peer nobody probed never counts"
        );
        assert!(holder.accept(&speaker, report, later(12), 1_012_000));
        assert!(
            !holder.accept(&speaker, report, later(13), 1_013_000),
            "each probe counts once"
        );
        assert_eq!(
            holder.state(&speaker, later(12), 1_012_000),
            ClockSyncState::Ready
        );
        let estimate = holder.estimate(&speaker, 1_012_000).expect("an estimate");
        // ((70_000_000 - 1_000_000) + (70_000_200 - 1_012_000)) / 2
        assert_eq!(estimate.offset_us(1_006_000), Some(68_994_100));

        holder.release(&speaker);
        assert!(
            holder.estimate(&speaker, 1_012_000).is_none(),
            "the last release forgets the estimate"
        );
        assert!(
            holder
                .due_probes(|_| true, later(1_000), 2_000_000, &mut nonce)
                .is_empty()
        );
    }

    #[test]
    fn an_unanswered_peer_fails_with_a_reason_after_the_deadline() {
        let now = Instant::now();
        let speaker = device("speaker");
        let mut core = FabricClockCore::default();
        core.set_enabled(true, now);
        core.hold(speaker.clone(), now);
        core.hold(speaker.clone(), now);
        assert_eq!(core.due_probes(|_| true, now, 1, || [3; 16]).len(), 1);
        let deadline = now + CLOCK_SYNC_DEADLINE;
        assert_eq!(
            core.state(
                &speaker,
                deadline.checked_sub(Duration::from_millis(1)).unwrap(),
                2
            ),
            ClockSyncState::Syncing
        );
        assert_eq!(
            core.state(&speaker, deadline, 3),
            ClockSyncState::Failed(ClockSyncFailure::NoReply)
        );
        core.release(&speaker);
        assert!(core.is_held(&speaker), "one hold is still alive");
        core.release(&speaker);
        assert!(!core.is_held(&speaker));
    }

    #[test]
    fn media_off_stops_probing_answering_and_forgets_every_estimate() {
        let now = Instant::now();
        let speaker = device("speaker");
        let mut core = FabricClockCore::default();
        core.set_enabled(true, now);
        core.hold(speaker.clone(), now);
        let probes = core.due_probes(|_| true, now, 1_000_000, || [5; 16]);
        let (nonce, sent_us) = probe_of(&probes[0].1);
        let report = ClockReport {
            probe_nonce: nonce,
            probe_sent_us: sent_us,
            received_us: 2_000_000,
            replied_us: 2_000_100,
        };
        assert!(core.accept(&speaker, report, now, 1_000_400));
        assert_eq!(core.state(&speaker, now, 1_000_400), ClockSyncState::Ready);

        let off = now + Duration::from_secs(1);
        core.set_enabled(false, off);
        assert!(core.estimate(&speaker, 1_000_400).is_none());
        assert_eq!(
            core.state(&speaker, off, 1_000_400),
            ClockSyncState::Failed(ClockSyncFailure::MediaDisabled)
        );
        assert!(
            core.due_probes(|_| true, off, 2_000_000, || [6; 16])
                .is_empty()
        );
        assert!(core.answer([7; 16], 1, 2, 3).is_none());

        let on = off + Duration::from_secs(60);
        core.set_enabled(true, on);
        assert_eq!(
            core.state(&speaker, on, 3_000_000),
            ClockSyncState::Syncing,
            "turning Media back on restarts the deadline"
        );
    }
}
