//! "Open with": which applications can open a file, and the session asked to open
//! it in one.

use runtime_portal::tools::{self, DocumentApp};
use std::path::Path;

pub(crate) type OpenRequest = fn(&Path, Option<&str>) -> Result<(), String>;

pub(crate) type CandidateList = fn(&Path) -> Result<Vec<DocumentApp>, String>;

pub(crate) type OpenReplies =
    std::sync::Arc<std::sync::Mutex<Vec<(std::path::PathBuf, Result<(), String>)>>>;

pub(crate) fn request(path: &Path, app: Option<&str>) -> Result<(), String> {
    tools::open_document(named(path)?, app)
}

pub(crate) fn candidates(path: &Path) -> Result<Vec<DocumentApp>, String> {
    tools::list_document_apps(named(path)?)
}

fn named(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "this file's name cannot be passed to the session".to_string())
}
