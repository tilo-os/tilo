//! Restart persistence for authenticated revocations on the existing identity cache.
//! This is monotonic trust state, not a source or replacement grant.
use std::{
    ffi::OsStr,
    fs::OpenOptions,
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory, lease::LeaseMode};
use tilo_user_identity::{IdentityPaths, RevocationEntry, RevocationList};

const CACHE_LIMIT: u64 = 256 * 1024;
const CACHE_NAME: &str = "revocations.bin";
const LEASE_NAME: &str = ".retained-revocations.lease";

pub(super) struct DurableRevocations {
    directory: StorageDirectory,
    person: [u8; 32],
}
impl DurableRevocations {
    pub(super) fn open(paths: &IdentityPaths, person: [u8; 32]) -> io::Result<Self> {
        let cache = crate::identity::revocation_list_path(paths);
        let parent = cache.parent().ok_or_else(super::refused)?;
        // The path is the actual mount's identity selection, never socket input.
        // Retain the admitted parent so later publication cannot follow a swap.
        if !parent.is_absolute() {
            return Err(super::refused());
        }
        for ancestor in parent.ancestors() {
            let metadata = std::fs::symlink_metadata(ancestor)?;
            if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
                return Err(super::refused());
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(parent)?;
        let metadata = file.metadata()?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(super::refused());
        }
        let directory = StorageDirectory::from_file(file)?;
        directory.validate_namespace_control()?;
        Ok(Self { directory, person })
    }

    /// Caller holds the same live revocation writer lock used by network admission.
    /// Learn verified rows in memory before persistence; a persistence failure
    /// refuses this enrollment without undoing already observed revocation.
    pub(super) fn merge_and_publish(
        &self,
        current: &mut RevocationList,
        entries: impl IntoIterator<Item = RevocationEntry>,
    ) -> io::Result<()> {
        self.merge(current, entries)?;
        self.directory.validate_namespace_control()?;
        let _lease = self
            .directory
            .try_acquire_lease(OsStr::new(LEASE_NAME), LeaseMode::Exclusive)
            .map_err(io::Error::other)?;
        match self
            .directory
            .open_read(OsStr::new(CACHE_NAME), Privacy::Inherit)
        {
            Ok(file) => {
                let metadata = file.metadata()?;
                if metadata.uid() != unsafe { libc::geteuid() }
                    || metadata.mode() & 0o022 != 0
                    || metadata.len() > CACHE_LIMIT
                {
                    return Err(super::refused());
                }
                let mut bytes = Vec::new();
                file.take(CACHE_LIMIT.checked_add(1).ok_or_else(super::refused)?)
                    .read_to_end(&mut bytes)?;
                if u64::try_from(bytes.len()).map_err(io::Error::other)? > CACHE_LIMIT {
                    return Err(super::refused());
                }
                let disk: Vec<RevocationEntry> =
                    postcard::from_bytes(&bytes).map_err(io::Error::other)?;
                self.merge(current, disk)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut entries: Vec<_> = current.iter().cloned().collect();
        entries.sort_unstable_by_key(|entry| entry.revoked_device_public_key);
        let bytes = postcard::to_allocvec(&entries).map_err(io::Error::other)?;
        if u64::try_from(bytes.len()).map_err(io::Error::other)? > CACHE_LIMIT {
            return Err(super::refused());
        }
        self.directory
            .publish_with_permissions(
                OsStr::new(CACHE_NAME),
                &bytes,
                PublishMode::Replace,
                Privacy::Inherit,
                0o644,
            )
            .map_err(io::Error::other)
    }

    fn merge(
        &self,
        current: &mut RevocationList,
        entries: impl IntoIterator<Item = RevocationEntry>,
    ) -> io::Result<()> {
        for entry in entries {
            if entry.user_public_key != self.person {
                return Err(super::refused());
            }
            current.insert(entry).map_err(io::Error::other)?;
        }
        if current
            .iter()
            .any(|entry| entry.user_public_key != self.person)
        {
            return Err(super::refused());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{RevocationReason, sign_revocation_entry};

    fn entry(key: &ed25519_dalek::SigningKey, device: u8, at: u64) -> RevocationEntry {
        let mut entry = RevocationEntry {
            user_public_key: key.verifying_key().to_bytes(),
            revoked_device_public_key: [device; 32],
            revoked_at_ns: at,
            reason: RevocationReason::Retired,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, key).unwrap();
        entry
    }

    #[test]
    fn real_cache_publication_merges_disk_and_preserves_earliest_revocation() {
        let directory = tempfile::tempdir().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
        // Lower storage test constructs no enrolled daemon or source grant.
        let sink = DurableRevocations {
            directory: StorageDirectory::open(directory.path()).unwrap(),
            person: key.verifying_key().to_bytes(),
        };
        let mut first = RevocationList::new();
        sink.merge_and_publish(&mut first, [entry(&key, 4, 100)])
            .unwrap();
        let mut restarted = RevocationList::new();
        sink.merge_and_publish(&mut restarted, [entry(&key, 4, 200), entry(&key, 5, 50)])
            .unwrap();
        assert_eq!(restarted.len(), 2);
        assert!(restarted.is_revoked_at(&[4; 32], 100));
        let loaded: Vec<RevocationEntry> =
            postcard::from_bytes(&std::fs::read(directory.path().join(CACHE_NAME)).unwrap())
                .unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(
            loaded
                .iter()
                .any(|entry| entry.revoked_device_public_key == [4; 32]
                    && entry.revoked_at_ns == 100)
        );
    }

    #[test]
    fn corrupt_cache_refuses_signature_path_without_forgetting_learned_revocation() {
        let directory = tempfile::tempdir().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
        let sink = DurableRevocations {
            directory: StorageDirectory::open(directory.path()).unwrap(),
            person: key.verifying_key().to_bytes(),
        };
        std::fs::write(directory.path().join(CACHE_NAME), [0xff]).unwrap();
        let mut live = RevocationList::new();
        assert!(
            sink.merge_and_publish(&mut live, [entry(&key, 6, 80)])
                .is_err()
        );
        assert!(live.is_revoked_at(&[6; 32], 80));
        assert_eq!(
            std::fs::read(directory.path().join(CACHE_NAME)).unwrap(),
            [0xff]
        );
    }
}
