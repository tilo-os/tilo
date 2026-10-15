//! Closed selected-parent view. This is a child of retained_source_relay; only
//! that root owner can supply a concrete authenticated HomeDirectory.
//! Neither names nor inode observations construct edit authority.
use super::super::{
    retained_selected_edit::{RootSelectedEditAdmission, RootSelectedEditor},
    retained_source_worker::RetainedSourceWorker,
};
use super::{Consumer, Custody, HomeDirectory, InstalledRetainedLaunchInput, Source};
use runtime_fs::directory_connection::{DirectoryConnection, ROOT_DIRECTORY};
use runtime_fs::{
    EntryKind, EntryName, Metadata, OpenMode,
    directory_backend::{
        DirectoryRow, DirectoryRows, DirectoryStat, DirectoryStreamPage, DirectoryStreamRow,
        JobPoll, JobUpdate, WorkspaceDirectoryBackend, WorkspaceDirectoryCursorBackend,
        WorkspaceJobBackend, WorkspaceMetadataBackend,
    },
    operations::ItemState,
    workspace_server::WorkspaceFileBackend,
};
use std::io;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[path = "retained_selected_parent/live_lineage.rs"]
mod live_lineage;
use live_lineage::LiveLineage;
const MAX_TEMPS: usize = 64;
const MAX_CURSORS: usize = 64;
const MAX_REPLACEMENT_HANDOFFS: usize = 64;
fn denied() -> String {
    "Selected-parent scope refused".into()
}

use super::super::{
    retained_history_read::RootHistoryReadContinuity,
    retained_selected_edit::{AcknowledgedSelectedReplacement, ObservedReplacementFinished},
    retained_source_worker::PendingHandoff,
};

/// Each row owns genuine completion evidence and keeps its exact stream through
/// both independently observed retirement ACKs. Partial progress is retryable.
struct CompletedReplacement {
    handoff: PendingHandoff,
    proof: Arc<AcknowledgedSelectedReplacement>,
    continuity: Option<Arc<RootHistoryReadContinuity>>,
    finished: ObservedReplacementFinished,
    retirement: Mutex<CompletedRetirement>,
}
#[derive(Default)]
struct CompletedRetirement {
    daemon_retired: bool,
    rpc_retired: bool,
    read_retired: bool,
    pin_retired: bool,
    job_retired: bool,
    pending_retired: bool,
}
impl CompletedRetirement {
    fn observed(&self) -> bool {
        self.daemon_retired
            && self.rpc_retired
            && self.read_retired
            && self.pin_retired
            && self.job_retired
            && self.pending_retired
    }
}

fn remove_older_completed<T>(
    rows: &mut Vec<Arc<T>>,
    latest: &Arc<T>,
    older: &Arc<T>,
    progress: &CompletedRetirement,
) -> Result<(), String> {
    if !progress.observed() || !rows.iter().any(|row| Arc::ptr_eq(row, latest)) {
        return Err("Save retirement unobserved or successor changed".into());
    }
    let index = rows
        .iter()
        .position(|row| Arc::ptr_eq(row, older))
        .ok_or_else(denied)?;
    if Arc::ptr_eq(older, latest) {
        return Err("Newest Save must remain retained".into());
    }
    rows.remove(index);
    Ok(())
}

/// Owns a fresh editor-only worker, connection and source provenance. It cannot
/// be constructed from a shared Files HomeDirectory or caller-supplied stream.
/// The root driver retains this owner through all consumers and retires off UI.
pub(in crate::setup) struct RootSelectedEditSource {
    editor: Arc<RootSelectedEditor>,
    replacement: Arc<super::super::retained_selected_edit::RootSelectedReplacementSeed>,
    worker: Option<RetainedSourceWorker>,
    selected_history: Option<Arc<super::super::retained_history_read::RootHistoryReadSession>>,
    custody: Option<Arc<Custody>>,
    exclusive: Option<ExclusiveSelectedSource>,
    source_view: [u8; 32],
    worker_epoch: Option<launcher::owned_service::ServiceCustody>,
    retirement: Option<Result<(), String>>,
    completed_handoffs: Vec<Arc<CompletedReplacement>>,
    // Includes completed and uncertain transfers. Session budget is reserved
    // before native effect; an uncertain connection never refunds its slot.
    handoff_reservations: HashSet<[u8; 16]>,
}
impl RootSelectedEditSource {
    pub(in crate::setup) fn start(
        editor: Arc<RootSelectedEditor>,
        cancel: Arc<AtomicBool>,
        replacement: Arc<super::super::retained_selected_edit::RootSelectedReplacementSeed>,
    ) -> io::Result<Self> {
        editor.check()?;
        replacement.check()?;
        if !replacement.uses_input(editor.input()) {
            return Err(super::refused());
        }
        // Start exactly once using the independently admitted editor input.
        // Origin Files lookup is evidence held by editor; it is not this input.
        let (worker, stream) = match RetainedSourceWorker::start_home_for_editor(
            editor.input().clone(),
            cancel.clone(),
            replacement.clone(),
        ) {
            Ok(started) => started,
            Err(error) => {
                editor.input().revoke();
                return Err(error);
            }
        };
        let source_view = worker.source_view_sha256();
        let mut owner = Self {
            editor: editor.clone(),
            replacement: replacement.clone(),
            worker: Some(worker),
            selected_history: None,
            custody: None,
            exclusive: None,
            source_view,
            worker_epoch: None,
            retirement: None,
            completed_handoffs: Vec::new(),
            handoff_reservations: HashSet::new(),
        };
        owner.worker_epoch = match owner
            .worker
            .as_ref()
            .ok_or_else(super::refused)?
            .check_current_custody()
        {
            Ok(epoch) => Some(epoch),
            Err(error) => {
                let cleanup = owner.retire();
                return Err(cleanup.err().unwrap_or(error));
            }
        };
        let connection = match DirectoryConnection::connected(stream) {
            Ok(connection) => connection,
            Err(error) => {
                let cleanup = owner.retire();
                return Err(cleanup.err().unwrap_or(error));
            }
        };
        let source = Source::Home(connection);
        let custody = Arc::new(Custody {
            input: editor.input().clone(),
            cancel,
            revoked: AtomicBool::new(false),
            interrupt: source.clone(),
            source: Mutex::new(Some(source)),
            consumer: Consumer::SelectedEditor(editor.clone()),
            history: Mutex::new(None),
        });
        owner.custody = Some(custody.clone());
        owner.exclusive = Some(ExclusiveSelectedSource {
            replacement,
            root: Arc::new(HomeDirectory {
                custody,
                id: ROOT_DIRECTORY,
            }),
        });
        if let Err(error) = owner.check_current() {
            let cleanup = owner.retire();
            return Err(cleanup.err().unwrap_or(error));
        }
        Ok(owner)
    }
    pub(in crate::setup) fn uses_editor(&self, editor: &Arc<RootSelectedEditor>) -> bool {
        Arc::ptr_eq(&self.editor, editor)
    }
    pub(in crate::setup) fn owns_source(&self, source: &Arc<SourceSelectedParent>) -> bool {
        self.custody
            .as_ref()
            .is_some_and(|custody| Arc::ptr_eq(custody, &source.root.custody))
    }
    /// Kernel-observed service custody and current certified editor, not a
    /// metadata-only source context. Failure retires permission immediately;
    /// explicit off-UI retirement still owns the actual namespace join.
    pub(in crate::setup) fn check_current(&self) -> io::Result<()> {
        let result = (|| {
            if self.retirement.is_some() {
                return Err(super::refused());
            }
            let worker = self.worker.as_ref().ok_or_else(super::refused)?;
            let expected = self.worker_epoch.ok_or_else(super::refused)?;
            let before = worker.check_current_custody()?;
            if before != expected {
                return Err(super::refused());
            }
            self.editor.check()?;
            self.custody
                .as_ref()
                .ok_or_else(super::refused)?
                .current()?;
            let after = worker.check_current_custody()?;
            if after != before {
                return Err(super::refused());
            }
            self.editor.check()
        })();
        if result.is_err() {
            self.editor.revoke();
            if let Some(custody) = &self.custody {
                custody.revoke();
            }
        }
        result
    }
    pub(in crate::setup) fn source_view_sha256(&self) -> io::Result<[u8; 32]> {
        self.check_current()?;
        let view = self.source_view;
        self.check_current()?;
        Ok(view)
    }
    pub(in crate::setup) fn resolve(
        &mut self,
        components: &[EntryName],
    ) -> Result<Arc<SourceSelectedParent>, String> {
        self.check_current().map_err(|e| e.to_string())?;
        let exclusive = self.exclusive.take().ok_or_else(denied)?;
        let source = SourceSelectedParent::resolve(exclusive, components)?;
        self.check_current().map_err(|e| e.to_string())?;
        Ok(source)
    }
    pub(in crate::setup) fn install_selected_history_read(
        &mut self,
        reader: Arc<super::super::retained_selected_editor_history_read::RootHistoryReadEnrollment>,
        selected: &Arc<SourceSelectedParent>,
    ) -> io::Result<()> {
        self.check_current()?;
        reader.check()?;
        if self.selected_history.is_some()
            || !self.owns_source(selected)
            || !reader.uses_input(self.editor.input())
            || !reader
                .selected_editor()
                .is_some_and(|editor| Arc::ptr_eq(editor, &self.editor))
        {
            return Err(super::refused());
        }
        let publisher = self
            .worker
            .as_ref()
            .ok_or_else(super::refused)?
            .history_read_publisher()?;
        let session = super::super::retained_history_read::RootHistoryReadSession::admit_selected(
            reader, publisher, selected,
        )?;
        let mut installed = selected.history_read.lock().map_err(|_| super::refused())?;
        if installed.is_some() {
            return Err(super::refused());
        }
        self.check_current()?;
        *installed = Some(session.cleanup());
        self.selected_history = Some(session);
        self.check_current()
    }
    /// Cleanup custody only; revocation must not obscure unfinished native work.
    pub(in crate::setup) fn has_unresolved_replacement(&self) -> bool {
        self.replacement.has_unresolved_replacement()
    }
    /// Both actual native worker stop and exact RPC custody departure were
    /// observed, including a terminal RPC error. Absence alone is no receipt.
    pub(in crate::setup) fn observed_stop(&self) -> bool {
        self.retirement.is_some()
            && self.worker.is_none()
            && self.custody.is_none()
            && self.exclusive.is_none()
            && self.completed_handoffs.is_empty()
    }
    /// Terminal revocation is immediate. Actual joins must be driven off UI;
    /// errors retain this owner until callbacks release custody and stop is seen.
    pub(in crate::setup) fn retire(&mut self) -> io::Result<()> {
        // The seed owns actual started jobs before native effect is possible.
        // Unknown effect keeps the source worker/pin/handoff alive; no revoke,
        // RPC shutdown or worker signal may substitute for signed completion.
        if self.replacement.has_unresolved_replacement() {
            return Err(io::Error::other(
                "Selected replacement still owns source custody",
            ));
        }
        // The session calls this only after actual editor consumer stop and
        // target/relay retirement. Close previous execution streams while the
        // genuine source worker is still held, before any source shutdown.
        if let Some(result) = &self.retirement {
            return result
                .as_ref()
                .copied()
                .map_err(|error| io::Error::other(error.clone()));
        }
        // Actual Reader cap/driver retirement must precede source revocation.
        // Failed/unknown cleanup preserves this same session and source owner.
        if let Some(history) = &self.selected_history {
            history.retire()?;
        }
        self.selected_history.take();
        self.completed_handoffs.clear();
        self.editor.input().revoke();
        if let Some(custody) = &self.custody {
            custody.revoke();
        }
        if let Some(worker) = &mut self.worker {
            worker.retire()?;
        }
        self.exclusive.take();
        if let Some(custody) = self.custody.take() {
            let custody = match Arc::try_unwrap(custody) {
                Ok(custody) => custody,
                Err(custody) => {
                    self.custody = Some(custody);
                    return Err(io::Error::other(
                        "Selected edit source still has live consumer custody",
                    ));
                }
            };
            // Native worker stop is already observed, but RPC retirement still
            // reports its own outcome. No receipt is synthesized from EOF.
            let (mut source, lock_ok) = match custody.source.into_inner() {
                Ok(source) => (source, true),
                Err(error) => (error.into_inner(), false),
            };
            drop(custody.interrupt);
            let result = source
                .take()
                .ok_or_else(super::refused)
                .and_then(Source::retire)
                .and(if lock_ok {
                    Ok(())
                } else {
                    Err(super::refused())
                });
            self.retirement = Some(result.as_ref().copied().map_err(ToString::to_string));
            self.worker.take();
            return result;
        }
        // Initial connection failed before any custody was constructed. The
        // actual worker was retired above; its observed stop permits release.
        self.worker.take();
        self.retirement = Some(Ok(()));
        Ok(())
    }
}
impl Drop for RootSelectedEditSource {
    fn drop(&mut self) {
        // This root-owned source is retired off UI. An unexpected drop cannot
        // revoke the actual worker while the seed still holds unknown native
        // effects. Registry removal requires real ACK, lineage and Finished.
        while self.replacement.has_unresolved_replacement() || !self.completed_handoffs.is_empty() {
            std::thread::park_timeout(std::time::Duration::from_secs(1));
        }
        self.editor.input().revoke();
        if self.retirement.is_some() {
            return;
        }
        if let Some(custody) = &self.custody {
            custody.revoke();
        }
        // Drop must not block UI or claim unknown shutdown. The root supervisor
        // owns explicit retirement; unwinding retains the exact revoked owner.
        if let Some(history) = self.selected_history.take() {
            std::mem::forget(history);
        }
        if let Some(worker) = self.worker.take() {
            std::mem::forget(worker);
        }
        if let Some(custody) = self.custody.take() {
            std::mem::forget(custody);
        }
        if let Some(exclusive) = self.exclusive.take() {
            std::mem::forget(exclusive);
        }
    }
}

pub(super) struct ExclusiveSelectedSource {
    replacement: Arc<super::super::retained_selected_edit::RootSelectedReplacementSeed>,
    root: Arc<HomeDirectory>,
}

/// Held source provenance resolved only by the genuine root relay owner.
/// Observational getters cannot construct native grant IDs or edit rights.
pub(in crate::setup) struct SourceSelectedParent {
    exclusive: ExclusiveSelectedSource,
    root: Arc<HomeDirectory>,
    parent: Arc<HomeDirectory>,
    pin: Arc<dyn WorkspaceMetadataBackend>,
    lineage: LiveLineage,
    history_read: Mutex<Option<super::super::retained_history_read::RootHistoryReadCleanup>>,
    // Weak linkage to the sole authenticated filtered backend; no source/scope cycle.
    scope: Mutex<Option<Weak<Scope>>>,
    components: Vec<EntryName>,
    selected_name: EntryName,
    root_identity: [u64; 2],
    parent_identity: [u64; 2],
    selected_identity: [u64; 2],
}
impl SourceSelectedParent {
    fn walk(
        root: &Arc<HomeDirectory>,
        components: &[EntryName],
    ) -> Result<Arc<HomeDirectory>, String> {
        let mut current = root.clone();
        for name in components {
            let id = current
                .custody
                .call(|source| source.home()?.directory(current.id, name))
                .map_err(|e| e.to_string())?;
            current = Arc::new(HomeDirectory {
                custody: current.custody.clone(),
                id,
            });
        }
        Ok(current)
    }
    // Only this module's parent (the authentic source relay) can resolve it.
    pub(super) fn resolve(
        exclusive: ExclusiveSelectedSource,
        components: &[EntryName],
    ) -> Result<Arc<Self>, String> {
        let root = exclusive.root.clone();
        if root.id != ROOT_DIRECTORY || components.is_empty() || components.len() > 64 {
            return Err(denied());
        }
        let (leaf, parents) = components.split_last().ok_or_else(denied)?;
        let root_identity = root.metadata()?.identity;
        let parent = Self::walk(&root, parents)?;
        let parent_stat = parent.metadata()?;
        if parent_stat.metadata.kind != EntryKind::Directory {
            return Err(denied());
        }
        let selected = parent.stat(leaf)?.ok_or_else(denied)?;
        if selected.metadata.kind != EntryKind::File {
            return Err(denied());
        }
        let pin = parent.pin_metadata(leaf, selected.identity)?;
        let result = Arc::new(Self {
            exclusive,
            root,
            parent,
            lineage: LiveLineage::new(selected.identity, pin.clone()),
            history_read: Mutex::new(None),
            scope: Mutex::new(None),
            pin,
            components: components.to_vec(),
            selected_name: leaf.clone(),
            root_identity,
            parent_identity: parent_stat.identity,
            selected_identity: selected.identity,
        });
        result.check()?;
        Ok(result)
    }
    fn revoke(&self) {
        self.exclusive.root.custody.revoke();
    }
    pub(in crate::setup) fn uses_history_editor(&self, editor: &Arc<RootSelectedEditor>) -> bool {
        matches!(&self.root.custody.consumer, Consumer::SelectedEditor(actual) if Arc::ptr_eq(actual, editor))
    }
    pub(in crate::setup) fn check_history_subject(
        &self,
        observed: &runtime_fs::directory_backend::HistoryReadObservation,
    ) -> io::Result<()> {
        self.check().map_err(io::Error::other)?;
        observed.validate().map_err(io::Error::other)?;
        let parents: Vec<Vec<u8>> = self.components[..self.components.len() - 1]
            .iter()
            .map(|name| {
                use std::os::unix::ffi::OsStrExt;
                name.as_os_str().as_bytes().to_vec()
            })
            .collect();
        use std::os::unix::ffi::OsStrExt;
        if observed.root != self.root_identity
            || observed.parent != self.parent_identity
            || observed.parents != parents
            || observed.name != self.selected_name.as_os_str().as_bytes()
            || observed.identity != self.current_selected_identity().map_err(io::Error::other)?
        {
            return Err(super::refused());
        }
        self.check().map_err(io::Error::other)
    }
    fn query_selected_history(
        &self,
        name: &EntryName,
        expected: [u64; 2],
        query: &[u8],
    ) -> io::Result<Vec<u8>> {
        self.check().map_err(io::Error::other)?;
        if name != self.selected_name()
            || expected != self.current_selected_identity().map_err(io::Error::other)?
        {
            return Err(super::refused());
        }
        let reader = self
            .history_read
            .lock()
            .map_err(|_| super::refused())?
            .as_ref()
            .and_then(|owner| owner.session())
            .ok_or_else(super::refused)?;
        let subject = self
            .parent
            .history_read_subject(name)
            .map_err(io::Error::other)?;
        self.check_history_subject(&subject.observe().map_err(io::Error::other)?)?;
        let bytes = reader.query(subject, expected, query)?;
        self.check().map_err(io::Error::other)?;
        Ok(bytes)
    }
    pub(in crate::setup) fn retire_history_preceding(
        &self,
        proof: &super::super::retained_selected_edit::AcknowledgedSelectedReplacement,
    ) -> io::Result<()> {
        let owner = self.history_read.lock().map_err(|_| super::refused())?;
        match owner.as_ref() {
            Some(owner) => owner.retire_preceding(
                proof,
                &self.components[..self.components.len() - 1],
                &self.selected_name,
                self.parent_identity,
            ),
            None => Ok(()),
        }
    }
    pub(in crate::setup) fn adopt_history_acknowledged(
        &self,
        proof: &super::super::retained_selected_edit::AcknowledgedSelectedReplacement,
    ) -> io::Result<Option<Arc<super::super::retained_history_read::RootHistoryReadContinuity>>>
    {
        proof.check_custody(self).map_err(io::Error::other)?;
        let reader = self
            .history_read
            .lock()
            .map_err(|_| super::refused())?
            .as_ref()
            .and_then(|owner| owner.session());
        let Some(reader) = reader else {
            return Ok(None);
        };
        // Open only the actual selected leaf in the independently admitted
        // original-user source, never a sibling or a target/FUSE inode label.
        let subject = self
            .parent
            .history_read_subject(&self.selected_name)
            .map_err(io::Error::other)?;
        self.check_history_subject(&subject.observe().map_err(io::Error::other)?)?;
        let token = reader.adopt_acknowledged(subject, proof, self)?;
        token.check_acknowledged(proof, self)?;
        Ok(Some(token))
    }
    pub(in crate::setup) fn input(&self) -> &Arc<InstalledRetainedLaunchInput> {
        &self.root.custody.input
    }
    pub(in crate::setup) fn components(&self) -> &[EntryName] {
        &self.components
    }
    pub(in crate::setup) fn selected_name(&self) -> &EntryName {
        &self.selected_name
    }
    pub(in crate::setup) fn selected_identity(&self) -> [u64; 2] {
        self.selected_identity
    }
    pub(in crate::setup) fn parent_identity(&self) -> [u64; 2] {
        self.parent_identity
    }
    pub(in crate::setup) fn check(&self) -> Result<(), String> {
        self.observe_named_scope()?;
        self.observe_named_scope()
    }
    // Root pending checks MUST use this nonrecursive authority/provenance seam.
    // The original pin and original identities never become live-lineage DTOs.
    pub(in crate::setup) fn check_authority(&self) -> Result<(), String> {
        if self.root.metadata()?.identity != self.root_identity {
            return Err(denied());
        }
        let parents = &self.components[..self.components.len() - 1];
        let named_parent = Self::walk(&self.root, parents)?;
        if named_parent.metadata()?.identity != self.parent_identity
            || self.parent.metadata()?.identity != self.parent_identity
        {
            return Err(denied());
        }
        let held = self.pin.metadata()?;
        if held.identity != self.selected_identity || held.metadata.kind != EntryKind::File {
            return Err(denied());
        }
        self.root.check()
    }
    fn observe_named_scope(&self) -> Result<(), String> {
        self.check_authority()?;
        self.lineage.check(self)?;
        self.check_authority()
    }
    pub(in crate::setup) fn current_selected_identity(&self) -> Result<[u64; 2], String> {
        self.check()?;
        self.lineage.identity()
    }
    pub(in crate::setup) fn replacement_seed(
        &self,
    ) -> &Arc<super::super::retained_selected_edit::RootSelectedReplacementSeed> {
        &self.exclusive.replacement
    }
    /// Only the actual completed creator capability from this one filtered
    /// backend can reserve work. Caller names/observations never supply a backend.
    pub(in crate::setup) fn prepare_owned_replacement(
        self: &Arc<Self>,
        temporary: &EntryName,
    ) -> Result<Arc<dyn runtime_fs::directory_backend::WorkspacePendingReplacementBackend>, String>
    {
        let _ = self.prune_completed_handoffs();
        let scope = self
            .scope
            .lock()
            .map_err(|_| denied())?
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or_else(denied)?;
        if !Arc::ptr_eq(&scope.source, self) {
            return Err(denied());
        }
        scope.call(|| {
            self.lineage.check_ready_to_reserve()?;
            {
                let owner = scope.owner.lock().map_err(|_| denied())?;
                owner.check_current().map_err(|e| e.to_string())?;
                if owner.handoff_reservations.len() >= MAX_REPLACEMENT_HANDOFFS {
                    return Err("Selected replacement session budget exhausted".into());
                }
            }
            if temporary == self.selected_name() {
                return Err(denied());
            }
            let expected = scope.identity(temporary)?;
            let victim = self.lineage.identity()?;
            let mut armed = scope.armed.lock().map_err(|_| denied())?;
            let save = armed
                .get_mut(temporary)
                .ok_or_else(|| "AtomicSave not armed".to_string())?;
            if save.expected != expected || save.victim != victim {
                return Err(denied());
            }
            let pending = save.reservation.prepared()?.clone();
            pending.check()?;
            let observed = pending.observe()?;
            observed.validate()?;
            if observed.source != expected || observed.expected_victim != victim {
                return Err(denied());
            }
            let actual = self.parent.stat(temporary)?.ok_or_else(denied)?;
            if actual.identity != expected || actual.metadata.kind != EntryKind::File {
                return Err(denied());
            }
            save.reservation.claim()
        })
        // Scope's callback permit is dropped BEFORE the root installs its
        // exclusive pending/drain gate and permits the daemon to start.
    }
    /// Forward only through this source allocation's actual supervised worker.
    /// The root caller holds the already-installed/drained private pending gate.
    pub(in crate::setup) fn connect_pending_handoff(
        self: &Arc<Self>,
        pending: &Arc<super::super::retained_selected_edit::PendingSelectedReplacement>,
    ) -> io::Result<super::super::retained_source_worker::PendingHandoff> {
        self.check_authority().map_err(io::Error::other)?;
        pending.check_custody(self).map_err(io::Error::other)?;
        self.lineage
            .check_drained_pending(pending)
            .map_err(io::Error::other)?;
        let scope = self
            .scope
            .lock()
            .map_err(|_| super::refused())?
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or_else(super::refused)?;
        let mut owner = scope.owner.lock().map_err(|_| super::refused())?;
        if !Arc::ptr_eq(&scope.source, self)
            || !owner.owns_source(self)
            || !owner.uses_editor(scope.admission.editor())
        {
            return Err(super::refused());
        }
        owner.check_current()?;
        let operation = pending.observation().operation;
        if owner.handoff_reservations.len() >= MAX_REPLACEMENT_HANDOFFS
            || !owner.handoff_reservations.insert(operation)
        {
            return Err(io::Error::other(
                "Selected replacement handoff budget unavailable",
            ));
        }
        let handoff = owner
            .worker
            .as_ref()
            .ok_or_else(super::refused)?
            .connect_pending_handoff(pending.backend().clone())?;
        owner.check_current()?;
        drop(owner);
        self.check_authority().map_err(io::Error::other)?;
        pending.check_custody(self).map_err(io::Error::other)?;
        self.lineage
            .check_drained_pending(pending)
            .map_err(io::Error::other)?;
        Ok(handoff)
    }
    /// Completed streams stay at the actual source owner so a later Save can
    /// revalidate prior admitted executions. Failure returns the exact owner;
    /// the root job remains unresolved and can never lose the live stream.
    pub(in crate::setup) fn retain_completed_handoff(
        self: &Arc<Self>,
        handoff: PendingHandoff,
        proof: Arc<AcknowledgedSelectedReplacement>,
        continuity: Option<Arc<RootHistoryReadContinuity>>,
        finished: ObservedReplacementFinished,
    ) -> Result<(), Box<PendingHandoff>> {
        let scope = match self.scope.lock() {
            Ok(registered) => registered.as_ref().and_then(Weak::upgrade),
            Err(_) => None,
        };
        let Some(scope) = scope else {
            return Err(Box::new(handoff));
        };
        if proof.check_custody(self).is_err()
            || finished
                .check_operation(proof.observation().operation)
                .is_err()
            || !Arc::ptr_eq(handoff.pending(), proof.pending().backend())
        {
            return Err(Box::new(handoff));
        }
        if let Some(read) = &continuity {
            if read.check_acknowledged(&proof, self).is_err() {
                return Err(Box::new(handoff));
            }
        }
        let Ok(mut owner) = scope.owner.lock() else {
            return Err(Box::new(handoff));
        };
        let operation = proof.observation().operation;
        if !Arc::ptr_eq(&scope.source, self)
            || !owner.owns_source(self)
            || !owner.uses_editor(scope.admission.editor())
            || owner.check_current().is_err()
            || handoff.check().is_err()
            || !owner.handoff_reservations.contains(&operation)
            || owner.completed_handoffs.len() >= MAX_REPLACEMENT_HANDOFFS
            || owner
                .completed_handoffs
                .iter()
                .any(|entry| entry.proof.observation().operation == operation)
        {
            return Err(Box::new(handoff));
        }
        owner
            .completed_handoffs
            .push(Arc::new(CompletedReplacement {
                handoff,
                proof,
                continuity,
                finished,
                retirement: Mutex::new(CompletedRetirement::default()),
            }));
        Ok(())
    }
    /// Keep the newest actual ACK. Older custody is released only after daemon
    /// signed ancestry admission, exact source RPC join and fresh bound Read.
    pub(in crate::setup) fn prune_completed_handoffs(self: &Arc<Self>) -> Result<(), String> {
        let scope = self
            .scope
            .lock()
            .map_err(|_| denied())?
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or_else(denied)?;
        let (latest, older) = {
            let owner = scope.owner.lock().map_err(|_| denied())?;
            if !Arc::ptr_eq(&scope.source, self) || !owner.owns_source(self) {
                return Err(denied());
            }
            owner.check_current().map_err(|e| e.to_string())?;
            if owner.completed_handoffs.len() < 2 {
                return Ok(());
            }
            let mut matching = owner.completed_handoffs.iter().filter_map(|entry| {
                match self.lineage.matches_acknowledged(&entry.proof) {
                    Ok(true) => Some(Ok(entry.clone())),
                    Ok(false) => None,
                    Err(error) => Some(Err(error)),
                }
            });
            let latest = matching
                .next()
                .ok_or_else(|| "Current acknowledged Save not retained".to_string())??;
            if let Some(result) = matching.next() {
                result?;
                return Err("Ambiguous current acknowledged Save".into());
            }
            let older = owner
                .completed_handoffs
                .iter()
                .filter(|row| !Arc::ptr_eq(row, &latest))
                .cloned()
                .collect::<Vec<_>>();
            (latest, older)
        };
        if older.is_empty() {
            return Ok(());
        }
        let read = latest
            .continuity
            .as_ref()
            .ok_or_else(|| "Current Save Read continuity unavailable".to_string())?;
        let check_latest = || {
            latest
                .finished
                .check_operation(latest.proof.observation().operation)?;
            latest.proof.check_custody(self)?;
            if !self.lineage.matches_acknowledged(&latest.proof)?
                || self.current_selected_identity()? != latest.proof.observation().destination
            {
                return Err(denied());
            }
            read.check_acknowledged(&latest.proof, self)
                .map_err(|e| e.to_string())
        };
        for entry in older {
            check_latest()?;
            entry
                .finished
                .check_operation(entry.proof.observation().operation)?;
            let mut progress = entry.retirement.lock().map_err(|_| denied())?;
            if !progress.daemon_retired {
                entry.handoff.check().map_err(|e| e.to_string())?;
                entry
                    .proof
                    .retire_before(&latest.proof, self)
                    .map_err(|e| e.to_string())?;
                progress.daemon_retired = true;
            }
            check_latest()?;
            if !progress.rpc_retired {
                entry.handoff.retire_rpc().map_err(|e| e.to_string())?;
                progress.rpc_retired = true;
            }
            check_latest()?;
            if !progress.read_retired {
                // This owner-backed cleanup ACK covers even a failed Reader
                // adoption which returned no token. None itself proves nothing.
                self.replacement_seed()
                    .retire_history_preceding(&entry.proof, read, &latest.proof, self)
                    .map_err(|error| error.to_string())?;
                check_latest()?;
                if let Some(older_read) = &entry.continuity {
                    older_read.retire().map_err(|error| error.to_string())?;
                }
                progress.read_retired = true;
            }
            check_latest()?;
            if !progress.pin_retired {
                entry.proof.retire_pin_controller_cache()?;
                progress.pin_retired = true;
            }
            check_latest()?;
            if !progress.job_retired {
                entry.finished.retire_controller_cache()?;
                progress.job_retired = true;
            }
            check_latest()?;
            if !progress.pending_retired {
                entry.proof.retire_pending_controller_cache()?;
                progress.pending_retired = true;
            }
            check_latest()?;
            // No old proof/current/observe invocation after any of its own
            // caches retire. Its inseparable native Finished remains descriptive
            // evidence; only the independently current successor is rechecked.
            if !progress.observed() {
                return Err(denied());
            }
            let mut owner = scope.owner.lock().map_err(|_| denied())?;
            owner.check_current().map_err(|e| e.to_string())?;
            if !self.lineage.matches_acknowledged(&latest.proof)? {
                return Err("Save successor changed during retirement".into());
            }
            // The private typed lineage chooses the successor; queue arrival
            // order never supplies authority. Always retain that exact ACK.
            remove_older_completed(&mut owner.completed_handoffs, &latest, &entry, &progress)?;
            owner
                .handoff_reservations
                .remove(&entry.proof.observation().operation);
        }
        Ok(())
    }
    /// Release committed-awaiting-transfer only after this actual completed
    /// queue owns the same ACK/native Finished and root registry transfer has
    /// independently observed removal of the same live operation slot.
    pub(in crate::setup) fn finish_completed_transfer(
        self: &Arc<Self>,
        proof: &AcknowledgedSelectedReplacement,
    ) -> Result<(), String> {
        self.check_authority()?;
        proof.check_custody(self)?;
        let scope = self
            .scope
            .lock()
            .map_err(|_| denied())?
            .as_ref()
            .and_then(Weak::upgrade)
            .ok_or_else(denied)?;
        let completed = {
            let owner = scope.owner.lock().map_err(|_| denied())?;
            if !Arc::ptr_eq(&scope.source, self) || !owner.owns_source(self) {
                return Err(denied());
            }
            owner.check_current().map_err(|e| e.to_string())?;
            owner
                .completed_handoffs
                .iter()
                .find(|row| {
                    std::ptr::eq(Arc::as_ptr(&row.proof), proof)
                        && Arc::ptr_eq(row.proof.pending(), proof.pending())
                })
                .cloned()
                .ok_or_else(|| "Completed replacement ownership not transferred".to_string())?
        };
        completed
            .finished
            .check_operation(proof.observation().operation)?;
        self.replacement_seed()
            .check_completed_registry_transfer(proof)?;
        self.lineage.finish_completed_transfer(self, proof)
    }
    pub(in crate::setup) fn begin_pending(
        self: &Arc<Self>,
        pending: Arc<super::super::retained_selected_edit::PendingSelectedReplacement>,
    ) -> Result<(), String> {
        self.lineage.begin(self, pending)
    }
    pub(in crate::setup) fn commit_lineage(
        &self,
        proof: &super::super::retained_selected_edit::AcknowledgedSelectedReplacement,
    ) -> Result<(), String> {
        self.lineage.commit(self, proof)
    }
}

// Pending includes unknown-effect outcomes. Dropping a client job cannot free
// its reservation and accidentally admit another 64 native effects.
enum Temp {
    Pending,
    Ready([u64; 2]),
}
// Installed before native prepare. An uncertain prepare retains its finite
// slot; a consumed arm stays closed to writes until actual lineage adoption.
struct ArmedSave {
    expected: [u64; 2],
    victim: [u64; 2],
    reservation: runtime_fs::directory_backend::AtomicSaveReservation<
        Arc<dyn runtime_fs::directory_backend::WorkspacePendingReplacementBackend>,
    >,
}
struct Scope {
    owner: Arc<Mutex<RootSelectedEditSource>>,
    source: Arc<SourceSelectedParent>,
    admission: Arc<RootSelectedEditAdmission>,
    temps: Mutex<HashMap<EntryName, Temp>>,
    created: Mutex<
        HashMap<
            EntryName,
            Arc<dyn runtime_fs::directory_backend::WorkspaceCreatedTemporaryBackend>,
        >,
    >,
    armed: Mutex<HashMap<EntryName, ArmedSave>>,
    revoked: AtomicBool,
    cursors: AtomicUsize,
}
impl Scope {
    fn check(&self) -> Result<(), String> {
        if self.revoked.load(Ordering::Acquire) {
            return Err(denied());
        }
        match self.current() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.revoked.store(true, Ordering::Release);
                // Exclusive editor connection only. Channel death is the
                // source session's native effect-gate lifetime witness.
                self.source.revoke();
                Err(error)
            }
        }
    }
    fn owner_current(&self) -> Result<(), String> {
        let owner = self.owner.lock().map_err(|_| denied())?;
        if !owner.uses_editor(self.admission.editor()) || !owner.owns_source(&self.source) {
            return Err(denied());
        }
        owner.check_current().map_err(|e| e.to_string())
    }
    fn current(&self) -> Result<(), String> {
        // Release this mutex before admission.check, which may consult the
        // same typed owner; no callback executes while holding its lock.
        self.owner_current()?;
        self.admission.check().map_err(|e| e.to_string())?;
        self.source.check()?;
        if !self.admission.uses_source(&self.source)
            || self.admission.selected_name() != self.source.selected_name()
            || self.admission.selected_identity() != self.source.selected_identity()
            || self.admission.parent_identity() != self.source.parent_identity()
        {
            return Err(denied());
        }
        if !self.admission.uses_input(&self.source.parent.custody.input) {
            return Err(denied());
        }
        let stat = self.source.parent.metadata()?;
        if stat.identity != self.admission.parent_identity()
            || stat.metadata.kind != EntryKind::Directory
        {
            return Err(denied());
        }
        self.admission.check().map_err(|e| e.to_string())?;
        self.owner_current()
    }
    fn call<T>(&self, run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.check()?;
        // Acquisition refusal during authentic pending work is transient and
        // never enters check()'s terminal authority-revocation path.
        let _operation = self.source.lineage.operation()?;
        self.check()?;
        let result = run();
        self.check()?;
        result
    }
    fn identity(&self, name: &EntryName) -> Result<[u64; 2], String> {
        if name == self.admission.selected_name() {
            return self.source.lineage.identity();
        }
        match self.temps.lock().map_err(|_| denied())?.get(name) {
            Some(Temp::Ready(identity)) => Ok(*identity),
            _ => Err(denied()),
        }
    }
    fn stat(&self, name: &EntryName) -> Result<Option<DirectoryStat>, String> {
        self.call(|| {
            let expected = self.identity(name)?;
            let stat = self.source.parent.stat(name)?;
            if stat
                .as_ref()
                .is_some_and(|s| s.identity != expected || s.metadata.kind != EntryKind::File)
            {
                return Err(denied());
            }
            Ok(stat)
        })
    }
}

pub(super) struct SelectedParent(Arc<Scope>);
impl SelectedParent {
    // Native lifetime cancellation needs no owner mutex, including when an
    // in-flight callback is waiting inside a genuine source operation.
    pub(super) fn revoke(&self) {
        self.0.revoked.store(true, Ordering::Release);
        self.0.source.revoke();
    }
    // This constructor is accessible only to the genuine source relay owner.
    // Arc<Input> alone is deliberately insufficient to create this backend.
    pub(super) fn new(
        source: Arc<SourceSelectedParent>,
        admission: Arc<RootSelectedEditAdmission>,
        owner: Arc<Mutex<RootSelectedEditSource>>,
    ) -> Result<Self, String> {
        let scope = Arc::new(Scope {
            owner,
            source,
            admission,
            temps: Mutex::new(HashMap::new()),
            created: Mutex::new(HashMap::new()),
            armed: Mutex::new(HashMap::new()),
            revoked: AtomicBool::new(false),
            cursors: AtomicUsize::new(0),
        });
        scope.check()?;
        let selected = scope
            .stat(scope.admission.selected_name())?
            .ok_or_else(denied)?;
        if selected.identity != scope.admission.selected_identity() {
            return Err(denied());
        }
        // Register exactly once. A second backend cannot import a different
        // temp registry into the same admitted source allocation.
        let mut registered = scope.source.scope.lock().map_err(|_| denied())?;
        if registered.is_some() {
            return Err(denied());
        }
        *registered = Some(Arc::downgrade(&scope));
        drop(registered);
        Ok(Self(scope))
    }
}
impl WorkspaceDirectoryBackend for SelectedParent {
    fn history_query(
        &self,
        name: &EntryName,
        expected: [u64; 2],
        query: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.0.call(|| {
            self.0
                .source
                .query_selected_history(name, expected, query)
                .map_err(|error| error.to_string())
        })
    }
    fn atomic_save(
        &self,
        temporary: &EntryName,
        expected: [u64; 2],
        target: &EntryName,
    ) -> Result<(), String> {
        let _ = self.0.source.prune_completed_handoffs();
        self.0.call(|| {
            self.0.source.lineage.check_ready_to_reserve()?;
            if temporary == target
                || target != self.0.source.selected_name()
                || self.0.identity(temporary)? != expected
            {
                return Err(denied());
            }
            {
                let owner = self.0.owner.lock().map_err(|_| denied())?;
                owner.check_current().map_err(|e| e.to_string())?;
                if owner.handoff_reservations.len() >= MAX_REPLACEMENT_HANDOFFS {
                    return Err("Selected replacement session budget exhausted".into());
                }
            }
            let victim = self.0.source.lineage.identity()?;
            // Serialize against every temp write/open/chmod. Existing callbacks
            // drain before prepare, and all subsequent byte effects refuse.
            let mut armed = self.0.armed.lock().map_err(|_| denied())?;
            if armed.contains_key(temporary) || armed.len() >= MAX_TEMPS {
                return Err(denied());
            }
            let cap = self
                .0
                .created
                .lock()
                .map_err(|_| denied())?
                .get(temporary)
                .cloned()
                .ok_or_else(denied)?;
            cap.check()?;
            let before = self.0.source.parent.stat(temporary)?.ok_or_else(denied)?;
            if before.identity != expected || before.metadata.kind != EntryKind::File {
                return Err(denied());
            }
            armed.insert(
                temporary.clone(),
                ArmedSave {
                    expected,
                    victim,
                    reservation: runtime_fs::directory_backend::AtomicSaveReservation::default(),
                },
            );
            // Actual CompletedCreate native AtomicSave purpose; neither a caller
            // marker nor an unmarked POSIX rename manufactures Save intent.
            let reservation = &mut armed.get_mut(temporary).ok_or_else(denied)?.reservation;
            reservation.capture(cap.prepare_atomic_save())?;
            // The actual owner is stored before every fallible check/observe/stat.
            reservation.validate(|pending| {
                pending.check()?;
                let observed = pending.observe()?;
                observed.validate()?;
                let after = self.0.source.parent.stat(temporary)?.ok_or_else(denied)?;
                if observed.source != expected
                    || observed.expected_victim != victim
                    || after.identity != expected
                    || after.metadata.kind != EntryKind::File
                {
                    return Err(denied());
                }
                cap.check()
            })?;
            Ok(())
        })
    }
    fn set_created_permissions(
        &self,
        name: &EntryName,
        expected: [u64; 2],
        mode: u32,
    ) -> Result<(), String> {
        self.0.call(|| {
            let armed = self.0.armed.lock().map_err(|_| denied())?;
            if armed.contains_key(name) {
                return Err(denied());
            }
            if mode & !0o7777 != 0
                || name == self.0.admission.selected_name()
                || self.0.identity(name)? != expected
            {
                return Err(denied());
            }
            let cap = self
                .0
                .created
                .lock()
                .map_err(|_| denied())?
                .get(name)
                .cloned()
                .ok_or_else(denied)?;
            cap.set_permissions(mode)
        })
    }

    fn sync_directory(&self) -> Result<(), String> {
        self.0.call(|| self.0.source.parent.sync_directory())
    }
    fn retire_created(
        &self,
        name: &EntryName,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        self.0.call(|| {
            if name == self.0.admission.selected_name() || self.0.identity(name)? != expected {
                return Err(denied());
            }
            let cap = self
                .0
                .created
                .lock()
                .map_err(|_| denied())?
                .get(name)
                .cloned()
                .ok_or_else(denied)?;
            cap.check()?;
            let mut armed = self.0.armed.lock().map_err(|_| denied())?;
            if let Some(save) = armed.get_mut(name) {
                save.reservation.release_with(|pending| pending.cancel())?;
            }
            // Retain the finite slot on unknown effects; native custody is one use.
            let inner = cap.retire()?;
            if let Some(save) = armed.get_mut(name) {
                save.reservation.cleanup_submitted()?;
            }
            Ok(Arc::new(CleanupJob {
                scope: self.0.clone(),
                name: name.clone(),
                expected,
                cap,
                inner,
                state: Mutex::new(CleanupCompletion::default()),
            }) as Arc<dyn WorkspaceJobBackend>)
        })
    }

    fn statfs(&self) -> Result<runtime_fs::directory_backend::DirectoryFilesystemStats, String> {
        self.0.call(|| self.0.source.parent.statfs())
    }
    fn readlink(&self, _: &EntryName, _: [u64; 2]) -> Result<Vec<u8>, String> {
        self.check()?;
        Err("Selected-parent scope admits only regular selected and owned files".into())
    }
    fn open_cursor(&self) -> Result<Arc<dyn WorkspaceDirectoryCursorBackend>, String> {
        self.0.call(|| {
            self.0
                .cursors
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                    (count < MAX_CURSORS).then_some(count + 1)
                })
                .map_err(|_| "Selected-parent cursor bound reached".to_string())?;
            let permit = CursorPermit(self.0.clone());
            let mut names = vec![self.0.admission.selected_name().clone()];
            names.extend(
                self.0
                    .temps
                    .lock()
                    .map_err(|_| denied())?
                    .iter()
                    .filter_map(|(name, state)| {
                        matches!(state, Temp::Ready(_)).then_some(name.clone())
                    }),
            );
            Ok(Arc::new(FilteredCursor {
                permit,
                state: Mutex::new(CursorState {
                    names,
                    next: 0,
                    done: false,
                }),
            }) as Arc<dyn WorkspaceDirectoryCursorBackend>)
        })
    }
    fn check(&self) -> Result<(), String> {
        self.0.check()
    }
    fn metadata(&self) -> Result<DirectoryStat, String> {
        self.0.call(|| self.0.source.parent.metadata())
    }
    fn stat(&self, name: &EntryName) -> Result<Option<DirectoryStat>, String> {
        self.0.stat(name)
    }
    fn pin_metadata(
        &self,
        name: &EntryName,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceMetadataBackend>, String> {
        self.0.call(|| {
            if self.0.identity(name)? != expected {
                return Err(denied());
            }
            Ok(Arc::new(MetadataPin {
                scope: self.0.clone(),
                inner: self.0.source.parent.pin_metadata(name, expected)?,
                expected,
            }) as Arc<dyn WorkspaceMetadataBackend>)
        })
    }
    fn list(&self, cursor: u64) -> Result<DirectoryRows, String> {
        self.0.call(|| {
            if cursor != 0 {
                return Err(denied());
            }
            let mut names = vec![self.0.admission.selected_name().clone()];
            names.extend(
                self.0
                    .temps
                    .lock()
                    .map_err(|_| denied())?
                    .iter()
                    .filter_map(|(name, state)| {
                        matches!(state, Temp::Ready(_)).then_some(name.clone())
                    }),
            );
            let mut entries = Vec::with_capacity(names.len());
            for name in names {
                if let Some(stat) = self.0.stat(&name)? {
                    entries.push(DirectoryRow {
                        name,
                        metadata: stat.metadata,
                    });
                }
            }
            Ok(DirectoryRows { entries, next: 0 })
        })
    }
    fn directory(&self, _: &EntryName) -> Result<Arc<dyn WorkspaceDirectoryBackend>, String> {
        self.check()?;
        Err(denied())
    }
    fn open_file(
        &self,
        name: &EntryName,
        mode: OpenMode,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceFileBackend>, String> {
        self.0.call(|| {
            let armed = self.0.armed.lock().map_err(|_| denied())?;
            if armed.contains_key(name) && !matches!(mode, OpenMode::Read) {
                return Err(denied());
            }
            if self.0.identity(name)? != expected {
                return Err(denied());
            }
            // Write opens truncate before a file callback can refuse. The
            // selected victim is only probed without changing its bytes;
            // replacement effects belong to a genuine created temporary.
            let created = if name == self.0.admission.selected_name() {
                if matches!(mode, OpenMode::Write | OpenMode::Append) {
                    return Err(denied());
                }
                None
            } else {
                let cap = self
                    .0
                    .created
                    .lock()
                    .map_err(|_| denied())?
                    .get(name)
                    .cloned()
                    .ok_or_else(denied)?;
                cap.check()?;
                Some((name.clone(), cap))
            };
            let inner = self.0.source.parent.open_file(name, mode, expected)?;
            if inner.native_identity()? != Some(expected) {
                return Err(denied());
            }
            Ok(Arc::new(File {
                scope: self.0.clone(),
                inner,
                expected,
                created,
            }) as Arc<dyn WorkspaceFileBackend>)
        })
    }
    fn create_file(&self, name: EntryName) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let inner = self.0.call(|| {
            // The typed committed lineage proves which previously owned temp
            // became selected. Reclaim only that exact Ready identity, never an
            // uncertain reservation or an arbitrary disappeared sibling.
            let selected = self.0.source.lineage.identity()?;
            let consumed: Vec<_> = self
                .0
                .temps
                .lock()
                .map_err(|_| denied())?
                .iter()
                .filter_map(|(name, temp)| {
                    matches!(temp,Temp::Ready(identity) if *identity==selected)
                        .then_some(name.clone())
                })
                .collect();
            for consumed in consumed {
                self.0.armed.lock().map_err(|_| denied())?.remove(&consumed);
                self.0
                    .created
                    .lock()
                    .map_err(|_| denied())?
                    .remove(&consumed);
                let mut temps = self.0.temps.lock().map_err(|_| denied())?;
                if matches!(temps.get(&consumed),Some(Temp::Ready(identity)) if *identity==selected)
                {
                    temps.remove(&consumed);
                }
            }
            {
                let mut temps = self.0.temps.lock().map_err(|_| denied())?;
                if name == *self.0.admission.selected_name()
                    || temps.contains_key(&name)
                    || temps.len() >= MAX_TEMPS
                {
                    return Err(denied());
                }
                temps.insert(name.clone(), Temp::Pending);
            }
            // Transport uncertainty retains the reservation. A transient gate
            // refusal before this closure allocates no reservation or effect.
            self.0.source.parent.create_file(name.clone())
        })?;
        Ok(Arc::new(CreateJob {
            scope: self.0.clone(),
            name,
            inner,
            observed: Mutex::new(Completion::default()),
        }))
    }
    fn create_directory(&self, _: EntryName) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        self.check()?;
        Err(denied())
    }
    fn rename_replace(
        &self,
        name: &EntryName,
        new_name: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        self.0.check()?;
        if name == self.0.source.selected_name() || new_name != *self.0.source.selected_name() {
            return Err(denied());
        }
        // The seed's private factory reacquires actual own CompletedCreate
        // custody and current named lineage. Never hold a callback permit while
        // begin_pending closes/drains that same operation gate.
        self.0
            .source
            .replacement_seed()
            .start_owned_replacement(&self.0.source, name)
    }
    fn rename_no_replace(
        &self,
        _: &EntryName,
        _: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        self.check()?;
        Err("Selected-parent replacement requires canonical history admission".into())
    }
}
// The bounded name set belongs to this admitted selected/own-created scope;
// it is not a snapshot or cursor over siblings in the original Home directory.
struct CursorPermit(Arc<Scope>);
impl Drop for CursorPermit {
    fn drop(&mut self) {
        self.0.cursors.fetch_sub(1, Ordering::AcqRel);
    }
}
struct CursorState {
    names: Vec<EntryName>,
    next: u64,
    done: bool,
}
struct FilteredCursor {
    permit: CursorPermit,
    state: Mutex<CursorState>,
}
impl WorkspaceDirectoryCursorBackend for FilteredCursor {
    fn check(&self) -> Result<(), String> {
        self.permit.0.check()
    }
    fn page(&self, expected: u64) -> Result<DirectoryStreamPage, String> {
        let scope = &self.permit.0;
        scope.call(|| {
            let mut state = self.state.lock().map_err(|_| denied())?;
            if expected != state.next || state.done {
                return Err("Selected-parent cursor sequence refused".into());
            }
            let next = state.next.checked_add(1).ok_or_else(denied)?;
            let mut entries = Vec::with_capacity(state.names.len());
            for name in &state.names {
                let expected = scope.identity(name)?;
                let observed = scope.source.parent.stat(name);
                // Source/authority errors are checked independently before
                // representing a still-live per-entry metadata refusal.
                scope.check()?;
                match observed {
                    Ok(None) => {} // Live owned name disappeared; no sibling fallback.
                    Ok(Some(stat))
                        if stat.identity == expected && stat.metadata.kind == EntryKind::File =>
                    {
                        entries.push(DirectoryStreamRow {
                            name: name.clone(),
                            stat: Some(stat),
                        })
                    }
                    Ok(Some(_)) | Err(_) => entries.push(DirectoryStreamRow {
                        name: name.clone(),
                        stat: None,
                    }),
                }
            }
            state.next = next;
            state.done = true;
            Ok(DirectoryStreamPage {
                entries,
                next_sequence: next,
                done: true,
            })
        })
    }
}

struct MetadataPin {
    scope: Arc<Scope>,
    inner: Arc<dyn WorkspaceMetadataBackend>,
    expected: [u64; 2],
}
impl WorkspaceMetadataBackend for MetadataPin {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(self.scope.check())
    }
    fn metadata(&self) -> Result<DirectoryStat, String> {
        self.scope.call(|| {
            let stat = self.inner.metadata()?;
            if stat.identity != self.expected {
                return Err(denied());
            }
            Ok(stat)
        })
    }
}
struct File {
    scope: Arc<Scope>,
    inner: Arc<dyn WorkspaceFileBackend>,
    expected: [u64; 2],
    // None is the selected victim, never a byte-mutation authorization.
    // Some retains custody minted by the actual completed CreateFile job.
    created: Option<(
        EntryName,
        Arc<dyn runtime_fs::directory_backend::WorkspaceCreatedTemporaryBackend>,
    )>,
}
impl File {
    fn call<T>(&self, run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        self.scope.call(|| {
            if self.inner.native_identity()? != Some(self.expected) {
                return Err(denied());
            }
            run()
        })
    }
}
impl File {
    fn mutate<T>(&self, run: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let (name, cap) = self.created.as_ref().ok_or_else(denied)?;
        self.call(|| {
            let armed = self.scope.armed.lock().map_err(|_| denied())?;
            // A retained file descriptor, mode or scalar inode is insufficient:
            // require the same still-owned creator capability and Ready slot.
            let current = self
                .scope
                .created
                .lock()
                .map_err(|_| denied())?
                .get(name)
                .cloned()
                .ok_or_else(denied)?;
            if name == self.scope.admission.selected_name()
                || armed.contains_key(name)
                || !Arc::ptr_eq(cap, &current)
                || self.scope.identity(name)? != self.expected
            {
                return Err(denied());
            }
            cap.check()?;
            run()
        })
    }
}
impl WorkspaceFileBackend for File {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(self.scope.check())
    }
    fn native_identity(&self) -> Result<Option<[u64; 2]>, String> {
        self.call(|| self.inner.native_identity())
    }
    fn read(&self, offset: u64, len: u32) -> Result<Vec<u8>, String> {
        self.call(|| self.inner.read(offset, len))
    }
    fn write(&self, offset: u64, bytes: &[u8]) -> Result<u32, String> {
        self.mutate(|| self.inner.write(offset, bytes))
    }
    fn append(&self, bytes: &[u8]) -> Result<u32, String> {
        self.mutate(|| self.inner.append(bytes))
    }
    fn flush(&self) -> Result<(), String> {
        self.call(|| self.inner.flush())
    }
    fn metadata(&self) -> Result<Metadata, String> {
        self.call(|| self.inner.metadata())
    }
}
#[derive(Default)]
struct Completion {
    operation: Option<[u8; 16]>,
    item: Option<Result<[u64; 2], bool>>,
    finished: bool,
    rejected: bool,
}
impl Completion {
    fn consume(&mut self, poll: &JobPoll) -> Result<Option<[u64; 2]>, String> {
        if self.rejected {
            return Err(denied());
        }
        let result = self.consume_inner(poll);
        if result.is_err() {
            self.rejected = true;
        }
        result
    }
    fn consume_inner(&mut self, poll: &JobPoll) -> Result<Option<[u64; 2]>, String> {
        if poll.operation_id == [0; 16] || self.operation.is_some_and(|id| id != poll.operation_id)
        {
            return Err(denied());
        }
        self.operation = Some(poll.operation_id);
        for update in &poll.updates {
            match update {
                JobUpdate::Progress { item: 0, .. } if !self.finished => {}
                JobUpdate::Item {
                    item: 0,
                    state: ItemState::Completed,
                    destination: Some(identity),
                    error: None,
                    recoveries,
                } if self.item.is_none()
                    && !self.finished
                    && recoveries.is_empty()
                    && identity[0] != 0
                    && identity[1] != 0 =>
                {
                    self.item = Some(Ok(*identity));
                }
                // Forward real failures without admitting the resulting name.
                // Uncertain effects retain their finite reservation.
                JobUpdate::Item { item: 0, state, .. }
                    if self.item.is_none()
                        && !self.finished
                        && !matches!(state, ItemState::Completed) =>
                {
                    self.item = Some(Err(matches!(state, ItemState::Cancelled)));
                }
                JobUpdate::Finished {
                    completed,
                    failed,
                    cancelled,
                } if !self.finished => {
                    let expected = match self.item {
                        Some(Ok(_)) => (1, 0, 0),
                        Some(Err(false)) => (0, 1, 0),
                        Some(Err(true)) => (0, 0, 1),
                        None => return Err(denied()),
                    };
                    if (*completed, *failed, *cancelled) != expected {
                        return Err(denied());
                    }
                    self.finished = true;
                }
                _ => return Err(denied()),
            }
        }
        Ok(if self.finished {
            self.item.and_then(Result::ok)
        } else {
            None
        })
    }
}
struct CreateJob {
    scope: Arc<Scope>,
    name: EntryName,
    inner: Arc<dyn WorkspaceJobBackend>,
    observed: Mutex<Completion>,
}
impl WorkspaceJobBackend for CreateJob {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(self.scope.check())
    }
    fn poll(&self) -> Result<JobPoll, String> {
        // Serialize before draining the genuine job, so concurrent polls cannot
        // observe Finished ahead of the Item consumed by another callback.
        let mut state = self.observed.lock().map_err(|_| denied())?;
        self.scope.call(|| {
            let poll = self.inner.poll()?;
            if let Some(expected) = state.consume(&poll)? {
                let actual = self
                    .scope
                    .source
                    .parent
                    .stat(&self.name)?
                    .ok_or_else(denied)?;
                if actual.identity != expected || actual.metadata.kind != EntryKind::File {
                    state.rejected = true;
                    return Err(denied());
                }
                // Only actual completed creator custody is recorded. DTO poll
                // observations alone never mint an owned temporary capability.
                let mut created = self.scope.created.lock().map_err(|_| denied())?;
                if !created.contains_key(&self.name) {
                    let cap = self.inner.created_temporary()?;
                    cap.check()?;
                    created.insert(self.name.clone(), cap);
                }
                drop(created);
                let mut temps = self.scope.temps.lock().map_err(|_| denied())?;
                match temps.get(&self.name) {
                    Some(Temp::Pending) => {
                        temps.insert(self.name.clone(), Temp::Ready(expected));
                    }
                    Some(Temp::Ready(identity)) if *identity == expected => {}
                    _ => {
                        state.rejected = true;
                        return Err(denied());
                    }
                }
            }
            Ok(poll)
        })
    }
    fn cancel(&self) -> Result<(), String> {
        self.scope.call(|| self.inner.cancel())
    }
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    fn poll(updates: Vec<JobUpdate>) -> JobPoll {
        JobPoll {
            operation_id: [7; 16],
            updates,
        }
    }
    fn item() -> JobUpdate {
        JobUpdate::Item {
            item: 0,
            state: ItemState::Completed,
            destination: Some([8, 9]),
            error: None,
            recoveries: vec![],
        }
    }
    fn finished() -> JobUpdate {
        JobUpdate::Finished {
            completed: 1,
            failed: 0,
            cancelled: 0,
        }
    }
    // Supplemental production receipt-parser tests. These deliberately do not
    // fabricate a RootSelectedEditAdmission or claim positive native custody.
    #[test]
    fn receipt_requires_both_actual_item_and_finished() {
        let mut state = Completion::default();
        assert_eq!(state.consume(&poll(vec![item()])).unwrap(), None);
        assert_eq!(
            state.consume(&poll(vec![finished()])).unwrap(),
            Some([8, 9])
        );
        assert_eq!(state.consume(&poll(vec![])).unwrap(), Some([8, 9]));
    }
    #[test]
    fn malformed_order_or_changed_operation_latches_refusal() {
        let mut state = Completion::default();
        assert!(state.consume(&poll(vec![finished()])).is_err());
        assert!(state.consume(&poll(vec![item(), finished()])).is_err());
        let mut state = Completion::default();
        state.consume(&poll(vec![item()])).unwrap();
        let mut changed = poll(vec![finished()]);
        changed.operation_id = [6; 16];
        assert!(state.consume(&changed).is_err());
    }
    #[test]
    fn failed_native_job_never_admits_a_temp() {
        let mut state = Completion::default();
        let result = state
            .consume(&poll(vec![
                JobUpdate::Item {
                    item: 0,
                    state: ItemState::OutcomeUnknown,
                    destination: Some([8, 9]),
                    error: Some("uncertain effect".into()),
                    recoveries: vec![[1; 16]],
                },
                JobUpdate::Finished {
                    completed: 0,
                    failed: 1,
                    cancelled: 0,
                },
            ]))
            .unwrap();
        assert_eq!(result, None);
        assert_eq!(state.consume(&poll(vec![])).unwrap(), None);
    }
    #[test]
    fn duplicate_or_inconsistent_receipt_never_admits_a_temp() {
        for updates in [
            vec![item(), item(), finished()],
            vec![
                item(),
                JobUpdate::Finished {
                    completed: 2,
                    failed: 0,
                    cancelled: 0,
                },
            ],
        ] {
            assert!(Completion::default().consume(&poll(updates)).is_err());
        }
    }
}

// A finite slot is released only after the real capture receipt and Finished.
// Loss, cancellation and unknown effects keep the actual custody reserved.
#[derive(Default)]
struct CleanupCompletion {
    operation: Option<[u8; 16]>,
    item: Option<bool>,
    finished: bool,
    rejected: bool,
    released: bool,
}
impl CleanupCompletion {
    fn consume(&mut self, poll: &JobPoll) -> Result<bool, String> {
        if self.rejected {
            return Err(denied());
        }
        let result = self.consume_inner(poll);
        if result.is_err() {
            self.rejected = true;
        }
        result
    }
    fn consume_inner(&mut self, poll: &JobPoll) -> Result<bool, String> {
        if poll.operation_id == [0; 16] || self.operation.is_some_and(|id| id != poll.operation_id)
        {
            return Err(denied());
        }
        self.operation = Some(poll.operation_id);
        for update in &poll.updates {
            match update {
                JobUpdate::Progress { item: 0, .. } if !self.finished => {}
                JobUpdate::Item {
                    item: 0,
                    state: ItemState::Completed,
                    destination: None,
                    error: None,
                    recoveries,
                } if self.item.is_none() && !self.finished && !recoveries.is_empty() => {
                    self.item = Some(true);
                }
                JobUpdate::Item { item: 0, state, .. }
                    if self.item.is_none()
                        && !self.finished
                        && !matches!(state, ItemState::Completed) =>
                {
                    self.item = Some(false);
                }
                JobUpdate::Finished {
                    completed: 1,
                    failed: 0,
                    cancelled: 0,
                } if !self.finished && self.item == Some(true) => {
                    self.finished = true;
                }
                JobUpdate::Finished {
                    completed: 0,
                    failed,
                    cancelled,
                } if !self.finished
                    && self.item == Some(false)
                    && failed.checked_add(*cancelled) == Some(1) =>
                {
                    self.finished = true;
                }
                _ => return Err(denied()),
            }
        }
        Ok(self.finished && self.item == Some(true))
    }
}
struct CleanupJob {
    scope: Arc<Scope>,
    name: EntryName,
    expected: [u64; 2],
    cap: Arc<dyn runtime_fs::directory_backend::WorkspaceCreatedTemporaryBackend>,
    inner: Arc<dyn WorkspaceJobBackend>,
    state: Mutex<CleanupCompletion>,
}
impl WorkspaceJobBackend for CleanupJob {
    fn live_scope(&self) -> Option<Result<(), String>> {
        Some(self.scope.check())
    }
    fn poll(&self) -> Result<JobPoll, String> {
        let mut state = self.state.lock().map_err(|_| denied())?;
        self.scope.call(|| {
            let poll = self.inner.poll()?;
            if state.consume(&poll)? && !state.released {
                self.cap.check()?;
                let mut armed = self.scope.armed.lock().map_err(|_| denied())?;
                let mut created = self.scope.created.lock().map_err(|_| denied())?;
                let mut temps = self.scope.temps.lock().map_err(|_| denied())?;
                if !matches!(temps.get(&self.name),Some(Temp::Ready(id)) if *id == self.expected)
                    || !created
                        .get(&self.name)
                        .is_some_and(|cap| Arc::ptr_eq(cap, &self.cap))
                {
                    return Err(denied());
                }
                temps.remove(&self.name);
                created.remove(&self.name);
                armed.remove(&self.name);
                state.released = true;
            }
            Ok(poll)
        })
    }
    fn cancel(&self) -> Result<(), String> {
        self.scope.call(|| self.inner.cancel())
    }
}

#[cfg(test)]
mod completed_retirement_tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    // Actual socket ownership tests of the production removal guard. These do
    // not construct ACK/Read authority or assert installed Save qualification.
    #[test]
    fn partial_retirement_retains_exact_socket_owner() {
        let (old, peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let old = Arc::new(old);
        let weak = Arc::downgrade(&old);
        let latest = Arc::new(UnixStream::pair().unwrap().0);
        let mut rows = vec![old.clone(), latest.clone()];
        let progress = CompletedRetirement {
            daemon_retired: true,
            rpc_retired: false,
            ..CompletedRetirement::default()
        };
        assert!(remove_older_completed(&mut rows, &latest, &old, &progress).is_err());
        drop(old);
        assert!(weak.upgrade().is_some());
        assert_eq!(rows.len(), 2);
        let mut byte = [0];
        assert_eq!(
            std::io::Read::read(&mut &peer, &mut byte)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
    }
    #[test]
    fn every_missing_cache_ack_retains_actual_stream_owner() {
        for missing in 0..4 {
            let (old, peer) = UnixStream::pair().unwrap();
            peer.set_nonblocking(true).unwrap();
            let old = Arc::new(old);
            let weak = Arc::downgrade(&old);
            let latest = Arc::new(UnixStream::pair().unwrap().0);
            let mut rows = vec![latest.clone(), old.clone()];
            let progress = CompletedRetirement {
                daemon_retired: true,
                rpc_retired: true,
                read_retired: missing != 0,
                pin_retired: missing != 1,
                job_retired: missing != 2,
                pending_retired: missing != 3,
            };
            assert!(remove_older_completed(&mut rows, &latest, &old, &progress).is_err());
            drop(old);
            assert!(weak.upgrade().is_some());
            assert_eq!(rows.len(), 2);
            let mut byte = [0];
            assert_eq!(
                std::io::Read::read(&mut &peer, &mut byte)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::WouldBlock
            );
        }
    }
    #[test]
    fn newest_and_missing_successor_cannot_release_owner() {
        let older = Arc::new(UnixStream::pair().unwrap().0);
        let latest = Arc::new(UnixStream::pair().unwrap().0);
        // The older terminal arrives last. The typed current successor must
        // remain retained regardless of its vector position.
        let mut rows = vec![latest.clone(), older.clone()];
        let progress = CompletedRetirement {
            daemon_retired: true,
            rpc_retired: true,
            read_retired: true,
            pin_retired: true,
            job_retired: true,
            pending_retired: true,
        };
        assert!(remove_older_completed(&mut rows, &latest, &latest, &progress).is_err());
        let missing = Arc::new(UnixStream::pair().unwrap().0);
        assert!(remove_older_completed(&mut rows, &missing, &older, &progress).is_err());
        assert_eq!(rows.len(), 2);
        assert!(Arc::ptr_eq(rows.first().unwrap(), &latest));
    }
}
