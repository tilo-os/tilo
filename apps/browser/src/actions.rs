//! Addresses, search scopes and limits the browser's resources share.
//!
//! Navigation and the visit log remain local or the person's own. Bookmarks
//! and preferences are the browser document's ([`crate::resource::Browser`]):
//! every visible control, Ctrl+K and agents change them through its verbs
//! and field edits.

pub(crate) const MAX_WEB_URL_BYTES: usize = 8 * 1024;
pub(crate) const MAX_BOOKMARK_TITLE_BYTES: usize = 4 * 1024;

/// Search scope and resource id of the history section. A history entry's id
/// lies beneath it, so a search narrowed to History admits exactly its hits.
pub const HISTORY_SCOPE: &str = "browser.history";
/// Search scope of the bookmarks: every bookmark's item resource lies
/// beneath it.
pub const BOOKMARKS_SCOPE: &str = "browser.application/bookmarks";
/// Search scope and resource id of the downloads section.
pub const DOWNLOADS_SCOPE: &str = "browser.downloads";

/// The most site exceptions the document keeps.
pub const MAX_SITE_EXCEPTIONS: usize = 1024;

/// Stable identity for a private history entry.  History is deliberately not
/// copied into the shared document (visited URLs are sensitive activity), but
/// it still needs the same canonical identity everywhere it is exposed: the
/// library tree, global search, and an agent's resource address.
pub fn history_resource_id(url: &str) -> String {
    format!(
        "{HISTORY_SCOPE}/{}",
        blake3::hash(url.trim().as_bytes()).to_hex()
    )
}

/// `url` as the document keeps a web address: absolute, http(s), bounded,
/// normalized; `None` for anything else.
pub fn normalized_bookmark_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.len() > MAX_WEB_URL_BYTES {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.into())
}
