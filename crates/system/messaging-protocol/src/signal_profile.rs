//! Sealed host-owned Signal profile images, outside shared records.
//!
//! The host supplies its existing key and original account authority. Beginning
//! use leaves a durable marker before any profile bytes are returned. An
//! interrupted lease never restores an older image. Checkpoints accept only
//! the private image issued by that same live store; no key is generated here.
//! This is the storage boundary, not a provider process or quiescence proof.
use crate::{Service, journal::Owner, supervisor::AccountAuthority};
use advisory_lock::FileLock;
use secret_vault::{VaultFormat, VaultKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
use zeroize::{Zeroize, Zeroizing};

const MAX_FILES: usize = 4096;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_SEALED_BYTES: u64 = 320 * 1024 * 1024;
const FORMAT: VaultFormat = VaultFormat {
    name: "tilo.signal-profile.v1",
    magic: b"TMSP",
    secret_service_label: "Tilo Signal profile",
    secret_service_attributes: &[
        ("application", "so.tilo.messaging-broker"),
        ("purpose", "signal-profile"),
    ],
    dpapi_entropy: b"tilo.signal-profile.v1",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileError {
    Unavailable,
    Revoked,
    Busy,
    Interrupted,
    InvalidImage,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contents {
    version: u32,
    owner: Owner,
    files: BTreeMap<String, Vec<u8>>,
}
impl Drop for Contents {
    fn drop(&mut self) {
        for (mut path, mut bytes) in std::mem::take(&mut self.files) {
            path.zeroize();
            bytes.zeroize();
        }
    }
}

/// Private, noncloneable, nonserializable contents of one account's lease.
/// Apps receive neither this object nor its encryption key.
pub struct ProfileImage {
    contents: Contents,
    authority: Arc<dyn AccountAuthority>,
    issued: Arc<()>,
}
impl ProfileImage {
    /// # Errors
    /// Refuses retired original authority; paths are exact, never prefix matches.
    pub fn file(&self, path: &str) -> Result<Option<&[u8]>, ProfileError> {
        check(&self.authority)?;
        Ok(self.contents.files.get(path).map(Vec::as_slice))
    }
    /// Visit this private image without cloning or exposing its map. The native
    /// runtime owner uses this to materialize a working profile; every file and
    /// the completed visit recheck the original authority.
    /// # Errors
    /// Refuses retired authority or propagates a visitor's storage failure.
    pub fn visit_files(
        &self,
        mut visit: impl FnMut(&str, &[u8]) -> Result<(), ProfileError>,
    ) -> Result<(), ProfileError> {
        check(&self.authority)?;
        for (path, bytes) in &self.contents.files {
            check(&self.authority)?;
            visit(path, bytes)?;
        }
        check(&self.authority)
    }
    /// # Errors
    /// Refuses retired authority, nonrelative paths or bounded-image overflow.
    pub fn put(&mut self, path: String, mut bytes: Zeroizing<Vec<u8>>) -> Result<(), ProfileError> {
        check(&self.authority)?;
        if !valid_path(&path) || bytes.len() > MAX_BYTES {
            return Err(ProfileError::InvalidImage);
        }
        let previous = self.contents.files.get(&path).map_or(0, Vec::len);
        let total = total_bytes(&self.contents.files)?
            .checked_sub(previous)
            .and_then(|size| size.checked_add(bytes.len()))
            .ok_or(ProfileError::InvalidImage)?;
        if total > MAX_BYTES
            || (self.contents.files.len() >= MAX_FILES && !self.contents.files.contains_key(&path))
        {
            return Err(ProfileError::InvalidImage);
        }
        if let Some(mut old) = self
            .contents
            .files
            .insert(path, std::mem::take(&mut *bytes))
        {
            old.zeroize();
        }
        Ok(())
    }
    /// # Errors
    /// Refuses retired authority. Removed secret bytes are cleared immediately.
    pub fn remove(&mut self, path: &str) -> Result<bool, ProfileError> {
        check(&self.authority)?;
        if let Some((mut path, mut bytes)) = self.contents.files.remove_entry(path) {
            path.zeroize();
            bytes.zeroize();
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// One exclusive encrypted profile owner. Marker retirement follows durable
/// ciphertext publication; a storage failure does not authorize another use.
pub struct EncryptedSignalProfile {
    root: PathBuf,
    key: VaultKey,
    owner: Owner,
    contents: Option<Contents>,
    authority: Arc<dyn AccountAuthority>,
    issued: Arc<()>,
    usable: bool,
    _lock: FileLock,
}
impl EncryptedSignalProfile {
    /// Explicit first-profile creation under a key already authorized by the
    /// host. Existing, unreadable or interrupted profiles are never replaced.
    /// # Errors
    /// Refuses wrong ownership, absent admission, competing writers or storage failure.
    pub fn create_with_key(
        root: &Path,
        owner: Owner,
        key: VaultKey,
        authority: Arc<dyn AccountAuthority>,
    ) -> Result<Self, ProfileError> {
        let mut store = Self::locked(root, owner, key, authority)?;
        if present(&root.join("profile.enc"))? || present(&root.join("active.enc"))? {
            return Err(ProfileError::Unavailable);
        }
        let contents = Contents {
            version: 1,
            owner: store.owner.clone(),
            files: BTreeMap::new(),
        };
        let sealed = store.seal(&contents)?;
        if !tilo_files::native::private::create_atomic(&root.join("profile.enc"), &sealed)
            .map_err(storage)?
        {
            return Err(ProfileError::Unavailable);
        }
        check(&store.authority)?;
        store.contents = Some(contents);
        Ok(store)
    }
    /// Open existing ciphertext only. Missing keys/data, interrupted use and
    /// another person/device/account/generation refuse without a reset.
    /// # Errors
    /// Refuses invalid custody, original authority, framing, ownership or storage.
    pub fn open_with_key(
        root: &Path,
        owner: Owner,
        key: VaultKey,
        authority: Arc<dyn AccountAuthority>,
    ) -> Result<Self, ProfileError> {
        let mut store = Self::locked(root, owner, key, authority)?;
        if present(&root.join("active.enc"))? {
            return Err(ProfileError::Interrupted);
        }
        let (file, _) = tilo_files::native::private::open_owned_for_read(
            &root.join("profile.enc"),
            MAX_SEALED_BYTES,
        )
        .map_err(storage)?;
        let mut sealed = Vec::new();
        file.take(MAX_SEALED_BYTES.saturating_add(1))
            .read_to_end(&mut sealed)
            .map_err(storage)?;
        if u64::try_from(sealed.len()).map_err(storage)? > MAX_SEALED_BYTES {
            return Err(ProfileError::Unavailable);
        }
        let plain = FORMAT
            .open(&store.key, &sealed)
            .ok_or(ProfileError::Unavailable)?;
        let contents: Contents = serde_json::from_slice(&plain).map_err(storage)?;
        validate(&contents, &store.owner)?;
        check(&store.authority)?;
        store.contents = Some(contents);
        Ok(store)
    }
    fn locked(
        root: &Path,
        owner: Owner,
        key: VaultKey,
        authority: Arc<dyn AccountAuthority>,
    ) -> Result<Self, ProfileError> {
        check(&authority)?;
        if !owner.valid_principal()
            || owner.service != Service::Signal
            || !crate::identifier(&owner.device)
            || !crate::identifier(&owner.account)
            || owner.linking_generation == 0
        {
            return Err(ProfileError::Unavailable);
        }
        tilo_files::native::private::directory(root).map_err(storage)?;
        let file =
            tilo_files::native::private::lock_file(&root.join("owner.lock")).map_err(storage)?;
        let lock = FileLock::try_exclusive(file).map_err(|_| ProfileError::Busy)?;
        check(&authority)?;
        Ok(Self {
            root: root.into(),
            key,
            owner,
            contents: None,
            authority,
            issued: Arc::new(()),
            usable: true,
            _lock: lock,
        })
    }
    /// Begin one use, durably refusing a competing/interrupted use before
    /// exposing plaintext. Dropping the returned image preserves the marker.
    /// # Errors
    /// Refuses retired authority, an outstanding lease or persistence failure.
    pub fn begin_use(&mut self) -> Result<ProfileImage, ProfileError> {
        check(&self.authority)?;
        if !self.usable || self.contents.is_none() {
            return Err(ProfileError::Interrupted);
        }
        let marker = Zeroizing::new(serde_json::to_vec(&self.owner).map_err(storage)?);
        let marker = FORMAT
            .seal(&self.key, &marker)
            .ok_or(ProfileError::Unavailable)?;
        if !tilo_files::native::private::create_atomic(&self.root.join("active.enc"), &marker)
            .map_err(storage)?
        {
            self.usable = false;
            return Err(ProfileError::Interrupted);
        }
        // A failure after marker publication stays interrupted, even if no
        // plaintext escaped. Never guess that an old crypto image is current.
        check(&self.authority)?;
        let contents = self.contents.take().ok_or(ProfileError::Interrupted)?;
        Ok(ProfileImage {
            contents,
            authority: self.authority.clone(),
            issued: self.issued.clone(),
        })
    }
    /// Seal the exact issued in-memory image and retire its marker. The
    /// runtime owner must establish writer quiescence before assembling it.
    /// This seal-only cleanup also works after admission retirement: it reads
    /// no new credential and returns no plaintext or revived authority.
    /// # Errors
    /// Refuses another lease, invalid contents or failed durable publication.
    pub fn checkpoint(&mut self, image: ProfileImage) -> Result<(), ProfileError> {
        if !self.usable || self.contents.is_some() || !Arc::ptr_eq(&image.issued, &self.issued) {
            return Err(ProfileError::Interrupted);
        }
        self.usable = false;
        validate(&image.contents, &self.owner)?;
        let sealed = self.seal(&image.contents)?;
        // Authenticate the bounded, privately opened marker before publishing
        // or removing anything. Tampering leaves this lease interrupted.
        let (file, _) =
            tilo_files::native::private::open_owned_for_read(&self.root.join("active.enc"), 4096)
                .map_err(storage)?;
        let mut marker = Vec::new();
        file.take(4097).read_to_end(&mut marker).map_err(storage)?;
        let marker = FORMAT
            .open(&self.key, &marker)
            .ok_or(ProfileError::Unavailable)?;
        let marker: Owner = serde_json::from_slice(&marker).map_err(storage)?;
        if marker != self.owner {
            return Err(ProfileError::InvalidImage);
        }
        tilo_files::native::private::write_atomic(&self.root.join("profile.enc"), &sealed)
            .map_err(storage)?;
        std::fs::remove_file(self.root.join("active.enc")).map_err(storage)?;
        tilo_files::native::private::sync_directory(&self.root).map_err(storage)?;
        self.contents = Some(image.contents);
        self.usable = true;
        Ok(())
    }
    /// Seal-only replacement of the exact issued lease after the native host
    /// proves every working-profile writer stopped. This returns no plaintext
    /// and reads no credential, including when original authority has retired.
    /// The storage layer does not construct a runtime quiescence witness.
    /// # Errors
    /// Refuses foreign leases, duplicate/invalid paths or an oversized image.
    pub fn checkpoint_files(
        &mut self,
        mut image: ProfileImage,
        files: impl IntoIterator<Item = (String, Zeroizing<Vec<u8>>)>,
    ) -> Result<(), ProfileError> {
        if !self.usable || self.contents.is_some() || !Arc::ptr_eq(&image.issued, &self.issued) {
            return Err(ProfileError::Interrupted);
        }
        let mut contents = Contents {
            version: 1,
            owner: self.owner.clone(),
            files: BTreeMap::new(),
        };
        let mut total = 0usize;
        for (path, mut bytes) in files {
            total = total
                .checked_add(bytes.len())
                .ok_or(ProfileError::InvalidImage)?;
            if !valid_path(&path)
                || total > MAX_BYTES
                || contents.files.len() >= MAX_FILES
                || contents.files.contains_key(&path)
            {
                return Err(ProfileError::InvalidImage);
            }
            contents.files.insert(path, std::mem::take(&mut *bytes));
        }
        validate(&contents, &self.owner)?;
        image.contents = contents;
        self.checkpoint(image)
    }
    fn seal(&self, contents: &Contents) -> Result<Vec<u8>, ProfileError> {
        let plain = Zeroizing::new(serde_json::to_vec(contents).map_err(storage)?);
        if u64::try_from(plain.len()).map_err(storage)? >= MAX_SEALED_BYTES {
            return Err(ProfileError::InvalidImage);
        }
        FORMAT
            .seal(&self.key, &plain)
            .ok_or(ProfileError::Unavailable)
    }
}
fn check(authority: &Arc<dyn AccountAuthority>) -> Result<(), ProfileError> {
    authority.check().map_err(|_| ProfileError::Revoked)
}
fn storage<T>(_: T) -> ProfileError {
    ProfileError::Unavailable
}
fn present(path: &Path) -> Result<bool, ProfileError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(ProfileError::Unavailable),
    }
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 1024
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path.split('/').count() <= 32
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}
fn total_bytes(files: &BTreeMap<String, Vec<u8>>) -> Result<usize, ProfileError> {
    files.values().try_fold(0usize, |size, bytes| {
        size.checked_add(bytes.len())
            .ok_or(ProfileError::InvalidImage)
    })
}
fn validate(contents: &Contents, owner: &Owner) -> Result<(), ProfileError> {
    if contents.version != 1
        || &contents.owner != owner
        || contents.files.len() > MAX_FILES
        || !contents.files.keys().all(|path| valid_path(path))
        || total_bytes(&contents.files)? > MAX_BYTES
    {
        return Err(ProfileError::InvalidImage);
    }
    Ok(())
}

#[cfg(test)]
#[path = "signal_profile_tests.rs"]
mod tests;
