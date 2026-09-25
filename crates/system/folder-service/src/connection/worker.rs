//! Serialized service owner. No path-based copy engine or reconstructed authority.
mod recovery;
use super::{
    endpoint::{self, Access, EndpointContext, PinnedEndpoint},
    store::{
        ConnectionPolicy, ConnectionRecord, ConnectionStore, EndpointBinding, Lifecycle,
        PageCursor, StoreError,
    },
    transaction::{InitialPreview, TransactionSession, WorkerReviewed},
};
use crate::connected::{
    ConflictPage, ConnectedService, ConnectionError, ConnectionId, ConnectionInfo,
    ConnectionLifecycle, ConnectionPage, DELETION_RETENTION_SECONDS, EndpointIssue, InitialPage,
    Observer, PAGE_LIMIT, Pending, Phase, ReviewId, Side, Snapshot,
};
pub use recovery::TombstoneVersion;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::AtomicBool,
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
};

#[cfg(target_os = "linux")]
use super::transaction;

const COMMAND_CAPACITY: usize = 16;
const COMMAND_BATCH_LIMIT: usize = 16;
const MAX_RUN_STEPS: u32 = 4096;
type Reply<T> = SyncSender<Result<T, ConnectionError>>;

/// Opaque, read-only selection hints. Fresh native qualification is mandatory
/// again at creation and on every run; these values carry no filesystem handles.
pub struct SelectedFolders {
    source: EndpointBinding,
    destination: EndpointBinding,
}
impl SelectedFolders {
    /// Probe explicitly selected paths without modifying either endpoint.
    /// # Errors
    /// Refuses unsupported hosts, unavailable or ambiguous identities and
    /// overlapping selections. Write capability is checked only on creation.
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "Both ancestor directions must be excluded."
    )]
    pub fn probe(source: &Path, destination: &Path) -> Result<Self, ConnectionError> {
        supported()?;
        let left = endpoint::probe_endpoint(source).map_err(endpoint_error)?;
        let right = endpoint::probe_endpoint(destination).map_err(endpoint_error)?;
        if source.starts_with(destination)
            || destination.starts_with(source)
            || left.identity == right.identity
        {
            return Err(ConnectionError::IdentityChanged);
        }
        Ok(Self {
            source: EndpointBinding {
                identity: left.identity,
                locator: left.locator,
            },
            destination: EndpointBinding {
                identity: right.identity,
                locator: right.locator,
            },
        })
    }
}

pub struct ConnectionCursor(PageCursor);
pub struct ConflictCursor(PageCursor);
pub struct RecoveryCursor(PageCursor);
pub(crate) enum Command {
    Create(SelectedFolders, Reply<ConnectionInfo>),
    List(Option<ConnectionCursor>, usize, Reply<ConnectionPage>),
    Open(ConnectionId, bool, Reply<Observer>),
    Preview(ReviewId, usize, Reply<InitialPage>),
    Accept(ReviewId, Reply<()>),
    Pause(ConnectionId, Reply<()>),
    Disconnect(ConnectionId, Reply<ConnectionInfo>),
    Conflicts(ConnectionId, Option<ConflictCursor>, Reply<ConflictPage>),
    Recovery(
        ConnectionId,
        Option<RecoveryCursor>,
        usize,
        Reply<crate::connected::RecoveryPage>,
    ),
    PreviewRecovery(
        crate::connected::RecoveryId,
        Side,
        Reply<crate::connected::SelectionPreview>,
    ),
    PreviewConflict(
        ConnectionId,
        u64,
        u64,
        crate::connected::ConflictChoice,
        Side,
        Reply<crate::connected::SelectionPreview>,
    ),
    Restore(crate::connected::SelectionReviewId, bool, Reply<()>),
}

fn supported() -> Result<(), ConnectionError> {
    if cfg!(target_os = "linux") {
        Ok(())
    } else {
        Err(ConnectionError::UnsupportedHost)
    }
}
pub(crate) fn open(path: Option<&Path>) -> Result<ConnectedService, ConnectionError> {
    supported()?; // Must precede store creation, lock acquisition, or device trials.
    let store = match path {
        Some(path) => ConnectionStore::open_at(path),
        None => ConnectionStore::open_default(),
    }
    .map_err(store_error)?;
    spawn(store)
}
fn spawn(store: ConnectionStore) -> Result<ConnectedService, ConnectionError> {
    let (sender, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
    let service = ConnectedService::from_sender(sender);
    let command_owners = service.command_lifetime();
    std::thread::Builder::new()
        .name("connected-folders".into())
        .spawn(move || {
            Worker {
                store,
                active: None,
                lifetime: Arc::new(()),
                generation: 0,
            }
            .run(&receiver, &command_owners);
        })
        .map_err(|_| ConnectionError::Persistence)?;
    Ok(service)
}
struct Pins {
    source: PinnedEndpoint,
    destination: PinnedEndpoint,
    session: TransactionSession,
}
struct Active {
    observer: Observer,
    pins: Option<Pins>,
    preview: Option<(ReviewId, InitialPreview)>,
    reviewed: Option<WorkerReviewed>,
    selection: Option<recovery::SelectionReview>,
    running: bool,
    attempts: u32,
    last_revision: Option<u64>,
}
struct Worker {
    store: ConnectionStore,
    active: Option<Active>,
    lifetime: Arc<()>,
    generation: u64,
}
impl Worker {
    fn run(&mut self, receiver: &Receiver<Command>, command_owners: &std::sync::Weak<()>) {
        let mut handled = 0usize;
        loop {
            let running = self.active.as_ref().is_some_and(|active| active.running)
                && command_owners.strong_count() != 0;
            // Reads cannot replenish the bounded queue forever without giving
            // native work a turn. Dropped command owners never start a new step;
            // already queued controls still drain at this durable boundary.
            if running && handled >= COMMAND_BATCH_LIMIT {
                self.step();
                handled = 0;
                continue;
            }
            let command = if running {
                match receiver.try_recv() {
                    Ok(command) => Some(command),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => break,
                }
            } else {
                match receiver.recv() {
                    Ok(command) => Some(command),
                    Err(_) => break,
                }
            };
            if let Some(command) = command {
                self.command(command);
                handled = handled.saturating_add(1);
                #[cfg(test)]
                tests::after_dispatch_command(
                    self.active
                        .as_ref()
                        .map(|active| active.observer.snapshot().connection.id),
                );
            } else {
                self.step();
                handled = 0;
            }
            if !self.active.as_ref().is_some_and(|active| active.running) {
                handled = 0;
            }
        }
        // No observer remains live after its sole publisher exits.
        if let Some(active) = &mut self.active {
            active.running = false;
            set_phase(active, Phase::Detached);
        }
    }
    fn command(&mut self, command: Command) {
        match command {
            Command::Recovery(id, cursor, limit, reply) => {
                let result = self.recovery_page(id, cursor, limit);
                let _ = reply.send(result);
            }
            Command::PreviewRecovery(id, target, reply) => {
                let result = self.preview_recovery(id, target);
                let _ = reply.send(result);
            }
            Command::PreviewConflict(id, serial, revision, choice, target, reply) => {
                let result = self.preview_conflict(id, serial, revision, choice, target);
                let _ = reply.send(result);
            }
            Command::Restore(review, replace, reply) => {
                let result = self.restore_selection(review, replace);
                let _ = reply.send(result);
            }
            Command::Create(selection, reply) => {
                let result = self.create(selection);
                let _ = reply.send(result);
            }
            Command::List(cursor, limit, reply) => {
                let result = self
                    .store
                    .connection_page(cursor.map(|cursor| cursor.0), limit)
                    .map(|page| ConnectionPage {
                        entries: page
                            .entries
                            .into_iter()
                            .map(|record| info(&record))
                            .collect(),
                        next: page.next.map(ConnectionCursor),
                    })
                    .map_err(store_error);
                let _ = reply.send(result);
            }
            Command::Open(id, start, reply) => {
                let result = self.open_connection(id, start);
                let _ = reply.send(result);
            }
            Command::Preview(review, offset, reply) => {
                let result = self.preview(&review, offset);
                let _ = reply.send(result);
            }
            Command::Accept(review, reply) => {
                let result = self.accept(&review);
                let _ = reply.send(result);
            }
            Command::Pause(id, reply) => {
                let result = self.pause(id);
                let _ = reply.send(result);
            }
            Command::Disconnect(id, reply) => {
                let result = self.disconnect(id);
                let _ = reply.send(result);
            }
            Command::Conflicts(id, cursor, reply) => {
                let result = self.conflict_page(id, cursor);
                let _ = reply.send(result);
            }
        }
    }
    fn create(&mut self, selection: SelectedFolders) -> Result<ConnectionInfo, ConnectionError> {
        supported()?;
        // Qualify before publishing a connection record. The selected write
        // trials use the existing private disposable native probe objects.
        let (_source, _destination) =
            pin_pair(&selection.source, &selection.destination).map_err(|(_, error)| error)?;
        #[cfg(not(target_os = "linux"))]
        let bytes = [0; 16];
        #[cfg(target_os = "linux")]
        let bytes = {
            use std::io::Read;
            let mut bytes = [0; 16];
            std::fs::File::open("/dev/urandom")
                .and_then(|mut file| file.read_exact(&mut bytes))
                .map_err(|_| ConnectionError::Persistence)?;
            bytes
        };
        let record = self
            .store
            .create_connection(
                ConnectionId(bytes),
                selection.source,
                selection.destination,
                ConnectionPolicy {
                    deletion_retention_seconds: DELETION_RETENTION_SECONDS,
                },
            )
            .map_err(store_error)?;
        Ok(info(record.value()))
    }
    fn open_connection(
        &mut self,
        id: ConnectionId,
        start: bool,
    ) -> Result<Observer, ConnectionError> {
        if !start
            && let Some(active) = &self.active
            && active.observer.snapshot().connection.id == id
        {
            return Ok(active.observer.clone());
        }
        if self.active.as_ref().is_some_and(|active| active.running) {
            return Err(ConnectionError::Busy);
        }
        let record = self.store.load_connection(id).map_err(store_error)?;
        if start && record.lifecycle != Lifecycle::Active {
            return Err(ConnectionError::Disconnected);
        }
        let pending = self
            .store
            .executor_position(id)
            .map_err(store_error)?
            .0
            .is_some();
        let phase = match record.lifecycle {
            _ if pending => Phase::RecoveryRequired(ConnectionError::RecoveryRequired),
            Lifecycle::Active => Phase::Pending(Pending::NotCompared),
            Lifecycle::Disconnecting => Phase::Disconnecting,
            Lifecycle::Disconnected => Phase::Disconnected,
        };
        let snapshot = Snapshot {
            connection: info(&record),
            phase,
            verified_steps: 0,
            latest_conflict: None,
            pending_operation: pending,
            endpoint_issue: None,
        };
        let observer = if let Some(previous) = &self.active {
            if previous.observer.snapshot().connection.id == id {
                let observer = previous.observer.clone();
                *observer
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot;
                observer
            } else {
                set_phase(previous, Phase::Detached);
                Observer(Arc::new(Mutex::new(snapshot)))
            }
        } else {
            Observer(Arc::new(Mutex::new(snapshot)))
        };
        self.active = Some(Active {
            observer: observer.clone(),
            pins: None,
            preview: None,
            reviewed: None,
            selection: None,
            running: false,
            attempts: 0,
            last_revision: None,
        });
        let active = self.active.as_mut().ok_or(ConnectionError::Closed)?;
        if start {
            match pin_pair(&record.source, &record.destination) {
                Ok((source, destination)) => match self.store.transaction_session(id) {
                    Ok(session) => {
                        active.pins = Some(Pins {
                            source,
                            destination,
                            session,
                        });
                        active.running = true;
                        set_phase(active, Phase::Scanning);
                    }
                    Err(error) => set_phase(active, Phase::RecoveryRequired(store_error(error))),
                },
                Err((side, error)) => endpoint_refused(active, side, error, pending),
            }
        }
        Ok(observer)
    }
    fn pause(&mut self, id: ConnectionId) -> Result<(), ConnectionError> {
        let active = self
            .active
            .as_mut()
            .filter(|active| active.observer.snapshot().connection.id == id)
            .ok_or(ConnectionError::Unavailable)?;
        if active.observer.snapshot().connection.lifecycle != ConnectionLifecycle::Active {
            return Err(ConnectionError::Disconnected);
        }
        active.running = false;
        active.selection = None;
        active.preview = None;
        active.reviewed = None;
        active.pins = None;
        let pending = self
            .store
            .executor_position(id)
            .map_err(store_error)?
            .0
            .is_some();
        active
            .observer
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending_operation = pending;
        set_phase(
            active,
            if pending {
                Phase::RecoveryRequired(ConnectionError::RecoveryRequired)
            } else {
                Phase::Paused
            },
        );
        Ok(())
    }
    fn disconnect(&mut self, id: ConnectionId) -> Result<ConnectionInfo, ConnectionError> {
        let record = self.store.load_connection(id).map_err(store_error)?;
        if record.lifecycle == Lifecycle::Disconnected {
            return Ok(info(&record));
        }
        if let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.observer.snapshot().connection.id == id)
        {
            active.running = false;
            active.selection = None;
            active.preview = None;
            active.reviewed = None;
            active.pins = None;
            set_phase(active, Phase::Disconnecting);
        }
        let result = (|| {
            let fence = self
                .store
                .request_disconnect(id, record.revision)
                .map_err(store_error)?;
            self.store
                .finish_disconnect(&fence)
                .map(|ack| info(ack.value()))
                .map_err(store_error)
        })();
        if let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.observer.snapshot().connection.id == id)
        {
            if let Ok(record) = self.store.load_connection(id) {
                active
                    .observer
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .connection = info(&record);
            }
            set_phase(
                active,
                match &result {
                    Ok(_) => Phase::Disconnected,
                    Err(error) => Phase::RecoveryRequired(*error),
                },
            );
        }
        result
    }
    fn preview(&self, review: &ReviewId, offset: usize) -> Result<InitialPage, ConnectionError> {
        let (_, preview) = self
            .active
            .as_ref()
            .and_then(|active| active.preview.as_ref())
            .filter(|(id, _)| id == review)
            .ok_or(ConnectionError::StaleReview)?;
        preview.worker_page(offset).map_err(store_error)
    }
    fn accept(&mut self, review: &ReviewId) -> Result<(), ConnectionError> {
        let active = self.active.as_mut().ok_or(ConnectionError::StaleReview)?;
        if active.preview.as_ref().is_none_or(|(id, _)| id != review) {
            return Err(ConnectionError::StaleReview);
        }
        let (_, preview) = active.preview.take().ok_or(ConnectionError::StaleReview)?;
        let pins = active.pins.as_ref().ok_or(ConnectionError::StaleReview)?;
        let accepted = preview.worker_accept(
            &self.store,
            &pins.session,
            &pins.source,
            &pins.destination,
            &AtomicBool::new(false),
        );
        match accepted {
            Ok(Some(reviewed)) => {
                active.reviewed = Some(reviewed);
                active.running = true;
                set_phase(active, Phase::Syncing);
                Ok(())
            }
            Ok(None) => {
                set_phase(active, Phase::Pending(Pending::Structural));
                Ok(())
            }
            Err(error) => {
                let error = store_error(error);
                #[cfg(target_os = "linux")]
                let phase = recovery_phase(active, error, false);
                #[cfg(not(target_os = "linux"))]
                let phase = Phase::RecoveryRequired(error);
                set_phase(active, phase);
                Err(error)
            }
        }
    }
    fn step(&mut self) {
        let Some(active) = self.active.as_mut().filter(|active| active.running) else {
            return;
        };
        #[cfg(test)]
        tests::before_dispatch_step(active.observer.snapshot().connection.id);
        active.attempts = active.attempts.saturating_add(1);
        if active.attempts > MAX_RUN_STEPS {
            active.running = false;
            set_phase(active, Phase::Pending(Pending::RunLimit));
            return;
        }
        #[cfg(target_os = "linux")]
        {
            let Some(pins) = active.pins.as_ref() else {
                active.running = false;
                set_phase(
                    active,
                    Phase::RecoveryRequired(ConnectionError::RecoveryRequired),
                );
                return;
            };
            let id = active.observer.snapshot().connection.id;
            // No external cancellation flag is forwarded into a begun atomic
            // step. Pause/disconnect commands are applied at its durable return.
            let cancel = AtomicBool::new(false);
            let pending = match self.store.executor_position(id) {
                Ok(position) => position.0.is_some(),
                Err(error) => {
                    active.running = false;
                    set_phase(active, Phase::RecoveryRequired(store_error(error)));
                    return;
                }
            };
            for (side, endpoint) in [
                (Side::Source, &pins.source),
                (Side::Destination, &pins.destination),
            ] {
                if let Err(error) = endpoint.revalidate_for_write() {
                    active.running = false;
                    endpoint_refused(active, side, endpoint_error(error), pending);
                    return;
                }
            }
            #[cfg(test)]
            tests::before_native_step(id);
            let outcome = match active.reviewed.take() {
                Some(WorkerReviewed::Empty(reviewed)) => transaction::execute_empty(
                    &mut self.store,
                    reviewed,
                    &pins.session,
                    &pins.source,
                    &pins.destination,
                    &cancel,
                ),
                Some(WorkerReviewed::Regular(reviewed)) => transaction::execute_reviewed_initial(
                    &mut self.store,
                    reviewed,
                    &pins.session,
                    &pins.source,
                    &pins.destination,
                    &cancel,
                ),
                None => transaction::execute_next_regular(
                    &mut self.store,
                    id,
                    &pins.session,
                    &pins.source,
                    &pins.destination,
                    &cancel,
                ),
            };
            self.outcome(outcome);
        }
        #[cfg(not(target_os = "linux"))]
        {
            active.running = false;
            set_phase(
                active,
                Phase::RecoveryRequired(ConnectionError::UnsupportedHost),
            );
        }
    }
    #[cfg(target_os = "linux")]
    fn outcome(&mut self, outcome: transaction::TransactionOutcome) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        active.running = false;
        let id = active.observer.snapshot().connection.id;
        #[cfg(test)]
        tests::record_native_position(&self.store, id);
        let metadata = self.store.load_connection(id).and_then(|record| {
            self.store
                .executor_position(id)
                .map(|position| (record, position.0.is_some()))
        });
        match metadata {
            Ok((record, pending)) => {
                let mut snapshot = active
                    .observer
                    .0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                snapshot.connection = info(&record);
                snapshot.pending_operation = pending;
                snapshot.endpoint_issue = None;
            }
            Err(error) => {
                set_phase(active, Phase::RecoveryRequired(store_error(error)));
                return;
            }
        }
        match outcome {
            transaction::TransactionOutcome::InitialReview(preview) => {
                let entries = preview.worker_len();
                let Some(generation) = self.generation.checked_add(1) else {
                    set_phase(
                        active,
                        Phase::RecoveryRequired(ConnectionError::LimitExceeded),
                    );
                    return;
                };
                self.generation = generation;
                let review = ReviewId {
                    generation: self.generation,
                    lifetime: Arc::clone(&self.lifetime),
                };
                active.preview = Some((review.clone(), preview));
                set_phase(active, Phase::AwaitingReview { review, entries });
            }
            transaction::TransactionOutcome::VerifiedEmpty { revision } => {
                active.last_revision = Some(revision);
                set_phase(active, Phase::UpToDate);
            }
            transaction::TransactionOutcome::Verified {
                operation,
                up_to_date,
            } => {
                let advanced = active.last_revision != Some(operation.revision);
                active.last_revision = Some(operation.revision);
                if advanced {
                    let mut snapshot = active
                        .observer
                        .0
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    snapshot.verified_steps = snapshot.verified_steps.saturating_add(1);
                }
                active.running = !up_to_date && advanced;
                set_phase(
                    active,
                    if up_to_date {
                        Phase::UpToDate
                    } else if advanced {
                        Phase::Syncing
                    } else {
                        Phase::Pending(Pending::NativeRefused)
                    },
                );
            }
            transaction::TransactionOutcome::Conflict(conflict) => {
                match recovery::conflict_info(&self.store, conflict) {
                    Ok(conflict) => {
                        active
                            .observer
                            .0
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .latest_conflict = Some(conflict);
                        set_phase(active, Phase::Conflict);
                    }
                    Err(error) => set_phase(active, Phase::RecoveryRequired(error)),
                }
            }
            transaction::TransactionOutcome::StructuralPending(reason) => {
                let _ = reason;
                set_phase(active, Phase::Pending(Pending::Structural));
            }
            transaction::TransactionOutcome::AccountedPending { operation } => {
                let _ = operation;
                set_phase(active, Phase::Pending(Pending::NativeRefused));
            }
            transaction::TransactionOutcome::RecoveryRequired { error, operation } => {
                set_phase(
                    active,
                    recovery_phase(active, store_error(error), operation.is_some()),
                );
            }
        }
    }
}
#[cfg(target_os = "linux")]
fn recovery_phase(active: &Active, error: ConnectionError, pending: bool) -> Phase {
    let mut phase = Phase::RecoveryRequired(error);
    let pending = pending || active.observer.snapshot().pending_operation;
    if !pending && let Some(pins) = &active.pins {
        for (side, endpoint) in [
            (Side::Source, &pins.source),
            (Side::Destination, &pins.destination),
        ] {
            use super::model::{Observation, PendingReason};
            match endpoint::observe(endpoint, &AtomicBool::new(false)).observation() {
                Observation::Unavailable(reason) => {
                    let error = match reason {
                        PendingReason::Offline => ConnectionError::Unavailable,
                        PendingReason::PermissionDenied => ConnectionError::PermissionDenied,
                        PendingReason::IdentityChanged => ConnectionError::IdentityChanged,
                        _ => ConnectionError::Unverified,
                    };
                    active
                        .observer
                        .0
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .endpoint_issue = Some(EndpointIssue { side, error });
                    phase = Phase::Unavailable { side, error };
                    break;
                }
                Observation::Incomplete(_) => {
                    phase = Phase::Pending(Pending::IncompleteObservation);
                    break;
                }
                Observation::Complete => (),
            }
        }
    }
    phase
}

fn endpoint_refused(active: &Active, side: Side, error: ConnectionError, pending: bool) {
    let mut snapshot = active
        .observer
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    snapshot.pending_operation = pending;
    snapshot.endpoint_issue = Some(EndpointIssue { side, error });
    snapshot.phase = if pending {
        Phase::RecoveryRequired(ConnectionError::RecoveryRequired)
    } else {
        Phase::Unavailable { side, error }
    };
}

fn set_phase(active: &Active, phase: Phase) {
    active
        .observer
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .phase = phase;
}
fn pin_pair(
    source: &EndpointBinding,
    destination: &EndpointBinding,
) -> Result<(PinnedEndpoint, PinnedEndpoint), (Side, ConnectionError)> {
    let mut context = EndpointContext::default();
    let mut pin = |binding: &EndpointBinding, side| {
        context
            .register(binding.locator.clone(), binding.identity)
            .map_err(|error| (side, endpoint_error(error)))?;
        context
            .open_endpoint(&binding.identity, Access::Write)
            .map_err(|error| (side, endpoint_error(error)))
    };
    Ok((
        pin(source, Side::Source)?,
        pin(destination, Side::Destination)?,
    ))
}
fn info(record: &ConnectionRecord) -> ConnectionInfo {
    ConnectionInfo {
        id: record.id,
        source: record.source.locator.path().to_owned(),
        destination: record.destination.locator.path().to_owned(),
        revision: record.revision,
        has_baseline: record.baseline_revision.is_some(),
        lifecycle: match record.lifecycle {
            Lifecycle::Active => ConnectionLifecycle::Active,
            Lifecycle::Disconnecting => ConnectionLifecycle::Disconnecting,
            Lifecycle::Disconnected => ConnectionLifecycle::Disconnected,
        },
        deletion_retention_seconds: record.policy.deletion_retention_seconds,
    }
}
fn store_error(error: StoreError) -> ConnectionError {
    match error {
        StoreError::Busy => ConnectionError::Busy,
        StoreError::UnsupportedDurability => ConnectionError::Unverified,
        StoreError::IdentityChanged => ConnectionError::IdentityChanged,
        StoreError::Corrupt => ConnectionError::CorruptStore,
        StoreError::LimitExceeded => ConnectionError::LimitExceeded,
        StoreError::StaleRevision => ConnectionError::StaleReview,
        StoreError::Disconnected => ConnectionError::Disconnected,
        StoreError::RecoverySpace => ConnectionError::RecoverySpace,
        StoreError::Persistence(error) => {
            drop(error);
            ConnectionError::Persistence
        }
        StoreError::RecoveryRequired | StoreError::LegacyNativePending | StoreError::Cancelled => {
            ConnectionError::RecoveryRequired
        }
    }
}
fn endpoint_error(error: endpoint::EndpointError) -> ConnectionError {
    use endpoint::EndpointError as E;
    match error {
        E::Offline => ConnectionError::Unavailable,
        E::PermissionDenied => ConnectionError::PermissionDenied,
        E::IdentityChanged | E::AmbiguousIdentity => ConnectionError::IdentityChanged,
        E::UnsafeVolume => ConnectionError::UnsafeVolume,
        E::ReadOnly => ConnectionError::ReadOnly,
        E::Unsupported | E::Unverified => ConnectionError::Unverified,
        E::Unstable => ConnectionError::Unstable,
        E::LimitExceeded => ConnectionError::LimitExceeded,
        E::Cancelled => ConnectionError::RecoveryRequired,
    }
}
#[cfg(test)]
mod tests;
