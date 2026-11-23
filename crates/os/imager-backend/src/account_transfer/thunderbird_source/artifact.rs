//! The selected mail a prepared setup carries from one computer to another: one artifact file,
//! written as the source is read and imported the same way.
//!
//! A package that held every message in one JSON bundle held the whole mailbox, its JSON expansion
//! and its ciphertext in memory at once, and a mailbox of any size took minutes of number parsing.
//! This artifact is a small password-wrapped header naming the accounts, folders and files, then
//! the messages in one stream sealed in 64 KiB chunks under the header's key: each message is
//! read, sealed, imported and let go before the next, so memory follows the largest message rather
//! than the mailbox.

use std::collections::BTreeSet;
use std::io::{self, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use accounts::AccountId;
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use migration_sources::pinned::PinnedDirectory;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tilo_seal::{Context, FrameError, Label, SymmetricKey, content, envelope};

use crate::account_sources::thunderbird::{
    FolderFormat, MAX_SIDECAR_BYTES, MboxMessages, ReviewedFolder, ReviewedThunderbird,
    ThunderbirdAccount, ThunderbirdFolder, entries, maildir_read_flags, mbox, read_file,
    read_reviewed, walk,
};
use crate::migration_directory::same_snapshot;
use crate::nofollow::metadata_nofollow;

use super::stream::{MergeError, Retention, Spool, publish_merge};
use super::{
    ConnectionError, MAIL_APP, MAIL_ARTIFACT, MAX_ACCOUNTS, MAX_CACHE, MAX_FILES, MAX_FOLDERS,
    ThunderbirdAccountReceipt, ThunderbirdFolderReceipt, ThunderbirdImportReceipt,
    ThunderbirdSelection, cache_path, component, ensure_dir, ensure_private_dir, file_stem,
    message, parse_kept, publish_account, read_private, target_root, write_file,
};

/// Names an artifact of this shape: the password-wrapped header, then the sealed message stream.
const MAGIC: &[u8; 8] = b"TILOTBV2";
/// The most an entry's payload may hold: one message, or one sidecar.
const MAX_ENTRY: u32 = 64 * 1024 * 1024;
const ENTRY_BEGIN_FILE: u8 = 1;
const ENTRY_MESSAGE: u8 = 2;
const ENTRY_END: u8 = 3;
const HEADER_VERSION: u8 = 1;
/// The most mail one package carries.
pub const MAX_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const RETAINED: [&str; 5] = [".local", "share", "tilo", "mail-import", "thunderbird"];

fn broken(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_owned())
}

fn frame_error(error: FrameError) -> io::Error {
    match error {
        FrameError::Malformed => broken("the mail artifact is malformed"),
        FrameError::Unauthenticated => broken("the mail artifact does not open"),
    }
}

fn as_io(error: ConnectionError) -> io::Error {
    match error {
        ConnectionError::InvalidConfiguration => broken("the selected mail is not as reviewed"),
        ConnectionError::CredentialUnavailable => broken("the mail artifact does not open"),
        other => io::Error::other(format!("{other:?}")),
    }
}

use crate::migration::check_cancel;

/// What the header names: the selection's accounts and folders, the files each folder's mail is
/// read from, and the selected address books and calendars. No message bytes: those follow in the
/// stream.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Header {
    version: u8,
    profiles: Vec<HeaderProfile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeaderProfile {
    source_id: String,
    label: String,
    accounts: Vec<HeaderAccount>,
    address_books: Vec<HeaderSidecar>,
    calendars: Vec<HeaderSidecar>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeaderAccount {
    inventory: ThunderbirdAccount,
    metadata: accounts::thunderbird::AccountMetadata,
    folders: Vec<HeaderFolder>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeaderFolder {
    inventory: ThunderbirdFolder,
    /// The parts of the folder that hold mail: an mbox's file, or a maildir's `cur` and `new`.
    files: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeaderSidecar {
    source_id: String,
    name: String,
    bytes: Vec<u8>,
}

/// The artifact a person's password opens.
pub struct PasswordSealedThunderbird {
    file: std::fs::File,
    len: u64,
}

impl std::fmt::Debug for PasswordSealedThunderbird {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PasswordSealedThunderbird")
            .field("bytes", &self.len)
            .finish_non_exhaustive()
    }
}

impl PasswordSealedThunderbird {
    /// Writes `selection`'s mail to `path` as it is read, opened by `password`; the file is private
    /// and appears only once complete.
    ///
    /// # Errors
    /// The selection differs from the source, the password is outside the migration policy, the
    /// source changed while it was read, a message or sidecar exceeds its bound, or `cancel` was
    /// set.
    pub fn seal(
        reviewed: &ReviewedThunderbird,
        selection: &ThunderbirdSelection,
        password: &[u8],
        setup_binding: &[u8],
        cancel: &AtomicBool,
        path: &Path,
    ) -> io::Result<Self> {
        selection.validate(reviewed).map_err(as_io)?;
        super::validate_password(password).map_err(as_io)?;
        super::binding(setup_binding).map_err(as_io)?;
        let key = SymmetricKey::generate().map_err(|_| broken("the artifact's key"))?;
        let header = build_header(reviewed, selection).map_err(as_io)?;
        let plain = serde_json::to_vec(&header).map_err(|_| broken("header"))?;
        let context = super::binding(setup_binding).map_err(as_io)?;
        let sealed_header = envelope::seal(
            &key,
            Context::new(Label::MigrationPackage, &context),
            &plain,
        )
        .map_err(|_| broken("header"))?;
        let frame = MAIL_ARTIFACT
            .seal(password, setup_binding, &key, &sealed_header)
            .map_err(frame_error)?;
        let temporary = temporary_path(path);
        if let Err(error) = write_artifact(reviewed, &header, &key, cancel, &frame, &temporary) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error);
        }
        std::fs::rename(&temporary, path).map_err(|_| io::Error::other("artifact publish"))?;
        Self::read(path, MAX_ARTIFACT_BYTES)
    }

    /// Opens an artifact at `file`, checking its framing and bounds without deriving a key; the
    /// password authenticates it only when it is imported.
    ///
    /// # Errors
    /// Not this artifact's shape, or larger than `limit`.
    pub fn open(file: std::fs::File, limit: u64) -> io::Result<Self> {
        let len = file.metadata()?.len();
        if len > limit {
            return Err(broken(
                "the mail artifact is larger than one package carries",
            ));
        }
        let mut reader = &file;
        let frame_len = read_prefix(&mut reader)?;
        if frame_len as u64 + 12 > len {
            return Err(broken("the mail artifact is cut short"));
        }
        Ok(Self { file, len })
    }

    pub fn read(path: &Path, limit: u64) -> io::Result<Self> {
        Self::open(std::fs::File::open(path)?, limit)
    }

    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The digest the package's declaration names, hashed as the file is read.
    ///
    /// # Errors
    /// The artifact could not be read back.
    pub fn sha256(&self) -> io::Result<[u8; 32]> {
        let mut file = self.file.try_clone()?;
        file.rewind()?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0_u8; mbox::CHUNK];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(buffer.get(..read).unwrap_or_default());
        }
        Ok(hash.finalize().into())
    }

    /// Authenticates the header, proves the selection is exactly the one prepared, and imports the
    /// streamed messages into the target's Mail store and cache.
    ///
    /// # Errors
    /// A wrong password or binding, a selection that differs from the prepared one, a changed,
    /// truncated or redirected artifact, an unavailable target, or `cancel` was set.
    pub fn import(
        &self,
        target_home: &Path,
        password: &[u8],
        setup_binding: &[u8],
        expected: &ThunderbirdSelection,
        cancel: &AtomicBool,
    ) -> io::Result<ThunderbirdImportReceipt> {
        super::validate_password(password).map_err(as_io)?;
        for _ in 0..super::CACHE_ATTEMPTS {
            match self.import_pass(target_home, password, setup_binding, expected, cancel)? {
                Some(receipt) => return Ok(receipt),
                None => continue,
            }
        }
        Err(io::Error::other(
            "the kept mail for an account kept changing while it was written",
        ))
    }

    /// One pass of [`Self::import`]: `None` when an account's kept mail changed under it, so the
    /// artifact is read again.
    fn import_pass(
        &self,
        target_home: &Path,
        password: &[u8],
        setup_binding: &[u8],
        expected: &ThunderbirdSelection,
        cancel: &AtomicBool,
    ) -> io::Result<Option<ThunderbirdImportReceipt>> {
        let mut file = self.file.try_clone()?;
        file.rewind()?;
        let frame_len = read_prefix(&mut file)?;
        let mut frame = vec![0_u8; frame_len];
        file.read_exact(&mut frame)?;
        let (key, sealed_header) = MAIL_ARTIFACT
            .open(password, setup_binding, &frame)
            .map_err(frame_error)?;
        let context = super::binding(setup_binding).map_err(as_io)?;
        let plain = envelope::open(
            &key,
            Context::new(Label::MigrationPackage, &context),
            &sealed_header,
        )
        .map_err(|_| broken("the mail artifact does not open"))?;
        let header: Header = serde_json::from_slice(&plain).map_err(|_| broken("header"))?;
        if header.version != HEADER_VERSION {
            return Err(broken("the mail artifact's version is not known"));
        }
        verify_selection(&header, expected).map_err(as_io)?;
        let stream = content::OpenReader::new(&key, file).map_err(|_| broken("stream"))?;
        import_stream(target_home, &header, stream, cancel)
    }
}

/// The header frame's length, from the artifact's first bytes; leaves the reader at the frame.
fn read_prefix(reader: &mut impl Read) -> io::Result<usize> {
    let mut magic = [0_u8; MAGIC.len()];
    reader.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(broken("not a mail artifact"));
    }
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let frame = u32::from_le_bytes(length) as usize;
    if frame == 0 || frame > MAIL_ARTIFACT.max_len() {
        return Err(broken("the mail artifact's header is out of bounds"));
    }
    Ok(frame)
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary = path.to_path_buf();
    let name = path
        .file_name()
        .map_or_else(|| "mail".to_owned(), |name| name.to_string_lossy().into());
    temporary.set_file_name(format!(".{name}.next"));
    temporary
}

fn private_create(path: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// What the header names for `selection`: its accounts and folders, the files their mail is read
/// from, and the address books and calendars chosen with them.
fn build_header(
    reviewed: &ReviewedThunderbird,
    selection: &ThunderbirdSelection,
) -> Result<Header, ConnectionError> {
    if selection.accounts.is_empty() || selection.accounts.len() > MAX_ACCOUNTS {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let selected: std::collections::BTreeMap<_, _> = selection
        .accounts
        .iter()
        .map(|account| (account.source_id.as_str(), account))
        .collect();
    let mut accounts = Vec::new();
    for account in &reviewed.accounts {
        let Some(chosen) = selected.get(account.inventory.source_id.as_str()) else {
            continue;
        };
        let mut folders = Vec::new();
        for folder in &account.folders {
            if !chosen
                .folders
                .iter()
                .any(|id| id == &folder.inventory.source_id)
            {
                continue;
            }
            let files = match folder.format {
                FolderFormat::Mbox => vec![
                    folder
                        .inventory
                        .hierarchy
                        .last()
                        .cloned()
                        .unwrap_or_else(|| "mail".into()),
                ],
                FolderFormat::Maildir => vec!["maildir".into()],
            };
            folders.push(HeaderFolder {
                inventory: folder.inventory.clone(),
                files,
            });
        }
        if folders.len() > MAX_FOLDERS {
            return Err(ConnectionError::InvalidConfiguration);
        }
        accounts.push(HeaderAccount {
            inventory: account.inventory.clone(),
            metadata: account.metadata.clone(),
            folders,
        });
    }
    let used = |id: &str, list: &[String]| list.iter().any(|chosen| chosen == id);
    let book = |file: &crate::account_sources::thunderbird::ReviewedFile|
        -> Result<Option<HeaderSidecar>, ConnectionError> {
        let chosen = selection
            .accounts
            .iter()
            .any(|account| used(&file.source_id, &account.address_books));
        if !chosen {
            return Ok(None);
        }
        let bytes = read_reviewed(
            reviewed,
            file.relative.as_ref().ok_or(ConnectionError::AccountMissing)?,
            MAX_SIDECAR_BYTES,
        )
        .map_err(|_| ConnectionError::AccountMissing)?;
        Ok(Some(HeaderSidecar {
            source_id: file.source_id.clone(),
            name: file.name.clone(),
            bytes,
        }))
    };
    let calendar = |file: &crate::account_sources::thunderbird::ReviewedFile|
        -> Result<Option<HeaderSidecar>, ConnectionError> {
        let chosen = selection
            .accounts
            .iter()
            .any(|account| used(&file.source_id, &account.calendars));
        if !chosen {
            return Ok(None);
        }
        let bytes = read_reviewed(
            reviewed,
            file.relative.as_ref().ok_or(ConnectionError::AccountMissing)?,
            MAX_SIDECAR_BYTES,
        )
        .map_err(|_| ConnectionError::AccountMissing)?;
        Ok(Some(HeaderSidecar {
            source_id: file.source_id.clone(),
            name: file.name.clone(),
            bytes,
        }))
    };
    let address_books: Vec<_> = reviewed
        .address_books
        .iter()
        .filter_map(|file| book(file).transpose())
        .collect::<Result<_, _>>()?;
    let calendars: Vec<_> = reviewed
        .calendars
        .iter()
        .filter_map(|file| calendar(file).transpose())
        .collect::<Result<_, _>>()?;
    Ok(Header {
        version: HEADER_VERSION,
        profiles: vec![HeaderProfile {
            source_id: reviewed.profile.source_id.clone(),
            label: reviewed.profile.label.clone(),
            accounts,
            address_books,
            calendars,
        }],
    })
}

/// The selection the header names must be exactly the one the caller expected: a package cannot be
/// widened after it was prepared.
fn verify_selection(
    header: &Header,
    expected: &ThunderbirdSelection,
) -> Result<(), ConnectionError> {
    if expected.accounts.is_empty() || expected.accounts.len() > MAX_ACCOUNTS {
        return Err(ConnectionError::InvalidConfiguration);
    }
    let mut actual = Vec::new();
    for profile in &header.profiles {
        for account in &profile.accounts {
            let selected = expected
                .accounts
                .iter()
                .find(|selected| selected.source_id == account.inventory.source_id)
                .ok_or(ConnectionError::InvalidConfiguration)?;
            let mut folders: Vec<_> = account
                .folders
                .iter()
                .map(|folder| folder.inventory.source_id.clone())
                .collect();
            let mut expected_folders = selected.folders.clone();
            folders.sort();
            expected_folders.sort();
            if folders != expected_folders {
                return Err(ConnectionError::InvalidConfiguration);
            }
            actual.push(account.inventory.source_id.clone());
        }
    }
    let mut expected_ids: Vec<_> = expected
        .accounts
        .iter()
        .map(|account| account.source_id.clone())
        .collect();
    actual.sort();
    expected_ids.sort();
    if actual != expected_ids {
        return Err(ConnectionError::InvalidConfiguration);
    }
    Ok(())
}

/// Writes the artifact: its magic, the header frame, and the sealed entry stream.
fn write_artifact(
    reviewed: &ReviewedThunderbird,
    header: &Header,
    key: &SymmetricKey,
    cancel: &AtomicBool,
    frame: &[u8],
    path: &Path,
) -> io::Result<()> {
    reviewed.root.validate().map_err(|_| broken("profile"))?;
    let file = private_create(path)?;
    let mut out = io::BufWriter::with_capacity(mbox::CHUNK, file);
    out.write_all(MAGIC)?;
    let length = u32::try_from(frame.len()).map_err(|_| broken("header frame"))?;
    out.write_all(&length.to_le_bytes())?;
    out.write_all(frame)?;
    let entries = Entries::new(reviewed, header, cancel)?;
    let mut sealed = content::SealReader::new(key, entries).map_err(|_| broken("seal"))?;
    io::copy(&mut sealed, &mut out)?;
    out.flush()?;
    let file = out
        .into_inner()
        .map_err(|_| io::Error::other("artifact flush"))?;
    file.sync_all()?;
    reviewed.root.validate().map_err(|_| broken("profile"))?;
    Ok(())
}

/// One folder's file: the folder it belongs to, its name, and how its mail is stored.
struct FilePlan {
    folder: usize,
    folder_id: String,
    name: String,
    format: FolderFormat,
}

/// The entry stream the artifact's content carries, produced from the source as it is read.
struct Entries<'a> {
    reviewed: &'a ReviewedThunderbird,
    cancel: &'a AtomicBool,
    plans: Vec<FilePlan>,
    next: usize,
    current: Option<Reader>,
    pending: Vec<u8>,
    at: usize,
    finished: bool,
}

/// One folder's messages, read a message at a time.
enum Reader {
    Mbox {
        splitter: Box<MboxMessages<BufReader<std::fs::File>>>,
        directory: Arc<PinnedDirectory>,
        name: String,
        before: cap_primitives::fs::Metadata,
    },
    Maildir {
        parts: Vec<(String, Arc<PinnedDirectory>, Vec<String>)>,
        part: usize,
        next: usize,
    },
}

impl<'a> Entries<'a> {
    fn new(
        reviewed: &'a ReviewedThunderbird,
        header: &'a Header,
        cancel: &'a AtomicBool,
    ) -> io::Result<Self> {
        let mut plans = Vec::new();
        let mut folder_index = 0;
        for profile in &header.profiles {
            for account in &profile.accounts {
                for folder in &account.folders {
                    let reviewed = folder_by_id(reviewed, &folder.inventory.source_id)
                        .ok_or_else(|| broken("the reviewed folder is gone"))?;
                    for name in &folder.files {
                        plans.push(FilePlan {
                            folder: folder_index,
                            folder_id: folder.inventory.source_id.clone(),
                            name: name.clone(),
                            format: reviewed.format,
                        });
                    }
                    folder_index += 1;
                }
            }
        }
        if plans.len() > MAX_FILES {
            return Err(broken("too many files"));
        }
        Ok(Self {
            reviewed,
            cancel,
            plans,
            next: 0,
            current: None,
            pending: Vec::new(),
            at: 0,
            finished: false,
        })
    }

    fn advance(&mut self) -> io::Result<Option<Vec<u8>>> {
        if self.finished {
            return Ok(None);
        }
        check_cancel(self.cancel)?;
        if let Some(reader) = &mut self.current {
            if let Some((name, raw)) = reader.next_message()? {
                let folder = self.plans[self.next - 1].folder;
                let bytes = entry(ENTRY_MESSAGE, raw);
                return Ok(Some(if reader.names_messages() {
                    [begin_entry(folder, &name), bytes].concat()
                } else {
                    bytes
                }));
            }
            self.current = None;
        }
        while self.next < self.plans.len() {
            let index = self.next;
            let plan = &self.plans[index];
            let (folder, name, format, folder_id) = (
                plan.folder,
                plan.name.clone(),
                plan.format,
                plan.folder_id.clone(),
            );
            self.next += 1;
            match format {
                FolderFormat::Mbox => {
                    self.current = Some(Reader::open_mbox(self.reviewed, &folder_id)?);
                    return Ok(Some(begin_entry(folder, &name)));
                }
                FolderFormat::Maildir => {
                    let mut reader = Reader::open_maildir(self.reviewed, &folder_id)?;
                    match reader.next_message()? {
                        Some((file, raw)) => {
                            self.current = Some(reader);
                            return Ok(Some(
                                [begin_entry(folder, &file), entry(ENTRY_MESSAGE, raw)].concat(),
                            ));
                        }
                        None => continue,
                    }
                }
            }
        }
        self.finished = true;
        Ok(Some(entry(ENTRY_END, Vec::new())))
    }
}

impl Read for Entries<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.at >= self.pending.len() {
            match self.advance()? {
                Some(entry) => {
                    self.pending = entry;
                    self.at = 0;
                }
                None => return Ok(0),
            }
        }
        let ready = self.pending.get(self.at..).unwrap_or_default();
        let taken = ready.len().min(buffer.len());
        if let (Some(to), Some(from)) = (buffer.get_mut(..taken), ready.get(..taken)) {
            to.copy_from_slice(from);
        }
        self.at += taken;
        Ok(taken)
    }
}

fn folder_by_id<'a>(reviewed: &'a ReviewedThunderbird, id: &str) -> Option<&'a ReviewedFolder> {
    reviewed
        .accounts
        .iter()
        .flat_map(|account| &account.folders)
        .find(|folder| folder.inventory.source_id == id)
}

impl Reader {
    fn open_mbox(reviewed: &ReviewedThunderbird, folder_id: &str) -> io::Result<Self> {
        let folder = folder_by_id(reviewed, folder_id).ok_or_else(|| broken("folder"))?;
        let (name, parent) = folder
            .relative
            .split_last()
            .ok_or_else(|| broken("mail file"))?;
        let directory = walk(&reviewed.root, parent).map_err(|_| broken("mail folder"))?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let file = cap_primitives::fs::open(&directory.file, Path::new(name), &options)
            .map_err(|_| broken("mail file"))?;
        let before =
            metadata_nofollow(&directory.file, Path::new(name)).map_err(|_| broken("mail file"))?;
        if before.is_symlink() || !before.is_file() {
            return Err(broken("the mail file is redirected"));
        }
        Ok(Self::Mbox {
            // A message of any size comes over whole: memory follows it, as the review's count
            // already assumes.
            splitter: Box::new(MboxMessages::new(
                BufReader::with_capacity(mbox::CHUNK, file),
                usize::MAX,
            )),
            directory,
            name: name.to_owned(),
            before,
        })
    }

    fn open_maildir(reviewed: &ReviewedThunderbird, folder_id: &str) -> io::Result<Self> {
        let folder = folder_by_id(reviewed, folder_id).ok_or_else(|| broken("folder"))?;
        let directory = walk(&reviewed.root, &folder.relative).map_err(|_| broken("folder"))?;
        let mut parts = Vec::new();
        for part in ["cur", "new"] {
            let Ok(part_directory) = walk(&directory, std::slice::from_ref(&part.to_owned()))
            else {
                continue;
            };
            let mut names = Vec::new();
            for (name, info) in entries(&part_directory).map_err(|_| broken("part"))? {
                if info.is_file() && !info.is_symlink() && !name.starts_with('.') {
                    names.push(name);
                }
            }
            names.sort();
            parts.push((part.to_owned(), part_directory, names));
        }
        Ok(Self::Maildir {
            parts,
            part: 0,
            next: 0,
        })
    }

    /// The next message, and the name of the file it was stored in (a maildir's message file, or
    /// none for an mbox).
    /// Whether each message names the file it was stored in (a maildir's message file).
    fn names_messages(&self) -> bool {
        matches!(self, Self::Maildir { .. })
    }

    fn next_message(&mut self) -> io::Result<Option<(String, Vec<u8>)>> {
        match self {
            Self::Mbox {
                splitter,
                directory,
                name,
                before,
            } => match splitter.next_message() {
                Ok(Some(raw)) => Ok(Some((name.clone(), raw))),
                Ok(None) => {
                    let after = metadata_nofollow(&directory.file, Path::new(name))
                        .map_err(|_| broken("mail file"))?;
                    if !same_snapshot(before, &after) {
                        return Err(broken("the mail file changed while it was read"));
                    }
                    directory.validate().map_err(|_| broken("mail folder"))?;
                    Ok(None)
                }
                Err(error) => Err(error),
            },
            Self::Maildir { parts, part, next } => loop {
                let Some((part_name, directory, names)) = parts.get(*part) else {
                    return Ok(None);
                };
                let Some(name) = names.get(*next) else {
                    *part += 1;
                    *next = 0;
                    continue;
                };
                *next += 1;
                let raw = read_file(directory, name, u64::MAX)
                    .map_err(|_| broken("a message could not be read"))?;
                return Ok(Some((format!("{part_name}/{name}"), raw)));
            },
        }
    }
}

fn begin_entry(folder: usize, file: &str) -> Vec<u8> {
    let index = u32::try_from(folder).unwrap_or(u32::MAX);
    let mut payload = Vec::with_capacity(4 + file.len());
    payload.extend_from_slice(&index.to_le_bytes());
    payload.extend_from_slice(file.as_bytes());
    entry(ENTRY_BEGIN_FILE, payload)
}

fn entry(kind: u8, payload: Vec<u8>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(5 + payload.len());
    bytes.push(kind);
    let length = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&payload);
    bytes
}

/// Imports the streamed messages into the target's Mail accounts and cache. Nothing is published
/// until the artifact has been read whole and every folder's count is as reviewed.
fn import_stream(
    target_home: &Path,
    header: &Header,
    mut stream: content::OpenReader<std::fs::File>,
    cancel: &AtomicBool,
) -> io::Result<Option<ThunderbirdImportReceipt>> {
    let root = target_root(target_home).map_err(|_| io::Error::other("target"))?;
    let record =
        crate::account_store::Accounts::read(&root).map_err(|_| io::Error::other("store"))?;
    let cache = ensure_dir(target_home, &[".cache", MAIL_APP, "mail"]).map_err(as_io)?;
    let retained = ensure_dir(target_home, &RETAINED).map_err(as_io)?;
    let mut receipt = ThunderbirdImportReceipt {
        accounts: Vec::new(),
        address_books: 0,
        calendars: 0,
    };
    let profile = header
        .profiles
        .first()
        .ok_or_else(|| broken("the mail artifact holds no profile"))?;
    let mut accounts = Vec::with_capacity(profile.accounts.len());
    for account in &profile.accounts {
        let (id, created) =
            publish_account(&record, &account.inventory, &account.metadata).map_err(as_io)?;
        let stem = file_stem(id.as_str());
        let files = cache.join(format!("{stem}.files"));
        let snapshot = read_private(&cache_path(&cache, &id), MAX_CACHE).map_err(as_io)?;
        let base = u64::try_from(snapshot.as_ref().map_or(0, Vec::len)).unwrap_or(u64::MAX);
        let kept = parse_kept(snapshot.as_deref()).map_err(as_io)?;
        let seen: BTreeSet<_> = kept.messages.iter().map(|message| message.id).collect();
        let spool = Spool::create(&cache, base)?;
        accounts.push(AccountImport {
            id,
            created,
            needs_reconnect: account.inventory.needs_reconnect,
            source_id: account.inventory.source_id.clone(),
            inventory: account.inventory.clone(),
            files,
            seen,
            spool,
            folder_messages: vec![0; account.folders.len()],
            folder_read: vec![0; account.folders.len()],
            cache: cache.clone(),
        });
    }
    let mut current: Option<CurrentFile> = None;
    let mut finished = false;
    loop {
        check_cancel(cancel)?;
        let Some((kind, payload)) = next_entry(&mut stream)? else {
            break;
        };
        match kind {
            ENTRY_BEGIN_FILE => {
                if let Some(file) = current.take() {
                    file.finish()?;
                }
                let (folder, name) = parse_begin(&payload)?;
                let (account_index, folder_index, folder) = folder_at(profile, folder)?;
                let directory = retained_folder(
                    &retained,
                    &profile.source_id,
                    &profile.accounts[account_index].inventory.source_id,
                    &folder.inventory.hierarchy,
                )?;
                let maildir = folder.inventory.format != "mbox";
                let retention = if maildir {
                    None
                } else {
                    Some(Retention::create(
                        directory.join(format!("{}.mbox", component(&name))),
                    )?)
                };
                current = Some(CurrentFile {
                    account: account_index,
                    folder: folder_index,
                    name,
                    maildir,
                    retention,
                    directory,
                });
            }
            ENTRY_MESSAGE => {
                let file = current
                    .as_mut()
                    .ok_or_else(|| broken("a message before its file"))?;
                let account = &mut accounts[file.account];
                if let Some(read) = account.folder_read.get_mut(file.folder) {
                    *read = read.saturating_add(1);
                }
                let folder = &profile.accounts[file.account].folders[file.folder].inventory;
                let flags = file.maildir.then(|| maildir_read_flags(&file.name));
                let parsed = message(&payload, folder, &account.inventory, flags).map_err(as_io)?;
                if let Some(retention) = file.retention.as_mut() {
                    retention.write(&payload)?;
                } else {
                    let mut name = file.name.splitn(2, '/');
                    let part = component(name.next().unwrap_or("cur"));
                    let message = component(name.next().unwrap_or("message"));
                    let part = file.directory.join(part);
                    ensure_private_dir(&part).map_err(as_io)?;
                    write_file(&part.join(message), &payload).map_err(as_io)?;
                }
                let Some((message, attachments)) = parsed else {
                    continue;
                };
                if !account.seen.insert(message.id) {
                    continue;
                }
                for (index, bytes) in attachments {
                    write_file(
                        &account.files.join(format!("{}.{index}", message.id)),
                        &bytes,
                    )
                    .map_err(as_io)?;
                }
                if let Some(count) = account.folder_messages.get_mut(file.folder) {
                    *count = count.saturating_add(1);
                }
                account.spool.push(&message)?;
            }
            ENTRY_END => finished = true,
            _ => return Err(broken("an unknown entry")),
        }
    }
    if let Some(file) = current.take() {
        file.finish()?;
    }
    if !finished {
        return Err(broken("the mail artifact is cut short"));
    }
    for (account, header) in accounts.iter_mut().zip(&profile.accounts) {
        for (count, folder) in account.folder_read.iter().zip(&header.folders) {
            if let Some(reviewed) = folder
                .inventory
                .messages
                .filter(|reviewed| reviewed != count)
            {
                return Err(broken(&format!(
                    "a selected mail folder changed since it was reviewed: {} reviewed {} imported {}",
                    folder.inventory.hierarchy.join("/"),
                    reviewed,
                    count
                )));
            }
        }
        if account.folder_messages.iter().all(|count| *count == 0) {
            continue;
        }
        if !publish_account_mail(account)? {
            return Ok(None);
        }
    }
    for book in &profile.address_books {
        retain_sidecar(&retained, &profile.source_id, "address-books", book)?;
        super::personal_data::stage(
            target_home,
            &profile.source_id,
            &book.source_id,
            super::personal_data::Sidecar::AddressBook,
            &book.bytes,
        )?;
        receipt.address_books = receipt.address_books.saturating_add(1);
    }
    for calendar in &profile.calendars {
        retain_sidecar(&retained, &profile.source_id, "calendars", calendar)?;
        super::personal_data::stage(
            target_home,
            &profile.source_id,
            &calendar.source_id,
            super::personal_data::Sidecar::Calendar,
            &calendar.bytes,
        )?;
        receipt.calendars = receipt.calendars.saturating_add(1);
    }
    for (account, header) in accounts.into_iter().zip(&profile.accounts) {
        let folders = header
            .folders
            .iter()
            .zip(&account.folder_messages)
            .map(|(folder, messages)| ThunderbirdFolderReceipt {
                source_id: folder.inventory.source_id.clone(),
                messages: *messages,
                bytes: folder.inventory.bytes,
                local_only: folder.inventory.local_only,
            })
            .collect();
        receipt.accounts.push(ThunderbirdAccountReceipt {
            source_id: account.source_id,
            created: account.created,
            needs_reconnect: account.needs_reconnect,
            folders,
        });
    }
    record
        .project(&root)
        .map_err(|_| io::Error::other("store"))?;
    Ok(Some(receipt))
}

struct AccountImport {
    id: AccountId,
    created: bool,
    needs_reconnect: bool,
    source_id: String,
    inventory: ThunderbirdAccount,
    files: PathBuf,
    seen: BTreeSet<uuid::Uuid>,
    /// Where this pass's messages wait, out of memory, until they are merged into the kept file.
    spool: Spool,
    /// Messages the artifact held for each folder, as reviewed.
    folder_read: Vec<u64>,
    /// Messages this import added to each folder.
    folder_messages: Vec<u64>,
    cache: PathBuf,
}

/// Merges this pass's messages into the account's kept mail; `false` when another writer changed
/// that file under the pass, so the artifact is read again.
fn publish_account_mail(account: &AccountImport) -> io::Result<bool> {
    let path = cache_path(&account.cache, &account.id);
    let snapshot = read_private(&path, MAX_CACHE).map_err(as_io)?;
    let kept = parse_kept(snapshot.as_deref()).map_err(as_io)?;
    match publish_merge(&path, snapshot.as_deref(), &kept, &account.spool) {
        Ok(()) => Ok(true),
        Err(MergeError::Conflict) => Ok(false),
        Err(MergeError::TooLarge) => Err(io::Error::other(
            "this account's local mail would exceed the Mail app's bound",
        )),
        Err(MergeError::Failed(error)) => Err(error),
    }
}

/// The file whose messages are being imported, and where its copy is kept.
struct CurrentFile {
    account: usize,
    folder: usize,
    name: String,
    maildir: bool,
    retention: Option<Retention>,
    directory: PathBuf,
}

impl CurrentFile {
    fn finish(self) -> io::Result<()> {
        match self.retention {
            Some(retention) => retention.finish(),
            None => Ok(()),
        }
    }
}

fn next_entry(stream: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0_u8; 5];
    let mut filled = 0;
    while filled < head.len() {
        match stream.read(head.get_mut(filled..).unwrap_or_default()) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(broken("the mail artifact is cut short")),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    let kind = head[0];
    let length = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
    if length > MAX_ENTRY {
        return Err(broken("a mail entry is larger than its bound"));
    }
    let mut payload = vec![0_u8; length as usize];
    stream.read_exact(&mut payload)?;
    Ok(Some((kind, payload)))
}

fn parse_begin(payload: &[u8]) -> io::Result<(u32, String)> {
    let (index, name) = payload.split_at_checked(4).ok_or_else(|| broken("file"))?;
    let folder = u32::from_le_bytes([index[0], index[1], index[2], index[3]]);
    let name = std::str::from_utf8(name).map_err(|_| broken("file name"))?;
    Ok((folder, name.to_owned()))
}

fn folder_at(profile: &HeaderProfile, index: u32) -> io::Result<(usize, usize, &HeaderFolder)> {
    let mut seen = 0_u32;
    for (account_index, account) in profile.accounts.iter().enumerate() {
        for (folder_index, folder) in account.folders.iter().enumerate() {
            if seen == index {
                return Ok((account_index, folder_index, folder));
            }
            seen += 1;
        }
    }
    Err(broken("a file names no folder"))
}

fn retained_folder(
    root: &Path,
    profile: &str,
    account: &str,
    hierarchy: &[String],
) -> io::Result<PathBuf> {
    let mut directory = root.join(component(profile)).join(component(account));
    for name in hierarchy {
        directory.push(component(name));
    }
    ensure_private_dir(&directory).map_err(as_io)?;
    Ok(directory)
}

fn retain_sidecar(root: &Path, profile: &str, kind: &str, file: &HeaderSidecar) -> io::Result<()> {
    let directory = root
        .join(component(profile))
        .join(kind)
        .join(component(&file.source_id));
    ensure_private_dir(&directory).map_err(as_io)?;
    write_file(&directory.join(component(&file.name)), &file.bytes)
        .map_err(|_| io::Error::other("a retained sidecar"))
}
