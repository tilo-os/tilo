use super::room_hold;
use super::*;
use crate::shell::broker_services::RoomStatusSource;
use crate::shell::chrome_state::ChromeState;
use crate::shell::state::{TileContent, WorkspaceTree, WorkspaceTreeHandle};

/// Starts nothing: says it started, in the session it started in.
#[expect(
    clippy::unnecessary_wraps,
    reason = "a held launch starts through the launcher's start, which can fail"
)]
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
    assert_eq!(
        room_hold::room_wait("so.tilo.notes"),
        Some(room_hold::RoomWait::Joining),
        "its window says it waits for the room being joined"
    );

    tree.write().joining_rooms.remove(&room);
    tilo_test_env::wait_until("the room's launch to start", || {
        !lock(&session.events).is_empty()
    });
    assert!(room_hold::nothing_waits(&session));
    let started = session.take();
    drop(session);
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
    assert!(room_hold::nothing_waits(&session));
    drop(session);
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
        drop(tree);
        (tab, room)
    };
    let session = session_over(&st.tree_handle());
    room_hold::admit(&session, launch_into("so.tilo.notes", room), recorded).unwrap();
    assert_eq!(
        room_hold::room_wait("so.tilo.notes"),
        Some(room_hold::RoomWait::Placing),
        "its window says it waits for the room being placed on this device's host"
    );

    crate::shell::window_actions::close_tab(&mut st, tab).expect("the window closes");
    assert_eq!(room_hold::room_wait("so.tilo.notes"), None);
    tilo_test_env::wait_until("the room's waiter to end", || {
        room_hold::nothing_waits(&session)
    });

    st.tree_mut().pending_placements.remove(&room);
    drop(st);
    assert!(room_hold::nothing_waits(&session));
    assert!(
        session.take().is_empty(),
        "the closed window's app never starts"
    );
    drop(session);
}

/// A workspace from before rooms had hosts waits for this device's host as
/// a new one does: a launch into it is held, never refused, and starts once
/// the room is placed.
#[test]
fn a_launch_into_a_workspace_from_before_hosts_starts_once_it_is_placed() {
    let mut tree = WorkspaceTree::default();
    let (_, room) = tree.create_hosted_workspace("Plans");
    // As a desktop saved before rooms had hosts loads: nothing waits.
    tree.pending_placements.clear();
    tree.claim_own_unhosted_rooms(|_| "me".to_owned());
    assert!(tree.pending_placements.contains(&room));
    let tree = tree.shared();
    let session = session_over(&tree);

    room_hold::admit(&session, launch_into("so.tilo.notes", room), recorded).unwrap();
    assert!(
        session.take().is_empty(),
        "nothing starts while the room is placed"
    );

    tree.write().pending_placements.remove(&room);
    tilo_test_env::wait_until("the room's launch to start", || {
        !lock(&session.events).is_empty()
    });
    let started = session.take();
    drop(session);
    assert_eq!(started.len(), 1, "{started:?}");
    assert!(
        matches!(started[0].kind, LaunchEventKind::Exited),
        "the launch started: {:?}",
        started[0].kind
    );
}

/// An app opened as the session starts, while this device's own host is
/// still placing its workspace, waits for the room's enrollment to commit
/// and opens as the device the commit installed: it is not refused for
/// coming first.
#[test]
fn an_opening_asked_for_before_the_enrollment_commits_goes_on_once_it_does() {
    let (_directory, owner, room, commit) =
        crate::shell::native_workspace_owner::test_owner_before_enrollment();
    let tree = crate::shell::native_workspace_owner::tree_of(&owner);
    assert!(!owner.rooms().contains(&room), "nothing is committed yet");
    let session = LaunchSession::current();
    let (opened, opened_now) = std::sync::mpsc::channel();
    let waiting = {
        let tree = tree.clone();
        std::thread::spawn(move || {
            opened
                .send(room_hold::committed_device(&session, &tree, room))
                .expect("the test waits for it");
        })
    };
    assert!(
        opened_now
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "the opening waits while the room's enrollment has not committed"
    );
    commit();
    let device = opened_now
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the commit wakes the opening")
        .expect("the opening goes on once the room is enrolled");
    assert_eq!(
        Ok(device),
        owner.with_current(room, |view| Ok(view.device().clone())),
        "it opens as the device the room is held under"
    );
    waiting.join().expect("the opening ends");
}

/// A room this device holds no enrollment in, and is neither placing,
/// joining nor enrolling into, will never be enrolled here: an app opened
/// into it is refused at once, saying so.
#[test]
fn an_opening_into_a_room_that_will_never_be_enrolled_is_refused() {
    let (_directory, owner, _room, _now, _key) =
        crate::shell::native_workspace_owner::test_enrolled_owner();
    let tree = crate::shell::native_workspace_owner::tree_of(&owner);
    let elsewhere = RoomId::new();
    let refused = room_hold::committed_device(&LaunchSession::current(), &tree, elsewhere)
        .expect_err("a room never enrolled here is refused");
    assert!(
        matches!(&refused, LaunchError::DeviceNotAdmitted { detail }
            if detail.contains("holds no enrollment in the workspace's room")),
        "{refused:?}"
    );
}
