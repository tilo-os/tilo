//! The resource's history as actions every app answers, native or
//! tilolang, so the people, scripts and agents working on it can look back,
//! compare and take back through the same door they edit through:
//!
//! - `undo` / `redo`: the caller's own last steps (an agent's are those of
//!   its run), or a named agent run's;
//! - `restore`: a past version, whole or only some items, as a new step;
//! - `review-restore` / `restore-reviewed`: pin a causal review and restore
//!   exact differing paths only if the live frontier still matches;
//! - `checkpoint`: name the current version;
//! - `history`: every step with what it was given, who made it, in which
//!   run, and which are undone or checkpointed;
//! - `diff`: the items that differ between two versions.
//!
//! Versions are change ids from `history` or checkpoint names. The verbs
//! are [`steps`]; an app with a richer comparison of its own (a picture of
//! what changed) publishes its own `diff`, which takes the place of this one.
//!
//! What is said in the reader's words (a step's sentence, a person's name)
//! comes from the caller's [`HistoryWords`]: the toolkit's on an app's
//! thread, the platform's in a process without the toolkit. Everything else
//! is here, so every kind of app publishes and answers the same verbs.

pub mod review;

use std::collections::HashSet;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash, Patch, PatchAction, Prop};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_actions::{
    ActionArgs, ActionError, ActionKind, ActionOutput, ActionParam, ActionSpec, ChangeKind,
    EncodedAction, ExecutionLocus, OperationKind, ParamType, ResultSpec,
};
use tilo_resource::history::Version;

use super::steps::{self, Checkpoint, Scope};
use super::words::HistoryWords;

/// The registry type of the history verbs that change the document.
pub const EDIT_TYPE: &str = "tilo.app.history-edit";
/// The type of a history verb on one part of a chain ([`PartEdit`]).
pub const PART_EDIT_TYPE: &str = "tilo.app.part-history-edit";
pub const VIEW_TYPE: &str = "tilo.app.history";

/// The most steps one undo or redo takes.
const MAX_STEPS: i64 = 50;
/// The most versions `history` lists.
const MAX_LISTED: i64 = 500;

/// A history verb that writes the document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HistoryEdit {
    Undo {
        steps: Option<u32>,
        run: Option<String>,
        /// An exact author id from history for steps outside a run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
        /// Every step after this version instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
        /// Only the step this change id from history is part of, wherever it
        /// stands, instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<String>,
    },
    Redo {
        steps: Option<u32>,
    },
    Restore {
        version: String,
        ids: Option<Vec<String>>,
    },
    RestoreReviewed {
        review: review::RestoreReview,
    },
    Checkpoint {
        name: String,
    },
}

fn limit(steps: Option<u32>, default: usize) -> usize {
    steps.map_or(default, |steps| usize::try_from(steps).unwrap_or(1))
}

impl HistoryEdit {
    /// The verb it is, as it is published.
    #[must_use]
    pub const fn verb(&self) -> &'static str {
        match self {
            Self::Undo { .. } => "undo",
            Self::Redo { .. } => "redo",
            Self::Restore { .. } => "restore",
            Self::RestoreReviewed { .. } => "restore-reviewed",
            Self::Checkpoint { .. } => "checkpoint",
        }
    }

    /// A checkpoint is housekeeping; any other move through history is a
    /// step of its own.
    #[must_use]
    pub fn change_kind(&self) -> ChangeKind {
        match self {
            Self::Checkpoint { .. } => ChangeKind::Housekeeping,
            _ => ChangeKind::Content,
        }
    }

    /// What history records this step through history did: for an undo of
    /// one step, the step it takes back.
    #[must_use]
    pub fn described(&self, doc: &tilo_actions::Doc) -> tilo_actions::Described {
        use tilo_actions::HistoryOp;
        let (op, steps) = match self {
            Self::Undo {
                steps,
                run: None,
                to: None,
                step: None,
                ..
            } => (HistoryOp::Undo, steps.unwrap_or(1)),
            Self::Undo { step: Some(_), .. } => (HistoryOp::Undo, 1),
            Self::Undo { steps, .. } => (HistoryOp::Undo, steps.unwrap_or(0)),
            Self::Redo { steps } => (HistoryOp::Redo, steps.unwrap_or(1)),
            Self::Restore { .. } | Self::RestoreReviewed { .. } => (HistoryOp::Restore, 0),
            Self::Checkpoint { name } => (HistoryOp::Checkpoint { name: name.clone() }, 0),
        };
        let of = match self {
            Self::Undo {
                run: None,
                to: None,
                by,
                ..
            } if steps == 1 => {
                let scope = by.as_ref().map_or_else(
                    || Scope::invoking(doc),
                    |author| Scope::Invoker(author.clone()),
                );
                // A gesture's changes share one description; the newest
                // names the step.
                steps::undo_preview(doc, &scope, 1)
                    .first()
                    .and_then(|version| version.context.as_ref())
                    .and_then(named_step)
            }
            Self::Redo { .. } if steps == 1 => {
                let scope = Scope::invoking(doc);
                steps::undos_in_effect(doc, &|version| scope.owns(version), 1)
                    .first()
                    .and_then(|version| version.context.as_ref())
                    .and_then(named_step)
            }
            _ => None,
        };
        tilo_actions::Described::new(tilo_actions::Did::history(op, steps, of))
    }
}

/// What a step is called: an undo or redo is called by the step it moved.
fn named_step(context: &tilo_actions::ActionContext) -> Option<tilo_actions::Described> {
    match &context.what {
        Some(tilo_actions::Did::History(step))
            if matches!(
                step.op,
                tilo_actions::HistoryOp::Undo | tilo_actions::HistoryOp::Redo
            ) =>
        {
            step.of.as_deref().cloned()
        }
        _ => context.described(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum HistoryView {
    History { last: Option<u32> },
    Diff { from: String, to: Option<String> },
    ReviewRestore { version: String, paths: Vec<String> },
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
        // Decoded by the resource's own history edit, which applies it.
        move |args| match build(args)
            .and_then(|edit| serde_json::to_vec(&edit).map_err(|error| error.to_string()))
        {
            Ok(payload) => EncodedAction {
                type_id: EDIT_TYPE.to_owned(),
                payload,
            },
            Err(reason) => invalid(&reason),
        },
    )
    .of_kind(tilo_actions::ActionKind::Undoable)
    .with_description(description);
    spec.type_mut().contract.result = ResultSpec::Json {
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
    .of_kind(ActionKind::Read)
    .with_description(description);
    spec.type_mut().contract.result = ResultSpec::Json {
        schema: json!({"type": "object"}),
    };
    spec
}

const VERSION: &str = "A change id from history, or a checkpoint name";

/// The history actions every framework app publishes, unless its resource
/// already publishes one of the same id.
pub fn specs() -> Vec<ActionSpec> {
    let mut specs = edit_specs();
    specs.extend(view_specs());
    specs.extend(review::specs());
    for spec in &mut specs {
        if names_a_version(&spec.id) {
            spec.type_mut()
                .contract
                .set_audience(tilo_actions::Audience::Agents);
        }
    }
    specs
}

/// Whether the history action `id` names a version by its change id, which
/// only history shows: agents and scripts list the versions and take one
/// from there, a person goes back to one in the app's own history. Undo,
/// redo and naming a checkpoint ask for nothing a person cannot say.
fn names_a_version(id: &str) -> bool {
    matches!(id, "restore" | "history" | "diff")
}

fn edit_specs() -> Vec<ActionSpec> {
    edit_specs_with(edit)
}

/// How a history verb's arguments build its edit.
type BuildEdit = fn(&ActionArgs) -> Result<HistoryEdit, String>;

/// How a history verb is published: its id, label, description,
/// parameters, and how they build the edit.
type MakeEdit =
    fn(&'static str, &'static str, &'static str, Vec<ActionParam>, BuildEdit) -> ActionSpec;

/// The history verbs that change one part of a chain, each naming the part
/// (`part`): what a chain publishes beside its parts' own actions, so the
/// people, agents and the room's moderation take back steps in a part
/// through the same door as in any record.
pub fn part_edit_specs() -> Vec<ActionSpec> {
    edit_specs_with(part_edit)
}

/// A history verb on part `part` of a chain.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartEdit {
    pub part: u64,
    pub edit: HistoryEdit,
}

impl PartEdit {
    /// The verb it runs, as it is published.
    #[must_use]
    pub const fn verb(&self) -> &'static str {
        self.edit.verb()
    }

    pub fn encoded(&self) -> Result<EncodedAction, String> {
        Ok(EncodedAction {
            type_id: EDIT_TYPE.to_owned(),
            payload: serde_json::to_vec(&self.edit).map_err(|error| error.to_string())?,
        })
    }
}

fn part_edit(
    id: &'static str,
    label: &'static str,
    description: &'static str,
    mut params: Vec<ActionParam>,
    build: BuildEdit,
) -> ActionSpec {
    params.push(ActionParam::required(
        "part",
        ParamType::Integer,
        "The number of the part whose history it changes",
    ));
    let mut spec = ActionSpec::typed(
        id,
        label,
        label,
        ExecutionLocus::ClientLocal,
        params,
        move |args| {
            let part = args
                .integer("part")
                .and_then(|part| u64::try_from(part).ok())
                .ok_or_else(|| "part is a part's number".to_owned());
            match part
                .and_then(|part| {
                    Ok(PartEdit {
                        part,
                        edit: build(args)?,
                    })
                })
                .and_then(|edit| serde_json::to_vec(&edit).map_err(|error| error.to_string()))
            {
                Ok(payload) => EncodedAction {
                    type_id: PART_EDIT_TYPE.to_owned(),
                    payload,
                },
                Err(reason) => invalid(&reason),
            }
        },
    )
    .of_kind(tilo_actions::ActionKind::Undoable)
    .with_description(description)
    // A part is named by its number, which only history shows: agents and
    // scripts take it from there. A person undoes in the app itself.
    .for_audience(tilo_actions::Audience::Agents);
    spec.type_mut().contract.result = ResultSpec::Json {
        schema: json!({"type": "object"}),
    };
    spec
}

fn edit_specs_with(edit: MakeEdit) -> Vec<ActionSpec> {
    let steps = |description: &'static str| {
        ActionParam::optional("steps", ParamType::Integer, description).nullable()
    };
    vec![
        edit(
            "undo",
            "Undo",
            "Take back your last steps (an agent: its run's) as a new change; with run, that agent run's steps; with by, that author's steps outside a run; with to, all after a version.",
            vec![
                steps("How many steps, newest first"),
                ActionParam::optional("run", ParamType::Text, "An agent run id from history")
                    .nullable(),
                ActionParam::optional(
                    "by",
                    ParamType::Text,
                    "Exact author id from history, for steps outside a run",
                )
                .nullable(),
                ActionParam::optional("to", ParamType::Text, VERSION).nullable(),
                ActionParam::optional(
                    "step",
                    ParamType::Text,
                    "A change id from history: undo only the step it is part of",
                )
                .nullable(),
            ],
            |args| {
                let to = args.text("to").map(str::to_owned);
                let step = args.text("step").map(str::to_owned);
                if to.is_some() && args.integer("steps").is_some() {
                    return Err("give steps or to, not both".into());
                }
                if step.is_some()
                    && (to.is_some()
                        || args.integer("steps").is_some()
                        || args.text("run").is_some())
                {
                    return Err("give step alone".into());
                }
                let by = args.text("by").map(str::trim).map(str::to_owned);
                if by.as_ref().is_some_and(|author| {
                    author.is_empty() || author.len() > 512 || author.chars().any(char::is_control)
                }) {
                    return Err(
                        "author id must be nonempty, at most 512 bytes, and contain no controls"
                            .into(),
                    );
                }
                if by.is_some() && args.text("run").is_some() {
                    return Err("choose an author or a run, not both".into());
                }
                Ok(HistoryEdit::Undo {
                    by,
                    steps: steps_arg(args)?,
                    run: args.text("run").map(str::to_owned),
                    to,
                    step,
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
            "List the versions, oldest first: change id (what undo to, restore and diff take), action and its arguments, run, author, time, undone, checkpoints.",
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
pub fn is_edit(action: &str) -> bool {
    matches!(
        action,
        "undo" | "redo" | "restore" | "restore-reviewed" | "checkpoint"
    )
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
        .or_else(|| super::authors::actor_handle(&version.actor))
        .unwrap_or_else(|| version.actor.chars().take(8).collect())
}

/// What a change's entry records, as data: what was done (`what`, with
/// field paths, values and verb arguments), to which `item`, by which
/// `actor` (`id`, `kind`, and the person it acted `for`), in which `run`,
/// and what kind of action and change it was.
fn recorded(context: &tilo_actions::ActionContext, out: &mut serde_json::Map<String, Value>) {
    if let Some(what) = &context.what {
        out.insert("what".into(), json!(what));
    }
    if let Some(item) = &context.item {
        out.insert("item".into(), json!(item));
    }
    if let Some(invoker) = &context.invoker {
        let mut actor = serde_json::Map::new();
        actor.insert("id".into(), invoker.actor.clone().into());
        actor.insert("kind".into(), json!(invoker.kind));
        if let Some(person) = &invoker.on_behalf_of {
            actor.insert("for".into(), person.clone().into());
        }
        out.insert("actor".into(), Value::Object(actor));
    }
    if let Some(group) = &context.group {
        out.insert("run".into(), group.label.clone().into());
        out.insert("run_id".into(), group.id.clone().into());
        out.insert("run_kind".into(), group.kind.clone().into());
    }
    if let Some(kind) = context.kind {
        out.insert("kind".into(), json!(kind));
    }
    if !context.change.is_content() {
        out.insert("change_kind".into(), json!(context.change));
    }
}

/// What a history row says about one change, in `words`: what it did, else
/// what its actor said they were doing. `None` when the entry says neither.
pub fn step_summary(
    entry: &tilo_actions::ActionContext,
    words: &dyn HistoryWords,
) -> Option<String> {
    if entry.what.is_some() {
        return Some(words.sentence(entry));
    }
    entry
        .purpose
        .as_deref()
        .map(str::trim)
        .filter(|purpose| !purpose.is_empty())
        .map(str::to_owned)
}

/// The sentence for a version, in `words`.
fn sentence(version: &Version, words: &dyn HistoryWords) -> String {
    version
        .context
        .as_ref()
        .and_then(|entry| step_summary(entry, words))
        .or_else(|| version.message.clone())
        .unwrap_or_default()
}

fn entry(
    version: &Version,
    undone: &HashSet<String>,
    checkpoints: &[Checkpoint],
    words: &dyn HistoryWords,
) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert("change".into(), steps::short(&version.hash).into());
    let context = version.context.as_ref();
    if let Some(context) = context {
        recorded(context, &mut entry);
    }
    entry.insert("sentence".into(), sentence(version, words).into());
    if context.and_then(|context| context.group.as_ref()).is_none() {
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
    /// What was done, as a sentence in the reader's language.
    pub action: String,
    /// The entry the change recorded: who did it, for whom, and what.
    pub entry: Option<tilo_actions::ActionContext>,
    /// Who did it: the person's handle, or the agent run's label.
    pub author: String,
    /// The agent run that made it, when an agent did: what `undo` takes as
    /// `run` to take the whole run back.
    pub run: Option<String>,
    /// Milliseconds since the Unix epoch; 0 when unknown.
    pub time_ms: i64,
    /// Taken back by an undo.
    pub undone: bool,
    pub checkpoints: Vec<String>,
    /// Housekeeping about the document (naming a checkpoint), not a change
    /// to what it holds.
    pub housekeeping: bool,
}

/// The last `last` versions, oldest first, as [`HistoryVersion`]s.
pub fn recent(doc: &AutoCommit, last: usize, words: &dyn HistoryWords) -> Vec<HistoryVersion> {
    let versions = tilo_resource::history::committed_versions(doc);
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
                .filter(|group| steps::names_a_run(&group.kind));
            HistoryVersion {
                change: steps::short(&version.hash),
                action: sentence(version, words),
                entry: version.context.clone(),
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
pub fn history(doc: &AutoCommit, last: usize, words: &dyn HistoryWords) -> Value {
    let versions = tilo_resource::history::committed_versions(doc);
    let undone = steps::undone(doc);
    let checkpoints = steps::checkpoints(doc);
    let total = versions.len();
    let mut listed: Vec<Value> = versions
        .iter()
        .skip(total.saturating_sub(last))
        .filter(|version| !steps::is_schema_genesis(version))
        .map(|version| entry(version, &undone, &checkpoints, words))
        .collect();
    carry_over(&mut listed);
    json!({
        "versions": listed,
        "carried": "run, run_id, by and time_ms, where absent, are the version before's",
        "total": total,
        "now": version_name(&doc.base_heads()),
        "checkpoints": checkpoints.iter().map(|checkpoint| json!({
            "name": checkpoint.name,
            "version": version_name(&checkpoint.heads),
        })).collect::<Vec<_>>(),
    })
}

/// Answer a history view.
///
/// # Errors
/// The payload is no history view, or names a version there is none of.
pub fn answer(
    doc: &AutoCommit,
    payload: &[u8],
    words: &dyn HistoryWords,
) -> Result<ActionOutput, String> {
    let view: HistoryView = tilo_actions::decode_json(payload)
        .map_err(|error| format!("not a history view: {error}"))?;
    let value = match view {
        HistoryView::History { last } => history(doc, limit(last, 50), words),
        HistoryView::ReviewRestore { version, paths } => review::answer(doc, &version, &paths)?,
        HistoryView::Diff { from, to } => {
            let from = steps::resolve(doc, &from)?;
            let to = match to {
                Some(to) => steps::resolve(doc, &to)?,
                None => doc.base_heads(),
            };
            let items: Vec<Value> = steps::changed_items(&mut doc.clone(), &from, &to)
                .into_iter()
                .map(|item| json!({"item": item.path, "change": item.change}))
                .collect();
            json!({
                "from": version_name(&from),
                "to": version_name(&to),
                "from_heads": from.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "to_heads": to.iter().map(ToString::to_string).collect::<Vec<_>>(),
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
            (PatchAction::PutMap { key, value, .. }, true)
                if prefix != steps::UNDONE_KEY
                    || value.0.as_i64().is_none_or(|attempt| attempt <= 0) =>
            {
                key.strip_prefix(prefix).map(str::to_owned)
            }
            (PatchAction::PutMap { key, value, .. }, false)
                if prefix == steps::UNDONE_KEY
                    && value.0.as_i64().is_some_and(|attempt| attempt > 0) =>
            {
                key.strip_prefix(prefix).map(str::to_owned)
            }
            (PatchAction::DeleteMap { key }, false) => key.strip_prefix(prefix).map(str::to_owned),
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
pub fn result(
    action: &str,
    doc: &AutoCommit,
    patches: &[Patch],
    words: &dyn HistoryWords,
) -> Option<Value> {
    let mut doc = doc.clone();
    let now = version_name(&doc.base_heads());
    let described = |hashes: Vec<String>, doc: &mut AutoCommit| -> Vec<Value> {
        let versions = tilo_resource::history::committed_versions(doc);
        versions
            .iter()
            .rev()
            .filter(|version| hashes.contains(&version.hash.to_string()))
            .map(|version| {
                let mut entry = serde_json::Map::new();
                entry.insert("change".into(), steps::short(&version.hash).into());
                if let Some(context) = &version.context {
                    recorded(context, &mut entry);
                }
                entry.insert("sentence".into(), sentence(version, words).into());
                Value::Object(entry)
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
        "restore" | "restore-reviewed" => json!({"restored": items(patches), "now": now}),
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

impl HistoryEdit {
    /// Apply the edit, decoding recorded actions with the registry
    /// `register` fills: one compiled copy for every resource.
    #[inline(never)]
    ///
    /// # Errors
    /// Why nothing was done, in `words`, as [`steps`] says it.
    pub fn apply_registered(
        &self,
        doc: &mut tilo_actions::Doc,
        register: fn(&mut tilo_actions::ActionRegistry),
        words: &dyn HistoryWords,
    ) -> Result<(), ActionError> {
        if let Self::RestoreReviewed { review } = self {
            return review.apply(doc).map_err(ActionError::Refused);
        }
        let registry = tilo_actions::ActionRegistry::with(register);
        let mut original = doc.fork();
        match self {
            Self::Undo {
                steps,
                run,
                by,
                to,
                step,
            } => {
                let (scope, default) = match run {
                    Some(run) => (Scope::Run(run.clone()), usize::MAX),
                    None => (
                        by.as_ref().map_or_else(
                            || Scope::invoking(doc),
                            |author| Scope::Invoker(author.clone()),
                        ),
                        1,
                    ),
                };
                let compensate = |version: &Version| inverse(&mut original, &registry, version);
                match (to, step) {
                    (_, Some(step)) => {
                        steps::resolve(doc, step).and_then(|heads| match heads[..] {
                            [hash] => steps::undo_step(doc, &scope, &hash, compensate, words),
                            _ => Err(format!("`{step}` names no one change")),
                        })
                    }
                    (Some(to), None) => steps::resolve(doc, to).and_then(|heads| {
                        steps::undo_since(doc, &scope, &heads, compensate, words)
                    }),
                    (None, None) => {
                        steps::undo(doc, &scope, limit(*steps, default), compensate, words)
                    }
                }
                .map(drop)
            }
            Self::Redo { steps } => {
                let scope = Scope::invoking(doc);
                steps::redo(
                    doc,
                    &scope,
                    limit(*steps, 1),
                    |version| restoration(&mut original, &registry, version),
                    words,
                )
                .map(drop)
            }
            Self::Restore { version, ids } => steps::resolve(doc, version).and_then(|heads| {
                steps::restore(doc, &heads, ids.as_deref().unwrap_or_default()).map(drop)
            }),
            Self::RestoreReviewed { review } => review.apply(doc),
            Self::Checkpoint { name } => steps::checkpoint(doc, name),
        }
        .map_err(ActionError::Refused)
    }
}

/// A step's verified inverse, when it recorded one: replaying its action at
/// its own state reproduces it, so the inverse is the canonical one. A redo
/// that brought back one such step is taken back by the inverse of that
/// step's action as the redo ran it, at the redo's own state.
fn inverse(
    original: &mut tilo_actions::Doc,
    registry: &tilo_actions::ActionRegistry,
    version: &Version,
) -> Result<Option<steps::Inverse>, String> {
    if let Some(redone) = redone_step(original, version) {
        let Some(action) = restoration(original, registry, &redone)? else {
            return Ok(None);
        };
        let meta = original
            .get_change_meta_by_hash(&version.hash)
            .ok_or("history change is unavailable")?;
        let (deps, actor) = (meta.deps.clone(), meta.actor.clone().into_owned());
        let mut at = original.fork_at(&deps).map_err(|error| error.to_string())?;
        let Some(inverse) = tilo_actions::executor::inverse_as(&mut at, action.as_ref(), actor)
        else {
            return Ok(None);
        };
        return local(
            registry
                .decode(&inverse)
                .map_err(|error| error.to_string())?,
        )
        .map(Some);
    }
    let Some(action) =
        tilo_actions::executor::verified_compensation(original, version.hash, registry)
            .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    local(action).map(Some)
}

/// The one step a redo brought back, when `version` is such a redo.
fn redone_step(doc: &AutoCommit, version: &Version) -> Option<Version> {
    let [hash] = steps::brought_back(doc, version).try_into().ok()?;
    let hash = steps::recorded_hash(&hash)?;
    let document = doc.committed_document();
    let meta = document.get_change_meta_by_hash(&hash)?;
    Some(tilo_resource::history::version_of(document, &meta))
}

/// How a redo brings back an outside step: its verified action, run again.
/// A redo taken back stands for the step it brought back.
fn restoration(
    original: &mut tilo_actions::Doc,
    registry: &tilo_actions::ActionRegistry,
    version: &Version,
) -> Result<Option<steps::Inverse>, String> {
    if let Some(redone) = redone_step(original, version) {
        return restoration(original, registry, &redone);
    }
    if inverse(original, registry, version)?.is_none() {
        return Ok(None);
    }
    let recorded = version
        .context
        .as_ref()
        .and_then(|context| context.action.as_ref())
        .ok_or("verified redo action absent")?;
    local(
        registry
            .decode(recorded)
            .map_err(|error| error.to_string())?,
    )
    .map(Some)
}

/// History applies only actions it may run here.
fn local(action: steps::Inverse) -> Result<steps::Inverse, String> {
    if action.locus() == ExecutionLocus::ClientLocal {
        Ok(action)
    } else {
        Err("history compensation requires another execution locus".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_source::TestWords;
    use automerge::{ROOT, ReadDoc};

    #[test]
    fn history_and_diff_leave_the_callers_open_edit_uncommitted() {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "saved", 1_i64).unwrap();
        let first = doc.commit().unwrap();
        doc.put(ROOT, "saved", 2_i64).unwrap();
        let second = doc.commit().unwrap();
        doc.put(ROOT, "draft", true).unwrap();
        let before = (doc.base_heads(), doc.pending_ops());
        assert!(before.1 > 0);

        let listed = history(&doc, 50, &TestWords);
        assert_eq!(listed["total"], json!(2));
        assert_eq!(listed["now"], json!(version_name(&[second])));
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);

        let payload = serde_json::to_vec(&HistoryView::Diff {
            from: first.to_string(),
            to: Some(second.to_string()),
        })
        .unwrap();
        let diff = answer(&doc, &payload, &TestWords).unwrap().value.unwrap();
        assert!(
            diff["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["item"] == "saved")
        );
        assert_eq!((doc.base_heads(), doc.pending_ops()), before);
        assert_eq!(
            doc.get(ROOT, "draft").unwrap().unwrap().0.as_bool(),
            Some(true)
        );
    }
}
