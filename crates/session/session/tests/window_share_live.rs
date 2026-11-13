//! A window one device of a person shares, pictured on their other device: two real sessions, each
//! with its own endpoint, its own engine and its own live plane, as two machines on one network
//! run them.
//!
//! A shared window's pictures travel the workspace's live plane, on a track of their own that the
//! viewing device asks the sharing device for. The plane is carried by the device hosting the
//! workspace, or by the site that does, and a device is in it only once it holds the keys of the
//! workspace's media channel and reached that relay. The shares' own tests put every member on one
//! relay in one process and carry the control messages by hand, so none of that is in them: here
//! the workspace is made by one device and reaches the other through the person's list, the share
//! is asked for by the tab alone, and what the viewing session says it received is what arrived.

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

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::people::{Device, log_sessions_at, two_devices_of_one_person};

/// The app whose window is shared.
const NOTES: &str = "so.tilo.notes";
/// The provider of the session's own actions.
const SESSION: &str = "so.tilo.session";
/// What the devices' sessions log here: the live plane, the shares and the routes between the
/// devices say how far a share came only below `info`.
const LOGGED: &str = "info,tilo_session::live=debug,tilo_session::window_share=debug,\
                      tilo_network_actor=debug,tilo_session_client=warn";
/// How many lines of one kind a failed step shows of a session's log.
const SHOWN: usize = 120;

/// What `device`'s session says of its window shares: `sharing`, `starting`, `watching` and
/// `received`, as its `session:window-share` resource reads.
fn shares(device: &Device) -> Value {
    device.state()["state"]["sharing"].clone()
}

/// The row of the window `tab` in `part` of `reading`.
fn row_of(reading: &Value, part: &str, tab: &str) -> Option<Value> {
    reading[part]
        .as_array()?
        .iter()
        .find(|row| row["tab"].as_str() == Some(tab))
        .cloned()
}

/// The lines of `log` that `keep` keeps, the last [`SHOWN`] of them, none longer than a screen.
fn lines_of(log: &str, keep: impl Fn(&str) -> bool) -> String {
    let kept: Vec<&str> = log.lines().filter(|line| keep(line)).collect();
    kept.iter()
        .skip(kept.len().saturating_sub(SHOWN))
        .map(|line| line.chars().take(700).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// How far a share came on each of `devices`, for a step that never happened: what each session
/// says of its shares and of who hosts `workspace`, who it says is present there, and what it
/// logged of the live plane, of the shares and of the windows' control messages.
fn report(workspace: &str, devices: &[&Device]) -> String {
    let mut out = String::new();
    for device in devices {
        let log = device.log();
        let _ = writeln!(
            out,
            "== {} (device {}) ==\nshares: {}\nhosts: {}\npresent in {workspace}: {:?}\n\
             -- the live plane --\n{}\n-- the shares --\n{}\n-- the windows' messages --\n{}\n\
             -- warnings --\n{}",
            device.label,
            hex::encode(device.cert.device_public_key),
            shares(device),
            device.ask(&json!({"op": "workspace_hosts"}))["answer"]["workspaces"],
            device.present(workspace),
            lines_of(&log, |line| line.contains("tilo_session::live")),
            lines_of(&log, |line| line.contains("tilo_session::window_share")),
            lines_of(&log, |line| {
                line.contains("tilo_network_actor")
                    && ["Surface", "surface", "view"]
                        .iter()
                        .any(|word| line.contains(word))
            }),
            lines_of(&log, |line| line.contains(" WARN ")
                || line.contains(" ERROR ")),
        );
    }
    out
}

/// Wait until `holds`, and if it does not within `within`, fail with how far the share came on
/// each of `devices`.
#[track_caller]
fn eventually(
    what: &str,
    within: Duration,
    workspace: &str,
    devices: &[&Device],
    mut holds: impl FnMut() -> bool,
) {
    let deadline = Instant::now() + within;
    while !holds() {
        assert!(
            Instant::now() < deadline,
            "{what}: not within {within:?}\n{}",
            report(workspace, devices)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Bring `workspace` to the front of `device`'s desk, so the window it opens next is a window of
/// that workspace.
fn bring_to_front(device: &Device, workspace: &str) {
    let key = tilo_test_env::wait_for(format!("{} holds {workspace}", device.label), || {
        device.state()["state"]["held_workspaces"]
            .as_array()?
            .iter()
            .find(|held| held["room"].as_str() == Some(workspace))
            .and_then(|held| held["workspace"].as_u64())
    });
    let shown = device.act(
        SESSION,
        "session:desk\u{1f}switch-workspace",
        &json!({"workspace": key.to_string()}),
    );
    assert!(
        shown.get("err").is_none(),
        "{} shows {workspace}: {shown}",
        device.label
    );
    device.wait_showing(workspace);
}

/// The tab `device`'s desk holds the window titled `title` in, in `workspace`: its id, as the
/// share's action takes it, and the tile it shows.
fn tab_titled(device: &Device, workspace: &str, title: &str) -> Option<(String, String)> {
    let state = device.state();
    let tree: tilo_desktop_model::DesktopTree =
        serde_json::from_value(state["state"]["workspace_tree"].clone()).ok()?;
    tree.workspaces
        .iter()
        .filter(|held| held.id().to_string() == workspace)
        .flat_map(|held| &held.strips)
        .flat_map(|strip| &strip.window_groups)
        .flat_map(|group| &group.tabs)
        .find(|tab| tab.title == title)
        .map(|tab| {
            (
                tab.id.to_string(),
                serde_json::to_string(&tab.tile).unwrap_or_default(),
            )
        })
}

/// A workspace `maker` makes and hosts, which `other`, a further device of the same person, takes
/// from the person's list and holds: its id.
fn made_on(maker: &Device, other: &Device) -> String {
    let before = maker.on_desk();
    let made = maker.act_wait("actions", "workspace.create", &json!({}));
    assert_eq!(
        made["answer"]["receipt"]["update"]["state"],
        json!("succeeded"),
        "{} makes a workspace: {made}\n{}",
        maker.label,
        maker.diagnostic()
    );
    tilo_test_env::wait_until(
        format!("{}'s desk holds the workspace it made", maker.label),
        || maker.on_desk().len() > before.len(),
    );
    let made = maker
        .on_desk()
        .into_iter()
        .find(|workspace| !before.contains(workspace))
        .expect("the workspace the device made");
    eventually(
        &format!("{} holds {made}, made on {}", other.label, maker.label),
        Duration::from_secs(90),
        &made,
        &[maker, other],
        || other.held().contains(&made),
    );
    made
}

/// `sharer` opens a window in `workspace` and shares it by its tab, and `viewer`, another device
/// of the workspace, holds it: each step a share takes, in the order it takes them, so the first
/// that does not happen is the one the failure names.
fn the_shared_window_arrives(workspace: &str, sharer: &Device, viewer: &Device, title: &str) {
    let both = [sharer, viewer];
    bring_to_front(sharer, workspace);
    bring_to_front(viewer, workspace);
    let _window = sharer.window(NOTES, title);
    eventually(
        &format!("{}'s desk holds the window's tab", sharer.label),
        Duration::from_secs(30),
        workspace,
        &both,
        || tab_titled(sharer, workspace, title).is_some(),
    );
    let (tab, shows) = tab_titled(sharer, workspace, title).expect("the window's tab");
    let asked = sharer.act(
        SESSION,
        "session:window-share\u{1f}share",
        &json!({"tab": tab}),
    );
    assert!(
        asked.get("err").is_none(),
        "{} shares tab {tab} ({shows}): {asked}",
        sharer.label
    );

    // The sharing device captured the window and started its stream.
    eventually(
        &format!("{} says it shares tab {tab} ({shows})", sharer.label),
        Duration::from_secs(40),
        workspace,
        &both,
        || row_of(&shares(sharer), "sharing", &tab).is_some(),
    );
    // The share is the workspace's: the other device reads it there and asks its host for it.
    eventually(
        &format!("{} watches tab {tab} of {}", viewer.label, sharer.label),
        Duration::from_secs(40),
        workspace,
        &both,
        || {
            row_of(&shares(viewer), "watching", &tab).is_some_and(|row| {
                row["host"].as_str().is_some_and(|host| {
                    host.parse::<iroh::EndpointId>()
                        .is_ok_and(|host| *host.as_bytes() == sharer.cert.device_public_key)
                })
            })
        },
    );
    // The window's first keyframe crossed the workspace's live plane.
    eventually(
        &format!(
            "{} received the window of tab {tab} that {} shares",
            viewer.label, sharer.label
        ),
        Duration::from_secs(60),
        workspace,
        &both,
        || row_of(&shares(viewer), "received", &tab).is_some(),
    );
    // Its tile decodes it, and the sharing device names who views it.
    eventually(
        &format!("{}'s desk pictures tab {tab}", viewer.label),
        Duration::from_secs(20),
        workspace,
        &both,
        || {
            row_of(&shares(viewer), "received", &tab)
                .is_some_and(|row| row["pictured"] == json!(true))
        },
    );
    eventually(
        &format!("{} names who views tab {tab}", sharer.label),
        Duration::from_secs(20),
        workspace,
        &both,
        || {
            row_of(&shares(sharer), "sharing", &tab)
                .and_then(|row| row["viewers"].as_array().map(|viewers| !viewers.is_empty()))
                .unwrap_or(false)
        },
    );
}

/// The device that made the workspace, and hosts it, shares a window: the person's other device,
/// which took the workspace from their list, pictures it.
#[test]
fn a_window_the_hosting_device_shares_is_pictured_on_the_persons_other_device() {
    log_sessions_at(LOGGED);
    let (_site, _ada, laptop, phone) = two_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    the_shared_window_arrives(&made, &phone, &laptop, "Shared by the host");
}

/// The person's other device shares a window in a workspace it took from their list: the device
/// that made the workspace, and hosts it, pictures it. This is the direction two machines on one
/// network failed in.
#[test]
fn a_window_the_persons_other_device_shares_is_pictured_on_the_hosting_device() {
    log_sessions_at(LOGGED);
    let (_site, _ada, laptop, phone) = two_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    the_shared_window_arrives(&made, &laptop, &phone, "Shared with the host");
}

/// Both of the person's devices in a workspace a site hosts: the site relays the workspace's live
/// plane, and what one shares the other pictures.
#[test]
fn a_window_shared_in_a_workspace_a_site_hosts_is_pictured_on_the_persons_other_device() {
    log_sessions_at(LOGGED);
    let (site, _ada, laptop, phone) = two_devices_of_one_person();
    the_shared_window_arrives(&site.workspace, &laptop, &phone, "Shared at the site");
}
