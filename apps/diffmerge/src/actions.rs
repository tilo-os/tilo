//! Diff & Merge's effects and the pure helpers its verbs and places use.
//!
//! Every change to the session is a verb or a field edit of
//! [`crate::doc::Session`] ([`crate::resource`]). Reading files from disk or a
//! file chooser is an effect ([`DiffmergeEffect`]) whose result opens a
//! comparison through a verb. Scrolling, the active hunk and the mode shown
//! are view state; the effects that move them (also [`DiffmergeEffect`])
//! change nothing in the record.

use ui::prelude::Effect;
// The effect derive names its runtime crate by this path.
use ui::tilo_resource;

use crate::doc::HunkChoice;
use crate::model::{BlockKind, ChangeBlock, DiffModel, MergeHunk, MergeModel, Resolution};

/// Keep global search/contextual expansion bounded even for enormous diffs.
pub const CONTEXTUAL_CHILD_LIMIT: usize = 64;

/// Bringing inputs in from outside the document, and moving around what is
/// open (no document delta).
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "diffmerge.effects")]
pub enum DiffmergeEffect {
    /// Open files
    #[effect(attended, job, cancellable)]
    OpenFiles {
        #[param("left file path (omit both paths to choose the files)")]
        left: Option<String>,
        #[param("right file path")]
        right: Option<String>,
    },
    /// Open one file against nothing
    ///
    /// A file only one folder of a comparison has: shown against an empty
    /// side, which reads as the whole file being added or removed.
    #[effect(attended, job, cancellable)]
    OpenOne {
        #[param("the file to show")]
        path: String,
        #[param("which side it takes (left or right)")]
        side: String,
    },
    /// Save merged result
    ///
    /// Writes the merged result where the person last saved it, asking for a
    /// place the first time; `path` names one instead. A merge that cannot
    /// be handed back is a merge that has not been done, so this is the verb
    /// the tool exists for.
    #[effect(attended, job, cancellable)]
    SaveMerged {
        #[param("where to write the merged result (omit to use the last place, or choose one)")]
        path: Option<String>,
    },
    /// Save merged result as…
    ///
    /// Always asks for a place, even when the merged result was saved before.
    #[effect(attended, job, cancellable)]
    SaveMergedAs,
    /// Compare folders
    ///
    /// Lists the files that differ between two folders; each one opens as an
    /// ordinary comparison. With no paths, both folders are chosen through
    /// the host's chooser.
    #[effect(attended, job, cancellable)]
    CompareFolders {
        #[param("left folder path (omit both paths to choose the folders)")]
        left: Option<String>,
        #[param("right folder path")]
        right: Option<String>,
    },
    /// Load sample
    #[effect(own_device)]
    LoadSample,
    /// Show two-way diff.
    #[effect(view, idempotent)]
    ShowDiff,
    /// Show three-way merge.
    #[effect(view, idempotent)]
    ShowMerge,
    /// Previous change.
    #[effect(view)]
    PreviousChange,
    /// Next change.
    #[effect(view)]
    NextChange,
    /// Show change.
    #[effect(view, row)]
    Show {
        #[param("The change's index")]
        block: u64,
        /// Snapshot fingerprint of the exact aligned rows this address named.
        /// The index is only a fast lookup hint; this prevents a stale palette
        /// result from selecting a newly recomputed, different block.
        #[param("The change's fingerprint")]
        fingerprint: String,
    },
}

/// Compose the merged output with `hunk`'s contribution forced to `choice`,
/// every other hunk keeping its current resolution.
pub fn compose_choice(model: &MergeModel, hunk: usize, choice: HunkChoice) -> String {
    model.merged_text_with(hunk, choice.to_resolution(), &[])
}

/// Compose the merged output with `hunk` replaced by the user's typed `text`,
/// every other hunk keeping its current resolution. Backs the per-hunk
/// "resolve with custom text" affordance.
pub fn compose_custom(model: &MergeModel, hunk: usize, text: &str) -> String {
    let lines: Vec<String> = text.lines().map(str::to_owned).collect();
    model.merged_text_with(hunk, Resolution::Written, &lines)
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

pub(crate) fn diff_block_resource_id(
    block: &ChangeBlock,
    fingerprint: &str,
    occurrence: usize,
) -> String {
    let kind = match block.kind {
        BlockKind::Added => "added",
        BlockKind::Removed => "removed",
        BlockKind::Modified => "modified",
    };
    format!("diffmerge.diff-block:{kind}:{fingerprint}:{occurrence}")
}

/// A short one-line preview of a hunk for the palette label.
pub(crate) fn hunk_preview(h: &MergeHunk) -> String {
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
pub(crate) fn hunk_resource_id(h: &MergeHunk, occurrence: usize) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A change block's address follows its content, never only its index.
    #[test]
    fn a_change_blocks_fingerprint_follows_its_content() {
        let first = DiffModel::compute("unchanged\nold\n", "unchanged\nnew\n");
        let second = DiffModel::compute("unchanged\nold\n", "unchanged\nnewer\n");
        assert_ne!(
            diff_block_fingerprint(&first, 0),
            diff_block_fingerprint(&second, 0),
            "a content address must not survive a changed hunk at the same index"
        );
    }

    /// A change's id is derived from its content; byte-equal changes are told
    /// apart by their occurrence.
    #[test]
    fn a_changes_id_is_its_content_and_occurrence() {
        let model = MergeModel::compute("a\nb\nc\n", "a\nOURS\nc\n", "a\nTHEIRS\nc\n");
        let h = model
            .hunks
            .iter()
            .find(|h| h.resolution.is_conflict())
            .unwrap();
        assert_eq!(hunk_resource_id(h, 0), hunk_resource_id(&h.clone(), 0));
        assert_ne!(
            hunk_resource_id(h, 0),
            hunk_resource_id(h, 1),
            "byte-identical conflicts still receive distinct addresses"
        );
        let both = compose_choice(&model, 1, HunkChoice::Both);
        assert_eq!(both, "a\nOURS\nTHEIRS\nc\n");
        assert_eq!(compose_custom(&model, 1, "mine"), "a\nmine\nc\n");
    }
}
