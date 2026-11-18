//! What the server's log keeps of a tool's refusal.
//!
//! A refusal has two parts: its sentence, and what it gives back to its caller so that the next
//! call is right: the call that works, the arguments to ask again with, what was wrong with
//! what the caller wrote. The second part repeats the caller's own words, and a place, a
//! person's name or a question's words stand in no log. So the log keeps the sentence only.

use super::LOGGED_CHARS;

/// What the log keeps of the refusal `said`: its first sentence, which ends at the first line
/// break or at the first full stop that another sentence follows, and at most
/// [`LOGGED_CHARS`] of it.
pub(super) fn refusal(said: &str) -> String {
    let line = said.lines().next().unwrap_or_default();
    let sentence = line.split_once(". ").map_or(line, |(first, _)| first);
    sentence.chars().take(LOGGED_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::{LOGGED_CHARS, refusal};

    #[test]
    fn the_log_keeps_a_refusals_sentence_and_nothing_its_caller_wrote() {
        // As Chat refuses a question for a secret: its sentence, then what was wrong with what
        // the caller wrote, then the arguments to ask again with.
        let asked = "failed: action_failed: a question for a secret says where the secret \
            belongs. In your detail: a place is written with what it is reached by, and yours \
            is `mail.plum.example`. Ask again with exactly these arguments: \
            {\"text\":\"The password of ada@plum.example\",\"kind\":\"secret\"}";
        assert_eq!(
            refusal(asked),
            "failed: action_failed: a question for a secret says where the secret belongs"
        );
        // As an agent tool refuses: its sentence, then the call that works on a line of its own.
        let named = "notes/Plums has no action rename/title; close matches: rename\n\
            The call that works: tilo_do {\"path\":\"notes/Plums#rename\",\"args\":{\"title\":\"For Ada\"}}";
        assert_eq!(
            refusal(named),
            "notes/Plums has no action rename/title; close matches: rename"
        );
        // A long sentence is kept to the log's bound.
        assert_eq!(refusal(&"a".repeat(1000)).chars().count(), LOGGED_CHARS);
    }
}
