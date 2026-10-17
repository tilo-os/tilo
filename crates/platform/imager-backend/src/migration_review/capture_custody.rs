//! Exact original-attested custody exception translated into a captured view.
//! A serialized token is a claim: the privileged broker must call
//! `validate_original` before passing it to the confined worker.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureCustody {
    relative: NativePath,
    stable: (u64, u32, u32, u32),
}
impl CaptureCustody {
    pub fn review(home: &Path) -> io::Result<Option<Self>> {
        InstallerCustody::review(home).map(|custody| {
            custody.map(|custody| Self {
                relative: NativePath::encode(&custody.relative),
                stable: custody.stable,
            })
        })
    }
    /// The caller supplies the independently attested acknowledged preparation
    /// namespace capability. An elevated helper's environment grants nothing.
    pub fn validate_original(
        &self,
        home: &Path,
        namespace: &Arc<PinnedDirectory>,
    ) -> io::Result<()> {
        namespace.validate()?;
        let home = PinnedDirectory::open_absolute(home)?;
        let selected = self.captured(&home)?;
        if !super::super::nofollow::same_directory_identity(
            &selected.pin.identity,
            &namespace.identity,
        ) {
            return Err(changed("Approved installer custody namespace changed"));
        }
        namespace.validate()?;
        home.validate()
    }
    fn captured(&self, home: &Arc<PinnedDirectory>) -> io::Result<Arc<InstallerCustody>> {
        let relative = self.relative.decode().map_err(io::Error::other)?;
        if !relative.ends_with(Path::new("tilo/installer/preparations"))
            || relative.as_os_str().is_empty()
            || relative.as_os_str().len() > 4096
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(changed(
                "Captured custody must be an exact bounded relative directory",
            ));
        }
        let mut pin = home.clone();
        for name in relative.components() {
            let expected = metadata_nofollow(&pin.file, Path::new(name.as_os_str()))?;
            pin = pin.open_child(name.as_os_str(), &expected)?;
        }
        super::super::destination_security::check_private_file(&pin.file)?;
        let custody = Arc::new(InstallerCustody {
            pin,
            relative,
            stable: self.stable,
            mutable_ancestors: false,
        });
        custody.validate()?;
        Ok(custody)
    }
}

#[cfg(any(feature = "capture-worker", test))]
impl SourceInventory {
    #[cfg(test)]
    pub fn capture_captured_home(
        root: &Arc<PinnedDirectory>,
        custody: Option<&CaptureCustody>,
    ) -> io::Result<Self> {
        let custody = custody.map(|value| value.captured(root)).transpose()?;
        let mut inventory = Self::capture_home_policy(root, custody)?;
        inventory.walk(root, Path::new(""), 0)?;
        Ok(inventory)
    }
    #[cfg(feature = "capture-worker")]
    pub fn capture_captured_home_with_scratch(
        root: &Arc<PinnedDirectory>,
        custody: Option<&CaptureCustody>,
        scratch: Arc<super::InventoryScratch>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        scratch.validate_source(root)?;
        let custody = custody.map(|value| value.captured(root)).transpose()?;
        let mut inventory = Self::capture_home_policy(root, custody)?;
        inventory.facts = super::storage::Storage::with_scratch(scratch);
        inventory.walk_with_cancel(root, Path::new(""), 0, cancel)?;
        inventory.facts.finish(cancel)?;
        Ok(inventory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn captured_exclusion_is_exact_identity_bound_and_ancestors_remain_inventoried() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().canonicalize().unwrap().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("HOME", &home);
        environment.set("XDG_STATE_HOME", home.join(".local/state"));
        let namespace = crate::session::preparation_namespace().unwrap();
        std::fs::create_dir_all(&namespace).unwrap();
        std::fs::set_permissions(&namespace, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            namespace.join("prepared-secret"),
            b"separate installer custody",
        )
        .unwrap();
        std::fs::create_dir_all(home.join(".hidden/app")).unwrap();
        std::fs::write(home.join(".hidden/app/settings"), b"real user state").unwrap();
        std::fs::write(home.join("document"), b"real user document").unwrap();
        let token = CaptureCustody::review(&home).unwrap().unwrap();
        let namespace_pin = PinnedDirectory::open_absolute(&namespace).unwrap();
        // Elevation changes the helper's environment. Authority remains the
        // independently held original namespace, never these ambient values.
        environment.set("HOME", temporary.path().join("different-helper-home"));
        environment.set(
            "XDG_STATE_HOME",
            temporary.path().join("different-helper-state"),
        );
        token.validate_original(&home, &namespace_pin).unwrap();
        let pin = PinnedDirectory::open_absolute(&home).unwrap();
        // This tests the actual selected-view verifier on a held source, not a
        // claimed native snapshot; Btrfs epoch behavior has separate native tests.
        let inventory = SourceInventory::capture_captured_home(&pin, Some(&token)).unwrap();
        assert_eq!(inventory.summary().unwrap().files, 2);
        assert!(
            inventory
                .facts
                .get(Path::new(".hidden/app/settings"))
                .unwrap()
                .is_some()
        );
        assert!(
            inventory
                .facts
                .get(
                    &namespace
                        .strip_prefix(&home)
                        .unwrap()
                        .join("prepared-secret")
                )
                .unwrap()
                .is_none()
        );
        assert!(
            inventory
                .facts
                .get(Path::new(".local"))
                .unwrap()
                .unwrap()
                .snapshot
                .is_some()
        );
        std::fs::rename(&namespace, namespace.with_extension("retired")).unwrap();
        std::fs::create_dir(&namespace).unwrap();
        std::fs::set_permissions(&namespace, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(token.validate_original(&home, &namespace_pin).is_err());
        assert!(SourceInventory::capture_captured_home(&pin, Some(&token)).is_err());
        assert_eq!(
            std::fs::read(home.join("document")).unwrap(),
            b"real user document"
        );
        assert_eq!(
            std::fs::read(home.join(".hidden/app/settings")).unwrap(),
            b"real user state"
        );
    }
}
