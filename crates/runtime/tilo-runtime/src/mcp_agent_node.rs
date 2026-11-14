//! The directory model a path is read through: one resource of an app's
//! directory, and the rows one listing holds under a parent.

use super::{ActionDescriptor, CatalogueRow, HashMap, bound_key, path_name};

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
        if apart(label) {
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
    level
}
