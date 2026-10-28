//! A document's history below the toolkit: its steps (undo, redo, restore
//! and checkpoints written into the document itself), the revert they are
//! taken back by, and who wrote them.
//!
//! The toolkit's `ui::history_source` builds the History views on this and
//! keeps its paths; a process without the toolkit reads and moves the same
//! history through the same code.

pub mod authors;
pub mod revert;
pub mod steps;
pub mod typing;
pub mod words;

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
/// `content` / `comments` / `props`, and the shell's room document uses
/// `workspaces` / `active`.
pub const GROUPS_KEY: &str = "tilo:group:";

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

/// Prefix of the root keys holding this document's cross-application runs.
/// Namespaced like [`GROUPS_KEY`] so
/// it cannot collide with an app's own schema.
pub const RUNS_KEY: &str = "tilo:run:";

/// True when every patch lands inside a run record — the change did nothing
/// but record bookkeeping, already on screen as a band, so a "1 field" row
/// under it would be noise. An empty patch set is not a marker.
pub fn is_run_marker(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|p| match p.path.first() {
            Some((_, Prop::Map(k))) => k.starts_with(RUNS_KEY),
            Some(_) => false,
            None => match &p.action {
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key } => {
                    key.starts_with(RUNS_KEY)
                }
                _ => false,
            },
        })
}

/// Namespaced beside the names of runs, apart from any app's own schema.
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
/// names of runs ([`GROUPS_KEY`]), cross-app runs ([`RUNS_KEY`]) and
/// descriptions ([`DESCRIPTIONS_KEY`]). Such a change is neither a row nor an undo step.
pub fn is_history_metadata(patches: &[automerge::Patch]) -> bool {
    !patches.is_empty()
        && patches.iter().all(|patch| {
            let one = std::slice::from_ref(patch);
            is_group_marker(one) || is_run_marker(one) || is_description_marker(one)
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
        authors::HistoryAuthor::new(
            name,
            authors::stable_hue(actor_hex),
            authors::short_id(actor_hex),
        )
    }
}

#[cfg(test)]
impl words::HistoryWords for TestWords {
    fn sentence(&self, entry: &tilo_actions::ActionContext) -> String {
        format!("{:?}", entry.what)
    }
}
