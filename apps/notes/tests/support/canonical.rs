//! One canonical notebook record, written with the notebook's own actions,
//! and the storage owner that holds it. No app/widget construction.
use notes::{
    PageId, Workspace,
    actions::{self, Action},
    model::{PageMeta, TagAssignment, TagMeta, WorkspaceData},
};
use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc};
use ui::{
    permissions::{PermissionContext, Role, UserId},
    record::{RecordCheckout, RecordId, RecordKind, RecordOwners, RecordStore},
};
use uuid::Uuid;

pub const PACKAGE: &str = "so.tilo.notes";
pub const ROOM: &str = "fixture-room";
pub const LAUNCH_BODY: &str = "# Heading **one**\n\n## Heading *two*\n\n### Heading three\n\né **bold** *ital* `λ` [[Projects]] [[Empty]]\n\n- bullet\n\n  - nested\n\n1. numbered\n\n  1. nested number\n\n- [ ] todo\n\n  - [x] done\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n\n```\nplain code\n```\n\n---\n";

/// A notebook of three notes — Projects, its child Launch, and an Empty
/// one — with one tag, and the bodies [`notebook`](super::notebook) writes.
pub struct NotebookFixture {
    pub records: RecordStore,
    pub owners: RecordOwners,
    pub state_root: PathBuf,
    pub root: PageId,
    pub child: PageId,
    pub empty: PageId,
    pub tag: Uuid,
    pub author: UserId,
    pub authority: Rc<RefCell<PermissionContext>>,
    pub workspace: Workspace,
    bodies: RefCell<HashMap<PageId, String>>,
    _temp: tempfile::TempDir,
}
impl NotebookFixture {
    pub fn rich() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let state_root = temp.path().join("state");
        let mut owners = RecordOwners::new(state_root.clone());
        let records = owners.open(PACKAGE, ROOM).unwrap();
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
            records,
            owners,
            state_root,
            root,
            child,
            empty,
            tag,
            author: UserId("notes-fixture-author".into()),
            authority: Rc::new(RefCell::new(
                PermissionContext::member("notes-fixture-author", Role::Owner).at_unix_time(100),
            )),
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

    /// The actions that write the fixture's notebook.
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
        for tag in self.workspace.tags() {
            out.push(actions::new_tag_action(tag.id, &tag.name));
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

    /// Write the notebook into its record, the room's default, once, and
    /// make it durable.
    pub fn write_notebook(&self) -> RecordCheckout {
        let checkout =
            RecordCheckout::new(RecordId::room_default(), RecordKind::new("notes.notebook"));
        let actions = self.actions();
        self.records
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                <notes::Notebook as ui::tilo_resource::Resource>::initialize(doc)
                    .map_err(|error| error.to_string())?;
                for action in &actions {
                    tilo_actions::apply_local(doc, action, actions::encode(action))
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            })
            .unwrap();
        self.records.try_flush().unwrap();
        checkout
    }

    pub fn reopen(mut self) -> Self {
        drop(self.records);
        self.records = self.owners.open(PACKAGE, ROOM).unwrap();
        self
    }
}
