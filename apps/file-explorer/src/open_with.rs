//! "Open with" — which applications can open a file, and opening it with one.
//!
//! Files decides none of this. The session owns the answer: which Tilo
//! packages and host applications claim a type, and which one opens it today
//! ([`runtime_portal::tools`], the same door every app opens a resource
//! through). Files only lists what it is told and asks for what the person
//! picked.

use runtime_portal::tools::{self, DocumentApp};
use std::path::Path;

/// Asks the session to open a file in an app: `None` opens it in the app this
/// device opens its type with. [`request`] in an app, a recorder in tests.
pub(crate) type OpenRequest = fn(&Path, Option<&str>) -> Result<(), String>;

/// Lists the apps that open a file's type. [`candidates`] in an app, a
/// recorder in tests.
pub(crate) type CandidateList = fn(&Path) -> Result<Vec<DocumentApp>, String>;

/// The answers to opens asked off the window's thread, each with the path it
/// was asked for, shared with the thread that asks.
pub(crate) type OpenReplies =
    std::sync::Arc<std::sync::Mutex<Vec<(std::path::PathBuf, Result<(), String>)>>>;

/// Ask the session to open `path` in `app`, or in the app that opens its type.
pub(crate) fn request(path: &Path, app: Option<&str>) -> Result<(), String> {
    tools::open_document(named(path)?, app)
}

/// The apps that open the document at `path`, the one it opens with today
/// first.
///
/// # Errors
///
/// As [`request`], or the session did not answer the listing.
pub(crate) fn candidates(path: &Path) -> Result<Vec<DocumentApp>, String> {
    tools::list_document_apps(named(path)?)
}

/// `path` as the shell is told it: a document's name on the wire is text.
fn named(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "this file's name cannot be passed to the session".to_string())
}
