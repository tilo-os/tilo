//! Current committed reads on the actual admitted channel.
mod capture;
use super::{Arc, CapnpChannelHandle, CrdtRoute, Duration, Ordering, Rc};
pub(super) use capture::Budget;
pub use capture::DocumentBytes;
use runtime_collab::committed_document::{
    CreationReceipt, CreationWorkspace, MAX_DOCUMENT_BYTES, Reply, Request,
};

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
#[path = "committed_document/initialization_tests.rs"]
mod initialization_tests;
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
    fn prepare_application(
        &self,
        service: &dyn NativeDocumentReads,
    ) -> Result<Option<CreationWorkspace>, String> {
        if self.application_creation().is_some() {
            let created = service.prepare_application(self)?;
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
/// Data only. Its provider retains the actual bounded native save's lifetime.
pub enum NativeDocumentRead {
    Unchanged([u8; 32]),
    Captured {
        revision: [u8; 32],
        body: Box<dyn DocumentBytes>,
    },
    /// The owner holds no complete capture right now (a checkpoint it is
    /// admitting has not fully arrived). Answered as [`Reply::Unavailable`]:
    /// the reader withholds readiness and asks again, it is not refused.
    Pending,
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
}
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
#[derive(Default)]
pub(super) struct Slot {
    active: Option<Active>,
    initialization: Option<Initialization>,
    closed: bool,
}
impl Slot {
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

    pub(super) fn committed_document_dispatch(&self, request: &Request) -> Result<Reply, String> {
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
        if slot.active.as_mut().is_some_and(|active| {
            active
                .capture
                .check_deadline(std::time::Instant::now())
                .is_err()
        }) {
            slot.closed = true;
        }
        if slot.closed {
            slot.active = None;
            return Err("committed read channel expired".into());
        }
        match request {
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
            Request::Cancel { capture, token } => {
                if !slot.active.as_ref().is_some_and(|active| {
                    active.capture.id == *capture && active.capture.acknowledges(*token)
                }) {
                    return Err("committed read acknowledgement mismatch".into());
                }
                let finished = slot
                    .active
                    .as_ref()
                    .is_some_and(|active| active.capture.finished());
                if let Some(initialization) = slot.initialization.as_mut() {
                    initialization.acknowledged |=
                        finished && initialization.capture_id == *capture;
                }
                slot.active = None;
                Ok(Reply::Cancelled)
            }
            Request::Open { known_revision } => {
                self.open_committed_document(&mut slot, *known_revision)
            }
            Request::Next {
                capture,
                offset,
                token,
            } => {
                let active = slot
                    .active
                    .as_mut()
                    .ok_or("committed read capture unavailable")?;
                if active.capture.id != *capture {
                    return Err("committed read capture mismatch".into());
                }
                active.context.revalidate()?;
                active.service.authorize(&active.context)?;
                active.capture.chunk(*offset, *token)
            }
        }
    }
    fn open_committed_document(
        &self,
        slot: &mut Slot,
        known_revision: Option<[u8; 32]>,
    ) -> Result<Reply, String> {
        if slot.active.is_some() {
            return Err("committed read requires previous response acknowledgement".into());
        }
        let Some(service) = self.state.services.committed_documents.clone() else {
            return Ok(Reply::Unavailable);
        };
        let context = self.document_read_context()?;
        let created = context.prepare_application(service.as_ref())?;
        service.authorize(&context)?;
        let reservation = self.state.committed_read_budget.reserve()?;
        let read = service.capture(&context, known_revision, MAX_DOCUMENT_BYTES)?;
        context.revalidate()?;
        service.authorize(&context)?;
        match read {
            NativeDocumentRead::Unchanged(revision) => Ok(Reply::Unchanged { revision }),
            // A record created by this Open is answered with its capture or
            // not at all; only an existing record may be read again later.
            NativeDocumentRead::Pending if created.is_none() => Ok(Reply::Unavailable),
            NativeDocumentRead::Pending => Err("created record capture unavailable".into()),
            NativeDocumentRead::Captured { revision, body } => {
                let id = self
                    .state
                    .committed_read_id
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
                    .map_err(|_| "committed read ids exhausted")?;
                let mut capture = capture::Capture::new(body, reservation, id, revision)?;
                let reply =
                    slot.creation_reply(capture.chunk(0, [0; 16])?, created, &context, &service)?;
                slot.active = Some(Active {
                    capture,
                    context,
                    service,
                });
                self.arm_capture_deadline(id);
                Ok(reply)
            }
        }
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
