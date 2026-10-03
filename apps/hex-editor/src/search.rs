//! Search across the byte buffer — either a hex pattern (`de ad be ef`)
//! or a literal UTF-8 text string. Pure functions over `&[u8]`; the view
//! owns the query string and current match cursor.

/// What kind of needle the query string represents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    Hex,
    Text,
}

/// Upper bound for retained match offsets. A binary file can contain millions
/// of one-byte hits; keeping every one would turn a local find into an
/// unbounded allocation and make the result counter misleadingly expensive.
pub const MAX_RETAINED_MATCHES: usize = 10_000;

impl SearchKind {
    #[must_use]
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
    for pair in cleaned.as_bytes().chunks_exact(2) {
        let hi = (*pair.first()? as char).to_digit(16)?;
        let lo = (*pair.get(1)? as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
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
    find_all_bounded(haystack, needle, usize::MAX).0
}

/// Find overlapping matches while retaining at most `limit` offsets.
///
/// The boolean is true when more hits exist. Callers can show an honest
/// `10,000+` result count without allocating an offset for every matching byte
/// in a large file.
pub fn find_all_bounded(haystack: &[u8], needle: &[u8], limit: usize) -> (Vec<usize>, bool) {
    if needle.is_empty() || needle.len() > haystack.len() {
        return (Vec::new(), false);
    }
    let mut hits = Vec::new();
    let mut truncated = false;
    for (offset, window) in haystack.windows(needle.len()).enumerate() {
        if window != needle {
            continue;
        }
        if hits.len() == limit {
            truncated = true;
            break;
        }
        hits.push(offset);
    }
    (hits, truncated)
}

/// The first match at or after `from`, wrapping to the start of the
/// buffer if none is found in the tail.
pub fn find_next(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    let from = from.min(last.saturating_add(1));
    (from..=last)
        .find(|&offset| haystack.get(offset..offset + needle.len()) == Some(needle))
        .or_else(|| {
            (0..from).find(|&offset| haystack.get(offset..offset + needle.len()) == Some(needle))
        })
}

/// The last match strictly before `from`, wrapping to the end of the
/// buffer if none precedes it.
pub fn find_prev(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    let before = from.min(last.saturating_add(1));
    (0..before)
        .rev()
        .find(|&offset| haystack.get(offset..offset + needle.len()) == Some(needle))
        .or_else(|| {
            (before..=last)
                .rev()
                .find(|&offset| haystack.get(offset..offset + needle.len()) == Some(needle))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_query_accepts_separators_and_rejects_odd_or_garbage() {
        let cases: &[(&str, Option<Vec<u8>>)] = &[
            ("de ad be ef", Some(vec![0xDE, 0xAD, 0xBE, 0xEF])),
            ("DEAD", Some(vec![0xDE, 0xAD])),
            ("de-ad:be", Some(vec![0xDE, 0xAD, 0xBE])),
            ("abc", None), // odd digit count
            ("zz", None),  // not hex
            ("", None),
        ];
        for (query, want) in cases {
            assert_eq!(&parse_hex_query(query), want, "case {query:?}");
        }
    }

    #[test]
    fn find_helpers_overlap_bound_and_wrap() {
        // find_all reports overlapping matches.
        let hay = b"aaaa";
        assert_eq!(find_all(hay, b"aa"), vec![0, 1, 2]);

        // Bounded find reports more hits without retaining them.
        let (hits, truncated) = find_all_bounded(b"aaaa", b"a", 2);
        assert_eq!(hits, vec![0, 1]);
        assert!(truncated);

        // find_next wraps.
        let hay = b"\x00\xFF\x00\xFF";
        let needle = b"\xFF";
        assert_eq!(find_next(hay, needle, 0), Some(1));
        assert_eq!(find_next(hay, needle, 2), Some(3));
        // Past the last match wraps to the first.
        assert_eq!(find_next(hay, needle, 4), Some(1));

        // find_prev wraps.
        assert_eq!(find_prev(hay, needle, 3), Some(1));
        assert_eq!(find_prev(hay, needle, 1), Some(3)); // wraps to end

        // Navigation does not depend on retaining all hits.
        let hay = vec![0_u8; MAX_RETAINED_MATCHES + 12];
        assert_eq!(
            find_next(&hay, &[0], MAX_RETAINED_MATCHES + 5),
            Some(MAX_RETAINED_MATCHES + 5)
        );
        assert_eq!(
            find_prev(&hay, &[0], MAX_RETAINED_MATCHES + 5),
            Some(MAX_RETAINED_MATCHES + 4)
        );
    }

    #[test]
    fn needle_encodes_text_as_utf8_and_hex_as_bytes() {
        assert_eq!(
            needle("PNG", SearchKind::Text),
            Some(vec![0x50, 0x4E, 0x47])
        );
        // The PNG magic bytes.
        let hay = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let n = needle("50 4e 47", SearchKind::Hex).unwrap();
        assert_eq!(find_next(&hay, &n, 0), Some(1));
    }
}
