//! Session-owned public Mesa shaders, copied into each app's private cache.
//! A seed is an optional optimization: malformed or incompatible bytes never
//! make an app launch fail. The driver still selects its own identity namespace.

use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) const CONSUME_ENV: &str = "TILO_MESA_SEED_CONSUME";

const MAX_BYTES: u64 = 32 * 1024 * 1024;
const MAX_FILES: usize = 16;
const MANIFEST: &str = "seed.json";
const FORMAT: u32 = 1;
const HEADER: &[u8] = b"\x81FOSSILIZEDB\0\0\0\x06";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    fingerprint: String,
    files: Vec<Archive>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Archive {
    path: PathBuf,
    bytes: u64,
    hash: String,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn regular_read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let meta = path.symlink_metadata()?;
    if !meta.is_file() || meta.len() > limit {
        return Err(invalid("seed entry is not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(io::Error::other)? != meta.len() {
        return Err(invalid("seed entry changed while reading"));
    }
    Ok(bytes)
}

fn archive_path(relative: &Path) -> io::Result<()> {
    let names = relative
        .components()
        .map(|part| match part {
            Component::Normal(name) => name.to_str().ok_or_else(|| invalid("non-UTF8 seed name")),
            _ => Err(invalid("seed path escapes its namespace")),
        })
        .collect::<io::Result<Vec<_>>>()?;
    let [driver, gpu, file] = names.as_slice() else {
        return Err(invalid("seed path must name driver, device and archive"));
    };
    if ![40, 64].contains(&driver.len())
        || !driver.bytes().all(|byte| byte.is_ascii_hexdigit())
        || gpu.is_empty()
        || !gpu
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        || !["foz_cache.foz", "foz_cache_idx.foz"].contains(file)
    {
        return Err(invalid("invalid Mesa driver/device archive name"));
    }
    Ok(())
}

fn directory(path: &Path) -> io::Result<()> {
    if !path.symlink_metadata()?.is_dir() {
        return Err(invalid("seed directory is not a real directory"));
    }
    Ok(())
}

fn load(root: &Path) -> io::Result<(Manifest, Vec<Vec<u8>>)> {
    directory(root)?;
    let manifest: Manifest = serde_json::from_slice(&regular_read(&root.join(MANIFEST), 16384)?)?;
    if manifest.format != FORMAT || manifest.files.is_empty() || manifest.files.len() > MAX_FILES {
        return Err(invalid("unsupported or unbounded seed manifest"));
    }
    let mut total = 0_u64;
    let mut files = Vec::new();
    let mut observed = std::collections::BTreeSet::new();
    for file in &manifest.files {
        archive_path(&file.path)?;
        if !observed.insert(file.path.clone()) {
            return Err(invalid("duplicate shader archive"));
        }
        total = total
            .checked_add(file.bytes)
            .ok_or_else(|| invalid("archive size overflow"))?;
        if total > MAX_BYTES {
            return Err(invalid("shader seed exceeds its byte budget"));
        }
        let parent = file
            .path
            .parent()
            .ok_or_else(|| invalid("archive parent missing"))?;
        directory(
            &root.join(
                parent
                    .parent()
                    .ok_or_else(|| invalid("driver parent missing"))?,
            ),
        )?;
        directory(&root.join(parent))?;
        let bytes = regular_read(&root.join(&file.path), file.bytes)?;
        if u64::try_from(bytes.len()).map_err(io::Error::other)? != file.bytes
            || !bytes.starts_with(HEADER)
            || blake3::hash(&bytes).to_hex().as_str() != file.hash
        {
            return Err(invalid("shader archive hash, length or format mismatch"));
        }
        files.push(bytes);
    }
    for path in &observed {
        let pair = path.with_file_name(
            if path.file_name().is_some_and(|name| name == "foz_cache.foz") {
                "foz_cache_idx.foz"
            } else {
                "foz_cache.foz"
            },
        );
        if !observed.contains(&pair) {
            return Err(invalid("unpaired shader archive"));
        }
    }
    Ok((manifest, files))
}

/// Read a validated seed's producer fingerprint.
pub fn fingerprint(root: &Path) -> io::Result<String> {
    load(root).map(|(manifest, _)| manifest.fingerprint)
}

fn readonly_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)?;
    let mut permissions = path.metadata()?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions)
}

/// Publish bounded completed public shader archives. The input is the trusted
/// renderer's private single-file cache, never an application's writable cache.
pub fn publish(cache: &Path, root: &Path, fingerprint: &str) -> io::Result<()> {
    std::fs::create_dir_all(root)?;
    directory(root)?;
    let stage = tempfile::Builder::new()
        .prefix("seed-stage-")
        .tempdir_in(root)?;
    let mut manifest = Manifest {
        format: FORMAT,
        fingerprint: fingerprint.to_owned(),
        files: Vec::new(),
    };
    let mut total = 0_u64;
    for driver in std::fs::read_dir(cache.join("mesa_shader_cache_sf"))? {
        let driver = driver?;
        directory(&driver.path())?;
        for gpu in std::fs::read_dir(driver.path())? {
            let gpu = gpu?;
            directory(&gpu.path())?;
            for name in ["foz_cache.foz", "foz_cache_idx.foz"] {
                let relative = PathBuf::from(driver.file_name())
                    .join(gpu.file_name())
                    .join(name);
                archive_path(&relative)?;
                let bytes = regular_read(
                    &cache.join("mesa_shader_cache_sf").join(&relative),
                    MAX_BYTES,
                )?;
                if !bytes.starts_with(HEADER) {
                    return Err(invalid("unexpected Mesa archive format"));
                }
                total = total
                    .checked_add(u64::try_from(bytes.len()).map_err(io::Error::other)?)
                    .ok_or_else(|| invalid("archive size overflow"))?;
                if total > MAX_BYTES || manifest.files.len() >= MAX_FILES {
                    return Err(invalid("shader seed exceeds its budget"));
                }
                let target = stage.path().join(&relative);
                std::fs::create_dir_all(
                    target
                        .parent()
                        .ok_or_else(|| invalid("archive parent missing"))?,
                )?;
                readonly_write(&target, &bytes)?;
                manifest.files.push(Archive {
                    path: relative,
                    bytes: u64::try_from(bytes.len()).map_err(io::Error::other)?,
                    hash: blake3::hash(&bytes).to_hex().to_string(),
                });
            }
        }
    }
    readonly_write(
        &stage.path().join(MANIFEST),
        &serde_json::to_vec(&manifest)?,
    )?;
    // Validate the exact staged files before replacing the published generation.
    load(stage.path())?;
    replace(&stage, &root.join("mesa"))
}

fn replace(stage: &tempfile::TempDir, target: &Path) -> io::Result<()> {
    // An interrupted replacement is a cold-cache fallback, never a launch error.
    // Only this dedicated seed directory is replaced; normal writable caches stay.
    if target.exists() || target.is_symlink() {
        if target.symlink_metadata()?.is_dir() {
            std::fs::remove_dir_all(target)?;
        } else {
            std::fs::remove_file(target)?;
        }
    }
    std::fs::rename(stage.path(), target)?;
    Ok(())
}

/// Copy a verified seed generation into the normal size-limited private Mesa
/// cache. Archives from another Mesa/LLVM/CPU/device namespace remain unused by
/// Mesa, which compiles the missing shaders normally.
pub fn prepare(source: &Path, private: &Path) -> io::Result<()> {
    let (manifest, files) = load(source)?;
    std::fs::create_dir_all(private)?;
    directory(private)?;
    let _lock = advisory_lock::FileLock::try_open(
        &private.join("seed-copy.lock"),
        advisory_lock::LockMode::Exclusive,
    )
    .map_err(io::Error::other)?;
    for entry in std::fs::read_dir(private)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("seed-stage-"))
            && entry.file_type()?.is_dir()
        {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    let stage = tempfile::Builder::new()
        .prefix("seed-stage-")
        .tempdir_in(private)?;
    for (entry, bytes) in manifest.files.iter().zip(files) {
        let relative = entry.path.with_file_name(
            if entry
                .path
                .file_name()
                .is_some_and(|name| name == "foz_cache.foz")
            {
                "tilo-seed.foz"
            } else {
                "tilo-seed_idx.foz"
            },
        );
        let target = stage.path().join(relative);
        std::fs::create_dir_all(
            target
                .parent()
                .ok_or_else(|| invalid("archive parent missing"))?,
        )?;
        readonly_write(&target, &bytes)?;
    }
    replace(&stage, &private.join("mesa_shader_cache_sf"))
}

/// Refresh the existing session's read-only seed from its validated persistent
/// public archive. This does not copy any user's scenes or private app data.
pub fn restore(source: &Path, session: &Path) -> io::Result<()> {
    let (manifest, files) = load(source)?;
    std::fs::create_dir_all(session)?;
    directory(session)?;
    let stage = tempfile::Builder::new()
        .prefix("seed-stage-")
        .tempdir_in(session)?;
    for (entry, bytes) in manifest.files.iter().zip(files) {
        let target = stage.path().join(&entry.path);
        std::fs::create_dir_all(
            target
                .parent()
                .ok_or_else(|| invalid("archive parent missing"))?,
        )?;
        readonly_write(&target, &bytes)?;
    }
    readonly_write(
        &stage.path().join(MANIFEST),
        &serde_json::to_vec(&manifest)?,
    )?;
    replace(&stage, &session.join("mesa"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(root: &Path, driver: char, digits: usize) -> PathBuf {
        let relative = PathBuf::from(driver.to_string().repeat(digits)).join("llvmpipe");
        let dir = root.join("mesa_shader_cache_sf").join(&relative);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["foz_cache.foz", "foz_cache_idx.foz"] {
            std::fs::write(dir.join(name), [HEADER, b"public shader fixture"].concat()).unwrap();
        }
        relative
    }

    #[test]
    fn private_copy_preserves_driver_names_and_never_changes_the_public_source() {
        let home = tempfile::tempdir().unwrap();
        let compiled = home.path().join("compiled");
        let a = cache(&compiled, 'a', 64);
        let b = cache(&compiled, 'b', 40);
        let published = home.path().join("published");
        publish(&compiled, &published, "renderer-v1").unwrap();
        let source = published.join("mesa");
        let before = load(&source).unwrap().1;
        let private = home.path().join("private");
        prepare(&source, &private).unwrap();
        for relative in [a, b] {
            let copied = private
                .join("mesa_shader_cache_sf")
                .join(relative)
                .join("tilo-seed.foz");
            assert_eq!(std::fs::read(&copied).unwrap(), before[0]);
            assert!(copied.metadata().unwrap().permissions().readonly());
        }
        assert_eq!(load(&source).unwrap().1, before);
        assert_eq!(fingerprint(&source).unwrap(), "renderer-v1");
    }

    #[test]
    fn corruption_fails_before_replacing_a_good_private_generation() {
        let home = tempfile::tempdir().unwrap();
        let compiled = home.path().join("compiled");
        let relative = cache(&compiled, 'a', 64);
        let published = home.path().join("published");
        publish(&compiled, &published, "v1").unwrap();
        let source = published.join("mesa");
        let private = home.path().join("private");
        prepare(&source, &private).unwrap();
        let copied = private
            .join("mesa_shader_cache_sf")
            .join(&relative)
            .join("tilo-seed.foz");
        let before = std::fs::read(&copied).unwrap();
        let broken = source.join(relative).join("foz_cache.foz");
        std::fs::remove_file(&broken).unwrap();
        std::fs::write(&broken, [HEADER, b"damaged data"].concat()).unwrap();
        assert_eq!(
            prepare(&source, &private).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(std::fs::read(copied).unwrap(), before);
        assert!(fingerprint(&source).is_err());
        publish(&compiled, &published, "v2").unwrap();
        prepare(&source, &private).unwrap();
        assert_eq!(fingerprint(&source).unwrap(), "v2");
    }

    #[test]
    fn missing_seed_leaves_private_cache_untouched() {
        let home = tempfile::tempdir().unwrap();
        let private = home.path().join("private");
        assert_eq!(
            prepare(&home.path().join("absent"), &private)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(!private.exists());
    }

    #[test]
    fn manifest_cannot_escape_the_seed_or_exceed_the_byte_budget() {
        let home = tempfile::tempdir().unwrap();
        let source = home.path().join("source");
        std::fs::create_dir(&source).unwrap();
        for (path, bytes) in [
            (PathBuf::from("../../outside"), 16),
            (
                PathBuf::from("a".repeat(64)).join("llvmpipe/foz_cache.foz"),
                MAX_BYTES + 1,
            ),
        ] {
            let manifest = Manifest {
                format: FORMAT,
                fingerprint: "v1".into(),
                files: vec![Archive {
                    path,
                    bytes,
                    hash: String::new(),
                }],
            };
            std::fs::write(
                source.join(MANIFEST),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            assert_eq!(
                load(&source).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn symlink_and_unpaired_archives_are_refused() {
        let home = tempfile::tempdir().unwrap();
        let compiled = home.path().join("compiled");
        let relative = cache(&compiled, 'a', 64);
        let published = home.path().join("published");
        publish(&compiled, &published, "v1").unwrap();
        let source = published.join("mesa");
        let entry = source.join(&relative).join("foz_cache.foz");
        std::fs::remove_file(&entry).unwrap();
        std::os::unix::fs::symlink(
            compiled
                .join("mesa_shader_cache_sf")
                .join(relative)
                .join("foz_cache.foz"),
            &entry,
        )
        .unwrap();
        assert_eq!(
            load(&source).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        publish(&compiled, &published, "v1").unwrap();
        let mut manifest: Manifest =
            serde_json::from_slice(&std::fs::read(source.join(MANIFEST)).unwrap()).unwrap();
        manifest.files.pop();
        std::fs::remove_file(source.join(MANIFEST)).unwrap();
        std::fs::write(
            source.join(MANIFEST),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(
            load(&source).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
