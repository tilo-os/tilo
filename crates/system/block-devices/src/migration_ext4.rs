//! Fixed format policy ONLY for the installer migration-data partition.
//! General ext4 formatting deliberately does not use these settings.
use std::io;
const BLOCK: u64 = 4096;
const GROUP_BLOCKS: u64 = 32768;
const INODE_BYTES: u64 = 256;
const BYTES_PER_INODE: u64 = 65536;
const JOURNAL_BLOCKS: u64 = 8 * 1024 * 1024 / BLOCK;
/// Explicit overrides prevent host mke2fs.conf from changing budget geometry.
pub const MKFS_ARGS: &[&str] = &[
    "-b",
    "4096",
    "-g",
    "32768",
    "-I",
    "256",
    "-i",
    "65536",
    "-m",
    "0",
    "-J",
    "size=8",
    "-O",
    "none,filetype,extent,has_journal,64bit,metadata_csum,sparse_super2",
    "-E",
    "desc_size=64,num_backup_sb=2,lazy_itable_init=0,lazy_journal_init=0,nodiscard",
];
/// Conservative fresh filesystem free bytes for exactly MKFS_ARGS.
///
/// # Errors
/// Refuses small filesystems, arithmetic overflow or metadata exceeding extent.
pub fn available_bytes(extent: u64) -> io::Result<u64> {
    let fail = || io::Error::other("Migration ext4 extent cannot cover fixed format metadata");
    if extent < 64 * 1024 * 1024 {
        return Err(fail());
    }
    let blocks = extent / BLOCK;
    let groups = blocks.div_ceil(GROUP_BLOCKS);
    let descriptors = groups.checked_mul(64).ok_or_else(fail)?.div_ceil(BLOCK);
    let inodes_per_group = GROUP_BLOCKS * BLOCK / BYTES_PER_INODE;
    let inode_table = (inodes_per_group * INODE_BYTES).div_ceil(BLOCK);
    // Charge a superblock and a full descriptor table in EVERY group even
    // though sparse_super2 writes at most two backups. No resize/meta_bg/flex
    // features are enabled. Charge both allocation bitmaps and full tables.
    let per_group = inode_table
        .checked_add(3)
        .and_then(|v| v.checked_add(descriptors))
        .ok_or_else(fail)?;
    let mut metadata = groups
        .checked_mul(per_group)
        .and_then(|v| v.checked_add(JOURNAL_BLOCKS))
        .ok_or_else(fail)?;
    // Worst-case one physical extent per data block: 340 twelve-byte entries
    // fit in each 4KiB extent node (12-byte header), recursively through the
    // complete possible index tree. Fixed root/lost+found/carrier directory
    // allowance includes short-lived atomic publication entries.
    let mut nodes = blocks;
    loop {
        nodes = nodes.div_ceil(340);
        metadata = metadata.checked_add(nodes).ok_or_else(fail)?;
        if nodes <= 1 {
            break;
        }
    }
    metadata = metadata.checked_add(1024).ok_or_else(fail)?;
    blocks
        .checked_sub(metadata)
        .and_then(|v| v.checked_mul(BLOCK))
        .filter(|v| *v > 0)
        .ok_or_else(fail)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_metadata_rejects_small_and_excludes_near_raw_capacity() {
        assert!(available_bytes(32 * 1024 * 1024).is_err());
        let extent = 512 * 1024 * 1024;
        let free = available_bytes(extent).unwrap();
        assert!(free < extent - 8 * 1024 * 1024);
        assert!(free > 400 * 1024 * 1024);
        assert_eq!(
            MKFS_ARGS.windows(2).find(|pair| pair[0] == "-m").unwrap()[1],
            "0"
        );
    }
}
