//! The endpoint side of the collaboration wire: one connection shared by
//! every caller, its calls and subscriptions multiplexed by id.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::{mpsc, oneshot};

use super::{Call, Event, FRAMING, Handle, HostFrame, Reply, Request, Topic};

const DISCONNECTED: &str = "the collaboration host closed the connection";

type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// What a subscription delivers: its events, then why it ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Delivery {
    Event(Event),
    Ended(String),
}

#[derive(Default)]
struct Routes {
    calls: HashMap<u64, oneshot::Sender<Result<Reply, String>>>,
    subscriptions: HashMap<u64, mpsc::UnboundedSender<Delivery>>,
    closed: bool,
}

/// A connection to the collaboration host. Cheap to share: calls from any
/// task run concurrently on it.
pub struct Client {
    writer: tokio::sync::Mutex<Writer>,
    routes: Arc<Mutex<Routes>>,
    next: AtomicU64,
}

/// One live subscription. Dropping it stops routing its events; the host
/// keeps sending them until [`Client::unsubscribe`].
pub struct Subscription {
    id: u64,
    deliveries: mpsc::UnboundedReceiver<Delivery>,
}

impl Subscription {
    /// The id the host names this subscription by.
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// The next delivery; `None` once the connection closed.
    pub async fn next(&mut self) -> Option<Delivery> {
        self.deliveries.recv().await
    }
}

impl Client {
    /// A client over `reader`/`writer`, and the future that reads the host's
    /// frames for it; spawn it. When it ends, every waiting call fails and
    /// every subscription ends.
    pub fn new<R, W>(reader: R, writer: W) -> (Self, impl Future<Output = ()> + Send + use<R, W>)
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        let routes = Arc::new(Mutex::new(Routes::default()));
        let client = Self {
            writer: tokio::sync::Mutex::new(Box::new(writer)),
            routes: Arc::clone(&routes),
            next: AtomicU64::new(1),
        };
        (client, read_frames(reader, routes))
    }

    /// Ask the host `request` and wait for its answer.
    ///
    /// # Errors
    ///
    /// The host's refusal, or the connection failing.
    pub async fn call(&self, request: Request) -> Result<Reply, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        {
            let mut routes = self.routes();
            if routes.closed {
                return Err(DISCONNECTED.to_owned());
            }
            routes.calls.insert(id, answer);
        }
        if let Err(error) = self.send(Call { id, request }).await {
            self.routes().calls.remove(&id);
            return Err(error);
        }
        answered.await.map_err(|_| DISCONNECTED.to_owned())?
    }

    /// Subscribe to `channel`'s `topic`.
    ///
    /// # Errors
    ///
    /// The host's refusal, or the connection failing.
    pub async fn subscribe(&self, channel: Handle, topic: Topic) -> Result<Subscription, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        let (deliver, deliveries) = mpsc::unbounded_channel();
        {
            let mut routes = self.routes();
            if routes.closed {
                return Err(DISCONNECTED.to_owned());
            }
            routes.calls.insert(id, answer);
            // Routed before the call leaves: the host's events may follow
            // its reply at once.
            routes.subscriptions.insert(id, deliver);
        }
        let sent = self
            .send(Call {
                id,
                request: Request::Subscribe { channel, topic },
            })
            .await;
        let answered = match sent {
            Ok(()) => answered.await.map_err(|_| DISCONNECTED.to_owned()),
            Err(error) => Err(error),
        };
        match answered {
            Ok(Ok(Reply::Subscribed)) => Ok(Subscription { id, deliveries }),
            refused => {
                {
                    let mut routes = self.routes();
                    routes.calls.remove(&id);
                    routes.subscriptions.remove(&id);
                }
                Err(match refused {
                    Ok(Ok(other)) => format!("the host answered a subscription with {other:?}"),
                    Ok(Err(error)) | Err(error) => error,
                })
            }
        }
    }

    /// End `subscription` on the host.
    ///
    /// # Errors
    ///
    /// The host's refusal, or the connection failing.
    pub async fn unsubscribe(&self, subscription: Subscription) -> Result<(), String> {
        self.routes().subscriptions.remove(&subscription.id);
        self.call(Request::Unsubscribe {
            subscription: subscription.id,
        })
        .await
        .map(drop)
    }

    async fn send(&self, call: Call) -> Result<(), String> {
        let mut writer = self.writer.lock().await;
        async {
            FRAMING.write_value_async(&mut *writer, &call).await?;
            writer.flush().await?;
            Ok::<_, wire_frame::FrameError>(())
        }
        .await
        .map_err(|error| format!("collaboration wire: {error}"))
    }

    fn routes(&self) -> std::sync::MutexGuard<'_, Routes> {
        self.routes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

async fn read_frames<R: AsyncRead + Unpin>(mut reader: R, routes: Arc<Mutex<Routes>>) {
    loop {
        let frame: HostFrame = match FRAMING.read_value_async(&mut reader).await {
            Ok(Some(frame)) => frame,
            Ok(None) | Err(_) => break,
        };
        let mut routes = routes.lock().unwrap_or_else(PoisonError::into_inner);
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
                    let _ = deliver.send(Delivery::Ended(reason));
                }
            }
        }
    }
    let mut routes = routes.lock().unwrap_or_else(PoisonError::into_inner);
    routes.closed = true;
    routes.calls.clear();
    for (_, deliver) in routes.subscriptions.drain() {
        let _ = deliver.send(Delivery::Ended(DISCONNECTED.to_owned()));
    }
}
