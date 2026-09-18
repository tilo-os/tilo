//! Spell-check span preparation, independent of widget cache and painting.

pub(super) fn misspelled_spans(
    dictionary: &spellcheck::Dictionary,
    language: &str,
    text: &str,
) -> Vec<(usize, usize)> {
    let mut spans = spellcheck::spans_for_language(language, text);
    spans.retain(|&(start, end)| {
        text.get(start..end)
            .is_some_and(|word| !dictionary.contains(word))
    });
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dictionary_words_are_removed_without_losing_byte_offsets() {
        let dictionary = spellcheck::Dictionary::from_words(["qwordz"]);
        assert_eq!(
            misspelled_spans(&dictionary, "en-US", "qwordz wrold"),
            vec![(7, 12)]
        );
    }
}
