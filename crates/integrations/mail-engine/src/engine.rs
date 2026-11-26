//! The account engine: the worker boundary that connects Mail to real IMAP and SMTP servers, off
//! the synchronous `ui` event loop.
//!
//! Every round trip runs on a worker thread; the UI posts [`EngineRequest`]s and drains
//! [`EngineEvent`]s. With the `live` feature the worker uses `async-imap` and `lettre`;
//! without it, and in the screenshot and test constructors, every request answers that the
//! live backend is absent, and a send is checked by building real MIME, never opening a
//! socket. Both speak the same [`MailEngine`] trait.

use std::sync::mpsc::{Receiver, Sender};

use tilo_content::ContentStore;

use crate::model::{AccountConfig, Draft, Folder, Message};
use crate::transport::build_mime_from;

pub type EngineRequestId = uuid::Uuid;

/// Who a message goes from: the identity its draft names (`Name <address>`, one of the
/// account's own), or the account's name and address when it names none.
fn draft_from(account: &AccountConfig, draft: &Draft) -> crate::model::Addr {
    let named = draft
        .sender
        .as_ref()
        .map(|sender| sender.email.trim())
        .filter(|identity| !identity.is_empty());
    match named {
        Some(identity) => match identity.rsplit_once('<') {
            Some((name, address)) => crate::model::Addr::new(
                name.trim().trim_matches('"'),
                address.trim_end_matches('>').trim(),
            ),
            None => crate::model::Addr::bare(identity),
        },
        None => crate::model::Addr::new(account.display_name.clone(), account.email.clone()),
    }
}

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
        /// The draft's attachments with their bytes, read where the draft is
        /// kept: what the message is written from.
        files: Vec<crate::model::SendFile>,
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
    pub fn send(
        account: AccountConfig,
        draft: Box<Draft>,
        files: Vec<crate::model::SendFile>,
    ) -> Self {
        Self::Send {
            request_id: next_request_id(),
            account,
            draft,
            files,
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
            | Self::Search { request_id, .. } => *request_id,
        }
    }
    /// The settings of the account the request is for.
    pub const fn account_config(&self) -> &AccountConfig {
        match self {
            Self::Connect { account, .. }
            | Self::Fetch { account, .. }
            | Self::StoreFlag { account, .. }
            | Self::Move { account, .. }
            | Self::Purge { account, .. }
            | Self::SaveDraft { account, .. }
            | Self::Send { account, .. }
            | Self::Watch { account, .. }
            | Self::Search { account, .. } => account,
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
}

/// The context of an [`EngineEvent::Error`] of a send whose SMTP server could not be reached.
pub const SMTP_UNREACHED: &str = "smtp.unreached";

/// What a request for a password account says when Mail holds no password for it: nothing was
/// sent to the server, so no server refused anything.
pub const NO_PASSWORD: &str = "Mail holds no password for this account, so it did not sign in";

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
    /// A message that left has no copy in its account's Sent folder: one was
    /// to be filed there and could not be. Told after [`Self::Sent`], which
    /// never waits for the copy.
    Unfiled {
        request_id: EngineRequestId,
        reason: String,
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
            | Self::Unfiled { request_id, .. }
            | Self::Arrived { request_id, .. }
            | Self::Found { request_id, .. }
            | Self::Error { request_id, .. }
            | Self::Completed { request_id, .. } => *request_id,
        }
    }
}

pub trait MailEngine: Send {
    fn handle(&mut self, req: EngineRequest, out: &Sender<EngineEvent>);
}

#[cfg(feature = "live")]
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

/// The engine a build without the live backend runs: it reaches no server,
/// and keeps no content — a send it is handed carries its files' bytes.
pub struct StubEngine {}

impl StubEngine {
    pub fn new(_content: ContentStore) -> Self {
        Self {}
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
            EngineRequest::Fetch { account, .. } => {
                let _ = out.send(EngineEvent::Error {
                    request_id,
                    kind,
                    context: "offline".into(),
                    message: format!(
                        "IMAP is unavailable for {}; rebuild with the live mail backend to fetch",
                        account.email
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
            EngineRequest::Send {
                account,
                draft,
                files,
                ..
            } => {
                let from = draft_from(&account, &draft);
                match build_mime_from(&from, &draft, &files) {
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
    use std::collections::HashMap;
    use std::sync::Arc;

    use tilo_mail_session::server::{self, ImapSession, MarkMail, Page, Places, Raw};
    use tilo_mail_session::{
        InboxHeard, InboxSeen, LiveMailSession, MailSession as _, SendFailure, Watching,
    };

    use super::{
        AccountConfig, Draft, EngineEvent, EngineRequest, EngineRequestId, EngineRequestKind,
        Folder, MailEngine, Message, Sender, draft_from,
    };
    use crate::sync::{CATCH_UP_LIMIT, Synced, Want};
    use crate::transport::{build_mime, build_mime_from, parse_fetched};

    /// The engine on Tilo's one mail client: the session that keeps each account signed in
    /// between requests, reaches its servers and watches its inbox, as it does for a
    /// workspace's host. What the engine adds is Mail's own model: the folder a message is
    /// in, its place on the server, and what the person's devices keep of it.
    pub struct LiveEngine {
        // The watches end before the session whose runtime they live on.
        watches: HashMap<String, Watching>,
        session: LiveMailSession,
        content: tilo_content::ContentStore,
        places: HashMap<String, Places>,
        wake: super::Wake,
    }

    /// The most messages a search answers from each folder.
    const SEARCH_PAGE: usize = 25;

    impl LiveEngine {
        /// An engine that trusts the system's certificate roots.
        ///
        /// # Errors
        /// None now; the signature stays the constructor every caller knows.
        pub fn new(content: tilo_content::ContentStore, wake: super::Wake) -> Result<Self, String> {
            Ok(Self::on(LiveMailSession::default(), content, wake))
        }

        /// An engine that also trusts `certificates` as roots: one whose test servers sign
        /// for themselves.
        ///
        /// # Errors
        /// As [`Self::new`].
        #[cfg(any(test, feature = "test-support"))]
        pub fn trusting(
            content: tilo_content::ContentStore,
            wake: super::Wake,
            certificates: Vec<rustls::pki_types::CertificateDer<'static>>,
        ) -> Result<Self, String> {
            Ok(Self::on(
                LiveMailSession::also_trusting(certificates),
                content,
                wake,
            ))
        }

        fn on(
            session: LiveMailSession,
            content: tilo_content::ContentStore,
            wake: super::Wake,
        ) -> Self {
            Self {
                watches: HashMap::new(),
                session,
                content,
                places: HashMap::new(),
                wake,
            }
        }

        /// Run `work` on the account's signed-in session.
        fn run<T>(
            &self,
            account: &AccountConfig,
            work: impl AsyncFnOnce(&mut ImapSession) -> Result<T, String>,
        ) -> Result<T, String> {
            let (signed, secret) = signed(account)?;
            self.session.on_session(&signed, &secret, work)
        }

        fn places_of(&mut self, account: &AccountConfig) -> Result<Places, String> {
            if let Some(places) = self.places.get(&account.email) {
                return Ok(places.clone());
            }
            let places = self.run(account, async |session: &mut ImapSession| {
                server::places(session).await
            })?;
            self.places.insert(account.email.clone(), places.clone());
            Ok(places)
        }

        fn start_watch(
            &mut self,
            request_id: EngineRequestId,
            account: &AccountConfig,
            out: &Sender<EngineEvent>,
        ) {
            // A watch already running for the account stops as it is let go.
            self.watches.remove(&account.email);
            let heard = Arc::new(Heard {
                out: out.clone(),
                request_id,
                account_id: account.email.clone(),
                wake: Arc::clone(&self.wake),
            });
            let watching = signed(account)
                .and_then(|(signed, secret)| self.session.watch(&signed, &secret, None, heard));
            match watching {
                Ok(watching) => {
                    self.watches.insert(account.email.clone(), watching);
                }
                Err(error) => tracing::debug!(%error, "mail: the inbox is not watched"),
            }
        }

        fn handle_fetch(
            &mut self,
            account: &AccountConfig,
            folder: Folder,
            want: Want,
        ) -> Result<Synced, String> {
            let places = self.places_of(account)?;
            let mailbox = places.name(folder.as_key()).unwrap_or("INBOX").to_owned();
            let page = match want {
                Want::Latest { known: None } => Page::Newest,
                Want::Latest { known: Some(known) } => Page::Since {
                    uid_validity: known.uid_validity,
                    newest: known.newest,
                    oldest: known.oldest,
                    catch_up: CATCH_UP_LIMIT,
                },
                Want::Older { known } => Page::Before {
                    uid_validity: known.uid_validity,
                    uid: known.oldest,
                },
            };
            let starred = folder == Folder::Starred;
            let read = self.run(account, async |session: &mut ImapSession| {
                server::read_page(session, &mailbox, starred, page).await
            })?;
            let mut synced = Synced {
                uid_validity: read.uid_validity,
                reset: read.reset,
                flags: read
                    .flags
                    .iter()
                    .map(|flag| (flag.uid, flag.read, flag.starred))
                    .collect(),
                window: read.window,
                has_older: read.has_older,
                ..Synced::default()
            };
            synced.receive(parsed(read.messages, account, folder, read.uid_validity));
            Ok(synced)
        }

        fn handle_search(
            &mut self,
            account: &AccountConfig,
            text: &str,
        ) -> Result<Vec<Message>, String> {
            // What a server cannot be asked is left to the search of what this device keeps.
            let Some(quoted) = server::quoted(text) else {
                return Ok(Vec::new());
            };
            let keys = format!("TEXT {quoted}");
            let places = self.places_of(account)?;
            let folders: Vec<(Folder, String)> = [Folder::Inbox, Folder::Archive]
                .into_iter()
                .filter_map(|folder| {
                    places
                        .listed(folder.as_key())
                        .map(|mailbox| (folder, mailbox.to_owned()))
                })
                .collect();
            let found = self.run(account, async |session: &mut ImapSession| {
                let mut found = Vec::new();
                for (folder, mailbox) in &folders {
                    let (uid_validity, raws) =
                        server::search_folder(session, mailbox, &keys, SEARCH_PAGE).await?;
                    found.push((*folder, uid_validity, raws));
                }
                Ok(found)
            })?;
            Ok(found
                .into_iter()
                .flat_map(|(folder, uid_validity, raws)| {
                    parsed(raws, account, folder, uid_validity)
                })
                .map(|(mut message, _)| {
                    // Outside the window this device syncs: no server place, so a refresh
                    // neither counts nor removes them.
                    message.server = None;
                    message
                })
                .collect())
        }

        fn mark(&mut self, account: &AccountConfig, marking: &MarkMail) -> Result<(), String> {
            let places = self.places_of(account)?;
            self.run(account, async |session: &mut ImapSession| {
                server::mark(session, &places, marking).await
            })
        }

        fn handle_write(&mut self, write: EngineRequest) -> (&'static str, Result<(), String>) {
            match write {
                EngineRequest::StoreFlag {
                    account, message, ..
                } => {
                    let marking = MarkMail {
                        read: Some(message.read),
                        starred: Some(message.starred),
                        ..marking(&message)
                    };
                    ("flag", self.mark(&account, &marking))
                }
                EngineRequest::Move {
                    account,
                    message,
                    to,
                    ..
                } => {
                    let marking = MarkMail {
                        to_folder: Some(to.as_key().to_owned()),
                        ..marking(&message)
                    };
                    ("move", self.mark(&account, &marking))
                }
                EngineRequest::Purge {
                    account, message, ..
                } => {
                    let marking = MarkMail {
                        delete: true,
                        ..marking(&message)
                    };
                    ("purge", self.mark(&account, &marking))
                }
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
            let from = draft_from(account, draft);
            let mime = build_mime(&self.content, &from, draft)?;
            let made = self.run(account, async |session: &mut ImapSession| {
                server::keep_draft(session, &places, &mime).await
            })?;
            self.made(account, made);
            Ok(())
        }

        /// Keep a copy of a message that left in the account's Sent folder
        /// ([`server::file_sent`]).
        fn file_sent(&mut self, account: &AccountConfig, mime: &[u8]) -> Result<(), String> {
            let places = self.places_of(account)?;
            let hosts = (account.imap_host.as_str(), account.smtp_host.as_str());
            let made = self.run(account, async |session: &mut ImapSession| {
                server::file_sent(session, &places, hosts, mime).await
            })?;
            self.made(account, made);
            Ok(())
        }

        /// A folder made here is listed with the server's own mark the next time its
        /// folders are read.
        fn made(&mut self, account: &AccountConfig, made: bool) {
            if made {
                self.places.remove(&account.email);
            }
        }

        fn handle_send(
            &self,
            account: &AccountConfig,
            draft: &Draft,
            files: &[crate::model::SendFile],
        ) -> Result<Vec<u8>, SendFailure> {
            let (signed, secret) = signed(account).map_err(SendFailure::Refused)?;
            // Who it goes from is the identity its draft names, or the account's own.
            let from = draft_from(account, draft);
            let bytes = build_mime_from(&from, draft, files).map_err(SendFailure::Refused)?;
            let envelope = build_envelope(account, draft).map_err(SendFailure::Refused)?;
            self.session.hand_over(&signed, &secret, envelope, &bytes)?;
            Ok(bytes)
        }

        fn process_connect(
            &mut self,
            request_id: EngineRequestId,
            kind: EngineRequestKind,
            account: AccountConfig,
            out: &Sender<EngineEvent>,
        ) {
            let listed = self.run(&account, async |session: &mut ImapSession| {
                server::list_folders(session).await
            });
            match listed {
                Ok(listed) => {
                    self.places
                        .insert(account.email.clone(), Places::from_list(&listed));
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
            &mut self,
            request_id: EngineRequestId,
            kind: EngineRequestKind,
            account: &AccountConfig,
            draft: &Draft,
            files: &[crate::model::SendFile],
            out: &Sender<EngineEvent>,
        ) {
            match self.handle_send(account, draft, files) {
                Ok(mime) => {
                    // The message left, and that is said first: a server
                    // slow to take the copy never holds back that it went
                    // out, and one that will not take it does not unsend it.
                    let _ = out.send(EngineEvent::Sent {
                        request_id,
                        wire_bytes: mime.len(),
                    });
                    (self.wake)();
                    if let Err(reason) = self.file_sent(account, &mime) {
                        let _ = out.send(EngineEvent::Unfiled { request_id, reason });
                    }
                }
                Err(failure) => {
                    let (context, message) = match failure {
                        SendFailure::Unreached(message) => (super::SMTP_UNREACHED, message),
                        SendFailure::Refused(message) => ("smtp", message),
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
                EngineRequest::Send {
                    account,
                    draft,
                    files,
                    ..
                } => {
                    self.process_send(request_id, kind, &account, &draft, &files, out);
                }
                EngineRequest::Watch { account, .. } => {
                    self.start_watch(request_id, &account, out);
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
            }
        }
    }

    /// What a watch of an account's inbox tells the engine's reader: that the inbox is worth
    /// reading again.
    struct Heard {
        out: Sender<EngineEvent>,
        request_id: EngineRequestId,
        account_id: String,
        wake: super::Wake,
    }

    impl Heard {
        fn tell(&self) {
            let _ = self.out.send(EngineEvent::Arrived {
                request_id: self.request_id,
                account_id: self.account_id.clone(),
            });
            (self.wake)();
        }
    }

    impl InboxHeard for Heard {
        // The first look tells no message, only that the watch starts: what arrived while
        // nothing watched is read then.
        fn arrived(&self, _: Vec<server::FetchedMessage>, _: InboxSeen) {
            self.tell();
        }

        fn lost(&self, reason: &str) {
            tracing::debug!(
                reason,
                "mail: the inbox watch stopped; connecting again later"
            );
        }

        fn changed(&self) {
            self.tell();
        }
    }

    /// The account as the mail client reads it, and its secret, as the engine was handed
    /// them: nothing signs in for an imported account that waits for its own sign-in, or
    /// without a secret.
    fn signed(account: &AccountConfig) -> Result<(accounts::Account, accounts::Secret), String> {
        if account.import_pending {
            return Err("this imported account needs its sign-in before it can connect".into());
        }
        if account.password.is_empty() {
            return Err(super::NO_PASSWORD.into());
        }
        let mut signed = accounts::Account::new(account.email.clone(), accounts::email::TYPE_ID);
        signed.config = account.to_value().map_err(|error| error.to_string())?;
        Ok((signed, accounts::Secret::password(account.password.clone())))
    }

    /// What a server is told about `message`: where it was read, and its `Message-ID` for when
    /// its folder was numbered anew since.
    fn marking(message: &Message) -> MarkMail {
        let at = message.server;
        MarkMail {
            account: message.account.clone(),
            folder: at
                .map_or(message.folder, |at| at.folder)
                .as_key()
                .to_owned(),
            uid: at.map_or(0, |at| at.uid),
            uid_validity: at.map_or(0, |at| at.uid_validity),
            message_id: message.rfc_message_id.clone().filter(|id| !id.is_empty()),
            ..MarkMail::default()
        }
    }

    /// The messages a server sent, read into Mail's model with where each is, and what the
    /// server sent for each, for the person's other devices.
    fn parsed(
        raws: Vec<Raw>,
        account: &AccountConfig,
        folder: Folder,
        uid_validity: u32,
    ) -> Vec<(Message, Vec<u8>)> {
        let now_min = crate::model::now_minutes();
        raws.into_iter()
            .filter_map(|raw| {
                let mut message = parse_fetched(
                    &raw.body,
                    folder,
                    raw.read,
                    raw.starred,
                    raw.minutes,
                    now_min,
                )?;
                message.account.clone_from(&account.email);
                message.server = Some(crate::model::ServerPlace {
                    folder,
                    uid: raw.uid,
                    uid_validity,
                });
                Some((message, raw.body))
            })
            .collect()
    }

    #[test]
    fn a_message_goes_from_the_identity_its_draft_names() {
        let account = AccountConfig {
            email: "sam@rivera.example".into(),
            display_name: "Sam Rivera".into(),
            ..Default::default()
        };
        // An account that sends as more than one name goes from the one its draft names, in the
        // message and in the delivery alike.
        let named = Draft {
            to: "ada@example.test".into(),
            sender: Some(crate::model::DraftSender {
                account: "sam@rivera.example".into(),
                email: "Ada at work <ada@work.example>".into(),
            }),
            ..Default::default()
        };
        let from = crate::engine::draft_from(&account, &named);
        assert_eq!(from.email, "ada@work.example");
        assert_eq!(from.display(), "Ada at work");
        let envelope = build_envelope(&account, &named).unwrap();
        assert_eq!(
            envelope.from().map(ToString::to_string),
            Some("ada@work.example".to_owned())
        );
        // A draft that names none goes from the account's own name and address.
        let plain = Draft {
            to: "ada@example.test".into(),
            ..Default::default()
        };
        let from = crate::engine::draft_from(&account, &plain);
        assert_eq!(from.email, "sam@rivera.example");
        assert_eq!(from.display(), "Sam Rivera");
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
        let from = lettre::Address::from_str(draft_from(account, draft).email.trim())
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

    // The live engine against the stand-in mail server: real sockets, real TLS with the
    // stand-ins' authority trusted, the exact IMAP command sequence, the `mail-parser`
    // decode and the `lettre` STARTTLS submission, with no public server and no real password.
}
