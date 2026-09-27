//! Interrupt-aware utterance queue.
//!
//! The screen reader is *chatty*: a fast Tab through a toolbar can enqueue a dozen
//! focus-narration utterances in the time it takes to speak one. Naively speaking them all
//! is unusable, so the queue exists to make it sane:
//!
//! * An **interrupting** utterance ([`Utterance::interrupt`]) clears all pending work of
//!   *equal or lower* significance, so the latest focus change wins — *but* a queued
//!   [`Priority::Important`] (error / permission prompt) survives a lower-priority interrupt.
//! * A **non-interrupting** utterance is appended; it waits its turn.
//! * The queue is engine-agnostic: it just produces an ordered list of [`Utterance`]s to hand
//!   to a [`Speaker`]. When to pull the next one belongs to the reader's run loop, not here —
//!   which keeps the queue trivially unit-testable without a daemon.
//!
//! Coalescing rule, precisely: when pushing utterance `U`:
//!   1. If `U.interrupt`, drop every *pending* utterance `P` with `P.priority <= U.priority`
//!      (equal priority drops too — the newer focus change supersedes the older one).
//!   2. Append `U`.
//!
//! [`Priority`]: crate::Priority
//! [`Priority::Important`]: crate::Priority::Important

use std::collections::VecDeque;

use crate::speaker::{Speaker, Utterance};

/// An ordered, interrupt-aware buffer of [`Utterance`]s. Holds no [`Speaker`] itself; call
/// [`SpeechQueue::drive`] (or pull manually with [`SpeechQueue::pop`]) to flush it into a
/// backend — the queue is pure data, fully testable without any TTS engine.
#[derive(Debug, Default, Clone)]
pub struct SpeechQueue {
    pending: VecDeque<Utterance>,
}

impl SpeechQueue {
    /// A fresh, empty queue.
    pub fn new() -> Self {
        SpeechQueue {
            pending: VecDeque::new(),
        }
    }

    /// Enqueue an utterance, applying the interrupt/coalesce rule above. Returns `true` if this
    /// push interrupted/cleared pending work (the caller should then `stop()` the backend to cut
    /// off in-progress speech), `false` if it was a plain append.
    pub fn push(&mut self, utterance: Utterance) -> bool {
        let interrupted = utterance.interrupt;
        if interrupted {
            // Drop pending work of equal-or-lower significance; strictly-higher (e.g. a queued
            // Important error) survives.
            self.pending.retain(|p| p.priority > utterance.priority);
        }
        self.pending.push_back(utterance);
        // An interrupting push always signals the caller to stop in-flight speech (the
        // daemon/subprocess may already be mid-utterance).
        interrupted
    }

    /// Convenience: push plain text at the default priority, interrupting.
    pub fn push_text(&mut self, text: impl Into<String>) -> bool {
        self.push(Utterance::new(text))
    }

    /// Remove and return the next utterance to speak, if any.
    pub fn pop(&mut self) -> Option<Utterance> {
        self.pending.pop_front()
    }

    /// Peek at the next utterance without removing it.
    pub fn peek(&self) -> Option<&Utterance> {
        self.pending.front()
    }

    /// Discard everything pending (the "silence key").
    pub fn clear(&mut self) {
        self.pending.clear();
    }

    /// Number of pending utterances.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether anything is pending.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Flush the entire queue into `speaker`, in order. For the subprocess/null backends
    /// (synchronous `speak`) this speaks everything; for speech-dispatcher (async daemon-side)
    /// this enqueues everything at once and the daemon serializes. The reader's run loop
    /// typically prefers [`SpeechQueue::pop`] one-at-a-time to keep interrupting, but `drive` is
    /// handy for tests and "say all" where no further interruption is expected.
    pub fn drive(&mut self, speaker: &mut dyn Speaker) {
        while let Some(u) = self.pop() {
            // The first popped utterance carries its own interrupt flag; the rest within a single
            // drive are continuations and should not re-interrupt each other.
            speaker.speak_utterance(u);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::speaker::Priority;

    #[test]
    fn the_queue_orders_interrupts_and_drains_by_priority() {
        // Phase `plain_append_preserves_order`.
        {
            let mut q = SpeechQueue::new();
            let interrupted = q.push(Utterance::new("a").with_interrupt(false));
            assert!(!interrupted);
            q.push(Utterance::new("b").with_interrupt(false));
            q.push(Utterance::new("c").with_interrupt(false));
            assert_eq!(q.len(), 3);
            assert_eq!(q.pop().unwrap().text, "a");
            assert_eq!(q.pop().unwrap().text, "b");
            assert_eq!(q.pop().unwrap().text, "c");
            assert!(q.is_empty());
        }
        // Phase `interrupt_drops_equal_and_lower_priority_pending`.
        {
            let mut q = SpeechQueue::new();
            q.push(Utterance::new("focus 1")); // Message, interrupt
            q.push(Utterance::new("focus 2")); // Message, interrupt -> drops focus 1
            assert_eq!(q.len(), 1);
            assert_eq!(q.peek().unwrap().text, "focus 2");
        }
        // Phase `interrupt_keeps_strictly_higher_priority_pending`.
        {
            let mut q = SpeechQueue::new();
            // An error is queued (Important).
            q.push(Utterance::new("error!").with_priority(Priority::Important));
            // A focus change comes in (Message, interrupting).
            let interrupted = q.push(Utterance::new("button"));
            assert!(interrupted);
            // The Important error must survive; the focus narration is appended after.
            assert_eq!(q.len(), 2);
            assert_eq!(q.pop().unwrap().text, "error!");
            assert_eq!(q.pop().unwrap().text, "button");
        }
        // Phase `lower_priority_text_dropped_by_message_interrupt`.
        {
            let mut q = SpeechQueue::new();
            q.push(
                Utterance::new("a long document being read aloud")
                    .with_priority(Priority::Text)
                    .with_interrupt(false),
            );
            // Focus change interrupts bulk reading.
            q.push(Utterance::new("Cancel button"));
            assert_eq!(q.len(), 1);
            assert_eq!(q.peek().unwrap().text, "Cancel button");
        }
        // Phase `clear_empties`.
        {
            let mut q = SpeechQueue::new();
            q.push_text("a");
            q.push_text("b");
            q.clear();
            assert!(q.is_empty());
            assert_eq!(q.pop(), None);
        }
        // Phase `drive_flushes_into_speaker_in_order`.
        {
            use crate::null::NullSpeaker;
            let mut q = SpeechQueue::new();
            q.push(Utterance::new("one").with_interrupt(false));
            q.push(Utterance::new("two").with_interrupt(false));
            let mut spk = NullSpeaker::new();
            q.drive(&mut spk);
            assert!(q.is_empty());
            assert_eq!(spk.spoken(), &["one".to_string(), "two".to_string()]);
        }
        // Phase `non_interrupting_does_not_clear`.
        {
            let mut q = SpeechQueue::new();
            q.push(Utterance::new("important focus"));
            let interrupted = q.push(Utterance::new("polite toast").with_interrupt(false));
            assert!(!interrupted);
            assert_eq!(q.len(), 2);
        }
    }
}
