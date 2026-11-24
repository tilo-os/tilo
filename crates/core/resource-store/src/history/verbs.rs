//! The resource's history as actions every app answers, native or
//! tilolang, so the people, scripts and agents working on it can look back,
//! compare and take back through the same action API they edit through:
//!
//! - `undo` / `redo`: the caller's own last steps (an agent's are those of
//!   its invocation), or a named group of them;
//! - `restore`: a past version, whole or only some items, as a new step;
//! - `review-restore` / `restore-reviewed`: pin a causal review and restore
//!   exact differing paths only if the live frontier still matches;
//! - `checkpoint`: name the current version;
//! - `history`: every step with what it was given, who made it, in which
//!   group, and which are undone or checkpointed;
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
use automerge::{AutoCommit, ChangeHash, Patch, PatchAction, Prop, ReadDoc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tilo_actions::{
    ActionError, ActionOutput, ActionSpec, ChangeKind, EncodedAction, ExecutionLocus,
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, tilo_resource::Effect)]
#[serde(rename_all = "kebab-case")]
// The operation is the type every recorded history edit travels as, as [`EDIT_TYPE`] names it.
#[effect(operation = "tilo.app.history-edit")]
pub enum HistoryEdit {
    /// Take back your last steps (an agent: its invocation's) as a new change; with invocation,
    /// that invocation's steps; with by, that author's steps outside one; with to, all after a
    /// version.
    #[effect(document, prompted, kind = "undoable", returns = HistoryAnswer)]
    Undo {
        #[param("How many steps, newest first")]
        steps: Option<u32>,
        #[param("A group id from history")]
        invocation: Option<String>,
        /// An author from history, for steps outside one.
        #[param("The actor's `by` of a history entry, for steps outside an invocation")]
        by: Option<tilo_actions::Actor>,
        /// Every step after this version instead.
        #[param("A change id from history, or a checkpoint name")]
        to: Option<String>,
        /// Only the step this change id from history is part of, wherever it
        /// stands, instead.
        #[param(
            "A change id from history: undo only the step it is part of, of your steps, or of \
             those invocation or by names"
        )]
        step: Option<String>,
    },
    /// Bring back what your last undo took back.
    #[effect(document, prompted, kind = "undoable", returns = HistoryAnswer)]
    Redo {
        #[param("How many undos, newest first")]
        steps: Option<u32>,
    },
    /// Bring the document back to a version as a new change, or only the items named by ids.
    #[effect(document, kind = "undoable", audience = "agents", returns = HistoryAnswer, icon = "rotate-ccw")]
    Restore {
        #[param("A change id from history, or a checkpoint name")]
        version: String,
        #[param("Only these item ids")]
        ids: Option<Vec<String>>,
        /// Only the step this change id from history is part of, taken back as one step of a
        /// restore to `version`: a restore writes one change for each step since.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<String>,
    },
    /// Restore precisely the reviewed paths as a new undoable change, only if the live causal
    /// frontier still matches. Empty selection never restores everything.
    #[effect(document, kind = "undoable", audience = "agents", returns = HistoryAnswer)]
    RestoreReviewed {
        #[param("The review object from review-restore")]
        review: review::RestoreReview,
    },
    /// Name the current version, to restore or compare with later.
    #[effect(
        document, kind = "undoable",
        returns = HistoryAnswer,
        icon = "bookmark"
    )]
    Checkpoint {
        #[param("The name")]
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
                invocation: None,
                to: None,
                step: None,
                ..
            } => (HistoryOp::Undo, steps.unwrap_or(1)),
            Self::Undo { step: Some(_), .. } => (HistoryOp::Undo, 1),
            Self::Undo { steps, .. } => (HistoryOp::Undo, steps.unwrap_or(0)),
            Self::Redo { steps } => (HistoryOp::Redo, steps.unwrap_or(1)),
            Self::Restore { step: Some(_), .. } => (HistoryOp::Restore, 1),
            Self::Restore { .. } | Self::RestoreReviewed { .. } => (HistoryOp::Restore, 0),
            Self::Checkpoint { name } => (HistoryOp::Checkpoint { name: name.clone() }, 0),
        };
        // The one step it moves, when it moves one: a gesture's changes share one description,
        // and the newest names the step.
        let moves = match self {
            Self::Undo {
                step: Some(step), ..
            }
            | Self::Restore {
                step: Some(step), ..
            } => steps::resolve(doc, step)
                .ok()
                .and_then(|heads| match heads[..] {
                    [hash] => version_of_hash(doc, &hash),
                    _ => None,
                }),
            Self::Undo {
                invocation,
                by,
                to: None,
                step: None,
                ..
            } if steps == 1 => {
                let scope = undo_scope(doc, invocation.as_ref(), by.as_ref());
                steps::undo_preview(doc, &scope, 1).into_iter().next()
            }
            Self::Redo { .. } if steps == 1 => {
                let scope = Scope::invoking(doc);
                steps::undos_in_effect(doc, &|version| scope.owns(version), 1)
                    .into_iter()
                    .next()
            }
            _ => None,
        };
        let mut did = tilo_actions::Did::history(op, steps, None);
        if let (tilo_actions::Did::History(step), Some(version)) = (&mut did, &moves) {
            let context = version.context.as_ref();
            step.of = context.and_then(named_step).map(Box::new);
            step.undoes = Some(invocation_of(version));
            step.verb = context.map(verb_of).unwrap_or_default();
        }
        tilo_actions::Described::new(did)
    }

    /// The edits this one is, one for each step it moves, newest first, when it moves steps of
    /// their own: an undo of several steps is an undo of each, a redo of several a redo of each,
    /// and a restore to a version an undo of every step since, so every change it writes moves
    /// one invocation and names it ([`tilo_actions::Action::steps`]). `None` for an edit that is
    /// one change whatever it moves (a checkpoint, a restore of some items, a reviewed restore),
    /// and for one that moves nothing, which applies to say why.
    #[must_use]
    pub fn steps(&self, doc: &AutoCommit) -> Option<Vec<Self>> {
        let one =
            |step: &[Version], invocation: Option<String>, by: Option<tilo_actions::Actor>| {
                Some(Self::Undo {
                    steps: None,
                    invocation,
                    by,
                    to: None,
                    step: Some(step.first()?.hash.to_string()),
                })
            };
        let edits: Vec<Self> = match self {
            Self::Undo {
                steps,
                invocation,
                by,
                to,
                step: None,
            } => {
                let scope = undo_scope(doc, invocation.as_ref(), by.as_ref());
                let default = if invocation.is_some() { usize::MAX } else { 1 };
                let limit = if to.is_some() {
                    usize::MAX
                } else {
                    limit(*steps, default)
                };
                let mut moved = steps::steps_in_effect(doc, &|version| scope.owns(version), limit);
                if let Some(to) = to {
                    let since = since(doc, to)?;
                    moved.retain(|step| step.iter().all(|version| since.contains(&version.hash)));
                }
                moved
                    .iter()
                    .map(|step| one(step, invocation.clone(), by.clone()))
                    .collect::<Option<_>>()?
            }
            Self::Redo { steps } => {
                // An undo is the changes one move wrote together: a redo brings back each of
                // them, its whole move as it was undone.
                let scope = Scope::invoking(doc);
                let undos = steps::undos_in_effect(doc, &|version| scope.owns(version), usize::MAX);
                let mut moves = 0_usize;
                let mut count = 0_usize;
                let mut last: Option<&Version> = None;
                for undo in &undos {
                    if !last.is_some_and(|last| together(last, undo)) {
                        if moves == limit(*steps, 1) {
                            break;
                        }
                        moves = moves.saturating_add(1);
                    }
                    count = count.saturating_add(1);
                    last = Some(undo);
                }
                vec![Self::Redo { steps: Some(1) }; count]
            }
            Self::Restore {
                version,
                ids: None,
                step: None,
            } => {
                let heads = steps::resolve(doc, version).ok()?;
                // One that matches the version already applies to say so.
                if steps::matches(doc, &heads) {
                    return None;
                }
                let since = since(doc, version)?;
                let mut moved = steps::steps_in_effect(doc, &|_| true, usize::MAX);
                moved.retain(|step| step.iter().all(|version| since.contains(&version.hash)));
                let mut edits: Vec<Self> = moved
                    .iter()
                    .map(|step| {
                        Some(Self::Restore {
                            version: version.clone(),
                            ids: None,
                            step: Some(step.first()?.hash.to_string()),
                        })
                    })
                    .collect::<Option<_>>()?;
                // A step made before the version and undone since stood then: taking back the
                // steps since leaves it undone, so the restore brings the rest of the version
                // back after them.
                if steps::undid_from_before(doc, &since) {
                    edits.push(self.clone());
                }
                if edits.is_empty() {
                    return None;
                }
                edits
            }
            _ => return None,
        };
        (!edits.is_empty()).then_some(edits)
    }
}

/// Whether two changes are of one move: written together, one for each step it moved.
fn together(last: &Version, next: &Version) -> bool {
    let gesture = |version: &Version| version.context.as_ref()?.gesture;
    gesture(last).is_some() && gesture(last) == gesture(next)
}

/// Whose steps an undo naming `invocation` or `by` takes back: that agent session's, that
/// author's, else the invoker's own.
fn undo_scope(
    doc: &AutoCommit,
    invocation: Option<&String>,
    by: Option<&tilo_actions::Actor>,
) -> Scope {
    match (invocation, by) {
        (Some(invocation), _) => Scope::Session(invocation.clone()),
        (None, Some(author)) => Scope::Invoker(author.clone()),
        (None, None) => Scope::invoking(doc),
    }
}

/// The changes made after the version `version` names; `None` when it names none.
fn since(doc: &AutoCommit, version: &str) -> Option<HashSet<ChangeHash>> {
    let heads = steps::resolve(doc, version).ok()?;
    Some(
        doc.committed_document()
            .get_changes(&heads)
            .iter()
            .map(automerge::Change::hash)
            .collect(),
    )
}

/// The version the change `hash` is.
fn version_of_hash(doc: &AutoCommit, hash: &ChangeHash) -> Option<Version> {
    let document = doc.committed_document();
    let meta = document.get_change_meta_by_hash(hash)?;
    Some(tilo_resource::history::version_of(document, &meta))
}

/// The invocation the change `version` is one change of: the one the action API admitted, else
/// the gesture it is a keystroke of, else the change itself.
fn invocation_of(version: &Version) -> String {
    let context = version.context.as_ref();
    context
        .and_then(|context| context.source_invocation.as_ref())
        .map(ToString::to_string)
        .or_else(|| {
            context
                .and_then(|context| context.gesture.as_ref())
                .map(|gesture| gesture.0.to_string())
        })
        .unwrap_or_else(|| version.hash.to_string())
}

/// The verb the change `context` describes was invoked as: an undo's or redo's, the verb of the
/// invocation it moved.
fn verb_of(context: &tilo_actions::ActionContext) -> String {
    let verb = |did: &tilo_actions::Did| match did {
        tilo_actions::Did::Verb(call) => Some(call.action.action.clone()),
        tilo_actions::Did::History(step) if !step.verb.is_empty() => Some(step.verb.clone()),
        _ => None,
    };
    context
        .what
        .as_ref()
        .and_then(verb)
        .or_else(|| {
            let recorded = context.action.as_ref()?;
            verb(&tilo_actions::Described::from_encoded(recorded, None)?.what)
        })
        .unwrap_or_default()
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

impl HistoryView {
    /// This read, refused when its required words are blank, them trimmed.
    fn validated(mut self) -> Result<Self, String> {
        if let Self::ReviewRestore { version, .. } = &mut self {
            required(version, "version")?;
        }
        Ok(self)
    }
}

impl HistoryEdit {
    /// This edit, refused when its arguments cannot be taken together or out of bounds, its
    /// required words trimmed.
    fn validated(mut self) -> Result<Self, String> {
        match &mut self {
            Self::Undo {
                steps,
                invocation,
                by,
                to,
                step,
            } => {
                if let Some(steps) = steps
                    && !(1..=MAX_STEPS).contains(&i64::from(*steps))
                {
                    return Err(format!("steps is 1 to {MAX_STEPS}"));
                }
                if to.is_some() && steps.is_some() {
                    return Err("give steps or to, not both".into());
                }
                if step.is_some() && (to.is_some() || steps.is_some()) {
                    return Err("give step alone, or with whose it is (invocation or by)".into());
                }
                if by.is_some() && invocation.is_some() {
                    return Err("choose an author or an invocation, not both".into());
                }
            }
            Self::Redo { steps } => {
                if let Some(steps) = steps
                    && !(1..=MAX_STEPS).contains(&i64::from(*steps))
                {
                    return Err(format!("steps is 1 to {MAX_STEPS}"));
                }
            }
            Self::Restore { version, .. } => required(version, "version")?,
            Self::Checkpoint { name } => required(name, "name")?,
            Self::RestoreReviewed { .. } => {}
        }
        Ok(self)
    }
}

/// `text`, trimmed, refused when it is nothing: a required word its caller types.
fn required(text: &mut String, name: &str) -> Result<(), String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(format!("{name} is required"));
    }
    *text = trimmed.to_owned();
    Ok(())
}

/// What every history verb answers: an object whose keys say what it did — the steps an undo took
/// back or a redo brought back, the items a restore wrote, the checkpoint it named, or the versions
/// and comparison a read lists.
#[derive(Clone, Copy, Debug)]
pub struct HistoryAnswer;

impl tilo_resource::JsonSchema for HistoryAnswer {
    fn json_schema() -> Value {
        json!({"type": "object"})
    }
}

/// The history verbs that change one part of a chain, each naming the part (`part`): what a chain
/// publishes beside its parts' own actions, so the people, agents and the workspace's moderation
/// take back steps in a part through the same action API as in any record.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
// The operation is the type every part's history edit travels as, as [`PART_EDIT_TYPE`] names it.
#[effect(operation = "tilo.app.part-history-edit")]
pub enum PartHistoryEditAction {
    /// Take back your last steps (an agent: its invocation's) as a new change; with invocation,
    /// that invocation's steps; with by, that author's steps outside one; with to, all after a
    /// version.
    #[effect(document, kind = "undoable", audience = "agents", returns = HistoryAnswer)]
    Undo {
        #[param("How many steps, newest first")]
        steps: Option<u32>,
        #[param("A group id from history")]
        invocation: Option<String>,
        #[param("The actor's `by` of a history entry, for steps outside an invocation")]
        by: Option<tilo_actions::Actor>,
        #[param("A change id from history, or a checkpoint name")]
        to: Option<String>,
        #[param(
            "A change id from history: undo only the step it is part of, of your steps, or of \
             those invocation or by names"
        )]
        step: Option<String>,
        #[param("The number of the part whose history it changes")]
        part: u64,
    },
    /// Bring back what your last undo took back.
    #[effect(document, kind = "undoable", audience = "agents", returns = HistoryAnswer)]
    Redo {
        #[param("How many undos, newest first")]
        steps: Option<u32>,
        #[param("The number of the part whose history it changes")]
        part: u64,
    },
    /// Bring the document back to a version as a new change, or only the items named by ids.
    #[effect(
        document, kind = "undoable",
        audience = "agents",
        returns = HistoryAnswer,
        icon = "rotate-ccw"
    )]
    Restore {
        #[param("A change id from history, or a checkpoint name")]
        version: String,
        #[param("Only these item ids")]
        ids: Option<Vec<String>>,
        #[param("The number of the part whose history it changes")]
        part: u64,
    },
    /// Name the current version, to restore or compare with later.
    #[effect(
        document, kind = "undoable",
        audience = "agents",
        returns = HistoryAnswer,
        icon = "bookmark"
    )]
    Checkpoint {
        #[param("The name")]
        name: String,
        #[param("The number of the part whose history it changes")]
        part: u64,
    },
}

/// `edit` as the declaration's action, when a part's history has it.
fn part_action(edit: &PartEdit) -> Result<PartHistoryEditAction, String> {
    let part = edit.part;
    Ok(match &edit.edit {
        HistoryEdit::Undo {
            steps,
            invocation,
            by,
            to,
            step,
        } => PartHistoryEditAction::Undo {
            steps: *steps,
            invocation: invocation.clone(),
            by: by.clone(),
            to: to.clone(),
            step: step.clone(),
            part,
        },
        HistoryEdit::Redo { steps } => PartHistoryEditAction::Redo {
            steps: *steps,
            part,
        },
        HistoryEdit::Restore {
            version,
            ids,
            step: None,
        } => PartHistoryEditAction::Restore {
            version: version.clone(),
            ids: ids.clone(),
            part,
        },
        HistoryEdit::Restore { step: Some(_), .. } => {
            return Err("a part's history restores no single step".into());
        }
        HistoryEdit::Checkpoint { name } => PartHistoryEditAction::Checkpoint {
            name: name.clone(),
            part,
        },
        HistoryEdit::RestoreReviewed { .. } => {
            return Err("a part's history has no reviewed restore".into());
        }
    })
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

    /// This edit as the action it runs, in the form the declaration encodes it.
    ///
    /// # Errors
    ///
    /// A part's history has no reviewed restore.
    pub fn encoded(&self) -> Result<EncodedAction, String> {
        let action = part_action(self)?;
        Ok(EncodedAction {
            type_id: PART_EDIT_TYPE.to_owned(),
            payload: declared_payload(&action)?,
        })
    }
}

/// The payload `edit` travels as, in the form its declaration encodes it: what an invocation of
/// the verb, or a step recorded in the document, carries.
///
/// # Errors
///
/// Its arguments cannot be serialized.
pub fn edit_payload(edit: &HistoryEdit) -> Result<Vec<u8>, String> {
    declared_payload(edit)
}

/// `action` as the payload an invocation of its verb carries.
fn declared_payload<E: tilo_resource::Effect>(action: &E) -> Result<Vec<u8>, String> {
    let payload = json!({
        "effect": tilo_resource::Effect::id(action),
        "args": tilo_resource::Effect::arguments(action),
    });
    serde_json::to_vec(&payload).map_err(|error| error.to_string())
}

impl From<PartHistoryEditAction> for PartEdit {
    fn from(action: PartHistoryEditAction) -> Self {
        match action {
            PartHistoryEditAction::Undo {
                steps,
                invocation,
                by,
                to,
                step,
                part,
            } => Self {
                part,
                edit: HistoryEdit::Undo {
                    steps,
                    invocation,
                    by,
                    to,
                    step,
                },
            },
            PartHistoryEditAction::Redo { steps, part } => Self {
                part,
                edit: HistoryEdit::Redo { steps },
            },
            PartHistoryEditAction::Restore { version, ids, part } => Self {
                part,
                edit: HistoryEdit::Restore {
                    version,
                    ids,
                    step: None,
                },
            },
            PartHistoryEditAction::Checkpoint { name, part } => Self {
                part,
                edit: HistoryEdit::Checkpoint { name },
            },
        }
    }
}

/// The history a call reads: the versions, a comparison of two, or the review of a restoration.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
// The operation is the type every history read travels as, as [`VIEW_TYPE`] names it.
#[effect(operation = "tilo.app.history")]
pub enum HistoryViewAction {
    /// List the versions, oldest first: change id (what undo to, restore and diff take), action
    /// and its arguments, invocation, author, time, undone, what an undo took back or a redo
    /// brought back, checkpoints.
    #[effect(
        view,
        prompted,
        audience = "agents",
        returns = HistoryAnswer,
        tag = tilo_actions::RESOURCE_READ_TAG
    )]
    History {
        #[param("How many recent versions (default 50)")]
        last: Option<i64>,
    },
    /// List the items that differ between two versions.
    #[effect(
        view,
        audience = "agents",
        returns = HistoryAnswer,
        tag = tilo_actions::RESOURCE_READ_TAG
    )]
    Diff {
        #[param("A change id from history, or a checkpoint name")]
        from: String,
        #[param("The later version (default now)")]
        to: Option<String>,
    },
    /// Review exact-item restoration without writing. Returns pinned target and live heads; copy
    /// review into restore-reviewed. Any later edit requires a new review.
    #[effect(
        view,
        audience = "agents",
        returns = HistoryAnswer,
        tag = tilo_actions::RESOURCE_READ_TAG
    )]
    ReviewRestore {
        #[param("A change id, causal frontier, or checkpoint")]
        version: String,
        #[param("Exact differing paths from diff; a root selects its entire value")]
        paths: Vec<String>,
    },
}

impl From<HistoryViewAction> for HistoryView {
    fn from(action: HistoryViewAction) -> Self {
        match action {
            HistoryViewAction::History { last } => Self::History {
                // A read asks for a bounded list, as the verbs always clamped it.
                last: last.map(|last| u32::try_from(last.clamp(1, MAX_LISTED)).unwrap_or(1)),
            },
            HistoryViewAction::Diff { from, to } => Self::Diff { from, to },
            HistoryViewAction::ReviewRestore { version, paths } => {
                Self::ReviewRestore { version, paths }
            }
        }
    }
}

/// The history actions every framework app publishes, unless its resource
/// already publishes one of the same id.
pub fn specs() -> Vec<ActionSpec> {
    let mut specs = edit_specs();
    specs.extend(view_specs());
    specs.extend(review::specs());
    specs
}

fn edit_specs() -> Vec<ActionSpec> {
    ["undo", "redo", "restore", "checkpoint"]
        .iter()
        .filter_map(|id| <HistoryEdit as tilo_resource::Effect>::spec(id))
        .map(published)
        .collect()
}

fn view_specs() -> Vec<ActionSpec> {
    ["history", "diff"]
        .iter()
        .filter_map(|id| <HistoryViewAction as tilo_resource::Effect>::spec(id))
        .map(published)
        .collect()
}

/// The surface the history verbs publish: an optional parameter is `nullable`
/// and carries no schema of its own — a count or a word is typed by its
/// parameter, and the bound it keeps is checked when the verb runs — while a
/// list or a review names its shape; and the record-read tag comes before the
/// audience a read is listed for.
fn published(mut spec: ActionSpec) -> ActionSpec {
    let names: Vec<String> = spec
        .params()
        .iter()
        .map(|param| param.name.to_string())
        .collect();
    for name in names {
        spec.narrow_param(&name, |param| {
            param.nullable = !param.required;
            let Some(schema) = param.schema.take() else {
                return;
            };
            // Only a list names a shape of its own: a count, a word or an author is typed by its
            // parameter, and the bound it keeps is checked when the verb runs.
            if plain_schema(&schema) || (!param.required && !list_schema(&schema)) {
                return;
            }
            param.schema = Some(std::sync::Arc::new(without_null(schema.as_ref().clone())));
        });
    }
    let tags = &mut spec.type_mut().contract.tags;
    if let Some(at) = tags
        .iter()
        .position(|tag| tag == tilo_actions::RESOURCE_READ_TAG)
    {
        let read = tags.remove(at);
        tags.insert(0, read);
    }
    spec
}

/// Whether `schema` is the plain type of a scalar, whose parameter already says
/// it, or a union of one with null.
fn plain_schema(schema: &Value) -> bool {
    let plain = |ty: &Value| {
        matches!(
            ty.as_str(),
            Some("string" | "integer" | "number" | "boolean")
        )
    };
    match schema.get("type") {
        Some(Value::String(ty)) => {
            matches!(ty.as_str(), "string" | "integer" | "number" | "boolean")
        }
        Some(Value::Array(types)) => types
            .iter()
            .all(|ty| ty.as_str() == Some("null") || plain(ty)),
        _ => false,
    }
}

/// Whether `schema` is a list's, possibly a union of one with null.
fn list_schema(schema: &Value) -> bool {
    let mut kinds: Vec<Option<&str>> = match schema.get("type") {
        Some(Value::String(ty)) => vec![Some(ty.as_str())],
        Some(Value::Array(types)) => types.iter().map(Value::as_str).collect(),
        _ => return false,
    };
    kinds.retain(|ty| *ty != Some("null"));
    kinds == [Some("array")]
}

/// `schema` with null dropped from its type, the one type left spelled alone.
fn without_null(mut schema: Value) -> Value {
    let Some(types) = schema.get_mut("type").and_then(Value::as_array_mut) else {
        return schema;
    };
    types.retain(|ty| ty.as_str() != Some("null"));
    let only = match types.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    };
    if let (Some(only), Some(ty)) = (only, schema.get_mut("type")) {
        *ty = only;
    }
    schema
}

/// The history verbs that change one part of a chain, each naming the part
/// (`part`): what a chain publishes beside its parts' own actions, so the
/// people, agents and the workspace's moderation take back steps in a part
/// through the same action API as in any record.
pub fn part_edit_specs() -> Vec<ActionSpec> {
    ["undo", "redo", "restore", "checkpoint"]
        .iter()
        .filter_map(|id| <PartHistoryEditAction as tilo_resource::Effect>::spec(id))
        .map(published)
        .collect()
}

/// The edit a recorded or invoked history action carries: the declaration's form, or, for a step
/// recorded before the verbs were declared, the serde form it travelled as.
///
/// # Errors
///
/// The payload is neither, or the edit's arguments cannot be taken together.
pub fn edit_from_payload(payload: &[u8]) -> Result<HistoryEdit, String> {
    match <HistoryEdit as tilo_resource::Effect>::from_payload(payload) {
        Ok(edit) => edit.validated(),
        Err(_) => serde_json::from_slice::<HistoryEdit>(payload)
            .map_err(|error| error.to_string())
            .and_then(HistoryEdit::validated),
    }
}

/// The edit on one part of a chain a recorded or invoked action carries.
///
/// # Errors
///
/// The payload is neither form, or the edit's arguments cannot be taken together.
pub fn part_from_payload(payload: &[u8]) -> Result<PartEdit, String> {
    let mut edit = match <PartHistoryEditAction as tilo_resource::Effect>::from_payload(payload) {
        Ok(action) => PartEdit::from(action),
        Err(_) => serde_json::from_slice::<PartEdit>(payload).map_err(|error| error.to_string())?,
    };
    edit.edit = edit.edit.validated()?;
    Ok(edit)
}

/// The view a history read carries, in either form ([`edit_from_payload`]).
///
/// # Errors
///
/// The payload is neither form, or its arguments are blank.
pub fn view_from_payload(payload: &[u8]) -> Result<HistoryView, String> {
    if let Ok(view) = <HistoryViewAction as tilo_resource::Effect>::from_payload(payload) {
        HistoryView::validated(HistoryView::from(view))
    } else {
        let view =
            serde_json::from_slice::<HistoryView>(payload).map_err(|error| error.to_string())?;
        HistoryView::validated(view)
    }
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
        .map(|invoker| {
            invoker
                .name
                .clone()
                .unwrap_or_else(|| invoker.acting.by.id().to_owned())
        })
        .or_else(|| super::authors::actor_handle(&version.actor))
        .unwrap_or_else(|| version.actor.chars().take(8).collect())
}

/// What a change's entry records, as data: what was done (`what`, with
/// field paths, values and verb arguments), to which `item`, by which
/// `actor` (the acting tree, and for an agent with a name the `name` and
/// `colour` it shows by), in which `group` (`title` and `group_kind`), through
/// which `invocation` of the action API, as part of which undo `step` (its
/// gesture), and what kind of action and change it was. The invocation and
/// the step are what a caller that ran an action finds its own changes by,
/// to take exactly those back (`undo` with `step`).
fn recorded(context: &tilo_actions::ActionContext, out: &mut serde_json::Map<String, Value>) {
    if let Some(what) = &context.what {
        out.insert("what".into(), json!(what));
    }
    if let Some(item) = &context.item {
        out.insert("item".into(), json!(item));
    }
    if let Some(invoker) = &context.invoker {
        let mut actor = json!(invoker.acting);
        // The group the agent said this change is a step of, by the id it gave
        // it: how an agent or a script finds its own invocation's steps again,
        // where `group` is the host's group of the whole connection.
        if let (Some(run), Some(fields)) = (
            context
                .group
                .as_ref()
                .and_then(|group| group.declared_id.as_ref()),
            actor.as_object_mut(),
        ) {
            fields.insert("declared".into(), run.clone().into());
        }
        if let (Some(name), Some(fields)) = (&invoker.name, actor.as_object_mut()) {
            let hue = invoker.acting.member_id().map_or_else(
                || collab_types::presence::PresenceHue::of_agent(name),
                collab_types::presence::PresenceHue::of_member,
            );
            let (red, green, blue) = collab_types::presence::identity_rgb(hue);
            fields.insert("name".into(), name.clone().into());
            fields.insert(
                "colour".into(),
                format!("#{red:02x}{green:02x}{blue:02x}").into(),
            );
        }
        out.insert("actor".into(), actor);
    }
    if let Some(group) = &context.group {
        if let Some(label) = &group.label {
            out.insert("title".into(), label.clone().into());
        }
        out.insert("group".into(), group.id.clone().into());
        out.insert("group_kind".into(), group.kind.clone().into());
    }
    if let Some(kind) = context.kind {
        out.insert("kind".into(), json!(kind));
    }
    if let Some(invocation) = context.source_invocation {
        out.insert("invocation".into(), invocation.0.to_string().into());
    }
    if let Some(gesture) = context.gesture {
        out.insert("step".into(), gesture.0.to_string().into());
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
    doc: &AutoCommit,
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
    // A group names who acted; a move through history is the person's own, named by them.
    if context
        .and_then(|context| context.group.as_ref())
        .is_none_or(|group| tilo_actions::context::history_move(&group.kind).is_some())
    {
        entry.insert("by".into(), by(version).into());
    }
    entry.insert("time_ms".into(), version.time_ms.into());
    // What the change did to the resource it is a version of, read from the change itself as the
    // store reads it, and which resource that was: a watcher is told both beside the change's id
    // rather than working them out from what it saw before.
    if let Some((what, item)) = happened(doc, version) {
        entry.insert("happened".into(), what.word().into());
        if context.is_none()
            && let Some(item) = item
        {
            entry.insert("item".into(), item.into());
        }
    }
    if let Some(named) =
        context.and_then(|context| tilo_actions::contract::readers_named(&context.facets))
    {
        entry.insert("readers".into(), json!(named));
    }
    if undone.contains(&version.hash.to_string()) {
        entry.insert("undone".into(), true.into());
    }
    // The changes an undo or redo moved: what tells it from an edit of its own.
    let moved = match steps::kind_of(doc, version) {
        steps::Kind::Undo(moved) => Some(("takes_back", moved)),
        steps::Kind::Redo(moved) => Some(("brings_back", moved)),
        steps::Kind::Edit | steps::Kind::Bookkeeping => None,
    };
    if let Some((key, moved)) = moved {
        let moved: Vec<String> = moved
            .iter()
            .map(|hash| hash.chars().take(12).collect())
            .collect();
        entry.insert(key.into(), json!(moved));
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

/// What `version` did to the resource it is a change of, and that resource's id, as the store
/// reads the change: it made the resource, removed it, or edited it. `None` for a document
/// whose changes are not resources kept in the store: a change with no word for what it did is
/// a fault to a watcher, never an edit.
fn happened(
    doc: &AutoCommit,
    version: &Version,
) -> Option<(tilo_actions::remote::What, Option<String>)> {
    use tilo_actions::remote::What;
    let committed = doc.committed_document();
    let change = committed.get_change_by_hash(&version.hash)?;
    let touched = crate::admit::touches(committed, "", &change)?;
    let made = touched.resources.iter().find(|(_, touch)| touch.made);
    let removed = touched.resources.iter().find(|(_, touch)| touch.removed);
    let what = if made.is_some() {
        What::Created
    } else if removed.is_some() {
        What::Removed
    } else {
        What::Edited
    };
    // The resource the change is about: the one it made or removed, else the one it touched when
    // it touched a single resource.
    let item =
        made.or(removed)
            .map(|(id, _)| id.clone())
            .or_else(|| match touched.resources.len() {
                1 => touched.resources.keys().next().cloned(),
                _ => None,
            });
    Some((what, item))
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
    /// Who did it: the person's handle, or the agent's name.
    pub author: String,
    /// The group that made it, when an agent did: what `undo` takes as
    /// `invocation` to take the whole of it back.
    pub invocation: Option<String>,
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
                .filter(|group| steps::names_a_session(&group.kind));
            HistoryVersion {
                change: steps::short(&version.hash),
                action: sentence(version, words),
                entry: version.context.clone(),
                author: by(version),
                invocation: run.map(|group| group.id.clone()),
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
/// before: forty steps of one group name it once, not forty times.
fn carry_over(listed: &mut [Value]) {
    let mut before = serde_json::Map::new();
    for entry in listed {
        let Value::Object(fields) = entry else {
            continue;
        };
        // One connection's group may go by a new title from one call to the
        // next: a version under another title names it again.
        let same_author = ["group", "by"].iter().any(|key| {
            fields
                .get(*key)
                .is_some_and(|value| before.get(*key) == Some(value))
        }) && fields.get("title") == before.get("title");
        let author: Vec<&str> = ["title", "group", "by"]
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

/// A document kept beside a record whose versions the record's history
/// lists with its own: part `index` of a chain, whose history verbs (`restore`
/// with `part`) are published on `resource`.
pub struct BesideDocument<'a> {
    pub resource: String,
    pub index: u64,
    pub doc: &'a AutoCommit,
}

/// The versions as `history` lists them: the last `last`, oldest first.
pub fn history(doc: &AutoCommit, last: usize, words: &dyn HistoryWords) -> Value {
    history_with(doc, &[], last, words)
}

/// The versions of `doc` and of the documents kept `beside` it, merged by
/// when they were made, as `history` lists them: the last `last`, oldest
/// first. A version of a part names the part and the resource whose verbs
/// take its change id.
pub fn history_with(
    doc: &AutoCommit,
    beside: &[BesideDocument<'_>],
    last: usize,
    words: &dyn HistoryWords,
) -> Value {
    let checkpoints = steps::checkpoints(doc);
    let reader = words.reader();
    let mut total: usize = 0;
    let mut sources = Vec::with_capacity(beside.len().saturating_add(1));
    for (document, part) in std::iter::once((doc, None)).chain(
        beside
            .iter()
            .map(|beside| (beside.doc, Some((beside.resource.as_str(), beside.index)))),
    ) {
        let versions = tilo_resource::history::committed_versions(document);
        let undone = steps::undone(document);
        let own_checkpoints = steps::checkpoints(document);
        // The framework making a document, a part as much as the record, is nobody's
        // change: History shows none of it, so this neither lists nor counts it.
        let shown: Vec<&Version> = versions
            .iter()
            .filter(|version| !steps::is_schema_genesis(version) && !is_document_making(version))
            .filter(|version| reads_version(version, reader.as_ref()))
            .collect();
        let listed: Vec<(i64, Value)> = moves(&shown)
            .into_iter()
            .map(|changes| {
                let mut listed = listed_move(document, &changes, &undone, &own_checkpoints, words);
                let time_ms = changes.last().map_or(0, |version| version.time_ms);
                if let (Some((resource, index)), Value::Object(fields)) = (part, &mut listed) {
                    fields.insert("resource".into(), resource.into());
                    fields.insert("part".into(), index.into());
                }
                (time_ms, listed)
            })
            .collect();
        total = total.saturating_add(listed.len());
        sources.push(listed.into_iter().peekable());
    }
    // Each document's versions stay in its own order; between documents the
    // earlier one goes first, the record's on a tie.
    let mut merged = Vec::new();
    while let Some(next) = sources
        .iter_mut()
        .enumerate()
        .filter_map(|(source, versions)| versions.peek().map(|(time, _)| (*time, source)))
        .min()
        .map(|(_, source)| source)
    {
        if let Some((_, version)) = sources.get_mut(next).and_then(Iterator::next) {
            merged.push(version);
        }
    }
    let mut listed: Vec<Value> = merged.split_off(merged.len().saturating_sub(last));
    carry_over(&mut listed);
    json!({
        "versions": listed,
        "carried": "title, group, by and time_ms, where absent, are the version before's",
        "total": total,
        "now": version_name(&doc.base_heads()),
        "checkpoints": checkpoints.iter().map(|checkpoint| json!({
            "name": checkpoint.name,
            "version": version_name(&checkpoint.heads),
        })).collect::<Vec<_>>(),
    })
}

/// The versions as History lists them, oldest first: each on its own, but the changes one move
/// through history wrote together (an undo of several steps, a restore), one for each step it
/// moved, as the one move.
fn moves<'a>(versions: &[&'a Version]) -> Vec<Vec<&'a Version>> {
    let mut listed: Vec<Vec<&Version>> = Vec::new();
    for version in versions {
        if let Some(last) = listed.last_mut()
            && let Some(before) = last.last()
            && moved_history(before)
            && moved_history(version)
            && together(before, version)
        {
            last.push(version);
            continue;
        }
        listed.push(vec![version]);
    }
    listed
}

/// The move through history `version` is a change of, by the group it was written in.
fn history_move(version: &Version) -> Option<tilo_actions::HistoryOp> {
    tilo_actions::context::history_move(&version.context.as_ref()?.group.as_ref()?.kind)
}

/// Whether `version` is a change of a move through history.
fn moved_history(version: &Version) -> bool {
    history_move(version).is_some()
}

/// The entry History lists for `changes`, a move's changes, oldest first: the one change's own,
/// or for several the newest's, saying how many steps the move took and every change it took
/// back or brought back.
fn listed_move(
    doc: &AutoCommit,
    changes: &[&Version],
    undone: &HashSet<String>,
    checkpoints: &[Checkpoint],
    words: &dyn HistoryWords,
) -> Value {
    let Some(newest) = changes.last() else {
        return Value::Null;
    };
    if changes.len() == 1 {
        return entry(doc, newest, undone, checkpoints, words);
    }
    let op = history_move(newest).unwrap_or(tilo_actions::HistoryOp::Undo);
    let mut whole = (*newest).clone();
    if let Some(context) = whole.context.as_mut() {
        let steps = u32::try_from(changes.len()).unwrap_or(u32::MAX);
        context.what = Some(tilo_actions::Did::history(op, steps, None));
    }
    let mut listed = entry(doc, &whole, undone, checkpoints, words);
    if let Value::Object(fields) = &mut listed {
        for key in ["takes_back", "brings_back"] {
            let moved: Vec<Value> = changes
                .iter()
                .rev()
                .filter_map(|change| {
                    let mut one = entry(doc, change, undone, checkpoints, words);
                    one.as_object_mut()?.remove(key)
                })
                .flat_map(|moved| moved.as_array().cloned().unwrap_or_default())
                .collect();
            if !moved.is_empty() {
                fields.insert(key.into(), moved.into());
            }
        }
        let each: Vec<Value> = changes
            .iter()
            .rev()
            .map(|change| steps::short(&change.hash).into())
            .collect();
        fields.insert("changes".into(), each.into());
    }
    listed
}

/// Whether `reader` reads `version`: one its change marks as read by some
/// members alone ([`tilo_actions::READERS_FACET`]) is theirs alone.
fn reads_version(version: &Version, reader: Option<&tilo_actions::MemberId>) -> bool {
    version
        .context
        .as_ref()
        .is_none_or(|context| tilo_actions::contract::reads_change(&context.facets, reader))
}

/// Whether `reader` reads `entry`, a change as [`entries_after`] lists it:
/// one listed with the members who alone read it (`readers`) is theirs alone.
#[must_use]
pub fn reads_entry(entry: &Value, reader: Option<&tilo_actions::MemberId>) -> bool {
    let Some(readers) = entry.get("readers") else {
        return true;
    };
    reader.is_some_and(|reader| {
        readers.as_array().is_some_and(|named| {
            named
                .iter()
                .any(|name| name.as_str() == Some(&tilo_actions::contract::reader_name(reader)))
        })
    })
}

/// The changes of `doc` made after the version `after` (every change, for
/// none), oldest first, each as `history` lists it, whole: what a feed of a
/// workspace's changes reports of one record, as the record's own history
/// would. A change only some members read lists them as `readers`, and whoever
/// passes it on lists it to them alone ([`reads_entry`]).
pub fn entries_after(
    doc: &AutoCommit,
    after: &[ChangeHash],
    words: &dyn HistoryWords,
) -> Vec<Value> {
    let since: HashSet<ChangeHash> = doc
        .committed_document()
        .get_changes_meta(after)
        .iter()
        .map(|meta| meta.hash)
        .collect();
    if since.is_empty() {
        return Vec::new();
    }
    let undone = steps::undone(doc);
    let checkpoints = steps::checkpoints(doc);
    tilo_resource::history::committed_versions(doc)
        .iter()
        .filter(|version| since.contains(&version.hash))
        .filter(|version| !steps::is_schema_genesis(version) && !is_document_making(version))
        .map(|version| entry(doc, version, &undone, &checkpoints, words))
        .collect()
}

/// Whether `version` is the framework making the record (its genesis, or the
/// structure every member derives the same way): nobody's change, which a
/// feed of what people and agents changed leaves out.
fn is_document_making(version: &Version) -> bool {
    version
        .message
        .as_deref()
        .and_then(tilo_actions::stamp::Label::of)
        .is_some_and(|label| {
            let tilo_actions::stamp::Label::Framework(change) = label else {
                return false;
            };
            matches!(
                change,
                tilo_actions::framework::FrameworkChange::Genesis
                    | tilo_actions::framework::FrameworkChange::Structure
            )
        })
}

/// Answer a history view.
pub fn answer(
    doc: &AutoCommit,
    payload: &[u8],
    words: &dyn HistoryWords,
) -> Result<ActionOutput, String> {
    answer_with(doc, &[], payload, words)
}

/// Answer a history view of `doc` and the documents kept `beside` it: the
/// versions of all of them, and a comparison in whichever one holds the
/// version it starts from.
pub fn answer_with(
    doc: &AutoCommit,
    beside: &[BesideDocument<'_>],
    payload: &[u8],
    words: &dyn HistoryWords,
) -> Result<ActionOutput, String> {
    let view = view_from_payload(payload)?;
    let value = match view {
        HistoryView::History { last } => history_with(doc, beside, limit(last, 50), words),
        HistoryView::ReviewRestore { version, paths } => review::answer(doc, &version, &paths)?,
        HistoryView::Diff { from, to } => {
            // The record's own versions first: a change id names one change,
            // and a checkpoint is the record's.
            let (doc, part, from) = match steps::resolve(doc, &from) {
                Ok(heads) => (doc, None, heads),
                Err(error) => beside
                    .iter()
                    .find_map(|beside| {
                        steps::resolve(beside.doc, &from)
                            .ok()
                            .map(|heads| (beside.doc, Some(beside), heads))
                    })
                    .ok_or(error)?,
            };
            let to = match to {
                Some(to) => steps::resolve(doc, &to)?,
                None => doc.base_heads(),
            };
            let items: Vec<Value> = steps::changed_items(&mut doc.clone(), &from, &to)
                .into_iter()
                .map(|item| json!({"item": item.path, "change": item.change}))
                .collect();
            let mut compared = json!({
                "from": version_name(&from),
                "to": version_name(&to),
                "from_heads": from.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "to_heads": to.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "items": items,
            });
            if let (Some(part), Value::Object(fields)) = (part, &mut compared) {
                fields.insert("resource".into(), part.resource.clone().into());
                fields.insert("part".into(), part.index.into());
            }
            compared
        }
    };
    Ok(ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    })
}

/// The root keys `patches` put (`true`) or delete under `prefix`, without it.
fn log(patches: &[Patch], prefix: &str, put: bool) -> Vec<String> {
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
            json!({"undone": described(log(patches, steps::UNDONE_KEY, true), &mut doc), "now": now})
        }
        "redo" => {
            json!({"redone": described(log(patches, steps::UNDONE_KEY, false), &mut doc), "now": now})
        }
        "restore" | "restore-reviewed" => json!({"restored": items(patches), "now": now}),
        "checkpoint" => {
            let name = log(patches, steps::CHECKPOINT_KEY, true);
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
        match self {
            Self::Undo {
                steps,
                invocation,
                by,
                to,
                step,
            } => {
                let (scope, default) = match invocation {
                    Some(invocation) => (Scope::Session(invocation.clone()), usize::MAX),
                    None => (
                        by.as_ref().map_or_else(
                            || Scope::invoking(doc),
                            |author| Scope::Invoker(author.clone()),
                        ),
                        1,
                    ),
                };
                let compensate =
                    |doc: &mut AutoCommit, version: &Version| inverse(doc, &registry, version);
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
                    |doc, version| restoration(doc, &registry, version),
                    words,
                )
                .map(drop)
            }
            Self::Restore {
                step: Some(step), ..
            } => steps::resolve(doc, step).and_then(|heads| match heads[..] {
                [hash] => steps::restore_step(doc, &hash, words),
                _ => Err(format!("`{step}` names no one change")),
            }),
            Self::Restore { version, ids, .. } => steps::resolve(doc, version).and_then(|heads| {
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
    local(recorded_action(registry, recorded)?).map(Some)
}

/// The action a recorded entry names, as it ran: its large parts, kept as
/// content in the record's store, read back before it decodes, and asked of
/// the record's route when this device lacks them.
fn recorded_action(
    registry: &tilo_actions::ActionRegistry,
    recorded: &tilo_actions::EncodedAction,
) -> Result<Box<dyn tilo_actions::Action>, String> {
    let resolved = registry
        .resolve(recorded)
        .map_err(|error| error.to_string())?
        .map_err(|missing| missing.to_string())?;
    registry
        .decode(&resolved)
        .map_err(|error| error.to_string())
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
    use crate::history::TestWords;
    use automerge::{ROOT, ReadDoc};

    tilo_test_env::scenario! {
        /// Reading history and a diff, each case on a document of its own.
        fn history_reads_leave_out_bookkeeping_and_leave_open_edits_alone;

        phase a_feed_leaves_out_the_framework_making_the_record;
        {
            let mut doc = AutoCommit::new();
            doc.put(ROOT, "made", true).unwrap();
            doc.commit_with(
                automerge::transaction::CommitOptions::default()
                    .with_message(tilo_actions::framework::FrameworkChange::Genesis.change_message()),
            );
            doc.put(ROOT, "saved", 1_i64).unwrap();
            doc.commit().unwrap();

            let entries = entries_after(&doc, &[], &TestWords);
            assert_eq!(entries.len(), 1, "{entries:?}");
            assert!(
                !entries[0]["sentence"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("tilo.framework")
            );
        }

        phase history_leaves_out_the_framework_making_the_record_and_its_parts;
        {
            let made = |saved: i64| {
                let mut doc = AutoCommit::new();
                doc.put(ROOT, "made", true).unwrap();
                doc.commit_with(
                    automerge::transaction::CommitOptions::default().with_message(
                        tilo_actions::framework::FrameworkChange::Genesis.change_message(),
                    ),
                );
                doc.put(ROOT, "saved", saved).unwrap();
                doc.commit().unwrap();
                doc
            };
            let (record, part) = (made(1), made(2));
            let beside = [BesideDocument {
                resource: "chain".into(),
                index: 0,
                doc: &part,
            }];
            let listed = history_with(&record, &beside, 50, &TestWords);
            let versions = listed["versions"].as_array().unwrap();
            assert_eq!(versions.len(), 2, "one step each: {listed}");
            assert_eq!(listed["total"], json!(2), "counts what it lists: {listed}");
            assert_eq!(versions[1]["part"], json!(0), "{listed}");
            let last = history_with(&record, &beside, 1, &TestWords);
            assert_eq!(last["versions"].as_array().unwrap().len(), 1, "{last}");
            assert_eq!(last["total"], json!(2), "{last}");
        }

        phase history_and_diff_leave_the_callers_open_edit_uncommitted;
        {
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

    /// Every change says what it did to the resource it is a version of, read from the change
    /// itself as the store reads it: the change that made it, one that edited it, and the one
    /// that removed it, each with its change id.
    #[test]
    fn a_change_says_whether_it_made_edited_or_removed_its_resource() {
        let mut doc = AutoCommit::new();
        let note = doc
            .put_object(ROOT, "r:note1", automerge::ObjType::Map)
            .unwrap();
        doc.put(&note, "k", "tl.test.Note").unwrap();
        let fields = doc.put_object(&note, "f", automerge::ObjType::Map).unwrap();
        doc.put(&fields, "title", "Plan").unwrap();
        doc.commit();
        doc.put(&fields, "title", "Plan B").unwrap();
        doc.commit();
        doc.delete(ROOT, "r:note1").unwrap();
        doc.commit();

        let entries = entries_after(&doc, &[], &TestWords);
        let said: Vec<(&str, &str, &str)> = entries
            .iter()
            .map(|entry| {
                (
                    entry["change"].as_str().unwrap_or_default(),
                    entry["happened"].as_str().unwrap_or_default(),
                    entry["item"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            said.iter()
                .map(|(_change, what, item)| (*what, *item))
                .collect::<Vec<_>>(),
            [
                ("created", "note1"),
                ("edited", "note1"),
                ("removed", "note1"),
            ],
            "{entries:?}"
        );
        assert!(
            said.iter().all(|(change, _, _)| !change.is_empty()),
            "every change keeps its id: {entries:?}"
        );
    }

    /// One step of `actor`'s adding `words` at the end of `body`, under a
    /// gesture of its own: the step history lists, and its change.
    fn wrote(
        doc: &mut AutoCommit,
        body: &automerge::ObjId,
        actor: &[u8],
        words: &str,
    ) -> (String, automerge::ChangeHash) {
        use automerge::transaction::CommitOptions;
        doc.set_actor(automerge::ActorId::from(actor));
        let end = doc.length(body);
        doc.splice_text(body, end, 0, words).unwrap();
        let gesture = tilo_actions::ActionId::new();
        let message = tilo_actions::ActionContext::default()
            .in_gesture(Some(gesture))
            .change_message()
            .unwrap();
        let hash = doc
            .commit_with(CommitOptions::default().with_message(message))
            .unwrap();
        (gesture.0.to_string(), hash)
    }

    fn take_back(doc: &mut AutoCommit, actor: &[u8], change: &automerge::ChangeHash) {
        let scope = Scope::Actor(automerge::ActorId::from(actor).to_hex_string());
        steps::undo_step(doc, &scope, change, |_, _| Ok(None), &TestWords).unwrap();
        doc.commit();
    }

    /// What holds the undo of `step` back, read from the history as a caller reads it.
    fn holding_back(doc: &AutoCommit, step: &str) -> Option<Value> {
        let listed = history(doc, 50, &TestWords);
        let entries = listed["versions"].as_array().unwrap();
        tilo_actions::remote::workspace_invocations::edited_after(entries, |entry| {
            entry["step"] == step
        })
        .cloned()
    }

    #[test]
    fn steps_are_taken_back_newest_first_and_a_later_edit_by_someone_else_holds_one_back() {
        const AGENT: &[u8] = b"agent";
        let mut doc = AutoCommit::new();
        let body = doc
            .put_object(ROOT, "body", automerge::ObjType::Text)
            .unwrap();
        doc.commit();
        let (cover, cover_change) = wrote(&mut doc, &body, AGENT, "Dear Visa Officer,");
        let (_, closing_change) = wrote(&mut doc, &body, AGENT, " Yours faithfully");

        take_back(&mut doc, AGENT, &closing_change);
        let listed = history(&doc, 50, &TestWords);
        let undo = listed["versions"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(
            undo["takes_back"],
            json!([steps::short(&closing_change)]),
            "{undo}"
        );
        assert_eq!(
            holding_back(&doc, &cover),
            None,
            "an undo of a later step holds none back"
        );
        take_back(&mut doc, AGENT, &cover_change);
        assert_eq!(doc.text(&body).unwrap(), "");

        let mut doc = AutoCommit::new();
        let body = doc
            .put_object(ROOT, "body", automerge::ObjType::Text)
            .unwrap();
        doc.commit();
        let (cover, _) = wrote(&mut doc, &body, AGENT, "Dear Visa Officer,");
        let (_, bobs) = wrote(&mut doc, &body, b"bob", " Kind regards");
        assert_eq!(
            holding_back(&doc, &cover).map(|entry| entry["change"].clone()),
            Some(json!(steps::short(&bobs))),
            "someone else's later edit holds it back"
        );
    }
}
