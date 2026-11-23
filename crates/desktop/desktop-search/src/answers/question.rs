//! Whether what was typed asks something. A question is answered where it
//! was typed ("Ask …"), a name is looked up ("notes" finds the notes), so
//! the row that answers is offered for questions alone.

/// The words a question opens with, in the languages the desktop speaks. A
/// name, a sum or an address opens with none of them.
const QUESTION_WORDS: &[&str] = &[
    "what",
    "why",
    "how",
    "who",
    "whom",
    "whose",
    "when",
    "where",
    "which",
    "can",
    "could",
    "should",
    "would",
    "will",
    "is",
    "are",
    "was",
    "were",
    "do",
    "does",
    "did",
    "explain",
    "summarize",
    "summarise",
    "translate",
    "was",
    "wie",
    "warum",
    "weshalb",
    "wer",
    "wo",
    "wann",
    "welche",
    "welcher",
    "welches",
    "kann",
    "können",
    "ist",
    "sind",
    "hat",
    "haben",
    "erkläre",
    "erklären",
    "übersetze",
    "übersetzen",
    "fasse",
];

/// Whether `typed` asks something rather than naming it: it ends in a
/// question mark, or opens with a question word.
#[must_use]
pub fn asks(typed: &str) -> bool {
    let typed = typed.trim();
    if typed.is_empty() {
        return false;
    }
    if typed.ends_with('?') {
        return true;
    }
    let first = typed
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    QUESTION_WORDS.contains(&first.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_is_asked_and_a_name_is_not() {
        assert!(asks("what is a CRDT?"));
        assert!(asks("How do I share a note"));
        assert!(asks("warum ist der Himmel blau?"));
        assert!(asks("Erkläre mir das"));
        assert!(!asks("notes"));
        assert!(!asks("budget"));
        assert!(!asks("12*7"));
        assert!(!asks("example.com"));
        assert!(!asks("   "));
    }
}
