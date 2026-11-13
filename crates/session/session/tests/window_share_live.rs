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
//! The same between two devices that were off and came back in either order, and between a
//! machine and the copy made of it.

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

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::people::{
    Device, Person, log_sessions_at, share_the_fleet, two_devices_of_one_person,
};

/// The app whose window is shared.
const NOTES: &str = "so.tilo.notes";
/// The provider of the session's own actions.
const SESSION: &str = "so.tilo.session";
/// What the devices' sessions log here: the live plane, the shares, the channels whose keys seal
/// the plane and the routes between the devices say how far a share came only below `info`.
const LOGGED: &str = "info,tilo_session::live=debug,tilo_session::window_share=debug,\
                      tilo_channels=debug,tilo_network_actor=debug,tilo_session_client=warn";
/// How many kinds of line a failed step shows of one part of a session's log.
const SHOWN: usize = 90;

/// What `device`'s session says of its window shares: `sharing`, `starting`, `watching`,
/// `received` and `live`, as its `session:window-share` resource reads.
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

/// How `device`'s session says the live plane of `workspace` stands: the workspace's row in `live`
/// of the shares' reading, `Null` while the session names no plane for it.
fn plane_of(device: &Device, workspace: &str) -> Value {
    shares(device)["live"]
        .as_array()
        .and_then(|live| {
            live.iter()
                .find(|plane| plane["room"].as_str() == Some(workspace))
                .cloned()
        })
        .unwrap_or(Value::Null)
}

/// What `device` says of who hosts `workspace` (`workspace_hosts`): `Null` while it does not hold
/// it.
fn hosting_of(device: &Device, workspace: &str) -> Value {
    device.ask(&json!({"op": "workspace_hosts"}))["answer"]["workspaces"]
        .as_array()
        .and_then(|held| {
            held.iter()
                .find(|row| row["room"].as_str() == Some(workspace))
                .cloned()
        })
        .unwrap_or(Value::Null)
}

/// What `device`'s desk holds of `workspace`: its tabs, each with what it shows, and the window
/// shares its copy of the workspace's governance names.
fn desk_of(device: &Device, workspace: &str) -> String {
    let state = device.state();
    let Some(held) = state["state"]["workspace_tree"]["workspaces"]
        .as_array()
        .and_then(|all| {
            all.iter()
                .find(|held| held["room"].as_str() == Some(workspace))
        })
    else {
        return "its desk holds no such workspace".to_owned();
    };
    let tabs: Vec<String> = held["strips"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|strip| strip["window_groups"].as_array().into_iter().flatten())
        .flat_map(|group| group["tabs"].as_array().into_iter().flatten())
        .map(|tab| {
            let shows = tab["tile"]
                .as_object()
                .and_then(|tile| tile.keys().next().cloned())
                .unwrap_or_else(|| tab["tile"].to_string());
            format!("{} {} {shows}", tab["id"], tab["title"])
        })
        .collect();
    let grants: Vec<String> = held["policy"]["grants"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|grant| grant["node"].to_string().contains("window"))
        .map(|grant| grant["id"].to_string())
        .collect();
    format!("tabs {tabs:?}; window shares in its governance {grants:?}")
}

/// The lines of `log` that `keep` keeps, in the order each kind was first said: a line said again
/// with other numbers is counted, and shown as it was said first and last.
fn lines_of(log: &str, keep: impl Fn(&str) -> bool) -> String {
    let mut order: Vec<String> = Vec::new();
    let mut said: BTreeMap<String, (usize, String, String)> = BTreeMap::new();
    for line in log.lines().filter(|line| keep(line)) {
        // Without its time, and with every digit alike.
        let shape: String = line
            .split_once(' ')
            .map_or(line, |(_, rest)| rest)
            .chars()
            .map(|c| if c.is_ascii_digit() { '#' } else { c })
            .collect();
        if let Some((count, _, last)) = said.get_mut(&shape) {
            *count += 1;
            line.clone_into(last);
        } else {
            order.push(shape.clone());
            said.insert(shape, (1, line.to_owned(), String::new()));
        }
    }
    let mut out = String::new();
    for shape in order.iter().take(SHOWN) {
        let (count, first, last) = &said[shape];
        let _ = writeln!(
            out,
            "x{count} {}",
            first.chars().take(600).collect::<String>()
        );
        if *count > 1 {
            let _ = writeln!(
                out,
                "   last {}",
                last.chars().take(600).collect::<String>()
            );
        }
    }
    if order.len() > SHOWN {
        let _ = writeln!(out, "and {} more kinds of line", order.len() - SHOWN);
    }
    out
}

/// How far a share came on each of `devices`, for a step that never happened: what each session
/// says of its shares, of who hosts `workspace` and of who is present there, what its desk holds
/// of the workspace, and what it logged of the live plane, of the workspace's channels, of the
/// shares and of the windows' control messages.
fn report(workspace: &str, devices: &[&Device]) -> String {
    let mut out = String::new();
    for device in devices {
        let log = device.log();
        let _ = writeln!(
            out,
            "== {} (device {}) ==\nshares: {}\nhosts: {}\npresent in {workspace}: {:?}\n\
             desk in {workspace}: {}\n\
             -- the live plane --\n{}\n-- the workspace's channels --\n{}\n-- the shares --\n{}\n\
             -- the windows' messages --\n{}\n-- warnings --\n{}",
            device.label,
            hex::encode(device.cert.device_public_key),
            shares(device),
            hosting_of(device, workspace),
            device.present(workspace),
            desk_of(device, workspace),
            lines_of(&log, |line| line.contains("tilo_session::live")),
            lines_of(&log, |line| {
                line.contains("tilo_channels") && line.contains(workspace)
            }),
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

/// The epoch `device`'s session last said it seals the live plane of `workspace` under: what it
/// logs when the plane opens and whenever its keys move.
fn sealing_epoch(device: &Device, workspace: &str) -> Option<u64> {
    let log = device.log();
    log.lines()
        .rev()
        .find(|line| {
            line.contains("tilo_session::live")
                && line.contains(workspace)
                && line.contains(" epoch=")
        })
        .and_then(|line| {
            line.rsplit_once(" epoch=")?
                .1
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
}

/// How often `device`'s session said that no relay of `workspace` took it into the workspace's
/// live plane.
fn missed_relays(device: &Device, workspace: &str) -> usize {
    device
        .log()
        .lines()
        .filter(|line| {
            line.contains(workspace)
                && (line.contains("no relay of this workspace was reached")
                    || line.contains("no device that relays this workspace took this device in"))
        })
        .count()
}

/// Wait until both devices seal the live plane of `workspace` under the same keys and have for a
/// while: nothing of the workspace's keys is still on its way to either, so neither is told of a
/// change of them later.
fn keys_at_rest(workspace: &str, one: &Device, other: &Device) {
    let mut alike: Option<(u64, Instant)> = None;
    eventually(
        "both devices hold the same keys of the workspace, and those stopped moving",
        Duration::from_secs(60),
        workspace,
        &[one, other],
        || match (
            sealing_epoch(one, workspace),
            sealing_epoch(other, workspace),
        ) {
            (Some(here), Some(there)) if here == there => match alike {
                Some((epoch, since)) if epoch == here => since.elapsed() >= Duration::from_secs(4),
                _ => {
                    alike = Some((here, Instant::now()));
                    false
                }
            },
            _ => {
                alike = None;
                false
            }
        },
    );
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
    // The pictures travel the workspace's live plane: each device is in it and says so, and both
    // run through the one device that relays the workspace.
    for device in both {
        eventually(
            &format!(
                "{} says the live plane of {workspace} is open",
                device.label
            ),
            Duration::from_secs(60),
            workspace,
            &both,
            || {
                let plane = plane_of(device, workspace);
                plane["state"] == json!("open") && plane["waiting_for"] == json!("")
            },
        );
    }
    assert_eq!(
        plane_of(sharer, workspace)["relay"],
        plane_of(viewer, workspace)["relay"],
        "both devices' planes run through one relay\n{}",
        report(workspace, &both)
    );
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

    // The sharing device captured the window and started its stream, and nothing its pictures
    // travel is missing.
    eventually(
        &format!(
            "{} says it shares tab {tab} ({shows}) and waits for nothing",
            sharer.label
        ),
        Duration::from_secs(40),
        workspace,
        &both,
        || {
            row_of(&shares(sharer), "sharing", &tab)
                .is_some_and(|row| row["waiting_for"] == json!(""))
        },
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
        &format!(
            "{} names who views tab {tab}, and says it sends the window",
            sharer.label
        ),
        Duration::from_secs(20),
        workspace,
        &both,
        || {
            row_of(&shares(sharer), "sharing", &tab).is_some_and(|row| {
                row["streaming"] == json!(true)
                    && row["viewers"]
                        .as_array()
                        .is_some_and(|viewers| !viewers.is_empty())
            })
        },
    );
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

/// Both devices were off and came back, the hosting device first: the other device finds the
/// workspace's relay there when its session starts, and what it shares the hosting device
/// pictures.
#[test]
fn a_window_shared_after_both_devices_came_back_the_host_first_is_pictured() {
    log_sessions_at(LOGGED);
    let (_site, _ada, mut laptop, mut phone) = two_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    laptop.stop();
    phone.stop();
    phone.start();
    phone.wait_holding(&made);
    laptop.start();
    laptop.wait_holding(&made);
    the_shared_window_arrives(&made, &laptop, &phone, "Shared after the host came back");
}

/// Both devices were off and the hosting device came back last: when the other device's session
/// started, the device relaying the workspace was not there to reach. Its live plane opens once
/// that device is back, and what it shares the hosting device pictures.
#[test]
fn a_window_shared_after_the_host_came_back_last_is_pictured() {
    log_sessions_at(LOGGED);
    let (_site, _ada, mut laptop, mut phone) = two_devices_of_one_person();
    let made = made_on(&phone, &laptop);
    // Neither device is behind on the workspace's keys when it goes off: a change of them that
    // reached the laptop only after the phone came back would have its plane settled again, and
    // hide that nothing else does.
    keys_at_rest(&made, &laptop, &phone);
    laptop.stop();
    phone.stop();
    let missed = missed_relays(&laptop, &made);
    laptop.start();
    laptop.wait_holding(&made);
    eventually(
        "the laptop asked the phone, which is off, for the workspace's live plane",
        Duration::from_secs(60),
        &made,
        &[&laptop],
        || missed_relays(&laptop, &made) > missed,
    );
    phone.start();
    phone.wait_holding(&made);
    the_shared_window_arrives(&made, &laptop, &phone, "Shared before the host was back");
}

/// A desktop that hosts the workspace its desk made, and a machine copied from it while it was
/// off, re-keyed at boot and certified by their person: the copy holds the workspace from the
/// copied disk and its host takes it in as the person's further device. The workspace's id.
fn a_desktop_and_its_copy() -> (Person, Device, Device, String) {
    let ada = Person::new("Ada");
    let mut desktop = ada.device("Ada's desktop");
    desktop.start();
    tilo_test_env::wait_until("the desktop shows the workspace its desk made", || {
        desktop.showing().is_some()
    });
    let workspace = desktop.showing().expect("the workspace the desk made");
    desktop.stop();
    let (mut copy, _) = ada.branch(&desktop);
    share_the_fleet(&ada, &[&desktop, &copy]);
    desktop.start();
    desktop.wait_holding(&workspace);
    copy.start();
    eventually(
        &format!("{} holds {workspace} and its host took it in", copy.label),
        Duration::from_secs(120),
        &workspace,
        &[&desktop, &copy],
        || {
            let row = hosting_of(&copy, &workspace);
            row["awaiting_host"] == json!(false) && row["hosted_here"] == json!(false)
        },
    );
    (ada, desktop, copy, workspace)
}

/// The machines the failure was found on: one makes and hosts the workspace, the other is a copy
/// of it that its person certified, and the copy shares a window.
#[test]
fn a_window_a_copied_machine_shares_is_pictured_on_the_device_it_was_copied_from() {
    log_sessions_at(LOGGED);
    let (_ada, desktop, copy, workspace) = a_desktop_and_its_copy();
    the_shared_window_arrives(&workspace, &copy, &desktop, "Shared by the copy");
}
