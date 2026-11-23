//! Outline, Linear, Forgejo and mail accounts as sources of the migration, beside an old computer.
//!
//! A service source is reached with a key or password the person types once. [`connect`] checks
//! it against the service and keeps it in the account vault, never in a record, a journal or an
//! action. [`review`] lists what the source holds, grouped the way the service groups it
//! (Outline's collections, Linear's teams, Forgejo's repositories, the mail account itself), and
//! the person chooses the groups that come over. [`run`] brings exactly the reviewed items of the chosen groups over,
//! recording each one in the operation's [`ServiceJournal`] the moment its app has it, so an
//! interrupted import resumes where it stopped and a second import lands nothing twice.
//!
//! What comes over lands in the app that owns it, as the Thunderbird address books and
//! calendars do: Outline documents become Notes pages and issues from Linear and Forgejo become
//! Tasks issues, through the [`Landing`] the caller gives. A mail account is published into the account store Mail reads, with its password in
//! the vault.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "page offsets and item counts stay under MAX_REVIEW_ITEMS"
)]
#![allow(
    clippy::format_push_string,
    reason = "a page's Markdown is built from the source's own small pieces"
)]
#![allow(
    clippy::indexing_slicing,
    reason = "the fixed keys of this service's own JSON request and answer"
)]
#![allow(
    clippy::struct_field_names,
    reason = "the result field is named for the thing it holds: pages, issues"
)]

pub mod forgejo;
mod http;
mod journal;
pub mod landing;
mod linear;
mod mail;
mod outline;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use accounts::person::AccountList;
use accounts::{Account, AccountId, SecretVault};
use tilo_resource::Invoke;

pub use mail::publish as publish_mail;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub use http::IN_FLIGHT;
pub use journal::{JournalState, ServiceJournal, valid_id};
pub use landing::{
    Delivered, IncomingAuthor, IncomingComment, IncomingGroup, IncomingIssue,
    IncomingPage, IncomingRelation, Landing, RefusedItem,
};

/// The account type a reviewed Outline key is kept under.
pub const OUTLINE_TYPE: &str = "outline";
/// The account type a reviewed Linear key is kept under.
pub const LINEAR_TYPE: &str = "linear";
/// The account type a reviewed Forgejo token is kept under.
pub const FORGEJO_TYPE: &str = "forgejo";
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
    Forgejo,
}

impl ServiceKind {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Outline => "outline",
            Self::Linear => "linear",
            Self::Forgejo => "forgejo",
            Self::Mail => "mail",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "outline" => Some(Self::Outline),
            "linear" => Some(Self::Linear),
            "forgejo" => Some(Self::Forgejo),
            "mail" => Some(Self::Mail),
            _ => None,
        }
    }

    #[must_use]
    pub fn of_account(account: &Account) -> Option<Self> {
        match account.type_id.as_str() {
            OUTLINE_TYPE => Some(Self::Outline),
            LINEAR_TYPE => Some(Self::Linear),
            FORGEJO_TYPE => Some(Self::Forgejo),
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
    /// The person stopped the import.
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

impl From<crate::vault::VaultError> for ServiceError {
    fn from(_: crate::vault::VaultError) -> Self {
        Self::Storage
    }
}

/// What the person typed to reach a source. The secret is held only in private memory until
/// it is sealed into the vault.
pub struct ServiceAddress {
    pub kind: ServiceKind,
    /// Outline's address; Linear's API address (its own when empty); the Forgejo repository,
    /// owner or forge whose issues are reviewed; a mail account's IMAP host (guessed from the
    /// address when empty).
    pub server: String,
    /// The mail address; empty for the other sources.
    pub login: String,
    /// Outline's API key, Linear's API key, a Forgejo access token, or the mail account's
    /// password.
    pub secret: Zeroizing<String>,
}

/// One thing the review lists: a document, an issue, a mail account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewItem {
    /// The source's own stable id for it.
    pub key: String,
    pub title: String,
    /// What reads beside its title, each fact on its own: a document's collection and the
    /// document it sits under, an issue's identifier and state.
    pub facts: Vec<String>,
}

/// A group the person chooses as a whole: an Outline collection, a Linear team, a Forgejo
/// repository, a mail account.
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

/// How an import went: what its apps now have, what was already there, and what could not come.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceReport {
    pub landed: usize,
    pub already_there: usize,
    /// Items the source holds now that the review did not list; a new review brings them.
    pub not_reviewed: usize,
    /// Reviewed items the source no longer holds.
    pub gone: Vec<String>,
    /// Items the destination refused, each with why; the next import tries them again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<RefusedItem>,
}

/// Check what the person typed against the service and keep the key or password in the vault
/// under `config_root` (Tilo's config directory of the destination home). Answers the account
/// the source is reached with from now on. Connecting the same source again replaces its key.
///
/// # Errors
/// The address is invalid, the service refused the key, or the vault could not be written.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the address owns the secret the person typed once"
)]
pub fn connect(
    config_root: &Path,
    record: &AccountList,
    address: ServiceAddress,
) -> Result<(AccountId, Invoke<AccountList>), ServiceError> {
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
        ServiceKind::Forgejo => {
            let scope = forgejo::scope(&address.server)?;
            let title = forgejo::check(&scope, &address.secret)?;
            persist(
                config_root,
                FORGEJO_TYPE,
                &format!("{}/{}", scope.server, scope.path),
                &title,
                serde_json::json!({ "server_url": scope.server, "scope": scope.path }),
                &address.secret,
            )
        }
        ServiceKind::Mail => mail::connect(config_root, record, &address),
    }
}

/// The repositories a connected Forgejo account reviews, as its config keeps them.
fn forgejo_scope(account: &Account, server: String) -> forgejo::Scope {
    forgejo::Scope {
        server,
        path: account
            .config
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

/// Tilo's config directory of the home this process runs in: where its sources' keys are kept.
#[must_use]
pub fn config_root() -> Option<std::path::PathBuf> {
    accounts::config_root()
}

/// The connected service sources under `config_root`.
#[must_use]
pub fn connected(record: &AccountList) -> Vec<(AccountId, ServiceKind, String)> {
    record
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
) -> Result<(AccountId, Invoke<AccountList>), ServiceError> {
    let id = AccountId::new(format!("{type_id}-{}", short_digest(server)));
    let mut account = Account::new(id, type_id);
    account.display_name = title.chars().take(256).collect();
    account.config = config;
    crate::vault::persist::<ServiceError>(
        config_root,
        &account,
        accounts::Secret::password(secret.to_owned()),
    )?;
    Ok((account.id.clone(), AccountList::saving(&account)))
}

/// The account, its server and its key as the vault holds them.
fn reach(
    config_root: &Path,
    record: &AccountList,
    account: &str,
) -> Result<(Account, String, Zeroizing<String>), ServiceError> {
    let id = AccountId::new(account);
    let account = record.account(id.as_str()).ok_or(ServiceError::Missing)?;
    let vault = SecretVault::open_read_only_in(config_root);
    if vault.recovery_error().is_some() {
        return Err(ServiceError::Storage);
    }
    let secret = crate::vault::PrivateSecret(vault.get(&id).ok_or(ServiceError::Storage)?);
    let key = Zeroizing::new(secret.0.password.clone());
    let server = account
        .config
        .get("server_url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    Ok((account, server, key))
}

/// The connected mail account `account`: its settings as Mail keeps them, and the password the
/// person typed, from the vault. The password is for handing straight to where it is kept next.
///
/// # Errors
/// No such connected mail account, or the vault could not be read.
pub fn mail_account(
    config_root: &Path,
    record: &AccountList,
    account: &str,
) -> Result<(accounts::email::AccountConfig, Zeroizing<String>), ServiceError> {
    let (stored, _, secret) = reach(config_root, record, account)?;
    if ServiceKind::of_account(&stored) != Some(ServiceKind::Mail) {
        return Err(ServiceError::Missing);
    }
    let config = accounts::email::AccountConfig::from_value(stored.config)
        .map_err(|_| ServiceError::Missing)?;
    Ok((config, secret))
}

/// What the connected source `account` holds now, every group chosen until the person says
/// otherwise.
///
/// # Errors
/// The source is not connected, refused its key or could not be read.
pub fn review(
    config_root: &Path,
    record: &AccountList,
    account: &str,
) -> Result<ServiceReview, ServiceError> {
    let (stored, server, key) = reach(config_root, record, account)?;
    let kind = ServiceKind::of_account(&stored).ok_or(ServiceError::Missing)?;
    let (title, groups) = match kind {
        ServiceKind::Outline => (stored.display_name, outline::review(&server, &key)?),
        ServiceKind::Linear => (stored.display_name, linear::review(&server, &key)?),
        ServiceKind::Forgejo => {
            let scope = forgejo_scope(&stored, server);
            (stored.display_name, forgejo::review(&scope, &key)?)
        }
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
/// again, so an import after an interruption, or after it finished, lands only what is missing.
///
/// # Errors
/// The source could not be read, the import was cancelled, or the journal could not be saved; the
/// journal keeps what landed before.
pub fn run(
    config_root: &Path,
    record: &AccountList,
    journal: &mut ServiceJournal,
    landing: &mut dyn Landing,
    publish: &mut dyn FnMut(&str) -> Result<(), String>,
    save: &mut dyn FnMut(&ServiceJournal) -> Result<(), String>,
    cancel: &AtomicBool,
) -> Result<ServiceReport, ServiceError> {
    journal.state = JournalState::Running;
    save(journal).map_err(ServiceError::Journal)?;
    let result = land(config_root, record, journal, landing, publish, save, cancel);
    journal.state = match &result {
        // What the destination refused did not come over: the import is not complete.
        Ok(report) if !report.refused.is_empty() => JournalState::Failed,
        Ok(_) => JournalState::Complete,
        Err(ServiceError::Cancelled) => JournalState::Interrupted,
        Err(_) => JournalState::Failed,
    };
    journal.failure = match &result {
        Ok(report) if !report.refused.is_empty() => Some(format!(
            "{} of the items were refused: {}",
            report.refused.len(),
            report
                .refused
                .iter()
                .map(|item| format!("{} ({})", item.name, item.why))
                .collect::<Vec<_>>()
                .join("; ")
        )),
        Ok(_) => None,
        Err(error) => Some(error.to_string()),
    };
    if let Ok(report) = &result {
        journal.report = Some(report.clone());
    }
    save(journal).map_err(ServiceError::Journal)?;
    result
}

fn land(
    config_root: &Path,
    record: &AccountList,
    journal: &mut ServiceJournal,
    landing: &mut dyn Landing,
    publish: &mut dyn FnMut(&str) -> Result<(), String>,
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
    match journal.review.kind {
        ServiceKind::Outline => {
            let (_, server, key) = reach(config_root, record, &journal.review.account)?;
            let pages = outline::pages(&server, &key, &journal.review, &chosen, cancel)?;
            let pending = waiting(pages.pages, journal, &mut report, |page| &page.key);
            for part in pending.chunks(landing.batch().max(1)) {
                stop_check(cancel)?;
                let delivered = landing.pages(part).map_err(|error| unavailable(&error))?;
                keep_delivered(delivered, journal, &mut report, save)?;
            }
            report.not_reviewed = pages.not_reviewed;
            report.gone = pages.gone;
        }
        ServiceKind::Linear | ServiceKind::Forgejo => {
            let issues = if journal.review.kind == ServiceKind::Linear {
                let (_, server, key) = reach(config_root, record, &journal.review.account)?;
                let issues = linear::issues(&server, &key, &chosen, cancel)?;
                (issues.issues, issues.not_reviewed, issues.gone)
            } else {
                let (account, server, key) = reach(config_root, record, &journal.review.account)?;
                let scope = forgejo_scope(&account, server);
                let issues = forgejo::issues(&scope, &key, &chosen, cancel)?;
                (issues.issues, issues.not_reviewed, issues.gone)
            };
            let (issues, not_reviewed, gone) = issues;
            hand_issues(issues, journal, landing, &mut report, save, cancel)?;
            report.not_reviewed = not_reviewed;
            report.gone = gone;
        }
        ServiceKind::Mail => {
            let account = journal.review.account.clone();
            if chosen.contains(&account) {
                stop_check(cancel)?;
                if journal.landed.contains(&account) {
                    report.already_there += 1;
                } else {
                    publish(&account).map_err(ServiceError::Unavailable)?;
                    journal.landed.insert(account);
                    save(journal).map_err(ServiceError::Journal)?;
                    report.landed += 1;
                }
            }
        }
    }
    Ok(report)
}

/// Hand those of `issues` the journal lacks to `landing`, as many at once as it takes, keeping
/// the journal after each hand-over; `report` counts what landed, was there or was refused.
///
/// # Errors
/// The import was cancelled, the destination could not be reached, or the journal not saved; the
/// journal keeps what landed before.
pub fn hand_issues(
    issues: Vec<IncomingIssue>,
    journal: &mut ServiceJournal,
    landing: &mut dyn Landing,
    report: &mut ServiceReport,
    save: &mut dyn FnMut(&ServiceJournal) -> Result<(), String>,
    cancel: &AtomicBool,
) -> Result<(), ServiceError> {
    let pending = waiting(issues, journal, report, |issue| &issue.key);
    for part in pending.chunks(landing.batch().max(1)) {
        stop_check(cancel)?;
        let delivered = landing.issues(part).map_err(|error| unavailable(&error))?;
        keep_delivered(delivered, journal, report, save)?;
    }
    Ok(())
}

fn stop_check(cancel: &AtomicBool) -> Result<(), ServiceError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(ServiceError::Cancelled);
    }
    Ok(())
}

fn unavailable(error: &std::io::Error) -> ServiceError {
    ServiceError::Unavailable(error.to_string())
}

/// Those of `items` the journal does not keep as landed, counting the others as there already.
fn waiting<T>(
    items: Vec<T>,
    journal: &ServiceJournal,
    report: &mut ServiceReport,
    key: impl Fn(&T) -> &String,
) -> Vec<T> {
    let mut pending = Vec::with_capacity(items.len());
    for item in items {
        if journal.landed.contains(key(&item)) {
            report.already_there += 1;
        } else {
            pending.push(item);
        }
    }
    pending
}

/// Keep what a landing answered for one hand-over in the journal, and save it.
fn keep_delivered(
    delivered: Delivered,
    journal: &mut ServiceJournal,
    report: &mut ServiceReport,
    save: &mut dyn FnMut(&ServiceJournal) -> Result<(), String>,
) -> Result<(), ServiceError> {
    report.landed += delivered.landed.len();
    journal.landed.extend(delivered.landed);
    report.refused.extend(delivered.refused);
    save(journal).map_err(ServiceError::Journal)
}

fn short_digest(text: &str) -> String {
    let hex = blake3::hash(text.as_bytes()).to_hex();
    hex.get(..16).unwrap_or_default().to_owned()
}
