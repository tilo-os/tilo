//! The mailbox's messages, kept in numbered parts beside the person's own
//! mailbox.
//!
//! One document cannot hold every message of a real mailbox at a cost that
//! stays flat: every holder of a document loads, verifies, diffs and saves all
//! of it, so marking one message read in a mailbox of fifty thousand would
//! cost fifty thousand. What the mailbox knows of each message lives in a
//! chain of [`MessagePart`] documents instead (`tilo_app`'s `Chained`), each
//! holding up to [`PART_MESSAGES`] messages or [`PART_BYTES`] of their text;
//! a change touches the parts holding the messages it names. The mailbox
//! itself ([`crate::resource::Mail`]) keeps its folders, drafts, sends and
//! checks, none of which grow with the mail.
//!
//! - **Where a new message goes.** To the newest part, unless it is full
//!   ([`new_part`]): then to the next. Fetched mail arriving in bulk fills the
//!   newest part and starts the ones after it, one share per part.
//! - **Where a change goes.** To the parts holding the messages it names, as
//!   one step: archiving a thousand messages writes the parts they are in and
//!   no other.
//! - **What a mail server is owed.** Each message keeps what its server last
//!   confirmed ([`ServerCopy`]) beside what the person did with it, so what
//!   the server still has to be told is read off the part ([`owed`]) and
//!   carried out by a device that holds the account. The server's
//!   confirmation is a change of the part holding the message
//!   ([`MessagePart::server_applied`]); nothing is queued anywhere else.
//! - **Deleting for good.** A purged message stays in its part, marked so,
//!   until its server confirmed the deletion and after: a late fetch of it
//!   never brings it back.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use ui::tilo_resource::{
    self, ActionCx, Collection, Derived, Id, Invoke, Model, Refusal, Set, resource,
};

use crate::actions::{IngestBaseline, ServerUpdate};
use crate::model::Folder;
use crate::resource::{
    MAX_MESSAGE_ID_CHARS, MAX_MESSAGES_PER_ACTION, Mail, MessageState, OPEN_MESSAGE_LIMIT,
    ServerCopy,
};

/// Messages a part holds before the next one starts.
pub const PART_MESSAGES: usize = 1000;

/// Text, in bytes, a part's messages hold (their labels, where their servers
/// keep them, and mail kept whole) before the next one starts: what keeps one
/// part, and so one change, cheap whatever the messages say.
pub const PART_BYTES: usize = 512 * 1024;

/// One part of the mailbox's messages: every message that arrived while it
/// was the newest, and everything done with them since.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct MessagePart {
    /// What the mailbox knows of each message, under its id.
    pub messages: Collection<MessageState>,
    /// Messages the mailbox holds whole, under the ids their state is kept
    /// under in `messages`: mail delivered without a server
    /// ([`crate::held`]).
    #[model(readonly)]
    pub held: Collection<crate::held::HeldMessage>,
}

impl MessagePart {
    /// Every message `parts` hold, oldest part first, as one: what the
    /// window, the places and a read across the mailbox go by. A message two
    /// parts both keep (never written so, but read safely) is the newer
    /// part's.
    #[must_use]
    pub fn of(parts: &[(u64, Rc<Self>)]) -> Self {
        let mut all = Self::default();
        let mut seen: HashSet<Id> = HashSet::new();
        let mut seen_held: HashSet<Id> = HashSet::new();
        for (_, part) in parts.iter().rev() {
            for (id, message) in part.messages.iter() {
                if seen.insert(id.clone()) {
                    all.messages.push(id.clone(), message.clone());
                }
            }
            for (id, held) in part.held.iter() {
                if seen_held.insert(id.clone()) {
                    all.held.push(id.clone(), held.clone());
                }
            }
        }
        all
    }

    /// The text its messages hold, in bytes: their labels, where their
    /// servers keep them, and the mail it holds whole.
    #[must_use]
    pub fn text_bytes(&self) -> usize {
        let states: usize = self
            .messages
            .iter()
            .map(|(id, message)| {
                let server = message.server.as_ref().map_or(0, |server| {
                    server.account.len().saturating_add(server.locator.len())
                });
                id.as_str()
                    .len()
                    .saturating_add(message.label.len())
                    .saturating_add(server)
                    .saturating_add(message.labels.iter().map(String::len).sum::<usize>())
            })
            .sum();
        let held: usize = self
            .held
            .iter()
            .map(|(_, held)| {
                held.from
                    .len()
                    .saturating_add(held.to.len())
                    .saturating_add(held.cc.len())
                    .saturating_add(held.subject.len())
                    .saturating_add(held.body.len())
                    .saturating_add(held.message_id.len())
                    .saturating_add(held.conversation.len())
            })
            .sum();
        states.saturating_add(held)
    }

    /// Whether the part takes no new message: as many as a part holds, or
    /// as much text.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.messages.len() >= PART_MESSAGES || self.text_bytes() >= PART_BYTES
    }

    /// Whether the part keeps message `id`, deleted for good or not.
    #[must_use]
    pub fn holds(&self, id: &Id) -> bool {
        self.messages.contains(id) || self.held.contains(id)
    }

    /// What the mailbox holds of the message `id`, unless it was deleted
    /// for good.
    #[must_use]
    pub fn message(&self, id: &str) -> Option<&MessageState> {
        self.messages
            .get(&Id::from_raw(id))
            .filter(|message| !message.purged)
    }

    /// Whether the mailbox knows message `id`, deleted for good or not.
    #[must_use]
    pub fn knows(&self, id: &str) -> bool {
        self.messages.contains(&Id::from_raw(id.to_owned()))
    }

    /// Show the reader's flags, folders and labels on the loaded mail, and
    /// drop what was deleted for good. The mail's content is the engine's;
    /// what the reader did with it is the mailbox's.
    pub fn apply_flags(&self, mailbox: &mut crate::model::Mailbox) {
        let ids: Vec<_> = mailbox.messages.keys().copied().collect();
        for id in ids {
            let Some(state) = self.messages.get(&Id::from_raw(id.to_string())) else {
                continue;
            };
            if state.purged {
                mailbox.purge(id);
                continue;
            }
            let labels: Vec<String> = state.labels.iter().cloned().collect();
            let changed = mailbox.get(id).is_some_and(|message| {
                message.read != state.read
                    || message.starred != state.starred
                    || message.folder != state.folder
                    || message.labels != labels
            });
            // Only a message that differs is changed, so an unchanged
            // mailbox keeps its revision and what is derived from it.
            if changed && let Some(message) = mailbox.get_mut(id) {
                message.read = state.read;
                message.starred = state.starred;
                message.folder = state.folder;
                message.labels = labels;
            }
        }
    }

    /// The ids of `messages` the part keeps and has not deleted for good, at
    /// most [`MAX_MESSAGES_PER_ACTION`] of them; refused when none is.
    fn present(&self, messages: &[String]) -> Result<Vec<Id>, Refusal> {
        let mut seen = HashSet::new();
        let ids: Vec<Id> = messages
            .iter()
            .filter_map(|id| message_key(id))
            .filter(|id| self.message(id.as_str()).is_some() && seen.insert(id.clone()))
            .take(MAX_MESSAGES_PER_ACTION)
            .collect();
        if ids.is_empty() {
            Err(no_message())
        } else {
            Ok(ids)
        }
    }

    fn move_to(&mut self, messages: &[String], folder: Folder) -> Result<(), Refusal> {
        for id in self.present(messages)? {
            if let Some(message) = self.messages.get_mut(&id) {
                message.folder = folder;
            }
        }
        Ok(())
    }

    fn ingest_one(&mut self, baseline: &IngestBaseline) {
        let Some(key) = message_key(&baseline.id) else {
            return;
        };
        let folder = baseline.folder.as_deref().and_then(Folder::from_key);
        let server = match (&baseline.account, &baseline.locator) {
            (Some(account), Some(locator)) => Some((account.clone(), locator.clone())),
            _ => None,
        };
        let Some(message) = self.messages.get_mut(&key) else {
            let state = MessageState {
                read: baseline.read.unwrap_or(false),
                starred: baseline.starred.unwrap_or(false),
                folder: folder.unwrap_or_default(),
                labels: Set::new(),
                server: server.map(|(account, locator)| ServerCopy {
                    account,
                    locator,
                    read: baseline.read.unwrap_or(false),
                    starred: baseline.starred.unwrap_or(false),
                    folder: folder.unwrap_or_default(),
                    removed: false,
                }),
                purged: false,
                label: baseline.label.clone().unwrap_or_default(),
            };
            self.messages.push(key, state);
            return;
        };
        if message.purged {
            return;
        }
        if let Some(label) = baseline
            .label
            .as_ref()
            .filter(|label| **label != message.label)
        {
            message.label.clone_from(label);
        }
        let Some((account, locator)) = server else {
            return;
        };
        let Some(known) = message.server.as_mut() else {
            message.server = Some(ServerCopy {
                account,
                locator,
                read: baseline.read.unwrap_or(message.read),
                starred: baseline.starred.unwrap_or(message.starred),
                folder: folder.unwrap_or(message.folder),
                removed: false,
            });
            return;
        };
        known.account = account;
        known.locator = locator;
        if let Some(read) = baseline.read.filter(|read| *read != known.read) {
            known.read = read;
            message.read = read;
        }
        if let Some(starred) = baseline.starred.filter(|starred| *starred != known.starred) {
            known.starred = starred;
            message.starred = starred;
        }
        if let Some(folder) = folder.filter(|folder| *folder != known.folder) {
            known.folder = folder;
            message.folder = folder;
        }
    }
}

/// Where the mailbox's messages are kept for the window's verbs.
impl MessagePart {
    /// Marking `messages` read or unread.
    #[must_use]
    pub fn mark(messages: Vec<String>, read: bool) -> Invoke<Self> {
        Self::MARK_READ.with(message_part::MarkRead { messages, read })
    }

    /// The edit starring the message `id`, or not.
    pub fn star(id: &str, starred: bool) -> tilo_resource::Edit<Self> {
        Self::paths()
            .messages
            .item(&Id::from_raw(id))
            .starred
            .set(starred)
    }
}

#[resource(
    kind = "mail.messages",
    params = message_part,
    read_content = MessagePart::read_held,
    answer = crate::held::answer_part,
)]
impl MessagePart {
    /// Mark messages read
    ///
    /// Marks messages read or unread together.
    #[action(housekeeping)]
    pub fn mark_read(
        &mut self,
        #[param("Ids of the messages")] messages: Vec<String>,
        #[param("Read, or unread")] read: bool,
    ) -> Result<(), Refusal> {
        for id in self.present(&messages)? {
            if let Some(message) = self.messages.get_mut(&id) {
                message.read = read;
            }
        }
        Ok(())
    }

    /// Archive messages
    ///
    /// Moves messages to the Archive together.
    #[action(label = "mail-folder-archive")]
    pub fn archive(
        &mut self,
        #[param("Ids of the messages")] messages: Vec<String>,
    ) -> Result<(), Refusal> {
        self.move_to(&messages, Folder::Archive)
    }

    /// Move messages to Trash
    ///
    /// Moves messages to the Trash together, from where they can still be
    /// brought back.
    #[action]
    pub fn trash(
        &mut self,
        #[param("Ids of the messages")] messages: Vec<String>,
    ) -> Result<(), Refusal> {
        self.move_to(&messages, Folder::Trash)
    }

    /// Move messages
    ///
    /// Moves messages to one folder together.
    #[action]
    pub fn move_messages(
        &mut self,
        #[param("Ids of the messages")] messages: Vec<String>,
        #[param("The folder")] to: Folder,
    ) -> Result<(), Refusal> {
        self.move_to(&messages, to)
    }

    /// Delete permanently
    ///
    /// Deletes messages already in the Trash for good: all of them, or none
    /// when one is no longer there.
    #[action(destructive, irreversible, label = "mail-delete-permanently")]
    pub fn purge(
        &mut self,
        #[param("Ids of the messages, every one in the Trash")] messages: Vec<String>,
    ) -> Result<(), Refusal> {
        let ids = self.present(&messages)?;
        if ids.iter().any(|id| {
            self.messages
                .get(id)
                .is_none_or(|message| message.folder != Folder::Trash)
        }) {
            return Err(not_in_trash());
        }
        for id in ids {
            if let Some(message) = self.messages.get_mut(&id) {
                message.purged = true;
            }
        }
        Ok(())
    }

    /// Messages fetched or found: each joins the mailbox with the flags its
    /// source reports, and a fetched one keeps what its server holds. What
    /// the mailbox already says wins, except where the server changed since
    /// it last reported (another mail client read the message).
    #[action(external)]
    pub fn ingest(
        &mut self,
        #[param("The messages, with what their source reports")] messages: Vec<IngestBaseline>,
    ) {
        for baseline in messages {
            self.ingest_one(&baseline);
        }
    }

    /// Mail servers confirmed they hold what `updates` asked of them.
    #[action(external)]
    pub fn server_applied(
        &mut self,
        #[param("What the servers were told")] updates: Vec<ServerUpdate>,
    ) {
        for update in updates {
            let Some(message) = self.messages.get_mut(&Id::from_raw(update.id())) else {
                continue;
            };
            let Some(server) = message.server.as_mut() else {
                continue;
            };
            match update {
                ServerUpdate::Flags { read, starred, .. } => {
                    server.read = read;
                    server.starred = starred;
                }
                ServerUpdate::Move { to, .. } => server.folder = to,
                ServerUpdate::Remove { .. } => server.removed = true,
                ServerUpdate::SaveDraft { .. } => {}
            }
        }
    }

    /// Deliver message
    ///
    /// Puts an incoming message in the mailbox itself, in a folder, as mail
    /// fetched from a server would arrive: for a scenario, an event a workspace
    /// hands on, or a worker with no account. A reply joins the
    /// conversation it answers. Nothing is sent. Answers its id.
    #[action(answers, local, audience = "agents")]
    pub fn deliver_message(
        &mut self,
        cx: &mut ActionCx,
        #[param(flatten)] message: crate::held::Delivery,
    ) -> Result<(), Refusal> {
        self.deliver(cx, &message).map(drop)
    }
}

/// The part a new message goes to, given the newest part held (its number
/// and state): that part, or the next once it is full.
#[must_use]
pub fn new_part(head: Option<(u64, &MessagePart)>) -> u64 {
    match head {
        None => 0,
        Some((index, part)) if part.is_full() => index.saturating_add(1),
        Some((index, _)) => index,
    }
}

/// The message id `id` names, trimmed, when it can name one.
fn message_key(id: &str) -> Option<Id> {
    let id = id.trim();
    (!id.is_empty() && id.chars().count() <= MAX_MESSAGE_ID_CHARS).then(|| Id::from_raw(id))
}

fn no_message() -> Refusal {
    Refusal::new("none of these messages is in the mailbox")
}

fn not_in_trash() -> Refusal {
    Refusal::new("every message must still be present in Trash before permanent deletion")
}

/// The part holding message `id`, newest first.
fn holding(parts: &[(u64, Rc<MessagePart>)], id: &Id) -> Option<u64> {
    parts
        .iter()
        .rev()
        .find(|(_, part)| part.holds(id))
        .map(|(index, _)| *index)
}

/// `ids` grouped by the part holding each, the parts in order; an id no part
/// holds, or one deleted for good, is left out, so no share is refused.
fn by_part(parts: &[(u64, Rc<MessagePart>)], ids: &[String]) -> Vec<(u64, Vec<String>)> {
    let mut held: HashMap<Id, u64> = HashMap::new();
    for (index, part) in parts {
        for (id, message) in part.messages.iter() {
            if !message.purged {
                held.insert(id.clone(), *index);
            }
        }
    }
    let mut grouped: std::collections::BTreeMap<u64, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut seen = HashSet::new();
    for id in ids {
        let Some(key) = message_key(id) else {
            continue;
        };
        if let Some(index) = held.get(&key)
            && seen.insert(key.clone())
        {
            grouped
                .entry(*index)
                .or_default()
                .push(key.as_str().to_owned());
        }
    }
    grouped.into_iter().collect()
}

/// The ids a verb over many messages names.
fn messages_named(action: &MessagePartAction) -> Option<&[String]> {
    use message_part as verb;
    match action {
        MessagePartAction::MarkRead(verb::MarkRead { messages, .. })
        | MessagePartAction::Archive(verb::Archive { messages })
        | MessagePartAction::Trash(verb::Trash { messages })
        | MessagePartAction::MoveMessages(verb::MoveMessages { messages, .. })
        | MessagePartAction::Purge(verb::Purge { messages }) => Some(messages),
        _ => None,
    }
}

/// The verb over many messages `action` is, over `messages` instead.
fn over(action: &MessagePartAction, messages: Vec<String>) -> Option<MessagePartAction> {
    use message_part as verb;
    Some(match action {
        MessagePartAction::MarkRead(verb::MarkRead { read, .. }) => {
            MessagePartAction::MarkRead(verb::MarkRead {
                messages,
                read: *read,
            })
        }
        MessagePartAction::Archive(_) => MessagePartAction::Archive(verb::Archive { messages }),
        MessagePartAction::Trash(_) => MessagePartAction::Trash(verb::Trash { messages }),
        MessagePartAction::MoveMessages(verb::MoveMessages { to, .. }) => {
            MessagePartAction::MoveMessages(verb::MoveMessages { messages, to: *to })
        }
        MessagePartAction::Purge(_) => MessagePartAction::Purge(verb::Purge { messages }),
        _ => return None,
    })
}

/// The part's number beside the share of an action it carries out.
type Shares = Vec<(u64, <MessagePart as tilo_resource::Resource>::Action)>;

/// What a verb over messages of several parts carries out in each: one share
/// per part holding some of the messages it names; for messages arriving,
/// those the mailbox knows go to the part holding them and new ones fill the
/// newest part and start the ones after it.
///
/// # Errors
/// Refuses a verb naming no message any part holds.
pub fn split(
    action: &<MessagePart as tilo_resource::Resource>::Action,
    parts: &[(u64, Rc<MessagePart>)],
) -> Result<Option<Shares>, String> {
    use message_part as verb;
    let share = |operation: MessagePartAction| {
        let mut share = action.clone();
        share.operation = operation;
        share
    };
    if let Some(messages) = messages_named(&action.operation) {
        let grouped = by_part(parts, messages);
        if grouped.is_empty() {
            return Err(no_message().to_string());
        }
        return Ok(Some(
            grouped
                .into_iter()
                .filter_map(|(index, ids)| Some((index, share(over(&action.operation, ids)?))))
                .collect(),
        ));
    }
    match &action.operation {
        MessagePartAction::ServerApplied(verb::ServerApplied { updates }) => {
            let mut grouped: std::collections::BTreeMap<u64, Vec<ServerUpdate>> =
                std::collections::BTreeMap::new();
            for update in updates {
                if let Some(index) = holding(parts, &Id::from_raw(update.id())) {
                    grouped.entry(index).or_default().push(update.clone());
                }
            }
            if grouped.is_empty() {
                return Err(no_message().to_string());
            }
            Ok(Some(
                grouped
                    .into_iter()
                    .map(|(index, updates)| {
                        (
                            index,
                            share(MessagePartAction::ServerApplied(verb::ServerApplied {
                                updates,
                            })),
                        )
                    })
                    .collect(),
            ))
        }
        MessagePartAction::Ingest(verb::Ingest { messages }) => Ok(Some(
            ingest_shares(parts, messages)
                .into_iter()
                .map(|(index, messages)| {
                    (
                        index,
                        share(MessagePartAction::Ingest(verb::Ingest { messages })),
                    )
                })
                .collect(),
        )),
        _ => Ok(None),
    }
}

/// Where each message arriving goes: one the mailbox knows to the part
/// holding it, a new one to the newest part until it is full, then to the
/// parts after it.
fn ingest_shares(
    parts: &[(u64, Rc<MessagePart>)],
    messages: &[IngestBaseline],
) -> Vec<(u64, Vec<IngestBaseline>)> {
    let mut kept: HashMap<Id, u64> = HashMap::new();
    for (index, part) in parts {
        for (id, _) in part.messages.iter() {
            kept.insert(id.clone(), *index);
        }
    }
    let (mut newest, mut free) = match parts.last() {
        None => (0, PART_MESSAGES),
        Some((index, part)) if part.is_full() => (index.saturating_add(1), PART_MESSAGES),
        Some((index, part)) => (*index, PART_MESSAGES.saturating_sub(part.messages.len())),
    };
    let mut grouped: std::collections::BTreeMap<u64, Vec<IngestBaseline>> =
        std::collections::BTreeMap::new();
    let mut arriving: HashSet<Id> = HashSet::new();
    for baseline in messages {
        let Some(key) = message_key(&baseline.id) else {
            continue;
        };
        if let Some(index) = kept.get(&key) {
            grouped.entry(*index).or_default().push(baseline.clone());
            continue;
        }
        if !arriving.insert(key.clone()) {
            // The same message twice in one arrival: it goes where the
            // first went, and the part applies both in order.
            if let Some((_, list)) = grouped
                .iter_mut()
                .find(|(_, list)| list.iter().any(|other| other.id.trim() == key.as_str()))
            {
                list.push(baseline.clone());
            }
            continue;
        }
        if free == 0 {
            newest = newest.saturating_add(1);
            free = PART_MESSAGES;
        }
        free = free.saturating_sub(1);
        grouped.entry(newest).or_default().push(baseline.clone());
    }
    grouped.into_iter().collect()
}

/// The part an action on one message changes: the one holding it, or for a
/// new message the newest part (or the next, once that is full).
///
/// # Errors
/// Refuses an edit of a message no part held here keeps.
pub fn part_for(
    action: &<MessagePart as tilo_resource::Resource>::Action,
    parts: &[(u64, Rc<MessagePart>)],
) -> Result<u64, String> {
    let head = || new_part(parts.last().map(|(index, part)| (*index, &**part)));
    match &action.operation {
        MessagePartAction::DeliverMessage(_) => Ok(head()),
        MessagePartAction::FieldEdit(edit) => match edit.path() {
            [
                tilo_resource::Segment::Field(field),
                tilo_resource::Segment::Item(message),
                ..,
            ] if field == "messages" || field == "held" => {
                holding(parts, message).ok_or_else(|| no_message().to_string())
            }
            _ => Ok(parts.last().map_or(0, |(index, _)| *index)),
        },
        _ => Ok(parts.last().map_or(0, |(index, _)| *index)),
    }
}

/// Fill in, where an action is invoked, what it reads of the whole mailbox,
/// and refuse what it does not allow: a permanent deletion of a message not
/// in the Trash, a delivery of a message the mailbox holds already, mail
/// arriving that is a retired copy of a draft.
///
/// # Errors
/// Refuses as the verb would, had it every part.
pub fn prepare(
    action: &mut <MessagePart as tilo_resource::Resource>::Action,
    mail: &Mail,
    parts: &[(u64, Rc<MessagePart>)],
) -> Result<(), String> {
    use message_part as verb;
    match &mut action.operation {
        MessagePartAction::Purge(verb::Purge { messages }) => {
            let in_trash = |id: &String| {
                message_key(id).is_none_or(|key| {
                    parts
                        .iter()
                        .rev()
                        .find_map(|(_, part)| part.message(key.as_str()))
                        .is_none_or(|message| message.folder == Folder::Trash)
                })
            };
            if !messages.iter().all(in_trash) {
                return Err(not_in_trash().to_string());
            }
        }
        MessagePartAction::Ingest(verb::Ingest { messages }) => {
            // A draft's copy its server was told to delete never comes back
            // with a late fetch.
            let retired = mail.retired_copy_ids();
            messages.retain(|baseline| !retired.contains(baseline.id.trim()));
        }
        MessagePartAction::DeliverMessage(verb::DeliverMessage { message }) => {
            crate::held::prepare_delivery(message, parts)?;
        }
        _ => {}
    }
    Ok(())
}

/// The newest messages the mailbox publishes as places, each with its
/// verbs: at most [`OPEN_MESSAGE_LIMIT`], from the newest part back. A copy
/// of a draft its server keeps is the draft's, not a message of its own.
#[must_use]
pub fn places(mail: &Mail, parts: &[(u64, Rc<MessagePart>)]) -> Vec<Derived<MessagePart>> {
    let copies: HashSet<String> = mail.draft_copy_ids().into_iter().collect();
    parts
        .iter()
        .rev()
        .flat_map(|(_, part)| {
            let mut listed: Vec<_> = part
                .messages
                .iter()
                .filter(|(id, message)| !message.purged && !copies.contains(id.as_str()))
                .map(|(id, message)| (id.clone(), message.clone()))
                .collect();
            listed.reverse();
            listed
        })
        .take(OPEN_MESSAGE_LIMIT)
        .map(|(id, message)| {
            let label = if message.label.is_empty() {
                id.as_str().to_owned()
            } else {
                message.label.clone()
            };
            message_place(id.as_str(), &label, &message)
        })
        .collect()
}

/// The published address of the message `id`'s place, beside the mailbox.
#[must_use]
pub fn message_resource(id: &str) -> String {
    format!(
        "{}/derived/{}",
        <MessagePart as tilo_resource::Resource>::KIND,
        crate::resource::message_place_id(id)
    )
}

/// The message `id` as a place: opening, previewing and answering it,
/// marking it read, archiving or deleting it.
pub(crate) fn message_place(id: &str, label: &str, message: &MessageState) -> Derived<MessagePart> {
    use crate::actions::{MailEffect, MailTarget};
    let ids = vec![id.to_owned()];
    let place = Derived::new(label)
        .of_kind("message")
        .identified(crate::resource::message_place_id(id))
        .with_effect(&MailEffect::Open {
            target: MailTarget::Message(id.to_owned()),
        })
        .with_effect(&MailEffect::Inspect {
            message: id.to_owned(),
        })
        .with_effect(&MailEffect::ReadMessage {
            message: id.to_owned(),
        })
        .with_effect(&MailEffect::Reply {
            message: id.to_owned(),
        })
        .with_effect(&MailEffect::ReplyAll {
            message: id.to_owned(),
        })
        .with_effect(&MailEffect::Forward {
            message: id.to_owned(),
        })
        .with_effect(&MailEffect::LoadRemoteContent {
            message: id.to_owned(),
        })
        .with_effect(&MailEffect::AlwaysLoadRemoteContent {
            message: id.to_owned(),
        })
        .with_effect_asking(
            &MailEffect::OpenLink {
                message: id.to_owned(),
                url: String::new(),
            },
            &["url"],
        )
        .with(
            MessagePart::MARK_READ
                .bind(message_part::MarkRead::paths().messages, ids.clone())
                .and(message_part::MarkRead::paths().read, !message.read),
        )
        .with(MessagePart::ARCHIVE.bind(message_part::Archive::paths().messages, ids.clone()));
    if message.folder == Folder::Trash {
        place.with(MessagePart::PURGE.bind(message_part::Purge::paths().messages, ids))
    } else {
        place.with(MessagePart::TRASH.bind(message_part::Trash::paths().messages, ids))
    }
}

/// What the servers of a part's messages still have to be told: each
/// message's flags, folder and permanent deletion where its server holds
/// otherwise, in the order a server applies them (flags in the folder the
/// message is in, then the move). Read off the part as it is, so an undo
/// that took a change back owes nothing.
#[must_use]
pub fn owed(part: &MessagePart) -> Vec<(Id, ServerUpdate)> {
    let mut updates = Vec::new();
    for (id, message) in part.messages.iter() {
        let Some(server) = &message.server else {
            continue;
        };
        let (key, account, locator) = (
            id.as_str().to_owned(),
            server.account.clone(),
            server.locator.clone(),
        );
        if message.purged {
            if !server.removed {
                updates.push((
                    id.clone(),
                    ServerUpdate::Remove {
                        id: key,
                        account,
                        locator,
                        folder: server.folder,
                    },
                ));
            }
            continue;
        }
        if message.read != server.read || message.starred != server.starred {
            updates.push((
                id.clone(),
                ServerUpdate::Flags {
                    id: key.clone(),
                    account: account.clone(),
                    locator: locator.clone(),
                    folder: server.folder,
                    read: message.read,
                    starred: message.starred,
                },
            ));
        }
        if message.folder != server.folder {
            updates.push((
                id.clone(),
                ServerUpdate::Move {
                    id: key,
                    account,
                    locator,
                    from: server.folder,
                    to: message.folder,
                },
            ));
        }
    }
    updates
}

impl ui::framework::Chained for MessagePart {
    type Base = Mail;

    /// Never used: the messages live only beside the person's mailbox.
    fn chain() -> ui::resource::DocumentId {
        ui::resource::DocumentId::derived(
            "mail-messages",
            &ui::resource::DocumentId::workspace_default(),
        )
    }

    /// The parts are the person's own mailbox's (`mailbox.part(n)`),
    /// so they are as private as it and sync with it. A mailbox kept as the
    /// app's own document of the workspace (someone with no root identity,
    /// on one device) has its parts under the chain's own name: that
    /// document has no parts of its own.
    fn chain_beside(base: &ui::resource::DocumentId) -> ui::resource::DocumentId {
        if base.is_package_primary() {
            Self::chain()
        } else {
            base.clone()
        }
    }

    fn part_for(action: &Self::Action, parts: &[(u64, Rc<Self>)]) -> Result<u64, String> {
        part_for(action, parts)
    }

    fn split(
        action: &Self::Action,
        parts: &[(u64, Rc<Self>)],
    ) -> Result<Option<Vec<(u64, Self::Action)>>, String> {
        split(action, parts)
    }

    fn prepare(
        action: &mut Self::Action,
        mail: &Mail,
        parts: &[(u64, Rc<Self>)],
        _invoker: &ui::tilo_actions::ActionInvoker,
    ) -> Result<(), String> {
        prepare(action, mail, parts)
    }

    fn places(mail: &Mail, parts: &[(u64, Rc<Self>)]) -> Vec<Derived<Self>> {
        places(mail, parts)
    }

    const LISTED: &'static [&'static str] = &["label", "folder", "read", "starred"];

    const BESIDE_RESOURCE: bool = true;

    /// The window shows flags on every message it loads, from any part, so
    /// it holds every part; a part bounds what one change costs.
    const HELD_WHOLE: bool = true;
}

/// A chain of the mailbox's parts held in memory, beside a mailbox of its
/// own: what a test of the window's state writes its messages into.
#[cfg(test)]
pub(crate) fn detached_chain() -> Option<ui::framework::Chain<MessagePart>> {
    let me = tilo_resource::MemberId::person("ada");
    let mailbox = ui::resource::DocumentId::derived(
        "mail-test",
        &ui::resource::DocumentId::workspace_default(),
    );
    let mail = Rc::new(Mail::default());
    ui::framework::Chain::detached_beside_in(
        ui::resource::ResourceStore::in_memory(),
        "so.tilo.mail",
        Some(&mailbox),
        &me,
        move || Rc::clone(&mail),
    )
}

#[cfg(test)]
mod tests;
