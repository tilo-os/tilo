//! Reusing runtime payload bytes the device already has.
//!
//! Successive runtime builds share most of their files; only the toolkit dylib changes with every
//! build. Before going to the network the installer looks for each blob in the installed bundles —
//! the image's and the user's — whose `bundle.toml` lists every file with its BLAKE3 hash.
//!
//! A listing is only a hint where to look. Bytes are taken from a local file only when they hash to
//! the blob the signed package names, checked while they are copied; a file that changed, or a
//! manifest that lies, costs a download, never a wrong byte.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use tilo_app_package::BlobHash;
use tilo_app_package::blobs::HashingReader;

/// Blob hash to the paths that may hold it, with the size the manifest records.
#[derive(Debug, Default)]
pub struct LocalRuntimeBlobs {
    files: BTreeMap<BlobHash, Vec<(PathBuf, u64)>>,
}

impl LocalRuntimeBlobs {
    /// Index every bundle directly under each of `roots` (directories holding `<version>/bundle.toml`).
    /// Unreadable roots, bundles and manifests are skipped: they only mean fewer bytes to reuse.
    #[must_use]
    pub fn index(roots: &[PathBuf]) -> Self {
        let mut files: BTreeMap<BlobHash, Vec<(PathBuf, u64)>> = BTreeMap::new();
        for root in roots {
            let Ok(bundles) = fs::read_dir(root) else {
                continue;
            };
            for bundle in bundles.flatten() {
                let dir = bundle.path();
                let Ok(text) =
                    fs::read_to_string(dir.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME))
                else {
                    continue;
                };
                let Ok(manifest) = toml::from_str::<runtime_bundle::BundleManifest>(&text) else {
                    continue;
                };
                for file in manifest.files {
                    let (Some(hash), Some(relative)) =
                        (BlobHash::from_hex(&file.hash), contained(&file.path))
                    else {
                        continue;
                    };
                    files
                        .entry(hash)
                        .or_default()
                        .push((dir.join(relative), file.size));
                }
            }
        }
        Self { files }
    }

    /// The bundle roots this device has: the device's own store (or
    /// [`runtime_bundle::RUNTIME_DIR_ENV`]'s) and the image's.
    #[must_use]
    pub fn installed() -> Self {
        let mut roots = Vec::new();
        if let Ok(root) = runtime_bundle::bundle_root() {
            roots.push(root);
        }
        // `/usr/lib/tilo-runtime`, the directory every system bundle is in.
        roots.push(runtime_bundle::system_bundle_path(""));
        Self::index(&roots)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Copy each of `wanted` that a local file holds into `dir` as `<dir>/<hash hex>`, verified
    /// against its hash; return the ones still to fetch with the number of bytes reused.
    ///
    /// # Errors
    ///
    /// When `dir` cannot be written. A local file that cannot be read or does not hash to its blob
    /// is skipped, not an error.
    pub fn copy_into(&self, wanted: &[BlobHash], dir: &Path) -> io::Result<(Vec<BlobHash>, u64)> {
        let mut missing = Vec::new();
        let mut reused = 0u64;
        for hash in wanted {
            let mut copied = None;
            for (path, size) in self.files.get(hash).into_iter().flatten() {
                if let Some(len) = copy_verified(path, *size, hash, dir)? {
                    copied = Some(len);
                    break;
                }
            }
            match copied {
                Some(len) => reused = reused.saturating_add(len),
                None => missing.push(*hash),
            }
        }
        Ok((missing, reused))
    }
}

/// `path` as a relative path that stays inside its bundle, or `None`.
fn contained(path: &str) -> Option<PathBuf> {
    let relative = Path::new(path);
    relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
        .then(|| relative.to_path_buf())
        .filter(|relative| !relative.as_os_str().is_empty())
}

/// Copy `source` to `<dir>/<hash>` when its bytes are exactly the `size` bytes named `hash`;
/// `Ok(None)` (with nothing left behind) when they are not or cannot be read.
fn copy_verified(source: &Path, size: u64, hash: &BlobHash, dir: &Path) -> io::Result<Option<u64>> {
    let Ok(file) = fs::File::open(source) else {
        return Ok(None);
    };
    let partial = dir.join(format!("{}.local", hash.to_hex()));
    let mut out = fs::File::create(&partial)?;
    // One byte past the recorded size, so a grown file is caught without reading all of it.
    let mut hashing = HashingReader::new(file.take(size.saturating_add(1)));
    let copied = io::copy(&mut hashing, &mut out).and_then(|_| out.flush());
    let (actual, len) = hashing.finish();
    if copied.is_err() || len != size || actual != *hash {
        drop(out);
        fs::remove_file(&partial)?;
        return Ok(None);
    }
    drop(out);
    fs::rename(&partial, dir.join(hash.to_hex()))?;
    Ok(Some(len))
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn local_runtime_blobs_are_copied_only_when_verified;

        phase manifest_paths_never_leave_their_bundle;
        {
            assert_eq!(
                contained("lib/libc.so.6"),
                Some(PathBuf::from("lib/libc.so.6"))
            );
            assert_eq!(contained("../outside"), None);
            assert_eq!(contained("/etc/shadow"), None);
            assert_eq!(contained("lib/../../outside"), None);
            assert_eq!(contained(""), None);
        }
    }
}
