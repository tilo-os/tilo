//! A refused write reaches its author, in a workspace of two people's at a
//! site: the owner takes a member's write permission away, the member edits
//! their desk, and the workspace's device refuses the change. The member's own
//! activity feed names the change, the device it is not on, and why; the
//! owner's replica never holds it; and when the member may write again, the
//! change lands and the verdict goes.
//!
//! The desk record the member writes is the one their own device founds: a
//! device writes the shared desk it holds, and a record a workspace holds but
//! has not sent to this device is not one it writes over. What the site's
//! device then refuses, clears and files is the same for a record any device
//! founded.

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

// A site admits its members' records through an Automerge worker: this binary
// is that worker, as the session's own is.
tilo_automerge_isolate::test_worker!();

use common::people::{Device, Person, Site};
use serde_json::{Value, json};
use tilo_relay_protocol::workspaces::WorkspaceRole;

/// The app whose window the people here open on their desks: a note, as a
/// package's window is one.
const NOTES: &str = "so.tilo.notes";

/// The desk's own action that brings a workspace to the front, by its key:
/// the session's `session:desk`, whose reading is the surface the desk shows.
const SWITCH_WORKSPACE: &str = "session:desk\u{1f}switch-workspace";

/// How many windows the workspace's shared desk holds, as the site keeps it.
fn windows(site: &Site) -> usize {
    site.desktop().map_or(0, |desk| {
        desk.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .map(|strip| strip.window_groups.len())
            .sum()
    })
}

/// Bring the workspace `site` serves to the front on `device`'s desk, so the
/// app it opens is a window of that workspace's shared desk.
fn show(device: &Device, site: &Site) {
    let state = device.state();
    let key = state["state"]["held_workspaces"]
        .as_array()
        .and_then(|held| {
            held.iter()
                .find(|workspace| workspace["room"].as_str() == Some(site.workspace.as_str()))
                .and_then(|workspace| workspace["workspace"].as_u64())
        })
        .unwrap_or_else(|| {
            panic!(
                "{} holds no key for the workspace: {state}\n{}",
                device.label,
                device.log()
            )
        });
    let shown = device.act(
        "so.tilo.session",
        SWITCH_WORKSPACE,
        &json!({"workspace": key.to_string()}),
    );
    assert!(
        shown.get("err").is_none(),
        "{} shows {}: {shown}\n{}",
        device.label,
        site.name,
        device.log()
    );
}

/// Open a note on `device`'s desk, as the person at it does: a window of the
/// workspace it shows, and so a change of the workspace's shared desk, which
/// the workspace's device admits or refuses. The window stays up while the
/// value lives.
fn open(device: &Device, title: &str) -> common::window::Client {
    device.window(NOTES, title)
}

/// The workspace's activity as `device` reads it now: its changes, each with
/// the devices that refused it.
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

/// Wait until `condition` holds, and if it never does, fail with what the
/// devices logged and what the site's desk holds: a refusal that reaches
/// nobody says why in the session's log.
fn waited(what: &str, site: &Site, devices: &[&Device], mut condition: impl FnMut() -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    while !condition() {
        if std::time::Instant::now() >= deadline {
            let logs: Vec<String> = devices
                .iter()
                .map(|device| {
                    format!(
                        "{}:\nits refusals: {:#?}\nits windows: {}\nits desk: {}\n{}",
                        device.label,
                        refusals(device),
                        device.ask(&json!({"op": "windows"})),
                        device.state()["state"]["workspace_tree"],
                        device.log()
                    )
                })
                .collect();
            panic!(
                "gave up waiting for {what}; the site's refusals: {}\nthe site's desk: {}\nthe devices logged:\n{}",
                site.refusals_report(),
                site.desk_report(),
                logs.join("\n")
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The windows `device` shows on `site`'s workspace, by title: the desk it
/// reads out of the workspace's shared desktop record.
fn titles_on(device: &Device, site: &Site) -> Vec<String> {
    let state = device.state();
    let tree: tilo_desktop_model::DesktopTree =
        serde_json::from_value(state["state"]["workspace_tree"].clone()).unwrap_or_else(|error| {
            panic!("{}'s desk does not read: {error}\n{state}", device.label)
        });
    let mut titles: Vec<String> = tree
        .workspaces
        .iter()
        .filter(|workspace| workspace.id().to_string() == site.workspace)
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| tab.title.clone())
        .collect();
    titles.sort_unstable();
    titles
}

/// Two people, one of whom hosts the workspace they share: Ada's site governs
/// it, and both devices join it.
fn two_people_at_a_site() -> (Site, Person, Person, Device, Device) {
    let ada = Person::new("Ada");
    let bob = Person::new("Bob");
    let site = Site::start(&ada, "Studio");
    // The workspace's host lets its desk author its records, as the host of a
    // workspace does before its people open windows there: a member's device
    // cannot grant that for itself, and without it the desk a member opens
    // founds nothing the site keeps.
    site.author_apps(&[tilo_desktop_model::ownership::DESKTOP_APP]);
    let mut ada_laptop = ada.device("Ada's laptop");
    let mut bob_laptop = bob.device("Bob's laptop");
    let hers = site.invite(ada.public, WorkspaceRole::Editor);
    ada_laptop.join(&hers);
    ada_laptop.wait_holding(&site.workspace);
    let his = site.invite(bob.public, WorkspaceRole::Editor);
    bob_laptop.join(&his);
    bob_laptop.wait_holding(&site.workspace);
    site.wait_for_member(bob.public);
    (site, ada, bob, ada_laptop, bob_laptop)
}

/// What a device opens is a change of the workspace's shared desk, and the
/// workspace's device takes it: the first window founds the record, and the
/// next change continues the one the device made instead of numbering it
/// again. The record is the workspace's, sealed in a workspace a site hosts:
/// the site keeps it and the other member's device reads it.
#[test]
fn the_workspace_keeps_the_desk_its_device_writes() {
    let (site, _ada, _bob, ada_laptop, bob_laptop) = two_people_at_a_site();
    show(&ada_laptop, &site);
    let _first = open(&ada_laptop, "Notes");
    waited(
        "a window on the workspace's desk",
        &site,
        &[&ada_laptop],
        || windows(&site) >= 1,
    );
    let _next = open(&ada_laptop, "Another");
    waited(
        "the next window on the workspace's desk",
        &site,
        &[&ada_laptop],
        || windows(&site) >= 2,
    );
    // What the workspace holds is the desk its device wrote, window by window.
    let desk = site.desktop().expect("the site keeps the workspace's desk");
    let mut shown: Vec<&str> = desk
        .workspaces
        .iter()
        .flat_map(|workspace| &workspace.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .map(|tab| tab.title.as_str())
        .collect();
    shown.sort_unstable();
    assert_eq!(
        shown,
        ["Another", "Notes"],
        "the windows the desk was written with are the ones the site holds"
    );
    // And the desk the site keeps is the other member's too: what one member
    // opened on this workspace's desk, the other member's device reads and
    // shows.
    show(&bob_laptop, &site);
    waited(
        "the other member's desk showing the workspace's windows",
        &site,
        &[&bob_laptop],
        || titles_on(&bob_laptop, &site) == ["Another", "Notes"],
    );
}

#[test]
#[ignore = "the workspace's refusal does not reach the member whose write it is: the \
            site keeps nothing of a write it refuses, and tells the member's device \
            nothing of the refusal, so the member's feed shows no refusal of his \
            change and no reason for it"]
fn a_member_reads_where_their_refused_change_is_not() {
    // Ada's site governs her workspace, hosts it, and keeps what its people
    // record while they are away. Both are editors, who write, and the
    // workspace shares its people's layout, so what either opens is on the
    // workspace's desk.
    let (site, _ada, bob, _ada_laptop, mut bob_laptop) = two_people_at_a_site();
    // Bob's desk shows the workspace, whose shared desk is the record his
    // apps' windows are written into.
    show(&bob_laptop, &site);

    // Bob opens an app on the shared desk, whose window is a change of the
    // workspace's record.
    let _bob_note = open(&bob_laptop, "Notes");
    waited(
        "Bob's window on the workspace's desk",
        &site,
        &[&bob_laptop],
        || windows(&site) >= 1,
    );

    // And a second one, while he may write: what the site holds is his desk,
    // and what he writes next is refused against it.
    let _bob_other = open(&bob_laptop, "Another");
    waited(
        "Bob's next window on the workspace's desk",
        &site,
        &[&bob_laptop],
        || windows(&site) >= 2,
    );

    // Ada takes Bob's write permission away: the workspace still lets him read
    // what it holds, and refuses what he writes.
    site.set_role(bob.public, WorkspaceRole::Viewer);
    let before = windows(&site);
    let refused_note = open(&bob_laptop, "Bob's notes");
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
    // The change is not on the site's device, and the site says why: no
    // permission, and nothing Bob can wait for changes that.
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
    // The verdict is the workspace's own state, not a live word: Bob's device
    // goes away and comes back, and reads the refusal the site filed as it
    // reads anything else the workspace holds.
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
    let _admitted_note = open(&bob_laptop, "Bob's notes, again");
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
