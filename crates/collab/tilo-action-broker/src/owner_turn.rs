//! One started owner turn, as the caller waiting on its answer sees it.
//!
//! The serialized owner runs an admitted turn to its end and then answers
//! definitely; losing the caller never cancels it. A caller must therefore
//! not turn a slow turn into an unknown outcome. The owner bounds its own
//! turn instead: it grants a stall budget when the turn starts and renews it
//! at every step it completes. Only a turn that makes no progress for a whole
//! budget is stalled, and only then is its outcome reported as unknown.
use std::time::Duration;
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

/// How long an owner turn may go without completing a step before its caller
/// treats it as stalled. Durable steps on a loaded host take seconds; this
/// bounds a hung owner, not an ordinary slow one.
pub const STALL_BUDGET: Duration = Duration::from_secs(60);

/// The owner's side of a started turn: renews the caller's deadline.
pub struct TurnProgress {
    deadline: watch::Sender<Instant>,
    budget: Duration,
}

/// The caller's side of a started turn.
pub struct TurnWatch {
    deadline: watch::Receiver<Instant>,
}

/// How a started turn ended for its caller.
#[derive(Debug, PartialEq, Eq)]
pub enum TurnWait<T> {
    /// The owner answered.
    Answered(T),
    /// The owner ended the turn without answering.
    Lost,
    /// The owner made no progress for a whole budget.
    Stalled,
}

/// Start a turn whose steps may each take up to `budget`.
#[must_use]
pub fn start(budget: Duration) -> (TurnProgress, TurnWatch) {
    let (deadline, watch) = watch::channel(after(budget));
    (
        TurnProgress { deadline, budget },
        TurnWatch { deadline: watch },
    )
}

/// `budget` from now. A budget past the clock's range can never run out.
fn after(budget: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(budget)
        .or_else(|| now.checked_add(Duration::from_secs(u64::from(u32::MAX))))
        .unwrap_or(now)
}

impl TurnProgress {
    /// A step of the turn completed: the next may take a whole budget again.
    pub fn advance(&self) {
        self.deadline.send_replace(after(self.budget));
    }
}

impl TurnWatch {
    /// Wait for the owner's answer on `reply` while the turn makes progress.
    pub async fn wait<T>(mut self, reply: oneshot::Receiver<T>) -> TurnWait<T> {
        let mut reply = std::pin::pin!(reply);
        loop {
            let deadline = *self.deadline.borrow_and_update();
            tokio::select! {
                biased;
                answer = &mut reply => {
                    return answer.map_or(TurnWait::Lost, TurnWait::Answered);
                }
                changed = self.deadline.changed() => {
                    if changed.is_err() {
                        // The owner ended the turn; its answer is sent or dropped.
                        return reply.await.map_or(TurnWait::Lost, TurnWait::Answered);
                    }
                }
                () = tokio::time::sleep_until(deadline) => {
                    if *self.deadline.borrow() <= deadline {
                        return TurnWait::Stalled;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: Duration = Duration::from_secs(5);

    #[tokio::test(start_paused = true)]
    async fn a_slow_turn_that_keeps_progressing_is_answered() {
        let (progress, watch) = start(BUDGET);
        let (reply, answer) = oneshot::channel();
        let owner = tokio::spawn(async move {
            // Ten steps, each most of a budget: far longer than one budget.
            for _ in 0..10 {
                tokio::time::sleep(BUDGET.checked_sub(Duration::from_millis(1)).unwrap()).await;
                progress.advance();
            }
            reply.send("done").unwrap();
            drop(progress);
        });
        assert_eq!(watch.wait(answer).await, TurnWait::Answered("done"));
        owner.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_without_progress_for_a_whole_budget_is_stalled() {
        let (progress, watch) = start(BUDGET);
        let (_reply, answer) = oneshot::channel::<()>();
        let began = Instant::now();
        assert_eq!(watch.wait(answer).await, TurnWait::Stalled);
        assert!(began.elapsed() >= BUDGET);
        drop(progress);
    }

    #[tokio::test(start_paused = true)]
    async fn an_ended_turn_answers_or_is_lost_never_stalled() {
        let (progress, watch) = start(BUDGET);
        let (reply, answer) = oneshot::channel::<()>();
        drop(progress);
        drop(reply);
        assert_eq!(watch.wait(answer).await, TurnWait::Lost);

        // Progress ends first; the answer that follows still arrives.
        let (progress, watch) = start(BUDGET);
        let (reply, answer) = oneshot::channel();
        drop(progress);
        let waiting = tokio::spawn(watch.wait(answer));
        tokio::time::sleep(BUDGET * 3).await;
        reply.send(7).unwrap();
        assert_eq!(waiting.await.unwrap(), TurnWait::Answered(7));
    }
}
