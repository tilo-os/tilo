use super::{OfflineRunner, Runner};
use crate::setup::migration::PinnedDirectory;
use std::{
    ffi::OsString,
    path::Path,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
/// Offline operations run without a user session helper or inherited proxy/env.
pub struct NativeOfflineRunner;
impl Runner for NativeOfflineRunner {
    fn review_offline_subset(
        &self,
        grant: &super::SourceExportGrant<'_>,
    ) -> Result<Option<super::ReviewedSubset>, String> {
        super::export::review(grant, None).map(Some)
    }
    fn run(&self, home: &Path, arguments: &[String]) -> Result<String, String> {
        super::super::NativeRunner.run(home, arguments)
    }
    fn verify_repository(&self, home: &Path) -> Result<(), String> {
        super::super::NativeRunner.verify_repository(home)
    }
}
impl OfflineRunner for NativeOfflineRunner {
    fn verify_extra_data(&self, repository: &Path, hash: &str) -> Result<u64, String> {
        super::extra_data::verify(repository, hash, None)
            .and_then(|storage| storage.target_bytes())
            .map_err(|error| error.to_string())
    }
    fn export_offline_subset(&self, grant: &super::SubsetExportGrant<'_>) -> Result<bool, String> {
        super::export::export(grant, None).map(|()| true)
    }
    fn ostree(&self, repository: &Path, arguments: &[String]) -> Result<String, String> {
        run_ostree(repository, arguments, None)
    }
}
pub struct CancellableOfflineRunner(pub Arc<AtomicBool>);
impl Runner for CancellableOfflineRunner {
    fn review_offline_subset(
        &self,
        grant: &super::SourceExportGrant<'_>,
    ) -> Result<Option<super::ReviewedSubset>, String> {
        super::export::review(grant, Some(&self.0)).map(Some)
    }
    fn run(&self, home: &Path, arguments: &[String]) -> Result<String, String> {
        super::super::CancellableNativeRunner(self.0.clone()).run(home, arguments)
    }
    fn verify_repository(&self, home: &Path) -> Result<(), String> {
        super::super::CancellableNativeRunner(self.0.clone()).verify_repository(home)
    }
}
impl OfflineRunner for CancellableOfflineRunner {
    fn verify_extra_data(&self, repository: &Path, hash: &str) -> Result<u64, String> {
        super::extra_data::verify(repository, hash, Some(&self.0))
            .and_then(|storage| storage.target_bytes())
            .map_err(|error| error.to_string())
    }
    fn export_offline_subset(&self, grant: &super::SubsetExportGrant<'_>) -> Result<bool, String> {
        super::export::export(grant, Some(&self.0)).map(|()| true)
    }
    fn ostree(&self, repository: &Path, arguments: &[String]) -> Result<String, String> {
        run_ostree(repository, arguments, Some(&self.0))
    }
}
fn run_ostree(
    repository: &Path,
    arguments: &[String],
    cancel: Option<&AtomicBool>,
) -> Result<String, String> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        return Err("Offline application transfer cancelled".into());
    }
    let pin = PinnedDirectory::open_absolute(repository).map_err(|e| e.to_string())?;
    pin.validate_destination().map_err(|e| e.to_string())?;
    if !matches!(
        arguments.first().map(String::as_str),
        Some("refs" | "pull-local" | "fsck")
    ) {
        return Err("Unsupported portable repository operation".into());
    }
    let mut command = Command::new("/usr/bin/ostree");
    // OSTree selects the first non-option as the command before parsing its
    // options. Keep the repository path in the same argument, including native
    // non-UTF8 bytes, so it cannot become a spurious command.
    let mut repo_option = OsString::from("--repo=");
    repo_option.push(repository.as_os_str());
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .arg(repo_option)
        .args(arguments);
    super::super::offline_original::configure_ostree(&mut command)
        .map_err(|error| error.to_string())?;
    let mut worker =
        bounded_command::spawn_prepared(command, None, 8 * 1024 * 1024, Duration::from_secs(3600))
            .map_err(|_| "Offline repository operation could not start")?;
    let output = loop {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err("Offline application transfer cancelled".into());
        }
        if let Some(result) = worker.try_wait() {
            break result.map_err(|_| "Offline repository operation exceeded its bounds")?;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    pin.validate_destination().map_err(|e| e.to_string())?;
    if !output.status.success() {
        #[cfg(test)]
        if std::env::var("TILO_NATIVE_FLATPAK_FIXTURE_DIAGNOSTICS").as_deref() == Ok("1") {
            eprintln!(
                "Native fixture OSTree operation {arguments:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        return Err("Offline repository verification/import failed".into());
    }
    String::from_utf8(output.stdout).map_err(|_| "Invalid repository response".into())
}
