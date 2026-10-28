//! Fixtures: signed packages built from payload directories, and a way to
//! see whether two files share their extents on disk.

#![allow(
    dead_code,
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::struct_field_names,
    reason = "test fixtures: failure is the assertion; FIEMAP needs a raw ioctl whose C structs name their fields"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tilo_app_package::{Manifest, Packed, SigningIdentity, pack};

/// A scratch directory on the build's filesystem (not a tmpfs `/tmp`), so
/// reflinks are possible where the filesystem supports them.
pub fn scratch() -> tempfile::TempDir {
    tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap()
}

pub fn manifest(identity: &SigningIdentity, id: &str, version: &str) -> Manifest {
    Manifest::from_toml_str(&format!(
        r#"
[package]
id = "{id}"
version = "{version}"
kind = "app"

[publisher]
identity = "ed25519:{}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/app"
type = "gui"
"#,
        hex::encode(identity.public_key_bytes())
    ))
    .unwrap()
}

/// Deterministic, incompressible-looking bytes.
pub fn bytes(seed: u8, len: usize) -> Vec<u8> {
    let mut state = u32::from(seed).wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state.to_le_bytes()[0]
        })
        .collect()
}

/// A package whose payload is `files` (path → bytes). The payload directory
/// is kept alive for as long as the returned guard, so the package can
/// still stream its files.
pub fn package(
    identity: &SigningIdentity,
    id: &str,
    version: &str,
    files: &BTreeMap<&str, Vec<u8>>,
) -> (tempfile::TempDir, Packed) {
    let dir = scratch();
    for (rel, content) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let packed = pack(dir.path(), &manifest(identity, id, version), identity).unwrap();
    (dir, packed)
}

pub fn full_tapp(packed: &Packed) -> Vec<u8> {
    tilo_app_package::tapp::to_bytes(&packed.meta, Some(packed)).unwrap()
}

pub fn thin_tapp(packed: &Packed) -> Vec<u8> {
    tilo_app_package::tapp::to_bytes(&packed.meta, None).unwrap()
}

/// Every regular file under `root`, relative path → bytes.
pub fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

pub fn content_files(store_root: &Path) -> Vec<PathBuf> {
    let data = store_root.join("content/data");
    let Ok(entries) = std::fs::read_dir(data) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "data"))
        .collect()
}

const FS_IOC_FIEMAP: libc::c_ulong = 0xC020_660B;
const FIEMAP_FLAG_SYNC: u32 = 0x1;
const FIEMAP_EXTENT_SHARED: u32 = 0x2000;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FiemapExtent {
    fe_logical: u64,
    fe_physical: u64,
    fe_length: u64,
    fe_reserved64: [u64; 2],
    fe_flags: u32,
    fe_reserved: [u32; 3],
}

#[repr(C)]
struct Fiemap {
    fm_start: u64,
    fm_length: u64,
    fm_flags: u32,
    fm_mapped_extents: u32,
    fm_extent_count: u32,
    fm_reserved: u32,
    fm_extents: [FiemapExtent; 64],
}

/// The physical extents of `path` as `(physical, length, shared)`, or `None`
/// when the filesystem cannot report them.
pub fn extents(path: &Path) -> Option<Vec<(u64, u64, bool)>> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(path).ok()?;
    let mut map = Fiemap {
        fm_start: 0,
        fm_length: u64::MAX,
        fm_flags: FIEMAP_FLAG_SYNC,
        fm_mapped_extents: 0,
        fm_extent_count: 64,
        fm_reserved: 0,
        fm_extents: [FiemapExtent::default(); 64],
    };
    // SAFETY: FS_IOC_FIEMAP reads and writes exactly one `struct fiemap`
    // followed by `fm_extent_count` extents, which `map` provides, and the
    // descriptor stays open for the call.
    let rc = unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP, &raw mut map) };
    if rc != 0 {
        return None;
    }
    let count = usize::try_from(map.fm_mapped_extents).ok()?.min(64);
    Some(
        map.fm_extents[..count]
            .iter()
            .map(|e| {
                (
                    e.fe_physical,
                    e.fe_length,
                    e.fe_flags & FIEMAP_EXTENT_SHARED != 0,
                )
            })
            .collect(),
    )
}

/// Whether `a` and `b` are backed by the same physical extents. `None` when
/// the filesystem cannot say (tmpfs, overlayfs without FIEMAP).
pub fn share_extents(a: &Path, b: &Path) -> Option<bool> {
    let a = extents(a)?;
    let b = extents(b)?;
    if a.is_empty() || b.is_empty() {
        return None;
    }
    Some(
        a.iter()
            .map(|e| (e.0, e.1))
            .eq(b.iter().map(|e| (e.0, e.1)))
            && a.iter().all(|e| e.2),
    )
}

/// Whether the filesystem under `dir` can clone files and say which
/// extents a clone shares. Overlayfs over XFS or Btrfs clones (it passes
/// FICLONE down) but answers FIEMAP with EOPNOTSUPP, so whether a file
/// shares extents cannot be observed there.
pub fn reflinks_supported(dir: &Path) -> bool {
    let src = dir.join(".probe-src");
    let dst = dir.join(".probe-dst");
    std::fs::write(&src, bytes(9, 64 * 1024)).unwrap();
    let ok = (|| {
        let from = std::fs::File::open(&src).ok()?;
        let to = std::fs::File::create(&dst).ok()?;
        Some(ficlone(&to, &from) && share_extents(&src, &dst).is_some())
    })()
    .unwrap_or(false);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&dst);
    ok
}

fn ficlone(to: &std::fs::File, from: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd;
    const FICLONE: libc::c_ulong = 0x4004_9409;
    // SAFETY: FICLONE takes the source descriptor as its argument; both
    // descriptors are open for the duration of the call.
    let rc = unsafe { libc::ioctl(to.as_raw_fd(), FICLONE, from.as_raw_fd()) };
    rc == 0
}
