//! Tasks a workspace's room kept before the Tasks app, read once for the
//! tracker's `import`.
//!
//! The room document kept each task as a JSON blob under `tasks_v1` and its
//! instruction and messages under `task_control_v1`. Nothing writes either
//! any more; this reads just what an issue keeps of a task, tolerating
//! fields it does not know.

use automerge::{AutoCommit, ReadDoc, ScalarValue, Value};
use serde::Deserialize;
use tasks::model::{Imported, ImportedMessage, Status, agent_assignee};

const TASKS_KEY: &str = "tasks_v1";
const CONTROL_KEY: &str = "task_control_v1";

#[derive(Deserialize)]
struct Task {
    id: String,
    title: String,
    #[serde(default)]
    created_by: String,
    #[serde(default)]
    assigned_agent: Option<String>,
    status: TaskStatus,
    #[serde(default)]
    created_at: i64,
    #[serde(default)]
    audit: Vec<Event>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    Queued,
    InProgress,
    Waiting,
    Done,
    Cancelled,
}

#[derive(Deserialize)]
struct Event {
    at: i64,
    #[serde(default)]
    actor: String,
    what: tilo_actions::Did,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Default, Deserialize)]
struct Control {
    #[serde(default)]
    instruction: Option<String>,
    #[serde(default)]
    directions: Vec<Direction>,
}

#[derive(Deserialize)]
struct Direction {
    author: String,
    text: String,
    created_at: i64,
}

/// The agent's own words in a task's history: its progress and how it ended.
const AGENT_WORDS: [&str; 3] = ["public-progress", "finish", "abandon"];

/// Every task the room's document `doc` kept, as the tracker imports it.
/// A task whose record does not read is left out.
#[must_use]
pub(crate) fn room_tasks(doc: &AutoCommit) -> Vec<Imported> {
    let controls: std::collections::HashMap<String, Control> = blobs(doc, CONTROL_KEY).collect();
    let mut tasks: Vec<Imported> = blobs::<Task>(doc, TASKS_KEY)
        .map(|(_, task)| {
            let control = controls.get(&task.id);
            imported(task, control)
        })
        .collect();
    tasks.sort_by_key(|task| task.created_ms);
    tasks
}

fn imported(task: Task, control: Option<&Control>) -> Imported {
    let agent = task
        .assigned_agent
        .as_deref()
        .filter(|agent| !agent.trim().is_empty())
        .map(|agent| agent_assignee(agent).0);
    let mut messages: Vec<ImportedMessage> = control
        .into_iter()
        .flat_map(|control| &control.directions)
        .map(|direction| ImportedMessage {
            author: direction.author.clone(),
            by_agent: false,
            at_ms: seconds_to_ms(direction.created_at),
            text: direction.text.clone(),
        })
        .collect();
    messages.extend(task.audit.iter().filter_map(|event| {
        let tilo_actions::Did::Verb(verb) = &event.what else {
            return None;
        };
        let note = event
            .note
            .as_deref()
            .map(str::trim)
            .filter(|note| !note.is_empty())?;
        AGENT_WORDS
            .contains(&verb.action.action.as_str())
            .then(|| ImportedMessage {
                author: agent.clone().unwrap_or_else(|| event.actor.clone()),
                by_agent: true,
                at_ms: seconds_to_ms(event.at),
                text: note.to_owned(),
            })
    }));
    messages.sort_by_key(|message| message.at_ms);
    Imported {
        task: task.id,
        title: task.title,
        instruction: control
            .and_then(|control| control.instruction.clone())
            .unwrap_or_default(),
        // A run does not survive the move: an agent's task waits for a
        // person to approve it again.
        status: match task.status {
            TaskStatus::Queued | TaskStatus::InProgress | TaskStatus::Waiting => Status::Todo,
            TaskStatus::Done => Status::Done,
            TaskStatus::Cancelled => Status::Cancelled,
        },
        assignee: agent,
        author: task.created_by,
        created_ms: seconds_to_ms(task.created_at),
        messages,
    }
}

fn seconds_to_ms(seconds: i64) -> i64 {
    seconds.saturating_mul(1000)
}

/// The JSON blobs of the map `key` at the document's root that read as `T`,
/// by their key.
fn blobs<'a, T: serde::de::DeserializeOwned + 'a>(
    doc: &'a AutoCommit,
    key: &str,
) -> impl Iterator<Item = (String, T)> + 'a {
    let map = doc
        .get(automerge::ROOT, key)
        .ok()
        .flatten()
        .map(|(_, id)| id);
    map.into_iter().flat_map(move |map| {
        doc.keys(&map)
            .filter_map(|key| {
                let (Value::Scalar(value), _) = doc.get(&map, key.as_str()).ok().flatten()? else {
                    return None;
                };
                let ScalarValue::Bytes(blob) = value.as_ref() else {
                    return None;
                };
                Some((key.clone(), serde_json::from_slice(blob).ok()?))
            })
            .collect::<Vec<_>>()
    })
}

#[cfg(test)]
#[path = "room_task_import_tests.rs"]
mod tests;
