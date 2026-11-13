//! Mentions as a workspace fact: who a record's rich text names, read from the record itself. A mention
//! is a run of rich text marked `mention` with the participant's id, whichever app wrote it: a
//! paragraph of a note (an Automerge text mark, as `tilo-doc` keeps them) or a block of a chat message
//! (a stored mark span, as `tilo_document::ClipBlock` keeps them).

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::record::RecordId;

/// The mark a mention's run carries, valued with the participant's id.
pub const MARK: &str = "mention";

/// Whether a `mention` mark's value names a resource rather than a person, in any form it was written
/// in (`tilo_reference::embed`).
#[must_use]
pub fn names_resource(value: &str) -> bool {
    tilo_reference::embed::names_resource(value)
}
const AUTHOR_MARK: &str = "author";
/// The key a stored enum value keeps its variant under (`tilo_resource::VARIANT_KEY`).
const VARIANT_KEY: &str = "$variant";
/// The key the `Str` variant of a stored mark value keeps its string under, and the bare key an older
/// layout kept it under.
const STR_FIELD: &str = "$field/str/0";
const STR_FIELD_FLAT: &str = "0";
/// The key and value slot of an item of a stored collection (`tilo_resource::Collection`): `{ <id>: {
/// k: <order>, v: <item> } }`.
const ITEM_VALUE: &str = "v";
/// The record's identity map (`crate::record`), never content.
const RECORD_META: &str = "tilo:record";

/// Deepest a mention is looked for, and most objects read in one record: the document is replicated
/// input, not a request to walk it without end.
const MAX_DEPTH: usize = 32;
const MAX_OBJECTS: usize = 200_000;
const EXCERPT_CHARS: usize = 160;

/// One mention in a record: who is named, by whom, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
    /// The participant named.
    pub person: String,
    /// Who wrote it, when the record says: the run's author mark, or the item's author.
    pub by: Option<String>,
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
    if reference.as_person().is_some() {
        tilo_reference::embed::Reading::AsWritten
    } else {
        tilo_reference::embed::Reading::Unshared
    }
}

/// Every mention in `doc`, in document order, each excerpt read for someone whose holdings nobody here
/// can tell ([`reading_nothing`]).
#[must_use]
pub fn in_record(doc: &AutoCommit) -> Vec<Mention> {
    in_record_for(doc, &reading_nothing)
}

/// Every mention in `doc`, in document order, each excerpt as `read` reads the references in it.
#[must_use]
pub fn in_record_for(doc: &AutoCommit, read: Reader<'_>) -> Vec<Mention> {
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
                    if path.is_empty() && key == RECORD_META {
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
            let (MARK, ScalarValue::Str(person)) = (mark.name(), mark.value()) else {
                continue;
            };
            if person.is_empty() || names_resource(person) {
                continue;
            }
            let by = marks
                .iter()
                .filter(|other| other.name() == AUTHOR_MARK)
                .filter(|other| (other.start..other.end).contains(&mark.start))
                .find_map(|other| match other.value() {
                    ScalarValue::Str(author) if !author.is_empty() => Some(author.to_string()),
                    _ => None,
                });
            let block = path
                .last()
                .filter(|key| key.parse::<usize>().is_err())
                .cloned();
            let excerpt = self.read_around(&text, &references, mark.start);
            self.push(person.to_string(), by, block, excerpt, cx);
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
        let Some(person) = self
            .mark_value(obj)
            .filter(|person| !person.is_empty() && !names_resource(person))
        else {
            return;
        };
        let start = self.int(obj, "start").unwrap_or(0);
        let text = (path.len() >= 2)
            .then(|| self.object_at(&path[..path.len() - 2]))
            .flatten()
            .and_then(|block| self.string(&block, "text"))
            .unwrap_or_default();
        let references = path
            .split_last()
            .and_then(|(_, up)| self.object_at(up))
            .map(|marks| self.stored_references(&marks))
            .unwrap_or_default();
        let by = cx
            .item_obj
            .as_ref()
            .and_then(|item| self.string(item, "author"));
        let excerpt = self.read_around(&text, &references, start);
        self.push(person, by, None, excerpt, cx);
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
        person: String,
        by: Option<String>,
        block: Option<String>,
        excerpt: String,
        cx: &Context,
    ) {
        let item = cx.item_obj.as_ref();
        let by = by.or_else(|| {
            item.and_then(|item| {
                self.string(item, "author")
                    .or_else(|| self.string(item, "creator"))
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

    /// The participant a stored mark names: a plain string, or the string of the `Str` variant it is
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

fn excerpt(text: &str, at: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = at.saturating_sub(EXCERPT_CHARS / 2).min(chars.len());
    let start = if start == 0 {
        0
    } else {
        chars[start..]
            .iter()
            .position(|c| c.is_whitespace())
            .map_or(start, |offset| start + offset + 1)
            .min(at.min(chars.len()))
    };
    let line: String = chars[start..]
        .iter()
        .take(EXCERPT_CHARS)
        .map(|c| if c.is_control() { ' ' } else { *c })
        .collect();
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let more = chars.len() > start + EXCERPT_CHARS;
    match (start > 0, more) {
        (false, false) => line,
        (true, false) => format!("…{line}"),
        (false, true) => format!("{line}…"),
        (true, true) => format!("…{line}…"),
    }
}

/// Where choosing a notice lands: the app keeping a record, the record, and the item in it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Place {
    /// The app keeping the record, by package id.
    pub app: String,
    /// The record, as its route names it: empty for the app's own record of the workspace.
    pub record: String,
    /// The resource whose `open` shows the item (`<kind>/<field>/<id>`), or empty for the record as a
    /// whole.
    pub resource: String,
}

/// The activation scheme a notice's [`Place`] is carried by: what a host that reports clicks hands back
/// when the person chooses it.
pub const SCHEME: &str = "tilo-mention";

/// What the tag of a mention's notice starts with ([`Place::tag`]): how a host showing them knows which
/// of its notifications are theirs.
pub const TAGS: &str = "mention:";

impl Place {
    /// The activation URI a host carries the place by: `tilo-mention:<app>?<record>#<resource>`, each
    /// part percent-encoded.
    #[must_use]
    pub fn activation(&self) -> String {
        format!(
            "{SCHEME}:{}?{}#{}",
            encode(&self.app),
            encode(&self.record),
            encode(&self.resource)
        )
    }

    /// The place an [`Self::activation`] names.
    #[must_use]
    pub fn from_activation(uri: &str) -> Option<Self> {
        let rest = uri.strip_prefix(SCHEME)?.strip_prefix(':')?;
        let (app, rest) = rest.split_once('?')?;
        let (record, resource) = rest.split_once('#')?;
        let place = Self {
            app: decode(app)?,
            record: decode(record)?,
            resource: decode(resource)?,
        };
        (!place.app.is_empty()).then_some(place)
    }

    /// The tag a notice of this place for `person` goes by: the same on every device and window, so the
    /// host shows it once.
    #[must_use]
    pub fn tag(&self, person: &str) -> String {
        format!(
            "{TAGS}{}/{}/{}/{}",
            self.app, self.record, self.resource, person
        )
    }
}

fn encode(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    for byte in part.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/' | b':' | b'@')
        {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode(part: &str) -> Option<String> {
    let bytes = part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = part.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// The route of the chain whose part the record `route` names ([`RecordId::part`]): empty for a part of
/// the app's own record.
#[must_use]
pub fn chain_of(route: &str) -> Option<&str> {
    let (role, chain) = route.strip_prefix("derived:")?.split_once('@')?;
    role.starts_with("part.")
        .then_some(if chain == RecordId::package_primary().as_str() {
            ""
        } else {
            chain
        })
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
    let channel = walk.object_at(&["channels".to_owned(), id.to_owned(), ITEM_VALUE.to_owned()])?;
    walk.string(&channel, "name")
        .filter(|name| !name.is_empty())
        .map(|name| format!("#{name}"))
}
