//! A refused write reaches its author, in a workspace of two people's at a site: the owner takes a
//! member's write permission away, the member edits their desk, and the workspace's device refuses
//! the change. The member's own activity feed names the change, the device it is not on, and why;
//! the owner's replica never holds it; and when the member may write again, the change lands and
//! the verdict goes.
//! The desk record the member writes is the one their own device founds: a device writes the shared
//! desk it holds, and a record a workspace holds but has not sent to this device is not one it
//! writes over. What the site's device then refuses, clears and files is the same for a record any
//! device founded. A member the site removes reads nothing more of the workspace, while the members
//! it keeps go on writing and reading its record.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live \
              until their sessions end"
)]

mod common;

// A site admits its members' records through an Automerge worker: this binary is that worker, as
// the session's own is.
tilo_automerge_isolate::test_worker!();

use common::people::{
    Device, show, titles_at, titles_on, two_people_at_a_site, wait_shown, waited, windows,
};
use serde_json::{Value, json};
use tilo_relay_protocol::workspaces::WorkspaceRole;

/// The app whose window the people here open on their desks: a note, as a package's window is one.
const NOTES: &str = "so.tilo.notes";

/// The workspace's activity as `device` reads it now: its changes, each with the devices that
/// refused it.
fn feed(device: &Device) -> Value {
    let started = device.act("tilo.activity", "workspace.activity\u{1f}feed", &json!({}));
    let number = started["answer"]["receipt"]["broker_invocation_id"]
        .as_u64()
        .unwrap_or_else(|| panic!("{} reads its activity: {started}", device.label));
    let status = device.ask(&json!({
        "op": "actions_status",
        "invocation_id": number,
        "wait_ms": 5_000,
    }));
    status["answer"]["receipt"]["update"]["value"].clone()
}

/// The refusals the feed of `device` names, each as its change's row reads it.
fn refusals(device: &Device) -> Vec<Value> {
    let feed = feed(device);
    feed["entries"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .flat_map(|entry| {
                    entry["refused"]
                        .as_array()
                        .map(Vec::as_slice)
                        .unwrap_or_default()
                        .iter()
                        .cloned()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What a device opens is a change of the workspace's shared desk, and the workspace's device takes
/// it: the first window founds the record, and the next change continues the one the device made
/// instead of numbering it again. The record is the workspace's, sealed in a workspace a site
/// hosts: the site keeps it and the other member's device reads it.
#[test]
fn the_workspace_keeps_the_desk_its_device_writes() {
    let (site, _ada, _bob, ada_laptop, bob_laptop) = two_people_at_a_site();
    show(&ada_laptop, &site);
    let _first = ada_laptop.window(NOTES, "Notes");
    wait_shown(&site, &[&ada_laptop], "Notes");
    let _next = ada_laptop.window(NOTES, "Another");
    wait_shown(&site, &[&ada_laptop], "Another");
    // What the workspace holds is the desk its device wrote, window by window.
    assert_eq!(
        titles_at(&site),
        ["Another", "Notes"],
        "the windows the desk was written with are the ones the site holds"
    );
    // And the desk the site keeps is the other member's too: what one member opened on this
    // workspace's desk, the other member's device reads and shows.
    show(&bob_laptop, &site);
    waited(
        "the other member's desk showing the workspace's windows",
        &site,
        &[&bob_laptop],
        || titles_on(&bob_laptop, &site) == ["Another", "Notes"],
    );
}

/// A window a member opens on a workspace's desk stays where they opened it while the workspace has
/// not taken it in yet: another member's change that reaches their device first is no close of it.
/// The member's write lands as well, and both people's desks show both windows. A member who may
/// not write keeps the window they opened on their own desk, however the record moves.
#[test]
fn a_window_a_member_opens_stays_while_another_member_changes_the_desk() {
    let (site, _ada, bob, ada_laptop, bob_laptop) = two_people_at_a_site();
    show(&ada_laptop, &site);
    show(&bob_laptop, &site);
    let _ada_first = ada_laptop.window(NOTES, "Ada's notes");
    wait_shown(&site, &[&ada_laptop, &bob_laptop], "Ada's notes");

    // Both open a window at once: each one's change reaches the other's device while its own is
    // still on its way to the workspace and back.
    let _bob_note = bob_laptop.window(NOTES, "Bob's notes");
    let _ada_plan = ada_laptop.window(NOTES, "Ada's plan");
    let both = ["Ada's notes", "Ada's plan", "Bob's notes"];
    let mut seen = false;
    waited(
        "both people's windows on both desks",
        &site,
        &[&ada_laptop, &bob_laptop],
        || {
            let shown = titles_on(&bob_laptop, &site);
            let holds = shown.contains(&"Bob's notes".to_owned());
            assert!(
                holds || !seen,
                "Bob's window left the workspace's desk on his device: {shown:?}"
            );
            seen |= holds;
            assert_eq!(
                bob_laptop.showing().as_deref(),
                Some(site.workspace.as_str()),
                "Bob's desk left the workspace he opened the window in"
            );
            titles_at(&site) == both
                && titles_on(&ada_laptop, &site) == both
                && titles_on(&bob_laptop, &site) == both
        },
    );

    // Bob may no longer write: what he opens now never reaches the workspace, and Ada's next change
    // reaches his device all the same.
    site.set_role(bob.public, WorkspaceRole::Viewer);
    let _bob_draft = bob_laptop.window(NOTES, "Bob's draft");
    waited("Bob's draft on his desk", &site, &[&bob_laptop], || {
        titles_on(&bob_laptop, &site).contains(&"Bob's draft".to_owned())
    });
    let _ada_list = ada_laptop.window(NOTES, "Ada's list");
    waited("Ada's list on Bob's desk", &site, &[&bob_laptop], || {
        titles_on(&bob_laptop, &site).contains(&"Ada's list".to_owned())
    });
    assert!(
        titles_on(&bob_laptop, &site).contains(&"Bob's draft".to_owned()),
        "Bob's own window stays on his desk: {:?}",
        titles_on(&bob_laptop, &site)
    );
    assert_eq!(
        bob_laptop.showing().as_deref(),
        Some(site.workspace.as_str()),
        "Bob's desk stays in the workspace"
    );
    assert!(
        !titles_at(&site).contains(&"Bob's draft".to_owned()),
        "the site never holds what Bob may not write"
    );
}

#[test]
#[ignore = "the workspace's refusal does not reach the member whose write it is: the \
            site keeps nothing of a write it refuses, and tells the member's device \
            nothing of the refusal, so the member's feed shows no refusal of his \
            change and no reason for it"]
fn a_member_reads_where_their_refused_change_is_not() {
    // Ada's site governs her workspace, hosts it, and keeps what its people record while they are
    // away. Both are editors, who write, and the workspace shares its people's layout, so what
    // either opens is on the workspace's desk.
    let (site, _ada, bob, _ada_laptop, mut bob_laptop) = two_people_at_a_site();
    // Bob's desk shows the workspace, whose shared desk is the record his apps' windows are written
    // into.
    show(&bob_laptop, &site);

    // Bob opens an app on the shared desk, whose window is a change of the workspace's record.
    let _bob_note = bob_laptop.window(NOTES, "Notes");
    waited(
        "Bob's window on the workspace's desk",
        &site,
        &[&bob_laptop],
        || windows(&site) >= 1,
    );

    // And a second one, while he may write: what the site holds is his desk,
    // and what he writes next is refused against it.
    let _bob_other = bob_laptop.window(NOTES, "Another");
    waited(
        "Bob's next window on the workspace's desk",
        &site,
        &[&bob_laptop],
        || windows(&site) >= 2,
    );

    // Ada takes Bob's write permission away: the workspace still lets him read what it holds, and
    // refuses what he writes.
    site.set_role(bob.public, WorkspaceRole::Viewer);
    let before = windows(&site);
    let refused_note = bob_laptop.window(NOTES, "Bob's notes");
    let refused_at = std::time::Instant::now();
    waited(
        "Bob reads where his change is not",
        &site,
        &[&bob_laptop],
        || !refusals(&bob_laptop).is_empty(),
    );
    let verdict = refusals(&bob_laptop)
        .into_iter()
        .next()
        .expect("the feed names the refused change");
    // The change is not on the site's device, and the site says why: no permission, and nothing Bob
    // can wait for changes that.
    assert_eq!(
        verdict["label"].as_str(),
        Some("The site"),
        "the refusal names the device the change is not on: {verdict}"
    );
    assert_eq!(
        verdict["reason"].as_str(),
        Some("no permission"),
        "the site refused the write for want of permission: {verdict}"
    );
    assert_eq!(
        verdict["permanent"],
        Value::Bool(true),
        "the site will not take the write while Bob may not write: {verdict}"
    );
    assert!(
        !verdict["change"].as_str().unwrap_or_default().is_empty(),
        "the refusal names the change Bob's feed rows: {verdict}"
    );
    assert!(
        verdict["device"]
            .as_str()
            .is_some_and(|device| device == hex::encode(site.host.device_public_key)),
        "the refusal is the site's own device's: {verdict}"
    );
    assert_eq!(
        windows(&site),
        before,
        "the site never holds what it refused, {refused_at:?} ago"
    );
    // The verdict is the workspace's own state, not a live word: Bob's device goes away and comes
    // back, and reads the refusal the site filed as it reads anything else the workspace holds.
    drop(refused_note);
    bob_laptop.stop();
    bob_laptop.start();
    bob_laptop.wait_holding(&site.workspace);
    show(&bob_laptop, &site);
    tilo_test_env::wait_for("Bob reads the refusal after a restart", || {
        let kept = refusals(&bob_laptop);
        kept.iter()
            .any(|refusal| refusal["change"] == verdict["change"])
            .then_some(())
    });

    // Ada lets Bob write again: his change lands, and the verdict goes.
    site.set_role(bob.public, WorkspaceRole::Editor);
    let _admitted_note = bob_laptop.window(NOTES, "Bob's notes, again");
    waited(
        "Bob's next change at the site",
        &site,
        &[&bob_laptop],
        || windows(&site) > before,
    );
    waited(
        "Bob's feed without a refusal",
        &site,
        &[&bob_laptop],
        || refusals(&bob_laptop).is_empty(),
    );
    assert!(
        refusals(&bob_laptop).is_empty(),
        "what the workspace holds is not missing anywhere: {}",
        bob_laptop.log()
    );
}

/// A member the site's owner removes reads nothing more of the workspace, while the member it keeps
/// writes and reads as before: what the workspace records after the removal never reaches the
/// removed member's device, and what he writes reaches neither the site nor the other member.
#[test]
fn a_member_the_site_removes_stops_reading_what_the_workspace_records() {
    let (site, _ada, bob, ada_laptop, bob_laptop) = two_people_at_a_site();
    show(&ada_laptop, &site);
    show(&bob_laptop, &site);
    let _bob_note = bob_laptop.window(NOTES, "Bob's notes");
    wait_shown(&site, &[&ada_laptop], "Bob's notes");

    // The site's owner removes Bob and every device of his.
    site.remove(bob.public);
    site.wait_for_removal(bob.public);

    // Ada's device writes the workspace's desk after the removal and reads it back: the route is
    // live for the member the workspace keeps.
    let _after = ada_laptop.window(NOTES, "After the removal");
    wait_shown(&site, &[&ada_laptop], "After the removal");

    // The removed member's device never reads it, nor what it keeps from then on, and what he
    // writes reaches neither the site nor Ada's device.
    let kept = titles_at(&site);
    let _bob_again = bob_laptop.window(NOTES, "Bob's notes, after the removal");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < until {
        let (at_site, on_ada, on_bob) = (
            titles_at(&site),
            titles_on(&ada_laptop, &site),
            titles_on(&bob_laptop, &site),
        );
        assert!(
            at_site == kept
                && on_ada == kept
                && !on_bob.iter().any(|title| title == "After the removal"),
            "the site and Ada's device keep nothing Bob writes after his removal, and Bob's \
             device reads nothing the workspace writes: the site keeps {at_site:?}, Ada's device \
             reads {on_ada:?}, Bob's reads {on_bob:?}\n{}",
            bob_laptop.log()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
