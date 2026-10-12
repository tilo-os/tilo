//! Read-only host inspection and the existing staging planner. No helper writes.
use std::sync::mpsc::{self, Receiver};

use super::super::staging::{self, plan, probe};

pub(super) struct Storage {
    pub facts: Option<probe::HostFacts>,
    pub size: ui::LocalState<f64>,
    pub plan: Option<Result<plan::Plan, plan::PlanError>>,
    pub bounds: Option<(u64, u64)>,
    pub details: bool,
    worker: Option<Receiver<probe::HostFacts>>,
    selected: u64,
    minimum: u64,
}

impl Default for Storage {
    fn default() -> Self {
        let selected = staging::DEFAULT_TARGET_BYTES >> 30;
        Self {
            facts: None,
            size: ui::LocalState::new(selected as f64),
            plan: None,
            bounds: None,
            details: false,
            worker: None,
            selected,
            minimum: staging::MIN_TARGET_BYTES,
        }
    }
}

impl Storage {
    pub fn checking(&self) -> bool {
        self.worker.is_some()
    }

    pub fn check(&mut self) {
        self.check_with(false);
    }

    pub fn review_capacity(&mut self) {
        self.check_with(true);
    }

    fn check_with(&mut self, elevated: bool) {
        if self.checking() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.facts = None;
        self.bounds = None;
        self.plan = None;
        std::thread::spawn(move || {
            let _ = tx.send(if elevated {
                staging::capacity_probe::review()
            } else {
                probe::probe()
            });
        });
    }

    pub fn poll(&mut self) -> bool {
        if let Some(worker) = &self.worker {
            match worker.try_recv() {
                Ok(facts) => {
                    self.worker = None;
                    self.accept(facts);
                    return true;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.accept(probe::HostFacts::unsupported(
                        probe::Blocker::DiskUnsupported,
                    ));
                    return true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let value = self.size.get().round() as u64;
        if value != self.selected && self.set_size(value).is_ok() {
            return true;
        }
        false
    }

    pub fn accept(&mut self, facts: probe::HostFacts) {
        self.bounds = facts.basis.as_ref().and_then(|basis| {
            plan::target_bounds(basis, requirements(0))
                .ok()
                .and_then(|bounds| {
                    let min = bounds.min_bytes.max(self.minimum).div_ceil(1 << 30);
                    let max = bounds.max_bytes >> 30;
                    (min <= max).then_some((min, max))
                })
        });
        self.facts = Some(facts);
        if let Some((min, max)) = self.bounds {
            self.selected = self.selected.clamp(min, max);
            self.size.set(self.selected as f64);
        }
        self.recompute();
    }

    pub fn set_size(&mut self, gib: u64) -> Result<(), &'static str> {
        let (min, max) = self.bounds.ok_or("installer-preview-storage-unavailable")?;
        if !(min..=max).contains(&gib) {
            return Err("installer-preview-storage-range-error");
        }
        self.selected = gib;
        self.size.set(gib as f64);
        self.recompute();
        Ok(())
    }

    fn recompute(&mut self) {
        self.plan = self
            .facts
            .as_ref()
            .and_then(|facts| facts.basis.as_ref())
            .map(|basis| {
                let selected = self.selected << 30;
                if selected < self.minimum {
                    Err(plan::PlanError::InsufficientSpace {
                        needed_bytes: self.minimum,
                        available_bytes: selected,
                    })
                } else {
                    plan::compute(basis, requirements(selected))
                }
            });
    }

    pub fn set_application_minimum(&mut self, bytes: u64) {
        self.minimum = bytes.max(staging::MIN_TARGET_BYTES);
        if let Some(facts) = self.facts.take() {
            self.accept(facts);
        }
    }

    pub fn application_estimate(&self) -> Option<u64> {
        (self.minimum > staging::MIN_TARGET_BYTES).then(|| self.minimum - staging::MIN_TARGET_BYTES)
    }

    pub fn selected(&self) -> u64 {
        self.selected
    }

    /// Captures use a partitioned disk and the production planner, never host I/O.
    pub fn stage(&mut self) {
        use staging::platform::{Disk, Partition};
        self.accept(probe::HostFacts {
            basis: Some(plan::Basis {
                linux_btrfs_volume: None,
                disk: Disk {
                    id: "capture-disk".into(),
                    guid: "11111111-2222-4333-8444-555555555555".into(),
                    size_bytes: 512 << 30,
                    sector_bytes: 512,
                    first_usable_lba: 34,
                    last_usable_lba: (512_u64 << 30) / 512 - 34,
                    partitions: vec![Partition {
                        number: 1,
                        guid: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into(),
                        type_guid: "0fc63daf-8483-4772-8e79-3d69d8477de4".into(),
                        first_lba: 2048,
                        last_lba: (128_u64 << 30) / 512 - 1,
                    }],
                },
                system_volume: None,
                firmware: plan::FirmwareFacts {
                    uefi: true,
                    secure_boot: false,
                    boot_current: Some("0001".into()),
                    boot_order: vec!["0001".into()],
                    boot_next: None,
                },
            }),
            bitlocker_protected: false,
            blockers: Vec::new(),
        });
    }
}

pub(super) fn requirements(target_bytes: u64) -> plan::Requirements {
    plan::Requirements {
        staging_esp_bytes: staging::PROVISIONAL_STAGING_ESP_BYTES,
        staging_root_bytes: staging::PROVISIONAL_STAGING_ROOT_BYTES,
        target_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn production_planner_and_size_control_enforce_application_lower_bound() {
        let mut storage = Storage::default();
        storage.set_application_minimum(180 << 30);
        storage.stage();
        let (minimum, _) = storage.bounds.unwrap();
        assert_eq!(minimum, 180);
        assert!(storage.selected() >= minimum);
        assert!(storage.set_size(179).is_err());
        let plan = storage.plan.as_ref().unwrap().as_ref().unwrap();
        assert!(plan.target_extent.lbas() * plan.basis.disk.sector_bytes >= 180 << 30);
        assert_eq!(
            storage.application_estimate(),
            Some((180 << 30) - staging::MIN_TARGET_BYTES)
        );
    }
}
