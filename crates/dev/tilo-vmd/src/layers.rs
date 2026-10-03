//! Content-addressed, immutable sysext layers under `<root>/layers/<sha256>/`.
//!
//! A layer is imported into a private staging directory, validated, hashed
//! and only then renamed into place, so a reader never sees a partial layer
//! and an interrupted import leaves nothing behind but staging debris.
//!
//! `SELinux` labels are part of the layer format: the `label` callback runs
//! on the staged tree before it is published (see [`crate::apply::Apply::label_layer`]).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use tilo_vm_protocol::{LayerInfo, Provenance};

use crate::error::DaemonError;
use crate::fsutil::{self, write_atomic};

/// Top-level trees systemd-sysext merges; anything else is rejected.
const ALLOWED_TOP: [&str; 2] = ["usr", "opt"];
/// Written by the importer, so excluded from the content hash.
const RELEASE_DIR: &str = "usr/lib/extension-release.d";
/// Metadata beside the tree, also excluded from the hash.
const PROVENANCE: &str = "provenance.json";

/// What [`LayerStore::collect`] removed, and what it kept for its age.
#[derive(Debug, Default)]
pub struct Collected {
    pub removed: Vec<String>,
    pub freed: u64,
    /// Unreferenced layers younger than the minimum age, with their age.
    pub young: Vec<(String, std::time::Duration)>,
}

/// One file or symlink in a layer, as the slot sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerFile {
    /// Absolute, e.g. `/usr/bin/shell`.
    pub path: String,
    pub sha256: Option<String>,
    pub link: Option<String>,
    pub executable: bool,
}

/// Labels a staged layer tree in place.
pub type Labeler<'a> = &'a mut dyn FnMut(&Path) -> Result<(), DaemonError>;

pub struct LayerStore {
    layers: PathBuf,
    staging: PathBuf,
}

impl LayerStore {
    pub fn new(root: &Path) -> Self {
        Self {
            layers: root.join("layers"),
            staging: root.join("staging"),
        }
    }

    pub fn path(&self, hash: &str) -> PathBuf {
        self.layers.join(hash)
    }

    /// # Errors
    /// [`DaemonError::LayerNotFound`] for unknown or malformed hashes.
    pub fn require(&self, hash: &str) -> Result<PathBuf, DaemonError> {
        let path = self.path(hash);
        if is_hash(hash) && path.is_dir() {
            Ok(path)
        } else {
            Err(DaemonError::LayerNotFound(hash.to_owned()))
        }
    }

    /// Import an uncompressed tar stream.
    ///
    /// # Errors
    /// Malformed archives, disallowed entries and I/O failures.
    pub fn import_tar(
        &self,
        reader: impl Read,
        provenance: &Provenance,
        label: Labeler<'_>,
    ) -> Result<(String, bool), DaemonError> {
        let stage = self.stage()?;
        let result = (|| {
            let mut archive = tar::Archive::new(reader);
            archive.set_preserve_permissions(true);
            archive.set_preserve_ownerships(false);
            archive.set_unpack_xattrs(false);
            archive
                .unpack(&stage)
                .map_err(|e| DaemonError::InvalidLayer(format!("cannot unpack archive: {e}")))?;
            self.commit(&stage, provenance, label)
        })();
        cleanup(&stage);
        result
    }

    /// Import a directory, or a tarball, that already exists in the VM.
    ///
    /// # Errors
    /// As [`Self::import_tar`].
    pub fn import_path(
        &self,
        source: &Path,
        provenance: &Provenance,
        label: Labeler<'_>,
    ) -> Result<(String, bool), DaemonError> {
        if source.is_dir() {
            let stage = self.stage()?;
            let result = fsutil::copy_tree(source, &stage)
                .map_err(DaemonError::io(format!("copy {}", source.display())))
                .and_then(|()| self.commit(&stage, provenance, label));
            cleanup(&stage);
            result
        } else {
            let file = fs::File::open(source)
                .map_err(DaemonError::io(format!("open {}", source.display())))?;
            self.import_tar(file, provenance, label)
        }
    }

    /// # Errors
    /// I/O failures while reading the store.
    pub fn list(&self) -> Result<Vec<LayerInfo>, DaemonError> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.layers) else {
            return Ok(out);
        };
        for entry in entries {
            let entry = entry.map_err(DaemonError::io("list layers"))?;
            let hash = entry.file_name().to_string_lossy().into_owned();
            if !is_hash(&hash) {
                continue;
            }
            let provenance = self
                .provenance(&hash)
                .unwrap_or_else(|| Provenance::local("unknown"));
            out.push(LayerInfo { hash, provenance });
        }
        out.sort_by(|a, b| a.hash.cmp(&b.hash));
        Ok(out)
    }

    pub fn provenance(&self, hash: &str) -> Option<Provenance> {
        let bytes = fs::read(self.path(hash).join(PROVENANCE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Physically allocated bytes of every layer.
    pub fn allocated_bytes(&self) -> u64 {
        fsutil::allocated_bytes(&self.layers)
    }

    /// Delete unreferenced layers older than `min_age` (younger ones may be
    /// an import whose swap has not happened yet). Returns hashes and bytes.
    ///
    /// # Errors
    /// I/O failures.
    pub fn collect(
        &self,
        referenced: &std::collections::BTreeSet<String>,
        min_age: std::time::Duration,
        dry_run: bool,
    ) -> Result<Collected, DaemonError> {
        let mut removed = Vec::new();
        let mut young = Vec::new();
        let mut freed: u64 = 0;
        for layer in self.list()? {
            let path = self.path(&layer.hash);
            if referenced.contains(&layer.hash) {
                continue;
            }
            // A clock step makes the age unknown: treat it as brand new.
            let age = fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .unwrap_or_default();
            if age < min_age {
                young.push((layer.hash, age));
                continue;
            }
            freed = freed.saturating_add(fsutil::allocated_bytes(&path));
            if !dry_run {
                // Move out of the store first so no reader sees a half-deleted layer.
                fs::create_dir_all(&self.staging).map_err(DaemonError::io("create staging"))?;
                let doomed = self.staging.join(format!("gc-{}", layer.hash));
                fs::rename(&path, &doomed)
                    .map_err(DaemonError::io(format!("remove layer {}", layer.hash)))?;
                fs::remove_dir_all(&doomed)
                    .map_err(DaemonError::io(format!("remove layer {}", layer.hash)))?;
            }
            removed.push(layer.hash);
        }
        Ok(Collected {
            removed,
            freed,
            young,
        })
    }

    /// Every file and symlink the layer adds, sorted by path, with the
    /// SHA-256 of each regular file; the importer's own metadata is left out.
    ///
    /// # Errors
    /// Unknown layers and I/O failures.
    pub fn files(&self, hash: &str) -> Result<Vec<LayerFile>, DaemonError> {
        let root = self.require(hash)?;
        let mut entries = Vec::new();
        fsutil::walk(&root, Path::new(""), &mut entries).map_err(DaemonError::io("walk layer"))?;
        entries.sort();
        let mut files = Vec::new();
        for relative in entries {
            let text = relative.to_string_lossy().replace('\\', "/");
            if text == PROVENANCE
                || text == RELEASE_DIR
                || text.starts_with(&format!("{RELEASE_DIR}/"))
            {
                continue;
            }
            let path = root.join(&relative);
            let meta = fs::symlink_metadata(&path).map_err(DaemonError::io("stat layer entry"))?;
            let (sha256, link) = if meta.file_type().is_symlink() {
                let target = fs::read_link(&path).map_err(DaemonError::io("read link"))?;
                (None, Some(target.to_string_lossy().into_owned()))
            } else if meta.is_file() {
                (Some(file_digest(&path)?), None)
            } else {
                continue;
            };
            files.push(LayerFile {
                path: format!("/{text}"),
                sha256,
                link,
                executable: meta.is_file() && fsutil::is_executable(&meta),
            });
        }
        Ok(files)
    }

    fn stage(&self) -> Result<PathBuf, DaemonError> {
        fs::create_dir_all(&self.staging).map_err(DaemonError::io("create staging"))?;
        fs::create_dir_all(&self.layers).map_err(DaemonError::io("create layer store"))?;
        let stage = self.staging.join(uuid::Uuid::new_v4().simple().to_string());
        fs::create_dir(&stage).map_err(DaemonError::io("create staging"))?;
        Ok(stage)
    }

    fn commit(
        &self,
        stage: &Path,
        provenance: &Provenance,
        label: Labeler<'_>,
    ) -> Result<(String, bool), DaemonError> {
        validate_top(stage)?;
        if !stage.join("usr").is_dir() {
            return Err(DaemonError::InvalidLayer("no usr/ directory".into()));
        }
        let hash = tree_hash(stage)?;
        let target = self.path(&hash);
        if target.is_dir() {
            return Ok((hash, true));
        }
        // systemd matches `extension-release.<NAME>` against the image name,
        // which for a directory is its basename: the content hash.
        let release_dir = stage.join(RELEASE_DIR);
        fs::create_dir_all(&release_dir).map_err(DaemonError::io("create extension-release.d"))?;
        let release = format!("ID=_any\nTILO_VM_COMPONENT={}\n", provenance.component);
        write_atomic(
            &release_dir.join(format!("extension-release.{hash}")),
            release.as_bytes(),
        )?;
        let provenance = serde_json::to_vec_pretty(provenance)
            .map_err(|e| DaemonError::InvalidParams(e.to_string()))?;
        write_atomic(&stage.join(PROVENANCE), &provenance)?;
        label(stage)?;
        match fs::rename(stage, &target) {
            Ok(()) => Ok((hash, false)),
            // A concurrent import of identical content won the rename.
            Err(_) if target.is_dir() => Ok((hash, true)),
            Err(e) => Err(DaemonError::io("publish layer")(e)),
        }
    }
}

pub fn is_hash(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn cleanup(stage: &Path) {
    if stage.exists() {
        let _ = fs::remove_dir_all(stage);
    }
}

fn validate_top(stage: &Path) -> Result<(), DaemonError> {
    for entry in fs::read_dir(stage).map_err(DaemonError::io("read staging"))? {
        let entry = entry.map_err(DaemonError::io("read staging"))?;
        let name = entry.file_name();
        if !ALLOWED_TOP.iter().any(|allowed| name == *allowed) {
            return Err(DaemonError::InvalidLayer(format!(
                "top-level entry {} is outside usr/ and opt/",
                name.to_string_lossy()
            )));
        }
    }
    Ok(())
}

/// SHA-256 over a canonical walk: files and symlinks sorted by path bytes,
/// each hashed as kind, path and content (file digest or link target). Ownership and
/// timestamps are ignored; the executable bit is not. The importer's own
/// outputs (extension-release, provenance) are excluded, so a published
/// layer hashes to its directory name.
///
/// # Errors
/// Special files and I/O failures.
pub fn tree_hash(root: &Path) -> Result<String, DaemonError> {
    let mut entries = Vec::new();
    fsutil::walk(root, Path::new(""), &mut entries).map_err(DaemonError::io("walk layer"))?;
    entries.sort();
    let mut hasher = Sha256::new();
    for relative in entries {
        let text = relative.to_string_lossy().replace('\\', "/");
        if text == PROVENANCE || text == RELEASE_DIR || text.starts_with(&format!("{RELEASE_DIR}/"))
        {
            continue;
        }
        let path = root.join(&relative);
        let meta = fs::symlink_metadata(&path).map_err(DaemonError::io("stat layer entry"))?;
        let kind = meta.file_type();
        let (tag, content) = if kind.is_dir() {
            // Directories are implied by the paths below them; empty ones
            // carry nothing a sysext needs.
            continue;
        } else if kind.is_symlink() {
            let target = fs::read_link(&path).map_err(DaemonError::io("read link"))?;
            ("l", target.to_string_lossy().into_owned())
        } else if kind.is_file() {
            let tag = if fsutil::is_executable(&meta) {
                "x"
            } else {
                "f"
            };
            (tag, file_digest(&path)?)
        } else {
            return Err(DaemonError::InvalidLayer(format!(
                "{text} is not a file, directory or symlink"
            )));
        };
        for part in [tag, text.as_str(), content.as_str()] {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn file_digest(path: &Path) -> Result<String, DaemonError> {
    use std::io::BufRead as _;
    let file = fs::File::open(path).map_err(DaemonError::io("read layer file"))?;
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Sha256::new();
    loop {
        let chunk = reader
            .fill_buf()
            .map_err(DaemonError::io("read layer file"))?;
        if chunk.is_empty() {
            break;
        }
        hasher.update(chunk);
        let read = chunk.len();
        reader.consume(read);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn layer_imports_are_content_addressed_and_atomic() {
        // Phase `labeler_sees_the_staged_tree_before_publication`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = LayerStore::new(root.path());
            let mut seen = Vec::new();
            let (hash, _) = store
                .import_tar(
                    tarball(&[("usr/bin/shell", b"v1")]).as_slice(),
                    &provenance(),
                    &mut |path| {
                        assert!(path.join("usr/bin/shell").is_file());
                        assert!(path.join(PROVENANCE).is_file());
                        seen.push(path.to_owned());
                        Ok(())
                    },
                )
                .unwrap();
            assert_eq!(seen.len(), 1);
            assert!(seen[0].starts_with(root.path().join("staging")));
            // A failing labeler means no layer.
            let error = store
                .import_tar(
                    tarball(&[("usr/bin/other", b"v1")]).as_slice(),
                    &provenance(),
                    &mut |_| {
                        Err(DaemonError::Apply {
                            step: "setfiles".into(),
                            detail: "denied".into(),
                        })
                    },
                )
                .unwrap_err();
            assert!(matches!(error, DaemonError::Apply { .. }));
            assert_eq!(store.list().unwrap().len(), 1);
            assert_eq!(store.list().unwrap()[0].hash, hash);
        }
        // Phase `identical_content_is_one_layer`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = LayerStore::new(root.path());
            let tar = tarball(&[("usr/bin/shell", b"v1")]);
            let (a, existed_a) = store
                .import_tar(tar.as_slice(), &provenance(), &mut |_| Ok(()))
                .unwrap();
            let (b, existed_b) = store
                .import_tar(tar.as_slice(), &provenance(), &mut |_| Ok(()))
                .unwrap();
            assert_eq!(a, b);
            assert!(!existed_a);
            assert!(existed_b);
            assert!(is_hash(&a));
            let layer = store.require(&a).unwrap();
            assert_eq!(fs::read(layer.join("usr/bin/shell")).unwrap(), b"v1");
            let release =
                fs::read_to_string(layer.join(format!("{RELEASE_DIR}/extension-release.{a}")))
                    .unwrap();
            assert!(release.starts_with("ID=_any\n"));
            assert!(layer.join("provenance.json").is_file());
            // The published layer hashes to its own name: the release file is excluded.
            assert_eq!(tree_hash(&layer).unwrap(), a);
            assert_eq!(store.list().unwrap().len(), 1);
            assert_eq!(
                fs::read_dir(root.path().join("staging")).unwrap().count(),
                0
            );
        }
        // Phase `different_content_different_hash`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = LayerStore::new(root.path());
            let (a, _) = store
                .import_tar(
                    tarball(&[("usr/bin/shell", b"v1")]).as_slice(),
                    &provenance(),
                    &mut |_| Ok(()),
                )
                .unwrap();
            let (b, _) = store
                .import_tar(
                    tarball(&[("usr/bin/shell", b"v2")]).as_slice(),
                    &provenance(),
                    &mut |_| Ok(()),
                )
                .unwrap();
            let (c, _) = store
                .import_tar(
                    tarball(&[("usr/bin/shel", b"v1")]).as_slice(),
                    &provenance(),
                    &mut |_| Ok(()),
                )
                .unwrap();
            assert_ne!(a, b);
            assert_ne!(a, c);
        }
        // Phase `rejected_import_leaves_nothing`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = LayerStore::new(root.path());
            let error = store
                .import_tar(
                    tarball(&[("etc/passwd", b"x")]).as_slice(),
                    &provenance(),
                    &mut |_| Ok(()),
                )
                .unwrap_err();
            assert!(matches!(error, DaemonError::InvalidLayer(_)));
            let error = store
                .import_tar(
                    &b"not a tar at all, definitely"[..],
                    &provenance(),
                    &mut |_| Ok(()),
                )
                .unwrap_err();
            assert!(matches!(error, DaemonError::InvalidLayer(_)));
            assert!(store.list().unwrap().is_empty());
            assert_eq!(
                fs::read_dir(root.path().join("staging")).unwrap().count(),
                0
            );
            assert_eq!(fs::read_dir(root.path().join("layers")).unwrap().count(), 0);
        }
        // Phase `directory_import_matches_tar_import`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = LayerStore::new(root.path());
            let (from_tar, _) = store
                .import_tar(
                    tarball(&[("usr/bin/shell", b"v1")]).as_slice(),
                    &provenance(),
                    &mut |_| Ok(()),
                )
                .unwrap();
            let source = tempfile::tempdir().unwrap();
            fs::create_dir_all(source.path().join("usr/bin")).unwrap();
            fs::write(source.path().join("usr/bin/shell"), b"v1").unwrap();
            fsutil::set_executable(&source.path().join("usr/bin/shell")).unwrap();
            let (from_dir, existed) = store
                .import_path(source.path(), &provenance(), &mut |_| Ok(()))
                .unwrap();
            assert_eq!(from_tar, from_dir);
            assert!(existed);
        }
    }

    fn provenance() -> Provenance {
        Provenance::local("shell")
    }

    fn tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn layer_lookup_listing_and_collection() {
        // Phase `collect_removes_only_old_unreferenced_layers`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = LayerStore::new(root.path());
            let (keep, _) = store
                .import_tar(
                    tarball(&[("usr/bin/a", b"1")]).as_slice(),
                    &provenance(),
                    &mut |_| Ok(()),
                )
                .unwrap();
            let (drop, _) = store
                .import_tar(
                    tarball(&[("usr/bin/b", b"2")]).as_slice(),
                    &provenance(),
                    &mut |_| Ok(()),
                )
                .unwrap();
            let referenced = std::collections::BTreeSet::from([keep.clone()]);
            // Too young: an import waiting for its swap survives.
            let young = store
                .collect(&referenced, std::time::Duration::from_secs(3600), false)
                .unwrap();
            assert!(young.removed.is_empty());
            // ... and is reported, so gc can say why it stayed.
            assert_eq!(young.young.len(), 1);
            assert_eq!(young.young[0].0, drop);
            let dry = store
                .collect(&referenced, std::time::Duration::ZERO, true)
                .unwrap();
            assert_eq!(dry.removed, std::slice::from_ref(&drop));
            assert!(dry.freed > 0);
            assert!(dry.young.is_empty());
            assert!(store.require(&drop).is_ok(), "dry run keeps the layer");
            store
                .collect(&referenced, std::time::Duration::ZERO, false)
                .unwrap();
            assert!(store.require(&drop).is_err());
            assert!(store.require(&keep).is_ok());
        }
        // Phase `files_lists_what_the_slot_sees_with_digests`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = LayerStore::new(root.path());
            let (hash, _) = store
                .import_tar(
                    tarball(&[("usr/bin/shell", b"v1"), ("usr/share/tilo/x.toml", b"k")])
                        .as_slice(),
                    &provenance(),
                    &mut |_| Ok(()),
                )
                .unwrap();
            let files = store.files(&hash).unwrap();
            let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
            // The importer's release file and provenance are not the layer's content.
            assert_eq!(paths, ["/usr/bin/shell", "/usr/share/tilo/x.toml"]);
            let digest = hex::encode(Sha256::digest(b"v1"));
            assert_eq!(files[0].sha256.as_deref(), Some(digest.as_str()));
            assert!(files[0].executable);
            assert!(store.files("0".repeat(64).as_str()).is_err());
        }
        // Phase `unknown_hash_is_not_found`.
        {
            let root = tempfile::tempdir().unwrap();
            let store = LayerStore::new(root.path());
            assert!(matches!(
                store.require("../etc"),
                Err(DaemonError::LayerNotFound(_))
            ));
            assert!(matches!(
                store.require(&"a".repeat(64)),
                Err(DaemonError::LayerNotFound(_))
            ));
        }
    }
}
