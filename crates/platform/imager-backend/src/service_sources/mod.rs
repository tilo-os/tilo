//! Outline, Linear and mail accounts as sources of the migration, beside an old computer.
//!
//! A service source is reached with a key or password the person types once. [`connect`] checks
//! it against the service and keeps it in the account vault, never in a record, a journal or an
//! action. [`review`] lists what the source holds, grouped the way the service groups it
//! (Outline's collections, Linear's teams, the mail account itself), and the person chooses the
//! groups that come over. [`run`] brings exactly the reviewed items of the chosen groups over,
//! recording each one in the operation's [`ServiceJournal`] the moment its app has it, so an
//! interrupted run resumes where it stopped and a second run lands nothing twice.
//!
//! What comes over lands in the app that owns it, as the Thunderbird address books and
//! calendars do: Outline documents become Notes pages and issues from Linear become Tasks issues
//! through each app's import inbox, which the app folds in through its own actions when it
//! opens. A mail account is published into the account store Mail reads, with its password in
//! the vault.

mod http;
mod journal;
pub mod landing;
mod linear;
mod mail;
mod outline;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use accounts::{Account, AccountId, AccountStore, SecretVault};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub use journal::{JournalState, ServiceJournal, valid_id};
pub use landing::{InboxLanding, IncomingComment, IncomingIssue, IncomingPage, Landing};

/// The account type a reviewed Outline key is kept under.
pub const OUTLINE_TYPE: &str = "outline";
/// The account type a reviewed Linear key is kept under.
pub const LINEAR_TYPE: &str = "linear";
/// Linear's own API, used when the person names no other address.
pub const LINEAR_API: &str = "https://api.linear.app";

/// The most items one review lists; a larger source is refused rather than half shown.
pub const MAX_REVIEW_ITEMS: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceKind {
    Outline,
    Linear,
    Mail,
}

impl ServiceKind {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Outline => "outline",
            Self::Linear => "linear",
            Self::Mail => "mail",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "outline" => Some(Self::Outline),
            "linear" => Some(Self::Linear),
            "mail" => Some(Self::Mail),
            _ => None,
        }
    }

    #[must_use]
    pub fn of_account(account: &Account) -> Option<Self> {
        match account.type_id.as_str() {
            OUTLINE_TYPE => Some(Self::Outline),
            LINEAR_TYPE => Some(Self::Linear),
            accounts::email::TYPE_ID if account.config.get("service_source").is_some() => {
                Some(Self::Mail)
            }
            _ => None,
        }
    }
}

/// Why a service source refused. Never carries a key, a password or a response body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "reason", content = "detail")]
pub enum ServiceError {
    /// The address, login or key is not one this source takes.
    InvalidAddress,
    /// The service refused the key or password.
    Refused,
    /// The service could not be reached or answered something unexpected.
    Unavailable(String),
    /// The vault or account store could not be read or written.
    Storage,
    /// No such connected source.
    Missing,
    /// The source holds more than one review lists.
    TooLarge,
    /// The mail address already has an account here that this source did not make.
    AlreadyThere,
    /// The person stopped the run.
    Cancelled,
    /// The journal could not be kept.
    Journal(String),
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAddress => formatter.write_str("the address, login or key is not valid"),
            Self::Refused => formatter.write_str("the service refused the key or password"),
            Self::Unavailable(detail) => write!(formatter, "the service is unavailable: {detail}"),
            Self::Storage => formatter.write_str("the account vault is unavailable"),
            Self::Missing => formatter.write_str("that source is not connected"),
            Self::TooLarge => formatter.write_str("the source holds more than one review lists"),
            Self::AlreadyThere => formatter.write_str("that account is already set up here"),
            Self::Cancelled => formatter.write_str("stopped"),
            Self::Journal(detail) => write!(formatter, "the journal could not be kept: {detail}"),
        }
    }
}

impl std::error::Error for ServiceError {}

/// What the person typed to reach a source. The secret is held only in private memory until
/// it is sealed into the vault.
pub struct ServiceAddress {
    pub kind: ServiceKind,
    /// Outline's address; Linear's API address (its own when empty); a mail account's IMAP
    /// host (guessed from the address when empty).
    pub server: String,
    /// The mail address; empty for Outline and Linear.
    pub login: String,
    /// Outline's API key, Linear's API key, or the mail account's password.
    pub secret: Zeroizing<String>,
}

/// One thing the review lists: a document, an issue, a mail account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewItem {
    /// The source's own stable id for it.
    pub key: String,
    pub title: String,
    /// How the item reads beside its title: a document's place, an issue's identifier.
    pub detail: String,
}

/// A group the person chooses as a whole: an Outline collection, a Linear team, a mail account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewGroup {
    pub key: String,
    pub title: String,
    pub items: Vec<ReviewItem>,
    pub chosen: bool,
}

/// What a source holds, as the person reviews it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceReview {
    pub kind: ServiceKind,
    /// The vault account the source is reached with.
    pub account: String,
    /// How the source names itself: the Outline team, the Linear workspace, the mail address.
    pub title: String,
    pub groups: Vec<ReviewGroup>,
}

impl ServiceReview {
    #[must_use]
    pub fn item_count(&self) -> usize {
        self.groups.iter().map(|group| group.items.len()).sum()
    }

    #[must_use]
    pub fn chosen_count(&self) -> usize {
        self.groups
            .iter()
            .filter(|group| group.chosen)
            .map(|group| group.items.len())
            .sum()
    }

    /// Choose or leave out the group `key`.
    ///
    /// # Errors
    /// The review has no such group.
    pub fn choose(&mut self, key: &str, chosen: bool) -> Result<(), ServiceError> {
        let group = self
            .groups
            .iter_mut()
            .find(|group| group.key == key)
            .ok_or(ServiceError::Missing)?;
        group.chosen = chosen;
        Ok(())
    }

    fn chosen_keys(&self) -> std::collections::BTreeSet<&str> {
        self.groups
            .iter()
            .filter(|group| group.chosen)
            .flat_map(|group| group.items.iter().map(|item| item.key.as_str()))
            .collect()
    }
}

/// How a run went: what its apps now have, what was already there, and what could not come.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceReport {
    pub landed: usize,
    pub already_there: usize,
    /// Items the source holds now that the review did not list; a new review brings them.
    pub not_reviewed: usize,
    /// Reviewed items the source no longer holds.
    pub gone: Vec<String>,
}

/// Check what the person typed against the service and keep the key or password in the vault
/// under `config_root` (Tilo's config directory of the destination home). Answers the account
/// the source is reached with from now on. Connecting the same source again replaces its key.
///
/// # Errors
/// The address is invalid, the service refused the key, or the vault could not be written.
pub fn connect(config_root: &Path, address: ServiceAddress) -> Result<AccountId, ServiceError> {
    match address.kind {
        ServiceKind::Outline => {
            let server = http::base(&address.server)?;
            let title = outline::check(&server, &address.secret)?;
            persist(
                config_root,
                OUTLINE_TYPE,
                &server,
                &title,
                serde_json::json!({ "server_url": server }),
                &address.secret,
            )
        }
        ServiceKind::Linear => {
            let server = if address.server.trim().is_empty() {
                LINEAR_API.to_owned()
            } else {
                http::base(&address.server)?
            };
            let title = linear::check(&server, &address.secret)?;
            persist(
                config_root,
                LINEAR_TYPE,
                &server,
                &title,
                serde_json::json!({ "server_url": server }),
                &address.secret,
            )
        }
        ServiceKind::Mail => mail::connect(config_root, &address),
    }
}

/// Tilo's config directory of the home this process runs in: where its sources' keys are kept.
#[must_use]
pub fn config_root() -> Option<std::path::PathBuf> {
    accounts::config_root()
}

/// The connected service sources under `config_root`.
#[must_use]
pub fn connected(config_root: &Path) -> Vec<(AccountId, ServiceKind, String)> {
    AccountStore::load_in(config_root)
        .all()
        .iter()
        .filter_map(|account| {
            ServiceKind::of_account(account)
                .map(|kind| (account.id.clone(), kind, account.display_name.clone()))
        })
        .collect()
}

fn persist(
    config_root: &Path,
    type_id: &str,
    server: &str,
    title: &str,
    config: serde_json::Value,
    secret: &str,
) -> Result<AccountId, ServiceError> {
    let id = AccountId::new(format!("{type_id}-{}", short_digest(server)));
    let mut account = Account::new(id, type_id);
    account.display_name = title.chars().take(256).collect();
    account.config = config;
    crate::account_store::persist(
        config_root,
        || Ok(AccountStore::load_in(config_root)),
        account,
        accounts::Secret::password(secret.to_owned()),
    )
    .map_err(|_| ServiceError::Storage)
}

/// The account, its server and its key as the vault holds them.
fn reach(
    config_root: &Path,
    account: &str,
) -> Result<(Account, String, Zeroizing<String>), ServiceError> {
    let id = AccountId::new(account);
    let store = AccountStore::load_in(config_root);
    let account = store.get(&id).cloned().ok_or(ServiceError::Missing)?;
    let vault = SecretVault::open_read_only_in(config_root);
    if vault.recovery_error().is_some() {
        return Err(ServiceError::Storage);
    }
    let secret = crate::account_store::PrivateSecret(vault.get(&id).ok_or(ServiceError::Storage)?);
    let key = Zeroizing::new(secret.0.password.clone());
    let server = account
        .config
        .get("server_url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok((account, server, key))
}

/// What the connected source `account` holds now, every group chosen until the person says
/// otherwise.
///
/// # Errors
/// The source is not connected, refused its key or could not be read.
pub fn review(config_root: &Path, account: &str) -> Result<ServiceReview, ServiceError> {
    let (stored, server, key) = reach(config_root, account)?;
    let kind = ServiceKind::of_account(&stored).ok_or(ServiceError::Missing)?;
    let (title, groups) = match kind {
        ServiceKind::Outline => (stored.display_name, outline::review(&server, &key)?),
        ServiceKind::Linear => (stored.display_name, linear::review(&server, &key)?),
        ServiceKind::Mail => mail::review(&stored),
    };
    let review = ServiceReview {
        kind,
        account: account.to_owned(),
        title,
        groups,
    };
    if review.item_count() > MAX_REVIEW_ITEMS {
        return Err(ServiceError::TooLarge);
    }
    Ok(review)
}

/// Bring the reviewed, chosen items of `journal` over through `landing`, saving the journal
/// with `save` after every item its app has. Items the journal already records are not landed
/// again, so a run after an interruption, or after it finished, lands only what is missing.
///
/// # Errors
/// The source could not be read, the run was cancelled, or the journal could not be saved; the
/// journal keeps what landed before.
pub fn run(
    config_root: &Path,
    journal: &mut ServiceJournal,
    landing: &mut dyn Landing,
    save: &mut dyn FnMut(&ServiceJournal) -> Result<(), String>,
    cancel: &AtomicBool,
) -> Result<ServiceReport, ServiceError> {
    journal.state = JournalState::Running;
    save(journal).map_err(ServiceError::Journal)?;
    let result = land(config_root, journal, landing, save, cancel);
    journal.state = match &result {
        Ok(_) => JournalState::Complete,
        Err(ServiceError::Cancelled) => JournalState::Interrupted,
        Err(_) => JournalState::Failed,
    };
    journal.failure = result.as_ref().err().map(ToString::to_string);
    if let Ok(report) = &result {
        journal.report = Some(report.clone());
    }
    save(journal).map_err(ServiceError::Journal)?;
    result
}

fn land(
    config_root: &Path,
    journal: &mut ServiceJournal,
    landing: &mut dyn Landing,
    save: &mut dyn FnMut(&ServiceJournal) -> Result<(), String>,
    cancel: &AtomicBool,
) -> Result<ServiceReport, ServiceError> {
    let chosen: std::collections::BTreeSet<String> = journal
        .review
        .chosen_keys()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut report = ServiceReport::default();
    let mut landed_one = |key: &str,
                          journal: &mut ServiceJournal,
                          deliver: &mut dyn FnMut() -> std::io::Result<()>|
     -> Result<(), ServiceError> {
        if cancel.load(Ordering::Relaxed) {
            return Err(ServiceError::Cancelled);
        }
        if journal.landed.contains(key) {
            report.already_there += 1;
            return Ok(());
        }
        deliver().map_err(|error| ServiceError::Unavailable(error.to_string()))?;
        journal.landed.insert(key.to_owned());
        save(journal).map_err(ServiceError::Journal)?;
        report.landed += 1;
        Ok(())
    };
    match journal.review.kind {
        ServiceKind::Outline => {
            let (_, server, key) = reach(config_root, &journal.review.account)?;
            let pages = outline::pages(&server, &key, &journal.review, &chosen, cancel)?;
            for page in pages.pages {
                landed_one(&page.key.clone(), journal, &mut || landing.page(&page))?;
            }
            report.not_reviewed = pages.not_reviewed;
            report.gone = pages.gone;
        }
        ServiceKind::Linear => {
            let (_, server, key) = reach(config_root, &journal.review.account)?;
            let issues = linear::issues(&server, &key, &chosen, cancel)?;
            for issue in issues.issues {
                landed_one(&issue.key.clone(), journal, &mut || landing.issue(&issue))?;
            }
            report.not_reviewed = issues.not_reviewed;
            report.gone = issues.gone;
        }
        ServiceKind::Mail => {
            let account = journal.review.account.clone();
            if chosen.contains(&account) {
                landed_one(&account, journal, &mut || {
                    mail::publish(config_root, &account).map_err(std::io::Error::other)
                })?;
            }
        }
    }
    Ok(report)
}

fn short_digest(text: &str) -> String {
    let hex = blake3::hash(text.as_bytes()).to_hex();
    hex.get(..16).unwrap_or_default().to_owned()
}

#[cfg(test)]
mod tests;
