//! When the person must be asked, what the prompt offers first, and what a
//! "Remember" may keep.

use serde::{Deserialize, Serialize};

use crate::identity::Identity;
use crate::protocol::{Mode, Placement};
use crate::topology::Topology;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriveFacts {
    pub topology: Topology,
    pub identity: Identity,
    pub present_at_boot: bool,
    pub on_install_disk: bool,
}

impl DriveFacts {
    pub fn is_internal(&self) -> bool {
        self.topology.is_internal() && self.present_at_boot
    }

    pub fn placement(&self) -> Placement {
        if self.is_internal() {
            Placement::Internal
        } else {
            Placement::External
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Binding {
    Internal {
        device_path: String,
        identity: Identity,
    },
    External {
        identity: Identity,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub label: String,
    pub mode: Mode,
    pub binding: Binding,
}

impl Grant {
    pub fn placement(&self) -> Placement {
        match self.binding {
            Binding::Internal { .. } => Placement::Internal,
            Binding::External { .. } => Placement::External,
        }
    }

    pub fn applies_to(&self, facts: &DriveFacts) -> bool {
        match &self.binding {
            Binding::Internal {
                device_path,
                identity,
            } => {
                facts.is_internal()
                    && *device_path == facts.topology.device_path
                    && identity.is_complete()
                    && identity.matches(&facts.identity)
            }
            Binding::External { identity } => identity.matches(&facts.identity),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assessment<'a> {
    pub remembered: Option<&'a Grant>,
    pub needs_consent: bool,
    pub preselect: Mode,
    pub can_remember: bool,
    pub can_remember_open: bool,
}

pub fn assess<'a>(facts: &DriveFacts, grants: &'a [Grant]) -> Assessment<'a> {
    let remembered = grants.iter().find(|grant| grant.applies_to(facts));
    let internal = facts.is_internal();
    let can_remember_open = internal && facts.identity.is_complete();
    let can_remember = can_remember_open
        || facts.identity.drive.is_some()
        || facts.identity.partition.is_some()
        || facts.identity.volume.is_some();
    match remembered {
        Some(grant) if matches!(grant.binding, Binding::Internal { .. }) => Assessment {
            remembered,
            needs_consent: false,
            preselect: grant.mode,
            can_remember,
            can_remember_open,
        },
        _ => Assessment {
            remembered,
            needs_consent: true,
            // A drive that looks like a remembered one but fails its checks
            // is treated as new, whatever disk it sits on (D5).
            preselect: if internal && facts.on_install_disk && !resembles_remembered(facts, grants)
            {
                Mode::Open
            } else {
                Mode::OpenSafely
            },
            can_remember,
            can_remember_open,
        },
    }
}

/// Whether `facts` shares a location or an identifier with a remembered
/// internal drive it does not match.
fn resembles_remembered(facts: &DriveFacts, grants: &[Grant]) -> bool {
    grants.iter().any(|grant| match &grant.binding {
        Binding::Internal {
            device_path,
            identity,
        } => {
            !grant.applies_to(facts)
                && (*device_path == facts.topology.device_path
                    || shares_identifier(identity, &facts.identity))
        }
        Binding::External { .. } => false,
    })
}

fn shares_identifier(recorded: &Identity, seen: &Identity) -> bool {
    (recorded.drive.is_some() && recorded.drive == seen.drive)
        || (recorded.partition.is_some() && recorded.partition == seen.partition)
        || (recorded.volume.is_some() && recorded.volume == seen.volume)
}

/// The grant a "Remember" keeps for `facts` after the person chose `mode`,
/// or `None` when nothing may be remembered for this drive.
pub fn remember(facts: &DriveFacts, mode: Mode, id: String, label: String) -> Option<Grant> {
    let assessment = assess(facts, &[]);
    if assessment.can_remember_open {
        Some(Grant {
            id,
            label,
            mode,
            binding: Binding::Internal {
                device_path: facts.topology.device_path.clone(),
                identity: facts.identity.clone(),
            },
        })
    } else if assessment.can_remember {
        Some(Grant {
            id,
            label,
            mode: Mode::OpenSafely,
            binding: Binding::External {
                identity: facts.identity.clone(),
            },
        })
    } else {
        None
    }
}

pub fn mode_allowed(grant: &Grant, mode: Mode) -> bool {
    mode == Mode::OpenSafely || matches!(grant.binding, Binding::Internal { .. })
}
