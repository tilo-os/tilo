//! Finding text in the source.
//!
//! The window's search finds in the document the window shows; a Typst file is
//! text, so its matches are byte ranges found here, under the options every
//! find in the system offers: match case and whole word. An empty query finds
//! nothing rather than everything, and a regular expression is not one of this
//! view's abilities — the bar does not offer it (`Findable` reports none), so
//! a pattern is read as the text it spells.

use ui::widget::{FindMatch, FindOptions};

/// Every match of `query` in `text`, in reading order.
pub(crate) fn in_text(text: &str, query: &str, options: FindOptions) -> Vec<FindMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let found = plain(text, query, options.match_case);
    if options.whole_word {
        found
            .into_iter()
            .filter(|found| {
                let before = text[..found.start].chars().next_back();
                let after = text[found.end..].chars().next();
                !before.is_some_and(is_word) && !after.is_some_and(is_word)
            })
            .collect()
    } else {
        found
    }
}

/// Whether `character` is part of a word, for whole-word matching.
fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

/// Matches of `query` read literally, honouring `match_case`.
fn plain(text: &str, query: &str, match_case: bool) -> Vec<FindMatch> {
    let (haystack, needle) = if match_case {
        (text.to_owned(), query.to_owned())
    } else {
        (text.to_lowercase(), query.to_lowercase())
    };
    // `to_lowercase` can change a string's length, so offsets found in the
    // lowered copy are only trusted when the lowering kept it: a source whose
    // case folding changes its length is matched as written instead.
    if haystack.len() != text.len() || needle.len() != query.len() {
        return text
            .match_indices(query)
            .map(|(at, found)| FindMatch::new(at, at + found.len()))
            .collect();
    }
    haystack
        .match_indices(&needle)
        .map(|(at, found)| FindMatch::new(at, at + found.len()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_reads_the_source_as_written() {
        let source = "= Title\n\nBody text here.\nText again.";
        let all = in_text(source, "text", FindOptions::default());
        assert_eq!(all.len(), 2, "both spellings under the first are found");
        let cased = in_text(
            source,
            "Text",
            FindOptions {
                match_case: true,
                ..FindOptions::default()
            },
        );
        assert_eq!(cased.len(), 1, "match case finds the capitalised one");
        assert_eq!(&source[cased[0].start..cased[0].end], "Text");
        assert!(in_text(source, "", FindOptions::default()).is_empty());
        let words = in_text(
            source,
            "here",
            FindOptions {
                whole_word: true,
                ..FindOptions::default()
            },
        );
        assert_eq!(words.len(), 1, "a whole word matches where it stands alone");
    }

    #[test]
    fn a_layout_whose_case_folding_changes_its_length_is_matched_as_written() {
        // Turkish dotless shapes and the German sharp s lower-case into a
        // different number of bytes; the offsets then come from the source
        // itself, so no match points at the wrong text.
        let source = "Straße IST gross";
        let found = in_text(source, "ist", FindOptions::default());
        assert_eq!(found.len(), 1);
        assert_eq!(&source[found[0].start..found[0].end], "IST");
    }
}
