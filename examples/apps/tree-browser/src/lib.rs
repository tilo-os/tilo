//! Tree browser: master-detail navigation that adapts to the window.
//!
//! A `TreeView` on the left picks a notebook section; the detail pane on the
//! right edits that section's notes. `NavShell` shows both panes side by
//! side in a wide window and one full-width pane at a time in a compact one,
//! with an accessible Back between them.
//!
//! The selection is view state (`cx.local`): never stored, synced or undone.
//! `cx.switch` rebuilds the detail pane, with its own bindings, whenever the
//! selection changes. The sections are the app's fixed navigation; what the
//! user writes in them is resource state and starts empty.

/// The application id.
pub const APP_ID: &str = "so.tilo.example.tree-browser";
/// The window and launcher title.
pub const APP_TITLE: &str = "Tree Browser";

pub mod locales;

use std::rc::Rc;

use ui::prelude::*;
use ui::selection::KeySelection;
use ui::widget::{ListRow, TreeMarks, VecListModel, VirtualList};

/// The sections that hold notes, by stable tree node id. The other nodes
/// only group them.
pub const SECTIONS: [u64; 3] = [3, 4, 6];

/// One section's durable fields. Collaborative text: concurrent typing merges.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Section {
    pub title: String,
    pub notes: String,
    pub shared: bool,
}

/// The app's state: every section's fields, keyed by section. Stored, synced
/// and undoable; which section is selected is view state, not part of it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Notebook {
    pub sections: Collection<Section>,
}

fn section_key(id: u64) -> Id {
    Id::from_raw(id.to_string())
}

impl Notebook {
    /// Every section, empty: what a new notebook holds, the same on every
    /// device that creates one.
    #[must_use]
    pub fn with_sections() -> Self {
        let mut notebook = Self::default();
        for id in SECTIONS {
            notebook.sections.push(section_key(id), Section::default());
        }
        notebook
    }

    /// The section `id`.
    pub fn section(&self, id: u64) -> Option<&Section> {
        self.sections.get(&section_key(id))
    }

    /// The paths of section `id`, where its fields are edited.
    #[must_use]
    pub fn section_paths(id: u64) -> SectionPaths<Notebook> {
        Self::paths().sections.item(&section_key(id))
    }
}

/// A section's title and notes are edited at their paths
/// ([`Notebook::section_paths`]); sharing it is a verb.
#[resource(kind = "example.notebook", initialize = Notebook::with_sections)]
impl Notebook {
    /// Share a section.
    #[action]
    pub fn share(&mut self, #[param("Stable section id")] id: u64) -> Result<(), &'static str> {
        self.sections
            .get_mut(&section_key(id))
            .ok_or("no such section")?
            .shared = true;
        Ok(())
    }
}

fn node_label(id: u64) -> String {
    crate::locales::CATALOG.t(match id {
        1 => "tree-notebook",
        2 => "tree-work",
        3 => "tree-projects",
        4 => "tree-meetings",
        5 => "tree-personal",
        6 => "tree-journal",
        _ => "tree-unknown",
    })
}

/// The detail pane of one section. Built again, with its own bindings, each
/// time the selection changes.
fn detail_pane(cx: &ViewCx<Notebook>, id: u64) -> Element {
    let section = Notebook::section_paths(id);
    let title = cx
        .input(section.title)
        .with_id("tree-title-input")
        .with_placeholder("tree-title-placeholder");
    let notes = cx
        .input(section.notes)
        .with_id("tree-notes-input")
        .with_placeholder("tree-notes-placeholder")
        .multiline(true);
    let share = cx.actions().bind_invocation(
        Button::new("tree-share", "tree-share"),
        Notebook::SHARE.with(id),
    );
    Box::new(ui::column![
        Text::new(&node_label(id))
            .with_id("tree-selected-item")
            .with_role(TextRole::Title),
        labeled_field("tree-title-label", title),
        labeled_field("tree-notes-label", notes),
        Text::new("tree-preview-heading").with_role(TextRole::Heading),
        cx.text(move |notebook| notebook
            .section(id)
            .map(|section| section.title.clone())
            .unwrap_or_default())
            .with_id("tree-title-preview")
            .with_role(TextRole::Heading),
        cx.text(move |notebook| notebook
            .section(id)
            .map(|section| section.notes.clone())
            .unwrap_or_default())
            .with_id("tree-notes-preview"),
        share,
    ])
}

pub struct App;

impl ResourceApp for App {
    type Resource = Notebook;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1000, 640)
    }

    fn view(cx: &mut ViewCx<Notebook>) -> Element {
        let selected = cx.local(3_u64);
        let narrow_screen = cx.local(NavPane::Content);
        let selected_from_tree = selected.clone();
        let screen_from_tree = narrow_screen.clone();
        let screen_from_activation = narrow_screen.clone();
        let chosen_from_tree = selected.clone();
        // The notebook's own sections, under the nodes that group them.
        let mut model = VecListModel::empty();
        for (id, parent, branch) in [
            (1, None, true),
            (2, Some(1), true),
            (3, Some(2), false),
            (4, Some(2), false),
            (5, Some(1), true),
            (6, Some(5), false),
        ] {
            let _ = model.push(id, (node_label(id), parent, branch));
        }
        let model = model.with_parents(|row| row.1).with_branches(|row| row.2);
        let marks = Rc::new(TreeMarks::<u64>::default());
        let row_marks = Rc::clone(&marks);
        let chosen_row = selected.with(|id| *id);
        let mut selection = KeySelection::new();
        selection.toggle(chosen_row);
        let tree =
            VirtualList::with_selection_delegate(model, 36.0, move |model, index, key, _| {
                let label = model
                    .item(index)
                    .map_or_else(String::new, |row| row.0.clone());
                Box::new(ListRow::new(key.to_string(), label).depth(row_marks.depth(key)))
            })
            .with_tree(marks)
            .with_id("section-tree")
            .with_semantic_key(|key: &u64| format!("node:{key}"))
            .with_accessible_name("tree-accessible-name")
            .with_selection(selection)
            .fitting_rows()
            .on_select(move |id, chosen| {
                if chosen && SECTIONS.contains(&id) {
                    selected_from_tree.set(id);
                }
                if chosen {
                    screen_from_tree.set(NavPane::Content);
                }
            })
            .on_activate(move |id| {
                if SECTIONS.contains(&id) {
                    chosen_from_tree.set(id);
                    // A compact window shows the section it was asked for: a row's
                    // activation is a choice as its press is.
                    screen_from_activation.set(NavPane::Content);
                }
            });
        let master = ui::column![
            Text::new("tree-navigation-heading").with_role(TextRole::Heading),
            tree,
        ]
        .with_fill_remaining_height();
        let detail = cx.switch(&selected, |cx, id| detail_pane(cx, *id));

        Box::new(
            NavShell::new()
                .content_kind(ui::widget::ContentKind::Page)
                .mode(NavMode::Push)
                .sidebar(Box::new(master))
                .content(detail)
                .with_id("notebook-shell")
                .with_pane_state(&narrow_screen),
        )
    }
}

ui::dev_reload_entry!(App);
