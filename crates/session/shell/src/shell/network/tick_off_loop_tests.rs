//! [`ShellNetwork::frame_tick`] runs on the compositor's loop, between a key's
//! arrival and its delivery. Nothing it does may wait for the disk or for
//! another thread's hold on the tree; what must follow the disk still
//! follows it.

use super::tests::{drain, personal_continuity_network_for_test};
use super::{NeighbourMsg, NetCmd, ShellNetwork, WorkspaceTreeReplica};
use crate::shell::personal_continuity::{AppInstanceId, AppInstanceState};
use crate::shell::personal_continuity_registry::global_registry;
use crate::shell::state::{RoomId, Tab, TileContent, WorkspaceTree};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const EDITOR: &str = "so.tilo.code-editor";
const EDITOR_TAB: u64 = 0x0E_D1_70;
const CLOSED_TAB: u64 = 0x0C_105E;

/// Park the process's state writer inside a job, as a disk stuck in fsync
/// would; the returned sender releases it.
fn stall_the_state_writer(dir: &std::path::Path) -> mpsc::Sender<()> {
    let (release, parked) = mpsc::channel::<()>();
    let (started, running) = mpsc::channel::<()>();
    crate::shell::durable_writer::global().submit(dir.join("stuck-fsync"), move || {
        started.send(()).unwrap();
        let _ = parked.recv();
    });
    running.recv().unwrap();
    release
}

fn continuity_permitted() -> crate::shell::continuity_policy::ContinuityPolicySnapshot {
    crate::shell::continuity_policy::ContinuityPolicySnapshot::from_document(
        serde_json::json!({ "continuity": { "enabled": true, "app_continuations": true } })
            .as_object()
            .unwrap(),
    )
}

/// A shell whose desktop has an editor tab the editor has claimed as its
/// live owner view, and a closed tab the next checkpoint records.
fn shell_with_a_claimed_editor(
    store: std::path::PathBuf,
) -> (ShellNetwork, tokio::sync::mpsc::Receiver<NetCmd>, RoomId) {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let room = tree.rooms()[0];
    let group = tree
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room == room)
        .and_then(|workspace| workspace.strips.first_mut())
        .and_then(|strip| strip.window_groups.first_mut())
        .unwrap();
    group.tabs.push(Tab {
        id: EDITOR_TAB,
        title: "Editor".into(),
        tile: TileContent::Foreign {
            app_id: EDITOR.into(),
            surface_id: 7,
        },
    });
    tree.continuation_closed_tabs.insert(CLOSED_TAB, room);
    let (mut net, commands) = personal_continuity_network_for_test([0x7C; 32]);
    net.replica = Arc::new(Mutex::new(WorkspaceTreeReplica::new(1, tree.shared())));
    net.personal_continuity_path = Some(store);
    net.continuity_policy_override = Some(continuity_permitted());

    let metadata =
        ui::app::compose_structural_continuation_metadata(ui::view::View::default(), None).unwrap();
    let registry = global_registry();
    let mut registry = registry.lock().unwrap();
    registry.bind_tab(AppInstanceId(EDITOR_TAB), AppInstanceId(EDITOR_TAB));
    registry
        .report(
            room,
            EDITOR,
            AppInstanceId(EDITOR_TAB),
            metadata.schema_version,
            metadata.bytes.clone(),
        )
        .unwrap();
    registry
        .claim_owner_view(room, EDITOR.into(), AppInstanceId(EDITOR_TAB), 1, metadata)
        .unwrap();
    drop(registry);
    (net, commands, room)
}

fn editor_claimed(net: &ShellNetwork) -> bool {
    let device = net.personal_continuity_device.clone().unwrap();
    let continuity = net.personal_continuity.lock().unwrap();
    let editor: AppInstanceState = continuity
        .snapshot_for(&device)
        .and_then(|snapshot| {
            snapshot
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.apps.iter())
                .find(|app| app.id == AppInstanceId(EDITOR_TAB))
                .cloned()
        })
        .expect("the editor is in this device's capture");
    let room = continuity
        .snapshot_for(&device)
        .and_then(|snapshot| {
            snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.apps.iter().any(|app| app.id == editor.id))
                .map(|workspace| workspace.room)
        })
        .unwrap();
    drop(continuity);
    let registry = global_registry();
    registry.lock().unwrap().owner_claimed(room, &editor)
}

fn upkeep_tick(net: &ShellNetwork) -> Duration {
    // Every two seconds a tick does the continuity upkeep; make this one.
    *net.last_sync_kick.lock().unwrap() = None;
    let started = Instant::now();
    net.frame_tick();
    started.elapsed()
}

fn closed_tab_marked(net: &ShellNetwork) -> bool {
    net.replica
        .lock()
        .unwrap()
        .tree
        .read()
        .continuation_closed_tabs
        .contains_key(&CLOSED_TAB)
}

#[test]
#[serial_test::serial(process_env)]
fn a_tick_never_waits_for_the_continuity_write_and_acknowledges_only_once_it_is_on_disk() {
    let (_apps, _env) = crate::shell::personal_continuity_registry::tests::app_store(
        EDITOR,
        Some("text-position-v1"),
        r#"["presentation"]"#,
    );
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("personal-continuity.postcard");
    let (net, _commands, _room) = shell_with_a_claimed_editor(store.clone());

    // The disk is stuck. Were the tick to write here, or to wait for the
    // writer, it would not come back.
    let release = stall_the_state_writer(dir.path());
    let waited = upkeep_tick(&net);
    assert!(
        !store.exists(),
        "the continuity store was written on the loop"
    );
    assert!(
        waited < Duration::from_secs(2),
        "the tick waited {waited:?} for a stuck disk"
    );
    // Further rounds while the disk is stuck release nothing either.
    upkeep_tick(&net);
    assert!(
        editor_claimed(&net),
        "the claim was acknowledged before its checkpoint was on disk"
    );
    assert!(
        closed_tab_marked(&net),
        "the closed-tab marker went before its tombstone was on disk"
    );

    // The disk catches up; the next round acknowledges what it holds.
    release.send(()).unwrap();
    crate::shell::persist::wait_for_saves();
    assert!(store.is_file());
    upkeep_tick(&net);
    assert!(
        !editor_claimed(&net),
        "the claim is acknowledged once its checkpoint is on disk"
    );
    assert!(!closed_tab_marked(&net));
    let on_disk = super::load_personal_continuity(&store);
    assert_eq!(
        on_disk
            .continuity
            .snapshot_for(net.personal_continuity_device.as_ref().unwrap()),
        net.personal_continuity
            .lock()
            .unwrap()
            .snapshot_for(net.personal_continuity_device.as_ref().unwrap()),
        "what was acknowledged is what is on disk"
    );
}

#[test]
#[serial_test::serial(process_env)]
fn a_failed_continuity_write_acknowledges_nothing_until_a_later_one_lands() {
    let (_apps, _env) = crate::shell::personal_continuity_registry::tests::app_store(
        EDITOR,
        Some("text-position-v1"),
        r#"["presentation"]"#,
    );
    let dir = tempfile::tempdir().unwrap();
    // The store's directory cannot be created: a file stands in its place.
    let blocked = dir.path().join("state");
    std::fs::write(&blocked, b"not a directory").unwrap();
    let store = blocked.join("personal-continuity.postcard");
    let (net, _commands, _room) = shell_with_a_claimed_editor(store.clone());

    upkeep_tick(&net);
    crate::shell::persist::wait_for_saves();
    upkeep_tick(&net);
    crate::shell::persist::wait_for_saves();
    assert!(!store.exists());
    assert!(
        editor_claimed(&net),
        "a write that failed acknowledged the claim"
    );
    assert!(closed_tab_marked(&net));

    // The disk recovers; the next round writes the same capture again and
    // the one after acknowledges it.
    std::fs::remove_file(&blocked).unwrap();
    upkeep_tick(&net);
    crate::shell::persist::wait_for_saves();
    upkeep_tick(&net);
    assert!(store.is_file());
    assert!(!editor_claimed(&net));
    assert!(!closed_tab_marked(&net));
}

#[test]
fn a_room_push_never_waits_for_another_threads_hold_on_the_tree() {
    const SALT: u16 = 0xB10C;
    let tree = WorkspaceTree::bootstrap_demo().shared();
    tree.write().set_salt_override(SALT);
    let (net, mut commands) = super::shell_network_for_test(tree.clone(), 0xB10C);
    let pushes = |commands: &mut tokio::sync::mpsc::Receiver<NetCmd>| {
        drain(commands)
            .into_iter()
            .filter(|command| {
                matches!(
                    command,
                    NetCmd::Publish {
                        msg: NeighbourMsg::SyncResp { .. },
                        ..
                    }
                )
            })
            .count()
    };
    net.frame_tick();
    // The first tick's other upkeep wrote the tree after its push; the
    // fallback projection takes that in, and from then on nothing moves.
    std::thread::sleep(super::ROOM_PUSH_FALLBACK_INTERVAL + Duration::from_millis(50));
    net.frame_tick();
    let _ = pushes(&mut commands);
    let idle = net.replica.lock().unwrap().tree.generation();
    net.frame_tick();
    net.frame_tick();
    assert_eq!(
        net.replica.lock().unwrap().tree.generation(),
        idle,
        "an idle tick does not take the tree for writing"
    );

    let room = tree.read().rooms()[0];
    let (held, holding) = mpsc::channel();
    let (let_go, release) = mpsc::channel::<()>();
    let holder = {
        std::thread::spawn(move || {
            let mut guard = tree.write();
            let group = guard
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room == room)
                .and_then(|workspace| workspace.strips.first_mut())
                .and_then(|strip| strip.window_groups.first_mut())
                .unwrap();
            group.tabs.push(Tab {
                id: tilo_desktop_model::compose_id(SALT, 99),
                title: "Held".into(),
                tile: TileContent::Stub {
                    kind: crate::shell::state::AppKind::Other("so.tilo.notes".into()),
                    label: "Notes".into(),
                },
            });
            guard.seed_doc_from_cache();
            drop(guard);
            // Still writing: the change above is visible, the tree is held.
            let _guard = tree.write();
            held.send(()).unwrap();
            let _ = release.recv();
        })
    };
    holding.recv().unwrap();
    let started = Instant::now();
    net.frame_tick();
    let waited = started.elapsed();
    assert!(
        waited < Duration::from_secs(2),
        "the tick waited {waited:?} for another thread's write"
    );
    assert_eq!(pushes(&mut commands), 0);
    assert!(
        net.next_deadline().is_some(),
        "a deferred push asks the loop to come back for it"
    );

    let_go.send(()).unwrap();
    holder.join().unwrap();
    net.frame_tick();
    assert_eq!(pushes(&mut commands), 1, "the deferred change goes out");
    assert_eq!(net.next_deadline(), None);
}
