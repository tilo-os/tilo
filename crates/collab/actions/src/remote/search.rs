//! What a word a caller searches with matches: the word itself and its
//! inflections, and one resource a search found.
//!
//! An agent describes what it wants to do in its own words, and an app
//! describes the same gesture in its own: "draw" against "drawing a
//! signature", "scrolling" against "Scroll down with `PageDown`". Reducing
//! both to a stem is what makes those the same word.
//!
//! [`ResourceSearch`] is the one search the desktop runs — the palette, text
//! typed on the canvas and an agent's `find` — reached from a host that
//! holds it. Its results are the ranking's own, best first, each naming the
//! resource a caller addresses.

use super::ActionHostError;

/// One resource a search found, as every caller of the one search reads a
/// result: the resource it is about, what it is, and what it reads as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The resource's published id: what a caller addresses it by.
    pub resource: String,
    /// Its kind (`tl.so.tilo.lang.notes.Notebook`), as its app publishes it.
    pub kind: String,
    /// Its label, as the person reads it.
    pub label: String,
    /// The action a caller runs to open it, when the result is one a caller
    /// opens (`open`); `None` for a result that is not a resource of an app
    /// (a setting, a file, a command) or one reached another way.
    pub open: Option<String>,
    /// The arguments that open it, by parameter name.
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The app publishing it, when one does: what a caller names it as.
    pub app: Option<String>,
}

/// The desktop's one search, as a host that holds it offers it to a caller
/// who asks by words: the palette's ranking, the same results in the same
/// order. The search itself lives on the thread that holds the desktop, so
/// an answer comes back later, as an action's does.
pub trait ResourceSearch: Send + Sync {
    /// What `needle` finds, best first, over the workspace `caller` acts in
    /// and what it may read there.
    ///
    /// # Errors
    /// The search could not be run: the host has none, or it refuses
    /// `caller`.
    fn find(
        &self,
        caller: &super::Caller,
        needle: &str,
    ) -> super::Pending<Result<Vec<Found>, ActionHostError>>;
}

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
