//! What the mailbox knows of each message: a collection the framework keeps
//! beside the workspace's mailbox, which decides where each message is
//! stored, so that marking one message read in a mailbox of fifty thousand
//! costs that message. The mailbox itself ([`crate::resource::Mail`]) keeps
//! its folders, drafts, sends and checks, none of which grow with the mail.
//!
//! - **Several messages at once.** Archiving, moving, trashing, marking and
//!   deleting name the messages they act on (`items = messages`), and so do
//!   mail arriving and a server's confirmations: each is carried out where
//!   its messages are kept, as one step, and new mail fills the newest
//!   part and the ones after it.
//! - **What a mail server is owed.** Each message keeps what its server last
//!   confirmed ([`ServerCopy`]) beside what the person did with it, so what
//!   the server still has to be told is read off what is kept ([`owed`]) and
//!   carried out by a device that holds the account. The server's
//!   confirmation is a change where the message is kept
//!   ([`MessageStates::server_applied`]); nothing is queued anywhere else.
//! - **Deleting for good.** A purged message stays where it is, marked so,
//!   until its server confirmed the deletion and after: a late fetch of it
//!   never brings it back.

use std::collections::HashSet;
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

/// The mailbox's messages the engine keeps in one of its parts: every
/// message that arrived while it was the newest, and everything done with
/// them since.
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct MessageStates {
    /// What the mailbox knows of each message, under its id.
    pub messages: Collection<MessageState>,
    /// Messages the mailbox holds whole, under the ids their state is kept
    /// under in `messages`: mail delivered without a server
    /// ([`crate::held`]).
    #[model(readonly)]
    pub held: Collection<crate::held::HeldMessage>,
}

impl MessageStates {
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

    /// Whether message `id` is kept here, deleted for good or not.
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
            if let Some(state) = self.messages.get(&Id::from_raw(id.to_string())) {
                show_flags(mailbox, id, state);
            }
        }
    }

    /// [`Self::apply_flags`] for the messages this part keeps: what a part
    /// that changed shows, at the cost of the part rather than the mailbox.
    pub fn show_own_flags(&self, mailbox: &mut crate::model::Mailbox) {
        for (id, state) in self.messages.iter() {
            if let Ok(id) = id.as_str().parse::<crate::model::MessageId>()
                && mailbox.get(id).is_some()
            {
                show_flags(mailbox, id, state);
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
impl MessageStates {
    /// Marking `messages` read or unread.
    #[must_use]
    pub fn mark(messages: Vec<String>, read: bool) -> Invoke<Self> {
        Self::MARK_READ.with(message_states::MarkRead { messages, read })
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
    params = message_states,
    read_content = MessageStates::read_held,
    answer = crate::held::answer_part,
)]
impl MessageStates {
    /// Mark messages read
    ///
    /// Marks messages read or unread together.
    #[action(housekeeping, items = messages)]
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
    #[action(label = "mail-folder-archive", items = messages)]
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
    #[action(items = messages)]
    pub fn trash(
        &mut self,
        #[param("Ids of the messages")] messages: Vec<String>,
    ) -> Result<(), Refusal> {
        self.move_to(&messages, Folder::Trash)
    }

    /// Move messages
    ///
    /// Moves messages to one folder together.
    #[action(items = messages)]
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
    #[action(destructive, irreversible, label = "mail-delete-permanently", items = messages)]
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
    #[action(external, items = messages)]
    pub fn ingest(
        &mut self,
        #[param("The messages, with what their source reports")] messages: Vec<IngestBaseline>,
    ) {
        for baseline in messages {
            self.ingest_one(&baseline);
        }
    }

    /// Mail servers confirmed they hold what `updates` asked of them.
    #[action(external, items = updates)]
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

/// Show `state`, what the mailbox says of message `id`, on the loaded mail.
/// Only a message that differs is changed, so an unchanged mailbox keeps its
/// revision and what is derived from it.
pub(crate) fn show_flags(
    mailbox: &mut crate::model::Mailbox,
    id: crate::model::MessageId,
    state: &MessageState,
) {
    if state.purged {
        mailbox.purge(id);
        return;
    }
    let labels: Vec<String> = state.labels.iter().cloned().collect();
    let changed = mailbox.get(id).is_some_and(|message| {
        message.read != state.read
            || message.starred != state.starred
            || message.folder != state.folder
            || message.labels != labels
    });
    if changed && let Some(message) = mailbox.get_mut(id) {
        message.read = state.read;
        message.starred = state.starred;
        message.folder = state.folder;
        message.labels = labels;
    }
}

/// The message id `id` names, trimmed, when it can name one.
fn message_key(id: &str) -> Option<Id> {
    let id = id.trim();
    (!id.is_empty() && id.chars().count() <= MAX_MESSAGE_ID_CHARS).then(|| Id::from_raw(id))
}

/// Mail arriving names the message it is: one the mailbox keeps already is
/// written where that one is, a new one goes with the newest.
impl tilo_resource::NamesItem for IngestBaseline {
    fn named_item(&self) -> Option<Id> {
        message_key(&self.id)
    }
}

/// A server's confirmation names the message it is about.
impl tilo_resource::NamesItem for ServerUpdate {
    fn named_item(&self) -> Option<Id> {
        message_key(self.id())
    }
}

fn no_message() -> Refusal {
    Refusal::new("none of these messages is in the mailbox")
}

fn not_in_trash() -> Refusal {
    Refusal::new("every message must still be present in Trash before permanent deletion")
}

/// Fill in, where actions are invoked, what each reads of the whole mailbox,
/// and refuse what it does not allow: a permanent deletion of a message not
/// in the Trash, a delivery of a message the mailbox holds already, mail
/// arriving that is a retired copy of a draft.
///
/// # Errors
/// Refuses as the verb would, had it every part.
pub fn prepare(
    actions: &mut [<MessageStates as tilo_resource::Resource>::Action],
    mail: &Mail,
    parts: &[(u64, Rc<MessageStates>)],
) -> Result<(), Refusal> {
    use message_states as verb;
    for action in actions {
        match &mut action.operation {
            MessageStatesAction::Purge(verb::Purge { messages }) => {
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
                    return Err(not_in_trash());
                }
            }
            MessageStatesAction::Ingest(verb::Ingest { messages }) => {
                // A draft's copy its server was told to delete never comes back
                // with a late fetch.
                let retired = mail.retired_copy_ids();
                messages.retain(|baseline| !retired.contains(baseline.id.trim()));
            }
            MessageStatesAction::DeliverMessage(verb::DeliverMessage { message }) => {
                crate::held::prepare_delivery(message, parts)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// The newest messages the mailbox publishes as places, each with its
/// verbs: at most [`OPEN_MESSAGE_LIMIT`], from the newest part back. A copy
/// of a draft its server keeps is the draft's, not a message of its own.
#[must_use]
pub fn places(mail: &Mail, parts: &[(u64, Rc<MessageStates>)]) -> Vec<Derived<MessageStates>> {
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
        <MessageStates as tilo_resource::Resource>::KIND,
        crate::resource::message_place_id(id)
    )
}

/// The message `id` as a place: opening, previewing and answering it,
/// marking it read, archiving or deleting it.
pub(crate) fn message_place(
    id: &str,
    label: &str,
    message: &MessageState,
) -> Derived<MessageStates> {
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
            MessageStates::MARK_READ
                .bind(message_states::MarkRead::paths().messages, ids.clone())
                .and(message_states::MarkRead::paths().read, !message.read),
        )
        .with(MessageStates::ARCHIVE.bind(message_states::Archive::paths().messages, ids.clone()));
    if message.folder == Folder::Trash {
        place.with(MessageStates::PURGE.bind(message_states::Purge::paths().messages, ids))
    } else {
        place.with(MessageStates::TRASH.bind(message_states::Trash::paths().messages, ids))
    }
}

/// What the servers of a part's messages still have to be told: each
/// message's flags, folder and permanent deletion where its server holds
/// otherwise, in the order a server applies them (flags in the folder the
/// message is in, then the move). Read off the part as it is, so an undo
/// that took a change back owes nothing.
#[must_use]
pub fn owed(part: &MessageStates) -> Vec<(Id, ServerUpdate)> {
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

impl ui::framework::Chained for MessageStates {
    type Base = Mail;

    /// The workspace's messages, beside its one mailbox: where every
    /// window of Mail reads them, and where an import or an agent delivers.
    fn chain() -> ui::resource::DocumentId {
        ui::resource::DocumentId::derived(
            "mail-messages",
            &ui::resource::DocumentId::workspace_default(),
        )
    }

    fn prepare(
        actions: &mut [Self::Action],
        mail: &Mail,
        parts: &[(u64, Rc<Self>)],
        _invoker: &ui::tilo_actions::ActionInvoker,
    ) -> Result<(), Refusal> {
        prepare(actions, mail, parts)
    }

    fn places(mail: &Mail, parts: &[(u64, Rc<Self>)]) -> Vec<Derived<Self>> {
        places(mail, parts)
    }

    const BESIDE_RESOURCE: bool = true;

    /// The window shows flags on every message it loads, from any part, so
    /// it holds every part; a part bounds what one change costs.
    const HELD_WHOLE: bool = true;
}

/// A chain of the mailbox's parts held in memory, beside a mailbox of its
/// own: what a test of the window's state writes its messages into.
#[cfg(test)]
pub(crate) fn detached_chain() -> Option<ui::framework::Chain<MessageStates>> {
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
