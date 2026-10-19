//! Opaque transfer custody for setup frontends on every host.
//! Unsupported hosts refuse before inspecting source or destination paths.
use serde_json::Value;
use std::path::{Path, PathBuf};

pub fn ensure_supported() -> Result<(), &'static str> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        Err("installer-preview-import-unavailable")
    }
}

#[derive(Clone)]
pub struct TransferReview {
    #[cfg(target_os = "linux")]
    inner: crate::setup::user_transfer::TransferReview,
    #[cfg(not(target_os = "linux"))]
    unavailable: std::convert::Infallible,
}
pub struct TransferWorker {
    #[cfg(target_os = "linux")]
    inner: crate::setup::user_transfer::TransferWorker,
    #[cfg(not(target_os = "linux"))]
    unavailable: std::convert::Infallible,
}
pub enum TransferEvent {
    CopyProgress(Value),
    RestoringApplications,
    Finished { state: &'static str, result: Value },
}
impl TransferReview {
    pub fn target_home(&self) -> &Path {
        #[cfg(target_os = "linux")]
        {
            self.inner.target_home()
        }
        #[cfg(not(target_os = "linux"))]
        match self.unavailable {}
    }
    pub fn recovered(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.inner.recovered()
        }
        #[cfg(not(target_os = "linux"))]
        match self.unavailable {}
    }
    pub fn projection(&self) -> Value {
        #[cfg(target_os = "linux")]
        {
            serde_json::json!({"target":path_projection(self.inner.target_home()),"copies":self.inner.copy_count(),"summary":self.inner.summary().ok(),"recovered":self.inner.recovered()})
        }
        #[cfg(not(target_os = "linux"))]
        match self.unavailable {}
    }
    pub fn start(&self, retry: bool) -> Result<TransferWorker, String> {
        #[cfg(target_os = "linux")]
        {
            self.inner
                .start(retry)
                .map(|inner| TransferWorker { inner })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = retry;
            match self.unavailable {}
        }
    }
    pub fn review_promotion(
        &self,
        home: &Path,
        journal: &Path,
    ) -> Result<crate::journey_promotion::PromotionReview, String> {
        #[cfg(target_os = "linux")]
        {
            self.inner
                .review_promotion(home, journal)
                .map(crate::journey_promotion::PromotionReview::new)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (home, journal);
            match self.unavailable {}
        }
    }
}
impl TransferWorker {
    pub fn cancel(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.inner.cancel()
        }
        #[cfg(not(target_os = "linux"))]
        match self.unavailable {}
    }
    pub fn try_next(&mut self) -> Option<TransferEvent> {
        #[cfg(target_os = "linux")]
        {
            self.inner.try_next().map(|event| {
            use crate::setup::user_transfer::TransferEvent as Native;
            match event {
                Native::CopyProgress(progress) => TransferEvent::CopyProgress(serde_json::json!({"verified_files":progress.verified_files,"verified_bytes":progress.verified_bytes,"current_relative_path":path_projection(&progress.current_relative_path)})),
                Native::RestoringApplications => TransferEvent::RestoringApplications,
                Native::Finished(outcome) => {
                    let state = match &outcome {
                        Ok(report) if report.cancelled => "cancelled",
                        Ok(report) if report.complete() => "complete",
                        Ok(_) => "incomplete",
                        Err(_) => "failed",
                    };
                    let result = match outcome {
                        Ok(report) => serde_json::json!({"copies":report.copies.iter().map(|copy| serde_json::json!({"verified_files":copy.verified_files.len(),"verified_bytes":copy.verified_bytes,"skipped":issue_projection(&copy.skipped),"failed":issue_projection(&copy.failed),"metadata_limitations":copy.metadata_limitations})).collect::<Vec<_>>(),"flatpak_restored":report.flatpak.restored,"flatpak_failed":report.flatpak.failed,"cancelled":report.cancelled}),
                        Err(error) => serde_json::json!({"error":error}),
                    };
                    TransferEvent::Finished { state, result }
                }
            }
        })
        }
        #[cfg(not(target_os = "linux"))]
        match self.unavailable {}
    }
}

pub struct TransferRequest {
    pub source_home: PathBuf,
    pub target_home: PathBuf,
    pub private_journals: PathBuf,
    pub copy_home: bool,
    pub copy_flatpak: bool,
    pub system_refs: Vec<crate::setup::flatpak_transfer::InstalledRef>,
}
pub fn review(request: TransferRequest) -> Result<TransferReview, String> {
    ensure_supported()?;
    #[cfg(target_os = "linux")]
    {
        crate::setup::user_transfer::review(
            &crate::setup::flatpak_transfer::NativeRunner,
            crate::setup::user_transfer::TransferRequest {
                source_home: request.source_home,
                target_home: request.target_home,
                private_journals: request.private_journals,
                copy_home: request.copy_home,
                copy_flatpak: request.copy_flatpak,
                system_refs: request.system_refs,
            },
        )
        .map(|inner| TransferReview { inner })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = request;
        Err("Whole-home transfer is currently supported on Linux only.".into())
    }
}

/// Read only a private, bounded, no-follow application handoff inventory.
/// Unsupported hosts refuse before resolving the supplied path.
pub fn read_flatpak_inventory(
    path: &std::path::Path,
) -> Result<crate::setup::flatpak_transfer::HandoffInventory, String> {
    ensure_supported()?;
    #[cfg(target_os = "linux")]
    {
        use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
        use cap_primitives::fs::{FollowSymlinks, OpenOptions};
        use std::io::Read;
        let parent = crate::migration::PinnedDirectory::open_absolute(
            path.parent()
                .ok_or("Flatpak inventory requires a private parent")?,
        )
        .map_err(|error| error.to_string())?;
        parent
            .validate_destination()
            .map_err(|error| error.to_string())?;
        crate::destination_security::check_private_file(&parent.file)
            .map_err(|error| error.to_string())?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let file = cap_primitives::fs::open(
            &parent.file,
            std::path::Path::new(
                path.file_name()
                    .ok_or("Flatpak inventory requires a file name")?,
            ),
            &options,
        )
        .map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > 8 * 1024 * 1024 {
            return Err("Flatpak inventory must be a bounded private regular file".into());
        }
        crate::destination_security::check_private_file(&file)
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        file.take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        parent.validate().map_err(|error| error.to_string())?;
        crate::setup::flatpak_transfer::HandoffInventory::parse(&bytes)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        Err("Whole-home transfer is currently supported on Linux only.".into())
    }
}

#[cfg(target_os = "linux")]
fn path_projection(path: &Path) -> Value {
    serde_json::json!({"display":path.to_string_lossy(),"native":crate::migration_store::NativePath::encode(path)})
}
#[cfg(target_os = "linux")]
fn issue_projection(issues: &[(PathBuf, String)]) -> Vec<Value> {
    issues
        .iter()
        .map(|(path, message)| serde_json::json!({"path":path_projection(path),"message":message}))
        .collect()
}

pub fn prepare_migration(
    source: &Path,
    directory: &Path,
    operation: &str,
    categories: [bool; 3],
    refs: Vec<crate::setup::flatpak_transfer::InstalledRef>,
) -> Result<Value, String> {
    ensure_supported()?;
    #[cfg(target_os = "linux")]
    {
        let intent = crate::setup::migration_intent::MigrationIntent::capture(
            source, directory, categories, refs,
        )
        .map_err(|error| error.to_string())?;
        let mut plan = intent.source_plan();
        operation.clone_into(&mut plan.operation_id);
        plan.write_private(directory)
            .map_err(|error| error.to_string())?;
        Ok(
            serde_json::json!({"operation":operation,"digest":plan.digest().map_err(|error|error.to_string())?}),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (source, directory, operation, categories, refs);
        Err("Whole-home transfer is currently supported on Linux only.".into())
    }
}

pub fn review_installed(
    directory: &Path,
    operation: &str,
    destination: &Path,
    journals: &Path,
    categories: [bool; 3],
    flatpak_inventory: Option<PathBuf>,
) -> Result<TransferReview, String> {
    ensure_supported()?;
    #[cfg(target_os = "linux")]
    {
        let plan = crate::setup::migration_handoff::Plan::read_private(directory, operation)
            .map_err(|error| error.to_string())?;
        if [
            plan.categories.home,
            plan.categories.flatpak,
            plan.categories.accounts,
        ] != categories
        {
            return Err("Migration choices changed; review the original plan again.".into());
        }
        let inventory = match flatpak_inventory {
            Some(path) if plan.categories.flatpak => Some(read_flatpak_inventory(&path)?),
            Some(_) => {
                return Err(
                    "Flatpak inventory was supplied without selecting applications.".into(),
                );
            }
            None if plan.categories.flatpak => {
                let raw = plan.flatpak_inventory.as_ref().ok_or("The original reviewed Flatpak inventory is required before restoring applications.")?;
                Some(crate::setup::flatpak_transfer::HandoffInventory::parse(
                    &serde_json::to_vec(raw).map_err(|error| error.to_string())?,
                )?)
            }
            None => None,
        };
        let source = crate::setup::migration_handoff::mount_read_only(&plan)
            .map_err(|error| error.to_string())?;
        let inner = match inventory {
            Some(inventory) => {
                source.review_transfer_with_handoff(destination, journals, &inventory)
            }
            None => source.review_transfer(destination, journals),
        }?;
        Ok(TransferReview { inner })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (
            directory,
            operation,
            destination,
            journals,
            categories,
            flatpak_inventory,
        );
        Err("Whole-home transfer is currently supported on Linux only.".into())
    }
}

#[cfg(not(target_os = "linux"))]
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unsupported_requests_refuse_without_resolving_paths() {
        let path = Path::new("missing-relative-path");
        assert_eq!(
            ensure_supported(),
            Err("installer-preview-import-unavailable")
        );
        assert!(read_flatpak_inventory(path).is_err());
        assert!(prepare_migration(path, path, "operation", [true; 3], vec![]).is_err());
        assert!(review_installed(path, "operation", path, path, [true; 3], None).is_err());
        assert!(
            review(TransferRequest {
                source_home: path.into(),
                target_home: path.into(),
                private_journals: path.into(),
                copy_home: true,
                copy_flatpak: true,
                system_refs: vec![]
            })
            .is_err()
        );
    }
}
