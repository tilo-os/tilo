//! Host readiness: facts plus blockers with stable IDs. Unprivileged where
//! the platform allows; the helper re-probes before mutating.

use super::plan::{PlanError, Requirements};

pub use imager_backend::model::{Blocker, HostFacts};

/// Probe this host. Never mutates; safe to call from the UI thread's worker.
///
/// Native facts come from the backend's platform adapter. Shared blockers from
/// [`add_shared_blockers`] are added here for both; `InsufficientSpace`
/// needs bundle sizes and comes from [`space_blocker`].
pub fn probe() -> HostFacts {
    #[cfg(all(test, target_os = "linux", feature = "installation"))]
    if let Some(facts) = super::super::journey::main_story_tests::fixture_host_facts() {
        return facts;
    }
    let mut facts = imager_backend::staging::probe_host();
    add_shared_blockers(&mut facts);
    facts
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

    #[test]
    fn non_uefi_preparation_geometry_does_not_offer_installation() {
        use super::super::plan;
        let mut basis = plan::tests::linux_basis(512, 64 << 30, Vec::new());
        basis.firmware.uefi = false;
        basis.firmware.secure_boot = false;
        basis.firmware.boot_current = None;
        basis.firmware.boot_order.clear();
        basis.firmware.boot_next = None;
        let mut facts = HostFacts {
            basis: Some(basis.clone()),
            bitlocker_protected: false,
            blockers: Vec::new(),
        };
        add_shared_blockers(&mut facts);
        assert!(facts.blockers.contains(&Blocker::NotUefi));
        assert!(!facts.offers_full_os());
        let reviewed = plan::compute(
            &basis,
            Requirements {
                staging_esp_bytes: 512 << 20,
                staging_root_bytes: 4 << 30,
                target_bytes: 35 << 30,
            },
        )
        .unwrap();
        assert_eq!(reviewed.basis, basis);
        assert!(!reviewed.basis.firmware.uefi);
        assert!(!facts.offers_full_os());
    }

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
