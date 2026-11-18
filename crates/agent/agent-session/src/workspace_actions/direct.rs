//! What is said in a direct conversation is given to the people it is between, and to nobody
//! else: the one rule this service answers a search and a read of Chat by, and the rule Chat's
//! own window lists a person's conversations by.
//!
//! A direct conversation is named by the people and agents it is between
//! ([`MemberId::direct_conversation`]), and Chat keeps what is said in it in documents every
//! member's device holds. Its window lists such a conversation only for someone it is between.
//! This service gives a caller of Chat what that window gives the person the caller acts for,
//! an agent included: never what the agent itself takes part in for somebody else, and whatever
//! role its person has in the workspace ([`said_to`]).
//!
//! What a document keeps of direct conversations is read from the document itself, with none of
//! its app's code, once per change of it ([`said_directly`]): every entry a map keeps under the
//! id of such a conversation, and every entry that itself holds the id of one (a message, by
//! the conversation it was said in), the way the toolkit reads whom an item is addressed to
//! ([`ui::mentions`]). A caller whose person takes no part in the conversation is then
//!
//! - listed no row of such an entry, nor of anything under it ([`Kept::keeps`]), with the pages
//!   counted in the rows they are given;
//! - answered for an action on one exactly as for one that does not exist;
//! - answered by a read with no row that names one ([`Kept::shown`]).
//!
//! This narrows what the service gives out. It changes nothing of what a member's device holds.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use automerge::{ObjId, ObjType, ReadDoc, ScalarValueRef, ValueRef};
use tilo_actions::ActionOutput;
use tilo_actions::remote::{Caller, CatalogueRow};
use ui::permissions::MemberId;
use ui::permissions::policy::PersonId;

use super::{DocumentRoute, Worker, WorkspaceState};

/// The app whose documents keep the workspace's conversations.
const CHAT: &str = "so.tilo.chat";

/// How deep into a document its entries are read: far below where an app keeps an item.
const MOST_DEPTH: usize = 32;

/// The fields a read's row names its item by: a listing's `item` address, or
/// the `id` a collection's entry keeps it under (a chain's typed read).
const ROW_NAMES: [&str; 2] = ["item", "id"];

/// The key a stored item keeps its own id under, where it keeps one.
const OWN_ID: &str = "id";

/// What each document keeps of direct conversations, and the revision it was read at.
pub(super) type ReadOnce = HashMap<DocumentRoute, ([u8; 32], Arc<[(String, String)]>)>;

/// The key a stored collection keeps an item's own fields under, in the entry it keeps the item
/// by: the entry stands under the item's id and holds the item's place in the order beside it
/// (`ui::tilo_resource`'s collection). The tests of this service that read Chat through it fail
/// if that layout changes.
const ITEM_VALUE: &str = "v";

/// Whether `person` is given what is said in the conversation `id`. A direct conversation is
/// given to the people it is between and to nobody else, whatever they may read of the app that
/// keeps it; an agent is given what the person it acts for takes part in, never what it takes
/// part in itself for somebody else. A conversation of any other kind is not decided here.
pub(super) fn said_to(person: &PersonId, id: &str) -> bool {
    MemberId::direct_conversation_members(id)
        .is_none_or(|members| members.contains(&MemberId::Person(person.clone())))
}

/// Every entry the document saved as `bytes` keeps of a direct conversation: the key it is kept
/// under, and the conversation. Nothing for bytes that are no document.
pub(crate) fn said_directly(bytes: &[u8]) -> Arc<[(String, String)]> {
    let mut found = Vec::new();
    if let Ok(doc) = automerge::AutoCommit::load(bytes) {
        gather(&doc, &automerge::ROOT, ObjType::Map, 0, &mut found);
    }
    found.into()
}

/// Put into `found` every entry at or below `object` that is of a direct conversation, and
/// answer the direct conversation `object` itself names in one of its own strings, if it names
/// one: what makes `object` an entry of that conversation, under the key it is kept by.
fn gather(
    doc: &impl ReadDoc,
    object: &ObjId,
    kind: ObjType,
    depth: usize,
    found: &mut Vec<(String, String)>,
) -> Option<String> {
    if depth > MOST_DEPTH {
        return None;
    }
    let mut named: Option<String> = None;
    let mut own_id: Option<String> = None;
    match kind {
        ObjType::Map | ObjType::Table => {
            for item in doc.map_range(object, ..) {
                let key: &str = &item.key;
                let key = key.to_owned();
                if MemberId::direct_conversation_members(&key).is_some() {
                    found.push((key.clone(), key.clone()));
                }
                match item.value {
                    ValueRef::Object(inner) => {
                        let below = depth.saturating_add(1);
                        if let Some(conversation) = gather(doc, &item.id(), inner, below, found) {
                            if key == ITEM_VALUE {
                                // The item's own fields name it: its entry is of the conversation.
                                if named.is_none() {
                                    named = Some(conversation);
                                }
                            } else {
                                found.push((key, conversation));
                            }
                        }
                    }
                    ValueRef::Scalar(ScalarValueRef::Str(text)) => {
                        let text: &str = &text;
                        if key == OWN_ID {
                            own_id = Some(text.to_owned());
                        }
                        if named.is_none() && MemberId::direct_conversation_members(text).is_some()
                        {
                            named = Some(text.to_owned());
                        }
                    }
                    ValueRef::Scalar(_) => {}
                }
            }
        }
        ObjType::List => {
            for item in doc.list_range(object, ..) {
                if let ValueRef::Object(inner) = item.value {
                    gather(doc, &item.id(), inner, depth.saturating_add(1), found);
                }
            }
        }
        ObjType::Text => {}
    }
    // An item kept in a list has no key: it is named by the id it keeps of itself.
    if let (Some(id), Some(conversation)) = (own_id, named.as_ref()) {
        found.push((id, conversation.clone()));
    }
    named
}

/// What of one resource a caller's person is not given: what its documents keep of direct
/// conversations that person takes no part in.
#[derive(Default)]
pub(super) struct Kept {
    /// The keys of the entries kept from them, the conversations' own among them.
    entries: HashSet<String>,
    /// The conversations themselves, by their ids.
    conversations: HashSet<String>,
}

impl Kept {
    /// Whether nothing is kept from them.
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `text` names something kept: one of the entries by its key, as a resource's id
    /// names it between two slashes, or one of the conversations anywhere in it.
    pub(super) fn names(&self, text: &str) -> bool {
        !self.is_empty()
            && (text.split('/').any(|part| self.entries.contains(part))
                || self
                    .conversations
                    .iter()
                    .any(|conversation| text.contains(conversation.as_str())))
    }

    /// The answer for an action on `resource` where it names something kept: that of an action
    /// on nothing, so that nothing tells a kept entry from one that does not exist.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the caller is answered with"
    )]
    pub(super) fn refuses(&self, resource: &str) -> Result<(), super::Outcome> {
        if self.names(resource) {
            Err(super::unknown_action())
        } else {
            Ok(())
        }
    }

    /// Whether `row` of a listing is of something kept, or of something under it.
    pub(super) fn keeps(&self, row: &CatalogueRow) -> bool {
        self.names(&row.resource.id)
            || self.names(&row.parent_resource)
            || row
                .action
                .as_ref()
                .is_some_and(|action| self.names(&action.address.resource))
    }

    /// `output` without what names something kept: the rows of a read whose item is kept, and
    /// the resources it says it produced or revealed.
    pub(super) fn shown(&self, mut output: ActionOutput) -> ActionOutput {
        if self.is_empty() {
            return output;
        }
        if let Some(value) = output.value.as_mut() {
            self.leave_out(value);
        }
        output.resources.retain(|resource| !self.names(resource));
        output
    }

    /// Take out of every list in `value` the rows that name something kept.
    fn leave_out(&self, value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Array(rows) => {
                rows.retain(|row| !self.names_in(row));
                for row in rows {
                    self.leave_out(row);
                }
            }
            serde_json::Value::Object(fields) => {
                for (_, field) in fields.iter_mut() {
                    self.leave_out(field);
                }
            }
            _ => {}
        }
    }

    /// Whether `row` of a read is of something kept: the address of a kept item on its own, or
    /// a row that names its item by one.
    fn names_in(&self, row: &serde_json::Value) -> bool {
        match row {
            serde_json::Value::String(address) => self.names(address),
            serde_json::Value::Object(fields) => ROW_NAMES.iter().any(|key| {
                fields
                    .get(*key)
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|named| self.names(named))
            }),
            _ => false,
        }
    }
}

impl Worker {
    /// The person `caller` is answered as where a direct conversation decides: themselves, the
    /// person an agent acts for, and this device's own person for a caller in nobody's name.
    pub(super) fn asking(&self, caller: &Caller) -> PersonId {
        self.bounded(caller)
            .map_or_else(|| self.member.clone(), |who| who.person)
    }

    /// [`Self::kept_from`] for the person `caller` is answered as ([`Self::asking`]).
    pub(super) fn kept_from_caller(
        &mut self,
        caller: &Caller,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Kept {
        let asking = self.asking(caller);
        self.kept_from(&asking, route, state)
    }

    /// What `person` is not given of the resource at `route`, as its documents are now: its own
    /// and those of the parts kept beside it. Nothing for an app that keeps no conversations.
    pub(super) fn kept_from(
        &mut self,
        person: &PersonId,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Kept {
        let mut kept = Kept::default();
        if route.app_id != CHAT {
            return kept;
        }
        let documents = self.documents(state, &route.cluster_id);
        for document in documents.iter().filter(|held| held.app_id == route.app_id) {
            for (key, conversation) in self.directly_kept_in(document, state).iter() {
                if !said_to(person, conversation) {
                    kept.entries.insert(key.clone());
                    kept.entries.insert(conversation.clone());
                    kept.conversations.insert(conversation.clone());
                }
            }
        }
        kept
    }

    /// What the document at `route` keeps of direct conversations, read again only once the
    /// document changed. A document that does not read is listed to nobody, and keeps nothing.
    fn directly_kept_in(
        &mut self,
        route: &DocumentRoute,
        state: &WorkspaceState,
    ) -> Arc<[(String, String)]> {
        let revision = state
            .engine
            .document_revision(&route.cluster_id, &route.app_id, &route.document)
            .ok()
            .flatten();
        if let (Some(revision), Some((read_at, said))) = (revision, self.said_directly.get(route))
            && *read_at == revision
        {
            return Arc::clone(said);
        }
        let said = state
            .document(route)
            .map_or_else(|_| said_directly(&[]), |(bytes, _)| said_directly(&bytes));
        if let Some(revision) = revision {
            self.said_directly
                .insert(route.clone(), (revision, Arc::clone(&said)));
        }
        said
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(name: &str) -> PersonId {
        name.into()
    }

    fn between(members: &[MemberId]) -> String {
        MemberId::direct_conversation(members.iter().cloned())
    }

    /// A document as an app keeps messages: each under its id, with the conversation it was
    /// said in; and a list of items that each keep their own id.
    fn saved(messages: &[(&str, &str)], listed: &[(&str, &str)]) -> Vec<u8> {
        use automerge::transaction::Transactable as _;
        let mut doc = automerge::AutoCommit::new();
        let map = doc
            .put_object(automerge::ROOT, "messages", ObjType::Map)
            .unwrap();
        let plain = doc
            .put_object(automerge::ROOT, "pinned", ObjType::Map)
            .unwrap();
        for (id, conversation) in messages {
            // As a stored collection keeps an item: an entry under its id, the item below it.
            let entry = doc.put_object(&map, *id, ObjType::Map).unwrap();
            doc.put(&entry, "k", "a0").unwrap();
            let message = doc.put_object(&entry, ITEM_VALUE, ObjType::Map).unwrap();
            doc.put(&message, "said_in", *conversation).unwrap();
            doc.put(&message, "text", "words").unwrap();
            // And as a plain map keeps one: under its id.
            let pinned = doc
                .put_object(&plain, format!("p-{id}"), ObjType::Map)
                .unwrap();
            doc.put(&pinned, "said_in", *conversation).unwrap();
        }
        let list = doc
            .put_object(automerge::ROOT, "listed", ObjType::List)
            .unwrap();
        for (at, (id, conversation)) in listed.iter().enumerate() {
            let item = doc.insert_object(&list, at, ObjType::Map).unwrap();
            doc.put(&item, OWN_ID, *id).unwrap();
            doc.put(&item, "said_in", *conversation).unwrap();
        }
        doc.save()
    }

    fn kept_of(bytes: &[u8], reader: &PersonId) -> Kept {
        let mut kept = Kept::default();
        for (key, conversation) in said_directly(bytes).iter() {
            if !said_to(reader, conversation) {
                kept.entries.insert(key.clone());
                kept.entries.insert(conversation.clone());
                kept.conversations.insert(conversation.clone());
            }
        }
        kept
    }

    #[test]
    fn a_direct_conversation_is_given_to_the_people_it_is_between_and_nobody_else() {
        let (ada, bo) = (person("ada"), person("bo"));
        let hers = between(&[MemberId::person("ada"), MemberId::agent("scout")]);
        assert!(said_to(&ada, &hers));
        assert!(!said_to(&bo, &hers), "another person");
        // The agent is in it, for Ada: whoever else it acts for is not.
        let his = between(&[MemberId::person("bo"), MemberId::agent("scout")]);
        assert!(!said_to(&ada, &his), "her agent's conversation with him");
        assert!(said_to(&bo, "general"), "a conversation of everyone");
    }

    #[test]
    fn what_a_document_keeps_of_a_direct_conversation_is_kept_from_whoever_is_not_in_it() {
        let (ada, bo) = (person("ada"), person("bo"));
        let hers = between(&[MemberId::person("ada"), MemberId::agent("scout")]);
        let bytes = saved(
            &[("m-hers", hers.as_str()), ("m-open", "general")],
            &[("l-hers", hers.as_str()), ("l-open", "general")],
        );
        let from_bo = kept_of(&bytes, &bo);
        for kept in [
            "kind/messages/m-hers",
            "kind/messages/m-hers/body",
            "kind/pinned/p-m-hers",
            "l-hers",
        ] {
            assert!(from_bo.names(kept), "{kept} is kept from Bo");
        }
        for given in [
            "kind/messages/m-open",
            "kind/pinned/p-m-open",
            "kind/messages/v",
            "l-open",
            "kind/messages",
            "kind",
        ] {
            assert!(!from_bo.names(given), "{given} is given to Bo");
        }
        assert!(from_bo.names(&format!("kind/conversations/{hers}")));
        assert!(kept_of(&bytes, &ada).is_empty(), "nothing is kept from Ada");

        // A read's rows: the kept one is gone, the page's other fields stay.
        let output = ActionOutput {
            value: Some(serde_json::json!({
                "messages": [
                    {"id": "m-hers", "value": {"text": "words"}},
                    {"id": "m-open", "value": {"text": "m-hers"}},
                ],
                "nextCursor": "0:2",
            })),
            resources: vec!["kind/messages/m-hers".into(), "kind/messages/m-open".into()],
        };
        let shown = from_bo.shown(output);
        assert_eq!(
            shown.value,
            Some(serde_json::json!({
                "messages": [{"id": "m-open", "value": {"text": "m-hers"}}],
                "nextCursor": "0:2",
            }))
        );
        assert_eq!(shown.resources, vec!["kind/messages/m-open".to_owned()]);
    }
}
