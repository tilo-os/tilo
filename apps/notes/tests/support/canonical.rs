//! Explicit, fixed legacy bytes and one canonical storage owner. No app/widget construction.
use automerge::{AutoCommit, ObjType, ROOT, transaction::Transactable};
use notes::{
    PageId, Workspace,
    legacy_import::LegacySourceKey,
    model::{PageMeta, TagAssignment, TagMeta, WorkspaceData},
    storage,
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    rc::Rc,
};
use ui::{
    permissions::{Actor, PermissionContext, Role, UserId},
    record::{RecordCheckout, RecordOwners, RecordStore},
};
use uuid::Uuid;

pub const PACKAGE: &str = "so.tilo.notes";
pub const ROOM: &str = "fixture-room";
pub const LAUNCH_BODY: &str = "# Heading **one**\n\n## Heading *two*\n\n### Heading three\n\né **bold** *ital* `λ` [[Projects]] [[Empty]]\n\n- bullet\n\n  - nested\n\n1. numbered\n\n  1. nested number\n\n- [ ] todo\n\n  - [x] done\n\n```rust\nlet x = 1;\nlet y = 2;\n```\n\n```\nplain code\n```\n\n---\n";
pub struct LegacyFixture {
    pub records: RecordStore,
    pub owners: RecordOwners,
    pub source: PathBuf,
    pub state_root: PathBuf,
    pub key: LegacySourceKey,
    pub root: PageId,
    pub child: PageId,
    pub empty: PageId,
    pub tag: Uuid,
    pub author: UserId,
    pub authority: Rc<RefCell<PermissionContext>>,
    pub workspace: Workspace,
    pub temp: tempfile::TempDir,
}
impl LegacyFixture {
    pub fn rich() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
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
                        order: 0,
                    },
                    PageMeta {
                        id: child,
                        title: "Launch".into(),
                        parent: Some(root),
                        order: 0,
                    },
                    PageMeta {
                        id: empty,
                        title: "Empty".into(),
                        parent: None,
                        order: 1,
                    },
                ],
                tags: vec![TagMeta {
                    id: tag,
                    name: "Work".into(),
                }],
                tag_assignments: vec![TagAssignment { page: child, tag }],
                selected: Some(child),
            },
        };
        storage::ensure_layout(&source).unwrap();
        let fixture = Self {
            records,
            owners,
            source,
            state_root,
            key: LegacySourceKey::from_bytes([0x31; 32]),
            root,
            child,
            empty,
            tag,
            author: UserId("notes-fixture-author".into()),
            authority: Rc::new(RefCell::new(
                PermissionContext::from_capabilities(
                    Actor::person(Some(Role::Owner), "notes-fixture-author"),
                    vec![],
                )
                .at_unix_time(100),
            )),
            workspace,
            temp,
        };
        fixture.write_metadata();
        fixture.write_page(root, b"# Project index\n\nSee [[Launch]].\n");
        fixture.write_page(child, LAUNCH_BODY.as_bytes());
        fixture.write_page(empty, b"");
        fixture.write_page(Uuid::from_u128(0xffff), b"unreferenced markdown\n");
        std::fs::write(fixture.source.join("unreferenced.bin"), [0, 255, 17]).unwrap();
        fixture
    }
    pub fn write_metadata(&self) {
        std::fs::write(
            storage::workspace_file_path(&self.source),
            storage::encode_workspace(&self.workspace).unwrap(),
        )
        .unwrap();
    }
    pub fn write_page(&self, page: PageId, bytes: &[u8]) {
        std::fs::write(storage::page_file_path(&self.source, page), bytes).unwrap();
    }
    pub fn source_bytes(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        fn collect(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
            for entry in std::fs::read_dir(at).unwrap() {
                let entry = entry.unwrap();
                let kind = entry.file_type().unwrap();
                assert!(!kind.is_symlink());
                if kind.is_dir() {
                    collect(root, &entry.path(), out);
                } else {
                    assert!(kind.is_file());
                    out.insert(
                        entry.path().strip_prefix(root).unwrap().to_owned(),
                        std::fs::read(entry.path()).unwrap(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        collect(&self.source, &self.source, &mut out);
        out
    }
    pub fn document_path(&self, checkout: &RecordCheckout) -> PathBuf {
        app_state::DocStore::room_record_dir_at(
            &self.state_root,
            PACKAGE,
            ROOM,
            &checkout.id.storage_key(),
        )
        .unwrap()
        .join(app_state::DOCUMENT_FILE)
    }
    pub fn reopen(mut self) -> Self {
        drop(self.records);
        self.records = self.owners.open(PACKAGE, ROOM).unwrap();
        self
    }
    pub fn write_destination(&self, checkout: &RecordCheckout, bytes: &[u8]) {
        let path = self.document_path(checkout);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}
pub fn wrapped_json(json: &str) -> Vec<u8> {
    let mut doc = AutoCommit::new();
    let text = doc
        .put_object(ROOT, "workspace_json", ObjType::Text)
        .unwrap();
    doc.splice_text(&text, 0, 0, json).unwrap();
    doc.save()
}
pub fn partial_tail(mut doc: AutoCommit) -> Vec<u8> {
    let heads = doc.get_heads();
    let mut bytes = doc.save();
    doc.put(ROOT, "fixture_tail", "incomplete").unwrap();
    let tail = doc.save_after(&heads);
    bytes.extend_from_slice(&tail[..tail.len() / 2]);
    bytes
}
