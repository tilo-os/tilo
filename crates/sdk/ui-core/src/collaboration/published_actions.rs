//! Bounded access to the existing certified action channel, under an original
//! framework invocation. Queue admission is not a remote acknowledgement.
#[cfg(feature = "broker-transport")]
use crate::invocation_scope::ResourceEffectScope;
#[cfg(feature = "broker-transport")]
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
#[cfg(feature = "broker-transport")]
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tilo_actions::InvocationId;
#[cfg(feature = "broker-transport")]
use tilo_actions::remote::RemoteActionFrame;
use tilo_actions::remote::{CatalogueQuery, InvokeRequest, RemoteActionMessage};
#[cfg(feature = "broker-transport")]
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot};

#[cfg(feature = "broker-transport")]
const MAX_ROUTES: usize = 16;
#[cfg(feature = "broker-transport")]
const MAX_PENDING: usize = 2;

/// A transport answer, or an outcome whose admission cannot be established.
/// A receipt in `Answer` still has its original Accepted/Rejected status.
#[derive(Clone, Debug, PartialEq)]
pub enum PublishedActionAnswer {
    Answer(RemoteActionMessage),
    /// Never retried automatically. For Invoke, retain this original child ID.
    Unknown {
        invocation: Option<InvocationId>,
    },
    /// Refused before dispatch, with no inferred remote action completion.
    Unavailable,
}
/// Existing framework jobs can await this without borrowing an owner Store.
pub type PublishedActionFuture =
    Pin<Box<dyn Future<Output = PublishedActionAnswer> + Send + 'static>>;

#[derive(Clone)]
pub struct PublishedActions {
    #[cfg(feature = "broker-transport")]
    route: String,
    parent: InvocationId,
    #[cfg(feature = "broker-transport")]
    scope: ResourceEffectScope,
}
impl PublishedActions {
    #[cfg(feature = "broker-transport")]
    pub(crate) fn new(route: String, parent: InvocationId, scope: ResourceEffectScope) -> Self {
        Self {
            route,
            parent,
            scope,
        }
    }
    /// Browse or resolve via the existing CatalogueQuery vocabulary.
    pub fn query(&self, query: CatalogueQuery) -> Result<PublishedActionFuture, String> {
        self.request(RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query,
        })
    }
    /// Invoke under this original caller; callers retain the chosen child ID.
    pub fn invoke(&self, mut request: InvokeRequest) -> Result<PublishedActionFuture, String> {
        if request
            .on_behalf_of
            .is_some_and(|parent| parent != self.parent)
        {
            return Err("published action original parent cannot be replaced".into());
        }
        request.on_behalf_of = Some(self.parent);
        self.request(RemoteActionMessage::Invoke(request))
    }
    /// Ask the existing platform palette to select an existing resource.
    pub fn select_resource(
        &self,
        filter: tilo_actions::remote::ResourceSelectionFilter,
    ) -> Result<PublishedActionFuture, String> {
        self.request(RemoteActionMessage::SelectResource {
            correlation: 1,
            filter,
        })
    }
    pub fn observe(&self, invocation: InvocationId) -> Result<PublishedActionFuture, String> {
        self.request(RemoteActionMessage::Observe { invocation })
    }
    pub fn cancel(&self, invocation: InvocationId) -> Result<PublishedActionFuture, String> {
        self.request(RemoteActionMessage::Cancel { invocation })
    }
    /// Invoke once and retain original custody until the child's canonical
    /// terminal receipt. Observation never reissues an invocation, and reports
    /// no copied progress or inferred completion. A lost answer retains the
    /// original child identity. Parent retirement is owned by the broker.
    pub fn invoke_and_wait(&self, request: InvokeRequest) -> Result<PublishedActionFuture, String> {
        let invocation = request.invocation;
        let admitted = self.invoke(request)?;
        let actions = self.clone();
        Ok(Box::pin(async move {
            let answer = admitted.await;
            match answer {
                PublishedActionAnswer::Answer(RemoteActionMessage::Receipt(receipt))
                    if matches!(
                        receipt.status,
                        tilo_actions::remote::ReceiptStatus::Accepted
                    ) => {}
                PublishedActionAnswer::Answer(message) => {
                    return PublishedActionAnswer::Answer(message);
                }
                _ => return retained_unknown(invocation),
            }
            loop {
                let observation = match actions.observe(invocation) {
                    Ok(observation) => observation.await,
                    Err(_) => return retained_unknown(invocation),
                };
                match observation {
                    PublishedActionAnswer::Answer(RemoteActionMessage::Update {
                        state: Some(state),
                        ..
                    }) if state.invocation() == invocation && state.is_terminal() => {
                        return PublishedActionAnswer::Answer(RemoteActionMessage::Receipt(
                            state.receipt(),
                        ));
                    }
                    PublishedActionAnswer::Answer(RemoteActionMessage::Update {
                        state: Some(state),
                        ..
                    }) if state.invocation() == invocation => {}
                    _ => return retained_unknown(invocation),
                }
                // One retained child and at most five read-only observations
                // per second, through the existing bounded transport lane.
                #[cfg(feature = "broker-transport")]
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                #[cfg(not(feature = "broker-transport"))]
                return retained_unknown(invocation);
            }
        }))
    }
    fn request(&self, request: RemoteActionMessage) -> Result<PublishedActionFuture, String> {
        #[cfg(feature = "broker-transport")]
        {
            self.request_native(request)
        }
        #[cfg(not(feature = "broker-transport"))]
        {
            let _ = request;
            Err("published actions require the admitted native broker transport".into())
        }
    }
    #[cfg(feature = "broker-transport")]
    fn request_native(
        &self,
        request: RemoteActionMessage,
    ) -> Result<PublishedActionFuture, String> {
        self.scope.with_effect(|| Ok(()))?;
        let expected = Expected::for_request(&request)?;
        let frame = match &request {
            RemoteActionMessage::Invoke(_) => RemoteActionFrame::new(request),
            _ => RemoteActionFrame::continuing(request, self.parent)
                .map_err(|error| error.to_string())?,
        };
        let endpoint = registry()
            .lock()
            .map_err(|_| "published action bridge unavailable")?
            .get(&self.route)
            .and_then(Weak::upgrade)
            .ok_or("no acknowledged published action route")?;
        let (reply, receive) = oneshot::channel();
        let admission = endpoint
            .admissions
            .clone()
            .try_acquire_owned()
            .map_err(|_| "published action outstanding admission bound reached")?;
        let selection = if matches!(expected, Expected::Selection(_)) {
            Some(
                endpoint
                    .selection
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| "an attended selection is already pending")?,
            )
        } else {
            None
        };
        let sender = if selection.is_some() {
            &endpoint.select
        } else {
            &endpoint.ordinary
        };
        sender
            .try_send(Job {
                bytes: frame.encode().map_err(|error| error.to_string())?,
                expected,
                scope: self.scope.clone(),
                reply,
                _admission: admission,
                _selection: selection,
            })
            .map_err(|_| "published action bounded lane unavailable or full")?;
        let scope = self.scope.clone();
        Ok(Box::pin(async move {
            let answer = receive.await.unwrap_or_else(|_| unknown(expected));
            if matches!(answer, PublishedActionAnswer::Unavailable) {
                answer
            } else if scope.with_effect(|| Ok(())).is_err() {
                unknown(expected)
            } else {
                answer
            }
        }))
    }
}

fn retained_unknown(invocation: InvocationId) -> PublishedActionAnswer {
    PublishedActionAnswer::Unknown {
        invocation: Some(invocation),
    }
}

#[cfg(feature = "broker-transport")]
struct Job {
    bytes: Vec<u8>,
    expected: Expected,
    scope: ResourceEffectScope,
    reply: oneshot::Sender<PublishedActionAnswer>,
    _admission: OwnedSemaphorePermit,
    _selection: Option<OwnedSemaphorePermit>,
}
#[cfg(feature = "broker-transport")]
struct Endpoint {
    ordinary: mpsc::Sender<Job>,
    select: mpsc::Sender<Job>,
    // The previous worker could retain one RPC plus MAX_PENDING queued calls.
    admissions: Arc<Semaphore>,
    selection: Arc<Semaphore>,
}
#[cfg(feature = "broker-transport")]
pub(super) struct Registration {
    _endpoint: Arc<Endpoint>,
    worker: tokio::task::JoinHandle<()>,
    selection_worker: tokio::task::JoinHandle<()>,
}
#[cfg(feature = "broker-transport")]
impl Drop for Registration {
    fn drop(&mut self) {
        self.worker.abort();
        self.selection_worker.abort();
    }
}
#[cfg(feature = "broker-transport")]
fn registry() -> &'static Mutex<BTreeMap<String, Weak<Endpoint>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<String, Weak<Endpoint>>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}
#[cfg(feature = "broker-transport")]
fn admit(route: String, endpoint: &Arc<Endpoint>) -> Result<(), String> {
    let mut entries = registry()
        .lock()
        .map_err(|_| "published action bridge unavailable")?;
    entries.retain(|_, endpoint| endpoint.strong_count() != 0);
    if route.is_empty()
        || route.len() > 1024
        || entries.contains_key(&route)
        || entries.len() >= MAX_ROUTES
    {
        return Err("published action route is ambiguous or exceeds bounds".into());
    }
    entries.insert(route, Arc::downgrade(endpoint));
    Ok(())
}
#[cfg(feature = "broker-transport")]
#[derive(Clone, Copy)]
enum Expected {
    Catalogue(u64),
    Selection(u64),
    Invoke(InvocationId),
    Observe(InvocationId),
    Cancel(InvocationId),
}
#[cfg(feature = "broker-transport")]
impl Expected {
    fn for_request(request: &RemoteActionMessage) -> Result<Self, String> {
        match request {
            RemoteActionMessage::CatalogueRequest { correlation, .. } => {
                Ok(Self::Catalogue(*correlation))
            }
            RemoteActionMessage::SelectResource { correlation, .. } => {
                Ok(Self::Selection(*correlation))
            }
            RemoteActionMessage::Invoke(request) => Ok(Self::Invoke(request.invocation)),
            RemoteActionMessage::Observe { invocation } => Ok(Self::Observe(*invocation)),
            RemoteActionMessage::Cancel { invocation } => Ok(Self::Cancel(*invocation)),
            _ => Err("unsupported published action request".into()),
        }
    }
}
#[cfg(feature = "broker-transport")]
fn unknown(expected: Expected) -> PublishedActionAnswer {
    PublishedActionAnswer::Unknown {
        invocation: match expected {
            Expected::Invoke(invocation) => Some(invocation),
            _ => None,
        },
    }
}
#[cfg(feature = "broker-transport")]
fn matches(expected: Expected, response: &RemoteActionMessage) -> bool {
    match (expected, response) {
        (
            Expected::Catalogue(left),
            RemoteActionMessage::CatalogueResponse {
                correlation: right, ..
            },
        )
        | (
            Expected::Selection(left),
            RemoteActionMessage::ResourceSelectionResponse {
                correlation: right, ..
            },
        ) => left == *right,
        (Expected::Invoke(invocation), RemoteActionMessage::Receipt(receipt)) => {
            invocation == receipt.invocation
        }
        (
            Expected::Observe(left),
            RemoteActionMessage::Update {
                invocation: right, ..
            },
        )
        | (
            Expected::Cancel(left),
            RemoteActionMessage::CancelResponse {
                invocation: right, ..
            },
        ) => left == *right,
        _ => false,
    }
}
/// The acknowledged native record channel owns the only transport capability.
#[cfg(feature = "broker-transport")]
pub(super) fn register(
    route: String,
    channel: runtime_collab::ChannelHandle,
) -> Result<Registration, String> {
    let (ordinary, receive) = mpsc::channel::<Job>(MAX_PENDING);
    let (select, selections) = mpsc::channel::<Job>(1);
    let endpoint = Arc::new(Endpoint {
        ordinary,
        select,
        admissions: Arc::new(Semaphore::new(MAX_PENDING.saturating_add(1))),
        selection: Arc::new(Semaphore::new(1)),
    });
    admit(route, &endpoint)?;
    let worker = serve_lane(channel.clone(), receive);
    let selection_worker = serve_lane(channel, selections);
    Ok(Registration {
        _endpoint: endpoint,
        worker,
        selection_worker,
    })
}

/// Separate wait lifetimes share the same native capability and exact matcher.
/// Only the selection lane is new; ordinary long RPCs retain their old ordering.
#[cfg(feature = "broker-transport")]
fn serve_lane(
    channel: runtime_collab::ChannelHandle,
    mut receive: mpsc::Receiver<Job>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_local(async move {
        while let Some(mut job) = receive.recv().await {
            if job.reply.is_closed() {
                continue;
            }
            if job.scope.with_effect(|| Ok(())).is_err() {
                let _ = job.reply.send(PublishedActionAnswer::Unavailable);
                continue;
            }
            let result = tokio::select! {
                result = channel.remote_action(&job.bytes) => result,
                () = job.reply.closed() => continue,
            };
            let answer = result
                .ok()
                .and_then(|bytes| RemoteActionFrame::decode(&bytes).ok())
                .filter(|frame| matches(job.expected, &frame.message))
                .map_or_else(
                    || unknown(job.expected),
                    |frame| PublishedActionAnswer::Answer(frame.message),
                );
            let _ = job.reply.send(answer);
        }
    })
}

#[cfg(all(test, feature = "broker-transport"))]
#[path = "published_actions_tests.rs"]
mod tests;
