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
//! the view. Every app runs in a workspace, so a caller the session cannot
//! name one for is refused, legibly: the same for a choice the store cannot
//! take (a folder where the app asked for a file). No caller is answered with
//! a host path: one route gives a chosen file, and it is the reference.

use std::path::{Path, PathBuf};

use tilo_documents::{AppKey, Documents, Kind};

use crate::PortalCaller;

/// What one answer names: the reference the app opens in the store its launcher bound, and the
/// host file it stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    /// What the app opens: its reference in the store it has bound.
    pub app_path: PathBuf,
    /// The file on the host the person chose.
    pub host_path: PathBuf,
}

/// Why a choice is not handed to the app: it runs in no workspace this session can name, or the
/// store cannot take what was chosen. The words are for the person.
pub const NOT_GIVEN: &str = "That file can't be given to this app.";

/// The key `caller`'s documents are kept under: the room its launcher
/// started it in and its package id, which is the key that launcher's view
/// of the store is named by.
///
/// `None` only for a caller whose room the session does not know: it has no
/// view of the store to grant into, so it is refused. Every caller whose
/// workspace is known is answered a reference, sandboxed or not: one path per
/// feature, never a host path some apps may use.
#[must_use]
pub fn key_of(caller: &PortalCaller) -> Option<AppKey> {
    AppKey::new(caller.workspace.as_deref()?, &caller.app_id)
}

/// What each of `chosen` is answered by for `caller`, as `kind`: the reference the app opens in
/// the store its launcher bound.
///
/// # Errors
/// [`NOT_GIVEN`] when the caller runs in no workspace, or the store cannot take what was chosen.
pub fn exports(
    documents: &Documents,
    caller: &PortalCaller,
    chosen: &[PathBuf],
    kind: Kind,
) -> Result<Vec<Export>, &'static str> {
    let key = key_of(caller).ok_or(NOT_GIVEN)?;
    chosen
        .iter()
        .map(|path| export(documents, caller, &key, path, kind))
        .collect()
}

/// [`exports`] for one path.
fn export(
    documents: &Documents,
    caller: &PortalCaller,
    key: &AppKey,
    chosen: &Path,
    kind: Kind,
) -> Result<Export, &'static str> {
    match documents.grant(key, chosen, kind) {
        Ok(app_path) => Ok(Export {
            // What the store keeps is the file as it resolved the choice (a
            // save's folder, an open's links), which is what the path stands
            // for from here on.
            host_path: stored_path_for(documents, key, &app_path)
                .unwrap_or_else(|| chosen.to_path_buf()),
            app_path,
        }),
        Err(error) => {
            tracing::debug!(
                %error,
                app = %caller.app_id,
                path = %chosen.display(),
                "the chosen file is not kept for the app"
            );
            Err(NOT_GIVEN)
        }
    }
}

/// The host file the store keeps at `app_path`, when that is one of `key`'s
/// documents.
fn stored_path_for(documents: &Documents, key: &AppKey, app_path: &Path) -> Option<PathBuf> {
    let (token, _) = tilo_documents::split_sandbox_path(app_path)?;
    Some(documents.document(key, &token)?.host)
}

#[cfg(test)]
mod tests {
    use tilo_documents::SANDBOX_DIR;

    use super::*;

    /// A caller in `workspace`, sandboxed or not.
    fn caller(workspace: Option<&str>, sandboxed: bool) -> PortalCaller {
        PortalCaller {
            sandboxed,
            workspace: workspace.map(str::to_owned),
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

    tilo_test_env::scenario! {
        fn a_chosen_file_is_answered_as_the_path_its_caller_can_open;

        phase a_sandboxed_caller_is_answered_with_a_store_path_holding_the_picked_file;
        {
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
            )
            .expect("a caller in a room is given its choice");
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

        phase a_save_is_answered_with_a_writable_path_carrying_the_typed_name;
        {
            let folder = tempfile::tempdir().unwrap();
            let chosen = folder.path().join("report.final.txt");
            let documents = Documents::new();
            let caller = caller(Some("studio"), true);

            let export = exports(&documents, &caller, &[chosen], Kind::Save)
                .expect("a caller in a room is given its choice")
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

        phase every_caller_in_a_room_is_answered_a_reference_and_one_without_is_refused;
        {
            let folder = tempfile::tempdir().unwrap();
            let chosen = folder.path().join("notes.txt");
            std::fs::write(&chosen, b"x").unwrap();
            // A host app, whose only name is the one the frontend gave: it is in a room, so it
            // is answered what a sandboxed app is — the store's path, never the host's.
            let export = exports(
                &Documents::new(),
                &caller(Some("studio"), false),
                std::slice::from_ref(&chosen),
                Kind::Read,
            )
            .expect("a caller in a room is given its choice")
            .pop()
            .unwrap();
            assert_eq!(
                export.app_path.parent().and_then(|token| token.parent()),
                Some(std::path::Path::new(SANDBOX_DIR))
            );
            assert_eq!(export.host_path, chosen.canonicalize().unwrap());

            // An app in no room this session knows has no view to grant into: it is refused,
            // legibly, never answered with a host path.
            assert_eq!(
                exports(
                    &Documents::new(),
                    &caller(None, true),
                    std::slice::from_ref(&chosen),
                    Kind::Read,
                ),
                Err(NOT_GIVEN)
            );
        }

        phase a_choice_the_store_does_not_take_is_refused;
        {
            // A folder where the app asked for a file: the store keeps files.
            let folder = tempfile::tempdir().unwrap();
            assert_eq!(
                exports(
                    &Documents::new(),
                    &caller(Some("studio"), true),
                    &[folder.path().to_path_buf()],
                    Kind::Read,
                ),
                Err(NOT_GIVEN)
            );
        }
    }
}
