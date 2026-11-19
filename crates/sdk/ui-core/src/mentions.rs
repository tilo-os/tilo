//! Mentions as a workspace fact: who a record's rich text names, read from the record itself. A mention
//! is a run of rich text marked `mention` with the member's id, whichever app wrote it: a
//! paragraph of a note (an Automerge text mark, as `tilo-doc` keeps them) or a block of a chat message
//! (a stored mark span, as `tilo_document::ClipBlock` keeps them).

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::resource::DocumentId;

use tilo_actions::MemberId;

/// The collection of the conversations items are said in: what a mention's `channel` names an
/// item of.
pub const CHANNELS: &str = "channels";

/// The mark a mention's run carries, valued with the member's id ([`tilo_reference::Embed`]).
pub const MARK: &str = "mention";

/// Whether a `mention` mark's value names a resource rather than a member, in any form it was written
/// in (`tilo_reference::embed`).
#[must_use]
pub fn names_resource(value: &str) -> bool {
    tilo_reference::embed::names_resource(value)
}

/// The member a `mention` mark's value names, when it names one.
#[must_use]
pub fn member_of(value: &str) -> Option<MemberId> {
    match tilo_reference::Embed::of_mark(value)? {
        tilo_reference::Embed::Member(member) => Some(member),
        tilo_reference::Embed::Resource(_) => None,
    }
}
/// The key a stored enum value keeps its variant under (`tilo_resource::VARIANT_KEY`).
const VARIANT_KEY: &str = "$variant";
/// The key the `Str` variant of a stored mark value keeps its string under, and the bare key an older
/// layout kept it under.
const STR_FIELD: &str = "$field/str/0";
const STR_FIELD_FLAT: &str = "0";
/// The key and value slot of an item of a stored collection (`tilo_resource::Collection`): `{ <id>: {
/// k: <order>, v: <item> } }`.
const ITEM_VALUE: &str = "v";
/// The record's identity map (`crate::resource`), never content.
const RESOURCE_META: &str = "tilo:record";

/// Deepest a mention is looked for, and most objects read in one record: the document is replicated
/// input, not a request to walk it without end.
const MAX_DEPTH: usize = 32;
const MAX_OBJECTS: usize = 200_000;

/// Longest mark value, in bytes, a batch of changes is read for as it spells it out character by
/// character: far past any member's id, so a long text written under a value's key costs no more.
const MAX_SPELLED_VALUE: usize = 1024;

/// One mention in a record: who is named, by whom, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
    /// The member named.
    pub person: MemberId,
    /// Who wrote it, when the record says: the run's author mark, or the item's author.
    pub by: Option<MemberId>,
    /// The item it was written in, as the record's top-level collection keeps it: its field and id.
    pub item: Option<(String, String)>,
    /// The paragraph it is in (a note's block), when its text has an id.
    pub block: Option<String>,
    /// What the item is called: its `title` or `name`.
    pub title: Option<String>,
    /// The channel the item was said in, by id, for an item of a chat.
    pub channel: Option<String>,
    /// The text around it. The text around it, as the person reading reads it ([`Reader`]): a reference
    /// to a resource they cannot read says only that something is there, never the title it was written
    /// with.
    pub excerpt: String,
}

/// How a reference in the text reads for the person told ([`tilo_reference::embed::Reading`]): a
/// resource they hold by what it is called now, one they cannot read as not shared.
pub type Reader<'a> = &'a dyn Fn(&tilo_reference::Embed) -> tilo_reference::embed::Reading;

/// What reads a reference for someone whose holdings nobody here can tell: a person as the text names
/// them, every resource as not shared. A reference never grants a read.
#[must_use]
pub fn reading_nothing(reference: &tilo_reference::Embed) -> tilo_reference::embed::Reading {
    if reference.as_member().is_some() {
        tilo_reference::embed::Reading::AsWritten
    } else {
        tilo_reference::embed::Reading::Unshared
    }
}

/// What a run of changes may do to the mentions of some people a record holds, read from the changes
/// alone: whether a value they write names one of the people (a new mention of them is such a value,
/// as a text's mark or a stored mark's), and whether they take anything away (delete, overwrite or
/// unmark), which can end a mention already there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Touched {
    pub names: bool,
    pub removes: bool,
}

/// What `changes`, encoded changes as a record's history keeps them, may do to the mentions of
/// `people` ([`Touched`]); `None` when they do not read as changes. Costs what the changes hold, never
/// what the record does: a host follows a large record's mentions without reading it whole.
#[must_use]
pub fn touched_by(
    changes: &[u8],
    people: &std::collections::BTreeSet<MemberId>,
) -> Option<Touched> {
    use automerge::legacy::{Key, MarkData, ObjectId, OpId, OpType};
    let mut touched = Touched::default();
    // A stored mark's value kept as a text rather than a string (a model's `String` field is an
    // Automerge text, as a chat paragraph's mark keeps its member) arrives as the text made under
    // the value's key and then its characters, one op each: gathered here, by the text's id, to
    // read the member they spell once every change is read.
    let mut spelled: std::collections::HashMap<OpId, String> = std::collections::HashMap::new();
    for change in automerge::Automerge::parse_changes(changes).ok()? {
        let change = change.decode();
        for (counter, op) in (change.start_op.get()..).zip(change.operations) {
            // A person's id is written in many places that are no mention of them (who wrote a
            // change, who sits where): only a mention's mark, or the value a stored mark span
            // keeps ([`Walk::mark_value`]), can make one.
            // So can an item's channel that is a direct conversation with one of them
            // ([`Walk::addressed`]).
            let key = match &op.key {
                Key::Map(key) => key.as_str(),
                Key::Seq(_) => "",
            };
            let mark_value = matches!(key, "value" | STR_FIELD | STR_FIELD_FLAT);
            match (&op.action, &op.obj) {
                (OpType::Make(ObjType::Text), _) if mark_value => {
                    spelled.insert(OpId::new(counter, &change.actor_id), String::new());
                }
                (OpType::Put(ScalarValue::Str(piece)), ObjectId::Id(text)) if op.insert => {
                    if let Some(value) = spelled.get_mut(text)
                        && value.len() < MAX_SPELLED_VALUE
                    {
                        value.push_str(piece);
                    }
                }
                _ => {}
            }
            match &op.action {
                OpType::Put(ScalarValue::Str(value))
                    if (mark_value
                        && member_of(value).is_some_and(|member| people.contains(&member)))
                        || (key == "channel"
                            && MemberId::direct_conversation_members(value).is_some_and(
                                |members| members.iter().any(|m| people.contains(m)),
                            )) =>
                {
                    touched.names = true;
                }
                OpType::MarkBegin(MarkData {
                    name,
                    value: ScalarValue::Str(value),
                    ..
                }) if name == MARK
                    && member_of(value).is_some_and(|member| people.contains(&member)) =>
                {
                    touched.names = true;
                }
                OpType::MarkBegin(MarkData {
                    value: ScalarValue::Null,
                    ..
                })
                | OpType::Delete => touched.removes = true,
                _ => {}
            }
            touched.removes |= !op.pred.is_empty();
            if touched.names && touched.removes {
                return Some(touched);
            }
        }
    }
    touched.names |= spelled
        .values()
        .any(|value| member_of(value).is_some_and(|member| people.contains(&member)));
    Some(touched)
}

/// Every mention in `doc`, in document order, each excerpt read for someone whose holdings nobody here
/// can tell ([`reading_nothing`]).
#[must_use]
pub fn in_resource(doc: &AutoCommit) -> Vec<Mention> {
    in_resource_for(doc, &reading_nothing)
}

/// Every mention in `doc`, in document order, each excerpt as `read` reads the references in it.
#[must_use]
pub fn in_resource_for(doc: &AutoCommit, read: Reader<'_>) -> Vec<Mention> {
    let mut walk = Walk {
        doc,
        found: Vec::new(),
        budget: MAX_OBJECTS,
        read,
        unshared: crate::i18n::I18nManager::lookup(tilo_reference::embed::UNSHARED_PHRASE, None),
    };
    walk.object(&ROOT, ObjType::Map, &mut Vec::new(), &Context::default());
    walk.found
}

#[derive(Clone, Default)]
struct Context {
    item: Option<(String, String)>,
    item_obj: Option<ObjId>,
}

struct Walk<'a> {
    doc: &'a AutoCommit,
    found: Vec<Mention>,
    budget: usize,
    read: Reader<'a>,
    /// What a reference to a resource the reader cannot read says, in their language.
    unshared: String,
}

impl Walk<'_> {
    fn object(&mut self, obj: &ObjId, kind: ObjType, path: &mut Vec<String>, cx: &Context) {
        if path.len() > MAX_DEPTH || self.budget == 0 {
            return;
        }
        self.budget -= 1;
        match kind {
            ObjType::Text => self.text(obj, path, cx),
            ObjType::Map | ObjType::Table => {
                self.stored_mark(obj, path, cx);
                for key in self.doc.keys(obj).collect::<Vec<_>>() {
                    if path.is_empty() && key == RESOURCE_META {
                        continue;
                    }
                    let Ok(Some((Value::Object(child_kind), child))) = self.doc.get(obj, &key)
                    else {
                        continue;
                    };
                    let entered =
                        (cx.item.is_none() && path.len() == 2 && key == ITEM_VALUE).then(|| {
                            Context {
                                item: Some((path[0].clone(), path[1].clone())),
                                item_obj: Some(child.clone()),
                            }
                        });
                    if let Some(item) = &entered {
                        self.addressed(item);
                    }
                    path.push(key);
                    self.object(&child, child_kind, path, entered.as_ref().unwrap_or(cx));
                    path.pop();
                }
            }
            ObjType::List => {
                for index in 0..self.doc.length(obj) {
                    let Ok(Some((Value::Object(child_kind), child))) = self.doc.get(obj, index)
                    else {
                        continue;
                    };
                    path.push(index.to_string());
                    self.object(&child, child_kind, path, cx);
                    path.pop();
                }
            }
        }
    }

    /// The mention marks of a text: each run once, its author the run's.
    fn text(&mut self, obj: &ObjId, path: &[String], cx: &Context) {
        let Ok(marks) = self.doc.marks(obj) else {
            return;
        };
        if !marks.iter().any(|mark| mark.name() == MARK) {
            return;
        }
        let text = self.doc.text(obj).unwrap_or_default();
        let references: Vec<(usize, usize, String)> = marks
            .iter()
            .filter_map(|mark| match (mark.name(), mark.value()) {
                (MARK, ScalarValue::Str(value)) if names_resource(value) => {
                    Some((mark.start, mark.end, value.to_string()))
                }
                _ => None,
            })
            .collect();
        for mark in &marks {
            let (MARK, ScalarValue::Str(value)) = (mark.name(), mark.value()) else {
                continue;
            };
            let Some(person) = member_of(value) else {
                continue;
            };
            let by = marks
                .iter()
                .filter(|other| other.name() == tilo_resource::text::AUTHOR_MARK)
                .filter(|other| (other.start..other.end).contains(&mark.start))
                .find_map(|other| match other.value() {
                    ScalarValue::Str(author) => MemberId::of_actor_bytes(author.as_bytes()),
                    _ => None,
                });
            let block = path
                .last()
                .filter(|key| key.parse::<usize>().is_err())
                .cloned();
            let excerpt = self.read_around(&text, &references, mark.start);
            self.push(person, by, block, excerpt, cx);
        }
    }

    /// An item said in a direct conversation is addressed to each member of it but its author: news
    /// for them as a mention of them is, its excerpt the item's first paragraph.
    fn addressed(&mut self, cx: &Context) {
        let Some(item) = cx.item_obj.clone() else {
            return;
        };
        let Some(members) = self
            .string(&item, "channel")
            .and_then(|channel| MemberId::direct_conversation_members(&channel))
        else {
            return;
        };
        let author = self.author(&item);
        let first = match self.doc.get(&item, "body") {
            Ok(Some((Value::Object(ObjType::List), body))) => self.doc.get(&body, 0).ok().flatten(),
            _ => None,
        };
        let excerpt = match first {
            Some((Value::Object(ObjType::Map), block)) => self.paragraph(&block, 0),
            _ => excerpt(&self.string(&item, "text").unwrap_or_default(), 0),
        };
        for member in members
            .into_iter()
            .filter(|member| author.as_ref() != Some(member))
        {
            self.push(member, None, None, excerpt.clone(), cx);
        }
    }

    /// The excerpt of `text` around character `at`, each of its `references` (a resource's run: start,
    /// end and the mark's value) read for the person told.
    fn read_around(&self, text: &str, references: &[(usize, usize, String)], at: usize) -> String {
        let runs = |until: usize| {
            references
                .iter()
                .filter(move |(_, end, _)| *end <= until)
                .map(|(start, end, value)| (*start, *end, value.as_str()))
        };
        let mut read = |reference: &tilo_reference::Embed| (self.read)(reference);
        let before: String = text.chars().take(at).collect();
        let at = tilo_reference::embed::read_text(&before, runs(at), &mut read, &self.unshared)
            .chars()
            .count();
        let read_text =
            tilo_reference::embed::read_text(text, runs(usize::MAX), &mut read, &self.unshared);
        excerpt(&read_text, at)
    }

    /// A mark span stored as a value (`{start, end, name, value}`), as a chat message's paragraphs keep
    /// their marks; its text is the paragraph's `text` beside the marks list.
    fn stored_mark(&mut self, obj: &ObjId, path: &[String], cx: &Context) {
        if self.string(obj, "name").as_deref() != Some(MARK) {
            return;
        }
        let Some(person) = self.mark_value(obj).and_then(|value| member_of(&value)) else {
            return;
        };
        let start = self.int(obj, "start").unwrap_or(0);
        let block = path
            .len()
            .checked_sub(2)
            .and_then(|up| self.object_at(&path[..up]));
        let excerpt = block.map_or_else(String::new, |block| self.paragraph(&block, start));
        self.push(person, None, None, excerpt, cx);
    }

    /// The excerpt around character `at` of a stored paragraph (`{text, marks}`), as a chat
    /// message keeps its paragraphs.
    fn paragraph(&self, block: &ObjId, at: usize) -> String {
        let references = match self.doc.get(block, "marks") {
            Ok(Some((Value::Object(_), marks))) => self.stored_references(&marks),
            _ => Vec::new(),
        };
        let text = self.string(block, "text").unwrap_or_default();
        self.read_around(&text, &references, at)
    }

    /// The references to resources among the stored mark spans of the list `marks`: each run's start,
    /// end and the mark's value.
    fn stored_references(&self, marks: &ObjId) -> Vec<(usize, usize, String)> {
        (0..self.doc.length(marks))
            .filter_map(|index| match self.doc.get(marks, index) {
                Ok(Some((Value::Object(ObjType::Map), mark))) => Some(mark),
                _ => None,
            })
            .filter(|mark| self.string(mark, "name").as_deref() == Some(MARK))
            .filter_map(|mark| {
                let value = self
                    .mark_value(&mark)
                    .filter(|value| names_resource(value))?;
                Some((self.int(&mark, "start")?, self.int(&mark, "end")?, value))
            })
            .collect()
    }

    fn push(
        &mut self,
        person: MemberId,
        by: Option<MemberId>,
        block: Option<String>,
        excerpt: String,
        cx: &Context,
    ) {
        let item = cx.item_obj.as_ref();
        let by = by.or_else(|| {
            item.and_then(|item| {
                self.author(item)
                    .or_else(|| MemberId::parsed_person(&self.string(item, "creator")?))
            })
        });
        let title = item.and_then(|item| {
            self.string(item, "title")
                .or_else(|| self.string(item, "name"))
                .filter(|title| !title.trim().is_empty())
        });
        let channel = item.and_then(|item| self.string(item, "channel"));
        self.found.push(Mention {
            person,
            by,
            item: cx.item.clone(),
            block,
            title,
            channel,
            excerpt,
        });
    }

    /// The member a stored mark names: a plain string, or the string of the `Str` variant it is
    /// stored as.
    fn mark_value(&self, obj: &ObjId) -> Option<String> {
        if let Some(value) = self.string(obj, "value") {
            return Some(value);
        }
        let Ok(Some((Value::Object(ObjType::Map), value))) = self.doc.get(obj, "value") else {
            return None;
        };
        self.string(&value, VARIANT_KEY)
            .is_some_and(|variant| variant.eq_ignore_ascii_case("str"))
            .then(|| {
                self.string(&value, STR_FIELD)
                    .or_else(|| self.string(&value, STR_FIELD_FLAT))
            })
            .flatten()
    }

    fn object_at(&self, path: &[String]) -> Option<ObjId> {
        let mut obj = ROOT;
        for key in path {
            let next = match key.parse::<usize>() {
                Ok(index) if self.doc.object_type(&obj).ok()? == ObjType::List => {
                    self.doc.get(&obj, index)
                }
                _ => self.doc.get(&obj, key.as_str()),
            };
            let Ok(Some((Value::Object(_), child))) = next else {
                return None;
            };
            obj = child;
        }
        Some(obj)
    }

    /// Who wrote the item `obj`: the member its `author` holds.
    fn author(&self, obj: &ObjId) -> Option<MemberId> {
        let author = self.string(obj, "author")?;
        serde_json::from_str(&author).ok()
    }

    fn string(&self, obj: &ObjId, key: &str) -> Option<String> {
        match self.doc.get(obj, key).ok()?? {
            (Value::Scalar(scalar), _) => match scalar.as_ref() {
                ScalarValue::Str(value) => Some(value.to_string()),
                _ => None,
            },
            (Value::Object(ObjType::Text), text) => self.doc.text(&text).ok(),
            (Value::Object(_), _) => None,
        }
    }

    fn int(&self, obj: &ObjId, key: &str) -> Option<usize> {
        match self.doc.get(obj, key).ok()?? {
            (Value::Scalar(scalar), _) => match scalar.as_ref() {
                ScalarValue::Int(value) => usize::try_from(*value).ok(),
                ScalarValue::Uint(value) => usize::try_from(*value).ok(),
                _ => None,
            },
            _ => None,
        }
    }
}

/// The text around character `at` of `text`, on one line.
fn excerpt(text: &str, at: usize) -> String {
    let around = tilo_search::excerpt_around(text, at, 0);
    around.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The record whose items the chain part `route` keeps ([`DocumentId::part`]), by route: the record
/// its chain is derived from, empty for the app's own record of the workspace, which an app's
/// chains are derived from. `None` for a route that names no part.
#[must_use]
pub fn record_of_part(route: &str) -> Option<&str> {
    let (role, chain) = route.strip_prefix("derived:")?.split_once('@')?;
    if !role.starts_with("part.") {
        return None;
    }
    let record = chain
        .strip_prefix("derived:")
        .and_then(|derived| derived.split_once('@'))
        .map_or(chain, |(_, of)| of);
    Some(
        if record == DocumentId::workspace_default().as_str()
            || record == DocumentId::package_primary().as_str()
        {
            ""
        } else {
            record
        },
    )
}

/// The name of channel `id` of a chat record, as `#name`.
#[must_use]
pub fn channel_name(tree: &AutoCommit, id: &str) -> Option<String> {
    let walk = Walk {
        doc: tree,
        found: Vec::new(),
        budget: 0,
        read: &reading_nothing,
        unshared: String::new(),
    };
    let channel = walk.object_at(&[CHANNELS.to_owned(), id.to_owned(), ITEM_VALUE.to_owned()])?;
    walk.string(&channel, "name")
        .filter(|name| !name.is_empty())
        .map(|name| format!("#{name}"))
}
