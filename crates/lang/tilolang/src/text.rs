//! The `Text` surface: how a text is counted, read, cut, cased and matched. One reading serves
//! every place that answers a `Text` method: the runtime's host calls, the index's `.len()` of a
//! text column and the identifier patterns search matches.
//!
//! A text is counted in graphemes — what a person sees as one character, so an emoji with its
//! modifiers or a letter with its accents is one, and cutting a text never splits one in two.
//! `len`, `find` and `slice` all count that way, so an index from one is an index for the others.
//!
//! Casing is Unicode's full mapping (`ß` uppercases to `SS`), not a locale's: the runtime holds
//! no locale data, and a value is computed where the code runs rather than where it is drawn.
//! A locale's tailoring (Turkish `ı`) is the platform's formatting to make.

use unicode_segmentation::UnicodeSegmentation;

/// How many graphemes a text has.
#[must_use]
pub fn len(text: &str) -> usize {
    text.graphemes(true).count()
}

/// How many words a text holds: its runs of non-whitespace, which is how a person counts the
/// words of what they wrote (`a text of words` is four, and punctuation alone is a word of
/// its own).
#[must_use]
pub fn words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Where `needle` first stands, counted in graphemes from the start.
#[must_use]
pub fn find(text: &str, needle: &str) -> Option<usize> {
    let at = text.find(needle)?;
    Some(text.get(..at).map_or(0, len))
}

/// The graphemes from `from` to before `to`, counted as [`len`] counts them; a bound past the
/// end is the end, and `from` past `to` is empty.
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

/// The text without the whitespace at either end.
#[must_use]
pub fn trim(text: &str) -> &str {
    text.trim()
}

/// The text in lowercase, by Unicode's full mapping.
#[must_use]
pub fn lower(text: &str) -> String {
    text.to_lowercase()
}

/// The text in uppercase, by Unicode's full mapping.
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

/// Whether `text` matches `pattern`, in which each `*` stands for any text and every other
/// character stands for itself. Case-sensitive: a caller that wants case folded folds it first
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

/// The text cut at each `sep`. A separator is text, not a pattern; an empty one cuts the text
/// into its graphemes, there being nothing else to cut it at. A separator at either end leaves
/// an empty piece (`"a,"` is `["a", ""]`), so no text is lost.
#[must_use]
pub fn split(text: &str, sep: &str) -> Vec<String> {
    if sep.is_empty() {
        return text.graphemes(true).map(str::to_owned).collect();
    }
    text.split(sep).map(str::to_owned).collect()
}

/// The pieces as one text, each after the other with `sep` between them.
#[must_use]
pub fn join(pieces: &[&str], sep: &str) -> String {
    pieces.join(sep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_is_counted_in_graphemes() {
        assert_eq!(len(""), 0);
        assert_eq!(len("héllo"), 5);
        assert_eq!(len("e\u{301}"), 1, "a letter and its accent are one");
        assert_eq!(len("👩‍👩‍👧‍👦"), 1, "a family is one grapheme");
        assert_eq!(len("🇩🇪!"), 2, "a flag is one");
        assert_eq!(len("a🇩🇪"), 2);
    }

    #[test]
    fn finding_and_cutting_count_the_same_way() {
        assert_eq!(find("abc", "c"), Some(2));
        assert_eq!(find("🇩🇪x", "x"), Some(1));
        assert_eq!(
            find("e\u{301}x", "x"),
            Some(1),
            "an accent makes no second place"
        );
        assert_eq!(find("abc", "z"), None);
        assert_eq!(find("abc", ""), Some(0));
        assert_eq!(slice("héllo", 1, 3), "él");
        assert_eq!(slice("👩‍👩‍👧‍👦x", 1, 2), "x", "a cut never splits a grapheme");
        assert_eq!(slice("abc", 0, 99), "abc", "past the end is the end");
        assert_eq!(slice("abc", 2, 1), "", "from after to is empty");
        assert_eq!(slice("abc", -3, 2), "ab", "a negative index is the start");
    }

    #[test]
    fn casing_is_unicode_s_full_mapping() {
        assert_eq!(upper("straße"), "STRASSE");
        assert_eq!(lower("HÉLLO"), "héllo");
        assert_eq!(lower("ΑΣ"), "ας", "a final sigma is its own case");
        assert_eq!(trim("  a\t\n"), "a");
        assert_eq!(
            trim("\u{a0}a\u{a0}"),
            "a",
            "a no-break space is whitespace too"
        );
    }

    #[test]
    fn words_are_a_text_s_runs_of_non_whitespace() {
        assert_eq!(words(""), 0);
        assert_eq!(words(" \n\t "), 0);
        assert_eq!(words("one"), 1);
        assert_eq!(words("one two  three\nfour"), 4);
        assert_eq!(words("—"), 1, "a run of punctuation is a word");
        assert_eq!(words("👩‍👩‍👧‍👦 x"), 2, "a family is one word");
    }

    #[test]
    fn matching_takes_a_star_per_any_run() {
        assert!(matches("a*", "abc"));
        assert!(matches("*c", "abc"));
        assert!(matches("a*c", "abc"));
        assert!(matches("a*c", "ac"));
        assert!(matches("a*b*c", "axxbyyc"));
        assert!(matches("*", ""));
        assert!(matches("abc", "abc"));
        assert!(!matches("abc", "abcd"));
        assert!(!matches("a*c", "abx"), "the tail must end it");
        assert!(!matches("A*", "abc"), "matching folds no case by itself");
    }

    #[test]
    fn splitting_keeps_every_piece() {
        assert_eq!(split("a,b", ","), ["a", "b"]);
        assert_eq!(split("a,,b", ","), ["a", "", "b"]);
        assert_eq!(split("a,", ","), ["a", ""]);
        assert_eq!(split("abc", ""), ["a", "b", "c"]);
        assert_eq!(split("👩‍👩‍👧‍👦x", ""), ["👩‍👩‍👧‍👦", "x"]);
        assert_eq!(split("", ","), [""]);
        assert_eq!(join(&["a", "b"], ", "), "a, b");
        assert_eq!(join(&[], ","), "");
    }
}
