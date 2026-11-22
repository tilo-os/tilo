//! Taking back one invocation: exactly the changes its calls made, in every
//! document it changed, and nothing anyone else did.
//!
//! The workspace's server finds the invocation as it lists it
//! ([`tilo_actions::remote::workspace_invocations`]): by the step its changes
//! are kept under in each document's history, from any connection and across
//! restarts, among the documents the caller may read. Each document's own `undo`
//! takes back exactly that step (`step`), as the caller may change the document.
//! A change someone else made on top of the invocation's in a document refuses
//! the whole undo before anything is taken back: undoing under their work would
//! change what they see without their say. An undo of a later step is no such
//! change ([`edited_after`]).

use std::fmt::Write as _;

use super::watch::history_entries;
use super::{
    ActionProvider, Address, Addressed, Failure, McpServer, OneCall, Reply, Segment, Value, json,
};
use tilo_actions::remote::workspace_invocations::{
    InvocationChange, InvocationsQuery, edited_after,
};
use tilo_lock::MutexRecover as _;

/// How many of a document's changes are read to find an invocation's.
const HISTORY_READ: u64 = 200;
/// How long a record has to answer its history or its undo.
const RECORD_WAIT_MS: u64 = 10_000;
/// The most steps one document's share of an invocation is taken back in: its
/// changes in a document are one step unless someone else's changes fell between
/// them.
const MOST_STEPS: usize = 16;

/// One document an invocation changed, where its changes are taken back.
struct Target {
    /// What a caller names it by.
    path: String,
    app: String,
    /// The document's provider.
    provider: String,
    /// The resource there that offers the document's history verbs.
    history: String,
}

/// Whether the history `entry` lists is one of the changes of the invocation
/// whose step is `step`.
fn made(step: &str, entry: &Value) -> bool {
    entry.get("step").and_then(Value::as_str) == Some(step)
}

/// What a document's history says of an invocation.
struct Effect {
    /// Its changes still in effect, oldest first.
    changes: Vec<String>,
    /// Whose steps they are, as the document's `undo` takes it beside `step`.
    whose: serde_json::Map<String, Value>,
    /// Someone else's change made after its last, in effect: why it is not taken back.
    over: Option<Value>,
}

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// Take back the invocation `invocation` (one of its calls' ids, or its
    /// step): every change it made, in every document it changed, or nothing when
    /// someone else's change stands on one of them.
    pub(super) async fn undo_invocation(
        &self,
        invocation: &str,
        structured: bool,
    ) -> Result<Reply, Failure> {
        let (step, targets) = self.invocation_targets(invocation).await?;
        let mut plans = Vec::new();
        let mut refusals = Vec::new();
        for target in targets {
            let effect = self.effect(&target, &step).await?;
            if let Some(over) = &effect.over {
                refusals.push(refusal(&target.path, over));
            } else if !effect.changes.is_empty() {
                plans.push((target, effect.changes, effect.whose));
            }
        }
        if !refusals.is_empty() {
            return Err(format!(
                "invocation {invocation} is not taken back: {}. Ask whoever made those changes, or take \
                 back that record's step alone with `<path>#undo {{\"step\": <change>}}`, which \
                 keeps their work",
                refusals.join("; ")
            )
            .into());
        }
        if plans.is_empty() {
            return Err(format!(
                "invocation {invocation} has nothing left to take back: what it changed is undone"
            )
            .into());
        }
        let mut undone = Vec::new();
        let mut text = String::new();
        for (target, mut changes, whose) in plans {
            let mut taken = Vec::new();
            for _ in 0..MOST_STEPS {
                let Some(first) = changes.first().cloned() else {
                    break;
                };
                self.undo_step(&target, &first, whose.clone()).await?;
                taken.push(first);
                let left = self.effect(&target, &step).await?.changes;
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
                "app": target.app,
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

    /// Note that the call changed `app`, whose history verbs `history` offers: a call naming
    /// the group it is a step of makes that group's step too, which `undo task` takes back in
    /// every app it changed, as it takes back a program's.
    pub(super) fn note_change(&self, app: &str, history: Option<String>) {
        if let Some((gesture, label)) = Self::grouped_invocation() {
            self.programs
                .lock_recover()
                .note_invocation(gesture, &label, app, history);
        }
    }

    /// The step of the invocation `invocation` names, as the workspace's server
    /// lists it for this caller, and the documents it changed.
    async fn invocation_targets(&self, invocation: &str) -> Result<(String, Vec<Target>), String> {
        let listed = self
            .listed_invocations(&InvocationsQuery {
                invocation: Some(invocation.to_owned()),
                ..InvocationsQuery::default()
            })
            .await?;
        let found = listed.items.into_iter().next().ok_or_else(|| {
            format!(
                "invocation {invocation} changed nothing you may read: one is taken back once it \
                 changed a resource"
            )
        })?;
        let step = found.step.ok_or_else(|| {
            format!("invocation {invocation} changed no resource, so nothing of it is taken back")
        })?;
        let mut targets = Vec::new();
        for document in &found.records {
            targets.push(self.document_target(document).await?);
        }
        Ok((step, targets))
    }

    /// Where the changes to `document` are taken back: its document's provider,
    /// and the resource of it that offers `undo`.
    async fn document_target(&self, document: &InvocationChange) -> Result<Target, String> {
        let provider = format!(
            "{}:{}:{}",
            tilo_actions::remote::RESOURCE_PROVIDER,
            document.app_id,
            document.record
        );
        let parsed = Address {
            segments: vec![Segment::named(document.app.clone())],
            action: None,
        };
        let at = self
            .tree
            .resolve_in(
                &parsed,
                document.app_id.clone(),
                provider.clone(),
                true,
                None,
                &mut crate::agent::Levels::new(),
                usize::MAX,
            )
            .await?;
        // History is the document's: any of its resources that offers `undo` takes back its steps.
        let history = at
            .history()
            .or_else(|| {
                at.children
                    .iter()
                    .find(|node| {
                        node.actions
                            .iter()
                            .any(|action| action.address.action == "undo")
                    })
                    .map(|node| node.id.clone())
            })
            .ok_or_else(|| format!("{} offers no undo to take its changes back", document.app))?;
        Ok(Target {
            path: document.app.clone(),
            app: document.app_id.clone(),
            provider,
            history,
        })
    }

    /// What `target`'s history says of the invocation whose step is `step`.
    async fn effect(&self, target: &Target, step: &str) -> Result<Effect, String> {
        let mut arguments = serde_json::Map::new();
        arguments.insert("last".into(), json!(HISTORY_READ));
        let value = self.on_resource(target, "history", arguments).await?;
        let entries = history_entries(&value).ok_or_else(|| {
            format!(
                "{} answered no history to find the invocation in",
                target.path
            )
        })?;
        Ok(effect(&entries, |entry| made(step, entry)))
    }

    /// Take back the step of `target`'s document that the change `change` is part of, of the
    /// steps `whose` names.
    async fn undo_step(
        &self,
        target: &Target,
        change: &str,
        mut arguments: serde_json::Map<String, Value>,
    ) -> Result<(), String> {
        arguments.insert("step".into(), json!(change));
        let value = self.on_resource(target, "undo", arguments).await?;
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
    async fn on_resource(
        &self,
        target: &Target,
        action: &str,
        arguments: serde_json::Map<String, Value>,
    ) -> Result<Value, String> {
        Box::pin(self.invoke_one(OneCall {
            provider: Addressed {
                provider: target.provider.clone(),
                app_id: Some(target.app.clone()),
            },
            workspace: false,
            revision: None,
            resource: target.history.clone(),
            action: action.to_owned(),
            arguments,
            invocation_id: None,
            wait_ms: Some(RECORD_WAIT_MS),
        }))
        .await
        .map_err(|why| format!("in {}: {why}", target.path))
    }
}

/// What a document's history `entries` (oldest first, each whole) say of the invocation whose changes
/// `of_invocation` tells: its changes still in effect, and the change that holds its undo back.
fn effect(entries: &[Value], of_invocation: impl Fn(&Value) -> bool) -> Effect {
    let in_effect = |entry: &&Value| entry.get("undone").and_then(Value::as_bool) != Some(true);
    let changes: Vec<String> = entries
        .iter()
        .filter(in_effect)
        .filter(|entry| of_invocation(entry))
        .filter_map(|entry| {
            entry
                .get("change")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    let over = edited_after(entries, &of_invocation).cloned();
    let whose = entries
        .iter()
        .find(|entry| of_invocation(entry))
        .map(whose)
        .unwrap_or_default();
    Effect {
        changes,
        whose,
        over,
    }
}

/// Whose steps the change the history `entry` lists is, as `undo` names them: the session it
/// was a step of (`invocation`), or the author outside one (`by`). Neither for a change that
/// names no invoker, which only its own writer takes back.
fn whose(entry: &Value) -> serde_json::Map<String, Value> {
    let mut whose = serde_json::Map::new();
    let field = |key: &str| entry.get(key).and_then(Value::as_str);
    if let (Some(group), Some(kind)) = (field("group"), field("group_kind"))
        && tilo_actions::context::names_a_session(kind)
    {
        whose.insert("invocation".into(), json!(group));
    } else if let Some(by) = entry.pointer("/actor/by") {
        whose.insert("by".into(), by.clone());
    }
    whose
}

/// Why the invocation is not taken back in the document at `path`: `over`, the change
/// over it.
fn refusal(path: &str, over: &Value) -> String {
    let who = over
        .get("title")
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
    use std::collections::BTreeSet;

    fn entry(change: &str, invocation: &str) -> Value {
        json!({"change": change, "invocation": invocation, "sentence": "Added a paragraph", "by": "Ada"})
    }

    fn named(invocations: &[&str]) -> impl Fn(&Value) -> bool {
        let invocations: BTreeSet<String> = invocations.iter().map(|id| (*id).to_owned()).collect();
        move |entry: &Value| {
            entry
                .get("invocation")
                .and_then(Value::as_str)
                .is_some_and(|id| invocations.contains(id))
        }
    }

    tilo_test_env::scenario! {
        fn an_invocation_takes_back_only_its_own_changes_in_effect;

        phase an_invocation_finds_its_own_changes_in_effect;
        {
            let entries = [
                entry("a1", "before"),
                entry("b1", "run"),
                json!({"change": "b2", "invocation": "run", "undone": true}),
                entry("b3", "run"),
            ];
            let effect = effect(&entries, named(&["run"]));
            assert_eq!(effect.changes, ["b1", "b3"]);
            assert!(effect.over.is_none());
        }

        phase someone_elses_change_on_top_holds_the_invocation;
        {
            let entries = [entry("b1", "run"), entry("c1", "theirs")];
            let held = effect(&entries, named(&["run"]));
            assert_eq!(held.over.as_ref().unwrap()["change"], "c1");
            assert_eq!(
                refusal("word-processor", held.over.as_ref().unwrap()),
                "in word-processor, Ada Added a paragraph (c1) after it"
            );
            // What they made before the invocation, what they took back since, and
            // housekeeping stand on nothing of its.
            let entries = [
                entry("a1", "theirs"),
                entry("b1", "run"),
                json!({"change": "c1", "invocation": "theirs", "undone": true}),
                json!({"change": "c2", "change_kind": "housekeeping"}),
            ];
            assert!(effect(&entries, named(&["run"])).over.is_none());
        }

        phase every_call_of_a_named_invocation_is_its_own;
        {
            let entries = [entry("b1", "open"), entry("b2", "append")];
            let effect = effect(&entries, named(&["open", "append"]));
            assert_eq!(effect.changes, ["b1", "b2"]);
            assert!(effect.over.is_none());
        }

        phase a_change_is_the_invocations_by_its_step;
        {
            assert!(made("s1", &json!({"change": "b1", "step": "s1"})));
            assert!(!made("s1", &json!({"change": "b1", "step": "s2"})));
            assert!(!made("s1", &json!({"change": "c1"})));
        }
    }
}
