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
//! * **Default (this build): a stub backend.** It parses the bundled sample
//!   corpus and acknowledges sends by building the real MIME with `mail-builder`
//!   (see `transport::build_mime`). No sockets, no TLS, no async runtime — so
//!   the headless build stays small and green and reproducible.
//! * **`live` feature: real IMAP/SMTP.** Behind `--features live`, the worker
//!   uses [`async-imap`] for fetch/flag/move and [`lettre`] for submission,
//!   over rustls. Those crates pull a TLS + async stack we deliberately keep
//!   out of the default build (and out of `--shots`); the data types
//!   ([`AccountConfig`], [`Draft`], [`Message`]) are already the exact shapes
//!   those libraries consume and produce, so enabling the feature is a drop-in.
//!
//! Both backends speak the same [`MailEngine`] trait, so the app code is
//! identical regardless of which is compiled in.

use std::sync::mpsc::{Receiver, Sender};

use crate::model::{AccountConfig, Draft, Folder, Message};
use crate::oauth::OAuthProvider;
use crate::transport::build_mime;

/// A request from the UI to the engine worker.
#[derive(Clone, Debug)]
pub enum EngineRequest {
    /// Connect and list the folders for an account.
    Connect { account: AccountConfig },
    /// Fetch the messages of a folder (`UID FETCH` in the live backend).
    Fetch { account: AccountConfig, folder: Folder },
    /// Persist a flag change to the server (`UID STORE \Seen`/`\Flagged`).
    StoreFlag { account: AccountConfig, message: Box<Message> },
    /// Move a message to another folder (`UID MOVE`). Carries the full message
    /// so the live backend can locate it on the server by its `Message-ID`.
    Move { account: AccountConfig, message: Box<Message>, to: Folder },
    /// Submit a draft via SMTP.
    Send { account: AccountConfig, draft: Box<Draft> },
    /// Run the OAuth authorization-code + PKCE browser flow for an account and
    /// seal the resulting tokens in the vault under `account_id`.
    OAuthAuthorize { account_id: String, provider: OAuthProvider, login_hint: String },
}

impl EngineRequest {
    /// The account this request targets, for the connect/fetch dedupe + error
    /// context. Empty for requests without a single account (OAuth carries its
    /// own id).
    pub fn account_email(&self) -> String {
        match self {
            EngineRequest::Connect { account }
            | EngineRequest::Fetch { account, .. }
            | EngineRequest::StoreFlag { account, .. }
            | EngineRequest::Move { account, .. }
            | EngineRequest::Send { account, .. } => account.email.clone(),
            EngineRequest::OAuthAuthorize { account_id, .. } => account_id.clone(),
        }
    }
}

/// An event from the engine worker back to the UI.
#[derive(Clone, Debug)]
pub enum EngineEvent {
    /// Connection + authentication established.
    Connected { account_id: String },
    /// The mailbox folders the server advertised (`LIST`).
    Folders { account_id: String, folders: Vec<String> },
    /// A batch of fetched messages for a folder.
    Fetched { account_id: String, folder: Folder, messages: Vec<Message> },
    /// A send completed; carries the wire byte count.
    Sent { wire_bytes: usize },
    /// An OAuth sign-in completed and the tokens were sealed in the vault.
    Authorized { account_id: String },
    /// Any operation failed; carries a human-readable reason.
    Error { context: String, message: String },
}

/// The backend abstraction. The stub and the live IMAP/SMTP backend both
/// implement it, so the app's calling code never branches on which is active.
pub trait MailEngine: Send {
    /// Handle one request, emitting zero or more events onto `out`.
    fn handle(&mut self, req: EngineRequest, out: &Sender<EngineEvent>);
}

/// The default, offline backend. It validates sends by building real MIME and
/// reports the wire size, and acknowledges connect/fetch without touching the
/// network. Live IMAP/SMTP is the `live`-feature backend below.
pub struct StubEngine;

impl MailEngine for StubEngine {
    fn handle(&mut self, req: EngineRequest, out: &Sender<EngineEvent>) {
        match req {
            EngineRequest::Connect { account } => {
                let _ = out.send(EngineEvent::Connected { account_id: account.email });
            }
            EngineRequest::Fetch { account, folder } => {
                // The offline backend has no server to fetch from; the app shows
                // its bundled corpus. Acknowledge with an empty batch so the
                // caller's state machine still advances.
                let _ = out.send(EngineEvent::Fetched { account_id: account.email, folder, messages: Vec::new() });
            }
            EngineRequest::StoreFlag { .. } | EngineRequest::Move { .. } => {
                // No-op offline; flags persist in the local CRDT.
            }
            EngineRequest::OAuthAuthorize { account_id, .. } => {
                // Sign-in needs the live network stack; the offline build can't
                // complete it. Surface a clear reason instead of silently
                // pretending to have authorized.
                let _ = out.send(EngineEvent::Error {
                    context: "oauth".into(),
                    message: format!("OAuth sign-in for {account_id} requires the live build"),
                });
            }
            EngineRequest::Send { account, draft } => {
                let from = crate::model::Addr::new(account.display_name.clone(), account.email.clone());
                match build_mime(&from, &draft) {
                    Ok(bytes) => {
                        let _ = out.send(EngineEvent::Sent { wire_bytes: bytes.len() });
                    }
                    Err(e) => {
                        let _ = out.send(EngineEvent::Error {
                            context: "send".into(),
                            message: e.to_string(),
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
/// used; otherwise the offline [`StubEngine`].
pub fn spawn() -> (Sender<EngineRequest>, Receiver<EngineEvent>) {
    #[cfg(feature = "live")]
    {
        match live::LiveEngine::new() {
            Ok(engine) => spawn_with(Box::new(engine)),
            Err(e) => {
                tracing::warn!(error = %e, "mail: live engine unavailable, using offline backend");
                spawn_with(Box::new(StubEngine))
            }
        }
    }
    #[cfg(not(feature = "live"))]
    {
        spawn_with(Box::new(StubEngine))
    }
}

/// Spawn the worker with a specific backend (used by tests and the `live`
/// feature).
pub fn spawn_with(mut backend: Box<dyn MailEngine>) -> (Sender<EngineRequest>, Receiver<EngineEvent>) {
    let (req_tx, req_rx) = std::sync::mpsc::channel::<EngineRequest>();
    let (evt_tx, evt_rx) = std::sync::mpsc::channel::<EngineEvent>();
    std::thread::Builder::new()
        .name("mail-engine".into())
        .spawn(move || {
            while let Ok(req) = req_rx.recv() {
                backend.handle(req, &evt_tx);
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

    use accounts::{AccountId, SecretVault};
    use futures::TryStreamExt;

    use super::*;
    // AccountConfig / Draft / Folder / Message / Sender / build_mime come via
    // `super::*`; these are the names the parent doesn't already bring in.
    use crate::model::{Addr, AuthMethod};
    use crate::oauth;
    use crate::transport::parse_fetched;
    use crate::validate::classify_conn_failure;

    /// Real IMAP fetch/flag/move + SMTP submit + OAuth sign-in. The worker owns
    /// a Tokio runtime and `block_on`s each request; this module is the only
    /// place the app opens a socket.
    pub struct LiveEngine {
        rt: tokio::runtime::Runtime,
        tls: Arc<rustls::ClientConfig>,
    }

    /// The credentials to present to a server, resolved from the account +
    /// vault (a password, or a fresh OAuth access token).
    enum AuthCreds {
        Password { user: String, pass: String },
        XOAuth2 { user: String, token: String },
    }

    impl LiveEngine {
        pub fn new() -> Result<Self, String> {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            Ok(Self { rt, tls: build_tls_config() })
        }

        fn handle_fetch(&self, account: &AccountConfig, folder: Folder) -> Result<Vec<Message>, String> {
            let creds = resolve_auth(account)?;
            let tls = self.tls.clone();
            self.rt.block_on(fetch_folder(account, folder, &creds, tls))
        }

        fn handle_store(&self, account: &AccountConfig, message: &Message) -> Result<(), String> {
            let creds = resolve_auth(account)?;
            let tls = self.tls.clone();
            self.rt.block_on(store_flags(account, message, &creds, tls))
        }

        fn handle_move(&self, account: &AccountConfig, message: &Message, to: Folder) -> Result<(), String> {
            let creds = resolve_auth(account)?;
            let tls = self.tls.clone();
            self.rt.block_on(move_message(account, message, to, &creds, tls))
        }

        fn handle_send(&self, account: &AccountConfig, draft: &Draft) -> Result<usize, String> {
            let creds = resolve_auth(account)?;
            let from = Addr::new(account.display_name.clone(), account.email.clone());
            let bytes = build_mime(&from, draft).map_err(|e| e.to_string())?;
            self.rt.block_on(smtp_send(account, draft, &creds, &bytes))?;
            Ok(bytes.len())
        }
    }

    impl MailEngine for LiveEngine {
        fn handle(&mut self, req: EngineRequest, out: &Sender<EngineEvent>) {
            match req {
                EngineRequest::Connect { account } => {
                    // A connect verifies auth by opening a session and listing
                    // folders, so a bad credential surfaces immediately rather
                    // than as a silently-empty inbox.
                    let creds = match resolve_auth(&account) {
                        Ok(c) => c,
                        Err(e) => {
                            let _ = out.send(EngineEvent::Error { context: "connect".into(), message: e });
                            return;
                        }
                    };
                    let tls = self.tls.clone();
                    match self.rt.block_on(list_folders(&account, &creds, tls)) {
                        Ok(folders) => {
                            let _ = out.send(EngineEvent::Connected { account_id: account.email.clone() });
                            let _ = out.send(EngineEvent::Folders { account_id: account.email, folders });
                        }
                        Err(e) => {
                            let _ = out.send(EngineEvent::Error { context: "connect".into(), message: e });
                        }
                    }
                }
                EngineRequest::Fetch { account, folder } => match self.handle_fetch(&account, folder) {
                    Ok(messages) => {
                        let _ = out.send(EngineEvent::Fetched { account_id: account.email, folder, messages });
                    }
                    Err(e) => {
                        let _ = out.send(EngineEvent::Error { context: "fetch".into(), message: e });
                    }
                },
                EngineRequest::StoreFlag { account, message } => {
                    if let Err(e) = self.handle_store(&account, &message) {
                        let _ = out.send(EngineEvent::Error { context: "flag".into(), message: e });
                    }
                }
                EngineRequest::Move { account, message, to } => {
                    if let Err(e) = self.handle_move(&account, &message, to) {
                        let _ = out.send(EngineEvent::Error { context: "move".into(), message: e });
                    }
                }
                EngineRequest::Send { account, draft } => match self.handle_send(&account, &draft) {
                    Ok(wire_bytes) => {
                        let _ = out.send(EngineEvent::Sent { wire_bytes });
                    }
                    Err(e) => {
                        let _ = out.send(EngineEvent::Error { context: "smtp".into(), message: e });
                    }
                },
                EngineRequest::OAuthAuthorize { account_id, provider, login_hint } => {
                    match authorize_oauth(&account_id, provider, &login_hint) {
                        Ok(()) => {
                            let _ = out.send(EngineEvent::Authorized { account_id });
                        }
                        Err(e) => {
                            let _ = out.send(EngineEvent::Error { context: "oauth".into(), message: e });
                        }
                    }
                }
            }
        }
    }

    /// Build the shared rustls client config with the bundled Mozilla roots.
    fn build_tls_config() -> Arc<rustls::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("rustls default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Arc::new(cfg)
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
    /// fresh OAuth access token from the vault (refreshing it when expired).
    fn resolve_auth(account: &AccountConfig) -> Result<AuthCreds, String> {
        let user = login_user(account);
        match account.auth {
            AuthMethod::Password => {
                if account.password.is_empty() {
                    return Err("no password set for this account".into());
                }
                Ok(AuthCreds::Password { user, pass: account.password.clone() })
            }
            AuthMethod::OAuth => {
                let token = access_token_for(&account.email)?;
                Ok(AuthCreds::XOAuth2 { user, token })
            }
        }
    }

    /// Fetch (and, when near expiry, refresh) the OAuth access token for an
    /// account from the vault. Refreshed tokens are sealed back so the next
    /// request reuses them.
    fn access_token_for(email: &str) -> Result<String, String> {
        let mut vault = SecretVault::open();
        let id = AccountId::from(email.to_string());
        let secret = vault.get(&id).ok_or("not signed in (no OAuth token in vault)")?;
        let mut tokens =
            oauth::tokens_from_secret(&secret).ok_or("stored credential is not an OAuth token")?;
        if tokens.needs_refresh() {
            let domain = email.split_once('@').map(|(_, d)| d).unwrap_or("");
            let provider = oauth::OAuthProvider::from_domain(domain)
                .ok_or("cannot refresh: unknown OAuth provider")?;
            let client_id = provider
                .client_id()
                .ok_or_else(|| format!("set {} to refresh the token", provider.client_id_env()))?;
            if tokens.refresh_token.is_empty() {
                return Err("access token expired and no refresh token; sign in again".into());
            }
            tokens = oauth::refresh_tokens(provider, &client_id, &tokens.refresh_token)?;
            vault.put(&id, &oauth::tokens_to_secret(&tokens));
            vault.save();
        }
        Ok(tokens.access_token)
    }

    /// Run the browser OAuth flow and seal the tokens under `account_id`.
    fn authorize_oauth(
        account_id: &str,
        provider: oauth::OAuthProvider,
        login_hint: &str,
    ) -> Result<(), String> {
        let client_id = provider
            .client_id()
            .ok_or_else(|| format!("set {} to sign in", provider.client_id_env()))?;
        let tokens = oauth::run_loopback_authorization(
            provider,
            &client_id,
            login_hint,
            |url| {
                let _ = std::process::Command::new("xdg-open").arg(url).spawn();
            },
            std::time::Duration::from_secs(300),
        )?;
        let mut vault = SecretVault::open();
        vault.put(&AccountId::from(account_id.to_string()), &oauth::tokens_to_secret(&tokens));
        vault.save();
        Ok(())
    }

    /// A connected + authenticated IMAP session over rustls.
    type ImapSession =
        async_imap::Session<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

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
        let port = if account.imap_port == 0 { 993 } else { account.imap_port };
        let tcp = tokio::net::TcpStream::connect((host, port))
            .await
            .map_err(|e| classify_conn_failure(&e.to_string()))?;
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
                let auth = XOAuth2 { user: user.clone(), token: token.clone() };
                client
                    .authenticate("XOAUTH2", auth)
                    .await
                    .map_err(|(e, _)| classify_conn_failure(&e.to_string()))
            }
        }
    }

    /// async-imap `XOAUTH2` authenticator. `process` returns the RAW SASL
    /// string; async-imap base64-encodes it before sending.
    struct XOAuth2 {
        user: String,
        token: String,
    }

    impl async_imap::Authenticator for XOAuth2 {
        type Response = String;
        fn process(&mut self, _challenge: &[u8]) -> Self::Response {
            format!("user={}\x01auth=Bearer {}\x01\x01", self.user, self.token)
        }
    }

    /// The IMAP mailbox + search predicate a [`Folder`] maps to. `Starred` is a
    /// virtual view of INBOX filtered by `\Flagged`.
    fn mailbox_and_search(folder: Folder) -> (&'static str, &'static str) {
        match folder {
            Folder::Starred => ("INBOX", "FLAGGED"),
            other => (other.imap_name(), "ALL"),
        }
    }

    /// LIST every mailbox the server advertises (verifies auth as a side effect).
    async fn list_folders(
        account: &AccountConfig,
        creds: &AuthCreds,
        tls: Arc<rustls::ClientConfig>,
    ) -> Result<Vec<String>, String> {
        let mut session = connect_session(account, creds, tls).await?;
        let names: Vec<String> = {
            let stream = session
                .list(Some(""), Some("*"))
                .await
                .map_err(|e| e.to_string())?;
            stream
                .map_ok(|n| n.name().to_string())
                .try_collect()
                .await
                .map_err(|e| e.to_string())?
        };
        let _ = session.logout().await;
        Ok(names)
    }

    /// SELECT a folder, fetch the most recent messages, and parse them.
    async fn fetch_folder(
        account: &AccountConfig,
        folder: Folder,
        creds: &AuthCreds,
        tls: Arc<rustls::ClientConfig>,
    ) -> Result<Vec<Message>, String> {
        let (mailbox, search) = mailbox_and_search(folder);
        let now_min = crate::state::now_minutes();
        let mut session = connect_session(account, creds, tls).await?;
        session.select(mailbox).await.map_err(|e| e.to_string())?;
        let uids: std::collections::HashSet<u32> =
            session.uid_search(search).await.map_err(|e| e.to_string())?;
        // Newest ~50 UIDs (highest numbers), so a large mailbox stays responsive.
        let mut uids: Vec<u32> = uids.into_iter().collect();
        uids.sort_unstable();
        let recent: Vec<u32> = uids.iter().rev().take(50).copied().collect();
        if recent.is_empty() {
            let _ = session.logout().await;
            return Ok(Vec::new());
        }
        let set = recent.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
        let messages = {
            let stream = session
                .uid_fetch(set, "(UID FLAGS INTERNALDATE BODY.PEEK[])")
                .await
                .map_err(|e| e.to_string())?;
            let fetches: Vec<async_imap::types::Fetch> =
                stream.try_collect().await.map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for f in &fetches {
                let Some(body) = f.body() else { continue };
                let seen = f.flags().any(|fl| matches!(fl, async_imap::types::Flag::Seen));
                let flagged = f.flags().any(|fl| matches!(fl, async_imap::types::Flag::Flagged));
                let internal = f.internal_date().map(|d| d.timestamp() / 60);
                if let Some(m) = parse_fetched(body, folder, seen, flagged, internal, now_min) {
                    out.push(m);
                }
            }
            out
        };
        let _ = session.logout().await;
        Ok(messages)
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
        uids.into_iter().max().ok_or_else(|| "message not found on the server".into())
    }

    /// STORE `\Seen`/`\Flagged` to match the local message's flag state.
    async fn store_flags(
        account: &AccountConfig,
        message: &Message,
        creds: &AuthCreds,
        tls: Arc<rustls::ClientConfig>,
    ) -> Result<(), String> {
        let (mailbox, _) = mailbox_and_search(message.folder);
        let mut session = connect_session(account, creds, tls).await?;
        session.select(mailbox).await.map_err(|e| e.to_string())?;
        let uid = find_uid(&mut session, message).await?;
        let seen_op = if message.read { "+FLAGS (\\Seen)" } else { "-FLAGS (\\Seen)" };
        let flag_op = if message.starred { "+FLAGS (\\Flagged)" } else { "-FLAGS (\\Flagged)" };
        for op in [seen_op, flag_op] {
            let stream = session
                .uid_store(uid.to_string(), op)
                .await
                .map_err(|e| e.to_string())?;
            let _: Vec<_> = stream.try_collect().await.map_err(|e| e.to_string())?;
        }
        let _ = session.logout().await;
        Ok(())
    }

    /// UID MOVE a message to another folder.
    async fn move_message(
        account: &AccountConfig,
        message: &Message,
        to: Folder,
        creds: &AuthCreds,
        tls: Arc<rustls::ClientConfig>,
    ) -> Result<(), String> {
        let (mailbox, _) = mailbox_and_search(message.folder);
        let mut session = connect_session(account, creds, tls).await?;
        session.select(mailbox).await.map_err(|e| e.to_string())?;
        let uid = find_uid(&mut session, message).await?;
        session
            .uid_mv(uid.to_string(), to.imap_name())
            .await
            .map_err(|e| e.to_string())?;
        let _ = session.logout().await;
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
        use lettre::transport::smtp::authentication::{Credentials, Mechanism};
        use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

        let host = account.smtp_host.trim();
        if host.is_empty() {
            return Err("no SMTP host".into());
        }
        let port = if account.smtp_port == 0 { 465 } else { account.smtp_port };
        // 465 → implicit TLS; anything else (587/25) → STARTTLS upgrade.
        let builder = if port == 465 {
            AsyncSmtpTransport::<Tokio1Executor>::relay(host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
        }
        .map_err(|e| classify_conn_failure(&e.to_string()))?;

        let (creds_l, mech) = match creds {
            AuthCreds::Password { user, pass } => {
                (Credentials::new(user.clone(), pass.clone()), Mechanism::Login)
            }
            AuthCreds::XOAuth2 { user, token } => {
                (Credentials::new(user.clone(), token.clone()), Mechanism::Xoauth2)
            }
        };
        let mailer: AsyncSmtpTransport<Tokio1Executor> = builder
            .port(port)
            .credentials(creds_l)
            .authentication(vec![mech])
            .build();

        let envelope = build_envelope(account, draft)?;
        mailer
            .send_raw(&envelope, mime)
            .await
            .map_err(|e| classify_conn_failure(&e.to_string()))?;
        Ok(())
    }

    /// Construct the SMTP envelope (return-path + recipients) from the account
    /// and draft. Recipients that don't parse as addresses are skipped.
    fn build_envelope(account: &AccountConfig, draft: &Draft) -> Result<lettre::address::Envelope, String> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_send_reports_wire_bytes() {
        let (tx, rx) = spawn();
        let account = AccountConfig {
            display_name: "Me".into(),
            email: "me@qos.example".into(),
            ..Default::default()
        };
        let draft = Draft { to: "you@there.example".into(), subject: "Hi".into(), body: "Body".into(), ..Default::default() };
        tx.send(EngineRequest::Send { account, draft: Box::new(draft) }).unwrap();
        let evt = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        match evt {
            EngineEvent::Sent { wire_bytes } => assert!(wire_bytes > 0),
            other => panic!("expected Sent, got {other:?}"),
        }
    }

    #[test]
    fn stub_connect_acks() {
        let (tx, rx) = spawn();
        tx.send(EngineRequest::Connect {
            account: AccountConfig { email: "a@b.example".into(), ..Default::default() },
        })
        .unwrap();
        let evt = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert!(matches!(evt, EngineEvent::Connected { .. }));
    }
}
