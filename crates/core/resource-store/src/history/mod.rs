//! The workspace's one history: every document's steps (undo, redo, restore and checkpoints),
//! the revert they are taken back by, and who wrote them, for every app's records and whichever
//! language declared them; and each resource's versions across the documents it is in
//! ([`Version`]).
//!
//! The toolkit's `ui::history_source` builds the History views on this and keeps its paths; a
//! tilolang host, the shell and a process without the toolkit read and move the same history
//! through the same code.

pub mod authors;
mod built_on;
pub mod feed;
mod patches;
pub mod revert;
mod sequence_replay;
pub mod steps;
pub mod typing;
mod undo_log;
pub mod verbs;
mod versions;
pub mod words;

pub use patches::{
    write_patches, write_revert_patches, write_revert_patches_where, write_selected_patches,
};
#[doc(hidden)]
pub use undo_log::{HistoryMoment, OwnedActionAttribution, OwnedActionReceipt, same_frontier};
pub use undo_log::{InstanceId, UndoEntry, UndoLog};
pub use versions::{Nested, Version, VersionKind, touched_by};
pub(crate) use versions::{Touches, stamped};

use automerge::{PatchAction, Prop};
use collab_types::history::HistoryEntryId;

/// The hash is the document's own content address, so two peers
/// independently building a timeline over the same history agree on the
/// ids — what lets one peer's `[Revert]` mean anything to another.
pub fn entry_id_from_hash(change_hash_hex: &str) -> HistoryEntryId {
    let mut bytes = [0u8; 16];
    let src = change_hash_hex.as_bytes();
    for (i, slot) in bytes.iter_mut().enumerate() {
        // `i` walks a fixed 16-byte array, so `idx` never approaches
        // `usize::MAX`; `saturating_mul`/`saturating_add` are a no-op here
        // and only exist to satisfy the arithmetic-side-effects lint.
        let idx = i.saturating_mul(2);
        let (hi, lo) = (src.get(idx), src.get(idx.saturating_add(1)));
        let hi = hi.and_then(|c| (*c as char).to_digit(16)).unwrap_or(0);
        let lo = lo.and_then(|c| (*c as char).to_digit(16)).unwrap_or(0);
        // `to_digit(16)` only ever returns `0..=15`, so `combined` is always
        // `0..=255` and the `try_from` below can never actually fail.
        let combined = hi.saturating_mul(16).saturating_add(lo);
        *slot = u8::try_from(combined).unwrap_or(u8::MAX);
    }
    HistoryEntryId(bytes)
}

// History's own metadata about other changes, kept in the document beside
// them: the toolkit's `groups`, `runs` and `descriptions` read and write it.

/// Namespaced so it cannot collide with an app's own schema: `tilo-doc` uses
/// `content` / `comments` / `props`, and the shell's workspace document uses
/// `workspaces` / `active`.
pub const GROUPS_KEY: &str = "tilo:group:";

/// The longest name of a run of changes: a label, not a paragraph.
pub const MAX_RUN_NAME: usize = 120;

/// The id history names a change by in a run's name: the first half of its hash, in hex.
#[must_use]
pub fn run_id(change: &automerge::ChangeHash) -> String {
    change.0[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Name the run of changes from `start` to `end` (open while `end` is `None`), each as
/// [`run_id`] gives it, `label`, as `by` wrote it, replacing the name `replaces` holds; an empty
/// label only removes that name. Two people naming one run write one key, the run itself, which
/// settles the same way on every peer. Leaves the transaction open. The key of the name it
/// wrote, if it wrote one.
pub fn name_run(
    doc: &mut automerge::AutoCommit,
    (start, end): (&str, Option<&str>),
    label: &str,
    by: &str,
    replaces: Option<&str>,
) -> Option<String> {
    use automerge::transaction::Transactable as _;
    let label: String = label.trim().chars().take(MAX_RUN_NAME).collect();
    let key = (!label.is_empty()).then(|| format!("{GROUPS_KEY}{start}"));
    if let Some(replaced) = replaces
        .filter(|replaced| replaced.starts_with(GROUPS_KEY) && Some(*replaced) != key.as_deref())
    {
        let _ = doc.delete(automerge::ROOT, replaced);
    }
    let key = key?;
    // The whole record is written again, so no peer reads a new label against an old run.
    let record = doc
        .put_object(automerge::ROOT, &key, automerge::ObjType::Map)
        .ok()?;
    let _ = doc.put(&record, "label", label);
    if let Some(end) = end {
        let _ = doc.put(&record, "end", end);
    }
    let _ = doc.put(&record, "by", by);
    Some(key)
}

/// True when every patch in `patches` lands inside the named-range map — used
/// by the producer to keep naming out of the row list. An empty patch set is
/// **not** a marker: a change that did nothing visible is still a change, and
/// hiding it would be incorrect behaviour.
pub fn is_group_marker(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|p| match p.path.first() {
            Some((_, Prop::Map(k))) => k.starts_with(GROUPS_KEY),
            Some(_) => false,
            // A patch at the root: the creation (or removal) of a record itself.
            None => match &p.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key, .. } => {
                    key.starts_with(GROUPS_KEY)
                }
                _ => false,
            },
        })
}

/// Prefix of the root keys holding this document's cross-application
/// invocations. Namespaced like [`GROUPS_KEY`] so it cannot collide with an
/// app's own schema.
pub const INVOCATIONS_KEY: &str = "tilo:invocation:";

/// True when every patch lands inside an invocation's metadata — the change did nothing
/// but record bookkeeping, already on screen as a band, so a "1 field" row
/// under it would be noise. An empty patch set is not a marker.
pub fn is_invocation_marker(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|p| match p.path.first() {
            Some((_, Prop::Map(k))) => k.starts_with(INVOCATIONS_KEY),
            Some(_) => false,
            None => match &p.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    key.starts_with(INVOCATIONS_KEY)
                }
                _ => false,
            },
        })
}

/// Namespaced beside the names of groups and invocations, apart from any
/// app's own schema.
pub const DESCRIPTIONS_KEY: &str = "tilo:describe:";

/// Whether a change only wrote descriptions.
pub fn is_description_marker(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|patch| match patch.path.first() {
            Some((_, Prop::Map(key))) => key.starts_with(DESCRIPTIONS_KEY),
            Some(_) => false,
            None => match &patch.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    key.starts_with(DESCRIPTIONS_KEY)
                }
                _ => false,
            },
        })
}

/// Whether a change only wrote history's own metadata about other changes:
/// names of groups ([`GROUPS_KEY`]), cross-app invocations ([`INVOCATIONS_KEY`]) and
/// descriptions ([`DESCRIPTIONS_KEY`]). Such a change is neither a row nor an undo step.
pub fn is_history_metadata(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|patch| {
            let one = std::slice::from_ref(patch);
            is_group_marker(one) || is_invocation_marker(one) || is_description_marker(one)
        })
}

/// Words for the tests here: an actor by the handle it declares, a step by
/// what its entry says it did.
#[cfg(test)]
pub(crate) struct TestWords;

#[cfg(test)]
impl authors::ResolveAuthor for TestWords {
    fn resolve(&self, actor_hex: &str) -> authors::HistoryAuthor {
        let name = authors::actor_handle(actor_hex)
            .unwrap_or_else(|| authors::UNKNOWN_AUTHOR_NAME.to_owned());
        authors::author_coloured(
            name,
            authors::short_id(actor_hex),
            collab_types::presence::user_color(actor_hex),
        )
    }
}

#[cfg(test)]
impl words::HistoryWords for TestWords {
    fn sentence(&self, entry: &tilo_actions::ActionContext) -> String {
        format!("{:?}", entry.what)
    }
}
