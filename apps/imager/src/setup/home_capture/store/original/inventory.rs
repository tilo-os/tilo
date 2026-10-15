//! Exact immutable sibling inventory in the already admitted root Store.
use super::*;
use crate::setup::migration_handoff::{INVENTORY_BOUND, OriginalInventoryBinding};
use std::os::unix::fs::MetadataExt;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InventoryCustody {
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    pub(super) len: u64,
    pub(super) sha256: [u8; 32],
}
impl InventoryCustody {
    pub(super) fn validate_selection(plan: &Plan, proof: Option<&Self>) -> io::Result<()> {
        match (OriginalInventoryBinding::parse(plan)?, proof) {
            (None, None) => Ok(()),
            (Some(binding), Some(proof))
                if proof.owner == 0
                    && proof.inode != 0
                    && proof.mode.trailing_zeros() >= 6
                    && binding.len == proof.len
                    && binding.sha256 == proof.sha256 =>
            {
                Ok(())
            }
            _ => Err(failure("Protected original inventory selection differs")),
        }
    }
    pub(super) fn register(
        storage: &StorageDirectory,
        plan: &Plan,
        source: Option<&File>,
    ) -> io::Result<Option<Self>> {
        let binding = OriginalInventoryBinding::parse(plan)?;
        let (binding, source) = match (binding, source) {
            (None, None) => return Ok(None),
            (Some(binding), Some(file)) => (binding, file),
            _ => {
                return Err(failure(
                    "Original inventory registration missing or invented",
                ));
            }
        };
        let before = source.metadata()?;
        if !before.is_file()
            || before.uid() != plan.home_uid()
            || before.nlink() != 1
            || before.mode() & 0o077 != 0
            || before.len() != binding.len
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let bytes = read(source)?;
        binding.validate_bytes(&bytes)?;
        let after = source.metadata()?;
        if stamp(&before) != stamp(&after) {
            return Err(failure("Original caller inventory changed"));
        }
        storage.validate_privacy(Privacy::OwnerOnly)?;
        let name = format!("{}.flatpak-inventory.json", plan.operation_id);
        match storage.open_read(OsStr::new(&name), Privacy::OwnerOnly) {
            Ok(file) => {
                if read(&file)? != bytes {
                    return Err(failure("Protected inventory retry differs"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                storage
                    .publish(
                        OsStr::new(&name),
                        &bytes,
                        PublishMode::IfAbsent,
                        Privacy::OwnerOnly,
                    )
                    .map_err(io::Error::other)?;
            }
            Err(error) => return Err(error),
        }
        let file = storage.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
        let facts = file.metadata()?;
        if facts.uid() != 0 || facts.nlink() != 1 || !facts.is_file() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let proof = Self {
            device: facts.dev(),
            inode: facts.ino(),
            owner: facts.uid(),
            group: facts.gid(),
            mode: facts.mode(),
            len: facts.len(),
            sha256: binding.sha256,
        };
        proof.verify(plan, &file, facts.dev())?;
        Ok(Some(proof))
    }
    pub(super) fn verify(&self, plan: &Plan, file: &File, current_device: u64) -> io::Result<()> {
        Self::validate_selection(plan, Some(self))?;
        let facts = file.metadata()?;
        // st_dev changes across readonly boot mounts. Only the exact same
        // authenticated source volume as the protected record may translate it.
        if !facts.is_file()
            || facts.nlink() != 1
            || facts.dev() != current_device
            || (
                facts.ino(),
                facts.uid(),
                facts.gid(),
                facts.mode(),
                facts.len(),
            ) != (self.inode, self.owner, self.group, self.mode, self.len)
        {
            return Err(failure("Protected original inventory inode differs"));
        }
        OriginalInventoryBinding::parse(plan)?
            .ok_or_else(|| failure("Protected inventory binding missing"))?
            .validate_bytes(&read(file)?)
    }
}
fn read(file: &File) -> io::Result<Vec<u8>> {
    // pread avoids shared descriptor offset races with launcher custody checks.
    use std::os::unix::fs::FileExt;
    let before = file.metadata()?;
    if before.len() == 0 || before.len() > INVENTORY_BOUND {
        return Err(failure("Protected inventory bound differs"));
    }
    let mut bytes = vec![0u8; usize::try_from(before.len()).map_err(io::Error::other)?];
    file.read_exact_at(&mut bytes, 0)?;
    if stamp(&before) != stamp(&file.metadata()?) {
        return Err(failure("Protected inventory changed while reading"));
    }
    Ok(bytes)
}
fn stamp(metadata: &std::fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    )
}
