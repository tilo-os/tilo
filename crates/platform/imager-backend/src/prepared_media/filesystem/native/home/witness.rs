//! Owner-private filesystem reuse witness, published before any home export.
use super::*;
use std::io::{Read as _, Write as _};
const FILE: &str = "filesystem-owner.json";
const MAX_BYTES: u64 = 4096;
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Witness {
    schema: u32,
    binding: MediaPartitionBinding,
    filesystem_uuid: String,
}
fn expected(binding: &MediaPartitionBinding, partition: &Partition) -> io::Result<Witness> {
    Ok(Witness {
        schema: 1,
        binding: binding.clone(),
        filesystem_uuid: partition
            .uuid
            .clone()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| changed("Migration filesystem UUID missing"))?,
    })
}
pub(super) fn matches(
    root: &Path,
    binding: &MediaPartitionBinding,
    partition: &Partition,
) -> io::Result<bool> {
    use cap_primitives::fs::OpenOptionsExt as _;
    let pin = PinnedRoot::open_absolute(root)?;
    let directory = match crate::nofollow::metadata_nofollow(&pin.file, Path::new(DIRECTORY)) {
        Ok(metadata) => pin.open_child(std::ffi::OsStr::new(DIRECTORY), &metadata)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let metadata = match crate::nofollow::metadata_nofollow(&directory.file, Path::new(FILE)) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    // SAFETY: reads current process credentials only.
    let owner = rustix::process::geteuid().as_raw();
    if !metadata.is_file()
        || metadata.len() > MAX_BYTES
        || metadata.uid() != owner
        || metadata.mode() & 0o777 != 0o600
    {
        return Ok(false);
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .custom_flags(libc::O_NONBLOCK);
    let file = cap_primitives::fs::open(&directory.file, Path::new(FILE), &options)?;
    let actual = cap_primitives::fs::Metadata::from_file(&file)?;
    if cap_fs_ext::MetadataExt::ino(&actual) != cap_fs_ext::MetadataExt::ino(&metadata)
        || cap_fs_ext::MetadataExt::dev(&actual) != cap_fs_ext::MetadataExt::dev(&metadata)
        || actual.len() != metadata.len()
    {
        return Err(changed(
            "Migration filesystem witness changed while opening",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    let Ok(receipt) = serde_json::from_slice::<Witness>(&bytes) else {
        return Ok(false);
    };
    Ok(receipt == expected(binding, partition)?)
}
pub(super) fn publish(
    grant: &MediaFilesystemGrant,
    binding: &MediaPartitionBinding,
    partition: &Partition,
) -> io::Result<()> {
    use cap_primitives::fs::OpenOptionsExt as _;
    let directory = delivery_directory(&grant.pin)?;
    let bytes = serde_json::to_vec(&expected(binding, partition)?).map_err(io::Error::other)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(changed("Migration filesystem witness exceeds bounds"));
    }
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No)
        .mode(0o600);
    let mut file = cap_primitives::fs::open(&directory.file, Path::new(FILE), &options)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    durability::sync_directory(&directory.file)?;
    if !matches(grant.root_path(), binding, partition)? {
        return Err(changed(
            "Published migration filesystem witness did not verify",
        ));
    }
    Ok(())
}

fn delivery_directory(root: &Arc<PinnedRoot>) -> io::Result<Arc<PinnedRoot>> {
    use cap_primitives::fs::{DirBuilderExt, DirOptions};
    root.validate_destination()?;
    match crate::nofollow::metadata_nofollow(&root.file, Path::new(DIRECTORY)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut options = DirOptions::new();
            // Root helper owns a protected custody namespace. This applies only
            // to fresh creation; existing nonroot/0755 trees are never repaired.
            #[cfg(unix)]
            let mode = if rustix::process::geteuid().as_raw() == 0 {
                0o700
            } else {
                0o755
            };
            #[cfg(not(unix))]
            let mode = 0o755;
            options.mode(mode);
            cap_primitives::fs::create_dir(&root.file, Path::new(DIRECTORY), &options)?;
        }
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let metadata = crate::nofollow::metadata_nofollow(&root.file, Path::new(DIRECTORY))?;
    let directory = root.open_child(std::ffi::OsStr::new(DIRECTORY), &metadata)?;
    directory.validate_destination()?;
    Ok(directory)
}
