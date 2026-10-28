//! Thunderbird profiles, accounts, local mail and personal data inside an
//! already granted home. `prefs.js` is parsed as data and never executed.
//! Folder counts and flags come from the actual mbox/maildir files, so a
//! directory scan is never reported as a verified complete mailbox. A folder
//! is read in fixed chunks with one bound per message, so a mailbox larger
//! than memory is still counted without holding it.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use accounts::thunderbird::{self as metadata, ServerKind};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::migration_directory::{PinnedDirectory, same_snapshot};
use crate::migration_durability::readable_directory;
use crate::nofollow::metadata_nofollow;

use super::Inventory;

pub(crate) mod mbox;

pub(crate) use mbox::{MboxMessages, MessageTooLarge};

const MAX_PROFILE_INDEX: u64 = 64 * 1024;
const MAX_PREFERENCE_BYTES: u64 = 2 * 1024 * 1024;
/// The per-message bound inspection and the streamed import share. A larger
/// message is reported, never buffered without limit or silently skipped.
pub(crate) const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_MESSAGES: u64 = 100_000;
const MAX_FOLDERS: usize = 4096;
const MAX_PROFILES: usize = 128;
const MAX_DEPTH: usize = 8;

/// One profile found under a granted Thunderbird root.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThunderbirdProfile {
    pub source_id: String,
    pub label: String,
    pub accounts: Vec<ThunderbirdAccount>,
    pub address_books: Vec<ThunderbirdAddressBook>,
    pub calendars: Vec<ThunderbirdCalendar>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThunderbirdAccount {
    pub source_id: String,
    pub source_account: String,
    pub email: Option<String>,
    pub display_name: String,
    pub kind: String,
    /// This account's mail exists on this computer. An IMAP cache is still a
    /// local copy, not proof that the server holds every message.
    pub local_mail: bool,
    /// The account can be configured only after signing in again.
    pub needs_reconnect: bool,
    pub folders: Vec<ThunderbirdFolder>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThunderbirdFolder {
    pub source_id: String,
    /// The folder's path below its account, as Thunderbird names it.
    pub hierarchy: Vec<String>,
    pub format: String,
    pub messages: Option<u64>,
    pub unread: Option<u64>,
    pub flagged: Option<u64>,
    pub attachments: Option<u64>,
    pub bytes: u64,
    /// Every message counted here is a local copy. `false` means the source
    /// changed or exceeded the inspection bound; the count is then unknown.
    pub counted: bool,
    /// A local-only account (POP or Local Folders), not an IMAP cache.
    pub local_only: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThunderbirdAddressBook {
    pub source_id: String,
    pub name: String,
    pub local: bool,
    pub present: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate fact from the profile's calendar registry"
)]
pub struct ThunderbirdCalendar {
    pub source_id: String,
    pub name: String,
    pub local: bool,
    pub read_only: bool,
    pub enabled: bool,
    pub present: bool,
}

/// A reviewed profile retains the exact pinned directories and file names
/// used by capture. It is not serialized into public status records.
#[derive(Clone)]
pub struct ReviewedThunderbird {
    pub profile: ThunderbirdProfile,
    pub root: Arc<PinnedDirectory>,
    pub accounts: Vec<ReviewedAccount>,
    pub address_books: Vec<ReviewedFile>,
    pub calendars: Vec<ReviewedFile>,
}

impl std::fmt::Debug for ReviewedThunderbird {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReviewedThunderbird")
            .field("source_id", &self.profile.source_id)
            .field("accounts", &self.accounts.len())
            .field("address_books", &self.address_books.len())
            .field("calendars", &self.calendars.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct ReviewedAccount {
    pub inventory: ThunderbirdAccount,
    pub metadata: metadata::AccountMetadata,
    pub folders: Vec<ReviewedFolder>,
}

#[derive(Clone, Debug)]
pub struct ReviewedFolder {
    pub inventory: ThunderbirdFolder,
    /// Relative to the profile root.
    pub relative: Vec<String>,
    pub kind: FolderKind,
    pub local_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderKind {
    Mbox,
    Maildir,
}

#[derive(Clone, Debug)]
pub struct ReviewedFile {
    pub source_id: String,
    pub name: String,
    /// Relative to the profile root, when the file is present.
    pub relative: Option<Vec<String>>,
    pub present: bool,
    pub local: bool,
}

/// Discover native and Flatpak Thunderbird profiles beneath the granted home.
/// The returned `Inventory` carries only serializable metadata; capture uses
/// the separately retained reviewed profiles.
pub fn discover(home: &Path, inventory: &mut Inventory) {
    for (relative, label) in [
        (".thunderbird", "Thunderbird"),
        (
            ".var/app/org.mozilla.Thunderbird/.thunderbird",
            "Thunderbird (Flatpak)",
        ),
    ] {
        let path = home.join(relative);
        match std::fs::symlink_metadata(&path) {
            Ok(info) if info.is_dir() && !info.is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Ok(_) | Err(_) => {
                inventory
                    .unreadable_sources
                    .push("Thunderbird profile root");
                continue;
            }
        }
        match discover_root(&path, label) {
            Ok((profiles, unreadable)) => {
                for reviewed in profiles {
                    inventory.thunderbird.push(reviewed.profile.clone());
                    inventory.thunderbird_reviewed.push(reviewed);
                }
                if unreadable {
                    inventory.unreadable_sources.push("Thunderbird profile");
                }
            }
            Err(_) => inventory.unreadable_sources.push("Thunderbird profile"),
        }
    }
}

fn discover_root(root: &Path, label: &str) -> std::io::Result<(Vec<ReviewedThunderbird>, bool)> {
    let root_path = root.to_path_buf();
    let root = PinnedDirectory::open_absolute(root)?;
    root.validate()?;
    let profiles = profiles(&root)?;
    if profiles.len() > MAX_PROFILES {
        return Err(invalid("Thunderbird profile index exceeds its bound"));
    }
    let mut output = Vec::new();
    let mut unreadable = false;
    for (name, relative) in profiles {
        let profile_path = relative
            .iter()
            .fold(root_path.clone(), |path, name| path.join(name));
        match discover_profile(&root, &profile_path, &relative, label, &name) {
            Ok(Some(profile)) => output.push(profile),
            Ok(None) => {}
            Err(_) => unreadable = true,
        }
        if output.len() > MAX_PROFILES {
            return Err(invalid("Too many Thunderbird profiles"));
        }
    }
    root.validate()?;
    Ok((output, unreadable))
}

fn profiles(root: &Arc<PinnedDirectory>) -> std::io::Result<Vec<(String, Vec<String>)>> {
    let index = match read_file(root, "profiles.ini", MAX_PROFILE_INDEX) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(bytes) = index {
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| invalid("Thunderbird profile index is not UTF-8"))?;
        return profile_index(text);
    }
    let mut found = Vec::new();
    for (name, info) in entries(root)? {
        if info.is_dir()
            && !info.is_symlink()
            && (name.ends_with(".default") || name.ends_with(".default-release"))
        {
            found.push((name.clone(), vec![name]));
        }
        if found.len() > MAX_PROFILES {
            return Err(invalid("Too many Thunderbird profiles"));
        }
    }
    Ok(found)
}

fn profile_index(text: &str) -> std::io::Result<Vec<(String, Vec<String>)>> {
    let mut output = Vec::new();
    let mut active = false;
    let mut name = None;
    let mut path = None;
    let mut relative = true;
    let finish = |active: bool,
                  name: &mut Option<String>,
                  path: &mut Option<String>,
                  relative: bool,
                  output: &mut Vec<(String, Vec<String>)>|
     -> std::io::Result<()> {
        if active {
            let path = path
                .take()
                .ok_or_else(|| invalid("Thunderbird profile has no path"))?;
            if !relative || path.starts_with('/') || path.contains('\\') {
                return Err(invalid(
                    "Thunderbird profile lies outside its granted source",
                ));
            }
            let names = components(Path::new(&path))?;
            output.push((name.take().unwrap_or_else(|| path.clone()), names));
            if output.len() > MAX_PROFILES {
                return Err(invalid("Too many Thunderbird profiles"));
            }
        }
        name.take();
        path.take();
        Ok(())
    };
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') && line.ends_with(']') {
            finish(active, &mut name, &mut path, relative, &mut output)?;
            active = line.starts_with("[Profile");
            relative = true;
        } else if active && let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "Name" => name = Some(value.trim().chars().take(256).collect()),
                "Path" => path = Some(value.trim().into()),
                "IsRelative" => relative = value.trim() != "0",
                _ => {}
            }
        }
    }
    finish(active, &mut name, &mut path, relative, &mut output)?;
    Ok(output)
}

fn discover_profile(
    root: &Arc<PinnedDirectory>,
    profile_path: &Path,
    relative: &[String],
    label: &str,
    name: &str,
) -> std::io::Result<Option<ReviewedThunderbird>> {
    let directory = walk(root, relative)?;
    let prefs = match read_file(&directory, "prefs.js", MAX_PREFERENCE_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let text = std::str::from_utf8(&prefs)
        .map_err(|_| invalid("Thunderbird preferences are not UTF-8"))?;
    let parsed = metadata::parse_profile(text)
        .map_err(|_| invalid("Thunderbird preferences are malformed"))?;
    let profile_id = opaque(
        "thunderbird-profile",
        &[label, &relative.join("/"), &prefs_fingerprint(&prefs)],
    );
    let mut profile = ThunderbirdProfile {
        source_id: profile_id.clone(),
        label: format!("{label} · {name}"),
        accounts: Vec::new(),
        address_books: Vec::new(),
        calendars: Vec::new(),
    };
    let mut accounts = Vec::new();
    for account in &parsed.accounts {
        let (inventory, reviewed) =
            account_inventory(&directory, profile_path, &profile_id, account)?;
        profile.accounts.push(inventory.clone());
        accounts.push(ReviewedAccount {
            inventory,
            metadata: account.clone(),
            folders: reviewed,
        });
    }
    let mut address_books = Vec::new();
    for book in &parsed.address_books {
        let relative = book
            .filename
            .as_deref()
            .and_then(|name| safe_relative(Path::new(name)).ok())
            .filter(|relative| !relative.is_empty());
        let present = relative.as_ref().is_some_and(|relative| {
            relative_file(&directory, relative)
                .is_ok_and(|info| info.is_file() && !info.is_symlink())
        });
        let source_id = opaque("thunderbird-book", &[&profile_id, &book.source_id]);
        profile.address_books.push(ThunderbirdAddressBook {
            source_id: source_id.clone(),
            name: book.name.clone(),
            local: book.local,
            present,
        });
        address_books.push(ReviewedFile {
            source_id,
            name: book.name.clone(),
            relative: present.then_some(relative).flatten(),
            present,
            local: book.local,
        });
    }
    let mut calendars = Vec::new();
    for calendar in &parsed.calendars {
        let relative = calendar
            .filename
            .as_deref()
            .and_then(|name| safe_relative(Path::new(name)).ok())
            .filter(|relative| !relative.is_empty());
        let present = relative.as_ref().is_some_and(|relative| {
            relative_file(&directory, relative)
                .is_ok_and(|info| info.is_file() && !info.is_symlink())
        });
        let local = calendar
            .uri
            .as_deref()
            .is_none_or(|uri| !uri.starts_with("http://") && !uri.starts_with("https://"));
        let source_id = opaque("thunderbird-calendar", &[&profile_id, &calendar.source_id]);
        profile.calendars.push(ThunderbirdCalendar {
            source_id: source_id.clone(),
            name: calendar.name.clone(),
            local,
            read_only: calendar.read_only,
            enabled: calendar.enabled,
            present,
        });
        calendars.push(ReviewedFile {
            source_id,
            name: calendar.name.clone(),
            relative: present.then_some(relative).flatten(),
            present,
            local,
        });
    }
    Ok(Some(ReviewedThunderbird {
        profile,
        root: directory,
        accounts,
        address_books,
        calendars,
    }))
}

fn account_inventory(
    profile: &Arc<PinnedDirectory>,
    profile_path: &Path,
    profile_id: &str,
    account: &metadata::AccountMetadata,
) -> std::io::Result<(ThunderbirdAccount, Vec<ReviewedFolder>)> {
    let source_id = opaque(
        "thunderbird-account",
        &[profile_id, &account.source_account, &account.server_key],
    );
    let local_mail = !matches!(account.server_kind, ServerKind::Other);
    let needs_reconnect = account.candidate.is_some();
    let mut inventory = ThunderbirdAccount {
        source_id: source_id.clone(),
        source_account: account.source_account.clone(),
        email: account.email.clone(),
        display_name: account.display_name.clone(),
        kind: match account.server_kind {
            ServerKind::Imap => "imap",
            ServerKind::Pop3 => "pop3",
            ServerKind::Local => "local",
            ServerKind::Other => "other",
        }
        .into(),
        local_mail,
        needs_reconnect,
        folders: Vec::new(),
    };
    let Some(relative) = mail_directory(profile, profile_path, account)? else {
        return Ok((inventory, Vec::new()));
    };
    let directory = walk(profile, &relative)?;
    let mut folders = Vec::new();
    scan_folders(
        &directory,
        &relative,
        &[],
        account.server_kind,
        &source_id,
        0,
        &mut folders,
    )?;
    for folder in &folders {
        inventory.folders.push(folder.inventory.clone());
    }
    Ok((inventory, folders))
}

fn mail_directory(
    profile: &Arc<PinnedDirectory>,
    profile_path: &Path,
    account: &metadata::AccountMetadata,
) -> std::io::Result<Option<Vec<String>>> {
    if let Some(path) = account.directory.as_deref().filter(|path| !path.is_empty()) {
        let path = Path::new(path);
        if path.is_absolute() {
            let Ok(relative) = path.strip_prefix(profile_path) else {
                return Err(invalid(
                    "Thunderbird mail directory lies outside its granted profile",
                ));
            };
            return safe_relative(relative).map(Some);
        }
        return safe_relative(path).map(Some);
    }
    if let Some(path) = account
        .directory_rel
        .as_deref()
        .filter(|path| !path.is_empty())
    {
        let path = path
            .strip_prefix("[ProfD]")
            .or_else(|| path.strip_prefix("[profD]"))
            .unwrap_or(path);
        return safe_relative(Path::new(path.trim_start_matches('/'))).map(Some);
    }
    let relative = match account.server_kind {
        ServerKind::Imap => account
            .hostname
            .as_deref()
            .filter(|host| !host.is_empty())
            .map(|host| vec!["ImapMail".into(), host.into()]),
        ServerKind::Pop3 => account
            .hostname
            .as_deref()
            .filter(|host| !host.is_empty())
            .map(|host| vec!["Mail".into(), host.into()]),
        ServerKind::Local => Some(vec!["Mail".into(), "Local Folders".into()]),
        ServerKind::Other => None,
    };
    if let Some(relative) = &relative
        && !walk(profile, relative).is_ok_and(|dir| dir.validate().is_ok())
    {
        return Ok(None);
    }
    Ok(relative)
}

fn scan_folders(
    directory: &Arc<PinnedDirectory>,
    relative: &[String],
    hierarchy: &[String],
    kind: ServerKind,
    account_id: &str,
    depth: usize,
    output: &mut Vec<ReviewedFolder>,
) -> std::io::Result<()> {
    if depth > MAX_DEPTH || output.len() > MAX_FOLDERS {
        return Err(invalid("Thunderbird folder tree exceeds its bound"));
    }
    let entries = entries(directory)?;
    let mut names = BTreeMap::new();
    for (name, info) in entries {
        if name.starts_with('.') || info.is_symlink() {
            continue;
        }
        names.insert(name, info);
    }
    for (name, info) in &names {
        if info.is_dir() {
            let child_relative = child(relative, name);
            let child_dir = walk(directory, std::slice::from_ref(name))?;
            if is_maildir(&child_dir)? {
                let folder_hierarchy = child(hierarchy, name);
                output.push(count_maildir(
                    &child_dir,
                    &child_relative,
                    folder_hierarchy,
                    kind,
                    account_id,
                )?);
            } else if Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("sbd"))
            {
                let base = name
                    .get(..name.len().saturating_sub(".sbd".len()))
                    .unwrap_or(name);
                let mut child_hierarchy = hierarchy.to_vec();
                if !base.is_empty() {
                    child_hierarchy.push(base.into());
                }
                scan_folders(
                    &child_dir,
                    &child_relative,
                    &child_hierarchy,
                    kind,
                    account_id,
                    depth.saturating_add(1),
                    output,
                )?;
            }
        } else if info.is_file() && !ignored_mail_file(name) {
            let folder_hierarchy = child(hierarchy, name);
            output.push(count_mbox(
                directory,
                name,
                &child(relative, name),
                folder_hierarchy,
                kind,
                account_id,
            )?);
        }
    }
    Ok(())
}

fn ignored_mail_file(name: &str) -> bool {
    let Some(extension) = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
    else {
        return false;
    };
    ["msf", "json", "sqlite", "mab", "html", "bak"]
        .iter()
        .any(|ignored| extension.eq_ignore_ascii_case(ignored))
}

fn count_mbox(
    directory: &Arc<PinnedDirectory>,
    name: &str,
    relative: &[String],
    hierarchy: Vec<String>,
    kind: ServerKind,
    account_id: &str,
) -> std::io::Result<ReviewedFolder> {
    let (bytes, facts) = mbox_facts(directory, name)?;
    let folder_id = opaque(
        "thunderbird-folder",
        &[account_id, &hierarchy.join("/"), "mbox"],
    );
    let counted = facts.is_some();
    Ok(ReviewedFolder {
        inventory: ThunderbirdFolder {
            source_id: folder_id,
            hierarchy,
            format: "mbox".into(),
            messages: facts.as_ref().map(|facts| facts.messages),
            unread: facts.as_ref().map(|facts| facts.unread),
            flagged: facts.as_ref().map(|facts| facts.flagged),
            attachments: facts.as_ref().map(|facts| facts.attachments),
            bytes,
            counted,
            local_only: !matches!(kind, ServerKind::Imap),
        },
        relative: relative.to_vec(),
        kind: FolderKind::Mbox,
        local_only: !matches!(kind, ServerKind::Imap),
    })
}

fn count_maildir(
    directory: &Arc<PinnedDirectory>,
    relative: &[String],
    hierarchy: Vec<String>,
    kind: ServerKind,
    account_id: &str,
) -> std::io::Result<ReviewedFolder> {
    let mut messages = 0_u64;
    let mut unread = 0_u64;
    let mut flagged = 0_u64;
    let mut attachments = 0_u64;
    let mut bytes = 0_u64;
    let mut counted = true;
    for part in ["cur", "new"] {
        let Ok(dir) = walk(directory, std::slice::from_ref(&part.to_string())) else {
            continue;
        };
        for (name, info) in entries(&dir)? {
            if !info.is_file() || info.is_symlink() || name.starts_with('.') {
                continue;
            }
            match read_file(&dir, &name, MAX_MESSAGE_BYTES as u64) {
                Ok(raw) => {
                    bytes = bytes.saturating_add(raw.len() as u64);
                    let (read, starred, found) =
                        message_facts(&raw, Some(maildir_read_flags(&name)));
                    messages = messages.saturating_add(1);
                    if !read {
                        unread = unread.saturating_add(1);
                    }
                    if starred {
                        flagged = flagged.saturating_add(1);
                    }
                    attachments = attachments.saturating_add(found);
                    if messages > MAX_MESSAGES {
                        counted = false;
                        break;
                    }
                }
                Err(_) => counted = false,
            }
        }
    }
    let folder_id = opaque(
        "thunderbird-folder",
        &[account_id, &hierarchy.join("/"), "maildir"],
    );
    Ok(ReviewedFolder {
        inventory: ThunderbirdFolder {
            source_id: folder_id,
            hierarchy,
            format: "maildir".into(),
            messages: counted.then_some(messages),
            unread: counted.then_some(unread),
            flagged: counted.then_some(flagged),
            attachments: counted.then_some(attachments),
            bytes,
            counted,
            local_only: !matches!(kind, ServerKind::Imap),
        },
        relative: relative.to_vec(),
        kind: FolderKind::Maildir,
        local_only: !matches!(kind, ServerKind::Imap),
    })
}

fn is_maildir(directory: &Arc<PinnedDirectory>) -> std::io::Result<bool> {
    let names: Vec<_> = entries(directory)?
        .into_iter()
        .filter(|(_, info)| info.is_dir() && !info.is_symlink())
        .map(|(name, _)| name)
        .collect();
    Ok(["cur", "new", "tmp"]
        .iter()
        .all(|part| names.iter().any(|name| name == part)))
}

fn mbox_facts(
    directory: &Arc<PinnedDirectory>,
    name: &str,
) -> std::io::Result<(u64, Option<MailFacts>)> {
    let path = Path::new(name);
    let before = match metadata_nofollow(&directory.file, path) {
        Ok(info) => info,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((0, Some(MailFacts::default())));
        }
        Err(error) => return Err(error),
    };
    if before.is_symlink() || !before.is_file() {
        return Err(invalid("Thunderbird mail file is redirected or not a file"));
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&directory.file, path, &options)?;
    let mut messages = MboxMessages::new(
        std::io::BufReader::with_capacity(mbox::CHUNK, file),
        MAX_MESSAGE_BYTES,
    );
    let mut facts = MailFacts::default();
    let mut counted = true;
    loop {
        match messages.next_message() {
            Ok(Some(raw)) => {
                facts.record(&raw);
                if facts.messages > MAX_MESSAGES {
                    counted = false;
                    break;
                }
            }
            Ok(None) => break,
            Err(error) if mbox::is_too_large(&error) => {
                counted = false;
                break;
            }
            Err(error) => return Err(error),
        }
    }
    let after = metadata_nofollow(&directory.file, path)?;
    if !same_snapshot(&before, &after) {
        counted = false;
    }
    directory.validate()?;
    Ok((before.len(), counted.then_some(facts)))
}

/// What inspection learned about one folder. Every message was read, so the
/// counts describe the local copies that are actually there.
#[derive(Default)]
struct MailFacts {
    messages: u64,
    unread: u64,
    flagged: u64,
    attachments: u64,
}

impl MailFacts {
    fn record(&mut self, raw: &[u8]) {
        let (read, starred, attachments) = message_facts(raw, None);
        self.messages = self.messages.saturating_add(1);
        if !read {
            self.unread = self.unread.saturating_add(1);
        }
        if starred {
            self.flagged = self.flagged.saturating_add(1);
        }
        self.attachments = self.attachments.saturating_add(attachments);
    }
}

fn message_facts(raw: &[u8], maildir: Option<(bool, bool)>) -> (bool, bool, u64) {
    let parsed = mail_parser::MessageParser::default().parse(raw);
    let attachments = parsed
        .as_ref()
        .map_or(0, |message| message.attachments().count() as u64);
    if let Some((read, starred)) = maildir {
        return (read, starred, attachments);
    }
    let status = header(raw, b"x-mozilla-status:")
        .and_then(|value| u32::from_str_radix(value.trim(), 16).ok())
        .unwrap_or(0);
    ((status & 0x0001) != 0, (status & 0x0004) != 0, attachments)
}

fn header<'a>(raw: &'a [u8], name: &[u8]) -> Option<&'a str> {
    let end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .or_else(|| raw.windows(2).position(|window| window == b"\n\n"))
        .unwrap_or(raw.len());
    for line in raw
        .get(..end)
        .unwrap_or_default()
        .split(|byte| *byte == b'\n')
    {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(head) = line.get(..name.len())
            && head.eq_ignore_ascii_case(name)
        {
            return std::str::from_utf8(line.get(name.len()..).unwrap_or_default()).ok();
        }
    }
    None
}

fn maildir_flags(name: &str) -> Option<(bool, bool)> {
    let flags = name.split_once(":2,").map(|(_, flags)| flags)?;
    Some((flags.contains('S'), flags.contains('F')))
}

pub(crate) fn maildir_read_flags(name: &str) -> (bool, bool) {
    maildir_flags(name).unwrap_or((false, false))
}

/// A file larger than a caller's bound, so discovery can report the folder as
/// not fully counted and the import can refuse it by name.
pub(crate) fn too_large(limit: usize) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, MessageTooLarge { limit })
}

fn prefs_fingerprint(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn opaque(prefix: &str, parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part.as_bytes());
    }
    format!("{prefix}-{}", hex::encode(digest.finalize()))
}

fn child(parent: &[String], name: &str) -> Vec<String> {
    let mut child = parent.to_vec();
    child.push(name.into());
    child
}

fn components(path: &Path) -> std::io::Result<Vec<String>> {
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(name) => names.push(
                name.to_str()
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("Thunderbird path is not valid UTF-8"))?,
            ),
            std::path::Component::CurDir => {}
            _ => {
                return Err(invalid(
                    "Thunderbird path must stay inside its granted profile",
                ));
            }
        }
    }
    Ok(names)
}

fn safe_relative(path: &Path) -> std::io::Result<Vec<String>> {
    if path.is_absolute() {
        return Err(invalid(
            "Thunderbird path must stay inside its granted profile",
        ));
    }
    components(path)
}

pub(crate) fn entries(
    directory: &Arc<PinnedDirectory>,
) -> std::io::Result<Vec<(String, cap_primitives::fs::Metadata)>> {
    let dir = cap_std::fs::Dir::from_std_file(readable_directory(&directory.file)?);
    let mut output = Vec::new();
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid("Thunderbird entry name is not valid UTF-8"))?;
        let info = metadata_nofollow(&directory.file, Path::new(&name))?;
        output.push((name, info));
    }
    Ok(output)
}

pub(crate) fn walk(
    root: &Arc<PinnedDirectory>,
    names: &[String],
) -> std::io::Result<Arc<PinnedDirectory>> {
    let mut directory = Arc::clone(root);
    for name in names {
        let info = metadata_nofollow(&directory.file, Path::new(name))?;
        directory = directory.open_child(name.as_ref(), &info)?;
        directory.validate()?;
    }
    Ok(directory)
}

fn relative_file(
    root: &Arc<PinnedDirectory>,
    relative: &[String],
) -> std::io::Result<cap_primitives::fs::Metadata> {
    let (name, parent) = relative
        .split_last()
        .ok_or_else(|| invalid("Thunderbird file has no name"))?;
    let directory = walk(root, parent)?;
    let info = metadata_nofollow(&directory.file, Path::new(name))?;
    if info.is_symlink() || !info.is_file() {
        return Err(invalid("Thunderbird file is not a regular file"));
    }
    Ok(info)
}

pub(crate) fn read_file(
    directory: &Arc<PinnedDirectory>,
    name: &str,
    limit: u64,
) -> std::io::Result<Vec<u8>> {
    let path = Path::new(name);
    let before = metadata_nofollow(&directory.file, path)?;
    if before.is_symlink() || !before.is_file() {
        return Err(invalid("Thunderbird file is redirected or not a file"));
    }
    if before.len() > limit {
        return Err(too_large(usize::try_from(limit).unwrap_or(usize::MAX)));
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut file = cap_primitives::fs::open(&directory.file, path, &options)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(too_large(usize::try_from(limit).unwrap_or(usize::MAX)));
    }
    let after = metadata_nofollow(&directory.file, path)?;
    if !same_snapshot(&before, &after) {
        return Err(invalid("Thunderbird file changed while it was read"));
    }
    directory.validate()?;
    Ok(bytes)
}

pub(crate) fn read_reviewed(
    reviewed: &ReviewedThunderbird,
    relative: &[String],
    limit: u64,
) -> std::io::Result<Vec<u8>> {
    let (name, parent) = relative
        .split_last()
        .ok_or_else(|| invalid("Thunderbird file has no name"))?;
    let directory = walk(&reviewed.root, parent)?;
    read_file(&directory, name, limit)
}

pub(crate) fn read_maildir_files(
    reviewed: &ReviewedThunderbird,
    folder: &ReviewedFolder,
    limit: u64,
) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let directory = walk(&reviewed.root, &folder.relative)?;
    let mut output = Vec::new();
    for part in ["cur", "new"] {
        let Ok(dir) = walk(&directory, std::slice::from_ref(&part.to_string())) else {
            continue;
        };
        for (name, info) in entries(&dir)? {
            if !info.is_file() || info.is_symlink() || name.starts_with('.') {
                continue;
            }
            output.push((format!("{part}/{name}"), read_file(&dir, &name, limit)?));
        }
    }
    Ok(output)
}

fn invalid(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests;
