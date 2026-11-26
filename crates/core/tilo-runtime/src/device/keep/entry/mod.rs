//! The wait list of an entry that takes people by invitation: the addresses
//! people leave there without one, the mail that confirms an address and
//! carries an invitation to it, and the four routes an entry page asks about
//! them (`<entry>wait-list`, `…/remove`, `…/confirm`, `…/state`).
//!
//! The addresses themselves are the workspace's own, kept beside its record
//! by its host (`tilo_workspace_service::workspaces::wait_list`); this is the entry in
//! front of it: the client address's windows ([`limits`]), the link the entry
//! page opens, and the outbox that keeps a mail until the mail service takes
//! it ([`outbox`], [`mail`]).
//!
//! It was the workspace service's, beside the server; the server answers it itself
//! now, on the same routes and with the same words, so an entry page of either
//! build reads the same answers.

pub(crate) mod limits;
pub(crate) mod mail;
pub(crate) mod outbox;

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::operator::WaitListStatus;
use tilo_server_protocol::workspaces::contact_address;

use tilo_workspace_service::workspaces::{WaitListSecrets, WaitListed, Workspaces};

use limits::{IpHasher, JoinLimits, JoinRates};
use mail::EntryMail;

/// How the entry sends mail: the sender the workspace's entry names, where
/// mail is posted (`TILO_WORKSPACES_RESEND_URL`, Resend's API unless the
/// deployment names another) and the file its Resend API key is read from
/// (`TILO_WORKSPACES_RESEND_KEY_FILE`). The key stays with the deployment:
/// what a workspace records is where its mail goes from, never the credential.
#[derive(Clone, Debug)]
pub struct MailConfig {
    pub from: String,
    pub resend_key_file: std::path::PathBuf,
    pub resend_url: String,
}

/// Where Resend's API is, unless the deployment names another.
const RESEND_URL: &str = "https://api.resend.com/emails";

/// The page an invitation's link opens when this host knows no site of its
/// own.
pub(crate) const ENTRY_WITHOUT_SITE: &str = "https://tilo.sh/";

impl MailConfig {
    /// The mail `entry` names, with the key file and endpoint this deployment
    /// gives its host.
    ///
    /// # Errors
    ///
    /// `TILO_WORKSPACES_RESEND_KEY_FILE` names no key file, or the URL is not
    /// an `https` one (a loopback `http` one for a local stack).
    pub fn for_entry(entry: &tilo_action_policy::EntryMail) -> anyhow::Result<Self> {
        let set = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let Some(key) = set("TILO_WORKSPACES_RESEND_KEY_FILE") else {
            anyhow::bail!(
                "the entry mails from {}, but TILO_WORKSPACES_RESEND_KEY_FILE names no key file",
                entry.from
            );
        };
        let resend_url = set("TILO_WORKSPACES_RESEND_URL").unwrap_or_else(|| RESEND_URL.to_owned());
        anyhow::ensure!(
            resend_url.starts_with("https://")
                || resend_url.starts_with("http://127.0.0.1")
                || resend_url.starts_with("http://localhost"),
            "TILO_WORKSPACES_RESEND_URL is an https URL (a loopback http one for a local stack)"
        );
        Ok(Self {
            from: entry.from.clone(),
            resend_key_file: key.into(),
            resend_url,
        })
    }
}

/// The body of `POST <entry>wait-list`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitListRequest {
    /// The address to be invited at.
    pub email: String,
    /// The language the visitor reads the entry in (a language tag), for
    /// the mails to the address.
    #[serde(default)]
    pub language: String,
}

/// What `POST <entry>wait-list` answers an address it kept.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitListAnswer {
    /// The address as typed, trimmed: what the entry confirms. Whether it
    /// was on the list already is not said, so the entry tells nobody whose
    /// address is.
    pub email: String,
    /// The secret (hex) that takes the entry off again (`…/remove`), for
    /// this visitor alone: the workspace keeps only its SHA-256.
    pub removal: String,
    /// A link that confirms the address was mailed there (at most hourly),
    /// and the address waits to be confirmed; without, it is listed.
    pub mail: bool,
}

/// The body of `POST <entry>wait-list/remove` and `…/state`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnlistRequest {
    /// The removal secret `…/wait-list` handed out, or a removal link
    /// carried (hex).
    pub secret: String,
}

/// The body of `POST <entry>wait-list/confirm`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmRequest {
    /// The secret the confirmation link carried (hex).
    pub token: String,
}

/// Why the entry kept nothing, as its page reads the `code`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The workspace takes anyone at its entry, or keeps no wait list.
    NoWaitList,
    Invalid(&'static str),
    /// A removal secret that is not 64 hex characters.
    NotASecret,
    TooMany {
        retry_after_secs: u64,
    },
    /// The workspace keeps as many as it takes.
    Full,
    Unavailable,
}

impl Refusal {
    /// The `code` the entry page tells its cases apart by.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoWaitList => "no-wait-list",
            Self::Invalid(_) => "invalid-email",
            Self::NotASecret => "invalid-secret",
            Self::TooMany { .. } => "too-many",
            Self::Full => "wait-list-full",
            Self::Unavailable => "unavailable",
        }
    }

    /// What happened, in plain words.
    #[must_use]
    pub fn said(&self) -> &str {
        match self {
            Self::NoWaitList => "this workspace keeps no wait list",
            Self::Invalid(message) => message,
            Self::NotASecret => "not a secret this entry handed out",
            Self::TooMany { .. } => "too many addresses from your network; try again later",
            Self::Full => "the wait list is full for now",
            Self::Unavailable => "the host is restarting; try again in a moment",
        }
    }

    /// How long the answer asks to wait before the next try, when it does.
    #[must_use]
    pub const fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::TooMany { retry_after_secs } => Some(*retry_after_secs),
            _ => None,
        }
    }
}

/// The wait list one workspace's entry keeps: its address windows and the
/// mail it sends from its page (`entry`, a URL ending in `/`).
pub struct WaitListPage {
    /// The workspace the entry lets people into.
    workspace: String,
    /// How many addresses it keeps.
    max: usize,
    /// Where someone asks to be taken off the list, when the operator named
    /// it.
    contact: Option<String>,
    /// How the entry sends mail; `None`, it sends none and lists an address at
    /// once.
    mail: Option<Arc<EntryMail>>,
    hasher: IpHasher,
    /// Addresses left, per address hash.
    waits: std::sync::Mutex<JoinLimits>,
    /// Entries taken off by their secret, per address hash.
    unlists: std::sync::Mutex<JoinLimits>,
    /// Confirmation links opened and stands asked, per address hash.
    lookups: std::sync::Mutex<JoinLimits>,
}

impl std::fmt::Debug for WaitListPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitListPage")
            .field("workspace", &self.workspace)
            .field("max", &self.max)
            .field("contact", &self.contact)
            .field("mail", &self.mails())
            .finish_non_exhaustive()
    }
}

impl WaitListPage {
    /// The wait list `workspace`'s entry keeps, as the entry settles it: the
    /// addresses it keeps, the contact it names, and the mail it sends from
    /// `page` (its outbox under `directory`), naming the workspace `name`.
    /// `mailer` sends the mail in place of the deployment's Resend sender, as
    /// a check supplies one.
    ///
    /// # Errors
    ///
    /// The mail could not be prepared: the outbox's directory, the key's
    /// file or the templates.
    pub fn open(
        workspaces: &Workspaces,
        workspace: &str,
        name: &str,
        directory: &Path,
        page: &str,
        entry: &tilo_action_policy::EntryPolicy,
        mailer: Option<Arc<dyn mail::Mailer>>,
    ) -> anyhow::Result<Option<Self>> {
        if !entry.keeps_requests() {
            return Ok(None);
        }
        let mail = match &entry.mail {
            Some(settings) => {
                let templates = match &settings.templates {
                    Some(dir) => mail::Templates::load(Path::new(dir))?,
                    None => mail::Templates::baked(),
                };
                let mailer = match mailer {
                    Some(mailer) => mailer,
                    None => Arc::new(mail::Resend::new(&MailConfig::for_entry(settings)?)?),
                };
                // The local review mode (`TILO_WORKSPACES_LOCAL`), where the entry's
                // page may be a plain `http` loopback address.
                let local = std::env::var_os("TILO_WORKSPACES_LOCAL").is_some();
                Some(Self::mailing(
                    workspaces,
                    workspace,
                    name,
                    mailer,
                    page,
                    templates,
                    local,
                    // Each workspace's mail waits in its own outbox.
                    &directory
                        .join("wait-lists")
                        .join(workspace)
                        .join("mail-outbox"),
                )?)
            }
            None => None,
        };
        Ok(Some(Self::assembled(
            workspace,
            entry.wait_list_max,
            entry.contact.clone(),
            mail,
        )))
    }

    /// The entry's mail, with the outbox asking the workspace before it sends a
    /// link whose entry may be gone.
    fn mailing(
        workspaces: &Workspaces,
        workspace: &str,
        name: &str,
        mailer: Arc<dyn mail::Mailer>,
        entry: &str,
        templates: mail::Templates,
        local: bool,
        outbox: &Path,
    ) -> anyhow::Result<Arc<EntryMail>> {
        let mail = Arc::new(EntryMail::durable(
            mailer, entry, name, templates, local, outbox,
        )?);
        mail.set_eligibility(Arc::new({
            let workspaces = workspaces.clone();
            let workspace = workspace.to_owned();
            move |removal| {
                let workspaces = workspaces.clone();
                let workspace = workspace.clone();
                Box::pin(async move {
                    let Ok(removal) =
                        <[u8; 32]>::try_from(hex::decode(removal).unwrap_or_default().as_slice())
                    else {
                        return None;
                    };
                    workspaces
                        .wait_list_state(&workspace, removal)
                        .await
                        .ok()
                        .map(|state| state != tilo_server_protocol::operator::WaitListStatus::Gone)
                })
            }
        }));
        mail.start();
        Ok(mail)
    }

    fn assembled(
        workspace: &str,
        max: usize,
        contact: Option<String>,
        mail: Option<Arc<EntryMail>>,
    ) -> Self {
        Self {
            workspace: workspace.to_owned(),
            max,
            contact,
            mail,
            hasher: IpHasher::new(rand::random()),
            waits: std::sync::Mutex::new(JoinLimits::new(JoinRates::WAIT_LIST)),
            unlists: std::sync::Mutex::new(JoinLimits::new(JoinRates::WAIT_LIST_REMOVALS)),
            lookups: std::sync::Mutex::new(JoinLimits::new(JoinRates::WAIT_LIST_LOOKUPS)),
        }
    }

    /// Whether the entry mails the address a link that confirms it.
    #[must_use]
    pub const fn mails(&self) -> bool {
        self.mail.is_some()
    }

    /// The invitation mails members ask for, for whoever sends them: the
    /// host publishes each as the mail a member asked the workspace to send.
    #[must_use]
    pub fn invitations(&self, workspaces: &Workspaces) -> Option<InvitationMail> {
        let mail = Arc::clone(self.mail.as_ref()?);
        let entry = mail.on_page_url().to_owned();
        let asked = workspaces.mail_wait_list(&entry)?;
        Some(InvitationMail { mail, asked })
    }

    fn ip_hash(&self, ip: std::net::IpAddr) -> String {
        self.hasher.hash(ip)
    }

    /// `POST <entry>wait-list`: keep the visitor's address on the workspace's
    /// wait list, to be invited at. Each address counts against its own
    /// per-address window, so one address cannot fill the list.
    ///
    /// # Errors
    ///
    /// The address is not one, the visitor asked too often, the list is
    /// full, or the host could not keep or confirm it.
    pub async fn wait(
        &self,
        workspaces: &Workspaces,
        ip: std::net::IpAddr,
        request: WaitListRequest,
    ) -> Result<WaitListAnswer, Refusal> {
        let email = contact_address(&request.email).map_err(Refusal::Invalid)?;
        let language = tilo_server_protocol::workspaces::mail_language(&request.language);
        let ip_hash = self.ip_hash(ip);
        let now = tilo_clock::unix_now_ms();
        self.waits
            .lock_recover()
            .check_and_record(&ip_hash, now)
            .map_err(|retry_after_secs| Refusal::TooMany { retry_after_secs })?;
        let removal: [u8; 32] = rand::random();
        let confirm: [u8; 32] = rand::random();
        let secrets = WaitListSecrets {
            removal: sha256(&removal),
            confirm: self.mail.as_ref().map(|_| sha256(&confirm)),
        };
        let answered = workspaces
            .wait(&self.workspace, &email, secrets, language, self.max)
            .await;
        match answered {
            Ok(WaitListed::Added { mail } | WaitListed::Already { mail }) => {
                if let (true, Some(entry)) = (mail, &self.mail) {
                    let to = tilo_server_protocol::operator::WaitListMailbox {
                        contact: email.clone(),
                        language: language.to_owned(),
                    };
                    if !entry.send_for(
                        mail::Kind::Confirm,
                        entry.confirm(&to, &hex::encode(confirm), &hex::encode(removal)),
                        &hex::encode(sha256(&removal)),
                    ) {
                        // The workspace kept the entry, but nobody can confirm
                        // it: say so rather than claim a mail went out.
                        return Err(Refusal::Unavailable);
                    }
                }
                if !mail
                    && self
                        .mail
                        .as_ref()
                        .is_some_and(|entry| !entry.confirmation_kept(&email))
                {
                    return Err(Refusal::Unavailable);
                }
                Ok(WaitListAnswer {
                    email,
                    removal: hex::encode(removal),
                    mail: self.mails(),
                })
            }
            Ok(WaitListed::Full) => Err(Refusal::Full),
            Err(error) => {
                tracing::warn!(%error, "an address could not be kept on the wait list");
                // Not kept: the attempt does not use the address's window up.
                self.waits.lock_recover().forget(&ip_hash, now);
                Err(Refusal::Unavailable)
            }
        }
    }

    /// `POST <entry>wait-list/remove`: take the entry the visitor's removal
    /// secret belongs to off the wait list. A secret no entry has is
    /// answered as one that took an entry off, so nobody learns from the
    /// answer what is listed; each network's tries are counted, so nobody
    /// guesses at secrets either. A confirmed address is mailed that it
    /// left.
    ///
    /// # Errors
    ///
    /// Not a secret, or the visitor asked too often.
    pub async fn unlist(
        &self,
        workspaces: &Workspaces,
        ip: std::net::IpAddr,
        request: UnlistRequest,
    ) -> Result<(), Refusal> {
        let secret = Self::secret(&request.secret)?;
        let window = self.open_window(&self.unlists, ip)?;
        let removed = workspaces
            .leave_wait_list_for_operator(&self.workspace, sha256(&secret))
            .await;
        let (told, contact) = match removed {
            Ok(removed) => removed,
            Err(error) => {
                tracing::warn!(%error, "the wait list could not be asked");
                Self::close_window(&self.unlists, &window);
                return Err(Refusal::Unavailable);
            }
        };
        if let Some(entry) = &self.mail {
            // Older operators supplied only the confirmed mailbox; the
            // private digest cancels a pending entry's mail too.
            let contact_sha256 = contact.map(hex::encode).or_else(|| {
                told.as_ref().map(|to| {
                    use sha2::Digest as _;
                    hex::encode(sha2::Sha256::digest(
                        tilo_server_protocol::workspaces::contact_key(&to.contact),
                    ))
                })
            });
            entry.cancel(&hex::encode(sha256(&secret)), contact_sha256.as_deref());
            if let Some(to) = told
                && !entry.send(
                    mail::Kind::Removed,
                    entry.removed(&tilo_server_protocol::operator::WaitListMailbox {
                        contact: to.contact,
                        language: to.language,
                    }),
                )
            {
                // Removal already committed. A notification failure must not
                // disclose whether this token belonged to a confirmed entry.
                tracing::warn!("a removal was not mailed");
            }
        }
        Ok(())
    }

    /// `POST <entry>wait-list/confirm`: confirm the address whose mailed link
    /// carried `token`; whether one had it (`false`: it was taken off, or
    /// not confirmed in time).
    ///
    /// # Errors
    ///
    /// Not a secret, or the visitor asked too often.
    pub async fn confirm(
        &self,
        workspaces: &Workspaces,
        ip: std::net::IpAddr,
        request: ConfirmRequest,
    ) -> Result<bool, Refusal> {
        let token = Self::secret(&request.token)?;
        let window = self.open_window(&self.lookups, ip)?;
        match workspaces
            .confirm_wait_list(&self.workspace, sha256(&token))
            .await
        {
            Ok(confirmed) => Ok(confirmed),
            Err(error) => {
                tracing::warn!(%error, "the wait list could not be asked");
                Self::close_window(&self.lookups, &window);
                Err(Refusal::Unavailable)
            }
        }
    }

    /// `POST <entry>wait-list/state`: where the entry the visitor's removal
    /// secret belongs to stands, so an entry opened again says so.
    ///
    /// # Errors
    ///
    /// Not a secret, or the visitor asked too often.
    pub async fn state(
        &self,
        workspaces: &Workspaces,
        ip: std::net::IpAddr,
        request: UnlistRequest,
    ) -> Result<WaitListStatus, Refusal> {
        let secret = Self::secret(&request.secret)?;
        let window = self.open_window(&self.lookups, ip)?;
        match workspaces
            .wait_list_state(&self.workspace, sha256(&secret))
            .await
        {
            Ok(state) => Ok(state),
            Err(error) => {
                tracing::warn!(%error, "the wait list could not be asked");
                Self::close_window(&self.lookups, &window);
                Err(Refusal::Unavailable)
            }
        }
    }

    /// `text` as a wait-list secret.
    fn secret(text: &str) -> Result<[u8; 32], Refusal> {
        hex_32(text).ok_or(Refusal::NotASecret)
    }

    /// The visitor's window on `limits`, recorded at `now`: its address hash and
    /// the moment, or how long the next ask waits.
    fn open_window(
        &self,
        limits: &std::sync::Mutex<JoinLimits>,
        ip: std::net::IpAddr,
    ) -> Result<(String, u64), Refusal> {
        let ip_hash = self.ip_hash(ip);
        let now = tilo_clock::unix_now_ms();
        limits
            .lock_recover()
            .check_and_record(&ip_hash, now)
            .map_err(|retry_after_secs| Refusal::TooMany { retry_after_secs })?;
        Ok((ip_hash, now))
    }

    /// Give back the ask [`Self::open_window`] recorded, for one that got no
    /// answer.
    fn close_window(limits: &std::sync::Mutex<JoinLimits>, window: &(String, u64)) {
        limits.lock_recover().forget(&window.0, window.1);
    }
}

/// The invitations members ask the entry to mail: kept in the workspace until
/// whoever sends mail takes each, and mailed here.
pub struct InvitationMail {
    mail: Arc<EntryMail>,
    asked: tokio::sync::mpsc::UnboundedReceiver<tilo_workspace_service::workspaces::WaitListMail>,
}

impl InvitationMail {
    /// Take the next invitation a member asked the entry to mail, and mail
    /// it: `true` once the outbox keeps it.
    pub async fn mail_next(&mut self) -> bool {
        let Some(asked) = self.asked.recv().await else {
            std::future::pending::<()>().await;
            return false;
        };
        let to = tilo_server_protocol::operator::WaitListMailbox {
            contact: asked.to.contact,
            language: asked.to.language,
        };
        let kept = if self.mail.on_page(&asked.link) {
            self.mail.send_for(
                mail::Kind::Invite,
                self.mail.invite(
                    &to,
                    &asked.link,
                    &hex::encode(asked.removal),
                    &asked.inviter,
                ),
                &hex::encode(sha256(&asked.removal)),
            )
        } else {
            tracing::warn!("an invitation to mail was not a link on this entry");
            false
        };
        let _ = asked.queued.send(kept);
        kept
    }

    /// Forget the unconfirmed addresses past their time, each start and each
    /// hour (`Workspaces::expire_wait_list`).
    pub async fn expire(&self, workspaces: &Workspaces, workspace: &str) {
        if let Err(error) = workspaces.expire_wait_list(workspace).await {
            tracing::warn!(%error, "the wait list's old addresses could not be forgotten");
        }
    }
}

/// What a request left: the answer's JSON and status, or the refusal's.
#[derive(Debug)]
pub struct Answered {
    pub status: hyper::StatusCode,
    pub body: serde_json::Value,
    pub retry_after_secs: Option<u64>,
}

impl From<Result<serde_json::Value, Refusal>> for Answered {
    fn from(answer: Result<serde_json::Value, Refusal>) -> Self {
        use hyper::StatusCode;
        match answer {
            Ok(body) => Self {
                status: StatusCode::OK,
                body,
                retry_after_secs: None,
            },
            Err(refusal) => Self {
                status: match refusal {
                    Refusal::NoWaitList => StatusCode::NOT_FOUND,
                    Refusal::Invalid(_) | Refusal::NotASecret => StatusCode::BAD_REQUEST,
                    Refusal::TooMany { .. } => StatusCode::TOO_MANY_REQUESTS,
                    Refusal::Full | Refusal::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
                },
                body: serde_json::json!({"error": refusal.said(), "code": refusal.code()}),
                retry_after_secs: refusal.retry_after_secs(),
            },
        }
    }
}

/// The answer to one of the entry's four wait-list routes: `step` is what the
/// route asks (`wait-list`, `wait-list/remove`, `…/confirm`, `…/state`),
/// `body` what it sent.
pub async fn route(
    entry: &WaitListPage,
    workspaces: &Workspaces,
    ip: std::net::IpAddr,
    step: &str,
    body: &[u8],
) -> Answered {
    use tilo_server_protocol::directory::{
        WAIT_LIST, WAIT_LIST_CONFIRM, WAIT_LIST_REMOVE, WAIT_LIST_STATUS,
    };
    match step {
        WAIT_LIST_REMOVE => match serde_json::from_slice::<UnlistRequest>(body) {
            Ok(asked) => entry
                .unlist(workspaces, ip, asked)
                .await
                .map(|()| serde_json::json!({}))
                .into(),
            Err(_) => Answered::invalid("not an address to take off the list"),
        },
        WAIT_LIST_CONFIRM => match serde_json::from_slice::<ConfirmRequest>(body) {
            Ok(asked) => entry
                .confirm(workspaces, ip, asked)
                .await
                .map(|confirmed| serde_json::json!({"confirmed": confirmed}))
                .into(),
            Err(_) => Answered::invalid("not a confirmation"),
        },
        WAIT_LIST_STATUS => match serde_json::from_slice::<UnlistRequest>(body) {
            Ok(asked) => entry
                .state(workspaces, ip, asked)
                .await
                .map(|state| serde_json::json!({"status": state}))
                .into(),
            Err(_) => Answered::invalid("not a secret"),
        },
        WAIT_LIST => match serde_json::from_slice::<WaitListRequest>(body) {
            Ok(asked) => entry
                .wait(workspaces, ip, asked)
                .await
                .map(|answer| serde_json::to_value(answer).unwrap_or_default())
                .into(),
            Err(_) => Answered::invalid("not an address"),
        },
        _ => Answered::invalid("not a wait-list route"),
    }
}

impl Answered {
    fn invalid(message: &str) -> Self {
        Self {
            status: hyper::StatusCode::BAD_REQUEST,
            body: serde_json::json!({"error": message, "code": "invalid-email"}),
            retry_after_secs: None,
        }
    }
}

fn sha256(bytes: &[u8; 32]) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).into()
}

fn hex_32(text: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(text).ok()?;
    bytes.try_into().ok()
}
