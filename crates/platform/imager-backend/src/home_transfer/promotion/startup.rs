//! Restrict imported startup entries before moving them into the active home.
use super::*;
use desktop_entry::migration_startup::{
    Entry as StartupEntry, RECORD_NAME, REQUIRED_NAME, RecordBuilder, Required,
};

impl PromotionReview {
    pub(super) fn protect_imported_startup(
        &self,
        quarantine: &Arc<PinnedDirectory>,
        home: &Arc<PinnedDirectory>,
        done: &Done,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let operation = hex::encode(plan_digest(&self.plan)?);
        let mut entries = RecordBuilder::new(operation.clone())?;
        let mut captured = false;
        for (index, entry) in self.plan.entries.iter().enumerate() {
            let entry = entry?;
            super::super::super::migration::check_cancel(cancel)?;
            if !matches!(entry.kind, Kind::Move) {
                continue;
            }
            let source = if done.contains(index)? {
                home
            } else {
                quarantine
            };
            for item in entry.items.iter() {
                if let Some(item) = StartupEntry::capture(&source.file, &decoded(&item?)?)? {
                    captured = true;
                    if !self.plan.applications {
                        entries.push(item)?;
                    }
                }
            }
        }
        if !captured {
            return Ok(());
        }
        // Both marker and record bind the same immutable adoption plan. A crash
        // after marker publication refuses startup until exact retry completes.

        let directory = parent(home, Path::new(".config/tilo"))?;
        destination_security::check_private_file(&directory.file)?;
        let marker = Required {
            schema: 1,
            operation,
        };
        let record = entries.finish()?;
        super::super::super::user_transfer::publish_private_artifact(
            &directory,
            REQUIRED_NAME,
            &serde_json::to_vec(&marker)?,
        )?;
        super::super::super::user_transfer::publish_private_artifact(
            &directory,
            RECORD_NAME,
            &serde_json::to_vec(&record)?,
        )?;
        home.validate_destination()?;
        quarantine.validate_destination()
    }
}
