//! The person's own bookmark library. Importing it never edits the room's Browser record.
//!
//! A bookmark is an add under the tag minted when it was made; removing it
//! tombstones the adds of it that were seen, so an add made concurrently on
//! another device survives the remove (an observed-remove set). An import
//! keeps a receipt per source, so retrying it changes nothing and a
//! different import from the same source is refused; the folder trees it
//! brings are kept whole per source, and removing a folder or placement
//! tombstones it beside its tree.

use crate::continuity::{ImportBatch, ImportBundle, MAX_BUNDLE_ENTRIES, MAX_IMPORT_BYTES};
use crate::continuity_library::{Bookmark, Bookmarks};
use crate::imported_folders::{ImportedNode, ImportedTree, NodeKind};
use ui::tilo_resource::Effect as _;
use ui::tilo_resource::{self, ActionCx, Atom, Map, Model, ModelError, Refusal, Set, resource};

pub const SCOPE: &str = "browser.bookmarks/personal";

/// The longest bookmark title kept, in bytes.
const MAX_BOOKMARK_TITLE_BYTES: usize = 4 * 1024;
/// The most import sources whose receipts a library keeps.
const MAX_RECEIPTS: usize = 1024;
/// The most imported trees a library keeps.
const MAX_TREES: usize = 128;

/// One bookmark added: the address and the title it was added with.
#[derive(Model, Default, Clone, Debug, PartialEq, Eq)]
pub struct Added {
    /// The normalized address.
    #[model(atom)]
    pub url: String,
    /// The title it shows.
    #[model(atom)]
    pub title: String,
}

/// The person's own bookmark library. See the module docs.
#[derive(Model, Default, Clone, Debug, PartialEq)]
pub struct PersonalBookmarks {
    /// Every bookmark added, by the tag minted when it was.
    #[model(readonly)]
    pub added: Map<Added>,
    /// The tags of the adds removed.
    #[model(readonly)]
    pub removed: Set<String>,
    /// What each import source brought, by the source in hex: the digest of
    /// its batch.
    #[model(readonly)]
    pub receipts: Map<Atom>,
    /// The folder trees imported, by their source in hex.
    #[model(readonly)]
    pub trees: Map<ImportedTree>,
    /// The imported folders and placements removed
    /// ([`crate::imported_folders::node_key`]).
    #[model(readonly)]
    pub hidden: Set<String>,
}

/// The library as the window shows it: its bookmarks, one row per address,
/// and its imported trees without what was removed from them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersonalBookmarkState {
    pub bookmarks: Bookmarks,
    pub imported_trees: Vec<ImportedTree>,
}

fn refused() -> Refusal {
    Refusal::new("Browser import is invalid or exceeds its limit")
}

fn tree_refused() -> Refusal {
    Refusal::new("Invalid or conflicting imported bookmark hierarchy")
}

fn url_refused() -> Refusal {
    Refusal::new("bookmark must be an absolute http(s) URL")
}

#[resource(
    kind = "browser.personal-bookmarks",
    label = "Personal bookmarks",
    version = 2,
    migrate = upgrade
)]
impl PersonalBookmarks {
    /// Bookmark a page.
    ///
    /// Adds the page under a tag minted now, titled `title` (its address
    /// when none).
    #[action]
    pub fn add_bookmark(
        &mut self,
        cx: &mut ActionCx,
        url: String,
        title: String,
    ) -> Result<(), Refusal> {
        let url = crate::actions::normalized_bookmark_url(&url).ok_or_else(url_refused)?;
        if title.len() > MAX_BOOKMARK_TITLE_BYTES {
            return Err(Refusal::new("bookmark metadata exceeds the safety limit"));
        }
        let title = if title.trim().is_empty() {
            url.clone()
        } else {
            title.trim().to_owned()
        };
        let tag = format!("bookmark:{}", cx.id());
        self.add(&tag, url, title);
        Ok(())
    }

    /// Remove a bookmark.
    ///
    /// Tombstones the adds of the page that were seen (`observed_tags`), so
    /// one made meanwhile on another device stays.
    #[action]
    pub fn remove_bookmark(
        &mut self,
        url: String,
        observed_tags: Vec<String>,
    ) -> Result<(), Refusal> {
        crate::actions::normalized_bookmark_url(&url).ok_or_else(url_refused)?;
        for tag in observed_tags
            .into_iter()
            .filter(|tag| !tag.trim().is_empty())
        {
            self.removed.insert(tag);
        }
        Ok(())
    }

    /// Import bookmarks.
    ///
    /// A reviewed import (its JSON) added beside what the library holds: a
    /// retry changes nothing, a different import from the same source is
    /// refused.
    #[action]
    pub fn import_bookmarks(
        &mut self,
        #[param("Reviewed bookmark import")] bundle: String,
    ) -> Result<(), Refusal> {
        let bundle = ImportBundle::decode(&bundle)
            .map_err(|_| Refusal::new("Invalid personal bookmark import"))?;
        self.import_bundle(&bundle)
    }

    /// Remove an imported folder or placement.
    ///
    /// Hides it, and everything in a folder, from the tree imported from
    /// `source` (hex); the tree itself stays as it was imported.
    #[action]
    pub fn remove_imported_node(&mut self, source: String, id: String) -> Result<(), Refusal> {
        let tree = self.trees.get(&source).ok_or_else(tree_refused)?;
        tree.validate().map_err(|_| tree_refused())?;
        if crate::imported_folders::source_id(&tree.source) != source
            || !tree.nodes.iter().any(|node| node.id == id)
        {
            return Err(tree_refused());
        }
        let key = crate::imported_folders::node_key(&tree.source, &id);
        self.hidden.insert(key);
        Ok(())
    }
}

impl PersonalBookmarks {
    /// Add `url` under `tag`, unless that tag was added already.
    fn add(&mut self, tag: &str, url: String, title: String) {
        if !self.added.contains_key(tag) {
            self.added.insert(tag, Added { url, title });
        }
    }

    /// The library as the window shows it.
    #[must_use]
    pub fn projected(&self) -> PersonalBookmarkState {
        PersonalBookmarkState {
            bookmarks: self.bookmarks(),
            imported_trees: self.imported_trees(),
        }
    }

    /// The bookmarks: every add not removed, one row per address with the
    /// tags still adding it.
    #[must_use]
    pub fn bookmarks(&self) -> Bookmarks {
        let mut rows: Vec<Bookmark> = self
            .added
            .iter()
            .filter(|(tag, _)| !self.removed.contains(*tag))
            .map(|(tag, added)| Bookmark {
                url: added.url.clone(),
                title: added.title.clone(),
                tags: vec![tag.clone()],
            })
            .collect();
        let imported = |row: &Bookmark| {
            row.tags
                .first()
                .is_some_and(|tag| tag.starts_with("import:"))
        };
        rows.sort_by(|a, b| {
            a.url
                .cmp(&b.url)
                .then_with(|| imported(a).cmp(&imported(b)))
                .then_with(|| a.title.cmp(&b.title))
        });
        let mut merged: Vec<Bookmark> = Vec::new();
        for row in rows {
            if let Some(last) = merged.last_mut()
                && last.url == row.url
            {
                last.tags.extend(row.tags);
                last.tags.sort();
                last.tags.dedup();
            } else {
                merged.push(row);
            }
        }
        merged.truncate(MAX_BUNDLE_ENTRIES);
        Bookmarks::from_entries(merged)
    }

    /// The imported trees, each without the folders and placements removed
    /// from it. A tree kept under another source than its own, or that does
    /// not hold together, is left out.
    #[must_use]
    pub fn imported_trees(&self) -> Vec<ImportedTree> {
        self.trees
            .iter()
            .filter(|(source, tree)| {
                crate::imported_folders::source_id(&tree.source) == **source
                    && tree.validate().is_ok()
            })
            .take(MAX_TREES)
            .map(|(_, tree)| {
                let hidden = crate::imported_folders::hidden(tree, |key| {
                    self.hidden.contains(&key.to_owned())
                });
                let mut tree = tree.clone();
                tree.nodes.retain(|node| !hidden.contains(&node.id));
                tree
            })
            .collect()
    }

    /// The receipt `batch` would leave, `None` when an identical import
    /// from its source was taken already.
    fn receipt(&self, batch: &ImportBatch) -> Result<Option<(String, String)>, Refusal> {
        batch.validate().map_err(|_| refused())?;
        let key = crate::imported_folders::source_id(&batch.source);
        if !self.receipts.contains_key(&key) && self.receipts.len() >= MAX_RECEIPTS {
            return Err(refused());
        }
        let encoded = serde_json::to_vec(batch).map_err(|_| refused())?;
        if encoded.len() > MAX_IMPORT_BYTES {
            return Err(refused());
        }
        let digest = blake3::hash(&encoded).to_hex().to_string();
        match self.receipts.get(&key) {
            Some(taken) if taken.0 == digest => Ok(None),
            Some(_) => Err(Refusal::new(
                "Browser source receipt conflicts with this import",
            )),
            None => Ok(Some((key, digest))),
        }
    }

    /// Check every chunk and tree of `bundle` and the library's capacity
    /// before anything is written.
    fn preflight(&self, bundle: &ImportBundle) -> Result<(), Refusal> {
        bundle.validate().map_err(|_| refused())?;
        let mut additions = 0usize;
        let mut receipts = 0usize;
        for chunk in &bundle.chunks {
            if !chunk.history.is_empty() {
                return Err(refused());
            }
            if self.receipt(chunk)?.is_some() {
                receipts = receipts.saturating_add(1);
                additions = additions.saturating_add(chunk.bookmarks.len());
            }
        }
        if self.added.len().saturating_add(additions) > MAX_BUNDLE_ENTRIES
            || self.receipts.len().saturating_add(receipts) > MAX_RECEIPTS
        {
            return Err(refused());
        }
        if let Some(tree) = &bundle.imported_tree {
            self.preflight_tree(tree)?;
        }
        Ok(())
    }

    /// Whether `tree` may be kept: whole and the same as a tree from its
    /// source kept already, or within what the library keeps.
    fn preflight_tree(&self, tree: &ImportedTree) -> Result<(), Refusal> {
        tree.validate().map_err(|_| tree_refused())?;
        match self
            .trees
            .get(&crate::imported_folders::source_id(&tree.source))
        {
            Some(kept) if kept == tree => Ok(()),
            Some(_) => Err(tree_refused()),
            None => {
                let kept: usize = self.trees.values().map(|tree| tree.nodes.len()).sum();
                if self.trees.len() >= MAX_TREES
                    || kept.saturating_add(tree.nodes.len()) > MAX_BUNDLE_ENTRIES
                {
                    Err(tree_refused())
                } else {
                    Ok(())
                }
            }
        }
    }

    fn import_bundle(&mut self, bundle: &ImportBundle) -> Result<(), Refusal> {
        self.preflight(bundle)?;
        for chunk in &bundle.chunks {
            self.import_batch(chunk)?;
        }
        if let Some(tree) = &bundle.imported_tree {
            let source = crate::imported_folders::source_id(&tree.source);
            if !self.trees.contains_key(&source) {
                self.trees.insert(source, tree.clone());
            }
        }
        Ok(())
    }

    fn import_batch(&mut self, batch: &ImportBatch) -> Result<(), Refusal> {
        if !batch.history.is_empty() {
            return Err(Refusal::new(
                "History belongs to its private history record",
            ));
        }
        let Some((key, digest)) = self.receipt(batch)? else {
            return Ok(());
        };
        if self.added.len().saturating_add(batch.bookmarks.len()) > MAX_BUNDLE_ENTRIES {
            return Err(Refusal::new("Too many imported bookmarks"));
        }
        for row in &batch.bookmarks {
            let tag = crate::continuity::tag(&batch.source, &row.url);
            self.add(&tag, row.url.clone(), row.title.clone());
        }
        self.receipts.insert(key, Atom(digest));
        Ok(())
    }
}

/// The root keys the library kept its parts under before it was a record.
mod legacy {
    pub const ADD: &str = "browser.bookmarks.v2.add:";
    pub const REMOVE: &str = "browser.bookmarks.v2.remove:";
    pub const RECEIPT: &str = "browser.import.bookmarks:";
    pub const TREE: &str = "browser.imported-tree:";
    pub const REMOVED_NODE: &str = "browser.imported-tree-removed:";
}

/// Bring a library of version 1, which kept its adds, tombstones, receipts
/// and trees as prefixed keys at the document's root, up to the record:
/// each moves into its field, under the same tag, source or node key.
fn upgrade(doc: &mut ui::tilo_resource::Doc, from: u32) -> Result<(), ModelError> {
    use automerge::transaction::Transactable as _;
    use automerge::{ObjType, ROOT, ReadDoc as _, Value};
    if from != 1 {
        return Err(ModelError::invalid(format!(
            "no personal bookmarks migration from {from}"
        )));
    }
    let scalar = |doc: &ui::tilo_resource::Doc, obj: &automerge::ObjId, key: &str| {
        doc.get(obj, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| value.into_scalar().ok())
            .and_then(|value| value.as_str().map(str::to_owned))
    };
    let mut added = Map::<Added>::new();
    let mut removed = Set::<String>::new();
    let mut receipts = Map::<Atom>::new();
    let mut trees = Map::<ImportedTree>::new();
    let mut hidden = Set::<String>::new();
    let keys: Vec<String> = doc.keys(ROOT).collect();
    let mut moved = Vec::new();
    for key in keys {
        if let Some(tag) = key.strip_prefix(legacy::ADD) {
            if let Some((Value::Object(ObjType::Map), add)) = doc.get(ROOT, key.as_str())?
                && let (Some(url), Some(title)) =
                    (scalar(doc, &add, "url"), scalar(doc, &add, "title"))
            {
                added.insert(tag, Added { url, title });
            }
        } else if let Some(tag) = key.strip_prefix(legacy::REMOVE) {
            removed.insert(tag.to_owned());
        } else if let Some(source) = key.strip_prefix(legacy::RECEIPT) {
            if let Some(digest) = scalar(doc, &ROOT, &key) {
                receipts.insert(source, Atom(digest));
            }
        } else if let Some(source) = key.strip_prefix(legacy::TREE) {
            let text = scalar(doc, &ROOT, &key)
                .ok_or_else(|| ModelError::invalid("an imported tree is no text").at(&key))?;
            let tree: ImportedTree = serde_json::from_str(&text)
                .map_err(|error| ModelError::invalid(error.to_string()).at(&key))?;
            trees.insert(source, tree);
        } else if let Some(node) = key.strip_prefix(legacy::REMOVED_NODE) {
            hidden.insert(node.to_owned());
        } else {
            continue;
        }
        moved.push(key);
    }
    for key in moved {
        doc.delete(ROOT, key.as_str())?;
    }
    ui::tilo_resource::write_key(doc, &ROOT, "added", &added)?;
    ui::tilo_resource::write_key(doc, &ROOT, "removed", &removed)?;
    ui::tilo_resource::write_key(doc, &ROOT, "receipts", &receipts)?;
    ui::tilo_resource::write_key(doc, &ROOT, "trees", &trees)?;
    ui::tilo_resource::write_key(doc, &ROOT, "hidden", &hidden)?;
    Ok(())
}

pub fn flat_id(url: &str) -> String {
    format!("{SCOPE}/{}", blake3::hash(url.as_bytes()).to_hex())
}
pub fn node_id(source: &[u8; 32], id: &str) -> String {
    format!(
        "{SCOPE}/{}:{}",
        blake3::Hash::from(*source).to_hex(),
        blake3::hash(id.as_bytes()).to_hex()
    )
}
pub(crate) fn node_resource(tree: &ImportedTree, node: &ImportedNode) -> tilo_actions::Resource {
    let kind = match node.kind {
        NodeKind::Folder => "browser.personal-bookmark-folder",
        NodeKind::Bookmark { .. } => "browser.personal-bookmark-placement",
        NodeKind::Separator => "browser.personal-bookmark-separator",
    };
    let mut resource =
        tilo_actions::Resource::new(node_id(&tree.source, &node.id), kind, &node.title);
    resource = resource.with_action(
        crate::effects::BrowserEffect::RemovePersonalImportedNode {
            source: blake3::Hash::from(tree.source).to_hex().to_string(),
            id: node.id.clone(),
        }
        .bound(),
    );
    match &node.kind {
        NodeKind::Folder => {
            resource = resource.with_action(
                crate::effects::BrowserEffect::ShowPersonalBookmarkFolder {
                    source: blake3::Hash::from(tree.source).to_hex().to_string(),
                    id: Some(node.id.clone()),
                    offset: 0,
                }
                .bound(),
            );
        }
        NodeKind::Bookmark { url } => {
            resource = resource
                .with_action(crate::effects::BrowserEffect::Open { url: url.clone() }.bound());
        }
        NodeKind::Separator => {}
    }
    resource
}
pub(crate) fn flat_resource(entry: &crate::continuity_library::Bookmark) -> tilo_actions::Resource {
    tilo_actions::Resource::new(
        flat_id(&entry.url),
        "browser.personal-bookmark",
        &entry.title,
    )
    .with_action(
        crate::effects::BrowserEffect::Open {
            url: entry.url.clone(),
        }
        .bound(),
    )
    .with_action(
        crate::effects::BrowserEffect::RemovePersonalBookmark {
            url: entry.url.clone(),
        }
        .bound(),
    )
}
pub(crate) fn search(state: &PersonalBookmarkState) -> Vec<ui::tilo_resource::SearchEntry> {
    let mut entries = Vec::new();
    for entry in state.bookmarks.entries() {
        if entry.tags.iter().all(|tag| tag.starts_with("import:"))
            && state.imported_trees.iter().any(|tree| {
                tree.nodes
                    .iter()
                    .any(|node| matches!(&node.kind,NodeKind::Bookmark {url} if *url==entry.url))
            })
        {
            continue;
        }
        entries.push(
            ui::tilo_resource::SearchEntry::new(&entry.title, &entry.url)
                .invoking(crate::effects::BrowserEffect::OPEN.on(flat_id(&entry.url)))
                .inside(crate::actions::BOOKMARKS_SCOPE)
                .anchored(flat_id(&entry.url)),
        );
    }
    for tree in &state.imported_trees {
        for node in &tree.nodes {
            let NodeKind::Bookmark { url } = &node.kind else {
                continue;
            };
            let id = node_id(&tree.source, &node.id);
            entries.push(
                ui::tilo_resource::SearchEntry::new(&node.title, url)
                    .invoking(crate::effects::BrowserEffect::OPEN.on(id.as_str()))
                    .inside(crate::actions::BOOKMARKS_SCOPE)
                    .anchored(id),
            );
        }
    }
    entries
}
pub(crate) fn resolve(state: &PersonalBookmarkState, id: &str) -> Option<tilo_actions::Resource> {
    for entry in state.bookmarks.entries() {
        if flat_id(&entry.url) == id {
            return Some(flat_resource(entry));
        }
    }
    for tree in &state.imported_trees {
        for node in &tree.nodes {
            if node_id(&tree.source, &node.id) == id {
                return Some(node_resource(tree, node));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
