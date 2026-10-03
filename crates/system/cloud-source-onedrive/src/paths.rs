//! Path segments in Graph URLs, and the headers that describe byte ranges.

use std::ops::Range;

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

/// `Content-Range: bytes <first>-<last>/<total|*>` as a half-open range and
/// the total.
pub fn parse_content_range(value: &str) -> Option<(Range<u64>, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (span, total) = rest.split_once('/')?;
    let (first, last) = span.split_once('-')?;
    let first: u64 = first.trim().parse().ok()?;
    let last: u64 = last.trim().parse().ok()?;
    let total = match total.trim() {
        "*" => None,
        total => Some(total.parse().ok()?),
    };
    (first <= last).then(|| (first..last.saturating_add(1), total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_encode_reserved_characters_and_round_trip() {
        assert_eq!(encode_segment("a b#c?d%e:f"), "a%20b%23c%3Fd%25e%3Af");
        assert_eq!(encode_segment("0123ABC!104"), "0123ABC!104");
        assert_eq!(encode_segment("é"), "%C3%A9");
        assert_eq!(percent_decode(&encode_segment("a b/é%")), "a b/é%");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(encode_path("/Docs//a b.txt"), Some("Docs/a%20b.txt".into()));
        assert_eq!(encode_path("/"), None);
    }

    #[test]
    fn content_ranges_parse_half_open() {
        assert_eq!(
            parse_content_range("bytes 0-1023/2048"),
            Some((0..1024, Some(2048)))
        );
        assert_eq!(parse_content_range("bytes 5-5/*"), Some((5..6, None)));
        assert_eq!(parse_content_range("bytes 9-3/10"), None);
        assert_eq!(parse_content_range("items 0-1/2"), None);
    }

    #[test]
    fn names_are_single_segments() {
        assert!(valid_name("report.pdf"));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(".."));
        assert!(!valid_name(""));
    }
}
