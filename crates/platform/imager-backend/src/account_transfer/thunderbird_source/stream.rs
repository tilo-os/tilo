//! Streamed import of a reviewed Thunderbird source into the destination
//! canonical Mail account store and cache.
//!
//! The same-computer route keeps the source Home and reads it directly after
//! first boot. Building one JSON bundle of every message would hold the whole
//! mailbox, and its ciphertext, in memory before anything is published; an
//! ordinary account of a few thousand messages already exceeds that. This
//! module reads a folder in fixed chunks, publishes one message at a time, and
//! keeps only the per-account kept file the Mail app itself reads. The sealed
//! artifact in the parent module stays the route for a transfer that has to
//! pass through installation media.

use std::collections::BTreeSet;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use accounts::AccountStore;
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use uuid::Uuid;

use crate::account_sources::thunderbird::{
    FolderKind, MAX_MESSAGE_BYTES, MboxMessages, ReviewedAccount, ReviewedFolder,
    ReviewedThunderbird, ThunderbirdAccount, entries, maildir_read_flags, mbox, read_file,
    read_reviewed, walk,
};
use crate::migration_directory::same_snapshot;
use crate::nofollow::metadata_nofollow;

use super::{
    CACHE_ATTEMPTS, CACHE_VERSION, CachedMessage, ConnectionError, Kept, MAIL_APP, MAX_CACHE,
    ThunderbirdAccountReceipt, ThunderbirdAccountSelection, ThunderbirdFolderReceipt,
    ThunderbirdImportReceipt, ThunderbirdSelection, cache_path, component, ensure_dir,
    ensure_private_dir, file_stem, message, parse_kept, publish_account, read_private, target_root,
    write_file,
};

/// The most selected source mail one streamed import walks. The bound is
/// checked before the first read, so an implausible selection is refused by
/// name instead of copying for hours. It bounds the work, not the kept JSON:
/// that is checked as the kept file grows.
const MAX_SOURCE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// The most messages one streamed import walks, as a whole and as the
/// reviewed inventory already knows them.
const MAX_SOURCE_MESSAGES: u64 = 1_000_000;
/// One address book or calendar file. These are small sidecars, unlike mail.
const MAX_SIDECAR_BYTES: u64 = 32 * 1024 * 1024;
/// The destination directory below the target Home that keeps each selected
/// folder's exact original bytes.
const RETAINED: [&str; 5] = [".local", "share", "tilo", "mail-import", "thunderbird"];

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn storage(message: &str) -> io::Error {
    io::Error::other(message)
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "migration cancelled")
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(cancelled())
    } else {
        Ok(())
    }
}

/// A message over the per-message bound. Its folder is named by its
/// Thunderbird path, which the person already sees in the review.
fn folder_too_large(folder: &ReviewedFolder) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "A message in the {} folder is larger than the {} MiB bound for one message, so nothing was imported.",
            folder.inventory.hierarchy.join("/"),
            MAX_MESSAGE_BYTES / (1024 * 1024)
        ),
    )
}

/// The account's kept file would grow past what the Mail app reads whole.
fn kept_too_large() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "This account's local mail would exceed the {} MiB the Mail app keeps for one account, so nothing was imported.",
            MAX_CACHE / (1024 * 1024)
        ),
    )
}

/// What one streamed import held at once. It is the evidence that a folder
/// larger than memory is read as a stream rather than as one buffer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StreamReport {
    pub peak_message_bytes: usize,
    pub peak_buffered_bytes: usize,
    pub messages: u64,
}

/// Import exactly the reviewed selection into the canonical Mail accounts and
/// cache below `target_home`, reading the retained source directly.
///
/// The source is the same pinned profile discovery reviewed: every folder file
/// is opened without following links, read in chunks, and checked against its
/// size at review and against itself after the read, so a source that changed
/// under the import is refused before anything is published.
///
/// # Errors
/// Refuses a selection that differs from the reviewed source, a changed or
/// redirected source, a message beyond the per-message bound, a kept file
/// beyond the Mail app's bound, an unavailable target, and cancellation.
pub fn import_reviewed(
    target_home: &Path,
    reviewed: &ReviewedThunderbird,
    selection: &ThunderbirdSelection,
    setup_binding: &[u8],
    cancel: &AtomicBool,
) -> io::Result<ThunderbirdImportReceipt> {
    let (receipt, report) = import_with(
        target_home,
        reviewed,
        selection,
        setup_binding,
        cancel,
        &mut |_: u64| {},
    )?;
    if report.peak_message_bytes > MAX_MESSAGE_BYTES {
        return Err(invalid("A mail message was held beyond its bound"));
    }
    Ok(receipt)
}

/// [`import_reviewed`], reporting what it held and observing each message, so a
/// test can prove the memory bound and stop a long import part way.
fn import_with(
    target_home: &Path,
    reviewed: &ReviewedThunderbird,
    selection: &ThunderbirdSelection,
    setup_binding: &[u8],
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> io::Result<(ThunderbirdImportReceipt, StreamReport)> {
    if super::binding(setup_binding).is_err() {
        return Err(invalid("Selected mail has no operation binding"));
    }
    selection
        .validate(reviewed)
        .map_err(|_| invalid("Selected mail differs from its reviewed source"))?;
    check_cancel(cancel)?;
    let budget = Budget::for_selection(reviewed, selection)?;
    reviewed
        .root
        .validate()
        .map_err(|_| invalid("The source profile changed since it was reviewed"))?;
    let root =
        target_root(target_home).map_err(|_| storage("The target account store is unavailable"))?;
    let mut store = super::super::connect::load_store(&root)
        .map_err(|_| storage("The target account store is unavailable"))?;
    let cache = ensure_dir(target_home, &[".cache", MAIL_APP, "mail"])
        .map_err(|_| storage("The target Mail cache is unavailable"))?;
    let retained = ensure_dir(target_home, &RETAINED)
        .map_err(|_| storage("The target mail copy area is unavailable"))?;
    let mut import = Import {
        reviewed,
        cache: &cache,
        retained: &retained,
        cancel,
        budget,
        report: StreamReport::default(),
    };
    let mut receipt = ThunderbirdImportReceipt {
        accounts: Vec::new(),
        address_books: 0,
        calendars: 0,
    };
    for account in &reviewed.accounts {
        let Some(selected) = selection
            .accounts
            .iter()
            .find(|selected| selected.source_id == account.inventory.source_id)
        else {
            continue;
        };
        check_cancel(cancel)?;
        let imported = import.account(account, selected, &mut store, progress)?;
        receipt.accounts.push(imported);
    }
    retain_sidecars(
        target_home,
        reviewed,
        selection,
        &retained,
        &mut receipt,
        cancel,
    )?;
    reviewed
        .root
        .validate()
        .map_err(|_| invalid("The source profile changed while its mail was imported"))?;
    store
        .save_in(&root)
        .map_err(|_| storage("The target account store could not be saved"))?;
    Ok((receipt, import.report))
}

/// Keep the selected address books and calendars byte for byte beside the
/// imported mail. They are small files, so each is read bounded and whole.
fn retain_sidecars(
    target_home: &Path,
    reviewed: &ReviewedThunderbird,
    selection: &ThunderbirdSelection,
    retained: &Path,
    receipt: &mut ThunderbirdImportReceipt,
    cancel: &AtomicBool,
) -> io::Result<()> {
    for book in &reviewed.address_books {
        if !selection
            .accounts
            .iter()
            .any(|account| account.address_books.iter().any(|id| id == &book.source_id))
        {
            continue;
        }
        check_cancel(cancel)?;
        let relative = book
            .relative
            .as_ref()
            .ok_or_else(|| invalid("A selected address book is no longer present"))?;
        let bytes = read_reviewed(reviewed, relative, MAX_SIDECAR_BYTES)
            .map_err(|_| invalid("A selected address book could not be read"))?;
        retain_sidecar(
            retained,
            &reviewed.profile.source_id,
            "address-books",
            &book.source_id,
            &book.name,
            &bytes,
        )?;
        super::personal_data::stage(
            target_home,
            &reviewed.profile.source_id,
            &book.source_id,
            super::personal_data::Sidecar::AddressBook,
            &bytes,
        )?;
        receipt.address_books = receipt.address_books.saturating_add(1);
    }
    for calendar in &reviewed.calendars {
        if !selection
            .accounts
            .iter()
            .any(|account| account.calendars.iter().any(|id| id == &calendar.source_id))
        {
            continue;
        }
        check_cancel(cancel)?;
        let relative = calendar
            .relative
            .as_ref()
            .ok_or_else(|| invalid("A selected calendar is no longer present"))?;
        let bytes = read_reviewed(reviewed, relative, MAX_SIDECAR_BYTES)
            .map_err(|_| invalid("A selected calendar could not be read"))?;
        retain_sidecar(
            retained,
            &reviewed.profile.source_id,
            "calendars",
            &calendar.source_id,
            &calendar.name,
            &bytes,
        )?;
        super::personal_data::stage(
            target_home,
            &reviewed.profile.source_id,
            &calendar.source_id,
            super::personal_data::Sidecar::Calendar,
            &bytes,
        )?;
        receipt.calendars = receipt.calendars.saturating_add(1);
    }
    Ok(())
}

/// The work a selection may make one import do, computed from the reviewed
/// folder inventory before the source is read.
struct Budget {
    messages: u64,
}

impl Budget {
    fn for_selection(
        reviewed: &ReviewedThunderbird,
        selection: &ThunderbirdSelection,
    ) -> io::Result<Self> {
        let mut bytes = 0_u64;
        let mut messages = 0_u64;
        for account in &reviewed.accounts {
            let Some(selected) = selection
                .accounts
                .iter()
                .find(|selected| selected.source_id == account.inventory.source_id)
            else {
                continue;
            };
            for folder in &account.folders {
                if !selected
                    .folders
                    .iter()
                    .any(|id| id == &folder.inventory.source_id)
                {
                    continue;
                }
                bytes = bytes.saturating_add(folder.inventory.bytes);
                messages = messages.saturating_add(folder.inventory.messages.unwrap_or(0));
            }
        }
        if bytes > MAX_SOURCE_BYTES || messages > MAX_SOURCE_MESSAGES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "This selection copies more than {} GiB of local mail, the most one import moves.",
                    MAX_SOURCE_BYTES / (1024 * 1024 * 1024)
                ),
            ));
        }
        Ok(Self { messages })
    }

    fn count(&mut self) -> io::Result<()> {
        self.messages = self.messages.saturating_add(1);
        if self.messages > MAX_SOURCE_MESSAGES {
            return Err(invalid(
                "This selection has more messages than one import moves.",
            ));
        }
        Ok(())
    }
}

/// One parsed message and the attachment bytes that belong beside the kept
/// file.
type ParsedMessage = (CachedMessage, Vec<(usize, Vec<u8>)>);

struct Import<'a> {
    reviewed: &'a ReviewedThunderbird,
    cache: &'a Path,
    retained: &'a Path,
    cancel: &'a AtomicBool,
    budget: Budget,
    report: StreamReport,
}

impl Import<'_> {
    /// Publish one account's configuration, then merge its selected folders
    /// into the account's kept mail. The store is only saved by the caller, so
    /// an account is never published without the mail that was selected with
    /// it.
    fn account(
        &mut self,
        account: &ReviewedAccount,
        selected: &ThunderbirdAccountSelection,
        store: &mut AccountStore,
        progress: &mut dyn FnMut(u64),
    ) -> io::Result<ThunderbirdAccountReceipt> {
        let (id, created) = publish_account(store, &account.inventory, &account.metadata)
            .map_err(|_| storage("A selected mail account could not be published"))?;
        let stem = file_stem(id.as_str());
        let files = self.cache.join(format!("{stem}.files"));
        for _ in 0..CACHE_ATTEMPTS {
            check_cancel(self.cancel)?;
            let path = cache_path(self.cache, &id);
            let snapshot = read_private(&path, MAX_CACHE)
                .map_err(|_| storage("The kept mail for this account is unreadable"))?;
            let kept = parse_kept(snapshot.as_deref())
                .map_err(|_| storage("The kept mail for this account is unreadable"))?;
            let mut seen: BTreeSet<Uuid> = kept.messages.iter().map(|message| message.id).collect();
            let mut spool = Spool::create(
                self.cache,
                u64::try_from(snapshot.as_ref().map_or(0, Vec::len)).unwrap_or(u64::MAX),
            )?;
            let mut folders = Vec::new();
            {
                let mut sink = FolderSink {
                    files: &files,
                    spool: &mut spool,
                    seen: &mut seen,
                    progress: &mut *progress,
                };
                for folder in &account.folders {
                    if !selected
                        .folders
                        .iter()
                        .any(|id| id == &folder.inventory.source_id)
                    {
                        continue;
                    }
                    check_cancel(self.cancel)?;
                    let retained = retained_folder(
                        self.retained,
                        &self.reviewed.profile.source_id,
                        &account.inventory.source_id,
                        &folder.inventory.hierarchy,
                    )?;
                    let inserted =
                        self.stream_folder(folder, &account.inventory, &mut sink, &retained)?;
                    folders.push(ThunderbirdFolderReceipt {
                        source_id: folder.inventory.source_id.clone(),
                        messages: inserted,
                        bytes: folder.inventory.bytes,
                        local_only: folder.inventory.local_only,
                    });
                }
            }
            let imported = spool.messages;
            if imported == 0 {
                // Every selected message is already kept: leave the person's
                // file, and its flags, exactly as they are.
                return Ok(ThunderbirdAccountReceipt {
                    source_id: account.inventory.source_id.clone(),
                    created,
                    needs_reconnect: account.inventory.needs_reconnect,
                    folders,
                });
            }
            match publish_merge(&path, snapshot.as_deref(), &kept, &spool) {
                Ok(()) => {
                    return Ok(ThunderbirdAccountReceipt {
                        source_id: account.inventory.source_id.clone(),
                        created,
                        needs_reconnect: account.inventory.needs_reconnect,
                        folders,
                    });
                }
                Err(MergeError::Conflict) => {}
                Err(MergeError::TooLarge) => return Err(kept_too_large()),
                Err(MergeError::Failed(error)) => return Err(error),
            }
        }
        Err(storage(
            "The kept mail for this account kept changing while it was written",
        ))
    }

    fn stream_folder(
        &mut self,
        folder: &ReviewedFolder,
        inventory: &ThunderbirdAccount,
        sink: &mut FolderSink<'_>,
        retained: &Path,
    ) -> io::Result<u64> {
        match folder.kind {
            FolderKind::Mbox => self.stream_mbox(folder, inventory, sink, retained),
            FolderKind::Maildir => self.stream_maildir(folder, inventory, sink, retained),
        }
    }

    /// One mbox, read from its pinned profile handle in chunks. The exact
    /// original bytes are copied to the retained area as they are read, so the
    /// folder is opened once.
    fn stream_mbox(
        &mut self,
        folder: &ReviewedFolder,
        inventory: &ThunderbirdAccount,
        sink: &mut FolderSink<'_>,
        retained: &Path,
    ) -> io::Result<u64> {
        let (name, parent) = folder
            .relative
            .split_last()
            .ok_or_else(|| invalid("A selected mail folder has no name"))?;
        let directory = walk(&self.reviewed.root, parent)?;
        let path = Path::new(name);
        let before = metadata_nofollow(&directory.file, path)?;
        if before.is_symlink() || !before.is_file() {
            return Err(invalid("A selected mail file is redirected or not a file"));
        }
        if before.len() != folder.inventory.bytes {
            return Err(invalid(
                "A selected mail folder changed since it was reviewed",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let file = cap_primitives::fs::open(&directory.file, path, &options)?;
        let retained_name = folder
            .inventory
            .hierarchy
            .last()
            .map_or_else(|| "mail".to_owned(), |name| component(name));
        let mut retention = Retention::create(retained.join(format!("{retained_name}.mbox")))?;
        let mut inserted = 0_u64;
        let mut streamed = 0_u64;
        {
            let reader = BufReader::with_capacity(
                mbox::CHUNK,
                Tee {
                    inner: file,
                    out: &mut retention,
                    cancel: self.cancel,
                },
            );
            let mut splitter = MboxMessages::new(reader, MAX_MESSAGE_BYTES);
            loop {
                check_cancel(self.cancel)?;
                match splitter.next_message() {
                    Ok(Some(raw)) => {
                        self.budget.count()?;
                        streamed = streamed.saturating_add(1);
                        self.report.messages = self.report.messages.saturating_add(1);
                        self.report.peak_message_bytes = self
                            .report
                            .peak_message_bytes
                            .max(splitter.max_message_bytes());
                        self.report.peak_buffered_bytes =
                            self.report.peak_buffered_bytes.max(splitter.max_buffered());
                        (sink.progress)(self.report.messages);
                        let parsed = message(&raw, &folder.inventory, inventory, None)
                            .map_err(|_| invalid("A selected mail message could not be read"))?;
                        if sink.insert(parsed)? {
                            inserted = inserted.saturating_add(1);
                        }
                    }
                    Ok(None) => break,
                    Err(error) if mbox::is_too_large(&error) => {
                        return Err(folder_too_large(folder));
                    }
                    Err(error) => return Err(error),
                }
            }
            self.report.peak_message_bytes = self
                .report
                .peak_message_bytes
                .max(splitter.max_message_bytes());
            self.report.peak_buffered_bytes =
                self.report.peak_buffered_bytes.max(splitter.max_buffered());
        }
        // Revalidate the source before the copy is published, so an import
        // that saw the folder change leaves nothing behind. The file length
        // and the message count the person reviewed must both still hold.
        if folder
            .inventory
            .messages
            .is_some_and(|count| count != streamed)
        {
            return Err(invalid(
                "A selected mail folder changed since it was reviewed",
            ));
        }
        let after = metadata_nofollow(&directory.file, path)?;
        if !same_snapshot(&before, &after) {
            return Err(invalid("A selected mail folder changed while it was read"));
        }
        directory.validate()?;
        self.reviewed
            .root
            .validate()
            .map_err(|_| invalid("The source profile changed while it was read"))?;
        retention.finish()?;
        Ok(inserted)
    }

    /// One maildir. Every message is its own bounded file, so the folder is
    /// walked in name order and each file is read once.
    fn stream_maildir(
        &mut self,
        folder: &ReviewedFolder,
        inventory: &ThunderbirdAccount,
        sink: &mut FolderSink<'_>,
        retained: &Path,
    ) -> io::Result<u64> {
        let directory = walk(&self.reviewed.root, &folder.relative)?;
        let mut inserted = 0_u64;
        let mut streamed = 0_u64;
        for part in ["cur", "new"] {
            let part = part.to_string();
            let Ok(current) = walk(&directory, std::slice::from_ref(&part)) else {
                continue;
            };
            for (name, info) in entries(&current)? {
                if !info.is_file() || info.is_symlink() || name.starts_with('.') {
                    continue;
                }
                check_cancel(self.cancel)?;
                let raw = read_file(
                    &current,
                    &name,
                    u64::try_from(MAX_MESSAGE_BYTES).unwrap_or(u64::MAX),
                )
                .map_err(|error| {
                    if mbox::is_too_large(&error) {
                        folder_too_large(folder)
                    } else {
                        invalid("A selected mail message could not be read")
                    }
                })?;
                self.budget.count()?;
                streamed = streamed.saturating_add(1);
                self.report.messages = self.report.messages.saturating_add(1);
                if sink.insert(
                    message(
                        &raw,
                        &folder.inventory,
                        inventory,
                        Some(maildir_read_flags(&name)),
                    )
                    .map_err(|_| invalid("A selected mail message could not be read"))?,
                )? {
                    inserted = inserted.saturating_add(1);
                }
                let retained_part = retained.join(component(&part));
                ensure_private_dir(&retained_part)
                    .map_err(|_| storage("The target mail copy area is unavailable"))?;
                write_file(&retained_part.join(component(&name)), &raw)
                    .map_err(|_| storage("A selected message could not be kept"))?;
                (sink.progress)(self.report.messages);
            }
        }
        if folder
            .inventory
            .messages
            .is_some_and(|count| count != streamed)
        {
            return Err(invalid(
                "A selected mail folder changed since it was reviewed",
            ));
        }
        self.reviewed
            .root
            .validate()
            .map_err(|_| invalid("The source profile changed while it was read"))?;
        Ok(inserted)
    }
}

/// The messages of one folder that are new to the account.
struct FolderSink<'a> {
    files: &'a Path,
    spool: &'a mut Spool,
    seen: &'a mut BTreeSet<Uuid>,
    progress: &'a mut dyn FnMut(u64),
}

impl FolderSink<'_> {
    /// Keep a parsed message once. Its attachment bytes are written only when
    /// the message is actually new, so an existing kept copy keeps its state.
    fn insert(&mut self, parsed: Option<ParsedMessage>) -> io::Result<bool> {
        let Some((message, attachments)) = parsed else {
            return Ok(false);
        };
        if !self.seen.insert(message.id) {
            return Ok(false);
        }
        for (index, bytes) in attachments {
            write_file(&self.files.join(format!("{}.{index}", message.id)), &bytes).map_err(
                |error| match error {
                    ConnectionError::AlreadyPresent => io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "An imported attachment differs from the one already kept",
                    ),
                    _ => storage("An imported attachment could not be kept"),
                },
            )?;
        }
        self.spool.push(&message)?;
        Ok(true)
    }
}

/// Streams the bytes it reads into a retained copy, so one pass over the
/// folder both imports and preserves it. Cancellation is observed on every
/// chunk, so stopping a long folder does not wait for a whole message.
struct Tee<'a> {
    inner: std::fs::File,
    out: &'a mut Retention,
    cancel: &'a AtomicBool,
}

impl Read for Tee<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        check_cancel(self.cancel)?;
        let read = self.inner.read(buffer)?;
        if let Some(bytes) = buffer.get(..read) {
            self.out.write(bytes)?;
        }
        Ok(read)
    }
}

/// The exact bytes of one source file, held beside the target until the file
/// has been read completely and its source identity checks out.
struct Retention {
    target: PathBuf,
    temporary: PathBuf,
    file: Option<std::fs::File>,
}

impl Retention {
    fn create(target: PathBuf) -> io::Result<Self> {
        let parent = target
            .parent()
            .ok_or_else(|| invalid("A retained mail copy needs a directory"))?;
        ensure_private_dir(parent)
            .map_err(|_| storage("The target mail copy area is unavailable"))?;
        let temporary = parent.join(format!(".tilo-{}.tmp", Uuid::new_v4()));
        let file = private_create(&temporary)?;
        Ok(Self {
            target,
            temporary,
            file: Some(file),
        })
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        match &mut self.file {
            Some(file) => file.write_all(bytes),
            None => Err(storage("A retained mail copy was already closed")),
        }
    }

    /// Publish the copy, refusing to replace one that holds other bytes: that
    /// copy may be mail someone still needs.
    fn finish(mut self) -> io::Result<()> {
        let Some(file) = self.file.take() else {
            return Err(storage("A retained mail copy was already closed"));
        };
        file.sync_all()?;
        drop(file);
        match std::fs::symlink_metadata(&self.target) {
            Ok(info) if info.is_file() && !info.is_symlink() => {
                if !same_bytes(&self.target, &self.temporary)? {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "A retained mail copy already holds other bytes",
                    ));
                }
                std::fs::remove_file(&self.temporary)?;
                Ok(())
            }
            Ok(_) => Err(invalid("A retained mail copy was replaced")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::fs::rename(&self.temporary, &self.target)
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for Retention {
    fn drop(&mut self) {
        self.file = None;
        // The name is unique to this copy, and a published copy was renamed
        // away, so this only ever removes the copy that was not published.
        let _ = std::fs::remove_file(&self.temporary);
    }
}

/// Messages already converted to the kept wire shape, held in a private
/// temporary file so a long folder is not kept in memory. The file only holds
/// the JSON array elements of the new messages.
struct Spool {
    path: PathBuf,
    out: Counted<std::fs::File>,
    first: bool,
    messages: u64,
    base: u64,
}

impl Spool {
    /// `base` is the size of the kept file that was read, so the destination
    /// bound is checked while the new messages are written, not after.
    fn create(directory: &Path, base: u64) -> io::Result<Self> {
        let path = directory.join(format!(".tilo-{}.spool", Uuid::new_v4()));
        let out = Counted::create(&path)?;
        Ok(Self {
            path,
            out,
            first: true,
            messages: 0,
            base,
        })
    }

    fn push(&mut self, message: &CachedMessage) -> io::Result<()> {
        if !self.first {
            self.out.write_all(b",")?;
        }
        serde_json::to_writer(&mut self.out, message)
            .map_err(|_| invalid("A mail message could not be written"))?;
        self.first = false;
        self.messages = self.messages.saturating_add(1);
        if self.base.saturating_add(self.out.bytes) > MAX_CACHE {
            return Err(kept_too_large());
        }
        Ok(())
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

enum MergeError {
    /// Another writer changed the kept file after it was read.
    Conflict,
    TooLarge,
    Failed(io::Error),
}

/// Write the person's kept mail plus the new messages to a temporary file and
/// put it in place only if the kept file is still exactly the one that was
/// read. A changed file is merged again rather than overwritten.
fn publish_merge(
    path: &Path,
    snapshot: Option<&[u8]>,
    kept: &Kept,
    spool: &Spool,
) -> Result<(), MergeError> {
    let parent = path
        .parent()
        .ok_or_else(|| MergeError::Failed(invalid("Kept mail needs a directory")))?;
    ensure_private_dir(parent)
        .map_err(|_| MergeError::Failed(storage("The target Mail cache is unavailable")))?;
    let temporary = parent.join(format!(".tilo-{}.tmp", Uuid::new_v4()));
    let written = write_merge(&temporary, kept, spool);
    match written {
        Ok(()) => {}
        Err(MergeError::Failed(error)) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(MergeError::Failed(error));
        }
        Err(other) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(other);
        }
    }
    let current = read_private(path, MAX_CACHE)
        .map_err(|_| MergeError::Failed(storage("The kept mail is unreadable")))?;
    if current.as_deref() != snapshot {
        let _ = std::fs::remove_file(&temporary);
        return Err(MergeError::Conflict);
    }
    std::fs::rename(&temporary, path).map_err(|_| {
        let _ = std::fs::remove_file(&temporary);
        MergeError::Failed(storage("The kept mail could not be published"))
    })
}

fn write_merge(temporary: &Path, kept: &Kept, spool: &Spool) -> Result<(), MergeError> {
    let mut out = Counted::create(temporary)
        .map_err(|_| MergeError::Failed(storage("The kept mail could not be written")))?;
    out.write_all(format!("{{\"version\":{CACHE_VERSION},\"messages\":[").as_bytes())
        .map_err(|_| MergeError::Failed(storage("The kept mail could not be written")))?;
    let mut first = true;
    for message in &kept.messages {
        if !first {
            out.write_all(b",")
                .map_err(|_| MergeError::Failed(storage("The kept mail could not be written")))?;
        }
        first = false;
        serde_json::to_writer(&mut out, message)
            .map_err(|_| MergeError::Failed(storage("The kept mail could not be written")))?;
    }
    if spool.messages > 0 {
        if !first {
            out.write_all(b",")
                .map_err(|_| MergeError::Failed(storage("The kept mail could not be written")))?;
        }
        copy_bounded(&spool.path, &mut out)?;
    }
    out.write_all(b"]}")
        .map_err(|_| MergeError::Failed(storage("The kept mail could not be written")))?;
    if out.bytes > MAX_CACHE {
        return Err(MergeError::TooLarge);
    }
    out.inner
        .sync_all()
        .map_err(|_| MergeError::Failed(storage("The kept mail could not be written")))?;
    Ok(())
}

fn copy_bounded(source: &Path, out: &mut Counted<std::fs::File>) -> Result<(), MergeError> {
    let mut file = std::fs::File::open(source)
        .map_err(|_| MergeError::Failed(storage("A spooled message could not be read")))?;
    let mut buffer = vec![0_u8; mbox::CHUNK];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| MergeError::Failed(storage("A spooled message could not be read")))?;
        if read == 0 {
            return Ok(());
        }
        let bytes = buffer
            .get(..read)
            .ok_or_else(|| MergeError::Failed(invalid("A spooled message was truncated")))?;
        out.write_all(bytes)
            .map_err(|_| MergeError::Failed(storage("The kept mail could not be written")))?;
        if out.bytes > MAX_CACHE {
            return Err(MergeError::TooLarge);
        }
    }
}

/// A writer that remembers how many bytes reached the file, so the Mail app's
/// per-account bound is checked before anything is published.
struct Counted<W> {
    inner: W,
    bytes: u64,
}

impl Counted<std::fs::File> {
    fn create(path: &Path) -> io::Result<Self> {
        Ok(Self {
            inner: private_create(path)?,
            bytes: 0,
        })
    }
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.bytes = self.bytes.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
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

/// The private directory that keeps one folder's original bytes.
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
    ensure_private_dir(&directory)
        .map_err(|_| storage("The target mail copy area is unavailable"))?;
    Ok(directory)
}

/// Keep an address book or calendar file's exact bytes beside the imported
/// mail, refusing to replace a copy that holds other bytes.
fn retain_sidecar(
    root: &Path,
    profile: &str,
    kind: &str,
    source_id: &str,
    name: &str,
    bytes: &[u8],
) -> io::Result<()> {
    let directory = root
        .join(component(profile))
        .join(kind)
        .join(component(source_id));
    ensure_private_dir(&directory)
        .map_err(|_| storage("The target mail copy area is unavailable"))?;
    match write_file(&directory.join(component(name)), bytes) {
        Ok(()) => Ok(()),
        Err(ConnectionError::AlreadyPresent) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "A retained file already holds other bytes",
        )),
        Err(_) => Err(storage("A selected file could not be kept")),
    }
}

/// Whether two files hold exactly the same bytes, read in chunks.
fn same_bytes(left: &Path, right: &Path) -> io::Result<bool> {
    let mut left = std::fs::File::open(left)?;
    let mut right = std::fs::File::open(right)?;
    let mut left_buffer = vec![0_u8; mbox::CHUNK];
    let mut right_buffer = vec![0_u8; mbox::CHUNK];
    loop {
        let left_read = left.read(&mut left_buffer)?;
        let right_read = right.read(&mut right_buffer)?;
        let left_bytes = left_buffer.get(..left_read).unwrap_or_default();
        let right_bytes = right_buffer.get(..right_read).unwrap_or_default();
        if left_bytes != right_bytes {
            return Ok(false);
        }
        if left_read == 0 {
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests;
