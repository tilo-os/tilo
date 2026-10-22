//! From a path inside an app's documents to the host file it names, and
//! opening it there without stepping outside what was chosen.
//!
//! Every access starts from the chosen item itself, never from a path
//! string: a file document is opened by name from its folder with
//! `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`, so only that name (or a
//! Save's sibling) can be reached and a link planted in its place is not
//! followed; a folder document resolves the rest of the path beneath the
//! folder with `RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS`, so `..` and
//! symbolic links cannot climb out of it.

use std::ffi::OsStr;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{Mode, OFlags, ResolveFlags};

use crate::{AppKey, Document, Documents, Kind};

/// A path inside one document, checked against what the choice covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub document: Document,
    /// The path below the document's own name: empty for the chosen file
    /// or folder itself, the save sibling's name for a Save's sibling, the
    /// path inside a folder document otherwise.
    pub inner: PathBuf,
}

impl Target {
    /// Whether this is the chosen file or folder itself.
    pub fn is_root(&self) -> bool {
        self.inner.as_os_str().is_empty()
    }

    /// Whether the app may change what this names.
    pub fn writable(&self) -> bool {
        self.document.kind.writes()
    }

    /// The host path this names. For display and tests: open with
    /// [`Self::open`], which resolves beneath the document.
    pub fn host_path(&self) -> PathBuf {
        match self.document.kind {
            Kind::Folder { .. } => self.document.host.join(&self.inner),
            Kind::Read | Kind::Save => match self.document.host.parent() {
                Some(folder) if !self.is_root() => folder.join(&self.inner),
                _ => self.document.host.clone(),
            },
        }
    }

    /// The folder every access to this target starts from, and the path
    /// resolved beneath it.
    fn anchor(&self) -> (PathBuf, PathBuf, ResolveFlags) {
        match self.document.kind {
            Kind::Folder { .. } => {
                let inner = if self.is_root() {
                    PathBuf::from(".")
                } else {
                    self.inner.clone()
                };
                (
                    self.document.host.clone(),
                    inner,
                    ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
                )
            }
            Kind::Read | Kind::Save => {
                let folder = self
                    .document
                    .host
                    .parent()
                    .map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
                let name = if self.is_root() {
                    PathBuf::from(self.document.name())
                } else {
                    self.inner.clone()
                };
                (
                    folder,
                    name,
                    ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
                )
            }
        }
    }

    /// Open what this names on the host with `flags` (and `mode` when
    /// creating), resolved beneath the chosen item.
    ///
    /// # Errors
    ///
    /// The operating system's error; `EXDEV`/`ELOOP` when the path would
    /// leave the chosen item.
    pub fn open(&self, flags: OFlags, mode: Mode) -> io::Result<OwnedFd> {
        let (folder, path, resolve) = self.anchor();
        let anchor = rustix::fs::open(
            &folder,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(rustix::fs::openat2(
            &anchor,
            &path,
            flags | OFlags::CLOEXEC,
            mode,
            resolve,
        )?)
    }

    /// Where this really is on the host, found as an open finds it:
    /// resolved beneath the chosen item, so a link swapped in since it was
    /// chosen cannot point it anywhere else. What a host-side opener (the
    /// app that opens the file's type) is handed.
    ///
    /// # Errors
    ///
    /// As [`Self::open`], and when the system cannot say where the opened
    /// item is.
    pub fn real_path(&self) -> io::Result<PathBuf> {
        use std::os::fd::AsRawFd as _;
        let opened = self.open(OFlags::PATH, Mode::empty())?;
        std::fs::read_link(format!("/proc/self/fd/{}", opened.as_raw_fd()))
    }

    /// The folder this target's entry lives in, opened beneath the chosen
    /// item, and the entry's name in it: what creating, renaming and
    /// removing work on. `None` for the chosen item itself in a folder
    /// document, which has no parent the app was given.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn open_parent(&self) -> io::Result<Option<(OwnedFd, PathBuf)>> {
        let (folder, path, resolve) = self.anchor();
        let anchor = rustix::fs::open(
            &folder,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let Some(name) = path.file_name().map(PathBuf::from) else {
            return Ok(None);
        };
        if path == Path::new(".") {
            return Ok(None);
        }
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
        let dir = rustix::fs::openat2(
            &anchor,
            parent.unwrap_or_else(|| Path::new(".")),
            OFlags::DIRECTORY | OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
            resolve,
        )?;
        Ok(Some((dir, name)))
    }
}

/// What `token`'s document with `inner` below it names, when the choice
/// covers it. A file document covers its own name, and a Save also its
/// save sibling; a folder document covers everything beneath it. `None`
/// for anything else, and for any path with `..`, `.` or a root in it.
pub fn target(documents: &Documents, key: &AppKey, token: &str, inner: &Path) -> Option<Target> {
    let document = documents.document(key, token)?;
    let mut components = inner.components();
    let first = match components.next() {
        None => {
            return Some(Target {
                document,
                inner: PathBuf::new(),
            });
        }
        Some(Component::Normal(first)) => first,
        Some(_) => return None,
    };
    let rest: Vec<&OsStr> = components
        .map(|component| match component {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect::<Option<_>>()?;
    match document.kind {
        Kind::Folder { .. } => {
            if first != document.name() {
                return None;
            }
            let inner: PathBuf = rest.iter().collect();
            Some(Target { document, inner })
        }
        Kind::Read | Kind::Save => {
            if !rest.is_empty() {
                return None;
            }
            if first == document.name() {
                Some(Target {
                    document,
                    inner: PathBuf::new(),
                })
            } else if document.save_sibling().as_deref() == Some(first) {
                Some(Target {
                    document,
                    inner: PathBuf::from(first),
                })
            } else {
                None
            }
        }
    }
}

/// What an absolute path an app was given (`/run/tilo/documents/<token>/…`)
/// names on the host, when a choice still covers it.
pub fn resolve(documents: &Documents, key: &AppKey, sandbox_path: &Path) -> Option<Target> {
    let (token, inner) = crate::split_sandbox_path(sandbox_path)?;
    target(documents, key, &token, &inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn key() -> AppKey {
        AppKey::new("room", "so.tilo.pdf-viewer").unwrap()
    }

    fn read(target: &Target) -> io::Result<Vec<u8>> {
        let fd = target.open(OFlags::RDONLY, Mode::empty())?;
        let mut bytes = Vec::new();
        std::fs::File::from(fd).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    #[test]
    fn a_file_document_covers_its_own_name_and_nothing_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("chosen.pdf"), b"chosen").unwrap();
        std::fs::write(dir.path().join("secret.txt"), b"secret").unwrap();
        let documents = Documents::new();
        let path = documents
            .grant(&key(), &dir.path().join("chosen.pdf"), Kind::Read)
            .unwrap();
        let target = resolve(&documents, &key(), &path).unwrap();
        assert_eq!(read(&target).unwrap(), b"chosen");
        let folder = path.parent().unwrap();
        for beside in [
            "secret.txt",
            ".chosen.pdf.tilo-save",
            "chosen.pdf/x",
            "../x",
        ] {
            assert!(
                resolve(&documents, &key(), &folder.join(beside)).is_none(),
                "{beside}"
            );
        }
        let elsewhere = AppKey::new("room", "so.tilo.files").unwrap();
        assert!(resolve(&documents, &elsewhere, &path).is_none());
    }

    #[test]
    fn a_link_planted_in_place_of_a_chosen_file_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let chosen = dir.path().join("chosen.txt");
        std::fs::write(&chosen, b"chosen").unwrap();
        std::fs::write(dir.path().join("secret.txt"), b"secret").unwrap();
        let documents = Documents::new();
        let path = documents.grant(&key(), &chosen, Kind::Read).unwrap();
        std::fs::remove_file(&chosen).unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret.txt"), &chosen).unwrap();
        let target = resolve(&documents, &key(), &path).unwrap();
        assert!(read(&target).is_err(), "the link is refused, not followed");
    }

    #[test]
    fn a_folder_document_covers_what_is_beneath_it_and_cannot_climb_out() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("backups");
        std::fs::create_dir_all(folder.join("2026")).unwrap();
        std::fs::write(folder.join("2026/a.tar"), b"a").unwrap();
        std::fs::write(dir.path().join("secret.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret.txt"), folder.join("out")).unwrap();
        let documents = Documents::new();
        let path = documents
            .grant(&key(), &folder, Kind::Folder { write: false })
            .unwrap();
        let inside = resolve(&documents, &key(), &path.join("2026/a.tar")).unwrap();
        assert_eq!(read(&inside).unwrap(), b"a");
        assert!(resolve(&documents, &key(), &path.join("../secret.txt")).is_none());
        let link = resolve(&documents, &key(), &path.join("out")).unwrap();
        assert!(
            read(&link).is_err(),
            "a link out of the folder is not followed"
        );
    }

    #[test]
    fn a_save_covers_its_new_file_and_its_save_sibling() {
        let dir = tempfile::tempdir().unwrap();
        let documents = Documents::new();
        let path = documents
            .grant(&key(), &dir.path().join("export.log"), Kind::Save)
            .unwrap();
        let file = resolve(&documents, &key(), &path).unwrap();
        assert!(file.writable());
        let fd = file
            .open(
                OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
                Mode::from_raw_mode(0o644),
            )
            .unwrap();
        drop(fd);
        assert!(dir.path().join("export.log").is_file());
        let sibling = path.with_file_name(".export.log.tilo-save");
        let sibling = resolve(&documents, &key(), &sibling).unwrap();
        assert_eq!(
            sibling.host_path(),
            dir.path()
                .canonicalize()
                .unwrap()
                .join(".export.log.tilo-save")
        );
        assert!(resolve(&documents, &key(), &path.with_file_name("other.log")).is_none());
    }
}
