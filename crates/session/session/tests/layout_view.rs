//! A workspace's layout is one record of the workspace, and each person's view of it is a record
//! of theirs: two people in one workspace see the same strips and windows, and each keeps the
//! window they are working in in front, on every device of theirs.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion, and the devices live \
              until their sessions end"
)]

use crate::common::people::{Site, show, titles_at, titles_on, two_people_at_a_site, waited};
use tilo_desktop_model::ownership::DESKTOP_APP;
use tilo_desktop_model::person_view::PersonView;

/// The app whose window the people here open on their desks.
const NOTES: &str = "so.tilo.notes";

/// Every view of the workspace's layout the site holds, each as the person whose it is and the
/// view itself.
fn views(site: &Site) -> Vec<(String, PersonView)> {
    site.records()
        .into_iter()
        .filter(|(app, record)| app == DESKTOP_APP && record.starts_with("view-of-"))
        .filter_map(|(_, record)| {
            let bytes = site.record(DESKTOP_APP, &record)?;
            let document = automerge::AutoCommit::load(&bytes).ok()?;
            Some((record, PersonView::read(&document)))
        })
        .collect()
}

/// The window each person has in front of the group they opened it in, by person.
fn fronts(views: &[(String, PersonView)]) -> Vec<Vec<u64>> {
    views
        .iter()
        .map(|(_, view)| view.front_tab.values().copied().collect())
        .collect()
}

/// The workspace's windows and strips are the workspace's, and where each person is looking is
/// theirs: both desks show the one layout, and each person's view of it is a record of the
/// workspace of their own.
#[test]
fn each_person_keeps_their_own_view_of_the_workspace_layout() {
    let (site, _ada, _bob, ada_laptop, bob_laptop) = two_people_at_a_site();
    show(&ada_laptop, &site);
    show(&bob_laptop, &site);
    let _hers = ada_laptop.window(NOTES, "Ada's notes");
    let _his = bob_laptop.window(NOTES, "Bob's notes");
    let both = ["Ada's notes", "Bob's notes"];
    waited(
        "both people's windows on both desks",
        &site,
        &[&ada_laptop, &bob_laptop],
        || {
            titles_at(&site) == both
                && titles_on(&ada_laptop, &site) == both
                && titles_on(&bob_laptop, &site) == both
        },
    );

    // Each person's view of the layout reached the workspace as a record of its own.
    waited("a view of the layout for each person", &site, &[], || {
        views(&site).len() == 2
    });
    let held = views(&site);
    assert_eq!(held.len(), 2, "one view per person");
    let fronts = fronts(&held);
    assert!(
        fronts.iter().all(|front| front.len() == 2),
        "each view names the window in front of each group: {held:?}\n{}\n{}\n{}\n{}",
        site.refusals_report(),
        ada_laptop.diagnostic(),
        bob_laptop.diagnostic(),
        site.records().len()
    );
}

/// A window one person moves into another strip is where the other person sees it: the strips are
/// the workspace's, whichever desk arranged them.
#[test]
fn a_window_one_person_moves_is_where_the_other_sees_it() {
    let (site, _ada, _bob, ada_laptop, bob_laptop) = two_people_at_a_site();
    show(&ada_laptop, &site);
    show(&bob_laptop, &site);
    let _first = ada_laptop.window(NOTES, "Notes");
    waited(
        "Ada's window on both desks",
        &site,
        &[&ada_laptop, &bob_laptop],
        || titles_on(&ada_laptop, &site) == ["Notes"] && titles_on(&bob_laptop, &site) == ["Notes"],
    );
    // Bob moves his window beside Ada's: one strip now holds both, as both desks show it.
    let _second = bob_laptop.window(NOTES, "Plan");
    waited(
        "both windows on both desks",
        &site,
        &[&ada_laptop, &bob_laptop],
        || {
            let both = ["Notes", "Plan"];
            titles_at(&site) == both
                && titles_on(&ada_laptop, &site) == both
                && titles_on(&bob_laptop, &site) == both
        },
    );
    let desk = site.desktop().expect("the workspace holds the layout");
    assert_eq!(
        desk.workspaces
            .first()
            .map(|workspace| workspace.strips.len()),
        Some(1),
        "one strip holds both windows"
    );
}
