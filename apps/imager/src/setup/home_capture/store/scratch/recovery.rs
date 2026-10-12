//! Reconcile protected sibling worker custody before records-wide budgeting.
use super::*;
const MAX_ENTRIES: usize = 4096;
impl Store {
    pub(super) fn require_inventory_workers_stopped(&self) -> io::Result<()> {
        self.records.validate_destination()?;
        let directory = storage(&self.records)?;
        directory.validate_privacy(Privacy::OwnerOnly)?;
        let parent = migration_durability::readable_directory(&self.records.file)?;
        for (position, entry) in cap_primitives::fs::read_base_dir(&parent)?.enumerate() {
            if position >= MAX_ENTRIES {
                return Err(failure("Inventory worker custody exceeds inspection bound"));
            }
            let name = entry?.file_name();
            let Some(name) = name.to_str() else {
                return Err(failure("Inventory worker custody name invalid"));
            };
            let Some(operation) = name.strip_suffix(".json") else {
                continue;
            };
            super::super::super::operation(operation)?;
            let file = directory.open_read(OsStr::new(name), Privacy::OwnerOnly)?;
            if file.metadata()?.len() > MAX_RECORD {
                return Err(failure("Inventory worker custody exceeds record bound"));
            }
            let mut bytes = Vec::new();
            file.take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
            if bytes.len() as u64 > MAX_RECORD {
                return Err(failure("Inventory worker custody grew beyond bound"));
            }
            match serde_json::from_slice::<DurableRecord>(&bytes)? {
                DurableRecord::Capture(record) => {
                    record.validate()?;
                    if record.grant.operation != operation {
                        return Err(failure("Inventory worker custody operation differs"));
                    }
                    if let Some(worker) = record.worker {
                        worker.require_stopped()?;
                    }
                }
                DurableRecord::Original(original) => {
                    // Closed legacy source-intent variant, no snapshot worker.
                    // Validation refuses unknown schema/custody instead of skipping.
                    if original.witness()?.operation != operation {
                        return Err(failure("Original source custody operation differs"));
                    }
                }
            }
        }
        self.records.validate_destination()
    }
}
