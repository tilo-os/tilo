//! Reviewed bookmark hierarchy. URL identity and placement identity are distinct.
//! A complete source tree is immutable custody; removals are separate tombstones,
//! so retrying its import cannot resurrect a removed folder or placement.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tilo_actions::ActionError;

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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedTree {
    pub source: [u8; 32],
    pub nodes: Vec<ImportedNode>,
}

// A retained tree is immutable custody of what an outside source held: up to
// `MAX_BUNDLE_ENTRIES` nodes, never edited once kept, compared whole when an
// import is retried, and removed from view only by tombstones kept beside it.
// It is stored as one value, so a library of imported folders costs one
// write per source, not one per node.
tilo_resource::json_model!(ImportedTree);

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
}

/// The id an imported tree is kept under: its source, in hex.
#[must_use]
pub fn source_id(source: &[u8; 32]) -> String {
    blake3::Hash::from(*source).to_hex().to_string()
}

/// The id a removed folder or placement of the tree from `source` is
/// tombstoned under.
#[must_use]
pub fn node_key(source: &[u8; 32], id: &str) -> String {
    format!(
        "{}:{}",
        source_id(source),
        blake3::hash(id.as_bytes()).to_hex()
    )
}

/// The nodes of `tree` that `removed` hides: the ones tombstoned and every
/// node beneath a hidden folder.
#[must_use]
pub fn hidden(tree: &ImportedTree, removed: impl Fn(&str) -> bool) -> BTreeSet<String> {
    let mut hidden: BTreeSet<String> = tree
        .nodes
        .iter()
        .filter(|node| removed(&node_key(&tree.source, &node.id)))
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
            return hidden;
        }
        hidden.extend(descendants);
    }
}
