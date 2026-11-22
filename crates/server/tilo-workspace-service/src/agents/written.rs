//! A reply grows while its model writes it.
//!
//! A turn made from an ask answers with the ask's reply ([`super::asked`]), and its model writes
//! that answer over seconds. The words go onto the reply as they arrive, through the agent
//! package's own `say`, which puts them at the end of the reply's body as one change of the
//! record: the person reads along on every device. Each piece goes the moment it comes, and no
//! clock holds a word back. What a model writes while a piece is on its way goes with the next
//! one, so a reply takes no more changes than its server can write.
//!
//! What a model thinks before it writes, where its provider shows that, goes into the reply's
//! thinking the same way, through the package's `think`, and never among its words.
//!
//! What a model writes before it uses a tool stays where its person read it, and what it
//! writes afterwards stands a paragraph below. A reply whose turn ends before its answer keeps
//! the words that came and says below them why ([`Written::ending`]). The reply of a model
//! whose answers arrive whole is written once, at its turn's end, and so is every answer that
//! is no ask's reply: an issue's comment.

use std::sync::Mutex;

use crate::asks::AskState;
use tilo_lock::MutexRecover as _;

use super::{Ask, Outcome, Reply, Turn};

/// What stands between what a model wrote before a tool, or before its turn ended early, and
/// what follows it: a paragraph's break.
const BELOW: &str = "\n\n";

/// What a model wrote that its reply does not hold yet. The thread the model is asked on
/// puts the words here as they arrive, and whoever follows the answer takes them.
#[derive(Default)]
pub(super) struct Writes {
    words: Mutex<String>,
    /// What the model thought, as its provider shows it, that the reply does not hold yet.
    thoughts: Mutex<String>,
    more: tokio::sync::Notify,
}

impl Writes {
    /// The model wrote `text`.
    pub(super) fn push(&self, text: &str) {
        self.words.lock_recover().push_str(text);
        self.more.notify_one();
    }

    /// The model thought `text`, and its provider shows it.
    pub(super) fn thought(&self, text: &str) {
        self.thoughts.lock_recover().push_str(text);
        self.more.notify_one();
    }

    /// What it thought since that was last taken.
    fn take_thoughts(&self) -> String {
        std::mem::take(&mut *self.thoughts.lock_recover())
    }

    /// The model's call broke off. Where it is asked again, what it writes then stands
    /// below what came; where it is not, the break is the end and adds nothing.
    pub(super) fn broke_off(&self) {
        self.push(BELOW);
    }

    /// What came since it was last taken.
    fn take(&self) -> String {
        std::mem::take(&mut *self.words.lock_recover())
    }
}

/// How far a turn's reply has grown.
pub(super) struct Written {
    /// Whether the reply grows: only an ask's does.
    grows: bool,
    grown: Mutex<Grown>,
}

#[derive(Default)]
struct Grown {
    /// Whether the reply holds words of this turn's.
    said: bool,
    /// What the call to the model that is under way put onto the reply.
    call: String,
    /// Whether what goes on next stands a paragraph below what the reply holds.
    below: bool,
    /// Whether the reply refused a piece: no more goes on, and the turn's end says the rest.
    lost: bool,
    /// What thinking the reply holds.
    thought: Thought,
}

/// What thinking a reply holds.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Thought {
    /// None yet.
    #[default]
    None,
    /// An earlier call's: what is thought now stands a paragraph below it.
    Earlier,
    /// The call under way's.
    Now,
}

impl Written {
    /// How the reply of `turn` grows: only an ask's does.
    pub(super) fn of(turn: &Turn) -> Self {
        Self {
            grows: matches!(turn.ask, Ask::Asked { .. }),
            grown: Mutex::default(),
        }
    }

    /// The reply holds words an earlier run of this turn wrote: what this one writes stands
    /// below them.
    pub(super) fn goes_on(&self) {
        self.grown.lock_recover().said = true;
    }

    /// The model is asked anew: what it writes now stands below what came.
    fn asked_anew(&self) {
        let mut grown = self.grown.lock_recover();
        grown.call.clear();
        grown.below = grown.said;
        if grown.thought == Thought::Now {
            grown.thought = Thought::Earlier;
        }
    }

    /// What goes into the reply's thinking for `more`, which the model thought just now: a
    /// later call's thinking stands a paragraph below an earlier one's.
    fn next_thought(&self, more: &str) -> Option<String> {
        let (lost, below) = {
            let grown = self.grown.lock_recover();
            (grown.lost, grown.thought == Thought::Earlier)
        };
        if !self.grows || lost || more.is_empty() {
            return None;
        }
        Some(if below {
            format!("{BELOW}{more}")
        } else {
            more.to_owned()
        })
    }

    /// The reply took thinking.
    fn thought_taken(&self) {
        self.grown.lock_recover().thought = Thought::Now;
    }

    /// What goes onto the reply for `more`, which the model wrote just now: none where the
    /// reply does not grow, or lost a piece.
    fn next(&self, more: &str) -> Option<String> {
        let (lost, below) = {
            let grown = self.grown.lock_recover();
            (grown.lost, grown.below)
        };
        if !self.grows || lost || more.is_empty() {
            return None;
        }
        Some(if below {
            format!("{BELOW}{more}")
        } else {
            more.to_owned()
        })
    }

    /// The reply took `more`.
    fn took(&self, more: &str) {
        let mut grown = self.grown.lock_recover();
        grown.said = true;
        grown.below = false;
        grown.call.push_str(more);
    }

    /// The reply refused a piece.
    fn lost(&self) {
        self.grown.lock_recover().lost = true;
    }

    /// What of `whole`, the words of the answer to the call that just ended, the reply lacks:
    /// what its provider said only at the end.
    fn lacks(&self, whole: &str) -> Option<String> {
        let (lost, call) = {
            let grown = self.grown.lock_recover();
            (grown.lost, grown.call.clone())
        };
        if !self.grows || lost {
            return None;
        }
        let rest = whole.trim_end().strip_prefix(call.as_str())?;
        (!rest.is_empty()).then(|| rest.to_owned())
    }

    /// `words`, which a turn says last, as they go onto its reply: below what it holds.
    pub(super) fn below(&self, words: &str) -> String {
        let words = words.trim();
        if self.grown.lock_recover().said && !words.is_empty() {
            format!("{BELOW}{words}")
        } else {
            words.to_owned()
        }
    }

    /// What goes onto the reply as its turn ends as `outcome`, and how the ask then stands. An
    /// answer that grew holds its words already; one that came whole goes on now, and a turn
    /// that ended before its answer says why below the words that came. A reply that lost a
    /// piece is given the whole answer below what it holds, so no word of it is gone.
    pub(super) fn ending(&self, outcome: &Outcome) -> (String, AskState) {
        let (said, lost) = {
            let grown = self.grown.lock_recover();
            (grown.said, grown.lost)
        };
        match outcome {
            Outcome::Answered(_) if said && !lost => (String::new(), AskState::Done),
            Outcome::Answered(words) => (self.below(words), AskState::Done),
            Outcome::Stopped(words) | Outcome::Failed(words) => {
                (self.below(words), AskState::Failed)
            }
            Outcome::Cancelled => (String::new(), AskState::Stopped),
        }
    }
}

impl Reply {
    /// Put `more`, which the turn's model thought just now and its provider showed, at the end
    /// of the reply's thinking: beside its words, never among them.
    pub(super) async fn thought(&self, more: &str) {
        let (Some(replying), Some(text)) = (&self.replying, self.written.next_thought(more)) else {
            return;
        };
        match replying.think(&text).await {
            Ok(()) => self.written.thought_taken(),
            Err(error) => {
                tracing::warn!(workspace = %self.workspace, %error, "a reply did not take what its model thought");
                self.written.lost();
            }
        }
    }

    /// Put `more`, which the turn's model wrote just now, at the end of the answer.
    pub(super) async fn wrote(&self, more: &str) {
        let (Some(replying), Some(text)) = (&self.replying, self.written.next(more)) else {
            return;
        };
        match replying.say(&text).await {
            Ok(()) => self.written.took(more),
            Err(error) => {
                tracing::warn!(workspace = %self.workspace, %error, "a reply did not take what its model wrote");
                self.written.lost();
            }
        }
    }
}

/// What takes a model's words while it writes them: a turn's reply.
pub(super) trait Takes {
    /// The model is asked anew: what it writes now stands below what came.
    fn asked_anew(&self);

    /// Put `more`, which the model wrote just now, at the end.
    fn take(&self, more: &str) -> impl std::future::Future<Output = ()> + Send;

    /// Put `more`, which the model thought just now, at the end of its thinking.
    fn think(&self, more: &str) -> impl std::future::Future<Output = ()> + Send;

    /// What of `whole`, the words of the answer to the call that just ended, it lacks.
    fn lacks(&self, whole: &str) -> Option<String>;
}

impl Takes for Reply {
    fn asked_anew(&self) {
        self.written.asked_anew();
    }

    async fn take(&self, more: &str) {
        self.wrote(more).await;
    }

    async fn think(&self, more: &str) {
        self.thought(more).await;
    }

    fn lacks(&self, whole: &str) -> Option<String> {
        self.written.lacks(whole)
    }
}

/// `words` with the blanks before each paragraph's break left out: where a call broke off
/// after a blank, the break alone stands between what came and what follows.
fn settled(words: &str) -> String {
    let mut pieces: Vec<&str> = words.split(BELOW).collect();
    let last = pieces.pop().unwrap_or_default();
    let mut settled = String::with_capacity(words.len());
    for piece in pieces {
        settled.push_str(piece.trim_end());
        settled.push_str(BELOW);
    }
    settled.push_str(last);
    settled
}

/// Wait for `answer`, a model's answer on its way, and put what the model writes meanwhile
/// (`writes`) into `reply`, each piece the moment it comes: what arrives while a piece is on
/// its way goes with the next. Words go in whole: a blank a model wrote last waits for the
/// word after it, and is left out at the answer's end. What of the answer's words the provider
/// said only at the end is put in when the answer is there ([`Takes::lacks`]).
pub(super) async fn follow<R>(
    reply: &(impl Takes + Sync),
    answer: impl std::future::Future<Output = R>,
    writes: &Writes,
) -> R {
    let mut answer = std::pin::pin!(answer);
    reply.asked_anew();
    let mut words = String::new();
    let mut thoughts = String::new();
    let answer = loop {
        tokio::select! {
            answer = &mut answer => break answer,
            () = writes.more.notified() => {
                // What the model thought stands in the reply before what it then wrote, and
                // goes the moment it comes, as its words do.
                thoughts.push_str(&writes.take_thoughts());
                if !thoughts.trim_end().is_empty() {
                    let rest = thoughts.split_off(thoughts.trim_end().len());
                    reply.think(&thoughts).await;
                    thoughts = rest;
                }
                words.push_str(&writes.take());
                words = settled(&words);
                // No clock holds a word back: it goes now. What the model writes while it
                // is on its way waits for nothing but that, and goes next.
                if !words.trim_end().is_empty() {
                    let rest = words.split_off(words.trim_end().len());
                    reply.take(&words).await;
                    words = rest;
                }
            }
        }
    };
    thoughts.push_str(&writes.take_thoughts());
    reply.think(thoughts.trim_end()).await;
    words.push_str(&writes.take());
    reply.take(settled(&words).trim_end()).await;
    answer
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    /// Keeps each piece it is given with the millisecond it came, on the test's own clock.
    /// Taking a piece costs it `costs`, as a write into a record costs a server.
    struct Kept {
        began: tokio::time::Instant,
        costs: Duration,
        pieces: Mutex<Vec<(u128, String)>>,
    }

    impl Kept {
        fn new(costs: Duration) -> Self {
            Self {
                began: tokio::time::Instant::now(),
                costs,
                pieces: Mutex::default(),
            }
        }

        /// What it was given, each piece with the millisecond it came.
        fn pieces(&self) -> Vec<(u128, String)> {
            self.pieces.lock_recover().clone()
        }
    }

    impl Takes for Kept {
        fn asked_anew(&self) {}

        async fn take(&self, more: &str) {
            if more.is_empty() {
                return;
            }
            self.pieces
                .lock_recover()
                .push((self.began.elapsed().as_millis(), more.to_owned()));
            tokio::time::sleep(self.costs).await;
        }

        async fn think(&self, _: &str) {}

        fn lacks(&self, _: &str) -> Option<String> {
            None
        }
    }

    /// A model that writes each of `pieces` after its wait in milliseconds, and answers a
    /// tenth of a second after the last.
    fn writing(
        writes: &Arc<Writes>,
        pieces: &'static [(u64, &'static str)],
    ) -> tokio::task::JoinHandle<Result<(), ()>> {
        let writes = Arc::clone(writes);
        tokio::spawn(async move {
            for (after, piece) in pieces {
                tokio::time::sleep(Duration::from_millis(*after)).await;
                writes.push(piece);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok(())
        })
    }

    /// A word that arrives alone stands in the reply the millisecond it came: no clock holds
    /// it back until more has come.
    #[tokio::test(start_paused = true)]
    async fn a_word_that_arrives_alone_is_written_when_it_arrives() {
        let writes = Arc::new(Writes::default());
        let kept = Kept::new(Duration::from_millis(2));
        let model = writing(
            &writes,
            &[
                (0, "The "),
                (10, "launch "),
                (10, "is "),
                (150, "planned "),
                (10, "for May."),
            ],
        );
        let answered = follow(&kept, model, &writes).await;
        assert!(matches!(answered, Ok(Ok(()))), "the model answered");
        assert_eq!(
            kept.pieces(),
            [
                (0, "The"),
                (10, " launch"),
                (20, " is"),
                (170, " planned"),
                (180, " for May.")
            ]
            .map(|(at, said)| (at, said.to_owned())),
            "each word stands in the reply the millisecond it came"
        );
    }

    /// What a model writes while a piece is on its way goes with the next one: a reply takes
    /// no more changes than its server can write, and waits for nothing else.
    #[tokio::test(start_paused = true)]
    async fn what_arrives_while_a_piece_is_on_its_way_goes_with_the_next() {
        let writes = Arc::new(Writes::default());
        let kept = Kept::new(Duration::from_millis(30));
        let model = writing(
            &writes,
            &[(0, "One "), (5, "two "), (5, "three "), (5, "four.")],
        );
        let answered = follow(&kept, model, &writes).await;
        assert!(matches!(answered, Ok(Ok(()))), "the model answered");
        assert_eq!(
            kept.pieces(),
            [(0, "One"), (30, " two three four.")].map(|(at, said)| (at, said.to_owned())),
            "the pieces that came during the first write go together, when it has ended"
        );
    }

    /// A reply that grew holds its answer: its turn's end adds nothing to it, and an end
    /// before the answer says why a paragraph below. One that came whole goes on at the end.
    #[test]
    fn a_reply_that_grew_ends_below_its_words() {
        let written = Written {
            grows: true,
            grown: Mutex::default(),
        };
        assert_eq!(
            written.ending(&Outcome::Answered("Whole.".to_owned())),
            ("Whole.".to_owned(), AskState::Done),
            "nothing came while it was written"
        );
        written.asked_anew();
        let next = written.next("The plan").unwrap();
        written.took(&next);
        assert_eq!(
            written.lacks("The plan is ready."),
            Some(" is ready.".to_owned())
        );
        assert_eq!(
            written.ending(&Outcome::Answered("The plan".to_owned())),
            (String::new(), AskState::Done)
        );
        written.asked_anew();
        assert_eq!(written.next("Then").unwrap(), "\n\nThen", "after a tool");
        assert_eq!(
            written.ending(&Outcome::Failed("The model stopped.".to_owned())),
            ("\n\nThe model stopped.".to_owned(), AskState::Failed)
        );
        written.lost();
        assert_eq!(
            written.next("more"),
            None,
            "a reply that lost a piece takes no more"
        );
        assert_eq!(
            written
                .ending(&Outcome::Answered("The plan is ready.".to_owned()))
                .0,
            "\n\nThe plan is ready.",
            "it is given the whole answer below what it holds"
        );
    }
}
