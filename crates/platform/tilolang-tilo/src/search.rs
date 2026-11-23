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
//! A resource whose page shows its rich text (a type with a default view: a note, a card) is
//! found by each of its paragraphs too: every paragraph with words, where the text has more than
//! one, is a row of its own, read as its words and said to be in the resource, which opens the
//! resource at that paragraph ([`block_anchor`]). A message, which no page of its own shows, is
//! one row however many paragraphs it holds.
//!
//! Search is the system's: a window publishes every hit to the session's search index, which
//! finds them as every Tilo surface finds things, and keeps them after the app closes. The hits
//! are read from the device's resource store ([`crate::resources`]): every resource of a type
//! that holds text, nothing kept beside it.

use std::collections::BTreeSet;
use std::sync::Arc;

use tilo_resource::search_index::{
    IndexAction, IndexDelta, IndexEntry, IndexState, IndexWeight, Publisher,
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
    /// The paragraphs of its rich text that are found on their own, in reading order: none for a
    /// resource no page shows, or whose text is one paragraph.
    pub blocks: Vec<BlockHit>,
}

/// One paragraph of a resource's rich text, found on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockHit {
    /// The paragraph's place in the text that holds it.
    pub block: Block,
    /// Its words, as the person searching reads them.
    pub text: String,
}

/// A paragraph of a resource's rich text, by where it is: the field that holds the text, and
/// its place among the text's paragraphs, counted from 0, each paragraph before what it holds
/// (as an editor of the text holds them). A rich text keeps no ids for its paragraphs; the
/// place is republished with every change that moves it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// The id of the rich-text field.
    pub field: String,
    /// The paragraph's place in it.
    pub place: usize,
}

/// What separates a resource's published id from the paragraph an anchor names in it.
const BLOCK_MARK: char = '#';

/// What separates a paragraph's field from its place in an anchor.
const PLACE_MARK: char = '.';

/// The anchor of the paragraph `block` of the resource published as `resource`: what a hit on
/// the paragraph opens, the resource shown at that paragraph.
#[must_use]
pub fn block_anchor(resource: &str, block: &Block) -> String {
    format!(
        "{resource}{BLOCK_MARK}{}{PLACE_MARK}{}",
        block.field, block.place
    )
}

/// The resource an anchor names, and the paragraph in it when it names one ([`block_anchor`]).
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
            value @ FieldValue::Text(_) => value,
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
        blocks,
    })
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

/// The searchable resources of the document `doc` of the package `meta` and the items below
/// it, read from the store now, each text as the person at the host reads it.
#[must_use]
pub fn hits(host: &Host, meta: &Meta, doc: &str) -> Vec<Hit> {
    let holdings = Holdings::of(host);
    let store = host.store();
    let title = crate::publish::app_name(&*host.runtime().words(), meta);
    let Some(ty) = store.type_of_id(doc) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut pending = vec![ResourceRef::root(doc, ty)];
    while let Some(at) = pending.pop() {
        let fields = texted(store, meta, &at.ty);
        out.extend(hit_at(&holdings, (meta, &title), &at, &fields));
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
                    .and_then(|entry| entry.items.first().cloned())
            };
            let Some(item_ty) = collection else {
                continue;
            };
            let spec = FieldSpec {
                id: field.id.clone(),
                kind: FieldKind::Collection,
            };
            let Ok(listing) = store.listing(&at, &spec) else {
                continue;
            };
            items.extend(listing.iter().map(|id| at.item(&id, item_ty.clone())));
        }
        pending.extend(items.into_iter().rev());
    }
    out
}

/// Every hit of the workspace, read from the store now: what a window publishes to the session's
/// search index, which is what search finds things through. One read of every resource of each
/// type that holds text. Each text is as the window's person reads it: the index matches a query
/// against what they read, never against the title of a resource they do not hold.
#[must_use]
pub fn every_hit(host: &Host) -> Vec<Hit> {
    let holdings = Holdings::of(host);
    let store = host.store();
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
                out.extend(hit_of(&holdings, (meta, &title), &at, values));
            }
        }
    }
    out
}

/// The action that opens a resource at its anchor (`open(r)`), as every published resource
/// offers it bound to itself.
const OPEN_ACTION: &str = "open";

/// The entries the session's persistent search index keeps of the resource `id` (`doc` or
/// `doc/item`) of the package `meta`, read now: the resource's own first, then one for each of
/// its paragraphs found on their own ([`Hit::blocks`]). None when it holds no text, when it is
/// gone, or when it is no resource of `meta`. What an instance publishes for the resources a
/// change reached, instead of reading the workspace again.
#[must_use]
pub fn index_entries_at(host: &Host, meta: &Meta, id: &str) -> Vec<IndexEntry> {
    let store = host.store();
    let id = tilolang_runtime::value::flat_id(id);
    let Some(ty) = store.type_of_id(&id) else {
        return Vec::new();
    };
    let fields = texted(store, meta, &ty);
    let Some(at) = ResourceRef::parse_id(&id, ty).filter(|_| !fields.is_empty()) else {
        return Vec::new();
    };
    let title = crate::publish::app_name(&*host.runtime().words(), meta);
    let Some(hit) = hit_at(&Holdings::of(host), (meta, &title), &at, &fields) else {
        return Vec::new();
    };
    entry(&hit).into_iter().chain(block_entries(&hit)).collect()
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
        image,
        ..IndexEntry::default()
    };
    entry.bounded()
}

/// The entries of the paragraphs of `hit` found on their own: each a row read as its words (its
/// opening words, for a long one), said to be in the resource it is in, and opening that resource
/// at it. What links to the resource stays its own entry's: a paragraph adds no backlink.
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
                resource: document_of(&hit.resource).to_owned(),
                action: OPEN_ACTION.to_owned(),
            }),
            title,
            subtitle: Some(hit.title.clone()),
            group: Some(hit.app_title.clone()),
            icon: Some(BLOCK_ICON.to_owned()),
            fields: long
                .then(|| (block.text.clone(), IndexWeight::Body))
                .into_iter()
                .collect(),
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
/// with text, then one per paragraph found on its own. What a first publication, a new route or
/// a change the journal no longer reaches sends, through [`publish_whole`]: an index that keeps
/// only so many entries keeps every resource before any paragraph.
#[must_use]
pub fn whole(host: &Host) -> Vec<IndexEntry> {
    let hits = every_hit(host);
    hits.iter()
        .filter_map(entry)
        .chain(hits.iter().flat_map(block_entries))
        .collect()
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
