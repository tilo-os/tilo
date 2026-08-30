//! Search across the byte buffer — either a hex pattern (`de ad be ef`)
//! or a literal UTF-8 text string. Pure functions over `&[u8]`; the view
//! owns the query string and current match cursor.

/// What kind of needle the query string represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    Hex,
    Text,
}

impl SearchKind {
    pub fn toggle(self) -> Self {
        match self {
            SearchKind::Hex => SearchKind::Text,
            SearchKind::Text => SearchKind::Hex,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SearchKind::Hex => "hex",
            SearchKind::Text => "text",
        }
    }
}

/// Parse a hex query like "de ad", "deadbeef", or "DE-AD" into raw bytes.
/// Whitespace and the separators `-`, `:`, `,` are ignored. Returns None
/// if any remaining char isn't a hex digit or the digit count is odd.
pub fn parse_hex_query(query: &str) -> Option<Vec<u8>> {
    let cleaned: String = query
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | ':' | ',' | '_'))
        .collect();
    if cleaned.is_empty() || !cleaned.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(cleaned.len() / 2);
    let bytes = cleaned.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
        i += 2;
    }
    Some(out)
}

/// Turn a query string + kind into the literal needle bytes.
pub fn needle(query: &str, kind: SearchKind) -> Option<Vec<u8>> {
    match kind {
        SearchKind::Hex => parse_hex_query(query),
        SearchKind::Text => {
            if query.is_empty() {
                None
            } else {
                Some(query.as_bytes().to_vec())
            }
        }
    }
}

/// Find every match offset of `needle` in `haystack` (overlapping). Used
/// for the "N matches" indicator and to drive next/prev navigation.
pub fn find_all(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let last = haystack.len() - needle.len();
    let mut i = 0;
    while i <= last {
        if &haystack[i..i + needle.len()] == needle {
            hits.push(i);
        }
        i += 1;
    }
    hits
}

/// The first match at or after `from`, wrapping to the start of the
/// buffer if none is found in the tail.
pub fn find_next(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let hits = find_all(haystack, needle);
    if hits.is_empty() {
        return None;
    }
    hits.iter()
        .find(|&&h| h >= from)
        .copied()
        .or_else(|| hits.first().copied())
}

/// The last match strictly before `from`, wrapping to the end of the
/// buffer if none precedes it.
pub fn find_prev(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let hits = find_all(haystack, needle);
    if hits.is_empty() {
        return None;
    }
    hits.iter()
        .rev()
        .find(|&&h| h < from)
        .copied()
        .or_else(|| hits.last().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_handles_separators() {
        assert_eq!(
            parse_hex_query("de ad be ef"),
            Some(vec![0xDE, 0xAD, 0xBE, 0xEF])
        );
        assert_eq!(parse_hex_query("DEAD"), Some(vec![0xDE, 0xAD]));
        assert_eq!(parse_hex_query("de-ad:be"), Some(vec![0xDE, 0xAD, 0xBE]));
    }

    #[test]
    fn parse_hex_rejects_odd_and_garbage() {
        assert_eq!(parse_hex_query("abc"), None); // odd digit count
        assert_eq!(parse_hex_query("zz"), None); // not hex
        assert_eq!(parse_hex_query(""), None);
    }

    #[test]
    fn find_all_overlapping() {
        let hay = b"aaaa";
        assert_eq!(find_all(hay, b"aa"), vec![0, 1, 2]);
    }

    #[test]
    fn find_next_wraps() {
        let hay = b"\x00\xFF\x00\xFF";
        let needle = b"\xFF";
        assert_eq!(find_next(hay, needle, 0), Some(1));
        assert_eq!(find_next(hay, needle, 2), Some(3));
        // Past the last match wraps to the first.
        assert_eq!(find_next(hay, needle, 4), Some(1));
    }

    #[test]
    fn find_prev_wraps() {
        let hay = b"\x00\xFF\x00\xFF";
        let needle = b"\xFF";
        assert_eq!(find_prev(hay, needle, 3), Some(1));
        assert_eq!(find_prev(hay, needle, 1), Some(3)); // wraps to end
    }

    #[test]
    fn text_needle_is_utf8_bytes() {
        assert_eq!(
            needle("PNG", SearchKind::Text),
            Some(vec![0x50, 0x4E, 0x47])
        );
    }

    #[test]
    fn hex_signature_matches_in_png() {
        // The PNG magic bytes.
        let hay = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let n = needle("50 4e 47", SearchKind::Hex).unwrap();
        assert_eq!(find_next(&hay, &n, 0), Some(1));
    }
}
