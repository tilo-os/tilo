//! Host readiness: facts plus blockers with stable IDs. Unprivileged where
//! the platform allows; the helper re-probes before mutating.

use serde::{Deserialize, Serialize};

use super::plan::{Basis, PlanError, Requirements};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Blocker {
    NotUefi,
    SecureBootOn,
    BitlockerLocked,
    NotGpt,
    DiskUnsupported,
    Hibernation,
    InsufficientSpace,
    OnBattery,
    PendingOperation,
    ArchMismatch,
}

impl Blocker {
    /// Blockers the person can clear from Imager (a remedy action exists).
    pub fn has_remedy(self) -> bool {
        matches!(
            self,
            Self::SecureBootOn | Self::Hibernation | Self::OnBattery | Self::PendingOperation
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFacts {
    /// `None` when the disk could not be read at all (then `blockers` says why).
    pub basis: Option<Basis>,
    pub bitlocker_protected: bool,
    pub blockers: Vec<Blocker>,
}

impl HostFacts {
    /// Full Tilo may be offered: nothing blocks that Imager cannot help clear.
    pub fn offers_full_os(&self) -> bool {
        self.basis.is_some() && self.blockers.iter().all(|b| b.has_remedy())
    }

    /// Add `blocker` once.
    pub fn block(&mut self, blocker: Blocker) {
        if !self.blockers.contains(&blocker) {
            self.blockers.push(blocker);
        }
    }

    /// Facts for a host that cannot be probed at all.
    pub fn unsupported(blocker: Blocker) -> Self {
        Self {
            basis: None,
            bitlocker_protected: false,
            blockers: vec![blocker],
        }
    }
}

/// Probe this host. Never mutates; safe to call from the UI thread's worker.
///
/// Linux: [`super::linux::probe_facts`]. Windows: `super::windows::probe_facts`
/// (owned by the Windows adapter). The shared blockers from
/// [`add_shared_blockers`] are added here for both; `InsufficientSpace`
/// needs bundle sizes and comes from [`space_blocker`].
pub fn probe() -> HostFacts {
    #[cfg(target_os = "linux")]
    {
        let mut facts = super::linux::probe_facts();
        add_shared_blockers(&mut facts);
        facts
    }
    #[cfg(windows)]
    {
        let mut facts = super::windows::probe_facts();
        add_shared_blockers(&mut facts);
        facts
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        HostFacts::unsupported(Blocker::DiskUnsupported)
    }
}

/// Blockers every platform derives the same way from a basis: Secure Boot,
/// an unfinished staging record for the disk, and no internal bundle for
/// this architecture.
pub fn add_shared_blockers(facts: &mut HostFacts) {
    let mut found = Vec::new();
    if let Some(basis) = &facts.basis {
        if !basis.firmware.uefi {
            found.push(Blocker::NotUefi);
        }
        if basis.firmware.secure_boot {
            found.push(Blocker::SecureBootOn);
        }
        let pending = super::helper::pending_records();
        if pending.iter().any(|r| {
            super::same_guid(&r.plan.basis.disk.guid, &basis.disk.guid)
                && r.status() == super::record::RecordStatus::InProgress
        }) {
            found.push(Blocker::PendingOperation);
        }
    }
    for blocker in found {
        facts.block(blocker);
    }
    if crate::release::bundled_internal_releases().map_or(true, |r| r.is_empty()) {
        facts.block(Blocker::ArchMismatch);
    }
}

/// `InsufficientSpace` when not even the installer minimum fits beside
/// staging of `requirements` (whose `target_bytes` is ignored).
pub fn space_blocker(facts: &HostFacts, requirements: Requirements) -> Option<Blocker> {
    let basis = facts.basis.as_ref()?;
    match super::plan::target_bounds(basis, requirements) {
        Ok(_) => None,
        Err(PlanError::InsufficientSpace { .. }) => Some(Blocker::InsufficientSpace),
        Err(PlanError::Unsupported(_)) => Some(Blocker::DiskUnsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Remedies, hard blockers and space blockers.
    #[test]
    fn staging_offer_blockers_and_remedies_follow_planner_bounds() {
        // Phase `remedies_do_not_hide_the_offer_but_hard_blockers_do`.
        {
            let basis = super::super::plan::tests::linux_basis(512, 256 << 30, Vec::new());
            let mut facts = HostFacts {
                basis: Some(basis),
                bitlocker_protected: false,
                blockers: vec![Blocker::SecureBootOn, Blocker::OnBattery],
            };
            assert!(facts.offers_full_os());
            facts.block(Blocker::NotGpt);
            facts.block(Blocker::NotGpt);
            assert_eq!(facts.blockers.len(), 3);
            assert!(!facts.offers_full_os());
            assert!(!HostFacts::unsupported(Blocker::NotUefi).offers_full_os());
        }

        // Phase `space_blocker_uses_the_planner_bounds`.
        {
            let small = super::super::plan::tests::linux_basis(512, 60 << 30, Vec::new());
            let facts = HostFacts {
                basis: Some(small),
                bitlocker_protected: false,
                blockers: Vec::new(),
            };
            let r = Requirements {
                staging_esp_bytes: 600 << 20,
                staging_root_bytes: 40 << 30,
                target_bytes: 0,
            };
            assert_eq!(space_blocker(&facts, r), Some(Blocker::InsufficientSpace));
            let big = HostFacts {
                basis: Some(super::super::plan::tests::linux_basis(
                    512,
                    200 << 30,
                    Vec::new(),
                )),
                ..facts
            };
            assert_eq!(space_blocker(&big, r), None);
        }
    }
}
