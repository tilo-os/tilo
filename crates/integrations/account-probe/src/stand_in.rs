//! A stand-in mail server with real sockets: an IMAP and an SMTP server that take one made-up
//! login and refuse every other, for a rehearsal of a mail account and for the tests of
//! everything that talks to mail servers, where no real mail server, no real address and
//! nobody's password may be used.
//!
//! Its folders are laid out as the servers people use lay theirs out ([`Layout`]): Gmail's,
//! Outlook's, or a plain server's that holds only an inbox until a mail program makes the rest.
//! It speaks what this crate's sign-in proof and a host's mail session speak: a port is TLS from
//! the first byte, or greets in the clear and upgrades with `STARTTLS`, and nothing signs in
//! before the connection is secure. It reads and finds mail by position and by UID, keeps
//! flags, moves, expunges, keeps what a client appends, and tells a connection that idles of a
//! change as a real server does. What its SMTP server is handed is kept, and a message sent to
//! its own login arrives in its inbox. It sends nothing anywhere.
//!
//! A test changes the mailbox as another client would ([`Started::append`],
//! [`Started::flag`]), reads what the servers were handed, and makes the server misbehave as
//! real ones do: refuse appends, offer no `IDLE`, drop every connection.
//!
//! Every IMAP sign-in is said on standard error, with the login it named, whether it was taken,
//! and the process that made the connection where the stand-in may read it (it runs beside its
//! clients, as a story's machine): who signed in is read off the server, never off a client.
//!
//! The example `mail_stand_in` is this module as a program (the local stack starts it as
//! `mail-stand-in`).

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use rustls::pki_types::CertificateDer;
use tokio::io::BufReader;
use tokio::io::{
    AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _,
};
use tokio::net::TcpStream;

/// The made-up mail a seeded inbox holds: its subject, its words, and the hour it arrived,
/// oldest first.
const INBOX: [(&str, &str, &str); 3] = [
    (
        "Quarterly plan",
        "The draft is in the shared folder.",
        "14-Nov-2026 09:05:00 +0000",
    ),
    (
        "Lunch on Friday?",
        "Does one o'clock work for you?",
        "17-Nov-2026 12:10:00 +0000",
    ),
    (
        "Welcome to the lab",
        "Your badge is at the reception from Monday.",
        "18-Nov-2026 08:30:00 +0000",
    ),
];

/// Who the seeded inbox's made-up mail is from.
const SENDER: &str = "Grace Hopper <grace@example.test>";

/// How a server lays out its folders.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    /// A plain server, as Dovecot serves a new account with its stock configuration: only the
    /// inbox exists, a folder carries its special-use mark once a client creates it under the
    /// name the server knows, the hierarchy separator is a dot, and nothing files a sent
    /// message.
    #[default]
    Plain,
    Gmail,
    Outlook,
}

impl Layout {
    fn folders(self) -> Vec<(&'static str, &'static [&'static str])> {
        match self {
            Self::Gmail => vec![
                ("INBOX", &[]),
                ("[Gmail]/All Mail", &["\\All"]),
                ("[Gmail]/Sent Mail", &["\\Sent"]),
                ("[Gmail]/Drafts", &["\\Drafts"]),
                ("[Gmail]/Spam", &["\\Junk"]),
                ("[Gmail]/Trash", &["\\Trash"]),
            ],
            Self::Outlook => vec![
                ("INBOX", &[]),
                ("Archive", &[]),
                ("Sent Items", &[]),
                ("Drafts", &[]),
                ("Junk Email", &[]),
                ("Deleted Items", &[]),
            ],
            Self::Plain => vec![("INBOX", &[])],
        }
    }

    /// The special-use marks a folder created as `name` carries.
    fn uses_of(self, name: &str) -> &'static [&'static str] {
        match (self, name) {
            (Self::Plain, "Drafts") => &["\\Drafts"],
            (Self::Plain, "Sent" | "Sent Messages") => &["\\Sent"],
            (Self::Plain, "Junk") => &["\\Junk"],
            (Self::Plain, "Trash") => &["\\Trash"],
            _ => &[],
        }
    }

    const fn separator(self) -> char {
        match self {
            Self::Gmail | Self::Outlook => '/',
            Self::Plain => '.',
        }
    }

    /// What the server says it can do once a connection is secure. Gmail names itself, which
    /// is how a client knows that it files a sent message on its own.
    const fn capabilities(self) -> &'static str {
        match self {
            Self::Gmail => {
                "IMAP4rev1 AUTH=PLAIN AUTH=XOAUTH2 IDLE MOVE UIDPLUS SPECIAL-USE X-GM-EXT-1"
            }
            Self::Outlook => "IMAP4rev1 AUTH=PLAIN AUTH=XOAUTH2 IDLE MOVE UIDPLUS SPECIAL-USE",
            Self::Plain => {
                "IMAP4rev1 AUTH=PLAIN AUTH=XOAUTH2 ID ENABLE IDLE MOVE UIDPLUS SPECIAL-USE"
            }
        }
    }
}

/// What a stand-in serves: the names its certificate is for, the one login it takes, where its
/// two servers listen and which of them is TLS from the first byte, how its folders are laid
/// out, and whether its inbox starts with a few made-up messages.
#[derive(Clone, Debug)]
pub struct Serving {
    pub names: Vec<String>,
    pub login: String,
    pub password: String,
    /// A bearer token the stand-in takes in place of the password, by `XOAUTH2`: the sign-in a
    /// provider's own page leaves an account with.
    pub oauth_token: Option<String>,
    pub imap: String,
    pub smtp: String,
    pub imap_wrapped: bool,
    pub smtp_wrapped: bool,
    pub layout: Layout,
    pub seeded: bool,
}

impl Serving {
    /// A stand-in on loopback ports of its own for `login` and `password`, its certificate for
    /// `localhost` and `127.0.0.1`: IMAP is TLS from the first byte, SMTP upgrades, its folders
    /// are a plain server's and its inbox is empty.
    #[must_use]
    pub fn local(login: &str, password: &str) -> Self {
        Self {
            names: vec!["localhost".to_owned(), "127.0.0.1".to_owned()],
            login: login.to_owned(),
            password: password.to_owned(),
            oauth_token: None,
            imap: "127.0.0.1:0".to_owned(),
            smtp: "127.0.0.1:0".to_owned(),
            imap_wrapped: true,
            smtp_wrapped: false,
            layout: Layout::Plain,
            seeded: false,
        }
    }
}

/// One message as the stand-in holds it.
#[derive(Clone, Debug)]
pub struct Stored {
    pub uid: u32,
    pub flags: Vec<String>,
    /// When it arrived, as `INTERNALDATE` writes a date.
    pub received: String,
    pub raw: Vec<u8>,
}

/// One folder of the stand-in's mailbox.
struct Folder {
    name: String,
    uses: Vec<&'static str>,
    validity: u32,
    next_uid: u32,
    messages: Vec<Stored>,
}

/// The stand-in's whole mailbox.
struct Mail {
    folders: Vec<Folder>,
}

impl Mail {
    fn laid_out(layout: Layout) -> Self {
        let folders = layout
            .folders()
            .into_iter()
            .enumerate()
            .map(|(index, (name, uses))| Folder {
                name: name.to_owned(),
                uses: uses.to_vec(),
                validity: u32::try_from(index).unwrap_or(0).saturating_add(100),
                next_uid: 1,
                messages: Vec::new(),
            })
            .collect();
        Self { folders }
    }

    fn folder(&self, name: &str) -> Option<&Folder> {
        self.folders
            .iter()
            .find(|folder| folder.name.eq_ignore_ascii_case(name))
    }

    fn folder_mut(&mut self, name: &str) -> Option<&mut Folder> {
        self.folders
            .iter_mut()
            .find(|folder| folder.name.eq_ignore_ascii_case(name))
    }

    /// Keep `raw` in `folder`, as `APPEND` and the SMTP server do: its validity and UID.
    fn keep(
        &mut self,
        folder: &str,
        flags: Vec<String>,
        raw: Vec<u8>,
        received: String,
    ) -> Option<(u32, u32)> {
        let folder = self.folder_mut(folder)?;
        let uid = folder.next_uid;
        folder.next_uid = folder.next_uid.saturating_add(1);
        folder.messages.push(Stored {
            uid,
            flags,
            received,
            raw,
        });
        Some((folder.validity, uid))
    }
}

/// One message of the seeded inbox: made-up headers, and what it says.
fn message(number: usize, subject: &str, body: &str, received: &str, to: &str) -> String {
    format!(
        "Message-ID: <stand-in-{number}@example.test>\r\nFrom: {SENDER}\r\nTo: {to}\r\n\
Subject: {subject}\r\nDate: {received}\r\nMIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\r\n{body}\r\n"
    )
}

/// The one login, the identity both servers present, the mail they hold and what a test told
/// them or asks of them.
struct Server {
    name: String,
    login: String,
    password: String,
    oauth_token: Option<String>,
    layout: Layout,
    tls: Arc<rustls::ServerConfig>,
    mail: Mutex<Mail>,
    /// Counts each change to the mailbox, which a connection that idles is told of.
    changed: tokio::sync::watch::Sender<usize>,
    /// Counts each time every connection was dropped.
    drops: tokio::sync::watch::Sender<usize>,
    idling: AtomicUsize,
    logins: AtomicUsize,
    cleartext_logins: AtomicUsize,
    refuses_appends: AtomicBool,
    /// Whether the server cannot tell an idling connection of a change.
    tells_nothing: AtomicBool,
    sent: Mutex<Vec<Vec<u8>>>,
}

impl Server {
    fn takes(&self, login: &str, password: &str) -> bool {
        login == self.login && password == self.password
    }

    /// Whether `user` with `token` is the one login signed in with `XOAUTH2`.
    fn takes_token(&self, user: &str, token: &str) -> bool {
        user.trim() == self.login && self.oauth_token.as_deref() == Some(token.trim())
    }

    fn mail(&self) -> std::sync::MutexGuard<'_, Mail> {
        self.mail.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn announce(&self) {
        self.changed
            .send_modify(|changes| *changes = changes.wrapping_add(1));
    }

    /// What the IMAP server says it can do, on a connection that is secure.
    fn capabilities(&self) -> String {
        let all = self.layout.capabilities();
        if self.tells_nothing.load(Ordering::SeqCst) {
            all.replace(" IDLE", "")
        } else {
            all.to_owned()
        }
    }
}

/// The runtime both servers answer on, let go without waiting: a test may end inside another.
struct Running(Option<tokio::runtime::Runtime>);

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

/// A stand-in that listens: where, the certificate what both servers present is signed by
/// ([`authority`]), and the TLS they speak it with (the autoconfig document, where one is served, presents the same). Both servers
/// answer while a copy of this is held.
#[derive(Clone)]
pub struct Started {
    pub imap: SocketAddr,
    pub smtp: SocketAddr,
    pub certificate: CertificateDer<'static>,
    pub tls: Arc<rustls::ServerConfig>,
    server: Arc<Server>,
    _running: Arc<Running>,
}

impl Started {
    /// A client configuration that trusts the stand-ins' authority ([`authority`]) and nothing
    /// else.
    ///
    /// # Errors
    /// The certificate is no root a client can trust.
    pub fn trusting_client(&self) -> Result<Arc<rustls::ClientConfig>, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(self.certificate.clone())
            .map_err(|error| error.to_string())?;
        tilo_net::client_config_with_roots(roots)
            .map(Arc::new)
            .map_err(|error| error.to_string())
    }

    /// Put `raw` (lines ended either way) in `folder` with `flags`, as mail that arrived from
    /// elsewhere, and tell a connection that idles. Its UID, or none for a folder the server
    /// does not hold.
    pub fn append(&self, folder: &str, raw: &str, flags: &[&str]) -> Option<u32> {
        let raw = raw.replace("\r\n", "\n").replace('\n', "\r\n").into_bytes();
        let flags = flags.iter().map(|flag| (*flag).to_owned()).collect();
        let kept = {
            let mut mail = self.server.mail();
            let next = mail.folder(folder)?.next_uid;
            mail.keep(folder, flags, raw, arrived_at(next))
        };
        self.server.announce();
        kept.map(|(_, uid)| uid)
    }

    /// Give the message `uid` of `folder` `flag`, as another client would, and tell a
    /// connection that idles.
    pub fn flag(&self, folder: &str, uid: u32, flag: &str) {
        {
            let mut mail = self.server.mail();
            let message = mail
                .folder_mut(folder)
                .and_then(|folder| folder.messages.iter_mut().find(|m| m.uid == uid));
            if let Some(message) = message
                && !message.flags.iter().any(|kept| kept == flag)
            {
                message.flags.push(flag.to_owned());
            }
            drop(mail);
        }
        self.server.announce();
    }

    /// Take a message away, as another client deleting it does.
    pub fn remove(&self, folder: &str, uid: u32) {
        {
            let mut mail = self.server.mail();
            if let Some(folder) = mail.folder_mut(folder) {
                folder.messages.retain(|message| message.uid != uid);
            }
            drop(mail);
        }
        self.server.announce();
    }

    /// Number the folder again under a new `UIDVALIDITY`, every message under a UID of its own
    /// past the ones it had: what a server rebuilding a mailbox does, and a client holding the
    /// old numbering has to read again.
    pub fn renumber(&self, folder: &str) {
        {
            let mut mail = self.server.mail();
            if let Some(folder) = mail.folder_mut(folder) {
                folder.validity = folder.validity.saturating_add(50);
                let mut uid = folder.next_uid.max(1000);
                for message in &mut folder.messages {
                    message.uid = uid;
                    uid = uid.saturating_add(1);
                }
                folder.next_uid = uid;
            }
            drop(mail);
        }
        self.server.announce();
    }

    /// How many connections idle now.
    pub fn idling(&self) -> usize {
        self.server.idling.load(Ordering::SeqCst)
    }

    /// How many times anyone signed in to IMAP, over every connection.
    pub fn logins(&self) -> usize {
        self.server.logins.load(Ordering::SeqCst)
    }

    /// How many sign-ins were tried before their connection was secure. A client that keeps a
    /// secret safe never tries one.
    pub fn cleartext_logins(&self) -> usize {
        self.server.cleartext_logins.load(Ordering::SeqCst)
    }

    /// From now on the server takes no message a client appends, as one whose account is over
    /// its quota.
    pub fn refuse_appends(&self) {
        self.server.refuses_appends.store(true, Ordering::SeqCst);
    }

    /// From now on the server cannot tell an idling connection of a change: it leaves `IDLE`
    /// out of what it says it can do, and refuses it.
    pub fn without_idle(&self) {
        self.server.tells_nothing.store(true, Ordering::SeqCst);
    }

    /// Close every connection the server holds, as one that restarts or drops what idled for
    /// too long. New connections are taken as before.
    pub fn drop_connections(&self) {
        self.server
            .drops
            .send_modify(|drops| *drops = drops.wrapping_add(1));
    }

    /// The names of the folders the server holds, in the order they came to be.
    pub fn folders(&self) -> Vec<String> {
        self.server
            .mail()
            .folders
            .iter()
            .map(|folder| folder.name.clone())
            .collect()
    }

    /// The messages `folder` holds now.
    pub fn messages(&self, folder: &str) -> Vec<Stored> {
        self.server
            .mail()
            .folder(folder)
            .map(|folder| folder.messages.clone())
            .unwrap_or_default()
    }

    /// Every message the SMTP server was handed, as it was handed, oldest first.
    pub fn sent(&self) -> Vec<Vec<u8>> {
        self.server
            .sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// When a message a test puts in a folder arrived: a minute of its own for each UID.
fn arrived_at(uid: u32) -> String {
    format!("01-Jan-2026 09:{:02}:00 +0000", uid % 60)
}

/// The authority every stand-in's certificate is signed by, one a process: what a client
/// trusts to reach any stand-in, also one started after the client.
struct Authority {
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
    certificate: CertificateDer<'static>,
}

fn authority_of() -> Result<&'static Authority, String> {
    static AUTHORITY: std::sync::OnceLock<Result<Authority, String>> = std::sync::OnceLock::new();
    AUTHORITY
        .get_or_init(|| {
            let failed = |error: rcgen::Error| format!("no stand-in authority: {error}");
            let key = rcgen::KeyPair::generate().map_err(failed)?;
            let mut params = rcgen::CertificateParams::default();
            params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
            params.key_usages = vec![
                rcgen::KeyUsagePurpose::KeyCertSign,
                rcgen::KeyUsagePurpose::DigitalSignature,
            ];
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, "Mail stand-in authority");
            let certificate = params.self_signed(&key).map_err(failed)?.der().clone();
            Ok(Authority {
                issuer: rcgen::Issuer::new(params, key),
                certificate,
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The certificate every stand-in's is signed by: trusting it trusts each stand-in this
/// process starts, before or after.
///
/// # Errors
/// No authority could be made.
pub fn authority() -> Result<CertificateDer<'static>, String> {
    authority_of().map(|authority| authority.certificate.clone())
}

/// Start both servers. They answer while the [`Started`] is held.
///
/// # Errors
/// No certificate could be made for the names, or an address could not be listened on.
pub fn started(serving: Serving) -> Result<Started, String> {
    let authority = authority_of()?;
    let no_certificate =
        |error: rcgen::Error| format!("no certificate for {:?}: {error}", serving.names);
    let signing_key = rcgen::KeyPair::generate().map_err(no_certificate)?;
    let leaf = rcgen::CertificateParams::new(serving.names.clone())
        .and_then(|params| params.signed_by(&signing_key, &authority.issuer))
        .map_err(no_certificate)?;
    let certificate = authority.certificate.clone();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der());
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| error.to_string())?
    .with_no_client_auth()
    .with_single_cert(
        vec![leaf.der().clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(key),
    )
    .map_err(|error| error.to_string())?;
    let tls = Arc::new(tls);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("mail-stand-in")
        .enable_all()
        .build()
        .map_err(|error| format!("no runtime for the stand-in: {error}"))?;
    let listening = |address: &str| {
        let listener = std::net::TcpListener::bind(address)
            .map_err(|error| format!("cannot listen on {address}: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        let at = listener.local_addr().map_err(|error| error.to_string())?;
        let _entered = runtime.enter();
        let listener =
            tokio::net::TcpListener::from_std(listener).map_err(|error| error.to_string())?;
        Ok::<_, String>((listener, at))
    };
    let (imap, imap_at) = listening(&serving.imap)?;
    let (smtp, smtp_at) = listening(&serving.smtp)?;
    let mut mail = Mail::laid_out(serving.layout);
    if serving.seeded {
        for (number, (subject, body, received)) in INBOX.iter().enumerate() {
            let raw = message(number, subject, body, received, &serving.login).into_bytes();
            mail.keep("INBOX", Vec::new(), raw, (*received).to_owned());
        }
    }
    let server = Arc::new(Server {
        name: serving
            .names
            .first()
            .cloned()
            .unwrap_or_else(|| "localhost".to_owned()),
        login: serving.login,
        password: serving.password,
        oauth_token: serving.oauth_token,
        layout: serving.layout,
        tls: Arc::clone(&tls),
        mail: Mutex::new(mail),
        changed: tokio::sync::watch::Sender::new(0),
        drops: tokio::sync::watch::Sender::new(0),
        idling: AtomicUsize::new(0),
        logins: AtomicUsize::new(0),
        cleartext_logins: AtomicUsize::new(0),
        refuses_appends: AtomicBool::new(false),
        tells_nothing: AtomicBool::new(false),
        sent: Mutex::default(),
    });
    let (imap_wrapped, smtp_wrapped) = (serving.imap_wrapped, serving.smtp_wrapped);
    let serving_imap = Arc::clone(&server);
    runtime.spawn(async move {
        // A connection that could not be taken is passed over; the next one is waited for.
        while let Ok((tcp, _)) = imap.accept().await {
            tokio::spawn(imap_connection(
                tcp,
                Arc::clone(&serving_imap),
                imap_wrapped,
            ));
        }
    });
    let serving_smtp = Arc::clone(&server);
    runtime.spawn(async move {
        while let Ok((tcp, _)) = smtp.accept().await {
            tokio::spawn(smtp_connection(
                tcp,
                Arc::clone(&serving_smtp),
                smtp_wrapped,
            ));
        }
    });
    Ok(Started {
        imap: imap_at,
        smtp: smtp_at,
        certificate,
        tls,
        server,
        _running: Arc::new(Running(Some(runtime))),
    })
}

/// One side of a conversation in lines.
struct Lines<S>(BufReader<S>);

impl<S: AsyncRead + AsyncWrite + Unpin> Lines<S> {
    /// The next line without its end, or nothing once the other side left.
    async fn line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.0.read_line(&mut line).await {
            Ok(read) if read > 0 => Some(line.trim_end_matches(['\r', '\n']).to_owned()),
            _ => None,
        }
    }

    /// Send `text` and a line's end. Whether it was sent.
    async fn say(&mut self, text: &str) -> bool {
        self.send(&format!("{text}\r\n")).await
    }

    /// Send `text` as it stands. Whether it was sent.
    async fn send(&mut self, text: &str) -> bool {
        let to = self.0.get_mut();
        to.write_all(text.as_bytes()).await.is_ok() && to.flush().await.is_ok()
    }

    /// Read `size` bytes as they stand: a command's literal. Whether they came.
    async fn literal(&mut self, size: usize) -> Option<Vec<u8>> {
        let mut raw = vec![0_u8; size];
        self.0.read_exact(&mut raw).await.ok()?;
        // The end of the command's line follows the literal.
        self.line().await?;
        Some(raw)
    }

    /// What the client answers to `prompt`, decoded from base64.
    async fn asked(&mut self, prompt: &str) -> Option<String> {
        self.say(prompt).await.then_some(())?;
        let said = base64::engine::general_purpose::STANDARD
            .decode(self.line().await?.trim())
            .ok()?;
        String::from_utf8(said).ok()
    }
}

type Secure = tokio_rustls::server::TlsStream<TcpStream>;

/// `tcp` as a connection secured with this stand-in's certificate.
async fn secured(tcp: TcpStream, server: &Server) -> Option<Lines<Secure>> {
    let stream = tokio_rustls::TlsAcceptor::from(Arc::clone(&server.tls))
        .accept(tcp)
        .await
        .ok()?;
    Some(Lines(BufReader::new(stream)))
}

async fn imap_connection(tcp: TcpStream, server: Arc<Server>, wrapped: bool) {
    let peer = tcp.peer_addr().ok();
    let mut dropped = server.drops.subscribe();
    let tcp = if wrapped {
        tcp
    } else if let Some(upgraded) = imap_in_the_clear(tcp, &server).await {
        upgraded
    } else {
        return;
    };
    let Some(mut lines) = secured(tcp, &server).await else {
        return;
    };
    // A connection that upgraded was greeted in the clear and is not greeted again.
    let greeting = format!(
        "* OK [CAPABILITY {}] mail stand-in ready",
        server.capabilities()
    );
    if wrapped && !lines.say(&greeting).await {
        return;
    }
    tokio::select! {
        // A dropped connection is closed under its client.
        _ = dropped.changed() => {}
        () = imap_secure(&mut lines, &server, peer) => {}
    }
}

/// The cleartext start of an IMAP connection: the greeting and, when asked, the upgrade. The
/// connection once the client may begin its handshake; nothing signs in before it.
async fn imap_in_the_clear(tcp: TcpStream, server: &Server) -> Option<TcpStream> {
    let mut lines = Lines(BufReader::new(tcp));
    lines
        .say("* OK [CAPABILITY IMAP4rev1 STARTTLS LOGINDISABLED] mail stand-in ready")
        .await
        .then_some(())?;
    loop {
        let line = lines.line().await?;
        let (tag, command, _) = parts(&line);
        let said = match command.as_str() {
            "CAPABILITY" => {
                lines
                    .say("* CAPABILITY IMAP4rev1 STARTTLS LOGINDISABLED")
                    .await
                    && lines.say(&format!("{tag} OK CAPABILITY completed")).await
            }
            "STARTTLS" => {
                // The client waits for this answer before its handshake, so nothing of the
                // handshake is left in this reader.
                let begun = lines
                    .say(&format!("{tag} OK begin TLS negotiation now"))
                    .await;
                return begun.then(|| lines.0.into_inner());
            }
            "LOGIN" | "AUTHENTICATE" => {
                server.cleartext_logins.fetch_add(1, Ordering::SeqCst);
                lines
                    .say(&format!(
                        "{tag} NO [PRIVACYREQUIRED] sign-in needs a secure connection"
                    ))
                    .await
            }
            "LOGOUT" => {
                lines.say("* BYE mail stand-in leaving").await;
                lines.say(&format!("{tag} OK LOGOUT completed")).await;
                return None;
            }
            "NOOP" => lines.say(&format!("{tag} OK NOOP completed")).await,
            _ => {
                lines
                    .say(&format!(
                        "{tag} NO [PRIVACYREQUIRED] secure the connection first"
                    ))
                    .await
            }
        };
        if !said {
            return None;
        }
    }
}

/// What one connection has done so far: whether it signed in, and the folder it selected.
struct Signed {
    taken: bool,
    selected: Option<String>,
}

/// The secured part of an IMAP connection: the sign-in, then its mailbox.
#[expect(
    clippy::too_many_lines,
    reason = "one connection's dispatch of the commands that wait on the client or the mailbox"
)]
async fn imap_secure<S: AsyncRead + AsyncWrite + Unpin>(
    lines: &mut Lines<S>,
    server: &Server,
    peer: Option<SocketAddr>,
) {
    let mut signed = Signed {
        taken: false,
        selected: None,
    };
    // A change made while this connection was between commands is one it has not been told of,
    // so a wait that follows answers it at once rather than sleeping through it.
    let mut news = server.changed.subscribe();
    while let Some(line) = lines.line().await {
        let (tag, command, rest) = parts(&line);
        // A command by message numbers of the other kind is answered as the plain one, which
        // is told which numbers its set names.
        let (uid, command, rest) = if command == "UID" {
            let (_, inner, rest) = parts(&format!("- {rest}"));
            (true, inner, rest)
        } else {
            (false, command, rest)
        };
        let said = match command.as_str() {
            "LOGIN" => {
                let named = match arguments(&rest).as_slice() {
                    [login, password] => Some((login.clone(), password.clone())),
                    _ => None,
                };
                signed.taken = named
                    .as_ref()
                    .is_some_and(|(login, password)| server.takes(login, password));
                if signed.taken {
                    server.logins.fetch_add(1, Ordering::SeqCst);
                }
                said_sign_in(
                    named.as_ref().map(|(login, _)| login.as_str()),
                    signed.taken,
                    peer,
                );
                lines.say(&signed_in(&tag, signed.taken)).await
            }
            "AUTHENTICATE" => {
                let mut words = rest.split(' ');
                let mechanism = words.next().unwrap_or_default().to_ascii_uppercase();
                let plain = mechanism == "PLAIN";
                let xoauth2 = mechanism == "XOAUTH2";
                let token = match words.next() {
                    Some(token) => Some(token.to_owned()),
                    None if (plain || xoauth2) && lines.say("+ ").await => lines.line().await,
                    None => None,
                };
                let plain_named = token.as_deref().filter(|_| plain).and_then(plain_login);
                let oauth_named = token.as_deref().filter(|_| xoauth2).and_then(oauth2_login);
                signed.taken = plain_named
                    .as_ref()
                    .is_some_and(|(login, password)| server.takes(login, password))
                    || oauth_named
                        .as_ref()
                        .is_some_and(|(user, token)| server.takes_token(user, token));
                if signed.taken {
                    server.logins.fetch_add(1, Ordering::SeqCst);
                }
                let login = plain_named.as_ref().or(oauth_named.as_ref());
                said_sign_in(login.map(|(login, _)| login.as_str()), signed.taken, peer);
                lines.say(&signed_in(&tag, signed.taken)).await
            }
            "IDLE" if signed.taken && server.tells_nothing.load(Ordering::SeqCst) => {
                lines.say(&format!("{tag} BAD unknown command")).await
            }
            "IDLE" if signed.taken => {
                if !lines.say("+ idling").await {
                    return;
                }
                // New mail or a flag ends the wait with an untagged answer, and the client then
                // says DONE.
                server.idling.fetch_add(1, Ordering::SeqCst);
                let heard = tokio::select! {
                    changed = news.changed() => changed.is_ok().then_some(true),
                    done = lines.line() => done.map(|_| false),
                };
                server.idling.fetch_sub(1, Ordering::SeqCst);
                let Some(heard) = heard else {
                    return;
                };
                if heard {
                    let count = signed.selected.as_deref().map_or(0, |name| {
                        server
                            .mail()
                            .folder(name)
                            .map_or(0, |folder| folder.messages.len())
                    });
                    if !lines.say(&format!("* {count} EXISTS")).await
                        || lines.line().await.is_none()
                    {
                        return;
                    }
                }
                lines.say(&format!("{tag} OK IDLE terminated")).await
            }
            "LOGOUT" => {
                lines.say("* BYE mail stand-in leaving").await;
                lines.say(&format!("{tag} OK LOGOUT completed")).await;
                return;
            }
            "APPEND" => {
                let folder = arguments(&rest).first().cloned().unwrap_or_default();
                match literal_size(&line) {
                    None => {
                        lines
                            .say(&format!("{tag} BAD APPEND needs a literal"))
                            .await
                    }
                    Some(_) if !signed.taken => lines.say(&format!("{tag} NO sign in first")).await,
                    Some(_) if server.refuses_appends.load(Ordering::SeqCst) => {
                        lines.say(&format!("{tag} NO quota exceeded")).await
                    }
                    // A server refuses a folder it does not hold before it takes the message,
                    // and says a client may create it.
                    Some(_) if server.mail().folder(&folder).is_none() => {
                        lines
                            .say(&format!("{tag} NO [TRYCREATE] no such mailbox"))
                            .await
                    }
                    Some(size) => {
                        if !lines.say("+ go on").await {
                            return;
                        }
                        let Some(raw) = lines.literal(size).await else {
                            return;
                        };
                        let kept =
                            server
                                .mail()
                                .keep(&folder, append_flags(&rest), raw, received_now());
                        server.announce();
                        match kept {
                            Some((validity, uid)) => {
                                lines
                                    .say(&format!(
                                        "{tag} OK [APPENDUID {validity} {uid}] APPEND completed"
                                    ))
                                    .await
                            }
                            None => {
                                lines
                                    .say(&format!("{tag} NO [TRYCREATE] no such mailbox"))
                                    .await
                            }
                        }
                    }
                }
            }
            "CAPABILITY" | "NOOP" | "ID" => {
                let reply = answer(&tag, &command, &rest, uid, &mut signed, server)
                    .unwrap_or_else(|| format!("{tag} OK\r\n"));
                lines.send(&reply).await
            }
            _ if !signed.taken => lines.say(&format!("{tag} NO sign in first")).await,
            _ => {
                let moved = command == "MOVE";
                match answer(&tag, &command, &rest, uid, &mut signed, server) {
                    Some(reply) => {
                        if moved {
                            server.announce();
                        }
                        lines.send(&reply).await
                    }
                    None => {
                        lines
                            .say(&format!(
                                "{tag} BAD the mail stand-in does not know {command}"
                            ))
                            .await
                    }
                }
            }
        };
        if !said {
            return;
        }
    }
}

/// What one signed-in command answers: every untagged line, and the tagged one, as the
/// connection sends them; none for a command this stand-in does not know.
#[expect(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "one IMAP command's dispatch, each answer built under the mailbox's one lock"
)]
fn answer(
    tag: &str,
    command: &str,
    rest: &str,
    uid: bool,
    signed: &mut Signed,
    server: &Server,
) -> Option<String> {
    match command {
        "CAPABILITY" => Some(format!(
            "* CAPABILITY {}\r\n{tag} OK CAPABILITY completed\r\n",
            server.capabilities()
        )),
        "NOOP" | "CHECK" | "ENABLE" | "SUBSCRIBE" | "UNSUBSCRIBE" | "UNSELECT" => {
            Some(format!("{tag} OK {command} completed\r\n"))
        }
        "ID" => Some(format!("* ID NIL\r\n{tag} OK ID completed\r\n")),
        "CLOSE" => {
            signed.selected = None;
            Some(format!("{tag} OK CLOSE completed\r\n"))
        }
        "NAMESPACE" => Some(format!(
            "* NAMESPACE ((\"\" \"{}\")) NIL NIL\r\n{tag} OK NAMESPACE completed\r\n",
            server.layout.separator()
        )),
        "LIST" | "LSUB" => {
            let separator = server.layout.separator();
            let pattern = arguments(rest).last().cloned().unwrap_or_default();
            if pattern.is_empty() {
                return Some(format!(
                    "* LIST (\\Noselect) \"{separator}\" \"\"\r\n{tag} OK LIST completed\r\n"
                ));
            }
            let mail = server.mail();
            let mut reply = String::new();
            for folder in &mail.folders {
                let mut attributes = vec!["\\HasNoChildren"];
                attributes.extend(folder.uses.iter().copied());
                let _ = write!(
                    reply,
                    "* LIST ({}) \"{separator}\" \"{}\"\r\n",
                    attributes.join(" "),
                    folder.name
                );
            }
            let _ = write!(reply, "{tag} OK LIST completed\r\n");
            Some(reply)
        }
        "SELECT" | "EXAMINE" => {
            let name = unquoted(rest);
            let mail = server.mail();
            let Some(folder) = mail.folder(&name) else {
                return Some(format!("{tag} NO no such mailbox\r\n"));
            };
            let (exists, validity, next) =
                (folder.messages.len(), folder.validity, folder.next_uid);
            signed.selected = Some(folder.name.clone());
            Some(format!(
                "* FLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft)\r\n\
* {exists} EXISTS\r\n* 0 RECENT\r\n\
* OK [UIDVALIDITY {validity}] the mail stand-in's mailbox\r\n\
* OK [UIDNEXT {next}] the next message it takes\r\n\
* OK [PERMANENTFLAGS (\\Answered \\Flagged \\Deleted \\Seen \\Draft \\*)] flags it keeps\r\n\
{tag} OK [READ-WRITE] {command} completed\r\n"
            ))
        }
        "STATUS" => {
            let name = arguments(rest).first().cloned().unwrap_or_default();
            let mail = server.mail();
            let Some(folder) = mail.folder(&name) else {
                return Some(format!("{tag} NO no such mailbox\r\n"));
            };
            let unseen = folder
                .messages
                .iter()
                .filter(|message| !has_flag(message, "\\Seen"))
                .count();
            let (exists, validity, next) =
                (folder.messages.len(), folder.validity, folder.next_uid);
            Some(format!(
                "* STATUS \"{}\" (MESSAGES {exists} RECENT 0 UIDNEXT {next} UIDVALIDITY {validity} UNSEEN {unseen})\r\n\
{tag} OK STATUS completed\r\n",
                folder.name
            ))
        }
        "CREATE" => {
            let name = unquoted(rest);
            let mut mail = server.mail();
            if mail.folder(&name).is_some() {
                return Some(format!("{tag} NO mailbox already exists\r\n"));
            }
            let validity = u32::try_from(mail.folders.len())
                .unwrap_or(0)
                .saturating_add(100);
            mail.folders.push(Folder {
                uses: server.layout.uses_of(&name).to_vec(),
                name,
                validity,
                next_uid: 1,
                messages: Vec::new(),
            });
            Some(format!("{tag} OK CREATE completed\r\n"))
        }
        "FETCH" => {
            let selected = signed.selected.clone()?;
            let (set, items) = fetch_arguments(rest);
            let mail = server.mail();
            let folder = mail.folder(&selected)?;
            let wanted = wanted(set, uid, &folder.messages);
            let mut reply = String::new();
            for (position, message) in positioned(&folder.messages) {
                if wanted(position, message) {
                    reply.push_str(&fetch_line(position, message, &items));
                }
            }
            let _ = write!(reply, "{tag} OK FETCH completed\r\n");
            Some(reply)
        }
        "SEARCH" => {
            let selected = signed.selected.clone()?;
            let keys = search_keys(rest);
            let mail = server.mail();
            let folder = mail.folder(&selected)?;
            let matching: Vec<String> = positioned(&folder.messages)
                .filter(|(_, message)| keys.iter().all(|key| key.matches(message)))
                .map(|(position, message)| if uid { message.uid } else { position }.to_string())
                .collect();
            Some(format!(
                "* SEARCH {}\r\n{tag} OK SEARCH completed\r\n",
                matching.join(" ")
            ))
        }
        "STORE" => {
            let selected = signed.selected.clone()?;
            let words: Vec<&str> = rest.split_whitespace().collect();
            let set = words.first().copied().unwrap_or_default();
            let modes = words
                .get(1)
                .copied()
                .unwrap_or_default()
                .to_ascii_uppercase();
            // What a store changes is named as RFC 3501 names it: `[+|-]FLAGS[.SILENT]`.
            if !matches!(
                modes.trim_start_matches(['+', '-']),
                "FLAGS" | "FLAGS.SILENT"
            ) {
                return Some(format!("{tag} BAD STORE names FLAGS or FLAGS.SILENT\r\n"));
            }
            let adding = modes.starts_with('+');
            let silent = modes.contains("SILENT");
            let flags: Vec<String> = rest
                .split_once('(')
                .and_then(|(_, tail)| tail.split_once(')'))
                .map(|(flags, _)| flags.split_whitespace().map(str::to_owned).collect())
                .unwrap_or_default();
            let mut mail = server.mail();
            let folder = mail.folder_mut(&selected)?;
            let wanted = wanted(set, uid, &folder.messages);
            let mut reply = String::new();
            for (position, message) in folder.messages.iter_mut().enumerate() {
                let position = u32::try_from(position).unwrap_or(0).saturating_add(1);
                if !wanted(position, message) {
                    continue;
                }
                for flag in &flags {
                    if adding {
                        if !message.flags.contains(flag) {
                            message.flags.push(flag.clone());
                        }
                    } else {
                        message.flags.retain(|kept| kept != flag);
                    }
                }
                if !silent {
                    let _ = write!(
                        reply,
                        "* {position} FETCH (UID {} FLAGS ({}))\r\n",
                        message.uid,
                        message.flags.join(" ")
                    );
                }
            }
            let _ = write!(reply, "{tag} OK STORE completed\r\n");
            Some(reply)
        }
        "MOVE" => {
            let selected = signed.selected.clone()?;
            let (set, destination) = rest.split_once(' ').unwrap_or((rest, ""));
            let destination = unquoted(destination);
            let mut mail = server.mail();
            let mut next_uid = mail.folder(&destination).map(|target| target.next_uid);
            if next_uid.is_none() {
                return Some(format!("{tag} NO [TRYCREATE] no such mailbox\r\n"));
            }
            let source = mail.folder_mut(&selected)?;
            let wanted = wanted(set, uid, &source.messages);
            let mut moved = Vec::new();
            let mut gone = Vec::new();
            let mut position = 0_u32;
            source.messages.retain(|message| {
                position = position.saturating_add(1);
                if wanted(position, message) {
                    moved.push(message.clone());
                    gone.push(position);
                    false
                } else {
                    true
                }
            });
            let target = mail.folder_mut(&destination)?;
            for mut message in moved {
                let uid = next_uid.unwrap_or(1);
                next_uid = Some(uid.saturating_add(1));
                message.uid = uid;
                target.messages.push(message);
            }
            target.next_uid = next_uid.unwrap_or(target.next_uid);
            Some(expunged(&gone, tag, "MOVE"))
        }
        "EXPUNGE" => {
            let selected = signed.selected.clone()?;
            let mut mail = server.mail();
            let folder = mail.folder_mut(&selected)?;
            let named = if uid {
                Some(uid_set(rest.trim(), &folder.messages))
            } else {
                None
            };
            let mut gone = Vec::new();
            let mut position = 0_u32;
            folder.messages.retain(|message| {
                position = position.saturating_add(1);
                let goes = has_flag(message, "\\Deleted")
                    && named
                        .as_ref()
                        .is_none_or(|named| named.contains(&message.uid));
                if goes {
                    gone.push(position);
                }
                !goes
            });
            Some(expunged(&gone, tag, "EXPUNGE"))
        }
        _ => None,
    }
}

/// The `EXPUNGE` lines for the messages that were at `gone` (counted before any went), the
/// later first so each position is still right when it is told, and the tagged end.
fn expunged(gone: &[u32], tag: &str, command: &str) -> String {
    let mut reply = String::new();
    for position in gone.iter().rev() {
        let _ = write!(reply, "* {position} EXPUNGE\r\n");
    }
    let _ = write!(reply, "{tag} OK {command} completed\r\n");
    reply
}

/// The messages of a folder with their positions, from 1.
fn positioned(messages: &[Stored]) -> impl Iterator<Item = (u32, &Stored)> {
    messages.iter().enumerate().map(|(position, message)| {
        (
            u32::try_from(position).unwrap_or(0).saturating_add(1),
            message,
        )
    })
}

/// Whether a message at a position is named by `set`, a set of UIDs when `uid`, of positions
/// otherwise.
fn wanted(set: &str, uid: bool, messages: &[Stored]) -> impl Fn(u32, &Stored) -> bool + use<> {
    let uids = if uid {
        uid_set(set, messages)
    } else {
        Vec::new()
    };
    let positions = if uid {
        Vec::new()
    } else {
        numbers(set, messages.len())
    };
    move |position, message| {
        if uid {
            uids.contains(&message.uid)
        } else {
            positions.contains(&position)
        }
    }
}

async fn smtp_connection(tcp: TcpStream, server: Arc<Server>, wrapped: bool) {
    let greeting = format!("220 {} ESMTP mail stand-in", server.name);
    let tcp = if wrapped {
        tcp
    } else if let Some(upgraded) = smtp_in_the_clear(tcp, &greeting, &server.name).await {
        upgraded
    } else {
        return;
    };
    let Some(mut lines) = secured(tcp, &server).await else {
        return;
    };
    // A connection that upgraded was greeted in the clear and is not greeted again.
    if wrapped && !lines.say(&greeting).await {
        return;
    }
    smtp_secure(&mut lines, &server).await;
}

/// The cleartext start of an SMTP connection: the greeting, what is offered, and the upgrade.
/// The connection once the client may begin its handshake; nothing signs in before it.
async fn smtp_in_the_clear(tcp: TcpStream, greeting: &str, name: &str) -> Option<TcpStream> {
    let mut lines = Lines(BufReader::new(tcp));
    lines.say(greeting).await.then_some(())?;
    loop {
        let line = lines.line().await?;
        let command = line
            .split(' ')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let said = match command.as_str() {
            "EHLO" | "HELO" => {
                lines.say(&format!("250-{name}")).await && lines.say("250 STARTTLS").await
            }
            "STARTTLS" => {
                let begun = lines.say("220 ready to start TLS").await;
                return begun.then(|| lines.0.into_inner());
            }
            "QUIT" => {
                lines.say("221 bye").await;
                return None;
            }
            "NOOP" | "RSET" => lines.say("250 ok").await,
            _ => lines.say("530 5.7.0 secure the connection first").await,
        };
        if !said {
            return None;
        }
    }
}

/// The secured part of an SMTP connection: what is offered, the sign-in, and the mail it takes.
/// What it takes is kept, and a message to its own login arrives in its inbox.
#[expect(
    clippy::too_many_lines,
    reason = "one SMTP conversation's dispatch: the sign-in mechanisms and the mail it takes"
)]
async fn smtp_secure<S: AsyncRead + AsyncWrite + Unpin>(lines: &mut Lines<S>, server: &Server) {
    let mut signed_in = false;
    let mut recipients: Vec<String> = Vec::new();
    while let Some(line) = lines.line().await {
        let mut words = line.split(' ');
        let command = words.next().unwrap_or_default().to_ascii_uppercase();
        let said = match command.as_str() {
            "EHLO" | "HELO" => {
                lines.say(&format!("250-{}", server.name)).await
                    && lines.say("250 AUTH PLAIN LOGIN XOAUTH2").await
            }
            "AUTH" => {
                let mechanism = words.next().unwrap_or_default().to_ascii_uppercase();
                let said = match mechanism.as_str() {
                    "PLAIN" => match words.next() {
                        Some(token) => plain_login(token),
                        None if lines.say("334 ").await => {
                            lines.line().await.as_deref().and_then(plain_login)
                        }
                        None => None,
                    },
                    "LOGIN" => match lines.asked("334 VXNlcm5hbWU6").await {
                        Some(login) => lines
                            .asked("334 UGFzc3dvcmQ6")
                            .await
                            .map(|password| (login, password)),
                        None => None,
                    },
                    "XOAUTH2" => match words.next() {
                        Some(token) => oauth2_login(token),
                        None if lines.say("334 ").await => {
                            lines.line().await.as_deref().and_then(oauth2_login)
                        }
                        None => None,
                    },
                    _ => None,
                };
                let accepted = said
                    .as_ref()
                    .is_some_and(|(login, password)| server.takes(login, password));
                let accepted_token = mechanism == "XOAUTH2"
                    && said
                        .as_ref()
                        .is_some_and(|(user, token)| server.takes_token(user, token));
                signed_in = accepted || accepted_token;
                if signed_in {
                    lines.say("235 2.7.0 signed in").await
                } else {
                    lines
                        .say("535 5.7.8 the mail stand-in does not take this login")
                        .await
                }
            }
            "MAIL" if signed_in => {
                recipients.clear();
                lines.say("250 ok").await
            }
            "RCPT" if signed_in => {
                let to = line
                    .split_once('<')
                    .and_then(|(_, rest)| rest.split_once('>'))
                    .map_or("", |(address, _)| address);
                recipients.push(to.to_ascii_lowercase());
                lines.say("250 ok").await
            }
            "DATA" if signed_in => {
                if !lines.say("354 go ahead").await {
                    return;
                }
                let mut raw = Vec::new();
                let mut complete = false;
                while let Some(line) = lines.line().await {
                    if line == "." {
                        complete = true;
                        break;
                    }
                    // A line the client began with a dot carries one more.
                    let line = line
                        .strip_prefix('.')
                        .filter(|_| line.starts_with(".."))
                        .unwrap_or(&line);
                    raw.extend_from_slice(line.as_bytes());
                    raw.extend_from_slice(b"\r\n");
                }
                if !complete {
                    return;
                }
                server
                    .sent
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(raw.clone());
                let own = recipients
                    .iter()
                    .any(|to| to.eq_ignore_ascii_case(&server.login));
                if own {
                    let kept = server.mail().keep("INBOX", Vec::new(), raw, received_now());
                    server.announce();
                    if let Some((_, uid)) = kept {
                        // Said, so a rehearsal sees the mail it was handed arrive.
                        eprintln!("mail stand-in: took a message as its {uid}th in the inbox");
                    }
                }
                lines.say("250 2.0.0 queued").await
            }
            "MAIL" | "RCPT" => lines.say("530 5.7.0 sign in first").await,
            "NOOP" | "RSET" => lines.say("250 ok").await,
            "QUIT" => {
                lines.say("221 bye").await;
                return;
            }
            _ => {
                lines
                    .say("502 5.5.1 the mail stand-in does not know this command")
                    .await
            }
        };
        if !said {
            return;
        }
    }
}

/// `certificate` as the text a trust bundle holds.
#[must_use]
pub fn pem(certificate: &CertificateDer<'_>) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(certificate.as_ref());
    let mut text = String::from("-----BEGIN CERTIFICATE-----\n");
    for piece in encoded.as_bytes().chunks(64) {
        text.push_str(&String::from_utf8_lossy(piece));
        text.push('\n');
    }
    text.push_str("-----END CERTIFICATE-----\n");
    text
}

/// A command line's tag, its command in capitals, and what follows.
fn parts(line: &str) -> (String, String, String) {
    let mut words = line.splitn(3, ' ');
    let tag = words.next().unwrap_or_default().to_owned();
    let command = words.next().unwrap_or_default().to_ascii_uppercase();
    let rest = words.next().unwrap_or_default().to_owned();
    (tag, command, rest)
}

/// The words of a command's arguments: quoted strings without their quotes, and bare words.
fn arguments(rest: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut letters = rest.chars().peekable();
    while let Some(first) = letters.next() {
        if first != ' ' {
            let mut word = String::new();
            if first == '"' {
                while let Some(letter) = letters.next() {
                    match letter {
                        '"' => break,
                        '\\' => word.extend(letters.next()),
                        other => word.push(other),
                    }
                }
            } else {
                word.push(first);
                while let Some(letter) = letters.next_if(|letter| *letter != ' ') {
                    word.push(letter);
                }
            }
            words.push(word);
        }
    }
    words
}

/// `rest` without the quotes it may be written in.
fn unquoted(rest: &str) -> String {
    let rest = rest.trim();
    rest.strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(rest)
        .replace("\\\"", "\"")
        .replace("\\\\", "\\")
}

/// The size a command's literal is declared with (`{123}` at its end).
fn literal_size(line: &str) -> Option<usize> {
    line.rsplit_once('{')?
        .1
        .strip_suffix('}')?
        .trim_end_matches('+')
        .parse()
        .ok()
}

/// The login and the password a `PLAIN` sign-in carries.
fn plain_login(token: &str) -> Option<(String, String)> {
    let said = base64::engine::general_purpose::STANDARD
        .decode(token.trim())
        .ok()?;
    let mut fields = said
        .split(|byte| *byte == 0)
        .map(|field| String::from_utf8_lossy(field).into_owned());
    let (_, login, password) = (fields.next()?, fields.next()?, fields.next()?);
    Some((login, password))
}

/// The user and bearer token an `XOAUTH2` sign-in carries, decoded:
/// `user=<name>\x01auth=Bearer <token>\x01\x01`.
fn oauth2_login(token: &str) -> Option<(String, String)> {
    let said = base64::engine::general_purpose::STANDARD
        .decode(token.trim())
        .ok()?;
    let said = String::from_utf8(said).ok()?;
    let mut fields = said.split('\u{1}');
    let user = fields.next()?.strip_prefix("user=")?.to_owned();
    let bearer = fields.next()?.strip_prefix("auth=Bearer ")?.to_owned();
    Some((user, bearer))
}
/// Say an IMAP sign-in on standard error: the login it named, whether it was taken, and the
/// process that holds the client's end of the connection, with the processes that started it.
fn said_sign_in(login: Option<&str>, taken: bool, peer: Option<SocketAddr>) {
    let client = peer.and_then(client_of).map_or_else(
        || "a process this stand-in cannot read".to_owned(),
        |lineage| format!("process {lineage}"),
    );
    let from = peer.map_or_else(|| "an unknown address".to_owned(), |peer| peer.to_string());
    eprintln!(
        "mail stand-in: IMAP sign-in as {} from {from} by {client}: {}",
        login.unwrap_or("(no login)"),
        if taken { "taken" } else { "refused" }
    );
}

/// The process holding the client's end of the loopback connection from `peer`, and each
/// process that started it, as `pid name < pid name < …`. Only where this machine's processes
/// are readable: on Linux, the socket whose own address is `peer` and the process one of whose
/// descriptors it is.
#[cfg(target_os = "linux")]
fn client_of(peer: SocketAddr) -> Option<String> {
    let inode = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|table| std::fs::read_to_string(table).ok())
        .find_map(|table| socket_inode(&table, peer))?;
    let wanted = format!("socket:[{inode}]");
    let holder = std::fs::read_dir("/proc")
        .ok()?
        .flatten()
        .find_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<u32>().ok()?;
            std::fs::read_dir(entry.path().join("fd"))
                .ok()?
                .flatten()
                .any(|fd| {
                    std::fs::read_link(fd.path())
                        .is_ok_and(|link| link.as_os_str() == wanted.as_str())
                })
                .then_some(pid)
        })?;
    let mut lineage = Vec::new();
    let mut pid = holder;
    while pid > 0 && lineage.len() < 64 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // `pid (name) state ppid …`: a name may hold spaces and brackets, so it ends at the last.
        let (head, tail) = stat.rsplit_once(')')?;
        let name = head.split_once('(').map_or("", |(_, name)| name);
        lineage.push(format!("{pid} {name}"));
        pid = tail.split_whitespace().nth(1)?.parse().ok()?;
    }
    Some(lineage.join(" < "))
}

#[cfg(not(target_os = "linux"))]
fn client_of(_peer: SocketAddr) -> Option<String> {
    None
}

/// The inode of the socket of a `/proc/net/tcp` or `tcp6` table whose own address is `peer`.
#[cfg(target_os = "linux")]
fn socket_inode(table: &str, peer: SocketAddr) -> Option<u64> {
    let wanted = (peer.ip().to_canonical(), peer.port());
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (address, port) = fields.get(1)?.split_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        // Each 32-bit word of the address is printed as the machine holds it.
        let words = address
            .as_bytes()
            .chunks(8)
            .map(|word| {
                std::str::from_utf8(word)
                    .ok()
                    .and_then(|word| u32::from_str_radix(word, 16).ok())
            })
            .collect::<Option<Vec<u32>>>()?;
        let bytes: Vec<u8> = words.iter().flat_map(|word| word.to_ne_bytes()).collect();
        let ip = match bytes.len() {
            4 => std::net::IpAddr::from(<[u8; 4]>::try_from(bytes).ok()?),
            16 => std::net::IpAddr::from(<[u8; 16]>::try_from(bytes).ok()?),
            _ => return None,
        };
        ((ip.to_canonical(), port) == wanted).then(|| fields.get(9)?.parse().ok())?
    })
}

fn signed_in(tag: &str, taken: bool) -> String {
    if taken {
        format!("{tag} OK signed in")
    } else {
        format!("{tag} NO [AUTHENTICATIONFAILED] the mail stand-in does not take this login")
    }
}

/// The flags an `APPEND` gives the message it keeps, as `(\\Seen \\Draft)` writes them.
fn append_flags(rest: &str) -> Vec<String> {
    let Some((open, closed)) = rest
        .split_once('(')
        .and_then(|(_, tail)| tail.split_once(')'))
    else {
        return Vec::new();
    };
    let _ = closed;
    open.split_whitespace().map(str::to_owned).collect()
}

fn has_flag(message: &Stored, flag: &str) -> bool {
    message
        .flags
        .iter()
        .any(|kept| kept.eq_ignore_ascii_case(flag))
}

/// The message set and the items of a `FETCH`: `1:3 (UID FLAGS …)`.
fn fetch_arguments(rest: &str) -> (&str, String) {
    let set = rest.split_whitespace().next().unwrap_or_default();
    let items = rest
        .split_once('(')
        .map_or_else(String::new, |(_, items)| items.to_ascii_uppercase());
    (set.trim_matches('('), items)
}

/// The positions a message set names, `*` being the folder's newest: `1`, `2:4`, `1,3`, `1:*`.
fn numbers(set: &str, count: usize) -> Vec<u32> {
    let newest = u32::try_from(count).unwrap_or(0);
    let mut wanted = Vec::new();
    for part in set.split(',') {
        let (first, last) = part.split_once(':').unwrap_or((part, part));
        let bound = |end: &str| {
            if end == "*" {
                newest
            } else {
                end.parse().unwrap_or(0)
            }
        };
        let (first, last) = (bound(first), bound(last));
        if first == 0 || last == 0 {
            continue;
        }
        wanted.extend(first.min(last)..=first.max(last));
    }
    wanted
}

/// The UIDs a message set names, `*` being the folder's newest.
fn uid_set(set: &str, messages: &[Stored]) -> Vec<u32> {
    let newest = messages
        .iter()
        .map(|message| message.uid)
        .max()
        .unwrap_or(0);
    let mut wanted = Vec::new();
    for part in set.split(',') {
        let (first, last) = part.split_once(':').unwrap_or((part, part));
        let bound = |end: &str| {
            if end == "*" {
                newest
            } else {
                end.parse().unwrap_or(0)
            }
        };
        let (first, last) = (bound(first), bound(last));
        wanted.extend(
            messages
                .iter()
                .map(|message| message.uid)
                .filter(|uid| *uid >= first.min(last) && *uid <= first.max(last)),
        );
    }
    wanted
}

/// One message as `FETCH` answers it: its position, its flags, when it arrived, and, when
/// asked for, its words as a literal.
fn fetch_line(position: u32, message: &Stored, items: &str) -> String {
    let mut said = format!(
        "* {position} FETCH (UID {} FLAGS ({})",
        message.uid,
        message.flags.join(" ")
    );
    if items.contains("INTERNALDATE") {
        let _ = write!(said, " INTERNALDATE \"{}\"", message.received);
    }
    if items.contains("BODY") || items.contains("RFC822") {
        let _ = write!(said, " BODY[] {{{}}}\r\n", message.raw.len());
        said.push_str(&String::from_utf8_lossy(&message.raw));
    }
    said.push_str(")\r\n");
    said
}

/// One key of a search, as far as this server answers them. A message has to match every key
/// of its search.
enum SearchKey {
    /// Words anywhere in the message.
    Text(String),
    /// Words in the header of this name alone.
    Header(String, String),
    Unseen,
    Seen,
    Flagged,
    /// A range of UIDs, both ends included.
    Uids(u32, u32),
    /// `ALL`, and what this server does not know.
    Any,
}

impl SearchKey {
    fn matches(&self, message: &Stored) -> bool {
        let raw = String::from_utf8_lossy(&message.raw).to_ascii_lowercase();
        match self {
            Self::Text(wanted) => raw.contains(wanted),
            Self::Header(name, wanted) => {
                let headers = raw.split("\r\n\r\n").next().unwrap_or_default();
                let prefix = format!("{name}:");
                headers.lines().any(|line| {
                    line.strip_prefix(&prefix)
                        .is_some_and(|value| value.contains(wanted))
                })
            }
            Self::Unseen => !has_flag(message, "\\Seen"),
            Self::Seen => has_flag(message, "\\Seen"),
            Self::Flagged => has_flag(message, "\\Flagged"),
            Self::Uids(first, last) => (*first..=*last).contains(&message.uid),
            Self::Any => true,
        }
    }
}

/// The keys of the search `query` asks: `TEXT`, `FROM`, `TO`, `SUBJECT`, `HEADER name`, each
/// with the words it looks for, `UNSEEN`, `SEEN`, `FLAGGED`, and `UID` with a range.
fn search_keys(query: &str) -> Vec<SearchKey> {
    let mut words = search_words(query).into_iter();
    let mut keys = Vec::new();
    while let Some(word) = words.next() {
        let mut wanted = || words.next().unwrap_or_default().to_ascii_lowercase();
        keys.push(match word.to_ascii_uppercase().as_str() {
            "TEXT" => SearchKey::Text(wanted()),
            "FROM" | "TO" | "SUBJECT" => SearchKey::Header(word.to_ascii_lowercase(), wanted()),
            "HEADER" => {
                let name = wanted();
                SearchKey::Header(name, wanted())
            }
            "UNSEEN" => SearchKey::Unseen,
            "SEEN" => SearchKey::Seen,
            "FLAGGED" => SearchKey::Flagged,
            "UID" => {
                let set = wanted();
                let (first, last) = set.split_once(':').unwrap_or((&set, &set));
                SearchKey::Uids(
                    first.parse().unwrap_or(0),
                    if last == "*" {
                        u32::MAX
                    } else {
                        last.parse().unwrap_or(u32::MAX)
                    },
                )
            }
            _ => SearchKey::Any,
        });
    }
    keys
}

/// The words of a search, a quoted string as one word without its quotes.
fn search_words(query: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut letters = query.chars();
    while let Some(letter) = letters.next() {
        match letter {
            '\\' if quoted => word.extend(letters.next()),
            '"' => {
                if quoted {
                    words.push(std::mem::take(&mut word));
                }
                quoted = !quoted;
            }
            ' ' if !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            letter => word.push(letter),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// When this message arrived, as `INTERNALDATE` writes it: the clock now, in UTC.
#[expect(
    clippy::arithmetic_side_effects,
    reason = "a calendar date is arithmetic over the clock, bounded by the epoch's own days"
)]
fn received_now() -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let rest = seconds % 86_400;
    // The day counted from 1970-01-01, as a civil date.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted - era * 146_097;
    let year_of_era = (of_era - of_era / 1_460 + of_era / 36_524 - of_era / 146_096) / 365;
    let day_of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_number = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_number + 2) / 5 + 1;
    let month = if month_number < 10 {
        month_number + 3
    } else {
        month_number - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    let month = MONTHS
        .get(usize::try_from(month).unwrap_or(0).saturating_sub(1))
        .copied()
        .unwrap_or("Jan");
    format!(
        "{day:02}-{month}-{year} {:02}:{:02}:{:02} +0000",
        rest / 3_600,
        (rest / 60) % 60,
        rest % 60
    )
}
