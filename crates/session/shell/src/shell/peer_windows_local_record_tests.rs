//! Exercise the real launcher claim, disk snapshot, and restore staging path.
use super::*;
use crate::shell::personal_continuity::{AppInstanceId, LaunchIntent};
use crate::shell::personal_continuity_registry::{ContinuationRegistry, global_registry};
use crate::shell::state::{RoomId, next_id};
use automerge::ReadDoc as _;
use tilo_automerge::ApplyIncoming as _;

const APP: &str = "dev.tilo.record-restart-test";

struct RuntimeReset(Option<std::sync::Arc<std::sync::Mutex<ContinuationRegistry>>>);
impl RuntimeReset {
    fn new() -> Self {
        launcher::clear_pending_placements();
        Self(
            crate::shell::personal_continuity_registry::replace_test_registry(Some(
                std::sync::Arc::new(std::sync::Mutex::new(ContinuationRegistry::default())),
            )),
        )
    }
    fn clear_registry() {
        *global_registry().lock().unwrap() = ContinuationRegistry::default();
    }
}
impl Drop for RuntimeReset {
    fn drop(&mut self) {
        crate::shell::personal_continuity_registry::replace_test_registry(self.0.take());
        launcher::clear_pending_placements();
    }
}

fn adopt(tree: &mut WorkspaceTree, instance: u64) -> TabId {
    launcher::clear_pending_placements();
    launcher::remember_launch_named(APP, &[APP.to_string()], "Record restart fixture");
    launcher::set_pending_placement(APP, Placement::NewGroup);
    launcher::attach_continuity_instance_for_test(APP, instance);
    let room = tree.desktop.active_workspace().unwrap().room;
    crate::shell::tab_binding::adopt_toplevel_in_room(tree, next_id(), Some(APP.into()), room)
        .expect("launcher-attributed adoption")
}

fn staged(tree: &WorkspaceTree, tab: TabId) -> Option<u64> {
    let item = pending(tree).into_iter().find(|item| item.tab == tab)?;
    let room = tree.desktop.workspaces[item.workspace].room;
    stage_continuation_for_tab(tree, &item);
    launcher::take_staged_resume_for_package(&item.app_id, room).map(|(_, record)| record)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn local_record_restart_keeps_exact_records_and_drops_mismatches() {
    fn local_record_restart_keeps_distinct_source_records_for_same_app_tabs() {
        let _runtime = RuntimeReset::new();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.persist_path = Some(path.clone());
        let first_record = next_id();
        let first_tab = adopt(&mut tree, first_record);
        let second_record = next_id();
        let second_tab = adopt(&mut tree, second_record);
        let room = tree.desktop.active_workspace().unwrap().room;
        let launch = LaunchIntent::app(APP).unwrap();
        for (tab, record) in [(first_tab, first_record), (second_tab, second_record)] {
            assert_eq!(
                global_registry().lock().unwrap().record_instance_for_tab(
                    room,
                    AppInstanceId(tab),
                    &launch
                ),
                Some(AppInstanceId(record)),
                "the real launcher/toplevel handshake bound this record"
            );
        }
        RuntimeReset::clear_registry();
        let mut restored = crate::shell::persist::load_tree_from(&path).unwrap();
        assert_eq!(
            staged(&restored, first_tab),
            Some(first_record),
            "desktop restore must stage the original record, not allocate a new one"
        );
        assert_eq!(
            staged(&restored, second_tab),
            Some(second_record),
            "sibling app tabs retain distinct records"
        );
        let mut peer_document = automerge::AutoCommit::new();
        peer_document
            .apply_incoming(
                restored
                    .export_room_changes_since(room, &[])
                    .unwrap()
                    .into_iter()
                    .map(|bytes| automerge::Change::from_bytes(bytes).unwrap()),
            )
            .unwrap();
        assert!(
            peer_document
                .get(automerge::ROOT, "local_app_records")
                .unwrap()
                .is_none(),
            "device-local record associations must not enter the room document"
        );
    }

    fn local_record_restart_does_not_guess_records_for_legacy_snapshots() {
        let _runtime = RuntimeReset::new();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.persist_path = Some(path.clone());
        let tab = adopt(&mut tree, next_id());
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("local_app_records");
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        RuntimeReset::clear_registry();
        let restored = crate::shell::persist::load_tree_from(&path).unwrap();
        assert_eq!(staged(&restored, tab), None);
    }

    fn local_record_restart_rejects_mismatched_room_app_and_remote_tab() {
        let _runtime = RuntimeReset::new();
        for mode in ["room", "app", "peer", "missing-tab"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("desktop.json");
            let mut tree = WorkspaceTree::bootstrap_default();
            tree.persist_path = Some(path.clone());
            let record = next_id();
            let tab = adopt(&mut tree, record);
            let room = tree.desktop.active_workspace().unwrap().room;
            let mut json: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            let recorded_room = if mode == "room" { RoomId::new() } else { room };
            let recorded_app = if mode == "app" { "dev.tilo.other" } else { APP };
            let recorded_tab = if mode == "missing-tab" {
                next_id()
            } else {
                tab
            };
            json["local_app_records"] = serde_json::json!({ (recorded_tab.to_string()): {
                "room": recorded_room, "app_id": recorded_app, "record_instance": record,
            }});
            if mode == "peer" {
                json["workspaces"][0]["strips"][0]["window_groups"][0]["tabs"][0]["tile"] =
                    serde_json::to_value(TileContent::PeerWindow {
                        app_id: APP.into(),
                        label: "Remote fixture".into(),
                        note: String::new(),
                    })
                    .unwrap();
            }
            std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
            RuntimeReset::clear_registry();
            let restored = crate::shell::persist::load_tree_from(&path).unwrap();
            assert_eq!(
                staged(&restored, tab),
                None,
                "{mode} must not reuse a local record"
            );
            let saved = serde_json::to_value(&restored).unwrap();
            assert!(
                saved.get("local_app_records").is_none_or(|entries| entries
                    .as_object()
                    .is_some_and(serde_json::Map::is_empty)),
                "invalid entries must be pruned at load: {mode}"
            );
        }
    }

    local_record_restart_keeps_distinct_source_records_for_same_app_tabs();
    local_record_restart_does_not_guess_records_for_legacy_snapshots();
    local_record_restart_rejects_mismatched_room_app_and_remote_tab();
}

#[test]
fn local_record_restart_removes_closed_and_revalidates_live_tabs() {
    fn local_record_restart_removes_closed_tab_metadata() {
        let _runtime = RuntimeReset::new();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("desktop.json");
        let mut tree = WorkspaceTree::bootstrap_default();
        tree.persist_path = Some(path.clone());
        let tab = adopt(&mut tree, next_id());
        assert!(crate::shell::tab_binding::remove_tab(&mut tree, tab));
        let current = serde_json::to_value(&tree).unwrap();
        assert!(
            current.get("local_app_records").is_none_or(|entries| {
                entries.as_object().is_some_and(serde_json::Map::is_empty)
            })
        );
        crate::shell::persist::save_tree_to(&tree, &path).unwrap();
        RuntimeReset::clear_registry();
        let restored = crate::shell::persist::load_tree_from(&path).unwrap();
        assert_eq!(staged(&restored, tab), None);
    }

    fn local_record_restart_revalidates_live_tab_before_each_staging() {
        let _runtime = RuntimeReset::new();
        for mode in ["room", "app", "peer"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("desktop.json");
            let mut tree = WorkspaceTree::bootstrap_default();
            tree.persist_path = Some(path.clone());
            let record = next_id();
            let tab = adopt(&mut tree, record);
            // Keep the process association: a previously valid local binding must
            // not later shadow a changed tab or an exact remote continuation.
            let mut restored = crate::shell::persist::load_tree_from(&path).unwrap();
            assert_eq!(staged(&restored, tab), Some(record));
            let room = restored.desktop.active_workspace().unwrap().room;
            let workspace = restored.desktop.active_workspace_mut().unwrap();
            let tile = &mut workspace.strips[0].window_groups[0].tabs[0].tile;
            let remote_record = next_id();
            match mode {
                "room" => workspace.room = RoomId::new(),
                "app" => {
                    *tile = TileContent::LocalApp {
                        app_id: "dev.tilo.changed-record-test".into(),
                        label: "Changed".into(),
                        note: String::new(),
                    }
                }
                _ => {
                    *tile = TileContent::PeerWindow {
                        app_id: APP.into(),
                        label: "Remote".into(),
                        note: String::new(),
                    };
                    global_registry().lock().unwrap().store_remote_record(
                        room,
                        AppInstanceId(tab),
                        APP.into(),
                        AppInstanceId(remote_record),
                    );
                }
            }
            assert_eq!(
                staged(&restored, tab),
                (mode == "peer").then_some(remote_record),
                "stale local mapping must not shadow {mode}"
            );
        }
    }

    local_record_restart_removes_closed_tab_metadata();
    local_record_restart_revalidates_live_tab_before_each_staging();
}
