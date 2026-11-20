//! The directory model a path is read through: one resource of an app's
//! directory, and the rows one listing holds under a parent.

use super::{ActionDescriptor, CatalogueRow, HashMap, bound_key, path_name};
use tilo_actions::api::ActionAddress;

impl Node {
    /// This resource as a row of a listing under `parent`: the name its line
    /// prints, the actions it offers by their ids, and whether a read answers
    /// it as a resource of its own (`reads`), or it is only a place inside
    /// what it is listed under (an option, a choice): those are part of their
    /// parent's read and name no resource, so a walk that reads resources one
    /// by one reads only the rows that say so.
    pub(super) fn listing_row(
        &self,
        parent: &str,
        name: &str,
        kind: &str,
        reads: bool,
    ) -> super::Value {
        super::json!({
            "path": format!("{parent}/{name}"),
            "name": name,
            "id": self.id,
            "key": self.key,
            "label": self.label,
            "kind": kind,
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
    pub(super) fn listed_actions(&self) -> impl Iterator<Item = &ActionDescriptor> {
        self.actions
            .iter()
            .filter(|action| !action.contract.needs_an_item())
    }
}

/// The names `rows` share, lowercased, each named among `siblings`
/// ([`Node::name`]): a line named by one of them is told apart by its id
/// beside it. Two titles used once each per parent are told apart by their
/// places instead, so they are not shared.
pub(super) fn shared_names(
    rows: &[Node],
    siblings: &Names<'_>,
) -> std::collections::HashSet<String> {
    let mut seen = std::collections::HashSet::new();
    rows.iter()
        .map(|row| row.name_in(siblings).trim().to_lowercase())
        .filter(|name| !seen.insert(name.clone()))
        .collect()
}

/// Siblings indexed by their titles: what tells whether a name sets one of
/// them apart ([`Node::name_in`]) without reading every other sibling, so a
/// listing of hundreds names each of its rows at the cost of one.
pub(super) struct Names<'a> {
    siblings: Vec<&'a Node>,
    /// Each sibling's index by its title, trimmed and lowercased, in order.
    titled: HashMap<String, Vec<usize>>,
}

impl<'a> Names<'a> {
    pub(super) fn of(siblings: impl IntoIterator<Item = &'a Node>) -> Self {
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
pub(crate) struct Node {
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
        if self.names_id(name) {
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

    /// Whether `name` is this resource's own id, or the item part of it
    /// (`<kind>/<field>/<item>`): an id names the resource before any label
    /// does, whatever the label says.
    pub(super) fn names_id(&self, name: &str) -> bool {
        let same = |text: &str| text.trim().eq_ignore_ascii_case(name);
        let key_tail = self.key.rsplit('/').next().unwrap_or_default();
        same(&self.key) || same(&self.id) || same(key_tail) || same(self.token())
    }

    /// The name that goes in a path: its label when it tells it apart from
    /// `siblings`, else its key's last part (`body`), else its key.
    pub(super) fn name(&self, siblings: &[Node]) -> String {
        self.name_in(&Names::of(siblings))
    }

    /// [`Self::name`] among siblings already indexed by their titles.
    pub(super) fn name_in(&self, siblings: &Names<'_>) -> String {
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
pub(crate) struct Level {
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
