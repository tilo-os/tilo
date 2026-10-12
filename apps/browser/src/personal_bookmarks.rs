//! The person's own bookmark library. Importing it never edits the room's Browser record.
use crate::continuity::ImportBundle;
use crate::continuity_bookmarks::{PersonalLibraryEdit, bookmarks_from_doc};
use crate::continuity_library::Bookmarks;
use crate::imported_folders::{ImportedNode, ImportedTree, NodeKind};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, Doc};
use ui::tilo_resource::Effect as _;
use ui::tilo_resource::{ModelError, Resource};

pub const SCOPE: &str = "browser.bookmarks/personal";
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersonalBookmarkState {
    pub bookmarks: Bookmarks,
    pub imported_trees: Vec<ImportedTree>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersonalBookmarkAction {
    Import {
        bundle: ImportBundle,
    },
    Add {
        url: String,
        title: String,
        tag: String,
    },
    Remove {
        url: String,
        observed_tags: Vec<String>,
    },
    RemoveNode {
        source: [u8; 32],
        id: String,
    },
}
impl Action for PersonalBookmarkAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        let action = match self {
            Self::Import { bundle } => PersonalLibraryEdit::ImportBookmarkBundle {
                bundle: bundle.clone(),
            },
            Self::Add { url, title, tag } => PersonalLibraryEdit::AddBookmark {
                url: url.clone(),
                title: title.clone(),
                tag: tag.clone(),
            },
            Self::Remove { url, observed_tags } => PersonalLibraryEdit::RemoveBookmark {
                url: url.clone(),
                observed_tags: observed_tags.clone(),
            },
            Self::RemoveNode { source, id } => PersonalLibraryEdit::RemoveImportedNode {
                source: *source,
                id: id.clone(),
            },
        };
        action.apply(doc)
    }
}
pub struct PersonalBookmarks;
impl Resource for PersonalBookmarks {
    const KIND: &'static str = "browser.personal-bookmarks";
    const LABEL: &'static str = "Personal bookmarks";
    const ACTION_TYPE: &'static str = "browser.personal-bookmarks.action";
    type State = PersonalBookmarkState;
    type Action = PersonalBookmarkAction;
    fn read(doc: &Doc) -> Result<Self::State, ModelError> {
        Ok(PersonalBookmarkState {
            bookmarks: bookmarks_from_doc(doc).unwrap_or_default(),
            imported_trees: crate::imported_folders::from_doc(doc)
                .map_err(|_| ModelError::invalid("Invalid personal bookmark hierarchy"))?,
        })
    }
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
                .invoking(tilo_actions::ActionAddress::new(
                    flat_id(&entry.url),
                    "open",
                ))
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
                    .invoking(tilo_actions::ActionAddress::new(&id, "open"))
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
