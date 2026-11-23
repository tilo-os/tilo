//! Selected Thunderbird mail and personal data, sealed into a transport bundle and imported into
//! the destination's Mail account store and cache. Selection is part of the authenticated
//! artifact, so a caller cannot widen an export after review.
//!
//! A transfer that stays on this computer streams the granted Home instead of building the bundle.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use accounts::email::{AccountConfig, AuthMethod};
use accounts::thunderbird::{AccountMetadata, Transport};
use accounts::{Account, AccountId};

use crate::account_store::Accounts;
use mail_parser::MimeHeaders;
use serde::{Deserialize, Serialize};
use tilo_seal::{Frame, migration_password};

use crate::account_sources::thunderbird::{
    ReviewedThunderbird, ThunderbirdAccount, ThunderbirdFolder,
};

use super::ConnectionError;
use super::actions::target_root;

mod artifact;
mod personal_data;
mod stream;

pub use artifact::{MAX_ARTIFACT_BYTES, PasswordSealedThunderbird};
pub use stream::import_reviewed;

const DOMAIN: &[u8] = b"tilo-thunderbird-selection/v1\0";
const MAX_BINDING: usize = 4096;
const MAX_PLAINTEXT: usize = 256 * 1024 * 1024;
const MAX_SEALED: usize = MAX_PLAINTEXT + 4096;
const MAX_FOLDERS: usize = 4096;
const MAX_ACCOUNTS: usize = 128;
const MAX_FILES: usize = 8192;
const CACHE_VERSION: u32 = 1;
/// The Mail app holds an account's kept-mail file in memory, so a larger corpus is refused rather
/// than read whole or overwritten.
/// in memory when it opens an account, so a larger corpus is refused rather
/// than read whole or overwritten.
const MAX_CACHE: u64 = 256 * 1024 * 1024;
const CACHE_ATTEMPTS: usize = 3;
const MAIL_APP: &str = "so.tilo.mail";
const MAIL_ARTIFACT: Frame = Frame::new(
    b"TILOTB01",
    b"tilo-thunderbird-password/v1\0",
    16 * 1024,
    MAX_SEALED,
);

/// An empty account list is an empty selection, not "everything"; the ids are the opaque ones from
/// discovery.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThunderbirdSelection {
    pub accounts: Vec<ThunderbirdAccountSelection>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThunderbirdAccountSelection {
    pub source_id: String,
    pub folders: Vec<String>,
    pub address_books: Vec<String>,
    pub calendars: Vec<String>,
}

impl ThunderbirdSelection {
    pub fn all(reviewed: &ReviewedThunderbird) -> Self {
        Self {
            accounts: reviewed
                .accounts
                .iter()
                .map(|account| ThunderbirdAccountSelection {
                    source_id: account.inventory.source_id.clone(),
                    folders: account
                        .folders
                        .iter()
                        .map(|folder| folder.inventory.source_id.clone())
                        .collect(),
                    address_books: reviewed
                        .address_books
                        .iter()
                        .map(|book| book.source_id.clone())
                        .collect(),
                    calendars: reviewed
                        .calendars
                        .iter()
                        .map(|calendar| calendar.source_id.clone())
                        .collect(),
                })
                .collect(),
        }
    }

    pub fn validate(&self, reviewed: &ReviewedThunderbird) -> Result<(), ConnectionError> {
        if self.accounts.is_empty() || self.accounts.len() > MAX_ACCOUNTS {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let mut seen = BTreeSet::new();
        for selected in &self.accounts {
            if !seen.insert(selected.source_id.as_str()) {
                return Err(ConnectionError::InvalidConfiguration);
            }
            let account = reviewed
                .accounts
                .iter()
                .find(|account| account.inventory.source_id == selected.source_id)
                .ok_or(ConnectionError::AccountMissing)?;
            unique(&selected.folders, MAX_FOLDERS)?;
            unique(&selected.address_books, MAX_FOLDERS)?;
            unique(&selected.calendars, MAX_FOLDERS)?;
            for folder in &selected.folders {
                if !account
                    .folders
                    .iter()
                    .any(|item| &item.inventory.source_id == folder)
                {
                    return Err(ConnectionError::AccountMissing);
                }
            }
            for book in &selected.address_books {
                if !reviewed
                    .address_books
                    .iter()
                    .any(|item| &item.source_id == book)
                {
                    return Err(ConnectionError::AccountMissing);
                }
            }
            for calendar in &selected.calendars {
                if !reviewed
                    .calendars
                    .iter()
                    .any(|item| &item.source_id == calendar)
                {
                    return Err(ConnectionError::AccountMissing);
                }
            }
        }
        Ok(())
    }

    /// Structural checks that need no live source; the manifest uses this before the exact
    /// selection is available again.
    pub fn validate_shape(&self) -> Result<(), ConnectionError> {
        if self.accounts.is_empty() || self.accounts.len() > MAX_ACCOUNTS {
            return Err(ConnectionError::InvalidConfiguration);
        }
        let mut seen = BTreeSet::new();
        for selected in &self.accounts {
            if selected.source_id.is_empty()
                || selected.source_id.len() > 256
                || !seen.insert(selected.source_id.as_str())
            {
                return Err(ConnectionError::InvalidConfiguration);
            }
            unique(&selected.folders, MAX_FOLDERS)?;
            unique(&selected.address_books, MAX_FOLDERS)?;
            unique(&selected.calendars, MAX_FOLDERS)?;
        }
        Ok(())
    }
}

fn unique(values: &[String], limit: usize) -> Result<(), ConnectionError> {
    if values.len() > limit
        || values
            .iter()
            .any(|value| value.is_empty() || value.len() > 256)
    {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let set: BTreeSet<_> = values.iter().collect();
    (set.len() == values.len())
        .then_some(())
        .ok_or(ConnectionError::InvalidConfiguration)
}

fn validate_password(password: &[u8]) -> Result<(), ConnectionError> {
    if !migration_password(password) {
        return Err(ConnectionError::InvalidConfiguration);
    }
    Ok(())
}

pub use prepared_setup_model::{
    ThunderbirdAccountReceipt, ThunderbirdFolderReceipt, ThunderbirdImportReceipt,
};

fn binding(binding: &[u8]) -> Result<Vec<u8>, ConnectionError> {
    if binding.is_empty() || binding.len() > MAX_BINDING {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let mut context = DOMAIN.to_vec();
    context.extend_from_slice(binding);
    Ok(context)
}

/// Publishes canonical Mail accounts and local mail under `target_home`. An existing account with
/// the same email is reused; its configuration is never overwritten with Thunderbird's.
///
/// # Errors
///
/// Refuses a wrong key, binding or selection, a changed target, a conflicting account or
/// unavailable storage. No account is reported created unless its metadata and selected mail were
/// written.
fn publish_account(
    accounts: &Accounts,
    inventory: &ThunderbirdAccount,
    metadata: &AccountMetadata,
) -> Result<(AccountId, bool), ConnectionError> {
    let email = inventory
        .email
        .as_deref()
        .filter(|email| email.contains('@'))
        .map(str::to_ascii_lowercase);
    let id = email.as_ref().map_or_else(
        || AccountId::new(format!("thunderbird-{}", inventory.source_id)),
        AccountId::new,
    );
    let existing = accounts.get(id.as_str())?.or_else(|| {
        email.as_deref().and_then(|email| {
            accounts.all().ok()?.into_iter().find(|account| {
                account
                    .config
                    .get("email")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| value.eq_ignore_ascii_case(email))
                    || account
                        .config
                        .get("connected_identity")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|value| value.eq_ignore_ascii_case(email))
            })
        })
    });
    if let Some(mut existing) = existing {
        if !matches!(
            existing.type_id.as_str(),
            "email" | accounts::thunderbird::PENDING_TYPE
        ) {
            return Err(ConnectionError::AlreadyPresent);
        }
        provenance(&mut existing, inventory, metadata);
        accounts.keep(&existing)?;
        return Ok((existing.id, false));
    }
    let mut account = Account::new(id.clone(), accounts::thunderbird::PENDING_TYPE);
    account.display_name = inventory.display_name.chars().take(256).collect();
    account.config = config(inventory, metadata)?;
    provenance(&mut account, inventory, metadata);
    accounts.keep(&account)?;
    Ok((id, true))
}

fn provenance(account: &mut Account, inventory: &ThunderbirdAccount, metadata: &AccountMetadata) {
    if !account.config.is_object() {
        account.config = serde_json::json!({});
    }
    if let Some(config) = account.config.as_object_mut() {
        config.insert(
            "source_provenance".into(),
            serde_json::json!({
                "mail_client": "thunderbird",
                "profile": metadata.source_account,
                "account": metadata.server_key,
                "kind": inventory.kind,
            }),
        );
        config.insert(
            "source_thunderbird_account".into(),
            serde_json::Value::String(inventory.source_id.clone()),
        );
    }
}

fn config(
    inventory: &ThunderbirdAccount,
    metadata: &AccountMetadata,
) -> Result<serde_json::Value, ConnectionError> {
    let candidate = metadata.candidate.as_ref();
    let authentication = candidate.map_or(AuthMethod::Password, |candidate| {
        if candidate.requires_oauth() {
            AuthMethod::OAuth
        } else {
            AuthMethod::Password
        }
    });
    let mut config = candidate
        .and_then(|candidate| candidate.reviewed_config(authentication).ok())
        .unwrap_or_else(|| {
            let (imap_host, imap_port, imap_tls) =
                candidate.map_or((String::new(), 993, true), |c| {
                    (
                        c.incoming.host.clone(),
                        c.incoming.port,
                        c.incoming.transport == Transport::Tls,
                    )
                });
            let (smtp_host, smtp_port, smtp_tls) =
                candidate.map_or((String::new(), 465, true), |c| {
                    (
                        c.outgoing.host.clone(),
                        c.outgoing.port,
                        c.outgoing.transport != Transport::Plain,
                    )
                });
            AccountConfig {
                identities: Vec::new(),
                display_name: inventory.display_name.clone(),
                email: inventory.email.clone().unwrap_or_default(),
                imap_host,
                imap_port,
                imap_tls,
                smtp_host,
                smtp_port,
                smtp_tls,
                username: candidate.map_or_else(String::new, |c| c.incoming.username.clone()),
                auth: authentication,
                import_pending: true,
                password: String::new(),
            }
        });
    config.import_pending = true;
    config.password.clear();
    config
        .to_value()
        .map_err(|_| ConnectionError::InvalidConfiguration)
}

/// The wire shape `mail-engine` reads, so the app can open what import writes. Mutable flags and
/// labels belong to the person, so an existing message wins over the imported copy.
#[derive(Serialize, Deserialize)]
struct Kept {
    version: u32,
    messages: Vec<CachedMessage>,
}

impl Kept {
    fn empty() -> Self {
        Self {
            version: CACHE_VERSION,
            messages: Vec::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedMessage {
    id: uuid::Uuid,
    rfc_message_id: Option<String>,
    in_reply_to: Option<String>,
    from: CachedAddr,
    to: Vec<CachedAddr>,
    cc: Vec<CachedAddr>,
    subject: String,
    body: String,
    #[serde(default)]
    html_body: Option<String>,
    attachments: Vec<CachedAttachment>,
    timestamp_min: i64,
    read: bool,
    starred: bool,
    folder: CachedFolder,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    account: String,
    #[serde(default)]
    server: Option<CachedServerPlace>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedAddr {
    name: Option<String>,
    email: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct CachedAttachment {
    filename: String,
    mime: String,
    size: u64,
    hydrated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_id: Option<String>,
    kept: bool,
}

/// Spelled exactly as the Mail app stores them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CachedFolder {
    Inbox,
    Starred,
    Sent,
    Drafts,
    Archive,
    Trash,
    Junk,
}

/// Imported mail is local-only, so this stays absent; the shape must match the app's `ServerPlace`.
#[derive(Clone, Copy, Serialize, Deserialize)]
struct CachedServerPlace {
    folder: CachedFolder,
    uid: u32,
    uid_validity: u32,
}

fn cache_path(cache: &Path, account: &AccountId) -> PathBuf {
    cache.join(format!("{}.json", file_stem(account.as_str())))
}

/// A version this importer does not know is treated as empty, as the Mail app treats it; malformed
/// content is an error, because replacing it would destroy mail the person may still recover.
fn parse_kept(bytes: Option<&[u8]>) -> Result<Kept, ConnectionError> {
    let Some(bytes) = bytes else {
        return Ok(Kept::empty());
    };
    let mut kept: Kept =
        serde_json::from_slice(bytes).map_err(|_| ConnectionError::StorageUnavailable)?;
    if kept.version != CACHE_VERSION {
        return Ok(Kept::empty());
    }
    let mut seen = BTreeSet::new();
    kept.messages.retain(|message| seen.insert(message.id));
    Ok(kept)
}

/// The attachment bytes, written once the message is inserted.
fn message(
    raw: &[u8],
    folder: &ThunderbirdFolder,
    inventory: &ThunderbirdAccount,
    maildir: Option<(bool, bool)>,
) -> Result<Option<(CachedMessage, Vec<(usize, Vec<u8>)>)>, ConnectionError> {
    let Some(parsed) = mail_parser::MessageParser::default().parse(raw) else {
        return Ok(None);
    };
    let rfc_message_id = parsed.message_id().map(std::string::ToString::to_string);
    let id = rfc_message_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map_or_else(
            || {
                let digest = blake3::hash(raw);
                let mut bytes = [0_u8; 16];
                bytes.copy_from_slice(&digest.as_bytes()[..16]);
                uuid::Uuid::from_bytes(bytes)
            },
            stable_message_id,
        );
    let from = parsed
        .from()
        .and_then(|addresses| addresses.first())
        .map_or_else(
            || CachedAddr {
                name: None,
                email: "unknown@unknown".into(),
            },
            addr,
        );
    let to = parsed
        .to()
        .map(|addresses| addresses.iter().map(addr).collect())
        .unwrap_or_default();
    let cc = parsed
        .cc()
        .map(|addresses| addresses.iter().map(addr).collect())
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
    let mut attachments = Vec::new();
    let mut payloads = Vec::new();
    for (index, part) in parsed.attachments().enumerate() {
        let filename = part
            .attachment_name()
            .map_or_else(|| "attachment.bin".into(), str::to_owned);
        let bytes = part.contents().to_vec();
        let mime = mime_guess::from_path(&filename)
            .first_or_octet_stream()
            .to_string();
        attachments.push(CachedAttachment {
            filename,
            mime,
            size: bytes.len() as u64,
            hydrated: true,
            content_id: None,
            kept: true,
        });
        payloads.push((index, bytes));
    }
    let (read, starred) = maildir.unwrap_or_else(|| mbox_flags(raw));
    let (folder_key, labels) = mail_folder(&folder.hierarchy, folder.local_only);
    let timestamp_min = parsed
        .date()
        .map_or(0, |date| date.to_timestamp().saturating_div(60));
    Ok(Some((
        CachedMessage {
            id,
            rfc_message_id,
            in_reply_to: parsed
                .references()
                .as_text_list()
                .and_then(|ids| ids.first())
                .map(std::string::ToString::to_string)
                .or_else(|| parsed.in_reply_to().as_text().map(str::to_string)),
            from,
            to,
            cc,
            subject: parsed.subject().unwrap_or("(no subject)").to_string(),
            body,
            html_body,
            attachments,
            timestamp_min,
            read,
            starred,
            folder: folder_key,
            labels,
            account: inventory.source_id.clone(),
            server: None,
        },
        payloads,
    )))
}

fn addr(address: &mail_parser::Addr) -> CachedAddr {
    CachedAddr {
        name: address
            .name
            .as_ref()
            .map(std::string::ToString::to_string)
            .filter(|name| !name.is_empty()),
        email: address
            .address
            .as_ref()
            .map(std::string::ToString::to_string)
            .unwrap_or_default(),
    }
}

fn stable_message_id(message_id: &str) -> uuid::Uuid {
    fn fnv1a(bytes: &[u8], key: u64) -> u64 {
        let mut hasher = fnv::FnvHasher::with_key(key);
        std::hash::Hasher::write(&mut hasher, bytes);
        std::hash::Hasher::finish(&hasher)
    }
    let norm = message_id
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>');
    let hi = fnv1a(norm.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let lo = fnv1a(norm.as_bytes(), 0x0102_0304_0506_0708);
    uuid::Uuid::from_u128((u128::from(hi) << 64) | u128::from(lo))
}

fn mail_folder(hierarchy: &[String], local_only: bool) -> (CachedFolder, Vec<String>) {
    let name = hierarchy
        .last()
        .map_or("", String::as_str)
        .to_ascii_lowercase();
    let folder = match name.as_str() {
        "inbox" => CachedFolder::Inbox,
        "sent" | "sent items" | "sent mail" => CachedFolder::Sent,
        "drafts" | "draft" => CachedFolder::Drafts,
        "trash" | "deleted items" | "deleted" => CachedFolder::Trash,
        "junk" | "spam" | "junk e-mail" => CachedFolder::Junk,
        _ => CachedFolder::Archive,
    };
    let labels =
        if matches!(folder, CachedFolder::Archive) && name != "archive" && name != "archives" {
            let path = hierarchy
                .iter()
                .map(|part| {
                    part.chars()
                        .filter(|ch| !ch.is_control() && *ch != '/')
                        .take(128)
                        .collect::<String>()
                })
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("/");
            if path.is_empty() {
                Vec::new()
            } else {
                vec![format!(
                    "Thunderbird/{}",
                    if local_only {
                        path
                    } else {
                        format!("{path} (local copy)")
                    }
                )]
            }
        } else {
            Vec::new()
        };
    (folder, labels)
}

fn mbox_flags(raw: &[u8]) -> (bool, bool) {
    const STATUS: &[u8] = b"x-mozilla-status:";
    let end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .or_else(|| raw.windows(2).position(|window| window == b"\n\n"))
        .unwrap_or(raw.len());
    for line in raw[..end].split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() < STATUS.len() {
            continue;
        }
        let (head, value) = line.split_at(STATUS.len());
        if !head.eq_ignore_ascii_case(STATUS) {
            continue;
        }
        if let Ok(value) = std::str::from_utf8(value)
            && let Ok(value) = u32::from_str_radix(value.trim(), 16)
        {
            return ((value & 0x0001) != 0, (value & 0x0004) != 0);
        }
    }
    (false, false)
}

fn component(value: &str) -> String {
    let name: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | '@' | '+') {
                ch
            } else {
                '_'
            }
        })
        .take(128)
        .collect();
    if name.is_empty() || name == "." || name == ".." {
        "item".into()
    } else {
        name
    }
}

fn file_stem(account: &str) -> String {
    account
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '@' | '.' | '-' | '_' | '+') {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn ensure_dir(base: &Path, parts: &[&str]) -> Result<PathBuf, ConnectionError> {
    let mut path = base.to_path_buf();
    for part in parts {
        path.push(part);
        ensure_private_dir(&path)?;
    }
    Ok(path)
}

fn ensure_private_dir(path: &Path) -> Result<(), ConnectionError> {
    match std::fs::symlink_metadata(path) {
        Ok(info) if info.is_dir() && !info.is_symlink() => {}
        Ok(_) => return Err(ConnectionError::InvalidConfiguration),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(ConnectionError::InvalidConfiguration)?;
            ensure_private_dir(parent)?;
            std::fs::create_dir(path).map_err(|_| ConnectionError::StorageUnavailable)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                    .map_err(|_| ConnectionError::StorageUnavailable)?;
            }
        }
        Err(_) => return Err(ConnectionError::StorageUnavailable),
    }
    Ok(())
}

/// A missing directory or file is `None`; anything else unreadable is an error, never an empty file
/// that would authorize replacing it.
fn read_private(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, ConnectionError> {
    use crate::migration_directory::PinnedDirectory;
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    let parent_path = path.parent().ok_or(ConnectionError::InvalidConfiguration)?;
    let name = path
        .file_name()
        .ok_or(ConnectionError::InvalidConfiguration)?;
    let parent = match PinnedDirectory::open_absolute(parent_path) {
        Ok(parent) => parent,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConnectionError::StorageUnavailable),
    };
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut file = match cap_primitives::fs::open(&parent.file, Path::new(name), &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConnectionError::StorageUnavailable),
    };
    let metadata = file
        .metadata()
        .map_err(|_| ConnectionError::StorageUnavailable)?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(ConnectionError::StorageUnavailable);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ConnectionError::StorageUnavailable)?;
    parent
        .validate()
        .map_err(|_| ConnectionError::StorageUnavailable)?;
    if bytes.len() as u64 > limit {
        return Err(ConnectionError::StorageUnavailable);
    }
    Ok(Some(bytes))
}

/// A reader never sees half a file, and a crash leaves the previous one intact.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), ConnectionError> {
    match std::fs::symlink_metadata(path) {
        Ok(info) if info.is_file() && !info.is_symlink() => {
            let existing = std::fs::read(path).map_err(|_| ConnectionError::StorageUnavailable)?;
            return if existing == bytes {
                Ok(())
            } else {
                Err(ConnectionError::AlreadyPresent)
            };
        }
        Ok(_) => return Err(ConnectionError::InvalidConfiguration),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(ConnectionError::StorageUnavailable),
    }
    ensure_private_dir(path.parent().ok_or(ConnectionError::StorageUnavailable)?)?;
    tilo_files::native::atomic::write_private(path, bytes)
        .map_err(|_| ConnectionError::StorageUnavailable)
}

#[cfg(test)]
mod tests;
