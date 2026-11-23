//! The spans of a line a search matched, found by the same rules a highlight uses.

/// Where `line` has one of `words`: at the start of a word of it, compared letter by letter
/// whatever their case.
pub(super) fn found_spans(line: &str, words: &[&str]) -> Vec<std::ops::Range<usize>> {
    let mut spans = Vec::new();
    let mut previous: Option<char> = None;
    for (at, c) in line.char_indices() {
        let starts_word = c.is_alphanumeric() && !previous.is_some_and(char::is_alphanumeric);
        previous = Some(c);
        if !starts_word {
            continue;
        }
        let Some(rest) = line.get(at..) else {
            continue;
        };
        let longest = words
            .iter()
            .filter_map(|word| matched_len(rest, word))
            .max();
        if let Some(len) = longest
            && spans
                .last()
                .is_none_or(|last: &std::ops::Range<usize>| last.end <= at)
        {
            spans.push(at..at + len);
        }
    }
    spans
}

fn matched_len(text: &str, word: &str) -> Option<usize> {
    if word.is_empty() {
        return None;
    }
    let mut wanted = word.chars().flat_map(char::to_lowercase).peekable();
    let mut len = 0;
    for c in text.chars() {
        if wanted.peek().is_none() {
            break;
        }
        for lower in c.to_lowercase() {
            if wanted.next() != Some(lower) {
                return None;
            }
        }
        len += c.len_utf8();
    }
    wanted.peek().is_none().then_some(len)
}
