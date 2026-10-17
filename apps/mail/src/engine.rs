//! The account engine — the worker seam that connects Mail to real IMAP and
//! SMTP servers, kept off the synchronous `ui` event loop.
//!
//! ## Why a worker
//! The `ui` event loop is synchronous and must never block on the network. So
//! every server round-trip (IMAP `FETCH`, SMTP submit) runs on a background
//! worker thread; the UI posts [`EngineRequest`]s and drains [`EngineEvent`]s
//! through a channel, exactly mirroring the `Saver` pattern the notes app uses.
//! The app stays responsive and `--shots` (which never starts the worker) stays
//! offline and deterministic.
//!
//! ## The two backends
//! * **Normal application build: real IMAP/SMTP.** The default feature set
//!   enables the `live` backend. The worker uses [`async_imap`] for
//!   fetch/flag/move and [`lettre`] for submission over rustls.
//! * **Hermetic/offline build: sample backend.** With default features disabled,
//!   and in the explicit screenshot/test constructors, the worker parses the
//!   bundled sample corpus and validates sends by building real MIME with
//!   `mail-builder` (see `transport::build_mime`) without opening a socket.
//! * **`live` feature implementation.** Behind `--features live`, the worker
//!   uses `async-imap` for fetch/flag/move and [`lettre`] for submission,
//!   over rustls. Screenshot/test construction remains network-free at runtime;
//!   the data types
//!   ([`AccountConfig`], [`Draft`], [`Message`]) are already the exact shapes
//!   those libraries consume and produce, so enabling the feature is a drop-in.
//!
//! Both backends speak the same [`MailEngine`] trait, so the app code is
//! identical regardless of which is compiled in.

use std::sync::mpsc::{Receiver, Sender};

use tilo_content::ContentStore;

use crate::model::{AccountConfig, Draft, Folder, Message};
use crate::oauth::OAuthProvider;
use crate::transport::build_mime;

/// Identity assigned when a request is authored. It follows every worker event
/// back to the UI, so concurrent connect/fetch/write/send results cannot
/// settle one another by accident.
pub type EngineRequestId = uuid::Uuid;

fn next_request_id() -> EngineRequestId {
    // Unlike a wrapping integer counter, a freshly-minted UUID cannot alias a
    // previous in-flight request after a counter exhaustion boundary. The
    // request id carries no account or message data.
    uuid::Uuid::new_v4()
}

/// A request from the UI to the engine worker.
#[derive(Clone, Debug)]
pub enum EngineRequest {
    /// Connect and list the folders for an account.
    Connect {
        request_id: EngineRequestId,
        account: AccountConfig,
    },
    /// Bring a folder up to date: what is new and the flags of what is
    /// held, or the page before the oldest message held (`want`).
    Fetch {
        request_id: EngineRequestId,
        account: AccountConfig,
        folder: Folder,
        want: crate::sync::Want,
    },
    /// Persist a flag change to the server (`UID STORE \Seen`/`\Flagged`).
    StoreFlag {
        request_id: EngineRequestId,
        account: AccountConfig,
        message: Box<Message>,
    },
    /// Move a message to another folder (`UID MOVE`). Carries the full message
    /// so the live backend can locate it on the server by its `Message-ID`.
    Move {
        request_id: EngineRequestId,
        account: AccountConfig,
        message: Box<Message>,
        to: Folder,
    },
    /// Permanently delete one message from its current server mailbox. The live
    /// backend uses UID STORE + UID EXPUNGE so unrelated `\\Deleted` messages
    /// are never swept up by a broad mailbox expunge.
    Purge {
        request_id: EngineRequestId,
        account: AccountConfig,
        message: Box<Message>,
    },
    /// Submit a draft via SMTP.
    Send {
        request_id: EngineRequestId,
        account: AccountConfig,
        draft: Box<Draft>,
    },
    /// Watch an account's inbox and say when it changes (IMAP IDLE), until
    /// the account is watched again or the engine stops.
    Watch {
        request_id: EngineRequestId,
        account: AccountConfig,
    },
    /// Run the OAuth authorization-code + PKCE browser flow for an account and
    /// seal the resulting tokens in the vault under `account_id`.
    OAuthAuthorize {
        request_id: EngineRequestId,
        account_id: String,
        provider: OAuthProvider,
        login_hint: String,
    },
}

impl EngineRequest {
    pub fn connect(account: AccountConfig) -> Self {
        Self::Connect {
            request_id: next_request_id(),
            account,
        }
    }
    /// What is new in `folder`; the app fills in what it holds of it when
    /// it posts the request.
    pub fn fetch(account: AccountConfig, folder: Folder) -> Self {
        Self::fetch_wanting(account, folder, crate::sync::Want::Latest { known: None })
    }

    pub fn fetch_wanting(account: AccountConfig, folder: Folder, want: crate::sync::Want) -> Self {
        Self::Fetch {
            request_id: next_request_id(),
            account,
            folder,
            want,
        }
    }
    pub fn store_flag(account: AccountConfig, message: Box<Message>) -> Self {
        Self::StoreFlag {
            request_id: next_request_id(),
            account,
            message,
        }
    }
    pub fn move_message(account: AccountConfig, message: Box<Message>, to: Folder) -> Self {
        Self::Move {
            request_id: next_request_id(),
            account,
            message,
            to,
        }
    }
    pub fn purge(account: AccountConfig, message: Box<Message>) -> Self {
        Self::Purge {
            request_id: next_request_id(),
            account,
            message,
        }
    }
    pub fn send(account: AccountConfig, draft: Box<Draft>) -> Self {
        Self::Send {
            request_id: next_request_id(),
            account,
            draft,
        }
    }
    pub fn watch(account: AccountConfig) -> Self {
        Self::Watch {
            request_id: next_request_id(),
            account,
        }
    }
    pub fn oauth_authorize(
        account_id: String,
        provider: OAuthProvider,
        login_hint: String,
    ) -> Self {
        Self::OAuthAuthorize {
            request_id: next_request_id(),
            account_id,
            provider,
            login_hint,
        }
    }

    pub const fn request_id(&self) -> EngineRequestId {
        match self {
            Self::Connect { request_id, .. }
            | Self::Fetch { request_id, .. }
            | Self::StoreFlag { request_id, .. }
            | Self::Move { request_id, .. }
            | Self::Purge { request_id, .. }
            | Self::Send { request_id, .. }
            | Self::Watch { request_id, .. }
            | Self::OAuthAuthorize { request_id, .. } => *request_id,
        }
    }
    /// A small, non-sensitive description used to settle UI request accounting
    /// after the worker has handled this request.  The UI must not infer
    /// completion from a successful data event: flag and move writes have no
    /// payload to display, and a connection can legitimately emit more than
    /// one event.
    pub const fn kind(&self) -> EngineRequestKind {
        match self {
            Self::Connect { .. } => EngineRequestKind::Connect,
            Self::Fetch { .. } => EngineRequestKind::Fetch,
            Self::StoreFlag { .. } => EngineRequestKind::StoreFlag,
            Self::Move { .. } => EngineRequestKind::Move,
            Self::Purge { .. } => EngineRequestKind::Purge,
            Self::Send { .. } => EngineRequestKind::Send,
            Self::Watch { .. } => EngineRequestKind::Watch,
            Self::OAuthAuthorize { .. } => EngineRequestKind::OAuthAuthorize,
        }
    }
}

/// The operation class of a completed engine request.  Kept deliberately
/// compact so no credentials, mail bodies, or attachment paths cross the
/// worker boundary merely to settle a spinner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineRequestKind {
    Connect,
    Fetch,
    StoreFlag,
    Move,
    Purge,
    Send,
    Watch,
    OAuthAuthorize,
}

/// An event from the engine worker back to the UI.
#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// Connection + authentication established.
    Connected {
        request_id: EngineRequestId,
        account_id: String,
    },
    /// The mailbox folders the server advertised (`LIST`).
    Folders {
        request_id: EngineRequestId,
        account_id: String,
        folders: Vec<String>,
    },
    /// A folder brought up to date.
    Fetched {
        request_id: EngineRequestId,
        account_id: String,
        folder: Folder,
        synced: crate::sync::Synced,
    },
    /// A watched inbox changed on its server: new mail, or another client
    /// read, starred or removed some. Carries the `Watch` that saw it.
    Arrived {
        request_id: EngineRequestId,
        account_id: String,
    },
    /// A send completed; carries the wire byte count.
    Sent {
        request_id: EngineRequestId,
        wire_bytes: usize,
    },
    /// An OAuth sign-in completed and the tokens were sealed in the vault.
    Authorized {
        request_id: EngineRequestId,
        account_id: String,
    },
    /// Any operation failed; carries a human-readable reason.
    Error {
        request_id: EngineRequestId,
        kind: EngineRequestKind,
        context: String,
        message: String,
    },
    /// The worker has finished handling exactly one queued request. This is
    /// emitted after that request's result/error events, including successful
    /// writes which otherwise have no UI payload of their own.
    Completed {
        request_id: EngineRequestId,
        kind: EngineRequestKind,
    },
}

impl EngineEvent {
    /// The request that produced this event. Every event, including a terminal
    /// completion and a failure, is tagged at the worker boundary.
    pub const fn request_id(&self) -> EngineRequestId {
        match self {
            Self::Connected { request_id, .. }
            | Self::Folders { request_id, .. }
            | Self::Fetched { request_id, .. }
            | Self::Sent { request_id, .. }
            | Self::Arrived { request_id, .. }
            | Self::Authorized { request_id, .. }
            | Self::Error { request_id, .. }
            | Self::Completed { request_id, .. } => *request_id,
        }
    }
}

/// The backend abstraction. The stub and the live IMAP/SMTP backend both
/// implement it, so the app's calling code never branches on which is active.
pub trait MailEngine: Send {
    /// Handle one request, emitting zero or more events onto `out`.
    fn handle(&mut self, req: EngineRequest, out: &Sender<EngineEvent>);
}

fn report_engine_error(
    out: &Sender<EngineEvent>,
    request_id: EngineRequestId,
    kind: EngineRequestKind,
    context: &str,
    result: Result<(), String>,
) {
    if let Err(message) = result {
        let _ = out.send(EngineEvent::Error {
            request_id,
            kind,
            context: context.into(),
            message,
        });
    }
}

/// The offline backend. It validates outgoing MIME locally, but never claims a
/// connection, sync, or delivery that did not happen. Live IMAP/SMTP is the
/// `live`-feature backend below.
pub struct StubEngine {
    /// Where draft attachment bytes are read from when validating MIME.
    content: ContentStore,
}

impl StubEngine {
    pub fn new(content: ContentStore) -> Self {
        Self { content }
    }
}

impl MailEngine for StubEngine {
    fn handle(&mut self, req: EngineRequest, out: &Sender<EngineEvent>) {
        let request_id = req.request_id();
        let kind = req.kind();
        match req {
            EngineRequest::Connect { account, .. } => {
                let _ = out.send(EngineEvent::Error {
                    request_id,
                    kind,
                    context: "offline".into(),
                    message: format!(
                        "IMAP is unavailable for {}; rebuild with the live mail backend to connect",
                        account.email
                    ),
                });
            }
            EngineRequest::Fetch {
                account, folder, ..
            } => {
                let _ = out.send(EngineEvent::Error {
                    request_id,
                    kind,
                    context: "offline".into(),
                    message: format!(
                        "Cannot refresh {} for {} while IMAP is offline",
                        folder.title(),
                        account.email
                    ),
                });
            }
            EngineRequest::StoreFlag { .. }
            | EngineRequest::Move { .. }
            | EngineRequest::Purge { .. } => {
                // Local CRDT changes remain available offline, but the user
                // must be told that the remote IMAP write was not performed.
                let _ = out.send(EngineEvent::Error {
                    request_id,
                    kind,
                    context: "offline".into(),
                    message: "Local change saved; it was not synchronized to IMAP".into(),
                });
            }
            // Offline there is no server to watch; the request just ends.
            EngineRequest::Watch { .. } => {}
            EngineRequest::OAuthAuthorize { account_id, .. } => {
                // Sign-in needs the live network stack; the offline build can't
                // complete it. Surface a clear reason instead of silently
                // pretending to have authorized.
                let _ = out.send(EngineEvent::Error {
                    request_id,
                    kind,
                    context: "oauth".into(),
                    message: format!("OAuth sign-in for {account_id} requires the live build"),
                });
            }
            EngineRequest::Send { account, draft, .. } => {
                let from = crate::model::Addr::new(account.display_name.clone(), account.email);
                match build_mime(&self.content, &from, &draft) {
                    Ok(bytes) => {
                        let _ = out.send(EngineEvent::Error {
                            request_id,
                            kind,
                            context: "send".into(),
                            message: format!(
                                "SMTP is offline; validated {} MIME bytes locally but did not send",
                                bytes.len()
                            ),
                        });
                    }
                    Err(e) => {
                        let _ = out.send(EngineEvent::Error {
                            request_id,
                            kind,
                            context: "send".into(),
                            message: e,
                        });
                    }
                }
            }
        }
    }
}

/// Spawn the engine worker thread and return the request sender + event
/// receiver. The live app calls this once at startup; `--shots` never does, so
/// it stays offline. Under `--features live` the real IMAP/SMTP backend is
/// used; otherwise the offline [`StubEngine`]. Either reads draft attachment
/// bytes from `content` when it builds outgoing MIME.
///
/// `wake` runs on the worker after each request has answered, so the window
/// draws the answer at once instead of polling for it.
pub fn spawn(content: ContentStore, wake: Wake) -> (Sender<EngineRequest>, Receiver<EngineEvent>) {
    #[cfg(feature = "live")]
    {
        match live::LiveEngine::new(content.clone(), std::sync::Arc::clone(&wake)) {
            Ok(engine) => spawn_waking(Box::new(engine), wake),
            Err(e) => {
                tracing::warn!(error = %e, "mail: live engine unavailable, using offline backend");
                spawn_waking(Box::new(StubEngine::new(content)), wake)
            }
        }
    }
    #[cfg(not(feature = "live"))]
    {
        spawn_waking(Box::new(StubEngine::new(content)), wake)
    }
}

/// What the worker calls once a request has answered.
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Spawn the worker with a specific backend (used by tests and the `live`
/// feature).
///
/// # Panics
/// As [`spawn_waking`].
pub fn spawn_with(backend: Box<dyn MailEngine>) -> (Sender<EngineRequest>, Receiver<EngineEvent>) {
    spawn_waking(backend, std::sync::Arc::new(|| {}))
}

/// [`spawn_with`], calling `wake` after each request has answered.
///
/// # Panics
/// Panics if the OS refuses to spawn the engine worker thread (e.g. resource
/// exhaustion). Returning the channel pair without a live worker would leave
/// every mail request queued and silently unanswered, and there is no
/// degraded mode to fall back to, so this is deliberately fatal at
/// construction time rather than a mail app that looks alive and never syncs.
#[allow(
    clippy::expect_used,
    reason = "without the worker thread every request queues unanswered forever, and there is no degraded mode to fall back to; failing loudly at startup beats a mail app that silently never syncs"
)]
pub fn spawn_waking(
    mut backend: Box<dyn MailEngine>,
    wake: Wake,
) -> (Sender<EngineRequest>, Receiver<EngineEvent>) {
    let (req_tx, req_rx) = std::sync::mpsc::channel::<EngineRequest>();
    let (evt_tx, evt_rx) = std::sync::mpsc::channel::<EngineEvent>();
    std::thread::Builder::new()
        .name("mail-engine".into())
        .spawn(move || {
            while let Ok(req) = req_rx.recv() {
                let kind = req.kind();
                let request_id = req.request_id();
                backend.handle(req, &evt_tx);
                // `MailEngine::handle` is synchronous from the worker's point
                // of view (the live backend blocks on each request), so
                // this terminal acknowledgement cannot overtake its result.
                let _ = evt_tx.send(EngineEvent::Completed { request_id, kind });
                wake();
            }
        })
        .expect("spawn mail engine worker");
    (req_tx, evt_rx)
}

/// Live IMAP/SMTP backend. Compiled only under `--features live` so the default
/// build doesn't pull the TLS/async stack.
#[cfg(feature = "live")]
pub mod live {
    use std::sync::Arc;

    use account_custody::Custodian;
    use futures_util::TryStreamExt;

    use super::{
        AccountConfig, Draft, EngineEvent, EngineRequest, EngineRequestId, EngineRequestKind,
        Folder, MailEngine, Message, Sender, build_mime,
    };
    // AccountConfig / Draft / Folder / Message / Sender / build_mime come via
    // `super::*`; these are the names the parent doesn't already bring in.
    use crate::model::{Addr, AuthMethod};
    use crate::oauth;
    use crate::transport::parse_fetched;
    use crate::validate::classify_conn_failure;

    /// Real IMAP fetch/flag/move + SMTP submit + OAuth sign-in. The engine
    /// worker waits on each request with the shared app runtime's
    /// `block_on`: the protocol futures are polled on the worker while the
    /// runtime thread drives their sockets and timers. This module is the
    /// only place the app opens a socket.
    pub struct LiveEngine {
        rt: &'static ui::rt::AppRuntime,
        tls: Arc<rustls::ClientConfig>,
        /// Where draft attachment bytes are read from when building MIME.
        content: tilo_content::ContentStore,
        /// Holds OAuth grants: seals sign-ins, hands out current access
        /// tokens.
        custodian: Custodian,
        /// One signed-in IMAP connection per account (by address), kept
        /// between requests, with when it last answered: a refresh or a
        /// flag change costs one round trip, not a TLS handshake and a login.
        sessions: std::collections::HashMap<String, (ImapSession, std::time::Instant)>,
        /// Where each account's server keeps its folders, by address, from
        /// its folder list.
        places: std::collections::HashMap<String, crate::sync::Places>,
        /// Each watched account's inbox watch, by address.
        watches: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,
        /// Wakes the window when a watch hears of new mail.
        wake: super::Wake,
    }

    /// A kept connection idle longer than this answers a NOOP before it is
    /// used: the server or the network (a suspended laptop) may have
    /// dropped it meanwhile.
    const IDLE_CHECK: std::time::Duration = std::time::Duration::from_secs(60);
    /// How long that NOOP may take before the connection counts as gone.
    const NOOP_ANSWER: std::time::Duration = std::time::Duration::from_secs(10);

    type SessionFuture<'s, T> = futures_util::future::LocalBoxFuture<'s, Result<T, String>>;

    /// The credentials to present to a server, resolved from the account +
    /// vault (a password, or a fresh OAuth access token).
    enum AuthCreds {
        Password { user: String, pass: String },
        XOAuth2 { user: String, token: String },
    }

    impl LiveEngine {
        /// # Errors
        ///
        /// Returns `Err` if the app runtime that drives the IMAP/SMTP sockets
        /// cannot be started (e.g. the OS refuses its thread).
        pub fn new(content: tilo_content::ContentStore, wake: super::Wake) -> Result<Self, String> {
            let rt = ui::rt::try_app().map_err(|e| e.to_string())?;
            Ok(Self {
                rt,
                tls: build_tls_config(),
                content,
                custodian: Custodian::live(),
                sessions: std::collections::HashMap::new(),
                places: std::collections::HashMap::new(),
                watches: std::collections::HashMap::new(),
                wake,
            })
        }

        /// Watch `account`'s inbox from now on, in place of any earlier
        /// watch of it.
        fn start_watch(
            &mut self,
            request_id: EngineRequestId,
            account: AccountConfig,
            out: &Sender<EngineEvent>,
        ) {
            if let Some(earlier) = self.watches.remove(&account.email) {
                earlier.abort();
            }
            let out = out.clone();
            let wake = std::sync::Arc::clone(&self.wake);
            let account_id = account.email.clone();
            let arrived = move || {
                let _ = out.send(EngineEvent::Arrived {
                    request_id,
                    account_id: account_id.clone(),
                });
                wake();
            };
            let key = account.email.clone();
            let task = self
                .rt
                .spawn(watch_inbox(account, self.tls.clone(), arrived));
            self.watches.insert(key, task);
        }

        /// Run `op` on `account`'s kept connection, opening one when there
        /// is none (or the kept one no longer answers). A connection an
        /// operation failed on is closed; the next request opens a new one.
        fn run<T>(
            &mut self,
            account: &AccountConfig,
            op: impl for<'s> Fn(&'s mut ImapSession) -> SessionFuture<'s, T>,
        ) -> Result<T, String> {
            let rt = self.rt;
            let key = account.email.clone();
            if let Some((session, answered)) = self.sessions.get_mut(&key)
                && answered.elapsed() > IDLE_CHECK
            {
                let alive = rt
                    .block_on(tokio::time::timeout(NOOP_ANSWER, session.noop()))
                    .is_ok_and(|answer| answer.is_ok_and(|noop| noop.is_ok()));
                if !alive {
                    self.sessions.remove(&key);
                }
            }
            if !self.sessions.contains_key(&key) {
                let creds = resolve_auth(&self.custodian, account)?;
                let session = self.block_on(connect_session(account, &creds, self.tls.clone()))?;
                self.sessions
                    .insert(key.clone(), (session, std::time::Instant::now()));
            }
            let Some((session, answered)) = self.sessions.get_mut(&key) else {
                return Err("the mail server connection closed".into());
            };
            let result = rt
                .block_on(op(session))
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            match result {
                Ok(value) => {
                    *answered = std::time::Instant::now();
                    Ok(value)
                }
                Err(error) => {
                    self.sessions.remove(&key);
                    Err(error)
                }
            }
        }

        /// Wait on one protocol future from the engine worker thread.
        fn block_on<T>(
            &self,
            future: impl std::future::Future<Output = Result<T, String>>,
        ) -> Result<T, String> {
            self.rt.block_on(future).map_err(|e| e.to_string())?
        }

        /// Where `account`'s server keeps its folders, listed once.
        fn places_of(&mut self, account: &AccountConfig) -> Result<crate::sync::Places, String> {
            if let Some(places) = self.places.get(&account.email) {
                return Ok(places.clone());
            }
            let listed = self.run(account, |session| Box::pin(list_folders(session)))?;
            let places = crate::sync::Places::from_list(&listed);
            self.places.insert(account.email.clone(), places.clone());
            Ok(places)
        }

        fn handle_fetch(
            &mut self,
            account: &AccountConfig,
            folder: Folder,
            want: crate::sync::Want,
        ) -> Result<crate::sync::Synced, String> {
            let places = self.places_of(account)?;
            let owner = account.clone();
            self.run(account, move |session| {
                let account = owner.clone();
                let places = places.clone();
                Box::pin(async move { sync_folder(session, &account, folder, &places, want).await })
            })
        }

        fn handle_store(
            &mut self,
            account: &AccountConfig,
            message: &Message,
        ) -> Result<(), String> {
            let places = self.places_of(account)?;
            let message = message.clone();
            self.run(account, move |session| {
                let message = message.clone();
                let places = places.clone();
                Box::pin(async move { store_flags(session, &message, &places).await })
            })
        }

        fn handle_move(
            &mut self,
            account: &AccountConfig,
            message: &Message,
            to: Folder,
        ) -> Result<(), String> {
            let places = self.places_of(account)?;
            let message = message.clone();
            self.run(account, move |session| {
                let message = message.clone();
                let places = places.clone();
                Box::pin(async move { move_message(session, &message, to, &places).await })
            })
        }

        fn handle_purge(
            &mut self,
            account: &AccountConfig,
            message: &Message,
        ) -> Result<(), String> {
            let places = self.places_of(account)?;
            let message = message.clone();
            self.run(account, move |session| {
                let message = message.clone();
                let places = places.clone();
                Box::pin(async move { purge_message(session, &message, &places).await })
            })
        }

        fn handle_send(&self, account: &AccountConfig, draft: &Draft) -> Result<usize, String> {
            let creds = resolve_auth(&self.custodian, account)?;
            let from = Addr::new(account.display_name.clone(), account.email.clone());
            let bytes = build_mime(&self.content, &from, draft)?;
            self.block_on(smtp_send(account, draft, &creds, &bytes))?;
            Ok(bytes.len())
        }

        fn process_connect(
            &mut self,
            request_id: EngineRequestId,
            kind: EngineRequestKind,
            account: AccountConfig,
            out: &Sender<EngineEvent>,
        ) {
            let listed = self.run(&account, |session| Box::pin(list_folders(session)));
            match listed {
                Ok(listed) => {
                    self.places.insert(
                        account.email.clone(),
                        crate::sync::Places::from_list(&listed),
                    );
                    let folders = listed.into_iter().map(|(name, _)| name).collect();
                    let _ = out.send(EngineEvent::Connected {
                        request_id,
                        account_id: account.email.clone(),
                    });
                    let _ = out.send(EngineEvent::Folders {
                        request_id,
                        account_id: account.email,
                        folders,
                    });
                }
                Err(message) => {
                    let _ = out.send(EngineEvent::Error {
                        request_id,
                        kind,
                        context: "connect".into(),
                        message,
                    });
                }
            }
        }

        fn process_send(
            &self,
            request_id: EngineRequestId,
            kind: EngineRequestKind,
            account: &AccountConfig,
            draft: &Draft,
            out: &Sender<EngineEvent>,
        ) {
            match self.handle_send(account, draft) {
                Ok(wire_bytes) => {
                    let _ = out.send(EngineEvent::Sent {
                        request_id,
                        wire_bytes,
                    });
                }
                Err(message) => {
                    let _ = out.send(EngineEvent::Error {
                        request_id,
                        kind,
                        context: "smtp".into(),
                        message,
                    });
                }
            }
        }

        fn process_oauth(
            &self,
            request_id: EngineRequestId,
            kind: EngineRequestKind,
            account_id: String,
            provider: oauth::OAuthProvider,
            login_hint: &str,
            out: &Sender<EngineEvent>,
        ) {
            let signed_in =
                oauth::sign_in(&self.custodian, &account_id, provider, login_hint, |url| {
                    ui::widget::try_open_external_url(url).map_err(|error| error.to_string())
                });
            match signed_in {
                Ok(()) => {
                    let _ = out.send(EngineEvent::Authorized {
                        request_id,
                        account_id,
                    });
                }
                Err(message) => {
                    let _ = out.send(EngineEvent::Error {
                        request_id,
                        kind,
                        context: "oauth".into(),
                        message,
                    });
                }
            }
        }
    }

    impl MailEngine for LiveEngine {
        fn handle(&mut self, req: EngineRequest, out: &Sender<EngineEvent>) {
            let request_id = req.request_id();
            let kind = req.kind();
            match req {
                EngineRequest::Connect { account, .. } => {
                    self.process_connect(request_id, kind, account, out);
                }
                EngineRequest::Fetch {
                    account,
                    folder,
                    want,
                    ..
                } => match self.handle_fetch(&account, folder, want) {
                    Ok(synced) => {
                        let _ = out.send(EngineEvent::Fetched {
                            request_id,
                            account_id: account.email,
                            folder,
                            synced,
                        });
                    }
                    Err(e) => {
                        let _ = out.send(EngineEvent::Error {
                            request_id,
                            kind,
                            context: "fetch".into(),
                            message: e,
                        });
                    }
                },
                EngineRequest::StoreFlag {
                    account, message, ..
                } => {
                    if let Err(e) = self.handle_store(&account, &message) {
                        let _ = out.send(EngineEvent::Error {
                            request_id,
                            kind,
                            context: "flag".into(),
                            message: e,
                        });
                    }
                }
                EngineRequest::Move {
                    account,
                    message,
                    to,
                    ..
                } => {
                    if let Err(e) = self.handle_move(&account, &message, to) {
                        let _ = out.send(EngineEvent::Error {
                            request_id,
                            kind,
                            context: "move".into(),
                            message: e,
                        });
                    }
                }
                EngineRequest::Purge {
                    account, message, ..
                } => {
                    super::report_engine_error(
                        out,
                        request_id,
                        kind,
                        "purge",
                        self.handle_purge(&account, &message),
                    );
                }
                EngineRequest::Send { account, draft, .. } => {
                    self.process_send(request_id, kind, &account, &draft, out);
                }
                EngineRequest::Watch { account, .. } => {
                    self.start_watch(request_id, account, out);
                }
                EngineRequest::OAuthAuthorize {
                    account_id,
                    provider,
                    login_hint,
                    ..
                } => self.process_oauth(request_id, kind, account_id, provider, &login_hint, out),
            }
        }
    }

    /// Build the shared rustls client config against the system trust bundle.
    ///
    /// # Panics
    /// Never in practice: the ring provider always advertises the default
    /// protocol versions, so this only fails against a provider built
    /// without TLS 1.2/1.3 support.
    #[allow(
        clippy::expect_used,
        reason = "the bundled ring provider always supports the default protocol versions; a TLS config with no usable version is not something the mail engine can degrade to"
    )]
    fn build_tls_config() -> Arc<rustls::ClientConfig> {
        Arc::new(tilo_net::client_config().expect("rustls default protocol versions"))
    }

    /// The login user for an account (the explicit username, or the email).
    fn login_user(account: &AccountConfig) -> String {
        if account.username.trim().is_empty() {
            account.email.clone()
        } else {
            account.username.clone()
        }
    }

    /// Resolve the credentials to present: a password from the account, or a
    /// current OAuth access token from custody (which refreshes the grant
    /// when it is due).
    fn resolve_auth(custodian: &Custodian, account: &AccountConfig) -> Result<AuthCreds, String> {
        let user = login_user(account);
        if account.import_pending {
            return Err(crate::i18n::t("mail-import-sign-in-required"));
        }
        match account.auth {
            AuthMethod::Password => {
                if account.password.is_empty() {
                    return Err("no password set for this account".into());
                }
                Ok(AuthCreds::Password {
                    user,
                    pass: account.password.clone(),
                })
            }
            AuthMethod::OAuth => {
                let provider =
                    oauth::OAuthProvider::for_account(&account.email, &account.imap_host)
                        .ok_or("OAuth sign-in is not supported for this provider")?;
                let token = oauth::access_token(custodian, &account.email, provider, false)?;
                Ok(AuthCreds::XOAuth2 { user, token })
            }
        }
    }

    /// A connected + authenticated IMAP session over rustls.
    type ImapSession = async_imap::Session<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

    /// Open a TLS connection and authenticate (password `LOGIN` or `XOAUTH2`).
    async fn connect_session(
        account: &AccountConfig,
        creds: &AuthCreds,
        tls: Arc<rustls::ClientConfig>,
    ) -> Result<ImapSession, String> {
        let host = account.imap_host.trim();
        if host.is_empty() {
            return Err("no IMAP host".into());
        }
        let port = if account.imap_port == 0 {
            993
        } else {
            account.imap_port
        };
        let tcp = crate::connect::tcp(host, port).await?;
        let connector = tokio_rustls::TlsConnector::from(tls);
        let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|e| format!("invalid IMAP host name: {e}"))?;
        let stream = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| classify_conn_failure(&e.to_string()))?;
        let client = async_imap::Client::new(stream);
        match creds {
            AuthCreds::Password { user, pass } => client
                .login(user, pass)
                .await
                .map_err(|(e, _)| classify_conn_failure(&e.to_string())),
            AuthCreds::XOAuth2 { user, token } => {
                let auth = crate::oauth::XOAuth2 {
                    user: user.clone(),
                    token: token.clone(),
                };
                client
                    .authenticate("XOAUTH2", auth)
                    .await
                    .map_err(|(e, _)| classify_conn_failure(&e.to_string()))
            }
        }
    }

    /// How long one IDLE lasts before it is renewed: under the 30 minutes
    /// after which RFC 2177 lets a server drop an idle client.
    const IDLE_RENEW: std::time::Duration = std::time::Duration::from_secs(25 * 60);
    /// The first wait before a watch that failed connects again; each
    /// further failure doubles it, up to [`WATCH_RETRY_MAX`].
    const WATCH_RETRY: std::time::Duration = std::time::Duration::from_secs(15);
    const WATCH_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(10 * 60);

    /// Watch `account`'s inbox on a connection of its own, held in IMAP IDLE:
    /// whenever the server says the inbox changed, `arrived` hears of it, so
    /// new mail shows within seconds instead of at the next refresh. A watch
    /// whose connection fails connects again after a growing pause; one the
    /// server does not offer IDLE to ends (the periodic refresh covers it).
    async fn watch_inbox(
        account: AccountConfig,
        tls: Arc<rustls::ClientConfig>,
        arrived: impl Fn() + Send + Sync + 'static,
    ) {
        let mut pause = WATCH_RETRY;
        loop {
            // Sign in afresh each time: an OAuth token may have been
            // renewed since the last connection.
            let owner = account.clone();
            let creds =
                tokio::task::spawn_blocking(move || resolve_auth(&Custodian::live(), &owner))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|creds| creds);
            let watched = match creds {
                Ok(creds) => idle_until_failure(&account, &creds, tls.clone(), &arrived).await,
                Err(error) => Err(error),
            };
            match watched {
                Ok(()) => return,
                Err(error) => {
                    tracing::debug!(%error, "mail: the inbox watch stopped; connecting again later");
                }
            }
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(WATCH_RETRY_MAX);
        }
    }

    /// One watching connection, until it fails (`Err`) or the server turns
    /// out not to offer IDLE (`Ok`).
    async fn idle_until_failure(
        account: &AccountConfig,
        creds: &AuthCreds,
        tls: Arc<rustls::ClientConfig>,
        arrived: &(impl Fn() + Sync),
    ) -> Result<(), String> {
        let mut session = connect_session(account, creds, tls).await?;
        let offered = session
            .capabilities()
            .await
            .map_err(|e| e.to_string())?
            .has_str("IDLE");
        if !offered {
            return Ok(());
        }
        session.select("INBOX").await.map_err(|e| e.to_string())?;
        loop {
            let mut idle = session.idle();
            idle.init().await.map_err(|e| e.to_string())?;
            let (wait, _stop) = idle.wait_with_timeout(IDLE_RENEW);
            let response = wait.await.map_err(|e| e.to_string())?;
            session = idle.done().await.map_err(|e| e.to_string())?;
            if let async_imap::extensions::idle::IdleResponse::NewData(_) = response {
                arrived();
            }
        }
    }

    /// LIST every mailbox the server advertises, with the special uses it
    /// marks each with (`"Sent"`, `"All"`, …).
    async fn list_folders(session: &mut ImapSession) -> Result<Vec<(String, Vec<String>)>, String> {
        use async_imap::types::NameAttribute;
        let stream = session
            .list(Some(""), Some("*"))
            .await
            .map_err(|e| e.to_string())?;
        stream
            .map_ok(|listed| {
                let uses = listed
                    .attributes()
                    .iter()
                    .filter_map(|attribute| match attribute {
                        NameAttribute::All => Some("All"),
                        NameAttribute::Archive => Some("Archive"),
                        NameAttribute::Drafts => Some("Drafts"),
                        NameAttribute::Junk => Some("Junk"),
                        NameAttribute::Sent => Some("Sent"),
                        NameAttribute::Trash => Some("Trash"),
                        _ => None,
                    })
                    .map(str::to_owned)
                    .collect();
                (listed.name().to_owned(), uses)
            })
            .try_collect()
            .await
            .map_err(|e| e.to_string())
    }

    /// Bring `folder` up to date as `want` asks, on an open session.
    async fn sync_folder(
        session: &mut ImapSession,
        account: &AccountConfig,
        folder: Folder,
        places: &crate::sync::Places,
        want: crate::sync::Want,
    ) -> Result<crate::sync::Synced, String> {
        use crate::sync::{CATCH_UP_LIMIT, PAGE, Synced, Want};
        let mailbox = places.name(folder);
        let selected = session.select(mailbox).await.map_err(|e| e.to_string())?;
        let validity = selected.uid_validity.unwrap_or(0);
        let exists = selected.exists;
        let uid_next = selected.uid_next.unwrap_or(0);
        let now_min = crate::state::now_minutes();
        let mut synced = Synced {
            uid_validity: validity,
            ..Synced::default()
        };
        if folder == Folder::Starred {
            // Starred is INBOX seen through its flag: the flagged messages,
            // newest page.
            let uids: Vec<u32> = {
                let mut uids: Vec<u32> = session
                    .uid_search("FLAGGED")
                    .await
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .collect();
                uids.sort_unstable();
                uids.into_iter().rev().take(PAGE as usize).collect()
            };
            synced.reset = true;
            synced.messages =
                fetch_uids(session, account, folder, validity, &uids, now_min).await?;
            return Ok(synced);
        }
        if exists == 0 {
            synced.reset = true;
            return Ok(synced);
        }
        match want {
            Want::Latest { known: Some(known) }
                if known.uid_validity == validity
                    && uid_next.saturating_sub(known.newest + 1) <= CATCH_UP_LIMIT =>
            {
                // What arrived since the newest message held. `n:*` always
                // answers with the last message, even an older one: keep
                // only the new.
                let range = format!("{}:*", known.newest + 1);
                synced.messages = fetch_set(session, account, folder, validity, &range, now_min)
                    .await?
                    .into_iter()
                    .filter(|message| message.server.is_some_and(|at| at.uid > known.newest))
                    .collect();
                synced.flags = fetch_flags(session, known.oldest, known.newest).await?;
                synced.window = Some((known.oldest, known.newest));
            }
            Want::Latest { .. } => {
                // Nothing held, the folder renumbered, or too much missed:
                // start from the newest page.
                synced.reset = true;
                let first = exists.saturating_sub(PAGE - 1).max(1);
                synced.messages =
                    fetch_seq(session, account, folder, validity, first, exists, now_min).await?;
                synced.has_older = Some(first > 1);
            }
            Want::Older { known } if known.uid_validity == validity => {
                // The position of the oldest message held, then the page
                // before it.
                let position = {
                    let stream = session
                        .uid_fetch(known.oldest.to_string(), "(UID)")
                        .await
                        .map_err(|e| e.to_string())?;
                    let fetched: Vec<async_imap::types::Fetch> =
                        stream.try_collect().await.map_err(|e| e.to_string())?;
                    fetched.first().map(|fetch| fetch.message)
                };
                let Some(position) = position.filter(|position| *position > 1) else {
                    return Ok(synced);
                };
                let last = position - 1;
                let first = last.saturating_sub(PAGE - 1).max(1);
                synced.messages =
                    fetch_seq(session, account, folder, validity, first, last, now_min).await?;
                synced.has_older = Some(first > 1);
            }
            Want::Older { .. } => {
                synced.reset = true;
                let first = exists.saturating_sub(PAGE - 1).max(1);
                synced.messages =
                    fetch_seq(session, account, folder, validity, first, exists, now_min).await?;
                synced.has_older = Some(first > 1);
            }
        }
        Ok(synced)
    }

    async fn fetch_seq(
        session: &mut ImapSession,
        account: &AccountConfig,
        folder: Folder,
        validity: u32,
        first: u32,
        last: u32,
        now_min: i64,
    ) -> Result<Vec<Message>, String> {
        let stream = session
            .fetch(format!("{first}:{last}"), FETCH_ITEMS)
            .await
            .map_err(|e| e.to_string())?;
        let fetches: Vec<async_imap::types::Fetch> =
            stream.try_collect().await.map_err(|e| e.to_string())?;
        Ok(parse_all(&fetches, account, folder, validity, now_min))
    }

    async fn fetch_set(
        session: &mut ImapSession,
        account: &AccountConfig,
        folder: Folder,
        validity: u32,
        uid_set: &str,
        now_min: i64,
    ) -> Result<Vec<Message>, String> {
        let stream = session
            .uid_fetch(uid_set, FETCH_ITEMS)
            .await
            .map_err(|e| e.to_string())?;
        let fetches: Vec<async_imap::types::Fetch> =
            stream.try_collect().await.map_err(|e| e.to_string())?;
        Ok(parse_all(&fetches, account, folder, validity, now_min))
    }

    async fn fetch_uids(
        session: &mut ImapSession,
        account: &AccountConfig,
        folder: Folder,
        validity: u32,
        uids: &[u32],
        now_min: i64,
    ) -> Result<Vec<Message>, String> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        let set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        fetch_set(session, account, folder, validity, &set, now_min).await
    }

    /// `(uid, seen, flagged)` of the messages from `oldest` to `newest`.
    async fn fetch_flags(
        session: &mut ImapSession,
        oldest: u32,
        newest: u32,
    ) -> Result<Vec<(u32, bool, bool)>, String> {
        let stream = session
            .uid_fetch(format!("{oldest}:{newest}"), "(UID FLAGS)")
            .await
            .map_err(|e| e.to_string())?;
        let fetches: Vec<async_imap::types::Fetch> =
            stream.try_collect().await.map_err(|e| e.to_string())?;
        Ok(fetches
            .iter()
            .filter_map(|fetch| {
                let uid = fetch.uid?;
                if uid < oldest || uid > newest {
                    return None;
                }
                let seen = fetch
                    .flags()
                    .any(|flag| matches!(flag, async_imap::types::Flag::Seen));
                let flagged = fetch
                    .flags()
                    .any(|flag| matches!(flag, async_imap::types::Flag::Flagged));
                Some((uid, seen, flagged))
            })
            .collect())
    }

    /// What a fetch asks for: the whole message, without marking it read.
    const FETCH_ITEMS: &str = "(UID FLAGS INTERNALDATE BODY.PEEK[])";

    fn parse_all(
        fetches: &[async_imap::types::Fetch],
        account: &AccountConfig,
        folder: Folder,
        validity: u32,
        now_min: i64,
    ) -> Vec<Message> {
        fetches
            .iter()
            .filter_map(|fetch| {
                let body = fetch.body()?;
                let seen = fetch
                    .flags()
                    .any(|flag| matches!(flag, async_imap::types::Flag::Seen));
                let flagged = fetch
                    .flags()
                    .any(|flag| matches!(flag, async_imap::types::Flag::Flagged));
                let internal = fetch.internal_date().map(|date| date.timestamp() / 60);
                let mut message = parse_fetched(body, folder, seen, flagged, internal, now_min)?;
                message.account.clone_from(&account.email);
                message.server = fetch.uid.map(|uid| crate::model::ServerPlace {
                    folder,
                    uid,
                    uid_validity: validity,
                });
                Some(message)
            })
            .collect()
    }

    /// Find a message's server UID in the currently-selected mailbox by its
    /// RFC 5322 `Message-ID`.
    async fn find_uid(session: &mut ImapSession, message: &Message) -> Result<u32, String> {
        let rfc_id = message
            .rfc_message_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or("message has no Message-ID to locate it on the server")?;
        let query = format!("HEADER MESSAGE-ID \"{}\"", rfc_id.replace('"', ""));
        let uids: std::collections::HashSet<u32> =
            session.uid_search(query).await.map_err(|e| e.to_string())?;
        uids.into_iter()
            .max()
            .ok_or_else(|| "message not found on the server".into())
    }

    /// Select the folder its server holds `message` in and find its UID
    /// there: the one the server reported, while the folder keeps that
    /// numbering, otherwise by its `Message-ID`.
    async fn locate(
        session: &mut ImapSession,
        message: &Message,
        places: &crate::sync::Places,
    ) -> Result<u32, String> {
        let folder = message.server.map_or(message.folder, |at| at.folder);
        let mailbox = places.name(folder);
        let selected = session.select(mailbox).await.map_err(|e| e.to_string())?;
        if let Some(at) = message.server
            && selected.uid_validity == Some(at.uid_validity)
        {
            return Ok(at.uid);
        }
        find_uid(session, message).await
    }

    /// STORE `\Seen`/`\Flagged` to match the local message's flag state.
    async fn store_flags(
        session: &mut ImapSession,
        message: &Message,
        places: &crate::sync::Places,
    ) -> Result<(), String> {
        let uid = locate(session, message, places).await?;
        let seen_op = if message.read {
            "+FLAGS.SILENT (\\Seen)"
        } else {
            "-FLAGS.SILENT (\\Seen)"
        };
        let flag_op = if message.starred {
            "+FLAGS.SILENT (\\Flagged)"
        } else {
            "-FLAGS.SILENT (\\Flagged)"
        };
        for op in [seen_op, flag_op] {
            let stream = session
                .uid_store(uid.to_string(), op)
                .await
                .map_err(|e| e.to_string())?;
            let _: Vec<_> = stream.try_collect().await.map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// UID MOVE a message to another folder.
    async fn move_message(
        session: &mut ImapSession,
        message: &Message,
        to: Folder,
        places: &crate::sync::Places,
    ) -> Result<(), String> {
        let uid = locate(session, message, places).await?;
        session
            .uid_mv(uid.to_string(), places.name(to))
            .await
            .map_err(|e| e.to_string())
    }

    /// Permanently delete exactly one UID. UID EXPUNGE deliberately avoids the
    /// dangerous plain EXPUNGE fallback, which could remove messages another
    /// client marked `\\Deleted` concurrently.
    async fn purge_message(
        session: &mut ImapSession,
        message: &Message,
        places: &crate::sync::Places,
    ) -> Result<(), String> {
        let uid = locate(session, message, places).await?;
        let deleted = session
            .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
            .await
            .map_err(|e| e.to_string())?;
        let _: Vec<_> = deleted.try_collect().await.map_err(|e| e.to_string())?;
        let expunged = session
            .uid_expunge(uid.to_string())
            .await
            .map_err(|e| format!("server cannot safely expunge this message: {e}"))?;
        let _: Vec<_> = expunged.try_collect().await.map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Submit a pre-built MIME message over SMTP (STARTTLS on 587, implicit TLS
    /// on 465), authenticating with a password or `XOAUTH2`.
    async fn smtp_send(
        account: &AccountConfig,
        draft: &Draft,
        creds: &AuthCreds,
        mime: &[u8],
    ) -> Result<(), String> {
        smtp_send_with_tls(account, draft, creds, mime, None).await
    }

    /// The submission path, with the TLS parameters injectable. Production passes
    /// `None`, which resolves to the same implicit-TLS (465) / STARTTLS (else)
    /// setup `lettre`'s `relay`/`starttls_relay` build with the Tilo system
    /// roots. A caller may pass explicit parameters (e.g. a test pinned to a
    /// local server's certificate) so the real submission code is exercised over
    /// a socket without reaching a public server.
    async fn smtp_send_with_tls(
        account: &AccountConfig,
        draft: &Draft,
        creds: &AuthCreds,
        mime: &[u8],
        tls_override: Option<lettre::transport::smtp::client::TlsParameters>,
    ) -> Result<(), String> {
        use lettre::transport::smtp::authentication::{Credentials, Mechanism};
        use lettre::transport::smtp::client::TlsParameters;

        let host = account.smtp_host.trim();
        if host.is_empty() {
            return Err("no SMTP host".into());
        }
        let port = if account.smtp_port == 0 {
            465
        } else {
            account.smtp_port
        };
        let params = match tls_override {
            Some(p) => p,
            None => TlsParameters::new(host.to_string())
                .map_err(|e| classify_conn_failure(&e.to_string()))?,
        };
        let (creds_l, mech) = match creds {
            AuthCreds::Password { user, pass } => (
                Credentials::new(user.clone(), pass.clone()),
                Mechanism::Login,
            ),
            AuthCreds::XOAuth2 { user, token } => (
                Credentials::new(user.clone(), token.clone()),
                Mechanism::Xoauth2,
            ),
        };
        let envelope = build_envelope(account, draft)?;
        // 465 → implicit TLS; anything else (587/25) → a required STARTTLS
        // upgrade. The connection races the host's addresses.
        let mut connection = crate::connect::smtp(host, port, build_tls_config(), params).await?;
        let sent = async {
            connection
                .auth(&[mech], &creds_l)
                .await
                .map_err(|e| classify_conn_failure(&e.to_string()))?;
            connection
                .send(&envelope, mime)
                .await
                .map_err(|e| classify_conn_failure(&e.to_string()))
        }
        .await;
        let _ = connection.quit().await;
        sent.map(drop)
    }

    #[test]
    fn blind_recipients_remain_in_the_delivery_envelope() {
        let account = AccountConfig {
            email: "sender@example.test".into(),
            ..Default::default()
        };
        for to in ["", "visible@example.test"] {
            let draft = Draft {
                to: to.into(),
                cc: if to.is_empty() {
                    String::new()
                } else {
                    "\"Copy, Person\" <copy@example.test>; other@example.test".into()
                },
                bcc: "Hidden Person <hidden@example.test>".into(),
                ..Default::default()
            };
            let envelope = build_envelope(&account, &draft).unwrap();
            let recipients: Vec<_> = envelope.to().iter().map(ToString::to_string).collect();
            let expected = if to.is_empty() {
                vec!["hidden@example.test"]
            } else {
                vec![
                    "visible@example.test",
                    "copy@example.test",
                    "other@example.test",
                    "hidden@example.test",
                ]
            };
            assert_eq!(recipients, expected);
        }
    }

    /// Construct the SMTP envelope (return-path + recipients) from the account
    /// and draft. Recipients that don't parse as addresses are skipped.
    fn build_envelope(
        account: &AccountConfig,
        draft: &Draft,
    ) -> Result<lettre::address::Envelope, String> {
        use std::str::FromStr;
        let from = lettre::Address::from_str(account.email.trim())
            .map_err(|e| format!("invalid From address: {e}"))?;
        let mut to = Vec::new();
        for field in [&draft.to, &draft.cc, &draft.bcc] {
            for addr in crate::transport::parse_addr_list(field) {
                if let Ok(a) = lettre::Address::from_str(addr.email.trim()) {
                    to.push(a);
                }
            }
        }
        if to.is_empty() {
            return Err("no valid recipients".into());
        }
        lettre::address::Envelope::new(Some(from), to).map_err(|e| e.to_string())
    }

    // ---- real-path integration tests against local in-process servers ------
    //
    // These stand up a local IMAP and SMTP server over a genuine TLS socket
    // (a throwaway self-signed certificate) and drive the *production* engine
    // functions against them: `list_folders` / `sync_folder` for receive,
    // `smtp_send_with_tls` for send. They prove the two loops end to end over a
    // real socket — TLS handshake, authentication, the exact IMAP command
    // sequence, the `mail-parser` decode, and the `lettre` STARTTLS submission —
    // with no public server and no live credentials. The only test shim is that
    // the client accepts this one local certificate; the wire protocol and the
    // parsing are the real ones.
    #[cfg(test)]
    mod live_tests {
        use std::sync::{Arc, Mutex};

        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::{TcpListener, TcpStream};

        use super::{connect_session, list_folders, resolve_auth, smtp_send_with_tls, sync_folder};
        use crate::model::{AccountConfig, Addr, AuthMethod, Draft, Folder};
        use crate::transport::build_mime;

        /// The mock servers present a fresh self-signed certificate.
        fn accepting_client_config() -> Arc<rustls::ClientConfig> {
            Arc::new(tilo_net::pinned_client_config(|_| true).unwrap())
        }

        /// A fresh self-signed cert + rustls server config for the mock servers.
        fn server_tls_config() -> Arc<rustls::ServerConfig> {
            let cert = rcgen::generate_simple_self_signed(vec![
                "localhost".to_string(),
                "127.0.0.1".to_string(),
            ])
            .unwrap();
            let cert_der = cert.cert.der().clone();
            let key_der =
                rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
            let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert_der],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key_der),
            )
            .unwrap();
            Arc::new(cfg)
        }

        /// Bind a loopback listener on an ephemeral port; returns it plus the port.
        async fn bind_local() -> (TcpListener, u16) {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            (listener, port)
        }

        /// A scripted IMAP server serving exactly one message. It echoes each
        /// command's tag, so it matches whatever tag async-imap generates, and
        /// answers the LOGIN → LIST → SELECT → UID SEARCH → UID FETCH → LOGOUT
        /// sequence the engine issues.
        async fn serve_imap(tls: Arc<rustls::ServerConfig>, tcp: TcpStream, eml: Vec<u8>) {
            let acceptor = tokio_rustls::TlsAcceptor::from(tls);
            let Ok(stream) = acceptor.accept(tcp).await else {
                return;
            };
            let (rd, mut wr) = tokio::io::split(stream);
            let mut reader = BufReader::new(rd);
            if wr
                .write_all(b"* OK [CAPABILITY IMAP4rev1] test ready\r\n")
                .await
                .is_err()
            {
                return;
            }
            let _ = wr.flush().await;
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let tag = line.split_whitespace().next().unwrap_or("*").to_string();
                let up = line.to_ascii_uppercase();
                let mut resp: Vec<u8> = Vec::new();
                if up.contains("LOGIN") {
                    resp.extend_from_slice(format!("{tag} OK LOGIN completed\r\n").as_bytes());
                } else if up.contains("LOGOUT") {
                    resp.extend_from_slice(b"* BYE logging out\r\n");
                    resp.extend_from_slice(format!("{tag} OK LOGOUT completed\r\n").as_bytes());
                    let _ = wr.write_all(&resp).await;
                    let _ = wr.flush().await;
                    break;
                } else if up.contains("LIST") {
                    resp.extend_from_slice(b"* LIST (\\HasNoChildren) \".\" \"INBOX\"\r\n");
                    resp.extend_from_slice(b"* LIST (\\HasNoChildren) \".\" \"Sent\"\r\n");
                    resp.extend_from_slice(format!("{tag} OK LIST completed\r\n").as_bytes());
                } else if up.contains("SELECT") || up.contains("EXAMINE") {
                    resp.extend_from_slice(b"* 1 EXISTS\r\n");
                    resp.extend_from_slice(b"* 0 RECENT\r\n");
                    resp.extend_from_slice(b"* OK [UIDVALIDITY 7] UIDs valid\r\n");
                    resp.extend_from_slice(b"* FLAGS (\\Seen \\Flagged)\r\n");
                    resp.extend_from_slice(
                        format!("{tag} OK [READ-WRITE] SELECT completed\r\n").as_bytes(),
                    );
                } else if up.contains("UID SEARCH") || up.contains("SEARCH") {
                    resp.extend_from_slice(b"* SEARCH 1\r\n");
                    resp.extend_from_slice(format!("{tag} OK SEARCH completed\r\n").as_bytes());
                } else if up.contains("UID FETCH") || up.contains("FETCH") {
                    let head = format!(
                        "* 1 FETCH (UID 1 FLAGS (\\Seen) INTERNALDATE \"01-Jan-2021 09:30:00 +0000\" BODY[] {{{}}}\r\n",
                        eml.len()
                    );
                    resp.extend_from_slice(head.as_bytes());
                    resp.extend_from_slice(&eml);
                    resp.extend_from_slice(b")\r\n");
                    resp.extend_from_slice(format!("{tag} OK FETCH completed\r\n").as_bytes());
                } else {
                    resp.extend_from_slice(format!("{tag} OK completed\r\n").as_bytes());
                }
                if wr.write_all(&resp).await.is_err() {
                    break;
                }
                let _ = wr.flush().await;
            }
        }

        /// A scripted STARTTLS SMTP server — the path the engine drives for the
        /// non-465 submission port (`Tls::Required`): a plaintext greeting +
        /// EHLO advertising STARTTLS, the in-band TLS upgrade, then EHLO +
        /// AUTH LOGIN + MAIL/RCPT/DATA over TLS, capturing the DATA payload.
        /// The plaintext half of the test SMTP server: greeting, EHLO, and the
        /// STARTTLS upgrade. Returns the raw socket once the client has been
        /// told to start TLS, or `None` if the session ended first (QUIT, EOF,
        /// or a write error).
        async fn smtp_plaintext_until_starttls(tcp: TcpStream) -> Option<TcpStream> {
            let mut plain = BufReader::new(tcp);
            if plain
                .write_all(b"220 localhost ESMTP test\r\n")
                .await
                .is_err()
            {
                return None;
            }
            let _ = plain.flush().await;
            let mut line = String::new();
            loop {
                line.clear();
                match plain.read_line(&mut line).await {
                    Ok(0) | Err(_) => return None,
                    Ok(_) => {}
                }
                let up = line.trim_end().to_ascii_uppercase();
                if up.starts_with("EHLO") || up.starts_with("HELO") {
                    let _ = plain
                        .write_all(b"250-localhost\r\n250-STARTTLS\r\n250 8BITMIME\r\n")
                        .await;
                    let _ = plain.flush().await;
                } else if up.starts_with("STARTTLS") {
                    let _ = plain.write_all(b"220 2.0.0 Ready to start TLS\r\n").await;
                    let _ = plain.flush().await;
                    break;
                } else if up.starts_with("QUIT") {
                    let _ = plain.write_all(b"221 2.0.0 Bye\r\n").await;
                    let _ = plain.flush().await;
                    return None;
                } else {
                    let _ = plain.write_all(b"250 2.0.0 Ok\r\n").await;
                    let _ = plain.flush().await;
                }
            }

            // The client waits for the "220 Ready" before its ClientHello, so no
            // application bytes are buffered past STARTTLS: the raw socket can be
            // handed to the TLS acceptor cleanly.
            Some(plain.into_inner())
        }

        async fn serve_smtp(
            tls: Arc<rustls::ServerConfig>,
            tcp: TcpStream,
            captured: Arc<Mutex<Vec<u8>>>,
        ) {
            let Some(tcp) = smtp_plaintext_until_starttls(tcp).await else {
                return;
            };
            let acceptor = tokio_rustls::TlsAcceptor::from(tls);
            let Ok(stream) = acceptor.accept(tcp).await else {
                return;
            };
            let (rd, mut wr) = tokio::io::split(stream);
            let mut reader = BufReader::new(rd);

            // ---- encrypted phase: EHLO, AUTH LOGIN, MAIL/RCPT/DATA ----
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let up = line.trim_end().to_ascii_uppercase();
                if up.starts_with("EHLO") || up.starts_with("HELO") {
                    let _ = wr
                        .write_all(b"250-localhost\r\n250-AUTH LOGIN\r\n250 8BITMIME\r\n")
                        .await;
                } else if up.starts_with("AUTH LOGIN") {
                    // base64("Username:") then base64("Password:").
                    let _ = wr.write_all(b"334 VXNlcm5hbWU6\r\n").await;
                    let _ = wr.flush().await;
                    line.clear();
                    let _ = reader.read_line(&mut line).await;
                    let _ = wr.write_all(b"334 UGFzc3dvcmQ6\r\n").await;
                    let _ = wr.flush().await;
                    line.clear();
                    let _ = reader.read_line(&mut line).await;
                    let _ = wr
                        .write_all(b"235 2.7.0 Authentication successful\r\n")
                        .await;
                } else if up.starts_with("MAIL FROM") {
                    let _ = wr.write_all(b"250 2.1.0 Ok\r\n").await;
                } else if up.starts_with("RCPT TO") {
                    let _ = wr.write_all(b"250 2.1.5 Ok\r\n").await;
                } else if up.starts_with("DATA") {
                    let _ = wr
                        .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                        .await;
                    let _ = wr.flush().await;
                    let mut data = Vec::new();
                    loop {
                        let mut dline = Vec::new();
                        match reader.read_until(b'\n', &mut dline).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                        if dline == b".\r\n" {
                            break;
                        }
                        data.extend_from_slice(&dline);
                    }
                    *captured.lock().unwrap() = data;
                    let _ = wr.write_all(b"250 2.0.0 Ok: queued\r\n").await;
                } else if up.starts_with("QUIT") {
                    let _ = wr.write_all(b"221 2.0.0 Bye\r\n").await;
                    let _ = wr.flush().await;
                    break;
                } else {
                    let _ = wr.write_all(b"250 2.0.0 Ok\r\n").await;
                }
                let _ = wr.flush().await;
            }
        }

        #[test]
        #[allow(
            clippy::too_many_lines,
            reason = "one scenario test keeps each merged phase and its fresh fixture visible"
        )]
        fn imap_fetch_and_smtp_send_work_over_real_sockets() {
            // Phase `imap_fetch_over_a_real_socket_parses_a_message`.
            {
                let eml = b"From: Ada Lovelace <ada@analytical.example>\r\n\
    To: you@example.test\r\n\
    Subject: Live inbox works\r\n\
    Date: Fri, 01 Jan 2021 09:30:00 +0000\r\n\
    Message-ID: <live-1@analytical.example>\r\n\
    \r\n\
    This message was fetched over a real IMAP socket.\r\n"
                    .to_vec();

                ui::rt::block_on(async move {
                    let server_tls = server_tls_config();
                    let (listener, port) = bind_local().await;
                    let eml_for_server = eml.clone();
                    let server = tokio::spawn(async move {
                        while let Ok((tcp, _)) = listener.accept().await {
                            let tls = server_tls.clone();
                            let eml = eml_for_server.clone();
                            tokio::spawn(async move { serve_imap(tls, tcp, eml).await });
                        }
                    });

                    let account = AccountConfig {
                        email: "me@example.com".into(),
                        username: "me@example.com".into(),
                        password: "app-password".into(),
                        imap_host: "127.0.0.1".into(),
                        imap_port: port,
                        auth: AuthMethod::Password,
                        ..Default::default()
                    };
                    let creds =
                        resolve_auth(&account_custody::Custodian::live(), &account).unwrap();
                    let client_tls = accepting_client_config();

                    // One signed-in session serves every request: LIST
                    // advertises the server's mailboxes …
                    let mut session = connect_session(&account, &creds, client_tls)
                        .await
                        .expect("sign in over TLS");
                    let folders = list_folders(&mut session)
                        .await
                        .expect("list folders over TLS");
                    assert!(
                        folders.iter().any(|(name, _)| name == "INBOX"),
                        "server folders: {folders:?}"
                    );
                    let places = crate::sync::Places::from_list(&folders);
                    assert_eq!(places.name(Folder::Sent), "Sent");

                    // … and the first sync of a folder fetches its newest page
                    // and parses the RFC 5322 message into our model.
                    let synced = sync_folder(
                        &mut session,
                        &account,
                        Folder::Inbox,
                        &places,
                        crate::sync::Want::Latest { known: None },
                    )
                    .await
                    .expect("fetch INBOX over TLS");
                    assert!(synced.reset, "a first sync replaces what was held");
                    assert_eq!(synced.uid_validity, 7);
                    assert_eq!(synced.has_older, Some(false));
                    let msgs = synced.messages;
                    assert_eq!(msgs.len(), 1, "the one served message is fetched");
                    let m = &msgs[0];
                    assert_eq!(m.account, "me@example.com");
                    assert_eq!(
                        m.server,
                        Some(crate::model::ServerPlace {
                            folder: Folder::Inbox,
                            uid: 1,
                            uid_validity: 7
                        })
                    );
                    assert_eq!(m.subject, "Live inbox works");
                    assert_eq!(m.from.email, "ada@analytical.example");
                    assert!(
                        m.body.contains("fetched over a real IMAP socket"),
                        "body: {:?}",
                        m.body
                    );
                    assert!(m.read, "the server's \\Seen flag maps to read");
                    // The INTERNALDATE the server returned (2021-01-01 09:30 UTC) is
                    // what parse_fetched used for the timestamp.
                    assert_eq!(m.timestamp_min, 1_609_493_400 / 60);

                    server.abort();
                })
                .unwrap();
            }

            // Phase `smtp_send_over_a_real_socket_delivers_the_mime`.
            {
                ui::rt::block_on(async move {
                    let server_tls = server_tls_config();
                    let (listener, port) = bind_local().await;
                    let captured = Arc::new(Mutex::new(Vec::<u8>::new()));
                    let captured_srv = captured.clone();
                    let server = tokio::spawn(async move {
                        while let Ok((tcp, _)) = listener.accept().await {
                            let tls = server_tls.clone();
                            let cap = captured_srv.clone();
                            tokio::spawn(async move { serve_smtp(tls, tcp, cap).await });
                        }
                    });

                    let account = AccountConfig {
                        display_name: "Me".into(),
                        email: "me@example.com".into(),
                        username: "me@example.com".into(),
                        password: "app-password".into(),
                        smtp_host: "127.0.0.1".into(),
                        smtp_port: port,
                        auth: AuthMethod::Password,
                        ..Default::default()
                    };
                    let draft = Draft {
                        to: "rcpt@example.com".into(),
                        subject: "SMTP loop works".into(),
                        body: "Sent over a real SMTP socket.".into(),
                        ..Default::default()
                    };
                    let from = Addr::new(account.display_name.clone(), account.email.clone());
                    let mime = build_mime(&tilo_content::ContentStore::memory(), &from, &draft)
                        .expect("build MIME");
                    let creds =
                        resolve_auth(&account_custody::Custodian::live(), &account).unwrap();

                    // Send: the production submission path over a real STARTTLS
                    // upgrade (the non-465 submission-port behaviour), authenticating
                    // with AUTH LOGIN and delivering via DATA.
                    let params = lettre::transport::smtp::client::TlsParameters::builder(
                        "127.0.0.1".to_string(),
                    )
                    .dangerous_accept_invalid_certs(true)
                    .build()
                    .unwrap();
                    smtp_send_with_tls(&account, &draft, &creds, &mime, Some(params))
                        .await
                        .expect("SMTP submit over STARTTLS");

                    let received = captured.lock().unwrap().clone();
                    assert!(!received.is_empty(), "server captured the DATA payload");
                    let text = String::from_utf8_lossy(&received);
                    assert!(text.contains("Subject: SMTP loop works"), "payload: {text}");
                    assert!(text.contains("me@example.com"), "From reached the server");
                    assert!(text.contains("rcpt@example.com"), "To reached the server");
                    assert!(
                        text.contains("Sent over a real SMTP socket."),
                        "body reached the server"
                    );

                    server.abort();
                })
                .unwrap();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn stub_engine_reports_offline_and_never_claims_delivery() {
        // Phase `stub_send_never_claims_delivery`.
        {
            let (tx, rx) = spawn_with(Box::new(StubEngine::new(ContentStore::memory())));
            let account = AccountConfig {
                display_name: "Me".into(),
                email: "me@example.test".into(),
                ..Default::default()
            };
            let draft = Draft {
                to: "you@there.example".into(),
                subject: "Hi".into(),
                body: "Body".into(),
                ..Default::default()
            };
            tx.send(EngineRequest::send(account, Box::new(draft)))
                .unwrap();
            let evt = rx.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            assert!(matches!(evt, EngineEvent::Error { context, message, .. }
                if context == "send" && message.contains("validated") && message.contains("did not send")));
            assert!(matches!(
                rx.recv_timeout(tilo_test_env::hang_bound()).unwrap(),
                EngineEvent::Completed {
                    kind: EngineRequestKind::Send,
                    ..
                }
            ));
        }

        // Phase `stub_connect_reports_offline_honestly`.
        {
            let (tx, rx) = spawn_with(Box::new(StubEngine::new(ContentStore::memory())));
            tx.send(EngineRequest::connect(AccountConfig {
                email: "a@b.example".into(),
                ..Default::default()
            }))
            .unwrap();
            let evt = rx.recv_timeout(tilo_test_env::hang_bound()).unwrap();
            assert!(matches!(evt, EngineEvent::Error { context, .. } if context == "offline"));
            assert!(matches!(
                rx.recv_timeout(tilo_test_env::hang_bound()).unwrap(),
                EngineEvent::Completed {
                    kind: EngineRequestKind::Connect,
                    ..
                }
            ));
        }
    }
}
