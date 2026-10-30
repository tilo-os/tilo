//! Exact original-attested custody exception translated into a captured view.
//! A serialized token is a claim: the privileged broker must call
//! `validate_original` before passing it to the confined worker.
use super::*;
use serde::{Deserialize, Serialize};

/// One installer path as the setup registered it: root-relative, with the
/// inode, mode, owner and group it had then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Held {
    relative: NativePath,
    stable: (u64, u32, u32, u32),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureCustody {
    /// The installer's preparation namespace, when it lies in this Home.
    preparation: Option<Held>,
    /// The lock of the settings profile the running setup holds in this
    /// Home; it stays behind when the setup exits and is never Home data.
    settings_lock: Option<Held>,
}
impl CaptureCustody {
    pub fn review(home: &Path) -> io::Result<Option<Self>> {
        let preparation = InstallerCustody::review(home)?.map(|custody| Held {
            relative: NativePath::encode(&custody.relative),
            stable: custody.stable,
        });
        let settings_lock = match setup_settings_lock(home) {
            Some(relative) => {
                held_file(&PinnedDirectory::open_absolute(home)?, &relative)?.map(|stable| Held {
                    relative: NativePath::encode(&relative),
                    stable,
                })
            }
            None => None,
        };
        Ok(
            (preparation.is_some() || settings_lock.is_some()).then_some(Self {
                preparation,
                settings_lock,
            }),
        )
    }
    /// Whether this custody names the installer's preparation namespace, whose
    /// independently held capability the broker must then supply.
    pub fn has_preparation(&self) -> bool {
        self.preparation.is_some()
    }
    /// The caller supplies the independently attested acknowledged preparation
    /// namespace capability. An elevated helper's environment grants nothing.
    pub fn validate_original(
        &self,
        home: &Path,
        namespace: Option<&Arc<PinnedDirectory>>,
    ) -> io::Result<()> {
        let home = PinnedDirectory::open_absolute(home)?;
        match (&self.preparation, namespace) {
            (Some(_), Some(namespace)) => {
                namespace.validate()?;
                let (selected, _) = self.captured(&home)?;
                let selected =
                    selected.ok_or_else(|| changed("Approved installer custody is missing"))?;
                if !super::super::nofollow::same_directory_identity(
                    &selected.pin.identity,
                    &namespace.identity,
                ) {
                    return Err(changed("Approved installer custody namespace changed"));
                }
                namespace.validate()?;
            }
            (None, None) => {
                self.captured(&home)?;
            }
            _ => return Err(changed("Approved installer custody namespace differs")),
        }
        if let Some(lock) = &self.settings_lock {
            // Only the Home owner's own lock file may be left out.
            if lock.stable.2 != std::os::unix::fs::MetadataExt::uid(&home.file.metadata()?) {
                return Err(changed("Approved settings lock owner differs"));
            }
        }
        home.validate()
    }
    /// The preparation custody and the setup's own files this custody leaves
    /// out of `home`, each checked against its registered identity there.
    fn captured(
        &self,
        home: &Arc<PinnedDirectory>,
    ) -> io::Result<(Option<Arc<InstallerCustody>>, Vec<PathBuf>)> {
        let preparation = self
            .preparation
            .as_ref()
            .map(|held| Self::captured_preparation(held, home))
            .transpose()?;
        let mut running = Vec::new();
        if let Some(lock) = &self.settings_lock {
            let relative = lock.relative.decode().map_err(io::Error::other)?;
            if !settings_lock_shape(&relative) || held_file(home, &relative)? != Some(lock.stable) {
                return Err(changed("Approved settings lock changed"));
            }
            running.push(relative);
        }
        Ok((preparation, running))
    }
    fn captured_preparation(
        held: &Held,
        home: &Arc<PinnedDirectory>,
    ) -> io::Result<Arc<InstallerCustody>> {
        let relative = held.relative.decode().map_err(io::Error::other)?;
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
            stable: held.stable,
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
        let (custody, running) = match custody {
            Some(value) => value.captured(root)?,
            None => (None, Vec::new()),
        };
        let mut inventory = Self::capture_home_policy(root, custody)?;
        inventory.running = running;
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
        let (custody, running) = match custody {
            Some(value) => value.captured(root)?,
            None => (None, Vec::new()),
        };
        let mut inventory = Self::capture_home_policy(root, custody)?;
        inventory.running = running;
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
        token
            .validate_original(&home, Some(&namespace_pin))
            .unwrap();
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
        assert!(
            token
                .validate_original(&home, Some(&namespace_pin))
                .is_err()
        );
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

    #[test]
    fn captured_view_leaves_out_the_setups_settings_lock_by_its_registered_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().canonicalize().unwrap().join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("HOME", &home);
        environment.set("XDG_STATE_HOME", temporary.path().join("state"));
        environment.set("XDG_CONFIG_HOME", home.join(".config"));
        // The setup holds its settings profile in this Home while it runs.
        let records =
            tilo_settings_records::Records::of(&tilo_settings_records::profile_root().unwrap());
        std::fs::create_dir_all(records.dir()).unwrap();
        let [lock, _] = records.runtime_files();
        std::fs::write(&lock, b"").unwrap();
        std::fs::write(home.join("document"), b"real user document").unwrap();
        let token = CaptureCustody::review(&home).unwrap().unwrap();
        assert!(!token.has_preparation());
        // The privileged side checks the claim without any environment.
        environment.set("XDG_CONFIG_HOME", temporary.path().join("elsewhere"));
        token.validate_original(&home, None).unwrap();
        let pin = PinnedDirectory::open_absolute(&home).unwrap();
        let inventory = SourceInventory::capture_captured_home(&pin, Some(&token)).unwrap();
        assert_eq!(inventory.summary().unwrap().files, 1);
        let relative = lock.strip_prefix(&home).unwrap();
        assert!(inventory.facts.get(relative).unwrap().is_none());
        // Only the settings lock's own place may be claimed.
        let mut forged = token.clone();
        if let Some(held) = &mut forged.settings_lock {
            held.relative = NativePath::encode(Path::new("document"));
        }
        assert!(SourceInventory::capture_captured_home(&pin, Some(&forged)).is_err());
        // A different file in its place is Home data again.
        std::fs::write(records.dir().join("replacement"), b"").unwrap();
        std::fs::rename(records.dir().join("replacement"), &lock).unwrap();
        assert!(SourceInventory::capture_captured_home(&pin, Some(&token)).is_err());
    }
}
