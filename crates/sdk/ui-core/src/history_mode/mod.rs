//! Window-independent projection of a managed record's historical states. The controller reads a
//! rolled-back clone of the live Automerge document.

mod assets;

/// Moves on whenever something reaches a History surface from outside a refresh of it: an asset or file
/// reply another thread delivers, a request to open or close it, a command one of its controls queued,
/// a width that changes its layout.
static NEWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`NEWS`] as it stands.
pub(crate) fn news() -> u64 {
    NEWS.load(std::sync::atomic::Ordering::Acquire)
}

/// Something reached a History surface ([`NEWS`]).
pub(crate) fn note_news() {
    NEWS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

pub(crate) mod api;
pub mod capability;
pub mod coverage;
pub(crate) mod feed;
pub mod folder;
pub mod preview;
pub mod restore;
pub use assets::{HistoricalAssetRequest, HistoricalAssetResponse, HistoricalAssetSource};
use tilo_sort::CompactSort as _;
pub(crate) mod owner;
pub mod registration;
pub use owner::HistoryOwner;
pub(crate) mod canvas;
pub(crate) mod frame;
pub(crate) mod frozen_root;
pub use frozen_root::{FrozenRootPreview, outline_preview};
pub(crate) mod host;
pub mod pending_controls;
pub use coverage::{HistoricalGap, HistoricalSupport, TypeHistory};
pub use preview::*;
pub use registration::*;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash};

use crate::history_source::{CompareError, HistorySource, compare_entries};
use crate::{DocumentId, HistoryEntry, HistoryEntryId};

/// Full authority namespace for one historical resource.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HistoryResource {
    pub profile_id: String,
    pub workspace_id: String,
    pub app_id: String,
    pub document_id: DocumentId,
}

impl HistoryResource {
    pub fn new(
        profile_id: impl Into<String>,
        workspace_id: impl Into<String>,
        app_id: impl Into<String>,
        document_id: DocumentId,
    ) -> Self {
        Self {
            profile_id: profile_id.into(),
            workspace_id: workspace_id.into(),
            app_id: app_id.into(),
            document_id,
        }
    }
}

/// Which exact frontier of a selected change or group is being previewed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewSide {
    Before,
    #[default]
    After,
}

/// Immutable Automerge bytes at one exact causal frontier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySnapshot {
    bytes: Arc<[u8]>,
    heads: Vec<ChangeHash>,
    side: PreviewSide,
    selection: Vec<HistoryEntryId>,
}

impl HistorySnapshot {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn heads(&self) -> &[ChangeHash] {
        &self.heads
    }

    pub fn side(&self) -> PreviewSide {
        self.side
    }

    pub fn selection(&self) -> &[HistoryEntryId] {
        &self.selection
    }
}

/// Opaque proof that a particular selected state was reviewed against a particular live frontier.
/// Validation grants no authority and writes nothing; the host must separately recheck permissions
/// before restoring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestoreReview {
    resource: HistoryResource,
    selection: Vec<HistoryEntryId>,
    selected_heads: Vec<ChangeHash>,
    reviewed_live_heads: Vec<ChangeHash>,
    generation: u64,
}

impl RestoreReview {
    pub fn resource(&self) -> &HistoryResource {
        &self.resource
    }

    pub fn selection(&self) -> &[HistoryEntryId] {
        &self.selection
    }

    pub fn selected_heads(&self) -> &[ChangeHash] {
        &self.selected_heads
    }

    pub fn reviewed_live_heads(&self) -> &[ChangeHash] {
        &self.reviewed_live_heads
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryError {
    EmptySelection,
    UnknownEntry(HistoryEntryId),
    Materialize(String),
    NoPreview,
    EffectOnlySelection,
    ResourceMismatch,
    ReviewInvalidated,
    StaleLiveHeads,
    UncommittedLiveChanges,
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySelection => f.write_str("history selection is empty"),
            Self::UnknownEntry(_) => f.write_str("history selection contains an unknown entry"),
            Self::Materialize(error) => write!(f, "could not materialize history: {error}"),
            Self::NoPreview => f.write_str("no historical preview is selected"),
            Self::EffectOnlySelection => {
                f.write_str("external effect receipts are not restorable content")
            }
            Self::ResourceMismatch => {
                f.write_str("restore review belongs to a different resource namespace")
            }
            Self::ReviewInvalidated => {
                f.write_str("restore review no longer matches the selected version")
            }
            Self::StaleLiveHeads => f.write_str("live record changed after the restore review"),
            Self::UncommittedLiveChanges => f.write_str("live record has uncommitted changes"),
        }
    }
}

impl std::error::Error for HistoryError {}

struct SelectedPreview {
    selection: Vec<HistoryEntryId>,
    before: HistorySnapshot,
    after: HistorySnapshot,
    effect_only: bool,
}

pub struct HistoryController {
    resource: HistoryResource,
    observed_live_heads: Vec<ChangeHash>,
    selected: Option<SelectedPreview>,
    side: PreviewSide,
    live_updates: bool,
    generation: u64,
    snapshots: HashMap<Vec<ChangeHash>, Arc<[u8]>>,
}

impl HistoryController {
    /// A controller over `resource` that has observed the committed frontier `live_heads`.
    pub fn new(resource: HistoryResource, live_heads: Vec<ChangeHash>) -> Self {
        Self {
            resource,
            observed_live_heads: sorted_heads(live_heads),
            selected: None,
            side: PreviewSide::After,
            live_updates: false,
            generation: 0,
            snapshots: HashMap::new(),
        }
    }

    pub fn resource(&self) -> &HistoryResource {
        &self.resource
    }

    /// Observe `live`'s committed frontier: whether the pinned preview is behind it.
    pub fn observe(&mut self, live: &AutoCommit) {
        let live_heads = sorted_heads(committed_clone(live).get_heads());
        self.live_updates = self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.after.heads != live_heads);
        self.observed_live_heads = live_heads;
    }

    /// Pin the exact before and after causal frontiers of one change or group. # Errors Returns
    /// [`HistoryError::EmptySelection`] or [`HistoryError::UnknownEntry`] for an invalid selection, and
    /// [`HistoryError::Materialize`] when Automerge cannot build its states.
    pub fn select(
        &mut self,
        live: &AutoCommit,
        entries: &[HistoryEntryId],
    ) -> Result<(), HistoryError> {
        let mut projection = committed_clone(live);
        let current_live_heads = sorted_heads(projection.get_heads());
        let comparison = compare_entries(&mut projection, entries).map_err(HistoryError::from)?;
        // What the changes were, read from the resource itself: a receipt of an outside effect is no
        // content to restore, whatever a listing says of it.
        let factual = HistorySource::new().entries(&mut projection);
        let effect_only = entries.iter().all(|id| {
            factual
                .iter()
                .find(|entry| entry.id == *id)
                .is_some_and(HistoryEntry::is_effect_receipt)
        });

        let before_heads = sorted_heads(comparison.before_heads);
        let after_heads = sorted_heads(comparison.after_heads);
        let selection = entries.to_vec();
        let before = self.snapshot_for(
            comparison.before_document,
            before_heads,
            PreviewSide::Before,
            &selection,
        );
        let after = self.snapshot_for(
            comparison.after_document,
            after_heads,
            PreviewSide::After,
            &selection,
        );
        let live_updates = after.heads != current_live_heads;

        self.generation = self.generation.wrapping_add(1);
        self.observed_live_heads = current_live_heads;
        self.selected = Some(SelectedPreview {
            selection,
            before,
            after,
            effect_only,
        });
        self.side = PreviewSide::After;
        self.live_updates = live_updates;
        Ok(())
    }

    /// Switch between the selected comparison's exact states.
    pub fn select_compare_side(&mut self, side: PreviewSide) -> Result<(), HistoryError> {
        if self.selected.is_none() {
            return Err(HistoryError::NoPreview);
        }
        self.side = side;
        Ok(())
    }

    pub fn snapshot(&self) -> Option<&HistorySnapshot> {
        self.selected.as_ref().map(|selected| match self.side {
            PreviewSide::Before => &selected.before,
            PreviewSide::After => &selected.after,
        })
    }

    /// Whether the observed committed live frontier differs from the pinned selected after-frontier.
    pub fn has_live_updates(&self) -> bool {
        self.live_updates
    }

    pub fn back_to_live(&mut self) {
        self.selected = None;
        self.side = PreviewSide::After;
        self.live_updates = false;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Bind the selected state to the latest observed committed live frontier.
    pub fn review_restore(&self) -> Result<RestoreReview, HistoryError> {
        let selected = self.selected.as_ref().ok_or(HistoryError::NoPreview)?;
        if selected.effect_only {
            return Err(HistoryError::EffectOnlySelection);
        }
        Ok(RestoreReview {
            resource: self.resource.clone(),
            selection: selected.selection.clone(),
            selected_heads: selected.after.heads.clone(),
            reviewed_live_heads: self.observed_live_heads.clone(),
            generation: self.generation,
        })
    }

    /// Validate a review token without changing the document.
    pub fn validate_restore(
        &self,
        review: &RestoreReview,
        live: &AutoCommit,
    ) -> Result<(), HistoryError> {
        if live.pending_ops() != 0 {
            return Err(HistoryError::UncommittedLiveChanges);
        }
        if review.resource != self.resource {
            return Err(HistoryError::ResourceMismatch);
        }
        let selected = self
            .selected
            .as_ref()
            .ok_or(HistoryError::ReviewInvalidated)?;
        if review.generation != self.generation
            || review.selection != selected.selection
            || review.selected_heads != selected.after.heads
        {
            return Err(HistoryError::ReviewInvalidated);
        }
        if selected.effect_only {
            return Err(HistoryError::EffectOnlySelection);
        }
        let mut projection = committed_clone(live);
        if sorted_heads(projection.get_heads()) != review.reviewed_live_heads {
            return Err(HistoryError::StaleLiveHeads);
        }
        Ok(())
    }

    fn snapshot_for(
        &mut self,
        bytes: Vec<u8>,
        heads: Vec<ChangeHash>,
        side: PreviewSide,
        selection: &[HistoryEntryId],
    ) -> HistorySnapshot {
        let bytes = self
            .snapshots
            .entry(heads.clone())
            .or_insert_with(|| Arc::from(bytes))
            .clone();
        HistorySnapshot {
            bytes,
            heads,
            side,
            selection: selection.to_vec(),
        }
    }
}

impl From<CompareError> for HistoryError {
    fn from(error: CompareError) -> Self {
        match error {
            CompareError::EmptySelection => Self::EmptySelection,
            CompareError::UnknownEntry(id) => Self::UnknownEntry(id),
            CompareError::Materialize(error) => Self::Materialize(error),
        }
    }
}

fn committed_clone(live: &AutoCommit) -> AutoCommit {
    let mut projection = live.clone();
    if projection.pending_ops() != 0 {
        projection.rollback();
    }
    projection
}

pub(crate) fn sorted_heads(mut heads: Vec<ChangeHash>) -> Vec<ChangeHash> {
    heads.compact_sort();
    heads
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod host_tests;

#[cfg(test)]
pub(crate) mod focus_tests;

#[cfg(test)]
pub(crate) mod isolation_tests;

#[cfg(test)]
pub(crate) mod integration_tests;

#[cfg(test)]
pub(crate) use host_tests::preview as test_preview;

pub(crate) mod routes;

/// Opaque retained framework surface used only by owning runner traversal.
#[doc(hidden)]
/// A window's History as the windows that own one hand it to a visitor.
pub(crate) type FrameworkHistorySurface = dyn host::HistoryChromeApi;
pub(crate) use host::SIDE_PANEL_WIDTH;
pub use host::{HISTORY, MakeHistory, builtin_history};
pub(crate) mod embedded;
