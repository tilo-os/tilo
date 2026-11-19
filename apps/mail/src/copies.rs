//! Server messages kept by the workspace.
//!
//! A message a device fetched is kept, as the server sent it, as content of the copies resource
//! ([`MailCopies`]). Members with access can take a message the resource names,
//! newest first, without asking any server; the server stays the authority over its messages,
//! and one it reports removed is dropped from the resource.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tilo_sort::CompactSort as _;

use serde::{Deserialize, Serialize};
use tilo_content::{ContentState, ContentStore};
use ui::framework::WorkspaceResource;
use ui::tilo_resource::{self, Collection, Id, JsonSchema, Model, Refusal, Set};

use crate::model::{Folder, Message, MessageId, ServerPlace};

/// The largest message retained in workspace storage.
pub(crate) const MAX_COPY_BYTES: usize = 16 * 1024 * 1024;

/// The number of content requests one window keeps in flight.
const MAX_ASKED: usize = 32;

const MAX_ACCOUNT_CHARS: usize = 320;
pub(crate) use ui::tilo_actions::policy::roles::WORKSPACE_MAIL_COPIES_DOCUMENT as DOCUMENT;

/// Mail fetched from servers and retained for workspace members.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct MailCopies {
    /// Each message under its account and id in the mailbox
    #[model(readonly)]
    pub copies: Collection<KeptCopy>,
    /// Server deletions that a previous copy must not undo.
    #[model(readonly)]
    pub removed: Set<String>,
}

fn copy_key(account: &str, id: &str) -> Id {
    Id::from_raw(format!("{account}/{id}"))
}

fn message_id(key: &Id) -> Option<MessageId> {
    key.as_str().rsplit('/').next()?.parse().ok()
}

/// One message a device fetched, and where its server holds it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = KeptCopy::valid)]
pub struct KeptCopy {
    /// The account it was fetched through
    pub account: String,
    /// The message as its server sent it, by the reference its content is
    /// kept under
    pub content: String,
    /// When it arrived, in minutes since the Unix epoch
    pub minutes: i64,
    /// The folder its server holds it in
    pub folder: Folder,
    /// Its UID in that folder
    pub uid: u32,
    /// The numbering that UID belongs to
    pub uid_validity: u32,
}

impl KeptCopy {
    fn valid(&self) -> Result<(), Refusal> {
        if self.account.trim().is_empty() || self.account.chars().count() > MAX_ACCOUNT_CHARS {
            return Err(Refusal::new("a kept message names its account"));
        }
        if !tilo_content::valid_reference(&self.content) {
            return Err(Refusal::new(
                "a kept message names its content by reference",
            ));
        }
        if self.folder == Folder::Starred {
            return Err(Refusal::new(
                "a kept message names the folder its server holds",
            ));
        }
        Ok(())
    }

    /// The message `bytes` hold, filed under `id`: `None` when they are not
    /// a message with the `Message-ID` that id was made from.
    fn message(&self, id: MessageId, bytes: &[u8]) -> Option<Message> {
        let mut message = crate::transport::parse_fetched(
            bytes,
            self.folder,
            false,
            false,
            Some(self.minutes),
            self.minutes,
        )?;
        let named = message.rfc_message_id.as_deref()?;
        if crate::transport::stable_message_id(named) != id {
            return None;
        }
        message.id = id;
        message.account.clone_from(&self.account);
        message.server = Some(ServerPlace {
            folder: self.folder,
            uid: self.uid,
            uid_validity: self.uid_validity,
        });
        Some(message)
    }
}

/// A message to retain in workspace storage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewCopy {
    /// Its id in the mailbox
    pub id: String,
    /// The account it was fetched through
    pub account: String,
    /// The message as its server sent it, by the reference its content is
    /// kept under
    pub content: String,
    /// When it arrived, in minutes since the Unix epoch
    pub minutes: i64,
    /// The folder its server holds it in
    pub folder: Folder,
    /// Its UID in that folder
    pub uid: u32,
    /// The numbering that UID belongs to
    pub uid_validity: u32,
}

#[ui::tilo_resource::resource(kind = "mail.copies", params = kept)]
impl MailCopies {
    /// Keep fetched mail
    ///
    /// Retains the newest messages each account fetched for the workspace.
    #[action(external, writes = copies, writes = removed)]
    pub fn keep_copies(
        &mut self,
        #[param("The messages, each with where its server holds it")] copies: Vec<NewCopy>,
    ) -> Result<(), Refusal> {
        self.keep(copies, false)
    }

    /// Import fetched mail
    ///
    /// Keep copies from an older mailbox where the workspace has none.
    #[action(external, writes = copies)]
    pub fn import_copies(&mut self, copies: Vec<NewCopy>) -> Result<(), Refusal> {
        self.keep(copies, true)
    }

    /// Forget fetched mail
    ///
    /// Drops messages their servers no longer hold.
    #[action(external, writes = copies, writes = removed)]
    pub fn forget_copies(
        &mut self,
        #[param("Ids of the messages")] messages: Vec<String>,
        #[param("The account whose server removed them")] account: Option<String>,
    ) {
        let gone: HashSet<MessageId> = messages.iter().filter_map(|id| id.parse().ok()).collect();
        if let Some(account) = &account {
            for id in &gone {
                self.removed
                    .insert(copy_key(account, &id.to_string()).to_string());
            }
        }
        self.copies.retain(|key, copy| {
            account
                .as_ref()
                .is_some_and(|account| *account != copy.account)
                || message_id(key).is_none_or(|id| !gone.contains(&id))
        });
    }
}

impl MailCopies {
    fn keep(&mut self, copies: Vec<NewCopy>, importing: bool) -> Result<(), Refusal> {
        let mut accounts = BTreeSet::new();
        for copy in copies {
            if uuid::Uuid::parse_str(&copy.id).is_err() {
                return Err(Refusal::new("a kept message is filed under its mailbox id"));
            }
            let kept = KeptCopy {
                account: copy.account,
                content: copy.content,
                minutes: copy.minutes,
                folder: copy.folder,
                uid: copy.uid,
                uid_validity: copy.uid_validity,
            };
            kept.valid()?;
            let key = copy_key(&kept.account, &copy.id);
            if importing && (self.removed.contains(&key.to_string()) || self.copies.contains(&key))
            {
                continue;
            }
            if !importing {
                self.removed.remove(&key.to_string());
            }
            accounts.insert(kept.account.clone());
            self.copies.push(key, kept);
        }
        for account in accounts {
            self.keep_newest(&account, crate::store::KEEP);
        }
        Ok(())
    }

    pub(crate) fn copy(&self, account: &str, id: MessageId) -> Option<&KeptCopy> {
        let key = copy_key(account, &id.to_string());
        if self.removed.contains(&key.to_string()) {
            return None;
        }
        self.copies.get(&key)
    }
    /// Keep only `account`'s newest `most` messages.
    fn keep_newest(&mut self, account: &str, most: usize) {
        let mut order: Vec<(Id, i64)> = self
            .copies
            .iter()
            .filter(|(_, copy)| copy.account == account)
            .map(|(id, copy)| (id.clone(), copy.minutes))
            .collect();
        if order.len() <= most {
            return;
        }
        order.compact_sort_by_key(|(_, minutes)| std::cmp::Reverse(*minutes));
        let dropped: HashSet<Id> = order.into_iter().skip(most).map(|(id, _)| id).collect();
        self.copies.retain(|id, _| !dropped.contains(id));
    }
}

/// Transfer copies from the current member's previous record through the
/// workspace content store, which seals them for the workspace route.
pub(crate) fn import_legacy(
    opener: &ui::framework::ResourceOpener<MailCopies>,
) -> Result<serde_json::Value, String> {
    let workspace = opener.open(DOCUMENT).ok_or("workspace mail is arriving")?;
    if !workspace.may_read() || !workspace.may("import-copies") {
        return Err("the workspace must grant permission to import saved mail".into());
    }
    let Some((_, legacy, source)) = opener.legacy_member_content()? else {
        return Ok(serde_json::json!({ "imported": 0, "waiting": 0 }));
    };
    let content = workspace
        .record()
        .content()
        .ok_or("the workspace has no mail content store")?;
    let current = workspace.get();
    let mut copies = Vec::new();
    let mut waiting = 0;
    let mut unavailable = 0;
    for (key, copy) in legacy.copies.iter() {
        let Some(id) = message_id(key) else {
            unavailable += 1;
            continue;
        };
        let key = copy_key(&copy.account, &id.to_string());
        if current.copies.contains(&key) || current.removed.contains(&key.to_string()) {
            continue;
        }
        if waiting >= MAX_ASKED || copies.len() >= MAX_ASKED {
            waiting += 1;
            continue;
        }
        let Some(bytes) = source.read(&copy.content) else {
            if source.state(&copy.content) == ContentState::Unavailable {
                unavailable += 1;
            } else {
                waiting += 1;
            }
            continue;
        };
        if bytes.len() > MAX_COPY_BYTES || copy.message(id, &bytes).is_none() {
            unavailable += 1;
            continue;
        }
        copies.push(NewCopy {
            id: id.to_string(),
            account: copy.account.clone(),
            content: content.put(&bytes).map_err(|error| error.to_string())?,
            minutes: copy.minutes,
            folder: copy.folder,
            uid: copy.uid,
            uid_validity: copy.uid_validity,
        });
    }
    let imported = copies.len();
    if imported != 0 {
        workspace
            .invoke(&MailCopies::IMPORT_COPIES.with(kept::ImportCopies { copies }))
            .map_err(|error| error.to_string())?;
    }
    Ok(serde_json::json!({ "imported": imported, "waiting": waiting, "unavailable": unavailable }))
}

/// A window's connection to fetched copies in the workspace.
pub(crate) struct Copies {
    record: WorkspaceResource<MailCopies>,
    /// The content carried on the resource's route.
    content: ContentStore,
    /// Outstanding requests by content reference.
    asked: BTreeMap<String, (MessageId, KeptCopy)>,
    /// Messages taken, or that cannot arrive, in this window: not asked
    /// for again.
    settled: HashSet<MessageId>,
    /// What content had reached this process, and how far the resource had
    /// moved, when this last looked.
    looked: Option<(u64, u64)>,
}

impl Copies {
    pub(crate) fn new(record: WorkspaceResource<MailCopies>, content: ContentStore) -> Self {
        Self {
            record,
            content,
            asked: BTreeMap::new(),
            settled: HashSet::new(),
            looked: None,
        }
    }

    pub(crate) fn record(&self) -> &WorkspaceResource<MailCopies> {
        &self.record
    }

    /// Keep what `account`'s server just sent (`originals`, by id).
    /// Mail without
    /// a `Message-ID`, which another device could not file under the same
    /// id, and messages larger than [`MAX_COPY_BYTES`] are not kept.
    pub(crate) fn keep(
        &self,
        account: &str,
        messages: &[Message],
        originals: &[(MessageId, Vec<u8>)],
    ) {
        if !self.record.may_read() || !self.record.may("keep-copies") {
            return;
        }
        let fetched: HashMap<MessageId, &Message> = messages
            .iter()
            .map(|message| (message.id, message))
            .collect();
        let held = self.record.get();
        let mut copies = Vec::new();
        for (id, original) in originals {
            let Some(message) = fetched.get(id) else {
                continue;
            };
            let Some(at) = message.server else {
                continue;
            };
            if message.rfc_message_id.as_deref().is_none_or(str::is_empty)
                || at.folder == Folder::Starred
                || original.len() > MAX_COPY_BYTES
            {
                continue;
            }
            // A server's message at one UID of one numbering never changes.
            let known = held.copy(account, *id);
            if known.is_some_and(|known| {
                known.folder == at.folder
                    && known.uid == at.uid
                    && known.uid_validity == at.uid_validity
            }) {
                continue;
            }
            let content = match self.content.put(original) {
                Ok(reference) => reference,
                Err(error) => {
                    tracing::warn!(%error, "mail: a message could not be kept in the workspace");
                    continue;
                }
            };
            copies.push(NewCopy {
                id: id.to_string(),
                account: account.to_owned(),
                content,
                minutes: message.timestamp_min,
                folder: at.folder,
                uid: at.uid,
                uid_validity: at.uid_validity,
            });
        }
        if !copies.is_empty()
            && let Err(error) = self
                .record
                .invoke(&MailCopies::KEEP_COPIES.with(kept::KeepCopies { copies }))
        {
            tracing::warn!(%error, "mail: fetched mail could not be kept in the workspace");
        }
        self.release();
    }

    /// Drop `gone`, which their server no longer holds.
    pub(crate) fn forget(&self, account: &str, gone: &[MessageId]) {
        if gone.is_empty() || !self.record.may_read() || !self.record.may("forget-copies") {
            return;
        }
        let messages = gone.iter().map(MessageId::to_string).collect();
        if let Err(error) =
            self.record
                .invoke(&MailCopies::FORGET_COPIES.with(kept::ForgetCopies {
                    messages,
                    account: Some(account.to_owned()),
                }))
        {
            tracing::warn!(%error, "mail: removed mail could not be dropped in the workspace");
        }
    }

    /// Whether there may be something new to take: content reached this
    /// process, or the resource moved, since this last looked.
    pub(crate) fn due(&self) -> bool {
        self.looked != Some(self.now())
    }

    pub(crate) fn discard_pending(&mut self) {
        self.asked.clear();
        self.settled.clear();
        self.looked = None;
        self.release();
    }

    fn now(&self) -> (u64, u64) {
        (
            ui::collaboration::document_content::arrivals(),
            self.record.revision(),
        )
    }

    /// Read workspace messages that are not already held by the mailbox.
    /// Request missing content in batches, newest first.
    pub(crate) fn take(&mut self, held: impl Fn(MessageId) -> bool) -> Vec<Message> {
        if !self.record.may_read() {
            self.discard_pending();
            return Vec::new();
        }
        let now = self.now();
        if self.looked.is_some_and(|(_, revision)| revision != now.1) {
            self.settled.clear();
        }
        self.looked = Some(now);
        let mut taken = Vec::new();
        let record = self.record.get();
        for (reference, (id, copy)) in std::mem::take(&mut self.asked) {
            if held(id) || record.copy(&copy.account, id) != Some(&copy) {
                continue;
            }
            match self.read(&reference, id, &copy) {
                Read::Here(message) => taken.extend(message.map(|message| *message)),
                Read::Gone => {}
                Read::Asked => {
                    self.asked.insert(reference, (id, copy));
                }
            }
        }
        if self.asked.len() < MAX_ASKED {
            let asked: HashSet<MessageId> = self.asked.values().map(|(id, _)| *id).collect();
            let mut missing: Vec<(MessageId, &KeptCopy)> = record
                .copies
                .iter()
                .filter_map(|(key, copy)| {
                    let id = message_id(key)?;
                    (!record.removed.contains(&key.to_string())
                        && !self.settled.contains(&id)
                        && !asked.contains(&id)
                        && !held(id))
                    .then_some((id, copy))
                })
                .collect();
            missing.compact_sort_by_key(|(_, copy)| std::cmp::Reverse(copy.minutes));
            let room = MAX_ASKED - self.asked.len();
            for (id, copy) in missing.into_iter().take(room) {
                match self.read(&copy.content, id, copy) {
                    Read::Here(message) => taken.extend(message.map(|message| *message)),
                    Read::Gone => {}
                    Read::Asked => {
                        self.asked.insert(copy.content.clone(), (id, copy.clone()));
                    }
                }
            }
        }
        if !taken.is_empty() {
            self.release();
        }
        taken
    }

    /// Read the copy, requesting its content from the workspace if needed.
    fn read(&mut self, reference: &str, id: MessageId, copy: &KeptCopy) -> Read {
        if let Some(bytes) = self.content.read(reference) {
            self.settled.insert(id);
            let message = copy.message(id, &bytes);
            if message.is_none() {
                tracing::warn!("mail: a kept message is not the message it is filed under");
            }
            return Read::Here(message.map(Box::new));
        }
        if self.content.state(reference) == ContentState::Unavailable {
            self.settled.insert(id);
            return Read::Gone;
        }
        Read::Asked
    }

    /// Whether this window holds `address` itself.
    #[cfg(test)]
    pub(crate) fn holds_for_test(&self, address: &str) -> bool {
        self.content.has(address)
    }

    /// Discard local bytes after the host has accepted them or the mailbox has read them.
    fn release(&self) {
        let live: BTreeSet<String> = self.asked.keys().cloned().collect();
        if let Err(error) = self.content.collect_unreferenced(&live) {
            tracing::debug!(%error, "mail: kept messages stay in memory");
        }
    }
}

/// Where reading one kept message got to.
enum Read {
    /// Here: the message, or `None` when the bytes are not it.
    Here(Option<Box<Message>>),
    /// No device that could hand it over keeps it.
    Gone,
    /// Requested from the workspace.
    Asked,
}

#[cfg(test)]
mod tests {
    use ui::tilo_resource::Replica;

    use super::*;

    fn raw(id: &str, body: &str) -> Vec<u8> {
        format!(
            "From: Ada <ada@x.example>\r\nSubject: Numbers\r\nMessage-ID: <{id}@x.example>\r\n\r\n{body}\r\n"
        )
        .into_bytes()
    }

    fn copy(id: MessageId, account: &str, minutes: i64, bytes: &[u8]) -> NewCopy {
        NewCopy {
            id: id.to_string(),
            account: account.to_owned(),
            content: tilo_content::hash_bytes(bytes),
            minutes,
            folder: Folder::Archive,
            uid: 9,
            uid_validity: 3,
        }
    }

    tilo_test_env::scenario! {
        /// The copies of mail a device keeps.
        fn kept_copies_name_file_resource_and_bound_their_mail;

        phase a_kept_message_reads_back_filed_where_its_server_holds_it;
        {
            let bytes = raw("q3", "They are up.");
            let id = crate::transport::stable_message_id("q3@x.example");
            let kept = KeptCopy {
                account: "me@x.example".into(),
                content: tilo_content::hash_bytes(&bytes),
                minutes: 500,
                folder: Folder::Archive,
                uid: 9,
                uid_validity: 3,
            };
            let message = kept.message(id, &bytes).expect("the message");
            assert_eq!(message.id, id);
            assert_eq!(message.account, "me@x.example");
            assert_eq!(message.folder, Folder::Archive);
            assert_eq!(message.timestamp_min, 500);
            assert!(message.body.contains("They are up."));
            assert_eq!(
                message.server,
                Some(ServerPlace {
                    folder: Folder::Archive,
                    uid: 9,
                    uid_validity: 3,
                })
            );
            // Bytes of another message are not this one, whatever the resource
            // files them under.
            assert!(kept.message(id, &raw("other", "Not it.")).is_none());
            assert!(kept.message(id, b"not mail at all").is_none());
        }

        phase kept_mail_is_recorded_and_forgotten_by_id;
        {
            let mut replica = Replica::<MailCopies>::new().as_author("u_ada");
            let first = uuid::Uuid::from_u128(1);
            let second = uuid::Uuid::from_u128(2);
            replica
                .apply(MailCopies::KEEP_COPIES.with(kept::KeepCopies {
                    copies: vec![
                        copy(first, "me@x.example", 10, b"one"),
                        copy(second, "me@x.example", 20, b"two"),
                    ],
                }))
                .unwrap();
            let state = replica.state().unwrap();
            assert_eq!(state.copies.len(), 2);
            let recorded = state.copy("me@x.example", first).unwrap();
            assert_eq!(recorded.content, tilo_content::hash_bytes(b"one"));
            assert_eq!(
                (recorded.folder, recorded.uid, recorded.uid_validity),
                (Folder::Archive, 9, 3)
            );

            replica
                .apply(MailCopies::FORGET_COPIES.with(kept::ForgetCopies {
                    messages: vec![first.to_string()],
                    account: Some("me@x.example".into()),
                }))
                .unwrap();
            let state = replica.state().unwrap();
            assert_eq!(state.copies.len(), 1);
            assert!(state.copy("me@x.example", second).is_some());
        }

        phase importing_never_restores_a_deleted_copy_or_replaces_the_workspace_copy;
        {
            let mut replica = Replica::<MailCopies>::new().as_author("u_ada");
            let id = uuid::Uuid::from_u128(1);
            let a = "first@x.example";
            let b = "next@x.example";
            replica
                .apply(MailCopies::KEEP_COPIES.with(kept::KeepCopies {
                    copies: vec![copy(id, a, 10, b"one"), copy(id, b, 10, b"two")],
                }))
                .unwrap();
            replica
                .apply(MailCopies::FORGET_COPIES.with(kept::ForgetCopies {
                    messages: vec![id.to_string()],
                    account: Some(a.into()),
                }))
                .unwrap();
            replica
                .apply(MailCopies::IMPORT_COPIES.with(kept::ImportCopies {
                    copies: vec![copy(id, a, 11, b"old"), copy(id, b, 11, b"old")],
                }))
                .unwrap();
            let state = replica.state().unwrap();
            assert!(state.copy(a, id).is_none());
            assert_eq!(state.copy(b, id).unwrap().content, tilo_content::hash_bytes(b"two"));
        }

        phase a_copy_names_its_message_its_content_and_a_folder_its_server_has;
        {
            let mut replica = Replica::<MailCopies>::new().as_author("u_ada");
            let id = uuid::Uuid::from_u128(1);
            let mut not_an_id = copy(id, "me@x.example", 10, b"one");
            not_an_id.id = "first".into();
            let mut no_address = copy(id, "me@x.example", 10, b"one");
            no_address.content = "not-a-hash".into();
            let mut starred = copy(id, "me@x.example", 10, b"one");
            starred.folder = Folder::Starred;
            let no_account = copy(id, " ", 10, b"one");
            for refused in [not_an_id, no_address, starred, no_account] {
                assert!(
                    replica
                        .apply(MailCopies::KEEP_COPIES.with(kept::KeepCopies {
                            copies: vec![refused.clone()],
                        }))
                        .is_err(),
                    "{refused:?}"
                );
            }
            assert!(replica.state().unwrap().copies.is_empty());
        }

        phase each_account_keeps_its_newest_messages;
        {
            let mut copies = MailCopies::default();
            for (n, account) in [(1, "a"), (2, "a"), (3, "a"), (4, "b")] {
                copies.copies.push(
                    Id::from_raw(uuid::Uuid::from_u128(n).to_string()),
                    KeptCopy {
                        account: account.into(),
                        content: tilo_content::hash_bytes(&[n as u8]),
                        minutes: i64::try_from(n).unwrap(),
                        folder: Folder::Inbox,
                        uid: 1,
                        uid_validity: 1,
                    },
                );
            }
            copies.keep_newest("a", 2);
            let mut left: Vec<i64> = copies.copies.values().map(|copy| copy.minutes).collect();
            left.sort_unstable();
            assert_eq!(left, [2, 3, 4], "the oldest of a goes; b is untouched");
        }
    }
}
