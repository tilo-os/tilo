//! The resource's history as actions every framework app answers, so the
//! people, scripts and agents working on it can look back, compare and
//! take back through the same door they edit through:
//!
//! - `undo` / `redo`: the caller's own last steps (an agent's are those of
//!   its run), or a named agent run's;
//! - `restore`: a past version, whole or only some items, as a new step;
//! - `checkpoint`: name the current version;
//! - `history`: every step with what it was given, who made it, in which
//!   run, and which are undone or checkpointed;
//! - `diff`: the items that differ between two versions.
//!
//! Versions are change ids from `history` or checkpoint names. The verbs
//! are [`ui_core::history_source::steps`]; an app with a richer comparison
//! of its own (a picture of what changed) publishes its own `diff`, which
//! takes the place of this one.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_actions::{
    ActionArgs, ActionError, ActionOutput, ActionParam, ActionSpec, ChangeKind, EncodedAction,
    ExecutionLocus, OperationKind, ParamType, PermissionRequirement, ResultSpec,
};
use tilo_resource::history::Version;
use ui_core::automerge::{AutoCommit, ChangeHash, Patch, PatchAction, Prop};
use ui_core::history_source::steps::{self, Checkpoint, Scope};

/// The registry type of the history verbs that change the document.
pub(crate) const EDIT_TYPE: &str = "tilo.app.history-edit";
/// The operation type of the history views.
pub(crate) const VIEW_TYPE: &str = "tilo.app.history";

/// The most steps one undo or redo takes.
const MAX_STEPS: i64 = 50;
/// The most versions `history` lists.
const MAX_LISTED: i64 = 500;

/// A history verb that writes the document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum HistoryEdit {
    Undo {
        steps: Option<u32>,
        run: Option<String>,
    },
    Redo {
        steps: Option<u32>,
    },
    Restore {
        version: String,
        ids: Option<Vec<String>>,
    },
    Checkpoint {
        name: String,
    },
}

fn limit(steps: Option<u32>, default: usize) -> usize {
    steps.map_or(default, |steps| usize::try_from(steps).unwrap_or(1))
}

impl tilo_actions::Action for HistoryEdit {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::Undo { steps, run } => {
                let (scope, default) = match run {
                    Some(run) => (Scope::Run(run.clone()), usize::MAX),
                    None => (Scope::invoking(doc), 1),
                };
                steps::undo(doc, &scope, limit(*steps, default)).map(drop)
            }
            Self::Redo { steps } => {
                let scope = Scope::invoking(doc);
                steps::redo(doc, &scope, limit(*steps, 1)).map(drop)
            }
            Self::Restore { version, ids } => steps::resolve(doc, version).and_then(|heads| {
                steps::restore(doc, &heads, ids.as_deref().unwrap_or_default()).map(drop)
            }),
            Self::Checkpoint { name } => steps::checkpoint(doc, name),
        }
        .map_err(ActionError::Refused)
    }

    fn change_kind(&self) -> ChangeKind {
        match self {
            Self::Checkpoint { .. } => ChangeKind::Housekeeping,
            _ => ChangeKind::Content,
        }
    }
}

/// A history view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
enum HistoryView {
    History { last: Option<u32> },
    Diff { from: String, to: Option<String> },
}

fn invalid(reason: &str) -> EncodedAction {
    EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
        payload: reason.as_bytes().to_vec(),
    }
}

fn steps_arg(args: &ActionArgs) -> Result<Option<u32>, String> {
    args.integer("steps")
        .map(|steps| {
            if (1..=MAX_STEPS).contains(&steps) {
                u32::try_from(steps).map_err(|error| error.to_string())
            } else {
                Err(format!("steps is 1 to {MAX_STEPS}"))
            }
        })
        .transpose()
}

fn text_arg(args: &ActionArgs, name: &str) -> Result<String, String> {
    args.text(name)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{name} is required"))
}

fn edit(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    params: Vec<ActionParam>,
    build: fn(&ActionArgs) -> Result<HistoryEdit, String>,
) -> ActionSpec {
    let mut spec = ActionSpec::typed(
        id,
        label,
        label,
        ExecutionLocus::ClientLocal,
        params,
        move |args| match build(args) {
            Ok(edit) => EncodedAction::encode(EDIT_TYPE, &edit)
                .unwrap_or_else(|error| invalid(&error.to_string())),
            Err(reason) => invalid(&reason),
        },
    )
    .requiring(PermissionRequirement::Write)
    .reversible()
    .with_description(description);
    spec.contract.result = ResultSpec::Json {
        schema: json!({"type": "object"}),
    };
    spec
}

fn view(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    params: Vec<ActionParam>,
    build: fn(&ActionArgs) -> HistoryView,
) -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        id,
        label,
        label,
        OperationKind::View,
        VIEW_TYPE,
        params,
        move |args| {
            EncodedAction::encode_operation(VIEW_TYPE, &build(args)).unwrap_or_else(|_| {
                EncodedAction {
                    type_id: VIEW_TYPE.into(),
                    payload: Vec::new(),
                }
            })
        },
    )
    .requiring(PermissionRequirement::Read)
    .with_description(description);
    spec.contract.result = ResultSpec::Json {
        schema: json!({"type": "object"}),
    };
    spec
}

const VERSION: &str = "A change id from history, or a checkpoint name";

/// The history actions every framework app publishes, unless its resource
/// already publishes one of the same id.
pub(crate) fn specs() -> Vec<ActionSpec> {
    let mut specs = edit_specs();
    specs.extend(view_specs());
    specs
}

fn edit_specs() -> Vec<ActionSpec> {
    let steps = |description: &'static str| {
        ActionParam::optional("steps", ParamType::Integer, description).nullable()
    };
    vec![
        edit(
            "undo",
            "Undo",
            "Take back your last steps (an agent: its run's) as a new change; with run, that agent run's steps.",
            vec![
                steps("How many steps, newest first"),
                ActionParam::optional("run", ParamType::Text, "An agent run id from history")
                    .nullable(),
            ],
            |args| {
                Ok(HistoryEdit::Undo {
                    steps: steps_arg(args)?,
                    run: args.text("run").map(str::to_owned),
                })
            },
        ),
        edit(
            "redo",
            "Redo",
            "Bring back what your last undo took back.",
            vec![steps("How many undos, newest first")],
            |args| {
                Ok(HistoryEdit::Redo {
                    steps: steps_arg(args)?,
                })
            },
        ),
        edit(
            "restore",
            "Restore version",
            "Bring the document back to a version as a new change, or only the items named by ids.",
            vec![
                ActionParam::required("version", ParamType::Text, VERSION),
                ActionParam::optional("ids", ParamType::Array, "Only these item ids")
                    .nullable()
                    .with_schema(json!({"type": "array", "items": {"type": "string"}})),
            ],
            |args| {
                let ids = args
                    .array("ids")
                    .map(|ids| {
                        ids.iter()
                            .map(|id| id.as_str().map(str::to_owned))
                            .collect::<Option<Vec<_>>>()
                            .ok_or_else(|| "ids are strings".to_owned())
                    })
                    .transpose()?;
                Ok(HistoryEdit::Restore {
                    version: text_arg(args, "version")?,
                    ids,
                })
            },
        ),
        edit(
            "checkpoint",
            "Checkpoint",
            "Name the current version, to restore or compare with later.",
            vec![ActionParam::required("name", ParamType::Text, "The name")],
            |args| {
                Ok(HistoryEdit::Checkpoint {
                    name: text_arg(args, "name")?,
                })
            },
        ),
    ]
}

fn view_specs() -> Vec<ActionSpec> {
    vec![
        view(
            "history",
            "History",
            "List the versions, oldest first: change id, action and its arguments, run, author, time, undone, checkpoints.",
            vec![
                ActionParam::optional(
                    "last",
                    ParamType::Integer,
                    "How many recent versions (default 50)",
                )
                .nullable(),
            ],
            |args| HistoryView::History {
                last: args
                    .integer("last")
                    .and_then(|last| u32::try_from(last.clamp(1, MAX_LISTED)).ok()),
            },
        ),
        view(
            "diff",
            "Compare versions",
            "List the items that differ between two versions.",
            vec![
                ActionParam::required("from", ParamType::Text, VERSION),
                ActionParam::optional("to", ParamType::Text, "The later version (default now)")
                    .nullable(),
            ],
            |args| HistoryView::Diff {
                from: args.text("from").unwrap_or_default().to_owned(),
                to: args.text("to").map(str::to_owned),
            },
        ),
    ]
}

/// Whether `action` is a history verb that writes the document.
pub(crate) fn is_edit(action: &str) -> bool {
    matches!(action, "undo" | "redo" | "restore" | "checkpoint")
}

/// A version's name in an answer: its change id, or several for a version
/// with concurrent heads.
pub fn version_name(heads: &[ChangeHash]) -> String {
    heads.iter().map(steps::short).collect::<Vec<_>>().join("+")
}

fn by(version: &Version) -> String {
    version
        .context
        .as_ref()
        .and_then(|context| context.invoker.as_ref())
        .map(|invoker| invoker.actor.clone())
        .or_else(|| ui_core::history_source::authors::actor_handle(&version.actor))
        .unwrap_or_else(|| version.actor.chars().take(8).collect())
}

/// One version as `history` lists it.
pub(crate) fn entry(
    version: &Version,
    undone: &HashSet<String>,
    checkpoints: &[Checkpoint],
) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert("change".into(), steps::short(&version.hash).into());
    let context = version.context.as_ref();
    if let Some(action) = context
        .map(|context| context.action_label.clone())
        .filter(|label| !label.is_empty())
        .or_else(|| version.summary())
    {
        entry.insert("action".into(), action.into());
    }
    if let Some(args) = context.and_then(tilo_actions::ActionContext::args) {
        entry.insert("args".into(), args.into());
    }
    if let Some(group) = context.and_then(|context| context.group.as_ref()) {
        entry.insert("run".into(), group.label.clone().into());
        entry.insert("run_id".into(), group.id.clone().into());
    } else {
        entry.insert("by".into(), by(version).into());
    }
    entry.insert("time_ms".into(), version.time_ms.into());
    if undone.contains(&version.hash.to_string()) {
        entry.insert("undone".into(), true.into());
    }
    let named: Vec<&str> = checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.heads.contains(&version.hash))
        .map(|checkpoint| checkpoint.name.as_str())
        .collect();
    if !named.is_empty() {
        entry.insert("checkpoint".into(), json!(named));
    }
    Value::Object(entry)
}

/// One version of the resource as an in-app history list shows it: what
/// was done, by whom, and whether it was taken back or named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryVersion {
    /// The change id: what `restore`, `diff` and a view's `version` take.
    pub change: String,
    /// What was done: the action's label.
    pub action: String,
    /// Who did it: the person's handle, or the agent run's label.
    pub author: String,
    /// The agent run that made it, when an agent did: what `undo` takes as
    /// `run` to take the whole run back.
    pub run: Option<String>,
    /// Milliseconds since the Unix epoch; 0 when unknown.
    pub time_ms: i64,
    /// Taken back by an undo.
    pub undone: bool,
    /// The names of the checkpoints at this version.
    pub checkpoints: Vec<String>,
    /// Housekeeping about the document (naming a checkpoint), not a change
    /// to what it holds.
    pub housekeeping: bool,
}

/// The kind of work unit an agent's run groups its actions under.
const AGENT_RUN: &str = "agent-run";

/// The last `last` versions, oldest first, as [`HistoryVersion`]s.
pub(crate) fn recent(doc: &mut AutoCommit, last: usize) -> Vec<HistoryVersion> {
    let versions = tilo_resource::history::versions(doc);
    let undone = steps::undone(doc);
    let checkpoints = steps::checkpoints(doc);
    let total = versions.len();
    versions
        .iter()
        .skip(total.saturating_sub(last))
        .filter(|version| !steps::is_schema_genesis(version))
        .map(|version| {
            let context = version.context.as_ref();
            let run = context
                .and_then(|context| context.group.as_ref())
                .filter(|group| group.kind == AGENT_RUN);
            HistoryVersion {
                change: steps::short(&version.hash),
                action: context
                    .map(|context| context.action_label.clone())
                    .filter(|label| !label.is_empty())
                    .or_else(|| version.summary())
                    .unwrap_or_default(),
                author: run.map_or_else(|| by(version), |run| run.label.clone()),
                run: run.map(|run| run.id.clone()),
                time_ms: version.time_ms,
                undone: undone.contains(&version.hash.to_string()),
                checkpoints: checkpoints
                    .iter()
                    .filter(|checkpoint| checkpoint.heads.contains(&version.hash))
                    .map(|checkpoint| checkpoint.name.clone())
                    .collect(),
                housekeeping: context.is_some_and(tilo_actions::ActionContext::is_housekeeping),
            }
        })
        .collect()
}

/// The history lists views follow ([`ViewCx::history`](super::ViewCx::history)),
/// brought up to date when the document moves.
#[derive(Clone, Default)]
pub(crate) struct HistoryFeeds(std::rc::Rc<std::cell::RefCell<FeedList>>);

type Feed = (
    std::rc::Weak<()>,
    usize,
    ui_core::LocalState<Vec<HistoryVersion>>,
);

#[derive(Default)]
struct FeedList {
    feeds: Vec<Feed>,
    /// The heads the feeds last showed; empty forces a refresh.
    heads: Vec<ChangeHash>,
}

impl HistoryFeeds {
    pub(crate) fn follow(
        &self,
        alive: std::rc::Weak<()>,
        last: usize,
    ) -> ui_core::LocalState<Vec<HistoryVersion>> {
        let state = ui_core::LocalState::new(Vec::new());
        let mut registry = self.0.borrow_mut();
        registry.feeds.push((alive, last, state.clone()));
        registry.heads.clear();
        state
    }

    /// Show the versions up to `doc`'s heads, when they moved.
    pub(crate) fn refresh(&self, doc: &mut AutoCommit) {
        let mut list = self.0.borrow_mut();
        list.feeds.retain(|(alive, _, _)| alive.strong_count() > 0);
        if list.feeds.is_empty() {
            return;
        }
        let heads = doc.get_heads();
        if list.heads == heads {
            return;
        }
        let most = list
            .feeds
            .iter()
            .map(|(_, last, _)| *last)
            .max()
            .unwrap_or(0);
        let versions = recent(doc, most);
        for (_, last, state) in &list.feeds {
            let shown = versions
                .get(versions.len().saturating_sub(*last)..)
                .unwrap_or_default()
                .to_vec();
            if state.with(|current| *current != shown) {
                state.set(shown);
            }
        }
        list.heads = heads;
    }
}

/// Leaves out of each version the author and time it shares with the one
/// before: a run of forty steps names its run once, not forty times.
fn carry_over(listed: &mut [Value]) {
    let mut before = serde_json::Map::new();
    for entry in listed {
        let Value::Object(fields) = entry else {
            continue;
        };
        let same_author = ["run_id", "by"].iter().any(|key| {
            fields
                .get(*key)
                .is_some_and(|value| before.get(*key) == Some(value))
        });
        let author: Vec<&str> = ["run", "run_id", "by"]
            .into_iter()
            .filter(|key| fields.contains_key(*key))
            .collect();
        let time = fields.get("time_ms").cloned();
        let shown = std::mem::take(&mut before);
        for key in &author {
            if let Some(value) = fields.get(*key) {
                before.insert((*key).to_owned(), value.clone());
            }
        }
        if same_author {
            for key in &author {
                fields.remove(*key);
            }
        }
        if let Some(time) = time {
            if shown.get("time_ms") == Some(&time) {
                fields.remove("time_ms");
            }
            before.insert("time_ms".into(), time);
        }
    }
}

/// The versions as `history` lists them: the last `last`, oldest first.
pub(crate) fn history(doc: &mut AutoCommit, last: usize) -> Value {
    let versions = tilo_resource::history::versions(doc);
    let undone = steps::undone(doc);
    let checkpoints = steps::checkpoints(doc);
    let total = versions.len();
    let mut listed: Vec<Value> = versions
        .iter()
        .skip(total.saturating_sub(last))
        .filter(|version| !steps::is_schema_genesis(version))
        .map(|version| entry(version, &undone, &checkpoints))
        .collect();
    carry_over(&mut listed);
    json!({
        "versions": listed,
        "carried": "run, run_id, by and time_ms, where absent, are the version before's",
        "total": total,
        "now": version_name(&doc.get_heads()),
        "checkpoints": checkpoints.iter().map(|checkpoint| json!({
            "name": checkpoint.name,
            "version": version_name(&checkpoint.heads),
        })).collect::<Vec<_>>(),
    })
}

/// Answer a history view.
///
/// # Errors
/// Returns a malformed request or an unknown version.
pub(crate) fn answer(doc: &mut AutoCommit, payload: &[u8]) -> Result<ActionOutput, String> {
    let view: HistoryView =
        serde_json::from_slice(payload).map_err(|error| format!("not a history view: {error}"))?;
    let value = match view {
        HistoryView::History { last } => history(doc, limit(last, 50)),
        HistoryView::Diff { from, to } => {
            let from = steps::resolve(doc, &from)?;
            let to = match to {
                Some(to) => steps::resolve(doc, &to)?,
                None => doc.get_heads(),
            };
            let items: Vec<Value> = steps::changed_items(doc, &from, &to)
                .into_iter()
                .map(|item| json!({"item": item.path, "change": item.change}))
                .collect();
            json!({
                "from": version_name(&from),
                "to": version_name(&to),
                "items": items,
            })
        }
    };
    Ok(ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    })
}

/// The root keys `patches` put (`true`) or delete under `prefix`, without it.
fn ledger(patches: &[Patch], prefix: &str, put: bool) -> Vec<String> {
    patches
        .iter()
        .filter(|patch| patch.path.is_empty())
        .filter_map(|patch| match (&patch.action, put) {
            (PatchAction::PutMap { key, .. }, true) | (PatchAction::DeleteMap { key }, false) => {
                key.strip_prefix(prefix).map(str::to_owned)
            }
            _ => None,
        })
        .collect()
}

/// The items `patches` wrote: root keys, or `map/id` entries.
fn items(patches: &[Patch]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for patch in patches.iter().filter(|patch| !steps::is_bookkeeping(patch)) {
        let mut keys = patch
            .path
            .iter()
            .filter_map(|(_, prop)| match prop {
                Prop::Map(key) => Some(key.clone()),
                Prop::Seq(_) => None,
            })
            .chain(match &patch.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    Some(key.clone())
                }
                _ => None,
            });
        let item = match (keys.next(), keys.next()) {
            (Some(map), Some(id)) => format!("{map}/{id}"),
            (Some(key), None) => key,
            (None, _) => continue,
        };
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// What a history verb did, answered to its caller: the steps it took back
/// or brought back, the items a restore wrote, the checkpoint it named.
pub(crate) fn result(action: &str, doc: &AutoCommit, patches: &[Patch]) -> Option<Value> {
    let mut doc = doc.clone();
    let now = version_name(&doc.get_heads());
    let described = |hashes: Vec<String>, doc: &mut AutoCommit| -> Vec<Value> {
        let versions = tilo_resource::history::versions(doc);
        versions
            .iter()
            .rev()
            .filter(|version| hashes.contains(&version.hash.to_string()))
            .map(|version| {
                let context = version.context.as_ref();
                json!({
                    "change": steps::short(&version.hash),
                    "action": context.map(|context| context.action_label.clone()),
                    "args": context.and_then(tilo_actions::ActionContext::args),
                })
            })
            .collect()
    };
    let value = match action {
        "undo" => {
            json!({"undone": described(ledger(patches, steps::UNDONE_KEY, true), &mut doc), "now": now})
        }
        "redo" => {
            json!({"redone": described(ledger(patches, steps::UNDONE_KEY, false), &mut doc), "now": now})
        }
        "restore" => json!({"restored": items(patches), "now": now}),
        "checkpoint" => {
            let name = ledger(patches, steps::CHECKPOINT_KEY, true);
            let version = steps::checkpoints(&doc)
                .into_iter()
                .find(|checkpoint| name.contains(&checkpoint.name))
                .map(|checkpoint| version_name(&checkpoint.heads));
            json!({"checkpoint": name.first(), "version": version})
        }
        _ => return None,
    };
    Some(value)
}
