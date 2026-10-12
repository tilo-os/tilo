//! Exact inode accounting through existing private spill storage. Keys are
//! internal derived facts, never authority, paths or public resource subjects.
use super::*;
use crate::setup::migration_review::storage::{IndexValue, SortedIndex};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, MetadataExt, OpenOptions};
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Inode {
    device: u64,
    inode: u64,
    logical: u64,
    path: crate::setup::migration_store::NativePath,
    sparse_candidate: bool,
}
impl IndexValue for Inode {
    const RECORD_BYTES: usize = 128 * 1024;
}
fn key(inode: &Inode, ordinal: usize) -> PathBuf {
    // Unique ordinals preserve original inventory duplicate rejection. Only
    // this accounting reduction intentionally groups multiple hardlink paths.
    PathBuf::from(format!(
        "{:016x}:{:016x}:{ordinal:016x}",
        inode.device, inode.inode
    ))
}
fn same_inode(left: &Inode, right: &Inode) -> bool {
    (left.device, left.inode, left.logical, left.sparse_candidate)
        == (
            right.device,
            right.inode,
            right.logical,
            right.sparse_candidate,
        )
}
fn measure(
    root: &Arc<PinnedDirectory>,
    inventory: &SourceInventory,
    inode: &Inode,
    cancel: &AtomicBool,
) -> io::Result<u64> {
    check_cancel(cancel)?;
    let path = inode.path.decode().map_err(io::Error::other)?;
    let (parent, name) = archive::parent(root, &path)?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&parent.file, Path::new(&name), &options)?;
    let before = cap_primitives::fs::Metadata::from_file(&file)?;
    inventory.check(&path, &before)?;
    let candidate = before.len() >= 512 && before.blocks() < before.len().div_ceil(512);
    if !before.is_file()
        || before.dev() != inode.device
        || before.ino() != inode.inode
        || before.len() != inode.logical
        || candidate != inode.sparse_candidate
    {
        return Err(changed("Reviewed inode accounting source changed"));
    }
    let stored = if candidate {
        sparse::stored_size(&file)?
    } else {
        before.len()
    };
    check_cancel(cancel)?;
    inventory.check(&path, &cap_primitives::fs::Metadata::from_file(&file)?)?;
    parent.validate()?;
    if stored > inode.logical {
        return Err(changed("Stored inode size exceeds logical length"));
    }
    Ok(stored)
}
fn fold(
    index: &SortedIndex<Inode>,
    root: &Arc<PinnedDirectory>,
    inventory: &SourceInventory,
    cancel: &AtomicBool,
) -> io::Result<(u64, bool)> {
    let mut previous: Option<Inode> = None;
    let mut bytes = 0u64;
    let mut sparse = false;
    for item in index.iter() {
        check_cancel(cancel)?;
        let (path, inode) = item?;
        let text = path
            .to_str()
            .ok_or_else(|| changed("Inode accounting key invalid"))?;
        let ordinal = text
            .rsplit_once(':')
            .and_then(|(_, n)| usize::from_str_radix(n, 16).ok())
            .ok_or_else(|| changed("Inode accounting ordinal invalid"))?;
        if path != key(&inode, ordinal) {
            return Err(changed("Inode accounting record binding changed"));
        }
        if let Some(previous) = &previous {
            if (previous.device, previous.inode) == (inode.device, inode.inode) {
                if !same_inode(previous, &inode) {
                    return Err(changed("Reviewed hardlink inode accounting differs"));
                }
                continue;
            }
            bytes = bytes
                .checked_add(measure(root, inventory, previous, cancel)?)
                .ok_or_else(|| changed("Stored archive data size overflows"))?;
            sparse |= previous.sparse_candidate;
        }
        previous = Some(inode);
    }
    if let Some(previous) = previous {
        bytes = bytes
            .checked_add(measure(root, inventory, &previous, cancel)?)
            .ok_or_else(|| changed("Stored archive data size overflows"))?;
        sparse |= previous.sparse_candidate;
    }
    Ok((bytes, sparse))
}
pub(super) fn source_sizes(
    root: &Arc<PinnedDirectory>,
    inventory: &SourceInventory,
    cancel: &AtomicBool,
) -> io::Result<(u64, bool)> {
    check_cancel(cancel)?;
    #[cfg(target_os = "linux")]
    let mut index = match inventory.working_storage() {
        Some(scratch) => SortedIndex::with_scratch(scratch),
        None => SortedIndex::new(),
    };
    #[cfg(not(target_os = "linux"))]
    let mut index = SortedIndex::new();
    inventory.validate_with_cancel(root, cancel)?;
    for (ordinal, path) in inventory.regular_files().enumerate() {
        check_cancel(cancel)?;
        let path = path?;
        let (parent, name) = archive::parent(root, &path)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let file = cap_primitives::fs::open(&parent.file, Path::new(&name), &options)?;
        let before = cap_primitives::fs::Metadata::from_file(&file)?;
        inventory.check(&path, &before)?;
        if !before.is_file() {
            return Err(changed("Reviewed regular file changed kind"));
        }
        // Exact GNU/Linux ST_IS_SPARSE predicate. Compressed Btrfs may qualify
        // even when every logical byte is a SEEK_DATA extent.
        let candidate = before.len() >= 512 && before.blocks() < before.len().div_ceil(512);
        check_cancel(cancel)?;
        inventory.check(&path, &cap_primitives::fs::Metadata::from_file(&file)?)?;
        parent.validate()?;
        let inode = Inode {
            device: before.dev(),
            inode: before.ino(),
            logical: before.len(),
            path: crate::setup::migration_store::NativePath::encode(&path),
            sparse_candidate: candidate,
        };
        index.insert(key(&inode, ordinal), inode, cancel)?;
    }
    index.finish(cancel)?;
    let sizes = fold(&index, root, inventory, cancel)?;
    inventory.validate_with_cancel(root, cancel)?;
    check_cancel(cancel)?;
    Ok(sizes)
}
#[cfg(all(test, feature = "native-capture-qualification", target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn fixture() -> tempfile::TempDir {
        let root = std::env::var_os("TILO_FS_NATIVE_STORE")
            .expect("owned supported native store required");
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(root)
            .unwrap()
    }
    #[test]
    fn inode_accounting_spills_real_hardlinks_and_sparse_holes_under_retained_grant() {
        let fixture = fixture();
        let source = fixture.path().join("source");
        let workspace = fixture.path().join("workspace");
        for path in [&source, &workspace] {
            std::fs::create_dir(path).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        for n in 0u32..1100 {
            std::fs::write(source.join(format!("file-{n:04}")), n.to_le_bytes()).unwrap();
        }
        // Independent names refer to one actual inode, not duplicated content.
        for n in 0..30 {
            std::fs::hard_link(
                source.join("file-0000"),
                source.join(format!("alias-{n:04}")),
            )
            .unwrap();
        }
        let hole = File::create(source.join("hole-only")).unwrap();
        hole.set_len(1024 * 1024 * 1024).unwrap();
        hole.sync_all().unwrap();
        drop(hole);
        std::fs::hard_link(source.join("hole-only"), source.join("hole-alias")).unwrap();
        let source_pin = PinnedDirectory::open_absolute(&source).unwrap();
        let scratch = crate::setup::migration_review::InventoryScratch::review(
            PinnedDirectory::open_absolute(&workspace).unwrap(),
            32 * 1024 * 1024,
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let inventory = SourceInventory::capture_portable_home_with_storage(
            &source_pin,
            &source,
            scratch.clone(),
            &cancel,
        )
        .unwrap();
        let actual = source_sizes(&source_pin, &inventory, &cancel).unwrap();
        assert_eq!(actual, (1100 * 4, true));
        assert_eq!(stored_bytes(&source_pin, &inventory).unwrap(), actual.0);
        assert!(source_sizes(&source_pin, &inventory, &AtomicBool::new(true)).is_err());
        // Changed source data cannot reuse a cached accounting result.
        std::fs::write(source.join("file-0001"), b"changed").unwrap();
        assert!(source_sizes(&source_pin, &inventory, &cancel).is_err());
        assert!(std::fs::read_dir(&workspace).unwrap().next().is_none());
    }
    #[test]
    fn inode_reduction_refuses_inconsistent_alias_facts_and_lost_private_custody() {
        let fixture = fixture();
        let workspace = fixture.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700)).unwrap();
        let scratch = crate::setup::migration_review::InventoryScratch::review(
            PinnedDirectory::open_absolute(&workspace).unwrap(),
            1024 * 1024,
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let mut index = SortedIndex::with_scratch(scratch.clone());
        let source = fixture.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("document"), b"checked source").unwrap();
        let source_pin = PinnedDirectory::open_absolute(&source).unwrap();
        let inventory = SourceInventory::capture_portable_home_with_storage(
            &source_pin,
            &source,
            scratch.clone(),
            &cancel,
        )
        .unwrap();
        let first = Inode {
            device: 1,
            inode: 2,
            logical: 4096,
            path: crate::setup::migration_store::NativePath::encode(Path::new("document")),
            sparse_candidate: false,
        };
        let mut changed = first.clone();
        changed.logical = 2048;
        index.insert(key(&first, 0), first, &cancel).unwrap();
        index.insert(key(&changed, 1), changed, &cancel).unwrap();
        index.finish(&cancel).unwrap();
        assert!(fold(&index, &source_pin, &inventory, &cancel).is_err());
        std::fs::rename(&workspace, fixture.path().join("retained-workspace")).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        assert!(fold(&index, &source_pin, &inventory, &cancel).is_err());
    }
}
