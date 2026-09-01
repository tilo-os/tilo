//! Diff's exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! The document-mutating surface of a merge is the *merge result*: the resolved
//! output (an Automerge `Text`) plus the per-hunk resolution choices (ours/
//! theirs/both), converging across peers as [`DiffmergeAction`]s. The exact
//! same [`EncodedAction`] is what the on-panel chips and Ctrl+K both dispatch —
//! one source of truth, one replay path. The resource tree mirrors this: the
//! merge owns its hunks, each exporting take-ours/theirs/both plus a prompted
//! custom-text resolve; the root exports a "replace merged output" edit.
//!
//! Everything else — scroll, focus, the active hunk, sync-scroll, loading the
//! input blobs and recomputing the diff — is plain app code, never an action.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus,
    OperationKind, Resource, ResourceProvider, RetryPolicy,
};

use crate::doc::MergeDoc;
use crate::model::{MergeHunk, MergeModel, Resolution};

/// Registry `type_id` for every Diff action.
pub const DIFFMERGE_ACTION_TYPE: &str = "diffmerge.action";
pub const DIFFMERGE_OPERATION_TYPE: &str = "diffmerge.operation";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffmergeOperation {
    ShowDiff,
    ShowMerge,
    PreviousChange,
    NextChange,
    OpenFiles,
    LoadSample,
}

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

fn operation(id: &str, label: &str, kind: OperationKind, value: DiffmergeOperation) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, DIFFMERGE_OPERATION_TYPE, &value)
        .expect("DiffmergeOperation is always serializable");
    match value {
        DiffmergeOperation::ShowDiff | DiffmergeOperation::ShowMerge => {
            spec.retry_policy(RetryPolicy::Idempotent)
        }
        DiffmergeOperation::PreviousChange | DiffmergeOperation::NextChange => spec,
        DiffmergeOperation::OpenFiles => spec,
        DiffmergeOperation::LoadSample => spec.destructive(),
    }
}

fn workspace_operations() -> Resource {
    let mut root = Resource::new("diffmerge.workspace", "diffmerge.workspace", "Diff & Merge");
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
            "open-files",
            "Open files",
            OperationKind::Effect,
            DiffmergeOperation::OpenFiles,
        ),
        (
            "load-sample",
            "Load sample",
            OperationKind::Effect,
            DiffmergeOperation::LoadSample,
        ),
    ] {
        root = root.with_action(operation(id, label, kind, value));
    }
    root
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
/// [`crate::actions::immediate_choice`] for the same contract on the palette
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

/// Same non-fallible-in-practice contract as [`encode`]: `provide_resources`
/// returns a plain `Vec<Resource>` (the [`qos_actions::ResourceProvider`]
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
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in h
        .base
        .iter()
        .chain(&h.ours)
        .chain(&h.theirs)
        .flat_map(|line| line.bytes().chain([0xff]))
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("diffmerge.conflict:{hash:016x}:{occurrence}")
}

/// Build the resource tree Diff currently has open: the merge, its change/
/// conflict hunks (each exporting the take-a-side and custom-resolve actions),
/// and a merged-output edit at the root — every document-mutating operation
/// exported as an action.
pub fn provide_resources(ctx: &MergeResourceCtx, _doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new("diffmerge.merge", "diffmerge.merge", ctx.title.clone());

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
            "open-files",
            "Open files",
            OperationKind::Effect,
            DiffmergeOperation::OpenFiles,
        ),
        (
            "load-sample",
            "Load sample",
            OperationKind::Effect,
            DiffmergeOperation::LoadSample,
        ),
    ] {
        root = root.with_action(operation(id, label, kind, value));
    }

    // Merge-level: replace the whole shared merged buffer with typed text — the
    // merged-output edit. A future in-place editor dispatches the same action.
    root = root.with_action(ActionSpec::prompted(
        "edit-merged",
        "Replace merged output",
        "Merged text",
        "the merged result…",
        ExecutionLocus::ClientLocal,
        move |text: &str| encode(&set_merged_action(&ensure_trailing_newline(text))),
    ));

    let mut ordinal = 0usize;
    let mut identities = std::collections::BTreeMap::<String, usize>::new();
    for (i, h) in ctx.model.hunks.iter().enumerate() {
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
            &ctx.model,
            i,
            HunkChoice::Ours,
            "take-ours",
            "Take ours",
        ));
        hunk_res = hunk_res.with_action(immediate_choice(
            &ctx.model,
            i,
            HunkChoice::Theirs,
            "take-theirs",
            "Take theirs",
        ));
        hunk_res = hunk_res.with_action(immediate_choice(
            &ctx.model,
            i,
            HunkChoice::Both,
            "take-both",
            "Take both",
        ));

        // Resolve with custom text — a prompted merged-output edit scoped to this
        // hunk: the typed lines replace the hunk in the recomposed output.
        let model = ctx.model.clone();
        hunk_res = hunk_res.with_action(ActionSpec::prompted(
            "resolve",
            "Resolve with custom text",
            "Replacement",
            "type the resolved line(s)…",
            ExecutionLocus::ClientLocal,
            move |text: &str| encode(&set_merged_action(&compose_custom(&model, i, text))),
        ));

        root = root.with_child(hunk_res);
    }

    vec![root]
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

impl ResourceProvider for crate::app::DiffMergeApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        match self.merge_resource_ctx() {
            Some(ctx) => provide_resources(&ctx, doc),
            None => vec![workspace_operations()],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples;
    use qos_actions::{ActionInput, LocalTransport};

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
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

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
    }

    /// Ctrl+K on a merge yields the tree: the merge root (with a merged-output
    /// edit) → one conflict resource exporting take-ours/theirs/both + resolve.
    #[test]
    fn ctrl_k_publishes_the_merge_tree() {
        let (ctx, _hunk) = conflict_ctx();
        let resources = provide_resources(&ctx, &AutoCommit::new());
        assert_eq!(resources.len(), 1, "one merge root");
        let merge = &resources[0];
        assert_eq!(merge.kind, "diffmerge.merge");
        let root_actions: Vec<&str> = merge.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(root_actions.contains(&"edit-merged"), "{root_actions:?}");

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

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        palette.set_query("Take theirs");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Take theirs")),
            "{labels:?}"
        );
        palette.set_query("Replace merged output");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Replace merged output")),
            "{labels:?}"
        );
    }

    #[test]
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
    #[test]
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
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

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

    /// The collaboration guarantee: replaying the same encoded action on a
    /// second replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let (ctx, hunk) = conflict_ctx();
        let mut a = AutoCommit::new();
        MergeDoc::set_merged(&mut a, &ctx.model.merged_text());
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = resolve_hunk_action(
            hunk,
            HunkChoice::Both,
            compose_choice(&ctx.model, hunk, HunkChoice::Both),
        );
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(MergeDoc::merged_text(&a), MergeDoc::merged_text(&b));
    }

    /// Single source of truth: the palette's immediate take-theirs spec encodes
    /// byte-identically to the action a chip builds via the shared builder +
    /// composer. And the prompted custom-resolve spec matches the `SetMerged` the
    /// builder produces for the same text.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (ctx, hunk) = conflict_ctx();
        let resources = provide_resources(&ctx, &AutoCommit::new());
        let conflict = &resources[0].children[0];

        let spec = conflict
            .actions
            .iter()
            .find(|a| a.id == "take-theirs")
            .unwrap();
        let from_spec = immediate_encode(spec);
        let from_builder = encode(&resolve_hunk_action(
            hunk,
            HunkChoice::Theirs,
            compose_choice(&ctx.model, hunk, HunkChoice::Theirs),
        ));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth"
        );

        let resolve = conflict.actions.iter().find(|a| a.id == "resolve").unwrap();
        let from_spec = prompted_encode(resolve, "CUSTOM");
        let from_builder = encode(&set_merged_action(&compose_custom(
            &ctx.model, hunk, "CUSTOM",
        )));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "custom resolve is one source of truth"
        );
    }

    /// A malformed / out-of-range hunk index doesn't wedge: the resolution is
    /// still recorded under that key and the carried merged text is written,
    /// with no panic (the app never authors an out-of-range index).
    #[test]
    fn out_of_range_hunk_is_harmless() {
        let mut doc = AutoCommit::new();
        MergeDoc::set_merged(&mut doc, "a\nb\n");
        let action = resolve_hunk_action(9999, HunkChoice::Ours, "x\ny\n".into());
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        assert_eq!(MergeDoc::merged_text(&doc), "x\ny\n");
    }

    /// Sanity: on the real sample triple the resource tree exposes a conflict
    /// hunk, so `--shots`/demo has something to resolve through Ctrl+K.
    #[test]
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
