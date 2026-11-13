//! The work a device takes from other devices: what, from whom, and when.
//!
//! A device runs its own person's work at the machine whoever set it up. Taking
//! work is a separate, personal setting: which kinds it takes, whose it takes,
//! and the states it takes them in. Nothing else on a device decides it.

use std::collections::BTreeSet;

use neighbour_wire::offer::{Answer, Decline, Power, Rank};
use serde::{Deserialize, Serialize};

use crate::WorkspaceId;

/// The kinds of work a device can take from another device.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each kind is taken or not on its own, and a person sets them one by one"
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkKinds {
    /// Run an app's action over a workspace's records, away from the device that asked.
    pub actions: bool,
    /// Keep the search indexes, previews and other derived data of those records up to date.
    pub indexing: bool,
    /// Run the model calls of the workspace's agents.
    pub model_calls: bool,
    /// Run builds and other long jobs.
    pub builds: bool,
}

impl WorkKinds {
    /// Every kind.
    pub const ALL: Self = Self {
        actions: true,
        indexing: true,
        model_calls: true,
        builds: true,
    };

    /// No kind: the device runs nothing for anyone else.
    pub const NONE: Self = Self {
        actions: false,
        indexing: false,
        model_calls: false,
        builds: false,
    };

    /// Whether it takes no kind at all.
    #[must_use]
    pub const fn none(self) -> bool {
        !self.actions && !self.indexing && !self.model_calls && !self.builds
    }
}

impl Default for WorkKinds {
    fn default() -> Self {
        Self::ALL
    }
}

/// Whose work a device takes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkFrom {
    /// Nobody else's.
    #[default]
    Nobody,
    /// Its own person's devices.
    OwnDevices,
    /// The members of the workspaces it names.
    Workspaces(BTreeSet<WorkspaceId>),
    /// Anyone whose workspace it holds.
    Anyone,
}

impl WorkFrom {
    /// Whether a device of the person `person`, in `workspace`, may hand work to a device of
    /// `own_person` that takes work this way.
    #[must_use]
    pub fn admits(
        &self,
        person: [u8; 32],
        own_person: [u8; 32],
        workspace: Option<WorkspaceId>,
    ) -> bool {
        match self {
            Self::Nobody => false,
            Self::OwnDevices => person == own_person,
            Self::Workspaces(named) => {
                person == own_person || workspace.is_some_and(|id| named.contains(&id))
            }
            Self::Anyone => true,
        }
    }
}

/// The states a device takes work in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkWhen {
    /// Take nothing while the machine runs on its battery.
    pub charging_only: bool,
    /// Leave at least this share of the processors free for the people at the machine.
    pub free_cpu_percent: u8,
    /// Leave at least this share of memory free for the people at the machine.
    pub free_memory_percent: u8,
}

impl WorkWhen {
    /// Takes work whatever the machine is doing: what a device that says nothing more about its
    /// states does.
    #[must_use]
    pub const fn always() -> Self {
        Self {
            charging_only: false,
            free_cpu_percent: 0,
            free_memory_percent: 0,
        }
    }
}

impl Default for WorkWhen {
    fn default() -> Self {
        Self::always()
    }
}

/// What work a device takes from other devices.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkSetting {
    /// The kinds it takes.
    pub kinds: WorkKinds,
    /// Whose it takes.
    pub from: WorkFrom,
    /// The states it takes them in.
    pub when: WorkWhen,
}

impl WorkSetting {
    /// No work for anyone else: what a device runs with before its person says otherwise.
    #[must_use]
    pub fn none() -> Self {
        Self {
            kinds: WorkKinds::NONE,
            from: WorkFrom::Nobody,
            when: WorkWhen::default(),
        }
    }

    /// Takes every kind of its person's work: what a device of the person's takes when it is told
    /// nothing else.
    #[must_use]
    pub fn own_person() -> Self {
        Self {
            from: WorkFrom::OwnDevices,
            ..Self::default()
        }
    }

    /// Whether it takes no work at all.
    #[must_use]
    pub fn takes_nothing(&self) -> bool {
        self.kinds.none() || self.from == WorkFrom::Nobody
    }

    /// What this device answers an offer with: `machine` is the state it is in now, `app_installed`
    /// whether it can run the app the work belongs to, `free_slots` how many more jobs it can start
    /// for a device that keeps count, and `holds` which of the work's inputs it already keeps.
    ///
    /// The reason a device declines is the first thing its setting says no about, so a person
    /// reading an offer round sees the limit they set.
    #[must_use]
    pub fn answer(
        &self,
        machine: Machine,
        app_installed: bool,
        free_slots: Option<u32>,
        holds: Vec<[u8; 32]>,
    ) -> Answer {
        let rank = Rank {
            power: machine.power,
            load_permille: machine.busy_permille,
            // The device that asked measures the round trip itself.
            latency_ms: 0,
        };
        let decline = if self.takes_nothing() || !self.kinds.actions {
            Decline::NotTakingWork
        } else if !app_installed {
            Decline::AppNotInstalled
        } else if free_slots == Some(0) {
            Decline::AtCapacity
        } else if self.when.charging_only && machine.power == Power::Battery {
            Decline::OnBattery
        } else if machine.free_cpu_percent() < u32::from(self.when.free_cpu_percent)
            || machine
                .free_memory_percent
                .is_some_and(|free| free < u32::from(self.when.free_memory_percent))
        {
            Decline::LowResources
        } else {
            return Answer::taking(rank, holds);
        };
        Answer::declining(decline)
    }
}

/// The state of a device's machine, as an offer round weighs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine {
    /// How it is powered.
    pub power: Power,
    /// The share of its processors already busy, per thousand.
    pub busy_permille: u32,
    /// The share of its memory free, where the machine reports it.
    pub free_memory_percent: Option<u32>,
}

impl Machine {
    /// This machine as an offer round sees it, powered `power`: the load the kernel reports against
    /// the processors it has, and the memory it has free.
    #[must_use]
    pub fn here(power: Power) -> Self {
        Self {
            power,
            busy_permille: busy_permille(),
            free_memory_percent: free_memory_percent(),
        }
    }

    /// The share of the processors left to the people at the machine.
    #[must_use]
    pub fn free_cpu_percent(self) -> u32 {
        (1_000_u32.saturating_sub(self.busy_permille)) / 10
    }
}

/// What share of this machine is already busy, per thousand, from the load the kernel reports
/// against the processors it has. A machine that reports no load says none is in hand.
fn busy_permille() -> u32 {
    let cores =
        u32::try_from(std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get))
            .unwrap_or(u32::MAX);
    let Ok(report) = std::fs::read_to_string("/proc/loadavg") else {
        return 0;
    };
    let Some(one) = report.split_whitespace().next() else {
        return 0;
    };
    // The kernel reports the one-minute load in hundredths; read it as thousandths so that dividing
    // by the processors stays whole numbers.
    let (whole, fraction) = one.split_once('.').unwrap_or((one, ""));
    let whole: u32 = whole.parse().unwrap_or(0);
    let mut fraction = fraction.chars().take(3).collect::<String>();
    while fraction.len() < 3 {
        fraction.push('0');
    }
    let fraction: u32 = fraction.parse().unwrap_or(0);
    let thousandths = whole.saturating_mul(1_000).saturating_add(fraction);
    thousandths.checked_div(cores.max(1)).unwrap_or(0)
}

/// The share of memory this machine reports as available, or `None` where it reports none.
fn free_memory_percent() -> Option<u32> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let read = |name: &str| -> Option<u64> {
        meminfo
            .lines()
            .find_map(|line| line.strip_prefix(name))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    let total = read("MemTotal:")?;
    read("MemAvailable:")?
        .min(total)
        .checked_mul(100)
        .and_then(|hundredfold| hundredfold.checked_div(total))
        .and_then(|share| u32::try_from(share).ok())
}

#[cfg(test)]
mod tests {
    use super::{Machine, WorkFrom, WorkSetting};
    use crate::WorkspaceId;
    use neighbour_wire::offer::{Decline, Power};

    /// A device of the person whose work is at stake may hand it work; another person's device
    /// only where the setting names the workspace it acts in.
    #[test]
    fn work_is_admitted_by_person_and_named_workspace() {
        let person = [1; 32];
        let stranger = [2; 32];
        let workspace = WorkspaceId::from_u128(7);

        assert!(!WorkFrom::Nobody.admits(person, person, Some(workspace)));
        assert!(WorkFrom::OwnDevices.admits(person, person, None));
        assert!(!WorkFrom::OwnDevices.admits(stranger, person, Some(workspace)));
        let named = WorkFrom::Workspaces([workspace].into_iter().collect());
        assert!(named.admits(stranger, person, Some(workspace)));
        assert!(!named.admits(stranger, person, None));
        assert!(WorkFrom::Anyone.admits(stranger, person, Some(workspace)));
        assert_eq!(WorkSetting::own_person().from, WorkFrom::OwnDevices);
        assert!(WorkSetting::none().takes_nothing());
        assert!(!WorkSetting::own_person().takes_nothing());
    }

    fn roomy() -> Machine {
        Machine {
            power: Power::External,
            busy_permille: 100,
            free_memory_percent: Some(80),
        }
    }

    /// A device answers with the first thing its setting says no about.
    #[test]
    fn a_device_answers_with_the_first_limit_its_setting_sets() {
        assert_eq!(
            WorkSetting::none()
                .answer(roomy(), true, None, Vec::new())
                .verdict,
            Err(Decline::NotTakingWork),
            "a device that takes nothing says so"
        );
        let mut no_actions = WorkSetting::own_person();
        no_actions.kinds.actions = false;
        assert_eq!(
            no_actions.answer(roomy(), true, None, Vec::new()).verdict,
            Err(Decline::NotTakingWork)
        );
        assert_eq!(
            WorkSetting::own_person()
                .answer(roomy(), false, None, Vec::new())
                .verdict,
            Err(Decline::AppNotInstalled)
        );
        assert_eq!(
            WorkSetting::own_person()
                .answer(roomy(), true, Some(0), Vec::new())
                .verdict,
            Err(Decline::AtCapacity)
        );

        let charging = WorkSetting {
            when: super::WorkWhen {
                charging_only: true,
                ..super::WorkWhen::always()
            },
            ..WorkSetting::own_person()
        };
        assert_eq!(
            charging
                .answer(
                    Machine {
                        power: Power::Battery,
                        ..roomy()
                    },
                    true,
                    None,
                    Vec::new()
                )
                .verdict,
            Err(Decline::OnBattery),
            "a device that takes work only while charging is on its battery"
        );
        assert!(
            charging
                .answer(
                    Machine {
                        power: Power::Charging,
                        ..roomy()
                    },
                    true,
                    None,
                    Vec::new()
                )
                .verdict
                .is_ok()
        );
    }

    /// The room a setting wants is the room it gets, and a device told nothing about its states
    /// takes work whatever the machine is doing.
    #[test]
    fn a_setting_that_wants_room_is_turned_away_where_it_has_none() {
        let room_kept = WorkSetting {
            when: super::WorkWhen {
                free_cpu_percent: 20,
                free_memory_percent: 20,
                ..super::WorkWhen::always()
            },
            ..WorkSetting::own_person()
        };
        let busy = Machine {
            busy_permille: 900,
            ..roomy()
        };
        assert_eq!(busy.free_cpu_percent(), 10);
        assert_eq!(
            room_kept.answer(busy, true, None, Vec::new()).verdict,
            Err(Decline::LowResources),
            "20 percent of the processors is what the setting wants free"
        );
        let full = Machine {
            free_memory_percent: Some(2),
            ..roomy()
        };
        assert_eq!(
            room_kept.answer(full, true, None, Vec::new()).verdict,
            Err(Decline::LowResources)
        );
        assert!(
            WorkSetting::own_person()
                .answer(busy, true, None, Vec::new())
                .verdict
                .is_ok(),
            "a device told nothing about the states it takes work in takes it whatever its load"
        );
        let answer = WorkSetting::own_person().answer(roomy(), true, None, vec![[7; 32]]);
        assert!(answer.verdict.is_ok(), "a machine with room takes the work");
        assert_eq!(answer.holds, vec![[7; 32]]);
    }
}
