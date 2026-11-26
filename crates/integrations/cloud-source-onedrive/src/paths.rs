//! Path segments in Graph URLs.

/// Percent-encode one path segment: everything but RFC 3986 unreserved
/// characters, so ids and names with `#`, `?`, `%`, `:` or spaces address
/// exactly one segment.
pub fn encode_segment(segment: &str) -> String {
    percent_encoding::utf8_percent_encode(segment, SEGMENT).to_string()
}

/// Everything but the RFC 3986 unreserved characters and `!`.
const SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~')
    .remove(b'!');

/// Encode a `/`-separated path relative to a folder; `None` when it names
/// the folder itself.
pub fn encode_path(path: &str) -> Option<String> {
    let segments: Vec<String> = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(encode_segment)
        .collect();
    (!segments.is_empty()).then(|| segments.join("/"))
}

/// Decode `%XX` escapes (and nothing else). Invalid escapes stay as they
/// are.
pub fn percent_decode(text: &str) -> String {
    percent_encoding::percent_decode_str(text)
        .decode_utf8_lossy()
        .into_owned()
}

/// A name this client will send: one non-empty segment. Graph judges the
/// rest (reserved characters and names differ between drive types) and
/// answers `400`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/')
}
