//! Item ids ⇄ URLs, and the HTTP date format.

use std::time::SystemTime;

use cloud_source::ItemId;
use tilo_dav::Url;

/// The URL of `id` below `root` (which ends in `/`). Folders get a
/// trailing slash.
pub fn url_for(root: &Url, id: &ItemId, folder: bool) -> Option<Url> {
    let mut url = root.clone();
    if !id.as_str().is_empty() {
        let mut segments = url.path_segments_mut().ok()?;
        segments.pop_if_empty();
        for segment in id.as_str().split('/') {
            if !valid_segment(segment) {
                return None;
            }
            segments.push(segment);
        }
        if folder {
            segments.push("");
        }
    }
    Some(url)
}

/// The id of a server href, or `None` when it is outside the root or
/// unsafe.
pub fn id_for_href(root: &Url, href: &str) -> Option<ItemId> {
    let url = root.join(href).ok()?;
    if url.scheme() != root.scheme()
        || url.host_str() != root.host_str()
        || url.port_or_known_default() != root.port_or_known_default()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let root_path = root.path().trim_end_matches('/');
    let rest = url.path().strip_prefix(root_path)?;
    if !rest.is_empty() && !rest.starts_with('/') {
        return None;
    }
    let mut parts = Vec::new();
    for raw in rest.split('/').filter(|part| !part.is_empty()) {
        let decoded = percent_decode(raw)?;
        if !valid_segment(&decoded) {
            return None;
        }
        parts.push(decoded);
    }
    Some(ItemId::new(parts.join("/")))
}

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains('/')
        && !segment.chars().any(char::is_control)
}

pub fn parent_of(id: &ItemId) -> ItemId {
    match id.as_str().rsplit_once('/') {
        Some((parent, _)) => ItemId::new(parent),
        None => ItemId::new(""),
    }
}

pub fn name_of(id: &ItemId) -> &str {
    id.as_str()
        .rsplit_once('/')
        .map_or(id.as_str(), |(_, name)| name)
}

pub fn child_of(parent: &ItemId, name: &str) -> Option<ItemId> {
    if !valid_segment(name) {
        return None;
    }
    Some(if parent.as_str().is_empty() {
        ItemId::new(name)
    } else {
        ItemId::new(format!("{}/{name}", parent.as_str()))
    })
}

/// Percent-decoding to UTF-8; `None` when the decoded bytes are not UTF-8.
pub fn percent_decode(raw: &str) -> Option<String> {
    percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .ok()
        .map(std::borrow::Cow::into_owned)
}

/// Parse an IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), the format
/// `getlastmodified` uses.
pub fn parse_http_date(value: &str) -> Option<SystemTime> {
    cloud_source::http::parse_http_date(value)
}
