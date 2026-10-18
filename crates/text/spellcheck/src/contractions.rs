//! English contractions typed without their apostrophe.

/// Each contraction's bare form, and the contraction. Word lists hold no
/// apostrophes, so a corrector resolves these before any edit-distance
/// search, which would pick a spelling neighbour ("dont" -> "done"). Bare
/// forms that are words in their own right ("ill", "its", "well", "were",
/// "shell") are absent; "cant" and "wont" are corrected by
/// [`crate::sure_correction`], since a word list holds them. Sorted by bare form, which
/// [`contraction`] searches by halves.
const CONTRACTIONS: &[(&str, &str)] = &[
    ("arent", "aren't"),
    ("couldnt", "couldn't"),
    ("didnt", "didn't"),
    ("doesnt", "doesn't"),
    ("dont", "don't"),
    ("hasnt", "hasn't"),
    ("havent", "haven't"),
    ("im", "I'm"),
    ("isnt", "isn't"),
    ("ive", "I've"),
    ("shes", "she's"),
    ("shouldnt", "shouldn't"),
    ("thats", "that's"),
    ("theyre", "they're"),
    ("theyve", "they've"),
    ("wasnt", "wasn't"),
    ("werent", "weren't"),
    ("weve", "we've"),
    ("whats", "what's"),
    ("wouldnt", "wouldn't"),
    ("youre", "you're"),
    ("youve", "you've"),
];

/// The English contraction `bare` (lowercase, no apostrophe) stands for:
/// `"doesnt"` gives `"doesn't"`, `"im"` gives `"I'm"`.
#[must_use]
pub fn contraction(bare: &str) -> Option<&'static str> {
    // A search by halves stays one small loop; a scan of the table is
    // unrolled into a comparison per entry.
    CONTRACTIONS
        .binary_search_by(|(typed, _)| (*typed).cmp(bare))
        .ok()
        .and_then(|at| CONTRACTIONS.get(at))
        .map(|&(_, full)| full)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_forms_resolve_and_real_words_do_not() {
        assert_eq!(contraction("doesnt"), Some("doesn't"));
        assert_eq!(contraction("im"), Some("I'm"));
        for word in [
            "cant", "wont", "ill", "its", "well", "were", "shell", "done",
        ] {
            assert_eq!(contraction(word), None, "{word}");
        }
    }

    #[test]
    fn every_contraction_is_found() {
        assert!(
            CONTRACTIONS
                .windows(2)
                .all(|pair| matches!(pair, [a, b] if a.0 < b.0)),
            "the table is sorted by bare form, without repeats"
        );
        for (bare, full) in CONTRACTIONS {
            assert_eq!(contraction(bare), Some(*full), "{bare}");
        }
    }

    #[test]
    fn no_bare_form_is_an_english_word() {
        let english = crate::Dictionary::builtin();
        for (bare, _) in CONTRACTIONS {
            assert!(!english.contains(bare), "{bare} is a word of its own");
        }
    }
}
