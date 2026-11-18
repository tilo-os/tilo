//! Addresses, origins and limits the browser's resource shares.

/// The longest address the browser accepts, in bytes.
pub const MAX_WEB_URL_BYTES: usize = 8 * 1024;
/// The longest bookmark title, in bytes.
pub const MAX_BOOKMARK_TITLE_BYTES: usize = 4 * 1024;
/// The most sites whose tracker protection may be off.
pub const MAX_SITE_EXCEPTIONS: usize = 1024;

/// A history entry's id lies beneath it.
pub const HISTORY_SCOPE: &str = "browser.history";
pub const BOOKMARKS_SCOPE: &str = "browser.application/bookmarks";
pub const DOWNLOADS_SCOPE: &str = "browser.downloads";

/// `url` as an http(s) address, absolute and bounded; `None` otherwise.
#[must_use]
pub fn normalized_bookmark_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.len() > MAX_WEB_URL_BYTES {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.into())
}

/// The exact origin `input` names, when it is an http(s) address.
#[must_use]
pub fn origin(input: &str) -> Option<String> {
    if input.len() > MAX_WEB_URL_BYTES {
        return None;
    }
    let url = url::Url::parse(input).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.origin().ascii_serialization())
}
