//! Authenticated archive metadata only; never source or filesystem authority.
use super::*;
use crate::setup::migration_review::storage::{IndexValue, SortedIndex};

#[derive(Clone, Default)]
pub struct Scratch {
    #[cfg(target_os = "linux")]
    lease: Option<Arc<crate::setup::migration_review::InventoryScratch>>,
    #[cfg(target_os = "linux")]
    existing: Option<Arc<PinnedDirectory>>,
}
impl Scratch {
    #[cfg(target_os = "linux")]
    pub fn retained(lease: Arc<crate::setup::migration_review::InventoryScratch>) -> Self {
        Self {
            lease: Some(lease),
            existing: None,
        }
    }
    #[cfg(target_os = "linux")]
    pub fn verification_only(
        lease: Arc<crate::setup::migration_review::InventoryScratch>,
        existing: Arc<PinnedDirectory>,
    ) -> io::Result<Self> {
        lease.validate_existing_output(&existing)?;
        Ok(Self {
            lease: Some(lease),
            existing: Some(existing),
        })
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Indexed {
    ordinal: usize,
    explicit_acl: bool,
    record: Record,
}
impl IndexValue for Indexed {
    // Existing sparse/PAX policy allows an individual metadata witness larger
    // than ordinary inode Facts. The sort buffer separately has a byte bound.
    const RECORD_BYTES: usize = 32 * 1024 * 1024;
}
pub(in crate::setup::portable_transfer) struct Records {
    index: SortedIndex<Indexed>,
    scratch: Scratch,
}
impl Records {
    pub(in crate::setup::portable_transfer) fn new(scratch: Scratch) -> Self {
        #[cfg(target_os = "linux")]
        if let Some(lease) = scratch.lease.clone() {
            return Self {
                index: SortedIndex::with_scratch(lease),
                scratch,
            };
        }
        Self {
            index: SortedIndex::new(),
            scratch,
        }
    }
    pub(in crate::setup::portable_transfer) fn len(&self) -> usize {
        self.index.len()
    }
    pub(in crate::setup::portable_transfer) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(in crate::setup::portable_transfer) fn push(
        &mut self,
        record: Record,
        explicit_acl: bool,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let ordinal = self.len();
        self.index.insert(
            record.path.clone(),
            Indexed {
                ordinal,
                explicit_acl,
                record,
            },
            cancel,
        )
    }
    pub(in crate::setup::portable_transfer) fn finish(
        &mut self,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        self.index.finish(cancel)?;
        // No extraction is permitted until the AEAD footer AND this pass finish.
        for entry in self.index.iter() {
            check_cancel(cancel)?;
            let (path, indexed) = entry?;
            if path != indexed.record.path {
                return Err(changed("Archive index path differs"));
            }
            if !path.as_os_str().is_empty() {
                let parent = self
                    .index
                    .get(path.parent().unwrap_or(Path::new("")))?
                    .ok_or_else(|| changed("Archive parent is missing or is a link"))?;
                if parent.ordinal >= indexed.ordinal
                    || !matches!(parent.record.kind, Kind::Directory)
                {
                    return Err(changed("Archive parent is missing or is a link"));
                }
            }
            if let Kind::HardLink(target) = &indexed.record.kind {
                let original = self
                    .index
                    .get(target)?
                    .ok_or_else(|| changed("Hardlink target is not an earlier regular file"))?;
                if original.ordinal >= indexed.ordinal
                    || !matches!(original.record.kind, Kind::File(_) | Kind::Sparse(_))
                {
                    return Err(changed("Hardlink target is not an earlier regular file"));
                }
                if indexed.explicit_acl && indexed.record.acls != original.record.acls {
                    return Err(changed("Hardlink carries conflicting POSIX ACL metadata"));
                }
            }
        }
        self.hold_restoration(cancel)
    }
    #[cfg(target_os = "linux")]
    pub(in crate::setup::portable_transfer) fn working_storage(
        &self,
    ) -> Option<Arc<crate::setup::migration_review::InventoryScratch>> {
        self.scratch.lease.clone()
    }
    pub(in crate::setup::portable_transfer) fn require_restoration(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if self.scratch.existing.is_some() {
            return Err(changed(
                "Existing-output review cannot authorize fresh restoration",
            ));
        }
        self.check_restoration()
    }
    pub(in crate::setup::portable_transfer) fn check_existing_root(
        &self,
        root: &Arc<PinnedDirectory>,
    ) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(existing) = &self.scratch.existing {
            let lease = self
                .scratch
                .lease
                .as_ref()
                .ok_or_else(|| changed("Existing archive workspace grant missing"))?;
            lease.validate_existing_output(existing)?;
            lease.validate_existing_output(root)?;
            if durability::identity(&durability::readable_directory(&existing.file)?)?
                != durability::identity(&durability::readable_directory(&root.file)?)?
            {
                return Err(changed("Existing archive output root changed"));
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = root;
        Ok(())
    }
    pub(in crate::setup::portable_transfer) fn check_restoration(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(lease) = &self.scratch.lease {
            lease.check_restoration()?;
        }
        Ok(())
    }
    fn hold_restoration(&self, cancel: &AtomicBool) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(lease) = &self.scratch.lease {
            if let Some(existing) = &self.scratch.existing {
                // This mode only authenticates metadata and verifies the held
                // candidate. It never permits extracting another payload.
                lease.validate_existing_output(existing)?;
                lease.check_restoration()?;
                return Ok(());
            }
            if let Some(block) = lease.restoration_block() {
                let round = |bytes: u64| -> io::Result<u64> {
                    bytes
                        .checked_add(block - 1)
                        .map(|n| n / block * block)
                        .ok_or_else(|| changed("Authenticated allocation reserve overflows"))
                };
                let mut bytes = 0u64;
                for record in self.iter() {
                    check_cancel(cancel)?;
                    let record = record?;
                    let data = match &record.kind {
                        Kind::File(_) => round(record.size)?,
                        Kind::Sparse(witness) => witness.sparse.allocation_bytes(block)?,
                        _ => 0,
                    };
                    bytes = bytes
                        .checked_add(data)
                        .ok_or_else(|| changed("Authenticated allocation reserve overflows"))?;
                    for (_, value) in &record.xattrs {
                        bytes = bytes
                            .checked_add(round(
                                u64::try_from(value.len()).map_err(io::Error::other)?,
                            )?)
                            .ok_or_else(|| changed("Authenticated metadata reserve overflows"))?;
                    }
                    for acl in record.acls.iter().flatten() {
                        bytes = bytes
                            .checked_add(round(
                                u64::try_from(acl.len()).map_err(io::Error::other)?,
                            )?)
                            .ok_or_else(|| changed("Authenticated ACL reserve overflows"))?;
                    }
                }
                lease.hold_authenticated_restoration(bytes, self.len())?;
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = cancel;
        Ok(())
    }
    fn resolved(&self, indexed: Indexed) -> io::Result<Record> {
        let mut record = indexed.record;
        if let Kind::HardLink(target) = &record.kind {
            record.acls = self
                .index
                .get(target)?
                .ok_or_else(|| changed("Hardlink ACL target missing"))?
                .record
                .acls;
        }
        Ok(record)
    }
    pub(in crate::setup::portable_transfer) fn get(
        &self,
        path: &Path,
    ) -> io::Result<Option<Record>> {
        self.index
            .get(path)?
            .map(|indexed| self.resolved(indexed))
            .transpose()
    }
    pub(in crate::setup::portable_transfer) fn contains(&self, path: &Path) -> io::Result<bool> {
        Ok(self.index.get(path)?.is_some())
    }
    pub(in crate::setup::portable_transfer) fn iter(
        &self,
    ) -> impl Iterator<Item = io::Result<Record>> + '_ {
        self.index.iter().map(|entry| self.resolved(entry?.1))
    }
}
pub(super) mod native_path {
    use super::*;
    pub fn serialize<S: serde::Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(
            &crate::setup::migration_store::NativePath::encode(path),
            serializer,
        )
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PathBuf, D::Error> {
        let path = <crate::setup::migration_store::NativePath as serde::Deserialize>::deserialize(
            deserializer,
        )?;
        path.decode().map_err(serde::de::Error::custom)
    }
}
