//! Environment images: `env-runtime` and `tool` packages.
//!
//! An image is a directory tree that becomes part of an environment's `/usr`
//! and `/etc`. A payload [`Collection`] names regular files only, so the
//! rest of a tree — which files are executable, its symbolic links and its
//! empty directories — travels in one more payload file, [`TREE_FILE`], at
//! the root of the payload. Being a payload file, it is covered by the
//! collection hash and so by the publisher's signature.
//!
//! [`seal_image`] turns a directory into a signed image; whoever
//! materialises one ([`TreeSpec::apply`]) recreates the tree exactly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::blobs::Collection;
use crate::id::{Kind, PackageId};
use crate::manifest::{AArch64Level, Arch, Image, Manifest, X86_64Level};
use crate::pack::{PackError, Packed, SigningIdentity, pack_with_files};
use crate::package::is_safe_path;

/// The payload file describing everything about an image's tree that is not
/// a regular file's bytes.
pub const TREE_FILE: &str = ".tilo-tree.toml";

/// Why an image could not be sealed or its tree is malformed.
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("{0} is not an image kind; use env-runtime or tool")]
    NotAnImage(Kind),
    #[error("{path}: {what} cannot be part of an image")]
    UnsupportedFile { path: String, what: &'static str },
    #[error("{0:?} is not valid UTF-8 — image paths must be")]
    NonUtf8Path(std::path::PathBuf),
    #[error("{TREE_FILE}: {0}")]
    BadTree(String),
    #[error("tree walk: {0}")]
    Walk(#[from] walkdir::Error),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Pack(#[from] PackError),
}

/// Everything about an image's tree beyond its regular files' bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeSpec {
    /// Regular files materialised with their execute bits set.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub executable: BTreeSet<String>,
    /// Symbolic links, path → target, created verbatim. A target is read
    /// inside the environment, never on the host.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub symlinks: BTreeMap<String, String>,
    /// Directories that hold nothing else in the payload.
    #[serde(
        default,
        rename = "empty-dirs",
        skip_serializing_if = "BTreeSet::is_empty"
    )]
    pub empty_dirs: BTreeSet<String>,
}

impl TreeSpec {
    /// Read the tree under `root`: execute bits, links and empty
    /// directories. Ownership, timestamps and special mode bits are not
    /// part of an image; device nodes, sockets and FIFOs are refused.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::UnsupportedFile`] for a file that is not a
    /// regular file, directory or symbolic link, [`ImageError::NonUtf8Path`]
    /// for a path that is not UTF-8, or the walk's I/O error.
    pub fn scan(root: &Path) -> Result<Self, ImageError> {
        let mut spec = Self::default();
        for entry in WalkDir::new(root).follow_links(false).min_depth(1) {
            let entry = entry?;
            let rel = entry
                .path()
                .strip_prefix(root)
                .map_err(|_| ImageError::NonUtf8Path(entry.path().to_path_buf()))?;
            let rel = rel
                .to_str()
                .ok_or_else(|| ImageError::NonUtf8Path(rel.to_path_buf()))?
                .replace(std::path::MAIN_SEPARATOR, "/");
            let kind = entry.file_type();
            if kind.is_symlink() {
                let target = std::fs::read_link(entry.path())?;
                let target = target
                    .to_str()
                    .ok_or_else(|| ImageError::NonUtf8Path(target.clone()))?
                    .to_owned();
                spec.symlinks.insert(rel, target);
            } else if kind.is_dir() {
                if std::fs::read_dir(entry.path())?.next().is_none() {
                    spec.empty_dirs.insert(rel);
                }
            } else if kind.is_file() {
                if is_executable(&entry.metadata()?) {
                    spec.executable.insert(rel);
                }
            } else {
                return Err(ImageError::UnsupportedFile {
                    path: rel,
                    what: "a device node, socket or FIFO",
                });
            }
        }
        Ok(spec)
    }

    /// Decode a tree file.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::BadTree`] when the bytes are not a tree file.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ImageError> {
        let text = std::str::from_utf8(bytes).map_err(|e| ImageError::BadTree(e.to_string()))?;
        toml::from_str(text).map_err(|e| ImageError::BadTree(e.to_string()))
    }

    /// Encode as the tree file's bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::BadTree`] when encoding fails.
    pub fn to_bytes(&self) -> Result<Vec<u8>, ImageError> {
        toml::to_string(self)
            .map(String::into_bytes)
            .map_err(|e| ImageError::BadTree(e.to_string()))
    }

    /// Check the tree against the payload `collection` it ships in: every
    /// path is safe, executables are payload files, and no link or empty
    /// directory takes a payload file's place or sits above any other entry
    /// (so materialising never writes through a link).
    ///
    /// # Errors
    ///
    /// Returns [`ImageError::BadTree`] naming the first offending path.
    pub fn check(&self, collection: &Collection) -> Result<(), ImageError> {
        let bad = |path: &str, why: &str| Err(ImageError::BadTree(format!("{path:?} {why}")));
        let files = &collection.entries;
        if !files.contains_key(TREE_FILE) {
            return bad(TREE_FILE, "is missing from the payload");
        }
        for path in &self.executable {
            if !files.contains_key(path) {
                return bad(path, "is marked executable but is not a payload file");
            }
        }
        for path in self.symlinks.keys().chain(&self.empty_dirs) {
            if !is_safe_path(path) {
                return bad(path, "is not a safe relative path");
            }
            if path == TREE_FILE || files.contains_key(path) {
                return bad(path, "is also a payload file");
            }
        }
        for (path, target) in &self.symlinks {
            if target.is_empty() || target.contains('\0') {
                return bad(path, "has an empty or NUL-containing link target");
            }
            if self.empty_dirs.contains(path) {
                return bad(path, "is both a link and a directory");
            }
        }
        // A link or empty directory must be a leaf: nothing else may live
        // beneath it.
        let leaves: BTreeSet<&str> = self
            .symlinks
            .keys()
            .chain(&self.empty_dirs)
            .map(String::as_str)
            .collect();
        let all = files
            .keys()
            .chain(self.symlinks.keys())
            .chain(&self.empty_dirs);
        for path in all {
            let mut ancestor = path.as_str();
            while let Some((parent, _)) = ancestor.rsplit_once('/') {
                if leaves.contains(parent) {
                    return bad(path, "lies beneath a link or an empty directory");
                }
                ancestor = parent;
            }
        }
        Ok(())
    }

    /// Whether `path` is materialised executable.
    pub fn is_executable(&self, path: &str) -> bool {
        self.executable.contains(path)
    }

    /// Create the empty directories and links under `root`, whose payload
    /// files are already in place. Call [`TreeSpec::check`] first.
    ///
    /// # Errors
    ///
    /// Returns the first I/O error.
    pub fn apply(&self, root: &Path) -> std::io::Result<()> {
        for dir in &self.empty_dirs {
            std::fs::create_dir_all(root.join(dir))?;
        }
        for (path, target) in &self.symlinks {
            let link = root.join(path);
            if let Some(parent) = link.parent() {
                std::fs::create_dir_all(parent)?;
            }
            symlink(target, &link)?;
        }
        Ok(())
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn symlink(target: &str, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn symlink(_target: &str, link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("{}: images are materialised on Unix only", link.display()),
    ))
}

/// What an image is, beyond its tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageSpec {
    /// [`Kind::EnvRuntime`] or [`Kind::Tool`].
    pub kind: Kind,
    /// Reverse-DNS package id, such as `so.tilo.env-runtime`.
    pub id: String,
    /// Semver of this build.
    pub version: String,
    /// Display name, when the id's trailing component is not enough.
    pub name: Option<String>,
    /// The architecture the image's binaries run on.
    pub arch: Arch,
    /// For a tool: the env-runtime package it was built on.
    pub runtime: Option<PackageId>,
    /// The distribution packages the image holds.
    pub packages: Vec<String>,
}

/// The manifest of the image `spec` describes, published by `publisher`.
///
/// # Errors
///
/// Returns [`ImageError::NotAnImage`] when `spec.kind` is not an image kind,
/// or [`ImageError::Pack`] when the resulting manifest fails validation (a
/// tool without a runtime, a malformed version).
pub fn image_manifest(spec: &ImageSpec, publisher: [u8; 32]) -> Result<Manifest, ImageError> {
    if !spec.kind.is_image() {
        return Err(ImageError::NotAnImage(spec.kind));
    }
    let mut manifest = Manifest::from_toml_str(IMAGE_MANIFEST_BASE)
        .map_err(|e| ImageError::Pack(PackError::ManifestEncode(e)))?;
    manifest.package.id.clone_from(&spec.id);
    manifest.package.version.clone_from(&spec.version);
    manifest.package.name.clone_from(&spec.name);
    manifest.package.kind = spec.kind;
    match spec.arch {
        Arch::X86_64 => manifest.package.isa.x86_64 = Some(X86_64Level::Baseline),
        Arch::AArch64 => manifest.package.isa.aarch64 = Some(AArch64Level::Armv8A),
    }
    let mut packages = spec.packages.clone();
    packages.sort();
    manifest.image = Some(Image {
        runtime: spec.runtime,
        packages,
    });
    manifest.publisher.identity = format!("ed25519:{}", hex::encode(publisher));
    crate::validate::validate_manifest(&manifest)
        .map_err(|e| ImageError::Pack(PackError::Manifest(e)))?;
    Ok(manifest)
}

/// What every image manifest starts from: no namespaces of its own, since an
/// image is never launched; the environment that stacks it is confined.
const IMAGE_MANIFEST_BASE: &str = r#"
[package]
id = "so.tilo.image"
version = "0.0.0"
kind = "env-runtime"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"

[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"
"#;

/// Seal the directory `tree` into a signed image.
///
/// The tree is taken as it is: regular files become payload, and
/// [`TREE_FILE`] records execute bits, links and empty directories. `tree`
/// must not itself contain a [`TREE_FILE`] at its root.
///
/// # Errors
///
/// Returns [`ImageError`] when `spec` is not an image, the tree holds a file
/// an image cannot carry, or packing fails.
pub fn seal_image(
    tree: &Path,
    spec: &ImageSpec,
    identity: &SigningIdentity,
) -> Result<Packed, ImageError> {
    let manifest = image_manifest(spec, identity.public_key_bytes())?;
    let tree_spec = TreeSpec::scan(tree)?;
    let packed = pack_with_files(
        tree,
        &manifest,
        identity,
        vec![(TREE_FILE.to_owned(), tree_spec.to_bytes()?)],
    )?;
    tree_spec.check(&packed.meta.collection)?;
    Ok(packed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blobs::BlobHash;

    fn collection(paths: &[&str]) -> Collection {
        Collection::from_entries(
            paths
                .iter()
                .map(|path| ((*path).to_owned(), BlobHash::of(path.as_bytes()))),
        )
    }

    #[test]
    fn a_tree_never_writes_through_a_link() {
        let files = collection(&[TREE_FILE, "usr/bin/bash", "usr/lib64/libc.so.6"]);
        let mut spec = TreeSpec::default();
        spec.executable.insert("usr/bin/bash".into());
        spec.symlinks.insert("usr/bin/sh".into(), "bash".into());
        spec.empty_dirs.insert("usr/share/empty".into());
        spec.check(&files).unwrap();

        let mut above = spec.clone();
        above.symlinks.insert("usr/lib64".into(), "/etc".into());
        assert!(above.check(&files).is_err(), "a link above a payload file");

        let mut shadow = spec.clone();
        shadow.symlinks.insert("usr/bin/bash".into(), "sh".into());
        assert!(shadow.check(&files).is_err(), "a link in a file's place");

        let mut escape = spec.clone();
        escape.symlinks.insert("../outside".into(), "x".into());
        assert!(escape.check(&files).is_err(), "an unsafe link path");

        let mut ghost = spec.clone();
        ghost.executable.insert("usr/bin/zsh".into());
        assert!(
            ghost.check(&files).is_err(),
            "an executable that is not shipped"
        );

        assert!(
            spec.check(&collection(&["usr/bin/bash"])).is_err(),
            "the tree file must ship"
        );
    }

    #[test]
    fn the_tree_file_round_trips() {
        let mut spec = TreeSpec::default();
        spec.executable.insert("usr/bin/bash".into());
        spec.symlinks
            .insert("usr/lib64/libz.so.1".into(), "libz.so.1.3.1".into());
        spec.empty_dirs.insert("etc/dnf/vars".into());
        let bytes = spec.to_bytes().unwrap();
        assert_eq!(TreeSpec::from_bytes(&bytes).unwrap(), spec);
        assert!(TreeSpec::from_bytes(b"surprise = 1").is_err());
    }

    #[test]
    fn image_manifests_bind_tools_to_their_runtime() {
        let runtime = PackageId::from_bytes([3; 32]);
        let spec = ImageSpec {
            kind: Kind::Tool,
            id: "so.tilo.tool.pandoc".into(),
            version: "3.1.11".into(),
            name: None,
            arch: Arch::X86_64,
            runtime: Some(runtime),
            packages: vec!["pandoc-0:3.1.11-1.fc44.x86_64".into()],
        };
        let manifest = image_manifest(&spec, [9; 32]).unwrap();
        assert_eq!(manifest.image.as_ref().unwrap().runtime, Some(runtime));
        assert!(manifest.entrypoint.is_none());
        let reparsed = Manifest::from_toml_str(&manifest.to_toml_string().unwrap()).unwrap();
        assert_eq!(reparsed, manifest);

        let unbound = ImageSpec {
            runtime: None,
            ..spec.clone()
        };
        assert!(image_manifest(&unbound, [9; 32]).is_err());
        let app = ImageSpec {
            kind: Kind::App,
            ..spec
        };
        assert!(matches!(
            image_manifest(&app, [9; 32]),
            Err(ImageError::NotAnImage(Kind::App))
        ));
    }
}
