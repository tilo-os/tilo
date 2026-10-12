//! Runtime payload blobs this device already holds.
//!
//! Successive runtime builds share most of their files (the C library,
//! `libstd`, `libglib-2.0`, `libgstreamer-1.0`, …); only the toolkit dylib
//! changes with every build. A runtime package is thin, so installing one fetches every payload
//! blob it names. Before going to the network the installer looks for each
//! blob in the runtime bundles already installed — the image's and the
//! user's — whose `bundle.toml` lists every file with its BLAKE3 hash.
//!
//! A listing is only a hint where to look. Bytes are taken from a local file
//! only when they hash to the blob the signed package names, checked while
//! they are copied; a file that changed, or a manifest that lies, costs a
//! download, never a wrong byte.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use tilo_app_package::BlobHash;
use tilo_app_package::blobs::HashingReader;

/// Where each blob named in an installed runtime bundle's manifest may be
/// read: candidate paths with the size the manifest records.
#[derive(Debug, Default)]
pub struct LocalRuntimeBlobs {
    files: BTreeMap<BlobHash, Vec<(PathBuf, u64)>>,
}

impl LocalRuntimeBlobs {
    /// Index every bundle directly under each of `roots` (directories holding
    /// `<version>/bundle.toml`). Unreadable roots, bundles and manifests are
    /// skipped: they only mean fewer bytes to reuse.
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

    /// The runtime bundle roots this device installs to and ships: the
    /// user's (or [`runtime_bundle::RUNTIME_DIR_ENV`]'s) and the image's.
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

    /// How many distinct blobs the index knows a place for.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether the index knows no blob.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Copy each of `wanted` that a local file holds into `dir` as
    /// `<dir>/<hash hex>`, verified against its hash, and return the ones
    /// still to fetch with the number of bytes reused.
    ///
    /// # Errors
    ///
    /// Returns an error when `dir` cannot be written. A local file that
    /// cannot be read or does not hash to its blob is skipped, not an error.
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

/// Copy `source` to `<dir>/<hash>` when its bytes are exactly the `size`
/// bytes named `hash`; `Ok(None)` (and nothing left behind) when they are
/// not or cannot be read.
fn copy_verified(source: &Path, size: u64, hash: &BlobHash, dir: &Path) -> io::Result<Option<u64>> {
    let Ok(file) = fs::File::open(source) else {
        return Ok(None);
    };
    let partial = dir.join(format!("{}.local", hash.to_hex()));
    let mut out = fs::File::create(&partial)?;
    // One byte past the recorded size, so a grown file is caught without
    // reading all of it.
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

    /// A bundle under `root/<version>` listing `files` as `(path, bytes)`;
    /// `recorded` overrides the hash a file is listed under.
    fn bundle(root: &Path, version: &str, files: &[(&str, &[u8], Option<BlobHash>)]) {
        let dir = root.join(version);
        let mut listed = Vec::new();
        for (path, bytes, recorded) in files {
            let target = dir.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(&target, bytes).unwrap();
            listed.push(runtime_bundle::BundleFile {
                path: (*path).to_string(),
                hash: recorded.unwrap_or_else(|| BlobHash::of(bytes)).to_hex(),
                size: bytes.len() as u64,
                verity: None,
            });
        }
        let manifest = runtime_bundle::BundleManifest {
            version: version.into(),
            arch: runtime_bundle::host_arch().into(),
            root_hash: "0".repeat(64),
            files: listed,
            produced_at: 0,
            publisher_hex: None,
            signature_hex: None,
            verity_signature_hex: None,
        };
        fs::write(
            dir.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME),
            toml::to_string(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn verified_local_files_are_copied_and_the_rest_left_to_fetch() {
        let root = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let libstd: &[u8] = b"libstd of every build";
        let toolkit: &[u8] = b"toolkit of the old build";
        bundle(
            root.path(),
            "old",
            &[
                ("lib/libstd.so", libstd, None),
                ("lib/libruntime_dylib.so", toolkit, None),
            ],
        );
        let index = LocalRuntimeBlobs::index(&[root.path().to_path_buf()]);
        assert_eq!(index.len(), 2);
        let new_toolkit = BlobHash::of(b"toolkit of the new build");
        let (missing, reused) = index
            .copy_into(&[BlobHash::of(libstd), new_toolkit], out.path())
            .unwrap();
        assert_eq!(missing, vec![new_toolkit]);
        assert_eq!(reused, libstd.len() as u64);
        assert_eq!(
            fs::read(out.path().join(BlobHash::of(libstd).to_hex())).unwrap(),
            libstd
        );
    }

    #[test]
    fn a_local_file_whose_hash_differs_is_never_copied() {
        let root = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let wanted: &[u8] = b"the bytes the signed package names";
        // A manifest that lists other bytes under the wanted hash, as a
        // tampered or half-updated bundle would.
        bundle(
            root.path(),
            "tampered",
            &[(
                "lib/libc.so.6",
                b"the bytes the signed package xxxxx",
                Some(BlobHash::of(wanted)),
            )],
        );
        let index = LocalRuntimeBlobs::index(&[root.path().to_path_buf()]);
        let (missing, reused) = index
            .copy_into(&[BlobHash::of(wanted)], out.path())
            .unwrap();
        assert_eq!(missing, vec![BlobHash::of(wanted)]);
        assert_eq!(reused, 0);
        assert_eq!(
            fs::read_dir(out.path()).unwrap().count(),
            0,
            "nothing left behind"
        );
    }

    #[test]
    fn manifest_paths_never_leave_their_bundle() {
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
