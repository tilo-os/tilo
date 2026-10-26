//! Agent runs of issues the broker saw end, until the session's history
//! recorder takes them.
//!
//! An agent running an issue stamps every change it makes with the run's
//! group (kind `task`, [`runtime_collab::task_run_group`]). Each
//! application's history can already undo those changes by naming the
//! group; what is missing without this is the record above them: one run,
//! attributed to the agent, that Activity lists and undoes as a unit. The
//! broker knows when a run ends and who it worked for, so it hands that over
//! here; the session finds the changes in the documents and records the run.

use std::collections::VecDeque;
use std::sync::Mutex;

/// The most ended runs kept for the recorder: it takes them every frame, so
/// only a stalled session ever reaches it, and then the oldest go first.
const MAX_ENDED_RUNS: usize = 64;

/// An agent's run of an issue that ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndedTaskRun {
    pub room: String,
    /// The group id the run's changes are stamped with.
    pub group: String,
    /// The issue's title, which history shows the run under.
    pub title: String,
    /// The agent that worked it, as its changes name it (`agent:<id>`).
    pub agent: String,
    /// Policy id of the person the run works for.
    pub person: String,
    /// What the agent was asked, when the issue still says.
    pub instruction: Option<String>,
    /// How the run ended, in a few words.
    pub outcome: String,
}

/// What the broker holds of a run while it lives and once it ends: a
/// shortcut run going (and the steps waiting on its person) is stopped
/// through here, and both kinds of run wait here until history takes them.
#[derive(Default)]
pub struct EndedRuns {
    pub shortcuts: crate::automation::ShortcutRuns,
    pub tasks: TaskRuns,
    pub running: crate::automation::RunLedger,
}

/// Ended task runs, until the history recorder takes them.
#[derive(Default)]
pub struct TaskRuns {
    ended: Mutex<VecDeque<EndedTaskRun>>,
}

impl TaskRuns {
    pub fn push(&self, run: EndedTaskRun) {
        if let Ok(mut ended) = self.ended.lock() {
            // A run reported twice (a retried report) is recorded once.
            ended.retain(|queued| queued.room != run.room || queued.group != run.group);
            ended.push_back(run);
            while ended.len() > MAX_ENDED_RUNS {
                ended.pop_front();
            }
        }
    }

    /// Whether an ended run is waiting to be taken.
    pub fn has_ended(&self) -> bool {
        self.ended.lock().is_ok_and(|ended| !ended.is_empty())
    }

    /// Every ended run not taken yet, oldest first.
    pub fn take(&self) -> Vec<EndedTaskRun> {
        self.ended
            .lock()
            .map(|mut ended| ended.drain(..).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended(group: &str, outcome: &str) -> EndedTaskRun {
        EndedTaskRun {
            room: "room".into(),
            group: group.into(),
            title: "Tidy".into(),
            agent: "agent:scribe".into(),
            person: "person:ada".into(),
            instruction: None,
            outcome: outcome.into(),
        }
    }

    #[test]
    fn a_run_reported_again_is_taken_once_with_its_last_outcome() {
        let runs = TaskRuns::default();
        assert!(!runs.has_ended());
        runs.push(ended("task:1", "done"));
        runs.push(ended("task:2", "done"));
        runs.push(ended("task:1", "cancelled"));
        assert!(runs.has_ended());
        let taken = runs.take();
        assert_eq!(
            taken
                .iter()
                .map(|run| (run.group.as_str(), run.outcome.as_str()))
                .collect::<Vec<_>>(),
            [("task:2", "done"), ("task:1", "cancelled")]
        );
        assert!(runs.take().is_empty());
    }
}
