//! What a chosen file is answered by, so the app that chose it can open it.
//!
//! A sandboxed app's mount namespace holds nothing of the host but what its launcher bound into
//! it, so the path the person picked is a path the app's own kernel cannot resolve: answering with
//! it would hand the app a URI it cannot open. Binding the picked path into the running app's
//! namespace is no way out either. Its namespace was made when it started, a binding made
//! afterwards is invisible to a process that already runs, and one made before it starts cannot be
//! taken back when the app stops being given the file — so a live re-bind answers with what the app
//! can reach now, not with what the person has given it, and it cannot revoke. What the app is
//! handed is therefore the choice's own reference ([`tilo_dialogs::chosen`]), opened through the
//! descriptor the chooser gave; the reference names the choice again later, and resolving it
//! reaches nothing else.
//!
//! The table is keyed by the room the app was launched into and the package it is ([`key_of`]), so
//! no two apps share a choice. Every app runs in a workspace, so a caller the session cannot name
//! one for is refused, legibly: the same for a choice the table cannot take (a folder where the app
//! asked for a file). No caller is answered with a host path: one route gives a chosen file, and it
//! is the reference.

use std::path::{Path, PathBuf};

use tilo_dialogs::chosen::{AppKey, ChosenTable, Kind};

use crate::PortalCaller;

/// What one answer names: the reference the app opens, and the host file it stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    /// What the app opens: the choice's reference.
    pub app_path: PathBuf,
    /// The file on the host the person chose.
    pub host_path: PathBuf,
}

/// Why a choice is not handed to the app: it runs in no workspace this session can name, or the
/// table cannot take what was chosen. The words are for the person.
pub const NOT_GIVEN: &str = "That file can't be given to this app.";

/// The key `caller`'s choices are kept under: the room its launcher started it in and its package
/// id.
///
/// `None` only for a caller whose room the session does not know: it has no choices to grant into,
/// so it is refused. Every caller whose workspace is known is answered a reference, sandboxed or
/// not: one path per feature, never a host path some apps may use.
#[must_use]
pub fn key_of(caller: &PortalCaller) -> Option<AppKey> {
    AppKey::new(caller.workspace.as_deref()?, &caller.app_id)
}

/// What each of `chosen` is answered by for `caller`, as `kind`: the reference the app opens.
///
/// # Errors
/// [`NOT_GIVEN`] when the caller runs in no workspace, or the table cannot take what was chosen.
pub fn exports(
    table: &ChosenTable,
    caller: &PortalCaller,
    chosen: &[PathBuf],
    kind: Kind,
) -> Result<Vec<Export>, &'static str> {
    let key = key_of(caller).ok_or(NOT_GIVEN)?;
    chosen
        .iter()
        .map(|path| export(table, caller, &key, path, kind))
        .collect()
}

/// [`exports`] for one path.
fn export(
    table: &ChosenTable,
    caller: &PortalCaller,
    key: &AppKey,
    chosen: &Path,
    kind: Kind,
) -> Result<Export, &'static str> {
    match table.grant(key, chosen, kind) {
        Ok(granted) => Ok(Export {
            host_path: granted.host,
            app_path: granted.reference,
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use tilo_dialogs::chosen::CHOSEN_ROOT;

    use super::*;

    /// A caller in `workspace`, sandboxed or not.
    fn caller(workspace: Option<&str>, sandboxed: bool) -> PortalCaller {
        PortalCaller {
            sandboxed,
            workspace: workspace.map(str::to_owned),
            ..PortalCaller::new("so.tilo.notes")
        }
    }

    /// The bytes `reference` stands for, as the table resolves it.
    fn read(table: &ChosenTable, key: &AppKey, reference: &Path) -> Vec<u8> {
        let target = crate::documents::resolve(table, key, reference)
            .expect("the export names a choice of the app");
        std::fs::read(target.host_path()).expect("the document reads")
    }

    tilo_test_env::scenario! {
        fn a_chosen_file_is_answered_as_the_path_its_caller_can_open;

        phase a_sandboxed_caller_is_answered_with_a_reference_holding_the_picked_file;
        {
            let picked = tempfile::tempdir().unwrap();
            let chosen = picked.path().join("notes.txt");
            std::fs::write(&chosen, b"the file the person picked").unwrap();
            let table = ChosenTable::new();
            let caller = caller(Some("studio"), true);

            let exported = exports(
                &table,
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
                "the export names the file the table keeps the choice as"
            );
            assert!(
                export.app_path.starts_with(CHOSEN_ROOT),
                "answered with a reference, not {}",
                export.app_path.display()
            );
            assert_eq!(
                export.app_path.file_name(),
                Some(chosen.file_name().unwrap())
            );

            let key = key_of(&caller).expect("a sandboxed caller in a room has a key");
            assert_eq!(
                read(&table, &key, &export.app_path),
                b"the file the person picked"
            );
            let (token, inner) =
                tilo_dialogs::chosen::split_reference(&export.app_path).expect("a token path");
            assert_eq!(inner, std::path::Path::new("notes.txt"));
            let document = table.document(&key, &token).expect("the document");
            assert_eq!(document.host, chosen.canonicalize().unwrap());
        }

        phase a_save_is_answered_with_a_writable_reference_carrying_the_typed_name;
        {
            let folder = tempfile::tempdir().unwrap();
            let chosen = folder.path().join("report.final.txt");
            let table = ChosenTable::new();
            let caller = caller(Some("studio"), true);

            let export = exports(&table, &caller, &[chosen], Kind::Save)
                .expect("a caller in a room is given its choice")
                .pop()
                .unwrap();
            assert!(export.app_path.starts_with(CHOSEN_ROOT));
            assert_eq!(
                export.host_path,
                folder
                    .path()
                    .canonicalize()
                    .unwrap()
                    .join("report.final.txt"),
                "the table keeps the file the save will make"
            );
            assert_eq!(
                export.app_path.file_name(),
                Some("report.final.txt".as_ref()),
                "the table keeps the name the person typed, extension and all"
            );
            let key = key_of(&caller).unwrap();
            let (token, _) = tilo_dialogs::chosen::split_reference(&export.app_path).unwrap();
            assert_eq!(
                table
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
            // A host app, whose only name is the one the frontend gave: it is in a room, so it is
            // answered what a sandboxed app is — a reference, never the host's path.
            let export = exports(
                &ChosenTable::new(),
                &caller(Some("studio"), false),
                std::slice::from_ref(&chosen),
                Kind::Read,
            )
            .expect("a caller in a room is given its choice")
            .pop()
            .unwrap();
            let token_dir = export.app_path.parent().unwrap();
            assert_eq!(
                token_dir.parent(),
                Some(Path::new(CHOSEN_ROOT).join("records").as_path())
            );
            assert_eq!(export.host_path, chosen.canonicalize().unwrap());

            // An app in no room this session knows has no choice to grant into: it is refused,
            // legibly, never answered with a host path.
            assert_eq!(
                exports(
                    &ChosenTable::new(),
                    &caller(None, true),
                    std::slice::from_ref(&chosen),
                    Kind::Read,
                ),
                Err(NOT_GIVEN)
            );
        }

        phase a_choice_the_table_does_not_take_is_refused;
        {
            // A folder where the app asked for a file: the table keeps files.
            let folder = tempfile::tempdir().unwrap();
            assert_eq!(
                exports(
                    &ChosenTable::new(),
                    &caller(Some("studio"), true),
                    &[folder.path().to_path_buf()],
                    Kind::Read,
                ),
                Err(NOT_GIVEN)
            );
        }
    }
}
