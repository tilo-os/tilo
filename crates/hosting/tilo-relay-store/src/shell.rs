//! The seam a workspace store drives native shell replay selection through.
//!
//! A host that serves the desktop's shell keeps one locally supplied
//! retention authority per workspace. With it bound, retention and replay
//! select an immutable basis under `shell-selected-v1/` and retire only what
//! exact historical proof pairs cover. Which frames those are is the
//! desktop's knowledge, not the store's: the store holds an opaque
//! [`ShellBinding`] that the embedder's [`ShellReplay`] adapter installed,
//! asks it for a [`ShellSelection`], a [`SelectedReplay`] or a [`ShellTail`],
//! and only steps them. Without a binding the store selects nothing and
//! retires nothing a selection would.
//!
//! The outcome types are plain data the store and its owner act on: what to
//! publish, and how each incoming input was represented.
use crate::StoreError;
use crate::bounded::{Step, WorkLimit, WorkStats};
use crate::tail_cache::SharedTailCache;
use std::any::Any;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The workspace directory holding the selected shell basis: a `CURRENT`
/// selector and immutable generations, each with its `basis.manifest`.
pub const DIRECTORY: &str = "shell-selected-v1";

/// What the owner publishes for a retained input once selection decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Publication {
    /// Publish the input itself.
    Original,
    /// Publish nothing: the selected basis already represents it.
    Suppress,
    /// Publish the newly selected generation at this path.
    Activate(PathBuf),
    /// Replay the committed fallback in place; no newer checkpoint was selected.
    ReplayRequired(PathBuf),
}

/// How a selection ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionOutcome {
    /// A single input's selection committed.
    Published(Publication),
    /// A batch selection committed.
    BatchPublished {
        /// A recovery generation to replay beside the publication.
        supplemental_recovery: Option<PathBuf>,
        /// What to publish.
        publication: Publication,
        /// How each incoming input was represented, in input order.
        dispositions: Vec<IncomingDisposition>,
    },
    /// Missing tail floors or dependencies prevent a coherent selection.
    UnresolvedTailDependencies,
}

/// How one incoming input ended up represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncomingDisposition {
    /// Covered by the selected basis.
    Covered,
    /// Staged, waiting for its pair.
    Staged,
    /// Published as part of the selected basis.
    SelectedLive,
    /// Published as itself.
    Original,
}

/// Why a projected batch selection ended the way it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchProjectionReason {
    Represented,
    /// Some input stays an original in the final basis.
    FinalBasisOmission,
    /// Missing tail floors or dependencies prevent a coherent projection.
    UnresolvedTailDependencies,
}

/// The projected outcome of a batch selection before its lineages are
/// promoted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchSelectionOutcome {
    /// A recovery generation to replay beside the publication.
    pub supplemental_recovery: Option<PathBuf>,
    /// What to publish.
    pub publication: Publication,
    /// How each incoming input was represented, in input order.
    pub dispositions: Vec<IncomingDisposition>,
    /// Why the projection ended this way.
    pub reason: BatchProjectionReason,
}

/// What one retained write did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainOutcome {
    /// Bytes written.
    pub written: u64,
    /// What to publish for it.
    pub publication: Publication,
}

/// What the selected snapshot says about an incoming tail before it is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Covered by the selected snapshot: the write is suppressed.
    Obsolete,
    /// Floor-safe and uncovered: write it, then try [`ShellTail::append`].
    Tail,
    /// Anything else: the full selected read and selection decide.
    Select,
}

/// What a workspace store tells the shell replay adapter about itself.
pub trait ShellStore: Send + Sync {
    /// The workspace's state directory.
    fn root(&self) -> &Path;
    /// The workspace id retained frames must carry.
    fn workspace_id(&self) -> u64;
    /// The tail cache every handle on [`Self::root`] shares.
    fn tail_cache(&self) -> &SharedTailCache;
    /// The bound shell authority, if any.
    fn shell_binding(&self) -> Option<&Arc<dyn ShellBinding>>;
    /// Install `binding` as this handle's shell authority. Only the adapter's
    /// bind, after it checked and persisted the authority, calls this.
    fn install_shell_binding(&mut self, binding: Arc<dyn ShellBinding>);
    /// Copy the continuity association of the retained record `source` to
    /// the selected link `destination`, accounting the work in `stats`.
    ///
    /// # Errors
    ///
    /// Returns an error when the association cannot be read or written.
    fn copy_measured(
        &self,
        source: &Path,
        destination: &Path,
        stats: &mut WorkStats,
    ) -> Result<(), StoreError>;
    fn handle(&self) -> Arc<dyn ShellStore>;
}

/// One workspace's bound shell retention authority.
///
/// It is installed by the [`ShellReplay`] adapter that checked it, and only
/// that adapter's selection code understands it. Rebinding after a restart is
/// mandatory: the persisted authority is a conflict fence, never trust.
pub trait ShellBinding: Any + Send + Sync {
    /// The device certificate of the room governor the authority names.
    fn governor(&self) -> &tilo_user_identity::DeviceCert;
    /// A selection of `store`'s retained history, staged under `scratch`,
    /// classifying `incoming` (the input just written) if any.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected basis cannot be opened or staged.
    fn selection(
        &self,
        store: &dyn ShellStore,
        scratch: PathBuf,
        incoming: Option<[u8; 32]>,
    ) -> Result<Box<dyn ShellSelection>, StoreError>;
    /// A batch selection over `incoming`, reading the projected lineage
    /// generations `lineage_overrides` names instead of their `CURRENT`.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected basis cannot be opened or staged.
    fn projected_selection(
        &self,
        store: &dyn ShellStore,
        scratch: PathBuf,
        incoming: Vec<[u8; 32]>,
        lineage_overrides: BTreeMap<OsString, PathBuf>,
    ) -> Result<Box<dyn ShellSelection>, StoreError>;
    /// A reader of `store`'s committed selected basis.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected basis cannot be opened.
    fn selected_read(
        &self,
        store: &dyn ShellStore,
    ) -> Result<Option<Box<dyn SelectedReplay>>, StoreError>;
    /// The cached cursor for the route the retained tail `bytes` writes,
    /// when the cache extends `store`'s selected `CURRENT`.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected `CURRENT` cannot be read.
    fn checkout_tail(
        &self,
        store: &dyn ShellStore,
        bytes: &[u8],
    ) -> Result<Option<Box<dyn ShellTail>>, StoreError>;
    /// The route key the retained tail `bytes` of `workspace` writes.
    fn incoming_route(&self, bytes: &[u8], workspace: u64) -> Option<String>;
}

/// A resumable selection pass, stepped in bounded quanta by its owner.
pub trait ShellSelection: Send {
    /// One quantum of selection.
    ///
    /// # Errors
    ///
    /// Returns an error when retained history cannot be read or the selected
    /// basis cannot be written; [`Self::publication_on_error`] then says
    /// what may already be visible.
    fn step(&mut self, limit: WorkLimit) -> Result<Step<SelectionOutcome>, StoreError>;
    /// Finish a projected batch's manifests without switching `CURRENT`.
    ///
    /// # Errors
    ///
    /// As [`Self::step`]; also when the selection would commit before the
    /// caller promoted its projected lineages.
    fn step_projection(
        &mut self,
        limit: WorkLimit,
    ) -> Result<Step<BatchSelectionOutcome>, StoreError>;
    /// What may already be visible if a later step fails after `CURRENT`'s
    /// switch was attempted.
    fn publication_on_error(&self) -> Option<Publication>;
    /// Whether the switch of the selected `CURRENT` was attempted.
    fn current_switch_attempted(&self) -> bool;
    /// The selection directory and this pass's staged generation, for the
    /// owner's cleanup of an abandoned pass.
    fn cleanup_paths(&self) -> (PathBuf, PathBuf);
    /// The work this pass did.
    fn stats(&self) -> &WorkStats;
    /// Keep `key`'s tail cursor too if this pass commits cleanly.
    fn want_route(&mut self, key: String);
    fn diagnostic_phase(&self) -> String;
}

/// A reader of the committed selected basis, stepped by its owner.
pub trait SelectedReplay: Send {
    /// One quantum of reading (and of recoverable retirement).
    ///
    /// # Errors
    ///
    /// Returns an error when the basis is invalid or cannot be read.
    fn step(&mut self) -> Result<Step<()>, StoreError>;
    /// Whether the selected basis already covers the retained frame `bytes`.
    ///
    /// # Errors
    ///
    /// Returns an error when a selected snapshot cannot be read or verified.
    fn obsolete(&self, bytes: &[u8]) -> Result<bool, StoreError>;
}

/// One route cursor checked out of the tail cache for a single retained tail.
pub trait ShellTail: Send {
    /// The selected snapshot's view of `bytes`.
    fn admission(&self, bytes: &[u8], workspace: u64) -> Admission;
    /// Whether an admitted [`Admission::Tail`], now durable, publishes as an
    /// original without selection. The cursor is unchanged when it does not.
    fn append(&mut self, bytes: &[u8], workspace: u64) -> bool;
    /// Return the cursor to `cache`.
    fn finish(self: Box<Self>, cache: &SharedTailCache);
}

/// The embedder's shell replay adapter.
pub trait ShellReplay: Send + Sync + 'static {
    /// Check `authority` (the adapter's serialised form) against `store`
    /// and any binding it holds or persisted, persist it, and install it.
    fn bind(
        &self,
        store: &mut dyn ShellStore,
        authority: &serde_json::Value,
    ) -> Result<(), StoreError>;
}
