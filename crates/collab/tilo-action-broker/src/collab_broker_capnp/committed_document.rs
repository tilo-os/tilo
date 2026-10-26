//! Current committed reads on the actual admitted channel.
mod capture;
use super::{Arc, CapnpChannelHandle, CrdtRoute, Duration, Ordering, Rc};
pub(super) use capture::Budget;
pub use capture::DocumentBytes;
use runtime_collab::committed_document::{
    CreationReceipt, CreationWorkspace, MAX_DOCUMENT_BYTES, Reply, Request,
};

/// What [`NativeDocumentReads::prepare_application_elsewhere`] answers: the
/// creation another device made for this caller, or `None` when the record
/// was already there.
pub type PreparedElsewhere = Result<Option<CreationWorkspace>, String>;
/// A creation another device makes, awaited on the broker's local set.
pub type PreparingElsewhere =
    std::pin::Pin<Box<dyn std::future::Future<Output = PreparedElsewhere>>>;

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
#[path = "committed_document/initialization_tests.rs"]
mod initialization_tests;
#[cfg(test)]
#[path = "committed_document/service_session_tests.rs"]
mod service_session_tests;
impl NativeDocumentReadContext {
    /// Bind a read to a source record while retaining the original channel's
    /// launcher custody and live admission check. Only the broker can do so.
    pub(super) fn for_source(mut self, app: &str, record: &str) -> Self {
        app.clone_into(&mut self.route.app_id);
        record.clone_into(&mut self.route.record_id);
        self
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
    /// The record this Open created for the caller, if any: decided by
    /// `service` itself, or by the device that `prepared` the record
    /// elsewhere before the Open ran.
    fn prepare_application(
        &self,
        service: &dyn NativeDocumentReads,
        prepared: Option<PreparedElsewhere>,
    ) -> Result<Option<CreationWorkspace>, String> {
        if self.application_creation().is_some() {
            let created = match prepared {
                Some(prepared) => prepared?,
                None => service.prepare_application(self)?,
            };
            self.revalidate()?;
            return Ok(created);
        }
        Ok(None)
    }
    /// Re-checks the admitted channel's live custody and admission.
    ///
    /// # Errors
    ///
    /// Returns a message once the broker, worker, launcher custody or admission ended.
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
pub trait NativeDocumentReads: Send + Sync {
    /// Durable native installation obligation, independent of runtime health.
    fn requires_room(&self, room: &str) -> bool;
    /// Prepare only a missing record for a live, original-package-attested
    /// person launch. This is not a read/capture fallback or a wire capability.
    ///
    /// # Errors
    /// Creator custody, the SDK's declared kind or durable publication fails.
    fn prepare_application(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Result<Option<CreationWorkspace>, String> {
        let _ = context;
        Ok(None)
    }
    /// Where another device must create a missing record (the host that
    /// governs the room): the future that asks it on the Open's behalf and
    /// resolves once the record is in this owner's room. It answers the
    /// creation workspace when it created the record for this caller, which
    /// then initializes it, and `None` when the record was already there.
    /// The broker awaits it without blocking the channel, then answers the
    /// Open with it in place of [`Self::prepare_application`]. `None` (no
    /// future) when this owner decides creation itself.
    fn prepare_application_elsewhere(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Option<PreparingElsewhere> {
        let _ = context;
        None
    }
    /// Commit bootstrap only after the broker checks the original creation receipt.
    /// # Errors
    /// Refuses unavailable custody, changed revision or failed durable publication.
    fn initialize_application(
        &self,
        context: &NativeDocumentReadContext,
        revision: [u8; 32],
        kind: &str,
        document: &[u8],
    ) -> Result<[u8; 32], String> {
        let _ = (context, revision, kind, document);
        Err("native application initialization unavailable".into())
    }
    /// Must check exact current selected custody, caller certificate, and native
    /// document Read permission. Called again before every returned chunk.
    ///
    /// # Errors
    ///
    /// Returns a message when the caller may not read the document now.
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
    ///
    /// # Errors
    ///
    /// Returns a message when the caller may not read the document now.
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
    /// then re-checks every [`UNANNOUNCED_RECHECK`], the cadence readers
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
}
struct Initialization {
    receipt: CreationReceipt,
    revision: [u8; 32],
    context: NativeDocumentReadContext,
    service: Arc<dyn NativeDocumentReads>,
    deadline: std::time::Instant,
    completed: Option<([u8; 32], [u8; 32])>,
    acknowledged: bool,
    capture_id: u64,
}
impl Initialization {
    fn commit(
        &mut self,
        token: [u8; 16],
        revision: [u8; 32],
        kind: &str,
        document: &[u8],
    ) -> Result<Reply, String> {
        if !self.acknowledged
            || token != self.receipt.token
            || revision != self.revision
            || self.context.record_kind() != Some(kind)
            || std::time::Instant::now() >= self.deadline
        {
            return Err("application initialization receipt mismatch or expired".into());
        }
        self.context.revalidate()?;
        self.service.authorize(&self.context)?;
        let digest = *blake3::hash(document).as_bytes();
        if let Some((original, committed)) = self.completed {
            return if original == digest {
                Ok(Reply::Initialized {
                    revision: committed,
                })
            } else {
                Err("application initialization receipt already consumed".into())
            };
        }
        let committed =
            self.service
                .initialize_application(&self.context, revision, kind, document)?;
        self.context.revalidate()?;
        self.completed = Some((digest, committed));
        Ok(Reply::Initialized {
            revision: committed,
        })
    }
}
/// A held `Await` on this channel; a newer request on the channel ends it.
struct HeldWait {
    generation: u64,
    wake: Arc<tokio::sync::Notify>,
}
#[derive(Default)]
pub(super) struct Slot {
    active: Option<Active>,
    initialization: Option<Initialization>,
    closed: bool,
    wait: Option<HeldWait>,
    waits: u64,
    /// The last revision this channel handed out whole or as changes (its
    /// last piece acknowledged), when its owner named the heads.
    delivered: Option<HeldRevision>,
    /// This channel's place in the byte budget's queue, after an Open found
    /// the budget held and answered `Busy`.
    queued: Option<Queued>,
    /// The record another device prepared for an Open that then answered
    /// `Busy`: the next Open answers with it, so a creation is never lost.
    prepared: Option<PreparedElsewhere>,
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
    /// byte budget's queue, or the room its turn was granted.
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
        let finished = self
            .active
            .as_ref()
            .is_some_and(|active| active.capture.finished());
        if let Some(initialization) = self.initialization.as_mut() {
            initialization.acknowledged |= finished && initialization.capture_id == capture;
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
    fn creation_reply(
        &mut self,
        reply: Reply,
        workspace: Option<CreationWorkspace>,
        context: &NativeDocumentReadContext,
        service: &Arc<dyn NativeDocumentReads>,
    ) -> Result<Reply, String> {
        let Some(workspace) = workspace else {
            return Ok(reply);
        };
        if self.initialization.is_some()
            || !workspace.valid()
            || workspace.id != context.route().cluster_id
        {
            return Err("invalid repeated application creation receipt".into());
        }
        let chunk = runtime_collab::committed_document::CreationChunk::from_reply(reply)
            .map_err(str::to_owned)?;
        let receipt = CreationReceipt {
            token: uuid::Uuid::new_v4().into_bytes(),
            workspace,
        };
        self.initialization = Some(Initialization {
            receipt: receipt.clone(),
            revision: chunk.revision,
            context: context.clone(),
            service: service.clone(),
            deadline: std::time::Instant::now()
                .checked_add(Duration::from_secs(30))
                .ok_or("initialization deadline overflow")?,
            completed: None,
            acknowledged: false,
            capture_id: chunk.capture,
        });
        Ok(Reply::Created { receipt, chunk })
    }
}
impl CapnpChannelHandle {
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
        let route = self.route();
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
        let creation = local
            .as_ref()
            .filter(|_| admitted.agent_id.is_none())
            .and_then(|local| local.creation.clone());
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
            } else {
                let local = local
                    .as_ref()
                    .ok_or("committed read requires launcher custody")?;
                if !local.current()
                    || local.app_id != checked_route.app_id
                    || local.workspace_id != checked_route.cluster_id
                    || admitted.agent_id.is_some()
                {
                    return Err("committed read launcher custody ended".into());
                }
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
        let route = self.route();
        if !self
            .state
            .services
            .committed_documents
            .as_ref()
            .is_some_and(|service| service.requires_room(&route.cluster_id))
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
        if !local.current()
            || local.app_id != route.app_id
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
            .is_some_and(|service| service.requires_room(&self.route().cluster_id))
        {
            return self
                .native_private_window_instance()?
                .ok_or_else(|| "native launch has no private window-instance binding".into());
        }
        self.route()
            .record_id
            .strip_prefix("continuation:")
            .and_then(|id| id.parse::<u64>().ok())
            .ok_or_else(|| "continuation requires a durable shell tab id".into())
    }

    /// For an Open of a record another device creates (the host governing
    /// the room), the creation to await before the Open answers: see
    /// [`NativeDocumentReads::prepare_application_elsewhere`]. `None` for
    /// every other request, and whenever the Open decides creation itself.
    pub(super) fn committed_creation_elsewhere(
        &self,
        request: &Request,
    ) -> Option<PreparingElsewhere> {
        if !matches!(request, Request::Open { .. }) {
            return None;
        }
        let service = self.state.services.committed_documents.clone()?;
        if !service.requires_room(&self.route().cluster_id) {
            return None;
        }
        let context = self.document_read_context().ok()?;
        context.application_creation()?;
        service.prepare_application_elsewhere(&context)
    }

    pub(super) fn committed_document_dispatch(&self, request: &Request) -> Result<Reply, String> {
        self.committed_document_dispatch_prepared(request, None)
    }

    /// [`Self::committed_document_dispatch`], with an Open's record already
    /// `prepared` by the device that creates it.
    pub(super) fn committed_document_dispatch_prepared(
        &self,
        request: &Request,
        prepared: Option<PreparedElsewhere>,
    ) -> Result<Reply, String> {
        if matches!(request, Request::ConnectionSnapshot) {
            let route = self.route();
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
        let required = self
            .state
            .services
            .committed_documents
            .as_ref()
            .is_some_and(|service| service.requires_room(&self.route().cluster_id));
        if !required {
            return Ok(Reply::Legacy);
        }
        let mut slot = self.committed_read.borrow_mut();
        slot.expire()?;
        slot.supersede_wait();
        match request {
            Request::ConnectionSnapshot => Err("connection snapshot was not handled".into()),
            Request::Await { .. } => Err("a committed read wait is answered asynchronously".into()),
            Request::PrivateWindowBinding => Ok(Reply::PrivateWindowBinding {
                instance: self.native_private_window_instance()?,
            }),
            Request::Initialize {
                token,
                revision,
                kind,
                document,
            } => {
                if slot.active.is_some() {
                    return Err("initialization requires capture acknowledgement".into());
                }
                slot.initialization
                    .as_mut()
                    .ok_or("application creation receipt unavailable")?
                    .commit(*token, *revision, kind, document)
            }
            Request::Cancel { capture, token } => slot.acknowledge(*capture, *token),
            Request::Leave => {
                slot.leave();
                Ok(Reply::Cancelled)
            }
            Request::Open { known_revision } => {
                self.open_committed_document(&mut slot, *known_revision, None, prepared)
            }
            Request::OpenChanges { known_revision } => {
                let held = slot.held(*known_revision);
                self.open_committed_document(&mut slot, Some(*known_revision), held.as_ref(), None)
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
        prepared: Option<PreparedElsewhere>,
    ) -> Result<Reply, String> {
        if slot.active.is_some() {
            return Err("committed read requires previous response acknowledgement".into());
        }
        let Some(service) = self.state.services.committed_documents.clone() else {
            return Ok(Reply::Unavailable);
        };
        let opened = self.document_read_context().and_then(|context| {
            let reading = Reading {
                context,
                service,
                budget: &self.state.committed_read_budget,
                ids: &self.state.committed_read_id,
                prepared,
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

/// What an Open reads with: the channel's checked custody and admission
/// (`context`), its document owner, the shared byte budget and capture ids.
struct Reading<'a> {
    context: NativeDocumentReadContext,
    service: Arc<dyn NativeDocumentReads>,
    budget: &'a Arc<Budget>,
    ids: &'a std::sync::atomic::AtomicU64,
    /// The record as the device that creates it elsewhere prepared it
    /// before this Open, if one does.
    prepared: Option<PreparedElsewhere>,
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
        prepared,
    } = reading;
    if slot.active.is_some() {
        return Err("committed read requires previous response acknowledgement".into());
    }
    // Custody and admission first: a reader that may no longer read is
    // refused, never queued. Then capacity: a read that must wait its turn
    // has done nothing yet, and a record prepared elsewhere for it is kept
    // for its next Open.
    if let Err(refused) = context.revalidate() {
        slot.queued = None;
        return Err(refused);
    }
    let Some(reservation) = slot.admit(budget) else {
        if let Some(prepared) = prepared {
            slot.prepared = Some(prepared);
        }
        return Ok((Reply::Busy, None));
    };
    let prepared = prepared.or_else(|| slot.prepared.take());
    let created = context.prepare_application(service.as_ref(), prepared)?;
    service.authorize(&context)?;
    let read = match held {
        Some(held) => service.capture_since(&context, held, MAX_DOCUMENT_BYTES)?,
        None => service.capture(&context, known_revision, MAX_DOCUMENT_BYTES)?,
    };
    context.revalidate()?;
    service.authorize(&context)?;
    let (revision, body, heads, base) = match read {
        NativeDocumentRead::Unchanged(revision) => {
            return Ok((Reply::Unchanged { revision }, None));
        }
        // A record created by this Open is answered with its capture or
        // not at all; only an existing record may be read again later.
        NativeDocumentRead::Pending if created.is_none() => return Ok((Reply::Unavailable, None)),
        NativeDocumentRead::Pending => return Err("created record capture unavailable".into()),
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
        } if created.is_none() && held.is_some_and(|held| held.heads == base) => {
            (revision, body, heads, Some(base))
        }
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
    let reply = slot.creation_reply(capture.chunk(0, [0; 16])?, created, &context, &service)?;
    slot.active = Some(Active {
        capture,
        context,
        service,
    });
    Ok((reply, Some(id)))
}

/// Whether an Open for `known_revision` would answer exactly what the caller
/// holds (`Unchanged` for that revision, `Unavailable` for `None`), with
/// every check the Open makes, in its order: custody and admission
/// (`context`), the owner's authorization, the read under the shared byte
/// budget, then both checks again. `None` when the Open would answer
/// anything else, refusals included; otherwise the checked context.
fn read_unchanged(
    service: &dyn NativeDocumentReads,
    context: impl FnOnce() -> Result<NativeDocumentReadContext, String>,
    budget: &Arc<Budget>,
    room: &str,
    known_revision: Option<[u8; 32]>,
) -> Option<NativeDocumentReadContext> {
    if !service.requires_room(room) {
        return None;
    }
    let context = context().ok()?;
    service.authorize(&context).ok()?;
    let unchanged = service
        .unchanged(&context, known_revision)
        .ok()?
        .or_else(|| captured_unchanged(service, &context, budget, known_revision))?;
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
) -> Option<bool> {
    let _reservation = budget.try_reserve()?;
    Some(
        match service
            .capture(context, known_revision, MAX_DOCUMENT_BYTES)
            .ok()?
        {
            NativeDocumentRead::Unchanged(revision) => Some(revision) == known_revision,
            NativeDocumentRead::Pending => known_revision.is_none(),
            NativeDocumentRead::Captured { .. } | NativeDocumentRead::Changes { .. } => false,
        },
    )
}

/// Re-run `check` whenever `wake` fires (re-`arm`ed before each check, so a
/// change made while it runs is kept), until it says the read changed, and
/// answer [`Reply::Stale`]. `check` answers `Some(announced)` while the read
/// is unchanged: with `announced` false some input can change unannounced,
/// and it re-runs every [`UNANNOUNCED_RECHECK`] as well.
async fn hold_until_stale(
    wake: &tokio::sync::Notify,
    mut arm: impl FnMut(),
    mut check: impl FnMut() -> Result<Option<bool>, String>,
) -> Result<Reply, String> {
    loop {
        arm();
        let Some(announced) = check()? else {
            return Ok(Reply::Stale);
        };
        if announced {
            wake.notified().await;
        } else {
            let _ = tokio::time::timeout_at(next_unannounced_recheck(), wake.notified()).await;
        }
    }
}

/// The next instant on one process-wide [`UNANNOUNCED_RECHECK`] grid, at
/// most that far ahead. Every clocked wait re-checks on the same ticks, so
/// however many readers wait, the broker thread wakes five times a second,
/// not five times a second per reader.
fn next_unannounced_recheck() -> tokio::time::Instant {
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

/// Ends the slot's record of a held wait when the wait is dropped (answered,
/// cancelled by its caller, or its connection gone), unless a newer request
/// already replaced it.
struct WaitGuard<'a> {
    handle: &'a CapnpChannelHandle,
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

impl CapnpChannelHandle {
    /// Answer an `Await { known_revision }`: hold until an
    /// `Open { known_revision }` on this channel would answer anything but
    /// what the caller holds, then answer [`Reply::Stale`].
    ///
    /// The checks are the Open's own (custody, admission, the owner's
    /// authorization and capture), re-run whenever something they read may
    /// have changed: every writer of those inputs announces
    /// ([`tilo_identity_host::policy_change`]), and the first instant a
    /// time-bound input changes is registered with `wake_at`. Where that
    /// cannot be relied on, they re-run every [`UNANNOUNCED_RECHECK`], as the
    /// reader's poll did. A check that fails answers `Stale` too: the
    /// caller's Open then meets the same refusal and fails closed, so a
    /// revocation ends the reader as it always did, only without the poll.
    /// No document byte and no readiness ever comes from this answer.
    ///
    /// # Errors
    ///
    /// The channel expired, a capture is unacknowledged, or a newer request
    /// on this channel replaced the wait.
    #[allow(
        clippy::future_not_send,
        reason = "capnp-rpc channel handles are Rc-based; driven only on the broker LocalSet"
    )]
    pub(super) async fn await_committed_change(
        self: Rc<Self>,
        known_revision: Option<[u8; 32]>,
    ) -> Result<Reply, String> {
        let wake = Arc::new(tokio::sync::Notify::new());
        let generation = {
            let mut slot = self.committed_read.borrow_mut();
            slot.expire()?;
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
        let (generation, turn) = generation;
        let _guard = WaitGuard {
            handle: &self,
            generation,
        };
        let watch = {
            let wake = Arc::clone(&wake);
            tilo_identity_host::policy_change::Watch::new(move || wake.notify_one())
        };
        if let Some((granted, turn)) = turn {
            // A read waiting in the budget's queue: what changes its Open's
            // answer is its turn coming, announced by the queue itself, or
            // its custody or admission ending, announced as any policy
            // change is. Either way its next Open decides.
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
                    return Ok(Reply::Stale);
                }
                tokio::select! {
                    () = turn.notified() => {}
                    () = wake.notified() => {}
                }
            }
        }
        hold_until_stale(
            &wake,
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
        .await
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
        let context = read_unchanged(
            service.as_ref(),
            || self.document_read_context(),
            &self.state.committed_read_budget,
            &self.route().cluster_id,
            known_revision,
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
            policy_change::wake_at(crate::policy_schedule::wall_instant(
                at,
                now,
                tilo_clock::unix_now_ns(),
            ));
        }
        true
    }
}

impl CapnpChannelHandle {
    pub(super) fn resource_view_query(
        self: Rc<Self>,
        correlation: u64,
        query: tilo_actions::remote::CatalogueQuery,
        mut results: super::channel_handle::RemoteActionResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        use capnp::capability::Promise;
        use tilo_actions::remote::RemoteActionFrame;
        let context = query
            .provider
            .as_deref()
            .and_then(|provider| provider.strip_prefix("record:"))
            .and_then(|provider| provider.split_once(':'))
            .ok_or_else(|| "view discovery requires an exact record provider".to_owned())
            .and_then(|(app, record)| {
                self.document_read_context()
                    .map(|context| context.for_source(app, record))
            });
        let service = self.state.services.resource_views.clone();
        Promise::from_future(async move {
            let result = match (context, service) {
                (Ok(context), Some(service)) => service.query(context, query).await,
                (Err(reason), _) => {
                    Err(tilo_actions::remote::ActionHostError::Unavailable { reason })
                }
                (_, None) => Err(tilo_actions::remote::ActionHostError::Unavailable {
                    reason: "the native host cannot discover shared resource views".into(),
                }),
            };
            let answer = tilo_actions::remote::RemoteActionMessage::CatalogueResponse {
                correlation,
                result,
            };
            let bytes = RemoteActionFrame::new(answer)
                .encode()
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            results.get().set_frame(&bytes);
            Ok(())
        })
    }
}
