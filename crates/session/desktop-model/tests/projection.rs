//! A room document carries exactly the windows their minting peer owns,
//! reads back as the same topology, and folds into another peer's tree
//! without disturbing what that peer owns.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "test fixtures: a failed lookup is the assertion"
)]

use automerge::AutoCommit;
use tilo_automerge::ApplyIncoming as _;
use tilo_desktop_model::projection::{
    EmptyStrips, Reader, changed_owned_entries, contents_of, declared_room, merge_room_contents,
    project_owned_contents, project_room_records, project_workspace_shortcuts,
    project_workspace_tasks, room_records_of, workspace_shortcuts_of, workspace_tasks_of,
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
        focus_history: Vec::new(),
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
fn projection_round_trips_owned_contents_once_shared() {
    // Phase `owned_contents_read_back_as_the_same_topology`.
    {
        let mut doc = AutoCommit::new();
        let strip = strip_of(ALICE, 10);
        project_owned_contents(
            &mut doc,
            std::slice::from_ref(&strip),
            ALICE,
            None,
            EmptyStrips::Skip,
        )
        .unwrap();

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
    // Phase `nothing_is_projected_without_an_id_space_or_before_the_share_floor`.
    {
        let mut unsalted = AutoCommit::new();
        project_owned_contents(
            &mut unsalted,
            &[strip_of(0, 10)],
            0,
            None,
            EmptyStrips::Skip,
        )
        .unwrap();
        assert!(contents_of(&unsalted).is_empty());

        let mut joined = AutoCommit::new();
        project_owned_contents(
            &mut joined,
            &[strip_of(ALICE, 10)],
            ALICE,
            Some(1_000),
            EmptyStrips::Skip,
        )
        .unwrap();
        assert!(
            contents_of(&joined).is_empty(),
            "windows minted before joining stay private"
        );
    }
    // Phase `a_document_names_the_room_it_describes`.
    {
        use automerge::{ROOT, transaction::Transactable};
        let mut doc = AutoCommit::new();
        assert_eq!(declared_room(&doc), None);
        doc.put(&ROOT, "room", room().to_string()).unwrap();
        assert_eq!(declared_room(&doc), Some(room()));
    }
    // Phase `tasks_and_record_references_round_trip`.
    {
        let mut doc = AutoCommit::new();
        let task = WorkspaceTask {
            id: "task-1".into(),
            title: "Review".into(),
            created_by: "ada".into(),
            assigned_agent: None,
            route: WorkspaceTaskRoute::Manual,
            status: WorkspaceTaskStatus::Queued,
            resource: None,
            subject: None,
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
}

#[test]
fn projection_detects_rewrites_and_merges_peer_windows() {
    // Phase `rewriting_an_owned_entry_is_detected_but_carrying_a_new_one_is_not`.
    {
        let mut ours = AutoCommit::new();
        let strip = strip_of(ALICE, 10);
        project_owned_contents(
            &mut ours,
            std::slice::from_ref(&strip),
            ALICE,
            None,
            EmptyStrips::Skip,
        )
        .unwrap();

        let mut renamed = strip.clone();
        renamed.window_groups[0].tabs[0].title = "Hijacked".into();
        let mut candidate = ours.fork();
        project_owned_contents(&mut candidate, &[renamed], ALICE, None, EmptyStrips::Skip).unwrap();
        assert_eq!(changed_owned_entries(Some(&ours), &candidate, ALICE), 1);

        let mut grown = ours.fork();
        project_owned_contents(
            &mut grown,
            &[strip, strip_of(ALICE, 50)],
            ALICE,
            None,
            EmptyStrips::Skip,
        )
        .unwrap();
        assert_eq!(changed_owned_entries(Some(&ours), &grown, ALICE), 0);
    }
    // Phase `a_peers_windows_merge_in_while_ours_keep_their_view_state`.
    {
        let mut bob_doc = AutoCommit::new();
        project_owned_contents(
            &mut bob_doc,
            &[strip_of(BOB, 10)],
            BOB,
            None,
            EmptyStrips::Skip,
        )
        .unwrap();

        let mut alice = DesktopTree::default();
        let mut ws = Workspace::new(WorkspaceId(compose_id(ALICE, 1)), "Shared");
        ws.set_room(room());
        ws.strips.push(strip_of(ALICE, 100));
        alice.workspaces.push(ws);

        assert!(merge_room_contents(
            &mut alice,
            room(),
            contents_of(&bob_doc),
            Reader::Owner(ALICE)
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
            !merge_room_contents(
                &mut alice,
                room(),
                contents_of(&bob_doc),
                Reader::Owner(ALICE)
            ),
            "merging the same document again changes nothing"
        );
    }
}

fn shortcut(id: &str, name: &str) -> tilo_automation::Shortcut {
    serde_json::from_value(serde_json::json!({ "id": id, "name": name, "steps": [] }))
        .expect("a shortcut with no steps")
}

/// Two collaborators saving different shortcuts at once both keep theirs,
/// and a deletion reaches the peer that already held the shortcut.
#[test]
fn shortcuts_merge_by_id_and_deletions_replicate() {
    let mut alice = AutoCommit::new();
    project_workspace_shortcuts(&mut alice, &[shortcut("tidy", "Tidy")]).unwrap();
    let mut bob = alice.fork();
    project_workspace_shortcuts(&mut alice, &[shortcut("tidy", "Tidy"), shortcut("a", "A")])
        .unwrap();
    project_workspace_shortcuts(&mut bob, &[shortcut("tidy", "Tidy"), shortcut("b", "B")]).unwrap();
    alice.merge_incoming(&mut bob).unwrap();
    assert_eq!(
        workspace_shortcuts_of(&alice),
        vec![
            shortcut("a", "A"),
            shortcut("b", "B"),
            shortcut("tidy", "Tidy")
        ]
    );

    let kept: Vec<_> = workspace_shortcuts_of(&alice)
        .into_iter()
        .filter(|shortcut| shortcut.id != "tidy")
        .collect();
    project_workspace_shortcuts(&mut alice, &kept).unwrap();
    bob.merge_incoming(&mut alice).unwrap();
    assert_eq!(
        workspace_shortcuts_of(&bob),
        vec![shortcut("a", "A"), shortcut("b", "B")]
    );
}

#[test]
fn a_closed_task_takes_the_grants_given_for_it_out_of_the_room() {
    use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, RoomPolicy, Subject, TreePath};
    use tilo_desktop_model::projection::{lapse_task_grants, project_room_policy, room_policy_of};
    let task = |id: &str, status| WorkspaceTask {
        id: id.into(),
        title: id.into(),
        created_by: "ada".into(),
        assigned_agent: None,
        route: WorkspaceTaskRoute::Manual,
        status,
        resource: None,
        subject: None,
        created_at: 1,
        updated_at: 1,
        audit: Vec::new(),
    };
    let for_task = |id: &str, task: &str| {
        Grant::new(
            id,
            Subject::agent("helper"),
            TreePath::room("r"),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            Subject::person("ada"),
        )
        .with_lifetime(Lifetime::Task(task.into()))
    };
    let mut policy = RoomPolicy::new("r", "ada");
    policy.grants.push(for_task("open-grant", "open"));
    policy.grants.push(for_task("done-grant", "done"));
    let mut doc = AutoCommit::new();
    project_room_policy(&mut doc, &policy).unwrap();
    project_workspace_tasks(
        &mut doc,
        &[
            task("open", WorkspaceTaskStatus::InProgress),
            task("done", WorkspaceTaskStatus::Done),
        ],
    )
    .unwrap();

    let lapsed = lapse_task_grants(&mut doc).unwrap();
    assert_eq!(lapsed, vec![tilo_action_policy::GrantId::new("done-grant")]);
    let held: Vec<String> = room_policy_of(&doc, "r")
        .grants
        .iter()
        .map(|grant| grant.id.as_str().to_owned())
        .collect();
    assert!(held.contains(&"open-grant".to_owned()));
    assert!(!held.contains(&"done-grant".to_owned()));
    assert!(
        lapse_task_grants(&mut doc).unwrap().is_empty(),
        "a lapsed grant is gone from the room's grants, not merely skipped"
    );
}
