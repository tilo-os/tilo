//! Private restart-index storage. A public path or decoded record is not custody.
use super::{DraftLock, ensure_private_tree, require_supported};
use crate::{destination_security, migration::PinnedDirectory, migration_durability};
use std::{
    ffi::OsStr,
    io::{self, Read},
    path::Path,
    sync::Arc,
};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};

const LIMIT: u64 = 32 * 1024;
const INDEX: &str = "draft.json";

#[derive(Clone)]
pub struct RestartStorage {
    pin: Arc<PinnedDirectory>,
}
fn invalid() -> io::Error {
    io::Error::other("Installer restart storage is invalid; retained preparation was not changed")
}
impl RestartStorage {
    pub fn open(root: &Path) -> io::Result<Self> {
        require_supported()?;
        if !root.is_absolute() || root.file_name() != Some(OsStr::new("preparations")) {
            return Err(invalid());
        }
        let pin = ensure_private_tree(root)?;
        destination_security::check_private_file(&pin.file)?;
        pin.validate_destination()?;
        Ok(Self { pin })
    }
    fn storage(&self) -> io::Result<StorageDirectory> {
        self.pin.validate_destination()?;
        destination_security::check_private_file(&self.pin.file)?;
        StorageDirectory::from_file(migration_durability::readable_directory(&self.pin.file)?)
    }
    pub fn validate(&self) -> io::Result<()> {
        self.storage()?;
        self.pin.validate()
    }
    pub fn lock(&self) -> io::Result<DraftLock> {
        let held = self.storage()?;
        match held.publish(
            OsStr::new("draft.lock"),
            &[],
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        ) {
            Ok(()) => {}
            Err(error) if error.io_error().kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into_io_error()),
        }
        let file = held.open_append_existing(OsStr::new("draft.lock"), Privacy::OwnerOnly)?;
        DraftLock::try_exclusive(file).map_err(io::Error::other)
    }
    pub fn read(&self) -> io::Result<Option<Vec<u8>>> {
        let held = self.storage()?;
        let file = match held.open_read(OsStr::new(INDEX), Privacy::OwnerOnly) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > LIMIT {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            return Err(invalid());
        }
        self.pin.validate()?;
        Ok(Some(bytes))
    }
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.len() as u64 > LIMIT {
            return Err(invalid());
        }
        self.storage()?
            .publish(
                OsStr::new(INDEX),
                bytes,
                PublishMode::Replace,
                Privacy::OwnerOnly,
            )
            .map_err(tilo_files::native::atomic::PublicationError::into_io_error)?;
        self.pin.validate()
    }
    pub fn create_operation(&self, operation: &str) -> io::Result<()> {
        // Restart operations have exactly one canonical UUID component.
        if uuid::Uuid::parse_str(operation)
            .map_err(|_| invalid())?
            .to_string()
            != operation
        {
            return Err(invalid());
        }
        let child = self.pin.create_child(OsStr::new(operation))?;
        migration_durability::sync_directory(&child.file)?;
        migration_durability::sync_directory(&self.pin.file)
    }
}

#[cfg(all(test, not(unix)))]
mod tests {
    use super::*;
    #[test]
    fn unavailable_restart_storage_does_not_create_a_namespace() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("preparations");
        assert_eq!(
            RestartStorage::open(&root).err().unwrap().kind(),
            io::ErrorKind::Unsupported
        );
        assert!(!root.exists());
    }
}
