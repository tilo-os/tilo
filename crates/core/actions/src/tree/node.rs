//! The directory model an address is read through: one resource as a caller
//! names it, and the resources and actions one listing holds under a parent.

use std::collections::{HashMap, HashSet};

use crate::ActionDescriptor;
use crate::api::ActionAddress;
use crate::remote::CatalogueRow;
use serde_json::{Value, json};

use super::names::{bound_key, path_name};

impl Node {
    /// This resource as a row of a listing under `parent`: the name its line
    /// prints, the actions it offers by their ids, and whether a read answers
    /// it as a resource of its own (`reads`), or it is only a place inside
    /// what it is listed under (an option, a choice): those are part of their
    /// parent's read and name no resource, so a walk that reads resources one
    /// by one reads only the rows that say so.
    pub fn listing_row(&self, parent: &str, name: &str, resource_type: &str, reads: bool) -> Value {
        json!({
            "path": format!("{parent}/{name}"),
            "name": name,
            "id": self.id,
            "key": self.key,
            "label": self.label,
            "kind": resource_type,
            "has_children": self.container,
            "reads": reads,
            "actions": self
                .listed_actions()
                .map(|action| action.address.action.clone())
                .collect::<Vec<_>>(),
        })
    }

    /// The actions a listing shows it offering: what acts on an item it
    /// holds is listed on that item, bound to it, and found here only by a
    /// call that names it.
    pub fn listed_actions(&self) -> impl Iterator<Item = &ActionDescriptor> {
        self.actions
            .iter()
            .filter(|action| !action.contract.needs_an_item())
    }
}

/// The names `rows` share, lowercased, each named among `siblings`
/// ([`Node::name`]): a line named by one of them is told apart by its id
/// beside it. Two titles used once each per parent are told apart by their
/// places instead, so they are not shared.
pub fn shared_names(rows: &[Node], siblings: &Names<'_>) -> std::collections::HashSet<String> {
    let mut seen = std::collections::HashSet::new();
    rows.iter()
        .map(|row| row.name_in(siblings).trim().to_lowercase())
        .filter(|name| !seen.insert(name.clone()))
        .collect()
}

/// Siblings indexed by their titles: what tells whether a name sets one of
/// them apart ([`Node::name_in`]) without reading every other sibling, so a
/// listing of hundreds names each of its rows at the cost of one.
pub struct Names<'a> {
    siblings: Vec<&'a Node>,
    /// Each sibling's index by its title, trimmed and lowercased, in order.
    titled: HashMap<String, Vec<usize>>,
}

impl<'a> Names<'a> {
    pub fn of(siblings: impl IntoIterator<Item = &'a Node>) -> Self {
        let siblings: Vec<&'a Node> = siblings.into_iter().collect();
        let mut titled: HashMap<String, Vec<usize>> = HashMap::new();
        for (at, node) in siblings.iter().enumerate() {
            titled
                .entry(node.label.trim().to_ascii_lowercase())
                .or_default()
                .push(at);
        }
        Self { siblings, titled }
    }

    /// The siblings titled `title`, trimmed, whatever its case.
    fn titled(&self, title: &str) -> impl Iterator<Item = &'a Node> + '_ {
        self.titled
            .get(&title.trim().to_ascii_lowercase())
            .into_iter()
            .flatten()
            .filter_map(|at| self.siblings.get(*at).copied())
    }

    /// The siblings `name` names first ([`Node::answers_to`] ranks them `0`):
    /// the one a title names, or the one whose place among those of its
    /// title `name` gives (`TODO 2`). Any other answers to it after them, if
    /// at all.
    fn first_named(&self, name: &str) -> Vec<&'a Node> {
        let mut named: Vec<&'a Node> = Vec::new();
        let mut by_place: Option<&str> = None;
        if let Some((base, place)) = name.rsplit_once(' ')
            && !base.trim().is_empty()
            && place.chars().all(|character| character.is_ascii_digit())
            && let Ok(place) = place.parse::<usize>()
        {
            by_place = Some(base.trim());
            named.extend(
                self.titled(base)
                    .filter(|node| place == node.ordinal && place >= 2),
            );
        }
        // A name that is the title itself names the first of that title; a
        // title its place names above answers only to that place.
        named.extend(self.titled(name).filter(|node| {
            node.ordinal <= 1
                && node.label.trim().eq_ignore_ascii_case(name)
                && by_place.is_none_or(|base| !node.label.trim().eq_ignore_ascii_case(base))
        }));
        named
    }
}

/// One resource of an app's directory, as an agent names it.
#[derive(Clone, Debug)]
pub struct Node {
    /// The broker's id: never shown, only sent back.
    pub id: String,
    pub resource_type: String,
    pub label: String,
    /// The app's own stable name for it: its semantic key, else the logical
    /// id inside a bound id, else the id.
    pub key: String,
    pub parent: String,
    /// Its place among the siblings that share its title, from 1: what tells
    /// two pages titled alike apart in a path (`TODO`, `TODO 2`).
    pub ordinal: usize,
    pub container: bool,
    pub actions: Vec<ActionDescriptor>,
}

impl Node {
    /// The part of the key that tells two resources of one type apart
    /// (`notes.page:3fa2…` → `3fa2…`).
    pub fn token(&self) -> &str {
        self.key
            .rsplit([':', '/'])
            .find(|part| !part.is_empty())
            .unwrap_or(&self.key)
    }

    /// Whether `other`'s key names this one's distinguishing part.
    pub fn owns(&self, other: &Node) -> bool {
        let token = self.token();
        token.len() >= 8 && other.key != self.key && other.key.contains(token)
    }

    /// Whether `other` is this resource's own and not one of its children.
    pub fn beside(&self, other: &Node) -> bool {
        self.owns(other) && !other.key.starts_with(&self.key)
    }

    /// Whether this resource is one of `other`'s: it sits under the other's id.
    pub fn within(&self, other: &Node) -> bool {
        matches!(self.id.strip_prefix(other.id.as_str()), Some(rest) if rest.starts_with('/'))
    }

    /// How well `name` names this node: 0 its label, 1 its key (whole or last
    /// part), 2 its resource type, 3 the start of either; `None` when it does not.
    pub fn answers_to(&self, name: &str) -> Option<u8> {
        let same = |text: &str| text.trim().eq_ignore_ascii_case(name);
        // A name that is a title and its place among the siblings of that
        // title (`TODO 2`) names the one whose place it is, and no other:
        // the title alone names the first.
        if let Some((base, place)) = name.rsplit_once(' ')
            && !base.trim().is_empty()
            && place.chars().all(|character| character.is_ascii_digit())
            && let Ok(place) = place.parse::<usize>()
            && self.label.trim().eq_ignore_ascii_case(base.trim())
        {
            return (place == self.ordinal && place >= 2).then_some(0);
        }
        if same(self.label.trim()) && self.ordinal > 1 {
            return None;
        }
        let tail = |text: &str| {
            text.rsplit(['.', ':', '/'])
                .next()
                .is_some_and(|tail| tail.eq_ignore_ascii_case(name))
        };
        if same(&self.label) {
            return Some(0);
        }
        if self.names_id(name) {
            return Some(1);
        }
        if same(&self.resource_type) || tail(&self.resource_type) {
            return Some(2);
        }
        let lower = name.to_lowercase();
        let starts = |text: &str| text.to_lowercase().starts_with(&lower);
        (lower.chars().count() >= 3
            && (starts(&self.label) || (lower.len() >= 6 && starts(self.token()))))
        .then_some(3)
    }

    /// Whether `name` is this resource's own id, or the item part of it
    /// (`<kind>/<field>/<item>`): an id names the resource before any label
    /// does, whatever the label says.
    pub fn names_id(&self, name: &str) -> bool {
        let same = |text: &str| text.trim().eq_ignore_ascii_case(name);
        let key_tail = self.key.rsplit('/').next().unwrap_or_default();
        same(&self.key) || same(&self.id) || same(key_tail) || same(self.token())
    }

    /// The name that goes in a path: its label when it tells it apart from
    /// `siblings`, else its key's last part (`body`), else its key.
    pub fn name(&self, siblings: &[Node]) -> String {
        self.name_in(&Names::of(siblings))
    }

    /// [`Self::name`] among siblings already indexed by their titles.
    pub fn name_in(&self, siblings: &Names<'_>) -> String {
        // Neither what belongs to this resource nor what it is a part of
        // competes with it: a note titled like its notebook keeps its title.
        let apart = |name: &str| {
            !name.trim().is_empty()
                && siblings
                    .first_named(name)
                    .into_iter()
                    .filter(|other| other.id != self.id)
                    .all(|other| self.owns(other) || self.within(other))
        };
        let label = self.label.trim();
        // A page and the body published beside it share a title but not a
        // level: only what sits beside this one takes its title.
        let twin = !label.is_empty()
            && siblings.titled(label).any(|other| {
                other.id != self.id
                    && other.parent == self.parent
                    && other.label.trim().eq_ignore_ascii_case(label)
            });
        if !label.is_empty() && (apart(label) || twin) {
            // The one a title names first keeps it; the others wear their
            // place among them, which the path accepts back.
            if twin && self.ordinal > 1 {
                return path_name(&format!("{label} {}", self.ordinal));
            }
            return path_name(label);
        }
        let tail = self.key.rsplit('/').next().unwrap_or_default();
        if tail != self.key && apart(tail) {
            return path_name(tail);
        }
        path_name(&self.key)
    }
}

/// The resources and actions one directory listing holds under `parent`.
#[derive(Clone, Default)]
pub struct Level {
    /// Actions of the parent itself.
    pub own: Vec<ActionDescriptor>,
    pub children: Vec<Node>,
    /// The directory revision it was read at.
    pub revision: String,
    /// The pages it was read from ([`LevelPages`]).
    pub pages: std::sync::Arc<LevelPages>,
}

/// The level under `parent` that `rows`, one listing's rows, hold: its own
/// actions and its children, each with the actions placed on it and its place
/// among the resources beside it that share its title.
#[must_use]
pub fn level_of(rows: &[CatalogueRow], parent: &str) -> Level {
    let mut level = Level::default();
    let mut at: HashMap<&str, usize> = HashMap::new();
    // The actions a page of compact rows described whole, by provider, kind and id: each row
    // after names them by id ([`tilo_actions::remote::compact`]), and its node takes each
    // placed on it straight from here, never a row per action first.
    let mut described: HashMap<(&str, &str, &str), &ActionDescriptor> = HashMap::new();
    let mut placed: Vec<ActionDescriptor> = Vec::new();
    for row in rows {
        placed.clear();
        let mut identity = None;
        match &row.offered {
            Some(offered) => {
                for action in &offered.described {
                    described.insert(
                        (
                            row.provider.as_str(),
                            action.resource_kind.as_str(),
                            action.address.action.as_str(),
                        ),
                        action,
                    );
                }
                identity = offered.semantic_identity.as_ref();
                for id in &offered.actions {
                    let key = (
                        row.provider.as_str(),
                        row.resource.kind.as_str(),
                        id.as_str(),
                    );
                    let Some(like) = described.get(&key) else {
                        continue;
                    };
                    let mut action = (*like).clone();
                    action.address = ActionAddress::new(row.resource.id.as_str(), id);
                    action.resource_label.clone_from(&row.resource.label);
                    action
                        .semantic_identity
                        .clone_from(&offered.semantic_identity);
                    placed.push(action);
                }
            }
            None => {
                if let Some(action) = &row.action {
                    identity = action.semantic_identity.as_ref();
                    placed.push(action.clone());
                }
            }
        }
        if !parent.is_empty() && row.resource.id == parent {
            level.own.append(&mut placed);
            continue;
        }
        let key = identity
            .map(|identity| identity.key.clone())
            .or_else(|| bound_key(&row.resource.id));
        let index = *at.entry(row.resource.id.as_str()).or_insert_with(|| {
            level.children.push(Node {
                id: row.resource.id.clone(),
                resource_type: row.resource.kind.clone(),
                label: row.resource.label.clone(),
                key: key.clone().unwrap_or_else(|| row.resource.id.clone()),
                parent: row.parent_resource.clone(),
                ordinal: 1,
                container: false,
                actions: Vec::new(),
            });
            level.children.len().saturating_sub(1)
        });
        if let Some(node) = level.children.get_mut(index) {
            node.container |= row.has_children;
            if let Some(key) = key.filter(|_| node.key == node.id) {
                node.key = key;
            }
            node.actions.append(&mut placed);
        }
    }
    // Where each item stands among the siblings sharing its title: what a
    // path names it by when its title alone would name several.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for node in &mut level.children {
        let title = node.label.trim().to_lowercase();
        let count = seen.entry(title).or_insert(0);
        *count = count.saturating_add(1);
        node.ordinal = *count;
    }
    level
}

impl Node {
    /// [`Self::name`] as a search result names it: a name that would be
    /// written with escapes (a file's title, `files/legal/contract.txt`)
    /// gives way to its key's last part, which an address takes as well, so
    /// the path reads plainly and the title is shown beside it whole.
    pub fn found_name(&self, among: &[Node]) -> String {
        let name = self.name(among);
        if !name.contains('\\') {
            return name;
        }
        let tail = self.key.rsplit('/').next().unwrap_or_default();
        for plain in [tail, self.token()] {
            if !plain.is_empty() && !plain.contains(['/', '#', '\\']) {
                return plain.to_owned();
            }
        }
        name
    }

    /// Whether `address` names this resource whole, as an action answers it
    /// (`tasks.tracker/issues/7`): the id it is published under, or its key.
    pub fn address_is(&self, address: &str) -> bool {
        self.id == address || self.key == address
    }
}

/// The rows a [`Level`] was read from, where each of its pages after the
/// first started, and, when the read stopped at its bound before the last
/// page, where the directory goes on: a listing paging past what was read
/// reads on from there (the walk that reads a level on), and the next page of
/// a listing starts at the page its first resource was read in
/// ([`LsCursor`]).
#[derive(Clone, Default)]
pub struct LevelPages {
    pub rows: Vec<CatalogueRow>,
    pub marks: Vec<PageMark>,
    pub rest: Option<PageMark>,
}

/// Where one page of a directory starts: the row it starts at among those
/// read, and the cursor and revision that ask for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageMark {
    pub row: usize,
    pub cursor: Vec<u8>,
    pub revision: String,
}

impl Level {
    /// Whether the directory goes on past what was read.
    pub fn unread(&self) -> bool {
        self.pages.rest.is_some()
    }

    /// The resources of the level every row of which was read. A read that stopped at its
    /// bound may have stopped inside its last resource, whose other rows (the actions it
    /// offers, the one that reads it among them) are on the page after: that one is no
    /// resource to name or act on until the level is read on.
    pub fn whole_children(&self) -> &[Node] {
        match self.children.split_last() {
            Some((_, whole)) if self.unread() => whole,
            _ => &self.children,
        }
    }

    /// Whether the provider's cursors the level was read by are row offsets ([`row_offset`]),
    /// as far as a cursor it handed out says.
    fn pages_by_offset(&self) -> bool {
        self.pages
            .rest
            .iter()
            .chain(&self.pages.marks)
            .next()
            .is_some_and(|mark| mark.cursor.len() == size_of::<u64>())
    }

    /// The provider's cursor for the row the resource at `child` among the
    /// level's rows starts at, and the revision it was read at: where a
    /// listing's next page asks for its first resource, so that page reads
    /// from there and not from the level's first one. A resource past the
    /// rows read starts at the page the level goes on from; `None` only when
    /// the rows read cover the directory whole, so there is nowhere to go on
    /// from.
    pub fn row_mark(&self, parent: &str, child: usize) -> Option<(Vec<u8>, String)> {
        let rows = &self.pages.rows;
        let mut seen: HashSet<&str> = HashSet::new();
        let mut at = None;
        for (index, row) in rows.iter().enumerate() {
            let id = row.resource.id.as_str();
            if (!parent.is_empty() && id == parent) || !seen.insert(id) {
                continue;
            }
            if seen.len() == child.saturating_add(1) {
                at = Some(index);
                break;
            }
        }
        let Some(at) = at else {
            // The page asks for a resource past those read: it stands on the
            // page the directory goes on from, if it does, and else after
            // every row read, where the directory holds nothing more.
            if let Some(rest) = self.pages.rest.as_ref() {
                return Some((rest.cursor.clone(), rest.revision.clone()));
            }
            let mark = self.pages.marks.last()?;
            let offset = row_offset(&mark.cursor)
                .saturating_add(spanned(rows.get(mark.row..).unwrap_or_default()));
            return Some((
                u64::try_from(offset)
                    .unwrap_or(u64::MAX)
                    .to_le_bytes()
                    .to_vec(),
                mark.revision.clone(),
            ));
        };
        // The marks a level leaves are pages of it, each with the provider's
        // row cursor it started at: the rows since move that on by as many as each stands
        // for. A row of the first page, which leaves no mark, is that many rows from the
        // directory's start, at the revision the level was read at, where the provider's
        // cursors are row offsets.
        let (offset, revision) = match self.pages.marks.iter().rev().find(|mark| mark.row <= at) {
            Some(mark) => (
                row_offset(&mark.cursor)
                    .saturating_add(spanned(rows.get(mark.row..at).unwrap_or_default())),
                &mark.revision,
            ),
            None if !self.revision.is_empty() && self.pages_by_offset() => {
                (spanned(rows.get(..at).unwrap_or_default()), &self.revision)
            }
            None => return None,
        };
        Some((
            u64::try_from(offset)
                .unwrap_or(u64::MAX)
                .to_le_bytes()
                .to_vec(),
            revision.clone(),
        ))
    }
}

/// The row a provider's cursor names: how many rows of the directory it
/// stands at.
#[must_use]
pub fn row_offset(cursor: &[u8]) -> usize {
    <[u8; 8]>::try_from(cursor)
        .map(u64::from_le_bytes)
        .ok()
        .and_then(|offset| usize::try_from(offset).ok())
        .unwrap_or(0)
}

/// How many of the provider's rows `rows` stand for: a resource's actions sent as one row
/// ([`CatalogueQuery::by_resource`]) are a row each where the provider counts its cursor.
#[must_use]
pub fn spanned(rows: &[CatalogueRow]) -> usize {
    rows.iter()
        .map(|row| {
            row.offered
                .as_ref()
                .map_or(1, |offered| offered.actions.len().max(1))
        })
        .sum()
}

/// How many resources of the level under `parent` `rows` name: each once,
/// and never `parent` itself, whose own actions are the level's own.
#[must_use]
pub fn distinct_children(rows: &[CatalogueRow], parent: &str) -> usize {
    let mut seen = HashSet::new();
    for row in rows {
        if row.resource.id != parent {
            seen.insert(row.resource.id.as_str());
        }
    }
    seen.len()
}
