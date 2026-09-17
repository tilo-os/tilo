//! Certified-device spatial calibration and bounded health presentation.
//!
//! Sensor observations are evidence about geometry, never identity. A proposal
//! is emitted only after the observed peer resolves to an already certified
//! personal device with a live display advertisement. Proposals carry their
//! previous placement and are never applied implicitly.

use std::collections::BTreeMap;

use crate::shell::device_fabric::{DisplayEdge, DisplayRect};
use crate::shell::personal_continuity::DeviceId;

pub const MAX_CALIBRATION_PROPOSALS: usize = 16;
pub const MAX_EVIDENCE_AGE_MS: u64 = 5_000;
pub const MIN_CONFIDENCE_MILLI: u16 = 650;
pub const MAX_RANGE_MM: u32 = 3_000;
pub const HIGH_LATENCY_MS: u32 = 120;
pub const STALE_HEALTH_MS: u64 = 15_000;
pub const MAX_ACTIVE_ROUTES: u8 = 16;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    fn proximity_never_bootstraps_identity_or_bypasses_freshness() {
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

    #[test]
    fn proposal_is_adjacent_reversible_and_not_applied() {
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

    #[test]
    fn health_explains_each_degraded_dimension_and_excludes_uncertified_peers() {
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

    #[test]
    fn health_uses_the_newest_local_observation_per_exact_peer() {
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
}
