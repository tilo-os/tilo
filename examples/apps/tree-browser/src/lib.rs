//! Tree browser: master-detail navigation that adapts to the window.
//!
//! A `TreeView` on the left picks a notebook section; the detail pane on the
//! right edits that section's notes. `AppShell` shows both panes side by
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

fn install_catalog() {
    locales::install();
}

use ui::i18n::I18nManager;
use ui::prelude::*;

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
    /// The section `id`, as stored (absent until first edited).
    pub fn section(&self, id: u64) -> Option<&Section> {
        self.sections.get(&section_key(id))
    }

    fn edit(&mut self, id: u64) -> Result<&mut Section, &'static str> {
        if !SECTIONS.contains(&id) {
            return Err("no such section");
        }
        let key = section_key(id);
        if !self.sections.contains(&key) {
            self.sections.push(key.clone(), Section::default());
        }
        self.sections.get_mut(&key).ok_or("no such section")
    }
}

#[resource(kind = "example.notebook", label = "Notebook")]
impl Notebook {
    /// Set a section's title.
    #[action]
    pub fn set_title(
        &mut self,
        #[param("Stable section id")] id: u64,
        #[param("Section title")] title: String,
    ) -> Result<(), &'static str> {
        self.edit(id)?.title = title;
        Ok(())
    }

    /// Set a section's notes.
    #[action]
    pub fn set_notes(
        &mut self,
        #[param("Stable section id")] id: u64,
        #[param("Section notes")] notes: String,
    ) -> Result<(), &'static str> {
        self.edit(id)?.notes = notes;
        Ok(())
    }

    /// Share a section.
    #[action]
    pub fn share(&mut self, #[param("Stable section id")] id: u64) -> Result<(), &'static str> {
        self.edit(id)?.shared = true;
        Ok(())
    }
}

fn node_label(id: u64) -> String {
    I18nManager::lookup_plain(match id {
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
    let title = cx
        .input(
            move |notebook| {
                notebook
                    .section(id)
                    .map(|section| section.title.clone())
                    .unwrap_or_default()
            },
            Notebook::SET_TITLE.map(move |title| (id, title)),
        )
        .with_id("tree-title-input")
        .with_placeholder("tree-title-placeholder");
    let notes = cx
        .input(
            move |notebook| {
                notebook
                    .section(id)
                    .map(|section| section.notes.clone())
                    .unwrap_or_default()
            },
            Notebook::SET_NOTES.map(move |notes| (id, notes)),
        )
        .with_id("tree-notes-input")
        .with_placeholder("tree-notes-placeholder")
        .multiline(true);
    let share = cx.actions().bind_invocation(
        Button::new("tree-share", "tree-share", None),
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
    const SHOT_VIEWPORT: (u32, u32) = (1000, 640);

    fn view(cx: &mut ViewCx<Notebook>) -> Element {
        install_catalog();
        let selected = cx.local(3_u64);
        let narrow_screen = cx.local(NarrowScreen::Content);
        let selected_from_tree = selected.clone();
        let screen_from_tree = narrow_screen.clone();
        let tree = TreeView::new(TreeNode::branch(
            1,
            node_label(1),
            (),
            vec![
                TreeNode::branch(
                    2,
                    node_label(2),
                    (),
                    vec![
                        TreeNode::leaf(3, node_label(3), ()),
                        TreeNode::leaf(4, node_label(4), ()),
                    ],
                ),
                TreeNode::branch(
                    5,
                    node_label(5),
                    (),
                    vec![TreeNode::leaf(6, node_label(6), ())],
                ),
            ],
        ))
        .with_id("section-tree")
        .with_expanded([1, 2])
        .with_selected(Some(3))
        .on_select(move |id| {
            if SECTIONS.contains(&id) {
                selected_from_tree.set(id);
            }
            screen_from_tree.set(NarrowScreen::Content);
        })
        .with_viewport(560.0, 36.0)
        .with_accessible_name("tree-accessible-name");
        let master = ui::column![
            Text::new("tree-navigation-heading").with_role(TextRole::Heading),
            tree,
        ]
        .with_fill_remaining_height();
        let detail = cx.switch(&selected, |cx, id| detail_pane(cx, *id));

        Box::new(
            AppShell::new(Box::new(master), detail)
                .with_id("notebook-shell")
                .with_back_accessible_name("tree-back")
                .with_narrow_screen_state(&narrow_screen),
        )
    }
}

ui::dev_reload_entry!(App);
