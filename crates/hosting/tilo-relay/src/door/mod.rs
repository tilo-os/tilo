//! The wait list of a door that takes people by invitation: the addresses
//! people leave there without one, the mail that confirms an address and
//! carries an invitation to it, and the four routes a door page asks about
//! them (`<door>wait-list`, `…/remove`, `…/confirm`, `…/standing`).
//!
//! The addresses themselves are the workspace's own, kept beside its record
//! by its host (`tilo_room_host::workspaces::wait_list`); this is the door in
//! front of it: the client address's windows ([`limits`]), the link the door
//! page opens, and the outbox that keeps a mail until the mail service takes
//! it ([`outbox`], [`mail`]).
//!
//! It was the room service's, beside the relay; the relay answers it itself
//! now, on the same routes and with the same words, so a door page of either
//! build reads the same answers.

pub(crate) mod limits;
pub(crate) mod mail;
pub(crate) mod outbox;

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::operator::WaitListStanding;
use tilo_relay_protocol::workspaces::contact_address;

use crate::workspaces::{WaitListSecrets, WaitListed, Workspaces};

use limits::{IpHasher, JoinLimits, JoinRates};
use mail::DoorMail;

/// How the door sends mail: the sender, its Resend API key's file and where
/// mail is posted (`TILO_ROOMS_MAIL_FROM`, `TILO_ROOMS_RESEND_KEY_FILE`,
/// `TILO_ROOMS_RESEND_URL`), and the directory the words are read from
/// (`TILO_ROOMS_MAIL_TEMPLATES`), or `None` for the ones built in.
#[derive(Clone, Debug)]
pub struct MailConfig {
    pub from: String,
    pub resend_key_file: std::path::PathBuf,
    pub resend_url: String,
    pub templates: Option<std::path::PathBuf>,
}

/// Where Resend's API is, unless the deployment names another.
const RESEND_URL: &str = "https://api.resend.com/emails";

impl MailConfig {
    /// The mail `TILO_ROOMS_MAIL_FROM` with `TILO_ROOMS_RESEND_KEY_FILE`
    /// describes, if the deployment set them: both or neither, as the room
    /// service's door required.
    ///
    /// # Errors
    ///
    /// Only one of the two is set, or the URL is not an `https` one.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let set = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let (from, key) = match (
            set("TILO_ROOMS_MAIL_FROM"),
            set("TILO_ROOMS_RESEND_KEY_FILE"),
        ) {
            (Some(from), Some(key)) => (from, key),
            (None, None) => return Ok(None),
            _ => anyhow::bail!(
                "TILO_ROOMS_MAIL_FROM and TILO_ROOMS_RESEND_KEY_FILE are set together, or neither"
            ),
        };
        let resend_url = set("TILO_ROOMS_RESEND_URL").unwrap_or_else(|| RESEND_URL.to_owned());
        anyhow::ensure!(
            resend_url.starts_with("https://")
                || resend_url.starts_with("http://127.0.0.1")
                || resend_url.starts_with("http://localhost"),
            "TILO_ROOMS_RESEND_URL is an https URL (a loopback http one for a local stack)"
        );
        Ok(Some(Self {
            from,
            resend_key_file: key.into(),
            resend_url,
            templates: set("TILO_ROOMS_MAIL_TEMPLATES").map(std::path::PathBuf::from),
        }))
    }
}

/// How the door's wait list is configured: what whoever runs the relay said,
/// from `--wait-list-max` and `--wait-list-mail` and the mail settings'
/// environment.
#[derive(Clone, Debug, Default)]
pub struct WaitListSettings {
    /// How many addresses the workspace keeps.
    pub max: usize,
    /// Where someone asks to be taken off the list (`TILO_ROOMS_CONTACT`).
    pub contact: Option<String>,
    /// The door's page, which its mails' links open.
    pub entry: Option<String>,
    /// How the door sends mail, when the deployment configured it.
    pub mail: Option<MailConfig>,
    /// The local review mode (`TILO_ROOMS_LOCAL`), where the door's page may
    /// be a plain `http` loopback address.
    pub local: bool,
}

impl WaitListSettings {
    /// What the environment says, with `max`, the door's `entry` and the local
    /// mode from the deployment's flags.
    ///
    /// # Errors
    ///
    /// The mail's variables are set half-way, or as no URL, as
    /// [`MailConfig::from_env`] refuses.
    pub fn from_env(max: usize, entry: Option<String>, local: bool) -> anyhow::Result<Self> {
        Ok(Self {
            max,
            contact: std::env::var("TILO_ROOMS_CONTACT")
                .ok()
                .map(|contact| contact.trim().to_owned())
                .filter(|contact| !contact.is_empty()),
            entry,
            mail: MailConfig::from_env()?,
            local,
        })
    }
}

/// The body of `POST <door>wait-list`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WaitListRequest {
    /// The address to be invited at.
    pub email: String,
    /// The language the visitor reads the door in (a language tag), for
    /// the mails to the address.
    #[serde(default)]
    pub language: String,
}

/// What `POST <door>wait-list` answers an address it kept.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitListAnswer {
    /// The address as typed, trimmed: what the door confirms. Whether it
    /// was on the list already is not said, so the door tells nobody whose
    /// address is.
    pub email: String,
    /// The secret (hex) that takes the entry off again (`…/remove`), for
    /// this visitor alone: the workspace keeps only its SHA-256.
    pub removal: String,
    /// A link that confirms the address was mailed there (at most hourly),
    /// and the address waits to be confirmed; without, it is listed.
    pub mail: bool,
}

/// The body of `POST <door>wait-list/remove` and `…/standing`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UnlistRequest {
    /// The removal secret `…/wait-list` handed out, or a removal link
    /// carried (hex).
    pub secret: String,
}

/// The body of `POST <door>wait-list/confirm`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmRequest {
    /// The secret the confirmation link carried (hex).
    pub token: String,
}

/// Why the door kept nothing, as its page reads the `code`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The workspace takes anyone at its door, or keeps no wait list.
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
    /// The `code` the door page tells its cases apart by.
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
            Self::NotASecret => "not a secret this door handed out",
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

/// The wait list one workspace's door keeps: its address windows and the
/// mail it sends from its page (`door`, a URL ending in `/`).
pub struct WaitListDoor {
    /// The workspace the door lets people into.
    workspace: String,
    /// How many addresses it keeps.
    max: usize,
    /// Where someone asks to be taken off the list, when the operator named
    /// it.
    contact: Option<String>,
    /// How the door sends mail; `None`, it sends none and lists an address at
    /// once.
    mail: Option<Arc<DoorMail>>,
    hasher: IpHasher,
    /// Addresses left, per address hash.
    waits: std::sync::Mutex<JoinLimits>,
    /// Entries taken off by their secret, per address hash.
    unlists: std::sync::Mutex<JoinLimits>,
    /// Confirmation links opened and stands asked, per address hash.
    lookups: std::sync::Mutex<JoinLimits>,
}

impl std::fmt::Debug for WaitListDoor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WaitListDoor")
            .field("workspace", &self.workspace)
            .field("max", &self.max)
            .field("contact", &self.contact)
            .field("mail", &self.mails())
            .finish_non_exhaustive()
    }
}

impl WaitListDoor {
    /// The door's wait list, when the workspace takes people by invitation
    /// and keeps one (`settings.max` above zero): the addresses it keeps, the
    /// contact the operator named and the mail it sends from `settings.entry`
    /// (its outbox under `directory`).
    ///
    /// # Errors
    ///
    /// The mail could not be prepared: the outbox's directory, the key's
    /// file or the templates.
    pub fn open(
        workspaces: &Workspaces,
        workspace: &str,
        directory: &Path,
        settings: &WaitListSettings,
    ) -> anyhow::Result<Option<Self>> {
        if settings.max == 0 {
            return Ok(None);
        }
        let mail = match (&settings.entry, &settings.mail) {
            (Some(door), Some(config)) => {
                let templates = match &config.templates {
                    Some(dir) => mail::Templates::load(dir)?,
                    None => mail::Templates::baked(),
                };
                let mailer = Arc::new(mail::Resend::new(config)?);
                Some(Self::mailing(
                    workspaces,
                    workspace,
                    mailer,
                    door,
                    templates,
                    settings.local,
                    &directory.join("mail-outbox"),
                )?)
            }
            (None, Some(_)) => {
                tracing::warn!(
                    "TILO_ROOMS_MAIL_FROM names a sender, but the door has no page to mail \
                     invitations on: the wait list lists addresses at once"
                );
                None
            }
            (_, None) => None,
        };
        Ok(Some(Self::assembled(
            workspace,
            settings.max,
            settings.contact.clone(),
            mail,
        )))
    }

    /// The door's mail, with the outbox asking the workspace before it sends a
    /// link whose entry may be gone.
    fn mailing(
        workspaces: &Workspaces,
        workspace: &str,
        mailer: Arc<dyn mail::Mailer>,
        door: &str,
        templates: mail::Templates,
        local: bool,
        outbox: &Path,
    ) -> anyhow::Result<Arc<DoorMail>> {
        let mail = Arc::new(DoorMail::durable(
            mailer, door, workspace, templates, local, outbox,
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
                        .wait_list_standing(&workspace, removal)
                        .await
                        .ok()
                        .map(|standing| {
                            standing != tilo_relay_protocol::operator::WaitListStanding::Gone
                        })
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
        mail: Option<Arc<DoorMail>>,
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

    /// The door's wait list as a check stands it up: `mailer` takes the mail,
    /// kept only while the check runs.
    #[cfg(test)]
    pub fn with_mailer(
        _workspaces: &Workspaces,
        workspace: &str,
        max: usize,
        contact: Option<&str>,
        mailer: Arc<dyn mail::Mailer>,
        door: &str,
    ) -> Self {
        let mail = Arc::new(DoorMail::new(mailer, door, workspace));
        // Every entry a check leaves stands until it takes it off, so the
        // outbox sends each mail at once rather than asking first.
        mail.set_eligibility(Arc::new(|_| Box::pin(async { Some(true) })));
        Self::assembled(workspace, max, contact.map(ToOwned::to_owned), Some(mail))
    }

    /// The workspace the door lets people into.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// How many addresses the workspace keeps.
    #[must_use]
    pub const fn max(&self) -> usize {
        self.max
    }

    /// Whether the door mails the address a link that confirms it.
    #[must_use]
    pub const fn mails(&self) -> bool {
        self.mail.is_some()
    }

    /// Where someone asks to be taken off the list, when the operator named it.
    #[must_use]
    pub fn contact(&self) -> Option<&str> {
        self.contact.as_deref()
    }

    /// What the door says about its wait list beside a refusal
    /// ([`tilo_relay_protocol::directory::DoorRefused::wait_list`]).
    #[must_use]
    pub fn offered(&self) -> tilo_relay_protocol::directory::DoorWaitList {
        tilo_relay_protocol::directory::DoorWaitList {
            contact: self.contact.clone(),
            mail: self.mails(),
        }
    }

    /// The invitation mails members ask for, for whoever sends them: the
    /// host publishes each as the mail a member asked the workspace to send.
    #[must_use]
    pub fn invitations(&self, workspaces: &Workspaces) -> Option<InvitationMail> {
        let mail = Arc::clone(self.mail.as_ref()?);
        let door = mail.on_door_url().to_owned();
        let asked = workspaces.mail_wait_list(&door)?;
        Some(InvitationMail {
            mail,
            asked: std::sync::Mutex::new(asked),
        })
    }

    fn ip_hash(&self, ip: std::net::IpAddr) -> String {
        self.hasher.hash(ip)
    }

    /// `POST <door>wait-list`: keep the visitor's address on the workspace's
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
        let language = tilo_relay_protocol::workspaces::mail_language(&request.language);
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
                if let (true, Some(door)) = (mail, &self.mail) {
                    let to = tilo_relay_protocol::operator::WaitListMailbox {
                        contact: email.clone(),
                        language: language.to_owned(),
                    };
                    if !door.send_for(
                        mail::Kind::Confirm,
                        door.confirm(&to, &hex::encode(confirm), &hex::encode(removal)),
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
                        .is_some_and(|door| !door.confirmation_kept(&email))
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

    /// `POST <door>wait-list/remove`: take the entry the visitor's removal
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
        let secret = self.secret(&request.secret)?;
        let window = self.open_window(&self.unlists, ip)?;
        let removed = workspaces
            .leave_wait_list_for_operator(&self.workspace, sha256(&secret))
            .await;
        let (told, contact) = match removed {
            Ok(removed) => removed,
            Err(error) => {
                tracing::warn!(%error, "the wait list could not be asked");
                self.close_window(&self.unlists, &window);
                return Err(Refusal::Unavailable);
            }
        };
        if let Some(door) = &self.mail {
            // Older operators supplied only the confirmed mailbox; the
            // private digest cancels a pending entry's mail too.
            let contact_sha256 = contact.map(hex::encode).or_else(|| {
                told.as_ref().map(|to| {
                    use sha2::Digest as _;
                    hex::encode(sha2::Sha256::digest(
                        tilo_relay_protocol::workspaces::contact_key(&to.contact),
                    ))
                })
            });
            door.cancel(&hex::encode(sha256(&secret)), contact_sha256.as_deref());
            if let Some(to) = told
                && !door.send(
                    mail::Kind::Removed,
                    door.removed(&tilo_relay_protocol::operator::WaitListMailbox {
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

    /// `POST <door>wait-list/confirm`: confirm the address whose mailed link
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
        let token = self.secret(&request.token)?;
        let window = self.open_window(&self.lookups, ip)?;
        match workspaces
            .confirm_wait_list(&self.workspace, sha256(&token))
            .await
        {
            Ok(confirmed) => Ok(confirmed),
            Err(error) => {
                tracing::warn!(%error, "the wait list could not be asked");
                self.close_window(&self.lookups, &window);
                Err(Refusal::Unavailable)
            }
        }
    }

    /// `POST <door>wait-list/standing`: where the entry the visitor's removal
    /// secret belongs to stands, so a door opened again says so.
    ///
    /// # Errors
    ///
    /// Not a secret, or the visitor asked too often.
    pub async fn standing(
        &self,
        workspaces: &Workspaces,
        ip: std::net::IpAddr,
        request: UnlistRequest,
    ) -> Result<WaitListStanding, Refusal> {
        let secret = self.secret(&request.secret)?;
        let window = self.open_window(&self.lookups, ip)?;
        match workspaces
            .wait_list_standing(&self.workspace, sha256(&secret))
            .await
        {
            Ok(standing) => Ok(standing),
            Err(error) => {
                tracing::warn!(%error, "the wait list could not be asked");
                self.close_window(&self.lookups, &window);
                Err(Refusal::Unavailable)
            }
        }
    }

    /// `text` as a wait-list secret.
    fn secret(&self, text: &str) -> Result<[u8; 32], Refusal> {
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
    fn close_window(&self, limits: &std::sync::Mutex<JoinLimits>, window: &(String, u64)) {
        limits.lock_recover().forget(&window.0, window.1);
    }
}

/// The invitations members ask the door to mail: kept in the workspace until
/// whoever sends mail takes each, and mailed here.
pub struct InvitationMail {
    mail: Arc<DoorMail>,
    asked: std::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<crate::workspaces::WaitListMail>>,
}

impl InvitationMail {
    /// Take the next invitation a member asked the door to mail, and mail
    /// it: `true` once the outbox keeps it.
    pub async fn mail_next(&self) -> bool {
        let asked = match self.asked.lock_recover().recv().await {
            Some(asked) => asked,
            None => {
                std::future::pending::<()>().await;
                return false;
            }
        };
        let to = tilo_relay_protocol::operator::WaitListMailbox {
            contact: asked.to.contact,
            language: asked.to.language,
        };
        let kept = if self.mail.on_door(&asked.link) {
            self.mail.send_for(
                mail::Kind::Invite,
                self.mail
                    .invite(&to, &asked.link, &hex::encode(asked.removal)),
                &hex::encode(sha256(&asked.removal)),
            )
        } else {
            tracing::warn!("an invitation to mail was not a link on this door");
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

/// The answer to one of the door's four wait-list routes: `step` is what the
/// route asks (`wait-list`, `wait-list/remove`, `…/confirm`, `…/standing`),
/// `body` what it sent.
pub async fn route(
    door: &WaitListDoor,
    workspaces: &Workspaces,
    ip: std::net::IpAddr,
    step: &str,
    body: &[u8],
) -> Answered {
    use tilo_relay_protocol::directory::{
        WAIT_LIST, WAIT_LIST_CONFIRM, WAIT_LIST_REMOVE, WAIT_LIST_STANDING,
    };
    match step {
        WAIT_LIST_REMOVE => match serde_json::from_slice::<UnlistRequest>(body) {
            Ok(asked) => door
                .unlist(workspaces, ip, asked)
                .await
                .map(|()| serde_json::json!({}))
                .into(),
            Err(_) => Answered::invalid("not an address to take off the list"),
        },
        WAIT_LIST_CONFIRM => match serde_json::from_slice::<ConfirmRequest>(body) {
            Ok(asked) => door
                .confirm(workspaces, ip, asked)
                .await
                .map(|confirmed| serde_json::json!({"confirmed": confirmed}))
                .into(),
            Err(_) => Answered::invalid("not a confirmation"),
        },
        WAIT_LIST_STANDING => match serde_json::from_slice::<UnlistRequest>(body) {
            Ok(asked) => door
                .standing(workspaces, ip, asked)
                .await
                .map(|standing| serde_json::json!({"standing": standing}))
                .into(),
            Err(_) => Answered::invalid("not a secret"),
        },
        WAIT_LIST => match serde_json::from_slice::<WaitListRequest>(body) {
            Ok(asked) => door
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
