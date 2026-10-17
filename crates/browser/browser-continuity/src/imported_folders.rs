//! Reviewed bookmark hierarchy. URL identity and placement identity are distinct.
//! A complete source tree is immutable custody; removals are separate tombstones,
//! so retrying its import cannot resurrect a removed folder or placement.
use std::collections::{BTreeMap, BTreeSet};

use automerge::transaction::Transactable;
use automerge::{ROOT, ReadDoc};
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionError, Doc};

const TREE: &str = "browser.imported-tree:";
const REMOVED: &str = "browser.imported-tree-removed:";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum NodeKind {
    Folder,
    Bookmark { url: String },
    Separator,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedNode {
    pub id: String,
    pub parent: Option<String>,
    pub position: u32,
    pub title: String,
    pub kind: NodeKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedTree {
    pub source: [u8; 32],
    pub nodes: Vec<ImportedNode>,
}

fn refused() -> ActionError {
    ActionError::Refused("Invalid or conflicting imported bookmark hierarchy".into())
}

impl ImportedTree {
    /// Validate complete parentage, stable node IDs and bounded ordered trees.
    /// # Errors
    /// Refuses duplicate identities/orders, orphan nodes, cycles and invalid URLs.
    pub fn validate(&self) -> Result<(), ActionError> {
        if self.nodes.len() > crate::imports::MAX_BUNDLE_ENTRIES
            || serde_json::to_vec(self).map_err(|_| refused())?.len()
                > crate::imports::MAX_BUNDLE_BYTES
        {
            return Err(refused());
        }
        let mut ids = BTreeMap::new();
        let mut positions = BTreeSet::new();
        for node in &self.nodes {
            if node.id.is_empty()
                || node.id.len() > 256
                || node.id.chars().any(char::is_control)
                || node.title.len() > 4096
                || !positions.insert((node.parent.as_deref(), node.position))
                || ids.insert(node.id.as_str(), node).is_some()
            {
                return Err(refused());
            }
            if let NodeKind::Bookmark { url } = &node.kind
                && crate::imports::normalized_bookmark_url(url).as_deref() != Some(url)
            {
                return Err(refused());
            }
        }
        for node in &self.nodes {
            let mut parent = node.parent.as_deref();
            let mut depth = 0usize;
            while let Some(id) = parent {
                depth = depth.saturating_add(1);
                let ancestor = ids.get(id).ok_or_else(refused)?;
                if depth > 64 || !matches!(ancestor.kind, NodeKind::Folder) {
                    return Err(refused());
                }
                parent = ancestor.parent.as_deref();
            }
        }
        Ok(())
    }

    /// Check hierarchy identity and aggregate capacity without writing.
    /// # Errors
    /// Refuses invalid, conflicting or oversized retained hierarchies.
    pub fn preflight(&self, doc: &Doc) -> Result<(), ActionError> {
        self.validate()?;
        let key = tree_key(&self.source);
        let encoded = serde_json::to_string(self).map_err(|_| refused())?;
        if let Some((value, _)) = doc.get(ROOT, &key)? {
            if value.as_str() != Some(encoded.as_str()) {
                return Err(refused());
            }
        } else {
            let retained = read_trees(doc, false)?;
            if retained.len() >= 128
                || retained
                    .iter()
                    .map(|tree| tree.nodes.len())
                    .sum::<usize>()
                    .saturating_add(self.nodes.len())
                    > crate::imports::MAX_BUNDLE_ENTRIES
            {
                return Err(refused());
            }
        }
        Ok(())
    }

    /// Retain an immutable hierarchy, accepting identical retries.
    /// # Errors
    /// Refuses invalid or conflicting hierarchies and failed document writes.
    pub fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        self.preflight(doc)?;
        let key = tree_key(&self.source);
        if doc.get(ROOT, &key)?.is_none() {
            doc.put(
                ROOT,
                key,
                serde_json::to_string(self).map_err(|_| refused())?,
            )?;
        }
        Ok(())
    }
}

fn source_id(source: &[u8; 32]) -> String {
    blake3::Hash::from(*source).to_hex().to_string()
}
fn tree_key(source: &[u8; 32]) -> String {
    format!("{TREE}{}", source_id(source))
}
fn remove_key(source: &[u8; 32], id: &str) -> String {
    format!(
        "{REMOVED}{}:{}",
        source_id(source),
        blake3::hash(id.as_bytes()).to_hex()
    )
}

/// Tombstone a retained folder or placement without changing source custody.
/// # Errors
/// Refuses unknown sources, unknown nodes and failed document writes.
pub fn remove(doc: &mut Doc, source: &[u8; 32], id: &str) -> Result<(), ActionError> {
    let Some((value, _)) = doc.get(ROOT, tree_key(source))? else {
        return Err(refused());
    };
    let tree: ImportedTree =
        serde_json::from_str(value.as_str().ok_or_else(refused)?).map_err(|_| refused())?;
    tree.validate()?;
    if tree.source != *source || !tree.nodes.iter().any(|node| node.id == id) {
        return Err(refused());
    }
    doc.put(ROOT, remove_key(source, id), true)?;
    Ok(())
}

/// Canonical complete hierarchy projection. Descendants of removed folders are hidden.
/// # Errors
/// Invalid persisted hierarchy is an error, never replaced with an empty library.
pub fn from_doc(doc: &Doc) -> Result<Vec<ImportedTree>, ActionError> {
    read_trees(doc, true)
}

fn read_trees(doc: &Doc, visible: bool) -> Result<Vec<ImportedTree>, ActionError> {
    let mut trees = Vec::new();
    let mut retained = 0usize;
    for key in doc.keys(ROOT).filter(|key| key.starts_with(TREE)) {
        let Some((value, _)) = doc.get(ROOT, &key)? else {
            return Err(refused());
        };
        let mut tree: ImportedTree =
            serde_json::from_str(value.as_str().ok_or_else(refused)?).map_err(|_| refused())?;
        tree.validate()?;
        retained = retained.saturating_add(tree.nodes.len());
        if retained > crate::imports::MAX_BUNDLE_ENTRIES || trees.len() >= 128 {
            return Err(refused());
        }
        if key != tree_key(&tree.source) {
            return Err(refused());
        }
        let mut hidden: BTreeSet<String> = tree
            .nodes
            .iter()
            .filter(|node| {
                doc.get(ROOT, remove_key(&tree.source, &node.id))
                    .ok()
                    .flatten()
                    .is_some()
            })
            .map(|node| node.id.clone())
            .collect();
        loop {
            let descendants: Vec<_> = tree
                .nodes
                .iter()
                .filter(|node| {
                    !hidden.contains(&node.id)
                        && node.parent.as_ref().is_some_and(|p| hidden.contains(p))
                })
                .map(|node| node.id.clone())
                .collect();
            if descendants.is_empty() {
                break;
            }
            hidden.extend(descendants);
        }
        if visible {
            tree.nodes.retain(|node| !hidden.contains(&node.id));
        }
        trees.push(tree);
    }
    Ok(trees)
}
