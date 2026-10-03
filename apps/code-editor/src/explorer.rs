//! The project explorer: the toolkit's [`TreeView`] over a snapshot of the
//! [`Project`], configured as a navigator sidebar.
//!
//! The tree owns rows, keyboard navigation, disclosure, selection, hover,
//! scrolling and semantics. This module owns only what is specific to Code:
//! the indexed model the view reads (folders first, then alphabetical, the
//! explorer convention), each row's file-type icon, and the activation and
//! expansion signals the app applies to the session.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use ui::icons::{Icon, icon_for_path};
use ui::theme::Theme;
use ui::widget::tree_view::{
    NodeId, TreeClickActivation, TreeModel, TreeRowDecoration, TreeRowInfo, TreeRowTone, TreeView,
};

use crate::model::{NodeId as ProjectId, NodeKind, Project};

/// What the user asked of the explorer, for the app to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplorerEvent {
    /// Enter, a click or the accessibility `Activate` verb on a row: open a
    /// file, or toggle a folder.
    Activate(ProjectId),
    /// A folder was expanded (`true`) or collapsed.
    Expand(ProjectId, bool),
}

/// The explorer widget type the app hosts.
pub type Explorer = TreeView<(), ProjectTree>;

/// Queue the explorer's callbacks push into and the app drains each frame.
pub type ExplorerEvents = Rc<RefCell<Vec<ExplorerEvent>>>;

struct Entry {
    label: String,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    folder: bool,
    path: PathBuf,
}

/// An indexed, read-only snapshot of the project hierarchy in explorer order.
/// Rebuilt whenever the project's shape changes (a folder's children load, a
/// collaborator adds a file); every query the view makes is a hash lookup.
pub struct ProjectTree {
    root: NodeId,
    entries: HashMap<NodeId, Entry>,
}

/// The tree key of a project node.
pub fn node_key(id: ProjectId) -> NodeId {
    NodeId::try_from(id).unwrap_or(NodeId::MAX)
}

fn project_id(id: NodeId) -> Option<ProjectId> {
    ProjectId::try_from(id).ok()
}

impl ProjectTree {
    pub fn from_project(project: &Project) -> Self {
        let mut entries = HashMap::with_capacity(project.nodes.len());
        for node in project.nodes.values() {
            let (folder, mut children) = match &node.kind {
                NodeKind::Folder { children, .. } => (true, children.clone()),
                NodeKind::File { .. } => (false, Vec::new()),
            };
            children.sort_by(|a, b| {
                let (Some(na), Some(nb)) = (project.get(*a), project.get(*b)) else {
                    return std::cmp::Ordering::Equal;
                };
                nb.is_folder()
                    .cmp(&na.is_folder())
                    .then_with(|| na.name.to_lowercase().cmp(&nb.name.to_lowercase()))
            });
            entries.insert(
                node_key(node.id),
                Entry {
                    label: node.name.clone(),
                    parent: node.parent.map(node_key),
                    children: children.into_iter().map(node_key).collect(),
                    folder,
                    path: node
                        .disk_path
                        .clone()
                        .unwrap_or_else(|| PathBuf::from(&node.name)),
                },
            );
        }
        Self {
            root: node_key(project.root),
            entries,
        }
    }

    /// The row's file-type icon: an open or closed folder, else the glyph
    /// the toolkit assigns the file's extension.
    fn icon(&self, id: NodeId, expanded: bool) -> Icon {
        match self.entries.get(&id) {
            Some(entry) if entry.folder && expanded => Icon::FolderOpen,
            Some(entry) => icon_for_path(&entry.path, entry.folder),
            None => Icon::File,
        }
    }
}

impl TreeModel for ProjectTree {
    fn root_id(&self) -> NodeId {
        self.root
    }

    fn contains(&self, id: NodeId) -> bool {
        self.entries.contains_key(&id)
    }

    fn label(&self, id: NodeId) -> Option<&str> {
        self.entries.get(&id).map(|entry| entry.label.as_str())
    }

    fn is_branch(&self, id: NodeId) -> bool {
        self.entries.get(&id).is_some_and(|entry| entry.folder)
    }

    fn parent_id(&self, id: NodeId) -> Option<NodeId> {
        self.entries.get(&id)?.parent
    }

    fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
        self.entries
            .get(&parent)?
            .children
            .iter()
            .position(|id| *id == child)
    }

    fn child_count(&self, parent: NodeId) -> usize {
        self.entries
            .get(&parent)
            .map_or(0, |entry| entry.children.len())
    }

    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
        self.entries.get(&parent)?.children.get(index).copied()
    }
}

/// Row decoration: a file-type icon in quiet ink, and labels in the
/// secondary tone except for the file on screen.
fn decorate(model: &ProjectTree, row: TreeRowInfo, theme: &Theme) -> TreeRowDecoration {
    let icon_color = theme.tokens.colors.text_dim;
    let tone = if row.current {
        TreeRowTone::Primary
    } else {
        TreeRowTone::Secondary
    };
    TreeRowDecoration::new()
        .icon(model.icon(row.id, row.expanded), icon_color)
        .tone(tone)
        .description(if row.is_branch { "Folder" } else { "File" })
}

/// Build the explorer over `project`, wired to push its signals into
/// `events`.
pub fn explorer(project: &Project, events: &ExplorerEvents) -> Explorer {
    let activated = Rc::clone(events);
    let expanded = Rc::clone(events);
    let mut view = TreeView::from_model(ProjectTree::from_project(project))
        .with_id("file_tree")
        .hide_root()
        .fill()
        .with_accessible_name(format!("{} files", project.name))
        .click_activation(TreeClickActivation::SingleClick)
        .selection_follows_focus(true)
        .with_row_decoration(decorate)
        .on_activate(move |id| {
            if let Some(id) = project_id(id) {
                activated.borrow_mut().push(ExplorerEvent::Activate(id));
            }
        })
        .on_expand(move |id, open| {
            if let Some(id) = project_id(id) {
                expanded.borrow_mut().push(ExplorerEvent::Expand(id, open));
            }
        });
    view.restore_expanded(expanded_folders(project));
    view
}

/// Re-read `project` into an existing explorer, keeping its selection,
/// focus and scroll anchored to the same rows.
pub fn refresh(view: &mut Explorer, project: &Project) {
    view.update_source(&[ui::widget::TreeModelChange::Reset], |model| {
        *model = ProjectTree::from_project(project);
    });
    view.restore_expanded(expanded_folders(project));
}

/// Mark the file on screen: it carries the current-row rail and is the
/// selection, so the open buffer is where keyboard browsing starts.
pub fn set_current(view: &mut Explorer, id: Option<ProjectId>) {
    let id = id.map(node_key);
    view.set_current(id);
    match id {
        Some(id) => view.selection_mut().select_only(id),
        None => view.selection_mut().clear(),
    }
}

fn expanded_folders(project: &Project) -> Vec<NodeId> {
    project
        .nodes
        .values()
        .filter(|node| node.id != project.root)
        .filter_map(|node| match node.kind {
            NodeKind::Folder { expanded: true, .. } => Some(node_key(node.id)),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Project {
        let mut project = Project::new("demo");
        let src = project.add_folder(project.root, "src");
        project.add_file(src, "main.rs", "fn main() {}");
        project.add_file(project.root, "README.md", "# demo");
        project.add_folder(project.root, "assets");
        project
    }

    #[test]
    fn project_tree_orders_rows_hides_root_and_assigns_icons() {
        // Folders sort before files, then by name.
        {
            let project = sample();
            let tree = ProjectTree::from_project(&project);
            let labels: Vec<_> = (0..tree.child_count(tree.root_id()))
                .filter_map(|index| tree.child_id(tree.root_id(), index))
                .filter_map(|id| tree.label(id))
                .collect();
            assert_eq!(labels, ["assets", "src", "README.md"]);
        }

        // The root is hidden and expansion comes from the project.
        {
            let mut project = sample();
            let src = project
                .nodes
                .values()
                .find(|node| node.name == "src")
                .map(|node| node.id)
                .unwrap();
            project.toggle_folder(src);
            let events = ExplorerEvents::default();
            let view = explorer(&project, &events);
            let rows: Vec<_> = view
                .visible_rows()
                .into_iter()
                .map(|(_, label, depth)| (label.to_owned(), depth))
                .collect();
            assert_eq!(
                rows,
                [
                    ("assets".to_owned(), 0),
                    ("src".to_owned(), 0),
                    ("main.rs".to_owned(), 1),
                    ("README.md".to_owned(), 0),
                ]
            );
        }

        // File rows get their type icon.
        {
            let project = sample();
            let tree = ProjectTree::from_project(&project);
            let find = |name: &str| {
                project
                    .nodes
                    .values()
                    .find(|node| node.name == name)
                    .map(|node| node_key(node.id))
                    .unwrap()
            };
            assert_eq!(tree.icon(find("README.md"), false), Icon::FileText);
            assert_eq!(tree.icon(find("src"), false), Icon::Folder);
            assert_eq!(tree.icon(find("src"), true), Icon::FolderOpen);
        }
    }
}
