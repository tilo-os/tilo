//! What a chosen file is answered by, so the app that chose it can open it.
//!
//! A sandboxed app's mount namespace holds nothing of the host but what its
//! launcher bound into it, so the path the person picked is a path the app's
//! own kernel cannot resolve: answering with it would hand the app a URI it
//! cannot open. Binding the picked path into the running app's namespace is
//! no way out either. Its namespace was made when it started, a binding made
//! afterwards is invisible to a process that already runs, and one made
//! before it starts cannot be taken back when the app stops being given the
//! file — so a live re-bind answers with what the app can reach now, not
//! with what the person has given it, and it cannot revoke. What the app is
//! handed is therefore a path in a store instead of a binding: one store
//! ([`tilo_documents`]), which every launcher binds whole at
//! [`tilo_documents::SANDBOX_DIR`] when it starts an app, and which holds
//! each app's own view of the files it was given. A choice adds the file to
//! that view; the app opens it there.
//!
//! The store is keyed by the room the app was launched into and the package
//! it is ([`key_of`]), which is the key its launcher computed when it bound
//! the view; a caller the session does not know runs in a sandbox has no key
//! and is answered with the path itself, as is any caller the store cannot
//! take the file for (a folder where the app asked for files). Host apps
//! share the session's namespace and need none of this: they are handed the
//! path, which is where the file is.

use std::path::{Path, PathBuf};

use tilo_documents::{AppKey, Documents, Kind};

use crate::PortalCaller;

/// What one answer names: the path the app opens, and the host file it
/// stands for. The same path for a caller that shares the session's
/// namespace, which needs no export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    /// What the app opens: the exported path in the store it has bound, or
    /// the host path itself.
    pub app_path: PathBuf,
    /// The file on the host the person chose.
    pub host_path: PathBuf,
}

impl Export {
    /// The answer for a caller answered with the path itself.
    fn direct(path: &Path) -> Self {
        Self {
            app_path: path.to_path_buf(),
            host_path: path.to_path_buf(),
        }
    }
}

/// The key `caller`'s documents are kept under: the room its launcher
/// started it in and its package id, which is the key that launcher's view
/// of the store is named by.
///
/// `None` for a caller the session does not know runs in a launcher sandbox,
/// and for one whose room it does not know: both run against the host's
/// files, so both are answered with a path on it.
#[must_use]
pub fn key_of(caller: &PortalCaller) -> Option<AppKey> {
    if !caller.sandboxed {
        return None;
    }
    AppKey::new(caller.workspace.as_deref()?, &caller.app_id)
}

/// What each of `chosen` is answered by for `caller`, as `kind`: the store
/// path the app opens, or the path itself.
///
/// A file the store cannot take for that kind of choice (a folder where the
/// app asked for a file, a file it may not write) is answered with its host
/// path, as every choice was before: the app is no worse off than with no
/// export at all, and the person's answer stands.
#[must_use]
pub fn exports(
    documents: &Documents,
    caller: &PortalCaller,
    chosen: &[PathBuf],
    kind: Kind,
) -> Vec<Export> {
    let key = key_of(caller);
    chosen
        .iter()
        .map(|path| export(documents, caller, key.as_ref(), path, kind))
        .collect()
}

/// [`exports`] for one path.
fn export(
    documents: &Documents,
    caller: &PortalCaller,
    key: Option<&AppKey>,
    chosen: &Path,
    kind: Kind,
) -> Export {
    let Some(key) = key else {
        return Export::direct(chosen);
    };
    match documents.grant(key, chosen, kind) {
        Ok(app_path) => Export {
            // What the store keeps is the file as it resolved the choice (a
            // save's folder, an open's links), which is what the path stands
            // for from here on.
            host_path: standing_for(documents, key, &app_path)
                .unwrap_or_else(|| chosen.to_path_buf()),
            app_path,
        },
        Err(error) => {
            tracing::debug!(
                %error,
                app = %caller.app_id,
                path = %chosen.display(),
                "the chosen file is not kept for the app"
            );
            Export::direct(chosen)
        }
    }
}

/// The host file the store keeps at `app_path`, when that is one of `key`'s
/// documents.
fn standing_for(documents: &Documents, key: &AppKey, app_path: &Path) -> Option<PathBuf> {
    let (token, _) = tilo_documents::split_sandbox_path(app_path)?;
    Some(documents.document(key, &token)?.host)
}

#[cfg(test)]
mod tests {
    use tilo_documents::SANDBOX_DIR;

    use super::*;

    /// A caller in `room`, sandboxed or not.
    fn caller(room: Option<&str>, sandboxed: bool) -> PortalCaller {
        PortalCaller {
            sandboxed,
            workspace: room.map(str::to_owned),
            ..PortalCaller::new("so.tilo.notes")
        }
    }

    /// The bytes `sandbox_path` stands for, as the store resolves it: the
    /// path must name one of `key`'s documents.
    fn read(documents: &Documents, key: &AppKey, sandbox_path: &Path) -> Vec<u8> {
        let target = tilo_documents::host::resolve(documents, key, sandbox_path)
            .expect("the export names a document of the app");
        std::fs::read(target.host_path()).expect("the document reads")
    }

    #[test]
    fn a_sandboxed_caller_is_answered_with_a_store_path_holding_the_picked_file() {
        let picked = tempfile::tempdir().unwrap();
        let chosen = picked.path().join("notes.txt");
        std::fs::write(&chosen, b"the file the person picked").unwrap();
        let documents = Documents::new();
        let caller = caller(Some("studio"), true);

        let exported = exports(
            &documents,
            &caller,
            std::slice::from_ref(&chosen),
            Kind::Read,
        );
        assert_eq!(exported.len(), 1);
        let export = &exported[0];
        assert_eq!(
            export.host_path,
            chosen.canonicalize().unwrap(),
            "the export names the file the store keeps the choice as"
        );
        assert!(
            export.app_path.starts_with(SANDBOX_DIR),
            "answered with a path inside the store, not {}",
            export.app_path.display()
        );
        assert_eq!(
            export.app_path.file_name(),
            Some(chosen.file_name().unwrap())
        );

        // The token path is the app's view of exactly this file: read
        // through the store it is the bytes the person picked.
        let key = key_of(&caller).expect("a sandboxed caller in a room has a key");
        assert_eq!(
            read(&documents, &key, &export.app_path),
            b"the file the person picked"
        );
        let (token, inner) =
            tilo_documents::split_sandbox_path(&export.app_path).expect("a token path");
        assert_eq!(inner, std::path::Path::new("notes.txt"));
        let document = documents.document(&key, &token).expect("the document");
        assert_eq!(document.host, chosen.canonicalize().unwrap());
    }

    #[test]
    fn a_save_is_answered_with_a_writable_path_carrying_the_typed_name() {
        let folder = tempfile::tempdir().unwrap();
        let chosen = folder.path().join("report.final.txt");
        let documents = Documents::new();
        let caller = caller(Some("studio"), true);

        let export = exports(&documents, &caller, &[chosen], Kind::Save)
            .pop()
            .unwrap();
        assert!(export.app_path.starts_with(SANDBOX_DIR));
        assert_eq!(
            export.host_path,
            folder
                .path()
                .canonicalize()
                .unwrap()
                .join("report.final.txt"),
            "the store keeps the file the save will make"
        );
        assert_eq!(
            export.app_path.file_name(),
            Some("report.final.txt".as_ref()),
            "the store keeps the name the person typed, extension and all"
        );
        let key = key_of(&caller).unwrap();
        let (token, _) = tilo_documents::split_sandbox_path(&export.app_path).unwrap();
        assert_eq!(
            documents
                .document(&key, &token)
                .map(|document| document.kind),
            Some(Kind::Save)
        );
    }

    #[test]
    fn a_host_caller_is_answered_with_the_picked_path_itself() {
        let folder = tempfile::tempdir().unwrap();
        let chosen = folder.path().join("notes.txt");
        std::fs::write(&chosen, b"x").unwrap();
        for caller in [
            // A host app, whose only name is the one the frontend gave.
            caller(Some("studio"), false),
            // An app in no room this session knows.
            caller(None, true),
        ] {
            let export = exports(
                &Documents::new(),
                &caller,
                std::slice::from_ref(&chosen),
                Kind::Read,
            )
            .pop()
            .unwrap();
            assert_eq!(export.app_path, chosen);
            assert_eq!(export.host_path, chosen);
        }
    }

    #[test]
    fn a_choice_the_store_does_not_take_is_still_answered() {
        // A folder where the app asked for a file: the store keeps files.
        let folder = tempfile::tempdir().unwrap();
        let export = exports(
            &Documents::new(),
            &caller(Some("studio"), true),
            &[folder.path().to_path_buf()],
            Kind::Read,
        )
        .pop()
        .unwrap();
        assert_eq!(export.app_path, folder.path());
    }
}
