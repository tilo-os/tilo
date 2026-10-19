//! A layer's tree against its manifest.
//!
//! [`describe`] is the endorsing side (tilo-vmd): it seals every file with
//! fs-verity, then reads it, so the SHA-256 and the measured digest it
//! reports are of the same immutable bytes. [`verify_payload`] is the
//! activation's side: it accepts a tree only if it is exactly what a signed
//! manifest lists.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest as _, Sha256};

use crate::manifest::{Entry, LayerManifest, TREES};
use crate::verity;

/// Beside the tree: tilo-vm's record of how the layer was built. Never merged.
pub const PROVENANCE: &str = "provenance.json";
/// The extension-release directory, which the importer writes.
pub const RELEASE_DIR: &str = "usr/lib/extension-release.d";

/// How the files of a verified payload were vouched for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PayloadCheck {
    /// By a kernel-measured fs-verity digest equal to the signed one; unread.
    pub by_verity: usize,
    /// By hashing (no fs-verity on the file).
    pub by_hash: usize,
}

/// Why a payload is not the signed layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadError(pub String);

impl fmt::Display for PayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PayloadError {}

fn fail<T>(message: impl Into<String>) -> Result<T, PayloadError> {
    Err(PayloadError(message.into()))
}

fn io_fail<T>(path: &str, error: &io::Error) -> Result<T, PayloadError> {
    fail(format!("{path}: {error}"))
}

fn relative_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn sha256_of(file: &mut File) -> io::Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(buffer.get(..read).unwrap_or_default());
    }
    Ok(hex::encode(hasher.finalize()))
}

fn open_nofollow(path: &Path) -> io::Result<File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

struct Walk<'a> {
    root: &'a Path,
    manifest: &'a LayerManifest,
    expected: BTreeMap<&'a str, &'a Entry>,
    seen: BTreeSet<String>,
    owner: u32,
    check: PayloadCheck,
}

impl Walk<'_> {
    fn ownership(&self, path: &str, meta: &fs::Metadata) -> Result<(), PayloadError> {
        if meta.uid() != self.owner {
            return fail(format!(
                "{path} is owned by uid {}, not {}",
                meta.uid(),
                self.owner
            ));
        }
        if !meta.file_type().is_symlink() && meta.mode() & 0o022 != 0 {
            return fail(format!(
                "{path} can be written by others (mode {:o})",
                meta.mode() & 0o7777
            ));
        }
        if meta.file_type().is_file() && meta.mode() & 0o6000 != 0 {
            return fail(format!("{path} is set-user-ID or set-group-ID"));
        }
        Ok(())
    }

    fn release(&self, path: &Path) -> Result<(), PayloadError> {
        let wanted = format!("extension-release.{}", self.manifest.name);
        let entries = fs::read_dir(path).or_else(|error| io_fail(RELEASE_DIR, &error))?;
        let mut found = false;
        for entry in entries {
            let entry = entry.or_else(|error| io_fail(RELEASE_DIR, &error))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let shown = format!("{RELEASE_DIR}/{name}");
            if name != wanted {
                return fail(format!("{shown} is not the layer's own extension-release"));
            }
            let meta =
                fs::symlink_metadata(entry.path()).or_else(|error| io_fail(&shown, &error))?;
            self.ownership(&shown, &meta)?;
            if !meta.file_type().is_file() || meta.len() > 4096 {
                return fail(format!("{shown} is not a small regular file"));
            }
            let text = fs::read(entry.path()).or_else(|error| io_fail(&shown, &error))?;
            if text != self.manifest.extension_release.as_bytes() {
                return fail(format!("{shown} is not the signed extension-release"));
            }
            found = true;
        }
        if !found {
            return fail(format!("{RELEASE_DIR}/{wanted} is missing"));
        }
        Ok(())
    }

    fn file(&mut self, shown: &str, abs: &Path, meta: &fs::Metadata) -> Result<(), PayloadError> {
        let Some(Entry::File {
            sha256,
            verity: signed_verity,
            executable,
            ..
        }) = self.expected.get(shown).copied()
        else {
            return fail(format!(
                "{shown} is a file the manifest does not list as one"
            ));
        };
        if (meta.mode() & 0o111 != 0) != *executable {
            return fail(format!(
                "{shown} has another executable bit than the signed one"
            ));
        }
        let mut file = open_nofollow(abs).or_else(|error| io_fail(shown, &error))?;
        let opened = file.metadata().or_else(|error| io_fail(shown, &error))?;
        if !opened.file_type().is_file() || opened.ino() != meta.ino() || opened.dev() != meta.dev()
        {
            return fail(format!("{shown} changed while it was checked"));
        }
        let measured = verity::measure(&file).map(hex::encode);
        match (signed_verity, measured) {
            (Some(signed), Some(measured)) if *signed == measured => {
                self.check.by_verity = self.check.by_verity.saturating_add(1);
                return Ok(());
            }
            (Some(_), Some(_)) => {
                return fail(format!(
                    "{shown} is sealed with other bytes than the signed ones"
                ));
            }
            _ => {}
        }
        let digest = sha256_of(&mut file).or_else(|error| io_fail(shown, &error))?;
        if digest != *sha256 {
            return fail(format!("{shown} has other bytes than the signed ones"));
        }
        self.check.by_hash = self.check.by_hash.saturating_add(1);
        Ok(())
    }

    fn walk(&mut self, relative: &Path) -> Result<(), PayloadError> {
        let shown = relative_text(relative);
        let abs = self.root.join(relative);
        let meta = fs::symlink_metadata(&abs).or_else(|error| io_fail(&shown, &error))?;
        self.ownership(&shown, &meta)?;
        let kind = meta.file_type();
        if kind.is_dir() {
            if shown == RELEASE_DIR {
                return self.release(&abs);
            }
            if self.expected.contains_key(shown.as_str()) {
                return fail(format!(
                    "{shown} is a directory the manifest lists as a file or link"
                ));
            }
            let mut children: Vec<PathBuf> = fs::read_dir(&abs)
                .or_else(|error| io_fail(&shown, &error))?
                .map(|entry| entry.map(|entry| relative.join(entry.file_name())))
                .collect::<io::Result<_>>()
                .or_else(|error| io_fail(&shown, &error))?;
            children.sort();
            for child in children {
                self.walk(&child)?;
            }
            return Ok(());
        }
        if kind.is_symlink() {
            let target = fs::read_link(&abs).or_else(|error| io_fail(&shown, &error))?;
            match self.expected.get(shown.as_str()) {
                Some(Entry::Link { target: signed, .. })
                    if target.to_string_lossy() == signed.as_str() => {}
                Some(Entry::Link { .. }) => {
                    return fail(format!("{shown} points elsewhere than the signed target"));
                }
                _ => {
                    return fail(format!(
                        "{shown} is a link the manifest does not list as one"
                    ));
                }
            }
        } else if kind.is_file() {
            self.file(&shown, &abs, &meta)?;
        } else {
            return fail(format!("{shown} is not a file, directory or link"));
        }
        self.seen.insert(shown);
        Ok(())
    }
}

/// Check the tree at `root` is exactly what `manifest` lists, owned by
/// `owner`. Top level: `usr/`, `opt/` and tilo-vm's `provenance.json`, which
/// is never merged; nothing else.
///
/// # Errors
/// The first difference: a missing, added or changed file, link or
/// executable bit, a wrong extension-release, a set-id bit, an entry someone
/// else owns or can write, or an unreadable tree.
pub fn verify_payload(
    root: &Path,
    manifest: &LayerManifest,
    owner: u32,
) -> Result<PayloadCheck, PayloadError> {
    let mut walk = Walk {
        root,
        manifest,
        expected: manifest
            .entries
            .iter()
            .map(|entry| (entry.path(), entry))
            .collect(),
        seen: BTreeSet::new(),
        owner,
        check: PayloadCheck::default(),
    };
    let meta = fs::symlink_metadata(root).or_else(|error| io_fail("the layer", &error))?;
    if !meta.file_type().is_dir() {
        return fail("the layer is not a directory");
    }
    walk.ownership("the layer", &meta)?;
    let mut tops: Vec<String> = fs::read_dir(root)
        .or_else(|error| io_fail("the layer", &error))?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<_>>()
        .or_else(|error| io_fail("the layer", &error))?;
    tops.sort();
    for top in tops {
        if TREES.contains(&top.as_str()) {
            walk.walk(Path::new(&top))?;
        } else if top == PROVENANCE {
            let meta =
                fs::symlink_metadata(root.join(&top)).or_else(|error| io_fail(&top, &error))?;
            if !meta.file_type().is_file() {
                return fail(format!("{top} is not a regular file"));
            }
        } else {
            return fail(format!("{top} is outside usr/ and opt/"));
        }
    }
    if let Some(missing) = walk
        .expected
        .keys()
        .find(|path| !walk.seen.contains(**path))
    {
        return fail(format!("{missing} is missing"));
    }
    Ok(walk.check)
}

/// Every path below `root`, relative, not following links.
fn collect(root: &Path, relative: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let child = relative.join(entry.file_name());
        let is_dir = entry.file_type()?.is_dir();
        out.push(child.clone());
        if is_dir {
            collect(root, &child, out)?;
        }
    }
    Ok(())
}

/// A file's SHA-256, and its fs-verity digest when the filesystem sealed it.
type Described = (String, Option<String>);

/// One file's SHA-256 and its fs-verity digest when the filesystem sealed it.
///
/// The kernel seals the file (one pass over it) while this thread reads it
/// once for both its SHA-256 and the fs-verity digest of the bytes read. The
/// sealed file is then measured: equal to the digest of what was read, the
/// SHA-256 is of the sealed bytes. Two passes run side by side instead of one
/// after the other, which halves the time a large binary takes.
fn seal_and_read(path: &Path) -> io::Result<Described> {
    let (sealed, read) = std::thread::scope(|scope| {
        let sealing = scope.spawn(|| verity::enable(path));
        let read = (|| {
            let mut file = open_nofollow(path)?;
            let mut sha256 = Sha256::new();
            let mut tree = verity::VerityHasher::new();
            let mut buffer = vec![0u8; 1 << 20];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                let bytes = buffer.get(..count).unwrap_or_default();
                sha256.update(bytes);
                tree.update(bytes);
            }
            Ok::<_, io::Error>((hex::encode(sha256.finalize()), hex::encode(tree.finalize())))
        })();
        let sealed = sealing
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("sealing the file failed")));
        (sealed, read)
    });
    let (sha256, digest) = read?;
    match sealed {
        Ok(()) => {}
        // No fs-verity on this filesystem: the activation hashes the file.
        Err(error) if error.kind() == io::ErrorKind::Unsupported => return Ok((sha256, None)),
        Err(error) => return Err(error),
    }
    let measured = verity::measure(&open_nofollow(path)?).map(hex::encode);
    match measured {
        Some(measured) if measured == digest => Ok((sha256, Some(measured))),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} changed while it was sealed", path.display()),
        )),
        None => Ok((sha256, None)),
    }
}

/// Seal every regular file of the layer at `root` with fs-verity where the
/// filesystem has it, then describe the layer: its files and links in
/// tilo-vm's layer order (paths sorted component-wise), each file's SHA-256
/// read after sealing and its measured fs-verity digest. `provenance.json`
/// and the extension-release directory are not the layer's content.
///
/// Files are sealed and read in parallel, largest first; each takes one
/// kernel pass and, beside it, one read.
///
/// # Errors
/// Unreadable entries, a special file ([`io::ErrorKind::InvalidData`]), or a
/// file fs-verity refuses for another reason than missing support.
pub fn describe(root: &Path) -> io::Result<Vec<Entry>> {
    let mut paths = Vec::new();
    collect(root, Path::new(""), &mut paths)?;
    paths.sort();
    let mut slots: Vec<(String, PathBuf, Option<Entry>)> = Vec::new();
    let mut files = Vec::new();
    for relative in paths {
        let text = relative_text(&relative);
        if text == PROVENANCE || text == RELEASE_DIR || text.starts_with(&format!("{RELEASE_DIR}/"))
        {
            continue;
        }
        let abs = root.join(&relative);
        let meta = fs::symlink_metadata(&abs)?;
        let kind = meta.file_type();
        if kind.is_dir() {
            continue;
        }
        if kind.is_symlink() {
            let target = fs::read_link(&abs)?.to_string_lossy().into_owned();
            slots.push((text.clone(), abs, Some(Entry::Link { path: text, target })));
        } else if kind.is_file() {
            files.push(slots.len());
            slots.push((text, abs, None));
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{text} is not a file, directory or symlink"),
            ));
        }
    }
    // Largest first: the biggest file decides the time, so it starts at once.
    files.sort_by_key(|slot| {
        std::cmp::Reverse(
            slots
                .get(*slot)
                .and_then(|(_, abs, _)| fs::symlink_metadata(abs).ok())
                .map_or(0, |meta| meta.len()),
        )
    });
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, io::Result<Described>)>> = Mutex::new(Vec::new());
    // Each file takes two threads: the kernel sealing it and the read.
    let workers = (std::thread::available_parallelism().map_or(2, std::num::NonZeroUsize::get) / 2)
        .min(files.len())
        .max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(slot) = files.get(index).copied() else {
                        break;
                    };
                    let Some((_, abs, _)) = slots.get(slot) else {
                        break;
                    };
                    let result = seal_and_read(abs);
                    if let Ok(mut results) = results.lock() {
                        results.push((slot, result));
                    }
                }
            });
        }
    });
    let results = results
        .into_inner()
        .map_err(|_| io::Error::other("a worker describing the layer failed"))?;
    if results.len() != files.len() {
        return Err(io::Error::other("a worker describing the layer failed"));
    }
    for (slot, result) in results {
        let (sha256, verity) = result?;
        let Some((text, abs, entry)) = slots.get_mut(slot) else {
            continue;
        };
        let meta = fs::symlink_metadata(abs)?;
        *entry = Some(Entry::File {
            path: text.clone(),
            sha256,
            verity,
            executable: meta.mode() & 0o111 != 0,
        });
    }
    slots
        .into_iter()
        .map(|(text, _, entry)| {
            entry.ok_or_else(|| io::Error::other(format!("{text} was not described")))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{MANIFEST_TYPE, SCHEMA, content_sha256, tilo_vm_extension_release};
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    fn layer() -> (tempfile::TempDir, LayerManifest, u32) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("usr/bin")).unwrap();
        fs::create_dir_all(root.join("usr/lib/extension-release.d")).unwrap();
        fs::write(root.join("usr/bin/shell"), b"v1").unwrap();
        fs::set_permissions(
            root.join("usr/bin/shell"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(root.join("usr/lib/libx.so.1"), b"lib").unwrap();
        symlink("libx.so.1", root.join("usr/lib/libx.so")).unwrap();
        fs::write(root.join(PROVENANCE), b"{}").unwrap();
        let entries = describe(root).unwrap();
        let content = content_sha256(&entries);
        fs::write(
            root.join(format!(
                "usr/lib/extension-release.d/extension-release.{content}"
            )),
            tilo_vm_extension_release("shell"),
        )
        .unwrap();
        let manifest = LayerManifest {
            kind: MANIFEST_TYPE.into(),
            schema: SCHEMA,
            name: content.clone(),
            component: "shell".into(),
            revision: "local".into(),
            content_sha256: content,
            generation: 1,
            base_image_digest: None,
            extension_release: tilo_vm_extension_release("shell"),
            entries,
        };
        manifest.check().unwrap();
        let owner = fs::metadata(root).unwrap().uid();
        (dir, manifest, owner)
    }

    #[test]
    fn describe_lists_files_and_links_in_layer_order_without_importer_metadata() {
        let (dir, manifest, _) = layer();
        let paths: Vec<&str> = manifest.entries.iter().map(Entry::path).collect();
        assert_eq!(
            paths,
            ["usr/bin/shell", "usr/lib/libx.so", "usr/lib/libx.so.1"]
        );
        let Entry::File {
            sha256, executable, ..
        } = &manifest.entries[0]
        else {
            panic!("a file")
        };
        assert_eq!(*sha256, hex::encode(Sha256::digest(b"v1")));
        assert!(*executable);
        // Describing again (already sealed where the filesystem seals) is the same.
        let again = describe(dir.path()).unwrap();
        assert_eq!(content_sha256(&again), manifest.content_sha256);
    }

    #[test]
    fn the_payload_must_be_exactly_the_signed_tree() {
        let (dir, manifest, owner) = layer();
        let root = dir.path();
        let check = verify_payload(root, &manifest, owner).unwrap();
        assert_eq!(check.by_verity.saturating_add(check.by_hash), 2);
        assert!(
            verify_payload(root, &manifest, owner.wrapping_add(1)).is_err(),
            "another owner"
        );

        // An added file.
        fs::write(root.join("usr/bin/extra"), b"x").unwrap();
        assert!(
            verify_payload(root, &manifest, owner)
                .unwrap_err()
                .0
                .contains("usr/bin/extra")
        );
        fs::remove_file(root.join("usr/bin/extra")).unwrap();
        // A link pointed elsewhere.
        fs::remove_file(root.join("usr/lib/libx.so")).unwrap();
        symlink("/etc/shadow", root.join("usr/lib/libx.so")).unwrap();
        assert!(
            verify_payload(root, &manifest, owner)
                .unwrap_err()
                .0
                .contains("elsewhere")
        );
        fs::remove_file(root.join("usr/lib/libx.so")).unwrap();
        // A missing link.
        assert!(
            verify_payload(root, &manifest, owner)
                .unwrap_err()
                .0
                .contains("missing")
        );
        symlink("libx.so.1", root.join("usr/lib/libx.so")).unwrap();
        // Something outside usr/ and opt/.
        fs::create_dir(root.join("etc")).unwrap();
        assert!(verify_payload(root, &manifest, owner).is_err());
        fs::remove_dir(root.join("etc")).unwrap();
        // A writable directory.
        fs::set_permissions(root.join("usr/lib"), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            verify_payload(root, &manifest, owner)
                .unwrap_err()
                .0
                .contains("written by others")
        );
        fs::set_permissions(root.join("usr/lib"), fs::Permissions::from_mode(0o755)).unwrap();
        // Another extension-release.
        let release = root.join(format!(
            "usr/lib/extension-release.d/extension-release.{}",
            manifest.name
        ));
        fs::write(&release, "ID=fedora\n").unwrap();
        assert!(
            verify_payload(root, &manifest, owner)
                .unwrap_err()
                .0
                .contains("extension-release")
        );
        fs::write(&release, tilo_vm_extension_release("shell")).unwrap();
        verify_payload(root, &manifest, owner).unwrap();
    }

    #[test]
    fn a_changed_file_or_executable_bit_is_refused() {
        let (dir, manifest, owner) = layer();
        let root = dir.path();
        let lib = root.join("usr/lib/libx.so.1");
        fs::set_permissions(&lib, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            verify_payload(root, &manifest, owner)
                .unwrap_err()
                .0
                .contains("executable bit")
        );
        fs::set_permissions(&lib, fs::Permissions::from_mode(0o644)).unwrap();
        // A sealed file cannot be written; replacing it is what tampering looks like.
        fs::remove_file(&lib).unwrap();
        fs::write(&lib, b"evil").unwrap();
        fs::set_permissions(&lib, fs::Permissions::from_mode(0o644)).unwrap();
        let error = verify_payload(root, &manifest, owner).unwrap_err().0;
        assert!(
            error.contains("usr/lib/libx.so.1") && error.contains("other bytes"),
            "{error}"
        );
    }
}
