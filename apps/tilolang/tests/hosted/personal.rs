//! A person's own state of a workspace in a hosted window: the place they pick and what they
//! mark seen are kept in the workspace's document of each person's own state, so they are still
//! theirs after the window opens again and reach the person's other devices.

use super::{Session, place_at};
use collaboration::{CollabRequest, RouteSpec};
use tilolang_process::testing::delivered;
use tilolang_tilo::personal;

/// The places the sidebar shows picked, each once: its row is said in more than one layer of
/// the window's tree.
fn picked(session: &mut Session) -> Vec<String> {
    let mut picked: Vec<String> = super::accessible(session)
        .into_iter()
        .filter(|node| node.role == ui::a11y::Role::TreeItem && node.state.selected)
        .filter_map(|node| node.name)
        .collect();
    picked.dedup();
    picked
}

/// The window of Tasks of `route`'s workspace that takes the shell's tab `tab`, whose host keeps
/// what it keeps in `storage`.
fn tasks(route: &RouteSpec, tab: u64, storage: Option<std::path::PathBuf>) -> Session {
    let chunks = tilolang_tilo::chunk::Chunks::of(automerge::AutoCommit::new());
    let spec = super::Spec::new(super::TASKS, route.cluster_id.to_string(), tab, "ada")
        .in_language("en-US");
    let mut session = Session::brokered_as(route, spec, chunks, storage, super::Window::in_harness);
    session.settle();
    session
}

/// The person picks `place` in the sidebar.
fn pick(session: &mut Session, place: &str) {
    let (x, y) = place_at(session, place);
    session.window.click_at(x, y);
    session.settle();
    assert_eq!(picked(session), [place], "the sidebar shows {place} picked");
}

/// Whether the person's own entries of the document hold the run `run` as reviewed.
fn reviewed(session: &Session, run: &str) -> bool {
    session
        .instance()
        .personal()
        .of("ada")
        .runs
        .iter()
        .any(|reviewed| reviewed.run == run)
}

#[test]
fn the_place_a_person_picks_and_what_they_saw_are_theirs_again_after_a_restart() {
    let storage = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("hosted-personal-restart");
    let _ = std::fs::remove_dir_all(&storage);
    let route = RouteSpec::app(super::TASKS, uuid::Uuid::new_v4());
    let mut first = tasks(&route, 1, Some(storage.clone()));
    assert_ne!(picked(&mut first), ["Inbox"], "the window opens elsewhere");
    pick(&mut first, "Inbox");
    // They review a run: every change it made is seen.
    assert!(first.instance_host().runtime().mark_run_seen(
        &tilolang_tilo::runtime::Actor::Member(tilolang_tilo::runtime::ids::MemberId::person(
            "ada"
        )),
        "run-1"
    ));
    first.settle();
    assert!(
        first
            .instance()
            .personal()
            .of("ada")
            .slots
            .iter()
            .any(|slot| slot.person == "ada"),
        "the document keeps the place as the person's own view state"
    );
    assert!(
        reviewed(&first, "run-1"),
        "the document keeps the run as seen"
    );
    first.close();

    // The window opens again over what the device kept.
    let mut again = tasks(&route, 1, Some(storage));
    assert_eq!(
        picked(&mut again),
        ["Inbox"],
        "the place they picked is theirs again"
    );
    assert!(reviewed(&again, "run-1"));
    assert!(
        !again.instance_host().runtime().mark_run_seen(
            &tilolang_tilo::runtime::Actor::Member(tilolang_tilo::runtime::ids::MemberId::person(
                "ada"
            )),
            "run-1"
        ),
        "the run they reviewed is still seen"
    );
}

/// What `from` sent on the route of the document of each person's own state, delivered to `to` as
/// the broker relays it.
fn relay(from: &Session, to: &mut Session) {
    let sent: Vec<_> = from.wire().1.borrow_mut().drain(..).collect();
    let mut relayed = 0;
    for (route, request) in sent {
        let CollabRequest::Document { mutation, .. } = request else {
            continue;
        };
        if route.app_id != personal::OWNER {
            continue;
        }
        relayed += 1;
        to.inbox()
            .borrow_mut()
            .push_back(delivered(&route, &mutation).expect("the change encodes"));
    }
    assert!(
        relayed > 0,
        "the window sent the place on the document's route"
    );
    to.settle();
}

#[test]
fn the_place_a_person_picks_reaches_their_other_device() {
    let route = RouteSpec::app(super::TASKS, uuid::Uuid::new_v4());
    // The same window, the shell's same tab, on the laptop and on the phone.
    let mut laptop = tasks(&route, 1, None);
    let mut phone = tasks(&route, 1, None);
    laptop.wire().1.borrow_mut().clear();
    pick(&mut laptop, "Inbox");
    assert_ne!(picked(&mut phone), ["Inbox"]);
    relay(&laptop, &mut phone);
    assert_eq!(
        picked(&mut phone),
        ["Inbox"],
        "the phone shows the place picked on the laptop"
    );
}

#[test]
fn each_window_of_an_app_keeps_its_own_place() {
    let route = RouteSpec::app(super::TASKS, uuid::Uuid::new_v4());
    let mut first = tasks(&route, 1, None);
    let mut second = tasks(&route, 2, None);
    let before = picked(&mut second);
    first.wire().1.borrow_mut().clear();
    pick(&mut first, "Inbox");
    relay(&first, &mut second);
    assert_eq!(
        picked(&mut second),
        before,
        "the other window stays where it was"
    );
}
