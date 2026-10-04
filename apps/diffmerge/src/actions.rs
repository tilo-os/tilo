//! Diff & Merge's vocabulary: the document's actions, the effects that bring
//! files in, and the view operations that move around what is open.
//!
//! Every change to the session is a [`DiffmergeAction`]: opening a comparison
//! or a merge, taking a side for a hunk, editing the merged result. The chips
//! on the merge panel, the palette, agents and scripts all submit the same
//! values, so they replay identically on every peer and undo as one step
//! each.
//!
//! Reading files from disk or a file chooser is an effect
//! ([`DiffmergeEffect`]) whose result opens a comparison through an action.
//! Scrolling, the active hunk and the mode shown are view state; the
//! operations that move them ([`DiffmergeOperation`]) change nothing in the
//! document.

use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, OperationKind, ParamType, Resource, RetryPolicy,
};
use ui::prelude::Effect;
// The effect derive names its runtime crate by this path.
use ui::tilo_resource;
use ui::tilo_resource::{ActionDecl, PlainText, TextEdit, publish_child};

use crate::doc::{Comparison, MergeDoc, MergeInputs, MergeState};
use crate::model::{BlockKind, ChangeBlock, DiffModel, MergeHunk, MergeModel, Resolution};

/// Registry `type_id` for every Diff action.
pub const DIFFMERGE_ACTION_TYPE: &str = "diffmerge.action";
/// Operation type of the view operations.
pub const DIFFMERGE_OPERATION_TYPE: &str = "diffmerge.operation";
/// Keep global search/contextual expansion bounded even for enormous diffs.
pub const CONTEXTUAL_CHILD_LIMIT: usize = 64;

/// Moving around what is open: no document delta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffmergeOperation {
    ShowDiff,
    ShowMerge,
    PreviousChange,
    NextChange,
    ShowDiffBlock {
        block: u64,
        /// Snapshot fingerprint of the exact aligned rows this address named.
        /// The index is only a fast lookup hint; this prevents a stale palette
        /// result from selecting a newly recomputed, different block.
        fingerprint: String,
    },
}

/// # Errors
///
/// Returns an error when `payload` is not valid JSON for this operation type.
pub fn decode_operation(
    type_id: &str,
    payload: &[u8],
) -> Result<Option<DiffmergeOperation>, String> {
    if type_id != DIFFMERGE_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[allow(
    clippy::expect_used,
    clippy::needless_pass_by_value,
    reason = "the operation is both serialized into the descriptor and inspected for its lifecycle contract; its serde shape is infallible"
)]
fn view(id: &str, label: &str, value: DiffmergeOperation) -> ActionSpec {
    let spec = ActionSpec::operation(
        id,
        label,
        OperationKind::View,
        DIFFMERGE_OPERATION_TYPE,
        &value,
    )
    .expect("DiffmergeOperation is always serializable");
    match value {
        DiffmergeOperation::ShowDiff | DiffmergeOperation::ShowMerge => {
            spec.retry_policy(RetryPolicy::Idempotent)
        }
        DiffmergeOperation::PreviousChange
        | DiffmergeOperation::NextChange
        | DiffmergeOperation::ShowDiffBlock { .. } => spec,
    }
}

/// The view operations on the workspace.
pub(crate) fn view_operations() -> Vec<ActionSpec> {
    vec![
        view(
            "show-diff",
            "Show two-way diff",
            DiffmergeOperation::ShowDiff,
        ),
        view(
            "show-merge",
            "Show three-way merge",
            DiffmergeOperation::ShowMerge,
        ),
        view(
            "previous-change",
            "Previous change",
            DiffmergeOperation::PreviousChange,
        ),
        view("next-change", "Next change", DiffmergeOperation::NextChange),
    ]
}

/// Bringing inputs in from outside the document.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "diffmerge.effects")]
pub enum DiffmergeEffect {
    /// Open files
    #[effect(job, cancellable)]
    OpenFiles {
        #[param("left file path (omit both paths to choose the files)")]
        left: Option<String>,
        #[param("right file path")]
        right: Option<String>,
    },
    /// Load sample
    LoadSample,
}

/// Which side a conflict/change hunk resolution takes. A strict subset of
/// [`Resolution`] — only the user-selectable outcomes — so an action can never
/// carry the internal `Conflict`/`Unchanged`/auto states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HunkChoice {
    /// Take our side (the left / "ours" column).
    Ours,
    /// Take their side (the right / "theirs" column).
    Theirs,
    /// Take both, ours then theirs.
    Both,
}

impl HunkChoice {
    /// Stable wire tag stored in the resolutions map.
    pub fn as_str(self) -> &'static str {
        match self {
            HunkChoice::Ours => "ours",
            HunkChoice::Theirs => "theirs",
            HunkChoice::Both => "both",
        }
    }

    /// Parse a stored resolutions-map tag back into a choice. An unknown tag is
    /// `None`, so a malformed stored value is ignored rather than panicking.
    pub fn from_tag(s: &str) -> Option<Self> {
        match s {
            "ours" => Some(HunkChoice::Ours),
            "theirs" => Some(HunkChoice::Theirs),
            "both" => Some(HunkChoice::Both),
            _ => None,
        }
    }

    /// The [`Resolution`] this choice sets on a hunk.
    pub fn to_resolution(self) -> Resolution {
        match self {
            HunkChoice::Ours => Resolution::TookOurs,
            HunkChoice::Theirs => Resolution::TookTheirs,
            HunkChoice::Both => Resolution::TookBoth,
        }
    }
}

/// Every change to the diff/merge session.
///
/// The merge is recomputed inside [`Action::apply`] from the inputs the
/// document holds, a pure function of them, so every peer composes the same
/// result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffmergeAction {
    /// Open two files side by side.
    OpenComparison {
        left_label: String,
        left: String,
        right_label: String,
        right: String,
    },
    /// Open a three-way merge: its inputs, a merged result with no side taken
    /// yet, and no recorded resolutions.
    OpenMerge {
        base_label: String,
        base: String,
        ours_label: String,
        ours: String,
        theirs_label: String,
        theirs: String,
    },
    /// Resolve change/conflict hunk `hunk` by taking a side: records the
    /// choice and recomposes the merged result. Refused once the result was
    /// edited by hand, which taking a side would overwrite.
    ResolveHunk { hunk: u64, choice: HunkChoice },
    /// One edit of the merged result.
    EditMerged(TextEdit),
}

fn written(error: &automerge::AutomergeError) -> ActionError {
    ActionError::Refused(format!("the document refused the write: {error}"))
}

impl Action for DiffmergeAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            DiffmergeAction::OpenComparison {
                left_label,
                left,
                right_label,
                right,
            } => Comparison {
                left_label: left_label.clone(),
                left: left.clone(),
                right_label: right_label.clone(),
                right: right.clone(),
            }
            .write(doc)
            .map_err(|e| written(&e)),
            DiffmergeAction::OpenMerge {
                base_label,
                base,
                ours_label,
                ours,
                theirs_label,
                theirs,
            } => {
                let inputs = MergeInputs {
                    base_label: base_label.clone(),
                    base: base.clone(),
                    ours_label: ours_label.clone(),
                    ours: ours.clone(),
                    theirs_label: theirs_label.clone(),
                    theirs: theirs.clone(),
                };
                inputs.write(doc).map_err(|e| written(&e))?;
                MergeDoc::clear_resolutions(doc).map_err(|e| written(&e))?;
                MergeDoc::set_merged(doc, &inputs.model().merged_text()).map_err(|e| written(&e))
            }
            DiffmergeAction::ResolveHunk { hunk, choice } => {
                let state = MergeState::from_doc(doc);
                if state.merge.is_none() {
                    return Err(ActionError::Refused("no merge is open".into()));
                }
                let index = usize::try_from(*hunk).unwrap_or(usize::MAX);
                if !state.model.hunks.get(index).is_some_and(|h| h.is_change) {
                    return Err(ActionError::Refused(format!(
                        "the merge has no change {}",
                        hunk.saturating_add(1)
                    )));
                }
                if !state.composed {
                    return Err(ActionError::Refused(
                        "the merged output was edited by hand; resolve the rest in it".into(),
                    ));
                }
                MergeDoc::set_resolution(doc, index, *choice).map_err(|e| written(&e))?;
                MergeDoc::set_merged(doc, &compose_choice(&state.model, index, *choice))
                    .map_err(|e| written(&e))
            }
            DiffmergeAction::EditMerged(edit) => {
                let text = MergeDoc::merged_text_obj(doc).map_err(|e| written(&e))?;
                edit.apply_under(doc, &text)
            }
        }
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register Diff's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<DiffmergeAction>(DIFFMERGE_ACTION_TYPE);
}

/// Encode a [`DiffmergeAction`] into its wire form.
///
/// # Panics
///
/// Never in practice: every [`DiffmergeAction`] field is a plain
/// string/number/enum, which `serde_json` cannot fail to serialize.
#[allow(
    clippy::expect_used,
    reason = "DiffmergeAction's fields are always JSON-serializable; see doc comment"
)]
pub fn encode(action: &DiffmergeAction) -> EncodedAction {
    EncodedAction::encode(DIFFMERGE_ACTION_TYPE, action)
        .expect("DiffmergeAction is always serializable")
}

/// The take-a-side action for `hunk`.
pub fn resolve_hunk_action(hunk: usize, choice: HunkChoice) -> DiffmergeAction {
    DiffmergeAction::ResolveHunk {
        hunk: hunk as u64,
        choice,
    }
}

/// Replace the merged result with `text`, kept newline-terminated like every
/// line the merge composes.
pub fn replace_merged_action(text: &str) -> DiffmergeAction {
    DiffmergeAction::EditMerged(TextEdit::ReplaceText {
        text: ensure_trailing_newline(text),
    })
}

/// Open the two samples as a comparison.
pub fn open_comparison_action(
    left: &crate::samples::Sample,
    right: &crate::samples::Sample,
) -> DiffmergeAction {
    DiffmergeAction::OpenComparison {
        left_label: left.label.clone(),
        left: left.text.clone(),
        right_label: right.label.clone(),
        right: right.text.clone(),
    }
}

/// Open the three samples as a merge.
pub fn open_merge_action(
    base: &crate::samples::Sample,
    ours: &crate::samples::Sample,
    theirs: &crate::samples::Sample,
) -> DiffmergeAction {
    DiffmergeAction::OpenMerge {
        base_label: base.label.clone(),
        base: base.text.clone(),
        ours_label: ours.label.clone(),
        ours: ours.text.clone(),
        theirs_label: theirs.label.clone(),
        theirs: theirs.text.clone(),
    }
}

/// The bundled sample comparison and merge, as the actions that open them.
pub fn sample_actions() -> [DiffmergeAction; 2] {
    let (left, right) = crate::samples::diff_pair();
    let (base, ours, theirs) = crate::samples::merge_triple();
    [
        open_comparison_action(&left, &right),
        open_merge_action(&base, &ours, &theirs),
    ]
}

/// The declared actions on the workspace.
pub(crate) fn declarations() -> Vec<ActionDecl<crate::resource::Diffmerge>> {
    let text = |name: &'static str, description: &'static str| {
        ActionParam::required(name, ParamType::Text, description)
    };
    let label = |name: &'static str, description: &'static str| {
        ActionParam::optional(name, ParamType::Text, description)
    };
    let arg = |args: &tilo_actions::ActionArgs, name: &str| {
        args.text(name).unwrap_or_default().to_owned()
    };
    vec![
        ActionDecl::new(
            "open-comparison",
            "Compare two texts",
            vec![
                label("left_label", "name of the left side"),
                text("left", "the left text"),
                label("right_label", "name of the right side"),
                text("right", "the right text"),
            ],
            move |args, _| {
                Ok(DiffmergeAction::OpenComparison {
                    left_label: arg(args, "left_label"),
                    left: arg(args, "left"),
                    right_label: arg(args, "right_label"),
                    right: arg(args, "right"),
                })
            },
        ),
        ActionDecl::new(
            "open-merge",
            "Merge three texts",
            vec![
                label("base_label", "name of the common base"),
                text("base", "the common base text"),
                label("ours_label", "name of our side"),
                text("ours", "our text"),
                label("theirs_label", "name of their side"),
                text("theirs", "their text"),
            ],
            move |args, _| {
                Ok(DiffmergeAction::OpenMerge {
                    base_label: arg(args, "base_label"),
                    base: arg(args, "base"),
                    ours_label: arg(args, "ours_label"),
                    ours: arg(args, "ours"),
                    theirs_label: arg(args, "theirs_label"),
                    theirs: arg(args, "theirs"),
                })
            },
        ),
        ActionDecl::new(
            "edit-merged",
            "Replace merged output",
            vec![text("text", "the merged result")],
            move |args, _| Ok(replace_merged_action(&arg(args, "text"))),
        ),
    ]
}

/// The id the merged result is published under, as a plain-text child of
/// the workspace.
pub(crate) fn merged_resource_id() -> String {
    ui::tilo_resource::child_resource_id::<crate::resource::Diffmerge>("merged")
}

/// What the workspace publishes inside itself: the merged result, the
/// merge's hunks while sides can still be taken, and the comparison's
/// change blocks.
pub(crate) fn document_children(state: &MergeState) -> Vec<Resource> {
    let mut children = Vec::new();
    if state.merge.is_some() {
        children.push(publish_child::<PlainText, crate::resource::Diffmerge>(
            &merged_resource_id(),
            "Merged output",
            Some(&state.merged),
            DiffmergeAction::EditMerged,
        ));
    }
    if state.composed {
        children.extend(hunk_children(&state.model));
    }
    if let Some(comparison) = &state.comparison {
        children.push(comparison_resource(comparison));
    }
    children
}

fn hunk_children(model: &MergeModel) -> Vec<Resource> {
    let mut children = Vec::new();
    let mut ordinal = 0usize;
    let mut identities = std::collections::BTreeMap::<String, usize>::new();
    for (i, h) in model.hunks.iter().enumerate() {
        if !h.is_change {
            continue;
        }
        ordinal += 1;
        let tag = if h.resolution.is_conflict() {
            "Conflict"
        } else {
            "Change"
        };
        let preview = hunk_preview(h);
        let label = if preview.is_empty() {
            format!("{tag} {ordinal}")
        } else {
            format!("{tag} {ordinal}: {preview}")
        };

        let identity = hunk_resource_id(h, 0);
        let occurrence = identities.entry(identity).or_default();
        let resource_id = hunk_resource_id(h, *occurrence);
        *occurrence = occurrence.saturating_add(1);
        let custom = model.clone();
        children.push(
            Resource::new(resource_id, "diffmerge.conflict", label)
                .with_action(immediate_choice(
                    i,
                    HunkChoice::Ours,
                    "take-ours",
                    "Take ours",
                ))
                .with_action(immediate_choice(
                    i,
                    HunkChoice::Theirs,
                    "take-theirs",
                    "Take theirs",
                ))
                .with_action(immediate_choice(
                    i,
                    HunkChoice::Both,
                    "take-both",
                    "Take both",
                ))
                .with_action(ActionSpec::prompted(
                    "resolve",
                    "Resolve with custom text",
                    "Replacement",
                    "type the resolved line(s)…",
                    ExecutionLocus::ClientLocal,
                    move |text: &str| {
                        encode(&replace_merged_action(&compose_custom(&custom, i, text)))
                    },
                )),
        );
    }
    children
}

/// Compose the merged output with `hunk`'s contribution forced to `choice`,
/// every other hunk keeping its current resolution.
pub fn compose_choice(model: &MergeModel, hunk: usize, choice: HunkChoice) -> String {
    compose(model, hunk, choice_lines(model.hunks.get(hunk), choice))
}

/// Compose the merged output with `hunk` replaced by the user's typed `text`,
/// every other hunk keeping its current resolution. Backs the per-hunk
/// "resolve with custom text" affordance.
pub fn compose_custom(model: &MergeModel, hunk: usize, text: &str) -> String {
    compose(
        model,
        hunk,
        text.lines().map(std::string::ToString::to_string).collect(),
    )
}

fn compose(model: &MergeModel, hunk: usize, replacement: Vec<String>) -> String {
    let mut out = String::new();
    let mut replacement = Some(replacement);
    for (i, h) in model.hunks.iter().enumerate() {
        let lines = if i == hunk {
            replacement.take().unwrap_or_default()
        } else {
            h.resolved_lines()
        };
        for l in lines {
            out.push_str(&l);
            out.push('\n');
        }
    }
    out
}

fn choice_lines(h: Option<&MergeHunk>, choice: HunkChoice) -> Vec<String> {
    let Some(h) = h else {
        return Vec::new();
    };
    match choice {
        HunkChoice::Ours => h.ours.clone(),
        HunkChoice::Theirs => h.theirs.clone(),
        HunkChoice::Both => {
            let mut v = h.ours.clone();
            v.extend(h.theirs.clone());
            v
        }
    }
}

/// Stable digest of the exact aligned rows addressed by a change block.
///
/// This is deliberately derived from semantic row content, not its first/last
/// visual line number: recomputing after a source edit can shift an unrelated
/// block into the same index and range. The operation still carries its index
/// as an efficient lookup hint, but the receiver must compare this digest
/// before navigating.
pub fn diff_block_fingerprint(model: &DiffModel, index: usize) -> Option<String> {
    let block = model.blocks.get(index)?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo.diffmerge.diff-block.v1\0");
    hasher.update(&[match block.kind {
        BlockKind::Added => 1,
        BlockKind::Removed => 2,
        BlockKind::Modified => 3,
    }]);
    let rows = model.rows.get(block.first_row..=block.last_row)?;
    for row in rows {
        hasher.update(&[match row.kind {
            crate::model::RowKind::Equal => 0,
            crate::model::RowKind::Insert => 1,
            crate::model::RowKind::Delete => 2,
            crate::model::RowKind::Modify => 3,
        }]);
        for text in [&row.left, &row.right] {
            match text {
                Some(text) => {
                    hasher.update(&[1]);
                    hasher.update(&(text.len() as u64).to_le_bytes());
                    hasher.update(text.as_bytes());
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
    }
    Some(hasher.finalize().to_hex().to_string())
}

fn diff_block_resource_id(block: &ChangeBlock, fingerprint: &str, occurrence: usize) -> String {
    let kind = match block.kind {
        BlockKind::Added => "added",
        BlockKind::Removed => "removed",
        BlockKind::Modified => "modified",
    };
    format!("diffmerge.diff-block:{kind}:{fingerprint}:{occurrence}")
}

/// The open comparison, with one child per change block (bounded) that
/// shows it.
pub fn comparison_resource(comparison: &Comparison) -> Resource {
    let model = comparison.model();
    let mut root = Resource::new(
        "diffmerge.comparison",
        "diffmerge.comparison",
        comparison.title(),
    );
    let mut fingerprint_occurrences = std::collections::HashMap::<String, usize>::new();
    for (index, block) in model.blocks.iter().take(CONTEXTUAL_CHILD_LIMIT).enumerate() {
        let kind = match block.kind {
            BlockKind::Added => "Added",
            BlockKind::Removed => "Removed",
            BlockKind::Modified => "Modified",
        };
        let label = format!(
            "{kind} lines {}–{}",
            block.first_row.saturating_add(1),
            block.last_row.saturating_add(1)
        );
        let Some(fingerprint) = diff_block_fingerprint(&model, index) else {
            continue;
        };
        let occurrence = fingerprint_occurrences
            .entry(fingerprint.clone())
            .and_modify(|count| *count += 1)
            .or_insert(0);
        let action = DiffmergeOperation::ShowDiffBlock {
            block: u64::try_from(index).unwrap_or(u64::MAX),
            fingerprint: fingerprint.clone(),
        };
        root = root.with_child(
            Resource::new(
                diff_block_resource_id(block, &fingerprint, *occurrence),
                "diffmerge.diff-block",
                label,
            )
            .with_action(view("show", "Show change", action)),
        );
    }
    root
}

#[allow(
    clippy::expect_used,
    reason = "DiffmergeAction's fields are always JSON-serializable; see encode's doc comment"
)]
fn immediate_choice(hunk: usize, choice: HunkChoice, id: &str, label: &str) -> ActionSpec {
    ActionSpec::immediate(
        id,
        label,
        DIFFMERGE_ACTION_TYPE,
        &resolve_hunk_action(hunk, choice),
    )
    .expect("encode diffmerge action")
    .reversible()
    .retry_policy(RetryPolicy::Idempotent)
}

/// A short one-line preview of a hunk for the palette label.
fn hunk_preview(h: &MergeHunk) -> String {
    let first = h
        .ours
        .iter()
        .chain(h.theirs.iter())
        .chain(h.base.iter())
        .find(|l| !l.trim().is_empty())
        .cloned()
        .unwrap_or_default();
    let mut t: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 44 {
        t = t.chars().take(41).collect::<String>() + "…";
    }
    t
}

/// Content-derived identity for a merge hunk. The model can be recomputed or
/// reordered without making an old address point at a different conflict.
/// The occurrence number only distinguishes byte-equal hunks, which are
/// semantically interchangeable until one changes.
fn hunk_resource_id(h: &MergeHunk, occurrence: usize) -> String {
    let mut hasher = fnv::FnvHasher::default();
    for (section, lines) in [&h.base, &h.ours, &h.theirs].into_iter().enumerate() {
        // Separate base/ours/theirs as well as individual lines: otherwise
        // `base=[a], ours=[b]` would hash like `base=[a,b], ours=[]`.
        std::hash::Hasher::write_u8(&mut hasher, u8::try_from(section).unwrap_or(0));
        for line in lines {
            std::hash::Hasher::write(&mut hasher, line.as_bytes());
            std::hash::Hasher::write_u8(&mut hasher, 0xff);
        }
    }
    let hash = std::hash::Hasher::finish(&hasher);
    format!("diffmerge.conflict:{hash:016x}:{occurrence}")
}

/// The merged buffer stores each line newline-terminated; an edit that omits
/// the trailing newline gets one so it stays consistent.
fn ensure_trailing_newline(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Diffmerge;
    use automerge::AutoCommit;
    use tilo_actions::{ActionInput, LocalTransport};
    use ui::tilo_resource::Resource as _;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn apply(doc: &mut AutoCommit, action: &DiffmergeAction) -> Result<(), ActionError> {
        let encoded = encode(action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).map(|_| ())
    }

    fn conflict_merge() -> DiffmergeAction {
        DiffmergeAction::OpenMerge {
            base_label: "base".into(),
            base: "a\nb\nc\n".into(),
            ours_label: "ours".into(),
            ours: "a\nOURS\nc\n".into(),
            theirs_label: "theirs".into(),
            theirs: "a\nTHEIRS\nc\n".into(),
        }
    }

    /// A merge with exactly one real conflict, opened through its action.
    fn opened_merge() -> AutoCommit {
        let mut doc = AutoCommit::new();
        Diffmerge::initialize(&mut doc).unwrap();
        apply(&mut doc, &conflict_merge()).unwrap();
        apply(
            &mut doc,
            &DiffmergeAction::OpenComparison {
                left_label: "before.rs".into(),
                left: "a\nb\nc\n".into(),
                right_label: "after.rs".into(),
                right: "a\nB\nc\nnew\n".into(),
            },
        )
        .unwrap();
        doc.commit();
        doc
    }

    fn conflict(doc: &AutoCommit) -> usize {
        MergeState::from_doc(doc)
            .model
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
            .expect("one conflict")
    }

    fn published(doc: &AutoCommit) -> Vec<Resource> {
        let state = Diffmerge::read(doc).unwrap();
        vec![ui::tilo_resource::publish::<Diffmerge>(
            Diffmerge::KIND,
            &state.title,
            &state,
        )]
    }

    ui::action_contract_tests! {
        seed: opened_merge(),
        registry: registry(),
        resources: |doc| published(doc),
        palette: ["Take theirs", "Replace merged output"],
        replay: [
            take_both => |doc| encode(&resolve_hunk_action(conflict(doc), HunkChoice::Both)),
            replace_merged => |_| encode(&replace_merged_action("a\nmerged\nc\n")),
        ],
        undo: [
            take_theirs => |doc| encode(&resolve_hunk_action(conflict(doc), HunkChoice::Theirs)),
            replace_merged => |_| encode(&replace_merged_action("a\nmerged\nc\n")),
        ],
        encodes: [
            take_theirs => ("diffmerge.conflict", "take-theirs", |doc, _| {
                encode(&resolve_hunk_action(conflict(doc), HunkChoice::Theirs))
            }),
            custom_resolve => ("diffmerge.conflict", "resolve", arg: "CUSTOM", |doc, _| {
                let state = MergeState::from_doc(doc);
                encode(&replace_merged_action(&compose_custom(&state.model, conflict(doc), "CUSTOM")))
            }),
        ],
    }

    /// Opening a merge writes its inputs and a merged result with no side
    /// taken; taking a side records it and recomposes the result; a hand
    /// edit stops sides from being taken, and an unknown hunk is refused.
    #[test]
    fn opening_and_resolving_a_merge_goes_through_the_document() {
        let mut doc = opened_merge();
        let hunk = conflict(&doc);
        assert!(MergeDoc::merged_text(&doc).contains("OURS"));

        apply(&mut doc, &resolve_hunk_action(hunk, HunkChoice::Theirs)).unwrap();
        let merged = MergeDoc::merged_text(&doc);
        assert!(
            merged.contains("THEIRS") && !merged.contains("OURS"),
            "{merged:?}"
        );
        assert_eq!(
            MergeDoc::resolutions(&doc),
            vec![(hunk, "theirs".to_string())]
        );
        assert!(MergeState::from_doc(&doc).composed);

        assert!(apply(&mut doc, &resolve_hunk_action(9999, HunkChoice::Ours)).is_err());

        apply(&mut doc, &replace_merged_action("hand made")).unwrap();
        assert_eq!(MergeDoc::merged_text(&doc), "hand made\n");
        let refused = apply(&mut doc, &resolve_hunk_action(hunk, HunkChoice::Ours));
        assert!(refused.is_err(), "a hand edit is never overwritten");
        assert_eq!(MergeDoc::merged_text(&doc), "hand made\n");
        assert!(
            published(&doc)[0]
                .children
                .iter()
                .all(|child| child.kind != "diffmerge.conflict"),
            "no side is offered once the result was edited"
        );

        apply(&mut doc, &conflict_merge()).unwrap();
        assert!(
            MergeDoc::resolutions(&doc).is_empty(),
            "a new merge starts unresolved"
        );
        assert!(MergeState::from_doc(&doc).composed);
    }

    /// The workspace publishes the merge's conflicts with content-based ids,
    /// the merged result as a text child, the comparison's change blocks as
    /// bounded view actions, and the open/edit declarations.
    #[test]
    fn the_workspace_publishes_conflicts_text_and_change_blocks() {
        let doc = opened_merge();
        let root = &published(&doc)[0];
        let actions: Vec<&str> = root.actions.iter().map(|a| a.id.as_str()).collect();
        for id in ["open-comparison", "open-merge", "edit-merged"] {
            assert!(actions.contains(&id), "{id}: {actions:?}");
        }
        let conflict = root
            .children
            .iter()
            .find(|r| r.kind == "diffmerge.conflict")
            .expect("a conflict resource");
        let ids: Vec<&str> = conflict.actions.iter().map(|a| a.id.as_str()).collect();
        for id in ["take-ours", "take-theirs", "take-both", "resolve"] {
            assert!(ids.contains(&id), "{id}: {ids:?}");
        }
        assert!(
            root.children
                .iter()
                .any(|child| child.id.as_str() == merged_resource_id()),
            "the merged result is a text child"
        );

        let state = MergeState::from_doc(&doc);
        let model = state.comparison.as_ref().unwrap().model();
        let comparison = root
            .children
            .iter()
            .find(|r| r.kind == "diffmerge.comparison")
            .expect("the comparison");
        assert_eq!(comparison.children.len(), model.blocks.len());
        for (index, child) in comparison.children.iter().enumerate() {
            let show = child.actions.iter().find(|a| a.id == "show").unwrap();
            assert_eq!(show.kind, OperationKind::View);
            let ActionInput::Immediate(encoded) = &show.input else {
                panic!("show is immediate");
            };
            let fingerprint = diff_block_fingerprint(&model, index).unwrap();
            assert_eq!(
                decode_operation(DIFFMERGE_OPERATION_TYPE, &encoded.payload)
                    .unwrap()
                    .unwrap(),
                DiffmergeOperation::ShowDiffBlock {
                    block: u64::try_from(index).unwrap(),
                    fingerprint: fingerprint.clone(),
                }
            );
            assert!(child.id.0.contains(&fingerprint));
        }

        let first = DiffModel::compute("unchanged\nold\n", "unchanged\nnew\n");
        let second = DiffModel::compute("unchanged\nold\n", "unchanged\nnewer\n");
        assert_ne!(
            diff_block_fingerprint(&first, 0),
            diff_block_fingerprint(&second, 0),
            "a content address must not survive a changed hunk at the same index"
        );

        let h = &state.model.hunks[conflict_index(&state)];
        assert_eq!(hunk_resource_id(h, 0), hunk_resource_id(&h.clone(), 0));
        assert_ne!(
            hunk_resource_id(h, 0),
            hunk_resource_id(h, 1),
            "byte-identical conflicts still receive distinct addresses"
        );
    }

    fn conflict_index(state: &MergeState) -> usize {
        state
            .model
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
            .unwrap()
    }

    /// The sample merge has a conflict to resolve through the palette.
    #[test]
    fn the_sample_merge_exposes_a_conflict() {
        let mut doc = AutoCommit::new();
        for action in sample_actions() {
            apply(&mut doc, &action).unwrap();
        }
        assert!(
            published(&doc)[0]
                .children
                .iter()
                .any(|r| r.kind == "diffmerge.conflict")
        );
    }
}
