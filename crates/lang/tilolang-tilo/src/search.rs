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
//! A resource whose page shows its text (a type with a default view: a note, a card, a document
//! of source) is found by each of its paragraphs and lines too: every paragraph or line with
//! words, where the text has more than one, is a row of its own, read as its words and said to
//! be in the resource, which opens the resource at that place ([`block_anchor`]). A message,
//! which no page of its own shows, is one row however many paragraphs it holds.
//!
//! A type says what its resources are to search and who they are with (`search { kind
//! conversation; people person }`): an entry of one carries that kind, those people with who made
//! it (its first version's author) first, and when it was made, so the shell draws a conversation
//! or a message as one and a box sends to a conversation and the people in it.
//!
//! Search is the system's: the device's daemon publishes every hit to its search index, which
//! finds them as every Tilo surface finds things, and keeps them after the app closes. The hits
//! are read from the device's resource store ([`crate::resources`]): every resource of a type
//! that holds text, nothing kept beside it. Each text is as the device's person reads it, and a
//! resource the records holding it keep from them (`type Direct (readers: people)`) is no hit.
//! The index keeps the hits of one store document in a slot of their own
//! ([`entries_in`]), which a caller who may read that document passes, so a hit of a resource
//! sealed to its readers names them ([`Hit::readers`]), by the rule the store seals it by: the
//! index answers it to them alone, whoever published it.

use std::collections::BTreeSet;
use std::sync::Arc;

use tilo_resource::search_index::{
    IndexAction, IndexDelta, IndexEntry, IndexKind, IndexPerson, IndexState, IndexWeight, Publisher,
};
use tilolang_runtime::meta::{FieldKind, Meta};
use tilolang_runtime::rich::RichValue;
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
    /// The paragraphs and lines of its text that are found on their own, in reading order: none
    /// for a resource no page shows, or whose text is one paragraph or one line.
    pub blocks: Vec<BlockHit>,
    /// The members who alone read it, each as a change's readers name them
    /// (`tilo_actions::contract::reader_name`), when a record holding it names its readers
    /// (`type Direct (readers: people)`): the store seals it to them, and search finds it for them
    /// alone. `None` for a resource whoever reads its document reads.
    pub readers: Option<Vec<String>>,
    /// The published resource it lives in, when it is an item of one (a chapter of a book, a
    /// note of a notebook): what a search scoped to that resource narrows to. `None` for a
    /// resource that lives on its own.
    pub owner: Option<String>,
    /// The published resource whose `open` shows it: the document of its app's library it is in
    /// or is, for an app each of whose windows shows one (`document: library.documents`), so a
    /// hit on a page or a mark opens that document's window at it; the record it is in otherwise.
    pub opens_in: String,
}

/// One paragraph of a resource's rich text, found on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockHit {
    /// The paragraph's place in the text that holds it.
    pub block: Block,
    /// Its words, as the person searching reads them.
    pub text: String,
}

/// A paragraph of a resource's rich text, or a line of its plain text, by where it is: the field
/// that holds the text, and its place among the text's paragraphs (or lines), counted from 0,
/// each paragraph before what it holds (as an editor of the text holds them). A rich text keeps
/// no ids for its paragraphs; the place is republished with every change that moves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// The id of the text field.
    pub field: String,
    /// Its place in it.
    pub place: usize,
}

/// What separates a resource's published id from the place an anchor names in it.
const BLOCK_MARK: char = '#';

/// What separates a text field's name from a place in an anchor.
const PLACE_MARK: char = '.';

/// The anchor of the place `block` of the resource published as `resource`: what a hit on a
/// paragraph or a line opens, the resource shown at that place.
#[must_use]
pub fn block_anchor(resource: &str, block: &Block) -> String {
    format!(
        "{resource}{BLOCK_MARK}{}{PLACE_MARK}{}",
        block.field, block.place
    )
}

/// The resource an anchor names, and the place in it when it names one ([`block_anchor`]).
#[must_use]
pub fn split_anchor(anchor: &str) -> (&str, Option<Block>) {
    match anchor.split_once(BLOCK_MARK) {
        Some((resource, block)) => (
            resource,
            block.rsplit_once(PLACE_MARK).and_then(|(field, place)| {
                Some(Block {
                    field: field.to_owned(),
                    place: place.parse().ok()?,
                })
            }),
        ),
        None => (anchor, None),
    }
}

/// Whether the index key `key` is the entry of the resource `resource` or of a paragraph of it.
#[must_use]
pub fn keys_resource(key: &str, resource: &str) -> bool {
    split_anchor(key).0 == resource
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

/// `member` as an entry names them: a person by their root key, an agent by its actor's bytes,
/// `agent:<person>/<id>`, named by what they are called now.
fn person_of(member: &tilo_actions::MemberId) -> Option<IndexPerson> {
    if member.is_agent() {
        return Some(IndexPerson {
            name: String::new(),
            anchor: String::new(),
            identity: String::from_utf8_lossy(&member.actor_bytes()).into_owned(),
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
    host: &'a Host,
    store: &'a ResourceStore,
    apps: BTreeSet<String>,
    unshared: Arc<str>,
}

impl<'a> Holdings<'a> {
    fn of(host: &'a Host) -> Self {
        Self {
            host,
            store: host.store(),
            apps: host
                .packages()
                .iter()
                .map(|package| package.meta.app.clone())
                .collect(),
            unshared: Arc::from(host.runtime().unshared()),
        }
    }

    /// Who alone reads `at`, by the one rule the store seals it by and history lists its changes
    /// by ([`tilolang_runtime::Runtime::readers`]), each as a change's readers name them.
    fn readers(&self, at: &ResourceRef) -> Option<Vec<String>> {
        self.host.runtime().readers(at).map(|readers| {
            readers
                .iter()
                .map(tilo_actions::contract::reader_name)
                .collect()
        })
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
        // A field the type conceals is never read into the index: a credential, a note kept for
        // one person, whatever search must neither hold nor find.
        .filter(|field| !declared.search.concealed.contains(&field.id))
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
    let values = fields.iter().filter_map(|field| {
        let value = holdings.store.read(at, field).ok()?;
        Some((field.id.clone(), value))
    });
    hit_of(holdings, named, at, values)
}

/// The hit of the resource `at` of the package `meta` that holds the texts `values`, each by the
/// id of its field: none when it holds no text.
fn hit_of(
    holdings: &Holdings<'_>,
    (meta, app_title): (&Meta, &str),
    at: &ResourceRef,
    values: impl IntoIterator<Item = (String, FieldValue)>,
) -> Option<Hit> {
    let type_name = meta
        .types
        .iter()
        .find(|declared| declared.id == at.ty)?
        .name
        .clone();
    let id = at.id_string();
    // A page shows the resource's text: each of its paragraphs is a place in it to open at.
    let paged = meta.default_view(&at.ty).is_some();
    let mut texts = Vec::new();
    let mut references: Vec<String> = Vec::new();
    let mut blocks = Vec::new();
    for (field, value) in values {
        let value = match value {
            FieldValue::Text(text) => {
                // A page shows the source's lines: each of them is a place in it to open at.
                if paged {
                    blocks.extend(line_hits(&field, &text));
                }
                FieldValue::Text(text)
            }
            FieldValue::Rich(rich) => {
                for reference in rich.references() {
                    let mark = reference.mark_value();
                    if reference.as_resource().is_some() && !references.contains(&mark) {
                        references.push(mark);
                    }
                }
                if paged {
                    blocks.extend(block_hits(holdings, &id, &field, &rich));
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
        blocks,
        readers: holdings.readers(at),
        // The resource the store holds it in: what a search scoped to that resource finds it
        // by, published as the index names every resource (`tl:<app>:<doc/item>`).
        owner: holdings
            .store
            .container(at)
            .ok()
            .flatten()
            .map(|(holder, _)| resource_id(&meta.app, &holder.id_string())),
        opens_in: document_item_of(holdings, meta, at).map_or_else(
            || document_of(&resource_id(&meta.app, &id)).to_owned(),
            |item| resource_id(&meta.app, &item.id_string()),
        ),
    })
}

/// The item of `meta`'s library that `at` is or is in, when each window of the app shows one
/// item of a collection of its root (`document: library.documents`): `at` or the nearest
/// resource holding it whose type is the collection's.
fn document_item_of(holdings: &Holdings<'_>, meta: &Meta, at: &ResourceRef) -> Option<ResourceRef> {
    if meta.document_items.is_empty() {
        return None;
    }
    let mut on = Some(at.clone());
    while let Some(item) = on.filter(|item| item.item.is_some()) {
        let ty = holdings.store.type_of(&item).ok()?;
        if meta
            .document_items
            .iter()
            .any(|items| items.ty == ty.as_str())
        {
            return Some(item);
        }
        on = holdings
            .store
            .container(&item)
            .ok()
            .flatten()
            .map(|(holder, _)| holder);
    }
    None
}

/// The paragraphs of `rich`, the rich text in the field `field` of the resource `id`, that are
/// found on their own: each one with words, read as the person searching reads it, at its place
/// ([`Block`]). None when fewer than two say anything: a text of one paragraph is found as the
/// resource it is in.
fn block_hits(holdings: &Holdings<'_>, id: &str, field: &str, rich: &RichValue) -> Vec<BlockHit> {
    // Every paragraph, each before the paragraphs it holds: the order of their places.
    let mut flat = Vec::new();
    let mut pending: Vec<&tilo_doc::snapshot::BlockView> = rich.blocks.iter().rev().collect();
    while let Some(block) = pending.pop() {
        flat.push(block);
        pending.extend(block.children.iter().rev());
    }
    let found: Vec<BlockHit> = flat
        .into_iter()
        .enumerate()
        .filter(|(_, block)| !block.text.trim().is_empty())
        .filter_map(|(place, block)| {
            let one = RichValue {
                blocks: vec![tilo_doc::snapshot::BlockView {
                    children: Vec::new(),
                    ..block.clone()
                }],
                styles: None,
            };
            let text = crate::reading::said(holdings, id, FieldValue::Rich(one))?;
            (!text.trim().is_empty()).then(|| BlockHit {
                block: Block {
                    field: field.to_owned(),
                    place,
                },
                text,
            })
        })
        .collect();
    if found.len() < 2 {
        return Vec::new();
    }
    found
}

/// The lines of the plain text `text` in the field `field` that are found on their own: each
/// line with words at its place ([`Block`]), so a search of a document's source (a Typst file,
/// a script) opens the line it found. None when fewer than two lines say anything: a text of one
/// line is found as the resource it is in.
fn line_hits(field: &str, text: &str) -> Vec<BlockHit> {
    let mut found = Vec::new();
    for (place, line) in text.lines().enumerate() {
        let words = line.trim();
        if !words.is_empty() {
            found.push(BlockHit {
                block: Block {
                    field: field.to_owned(),
                    place,
                },
                text: words.to_owned(),
            });
        }
        if found.len() >= MAX_LINE_HITS {
            break;
        }
    }
    if found.len() < 2 {
        return Vec::new();
    }
    found
}

/// The most lines of one text that are entries of their own: a document of thousands of lines
/// does not crowd the index out of the workspace's other resources.
const MAX_LINE_HITS: usize = 2_000;

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

/// Every hit of the workspace, read from the store now: every entry system search keeps of it,
/// before a document's set is narrowed to that document. One read of every resource of each
/// type that holds text. Each text is as the host's person reads it: the index matches a query
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

/// The entries the session's persistent search index keeps of the resource `id` (`doc` or
/// `doc/item`) of the package `meta`, read now: the resource's own first, then one for each of
/// its paragraphs found on their own ([`Hit::blocks`]). None when it holds no text, when it is
/// gone, when it is no resource of `meta`, or when the records holding it keep it from
/// the host's viewer. What an instance publishes for the resources a
/// change reached, instead of reading the workspace again.
#[must_use]
pub fn index_entries_at(host: &Host, meta: &Meta, id: &str) -> Vec<IndexEntry> {
    let id = tilolang_runtime::value::flat_id(id);
    let Some(ty) = host.store().type_of_id(&id) else {
        return Vec::new();
    };
    index_entries_of(host, meta, &ty, &id)
}

/// [`index_entries_at`] for the resource `id` of `meta`'s type `ty`, its type known rather than
/// read from the store: what a document's resources are read by, their kind naming their type.
fn index_entries_of(host: &Host, meta: &Meta, ty: &str, id: &str) -> Vec<IndexEntry> {
    let store = host.store();
    let fields = texted(store, meta, ty);
    let Some(at) = ResourceRef::parse_id(id, ty.to_owned()).filter(|_| !fields.is_empty()) else {
        return Vec::new();
    };
    if !host.reads_for(host.viewer())(&at) {
        return Vec::new();
    }
    let title = crate::publish::app_name(&*host.runtime().words(), meta);
    let Some(hit) = hit_at(&Holdings::of(host), (meta, &title), &at, &fields) else {
        return Vec::new();
    };
    entry(&hit).into_iter().chain(block_entries(&hit)).collect()
}

/// Every entry the search index keeps of the resources the store's document `document` holds
/// now, each read as the host's person reads it ([`index_entries_at`]): what the device's daemon
/// publishes for that document, in one slot of its own. Each run of a document publishes the
/// whole of its set, so an entry the document no longer holds — a paragraph deleted, a
/// resource removed — goes with it.
#[must_use]
pub fn entries_in(host: &Host, document: &str) -> Vec<IndexEntry> {
    let store = host.store();
    let packages = host.packages();
    let mut out = Vec::new();
    for (kind, id) in store.resources_in(document) {
        // The type the store's kind names, as the store answers a resource's type: the one the
        // package that declares it reads its fields with.
        let declared = {
            let kinds = store.kinds();
            kinds
                .of_kind(&kind)
                .map(|entry| (entry.app.clone(), entry.id.clone()))
        };
        let Some((app, ty)) = declared else {
            continue;
        };
        let Some(package) = packages.iter().find(|package| package.meta.app == app) else {
            continue;
        };
        out.extend(index_entries_of(host, &package.meta, &ty, &id));
    }
    out
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
    // A record that names a picture's bytes is a picture for search: the
    // system's image analysis reads it from the content store, so the picture
    // is found by the words in it and by what it shows, in every app.
    let image = hit
        .texts
        .iter()
        .find_map(|text| tilo_resource::search_index::picture_address(text));
    let entry = IndexEntry {
        key: hit.resource.clone(),
        resource: hit.resource.clone(),
        action: Some(IndexAction {
            resource: hit.opens_in.clone(),
            action: OPEN_ACTION.to_owned(),
        }),
        owner: hit.owner.clone(),
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
        image,
        readers: hit.readers.clone(),
        ..IndexEntry::default()
    };
    entry.bounded()
}

/// The entries of the paragraphs and lines of `hit` found on their own: each a row read as its
/// words (its opening words, for a long one), said to be in the resource it is in, and opening
/// that resource at it. What links to the resource stays its own entry's: a paragraph or line
/// adds no backlink.
fn block_entries(hit: &Hit) -> impl Iterator<Item = IndexEntry> + '_ {
    hit.blocks.iter().filter_map(|block| {
        let collapsed = block.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let long = collapsed.chars().count() > BLOCK_LABEL;
        let title = if long {
            let mut cut: String = collapsed.chars().take(BLOCK_LABEL - 1).collect();
            cut.push('…');
            cut
        } else {
            collapsed
        };
        let anchor = block_anchor(&hit.resource, &block.block);
        IndexEntry {
            key: anchor.clone(),
            resource: anchor,
            action: Some(IndexAction {
                resource: hit.opens_in.clone(),
                action: OPEN_ACTION.to_owned(),
            }),
            owner: Some(hit.resource.clone()),
            title,
            subtitle: Some(hit.title.clone()),
            group: Some(hit.app_title.clone()),
            icon: Some(BLOCK_ICON.to_owned()),
            fields: long
                .then(|| (block.text.clone(), IndexWeight::Body))
                .into_iter()
                .collect(),
            readers: hit.readers.clone(),
            ..IndexEntry::default()
        }
        .bounded()
    })
}

/// The longest a paragraph's row reads; the whole paragraph is still found.
const BLOCK_LABEL: usize = 72;

/// What a paragraph's row is drawn with.
const BLOCK_ICON: &str = "align-left";

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
/// with text, then one per paragraph found on its own. What a caller that reads an app whole
/// sends, through [`publish_whole`]: an index that keeps only so many entries keeps every
/// resource before any paragraph.
#[must_use]
pub fn whole(host: &Host) -> Vec<IndexEntry> {
    let hits = every_hit(host);
    hits.iter()
        .filter_map(entry)
        .chain(hits.iter().flat_map(block_entries))
        .collect()
}

/// The deltas that publish every one of `entries` through `publisher` at `state`, and the
/// entries by resource. Plain data, read of no store: a caller publishes ten thousand entries
/// off its frame.
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
