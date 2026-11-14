//! A person's own state of a workspace in a hosted window: the place they pick and what they
//! mark seen are kept in the workspace's record of each person's own state, so they are still
//! theirs after the window opens again and reach the person's other devices.

use super::chunks::delivered;
use super::{Session, place_at};
use collaboration::{CollabRequest, RouteSpec};
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

/// A Tasks window of `route`'s workspace, whose host keeps what it keeps in `storage`.
fn tasks(route: &RouteSpec, storage: Option<std::path::PathBuf>) -> Session {
    let chunks = tilolang_tilo::chunk::Chunks::of(automerge::AutoCommit::new());
    Session::tasks_over_in(route, chunks, storage)
}

/// The person picks `place` in the sidebar.
fn pick(session: &mut Session, place: &str) {
    let (x, y) = place_at(session, place);
    session.window.click_at(x, y);
    session.settle();
    assert_eq!(picked(session), [place], "the sidebar shows {place} picked");
}

/// Whether the person's own entries of the record hold the run `run` as reviewed.
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
    let mut first = tasks(&route, Some(storage.clone()));
    assert_ne!(picked(&mut first), ["Inbox"], "the window opens elsewhere");
    pick(&mut first, "Inbox");
    // They review a run: every change it made is seen.
    assert!(
        first
            .instance_host()
            .runtime()
            .mark_run_seen("ada", "run-1")
    );
    first.settle();
    assert!(
        first
            .instance()
            .personal()
            .of("ada")
            .slots
            .iter()
            .any(|slot| slot.person == "ada"),
        "the record keeps the place as the person's own view state"
    );
    assert!(
        reviewed(&first, "run-1"),
        "the record keeps the run as seen"
    );
    first.close();

    // The window opens again over what the device kept.
    let mut again = tasks(&route, Some(storage));
    assert_eq!(
        picked(&mut again),
        ["Inbox"],
        "the place they picked is theirs again"
    );
    assert!(reviewed(&again, "run-1"));
    assert!(
        !again
            .instance_host()
            .runtime()
            .mark_run_seen("ada", "run-1"),
        "the run they reviewed is still seen"
    );
}

#[test]
fn the_place_a_person_picks_reaches_their_other_device() {
    let route = RouteSpec::app(super::TASKS, uuid::Uuid::new_v4());
    let mut laptop = tasks(&route, None);
    let mut phone = tasks(&route, None);
    laptop.wire().1.borrow_mut().clear();
    pick(&mut laptop, "Inbox");
    assert_ne!(picked(&mut phone), ["Inbox"]);

    // What the laptop sent on the record's route reaches the phone as the broker relays it.
    let sent: Vec<_> = laptop.wire().1.borrow_mut().drain(..).collect();
    let mut relayed = 0;
    for (to, request) in sent {
        let CollabRequest::Document { mutation, .. } = request else {
            continue;
        };
        if to.app_id != personal::OWNER {
            continue;
        }
        relayed += 1;
        phone
            .inbox()
            .borrow_mut()
            .push_back(delivered(&to, "ada", &mutation));
    }
    assert!(
        relayed > 0,
        "the laptop sent the place on the record's route"
    );
    phone.settle();
    assert_eq!(
        picked(&mut phone),
        ["Inbox"],
        "the phone shows the place picked on the laptop"
    );
}
