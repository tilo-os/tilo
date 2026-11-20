//! Bringing the mailbox a member kept as their own into the workspace's.
//!
//! Mail once kept a mailbox per member of a workspace
//! ([`DocumentId::of_member`]), its messages in parts beside it, sent to that
//! member's devices alone. Every member's Mail now opens the workspace's one
//! mailbox, so what a member's own mailbox held is brought into it the first
//! time Mail opens the workspace on a device of theirs that keeps that
//! mailbox: no other device can read it.
//!
//! - **Its messages** join the workspace's parts through their own verb
//!   ([`MessageStates::take_in`]), each under its own id with what the
//!   member did with it, what its server holds and, for mail delivered
//!   without a server, the message itself. A message the workspace's mailbox
//!   knows already (the same mail fetched for someone else) stays as it is
//!   there.
//! - **Its folders, drafts, sends and checks** join the workspace's mailbox
//!   under their own ids, and one it holds already stays as it is. The draft
//!   the member's composer was writing becomes a draft of its own beside the
//!   workspace's composer draft, under an id derived from the member's
//!   mailbox, rather than written over what someone else is writing. What
//!   servers are still to be told of the drafts is worked out from them
//!   again, as after every change.
//! - **Once.** The mailbox's document id is kept in the workspace's
//!   ([`Mail::taken_in`]) in the same change that brings its contents in, and
//!   a mailbox kept there is never read again, so a draft discarded or a
//!   message sent since is not brought back. Two devices of the member that
//!   bring it in at once write the same items under the same ids, which read
//!   as one item each.
//!
//! The member's own mailbox is only read, never changed: it stays on their
//! devices as it was. A member who may not change the workspace's mailbox (a
//! Viewer) has theirs brought in once they may; until then Mail says so in
//! the log each time it opens, and their mail stays where it was.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::application::ProcessContext;
use ui::framework::Chain;
use ui::resource::DocumentId;
use ui::tilo_resource::{self, Fields as _, Id, JsonSchema, Model};

use crate::held::HeldMessage;
use crate::message_states::{MessageStates, message_states};
use crate::resource::{COMPOSER_DRAFT, Mail, MessageState};

/// The most messages one step of bringing them in carries.
const MESSAGES_PER_STEP: usize = 500;

/// A value of the mailbox's model carried as an argument in the form its
/// document keeps it.
#[derive(Clone, Debug, PartialEq)]
pub struct AsKept<T>(pub T);

impl<T: Model> Serialize for AsKept<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.to_json().serialize(serializer)
    }
}

impl<'de, T: Model> Deserialize<'de> for AsKept<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        T::from_json(&value)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

impl<T> JsonSchema for AsKept<T> {
    fn json_schema() -> serde_json::Value {
        serde_json::json!({})
    }
}

/// One message as a member's own mailbox kept it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct KeptMessage {
    /// Its id.
    pub id: String,
    /// What the member did with it, and what its server holds.
    pub state: AsKept<MessageState>,
    /// The message itself, for mail delivered without a server.
    #[serde(default)]
    pub held: Option<AsKept<HeldMessage>>,
}

impl tilo_resource::NamesItem for KeptMessage {
    fn named_item(&self) -> Option<Id> {
        crate::message_states::message_key(&self.id)
    }
}

/// Bring the mailbox the window's person kept as their own in this workspace
/// into the workspace's, once: see the module docs. A window on another
/// document, or of someone with no root identity, has none to bring in.
///
/// # Errors
/// The workspace's mailbox cannot be written.
pub(crate) fn take_in(context: &ProcessContext) -> Result<(), String> {
    if !context.record().id.is_package_primary() {
        return Ok(());
    }
    let Some(own) = context
        .author()
        .person_id()
        .and_then(|person| DocumentId::of_member(person, <Mail as tilo_resource::Resource>::KIND))
    else {
        return Ok(());
    };
    let store = context.records();
    let Some(kept) = store.stored_resource(&own) else {
        return Ok(());
    };
    let Ok(Some(mailbox)) = context.read_resource(crate::resource::read_mail) else {
        return Ok(());
    };
    if mailbox.taken_in.contains(&own.as_str().to_owned()) {
        return Ok(());
    }
    let writes = ui::permissions::PermissionScope::app(
        context.app_id(),
        ui::permissions::ActionKind::Undoable,
    );
    if !context.permissions().check(&writes) {
        tracing::warn!(
            mailbox = %own,
            "the person's own mailbox is not brought into the workspace's: they may not change it here"
        );
        return Ok(());
    }
    let Some(theirs) = crate::resource::read_mail(&kept) else {
        tracing::warn!(mailbox = %own, "the person's own mailbox does not read as a mailbox");
        return Ok(());
    };
    let mut messages = Vec::new();
    for index in store.held_parts(&own) {
        let Some(part) = store
            .stored_resource(&own.part(index))
            .and_then(|doc| <MessageStates as tilo_resource::Resource>::read(&doc).ok())
        else {
            tracing::warn!(mailbox = %own, part = index, "a part of the person's own mailbox does not read");
            return Ok(());
        };
        messages.extend(kept_messages(&part));
    }
    let author = context.author().clone();
    let base = Rc::new(mailbox);
    // The messages beside the workspace's mailbox, as its windows hold them.
    let chain = Chain::<MessageStates>::detached_beside_in(
        store.clone(),
        ui::apps::MAIL,
        Some(&DocumentId::package_primary()),
        &author,
        move || Rc::clone(&base),
    )
    .ok_or("the workspace's messages cannot be opened")?;
    if chain.head_arriving() || !bring_messages(&chain, messages)? {
        // The newest part is on its way from the workspace, or some messages
        // wait for a part that is: what waits is written once it is here, and
        // the rest is brought in the next time Mail opens.
        return Ok(());
    }
    context.migrate_resource(|doc| {
        let before =
            <Mail as tilo_resource::Resource>::read(doc).map_err(|error| error.to_string())?;
        let after = merged(&before, &theirs, &own);
        after
            .write_changed_fields(
                &before,
                doc,
                &ui::automerge::ROOT,
                &tilo_resource::Delta::default(),
            )
            .map_err(|error| error.to_string())?;
        doc.commit_with(
            ui::automerge::transaction::CommitOptions::default()
                .with_message(ui::tilo_actions::FrameworkChange::Native.change_message()),
        );
        Ok(true)
    })?;
    tracing::info!(mailbox = %own, "brought the person's own mailbox into the workspace's");
    Ok(())
}

/// Every message `part` keeps, as [`MessageStates::take_in`] carries it.
fn kept_messages(part: &MessageStates) -> Vec<KeptMessage> {
    part.messages
        .iter()
        .map(|(id, state)| KeptMessage {
            id: id.as_str().to_owned(),
            state: AsKept(state.clone()),
            held: part.held.get(id).cloned().map(AsKept),
        })
        .collect()
}

/// Bring `messages` the chain does not hold yet into it, a step at a time.
/// Whether all of them were written: `false` while some wait for a part on
/// its way.
fn bring_messages(
    chain: &Chain<MessageStates>,
    messages: Vec<KeptMessage>,
) -> Result<bool, String> {
    let held: std::collections::HashSet<Id> = chain
        .parts()
        .iter()
        .flat_map(|(_, part)| {
            part.messages
                .ids()
                .chain(part.held.ids())
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect();
    let new: Vec<KeptMessage> = messages
        .into_iter()
        .filter(|kept| {
            crate::message_states::message_key(&kept.id).is_some_and(|id| !held.contains(&id))
        })
        .collect();
    let mut written = true;
    for step in new.chunks(MESSAGES_PER_STEP) {
        let invoke = MessageStates::TAKE_IN.with(message_states::TakeIn {
            messages: step.to_vec(),
        });
        written &= chain
            .invoke(&invoke)
            .map_err(|error| error.to_string())?
            .is_some();
    }
    Ok(written)
}

/// The workspace's mailbox `ours` with what the member's own mailbox `theirs`
/// (document `own`) held brought in: see the module docs.
fn merged(ours: &Mail, theirs: &Mail, own: &DocumentId) -> Mail {
    let mut mail = ours.clone();
    for (id, folder) in theirs.folders.iter() {
        if !mail.folders.contains(id) {
            mail.folders.push(id.clone(), folder.clone());
        }
    }
    if theirs.has_draft() {
        let id = composer_draft_of(own);
        if !mail.drafts.contains(&id) {
            mail.drafts.push(id, theirs.draft.clone());
        }
    }
    for (id, draft) in theirs.drafts.iter() {
        if !mail.drafts.contains(id) && id.as_str() != COMPOSER_DRAFT {
            mail.drafts.push(id.clone(), draft.clone());
        }
    }
    for (id, outgoing) in theirs.outbox.pending() {
        if mail.outbox.get(id).is_none() {
            mail.outbox.push(id.clone(), outgoing.clone());
        }
    }
    for (id, check) in theirs.checks.pending() {
        if mail.checks.get(id).is_none() {
            mail.checks.push(id.clone(), check.clone());
        }
    }
    for (id, copy) in theirs.retired.iter() {
        if !mail.retired.contains(id) {
            mail.retired.push(id.clone(), copy.clone());
        }
    }
    mail.taken_in.insert(own.as_str().to_owned());
    mail
}

/// The id the draft a member's composer was writing in their own mailbox
/// `own` goes by among the workspace's drafts: the same on each of their
/// devices, and another for each member.
fn composer_draft_of(own: &DocumentId) -> Id {
    let digest = tilo_content::hash_bytes(format!("mail composer draft\0{own}").as_bytes());
    let mut bytes = [0_u8; 16];
    // The digest is hex, so its first 32 characters are 16 bytes.
    let _ = hex::decode_to_slice(digest.get(..32).unwrap_or_default(), &mut bytes);
    Id::from_raw(
        uuid::Builder::from_custom_bytes(bytes)
            .into_uuid()
            .to_string(),
    )
}

#[cfg(test)]
#[path = "own_mailbox/tests.rs"]
mod tests;
