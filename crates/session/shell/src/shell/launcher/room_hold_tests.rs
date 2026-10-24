use super::room_hold;
use super::*;
use crate::shell::broker_services::RoomStatusSource;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::{TileContent, WorkspaceTree, WorkspaceTreeHandle};

/// Starts nothing: says it started, in the session it started in.
fn recorded(launch: HeldLaunch, session: &LaunchSession) -> Result<(), String> {
    session.push(LaunchEvent {
        package_id: launch.package_id,
        display_name: launch.display_name,
        managed_agent_id: None,
        managed_workspace_id: None,
        kind: LaunchEventKind::Exited,
    });
    Ok(())
}

fn launch_into(package_id: &str, room: RoomId) -> HeldLaunch {
    HeldLaunch {
        authority: None,
        #[cfg(target_os = "linux")]
        retained: None,
        package_id: package_id.into(),
        display_name: package_id.into(),
        command: LaunchCommand {
            program: PathBuf::from("/bin/true"),
            args: vec!["run".into(), "--workspace-id".into(), room.to_string()],
            env: Vec::new(),
            service_identity: None,
            portal_workspace_id: None,
            managed_agent_id: None,
            x11_workspace: None,
        },
    }
}

/// This thread's session, with rooms standing as `tree` says.
fn session_over(tree: &WorkspaceTreeHandle) -> LaunchSession {
    room_hold::set_source(RoomStatusSource::new(tree.clone(), None));
    LaunchSession::current()
}

fn nothing_waits(session: &LaunchSession) -> bool {
    lock(&session.rooms.0).held.is_empty()
}

#[test]
fn a_launch_into_a_room_being_joined_starts_once_the_room_is_ready_and_only_once() {
    let room = RoomId::new();
    let mut tree = WorkspaceTree::default();
    tree.joining_rooms.insert(room);
    let tree = tree.shared();
    let session = session_over(&tree);

    room_hold::admit(&session, launch_into("so.tilo.notes", room), recorded).unwrap();
    room_hold::admit(&session, launch_into("so.tilo.notes", room), recorded).unwrap();
    assert!(
        session.take().is_empty(),
        "nothing starts while the room is joined"
    );
    let wait = room_hold::room_wait("so.tilo.notes").expect("its window says why it waits");
    assert!(
        wait.starts_with(&crate::shell::i18n::t("workspace-not-ready-no-documents")),
        "{wait}"
    );

    tree.write().joining_rooms.remove(&room);
    tilo_test_env::wait_until("the room's launch to start", || {
        !lock(&session.events).is_empty()
    });
    assert!(nothing_waits(&session));
    let started = session.take();
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(started[0].package_id, "so.tilo.notes");
    assert_eq!(room_hold::room_wait("so.tilo.notes"), None);
}

#[test]
fn a_launch_into_a_ready_room_starts_at_once() {
    let room = RoomId::new();
    let session = session_over(&WorkspaceTree::default().shared());

    room_hold::admit(&session, launch_into("so.tilo.notes", room), recorded).unwrap();
    assert_eq!(session.take().len(), 1);
    assert!(nothing_waits(&session));
}

#[test]
fn closing_the_window_of_a_held_launch_lets_it_go_so_nothing_starts_later() {
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
    let mut st = chrome.borrow_mut();
    let (tab, room) = {
        let mut tree = st.tree_mut();
        let workspace = tree.desktop.workspaces.first_mut().expect("a workspace");
        let room = workspace.room();
        let tab = workspace
            .strips
            .iter_mut()
            .flat_map(|strip| strip.window_groups.iter_mut())
            .flat_map(|group| group.tabs.iter_mut())
            .next()
            .expect("a tab");
        tab.tile = TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: crate::shell::peer_windows::OPENING.into(),
        };
        let tab = tab.id;
        tree.pending_placements.insert(room);
        (tab, room)
    };
    let session = session_over(&st.tree_handle());
    room_hold::admit(&session, launch_into("so.tilo.notes", room), recorded).unwrap();
    assert!(room_hold::room_wait("so.tilo.notes").is_some());

    crate::shell::window_actions::close_tab(&mut st, tab).expect("the window closes");
    assert_eq!(room_hold::room_wait("so.tilo.notes"), None);
    tilo_test_env::wait_until("the room's waiter to end", || nothing_waits(&session));

    st.tree_mut().pending_placements.remove(&room);
    drop(st);
    assert!(nothing_waits(&session));
    assert!(
        session.take().is_empty(),
        "the closed window's app never starts"
    );
}

#[test]
fn a_held_launch_whose_room_will_not_be_ready_is_reported_with_the_reason() {
    let mut tree = WorkspaceTree::default();
    let (_, room) = tree.create_hosted_workspace("Plans");
    assert!(tree.pending_placements.contains(&room));
    let tree = tree.shared();
    let session = session_over(&tree);

    room_hold::admit(&session, launch_into("so.tilo.notes", room), recorded).unwrap();
    assert!(
        session.take().is_empty(),
        "nothing starts while the room is placed"
    );

    tree.write().pending_placements.remove(&room);
    tilo_test_env::wait_until("the room's launch to be reported", || {
        !lock(&session.events).is_empty()
    });
    let reported = session.take();
    assert_eq!(reported.len(), 1, "{reported:?}");
    let reason = crate::shell::i18n::t("workspace-on-this-device-only-no-documents");
    assert!(
        matches!(
            &reported[0].kind,
            LaunchEventKind::SpawnFailed { reason: LaunchFailure::RoomRefused, detail } if *detail == reason
        ),
        "{:?}",
        reported[0].kind
    );
    assert_eq!(
        reported[0].failure_notice().map(|notice| notice.body),
        Some(reason)
    );
}
