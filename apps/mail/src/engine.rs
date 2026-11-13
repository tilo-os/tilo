//! The account engine: the worker seam that connects Mail to real IMAP and SMTP servers, off
//! the synchronous `ui` event loop.
//!
//! Every round trip runs on a worker thread; the UI posts [`EngineRequest`]s and drains
//! [`EngineEvent`]s. With the `live` feature the worker uses `async-imap` and `lettre`;
//! without it, and in the screenshot and test constructors, it answers from the bundled sample
//! corpus and checks sends by building real MIME, never opening a socket. Both speak the same
//! [`MailEngine`] trait.

use std::sync::mpsc::{Receiver, Sender};

use tilo_content::ContentStore;

use crate::model::{AccountConfig, Draft, Folder, Message};
use crate::oauth::OAuthProvider;
use crate::transport::build_mime;

pub type EngineRequestId = uuid::Uuid;

fn next_request_id() -> EngineRequestId {
    // Unlike a wrapping integer counter, a freshly-minted UUID cannot alias a
    // previous in-flight request after a counter exhaustion boundary. The
    // request id carries no account or message data.
    uuid::Uuid::new_v4()
}

#[derive(Clone, Debug)]
pub enum EngineRequest {
    Connect {
        request_id: EngineRequestId,
        account: AccountConfig,
    },
    Fetch {
        request_id: EngineRequestId,
        account: AccountConfig,
        folder: Folder,
        want: crate::sync::Want,
    },
    StoreFlag {
        request_id: EngineRequestId,
        account: AccountConfig,
        message: Box<Message>,
    },
    Move {
        request_id: EngineRequestId,
        account: AccountConfig,
        message: Box<Message>,
        to: Folder,
    },
    Purge {
        request_id: EngineRequestId,
        account: AccountConfig,
        message: Box<Message>,
    },
    SaveDraft {
        request_id: EngineRequestId,
        account: AccountConfig,
        draft: Box<Draft>,
    },
    Send {
        request_id: EngineRequestId,
        account: AccountConfig,
        draft: Box<Draft>,
    },
    Search {
        request_id: EngineRequestId,
        account: AccountConfig,
        text: String,
    },
    Watch {
        request_id: EngineRequestId,
        account: AccountConfig,
    },
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
    pub fn save_draft(account: AccountConfig, draft: Box<Draft>) -> Self {
        Self::SaveDraft {
            request_id: next_request_id(),
            account,
            draft,
        }
    }
    pub fn send(account: AccountConfig, draft: Box<Draft>) -> Self {
        Self::Send {
            request_id: next_request_id(),
            account,
            draft,
        }
    }
    pub fn search(account: AccountConfig, text: String) -> Self {
        Self::Search {
            request_id: next_request_id(),
            account,
            text,
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
            | Self::SaveDraft { request_id, .. }
            | Self::Send { request_id, .. }
            | Self::Watch { request_id, .. }
            | Self::Search { request_id, .. }
            | Self::OAuthAuthorize { request_id, .. } => *request_id,
        }
    }
    pub fn account(&self) -> &str {
        match self {
            Self::Connect { account, .. }
            | Self::Fetch { account, .. }
            | Self::StoreFlag { account, .. }
            | Self::Move { account, .. }
            | Self::Purge { account, .. }
            | Self::SaveDraft { account, .. }
            | Self::Send { account, .. }
            | Self::Watch { account, .. }
            | Self::Search { account, .. } => &account.email,
            Self::OAuthAuthorize { account_id, .. } => account_id,
        }
    }
    pub const fn kind(&self) -> EngineRequestKind {
        match self {
            Self::Connect { .. } => EngineRequestKind::Connect,
            Self::Fetch { .. } => EngineRequestKind::Fetch,
            Self::StoreFlag { .. } => EngineRequestKind::StoreFlag,
            Self::Move { .. } => EngineRequestKind::Move,
            Self::Purge { .. } => EngineRequestKind::Purge,
            Self::SaveDraft { .. } => EngineRequestKind::SaveDraft,
            Self::Send { .. } => EngineRequestKind::Send,
            Self::Watch { .. } => EngineRequestKind::Watch,
            Self::Search { .. } => EngineRequestKind::Search,
            Self::OAuthAuthorize { .. } => EngineRequestKind::OAuthAuthorize,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineRequestKind {
    Connect,
    Fetch,
    StoreFlag,
    Move,
    Purge,
    SaveDraft,
    Send,
    Watch,
    Search,
    OAuthAuthorize,
}

/// The context of an [`EngineEvent::Error`] of a send whose SMTP server could not be reached.
pub const SMTP_UNREACHED: &str = "smtp.unreached";

#[derive(Clone, Debug)]
pub enum EngineEvent {
    Connected {
        request_id: EngineRequestId,
        account_id: String,
    },
    Folders {
        request_id: EngineRequestId,
        account_id: String,
        folders: Vec<String>,
    },
    Fetched {
        request_id: EngineRequestId,
        account_id: String,
        folder: Folder,
        synced: crate::sync::Synced,
    },
    Found {
        request_id: EngineRequestId,
        account_id: String,
        messages: Vec<Message>,
    },
    Arrived {
        request_id: EngineRequestId,
        account_id: String,
    },
    Sent {
        request_id: EngineRequestId,
        wire_bytes: usize,
    },
    Authorized {
        request_id: EngineRequestId,
        account_id: String,
    },
    Error {
        request_id: EngineRequestId,
        kind: EngineRequestKind,
        context: String,
        message: String,
    },
    Completed {
        request_id: EngineRequestId,
        kind: EngineRequestKind,
    },
}

impl EngineEvent {
    pub const fn request_id(&self) -> EngineRequestId {
        match self {
            Self::Connected { request_id, .. }
            | Self::Folders { request_id, .. }
            | Self::Fetched { request_id, .. }
            | Self::Sent { request_id, .. }
            | Self::Arrived { request_id, .. }
            | Self::Found { request_id, .. }
            | Self::Authorized { request_id, .. }
            | Self::Error { request_id, .. }
            | Self::Completed { request_id, .. } => *request_id,
        }
    }
}

pub trait MailEngine: Send {
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

pub struct StubEngine {
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
                    message: crate::i18n::t_args(
                        "mail-refresh-offline",
                        &[
                            (
                                "folder",
                                ui::i18n::ArgValue::Text(crate::i18n::t(folder.message())),
                            ),
                            ("account", ui::i18n::ArgValue::Text(account.email)),
                        ],
                    ),
                });
            }
            EngineRequest::StoreFlag { .. }
            | EngineRequest::Move { .. }
            | EngineRequest::Purge { .. }
            | EngineRequest::SaveDraft { .. } => {
                // Local CRDT changes remain available offline, but the user
                // must be told that the remote IMAP write was not performed.
                let _ = out.send(EngineEvent::Error {
                    request_id,
                    kind,
                    context: "offline".into(),
                    message: "Local change saved; it was not synchronized to IMAP".into(),
                });
            }
            // Offline there is no server to watch or search; the request
            // just ends.
            EngineRequest::Watch { .. } | EngineRequest::Search { .. } => {}
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

    pub struct LiveEngine {
        rt: &'static ui::rt::AppRuntime,
        tls: std::sync::OnceLock<Arc<rustls::ClientConfig>>,
        trusts_test_servers: bool,
        content: tilo_content::ContentStore,
        custodian: Custodian,
        sessions: std::collections::HashMap<String, (ImapSession, std::time::Instant)>,
        places: std::collections::HashMap<String, crate::sync::Places>,
        watches: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,
        wake: super::Wake,
    }

    const IDLE_CHECK: std::time::Duration = std::time::Duration::from_secs(60);
    const NOOP_ANSWER: std::time::Duration = std::time::Duration::from_secs(10);

    type SessionFuture<'s, T> = futures_util::future::LocalBoxFuture<'s, Result<T, String>>;

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
                tls: std::sync::OnceLock::new(),
                trusts_test_servers: false,
                content,
                custodian: Custodian::live(),
                sessions: std::collections::HashMap::new(),
                places: std::collections::HashMap::new(),
                watches: std::collections::HashMap::new(),
                wake,
            })
        }

        #[cfg(any(test, feature = "fake-servers"))]
        pub fn trusting(
            content: tilo_content::ContentStore,
            wake: super::Wake,
            tls: Arc<rustls::ClientConfig>,
        ) -> Result<Self, String> {
            let mut engine = Self::new(content, wake)?;
            engine.tls = std::sync::OnceLock::from(tls);
            engine.trusts_test_servers = true;
            Ok(engine)
        }

        fn tls(&self) -> Result<Arc<rustls::ClientConfig>, String> {
            if let Some(tls) = self.tls.get() {
                return Ok(Arc::clone(tls));
            }
            let built = build_tls_config()?;
            Ok(Arc::clone(self.tls.get_or_init(|| built)))
        }

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
            let tls = match self.tls() {
                Ok(tls) => tls,
                Err(error) => {
                    tracing::debug!(%error, "mail: the inbox is not watched");
                    return;
                }
            };
            let task = self.rt.spawn(watch_inbox(account, tls, arrived));
            self.watches.insert(key, task);
        }

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
                // The timer is made inside the wait: this worker thread is in no runtime's
                // context, and a timer made outside one panics.
                let alive = rt
                    .block_on(async { tokio::time::timeout(NOOP_ANSWER, session.noop()).await })
                    .is_ok_and(|answer| answer.is_ok_and(|noop| noop.is_ok()));
                if !alive {
                    self.sessions.remove(&key);
                }
            }
            if !self.sessions.contains_key(&key) {
                let creds = resolve_auth(&self.custodian, account)?;
                let session = self.block_on(connect_session(account, &creds, self.tls()?))?;
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

        fn block_on<T>(
            &self,
            future: impl std::future::Future<Output = Result<T, String>>,
        ) -> Result<T, String> {
            self.rt.block_on(future).map_err(|e| e.to_string())?
        }

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

        fn handle_search(
            &mut self,
            account: &AccountConfig,
            text: &str,
        ) -> Result<Vec<Message>, String> {
            let places = self.places_of(account)?;
            let owner = account.clone();
            let text = text.to_owned();
            self.run(account, move |session| {
                let account = owner.clone();
                let places = places.clone();
                let text = text.clone();
                Box::pin(async move { search_server(session, &account, &places, &text).await })
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

        fn handle_write(&mut self, write: EngineRequest) -> (&'static str, Result<(), String>) {
            match write {
                EngineRequest::StoreFlag {
                    account, message, ..
                } => ("flag", self.handle_store(&account, &message)),
                EngineRequest::Move {
                    account,
                    message,
                    to,
                    ..
                } => ("move", self.handle_move(&account, &message, to)),
                EngineRequest::Purge {
                    account, message, ..
                } => ("purge", self.handle_purge(&account, &message)),
                EngineRequest::SaveDraft { account, draft, .. } => {
                    ("save-draft", self.handle_save_draft(&account, &draft))
                }
                _ => ("write", Err("not a write a server is told".into())),
            }
        }

        fn handle_save_draft(
            &mut self,
            account: &AccountConfig,
            draft: &Draft,
        ) -> Result<(), String> {
            let places = self.places_of(account)?;
            let from = crate::model::Addr::new(account.display_name.clone(), account.email.clone());
            let mime = build_mime(&self.content, &from, draft)?;
            let folder = places.name(Folder::Drafts).to_owned();
            self.run(account, move |session| {
                let (mime, folder) = (mime.clone(), folder.clone());
                Box::pin(async move {
                    session
                        .append(&folder, Some("(\\Seen \\Draft)"), None, &mime)
                        .await
                        .map_err(|e| format!("the draft was not kept in {folder}: {e}"))
                })
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

        fn handle_send(
            &self,
            account: &AccountConfig,
            draft: &Draft,
        ) -> Result<usize, SmtpFailure> {
            let creds = resolve_auth(&self.custodian, account).map_err(SmtpFailure::Failed)?;
            let from = Addr::new(account.display_name.clone(), account.email.clone());
            let bytes = build_mime(&self.content, &from, draft).map_err(SmtpFailure::Failed)?;
            let tls = if self.trusts_test_servers {
                Some(
                    lettre::transport::smtp::client::TlsParameters::builder(
                        account.smtp_host.trim().to_owned(),
                    )
                    .dangerous_accept_invalid_certs(true)
                    .build()
                    .map_err(|e| SmtpFailure::Failed(e.to_string()))?,
                )
            } else {
                None
            };
            self.block_on(async {
                Ok(smtp_send_with_tls(account, draft, &creds, &bytes, tls).await)
            })
            .map_err(SmtpFailure::Failed)??;
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
                Err(failure) => {
                    let (context, message) = match failure {
                        SmtpFailure::Unreached(message) => (super::SMTP_UNREACHED, message),
                        SmtpFailure::Failed(message) => ("smtp", message),
                    };
                    let _ = out.send(EngineEvent::Error {
                        request_id,
                        kind,
                        context: context.into(),
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
                write @ (EngineRequest::StoreFlag { .. }
                | EngineRequest::Move { .. }
                | EngineRequest::Purge { .. }
                | EngineRequest::SaveDraft { .. }) => {
                    let (context, result) = self.handle_write(write);
                    super::report_engine_error(out, request_id, kind, context, result);
                }
                EngineRequest::Send { account, draft, .. } => {
                    self.process_send(request_id, kind, &account, &draft, out);
                }
                EngineRequest::Watch { account, .. } => {
                    self.start_watch(request_id, account, out);
                }
                EngineRequest::Search { account, text, .. } => {
                    match self.handle_search(&account, &text) {
                        Ok(messages) => {
                            let _ = out.send(EngineEvent::Found {
                                request_id,
                                account_id: account.email,
                                messages,
                            });
                        }
                        Err(message) => {
                            let _ = out.send(EngineEvent::Error {
                                request_id,
                                kind,
                                context: "search".into(),
                                message,
                            });
                        }
                    }
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

    fn build_tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
        tilo_net::client_config()
            .map(Arc::new)
            .map_err(|error| format!("the system's trusted certificates are unreadable: {error}"))
    }

    fn login_user(account: &AccountConfig) -> String {
        if account.username.trim().is_empty() {
            account.email.clone()
        } else {
            account.username.clone()
        }
    }

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

    type ImapSession = async_imap::Session<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

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

    const IDLE_RENEW: std::time::Duration = std::time::Duration::from_secs(25 * 60);
    const WATCH_RETRY: std::time::Duration = std::time::Duration::from_secs(15);
    const WATCH_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(10 * 60);

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
        // What arrived while no watch held the inbox — between the last fetch and this
        // connection, or while the watch was away — is fetched now: IDLE tells only what comes
        // after it.
        arrived();
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
            synced.receive(fetch_uids(session, account, folder, validity, &uids, now_min).await?);
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
                synced.receive(
                    fetch_set(session, account, folder, validity, &range, now_min)
                        .await?
                        .into_iter()
                        .filter(|(message, _)| {
                            message.server.is_some_and(|at| at.uid > known.newest)
                        })
                        .collect(),
                );
                synced.flags = fetch_flags(session, known.oldest, known.newest).await?;
                synced.window = Some((known.oldest, known.newest));
            }
            Want::Latest { .. } => {
                // Nothing held, the folder renumbered, or too much missed: start over.
                synced.reset = true;
                let first = exists.saturating_sub(PAGE - 1).max(1);
                synced.receive(
                    fetch_seq(session, account, folder, validity, first, exists, now_min).await?,
                );
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
                synced.receive(
                    fetch_seq(session, account, folder, validity, first, last, now_min).await?,
                );
                synced.has_older = Some(first > 1);
            }
            Want::Older { .. } => {
                synced.reset = true;
                let first = exists.saturating_sub(PAGE - 1).max(1);
                synced.receive(
                    fetch_seq(session, account, folder, validity, first, exists, now_min).await?,
                );
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
    ) -> Result<Vec<(Message, Vec<u8>)>, String> {
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
    ) -> Result<Vec<(Message, Vec<u8>)>, String> {
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
    ) -> Result<Vec<(Message, Vec<u8>)>, String> {
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

    const SEARCH_PAGE: usize = 25;

    async fn search_server(
        session: &mut ImapSession,
        account: &AccountConfig,
        places: &crate::sync::Places,
        text: &str,
    ) -> Result<Vec<Message>, String> {
        let Some(quoted) = crate::sync::imap_quoted(text) else {
            return Ok(Vec::new());
        };
        let now_min = crate::state::now_minutes();
        let mut found = Vec::new();
        for folder in [Folder::Inbox, Folder::Archive] {
            let mailbox = places.name(folder);
            let Ok(selected) = session.select(mailbox).await else {
                continue;
            };
            let validity = selected.uid_validity.unwrap_or(0);
            let mut uids: Vec<u32> = session
                .uid_search(format!("TEXT {quoted}"))
                .await
                .map_err(|e| e.to_string())?
                .into_iter()
                .collect();
            uids.sort_unstable();
            let newest: Vec<u32> = uids.into_iter().rev().take(SEARCH_PAGE).collect();
            let mut messages: Vec<Message> =
                fetch_uids(session, account, folder, validity, &newest, now_min)
                    .await?
                    .into_iter()
                    .map(|(message, _)| message)
                    .collect();
            // Outside the window this device syncs: no server place, so a
            // refresh neither counts nor removes them.
            for message in &mut messages {
                message.server = None;
            }
            found.extend(messages);
        }
        Ok(found)
    }

    const FETCH_ITEMS: &str = "(UID FLAGS INTERNALDATE BODY.PEEK[])";

    fn parse_all(
        fetches: &[async_imap::types::Fetch],
        account: &AccountConfig,
        folder: Folder,
        validity: u32,
        now_min: i64,
    ) -> Vec<(Message, Vec<u8>)> {
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
                Some((message, body.to_vec()))
            })
            .collect()
    }

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

    #[derive(Debug)]
    pub(crate) enum SmtpFailure {
        Unreached(String),
        Failed(String),
    }

    async fn smtp_send_with_tls(
        account: &AccountConfig,
        draft: &Draft,
        creds: &AuthCreds,
        mime: &[u8],
        tls_override: Option<lettre::transport::smtp::client::TlsParameters>,
    ) -> Result<(), SmtpFailure> {
        use lettre::transport::smtp::authentication::{Credentials, Mechanism};
        use lettre::transport::smtp::client::TlsParameters;

        let host = account.smtp_host.trim();
        if host.is_empty() {
            return Err(SmtpFailure::Failed("no SMTP host".into()));
        }
        let port = if account.smtp_port == 0 {
            465
        } else {
            account.smtp_port
        };
        let params = match tls_override {
            Some(p) => p,
            None => TlsParameters::new(host.to_string())
                .map_err(|e| SmtpFailure::Failed(classify_conn_failure(&e.to_string())))?,
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
        let envelope = build_envelope(account, draft).map_err(SmtpFailure::Failed)?;
        // 465 → implicit TLS; anything else (587/25) → a required STARTTLS
        // upgrade. The connection races the host's addresses.
        let tls = build_tls_config().map_err(SmtpFailure::Failed)?;
        let mut connection = crate::connect::smtp(host, port, tls, params)
            .await
            .map_err(SmtpFailure::Unreached)?;
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
        sent.map(drop).map_err(SmtpFailure::Failed)
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

        use super::{
            LiveEngine, connect_session, list_folders, resolve_auth, smtp_send_with_tls,
            sync_folder,
        };
        use crate::model::{AccountConfig, Addr, AuthMethod, Draft, Folder};
        use crate::transport::build_mime;

        use crate::fake_servers::{accepting_client_config, serve_smtp, server_tls_config};

        async fn bind_local() -> (TcpListener, u16) {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            (listener, port)
        }

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

        fn mail(id: &str, subject: &str, body: &str) -> String {
            format!(
                "From: Ada <ada@analytical.example>\nTo: me@example.com\nSubject: {subject}\n\
Message-ID: <{id}@analytical.example>\nDate: Thu, 01 Jan 2026 09:00:00 +0000\n\n{body}\n"
            )
        }

        #[test]
        #[allow(
            clippy::too_many_lines,
            reason = "one account's life with a server, each step depending on the last"
        )]
        fn an_account_lives_against_gmail_and_outlook_layouts() {
            use crate::fake_imap::{FakeImap, Layout};
            use crate::sync::{Synced, Want, Window};
            use std::sync::mpsc::Receiver;

            fn next(events: &Receiver<super::EngineEvent>) -> super::EngineEvent {
                events
                    .recv_timeout(ui::testing::hang_bound())
                    .expect("the engine answers")
            }
            fn fetched(events: &Receiver<super::EngineEvent>) -> Synced {
                loop {
                    match next(events) {
                        super::EngineEvent::Fetched { synced, .. } => return synced,
                        super::EngineEvent::Error { message, .. } => panic!("{message}"),
                        _ => {}
                    }
                }
            }

            for (layout, archive) in [
                (Layout::Gmail, "[Gmail]/All Mail"),
                (Layout::Outlook, "Archive"),
            ] {
                let server =
                    // On the app runtime, which outlives this call, so the
                    // server keeps serving.
                    ui::rt::app()
                        .block_on(FakeImap::start(layout, server_tls_config()))
                        .unwrap();
                server.append(
                    archive,
                    &mail("old", "Quarterly numbers", "From last year."),
                    &["\\Seen"],
                );
                for n in 1..=3 {
                    server.append(
                        "INBOX",
                        &mail(&format!("m{n}"), &format!("Note {n}"), "Hello."),
                        &[],
                    );
                }
                let account = AccountConfig {
                    email: "me@example.com".into(),
                    username: "me@example.com".into(),
                    password: "app-password".into(),
                    imap_host: "127.0.0.1".into(),
                    imap_port: server.port,
                    auth: AuthMethod::Password,
                    ..Default::default()
                };
                let engine = LiveEngine::trusting(
                    tilo_content::ContentStore::memory(),
                    Arc::new(|| {}),
                    accepting_client_config(),
                )
                .unwrap();
                let (requests, events) = super::super::spawn_with(Box::new(engine));

                requests
                    .send(super::EngineRequest::fetch(account.clone(), Folder::Inbox))
                    .unwrap();
                let first = fetched(&events);
                assert!(first.reset, "{layout:?}");
                assert_eq!(first.messages.len(), 3);
                // What the server sent for each message comes with it, for
                // the person's other devices.
                assert_eq!(first.originals.len(), 3);
                for (id, original) in &first.originals {
                    let read = crate::transport::parse_eml(original, 0, Folder::Inbox).unwrap();
                    assert_eq!(read.id, *id);
                }
                let mut held = crate::model::Mailbox::default();
                for message in first.messages {
                    held.insert(message);
                }
                let known = Window::of(&held, "me@example.com", Folder::Inbox).unwrap();
                assert_eq!((known.oldest, known.newest), (1, 3));

                requests
                    .send(super::EngineRequest::watch(account.clone()))
                    .unwrap();
                ui::testing::wait_until("the inbox to be watched", || server.idling() > 0);
                server.append("INBOX", &mail("m4", "Fresh", "Just now."), &[]);
                server.flag("INBOX", 1, "\\Seen");
                loop {
                    if let super::EngineEvent::Arrived { account_id, .. } = next(&events) {
                        assert_eq!(account_id, "me@example.com");
                        break;
                    }
                }

                // A refresh brings only what is new, and what another client
                // read.
                requests
                    .send(super::EngineRequest::fetch_wanting(
                        account.clone(),
                        Folder::Inbox,
                        Want::Latest { known: Some(known) },
                    ))
                    .unwrap();
                let refresh = fetched(&events);
                assert!(!refresh.reset);
                assert_eq!(refresh.messages.len(), 1, "{:?}", refresh.messages);
                assert_eq!(refresh.messages[0].subject, "Fresh");
                assert_eq!(refresh.originals.len(), 1);
                assert!(
                    String::from_utf8_lossy(&refresh.originals[0].1).contains("Subject: Fresh")
                );
                assert!(
                    refresh.flags.contains(&(1, true, false)),
                    "{:?}",
                    refresh.flags
                );

                let archived = held
                    .messages
                    .values()
                    .find(|message| message.subject == "Note 3")
                    .cloned()
                    .unwrap();
                requests
                    .send(super::EngineRequest::move_message(
                        account.clone(),
                        Box::new(archived),
                        Folder::Archive,
                    ))
                    .unwrap();
                loop {
                    match next(&events) {
                        super::EngineEvent::Completed {
                            kind: super::EngineRequestKind::Move,
                            ..
                        } => break,
                        super::EngineEvent::Error { message, .. } => {
                            panic!("{layout:?}: {message}")
                        }
                        _ => {}
                    }
                }
                assert!(
                    server
                        .messages(archive)
                        .iter()
                        .any(|stored| String::from_utf8_lossy(&stored.raw).contains("Note 3")),
                    "{layout:?}: archived into {archive}"
                );
                assert_eq!(server.messages("INBOX").len(), 3);

                requests
                    .send(super::EngineRequest::search(
                        account.clone(),
                        "quarterly".into(),
                    ))
                    .unwrap();
                let found = loop {
                    match next(&events) {
                        super::EngineEvent::Found { messages, .. } => break messages,
                        super::EngineEvent::Error { message, .. } => panic!("{message}"),
                        _ => {}
                    }
                };
                assert!(
                    found
                        .iter()
                        .any(|message| message.subject == "Quarterly numbers"
                            && message.server.is_none()),
                    "{layout:?}: {found:?}"
                );
            }
        }

        #[test]
        #[allow(
            clippy::too_many_lines,
            reason = "one scenario test keeps each merged phase and its fresh fixture visible"
        )]
        fn imap_fetch_and_smtp_send_work_over_real_sockets() {
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
            let evt = rx.recv_timeout(ui::testing::hang_bound()).unwrap();
            assert!(matches!(evt, EngineEvent::Error { context, message, .. }
                if context == "send" && message.contains("validated") && message.contains("did not send")));
            assert!(matches!(
                rx.recv_timeout(ui::testing::hang_bound()).unwrap(),
                EngineEvent::Completed {
                    kind: EngineRequestKind::Send,
                    ..
                }
            ));
        }

        {
            let (tx, rx) = spawn_with(Box::new(StubEngine::new(ContentStore::memory())));
            tx.send(EngineRequest::connect(AccountConfig {
                email: "a@b.example".into(),
                ..Default::default()
            }))
            .unwrap();
            let evt = rx.recv_timeout(ui::testing::hang_bound()).unwrap();
            assert!(matches!(evt, EngineEvent::Error { context, .. } if context == "offline"));
            assert!(matches!(
                rx.recv_timeout(ui::testing::hang_bound()).unwrap(),
                EngineEvent::Completed {
                    kind: EngineRequestKind::Connect,
                    ..
                }
            ));
        }
    }
}
