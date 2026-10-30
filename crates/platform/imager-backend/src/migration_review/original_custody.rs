//! Exact installer-only omissions authenticated by original source registration.
use super::*;
use serde::{Deserialize, Serialize};
use std::{fs::File, os::unix::fs::MetadataExt};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Kind {
    Preparation,
    SourceControl,
    /// The lock of the settings profile the setup held in this Home; a file
    /// that stays behind when the setup exits and is never Home data.
    SettingsLock,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalOmission {
    kind: Kind,
    relative: NativePath,
    facts: (u64, u64, u32, u32, u32),
}
#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalPreparation {
    namespace: NativePath,
    omission: OriginalOmission,
    settings_lock: Option<OriginalOmission>,
}
impl<'de> Deserialize<'de> for OriginalOmission {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            kind: Kind,
            relative: NativePath,
            facts: (u64, u64, u32, u32, u32),
        }
        let fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            kind: fields.kind,
            relative: fields.relative,
            facts: fields.facts,
        })
    }
}
impl<'de> Deserialize<'de> for OriginalPreparation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Fields {
            namespace: NativePath,
            omission: OriginalOmission,
            settings_lock: Option<OriginalOmission>,
        }
        let fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            namespace: fields.namespace,
            omission: fields.omission,
            settings_lock: fields.settings_lock,
        })
    }
}
pub(super) struct OriginalCustody {
    pin: Arc<PinnedDirectory>,
    pub(super) relative: PathBuf,
    proof: OriginalOmission,
}
impl OriginalCustody {
    pub(super) fn validate(&self) -> io::Result<()> {
        self.pin.validate()?;
        if facts(&self.pin.file)? != self.proof.facts {
            return Err(changed("Original omission identity changed"));
        }
        self.proof.validate_shape()
    }
}
impl OriginalPreparation {
    /// Only the original preparation process resolves its own state root.
    pub fn review(home: &Path) -> io::Result<Option<Self>> {
        let Some(held) = InstallerCustody::review(home)? else {
            return Ok(None);
        };
        let namespace = crate::session::preparation_namespace()
            .ok_or_else(|| changed("Preparation namespace unavailable"))?;
        held.validate()?;
        let root = PinnedDirectory::open_absolute(home)?;
        let settings_lock = match setup_settings_lock(home) {
            Some(relative) if held_file(&root, &relative)?.is_some() => {
                let file = open_plain_file(&root.file, &relative)?;
                Some(OriginalOmission::from_held(
                    Kind::SettingsLock,
                    &relative,
                    &file,
                )?)
            }
            _ => None,
        };
        let value = Self {
            namespace: NativePath::encode(&namespace),
            omission: OriginalOmission::from_held(
                Kind::Preparation,
                &held.relative,
                &held.pin.file,
            )?,
            settings_lock,
        };
        value.validate_original(home, rustix::process::getuid().as_raw())?;
        Ok(Some(value))
    }
    /// Every omission this preparation registers, checked in `home`.
    pub fn validate_original(&self, home: &Path, caller: u32) -> io::Result<Vec<OriginalOmission>> {
        if self.omission.kind != Kind::Preparation || self.omission.facts.3 != caller {
            return Err(changed("Original preparation owner differs"));
        }
        let root = PinnedDirectory::open_absolute(home)?;
        if let Some(lock) = &self.settings_lock {
            if lock.kind != Kind::SettingsLock || lock.facts.3 != caller {
                return Err(changed("Original settings lock owner differs"));
            }
            lock.open(&root.file)?;
        }
        let selected = self.omission.open(&root.file)?;
        let namespace =
            PinnedDirectory::open_absolute(&self.namespace.decode().map_err(io::Error::other)?)?;
        if facts(&selected)? != facts(&namespace.file)? {
            return Err(changed("Original preparation capability differs"));
        }
        root.validate()?;
        namespace.validate()?;
        Ok(std::iter::once(self.omission.clone())
            .chain(self.settings_lock.clone())
            .collect())
    }
}
/// The identity of a held omission; its registered shape decides whether it
/// has to be a directory or a file.
fn facts(file: &File) -> io::Result<(u64, u64, u32, u32, u32)> {
    let m = file.metadata()?;
    if !m.is_dir() && !m.is_file() {
        return Err(changed(
            "Original omission is neither a directory nor a file",
        ));
    }
    Ok((m.dev(), m.ino(), m.mode(), m.uid(), m.gid()))
}
/// Open the plain file at `relative` below `home`, one no-follow edge at a time.
fn open_plain_file(home: &File, relative: &Path) -> io::Result<File> {
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    let mut parts: Vec<_> = relative.components().collect();
    let name = parts
        .pop()
        .ok_or_else(|| changed("Original omission has no name"))?;
    let mut directory = home.try_clone()?;
    for part in parts {
        let part = Path::new(part.as_os_str());
        let m = metadata_nofollow(&directory, part)?;
        directory = super::super::nofollow::open_directory_nofollow(&directory, part, &m)?;
    }
    let mut options = cap_primitives::fs::OpenOptions::new();
    options
        .read(true)
        .follow(cap_primitives::fs::FollowSymlinks::No)
        .nonblock(true);
    let file = cap_primitives::fs::open(&directory, Path::new(name.as_os_str()), &options)?;
    if !file.metadata()?.is_file() {
        return Err(changed("Original settings lock is not a plain file"));
    }
    Ok(file)
}
impl OriginalOmission {
    fn from_held(kind: Kind, relative: &Path, held: &File) -> io::Result<Self> {
        let value = Self {
            kind,
            relative: NativePath::encode(relative),
            facts: facts(held)?,
        };
        value.validate_shape()?;
        Ok(value)
    }
    /// The exact root-held Store namespace is excluded only when contained.
    pub fn source_control(home: &Path, records: &Path, held: &File) -> io::Result<Option<Self>> {
        if rustix::process::geteuid().as_raw() != 0
            || !records.ends_with("var/lib/tilo-imager/home-capture/records")
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let Ok(relative) = records.strip_prefix(home) else {
            return Ok(None);
        };
        let value = Self::from_held(Kind::SourceControl, relative, held)?;
        let root = PinnedDirectory::open_absolute(home)?;
        value.open(&root.file)?;
        root.validate()?;
        Ok(Some(value))
    }
    pub fn validate_shape(&self) -> io::Result<()> {
        let p = self.relative.decode().map_err(io::Error::other)?;
        let suffix = match self.kind {
            Kind::Preparation => "tilo/installer/preparations",
            Kind::SourceControl => "tilo-imager/home-capture/records",
            Kind::SettingsLock => return self.validate_lock_shape(&p),
        };
        if p.as_os_str().is_empty()
            || p.as_os_str().len() > 4096
            || p.components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            || !p.ends_with(suffix)
            || self.facts.0 == 0
            || self.facts.1 == 0
            || self.facts.2 & libc::S_IFMT != libc::S_IFDIR
            || self.facts.2 & 0o077 != 0
            || self.kind == Kind::SourceControl && self.facts.3 != 0
        {
            return Err(changed(
                "Original omission is not an exact private installer directory",
            ));
        }
        Ok(())
    }
    /// A settings lock: the plain file the settings records keep, at a
    /// bounded path of plain names.
    fn validate_lock_shape(&self, relative: &Path) -> io::Result<()> {
        if !settings_lock_shape(relative)
            || self.facts.0 == 0
            || self.facts.1 == 0
            || self.facts.2 & libc::S_IFMT != libc::S_IFREG
        {
            return Err(changed(
                "Original omission is not the exact settings lock of its Home",
            ));
        }
        Ok(())
    }
    fn open(&self, home: &File) -> io::Result<File> {
        self.open_identity(home, false)
    }
    fn open_identity(&self, home: &File, translated_device: bool) -> io::Result<File> {
        self.validate_shape()?;
        let relative = self.relative.decode().map_err(io::Error::other)?;
        let file = if self.kind == Kind::SettingsLock {
            open_plain_file(home, &relative)?
        } else {
            let mut file = home.try_clone()?;
            for name in relative.components() {
                let name = Path::new(name.as_os_str());
                let m = metadata_nofollow(&file, name)?;
                file = super::super::nofollow::open_directory_nofollow(&file, name, &m)?;
            }
            file
        };
        let observed = facts(&file)?;
        let mut expected = self.facts;
        if translated_device {
            expected.0 = home.metadata()?.dev();
        }
        if observed.0 != home.metadata()?.dev()
            || fs_native::snapshot_lease::mount_id(&file)?
                != fs_native::snapshot_lease::mount_id(home)?
            || observed != expected
        {
            return Err(changed("Original installer omission changed"));
        }
        Ok(file)
    }
    /// Only the root original-volume resolver may translate a saved st_dev.
    /// That kernel number is not portable across boots; inode and owner facts
    /// remain exact on the same authenticated, held, readonly source volume.
    pub fn read_projection(values: &[Self], home: &File) -> io::Result<Vec<Self>> {
        use std::os::fd::AsRawFd;
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        Self::validate_shapes(values)?;
        let mut flags: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(home.as_raw_fd(), &raw mut flags) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags.f_flag & libc::ST_RDONLY == 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        values
            .iter()
            .map(|value| {
                let held = value.open_identity(home, true)?;
                Self::from_held(
                    value.kind,
                    &value.relative.decode().map_err(io::Error::other)?,
                    &held,
                )
            })
            .collect()
    }
    pub fn validate_set(values: &[Self], home: &File, owner: u32) -> io::Result<()> {
        Self::validate_shapes(values)?;
        for value in values {
            value.open(home)?;
            if matches!(value.kind, Kind::Preparation | Kind::SettingsLock)
                && value.facts.3 != owner
            {
                return Err(changed("Original omission owner differs"));
            }
        }
        Ok(())
    }
    pub fn validate_shapes(values: &[Self]) -> io::Result<()> {
        if values.len() > 3 {
            return Err(changed("Original omission count exceeds bound"));
        }
        for (index, value) in values.iter().enumerate() {
            value.validate_shape()?;
            if values[..index].iter().any(|other| other.kind == value.kind) {
                return Err(changed("Original omission kind repeated"));
            }
        }
        Ok(())
    }
    fn custody(&self, home: &Arc<PinnedDirectory>) -> io::Result<Arc<OriginalCustody>> {
        let relative = self.relative.decode().map_err(io::Error::other)?;
        let mut pin = home.clone();
        for name in relative.components() {
            let m = metadata_nofollow(&pin.file, Path::new(name.as_os_str()))?;
            pin = pin.open_child(name.as_os_str(), &m)?;
        }
        if facts(&pin.file)? != self.facts {
            return Err(changed("Original omission projection differs"));
        }
        Ok(Arc::new(OriginalCustody {
            pin,
            relative,
            proof: self.clone(),
        }))
    }
}
impl SourceInventory {
    /// Leave the registered omissions out: installer directories as custody,
    /// the setup's settings lock as one of its own files, each checked
    /// against its registered identity in `root`.
    fn admit_omissions(
        &mut self,
        root: &Arc<PinnedDirectory>,
        values: &[OriginalOmission],
    ) -> io::Result<()> {
        for value in values {
            if value.kind == Kind::SettingsLock {
                value.open(&root.file)?;
                self.running
                    .push(value.relative.decode().map_err(io::Error::other)?);
            } else {
                self.original_custody.push(value.custody(root)?);
            }
        }
        Ok(())
    }
    /// Metadata-discovery policy from protected original registration, without
    /// deriving privileged HOME/XDG or enumerating mounted contents.
    pub fn original_home_policy(
        root: &Arc<PinnedDirectory>,
        values: &[OriginalOmission],
    ) -> io::Result<Self> {
        OriginalOmission::validate_set(values, &root.file, root.file.metadata()?.uid())?;
        let mut inventory = Self::capture_home_policy(root, None)?;
        inventory.admit_omissions(root, values)?;
        Ok(inventory)
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn capture_original_home(
        root: &Arc<PinnedDirectory>,
        values: &[OriginalOmission],
    ) -> io::Result<Self> {
        OriginalOmission::validate_set(values, &root.file, root.file.metadata()?.uid())?;
        let mut inventory = Self::capture_home_policy(root, None)?;
        inventory.admit_omissions(root, values)?;
        inventory.walk(root, Path::new(""), 0)?;
        Ok(inventory)
    }
    #[cfg(feature = "capture-worker")]
    pub fn capture_projected_original_home_with_scratch(
        root: &Arc<PinnedDirectory>,
        values: &[OriginalOmission],
        scratch: Arc<super::InventoryScratch>,
        roots: Arc<launcher::owned_service::VerifiedOriginalMountedRoots>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        roots.check()?;
        scratch.validate_source(root)?;
        OriginalOmission::validate_set(values, &root.file, root.file.metadata()?.uid())?;
        let mut inventory = Self::capture_home_policy(root, None)?;
        inventory.facts = super::storage::Storage::with_scratch(scratch);
        inventory.admit_omissions(root, values)?;
        inventory.projected_roots = Some(roots);
        inventory.walk_with_cancel(root, Path::new(""), 0, cancel)?;
        inventory.facts.finish(cancel)?;
        inventory
            .projected_roots
            .as_ref()
            .ok_or_else(|| changed("Original projections disappeared"))?
            .check()?;
        Ok(inventory)
    }
    #[cfg(any(
        feature = "capture-worker",
        all(test, feature = "native-capture-qualification")
    ))]
    pub fn capture_original_home_with_scratch(
        root: &Arc<PinnedDirectory>,
        values: &[OriginalOmission],
        scratch: Arc<super::InventoryScratch>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        scratch.validate_source(root)?;
        OriginalOmission::validate_set(values, &root.file, root.file.metadata()?.uid())?;
        let mut inventory = Self::capture_home_policy(root, None)?;
        inventory.facts = super::storage::Storage::with_scratch(scratch);
        inventory.admit_omissions(root, values)?;
        inventory.walk_with_cancel(root, Path::new(""), 0, cancel)?;
        inventory.facts.finish(cancel)?;
        Ok(inventory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};
    #[cfg(feature = "native-capture-qualification")]
    #[test]
    fn original_inventory_uses_held_scratch_for_capture_validation_and_cancellation() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let root = std::env::var_os("TILO_FS_NATIVE_STORE")
            .expect("an explicitly owned supported native scratch store is required");
        let owned = tempfile::Builder::new()
            .prefix("inventory-capture-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(root)
            .unwrap();
        let parent = PinnedDirectory::open_absolute(owned.path()).unwrap();
        let home = parent.create_child(std::ffi::OsStr::new("home")).unwrap();
        let scratch_pin = parent
            .create_child(std::ffi::OsStr::new("scratch"))
            .unwrap();
        let scratch =
            super::super::InventoryScratch::review(scratch_pin, 32 * 1024 * 1024).unwrap();
        let app = owned.path().join("home/.var/app/org.tilo.Fixture");
        fs::create_dir_all(&app).unwrap();
        for index in 0..2048 {
            fs::write(
                app.join(format!("state-{index}")),
                b"actual application state",
            )
            .unwrap();
        }
        let document = owned.path().join("home/document");
        fs::write(&document, b"original document").unwrap();
        let cancel = AtomicBool::new(false);
        let inventory = SourceInventory::capture_original_home_with_scratch(
            &home,
            &[],
            scratch.clone(),
            &cancel,
        )
        .unwrap();
        assert_eq!(inventory.summary().unwrap().files, 2049);
        inventory.validate_with_cancel(&home, &cancel).unwrap();
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            inventory
                .validate_with_cancel(&home, &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        cancel.store(false, Ordering::Relaxed);
        fs::write(&document, b"changed original document").unwrap();
        assert!(inventory.validate_with_cancel(&home, &cancel).is_err());
        assert_eq!(fs::read(document).unwrap(), b"changed original document");
        assert!(
            fs::read_dir(owned.path().join("scratch"))
                .unwrap()
                .next()
                .is_none()
        );
        let source_names = fs::read_dir(&app).unwrap().count();
        assert_eq!(source_names, 2048);
        drop(inventory);
        drop(scratch);
    }
    #[test]
    fn original_omission_refuses_arbitrary_private_paths_and_duplicate_claims() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        fs::create_dir(&home).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("HOME", &home);
        environment.set("XDG_STATE_HOME", home.join(".local/state"));
        let namespace = home.join(".local/state/tilo/installer/preparations");
        fs::create_dir_all(&namespace).unwrap();
        fs::set_permissions(&namespace, fs::Permissions::from_mode(0o700)).unwrap();
        let proof = OriginalPreparation::review(&home)
            .unwrap()
            .unwrap()
            .validate_original(&home, rustix::process::getuid().as_raw())
            .unwrap()
            .remove(0);
        assert!(OriginalOmission::validate_shapes(&[proof.clone(), proof.clone()]).is_err());
        let mut forged = serde_json::to_value(&proof).unwrap();
        forged["relative"] =
            serde_json::to_value(NativePath::encode(Path::new(".hidden/private"))).unwrap();
        let forged: OriginalOmission = serde_json::from_value(forged).unwrap();
        assert!(forged.validate_shape().is_err());
        let mut forged = proof;
        forged.facts.0 += 1;
        assert!(
            OriginalOmission::validate_set(
                &[forged],
                &File::open(home).unwrap(),
                rustix::process::getuid().as_raw(),
            )
            .is_err()
        );
    }
    #[test]
    fn preparation_outside_selected_home_is_not_an_omission() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir(&home).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("HOME", &home);
        environment.set("XDG_STATE_HOME", temp.path().join("outside"));
        assert!(OriginalPreparation::review(&home).unwrap().is_none());
    }
    #[test]
    fn original_capture_leaves_out_the_setups_settings_lock_by_its_registered_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().canonicalize().unwrap().join("home");
        fs::create_dir(&home).unwrap();
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("HOME", &home);
        environment.set("XDG_STATE_HOME", home.join(".local/state"));
        environment.set("XDG_CONFIG_HOME", home.join(".config"));
        let namespace = home.join(".local/state/tilo/installer/preparations");
        fs::create_dir_all(&namespace).unwrap();
        fs::set_permissions(&namespace, fs::Permissions::from_mode(0o700)).unwrap();
        // The setup held its settings profile in this Home; its lock stays.
        let records =
            tilo_settings_records::Records::of(&tilo_settings_records::profile_root().unwrap());
        fs::create_dir_all(records.dir()).unwrap();
        let [lock, _] = records.runtime_files();
        fs::write(&lock, b"").unwrap();
        fs::write(records.dir().join("person.automerge"), b"real settings").unwrap();
        fs::write(home.join("document"), b"real document").unwrap();
        let proofs = OriginalPreparation::review(&home)
            .unwrap()
            .unwrap()
            .validate_original(&home, rustix::process::getuid().as_raw())
            .unwrap();
        assert_eq!(proofs.len(), 2);
        // The reader resolves nothing from its own environment.
        environment.set("XDG_CONFIG_HOME", temporary.path().join("elsewhere"));
        let pin = PinnedDirectory::open_absolute(&home).unwrap();
        let inventory = SourceInventory::capture_original_home(&pin, &proofs).unwrap();
        let relative = lock.strip_prefix(&home).unwrap();
        assert!(inventory.facts.get(relative).unwrap().is_none());
        assert_eq!(
            inventory.summary().unwrap().files,
            2,
            "the document and settings"
        );
        // Only the settings lock's own place may be claimed.
        let mut forged = serde_json::to_value(&proofs[1]).unwrap();
        forged["relative"] =
            serde_json::to_value(NativePath::encode(Path::new("document"))).unwrap();
        let forged: OriginalOmission = serde_json::from_value(forged).unwrap();
        assert!(forged.validate_shape().is_err());
        // A different file in its place is Home data again.
        fs::write(records.dir().join("replacement"), b"").unwrap();
        fs::rename(records.dir().join("replacement"), &lock).unwrap();
        assert!(SourceInventory::capture_original_home(&pin, &proofs).is_err());
    }
}
