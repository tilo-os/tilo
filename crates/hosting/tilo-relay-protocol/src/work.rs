//! The work a device takes from other devices: what, from whom, and when.
//!
//! A device runs its own person's work at the machine whoever set it up. Taking
//! work is a separate, personal setting: which kinds it takes, whose it takes,
//! and the states it takes them in. Nothing else on a device decides it.

use std::collections::BTreeSet;

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

impl Default for WorkWhen {
    fn default() -> Self {
        Self {
            charging_only: false,
            free_cpu_percent: 20,
            free_memory_percent: 20,
        }
    }
}

/// What work a device takes from other devices.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

    /// Takes every kind of its person's work in every state: an always-on device.
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
}

#[cfg(test)]
mod tests {
    use super::{WorkFrom, WorkSetting};
    use crate::WorkspaceId;

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
}
