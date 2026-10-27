//! Certified-device spatial calibration and bounded health presentation.
//!
//! Sensor observations are evidence about geometry, never identity. A proposal
//! is emitted only after the observed peer resolves to an already certified
//! personal device with a live display advertisement. Proposals carry their
//! previous placement and are never applied implicitly.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::shell::device_fabric::{DisplayEdge, DisplayRect};
use crate::shell::personal_continuity::DeviceId;

pub const MAX_CALIBRATION_PROPOSALS: usize = 16;
pub const MAX_EVIDENCE_AGE_MS: u64 = 5_000;
pub const MIN_CONFIDENCE_MILLI: u16 = 650;
pub const MAX_RANGE_MM: u32 = 3_000;
pub const HIGH_LATENCY_MS: u32 = 120;
pub const STALE_HEALTH_MS: u64 = 15_000;
pub const MAX_ACTIVE_ROUTES: u8 = 16;
pub const HEALTH_PROBE_INTERVAL: Duration = Duration::from_secs(10);
pub const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Probes awaiting a report per peer, so a round trip slower than the
/// probe cadence still completes.
const MAX_OUTSTANDING_PROBES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpatialEvidenceKind {
    UltraWideband,
    BluetoothDirectionFinding,
    NearUltrasound,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpatialEvidence {
    pub peer: DeviceId,
    pub edge: DisplayEdge,
    pub range_mm: u32,
    pub confidence_milli: u16,
    pub observed_unix_ms: u64,
    pub kind: SpatialEvidenceKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalibrationDevice {
    pub id: DeviceId,
    pub certified: bool,
    pub online: bool,
    pub display: bool,
    pub rect: Option<DisplayRect>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalibrationProposal {
    pub peer: DeviceId,
    pub edge: DisplayEdge,
    pub previous: DisplayRect,
    pub proposed: DisplayRect,
    pub evidence_kind: SpatialEvidenceKind,
    pub confidence_milli: u16,
    pub expires_unix_ms: u64,
}

/// Bounded compositor-owned sensor observations and reversible apply history.
///
/// Hardware adapters may record an observation only after resolving it to an
/// already certified peer. `propose_layout` deliberately re-checks that
/// binding against the current fabric snapshot so proximity can never enroll
/// or resurrect a device.
#[derive(Default)]
pub struct SpatialCalibrationState {
    evidence: VecDeque<SpatialEvidence>,
    previous: BTreeMap<DeviceId, DisplayRect>,
}

impl SpatialCalibrationState {
    pub fn record_sensor_evidence(&mut self, evidence: SpatialEvidence) -> bool {
        if evidence.peer.as_str().is_empty()
            || evidence.range_mm > MAX_RANGE_MM
            || evidence.confidence_milli > 1_000
        {
            return false;
        }
        if self.evidence.len() >= MAX_CALIBRATION_PROPOSALS {
            self.evidence.pop_front();
        }
        self.evidence.push_back(evidence);
        true
    }

    #[must_use]
    pub fn proposals(
        &self,
        local: &DeviceId,
        devices: &[CalibrationDevice],
        now_unix_ms: u64,
    ) -> Vec<CalibrationProposal> {
        let evidence = self.evidence.iter().cloned().collect::<Vec<_>>();
        propose_layout(local, devices, &evidence, now_unix_ms)
    }

    pub fn remember_apply(&mut self, proposal: &CalibrationProposal) {
        self.previous
            .insert(proposal.peer.clone(), proposal.previous);
        self.evidence.retain(|item| item.peer != proposal.peer);
    }

    pub fn dismiss(&mut self, peer: &DeviceId) {
        self.evidence.retain(|item| &item.peer != peer);
    }

    #[must_use]
    pub fn revert_rect(&self, peer: &DeviceId) -> Option<DisplayRect> {
        self.previous.get(peer).copied()
    }

    pub fn finish_revert(&mut self, peer: &DeviceId) {
        self.previous.remove(peer);
    }

    #[must_use]
    pub fn revertible_peers(&self) -> Vec<DeviceId> {
        self.previous.keys().cloned().collect()
    }
}

fn adjacent_rect(local: DisplayRect, target: DisplayRect, edge: DisplayEdge) -> DisplayRect {
    let width = i32::try_from(target.width).unwrap_or(i32::MAX);
    let height = i32::try_from(target.height).unwrap_or(i32::MAX);
    match edge {
        DisplayEdge::Left => DisplayRect {
            x: local.x.saturating_sub(width),
            y: local.y,
            width: target.width,
            height: target.height,
        },
        DisplayEdge::Right => DisplayRect {
            x: local.x.saturating_add_unsigned(local.width),
            y: local.y,
            width: target.width,
            height: target.height,
        },
        DisplayEdge::Top => DisplayRect {
            x: local.x,
            y: local.y.saturating_sub(height),
            width: target.width,
            height: target.height,
        },
        DisplayEdge::Bottom => DisplayRect {
            x: local.x,
            y: local.y.saturating_add_unsigned(local.height),
            width: target.width,
            height: target.height,
        },
    }
}

/// Turn fresh local sensor evidence into reversible placement proposals.
///
/// Uncertified, offline, stale, weak, non-display, out-of-range, overlapping,
/// and self observations are discarded. At most one strongest proposal is
/// returned per exact peer.
#[must_use]
pub fn propose_layout(
    local: &DeviceId,
    devices: &[CalibrationDevice],
    evidence: &[SpatialEvidence],
    now_unix_ms: u64,
) -> Vec<CalibrationProposal> {
    let Some(local_rect) = devices
        .iter()
        .find(|device| &device.id == local && device.certified && device.display)
        .and_then(|device| device.rect)
    else {
        return Vec::new();
    };
    let mut ordered = evidence.to_vec();
    ordered.sort_by_key(|item| std::cmp::Reverse(item.confidence_milli));
    let mut proposals = Vec::new();
    for item in ordered {
        if proposals.len() >= MAX_CALIBRATION_PROPOSALS
            || &item.peer == local
            || item.range_mm > MAX_RANGE_MM
            || item.confidence_milli < MIN_CONFIDENCE_MILLI
            || item.observed_unix_ms > now_unix_ms
            || now_unix_ms.saturating_sub(item.observed_unix_ms) > MAX_EVIDENCE_AGE_MS
            || proposals
                .iter()
                .any(|proposal: &CalibrationProposal| proposal.peer == item.peer)
        {
            continue;
        }
        let Some(target) = devices.iter().find(|device| {
            device.id == item.peer
                && device.certified
                && device.online
                && device.display
                && device.rect.is_some()
        }) else {
            continue;
        };
        let previous = target.rect.unwrap_or(local_rect);
        let proposed = adjacent_rect(local_rect, previous, item.edge);
        let blocked = devices.iter().any(|device| {
            &device.id != local
                && device.id != item.peer
                && device.rect.is_some_and(|rect| proposed.intersects(rect))
        }) || proposals
            .iter()
            .any(|proposal| proposed.intersects(proposal.proposed));
        if blocked {
            continue;
        }
        proposals.push(CalibrationProposal {
            peer: item.peer,
            edge: item.edge,
            previous,
            proposed,
            evidence_kind: item.kind,
            confidence_milli: item.confidence_milli,
            expires_unix_ms: item.observed_unix_ms.saturating_add(MAX_EVIDENCE_AGE_MS),
        });
    }
    proposals
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DevicePower {
    #[default]
    Unknown,
    External,
    Charging(u8),
    Battery(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceReachability {
    Online,
    Stale,
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceDegradedReason {
    Offline,
    StalePresence,
    HighLatency,
    LowBattery,
    EnergySaver,
    RoutePressure,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceHealthObservation {
    pub peer: DeviceId,
    pub certified: bool,
    pub online: bool,
    pub observed_unix_ms: u64,
    pub latency_ms: Option<u32>,
    pub power: DevicePower,
    pub energy_saver: bool,
    pub active_routes: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceHealthView {
    pub peer: DeviceId,
    pub reachability: DeviceReachability,
    pub latency_ms: Option<u32>,
    pub power: DevicePower,
    pub energy_saver: bool,
    pub active_routes: u8,
    pub degraded: Vec<DeviceDegradedReason>,
}

/// Reply to one health probe: the reporter's power state, bound to the
/// probe's nonce. Clock timestamps travel only on the Media-classified
/// [`crate::shell::fabric_clock::ClockReport`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceHealthReport {
    pub probe_nonce: [u8; 16],
    pub power: DevicePower,
    pub energy_saver: bool,
}

impl DeviceHealthReport {
    #[must_use]
    pub fn valid(self) -> bool {
        self.probe_nonce != [0; 16]
            && match self.power {
                DevicePower::Charging(percent) | DevicePower::Battery(percent) => percent <= 100,
                DevicePower::Unknown | DevicePower::External => true,
            }
    }
}

#[derive(Clone, Debug)]
struct PendingHealthProbe {
    nonce: [u8; 16],
    sent: Instant,
}

#[derive(Clone, Debug)]
struct MeasuredHealth {
    measured: Instant,
    latency_ms: u32,
    power: DevicePower,
    energy_saver: bool,
}

/// Exact-peer probe state. Latency is measured locally and remote reports are
/// accepted only as replies to an outstanding random nonce for that same peer.
#[derive(Default)]
pub struct DeviceHealthTracker {
    pending: BTreeMap<DeviceId, VecDeque<PendingHealthProbe>>,
    measured: BTreeMap<DeviceId, MeasuredHealth>,
}

impl DeviceHealthTracker {
    /// Record a probe sent at `now`.
    pub fn begin_probe(&mut self, peer: DeviceId, nonce: [u8; 16], now: Instant) -> bool {
        self.prune(now);
        if nonce == [0; 16]
            || (!self.pending.contains_key(&peer)
                && self.pending.len() >= MAX_CALIBRATION_PROPOSALS)
        {
            return false;
        }
        let outstanding = self.pending.entry(peer).or_default();
        if outstanding.len() >= MAX_OUTSTANDING_PROBES {
            outstanding.pop_front();
        }
        outstanding.push_back(PendingHealthProbe { nonce, sent: now });
        true
    }

    /// Drop one outstanding probe that never left this device.
    pub fn abandon_probe(&mut self, peer: &DeviceId, nonce: [u8; 16]) {
        if let Some(outstanding) = self.pending.get_mut(peer) {
            outstanding.retain(|probe| probe.nonce != nonce);
            if outstanding.is_empty() {
                self.pending.remove(peer);
            }
        }
    }

    /// Accept a report that arrived at `now`.
    pub fn accept_report(
        &mut self,
        peer: &DeviceId,
        report: DeviceHealthReport,
        now: Instant,
    ) -> bool {
        self.prune(now);
        if !report.valid() {
            return false;
        }
        let Some(outstanding) = self.pending.get_mut(peer) else {
            return false;
        };
        let Some(index) = outstanding
            .iter()
            .position(|probe| probe.nonce == report.probe_nonce && now >= probe.sent)
        else {
            return false;
        };
        let Some(pending) = outstanding.remove(index) else {
            return false;
        };
        if outstanding.is_empty() {
            self.pending.remove(peer);
        }
        let latency_ms =
            u32::try_from(now.duration_since(pending.sent).as_millis()).unwrap_or(u32::MAX);
        self.measured.insert(
            peer.clone(),
            MeasuredHealth {
                measured: now,
                latency_ms,
                power: report.power,
                energy_saver: report.energy_saver,
            },
        );
        true
    }

    pub fn forget_peer(&mut self, peer: &DeviceId) {
        self.pending.remove(peer);
        self.measured.remove(peer);
    }

    pub fn retain_peers(&mut self, mut keep: impl FnMut(&DeviceId) -> bool) {
        self.pending.retain(|peer, _| keep(peer));
        self.measured.retain(|peer, _| keep(peer));
    }

    pub fn clear(&mut self) {
        self.pending.clear();
        self.measured.clear();
    }

    #[must_use]
    pub fn observation(
        &self,
        peer: &DeviceId,
        certified: bool,
        online: bool,
        active_routes: u8,
        now: Instant,
        now_unix_ms: u64,
        presence_observed_unix_ms: u64,
    ) -> DeviceHealthObservation {
        let measured = self.measured.get(peer);
        let age_ms = measured
            .and_then(|item| now.checked_duration_since(item.measured))
            .and_then(|age| u64::try_from(age.as_millis()).ok())
            .unwrap_or(STALE_HEALTH_MS.saturating_add(1));
        DeviceHealthObservation {
            peer: peer.clone(),
            certified,
            online,
            observed_unix_ms: measured.map_or(presence_observed_unix_ms, |_| {
                now_unix_ms.saturating_sub(age_ms)
            }),
            latency_ms: measured.map(|item| item.latency_ms),
            power: measured.map_or(DevicePower::Unknown, |item| item.power),
            energy_saver: measured.is_some_and(|item| item.energy_saver),
            active_routes,
        }
    }

    fn prune(&mut self, now: Instant) {
        self.pending.retain(|_, outstanding| {
            outstanding.retain(|probe| {
                now.checked_duration_since(probe.sent)
                    .is_some_and(|age| age <= HEALTH_PROBE_TIMEOUT)
            });
            !outstanding.is_empty()
        });
        self.measured.retain(|_, item| {
            now.checked_duration_since(item.measured)
                .is_some_and(|age| age <= HEALTH_PROBE_TIMEOUT.saturating_mul(4))
        });
    }
}

/// Build bounded user-facing health for certified devices only.
#[must_use]
pub fn summarize_health(
    observations: impl IntoIterator<Item = DeviceHealthObservation>,
    now_unix_ms: u64,
) -> Vec<DeviceHealthView> {
    let mut newest = BTreeMap::<DeviceId, DeviceHealthObservation>::new();
    for item in observations {
        if item.certified
            && item.observed_unix_ms <= now_unix_ms
            && newest
                .get(&item.peer)
                .is_none_or(|existing| existing.observed_unix_ms < item.observed_unix_ms)
        {
            newest.insert(item.peer.clone(), item);
        }
    }
    newest
        .into_values()
        .take(MAX_CALIBRATION_PROPOSALS)
        .map(|item| {
            let stale = now_unix_ms.saturating_sub(item.observed_unix_ms) > STALE_HEALTH_MS;
            let reachability = if !item.online {
                DeviceReachability::Offline
            } else if stale {
                DeviceReachability::Stale
            } else {
                DeviceReachability::Online
            };
            let mut degraded = Vec::new();
            if !item.online {
                degraded.push(DeviceDegradedReason::Offline);
            } else if stale {
                degraded.push(DeviceDegradedReason::StalePresence);
            }
            if item
                .latency_ms
                .is_some_and(|latency| latency > HIGH_LATENCY_MS)
            {
                degraded.push(DeviceDegradedReason::HighLatency);
            }
            if matches!(item.power, DevicePower::Battery(percent) if percent <= 15) {
                degraded.push(DeviceDegradedReason::LowBattery);
            }
            if item.energy_saver {
                degraded.push(DeviceDegradedReason::EnergySaver);
            }
            if item.active_routes >= MAX_ACTIVE_ROUTES {
                degraded.push(DeviceDegradedReason::RoutePressure);
            }
            DeviceHealthView {
                peer: item.peer,
                reachability,
                latency_ms: item.latency_ms,
                power: item.power,
                energy_saver: item.energy_saver,
                active_routes: item.active_routes.min(MAX_ACTIVE_ROUTES),
                degraded,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> DeviceId {
        DeviceId::new(value).unwrap()
    }

    fn display(value: &str, x: i32, certified: bool) -> CalibrationDevice {
        CalibrationDevice {
            id: id(value),
            certified,
            online: true,
            display: true,
            rect: Some(DisplayRect {
                x,
                y: 0,
                width: 100,
                height: 80,
            }),
        }
    }

    #[test]
    fn calibration_proposals_are_fresh_adjacent_and_applied_only_explicitly() {
        {
            let local = id("local");
            let evidence = |observed_unix_ms| SpatialEvidence {
                peer: id("nearby"),
                edge: DisplayEdge::Right,
                range_mm: 100,
                confidence_milli: 990,
                observed_unix_ms,
                kind: SpatialEvidenceKind::UltraWideband,
            };
            let devices = vec![display("local", 0, true), display("nearby", 500, false)];
            assert!(propose_layout(&local, &devices, &[evidence(10_000)], 10_001).is_empty());

            let devices = vec![display("local", 0, true), display("nearby", 500, true)];
            assert!(propose_layout(&local, &devices, &[evidence(1)], 10_001).is_empty());
            assert!(
                propose_layout(&local, &devices, &[evidence(10_002)], 10_001).is_empty(),
                "future local observations must not remain fresh through saturation"
            );
        }

        {
            let local = id("local");
            let devices = vec![display("local", 0, true), display("tablet", 800, true)];
            let proposals = propose_layout(
                &local,
                &devices,
                &[SpatialEvidence {
                    peer: id("tablet"),
                    edge: DisplayEdge::Right,
                    range_mm: 220,
                    confidence_milli: 900,
                    observed_unix_ms: 9_000,
                    kind: SpatialEvidenceKind::BluetoothDirectionFinding,
                }],
                10_000,
            );
            assert_eq!(proposals.len(), 1);
            assert_eq!(proposals[0].previous.x, 800);
            assert_eq!(proposals[0].proposed.x, 100);
            assert_eq!(devices[1].rect.unwrap().x, 800);
        }

        {
            let local = id("local");
            let tablet = id("tablet");
            let devices = vec![display("local", 0, true), display("tablet", 800, true)];
            let mut state = SpatialCalibrationState::default();
            assert!(state.record_sensor_evidence(SpatialEvidence {
                peer: tablet.clone(),
                edge: DisplayEdge::Right,
                range_mm: 120,
                confidence_milli: 880,
                observed_unix_ms: 10_000,
                kind: SpatialEvidenceKind::UltraWideband,
            }));
            let proposal = state.proposals(&local, &devices, 10_001).remove(0);
            assert_eq!(devices[1].rect, Some(proposal.previous));
            state.remember_apply(&proposal);
            assert!(state.proposals(&local, &devices, 10_001).is_empty());
            assert_eq!(state.revert_rect(&tablet), Some(proposal.previous));
            state.finish_revert(&tablet);
            assert_eq!(state.revert_rect(&tablet), None);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn calibration_health_uses_exact_certified_peers_and_one_shot_probes() {
        {
            let views = summarize_health(
                [
                    DeviceHealthObservation {
                        peer: id("certified"),
                        certified: true,
                        online: true,
                        observed_unix_ms: 1,
                        latency_ms: Some(180),
                        power: DevicePower::Battery(9),
                        energy_saver: true,
                        active_routes: u8::MAX,
                    },
                    DeviceHealthObservation {
                        peer: id("stranger"),
                        certified: false,
                        online: true,
                        observed_unix_ms: 20_000,
                        latency_ms: None,
                        power: DevicePower::Unknown,
                        energy_saver: false,
                        active_routes: 0,
                    },
                ],
                20_000,
            );
            assert_eq!(views.len(), 1);
            assert_eq!(views[0].reachability, DeviceReachability::Stale);
            assert_eq!(views[0].active_routes, MAX_ACTIVE_ROUTES);
            assert_eq!(
                views[0].degraded,
                vec![
                    DeviceDegradedReason::StalePresence,
                    DeviceDegradedReason::HighLatency,
                    DeviceDegradedReason::LowBattery,
                    DeviceDegradedReason::EnergySaver,
                    DeviceDegradedReason::RoutePressure,
                ]
            );
        }

        {
            let base = DeviceHealthObservation {
                peer: id("tablet"),
                certified: true,
                online: false,
                observed_unix_ms: 10,
                latency_ms: Some(900),
                power: DevicePower::Battery(2),
                energy_saver: true,
                active_routes: MAX_ACTIVE_ROUTES,
            };
            let fresh = DeviceHealthObservation {
                online: true,
                observed_unix_ms: 20,
                latency_ms: Some(12),
                power: DevicePower::Charging(80),
                energy_saver: false,
                active_routes: 1,
                ..base.clone()
            };
            let views = summarize_health([fresh, base], 21);
            assert_eq!(views.len(), 1);
            assert_eq!(views[0].reachability, DeviceReachability::Online);
            assert!(views[0].degraded.is_empty());
        }

        {
            let now = Instant::now();
            let peer = id("tablet");
            let other = id("phone");
            let mut tracker = DeviceHealthTracker::default();
            assert!(tracker.begin_probe(peer.clone(), [4; 16], now));
            let report = DeviceHealthReport {
                probe_nonce: [4; 16],
                power: DevicePower::Battery(64),
                energy_saver: true,
            };
            let later = |ms| now + Duration::from_millis(ms);
            assert!(!tracker.accept_report(&other, report, later(8)));
            for forged in [
                DeviceHealthReport {
                    probe_nonce: [5; 16],
                    ..report
                },
                DeviceHealthReport {
                    power: DevicePower::Battery(101),
                    ..report
                },
            ] {
                assert!(!tracker.accept_report(&peer, forged, later(8)));
            }
            assert!(tracker.accept_report(&peer, report, later(12)));
            assert!(!tracker.accept_report(&peer, report, later(13)));
            let observation = tracker.observation(
                &peer,
                true,
                true,
                2,
                now + Duration::from_millis(12),
                20_000,
                20_000,
            );
            assert_eq!(observation.latency_ms, Some(12));
            assert_eq!(observation.power, DevicePower::Battery(64));
            assert!(observation.energy_saver);
        }

        {
            let now = Instant::now();
            let peer = id("speaker");
            let mut tracker = DeviceHealthTracker::default();
            let report = |nonce: u8| DeviceHealthReport {
                probe_nonce: [nonce; 16],
                power: DevicePower::External,
                energy_saver: false,
            };
            for (step, nonce) in [1_u8, 2, 3, 4, 5].into_iter().enumerate() {
                let offset = u64::try_from(step).unwrap() * 250;
                assert!(tracker.begin_probe(
                    peer.clone(),
                    [nonce; 16],
                    now + Duration::from_millis(offset),
                ));
            }
            let arrival = now + Duration::from_millis(1_100);
            // Only the four newest probes stay outstanding.
            assert!(!tracker.accept_report(&peer, report(1), arrival));
            assert!(tracker.accept_report(&peer, report(3), arrival));
            assert!(tracker.accept_report(&peer, report(2), arrival));
            tracker.abandon_probe(&peer, [4; 16]);
            assert!(!tracker.accept_report(&peer, report(4), arrival));
            assert!(tracker.accept_report(&peer, report(5), arrival));
        }
    }
}
