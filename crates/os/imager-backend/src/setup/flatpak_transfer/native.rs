use super::Runner;
use std::path::Path;
#[cfg(target_os = "linux")]
pub(super) mod bindings;
#[cfg(target_os = "linux")]
mod trust;
#[cfg(target_os = "linux")]
use std::{process::Command, time::Duration};

/// Runs the system Flatpak binary with a clean environment and explicit user
/// directories. Run target restoration as the target user after first login;
/// root must not create root-owned user installations on their behalf.
pub struct NativeRunner;

impl Runner for NativeRunner {
    #[cfg(target_os = "linux")]
    fn review_offline_subset(
        &self,
        grant: &super::portable::SourceExportGrant<'_>,
    ) -> Result<Option<super::portable::ReviewedSubset>, String> {
        super::portable::export::review(grant, None).map(Some)
    }
    fn run(&self, home: &Path, arguments: &[String]) -> Result<String, String> {
        run_native(home, arguments, None)
    }
    fn verify_repository(&self, home: &Path) -> Result<(), String> {
        verify_repository(home, None)
    }
}

pub struct CancellableNativeRunner(pub std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Runner for CancellableNativeRunner {
    #[cfg(target_os = "linux")]
    fn review_offline_subset(
        &self,
        grant: &super::portable::SourceExportGrant<'_>,
    ) -> Result<Option<super::portable::ReviewedSubset>, String> {
        super::portable::export::review(grant, Some(&self.0)).map(Some)
    }
    fn run(&self, home: &Path, arguments: &[String]) -> Result<String, String> {
        run_native(home, arguments, Some(&self.0))
    }
    fn verify_repository(&self, home: &Path) -> Result<(), String> {
        verify_repository(home, Some(&self.0))
    }
}
fn run_native(
    home: &Path,
    arguments: &[String],
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<String, String> {
    #[cfg(all(any(test, feature = "test-support"), target_os = "linux"))]
    if crate::setup::discovery_fixture::fixture_mount_root()
        .is_some_and(|root| home == root.join("home"))
        && matches!(
            arguments.first().map(String::as_str),
            Some("list" | "remotes")
        )
    {
        return Err("Flatpak discovery is unavailable in the isolated guided-story fixture".into());
    }
    if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
        return Err("Flatpak transfer cancelled".into());
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let pinned = super::super::migration::PinnedDirectory::open_absolute(home)
            .map_err(|error| error.to_string())?;
        pinned.validate().map_err(|error| error.to_string())?;
        let metadata = pinned.file.metadata().map_err(|error| error.to_string())?;
        // SAFETY: geteuid has no preconditions.
        let uid = rustix::process::geteuid().as_raw();
        if uid == 0 || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
            return Err(
                "Flatpak migration must run as the owner of a private home directory".into(),
            );
        }
        if arguments.first().is_some_and(|verb| verb == "remote-add") {
            trust::verify_remote_reuse(home, arguments)?;
        }
        let installation = install_selection(arguments);
        if let Some((origin, reference)) = installation {
            let info = vec![
                "info".into(),
                "--user".into(),
                "--show-origin".into(),
                reference.to_owned(),
            ];
            if let Ok(existing) = run_native(home, &info, cancelled)
                && existing.trim() != origin
            {
                return Err("Existing Flatpak ref belongs to another repository".into());
            }
        }
        if arguments.first().is_some_and(|verb| verb == "create-usb") {
            validate_export(arguments, &pinned)?;
        }
        let mut command = Command::new("/usr/bin/flatpak");
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .env("HOME", home)
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env(
                "XDG_CACHE_HOME",
                home.join(".local/share/flatpak/.migration-cache"),
            )
            .env("FLATPAK_USER_DIR", home.join(".local/share/flatpak"))
            .args(arguments);
        let timeout = if arguments
            .first()
            .is_some_and(|verb| matches!(verb.as_str(), "install" | "create-usb"))
        {
            Duration::from_secs(3600)
        } else {
            Duration::from_secs(30)
        };
        let mut process = bounded_command::spawn_prepared(command, None, 4 * 1024 * 1024, timeout)
            .map_err(|error| error.to_string())?;
        let output = loop {
            if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
                drop(process);
                return Err("Flatpak transfer cancelled".into());
            }
            if let Some(result) = process.try_wait() {
                break result.map_err(|_| {
                    "Flatpak operation timed out or exceeded its output limit".to_owned()
                })?;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        pinned.validate().map_err(|error| error.to_string())?;
        if !output.status.success() {
            #[cfg(test)]
            if std::env::var("TILO_NATIVE_FLATPAK_FIXTURE_DIAGNOSTICS").as_deref() == Ok("1") {
                eprintln!(
                    "Synthetic native Flatpak command {:?} failed: {}",
                    arguments,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return Err(format!(
                "Flatpak {} failed (exit {:?})",
                arguments.first().map_or("operation", String::as_str),
                output.status.code()
            ));
        }
        if let Some((origin, reference)) = installation {
            let info = vec![
                "info".into(),
                "--user".into(),
                "--show-origin".into(),
                reference.to_owned(),
            ];
            if run_native(home, &info, cancelled)?.trim() != origin {
                return Err("Installed Flatpak ref does not match the reviewed repository".into());
            }
        }
        String::from_utf8(output.stdout).map_err(|_| "Flatpak returned invalid UTF-8".into())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (home, arguments, cancelled);
        Err("Flatpak migration is supported on Linux".into())
    }
}

#[cfg(target_os = "linux")]
fn install_selection(arguments: &[String]) -> Option<(&str, &str)> {
    if arguments.first()? != "install" {
        return None;
    }
    let reference = arguments.last()?.as_str();
    let origin = arguments.get(arguments.len().checked_sub(2)?)?.as_str();
    Some((origin, reference))
}

#[cfg(target_os = "linux")]
fn validate_export(
    arguments: &[String],
    pinned: &crate::setup::migration::PinnedDirectory,
) -> Result<(), String> {
    let destination = Path::new(
        arguments
            .get(2)
            .ok_or("Flatpak export has no destination")?,
    );
    let output = super::super::migration::PinnedDirectory::open_absolute(destination)
        .map_err(|error| error.to_string())?;
    output
        .validate_destination()
        .map_err(|error| error.to_string())?;
    super::super::destination_security::check_private_file(&output.file)
        .map_err(|error| error.to_string())?;
    if output.contains_identity(&pinned.identity) {
        return Err("Flatpak export must be outside the source home".into());
    }
    let readable = super::super::migration_durability::readable_directory(&output.file)
        .map_err(|error| error.to_string())?;
    if cap_std::fs::Dir::from_std_file(readable)
        .entries()
        .map_err(|error| error.to_string())?
        .next()
        .is_some()
    {
        return Err("Flatpak export destination must be empty".into());
    }
    Ok(())
}

fn verify_repository(
    home: &Path,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let pin = super::super::migration::PinnedDirectory::open_absolute(home)
            .map_err(|error| error.to_string())?;
        pin.validate_destination()
            .map_err(|error| error.to_string())?;
        let metadata = pin.file.metadata().map_err(|error| error.to_string())?;
        // SAFETY: geteuid has no preconditions.
        let uid = rustix::process::geteuid().as_raw();
        if uid == 0 || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
            return Err("Repository verification requires the owner of a private home".into());
        }
        let path = home.join(".local/share/flatpak/repo");
        let repo = super::super::migration::PinnedDirectory::open_absolute(&path)
            .map_err(|error| error.to_string())?;
        repo.validate_destination()
            .map_err(|error| error.to_string())?;
        bindings::verify(&path, cancelled)?;
        let mut command = Command::new("/usr/bin/ostree");
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .arg({
                let mut option = std::ffi::OsString::from("--repo=");
                option.push(path.as_os_str());
                option
            })
            // Flatpak deploy/<logical> GC aliases cannot be passed verbatim to
            // OSTree's blanket binding check. Explicit alias-aware checks above
            // and below retain every logical/collection binding independently.
            .args(["fsck", "--quiet"]);
        let mut process = bounded_command::spawn_prepared(
            command,
            None,
            4 * 1024 * 1024,
            Duration::from_secs(3600),
        )
        .map_err(|_| "Repository verification could not start")?;
        let output = loop {
            if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Relaxed)) {
                return Err("Repository verification cancelled".into());
            }
            if let Some(result) = process.try_wait() {
                break result.map_err(
                    |_| "Repository verification timed out or exceeded its output bound",
                )?;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        pin.validate_destination()
            .map_err(|error| error.to_string())?;
        repo.validate_destination()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            #[cfg(test)]
            if std::env::var("TILO_NATIVE_FLATPAK_FIXTURE_DIAGNOSTICS").as_deref() == Ok("1") {
                eprintln!(
                    "Native fixture repository fsck failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return Err("Repository object integrity or ref binding verification failed".into());
        }
        bindings::verify(&path, cancelled)?;
        pin.validate_destination()
            .map_err(|error| error.to_string())?;
        repo.validate_destination()
            .map_err(|error| error.to_string())?;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (home, cancelled);
        Err("Repository verification requires Linux".into())
    }
}
