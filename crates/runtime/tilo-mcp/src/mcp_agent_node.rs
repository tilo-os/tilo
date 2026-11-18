//! The directory model a path is read through: one resource of an app's
//! directory, and the rows one listing holds under a parent.

use super::{ActionDescriptor, CatalogueRow, HashMap, bound_key, path_name};

impl Node {
    /// This resource as a row of a listing under `parent`: the name its line
    /// prints, and the actions it offers by their ids.
    pub(super) fn listing_row(&self, parent: &str, name: &str, kind: &str) -> super::Value {
        super::json!({
            "path": format!("{parent}/{name}"),
            "name": name,
            "id": self.id,
            "key": self.key,
            "label": self.label,
            "kind": kind,
            "has_children": self.container,
            "actions": self
                .actions
                .iter()
                .map(|action| action.address.action.clone())
                .collect::<Vec<_>>(),
        })
    }
}

/// The names `rows` share, lowercased, each named among `siblings`
/// ([`Node::name`]): a line named by one of them is told apart by its id
/// beside it. Two titles used once each per parent are told apart by their
/// places instead, so they are not shared.
pub(super) fn shared_names(rows: &[Node], siblings: &[Node]) -> std::collections::HashSet<String> {
    let mut seen = std::collections::HashSet::new();
    rows.iter()
        .filter(|row| !seen.insert(row.name(siblings).trim().to_lowercase()))
        .map(|row| row.name(siblings).trim().to_lowercase())
        .collect()
}

/// One resource of an app's directory, as an agent names it.
#[derive(Clone, Debug)]
pub(super) struct Node {
    /// The broker's id: never shown, only sent back.
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) label: String,
    /// The app's own stable name for it: its semantic key, else the logical
    /// id inside a bound id, else the id.
    pub(super) key: String,
    pub(super) parent: String,
    /// Its place among the siblings that share its title, from 1: what tells
    /// two pages titled alike apart in a path (`TODO`, `TODO 2`).
    pub(super) ordinal: usize,
    pub(super) container: bool,
    pub(super) actions: Vec<ActionDescriptor>,
}

impl Node {
    /// The part of the key that tells two resources of one kind apart
    /// (`notes.page:3fa2…` → `3fa2…`).
    pub(super) fn token(&self) -> &str {
        self.key
            .rsplit([':', '/'])
            .find(|part| !part.is_empty())
            .unwrap_or(&self.key)
    }

    /// Whether `other`'s key names this one's distinguishing part.
    pub(super) fn owns(&self, other: &Node) -> bool {
        let token = self.token();
        token.len() >= 8 && other.key != self.key && other.key.contains(token)
    }

    /// Whether `other` is this resource's own and not one of its children.
    pub(super) fn beside(&self, other: &Node) -> bool {
        self.owns(other) && !other.key.starts_with(&self.key)
    }

    /// Whether this resource is one of `other`'s: it sits under the other's id.
    pub(super) fn within(&self, other: &Node) -> bool {
        matches!(self.id.strip_prefix(other.id.as_str()), Some(rest) if rest.starts_with('/'))
    }

    /// How well `name` names this node: 0 its label, 1 its key (whole or last
    /// part), 2 its kind, 3 the start of either; `None` when it does not.
    pub(super) fn answers_to(&self, name: &str) -> Option<u8> {
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
        let key_tail = self.key.rsplit('/').next().unwrap_or_default();
        if same(&self.key) || same(&self.id) || same(key_tail) || same(self.token()) {
            return Some(1);
        }
        if same(&self.kind) || tail(&self.kind) {
            return Some(2);
        }
        let lower = name.to_lowercase();
        let starts = |text: &str| text.to_lowercase().starts_with(&lower);
        (lower.chars().count() >= 3
            && (starts(&self.label) || (lower.len() >= 6 && starts(self.token()))))
        .then_some(3)
    }

    /// The name that goes in a path: its label when it tells it apart from
    /// `siblings`, else its key's last part (`body`), else its key.
    pub(super) fn name(&self, siblings: &[Node]) -> String {
        // Neither what belongs to this resource nor what it is a part of
        // competes with it: a note titled like its notebook keeps its title.
        let apart = |name: &str| {
            !name.trim().is_empty()
                && siblings
                    .iter()
                    .filter(|other| other.id != self.id)
                    .filter(|other| !self.owns(other) && !self.within(other))
                    .all(|other| other.answers_to(name).is_none_or(|rank| rank > 0))
        };
        let label = self.label.trim();
        // A page and the body published beside it share a title but not a
        // level: only what sits beside this one takes its title.
        let twin = !label.is_empty()
            && siblings.iter().any(|other| {
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
pub(super) struct Level {
    /// Actions of the parent itself.
    pub(super) own: Vec<ActionDescriptor>,
    pub(super) children: Vec<Node>,
    /// The directory revision it was read at.
    pub(super) revision: String,
    /// The pages it was read from ([`super::LevelPages`]).
    pub(super) pages: std::sync::Arc<super::LevelPages>,
}

pub(super) fn level_of(rows: &[CatalogueRow], parent: &str) -> Level {
    let mut level = Level::default();
    let mut at: HashMap<&str, usize> = HashMap::new();
    for row in rows {
        if !parent.is_empty() && row.resource.id == parent {
            if let Some(action) = &row.action {
                level.own.push(action.clone());
            }
            continue;
        }
        let key = row
            .action
            .as_ref()
            .and_then(|action| action.semantic_identity.as_ref())
            .map(|identity| identity.key.clone())
            .or_else(|| bound_key(&row.resource.id));
        let index = *at.entry(row.resource.id.as_str()).or_insert_with(|| {
            level.children.push(Node {
                id: row.resource.id.clone(),
                kind: row.resource.kind.clone(),
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
            if let Some(action) = &row.action {
                node.actions.push(action.clone());
            }
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
