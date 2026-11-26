//! The Storage Access Framework half of the Android file picker
//! (`native::android::files`) that needs no JVM: which MIME types a chooser
//! asks for, what an activity result means, and the name a picked document is
//! stored under. Pure, so it is tested on every process.

use crate::portal::FileFilter;

/// `Activity.RESULT_OK`.
pub(crate) const RESULT_OK: i32 = -1;
/// `Activity.RESULT_CANCELED`.
pub(crate) const RESULT_CANCELED: i32 = 0;

const ANY_TYPE: &str = "*/*";
const OCTET_STREAM: &str = "application/octet-stream";
/// Longest file name Android's filesystems store, in bytes.
const MAX_NAME_BYTES: usize = 255;

/// The MIME types an `ACTION_OPEN_DOCUMENT` chooser offers for `filters`.
///
/// Android shows one merged list instead of a type dropdown, so this is the
/// union of every filter. A filter MIME types cannot express (an "All files"
/// entry, a glob that is not `*.ext`, an extension `mime_type` does not know)
/// widens the chooser to everything: a narrower one would hide a file the app
/// offered to open.
pub(crate) fn open_mime_types(
    filters: &[FileFilter],
    mut mime_type: impl FnMut(&str) -> Option<String>,
) -> Vec<String> {
    let any = || vec![ANY_TYPE.to_string()];
    if filters.is_empty() {
        return any();
    }
    let mut types = Vec::new();
    let mut add = |mime: String| {
        if !types.contains(&mime) {
            types.push(mime);
        }
    };
    for filter in filters {
        if filter.globs().is_empty() && filter.mimetypes().is_empty() {
            return any();
        }
        for mime in filter.mimetypes() {
            if mime == ANY_TYPE {
                return any();
            }
            add(mime.to_ascii_lowercase());
        }
        for glob in filter.globs() {
            let Some(mime) = glob_extension(glob).and_then(|extension| mime_type(&extension))
            else {
                return any();
            };
            add(mime.to_ascii_lowercase());
        }
    }
    types
}

/// The type an `ACTION_CREATE_DOCUMENT` chooser creates `suggested_name`
/// with. The name's own extension is the most specific answer; then the first
/// concrete type the filters offer.
pub(crate) fn save_mime_type(
    filters: &[FileFilter],
    suggested_name: &str,
    mut mime_type: impl FnMut(&str) -> Option<String>,
) -> String {
    let from_name = suggested_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .filter(|extension| !extension.is_empty())
        .and_then(|extension| mime_type(&extension));
    from_name
        .or_else(|| {
            filters
                .iter()
                .flat_map(FileFilter::mimetypes)
                .find(|mime| !mime.contains('*'))
                .cloned()
        })
        .unwrap_or_else(|| OCTET_STREAM.to_string())
}

/// `png` from `*.png`; `None` for a glob that is not a plain extension match.
fn glob_extension(glob: &str) -> Option<String> {
    let extension = glob.strip_prefix("*.")?;
    let plain = !extension.is_empty()
        && extension
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '+'));
    plain.then(|| extension.to_ascii_lowercase())
}

/// What a document chooser answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DocumentResult {
    /// The chosen `content:` URIs, in the order the chooser returned them.
    /// Never empty.
    Picked(Vec<String>),
    Cancelled,
    Failed(String),
}

/// Read an activity result. Only `content:` URIs count as picks: the
/// Storage Access Framework returns nothing else, and a `file:` URI from a
/// misbehaving chooser must not reach the app as a document it may read.
pub(crate) fn document_result(
    result_code: i32,
    uris: Vec<String>,
    error: Option<String>,
) -> DocumentResult {
    if let Some(error) = error {
        return DocumentResult::Failed(error);
    }
    match result_code {
        RESULT_OK => {
            let uris: Vec<String> = uris
                .into_iter()
                .filter(|uri| uri.starts_with("content:"))
                .collect();
            if uris.is_empty() {
                DocumentResult::Failed("the chooser returned no document".to_string())
            } else {
                DocumentResult::Picked(uris)
            }
        }
        RESULT_CANCELED => DocumentResult::Cancelled,
        other => DocumentResult::Failed(format!("the chooser failed with result {other}")),
    }
}

/// A single file name for `display_name`, the provider's label for a
/// document: separators and NUL are replaced, a name that would climb or stay
/// in place is renamed, and an overlong one is shortened keeping its
/// extension.
pub(crate) fn document_file_name(display_name: Option<&str>) -> String {
    let name: String = display_name
        .unwrap_or_default()
        .trim()
        .chars()
        .map(|c| if matches!(c, '/' | '\0') { '_' } else { c })
        .collect();
    let name = if matches!(name.as_str(), "" | "." | "..") {
        "document".to_string()
    } else {
        name
    };
    if name.len() <= MAX_NAME_BYTES {
        return name;
    }
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| extension.len() <= 16)
        .unwrap_or_default();
    let keep = MAX_NAME_BYTES
        .saturating_sub(extension.len())
        .saturating_sub(usize::from(!extension.is_empty()));
    let end = (0..=keep)
        .rev()
        .find(|&end| name.is_char_boundary(end))
        .unwrap_or_default();
    let stem = name.get(..end).unwrap_or_default();
    if extension.is_empty() {
        stem.to_string()
    } else {
        format!("{stem}.{extension}")
    }
}
