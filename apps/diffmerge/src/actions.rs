//! Diff's exported **actions** and the helpers that feed [`tilo_resource::publish`].
//!
//! The document-mutating surface of a merge is the *merge result*: the resolved
//! output (an Automerge `Text`) plus the per-hunk resolution choices (ours/
//! theirs/both), converging across peers as [`DiffmergeAction`]s. The exact
//! same [`EncodedAction`] is what the on-panel chips and Ctrl+K both dispatch —
//! one source of truth, one replay path. Shared conflict children,
//! `edit-merged`, and View/Effect ops live on the framework resource; local
//! two-way comparisons stay live extras when hosted.
//!
//! Everything else — scroll, focus, the active hunk, sync-scroll, loading the
//! input blobs and recomputing the diff — is plain app code, never an action.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource, ResultSpec, RetryPolicy,
};

use crate::doc::{MergeDoc, MergeState};
use crate::model::{BlockKind, ChangeBlock, DiffModel, MergeHunk, MergeModel, Resolution};

/// Registry `type_id` for every Diff action.
pub const DIFFMERGE_ACTION_TYPE: &str = "diffmerge.action";
pub const DIFFMERGE_OPERATION_TYPE: &str = "diffmerge.operation";
/// Keep global search/contextual expansion bounded even for enormous diffs.
pub const CONTEXTUAL_CHILD_LIMIT: usize = 64;

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
    OpenFiles {
        left: Option<String>,
        right: Option<String>,
    },
    LoadSample,
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
fn operation(id: &str, label: &str, kind: OperationKind, value: DiffmergeOperation) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, DIFFMERGE_OPERATION_TYPE, &value)
        .expect("DiffmergeOperation is always serializable");
    match value {
        DiffmergeOperation::ShowDiff | DiffmergeOperation::ShowMerge => {
            spec.retry_policy(RetryPolicy::Idempotent)
        }
        DiffmergeOperation::PreviousChange
        | DiffmergeOperation::NextChange
        | DiffmergeOperation::ShowDiffBlock { .. }
        | DiffmergeOperation::OpenFiles { .. }
        | DiffmergeOperation::LoadSample => spec,
    }
}

/// View / Effect catalogue on the workspace door (no document delta).
#[allow(
    clippy::expect_used,
    reason = "the typed local operation has an infallible serde shape"
)]
pub(crate) fn root_local_operations() -> Vec<ActionSpec> {
    let mut actions = vec![
        ActionSpec::typed_operation(
            "open-files",
            "Open files",
            "left and right paths (omit both to use system file pickers)",
            OperationKind::Effect,
            DIFFMERGE_OPERATION_TYPE,
            vec![
                ActionParam::optional("left", ParamType::Text, "left file path"),
                ActionParam::optional("right", ParamType::Text, "right file path"),
            ],
            |args: &ActionArgs| {
                EncodedAction::encode_operation(
                    DIFFMERGE_OPERATION_TYPE,
                    &DiffmergeOperation::OpenFiles {
                        left: args
                            .text("left")
                            .filter(|p| !p.is_empty())
                            .map(str::to_owned),
                        right: args
                            .text("right")
                            .filter(|p| !p.is_empty())
                            .map(str::to_owned),
                    },
                )
                .expect("DiffmergeOperation is always serializable")
            },
        )
        .execution(ExecutionMode::Background { cancellable: true })
        .returns(ResultSpec::Job),
    ];
    for (id, label, kind, value) in [
        (
            "show-diff",
            "Show two-way diff",
            OperationKind::View,
            DiffmergeOperation::ShowDiff,
        ),
        (
            "show-merge",
            "Show three-way merge",
            OperationKind::View,
            DiffmergeOperation::ShowMerge,
        ),
        (
            "previous-change",
            "Previous change",
            OperationKind::View,
            DiffmergeOperation::PreviousChange,
        ),
        (
            "next-change",
            "Next change",
            OperationKind::View,
            DiffmergeOperation::NextChange,
        ),
        (
            "load-sample",
            "Load sample",
            OperationKind::Effect,
            DiffmergeOperation::LoadSample,
        ),
    ] {
        actions.push(operation(id, label, kind, value));
    }
    actions
}

/// Standalone / empty-workspace door used by tests that still speak the old
/// resource shape. Prefer [`tilo_resource::publish`] via [`provide_resources`].
#[allow(dead_code, reason = "framework migration test helper")]
pub(crate) fn workspace_operations() -> Resource {
    let mut root = Resource::new("diffmerge.workspace", "diffmerge.workspace", "Diff & Merge");
    for spec in root_local_operations() {
        root = root.with_action(spec);
    }
    root
}

/// Document actions bound from live merge state for [`tilo_resource::publish`].
pub(crate) fn document_bound_actions(_state: &MergeState) -> Vec<ActionSpec> {
    vec![ActionSpec::prompted(
        "edit-merged",
        "Replace merged output",
        "Merged text",
        "the merged result…",
        ExecutionLocus::ClientLocal,
        move |text: &str| encode(&set_merged_action(&ensure_trailing_newline(text))),
    )]
}

/// Conflict / change children for [`tilo_resource::publish`].
pub(crate) fn document_children(state: &MergeState) -> Vec<Resource> {
    let mut children = Vec::new();
    let mut ordinal = 0usize;
    let mut identities = std::collections::BTreeMap::<String, usize>::new();
    for (i, h) in state.model.hunks.iter().enumerate() {
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
        let mut hunk_res = Resource::new(resource_id, "diffmerge.conflict", label);
        hunk_res = hunk_res.with_action(immediate_choice(
            &state.model,
            i,
            HunkChoice::Ours,
            "take-ours",
            "Take ours",
        ));
        hunk_res = hunk_res.with_action(immediate_choice(
            &state.model,
            i,
            HunkChoice::Theirs,
            "take-theirs",
            "Take theirs",
        ));
        hunk_res = hunk_res.with_action(immediate_choice(
            &state.model,
            i,
            HunkChoice::Both,
            "take-both",
            "Take both",
        ));

        let model = state.model.clone();
        hunk_res = hunk_res.with_action(ActionSpec::prompted(
            "resolve",
            "Resolve with custom text",
            "Replacement",
            "type the resolved line(s)…",
            ExecutionLocus::ClientLocal,
            move |text: &str| encode(&set_merged_action(&compose_custom(&model, i, text))),
        ));

        children.push(hunk_res);
    }
    children
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
    /// `None`, so a malformed persisted value is ignored rather than panicking.
    ///
    /// Named `from_tag` rather than `from_str` so it isn't confused for
    /// [`std::str::FromStr::from_str`] — that trait returns `Result`, not the
    /// deliberate `Option` this "ignore, don't panic" contract needs.
    pub fn from_tag(s: &str) -> Option<Self> {
        match s {
            "ours" => Some(HunkChoice::Ours),
            "theirs" => Some(HunkChoice::Theirs),
            "both" => Some(HunkChoice::Both),
            _ => None,
        }
    }

    /// The [`Resolution`] this choice sets on a hunk for immediate render.
    pub fn to_resolution(self) -> Resolution {
        match self {
            HunkChoice::Ours => Resolution::TookOurs,
            HunkChoice::Theirs => Resolution::TookTheirs,
            HunkChoice::Both => Resolution::TookBoth,
        }
    }
}

/// Every collaborative operation Diff exports on its merge result.
///
/// Determinism contract: the recomposed merged text is composed at *authoring*
/// time (the app has the external hunks) and carried in the payload, never
/// recomputed inside [`Action::apply`], which touches neither the RNG nor the
/// wall-clock. A malformed payload is a **no-op** rather than an error, so a bad
/// replay can't wedge a peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffmergeAction {
    /// Resolve change/conflict hunk `hunk` by taking a side. Records the choice
    /// in the resolutions map (so the chip state converges on peers) and writes
    /// the recomposed merged buffer `merged` — both inside one change, so one
    /// undo boundary covers the resolution.
    ResolveHunk {
        hunk: u64,
        choice: HunkChoice,
        merged: String,
    },
    /// Replace the shared merged-output buffer with `merged` — a free-form edit
    /// to the collaboratively-editable result (also how a custom per-hunk
    /// resolution lands). Splice-diffed against the current text, so the CRDT
    /// records a minimal edit.
    SetMerged { merged: String },
    /// Open a new merge: drop resolutions that belonged to the previous inputs
    /// and install the provisional merged buffer. One history step.
    SeedMerge { merged: String },
    /// Replace the shared three-way input projection (same grain as git-client's
    /// shared repo write). Local two-way comparisons are never projected.
    WriteSharedMerge { json: String },
}

impl Action for DiffmergeAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            DiffmergeAction::ResolveHunk {
                hunk,
                choice,
                merged,
            } => {
                MergeDoc::set_resolution(doc, *hunk as usize, choice.as_str());
                MergeDoc::set_merged(doc, merged);
            }
            DiffmergeAction::SetMerged { merged } => {
                MergeDoc::set_merged(doc, merged);
            }
            DiffmergeAction::SeedMerge { merged } => {
                MergeDoc::clear_resolutions(doc);
                MergeDoc::set_merged(doc, merged);
            }
            DiffmergeAction::WriteSharedMerge { json } => {
                crate::doc::SharedMerge::write_json(doc, json);
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register Diff's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<DiffmergeAction>(DIFFMERGE_ACTION_TYPE);
}

/// Encode any [`DiffmergeAction`] into the wire form the palette and the on-panel
/// chips both dispatch. This is the single point that turns Diff's actions into
/// replayable envelopes.
///
/// Every [`DiffmergeAction`] variant carries only plain `String`/`usize`/enum
/// fields, which `serde_json` cannot fail to serialize — see
/// `crate::actions::immediate_choice` for the same contract on the palette
/// side.
///
/// # Panics
///
/// Never in practice: every [`DiffmergeAction`] field is a plain
/// `String`/`usize`/enum, which `serde_json` cannot fail to serialize.
#[allow(
    clippy::expect_used,
    reason = "DiffmergeAction's fields are always JSON-serializable; see doc comment"
)]
pub fn encode(action: &DiffmergeAction) -> EncodedAction {
    EncodedAction::encode(DIFFMERGE_ACTION_TYPE, action)
        .expect("DiffmergeAction is always serializable")
}

// ===========================================================================
// Action builders + composition — the single source of truth shared by the
// on-panel chips (see `crate::merge_view`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Build the "take a side" action for `hunk`, baking in the recomposed merged
/// output so `apply` stays pure. Used by both the chip handler and the palette
/// spec, so they encode byte-identically.
pub fn resolve_hunk_action(hunk: usize, choice: HunkChoice, merged: String) -> DiffmergeAction {
    DiffmergeAction::ResolveHunk {
        hunk: hunk as u64,
        choice,
        merged,
    }
}

/// Build the "replace merged output" action.
pub fn set_merged_action(merged: &str) -> DiffmergeAction {
    DiffmergeAction::SetMerged {
        merged: merged.to_string(),
    }
}

/// Compose the merged output with `hunk`'s contribution forced to `choice`,
/// every other hunk keeping its current resolution. Pure over `model`, so the
/// chip handler and the palette spec produce the identical text regardless of
/// order.
pub fn compose_choice(model: &MergeModel, hunk: usize, choice: HunkChoice) -> String {
    let mut out = String::new();
    for (i, h) in model.hunks.iter().enumerate() {
        let lines = if i == hunk {
            choice_lines(h, choice)
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

/// Compose the merged output with `hunk` replaced by the user's typed `text`,
/// every other hunk keeping its current resolution. Backs the per-hunk
/// "resolve with custom text" affordance.
pub fn compose_custom(model: &MergeModel, hunk: usize, text: &str) -> String {
    let custom: Vec<String> = text.lines().map(std::string::ToString::to_string).collect();
    let mut out = String::new();
    for (i, h) in model.hunks.iter().enumerate() {
        let lines = if i == hunk {
            custom.clone()
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

fn choice_lines(h: &MergeHunk, choice: HunkChoice) -> Vec<String> {
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

/// The presentation context the app hands [`provide_resources`]: the merge's
/// human title and a snapshot of the (external-derived) merge model the
/// per-hunk specs are composed from.
#[derive(Debug, Clone)]
pub struct MergeResourceCtx {
    pub title: String,
    pub model: MergeModel,
}

/// Presentation-only context for the currently open two-way comparison. The
/// input bytes and their absolute paths stay device-local; the published
/// resource identifiers derive only from displayed labels and block contents.
#[derive(Clone)]
pub struct DiffResourceCtx {
    pub left_label: String,
    pub right_label: String,
    pub model: DiffModel,
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

/// Local two-way comparison resource — live when hosted, never document state.
pub fn local_comparison_resource(ctx: &DiffResourceCtx) -> Resource {
    let title = if ctx.left_label.is_empty() && ctx.right_label.is_empty() {
        "Local comparison".into()
    } else {
        format!("{} ↔ {}", ctx.left_label, ctx.right_label)
    };
    let mut root = Resource::new(
        "diffmerge.local-comparison",
        "diffmerge.local-comparison",
        title,
    );
    let mut fingerprint_occurrences = std::collections::HashMap::<String, usize>::new();
    for (index, block) in ctx
        .model
        .blocks
        .iter()
        .take(CONTEXTUAL_CHILD_LIMIT)
        .enumerate()
    {
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
        let Some(fingerprint) = diff_block_fingerprint(&ctx.model, index) else {
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
            .with_action(operation(
                "show",
                "Show change",
                OperationKind::View,
                action,
            )),
        );
    }
    root
}

/// Bounded, canonical children for navigable two-way change blocks. Standalone
/// surfaces attach View/Effect workspace ops on the same root for convenience;
/// hosted surfaces publish only [`local_comparison_resource`] as a live extra.
pub fn provide_diff_resources(ctx: &DiffResourceCtx) -> Vec<Resource> {
    let mut root = local_comparison_resource(ctx);
    for spec in root_local_operations() {
        root.actions.push(spec);
    }
    vec![root]
}

/// Same non-fallible-in-practice contract as [`encode`]: `provide_resources`
/// returns a plain `Vec<Resource>` (the [`tilo_actions::ResourceProvider`]
/// trait shape), so there is no `Result` to propagate an encode failure into.
#[allow(
    clippy::expect_used,
    reason = "DiffmergeAction's fields are always JSON-serializable; see encode's doc comment"
)]
fn immediate_choice(
    model: &MergeModel,
    hunk: usize,
    choice: HunkChoice,
    id: &str,
    label: &str,
) -> ActionSpec {
    let action = resolve_hunk_action(hunk, choice, compose_choice(model, hunk, choice));
    ActionSpec::immediate(id, label, DIFFMERGE_ACTION_TYPE, &action)
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

/// Content-derived identity for an externally-derived merge hunk. The model
/// can be recomputed or reordered without making an old address point at a
/// different conflict. The occurrence number only distinguishes byte-equal
/// hunks, which are semantically interchangeable until one changes.
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

/// Build the resource tree via [`tilo_resource::publish`]. Standalone /
/// `--shots` overlay the live merge model; the framework host reads the same
/// shape from the shared projection in the document.
pub fn provide_resources(ctx: &MergeResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let mut state = MergeState::from_doc(doc);
    state.title.clone_from(&ctx.title);
    state.model = ctx.model.clone();
    crate::doc::apply_resolutions(&mut state.model, doc);
    vec![ui::tilo_resource::publish::<crate::resource::Diffmerge>(
        "diffmerge.workspace",
        &state.title,
        &state,
    )]
}

/// The merged buffer stores each line newline-terminated; a prompted edit that
/// omits the trailing newline gets one so it stays consistent.
fn ensure_trailing_newline(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::DiffMergeApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        match self.merge_resource_ctx(doc) {
            Some(ctx) => {
                let mut resources = provide_resources(&ctx, doc);
                if let Some(diff) = self.diff_resource_ctx() {
                    resources.push(local_comparison_resource(&diff));
                }
                resources
            }
            None => self.diff_resource_ctx().map_or_else(
                || {
                    let state = MergeState::from_doc(doc);
                    vec![ui::tilo_resource::publish::<crate::resource::Diffmerge>(
                        "diffmerge.workspace",
                        "Diff & Merge",
                        &state,
                    )]
                },
                |ctx| {
                    let state = MergeState::from_doc(doc);
                    let mut root = ui::tilo_resource::publish::<crate::resource::Diffmerge>(
                        "diffmerge.workspace",
                        "Diff & Merge",
                        &state,
                    );
                    root = root.with_child(local_comparison_resource(&ctx));
                    vec![root]
                },
            ),
        }
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        self.publish_resources(doc)
            .into_iter()
            .find_map(|root| find_resource(&root, id))
    }
}

fn find_resource(root: &Resource, id: &str) -> Option<Resource> {
    if root.id.as_str() == id {
        return Some(root.clone());
    }
    for child in &root.children {
        if let Some(found) = find_resource(child, id) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples;
    use tilo_actions::{ActionInput, LocalTransport};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// Operations outside the merge tree: open-files binds both paths as a
    /// cancellable job, diff blocks publish bounded view actions, and a block's
    /// fingerprint changes with its rows. Fresh model per phase.
    #[test]
    fn workspace_and_diff_block_operations_are_bound_and_content_addressed() {
        open_files_strictly_binds_both_direct_paths_as_a_job();
        local_diff_blocks_publish_bounded_canonical_view_actions();
        diff_block_fingerprint_changes_when_the_addressed_rows_change();
    }

    fn open_files_strictly_binds_both_direct_paths_as_a_job() {
        let root = workspace_operations();
        let spec = root.actions.iter().find(|a| a.id == "open-files").unwrap();
        assert_eq!(
            spec.params().iter().map(|p| &*p.name).collect::<Vec<_>>(),
            vec!["left", "right"]
        );
        assert!(matches!(
            spec.contract.execution,
            ExecutionMode::Background { cancellable: true }
        ));
        assert_eq!(spec.contract.result, ResultSpec::Job);
        let args = serde_json::json!({"left":"/tmp/a", "right":"/tmp/b"})
            .as_object()
            .unwrap()
            .clone();
        let encoded = spec.build_json(&args).unwrap();
        assert_eq!(
            decode_operation(DIFFMERGE_OPERATION_TYPE, &encoded.payload)
                .unwrap()
                .unwrap(),
            DiffmergeOperation::OpenFiles {
                left: Some("/tmp/a".into()),
                right: Some("/tmp/b".into())
            }
        );
    }

    fn local_diff_blocks_publish_bounded_canonical_view_actions() {
        let model = DiffModel::compute("a\nb\nc\n", "a\nB\nc\nnew\n");
        let resources = provide_diff_resources(&DiffResourceCtx {
            left_label: "before.rs".into(),
            right_label: "after.rs".into(),
            model: model.clone(),
        });
        let root = &resources[0];
        assert_eq!(root.kind, "diffmerge.local-comparison");
        assert_eq!(root.children.len(), model.blocks.len());
        for (index, child) in root.children.iter().enumerate() {
            assert!(child.id.0.starts_with("diffmerge.diff-block:"));
            let show = child
                .actions
                .iter()
                .find(|action| action.id == "show")
                .unwrap();
            assert_eq!(show.kind, OperationKind::View);
            let ActionInput::Immediate(encoded) = &show.input else {
                panic!("show is immediate");
            };
            assert_eq!(
                decode_operation(DIFFMERGE_OPERATION_TYPE, &encoded.payload)
                    .unwrap()
                    .unwrap(),
                DiffmergeOperation::ShowDiffBlock {
                    block: u64::try_from(index).unwrap(),
                    fingerprint: diff_block_fingerprint(&model, index).unwrap(),
                }
            );
            assert!(
                child
                    .id
                    .0
                    .contains(&diff_block_fingerprint(&model, index).unwrap())
            );
        }
    }

    fn diff_block_fingerprint_changes_when_the_addressed_rows_change() {
        let first = DiffModel::compute("unchanged\nold\n", "unchanged\nnew\n");
        let second = DiffModel::compute("unchanged\nold\n", "unchanged\nnewer\n");
        assert_ne!(
            diff_block_fingerprint(&first, 0),
            diff_block_fingerprint(&second, 0),
            "a content address must not survive a changed hunk at the same index"
        );
    }

    /// A merge model with exactly one real conflict (ours vs theirs on the same
    /// base line), plus its title/ctx.
    fn conflict_ctx() -> (MergeResourceCtx, usize) {
        let model = MergeModel::compute("a\nb\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
        let hunk = model
            .hunks
            .iter()
            .position(|h| h.resolution.is_conflict())
            .expect("one conflict");
        (
            MergeResourceCtx {
                title: "Merge".into(),
                model,
            },
            hunk,
        )
    }

    /// The provisional merged output of the conflict model, as the app writes
    /// it when a merge opens.
    fn seeded_merge() -> AutoCommit {
        let mut doc = AutoCommit::new();
        MergeDoc::set_merged(&mut doc, &conflict_ctx().0.model.merged_text());
        doc.commit();
        doc
    }

    fn take(choice: HunkChoice) -> EncodedAction {
        let (ctx, hunk) = conflict_ctx();
        encode(&resolve_hunk_action(
            hunk,
            choice,
            compose_choice(&ctx.model, hunk, choice),
        ))
    }

    ui::action_contract_tests! {
        seed: seeded_merge(),
        registry: registry(),
        resources: |doc| provide_resources(&conflict_ctx().0, doc),
        palette: ["Take theirs", "Replace merged output"],
        replay: [
            take_both => |_| take(HunkChoice::Both),
            set_merged => |_| encode(&set_merged_action("a\nmerged\nc\n")),
        ],
        undo: [
            take_theirs => |_| take(HunkChoice::Theirs),
            set_merged => |_| encode(&set_merged_action("a\nmerged\nc\n")),
        ],
        encodes: [
            take_theirs => ("diffmerge.conflict", "take-theirs", |_, _| take(HunkChoice::Theirs)),
            custom_resolve => ("diffmerge.conflict", "resolve", arg: "CUSTOM", |_, _| {
                let (ctx, hunk) = conflict_ctx();
                encode(&set_merged_action(&compose_custom(&ctx.model, hunk, "CUSTOM")))
            }),
            edit_merged => ("diffmerge.workspace", "edit-merged", arg: "a\nmerged\nc", |_, _| {
                encode(&set_merged_action("a\nmerged\nc\n"))
            }),
        ],
    }

    /// The merge resource tree (Ctrl+K) and its conflict actions: the tree shape,
    /// content-based conflict identity, take-theirs dispatch, an out-of-range
    /// hunk, and the real sample triple. Every phase builds its own model/doc.
    #[test]
    fn merge_tree_publishes_content_addressed_conflicts_whose_actions_resolve() {
        ctrl_k_publishes_the_merge_tree();
        conflict_identity_depends_on_content_not_hunk_position();
        take_theirs_action_mutates_merged_and_records_choice();
        out_of_range_hunk_is_harmless();
        sample_triple_exposes_a_conflict_resource();
    }

    /// Ctrl+K on a merge yields the tree: the workspace root (with a merged-output
    /// edit) → one conflict resource exporting take-ours/theirs/both + resolve.
    fn ctrl_k_publishes_the_merge_tree() {
        let (ctx, _hunk) = conflict_ctx();
        let resources = provide_resources(&ctx, &AutoCommit::new());
        assert_eq!(resources.len(), 1, "one workspace root");
        let merge = &resources[0];
        assert_eq!(merge.kind, "diffmerge.workspace");
        let root_actions: Vec<&str> = merge.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(root_actions.contains(&"edit-merged"), "{root_actions:?}");
        assert!(root_actions.contains(&"show-diff"), "{root_actions:?}");

        let conflict = merge
            .children
            .iter()
            .find(|r| r.kind == "diffmerge.conflict")
            .expect("a conflict resource");
        let ids: Vec<&str> = conflict.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"take-ours"), "{ids:?}");
        assert!(ids.contains(&"take-theirs"), "{ids:?}");
        assert!(ids.contains(&"take-both"), "{ids:?}");
        assert!(ids.contains(&"resolve"), "{ids:?}");
    }

    fn conflict_identity_depends_on_content_not_hunk_position() {
        let (ctx, hunk) = conflict_ctx();
        let conflict = &ctx.model.hunks[hunk];
        let (rebuilt, rebuilt_hunk) = conflict_ctx();
        assert_eq!(
            hunk_resource_id(conflict, 0),
            hunk_resource_id(&rebuilt.model.hunks[rebuilt_hunk], 0),
            "rebuilding the model preserves the externally visible address"
        );
        assert_ne!(
            hunk_resource_id(conflict, 0),
            hunk_resource_id(conflict, 1),
            "byte-identical conflicts still receive distinct addresses"
        );
    }

    /// Taking theirs through the action mutates the merged buffer (the conflict
    /// output flips from the provisional "ours" to "theirs") and records the
    /// choice in the resolutions map.
    fn take_theirs_action_mutates_merged_and_records_choice() {
        let (ctx, hunk) = conflict_ctx();
        let mut doc = AutoCommit::new();
        // Seed the provisional merged output (conflict shows ours).
        MergeDoc::set_merged(&mut doc, &ctx.model.merged_text());
        assert!(MergeDoc::merged_text(&doc).contains("OURS"));

        let action = resolve_hunk_action(
            hunk,
            HunkChoice::Theirs,
            compose_choice(&ctx.model, hunk, HunkChoice::Theirs),
        );
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let merged = MergeDoc::merged_text(&doc);
        assert!(merged.contains("THEIRS"), "{merged:?}");
        assert!(!merged.contains("OURS"), "{merged:?}");
        let stored = MergeDoc::resolutions(&doc);
        assert_eq!(
            stored,
            vec![(hunk, "theirs".to_string())],
            "choice recorded"
        );
    }

    /// A malformed / out-of-range hunk index doesn't wedge: the resolution is
    /// still recorded under that key and the carried merged text is written,
    /// with no panic (the app never authors an out-of-range index).
    fn out_of_range_hunk_is_harmless() {
        let mut doc = AutoCommit::new();
        MergeDoc::set_merged(&mut doc, "a\nb\n");
        let action = resolve_hunk_action(9999, HunkChoice::Ours, "x\ny\n".into());
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        assert_eq!(MergeDoc::merged_text(&doc), "x\ny\n");
    }

    /// Sanity: on the real sample triple the resource tree exposes a conflict
    /// hunk, so `--shots`/demo has something to resolve through Ctrl+K.
    fn sample_triple_exposes_a_conflict_resource() {
        let (base, ours, theirs) = samples::merge_triple();
        let model = MergeModel::compute(&base.text, &ours.text, &theirs.text);
        let ctx = MergeResourceCtx {
            title: "Merge".into(),
            model,
        };
        let resources = provide_resources(&ctx, &AutoCommit::new());
        assert!(
            resources[0]
                .children
                .iter()
                .any(|r| r.kind == "diffmerge.conflict"),
            "the sample merge exposes at least one hunk resource"
        );
    }
}
