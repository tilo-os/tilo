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
        if self.nodes.len() > crate::continuity::MAX_BUNDLE_ENTRIES
            || serde_json::to_vec(self).map_err(|_| refused())?.len()
                > crate::continuity::MAX_BUNDLE_BYTES
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
                && crate::actions::normalized_bookmark_url(url).as_deref() != Some(url)
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

    pub(crate) fn preflight(&self, doc: &Doc) -> Result<(), ActionError> {
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
                    > crate::continuity::MAX_BUNDLE_ENTRIES
            {
                return Err(refused());
            }
        }
        Ok(())
    }

    pub(crate) fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
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

pub(crate) fn remove(doc: &mut Doc, source: &[u8; 32], id: &str) -> Result<(), ActionError> {
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
        if retained > crate::continuity::MAX_BUNDLE_ENTRIES || trees.len() >= 128 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::Action;
    use ui::tilo_resource::Resource as _;

    fn find<'a>(
        rows: &'a [tilo_actions::Resource],
        id: &str,
    ) -> Option<&'a tilo_actions::Resource> {
        rows.iter().find_map(|row| {
            if row.id.as_str() == id {
                Some(row)
            } else {
                find(&row.children, id)
            }
        })
    }

    /// The production Browser resource/surface, mounted live with an explicitly
    /// lent deterministic engine instead of starting a native browser process.
    struct TestBrowser;
    impl ui::framework::ResourceApp for TestBrowser {
        type Resource = crate::resource::Browser;
        const ID: &'static str = "so.tilo.browser";
        const TITLE: &'static str = "Browser";
        fn view(cx: &mut ui::framework::ViewCx<Self::Resource>) -> ui::widget::Element {
            let engine = cx
                .take_lent::<std::sync::Arc<ui::webview::test_engine::TestEngine>>()
                .unwrap();
            cx.surface(crate::BrowserWindow::new(
                crate::BrowserWidget::with_engine(engine, true),
                false,
            ))
        }
    }

    #[test]
    fn canonical_hierarchy_retry_and_removal_survive_reload_without_url_dedup() {
        let source = [17; 32];
        let folder = |id: &str, parent: Option<&str>, position| ImportedNode {
            id: id.into(),
            parent: parent.map(str::to_owned),
            position,
            title: id.into(),
            kind: NodeKind::Folder,
        };
        let bookmark = |id: &str, parent: &str| ImportedNode {
            id: id.into(),
            parent: Some(parent.into()),
            position: 0,
            title: id.into(),
            kind: NodeKind::Bookmark {
                url: "https://example.org/".into(),
            },
        };
        let tree = ImportedTree {
            source,
            nodes: vec![
                folder("root", None, 0),
                folder("one", Some("root"), 0),
                folder("two", Some("root"), 1),
                bookmark("first", "one"),
                bookmark("second", "two"),
            ],
        };
        let mut bundle =
            crate::continuity::ImportBundle::from_batch(crate::continuity::ImportBatch {
                source,
                bookmarks: vec![crate::continuity::BookmarkImport {
                    url: "https://example.org/".into(),
                    title: "Example".into(),
                }],
                history: vec![],
            })
            .unwrap();
        bundle.imported_tree = Some(tree.clone());
        let action = bundle.bookmark_action();
        let mut doc = Doc::new();
        action.apply(&mut doc).unwrap();
        let state = crate::personal_bookmarks::PersonalBookmarks::read(&doc).unwrap();
        assert_eq!(state.imported_trees, vec![tree]);
        assert_eq!(
            state.bookmarks.entries().len(),
            1,
            "legacy flat URL identity remains compatible"
        );
        let captured = &state.imported_trees[0];
        let children: Vec<_> = captured
            .nodes
            .iter()
            .filter(|node| node.parent.as_deref() == Some("root"))
            .collect();
        assert_eq!(children.len(), 2);
        let first = captured
            .nodes
            .iter()
            .find(|node| node.parent.as_deref() == Some(&children[0].id))
            .unwrap();
        let second = captured
            .nodes
            .iter()
            .find(|node| node.parent.as_deref() == Some(&children[1].id))
            .unwrap();
        let first = crate::personal_bookmarks::node_resource(captured, first);
        let second = crate::personal_bookmarks::node_resource(captured, second);
        assert_eq!(first.label, "first");
        assert_eq!(second.label, "second");
        assert!(
            first
                .actions
                .iter()
                .any(|action| action.id == "remove-personal-imported-node")
        );
        crate::continuity_bookmarks::PersonalLibraryEdit::RemoveImportedNode {
            source,
            id: "one".into(),
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();
        let mut reopened = Doc::load(&doc.save()).unwrap();
        action.apply(&mut reopened).unwrap();
        let restored = crate::personal_bookmarks::PersonalBookmarks::read(&reopened).unwrap();
        assert_eq!(
            restored.imported_trees[0]
                .nodes
                .iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>(),
            vec!["root", "two", "second"]
        );
        let mut conflicting = bundle;
        conflicting.imported_tree.as_mut().unwrap().nodes[0].title = "changed source".into();
        assert!(conflicting.bookmark_action().apply(&mut reopened).is_err());
        assert_eq!(
            crate::personal_bookmarks::PersonalBookmarks::read(&reopened)
                .unwrap()
                .imported_trees,
            restored.imported_trees
        );
    }

    #[test]
    fn published_placement_opens_actual_test_engine_and_removes_only_that_placement() {
        let source = [51; 32];
        let mut bundle =
            crate::continuity::ImportBundle::from_batch(crate::continuity::ImportBatch {
                source,
                bookmarks: vec![crate::continuity::BookmarkImport {
                    url: "https://example.org/migrated".into(),
                    title: "Migrated".into(),
                }],
                history: vec![],
            })
            .unwrap();
        bundle.imported_tree = Some(ImportedTree {
            source,
            nodes: vec![
                ImportedNode {
                    id: "root".into(),
                    parent: None,
                    position: 0,
                    title: "Imported toolbar".into(),
                    kind: NodeKind::Folder,
                },
                ImportedNode {
                    id: "placement".into(),
                    parent: Some("root".into()),
                    position: 0,
                    title: "Migrated page".into(),
                    kind: NodeKind::Bookmark {
                        url: "https://example.org/migrated".into(),
                    },
                },
            ],
        });
        let engine = ui::webview::test_engine::TestEngine::new();
        let mut app = ui::framework::testing::TestApp::<TestBrowser>::for_person_with_lent(
            &"19".repeat(32),
            engine.clone(),
        )
        .unwrap();
        app.call_on(
            crate::personal_bookmarks::SCOPE,
            "import-personal-bookmarks",
            serde_json::json!({"bundle":serde_json::to_string(&bundle).unwrap()}),
        )
        .unwrap();
        app.settle();
        let record = app
            .person::<crate::personal_bookmarks::PersonalBookmarks>()
            .unwrap();
        let state = record.get();
        let tree = &state.imported_trees[0];
        let leaf = crate::personal_bookmarks::node_resource(
            tree,
            tree.nodes
                .iter()
                .find(|node| node.id == "placement")
                .unwrap(),
        );
        app.call_on(
            &crate::personal_bookmarks::node_id(&source, "root"),
            "show-personal-bookmark-folder",
            serde_json::json!({}),
        )
        .unwrap();
        app.settle();
        assert!(leaf.actions.iter().any(|a| a.id == "open"));
        let id = leaf.id.as_str().to_owned();
        let publication = app.harness().resources();
        let published = find(&publication, &id).unwrap_or_else(|| {
            panic!(
                "Imported placement missing from actual framework publication: {}",
                publication
                    .iter()
                    .map(|row| row.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        assert!(
            published.actions.iter().any(|action| action.id == "open"),
            "Placement lacks open in actual framework publication: {}",
            published
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        app.call_on(&id, "open", serde_json::json!({})).unwrap();
        app.settle();
        assert_eq!(
            engine.current_uri().as_deref(),
            Some("https://example.org/migrated")
        );
        app.call_on(&id, "remove-personal-imported-node", serde_json::json!({}))
            .unwrap();
        app.settle();
        assert_eq!(record.get().imported_trees[0].nodes.len(), 1);
        assert_eq!(
            record.get().bookmarks.entries().len(),
            1,
            "removing one placement must not erase independent flat URL bookmark"
        );
        app.call_on(
            crate::personal_bookmarks::SCOPE,
            "import-personal-bookmarks",
            serde_json::json!({"bundle":serde_json::to_string(&bundle).unwrap()}),
        )
        .unwrap();
        app.settle();
        assert_eq!(record.get().imported_trees[0].nodes.len(), 1);
    }

    #[test]
    fn orphan_cycle_duplicate_order_and_corrupt_state_refuse_before_projection() {
        let node = ImportedNode {
            id: "root".into(),
            parent: None,
            position: 0,
            title: "Root".into(),
            kind: NodeKind::Folder,
        };
        let valid = ImportedTree {
            source: [23; 32],
            nodes: vec![node.clone()],
        };
        let mut invalid = valid.clone();
        invalid.nodes[0].parent = Some("missing".into());
        assert!(invalid.validate().is_err());
        invalid.nodes[0].parent = Some("root".into());
        assert!(invalid.validate().is_err());
        invalid = valid.clone();
        let mut duplicate = node;
        duplicate.id = "other".into();
        invalid.nodes.push(duplicate);
        assert!(invalid.validate().is_err());
        let mut doc = Doc::new();
        doc.put(ROOT, tree_key(&valid.source), "{}").unwrap();
        assert!(crate::personal_bookmarks::PersonalBookmarks::read(&doc).is_err());
    }
}
