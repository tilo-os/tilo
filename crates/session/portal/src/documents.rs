//! Files an app hands over that the person chose for it earlier.
//!
//! A sandboxed app names a file of its own by the `file://` URI of the
//! export the chooser made for it (`/run/tilo/documents/<token>/<name>`),
//! never by a host path. Whatever the backend reads on an app's behalf must
//! therefore resolve to one of *that* app's documents in the one store the
//! chooser writes ([`crate::doc_store`]): nothing else of the store, and
//! nothing outside what was chosen — no name the app made up, no `..`, and
//! no symbolic link out. The store resolves the link question itself, by
//! opening beneath the chosen item ([`tilo_documents::host`]), so a link
//! planted where a document was cannot point the read anywhere else.
//!
//! A caller the session does not know runs in a sandbox is handed its files
//! as paths on the host, and has no documents of its own here: it reads them
//! itself, so there is nothing of its to resolve.

use std::os::unix::ffi::OsStringExt as _;
use std::path::{Component, PathBuf};

use tilo_documents::{Documents, host};

use crate::PortalCaller;

/// The host file the export `uri` names, when `uri` is one of `caller`'s
/// documents.
///
/// # Errors
/// Why it is no export of this app's: malformed, remote, not one of its
/// documents, gone, or not a file.
pub fn exported_file(
    documents: &Documents,
    caller: &PortalCaller,
    uri: &str,
) -> Result<PathBuf, String> {
    let path = file_path(uri)?;
    let key = crate::doc_store::key_of(caller).ok_or("the app is handed no documents")?;
    let target = host::resolve(documents, &key, &path).ok_or("not a document of this app")?;
    let real = target
        .real_path()
        .map_err(|_| "the exported file is gone")?;
    if !real.is_file() {
        return Err("the exported file is not a file".into());
    }
    Ok(real)
}

/// The path the `file://` URI `uri` names, before anything is resolved on
/// the host: plain text, local, and with no `..` in it.
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

/// Read at most `max` bytes of the export `uri`.
///
/// # Errors
/// It is no export of the caller's ([`exported_file`]), is larger, or could
/// not be read.
pub fn read_exported(
    documents: &Documents,
    caller: &PortalCaller,
    uri: &str,
    max: u64,
) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let path = exported_file(documents, caller, uri)?;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(max.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Err("the file is too large".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tilo_documents::{AppKey, Kind, SANDBOX_DIR};

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

    #[test]
    fn an_export_the_chooser_made_resolves_to_the_file_it_stands_for() {
        let picked = tempfile::tempdir().unwrap();
        let chosen = picked.path().join("my picture.png");
        std::fs::write(&chosen, b"pixels").unwrap();
        let documents = Documents::new();
        let caller = caller();

        let export = doc_store::exports(
            &documents,
            &caller,
            std::slice::from_ref(&chosen),
            Kind::Read,
        )
        .pop()
        .unwrap();
        assert!(export.app_path.starts_with(SANDBOX_DIR));
        assert_eq!(
            exported_file(&documents, &caller, &uri(&export.app_path)),
            Ok(chosen.canonicalize().unwrap()),
            "what the chooser handed out comes back as the chosen file"
        );
        assert_eq!(
            read_exported(&documents, &caller, &uri(&export.app_path), 1024),
            Ok(b"pixels".to_vec())
        );
    }

    #[test]
    fn only_that_apps_documents_resolve() {
        let picked = tempfile::tempdir().unwrap();
        let chosen = picked.path().join("chosen.txt");
        std::fs::write(&chosen, b"chosen").unwrap();
        let secret = picked.path().join("secret.txt");
        std::fs::write(&secret, b"secret").unwrap();
        let documents = Documents::new();
        let caller = caller();
        let export = doc_store::exports(
            &documents,
            &caller,
            std::slice::from_ref(&chosen),
            Kind::Read,
        )
        .pop()
        .unwrap();

        // A host file: no document of the app names it.
        assert!(exported_file(&documents, &caller, &uri(&secret)).is_err());
        // Another app's document, in the same room and in another.
        let token = export.app_path.parent().unwrap();
        for other in [
            AppKey::new("studio", "so.tilo.other").unwrap(),
            AppKey::new("elsewhere", "so.tilo.notes").unwrap(),
        ] {
            let documents = Documents::new();
            let theirs = documents
                .grant(&other, &secret, Kind::Read)
                .expect("the other app's document");
            assert!(
                exported_file(&documents, &caller, &uri(&theirs)).is_err(),
                "{other:?}'s export is not this app's"
            );
        }
        // A name beside the document, in its own token directory.
        assert!(
            exported_file(&documents, &caller, &uri(&token.join("secret.txt"))).is_err(),
            "a name the app made up beside its document"
        );
        // The document itself still resolves.
        assert!(exported_file(&documents, &caller, &uri(&export.app_path)).is_ok());
    }

    #[test]
    fn a_link_out_of_the_store_is_not_followed() {
        let picked = tempfile::tempdir().unwrap();
        let chosen = picked.path().join("chosen.txt");
        std::fs::write(&chosen, b"chosen").unwrap();
        let secret = picked.path().join("secret.txt");
        std::fs::write(&secret, b"secret").unwrap();
        let documents = Documents::new();
        let caller = caller();
        let export = doc_store::exports(
            &documents,
            &caller,
            std::slice::from_ref(&chosen),
            Kind::Read,
        )
        .pop()
        .unwrap();

        // What a sandbox could plant in its own view of the store: a link
        // where its document is, pointing out. The store opens the chosen
        // name beneath the chosen item, never following it.
        std::fs::remove_file(&chosen).unwrap();
        std::os::unix::fs::symlink(&secret, &chosen).unwrap();
        assert!(
            exported_file(&documents, &caller, &uri(&export.app_path)).is_err(),
            "a link swapped in where the document was"
        );

        // A link planted as a document of its own never was one.
        let planted = Path::new(SANDBOX_DIR).join("0000").join("secret.txt");
        assert!(exported_file(&documents, &caller, &uri(&planted)).is_err());
    }

    #[test]
    fn a_caller_without_documents_has_nothing_to_resolve() {
        let documents = Documents::new();
        let host = PortalCaller::new("org.example");
        assert!(
            exported_file(&documents, &host, "file:///home/someone/notes.txt").is_err(),
            "a host app reads its own files itself"
        );
        // The same export, for a caller the session knows runs sandboxed.
        assert!(
            exported_file(
                &documents,
                &caller(),
                "file:///run/tilo/documents/ffff/notes.txt"
            )
            .is_err()
        );
    }

    #[test]
    fn malformed_uris_are_refused_before_any_resolution() {
        let documents = Documents::new();
        let caller = caller();
        let token = format!("{SANDBOX_DIR}/ffff/notes.txt");
        for text in [
            "https://example.org/x".to_owned(),
            "file://example.org/doc/x".to_owned(),
            format!("file://{token}\u{0}"),
            format!("file://{SANDBOX_DIR}/ffff/../../etc/passwd"),
            format!(" file://{token}"),
        ] {
            assert!(
                exported_file(&documents, &caller, &text).is_err(),
                "{text:?} is no document URI"
            );
        }
    }
}
