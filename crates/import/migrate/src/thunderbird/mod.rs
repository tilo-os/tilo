//! Thunderbird profiles, accounts and personal data inside an already granted home.
//! `prefs.js` is parsed as data and never executed, folder counts and flags come from the actual
//! mbox/maildir files, and a folder is counted in fixed chunks with one bound per message.
//!
//! [`discover`], [`contacts`], [`events`] and [`account_settings`] are what a laptop command
//! reads through; [`scan`] additionally keeps the pinned profile root the installer's importer
//! reads in place, so both drive the same reader.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use accounts::thunderbird::{self as metadata, ServerKind};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::bounded::read_bounded;
use crate::pinned::nofollow::metadata_nofollow;
use crate::pinned::{PinnedDirectory, readable_directory, same_snapshot};

pub mod contacts;
pub mod events;
#[cfg(any(test, feature = "fixture"))]
pub mod fixture;
pub mod logins;
pub mod mbox;
pub mod personal_data;

pub use contacts::address_book;
pub use events::events;
pub use logins::{LoginsError, SavedLogin, password_for, saved_logins};
pub use mbox::{MboxMessages, MessageTooLarge};

const MAX_PROFILE_INDEX: u64 = 64 * 1024;
const MAX_PREFERENCE_BYTES: u64 = 2 * 1024 * 1024;
/// The largest message a review reads to count its flags and attachments; a larger one is counted
/// without being read, and the import carries it whole, as the app's kept-mail bound allows.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_MESSAGES: u64 = 100_000;
/// The largest address book or calendar sidecar one profile read keeps.
pub const MAX_SIDECAR_BYTES: u64 = 32 * 1024 * 1024;
const MAX_FOLDERS: usize = 4096;
const MAX_PROFILES: usize = 128;
const MAX_PROBLEMS: usize = 256;
const MAX_DEPTH: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThunderbirdProfile {
    pub source_id: String,
    pub label: String,
    pub accounts: Vec<ThunderbirdAccount>,
    pub address_books: Vec<ThunderbirdAddressBook>,
    pub calendars: Vec<ThunderbirdCalendar>,
}

/// One sender identity in a profile's `prefs.js`: a display name and an
/// address, as the metadata reader names it.
pub use accounts::thunderbird::Identity as ThunderbirdIdentity;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThunderbirdAccount {
    pub source_id: String,
    pub source_account: String,
    pub email: Option<String>,
    pub display_name: String,
    pub kind: String,
    /// Every identity the account sends as, in the order `prefs.js` names them.
    pub identities: Vec<ThunderbirdIdentity>,
    /// An IMAP cache is still a local copy, not proof that the server holds every message.
    pub local_mail: bool,
    pub needs_reconnect: bool,
    pub folders: Vec<ThunderbirdFolder>,
    /// The IMAP/SMTP settings a mail app needs, without a password. It is source
    /// metadata, so it never reaches a serialized status record.
    #[serde(skip)]
    pub settings: Option<metadata::AccountMetadata>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThunderbirdFolder {
    pub source_id: String,
    pub hierarchy: Vec<String>,
    /// What the folder is for, from its name in the tree.
    pub kind: FolderKind,
    pub format: String,
    pub messages: Option<u64>,
    pub unread: Option<u64>,
    pub flagged: Option<u64>,
    pub attachments: Option<u64>,
    pub bytes: u64,
    /// Every message counted here is a local copy; `false` means the source changed or exceeded the
    /// inspection bound, so the count is unknown.
    pub counted: bool,
    pub local_only: bool,
}

/// What a folder is for, as Thunderbird names it. A folder that is not one of
/// the well-known ones is [`FolderKind::Other`], and still importable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FolderKind {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    #[default]
    Other,
}

impl FolderKind {
    /// Thunderbird keeps its well-known folders under fixed names, locally or
    /// on the server whose language it follows.
    #[must_use]
    pub fn of(hierarchy: &[String]) -> Self {
        for name in hierarchy {
            let name = name.to_ascii_lowercase();
            let role = match name.as_str() {
                "inbox" => Self::Inbox,
                "sent" | "sent mail" | "sent items" | "sent messages" => Self::Sent,
                "drafts" | "draft" => Self::Drafts,
                "trash" | "deleted" | "deleted items" | "deleted messages" | "bin" => Self::Trash,
                "junk" | "spam" | "bulk mail" => Self::Junk,
                "archives" | "archive" | "all mail" => Self::Archive,
                _ => continue,
            };
            return role;
        }
        Self::Other
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThunderbirdAddressBook {
    pub source_id: String,
    pub name: String,
    pub local: bool,
    pub present: bool,
    /// How many contacts the book holds, when its file could be read.
    pub contacts: Option<u64>,
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
    /// How many events the calendar holds, when its file could be read.
    pub events: Option<u64>,
}

/// Something of a Thunderbird source that could not be read, named so a person
/// or their agent can act on it. A read with any of these is incomplete, never
/// silently empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreadable {
    /// The profile as people see it, when the failure is at or inside one;
    /// `None` names the Thunderbird root itself.
    pub profile: Option<String>,
    /// The path inside the profile that could not be read; `None` names the
    /// profile itself.
    pub relative: Option<String>,
    /// What kind of failure it was.
    pub kind: std::io::ErrorKind,
    /// The reader's own words, when it said more than the kind does.
    pub detail: Option<String>,
}

impl Unreadable {
    fn of(profile: Option<String>, relative: Option<String>, error: &std::io::Error) -> Self {
        let detail = error.to_string();
        Self {
            profile,
            relative,
            kind: error.kind(),
            detail: (!detail.is_empty()).then_some(detail),
        }
    }

    fn of_kind(
        profile: Option<String>,
        relative: Option<String>,
        kind: std::io::ErrorKind,
    ) -> Self {
        Self {
            profile,
            relative,
            kind,
            detail: None,
        }
    }

    fn inside_profile(profile: &str, relative: &[String], error: &std::io::Error) -> Self {
        Self::of(Some(profile.to_owned()), Some(relative.join("/")), error)
    }

    /// The source this belongs to, as a receipt names it without a path.
    #[must_use]
    pub fn source(&self) -> &'static str {
        if self.profile.is_some() {
            "Thunderbird profile"
        } else {
            "Thunderbird profile root"
        }
    }

    fn says(&self) -> &'static str {
        match self.kind {
            std::io::ErrorKind::NotFound => "not found",
            std::io::ErrorKind::PermissionDenied => "permission denied",
            std::io::ErrorKind::InvalidData => "unreadable data",
            std::io::ErrorKind::InvalidInput => "refused",
            std::io::ErrorKind::NotADirectory => "not a directory",
            std::io::ErrorKind::IsADirectory => "a directory, not a file",
            _ => "unreadable",
        }
    }

    fn reason(&self) -> String {
        let says = self.says();
        match &self.detail {
            Some(detail) if !detail.to_ascii_lowercase().contains(says) => {
                format!("{says}: {detail}")
            }
            _ => says.to_owned(),
        }
    }
}

impl std::fmt::Display for Unreadable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let reason = self.reason();
        match (&self.profile, &self.relative) {
            (None, _) => write!(
                formatter,
                "the Thunderbird profile root could not be read ({reason})"
            ),
            (Some(profile), None) => write!(
                formatter,
                "Thunderbird profile {profile:?} could not be read ({reason})"
            ),
            (Some(profile), Some(relative)) => write!(
                formatter,
                "Thunderbird profile {profile:?}, {relative}, could not be read ({reason})"
            ),
        }
    }
}

/// It is not serialized into public status records.
#[derive(Clone)]
pub struct ReviewedThunderbird {
    pub profile: ThunderbirdProfile,
    pub root: Arc<PinnedDirectory>,
    pub accounts: Vec<ReviewedAccount>,
    pub address_books: Vec<ReviewedFile>,
    pub calendars: Vec<ReviewedFile>,
    /// The folders and files inside this profile that could not be read; the
    /// rest of the profile is still listed.
    pub problems: Vec<Unreadable>,
}

impl ReviewedThunderbird {
    /// The profile as a review lists it, with how many contacts and events its
    /// address books and calendars hold.
    #[must_use]
    pub fn profile(&self) -> ThunderbirdProfile {
        let mut profile = self.profile.clone();
        count_personal_data(&mut profile, self);
        profile
    }
}

impl std::fmt::Debug for ReviewedThunderbird {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReviewedThunderbird")
            .field("source_id", &self.profile.source_id)
            .field("accounts", &self.accounts.len())
            .field("address_books", &self.address_books.len())
            .field("calendars", &self.calendars.len())
            .field("problems", &self.problems.len())
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
    pub relative: Vec<String>,
    pub format: FolderFormat,
    pub local_only: bool,
}

/// How a folder is stored on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderFormat {
    Mbox,
    Maildir,
}

#[derive(Clone, Debug)]
pub struct ReviewedFile {
    pub source_id: String,
    pub name: String,
    pub relative: Option<Vec<String>>,
    pub present: bool,
    pub local: bool,
}

/// Where Thunderbird keeps its profiles in a home, with the label people see.
fn roots(home: &Path) -> Vec<(&'static str, PathBuf)> {
    // Windows keeps profiles under the roaming application data of the person's home. It is
    // looked for on every system: a Windows home is read natively, and also from its disk by the
    // installer running on another system.
    let windows = (
        "Thunderbird (Windows)",
        home.join("AppData/Roaming/Thunderbird"),
    );
    #[cfg(target_os = "macos")]
    {
        vec![("Thunderbird", home.join("Library/Thunderbird")), windows]
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec![
            ("Thunderbird", home.join(".thunderbird")),
            (
                "Thunderbird (Flatpak)",
                home.join(".var/app/org.mozilla.Thunderbird/.thunderbird"),
            ),
            windows,
        ]
    }
}

/// What one home's Thunderbird profiles are, and everything that could not be
/// read while looking. A scan with any problem is incomplete, never empty.
pub struct Scan {
    /// The profiles with the pinned roots the importer reads through.
    pub reviewed: Vec<ReviewedThunderbird>,
    /// The sources outside a returned profile that could not be read: a root,
    /// a profile's own files, its index. Each names its path and reason.
    pub problems: Vec<Unreadable>,
}

impl Scan {
    /// What could not be read, as one line, so a message says every reason it has.
    #[must_use]
    pub fn problems_text(&self) -> String {
        self.problems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Every Thunderbird profile in `home`, with the pinned profile root kept for
/// an importer that reads mail, address books and calendars from it. A `home`
/// that is itself a profile, by its folder or its `prefs.js` (one a person
/// handed over from wherever Thunderbird kept it), is that one profile.
#[must_use]
pub fn scan(home: &Path) -> Scan {
    let mut scan = Scan {
        reviewed: Vec::new(),
        problems: Vec::new(),
    };
    // The home is where the person keeps their profiles, named however their system names it:
    // on Fedora Atomic `/home` is a link to `/var/home`. Its own path is resolved once; inside
    // it, a profile is read without following any link.
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if let Some((root, name)) = handed_profile(&home) {
        match handed(&root, &name) {
            Ok(Some(profile)) => scan.reviewed.push(profile),
            Ok(None) => {}
            Err(error) => scan.problems.push(Unreadable::of(Some(name), None, &error)),
        }
        return scan;
    }
    for (label, path) in roots(&home) {
        // The root's own last name is never followed. Its parent may be a link,
        // as Flatpak's application directory is, and is resolved to where it
        // points: a granted root reached through a link is still that root.
        let path = match resolve_root(&path) {
            Ok(Some(path)) => path,
            Ok(None) => continue,
            Err(error) => {
                scan.problems.push(Unreadable::of(None, None, &error));
                continue;
            }
        };
        match std::fs::symlink_metadata(&path) {
            Ok(info) if info.is_dir() && !info.is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Ok(_) => {
                scan.problems.push(Unreadable::of_kind(
                    None,
                    None,
                    std::io::ErrorKind::NotADirectory,
                ));
                continue;
            }
            Err(error) => {
                scan.problems.push(Unreadable::of(None, None, &error));
                continue;
            }
        }
        match discover_root(&path, label) {
            Ok((profiles, problems)) => {
                scan.reviewed.extend(profiles);
                scan.problems.extend(problems);
            }
            Err(error) => scan.problems.push(Unreadable::of(None, None, &error)),
        }
    }
    scan
}

/// The folder a handed-over profile sits in and the profile's own name, when `path` is a
/// profile's folder or its `prefs.js`.
fn handed_profile(path: &Path) -> Option<(PathBuf, String)> {
    let folder = if path.file_name() == Some("prefs.js".as_ref()) {
        path.parent()?
    } else {
        path
    };
    let prefs = std::fs::symlink_metadata(folder.join("prefs.js")).ok()?;
    if !prefs.is_file() {
        return None;
    }
    Some((
        folder.parent()?.to_path_buf(),
        folder.file_name()?.to_str()?.to_owned(),
    ))
}

/// The profile `name` in the folder `root`, read as a root holding that one profile.
fn handed(root: &Path, name: &str) -> std::io::Result<Option<ReviewedThunderbird>> {
    let pinned = PinnedDirectory::open_absolute(root)?;
    pinned.validate()?;
    let found = discover_profile(
        &pinned,
        &root.join(name),
        &[name.to_owned()],
        "Thunderbird",
        name,
    )?;
    pinned.validate()?;
    Ok(found)
}

/// Where a root really is. Its own last name is left as it is, so a linked
/// root is refused rather than followed; a linked parent is resolved, because
/// a system may reach a granted folder through one. `None` when the parent is
/// not there, which means the root is not either.
fn resolve_root(path: &Path) -> std::io::Result<Option<PathBuf>> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Ok(Some(path.to_path_buf()));
    };
    match std::fs::canonicalize(parent) {
        Ok(parent) => Ok(Some(parent.join(name))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Every Thunderbird profile under `home`, with its accounts, folders, address
/// books and calendars. Counts are filled in from the files that are present.
///
/// # Errors
/// Thunderbird profiles were there but none could be read; the message names
/// what failed and why.
pub fn discover(home: &Path) -> Result<Vec<ThunderbirdProfile>, String> {
    let scan = scan(home);
    if scan.reviewed.is_empty() {
        return if scan.problems.is_empty() {
            Ok(Vec::new())
        } else {
            Err(format!(
                "no Thunderbird profile could be read: {}",
                scan.problems_text()
            ))
        };
    }
    Ok(scan
        .reviewed
        .iter()
        .map(ReviewedThunderbird::profile)
        .collect())
}

/// The reviewed profile `name` names by its `source_id`, its label, one of its
/// accounts' keys or one of its addresses.
pub fn find_reviewed<'a>(
    profiles: &'a [ReviewedThunderbird],
    name: &str,
) -> Option<&'a ReviewedThunderbird> {
    for candidate in profiles {
        if candidate.profile.source_id == name
            || candidate.profile.label == name
            || candidate
                .accounts
                .iter()
                .any(|account| account.inventory.source_account == name)
        {
            return Some(candidate);
        }
    }
    profiles.iter().find(|candidate| {
        candidate
            .accounts
            .iter()
            .any(|account| account.inventory.email.as_deref() == Some(name))
    })
}

/// The reviewed address book or calendar `name` names by its `source_id` or its
/// own name.
#[must_use]
pub fn find_sidecar<'a>(files: &'a [ReviewedFile], name: &str) -> Option<&'a ReviewedFile> {
    files
        .iter()
        .find(|file| file.source_id == name)
        .or_else(|| files.iter().find(|file| file.name == name))
}

/// Fill in how many contacts and events a profile's address books and
/// calendars hold. A sidecar that cannot be read keeps `None` rather than a
/// guessed count.
fn count_personal_data(profile: &mut ThunderbirdProfile, reviewed: &ReviewedThunderbird) {
    for (book, file) in profile
        .address_books
        .iter_mut()
        .zip(&reviewed.address_books)
    {
        book.contacts = Some(0);
        let Some(relative) = file
            .relative
            .as_deref()
            .filter(|_| file.present && file.local)
        else {
            book.contacts = None;
            continue;
        };
        book.contacts = read_reviewed(reviewed, relative, MAX_SIDECAR_BYTES)
            .ok()
            .and_then(|bytes| {
                personal_data::canonical(personal_data::Sidecar::AddressBook, &bytes)
                    .ok()
                    .flatten()
            })
            .map(|text| count_blocks(&text, "BEGIN:VCARD"));
    }
    for (calendar, file) in profile.calendars.iter_mut().zip(&reviewed.calendars) {
        calendar.events = Some(0);
        let Some(relative) = file
            .relative
            .as_deref()
            .filter(|_| file.present && file.local)
        else {
            calendar.events = None;
            continue;
        };
        calendar.events = read_reviewed(reviewed, relative, MAX_SIDECAR_BYTES)
            .ok()
            .and_then(|bytes| {
                personal_data::canonical(personal_data::Sidecar::Calendar, &bytes)
                    .ok()
                    .flatten()
            })
            .map(|text| count_blocks(&text, "BEGIN:VEVENT"));
    }
}

fn count_blocks(text: &str, begin: &str) -> u64 {
    u64::try_from(text.matches(begin).count()).unwrap_or(u64::MAX)
}

/// The connection settings for `account` (its `source_id`, its email or its
/// Thunderbird account key), so a mail app can be set up without a password.
#[must_use]
pub fn account_settings(
    profile: &ThunderbirdProfile,
    account: &str,
) -> Option<metadata::AccountMetadata> {
    profile
        .accounts
        .iter()
        .find(|candidate| {
            candidate.source_id == account
                || candidate.source_account == account
                || candidate.email.as_deref() == Some(account)
        })
        .and_then(|candidate| candidate.settings.clone())
}

fn discover_root(
    root: &Path,
    label: &str,
) -> std::io::Result<(Vec<ReviewedThunderbird>, Vec<Unreadable>)> {
    let root_path = root.to_path_buf();
    let root = PinnedDirectory::open_absolute(root)?;
    root.validate()?;
    let profiles = profiles(&root)?;
    if profiles.len() > MAX_PROFILES {
        return Err(invalid("Thunderbird profile index exceeds its bound"));
    }
    let mut output = Vec::new();
    let mut problems = Vec::new();
    for (name, relative) in profiles {
        let profile_path = relative
            .iter()
            .fold(root_path.clone(), |path, name| path.join(name));
        match discover_profile(&root, &profile_path, &relative, label, &name) {
            Ok(Some(profile)) => output.push(profile),
            Ok(None) => {}
            Err(error) => problems.push(Unreadable::of(
                Some(format!("{label} · {name}")),
                None,
                &error,
            )),
        }
        if output.len() > MAX_PROFILES {
            return Err(invalid("Too many Thunderbird profiles"));
        }
    }
    root.validate()?;
    Ok((output, problems))
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
    let profile_label = format!("{label} · {name}");
    let mut problems = Problems {
        profile: &profile_label,
        problems: Vec::new(),
    };
    for account in &parsed.accounts {
        let identities = account.identities.clone();
        let (inventory, reviewed) = account_inventory(
            &directory,
            profile_path,
            &profile_id,
            account,
            identities,
            &mut problems,
        );
        profile.accounts.push(inventory.clone());
        accounts.push(ReviewedAccount {
            inventory,
            metadata: account.clone(),
            folders: reviewed,
        });
    }
    let mut address_books = Vec::new();
    for book in &parsed.address_books {
        let relative = sidecar_relative(&directory, book.filename.as_deref(), &[]);
        let present = relative.is_some();
        let source_id = opaque("thunderbird-book", &[&profile_id, &book.source_id]);
        profile.address_books.push(ThunderbirdAddressBook {
            source_id: source_id.clone(),
            name: book.name.clone(),
            local: book.local,
            present,
            contacts: None,
        });
        address_books.push(ReviewedFile {
            source_id,
            name: book.name.clone(),
            relative,
            present,
            local: book.local,
        });
    }
    let mut calendars = Vec::new();
    for calendar in &parsed.calendars {
        let relative =
            sidecar_relative(&directory, calendar.filename.as_deref(), &["calendar-data"]);
        let present = relative.is_some();
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
            events: None,
        });
        calendars.push(ReviewedFile {
            source_id,
            name: calendar.name.clone(),
            relative,
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
        problems: problems.problems,
    }))
}

/// Where one profile's read notes what it could not read, under that
/// profile's name. A profile with more failures than the bound names the first
/// ones and stops; it is already incomplete, and the rest add no reason.
struct Problems<'a> {
    profile: &'a str,
    problems: Vec<Unreadable>,
}

impl Problems<'_> {
    fn note(&mut self, relative: &[String], error: &std::io::Error) {
        if self.problems.len() < MAX_PROBLEMS {
            self.problems
                .push(Unreadable::inside_profile(self.profile, relative, error));
        }
    }
}

fn account_inventory(
    profile: &Arc<PinnedDirectory>,
    profile_path: &Path,
    profile_id: &str,
    account: &metadata::AccountMetadata,
    identities: Vec<ThunderbirdIdentity>,
    problems: &mut Problems<'_>,
) -> (ThunderbirdAccount, Vec<ReviewedFolder>) {
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
        identities,
        local_mail,
        needs_reconnect,
        folders: Vec::new(),
        settings: Some(account.clone()),
    };
    let relative = match mail_directory(profile, profile_path, account) {
        Ok(Some(relative)) => relative,
        Ok(None) => return (inventory, Vec::new()),
        Err((relative, error)) => {
            problems.note(&relative, &error);
            return (inventory, Vec::new());
        }
    };
    let directory = match walk(profile, &relative) {
        Ok(directory) => directory,
        Err(error) => {
            problems.note(&relative, &error);
            return (inventory, Vec::new());
        }
    };
    let mut folders = Vec::new();
    if let Err(error) = scan_folders(
        &directory,
        &relative,
        &[],
        account.server_kind,
        &source_id,
        0,
        &mut folders,
        problems,
    ) {
        problems.note(&relative, &error);
    }
    for folder in &folders {
        inventory.folders.push(folder.inventory.clone());
    }
    (inventory, folders)
}

/// Where `account` keeps its mail inside this profile.
///
/// `prefs.js` names the directory both absolutely and relative to the profile.
/// A profile Thunderbird copied from another location keeps naming the old
/// place; that is stale metadata, not an unreadable folder, so a path outside
/// the granted profile is left out and the next candidate is tried. The last
/// candidate is where Thunderbird lays the mail out for the account's server.
///
/// # Errors
/// A named directory is present but could not be opened: its path and the
/// reason, so a review can say which folder is missing from it.
fn mail_directory(
    profile: &Arc<PinnedDirectory>,
    profile_path: &Path,
    account: &metadata::AccountMetadata,
) -> Result<Option<Vec<String>>, (Vec<String>, std::io::Error)> {
    let mut candidates: Vec<Vec<String>> = Vec::new();
    let mut try_candidate = |relative: Option<Vec<String>>| {
        if let Some(relative) = relative.filter(|relative| !relative.is_empty())
            && !candidates.contains(&relative)
        {
            candidates.push(relative);
        }
    };
    if let Some(path) = account.directory.as_deref().filter(|path| !path.is_empty()) {
        let path = Path::new(path);
        try_candidate(if path.is_absolute() {
            path.strip_prefix(profile_path)
                .ok()
                .and_then(|inside| safe_relative(inside).ok())
        } else {
            safe_relative(path).ok()
        });
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
        try_candidate(safe_relative(Path::new(path.trim_start_matches('/'))).ok());
    }
    try_candidate(match account.server_kind {
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
    });
    let mut failure = None;
    for relative in candidates {
        match walk(profile, &relative) {
            Ok(_) => return Ok(Some(relative)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => failure = Some((relative, error)),
        }
    }
    match failure {
        Some((relative, error)) => Err((relative, error)),
        None => Ok(None),
    }
}

fn scan_folders(
    directory: &Arc<PinnedDirectory>,
    relative: &[String],
    hierarchy: &[String],
    kind: ServerKind,
    account_id: &str,
    depth: usize,
    output: &mut Vec<ReviewedFolder>,
    problems: &mut Problems<'_>,
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
            let child_dir = match walk(directory, std::slice::from_ref(name)) {
                Ok(child_dir) => child_dir,
                Err(error) => {
                    problems.note(&child_relative, &error);
                    continue;
                }
            };
            let maildir = match is_maildir(&child_dir) {
                Ok(maildir) => maildir,
                Err(error) => {
                    problems.note(&child_relative, &error);
                    continue;
                }
            };
            if maildir {
                let folder_hierarchy = child(hierarchy, name);
                match count_maildir(
                    &child_dir,
                    &child_relative,
                    folder_hierarchy,
                    kind,
                    account_id,
                ) {
                    Ok(folder) => output.push(folder),
                    Err(error) => problems.note(&child_relative, &error),
                }
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
                if let Err(error) = scan_folders(
                    &child_dir,
                    &child_relative,
                    &child_hierarchy,
                    kind,
                    account_id,
                    depth.saturating_add(1),
                    output,
                    problems,
                ) {
                    problems.note(&child_relative, &error);
                }
            }
        } else if info.is_file() && !ignored_mail_file(name) {
            let folder_hierarchy = child(hierarchy, name);
            match count_mbox(
                directory,
                name,
                &child(relative, name),
                folder_hierarchy,
                kind,
                account_id,
            ) {
                Ok(folder) => output.push(folder),
                Err(error) => problems.note(&child(relative, name), &error),
            }
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
            kind: FolderKind::of(&hierarchy),
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
        format: FolderFormat::Mbox,
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
                // Larger than a review reads, but a message all the same: the import brings it.
                Err(error) if mbox::is_too_large(&error) => messages = messages.saturating_add(1),
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
            kind: FolderKind::of(&hierarchy),
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
        format: FolderFormat::Maildir,
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
            // Larger than a review reads, but a message all the same: the import brings it.
            Err(error) if mbox::is_too_large(&error) => {
                facts.messages = facts.messages.saturating_add(1);
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

/// Every message was read, so the counts describe the local copies that are actually there.
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
    let (read, starred) = message_flags(raw, maildir);
    let attachments = mail_parser::MessageParser::default()
        .parse(raw)
        .as_ref()
        .map_or(0, |message| message.attachments().count() as u64);
    (read, starred, attachments)
}

/// Whether a message was read and starred, from what a maildir's file name says or from the
/// `X-Mozilla-Status` an mbox keeps in its headers: never from a reading of the whole message,
/// which a copy of one only reads for what it delivers.
fn message_flags(raw: &[u8], maildir: Option<(bool, bool)>) -> (bool, bool) {
    if let Some((read, starred)) = maildir {
        return (read, starred);
    }
    let status = header(raw, b"x-mozilla-status:")
        .and_then(|value| u32::from_str_radix(value.trim(), 16).ok())
        .unwrap_or(0);
    ((status & 0x0001) != 0, (status & 0x0004) != 0)
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

pub fn maildir_read_flags(name: &str) -> (bool, bool) {
    maildir_flags(name).unwrap_or((false, false))
}

/// Discovery reports the folder as not fully counted and the import can refuse it by name.
pub fn too_large(limit: usize) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, MessageTooLarge { limit })
}

/// The relative path of an address book or calendar file, tried where
/// `prefs.js` names it and then under the directory Thunderbird really keeps
/// it in. `None` when no such file is present.
fn sidecar_relative(
    directory: &Arc<PinnedDirectory>,
    filename: Option<&str>,
    fallbacks: &[&str],
) -> Option<Vec<String>> {
    let relative = filename
        .and_then(|name| safe_relative(Path::new(name)).ok())
        .filter(|relative| !relative.is_empty())?;
    let present = |relative: &[String]| {
        relative_file(directory, relative).is_ok_and(|info| info.is_file() && !info.is_symlink())
    };
    if present(&relative) {
        return Some(relative);
    }
    let leaf = relative.last()?.clone();
    for fallback in fallbacks {
        let candidate = vec![(*fallback).to_owned(), leaf.clone()];
        if present(&candidate) {
            return Some(candidate);
        }
    }
    None
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

pub fn entries(
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

pub fn walk(
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

pub fn read_file(
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
    let bytes = read_bounded(&mut file, limit, || {
        too_large(usize::try_from(limit).unwrap_or(usize::MAX))
    })?;
    let after = metadata_nofollow(&directory.file, path)?;
    if !same_snapshot(&before, &after) {
        return Err(invalid("Thunderbird file changed while it was read"));
    }
    directory.validate()?;
    Ok(bytes)
}

pub fn read_reviewed(
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

pub fn read_maildir_files(
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
