//! The `Text` surface: how a text is counted, read, cut, cased and matched. One reading serves every place
//! that answers a `Text` method: the runtime's host calls, the index's `.len()` of a text column and the
//! identifier patterns search matches.
//!
//! A text is counted in graphemes — what a person sees as one character, so an emoji with its modifiers is
//! one, and cutting a text never splits one in two. `len`, `find` and `slice` all count that way.
//!
//! Casing is Unicode's full mapping (`ß` uppercases to `SS`), not a locale's: the runtime holds no locale
//! data. A locale's tailoring (Turkish `ı`) is the platform's formatting to make.

use unicode_segmentation::UnicodeSegmentation;

#[must_use]
pub fn len(text: &str) -> usize {
    text.graphemes(true).count()
}

/// How many words a text holds: its runs of non-whitespace, which is how a person counts the words of what
/// they wrote (punctuation alone is a word of its own).
#[must_use]
pub fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

#[must_use]
pub fn find(text: &str, needle: &str) -> Option<usize> {
    let at = text.find(needle)?;
    Some(text.get(..at).map_or(0, len))
}

/// The graphemes from `from` to before `to`, counted as [`len`] counts them; a bound past the end is the
/// end, and `from` past `to` is empty.
#[must_use]
pub fn slice(text: &str, from: i64, to: i64) -> String {
    let count = len(text);
    let clamp = |n: i64| usize::try_from(n).unwrap_or(0).min(count);
    let (from, to) = (clamp(from), clamp(to));
    text.graphemes(true)
        .skip(from)
        .take(to.saturating_sub(from))
        .collect()
}

#[must_use]
pub fn trim(text: &str) -> &str {
    text.trim()
}

#[must_use]
pub fn lower(text: &str) -> String {
    text.to_lowercase()
}

#[must_use]
pub fn upper(text: &str) -> String {
    text.to_uppercase()
}

#[must_use]
pub fn starts_with(text: &str, prefix: &str) -> bool {
    text.starts_with(prefix)
}

#[must_use]
pub fn ends_with(text: &str, suffix: &str) -> bool {
    text.ends_with(suffix)
}

#[must_use]
pub fn contains(text: &str, needle: &str) -> bool {
    text.contains(needle)
}

/// Whether `text` matches `pattern`, in which each `*` stands for any text and every other character
/// stands for itself. Case-sensitive: a caller that wants case folded folds it first
/// ([`identifier::matches`](crate::identifier::matches) does).
#[must_use]
pub fn matches(pattern: &str, text: &str) -> bool {
    let mut pieces = pattern.split('*');
    let Some(first) = pieces.next() else {
        return true;
    };
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let pieces: Vec<&str> = pieces.collect();
    let Some((last, middle)) = pieces.split_last() else {
        return rest.is_empty();
    };
    for piece in middle {
        match rest.split_once(piece) {
            Some((_, after)) => rest = after,
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// The text cut at each `sep`. A separator is text, not a pattern; an empty one cuts the text into its
/// graphemes. A separator at either end leaves an empty piece (`"a,"` is `["a", ""]`), so no text is lost.
#[must_use]
pub fn split(text: &str, sep: &str) -> Vec<String> {
    if sep.is_empty() {
        return text.graphemes(true).map(str::to_owned).collect();
    }
    text.split(sep).map(str::to_owned).collect()
}

#[must_use]
pub fn join(pieces: &[&str], sep: &str) -> String {
    pieces.join(sep)
}

/// `text` escaped so it travels in a URL's query component: every byte outside the unreserved
/// letters, digits and `-._~` written as `%XX`.
#[must_use]
pub fn url_encode(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// `text` with its percent escapes (`%XX`) read back: the bytes they stand for, and `+` as a
/// space, as a link's query means them to the person who wrote it.
#[must_use]
pub fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let high = (bytes[i + 1] as char).to_digit(16);
                let low = (bytes[i + 2] as char).to_digit(16);
                match (high, low) {
                    (Some(high), Some(low)) => {
                        out.push((high * 16 + low) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
