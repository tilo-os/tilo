//! Word tokenization with byte-offset spans.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token<'a> {
    pub text: &'a str,
    pub start: usize,
    pub end: usize,
}

/// Split `text` into word tokens, preserving byte offsets: a token is a maximal run of
/// word characters (Unicode alphanumerics plus the connector set), and connectors stay
/// inside a token only between word characters.
pub fn tokenize(text: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let bytes_len = text.len();
    let mut iter = text.char_indices().peekable();

    while let Some(&(idx, ch)) = iter.peek() {
        if is_word_start(ch) {
            let start = idx;
            let mut end = idx.saturating_add(ch.len_utf8());
            iter.next();

            while let Some(&(j, c)) = iter.peek() {
                if is_word_char(c) {
                    end = j.saturating_add(c.len_utf8());
                    iter.next();
                } else if is_connector(c) {
                    // Keep the connector only if a word character follows across an unbroken run of
                    // connectors, so `://` and `.co.` stay in a URL while a trailing `'` or `.` does not.
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

fn is_connector(c: char) -> bool {
    matches!(
        c,
        '\'' | '\u{2019}' | '.' | '@' | ':' | '/' | '_' | ',' | '-'
    )
}

/// Starting at byte offset `idx`, skip any run of connectors and report whether the first
/// non-connector is a word character.
fn word_char_follows_connectors(text: &str, idx: usize) -> bool {
    // `idx` is always a char boundary (see the callers), so this can only hit `Some`.
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
        let text = "  café, déjà-vu? oui!";
        for tok in tokenize(text) {
            assert_eq!(&text[tok.start..tok.end], tok.text);
        }
    }
}
