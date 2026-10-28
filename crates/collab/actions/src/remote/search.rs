//! What a word a caller searches with matches: the word itself and its
//! inflections.
//!
//! An agent describes what it wants to do in its own words, and an app
//! describes the same gesture in its own: "draw" against "drawing a
//! signature", "scrolling" against "Scroll down with PageDown". Reducing
//! both to a stem is what makes those the same word.

/// The stem an English word is matched by: its plural, its participle and
/// its "-ing" form reduce to the word a description most likely spells
/// ("drawing" and "draw", "dragging" and "drag", "scrolling" and "scroll",
/// "signed" and "sign"). An inflection is only undone when what is left is
/// still a word of reasonable length, so "ring" and "thing" keep their own
/// letters rather than becoming "r" and "th".
#[must_use]
pub fn stem_word(word: &str) -> String {
    let letters = word.chars().count();
    let strip = |ending: &str, shortest: usize| -> Option<String> {
        let kept = word.strip_suffix(ending)?;
        (word.chars().count() >= shortest && kept.chars().count() >= 3).then(|| kept.to_owned())
    };
    if letters <= 3 {
        return word.to_owned();
    }
    // An "-ing" and an "-ed" are where a word of a single syllable doubles
    // its last consonant ("dragging", "dragged"), so those undo the
    // doubling; a plural never does.
    let inflected = strip("ing", 4)
        .or_else(|| strip("ed", 5))
        .map(|stemmed| (stemmed, true))
        .or_else(|| strip("ies", 5).map(|stemmed| (format!("{stemmed}y"), false)))
        .or_else(|| strip("es", 5).map(|stemmed| (stemmed, false)))
        // "press" is not "pres": a word already ending in a double s is the
        // form "-ing", "-ed" and "-es" reduce to.
        .or_else(|| {
            (!word.ends_with("ss"))
                .then(|| strip("s", 4))
                .flatten()
                .map(|stemmed| (stemmed, false))
        });
    let Some((mut stemmed, doubled)) = inflected else {
        return word.to_owned();
    };
    if doubled && doubles_its_last_letter(&stemmed) {
        stemmed.pop();
    }
    stemmed
}

/// Whether a stem's last two letters are one consonant doubled, as "-ing"
/// and "-ed" leave it in "dragging" and "dragged": the "e" of "agreed", the
/// "l" of "called" and the "s" of "passed" are not doubled ones.
fn doubles_its_last_letter(stem: &str) -> bool {
    let mut doubled = stem.chars().rev().take(2);
    matches!(
        (doubled.next(), doubled.next()),
        (Some(last), Some(previous))
            if last == previous && !matches!(last, 'e' | 'l' | 's' | 'z')
    )
}

#[cfg(test)]
mod tests {
    use super::stem_word;

    #[test]
    fn a_stem_undoes_an_inflection_and_nothing_else() {
        for (word, stem) in [
            ("drawing", "draw"),
            ("dragging", "drag"),
            ("dragged", "drag"),
            ("scrolling", "scroll"),
            ("scrolled", "scroll"),
            ("scrolls", "scroll"),
            ("signing", "sign"),
            ("signed", "sign"),
            ("signs", "sign"),
            ("presses", "press"),
            ("pressing", "press"),
            ("copies", "copy"),
        ] {
            assert_eq!(stem_word(word), stem, "{word}");
        }
        // A word that is already a stem keeps its letters.
        for word in ["press", "ring", "thing", "id", "use", "scroll", "drag"] {
            assert_eq!(stem_word(word), word, "{word}");
        }
    }
}
