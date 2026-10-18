//! The app's side: one connection per process, many subscriptions on it.
//!
//! A sandboxed app finds the connection its launcher opened in
//! `TILO_SVC_SYSTEM-STATS_FD`. An unconfined copy (a development run) dials
//! the session's socket itself. Either way the process holds one
//! [`Client`]; a reader thread files each pushed snapshot under its
//! subscription and wakes whoever subscribed, and requests (end, ask) wait
//! for their own answer.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, mpsc};

use crate::protocol::{
    Grant, Grants, Handoff, PROTOCOL, Refusal, Reply, Request, SERVICE, Snapshot, read_message,
    write_handoff, write_message,
};

/// Wakes a subscriber when a snapshot arrived.
pub type Notify = Box<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct SubscriptionState {
    latest: Mutex<Option<Snapshot>>,
    /// Signalled with `latest` held when a snapshot, a refusal or the end
    /// of the connection arrives.
    arrived: Condvar,
    refused: Mutex<Option<Refusal>>,
    notify: Option<Notify>,
}

impl SubscriptionState {
    fn wake(&self) {
        drop(lock(&self.latest));
        self.arrived.notify_all();
        if let Some(notify) = &self.notify {
            notify();
        }
    }
}

struct Inner {
    writer: Mutex<UnixStream>,
    subscriptions: Mutex<HashMap<u64, Arc<SubscriptionState>>>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Reply>>>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // The reader thread holds a second descriptor of the socket; shut
        // the socket itself so both it and the service see the end.
        let _ = lock(&self.writer).shutdown(std::net::Shutdown::Both);
    }
}

/// One connection to the service.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

/// Snapshots pushed for one subscription; dropping it unsubscribes.
pub struct ClientSubscription {
    client: Client,
    id: u64,
    state: Arc<SubscriptionState>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ClientSubscription {
    /// The newest snapshot since the last call, if one arrived.
    #[must_use]
    pub fn take(&self) -> Option<Snapshot> {
        lock(&self.state.latest).take()
    }

    /// Wait for the next snapshot; `None` when the subscription was refused
    /// or the connection ended instead. The service sends the first one at
    /// once, so a fresh subscription answers as fast as the kernel is read.
    #[must_use]
    pub fn wait(&self) -> Option<Snapshot> {
        let mut latest = lock(&self.state.latest);
        loop {
            if let Some(snapshot) = latest.take() {
                return Some(snapshot);
            }
            if self.client.is_closed() || lock(&self.state.refused).is_some() {
                return None;
            }
            latest = self
                .state
                .arrived
                .wait(latest)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Why the service refused this subscription, if it did.
    #[must_use]
    pub fn refusal(&self) -> Option<Refusal> {
        lock(&self.state.refused).clone()
    }

    /// Whether the connection is gone: nothing more will arrive.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.client.is_closed()
    }

    /// The connection this subscription is on.
    #[must_use]
    pub fn client(&self) -> &Client {
        &self.client
    }
}

impl Drop for ClientSubscription {
    fn drop(&mut self) {
        lock(&self.client.inner.subscriptions).remove(&self.id);
        let _ = self.client.send(&Request::Unsubscribe {
            subscription: self.id,
        });
    }
}

impl Client {
    /// Speak the protocol on `stream`, whose handoff was already written.
    ///
    /// # Errors
    /// The stream cannot be duplicated for the reader, or its thread cannot
    /// start.
    pub fn from_stream(stream: UnixStream) -> std::io::Result<Self> {
        let mut reader = stream.try_clone()?;
        let client = Self {
            inner: Arc::new(Inner {
                writer: Mutex::new(stream),
                subscriptions: Mutex::new(HashMap::new()),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                closed: AtomicBool::new(false),
            }),
        };
        let inner = Arc::downgrade(&client.inner);
        std::thread::Builder::new()
            .name("tilo-system-stats-client".into())
            .spawn(move || {
                loop {
                    let reply = read_message::<_, Reply>(&mut reader);
                    let Some(inner) = inner.upgrade() else { break };
                    if let Ok(Some(reply)) = reply {
                        deliver(&inner, reply);
                    } else {
                        close(&inner);
                        break;
                    }
                }
            })?;
        Ok(client)
    }

    /// The connection the launcher handed this process, if it did.
    #[must_use]
    pub fn inherited() -> Option<Self> {
        let stream = take_inherited()?;
        Self::from_stream(stream).ok()
    }

    /// Dial the session's socket as the unconfined program `app_id`.
    ///
    /// # Errors
    /// There is no session service to reach.
    pub fn connect_unconfined(socket: &std::path::Path, app_id: &str) -> std::io::Result<Self> {
        let mut stream = UnixStream::connect(socket)?;
        write_handoff(
            &mut stream,
            &Handoff {
                protocol: PROTOCOL,
                app_id: app_id.to_owned(),
                workspace_id: None,
                grants: Grants::IMAGE_DEFAULT,
                ceiling: Grants::ALL,
            },
        )?;
        Self::from_stream(stream)
    }

    /// This process's connection: the one installed for it, else the one
    /// its launcher handed it, else one dialled at `socket` when it runs
    /// unconfined. `None` when there is no service to reach.
    #[must_use]
    pub fn shared(
        socket: impl FnOnce() -> Option<std::path::PathBuf>,
        app_id: &str,
    ) -> Option<Self> {
        let current = lock(shared_slot())
            .as_ref()
            .filter(|client| !client.is_closed())
            .cloned();
        if current.is_some() {
            return current;
        }
        let client = Self::inherited().or_else(|| {
            if inherited_was_offered() {
                // Sandboxed: the launcher decided. Never dial past it.
                return None;
            }
            Self::connect_unconfined(&socket()?, app_id).ok()
        })?;
        *lock(shared_slot()) = Some(client.clone());
        Some(client)
    }

    /// Make `client` this process's connection (a test's fake service).
    pub fn install_shared(client: Self) {
        *lock(shared_slot()) = Some(client);
    }

    /// Whether the connection is gone.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    fn next_id(&self) -> u64 {
        self.inner.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn send(&self, request: &Request) -> std::io::Result<()> {
        write_message(&mut *lock(&self.inner.writer), request)
    }

    /// Push a snapshot every `interval_ms`, with the process list when
    /// `processes`, calling `notify` as each arrives.
    #[must_use]
    pub fn subscribe(
        &self,
        interval_ms: u32,
        processes: bool,
        notify: Option<Notify>,
    ) -> ClientSubscription {
        let id = self.next_id();
        let state = Arc::new(SubscriptionState {
            notify,
            ..SubscriptionState::default()
        });
        lock(&self.inner.subscriptions).insert(id, Arc::clone(&state));
        if self
            .send(&Request::Subscribe {
                subscription: id,
                interval_ms,
                processes,
            })
            .is_err()
        {
            close(&self.inner);
        }
        ClientSubscription {
            client: self.clone(),
            id,
            state,
        }
    }

    /// Send `make(id)` and wait for its answer. Fails cleanly, never hangs,
    /// when the connection goes away: the reader drops every waiter then.
    fn call(&self, make: impl FnOnce(u64) -> Request) -> Result<Reply, Refusal> {
        let gone = || Refusal::Failed {
            detail: "the system information service is not running".into(),
        };
        if self.is_closed() {
            return Err(gone());
        }
        let id = self.next_id();
        let (tx, rx) = mpsc::channel();
        lock(&self.inner.pending).insert(id, tx);
        if self.send(&make(id)).is_err() {
            lock(&self.inner.pending).remove(&id);
            close(&self.inner);
            return Err(gone());
        }
        rx.recv().map_err(|_| gone())
    }

    /// End the process `pid` of the lifetime `started_at`.
    ///
    /// # Errors
    /// The service's refusal.
    pub fn end_process(&self, pid: u32, started_at: u64, force: bool) -> Result<u32, Refusal> {
        ended(self.call(|request| Request::EndProcess {
            request,
            pid,
            started_at,
            force,
        })?)
    }

    /// End every process of `app` the person may end.
    ///
    /// # Errors
    /// The service's refusal.
    pub fn end_app(&self, app: &str, force: bool) -> Result<u32, Refusal> {
        ended(self.call(|request| Request::EndApp {
            request,
            app: app.to_owned(),
            force,
        })?)
    }

    /// Ask the person for `grant`; the connection's grants once they
    /// answered yes.
    ///
    /// # Errors
    /// They declined, or nobody could be asked.
    pub fn ask_grant(&self, grant: Grant) -> Result<Grants, Refusal> {
        match self.call(|request| Request::AskGrant { request, grant })? {
            Reply::Grants { grants, .. } => Ok(grants),
            Reply::Refused { refusal, .. } => Err(refusal),
            _ => Err(Refusal::Declined),
        }
    }
}

fn ended(reply: Reply) -> Result<u32, Refusal> {
    match reply {
        Reply::Ended { ended, .. } => Ok(ended),
        Reply::Refused { refusal, .. } => Err(refusal),
        _ => Err(Refusal::Failed {
            detail: "unexpected answer".into(),
        }),
    }
}

fn deliver(inner: &Inner, reply: Reply) {
    match reply {
        Reply::Snapshot {
            subscription,
            snapshot,
        } => {
            let state = lock(&inner.subscriptions).get(&subscription).cloned();
            if let Some(state) = state {
                *lock(&state.latest) = Some(*snapshot);
                state.wake();
            }
        }
        Reply::SubscriptionRefused {
            subscription,
            refusal,
        } => {
            let state = lock(&inner.subscriptions).get(&subscription).cloned();
            if let Some(state) = state {
                *lock(&state.refused) = Some(refusal);
                state.wake();
            }
        }
        Reply::Grants { request, .. }
        | Reply::Ended { request, .. }
        | Reply::Refused { request, .. } => {
            let waiter = lock(&inner.pending).remove(&request);
            if let Some(waiter) = waiter {
                let _ = waiter.send(reply);
            }
        }
    }
}

fn close(inner: &Inner) {
    inner.closed.store(true, Ordering::Release);
    // Every waiter's sender goes, so each `recv` returns at once.
    lock(&inner.pending).clear();
    let subscriptions: Vec<_> = lock(&inner.subscriptions).values().cloned().collect();
    for state in subscriptions {
        state.wake();
    }
}

fn shared_slot() -> &'static Mutex<Option<Client>> {
    static SHARED: OnceLock<Mutex<Option<Client>>> = OnceLock::new();
    SHARED.get_or_init(|| Mutex::new(None))
}

/// The variable the launcher names the descriptor in.
fn inherited_variable() -> String {
    format!("TILO_SVC_{}_FD", SERVICE.to_uppercase())
}

/// Whether the launcher ran this process: it says so in `TILO_APP_DIR`,
/// whether or not it handed a connection.
fn inherited_was_offered() -> bool {
    std::env::var_os("TILO_APP_DIR").is_some() || std::env::var_os(inherited_variable()).is_some()
}

/// Take the launcher's descriptor, once.
fn take_inherited() -> Option<UnixStream> {
    static TAKEN: AtomicBool = AtomicBool::new(false);
    let fd: i32 = std::env::var(inherited_variable()).ok()?.parse().ok()?;
    if fd < 0 || TAKEN.swap(true, Ordering::AcqRel) {
        return None;
    }
    // SAFETY: F_GETFD neither reads nor changes the descriptor; it fails
    // with EBADF when the slot is empty.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return None;
    }
    // SAFETY: the launcher handed this slot to this process for this
    // service alone, and `TAKEN` makes this the only owner.
    Some(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
}
