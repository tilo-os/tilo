//! Taking back one run: exactly the changes a run of this session made, in every record it
//! changed, and nothing anyone else did.
//!
//! A run is an invocation this session started, or, when the call named a run of the client's
//! (`_meta` run), every call of that run: the client chose that as one thing to take back, and
//! they share one undo step. Each record it changed lists its changes in its own `history`, each
//! with the undo step it is part of (as recorded under its run) and, where the host kept it, the
//! invocation that made it, so the run's changes are found there by those and taken back with
//! the record's own `undo` of exactly that step (`step`), never of whatever the record's last
//! step happens to be. A change someone else made on top of the run's in a record refuses the whole
//! undo before anything is taken back: undoing under their work would change what they see
//! without their say.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::watch::history_entries;
use super::{ActionHost, Addressed, McpServer, OneCall, Reply, Value, json};
use tilo_lock::MutexRecover as _;

/// How many of a record's changes are read to find a run's.
const HISTORY_READ: u64 = 200;
/// How long a record has to answer its history or its undo.
const RECORD_WAIT_MS: u64 = 10_000;
/// The most steps one record's share of a run is taken back in: a run's changes in a record are
/// one step unless someone else's changes fell between them.
const MOST_STEPS: usize = 16;

/// How many invocations' changes a session keeps to take back.
const KEPT: usize = 1024;

/// The record one invocation of this session changed, where its change is taken back.
#[derive(Clone, Debug)]
pub(crate) struct Undoable {
    pub(crate) invocation: String,
    /// The resource the call named, as a caller writes it.
    pub(crate) path: String,
    /// The provider that answers for the record.
    pub(crate) provider: String,
    /// The app whose record it is.
    pub(crate) app: String,
    /// The resource there that offers the record's history verbs.
    pub(crate) history: String,
    /// The undo step the call joined: its own, or the one every call of a run the client
    /// named shares.
    pub(crate) step: tilo_actions::ActionId,
}

/// One record a run changed.
struct Target {
    /// What a caller names it by.
    path: String,
    changed: Undoable,
}

/// The run being taken back: its undo step, and the invocations of it.
pub(crate) struct Run {
    step: tilo_actions::ActionId,
    invocations: BTreeSet<String>,
}

impl Run {
    /// Whether the history `entry` lists is one of this run's changes: of its undo step, as
    /// recorded under the run the entry names, or made by one of its invocations.
    fn made(&self, entry: &Value) -> bool {
        let field = |key: &str| entry.get(key).and_then(Value::as_str);
        let invoked = field("invocation").is_some_and(|id| self.invocations.contains(id));
        let stepped = field("step").is_some_and(|step| {
            let run = field("run_kind").zip(field("run_id"));
            tilo_actions::context::recorded_gesture(self.step, run)
                .0
                .to_string()
                == step
        });
        invoked || stepped
    }
}

/// What a record's history says of a run.
struct Standing {
    /// The run's changes still in effect, oldest first.
    changes: Vec<String>,
    /// Someone else's change made after the run's last, in effect: why it is not taken back.
    over: Option<Value>,
}

impl<H: ActionHost + Send + Sync + 'static> McpServer<H> {
    /// Take back the run invocation `invocation` of this session is (a step of): every change it
    /// made, in every record it changed, or nothing when someone else's change stands on one of
    /// them.
    pub(super) async fn undo_run(
        &self,
        invocation: &str,
        structured: bool,
    ) -> Result<Reply, String> {
        let (run, targets) = self.run_targets(invocation)?;
        let mut plans = Vec::new();
        let mut refusals = Vec::new();
        for target in targets {
            let standing = self.standing(&target, &run).await?;
            if let Some(over) = &standing.over {
                refusals.push(refusal(&target.path, over));
            } else if !standing.changes.is_empty() {
                plans.push((target, standing.changes));
            }
        }
        if !refusals.is_empty() {
            return Err(format!(
                "run {invocation} is not taken back: {}. Ask whoever made those changes, or take \
                 back that record's step alone with `<path>#undo {{\"step\": <change>}}`, which \
                 keeps their work",
                refusals.join("; ")
            ));
        }
        if plans.is_empty() {
            return Err(format!(
                "run {invocation} has nothing left to take back: what it changed is undone"
            ));
        }
        let mut undone = Vec::new();
        let mut text = String::new();
        for (target, mut changes) in plans {
            let mut taken = Vec::new();
            for _ in 0..MOST_STEPS {
                let Some(first) = changes.first().cloned() else {
                    break;
                };
                self.undo_step(&target, &first).await?;
                taken.push(first);
                let left = self.standing(&target, &run).await?.changes;
                // A step its undo did not take is left as it is, never asked again.
                changes = left
                    .into_iter()
                    .filter(|change| !taken.contains(change))
                    .collect();
            }
            let _ = write!(
                text,
                "{}undone in {}: {} {}",
                if text.is_empty() { "" } else { "\n" },
                target.path,
                taken.len(),
                if taken.len() == 1 { "step" } else { "steps" }
            );
            undone.push(json!({
                "path": target.path,
                "app": target.changed.app,
                "changes": taken,
            }));
        }
        let mut reply = Reply::ok(text);
        if structured {
            reply.structured = Some(json!({
                "status": "ok",
                "invocation_id": invocation,
                "undone": undone,
            }));
        }
        Ok(reply)
    }

    /// Note what the call answered `value` changed: the record `provider` of `app` answers
    /// for, at `path`, whose history verbs `history` offers. A call naming the run it is a step
    /// of makes that run's step too, which `undo task` takes back in every app it changed, as it
    /// takes back a program's. A call that names no invocation, or a record with no history to
    /// take it back in, leaves nothing for [`Self::undo_run`] to take back.
    pub(super) fn note_change(
        &self,
        value: &Value,
        path: &str,
        (provider, app): (&str, &str),
        history: Option<String>,
    ) {
        if let Some((gesture, label)) = Self::grouped_run() {
            self.programs
                .lock_recover()
                .note_run(gesture, &label, app, history.clone());
        }
        let (Some(invocation), Some(history), Some(step)) = (
            value.get("invocation_id").and_then(Value::as_str),
            history,
            Self::call_step(),
        ) else {
            return;
        };
        let mut kept = self.undoable.lock_recover();
        kept.push_back(Undoable {
            invocation: invocation.to_owned(),
            path: path.to_owned(),
            provider: provider.to_owned(),
            app: app.to_owned(),
            history,
            step,
        });
        while kept.len() > KEPT {
            kept.pop_front();
        }
        drop(kept);
    }

    /// The invocations of the run `invocation` is a step of, and the records they changed, each
    /// once.
    fn run_targets(&self, invocation: &str) -> Result<(Run, Vec<Target>), String> {
        let kept = self.undoable.lock_recover();
        let step = kept
            .iter()
            .find(|changed| changed.invocation == invocation)
            .ok_or_else(|| {
                format!(
                    "run {invocation} changed nothing this connection can take back: a run is \
                     taken back by the connection that started it, once it changed a record"
                )
            })?
            .step;
        let members: Vec<&Undoable> = kept.iter().filter(|changed| changed.step == step).collect();
        let run = Run {
            step,
            invocations: members
                .iter()
                .map(|changed| changed.invocation.clone())
                .collect(),
        };
        let mut targets: Vec<Target> = Vec::new();
        for changed in members {
            if targets.iter().any(|target| {
                target.changed.provider == changed.provider
                    && target.changed.history == changed.history
            }) {
                continue;
            }
            targets.push(Target {
                path: changed.path.clone(),
                changed: changed.clone(),
            });
        }
        drop(kept);
        Ok((run, targets))
    }

    /// What `target`'s history says of the run whose invocations are `invocations`.
    async fn standing(&self, target: &Target, run: &Run) -> Result<Standing, String> {
        let mut arguments = serde_json::Map::new();
        arguments.insert("last".into(), json!(HISTORY_READ));
        let value = self.on_record(target, "history", arguments).await?;
        let entries = history_entries(&value)
            .ok_or_else(|| format!("{} answered no history to find the run in", target.path))?;
        Ok(standing(&entries, |entry| run.made(entry)))
    }

    /// Take back the step of `target`'s record that the change `change` is part of.
    async fn undo_step(&self, target: &Target, change: &str) -> Result<(), String> {
        let mut arguments = serde_json::Map::new();
        arguments.insert("step".into(), json!(change));
        let value = self.on_record(target, "undo", arguments).await?;
        if value.pointer("/update/state").and_then(Value::as_str) == Some("succeeded") {
            return Ok(());
        }
        Err(format!(
            "in {}: {}",
            target.path,
            super::outcome(&value).text
        ))
    }

    /// Run the history verb `action` on `target`'s record with `arguments`.
    async fn on_record(
        &self,
        target: &Target,
        action: &str,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        Box::pin(self.invoke_one(OneCall {
            provider: Addressed {
                provider: target.changed.provider.clone(),
                app_id: Some(target.changed.app.clone()),
            },
            workspace: false,
            revision: None,
            resource: target.changed.history.clone(),
            action: action.to_owned(),
            arguments,
            invocation_id: None,
            wait_ms: Some(RECORD_WAIT_MS),
        }))
        .await
        .map_err(|why| format!("in {}: {why}", target.path))
    }
}

/// What a record's history `entries` (oldest first, each whole) say of the run whose changes
/// `of_run` tells: its changes still in effect, and a change someone else made after its last
/// one that is still in effect.
fn standing(entries: &[Value], of_run: impl Fn(&Value) -> bool) -> Standing {
    let in_effect = |entry: &&Value| entry.get("undone").and_then(Value::as_bool) != Some(true);
    let changes: Vec<String> = entries
        .iter()
        .filter(in_effect)
        .filter(|entry| of_run(entry))
        .filter_map(|entry| {
            entry
                .get("change")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    let last = entries
        .iter()
        .rposition(|entry| of_run(entry) && in_effect(&entry));
    let over = last.and_then(|last| {
        entries
            .iter()
            .skip(last.saturating_add(1))
            .filter(in_effect)
            // Housekeeping (naming a checkpoint) changes nothing the run wrote.
            .filter(|entry| entry.get("change_kind").is_none())
            .find(|entry| !of_run(entry))
            .cloned()
    });
    Standing { changes, over }
}

/// Why the run is not taken back in the record at `path`: `over`, the change standing on it.
fn refusal(path: &str, over: &Value) -> String {
    let who = over
        .get("run")
        .or_else(|| over.get("by"))
        .or_else(|| over.pointer("/actor/name"))
        .and_then(Value::as_str)
        .unwrap_or("someone");
    let what = over
        .get("sentence")
        .and_then(Value::as_str)
        .filter(|sentence| !sentence.is_empty())
        .unwrap_or("changed it");
    let change = over
        .get("change")
        .and_then(Value::as_str)
        .unwrap_or_default();
    format!("in {path}, {who} {what} ({change}) after it")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(change: &str, invocation: &str) -> Value {
        json!({"change": change, "invocation": invocation, "sentence": "Added a paragraph", "by": "Ada"})
    }

    fn run(invocations: &[&str]) -> impl Fn(&Value) -> bool {
        let invocations: BTreeSet<String> = invocations.iter().map(|id| (*id).to_owned()).collect();
        move |entry: &Value| {
            entry
                .get("invocation")
                .and_then(Value::as_str)
                .is_some_and(|id| invocations.contains(id))
        }
    }

    #[test]
    fn a_run_finds_its_own_changes_in_effect() {
        let entries = [
            entry("a1", "before"),
            entry("b1", "run"),
            json!({"change": "b2", "invocation": "run", "undone": true}),
            entry("b3", "run"),
        ];
        let standing = standing(&entries, run(&["run"]));
        assert_eq!(standing.changes, ["b1", "b3"]);
        assert!(standing.over.is_none());
    }

    #[test]
    fn someone_elses_change_on_top_holds_the_run() {
        let entries = [entry("b1", "run"), entry("c1", "theirs")];
        let held = standing(&entries, run(&["run"]));
        assert_eq!(held.over.as_ref().unwrap()["change"], "c1");
        assert_eq!(
            refusal("word-processor", held.over.as_ref().unwrap()),
            "in word-processor, Ada Added a paragraph (c1) after it"
        );
        // What they made before the run, what they took back since, and housekeeping
        // stand on nothing of the run's.
        let entries = [
            entry("a1", "theirs"),
            entry("b1", "run"),
            json!({"change": "c1", "invocation": "theirs", "undone": true}),
            json!({"change": "c2", "change_kind": "housekeeping"}),
        ];
        assert!(standing(&entries, run(&["run"])).over.is_none());
    }

    #[test]
    fn every_call_of_a_named_run_is_its_own() {
        let entries = [entry("b1", "open"), entry("b2", "append")];
        let standing = standing(&entries, run(&["open", "append"]));
        assert_eq!(standing.changes, ["b1", "b2"]);
        assert!(standing.over.is_none());
    }

    /// A change is the run's by the undo step history recorded for it under the run it names,
    /// where the host kept no invocation: never by a step recorded under another run.
    #[test]
    fn a_change_is_the_runs_by_its_recorded_step() {
        let step = tilo_actions::ActionId::new();
        let run = Run {
            step,
            invocations: BTreeSet::new(),
        };
        let recorded =
            tilo_actions::context::recorded_gesture(step, Some(("agent-run", "agent-run:1")));
        let ours = json!({"change": "b1", "step": recorded.0.to_string(),
                          "run_kind": "agent-run", "run_id": "agent-run:1"});
        assert!(run.made(&ours));
        let elsewhere = json!({"change": "b1", "step": recorded.0.to_string(),
                               "run_kind": "agent-run", "run_id": "agent-run:2"});
        assert!(!run.made(&elsewhere));
        assert!(!run.made(&json!({"change": "c1"})));
    }
}
