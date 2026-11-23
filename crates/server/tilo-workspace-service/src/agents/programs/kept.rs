//! What a program keeps between its runs, with its installation on the host
//! that runs it: its counts and its note.
//!
//! A run starts with nothing but what fired it. What a program needs across
//! its runs is kept here. A count lets it act on every tenth event rather
//! than on each. The note says what the program is for and how its person
//! wants it done, holds answers the person accepted and rejected as
//! examples, and carries what the last run wants the next to know: what it
//! saw last, where it stopped.
//!
//! The note's parts are the ones a person's own instruction for a kind of
//! work has (the agent app's `Skill`): an instruction, a style, guidance and
//! examples. One typed change ([`NoteChange`]) updates it, whoever makes
//! it: a step of the program's own run, or the person's assistant from a
//! turn.
//!
//! Two more things are kept with a program and are its person's alone to
//! change, never a step's of its own run: whether it is paused, and the
//! permissions it carries. A permission lets the work the program starts
//! take one action that leaves the workspace without asking first. It
//! names exactly that action, so setting a program up gives it no reach.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::policy::Rule;
use tilo_lock::MutexRecover as _;

use super::api::AutomationStep;
use super::host_steps::{HostStepError, Keeps};

/// The longest a note's instruction, style or guidance may be.
pub const MAX_NOTE_TEXT_BYTES: usize = 16 * 1024;
/// The most examples a note keeps of each kind: a further one takes the
/// oldest one's place.
pub const MAX_EXAMPLES: usize = 20;
/// The longest one example may be.
pub const MAX_EXAMPLE_BYTES: usize = 8 * 1024;
/// The most that what a run keeps for the next may take, written as JSON.
pub const MAX_KEPT_BYTES: usize = 64 * 1024;
/// The most counts one program keeps.
pub const MAX_COUNTS: usize = 32;
/// The longest a count's name may be.
pub const MAX_COUNT_NAME_BYTES: usize = 64;
/// The most permissions one program carries.
pub const MAX_PERMITS: usize = 32;

/// A program's note. Every part is there when it is read, empty until
/// someone writes it, so a reader never has to ask whether a part exists.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Note {
    /// What the program is for, in its person's words.
    pub instruction: String,
    /// How what it writes should read: a voice, a length, a form.
    pub style: String,
    /// What its person cares about and how the work is done.
    pub guidance: String,
    /// Answers the person accepted, kept as examples, oldest first.
    pub accepted: Vec<String>,
    /// Answers the person rejected, kept as examples, oldest first.
    pub rejected: Vec<String>,
    /// What the last run kept for the next: what it saw last, where it
    /// stopped. Null until a run keeps something.
    pub kept: Value,
}

/// One change to a note. A text given takes that part's place, an example
/// given joins the examples of its kind, and `kept` takes the place of what
/// the last run kept. What is left out stays as it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoteChange {
    pub instruction: Option<String>,
    pub style: Option<String>,
    pub guidance: Option<String>,
    /// An answer the person accepted, to keep as an example.
    pub accepted: Option<String>,
    /// An answer the person rejected, to keep as an example.
    pub rejected: Option<String>,
    /// What this run keeps for the next.
    pub kept: Option<Value>,
}

impl Note {
    /// Make `change`.
    ///
    /// # Errors
    /// The part that is past its bound, in words for whoever made the
    /// change. The note is then left as it was.
    pub fn change(&mut self, change: NoteChange) -> Result<(), String> {
        let texts = [
            ("instruction", &change.instruction),
            ("style", &change.style),
            ("guidance", &change.guidance),
        ];
        for (part, text) in texts {
            if text
                .as_ref()
                .is_some_and(|text| text.len() > MAX_NOTE_TEXT_BYTES)
            {
                return Err(format!(
                    "the note's {part} is at most {MAX_NOTE_TEXT_BYTES} bytes"
                ));
            }
        }
        if [&change.accepted, &change.rejected]
            .into_iter()
            .flatten()
            .any(|example| example.len() > MAX_EXAMPLE_BYTES)
        {
            return Err(format!("an example is at most {MAX_EXAMPLE_BYTES} bytes"));
        }
        if let Some(kept) = &change.kept {
            let bytes = serde_json::to_vec(kept).map_or(usize::MAX, |bytes| bytes.len());
            if bytes > MAX_KEPT_BYTES {
                return Err(format!(
                    "a run keeps at most {MAX_KEPT_BYTES} bytes for the next"
                ));
            }
        }
        if let Some(instruction) = change.instruction {
            self.instruction = instruction;
        }
        if let Some(style) = change.style {
            self.style = style;
        }
        if let Some(guidance) = change.guidance {
            self.guidance = guidance;
        }
        if let Some(example) = change.accepted {
            keep_example(&mut self.accepted, example);
        }
        if let Some(example) = change.rejected {
            keep_example(&mut self.rejected, example);
        }
        if let Some(kept) = change.kept {
            self.kept = kept;
        }
        Ok(())
    }
}

/// Add `example` to `examples`, the oldest making room for it.
fn keep_example(examples: &mut Vec<String>, example: String) {
    examples.push(example);
    let extra = examples.len().saturating_sub(MAX_EXAMPLES);
    examples.drain(..extra);
}

/// A note as a run and an assistant read it: every part, empty or not.
#[must_use]
pub fn note_value(note: &Note) -> Value {
    serde_json::to_value(note).unwrap_or(Value::Null)
}

/// The steps of one invocation that advanced a count.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Counted {
    /// Kept under the name a program's file has always given it. A file that names it as
    /// the code does reads too, because builds wrote that for a while.
    #[serde(rename = "run", alias = "invocation")]
    invocation: String,
    steps: BTreeSet<String>,
}

/// How a run of a program ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Ended {
    /// It ran to its end.
    Ran,
    /// It handed its work over, and that work waits for its person's
    /// answer.
    Waits,
    /// It was refused, and will not be tried again on its own.
    Refused,
    /// It failed. Where it can go differently later it is tried again.
    Failed,
}

/// A program's last invocation: when it ended, how, and why when it did not
/// get to its end.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRun {
    /// When it ended, ms since the epoch.
    pub at_ms: i64,
    pub ended: Ended,
    /// Why it was refused or failed, in words for its person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// The longest a last run's reason is kept.
pub const MAX_WHY_BYTES: usize = 1024;

/// What one program keeps between its runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Kept {
    /// Its counts by name. The one a step does not name has the empty name.
    counts: BTreeMap<String, u64>,
    /// The run that advanced a count last, with the steps of it that did.
    /// Asked again, as a run that is tried again asks, each of them is
    /// answered where its count stands and advances nothing.
    counted: Option<Counted>,
    note: Note,
    /// The actions that leave the workspace which the program's person let
    /// its work take without asking, each rule naming one action.
    permits: Vec<Rule>,
    /// Whether its person paused it: a paused program answers nothing that
    /// comes while it is paused.
    paused: bool,
    /// How its last run ended; none until it ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    last_run: Option<LastRun>,
    /// How many runs ended, however they did.
    runs: u64,
}

impl Kept {
    /// Advance the count `name` by one for `step` of its run, and answer
    /// where the count stands.
    ///
    /// # Errors
    /// The name is too long, or the program keeps too many counts.
    pub fn count(&mut self, step: &AutomationStep, name: &str) -> Result<u64, String> {
        if name.len() > MAX_COUNT_NAME_BYTES {
            return Err(format!(
                "a count's name is at most {MAX_COUNT_NAME_BYTES} bytes"
            ));
        }
        if !self.counts.contains_key(name) && self.counts.len() >= MAX_COUNTS {
            return Err(format!("a program keeps at most {MAX_COUNTS} counts"));
        }
        if self
            .counted
            .as_ref()
            .is_none_or(|counted| counted.invocation != step.invocation)
        {
            self.counted = Some(Counted {
                invocation: step.invocation.clone(),
                steps: BTreeSet::new(),
            });
        }
        let fresh = self
            .counted
            .as_mut()
            .is_some_and(|counted| counted.steps.insert(step.step.clone()));
        let count = self.counts.entry(name.to_owned()).or_insert(0);
        if fresh {
            *count = count.saturating_add(1);
        }
        Ok(*count)
    }

    /// The program's counts, by name.
    #[must_use]
    pub const fn counts(&self) -> &BTreeMap<String, u64> {
        &self.counts
    }

    /// The program's note.
    #[must_use]
    pub const fn note(&self) -> &Note {
        &self.note
    }

    /// The actions that leave the workspace which the program's work may
    /// take without asking.
    #[must_use]
    pub fn permits(&self) -> &[Rule] {
        &self.permits
    }

    /// Let the program's work take one action that leaves the workspace
    /// without asking, from now on.
    ///
    /// # Errors
    /// `permission` does not allow, or names no single action: nothing
    /// wider is ever kept. Or the program carries too many already.
    pub fn permit(&mut self, permission: Rule) -> Result<(), String> {
        if tilo_automation::grant::advance_consents(std::slice::from_ref(&permission)).is_empty() {
            return Err("a permission allows exactly one action".into());
        }
        if self.permits.contains(&permission) {
            return Ok(());
        }
        if self.permits.len() >= MAX_PERMITS {
            return Err(format!(
                "a program carries at most {MAX_PERMITS} permissions"
            ));
        }
        self.permits.push(permission);
        Ok(())
    }

    /// Take `permission` back: its action asks again.
    pub fn withdraw(&mut self, permission: &Rule) {
        self.permits.retain(|kept| kept != permission);
    }

    /// Whether the program's person paused it.
    #[must_use]
    pub const fn paused(&self) -> bool {
        self.paused
    }

    /// Pause the program, or let it go on.
    pub const fn pause(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Change the note ([`Note::change`]).
    ///
    /// # Errors
    /// As [`Note::change`].
    pub fn change_note(&mut self, change: NoteChange) -> Result<(), String> {
        self.note.change(change)
    }

    /// How the program's last run ended; none until it ran.
    #[must_use]
    pub const fn last_run(&self) -> Option<&LastRun> {
        self.last_run.as_ref()
    }

    /// How many of the program's runs ended, however they did.
    #[must_use]
    pub const fn runs(&self) -> u64 {
        self.runs
    }

    /// Keep how a run ended, its reason cut to [`MAX_WHY_BYTES`]. A run
    /// that is tried again is one run: `again` says this is the same run
    /// ending once more.
    pub fn ran(&mut self, mut last: LastRun, again: bool) {
        if let Some(why) = &mut last.why {
            let end = why
                .char_indices()
                .map(|(at, c)| at.saturating_add(c.len_utf8()))
                .take_while(|end| *end <= MAX_WHY_BYTES)
                .last()
                .unwrap_or(0);
            why.truncate(end);
        }
        self.last_run = Some(last);
        if !again {
            self.runs = self.runs.saturating_add(1);
        }
    }
}

/// Kept in memory only: for a host with nowhere to write, and for tests.
impl Keeps for Mutex<Kept> {
    fn count(&self, step: &AutomationStep, name: &str) -> Result<u64, HostStepError> {
        self.lock_recover()
            .count(step, name)
            .map_err(HostStepError::Refused)
    }

    fn keep(&self, change: NoteChange) -> Result<(), HostStepError> {
        self.lock_recover()
            .change_note(change)
            .map_err(HostStepError::Refused)
    }

    fn note(&self) -> Value {
        note_value(self.lock_recover().note())
    }

    fn permits(&self) -> Vec<Rule> {
        self.lock_recover().permits().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn step(run: &str, step: &str) -> AutomationStep {
        AutomationStep {
            invocation: run.into(),
            step: step.into(),
        }
    }

    #[test]
    fn a_count_advances_once_for_each_step_of_each_run() {
        let mut kept = Kept::default();
        assert_eq!(kept.count(&step("r1", "0"), ""), Ok(1));
        assert_eq!(
            kept.count(&step("r1", "0"), ""),
            Ok(1),
            "the run tried again advances nothing"
        );
        assert_eq!(kept.count(&step("r2", "0"), ""), Ok(2));
        assert_eq!(
            kept.count(&step("r2", "1"), "mail"),
            Ok(1),
            "a named count is its own"
        );
        // Written and read back, as across a restart, it stands where it
        // stood and still knows the run it counted last.
        let written = serde_json::to_value(&kept).unwrap_or_default();
        let mut read: Kept = serde_json::from_value(written).unwrap_or_default();
        assert_eq!(read, kept);
        assert_eq!(read.count(&step("r2", "0"), ""), Ok(2));
        assert_eq!(read.count(&step("r3", "0"), ""), Ok(3));
        assert_eq!(read.counts().get("mail"), Some(&1));
        assert!(
            read.count(
                &step("r4", "0"),
                &"n".repeat(MAX_COUNT_NAME_BYTES.saturating_add(1))
            )
            .is_err()
        );
    }

    /// A program's file as a server wrote it before the code named an invocation, and as
    /// builds wrote it for a while after: both read with what the program counted, and the
    /// file is written as it always was.
    #[test]
    fn what_a_program_counted_reads_from_a_file_of_before_and_of_between() {
        let before = json!({
            "counts": {"": 2, "mail": 1},
            "counted": {"run": "r2", "steps": ["0", "1"]},
            "paused": true,
            "runs": 2,
        });
        let mut between = before.clone();
        between["counted"] = json!({"invocation": "r2", "steps": ["0", "1"]});
        for file in [before.clone(), between] {
            let mut kept: Kept = serde_json::from_value(file).expect("the file reads");
            assert!(
                kept.paused(),
                "what stood beside the counts is read with them"
            );
            assert_eq!(
                kept.count(&step("r2", "1"), ""),
                Ok(2),
                "a step that counted already advances nothing"
            );
            assert_eq!(kept.count(&step("r3", "0"), ""), Ok(3));
            let mut read: Kept = serde_json::from_value(before.clone()).expect("the file reads");
            read.count(&step("r2", "1"), "").expect("counted before");
            let written = serde_json::to_value(&read).unwrap_or_default();
            assert_eq!(written["counted"], before["counted"]);
        }
    }

    #[test]
    fn a_permission_names_exactly_one_action_and_can_be_taken_back() {
        use tilo_actions::policy::{Answer, KindSet, Level, TreePath};
        let send = tilo_automation::action_rule(
            "so.tilo.mail",
            "mail.mailbox",
            None,
            "send",
            KindSet::OUTSIDE,
            Answer::Allow,
        );
        let mut kept = Kept::default();
        assert!(kept.permits().is_empty(), "a program starts with no reach");
        assert_eq!(kept.permit(send.clone()), Ok(()));
        assert_eq!(
            kept.permit(send.clone()),
            Ok(()),
            "the same one is kept once"
        );
        assert_eq!(kept.permits(), std::slice::from_ref(&send));
        // Nothing wider: a whole app, or a rule that asks or denies.
        let whole_app = Rule::new(
            TreePath::root().any(Level::Workspace).app("so.tilo.mail"),
            KindSet::ALL,
            Answer::Allow,
        );
        assert!(kept.permit(whole_app).is_err());
        let asks = Rule {
            answer: Answer::Ask,
            ..send.clone()
        };
        assert!(kept.permit(asks).is_err());
        assert_eq!(kept.permits().len(), 1);
        kept.pause(true);
        let written = serde_json::to_value(&kept).unwrap_or_default();
        let mut read: Kept = serde_json::from_value(written).unwrap_or_default();
        assert_eq!(read, kept, "both are kept across a restart");
        assert!(read.paused());
        read.withdraw(&send);
        assert!(read.permits().is_empty());
    }

    #[test]
    fn a_note_is_changed_part_by_part_within_its_bounds() {
        let mut note = Note::default();
        assert_eq!(
            note_value(&note),
            json!({
                "instruction": "", "style": "", "guidance": "",
                "accepted": [], "rejected": [], "kept": null
            }),
            "every part reads, empty until written"
        );
        assert_eq!(
            note.change(NoteChange {
                instruction: Some("Answer new inquiries.".into()),
                style: Some("Short. First names.".into()),
                ..NoteChange::default()
            }),
            Ok(())
        );
        for answer in 0..25 {
            assert_eq!(
                note.change(NoteChange {
                    accepted: Some(format!("answer {answer}")),
                    ..NoteChange::default()
                }),
                Ok(())
            );
        }
        assert_eq!(note.accepted.len(), MAX_EXAMPLES);
        assert_eq!(
            note.accepted.first().map(String::as_str),
            Some("answer 5"),
            "the oldest examples made room"
        );
        assert_eq!(
            note.change(NoteChange {
                kept: Some(json!({"seen": "m-41"})),
                ..NoteChange::default()
            }),
            Ok(())
        );
        assert_eq!(note.kept, json!({"seen": "m-41"}));
        assert_eq!(note.instruction, "Answer new inquiries.", "the rest stays");
        // A change past a bound changes nothing, also not its other parts.
        let refused = note.change(NoteChange {
            style: Some("x".repeat(MAX_NOTE_TEXT_BYTES.saturating_add(1))),
            guidance: Some("Ask before promising a date.".into()),
            ..NoteChange::default()
        });
        assert!(refused.is_err());
        assert_eq!(note.style, "Short. First names.");
        assert_eq!(note.guidance, "");
    }
}
