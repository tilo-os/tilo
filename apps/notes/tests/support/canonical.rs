//! One canonical notebook resource, written with the notebook's own actions,
//! and the storage owner that holds it. No app/widget construction.
use notes::{
    PageId, Workspace,
    actions::{self, Action},
    model::{PageMeta, TagAssignment, TagMeta, WorkspaceData},
};
use std::{cell::RefCell, collections::HashMap, path::PathBuf};
use ui::{
    permissions::{MemberId, PermissionContext, Role},
    resource::{DocumentCheckout, DocumentId, ResourceOwners, ResourceStore, ResourceType},
};
use uuid::Uuid;

pub const PACKAGE: &str = "so.tilo.notes";
pub const WORKSPACE: &str = "fixture-workspace";
pub const LAUNCH_BODY: &str = "# Heading **one**\n\n## Heading *two*\n\n### Heading three\n\né **bold** *ital* `λ` [[Projects]] [[Empty]]\n\n- bullet\n\n  - nested\n\n1. numbered\n\n  1. nested number\n\n- [ ] todo\n\n  - [x] done\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n\n```\nplain code\n```\n\n---\n";

/// A notebook of three notes — Projects, its child Launch, and an Empty
/// one — with one tag, and the bodies [`notebook`](super::notebook) writes.
pub struct NotebookFixture {
    pub store: ResourceStore,
    pub owners: ResourceOwners,
    pub state_root: PathBuf,
    pub root: PageId,
    pub child: PageId,
    pub empty: PageId,
    pub tag: Uuid,
    pub author: MemberId,
    pub authority: ui::permissions::SharedPermissions,
    pub workspace: Workspace,
    bodies: RefCell<HashMap<PageId, String>>,
    _temp: tempfile::TempDir,
}
impl NotebookFixture {
    /// A notebook with nothing in it yet: what a workspace's Notes window
    /// opens on before the person has made a note. `root`, `child` and `empty`
    /// name no note this notebook holds.
    pub fn empty() -> Self {
        let mut fixture = Self::rich();
        fixture.workspace = Workspace::default();
        fixture.bodies.borrow_mut().clear();
        fixture
    }

    pub fn rich() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("state");
        let mut owners = ResourceOwners::new(state_root.clone());
        let store = owners.open(PACKAGE, WORKSPACE).unwrap();
        let root = Uuid::from_u128(0x1001);
        let child = Uuid::from_u128(0x1002);
        let empty = Uuid::from_u128(0x1003);
        let tag = Uuid::from_u128(0x2001);
        let workspace = Workspace {
            data: WorkspaceData {
                pages: vec![
                    PageMeta {
                        id: root,
                        title: "Projects".into(),
                        parent: None,
                    },
                    PageMeta {
                        id: child,
                        title: "Launch".into(),
                        parent: Some(root),
                    },
                    PageMeta {
                        id: empty,
                        title: "Empty".into(),
                        parent: None,
                    },
                ],
                tags: vec![TagMeta {
                    id: tag,
                    name: "Work".into(),
                }],
                tag_assignments: vec![TagAssignment { page: child, tag }],
                selected: Some(child),
            },
            ..Workspace::default()
        };
        let bodies = HashMap::from([
            (root, "# Project index\n\nSee [[Launch]].\n".to_owned()),
            (child, LAUNCH_BODY.to_owned()),
        ]);
        Self {
            store,
            owners,
            state_root,
            root,
            child,
            empty,
            tag,
            author: MemberId::person("notes-fixture-author"),
            authority: ui::permissions::SharedPermissions::new(
                PermissionContext::member(
                    &ui::permissions::MemberId::person("notes-fixture-author"),
                    Role::Owner,
                )
                .at_unix_time(100),
            ),
            workspace,
            bodies: RefCell::new(bodies),
            _temp: temp,
        }
    }

    /// Give `page` the Markdown body `bytes` when the notebook is written.
    pub fn write_page(&self, page: PageId, bytes: &[u8]) {
        self.bodies
            .borrow_mut()
            .insert(page, String::from_utf8(bytes.to_vec()).unwrap());
    }

    /// The notebook's own actions: its tags.
    pub fn tag_actions(&self) -> Vec<actions::TagAction> {
        self.workspace
            .tags()
            .iter()
            .map(|tag| actions::new_tag_action(tag.id, &tag.name))
            .collect()
    }

    /// The notes' actions: every note, its body and its tags.
    pub fn actions(&self) -> Vec<Action> {
        let titles = self
            .workspace
            .pages()
            .iter()
            .map(|page| (page.title.clone(), page.id))
            .collect::<HashMap<_, _>>();
        let mut out = self
            .workspace
            .pages()
            .iter()
            .map(|page| actions::create_page_action(page.id, page.parent, &page.title))
            .collect::<Vec<_>>();
        for page in self.workspace.pages() {
            let Some(body) = self.bodies.borrow().get(&page.id).cloned() else {
                continue;
            };
            let blocks = notes::MainPanel::seed_blocks(&body, |title| titles.get(title).copied());
            out.push(actions::write_body_action(page.id, &blocks));
        }
        for assignment in &self.workspace.data.tag_assignments {
            out.push(actions::set_tag_assignment_action(
                assignment.page,
                assignment.tag,
                true,
            ));
        }
        out
    }

    /// Write the notebook into its resource, the workspace's default, and its
    /// notes into the first part of the notes beside it, once, and make them
    /// durable.
    pub fn write_notebook(&self) -> DocumentCheckout {
        let checkout = DocumentCheckout::new(
            DocumentId::workspace_default(),
            ResourceType::new("notes.notebook"),
        );
        let tags = self.tag_actions();
        self.store
            .reserve_resource(&checkout)
            .unwrap()
            .initialize(|doc| {
                <notes::Notebook as ui::tilo_resource::Resource>::initialize(doc)
                    .map_err(|error| error.to_string())?;
                for action in &tags {
                    tilo_actions::apply_local(doc, action, actions::encode_tag(action))
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            })
            .unwrap();
        let pages = self.actions();
        // The notes are written as a window writes them: each read against
        // the part and the notebook first (its place among its siblings,
        // the tags it names).
        let notebook = self
            .store
            .read(&checkout.id, notes::read::notebook)
            .unwrap_or_default();
        if !pages.is_empty() {
            let part = DocumentCheckout::new(
                notes::part::chain().part(0),
                ResourceType::new(<notes::PagePart as ui::tilo_resource::Resource>::KIND),
            );
            self.store
                .reserve_resource(&part)
                .unwrap()
                .initialize(|doc| {
                    <notes::PagePart as ui::tilo_resource::Resource>::initialize(doc)
                        .map_err(|error| error.to_string())?;
                    for action in &pages {
                        notes::read::apply_in_part(doc, &notebook, action)?;
                    }
                    Ok(())
                })
                .unwrap();
        }
        self.store.try_flush().unwrap();
        checkout
    }

    /// The record the fixture's notes are kept in: the first part of the
    /// notes.
    #[allow(
        clippy::unused_self,
        reason = "read beside the fixture's other records, as one of them"
    )]
    pub fn part(&self) -> DocumentId {
        notes::part::chain().part(0)
    }

    pub fn reopen(mut self) -> Self {
        drop(self.store);
        self.store = self.owners.open(PACKAGE, WORKSPACE).unwrap();
        self
    }
}
