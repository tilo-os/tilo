//! Word tokenization with byte-offset spans.

/// A single word span within a source string.
///
/// `text` is a borrowed slice of the original input, and `start`/`end` are the
/// byte offsets such that `&source[start..end] == text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    /// The word as it appears in the source (original case, original characters).
    pub text: &'a str,
    /// Byte offset of the first byte of `text` in the source string.
    pub start: usize,
    /// Byte offset one past the last byte of `text` in the source string.
    pub end: usize,
}

/// Split `text` into word tokens, preserving byte offsets.
///
/// Tokenization rules:
/// - A token is a maximal run of "word characters".
/// - A word character is any Unicode alphanumeric, plus the connector
///   characters `'`, `.`, `@`, `:`, `/`, and `_` *when they sit between two
///   other word characters*. Keeping these connectors internal lets
///   contractions ("don't"), decimals ("3.14"), URLs and emails survive as one
///   token, so downstream filtering in [`crate::check`] can recognise and skip
///   them as a unit.
/// - Leading/trailing connectors are trimmed off the span (so `"world!"` yields
///   `world` and `'tis` keeps its leading apostrophe stripped to `tis`).
///
/// Offsets are always on char boundaries, so `&text[tok.start..tok.end]` is
/// always valid and equals `tok.text`.
pub fn tokenize(text: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let bytes_len = text.len();
    let mut iter = text.char_indices().peekable();

    while let Some(&(idx, ch)) = iter.peek() {
        if is_word_start(ch) {
            let start = idx;
            let mut end = idx.saturating_add(ch.len_utf8());
            iter.next();

            // Extend over word chars and *internal* connectors.
            while let Some(&(j, c)) = iter.peek() {
                if is_word_char(c) {
                    end = j.saturating_add(c.len_utf8());
                    iter.next();
                } else if is_connector(c) {
                    // Keep the connector only if a word char eventually follows
                    // it across an unbroken run of connectors. This lets `://`
                    // and `.co.` chains inside a URL stay attached, while a
                    // trailing `'` or `.` at the end of a word is dropped.
                    if word_char_follows_connectors(text, j.saturating_add(c.len_utf8())) {
                        end = j.saturating_add(c.len_utf8());
                        iter.next();
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }

            debug_assert!(end <= bytes_len);
            // `start` and `end` are always char boundaries: `start` comes
            // straight from `char_indices`, and `end` is only ever advanced by
            // a char's own `len_utf8()` from a boundary already known to be
            // valid. `.get()` still guards against that invariant ever lapsing
            // rather than slicing straight through it.
            tokens.push(Token {
                text: text.get(start..end).unwrap_or(""),
                start,
                end,
            });
        } else {
            iter.next();
        }
    }

    tokens
}

/// A token may *start* only on an alphanumeric (never on a bare connector), so
/// that punctuation like `'` or `.` does not anchor a token by itself.
fn is_word_start(c: char) -> bool {
    c.is_alphanumeric()
}

/// Characters that always belong to a word.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// Characters that join two word characters into one token but are not words on
/// their own.
fn is_connector(c: char) -> bool {
    matches!(
        c,
        '\'' | '\u{2019}' | '.' | '@' | ':' | '/' | '_' | ',' | '-'
    )
}

/// Starting at byte offset `idx`, skip any run of connector characters and
/// report whether the first non-connector reached is a word character. Used so a
/// connector run like `://` attaches to the following word characters.
fn word_char_follows_connectors(text: &str, idx: usize) -> bool {
    // `idx` is always a char boundary (see the callers), so this can only ever
    // hit the `Some` arm; `.get()` keeps that a guarantee rather than an
    // assumption a future caller could silently break.
    for c in text.get(idx..).unwrap_or("").chars() {
        if is_word_char(c) {
            return true;
        }
        if is_connector(c) {
            continue;
        }
        return false;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<&str> {
        tokenize(text).into_iter().map(|t| t.text).collect()
    }

    #[test]
    fn tokenize_splits_words_keeping_apostrophes_urls_and_decimals() {
        for (case, text, want) in [
            ("basic_split", "hello world", &["hello", "world"][..]),
            (
                "strips_surrounding_punctuation",
                "Hello, world!",
                &["Hello", "world"],
            ),
            (
                "strips_surrounding_punctuation",
                "(parenthetical)",
                &["parenthetical"],
            ),
            (
                "keeps_internal_apostrophe",
                "don't can't won't",
                &["don't", "can't", "won't"],
            ),
            // U+2019 RIGHT SINGLE QUOTATION MARK as used by smart-quote editors.
            ("unicode_apostrophe_kept", "don\u{2019}t", &["don\u{2019}t"]),
            (
                "url_and_email_stay_intact",
                "see https://example.com or a@b.com",
                &["see", "https://example.com", "or", "a@b.com"],
            ),
            (
                "decimals_intact",
                "pi is 3.14 ok",
                &["pi", "is", "3.14", "ok"],
            ),
            ("unicode_letters_are_words", "café déjà", &["café", "déjà"]),
            ("empty_input", "", &[]),
            ("empty_input", "   ,.!  ", &[]),
        ] {
            assert_eq!(words(text), want, "case {case}: {text:?}");
        }
        // offsets_round_trip
        let text = "  café, déjà-vu? oui!";
        for tok in tokenize(text) {
            assert_eq!(&text[tok.start..tok.end], tok.text);
        }
    }
}
