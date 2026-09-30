//! Device-local, opt-in address profiles. Never part of collaborative history.

use crate::browsing_mode::BrowsingMode;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_BYTES: u64 = 65536;
const MAX_PROFILES: usize = 16;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AddressProfile {
    pub id: String,
    pub label: String,
    pub full_name: String,
    pub given_name: String,
    pub family_name: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal_code: String,
    pub country: String,
}
impl AddressProfile {
    fn valid(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 64
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            && !self.label.trim().is_empty()
            && [
                (&self.label, 128),
                (&self.full_name, 512),
                (&self.given_name, 256),
                (&self.family_name, 256),
                (&self.street, 1024),
                (&self.city, 256),
                (&self.region, 256),
                (&self.postal_code, 64),
                (&self.country, 64),
            ]
            .into_iter()
            .all(|(value, limit)| {
                value.len() <= limit && !value.chars().any(|c| c.is_control() && c != '\n')
            })
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedProfiles {
    enabled: bool,
    profiles: Vec<AddressProfile>,
}
impl SavedProfiles {
    fn valid(&self) -> bool {
        let mut ids = std::collections::BTreeSet::new();
        self.profiles.len() <= MAX_PROFILES
            && self
                .profiles
                .iter()
                .all(|profile| profile.valid() && ids.insert(&profile.id))
    }
}

/// Contains personal data: deliberately has no Debug or collaborative action implementation.
pub struct AutofillStore {
    path: Option<PathBuf>,
    saved: SavedProfiles,
}
impl AutofillStore {
    /// Open normal address profiles, or an empty nonpersistent private store.
    ///
    /// # Errors
    /// Returns an error if the normal profile file cannot be read or exceeds schema or size limits.
    pub fn open(mode: BrowsingMode, path: &Path) -> Result<Self, String> {
        // This return must precede metadata/read calls on a normal profile path.
        if mode.is_private() {
            return Ok(Self {
                path: None,
                saved: SavedProfiles::default(),
            });
        }
        let saved = match std::fs::File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "Cannot read address profiles")?;
                if bytes.len() as u64 > MAX_BYTES {
                    return Err("Address profiles file is too large".into());
                }
                let saved: SavedProfiles = serde_json::from_slice(&bytes)
                    .map_err(|_| "Address profiles file is invalid")?;
                if !saved.valid() {
                    return Err("Address profiles file is invalid".into());
                }
                saved
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => SavedProfiles::default(),
            Err(_) => return Err("Cannot open address profiles".into()),
        };
        Ok(Self {
            path: Some(path.to_owned()),
            saved,
        })
    }
    pub fn enabled(&self) -> bool {
        self.path.is_some() && self.saved.enabled
    }
    pub fn profiles(&self) -> &[AddressProfile] {
        &self.saved.profiles
    }
    pub fn profile_for_fill(&self, id: &str) -> Option<&AddressProfile> {
        self.enabled()
            .then(|| self.saved.profiles.iter().find(|profile| profile.id == id))
            .flatten()
    }
    /// Persist the address-autofill preference.
    ///
    /// # Errors
    /// Returns an error in private mode or if the updated store cannot be saved.
    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), String> {
        let mut next = self.saved.clone();
        next.enabled = enabled;
        self.persist(next)
    }
    /// Save a validated address profile.
    ///
    /// # Errors
    /// Returns an error when autofill is disabled, fields or profile count exceed limits, or persistence fails.
    pub fn put(&mut self, profile: AddressProfile) -> Result<(), String> {
        if !self.enabled() {
            return Err("Enable address autofill before adding profiles".into());
        }
        if !profile.valid() {
            return Err("Address profile fields are invalid or too long".into());
        }
        let mut next = self.saved.clone();
        if let Some(existing) = next.profiles.iter_mut().find(|p| p.id == profile.id) {
            *existing = profile;
        } else {
            if next.profiles.len() >= MAX_PROFILES {
                return Err("Address profile limit reached".into());
            }
            next.profiles.push(profile);
        }
        self.persist(next)
    }
    /// Delete a saved address profile by ID.
    ///
    /// # Errors
    /// Returns an error in private mode or if the updated store cannot be saved.
    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        let mut next = self.saved.clone();
        next.profiles.retain(|p| p.id != id);
        self.persist(next)
    }
    fn persist(&mut self, next: SavedProfiles) -> Result<(), String> {
        let path = self
            .path
            .as_ref()
            .ok_or("Address profiles are unavailable in private windows")?;
        let bytes = serde_json::to_vec(&next).map_err(|_| "Cannot encode address profiles")?;
        if !next.valid() || bytes.len() as u64 > MAX_BYTES {
            return Err("Address profiles exceed storage limits".into());
        }
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or("Address profile directory is unavailable")?;
        std::fs::create_dir_all(parent).map_err(|_| "Cannot create address profile directory")?;
        tilo_files::native::atomic::write_with_mode(path, &bytes, 0o600)
            .map_err(|_| "Cannot save address profiles")?;
        self.saved = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browsing_mode::BrowsingMode;

    fn profile(id: &str) -> AddressProfile {
        AddressProfile {
            id: id.into(),
            label: "Home".into(),
            full_name: "Synthetic Person".into(),
            given_name: "Synthetic".into(),
            family_name: "Person".into(),
            street: "123 Test Street".into(),
            city: "Example".into(),
            region: "CA".into(),
            postal_code: "00000".into(),
            country: "US".into(),
        }
    }

    /// Profile lifecycle: opt-in, private isolation and capacity.
    #[test]
    fn address_profiles_opt_in_edit_capacity_and_private_isolation() {
        // Phase `address_profiles_require_opt_in_and_survive_explicit_edits`.
        {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("addresses.json");
            let mut store = AutofillStore::open(BrowsingMode::Normal, &path).unwrap();
            assert!(!store.enabled());
            assert!(store.put(profile("one")).is_err());
            store.set_enabled(true).unwrap();
            store.put(profile("one")).unwrap();
            store.put(profile("two")).unwrap();
            let mut edited = profile("one");
            edited.city = "Changed".into();
            store.put(edited).unwrap();
            let mut reopened = AutofillStore::open(BrowsingMode::Normal, &path).unwrap();
            assert!(reopened.enabled());
            assert_eq!(reopened.profiles().len(), 2);
            assert_eq!(reopened.profiles()[0].city, "Changed");
            reopened.delete("one").unwrap();
            assert_eq!(
                AutofillStore::open(BrowsingMode::Normal, &path)
                    .unwrap()
                    .profiles()[0]
                    .id,
                "two"
            );
            reopened.set_enabled(false).unwrap();
            assert!(reopened.profile_for_fill("two").is_none());
        }

        // Phase `private_mode_never_reads_or_changes_normal_profiles`.
        {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("addresses.json");
            std::fs::write(&path, "deliberately invalid normal data").unwrap();
            let mut private = AutofillStore::open(BrowsingMode::Private, &path).unwrap();
            assert!(private.profiles().is_empty());
            assert!(private.set_enabled(true).is_err());
            assert!(private.put(profile("one")).is_err());
            assert!(private.delete("one").is_err());
            assert_eq!(
                std::fs::read_to_string(path).unwrap(),
                "deliberately invalid normal data"
            );
        }

        // Phase `profile_capacity_still_allows_editing_and_deleting_existing_entries`.
        {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("addresses.json");
            let mut store = AutofillStore::open(BrowsingMode::Normal, &path).unwrap();
            store.set_enabled(true).unwrap();
            for index in 0..16 {
                store.put(profile(&format!("profile-{index}"))).unwrap();
            }
            assert!(store.put(profile("overflow")).is_err());
            store.put(profile("profile-0")).unwrap();
            store.delete("profile-0").unwrap();
            store.put(profile("replacement")).unwrap();
            assert_eq!(store.profiles().len(), 16);
        }
    }

    /// Rejected input, failed saves and malformed stores.
    #[test]
    fn invalid_fields_failed_saves_and_bad_stores_leave_state_untouched() {
        // Phase `rejects_oversized_or_secret_fields_without_changing_saved_state`.
        {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("addresses.json");
            let mut store = AutofillStore::open(BrowsingMode::Normal, &path).unwrap();
            store.set_enabled(true).unwrap();
            let before = std::fs::read(&path).unwrap();
            let mut invalid = profile("one");
            invalid.street = "x".repeat(1025);
            assert!(store.put(invalid).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            let mut data = serde_json::to_value(profile("one")).unwrap();
            data["password"] = "must never enter this store".into();
            assert!(serde_json::from_value::<AddressProfile>(data).is_err());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }

        // Phase `failed_save_does_not_publish_an_unsaved_profile`.
        {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("addresses.json");
            let mut store = AutofillStore::open(BrowsingMode::Normal, &path).unwrap();
            store.set_enabled(true).unwrap();
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            assert!(store.put(profile("one")).is_err());
            assert!(store.profiles().is_empty());
        }

        // Phase `malformed_or_oversized_store_is_reported_and_left_untouched`.
        {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("addresses.json");
            for bytes in [b"invalid".to_vec(), vec![b' '; 65537]] {
                std::fs::write(&path, &bytes).unwrap();
                assert!(AutofillStore::open(BrowsingMode::Normal, &path).is_err());
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
            }
        }
    }
}
