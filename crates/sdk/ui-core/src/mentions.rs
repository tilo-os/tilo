//! Mentions as a workspace fact: who a record's rich text names, read from
//! the record itself, and the one notice each mentioned reader gets.
//!
//! A mention is a run of rich text marked `mention` with the participant's
//! id, whichever app wrote it: a paragraph of a note (an Automerge text
//! mark, as `tilo-doc` keeps them) or a block of a chat message (a stored
//! mark span, as `tilo_document::ClipBlock` keeps them). [`in_record`] finds
//! every one in a record's document without the app's code, so whatever
//! holds the record — the person's desktop, on any device, whether or not
//! the app is open — derives the same facts from what it was sent. Only
//! the record's readers are sent it, so only they learn of a mention in it.
//!
//! Where a mention is is the item of the record it was written in, as the
//! app publishes it (`<kind>/<field>/<id>`, `tilo_resource::items`): a
//! note of a notebook, a message of a chat (whose messages are kept in
//! parts of a chain, but published as the chat's items). Opening that
//! resource is what opening the mention means.
//!
//! [`MentionInbox`] turns the facts into notices for one person: one per
//! place, never for their own mention, quietly taken for what they were
//! looking at when it arrived, withdrawn when the mention is edited away,
//! and never again once told — what it told it keeps, so reopening a record
//! or reloading tells nothing.

use std::collections::{BTreeMap, BTreeSet};

use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::record::RecordId;

/// The mark a mention's run carries, valued with the participant's id.
pub const MARK: &str = "mention";
/// The mark naming who wrote a run of text (`tilo_resource::text::AUTHOR_MARK`).
const AUTHOR_MARK: &str = "author";
/// The key a stored enum value keeps its variant under
/// (`tilo_resource::VARIANT_KEY`).
const VARIANT_KEY: &str = "$variant";
/// The key and value slot of an item of a stored collection
/// (`tilo_resource::Collection`): `{ <id>: { k: <order>, v: <item> } }`.
const ITEM_VALUE: &str = "v";
/// The record's identity map (`crate::record`), never content.
const RECORD_META: &str = "tilo:record";

/// Deepest a mention is looked for, and most objects read in one record:
/// the document is replicated input, not a request to walk it without end.
const MAX_DEPTH: usize = 32;
const MAX_OBJECTS: usize = 200_000;
/// Longest excerpt a notice carries, in characters.
const EXCERPT_CHARS: usize = 160;

/// One mention in a record: who is named, by whom, and where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mention {
    /// The participant named.
    pub person: String,
    /// Who wrote it, when the record says: the run's author mark, or the
    /// item's author.
    pub by: Option<String>,
    /// The item it was written in, as the record's top-level collection
    /// keeps it: its field and id. `None` for a mention outside any item.
    pub item: Option<(String, String)>,
    /// The paragraph it is in (a note's block), when its text has an id.
    pub block: Option<String>,
    /// What the item is called: its `title` or `name`.
    pub title: Option<String>,
    /// The channel the item was said in, by id, for an item of a chat.
    pub channel: Option<String>,
    /// The text around it.
    pub excerpt: String,
}

/// Every mention in `doc`, in document order.
#[must_use]
pub fn in_record(doc: &AutoCommit) -> Vec<Mention> {
    let mut walk = Walk {
        doc,
        found: Vec::new(),
        budget: MAX_OBJECTS,
    };
    walk.object(&ROOT, ObjType::Map, &mut Vec::new(), &Context::default());
    walk.found
}

/// What encloses the object being read.
#[derive(Clone, Default)]
struct Context {
    item: Option<(String, String)>,
    item_obj: Option<ObjId>,
}

struct Walk<'a> {
    doc: &'a AutoCommit,
    found: Vec<Mention>,
    budget: usize,
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
                    // An item of a top-level collection: `<field>/<id>/v`.
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
        for mark in &marks {
            let (MARK, ScalarValue::Str(person)) = (mark.name(), mark.value()) else {
                continue;
            };
            if person.is_empty() {
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
            self.push(
                person.to_string(),
                by,
                block,
                excerpt(&text, mark.start),
                cx,
            );
        }
    }

    /// A mark span stored as a value (`{start, end, name, value}`), as a
    /// chat message's paragraphs keep their marks; its text is the
    /// paragraph's `text` beside the marks list.
    fn stored_mark(&mut self, obj: &ObjId, path: &[String], cx: &Context) {
        if self.string(obj, "name").as_deref() != Some(MARK) {
            return;
        }
        let Some(person) = self.mark_value(obj).filter(|person| !person.is_empty()) else {
            return;
        };
        let start = self.int(obj, "start").unwrap_or(0);
        // `…/<block>/marks/<n>`: the paragraph is two up.
        let text = (path.len() >= 2)
            .then(|| self.object_at(&path[..path.len() - 2]))
            .flatten()
            .and_then(|block| self.string(&block, "text"))
            .unwrap_or_default();
        let by = cx
            .item_obj
            .as_ref()
            .and_then(|item| self.string(item, "author"));
        self.push(person, by, None, excerpt(&text, start), cx);
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

    /// The participant a stored mark names: a plain string, or the string
    /// of the `Str` variant it is stored as.
    fn mark_value(&self, obj: &ObjId) -> Option<String> {
        if let Some(value) = self.string(obj, "value") {
            return Some(value);
        }
        let Ok(Some((Value::Object(ObjType::Map), value))) = self.doc.get(obj, "value") else {
            return None;
        };
        self.string(&value, VARIANT_KEY)
            .is_some_and(|variant| variant.eq_ignore_ascii_case("str"))
            .then(|| self.string(&value, "0"))
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

    /// A field read as a string: a scalar, or a text object.
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
    let chars: Vec<char> = text.chars().collect();
    let start = at.saturating_sub(EXCERPT_CHARS / 2).min(chars.len());
    let start = if start == 0 {
        0
    } else {
        // Begin at a word.
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

/// Where a mention opens: the app's resource, in its record.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Place {
    /// The app keeping the record, by package id.
    pub app: String,
    /// The record, as its route names it: empty for the app's own record of
    /// the workspace.
    pub record: String,
    /// The resource whose `open` shows the mention (`<kind>/<field>/<id>`),
    /// or empty for the record as a whole.
    pub resource: String,
}

/// The activation scheme a mention's notice carries.
pub const SCHEME: &str = "tilo-mention";

impl Place {
    /// The notice's activation: `tilo-mention:<app>?<record>#<resource>`,
    /// each part percent-encoded.
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

    /// The tag a notice of this place for `person` goes by: the same on
    /// every device and window, so the host shows it once.
    #[must_use]
    pub fn tag(&self, person: &str) -> String {
        format!(
            "mention:{}/{}/{}/{}",
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

/// One notice for the person: what the host shows, and where it opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub tag: String,
    pub title: String,
    pub body: String,
    pub place: Place,
}

impl Notice {
    /// The host notification this notice is.
    #[must_use]
    pub fn notification(&self) -> tilo_host::notify::Notification {
        tilo_host::notify::Notification::new(&self.tag, &self.title, &self.body)
            .with_activation(self.place.activation())
    }
}

/// What one look at a record changed: notices to show, and the tags of
/// those whose mention is gone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Told {
    pub show: Vec<Notice>,
    pub withdraw: Vec<String>,
}

/// A record as an inbox looks at it.
pub struct Look<'a> {
    /// The app keeping it.
    pub app: &'a str,
    /// The record, as its route names it: empty for the app's own record
    /// of the workspace.
    pub record: &'a str,
    /// The kind of the app's records, for a record (or chain) whose
    /// document names none, as the app's own record of the workspace does.
    pub kind: &'a str,
    /// Its document.
    pub doc: &'a AutoCommit,
    /// The record its places belong to, when it is a part of another (a
    /// chat's part, published as the chat's items): its document.
    pub tree: Option<&'a AutoCommit>,
    /// Take what it holds as seen: the person is looking at it, or it is
    /// what the room held before this device first followed it.
    pub quiet: bool,
}

/// The notices of one person's mentions, on one device: what was told is
/// kept ([`Self::told_state`]) so nothing is told twice.
#[derive(Default)]
pub struct MentionInbox {
    /// Every id the person goes by.
    me: BTreeSet<String>,
    /// Per record (`app` and route), the tags told and still standing.
    told: BTreeMap<(String, String), BTreeSet<String>>,
}

impl MentionInbox {
    /// The inbox of the person going by `ids`.
    #[must_use]
    pub fn new(ids: impl IntoIterator<Item = String>) -> Self {
        Self {
            me: ids.into_iter().filter(|id| !id.is_empty()).collect(),
            told: BTreeMap::new(),
        }
    }

    /// Whether this device ever looked at `app`'s record `route`.
    #[must_use]
    pub fn knows(&self, app: &str, route: &str) -> bool {
        self.told.contains_key(&(app.to_owned(), route.to_owned()))
    }

    /// Look at a record again: new mentions of the person are told, ones no
    /// longer there withdrawn. `name` gives a participant's name.
    pub fn look(&mut self, look: &Look<'_>, name: &dyn Fn(&str) -> String) -> Told {
        let (record, kind) = place_record(look);
        let route = look.record.to_owned();
        let mut standing: BTreeMap<String, Notice> = BTreeMap::new();
        for mention in in_record(look.doc) {
            if !self.me.contains(&mention.person)
                || mention.by.as_ref().is_some_and(|by| self.me.contains(by))
            {
                continue;
            }
            // Without a kind the item has no address: the record opens.
            let resource = mention
                .item
                .as_ref()
                .filter(|_| !kind.is_empty())
                .map_or_else(String::new, |(field, id)| format!("{kind}/{field}/{id}"));
            let place = Place {
                app: look.app.to_owned(),
                record: record.clone(),
                resource,
            };
            let tag = place.tag(&mention.person);
            if standing.contains_key(&tag) {
                continue;
            }
            let who = mention.by.as_deref().map_or_else(
                || crate::i18n::widget_format("mention-notice-someone", &[]),
                name,
            );
            let channel = mention
                .channel
                .as_deref()
                .and_then(|channel| look.tree.and_then(|tree| channel_name(tree, channel)));
            let title = match (channel, &mention.title) {
                (Some(channel), _) => crate::i18n::widget_format(
                    "mention-notice-channel",
                    &[("name", who.into()), ("channel", channel.into())],
                ),
                (None, Some(title)) => crate::i18n::widget_format(
                    "mention-notice-title",
                    &[("name", who.into()), ("place", title.clone().into())],
                ),
                (None, None) => {
                    crate::i18n::widget_format("mention-notice", &[("name", who.into())])
                }
            };
            standing.insert(
                tag.clone(),
                Notice {
                    tag,
                    title,
                    body: mention.excerpt,
                    place,
                },
            );
        }
        let told = self.told.entry((look.app.to_owned(), route)).or_default();
        let withdraw: Vec<String> = told
            .iter()
            .filter(|tag| !standing.contains_key(*tag))
            .cloned()
            .collect();
        for tag in &withdraw {
            told.remove(tag);
        }
        let show = standing
            .into_iter()
            .filter(|(tag, _)| told.insert(tag.clone()))
            .map(|(_, notice)| notice)
            .filter(|_| !look.quiet)
            .collect();
        Told { show, withdraw }
    }

    /// What this inbox told, to keep: one line per record, its app and
    /// route then each tag, tab-separated.
    #[must_use]
    pub fn told_state(&self) -> String {
        let mut out = String::new();
        for ((app, route), tags) in &self.told {
            out.push_str(&encode(app));
            out.push('\t');
            out.push_str(&encode(route));
            for tag in tags {
                out.push('\t');
                out.push_str(&encode(tag));
            }
            out.push('\n');
        }
        out
    }

    /// Take up what [`Self::told_state`] kept.
    pub fn restore(&mut self, state: &str) {
        for line in state.lines() {
            let mut parts = line.split('\t').filter_map(decode);
            let (Some(app), Some(route)) = (parts.next(), parts.next()) else {
                continue;
            };
            self.told.entry((app, route)).or_default().extend(parts);
        }
    }
}

/// The record a mention in the record `look` names opens in, by route,
/// and that record's kind: the chain a part belongs to, else the record
/// itself.
fn place_record(look: &Look<'_>) -> (String, String) {
    let kind_of = |doc: Option<&AutoCommit>| {
        doc.and_then(crate::record::identity).map_or_else(
            || look.kind.to_owned(),
            |identity| identity.kind.as_str().to_owned(),
        )
    };
    match chain_of(look.record) {
        Some(chain) => (chain.to_owned(), kind_of(look.tree)),
        None => (look.record.to_owned(), kind_of(Some(look.doc))),
    }
}

/// The route of the chain whose part the record `route` names
/// ([`RecordId::part`]): empty for a part of the app's own record.
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
fn channel_name(tree: &AutoCommit, id: &str) -> Option<String> {
    let walk = Walk {
        doc: tree,
        found: Vec::new(),
        budget: 0,
    };
    let channel = walk.object_at(&["channels".to_owned(), id.to_owned(), ITEM_VALUE.to_owned()])?;
    walk.string(&channel, "name")
        .filter(|name| !name.is_empty())
        .map(|name| format!("#{name}"))
}

#[cfg(test)]
#[path = "mentions_tests.rs"]
mod tests;
