//! Current committed reads on the actual admitted channel.
mod capture;
use super::{Arc, CapnpChannelHandle, CrdtRoute, Duration, Ordering, Rc};
pub(super) use capture::Budget;
pub use capture::DocumentBytes;
use runtime_collab::committed_document::{MAX_DOCUMENT_BYTES, Reply, Request};

/// No public constructor. The route and original caller are retained from the
/// admitted channel, together with live launcher/host-worker custody.
#[derive(Clone)]
pub struct NativeDocumentReadContext {
    route: CrdtRoute,
    certificate: tilo_user_identity::DeviceCert,
    revalidate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}
impl NativeDocumentReadContext {
    pub fn route(&self) -> &CrdtRoute {
        &self.route
    }
    pub fn certificate(&self) -> &tilo_user_identity::DeviceCert {
        &self.certificate
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
}
pub trait NativeDocumentReads: Send + Sync {
    /// Durable native installation obligation, independent of runtime health.
    fn requires_room(&self, room: &str) -> bool;
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
#[derive(Default)]
pub(super) struct Slot {
    active: Option<Active>,
    closed: bool,
}
impl CapnpChannelHandle {
    fn document_read_context(&self) -> Result<NativeDocumentReadContext, String> {
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
            revalidate,
        };
        context.revalidate()?;
        Ok(context)
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
            return Err("committed read channel expired".into());
        }
        match *request {
            Request::Cancel { capture, token } => {
                if !slot.active.as_ref().is_some_and(|active| {
                    active.capture.id == capture && active.capture.acknowledges(token)
                }) {
                    return Err("committed read acknowledgement mismatch".into());
                }
                slot.active = None;
                Ok(Reply::Cancelled)
            }
            Request::Open { known_revision } => {
                if slot.active.is_some() {
                    return Err("committed read requires previous response acknowledgement".into());
                }
                let Some(service) = self.state.services.committed_documents.clone() else {
                    return Ok(Reply::Unavailable);
                };
                let context = self.document_read_context()?;
                service.authorize(&context)?;
                let reservation = self.state.committed_read_budget.reserve()?;
                let read = service.capture(&context, known_revision, MAX_DOCUMENT_BYTES)?;
                context.revalidate()?;
                service.authorize(&context)?;
                match read {
                    NativeDocumentRead::Unchanged(revision) => Ok(Reply::Unchanged { revision }),
                    NativeDocumentRead::Captured { revision, body } => {
                        let id = self
                            .state
                            .committed_read_id
                            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| {
                                id.checked_add(1)
                            })
                            .map_err(|_| "committed read ids exhausted")?;
                        let mut capture = capture::Capture::new(body, reservation, id, revision)?;
                        let reply = capture.chunk(0, [0; 16])?;
                        {
                            slot.active = Some(Active {
                                capture,
                                context,
                                service,
                            });
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
                                    }
                                }
                            });
                        }
                        Ok(reply)
                    }
                }
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
                if active.capture.id != capture {
                    return Err("committed read capture mismatch".into());
                }
                active.context.revalidate()?;
                active.service.authorize(&active.context)?;
                active.capture.chunk(offset, token)
            }
        }
    }
}
