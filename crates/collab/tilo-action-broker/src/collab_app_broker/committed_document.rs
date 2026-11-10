//! Current committed reads on the actual admitted channel.
mod capture;
use super::{Arc, CrdtRoute, Duration, JoinedChannel, Ordering, Rc, Weak};
use crate::channel_encryption::ChannelKeyProvider;
pub(super) use capture::Budget;
pub use capture::DocumentBytes;
use runtime_collab::committed_document::{MAX_DOCUMENT_BYTES, Reply, Request};

/// What [`NativeDocumentReads::await_sealed_history`] waits on, awaited on
/// the broker's local set.
pub type AwaitingHistory = std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>;

/// No public constructor. The route and original caller are retained from the
/// admitted channel, together with live launcher/host-worker custody.
#[derive(Clone)]
pub struct NativeDocumentReadContext {
    route: CrdtRoute,
    certificate: tilo_user_identity::DeviceCert,
    record_kind: Option<String>,
    creation: Option<crate::services::ApplicationCreationEvidence>,
    revalidate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}

#[cfg(test)]
#[path = "committed_document/await_tests.rs"]
mod await_tests;
#[cfg(test)]
#[path = "committed_document/changes_tests.rs"]
mod changes_tests;
#[cfg(test)]
#[path = "committed_document/service_session_tests.rs"]
mod service_session_tests;
impl NativeDocumentReadContext {
    /// A read of `route` by an agent task acting for the person whose device certificate is
    /// `certificate` (`record_read`): no launcher custody and no creation, only `revalidate`,
    /// which decides the task's read of the record again. Only the broker can make one.
    pub(super) fn for_task(
        route: CrdtRoute,
        certificate: tilo_user_identity::DeviceCert,
        revalidate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    ) -> Self {
        Self {
            route,
            certificate,
            record_kind: None,
            creation: None,
            revalidate,
        }
    }

    /// A read of `route` by `certificate`'s device with nothing else to check: what a test of a
    /// document owner outside this crate reads with.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn for_reader(route: CrdtRoute, certificate: tilo_user_identity::DeviceCert) -> Self {
        Self {
            route,
            certificate,
            record_kind: None,
            creation: None,
            revalidate: Arc::new(|| Ok(())),
        }
    }
    pub fn route(&self) -> &CrdtRoute {
        &self.route
    }
    pub fn certificate(&self) -> &tilo_user_identity::DeviceCert {
        &self.certificate
    }
    pub fn record_kind(&self) -> Option<&str> {
        self.record_kind.as_deref()
    }
    pub fn application_creation(&self) -> Option<&crate::services::ApplicationCreationEvidence> {
        self.creation.as_ref()
    }
    /// Re-checks the admitted channel's live custody and admission.
    pub fn revalidate(&self) -> Result<(), String> {
        (self.revalidate)()
    }
}
/// A revision a committed read on this channel handed out (its last piece
/// acknowledged), with the heads it names: what a later read may answer
/// with only the changes since.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldRevision {
    pub revision: [u8; 32],
    /// Sorted, at most [`runtime_collab::committed_document::MAX_CHANGES_BASE`].
    pub heads: Vec<[u8; 32]>,
}

/// Data only. Its provider retains the actual bounded native save's lifetime.
pub enum NativeDocumentRead {
    Unchanged([u8; 32]),
    /// The owner holds no complete capture right now: the record's governed
    /// checkpoint is still arriving. Not readable yet, and no failure:
    /// answered as [`Reply::Unavailable`], the reader withholds readiness
    /// and asks again.
    Pending,
    /// A record the workspace's hosts send a device only once it asks
    /// (`neighbour_wire::record_sync`), which they said they have none of,
    /// or no host of the workspace is connected to ask: answered as
    /// [`Reply::Absent`], no failure either, and a held `Await` wakes when
    /// that changes.
    Absent,
    Captured {
        revision: [u8; 32],
        /// The sorted heads `revision` names; empty when the owner does not
        /// say, and later reads on the channel are then whole.
        heads: Vec<[u8; 32]>,
        body: Box<dyn DocumentBytes>,
    },
    /// Only the changes from the held revision with heads `base` to
    /// `revision` (with `heads`): Automerge change bytes.
    Changes {
        revision: [u8; 32],
        heads: Vec<[u8; 32]>,
        base: Vec<[u8; 32]>,
        body: Box<dyn DocumentBytes>,
    },
}
/// Who answers an app's read of one record ([`NativeDocumentReads::record_read`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordRead {
    /// The app's own channel: a workspace this owner keeps no records of, or a
    /// record a sealed workspace does not hold yet, which is new and which the
    /// app creates on its own sealed channel. Answered [`Reply::Legacy`].
    Own,
    /// Every record of a workspace this owner commits
    /// ([`NativeDocumentReads::requires_workspace`]): read under the launcher's or
    /// a host worker's custody.
    Committed,
    /// A record a sealed workspace this device holds already holds, read through
    /// the channel's keys. Its authority is the one the app's own sealed
    /// channel reads under: the channel's live admission and the workspace's read
    /// policy, plus the launcher's custody whenever the app has one.
    Sealed,
}
pub trait NativeDocumentReads: Send + Sync {
    /// Durable native installation obligation, independent of runtime health.
    fn requires_workspace(&self, workspace: &str) -> bool;
    /// Whether `workspace` seals every record write under the record channel's
    /// keys: no write in it is taken unencrypted, from any app.
    fn seals_records(&self, workspace: &str) -> bool {
        let _ = workspace;
        false
    }
    /// Who answers an app's read of `app`'s `record` in `workspace`.
    fn record_read(&self, workspace: &str, app: &str, record: &str) -> RecordRead {
        let _ = (app, record);
        if self.requires_workspace(workspace) {
            RecordRead::Committed
        } else {
            RecordRead::Own
        }
    }
    /// Whether this device holds anything of `app`'s `record` in `workspace`: what its owner commits
    /// or holds, or sealed writes of it. A sealed workspace's record it holds nothing of is new to
    /// this device: the workspace has none yet, once its host's replay landed
    /// ([`Self::await_sealed_history`]).
    fn holds_record(&self, workspace: &str, app: &str, record: &str) -> bool {
        self.record_read(workspace, app, record) != RecordRead::Own
    }
    /// Before an Open this owner would answer [`RecordRead::Own`], as a
    /// record that is new: the wait for what decides it, when the record may
    /// exist where this device has not seen it yet. In a workspace whose records
    /// are sealed channels, a record this device holds nothing of may have
    /// been made on another device or before this session, and reaches this
    /// device only in the workspace's host's replay; deciding before it landed
    /// would have the app make a second one. The broker awaits it without
    /// blocking the channel, then decides. `None` when nothing is awaited.
    fn await_sealed_history(&self, context: &NativeDocumentReadContext) -> Option<AwaitingHistory> {
        let _ = context;
        None
    }
    /// Must check exact current selected custody, caller certificate, and native
    /// document Read permission. Called again before every returned chunk.
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String>;
    /// Called only after the broker reserves maximum bytes. Implementations must
    /// use the actual bounded native save, refusing before output allocation.
    ///
    /// # Errors
    ///
    /// Returns a message when the document cannot be captured within `maximum` bytes.
    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String>;
    /// What [`Self::capture`] answers for a caller holding `held`, except
    /// that a changed document may be answered with only the changes since
    /// ([`NativeDocumentRead::Changes`], whose `base` is `held.heads`). The
    /// same checks as `capture`, refusing exactly what it refuses; every
    /// case it cannot answer with changes it answers as `capture` does. The
    /// default always does.
    ///
    /// # Errors
    ///
    /// Returns a message when the document cannot be captured within `maximum` bytes,
    /// or the caller may not read it now.
    fn capture_since(
        &self,
        context: &NativeDocumentReadContext,
        held: &HeldRevision,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.capture(context, Some(held.revision), maximum)
    }
    /// Whether a caller holding `known_revision` (or nothing, after
    /// `Unavailable`) still holds what [`Self::capture`] would answer,
    /// decided without capturing. `None` when only a capture can say; the
    /// broker then captures under its byte budget. A held `Await` asks this
    /// on every announcement, so it must stay cheap, and it must refuse
    /// exactly what [`Self::authorize`] refuses.
    fn unchanged(
        &self,
        context: &NativeDocumentReadContext,
        known_revision: Option<[u8; 32]>,
    ) -> Result<Option<bool>, String> {
        let _ = (context, known_revision);
        Ok(None)
    }
    /// The first wall-clock second after `after` (Unix seconds) at which
    /// [`Self::authorize`] or [`Self::capture`] for `context` can answer
    /// differently with nothing written, so nothing announced: a grant, a
    /// certificate or a revocation taking effect at an instant known in
    /// advance. A held `Await` registers it with
    /// [`tilo_identity_host::policy_change::wake_at`].
    ///
    /// # Errors
    ///
    /// The default, and any implementation that cannot tell: a held wait
    /// then re-checks every `UNANNOUNCED_RECHECK`, the cadence readers
    /// polled at, so an unknown expiry is never enforced later than before.
    fn next_change(
        &self,
        context: &NativeDocumentReadContext,
        after: u64,
    ) -> Result<Option<u64>, String> {
        let _ = (context, after);
        Err("this document owner does not know its time-bound inputs".into())
    }
}

/// How often a held `Await` re-checks while a change it depends on cannot be
/// announced (no reliable policy clock or identity-file watch, or an owner
/// that cannot name its expiries): the interval readers used to poll at, so
/// a revocation is never noticed later than it was.
pub(super) const UNANNOUNCED_RECHECK: Duration = Duration::from_millis(200);
struct Active {
    capture: capture::Capture,
    context: NativeDocumentReadContext,
    service: Arc<dyn NativeDocumentReads>,
    /// Started as the answer to a held `Await`, and asked nothing since: a
    /// reader that gave up the wait before its answer came never saw it, so
    /// its next read replaces it.
    unseen: bool,
}
/// A held `Await` on this channel; a newer request on the channel ends it.
struct HeldWait {
    generation: u64,
    wake: Arc<tokio::sync::Notify>,
}
#[derive(Default)]
pub(super) struct Slot {
    active: Option<Active>,
    closed: bool,
    wait: Option<HeldWait>,
    waits: u64,
    /// The last revision this channel handed out whole or as changes (its
    /// last piece acknowledged), when its owner named the heads.
    delivered: Option<HeldRevision>,
    /// This channel's place in the byte budget's queue, after an Open found
    /// the budget held and answered `Busy`.
    queued: Option<Queued>,
    /// Whether the last Open without a revision answered [`Reply::Absent`]
    /// rather than [`Reply::Unavailable`]: what a held `Await` for `None`
    /// holds.
    absent: bool,
}
/// A channel's turn in the byte budget's queue: a task on the broker's local
/// set that waits for the reservation, keeps it for the channel's next Open
/// and wakes the channel's held `Await`. Dropped with the channel (or once
/// the Open took the reservation), which takes it out of the queue or
/// releases what it was granted.
struct Queued {
    granted: Rc<std::cell::RefCell<Option<capture::Reservation>>>,
    wake: Rc<tokio::sync::Notify>,
    task: tokio::task::AbortHandle,
}
impl Queued {
    fn start(budget: &Arc<Budget>) -> Self {
        let granted = Rc::new(std::cell::RefCell::new(None));
        let wake = Rc::new(tokio::sync::Notify::new());
        let turn = budget.wait();
        let task = {
            let granted = Rc::clone(&granted);
            let wake = Rc::clone(&wake);
            tokio::task::spawn_local(async move {
                if let Some(reservation) = turn.await {
                    *granted.borrow_mut() = Some(reservation);
                    wake.notify_one();
                }
            })
            .abort_handle()
        };
        Self {
            granted,
            wake,
            task,
        }
    }
}
impl Drop for Queued {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Slot {
    /// The reservation an Open captures under: the one this channel's turn
    /// was granted, or one taken now when nobody waits ahead. `None` puts
    /// the channel in the queue (or keeps its place) and the Open answers
    /// `Busy`.
    fn admit(&mut self, budget: &Arc<Budget>) -> Option<capture::Reservation> {
        if let Some(queued) = &self.queued {
            let granted = queued.granted.borrow_mut().take();
            if granted.is_some() {
                self.queued = None;
            }
            return granted;
        }
        let reservation = budget.try_reserve();
        if reservation.is_none() {
            self.queued = Some(Queued::start(budget));
        }
        reservation
    }
    /// The reader reads no more on this channel: give up its place in the
    /// byte budget's queue, or the workspace its turn was granted.
    fn leave(&mut self) {
        self.queued = None;
    }
    /// Close the channel once an unacknowledged capture outlived its
    /// deadline; refuse every request on a closed channel.
    fn expire(&mut self) -> Result<(), String> {
        if self.active.as_mut().is_some_and(|active| {
            active
                .capture
                .check_deadline(std::time::Instant::now())
                .is_err()
        }) {
            self.closed = true;
        }
        if self.closed {
            self.active = None;
            self.queued = None;
            return Err("committed read channel expired".into());
        }
        Ok(())
    }
    /// The revision this channel handed out, when it is `known`: changes
    /// start only from what this channel itself handed out.
    fn held(&self, known: [u8; 32]) -> Option<HeldRevision> {
        self.delivered.clone().filter(|held| held.revision == known)
    }
    /// Drop a capture that answered a held `Await` the reader gave up before
    /// the answer came: the reader asks again, and never saw it.
    fn drop_unseen(&mut self) {
        if self.active.as_ref().is_some_and(|active| active.unseen) {
            self.active = None;
        }
    }
    /// The next piece of the active capture, re-checking custody and the
    /// owner's authorization before it is handed out.
    fn next(&mut self, capture: u64, offset: u32, token: [u8; 16]) -> Result<Reply, String> {
        let active = self
            .active
            .as_mut()
            .ok_or("committed read capture unavailable")?;
        if active.capture.id != capture {
            return Err("committed read capture mismatch".into());
        }
        active.unseen = false;
        active.context.revalidate()?;
        active.service.authorize(&active.context)?;
        active.capture.chunk(offset, token)
    }
    /// Acknowledge (or abandon) the active capture with its latest token.
    /// An entirely read capture becomes what this channel handed out.
    fn acknowledge(&mut self, capture: u64, token: [u8; 16]) -> Result<Reply, String> {
        if !self.active.as_ref().is_some_and(|active| {
            active.capture.id == capture && active.capture.acknowledges(token)
        }) {
            return Err("committed read acknowledgement mismatch".into());
        }
        if let Some(held) = self
            .active
            .as_ref()
            .and_then(|active| active.capture.delivers())
        {
            self.delivered = Some(held);
        }
        self.active = None;
        Ok(Reply::Cancelled)
    }
    /// End a held `Await`: the caller asked something newer.
    fn supersede_wait(&mut self) {
        if let Some(wait) = self.wait.take() {
            wait.wake.notify_one();
        }
    }
}
impl JoinedChannel {
    fn arm_capture_deadline(&self, id: u64) {
        let weak = Rc::downgrade(&self.committed_read);
        tokio::task::spawn_local(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            if let Some(slot) = weak.upgrade() {
                let mut slot = slot.borrow_mut();
                if slot
                    .active
                    .as_ref()
                    .is_some_and(|active| active.capture.id == id)
                {
                    slot.closed = true;
                    slot.active = None;
                }
            }
        });
    }
    pub(super) fn document_read_context(&self) -> Result<NativeDocumentReadContext, String> {
        self.read_context(false)
    }
    /// Whether this channel's workspace seals every write
    /// ([`NativeDocumentReads::seals_records`]).
    pub(super) fn seals_records(&self) -> bool {
        let route = self.route();
        self.state
            .services
            .committed_documents
            .as_ref()
            .is_some_and(|service| service.seals_records(&route.cluster_id))
    }
    /// Who answers this channel's read of its own record.
    fn record_read(&self) -> RecordRead {
        let route = self.record_route();
        self.state
            .services
            .committed_documents
            .as_ref()
            .map_or(RecordRead::Own, |service| {
                service.record_read(&route.cluster_id, &route.app_id, &route.record_id)
            })
    }
    /// The context this channel reads its own record with, as
    /// [`Self::record_read`] decides.
    fn own_record_context(&self) -> Result<NativeDocumentReadContext, String> {
        self.read_context(self.record_read() == RecordRead::Sealed)
    }
    /// The read context under this channel's custody. A `sealed` read of the
    /// channel's own record needs no launcher custody when the app has
    /// none: an app the person opened directly (an SDK app the runtime
    /// starts) reads its sealed record under its channel's admission, as its
    /// own sealed channel does. An agent never reads that way.
    fn read_context(&self, sealed: bool) -> Result<NativeDocumentReadContext, String> {
        // The record the channel carries: a renamed app reads the one of the id it had before.
        let route = self.record_route();
        let admitted = self.verified_identity.clone();
        let certificate = admitted
            .certificate
            .clone()
            .ok_or("committed read requires a certified caller")?;
        let local = self
            .lease
            .as_ref()
            .and_then(|lease| lease.local_media.clone());
        let peer = self.peer_id;
        let workspace_record = local
            .as_ref()
            .and_then(|local| launch_workspace_record(local, &route));
        let creation = local
            .as_ref()
            .filter(|_| admitted.agent_id.is_none())
            .and_then(|local| local.creation.clone())
            .map(|evidence| match workspace_record {
                // A workspace record's kind is declared by the platform's
                // list of them, not by the package of the window that
                // opens it; the launch's signed original still stands for
                // who creates it, and the workspace's grants decide whether they
                // may.
                Some(record) => crate::services::ApplicationCreationEvidence {
                    declared_kinds: vec![record.kind.to_owned()],
                    ..evidence
                },
                None => evidence,
            });
        let state = Arc::downgrade(&self.state);
        let checked_route = route.clone();
        let revalidate = Arc::new(move || {
            let state = state.upgrade().ok_or("committed read broker ended")?;
            if !state.is_accepting() {
                return Err("committed read broker stopped".into());
            }
            if let Some(scope) = &admitted.host_scope {
                if !state.is_current_host_worker(peer, &checked_route, &scope.certificate)? {
                    return Err("committed read worker ended".into());
                }
            } else if let Some(local) = local.as_ref() {
                // The launch's own routes, or a record every app of its
                // workspace opens: the owner's grants decide the rest, below and
                // in the owner's own authorization.
                if !local.current()
                    || (!local.keeps_records_of(&checked_route.app_id)
                        && launch_workspace_record(local, &checked_route).is_none())
                    || local.workspace_id != checked_route.cluster_id
                    || admitted.agent_id.is_some()
                {
                    return Err("committed read launcher custody ended".into());
                }
            } else if !sealed || admitted.agent_id.is_some() {
                return Err("committed read requires launcher custody".into());
            }
            state
                .revalidate_admission(&checked_route, &admitted)
                .map(|_| ())
                .map_err(|_| "committed read admission ended".into())
        });
        let context = NativeDocumentReadContext {
            route,
            certificate,
            record_kind: self.record_kind.clone(),
            creation,
            revalidate,
        };
        context.revalidate()?;
        Ok(context)
    }
    pub(super) fn native_private_window_instance(&self) -> Result<Option<u64>, String> {
        let route = self.record_route();
        if !self
            .state
            .services
            .committed_documents
            .as_ref()
            .is_some_and(|service| service.requires_workspace(&route.cluster_id))
        {
            return Ok(None);
        }
        let Some(local) = self
            .lease
            .as_ref()
            .and_then(|lease| lease.local_media.as_ref())
            .filter(|_| self.verified_identity.agent_id.is_none())
            .filter(|local| local.continuation_instance.is_some())
        else {
            return Ok(None);
        };
        // Another app's workspace record is no record of this window's own:
        // it carries no private binding.
        if local.current()
            && !local.keeps_records_of(&route.app_id)
            && launch_workspace_record(local, &route).is_some()
        {
            return Ok(None);
        }
        if !local.current()
            || !local.keeps_records_of(&route.app_id)
            || local.workspace_id != route.cluster_id
        {
            return Err("native private launch custody ended".into());
        }
        let context = self.document_read_context()?;
        self.state
            .services
            .committed_documents
            .as_ref()
            .ok_or("native document owner unavailable")?
            .authorize(&context)?;
        context.revalidate()?;
        Ok(local
            .continuation_instance
            .filter(|instance| *instance != 0))
    }

    pub(super) fn continuation_instance(&self) -> Result<u64, String> {
        if self
            .state
            .services
            .committed_documents
            .as_ref()
            .is_some_and(|service| service.requires_workspace(&self.route().cluster_id))
        {
            return self
                .native_private_window_instance()?
                .ok_or_else(|| "native launch has no private window-instance binding".into());
        }
        self.route()
            .record_id
            .strip_prefix("continuation:")
            .ok_or_else(|| "continuation requires a durable shell tab id".to_owned())?
            .parse::<u64>()
            .map_err(|error| format!("continuation names no durable shell tab id: {error}"))
    }

    /// For an Open this channel's record would answer as new
    /// ([`RecordRead::Own`]): the wait before deciding, see
    /// [`NativeDocumentReads::await_sealed_history`]. `None` for every other
    /// request, and when nothing is awaited.
    pub(super) fn committed_history_wait(&self, request: &Request) -> Option<AwaitingHistory> {
        if !matches!(request, Request::Open { .. }) || self.record_read() != RecordRead::Own {
            return None;
        }
        let service = self.state.services.committed_documents.clone()?;
        let context = self.document_read_context().ok()?;
        service.await_sealed_history(&context)
    }

    /// Answer a committed-document request.
    pub(super) fn committed_document_dispatch(&self, request: &Request) -> Result<Reply, String> {
        if matches!(request, Request::ConnectionSnapshot) {
            // The connection as its record names it: the channel's own route.
            let route = self.record_route();
            let current = self
                .state
                .revalidate_admission(&route, &self.verified_identity)
                .map_err(|error| error.to_string())?;
            let certificate = current
                .certificate
                .as_ref()
                .ok_or("connection snapshot requires a certified principal")?;
            let snapshot = runtime_collab::committed_document::ConnectionSnapshot {
                root: certificate.user_public_key,
                device: certificate.device_public_key,
                agent: current.agent_id,
                app_id: route.app_id,
                room: route.cluster_id,
                document: route.record_id,
            };
            if !snapshot.valid() {
                return Err("invalid authenticated connection snapshot".into());
            }
            return Ok(Reply::ConnectionSnapshot(snapshot));
        }
        if matches!(request, Request::Writes) {
            return Ok(Reply::Writes {
                sealed: self.seals_records(),
            });
        }
        if self.record_read() == RecordRead::Own {
            return Ok(Reply::Legacy);
        }
        let mut slot = self.committed_read.borrow_mut();
        slot.expire()?;
        slot.supersede_wait();
        match request {
            Request::ConnectionSnapshot => Err("connection snapshot was not handled".into()),
            Request::Writes => Err("the channel's writes were not answered".into()),
            Request::Await { .. } => Err("a committed read wait is answered asynchronously".into()),
            Request::PrivateWindowBinding => Ok(Reply::PrivateWindowBinding {
                instance: self.native_private_window_instance()?,
            }),
            Request::Initialize { .. } => Err("no record is created through this channel".into()),
            Request::Cancel { capture, token } => slot.acknowledge(*capture, *token),
            Request::Leave => {
                slot.leave();
                Ok(Reply::Cancelled)
            }
            Request::Open { known_revision } => {
                self.open_committed_document(&mut slot, *known_revision, None)
            }
            Request::OpenChanges { known_revision } => {
                let held = slot.held(*known_revision);
                self.open_committed_document(&mut slot, Some(*known_revision), held.as_ref())
            }
            Request::Next {
                capture,
                offset,
                token,
            } => slot.next(*capture, *offset, *token),
        }
    }
    /// An Open for `known_revision`. With `held` (the revision this channel
    /// handed out, equal to `known_revision`) the owner may answer with only
    /// the changes since; every check is the same either way.
    fn open_committed_document(
        &self,
        slot: &mut Slot,
        known_revision: Option<[u8; 32]>,
        held: Option<&HeldRevision>,
    ) -> Result<Reply, String> {
        let Some(service) = self.state.services.committed_documents.clone() else {
            return Ok(Reply::Unavailable);
        };
        let opened = self.own_record_context().and_then(|context| {
            let reading = Reading {
                context,
                service,
                budget: &self.state.committed_read_budget,
                ids: &self.state.committed_read_id,
            };
            open_read(slot, reading, known_revision, held)
        });
        // A refusal is final: a read refused while it waited in the byte
        // budget's queue gives up its place, so reads behind it are served.
        let (reply, capture) = opened.inspect_err(|_| slot.queued = None)?;
        if let Some(id) = capture {
            self.arm_capture_deadline(id);
        }
        Ok(reply)
    }
}

/// The record every app of `local`'s launch workspace opens that `route` names
/// ([`collab_types::workspace_record`]), when it does: the one route of
/// another app a launch reads and commits to, and only in its own workspace.
fn launch_workspace_record(
    local: &super::native_media::LocalAppConnection,
    route: &CrdtRoute,
) -> Option<&'static collab_types::WorkspaceRecord> {
    (local.workspace_id == route.cluster_id)
        .then(|| collab_types::workspace_record(&route.app_id, &route.record_id))
        .flatten()
}

/// What an Open reads with: the channel's checked custody and admission
/// (`context`), its document owner, the shared byte budget and capture ids.
struct Reading<'a> {
    context: NativeDocumentReadContext,
    service: Arc<dyn NativeDocumentReads>,
    budget: &'a Arc<Budget>,
    ids: &'a std::sync::atomic::AtomicU64,
}

/// An Open's answer for `known_revision`, and the id of the capture it
/// started, whose deadline the caller arms. With `held` the owner may answer
/// with only the changes since; every check is the same either way: the
/// owner's authorization, the read under the byte budget, then custody and
/// authorization again.
fn open_read(
    slot: &mut Slot,
    reading: Reading<'_>,
    known_revision: Option<[u8; 32]>,
    held: Option<&HeldRevision>,
) -> Result<(Reply, Option<u64>), String> {
    let Reading {
        context,
        service,
        budget,
        ids,
    } = reading;
    slot.drop_unseen();
    if slot.active.is_some() {
        return Err("committed read requires previous response acknowledgement".into());
    }
    // Custody and admission first: a reader that may no longer read is
    // refused, never queued. Then capacity: a read that must wait its turn
    // has done nothing yet.
    if let Err(refused) = context.revalidate() {
        slot.queued = None;
        return Err(refused);
    }
    let Some(reservation) = slot.admit(budget) else {
        return Ok((Reply::Busy, None));
    };
    service.authorize(&context)?;
    let read = match held {
        Some(held) => service.capture_since(&context, held, MAX_DOCUMENT_BYTES)?,
        None => service.capture(&context, known_revision, MAX_DOCUMENT_BYTES)?,
    };
    context.revalidate()?;
    service.authorize(&context)?;
    slot.absent = matches!(read, NativeDocumentRead::Absent);
    let (revision, body, heads, base) = match read {
        NativeDocumentRead::Unchanged(revision) => {
            return Ok((Reply::Unchanged { revision }, None));
        }
        NativeDocumentRead::Pending => return Ok((Reply::Unavailable, None)),
        NativeDocumentRead::Absent => return Ok((Reply::Absent, None)),
        NativeDocumentRead::Captured {
            revision,
            heads,
            body,
        } => (revision, body, heads, None),
        // Changes start only from the revision this channel handed out.
        NativeDocumentRead::Changes {
            revision,
            heads,
            base,
            body,
        } if held.is_some_and(|held| held.heads == base) => (revision, body, heads, Some(base)),
        NativeDocumentRead::Changes { .. } => {
            return Err("native document changes from a revision never handed out".into());
        }
    };
    let id = ids
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
        .map_err(|_| "committed read ids exhausted")?;
    let mut capture = capture::Capture::new(body, reservation, id, revision)?.naming(heads);
    if let Some(base) = base {
        capture = capture.changes_since(base)?;
    }
    let reply = capture.chunk(0, [0; 16])?;
    slot.active = Some(Active {
        capture,
        context,
        service,
        unseen: false,
    });
    Ok((reply, Some(id)))
}

/// Whether an Open for `known_revision` would answer exactly what the caller
/// holds (`Unchanged` for that revision; for `None` `Absent` when `absent`,
/// else `Unavailable`), with
/// every check the Open makes, in its order: custody and admission
/// (`context`), the owner's authorization, the read under the shared byte
/// budget, then both checks again. `None` when the Open would answer
/// anything else, refusals included; otherwise the checked context.
fn read_unchanged(
    service: &dyn NativeDocumentReads,
    read: RecordRead,
    context: impl FnOnce() -> Result<NativeDocumentReadContext, String>,
    budget: &Arc<Budget>,
    known_revision: Option<[u8; 32]>,
    absent: bool,
) -> Option<NativeDocumentReadContext> {
    if read == RecordRead::Own {
        return None;
    }
    let context = context().ok()?;
    service.authorize(&context).ok()?;
    let unchanged = service
        .unchanged(&context, known_revision)
        .ok()?
        .or_else(|| captured_unchanged(service, &context, budget, known_revision, absent))?;
    context.revalidate().ok()?;
    service.authorize(&context).ok()?;
    unchanged.then_some(context)
}

/// What only a capture can say: whether it answers what the caller holds.
/// Captured under the shared byte budget, as an Open's capture is; `None`
/// when the budget or the capture refuses.
fn captured_unchanged(
    service: &dyn NativeDocumentReads,
    context: &NativeDocumentReadContext,
    budget: &Arc<Budget>,
    known_revision: Option<[u8; 32]>,
    absent: bool,
) -> Option<bool> {
    let _reservation = budget.try_reserve()?;
    Some(
        match service
            .capture(context, known_revision, MAX_DOCUMENT_BYTES)
            .ok()?
        {
            NativeDocumentRead::Unchanged(revision) => Some(revision) == known_revision,
            NativeDocumentRead::Pending => known_revision.is_none() && !absent,
            NativeDocumentRead::Absent => known_revision.is_none() && absent,
            NativeDocumentRead::Captured { .. } | NativeDocumentRead::Changes { .. } => false,
        },
    )
}

/// Re-run `check` whenever `wake` fires (re-`arm`ed before each check, so a
/// change made while it runs is kept), until it says the read changed.
/// `check` answers `Some(announced)` while the read is unchanged: with
/// `announced` false some input can change unannounced, and it re-runs every
/// [`UNANNOUNCED_RECHECK`] as well.
///
/// A sealed record's read also changes when this device's channel `keys`
/// do: a write sealed under an epoch or branch whose key had not reached the
/// device opens once it has, with nothing written to the record.
async fn hold_until_changed(
    wake: &tokio::sync::Notify,
    keys: Option<&ChannelKeyProvider>,
    mut arm: impl FnMut(),
    mut check: impl FnMut() -> Result<Option<bool>, String>,
) -> Result<(), String> {
    // Read before the first check, so keys arriving while it runs are seen.
    let mut keyed = keys.map(ChannelKeyProvider::generation);
    loop {
        arm();
        let Some(announced) = check()? else {
            return Ok(());
        };
        let keys_changed = async move {
            match keys.zip(keyed) {
                Some((keys, after)) => keys.changed(after).await,
                None => std::future::pending().await,
            }
        };
        let changed = async {
            if announced {
                wake.notified().await;
            } else {
                let _ = tokio::time::timeout_at(next_unannounced_recheck(), wake.notified()).await;
            }
        };
        tokio::select! {
            generation = keys_changed => keyed = Some(generation),
            () = changed => {}
        }
    }
}

/// The next instant on one process-wide [`UNANNOUNCED_RECHECK`] grid, at
/// most that far ahead. Every clocked wait re-checks on the same ticks, so
/// however many readers wait, the broker thread wakes five times a second,
/// not five times a second per reader.
pub(super) fn next_unannounced_recheck() -> tokio::time::Instant {
    static GRID: std::sync::OnceLock<tokio::time::Instant> = std::sync::OnceLock::new();
    let now = tokio::time::Instant::now();
    let origin = *GRID.get_or_init(|| now);
    let period = UNANNOUNCED_RECHECK.as_nanos();
    let until = now.checked_duration_since(origin).map_or(period, |since| {
        period.saturating_sub(since.as_nanos().checked_rem(period).unwrap_or(0))
    });
    let until = Duration::from_nanos(u64::try_from(until).unwrap_or(u64::MAX));
    now.checked_add(until)
        .unwrap_or_else(|| now.checked_add(UNANNOUNCED_RECHECK).unwrap_or(now))
}

/// Whether `reply` is what a reader holding `known` holds already: its own
/// revision unchanged, or for `None` the `Unavailable` or `Absent` (as
/// `absent` says) the channel last answered.
fn holds_what_it_answers(reply: &Reply, known: Option<[u8; 32]>, absent: bool) -> bool {
    match (reply, known) {
        (Reply::Unchanged { revision }, Some(known)) => *revision == known,
        (Reply::Unavailable, None) => !absent,
        (Reply::Absent, None) => absent,
        _ => false,
    }
}

/// Ends the slot's record of a held wait when the wait is dropped (answered,
/// cancelled by its caller, or its connection gone), unless a newer request
/// already replaced it.
struct WaitGuard<'a> {
    handle: &'a JoinedChannel,
    generation: u64,
}
impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.handle.committed_read.try_borrow_mut()
            && slot
                .wait
                .as_ref()
                .is_some_and(|wait| wait.generation == self.generation)
        {
            slot.wait = None;
        }
    }
}

impl JoinedChannel {
    /// Answer an `Await { known_revision }`: hold until an
    /// `Open { known_revision }` on this channel (an `OpenChanges` when the
    /// caller holds a revision) would answer anything but what the caller
    /// holds, then answer with what that Open answers: the changed record
    /// goes to the reader with the wake.
    ///
    /// The checks are the Open's own (custody, admission, the owner's
    /// authorization and capture), re-run whenever something they read may
    /// have changed: every writer of those inputs announces
    /// ([`tilo_identity_host::policy_change`]), and the first instant a
    /// time-bound input changes is registered with `wake_at`. Where that
    /// cannot be relied on, they re-run every [`UNANNOUNCED_RECHECK`], as the
    /// reader's poll did. A check that fails ends the hold too, and the Open
    /// meets the same refusal: a revocation ends the reader as it always did.
    #[allow(
        clippy::future_not_send,
        reason = "channel handles are Rc-based; driven only on the broker LocalSet"
    )]
    pub(super) async fn await_committed_change(
        self: Rc<Self>,
        known_revision: Option<[u8; 32]>,
    ) -> Result<Reply, String> {
        let wake = Arc::new(tokio::sync::Notify::new());
        let (generation, turn) = {
            let mut slot = self.committed_read.borrow_mut();
            slot.expire()?;
            slot.drop_unseen();
            if slot.active.is_some() {
                return Err(
                    "committed read wait requires previous response acknowledgement".into(),
                );
            }
            slot.supersede_wait();
            slot.waits = slot.waits.wrapping_add(1);
            let generation = slot.waits;
            slot.wait = Some(HeldWait {
                generation,
                wake: Arc::clone(&wake),
            });
            let turn = slot
                .queued
                .as_ref()
                .map(|queued| (Rc::clone(&queued.granted), Rc::clone(&queued.wake)));
            (generation, turn)
        };
        let _guard = WaitGuard {
            handle: &self,
            generation,
        };
        self.state
            .follow_committed_writes(&self.record_route().channel_key(), &wake);
        let watch = {
            let wake = Arc::clone(&wake);
            tilo_identity_host::policy_change::Watch::new(move || wake.notify_one())
        };
        if let Some((granted, turn)) = turn {
            // A read waiting in the budget's queue: what changes its Open's
            // answer is its turn coming, announced by the queue itself, or
            // its custody or admission ending, announced as any policy
            // change is. Either way the Open decides now.
            loop {
                if !self.holds_wait(generation) {
                    return Err("committed read wait was replaced".into());
                }
                let _ = watch.arm();
                if granted.borrow().is_some()
                    || self
                        .document_read_context()
                        .and_then(|context| context.revalidate())
                        .is_err()
                {
                    return self.answer_wait(known_revision);
                }
                tokio::select! {
                    () = turn.notified() => {}
                    () = wake.notified() => {}
                }
            }
        }
        let keys = (self.record_read() == RecordRead::Sealed)
            .then(|| self.state.services.channel_keys.clone())
            .flatten();
        loop {
            hold_until_changed(
                &wake,
                keys.as_ref(),
                || {
                    let _ = watch.arm();
                },
                || {
                    if !self.holds_wait(generation) {
                        return Err("committed read wait was replaced".into());
                    }
                    Ok(self.committed_read_unchanged(known_revision))
                },
            )
            .await?;
            let absent = self.committed_read.borrow().absent;
            let reply = self.answer_wait(known_revision)?;
            if !holds_what_it_answers(&reply, known_revision, absent) {
                return Ok(reply);
            }
            // The check and the Open disagreed: look again on the next
            // announcement or tick, not at once.
            let _ = tokio::time::timeout_at(next_unannounced_recheck(), wake.notified()).await;
        }
    }

    /// What the Open a held wait stands for answers now: `OpenChanges` for a
    /// revision this channel handed out, otherwise `Open`. A capture it
    /// starts is the reader's to read on.
    fn answer_wait(&self, known_revision: Option<[u8; 32]>) -> Result<Reply, String> {
        if self.record_read() == RecordRead::Own {
            return Ok(Reply::Legacy);
        }
        let mut slot = self.committed_read.borrow_mut();
        slot.expire()?;
        let held = known_revision.and_then(|known| slot.held(known));
        let reply = self.open_committed_document(&mut slot, known_revision, held.as_ref())?;
        if let Some(active) = slot.active.as_mut() {
            active.unseen = true;
        }
        Ok(reply)
    }

    fn holds_wait(&self, generation: u64) -> bool {
        self.committed_read.try_borrow().is_ok_and(|slot| {
            !slot.closed
                && slot.active.is_none()
                && slot
                    .wait
                    .as_ref()
                    .is_some_and(|wait| wait.generation == generation)
        })
    }

    /// [`read_unchanged`] on this channel: `Some(announced)` while an Open
    /// would answer exactly what the caller holds, where `announced` says
    /// whether every input that can change that is announced or timed.
    fn committed_read_unchanged(&self, known_revision: Option<[u8; 32]>) -> Option<bool> {
        let service = self.state.services.committed_documents.clone()?;
        let absent = self.committed_read.try_borrow().ok()?.absent;
        let context = read_unchanged(
            service.as_ref(),
            self.record_read(),
            || self.own_record_context(),
            &self.state.committed_read_budget,
            known_revision,
            absent,
        )?;
        Some(self.register_read_expiry(&context, service.as_ref()))
    }

    /// Register with [`tilo_identity_host::policy_change::wake_at`] the first
    /// instant at which this read's checks can answer differently by time
    /// alone: the reader's certificate validity, the membership tree's next
    /// grant expiry, and what the document owner names. Returns `false` when
    /// any input is neither announced nor timed, and the wait keeps its
    /// clock.
    fn register_read_expiry(
        &self,
        context: &NativeDocumentReadContext,
        service: &dyn NativeDocumentReads,
    ) -> bool {
        use tilo_identity_host::policy_change;
        if !policy_change::timers_reliable() || !policy_change::identity_files_watched() {
            return false;
        }
        if self
            .state
            .membership
            .as_ref()
            .is_some_and(|membership| !membership.announces_policy_changes())
        {
            return false;
        }
        let Ok(now) = self.state.policy_now_ns() else {
            return false;
        };
        let Ok(owner) = service.next_change(context, now / 1_000_000_000) else {
            return false;
        };
        let Ok((membership, _)) = self.state.next_policy_change(now) else {
            return false;
        };
        let certificate = context.certificate();
        let certificate = [Some(certificate.issued_at_ns), certificate.expires_at_ns]
            .into_iter()
            .flatten()
            .filter(|at| *at > now)
            .min();
        let next = owner
            .map(|seconds| seconds.saturating_mul(1_000_000_000))
            .into_iter()
            .chain(membership)
            .chain(certificate)
            .min();
        if let Some(at) = next {
            crate::policy_schedule::announce_at(crate::policy_schedule::wall_instant(
                at,
                now,
                tilo_clock::unix_now_ns(),
            ));
        }
        true
    }
}

impl super::AppBrokerState {
    /// A write another device made in `record` of `app` in `workspace` landed here: what a read
    /// holding that record open sees changed, and what nothing else announces.
    pub fn wake_record_reads(&self, workspace: &str, app: &str, record: &str) {
        self.wake_committed_reads(&CrdtRoute::record(app, workspace, record));
    }

    /// Wake the held committed-read waits of every channel `route`
    /// delivers to: a write on it changed what their reads answer (a sealed
    /// record's read is made of the writes this broker holds), and nothing
    /// else announces that. Each wait then checks its read again, and only
    /// its own.
    pub(super) fn wake_committed_reads(&self, route: &CrdtRoute) {
        let Ok(mut waits) = self.committed_waits.lock() else {
            return;
        };
        waits.retain(|channel, held| {
            let delivers = route.delivers_to(&CrdtRoute::from_channel_key(channel));
            held.retain(|wake| wake.landed(delivers));
            !held.is_empty()
        });
    }

    /// Have writes on `channel_id`'s route wake `wake`, a held committed
    /// read's, for as long as the wait holds it.
    pub(super) fn follow_committed_writes(
        &self,
        channel_id: &str,
        wake: &Arc<tokio::sync::Notify>,
    ) {
        self.follow_writes(channel_id, LandedWake::Held(Arc::downgrade(wake)));
    }

    /// Have the next write on `route` wake `wake`, a record reader's
    /// ([`tilo_actions::remote::RecordReads::watch`]).
    pub(super) fn follow_record_writes(
        &self,
        route: &CrdtRoute,
        wake: &tilo_actions::remote::RecordWake,
    ) {
        self.follow_writes(&route.channel_key(), LandedWake::Once(wake.clone()));
    }

    fn follow_writes(&self, channel_id: &str, wake: LandedWake) {
        if let Ok(mut waits) = self.committed_waits.lock() {
            waits.entry(channel_id.to_owned()).or_default().push(wake);
        }
    }
}

/// Who a write landing on a channel wakes.
pub(super) enum LandedWake {
    /// A held committed read, for as long as it holds its wait.
    Held(Weak<tokio::sync::Notify>),
    /// A record reader, once.
    Once(tilo_actions::remote::RecordWake),
}

impl LandedWake {
    /// A write landed, on this wake's channel when `delivers`: wake it then.
    /// Whether it still waits for another.
    fn landed(&self, delivers: bool) -> bool {
        match self {
            Self::Held(wake) => {
                let Some(wake) = wake.upgrade() else {
                    return false;
                };
                if delivers {
                    wake.notify_one();
                }
                true
            }
            Self::Once(wake) => {
                if delivers {
                    wake.wake();
                }
                wake.waiting()
            }
        }
    }
}
