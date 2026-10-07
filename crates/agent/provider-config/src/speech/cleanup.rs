//! Conservative final-text edits. Never an instruction-following rewrite.
use std::borrow::Cow;

/// Remove only explicit, comma-delimited English numeric self-repairs.
/// Literal mode, other selected languages, quotations and oversized input are
/// returned byte-for-byte. The replacement's spelling is never normalised.
/// This deliberately does not guess grammar, remove isolated hesitation words,
/// rewrite proper names or infer an unspoken numeric correction.
pub fn clean_final<'a>(text: &'a str, language: &str, literal: bool) -> Cow<'a, str> {
    if literal
        || !matches!(language, "en" | "auto")
        || text.len() > super::MAX_TEXT_BYTES
        || text.contains('\u{0027}')
        || text.contains(['"', '“', '”', '‘', '’'])
    {
        return Cow::Borrowed(text);
    }
    let mut tokens = Vec::new();
    let mut offset = 0;
    for token in text.split_whitespace() {
        let Some(relative) = text.get(offset..).and_then(|tail| tail.find(token)) else {
            return Cow::Borrowed(text);
        };
        let start = offset.saturating_add(relative);
        tokens.push((start, token));
        offset = start.saturating_add(token.len());
    }
    let mut output = String::new();
    let mut copied = 0;
    let mut index = 0;
    while let Some(&(start, token)) = tokens.get(index) {
        if !token.strip_suffix(',').is_some_and(number) {
            index = index.saturating_add(1);
            continue;
        }
        // Uppercase number-like proper names and padded digits are excluded.
        let mut next = index.saturating_add(1);
        if tokens
            .get(next)
            .is_some_and(|(_, word)| matches!(*word, "uh," | "um,"))
        {
            next = next.saturating_add(1);
        }
        if tokens.get(next).is_none_or(|(_, word)| *word != "no,") {
            index = index.saturating_add(1);
            continue;
        }
        next = next.saturating_add(1);
        let Some(&(replacement, word)) = tokens.get(next) else {
            break;
        };
        if !number(word.trim_end_matches(['.', ',', '!', '?', ';', ':'])) {
            index = index.saturating_add(1);
            continue;
        }
        let Some(prefix) = text.get(copied..start) else {
            return Cow::Borrowed(text);
        };
        output.push_str(prefix);
        copied = replacement;
        index = next.saturating_add(1);
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    if let Some(tail) = text.get(copied..) {
        output.push_str(tail);
    }
    Cow::Owned(output)
}

fn number(word: &str) -> bool {
    matches!(
        word,
        "zero"
            | "one"
            | "two"
            | "three"
            | "four"
            | "five"
            | "six"
            | "seven"
            | "eight"
            | "nine"
            | "ten"
            | "eleven"
            | "twelve"
            | "thirteen"
            | "fourteen"
            | "fifteen"
            | "sixteen"
            | "seventeen"
            | "eighteen"
            | "nineteen"
            | "twenty"
    ) || (!word.is_empty()
        && word.len() <= 6
        && !word.starts_with('0')
        && word.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_numeric_repair_keeps_replacement_and_surrounding_text() {
        for (raw, expected) in [
            (
                "Let me grab two, uh, no, three of these",
                "Let me grab three of these",
            ),
            ("Take 2, um, no, 3.", "Take 3."),
            (
                "Take two, no, three; then four, uh, no, five.",
                "Take three; then five.",
            ),
            ("Tilo needs two, uh, no, three.", "Tilo needs three."),
        ] {
            assert_eq!(clean_final(raw, "en", false), expected);
            assert_eq!(clean_final(raw, "auto", false), expected);
            assert_eq!(clean_final(raw, "en", true), raw);
        }
    }

    #[test]
    fn uncertain_disfluency_names_quotes_languages_and_numbers_stay_literal() {
        for raw in [
            "Let me, uh, grab three of these",
            "Say 'Let me grab two, uh, no, three of these' exactly",
            "Two, uh, no, Three knows Tilo",
            "Say \"two, uh, no, three\" exactly",
            "two, no, threefold",
            "two, no, 03",
            "two, no three",
            "two uh no three",
            "two, no, 3.5",
            "two, no, three-legged",
            "three and two",
            "no, three",
            "two, no, Tilo",
        ] {
            assert_eq!(clean_final(raw, "en", false), raw);
        }
        for language in ["fr", "de", "es", "ar", "zh"] {
            let raw = "two, uh, no, three";
            assert_eq!(clean_final(raw, language, false), raw);
        }
    }
}
