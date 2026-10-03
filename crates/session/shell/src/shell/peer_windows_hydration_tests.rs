//! A window continuing from another device brings the record it showed
//! there before it opens, and waits visibly while that record is on its way.
use super::*;
use crate::shell::personal_continuity::{AppInstanceId, ResumeMetadata, ResumeSensitivity};
use crate::shell::personal_continuity_registry::{ContinuationRegistry, global_registry};
use crate::shell::record_hydration::{RecordAddress, RecordSource, profile_root, set_source};
use crate::shell::state::{
    Strip, StripId, Tab, WindowGroup, WindowWidth, Workspace, WorkspaceId, next_id,
};
use automerge::transaction::Transactable as _;
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{Completer, Pending as Answer};
use ui::application::NativeProfile;
use ui::record::{RecordCheckout, RecordId, RecordKind, RecordOwners};

const APP: &str = "dev.tilo.continued-record-test";
const RECORD_INSTANCE: u64 = 9_310;

/// The other device of the room, answering once the test says so.
type Fetched = Result<Vec<u8>, String>;

#[derive(Default)]
struct Device {
    asked: Mutex<Vec<RecordAddress>>,
    answer: Mutex<Option<Completer<Fetched>>>,
}

impl RecordSource for Device {
    fn fetch(&self, address: &RecordAddress) -> Answer<Fetched> {
        self.asked.lock().unwrap().push(address.clone());
        let (completer, answer) = tilo_actions::remote::pending();
        *self.answer.lock().unwrap() = Some(completer);
        answer
    }
}

struct Reset(Option<Arc<Mutex<ContinuationRegistry>>>);
impl Drop for Reset {
    fn drop(&mut self) {
        crate::shell::personal_continuity_registry::replace_test_registry(self.0.take());
    }
}

/// The record as the other device keeps it.
fn kept_elsewhere(room: crate::shell::state::RoomId) -> (RecordCheckout, Vec<u8>) {
    let holder_dir = tempfile::tempdir().unwrap();
    let profile_dir = profile_root(holder_dir.path(), room, APP);
    std::fs::create_dir_all(&profile_dir).unwrap();
    let profile = NativeProfile::open(&profile_dir, Some(&room.to_string())).unwrap();
    let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("draft"));
    let records = RecordOwners::new(profile_dir)
        .open(APP, profile.workspace_id())
        .unwrap();
    records.create_record(&checkout).unwrap();
    records
        .with(&checkout.id, |doc| {
            doc.put(automerge::ROOT, "body", "written on the laptop")
                .unwrap();
        })
        .unwrap();
    records.try_flush().unwrap();
    drop(records);
    let bytes = ui::record::transfer::read_by_id(
        profile.store(),
        APP,
        profile.workspace_id(),
        &checkout.id,
    )
    .unwrap()
    .save();
    (checkout, bytes)
}

#[test]
fn a_continued_window_waits_for_its_record_then_reopens_on_it() {
    let _reset = Reset(
        crate::shell::personal_continuity_registry::replace_test_registry(Some(Arc::new(
            Mutex::new(ContinuationRegistry::default()),
        ))),
    );
    let data = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_DATA_HOME", data.path());

    let mut ws = Workspace::new(WorkspaceId::from(next_id()), "shared");
    ws.owner = Some("quinten".into());
    let room = ws.room;
    let tab = next_id();
    let mut strip = Strip::new(StripId::from(next_id()), "Main");
    strip.window_groups.push(WindowGroup {
        id: next_id(),
        active_tab: tab,
        tabs: vec![Tab {
            id: tab,
            title: "Draft".into(),
            tile: TileContent::PeerWindow {
                app_id: APP.into(),
                label: "Available on another device".into(),
                note: String::new(),
            },
        }],
        width: WindowWidth::Normal,
        maximized: false,
    });
    ws.strips.push(strip);
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.desktop.workspaces = vec![ws];

    let (record, bytes) = kept_elsewhere(room);
    let checkpoint = ui::app::with_continuation_record(
        ui::app::compose_structural_continuation_metadata(ui::view::View::default(), None).unwrap(),
        &record.id,
    );
    global_registry().lock().unwrap().store_remote_resume(
        room,
        AppInstanceId(tab),
        APP.into(),
        ResumeMetadata::new(
            checkpoint.schema_version,
            ResumeSensitivity::Syncable,
            checkpoint.bytes,
        )
        .unwrap(),
        AppInstanceId(RECORD_INSTANCE),
    );
    let device = Arc::new(Device::default());
    set_source(Arc::clone(&device) as Arc<dyn RecordSource>);
    let item = Pending {
        tab,
        app_id: APP.into(),
        workspace: 0,
        local: false,
    };

    // The record is asked for, and the window says so instead of opening.
    let note = bring_over_continued_record(&item, room).expect_err("the record is on its way");
    assert_eq!(note, bringing_note());
    assert_eq!(
        device.asked.lock().unwrap().as_slice(),
        [RecordAddress {
            room,
            app_id: APP.into(),
            record: record.id.clone(),
        }]
    );
    assert!(set_note(&mut tree, tab, note));
    assert!(!release_brought_over(&mut tree), "still on its way");
    assert!(!any_pending(&tree));

    // It arrives: the window is released to open, and opens on that record.
    device
        .answer
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .complete(Ok(bytes));
    assert!(release_brought_over(&mut tree));
    assert!(any_pending(&tree));
    assert_eq!(bring_over_continued_record(&item, room), Ok(()));
    assert_eq!(device.asked.lock().unwrap().len(), 1, "asked once");
    let here = NativeProfile::open(
        &profile_root(data.path(), room, APP),
        Some(&room.to_string()),
    )
    .unwrap();
    assert_eq!(
        here.saved_view(RECORD_INSTANCE)
            .unwrap()
            .map(|view| view.record_id),
        Some(record.id.as_str().to_owned()),
        "the continuing instance reopens on the record it showed"
    );
    drop(env);
}
