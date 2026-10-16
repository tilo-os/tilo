//! Word-level helpers for building a record's searchable fields.

/// The first letters of a title's words, folded: `Word Processor` is `wp`,
/// so the two letters typed for it find it. `None` for a title of one word,
/// which has nothing to abbreviate.
#[must_use]
pub fn initials(title: &str) -> Option<String> {
    let initials: String = title
        .split(|scalar: char| !scalar.is_alphanumeric())
        .filter_map(|word| word.chars().next())
        .flat_map(|first| {
            crate::fold(first.encode_utf8(&mut [0; 4]))
                .chars()
                .collect::<Vec<_>>()
        })
        .collect();
    (initials.chars().count() >= 2).then_some(initials)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_abbreviate_to_their_initials() {
        assert_eq!(initials("Word Processor").as_deref(), Some("wp"));
        assert_eq!(initials("Video-Player / Öffnen").as_deref(), Some("vpo"));
        assert_eq!(initials("Tube"), None);
        assert_eq!(initials(""), None);
    }
}
