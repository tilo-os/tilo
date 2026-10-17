//! The mail this device has already fetched, kept per account in the app's
//! cache area, so a window opens on it at once and a refresh asks the server
//! only for what is new.
//!
//! What the person did to a message (read, starred, moved) is the mailbox
//! record's and syncs with it; this keeps only the corpus the servers sent:
//! headers, bodies, attachment names and where each message is on its
//! server. It is a cache: losing it costs one full fetch, nothing else.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::Message;

/// The layout of a kept file. A file of another version is fetched again.
const VERSION: u32 = 1;

/// How many messages are kept per account: the newest ones. More stay on
/// the server and come back as the list scrolls to them.
pub(crate) const KEEP: usize = 5_000;

#[derive(Serialize, Deserialize)]
struct Kept {
    version: u32,
    messages: Vec<Message>,
}

/// Where fetched mail is kept, or nowhere (a capture, a past version, a
/// host without an app cache).
#[derive(Clone, Debug, Default)]
pub(crate) struct Store {
    root: Option<PathBuf>,
    /// The one thread that writes, in the order the writes were asked for,
    /// so a later sync's file is never overwritten by an earlier one's.
    writer: std::sync::Arc<std::sync::OnceLock<Option<Sender<Write>>>>,
}

/// A file to write: an account's mail (`Some`), or nothing (`None`, to
/// remove it), and the attachment files that go with it.
type Write = (PathBuf, Option<Vec<Message>>, Vec<(PathBuf, Vec<u8>)>);
type Sender<T> = std::sync::mpsc::Sender<T>;

impl Store {
    /// Keep mail under `root`.
    pub(crate) fn at(root: PathBuf) -> Self {
        Self {
            root: Some(root),
            writer: std::sync::Arc::default(),
        }
    }

    /// Keep nothing.
    pub(crate) fn nowhere() -> Self {
        Self::default()
    }

    fn writer(&self) -> Option<&Sender<Write>> {
        self.writer
            .get_or_init(|| {
                let (sender, receiver) = std::sync::mpsc::channel::<Write>();
                let spawned = std::thread::Builder::new().name("mail-keep".into()).spawn(
                    move || {
                        while let Ok((path, messages, files)) = receiver.recv() {
                            for (file, bytes) in files {
                                if let Err(error) = write_file(&file, &bytes) {
                                    tracing::warn!(%error, "mail: an attachment could not be kept");
                                }
                            }
                            let done = match messages {
                                Some(messages) => write(&path, messages),
                                None => remove(&path)
                                    .and_then(|()| remove_dir(&path.with_extension("files"))),
                            };
                            if let Err(error) = done {
                                tracing::warn!(%error, "mail: kept mail could not be updated");
                            }
                        }
                    },
                );
                match spawned {
                    Ok(_) => Some(sender),
                    Err(error) => {
                        tracing::warn!(%error, "mail: fetched mail will not be kept");
                        None
                    }
                }
            })
            .as_ref()
    }

    fn file(&self, account: &str) -> Option<PathBuf> {
        let root = self.root.as_ref()?;
        Some(root.join(format!("{}.json", file_stem(account))))
    }

    /// The mail kept for `account`, oldest first as it was written; none
    /// when nothing (or nothing readable) is kept.
    pub(crate) fn load(&self, account: &str) -> Vec<Message> {
        let Some(path) = self.file(account) else {
            return Vec::new();
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return Vec::new();
        };
        match serde_json::from_slice::<Kept>(&bytes) {
            Ok(kept) if kept.version == VERSION => kept
                .messages
                .into_iter()
                .map(|mut message| {
                    account.clone_into(&mut message.account);
                    message
                })
                .collect(),
            Ok(_) => Vec::new(),
            Err(error) => {
                tracing::warn!(%error, "mail: the kept mail could not be read; fetching it again");
                Vec::new()
            }
        }
    }

    /// Keep `messages` as `account`'s mail, the newest [`KEEP`] of them,
    /// written on a worker thread so the window never waits for the disk.
    /// Attachment bytes not kept yet are kept beside it; the returned
    /// `(message, attachment index)` pairs are kept from now on.
    pub(crate) fn save(
        &self,
        account: &str,
        mut messages: Vec<Message>,
    ) -> Vec<(crate::model::MessageId, usize)> {
        let Some(path) = self.file(account) else {
            return Vec::new();
        };
        messages.sort_by_key(|message| std::cmp::Reverse(message.timestamp_min));
        messages.truncate(KEEP);
        let mut files = Vec::new();
        let mut newly_kept = Vec::new();
        for message in &mut messages {
            for (index, attachment) in message.attachments.iter_mut().enumerate() {
                if attachment.kept || attachment.content.is_empty() {
                    continue;
                }
                if let Some(file) = self.attachment_file(account, message.id, index) {
                    files.push((file, std::mem::take(&mut attachment.content)));
                    attachment.kept = true;
                    newly_kept.push((message.id, index));
                }
            }
        }
        if let Some(writer) = self.writer() {
            let _ = writer.send((path, Some(messages), files));
        }
        newly_kept
    }

    /// Where attachment `index` of `account`'s message `message` is kept.
    fn attachment_file(
        &self,
        account: &str,
        message: crate::model::MessageId,
        index: usize,
    ) -> Option<PathBuf> {
        let folder = self.file(account)?.with_extension("files");
        Some(folder.join(format!("{message}.{index}")))
    }

    /// The bytes of attachment `index` of `account`'s message `message`, as
    /// kept on this device.
    pub(crate) fn attachment(
        &self,
        account: &str,
        message: crate::model::MessageId,
        index: usize,
    ) -> Option<Vec<u8>> {
        std::fs::read(self.attachment_file(account, message, index)?).ok()
    }

    /// A file to hand an attachment to another app through: `name` in a
    /// folder of this app's own, emptied of what an earlier opening left.
    pub(crate) fn opening(&self, name: &str) -> Option<PathBuf> {
        let folder = self.root.as_ref()?.join("opened");
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).ok()?;
        Some(folder.join(file_stem(name)))
    }

    /// Forget what is kept for `account` (the account was removed).
    pub(crate) fn forget(&self, account: &str) {
        if let Some(path) = self.file(account)
            && let Some(writer) = self.writer()
        {
            let _ = writer.send((path, None, Vec::new()));
        }
    }
}

fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = path.with_extension("partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path)
}

fn remove_dir(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn write(path: &Path, messages: Vec<Message>) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&Kept {
        version: VERSION,
        messages,
    })
    .map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Written beside and moved over, so a crash never leaves half a file.
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, path)
}

/// A file name for an account: its address with anything a file name
/// should not carry replaced.
fn file_stem(account: &str) -> String {
    account
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '-' | '_' | '+') {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Folder;

    fn message(at: i64) -> Message {
        let raw = format!(
            "From: Maya <maya@x.example>\r\nSubject: S{at}\r\nMessage-ID: <{at}@x>\r\n\r\nBody {at}.\r\n"
        );
        crate::transport::parse_eml(raw.as_bytes(), at, Folder::Inbox).unwrap()
    }

    #[test]
    fn kept_mail_comes_back_for_its_account_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_path_buf());
        let kept = vec![message(1), message(2)];
        write(&store.file("me@x.example").unwrap(), kept.clone()).unwrap();
        let loaded = store.load("me@x.example");
        assert_eq!(loaded.len(), 2);
        assert!(loaded.iter().all(|m| m.account == "me@x.example"));
        assert_eq!(loaded[0].body, kept[0].body);
        assert!(store.load("other@x.example").is_empty());
        store.forget("me@x.example");
        let deadline = tilo_test_env::hang_deadline();
        while !store.load("me@x.example").is_empty() {
            assert!(
                ui::time::now() < deadline,
                "the kept mail was never forgotten"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(Store::nowhere().load("me@x.example").is_empty());
    }

    #[test]
    fn attachment_bytes_are_kept_once_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_path_buf());
        let mut with_file = message(1);
        with_file.attachments = vec![crate::model::Attachment::hydrated(
            "plan.pdf",
            "application/pdf",
            b"%PDF-1.7".to_vec(),
        )];
        let id = with_file.id;
        let kept = store.save("me@x.example", vec![with_file.clone()]);
        assert_eq!(kept, [(id, 0)]);
        let deadline = tilo_test_env::hang_deadline();
        let bytes = loop {
            if let Some(bytes) = store.attachment("me@x.example", id, 0) {
                break bytes;
            }
            assert!(ui::time::now() < deadline, "the attachment was never kept");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(bytes, b"%PDF-1.7");
        with_file.attachments[0].kept = true;
        assert!(
            store.save("me@x.example", vec![with_file]).is_empty(),
            "what is kept is not written again"
        );
        let loaded = loop {
            let loaded = store.load("me@x.example");
            if loaded
                .first()
                .is_some_and(|message| message.attachments.first().is_some_and(|a| a.kept))
            {
                break loaded;
            }
            assert!(ui::time::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(loaded[0].attachments[0].is_downloadable());
    }

    #[test]
    fn a_damaged_file_reads_as_nothing_kept() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path().to_path_buf());
        std::fs::write(store.file("me@x.example").unwrap(), b"{not json").unwrap();
        assert!(store.load("me@x.example").is_empty());
    }

    #[test]
    fn file_names_carry_no_path() {
        assert_eq!(file_stem("Me+tag@X.example"), "me+tag@x.example");
        assert_eq!(file_stem("../../etc/passwd"), ".._.._etc_passwd");
    }
}
