//! The community workspace a computer is in by default: joined without a
//! prompt, left for good once the person leaves it, joined again from
//! the directory, and joined anew when its host was set up again.

use super::community::Pass;
use super::site_join_tests::{Site, computer, held, key};
use crate::shell::state::RoomId;

fn holds(tree: &crate::shell::state::WorkspaceTreeHandle, room: RoomId) -> bool {
    tree.read()
        .desktop
        .workspaces
        .iter()
        .any(|workspace| workspace.room() == room)
}

#[test]
#[serial_test::serial]
fn the_community_workspace_is_joined_by_default_and_leaving_it_lasts() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let site = Site::start(dir.path());
    let ada = key(1);
    let computer_dir = dir.path().join("computer");
    let room = RoomId::parse(&site.room).unwrap();

    let (net, tree) = computer(&computer_dir, ada.clone(), "Ada's computer");
    assert_eq!(net.keep_community(&site.entry), Pass::Held);
    held(&net, &tree, room);
    assert!(site.member(ada.verifying_key().to_bytes()));
    assert_eq!(
        net.keep_community(&site.entry),
        Pass::Held,
        "a held community workspace is kept as it is"
    );

    net.leave_site(room).unwrap();
    assert!(!holds(&tree, room), "Leave takes the workspace away");
    assert!(net.left_site(&site.entry));
    assert_eq!(
        net.keep_community(&site.entry),
        Pass::Idle,
        "a left community workspace is not joined again"
    );
    // A restart: nothing of this session is left.
    drop(net);
    drop(tree);
    crate::shell::persist::wait_for_saves();

    // After a restart the person is still out of it.
    let (net, tree) = computer(&computer_dir, ada, "Ada's computer");
    assert!(net.left_site(&site.entry));
    assert_eq!(net.keep_community(&site.entry), Pass::Idle);
    assert!(!holds(&tree, room));

    // Joining it from the directory brings it back.
    net.rejoin_site(&site.entry).unwrap();
    held(&net, &tree, room);
    assert!(!net.left_site(&site.entry));
    drop(net);
    site.shutdown();
}

#[test]
#[serial_test::serial]
fn a_community_host_set_up_again_is_joined_anew_as_the_same_person_and_the_room_before_stays() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut site = Site::start(dir.path());
    let ada = key(1);
    let (net, tree) = computer(&dir.path().join("computer"), ada.clone(), "Ada's computer");
    let before = RoomId::parse(&site.room).unwrap();
    assert_eq!(net.keep_community(&site.entry), Pass::Held);
    held(&net, &tree, before);

    site.reset(dir.path(), 42);
    let after = RoomId::parse(&site.room).unwrap();
    assert_ne!(before, after);
    assert_eq!(net.keep_community(&site.entry), Pass::Held);
    held(&net, &tree, after);
    // The site names another room now, joined as a workspace of its own;
    // the room of the host before and its workspace stay with what this
    // computer holds of them, until the person deletes them: never data
    // dropped behind their back, never a workspace re-pointed.
    assert!(
        holds(&tree, before),
        "the workspace of the host before stays"
    );
    assert_eq!(
        net.site_room_labels()
            .into_iter()
            .filter(|label| label.site.is_some())
            .map(|label| label.room)
            .collect::<Vec<_>>(),
        vec![after],
        "the site names only its room now"
    );
    assert_eq!(tree.read().one_record_violations(), Vec::<String>::new());
    assert!(
        site.member(ada.verifying_key().to_bytes()),
        "the new host admitted the same person"
    );
    drop(net);
    site.shutdown();
}
