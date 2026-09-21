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
pub const QOS_PACKAGE: &str = "dev.qos.notes";
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

    /// Write the exact canonical custody shape produced by the deployed qOS
    /// Notes v1 code. Keep this test oracle independent of the compatibility
    /// implementation: the domain, raw record grammar, FNV storage key and
    /// qOS metadata/schema keys are all literal historical values.
    pub fn write_qos_canonical_v1(&self, imported: bool) -> HistoricalQosCustody {
        let canonical_source = self.source.canonicalize().unwrap();
        let mut hash = blake3::Hasher::new();
        hash.update(b"qos-notes-legacy-source-v1\0");
        hash.update(canonical_source.as_os_str().as_encoded_bytes());
        let source_key = hash.finalize().to_hex().to_string();
        let record = format!("derived:notes-legacy-v1@{source_key}");
        let storage_key = historical_storage_key(&record);
        std::fs::create_dir_all(&self.state_root).unwrap();
        let state_root = self.state_root.canonicalize().unwrap();
        let directory =
            app_state::DocStore::room_record_dir_at(&state_root, QOS_PACKAGE, ROOM, &storage_key)
                .unwrap();
        // Establish the real canonical directory, actor and exclusive gate,
        // then release it before publishing the historical document bytes.
        drop(app_state::DocStore::open_canonical_at(QOS_PACKAGE, &directory).unwrap());

        let mut doc = AutoCommit::new();
        doc.put(ROOT, "qos:schema_version", 1_u64).unwrap();
        let meta = doc.put_object(ROOT, "qos:record", ObjType::Map).unwrap();
        for (field, value) in [
            ("id", record.as_str()),
            ("kind", "notes.notebook"),
            ("home", ROOM),
            ("room", ROOM),
        ] {
            doc.put(&meta, field, value).unwrap();
        }
        doc.put_object(&meta, "refs", ObjType::List).unwrap();
        let pages = doc.put_object(ROOT, "pages", ObjType::Map).unwrap();
        doc.put_object(ROOT, "tags", ObjType::Map).unwrap();
        doc.put_object(ROOT, "tag_assignments", ObjType::Map)
            .unwrap();
        doc.put(ROOT, "notes_schema_version", 1_u64).unwrap();
        if imported {
            let page_id = Uuid::from_u128(0x35ed_0001).to_string();
            let page = doc
                .put_object(&pages, page_id.as_str(), ObjType::Map)
                .unwrap();
            let title = doc.put_object(&page, "title", ObjType::Text).unwrap();
            doc.splice_text(&title, 0, 0, "Historical qOS page")
                .unwrap();
            doc.put(&page, "parent", "").unwrap();
            doc.put(&page, "order", 0_i64).unwrap();
            doc.put_object(&page, "blocks", ObjType::List).unwrap();
            let receipt = doc
                .put_object(ROOT, "notes_import_v1", ObjType::Map)
                .unwrap();
            for (field, value) in [
                ("source_key", source_key.as_str()),
                ("snapshot_digest", "42".repeat(32).as_str()),
                ("destination_id", record.as_str()),
                ("format", "workspace-json-markdown-v1"),
            ] {
                doc.put(&receipt, field, value).unwrap();
            }
            doc.put(&receipt, "version", 1_u64).unwrap();
        }
        let document_path = directory.join(app_state::DOCUMENT_FILE);
        std::fs::write(&document_path, doc.save()).unwrap();
        let marker = serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "state_root": state_root,
            "workspace": ROOM,
            "package": QOS_PACKAGE,
            "record": record,
        }))
        .unwrap();
        let marker_path = self.source.join(".notes-canonical-v1");
        std::fs::write(&marker_path, &marker).unwrap();
        HistoricalQosCustody {
            source_key,
            record,
            document_path,
            marker_path,
            marker,
        }
    }
}

pub struct HistoricalQosCustody {
    pub source_key: String,
    pub record: String,
    pub document_path: PathBuf,
    pub marker_path: PathBuf,
    pub marker: Vec<u8>,
}

fn historical_storage_key(record: &str) -> String {
    let mut prefix = String::with_capacity(48);
    for c in record.chars() {
        if prefix.len() >= 40 {
            break;
        }
        if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
            prefix.push(c);
        } else {
            prefix.push('_');
        }
    }
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in record.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{prefix}-{hash:016x}")
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
