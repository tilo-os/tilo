//! What an issue keeps while it is worked on over many runs: where its working material is, what
//! it waits for, what it may cost, and why it stopped.
//!
//! Work that takes hours or weeks does not fit one run of whoever does it. A run ends, the
//! machine restarts, a reply takes two days. So everything the next run needs is kept with the
//! issue, in the workspace's own records, and nothing beside them:
//!
//! - **A place.** A folder in the workspace's files and a note in Notes, both named here. A run
//!   writes its plan, what it found (with where it found it) and what is still open into the
//!   note, and puts files into the folder; a fresh run starts from them.
//! - **Waits.** What the work waits for, in words a person reads, and the latest moment the wait
//!   ends. Whichever comes first ends it: any one of the things waited for, or that moment.
//!   Waiting costs nothing: nothing runs until the wait ends.
//! - **A budget.** Money and working time for the whole issue, over all its runs.
//! - **Why it stopped.** A run that hit the budget, lost its worker or failed says so here, in a
//!   plain line, so an issue nobody works on never reads as one somebody does.
//!
//! [`standing`] is how every reader tells these apart: the panel that lists a person's tasks,
//! the host that decides whether to run, and a test.

use serde::{Deserialize, Serialize};
use tilo_resource::{JsonSchema, Model, Refusal};

use super::{Issue, Status};

/// The most things one wait names.
pub const MAX_WAITS: usize = 16;
/// The longest line a wait, a stop or a resume keeps, in characters.
pub const MAX_LINE_CHARS: usize = 1024;
/// The longest address a wait or a place names, in characters.
pub const MAX_ADDRESS_CHARS: usize = 1024;
/// How often in a row a run may be lost before the issue stops and says so.
pub const MOST_LOST_RUNS: u64 = 2;

/// What a wait is for.
#[derive(
    Model, Default, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum WaitKind {
    /// A moment: nothing else ends it.
    #[default]
    Time,
    /// A reply: in a mail thread, a conversation.
    Reply,
    /// A record of the workspace changing.
    Change,
    /// An answer of the person the work is for: the issue needs them.
    Answer,
    /// Answers of several people.
    People,
}

/// One thing an issue waits for.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WaitFor {
    /// What kind of thing it is.
    #[serde(default)]
    pub kind: WaitKind,
    /// What is waited for, in a few words a person reads: `a reply from the supplier`.
    pub what: String,
    /// What is watched: a mail thread, a resource's address, the message that asks the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<String>,
    /// The event of the person's that can end the wait early, by its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

/// Why an issue's work stopped.
#[derive(
    Model, Default, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum StoppedWhy {
    /// It could not go on: the line says what failed.
    #[default]
    Failed,
    /// Its budget in money is spent.
    Budget,
    /// Its budget in working time is spent.
    Time,
    /// Its runs were interrupted and did not come back.
    LostWorker,
    /// A person stopped it.
    Person,
}

/// That an issue's work stopped, and why.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub why: StoppedWhy,
    /// The plain line its person reads.
    pub words: String,
    pub at_ms: i64,
}

/// That an issue's work was told to go on, and by what.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Resumed {
    /// What ended the wait or the stop, in words: `the supplier replied`.
    pub cause: String,
    pub at_ms: i64,
}

/// Who set an issue's budget.
#[derive(
    Model, Default, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum BudgetBy {
    /// A person: only a person raises it.
    #[default]
    Person,
    /// The agent working on it proposed it, and holds itself to it.
    Agent,
}

/// What an issue keeps while it is worked on over many runs.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Work {
    /// The folder in the workspace's files its working material is kept in; empty for none.
    pub folder: String,
    /// The note its plan, findings and open points are kept in, by its resource in Notes; empty
    /// for none.
    pub note: String,
    /// What it waits for; empty while it does not wait. Any one of them ends the wait.
    pub waits: Vec<WaitFor>,
    /// The latest moment the wait ends, in milliseconds since the epoch. Every wait has one.
    pub until_ms: Option<i64>,
    /// Since when it waits.
    pub since_ms: Option<i64>,
    /// What the whole issue may cost, in millionths of the currency.
    pub budget_micros: Option<u64>,
    /// How long it may be worked on over all its runs, in milliseconds. Waiting is not counted.
    pub budget_ms: Option<u64>,
    /// Who set the budget.
    pub budget_by: BudgetBy,
    /// What its runs cost so far.
    pub spent_micros: u64,
    /// How long its runs worked so far.
    pub worked_ms: u64,
    /// How many runs began.
    pub runs: u64,
    /// Since when a run is going, as its agent said when it began.
    pub running_since_ms: Option<i64>,
    /// How many runs in a row began and never ended.
    pub lost: u64,
    /// Set when the work stopped.
    pub stopped: Option<Stopped>,
    /// Set when the work was told to go on: its agent is asked again from then.
    pub resumed: Option<Resumed>,
}

impl Work {
    /// Whether it waits for something.
    #[must_use]
    pub const fn waiting(&self) -> bool {
        self.until_ms.is_some()
    }

    /// The money still to spend, when there is a budget in money.
    #[must_use]
    pub fn money_left(&self) -> Option<u64> {
        self.budget_micros
            .map(|budget| budget.saturating_sub(self.spent_micros))
    }

    /// The working time still to use, when there is a budget in time.
    #[must_use]
    pub fn time_left_ms(&self) -> Option<u64> {
        self.budget_ms
            .map(|budget| budget.saturating_sub(self.worked_ms))
    }

    /// Which budget is used up, if one is: money first.
    #[must_use]
    pub fn used_up(&self) -> Option<StoppedWhy> {
        if self.money_left() == Some(0) {
            Some(StoppedWhy::Budget)
        } else if self.time_left_ms() == Some(0) {
            Some(StoppedWhy::Time)
        } else {
            None
        }
    }

    /// Stop waiting: the things waited for, the moment, and since when.
    pub fn end_wait(&mut self) {
        self.waits.clear();
        self.until_ms = None;
        self.since_ms = None;
    }
}

/// How an issue stands, as whoever lists tasks reads it. The first of these that fits, in the
/// order [`standing`] tries them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// A person called it off.
    Cancelled,
    /// Its work stopped before it was finished: it needs its person, and is never read as done
    /// or as ready for review.
    Stopped {
        why: StoppedWhy,
        words: String,
    },
    Done,
    /// Whoever worked on it is done, and a person has not looked yet.
    ReadyForReview,
    /// It waits for an answer of its person: the question, in words.
    NeedsYou {
        what: String,
    },
    /// It waits for something else: each thing in words, and the latest moment the wait ends.
    Waiting {
        what: Vec<String>,
        until_ms: i64,
    },
    /// It is being worked on.
    Working,
    /// Nobody started it yet.
    UpNext,
}

impl Standing {
    /// Whether the issue is over: finished or called off. A stopped one never is.
    #[must_use]
    pub const fn resolved(&self) -> bool {
        matches!(self, Self::Cancelled | Self::Done)
    }
}

/// How `issue` stands.
///
/// A stop is read before `done` and `in-review`: work that hit its budget, lost its worker or
/// failed is not finished, whatever its status was set to since.
#[must_use]
pub fn standing(issue: &Issue) -> Standing {
    if issue.status == Status::Cancelled {
        return Standing::Cancelled;
    }
    let work = issue.work.as_ref();
    if let Some(stopped) = work.and_then(|work| work.stopped.as_ref()) {
        return Standing::Stopped {
            why: stopped.why,
            words: stopped.words.clone(),
        };
    }
    match issue.status {
        Status::Done => return Standing::Done,
        Status::InReview => return Standing::ReadyForReview,
        _ => {}
    }
    if let Some(work) = work
        && let Some(until_ms) = work.until_ms
    {
        if let Some(asked) = work.waits.iter().find(|wait| wait.kind == WaitKind::Answer) {
            return Standing::NeedsYou {
                what: asked.what.clone(),
            };
        }
        return Standing::Waiting {
            what: work.waits.iter().map(|wait| wait.what.clone()).collect(),
            until_ms,
        };
    }
    if issue.status == Status::InProgress {
        Standing::Working
    } else {
        Standing::UpNext
    }
}

/// `line` as an issue keeps it: trimmed, and short enough.
///
/// # Errors
/// Refuses an empty line or one past [`MAX_LINE_CHARS`], naming `what` it is.
pub fn checked_line(line: &str, what: &str) -> Result<String, Refusal> {
    let line = line.trim();
    if line.is_empty() {
        return Err(Refusal::new(format!("Say {what} in a few words")));
    }
    if line.chars().count() > MAX_LINE_CHARS {
        return Err(Refusal::new(format!(
            "{what} fits in {MAX_LINE_CHARS} characters"
        )));
    }
    Ok(line.to_owned())
}

/// `address` as a place or a wait names it: trimmed, none when blank.
///
/// # Errors
/// Refuses one past [`MAX_ADDRESS_CHARS`].
pub fn checked_address(address: Option<String>) -> Result<Option<String>, Refusal> {
    let Some(address) = address
        .map(|address| address.trim().to_owned())
        .filter(|address| !address.is_empty())
    else {
        return Ok(None);
    };
    if address.chars().count() > MAX_ADDRESS_CHARS {
        return Err(Refusal::new("That address is too long"));
    }
    Ok(Some(address))
}

/// A folder path as the workspace's files keep it: its parts joined by `/`, none of them empty,
/// `.` or `..`.
///
/// # Errors
/// Refuses an empty path, a part a folder may not be called, or one past [`MAX_ADDRESS_CHARS`].
pub fn checked_folder(path: &str) -> Result<String, Refusal> {
    let parts: Vec<&str> = path
        .split('/')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(Refusal::new("Name the folder its material is kept in"));
    }
    if parts
        .iter()
        .any(|part| *part == "." || *part == ".." || part.contains('\0'))
    {
        return Err(Refusal::new("That is no folder's path"));
    }
    let folder = parts.join("/");
    if folder.chars().count() > MAX_ADDRESS_CHARS {
        return Err(Refusal::new("That folder's path is too long"));
    }
    Ok(folder)
}

/// The waits a `wait` takes, as an issue keeps them.
///
/// # Errors
/// Refuses more than [`MAX_WAITS`], and a wait that does not say what it is for.
pub fn checked_waits(waits: Vec<WaitFor>) -> Result<Vec<WaitFor>, Refusal> {
    if waits.len() > MAX_WAITS {
        return Err(Refusal::new(format!(
            "A wait names at most {MAX_WAITS} things"
        )));
    }
    waits
        .into_iter()
        .map(|wait| {
            Ok(WaitFor {
                kind: wait.kind,
                what: checked_line(&wait.what, "what it waits for")?,
                on: checked_address(wait.on)?,
                event: checked_address(wait.event)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test these are the assertion")]

    use super::*;

    fn issue(status: Status, work: Option<Work>) -> Issue {
        Issue {
            title: "Write the analysis".into(),
            status,
            work,
            ..Issue::default()
        }
    }

    fn stopped(why: StoppedWhy) -> Work {
        Work {
            stopped: Some(Stopped {
                why,
                words: "I stopped: the budget of $2.00 is spent.".into(),
                at_ms: 5,
            }),
            ..Work::default()
        }
    }

    /// Work that stopped at its budget or lost its worker never reads as done, as ready for
    /// review or as resolved, whatever its status says.
    #[test]
    fn work_that_stopped_never_reads_as_done() {
        for why in [
            StoppedWhy::Budget,
            StoppedWhy::Time,
            StoppedWhy::LostWorker,
            StoppedWhy::Failed,
        ] {
            for status in [
                Status::Backlog,
                Status::Todo,
                Status::InProgress,
                Status::InReview,
                Status::Done,
            ] {
                let read = standing(&issue(status, Some(stopped(why))));
                assert!(
                    matches!(&read, Standing::Stopped { why: read_why, words }
                        if *read_why == why && words.contains("budget of $2.00")),
                    "{status:?} stopped for {why:?} reads {read:?}"
                );
                assert!(!read.resolved(), "{status:?} stopped for {why:?}");
            }
        }
        // Called off by a person, it is over.
        assert_eq!(
            standing(&issue(Status::Cancelled, Some(stopped(StoppedWhy::Budget)))),
            Standing::Cancelled
        );
    }

    #[test]
    fn an_issue_reads_by_its_status_and_what_it_waits_for() {
        assert_eq!(standing(&issue(Status::Backlog, None)), Standing::UpNext);
        assert_eq!(standing(&issue(Status::Todo, None)), Standing::UpNext);
        assert_eq!(
            standing(&issue(Status::InProgress, None)),
            Standing::Working
        );
        assert_eq!(
            standing(&issue(Status::InReview, None)),
            Standing::ReadyForReview
        );
        assert_eq!(standing(&issue(Status::Done, None)), Standing::Done);

        let reply = WaitFor {
            kind: WaitKind::Reply,
            what: "a reply from the supplier".into(),
            ..WaitFor::default()
        };
        let waiting = Work {
            waits: vec![reply.clone()],
            until_ms: Some(9_000),
            ..Work::default()
        };
        assert_eq!(
            standing(&issue(Status::InProgress, Some(waiting.clone()))),
            Standing::Waiting {
                what: vec!["a reply from the supplier".into()],
                until_ms: 9_000
            }
        );
        // A wait for its person's answer is what needs them, beside whatever else it waits for.
        let asked = Work {
            waits: vec![
                reply,
                WaitFor {
                    kind: WaitKind::Answer,
                    what: "Which regions count?".into(),
                    ..WaitFor::default()
                },
            ],
            ..waiting
        };
        assert_eq!(
            standing(&issue(Status::InProgress, Some(asked))),
            Standing::NeedsYou {
                what: "Which regions count?".into()
            }
        );
        // A moment alone is a wait too.
        let until = Work {
            until_ms: Some(9_000),
            ..Work::default()
        };
        assert_eq!(
            standing(&issue(Status::Todo, Some(until))),
            Standing::Waiting {
                what: Vec::new(),
                until_ms: 9_000
            }
        );
    }

    #[test]
    fn a_budget_says_what_is_left_and_which_part_is_used_up() {
        let mut work = Work {
            budget_micros: Some(2_000_000),
            budget_ms: Some(60_000),
            ..Work::default()
        };
        assert_eq!(work.money_left(), Some(2_000_000));
        assert_eq!(work.used_up(), None);
        work.worked_ms = 60_000;
        assert_eq!(work.used_up(), Some(StoppedWhy::Time));
        work.spent_micros = 2_300_000;
        assert_eq!(work.money_left(), Some(0));
        assert_eq!(work.used_up(), Some(StoppedWhy::Budget));
        assert_eq!(
            Work::default().used_up(),
            None,
            "no budget, nothing used up"
        );
    }

    #[test]
    fn a_folder_and_a_wait_are_kept_as_written_or_refused() {
        assert_eq!(
            checked_folder(" Tasks / 12 Market analysis/ ").unwrap(),
            "Tasks/12 Market analysis"
        );
        assert!(checked_folder("  ").is_err());
        assert!(checked_folder("Tasks/../Private").is_err());
        let kept = checked_waits(vec![WaitFor {
            kind: WaitKind::Reply,
            what: "  a reply from the supplier ".into(),
            on: Some("  ".into()),
            event: Some(" e1 ".into()),
        }])
        .unwrap();
        assert_eq!(kept[0].what, "a reply from the supplier");
        assert_eq!(kept[0].on, None);
        assert_eq!(kept[0].event.as_deref(), Some("e1"));
        assert!(
            checked_waits(vec![WaitFor::default()]).is_err(),
            "a wait says what it is for"
        );
        assert!(checked_waits(vec![kept[0].clone(); MAX_WAITS + 1]).is_err());
    }
}
