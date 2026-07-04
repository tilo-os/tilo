//! qOS runtime bundle producer + verifier.
//!
//! ## What a runtime bundle is
//!
//! A directory laid out as
//!
//! ```text
//! <root>/
//!   bundle.toml           metadata: version, root hash, payload manifest
//!   lib/                  host-copied loader + libc + friends
//!     ld-linux-x86-64.so.2
//!     libc.so.6
//!     libgcc_s.so.1
//!     …
//!   etc/ld.so.cache       (optional; copied from host if present)
//! ```
//!
//! Content-addressed: `bundle.toml.root_hash` is a BLAKE3 root over the
//! sorted list of `(payload-relative-path, blake3(file))` pairs. Any
//! byte change in any file — or in the manifest of paths itself —
//! flips the root hash.
//!
//! ## Discovery
//!
//! The launcher looks a bundle up by its version tag. Search order:
//! 1. `$XDG_DATA_HOME/qos-runtime/<version>/` — per-user (default when
//!    installed with `qos-install runtime install`).
//! 2. `/usr/lib/qos-runtime/<version>/` — system-wide (baked into the
//!    OS image).
//!
//! The `default_bundle_path` and `system_bundle_path` helpers surface
//! these paths from the library so both the CLI and the launcher agree
//! on where to look.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Standard bundle manifest filename inside a bundle directory.
pub const BUNDLE_MANIFEST_FILENAME: &str = "bundle.toml";

/// Bundle metadata written into `bundle.toml`. All fields are stable
/// so that a `verify` roundtrip is meaningful across tool versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleManifest {
    /// Human-readable version tag — matches the manifest string
    /// `qos-runtime/<version>` in a package manifest.
    pub version: String,
    /// BLAKE3 root hash over the sorted `(path, file-hash)` list.
    /// Hex, lowercase, 64 chars.
    pub root_hash: String,
    /// Sorted list of every file in the bundle (relative to the
    /// bundle root) with its individual BLAKE3. Sorted so the
    /// encoding is deterministic across filesystem traversal
    /// orderings.
    #[serde(default)]
    pub files: Vec<BundleFile>,
    /// Produced-at unix time. Informational only — the hash is what
    /// matters for identity.
    #[serde(default)]
    pub produced_at: u64,
}

/// One `(payload-relative-path, hash)` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleFile {
    /// Path relative to the bundle root, always using `/` as
    /// separator so the encoding is portable.
    pub path: String,
    /// BLAKE3 hash of the file bytes. Hex, lowercase.
    pub hash: String,
    /// Size in bytes.
    pub size: u64,
}

/// Errors produced by the builder + verifier.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("no bundle sources found — checked {checked:?}")]
    NoSources { checked: Vec<PathBuf> },
    #[error("bundle root {root} contains no lib/ payload")]
    EmptyBundle { root: PathBuf },
    #[error("bundle.toml not found at {path}")]
    ManifestMissing { path: PathBuf },
    #[error("bundle.toml parse error: {0}")]
    ManifestParse(String),
    #[error("hash mismatch: expected {expected} in bundle.toml, computed {computed}")]
    HashMismatch { expected: String, computed: String },
    #[error("file listed in bundle.toml is missing from the payload: {path}")]
    FileMissing { path: String },
    #[error("file present in payload but not listed in bundle.toml: {path}")]
    FileUnlisted { path: String },
    #[error("home / data-dir not resolvable — set XDG_DATA_HOME or HOME")]
    NoDataDir,
}

/// The minimal loader + libc set the launcher needs to `execve()` a
/// dynamically-linked ELF inside the sandbox. Mirrors the historical
/// list in `launcher::run::minimal_loader_binds` — the two must stay
/// in sync as long as `--minimal-libs` remains supported.
///
/// These are absolute paths on the *host*. The builder resolves each
/// against `--from-host` (default `/`), and any that don't exist are
/// silently skipped so a bundle can be built on a system whose loader
/// lives at only one of the alternative locations. The result is still
/// hash-stable — the manifest lists what was actually included.
pub const HOST_MINIMAL_PATHS: &[&str] = &[
    // Dynamic loader itself.
    "/lib64/ld-linux-x86-64.so.2",
    "/lib/ld-linux-x86-64.so.2",
    "/lib/ld-linux.so.2",
    "/usr/lib64/ld-linux-x86-64.so.2",
    // libc + friends that most Rust binaries need.
    "/lib64/libc.so.6",
    "/lib64/libgcc_s.so.1",
    "/lib64/libm.so.6",
    "/lib64/libdl.so.2",
    "/lib64/libpthread.so.0",
    "/lib64/librt.so.1",
    "/lib64/libutil.so.1",
];

/// The optional `/etc/ld.so.cache` — the loader falls back to a
/// slower search without it; still worth including when present.
pub const HOST_LD_SO_CACHE: &str = "/etc/ld.so.cache";

/// Default per-user bundle directory: `$XDG_DATA_HOME/qos-runtime/<v>/`
/// (or `~/.local/share/qos-runtime/<v>/`).
pub fn default_bundle_path(version: &str) -> Result<PathBuf, BundleError> {
    let base = dirs::data_dir().ok_or(BundleError::NoDataDir)?;
    Ok(base.join("qos-runtime").join(version))
}

/// System-wide fallback: `/usr/lib/qos-runtime/<v>/`.
pub fn system_bundle_path(version: &str) -> PathBuf {
    PathBuf::from("/usr/lib/qos-runtime").join(version)
}

/// Walk the search path and return the first existing bundle directory
/// that already has a `bundle.toml`. Ordering: per-user, then system.
pub fn resolve_bundle_path(version: &str) -> Option<PathBuf> {
    let mut checked: Vec<PathBuf> = Vec::new();
    if let Ok(p) = default_bundle_path(version) {
        checked.push(p.clone());
        if p.join(BUNDLE_MANIFEST_FILENAME).exists() {
            return Some(p);
        }
    }
    let sys = system_bundle_path(version);
    checked.push(sys.clone());
    if sys.join(BUNDLE_MANIFEST_FILENAME).exists() {
        return Some(sys);
    }
    None
}

/// Build (or rebuild) a bundle at `out`.
///
/// - `version` names the bundle (also written into `bundle.toml`).
/// - `from_host` overrides the host-root prefix each source path is
///   resolved against; defaults to `/`. Useful for tests and for
///   building against a distrobox rootfs from outside the container.
/// - Missing source paths are silently skipped; the bundle's hash
///   reflects what was actually copied. If *no* source resolves, we
///   return `NoSources` so a build against the wrong `--from-host`
///   surfaces immediately.
/// - Existing files at `out/lib/…` are overwritten. Non-payload files
///   under `out` are left alone (so a bundle dir can hold sibling
///   metadata beyond `bundle.toml` — future-proofing for
///   signature files etc.).
pub fn build_bundle(
    out: &Path,
    version: &str,
    from_host: Option<&Path>,
) -> Result<BundleManifest, BundleError> {
    let host_root = from_host.unwrap_or_else(|| Path::new("/"));
    fs::create_dir_all(out.join("lib"))?;

    // Copy each candidate. Track what we got so the manifest is
    // deterministic.
    let mut written: Vec<(String, PathBuf)> = Vec::new(); // relative-in-bundle → absolute-in-bundle

    for src_abs in HOST_MINIMAL_PATHS {
        let src = resolve_from(host_root, src_abs);
        if !src.exists() {
            continue;
        }
        // Preserve the absolute-path shape under `lib/` — e.g.
        // `/lib64/libc.so.6` lives at `lib/lib64/libc.so.6` in the
        // bundle. This way `LD_LIBRARY_PATH=<bundle>/lib` combined
        // with the loader's own search inside the bundle-mount finds
        // everything without extra symlink dances.
        let rel_no_slash = src_abs.trim_start_matches('/');
        let rel_in_bundle = format!("lib/{}", rel_no_slash);
        let dst = out.join(&rel_in_bundle);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        copy_file(&src, &dst)?;
        written.push((rel_in_bundle, dst));
    }

    // Optional ld.so.cache.
    let cache_src = resolve_from(host_root, HOST_LD_SO_CACHE);
    if cache_src.exists() {
        let rel_in_bundle = "etc/ld.so.cache".to_string();
        let dst = out.join(&rel_in_bundle);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        copy_file(&cache_src, &dst)?;
        written.push((rel_in_bundle, dst));
    }

    if written.is_empty() {
        return Err(BundleError::NoSources {
            checked: HOST_MINIMAL_PATHS
                .iter()
                .map(|p| resolve_from(host_root, p))
                .collect(),
        });
    }

    // Hash payload.
    let mut files: Vec<BundleFile> = Vec::with_capacity(written.len());
    for (rel, abs) in &written {
        let (hash, size) = hash_file(abs)?;
        files.push(BundleFile {
            path: rel.clone(),
            hash,
            size,
        });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let root_hash = compute_root_hash(&files);

    let manifest = BundleManifest {
        version: version.to_string(),
        root_hash,
        files,
        produced_at: unix_now(),
    };

    // Persist bundle.toml.
    let toml = toml::to_string_pretty(&manifest)
        .map_err(|e| BundleError::ManifestParse(e.to_string()))?;
    fs::write(out.join(BUNDLE_MANIFEST_FILENAME), toml)?;

    Ok(manifest)
}

/// Verify an existing bundle: recompute every file's hash + the root
/// hash, then compare against `bundle.toml`. Returns the parsed
/// manifest on success so callers can print / inspect it.
pub fn verify_bundle(root: &Path) -> Result<BundleManifest, BundleError> {
    let manifest_path = root.join(BUNDLE_MANIFEST_FILENAME);
    if !manifest_path.exists() {
        return Err(BundleError::ManifestMissing {
            path: manifest_path,
        });
    }
    let text = fs::read_to_string(&manifest_path)?;
    let manifest: BundleManifest = toml::from_str(&text)
        .map_err(|e| BundleError::ManifestParse(e.to_string()))?;

    // Recompute per-file hashes; also compare against what's actually
    // on disk. Any file listed in bundle.toml but missing on disk is
    // a corruption; any file on disk that isn't listed is likewise
    // suspect (unknown-provenance content in a supposedly
    // content-addressed dir).
    let mut on_disk: BTreeMap<String, PathBuf> = BTreeMap::new();
    walk_payload(root, &mut on_disk)?;
    // The manifest itself lives at the root; don't treat it as payload.
    on_disk.remove(BUNDLE_MANIFEST_FILENAME);

    for expected in &manifest.files {
        let abs = root.join(&expected.path);
        if !abs.exists() {
            return Err(BundleError::FileMissing {
                path: expected.path.clone(),
            });
        }
        let (hash, _size) = hash_file(&abs)?;
        if hash != expected.hash {
            return Err(BundleError::HashMismatch {
                expected: expected.hash.clone(),
                computed: hash,
            });
        }
        on_disk.remove(&expected.path);
    }
    if let Some((path, _)) = on_disk.into_iter().next() {
        return Err(BundleError::FileUnlisted { path });
    }

    // Root hash.
    let mut sorted = manifest.files.clone();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let computed = compute_root_hash(&sorted);
    if computed != manifest.root_hash {
        return Err(BundleError::HashMismatch {
            expected: manifest.root_hash.clone(),
            computed,
        });
    }

    Ok(manifest)
}

/// List the immediate contents of the bundle root that constitute the
/// payload (used by `verify_bundle`). Recursive.
fn walk_payload(root: &Path, out: &mut BTreeMap<String, PathBuf>) -> std::io::Result<()> {
    fn recurse(
        base: &Path,
        cur: &Path,
        out: &mut BTreeMap<String, PathBuf>,
    ) -> std::io::Result<()> {
        for entry in fs::read_dir(cur)? {
            let entry = entry?;
            let path = entry.path();
            let ty = entry.file_type()?;
            if ty.is_dir() {
                recurse(base, &path, out)?;
            } else if ty.is_file() {
                let rel = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, path);
            }
            // symlinks / other: ignore. bundles should be plain files.
        }
        Ok(())
    }
    recurse(root, root, out)
}

/// BLAKE3 root: hash the newline-delimited `"<path>\t<file-hash>"`
/// concatenation, sorted by `path`. Stable + independent of file
/// mtime / permissions / directory-traversal order.
fn compute_root_hash(files: &[BundleFile]) -> String {
    let mut hasher = blake3::Hasher::new();
    for f in files {
        hasher.update(f.path.as_bytes());
        hasher.update(b"\t");
        hasher.update(f.hash.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

fn hash_file(path: &Path) -> Result<(String, u64), BundleError> {
    let mut f = fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 8192];
    let mut total = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    Ok((hasher.finalize().to_hex().to_string(), total))
}

fn copy_file(src: &Path, dst: &Path) -> Result<(), BundleError> {
    // `fs::copy` follows symlinks by default — we WANT that: the
    // point of the bundle is to capture the actual bytes, so mounting
    // it into a sandbox doesn't depend on the symlink's target being
    // reachable.
    fs::copy(src, dst)?;
    Ok(())
}

fn resolve_from(host_root: &Path, abs: &str) -> PathBuf {
    if host_root == Path::new("/") {
        return PathBuf::from(abs);
    }
    // Strip the leading slash so `join()` doesn't discard host_root.
    let rel = abs.trim_start_matches('/');
    host_root.join(rel)
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fake a host tree with a couple of "loader" files, build a
    /// bundle from it, then verify.
    #[test]
    fn build_then_verify_roundtrips() {
        let host = tempfile::tempdir().unwrap();
        // Create fake libc + loader files under a fake host root.
        for rel in ["lib64/ld-linux-x86-64.so.2", "lib64/libc.so.6", "lib64/libm.so.6"] {
            let p = host.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("fake contents of {rel}")).unwrap();
        }
        let out = tempfile::tempdir().unwrap();
        let manifest =
            build_bundle(out.path(), "2026.07", Some(host.path())).expect("build");
        assert_eq!(manifest.version, "2026.07");
        assert!(!manifest.files.is_empty());
        assert_eq!(manifest.root_hash.len(), 64);

        // Verify against the same tree.
        let verified = verify_bundle(out.path()).expect("verify");
        assert_eq!(verified.root_hash, manifest.root_hash);
        assert_eq!(verified.files.len(), manifest.files.len());
    }

    #[test]
    fn verify_detects_a_modified_byte() {
        let host = tempfile::tempdir().unwrap();
        for rel in ["lib64/ld-linux-x86-64.so.2", "lib64/libc.so.6"] {
            let p = host.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("fake contents of {rel}")).unwrap();
        }
        let out = tempfile::tempdir().unwrap();
        build_bundle(out.path(), "2026.07", Some(host.path())).expect("build");
        // Tamper with one byte in the copied libc.
        let libc_in_bundle = out.path().join("lib/lib64/libc.so.6");
        let mut bytes = fs::read(&libc_in_bundle).unwrap();
        bytes[0] ^= 0xff;
        fs::write(&libc_in_bundle, bytes).unwrap();

        let err = verify_bundle(out.path()).unwrap_err();
        match err {
            BundleError::HashMismatch { .. } => {}
            other => panic!("expected HashMismatch, got {other:?}"),
        }
    }

    #[test]
    fn verify_detects_unlisted_file() {
        let host = tempfile::tempdir().unwrap();
        for rel in ["lib64/ld-linux-x86-64.so.2", "lib64/libc.so.6"] {
            let p = host.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("fake contents of {rel}")).unwrap();
        }
        let out = tempfile::tempdir().unwrap();
        build_bundle(out.path(), "2026.07", Some(host.path())).expect("build");
        // Inject a bogus file into the payload.
        fs::write(out.path().join("lib/lib64/extra.so"), b"surprise").unwrap();
        let err = verify_bundle(out.path()).unwrap_err();
        match err {
            BundleError::FileUnlisted { .. } => {}
            other => panic!("expected FileUnlisted, got {other:?}"),
        }
    }

    #[test]
    fn build_fails_when_no_sources_resolve() {
        let host = tempfile::tempdir().unwrap();
        // Empty host — nothing to copy.
        let out = tempfile::tempdir().unwrap();
        let err = build_bundle(out.path(), "0.0.1", Some(host.path())).unwrap_err();
        match err {
            BundleError::NoSources { .. } => {}
            other => panic!("expected NoSources, got {other:?}"),
        }
    }

    #[test]
    fn root_hash_is_deterministic() {
        // Building twice from identical inputs yields the same root
        // hash, even though produced_at differs.
        let host = tempfile::tempdir().unwrap();
        for rel in ["lib64/ld-linux-x86-64.so.2", "lib64/libc.so.6"] {
            let p = host.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("fake contents of {rel}")).unwrap();
        }
        let out1 = tempfile::tempdir().unwrap();
        let out2 = tempfile::tempdir().unwrap();
        let a = build_bundle(out1.path(), "v", Some(host.path())).unwrap();
        let b = build_bundle(out2.path(), "v", Some(host.path())).unwrap();
        assert_eq!(a.root_hash, b.root_hash);
    }

    #[test]
    fn default_bundle_path_uses_data_dir() {
        // Just check the shape: qos-runtime/<v> under the resolved
        // data dir. If HOME/XDG_DATA_HOME are both unset in CI this
        // returns an error, which is also fine (we just skip).
        if let Ok(p) = default_bundle_path("v") {
            assert!(p.ends_with("qos-runtime/v"));
        }
    }
}
