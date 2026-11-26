//! Mentions as a workspace fact: who a record's rich text names, read from the record itself. A mention
//! is a run of rich text marked `mention` with the member's id, whichever app wrote it: an Automerge
//! text mark, as `tilo-doc` keeps the rich text of a note, a chat message and every tilolang
//! `RichText`.

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use tilo_actions::MemberId;

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
/// The key and value slot of an item of a stored collection (`tilo_resource::Collection`): `{ <id>: {
/// k: <order>, v: <item> } }`.
const ITEM_VALUE: &str = "v";
/// The record's identity map (`crate::resource`), never content.
const RESOURCE_META: &str = "tilo:record";

/// Deepest a mention is looked for, and most objects read in one record: the document is replicated
/// input, not a request to walk it without end.
const MAX_DEPTH: usize = 32;
const MAX_OBJECTS: usize = 200_000;

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
    use automerge::legacy::{MarkData, OpType};
    let mut touched = Touched::default();
    for change in automerge::Automerge::parse_changes(changes).ok()? {
        let change = change.decode();
        for op in change.operations {
            // A person's id is written in many places that are no mention of them (who wrote a
            // change, who sits where): only a mention's mark can make one.
            match &op.action {
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
        read_by: read_by(doc),
    };
    walk.object(&ROOT, ObjType::Map, &mut Vec::new(), &Context::default());
    walk.found
}

/// The changes of `doc` only some members read ([`tilo_actions::READERS_FACET`]): each one's
/// author, the counters of the operations it made, and how it names those who alone read it.
type ReadBy = Vec<(
    automerge::ActorId,
    std::ops::RangeInclusive<u64>,
    Vec<String>,
)>;

fn read_by(doc: &AutoCommit) -> ReadBy {
    let mut entries = tilo_actions::EntryReader::default();
    doc.committed_document()
        .get_changes_meta(&[])
        .iter()
        .filter_map(|meta| {
            let context = entries.read(&meta.actor, meta.message.as_deref())?;
            let named = tilo_actions::contract::readers_named(&context.facets)?;
            Some((
                meta.actor.as_ref().clone(),
                meta.start_op..=meta.max_op,
                named,
            ))
        })
        .collect()
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
    /// The changes only some members read: a mention in a text one of them made is news to
    /// those members alone, as its history is theirs alone.
    read_by: ReadBy,
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
            if !self.reads_text(obj, &person) {
                continue;
            }
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

    /// Whether `person` reads the text `obj`: every member reads a text no change only some
    /// members read made ([`Walk::read_by`]).
    fn reads_text(&self, obj: &ObjId, person: &MemberId) -> bool {
        let ObjId::Id(counter, actor, _) = obj else {
            return true;
        };
        self.read_by
            .iter()
            .find(|(by, ops, _)| by == actor && ops.contains(counter))
            .is_none_or(|(_, _, named)| {
                named.contains(&tilo_actions::contract::reader_name(person))
            })
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
        self.found.push(Mention {
            person,
            by,
            item: cx.item.clone(),
            block,
            title,
            excerpt,
        });
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
}

/// The text around character `at` of `text`, on one line.
fn excerpt(text: &str, at: usize) -> String {
    let around = tilo_search::excerpt_around(text, at, 0);
    around.split_whitespace().collect::<Vec<_>>().join(" ")
}
