//! Item ids ⇄ URLs, and the HTTP date format.
//!
//! A `WebDAV` item's id is its decoded path relative to the account root:
//! `""` is the root, `"Docs/a b.txt"` a file. Hrefs from the server are
//! resolved against the root and accepted only when they stay on its origin
//! and under its path, and when no segment decodes to something that would
//! change the path's meaning (`/`, `.`, `..`, controls).

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

/// The parent id of a non-root id.
pub fn parent_of(id: &ItemId) -> ItemId {
    match id.as_str().rsplit_once('/') {
        Some((parent, _)) => ItemId::new(parent),
        None => ItemId::new(""),
    }
}

/// The last segment of an id.
pub fn name_of(id: &ItemId) -> &str {
    id.as_str()
        .rsplit_once('/')
        .map_or(id.as_str(), |(_, name)| name)
}

/// The id of `name` inside `parent`, if `name` is a valid segment.
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

/// Format a time as an IMF-fixdate.
pub fn format_http_date(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time)
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> Url {
        Url::parse("https://cloud.example/remote.php/dav/files/ada/").unwrap()
    }

    #[test]
    fn hrefs_map_to_decoded_relative_ids_and_back() {
        let root = root();
        assert_eq!(
            id_for_href(&root, "/remote.php/dav/files/ada/Notes%20and%20plans.md"),
            Some(ItemId::new("Notes and plans.md"))
        );
        assert_eq!(
            id_for_href(&root, "/remote.php/dav/files/ada/"),
            Some(ItemId::new(""))
        );
        assert_eq!(id_for_href(&root, "Photos/"), Some(ItemId::new("Photos")));
        let url = url_for(&root, &ItemId::new("Notes and plans.md"), false).unwrap();
        assert_eq!(
            url.as_str(),
            "https://cloud.example/remote.php/dav/files/ada/Notes%20and%20plans.md"
        );
        assert!(
            url_for(&root, &ItemId::new("Photos"), true)
                .unwrap()
                .as_str()
                .ends_with("/Photos/")
        );
    }

    #[test]
    fn unsafe_hrefs_are_refused() {
        let root = root();
        for href in [
            "https://attacker.example/remote.php/dav/files/ada/x",
            "/remote.php/dav/files/ada/bad%2Fname",
            "/remote.php/dav/files/ada/%2e%2e",
            "/remote.php/dav/files/ada/a?token=1",
            "/remote.php/dav/files/adam/x",
            "/remote.php/dav/files/ada/%ff",
        ] {
            assert_eq!(id_for_href(&root, href), None, "{href}");
        }
        assert!(url_for(&root, &ItemId::new("../x"), false).is_none());
    }

    #[test]
    fn http_dates_round_trip() {
        let time = parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        assert_eq!(
            time.duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            784_111_777
        );
        assert_eq!(format_http_date(time), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert!(parse_http_date("yesterday").is_none());
    }
}
