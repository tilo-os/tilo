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
    /// Whether it takes work of anyone but the device's own person.
    #[must_use]
    pub fn takes_others(&self) -> bool {
        match self {
            Self::Nobody | Self::OwnDevices => false,
            Self::Workspaces(named) => !named.is_empty(),
            Self::Anyone => true,
        }
    }

    /// Whether this device takes the work `whose` describes from whom asked.
    #[must_use]
    pub fn admits(&self, whose: &Whose) -> bool {
        match self {
            Self::Nobody => false,
            Self::OwnDevices => whose.asker == whose.own,
            Self::Workspaces(named) => {
                whose.asker == whose.own || whose.workspace.is_some_and(|id| named.contains(&id))
            }
            Self::Anyone => true,
        }
    }
}

/// Whose work a hand-over is, as the workspace it hands over says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Whose {
    /// The person whose device asked.
    pub asker: String,
    /// The person of the device asked.
    pub own: String,
    /// The workspace the work is in.
    pub workspace: Option<WorkspaceId>,
}

impl Whose {
    /// The work of the person with the root `asker`, offered to a device of the person with the
    /// root `own`, outside any workspace.
    #[must_use]
    pub fn of_roots(asker: [u8; 32], own: [u8; 32]) -> Self {
        let person = |root| hex::encode(root);
        Self {
            asker: person(asker),
            own: person(own),
            workspace: None,
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
}
