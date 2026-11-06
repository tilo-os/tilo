//! The project explorer: the toolkit's list, its rows standing as a tree, over
//! a snapshot of the [`Project`], in the shell's sidebar column.
//!
//! The list owns rows, keyboard navigation, disclosure, selection, hover,
//! scrolling and semantics. This module owns only what is specific to Code:
//! the indexed rows it reads (folders first, then alphabetical, the explorer
//! convention), each row's file-type icon, and the activation and expansion
//! signals the app applies to the session.
//!
//! The session stays the one truth: the app shows the project and the file
//! on screen through an [`ExplorerFeed`], which the list takes up at its next
//! sync, and what the person does in the list comes back as
//! [`ExplorerEvent`]s.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use ui::icons::{Icon, icon_for_path};
use ui::widget::{
    Element, EmptyState, ListRow, Manual, TreeMarks, VecListModel, VirtualList, When,
};
use vello::kurbo::Rect;

use crate::model::{NodeId as ProjectId, NodeKind, Project};

/// What the user asked of the explorer, for the app to apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplorerEvent {
    /// Enter, a click or the accessibility `Activate` verb on a row: open a
    /// file, or toggle a folder.
    Activate(ProjectId),
    /// A folder was expanded (`true`) or collapsed.
    Expand(ProjectId, bool),
    /// The no-project state's Open folder.
    OpenFolder,
}

/// The explorer widget type the app hosts.
type Explorer = VirtualList<Rows>;

/// The explorer's rows, keyed by the project node each stands for.
type Rows = VecListModel<Row, ProjectId>;

/// Queue the explorer's callbacks push into and the app drains each frame.
pub type ExplorerEvents = Rc<RefCell<Vec<ExplorerEvent>>>;

/// One row of the explorer: a file or folder of the project, and the row it
/// stands under.
#[derive(Clone)]
struct Row {
    id: ProjectId,
    label: String,
    parent: Option<ProjectId>,
    folder: bool,
    /// Whether the session holds the folder open: its subtasks show.
    open: bool,
    path: PathBuf,
}

/// The project as the explorer lists it: folders before files, each by name,
/// depth first, the root left out.
fn rows(project: &Project) -> Vec<Row> {
    fn walk(project: &Project, parent: ProjectId, rows: &mut Vec<Row>) {
        let Some(node) = project.get(parent) else {
            return;
        };
        let NodeKind::Folder { children, .. } = &node.kind else {
            return;
        };
        let mut listed = children
            .iter()
            .filter_map(|id| project.get(*id))
            .collect::<Vec<_>>();
        listed.sort_by(|a, b| {
            b.is_folder()
                .cmp(&a.is_folder())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        for child in listed {
            let (folder, open) = match &child.kind {
                NodeKind::Folder { expanded, .. } => (true, *expanded),
                NodeKind::File { .. } => (false, false),
            };
            rows.push(Row {
                id: child.id,
                label: child.name.clone(),
                parent: (parent != project.root).then_some(parent),
                folder,
                open,
                path: child
                    .disk_path
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(&child.name)),
            });
            if folder {
                walk(project, child.id, rows);
            }
        }
    }
    let mut rows = Vec::new();
    walk(project, project.root, &mut rows);
    rows
}

/// The rows as the list reads them: each under the row it names, and every folder one that
/// takes subtasks whether or not they are read yet.
fn model(rows: Vec<Row>) -> Rows {
    // A row is one project node, so no two share a key: the list starts empty if one ever
    // did rather than the sidebar failing to show the project.
    Rows::new(rows, |row| row.id).map_or_else(
        |_| Rows::empty(),
        |model| {
            model
                .with_parents(|row| row.parent)
                .with_branches(|row| row.folder)
        },
    )
}

/// Build the explorer over `model`, wired to push its signals into `events`.
fn explorer(model: Rows, marks: &Rc<TreeMarks<ProjectId>>, events: &ExplorerEvents) -> Explorer {
    let activated = Rc::clone(events);
    let folding = Rc::clone(events);
    let marks = Rc::clone(marks);
    let opening = Rc::clone(&marks);
    let selecting = Rc::clone(&marks);
    VirtualList::new(
        model,
        ui::tokens::Metrics::BASE.panel_row_height,
        move |model, index, key| {
            let Some(row) = model.item(index) else {
                return Box::new(ListRow::new(key.to_string(), "")) as Element;
            };
            let icon = if row.folder && !opening.is_folded(key) {
                Icon::FolderOpen
            } else {
                icon_for_path(&row.path, row.folder)
            };
            Box::new(
                ListRow::new(key.to_string(), row.label.as_str())
                    .depth(selecting.depth(key))
                    .icon(icon),
            )
        },
    )
    .with_id("file_tree")
    .filling()
    .with_tree(marks)
    .with_semantic_key(ToString::to_string)
    // Choosing a row opens it: a click, Enter, a screen reader, a script
    // or an agent, through the list's published `select`.
    .on_view("file_tree", move |id| {
        activated.borrow_mut().push(ExplorerEvent::Activate(id));
    })
    .on_fold(move |id, folded| {
        folding
            .borrow_mut()
            .push(ExplorerEvent::Expand(*id, !folded));
    })
}

/// What the app shows in the explorer, for the list to take up at its next
/// sync: the project as it stands and the file on screen.
#[derive(Clone, Default)]
pub struct ExplorerFeed(Rc<RefCell<Shown>>);

#[derive(Default)]
struct Shown {
    revision: u64,
    project: Option<Project>,
    name: String,
    has_project: bool,
    current: Option<ProjectId>,
}

impl ExplorerFeed {
    /// Show `project` as it stands: its rows, its open folders and its name.
    pub fn show(&self, project: &Project) {
        let mut shown = self.0.borrow_mut();
        shown.project = Some(project.clone());
        shown.name.clone_from(&project.name);
        shown.has_project = project.nodes.len() > 1;
        shown.revision += 1;
    }

    /// Mark the file on screen: it is the selection, so the open buffer is
    /// where keyboard browsing starts.
    pub fn set_current(&self, id: Option<ProjectId>) {
        let mut shown = self.0.borrow_mut();
        shown.current = id;
        shown.revision += 1;
    }

    /// Whether there is a project to list: the root alone is no project.
    #[must_use]
    pub fn has_project(&self) -> bool {
        self.0.borrow().has_project
    }

    /// The project's name, over its files.
    #[must_use]
    pub fn name(&self) -> String {
        self.0.borrow().name.clone()
    }
}

/// The explorer as the sidebar holds it: the list, taking up what its
/// [`ExplorerFeed`] shows at each sync.
struct FedExplorer {
    view: Explorer,
    marks: Rc<TreeMarks<ProjectId>>,
    feed: ExplorerFeed,
    seen: u64,
}

impl FedExplorer {
    fn pull(&mut self) -> bool {
        let mut shown = self.feed.0.borrow_mut();
        if shown.revision == self.seen {
            return false;
        }
        self.seen = shown.revision;
        if let Some(project) = shown.project.take() {
            let rows = rows(&project);
            // The session keeps which folders are open; the list shows the
            // rest folded away.
            for row in &rows {
                if row.folder {
                    self.marks.fold(&row.id, !row.open);
                }
            }
            self.view.update_model(|current| *current = model(rows));
        }
        match shown.current {
            Some(id) => self.view.selection_mut().select_only(id),
            None => self.view.selection_mut().clear(),
        }
        true
    }
}

impl ui::widget::Decorator for FedExplorer {
    type Inner = Explorer;

    fn inner(&self) -> &Explorer {
        &self.view
    }

    fn inner_mut(&mut self) -> &mut Explorer {
        &mut self.view
    }

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        self.pull();
        ui::widget::Widget::sync(&mut self.view, state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let pulled = self.pull();
        ui::widget::Widget::update(&mut self.view, state, path) || pulled
    }
}

/// The sidebar's column: the project's name over its files, or, with no
/// project open, the offer to open a folder.
pub fn sidebar(feed: &ExplorerFeed, events: &ExplorerEvents) -> Element {
    let marks = Rc::new(TreeMarks::default());
    let mut explorer = FedExplorer {
        view: explorer(model(Vec::new()), &marks, events),
        marks,
        feed: feed.clone(),
        seen: 0,
    };
    explorer.pull();
    let open = Rc::clone(events);
    let empty = EmptyState::new("No folder open")
        .with_icon(Icon::FolderOpen)
        .with_action("Open folder")
        .on_view(move || open.borrow_mut().push(ExplorerEvent::OpenFolder));
    let named = feed.clone();
    let project = Box::new(
        ui::widget::Text::dynamic(move || named.name())
            .with_id("code-explorer-project")
            .with_role(ui::TextRole::Caption),
    );
    let listed = feed.clone();
    let unlisted = feed.clone();
    Box::new(
        Manual::new(|ctx| {
            let size = ctx.available();
            let (w, h) = (f64::from(size.width), f64::from(size.height));
            let inset = f64::from(ui::tokens::Spacing::BASE.s3);
            let band = taffy::Size {
                width: ui::widget::MeasureConstraint::Exact(size.width),
                height: ui::widget::MeasureConstraint::Intrinsic,
            };
            let band = f64::from(ctx.measure(0, band)?.size.height);
            // A pass can hand the column no height at all: nothing then stands under the
            // name, and the placement below it takes no room.
            let top = (inset + band + inset).min(h);
            let name_bottom = (inset + band).min(top);
            ctx.place(
                0,
                Rect::new(
                    inset.min(w),
                    inset.min(top),
                    (w - inset).max(inset),
                    name_bottom,
                ),
            )?;
            let rest = Rect::new(0.0, top, w, h);
            ctx.place(1, rest)?;
            ctx.place(2, rest)?;
            Ok(size)
        })
        .child(project)
        .child(Box::new(When::new(Box::new(explorer), move || {
            listed.has_project()
        })))
        .child(Box::new(When::new(Box::new(empty), move || {
            !unlisted.has_project()
        }))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::ListModel;

    fn sample() -> Project {
        let mut project = Project::new("demo");
        let src = project.add_folder(project.root, "src");
        project.add_file(src, "main.rs", "fn main() {}");
        project.add_file(project.root, "README.md", "# demo");
        project.add_folder(project.root, "assets");
        project
    }

    /// Each row's label and how deep it stands under the top rows.
    fn listed(project: &Project) -> Vec<(String, u32)> {
        let rows = rows(project);
        let parents: std::collections::HashMap<_, _> =
            rows.iter().map(|row| (row.id, row.parent)).collect();
        rows.iter()
            .map(|row| {
                let mut depth = 0;
                let mut parent = row.parent;
                while let Some(above) = parent {
                    depth += 1;
                    parent = parents.get(&above).copied().flatten();
                }
                (row.label.clone(), depth)
            })
            .collect()
    }

    #[test]
    fn rows_stand_in_explorer_order_under_the_folder_they_are_in() {
        // Folders sort before files, then by name, and a folder's files follow it.
        assert_eq!(
            listed(&sample()),
            [
                ("assets".to_owned(), 0),
                ("src".to_owned(), 0),
                ("main.rs".to_owned(), 1),
                ("README.md".to_owned(), 0),
            ]
        );
    }

    #[test]
    fn a_folder_is_a_branch_before_its_files_are_read() {
        let project = sample();
        let rows = rows(&project);
        let model = model(rows);
        // `assets` has no files read yet, and still takes subtasks.
        let assets = (0..model.len())
            .find(|index| model.item(*index).is_some_and(|row| row.label == "assets"))
            .expect("the folder is listed");
        assert!(model.accepts_children(assets), "the folder opens");
        let readme = (0..model.len())
            .find(|index| {
                model
                    .item(*index)
                    .is_some_and(|row| row.label == "README.md")
            })
            .expect("the file is listed");
        assert!(!model.accepts_children(readme), "a file takes none");
    }
}
