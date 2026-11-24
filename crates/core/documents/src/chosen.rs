//! A chosen document, as the chooser hands it to an app.
//!
//! The chooser runs outside the app's sandbox and opens the file with the person's rights, so it
//! needs more than the reference: the host file behind it, what the choice allows, and where a
//! save writes before its bytes replace the file. A host answers those from this module, so the
//! chooser reaches documents through the host that serves it and knows no store of its own.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::{AppKey, Document, Documents, GrantError, Kind, SANDBOX_DIR, split_sandbox_path};

/// One document of an app, as the chooser opens and names it.
#[derive(Clone, Debug)]
pub struct Chosen {
    /// The path the app reaches it by inside its sandbox.
    pub reference: PathBuf,
    /// Its name there.
    pub name: String,
    /// The host file or folder it is.
    pub host: PathBuf,
    /// What the person's choice allows.
    pub kind: Kind,
    /// Where a save writes before its bytes replace the file; `None` for anything but a save.
    pub save_sibling: Option<OsString>,
}

impl Chosen {
    fn of(reference: PathBuf, document: Document) -> Self {
        Self {
            reference,
            name: document.name().to_string_lossy().into_owned(),
            save_sibling: document.save_sibling(),
            host: document.host,
            kind: document.kind,
        }
    }
}

/// Keep what the person chose for `key` as `kind`, answering the document that now holds it.
///
/// # Errors
///
/// As [`Documents::grant`] answers: what was chosen is not what `kind` needs, or it cannot be
/// resolved on the host.
pub fn granted(
    documents: &Documents,
    key: &AppKey,
    path: &Path,
    kind: Kind,
) -> Result<Chosen, GrantError> {
    let reference = documents.grant(key, path, kind)?;
    let (token, _) = split_sandbox_path(&reference).ok_or(GrantError::NotThatKind)?;
    let document = documents
        .document(key, &token)
        .ok_or(GrantError::NotThatKind)?;
    Ok(Chosen::of(reference, document))
}

/// The document `reference` names for `key`, if it is one the person gave that app.
///
/// The answer names the document's own view path whatever the reference asked for: a caller
/// checking what `reference` may reach strips it from this path, and so refuses anything that
/// is not a plain name beneath a chosen folder.
#[must_use]
pub fn looked_up(documents: &Documents, key: &AppKey, reference: &Path) -> Option<Chosen> {
    let (token, _) = split_sandbox_path(reference)?;
    let document = documents.document(key, &token)?;
    let path = Path::new(SANDBOX_DIR).join(&token).join(document.name());
    Some(Chosen::of(path, document))
}
