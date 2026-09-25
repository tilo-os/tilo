use ui::prelude::*;

/// The projects of the workspace, by stable tree node id.
const PROJECTS: [u64; 3] = [3, 4, 6];

/// One project's durable fields. Collaborative text: concurrent typing merges.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Project {
    pub title: String,
    pub notes: String,
    pub shared: bool,
}

/// The app's state: every project's fields, keyed by project. Stored, synced
/// and undoable; which project is selected is view state, not part of it.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Workspace {
    pub projects: Collection<Project>,
}

fn project_key(id: u64) -> Id {
    Id::from_raw(id.to_string())
}

impl Workspace {
    /// The project `id`, as stored (empty until first edited).
    pub fn project(&self, id: u64) -> Option<&Project> {
        self.projects.get(&project_key(id))
    }

    fn edit(&mut self, id: u64) -> Result<&mut Project, &'static str> {
        if !PROJECTS.contains(&id) {
            return Err("no such project");
        }
        let key = project_key(id);
        if !self.projects.contains(&key) {
            self.projects.push(key.clone(), Project::default());
        }
        self.projects.get_mut(&key).ok_or("no such project")
    }
}

#[resource(kind = "generated.workspace-item", label = "Workspace item")]
impl Workspace {
    /// Set a project's title.
    #[action]
    pub fn set_title(
        &mut self,
        #[param("Stable project id")] id: u64,
        #[param("Project title")] title: String,
    ) -> Result<(), &'static str> {
        self.edit(id)?.title = title;
        Ok(())
    }

    /// Set a project's notes.
    #[action]
    pub fn set_notes(
        &mut self,
        #[param("Stable project id")] id: u64,
        #[param("Project notes")] notes: String,
    ) -> Result<(), &'static str> {
        self.edit(id)?.notes = notes;
        Ok(())
    }

    /// Share a project.
    #[action]
    pub fn share(&mut self, #[param("Stable project id")] id: u64) -> Result<(), &'static str> {
        self.edit(id)?.shared = true;
        Ok(())
    }
}

fn item_label(id: u64) -> String {
    let message = match id {
        3 => "workspace-desktop-app",
        4 => "workspace-design-system",
        6 => "workspace-first-prototype",
        _ => "workspace-unknown-item",
    };
    ui::i18n::I18nManager::lookup_plain(message)
}

/// The detail pane of one project. Built again, with its own bindings, each
/// time the selection changes.
fn detail_pane(cx: &ViewCx<Workspace>, id: u64) -> Element {
    let title = cx
        .input(
            move |workspace| {
                workspace
                    .project(id)
                    .map(|project| project.title.clone())
                    .unwrap_or_default()
            },
            Workspace::SET_TITLE.map(move |title| (id, title)),
        )
        .with_id("workspace-title-input")
        .with_placeholder("workspace-title-placeholder");
    let notes = cx
        .input(
            move |workspace| {
                workspace
                    .project(id)
                    .map(|project| project.notes.clone())
                    .unwrap_or_default()
            },
            Workspace::SET_NOTES.map(move |notes| (id, notes)),
        )
        .with_id("workspace-notes-input")
        .with_placeholder("workspace-notes-placeholder")
        .multiline(true);
    let share = cx.actions().bind_invocation(
        Button::new("workspace-share", "workspace-share", None),
        Workspace::SHARE.with(id),
    );
    Box::new(ui::column![
        Text::new(&item_label(id))
            .with_id("workspace-selected-item")
            .with_role(TextRole::Title),
        labeled_field("workspace-title-label", title),
        labeled_field("workspace-notes-label", notes),
        Text::new("workspace-preview-heading").with_role(TextRole::Heading),
        cx.text(move |workspace| workspace
            .project(id)
            .map(|project| project.title.clone())
            .unwrap_or_default())
            .with_id("workspace-title-preview")
            .with_role(TextRole::Heading),
        cx.text(move |workspace| workspace
            .project(id)
            .map(|project| project.notes.clone())
            .unwrap_or_default())
            .with_id("workspace-notes-preview"),
        share,
    ])
}

/// A responsive professional master-detail shell with durable per-project
/// editing.
///
/// Wide windows show navigation and detail together; compact windows switch
/// between full-width panes. The selection is typed view state; the detail
/// pane follows it through `cx.switch`, and its fields bind to the selected
/// project in the resource.
pub struct App;

impl ResourceApp for App {
    type Resource = Workspace;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;
    const SHOT_VIEWPORT: (u32, u32) = (1000, 640);

    fn view(cx: &mut ViewCx<Workspace>) -> Element {
        crate::install_catalog();
        let selected = cx.local(3_u64);
        let narrow_screen = cx.local(NarrowScreen::Content);
        let selected_from_tree = selected.clone();
        let screen_from_tree = narrow_screen.clone();
        let label = |message| ui::i18n::I18nManager::lookup_plain(message);
        let tree = TreeView::new(TreeNode::branch(
            1,
            label("workspace-root"),
            (),
            vec![
                TreeNode::branch(
                    2,
                    label("workspace-projects"),
                    (),
                    vec![
                        TreeNode::leaf(3, label("workspace-desktop-app"), ()),
                        TreeNode::leaf(4, label("workspace-design-system"), ()),
                    ],
                ),
                TreeNode::branch(
                    5,
                    label("workspace-archive"),
                    (),
                    vec![TreeNode::leaf(6, label("workspace-first-prototype"), ())],
                ),
            ],
        ))
        .with_id("workspace-tree")
        .with_expanded([1, 2])
        .with_selected(Some(3))
        .on_select(move |id| {
            if PROJECTS.contains(&id) {
                selected_from_tree.set(id);
            }
            screen_from_tree.set(NarrowScreen::Content);
        })
        .with_viewport(560.0, 36.0)
        .with_accessible_name("workspace-tree-name");
        let master = ui::column![
            Text::new("workspace-navigation-heading").with_role(TextRole::Heading),
            tree,
        ]
        .with_fill_remaining_height();
        let detail = cx.switch(&selected, |cx, id| detail_pane(cx, *id));

        Box::new(
            AppShell::new(Box::new(master), detail)
                .with_id("workspace-shell")
                .with_back_accessible_name("workspace-back")
                .with_narrow_screen_state(&narrow_screen),
        )
    }
}
