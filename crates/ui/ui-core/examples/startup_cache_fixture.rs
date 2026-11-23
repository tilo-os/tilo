//! Bounded public Mesa shader archives for private startup fixtures.
//! This is not production cache publication or a sandbox policy.

use std::path::{Path, PathBuf};

fn archive_dirs(root: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    // Mesa namespaces the single-file cache by driver/LLVM/CPU identity,
    // then GPU name. Preserve both rather than substituting another device.
    let mut directories = Vec::new();
    for driver in std::fs::read_dir(root)? {
        let driver = driver?;
        if !driver.file_type()?.is_dir() {
            return Err("unexpected entry in archive root".into());
        }
        for gpu in std::fs::read_dir(driver.path())? {
            let gpu = gpu?;
            if !gpu.file_type()?.is_dir() {
                return Err("unexpected entry in driver archive directory".into());
            }
            directories.push(PathBuf::from(driver.file_name()).join(gpu.file_name()));
        }
    }
    if directories.is_empty() {
        return Err("no driver-specific shader archives observed".into());
    }
    Ok(directories)
}

fn copy_archive_pair(
    source: &Path,
    target: &Path,
    seed: bool,
) -> Result<u64, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(target)?;
    let mut bytes = 0_u64;
    for name in ["foz_cache.foz", "foz_cache_idx.foz"] {
        let path = source.join(name);
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(format!("missing or empty shader archive: {}", path.display()).into());
        }
        if seed && !metadata.permissions().readonly() {
            return Err(format!("seed is writable: {}", path.display()).into());
        }
        let name = if seed {
            name.replace("foz_cache", "tilo-seed")
        } else {
            name.to_owned()
        };
        let target = target.join(name);
        let mut output = std::fs::File::create_new(&target)?;
        let copied = std::io::copy(&mut std::fs::File::open(path)?, &mut output)?;
        if copied != metadata.len() {
            return Err("shader archive changed during copy".into());
        }
        let mut permissions = output.metadata()?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&target, permissions)?;
        bytes = bytes
            .checked_add(copied)
            .ok_or("shader archive size overflow")?;
    }
    Ok(bytes)
}

/// Export completed public probe archives into a new read-only directory.
pub fn publish_cache(cache: &Path, dir: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    // Only the probe's bounded public shader archives are exported. Never
    // overwrite an existing artifact or publish user scene data.
    let sf = cache.join("mesa_shader_cache_sf");
    let directories = archive_dirs(&sf)?;
    std::fs::create_dir(dir)?;
    let mut bytes = 0_u64;
    for relative in &directories {
        bytes = bytes
            .checked_add(copy_archive_pair(
                &sf.join(relative),
                &dir.join(relative),
                false,
            )?)
            .ok_or("shader archive size overflow")?;
    }
    for relative in directories {
        let target = dir.join(relative);
        let mut permissions = target.metadata()?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&target, permissions)?;
        let parent = target.parent().ok_or("missing driver directory")?;
        let mut permissions = parent.metadata()?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(parent, permissions)?;
    }
    let mut permissions = std::fs::metadata(dir)?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(dir, permissions)?;
    Ok(bytes)
}

/// Copy regular read-only archives into a private cache namespace.
pub fn populate_seed(source: &Path, cache: &Path) -> Result<u64, Box<dyn std::error::Error>> {
    let sf = cache.join("mesa_shader_cache_sf");
    let mut bytes = 0_u64;
    for relative in archive_dirs(source)? {
        bytes = bytes
            .checked_add(copy_archive_pair(
                &source.join(&relative),
                &sf.join(relative),
                true,
            )?)
            .ok_or("shader archive size overflow")?;
    }
    Ok(bytes)
}
