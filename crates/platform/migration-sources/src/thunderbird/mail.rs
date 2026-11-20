//! Reading one Thunderbird folder's messages, for a laptop command that copies
//! them into a mail app without the installer's importer.
//!
//! The folder is read the way the installer reads it, an mbox in fixed chunks
//! and a maildir one file per message, but whole: memory follows the largest
//! message, and no message is too large or folder too full to come over, since
//! the mail app keeps a message's large parts apart from it.

use std::io::BufReader;
use std::ops::ControlFlow;
use std::path::Path;

use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};

use super::{
    FolderFormat, MboxMessages, ReviewedFolder, ReviewedThunderbird, header, maildir_read_flags,
    mbox, message_flags, read_file, scan, walk,
};

/// One message as the folder holds it, with its flags.
pub struct StoredMessage {
    /// The message exactly as stored, including its `From ` separator line in
    /// an mbox.
    pub bytes: Vec<u8>,
    /// Taken from `X-Mozilla-Status` (`0x1`) in an mbox, from the file's flags
    /// in a maildir, or `false` when neither says.
    pub read: bool,
    /// Taken from `X-Mozilla-Status` (`0x4`) in an mbox, from the file's flags
    /// in a maildir, or `false` when neither says.
    pub starred: bool,
    /// The `Message-ID` header, when the message has one.
    pub message_id: Option<String>,
}

/// Every message of one folder, in the order the folder stores them. `each`
/// sees one message at a time; returning [`ControlFlow::Break`] stops the read.
///
/// `profile` names a profile by its `source_id`, its label or one of its
/// accounts' addresses; `folder` names a folder by its `source_id` or by its
/// path in the tree (`Inbox`, `Projects/2024`).
///
/// # Errors
/// The home holds no such profile or folder, or the folder could not be read
/// within its bounds.
pub fn messages(
    home: &Path,
    profile: &str,
    folder: &str,
    each: &mut dyn FnMut(StoredMessage) -> ControlFlow<()>,
) -> Result<(), String> {
    let scan = scan(home);
    let reviewed = super::find_reviewed(&scan.reviewed, profile)
        .ok_or_else(|| format!("no Thunderbird profile {profile:?}"))?;
    let folder = find_folder(reviewed, folder)
        .ok_or_else(|| format!("no Thunderbird folder {folder:?} in that profile"))?;
    match folder.format {
        FolderFormat::Mbox => read_mbox(reviewed, folder, each),
        FolderFormat::Maildir => read_maildir(reviewed, folder, each),
    }
}

fn read_mbox(
    reviewed: &ReviewedThunderbird,
    folder: &ReviewedFolder,
    each: &mut dyn FnMut(StoredMessage) -> ControlFlow<()>,
) -> Result<(), String> {
    let (name, parent) = folder
        .relative
        .split_last()
        .ok_or_else(|| "the folder has no mail file".to_owned())?;
    let directory = walk(&reviewed.root, parent).map_err(|error| error.to_string())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&directory.file, Path::new(name), &options)
        .map_err(|error| error.to_string())?;
    let mut messages = MboxMessages::new(BufReader::with_capacity(mbox::CHUNK, file), usize::MAX);
    loop {
        match messages.next_message() {
            Ok(Some(raw)) => {
                let (read, starred) = message_flags(&raw, None);
                let message_id = header(&raw, b"message-id:").map(|value| value.trim().to_owned());
                if each(StoredMessage {
                    bytes: raw,
                    read,
                    starred,
                    message_id,
                })
                .is_break()
                {
                    return Ok(());
                }
            }
            Ok(None) => return Ok(()),
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn read_maildir(
    reviewed: &ReviewedThunderbird,
    folder: &ReviewedFolder,
    each: &mut dyn FnMut(StoredMessage) -> ControlFlow<()>,
) -> Result<(), String> {
    let directory = walk(&reviewed.root, &folder.relative).map_err(|error| error.to_string())?;
    for part in ["cur", "new"] {
        let part = part.to_owned();
        let Ok(current) = walk(&directory, std::slice::from_ref(&part)) else {
            continue;
        };
        let entries = super::entries(&current).map_err(|error| error.to_string())?;
        for (name, info) in entries {
            if !info.is_file() || info.is_symlink() || name.starts_with('.') {
                continue;
            }
            let raw = read_file(&current, &name, u64::MAX).map_err(|error| error.to_string())?;
            let (read, starred) = maildir_read_flags(&name);
            let message_id = header(&raw, b"message-id:").map(|value| value.trim().to_owned());
            if each(StoredMessage {
                bytes: raw,
                read,
                starred,
                message_id,
            })
            .is_break()
            {
                return Ok(());
            }
        }
    }
    Ok(())
}

fn find_folder<'a>(reviewed: &'a ReviewedThunderbird, folder: &str) -> Option<&'a ReviewedFolder> {
    for account in &reviewed.accounts {
        for candidate in &account.folders {
            if candidate.inventory.source_id == folder
                || candidate.inventory.hierarchy.join("/") == folder
            {
                return Some(candidate);
            }
        }
    }
    let leaf = folder.to_owned();
    for account in &reviewed.accounts {
        for candidate in &account.folders {
            if candidate.inventory.hierarchy.last() == Some(&leaf) {
                return Some(candidate);
            }
        }
    }
    None
}
