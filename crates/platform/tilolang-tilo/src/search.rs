//! What search finds of a workspace: the text of every resource and item of every package.
//!
//! Every `Text` and `RichText` field of a type is searchable by default (a rich text by its text
//! as the person searching reads it: a reference to a resource they hold by what it is called
//! now, one they do not hold as not shared, never by the title it was written with,
//! [`crate::reading`]); a hit is the resource or item that holds it, found by its text, and it
//! opens the host at that resource (its `open` view action, see [`crate::effects::OpenEffect`]).
//! A package will be able to say which fields to index; until then all of them are, the most
//! useful default for a notes app and a chat alike.
//!
//! A type says what its resources are to search and who they are with (`search { kind
//! conversation; people person }`): an entry of one carries that kind, those people with who made
//! it (its first version's author) first, and when it was made, so the shell draws a conversation
//! or a message as one and a box sends to a conversation and the people in it.
//!
//! Search is the system's: a window publishes every hit to the session's search index, which
//! finds them as every Tilo surface finds things, and keeps them after the app closes. The hits
//! are read from the device's resource store ([`crate::resources`]): every resource of a type
//! that holds text, nothing kept beside it. Each text is as the window's person reads it, and a
//! resource the records holding it keep from them (`type Direct (readers: people)`) is no hit.

use std::collections::BTreeSet;
use std::sync::Arc;

use tilo_resource::search_index::{
    IndexAction, IndexDelta, IndexEntry, IndexKind, IndexPerson, IndexState, IndexWeight, Publisher,
};
use tilolang_runtime::meta::{FieldKind, Meta};
use tilolang_runtime::store::{FieldSpec, FieldValue, Store as _};
use tilolang_runtime::value::ResourceRef;

use crate::host::Host;
use crate::publish::resource_id;
use crate::resources::ResourceStore;

/// One searchable resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The id the resource is published under (`tl:<app>:<id>`): what its `open` action is
    /// addressed to.
    pub resource: String,
    /// The app the resource belongs to, as people call it.
    pub app_title: String,
    /// The type of the resource.
    pub type_name: String,
    /// The first of its texts: what the hit reads as.
    pub title: String,
    /// Every text it holds, in field order.
    pub texts: Vec<String>,
    /// The resources its rich text refers to, in the one form (`tilo_reference::embed`): the
    /// system's index finds it by what they are called now, and keeps its backlinks.
    pub references: Vec<String>,
    /// What its type says it is to search, who it is with and when it was made ([`Facts`]).
    pub facts: Facts,
}

/// What a resource's type says it is to search (`kind`), who it is with (`people`: who made it,
/// then the people its type's `people` fields name, each once) and when it was made, in seconds
/// since the Unix epoch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub kind: Option<IndexKind>,
    pub people: Vec<IndexPerson>,
    pub when: Option<i64>,
}

impl Facts {
    /// The facts of the resource `at`, read from `store` now: none for a type that declares no
    /// kind of thing to search.
    fn of(store: &ResourceStore, at: &ResourceRef) -> Self {
        let Some(schema) = crate::schema::lookup(&at.ty) else {
            return Self::default();
        };
        let Some(kind) = schema.search_kind.map(index_kind) else {
            return Self::default();
        };
        // Its making is its first version, which names who made it and when.
        let made = store.version_end(at, false).ok().flatten();
        let mut members: Vec<tilo_actions::MemberId> = Vec::new();
        let mut add = |member: &tilo_actions::MemberId| {
            if !members.contains(member) {
                members.push(member.clone());
            }
        };
        if let Some(member) = made
            .as_ref()
            .and_then(|made| made.who.as_ref())
            .and_then(|who| who.member_id())
        {
            add(member);
        }
        let kinds = store.kinds();
        let people: Vec<FieldSpec> = schema
            .people
            .iter()
            .filter_map(|field| {
                kinds
                    .get(&at.ty)
                    .and_then(|entry| entry.fields.get(field))
                    .map(|entry| FieldSpec {
                        id: field.clone(),
                        kind: entry.kind,
                    })
            })
            .collect();
        drop(kinds);
        for spec in &people {
            let value = store.read(at, spec).ok();
            for member in members_in(value.as_ref()) {
                add(&member);
            }
        }
        Self {
            kind: Some(kind),
            people: members.iter().filter_map(person_of).collect(),
            when: made.and_then(|made| tilolang::calendar::instant(&made.at)),
        }
    }
}

/// A type's declared kind of thing to search, as the system's index names it.
fn index_kind(kind: tilolang::meta::SearchKind) -> IndexKind {
    match kind {
        tilolang::meta::SearchKind::Conversation => IndexKind::Conversation,
        tilolang::meta::SearchKind::Message => IndexKind::Message,
        tilolang::meta::SearchKind::Person => IndexKind::Person,
        tilolang::meta::SearchKind::Agent => IndexKind::Agent,
    }
}

/// The members a stored `Actor` field (or an option or a set of them) names.
fn members_in(value: Option<&tilolang_runtime::store::FieldValue>) -> Vec<tilo_actions::MemberId> {
    use tilolang_runtime::store::{FieldValue, Scalar};
    use tilolang_runtime::value::Value;
    // A field names its person, maybe one, or a list of them (the people a conversation is with).
    let of_json = |json: &serde_json::Value| -> Vec<tilo_actions::MemberId> {
        let actor = |value: &Value| match value {
            Value::Actor(actor) => actor.member_id().cloned(),
            _ => None,
        };
        match tilo_failure::ignored!(
            "a stored value that is no encoded value names nobody",
            tilolang_runtime::value::from_json(json)
        ) {
            Ok(Value::Opt(Some(inner))) => actor(&inner).into_iter().collect(),
            Ok(Value::List(items)) => items.iter().filter_map(actor).collect(),
            Ok(value) => actor(&value).into_iter().collect(),
            Err(_) => Vec::new(),
        }
    };
    let of_text = |text: &str| {
        tilo_failure::ignored!(
            "a stored text that is no encoded actor names nobody",
            serde_json::from_str(text).ok()
        )
        .as_ref()
        .map(of_json)
        .unwrap_or_default()
    };
    match value {
        Some(FieldValue::Scalar(Scalar::Json(json))) => of_json(json),
        Some(FieldValue::Scalar(Scalar::Text(text))) => of_text(text),
        Some(FieldValue::Keys(keys)) => keys.iter().flat_map(|key| of_text(key)).collect(),
        _ => Vec::new(),
    }
}

/// `member` as an entry names them: a person by their root key, an agent as `agent:<id>`, named by
/// what they are called now.
fn person_of(member: &tilo_actions::MemberId) -> Option<IndexPerson> {
    if let Some(agent) = member.agent_id() {
        return Some(IndexPerson {
            name: String::new(),
            anchor: String::new(),
            identity: format!("agent:{agent}"),
            agent: true,
        });
    }
    Some(IndexPerson {
        name: String::new(),
        anchor: String::new(),
        identity: member.person_id()?.to_owned(),
        agent: false,
    })
}

/// What the person at the host holds, as their rich text is read for them: the resources the
/// store holds, of the packages the host runs.
struct Holdings<'a> {
    store: &'a ResourceStore,
    apps: BTreeSet<String>,
    unshared: Arc<str>,
}

impl<'a> Holdings<'a> {
    fn of(host: &'a Host) -> Self {
        Self {
            store: host.store(),
            apps: host
                .packages()
                .iter()
                .map(|package| package.meta.app.clone())
                .collect(),
            unshared: Arc::from(host.runtime().unshared()),
        }
    }
}

impl crate::reading::Holdings for Holdings<'_> {
    fn held(&self, at: &ResourceRef) -> Option<String> {
        self.store.holds(at)
    }

    fn first_value(&self, at: &ResourceRef) -> Option<FieldValue> {
        self.store.title_value(at)
    }

    fn keeps(&self, app: &str) -> bool {
        self.apps.contains(app)
    }

    fn unshared(&self) -> Arc<str> {
        Arc::clone(&self.unshared)
    }
}

/// The fields of the type `ty` that hold text, the one it is titled by first and the rest in the
/// order `meta` declares them: what a hit of one of its resources reads. Empty for a type `meta`
/// does not declare.
fn texted(store: &ResourceStore, meta: &Meta, ty: &str) -> Vec<FieldSpec> {
    let kinds = store.kinds();
    let (Some(entry), Some(declared)) = (
        kinds.get(ty),
        meta.types.iter().find(|declared| declared.id == ty),
    ) else {
        return Vec::new();
    };
    let mut fields: Vec<FieldSpec> = declared
        .fields
        .iter()
        .filter_map(|field| {
            let kind = entry.fields.get(&field.id)?.kind;
            matches!(kind, FieldKind::Text | FieldKind::RichText).then(|| FieldSpec {
                id: field.id.clone(),
                kind,
            })
        })
        .collect();
    // The field the type says names its resources (`(title: subject)`) comes first: what search
    // reads as a resource's name, with the words under it after.
    if let Some(title) = entry.texts.first()
        && let Some(at) = fields.iter().position(|field| &field.id == title)
        && at > 0
    {
        let named = fields.remove(at);
        fields.insert(0, named);
    }
    fields
}

/// The hit of the resource `at` of the package `meta`, read from the store now: none when it
/// holds no text, or is gone.
fn hit_at(
    holdings: &Holdings<'_>,
    named: (&Meta, &str),
    at: &ResourceRef,
    fields: &[FieldSpec],
) -> Option<Hit> {
    let values = fields
        .iter()
        .filter_map(|field| holdings.store.read(at, field).ok());
    hit_of(holdings, named, at, values)
}

/// The hit of the resource `at` of the package `meta` that holds the texts `values`: none when
/// it holds no text.
fn hit_of(
    holdings: &Holdings<'_>,
    (meta, app_title): (&Meta, &str),
    at: &ResourceRef,
    values: impl IntoIterator<Item = FieldValue>,
) -> Option<Hit> {
    let type_name = meta
        .types
        .iter()
        .find(|declared| declared.id == at.ty)?
        .name
        .clone();
    let id = at.id_string();
    let mut texts = Vec::new();
    let mut references: Vec<String> = Vec::new();
    for value in values {
        let value = match value {
            value @ FieldValue::Text(_) => value,
            FieldValue::Rich(rich) => {
                for reference in rich.references() {
                    let mark = reference.mark_value();
                    if reference.as_resource().is_some() && !references.contains(&mark) {
                        references.push(mark);
                    }
                }
                FieldValue::Rich(rich)
            }
            _ => continue,
        };
        let text = crate::reading::said(holdings, &id, value).unwrap_or_default();
        if !text.trim().is_empty() {
            texts.push(text);
        }
    }
    Some(Hit {
        resource: resource_id(&meta.app, &id),
        app_title: app_title.to_owned(),
        type_name,
        title: texts.first()?.clone(),
        texts,
        references,
        facts: Facts::of(holdings.store, at),
    })
}

/// The searchable resources of the document `doc` of the package `meta` and the items below
/// it, read from the store now, each text as the person at the host reads it.
#[must_use]
pub fn hits(host: &Host, meta: &Meta, doc: &str) -> Vec<Hit> {
    let holdings = Holdings::of(host);
    let store = host.store();
    let reads = host.reads_for(host.viewer());
    let title = crate::publish::app_name(&*host.runtime().words(), meta);
    let Some(ty) = store.type_of_id(doc) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut pending = vec![ResourceRef::root(doc, ty)];
    while let Some(at) = pending.pop() {
        let fields = texted(store, meta, &at.ty);
        out.extend(hit_at(&holdings, (meta, &title), &at, &fields).filter(|_| reads(&at)));
        // Its items, in their collections' order, read next.
        let mut items = Vec::new();
        for field in meta
            .types
            .iter()
            .find(|declared| declared.id == at.ty)
            .into_iter()
            .flat_map(|declared| &declared.fields)
        {
            let collection = {
                let kinds = store.kinds();
                kinds
                    .get(&at.ty)
                    .and_then(|entry| entry.fields.get(&field.id))
                    .filter(|entry| entry.kind == FieldKind::Collection)
                    .map(|entry| entry.items.clone())
            };
            let Some(item_types) = collection else {
                continue;
            };
            let spec = FieldSpec {
                id: field.id.clone(),
                kind: FieldKind::Collection,
            };
            let Ok(listing) = store.listing(&at, &spec) else {
                continue;
            };
            items.extend(
                listing
                    .iter()
                    .filter_map(|id| crate::publish::item_of(store, &at, &id, &item_types)),
            );
        }
        pending.extend(items.into_iter().rev());
    }
    out
}

/// Every hit of the workspace, read from the store now: what a window publishes to the session's
/// search index, which is what search finds things through. One read of every resource of each
/// type that holds text. Each text is as the window's person reads it: the index matches a query
/// against what they read, never against the title of a resource they do not hold. A resource
/// the records holding it keep from them (`type Direct (readers: people)`) is no hit.
#[must_use]
pub fn every_hit(host: &Host) -> Vec<Hit> {
    let holdings = Holdings::of(host);
    let store = host.store();
    let reads = host.reads_for(host.viewer());
    let words = host.runtime().words();
    let mut out = Vec::new();
    for package in host.packages() {
        let meta = &package.meta;
        let title = crate::publish::app_name(&*words, meta);
        for declared in &meta.types {
            let fields = texted(store, meta, &declared.id);
            if fields.is_empty() {
                continue;
            }
            let read = match store.texts_of(&declared.id, &fields) {
                Ok(read) => read,
                Err(error) => {
                    tracing::debug!(%error, ty = %declared.id, "a type's texts are not read");
                    continue;
                }
            };
            for (at, values) in read {
                if !reads(&at) {
                    continue;
                }
                out.extend(hit_of(&holdings, (meta, &title), &at, values));
            }
        }
    }
    out
}

/// The action that opens a resource at its anchor (`open(r)`), as every published resource
/// offers it bound to itself.
const OPEN_ACTION: &str = "open";

/// Every entry a host's packages publish to the search index, read from what it holds now, as
/// its viewer reads them: one per resource with text, each named by the resource's published id.
/// A record the host opened for one person answers with what that person reads, a direct
/// conversation's messages among them only for the people it is between.
#[must_use]
pub fn entries(host: &Host) -> Vec<IndexEntry> {
    every_hit(host).iter().filter_map(entry).collect()
}

/// The entry the session's persistent search index keeps of the resource `id` (`doc` or
/// `doc/item`) of the package `meta`, read now: none when it holds no text, when it is gone, or
/// when the records holding it keep it from the host's viewer. What an instance publishes for the
/// resources a change reached, instead of reading the workspace again.
#[must_use]
pub fn index_entry_at(host: &Host, meta: &Meta, id: &str) -> Option<IndexEntry> {
    let store = host.store();
    let id = tilolang_runtime::value::flat_id(id);
    let ty = store.type_of_id(&id)?;
    let fields = texted(store, meta, &ty);
    if fields.is_empty() {
        return None;
    }
    let at = ResourceRef::parse_id(&id, ty)?;
    if !host.reads_for(host.viewer())(&at) {
        return None;
    }
    let title = crate::publish::app_name(&*host.runtime().words(), meta);
    let hit = hit_at(&Holdings::of(host), (meta, &title), &at, &fields)?;
    entry(&hit)
}

/// The index's entry of `hit`: short metadata first (a sender, a subject), then long prose (a
/// body), so the index keeps a bounded number of fields ([`IndexEntry::bounded`]) with the
/// metadata that ranks highest. A hit opens through the `open` of the document that holds the
/// resource, which every window showing anything of the document publishes: the caller names
/// the resource as the item to show, which no window may show yet.
fn entry(hit: &Hit) -> Option<IndexEntry> {
    let mut fields: Vec<(String, IndexWeight)> = hit
        .texts
        .iter()
        .skip(1)
        .filter(|text| text.chars().count() <= SHORT_TEXT)
        .map(|text| (text.clone(), IndexWeight::Metadata))
        .collect();
    fields.extend(
        hit.texts
            .iter()
            .skip(1)
            .filter(|text| text.chars().count() > SHORT_TEXT)
            .map(|text| (text.clone(), IndexWeight::Body)),
    );
    let entry = IndexEntry {
        key: hit.resource.clone(),
        resource: hit.resource.clone(),
        action: Some(IndexAction {
            resource: document_of(&hit.resource).to_owned(),
            action: OPEN_ACTION.to_owned(),
        }),
        title: hit.title.clone(),
        subtitle: Some(hit.type_name.clone()),
        group: Some(hit.app_title.clone()),
        icon: None,
        boost: 0,
        fields,
        secret: false,
        references: hit.references.clone(),
        kind: hit.facts.kind,
        people: hit.facts.people.clone(),
        when: hit.facts.when,
        ..IndexEntry::default()
    };
    entry.bounded()
}

/// The published record that holds the published resource `resource`: an item's id goes on
/// from its document's id after a `/`.
fn document_of(resource: &str) -> &str {
    resource
        .split_once('/')
        .map_or(resource, |(record, _)| record)
}

/// The longest text an entry keeps as a short field: a name, a subject, a sender.
const SHORT_TEXT: usize = 80;

/// What [`publish_whole`] made of the whole index.
pub struct Published {
    /// The publisher, holding the whole set as what it has sent.
    pub publisher: Publisher,
    /// One entry per resource with text, by the resource's published id.
    pub entries: std::collections::BTreeMap<String, IndexEntry>,
    /// The deltas that publish them, each as it crosses the broker.
    pub deltas: Vec<Vec<u8>>,
}

/// The whole of a workspace's search index, read from the store now: one entry per resource
/// with text. What a first publication, a new route or a change the journal no longer reaches
/// sends, through [`publish_whole`].
#[must_use]
pub fn whole(host: &Host) -> Vec<IndexEntry> {
    every_hit(host).iter().filter_map(entry).collect()
}

/// The deltas that publish every one of `entries` through `publisher` at `state`, and the
/// entries by resource. Plain data, read of no store: a host publishes ten thousand entries off
/// its frame.
#[must_use]
pub fn publish_whole(
    mut publisher: Publisher,
    state: IndexState,
    entries: Vec<IndexEntry>,
) -> Published {
    let kept = entries
        .iter()
        .map(|entry| (entry.resource.clone(), entry.clone()))
        .collect();
    let deltas = publisher
        .publish(state, entries)
        .iter()
        .filter_map(IndexDelta::encode)
        .collect();
    Published {
        publisher,
        entries: kept,
        deltas,
    }
}
