//! Addresses, search scopes and limits the browser's resources share.

pub const MAX_WEB_URL_BYTES: usize = 8 * 1024;
pub const MAX_BOOKMARK_TITLE_BYTES: usize = 4 * 1024;

/// A history entry's id lies beneath it.
pub const HISTORY_SCOPE: &str = "browser.history";
pub const BOOKMARKS_SCOPE: &str = "browser.application/bookmarks";
pub const DOWNLOADS_SCOPE: &str = "browser.downloads";

pub const MAX_SITE_EXCEPTIONS: usize = 1024;

/// History stays out of the shared document, so its id is derived from the URL.
pub fn history_resource_id(url: &str) -> String {
    format!(
        "{HISTORY_SCOPE}/{}",
        blake3::hash(url.trim().as_bytes()).to_hex()
    )
}

/// `url` as an http(s) address, absolute and bounded; `None` otherwise.
pub fn normalized_bookmark_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.len() > MAX_WEB_URL_BYTES {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.into())
}
