//! Window-independent projection of a managed record's historical states. The controller reads a
//! rolled-back clone of the live Automerge document.

#[cfg(feature = "model")]
mod assets;

/// Moves on whenever something reaches a History surface from outside a refresh of it: an asset or file
/// reply another thread delivers, a request to open or close it, a command one of its controls queued,
/// a width that changes its layout.
static NEWS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`NEWS`] as it stands.
pub fn news() -> u64 {
    NEWS.load(std::sync::atomic::Ordering::Acquire)
}

/// Something reached a History surface ([`NEWS`]).
pub(crate) fn note_news() {
    NEWS.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

pub mod api;
#[cfg(feature = "model")]
pub mod capability;
#[cfg(feature = "model")]
pub mod coverage;
#[cfg(feature = "model")]
pub(crate) mod feed;
#[cfg(feature = "model")]
pub mod folder;
#[cfg(feature = "model")]
mod listing;
#[cfg(feature = "model")]
pub mod preview;
#[cfg(feature = "model")]
pub use assets::{HistoricalAssetRequest, HistoricalAssetResponse, HistoricalAssetSource};
#[cfg(feature = "model")]
use tilo_sort::CompactSort as _;
#[cfg(feature = "model")]
pub mod owner;
#[cfg(feature = "model")]
pub mod registration;
#[cfg(feature = "model")]
pub use owner::HistoryOwner;
#[cfg(feature = "model")]
pub(crate) mod canvas;
pub mod frame;
#[cfg(feature = "model")]
pub(crate) mod frozen_root;
#[cfg(feature = "model")]
pub use frozen_root::{FrozenRootPreview, outline_preview};
#[cfg(feature = "model")]
pub mod host;
#[cfg(feature = "model")]
pub mod pending_controls;
#[cfg(feature = "model")]
pub use coverage::{HistoricalGap, HistoricalSupport};
#[cfg(feature = "model")]
pub use preview::*;
#[cfg(feature = "model")]
pub use registration::*;

#[cfg(feature = "model")]
use std::collections::HashMap;
#[cfg(feature = "model")]
use std::fmt;
#[cfg(feature = "model")]
use std::rc::Rc;
#[cfg(feature = "model")]
use std::sync::Arc;

#[cfg(feature = "model")]
use automerge::transaction::Transactable;
#[cfg(feature = "model")]
use automerge::{AutoCommit, ChangeHash};

#[cfg(feature = "model")]
use crate::history_source::{CompareError, HistorySource, compare_entries};
#[cfg(feature = "model")]
use crate::{DocumentId, HistoryEntry};
use tilo_ui_base::HistoryEntryId;
#[cfg(feature = "model")]
use tilo_ui_base::permissions::MemberId;

#[cfg(feature = "model")]
/// Full authority namespace for one historical resource.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HistoryResource {
    pub profile_id: String,
    pub workspace_id: String,
    pub app_id: String,
    pub document_id: DocumentId,
}

#[cfg(feature = "model")]
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

#[cfg(feature = "model")]
/// Which exact frontier of a selected change or group is being previewed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PreviewSide {
    Before,
    #[default]
    After,
}

#[cfg(feature = "model")]
/// Immutable Automerge bytes at one exact causal frontier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistorySnapshot {
    bytes: Arc<[u8]>,
    heads: Vec<ChangeHash>,
    side: PreviewSide,
    selection: Vec<HistoryEntryId>,
}

#[cfg(feature = "model")]
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

#[cfg(feature = "model")]
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

#[cfg(feature = "model")]
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

#[cfg(feature = "model")]
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

#[cfg(feature = "model")]
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

#[cfg(feature = "model")]
impl std::error::Error for HistoryError {}

#[cfg(feature = "model")]
struct SelectedPreview {
    selection: Vec<HistoryEntryId>,
    before: HistorySnapshot,
    after: HistorySnapshot,
    effect_only: bool,
    part: Option<SelectedPart>,
}

#[cfg(feature = "model")]
/// Documents a window keeps beside its own, whose versions its History shows and restores: the parts
/// of the app's chain. The workspace's history lists their changes with the window's own.
pub trait HistoryParts {
    /// Every part's committed frontier, in one list: it moves whenever a part changes or arrives.
    fn heads(&self) -> Vec<ChangeHash>;
    /// Every part's committed document.
    fn documents(&self) -> Vec<(HistoryPart, AutoCommit)>;
    /// The window's view of what `part` held at `version.heads`, drawn over `version.own`, the
    /// window's document as it is now.
    ///
    /// # Errors
    /// Why the version cannot be drawn.
    fn preview(&self, version: PartVersion<'_>) -> Result<Box<dyn HistoryPreview>, String>;
    /// Write `part` back as it was at `heads`, as a new change by `author`.
    ///
    /// # Errors
    /// Why the part was not restored.
    fn restore(
        &self,
        part: &HistoryPart,
        heads: &[ChangeHash],
        author: &MemberId,
    ) -> Result<(), String>;
}

#[cfg(feature = "model")]
/// One part of a chain whose changes History lists: the chain's document id and the part's number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPart {
    pub chain: String,
    pub index: u64,
}

#[cfg(feature = "model")]
/// A version of a part to draw: the part, the frontier shown, the selected changes' frontiers before
/// and after them, the window's document and where the person left the previous version.
pub struct PartVersion<'a> {
    pub part: &'a HistoryPart,
    pub heads: &'a [ChangeHash],
    pub before: &'a [ChangeHash],
    pub after: &'a [ChangeHash],
    pub own: &'a AutoCommit,
    pub view: PreviewView,
}

#[cfg(feature = "model")]
/// The selected changes are a part's: the part, the window's document and the part's frontier when
/// they were selected.
struct SelectedPart {
    part: HistoryPart,
    own: AutoCommit,
    live_heads: Vec<ChangeHash>,
}

#[cfg(feature = "model")]
pub struct HistoryController {
    resource: HistoryResource,
    /// The documents kept beside the resource, whose versions are shown when the selected changes
    /// are theirs.
    parts: Option<Rc<dyn HistoryParts>>,
    observed_live_heads: Vec<ChangeHash>,
    selected: Option<SelectedPreview>,
    side: PreviewSide,
    live_updates: bool,
    generation: u64,
    snapshots: HashMap<Vec<ChangeHash>, Arc<[u8]>>,
}

#[cfg(feature = "model")]
impl HistoryController {
    /// A controller over `resource` that has observed the committed frontier `live_heads`.
    pub fn new(resource: HistoryResource, live_heads: Vec<ChangeHash>) -> Self {
        Self {
            resource,
            parts: None,
            observed_live_heads: sorted_heads(live_heads),
            selected: None,
            side: PreviewSide::After,
            live_updates: false,
            generation: 0,
            snapshots: HashMap::new(),
        }
    }

    /// Show and restore the versions of `parts` when the selected changes are theirs.
    #[must_use]
    pub fn with_parts(mut self, parts: Option<Rc<dyn HistoryParts>>) -> Self {
        self.parts = parts;
        self
    }

    /// Whether the selected part changed since its changes were selected.
    fn part_moved(&self, selected: &SelectedPart) -> bool {
        self.parts.as_ref().is_some_and(|parts| {
            parts
                .documents()
                .into_iter()
                .find(|(held, _)| *held == selected.part)
                .map(|(_, mut document)| sorted_heads(document.get_heads()))
                .as_ref()
                != Some(&selected.live_heads)
        })
    }

    /// Observe `live`'s committed frontier: whether the pinned preview is behind it, or behind its
    /// part's.
    pub fn observe(&mut self, live: &AutoCommit) {
        let live_heads = sorted_heads(committed_clone(live).get_heads());
        self.live_updates = self.selected.as_ref().is_some_and(|selected| {
            selected.part.as_ref().map_or_else(
                || selected.after.heads != live_heads,
                |part| selected.after.heads != part.live_heads || self.part_moved(part),
            )
        });
        self.observed_live_heads = live_heads;
    }

    pub fn resource(&self) -> &HistoryResource {
        &self.resource
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
        let (comparison, part) = match compare_entries(&mut projection, entries) {
            Ok(comparison) => (comparison, None),
            Err(CompareError::UnknownEntry(unknown)) => self
                .part_comparison(entries, &projection)
                .ok_or(HistoryError::UnknownEntry(unknown))?,
            Err(error) => return Err(error.into()),
        };
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
        let live_updates = &after.heads
            != part
                .as_ref()
                .map_or(&current_live_heads, |part| &part.live_heads);

        self.generation = self.generation.wrapping_add(1);
        self.observed_live_heads = current_live_heads;
        self.selected = Some(SelectedPreview {
            selection,
            before,
            after,
            effect_only,
            part,
        });
        self.side = PreviewSide::After;
        self.live_updates = live_updates;
        Ok(())
    }

    /// The comparison of `entries` in the part that made them, with the part, `own` and the part's
    /// frontier now.
    fn part_comparison(
        &self,
        entries: &[HistoryEntryId],
        own: &AutoCommit,
    ) -> Option<(
        crate::history_source::HistoryComparison,
        Option<SelectedPart>,
    )> {
        self.parts
            .as_ref()?
            .documents()
            .into_iter()
            .find_map(|(part, mut document)| {
                let comparison = compare_entries(&mut document, entries).ok()?;
                let selected = SelectedPart {
                    part,
                    own: own.clone(),
                    live_heads: sorted_heads(document.get_heads()),
                };
                Some((comparison, Some(selected)))
            })
    }

    /// The part the selected changes were made in, when they are a part's.
    pub fn selected_part(&self) -> Option<&HistoryPart> {
        self.selected
            .as_ref()
            .and_then(|selected| selected.part.as_ref())
            .map(|selected| &selected.part)
    }

    /// The window's view of the part the selected changes were made in, at `snapshot`; `None` when
    /// they are the window's document's own.
    pub(crate) fn part_preview(
        &self,
        snapshot: &HistorySnapshot,
        view: PreviewView,
    ) -> Option<Result<Box<dyn HistoryPreview>, String>> {
        let selected = self.selected.as_ref()?;
        let part = selected.part.as_ref()?;
        let parts = self.parts.as_ref()?;
        Some(parts.preview(PartVersion {
            part: &part.part,
            heads: snapshot.heads(),
            before: &selected.before.heads,
            after: &selected.after.heads,
            own: &part.own,
            view,
        }))
    }

    /// Write the reviewed part back as it was at the reviewed version, as a new change by `author`.
    ///
    /// # Errors
    /// The review no longer matches the selection, the part changed since, or the part refused.
    pub(crate) fn restore_part(
        &self,
        review: &RestoreReview,
        author: &MemberId,
    ) -> Result<(), HistoryError> {
        let selected = self
            .selected
            .as_ref()
            .ok_or(HistoryError::ReviewInvalidated)?;
        let (Some(part), Some(parts)) = (&selected.part, &self.parts) else {
            return Err(HistoryError::ReviewInvalidated);
        };
        if review.generation != self.generation
            || review.selection != selected.selection
            || review.selected_heads != selected.after.heads
        {
            return Err(HistoryError::ReviewInvalidated);
        }
        let now = parts
            .documents()
            .into_iter()
            .find(|(held, _)| *held == part.part)
            .map(|(_, mut document)| sorted_heads(document.get_heads()));
        if now.as_ref() != Some(&review.reviewed_live_heads) {
            return Err(HistoryError::StaleLiveHeads);
        }
        parts
            .restore(&part.part, &review.selected_heads, author)
            .map_err(HistoryError::Materialize)
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
            reviewed_live_heads: selected.part.as_ref().map_or_else(
                || self.observed_live_heads.clone(),
                |part| part.live_heads.clone(),
            ),
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
        // A part's version is written back by the part ([`Self::restore_part`]).
        if selected.part.is_some() {
            return Err(HistoryError::ReviewInvalidated);
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

#[cfg(feature = "model")]
impl From<CompareError> for HistoryError {
    fn from(error: CompareError) -> Self {
        match error {
            CompareError::EmptySelection => Self::EmptySelection,
            CompareError::UnknownEntry(id) => Self::UnknownEntry(id),
            CompareError::Materialize(error) => Self::Materialize(error),
        }
    }
}

#[cfg(feature = "model")]
fn committed_clone(live: &AutoCommit) -> AutoCommit {
    let mut projection = live.clone();
    if projection.pending_ops() != 0 {
        projection.rollback();
    }
    projection
}

#[cfg(feature = "model")]
pub fn sorted_heads(mut heads: Vec<ChangeHash>) -> Vec<ChangeHash> {
    heads.compact_sort();
    heads
}

#[cfg(feature = "model")]
#[cfg(test)]
mod tests;

/// The fixtures History's own tests and the app runner's share.
#[cfg(feature = "model")]
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(feature = "model")]
#[cfg(any(test, feature = "test-support"))]
pub use api::HISTORY_RESOURCE;

#[cfg(feature = "model")]
#[cfg(any(test, feature = "test-support"))]
pub use host::{HistoryHost, SIDE_PANEL_WIDTH};

#[cfg(feature = "model")]
#[cfg(any(test, feature = "test-support"))]
pub use embedded::{FocusRestrictions, allows_focus, has_surfaces};

#[cfg(feature = "model")]
#[cfg(any(test, feature = "test-support"))]
pub use testing::preview as test_preview;

#[cfg(feature = "model")]
pub mod routes;

#[cfg(feature = "model")]
/// Opaque retained framework surface used only by owning runner traversal.
#[doc(hidden)]
/// A window's History as the windows that own one hand it to a visitor.
pub(crate) type FrameworkHistorySurface = host::HistoryHost;
#[cfg(feature = "model")]
pub mod embedded;
