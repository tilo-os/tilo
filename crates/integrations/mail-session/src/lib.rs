//! A mail account's session with its servers: Mail's engine over IMAP and
//! SMTP, for the app's connector, which runs where the network and the
//! account's sign-in are.
//!
//! It reads a folder as a mail program does (the newest page of
//! messages, what arrived since the newest one the reader holds, or the
//! page before the oldest), reads one message, asks a server what it holds
//! that matches, tells a server what changed about a message, keeps a draft
//! there, and hands a written message to the server's SMTP endpoint. An
//! account's signed-in session is kept for its next request, so a reader
//! that asks often signs in once. Every step is bounded, and nothing here
//! keeps a secret or answers one.

mod leaving;
pub mod server;
mod shapes;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use tokio::time::timeout;
use web_time::Instant;

pub use crate::leaving::{Leaving, Recipient};
use crate::server::{ImapSession, PAGE, Raw};
pub use crate::shapes::{
    FetchMail, FetchedMail, FetchedMessage, FoundFolder, FoundMail, MAX_FOUND, MailAddress,
    MailAttachment, MailFlag, MarkMail, ReadMail, SearchMail, SendMail, SentMail,
};
/// How far behind the reader's newest message a read still catches up one
/// message at a time instead of starting the folder over.
const CATCH_UP_LIMIT: u32 = 200;
/// How long one session step may take.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a server may take over everything one request asks of it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
/// How long a kept session may rest before it is asked whether its server
/// still answers, and how long that answer may take.
const REST_CHECK: Duration = Duration::from_secs(30);
const NOOP_ANSWER: Duration = Duration::from_secs(10);
/// How long a session nobody uses is kept.
const KEPT_FOR: Duration = Duration::from_secs(5 * 60);
/// The most sessions kept at once, an account each.
const MAX_KEPT: usize = 64;
/// The most of its messages' words one answer carries. A frame holds a
/// megabyte, and what the messages say is written into it with room for
/// everything else the answer names.
const ANSWER_WORDS: usize = 512 * 1024;
/// The most of its words one message read on its own carries.
const READ_WORDS: usize = 768 * 1024;
/// The words kept of a message whose whole does not fit its answer.
const EXCERPT_WORDS: usize = 2 * 1024;
/// The most messages a watch tells at once: one that was away for long tells
/// the newest of what it missed.
const WATCH_MOST: u32 = 200;

/// What a host does with the accounts it keeps.
pub trait MailSession: Send + Sync {
    /// Read `asking`'s folder as the server holds it.
    ///
    /// # Errors
    /// The account's server could not be reached, refused the sign-in, or
    /// answered something this cannot read.
    fn fetch(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        asking: &FetchMail,
    ) -> Result<FetchedMail, String>;

    /// Read the one message `reading` names, as its server holds it.
    ///
    /// # Errors
    /// As [`Self::fetch`], or the message is no longer where it was read.
    fn read(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        reading: &ReadMail,
    ) -> Result<FetchedMessage, String>;

    /// Ask the account's server which messages it holds that match
    /// `searching`, newest first in each folder.
    ///
    /// # Errors
    /// As [`Self::fetch`], or the search names nothing a server can be asked.
    fn search(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        searching: &SearchMail,
    ) -> Result<FoundMail, String>;

    /// Keep `draft` in the account's Drafts folder, with the `files` it
    /// carries and the calendar `invitation` it brings. Nothing is sent.
    ///
    /// # Errors
    /// The server could not be reached, refused the sign-in or the draft.
    fn save_draft(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        draft: &SendMail,
        files: &[Attachment],
        invitation: Option<&str>,
    ) -> Result<(), String>;

    /// Hand `sending` to its server, with the `files` it carries and the
    /// calendar `invitation` it brings, and keep a copy in the account's
    /// Sent folder unless its server files what it sends. It goes to the
    /// recipients `sending` was read with ([`Leaving::read`]) and to nobody
    /// else: whoever showed the message to a person first showed them those.
    ///
    /// # Errors
    /// The server could not be reached, refused the sign-in or the message.
    /// A copy that could not be filed is no error: the message left, and
    /// the answer says why its folder holds none.
    fn send(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        sending: &Leaving,
        files: &[Attachment],
        invitation: Option<&str>,
    ) -> Result<SentMail, String>;

    /// Tell `marking`'s server what changed about one message.
    ///
    /// # Errors
    /// The server could not be reached or refused the change.
    fn mark(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        marking: &MarkMail,
    ) -> Result<(), String>;

    /// Watch `account`'s inbox until the answer is dropped, telling `heard`
    /// each message that reaches it after `seen`: after now, when nothing
    /// was seen of it yet. A watch that loses its server reaches for it
    /// again by itself.
    ///
    /// # Errors
    /// The watch could not be started at all.
    fn watch(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        seen: Option<InboxSeen>,
        heard: Arc<dyn InboxHeard>,
    ) -> Result<Watching, String>;
}

/// Where a watch of an inbox has read up to: the numbering its server gave
/// the folder, and the newest `UID` seen under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InboxSeen {
    pub uid_validity: u32,
    pub uid: u32,
}

/// What a watch of an inbox tells whoever started it.
pub trait InboxHeard: Send + Sync {
    /// `messages` reached the inbox, oldest first, and the watch has read up
    /// to `seen`. The first look at an inbox, and the look after its server
    /// numbered it anew, tell no message, only where the watch starts: what
    /// was there before is no arrival.
    fn arrived(&self, messages: Vec<FetchedMessage>, seen: InboxSeen);

    /// The watch lost its server for `reason`, and reaches for it again.
    fn lost(&self, reason: &str);

    /// The server told of a change to the inbox that brought no message: one another client
    /// read, starred, moved or removed. A reader that shows flags reads them again.
    fn changed(&self) {}
}

/// A watch of one account's inbox, for as long as this is held: dropping it
/// stops the watch.
pub struct Watching {
    stop: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl Watching {
    /// A watch that `stop` ends.
    #[must_use]
    pub fn new(stop: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            stop: Some(Box::new(stop)),
        }
    }
}

impl Drop for Watching {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

/// How a watch paces itself.
#[derive(Clone, Copy, Debug)]
pub struct WatchPace {
    /// How long a server is left to tell of a change before the watch says
    /// it still listens: a server drops a connection that only waits for
    /// half an hour.
    pub renew: Duration,
    /// How often a server that cannot tell of a change is asked instead.
    pub poll: Duration,
    /// How long the watch waits before it reaches for a server it lost. The
    /// wait doubles with each loss in a row,
    pub retry: Duration,
    /// up to this.
    pub retry_most: Duration,
}

impl Default for WatchPace {
    fn default() -> Self {
        Self {
            renew: Duration::from_secs(25 * 60),
            poll: Duration::from_secs(60),
            retry: Duration::from_secs(15),
            retry_most: Duration::from_secs(10 * 60),
        }
    }
}

/// A file a message carries: the bytes the host holds of it and the name and
/// type it is written under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// The sessions a host holds with the servers of the accounts it keeps.
#[derive(Default)]
pub struct LiveMailSession {
    /// The secure connection the host makes, when it does not use the
    /// system's own roots (a stack whose mail server signs itself).
    tls: Option<Arc<rustls::ClientConfig>>,
    smtp_trust: Vec<rustls::pki_types::CertificateDer<'static>>,
    /// The runtime every session's connection lives on, started with the
    /// first request: a session kept for the next request outlives the
    /// request that opened it.
    runtime: OnceLock<Result<tokio::runtime::Runtime, String>>,
    /// Each account's signed-in session, from its last request to its next.
    kept: Mutex<HashMap<String, Kept>>,
    /// How the watches this starts pace themselves.
    pace: WatchPace,
    /// Where its connections may go ([`Self::reaching`]); any address where
    /// nothing was said, as on the device that keeps its person's accounts.
    reach: Option<tilo_account_probe::imap::Reach>,
}

/// A signed-in session between two requests, and when its server last
/// answered.
struct Kept {
    session: ImapSession,
    answered: Instant,
}

impl Drop for LiveMailSession {
    fn drop(&mut self) {
        // The sessions go before the runtime their connections live on, and
        // the runtime is let go without waiting for it: this may be dropped
        // where waiting is not allowed.
        self.kept
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        if let Some(Ok(runtime)) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl LiveMailSession {
    /// A session that trusts `extra` as chain roots beside the system's own:
    /// a host whose mail servers are not publicly signed.
    #[must_use]
    pub fn also_trusting(extra: Vec<rustls::pki_types::CertificateDer<'static>>) -> Self {
        let mut roots = tilo_net::system_roots();
        for certificate in &extra {
            let _ = roots.add(certificate.clone());
        }
        let tls = tilo_net::client_config_with_roots(roots).ok().map(Arc::new);
        Self {
            tls,
            smtp_trust: extra,
            runtime: OnceLock::new(),
            kept: Mutex::default(),
            pace: WatchPace::default(),
            reach: None,
        }
    }

    /// The same sessions, every connection of theirs made only within
    /// `reach`: what a server asks for, whose people name its mail servers.
    #[must_use]
    pub fn reaching(mut self, reach: tilo_account_probe::imap::Reach) -> Self {
        self.reach = Some(reach);
        self
    }

    /// The same sessions, their watches paced by `pace`.
    #[must_use]
    pub fn pacing(mut self, pace: WatchPace) -> Self {
        self.pace = pace;
        self
    }

    /// Run `work` on `account`'s signed-in session, and wait here for it: what a mail client
    /// of its own asks of the server ([`server`]), with the session kept for its next request
    /// as every request here keeps it.
    ///
    /// # Errors
    /// The server could not be reached or refused the sign-in, or `work` failed.
    pub fn on_session<T>(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        work: impl AsyncFnOnce(&mut ImapSession) -> Result<T, String>,
    ) -> Result<T, String> {
        self.block(self.with_session(account, secret, work))
    }

    /// Hand the message `mime` holds to `account`'s server, for the recipients `envelope`
    /// names, and wait here until it took it. Nothing is filed: that is
    /// [`server::file_sent`].
    ///
    /// # Errors
    /// No server answered, or it did not take the message.
    pub fn hand_over(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        envelope: lettre::address::Envelope,
        mime: &[u8],
    ) -> Result<(), SendFailure> {
        self.started()
            .map_err(SendFailure::Refused)?
            .block_on(hand_over(
                self.reach.as_ref(),
                account,
                secret,
                envelope,
                mime,
                &self.smtp_trust,
            ))
    }
}

impl MailSession for LiveMailSession {
    fn fetch(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        asking: &FetchMail,
    ) -> Result<FetchedMail, String> {
        self.block(
            self.with_session(account, secret, async |session: &mut ImapSession| {
                fetch_folder(session, asking).await
            }),
        )
    }

    fn read(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        reading: &ReadMail,
    ) -> Result<FetchedMessage, String> {
        self.block(
            self.with_session(account, secret, async |session: &mut ImapSession| {
                read_message(session, reading).await
            }),
        )
    }

    fn search(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        searching: &SearchMail,
    ) -> Result<FoundMail, String> {
        self.block(
            self.with_session(account, secret, async |session: &mut ImapSession| {
                search_mail(session, searching).await
            }),
        )
    }

    fn save_draft(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        draft: &SendMail,
        files: &[Attachment],
        invitation: Option<&str>,
    ) -> Result<(), String> {
        self.block(async {
            let mime = write_mime(draft, files, invitation)?;
            self.with_session(account, secret, async |session: &mut ImapSession| {
                let places = server::places(session).await?;
                server::keep_draft(session, &places, &mime).await.map(drop)
            })
            .await
        })
    }

    fn send(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        sending: &Leaving,
        files: &[Attachment],
        invitation: Option<&str>,
    ) -> Result<SentMail, String> {
        self.block(async {
            let mime = write_leaving(sending, files, invitation)?;
            hand_over(
                self.reach.as_ref(),
                account,
                secret,
                sending.envelope()?,
                &mime,
                &self.smtp_trust,
            )
            .await
            .map_err(SendFailure::into_words)?;
            // The message left. Its copy is told apart from that: a server
            // that will not take one does not unsend the message.
            let unfiled = self
                .with_session(account, secret, async |session: &mut ImapSession| {
                    let places = server::places(session).await?;
                    let host = |name: &str| field(account, name).unwrap_or_default();
                    let hosts = (host("imap_host"), host("smtp_host"));
                    server::file_sent(session, &places, (&hosts.0, &hosts.1), &mime)
                        .await
                        .map(drop)
                })
                .await
                .err();
            Ok(SentMail { unfiled })
        })
    }

    fn mark(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        marking: &MarkMail,
    ) -> Result<(), String> {
        self.block(
            self.with_session(account, secret, async |session: &mut ImapSession| {
                let places = server::places(session).await?;
                server::mark(session, &places, marking).await
            }),
        )
    }

    fn watch(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        seen: Option<InboxSeen>,
        heard: Arc<dyn InboxHeard>,
    ) -> Result<Watching, String> {
        let inbox = Inbox {
            tls: self.tls.clone(),
            reach: self.reach.clone(),
            account: account.clone(),
            secret: secret.clone(),
            pace: self.pace,
        };
        let task = self.started()?.spawn(inbox.watch(seen, heard));
        Ok(Watching::new(move || task.abort()))
    }
}

impl LiveMailSession {
    /// Wait here for `work`, which runs on the sessions' own runtime.
    fn block<T>(
        &self,
        work: impl std::future::Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        self.started()?.block_on(work)
    }

    /// The runtime every session's connection lives on, started when it is
    /// first needed.
    fn started(&self) -> Result<&tokio::runtime::Runtime, String> {
        self.runtime
            .get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .thread_name("mail-session")
                    .enable_all()
                    .build()
                    .map_err(|_| "could not start the mail session".to_string())
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Run `work` on the account's signed-in session: the one its last
    /// request left, while its server still answers, or a new one. A session
    /// is kept for the next request unless it took longer than a request
    /// may, or stopped answering after work that failed.
    async fn with_session<T>(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
        work: impl AsyncFnOnce(&mut ImapSession) -> Result<T, String>,
    ) -> Result<T, String> {
        let key = kept_under(account);
        let mut session = match self.take_kept(&key) {
            Some(kept) if kept.answered.elapsed() < REST_CHECK => kept.session,
            Some(mut kept) => {
                let answers = timeout(NOOP_ANSWER, kept.session.noop()).await;
                if matches!(answers, Ok(Ok(()))) {
                    kept.session
                } else {
                    self.login(account, secret).await?
                }
            }
            None => self.login(account, secret).await?,
        };
        let answer = timeout(REQUEST_TIMEOUT, work(&mut session))
            .await
            .map_err(|_| "the mail server did not answer in time".to_string())?;
        // A request its server refused leaves the connection as good as it
        // was; one that broke on the way no longer answers.
        let answers =
            answer.is_ok() || matches!(timeout(NOOP_ANSWER, session.noop()).await, Ok(Ok(())));
        if answers {
            self.keep(key, session);
        }
        answer
    }

    /// The session kept under `key`, taken out for one request. What nobody
    /// used for long is let go here, where a request passes anyway.
    fn take_kept(&self, key: &str) -> Option<Kept> {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        kept.retain(|_, kept| kept.answered.elapsed() < KEPT_FOR);
        kept.remove(key)
    }

    /// Keep `session`, which just answered, for the account's next request.
    fn keep(&self, key: String, session: ImapSession) {
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        if kept.len() >= MAX_KEPT && !kept.contains_key(&key) {
            // The one that rested longest makes room.
            let longest = kept
                .iter()
                .min_by_key(|(_, kept)| kept.answered)
                .map(|(key, _)| key.clone());
            if let Some(longest) = longest {
                kept.remove(&longest);
            }
        }
        kept.insert(
            key,
            Kept {
                session,
                answered: Instant::now(),
            },
        );
    }

    async fn login(
        &self,
        account: &accounts::Account,
        secret: &accounts::Secret,
    ) -> Result<ImapSession, String> {
        sign_in(self.tls.clone(), self.reach.as_ref(), account, secret).await
    }
}

/// Open the account's connection, secured as its settings say, and sign in.
async fn sign_in(
    tls: Option<Arc<rustls::ClientConfig>>,
    reach: Option<&tilo_account_probe::imap::Reach>,
    account: &accounts::Account,
    secret: &accounts::Secret,
) -> Result<ImapSession, String> {
    let named = field(account, "imap_host")
        .filter(|named| !named.trim().is_empty())
        .ok_or_else(|| "account has no IMAP server".to_string())?;
    let port = port_of(account, "imap_port", 993);
    let user = login_of(account).ok_or_else(|| "account has no IMAP login name".to_string())?;
    if secret.password.is_empty() {
        return Err(no_sign_in(account));
    }
    let user = login_name(&user)?;
    let password = login_name(&secret.password)?;
    let config = match &tls {
        Some(config) => Arc::clone(config),
        None => Arc::new(
            tilo_net::client_config_with_roots(tilo_net::system_roots())
                .map_err(|_| "could not initialize secure protocol versions".to_string())?,
        ),
    };
    // The connection is secured as the sign-in probe secured it when the
    // account was added: TLS from the first byte, or upgraded with
    // STARTTLS before the sign-in is sent.
    let security = accounts::email::imap_security_of(&account.config);
    let reach = reach
        .cloned()
        .unwrap_or_else(tilo_account_probe::imap::Reach::any);
    let client =
        tilo_account_probe::imap::open(&reach, &named, port, security, config, NETWORK_TIMEOUT)
            .await?;
    // A provider that signed the account in on its own page takes a bearer
    // token by `XOAUTH2`; every other account signs in with its password.
    let signed_in = if signed_in_with_oauth(account) {
        let bearer = tilo_account_probe::imap::Bearer {
            user: user.to_owned(),
            access_token: password.to_owned(),
        };
        timeout(NETWORK_TIMEOUT, client.authenticate("XOAUTH2", bearer)).await
    } else {
        timeout(NETWORK_TIMEOUT, client.login(user, password)).await
    };
    signed_in
        .map_err(|_| "IMAP server did not answer before the timeout".to_string())?
        .map_err(|(error, _)| match error {
            async_imap::error::Error::No(_) | async_imap::error::Error::Bad(_) => {
                "IMAP server rejected the saved credential. Check the username and password"
                    .to_string()
            }
            _ => "IMAP server closed the connection unexpectedly".to_string(),
        })
}

/// Whether `account` signs in with a bearer token instead of a password.
fn signed_in_with_oauth(account: &accounts::Account) -> bool {
    accounts::email::is_oauth(&account.config)
}

/// What an account without a sign-in is told, in the words of what it keeps.
fn no_sign_in(account: &accounts::Account) -> String {
    if signed_in_with_oauth(account) {
        "account has no saved sign-in".to_owned()
    } else {
        "account has no saved password".to_owned()
    }
}

/// What an account's session is kept under: the account, and where and as
/// whom it signs in, so a changed account never takes the connection of what
/// it was.
fn kept_under(account: &accounts::Account) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        account.id,
        field(account, "imap_host").unwrap_or_default(),
        port_of(account, "imap_port", 993),
        field(account, "username").unwrap_or_default()
    ) + if signed_in_with_oauth(account) {
        "\noauth"
    } else {
        ""
    }
}

fn field(account: &accounts::Account, name: &str) -> Option<String> {
    account
        .config
        .get(name)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn port_of(account: &accounts::Account, name: &str, default: u16) -> u16 {
    account
        .config
        .get(name)
        .and_then(serde_json::Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .filter(|port| *port != 0)
        .unwrap_or(default)
}

fn login_name(value: &str) -> Result<&str, String> {
    if value.chars().any(char::is_control) {
        return Err("login contains unsupported control characters".into());
    }
    Ok(value)
}

/// Read `asking`'s folder, answering as much of what its messages say as one
/// answer carries.
async fn fetch_folder(
    session: &mut ImapSession,
    asking: &FetchMail,
) -> Result<FetchedMail, String> {
    let mut fetched = read_folder(session, asking).await?;
    let mut room = ANSWER_WORDS;
    fit(&mut fetched.messages, &mut room);
    Ok(fetched)
}

async fn read_folder(session: &mut ImapSession, asking: &FetchMail) -> Result<FetchedMail, String> {
    if asking.folder.is_empty() {
        return Err("a read names its folder".into());
    }
    let places = server::places(session).await?;
    let mailbox = places
        .listed(&asking.folder)
        .ok_or_else(|| format!("this account's server has no {} folder", asking.folder))?;
    let page = match (
        asking.before_uid,
        asking.known_uid,
        asking.known_uid_validity,
    ) {
        (Some(uid), _, Some(uid_validity)) => server::Page::Before { uid_validity, uid },
        (None, Some(newest), Some(uid_validity)) => server::Page::Since {
            uid_validity,
            newest,
            oldest: newest.saturating_sub(PAGE - 1).max(1),
            catch_up: CATCH_UP_LIMIT,
        },
        _ => server::Page::Newest,
    };
    let read = server::read_page(session, mailbox, asking.folder == "starred", page).await?;
    Ok(FetchedMail {
        account: asking.account.clone(),
        folder: asking.folder.clone(),
        uid_validity: read.uid_validity,
        newest_uid: read.newest_uid,
        reset: read.reset,
        has_older: read.has_older.unwrap_or(false),
        messages: parse_all(read.messages),
        flags: read.flags,
    })
}

fn parse_all(raws: Vec<Raw>) -> Vec<FetchedMessage> {
    let now_min = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs().cast_signed() / 60);
    raws.into_iter()
        .filter_map(|raw| {
            parse_message(
                &raw.body,
                raw.uid,
                raw.read,
                raw.starred,
                raw.minutes.unwrap_or(now_min),
            )
        })
        .collect()
}

/// One message as the wire carries it: what a reader shows, never how it is
/// stored.
fn parse_message(
    raw: &[u8],
    uid: u32,
    read: bool,
    starred: bool,
    minutes: i64,
) -> Option<FetchedMessage> {
    use mail_parser::MimeHeaders as _;
    let parsed = mail_parser::MessageParser::default().parse(raw)?;
    let address = |address: &mail_parser::Addr| MailAddress {
        name: address
            .name
            .as_ref()
            .map(std::string::ToString::to_string)
            .filter(|name| !name.trim().is_empty()),
        email: address
            .address
            .as_ref()
            .map(std::string::ToString::to_string)
            .unwrap_or_default(),
    };
    let from = parsed.from().and_then(|from| from.first()).map_or_else(
        || MailAddress {
            name: None,
            email: "unknown@unknown".into(),
        },
        address,
    );
    let to = parsed
        .to()
        .map(|to| to.iter().map(address).collect())
        .unwrap_or_default();
    let cc = parsed
        .cc()
        .map(|cc| cc.iter().map(address).collect())
        .unwrap_or_default();
    let body = parsed
        .body_text(0)
        .map(std::borrow::Cow::into_owned)
        .unwrap_or_default();
    let html_body = parsed
        .html_bodies()
        .find_map(|part| match &part.body {
            mail_parser::PartType::Html(html) => Some(html.to_string()),
            _ => None,
        })
        .filter(|html| !html.trim().is_empty());
    let referenced = html_body
        .as_deref()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let attachments = parsed
        .attachments()
        .map(|part| {
            let inline_id = part
                .content_id()
                .map(|id| {
                    id.trim()
                        .trim_start_matches('<')
                        .trim_end_matches('>')
                        .to_owned()
                })
                .filter(|id| {
                    !id.is_empty()
                        && referenced.contains(&format!("cid:{}", id.to_ascii_lowercase()))
                });
            let filename = part
                .attachment_name()
                .unwrap_or("attachment.bin")
                .to_string();
            let declared = part.content_type().map(|kind| {
                format!("{}/{}", kind.ctype(), kind.subtype().unwrap_or_default())
                    .to_ascii_lowercase()
            });
            MailAttachment {
                mime: declared.unwrap_or_else(|| "application/octet-stream".to_owned()),
                filename,
                size: part.contents().len() as u64,
                inline_id,
            }
        })
        .collect();
    Some(FetchedMessage {
        uid,
        message_id: parsed.message_id().map(std::string::ToString::to_string),
        in_reply_to: parsed
            .references()
            .as_text_list()
            .and_then(|ids| ids.first())
            .map(std::string::ToString::to_string)
            .or_else(|| parsed.in_reply_to().as_text().map(str::to_string)),
        from,
        to,
        cc,
        subject: parsed.subject().unwrap_or_default().to_string(),
        body,
        html_body,
        attachments,
        minutes,
        read,
        starred,
        partial: false,
    })
}

/// Leave out of `messages` what `room` does not hold, the newest first to
/// be answered whole: a message whose words do not fit is answered by their
/// start, and says so.
fn fit(messages: &mut [FetchedMessage], room: &mut usize) {
    let mut newest_first: Vec<usize> = (0..messages.len()).collect();
    newest_first.sort_by_key(|index| std::cmp::Reverse(messages.get(*index).map(|m| m.uid)));
    for index in newest_first {
        let Some(message) = messages.get_mut(index) else {
            continue;
        };
        let words = message
            .body
            .len()
            .saturating_add(message.html_body.as_ref().map_or(0, String::len));
        if words > *room {
            message.html_body = None;
            message.partial = true;
            shorten(&mut message.body, EXCERPT_WORDS);
        }
        *room = room.saturating_sub(message.body.len());
        *room = room.saturating_sub(message.html_body.as_ref().map_or(0, String::len));
    }
}

/// Cut `words` to at most `most` bytes, at a letter's edge.
fn shorten(words: &mut String, most: usize) {
    if words.len() <= most {
        return;
    }
    let mut end = most;
    while !words.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    words.truncate(end);
}

/// Read the one message `reading` names. One too long for an answer is
/// answered without its HTML part, then by the start of its words.
async fn read_message(
    session: &mut ImapSession,
    reading: &ReadMail,
) -> Result<FetchedMessage, String> {
    let places = server::places(session).await?;
    let mailbox = places
        .listed(&reading.folder)
        .ok_or_else(|| format!("this account's server has no {} folder", reading.folder))?;
    let selected = session.select(mailbox).await.map_err(|e| e.to_string())?;
    let uid_validity = selected.uid_validity.unwrap_or(0);
    if uid_validity != reading.uid_validity {
        return Err("the folder changed; its messages are read again".into());
    }
    let mut message = parse_all(server::fetch_uids(session, &[reading.uid]).await?)
        .into_iter()
        .find(|message| message.uid == reading.uid)
        .ok_or_else(|| "that message is no longer in its folder".to_string())?;
    let html = message.html_body.as_ref().map_or(0, String::len);
    if message.body.len().saturating_add(html) > READ_WORDS {
        message.html_body = None;
        message.partial = true;
        shorten(&mut message.body, READ_WORDS);
    }
    Ok(message)
}

/// What a server is asked to find the messages `searching` describes, every
/// part of it to match.
fn search_keys(searching: &SearchMail) -> Result<String, String> {
    let mut keys = Vec::new();
    for (key, wanted) in [
        ("TEXT", &searching.text),
        ("FROM", &searching.from),
        ("TO", &searching.to),
        ("SUBJECT", &searching.subject),
    ] {
        let Some(wanted) = wanted.as_deref().map(str::trim).filter(|w| !w.is_empty()) else {
            continue;
        };
        let wanted = server::quoted(wanted)
            .ok_or_else(|| "a mail server is searched in plain letters".to_string())?;
        keys.push(format!("{key} {wanted}"));
    }
    if keys.is_empty() {
        return Err("a search names what it looks for".into());
    }
    Ok(keys.join(" "))
}

/// Ask the server which messages match `searching`, in the folders it
/// names, newest first.
async fn search_mail(
    session: &mut ImapSession,
    searching: &SearchMail,
) -> Result<FoundMail, String> {
    let keys = search_keys(searching)?;
    let places = server::places(session).await?;
    let everywhere = ["inbox".to_owned(), "archive".to_owned()];
    let folders = if searching.folders.is_empty() {
        &everywhere[..]
    } else {
        &searching.folders[..]
    };
    let most = searching.limit.unwrap_or(MAX_FOUND).clamp(1, MAX_FOUND) as usize;
    let mut found = FoundMail::default();
    let mut room = ANSWER_WORDS;
    for folder in folders {
        // A server that keeps no such folder holds nothing there to find.
        let Some(mailbox) = places.listed(folder) else {
            continue;
        };
        let (uid_validity, raws) = server::search_folder(session, mailbox, &keys, most).await?;
        let mut messages = parse_all(raws);
        fit(&mut messages, &mut room);
        found.folders.push(FoundFolder {
            folder: folder.clone(),
            uid_validity,
            messages,
        });
    }
    Ok(found)
}

/// Why a message was not handed to its server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendFailure {
    /// No server answered where the account says it is: worth another try once the device
    /// is online again.
    Unreached(String),
    /// The server answered and did not take the message, or the account cannot send.
    Refused(String),
}

impl SendFailure {
    /// What a person reads.
    #[must_use]
    pub fn into_words(self) -> String {
        match self {
            Self::Unreached(words) | Self::Refused(words) => words,
        }
    }
}

/// Hand the message `mime` holds to the account's server, to the recipients `envelope` names.
async fn hand_over(
    reach: Option<&tilo_account_probe::imap::Reach>,
    account: &accounts::Account,
    secret: &accounts::Secret,
    envelope: lettre::address::Envelope,
    mime: &[u8],
    trust: &[rustls::pki_types::CertificateDer<'static>],
) -> Result<(), SendFailure> {
    use lettre::AsyncTransport as _;
    use lettre::transport::smtp::authentication::{Credentials, Mechanism};
    use lettre::transport::smtp::client::TlsParameters;
    use lettre::{AsyncSmtpTransport, Tokio1Executor};

    let refused = SendFailure::Refused;
    let named = field(account, "smtp_host")
        .filter(|host| !host.trim().is_empty())
        .ok_or_else(|| refused("account has no SMTP server".to_string()))?;
    let port = port_of(account, "smtp_port", 465);
    let user = login_of(account).ok_or_else(|| refused("account has no SMTP login name".into()))?;
    if secret.password.is_empty() {
        return Err(refused(no_sign_in(account)));
    }
    // The server's name is looked up once, within what this session reaches,
    // and the transport is handed the address that answered: it looks nothing
    // up itself, and the certificate is still checked against the name.
    let reach = reach
        .cloned()
        .unwrap_or_else(tilo_account_probe::imap::Reach::any);
    let answered = tilo_account_probe::imap::reach(&reach, &named, port, NETWORK_TIMEOUT)
        .await
        .map_err(SendFailure::Unreached)?
        .peer_addr()
        .map_err(|_| {
            SendFailure::Unreached("the SMTP server's address could not be read".into())
        })?;
    let mut builder = TlsParameters::builder(named.clone());
    for certificate in trust {
        let certificate = lettre::transport::smtp::client::Certificate::from_der(
            certificate.to_vec(),
        )
        .map_err(|error| refused(format!("a trusted certificate is not readable: {error}")))?;
        builder = builder.add_root_certificate(certificate);
    }
    let params = builder.build().map_err(|error| {
        refused(format!(
            "could not prepare the secure SMTP connection: {error}"
        ))
    })?;
    let tls = match port {
        465 => lettre::transport::smtp::client::Tls::Wrapper(params),
        _ => lettre::transport::smtp::client::Tls::Required(params),
    };
    // A bearer token is offered by `XOAUTH2` alone; a password by the
    // mechanisms that carry one.
    let mechanisms = if signed_in_with_oauth(account) {
        vec![Mechanism::Xoauth2]
    } else {
        vec![Mechanism::Login, Mechanism::Plain]
    };
    let transport =
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(answered.ip().to_string())
            .port(port)
            .tls(tls)
            .credentials(Credentials::new(user, secret.password.clone()))
            .authentication(mechanisms)
            // Each step of the exchange is bounded, not the whole: a message with large files
            // takes as long as its words take to travel.
            .timeout(Some(NETWORK_TIMEOUT))
            .build();
    transport
        .send_raw(&envelope, mime)
        .await
        .map(drop)
        .map_err(|error| refused(not_sent(&error)))
}

/// Why a server did not take a message, in words a person can act on.
fn not_sent(error: &lettre::transport::smtp::Error) -> String {
    let code = error.status().map(u16::from);
    if matches!(code, Some(530 | 534 | 535)) {
        return "SMTP server rejected the saved credential. Check the username and password".into();
    }
    if error.is_tls() {
        let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
        while let Some(at) = cause {
            if let Some(rustls::Error::InvalidCertificate(_)) = at.downcast_ref::<rustls::Error>() {
                return "could not establish the secure SMTP connection: the server's certificate \
                        is not trusted"
                    .into();
            }
            cause = at.source();
        }
        return "could not establish the secure SMTP connection".into();
    }
    if error.is_timeout() {
        return "SMTP server did not answer before the timeout".into();
    }
    format!("the message could not be sent: {error}")
}

/// The name an account signs in as: its user name, or its address where it names none.
fn login_of(account: &accounts::Account) -> Option<String> {
    field(account, "username")
        .filter(|name| !name.trim().is_empty())
        .or_else(|| field(account, "email"))
}

/// The MIME of a message that leaves: its head written from what `leaving`
/// was read as, so the names and addresses it shows its readers are the
/// envelope's own, with its text and HTML bodies, the files it carries and a
/// calendar invitation as its own part.
fn write_leaving(
    leaving: &Leaving,
    files: &[Attachment],
    invitation: Option<&str>,
) -> Result<Vec<u8>, String> {
    let named = |recipients: &[Recipient]| {
        recipients
            .iter()
            .map(|one| (one.name().to_owned(), one.address().to_owned()))
            .collect()
    };
    write_parts(
        leaving.message(),
        named(leaving.to()),
        named(leaving.cc()),
        files,
        invitation,
    )
}

/// The MIME of a draft: a message as far as it is written. A recipient that
/// is no address yet stays in its head as it stands, since nothing leaves
/// with a draft. What no line of a head can hold is left out of it, so
/// nothing a draft says begins a header of its own.
fn write_mime(
    draft: &SendMail,
    files: &[Attachment],
    invitation: Option<&str>,
) -> Result<Vec<u8>, String> {
    let spaced = |text: &str| -> String {
        let cleaned = text.chars().map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        });
        cleaned.collect()
    };
    let joined = |id: &String| -> String {
        id.chars()
            .filter(|character| !character.is_control())
            .collect()
    };
    let draft = SendMail {
        from_name: draft.from_name.as_deref().map(spaced),
        from_address: joined(&draft.from_address),
        to: spaced(&draft.to),
        cc: spaced(&draft.cc),
        subject: spaced(&draft.subject),
        message_id: draft.message_id.as_ref().map(joined),
        in_reply_to: draft.in_reply_to.as_ref().map(joined),
        references: draft.references.iter().map(joined).collect(),
        ..draft.clone()
    };
    let (to, cc) = (draft_addresses(&draft.to), draft_addresses(&draft.cc));
    write_parts(&draft, to, cc, files, invitation)
}

/// The MIME of `sending`, addressed in its head to `to` and `cc`, each a
/// name and an address.
fn write_parts(
    sending: &SendMail,
    to: Vec<(String, String)>,
    cc: Vec<(String, String)>,
    files: &[Attachment],
    invitation: Option<&str>,
) -> Result<Vec<u8>, String> {
    let from = (
        sending.from_name.clone().unwrap_or_default(),
        sending.from_address.clone(),
    );
    let mut builder = mail_builder::MessageBuilder::new()
        .from(from)
        .subject(sending.subject.clone())
        .text_body(sending.body.clone());
    if let Some(html) = &sending.html_body {
        builder = builder.html_body(html.clone());
    }
    if let Some(message_id) = &sending.message_id {
        builder = builder.message_id(message_id.clone());
    }
    if let Some(parent) = &sending.in_reply_to {
        builder = builder.in_reply_to(parent.clone());
    }
    if !sending.references.is_empty() {
        builder = builder.references(sending.references.clone());
    }
    if !to.is_empty() {
        builder = builder.to(to);
    }
    if !cc.is_empty() {
        builder = builder.cc(cc);
    }
    for file in files {
        builder = builder.attachment(file.mime.clone(), file.filename.clone(), file.bytes.clone());
    }
    if let Some(invitation) = invitation {
        builder = builder.attachment(
            "text/calendar; method=REQUEST",
            "invitation.ics",
            invitation.as_bytes().to_vec(),
        );
    }
    // Blind recipients belong only in the envelope; a header here would
    // show them to every other recipient.
    builder
        .write_to_vec()
        .map_err(|error| format!("could not write the message: {error}"))
}

/// What a draft's list names so far, `Name <address>, other@example`, as
/// mail's own tuple form. Nothing is sent to these: a message that leaves is
/// addressed by [`Leaving::read`].
fn draft_addresses(list: &str) -> Vec<(String, String)> {
    list.split([';', ','])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            if let Some((name, rest)) = entry.split_once('<')
                && let Some((address, _)) = rest.rsplit_once('>')
            {
                (
                    name.trim().trim_matches('"').to_owned(),
                    address.trim().to_owned(),
                )
            } else {
                (String::new(), entry.to_owned())
            }
        })
        .collect()
}

/// One account's inbox, as a watch reaches it.
struct Inbox {
    tls: Option<Arc<rustls::ClientConfig>>,
    reach: Option<tilo_account_probe::imap::Reach>,
    account: accounts::Account,
    secret: accounts::Secret,
    pace: WatchPace,
}

impl Inbox {
    /// Watch the inbox for good: over one connection for as long as its
    /// server keeps it, then over the next, waiting longer before each try
    /// while the server stays lost.
    async fn watch(self, mut seen: Option<InboxSeen>, heard: Arc<dyn InboxHeard>) {
        let mut wait = self.pace.retry;
        loop {
            let lost = match self.watch_connected(&mut seen, &*heard, &mut wait).await {
                Ok(never) => match never {},
                Err(reason) => reason,
            };
            heard.lost(&lost);
            tokio::time::sleep(wait).await;
            wait = wait.saturating_mul(2).min(self.pace.retry_most);
        }
    }

    /// Watch over one connection until its server is lost, which is the
    /// only way this ends.
    async fn watch_connected(
        &self,
        seen: &mut Option<InboxSeen>,
        heard: &dyn InboxHeard,
        wait: &mut Duration,
    ) -> Result<std::convert::Infallible, String> {
        let unanswered = || "IMAP server did not answer before the timeout".to_string();
        let mut session = sign_in(
            self.tls.clone(),
            self.reach.as_ref(),
            &self.account,
            &self.secret,
        )
        .await?;
        let tells = timeout(NETWORK_TIMEOUT, session.capabilities())
            .await
            .map_err(|_| unanswered())?
            .map_err(|e| e.to_string())?
            .has_str("IDLE");
        // A watch that comes back after a loss may have missed changes that
        // brought no message, as one the server tells of while it idles.
        let mut news = seen.is_some();
        loop {
            // Each round looks at the inbox afresh: what arrived while
            // nothing watched it, and what the wait just over heard of.
            let inbox = timeout(NETWORK_TIMEOUT, session.select("INBOX"))
                .await
                .map_err(|_| unanswered())?
                .map_err(|e| e.to_string())?;
            let told = timeout(
                REQUEST_TIMEOUT,
                take_arrivals(&mut session, &inbox, seen, heard),
            )
            .await
            .map_err(|_| unanswered())??;
            if news && !told {
                heard.changed();
            }
            // The server answers: the next loss starts its waits over.
            *wait = self.pace.retry;
            if tells {
                let mut idle = session.idle();
                timeout(NETWORK_TIMEOUT, idle.init())
                    .await
                    .map_err(|_| unanswered())?
                    .map_err(|e| e.to_string())?;
                // The wait ends when the server tells of a change, or when
                // it is time to say the watch still listens: either way the
                // inbox is looked at again.
                let (told, _stop) = idle.wait_with_timeout(self.pace.renew);
                news = matches!(
                    told.await.map_err(|e| e.to_string())?,
                    async_imap::extensions::idle::IdleResponse::NewData(_)
                );
                session = timeout(NETWORK_TIMEOUT, idle.done())
                    .await
                    .map_err(|_| unanswered())?
                    .map_err(|e| e.to_string())?;
            } else {
                tokio::time::sleep(self.pace.poll).await;
                // A server that cannot tell is asked: what changed is read again.
                news = true;
            }
        }
    }
}

/// Tell `heard` what reached the selected `inbox` after `seen`, and move
/// `seen` past it. Whether there was anything to tell.
async fn take_arrivals(
    session: &mut ImapSession,
    inbox: &async_imap::types::Mailbox,
    seen: &mut Option<InboxSeen>,
    heard: &dyn InboxHeard,
) -> Result<bool, String> {
    let uid_validity = inbox.uid_validity.unwrap_or(0);
    // The newest number the folder has given out. A server that does not
    // say which comes next is asked for the numbers it holds.
    let newest = match inbox.uid_next {
        Some(next) => next.saturating_sub(1),
        None => session
            .uid_search("ALL")
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .max()
            .unwrap_or(0),
    };
    let after = match *seen {
        Some(mark) if mark.uid_validity == uid_validity => mark.uid,
        _ => {
            // The first look, or a folder its server numbered anew: what it
            // holds now was there before, and is no arrival.
            let mark = InboxSeen {
                uid_validity,
                uid: newest,
            };
            *seen = Some(mark);
            heard.arrived(Vec::new(), mark);
            return Ok(true);
        }
    };
    if newest <= after {
        return Ok(false);
    }
    let first = after
        .saturating_add(1)
        .max(newest.saturating_sub(WATCH_MOST.saturating_sub(1)));
    let mut messages = parse_all(server::fetch_set(session, &format!("{first}:*")).await?);
    messages.retain(|message| message.uid > after);
    messages.sort_by_key(|message| message.uid);
    let mark = InboxSeen {
        uid_validity,
        uid: messages
            .last()
            .map_or(newest, |message| message.uid.max(newest)),
    };
    *seen = Some(mark);
    heard.arrived(messages, mark);
    Ok(true)
}
