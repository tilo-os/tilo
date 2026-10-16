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

/// How a text query matches the bytes: letter case and word boundaries. A
/// hex query is always exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchRules {
    /// Letters must agree in case.
    pub match_case: bool,
    /// The match must not touch another letter, digit or underscore.
    pub whole_word: bool,
}

impl MatchRules {
    /// Byte-exact, anywhere: how a hex pattern matches.
    pub const EXACT: Self = Self {
        match_case: true,
        whole_word: false,
    };
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Like [`find_all_bounded`], under `rules`.
pub fn find_matches(
    haystack: &[u8],
    needle: &[u8],
    rules: MatchRules,
    limit: usize,
) -> (Vec<usize>, bool) {
    if needle.is_empty() || needle.len() > haystack.len() {
        return (Vec::new(), false);
    }
    let mut hits = Vec::new();
    let mut truncated = false;
    for (offset, window) in haystack.windows(needle.len()).enumerate() {
        let same = if rules.match_case {
            window == needle
        } else {
            window.eq_ignore_ascii_case(needle)
        };
        if !same {
            continue;
        }
        if rules.whole_word {
            let before = offset.checked_sub(1).and_then(|i| haystack.get(i));
            let after = haystack.get(offset + needle.len());
            if before.copied().is_some_and(is_word_byte) || after.copied().is_some_and(is_word_byte)
            {
                continue;
            }
        }
        if hits.len() == limit {
            truncated = true;
            break;
        }
        hits.push(offset);
    }
    (hits, truncated)
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
    fn find_matches_honours_case_and_word_boundaries() {
        let hay = b"IHDR ihdr IHDRx _IHDR";
        let any = MatchRules {
            match_case: false,
            whole_word: false,
        };
        assert_eq!(find_matches(hay, b"ihdr", any, 10).0, vec![0, 5, 10, 17]);
        let exact = MatchRules {
            match_case: true,
            whole_word: false,
        };
        assert_eq!(find_matches(hay, b"ihdr", exact, 10).0, vec![5]);
        let word = MatchRules {
            match_case: false,
            whole_word: true,
        };
        assert_eq!(find_matches(hay, b"ihdr", word, 10).0, vec![0, 5]);
        assert_eq!(find_matches(hay, b"IHDR", word, 1), (vec![0], true));
    }

    #[test]
    fn needle_encodes_text_as_utf8_and_hex_as_bytes() {
        assert_eq!(
            needle("PNG", SearchKind::Text),
            Some(vec![0x50, 0x4E, 0x47])
        );
        // The PNG magic bytes.
        assert_eq!(
            needle("50 4e 47", SearchKind::Hex),
            Some(vec![0x50, 0x4E, 0x47])
        );
    }
}
