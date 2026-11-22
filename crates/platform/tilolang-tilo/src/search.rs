//! What search finds of a workspace: the text of every resource and item of every package.
//!
//! Every `Text` and `RichText` field of a type is searchable by default (a rich text by its text
//! as the person searching reads it: a reference to a resource they hold by what it is called
//! now, one they do not hold as not shared, never by the title it was written with,
//! [`crate::reading`]); a hit is the resource or item that holds it, found by its text, and it opens the host at that resource (its `open` view action, see
//! [`crate::effects::OpenEffect`]). A package will be able to say which fields to index; until
//! then all of them are, the most useful default for a notes app and a chat alike.
//!
//! A type says what its resources are to search and who they are with (`search { kind
//! conversation; people person }`): an entry of one carries that kind, those people with who made
//! it (its first version's author) first, and when it was made, so the shell draws a conversation
//! or a message as one and a box sends to a conversation and the people in it.
//!
//! Search is the system's: a window publishes every hit to the session's search index, which
//! finds them as every Tilo surface finds things, and keeps them after the app closes. The hits
//! are read from each package's index ([`crate::index`]), the one the store's lists read: one
//! pass over its fields, nothing kept beside it.

use tilolang_runtime::meta::Meta;

use crate::host::Host;
use crate::publish::resource_id;
use crate::resource::{DynResource, DynValue};
use crate::runtime::Actor;
use tilo_resource::search_index::{
    IndexAction, IndexDelta, IndexEntry, IndexKind, IndexPerson, IndexState, IndexWeight, Publisher,
};

/// One searchable resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    /// The id the resource is published under (`tl:<app>:<path>`): what its `open` action is
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
    /// The facts of the resource at `path`, of type `schema`, as `view` holds it: none for a type
    /// that declares no kind.
    fn of(
        view: &crate::index::view::View<'_>,
        path: &str,
        schema: &crate::schema::TypeSchema,
    ) -> Self {
        use crate::index::reflect::{CREATED, Stamp};
        let Some(kind) = schema.search_kind else {
            return Self::default();
        };
        let kind = match kind {
            tilolang::meta::SearchKind::Conversation => IndexKind::Conversation,
            tilolang::meta::SearchKind::Message => IndexKind::Message,
            tilolang::meta::SearchKind::Person => IndexKind::Person,
            tilolang::meta::SearchKind::Agent => IndexKind::Agent,
        };
        let made = tilo_failure::ignored!(
            "a resource whose first version cannot be read is told as made by nobody, at no time",
            view.field(path, CREATED).ok()
        )
        .flatten()
        .as_ref()
        .and_then(Stamp::of_value)
        .map(|stamp| stamp.version);
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
        for field in &schema.people {
            let value = tilo_failure::ignored!(
                "a field of people that cannot be read names nobody",
                view.field(path, field).ok()
            )
            .flatten();
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

/// The members a stored `Actor` field (or an option or a set of them) names.
fn members_in(value: Option<&tilolang_runtime::store::FieldValue>) -> Vec<tilo_actions::MemberId> {
    use tilolang_runtime::store::{FieldValue, Scalar};
    let of_json = |json: &serde_json::Value| match tilolang_runtime::value::from_json(json) {
        Ok(tilolang_runtime::value::Value::Actor(actor)) => actor.member_id().cloned(),
        Ok(tilolang_runtime::value::Value::Opt(Some(inner))) => match *inner {
            tilolang_runtime::value::Value::Actor(actor) => actor.member_id().cloned(),
            _ => None,
        },
        _ => None,
    };
    let of_text = |text: &str| {
        tilo_failure::ignored!(
            "a stored text that is no encoded actor names nobody",
            serde_json::from_str(text).ok()
        )
        .as_ref()
        .and_then(of_json)
    };
    match value {
        Some(FieldValue::Scalar(Scalar::Json(json))) => of_json(json).into_iter().collect(),
        Some(FieldValue::Scalar(Scalar::Text(text))) => of_text(text).into_iter().collect(),
        Some(FieldValue::Keys(keys)) => keys.iter().filter_map(|key| of_text(key)).collect(),
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

/// The searchable resources of the resource `doc` and the items below it, read from the resource
/// whole, each text as the person `reader` holds resources for reads it: what the index's hits
/// are held to.
#[must_use]
pub fn hits(
    meta: &Meta,
    record: &DynResource,
    doc: &str,
    reader: &dyn crate::reading::Holdings,
) -> Vec<Hit> {
    let mut out = Vec::new();
    collect(meta, record, doc, reader, &mut out);
    out
}

fn collect(
    meta: &Meta,
    record: &DynResource,
    path: &str,
    reader: &dyn crate::reading::Holdings,
    out: &mut Vec<Hit>,
) {
    let mut texts = Vec::new();
    let mut references: Vec<String> = Vec::new();
    for field in &record.schema().fields {
        let text = match record.get(&field.id) {
            Some(DynValue::Text(text)) => text.clone(),
            Some(DynValue::RichText(rich)) => {
                let value = tilolang_runtime::rich::RichValue {
                    blocks: rich
                        .state()
                        .map(|state| {
                            state
                                .body()
                                .all()
                                .filter_map(|id| state.block(id))
                                .map(|block| (**block).clone())
                                .collect()
                        })
                        .unwrap_or_default(),
                };
                for reference in value.references() {
                    let mark = reference.mark_value();
                    if reference.as_resource().is_some() && !references.contains(&mark) {
                        references.push(mark);
                    }
                }
                crate::reading::said(
                    reader,
                    path,
                    tilolang_runtime::store::FieldValue::Rich(value),
                )
                .unwrap_or_default()
            }
            _ => continue,
        };
        if !text.trim().is_empty() {
            texts.push(text);
        }
    }
    if let Some(title) = texts.first().cloned() {
        out.push(Hit {
            resource: resource_id(&meta.app, path),
            app_title: meta.title.clone(),
            type_name: record.schema().name.clone(),
            title,
            texts,
            references,
            facts: Facts::default(),
        });
    }
    for field in &record.schema().fields {
        if let Some(DynValue::Collection(items)) = record.get(&field.id) {
            for (item_id, item) in items.iter() {
                collect(
                    meta,
                    item,
                    &crate::index::derive::item_path(path, item_id.as_str()),
                    reader,
                    out,
                );
            }
        }
    }
}

/// Every hit of the workspace, by package and path, read from the indexes now: what a window
/// publishes to the session's search index, which is what search finds things through. One pass
/// over each package's index. Each text is as the window's person reads it: the index matches a
/// query against what they read, never against the title of a resource they do not hold. A
/// resource the records holding it keep from them (`type Direct (readers: people)`) is no hit.
#[must_use]
pub fn every_hit(host: &Host) -> Vec<Hit> {
    hits_of(&host.store().snapshot(), &catalogue(host), host.viewer())
}

/// The apps the host runs a package of, each with the name people call it in their language: what
/// a hit is grouped by and what it opens through. What a read off the frame takes with it, beside
/// the snapshot it reads.
#[must_use]
pub fn catalogue(host: &Host) -> Vec<(String, String)> {
    let words = host.runtime().words();
    host.packages()
        .iter()
        .map(|package| {
            (
                package.meta.app.clone(),
                crate::publish::app_name(&*words, &package.meta),
            )
        })
        .collect()
}

/// [`every_hit`], read from `snapshot` and the apps `catalogue` names, for `viewer`: what a
/// thread of its own reads in one pass, a workspace of ten thousand included.
fn hits_of(
    snapshot: &crate::store::Snapshot,
    catalogue: &[(String, String)],
    viewer: &Actor,
) -> Vec<Hit> {
    let _schemas = snapshot.scoped();
    let reader: &crate::store::Snapshot = snapshot;
    let mut out = Vec::new();
    for (app, title) in catalogue {
        let Some(view) = snapshot.view(app) else {
            continue;
        };
        let Ok(texted) = view.every_text() else {
            continue;
        };
        out.extend(texted.into_iter().filter_map(|held| {
            if !view.reads(&held.path, viewer).unwrap_or(false) {
                return None;
            }
            let texts: Vec<String> = held
                .texts
                .into_iter()
                .filter_map(|text| reader.said(&held.path, text))
                .collect();
            Some(Hit {
                resource: resource_id(app, &held.path),
                app_title: title.clone(),
                type_name: held.schema.name.clone(),
                title: texts.first()?.clone(),
                texts,
                references: held.references,
                facts: Facts::of(&view, &held.path, held.schema),
            })
        }));
    }
    out
}

/// The action that opens a resource at its anchor (`open(r)`), as every published resource
/// offers it bound to itself.
const OPEN_ACTION: &str = "open";

/// Every hit of the workspace as the session's persistent search index keeps it: one [`IndexEntry`]
/// per resource with text, short metadata only (never a prose body), keyed by the resource's
/// published id and opening it through its document's `open` action. This is what
/// a live tilolang instance publishes on its connection, so a Mail message or a note is found by
/// a word of its body through the system's one search, whether or not the app runs.
fn entries_of(
    snapshot: &crate::store::Snapshot,
    catalogue: &[(String, String)],
    viewer: &Actor,
) -> Vec<IndexEntry> {
    hits_of(snapshot, catalogue, viewer)
        .into_iter()
        .filter_map(|hit| {
            entry(
                &hit.resource,
                &hit.title,
                &hit.type_name,
                &hit.app_title,
                &hit.texts,
                &hit.references,
                hit.facts,
            )
        })
        .collect()
}

/// The entry the session's persistent search index keeps of the resource at `path` of the
/// package `meta`, read now: none when it holds no text, when it is gone, or when the records
/// holding it keep it from the host's viewer. What an instance publishes for the resources a
/// change reached, instead of reading the workspace again.
#[must_use]
pub fn index_entry_at(host: &Host, meta: &Meta, path: &str) -> Option<IndexEntry> {
    let snapshot = host.store().snapshot();
    let _schemas = snapshot.scoped();
    let reader: &crate::store::Snapshot = &snapshot;
    let view = snapshot.view(&meta.app)?;
    if !view.reads(path, host.viewer()).unwrap_or(false) {
        return None;
    }
    let held = view.texts_at(path).ok()??;
    let facts = Facts::of(&view, &held.path, held.schema);
    let texts: Vec<String> = held
        .texts
        .into_iter()
        .filter_map(|text| reader.said(&held.path, text))
        .collect();
    let resource = resource_id(&meta.app, &held.path);
    let app_title = crate::publish::app_name(&*host.runtime().words(), meta);
    entry(
        &resource,
        texts.first()?,
        &held.schema.name,
        &app_title,
        &texts,
        &held.references,
        facts,
    )
}

/// The index's entry of the resource `resource` with `texts`: short metadata first (a sender, a
/// subject), then long prose (a body), so the index keeps a bounded number of fields
/// ([`IndexEntry::bounded`]) with the metadata that ranks highest. A hit opens through the `open`
/// of the document that holds the resource, which every window showing anything of the document
/// publishes: the caller names the resource as the item to show, which no window may show yet.
fn entry(
    resource: &str,
    title: &str,
    type_name: &str,
    app_title: &str,
    texts: &[String],
    references: &[String],
    facts: Facts,
) -> Option<IndexEntry> {
    let mut fields: Vec<(String, IndexWeight)> = texts
        .iter()
        .skip(1)
        .filter(|text| text.chars().count() <= SHORT_TEXT)
        .map(|text| (text.clone(), IndexWeight::Metadata))
        .collect();
    fields.extend(
        texts
            .iter()
            .skip(1)
            .filter(|text| text.chars().count() > SHORT_TEXT)
            .map(|text| (text.clone(), IndexWeight::Body)),
    );
    let entry = IndexEntry {
        key: resource.to_owned(),
        resource: resource.to_owned(),
        action: Some(IndexAction {
            resource: document_of(resource).to_owned(),
            action: OPEN_ACTION.to_owned(),
        }),
        title: title.to_owned(),
        subtitle: Some(type_name.to_owned()),
        group: Some(app_title.to_owned()),
        icon: None,
        boost: 0,
        fields,
        secret: false,
        references: references.to_vec(),
        kind: facts.kind,
        people: facts.people,
        when: facts.when,
        ..IndexEntry::default()
    };
    entry.bounded()
}

/// The published record that holds the published resource `resource`: an item's path goes on
/// from its document's id after a `/`.
fn document_of(resource: &str) -> &str {
    resource
        .split_once('/')
        .map_or(resource, |(record, _)| record)
}

/// The longest text an entry keeps as a short field: a name, a subject, a sender.
const SHORT_TEXT: usize = 80;

/// The whole of a workspace's search index, read on a worker thread: every resource with
/// text, one pass over the indexes, as publication's first trees are read. A frame after it ends
/// takes it in and sends what it holds; until then the window shows.
pub struct WholeIndex {
    building: crate::background::Pending<Published>,
}

/// What [`WholeIndex`] read.
pub struct Published {
    /// The publisher, holding the whole set as what it has sent.
    pub publisher: Publisher,
    /// One entry per resource with text, by the resource's published id.
    pub entries: std::collections::BTreeMap<String, IndexEntry>,
    /// The deltas that publish them, each as it crosses the broker.
    pub deltas: Vec<Vec<u8>>,
}

impl WholeIndex {
    /// Read the index of `host` off the frame, publishing every entry through `publisher` at
    /// `state`: what a first publication, a new route or a change the journal no longer reaches
    /// sends. `wake` is called once it is read.
    #[must_use]
    pub fn start(
        host: &Host,
        publisher: Publisher,
        state: IndexState,
        wake: Option<crate::background::Waker>,
    ) -> Self {
        let snapshot = host.store().snapshot();
        let catalogue = catalogue(host);
        let viewer = host.viewer().clone();
        Self {
            building: crate::background::Pending::start(
                move || read(&snapshot, &catalogue, &viewer, publisher.clone(), state),
                wake,
            ),
        }
    }

    /// What it read, once it has.
    pub fn poll(&mut self) -> Option<Published> {
        self.building.poll()
    }
}

/// [`entries_of`] and the deltas that publish them, read from the snapshot `snapshot` and the
/// apps `catalogue` names, for `viewer`.
#[must_use]
pub fn read(
    snapshot: &crate::store::Snapshot,
    catalogue: &[(String, String)],
    viewer: &Actor,
    mut publisher: Publisher,
    state: IndexState,
) -> Published {
    let entries = entries_of(snapshot, catalogue, viewer);
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
