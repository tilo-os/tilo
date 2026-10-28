//! The mail the person's devices hold for one another.
//!
//! A message a device fetched from its server is kept, as the server sent
//! it, as content of the person's own copies record ([`MailCopies`]): a
//! record of their own room, which follows them to their other devices and
//! never reaches a collaborator, whatever room Mail is open in. The record
//! names each message and where its server holds it; the bytes travel on
//! the record's own route, from whichever of the person's devices keeps
//! them, verified against the address the record names.
//!
//! Another of the person's devices that lacks a message the record names
//! takes it from there, newest first, without asking any server: a device
//! whose server cannot be reached still shows the mail its other devices
//! fetched, and once its server answers again it is asked only for what is
//! newer than what it holds. The server stays the authority over its
//! messages: one it reports removed is dropped from the record by
//! whichever device hears so.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use tilo_content::{ContentState, ContentStore};
use ui::framework::PersonRecord;
use ui::tilo_resource::{self, Collection, Id, JsonSchema, Model, Refusal};

use crate::model::{Folder, Message, MessageId, ServerPlace};

/// The largest message kept for the person's other devices; a larger one
/// they fetch from its server.
pub(crate) const MAX_COPY_BYTES: usize = 16 * 1024 * 1024;

/// How many messages one window asks the person's other devices for at a
/// time; it asks for more as they arrive.
const MAX_ASKED: usize = 32;

const MAX_ACCOUNT_CHARS: usize = 320;

/// The mail the person's devices fetched, each message kept as its server
/// sent it, for their other devices to take.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct MailCopies {
    /// Each message under its id in the mailbox
    #[model(readonly)]
    pub copies: Collection<KeptCopy>,
}

/// One message a device fetched, and where its server holds it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = KeptCopy::valid)]
pub struct KeptCopy {
    /// The account it was fetched through
    pub account: String,
    /// The message as its server sent it, by content address
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
        if !tilo_content::valid_hash(&self.content) {
            return Err(Refusal::new("a kept message names its content by address"));
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

/// A message to keep for the person's other devices: the parameters of
/// [`MailCopies::keep_copies`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NewCopy {
    /// Its id in the mailbox
    pub id: String,
    /// The account it was fetched through
    pub account: String,
    /// The message as its server sent it, by content address
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

#[ui::tilo_resource::resource(kind = "mail.copies", label = "Mail on your devices", params = kept)]
impl MailCopies {
    /// Keep fetched mail
    ///
    /// Records messages a device fetched from their servers, each kept as
    /// its server sent it, for the person's other devices to take. Each
    /// account keeps its newest messages, as many as a device keeps.
    #[action(external)]
    pub fn keep_copies(
        &mut self,
        #[param("The messages, each with where its server holds it")] copies: Vec<NewCopy>,
    ) -> Result<(), Refusal> {
        let mut accounts = BTreeSet::new();
        for copy in copies {
            if uuid::Uuid::parse_str(&copy.id).is_err() {
                return Err(Refusal::new("a kept message is filed under its mailbox id"));
            }
            let kept = KeptCopy {
                account: copy.account,
                content: copy.content.to_ascii_lowercase(),
                minutes: copy.minutes,
                folder: copy.folder,
                uid: copy.uid,
                uid_validity: copy.uid_validity,
            };
            kept.valid()?;
            accounts.insert(kept.account.clone());
            self.copies.push(Id::from_raw(copy.id), kept);
        }
        for account in accounts {
            self.keep_newest(&account, crate::store::KEEP);
        }
        Ok(())
    }

    /// Forget fetched mail
    ///
    /// Drops messages their servers no longer hold.
    #[action(external)]
    pub fn forget_copies(&mut self, #[param("Ids of the messages")] messages: Vec<String>) {
        let gone: HashSet<String> = messages.into_iter().collect();
        self.copies.retain(|id, _| !gone.contains(id.as_str()));
    }
}

impl MailCopies {
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
        order.sort_by_key(|(_, minutes)| std::cmp::Reverse(*minutes));
        let dropped: HashSet<Id> = order.into_iter().skip(most).map(|(id, _)| id).collect();
        self.copies.retain(|id, _| !dropped.contains(id));
    }
}

/// The person's copies record as one window uses it: what this device
/// fetches is kept there, and what another of their devices kept is taken
/// from there.
pub(crate) struct Copies {
    record: PersonRecord<MailCopies>,
    /// The record's content: kept with the record on the person's devices
    /// and carried on its route.
    content: ContentStore,
    /// Messages asked of the person's other devices and not here yet, by
    /// the address of their content.
    asked: BTreeMap<String, (MessageId, KeptCopy)>,
    /// Messages taken, or that cannot arrive, in this window: not asked
    /// for again.
    settled: HashSet<MessageId>,
    /// What content had reached this process, and how far the record had
    /// moved, when this last looked.
    looked: Option<(u64, u64)>,
}

impl Copies {
    pub(crate) fn new(record: PersonRecord<MailCopies>, content: ContentStore) -> Self {
        Self {
            record,
            content,
            asked: BTreeMap::new(),
            settled: HashSet::new(),
            looked: None,
        }
    }

    /// Keep what `account`'s server just sent, as it sent it
    /// (`originals`, by id), for the person's other devices. Mail without
    /// a `Message-ID`, which another device could not file under the same
    /// id, and messages larger than [`MAX_COPY_BYTES`] are not kept.
    pub(crate) fn keep(
        &self,
        account: &str,
        messages: &[Message],
        originals: &[(MessageId, Vec<u8>)],
    ) {
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
            let content = tilo_content::hash_bytes(original);
            let known = held.copies.get(&Id::from_raw(id.to_string()));
            if known.is_some_and(|known| {
                known.content == content
                    && known.folder == at.folder
                    && known.uid == at.uid
                    && known.uid_validity == at.uid_validity
            }) {
                continue;
            }
            if let Err(error) = self.content.put(original) {
                tracing::warn!(%error, "mail: a message could not be kept for the person's devices");
                continue;
            }
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
            tracing::warn!(%error, "mail: fetched mail could not be kept for the person's devices");
        }
        self.release();
    }

    /// Drop `gone`, which their server no longer holds.
    pub(crate) fn forget(&self, gone: &[MessageId]) {
        let held = self.record.get();
        let messages: Vec<String> = gone
            .iter()
            .map(MessageId::to_string)
            .filter(|id| held.copies.contains(&Id::from_raw(id.clone())))
            .collect();
        if messages.is_empty() {
            return;
        }
        if let Err(error) = self
            .record
            .invoke(&MailCopies::FORGET_COPIES.with(kept::ForgetCopies { messages }))
        {
            tracing::warn!(%error, "mail: removed mail could not be dropped for the person's devices");
        }
    }

    /// Whether there may be something new to take: content reached this
    /// process, or the record moved, since this last looked.
    pub(crate) fn due(&self) -> bool {
        self.looked != Some(self.now())
    }

    fn now(&self) -> (u64, u64) {
        (
            ui::collaboration::record_content::arrivals(),
            self.record.revision(),
        )
    }

    /// The messages of `accounts` the person's other devices kept that are
    /// here now, and not `held` (the mailbox has it, or it was deleted for
    /// good). What is not here yet is asked for, newest first, a few at a
    /// time; it is taken once it arrives.
    pub(crate) fn take(
        &mut self,
        accounts: &[String],
        held: impl Fn(MessageId) -> bool,
    ) -> Vec<Message> {
        self.looked = Some(self.now());
        let mut taken = Vec::new();
        for (address, (id, copy)) in std::mem::take(&mut self.asked) {
            if held(id) {
                self.settled.insert(id);
                continue;
            }
            match self.read(&address, id, &copy) {
                Read::Here(message) => taken.extend(message.map(|message| *message)),
                Read::Gone => {}
                Read::Asked => {
                    self.asked.insert(address, (id, copy));
                }
            }
        }
        if self.asked.len() < MAX_ASKED {
            let record = self.record.get();
            let asked: HashSet<MessageId> = self.asked.values().map(|(id, _)| *id).collect();
            let mut missing: Vec<(MessageId, &KeptCopy)> = record
                .copies
                .iter()
                .filter_map(|(key, copy)| {
                    let id = uuid::Uuid::parse_str(key.as_str()).ok()?;
                    (accounts.contains(&copy.account)
                        && !self.settled.contains(&id)
                        && !asked.contains(&id)
                        && !held(id))
                    .then_some((id, copy))
                })
                .collect();
            missing.sort_by_key(|(_, copy)| std::cmp::Reverse(copy.minutes));
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

    /// Read `copy` from the record's content, asking the person's other
    /// devices for it when it is not here.
    fn read(&mut self, address: &str, id: MessageId, copy: &KeptCopy) -> Read {
        if let Some(bytes) = self.content.read(address) {
            self.settled.insert(id);
            let message = copy.message(id, &bytes);
            if message.is_none() {
                tracing::warn!("mail: a kept message is not the message it is filed under");
            }
            return Read::Here(message.map(Box::new));
        }
        if self.content.state(address) == ContentState::Unavailable {
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

    /// Let go of the bytes read or kept here: the person's devices keep
    /// them with the record, and the mailbox keeps what it took.
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
    /// Asked of the person's other devices.
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

    #[test]
    fn a_kept_message_reads_back_filed_where_its_server_holds_it() {
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
        // Bytes of another message are not this one, whatever the record
        // files them under.
        assert!(kept.message(id, &raw("other", "Not it.")).is_none());
        assert!(kept.message(id, b"not mail at all").is_none());
    }

    #[test]
    fn kept_mail_is_recorded_and_forgotten_by_id() {
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
        let recorded = state.copies.get(&Id::from_raw(first.to_string())).unwrap();
        assert_eq!(recorded.content, tilo_content::hash_bytes(b"one"));
        assert_eq!(
            (recorded.folder, recorded.uid, recorded.uid_validity),
            (Folder::Archive, 9, 3)
        );

        replica
            .apply(MailCopies::FORGET_COPIES.with(kept::ForgetCopies {
                messages: vec![first.to_string()],
            }))
            .unwrap();
        let state = replica.state().unwrap();
        assert_eq!(state.copies.len(), 1);
        assert!(state.copies.contains(&Id::from_raw(second.to_string())));
    }

    #[test]
    fn a_copy_names_its_message_its_content_and_a_folder_its_server_has() {
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

    #[test]
    fn each_account_keeps_its_newest_messages() {
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
