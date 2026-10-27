//! Window-independent projection of a managed record's historical states.
//!
//! The controller reads a rolled-back clone of the live Automerge document.
//! It therefore neither commits an app's open transaction nor gives a
//! historical renderer access to a live record, persistence, or dispatch.

mod assets;
pub mod capability;
pub mod checked_files;
mod files_session;
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
pub use frozen_root::FrozenRootPreview;
pub(crate) mod host;
pub mod pending_controls;
pub use preview::*;
pub use registration::*;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ChangeHash};

use crate::history_source::{
    CompareError, HistorySource, compare_entries, groups::read_named_ranges,
};
use crate::{HistoryEntry, HistoryEntryId, NamedRange, RecordId};

/// Full authority namespace for one historical resource.
///
/// A record id alone is not globally unique: the same spelling in another
/// profile, workspace, or app belongs to a different authority domain.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HistoryResource {
    pub profile_id: String,
    pub workspace_id: String,
    pub app_id: String,
    pub record_id: RecordId,
}

impl HistoryResource {
    pub fn new(
        profile_id: impl Into<String>,
        workspace_id: impl Into<String>,
        app_id: impl Into<String>,
        record_id: RecordId,
    ) -> Self {
        Self {
            profile_id: profile_id.into(),
            workspace_id: workspace_id.into(),
            app_id: app_id.into(),
            record_id,
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
///
/// This value intentionally carries no live document, record handle,
/// permission, persistence, publication, invocation, or effect authority.
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

/// Opaque proof that a particular selected state was reviewed against a
/// particular live frontier. Validation grants no authority and writes
/// nothing; the host must separately recheck permissions before restoring.
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
    source: HistorySource,
    entries: Vec<HistoryEntry>,
    /// The named runs of changes the document carries, as the timeline bands them.
    named: Vec<NamedRange>,
    /// What members' devices wrote that changes did, by change.
    descriptions: HashMap<HistoryEntryId, String>,
    projected_heads: Option<Vec<ChangeHash>>,
    observed_live_heads: Vec<ChangeHash>,
    selected: Option<SelectedPreview>,
    side: PreviewSide,
    live_updates: bool,
    generation: u64,
    snapshots: HashMap<Vec<ChangeHash>, Arc<[u8]>>,
    /// How many of the newest changes the projection describes.
    window: usize,
    /// The projected history holds changes older than the window.
    has_earlier: bool,
    /// For an app's own history, which of the session's lists of its rooms'
    /// verified agents ([`crate::history_source::set_room_agents`]) the
    /// source names; a newer list is named on the next refresh.
    room_agents: Option<u64>,
    /// The room's participants by id and name, as presence last named them:
    /// the authors the source resolves besides registered identities.
    roster: Vec<(String, String)>,
}

/// How many of the newest changes a history view describes when it opens,
/// and how many more each request for earlier ones adds. Describing a change
/// costs a replay step, so a long history opens at its latest page instead
/// of replaying everything.
pub const HISTORY_PAGE: usize = 200;

impl HistoryController {
    /// Build a controller with the default factual history source.
    ///
    /// # Errors
    ///
    /// Returns a projection error if the committed history cannot be read.
    pub fn new(resource: HistoryResource, live: &AutoCommit) -> Result<Self, HistoryError> {
        Self::with_source(resource, HistorySource::new(), live)
    }

    /// Build a controller with a caller-configured source, for example one
    /// carrying the host's author directory and artifact kind.
    ///
    /// # Errors
    ///
    /// Returns a projection error if the committed history cannot be read.
    pub fn with_source(
        resource: HistoryResource,
        source: HistorySource,
        live: &AutoCommit,
    ) -> Result<Self, HistoryError> {
        let mut controller = Self::unprojected_with_source(resource, source, Vec::new());
        controller.refresh(live)?;
        Ok(controller)
    }

    /// A controller that has observed the committed frontier `live_heads`
    /// but projected nothing yet. Projecting replays the whole change log,
    /// which only an open history view needs; the first [`Self::refresh`]
    /// against a document does it.
    pub(crate) fn unprojected(resource: HistoryResource, live_heads: Vec<ChangeHash>) -> Self {
        // An app's own history names the agents the session verified for
        // its rooms, as the workspace's Activity does.
        let revision = crate::history_source::room_agents_revision();
        let source = HistorySource::new()
            .with_authors(crate::history_source::AuthorDirectory::with_room_agents());
        let mut controller = Self::unprojected_with_source(resource, source, live_heads);
        controller.room_agents = Some(revision);
        controller
    }

    fn unprojected_with_source(
        resource: HistoryResource,
        source: HistorySource,
        live_heads: Vec<ChangeHash>,
    ) -> Self {
        Self {
            resource,
            source: source.with_max_entries(HISTORY_PAGE),
            entries: Vec::new(),
            named: Vec::new(),
            descriptions: HashMap::new(),
            projected_heads: None,
            observed_live_heads: sorted_heads(live_heads),
            selected: None,
            side: PreviewSide::After,
            live_updates: false,
            generation: 0,
            roster: Vec::new(),
            snapshots: HashMap::new(),
            window: HISTORY_PAGE,
            has_earlier: false,
            room_agents: None,
        }
    }

    /// Whether changes older than the listed ones exist.
    pub fn has_earlier(&self) -> bool {
        self.has_earlier
    }

    /// List another page of older changes.
    ///
    /// # Errors
    ///
    /// Returns a projection error if the committed history cannot be read.
    pub fn show_earlier(&mut self, live: &AutoCommit) -> Result<(), HistoryError> {
        self.window = self.window.saturating_add(HISTORY_PAGE);
        self.source = std::mem::take(&mut self.source).with_max_entries(self.window);
        self.projected_heads = None;
        self.refresh(live)
    }

    pub fn resource(&self) -> &HistoryResource {
        &self.resource
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    pub fn named_ranges(&self) -> &[NamedRange] {
        &self.named
    }

    pub fn descriptions(&self) -> &HashMap<HistoryEntryId, String> {
        &self.descriptions
    }

    /// Committed activity at `live`'s frontier, in causal order, leaving
    /// this controller's projection and selection where they are.
    #[cfg(any(feature = "native", test))]
    pub(crate) fn entries_at(&self, live: &AutoCommit) -> Vec<HistoryEntry> {
        let mut projection = committed_clone(live);
        if self.projected_heads.as_ref() == Some(&sorted_heads(projection.get_heads())) {
            return self.entries.clone();
        }
        self.source.entries(&mut projection)
    }

    /// Reproject factual activity from committed graph state. An existing
    /// preview stays pinned to its exact bytes and heads.
    pub(crate) fn follow_roster(&mut self, presence: &crate::presence::PresenceCtx) -> bool {
        let mut roster: Vec<(String, String)> = presence
            .names
            .iter()
            .map(|(crate::permissions::UserId(id), name)| (id.clone(), name.clone()))
            .collect();
        roster.compact_sort();
        if roster == self.roster {
            return false;
        }
        self.source
            .authors_mut()
            .register_roster(roster.iter().map(|(id, name)| (id.as_str(), name.as_str())));
        self.roster = roster;
        self.projected_heads = None;
        true
    }

    /// Returns a projection error if the committed history cannot be read.
    pub fn refresh(&mut self, live: &AutoCommit) -> Result<(), HistoryError> {
        let mut projection = committed_clone(live);
        let live_heads = sorted_heads(projection.get_heads());
        self.live_updates = self
            .selected
            .as_ref()
            .is_some_and(|selected| selected.after.heads != live_heads);
        self.observed_live_heads.clone_from(&live_heads);
        if let Some(named) = self.room_agents {
            let current = crate::history_source::room_agents_revision();
            if current != named {
                let mut authors = crate::history_source::AuthorDirectory::with_room_agents();
                authors.register_roster(
                    self.roster
                        .iter()
                        .map(|(id, name)| (id.as_str(), name.as_str())),
                );
                *self.source.authors_mut() = authors;
                self.room_agents = Some(current);
                self.projected_heads = None;
            }
        }
        if self.projected_heads.as_ref() == Some(&live_heads) {
            return Ok(());
        }
        let entries = self.source.entries(&mut projection);
        let named = read_named_ranges(&projection, self.source.authors());
        self.descriptions = crate::history_source::descriptions::read_descriptions(&projection);

        self.has_earlier = projection.get_changes_meta(&[]).len() > self.window;
        self.projected_heads = Some(live_heads);
        self.entries = entries;
        self.named = named;
        Ok(())
    }

    /// Pin the exact before and after causal frontiers of one change or group.
    /// Failed selection leaves the previous valid preview untouched.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::EmptySelection`] or
    /// [`HistoryError::UnknownEntry`] for an invalid selection, and
    /// [`HistoryError::Materialize`] when Automerge cannot build its states.
    pub fn select(
        &mut self,
        live: &AutoCommit,
        entries: &[HistoryEntryId],
    ) -> Result<(), HistoryError> {
        let mut projection = committed_clone(live);
        let current_live_heads = sorted_heads(projection.get_heads());
        let comparison = compare_entries(&mut projection, entries).map_err(HistoryError::from)?;
        let refreshed_projection = if self.projected_heads.as_ref() == Some(&current_live_heads) {
            None
        } else {
            let projected_entries = self.source.entries(&mut projection);
            let named = read_named_ranges(&projection, self.source.authors());
            Some((projected_entries, named))
        };
        let factual_entries = refreshed_projection
            .as_ref()
            .map_or(self.entries.as_slice(), |(entries, _)| entries.as_slice());
        let effect_only = entries.iter().all(|id| {
            factual_entries
                .iter()
                .find(|entry| entry.id == *id)
                .is_some_and(is_effect_receipt)
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
        if let Some((entries, named)) = refreshed_projection {
            self.entries = entries;
            self.named = named;
            self.projected_heads = Some(current_live_heads.clone());
        }
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
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::NoPreview`] when no history is selected.
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

    /// Whether the observed committed live frontier differs from the pinned
    /// selected after-frontier.
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
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::NoPreview`] when no history is selected and
    /// [`HistoryError::EffectOnlySelection`] for an external effect receipt.
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

    /// Validate a review token without changing the document. The host owns
    /// the separate authority check and the eventual canonical restore write.
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

fn is_effect_receipt(entry: &HistoryEntry) -> bool {
    entry.is_effect_receipt()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod host_tests;

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
/// No construction, document, review or write authority is public on this type.
#[doc(hidden)]
pub use host::HistoryHost as FrameworkHistorySurface;
pub(crate) mod embedded;
