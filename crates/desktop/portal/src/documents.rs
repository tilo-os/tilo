//! Files an app hands over that the person chose for it earlier.
//!
//! A sandboxed app names a file of its own by the `file://` URI of the reference the chooser gave
//! it (`/run/tilo/chosen/records/<token>/<name>`), never by a host path. Whatever the backend
//! reads on an app's behalf must therefore resolve to one of *that* app's choices in the session's
//! one table ([`tilo_dialogs::chosen`]): nothing else of the table, and nothing outside what was
//! chosen — no name the app made up, no `..`, and no symbolic link out. The table resolves the
//! link question itself, by opening beneath the chosen item ([`Target`]), so a link planted where
//! a document was cannot point the read anywhere else.
//!
//! A caller the session does not know runs in a sandbox is handed its files as paths on the host,
//! and has no documents of its own here: it reads them itself, so there is nothing of its to
//! resolve.

use std::ffi::OsStr;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{Mode, OFlags, ResolveFlags};
use tilo_dialogs::chosen::{AppKey, ChosenTable, Document, Kind, split_reference};

use crate::PortalCaller;

/// The host file the reference `uri` names, when `uri` is one of `caller`'s choices.
///
/// # Errors
/// Why it is no reference of this app's: malformed, remote, not one of its choices, gone, or not
/// a file.
pub fn exported_file(
    chosen: &ChosenTable,
    caller: &PortalCaller,
    uri: &str,
) -> Result<PathBuf, String> {
    let path = file_path(uri)?;
    let key = crate::doc_store::key_of(caller).ok_or("the app is handed no documents")?;
    let target = resolve(chosen, &key, &path).ok_or("not a document of this app")?;
    let real = target
        .real_path()
        .map_err(|_| "the exported file is gone")?;
    if !real.is_file() {
        return Err("the exported file is not a file".into());
    }
    Ok(real)
}

/// The path the `file://` URI `uri` names, before anything is resolved on the host: plain text,
/// local, and with no `..` in it.
fn file_path(uri: &str) -> Result<PathBuf, String> {
    if !crate::plain_text(uri, 8192) || uri.trim() != uri {
        return Err("malformed document URI".into());
    }
    let rest = uri.strip_prefix("file://").ok_or("not a file URI")?;
    let encoded = if rest.starts_with('/') {
        rest
    } else {
        rest.strip_prefix("localhost")
            .filter(|path| path.starts_with('/'))
            .ok_or("the file URI names another host")?
    };
    let bytes: Vec<u8> = percent_encoding::percent_decode_str(encoded).collect();
    if bytes.contains(&0) {
        return Err("the file URI holds a NUL".into());
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    if path.components().any(|part| part == Component::ParentDir) {
        return Err("the file URI climbs out of its directory".into());
    }
    Ok(path)
}

/// Read at most `max` bytes of the reference `uri`.
///
/// # Errors
/// It is no reference of the caller's ([`exported_file`]), is larger, or could not be read.
pub fn read_exported(
    chosen: &ChosenTable,
    caller: &PortalCaller,
    uri: &str,
    max: u64,
) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let path = exported_file(chosen, caller, uri)?;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(max.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Err("the file is too large".into());
    }
    Ok(bytes)
}

/// What a path inside a chosen item names, once the table says the choice covers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub document: Document,
    /// The path below the chosen item's own name: empty for the chosen file or folder itself, the
    /// save sibling's name for a Save's sibling, the path inside a folder choice otherwise.
    pub inner: PathBuf,
}

impl Target {
    #[must_use]
    pub fn is_root(&self) -> bool {
        self.inner.as_os_str().is_empty()
    }

    #[must_use]
    pub fn writable(&self) -> bool {
        self.document.kind.writes()
    }

    /// The host path this names, for display and tests: open with [`Self::open`], which resolves
    /// beneath the chosen item.
    #[must_use]
    pub fn host_path(&self) -> PathBuf {
        match self.document.kind {
            Kind::Folder { .. } => self.document.host.join(&self.inner),
            Kind::Read | Kind::Save => match self.document.host.parent() {
                Some(folder) if !self.is_root() => folder.join(&self.inner),
                _ => self.document.host.clone(),
            },
        }
    }

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

    /// Open what this names on the host with `flags` (and `mode` when creating), resolved beneath
    /// the chosen item.
    ///
    /// # Errors
    ///
    /// The operating system's error; `EXDEV`/`ELOOP` when the path would leave the chosen item.
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

    /// Where this really is on the host, found as an open finds it: resolved beneath the chosen
    /// item, so a link swapped in since cannot point it elsewhere. What a host-side opener (the
    /// app that opens the file's type) is handed.
    ///
    /// # Errors
    ///
    /// As [`Self::open`], and when the system cannot say where the opened item is.
    pub fn real_path(&self) -> io::Result<PathBuf> {
        use std::os::fd::AsRawFd as _;
        let opened = self.open(OFlags::PATH, Mode::empty())?;
        std::fs::read_link(format!("/proc/self/fd/{}", opened.as_raw_fd()))
    }
}

/// What `token`'s choice with `inner` below it names, when the choice covers it: a file choice
/// covers its own name, and a Save also its save sibling; a folder choice covers everything
/// beneath it. `None` for anything else, and for any path with `..`, `.` or a root in it.
#[must_use]
pub fn target(chosen: &ChosenTable, key: &AppKey, token: &str, inner: &Path) -> Option<Target> {
    let document = chosen.document(key, token)?;
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

/// What an absolute path an app was given (`/run/tilo/chosen/records/<token>/…`) names on the
/// host, when a choice still covers it.
#[must_use]
pub fn resolve(chosen: &ChosenTable, key: &AppKey, reference: &Path) -> Option<Target> {
    let (token, inner) = split_reference(reference)?;
    target(chosen, key, &token, &inner)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tilo_dialogs::chosen::{AppKey, CHOSEN_ROOT, Kind};

    use super::*;
    use crate::doc_store;

    /// A sandboxed caller in `studio`, and its key.
    fn caller() -> PortalCaller {
        PortalCaller {
            sandboxed: true,
            workspace: Some("studio".into()),
            ..PortalCaller::new("so.tilo.notes")
        }
    }

    fn uri(path: &Path) -> String {
        format!("file://{}", path.display()).replace(' ', "%20")
    }

    tilo_test_env::scenario! {
        fn a_document_uri_resolves_only_for_its_app_and_only_inside_the_table;

        phase an_export_the_chooser_made_resolves_to_the_file_it_stands_for;
        {
            let picked = tempfile::tempdir().unwrap();
            let chosen_file = picked.path().join("my picture.png");
            std::fs::write(&chosen_file, b"pixels").unwrap();
            let chosen = ChosenTable::new();
            let caller = caller();

            let export = doc_store::exports(
                &chosen,
                &caller,
                std::slice::from_ref(&chosen_file),
                Kind::Read,
            )
            .expect("the person's choice is given")
            .pop()
            .unwrap();
            assert!(export.app_path.starts_with(CHOSEN_ROOT));
            assert_eq!(
                exported_file(&chosen, &caller, &uri(&export.app_path)),
                Ok(chosen_file.canonicalize().unwrap()),
                "what the chooser handed out comes back as the chosen file"
            );
            assert_eq!(
                read_exported(&chosen, &caller, &uri(&export.app_path), 1024),
                Ok(b"pixels".to_vec())
            );
        }

        phase only_that_apps_documents_resolve;
        {
            let picked = tempfile::tempdir().unwrap();
            let chosen_file = picked.path().join("chosen.txt");
            std::fs::write(&chosen_file, b"chosen").unwrap();
            let secret = picked.path().join("secret.txt");
            std::fs::write(&secret, b"secret").unwrap();
            let chosen = ChosenTable::new();
            let caller = caller();
            let export = doc_store::exports(
                &chosen,
                &caller,
                std::slice::from_ref(&chosen_file),
                Kind::Read,
            )
            .expect("the person's choice is given")
            .pop()
            .unwrap();

            // A host file: no reference of the app names it.
            assert!(exported_file(&chosen, &caller, &uri(&secret)).is_err());
            // Another app's choice, in the same room and in another.
            let token = export.app_path.parent().unwrap();
            for other in [
                AppKey::new("studio", "so.tilo.other").unwrap(),
                AppKey::new("elsewhere", "so.tilo.notes").unwrap(),
            ] {
                let theirs = chosen
                    .grant(&other, &secret, Kind::Read)
                    .expect("the other app's choice")
                    .reference;
                assert!(
                    exported_file(&chosen, &caller, &uri(&theirs)).is_err(),
                    "{other:?}'s export is not this app's"
                );
            }
            // A name beside the choice, in its own token directory.
            assert!(
                exported_file(&chosen, &caller, &uri(&token.join("secret.txt"))).is_err(),
                "a name the app made up beside its document"
            );
            // The choice itself still resolves.
            assert!(exported_file(&chosen, &caller, &uri(&export.app_path)).is_ok());
        }

        phase a_link_out_of_the_table_is_not_followed;
        {
            let picked = tempfile::tempdir().unwrap();
            let chosen_file = picked.path().join("chosen.txt");
            std::fs::write(&chosen_file, b"chosen").unwrap();
            let secret = picked.path().join("secret.txt");
            std::fs::write(&secret, b"secret").unwrap();
            let chosen = ChosenTable::new();
            let caller = caller();
            let export = doc_store::exports(
                &chosen,
                &caller,
                std::slice::from_ref(&chosen_file),
                Kind::Read,
            )
            .expect("the person's choice is given")
            .pop()
            .unwrap();

            // A link where its document is, pointing out. The table opens the chosen name
            // beneath the chosen item, never following it.
            std::fs::remove_file(&chosen_file).unwrap();
            std::os::unix::fs::symlink(&secret, &chosen_file).unwrap();
            assert!(
                exported_file(&chosen, &caller, &uri(&export.app_path)).is_err(),
                "a link swapped in where the document was"
            );

            // A reference the app made up never was a choice.
            let planted = Path::new(CHOSEN_ROOT)
                .join("records")
                .join("0000")
                .join("secret.txt");
            assert!(exported_file(&chosen, &caller, &uri(&planted)).is_err());
        }

        phase a_caller_without_documents_has_nothing_to_resolve;
        {
            let chosen = ChosenTable::new();
            let host = PortalCaller::new("org.example");
            assert!(
                exported_file(&chosen, &host, "file:///home/someone/notes.txt").is_err(),
                "a host app reads its own files itself"
            );
            // The same reference, for a caller the session knows runs sandboxed.
            assert!(
                exported_file(
                    &chosen,
                    &caller(),
                    &format!("file://{CHOSEN_ROOT}/records/ffff/notes.txt")
                )
                .is_err()
            );
        }

        phase malformed_uris_are_refused_before_any_resolution;
        {
            let chosen = ChosenTable::new();
            let caller = caller();
            let token = format!("{CHOSEN_ROOT}/records/ffff/notes.txt");
            for text in [
                "https://example.org/x".to_owned(),
                "file://example.org/doc/x".to_owned(),
                format!("file://{token}\u{0}"),
                format!("file://{CHOSEN_ROOT}/records/ffff/../../etc/passwd"),
                format!(" file://{token}"),
            ] {
                assert!(
                    exported_file(&chosen, &caller, &text).is_err(),
                    "{text:?} is no document URI"
                );
            }
        }
    }
}
