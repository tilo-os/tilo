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
//! - answered for an action offered to them that takes one by an argument as for an action on
//!   nothing ([`Kept::given`]): the action does nothing with it and asks nobody about it, and
//!   one that takes the id of such a conversation is answered so whether or not anything was
//!   said in it yet;
//! - answered by a read with no row that names one ([`Kept::shown`]);
//! - told no change of one by the feed of the workspace's changes ([`Worker::told`]): not what
//!   was said, not where, and not that anything was.
//!
//! This narrows what the service gives out. It changes nothing of what a member's device holds.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use automerge::{ChangeHash, ObjId, ObjType, ReadDoc, ScalarValueRef, ValueRef};
use tilo_actions::ActionOutput;
use tilo_actions::remote::{Caller, CatalogueRow, ChangePage};
use ui::permissions::MemberId;
use ui::permissions::policy::PersonId;

use super::{DocumentRoute, Worker, WorkspaceState};

mod reading;

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
pub(super) type ReadOnce = HashMap<DocumentRoute, ([u8; 32], reading::Reading)>;

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
    let mut walk = Walk::default();
    if let Ok(doc) = automerge::AutoCommit::load(bytes) {
        walk.gather(&doc, None, (&automerge::ROOT, ObjType::Map, 0));
    }
    walk.found.into()
}

/// One walk through a document for what it keeps of direct conversations.
#[derive(Default)]
struct Walk {
    /// Every entry found: the key it is kept under, and the conversation.
    found: Vec<(String, String)>,
    /// How many objects were read for it.
    read: usize,
}

impl Walk {
    /// Put into what is found every entry at or below `object` that is of a direct
    /// conversation, in `doc` as it stands or as it stood at the heads `at`, and answer the
    /// direct conversation `object` itself names in one of its own strings, if it names one:
    /// what makes `object` an entry of that conversation, under the key it is kept by.
    fn gather(
        &mut self,
        doc: &impl ReadDoc,
        at: Option<&[ChangeHash]>,
        (object, kind, depth): (&ObjId, ObjType, usize),
    ) -> Option<String> {
        if depth > MOST_DEPTH {
            return None;
        }
        self.read = self.read.saturating_add(1);
        let mut named: Option<String> = None;
        let mut own_id: Option<String> = None;
        match kind {
            ObjType::Map | ObjType::Table => {
                let entries = match at {
                    Some(heads) => doc.map_range_at(object, .., heads),
                    None => doc.map_range(object, ..),
                };
                for item in entries {
                    let key: &str = &item.key;
                    let key = key.to_owned();
                    if MemberId::direct_conversation_members(&key).is_some() {
                        self.found.push((key.clone(), key.clone()));
                    }
                    match item.value {
                        ValueRef::Object(inner) => {
                            let id = item.id();
                            let below = (&id, inner, depth.saturating_add(1));
                            if let Some(conversation) = self.gather(doc, at, below) {
                                if key == ITEM_VALUE {
                                    // The item's own fields name it: its entry is of the conversation.
                                    if named.is_none() {
                                        named = Some(conversation);
                                    }
                                } else {
                                    self.found.push((key, conversation));
                                }
                            }
                        }
                        ValueRef::Scalar(ScalarValueRef::Str(text)) => {
                            let text: &str = &text;
                            if key == OWN_ID {
                                own_id = Some(text.to_owned());
                            }
                            if named.is_none()
                                && MemberId::direct_conversation_members(text).is_some()
                            {
                                named = Some(text.to_owned());
                            }
                        }
                        ValueRef::Scalar(_) => {}
                    }
                }
            }
            ObjType::List => {
                let items = match at {
                    Some(heads) => doc.list_range_at(object, .., heads),
                    None => doc.list_range(object, ..),
                };
                for item in items {
                    if let ValueRef::Object(inner) = item.value {
                        let id = item.id();
                        self.gather(doc, at, (&id, inner, depth.saturating_add(1)));
                    }
                }
            }
            ObjType::Text => {}
        }
        // An item kept in a list has no key: it is named by the id it keeps of itself.
        if let (Some(id), Some(conversation)) = (own_id, named.as_ref()) {
            self.found.push((id, conversation.clone()));
        }
        named
    }

    /// The direct conversation `object` names and the id it keeps of itself, read from its
    /// own entries alone, as [`Self::gather`] reads them: nothing below it but the item it
    /// keeps its fields in, which names it.
    fn names(
        &mut self,
        doc: &impl ReadDoc,
        at: Option<&[ChangeHash]>,
        (object, kind, depth): (&ObjId, ObjType, usize),
    ) -> (Option<String>, Option<String>) {
        let (mut named, mut own_id) = (None, None);
        if depth > MOST_DEPTH || !matches!(kind, ObjType::Map | ObjType::Table) {
            return (named, own_id);
        }
        self.read = self.read.saturating_add(1);
        let entries = match at {
            Some(heads) => doc.map_range_at(object, .., heads),
            None => doc.map_range(object, ..),
        };
        for item in entries {
            let key: &str = &item.key;
            let key = key.to_owned();
            match item.value {
                ValueRef::Object(inner) if key == ITEM_VALUE => {
                    let id = item.id();
                    let below = (&id, inner, depth.saturating_add(1));
                    let item_names = self.names(doc, at, below).0;
                    if named.is_none() {
                        named = item_names;
                    }
                }
                ValueRef::Scalar(ScalarValueRef::Str(text)) => {
                    let text: &str = &text;
                    if key == OWN_ID {
                        own_id = Some(text.to_owned());
                    }
                    if named.is_none() && MemberId::direct_conversation_members(text).is_some() {
                        named = Some(text.to_owned());
                    }
                }
                ValueRef::Object(_) | ValueRef::Scalar(_) => {}
            }
        }
        (named, own_id)
    }
}

/// What of one resource a caller's person is not given: what its documents keep of direct
/// conversations that person takes no part in.
#[derive(Default)]
pub(super) struct Kept {
    /// The keys of the entries kept from them, the conversations' own among them.
    entries: HashSet<String>,
    /// The conversations themselves, by their ids.
    conversations: HashSet<String>,
    /// The person it is kept from, where the resource is of the app that keeps conversations.
    from: Option<PersonId>,
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

    /// The `arguments` of an action its caller is offered, as it is carried out with them; or,
    /// where one of them is something not there for the person this is kept from
    /// ([`Self::withholds`]), the answer of an action on nothing. Asked only once the action is
    /// known to be offered to the caller, so that whoever is not offered it is answered as
    /// they were before, the same for what is kept and for what does not exist.
    #[allow(
        clippy::result_large_err,
        reason = "the outcome becomes the receipt the caller is answered with"
    )]
    pub(super) fn given(
        &self,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, super::Outcome> {
        if arguments.values().any(|value| self.argued(value)) {
            Err(super::unknown_action())
        } else {
            Ok(arguments)
        }
    }

    /// Whether an action's argument `value` is, or holds, something not there for them
    /// ([`Self::withholds`]). What a map in it is keyed by is not asked: an action that counts
    /// by conversation names every one of them so.
    fn argued(&self, value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(text) => self.withholds(text),
            serde_json::Value::Array(items) => items.iter().any(|item| self.argued(item)),
            serde_json::Value::Object(fields) => fields.values().any(|field| self.argued(field)),
            _ => false,
        }
    }

    /// Whether `text` is, whole, something not there for the person this is kept from: the key
    /// of a kept entry, or the id of a direct conversation they take no part in, whether or not
    /// anything is kept of it.
    fn withholds(&self, text: &str) -> bool {
        self.entries.contains(text)
            || self
                .from
                .as_ref()
                .is_some_and(|person| !said_to(person, text))
    }

    /// Whether `entry`, a change as the feed of a workspace's changes tells it, is of something
    /// kept: anywhere in it, in what it acted on, in its action's arguments or in what a map of
    /// them is keyed by, it names a kept entry or a direct conversation they take no part in.
    pub(super) fn tells_of(&self, entry: &serde_json::Value) -> bool {
        match entry {
            serde_json::Value::String(text) => self.withholds(text) || self.names(text),
            serde_json::Value::Array(items) => items.iter().any(|item| self.tells_of(item)),
            serde_json::Value::Object(fields) => fields
                .iter()
                .any(|(key, field)| self.withholds(key) || self.tells_of(field)),
            _ => false,
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
        kept.from = Some(person.clone());
        let documents = self.documents(None, state, &route.cluster_id);
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

    /// `page` of the feed of the changes of the workspace kept as `cluster_id`, as `caller` is
    /// told it: without the changes of what is kept from the person they are answered as. The
    /// page's cursor stands, so whoever follows the feed reads on behind what was left out.
    pub(super) fn told(
        &mut self,
        caller: &Caller,
        cluster_id: &str,
        state: &WorkspaceState,
        mut page: ChangePage,
    ) -> ChangePage {
        let chat = DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: cluster_id.to_owned(),
            document: String::new(),
        };
        let kept = self.kept_from_caller(caller, &chat, state);
        page.changes
            .retain(|change| change.app != CHAT || !kept.tells_of(&change.entry));
        page
    }

    /// What the document at `route` keeps of direct conversations, read again only once the
    /// document changed, and then by what it took since ([`reading::Reading::again`]). A
    /// document that does not read is listed to nobody, and keeps nothing.
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
        if let (Some(revision), Some((read_at, read))) = (revision, self.said_directly.get(route))
            && *read_at == revision
        {
            return read.said();
        }
        let before = self.said_directly.remove(route).map(|(_, read)| read);
        let read = reading::Reading::again(before, route, state);
        let said = read.said();
        if let Some(revision) = revision {
            self.said_directly.insert(route.clone(), (revision, read));
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

    pub(super) fn between(members: &[MemberId]) -> String {
        MemberId::direct_conversation(members.iter().cloned())
    }

    /// A document as an app keeps messages: each under its id, with the conversation it was
    /// said in; and a list of items that each keep their own id.
    pub(super) fn saved(messages: &[(&str, &str)], listed: &[(&str, &str)]) -> Vec<u8> {
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
        let mut kept = Kept {
            from: Some(reader.clone()),
            ..Kept::default()
        };
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

    #[test]
    fn a_change_of_a_direct_conversation_is_told_to_the_people_it_is_between_and_nobody_else() {
        use serde_json::json;
        let (ada, bo) = (person("ada"), person("bo"));
        let hers = between(&[MemberId::person("ada"), MemberId::agent("scout")]);
        let unsaid = between(&[MemberId::person("ada"), MemberId::agent("nobody")]);
        let bytes = saved(&[("m-hers", hers.as_str()), ("m-open", "general")], &[]);
        let (from_bo, from_ada) = (kept_of(&bytes, &bo), kept_of(&bytes, &ada));
        let mut counted = serde_json::Map::new();
        counted.insert(hers.clone(), json!(2));
        // By the conversation it was said in, also one nothing is kept of yet; by the message it
        // changed; by what a map of its arguments is keyed by.
        let said =
            json!({"what": {"args": {"said_in": hers, "text": "words"}}, "sentence": "Said"});
        let first = json!({"what": {"args": {"said_in": unsaid, "text": "words"}}});
        let changed = json!({"item": {"id": "kind/messages/m-hers"}, "sentence": "Changed"});
        let named = json!({"what": {"args": {"message": "m-hers", "text": "other words"}}});
        let carried = json!({"what": {"args": {"earlier": {"0": counted}}}});
        for kept in [&said, &first, &changed, &named, &carried] {
            assert!(from_bo.tells_of(kept), "{kept} is not told to Bo");
            assert!(!from_ada.tells_of(kept), "{kept} is told to Ada");
        }
        let open = json!({
            "item": {"id": "kind/messages/m-open"},
            "what": {"args": {"text": "m-hers was said"}},
        });
        assert!(
            !from_bo.tells_of(&open),
            "the open chat's change is told to Bo"
        );
    }

    #[test]
    fn an_action_is_given_no_argument_that_names_what_is_kept() {
        use serde_json::json;
        let (ada, bo) = (person("ada"), person("bo"));
        let hers = between(&[MemberId::person("ada"), MemberId::agent("scout")]);
        let unsaid = between(&[MemberId::person("ada"), MemberId::agent("nobody")]);
        let bytes = saved(&[("m-hers", hers.as_str()), ("m-open", "general")], &[]);
        let (from_bo, from_ada) = (kept_of(&bytes, &bo), kept_of(&bytes, &ada));
        let taking = |value: serde_json::Value| value.as_object().unwrap().clone();
        // Her message, her conversation, one of hers nothing was said in, and one inside a list.
        for kept in [
            json!({"message": "m-hers", "pinned": true}),
            json!({"said_in": hers, "text": "words"}),
            json!({"said_in": unsaid, "text": "words"}),
            json!({"about": [{"message": "m-hers"}]}),
        ] {
            let kept = taking(kept);
            assert!(from_bo.given(kept.clone()).is_err(), "{kept:?} for Bo");
            assert_eq!(from_ada.given(kept.clone()).ok(), Some(kept), "for Ada");
        }
        // Words that only hold a key, the open chat's message, and what a map is keyed by.
        let mut counted = serde_json::Map::new();
        counted.insert(hers, json!(2));
        for given in [
            json!({"message": "m-open", "text": "about m-hers and more"}),
            json!({"earlier": {"0": counted}}),
        ] {
            let given = taking(given);
            assert_eq!(from_bo.given(given.clone()).ok(), Some(given), "for Bo");
        }
    }
}
