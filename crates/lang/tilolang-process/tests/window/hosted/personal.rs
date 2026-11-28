//! A person's own state of a workspace in a hosted window: the place they pick and what they
//! mark seen are kept in the workspace's store, so they are still theirs after the window opens
//! again.

use super::{Session, place_at};
use collaboration::RouteSpec;

/// The places the sidebar shows picked, each once: its row is said in more than one layer of
/// the window's tree.
fn picked(session: &mut Session) -> Vec<String> {
    let mut picked: Vec<String> = super::accessible(session)
        .into_iter()
        .filter(|node| node.role == ui::a11y::Role::TreeItem && node.state.selected)
        .filter_map(|node| node.shown_name().map(str::to_owned))
        .collect();
    picked.dedup();
    picked
}

/// The window of Tasks of `route`'s workspace that takes the shell's tab `tab`, whose workspace
/// is kept in `dir` (in memory when `None`).
fn tasks(route: &RouteSpec, tab: u64, dir: Option<&std::path::Path>) -> Session {
    if let Some(dir) = dir {
        return Session::in_store(super::TASKS, "ada", dir, super::Window::in_harness);
    }
    let spec = super::Spec::new(super::TASKS, route.cluster_id.to_string(), tab, "ada")
        .in_language("en-US");
    Session::brokered_as(route, spec, super::Window::in_harness)
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
    let mut first = tasks(&route, 1, Some(&storage));
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
    let mut again = tasks(&route, 1, Some(&storage));
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

/// A person quits Gallery on a page and opens it again: the shell gives the new window a tab of
/// its own, with no place kept under it, and the window opens on the page they left rather than
/// on the first. That page is theirs: another member opening Gallery over the same workspace
/// starts on the first page.
#[test]
fn an_app_opened_again_after_a_quit_shows_the_page_the_person_left() {
    let storage = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("hosted-personal-reopen");
    let _ = std::fs::remove_dir_all(&storage);
    let gallery = |person, tab| {
        Session::in_store_at(
            super::GALLERY,
            person,
            &storage,
            tab,
            super::Window::in_harness,
        )
    };
    let mut first = gallery("ada", 1);
    assert_eq!(
        picked(&mut first),
        ["Basics"],
        "a new gallery opens on its first page"
    );
    pick(&mut first, "States");
    first.close();

    let mut again = gallery("ada", 2);
    assert_eq!(
        picked(&mut again),
        ["States"],
        "the window opened again shows the page the person left"
    );
    // What they pick in this window is where they are now, for the next window too.
    pick(&mut again, "Release board");
    again.close();
    let mut third = gallery("ada", 3);
    assert_eq!(picked(&mut third), ["Release board"]);
    third.close();

    let mut other = gallery("grace", 4);
    assert_eq!(
        picked(&mut other),
        ["Basics"],
        "another member's window does not open on Ada's page"
    );
}
