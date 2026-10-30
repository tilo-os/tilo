//! The endpoint side of the collaboration wire: one connection shared by
//! every caller, its calls and subscriptions multiplexed by id, and the
//! host's asks of what the endpoint published answered on it.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot, watch};

use super::{Answer, Ask, Call, Event, FRAMING, Handle, HostFrame, Operation, Reply, Topic};

/// Why a call got no answer from the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The host read the call and refused it, saying why.
    Refused(String),
    /// The connection closed before the host answered.
    Closed,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(reason) => f.write_str(reason),
            Self::Closed => f.write_str(DISCONNECTED),
        }
    }
}

impl std::error::Error for Failure {}

const DISCONNECTED: &str = "the collaboration host closed the connection";

/// What a subscription delivers: its events, then why it ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Delivery {
    Event(Event),
    Ended(Failure),
}

/// How an endpoint answers the host's asks of something it published on a
/// channel, such as its action directory.
pub type Answerer =
    Arc<dyn Fn(Ask) -> Pin<Box<dyn Future<Output = Result<Answer, String>> + Send>> + Send + Sync>;

#[derive(Default)]
struct Routes {
    calls: HashMap<u64, oneshot::Sender<Result<Reply, String>>>,
    subscriptions: HashMap<u64, mpsc::UnboundedSender<Delivery>>,
    answerers: HashMap<Handle, Answerer>,
    closed: bool,
}

struct Shared {
    routes: Mutex<Routes>,
    next: AtomicU64,
    outgoing: mpsc::UnboundedSender<Call>,
    closed: watch::Receiver<bool>,
}

impl Shared {
    fn routes(&self) -> std::sync::MutexGuard<'_, Routes> {
        self.routes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Send `request` without waiting for its answer.
    fn notify(&self, request: Operation) {
        let id = self.id();
        let _ = self.outgoing.send(Call { id, request });
    }
}

/// A connection to the collaboration host. Cheap to clone and to share:
/// calls from any task or thread run concurrently on it.
#[derive(Clone)]
pub struct Client {
    shared: Arc<Shared>,
}

/// One live subscription. Dropping it ends it on the host.
pub struct Subscription {
    id: u64,
    deliveries: mpsc::UnboundedReceiver<Delivery>,
    shared: Weak<Shared>,
}

impl Subscription {
    /// The id the host names this subscription by.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// The next delivery; `None` once it ended and its end was delivered.
    pub async fn next(&mut self) -> Option<Delivery> {
        self.deliveries.recv().await
    }

    /// The next event, or why the subscription ended.
    ///
    /// # Errors
    ///
    /// The host ended the subscription, or the connection closed.
    pub async fn event(&mut self) -> Result<Event, Failure> {
        match self.deliveries.recv().await {
            Some(Delivery::Event(event)) => Ok(event),
            Some(Delivery::Ended(failure)) => Err(failure),
            None => Err(Failure::Closed),
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            let live = shared.routes().subscriptions.remove(&self.id).is_some();
            if live {
                shared.notify(Operation::Unsubscribe {
                    subscription: self.id,
                });
            }
        }
    }
}

impl Client {
    /// A client over `reader`/`writer`, and the future that carries its
    /// frames both ways; spawn it. When it ends, every waiting call fails
    /// and every subscription ends.
    pub fn new<R, W>(reader: R, writer: W) -> (Self, impl Future<Output = ()> + use<R, W>)
    where
        R: AsyncRead + Unpin + 'static,
        W: AsyncWrite + Unpin + 'static,
    {
        let (outgoing, calls) = mpsc::unbounded_channel();
        let (close, closed) = watch::channel(false);
        let shared = Arc::new(Shared {
            routes: Mutex::new(Routes::default()),
            next: AtomicU64::new(1),
            outgoing,
            closed,
        });
        let client = Self {
            shared: Arc::clone(&shared),
        };
        (
            client,
            carry(reader, writer, calls, Arc::downgrade(&shared), close),
        )
    }

    /// Ask the host `request` and wait for its answer.
    ///
    /// # Errors
    ///
    /// The host's refusal, or the connection failing.
    pub async fn call(&self, request: Operation) -> Result<Reply, Failure> {
        let id = self.shared.id();
        let (answer, answered) = oneshot::channel();
        {
            let mut routes = self.shared.routes();
            if routes.closed {
                return Err(Failure::Closed);
            }
            routes.calls.insert(id, answer);
        }
        if self.shared.outgoing.send(Call { id, request }).is_err() {
            self.shared.routes().calls.remove(&id);
            return Err(Failure::Closed);
        }
        answered
            .await
            .map_err(|_| Failure::Closed)?
            .map_err(Failure::Refused)
    }

    /// Send `request` without waiting for its answer.
    pub fn notify(&self, request: Operation) {
        self.shared.notify(request);
    }

    /// Subscribe to `channel`'s `topic`.
    ///
    /// # Errors
    ///
    /// The host's refusal, or the connection failing.
    pub async fn subscribe(&self, channel: Handle, topic: Topic) -> Result<Subscription, Failure> {
        let id = self.shared.id();
        let (answer, answered) = oneshot::channel();
        let (deliver, deliveries) = mpsc::unbounded_channel();
        {
            let mut routes = self.shared.routes();
            if routes.closed {
                return Err(Failure::Closed);
            }
            routes.calls.insert(id, answer);
            // Routed before the call leaves: the host's events may follow
            // its reply at once.
            routes.subscriptions.insert(id, deliver);
        }
        let subscription = Subscription {
            id,
            deliveries,
            shared: Arc::downgrade(&self.shared),
        };
        let request = Operation::Subscribe { channel, topic };
        if self.shared.outgoing.send(Call { id, request }).is_err() {
            self.shared.routes().calls.remove(&id);
            return Err(Failure::Closed);
        }
        match answered.await {
            Ok(Ok(Reply::Subscribed)) => Ok(subscription),
            Ok(Ok(other)) => Err(Failure::Refused(format!(
                "the host answered a subscription with {other:?}"
            ))),
            Ok(Err(error)) => {
                // Refused: nothing to end on the host.
                self.shared.routes().subscriptions.remove(&id);
                Err(Failure::Refused(error))
            }
            Err(_) => Err(Failure::Closed),
        }
    }

    /// Answer the host's asks about what this endpoint publishes on
    /// `channel` with `answerer`, until the channel is closed.
    pub fn answer(&self, channel: Handle, answerer: Answerer) {
        self.shared.routes().answerers.insert(channel, answerer);
    }

    /// Stop answering for `channel`, and close it on the host.
    pub fn close(&self, channel: Handle) {
        self.shared.routes().answerers.remove(&channel);
        self.shared.notify(Operation::Close { handle: channel });
    }

    /// Resolves once the connection has ended.
    pub fn closed(&self) -> impl Future<Output = ()> + Send + 'static {
        let mut closed = self.shared.closed.clone();
        async move {
            let _ = closed.wait_for(|closed| *closed).await;
        }
    }
}

/// Carry `calls` to the host and the host's frames to their routes until
/// either side ends.
async fn carry<R, W>(
    mut reader: R,
    mut writer: W,
    mut calls: mpsc::UnboundedReceiver<Call>,
    shared: Weak<Shared>,
    close: watch::Sender<bool>,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let write = async {
        while let Some(call) = calls.recv().await {
            let written = async {
                FRAMING.write_value_async(&mut writer, &call).await?;
                writer.flush().await?;
                Ok::<_, wire_frame::FrameError>(())
            }
            .await;
            if written.is_err() {
                break;
            }
        }
    };
    let read = async {
        loop {
            let frame: HostFrame = match FRAMING.read_value_async(&mut reader).await {
                Ok(Some(frame)) => frame,
                Ok(None) | Err(_) => break,
            };
            let Some(shared) = shared.upgrade() else {
                break;
            };
            route(&shared, frame);
        }
    };
    tokio::select! {
        () = write => {}
        () = read => {}
    }
    if let Some(shared) = shared.upgrade() {
        let mut routes = shared.routes();
        routes.closed = true;
        routes.calls.clear();
        routes.answerers.clear();
        for (_, deliver) in routes.subscriptions.drain() {
            let _ = deliver.send(Delivery::Ended(Failure::Closed));
        }
    }
    let _ = close.send(true);
}

fn route(shared: &Arc<Shared>, frame: HostFrame) {
    let mut routes = shared.routes();
    match frame {
        HostFrame::Reply { id, result } => {
            if let Some(answer) = routes.calls.remove(&id) {
                let _ = answer.send(result);
            }
        }
        HostFrame::Event {
            subscription,
            event,
        } => {
            if let Some(deliver) = routes.subscriptions.get(&subscription)
                && deliver.send(Delivery::Event(event)).is_err()
            {
                routes.subscriptions.remove(&subscription);
            }
        }
        HostFrame::Ended {
            subscription,
            reason,
        } => {
            if let Some(deliver) = routes.subscriptions.remove(&subscription) {
                let _ = deliver.send(Delivery::Ended(Failure::Refused(reason)));
            }
        }
        HostFrame::Ask { id, ask } => {
            let channel = match &ask {
                Ask::Query { channel, .. } | Ask::Resolve { channel, .. } => *channel,
            };
            let answerer = routes.answerers.get(&channel).cloned();
            drop(routes);
            let Some(answerer) = answerer else {
                shared.notify(Operation::Answer {
                    ask: id,
                    answer: Err("nothing answers on this channel".into()),
                });
                return;
            };
            let pending = answerer(ask);
            let shared = Arc::downgrade(shared);
            tokio::spawn(async move {
                let answer = pending.await;
                if let Some(shared) = shared.upgrade() {
                    shared.notify(Operation::Answer { ask: id, answer });
                }
            });
        }
    }
}
