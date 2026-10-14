//! Choosing which of a person's certified devices runs a compute job.
//!
//! [`place_compute_job`] ranks [`ServerAdvertisement`]s and names one
//! worker, with the reason it won and why every other candidate did not.
//! The choice is a routing preference, never authority:
//!
//! - eligibility is the one rule admission enforces
//!   ([`ServerAdvertisement::compute_ineligibility`]), so placement never
//!   names a worker the relay would refuse the job for;
//! - the job that follows still binds that exact worker, is re-admitted by
//!   the relay against the worker's live advertisement and runs only as a
//!   signed package entrypoint under the worker's launcher sandbox;
//! - advertised power, load and latency are hints a lying peer can use only
//!   to attract work it was already certified and sandboxed to run.
//!
//! Ranking, best first: operator [`priority`](ServerAdvertisement::priority)
//! (higher wins, as configured preference precedes load balancing), then
//! power (external, charging, unknown, battery), then load as a fraction of
//! capacity, then advertised latency, then the device key so equal
//! candidates always resolve the same way.

use serde::Serialize;

use crate::mesh::{ComputeAccelerator, DeviceKey, PowerAvailability, ServerAdvertisement};

/// What a compute job needs from the device that runs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ComputeRequirements<'a> {
    /// The device asking. It is never its own remote worker: a job with no
    /// other eligible worker runs where it was asked, or not at all.
    pub requester: DeviceKey,
    /// Workspace whose content and permissions the job runs under.
    pub workspace_id: u64,
    /// Installed signed package the entrypoint belongs to.
    pub app_id: &'a str,
    /// Whether the job needs a sandbox-approved GPU.
    pub gpu: bool,
}

/// Why an advertised device cannot take a compute job now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ineligible {
    /// The device is the requester itself.
    Requester,
    /// Its liveness is not fresh enough to route new work.
    Offline,
    /// Its role does not serve other devices.
    NotAServer,
    /// It does not offer the compute service.
    NoComputeService,
    /// It does not host the job's workspace.
    WorkspaceNotHosted,
    /// It cannot run the job's app.
    AppNotOffered,
    /// The job needs a GPU it does not advertise.
    NoGpu,
    /// Every assignment slot it advertises is taken, or it has none.
    AtCapacity,
    /// Its person asked the mesh to avoid discretionary work.
    EnergySaver,
    /// Its battery is critically low.
    CriticalBattery,
}

impl Ineligible {
    /// Stable token for logs and action output.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Requester => "requester",
            Self::Offline => "offline",
            Self::NotAServer => "not_a_server",
            Self::NoComputeService => "no_compute_service",
            Self::WorkspaceNotHosted => "workspace_not_hosted",
            Self::AppNotOffered => "app_not_offered",
            Self::NoGpu => "no_gpu",
            Self::AtCapacity => "at_capacity",
            Self::EnergySaver => "energy_saver",
            Self::CriticalBattery => "critical_battery",
        }
    }
}

impl ServerAdvertisement {
    /// Why this device cannot take a job with `needs`, or `None` when it
    /// can. The single eligibility rule shared by placement and admission.
    #[must_use]
    pub fn compute_ineligibility(&self, needs: &ComputeRequirements<'_>) -> Option<Ineligible> {
        let reason = if self.device == needs.requester {
            Ineligible::Requester
        } else if !self.online {
            Ineligible::Offline
        } else if !self.role.can_serve() {
            Ineligible::NotAServer
        } else if !self.services.compute {
            Ineligible::NoComputeService
        } else if !self.workspaces.contains(&needs.workspace_id) {
            Ineligible::WorkspaceNotHosted
        } else if !self.apps.contains(needs.app_id) {
            Ineligible::AppNotOffered
        } else if needs.gpu && !self.accelerators.contains(&ComputeAccelerator::Gpu) {
            Ineligible::NoGpu
        } else if self.assignment_capacity == 0
            || self.active_assignments >= self.assignment_capacity
        {
            Ineligible::AtCapacity
        } else if self.energy_saver {
            Ineligible::EnergySaver
        } else if self.power.is_critically_constrained() {
            Ineligible::CriticalBattery
        } else {
            return None;
        };
        Some(reason)
    }

    fn placement_rank(&self) -> PlacementRank {
        let load_permille = u64::from(self.active_assignments)
            .saturating_mul(1_000)
            .checked_div(u64::from(self.assignment_capacity))
            .unwrap_or(u64::MAX);
        PlacementRank {
            priority: self.priority,
            power: PowerClass::of(self.power),
            load_permille: u32::try_from(load_permille).unwrap_or(u32::MAX),
            latency_ms: self.latency_ms,
        }
    }
}

/// Coarse power standing, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerClass {
    /// Stable external power.
    External,
    /// On external power and charging.
    Charging,
    /// No reliable observation.
    Unknown,
    /// Running from a battery above the critical threshold.
    Battery,
}

impl PowerClass {
    const fn of(power: PowerAvailability) -> Self {
        match power {
            PowerAvailability::External => Self::External,
            PowerAvailability::Charging(_) => Self::Charging,
            PowerAvailability::Unknown => Self::Unknown,
            PowerAvailability::Battery(_) => Self::Battery,
        }
    }
}

/// What an eligible worker was ranked by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PlacementRank {
    /// Operator preference; higher wins.
    pub priority: i32,
    /// Power standing; better wins.
    pub power: PowerClass,
    /// Active assignments per thousand slots; lower wins.
    pub load_permille: u32,
    /// Advertised round-trip latency; lower wins.
    pub latency_ms: u32,
}

impl PlacementRank {
    /// Sort key: smaller is better.
    fn key(&self, device: DeviceKey) -> (std::cmp::Reverse<i32>, PowerClass, u32, u32, DeviceKey) {
        (
            std::cmp::Reverse(self.priority),
            self.power,
            self.load_permille,
            self.latency_ms,
            device,
        )
    }
}

/// One advertised device and what placement made of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PlacementCandidate {
    /// Certified device public key.
    pub device: DeviceKey,
    /// Its rank when eligible, or why it is not.
    pub verdict: Result<PlacementRank, Ineligible>,
}

/// The worker a compute job should be sent to, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Placement {
    /// The chosen worker.
    pub worker: DeviceKey,
    /// What it won on.
    pub rank: PlacementRank,
    /// Every advertised device, best first among the eligible, then the
    /// ineligible by device key.
    pub considered: Vec<PlacementCandidate>,
}

/// No advertised device can take the job; each candidate says why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NoEligibleWorker {
    /// Every advertised device and why it was passed over.
    pub considered: Vec<PlacementCandidate>,
}

impl std::fmt::Display for NoEligibleWorker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.considered.is_empty() {
            return formatter.write_str("no compute worker is advertised");
        }
        formatter.write_str("no advertised compute worker can take the job")?;
        for candidate in &self.considered {
            if let Err(reason) = candidate.verdict {
                write!(
                    formatter,
                    "; {}: {}",
                    short_device(&candidate.device),
                    reason.token()
                )?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for NoEligibleWorker {}

impl std::fmt::Display for Placement {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let eligible = self
            .considered
            .iter()
            .filter(|candidate| candidate.verdict.is_ok())
            .count();
        write!(
            formatter,
            "{} (priority {}, power {:?}, load {}‰, latency {} ms) of {eligible} eligible among {} advertised",
            short_device(&self.worker),
            self.rank.priority,
            self.rank.power,
            self.rank.load_permille,
            self.rank.latency_ms,
            self.considered.len(),
        )
    }
}

/// The first eight hex characters of a device key, for log lines.
fn short_device(device: &DeviceKey) -> String {
    hex::encode(device.get(..4).unwrap_or_default())
}

/// Choose the best eligible worker for `needs` among `candidates`.
///
/// # Errors
///
/// Returns [`NoEligibleWorker`], listing every candidate's reason, when none
/// can take the job.
pub fn place_compute_job<'a>(
    needs: &ComputeRequirements<'_>,
    candidates: impl IntoIterator<Item = &'a ServerAdvertisement>,
) -> Result<Placement, NoEligibleWorker> {
    let mut considered = candidates
        .into_iter()
        .map(|candidate| PlacementCandidate {
            device: candidate.device,
            verdict: match candidate.compute_ineligibility(needs) {
                Some(reason) => Err(reason),
                None => Ok(candidate.placement_rank()),
            },
        })
        .collect::<Vec<_>>();
    considered.sort_by(|left, right| match (&left.verdict, &right.verdict) {
        (Ok(a), Ok(b)) => a.key(left.device).cmp(&b.key(right.device)),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => left.device.cmp(&right.device),
    });
    match considered.first().map(|best| (best.device, best.verdict)) {
        Some((worker, Ok(rank))) => Ok(Placement {
            worker,
            rank,
            considered,
        }),
        _ => Err(NoEligibleWorker { considered }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::mesh::{DeviceRole, ServiceCapabilities};

    const REQUESTER: DeviceKey = [9; 32];

    fn needs(gpu: bool) -> ComputeRequirements<'static> {
        ComputeRequirements {
            requester: REQUESTER,
            workspace_id: 7,
            app_id: "org.tilo.thumbnails",
            gpu,
        }
    }

    fn worker(seed: u8) -> ServerAdvertisement {
        ServerAdvertisement {
            device: [seed; 32],
            role: DeviceRole::AlwaysOnServer,
            services: ServiceCapabilities {
                compute: true,
                ..ServiceCapabilities::default()
            },
            workspaces: BTreeSet::from([7]),
            apps: BTreeSet::from(["org.tilo.thumbnails".to_owned()]),
            accelerators: BTreeSet::new(),
            priority: 0,
            latency_ms: 20,
            power: PowerAvailability::External,
            energy_saver: false,
            active_assignments: 0,
            assignment_capacity: 4,
            online: true,
        }
    }

    fn chosen(candidates: &[ServerAdvertisement], gpu: bool) -> DeviceKey {
        place_compute_job(&needs(gpu), candidates)
            .expect("an eligible worker")
            .worker
    }

    #[test]
    fn a_critically_constrained_worker_is_never_chosen() {
        let mut drained = worker(1);
        drained.power = PowerAvailability::Battery(15);
        drained.priority = 100;
        drained.latency_ms = 0;
        let mut saving = worker(2);
        saving.energy_saver = true;
        saving.priority = 100;

        let refused = place_compute_job(&needs(false), [&drained, &saving]).unwrap_err();
        let reasons = refused
            .considered
            .iter()
            .map(|candidate| (candidate.device[0], candidate.verdict))
            .collect::<Vec<_>>();
        assert_eq!(
            reasons,
            vec![
                (1, Err(Ineligible::CriticalBattery)),
                (2, Err(Ineligible::EnergySaver)),
            ]
        );
        assert_eq!(
            refused.to_string(),
            "no advertised compute worker can take the job; 01010101: critical_battery; 02020202: energy_saver"
        );

        // A loaded, distant desktop still beats the drained laptop.
        let mut desktop = worker(3);
        desktop.active_assignments = 3;
        desktop.latency_ms = 400;
        assert_eq!(chosen(&[drained, saving, desktop], false), [3; 32]);
    }

    #[test]
    fn the_lowest_cost_healthy_worker_is_chosen() {
        let busy = {
            let mut busy = worker(1);
            busy.active_assignments = 3;
            busy
        };
        let idle_far = {
            let mut far = worker(2);
            far.latency_ms = 90;
            far
        };
        let idle_near = worker(3);
        let on_battery = {
            let mut battery = worker(4);
            battery.power = PowerAvailability::Battery(80);
            battery.latency_ms = 1;
            battery
        };
        let candidates = [busy, idle_far, idle_near.clone(), on_battery];
        let placement = place_compute_job(&needs(false), &candidates).unwrap();
        assert_eq!(placement.worker, [3; 32]);
        assert_eq!(
            placement
                .considered
                .iter()
                .map(|candidate| candidate.device[0])
                .collect::<Vec<_>>(),
            vec![3, 2, 1, 4],
            "power, then load, then latency"
        );
        assert_eq!(
            placement.to_string(),
            "03030303 (priority 0, power External, load 0‰, latency 20 ms) of 4 eligible among 4 advertised"
        );

        // Operator preference comes before everything else.
        let mut preferred = worker(5);
        preferred.priority = 1;
        preferred.power = PowerAvailability::Charging(40);
        preferred.active_assignments = 2;
        assert_eq!(chosen(&[idle_near, preferred], false), [5; 32]);
    }

    #[test]
    fn equal_workers_resolve_the_same_way_whatever_their_order() {
        let (first, second) = (worker(4), worker(2));
        assert_eq!(chosen(&[first.clone(), second.clone()], false), [2; 32]);
        assert_eq!(chosen(&[second, first], false), [2; 32]);
    }

    #[test]
    fn each_admission_rule_passes_a_worker_over_with_its_reason() {
        type Change = fn(&mut ServerAdvertisement);
        let cases: [(Change, Ineligible); 10] = [
            (|w| w.device = REQUESTER, Ineligible::Requester),
            (|w| w.online = false, Ineligible::Offline),
            (|w| w.role = DeviceRole::Interactive, Ineligible::NotAServer),
            (|w| w.services.compute = false, Ineligible::NoComputeService),
            (|w| w.workspaces.clear(), Ineligible::WorkspaceNotHosted),
            (|w| w.apps.clear(), Ineligible::AppNotOffered),
            (|w| w.active_assignments = 4, Ineligible::AtCapacity),
            (|w| w.assignment_capacity = 0, Ineligible::AtCapacity),
            (|w| w.energy_saver = true, Ineligible::EnergySaver),
            (
                |w| w.power = PowerAvailability::Battery(3),
                Ineligible::CriticalBattery,
            ),
        ];
        for (change, reason) in cases {
            let mut candidate = worker(1);
            change(&mut candidate);
            assert_eq!(
                candidate.compute_ineligibility(&needs(false)),
                Some(reason),
                "{reason:?}"
            );
        }

        let cpu_only = worker(1);
        assert_eq!(
            cpu_only.compute_ineligibility(&needs(true)),
            Some(Ineligible::NoGpu)
        );
        let mut gpu = worker(2);
        gpu.accelerators.insert(ComputeAccelerator::Gpu);
        assert_eq!(chosen(&[cpu_only, gpu], true), [2; 32]);
    }

    #[test]
    fn nothing_advertised_says_so() {
        let refused = place_compute_job(&needs(false), []).unwrap_err();
        assert!(refused.considered.is_empty());
        assert_eq!(refused.to_string(), "no compute worker is advertised");
    }
}
