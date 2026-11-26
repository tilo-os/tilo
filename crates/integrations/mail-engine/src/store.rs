//! The mail this device has already fetched, kept per account in the app's
//! cache area, so a window opens on it at once and a refresh asks the server
//! only for what is new.
//!
//! What the person did to a message (read, starred, moved) is the mailbox
//! resource's and syncs with it; this keeps only the corpus the servers sent:
//! headers, bodies, attachment names and where each message is on its
//! server. It is a cache: losing it costs one full fetch, nothing else.

#[cfg(feature = "live")]
use std::path::{Path, PathBuf};
#[cfg(feature = "live")]
use tilo_sort::CompactSort as _;

#[cfg(feature = "live")]
use serde::{Deserialize, Serialize};

use crate::model::Message;

#[cfg(feature = "live")]
const VERSION: u32 = 1;

pub const KEEP: usize = 5_000;

#[cfg(feature = "live")]
#[derive(Serialize, Deserialize)]
struct Kept {
    version: u32,
    messages: Vec<Message>,
}

/// The mail this device has fetched, kept in its cache area. A page keeps
/// none: the workspace's copies resource holds what its person has.
#[derive(Clone, Debug, Default)]
pub struct Store {
    #[cfg(feature = "live")]
    root: Option<PathBuf>,
    #[cfg(feature = "live")]
    writer: std::sync::Arc<std::sync::OnceLock<Option<Sender<Write>>>>,
}

#[cfg(feature = "live")]
type Write = (PathBuf, Option<Vec<Message>>, Vec<(PathBuf, Vec<u8>)>);
#[cfg(feature = "live")]
type Sender<T> = std::sync::mpsc::Sender<T>;

impl Store {
    /// A store that keeps nothing here, for a device with no cache area.
    pub fn nowhere() -> Self {
        Self::default()
    }
}

#[cfg(feature = "live")]
impl Store {
    pub fn at(root: PathBuf) -> Self {
        Self {
            root: Some(root),
            writer: std::sync::Arc::default(),
        }
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
                                Some(messages) => {
                                    // What is no longer kept takes its
                                    // attachments with it.
                                    let kept = attachment_names(&messages);
                                    write(&path, messages)
                                        .and_then(|()| prune(&path.with_extension("files"), &kept))
                                }
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

    pub fn load(&self, account: &str) -> Vec<Message> {
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

    pub fn save(
        &self,
        account: &str,
        mut messages: Vec<Message>,
    ) -> Vec<(crate::model::MessageId, usize)> {
        let Some(path) = self.file(account) else {
            return Vec::new();
        };
        messages.compact_sort_by_key(|message| std::cmp::Reverse(message.timestamp_min));
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

    fn attachment_file(
        &self,
        account: &str,
        message: crate::model::MessageId,
        index: usize,
    ) -> Option<PathBuf> {
        let folder = self.file(account)?.with_extension("files");
        Some(folder.join(format!("{message}.{index}")))
    }

    pub fn attachment(
        &self,
        account: &str,
        message: crate::model::MessageId,
        index: usize,
    ) -> Option<Vec<u8>> {
        std::fs::read(self.attachment_file(account, message, index)?).ok()
    }

    pub fn forget(&self, account: &str) {
        if let Some(path) = self.file(account)
            && let Some(writer) = self.writer()
        {
            let _ = writer.send((path, None, Vec::new()));
        }
    }
}

/// A page keeps no mail on a device: what its person has is the workspace's
/// copies resource, read over the session.
#[cfg(not(feature = "live"))]
#[allow(
    clippy::unused_self,
    reason = "a page's store keeps nothing, so no method reads it"
)]
impl Store {
    pub fn load(&self, _account: &str) -> Vec<Message> {
        Vec::new()
    }

    pub fn save(
        &self,
        _account: &str,
        _messages: Vec<Message>,
    ) -> Vec<(crate::model::MessageId, usize)> {
        Vec::new()
    }

    pub fn attachment(
        &self,
        _account: &str,
        _message: crate::model::MessageId,
        _index: usize,
    ) -> Option<Vec<u8>> {
        None
    }

    pub fn forget(&self, _account: &str) {}
}

#[cfg(feature = "live")]
fn attachment_names(messages: &[Message]) -> std::collections::HashSet<String> {
    messages
        .iter()
        .flat_map(|message| {
            message
                .attachments
                .iter()
                .enumerate()
                .filter(|(_, attachment)| attachment.kept)
                .map(move |(index, _)| format!("{}.{index}", message.id))
        })
        .collect()
}

#[cfg(feature = "live")]
fn prune(folder: &Path, kept: &std::collections::HashSet<String>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !kept.contains(&name) {
            std::fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

#[cfg(feature = "live")]
fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    tilo_files::native::atomic::write_creating_parent(path, bytes)
}

#[cfg(feature = "live")]
fn remove_dir(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(feature = "live")]
fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(feature = "live")]
fn write(path: &Path, messages: Vec<Message>) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&Kept {
        version: VERSION,
        messages,
    })
    .map_err(std::io::Error::other)?;
    tilo_files::native::atomic::write_creating_parent(path, &bytes)
}

#[cfg(feature = "live")]
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

    tilo_test_env::scenario! {
        /// The kept-mail store, each case in a directory of its own.
        fn the_store_keeps_mail_and_attachment_bytes_per_account;

        phase kept_mail_comes_back_for_its_account_only;
        {
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
            tilo_test_env::wait_until("the kept mail to be forgotten", || {
                store.load("me@x.example").is_empty()
            });
            assert!(Store::nowhere().load("me@x.example").is_empty());
        }

        phase attachment_bytes_are_kept_once_and_read_back;
        {
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
            let bytes = tilo_test_env::wait_for("the attachment to be kept", || {
                store.attachment("me@x.example", id, 0)
            });
            assert_eq!(bytes, b"%PDF-1.7");
            with_file.attachments[0].kept = true;
            assert!(
                store.save("me@x.example", vec![with_file]).is_empty(),
                "what is kept is not written again"
            );
            let loaded = tilo_test_env::wait_for("the attachment to load as kept", || {
                Some(store.load("me@x.example")).filter(|loaded| {
                    loaded
                        .first()
                        .is_some_and(|message| message.attachments.first().is_some_and(|a| a.kept))
                })
            });
            assert!(loaded[0].attachments[0].is_downloadable());

            assert!(store.save("me@x.example", vec![message(2)]).is_empty());
            tilo_test_env::wait_until("the attachment to go with its mail", || {
                store.attachment("me@x.example", id, 0).is_none()
            });
        }

        phase a_damaged_file_reads_as_nothing_kept;
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::at(dir.path().to_path_buf());
            std::fs::write(store.file("me@x.example").unwrap(), b"{not json").unwrap();
            assert!(store.load("me@x.example").is_empty());
        }

        phase file_names_carry_no_path;
        {
            assert_eq!(file_stem("Me+tag@X.example"), "me+tag@x.example");
            assert_eq!(file_stem("../../etc/passwd"), ".._.._etc_passwd");
        }
    }
}
