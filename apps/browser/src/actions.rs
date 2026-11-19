//! Addresses, search scopes and limits the browser's resources share.

pub use browser_model::urls::{
    BOOKMARKS_SCOPE, DOWNLOADS_SCOPE, HISTORY_SCOPE, MAX_BOOKMARK_TITLE_BYTES, MAX_SITE_EXCEPTIONS,
    MAX_WEB_URL_BYTES, normalized_bookmark_url, origin,
};

/// History stays out of the shared document, so its id is derived from the URL.
pub fn history_resource_id(url: &str) -> String {
    format!(
        "{HISTORY_SCOPE}/{}",
        blake3::hash(url.trim().as_bytes()).to_hex()
    )
}
