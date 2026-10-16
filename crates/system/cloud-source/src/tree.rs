//! Projecting id-based items onto a POSIX tree.
//!
//! Providers identify items by id; people and tools see paths. [`Tree`] holds
//! the known items of one account and derives, for every folder, a set of
//! child names that is valid and unique on a POSIX filesystem:
//!
//! - names that cannot be file names (`/`, NUL, `.`, `..`, empty) are
//!   rewritten;
//! - siblings with the same name (Google Drive allows them) are told apart
//!   as `name (2).ext`, `name (3).ext`, … in id order, so the projection is
//!   deterministic and does not depend on the order changes arrived in;
//! - an item with several parents appears under each of them;
//! - a supported native document uses its preferred Office export extension;
//!   unsupported native documents retain their unadorned name.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::{Item, ItemId, ItemKind};

/// Longest projected name in bytes (`NAME_MAX`).
pub const MAX_NAME_BYTES: usize = 255;
/// Deepest path [`Tree::paths`] follows; deeper chains are treated as cycles.
const MAX_DEPTH: usize = 256;

/// The known items of one account, indexed by id and by parent.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    root: ItemId,
    items: BTreeMap<ItemId, Item>,
    children: BTreeMap<ItemId, BTreeSet<ItemId>>,
}

impl Tree {
    pub fn new(root: ItemId) -> Self {
        Self {
            root,
            items: BTreeMap::new(),
            children: BTreeMap::new(),
        }
    }

    pub fn root(&self) -> &ItemId {
        &self.root
    }

    pub fn get(&self, id: &ItemId) -> Option<&Item> {
        self.items.get(id)
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.items.values()
    }

    /// Insert or replace an item. Returns the previous version.
    pub fn upsert(&mut self, item: Item) -> Option<Item> {
        let previous = self.unlink(&item.id);
        for parent in &item.parents {
            self.children
                .entry(parent.clone())
                .or_default()
                .insert(item.id.clone());
        }
        self.items.insert(item.id.clone(), item);
        previous
    }

    /// Remove one item. Its children stay known but unreachable until they
    /// are removed or re-parented.
    pub fn remove(&mut self, id: &ItemId) -> Option<Item> {
        self.unlink(id)
    }

    /// Remove an item and everything reachable only through it.
    pub fn remove_recursive(&mut self, id: &ItemId) -> Vec<Item> {
        let mut removed = Vec::new();
        let mut pending = vec![id.clone()];
        while let Some(next) = pending.pop() {
            if let Some(kids) = self.children.get(&next) {
                for kid in kids {
                    let other_parent = self.items.get(kid).is_some_and(|item| {
                        item.parents
                            .iter()
                            .any(|p| p != &next && self.items.contains_key(p))
                    });
                    if !other_parent {
                        pending.push(kid.clone());
                    }
                }
            }
            if let Some(item) = self.unlink(&next) {
                removed.push(item);
            }
            self.children.remove(&next);
        }
        removed
    }

    fn unlink(&mut self, id: &ItemId) -> Option<Item> {
        let previous = self.items.remove(id)?;
        for parent in &previous.parents {
            if let Some(set) = self.children.get_mut(parent) {
                set.remove(id);
            }
        }
        Some(previous)
    }

    /// The children of `folder` under their projected names, sorted by name.
    pub fn children(&self, folder: &ItemId) -> Vec<(String, &Item)> {
        let Some(ids) = self.children.get(folder) else {
            return Vec::new();
        };
        let mut by_name: BTreeMap<String, Vec<&Item>> = BTreeMap::new();
        for id in ids {
            if let Some(item) = self.items.get(id) {
                by_name.entry(base_name(item)).or_default().push(item);
            }
        }
        let taken: HashSet<String> = by_name.keys().cloned().collect();
        let mut out = Vec::with_capacity(ids.len());
        for (name, mut group) in by_name {
            group.sort_by(|a, b| a.id.cmp(&b.id));
            let mut ordinal: u32 = 1;
            for item in group {
                if ordinal == 1 {
                    out.push((name.clone(), item));
                } else {
                    let mut candidate = numbered(&name, ordinal);
                    while taken.contains(&candidate) {
                        ordinal = ordinal.saturating_add(1);
                        candidate = numbered(&name, ordinal);
                    }
                    out.push((candidate, item));
                }
                ordinal = ordinal.saturating_add(1);
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The child of `folder` projected as `name`.
    pub fn child(&self, folder: &ItemId, name: &str) -> Option<&Item> {
        self.children(folder)
            .into_iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, item)| item)
    }

    /// The item at a projected path (components relative to the root).
    pub fn resolve<'a>(&self, path: impl IntoIterator<Item = &'a str>) -> Option<&Item> {
        let mut current = self.root.clone();
        let mut found = None;
        for component in path {
            let item = self.child(&current, component)?;
            current = item.id.clone();
            found = Some(item);
        }
        found.or_else(|| self.items.get(&self.root))
    }

    /// The projected name of `id` inside `parent`.
    pub fn name_in(&self, parent: &ItemId, id: &ItemId) -> Option<String> {
        self.children(parent)
            .into_iter()
            .find(|(_, item)| &item.id == id)
            .map(|(name, _)| name)
    }

    /// Every projected path of `id`, one per chain of parents that reaches
    /// the root. The root itself has one empty path.
    pub fn paths(&self, id: &ItemId) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        self.collect_paths(id, &mut Vec::new(), &mut seen, &mut out);
        out
    }

    fn collect_paths(
        &self,
        id: &ItemId,
        suffix: &mut Vec<String>,
        seen: &mut HashSet<ItemId>,
        out: &mut Vec<Vec<String>>,
    ) {
        if id == &self.root {
            out.push(suffix.iter().rev().cloned().collect());
            return;
        }
        if suffix.len() >= MAX_DEPTH || !seen.insert(id.clone()) {
            return;
        }
        if let Some(item) = self.items.get(id) {
            for parent in &item.parents {
                if let Some(name) = self.name_in(parent, id) {
                    suffix.push(name);
                    self.collect_paths(parent, suffix, seen, out);
                    suffix.pop();
                }
            }
        }
        seen.remove(id);
    }
}

/// The projected name of an item before sibling disambiguation.
pub fn base_name(item: &Item) -> String {
    let name = sanitize(&item.name);
    match &item.kind {
        ItemKind::Native(document) => crate::materialize::preferred_export(document).map_or_else(
            |_| name.clone(),
            |format| truncate_keeping(&name, &format!(".{}", format.extension)),
        ),
        _ => name,
    }
}

/// A valid POSIX file name for a provider name.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' => '\u{2215}',
            '\0' => '\u{fffd}',
            c => c,
        })
        .collect();
    let cleaned = match cleaned.as_str() {
        "" => "_".to_owned(),
        "." => "_.".to_owned(),
        ".." => "_..".to_owned(),
        _ => cleaned,
    };
    truncate_keeping(&cleaned, "")
}

/// `stem (n).ext`.
fn numbered(name: &str, ordinal: u32) -> String {
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 => (
            name.get(..dot).unwrap_or(name),
            name.get(dot..).unwrap_or(""),
        ),
        _ => (name, ""),
    };
    truncate_keeping(stem, &format!(" ({ordinal}){extension}"))
}

/// `prefix + suffix`, cutting `prefix` at a char boundary so the result
/// fits [`MAX_NAME_BYTES`].
fn truncate_keeping(prefix: &str, suffix: &str) -> String {
    let budget = MAX_NAME_BYTES.saturating_sub(suffix.len());
    let mut end = prefix.len().min(budget);
    while end > 0 && !prefix.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}{suffix}", prefix.get(..end).unwrap_or(""))
}

/// Unique directory names for mounted accounts, from `(account id, label)`
/// pairs: sanitised labels, duplicates numbered in account-id order.
pub fn mount_names(accounts: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut tree = Tree::new(ItemId::new(""));
    for (id, label) in accounts {
        tree.upsert(Item {
            id: ItemId::new(*id),
            parents: vec![ItemId::new("")],
            name: if label.trim().is_empty() {
                (*id).to_owned()
            } else {
                label.trim().to_owned()
            },
            kind: ItemKind::Folder,
            size: None,
            modified: None,
            revision: None,
            hashes: Vec::new(),
            mime: None,
        });
    }
    tree.children(&ItemId::new(""))
        .into_iter()
        .map(|(name, item)| (item.id.0.clone(), name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativeDocument;

    fn item(id: &str, parents: &[&str], name: &str, kind: ItemKind) -> Item {
        Item {
            id: ItemId::new(id),
            parents: parents.iter().map(|p| ItemId::new(*p)).collect(),
            name: name.into(),
            kind,
            size: None,
            modified: None,
            revision: None,
            hashes: Vec::new(),
            mime: None,
        }
    }

    #[test]
    fn duplicate_names_are_numbered_in_id_order() {
        let mut tree = Tree::new(ItemId::new("root"));
        tree.upsert(item("b", &["root"], "Report.pdf", ItemKind::File));
        tree.upsert(item("a", &["root"], "Report.pdf", ItemKind::File));
        tree.upsert(item("c", &["root"], "Report (2).pdf", ItemKind::File));
        let names: Vec<(String, String)> = tree
            .children(&ItemId::new("root"))
            .into_iter()
            .map(|(n, i)| (n, i.id.0.clone()))
            .collect();
        assert_eq!(
            names,
            [
                ("Report (2).pdf".to_owned(), "c".to_owned()),
                ("Report (3).pdf".to_owned(), "b".to_owned()),
                ("Report.pdf".to_owned(), "a".to_owned()),
            ]
        );
    }

    #[test]
    fn multi_parent_items_have_a_path_per_parent_and_resolve_back() {
        let mut tree = Tree::new(ItemId::new("root"));
        tree.upsert(item("x", &["root"], "Work", ItemKind::Folder));
        tree.upsert(item("y", &["root"], "Shared", ItemKind::Folder));
        tree.upsert(item("f", &["x", "y"], "plan.txt", ItemKind::File));
        let mut paths = tree.paths(&ItemId::new("f"));
        paths.sort();
        assert_eq!(paths, [["Shared", "plan.txt"], ["Work", "plan.txt"]]);
        assert_eq!(
            tree.resolve(["Shared", "plan.txt"])
                .map(|i| i.id.0.as_str()),
            Some("f")
        );
        let removed = tree.remove_recursive(&ItemId::new("x"));
        assert_eq!(removed.len(), 1, "plan.txt still lives under Shared");
        assert!(tree.resolve(["Shared", "plan.txt"]).is_some());
    }

    #[test]
    fn invalid_names_and_native_documents_project_safely() {
        let mut tree = Tree::new(ItemId::new("root"));
        tree.upsert(item("1", &["root"], "a/b", ItemKind::File));
        tree.upsert(item("2", &["root"], "..", ItemKind::File));
        tree.upsert(item(
            "3",
            &["root"],
            "Budget",
            ItemKind::Native(NativeDocument {
                mime: "application/vnd.google-apps.spreadsheet".into(),
                link_extension: "gsheet".into(),
                web_url: None,
                exports: Vec::new(),
            }),
        ));
        let names: Vec<String> = tree
            .children(&ItemId::new("root"))
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(names, ["Budget", "_..", "a\u{2215}b"]);
        let long = "é".repeat(300);
        assert!(sanitize(&long).len() <= MAX_NAME_BYTES);
    }

    #[test]
    fn cycles_do_not_hang_path_projection() {
        let mut tree = Tree::new(ItemId::new("root"));
        tree.upsert(item("a", &["b"], "A", ItemKind::Folder));
        tree.upsert(item("b", &["a"], "B", ItemKind::Folder));
        assert!(tree.paths(&ItemId::new("a")).is_empty());
    }

    #[test]
    fn mount_names_are_unique_and_stable() {
        let names = mount_names(&[("n2", "Cloud"), ("n1", "Cloud"), ("w", " ")]);
        assert_eq!(
            names,
            [
                ("n1".to_owned(), "Cloud".to_owned()),
                ("n2".to_owned(), "Cloud (2)".to_owned()),
                ("w".to_owned(), "w".to_owned()),
            ]
        );
    }
}
