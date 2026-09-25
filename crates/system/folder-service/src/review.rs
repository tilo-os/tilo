//! Revalidated copy expectations; identities are comparison hints, never grants.

pub use crate::migration_durability::Identity as DirectoryIdentity;
use crate::{migration::PinnedDirectory, migration_durability};
use std::path::{Component, Path, PathBuf};

pub fn directory_identity(path: &Path) -> Result<migration_durability::Identity, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return Err("continuation requires native absolute paths without traversal".into());
    }
    let pin = PinnedDirectory::open_absolute(path).map_err(|e| e.to_string())?;
    pin.validate().map_err(|e| e.to_string())?;
    // Linux traversal descriptors are O_PATH. Sample the same readable
    // directory capability as the journal, including durable Btrfs identity.
    let readable =
        migration_durability::readable_directory(&pin.file).map_err(|e| e.to_string())?;
    migration_durability::identity(&readable).map_err(|e| e.to_string())
}

pub fn path_is_absent(path: &Path) -> Result<bool, String> {
    let parent = PinnedDirectory::open_absolute(path.parent().ok_or("copy path has no parent")?)
        .map_err(|e| e.to_string())?;
    parent.validate_destination().map_err(|e| e.to_string())?;
    let result = super::nofollow::metadata_nofollow(
        &parent.file,
        Path::new(path.file_name().ok_or("copy path has no name")?),
    );
    parent.validate_destination().map_err(|e| e.to_string())?;
    match result {
        Ok(_) => Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(e) => Err(e.to_string()),
    }
}

pub struct CopyReview {
    parent_identity: migration_durability::Identity,
    source_revision: [u8; 32],
}

impl CopyReview {
    pub fn into_expectations(self) -> (migration_durability::Identity, [u8; 32]) {
        (self.parent_identity, self.source_revision)
    }
}

pub fn inspect_copy_review(source: &Path, destination: &Path) -> Result<CopyReview, String> {
    let parent = destination.parent().ok_or("destination has no parent")?;
    let pin = PinnedDirectory::open_absolute(parent).map_err(|e| e.to_string())?;
    pin.validate_destination().map_err(|e| e.to_string())?;
    Ok(CopyReview {
        parent_identity: directory_identity(parent)?,
        source_revision: super::migration_review::SourceInventory::capture(
            &PinnedDirectory::open_absolute(source).map_err(|e| e.to_string())?,
        )
        .and_then(|inventory| inventory.revision())
        .map_err(|e| e.to_string())?,
    })
}

pub fn prepare_managed_import_parent() -> Result<PathBuf, String> {
    let parent = dirs::data_local_dir()
        .ok_or("No local user data directory")?
        .join("qos/files");
    crate::migration_store::ensure_private_root(&parent).map_err(|e| e.to_string())?;
    Ok(parent)
}
