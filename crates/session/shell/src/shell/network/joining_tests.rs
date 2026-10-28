//! A workspace joined through its host is its room from the first moment;
//! a failed join keeps it with its reason and one way to try again, from the
//! workspace itself.

use super::super::hosted_sites::JoinProgress;
use super::super::site_join::SiteJoin;
use super::super::site_join_tests::{Site, computer, held, key, until};
use crate::shell::state::RoomId;

/// A join refused by the host leaves the room's workspace saying why; the
/// person's "Join again" from that workspace joins it, into the same
/// workspace.
#[test]
#[serial_test::serial]
fn join_again_from_a_failed_workspace_joins_into_it() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let site = Site::start(dir.path());
    let room = RoomId::parse(&site.room).unwrap();
    let (net, tree) = computer(&dir.path().join("computer"), key(1), "Ada's computer");

    site.stale.store(true, std::sync::atomic::Ordering::SeqCst);
    let joined = net.join_site(&site.entry).unwrap();
    assert!(matches!(joined, SiteJoin::Enrolling { .. }), "{joined:?}");
    until("the join to fail", || {
        net.tick();
        net.site_joins()
            .into_iter()
            .any(|join| join.room == room && matches!(join.progress, JoinProgress::Failed(_)))
            .then_some(())
    });
    let workspace = tree
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .map(|workspace| workspace.id)
        .expect("the failed join keeps the room's workspace");

    site.stale.store(false, std::sync::atomic::Ordering::SeqCst);
    net.retry_join(room).expect("joining again is let in now");
    held(&net, &tree, room);
    let workspaces: Vec<_> = tree
        .read()
        .desktop
        .workspaces
        .iter()
        .filter(|candidate| candidate.room() == room)
        .map(|candidate| candidate.id)
        .collect();
    assert_eq!(workspaces, vec![workspace], "the same workspace, the room");
    assert_eq!(tree.read().one_record_violations(), Vec::<String>::new());
    drop(net);
    site.shutdown();
}

/// A room whose host stopped admitting the person says it needs joining
/// again, and nothing is asked of the host until the person does.
#[test]
#[serial_test::serial]
fn a_room_its_host_no_longer_admits_waits_for_the_person() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let site = Site::start(dir.path());
    let room = RoomId::parse(&site.room).unwrap();
    let (net, tree) = computer(&dir.path().join("computer"), key(1), "Ada's computer");
    net.join_site(&site.entry).unwrap();
    held(&net, &tree, room);

    net.room_not_admitted(room);
    let joins = net.site_joins();
    assert_eq!(
        joins
            .iter()
            .map(|join| (join.room, join.progress.clone()))
            .collect::<Vec<_>>(),
        vec![(room, JoinProgress::NeedsJoinAgain)]
    );
    // The community pass leaves it to the person.
    assert_eq!(
        net.keep_community(&site.entry),
        super::super::community::Pass::Held
    );
    assert!(
        tree.read().holds_room(room),
        "the room and its workspace stay"
    );
    drop(net);
    site.shutdown();
}

/// A workspace being joined when the session before ended, whose join left
/// no proof and no site to ask, can only be joined again with a new
/// invitation, and says so.
#[test]
fn an_unfinished_join_with_nothing_to_retry_asks_for_an_invitation() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (net, tree) = computer(&dir.path().join("computer"), key(1), "Ada's computer");
    let room = RoomId::new();
    tree.write()
        .join_native_room("Somewhere", room)
        .expect("a room is joined as a workspace of its own");
    assert!(net.retry_join(room).is_err());
    assert!(tree.read().is_joining(room), "the workspace stays the room");
}
