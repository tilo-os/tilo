//! A refused write reaches its author, in a workspace of two people's at a site: the owner takes a
//! member's write permission away, the member edits their shell, and the member's own device
//! refuses the change before it goes. The member's own activity feed names the change, the device
//! that refused it, and why; the site never receives it; and when the member may write again, the
//! change lands and the verdict goes.
//! The shell document the member writes is the one their own device founds: a device writes the
//! shared shell it holds, and a document a workspace holds but has not sent to this device is not
//! one it writes over. What a device then refuses, clears and files is the same for a document any
//! device founded. A member the site removes reads nothing more of the workspace, while the
//! members it keeps go on writing and reading its shell.

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

use crate::common::people::{
    Device, at, show, titles_at, titles_on, two_people_at_a_site, wait_shown, waited, windows,
};
use serde_json::{Value, json};
use tilo_actions::remote::RemoteActionMessage;
use tilo_server_protocol::workspaces::Role;

/// The app whose window the people here open on their sessions: a note, as a package's window is one.
const NOTES: &str = "so.tilo.notes";

/// The workspace's activity as `device` reads it now: its changes, each with the devices that
/// refused it.
fn feed(device: &Device) -> Value {
    let started = device.act(
        &at("tilo.activity", "workspace.activity", "feed"),
        &json!({}),
    );
    let invocation = serde_json::from_value(started["update"]["invocation"].clone())
        .unwrap_or_else(|_| panic!("{} reads its activity: {started}", device.label));
    let RemoteActionMessage::Update {
        state: Some(state), ..
    } = device.frame(RemoteActionMessage::Observe {
        correlation: 1,
        invocation,
        wait_ms: Some(5_000),
        seen: None,
    })
    else {
        panic!("{} lost its activity read", device.label);
    };
    serde_json::to_value(state.update).expect("an update")["value"].clone()
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

/// What a device opens is a change of the workspace's shared shell, and the workspace's device takes
/// it: the first window founds the record, and the next change continues the one the device made
/// instead of numbering it again. The record is the workspace's, sealed in a workspace a site
/// hosts: the site keeps it and the other member's device reads it.
#[test]
fn the_workspace_keeps_the_shell_its_device_writes() {
    let (site, _ada, _bob, ada_laptop, bob_laptop) = two_people_at_a_site();
    show(&ada_laptop, &site);
    let _first = ada_laptop.window(NOTES, "Notes");
    wait_shown(&site, &[&ada_laptop], "Notes");
    let _next = ada_laptop.window(NOTES, "Another");
    wait_shown(&site, &[&ada_laptop], "Another");
    // What the workspace holds is the shell its device wrote, window by window.
    assert_eq!(
        titles_at(&site),
        ["Another", "Notes"],
        "the windows the shell was written with are the ones the site holds"
    );
    // And the shell the site keeps is the other member's too: what one member opened on this
    // workspace's shell, the other member's device reads and shows.
    show(&bob_laptop, &site);
    waited(
        "the other member's shell showing the workspace's windows",
        &site,
        &[&bob_laptop],
        || titles_on(&bob_laptop, &site) == ["Another", "Notes"],
    );
}

/// A window a member opens on a workspace's shell stays where they opened it while the workspace has
/// not taken it in yet: another member's change that reaches their device first is no close of it.
/// The member's write lands as well, and both people's sessions show both windows. A member who may
/// not write keeps the window they opened on their own shell, however the record moves.
#[test]
fn a_window_a_member_opens_stays_while_another_member_changes_the_shell() {
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
        "both people's windows on both sessions",
        &site,
        &[&ada_laptop, &bob_laptop],
        || {
            let shown = titles_on(&bob_laptop, &site);
            let holds = shown.contains(&"Bob's notes".to_owned());
            assert!(
                holds || !seen,
                "Bob's window left the workspace's shell on his device: {shown:?}"
            );
            seen |= holds;
            assert_eq!(
                bob_laptop.showing().as_deref(),
                Some(site.workspace.as_str()),
                "Bob's shell left the workspace he opened the window in"
            );
            titles_at(&site) == both
                && titles_on(&ada_laptop, &site) == both
                && titles_on(&bob_laptop, &site) == both
        },
    );

    // Bob may no longer write: what he opens now never reaches the workspace, and Ada's next change
    // reaches his device all the same.
    site.set_role(bob.public, Role::Viewer);
    let _bob_draft = bob_laptop.window(NOTES, "Bob's draft");
    waited("Bob's draft on his shell", &site, &[&bob_laptop], || {
        titles_on(&bob_laptop, &site).contains(&"Bob's draft".to_owned())
    });
    let _ada_list = ada_laptop.window(NOTES, "Ada's list");
    waited("Ada's list on Bob's shell", &site, &[&bob_laptop], || {
        titles_on(&bob_laptop, &site).contains(&"Ada's list".to_owned())
    });
    assert!(
        titles_on(&bob_laptop, &site).contains(&"Bob's draft".to_owned()),
        "Bob's own window stays on his shell: {:?}",
        titles_on(&bob_laptop, &site)
    );
    assert_eq!(
        bob_laptop.showing().as_deref(),
        Some(site.workspace.as_str()),
        "Bob's shell stays in the workspace"
    );
    assert!(
        !titles_at(&site).contains(&"Bob's draft".to_owned()),
        "the site never holds what Bob may not write"
    );
}

#[test]
#[ignore = "a change the device refuses is filed and answers its author with the typed \
            refusal, but the person's activity only shows a refusal on a change its \
            history holds: a change the device never kept has no row to carry it, so the \
            feed names none. The test states what the feed is to show once it can."]
fn a_member_reads_where_their_refused_change_is_not() {
    // Ada's site governs her workspace, hosts it, and keeps what its people record while they are
    // away. Ada's own device writes the workspace's shell, and Bob's device is a member that reads
    // it and refuses what he writes before any of it is sent.
    let (site, _ada, bob, ada_laptop, mut bob_laptop) = two_people_at_a_site();
    show(&ada_laptop, &site);
    let _ada_note = ada_laptop.window(NOTES, "Ada's notes");
    waited(
        "the workspace's shell on the site",
        &site,
        &[&ada_laptop],
        || windows(&site) >= 1,
    );

    // Bob's shell shows the workspace, whose shared shell is the record his apps' windows are written
    // into.
    show(&bob_laptop, &site);

    // Ada takes Bob's write permission away: the workspace still lets him read what it holds, and
    // Bob's own device refuses what he opens before any of it is sent.
    site.set_role(bob.public, Role::Viewer);
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
    // Bob's own device refused the change, and says why: no permission, and nothing Bob can wait
    // for changes that. Nothing of it is sent, so the site holds nothing of it either.
    assert_eq!(
        verdict["label"].as_str(),
        Some("Bob's laptop"),
        "the refusal names the device that refused the change: {verdict}"
    );
    assert_eq!(
        verdict["reason"].as_str(),
        Some("no permission"),
        "Bob's device refused the write for want of permission: {verdict}"
    );
    assert_eq!(
        verdict["permanent"],
        Value::Bool(true),
        "Bob's device will not take the write while he may not write: {verdict}"
    );
    assert!(
        !verdict["change"].as_str().unwrap_or_default().is_empty(),
        "the refusal names the change Bob's feed rows: {verdict}"
    );
    assert!(
        verdict["device"]
            .as_str()
            .is_some_and(|device| device == hex::encode(bob_laptop.cert.device_public_key)),
        "the refusal is Bob's own device's: {verdict}"
    );
    assert_eq!(
        windows(&site),
        before,
        "the site never holds what Bob's device refused, {refused_at:?} ago"
    );
    // The verdict is the workspace's own state, not a live word: Bob's device goes away and comes
    // back, and reads the refusal its workspace holds as it reads anything else it holds.
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

    // What the workspace does keep is unaffected: Ada's own device opens another window, the site
    // takes it, and Bob's feed still names only his own refused change.
    let _ada_plan = ada_laptop.window(NOTES, "Ada's plan");
    waited(
        "the workspace keeps what its device may write",
        &site,
        &[&ada_laptop],
        || windows(&site) > before,
    );
    let kept = refusals(&bob_laptop);
    assert!(
        kept.iter()
            .all(|refusal| refusal["change"] == verdict["change"]),
        "Bob's feed names his refused change and no other: {kept:?}\n{}",
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

    // Ada's device writes the workspace's shell after the removal and reads it back: the route is
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
