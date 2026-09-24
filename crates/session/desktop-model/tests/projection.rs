//! A room document carries exactly the windows their minting peer owns,
//! reads back as the same topology, and folds into another peer's tree
//! without disturbing what that peer owns.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use automerge::AutoCommit;
use tilo_desktop_model::projection::{
    changed_owned_entries, contents_of, declared_room, merge_room_contents, project_owned_contents,
    project_room_records, project_workspace_tasks, room_records_of, workspace_tasks_of,
};
use tilo_desktop_model::{
    AppKind, DesktopTree, RoomId, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId, WorkspaceTask, WorkspaceTaskRoute, WorkspaceTaskStatus, compose_id,
};

const ALICE: u16 = 0x0A11;
const BOB: u16 = 0x0B0B;

fn tab(id: u64, title: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: format!("org.example.{title}"),
            label: title.into(),
            note: String::new(),
        },
    }
}

fn group(id: u64, tabs: Vec<Tab>) -> WindowGroup {
    WindowGroup {
        id,
        active_tab: tabs.first().map_or(0, |t| t.id),
        tabs,
        width: WindowWidth::Wide,
        maximized: true,
    }
}

fn strip_of(salt: u16, base: u64) -> Strip {
    let id = |n: u64| compose_id(salt, base + n);
    let mut strip = Strip::new(StripId(id(0)), "Main");
    strip.window_groups = vec![
        group(id(1), vec![tab(id(2), "Editor"), tab(id(3), "Terminal")]),
        group(
            id(4),
            vec![Tab {
                id: id(5),
                title: "Activity".into(),
                tile: TileContent::WorkspaceActivity,
            }],
        ),
    ];
    strip
}

fn room() -> RoomId {
    RoomId::parse("00000000-0000-4000-8000-00000000abcd").unwrap()
}

#[test]
fn owned_contents_read_back_as_the_same_topology() {
    let mut doc = AutoCommit::new();
    let strip = strip_of(ALICE, 10);
    project_owned_contents(&mut doc, std::slice::from_ref(&strip), ALICE, None).unwrap();

    let read = contents_of(&doc);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].id, strip.id.raw());
    assert_eq!(read[0].name.as_deref(), Some("Main"));
    // The device-local activity group never leaves this machine.
    assert_eq!(read[0].groups.len(), 1);
    let titles: Vec<&str> = read[0].groups[0]
        .tabs
        .iter()
        .map(|t| t.title.as_str())
        .collect();
    assert_eq!(titles, ["Editor", "Terminal"]);
}

#[test]
fn nothing_is_projected_without_an_id_space_or_before_the_share_floor() {
    let mut unsalted = AutoCommit::new();
    project_owned_contents(&mut unsalted, &[strip_of(0, 10)], 0, None).unwrap();
    assert!(contents_of(&unsalted).is_empty());

    let mut joined = AutoCommit::new();
    project_owned_contents(&mut joined, &[strip_of(ALICE, 10)], ALICE, Some(1_000)).unwrap();
    assert!(
        contents_of(&joined).is_empty(),
        "windows minted before joining stay private"
    );
}

#[test]
fn rewriting_an_owned_entry_is_detected_but_carrying_a_new_one_is_not() {
    let mut ours = AutoCommit::new();
    let strip = strip_of(ALICE, 10);
    project_owned_contents(&mut ours, std::slice::from_ref(&strip), ALICE, None).unwrap();

    let mut renamed = strip.clone();
    renamed.window_groups[0].tabs[0].title = "Hijacked".into();
    let mut candidate = ours.fork();
    project_owned_contents(&mut candidate, &[renamed], ALICE, None).unwrap();
    assert_eq!(changed_owned_entries(Some(&ours), &candidate, ALICE), 1);

    let mut grown = ours.fork();
    project_owned_contents(&mut grown, &[strip, strip_of(ALICE, 50)], ALICE, None).unwrap();
    assert_eq!(changed_owned_entries(Some(&ours), &grown, ALICE), 0);
}

#[test]
fn a_peers_windows_merge_in_while_ours_keep_their_view_state() {
    let mut bob_doc = AutoCommit::new();
    project_owned_contents(&mut bob_doc, &[strip_of(BOB, 10)], BOB, None).unwrap();

    let mut alice = DesktopTree::default();
    let mut ws = Workspace::new(WorkspaceId(compose_id(ALICE, 1)), "Shared");
    ws.room = room();
    ws.strips.push(strip_of(ALICE, 100));
    alice.workspaces.push(ws);

    assert!(merge_room_contents(
        &mut alice,
        room(),
        contents_of(&bob_doc),
        ALICE
    ));
    let strips = &alice.workspaces[0].strips;
    assert_eq!(strips.len(), 2);
    let ours = strips
        .iter()
        .find(|s| s.id.raw() == compose_id(ALICE, 100))
        .unwrap();
    assert!(ours.window_groups[0].maximized, "our view state is ours");
    let theirs = strips
        .iter()
        .find(|s| s.id.raw() == compose_id(BOB, 10))
        .unwrap();
    assert!(
        !theirs.window_groups[0].maximized,
        "a peer's group opens with this device's defaults"
    );

    assert!(
        !merge_room_contents(&mut alice, room(), contents_of(&bob_doc), ALICE),
        "merging the same document again changes nothing"
    );
}

#[test]
fn a_document_names_the_room_it_describes() {
    use automerge::{ROOT, transaction::Transactable};
    let mut doc = AutoCommit::new();
    assert_eq!(declared_room(&doc), None);
    doc.put(&ROOT, "room", room().to_string()).unwrap();
    assert_eq!(declared_room(&doc), Some(room()));
}

#[test]
fn tasks_and_record_references_round_trip() {
    let mut doc = AutoCommit::new();
    let task = WorkspaceTask {
        id: "task-1".into(),
        title: "Review".into(),
        created_by: "ada".into(),
        assigned_agent: None,
        route: WorkspaceTaskRoute::Manual,
        status: WorkspaceTaskStatus::Queued,
        resource: None,
        created_at: 1,
        updated_at: 2,
        audit: Vec::new(),
    };
    project_workspace_tasks(&mut doc, std::slice::from_ref(&task)).unwrap();
    assert_eq!(workspace_tasks_of(&doc), vec![task]);

    let held = [("rec-1".to_string(), "note".to_string())]
        .into_iter()
        .collect();
    project_room_records(&mut doc, &held).unwrap();
    assert_eq!(
        room_records_of(&doc),
        vec![("rec-1".to_string(), "note".to_string())]
    );
    let _ = AppKind::Terminal;
}
