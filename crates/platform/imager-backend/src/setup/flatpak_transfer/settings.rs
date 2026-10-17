//! Read-only disclosure of source administrator settings. Presence is not an
//! effective permission diff: later user layers can cancel these settings.
use super::{Installation, InstalledRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsDomain {
    Folders,
    Network,
    Devices,
    Services,
    Environment,
    Other,
}
impl SettingsDomain {
    pub fn key(self) -> &'static str {
        match self {
            Self::Folders => "installer-flatpak-settings-folders",
            Self::Network => "installer-flatpak-settings-network",
            Self::Devices => "installer-flatpak-settings-devices",
            Self::Services => "installer-flatpak-settings-services",
            Self::Environment => "installer-flatpak-settings-environment",
            Self::Other => "installer-flatpak-settings-other",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsLayer {
    pub sha256: [u8; 32],
    pub domains: Vec<SettingsDomain>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdministratorSettings {
    pub installed: InstalledRef,
    pub global: Option<SettingsLayer>,
    pub application: Option<SettingsLayer>,
}
impl AdministratorSettings {
    pub fn domains(&self) -> Vec<SettingsDomain> {
        self.global
            .iter()
            .chain(self.application.iter())
            .flat_map(|layer| layer.domains.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}
#[cfg(target_os = "linux")]
pub(super) fn validate(
    notices: Option<&[AdministratorSettings]>,
    refs: &[InstalledRef],
) -> Result<(), String> {
    let Some(notices) = notices else {
        return Ok(());
    };
    if notices.len() > 4096 {
        return Err("Administrator settings inventory exceeds bounds".into());
    }
    let mut seen = BTreeSet::new();
    for notice in notices {
        if notice.installed.app_id().is_none()
            || notice.installed.installation == Installation::User
            || !refs.contains(&notice.installed)
            || !seen.insert((
                notice.installed.installation.clone(),
                notice.installed.reference.clone(),
            ))
        {
            return Err("Administrator settings lack exact application selection".into());
        }
        for layer in notice.global.iter().chain(notice.application.iter()) {
            if layer.sha256 == [0; 32]
                || layer
                    .domains
                    .iter()
                    .zip(layer.domains.iter().skip(1))
                    .any(|(left, right)| left >= right)
            {
                return Err("Invalid administrator settings layer".into());
            }
        }
    }
    // Known data covers every system app in the supplied reviewed inventory.
    if refs
        .iter()
        .filter(|item| item.app_id().is_some() && item.installation != Installation::User)
        .count()
        != notices.len()
    {
        return Err("Administrator settings inventory is incomplete".into());
    }
    Ok(())
}
#[cfg(target_os = "linux")]
pub(super) fn review(
    runner: &impl super::Runner,
    home: &std::path::Path,
    refs: &[InstalledRef],
) -> Result<Vec<AdministratorSettings>, String> {
    if refs
        .iter()
        .filter(|item| item.app_id().is_some() && item.installation != Installation::User)
        .count()
        > 4096
    {
        return Err("Administrator settings inventory exceeds bounds".into());
    }
    let inventory = super::Inventory {
        refs: refs.to_vec(),
        ..Default::default()
    };
    let roots = super::installation_roots(runner, home, &inventory)?;
    let mut result = Vec::new();
    for (scope, root) in roots {
        if scope != Installation::User {
            let apps: Vec<_> = refs
                .iter()
                .filter(|item| item.installation == scope && item.app_id().is_some())
                .cloned()
                .collect();
            result.extend(review_root(&root, &apps).map_err(|error| error.to_string())?);
        }
    }
    validate(Some(&result), refs)?;
    Ok(result)
}
#[cfg(all(test, target_os = "linux"))]
fn verify(root: &std::path::Path, expected: &[AdministratorSettings]) -> Result<(), String> {
    let apps: Vec<_> = expected
        .iter()
        .map(|notice| notice.installed.clone())
        .collect();
    if review_root(root, &apps).map_err(|error| error.to_string())? != expected {
        return Err("Original administrator settings changed after review".into());
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn review_root(
    root: &std::path::Path,
    apps: &[InstalledRef],
) -> std::io::Result<Vec<AdministratorSettings>> {
    use crate::setup::migration::PinnedDirectory;
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    use std::path::Path;
    let pin = PinnedDirectory::open_absolute(root)?;
    pin.validate()?;
    let mut options = OpenOptions::new();
    options.read(true).nonblock(true).follow(FollowSymlinks::No);
    let directory = match cap_primitives::fs::open(&pin.file, Path::new("overrides"), &options) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(directory) = &directory {
        use std::os::unix::fs::MetadataExt;
        let m = directory.metadata()?;
        if !m.is_dir() || m.uid() != pin.file.metadata()?.uid() || m.mode() & 0o022 != 0 {
            return Err(std::io::ErrorKind::PermissionDenied.into());
        }
    }
    let global = directory
        .as_ref()
        .map(|directory| read_layer(directory, "global"))
        .transpose()?
        .flatten();
    let mut notices = Vec::new();
    for app in apps {
        let id = app.app_id().ok_or(std::io::ErrorKind::InvalidInput)?;
        if !super::token(id) {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let application = directory
            .as_ref()
            .map(|directory| read_layer(directory, id))
            .transpose()?
            .flatten();
        notices.push(AdministratorSettings {
            installed: app.clone(),
            global: global.clone(),
            application,
        });
    }
    // Reopen only the same fixed child to detect directory replacement or
    // creation after an absence review. Never follow a substituted link.
    let current = match cap_primitives::fs::open(&pin.file, Path::new("overrides"), &options) {
        Ok(file) => Some(file),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    use std::os::unix::fs::MetadataExt;
    match (&directory, current) {
        (None, None) => {}
        (Some(before), Some(after))
            if before.metadata()?.dev() == after.metadata()?.dev()
                && before.metadata()?.ino() == after.metadata()?.ino() => {}
        _ => {
            return Err(std::io::Error::other(
                "Administrator settings directory changed during review",
            ));
        }
    }
    pin.validate()?;
    Ok(notices)
}
#[cfg(target_os = "linux")]
fn read_layer(directory: &std::fs::File, name: &str) -> std::io::Result<Option<SettingsLayer>> {
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    use sha2::{Digest, Sha256};
    use std::{io::Read, os::unix::fs::MetadataExt, path::Path};
    let mut options = OpenOptions::new();
    options.read(true).nonblock(true).follow(FollowSymlinks::No);
    let mut file = match cap_primitives::fs::open(directory, Path::new(name), &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != directory.metadata()?.uid()
        || before.mode() & 0o022 != 0
        || before.len() > 256 * 1024
    {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(256 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() > 256 * 1024
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(std::io::Error::other(
            "Administrator settings changed during review",
        ));
    }
    let named = cap_primitives::fs::open(directory, Path::new(name), &options)?.metadata()?;
    if named.dev() != before.dev()
        || named.ino() != before.ino()
        || u64::try_from(bytes.len()).map_err(std::io::Error::other)? != before.len()
    {
        return Err(std::io::Error::other(
            "Administrator settings file changed during review",
        ));
    }
    let text = std::str::from_utf8(&bytes).map_err(std::io::Error::other)?;
    Ok(Some(SettingsLayer {
        sha256: Sha256::digest(&bytes).into(),
        domains: domains(text)?,
    }))
}
#[cfg(target_os = "linux")]
fn domains(text: &str) -> std::io::Result<Vec<SettingsDomain>> {
    let mut section = "";
    let mut domains = BTreeSet::new();
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
        {
            section = name;
            continue;
        }
        let (key, _) = line
            .split_once('=')
            .ok_or_else(|| std::io::Error::other("Malformed administrator settings"))?;
        domains.insert(match (section, key.trim()) {
            ("Context", "filesystems" | "persistent") => SettingsDomain::Folders,
            ("Context", "shared") => SettingsDomain::Network,
            ("Context", "devices" | "sockets") => SettingsDomain::Devices,
            ("Session Bus Policy" | "System Bus Policy", _) => SettingsDomain::Services,
            ("Environment", _) => SettingsDomain::Environment,
            _ => SettingsDomain::Other,
        });
    }
    Ok(domains.into_iter().collect())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    fn app() -> InstalledRef {
        InstalledRef {
            reference: "app/org.tilo.Settings/x86_64/stable".into(),
            origin: "approved".into(),
            installation: Installation::System("named".into()),
            name: "Settings fixture".into(),
        }
    }
    #[test]
    fn exact_source_settings_bind_app_scope_and_detect_changed_or_removed_bytes() {
        let root = tempfile::tempdir().unwrap();
        let overrides = root.path().join("overrides");
        std::fs::create_dir(&overrides).unwrap();
        let path = overrides.join("org.tilo.Settings");
        std::fs::write(
            overrides.join("global"),
            "[Context]\nshared=!network;ipc;\n",
        )
        .unwrap();
        std::fs::write(&path,"[Context]\nfilesystems=!home;/private/synthetic:ro;\nsockets=if:wayland:has-wayland;\n[Environment]\nPRIVATE_VALUE=never project this\n").unwrap();
        let reviewed = review_root(root.path(), &[app()]).unwrap();
        validate(Some(&reviewed), &[app()]).unwrap();
        assert_eq!(
            reviewed.first().unwrap().domains(),
            vec![
                SettingsDomain::Folders,
                SettingsDomain::Network,
                SettingsDomain::Devices,
                SettingsDomain::Environment
            ]
        );
        let public = serde_json::to_string(&reviewed).unwrap();
        assert!(!public.contains("/private/synthetic"));
        assert!(!public.contains("never project"));
        verify(root.path(), &reviewed).unwrap();
        std::fs::write(&path, "[Context]\nfilesystems=!host;\n").unwrap();
        assert!(verify(root.path(), &reviewed).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(verify(root.path(), &reviewed).is_err());
        let mut wrong = app();
        wrong.installation = Installation::System("other".into());
        assert!(validate(Some(&reviewed), &[wrong]).is_err());
    }
    #[test]
    fn absent_is_checked_but_symlink_and_unsafe_settings_are_not_none_found() {
        let root = tempfile::tempdir().unwrap();
        let original = review_root(root.path(), &[app()]).unwrap();
        assert!(original.first().unwrap().global.is_none());
        assert!(original.first().unwrap().application.is_none());
        verify(root.path(), &original).unwrap();
        std::fs::create_dir(root.path().join("overrides")).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), root.path().join("overrides/global")).unwrap();
        assert!(review_root(root.path(), &[app()]).is_err());
        std::fs::remove_file(root.path().join("overrides/global")).unwrap();
        std::fs::write(
            root.path().join("overrides/global"),
            "[Context]\nshared=network;\n",
        )
        .unwrap();
        std::fs::set_permissions(
            root.path().join("overrides/global"),
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        assert!(review_root(root.path(), &[app()]).is_err());
        assert!(validate(None, &[app()]).is_ok());
        assert!(
            validate(Some(&[]), &[app()]).is_err(),
            "known inventory must cover each system app"
        );
    }
    #[test]
    fn override_directory_fifo_refuses_without_waiting_for_a_writer() {
        use std::{
            os::unix::{
                ffi::OsStrExt,
                fs::{FileTypeExt, MetadataExt},
            },
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        const SUBJECT: &str = "TILO_SETTINGS_FIFO_FIXTURE";
        const SELECTOR: &str = "setup::flatpak_transfer::settings::tests::override_directory_fifo_refuses_without_waiting_for_a_writer";
        if let Some(path) = std::env::var_os(SUBJECT) {
            let root = std::path::PathBuf::from(path);
            let pin = crate::setup::migration::PinnedDirectory::open_absolute(&root).unwrap();
            let owner = pin.file.metadata().unwrap();
            // SAFETY: geteuid only observes this fixture child's credentials.
            assert_eq!(owner.uid(), rustix::process::geteuid().as_raw());
            assert_eq!(owner.mode() & 0o777, 0o700);
            assert!(
                std::fs::symlink_metadata(root.join("overrides"))
                    .unwrap()
                    .file_type()
                    .is_fifo()
            );
            let started = Instant::now();
            assert!(review_root(&root, &[app()]).is_err());
            assert!(started.elapsed() < Duration::from_secs(1));
            std::fs::write(root.join("fifo-refused"), b"actual-review-refused").unwrap();
            return;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let name =
            std::ffi::CString::new(root.path().join("overrides").as_os_str().as_bytes()).unwrap();
        // SAFETY: the terminated path names only this fixture's private directory.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", SELECTOR, "--nocapture"])
            .env(SUBJECT, root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "FIFO refusal child failed: {status}");
                assert_eq!(
                    std::fs::read(root.path().join("fifo-refused")).unwrap(),
                    b"actual-review-refused",
                    "the exact child must execute the actual review, not select zero tests"
                );
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!(
                    "Override directory FIFO blocked actual review; watchdog killed and reaped child"
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
