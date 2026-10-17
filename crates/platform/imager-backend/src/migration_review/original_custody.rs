//! Exact installer-only omissions authenticated by original source registration.
use super::*;
use serde::{Deserialize, Serialize};
use std::{fs::File, os::unix::fs::MetadataExt};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Kind {
    Preparation,
    SourceControl,
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
        }
        let fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            namespace: fields.namespace,
            omission: fields.omission,
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
        let value = Self {
            namespace: NativePath::encode(&namespace),
            omission: OriginalOmission::from_held(
                Kind::Preparation,
                &held.relative,
                &held.pin.file,
            )?,
        };
        value.validate_original(home, rustix::process::getuid().as_raw())?;
        Ok(Some(value))
    }
    pub fn validate_original(&self, home: &Path, caller: u32) -> io::Result<OriginalOmission> {
        if self.omission.kind != Kind::Preparation || self.omission.facts.3 != caller {
            return Err(changed("Original preparation owner differs"));
        }
        let root = PinnedDirectory::open_absolute(home)?;
        let selected = self.omission.open(&root.file)?;
        let namespace =
            PinnedDirectory::open_absolute(&self.namespace.decode().map_err(io::Error::other)?)?;
        if facts(&selected)? != facts(&namespace.file)? {
            return Err(changed("Original preparation capability differs"));
        }
        root.validate()?;
        namespace.validate()?;
        Ok(self.omission.clone())
    }
}
fn facts(file: &File) -> io::Result<(u64, u64, u32, u32, u32)> {
    let m = file.metadata()?;
    if !m.is_dir() {
        return Err(changed("Original omission is not a directory"));
    }
    Ok((m.dev(), m.ino(), m.mode(), m.uid(), m.gid()))
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
    fn open(&self, home: &File) -> io::Result<File> {
        self.open_identity(home, false)
    }
    fn open_identity(&self, home: &File, translated_device: bool) -> io::Result<File> {
        self.validate_shape()?;
        let mut file = home.try_clone()?;
        for name in self
            .relative
            .decode()
            .map_err(io::Error::other)?
            .components()
        {
            let name = Path::new(name.as_os_str());
            let m = metadata_nofollow(&file, name)?;
            file = super::super::nofollow::open_directory_nofollow(&file, name, &m)?;
        }
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
            if value.kind == Kind::Preparation && value.facts.3 != owner {
                return Err(changed("Original omission owner differs"));
            }
        }
        Ok(())
    }
    pub fn validate_shapes(values: &[Self]) -> io::Result<()> {
        if values.len() > 2 {
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
    /// Metadata-discovery policy from protected original registration, without
    /// deriving privileged HOME/XDG or enumerating mounted contents.
    pub fn original_home_policy(
        root: &Arc<PinnedDirectory>,
        values: &[OriginalOmission],
    ) -> io::Result<Self> {
        OriginalOmission::validate_set(values, &root.file, root.file.metadata()?.uid())?;
        let mut inventory = Self::capture_home_policy(root, None)?;
        inventory.original_custody = values
            .iter()
            .map(|v| v.custody(root))
            .collect::<io::Result<_>>()?;
        Ok(inventory)
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn capture_original_home(
        root: &Arc<PinnedDirectory>,
        values: &[OriginalOmission],
    ) -> io::Result<Self> {
        OriginalOmission::validate_set(values, &root.file, root.file.metadata()?.uid())?;
        let mut inventory = Self::capture_home_policy(root, None)?;
        inventory.original_custody = values
            .iter()
            .map(|v| v.custody(root))
            .collect::<io::Result<_>>()?;
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
        inventory.original_custody = values
            .iter()
            .map(|value| value.custody(root))
            .collect::<io::Result<_>>()?;
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
        inventory.original_custody = values
            .iter()
            .map(|v| v.custody(root))
            .collect::<io::Result<_>>()?;
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
            .unwrap();
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
}
