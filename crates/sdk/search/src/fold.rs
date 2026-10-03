//! Text folding shared by indexing, queries and anything that compares a
//! query with displayed text.
//!
//! Folding is case-insensitive, compatibility-folded and
//! diacritic-insensitive for alphabetic scripts. Only marks from the
//! combining-diacritics blocks are dropped. Marks that carry meaning
//! elsewhere (kana voicing, Indic vowel signs, Hebrew and Arabic points) are
//! kept, and Hangul syllables are not split into jamo.

const fn is_folded_diacritic(scalar: char) -> bool {
    matches!(
        scalar,
        '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE20}'..='\u{FE2F}'
    )
}

const fn keeps_composition(scalar: char) -> bool {
    matches!(
        scalar,
        // Hangul syllables, and kana whose voicing marks distinguish words.
        '\u{AC00}'..='\u{D7A3}' | '\u{3040}'..='\u{30FF}'
    )
}

/// Append the folded form of `scalar` (zero or more scalars) to `out`.
pub fn fold_char(scalar: char, out: &mut impl FnMut(char)) {
    if scalar.is_ascii() {
        out(scalar.to_ascii_lowercase());
        return;
    }
    for lower in scalar.to_lowercase() {
        // Ligatures and typographic quotes that have no compatibility
        // decomposition: "coeur" finds "cœur", "strasse" finds "Straße".
        let spelled = match lower {
            'œ' => "oe",
            'æ' => "ae",
            'ß' => "ss",
            '\u{2018}' | '\u{2019}' => "'",
            _ => "",
        };
        if !spelled.is_empty() {
            spelled.chars().for_each(&mut *out);
            continue;
        }
        if keeps_composition(lower) {
            out(lower);
            continue;
        }
        unicode_normalization::char::decompose_compatible(lower, |part| {
            if is_folded_diacritic(part) {
                return;
            }
            // Compatibility forms can decompose to capitals (`ℌ`), so
            // lowercase once more after decomposition.
            for folded in part.to_lowercase() {
                out(folded);
            }
        });
    }
}

/// Fold complete text.
#[must_use]
pub fn fold(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for scalar in text.chars() {
        fold_char(scalar, &mut |folded_scalar| folded.push(folded_scalar));
    }
    folded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_diacritics_and_compatibility_forms() {
        assert_eq!(fold("Crème Brûlée"), "creme brulee");
        assert_eq!(fold("ÅNGSTRÖM"), "angstrom");
        assert_eq!(fold("Ｔｏｋｙｏ ﬁle"), "tokyo file");
        assert_eq!(fold("Ἀθῆναι"), "αθηναι");
        assert_eq!(
            fold("Cœur Straße Æsir Côte d’Ivoire"),
            "coeur strasse aesir cote d'ivoire"
        );
        // Canonically equivalent spellings fold alike.
        assert_eq!(fold("e\u{301}"), fold("é"));
    }

    #[test]
    fn keeps_marks_that_distinguish_words() {
        assert_eq!(fold("がか"), "がか");
        assert_eq!(fold("한국어"), "한국어");
        assert_eq!(fold("किताब"), "किताब");
    }
}
